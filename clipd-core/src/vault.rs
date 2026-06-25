//! Secure password handoff to an external, audited vault.
//!
//! clipd deliberately does **not** persist passwords (see [`crate::privacy`] —
//! sensitive clips are dropped before they ever reach SQLite). This module is
//! the escape hatch: when the user *wants* to keep a copied password, it routes
//! the secret straight into a real vault — 1Password, Bitwarden, or the macOS
//! Keychain — without clipd storing any plaintext at rest.
//!
//! Each backend shells out to the vendor's own CLI (`op`, `bw`, `security`), so
//! clipd never implements its own cryptography. Where the CLI allows it, the
//! secret is passed via stdin rather than argv to avoid leaking through `ps`.

use std::io::Write;
use std::process::{Command, Stdio};

/// A vault backend clipd can hand a secret to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultTarget {
    OnePassword,
    Bitwarden,
    Keychain,
}

impl VaultTarget {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OnePassword => "1Password",
            Self::Bitwarden => "Bitwarden",
            Self::Keychain => system_store_label(),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::OnePassword => "1password",
            Self::Bitwarden => "bitwarden",
            Self::Keychain => "keychain",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "1password" | "op" | "onepassword" => Some(Self::OnePassword),
            "bitwarden" | "bw" => Some(Self::Bitwarden),
            "keychain" | "macos" | "security" => Some(Self::Keychain),
            _ => None,
        }
    }

    pub const ALL: [VaultTarget; 3] = [Self::OnePassword, Self::Bitwarden, Self::Keychain];

    /// Whether this backend looks usable on this machine (CLI present, etc.).
    /// This does NOT verify the vault is unlocked — that surfaces at save time.
    pub fn is_available(&self) -> bool {
        match self {
            Self::OnePassword => cli_exists("op"),
            Self::Bitwarden => cli_exists("bw"),
            Self::Keychain => system_store_available(),
        }
    }
}

/// The set of backends usable on this machine right now.
pub fn available_targets() -> Vec<VaultTarget> {
    VaultTarget::ALL
        .iter()
        .copied()
        .filter(|t| t.is_available())
        .collect()
}

/// A login secret to store in a vault. `password` is the only required field.
#[derive(Debug, Clone, Default)]
pub struct SecretEntry {
    pub title: String,
    pub username: String,
    pub password: String,
    pub url: String,
    pub notes: String,
}

impl SecretEntry {
    pub fn new(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
            ..Default::default()
        }
    }

    fn effective_title(&self) -> String {
        if self.title.trim().is_empty() {
            "clipd saved password".to_string()
        } else {
            self.title.trim().to_string()
        }
    }
}

/// Save a secret to the chosen vault. Returns a human-readable success message
/// or an error explaining what went wrong (missing CLI, locked vault, etc.).
pub fn save_secret(target: VaultTarget, entry: &SecretEntry) -> Result<String, String> {
    if entry.password.trim().is_empty() {
        return Err("Refusing to save an empty password.".into());
    }
    match target {
        VaultTarget::OnePassword => save_1password(entry),
        VaultTarget::Bitwarden => save_bitwarden(entry),
        VaultTarget::Keychain => save_keychain(entry),
    }
}

// ── 1Password (`op` CLI) ──────────────────────────────────────────────────

fn save_1password(entry: &SecretEntry) -> Result<String, String> {
    if !cli_exists("op") {
        return Err("1Password CLI (`op`) not found. Install it and run `op signin`.".into());
    }
    let title = entry.effective_title();

    // `op item create` takes field assignments as args. The password assignment
    // is unavoidably visible in argv to other local processes — acceptable for a
    // single-user machine, but noted. Username/URL are non-secret.
    let mut args: Vec<String> = vec![
        "item".into(),
        "create".into(),
        "--category=login".into(),
        format!("--title={title}"),
    ];
    if !entry.url.trim().is_empty() {
        args.push(format!("--url={}", entry.url.trim()));
    }
    if !entry.username.trim().is_empty() {
        args.push(format!("username={}", entry.username.trim()));
    }
    args.push(format!("password={}", entry.password));
    if !entry.notes.trim().is_empty() {
        args.push(format!("notesPlain={}", entry.notes.trim()));
    }

    let out = Command::new("op")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to run `op`: {e}"))?;

    if out.status.success() {
        Ok(format!("Saved “{title}” to 1Password."))
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("not currently signed in") || err.contains("no account found") {
            Err("1Password is locked. Run `op signin` (or enable CLI integration in the app), then retry.".into())
        } else {
            Err(format!("1Password rejected the item: {}", err.trim()))
        }
    }
}

