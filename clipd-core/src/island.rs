//! The notch island layout: what it can show, and where that data comes from.
//!
//! The island hugs the MacBook notch, so it is the one clipd surface that is
//! always on screen. What it holds is the clipboard: recent clips — text,
//! images, files and PDFs — that copy back with a click.
//!
//! It also carries two small **modules** that need no provider: a countdown,
//! and the next calendar event. An earlier version hosted a media player, a
//! battery gauge, a weather tile and a file shelf; they were removed because
//! none of them had anything to do with a clipboard, and a widget dashboard is
//! not what the notch is for.
//!
//! Calendar is read-only, best-effort, and shells out to `osascript` on the
//! cadence it declares — polled from a worker thread, never from `update`.

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

// ── Modules ──

/// One tile in the expanded island.
///
/// Serialized by name so reordering the enum, or a user hand-editing
/// `island.json`, can't silently repoint a module at a different tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IslandModule {
    /// Recent clips — text, images, files and PDFs. Click one to copy it.
    Clipboard,
    /// A drop shelf: drag files onto the island to carry them between apps.
    Files,
}

impl IslandModule {
    pub const ALL: [IslandModule; 2] = [
        IslandModule::Clipboard,
        IslandModule::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            IslandModule::Clipboard => "Clipboard",
            IslandModule::Files => "File shelf",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            IslandModule::Clipboard => {
                "Recent clips — text, images, files and PDFs. Click one to copy it."
            }
            IslandModule::Files => "Drag files onto the island to carry them between apps.",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            IslandModule::Clipboard => "☷",
            IslandModule::Files => "🗀",
        }
    }

    /// Whether this module can work at all on this platform. Everything that
    /// shells out to AppleScript or `pmset` is macOS-only; the two clipd-native
    /// modules work anywhere the GUI does.
    pub fn supported(self) -> bool {
        true
    }

    pub fn refresh_every(self) -> Option<Duration> {
        None
    }

    /// Whether the module reaches the network.
    ///
    /// Nothing does any more — the one that did was the weather. Kept as a
    /// hook so a future module that phones out has to say so where the user
    /// turns it on, rather than doing it quietly.
    pub fn uses_network(self) -> bool {
        false
    }
}

// ── Configuration ──

/// Where the island sits when the display has no notch to hug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IslandAnchor {
    /// Hug the notch when there is one, otherwise float under the menu bar.
    Auto,
    /// Always float below the menu bar, centred, even on a notched display.
    Floating,
}

impl Default for IslandAnchor {
    fn default() -> Self {
        Self::Auto
    }
}

impl IslandAnchor {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Hug the notch",
            Self::Floating => "Float under the menu bar",
        }
    }

    pub const ALL: [IslandAnchor; 2] = [Self::Auto, Self::Floating];
}

/// What clipd is holding right now, for the bar's three counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClipCounts {
    /// Captured today — "how much have I copied since this morning".
    pub copied: usize,
    pub pinned: usize,
    /// Everything the island can see without opening the palette.
    pub recent: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IslandConfig {
    /// Enabled modules, in the order they are drawn. Order is the list order,
    /// so a user reordering in settings is the whole story — no separate rank.
    #[serde(default = "default_modules")]
    pub modules: Vec<IslandModule>,

    /// Open on hover. Off means the island only expands when clicked, which is
    /// what you want if the notch is somewhere your pointer passes constantly.
    #[serde(default = "default_true")]
    pub expand_on_hover: bool,

    /// Briefly show each newly copied clip in the collapsed island — the
    /// "live activity" beat that makes a copy feel acknowledged.
    #[serde(default = "default_true")]
    pub live_activity: bool,

    /// Manual notch width in points. 0 asks AppKit, which is right on every
    /// notched Mac; the override exists for external displays and for people
    /// who want the resting island wider than the physical cutout.
    #[serde(default)]
    pub notch_width: f32,

    #[serde(default)]
    pub anchor: IslandAnchor,

    /// How many clips the clipboard tile lists.
    #[serde(default = "default_clip_rows")]
    pub clip_rows: usize,
}

fn default_true() -> bool {
    true
}
fn default_clip_rows() -> usize {
    5
}
fn default_modules() -> Vec<IslandModule> {
    vec![IslandModule::Clipboard]
}

impl Default for IslandConfig {
    fn default() -> Self {
        Self {
            modules: default_modules(),
            expand_on_hover: true,
            live_activity: true,
            notch_width: 0.0,
            anchor: IslandAnchor::default(),
            clip_rows: default_clip_rows(),
        }
    }
}

impl IslandConfig {
    pub fn has(&self, module: IslandModule) -> bool {
        self.modules.contains(&module)
    }

    /// Turn a module on (appended last) or off, keeping the rest of the order.
    pub fn set(&mut self, module: IslandModule, on: bool) {
        if on {
            if !self.has(module) {
                self.modules.push(module);
            }
        } else {
            self.modules.retain(|m| *m != module);
        }
    }

