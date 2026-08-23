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
pub fn missing_keyboard_permission_label() -> &'static str {
    match (accessibility_granted(), input_monitoring_granted()) {
        (false, false) => "Accessibility and Input Monitoring",
        (false, true) => "Accessibility",
        (true, false) => "Input Monitoring",
        (true, true) => "keyboard access",
    }
}
