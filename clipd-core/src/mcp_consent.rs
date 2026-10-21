//! What AI apps may do with your clipboard, and asking you first.
//!
//! clipd's MCP server lets Claude, Cursor and other assistants search your
//! clipboard history. Reading stays frictionless — that is the point — but two
//! rules hold:
//!
//! - **Secrets are never handed over.** A clip the privacy detectors flag (an
//!   API key, a likely password, a one-time code) is replaced by a placeholder
//!   in every result, whatever tool asked.
//! - **Changes need a yes.** Putting text on your clipboard, saving a clip or a
//!   snippet asks you first: *Allow once*, *Allow for 1 hour* (that app, for
//!   that hour), or *Deny*. No answer within a minute is a no, and so is a
//!   machine where clipd cannot ask.
//!
//! Every request is written to an audit log — which app, which tool, what was
//! decided — and never what was in the clip.

use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// What the person said to a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowForHour,
    Deny,
}

/// Shown in place of a clip an AI app must not see.
pub const WITHHELD: &str = "[withheld by clipd: this clip looks like a password, key or one-time code]";

/// Whether `content` must be kept from AI apps. Checks with detection on even
/// when "Privacy protection" is off: turning off what clipd *stores* is not
/// the same as agreeing to hand secrets to an AI.
pub fn withhold_from_ai(
    content: &str,
    source_app: Option<&str>,
    config: &crate::privacy::PrivacyConfig,
) -> bool {
    let config = crate::privacy::PrivacyConfig {
        enabled: true,
        ..config.clone()
    };
    !crate::privacy::detect_sensitive(content, &config).is_empty()
        || crate::privacy::looks_like_password(content)
        || crate::privacy::is_one_time_code(content, source_app)
}

fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
}

fn grants_path() -> PathBuf {
    data_dir().join("mcp_grants.json")
}

fn load_grants() -> BTreeMap<String, DateTime<Utc>> {
    std::fs::read_to_string(grants_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn has_grant(client: &str, now: DateTime<Utc>) -> bool {
    load_grants().get(client).is_some_and(|until| *until > now)
}

fn grant_for_an_hour(client: &str, now: DateTime<Utc>) {
    let mut grants = load_grants();
    grants.retain(|_, until| *until > now);
    grants.insert(client.to_string(), now + Duration::hours(1));
    if let Ok(json) = serde_json::to_string_pretty(&grants) {
        let _ = std::fs::create_dir_all(data_dir());
        let _ = std::fs::write(grants_path(), json);
    }
}

/// Ask whether `client` may `action` (e.g. "put text on your clipboard"),
/// showing `preview` of what it wants to write. True when allowed.
pub fn approve_write(client: &str, tool: &str, action: &str, preview: &str) -> bool {
    let config = crate::privacy::load_privacy_config();
    let now = Utc::now();
    let decision = if !config.mcp_writes_need_approval {
        audit(client, tool, "allowed (approval turned off)");
        return true;
    } else if has_grant(client, now) {
        audit(client, tool, "allowed (within an hour you allowed)");
        return true;
    } else {
        ask(client, action, preview)
    };
    match decision {
        Decision::AllowOnce => audit(client, tool, "allowed once"),
        Decision::AllowForHour => {
            grant_for_an_hour(client, now);
            audit(client, tool, "allowed for 1 hour");
        }
        Decision::Deny => audit(client, tool, "denied"),
    }
    decision != Decision::Deny
}

/// Note a read in the audit log.
pub fn audit_read(client: &str, tool: &str, withheld: usize) {
    if withheld > 0 {
        audit(client, tool, &format!("read ({withheld} clip(s) withheld as secrets)"));
    } else {
        audit(client, tool, "read");
    }
}

fn audit(client: &str, tool: &str, outcome: &str) {
    use std::io::Write;
    let dir = if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs"))
    } else {
        Some(data_dir().join("logs"))
    };
    let Some(dir) = dir else { return };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("clipd-mcp.log"))
    {
        let _ = writeln!(
            f,
            "{} {} {} — {}",
            Utc::now().to_rfc3339(),
            one_line(client, 60),
            tool,
            outcome
        );
    }
}

/// A single line of at most `max` characters, safe inside a tab-separated
/// prompt payload.
fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// The question, as the person sees it.
fn question(client: &str, action: &str) -> String {
    format!("{} wants to {}", one_line(client, 40), action)
}

