use crate::models::ClipEntry;
use sha2::{Digest, Sha256};
use std::sync::mpsc;
use std::time::Duration;

/// Events emitted by the clipboard watcher.
#[derive(Debug, Clone)]
pub enum ClipEvent {
    /// New content detected on the clipboard.
    NewClip(ClipEntry),
}

/// Watches the OS clipboard for changes by polling.
pub struct ClipWatcher {
    poll_interval: Duration,
}

impl ClipWatcher {
    pub fn new(poll_interval_ms: u64) -> Self {
        ClipWatcher {
            poll_interval: Duration::from_millis(poll_interval_ms),
        }
    }

    /// Start watching the clipboard in a loop, sending events to the channel.
    /// This blocks the current thread — run it in a spawned thread.
    pub fn watch(&self, sender: mpsc::Sender<ClipEvent>, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let mut last_hash = String::new();

        // Try to create the clipboard handle
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                log::error!("Failed to open clipboard: {}", e);
                return;
            }
        };

        log::info!("Clipboard watcher started (polling every {:?})", self.poll_interval);

        loop {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                log::info!("Clipboard watcher stopping");
                break;
            }

            // Poll for text content
            if let Ok(text) = clipboard.get_text() {
                if !text.is_empty() {
                    let hash = Self::hash_content(&text);

                    if hash != last_hash {
                        last_hash = hash;

                        // Detect the source app (macOS-specific, best-effort)
                        let source_app = Self::get_frontmost_app();

                        let entry = ClipEntry::new(text, source_app);
                        log::debug!(
                            "New clip: {} [{}] {}",
                            entry.content_type.icon(),
                            entry.content_type.as_str(),
                            &entry.preview
                        );

                        if sender.send(ClipEvent::NewClip(entry)).is_err() {
                            log::error!("Clip event channel closed, stopping watcher");
                            break;
                        }
                    }
                }
            }

            std::thread::sleep(self.poll_interval);
        }
    }

    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Get the name of the frontmost application on macOS.
    /// Returns None on other platforms or on error.
    #[cfg(target_os = "macos")]
    fn get_frontmost_app() -> Option<String> {
        use std::process::Command;
        let output = Command::new("osascript")
            .arg("-e")
            .arg("tell application \"System Events\" to get name of first application process whose frontmost is true")
            .output()
            .ok()?;

        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        } else {
            None
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn get_frontmost_app() -> Option<String> {
        None
    }
}

impl Default for ClipWatcher {
    fn default() -> Self {
        Self::new(500)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_content() {
        let h1 = ClipWatcher::hash_content("hello");
        let h2 = ClipWatcher::hash_content("hello");
        let h3 = ClipWatcher::hash_content("world");

        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }
}
