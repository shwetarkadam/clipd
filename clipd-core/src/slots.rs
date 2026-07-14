use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

/// Highest slot index (0 is the OS mirror; numbered slots are 1..=MAX_CLIP_SLOT).
pub const MAX_CLIP_SLOT: u8 = 56;

/// Multi-slot clipboard manager. Slots 0..=MAX_CLIP_SLOT hold text content.
/// Slot 0 is the "default" slot (mirrors OS clipboard).
#[derive(Clone)]
pub struct SlotManager {
    slots: Arc<RwLock<HashMap<u8, String>>>,
    /// Active slots must be shared by the tray-hosted daemon, GUI helpers, and
    /// any accidentally overlapping daemon during an upgrade. `None` keeps
    /// unit tests and explicitly ephemeral callers fully in-memory.
    backing_db: Option<Arc<PathBuf>>,
}

impl SlotManager {
    pub fn new() -> Self {
        SlotManager {
            slots: Arc::new(RwLock::new(HashMap::new())),
            backing_db: None,
        }
    }

    /// Open the process-shared active-slot store in clipd's normal database.
    pub fn persistent_default() -> Result<Self, String> {
        Self::persistent(crate::store::ClipStore::default_path())
    }

    /// Open a process-shared active-slot store backed by the supplied SQLite
    /// database. Reads consult SQLite so separate clipd processes see the same
    /// values immediately; the in-memory map remains a fallback if SQLite is
    /// briefly unavailable.
    pub fn persistent(db_path: impl AsRef<Path>) -> Result<Self, String> {
        let path = db_path.as_ref().to_path_buf();
        let conn = initialize_slot_db(&path)?;
        let mut stmt = conn
            .prepare("SELECT slot, content FROM active_slots ORDER BY slot")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, u8>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut slots = HashMap::new();
        for row in rows {
            let (slot, content) = row.map_err(|e| e.to_string())?;
            if slot <= MAX_CLIP_SLOT {
                slots.insert(slot, content);
            }
        }
        drop(stmt);
        drop(conn);
        Ok(Self {
            slots: Arc::new(RwLock::new(slots)),
            backing_db: Some(Arc::new(path)),
        })
    }

