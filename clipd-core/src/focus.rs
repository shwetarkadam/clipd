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
/// shortcut.
///
/// The yield has to happen on the main thread. The shortcut path calls this
/// from the daemon's hotkey thread, where the yield used to be skipped without
/// a word — leaving only an activation request that macOS 14+ refuses, so the
/// window opened with the keyboard still in the previous app and ↑/↓ went
/// there instead. Off the main thread, this now runs itself on it, and waits:
/// the yield must land before the window asks for the keyboard.
#[cfg(target_os = "macos")]
pub fn hand_focus_to_surface(name: &str) {
    use objc2::runtime::NSObjectProtocol;
    use objc2::sel;
    use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSRunningApplication};

    if objc2::MainThreadMarker::new().is_none() {
        main_thread::run_sync(|| hand_focus_to_surface(name));
        return;
    }

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

#[cfg(target_os = "macos")]
mod main_thread {
    use std::ffi::c_void;

    #[repr(C)]
    struct DispatchQueue {
        _private: [u8; 0],
    }
    extern "C" {
        // dispatch_get_main_queue() is a macro for the address of this.
        static _dispatch_main_q: DispatchQueue;
        fn dispatch_sync_f(queue: *const DispatchQueue, context: *mut c_void, work: extern "C" fn(*mut c_void));
    }

    /// Run `f` on the main thread and wait for it. Needs a main thread that is
    /// running its run loop, as the tray host's is; never call it *from* the
    /// main thread (callers check first).
    pub(super) fn run_sync<F: FnOnce()>(f: F) {
        extern "C" fn trampoline<F: FnOnce()>(ctx: *mut c_void) {
            // SAFETY: `ctx` is the `Option<F>` below, alive for the whole
            // synchronous call, and taken exactly once.
            let slot = unsafe { &mut *(ctx as *mut Option<F>) };
            if let Some(f) = slot.take() {
                f();
            }
        }
        let mut slot = Some(f);
        unsafe {
            dispatch_sync_f(&_dispatch_main_q, &mut slot as *mut Option<F> as *mut c_void, trampoline::<F>);
        }
    }
}

/// Set by a process whose main thread runs an AppKit run loop (the tray host).
static APPKIT_LOOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Note that this process's main thread is running an AppKit run loop, which
/// is what keeps NSWorkspace's idea of the frontmost app current.
pub fn mark_appkit_main_loop() {
    APPKIT_LOOP.store(true, std::sync::atomic::Ordering::Release);
}

/// The frontmost app's name and bundle id, from NSWorkspace — instantly,
/// rather than the ~200ms an `osascript` round trip to System Events costs.
///
/// `None` when this process has no AppKit main loop (a headless `clipd
/// daemon`): NSWorkspace would answer with whatever was frontmost when the
/// process started, so callers fall back to asking System Events.
#[cfg(target_os = "macos")]
pub fn frontmost_application() -> Option<(String, String)> {
    use objc2_app_kit::NSWorkspace;
    if !APPKIT_LOOP.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    // The localized name is what System Events calls the process ("Code" for
    // VS Code, whose executable is "Electron"), so names stored by either
    // path keep meaning the same app.
    let name = app.localizedName().map(|n| n.to_string()).or_else(|| {
        app.executableURL()
            .and_then(|url| url.lastPathComponent())
            .map(|n| n.to_string())
    })?;
    let bundle = app.bundleIdentifier().map(|b| b.to_string()).unwrap_or_default();
    Some((name, bundle))
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_application() -> Option<(String, String)> {
    None
}

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