    /// Move an enabled module one place earlier or later in the draw order.
    pub fn shift(&mut self, module: IslandModule, delta: isize) {
        let Some(at) = self.modules.iter().position(|m| *m == module) else {
            return;
        };
        let to = at as isize + delta;
        if to < 0 || to >= self.modules.len() as isize {
            return;
        }
        self.modules.swap(at, to as usize);
    }

    /// Modules that are enabled and supported on this platform.
    pub fn active_modules(&self) -> Vec<IslandModule> {
        self.modules
            .iter()
            .copied()
            .filter(|m| m.supported())
            .collect()
    }
}

fn config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("island.json")
}

/// Whether the island is the layout the user has chosen.
///
/// Deliberately *not* a field on [`IslandConfig`]: the layout switch is one
/// setting shared by every clipd process, and keeping a second copy here is
/// how the palette and the island would end up both running, or neither.
/// Marker file recording that a clipd window is on screen.
fn gui_window_flag_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("gui-window.open")
}

/// How long a claim stays good without being refreshed.
const GUI_WINDOW_CLAIM_TTL: Duration = Duration::from_secs(3);

/// Announce that a real clipd window (palette, settings, HUD) is showing.
///
/// The island is a passive HUD that owns the top of the screen. A window the
/// user deliberately opened is not passive, and on a laptop display there is
/// no arrangement where a full-height settings window and the island can both
/// have that space — so the island yields while one is up.
///
/// The claim is a pid and a timestamp, and it expires. Checking whether the
/// owning process is still alive sounds more direct, but the only cheap way to
/// do it is `kill(pid, 0)`, which does not exist on Windows — the fallback
/// there had to assume the owner was alive, so a crashed window would have
/// pinned the island shut for the rest of the session. A timestamp behaves the
/// same on every platform, and it also survives the case a liveness check
/// cannot handle: the pid being recycled by something unrelated.
pub fn set_gui_window_open(open: bool) {
    let path = gui_window_flag_path();
    if open {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, claim_line());
    } else {
        // Release only our own claim. The flag is one file shared by every
        // clipd window, so a process clearing it unconditionally can drop
        // somebody else's — the palette raises it, tells the popover to hide,
        // and the popover's hide then wiped the palette's claim on the way
        // out, letting the island reappear underneath an open window.
        if read_claim(&path).is_some_and(|(pid, _)| pid == std::process::id()) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Keep our claim from expiring. Cheap enough to call on a timer while a
/// window is on screen; does nothing if the claim is somebody else's.
pub fn refresh_gui_window_claim() {
    let path = gui_window_flag_path();
    if read_claim(&path).is_some_and(|(pid, _)| pid == std::process::id()) {
        let _ = std::fs::write(path, claim_line());
    }
}

fn claim_line() -> String {
    format!("{} {}", std::process::id(), now_millis())
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn read_claim(path: &std::path::Path) -> Option<(u32, u128)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut parts = raw.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    // A claim with no timestamp is from an older build; treat it as expired
    // rather than trusting it forever.
    let stamp = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0);
    Some((pid, stamp))
}

/// Whether a clipd window is currently on screen.
pub fn gui_window_open() -> bool {
    let path = gui_window_flag_path();
    let Some((pid, stamp)) = read_claim(&path) else {
        return false;
    };
    if pid == std::process::id() {
        return true;
    }
    now_millis().saturating_sub(stamp) < GUI_WINDOW_CLAIM_TTL.as_millis()
}

/// How a slot is written on screen: `3`, or `A` for a letter slot.
///
/// Letter slots are stored as 31..=56 — the numbering the daemon assigns so
/// they share one `u8` with the numeric slots. That number is an
/// implementation detail: nobody presses 31, they press A. Anything showing a
/// slot to a person goes through here, so the island and the palette cannot
/// drift into showing different things for the same clip.
pub fn slot_badge(slot: u8) -> String {
    match slot {
        31..=56 => ((b'A' + (slot - 31)) as char).to_string(),
        other => other.to_string(),
    }
}

/// How much of the top of the screen the island can occupy when it is open.
///
/// Anything else that places a window near the top — the HUD toast, a palette
/// opened at the cursor — keeps below this so it does not land underneath the
/// island. Clicking the island's own gear puts the pointer *inside* that band,
/// so a window opened at the cursor lands squarely behind it.
pub const ISLAND_RESERVED_TOP: f32 = 340.0;

pub fn island_layout_active() -> bool {
    crate::transform::load_paste_transform_settings().gui_layout == crate::transform::GuiLayout::Notch
}

pub fn load_island_config() -> IslandConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_island_config(config: &IslandConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(path, json);
    }
}

// ── The file shelf ──

/// One file parked on the island.
///
/// The shelf stores *paths*, not copies. It is a carrying handle for files
/// that already exist — dragging a file on and then moving it would leave a
/// dead row, which `load_shelf` drops, rather than clipd silently holding a
/// duplicate of someone's 4 GB video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelfItem {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub added: DateTime<Utc>,
}

impl ShelfItem {
    pub fn from_path(path: PathBuf) -> Option<Self> {
        let meta = std::fs::metadata(&path).ok()?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        Some(Self {
            path,
            name,
            size: meta.len(),
            added: Utc::now(),
        })
    }
}

