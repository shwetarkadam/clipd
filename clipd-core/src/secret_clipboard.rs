//! Putting a secret on the clipboard without it leaking into history.
//!
//! Pasting a password back is the whole point of a vault, but a naive
//! `set_text` creates two problems: clipd's own watcher sees the password on
//! the next poll and files it into history, and the secret then sits on the
//! clipboard indefinitely for any app to read.
//!
//! This module solves both. Copies are tagged with
//! [`CONCEALED_TYPE`] — the `org.nspasteboard.ConcealedType` convention that
//! password managers use to say "this is a secret, don't record it" — and are
//! wiped after a timeout. clipd honours the same flag on the way in via
//! [`clipboard_is_concealed`], so secrets copied out of 1Password, Bitwarden,
//! Keychain Access, or clipd's own vault never reach the database.

use std::time::Duration;

/// The de-facto standard pasteboard flavour marking a copy as secret.
///
/// Set by 1Password, Bitwarden, KeePassXC and friends; respected by
/// Maccy, CopyQ, Alfred and others. Honouring it is far more reliable than
/// guessing from the text whether something is a password.
pub const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";

/// How long a revealed secret stays on the clipboard before it is wiped.
/// Long enough to switch apps and paste, short enough to bound exposure.
pub const DEFAULT_CLEAR_AFTER: Duration = Duration::from_secs(30);

/// Place a secret on the clipboard, marked concealed and set to self-destruct.
///
/// Returns as soon as the secret is on the clipboard; the wipe runs on a
/// background thread after `clear_after` (defaults to [`DEFAULT_CLEAR_AFTER`]).
/// Passing `Some(Duration::ZERO)` disables the wipe.
///
/// **Only safe for processes that outlive `clear_after`** — the wipe thread dies
/// with the process, so a short-lived caller like a CLI command would leave the
/// password on the clipboard indefinitely. Those callers want
/// [`copy_secret_blocking`].
///
/// The wipe is conditional: if anything else has been copied in the meantime,
/// the clipboard is left alone rather than clobbering the user's work.
pub fn copy_secret(secret: &str, clear_after: Option<Duration>) -> Result<(), String> {
    let clear_after = clear_after.unwrap_or(DEFAULT_CLEAR_AFTER);
    let token = platform::put(secret)?;
    if !clear_after.is_zero() {
        let secret = secret.to_string();
        std::thread::Builder::new()
            .name("clipd-secret-wipe".into())
            .spawn(move || {
                std::thread::sleep(clear_after);
                report(platform::clear_if_unchanged(&token, &secret));
            })
            .map_err(|e| format!("Couldn't schedule the clipboard wipe: {e}"))?;
    }
    Ok(())
}

/// Like [`copy_secret`], but waits out the timeout and wipes before returning.
///
/// For short-lived callers (CLI commands) where a background thread would be
/// killed on exit. `on_copied` runs once the secret is on the clipboard, before
/// the wait begins, so the caller can print something while the user pastes.
pub fn copy_secret_blocking(
    secret: &str,
    clear_after: Option<Duration>,
    on_copied: impl FnOnce(),
) -> Result<(), String> {
    let clear_after = clear_after.unwrap_or(DEFAULT_CLEAR_AFTER);
    let token = platform::put(secret)?;
    on_copied();
    if !clear_after.is_zero() {
        std::thread::sleep(clear_after);
        report(platform::clear_if_unchanged(&token, secret));
    }
    Ok(())
}

fn report(outcome: Result<bool, String>) {
    match outcome {
        Ok(true) => log::debug!("Wiped a revealed secret from the clipboard"),
        Ok(false) => log::debug!("Clipboard moved on; leaving it alone instead of wiping"),
        Err(e) => log::warn!("Couldn't wipe the secret from the clipboard: {e}"),
    }
}

