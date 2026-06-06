use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Highest slot index (0 is the OS mirror; numbered slots are 1..=MAX_CLIP_SLOT).
pub const MAX_CLIP_SLOT: u8 = 30;

/// Multi-slot clipboard manager. Slots 0..=MAX_CLIP_SLOT hold text content.
/// Slot 0 is the "default" slot (mirrors OS clipboard).
#[derive(Clone)]
pub struct SlotManager {
    slots: Arc<RwLock<HashMap<u8, String>>>,
}

impl SlotManager {
    pub fn new() -> Self {
        SlotManager {
            slots: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Copy content into a numbered slot (0..=MAX_CLIP_SLOT).
    pub fn copy_to_slot(&self, slot: u8, content: &str) -> Result<(), String> {
        if slot > MAX_CLIP_SLOT {
            return Err(format!("Slot {} out of range (0-{})", slot, MAX_CLIP_SLOT));
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
        let slots = self.slots.read().map_err(|e| e.to_string())?;
        Ok(slots.get(&slot).cloned())
    }

    /// List all non-empty slots, sorted by slot number.
    pub fn list_slots(&self) -> Result<Vec<(u8, String)>, String> {
        let slots = self.slots.read().map_err(|e| e.to_string())?;
        let mut list: Vec<(u8, String)> = slots
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        list.sort_by_key(|(k, _)| *k);
        Ok(list)
    }

    /// Clear a specific slot.
    pub fn clear_slot(&self, slot: u8) -> Result<(), String> {
        let mut slots = self.slots.write().map_err(|e| e.to_string())?;
        slots.remove(&slot);
        Ok(())
    }

    /// Clear all slots.
    pub fn clear_all(&self) -> Result<(), String> {
        let mut slots = self.slots.write().map_err(|e| e.to_string())?;
        slots.clear();
        Ok(())
    }

    /// Push new content into the history ring.
    /// Shifts slots 1..MAX_CLIP_SLOT-1 down (oldest in slot MAX_CLIP_SLOT is evicted),
    /// inserts the new content into slot 1 (most recent) and slot 0 (OS mirror).
    pub fn push_history(&self, content: String) -> Result<(), String> {
        let mut slots = self.slots.write().map_err(|e| e.to_string())?;
        for s in (1..MAX_CLIP_SLOT).rev() {
            if let Some(val) = slots.remove(&s) {
                slots.insert(s + 1, val);
            }
        }
        slots.insert(1, content.clone());
        slots.insert(0, content);
        Ok(())
    }

    /// Check if a slot has content.
    pub fn has_content(&self, slot: u8) -> bool {
        self.slots
            .read()
            .map(|s| s.contains_key(&slot))
            .unwrap_or(false)
    }

    /// Number of occupied slots.
    pub fn occupied_count(&self) -> usize {
        self.slots.read().map(|s| s.len()).unwrap_or(0)
    }
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
}
