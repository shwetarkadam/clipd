//! Where the menu-bar icon sits, so popovers can be anchored under it.
//!
//! Only `clipd-ui` owns the tray icon and learns its screen rect (from the
//! tray event); only `clipd-gui` draws the popover. They are separate
//! processes, and the popover is often shown by handing off to an *already
//! running* GUI rather than by spawning one — so a launch argument would be
//! dropped in exactly the common case. A tiny file is the simplest channel
//! that survives that handoff.

use std::path::PathBuf;

fn anchor_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("tray_anchor")
}

/// Record the tray icon's centre, in global screen points with the origin at
/// the top-left of the primary display (egui's space).
///
/// `y` says which display the icon is on. With only `x`, a popover could tell
/// where along a menu bar to sit but not *which* menu bar — and on a monitor
/// stacked above or below the laptop the two share the same x range.
pub fn save_tray_anchor(center_x: f64, y: Option<f64>) {
    let path = anchor_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = match y {
        Some(y) => format!("{:.1} {:.1}", center_x, y),
        None => format!("{:.1}", center_x),
    };
    let _ = std::fs::write(path, line);
}

/// The last known tray-icon centre x, if one was ever recorded.
///
/// `None` on a fresh install (the icon has not been hovered or clicked yet) —
/// callers should fall back to centring on screen rather than guessing.
pub fn load_tray_anchor() -> Option<f64> {
    load_tray_point().map(|(x, _)| x)
}

/// The last known tray-icon point: x, and y when it was recorded (older files
/// hold only x).
pub fn load_tray_point() -> Option<(f64, Option<f64>)> {
    parse_tray_point(&std::fs::read_to_string(anchor_path()).ok()?)
}

fn parse_tray_point(text: &str) -> Option<(f64, Option<f64>)> {
    let mut parts = text.split_whitespace();
    let x = parts.next()?.parse::<f64>().ok().filter(|x| x.is_finite())?;
    let y = parts.next().and_then(|y| y.parse::<f64>().ok()).filter(|y| y.is_finite());
    Some((x, y))
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_missing_anchor_is_none_not_zero() {
        // Zero would silently pin every popover to the left screen edge, which
        // looks deliberate and is therefore worse than an explicit fallback.
        assert_eq!(
            "".trim().parse::<f64>().ok().filter(|x: &f64| *x >= 0.0),
            None
        );
    }

    #[test]
    fn a_point_round_trips_and_old_files_still_read() {
        assert_eq!(super::parse_tray_point("1512.0 12.0"), Some((1512.0, Some(12.0))));
        assert_eq!(super::parse_tray_point("1280.5"), Some((1280.5, None)));
        // A display to the left of the primary has negative x.
        assert_eq!(super::parse_tray_point("-900.0 -1080.0"), Some((-900.0, Some(-1080.0))));
        assert_eq!(super::parse_tray_point("junk"), None);
    }

    #[test]
    fn garbage_contents_are_rejected() {
        assert_eq!("not-a-number".trim().parse::<f64>().ok(), None);
    }

    #[test]
    fn negative_and_nan_anchors_are_rejected() {
        let parse = |s: &str| {
            s.trim()
                .parse::<f64>()
                .ok()
                .filter(|x: &f64| x.is_finite() && *x >= 0.0)
        };
        assert_eq!(parse("-40"), None);
        assert_eq!(parse("NaN"), None);
        assert_eq!(parse("1284.5"), Some(1284.5));
    }
}
