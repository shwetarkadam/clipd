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
//!
//! The system store (macOS Keychain) additionally supports **reading back**:
//! [`list_secrets`] enumerates what clipd has saved and [`reveal_secret`]
//! fetches one plaintext on demand. Listing never touches plaintext, so the
//! vault browser can be rendered without unlocking anything; `reveal_secret` is
//! the single choke point where a password leaves the Keychain.

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

// ── Reading back what clipd saved ──────────────────────────────────────────

/// A secret clipd has stored in the system store, as returned by
/// [`list_secrets`].
///
/// Deliberately carries no plaintext: a list of these can be held in UI state,
/// logged, or rendered without ever decrypting anything. [`reveal_secret`] is
/// the only way to get the password itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    /// Keychain service attribute; with `account` this is the item's primary key.
    pub service: String,
    /// Keychain account attribute — a unique, sortable id for clipd-written items.
    pub account: String,
    /// Human-readable name, shown both in clipd and in Keychain Access.
    pub title: String,
    /// Provenance note: where and when the password was captured.
    pub note: String,
    /// Unix seconds the item was created, when the store reports it.
    pub saved_at: Option<i64>,
}

impl SecretRef {
    /// Stable identity for UI selection and dedup.
    pub fn key(&self) -> String {
        format!("{}\u{0}{}", self.service, self.account)
    }
}

/// Every secret clipd has written to the system store, newest first.
///
/// Includes items written by older clipd versions under the previous naming
/// scheme so nothing already saved becomes unreachable.
pub fn list_secrets() -> Result<Vec<SecretRef>, String> {
    #[cfg(target_os = "macos")]
    {
        keychain::list()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(format!(
            "Browsing saved secrets isn't supported for the {} yet — open it directly to view them.",
            system_store_label()
        ))
    }
}

/// Fetch one secret's plaintext. This is the only call that decrypts.
pub fn reveal_secret(secret: &SecretRef) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        keychain::reveal(secret)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = secret;
        Err(format!(
            "Reading secrets back isn't supported for the {} yet.",
            system_store_label()
        ))
    }
}

/// Permanently remove a secret from the system store.
pub fn forget_secret(secret: &SecretRef) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        keychain::forget(secret)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = secret;
        Err(format!(
            "Deleting secrets isn't supported for the {} yet.",
            system_store_label()
        ))
    }
}

