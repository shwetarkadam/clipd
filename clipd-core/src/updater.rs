//! Updating Clipd from inside Clipd.
//!
//! The one-line installer gets the first copy on a Mac without a Gatekeeper
//! prompt, but nobody re-runs a Terminal command because a new version might
//! exist. So the tray checks GitHub once a day, offers "Update to v…" when
//! there is one, and does the install itself:
//!
//! - **macOS:** downloads the release zip, checks the new app is signed by the
//!   same certificate as the one running (so a tampered download is refused,
//!   and Accessibility / Input Monitoring grants carry over), swaps it into
//!   place where the current app lives, and relaunches. A file Clipd downloads
//!   itself carries no quarantine flag, so there is no "could not verify"
//!   prompt either.
//! - **Windows:** downloads the installer and runs it.
//! - **Linux:** points at the install command, which updates in place.
//!
//! History, slots and settings live outside the app, so an update keeps them.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "shwetarkadam/clipd";
pub const INSTALL_COMMAND: &str =
    "curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash";

/// How often the tray asks GitHub.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// A release newer than the running one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    /// The file this platform installs from, when the release has one.
    pub download_url: Option<String>,
    pub page_url: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CheckState {
    /// Unix seconds of the last successful check.
    checked_at: i64,
    latest: Option<Release>,
    /// The version the person was last told about, so the notice appears once
    /// per release rather than once per day.
    notified: Option<String>,
}

fn state_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("update_check.json")
}

fn load_state() -> CheckState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_state(state: &CheckState) {
    let path = state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, json);
    }
}

/// A build run from a cargo `target/` folder: a developer's, never updated
/// from GitHub over the top of what they are working on.
pub fn running_dev_build() -> bool {
    std::env::current_exe()
        .map(|exe| exe.components().any(|c| c.as_os_str() == "target"))
        .unwrap_or(false)
}

/// `latest` is a later version than `current` (both `1.2.3`, `v` optional).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    parse(latest) > parse(current)
}

/// The asset this machine installs from.
fn asset_name(version: &str) -> Option<String> {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    if cfg!(target_os = "macos") {
        Some(format!("Clipd-macos-{arch}-v{version}.zip"))
    } else if cfg!(target_os = "windows") {
        Some(format!("Clipd-Setup-v{version}.exe"))
    } else {
        None
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .build()
}

/// Ask GitHub for the latest release. `None` when offline or rate-limited.
pub fn fetch_latest() -> Option<Release> {
    let body: serde_json::Value = agent()
        .get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .set("User-Agent", "clipd-updater")
        .set("Accept", "application/vnd.github+json")
        .call()
        .ok()?
        .into_json()
        .ok()?;
    release_from_json(&body)
}

fn release_from_json(body: &serde_json::Value) -> Option<Release> {
    let tag = body["tag_name"].as_str()?;
    let version = tag.trim_start_matches('v').to_string();
    let wanted = asset_name(&version);
    let download_url = body["assets"].as_array().and_then(|assets| {
        assets.iter().find_map(|a| {
            (Some(a["name"].as_str()?) == wanted.as_deref())
                .then(|| a["browser_download_url"].as_str().map(str::to_string))
                .flatten()
        })
    });
    Some(Release {
        version,
        download_url,
        page_url: body["html_url"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/latest")),
    })
}

/// A newer release, checking GitHub only when the last check is a day old
/// (otherwise the remembered answer). Never blocks for long; call it off the
/// main thread.
pub fn available_update() -> Option<Release> {
    let mut state = load_state();
    let now = chrono::Utc::now().timestamp();
    if now - state.checked_at >= CHECK_EVERY.as_secs() as i64 {
        if let Some(latest) = fetch_latest() {
            state.checked_at = now;
            state.latest = Some(latest);
            save_state(&state);
        }
    }
    state.latest.filter(|r| is_newer(&r.version, CURRENT_VERSION))
}

/// True the first time it is asked about `version`: whether to show the
/// one-off "update available" notice.
pub fn take_first_notice(version: &str) -> bool {
    let mut state = load_state();
    if state.notified.as_deref() == Some(version) {
        return false;
    }
    state.notified = Some(version.to_string());
    save_state(&state);
    true
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let resp = agent()
        .get(url)
        .set("User-Agent", "clipd-updater")
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut file = std::fs::File::create(to).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp.into_reader(), &mut file).map_err(|e| format!("download failed: {e}"))?;
    Ok(())
}

/// The installed Clipd.app to update: the one this process runs from, else
/// the usual install locations (for the `clipd` CLI copied to the PATH).
#[cfg(target_os = "macos")]
pub fn installed_app() -> Option<PathBuf> {
    if let Some(bundle) = crate::install_location::current_bundle() {
        return Some(bundle);
    }
    let home = dirs::home_dir()?;
    [home.join("Applications/Clipd.app"), PathBuf::from("/Applications/Clipd.app")]
        .into_iter()
        .find(|p| p.exists())
}

/// The requirement the running app's signature states ("identifier … and
/// certificate leaf = H…"). The new build must satisfy it.
#[cfg(target_os = "macos")]
fn designated_requirement(app: &Path) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(app)
        .output()
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let req = text.lines().find_map(|l| l.strip_prefix("designated => "))?.trim().to_string();
    // An ad-hoc signature's requirement is its own hash: no other build can
    // ever meet it, so there is nothing to compare against.
    (!req.starts_with("cdhash")).then_some(req)
}

