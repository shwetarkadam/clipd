//! Lightweight anonymous telemetry — one HTTP GET on daemon startup.
//!
//! Privacy: no cookies, no fingerprinting, no personal data, and never
//! anything that passed through the clipboard.
//!
//! What does leave the machine: the app version, the OS and architecture, how
//! many days since this install first ran, the names of actions taken
//! (`clip_copied`, `blocked_permission`), and a random id that ties them
//! together. Country is derived by the receiving end from the request's IP —
//! which is to say any HTTP request reveals it, but it is worth saying out
//! loud, because the settings switch promises country data and this is where
//! it comes from. The IP itself is not stored by us.
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
        // Written once, never updated. Tenure has to be measured from the
        // first launch on *this* machine — inferring it from the first event
        // PostHog happens to hold would restart everyone's clock the day
        // telemetry is switched on.
        "first_seen": now_unix(),
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

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days since this install first ran, or 0 if unknown.
///
/// Sent with every event so "how long have people been using it" is a
/// property you can group by, rather than something only derivable from
/// whatever history the analytics backend happens to still hold.
fn days_since_install() -> u64 {
    let path = telemetry_json_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    let Some(first) = json.get("first_seen").and_then(|v| v.as_u64()) else {
        // Installed before first_seen existed: stamp it now so tenure starts
        // counting rather than staying unknown forever.
        let mut obj = json.clone();
        if let Some(map) = obj.as_object_mut() {
            map.insert("first_seen".into(), serde_json::json!(now_unix()));
            let _ = std::fs::write(&path, serde_json::to_string_pretty(&obj).unwrap_or_default());
        }
        return 0;
    };
    now_unix().saturating_sub(first) / 86_400
}

/// A random id for this install.
///
/// The previous version mixed the clock, the pid and a stack address into a
/// u128 and sliced it up. Two faults, both visible in any id it produced:
/// nanos never reaches bit 96, so every id began `00000000-0000-`, and the
/// last group was printed `{:012x}` from a u64, which is sixteen hex digits
/// wide — so the ids were malformed *and* carried far less entropy than their
/// length suggested.
///
/// That did not matter while the id was ignored by the receiving end. It
/// matters now: distinct installs are counted by this string, and ids that
/// collide undercount people.
///
/// /dev/urandom where there is one, the old mixing only as a fallback.
fn uuid_simple() -> String {
    let mut b = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut b)
        })
        .is_ok();

    if !filled {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id() as u128;
        let addr = std::ptr::addr_of!(b) as u128;
        let mix = nanos ^ (pid << 64) ^ (addr << 32) ^ (addr.rotate_left(17));
        b.copy_from_slice(&mix.to_le_bytes());
    }

    // Version 4, variant 1 — so it is a real UUID and not merely shaped like
    // one, which matters if it is ever pasted into a tool that validates.
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = |r: &[u8]| r.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&b[0..4]),
        h(&b[4..6]),
        h(&b[6..8]),
        h(&b[8..10]),
        h(&b[10..16])
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

/// Record something the user did, by name.
///
/// Names only, never content: `clip_copied`, not what was copied. This is a
/// clipboard manager, so the rule is absolute — nothing that passes through
/// the clipboard is ever a property here, and no free text from the user is
/// either.
///
/// What it buys: a funnel. "Opened the palette" minus "copied something" is
/// the count of people who came looking and left empty-handed, and the
/// permission events say whether the reason was a grant macOS never gave.
///
/// A no-op unless a PostHog key was compiled in and telemetry is on.
pub fn event(name: &'static str, props: &[(&'static str, String)]) {
    let Some(key) = posthog_key() else {
        return;
    };
    if !is_telemetry_enabled() {
        return;
    }
    let mut map = serde_json::Map::new();
    map.insert("version".into(), serde_json::json!(clipd_version()));
    map.insert("os".into(), serde_json::json!(os_name()));
    map.insert("arch".into(), serde_json::json!(arch_name()));
    map.insert("days_since_install".into(), serde_json::json!(days_since_install()));
    for (k, v) in props {
        map.insert((*k).into(), serde_json::json!(v));
    }
    let body = serde_json::json!({
        "api_key": key,
        "event": name,
        "distinct_id": get_or_create_install_id(),
        "properties": serde_json::Value::Object(map),
    });
    send_json_async(body);
}

/// POST a prepared body without blocking the caller.
fn send_json_async(body: serde_json::Value) {
    let url = format!("{}/i/v0/e/", posthog_host().trim_end_matches('/'));
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(4))
            .timeout_read(Duration::from_secs(4))
            .build();
        match agent.post(&url).send_json(body) {
            Ok(_) => log::debug!("📊 telemetry ok"),
            Err(e) => log::debug!("📊 telemetry skipped: {}", e),
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
    let _ = url;
    let body = serde_json::json!({
        "api_key": key,
        "event": "daemon_started",
        "distinct_id": install_id,
        "properties": {
            "version": version,
            "os": os,
            "arch": arch,
            "days_since_install": days_since_install(),
        },
    });
    send_json_async(body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_install_id_is_a_well_formed_unique_uuid() {
        // The old generator produced "00000000-0000-4xxx-xxxx-<16 hex>": a
        // constant first half, and a last group four digits too long. Both
        // faults shrink the space that distinct-install counting relies on.
        let a = uuid_simple();
        let parts: Vec<&str> = a.split('-').collect();
        assert_eq!(parts.len(), 5, "not a uuid: {a}");
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "group widths wrong: {a}"
        );
        assert!(a.chars().all(|c| c == '-' || c.is_ascii_hexdigit()), "{a}");
        assert!(parts[0] != "00000000", "first group is constant again: {a}");
        assert!(parts[2].starts_with('4'), "not version 4: {a}");
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "not variant 1: {a}"
        );

        // Two ids in a row must differ, or every install on a machine shares
        // one and the user count collapses to the number of machines.
        let b = uuid_simple();
        assert_ne!(a, b, "two ids came out identical");
    }
}