/// Give a saved secret a meaningful name. Renaming rewrites the label only —
/// the password is never read or re-written.
pub fn rename_secret(secret: &SecretRef, new_title: &str) -> Result<(), String> {
    let new_title = new_title.trim();
    if new_title.is_empty() {
        return Err("A name can't be empty.".into());
    }
    #[cfg(target_os = "macos")]
    {
        keychain::rename(secret, new_title)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = secret;
        Err(format!(
            "Renaming secrets isn't supported for the {} yet.",
            system_store_label()
        ))
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

#[cfg(target_os = "macos")]
fn save_keychain_macos(entry: &SecretEntry) -> Result<String, String> {
    let saved = keychain::save(entry)?;
    Ok(format!("Saved “{}” to the macOS Keychain.", saved.title))
}

// Uses the Security framework directly — no `security` CLI, so there is no
// terminal prompt and the password never touches argv or a tty. Works whether
// clipd was launched from Finder, a tray, or a terminal.
//
// Schema: every clipd secret is a generic password under the constant service
// `clipd-vault`, keyed by a unique, sortable `account` id. The display name
// lives in `kSecAttrLabel` — that is the column Keychain Access actually shows,
// so items stay findable outside clipd. Using one fixed service (rather than
// baking the title into it, as older builds did) means listing is a single
// exact-match query and renaming doesn't have to move the item.
#[cfg(target_os = "macos")]
pub mod keychain {
    use super::{SecretEntry, SecretRef};
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::date::CFDate;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::CFArrayRef;
    use core_foundation_sys::base::CFTypeRef;
    use core_foundation_sys::string::CFStringRef;
    use security_framework_sys::item::{
        kSecAttrAccount, kSecAttrComment, kSecAttrDescription, kSecAttrLabel, kSecAttrService,
        kSecAttrSynchronizable, kSecClass, kSecClassGenericPassword, kSecMatchLimit,
        kSecMatchLimitAll, kSecReturnAttributes, kSecReturnData, kSecValueData,
    };
    use security_framework_sys::keychain_item::{
        SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
    };

    /// The service every clipd secret is filed under.
    pub const SERVICE: &str = "clipd-vault";
    /// Services used by clipd builds that predate the vault browser. Items
    /// written back then must stay listable, or upgrading would strand them.
    const LEGACY_PREFIX: &str = "clipd: ";
    /// Shown in the "Kind" column of Keychain Access.
    const KIND: &str = "clipd saved password";

    // Attributes the Security framework exports but `security-framework-sys`
    // does not re-export. Declared here against the same framework binary.
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        static kSecAttrAccessible: CFStringRef;
        static kSecAttrAccessibleWhenUnlockedThisDeviceOnly: CFStringRef;
        static kSecAttrCreationDate: CFStringRef;
    }

    const ERR_SUCCESS: i32 = 0;
    const ERR_ITEM_NOT_FOUND: i32 = -25300;
    const ERR_DUPLICATE_ITEM: i32 = -25299;
    /// CFAbsoluteTime epoch (2001-01-01) expressed in Unix seconds.
    const CF_EPOCH_OFFSET: f64 = 978_307_200.0;

    fn cfkey(k: CFStringRef) -> CFString {
        unsafe { CFString::wrap_under_get_rule(k) }
    }

    fn describe(status: i32, context: &str) -> String {
        let detail = security_framework::base::Error::from_code(status).to_string();
        format!("{context} (Keychain error {status}: {detail})")
    }

    /// Base query identifying a single generic-password item.
    /// Store a password under a custom service/account (for clipd's own keys).
    /// Unlike `save`, this is a simple upsert — no provenance or title tracking.
    pub fn store(service: &str, account: &str, password: &str, label: &str) -> Result<(), String> {
        // Delete existing entry first (upsert).
        let _ = delete_by_service_account(service, account);
        let query = item_query(service, account);
        add_item_with_label(&query, password.as_bytes(), label)
    }

    /// Load a password by service/account. Returns None if not found.
    pub fn load(service: &str, account: &str) -> Result<Option<String>, String> {
        let mut pairs = item_query(service, account);
        pairs.push((cfkey(unsafe { kSecReturnData }), CFBoolean::true_value().into_CFType()));
        let query = CFDictionary::from_CFType_pairs(&pairs);
        let mut out: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut out) };
        if status == ERR_ITEM_NOT_FOUND {
            return Ok(None);
        }
        if status != ERR_SUCCESS {
            return Err(describe(status, "Couldn't read keychain item"));
        }
        if out.is_null() {
            return Ok(None);
        }
        let data = unsafe { CFData::wrap_under_create_rule(out as _) };
        String::from_utf8(data.bytes().to_vec())
            .map(Some)
            .map_err(|_| "Keychain item isn't valid UTF-8.".to_string())
    }

    /// Delete a keychain item by service/account.
    pub fn delete_by_service_account(service: &str, account: &str) -> Result<(), String> {
        let query = CFDictionary::from_CFType_pairs(&item_query(service, account));
        let status = unsafe { SecItemDelete(query.as_concrete_TypeRef()) };
        match status {
            ERR_SUCCESS | ERR_ITEM_NOT_FOUND => Ok(()),
            other => Err(describe(other, "Couldn't delete keychain item")),
        }
    }

    fn add_item_with_label(
        query: &[(CFString, CFType)],
        password: &[u8],
        label: &str,
    ) -> Result<(), String> {
        let mut pairs = query.to_vec();
        pairs.push((cfkey(unsafe { kSecAttrLabel }), CFString::from(label).into_CFType()));
        // Reuse add_item which handles duplicate → update.
        match add_item(&pairs, password) {
            Ok(()) => Ok(()),
            Err(code) => Err(describe(code, "Couldn't store keychain item")),
        }
    }

    fn item_query(service: &str, account: &str) -> Vec<(CFString, CFType)> {
        vec![
            (cfkey(unsafe { kSecClass }), unsafe {
                CFString::wrap_under_get_rule(kSecClassGenericPassword)
            }
            .into_CFType()),
            (
                cfkey(unsafe { kSecAttrService }),
                CFString::from(service).into_CFType(),
            ),
            (
                cfkey(unsafe { kSecAttrAccount }),
                CFString::from(account).into_CFType(),
            ),
        ]
    }

    /// A unique, sortable, human-legible item id (`20260730T153302-0007`).
    ///
    /// The counter disambiguates saves that land in the same second, which the
    /// timestamp alone cannot; without it a rapid second save would overwrite
    /// the first.
    fn new_account_id() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed) % 10_000;
        format!(
            "{}-{:04}",
            chrono::Local::now().format("%Y%m%dT%H%M%S"),
            n
        )
    }

    pub fn save(entry: &SecretEntry) -> Result<SecretRef, String> {
        let title = entry.effective_title();
        let account = new_account_id();
        let note = build_note(entry);

        let mut query = item_query(SERVICE, &account);
        query.push((
            cfkey(unsafe { kSecAttrLabel }),
            CFString::from(title.as_str()).into_CFType(),
        ));
        query.push((
            cfkey(unsafe { kSecAttrDescription }),
            CFString::from(KIND).into_CFType(),
        ));
        query.push((
            cfkey(unsafe { kSecAttrComment }),
            CFString::from(note.as_str()).into_CFType(),
        ));
        // Never let a clipd-captured password ride iCloud Keychain to other
        // devices. Non-synchronizable is already the default; state it anyway so
        // the intent survives future edits.
        query.push((
            cfkey(unsafe { kSecAttrSynchronizable }),
            CFBoolean::false_value().into_CFType(),
        ));

        // Everything above is accepted by both keychain implementations. The
        // accessibility class is only honoured by the data-protection keychain,
        // and the legacy file-based keychain rejects the whole add when it sees
        // it — so it goes on a separate attempt we can fall back from.
        let mut hardened = query.clone();
        hardened.push((
            cfkey(unsafe { kSecAttrAccessible }),
            unsafe { CFString::wrap_under_get_rule(kSecAttrAccessibleWhenUnlockedThisDeviceOnly) }
                .into_CFType(),
        ));

        match add_item(&hardened, entry.password.as_bytes()) {
            Ok(()) => {}
            Err(status) => {
                log::debug!(
                    "Keychain rejected device-only accessibility ({status}); \
                     retrying without it"
                );
                add_item(&query, entry.password.as_bytes())
                    .map_err(|s| describe(s, "Keychain refused to store the password"))?;
            }
        }

        Ok(SecretRef {
            service: SERVICE.to_string(),
            account,
            title,
            note,
            saved_at: Some(chrono::Local::now().timestamp()),
        })
    }

    fn add_item(query: &[(CFString, CFType)], password: &[u8]) -> Result<(), i32> {
        let mut pairs = query.to_vec();
        pairs.push((
            cfkey(unsafe { kSecValueData }),
            CFData::from_buffer(password).into_CFType(),
        ));
        let params = CFDictionary::from_CFType_pairs(&pairs);
        let mut out: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemAdd(params.as_concrete_TypeRef(), &mut out) };
        match status {
            ERR_SUCCESS => Ok(()),
            // Account ids are unique per save, so this only happens if the same
            // id is reused after a process restart within the same second.
            ERR_DUPLICATE_ITEM => {
                let find = CFDictionary::from_CFType_pairs(query);
                let update = CFDictionary::from_CFType_pairs(&[(
                    cfkey(unsafe { kSecValueData }),
                    CFData::from_buffer(password).into_CFType(),
                )]);
                let status = unsafe {
                    SecItemUpdate(find.as_concrete_TypeRef(), update.as_concrete_TypeRef())
                };
                if status == ERR_SUCCESS {
                    Ok(())
                } else {
                    Err(status)
                }
            }
            other => Err(other),
        }
    }

    /// Provenance line stored alongside the item, so a password found months
    /// later in Keychain Access still explains where it came from.
    fn build_note(entry: &SecretEntry) -> String {
        let mut note = entry.notes.trim().to_string();
        if !entry.username.trim().is_empty() {
            let user = format!("Username: {}", entry.username.trim());
            note = if note.is_empty() {
                user
            } else {
                format!("{user}\n{note}")
            };
        }
        if !entry.url.trim().is_empty() {
            note = format!("{note}\n{}", entry.url.trim());
        }
        note.trim().to_string()
    }

    pub fn list() -> Result<Vec<SecretRef>, String> {
        // Attributes only — no kSecReturnData — so this never decrypts and
        // never triggers a per-item access prompt.
        let query = CFDictionary::from_CFType_pairs(&[
            (cfkey(unsafe { kSecClass }), unsafe {
                CFString::wrap_under_get_rule(kSecClassGenericPassword)
            }
            .into_CFType()),
            (cfkey(unsafe { kSecMatchLimit }), unsafe {
                CFString::wrap_under_get_rule(kSecMatchLimitAll)
            }
            .into_CFType()),
            (
                cfkey(unsafe { kSecReturnAttributes }),
                CFBoolean::true_value().into_CFType(),
            ),
        ]);

        let mut out: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut out) };
        if status == ERR_ITEM_NOT_FOUND {
            return Ok(Vec::new());
        }
        if status != ERR_SUCCESS {
            return Err(describe(status, "Couldn't read the Keychain"));
        }
        if out.is_null() {
            return Ok(Vec::new());
        }

        let items: CFArray<CFDictionary<CFString, CFType>> =
            unsafe { CFArray::wrap_under_create_rule(out as CFArrayRef) };

        let mut secrets: Vec<SecretRef> = Vec::new();
        for item in items.iter() {
            let service = attr_string(&item, unsafe { kSecAttrService }).unwrap_or_default();
            if !is_clipd_item(&service) {
                continue;
            }
            let account = attr_string(&item, unsafe { kSecAttrAccount }).unwrap_or_default();
            let label = attr_string(&item, unsafe { kSecAttrLabel });
            secrets.push(SecretRef {
                title: display_title(label, &service, &account),
                note: attr_string(&item, unsafe { kSecAttrComment }).unwrap_or_default(),
                saved_at: attr_unix_time(&item, unsafe { kSecAttrCreationDate }),
                service,
                account,
            });
        }

        // Newest first; undated legacy items sort to the bottom but stay stable.
        secrets.sort_by(|a, b| {
            b.saved_at
                .cmp(&a.saved_at)
                .then_with(|| b.account.cmp(&a.account))
        });
        Ok(secrets)
    }

    fn is_clipd_item(service: &str) -> bool {
        service == SERVICE || service.starts_with(LEGACY_PREFIX)
    }

    /// Legacy items had no useful label — their name was baked into the service
    /// as `clipd: <title>`. Recover it so old saves don't list as blank rows.
    fn display_title(label: Option<String>, service: &str, account: &str) -> String {
        if let Some(label) = label {
            let label = label.trim();
            if !label.is_empty() && label != service {
                return label.to_string();
            }
        }
        if let Some(rest) = service.strip_prefix(LEGACY_PREFIX) {
            if !rest.trim().is_empty() {
                return rest.trim().to_string();
            }
        }
        if account.is_empty() {
            "Untitled password".to_string()
        } else {
            account.to_string()
        }
    }

    fn attr_string(item: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<String> {
        let value = item.find(cfkey(key))?;
        value.downcast::<CFString>().map(|s| s.to_string())
    }

    fn attr_unix_time(item: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<i64> {
        let value = item.find(cfkey(key))?;
        let date = value.downcast::<CFDate>()?;
        Some((date.abs_time() + CF_EPOCH_OFFSET) as i64)
    }

    pub fn reveal(secret: &SecretRef) -> Result<String, String> {
        let mut pairs = item_query(&secret.service, &secret.account);
        pairs.push((
            cfkey(unsafe { kSecReturnData }),
            CFBoolean::true_value().into_CFType(),
        ));
        let query = CFDictionary::from_CFType_pairs(&pairs);

        let mut out: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut out) };
        if status == ERR_ITEM_NOT_FOUND {
            return Err("That password is no longer in the Keychain.".into());
        }
        if status != ERR_SUCCESS {
            return Err(describe(status, "Couldn't read that password"));
        }
        if out.is_null() {
            return Err("The Keychain returned an empty result.".into());
        }
        let data = unsafe { CFData::wrap_under_create_rule(out as _) };
        String::from_utf8(data.bytes().to_vec())
            .map_err(|_| "That Keychain item isn't valid UTF-8 text.".to_string())
    }

    pub fn forget(secret: &SecretRef) -> Result<(), String> {
        let query = CFDictionary::from_CFType_pairs(&item_query(&secret.service, &secret.account));
        let status = unsafe { SecItemDelete(query.as_concrete_TypeRef()) };
        match status {
            ERR_SUCCESS | ERR_ITEM_NOT_FOUND => Ok(()),
            other => Err(describe(other, "Couldn't delete that password")),
        }
    }

    pub fn rename(secret: &SecretRef, new_title: &str) -> Result<(), String> {
        let query = CFDictionary::from_CFType_pairs(&item_query(&secret.service, &secret.account));
        let update = CFDictionary::from_CFType_pairs(&[(
            cfkey(unsafe { kSecAttrLabel }),
            CFString::from(new_title).into_CFType(),
        )]);
        let status =
            unsafe { SecItemUpdate(query.as_concrete_TypeRef(), update.as_concrete_TypeRef()) };
        match status {
            ERR_SUCCESS => Ok(()),
            ERR_ITEM_NOT_FOUND => Err("That password is no longer in the Keychain.".into()),
            other => Err(describe(other, "Couldn't rename that password")),
        }
    }
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

    // Exercises the real login Keychain, so it is opt-in: run with
    // `CLIPD_KEYCHAIN_TESTS=1 cargo test -p clipd-core`. Cleans up after itself
    // even when an assertion fails, so a bad run can't litter the Keychain.
    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_save_list_reveal_rename_forget() {
        if std::env::var("CLIPD_KEYCHAIN_TESTS").is_err() {
            return;
        }
        let marker = format!("clipd selftest {}", std::process::id());
        let mut entry = SecretEntry::new("correct horse battery staple");
        entry.title = marker.clone();
        entry.notes = "written by clipd's test suite".into();

        let saved = keychain::save(&entry).expect("save");
        let cleanup = saved.clone();
        let result = std::panic::catch_unwind(|| {
            let listed = list_secrets().expect("list");
            let found = listed
                .iter()
                .find(|s| s.account == cleanup.account)
                .expect("saved item appears in the listing");
            assert_eq!(found.title, cleanup.title);
            assert_eq!(found.service, keychain::SERVICE);
            assert!(found.saved_at.is_some(), "creation date should be reported");
            // The listing must never carry plaintext.
            assert!(!found.note.contains("correct horse"));

            assert_eq!(
                reveal_secret(found).expect("reveal"),
                "correct horse battery staple"
            );

            let renamed_to = format!("{} renamed", cleanup.title);
            rename_secret(found, &renamed_to).expect("rename");
            let after = list_secrets().expect("list after rename");
            let found = after
                .iter()
                .find(|s| s.account == cleanup.account)
                .expect("still present after rename");
            assert_eq!(found.title, renamed_to);
            // Renaming must not disturb the stored password.
            assert_eq!(
                reveal_secret(found).expect("reveal after rename"),
                "correct horse battery staple"
            );
        });

        forget_secret(&saved).expect("forget");
        assert!(
            list_secrets()
                .expect("list after delete")
                .iter()
                .all(|s| s.account != saved.account),
            "deleted item should be gone"
        );
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
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
