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

/// Windows: a lock holder is alive if we can open a handle to its PID.
/// (The old stub returned `false` unconditionally, which made every lock look
/// stale — multiple daemons would run at once, each with its own in-memory
/// slots, breaking multi-slot copy/paste in confounding ways.)
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
