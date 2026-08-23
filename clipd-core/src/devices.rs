//! Which Macs clipd can send to, and how they find each other.
//!
//! The transport is a shared iCloud Drive folder. That choice is doing a lot of
//! work: both Macs are already signed into the same Apple account, so the
//! folder is authenticated, encrypted in transit, and reachable from any
//! network without a server, a pairing code, an open port, or NAT traversal.
//! Being signed into the same account *is* the trust boundary — there is no
//! separate pairing step to get wrong.
//!
//! Layout under [`sync_root`]:
//!
//! ```text
//! clipd/
//!   devices/<device-id>.json     one presence file per Mac, refreshed on start
//!   inbox/<device-id>/*.env      envelopes waiting for that Mac
//! ```

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// How long since its last heartbeat before a device stops being offered as a
/// send target. Long enough to survive a laptop being shut for a weekend.
pub const STALE_AFTER_DAYS: i64 = 30;

/// A Mac that clipd can send to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Stable, opaque identifier. Never displayed.
    pub id: String,
    /// The Mac's name as its owner sees it ("Shweta's MacBook Air").
    pub name: String,
    /// Last time this device announced itself.
    pub last_seen: DateTime<Utc>,
}

impl Device {
    /// Whether this device has gone quiet long enough to stop offering it.
    pub fn is_stale(&self) -> bool {
        Utc::now() - self.last_seen > Duration::days(STALE_AFTER_DAYS)
    }

    /// A short prefix of the id, for disambiguating two Macs with one name.
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(6)]
    }
}

/// The shared clipd folder in iCloud Drive, or `None` when iCloud Drive is off.
///
/// The `com~apple~CloudDocs` container only exists once the user has enabled
/// iCloud Drive, so its absence is the signal that sync cannot work here.
///
/// iCloud is only the *default*, not a requirement. Any directory both
/// machines can see works — a mounted SMB share, a USB stick, Dropbox,
/// Syncthing — because nothing below this function knows or cares how the
/// bytes get from one side to the other. Resolution order:
///
/// 1. `CLIPD_SYNC_ROOT` — for a one-off or a test run
/// 2. the path saved by [`save_sync_root`]
/// 3. iCloud Drive, when it is switched on
pub fn sync_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLIPD_SYNC_ROOT") {
        let path = PathBuf::from(dir);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    if let Some(saved) = load_sync_root() {
        return Some(saved);
    }
    icloud_root()
}

/// The iCloud Drive location, when iCloud Drive is enabled.
///
/// The `com~apple~CloudDocs` container only exists once the user has turned
/// iCloud Drive on, so its absence is what tells us not to default there.
pub fn icloud_root() -> Option<PathBuf> {
    let cloud = dirs::home_dir()?
        .join("Library")
        .join("Mobile Documents")
        .join("com~apple~CloudDocs");
    cloud.is_dir().then(|| cloud.join("clipd"))
}

/// Path of the file recording a user-chosen sync folder.
fn sync_root_config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("sync-root.txt")
}

/// The sync folder the user picked, if they picked one.
pub fn load_sync_root() -> Option<PathBuf> {
    let raw = std::fs::read_to_string(sync_root_config_path()).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(expand_tilde(trimmed)))
}

/// Choose the folder clipd syncs through. `None` clears it, falling back to
/// iCloud Drive.
///
/// The directory is created if missing and checked for writability now, rather
/// than failing later from a background thread where nobody would see it.
pub fn save_sync_root(dir: Option<&std::path::Path>) -> Result<(), String> {
    let path = sync_root_config_path();
    let Some(dir) = dir else {
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("Couldn't clear the sync folder: {e}"));
            }
        }
        return Ok(());
    };

    let dir = PathBuf::from(expand_tilde(&dir.to_string_lossy()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Couldn't use {}: {e}", dir.display()))?;

    // Prove it is writable now. A read-only mount or an unmounted share fails
    // here, with the user watching, instead of silently swallowing every send.
    let probe = dir.join(".clipd-write-test");
    std::fs::write(&probe, b"ok")
        .map_err(|e| format!("{} isn't writable: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);

    write_atomically(&path, dir.to_string_lossy().as_bytes())
}

/// Expand a leading `~` so a hand-typed path works.
fn expand_tilde(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest).to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        None => path.to_string(),
    }
}

/// Where this device's incoming envelopes land.
pub fn inbox_dir(root: &std::path::Path, device_id: &str) -> PathBuf {
    root.join("inbox").join(device_id)
}

/// Where presence files live.
pub fn devices_dir(root: &std::path::Path) -> PathBuf {
    root.join("devices")
}

/// Path of the file holding this Mac's generated device id.
fn device_id_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("device-id")
}

