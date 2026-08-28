//! Lightweight anonymous telemetry — one HTTP GET on daemon startup.
//!
//! Privacy: no cookies, no fingerprinting, no personal data.
//! Users can opt out by setting `"enabled": false` in `~/.local/share/clipd/telemetry.json`
//! (or simply deleting that file).
//!
//! Two ways to receive the ping, both set at **compile time**, both optional:
//!
//!   CLIPD_POSTHOG_KEY=phc_...            → PostHog. Nothing to deploy or run;
//!                                          it counts distinct install ids, so
//!                                          active users and retention come out
//!                                          of the box. Free to 1M events/month.
//!   CLIPD_TELEMETRY_ENDPOINT=https://... → your own counter (the worker in
//!                                          telemetry-worker/), if you would
//!                                          rather the data landed on
//!                                          infrastructure you own.
//!
//! PostHog wins if both are set. If neither is, this is a no-op with zero
//! binary cost — which is every local build.

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
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    );
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
            return json
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
        }
    }
    true
}

/// Whether anonymous telemetry is on, for the settings UI to show.
pub fn telemetry_enabled() -> bool {
    is_telemetry_enabled()
}

/// Turn anonymous telemetry on or off, preserving the install id.
///
/// Opting out used to mean finding `telemetry.json` and editing it by hand,
/// which is not an opt-out any user is going to discover — and this is a
/// clipboard manager, where "what does it send home" is a fair question to
/// have a switch for.
pub fn set_telemetry_enabled(on: bool) {
    let path = telemetry_json_path();
    let install_id = get_or_create_install_id();
    let json = serde_json::json!({ "install_id": install_id, "enabled": on });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap_or_default());
}

/// Whether this build can send anything at all — false when no endpoint was
/// compiled in, which is every local build. The settings row says so rather
/// than offering a switch that does nothing.
pub fn telemetry_configured() -> bool {
    endpoint().is_some() || posthog_key().is_some()
}

/// Returns the configured endpoint, or None if not set at compile time.
fn endpoint() -> Option<&'static str> {
    option_env!("CLIPD_TELEMETRY_ENDPOINT").filter(|s| !s.is_empty())
}

/// PostHog project key, if this build was made with one.
///
/// The alternative to running a counter yourself. Set this instead of
/// `CLIPD_TELEMETRY_ENDPOINT` and there is nothing to deploy or maintain:
/// PostHog counts distinct `distinct_id`s for you, which is the "how many
/// people actually use this" number a hand-rolled counter kept failing to
/// answer.
fn posthog_key() -> Option<&'static str> {
    option_env!("CLIPD_POSTHOG_KEY").filter(|s| !s.is_empty())
}

/// Which PostHog region to talk to. Defaults to US cloud; set
/// `CLIPD_POSTHOG_HOST=https://eu.i.posthog.com` for EU.
fn posthog_host() -> &'static str {
    option_env!("CLIPD_POSTHOG_HOST")
        .filter(|s| !s.is_empty())
        .unwrap_or("https://us.i.posthog.com")
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
    // Nothing configured at compile time → nothing to send, no binary cost.
    let posthog = posthog_key();
    let endpoint = endpoint();
    if posthog.is_none() && endpoint.is_none() {
        return;
    }

    if !is_telemetry_enabled() {
        return;
    }

    let install_id = get_or_create_install_id();
    let version = clipd_version().to_string();
    let os = os_name().to_string();
    let arch = arch_name().to_string();

    // PostHog wins when both are set: it is the one that can answer "how many
    // people", because it counts distinct ids rather than requests.
    if let Some(key) = posthog {
        posthog_ping(key, &install_id, &version, &os, &arch);
        return;
    }
    let endpoint = match endpoint {
        Some(e) => e,
        None => return,
    };

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

/// One event to PostHog's capture endpoint, off the startup path.
///
/// A plain POST — no SDK, no extra dependency, `ureq` was already here. The
/// install id goes in as `distinct_id`, which is the whole point: PostHog's
/// active-user and retention views are counts of distinct ids, so they come
/// out right without any aggregation code on our side.
fn posthog_ping(key: &str, install_id: &str, version: &str, os: &str, arch: &str) {
    let url = format!("{}/i/v0/e/", posthog_host().trim_end_matches('/'));
    let body = serde_json::json!({
        "api_key": key,
        "event": "daemon_started",
        "distinct_id": install_id,
        "properties": {
            "version": version,
            "os": os,
            "arch": arch,
        },
    });

    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(4))
            .timeout_read(Duration::from_secs(4))
            .build();
        match agent.post(&url).send_json(body) {
            Ok(_) => log::debug!("📊 telemetry ping ok"),
            Err(e) => log::debug!("📊 telemetry skipped: {}", e),
        }
    });
}
