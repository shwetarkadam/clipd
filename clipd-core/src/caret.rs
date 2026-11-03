//! Where the text cursor is, so the palette can open next to it.
//!
//! Summoned from the keyboard, the palette is about to paste into whatever you
//! were typing in. Opening it at the text cursor keeps your eyes where they
//! already are; opening it mid-screen makes you look away and back.
//!
//! The caret is read through Accessibility (the same trust clipd already needs
//! to paste for you): the focused element's selected range, then the screen
//! bounds of that range. It has to be read *before* clipd takes focus, and in
//! the process that has the trust — the daemon — while the palette is a
//! separate, often already-running process. So, like the tray anchor, the
//! answer travels as a small file, stamped with when it was taken so a stale
//! one from an earlier summon is never used.
//!
//! Many apps do not report a caret (Terminal, most Electron apps, games). Then
//! there is no file and the palette opens where it always has.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A rectangle in global screen points, origin at the top-left of the primary
/// display (egui's space, and Accessibility's).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// An anchor older than this belongs to some earlier summon.
const FRESH_FOR: Duration = Duration::from_secs(3);

fn anchor_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("caret_anchor")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Read the caret of the focused app and leave it for the palette. Clears any
/// older anchor when there is no caret to read, so the palette never opens at
/// a place you have since left.
pub fn capture() {
    let path = anchor_path();
    match read_caret() {
        Some(r) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(
                &path,
                format!("{} {:.1} {:.1} {:.1} {:.1}", now_ms(), r.x, r.y, r.width, r.height),
            );
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Where the text cursor is right now, read directly. For overlays drawn by
/// the daemon itself, which has the Accessibility trust to ask.
pub fn current() -> Option<CaretRect> {
    read_caret()
}

/// The caret captured for this summon, if any. Taking it removes it: one
/// summon, one placement.
pub fn take_fresh() -> Option<CaretRect> {
    let path = anchor_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    parse(&raw, now_ms())
}

fn parse(raw: &str, now: u128) -> Option<CaretRect> {
    let mut parts = raw.split_whitespace();
    let taken: u128 = parts.next()?.parse().ok()?;
    if now.saturating_sub(taken) > FRESH_FOR.as_millis() {
        return None;
    }
    let mut next = || parts.next()?.parse::<f64>().ok().filter(|v| v.is_finite());
    let rect = CaretRect {
        x: next()?,
        y: next()?,
        width: next()?,
        height: next()?,
    };
    plausible(rect).then_some(rect)
}

/// Apps that do not really know report a zero rect at the origin, or a
/// "caret" the height of a page. Neither is a place to open.
fn plausible(r: CaretRect) -> bool {
    r.height > 0.0 && r.height < 400.0 && r.width >= 0.0 && !(r.x == 0.0 && r.y == 0.0)
}

/// Where a window of `size` goes so it sits next to `caret` without covering
/// it, kept on `screen` (min x, min y, max x, max y). Below the line when it
/// fits, above when that fits, otherwise beside it.
pub fn place_near(caret: CaretRect, size: (f64, f64), screen: (f64, f64, f64, f64)) -> (f64, f64) {
    const GAP: f64 = 6.0;
    const MARGIN: f64 = 8.0;
    let (w, h) = size;
    let (left, top, right, bottom) = screen;
    let clamp_x = |x: f64| x.clamp(left + MARGIN, (right - w - MARGIN).max(left + MARGIN));
    let clamp_y = |y: f64| y.clamp(top + MARGIN, (bottom - h - MARGIN).max(top + MARGIN));

    let below = caret.y + caret.height + GAP;
    if below + h <= bottom - MARGIN {
        return (clamp_x(caret.x - 24.0), below);
    }
    let above = caret.y - GAP - h;
    if above >= top + MARGIN {
        return (clamp_x(caret.x - 24.0), above);
    }
    // Too tall for either side of the line: stand beside the caret instead,
    // on whichever side has room.
    let y = clamp_y(caret.y - h * 0.3);
    let right_of = caret.x + caret.width + 16.0;
    let x = if right_of + w <= right - MARGIN {
        right_of
    } else {
        clamp_x(caret.x - 16.0 - w)
    };
    (x, y)
}

#[cfg(target_os = "macos")]
fn read_caret() -> Option<CaretRect> {
    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
    use core_foundation::string::CFString;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct CFRange {
        location: isize,
        length: isize,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> CFTypeRef;
        fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
        fn AXUIElementCopyAttributeValue(element: CFTypeRef, attribute: CFTypeRef, value: *mut CFTypeRef) -> i32;
        fn AXUIElementCopyParameterizedAttributeValue(
            element: CFTypeRef,
            attribute: CFTypeRef,
            parameter: CFTypeRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXValueCreate(kind: u32, value: *const c_void) -> CFTypeRef;
        fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
    }
    const AX_VALUE_CGRECT: u32 = 3;
    const AX_VALUE_CFRANGE: u32 = 4;
    // An app that is busy must not hold up the palette: it opens where it
    // always did instead.
    const TIMEOUT_S: f32 = 0.05;

    struct Owned(CFTypeRef);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }
    unsafe fn copy(element: CFTypeRef, name: &str) -> Option<Owned> {
        let attr = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(element, attr.as_CFTypeRef(), &mut out);
        (err == 0 && !out.is_null()).then(|| Owned(out))
    }
    unsafe fn bounds(element: CFTypeRef, range: CFRange) -> Option<CGRect> {
        let param = Owned(AXValueCreate(AX_VALUE_CFRANGE, &range as *const _ as *const c_void));
        if param.0.is_null() {
            return None;
        }
        let attr = CFString::new("AXBoundsForRange");
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyParameterizedAttributeValue(element, attr.as_CFTypeRef(), param.0, &mut out);
        if err != 0 || out.is_null() {
            return None;
        }
        let value = Owned(out);
        let mut rect = CGRect::default();
        AXValueGetValue(value.0, AX_VALUE_CGRECT, &mut rect as *mut _ as *mut c_void).then_some(rect)
    }

    unsafe {
        let system = Owned(AXUIElementCreateSystemWide());
        if system.0.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(system.0, TIMEOUT_S);
        let focused = copy(system.0, "AXFocusedUIElement")?;
        AXUIElementSetMessagingTimeout(focused.0, TIMEOUT_S);
        let selected = copy(focused.0, "AXSelectedTextRange")?;
        let mut range = CFRange::default();
        if !AXValueGetValue(selected.0, AX_VALUE_CFRANGE, &mut range as *mut _ as *mut c_void) {
            return None;
        }
        // An empty selection is a bare caret; some apps only answer for a
        // range with a character in it.
        let rect = bounds(focused.0, range)
            .filter(|r| r.height > 0.0)
            .or_else(|| {
                bounds(
                    focused.0,
                    CFRange {
                        location: range.location,
                        length: 1,
                    },
                )
            })?;
        let caret = CaretRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };
        plausible(caret).then_some(caret)
    }
}

#[cfg(not(target_os = "macos"))]
fn read_caret() -> Option<CaretRect> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (f64, f64, f64, f64) = (0.0, 0.0, 1512.0, 982.0);
    const PALETTE: (f64, f64) = (600.0, 740.0);

    fn caret(x: f64, y: f64) -> CaretRect {
        CaretRect {
            x,
            y,
            width: 1.0,
            height: 18.0,
        }
    }

    #[test]
    fn a_fresh_anchor_is_read_and_a_stale_one_is_not() {
        let line = "1000 300.0 120.0 1.0 18.0";
        assert_eq!(parse(line, 1500), Some(caret(300.0, 120.0)));
        assert_eq!(parse(line, 1000 + FRESH_FOR.as_millis() + 1), None);
        assert_eq!(parse("garbage", 1000), None);
    }

    #[test]
    fn apps_that_do_not_know_are_ignored() {
        assert_eq!(parse("1000 0.0 0.0 0.0 0.0", 1000), None);
        assert_eq!(parse("1000 10.0 10.0 1.0 0.0", 1000), None);
        assert_eq!(parse("1000 10.0 10.0 1.0 900.0", 1000), None);
    }

    #[test]
    fn opens_below_the_line_when_it_fits() {
        let (x, y) = place_near(caret(400.0, 100.0), PALETTE, SCREEN);
        assert_eq!(x, 376.0);
        assert_eq!(y, 124.0);
    }

    #[test]
    fn opens_above_near_the_bottom() {
        let (_, y) = place_near(caret(400.0, 900.0), PALETTE, SCREEN);
        assert_eq!(y, 900.0 - 6.0 - 740.0);
    }

    #[test]
    fn beside_the_caret_when_neither_fits_and_never_off_screen() {
        // Mid-screen on a short display: no room above or below.
        let (x, y) = place_near(caret(200.0, 400.0), PALETTE, SCREEN);
        assert!(x > 200.0, "to the right of the caret");
        assert!(y >= 8.0 && y + 740.0 <= 982.0 - 8.0 + 0.01);
        // Near the right edge it goes to the left instead.
        let (x, _) = place_near(caret(1400.0, 400.0), PALETTE, SCREEN);
        assert!(x + 600.0 <= 1400.0);
        // Near the left edge it never leaves the screen.
        let (x, _) = place_near(caret(2.0, 100.0), PALETTE, SCREEN);
        assert_eq!(x, 8.0);
    }
}