/// Process-wide cache. Two threads asking at once must get one answer, or a
/// clip could be addressed to an id this Mac then stops answering to.
static DEVICE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// This Mac's stable device id, generated once and kept forever.
///
/// Deliberately *not* the hardware UUID: that is a real device fingerprint, and
/// writing it into a folder that syncs to other machines is more identity than
/// this feature needs. A random local id is equally stable and says nothing.
pub fn device_id() -> String {
    DEVICE_ID.get_or_init(load_or_create_id).clone()
}

fn load_or_create_id() -> String {
    let path = device_id_path();
    if let Some(id) = read_id(&path) {
        return id;
    }

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // `create_new` so a concurrent process cannot have its id overwritten by
    // ours. If it lost the race, we adopt the id that actually landed rather
    // than believing our own — two ids for one Mac means clips sent to the
    // other one are never collected.
    let id = generate_id();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            use std::io::Write;
            if let Err(e) = f.write_all(id.as_bytes()) {
                log::warn!("Couldn't save this Mac's device id: {e}");
            }
            id
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_id(&path).unwrap_or(id)
        }
        Err(e) => {
            // Without a persisted id this Mac gets a new one every launch and
            // accumulates ghost peers, so this is worth complaining about.
            log::warn!("Couldn't save this Mac's device id: {e}");
            id
        }
    }
}

fn read_id(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn generate_id() -> String {
    let mut hasher = Sha256::new();
    hasher.update(hostname().as_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    // A second time source, so two Macs first launched in the same nanosecond
    // still diverge.
    hasher.update(format!("{:p}", &hasher as *const _).as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// This Mac's human-readable name — what the user calls it in Settings.
pub fn device_name() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    hostname()
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown-mac".to_string())
}

/// This Mac, as other Macs will see it.
pub fn this_device() -> Device {
    Device {
        id: device_id(),
        name: device_name(),
        last_seen: Utc::now(),
    }
}

/// Announce this Mac in the shared folder and make sure its inbox exists.
///
/// Called on daemon start. Creating our own inbox here (rather than making the
/// sender do it) means a sender never has to guess at directory layout for a
/// device that has not run this version yet.
pub fn register(root: &std::path::Path) -> Result<Device, String> {
    let me = this_device();
    let dir = devices_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    std::fs::create_dir_all(inbox_dir(root, &me.id))
        .map_err(|e| format!("Couldn't create this Mac's inbox: {e}"))?;

    let json = serde_json::to_string_pretty(&me).map_err(|e| e.to_string())?;
    write_atomically(&dir.join(format!("{}.json", me.id)), json.as_bytes())?;
    Ok(me)
}

/// Every device announced in the shared folder, including this one, newest
/// heartbeat first. Unreadable entries are skipped rather than failing the lot.
pub fn all_devices(root: &std::path::Path) -> Vec<Device> {
    let Ok(entries) = std::fs::read_dir(devices_dir(root)) else {
        return Vec::new();
    };
    let mut devices: Vec<Device> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let raw = std::fs::read_to_string(e.path()).ok()?;
            match serde_json::from_str::<Device>(&raw) {
                Ok(d) => Some(d),
                Err(err) => {
                    log::debug!("ignoring unreadable device file {:?}: {err}", e.path());
                    None
                }
            }
        })
        .collect();
    devices.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    devices
}

/// The other Macs this one can send to: everything but us, still fresh.
pub fn peers(root: &std::path::Path) -> Vec<Device> {
    let me = device_id();
    all_devices(root)
        .into_iter()
        .filter(|d| d.id != me && !d.is_stale())
        .collect()
}

/// Resolve what the user typed — a device name, a prefix of one, or an id —
/// to exactly one peer.
///
/// With a single peer, an empty query resolves to it: two Macs means there is
/// no choice to make, and making the user name the target anyway is the exact
/// friction this feature exists to remove.
pub fn resolve_peer(root: &std::path::Path, query: Option<&str>) -> Result<Device, String> {
    let peers = peers(root);
    if peers.is_empty() {
        return Err(
            "No other Macs found. Run clipd on your other Mac with the same Apple ID, \
             then try again."
                .into(),
        );
    }

    let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) else {
        return match peers.len() {
            1 => Ok(peers.into_iter().next().expect("checked len")),
            _ => {
                // More than one peer and nothing to go on: name them rather
                // than picking for the user.
                let names: Vec<&str> = peers.iter().map(|p| p.name.as_str()).collect();
                Err(format!(
                    "More than one Mac to send to — name one of: {}",
                    names.join(", ")
                ))
            }
        };
    };

    let q = query.to_lowercase();
    let exact: Vec<&Device> = peers
        .iter()
        .filter(|d| d.name.to_lowercase() == q || d.id == query)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }

    let partial: Vec<&Device> = peers
        .iter()
        .filter(|d| d.name.to_lowercase().contains(&q) || d.id.starts_with(query))
        .collect();
    match partial.len() {
        1 => Ok(partial[0].clone()),
        0 => Err(format!("No Mac matching \"{query}\".")),
        _ => {
            let names: Vec<String> = partial
                .iter()
                .map(|d| format!("{} ({})", d.name, d.short_id()))
                .collect();
            Err(format!(
                "\"{query}\" matches more than one Mac: {}",
                names.join(", ")
            ))
        }
    }
}

