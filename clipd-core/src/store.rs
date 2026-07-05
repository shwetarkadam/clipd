use crate::collections::{Collection, CollectionItem};
use crate::embedding::{embedding_from_bytes, embedding_to_bytes, Embedding};
use crate::models::{ClipEntry, ClipStats, ContentType, SearchFilters};
use crate::snippets::Snippet;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
        conn.busy_timeout(Duration::from_millis(150))?;
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
                preview       TEXT NOT NULL,
                slot          INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_clips_timestamp ON clips(timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_clips_hash ON clips(content_hash);
            CREATE INDEX IF NOT EXISTS idx_clips_type ON clips(content_type);
            CREATE INDEX IF NOT EXISTS idx_clips_app ON clips(source_app);
            ",
        )?;

        // Add slot column if upgrading from schema before slot support.
        // Must run before creating idx_clips_slot index.
        if let Err(e) = self
            .conn
            .execute("ALTER TABLE clips ADD COLUMN slot INTEGER", [])
        {
            log::debug!(
                "slot column migration (expected on fresh DB or if already present): {}",
                e
            );
        }

        // Now safe to create the slot index (column guaranteed to exist).
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_clips_slot ON clips(slot)",
            [],
        )?;

        // Image-clip columns (added in the image/OCR release). Each ALTER is a
        // no-op error on a DB that already has the column, so ignore failures.
        for col in [
            "ALTER TABLE clips ADD COLUMN image_path TEXT",
            "ALTER TABLE clips ADD COLUMN thumb_path TEXT",
            "ALTER TABLE clips ADD COLUMN ocr_text TEXT",
        ] {
            if let Err(e) = self.conn.execute(col, []) {
                log::debug!("image column migration (expected if present): {}", e);
            }
        }

        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS clip_embeddings (
                clip_id   INTEGER PRIMARY KEY REFERENCES clips(id) ON DELETE CASCADE,
                embedding BLOB NOT NULL
            );
            ",
        )?;

        // Collections: named, persistent buckets of clips.
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS collections (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                name        TEXT NOT NULL UNIQUE,
                source_app  TEXT,
                created_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS collection_items (
                collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
                clip_id       INTEGER NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
                position      INTEGER NOT NULL,
                added_at      TEXT NOT NULL,
                PRIMARY KEY (collection_id, clip_id)
            );

            CREATE INDEX IF NOT EXISTS idx_coll_items ON collection_items(collection_id, position);

            CREATE TABLE IF NOT EXISTS snippets (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                trigger     TEXT NOT NULL UNIQUE,
                name        TEXT NOT NULL DEFAULT '',
                body        TEXT NOT NULL,
                created_at  TEXT NOT NULL
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

    /// The clip columns, in the canonical order expected by `row_to_clip`.
    const CLIP_COLUMNS: &'static str = "id, content, content_type, content_hash, source_app, \
         timestamp, preview, slot, image_path, thumb_path, ocr_text";

    /// Map a row selected with `CLIP_COLUMNS` into a `ClipEntry`.
    fn row_to_clip(row: &rusqlite::Row) -> SqlResult<ClipEntry> {
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
            slot: row.get(7)?,
            image_path: row.get(8)?,
            thumb_path: row.get(9)?,
            ocr_text: row.get(10)?,
        })
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
                "UPDATE clips SET timestamp = ?1, source_app = ?2, slot = ?3 WHERE id = ?4",
                params![
                    entry.timestamp.to_rfc3339(),
                    entry.source_app,
                    entry.slot,
                    existing_id
                ],
            )?;
            return Ok(existing_id);
        }

        self.conn.execute(
            "INSERT INTO clips (content, content_type, content_hash, source_app, timestamp, preview, slot, image_path, thumb_path, ocr_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.content,
                entry.content_type.as_str(),
                entry.content_hash,
                entry.source_app,
                entry.timestamp.to_rfc3339(),
                entry.preview,
                entry.slot,
                entry.image_path,
                entry.thumb_path,
                entry.ocr_text,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get recent clips, newest first.
    pub fn get_recent(&self, limit: usize) -> SqlResult<Vec<ClipEntry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM clips ORDER BY timestamp DESC LIMIT ?1",
            Self::CLIP_COLUMNS
        ))?;

        let rows = stmt.query_map(params![limit as i64], Self::row_to_clip)?;

        rows.collect()
    }

    /// Full-text search with optional filters.
    pub fn search(&self, filters: &SearchFilters) -> SqlResult<Vec<ClipEntry>> {
        let limit = if filters.limit > 0 { filters.limit } else { 50 };

        // If we have a text query, use FTS5
        if let Some(ref query) = filters.query {
            let fts_query = format!("\"{}\"", query.replace('"', "\"\""));
            let mut sql = format!(
                "SELECT {} FROM clips c
                 JOIN clips_fts f ON c.id = f.rowid
                 WHERE clips_fts MATCH ?1",
                Self::CLIP_COLUMNS
                    .split(", ")
                    .map(|c| format!("c.{}", c))
                    .collect::<Vec<_>>()
                    .join(", ")
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
            let rows = stmt.query_map(params_refs.as_slice(), Self::row_to_clip)?;

            rows.collect()
        } else {
            // No text query — just filter
            let mut sql = format!("SELECT {} FROM clips WHERE 1=1", Self::CLIP_COLUMNS);
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
            let rows = stmt.query_map(params_refs.as_slice(), Self::row_to_clip)?;

            rows.collect()
        }
    }

    /// Get a single clip by ID.
    pub fn get_by_id(&self, id: i64) -> SqlResult<ClipEntry> {
        self.conn.query_row(
            &format!("SELECT {} FROM clips WHERE id = ?1", Self::CLIP_COLUMNS),
            params![id],
            Self::row_to_clip,
        )
    }

    /// Delete a clip by ID.
    pub fn delete(&self, id: i64) -> SqlResult<bool> {
        // Remove backing image files first (best-effort) if this is an image clip.
        if let Ok(clip) = self.get_by_id(id) {
            crate::images::delete_image_files(
                clip.image_path.as_deref(),
                clip.thumb_path.as_deref(),
            );
        }
        let count = self
            .conn
            .execute("DELETE FROM clips WHERE id = ?1", params![id])?;
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
    pub fn get_embeddings_for_clip_ids(&self, ids: &[i64]) -> SqlResult<Vec<(i64, Embedding)>> {
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
        let params: Vec<Box<dyn ToSql>> = ids
            .iter()
            .map(|&id| Box::new(id) as Box<dyn ToSql>)
            .collect();
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
        self.conn
            .query_row("SELECT COUNT(*) FROM clip_embeddings", [], |row| row.get(0))
    }

    /// Delete clips beyond `max_clips` (keeps most recent). Removes their embeddings too.
    /// Call this periodically to prevent unbounded DB growth.
    pub fn prune_old_clips(&self, max_clips: usize) -> SqlResult<usize> {
        let count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM clips", [], |row| row.get(0))?;
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
            .query_row("SELECT MIN(timestamp) FROM clips", [], |row| {
                row.get::<_, Option<String>>(0)
            })?
            .and_then(|ts| DateTime::parse_from_rfc3339(&ts).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let newest_clip: Option<DateTime<Utc>> = self
            .conn
            .query_row("SELECT MAX(timestamp) FROM clips", [], |row| {
                row.get::<_, Option<String>>(0)
            })?
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

    // ── Collections ──

    /// Create a collection. `source_app` (optional) auto-routes clips copied
    /// while that app is frontmost. Returns the new collection id; errors if
    /// the name already exists.
    pub fn create_collection(&self, name: &str, source_app: Option<&str>) -> SqlResult<i64> {
        self.conn.execute(
            "INSERT INTO collections (name, source_app, created_at) VALUES (?1, ?2, ?3)",
            params![name, source_app, Utc::now().to_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn row_to_collection(row: &rusqlite::Row, count: usize) -> SqlResult<Collection> {
        Ok(Collection {
            id: row.get(0)?,
            name: row.get(1)?,
            source_app: row.get(2)?,
            created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            item_count: count,
        })
    }

    /// List collections, newest first, with item counts.
    pub fn list_collections(&self) -> SqlResult<Vec<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.name, c.source_app, c.created_at,
                    (SELECT COUNT(*) FROM collection_items ci WHERE ci.collection_id = c.id)
             FROM collections c ORDER BY c.created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let count: i64 = row.get(4)?;
            Self::row_to_collection(row, count as usize)
        })?;
        rows.collect()
    }

    /// Look up a collection by exact name.
    pub fn get_collection_by_name(&self, name: &str) -> SqlResult<Option<Collection>> {
        let result = self.conn.query_row(
            "SELECT id, name, source_app, created_at FROM collections WHERE name = ?1",
            params![name],
            |row| Self::row_to_collection(row, 0),
        );
        match result {
            Ok(c) => Ok(Some(c)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Find a collection whose `source_app` matches the given frontmost app
    /// (case-insensitive substring), for auto-routing copies.
    pub fn collection_for_app(&self, app: &str) -> SqlResult<Option<Collection>> {
        let app_lower = app.to_lowercase();
        let mut stmt = self.conn.prepare(
            "SELECT id, name, source_app, created_at FROM collections
             WHERE source_app IS NOT NULL AND source_app != ''",
        )?;
        let rows = stmt.query_map([], |row| Self::row_to_collection(row, 0))?;
        for c in rows {
            let c = c?;
            if let Some(ref src) = c.source_app {
                if app_lower.contains(&src.to_lowercase()) {
                    return Ok(Some(c));
                }
            }
        }
        Ok(None)
    }

    /// Add a clip to a collection (deduplicated; no-op if already present).
    pub fn add_clip_to_collection(&self, collection_id: i64, clip_id: i64) -> SqlResult<()> {
        let next_pos: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), 0) + 1 FROM collection_items WHERE collection_id = ?1",
                params![collection_id],
                |row| row.get(0),
            )
            .unwrap_or(1);
        self.conn.execute(
            "INSERT OR IGNORE INTO collection_items (collection_id, clip_id, position, added_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![collection_id, clip_id, next_pos, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Items in a collection, in saved order, with content joined in.
    pub fn collection_items(&self, collection_id: i64) -> SqlResult<Vec<CollectionItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT ci.clip_id, c.content, c.preview, ci.position, ci.added_at
             FROM collection_items ci JOIN clips c ON c.id = ci.clip_id
             WHERE ci.collection_id = ?1 ORDER BY ci.position ASC",
        )?;
        let rows = stmt.query_map(params![collection_id], |row| {
            Ok(CollectionItem {
                clip_id: row.get(0)?,
                content: row.get(1)?,
                preview: row.get(2)?,
                position: row.get(3)?,
                added_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect()
    }

    /// Remove one clip from a collection.
    pub fn remove_collection_item(&self, collection_id: i64, clip_id: i64) -> SqlResult<()> {
        self.conn.execute(
            "DELETE FROM collection_items WHERE collection_id = ?1 AND clip_id = ?2",
            params![collection_id, clip_id],
        )?;
        Ok(())
    }

    /// Delete a collection and its membership rows (clips themselves stay).
    pub fn delete_collection(&self, collection_id: i64) -> SqlResult<()> {
        self.conn.execute(
            "DELETE FROM collections WHERE id = ?1",
            params![collection_id],
        )?;
        Ok(())
    }

    /// Set or clear a collection's auto-route source app.
    pub fn set_collection_source_app(
        &self,
        collection_id: i64,
        source_app: Option<&str>,
    ) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE collections SET source_app = ?1 WHERE id = ?2",
            params![source_app, collection_id],
        )?;
        Ok(())
    }

    // ── Snippets ──

    /// Create or update a snippet (upsert on its unique trigger).
    pub fn upsert_snippet(&self, trigger: &str, name: &str, body: &str) -> SqlResult<i64> {
        self.conn.execute(
            "INSERT INTO snippets (trigger, name, body, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(trigger) DO UPDATE SET name = excluded.name, body = excluded.body",
            params![trigger, name, body, Utc::now().to_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn row_to_snippet(row: &rusqlite::Row) -> SqlResult<Snippet> {
        Ok(Snippet {
            id: row.get(0)?,
            trigger: row.get(1)?,
            name: row.get(2)?,
            body: row.get(3)?,
            created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    /// All snippets, ordered by trigger.
    pub fn list_snippets(&self) -> SqlResult<Vec<Snippet>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, trigger, name, body, created_at FROM snippets ORDER BY trigger ASC",
        )?;
        let rows = stmt.query_map([], Self::row_to_snippet)?;
        rows.collect()
    }

    pub fn delete_snippet(&self, id: i64) -> SqlResult<()> {
        self.conn
            .execute("DELETE FROM snippets WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Delete by trigger; returns true if a row was removed.
    pub fn delete_snippet_by_trigger(&self, trigger: &str) -> SqlResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM snippets WHERE trigger = ?1", params![trigger])?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ClipEntry;

    fn make_entry(content: &str) -> ClipEntry {
        ClipEntry::new(content.to_string(), Some("test_app".to_string()), None)
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
    fn test_image_clip_roundtrip_and_ocr_search() {
        let store = ClipStore::in_memory().unwrap();
        let entry = ClipEntry::new_image(
            "abc123".into(),
            "/tmp/abc123.png".into(),
            "/tmp/abc123_thumb.png".into(),
            Some("Invoice total 4200".into()),
            Some("Preview".into()),
            800,
            600,
        );
        let id = store.insert(&entry).unwrap();

        let got = store.get_by_id(id).unwrap();
        assert_eq!(got.content_type, ContentType::Image);
        assert_eq!(got.image_path.as_deref(), Some("/tmp/abc123.png"));
        assert_eq!(got.thumb_path.as_deref(), Some("/tmp/abc123_thumb.png"));
        assert_eq!(got.ocr_text.as_deref(), Some("Invoice total 4200"));

        // The OCR text is mirrored into content, so it's full-text searchable.
        let filters = SearchFilters {
            query: Some("Invoice".to_string()),
            limit: 10,
            ..Default::default()
        };
        let results = store.search(&filters).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
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

    #[test]
    fn test_collections() {
        let store = ClipStore::in_memory().unwrap();
        let c1 = store.insert(&make_entry("prompt one")).unwrap();
        let c2 = store.insert(&make_entry("prompt two")).unwrap();

        let coll = store
            .create_collection("Cursor prompts", Some("Cursor"))
            .unwrap();
        store.add_clip_to_collection(coll, c1).unwrap();
        store.add_clip_to_collection(coll, c2).unwrap();
        store.add_clip_to_collection(coll, c1).unwrap(); // dedup — no-op

        let items = store.collection_items(coll).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].position, 1);

        let list = store.list_collections().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].item_count, 2);

        // Auto-route lookup by frontmost app (substring, case-insensitive).
        assert!(store.collection_for_app("Cursor").unwrap().is_some());
        assert!(store
            .collection_for_app("com.todesktop.cursor")
            .unwrap()
            .is_some());
        assert!(store.collection_for_app("Safari").unwrap().is_none());

        store.remove_collection_item(coll, c1).unwrap();
        assert_eq!(store.collection_items(coll).unwrap().len(), 1);

        store.delete_collection(coll).unwrap();
        assert_eq!(store.list_collections().unwrap().len(), 0);
    }
}
