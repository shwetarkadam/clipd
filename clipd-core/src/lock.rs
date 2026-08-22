use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

fn lock_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("daemon.lock")
}

fn named_lock_path(name: &str) -> PathBuf {
    let safe_name: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join(format!("{safe_name}.lock"))
}

/// An atomic, crash-tolerant single-process lease.
///
/// GUI surfaces use separate names (`gui-main`, `gui-hud`) so
/// they may coexist while repeated clicks cannot create duplicates. A PID left
/// behind by Force Quit is recognized as stale on the next launch.
pub struct ProcessLock {
    path: PathBuf,
    pid: u32,
}

impl ProcessLock {
    pub fn try_acquire(name: &str) -> Option<Self> {
        Self::try_acquire_path(named_lock_path(name))
    }

    fn try_acquire_path(path: PathBuf) -> Option<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok()?;
        }

        // One retry is normally enough for a stale PID. A few iterations also
        // cover two launchers racing to clean up the same crashed process.
        for _ in 0..4 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let pid = std::process::id();
                    if write!(file, "{pid}").is_err() || file.sync_all().is_err() {
                        let _ = fs::remove_file(&path);
                        return None;
                    }
                    return Some(Self { path, pid });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    let existing = fs::read_to_string(&path).ok();
                    let existing_pid = existing
                        .as_deref()
                        .and_then(|contents| contents.trim().parse::<u32>().ok());
                    if existing_pid.is_some_and(is_process_alive) {
                        return None;
                    }

                    // Only remove the exact stale value we inspected. This
                    // avoids deleting a new owner's lock if launchers race.
                    if fs::read_to_string(&path).ok() == existing {
                        let _ = fs::remove_file(&path);
                    }
                }
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let owns_lock = fs::read_to_string(&self.path)
            .ok()
            .and_then(|contents| contents.trim().parse::<u32>().ok())
            == Some(self.pid);
        if owns_lock {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Try to acquire the daemon lock. Returns true if this process now owns it.
/// Stale locks (PID no longer running) are automatically cleaned up.
pub fn try_acquire_daemon_lock() -> bool {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }

    if path.exists() {
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                if is_process_alive(pid) {
                    return false;
                }
            }
        }
        fs::remove_file(&path).ok();
    }

    let pid = std::process::id();
    fs::write(&path, pid.to_string()).is_ok()
}

/// PID of the process holding the daemon lock, when it is genuinely running.
///
/// clipd-ui hosts the daemon in-process, so this is how another process finds
/// the tray host in order to shut the whole app down.
pub fn daemon_lock_pid() -> Option<u32> {
    let pid = fs::read_to_string(lock_path())
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    is_process_alive(pid).then_some(pid)
}

/// Release the daemon lock (call on shutdown).
pub fn release_daemon_lock() {
    let path = lock_path();
    if let Ok(contents) = fs::read_to_string(&path) {
        if let Ok(pid) = contents.trim().parse::<u32>() {
            if pid == std::process::id() {
                fs::remove_file(&path).ok();
            }
        }
    }
}

/// Check if another daemon instance is already running.
/// Whether a specific clipd surface is currently running, by its lock.
///
/// `ProcessLock::try_acquire` succeeds only when nobody holds the lock, so a
/// failure to take it means that surface is alive. The lock is released when
/// the process dies, including a crash, so this cannot go stale.
pub fn surface_is_running(name: &str) -> bool {
    ProcessLock::try_acquire(name).is_none()
}

pub fn is_daemon_running() -> bool {
    let path = lock_path();
    if !path.exists() {
        return false;
    }
    if let Ok(contents) = fs::read_to_string(&path) {
        if let Ok(pid) = contents.trim().parse::<u32>() {
            return is_process_alive(pid);
        }
    }
    false
}

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // `kill(pid, 0)` succeeds for a *zombie* — the PID entry outlives the
    // process until its parent reaps it. When clipd is killed and its launching
    // shell is already gone, the corpse can sit there indefinitely, and a lock
    // held by that PID then looks permanently live: the GUI hands off to a
    // process that will never answer, so no window can ever open again.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return false;
    }
    !is_zombie(pid)
}

