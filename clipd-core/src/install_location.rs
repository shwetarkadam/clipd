#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
//! Get Clipd into Applications on its first launch from a download.
//!
//! Double-click Clipd.app straight out of the unzipped download and macOS runs
//! it from a randomised, read-only copy ("App Translocation"), because the
//! browser marked the download as quarantined. From there nothing that makes
//! Clipd work holds: Accessibility and Input Monitoring are granted to a path
//! that is different next launch, the helpers beside the app are not where it
//! looks, and launch-at-login points at a folder that no longer exists. To the
//! person it looks like "I granted the permission and it still doesn't work".
//!
//! So on a launch from anywhere but an Applications folder, Clipd offers to
//! move itself there — the way most Mac apps distributed outside the App Store
//! do — clears the quarantine flag on the installed copy, puts the downloaded
//! one in the Trash, and relaunches from Applications. One click, once.

use std::path::{Path, PathBuf};

/// The `.app` this process runs from, if it runs from one.
pub fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

fn is_translocated(bundle: &Path) -> bool {
    bundle.to_string_lossy().contains("/AppTranslocation/")
}

/// Inside /Applications or ~/Applications (or a folder within either).
fn in_applications(bundle: &Path, home: &Path) -> bool {
    bundle.starts_with("/Applications") || bundle.starts_with(home.join("Applications"))
}

/// A developer's own build (`target/release/Clipd.app`) is run where it was
/// built, on purpose.
fn is_dev_build(bundle: &Path) -> bool {
    bundle.components().any(|c| c.as_os_str() == "target")
}

/// Whether this launch should offer the move.
fn should_offer(bundle: &Path, home: &Path) -> bool {
    !in_applications(bundle, home) && (is_translocated(bundle) || !is_dev_build(bundle))
}

fn declined_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("move_to_applications_declined")
}

/// Where the installed copy goes: over an existing install wherever it is,
/// else /Applications when it can be written, else ~/Applications (which
/// needs no admin password).
fn destination_dir(home: &Path) -> PathBuf {
    let user = home.join("Applications");
    if user.join("Clipd.app").exists() {
        return user;
    }
    let system = Path::new("/Applications");
    if system.join("Clipd.app").exists() {
        return system.to_path_buf();
    }
    let probe = system.join(".clipd-write-test");
    if std::fs::write(&probe, b"").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return system.to_path_buf();
    }
    home.join("Applications")
}

