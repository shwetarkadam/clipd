//! Which physical key types a given letter, for keystrokes clipd sends itself.
//!
//! A posted CGEvent names a *key*, not a character. ⌘V on QWERTY is the key in
//! the V position, but the same key is ⌘. on Dvorak, and the key in QWERTY's Z
//! position is W on AZERTY — so an "undo" sent by position would be ⌘W, and
//! close the person's window. The answer depends on the keyboard layout, so it
//! is looked up in that layout rather than assumed.
//!
//! The lookup uses Text Input Sources, which must run on the main thread (calling
//! them from a background thread is what made rdev crash on macOS 15). So the
//! tray host refreshes this table on its main thread, and the daemon — which
//! sends the keystrokes from its own threads — only ever reads it. Until the
//! first refresh, every lookup says "unknown" and callers fall back to a
//! layout-aware but slow path.

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
        static kTISPropertyUnicodeKeyLayoutData: *const c_void;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            layout: *const c_void,
            virtual_key: u16,
            action: u16,
            modifier_state: u32,
            keyboard_type: u32,
            options: u32,
            dead_key_state: *mut u32,
            max_len: usize,
            actual_len: *mut usize,
            chars: *mut u16,
        ) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFRelease(cf: *const c_void);
    }

    /// kUCKeyActionDisplay
    const ACTION_DISPLAY: u16 = 3;
    /// cmdKey >> 8. Translating with ⌘ held is what makes "Dvorak – QWERTY ⌘"
    /// right: it types Dvorak, but its shortcuts sit on the QWERTY keys.
    const COMMAND_MODIFIER: u32 = 0x01;
    /// kUCKeyTranslateNoDeadKeysMask
    const NO_DEAD_KEYS: u32 = 1;

    use super::{codes_for, LETTERS};
    static CODES: [AtomicI32; 3] = [AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1)];
    static READY: AtomicBool = AtomicBool::new(false);

    /// Re-read the layout. Main thread only — see the module docs.
    pub fn refresh() {
        let found = unsafe { lookup() };
        let mut changed = !READY.load(Ordering::Acquire);
        for (slot, code) in CODES.iter().zip(found) {
            let code = code.map_or(-1, i32::from);
            changed |= slot.swap(code, Ordering::Relaxed) != code;
        }
        READY.store(true, Ordering::Release);
        if changed {
            log::info!("keyboard layout: ⌘C/⌘V/⌘Z are key codes {:?}", found);
        }
    }

    pub fn keycode(letter: char) -> Option<u16> {
        if !READY.load(Ordering::Acquire) {
            return None;
        }
        let i = LETTERS.iter().position(|&l| l == letter)?;
        u16::try_from(CODES[i].load(Ordering::Relaxed)).ok()
    }

    pub fn is_ready() -> bool {
        READY.load(Ordering::Acquire)
    }

    unsafe fn lookup() -> [Option<u16>; 3] {
        let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
        if source.is_null() {
            return [None; 3];
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let out = if data.is_null() {
            [None; 3]
        } else {
            let layout = CFDataGetBytePtr(data) as *const c_void;
            let kbd = u32::from(LMGetKbdType());
            codes_for(|code| {
                let mut dead = 0u32;
                let mut len = 0usize;
                let mut chars = [0u16; 4];
                let status = UCKeyTranslate(
                    layout,
                    code,
                    ACTION_DISPLAY,
                    COMMAND_MODIFIER,
                    kbd,
                    NO_DEAD_KEYS,
                    &mut dead,
                    chars.len(),
                    &mut len,
                    chars.as_mut_ptr(),
                );
                if status != 0 || len != 1 {
                    return None;
                }
                char::from_u32(u32::from(chars[0]))
            })
        };
        CFRelease(source);
        out
    }
}

/// The letters clipd ever presses for itself: copy, paste, undo.
const LETTERS: [char; 3] = ['c', 'v', 'z'];

/// Which key code types each of [`LETTERS`], given what each code types.
/// Pure, so it can be tested without touching the real layout.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn codes_for(types: impl Fn(u16) -> Option<char>) -> [Option<u16>; 3] {
    let mut out = [None; 3];
    for code in 0u16..128 {
        let Some(ch) = types(code) else { continue };
        let ch = ch.to_ascii_lowercase();
        if let Some(i) = LETTERS.iter().position(|&l| l == ch) {
            // First match wins: the main block comes before the keypad.
            out[i].get_or_insert(code);
        }
    }
    out
}

#[cfg(target_os = "macos")]
pub use imp::{is_ready, keycode, refresh};

#[cfg(not(target_os = "macos"))]
pub fn refresh() {}
#[cfg(not(target_os = "macos"))]
pub fn keycode(_letter: char) -> Option<u16> {
    None
}
#[cfg(not(target_os = "macos"))]
pub fn is_ready() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::codes_for;

    /// Key codes in the QWERTY positions of C, V, Z, W and period.
    const C: u16 = 8;
    const V: u16 = 9;
    const Z: u16 = 6;
    const W: u16 = 13;
    const PERIOD: u16 = 47;
    const I: u16 = 34;
    const SLASH: u16 = 44;

    #[test]
    fn qwerty_keys_are_where_the_letters_are() {
        let qwerty = |code| match code {
            C => Some('c'),
            V => Some('v'),
            Z => Some('z'),
            W => Some('w'),
            _ => None,
        };
        assert_eq!(codes_for(qwerty), [Some(C), Some(V), Some(Z)]);
    }

    #[test]
    fn azerty_undo_is_not_the_qwerty_z_key() {
        // On AZERTY the QWERTY-Z key types W. Undo sent by position would be
        // ⌘W — closing the person's window — so it must come from the layout.
        let azerty = |code| match code {
            C => Some('c'),
            V => Some('v'),
            Z => Some('w'),
            W => Some('z'),
            _ => None,
        };
        assert_eq!(codes_for(azerty), [Some(C), Some(V), Some(W)]);
    }

    #[test]
    fn dvorak_moves_all_three_and_uppercase_still_matches() {
        let dvorak = |code| match code {
            I => Some('C'),
            PERIOD => Some('V'),
            SLASH => Some('Z'),
            C => Some('j'),
            _ => None,
        };
        assert_eq!(codes_for(dvorak), [Some(I), Some(PERIOD), Some(SLASH)]);
    }

    #[test]
    fn a_layout_without_the_letter_reports_unknown() {
        assert_eq!(codes_for(|_| None), [None, None, None]);
    }
}