/// True when `pid` exists but has already exited and is awaiting reaping.
///
/// Asks `ps` for the process state rather than a libc struct: `kinfo_proc` is
/// not exposed by the `libc` crate, and `proc_pidinfo` fails outright on a
/// corpse — reading that failure as "still alive" is the very bug this guards
/// against. The cost is irrelevant: this runs once at process startup, not on
/// any hot path.
#[cfg(unix)]
fn is_zombie(pid: u32) -> bool {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "state="])
        .output()
    else {
        // Cannot prove it is a corpse, so leave a possibly-genuine owner be.
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .starts_with('Z')
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

#[cfg(not(any(unix, windows)))]
fn is_process_alive(_pid: u32) -> bool {
    false
}

/// Whether the daemon's global hotkey grab is working.
///
/// Written by the daemon on startup so the GUI can show a persistent banner
/// when multi-slot copy / HUD toasts are dead because macOS denied the
/// Accessibility / Input Monitoring event tap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyStatus {
    Ok,
    NeedsAccessibility,
}

fn hotkey_status_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("hotkey_status.txt")
}

pub fn save_hotkey_status(status: HotkeyStatus) {
    let path = hotkey_status_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let text = match status {
        HotkeyStatus::Ok => "ok",
        HotkeyStatus::NeedsAccessibility => "needs_accessibility",
    };
    let _ = fs::write(path, text);
}

pub fn load_hotkey_status() -> HotkeyStatus {
    match fs::read_to_string(hotkey_status_path())
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("needs_accessibility") => HotkeyStatus::NeedsAccessibility,
        _ => HotkeyStatus::Ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zombie must read as dead. `kill(pid, 0)` succeeds for one, so without
    /// the extra check a crashed clipd leaves a lock that never expires and no
    /// GUI window can open again for the life of the login session.
    #[cfg(unix)]
    #[test]
    fn a_zombie_process_is_not_alive() {
        use std::process::Command;

        // Spawn and let it exit, but deliberately do NOT wait() on it, so the
        // kernel keeps the PID around as an unreaped corpse.
        let mut child = Command::new("true").spawn().expect("spawn");
        let pid = child.id();

        // Give it a moment to exit and enter Z state.
        for _ in 0..50 {
            if is_zombie(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(is_zombie(pid), "child should be an unreaped zombie");
        assert!(
            !is_process_alive(pid),
            "a zombie holds a PID but cannot own a lock"
        );

        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn the_running_process_is_alive_and_not_a_zombie() {
        let me = std::process::id();
        assert!(is_process_alive(me));
        assert!(!is_zombie(me));
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_held_by_a_zombie_can_be_taken_over() {
        use std::process::Command;

        let dir = std::env::temp_dir().join(format!("clipd-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zombie.lock");

        let mut child = Command::new("true").spawn().expect("spawn");
        let pid = child.id();
        for _ in 0..50 {
            if is_zombie(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::fs::write(&path, pid.to_string()).unwrap();

        let guard = ProcessLock::try_acquire_path(path.clone());
        assert!(
            guard.is_some(),
            "a lock whose owner is a corpse must be reclaimable"
        );

        drop(guard);
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_process_lock_allows_only_one_owner_and_releases_on_drop() {
        let path = std::env::temp_dir().join(format!(
            "clipd-test-gui-lock-{}-{}.lock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let first =
            ProcessLock::try_acquire_path(path.clone()).expect("first owner should acquire");
        assert!(ProcessLock::try_acquire_path(path.clone()).is_none());
        drop(first);
        assert!(ProcessLock::try_acquire_path(path).is_some());
    }
}