    fn read_persistent_slot(&self, slot: u8) -> Result<Option<String>, String> {
        // Slot 0 mirrors the live OS clipboard and should not be retained on
        // disk (especially when clipboard-history recording is disabled).
        let Some(path) = self.backing_db.as_deref().filter(|_| slot != 0) else {
            return self
                .slots
                .read()
                .map_err(|e| e.to_string())
                .map(|slots| slots.get(&slot).cloned());
        };
        let conn = connect_slot_db(path)?;
        conn.query_row(
            "SELECT content FROM active_slots WHERE slot = ?1",
            params![slot],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    /// Copy content into a numbered slot (0..=MAX_CLIP_SLOT).
    pub fn copy_to_slot(&self, slot: u8, content: &str) -> Result<(), String> {
        if slot > MAX_CLIP_SLOT {
            return Err(format!("Slot {} out of range (0-{})", slot, MAX_CLIP_SLOT));
        }
        if let Some(path) = self.backing_db.as_deref().filter(|_| slot != 0) {
            let conn = connect_slot_db(path)?;
            conn.execute(
                "INSERT INTO active_slots (slot, content, updated_at)
                 VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 ON CONFLICT(slot) DO UPDATE SET
                    content = excluded.content,
                    updated_at = excluded.updated_at",
                params![slot, content],
            )
            .map_err(|e| e.to_string())?;
        }
        let mut slots = self.slots.write().map_err(|e| e.to_string())?;
        slots.insert(slot, content.to_string());
        Ok(())
    }

    /// Get content from a numbered slot.
    pub fn get_slot(&self, slot: u8) -> Result<Option<String>, String> {
        if slot > MAX_CLIP_SLOT {
            return Err(format!("Slot {} out of range (0-{})", slot, MAX_CLIP_SLOT));
        }
        match self.read_persistent_slot(slot) {
            Ok(value) => {
                // Keep the fallback cache synchronized with cross-process writes
                // and clears while preserving the cheap in-process fast path.
                let mut slots = self.slots.write().map_err(|e| e.to_string())?;
                if let Some(content) = &value {
                    slots.insert(slot, content.clone());
                } else {
                    slots.remove(&slot);
                }
                Ok(value)
            }
            Err(e) if self.backing_db.is_some() => {
                log::warn!(
                    "Active-slot database read failed; using memory cache: {}",
                    e
                );
                let slots = self.slots.read().map_err(|e| e.to_string())?;
                Ok(slots.get(&slot).cloned())
            }
            Err(e) => Err(e),
        }
    }

    /// Find which slot contains the given content string.
    /// Returns the slot number (if found).
    pub fn find_slot(&self, content: &str) -> Option<u8> {
        if let Some(path) = self.backing_db.as_deref() {
            if let Ok(conn) = connect_slot_db(path) {
                if let Ok(slot) = conn
                    .query_row(
                        "SELECT slot FROM active_slots WHERE content = ?1
                         ORDER BY CASE WHEN slot = 0 THEN 1 ELSE 0 END, slot
                         LIMIT 1",
                        params![content],
                        |row| row.get(0),
                    )
                    .optional()
                {
                    return slot;
                }
            }
        }
        let slots = self.slots.read().ok()?;
        slots
            .iter()
            .filter_map(|(k, v)| if v == content { Some(*k) } else { None })
            // Prefer a real user slot over slot 0, the live clipboard mirror.
            .min_by_key(|slot| (*slot == 0, *slot))
    }

    /// List all non-empty slots, sorted by slot number.
    pub fn list_slots(&self) -> Result<Vec<(u8, String)>, String> {
        if let Some(path) = self.backing_db.as_deref() {
            let conn = connect_slot_db(path)?;
            let mut stmt = conn
                .prepare("SELECT slot, content FROM active_slots ORDER BY slot")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| e.to_string())?;
            return rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string());
        }
        let slots = self.slots.read().map_err(|e| e.to_string())?;
        let mut list: Vec<(u8, String)> = slots.iter().map(|(k, v)| (*k, v.clone())).collect();
        list.sort_by_key(|(k, _)| *k);
        Ok(list)
    }

    /// Clear a specific slot.
    pub fn clear_slot(&self, slot: u8) -> Result<(), String> {
        if slot > MAX_CLIP_SLOT {
            return Err(format!("Slot {} out of range (0-{})", slot, MAX_CLIP_SLOT));
        }
        if let Some(path) = self.backing_db.as_deref() {
            connect_slot_db(path)?
                .execute("DELETE FROM active_slots WHERE slot = ?1", params![slot])
                .map_err(|e| e.to_string())?;
        }
        let mut slots = self.slots.write().map_err(|e| e.to_string())?;
        slots.remove(&slot);
        Ok(())
    }

    /// Clear all slots.
    pub fn clear_all(&self) -> Result<(), String> {
        if let Some(path) = self.backing_db.as_deref() {
            connect_slot_db(path)?
                .execute("DELETE FROM active_slots", [])
                .map_err(|e| e.to_string())?;
        }
        let mut slots = self.slots.write().map_err(|e| e.to_string())?;
        slots.clear();
        Ok(())
    }

