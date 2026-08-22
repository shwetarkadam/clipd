//! Serialized access to the system clipboard.
//!
//! `NSPasteboard` is a process-wide singleton and is **not thread-safe**.
//! Creating separate `arboard::Clipboard` values does not help: they all wrap
//! the same `generalPasteboard`. clipd reads it from several threads at once —
//! the watcher polls continuously while the daemon reads and restores it around
//! every paste — and concurrent access corrupts AppKit's internal type cache.
//!
//! The observed failure was a SIGSEGV inside `objc_msgSend`, reached from
//! `-[NSPasteboard _updateTypeCacheIfNeeded]` on the watcher thread while the
//! daemon thread was inside `CFPasteboardCopyData`. The crash reported
//! "possible pointer authentication failure", which is what a garbage `isa`
//! looks like once the cache has been freed underneath the reader.
//!
//! Every clipboard touch therefore goes through these two functions, which
//! hold a process-wide lock for exactly one operation. They deliberately do
//! not call each other and never invoke user code while holding the lock, so
//! there is no path that can deadlock.

use std::path::PathBuf;
use std::sync::Mutex;

/// The UTI Finder uses to put copied files on the pasteboard.
///
/// A Finder copy carries one pasteboard *item* per file, each holding a
/// percent-encoded `file://` URL under this type — not the bytes.
pub const FILE_URL_TYPE: &str = "public.file-url";

/// Guards the shared `NSPasteboard`. One operation at a time, process-wide.
static PASTEBOARD: Mutex<()> = Mutex::new(());

/// Take the lock, ignoring poisoning.
///
/// A panic in another thread must not disable the clipboard for the rest of
/// the session — the data behind this lock is `()`, so there is no invariant a
/// poisoned guard could be protecting.
fn guard() -> std::sync::MutexGuard<'static, ()> {
    PASTEBOARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read the clipboard as text. `None` when it is empty, non-text, or
/// unreadable (which on macOS usually means permissions).
pub fn read_text() -> Option<String> {
    let _lock = guard();
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Replace the clipboard contents with `text`.
pub fn write_text(text: &str) -> Result<(), String> {
    let _lock = guard();
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text).map_err(|e| e.to_string())
}

/// The files currently on the clipboard, in the order Finder put them there.
///
/// Empty when the clipboard holds anything else, which is the overwhelmingly
/// common case — callers should treat a non-empty result as "this is a file
/// copy" and fall back to [`read_text`] otherwise.
pub fn read_file_urls() -> Vec<PathBuf> {
    let _lock = guard();
    platform::read_file_urls()
}

/// Put files on the clipboard so `Cmd+V` in Finder pastes the real files.
///
/// The first file's path is also written as plain text, so pasting into a
/// terminal or an editor gives you the path rather than nothing at all.
pub fn write_file_urls(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("No files to put on the clipboard.".into());
    }
    let _lock = guard();
    platform::write_file_urls(paths)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::FILE_URL_TYPE;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString};
    use objc2_foundation::{NSArray, NSString, NSURL};
    use std::path::{Path, PathBuf};

    pub fn read_file_urls() -> Vec<PathBuf> {
        let pb = NSPasteboard::generalPasteboard();
        let Some(items) = pb.pasteboardItems() else {
            return Vec::new();
        };
        let file_url = NSString::from_str(FILE_URL_TYPE);

        items
            .iter()
            .filter_map(|item| {
                let url_string = item.stringForType(&file_url)?;
                // Go through NSURL rather than trimming "file://" by hand: the
                // pasteboard value is percent-encoded, so a path containing a
                // space or a '#' would otherwise come back mangled.
                let url = NSURL::URLWithString(&url_string)?;
                let path = url.path()?;
                Some(PathBuf::from(path.to_string()))
            })
            .collect()
    }

    pub fn write_file_urls(paths: &[PathBuf]) -> Result<(), String> {
        let items: Vec<Retained<NSPasteboardItem>> = paths
            .iter()
            .enumerate()
            .filter_map(|(i, path)| build_item(path, i == 0))
            .collect();

        if items.is_empty() {
            return Err("None of those files could be put on the clipboard.".into());
        }

        let writers: Vec<&ProtocolObject<_>> =
            items.iter().map(|i| ProtocolObject::from_ref(&**i)).collect();

        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if pb.writeObjects(&NSArray::from_slice(&writers)) {
            Ok(())
        } else {
            Err("The pasteboard refused the files.".into())
        }
    }

    /// One pasteboard item for one file. `with_text` adds a plain-text flavour,
    /// used only for the first file so a text paste yields one path, not a
    /// concatenation of all of them.
    fn build_item(path: &Path, with_text: bool) -> Option<Retained<NSPasteboardItem>> {
        let path_str = path.to_str()?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
        let url_string = url.absoluteString()?;

        let item = NSPasteboardItem::new();
        let wrote = item.setString_forType(&url_string, &NSString::from_str(FILE_URL_TYPE));
        if !wrote {
            return None;
        }
        if with_text {
            // SAFETY: immutable framework string constant.
            let plain = unsafe { NSPasteboardTypeString };
            item.setString_forType(&NSString::from_str(path_str), plain);
        }
        Some(item)
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::path::PathBuf;

    /// File clips are a macOS pasteboard concept; nothing equivalent exists on
    /// the Windows or X11 clipboards that arboard exposes.
    pub fn read_file_urls() -> Vec<PathBuf> {
        Vec::new()
    }

    pub fn write_file_urls(_paths: &[PathBuf]) -> Result<(), String> {
        Err("Copying files to the clipboard is only supported on macOS.".into())
    }
}