// ── Bitwarden (`bw` CLI) ───────────────────────────────────────────────────

fn save_bitwarden(entry: &SecretEntry) -> Result<String, String> {
    if !cli_exists("bw") {
        return Err("Bitwarden CLI (`bw`) not found. Install it and run `bw login` / `bw unlock`.".into());
    }
    // bw needs an unlocked session via the BW_SESSION env var.
    if std::env::var("BW_SESSION").map(|s| s.is_empty()).unwrap_or(true) {
        return Err(
            "Bitwarden is locked. Run `bw unlock` and export BW_SESSION, then retry.".into(),
        );
    }
    let title = entry.effective_title();

    // Build a Bitwarden login item, base64-encode it, and feed it to
    // `bw create item` over stdin so the password never appears in argv.
    let json = bitwarden_item_json(entry, &title);
    let encoded = base64_encode(json.as_bytes());

    let mut child = Command::new("bw")
        .args(["create", "item"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run `bw`: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(encoded.as_bytes())
            .map_err(|e| format!("Failed to write to `bw`: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("Failed to run `bw`: {e}"))?;

    if out.status.success() {
        Ok(format!("Saved “{title}” to Bitwarden."))
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("Bitwarden rejected the item: {}", err.trim()))
    }
}

/// Minimal Bitwarden login-item JSON (type 1 = login).
fn bitwarden_item_json(entry: &SecretEntry, title: &str) -> String {
    let uris = if entry.url.trim().is_empty() {
        "[]".to_string()
    } else {
        format!(r#"[{{"match":null,"uri":{}}}]"#, json_str(entry.url.trim()))
    };
    format!(
        r#"{{"organizationId":null,"folderId":null,"type":1,"name":{name},"notes":{notes},"favorite":false,"login":{{"username":{user},"password":{pass},"uris":{uris}}}}}"#,
        name = json_str(title),
        notes = json_str(entry.notes.trim()),
        user = json_str(entry.username.trim()),
        pass = json_str(&entry.password),
        uris = uris,
    )
}

// ── OS-native secret store ─────────────────────────────────────────────────
// macOS Keychain (`security`), Windows Credential Manager (`cmdkey`), or Linux
// Secret Service / GNOME Keyring / KWallet (`secret-tool`). Picked at compile
// time so each platform uses its own audited store.

fn system_store_label() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Windows Credential Manager"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux Secret Service"
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        "macOS Keychain"
    }
}

fn system_store_available() -> bool {
    #[cfg(target_os = "windows")]
    {
        cli_exists("cmdkey")
    }
    #[cfg(target_os = "linux")]
    {
        cli_exists("secret-tool")
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

fn save_keychain(entry: &SecretEntry) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        save_credential_windows(entry)
    }
    #[cfg(target_os = "linux")]
    {
        save_secret_service_linux(entry)
    }
    #[cfg(target_os = "macos")]
    {
        save_keychain_macos(entry)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = entry;
        Err("No system secret store is available on this platform.".into())
    }
}

// Uses the Security framework directly — no `security` CLI, so there is no
// terminal prompt and the password never touches argv or a tty. Works whether
// clipd was launched from Finder, a tray, or a terminal.
#[cfg(target_os = "macos")]
fn save_keychain_macos(entry: &SecretEntry) -> Result<String, String> {
    use security_framework::passwords::{
        delete_generic_password, set_generic_password,
    };

    let title = entry.effective_title();
    let account = if entry.username.trim().is_empty() {
        "clipd"
    } else {
        entry.username.trim()
    };
    // The (service, account) pair is the item key; scope service per title so
    // distinct saves don't overwrite each other.
    let service = format!("clipd: {title}");

    // Replace any existing item so a re-save updates rather than errors.
    let _ = delete_generic_password(&service, account);
    set_generic_password(&service, account, entry.password.as_bytes())
        .map(|_| format!("Saved “{title}” to the macOS Keychain (account: {account})."))
        .map_err(|e| format!("Keychain rejected the item: {e}"))
}

// Linux Secret Service via `secret-tool` (libsecret). The password is read from
// stdin, so it never appears in argv.
#[cfg(target_os = "linux")]
fn save_secret_service_linux(entry: &SecretEntry) -> Result<String, String> {
    if !cli_exists("secret-tool") {
        return Err(
            "`secret-tool` not found. Install libsecret-tools (e.g. `apt install libsecret-tools`)."
                .into(),
        );
    }
    let title = entry.effective_title();
    let account = if entry.username.trim().is_empty() {
        "clipd".to_string()
    } else {
        entry.username.trim().to_string()
    };

    let mut child = Command::new("secret-tool")
        .arg("store")
        .arg(format!("--label=clipd: {title}"))
        .args(["service", "clipd"])
        .args(["account", &account])
        .args(["title", &title])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run `secret-tool`: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        // secret-tool reads the secret from stdin until EOF — no trailing
        // newline so the stored value is exactly the password.
        let _ = stdin.write_all(entry.password.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("Failed to run `secret-tool`: {e}"))?;

    if out.status.success() {
        Ok(format!(
            "Saved “{title}” to the Secret Service (account: {account})."
        ))
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "Secret Service rejected the item (is a keyring unlocked?): {}",
            err.trim()
        ))
    }
}

