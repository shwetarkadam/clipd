use std::fs;
use std::path::PathBuf;

fn lock_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("daemon.lock")
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
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            out.contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn is_process_alive(_pid: u32) -> bool {
    false
}