/// Serialises every test in the crate that touches the real pasteboard.
///
/// There is one system pasteboard, so tests in different modules contend for it
/// just as much as tests in the same one. Two module-local mutexes do not help —
/// they only stop each module racing itself, which is how a file path from the
/// pasteboard tests ended up inside a secret-clipboard assertion. AppKit's type
/// cache is also not thread-safe, so unsynchronised access takes the whole test
/// binary down with a SIGTRAP rather than failing an assertion.
#[cfg(test)]
pub(crate) static PASTEBOARD_TESTS: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn test_serial() -> std::sync::MutexGuard<'static, ()> {
    PASTEBOARD_TESTS.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        test_serial()
    }

    /// The whole point of the module: many threads hammering the pasteboard
    /// must not fault. Before the lock this pattern is what crashed clipd.
    #[test]
    fn concurrent_access_is_serialized() {
        let _serial = serial();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                std::thread::spawn(move || {
                    for n in 0..10 {
                        // Interleave reads and writes the way the watcher and
                        // the daemon do in production.
                        if (i + n) % 2 == 0 {
                            let _ = read_text();
                        } else {
                            let _ = write_text(&format!("clipd test {i}-{n}"));
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("no thread may fault or panic");
        }
    }

    /// Round-trips real files through the real pasteboard. Opt in with
    /// `CLIPD_CLIPBOARD_TESTS=1` — it clobbers the user's clipboard.
    #[cfg(target_os = "macos")]
    #[test]
    fn file_urls_roundtrip_through_the_pasteboard() {
        if std::env::var("CLIPD_CLIPBOARD_TESTS").is_err() {
            return;
        }
        let _serial = serial();
        let tmp = tempfile::tempdir().expect("tempdir");
        // A space and a '#' in the name: both are percent-encoded on the
        // pasteboard, and both come back wrong if the URL is parsed by hand.
        let a = tmp.path().join("quarterly report #4.pdf");
        let b = tmp.path().join("plain.txt");
        std::fs::write(&a, b"pdf").expect("write a");
        std::fs::write(&b, b"txt").expect("write b");

        write_file_urls(&[a.clone(), b.clone()]).expect("write file urls");

        let back = read_file_urls();
        assert_eq!(back, vec![a.clone(), b], "both files, in order");

        // The first file's path is also readable as text.
        assert_eq!(read_text().as_deref(), a.to_str());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_text_clipboard_reports_no_files() {
        if std::env::var("CLIPD_CLIPBOARD_TESTS").is_err() {
            return;
        }
        let _serial = serial();
        write_text("just some text").expect("write");
        assert!(
            read_file_urls().is_empty(),
            "plain text must not look like a file copy"
        );
    }

    #[test]
    fn a_poisoned_lock_still_hands_out_access() {
        let _serial = serial();
        // Poison the mutex from a thread that panics while holding it.
        let _ = std::thread::spawn(|| {
            let _held = guard();
            panic!("deliberate");
        })
        .join();

        // The clipboard must keep working for the rest of the session.
        let _ = read_text();
    }
}