/// Whether whatever is on the clipboard right now is flagged as a secret.
///
/// Always false off macOS: the convention is a pasteboard flavour, and no
/// equivalent exists on the Windows or X11 clipboards.
pub fn clipboard_is_concealed() -> bool {
    platform::is_concealed()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::CONCEALED_TYPE;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::{NSArray, NSString};

    /// The pasteboard's change count at the moment we wrote. Comparing it later
    /// detects any intervening copy, including one with identical text.
    pub type Token = isize;

    pub fn put(secret: &str) -> Result<Token, String> {
        // SAFETY: these are immutable framework string constants.
        let plain_text = unsafe { NSPasteboardTypeString };
        let concealed = NSString::from_str(CONCEALED_TYPE);

        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        // SAFETY: both entries are valid pasteboard type strings, and passing a
        // nil owner means the data is provided eagerly below rather than lazily
        // called back on a dangling owner.
        unsafe { pb.declareTypes_owner(&NSArray::from_slice(&[plain_text, &concealed]), None) };

        if !pb.setString_forType(&NSString::from_str(secret), plain_text) {
            return Err("The pasteboard refused the text.".into());
        }
        // The flag's presence is what matters, not its payload; an empty string
        // keeps us from writing the secret a second time.
        pb.setString_forType(&NSString::from_str(""), &concealed);
        Ok(pb.changeCount())
    }

    pub fn clear_if_unchanged(token: &Token, _secret: &str) -> Result<bool, String> {
        let pb = NSPasteboard::generalPasteboard();
        if pb.changeCount() != *token {
            return Ok(false);
        }
        pb.clearContents();
        Ok(true)
    }

    pub fn is_concealed() -> bool {
        let Some(types) = NSPasteboard::generalPasteboard().types() else {
            return false;
        };
        types.iter().any(|t| t.to_string() == CONCEALED_TYPE)
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    /// Without a pasteboard-flavour equivalent, the best available check is
    /// whether the text is still the secret we wrote.
    pub type Token = ();

    pub fn put(secret: &str) -> Result<Token, String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
        cb.set_text(secret)
            .map_err(|e| format!("Couldn't write to the clipboard: {e}"))?;
        Ok(())
    }

    pub fn clear_if_unchanged(_token: &Token, secret: &str) -> Result<bool, String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
        if cb.get_text().ok().as_deref() != Some(secret) {
            return Ok(false);
        }
        cb.set_text("")
            .map_err(|e| format!("Couldn't clear the clipboard: {e}"))?;
        Ok(true)
    }

    pub fn is_concealed() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises access and skips unless clipboard tests are opted into with
    /// `CLIPD_CLIPBOARD_TESTS=1`, since these clobber the real clipboard.
    ///
    /// Uses the crate-wide guard rather than a local one: the pasteboard tests
    /// write to the same single system clipboard, and a module-local mutex only
    /// stops this module racing itself.
    #[cfg(target_os = "macos")]
    fn clipboard_guard() -> Option<std::sync::MutexGuard<'static, ()>> {
        std::env::var("CLIPD_CLIPBOARD_TESTS").ok()?;
        Some(crate::pasteboard::test_serial())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn concealed_copy_is_flagged_and_wiped() {
        let Some(_guard) = clipboard_guard() else {
            return;
        };
        copy_secret("hunter2", Some(Duration::from_millis(300))).expect("copy");
        assert!(
            clipboard_is_concealed(),
            "a secret copy must carry the concealed flag"
        );
        assert_eq!(
            arboard::Clipboard::new().unwrap().get_text().unwrap(),
            "hunter2"
        );

        std::thread::sleep(Duration::from_millis(900));
        let after = arboard::Clipboard::new()
            .unwrap()
            .get_text()
            .unwrap_or_default();
        assert!(after.is_empty(), "secret should have been wiped, got {after:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wipe_does_not_clobber_a_later_copy() {
        let Some(_guard) = clipboard_guard() else {
            return;
        };
        copy_secret("hunter2", Some(Duration::from_millis(300))).expect("copy");
        arboard::Clipboard::new()
            .unwrap()
            .set_text("something the user copied")
            .unwrap();

        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(
            arboard::Clipboard::new().unwrap().get_text().unwrap(),
            "something the user copied",
            "the wipe must not destroy unrelated clipboard content"
        );
    }
}
