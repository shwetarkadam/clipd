//! Bring your history over from another clipboard manager.
//!
//! Switching costs nothing when the clips, pins and snippets you already have
//! come with you, so clipd reads the other app's own store and imports from
//! it in one click. Read-only on the other side: each database is copied to a
//! temporary folder and read from there, so a running Maccy or Alfred is never
//! locked or touched.
//!
//! - **Maccy** — `Storage.sqlite` (Core Data / SwiftData): history and pins.
//! - **Alfred** — `clipboard.alfdb` and `snippets.alfdb`: history and snippets.
//! - **Paste** — `db.sqlite` (Core Data): history, with pinboard items as pins.
//!   Its pasteboard payloads are archived property lists with no published
//!   format, so text is recovered on a best-effort basis and the import shows
//!   samples before anything is written.
//! - **Raycast** — its database is encrypted, so history cannot be read; its
//!   snippets come over from the JSON file Raycast's "Export Snippets" writes.
//!
//! Text only: images and files are counted and left behind. Secrets the
//! detector flags are left behind too, as they would be on a fresh copy.

use crate::models::ClipEntry;
use crate::privacy::{detect_sensitive, PrivacyConfig};
use crate::store::ClipStore;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Seconds from the Unix epoch to Core Data's reference date (2001-01-01).
const CORE_DATA_EPOCH: f64 = 978_307_200.0;
/// The collection the GUI shows as Pinned.
const PINNED_COLLECTION: &str = "Pinned";

/// Which app the data comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportSource {
    Maccy,
    Alfred,
    Paste,
    Raycast,
}

impl ImportSource {
    pub fn label(&self) -> &'static str {
        match self {
            ImportSource::Maccy => "Maccy",
            ImportSource::Alfred => "Alfred",
            ImportSource::Paste => "Paste",
            ImportSource::Raycast => "Raycast",
        }
    }

    fn key(&self) -> &'static str {
        match self {
            ImportSource::Maccy => "maccy",
            ImportSource::Alfred => "alfred",
            ImportSource::Paste => "paste",
            ImportSource::Raycast => "raycast",
        }
    }
}

/// One clip to bring over.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedClip {
    pub text: String,
    pub at: DateTime<Utc>,
    pub app: Option<String>,
    pub pinned: bool,
}

/// One snippet to bring over.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedSnippet {
    pub trigger: String,
    pub name: String,
    pub text: String,
}

/// Everything read from one source, before anything is written.
#[derive(Debug, Clone)]
pub struct ImportBundle {
    pub source: ImportSource,
    pub clips: Vec<ImportedClip>,
    pub snippets: Vec<ImportedSnippet>,
    /// Images and files, which this import does not bring over.
    pub non_text: usize,
}

impl ImportBundle {
    pub fn pins(&self) -> usize {
        self.clips.iter().filter(|clip| clip.pinned).count()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && self.snippets.is_empty()
    }
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub added: usize,
    pub already_had: usize,
    pub pinned: usize,
    pub snippets: usize,
    pub secrets_skipped: usize,
    pub non_text_skipped: usize,
}

impl ImportReport {
    /// "Imported 1,204 clips, 12 pins and 8 snippets from Maccy."
    pub fn summary(&self, source: ImportSource) -> String {
        fn count(n: usize, one: &str, many: &str) -> String {
            format!("{n} {}", if n == 1 { one } else { many })
        }
        let mut parts = vec![count(self.added + self.already_had, "clip", "clips")];
        if self.pinned > 0 {
            parts.push(count(self.pinned, "pin", "pins"));
        }
        if self.snippets > 0 {
            parts.push(count(self.snippets, "snippet", "snippets"));
        }
        let list = match parts.len() {
            1 => parts[0].clone(),
            _ => {
                let last = parts.pop().unwrap_or_default();
                format!("{} and {last}", parts.join(", "))
            }
        };
        let mut text = format!("Imported {list} from {}.", source.label());
        if self.secrets_skipped > 0 {
            text.push_str(&format!(" {} left out (they look like secrets).", count(self.secrets_skipped, "clip", "clips")));
        }
        if self.non_text_skipped > 0 {
            text.push_str(&format!(" {} stayed behind.", count(self.non_text_skipped, "image or file", "images and files")));
        }
        text
    }
}

