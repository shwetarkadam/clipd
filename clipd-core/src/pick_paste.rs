//! Picking a clip pastes it.
//!
//! Fetching an older clip used to take five steps: open clipd, find the clip,
//! pick it (which only *copied* it), switch back to the app, press ⌘V. The
//! last two are the ones people gave up on. Now the window puts the clip on
//! the clipboard, gets out of the way and asks the daemon — which holds the
//! Accessibility trust to type — to paste once the app is back in front.
//!
//! A request names the app it is for. The daemon waits for that app to be
//! frontmost and pastes; if it never comes back, nothing is pasted, because
//! pasting into whatever happens to be in front instead is worse than not
//! pasting at all. Same file channel as form fill.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A paste the daemon should make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteRequest {
    /// The app the clip is for, as the daemon names frontmost apps. `None`
    /// when the asker never took focus (the tray popover), so whatever is in
    /// front is where the person was.
    pub target_app: Option<String>,
    /// Unix milliseconds when it was asked for.
    pub made_ms: u64,
}

/// A request older than this is not acted on.
const FRESH_FOR: Duration = Duration::from_secs(5);

fn request_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("pick_paste_request.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Ask the daemon to paste the clipboard into `target_app` once it is in front.
pub fn request(target_app: Option<String>) {
    let req = PasteRequest {
        target_app,
        made_ms: now_ms(),
    };
    let path = request_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&req) {
        let _ = std::fs::write(path, json);
    }
}

/// The pending request, if there is a fresh one. Taking it removes it.
pub fn take_request() -> Option<PasteRequest> {
    let path = request_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let req: PasteRequest = serde_json::from_str(&raw).ok()?;
    is_fresh(&req, now_ms()).then_some(req)
}

fn is_fresh(req: &PasteRequest, now: u64) -> bool {
    now.saturating_sub(req.made_ms) <= FRESH_FOR.as_millis() as u64
}

/// Whether the daemon may paste now: the target is in front, and clipd itself
/// is not (a paste into clipd's own search field is never what was meant).
pub fn may_paste(target: Option<&str>, frontmost: Option<&str>) -> bool {
    let Some(front) = frontmost else {
        return false;
    };
    if is_clipd(front) {
        return false;
    }
    match target {
        Some(t) => t == front,
        None => true,
    }
}

fn is_clipd(app: &str) -> bool {
    matches!(app, "Clipd" | "clipd" | "clipd-gui" | "clipd-ui")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pastes_only_into_the_app_it_was_for() {
        assert!(may_paste(Some("Slack"), Some("Slack")));
        assert!(!may_paste(Some("Slack"), Some("Safari")), "never into whatever else is in front");
        assert!(!may_paste(Some("Slack"), None));
    }

    #[test]
    fn never_into_clipd_itself() {
        assert!(!may_paste(None, Some("Clipd")));
        assert!(!may_paste(None, Some("clipd-gui")));
        assert!(may_paste(None, Some("Notes")));
    }

    #[test]
    fn stale_requests_are_ignored() {
        let req = PasteRequest { target_app: None, made_ms: 10_000 };
        assert!(is_fresh(&req, 10_500));
        assert!(!is_fresh(&req, 10_000 + FRESH_FOR.as_millis() as u64 + 1));
    }
}
