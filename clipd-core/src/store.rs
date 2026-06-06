use crate::embedding::{embedding_from_bytes, embedding_to_bytes, Embedding};
use crate::models::{ClipEntry, ClipStats, ContentType, SearchFilters};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::{Path, PathBuf};

/// SQLite-backed clipboard history store with FTS5 full-text search.
pub struct ClipStore {
    conn: Connection,
    db_path: PathBuf,
}

impl ClipStore {
    /// Open or create the clip store at the given path.
    pub fn new(db_path: &Path) -> SqlResult<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let conn = Connection::open(db_path)?;
        let store = ClipStore {
            conn,
            db_path: db_path.to_path_buf(),
        };
        store.run_migrations()?;
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    #[cfg(test)]
    pub fn in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        let store = ClipStore {
            conn,
            db_path: PathBuf::from(":memory:"),
        };
        store.run_migrations()?;
        Ok(store)
    }

    /// Get the default database path: ~/.local/share/clipd/clipd.db
    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("clipd")
            .join("clipd.db")
    }

    fn run_migrations(&self) -> SqlResult<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS clips (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                content       TEXT NOT NULL,
                content_type  TEXT NOT NULL DEFAULT 'text',
                content_hash  TEXT NOT NULL,
                source_app    TEXT,
                timestamp     TEXT NOT NULL,
                preview       TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_clips_timestamp ON clips(timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_clips_hash ON clips(content_hash);
            CREATE INDEX IF NOT EXISTS idx_clips_type ON clips(content_type);
            CREATE INDEX IF NOT EXISTS idx_clips_app ON clips(source_app);
            ",
        )?;

        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS clip_embeddings (
                clip_id   INTEGER PRIMARY KEY REFERENCES clips(id) ON DELETE CASCADE,
                embedding BLOB NOT NULL
            );
            ",
        )?;

        // Create FTS5 virtual table for full-text search
        self.conn.execute_batch(
            "
            CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(
                content,
                preview,
                content='clips',
                content_rowid='id'
            );

            -- Triggers to keep FTS in sync
            CREATE TRIGGER IF NOT EXISTS clips_ai AFTER INSERT ON clips BEGIN
                INSERT INTO clips_fts(rowid, content, preview)
                VALUES (new.id, new.content, new.preview);
            END;

            CREATE TRIGGER IF NOT EXISTS clips_ad AFTER DELETE ON clips BEGIN
                INSERT INTO clips_fts(clips_fts, rowid, content, preview)
                VALUES ('delete', old.id, old.content, old.preview);
            END;

            CREATE TRIGGER IF NOT EXISTS clips_au AFTER UPDATE ON clips BEGIN
                INSERT INTO clips_fts(clips_fts, rowid, content, preview)
                VALUES ('delete', old.id, old.content, old.preview);
                INSERT INTO clips_fts(rowid, content, preview)
                VALUES (new.id, new.content, new.preview);
            END;
            ",
        )?;

        Ok(())
    }

    /// Insert a new clip. Deduplicates by content hash (updates timestamp if duplicate).
    pub fn insert(&self, entry: &ClipEntry) -> SqlResult<i64> {
        // Check if same content already exists (dedup)
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM clips WHERE content_hash = ?1",
                params![entry.content_hash],
                |row| row.get(0),
            )
            .ok();

        if let Some(existing_id) = existing {
            // Update timestamp to move it to the top
            self.conn.execute(
                "UPDATE clips SET timestamp = ?1, source_app = ?2 WHERE id = ?3",
                params![entry.timestamp.to_rfc3339(), entry.source_app, existing_id],
            )?;
            return Ok(existing_id);
        }

        self.conn.execute(
            "INSERT INTO clips (content, content_type, content_hash, source_app, timestamp, preview)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.content,
                entry.content_type.as_str(),
                entry.content_hash,
                entry.source_app,
                entry.timestamp.to_rfc3339(),
                entry.preview,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get recent clips, newest first.
    pub fn get_recent(&self, limit: usize) -> SqlResult<Vec<ClipEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_type, content_hash, source_app, timestamp, preview
             FROM clips ORDER BY timestamp DESC LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(ClipEntry {
                id: row.get(0)?,
                content: row.get(1)?,
                content_type: ContentType::from_str(&row.get::<_, String>(2)?),
                content_hash: row.get(3)?,
                source_app: row.get(4)?,
                timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                preview: row.get(6)?,
            })
        })?;

        rows.collect()
    }

    /// Full-text search with optional filters.
    pub fn search(&self, filters: &SearchFilters) -> SqlResult<Vec<ClipEntry>> {
        let limit = if filters.limit > 0 { filters.limit } else { 50 };

        // If we have a text query, use FTS5
        if let Some(ref query) = filters.query {
            let fts_query = format!("\"{}\"", query.replace('"', "\"\""));
            let mut sql = String::from(
                "SELECT c.id, c.content, c.content_type, c.content_hash, c.source_app, c.timestamp, c.preview
                 FROM clips c
                 JOIN clips_fts f ON c.id = f.rowid
                 WHERE clips_fts MATCH ?1",
            );
            let mut param_idx = 2;
            let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(fts_query.clone())];

            if let Some(ref ct) = filters.content_type {
                sql.push_str(&format!(" AND c.content_type = ?{}", param_idx));
                params_vec.push(Box::new(ct.as_str().to_string()));
                param_idx += 1;
            }

            if let Some(ref app) = filters.source_app {
                sql.push_str(&format!(" AND c.source_app = ?{}", param_idx));
                params_vec.push(Box::new(app.clone()));
                param_idx += 1;
            }

            if let Some(since) = filters.since {
                sql.push_str(&format!(" AND c.timestamp >= ?{}", param_idx));
                params_vec.push(Box::new(since.to_rfc3339()));
                // param_idx += 1; // not needed, last param
            }

            sql.push_str(&format!(" ORDER BY c.timestamp DESC LIMIT {}", limit));

            let mut stmt = self.conn.prepare(&sql)?;
            let params_refs: Vec<&dyn rusqlite::types::ToSql> =
                params_vec.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(params_refs.as_slice(), |row| {
                Ok(ClipEntry {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    content_type: ContentType::from_str(&row.get::<_, String>(2)?),
                    content_hash: row.get(3)?,
                    source_app: row.get(4)?,
                    timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    preview: row.get(6)?,
                })
            })?;

            rows.collect()
        } else {
            // No text query — just filter
            let mut sql = String::from(
                "SELECT id, content, content_type, content_hash, source_app, timestamp, preview
                 FROM clips WHERE 1=1",
            );
            let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
            let mut param_idx = 1;

            if let Some(ref ct) = filters.content_type {
                sql.push_str(&format!(" AND content_type = ?{}", param_idx));
                params_vec.push(Box::new(ct.as_str().to_string()));
                param_idx += 1;
            }

            if let Some(ref app) = filters.source_app {
                sql.push_str(&format!(" AND source_app = ?{}", param_idx));
                params_vec.push(Box::new(app.clone()));
                param_idx += 1;
            }

            if let Some(since) = filters.since {
                sql.push_str(&format!(" AND timestamp >= ?{}", param_idx));
                params_vec.push(Box::new(since.to_rfc3339()));
                // param_idx += 1;
            }

            sql.push_str(&format!(" ORDER BY timestamp DESC LIMIT {}", limit));

            let mut stmt = self.conn.prepare(&sql)?;
            let params_refs: Vec<&dyn rusqlite::types::ToSql> =
                params_vec.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(params_refs.as_slice(), |row| {
                Ok(ClipEntry {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    content_type: ContentType::from_str(&row.get::<_, String>(2)?),
                    content_hash: row.get(3)?,
                    source_app: row.get(4)?,
                    timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    preview: row.get(6)?,
                })
            })?;

            rows.collect()
        }
    }

    /// Get a single clip by ID.
    pub fn get_by_id(&self, id: i64) -> SqlResult<ClipEntry> {
        self.conn.query_row(
            "SELECT id, content, content_type, content_hash, source_app, timestamp, preview
             FROM clips WHERE id = ?1",
            params![id],
            |row| {
                Ok(ClipEntry {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    content_type: ContentType::from_str(&row.get::<_, String>(2)?),
                    content_hash: row.get(3)?,
                    source_app: row.get(4)?,
                    timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    preview: row.get(6)?,
                })
            },
        )
    }

    /// Delete a clip by ID.
    pub fn delete(&self, id: i64) -> SqlResult<bool> {
        let count = self.conn.execute("DELETE FROM clips WHERE id = ?1", params![id])?;
        Ok(count > 0)
    }

    /// Delete clips older than the given date.
    pub fn delete_before(&self, before: &DateTime<Utc>) -> SqlResult<usize> {
        let count = self.conn.execute(
            "DELETE FROM clips WHERE timestamp < ?1",
            params![before.to_rfc3339()],
        )?;
        Ok(count)
    }

    /// Delete all clips.
    pub fn clear_all(&self) -> SqlResult<usize> {
        let count = self.conn.execute("DELETE FROM clips", [])?;
        Ok(count)
    }

    /// Store an embedding vector for a clip.
    pub fn store_embedding(&self, clip_id: i64, embedding: &Embedding) -> SqlResult<()> {
        let bytes = embedding_to_bytes(embedding);
        self.conn.execute(
            "INSERT OR REPLACE INTO clip_embeddings (clip_id, embedding) VALUES (?1, ?2)",
            params![clip_id, bytes],
        )?;
        Ok(())
    }

    /// Get the embedding for a specific clip.
    pub fn get_embedding(&self, clip_id: i64) -> SqlResult<Option<Embedding>> {
        let result: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT embedding FROM clip_embeddings WHERE clip_id = ?1",
                params![clip_id],
                |row| row.get(0),
            )
            .ok();
        Ok(result.map(|bytes| embedding_from_bytes(&bytes)))
    }

    /// Get all stored embeddings (for search).
    pub fn get_all_embeddings(&self) -> SqlResult<Vec<(i64, Embedding)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT clip_id, embedding FROM clip_embeddings")?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            Ok((id, embedding_from_bytes(&bytes)))
        })?;
        rows.collect()
    }

    /// Get embeddings only for the given clip IDs. Useful when only a subset of clips
    /// are loaded into memory (e.g., the 200 most recent), avoiding loading all embeddings.
    /// Uses a single SQL query with an IN clause rather than one query per ID.
    pub fn get_embeddings_for_clip_ids(
        &self,
        ids: &[i64],
    ) -> SqlResult<Vec<(i64, Embedding)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT clip_id, embedding FROM clip_embeddings WHERE clip_id IN ({})",
            placeholders
        );
        let mut stmt = self.conn.prepare(&sql)?;

        // Build owned Box<dyn ToSql> params and keep them alive for the query call.
        use rusqlite::ToSql;
        let params: Vec<Box<dyn ToSql>> =
            ids.iter().map(|&id| Box::new(id) as Box<dyn ToSql>).collect();
        let params_refs: Vec<&dyn ToSql> =
            params.iter().map(|b| b.as_ref() as &dyn ToSql).collect();

        let mut rows = stmt.query(params_refs.as_slice())?;
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            results.push((id, embedding_from_bytes(&bytes)));
        }
        Ok(results)
    }

    /// Get clip IDs that don't have embeddings yet.
    pub fn get_unembedded_clip_ids(&self, limit: usize) -> SqlResult<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id FROM clips c
             LEFT JOIN clip_embeddings e ON c.id = e.clip_id
             WHERE e.clip_id IS NULL
             ORDER BY c.timestamp DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| row.get(0))?;
        rows.collect()
    }

    /// Count how many clips have embeddings.
    pub fn embedding_count(&self) -> SqlResult<usize> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM clip_embeddings",
            [],
            |row| row.get(0),
        )
    }

    /// Delete clips beyond `max_clips` (keeps most recent). Removes their embeddings too.
    /// Call this periodically to prevent unbounded DB growth.
    pub fn prune_old_clips(&self, max_clips: usize) -> SqlResult<usize> {
        let count: usize = self.conn.query_row("SELECT COUNT(*) FROM clips", [], |row| row.get(0))?;
        if count <= max_clips {
            return Ok(0);
        }
        let to_delete = count - max_clips;
        // Delete oldest clips (lowest id = earliest)
        let deleted = self.conn.execute(
            "DELETE FROM clips WHERE id IN (
                SELECT id FROM clips ORDER BY timestamp ASC LIMIT ?1
            )",
            params![to_delete as i64],
        )?;
        log::info!("🗑️ Pruned {} old clips (capped at {})", deleted, max_clips);
        Ok(deleted as usize)
    }

    /// Convenience: prune if total clips exceed `max_clips`.
    pub fn prune_if_needed(&self, max_clips: usize) -> SqlResult<usize> {
        self.prune_old_clips(max_clips)
    }

    /// Gather statistics about the store.
    pub fn stats(&self) -> SqlResult<ClipStats> {
        let total_clips: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM clips", [], |row| row.get(0))?;

        let unique_apps: usize = self.conn.query_row(
            "SELECT COUNT(DISTINCT source_app) FROM clips WHERE source_app IS NOT NULL",
            [],
            |row| row.get(0),
        )?;

        let oldest_clip: Option<DateTime<Utc>> = self
            .conn
            .query_row(
                "SELECT MIN(timestamp) FROM clips",
                [],
                |row| row.get::<_, Option<String>>(0),
            )?
            .and_then(|ts| DateTime::parse_from_rfc3339(&ts).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let newest_clip: Option<DateTime<Utc>> = self
            .conn
            .query_row(
                "SELECT MAX(timestamp) FROM clips",
                [],
                |row| row.get::<_, Option<String>>(0),
            )?
            .and_then(|ts| DateTime::parse_from_rfc3339(&ts).ok())
            .map(|dt| dt.with_timezone(&Utc));

        // Top source apps
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(source_app, 'unknown'), COUNT(*) as cnt
             FROM clips GROUP BY source_app ORDER BY cnt DESC LIMIT 10",
        )?;
        let top_apps: Vec<(String, usize)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        // Content type distribution
        let mut stmt = self.conn.prepare(
            "SELECT content_type, COUNT(*) as cnt
             FROM clips GROUP BY content_type ORDER BY cnt DESC",
        )?;
        let type_counts: Vec<(String, usize)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        // DB file size
        let db_size_bytes = std::fs::metadata(&self.db_path)
            .map(|m| m.len())
            .unwrap_or(0);

        Ok(ClipStats {
            total_clips,
            unique_apps,
            db_size_bytes,
            oldest_clip,
            newest_clip,
            top_apps,
            type_counts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ClipEntry;

    fn make_entry(content: &str) -> ClipEntry {
        ClipEntry::new(content.to_string(), Some("test_app".to_string()))
    }

    #[test]
    fn test_insert_and_get_recent() {
        let store = ClipStore::in_memory().unwrap();
        store.insert(&make_entry("hello world")).unwrap();
        store.insert(&make_entry("fn main() {\n}")).unwrap();

        let recent = store.get_recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert!(recent[0].content.contains("fn main")); // newest first
    }

    #[test]
    fn test_dedup_same_content() {
        let store = ClipStore::in_memory().unwrap();
        store.insert(&make_entry("duplicate text")).unwrap();
        store.insert(&make_entry("duplicate text")).unwrap();

        let recent = store.get_recent(10).unwrap();
        assert_eq!(recent.len(), 1); // deduped
    }

    #[test]
    fn test_fts_search() {
        let store = ClipStore::in_memory().unwrap();
        store.insert(&make_entry("hello world")).unwrap();
        store.insert(&make_entry("goodbye world")).unwrap();
        store
            .insert(&make_entry("rust programming is great"))
            .unwrap();

        let filters = SearchFilters {
            query: Some("world".to_string()),
            limit: 10,
            ..Default::default()
        };
        let results = store.search(&filters).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_delete() {
        let store = ClipStore::in_memory().unwrap();
        let id = store.insert(&make_entry("to delete")).unwrap();

        assert!(store.delete(id).unwrap());
        assert_eq!(store.get_recent(10).unwrap().len(), 0);
    }

    #[test]
    fn test_stats() {
        let store = ClipStore::in_memory().unwrap();
        store.insert(&make_entry("clip one")).unwrap();
        store.insert(&make_entry("https://example.com")).unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_clips, 2);
    }
}
