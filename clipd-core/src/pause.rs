//! Pausing capture: "don't record anything for a while".
//!
//! Some stretches of work should leave no history: a screen share, a banking
//! session, anything you would rather not find in search next week. A pause
//! stops clips reaching history until it runs out or is lifted; slots you fill
//! on purpose with ⌘C×N still work, because that is an explicit act, but
//! nothing is written to history meanwhile.
//!
//! The state is a small file rather than memory, so every clipd process — the
//! tray that offers the menu, the daemon that records, the window that shows a
//! badge — agrees on it, and a pause survives a restart for as long as it was
//! meant to last.

use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How long to pause for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseFor {
    Minutes(i64),
    /// Until local midnight tonight.
    UntilTomorrow,
    /// Until lifted by hand.
    Indefinitely,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PauseState {
    /// `None` means until lifted.
    until: Option<DateTime<Utc>>,
}

fn state_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("capture_pause.json")
}

/// When a pause of `length` started at `now` ends. `None` for indefinitely.
fn ends_at(length: PauseFor, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match length {
        PauseFor::Minutes(m) => Some(now + Duration::minutes(m.max(1))),
        PauseFor::UntilTomorrow => {
            let local = now.with_timezone(&Local);
            let midnight = local.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
            Local
                .from_local_datetime(&midnight)
                .earliest()
                .map(|t| t.with_timezone(&Utc))
        }
        PauseFor::Indefinitely => None,
    }
}

/// Stop recording clips into history.
pub fn pause(length: PauseFor) {
    let state = PauseState {
        until: ends_at(length, Utc::now()),
    };
    let path = state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&state) {
        let _ = std::fs::write(path, json);
    }
    match state.until {
        Some(t) => log::info!("⏸  Capture paused until {}", t.with_timezone(&Local).format("%H:%M")),
        None => log::info!("⏸  Capture paused until resumed"),
    }
}

/// Start recording again.
pub fn resume() {
    if std::fs::remove_file(state_path()).is_ok() {
        log::info!("▶️  Capture resumed");
    }
}

/// The current pause: `Some(None)` = paused until lifted, `Some(Some(t))` =
/// paused until `t`, `None` = recording. An expired pause is cleared here.
pub fn current() -> Option<Option<DateTime<Utc>>> {
    let raw = std::fs::read_to_string(state_path()).ok()?;
    let state: PauseState = serde_json::from_str(&raw).ok()?;
    match state.until {
        Some(t) if t <= Utc::now() => {
            let _ = std::fs::remove_file(state_path());
            None
        }
        until => Some(until),
    }
}

pub fn is_paused() -> bool {
    current().is_some()
}

/// "Paused until 14:30", "Paused until resumed", or `None` when recording.
pub fn status_label() -> Option<String> {
    current().map(|until| match until {
        Some(t) => format!("Capture paused until {}", t.with_timezone(&Local).format("%H:%M")),
        None => "Capture paused until resumed".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_and_indefinite_end_where_they_should() {
        let now = Utc::now();
        assert_eq!(ends_at(PauseFor::Minutes(15), now), Some(now + Duration::minutes(15)));
        assert_eq!(ends_at(PauseFor::Indefinitely, now), None);
        // A zero or negative length still pauses for a moment, never "forever".
        assert_eq!(ends_at(PauseFor::Minutes(0), now), Some(now + Duration::minutes(1)));
    }

    #[test]
    fn until_tomorrow_is_the_next_local_midnight() {
        let now = Utc::now();
        let end = ends_at(PauseFor::UntilTomorrow, now).expect("a time");
        let local = end.with_timezone(&Local);
        assert!(end > now);
        assert!(end - now <= Duration::hours(25), "at most a day away (DST included)");
        assert_eq!(local.format("%H:%M").to_string(), "00:00");
    }
}
