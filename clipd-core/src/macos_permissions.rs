//! macOS keyboard-permission helpers for multi-slot copy.
//!
//! Multi-tap Cmd+C / Cmd+V needs a CGEventTap. On modern macOS that tap is
//! gated by **Accessibility** (modifying tap) and **Input Monitoring** (listen).
//! Without both, rdev's `grab` returns `EventTapError` and slots go dark while
//! ordinary clipboard history keeps working — a silent, confusing failure.
//!
//! These helpers:
//! 1. Ask macOS to show the consent dialogs / list Clipd under Privacy
//! 2. Report whether the grants are actually in place
//! 3. Open the System Settings panes so the user can flip the toggles

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::string::CFStringRef;
use std::process::Command;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: core_foundation_sys::dictionary::CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

/// Whether Accessibility (for a modifying event tap) is currently granted.
pub fn accessibility_granted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Whether Input Monitoring is currently granted.
pub fn input_monitoring_granted() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}

/// True when both grants the multi-slot listener needs are in place.
pub fn keyboard_permissions_granted() -> bool {
    accessibility_granted() && input_monitoring_granted()
}

/// Prompt macOS only for the keyboard permissions that are still missing.
///
/// In particular, do not pass `kAXTrustedCheckOptionPrompt` after Accessibility
/// has already been granted. The daemon may retry its event tap while TCC is
/// settling, and prompting unconditionally here can make macOS present the
/// Accessibility sheet again even though the existing grant is valid.
pub fn request_keyboard_permissions() -> bool {
    // Input Monitoring first. Checking Accessibility with a prompt before
    // requesting Input Monitoring can suppress the IM dialog (rdar://7381305).
    let im = if input_monitoring_granted() {
        true
    } else {
        unsafe { CGRequestListenEventAccess() }
    };

    let ax = if accessibility_granted() {
        true
    } else {
        unsafe {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
            let value = CFBoolean::true_value();
            let dict = CFDictionary::from_CFType_pairs(&[(key, value)]);
            AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef())
        }
    };

    log::info!(
        "macOS keyboard permissions: Accessibility={} InputMonitoring={}",
        ax,
        im
    );
    ax && im
}

/// Open System Settings to the Accessibility and Input Monitoring panes.
///
/// Uses both the Ventura+ Settings URLs and the legacy Preference Pane URLs
/// so at least one lands on a useful screen across macOS versions.
pub fn open_keyboard_permission_settings() {
    let urls = [
        "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ListenEvent",
        "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    ];
    for url in urls {
        let _ = Command::new("open").arg(url).spawn();
    }
}

/// Short human label for the missing permission(s), for HUD / banner copy.
/// A request to ask macOS for keyboard access, waiting for the main thread.
///
/// The ask comes from a background thread (the daemon, after ⌘C twice), but
/// macOS only presents its permission sheet for a request made on the main
/// thread. The tray's event loop takes this and asks.
static KEYBOARD_ACCESS_WANTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ask for keyboard access as soon as the main thread can.
pub fn want_keyboard_access() {
    KEYBOARD_ACCESS_WANTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// True once per `want_keyboard_access` call. Main thread only.
pub fn take_keyboard_access_request() -> bool {
    KEYBOARD_ACCESS_WANTED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Whether the ⌘C ×2 prompt may be shown now: at most three times ever, two
/// days apart. The person can always turn access on from Settings or the tray.
pub fn keyboard_ask_due() -> bool {
    let (count, last) = read_keyboard_asks();
    keyboard_ask_due_at(count, last, chrono::Utc::now().timestamp())
}

fn keyboard_ask_due_at(count: u32, last: i64, now: i64) -> bool {
    count < 3 && now - last >= 2 * 24 * 60 * 60
}

/// Note that the ⌘C ×2 prompt was shown.
pub fn record_keyboard_ask() {
    let (count, _) = read_keyboard_asks();
    let path = keyboard_ask_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{} {}", count + 1, chrono::Utc::now().timestamp()));
}

fn keyboard_ask_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join("keyboard_ask")
}

fn read_keyboard_asks() -> (u32, i64) {
    let text = std::fs::read_to_string(keyboard_ask_path()).unwrap_or_default();
    let mut parts = text.split_whitespace();
    let count = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let last = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (count, last)
}

pub fn missing_keyboard_permission_label() -> &'static str {
    match (accessibility_granted(), input_monitoring_granted()) {
        (false, false) => "Accessibility and Input Monitoring",
        (false, true) => "Accessibility",
        (true, false) => "Input Monitoring",
        (true, true) => "keyboard access",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_double_copy_prompt_is_rationed() {
        let day = 24 * 60 * 60;
        assert!(super::keyboard_ask_due_at(0, 0, 10 * day));
        assert!(!super::keyboard_ask_due_at(1, 9 * day, 10 * day), "two days apart");
        assert!(super::keyboard_ask_due_at(1, 7 * day, 10 * day));
        assert!(!super::keyboard_ask_due_at(3, 0, 10 * day), "three times at most");
    }
}