fn shelf_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("island_shelf.json")
}

/// Load the shelf, dropping rows whose file has since been moved or deleted.
pub fn load_shelf() -> Vec<ShelfItem> {
    let items: Vec<ShelfItem> = std::fs::read_to_string(shelf_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    items.into_iter().filter(|i| i.path.exists()).collect()
}

pub fn save_shelf(items: &[ShelfItem]) {
    let path = shelf_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(items) {
        let _ = std::fs::write(path, json);
    }
}

// ── Live readings ──

/// One event from Calendar.app.
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarEvent {
    pub title: String,
    pub calendar: String,
    pub start: DateTime<Local>,
    pub all_day: bool,
}

impl CalendarEvent {
    /// Minutes from now until the event starts. Negative once it has begun.
    pub fn minutes_until(&self) -> i64 {
        (self.start - Local::now()).num_minutes()
    }

    /// "in 25m", "in 3h 10m", "now", "started 5m ago".
    pub fn countdown(&self) -> String {
        let mins = self.minutes_until();
        if mins <= -1 {
            return format!("started {}m ago", -mins);
        }
        if mins == 0 {
            return "now".into();
        }
        if mins < 60 {
            return format!("in {mins}m");
        }
        let (h, m) = (mins / 60, mins % 60);
        if m == 0 {
            format!("in {h}h")
        } else {
            format!("in {h}h {m}m")
        }
    }
}

/// Everything the background poller has managed to read so far.
#[derive(Debug, Clone, Default)]
pub struct IslandSnapshot {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_slots_read_as_letters() {
        // Letter slots are stored as 31..=56 so they share one u8 with the
        // numeric slots. That number is an implementation detail — nobody
        // presses 31, they press A — so a slot shown to a person that reads
        // "31" is simply wrong.
        assert_eq!(slot_badge(31), "A");
        assert_eq!(slot_badge(56), "Z");
        assert_eq!(slot_badge(32), "B");
        // Numeric slots are unchanged, including the ones either side of the
        // letter range.
        assert_eq!(slot_badge(1), "1");
        assert_eq!(slot_badge(9), "9");
        assert_eq!(slot_badge(30), "30");
        assert_eq!(slot_badge(57), "57");
    }

    #[test]
    fn a_dead_window_does_not_keep_the_island_hidden() {
        // Everything about this flag lives in one file, so the whole story is
        // one test — split across two, they raced each other's writes.
        // The flag carries the pid of the window that set it. If that process
        // is gone — crashed, force-quit — the island must come back rather
        // than staying hidden for the rest of the session.
        let path = gui_window_flag_path();
        let restore = std::fs::read_to_string(&path).ok();

        set_gui_window_open(true);
        assert!(gui_window_open(), "our own pid counts as open");

        // A pid that cannot be running: the kernel's maximum is well below
        // this on every platform clipd builds for.
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // A claim old enough to have expired: a window that crashed without
        // releasing must not hold the island down for the rest of the session.
        std::fs::write(&path, "4194303 1").expect("write flag");
        assert!(!gui_window_open(), "an expired claim must not hold the island down");

        // A fresh claim from another process is honoured.
        let fresh = format!("4194303 {}", now_millis());
        std::fs::write(&path, fresh).expect("write flag");
        assert!(gui_window_open(), "a live window's claim should stand");

        std::fs::write(&path, "not a pid").expect("write flag");
        assert!(!gui_window_open(), "garbage in the flag is not a window");

        // Releasing drops only your own claim. One file is shared by every
        // clipd window, so an unconditional release drops somebody else's:
        // the palette raises the flag and asks the popover to hide, and the
        // popover's hide would wipe the palette's claim on the way out,
        // letting the island reappear underneath an open window.
        std::fs::write(&path, "4194303 999").expect("write flag");
        set_gui_window_open(false);
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref().map(str::trim),
            Some("4194303 999"),
            "another process's claim must survive our release"
        );

        set_gui_window_open(true);
        set_gui_window_open(false);
        assert!(!gui_window_open());

        if let Some(prev) = restore {
            let _ = std::fs::write(&path, prev);
        }
    }

    #[test]
    fn module_order_survives_toggling() {
        // Order is the user's, set in settings, and toggling one module must
        // not shuffle the others.
        let mut config = IslandConfig::default();
        config.modules = vec![IslandModule::Clipboard];

        config.set(IslandModule::Files, true);
        assert_eq!(
            config.modules,
            vec![IslandModule::Clipboard, IslandModule::Files],
            "a newly enabled module goes on the end"
        );

        config.shift(IslandModule::Files, -1);
        assert_eq!(
            config.modules,
            vec![IslandModule::Files, IslandModule::Clipboard]
        );

        // Shifting past either end is a no-op, not a panic.
        config.shift(IslandModule::Files, -1);
        config.shift(IslandModule::Clipboard, 5);
        assert_eq!(
            config.modules,
            vec![IslandModule::Files, IslandModule::Clipboard]
        );

        config.set(IslandModule::Clipboard, false);
        assert!(!config.has(IslandModule::Clipboard));
        assert_eq!(config.modules, vec![IslandModule::Files]);
    }
}