    /// Push new content into the history ring.
    /// Shifts slots 1..MAX_CLIP_SLOT-1 down (oldest in slot MAX_CLIP_SLOT is evicted),
    /// inserts the new content into slot 1 (most recent) and slot 0 (OS mirror).
    pub fn push_history(&self, content: String) -> Result<(), String> {
        let snapshot = {
            let mut slots = self.slots.write().map_err(|e| e.to_string())?;
            for s in (1..MAX_CLIP_SLOT).rev() {
                if let Some(val) = slots.remove(&s) {
                    slots.insert(s + 1, val);
                }
            }
            slots.insert(1, content.clone());
            slots.insert(0, content);
            slots.clone()
        };
        if let Some(path) = self.backing_db.as_deref() {
            let mut conn = connect_slot_db(path)?;
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM active_slots", [])
                .map_err(|e| e.to_string())?;
            for (slot, content) in snapshot.into_iter().filter(|(slot, _)| *slot != 0) {
                tx.execute(
                    "INSERT INTO active_slots (slot, content, updated_at)
                     VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                    params![slot, content],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Check if a slot has content.
    pub fn has_content(&self, slot: u8) -> bool {
        self.get_slot(slot).ok().flatten().is_some()
    }

    /// Number of occupied slots.
    pub fn occupied_count(&self) -> usize {
        self.list_slots().map(|slots| slots.len()).unwrap_or(0)
    }
}

fn connect_slot_db(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn initialize_slot_db(path: &Path) -> Result<Connection, String> {
    let conn = connect_slot_db(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS active_slots (
            slot       INTEGER PRIMARY KEY,
            content    TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        DELETE FROM active_slots WHERE slot = 0;",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

impl Default for SlotManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_and_get() {
        let sm = SlotManager::new();
        sm.copy_to_slot(1, "hello").unwrap();
        assert_eq!(sm.get_slot(1).unwrap(), Some("hello".to_string()));
        assert_eq!(sm.get_slot(2).unwrap(), None);
    }

    #[test]
    fn test_slot_out_of_range() {
        let sm = SlotManager::new();
        assert!(sm.copy_to_slot(MAX_CLIP_SLOT + 1, "bad").is_err());
        assert!(sm.get_slot(MAX_CLIP_SLOT + 1).is_err());
    }

    #[test]
    fn test_list_slots() {
        let sm = SlotManager::new();
        sm.copy_to_slot(3, "three").unwrap();
        sm.copy_to_slot(1, "one").unwrap();

        let list = sm.list_slots().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], (1, "one".to_string()));
        assert_eq!(list[1], (3, "three".to_string()));
    }

    #[test]
    fn test_clear() {
        let sm = SlotManager::new();
        sm.copy_to_slot(1, "one").unwrap();
        sm.copy_to_slot(2, "two").unwrap();

        sm.clear_slot(1).unwrap();
        assert_eq!(sm.get_slot(1).unwrap(), None);
        assert_eq!(sm.occupied_count(), 1);

        sm.clear_all().unwrap();
        assert_eq!(sm.occupied_count(), 0);
    }

    #[test]
    fn test_thread_safety() {
        let sm = SlotManager::new();
        let sm2 = sm.clone();

        let handle = std::thread::spawn(move || {
            sm2.copy_to_slot(5, "from thread").unwrap();
        });

        handle.join().unwrap();
        assert_eq!(sm.get_slot(5).unwrap(), Some("from thread".to_string()));
    }

    #[test]
    fn test_persistent_slots_are_shared_between_managers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipd.db");
        let first = SlotManager::persistent(&path).unwrap();
        let second = SlotManager::persistent(&path).unwrap();

        first.copy_to_slot(2, "shared").unwrap();
        assert_eq!(second.get_slot(2).unwrap(), Some("shared".to_string()));

        first.copy_to_slot(0, "live clipboard only").unwrap();
        assert_eq!(second.get_slot(0).unwrap(), None);

        second.clear_slot(2).unwrap();
        assert_eq!(first.get_slot(2).unwrap(), None);
    }

    #[test]
    fn test_find_slot_prefers_user_slot_over_clipboard_mirror() {
        let sm = SlotManager::new();
        sm.copy_to_slot(0, "same").unwrap();
        sm.copy_to_slot(3, "same").unwrap();
        assert_eq!(sm.find_slot("same"), Some(3));
    }
}