/// Download, verify and install `release` over `app`, then relaunch it.
/// Returns once the new copy is in place; the caller should exit so the
/// relaunch can take over.
#[cfg(target_os = "macos")]
pub fn install(release: &Release, app: &Path) -> Result<(), String> {
    use std::process::Command;

    let url = release
        .download_url
        .as_deref()
        .ok_or("this release has no Mac download")?;
    let requirement = designated_requirement(app).ok_or(
        "this copy of Clipd isn't signed in a way an update can be checked against — install with the command on the release page",
    )?;
    let work = std::env::temp_dir().join(format!("clipd-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let zip = work.join("update.zip");
        download(url, &zip)?;
        let unpacked = work.join("unpacked");
        let ok = Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(&zip)
            .arg(&unpacked)
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !ok {
            return Err("the download is not a valid zip".to_string());
        }
        let new_app = find_app(&unpacked).ok_or("the download has no Clipd.app in it")?;

        // Same signer as what is installed, and intact. A download that fails
        // this is not installed, whatever it claims to be.
        let verified = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(format!("-R={requirement}"))
            .arg(&new_app)
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !verified {
            return Err("the download isn't signed like the installed Clipd, so it wasn't installed".to_string());
        }

        // Swap: stage next to the app (same volume, so the renames are
        // atomic), move the old one aside, move the new one in.
        let parent = app.parent().ok_or("unexpected install location")?;
        let staged = parent.join(".Clipd-update.app");
        let old = parent.join(".Clipd-previous.app");
        let _ = std::fs::remove_dir_all(&staged);
        let _ = std::fs::remove_dir_all(&old);
        let copied = Command::new("/usr/bin/ditto")
            .arg(&new_app)
            .arg(&staged)
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !copied {
            return Err(format!("couldn't write to {}", parent.display()));
        }
        std::fs::rename(app, &old).map_err(|e| format!("couldn't replace the app: {e}"))?;
        if let Err(e) = std::fs::rename(&staged, app) {
            let _ = std::fs::rename(&old, app);
            return Err(format!("couldn't replace the app: {e}"));
        }
        let _ = std::fs::remove_dir_all(&old);
        let _ = Command::new("/usr/bin/xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(app)
            .status();
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result?;
    log::info!("Updated {} to v{}", app.display(), release.version);
    Ok(())
}

#[cfg(target_os = "macos")]
fn find_app(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "Clipd.app") {
            return Some(path);
        }
        // Release folders have dots in them (Clipd-macos-arm64-v0.4.24), so
        // "has an extension" can't mean "is a bundle": only skip other apps.
        if path.is_dir() && path.extension().map_or(true, |e| e != "app") {
            if let Some(found) = find_app(&path) {
                return Some(found);
            }
        }
    }
    None
}

/// Stop every Clipd process running from `app` and start it again. The
/// caller is one of those processes and should exit right after.
#[cfg(target_os = "macos")]
pub fn relaunch(app: &Path) {
    let _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 1; /usr/bin/pkill -f \"$0/Contents/MacOS/\"; sleep 1; /usr/bin/open \"$0\"")
        .arg(app)
        .spawn();
}

/// Download the installer and start it. It closes the running Clipd and
/// installs over it.
#[cfg(target_os = "windows")]
pub fn install(release: &Release) -> Result<(), String> {
    let url = release
        .download_url
        .as_deref()
        .ok_or("this release has no Windows installer")?;
    let path = std::env::temp_dir().join(format!("Clipd-Setup-v{}.exe", release.version));
    download(url, &path)?;
    std::process::Command::new(&path)
        .spawn()
        .map_err(|e| format!("couldn't start the installer: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_numbers_not_text() {
        assert!(is_newer("0.4.24", "0.4.23"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.4.23", "0.4.23"));
        assert!(!is_newer("0.4.9", "0.4.23"));
    }

    #[test]
    fn the_release_names_this_platforms_download() {
        let body = serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/shwetarkadam/clipd/releases/tag/v9.9.9",
            "assets": [
                {"name": "Clipd-linux-x86_64-v9.9.9.zip", "browser_download_url": "https://x/linux.zip"},
                {"name": "Clipd-macos-arm64-v9.9.9.zip", "browser_download_url": "https://x/mac-arm.zip"},
                {"name": "Clipd-macos-x86_64-v9.9.9.zip", "browser_download_url": "https://x/mac-intel.zip"},
                {"name": "Clipd-Setup-v9.9.9.exe", "browser_download_url": "https://x/setup.exe"}
            ]
        });
        let release = release_from_json(&body).unwrap();
        assert_eq!(release.version, "9.9.9");
        let expected = if cfg!(target_os = "macos") {
            Some(if cfg!(target_arch = "aarch64") { "https://x/mac-arm.zip" } else { "https://x/mac-intel.zip" })
        } else if cfg!(target_os = "windows") {
            Some("https://x/setup.exe")
        } else {
            None
        };
        assert_eq!(release.download_url.as_deref(), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_the_app_inside_a_versioned_folder() {
        // "v0.4.24" makes Path::extension() say "24": the folder must still be
        // searched.
        let root = std::env::temp_dir().join(format!("clipd-find-app-{}", std::process::id()));
        let app = root.join("Clipd-macos-arm64-v0.4.24/Clipd.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::create_dir_all(root.join("Other.app/Contents/Clipd.app")).unwrap();
        assert_eq!(find_app(&root), Some(app));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_release_without_our_file_has_no_download() {
        let body = serde_json::json!({"tag_name": "v9.9.9", "assets": []});
        let release = release_from_json(&body).unwrap();
        assert_eq!(release.download_url, None);
        assert!(release.page_url.ends_with("/releases/latest"));
    }
}
