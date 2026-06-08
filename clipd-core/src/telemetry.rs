//! Lightweight anonymous telemetry — one HTTP GET on daemon startup.
//!
//! Privacy: no cookies, no fingerprinting, no personal data.
//! Users can opt out by setting `"enabled": false` in `~/.local/share/clipd/telemetry.json`
//! (or simply deleting that file).
//!
//! The telemetry endpoint is set at **compile time** via the `CLIPD_TELEMETRY_ENDPOINT`
//! environment variable:
//!   CLIPD_TELEMETRY_ENDPOINT=https://your-worker.workers.dev cargo build --release -p clipd-daemon
//!
//! If the env var is absent, the telemetry feature is a no-op (zero binary cost).

use std::path::PathBuf;
use std::time::Duration;

// ── platform helpers ──────────────────────────────────────────────────────────

fn telemetry_json_path() -> PathBuf {
    let dir = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("clipd").join("telemetry.json")
}

fn clipd_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn os_name() -> &'static str {
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(target_os = "windows")]
    return "windows";
    #[cfg(target_os = "linux")]
    return "linux";
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    return "other";
}

fn arch_name() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(any(target_arch = "aarch64", target_arch = "arm")) {
        "aarch64"
    } else {
        "unknown"
    }
}

// ── install ID ────────────────────────────────────────────────────────────────

/// Reads the install_id from telemetry.json, or creates a new one.
fn get_or_create_install_id() -> String {
    let path = telemetry_json_path();

    // Try to read existing
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(id) = json.get("install_id").and_then(|v| v.as_str()) {
                return id.to_string();
            }
        }
    }

    // Create new
    let id = uuid_simple();
    let json = serde_json::json!({
        "install_id": &id,
        "enabled": true,
    });
    if let Some(parent) = telemetry_json_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap_or_default());
    id
}

/// Simple random UUID-v4-like string using only std library.
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.as_nanos();
    let pid = std::process::id();
    // Mix pid and a stack address for entropy
    let entropy = nanos ^ ((pid as u128) << 64) ^ (std::ptr::addr_of!(now) as u128);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (entropy >> 96) as u32,
        (entropy >> 80) as u16,
        ((entropy >> 64) as u16) & 0x0fff,
        (((entropy >> 48) as u16) & 0x3fff) | 0x8000,
        entropy as u64
    )
}

// ── telemetry config ──────────────────────────────────────────────────────────

/// Whether the user has enabled telemetry (defaults to true on first run).
fn is_telemetry_enabled() -> bool {
    let path = telemetry_json_path();
    if !path.exists() {
        return true; // first run — create config and default to enabled
    }
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            // Absent or null → default true
            return json.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        }
    }
    true
}

/// Returns the configured endpoint, or None if not set at compile time.
fn endpoint() -> Option<&'static str> {
    option_env!("CLIPD_TELEMETRY_ENDPOINT").filter(|s| !s.is_empty())
}

// ── url encoding (no external dep) ───────────────────────────────────────────

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

// ── ping ─────────────────────────────────────────────────────────────────────

/// Fires one anonymous telemetry GET.
///
/// Runs in a spawned background thread — never blocks daemon startup.
/// On network failure or if telemetry is disabled, silently does nothing.
pub fn ping() {
    // If no endpoint is configured at compile time, this is a no-op.
    let endpoint = match endpoint() {
        Some(e) => e,
        None => return,
    };

    if !is_telemetry_enabled() {
        return;
    }

    let install_id = get_or_create_install_id();
    let version = clipd_version().to_string();
    let os = os_name().to_string();
    let arch = arch_name().to_string();

    let url = format!(
        "{}/ping?v={}&os={}&arch={}&id={}",
        endpoint.trim_end_matches('/'),
        urlencoding_encode(&version),
        urlencoding_encode(&os),
        urlencoding_encode(&arch),
        urlencoding_encode(&install_id),
    );

    std::thread::spawn(move || {
        // Use ureq with a 4-second total timeout on the connection.
        // ureq 2.x sets timeout on the Agent, not the Request.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(4))
            .timeout_read(Duration::from_secs(4))
            .build();

        match agent.get(&url).call() {
            Ok(resp) => {
                log::debug!(
                    "📊 telemetry ping ok — {} users on {}",
                    resp.status_text(),
                    version
                );
            }
            Err(e) => {
                // Silently ignore — not critical functionality
                log::debug!("📊 telemetry skipped: {}", e);
            }
        }
    });
}
