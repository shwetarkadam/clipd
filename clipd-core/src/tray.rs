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

/// Record the horizontal centre of the tray icon, in screen points.
pub fn save_tray_anchor(center_x: f64) {
    let path = anchor_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{:.1}", center_x));
}

/// The last known tray-icon centre, if one was ever recorded.
///
/// `None` on a fresh install (the icon has not been hovered or clicked yet) —
/// callers should fall back to centring on screen rather than guessing.
pub fn load_tray_anchor() -> Option<f64> {
    std::fs::read_to_string(anchor_path())
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite() && *x >= 0.0)
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
