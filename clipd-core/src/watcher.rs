use crate::models::ClipEntry;
use crate::privacy::{
    detect_sensitive, is_excluded_app, load_privacy_config, looks_like_password, PrivacyConfig,
};
use crate::slots::SlotManager;
use sha2::{Digest, Sha256};
use std::sync::mpsc;
use std::time::Duration;

/// Events emitted by the clipboard watcher.
#[derive(Debug, Clone)]
pub enum ClipEvent {
    /// New content detected on the clipboard.
    NewClip(ClipEntry),
    /// A new image was copied. Carries raw RGBA8 pixels; the daemon persists it
    /// to disk, runs OCR, and inserts the resulting clip. Kept separate from
    /// NewClip so the (potentially large) pixel buffer only travels when needed.
    NewImage {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        source_app: Option<String>,
    },
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
        let mut last_image_hash = String::new();
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

            // Poll for text content. When there's no text on the clipboard,
            // fall through to image polling (a copied screenshot has no text).
            let text = clipboard.get_text().ok().filter(|t| !t.is_empty());
            if text.is_none() {
                Self::poll_image(
                    &mut clipboard,
                    &mut last_image_hash,
                    &privacy_config,
                    &sender,
                );
            }
            if let Some(text) = text {
                {
                    let hash = Self::hash_content(&text);

                    if hash != last_hash {
                        last_hash = hash;

                        let (source_app, source_title) = Self::get_frontmost_context();

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

                        let mut entry = ClipEntry::new(text, source_app, slot);
                        entry.source_title = source_title;
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

    fn hash_bytes(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    /// Read an image off the clipboard and emit `NewImage` if it's new. Called
    /// only when there's no text (a copied screenshot carries no text), so this
    /// is cheap on the common text-copy path.
    fn poll_image(
        clipboard: &mut arboard::Clipboard,
        last_image_hash: &mut String,
        privacy_config: &PrivacyConfig,
        sender: &mpsc::SyncSender<ClipEvent>,
    ) {
        let img = match clipboard.get_image() {
            Ok(i) => i,
            Err(_) => return, // no image on the clipboard
        };
        if img.width == 0 || img.height == 0 || img.bytes.is_empty() {
            return;
        }

        let hash = Self::hash_bytes(&img.bytes);
        if hash == *last_image_hash {
            return; // same image still sitting on the clipboard
        }
        *last_image_hash = hash;

        let source_app = Self::get_frontmost_app();
        if privacy_config.enabled {
            if let Some(app) = source_app.as_deref() {
                if is_excluded_app(app, privacy_config) {
                    log::info!("🔒 Image clip skipped (excluded app: {})", app);
                    return;
                }
            }
        }

        log::debug!("New image clip: {}×{}", img.width, img.height);
        let _ = sender.send(ClipEvent::NewImage {
            width: img.width,
            height: img.height,
            rgba: img.bytes.into_owned(),
            source_app,
        });
    }

    /// Frontmost application name + window title (provenance) at copy time.
    /// macOS: one osascript call (accessibility permission the daemon already
    /// holds). Windows: Win32 foreground-window APIs. Title is best-effort.
    #[cfg(target_os = "macos")]
    fn get_frontmost_context() -> (Option<String>, Option<String>) {
        use std::process::Command;
        let script = r#"tell application "System Events"
  set p to first application process whose frontmost is true
  set appName to name of p
  set winTitle to ""
  try
    set winTitle to name of front window of p
  end try
end tell
appName & linefeed & winTitle"#;
        let output = match Command::new("osascript").arg("-e").arg(script).output() {
            Ok(o) if o.status.success() => o,
            _ => return (None, None),
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let app = lines
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let title = lines
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        (app, title)
    }

    #[cfg(target_os = "windows")]
    fn get_frontmost_context() -> (Option<String>, Option<String>) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
        };
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return (None, None);
            }
            // Window title.
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            let title = if n > 0 {
                Some(String::from_utf16_lossy(&buf[..n as usize]))
            } else {
                None
            };
            // Owning process → executable stem as the app name.
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let mut app = None;
            if pid != 0 {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if !handle.is_null() {
                    let mut path_buf = [0u16; 1024];
                    let mut len = path_buf.len() as u32;
                    if QueryFullProcessImageNameW(handle, 0, path_buf.as_mut_ptr(), &mut len) != 0 {
                        let full = String::from_utf16_lossy(&path_buf[..len as usize]);
                        app = std::path::Path::new(&full)
                            .file_stem()
                            .map(|st| st.to_string_lossy().to_string());
                    }
                    CloseHandle(handle);
                }
            }
            (app, title)
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn get_frontmost_context() -> (Option<String>, Option<String>) {
        (None, None)
    }

    /// App name only — used by the image path and privacy exclusion checks.
    fn get_frontmost_app() -> Option<String> {
        Self::get_frontmost_context().0
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
