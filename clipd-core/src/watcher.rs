use crate::models::ClipEntry;
use crate::privacy::{detect_sensitive, is_excluded_app, load_privacy_config, looks_like_password};
use crate::slots::SlotManager;
use sha2::{Digest, Sha256};
use std::sync::mpsc;
use std::time::Duration;

/// Events emitted by the clipboard watcher.
#[derive(Debug, Clone)]
pub enum ClipEvent {
    /// New content detected on the clipboard.
    NewClip(ClipEntry),
    /// A password/secret was detected, so the daemon can offer to save it to a
    /// vault. Carries only the human label(s) — never the secret itself; it is
    /// re-read from the live clipboard at save time. `stored` is false for
    /// confidently-detected secrets (dropped from history) and true for fuzzy
    /// heuristic matches (kept in history, just offered).
    SensitiveClip { kinds: String, stored: bool },
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
    /// `slot_manager` is used to tag clips with the slot they were saved to
    /// via multi-tap hotkey (None = auto-saved via OS copy).
    pub fn watch(
        &self,
        sender: mpsc::SyncSender<ClipEvent>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        suppress: std::sync::Arc<std::sync::atomic::AtomicBool>,
        refresh_hash: std::sync::Arc<std::sync::atomic::AtomicBool>,
        slot_manager: Option<SlotManager>,
    ) {
        let mut last_hash = String::new();
        let privacy_config = load_privacy_config();

        // Try to create the clipboard handle.
        // On macOS 13+, if this fails the most likely cause is missing
        // Screen Recording permission. Without it arboard cannot read the clipboard.
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                log::error!(
                    "Failed to open clipboard (Screen Recording permission?): {} \
                     Grant: System Settings → Privacy & Security → Screen Recording → clipd",
                    e
                );
                return;
            }
        };

        log::info!(
            "Clipboard watcher started (polling every {:?})",
            self.poll_interval
        );

        loop {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                log::info!("Clipboard watcher stopping");
                break;
            }

            // Don't read clipboard while our own paste operations are in progress.
            // Must be checked BEFORE get_text() to avoid a race where the paste
            // function clears suppress between our read and our check.
            if suppress.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(self.poll_interval);
                continue;
            }

            // The daemon mutated the clipboard (e.g. restored slot 1 after multi-tap copy).
            // Re-sync last_hash so we don't emit a duplicate NewClip.
            if refresh_hash.swap(false, std::sync::atomic::Ordering::SeqCst) {
                if let Ok(text) = clipboard.get_text() {
                    if !text.is_empty() {
                        last_hash = Self::hash_content(&text);
                    }
                }
                std::thread::sleep(self.poll_interval);
                continue;
            }

            // Poll for text content
            if let Ok(text) = clipboard.get_text() {
                if !text.is_empty() {
                    let hash = Self::hash_content(&text);

                    if hash != last_hash {
                        last_hash = hash;

                        let source_app = Self::get_frontmost_app();

                        // Copies made from a password manager are already vaulted
                        // — drop silently, nothing to offer.
                        if privacy_config.enabled {
                            if let Some(app) = source_app.as_deref() {
                                if is_excluded_app(app, &privacy_config) {
                                    log::info!("🔒 Clip skipped (excluded app: {})", app);
                                    std::thread::sleep(self.poll_interval);
                                    continue;
                                }
                            }
                        }

                        // A confidently-detected secret is never stored in
                        // history; we surface it so the user can vault it.
                        let matches = detect_sensitive(&text, &privacy_config);
                        if !matches.is_empty() {
                            let kinds = matches
                                .iter()
                                .map(|m| m.kind.label())
                                .collect::<Vec<_>>()
                                .join(", ");
                            log::info!("🔒 Sensitive clip not stored ({})", kinds);
                            let _ = sender.send(ClipEvent::SensitiveClip {
                                kinds,
                                stored: false,
                            });
                            std::thread::sleep(self.poll_interval);
                            continue;
                        }

                        // A fuzzy "looks like a generated password" guess does
                        // NOT remove the clip from history (avoids losing tokens
                        // on a false positive) — it only adds a vault offer.
                        let heuristic_password = privacy_config.enabled
                            && privacy_config.offer_vault_on_secret
                            && looks_like_password(&text);

                        // Look up which slot this content is in (if any).
                        let slot = slot_manager.as_ref().and_then(|mgr| mgr.find_slot(&text));

                        let entry = ClipEntry::new(text, source_app, slot);
                        log::debug!(
                            "New clip: {} [{}] slot={:?} {}",
                            entry.content_type.icon(),
                            entry.content_type.as_str(),
                            entry.slot,
                            &entry.preview
                        );

                        if sender.send(ClipEvent::NewClip(entry)).is_err() {
                            log::error!("Clip event channel closed, stopping watcher");
                            break;
                        }

                        if heuristic_password {
                            log::info!("🔐 Clip looks like a password — offering to vault it");
                            let _ = sender.send(ClipEvent::SensitiveClip {
                                kinds: "possible password".to_string(),
                                stored: true,
                            });
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