/// Write a file so a reader never observes it half-written.
///
/// Both halves of sync watch directories for new files; a plain write would let
/// the other side pick up a truncated envelope and reject it. Renaming within
/// the same directory is atomic, so a file is either absent or complete.
pub fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;

    let staging = parent.join(format!(
        ".{}.partial",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&staging, bytes).map_err(|e| format!("Couldn't write {}: {e}", staging.display()))?;
    std::fs::rename(&staging, path).map_err(|e| {
        let _ = std::fs::remove_file(&staging);
        format!("Couldn't finalise {}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str, days_ago: i64) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            last_seen: Utc::now() - Duration::days(days_ago),
        }
    }

    /// Seeds a fake sync root with the given devices, plus this Mac.
    fn seed(devices: &[Device]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = devices_dir(tmp.path());
        std::fs::create_dir_all(&dir).expect("mkdir");
        for d in devices {
            std::fs::write(
                dir.join(format!("{}.json", d.id)),
                serde_json::to_string(d).expect("json"),
            )
            .expect("write");
        }
        // This Mac's own presence file must never be offered as a target.
        let me = this_device();
        std::fs::write(
            dir.join(format!("{}.json", me.id)),
            serde_json::to_string(&me).expect("json"),
        )
        .expect("write me");
        tmp
    }

    #[test]
    fn a_single_peer_needs_no_naming() {
        let tmp = seed(&[device("aaa111", "MacBook Air", 0)]);
        let picked = resolve_peer(tmp.path(), None).expect("resolve");
        assert_eq!(picked.name, "MacBook Air");
        // Blank input is the same as none — it comes from an empty CLI arg.
        assert_eq!(resolve_peer(tmp.path(), Some("  ")).unwrap().id, "aaa111");
    }

    #[test]
    fn this_mac_is_never_a_send_target() {
        let tmp = seed(&[]);
        assert!(peers(tmp.path()).is_empty());
        let err = resolve_peer(tmp.path(), None).unwrap_err();
        assert!(err.contains("No other Macs"), "{err}");
    }

    #[test]
    fn several_peers_must_be_named() {
        let tmp = seed(&[
            device("aaa111", "MacBook Air", 0),
            device("bbb222", "Mac mini", 1),
        ]);
        let err = resolve_peer(tmp.path(), None).unwrap_err();
        assert!(err.contains("More than one Mac"), "{err}");

        assert_eq!(resolve_peer(tmp.path(), Some("air")).unwrap().id, "aaa111");
        assert_eq!(resolve_peer(tmp.path(), Some("mini")).unwrap().id, "bbb222");
        // By id and by id prefix.
        assert_eq!(resolve_peer(tmp.path(), Some("bbb222")).unwrap().id, "bbb222");
        assert_eq!(resolve_peer(tmp.path(), Some("bbb")).unwrap().id, "bbb222");
    }

    #[test]
    fn an_ambiguous_name_is_reported_not_guessed() {
        let tmp = seed(&[
            device("aaa111", "Work MacBook", 0),
            device("bbb222", "Home MacBook", 0),
        ]);
        let err = resolve_peer(tmp.path(), Some("macbook")).unwrap_err();
        assert!(err.contains("more than one Mac"), "{err}");
        assert!(err.contains("aaa111") && err.contains("bbb222"), "{err}");
    }

    #[test]
    fn an_exact_name_beats_a_substring_of_another() {
        let tmp = seed(&[
            device("aaa111", "Air", 0),
            device("bbb222", "Air (old)", 0),
        ]);
        assert_eq!(resolve_peer(tmp.path(), Some("Air")).unwrap().id, "aaa111");
    }

    #[test]
    fn long_silent_macs_drop_off_the_list() {
        let tmp = seed(&[
            device("aaa111", "MacBook Air", 0),
            device("ccc333", "Retired iMac", STALE_AFTER_DAYS + 1),
        ]);
        let names: Vec<String> = peers(tmp.path()).into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["MacBook Air"]);
    }

    #[test]
    fn unreadable_device_files_are_skipped_not_fatal() {
        let tmp = seed(&[device("aaa111", "MacBook Air", 0)]);
        std::fs::write(devices_dir(tmp.path()).join("junk.json"), b"{ not json")
            .expect("write junk");
        assert_eq!(peers(tmp.path()).len(), 1);
    }

    #[test]
    fn register_creates_the_inbox_and_announces_this_mac() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let me = register(tmp.path()).expect("register");
        assert!(inbox_dir(tmp.path(), &me.id).is_dir());
        let listed = all_devices(tmp.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, me.id);
    }

    #[test]
    fn atomic_writes_leave_no_partial_file_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("nested").join("thing.env");
        write_atomically(&target, b"payload").expect("write");
        assert_eq!(std::fs::read(&target).unwrap(), b"payload");

        let strays: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("partial"))
            .collect();
        assert!(strays.is_empty(), "staging file should be gone");
    }

    #[test]
    fn the_env_var_overrides_everything() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: single-threaded test; restored before it returns.
        unsafe { std::env::set_var("CLIPD_SYNC_ROOT", tmp.path()) };
        assert_eq!(sync_root().as_deref(), Some(tmp.path()));
        unsafe { std::env::remove_var("CLIPD_SYNC_ROOT") };
    }

    #[test]
    fn an_empty_env_var_is_ignored_rather_than_used_as_a_path() {
        unsafe { std::env::set_var("CLIPD_SYNC_ROOT", "") };
        // Must fall through, not return "".
        assert_ne!(sync_root().as_deref(), Some(std::path::Path::new("")));
        unsafe { std::env::remove_var("CLIPD_SYNC_ROOT") };
    }

    #[test]
    fn an_unwritable_folder_is_refused_when_it_is_chosen() {
        let missing = std::path::Path::new("/System/nope/clipd-sync");
        let err = save_sync_root(Some(missing)).unwrap_err();
        assert!(err.contains("Couldn't use") || err.contains("isn't writable"), "{err}");
    }

    #[test]
    fn a_tilde_path_is_expanded() {
        assert!(expand_tilde("~/Documents").starts_with('/'));
        assert!(!expand_tilde("~/Documents").contains('~'));
        // Absolute paths pass through untouched.
        assert_eq!(expand_tilde("/Volumes/share"), "/Volumes/share");
    }

    #[test]
    fn the_device_id_is_stable_across_calls() {
        assert_eq!(device_id(), device_id());
        assert_eq!(device_id().len(), 16);
    }
}
