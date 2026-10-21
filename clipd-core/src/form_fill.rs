//! Fill a form from your slots: slot 1, Tab, slot 2, Tab, … in one go.
//!
//! Copying a name, an email and an address into slots 1–3 with ⌘C×N and then
//! pasting them one field at a time is most of the work of filling a form.
//! This does the pasting: click the first field, ask for a fill, and each
//! filled slot lands in the next field.
//!
//! The keystrokes have to come from the daemon — it holds the Accessibility
//! trust that lets clipd type — while the request can come from the palette,
//! the tray menu or `clipd fill`. A request is a small file the daemon checks
//! for, the same channel the palette and tray already use with each other.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The most fields one fill types into. Enough for any form worth filling
/// this way; a stray request can never run on for long.
pub const MAX_FIELDS: usize = 12;

/// A request older than this was not meant for now (clipd was not running
/// when it was made, say) and is dropped rather than typed into whatever has
/// focus today.
const FRESH_FOR: Duration = Duration::from_secs(10);

fn request_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("fill_form_request")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Ask the daemon to fill the focused form. `delay_ms` is how long to wait
/// first, so the app with the form can get focus back from whoever asked.
pub fn request(delay_ms: u64) {
    let path = request_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{} {}", now_ms(), delay_ms.min(3_000)));
}

/// The pending request's delay, if there is a fresh one. Taking it removes it.
pub fn take_request() -> Option<Duration> {
    let path = request_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    parse_request(&raw, now_ms())
}

fn parse_request(raw: &str, now: u128) -> Option<Duration> {
    let mut parts = raw.split_whitespace();
    let made: u128 = parts.next()?.parse().ok()?;
    if now.saturating_sub(made) > FRESH_FOR.as_millis() {
        return None;
    }
    let delay: u64 = parts.next().and_then(|d| d.parse().ok()).unwrap_or(0);
    Some(Duration::from_millis(delay.min(3_000)))
}

/// The slots that go into the form, in order: the numbered slots from 1 up to
/// the first empty one. Stopping at a gap, rather than skipping it, keeps the
/// field each slot lands in predictable — slot 3 is always the third field.
pub fn fields(slot: impl Fn(u8) -> Option<String>) -> Vec<(u8, String)> {
    (1u8..)
        .take(MAX_FIELDS)
        .map_while(|n| slot(n).filter(|s| !s.is_empty()).map(|s| (n, s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_run_from_slot_one_to_the_first_gap() {
        let slots = |n: u8| match n {
            1 => Some("Ada Lovelace".to_string()),
            2 => Some("ada@example.com".to_string()),
            3 => Some(String::new()),
            4 => Some("never reached".to_string()),
            _ => None,
        };
        let got = fields(slots);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (1, "Ada Lovelace".to_string()));
        assert_eq!(got[1].0, 2);
    }

    #[test]
    fn nothing_in_slot_one_means_nothing_to_fill() {
        assert!(fields(|n| (n == 2).then(|| "x".to_string())).is_empty());
    }

    #[test]
    fn a_fill_never_runs_past_the_cap() {
        assert_eq!(fields(|_| Some("x".to_string())).len(), MAX_FIELDS);
    }

    #[test]
    fn stale_requests_are_dropped() {
        assert_eq!(parse_request("1000 250", 1100), Some(Duration::from_millis(250)));
        assert_eq!(parse_request("1000", 1100), Some(Duration::ZERO));
        assert_eq!(parse_request("1000 250", 1000 + FRESH_FOR.as_millis() + 1), None);
        assert_eq!(parse_request("1000 99999", 1000), Some(Duration::from_millis(3_000)));
        assert_eq!(parse_request("", 1000), None);
    }
}