/// A source found on this Mac: where its data is.
#[derive(Debug, Clone)]
pub struct FoundSource {
    pub source: ImportSource,
    pub path: PathBuf,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn maccy_db() -> PathBuf {
    home().join("Library/Containers/org.p0deje.Maccy/Data/Library/Application Support/Maccy/Storage.sqlite")
}

fn alfred_dir() -> PathBuf {
    home().join("Library/Application Support/Alfred/Databases")
}

fn paste_db() -> PathBuf {
    home().join("Library/Containers/com.wiheads.paste/Data/Library/Application Support/Paste/db.sqlite")
}

/// Whether Raycast is installed (its history is encrypted; only its exported
/// snippets can be imported).
pub fn raycast_installed() -> bool {
    home().join("Library/Application Support/com.raycast.macos").exists()
}

/// Every other clipboard manager whose data is on this Mac.
pub fn detect_sources() -> Vec<FoundSource> {
    let mut found = Vec::new();
    if maccy_db().exists() {
        found.push(FoundSource { source: ImportSource::Maccy, path: maccy_db() });
    }
    let alfred = alfred_dir();
    if alfred.join("clipboard.alfdb").exists() || alfred.join("snippets.alfdb").exists() {
        found.push(FoundSource { source: ImportSource::Alfred, path: alfred });
    }
    if paste_db().exists() {
        found.push(FoundSource { source: ImportSource::Paste, path: paste_db() });
    }
    if let Some(export) = find_raycast_export() {
        found.push(FoundSource { source: ImportSource::Raycast, path: export });
    }
    found
}

/// Read everything a source has, without writing anything.
pub fn read_source(found: &FoundSource) -> Result<ImportBundle, String> {
    match found.source {
        ImportSource::Maccy => with_copy(&found.path, read_maccy),
        ImportSource::Alfred => read_alfred(&found.path),
        ImportSource::Paste => with_copy(&found.path, read_paste),
        ImportSource::Raycast => read_raycast_snippets(&found.path),
    }
}

/// Open a copy of a SQLite database (with its WAL and SHM) so the owning app
/// is never locked, then read it.
fn with_copy(
    db: &Path,
    read: impl Fn(&Connection) -> Result<ImportBundle, String>,
) -> Result<ImportBundle, String> {
    let dir = std::env::temp_dir().join(format!(
        "clipd-import-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = db.file_name().map(|n| n.to_owned()).unwrap_or_default();
    let copy = dir.join(&name);
    let result = (|| {
        std::fs::copy(db, &copy).map_err(|e| format!("Couldn't read {}: {e}", db.display()))?;
        for suffix in ["-wal", "-shm"] {
            let mut side = db.as_os_str().to_owned();
            side.push(suffix);
            let side = PathBuf::from(side);
            if side.exists() {
                let mut target = copy.as_os_str().to_owned();
                target.push(suffix);
                let _ = std::fs::copy(&side, PathBuf::from(target));
            }
        }
        let conn = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("Couldn't open {}: {e}", db.display()))?;
        read(&conn)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// "com.brave.Browser" → "Brave Browser". Maccy records the source app by
/// bundle identifier, and clipd shows app names; read the name from the
/// installed app's Info.plist, else fall back to the identifier's last part.
fn app_name(app: &str) -> String {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();
    if !app.contains('.') || app.contains(' ') {
        return app.to_string();
    }
    let names = NAMES.get_or_init(|| {
        let mut names = HashMap::new();
        for dir in [
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Applications/Utilities"),
            PathBuf::from("/Applications/Utilities"),
            home().join("Applications"),
        ] {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            for entry in entries.flatten() {
                let plist_path = entry.path().join("Contents/Info.plist");
                let Ok(value) = plist::Value::from_file(&plist_path) else { continue };
                let Some(dict) = value.as_dictionary() else { continue };
                let get = |key: &str| dict.get(key).and_then(|v| v.as_string()).map(str::to_string);
                if let Some(id) = get("CFBundleIdentifier") {
                    let name = get("CFBundleDisplayName").or_else(|| get("CFBundleName")).or_else(|| {
                        entry.path().file_stem().map(|s| s.to_string_lossy().to_string())
                    });
                    if let Some(name) = name {
                        names.insert(id.to_lowercase(), name);
                    }
                }
            }
        }
        names
    });
    names
        .get(&app.to_lowercase())
        .cloned()
        .unwrap_or_else(|| app.rsplit('.').next().unwrap_or(app).to_string())
}

fn core_data_time(seconds: f64) -> DateTime<Utc> {
    Utc.timestamp_opt((seconds + CORE_DATA_EPOCH) as i64, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

fn columns(conn: &Connection, table: &str) -> HashSet<String> {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default()
}

fn has_table(conn: &Connection, table: &str) -> bool {
    !columns(conn, table).is_empty()
}

fn is_text_type(uti: &str) -> bool {
    matches!(
        uti,
        "public.utf8-plain-text" | "public.plain-text" | "public.text" | "NSStringPboardType"
            | "public.utf16-plain-text"
    )
}

/// Maccy: `ZHISTORYITEM` rows with their contents in `ZHISTORYITEMCONTENT`.
fn read_maccy(conn: &Connection) -> Result<ImportBundle, String> {
    if !has_table(conn, "ZHISTORYITEM") || !has_table(conn, "ZHISTORYITEMCONTENT") {
        return Err("This doesn't look like a Maccy history.".into());
    }
    let item_cols = columns(conn, "ZHISTORYITEM");
    let pick = |name: &str| if item_cols.contains(name) { format!("i.{name}") } else { "NULL".into() };
    let sql = format!(
        "SELECT i.Z_PK, {}, {}, {}, c.ZTYPE, c.ZVALUE
         FROM ZHISTORYITEM i LEFT JOIN ZHISTORYITEMCONTENT c ON c.ZITEM = i.Z_PK
         ORDER BY i.Z_PK",
        pick("ZLASTCOPIEDAT"),
        pick("ZAPPLICATION"),
        pick("ZPIN"),
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    // Gather per item: its best text, and whether it had anything at all.
    let mut order: Vec<i64> = Vec::new();
    let mut items: std::collections::HashMap<i64, (Option<ImportedClip>, bool)> = Default::default();
    for row in rows.filter_map(Result::ok) {
        let (id, at, app, pin, uti, value) = row;
        let entry = items.entry(id).or_insert_with(|| {
            order.push(id);
            (None, false)
        });
        entry.1 = true;
        if entry.0.is_some() {
            continue;
        }
        let Some(uti) = uti else { continue };
        if !is_text_type(&uti) {
            continue;
        }
        let Some(text) = value.and_then(|bytes| String::from_utf8(bytes).ok()) else { continue };
        if text.trim().is_empty() {
            continue;
        }
        entry.0 = Some(ImportedClip {
            text,
            at: at.map(core_data_time).unwrap_or_else(Utc::now),
            app: app.filter(|a| !a.trim().is_empty()).map(|a| app_name(&a)),
            pinned: pin.is_some_and(|p| !p.trim().is_empty()),
        });
    }
    let mut clips = Vec::new();
    let mut non_text = 0;
    for id in order {
        match items.remove(&id) {
            Some((Some(clip), _)) => clips.push(clip),
            Some((None, true)) => non_text += 1,
            _ => {}
        }
    }
    Ok(ImportBundle { source: ImportSource::Maccy, clips, snippets: Vec::new(), non_text })
}

/// Alfred: clipboard history and snippets, each in its own database.
fn read_alfred(dir: &Path) -> Result<ImportBundle, String> {
    let mut bundle = ImportBundle {
        source: ImportSource::Alfred,
        clips: Vec::new(),
        snippets: Vec::new(),
        non_text: 0,
    };
    let clipboard = dir.join("clipboard.alfdb");
    if clipboard.exists() {
        let read = with_copy(&clipboard, |conn| {
            let mut stmt = conn
                .prepare("SELECT item, ts, app, dataType FROM clipboard ORDER BY ts")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let mut out = ImportBundle {
                source: ImportSource::Alfred,
                clips: Vec::new(),
                snippets: Vec::new(),
                non_text: 0,
            };
            for (item, ts, app, kind) in rows.filter_map(Result::ok) {
                // 0 is text; 1 an image, 2 files.
                if kind.unwrap_or(0) != 0 {
                    out.non_text += 1;
                    continue;
                }
                let Some(text) = item.filter(|t| !t.trim().is_empty()) else { continue };
                out.clips.push(ImportedClip {
                    text,
                    at: ts.map(core_data_time).unwrap_or_else(Utc::now),
                    app: app.filter(|a| !a.trim().is_empty()),
                    pinned: false,
                });
            }
            Ok(out)
        })?;
        bundle.clips = read.clips;
        bundle.non_text = read.non_text;
    }
    let snippets = dir.join("snippets.alfdb");
    if snippets.exists() {
        let read = with_copy(&snippets, |conn| {
            let mut stmt = conn
                .prepare("SELECT name, keyword, snippet FROM snippets")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let snippets = rows
                .filter_map(Result::ok)
                .filter_map(|(name, keyword, text)| snippet(name, keyword, text))
                .collect();
            Ok(ImportBundle {
                source: ImportSource::Alfred,
                clips: Vec::new(),
                snippets,
                non_text: 0,
            })
        })?;
        bundle.snippets = read.snippets;
    }
    Ok(bundle)
}

fn snippet(name: Option<String>, keyword: Option<String>, text: Option<String>) -> Option<ImportedSnippet> {
    let text = text.filter(|t| !t.trim().is_empty())?;
    let name = name.unwrap_or_default().trim().to_string();
    let keyword = keyword.unwrap_or_default().trim().to_string();
    let trigger = if !keyword.is_empty() { keyword } else { trigger_from(&name) };
    if trigger.is_empty() {
        return None;
    }
    let name = if name.is_empty() { trigger.clone() } else { name };
    Some(ImportedSnippet { trigger, name, text })
}

fn trigger_from(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(32).collect()
}

/// Paste: `ZITEMENTITY` for each clip, its pasteboard payload archived in
/// `ZITEMDATAENTITY.ZRAWPASTEBOARDITEMS`. Column names are probed rather than
/// assumed, since Paste has changed them between versions.
fn read_paste(conn: &Connection) -> Result<ImportBundle, String> {
    if !has_table(conn, "ZITEMENTITY") {
        return Err("This doesn't look like a Paste library.".into());
    }
    let item = columns(conn, "ZITEMENTITY");
    let first = |names: &[&str]| names.iter().find(|n| item.contains(**n)).map(|n| n.to_string());
    let date = first(&["ZCREATEDAT", "ZCREATIONDATE", "ZDATE", "ZTIMESTAMP", "ZLASTUSEDAT", "ZUPDATEDAT", "ZMODIFIEDAT"]);
    let title = first(&["ZTITLE", "ZPLAINTEXT", "ZTEXT", "ZPREVIEW", "ZNAME"]);
    let list = first(&["ZLIST", "ZPINBOARD", "ZLISTENTITY"]);
    let app_fk = first(&["ZAPPLICATION", "ZAPPLICATIONENTITY", "ZSOURCEAPPLICATION"]);

    let data_cols = columns(conn, "ZITEMDATAENTITY");
    let (data_join, data_col) = if data_cols.contains("ZRAWPASTEBOARDITEMS") {
        let join = if item.contains("ZDATA") {
            "LEFT JOIN ZITEMDATAENTITY d ON d.Z_PK = i.ZDATA"
        } else if data_cols.contains("ZITEM") {
            "LEFT JOIN ZITEMDATAENTITY d ON d.ZITEM = i.Z_PK"
        } else {
            ""
        };
        (join, if join.is_empty() { "NULL" } else { "d.ZRAWPASTEBOARDITEMS" })
    } else {
        ("", "NULL")
    };
    let app_cols = columns(conn, "ZAPPLICATIONENTITY");
    let app_name_col = ["ZNAME", "ZTITLE", "ZBUNDLEIDENTIFIER", "ZBUNDLEID"]
        .iter()
        .find(|n| app_cols.contains(**n));
    let (app_join, app_col) = match (&app_fk, app_name_col) {
        (Some(fk), Some(name)) => (
            format!("LEFT JOIN ZAPPLICATIONENTITY a ON a.Z_PK = i.{fk}"),
            format!("a.{name}"),
        ),
        _ => (String::new(), "NULL".to_string()),
    };
    let col = |c: &Option<String>| c.as_ref().map(|c| format!("i.{c}")).unwrap_or_else(|| "NULL".into());
    let sql = format!(
        "SELECT {}, {}, {}, {data_col}, {app_col} FROM ZITEMENTITY i {data_join} {app_join}",
        col(&date),
        col(&title),
        col(&list),
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("Couldn't read Paste's library: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<f64>>(0).unwrap_or(None),
                row.get::<_, Option<String>>(1).unwrap_or(None),
                row.get::<_, Option<i64>>(2).unwrap_or(None),
                row.get::<_, Option<Vec<u8>>>(3).unwrap_or(None),
                row.get::<_, Option<String>>(4).unwrap_or(None),
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut clips = Vec::new();
    let mut non_text = 0;
    for (at, title, list, raw, app) in rows.filter_map(Result::ok) {
        let from_payload = raw.as_deref().and_then(text_from_pasteboard_archive);
        let text = match from_payload {
            Some(PayloadText::Text(text)) => Some(text),
            Some(PayloadText::NotText) => None,
            None => title.filter(|t| !t.trim().is_empty()),
        };
        match text {
            Some(text) => clips.push(ImportedClip {
                text,
                at: at.map(core_data_time).unwrap_or_else(Utc::now),
                app: app.filter(|a| !a.trim().is_empty()).map(|a| app_name(&a)),
                pinned: list.is_some(),
            }),
            None => non_text += 1,
        }
    }
    clips.sort_by_key(|clip| clip.at);
    Ok(ImportBundle { source: ImportSource::Paste, clips, snippets: Vec::new(), non_text })
}

/// What an archived pasteboard item holds.
#[derive(Debug, PartialEq)]
enum PayloadText {
    Text(String),
    /// An image or file, with no text representation.
    NotText,
}

/// Best-effort: find the plain-text representation in an archived pasteboard
/// item. A dictionary keyed by type identifier is read directly; a keyed
/// archive (`$objects`) is searched for the text type and the first payload
/// that decodes as text.
fn text_from_pasteboard_archive(bytes: &[u8]) -> Option<PayloadText> {
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let mut types: Vec<String> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    walk(&value, &mut types, &mut texts, None);
    if let Some(text) = texts.into_iter().find(|t| !t.trim().is_empty()) {
        return Some(PayloadText::Text(text));
    }
    let non_text = types.iter().any(|t| {
        t.starts_with("public.png")
            || t.starts_with("public.tiff")
            || t.starts_with("public.jpeg")
            || t == "public.file-url"
            || t.starts_with("com.adobe.pdf")
    });
    non_text.then_some(PayloadText::NotText)
}

fn walk(value: &plist::Value, types: &mut Vec<String>, texts: &mut Vec<String>, under: Option<&str>) {
    match value {
        plist::Value::Dictionary(dict) => {
            for (key, inner) in dict {
                if key.contains('.') || key.starts_with("NS") {
                    types.push(key.clone());
                }
                let next = if is_text_type(key) { Some(key.as_str()) } else { under };
                walk(inner, types, texts, next);
            }
        }
        plist::Value::Array(items) => {
            // A keyed archive: note the type strings, then take text payloads.
            for item in items {
                if let plist::Value::String(s) = item {
                    if is_uti_like(s) {
                        types.push(s.clone());
                    }
                }
            }
            let archive_has_text = items
                .iter()
                .any(|v| matches!(v, plist::Value::String(s) if is_text_type(s)));
            for item in items {
                walk(item, types, texts, if archive_has_text { Some("archive") } else { under });
            }
        }
        plist::Value::Data(bytes) if under.is_some() => {
            if let Some(text) = decode_text(bytes) {
                texts.push(text);
            }
        }
        plist::Value::String(s) if under.is_some_and(|u| u != "archive") => {
            texts.push(s.clone());
        }
        _ => {}
    }
}

fn is_uti_like(s: &str) -> bool {
    s.contains('.') && !s.contains(' ') && s.len() < 80 && s.chars().all(|c| c.is_ascii_graphic())
}

/// UTF-8 (or UTF-16 with a BOM) that reads as text: no NULs, mostly printable.
fn decode_text(bytes: &[u8]) -> Option<String> {
    let text = if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let little = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if little { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16(&units).ok()?
    } else {
        String::from_utf8(bytes.to_vec()).ok()?
    };
    let total = text.chars().count();
    if total == 0 || text.contains('\0') {
        return None;
    }
    let printable = text.chars().filter(|c| !c.is_control() || c.is_whitespace()).count();
    (printable * 10 >= total * 9).then_some(text)
}

/// Raycast's "Export Snippets" file: a JSON array of `{name, text, keyword}`.
fn read_raycast_snippets(path: &Path) -> Result<ImportBundle, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let snippets = parse_raycast_snippets(&text).ok_or("This isn't a Raycast snippets export.")?;
    Ok(ImportBundle { source: ImportSource::Raycast, clips: Vec::new(), snippets, non_text: 0 })
}

fn parse_raycast_snippets(text: &str) -> Option<Vec<ImportedSnippet>> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let items = match &value {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(map) => map.get("snippets")?.as_array()?.clone(),
        _ => return None,
    };
    let out: Vec<ImportedSnippet> = items
        .iter()
        .filter_map(|item| {
            let field = |key: &str| item.get(key).and_then(|v| v.as_str()).map(str::to_string);
            snippet(field("name"), field("keyword"), field("text"))
        })
        .collect();
    (!out.is_empty()).then_some(out)
}

/// A Raycast snippets export in Downloads, Desktop or Documents, newest first.
pub fn find_raycast_export() -> Option<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for dir in ["Downloads", "Desktop", "Documents"] {
        let Ok(entries) = std::fs::read_dir(home().join(dir)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
            if !(name.ends_with(".json") && name.contains("snippet")) {
                continue;
            }
            let small = entry.metadata().map(|m| m.len() < 5_000_000).unwrap_or(false);
            let parses = small
                && std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| parse_raycast_snippets(&t))
                    .is_some();
            if parses {
                let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                found.push((modified, path));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().next().map(|(_, path)| path)
}

/// Write a bundle into clipd. Clips already in history are left where they
/// are (re-dating them would shuffle the list); a pin still applies to them.
/// Snippets never replace one of yours: a clashing trigger gets the source's
/// name appended.
pub fn apply_import(store: &ClipStore, bundle: &ImportBundle, privacy: &PrivacyConfig) -> ImportReport {
    let mut report = ImportReport { non_text_skipped: bundle.non_text, ..Default::default() };
    let pinned_collection = if bundle.pins() > 0 {
        match store.get_collection_by_name(PINNED_COLLECTION) {
            Ok(Some(collection)) => Some(collection.id),
            _ => store.create_collection(PINNED_COLLECTION, None).ok(),
        }
    } else {
        None
    };
    let mut clips: Vec<&ImportedClip> = bundle.clips.iter().collect();
    clips.sort_by_key(|clip| clip.at);
    for clip in clips {
        if !detect_sensitive(&clip.text, privacy).is_empty() {
            report.secrets_skipped += 1;
            continue;
        }
        let id = match store.find_by_content(&clip.text) {
            Ok(Some(existing)) => {
                report.already_had += 1;
                existing.id
            }
            _ => {
                let mut entry = ClipEntry::new(clip.text.clone(), clip.app.clone(), None);
                entry.timestamp = clip.at;
                match store.insert(&entry) {
                    Ok(id) => {
                        report.added += 1;
                        id
                    }
                    Err(_) => continue,
                }
            }
        };
        if clip.pinned {
            if let Some(collection) = pinned_collection {
                if store.add_clip_to_collection(collection, id).is_ok() {
                    report.pinned += 1;
                }
            }
        }
    }
    let existing: Vec<(String, String)> = store
        .list_snippets()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.trigger, s.body))
        .collect();
    for snippet in &bundle.snippets {
        let clash = existing.iter().find(|(trigger, _)| *trigger == snippet.trigger);
        let trigger = match clash {
            Some((_, body)) if *body == snippet.text => {
                report.snippets += 1;
                continue;
            }
            Some(_) => format!("{}-{}", snippet.trigger, bundle.source.key()),
            None => snippet.trigger.clone(),
        };
        if store.upsert_snippet(&trigger, &snippet.name, &snippet.text).is_ok() {
            report.snippets += 1;
        }
    }
    mark_imported(bundle.source);
    report
}

fn imports_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("imports.json")
}

fn imports_state() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(imports_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_imports_state(state: &serde_json::Map<String, serde_json::Value>) {
    let path = imports_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, text);
    }
}

fn mark_imported(source: ImportSource) {
    let mut state = imports_state();
    state.insert(source.key().into(), serde_json::Value::String(Utc::now().to_rfc3339()));
    write_imports_state(&state);
}

/// Whether this source was imported, or its offer dismissed.
pub fn import_answered(source: ImportSource) -> bool {
    let state = imports_state();
    state.contains_key(source.key()) || state.contains_key(&format!("{}-dismissed", source.key()))
}

/// "Not now, and don't offer it again."
pub fn dismiss_import(source: ImportSource) {
    let mut state = imports_state();
    state.insert(format!("{}-dismissed", source.key()), serde_json::Value::Bool(true));
    write_imports_state(&state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clipd-import-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn maccy_history_and_pins_come_over_as_text() {
        let dir = temp_dir("maccy");
        let db = dir.join("Storage.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE ZHISTORYITEM (Z_PK INTEGER PRIMARY KEY, ZAPPLICATION TEXT, ZLASTCOPIEDAT REAL, ZPIN TEXT, ZTITLE TEXT);
             CREATE TABLE ZHISTORYITEMCONTENT (Z_PK INTEGER PRIMARY KEY, ZITEM INTEGER, ZTYPE TEXT, ZVALUE BLOB);
             INSERT INTO ZHISTORYITEM VALUES (1, 'com.apple.Terminal', 800000000, NULL, 'cargo build');
             INSERT INTO ZHISTORYITEM VALUES (2, 'com.brave.Browser', 800000100, 'b', 'link');
             INSERT INTO ZHISTORYITEM VALUES (3, NULL, 800000200, NULL, 'Image');
             INSERT INTO ZHISTORYITEMCONTENT VALUES (1, 1, 'public.utf8-plain-text', CAST('cargo build' AS BLOB));
             INSERT INTO ZHISTORYITEMCONTENT VALUES (2, 1, 'public.rtf', CAST('{\\rtf' AS BLOB));
             INSERT INTO ZHISTORYITEMCONTENT VALUES (3, 2, 'public.utf8-plain-text', CAST('https://example.com' AS BLOB));
             INSERT INTO ZHISTORYITEMCONTENT VALUES (4, 3, 'public.png', X'89504E47');",
        )
        .unwrap();
        drop(conn);
        let bundle = read_source(&FoundSource { source: ImportSource::Maccy, path: db }).unwrap();
        let texts: Vec<&str> = bundle.clips.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["cargo build", "https://example.com"]);
        assert_eq!(bundle.pins(), 1);
        assert_eq!(bundle.non_text, 1);
        assert_eq!(bundle.clips[0].at.timestamp(), 800000000 + 978307200);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alfred_history_and_snippets_come_over() {
        let dir = temp_dir("alfred");
        let clip_db = Connection::open(dir.join("clipboard.alfdb")).unwrap();
        clip_db
            .execute_batch(
                "CREATE TABLE clipboard(item, ts decimal, app, apppath, dataType integer, dataHash);
                 INSERT INTO clipboard VALUES ('git push', 790000000, 'Terminal', '', 0, 'a');
                 INSERT INTO clipboard VALUES ('shot.png', 790000100, 'Finder', '', 1, 'b');",
            )
            .unwrap();
        drop(clip_db);
        let snip_db = Connection::open(dir.join("snippets.alfdb")).unwrap();
        snip_db
            .execute_batch(
                "CREATE TABLE snippets(uid, name, keyword, snippet, snippetrtf BLOB, collection, autoexpand BOOLEAN, ignoredynamicplaceholders BOOLEAN);
                 INSERT INTO snippets VALUES ('1', 'Email sign-off', 'sig', 'Best, Ada', NULL, 'Mail', 1, 0);
                 INSERT INTO snippets VALUES ('2', 'Home Address', '', '1 Main St', NULL, 'Me', 0, 0);",
            )
            .unwrap();
        drop(snip_db);
        let bundle = read_source(&FoundSource { source: ImportSource::Alfred, path: dir.clone() }).unwrap();
        assert_eq!(bundle.clips.len(), 1);
        assert_eq!(bundle.non_text, 1);
        let triggers: Vec<&str> = bundle.snippets.iter().map(|s| s.trigger.as_str()).collect();
        assert_eq!(triggers, vec!["sig", "home-address"], "a missing keyword comes from the name");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_ids_become_app_names() {
        assert_eq!(app_name("com.apple.Terminal"), "Terminal");
        assert_eq!(app_name("Brave Browser"), "Brave Browser", "a name is left alone");
        assert_eq!(app_name("com.example.notinstalled"), "notinstalled");
    }

    #[test]
    fn raycast_snippet_exports_parse_and_other_json_does_not() {
        let export = r#"[{"name":"Greeting","text":"Hi there","keyword":"hi"},{"name":"Empty","text":""}]"#;
        let snippets = parse_raycast_snippets(export).unwrap();
        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].trigger, "hi");
        assert!(parse_raycast_snippets(r#"{"name":"package","version":"1.0.0"}"#).is_none());
    }

    #[test]
    fn paste_payloads_yield_their_text_and_images_are_not_text() {
        let mut dict = plist::Dictionary::new();
        dict.insert("public.utf8-plain-text".into(), plist::Value::Data(b"hello from Paste".to_vec()));
        dict.insert("public.rtf".into(), plist::Value::Data(b"{\\rtf1}".to_vec()));
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dict).to_writer_binary(&mut bytes).unwrap();
        assert_eq!(
            text_from_pasteboard_archive(&bytes),
            Some(PayloadText::Text("hello from Paste".into()))
        );
        let mut image = plist::Dictionary::new();
        image.insert("public.png".into(), plist::Value::Data(vec![0x89, 0x50, 0x4E, 0x47, 0, 1]));
        let mut bytes = Vec::new();
        plist::Value::Dictionary(image).to_writer_binary(&mut bytes).unwrap();
        assert_eq!(text_from_pasteboard_archive(&bytes), Some(PayloadText::NotText));
    }

    #[test]
    fn importing_skips_secrets_keeps_existing_clips_and_never_clobbers_snippets() {
        let store = ClipStore::in_memory().unwrap();
        let mut mine = ClipEntry::new("already here".into(), None, None);
        mine.timestamp = Utc::now();
        store.insert(&mine).unwrap();
        store.upsert_snippet("sig", "My sign-off", "Cheers, me").unwrap();
        let bundle = ImportBundle {
            source: ImportSource::Alfred,
            clips: vec![
                ImportedClip { text: "already here".into(), at: Utc::now() - chrono::Duration::days(30), app: None, pinned: true },
                ImportedClip { text: "brand new".into(), at: Utc::now() - chrono::Duration::days(2), app: Some("Terminal".into()), pinned: false },
                ImportedClip { text: "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789".into(), at: Utc::now(), app: None, pinned: false },
            ],
            snippets: vec![ImportedSnippet { trigger: "sig".into(), name: "Sign-off".into(), text: "Best, Ada".into() }],
            non_text: 2,
        };
        let report = apply_import(&store, &bundle, &PrivacyConfig::default());
        assert_eq!(report.added, 1);
        assert_eq!(report.already_had, 1);
        assert_eq!(report.pinned, 1);
        assert_eq!(report.secrets_skipped, 1);
        assert_eq!(report.non_text_skipped, 2);
        let snippets = store.list_snippets().unwrap();
        assert!(snippets.iter().any(|s| s.trigger == "sig" && s.body == "Cheers, me"), "yours is kept");
        assert!(snippets.iter().any(|s| s.trigger == "sig-alfred" && s.body == "Best, Ada"));
        assert!(report.summary(ImportSource::Alfred).starts_with("Imported 2 clips, 1 pin and 1 snippet from Alfred."));
    }
}