#[cfg(target_os = "macos")]
fn ask(client: &str, action: &str, preview: &str) -> Decision {
    // The overlay that draws clipd's toasts also draws prompts, answering on
    // stdout with the id of the button pressed.
    let hud = std::env::var_os("CLIPD_HUD_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .parent()
                .map(|dir| dir.join("clipd-hud"))
                .filter(|p| p.is_file())
        });
    let Some(hud) = hud else {
        log::warn!("MCP approval: no clipd-hud next to clipd-mcp, so the request is denied");
        return Decision::Deny;
    };
    let payload = format!(
        "STYLE\tprompt\nBADGE\t🤖\nTITLE\t{}\nHINT\tclipd asks before an AI app changes your clipboard or history\n\
         PREVIEW\ttext\t{}\nBTN\tdeny\tDeny\t0\nBTN\thour\tAllow for 1 hour\t0\nBTN\tonce\tAllow once\t1\nTIMEOUT\t60",
        question(client, action),
        one_line(preview, 140),
    );
    match std::process::Command::new(&hud)
        .arg(payload)
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(out) => decision_from_choice(String::from_utf8_lossy(&out.stdout).trim()),
        Err(e) => {
            log::warn!("MCP approval prompt failed to open ({e}); denying");
            Decision::Deny
        }
    }
}

#[cfg(target_os = "windows")]
fn ask(client: &str, action: &str, preview: &str) -> Decision {
    // A native Yes/No box: Yes allows this once.
    let text = format!(
        "{}.\n\n\"{}\"\n\nAllow it this once?",
        question(client, action),
        one_line(preview, 140)
    )
    .replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName PresentationFramework; \
         [System.Windows.MessageBox]::Show('{text}', 'clipd', 'YesNo', 'Question')"
    );
    match std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == "Yes" => Decision::AllowOnce,
        _ => Decision::Deny,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn ask(client: &str, action: &str, preview: &str) -> Decision {
    // zenity where it exists; without a way to ask, the answer is no.
    let text = format!(
        "{}.\n\n“{}”\n\nAllow it this once?",
        question(client, action),
        one_line(preview, 140)
    );
    match std::process::Command::new("zenity")
        .args(["--question", "--title=clipd", "--timeout=60"])
        .arg(format!("--text={text}"))
        .status()
    {
        Ok(status) if status.success() => Decision::AllowOnce,
        _ => Decision::Deny,
    }
}

fn decision_from_choice(choice: &str) -> Decision {
    match choice {
        "once" => Decision::AllowOnce,
        "hour" => Decision::AllowForHour,
        // "deny", a timeout, a dismissed prompt: all no.
        _ => Decision::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_allow_allows() {
        assert_eq!(decision_from_choice("once"), Decision::AllowOnce);
        assert_eq!(decision_from_choice("hour"), Decision::AllowForHour);
        for no in ["deny", "timeout", "dismiss", ""] {
            assert_eq!(decision_from_choice(no), Decision::Deny, "{no:?}");
        }
    }

    #[test]
    fn prompt_text_is_one_safe_line() {
        let line = one_line("line one\nline\ttwo\u{0}", 100);
        assert!(!line.contains(['\n', '\t', '\u{0}']));
        assert_eq!(line, "line one line two");
        assert_eq!(one_line(&"x".repeat(10), 4), "xxxx…");
        assert_eq!(
            question("Claude\nCode", "put text on your clipboard"),
            "Claude Code wants to put text on your clipboard"
        );
    }

    #[test]
    fn secrets_and_codes_are_withheld_from_ai() {
        let mut config = crate::privacy::PrivacyConfig::default();
        for off in [false, true] {
            // Even with protection switched off, nothing secret goes to an AI.
            config.enabled = !off;
            assert!(withhold_from_ai("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCD", None, &config));
            assert!(withhold_from_ai("Xq7!vR2#pLm9@Tz4", None, &config));
            assert!(withhold_from_ai("483920", Some("Messages"), &config));
            assert!(!withhold_from_ai("https://staging.acme.dev/api", Some("Slack"), &config));
            assert!(!withhold_from_ai("Invoice #4182 — net 30", Some("Mail"), &config));
        }
    }
}