// Windows Credential Manager via the built-in `cmdkey`. Note: cmdkey takes the
// password as an argument, so it is briefly visible in the process table — an
// accepted trade-off for a single-user machine using the OS's own tool.
#[cfg(target_os = "windows")]
fn save_credential_windows(entry: &SecretEntry) -> Result<String, String> {
    if !cli_exists("cmdkey") {
        return Err("`cmdkey` not found (it ships with Windows).".into());
    }
    let title = entry.effective_title();
    let account = if entry.username.trim().is_empty() {
        "clipd".to_string()
    } else {
        entry.username.trim().to_string()
    };

    let out = Command::new("cmdkey")
        .arg(format!("/generic:clipd:{title}"))
        .arg(format!("/user:{account}"))
        .arg(format!("/pass:{}", entry.password))
        .output()
        .map_err(|e| format!("Failed to run `cmdkey`: {e}"))?;

    if out.status.success() {
        Ok(format!(
            "Saved “{title}” to Windows Credential Manager (user: {account})."
        ))
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("Credential Manager rejected the item: {}", err.trim()))
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Is `name` an executable on PATH? Uses the platform's lookup tool.
fn cli_exists(name: &str) -> bool {
    let finder = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    Command::new(finder)
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Escape a string as a JSON string literal (including the surrounding quotes).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Standard base64 (no external crate) for the Bitwarden encoded payload.
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_roundtrip() {
        for t in VaultTarget::ALL {
            assert_eq!(VaultTarget::from_id(t.id()), Some(t));
        }
        assert_eq!(VaultTarget::from_id("op"), Some(VaultTarget::OnePassword));
        assert_eq!(VaultTarget::from_id("bw"), Some(VaultTarget::Bitwarden));
        assert_eq!(VaultTarget::from_id("nope"), None);
    }

    #[test]
    fn empty_password_rejected() {
        let e = SecretEntry::new("   ");
        assert!(save_secret(VaultTarget::Keychain, &e).is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn json_escaping() {
        assert_eq!(json_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_str("a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn bitwarden_json_is_wellformed() {
        let mut e = SecretEntry::new("p@ss\"word");
        e.title = "GitHub".into();
        e.username = "me".into();
        e.url = "https://github.com".into();
        let json = bitwarden_item_json(&e, "GitHub");
        assert!(json.contains("\"type\":1"));
        assert!(json.contains("\"name\":\"GitHub\""));
        assert!(json.contains("\"password\":\"p@ss\\\"word\""));
        assert!(json.contains("https://github.com"));
    }
}
