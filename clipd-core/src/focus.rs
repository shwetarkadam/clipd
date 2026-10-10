//! Handing the keyboard to another clipd process.
//!
//! clipd's main window lives in its own process. Since macOS 14, activation
//! is cooperative: an app in the background asking to come to the front is
//! usually refused, so the main window showing itself left the keyboard with
//! whatever had it — the tray host, after a click on the popover. The process
//! the person just interacted with (the tray host, the popover) is allowed to
//! pass activation on, so it does: it yields to the main window's process and
//! activates it. The main window then accepts with `NSApp.activate()`.

/// Hand the keyboard to the clipd surface `name` (`gui-main`), if it is
/// running. Call from the process that just received the person's click or
/// shortcut. Main thread for the yield; the activation works from any thread.
#[cfg(target_os = "macos")]
pub fn hand_focus_to_surface(name: &str) {
    use objc2::runtime::NSObjectProtocol;
    use objc2::sel;
    use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSRunningApplication};

    let Some(pid) = crate::lock::surface_pid(name) else { return };
    let Some(target) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32) else {
        return;
    };
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let me = NSApplication::sharedApplication(mtm);
        // You can only hand over what you hold. The popover is a
        // non-activating panel, so clicking it leaves the person's app
        // active; take activation first — the click just made us eligible —
        // then pass it on.
        if !me.isActive() {
            if me.respondsToSelector(sel!(activate)) {
                me.activate();
            } else {
                #[allow(deprecated)]
                me.activateIgnoringOtherApps(true);
            }
        }
        // macOS 14+: say who may take the focus next. Older systems lack the
        // method, and activation is not cooperative there anyway.
        if me.respondsToSelector(sel!(yieldActivationToApplication:)) {
            me.yieldActivationToApplication(&target);
        }
    }
    #[allow(deprecated)]
    target.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
}

#[cfg(not(target_os = "macos"))]
pub fn hand_focus_to_surface(_name: &str) {}

/// Take the keyboard in this process, as the receiving end of
/// `hand_focus_to_surface`. Main thread only.
#[cfg(target_os = "macos")]
pub fn take_focus() {
    use objc2::runtime::NSObjectProtocol;
    use objc2::sel;
    use objc2_app_kit::NSApplication;
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    if app.isHidden() {
        app.unhide(None);
    }
    if app.respondsToSelector(sel!(activate)) {
        app.activate();
    } else {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn take_focus() {}