/// Offer the move when it applies; on a yes, install, relaunch from
/// Applications and exit this process. Returns when there is nothing to do or
/// the person said not now. Call first thing, before the daemon starts.
#[cfg(target_os = "macos")]
pub fn offer_move_to_applications() {
    if std::env::var_os("CLIPD_NO_MOVE").is_some() {
        return;
    }
    let (Some(bundle), Some(home)) = (current_bundle(), dirs::home_dir()) else {
        return;
    };
    if !should_offer(&bundle, &home) {
        return;
    }
    let original = if is_translocated(&bundle) {
        original_path(&bundle)
    } else {
        Some(bundle.clone())
    };
    // "Not now" holds for that copy; a newer download is asked again.
    let key = original.as_deref().unwrap_or(&bundle).to_string_lossy().into_owned();
    if std::fs::read_to_string(declined_path()).is_ok_and(|d| d == key) {
        return;
    }
    log::info!("Running from {} — offering to move to Applications", bundle.display());
    if !ask(&bundle) {
        log::info!("Move to Applications declined");
        if let Some(dir) = declined_path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(declined_path(), key);
        return;
    }
    match install(&bundle, &home) {
        Ok(dest) => {
            if let Some(original) = original.filter(|o| *o != dest) {
                trash(&original, &home);
            }
            log::info!("Installed to {}; relaunching from there", dest.display());
            // Give this process a moment to be gone, so the new one does not
            // meet it holding the daemon lock.
            let _ = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("sleep 1; /usr/bin/open -n \"$0\"")
                .arg(&dest)
                .spawn();
            std::process::exit(0);
        }
        Err(e) => {
            log::warn!("Move to Applications failed: {e}");
            alert(&format!(
                "Clipd couldn't move itself to Applications ({e}).\n\nDrag Clipd into your Applications folder in Finder, then open it from there."
            ));
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn offer_move_to_applications() {}

#[cfg(target_os = "macos")]
fn install(bundle: &Path, home: &Path) -> Result<PathBuf, String> {
    let mut dir = destination_dir(home);
    let mut dest = dir.join("Clipd.app");
    // An installed Clipd that is running would keep running the old build
    // beside the new one.
    let _ = std::process::Command::new("/usr/bin/pkill")
        .arg("-f")
        .arg(format!("{}/Contents/MacOS/", dest.display()))
        .status();
    // An older Clipd already installed is replaced. If it cannot be removed
    // (owned by another user), install into ~/Applications instead.
    if dest.exists() && std::fs::remove_dir_all(&dest).is_err() {
        dir = home.join("Applications");
        dest = dir.join("Clipd.app");
        if dest.exists() {
            std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // ditto keeps the signature, symlinks and extended attributes intact.
    let status = std::process::Command::new("/usr/bin/ditto")
        .arg(bundle)
        .arg(&dest)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("copy failed".into());
    }
    // The installed copy was chosen on purpose: it should open without the
    // download warning from now on.
    let _ = std::process::Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&dest)
        .status();
    Ok(dest)
}

/// The download the translocated copy came from, so it can go in the Trash.
#[cfg(target_os = "macos")]
fn original_path(bundle: &Path) -> Option<PathBuf> {
    use core_foundation::base::TCFType;
    use core_foundation::url::{CFURL, CFURLRef};
    use std::ffi::c_void;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecTranslocateCreateOriginalPathForURL(url: CFURLRef, error: *mut *mut c_void) -> CFURLRef;
    }
    let url = CFURL::from_path(bundle, true)?;
    unsafe {
        let original = SecTranslocateCreateOriginalPathForURL(url.as_concrete_TypeRef(), std::ptr::null_mut());
        if original.is_null() {
            return None;
        }
        CFURL::wrap_under_create_rule(original).to_path()
    }
}

#[cfg(target_os = "macos")]
fn trash(path: &Path, home: &Path) {
    let trash = home.join(".Trash");
    let name = path.file_name().map(|n| n.to_owned()).unwrap_or_else(|| "Clipd.app".into());
    let mut target = trash.join(&name);
    let mut n = 2;
    while target.exists() {
        target = trash.join(format!("Clipd {n}.app"));
        n += 1;
    }
    if let Err(e) = std::fs::rename(path, &target) {
        log::info!("Left the downloaded copy at {} ({e})", path.display());
    }
}

/// AppleScript-quoted string literal.
#[cfg(target_os = "macos")]
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
fn ask(bundle: &Path) -> bool {
    let icon = bundle.join("Contents/Resources/AppIcon.icns");
    let icon = if icon.exists() {
        format!(" with icon (POSIX file {})", quoted(&icon.to_string_lossy()))
    } else {
        String::new()
    };
    let script = format!(
        "display dialog {} with title \"Clipd\" buttons {{\"Not now\", \"Move to Applications\"}} \
         default button \"Move to Applications\"{icon}",
        quoted(
            "Move Clipd to your Applications folder?\n\n\
             Clipd is running from your download. macOS only keeps its keyboard \
             permissions, and starts it at login, once it lives in Applications."
        ),
    );
    std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("Move to Applications"))
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn alert(message: &str) {
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(format!(
            "display alert \"Clipd\" message {} as warning",
            quoted(message)
        ))
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/Users/ada")
    }

    #[test]
    fn a_download_is_offered_the_move() {
        assert!(should_offer(Path::new("/Users/ada/Downloads/Clipd-macos-arm64-v0.4.24/Clipd.app"), &home()));
        assert!(should_offer(
            Path::new("/private/var/folders/x/T/AppTranslocation/ABC-123/d/Clipd.app"),
            &home()
        ));
    }

    #[test]
    fn an_installed_copy_is_left_alone() {
        assert!(!should_offer(Path::new("/Applications/Clipd.app"), &home()));
        assert!(!should_offer(Path::new("/Users/ada/Applications/Clipd.app"), &home()));
        assert!(!should_offer(Path::new("/Applications/Utilities/Clipd.app"), &home()));
    }

    #[test]
    fn a_developer_build_runs_where_it_was_built() {
        assert!(!should_offer(Path::new("/Users/ada/src/clipd/target/release/Clipd.app"), &home()));
    }
}
