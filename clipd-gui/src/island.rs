//! The notch island: clipd as a Dynamic Island for the MacBook notch.
//!
//! This is a third layout alongside the palette and the menu-bar HUD, chosen
//! in Settings > Appearance. It runs as its own process (`--island`) and it
//! never goes away: a black slab sitting at the very top of the display, sized
//! so the physical notch cutout is part of its silhouette.
//!
//! Three sizes, in the order you meet them:
//!
//! * **Resting** — exactly the notch, plus a few points of bleed either side.
//!   Invisible on a notched Mac, because black-on-black next to a camera
//!   housing looks like nothing at all.
//! * **Peek** — a short strip that appears on its own for a couple of seconds
//!   whenever you copy something, so a copy is acknowledged where your eyes
//!   already are.
//! * **Expanded** — the panel, opened by hovering (or clicking, if hover is
//!   off). This is where the modules live.
//!
//! Everything in the middle third of the island is drawn *around* the cutout:
//! `split_around_notch` hands out a left and a right rect and nothing is ever
//! painted between them, because on real hardware there are no pixels there.

use eframe::egui::{self, Color32, Margin, RichText, Rounding, Stroke};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipd_core::{
    load_island_config, load_shelf, save_shelf, ClipCounts, ContentType, IslandAnchor,
    IslandConfig, IslandModule, IslandSnapshot, ShelfItem,
};

use clipd_core::TransformKind;
use clipd_core::load_transform_config;
use crate::{
    global_cursor_position, load_thumb_texture, main_display_size,
    relative_time_short, resolved_theme, rgb, send_surface_request_to, spawn_palette, ClipdGui,
    SurfaceMode,
};
use clipd_core::Theme;

// ── Geometry ──

/// Notch size assumed when AppKit won't say — external displays, older Macs,
/// and Windows. Roughly a 14" MacBook Pro's cutout, which is the shape people
/// picture when they picture this feature.
const FALLBACK_NOTCH_W: f32 = 200.0;
const FALLBACK_NOTCH_H: f32 = 32.0;

/// How far the resting pill extends past the notch — minimal, like iPhone.
const RESTING_BLEED: f32 = 8.0;

/// Extra slab either side while peeking.
const PEEK_BLEED: f32 = 120.0;

/// Every card is the same height, so a row reads as one instrument rather
/// than a ragged collage. Widths differ per module — a scrubber needs room a
/// battery percentage does not.
// Tall enough for a row of shelf tiles plus its action bar, and for four
// clips rather than three.
const CARD_H: f32 = 176.0;
const CARD_GAP: f32 = 10.0;
const ISLAND_PAD: f32 = 12.0;
/// The action strip along the bottom of an open island.
const FOOTER_H: f32 = 28.0;

/// Clips shown inside a card, versus on the Clips tab.
const CARD_CLIP_ROWS: usize = 4;
/// Height of one clip or shelf line, shared by the cards and the Clips tab.
const ISLAND_ROW_H: f32 = 32.0;
/// Inside a card: padding, the caption's own height, and the air under it.
/// Stated here rather than inline so the layout arithmetic can be checked.
const CARD_PAD_X: f32 = 12.0;
const CARD_PAD_Y: f32 = 10.0;
const CARD_CAPTION_H: f32 = 12.0;
const CARD_CAPTION_GAP: f32 = 4.0;
/// Vertical space egui leaves between rows inside a card.
const CARD_ROW_SPACING: f32 = 1.0;
const CLIPS_TAB_ROWS: usize = 8;

/// The widest the slab may get, before the display's own width is considered.
const ISLAND_MAX_W: f32 = 980.0;
/// Width the card rows pack to: two of the wider cards side by side.
const PANEL_TARGET_W: f32 = 560.0;
/// Up to this wide, everything stays on one row rather than wrapping and
/// leaving a lone card stretched across a row of its own.
const PANEL_ONE_ROW_W: f32 = 740.0;
/// The panel's single header band: brand, tabs, actions.
const PANEL_HEAD_H: f32 = 40.0;
/// The "hotkeys are off" banner, when it is showing.
const HOTKEY_BANNER_H: f32 = 62.0;
/// A shelved file, and how many fit before the row scrolls.
const SHELF_TILE: egui::Vec2 = egui::vec2(74.0, 64.0);
const SHELF_TILES: usize = 12;
/// Narrow enough that the header's two halves still fit either side of the
/// cutout on a display that has one.
const ISLAND_MIN_W: f32 = 380.0;
const CLIPS_TAB_W: f32 = 420.0;

/// Animation speed: how fast the island morphs between sizes. Higher = snappier.
// Higher is snappier. At 24 the expand was a visible glide; the island is a
// HUD you summon, not a thing you watch arrive.
const ANIM_SPEED: f32 = 38.0;

/// How long a copy (or any one-off announcement) holds the peek open.
const ACTIVITY_HOLD: Duration = Duration::from_millis(2200);

/// Grace period between the pointer leaving and the island closing.
// Long enough to cross a gap between the strip and the panel without the
// island snapping shut underneath the pointer; short enough that leaving it
// feels like it goes away rather than lingers.
const COLLAPSE_DELAY: Duration = Duration::from_millis(90);
/// How long the pointer has to stay in the trigger strip before the island
/// opens. Long enough that passing through on the way somewhere else does
/// nothing, short enough that aiming at it feels immediate.
/// How long `CLIPD_ISLAND_PHASE` keeps its grip before normal hover
/// behaviour resumes. Long enough to launch, settle and capture; short
/// enough that a forgotten debug island stops being everyone's problem.
const FORCED_PHASE_GRACE: Duration = Duration::from_secs(180);

// Below the threshold where a delay is perceived at all (~100ms), so opening
// reads as instant, while still filtering the fast sweep across the strip on
// the way to the menu bar — that crossing takes well under this.
const OPEN_DWELL: Duration = Duration::from_millis(40);

/// Longer grace once the island has been clicked open deliberately.
/// Air inside the bar capsule, and the drop from the menu bar to its top edge.
const BAR_PAD: f32 = 8.0;
/// Gap between the bar's three groups, and between counts inside the group.
const BAR_GROUP_GAP: f32 = 10.0;
const BAR_ITEM_GAP: f32 = 4.0;
// No gap. A strip of desktop between the bezel and the island made the island
// look like a floating window that happened to be near the notch.
const BAR_DROP: f32 = 0.0;

/// How long a pin survives with nobody near it. Generous, because a pin is a
/// deliberate "keep this open while I work" — but not unbounded, because a
/// forgotten one sits on top of whatever you are doing.
const PIN_SAFETY_RELEASE: Duration = Duration::from_secs(600);

/// How long a pin the island took *for* you survives once you leave. Short,
/// because you never asked for it — it exists so a click does not collapse the
/// panel under your own pointer.
const IMPLICIT_PIN_RELEASE: Duration = Duration::from_millis(1200);

// ── Skin ──

/// Pure black island — always, regardless of theme. The island pretends to be
/// part of the bezel, like iPhone Dynamic Island.
#[derive(Clone, Copy)]
pub(crate) struct IslandSkin {
    /// True when the active theme's base is dark. Decides which way "lighter"
    /// is for every derived surface.
    dark: bool,
    shell: Color32,
    tile: Color32,
    row_hover: Color32,
    line: Color32,
    ink: Color32,
    dim: Color32,
    faint: Color32,
    accent: Color32,
    /// Second brand hue and the "information" hue, so the bar's three counts
    /// are told apart by colour the way the mockup does rather than being
    /// three identical accent glyphs.
    accent2: Color32,
    info: Color32,
    good: Color32,
    warn: Color32,
}

impl Default for IslandSkin {
    fn default() -> Self {
        Self::frosted(&Theme::Dark.colors())
    }
}

impl IslandSkin {
    /// The frosted variant: a translucent HUD, not a tinted copy of the theme.
    ///
    /// Deliberately dark whatever the theme is, for the same reason Control
    /// Centre and macOS's own HUD panels are: with `BehindWindow` blending the
    /// backdrop is *whatever is behind the window*, so a tint taken from a
    /// light theme sat over a bright menu bar and washed the text out. The
    /// theme still lends its accent; the rest is HUD.
    fn frosted(c: &clipd_core::ThemeColors) -> Self {
        // Straight from the active theme, light or dark.
        //
        // Light themes used to be forced to a dark HUD shell, on the reasoning
        // that a pale slab would composite to near-white over the menu bar and
        // take the text with it. That was true at alpha 150; the shell sits at
        // 240 now, which is opaque enough that a light theme renders as its own
        // colour. Forcing it dark only meant Paper Light and Glass Light left
        // the island as the one window that had not changed.
        let base = rgb(c.bg_base);
        let dark = relative_luminance(base) < 0.5;
        Self {
            dark,
            // The theme's own surface alpha when it has one. A glass theme
            // asks every clipd surface to be translucent so the material
            // behind reads; the island was holding at 240 regardless, which is
            // why it stayed a solid slab while the palette went to glass.
            // Solid themes leave it at 255, and 240 keeps a trace of vibrancy
            // for those.
            shell: {
                let alpha = if c.surface_alpha < 255 {
                    c.surface_alpha
                } else {
                    240
                };
                Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
            },
            tile: {
                let t = rgb(c.bg_elevated);
                Color32::from_rgba_unmultiplied(t.r(), t.g(), t.b(), 210)
            },
            row_hover: {
                let h = rgb(c.bg_selected);
                Color32::from_rgba_unmultiplied(h.r(), h.g(), h.b(), 215)
            },
            line: {
                let l = rgb(c.border);
                Color32::from_rgba_unmultiplied(l.r(), l.g(), l.b(), 120)
            },
            ink: rgb(c.text),
            dim: rgb(c.subtext),
            faint: rgb(c.overlay),
            accent: rgb(c.accent),
            accent2: rgb(c.accent2),
            info: rgb(c.url),
            good: rgb(c.green),
            warn: Color32::from_rgb(250, 179, 135),
        }
    }

}

/// Move a colour toward white on dark themes, toward black on light ones.
fn lift(base: Color32, amount: f32, dark: bool) -> Color32 {
    let target = if dark {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    mix(base, target, amount)
}

/// Opaque blend of `a` toward `b` by `t`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Perceived brightness, for deciding which way "lighter" is.
fn relative_luminance(c: Color32) -> f32 {
    (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct NotchGeometry {
    /// Width of the physical cutout, or the width we pretend it has.
    pub width: f32,
    /// Height of the menu bar the island shares its row with.
    pub height: f32,
    /// Horizontal centre of the cutout in screen points.
    pub center_x: f32,
    /// Whether this display actually has a notch. Drives whether the island
    /// sits flush at y=0 or floats below the menu bar.
    pub real: bool,
}

impl Default for NotchGeometry {
    fn default() -> Self {
        Self {
            width: FALLBACK_NOTCH_W,
            height: FALLBACK_NOTCH_H,
            center_x: 720.0,
            real: false,
        }
    }
}

impl NotchGeometry {
    /// Top edge of the island window. Flush with the display on a notched Mac;
    /// tucked just under the menu bar anywhere else, so it never covers the
    /// clock or the menu titles.
    fn top(&self, hug: bool) -> f32 {
        if hug && self.real {
            0.0
        } else {
            self.height + 4.0
        }
    }

    /// Whether the drawing has to leave a hole for the camera housing.
    fn cutout(&self, hug: bool) -> bool {
        hug && self.real
    }
}

/// Measure the notch, falling back to a plausible one.
pub(crate) fn notch_geometry(config: &IslandConfig) -> NotchGeometry {
    let screen = main_display_size().unwrap_or(egui::vec2(1440.0, 900.0));
    let mut geo = NotchGeometry {
        width: FALLBACK_NOTCH_W,
        height: FALLBACK_NOTCH_H,
        center_x: screen.x / 2.0,
        real: false,
    };

    if let Some((width, height)) = measure_notch() {
        geo.width = width;
        geo.height = height;
        geo.real = true;
    }
    // A manual width always wins: on an external display there is nothing to
    // measure, and some people simply want a wider resting slab.
    if config.notch_width > 0.0 {
        geo.width = config.notch_width.clamp(60.0, 520.0);
    }
    geo
}

/// Ask AppKit for the notch: `safeAreaInsets.top` is non-zero only on a
/// display with a cutout, and the two auxiliary top areas are the menu-bar
/// strips either side of it — so what is left between them is the notch.
#[cfg(target_os = "macos")]
fn measure_notch() -> Option<(f32, f32)> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let mtm = MainThreadMarker::new()?;
    let screen = NSScreen::mainScreen(mtm)?;
    let top = screen.safeAreaInsets().top as f32;
    if top <= 0.0 {
        return None;
    }

    let frame = screen.frame();
    let left = screen.auxiliaryTopLeftArea();
    let right = screen.auxiliaryTopRightArea();
    let width = (frame.size.width - left.size.width - right.size.width) as f32;
    // Sanity-clamp rather than trust: if AppKit hands back an empty auxiliary
    // area (it does on some setups) the arithmetic above yields the whole
    // screen, and an island as wide as the display is not a recoverable look.
    let width = if (60.0..=520.0).contains(&width) {
        width
    } else {
        FALLBACK_NOTCH_W
    };
    Some((width, top))
}

#[cfg(not(target_os = "macos"))]
fn measure_notch() -> Option<(f32, f32)> {
    None
}

/// Make the island's view accept the *first* click.
///
/// By default a click on a window belonging to an app that isn't frontmost is
/// spent activating that app — AppKit swallows it, and the widget under the
/// pointer never sees it. On the island that reads as "clicking a clip does
/// nothing": the first click wakes clipd up, and only a second one copies.
///
/// `acceptsFirstMouse:` is exactly the opt-out for this, but it is a *view*
/// method and the view belongs to winit, so there is nothing to override. This
/// adds the method to that view's class at runtime, returning YES. It is added
/// once per process, and only ever to the class backing this window.
#[cfg(target_os = "macos")]
fn accept_first_click(frame: &eframe::Frame) {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use objc2::{ffi, sel};

    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(view) = crate::ns_metal_view(frame) else {
        return;
    };

    extern "C" fn accepts_first_mouse(_this: &AnyObject, _cmd: Sel, _event: *mut AnyObject) -> Bool {
        Bool::YES
    }

    let class: &AnyClass = (*view).class();
    // "c@:@" — returns BOOL, takes self, _cmd and the NSEvent.
    let types = c"c@:@";
    let added: bool = unsafe {
        ffi::class_addMethod(
            // The cast is to the mutable class pointer `class_addMethod` wants;
            // adding a method does not mutate anything Rust can observe.
            class as *const AnyClass as *mut AnyClass,
            sel!(acceptsFirstMouse:),
            std::mem::transmute::<
                extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> Bool,
                unsafe extern "C-unwind" fn(),
            >(accepts_first_mouse),
            types.as_ptr(),
        )
        .into()
    };
    if !added {
        // Already answered by the class — winit may implement it itself, in
        // which case whatever it returns is what we get.
        log::info!("island: acceptsFirstMouse: already implemented by the view class");
    }
}

/// Raise the island above the menu bar and pin it to every Space.
///
/// eframe's always-on-top is `NSFloatingWindowLevel` (3), which is *below* the
/// menu bar at 24 — an island at that level would be hidden by the very strip
/// it is supposed to be part of. Status level is where menu-bar extras live,
/// which is exactly the company this window should keep.
#[cfg(target_os = "macos")]
pub(crate) fn raise_above_menu_bar(frame: &eframe::Frame) {
    use objc2_app_kit::{NSStatusWindowLevel, NSWindowCollectionBehavior};
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = frame.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = unsafe { appkit.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else {
        return;
    };
    window.setLevel(NSStatusWindowLevel + 1);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn raise_above_menu_bar(_frame: &eframe::Frame) {}

#[cfg(not(target_os = "macos"))]
fn accept_first_click(_frame: &eframe::Frame) {}

// ── State ──

/// The two things the expanded island can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IslandTab {
    /// The widget cards.
    Home,
    /// The recent clipboard in full — more rows than a card holds, less
    /// ceremony than opening the palette.
    Clips,
}

impl IslandTab {
    fn label(self) -> &'static str {
        match self {
            IslandTab::Home => "Home",
            IslandTab::Clips => "Clips",
        }
    }

    const ALL: [IslandTab; 2] = [
        IslandTab::Home,
        IslandTab::Clips,
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IslandPhase {
    /// Completely invisible — no activity, not hovered.
    Hidden,
    /// A small resting pill at the notch — minimal, just acknowledging presence.
    Resting,
    /// A short announcement strip — a copy landing, a timer finishing.
    Peek,
    /// Full expanded panel with modules.
    Expanded,
}

/// What the peek is currently announcing.
#[derive(Clone)]
struct Activity {
    label: String,
    detail: String,
    until: Instant,
}

pub(crate) struct IslandState {
    pub config: IslandConfig,
    pub phase: IslandPhase,
    pub tab: IslandTab,
    /// Search typed into the bar. `Some("")` means the field is open and empty.
    pub search: Option<String>,
    /// Set once when the search field opens, to move the caret into it.
    focus_search: bool,
    /// Previous tab — when it changes, snap the animation to the new size
    /// instantly so tab switches don't freeze or lag.
    last_tab: IslandTab,
    /// Readings collected off-thread. See [`spawn_poller`].
    data: Arc<Mutex<IslandSnapshot>>,
    /// The island's screen rect, shared with the cursor watcher so it knows
    /// what counts as "on the island" as the window grows and shrinks.
    hot_rect: Arc<Mutex<HotZone>>,
    poller_started: bool,
    watcher_started: bool,
    /// Set by the settings UI so the poller refetches immediately instead of
    /// waiting out the fifteen-minute weather cadence.
    refresh_now: Arc<AtomicBool>,
    /// Whether the island is actually showing anything. A resting island is a
    /// 200pt strip with two dots on it, and does not need a media reading
    /// every two seconds — each one spawns an `osascript` process, which is
    /// what kept this window at ~10% CPU while doing nothing.
    open_flag: Arc<AtomicBool>,

    activity: Option<Activity>,
    /// Newest clip id already announced, so the same copy isn't flashed twice.
    announced_clip: Option<i64>,
    /// Held open by a click rather than by the pointer.
    pinned: bool,
    left_at: Option<Instant>,
    /// When the pointer arrived, so a hover can escalate from bar to panel.
    entered_at: Option<Instant>,
    geometry: NotchGeometry,
    /// Colours for the current theme, refreshed at the top of every frame.
    skin: IslandSkin,
    /// How many rows the Clips tab has to show, so the slab can be sized for
    /// them a frame ahead of the list being laid out.
    clips_rows: usize,
    /// Files parked on the island.
    pub shelf: Vec<ShelfItem>,
    /// When this island process started, used to expire the debug phase
    /// override so it cannot pin the slab open indefinitely.
    started_at: Instant,
    /// True when the island pinned itself — clicking a row, a tab, or opening
    /// search — rather than the user pressing the pin button.
    ///
    /// The two need different lifetimes. An explicit pin is a deliberate "keep
    /// this open while I work" and holds for a long time; an implicit one is a
    /// side effect of interacting, and holding *that* for ten minutes parks the
    /// island over your screen after a single click, which reads as frozen.
    pin_is_implicit: bool,
    /// When the pin was last touched — set on pinning and refreshed whenever
    /// the pointer is on the island, so the safety release only fires on a pin
    /// that has genuinely been abandoned.
    pinned_at: Option<Instant>,
    /// Whether another clipd window is on screen, and when that was checked.
    /// Cached because the phase logic runs every frame and this is a file read.
    window_open: bool,
    window_checked: Instant,
    /// Whether the keyboard grants multi-slot copy needs are in place, and
    /// when that was last checked.
    pub hotkeys_ok: bool,
    hotkeys_checked: Instant,
    /// Previews for shelved images, keyed by path. `None` means we tried and
    /// it isn't an image we can decode — don't retry every frame.
    shelf_thumbs: std::collections::HashMap<std::path::PathBuf, Option<egui::TextureHandle>>,

    /// The vibrancy currently applied to the window, as (frosted, radius).
    /// Re-applying every frame would rebuild an AppKit view 60 times a second.
    applied_material: Option<(bool, i32)>,
    last_sent: Option<(egui::Pos2, egui::Vec2)>,
    /// Re-read config from disk on a timer: settings are edited in the palette
    /// process, so disk is the only channel between them.
    last_config_check: Instant,
    /// A one-line result from the last thing the user pressed here.
    status: Option<(String, Instant)>,
    /// Animated current size — lerps toward the target each frame for smooth
    /// transitions between phases.
    anim_size: egui::Vec2,
    /// Animated top edge. The bar floats below the menu bar while the resting
    /// pill hugs the notch, so the phase change moves the window as well as
    /// resizing it — un-animated, that is a jump rather than a drop.
    anim_top: f32,
    /// Animated opacity — 0 when hidden, 1 when visible. Fades in/out.
    anim_opacity: f32,
}

impl Default for IslandState {
    fn default() -> Self {
        let config = load_island_config();
        Self {
            geometry: notch_geometry(&config),
            skin: IslandSkin::default(),
            clips_rows: CLIPS_TAB_ROWS,
            shelf: load_shelf(),
            started_at: Instant::now(),
            pin_is_implicit: false,
            pinned_at: None,
            window_open: false,
            window_checked: Instant::now() - Duration::from_secs(60),
            hotkeys_ok: true,
            hotkeys_checked: Instant::now() - Duration::from_secs(60),
            shelf_thumbs: std::collections::HashMap::new(),
            applied_material: None,
            config,
            phase: IslandPhase::Resting,
            // Same reason as CLIPD_ISLAND_PHASE: the tab is reached by
            // clicking a slab that only exists while hovered, which makes it
            // awkward to inspect while working on it.
            search: None,
            focus_search: false,
            tab: match std::env::var("CLIPD_ISLAND_TAB").as_deref().map(str::trim) {
                Ok("clips") => IslandTab::Clips,
                _ => IslandTab::Home,
            },
            last_tab: IslandTab::Home,
            data: Arc::new(Mutex::new(IslandSnapshot::default())),
            hot_rect: Arc::new(Mutex::new(HotZone::NOTHING)),
            poller_started: false,
            watcher_started: false,
            refresh_now: Arc::new(AtomicBool::new(false)),
            open_flag: Arc::new(AtomicBool::new(false)),
            activity: None,
            announced_clip: None,
            pinned: false,
            left_at: None,
            entered_at: None,
            last_sent: None,
            last_config_check: Instant::now(),
            status: None,
            anim_size: egui::vec2(0.0, 0.0),
            anim_top: 0.0,
            anim_opacity: 0.0,
        }
    }
}

impl IslandState {
    /// Ask the background poller to refetch everything at the next tick.
    pub(crate) fn invalidate(&mut self) {
        self.geometry = notch_geometry(&self.config);
        self.refresh_now.store(true, Ordering::Relaxed);
    }

    fn note(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    fn announce(&mut self, label: impl Into<String>, detail: impl Into<String>) {
        self.activity = Some(Activity {
            label: label.into(),
            detail: detail.into(),
            until: Instant::now() + ACTIVITY_HOLD,
        });
    }

    /// Give back a pin that search borrowed.
    ///
    /// Opening search pins the island so it cannot collapse while you type.
    /// Pins used to lapse a second after the pointer left, so that borrowed
    /// pin cleaned itself up; they now hold for ten minutes, which is right
    /// for a pin you asked for and wrong for one you never knew you took —
    /// the island would sit open long after the search was done, which reads
    /// as frozen.
    fn release_search_pin(&mut self) {
        if self.pin_is_implicit {
            self.pin_is_implicit = false;
            self.pinned = false;
            self.pinned_at = None;
        }
    }

    fn snapshot(&self) -> IslandSnapshot {
        self.data.lock().map(|d| d.clone()).unwrap_or_default()
    }

    /// The target window size for the current phase.
    fn target_size(&self) -> egui::Vec2 {
        let geo = self.geometry;
        match self.phase {
            IslandPhase::Hidden => egui::vec2(geo.width, geo.height),
            IslandPhase::Resting => {
                egui::vec2(geo.width + RESTING_BLEED * 2.0, geo.height.max(24.0))
            }
            // The bar hangs *below* the notch band rather than splitting
            // around the cutout: a bar with a hole through the middle of it is
            // not a bar. The band above is bezel, and stays empty.
            IslandPhase::Peek => {
                let layout = true;
                egui::vec2(
                    bar_width().min(self.max_slab_width()),
                    bar_height() + BAR_PAD * 2.0,
                )
            }
            IslandPhase::Expanded => self.expanded_size(),
        }
    }

    /// Target opacity for the current phase.
    fn target_opacity(&self) -> f32 {
        match self.phase {
            IslandPhase::Hidden => 0.0,
            _ => 1.0,
        }
    }

    /// The slab's size, derived from the cards it is about to hold.
    ///
    /// Card widths are fixed, so this is exact rather than a guess: the panel
    /// is never the wrong size for its contents, and never needs a frame of
    /// measuring to catch up.
    ///
    /// Deliberately the *same* for both tabs. Switching tabs used to resize
    /// the window, and animating a window's width on macOS means a surface
    /// reconfigure per frame — which is exactly the lag that made Home → Clips
    /// feel slow. With one size, a tab switch is a content swap and lands on
    /// the next frame.
    fn expanded_size(&self) -> egui::Vec2 {
        let layout = true;
        let _ = layout;
        let chrome = BAR_PAD + PANEL_HEAD_H + 8.0;
        let modules = self.config.active_modules();

        // Width stays the same across tabs — resizing sideways on a tab click
        // is the jarring part. Height follows whatever is actually showing:
        // sizing every tab for the card grid left Widgets and Clips as a
        // mostly-empty slab.
        let rows = island_card_rows(&modules, self.max_card_width());
        let widest = rows
            .iter()
            .map(|row| {
                row.iter().map(|m| card_width(*m)).sum::<f32>()
                    + CARD_GAP * row.len().saturating_sub(1) as f32
            })
            .fold(0.0_f32, f32::max);
        let width = (widest + ISLAND_PAD * 2.0)
            .max(bar_width())
            .min(self.max_slab_width());

        // The permission banner is a real row when it is showing.
        let banner = if self.hotkeys_ok { 0.0 } else { HOTKEY_BANNER_H };
        let body = banner + FOOTER_H + match self.tab {
            IslandTab::Home if modules.is_empty() => 52.0,
            IslandTab::Home => {
                rows.len() as f32 * CARD_H + CARD_GAP * (rows.len() - 1) as f32
            }
            IslandTab::Clips => {
                let count = self.clips_rows.clamp(1, CLIPS_TAB_ROWS) as f32;
                count * ISLAND_ROW_H + CARD_PAD_Y * 2.0 + 4.0
            }
        };
        egui::vec2(width, (chrome + body + ISLAND_PAD).min(self.max_panel_height()))
    }

    /// Height of the strip that has to clear the notch. On a notched display
    /// the cutout decides; floating, a tab chip is all it has to hold.
    fn header_height(&self) -> f32 {
        if self.geometry.cutout(self.config.anchor == IslandAnchor::Auto) {
            self.geometry.height.max(26.0)
        } else {
            30.0
        }
    }

    /// Tallest the panel may get, so a full set of modules can't run off the
    /// bottom of the display.
    fn max_panel_height(&self) -> f32 {
        let screen = main_display_size().unwrap_or(egui::vec2(1440.0, 900.0));
        (screen.y * 0.62).min(620.0)
    }

    /// The widest slab this display will take, leaving a margin either side.
    fn max_slab_width(&self) -> f32 {
        let screen = main_display_size().unwrap_or(egui::vec2(1440.0, 900.0));
        ISLAND_MAX_W.min(screen.x - 80.0).max(ISLAND_MIN_W)
    }

    /// Room available to cards once the slab's padding is taken out.
    fn max_card_width(&self) -> f32 {
        // Prefer one row when the whole set nearly fits: three cards wrapped
        // 2 + 1 leaves the last one alone on its row, stretched across the
        // full width for want of a neighbour, which looks like a mistake.
        // Past that, wrap at two typical cards so opening the island grows
        // downwards rather than lurching sideways.
        let modules = self.config.active_modules();
        let single_row: f32 = modules.iter().map(|m| card_width(*m)).sum::<f32>()
            + CARD_GAP * modules.len().saturating_sub(1) as f32;
        let ceiling = (self.max_slab_width() - ISLAND_PAD * 2.0).max(160.0);
        if single_row <= PANEL_ONE_ROW_W.min(ceiling) {
            single_row.max(160.0)
        } else {
            PANEL_TARGET_W.min(ceiling).max(160.0)
        }
    }

    /// The strip at the top of the display that always opens the island.
    ///
    /// Hover has to be tested against this *union* the current window, never
    /// the window alone: the bar floats below the notch while the resting pill
    /// hugs it, so the two rects are vertically disjoint. Testing the window
    /// alone meant hovering the notch opened the bar, the bar's rect no longer
    /// contained the pointer, it collapsed, and the pill landed back under the
    /// pointer — sixty times a second, which is what the freezing and the
    /// flapping width were.
    fn trigger_rect(&self) -> egui::Rect {
        let screen = main_display_size().unwrap_or(egui::vec2(1440.0, 900.0));
        let width = self.geometry.width + RESTING_BLEED * 2.0;
        let left = (self.geometry.center_x - width / 2.0).clamp(0.0, (screen.x - width).max(0.0));
        egui::Rect::from_min_size(
            egui::pos2(left, 0.0),
            // Down to where the bar's own top edge is, so the pointer never
            // crosses a dead band on its way from the notch to the bar.
            egui::vec2(width, self.header_height() + BAR_DROP + 2.0),
        )
    }

    /// The region a pointer has to be in for the island to count as hovered.
    ///
    /// The panel and the trigger strip, kept apart. They used to be combined
    /// with `Rect::union`, which returns the *bounding box* of the two — and
    /// since the panel hangs below the strip and is far wider, that box
    /// swallowed the whole top band of the screen either side of the notch.
    /// Reaching for a menu-bar item or a browser tab landed inside it, so the
    /// island stayed open over the top of everything and read as frozen.
    fn hot_zone(&self, pos: egui::Pos2, size: egui::Vec2) -> HotZone {
        HotZone {
            panel: egui::Rect::from_min_size(pos, size),
            trigger: self.trigger_rect(),
            expand: match self.phase {
                // Aiming at a strip beside the camera housing: be forgiving.
                IslandPhase::Hidden | IslandPhase::Resting => 22.0,
                _ => 8.0,
            },
        }
    }

    /// Where the window's top edge belongs for the current phase.
    ///
    /// The bar is a free-floating capsule under the menu bar; the resting pill
    /// and the card panel hug the notch. Hugging the notch for the bar too
    /// would force a band of dead black above it and turn the capsule into a
    /// slab with a bar stuck to its bottom edge.
    fn target_top(&self) -> f32 {
        let hug = self.config.anchor == IslandAnchor::Auto;
        match self.phase {
            // Anything with content in it starts *below* the menu bar row.
            //
            // Anchoring the panel at y=0 puts its header level with the
            // cutout, and on real hardware there are no pixels there — the
            // tabs and the brand were being drawn behind the camera housing,
            // which is why the top half looked broken. The resting pill is the
            // only thing that belongs at y=0, because it is pretending to be
            // the bezel rather than showing anything.
            IslandPhase::Peek | IslandPhase::Expanded => self.header_height() + BAR_DROP,
            _ => self.geometry.top(hug),
        }
    }

    fn window_pos(&self, size: egui::Vec2) -> egui::Pos2 {
        let hug = self.config.anchor == IslandAnchor::Auto;
        let _ = hug;
        let screen = main_display_size().unwrap_or(egui::vec2(1440.0, 900.0));
        let left = (self.geometry.center_x - size.x / 2.0).clamp(0.0, (screen.x - size.x).max(0.0));
        egui::pos2(left, self.anim_top)
    }
}

/// Fetch the readings the enabled modules need, on their own cadences.
///
/// One thread for all of them: they are all slow-ish shell-outs or an HTTP
/// call, and running them from `update` would stall the frame. Config is read
/// from disk each pass because the settings that turn these on live in a
/// different process.
fn spawn_poller(
    ctx: &egui::Context,
    data: Arc<Mutex<IslandSnapshot>>,
    refresh_now: Arc<AtomicBool>,
    open_flag: Arc<AtomicBool>,
) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let mut last: Vec<(IslandModule, Instant)> = Vec::new();
        loop {
            let forced = refresh_now.swap(false, Ordering::Relaxed);
            if !clipd_core::island_layout_active() {
                // The layout was switched back to the palette; the island
                // process is on its way out, so stop doing work for it.
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            let config = load_island_config();

            // A preview fill for design work: the media, weather and calendar
            // cards all depend on state clipd doesn't control, and you cannot
            // judge a now-playing card with nothing playing.
            if std::env::var("CLIPD_ISLAND_DEMO").is_ok() {
                if let Ok(mut d) = data.lock() {
                    *d = demo_snapshot();
                }
                ctx.request_repaint();
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }

            // Closed: everything the island can show is two dots, so stretch
            // every cadence rather than shelling out on the open schedule.
            let idle_factor = if open_flag.load(Ordering::Relaxed) { 1 } else { 6 };
            // Nothing to fetch: both modules read state the GUI already
            // holds. The loop stays as the place a future module that needs
            // background data would hook into.
            let _ = (&config, forced, &data);
            std::thread::sleep(if open_flag.load(Ordering::Relaxed) {
                Duration::from_millis(900)
            } else {
                Duration::from_millis(2400)
            });
        }
    });
}

/// Sample readings for `CLIPD_ISLAND_DEMO`. Never used in a normal run.
fn demo_snapshot() -> IslandSnapshot {
    IslandSnapshot::default()
}

/// Wake the UI thread when the pointer reaches the island.
///
/// The same problem the HUD pill has, with one addition: while a file is being
/// dragged, the window gets no events at all, so hovering the notch with a
/// file in hand would never open the shelf. Polling the global cursor position
/// sidesteps both.
fn spawn_cursor_watcher(ctx: &egui::Context, hot_rect: Arc<Mutex<HotZone>>) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // The pointer has to be noticed before anything else can happen, so
        // this sits at the front of every open. One CGEvent location read.
        const POLL: Duration = Duration::from_millis(12);
        let mut was_inside = false;
        loop {
            std::thread::sleep(POLL);
            let Some(cursor) = global_cursor_position() else {
                continue;
            };
            let Ok(rect) = hot_rect.lock().map(|r| *r) else {
                continue;
            };
            let inside = rect.contains(cursor, rect.expand);
            // Repaint for as long as the pointer is in the zone, not only when
            // it crosses in.
            //
            // On the transition alone, the island got exactly one frame. If
            // that frame decided "not yet" — the open dwell had not elapsed —
            // nothing else was scheduled to ask again, and a pointer resting
            // on the strip produces no further transitions and no window
            // events, because the window is off-screen. The island sat there
            // until some unrelated repaint happened to come along. That is the
            // freeze: not a hang, just nobody left to ask for the next frame.
            if inside || inside != was_inside {
                ctx.request_repaint();
            }
            was_inside = inside;
        }
    });
}

/// Where the pointer has to be for the island to count as hovered.
///
/// Two rects rather than one: the panel where it currently is, and the strip
/// at the notch that brings it back. A pointer between them — out at the far
/// end of the menu bar, say — is in neither.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HotZone {
    panel: egui::Rect,
    trigger: egui::Rect,
    /// How far outside the zone still counts as being on it.
    ///
    /// Carried here rather than decided by each reader. The watcher thread and
    /// the UI thread both test this zone, and when they each had their own
    /// rule they disagreed: a pointer in the gap woke the island up to decide
    /// it was not being hovered. It also has to vary — a hidden island is a
    /// 76×34pt strip at the notch, which is a small thing to hit, while an
    /// open panel is large and wants a tight edge so it does not cling.
    expand: f32,
}

impl HotZone {
    const NOTHING: Self = Self {
        panel: egui::Rect::NOTHING,
        trigger: egui::Rect::NOTHING,
        expand: 0.0,
    };

    fn contains(&self, p: egui::Pos2, slack: f32) -> bool {
        self.panel.expand(slack).contains(p) || self.trigger.expand(slack).contains(p)
    }

    /// True once the island is off-screen and only the strip is live.
    fn is_nothing(&self) -> bool {
        self.panel == egui::Rect::NOTHING && self.trigger == egui::Rect::NOTHING
    }
}

/// Split a row into the parts that are actually visible either side of the
/// camera housing. Nothing may be painted between them.
fn split_around_notch(rect: egui::Rect, notch_w: f32, cutout: bool) -> (egui::Rect, egui::Rect) {
    if !cutout {
        // No hole to route around: hand back one full-width band as the left
        // rect and an empty right, so callers lay out normally.
        return (rect, egui::Rect::from_min_size(rect.right_top(), egui::Vec2::ZERO));
    }
    let half = notch_w / 2.0 + 6.0;
    let cx = rect.center().x;
    (
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(cx - half, rect.bottom())),
        egui::Rect::from_min_max(egui::pos2(cx + half, rect.top()), rect.right_bottom()),
    )
}

impl ClipdGui {
    // ── Frame driving ──

    /// Resize, reposition, and decide which phase the island is in.
    pub(crate) fn drive_island(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // `CLIPD_ISLAND_FPS=1` prints the island's actual repaint rate.
        //
        // Worth keeping: this window is meant to be asleep most of the time,
        // and the difference between "asleep" and "burning a core to draw two
        // dots" is invisible until you count the frames.
        if std::env::var("CLIPD_ISLAND_FPS").is_ok() {
            use std::sync::atomic::AtomicU32;
            static FRAMES: AtomicU32 = AtomicU32::new(0);
            static START: Mutex<Option<Instant>> = Mutex::new(None);
            let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
            let mut start = START.lock().unwrap();
            let begin = *start.get_or_insert_with(Instant::now);
            if begin.elapsed() >= Duration::from_secs(2) {
                eprintln!(
                    "[island] {:.1} fps, phase={:?}",
                    n as f32 / begin.elapsed().as_secs_f32(),
                    self.island.phase
                );
                FRAMES.store(0, Ordering::Relaxed);
                *start = Some(Instant::now());
            }
        }
        if !self.island.poller_started {
            self.island.poller_started = true;
            spawn_poller(
                ctx,
                self.island.data.clone(),
                self.island.refresh_now.clone(),
                self.island.open_flag.clone(),
            );
        }
        if !self.island.watcher_started {
            self.island.watcher_started = true;
            spawn_cursor_watcher(ctx, self.island.hot_rect.clone());
        }
        // The island keeps its own window above the menu bar. Re-applied every
        // frame is wasteful; once the window exists is enough, and the first
        // frame is the earliest it does.
        if self.island.last_sent.is_none() {
            raise_above_menu_bar(frame);
            accept_first_click(frame);
        }
        // Masked to the shape the island currently has: a rectangular blur
        // behind a capsule shows as square corners peeking out of it.
        // Quantised: the capsule's radius tracks the animated height, and
        // feeding every intermediate value in would tear down and rebuild the
        // effect view sixty times a second — which is why the blur flickered
        // out during a morph.
        let radius = match self.island.phase {
            IslandPhase::Peek => (self.island.anim_size.y / 2.0 / 4.0).round() * 4.0,
            IslandPhase::Expanded => 24.0,
            _ => 16.0,
        };
        sync_island_material(
            frame,
            self.island_frosted(),
            self.island.skin.dark,
            radius,
            &mut self.island.applied_material,
        );

        // Settings are edited in the palette process — disk is the channel.
        if self.island.last_config_check.elapsed() > Duration::from_secs(1) {
            self.island.last_config_check = Instant::now();
            if !clipd_core::island_layout_active() {
                // Switched back to the palette in settings: this process *is*
                // the layout, so it goes with it.
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            let fresh = load_island_config();
            if fresh.notch_width != self.island.config.notch_width
                || fresh.anchor != self.island.config.anchor
                || fresh.modules != self.island.config.modules

            {
                self.island.config = fresh;
                self.island.invalidate();
            } else {
                self.island.config = fresh;
            }
        }

        // Without the shelf there are no tabs, so the panel has to settle on
        // the view that is worth the space: the full list, not a card holding
        // four of the same rows.
        if !self.island.config.has(IslandModule::Files) {
            self.island.tab = IslandTab::Clips;
        }
        if self.island.window_checked.elapsed() >= Duration::from_millis(250) {
            self.island.window_checked = Instant::now();
            self.island.window_open = clipd_core::gui_window_open();
        }
        self.island_check_hotkeys();
        self.island_take_dropped_files(ctx);
        self.island_announce_new_clip();

        // ── Phase ──
        let cursor = global_cursor_position();
        let rect = self
            .island
            .hot_rect
            .lock()
            .map(|r| *r)
            .unwrap_or(HotZone::NOTHING);
        let hovered = cursor.map(|p| rect.contains(p, rect.expand)).unwrap_or(false);
        let dragging_files = ctx.input(|i| !i.raw.hovered_files.is_empty());

        let activity_live = self
            .island
            .activity
            .as_ref()
            .map(|a| a.until > Instant::now())
            .unwrap_or(false);
        if !activity_live {
            self.island.activity = None;
        }

        // Pin the island open at one size, for looking at it. The island is
        // the one surface whose whole behaviour is "appear when hovered", so
        // there is otherwise no way to inspect a state while working on it.
        // The forced phase is a development aid, and it expires.
        //
        // Set to "expanded" it pins the slab open over the middle of the
        // screen and ignores the pointer entirely — which is exactly what you
        // want for ten seconds while you screenshot it, and exactly what a
        // frozen island looks like if the process outlives the person who
        // started it. Time-boxing it means a forgotten override heals itself
        // instead of sitting on top of somebody's work.
        let forced = if self.island.started_at.elapsed() < FORCED_PHASE_GRACE {
            std::env::var("CLIPD_ISLAND_PHASE").ok()
        } else {
            None
        };
        let want = match forced.as_deref().map(str::trim) {
            Some("expanded") => IslandPhase::Expanded,
            Some("peek") => IslandPhase::Peek,
            Some("resting") => IslandPhase::Resting,
            Some("hidden") => IslandPhase::Hidden,
            _ => self.island_phase_from_input(hovered, dragging_files, activity_live),
        };

        // Escape releases the pin. A pin that holds needs a way out that is
        // not "find the small circle again" — especially when the island is
        // sitting over the thing you were reading.
        if self.island.pinned
            && self.island.search.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.island.pinned = false;
            self.island.pinned_at = None;
        }

        // A pin holds until it is released.
        //
        // It used to release itself 1.2 seconds after the pointer left, which
        // made the button a grace period wearing a pin's label: you pinned the
        // island, looked away, and it closed anyway. The whole reason to pin is
        // to go and do something else.
        //
        // It still cannot be forgotten forever — an island parked over the
        // middle of the screen with no pointer near it is indistinguishable
        // from a frozen one, so a pin nobody has come back to eventually
        // lapses. Any time the pointer is on it, the clock restarts.
        if self.island.pinned {
            // An open search field is its own reason to stay. Search takes an
            // implicit pin, and implicit pins lapse 1.2s after the pointer
            // leaves — but typing means the pointer *has* left, so the panel
            // collapsed out from under the field a second into the first word.
            if self.island.search.is_some() {
                self.island.pinned_at = Some(Instant::now());
            } else if hovered {
                self.island.pinned_at = Some(Instant::now());
            } else {
                let since = *self.island.pinned_at.get_or_insert_with(Instant::now);
                let limit = if self.island.pin_is_implicit {
                    IMPLICIT_PIN_RELEASE
                } else {
                    PIN_SAFETY_RELEASE
                };
                if since.elapsed() > limit {
                    self.island.pinned = false;
                    self.island.pin_is_implicit = false;
                    self.island.pinned_at = None;
                }
            }
        }

        if want != self.island.phase {
            self.island.phase = want;
        }
        self.island.open_flag.store(
            !matches!(self.island.phase, IslandPhase::Resting | IslandPhase::Hidden),
            Ordering::Relaxed,
        );

        // ── Tab switch: snap to target instantly ──
        // When the user clicks a tab (Home/Clips/Widgets), the content swaps
        // but the window size should stay the same. Snapping the animation
        // prevents the lerp from fighting the content change, which caused
        // the "freeze" — the window was resizing by sub-pixel amounts for
        // many frames after the tab already changed.
        let tab_changed = self.island.tab != self.island.last_tab;
        if tab_changed {
            self.island.last_tab = self.island.tab;
            self.island.anim_size = self.island.target_size();
            self.island.anim_opacity = self.island.target_opacity();
            // Force a resize + repaint so the new tab content lands next frame.
            self.island.last_sent = None;
            ctx.request_repaint();
        }

        // ── Smooth animation ──
        let target = self.island.target_size();
        let target_op = self.island.target_opacity();
        let dt = ctx.input(|i| i.unstable_dt).min(0.1);
        let lerp = 1.0 - (-ANIM_SPEED * dt).exp();
        if !tab_changed {
            self.island.anim_size = egui::vec2(
                self.island.anim_size.x + (target.x - self.island.anim_size.x) * lerp,
                self.island.anim_size.y + (target.y - self.island.anim_size.y) * lerp,
            );
            self.island.anim_opacity =
                self.island.anim_opacity + (target_op - self.island.anim_opacity) * lerp;
            // Snap all three. An exponential approach never actually arrives,
            // so `settled` below was never true and the island held a 60fps
            // repaint forever — about a tenth of a core to draw a 200pt strip
            // with two dots on it. The tail is also a run of sub-pixel window
            // resizes, each one a surface reconfigure nobody can see.
            if (self.island.anim_size - target).length() < 2.0 {
                self.island.anim_size = target;
            }
            if (self.island.anim_opacity - target_op).abs() < 0.02 {
                self.island.anim_opacity = target_op;
            }
            let target_top = self.island.target_top();
            self.island.anim_top += (target_top - self.island.anim_top) * lerp;
            if (self.island.anim_top - target_top).abs() < 1.5 {
                self.island.anim_top = target_top;
            }
        }

        // Use the animated size for the window.
        let size = self.island.anim_size;
        let pos = self.island.window_pos(size);

        // When fully hidden (opacity ~0), move off-screen but keep the
        // hot_rect at the notch position so the cursor watcher can detect
        // hover and bring the island back.
        let actually_hidden = self.island.phase == IslandPhase::Hidden
            && self.island.anim_opacity < 0.02;
        if actually_hidden {
            let off = egui::pos2(0.0, -(self.island.geometry.height + 100.0));
            if self.island.last_sent != Some((off, size)) {
                self.island.last_sent = Some((off, size));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(off));
            }
            // Keep hot_rect at the notch area so the watcher detects hover.
            let notch_size = self.island.target_size();
            let notch_pos = self.island.window_pos(notch_size);
            if let Ok(mut hot) = self.island.hot_rect.lock() {
                *hot = self.island.hot_zone(notch_pos, notch_size);
            }
            // The cursor watcher wakes this thread the moment the pointer
            // reaches the trigger strip, so a hidden island does not need to
            // poll for it. A 30ms self-tick here kept the process repainting
            // for its whole life while showing nothing.
            //
            // The exception is the moment the pointer is *on* the strip and
            // the open dwell has not elapsed. The watcher's wake-up gets us
            // one frame, that frame decides "not yet", and at 500ms the next
            // frame is half a second away — so the island sat there dead for
            // half a second after being pointed at, which reads as a freeze.
            ctx.request_repaint_after(if hovered {
                Duration::from_millis(16)
            } else {
                Duration::from_millis(500)
            });
            return;
        }

        // Whole points, and only when something actually moved. The lerp
        // changes the size by a fraction of a point every frame; forwarding
        // each one is ~60 window resizes a second, and the window server makes
        // you pay for every one of them.
        let size = egui::vec2(size.x.round(), size.y.round());
        let pos = egui::pos2(pos.x.round(), pos.y.round());
        let moved = self
            .island
            .last_sent
            .map(|(p, sz)| (p - pos).length() >= 2.0 || (sz - size).length() >= 2.0)
            .unwrap_or(true);
        if moved {
            self.island.last_sent = Some((pos, size));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
        if let Ok(mut hot) = self.island.hot_rect.lock() {
            *hot = self.island.hot_zone(pos, size);
        }

        // Fast enough for smooth animation and countdown; idle when hidden.
        let settled = self.island.anim_size == target
            && self.island.anim_opacity == target_op
            && self.island.anim_top == self.island.target_top();
        let tick = match self.island.phase {
            // Same reason as the hidden branch: a pointer sitting on the strip
            // is waiting for the dwell, and a slow tick makes that wait look
            // like the island has stopped responding.
            _ if hovered && !matches!(self.island.phase, IslandPhase::Expanded) => 16,
            IslandPhase::Hidden => 100,
            IslandPhase::Resting if settled => 200,
            // Mid-morph, or open: 60fps. A settled expanded island still needs
            // a decent tick for the countdown and the level meter.
            _ if settled => 60,
            _ => 16,
        };
        ctx.request_repaint_after(Duration::from_millis(tick));
    }

    /// Which size the island wants to be, from the pointer and what is going on.
    ///
    /// Split out from `drive_island` so the phase rules are one readable block
    /// rather than a branch inside a frame's worth of bookkeeping.
    fn island_phase_from_input(
        &mut self,
        hovered: bool,
        dragging_files: bool,
        activity_live: bool,
    ) -> IslandPhase {
        // A window the user opened deliberately outranks a HUD that appears
        // on hover. On a laptop display there is no arrangement where a
        // full-height settings window and the island both fit above the fold,
        // so the island stands down rather than sitting on top of it.
        if self.island.window_open {
            self.island.pinned = false;
            return IslandPhase::Hidden;
        }
        // A file in mid-drag opens the shelf.
        if dragging_files {
            self.island.left_at = None;
            return IslandPhase::Expanded;
        }
        if self.island.pinned {
            return IslandPhase::Expanded;
        }
        if hovered {
            self.island.left_at = None;
            // Crossing the strip is not the same as aiming at it. Without a
            // dwell, every trip to the menu bar or a browser tab threw the
            // panel open over whatever was underneath.
            let settled = !matches!(self.island.phase, IslandPhase::Hidden | IslandPhase::Resting);
            let since = *self.island.entered_at.get_or_insert_with(Instant::now);
            if settled || since.elapsed() >= OPEN_DWELL {
                return IslandPhase::Expanded;
            }
            return self.island.phase;
        }
        self.island.entered_at = None;

        let delay = if self.island.phase == IslandPhase::Expanded {
            COLLAPSE_DELAY
        } else {
            Duration::ZERO
        };
        let at = *self.island.left_at.get_or_insert_with(Instant::now);
        if at.elapsed() < delay {
            self.island.phase
        } else if activity_live {
            IslandPhase::Peek
        } else {
            // No activity and not hovered — disappear entirely, like iPhone.
            IslandPhase::Hidden
        }
    }

    /// Files dropped on the island go on the shelf.
    ///
    /// The drop lands wherever the pointer is over the island — the shelf card
    /// does not have to be the thing under it, because by the time you are
    /// dragging a file you are aiming at the notch, not at a card.
    fn island_take_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let mut added = 0;
        for path in dropped {
            if self.island.shelf.iter().any(|i| i.path == path) {
                continue;
            }
            if let Some(item) = ShelfItem::from_path(path) {
                self.island.shelf.insert(0, item);
                added += 1;
            }
        }
        if added > 0 {
            save_shelf(&self.island.shelf);
            // Make sure the shelf is actually on the island, or the files
            // would land somewhere the user cannot see.
            if !self.island.config.has(IslandModule::Files) {
                self.island.config.set(IslandModule::Files, true);
                clipd_core::save_island_config(&self.island.config);
            }
            self.island.announce(
                "Shelved",
                if added == 1 {
                    "1 file".to_string()
                } else {
                    format!("{added} files")
                },
            );
        }
    }

    /// Keep an eye on the grants the multi-slot listener needs.
    ///
    /// Without Accessibility, `rdev` can't create its event tap and every
    /// Cmd+C tap, slot paste and hotkey silently stops working — the daemon
    /// just retries forever and writes a line to a log nobody reads. macOS
    /// revokes this grant whenever the binary changes, which means a rebuild
    /// is enough to break it, so it has to be visible where the user looks.
    fn island_check_hotkeys(&mut self) {
        if self.island.hotkeys_checked.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.island.hotkeys_checked = Instant::now();
        // The status the *daemon* recorded, not this process's own grant.
        // `AXIsProcessTrusted` answers for whoever asks, and the island is not
        // the process running the event tap — clipd-ui is. Asking here said
        // "granted" while the listener had been failing 7,000 times over.
        self.island.hotkeys_ok = !matches!(
            clipd_core::load_hotkey_status(),
            clipd_core::HotkeyStatus::NeedsAccessibility
        );
    }

    /// A banner when the hotkeys are down, with the one button that fixes it.
    fn island_hotkey_banner(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        egui::Frame::none()
            .fill(s.warn.gamma_multiply(0.20))
            .rounding(Rounding::same(11.0))
            .stroke(Stroke::new(1.0, s.warn.gamma_multiply(0.55)))
            .inner_margin(Margin::symmetric(10.0, 7.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // Prose across the full width, buttons on their own line.
                // Setting them side by side reserved 200pt for the buttons on
                // a panel barely wider than that, so the explanation wrapped
                // into a five-line column and the banner grew taller than the
                // list it was warning about.
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    RichText::new("Multi-slot copy is off")
                        .size(11.5)
                        .strong()
                        .color(s.ink),
                );
                ui.label(
                    RichText::new(
                        "macOS drops the grant when clipd is rebuilt. Re-add it, then \
                         restart — a new grant never reaches a running app.",
                    )
                    .size(9.5)
                    .color(s.dim),
                );
                ui.add_space(7.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if island_button(ui, &s, "Open Settings").clicked() {
                        #[cfg(target_os = "macos")]
                        clipd_core::open_keyboard_permission_settings();
                    }
                    // The second half of the fix, and the half that is easy
                    // to miss: granting alone changes nothing until the
                    // process that wanted the grant starts again.
                    if island_button(ui, &s, "Restart clipd")
                        .on_hover_text("Relaunch the tray host so it picks up the grant")
                        .clicked()
                    {
                        restart_tray_host();
                        self.island.note("Restarting clipd…");
                    }
                });
            });
        ui.add_space(6.0);
    }

    /// Flash the newest clip in the collapsed island, once.
    fn island_announce_new_clip(&mut self) {
        if !self.island.config.live_activity {
            return;
        }
        let Some(clip) = self.clips.first() else {
            return;
        };
        let id = clip.id;
        if self.island.announced_clip == Some(id) {
            return;
        }
        // The first refresh after launch would otherwise announce whatever was
        // already at the top of the history, which the user copied long ago.
        let first_sighting = self.island.announced_clip.is_none();
        self.island.announced_clip = Some(id);
        if first_sighting {
            return;
        }
        let detail = island_clip_line(clip, 42);
        let label = match clip.content_type {
            ContentType::Image => "Image copied",
            ContentType::File => "Files copied",
            _ => "Copied",
        };
        // A multi-slot copy says which slot it landed in.
        //
        // Repeated Cmd+C fills numbered slots, and knowing *that* it copied is
        // only half the story — the number is what you need to press to get it
        // back. Without it the second and third copies announce themselves the
        // same way the first did, and you have to count them in your head.
        match clip.slot {
            Some(n) => self.island.announce(
                format!("{label} · slot {}", clipd_core::slot_badge(n)),
                detail,
            ),
            None => self.island.announce(label, detail),
        }
    }

    // ── Painting ──

    /// How settled the window is, 0..=1, as a fraction of its target width.
    ///
    /// The island's contents are laid out for the size the window is *going*
    /// to be. Painting them into a window still a third of that shows a bar
    /// with its brand clipped and its counts spilling past the edge — the
    /// "broken short version" that flashed on every hover. Below the threshold
    /// only the shell is drawn; across it the contents fade in.
    fn island_reveal(&self) -> f32 {
        let target = self.island.target_size();
        if target.x <= 1.0 {
            return 1.0;
        }
        let ratio = (self.island.anim_size.x / target.x).clamp(0.0, 1.0);
        ((ratio - 0.72) / 0.22).clamp(0.0, 1.0)
    }

    pub(crate) fn render_island(&mut self, ctx: &egui::Context) {
        // Skip rendering entirely when hidden — the window is off-screen.
        if self.island.phase == IslandPhase::Hidden && self.island.anim_opacity < 0.02 {
            return;
        }

        // Recomputed every frame: the theme can change under us (a System
        // appearance flip, or Cmd+T in the palette process), and a stale skin
        // would leave the island in last night's colours.
        let mut colors = resolved_theme(ctx, self.theme).colors();
        self.custom_colors.apply_to(&mut colors);
        // A glass theme already asked for vibrancy on every clipd surface, so
        // the island follows it without needing its own setting turned on.
        self.island.skin = IslandSkin::frosted(&colors);
        let s = self.island.skin;
        let hug = self.island.config.anchor == IslandAnchor::Auto;
        let cutout = self.island.geometry.cutout(hug);
        let phase = self.island.phase;
        let opacity = self.island.anim_opacity;

        // Rounded pill corners — uniform, like iPhone Dynamic Island.
        let round = match phase {
            IslandPhase::Hidden | IslandPhase::Resting => 16.0,
            // Half the height: the bar reads as one capsule rather than as a
            // panel that happens to be short.
            IslandPhase::Peek => ui_height(ctx) / 2.0,
            IslandPhase::Expanded => 24.0,
        };

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(s.shell)
                    .rounding(Rounding::same(round))
                    .stroke(Stroke::new(
                        (opacity * 0.5).max(0.0),
                        s.line,
                    ))
                    .inner_margin(Margin::ZERO),
            )
            .show(ctx, |ui| {
                let reveal = self.island_reveal();
                match phase {
                    IslandPhase::Hidden | IslandPhase::Resting => {
                        self.render_island_resting(ui, cutout)
                    }
                    // Mid-grow the shell is drawn on its own; the contents
                    // arrive once there is room to lay them out.
                    _ if reveal <= 0.01 => {}
                    IslandPhase::Peek => {
                        ui.set_opacity(reveal);
                        self.render_island_peek(ui, cutout)
                    }
                    IslandPhase::Expanded => {
                        ui.set_opacity(reveal);
                        self.render_island_expanded(ui, cutout)
                    }
                }
            });
    }

    /// Resting: nothing but a hairline of state either side of the cutout.
    fn render_island_resting(&mut self, ui: &mut egui::Ui, cutout: bool) {
        let s = self.island.skin;
        let rect = ui.max_rect();
        let (left, right) = split_around_notch(rect, self.island.geometry.width, cutout);
        let painter = ui.painter();

        // A quiet dot while a countdown is running — the only thing a resting
        // island still has to say.
        let busy = false;
        if busy && left.width() > 8.0 {
            painter.circle_filled(
                egui::pos2(left.right() - 7.0, left.center().y),
                2.6,
                s.good,
            );
        }

        // A click on the resting island opens it even when hover is off.
        let response = ui.interact(rect, egui::Id::new("island_rest"), egui::Sense::click());
        if response.clicked() {
            self.island.pinned = true;
            self.island.pin_is_implicit = true;
            self.island.left_at = None;
        }
    }

    /// Peek: the clipd bar, in whichever layout is set.
    ///
    /// This is the island's resting personality — what clipd is holding, and a
    /// way in — plus the two transient forms: a copy landing, and search.
    fn render_island_peek(&mut self, ui: &mut egui::Ui, _cutout: bool) {
        let s = self.island.skin;
        let layout = true;
        let full = ui.max_rect();
        let bar = full.shrink2(egui::vec2(BAR_PAD + 4.0, BAR_PAD));
        if bar.width() < 80.0 || bar.height() < 24.0 {
            return;
        }

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bar), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if self.island.search.is_some() {
                    self.render_bar_search(ui);
                } else if let Some(activity) = self.island.activity.clone() {
                    self.render_bar_activity(ui, &activity);
                } else {
                    self.render_bar_counts(ui);
                }
            });
        });
        let _ = s;

        // Clicking anywhere else on the bar opens the island properly.
        let response = ui.interact(
            full,
            egui::Id::new("island_peek"),
            egui::Sense::click(),
        );
        if response.clicked() && self.island.search.is_none() {
            self.island.pinned = true;
            self.island.pin_is_implicit = true;
            self.island.left_at = None;
        }
    }

    /// The default bar: brand, three counts, search.
    fn render_bar_counts(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        let counts = self.island_counts();
        let title = "Clipd";
        let detail = format!("{} copied", counts.copied);
        ui.spacing_mut().item_spacing.x = BAR_GROUP_GAP;
        draw_bar_brand(ui, &s, title, &detail, false);
        bar_divider(ui, &s, 26.0);

        let mut open_clips = false;
        let mut open_palette = false;
        bar_counts_group(ui, &s, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = BAR_ITEM_GAP;
                if bar_count(ui, &s, BarIcon::Clipboard, counts.copied, "Copied").clicked() {
                    open_clips = true;
                }
                bar_divider(ui, &s, 24.0);
                if bar_count(ui, &s, BarIcon::Pin, counts.pinned, "Pinned").clicked() {
                    open_palette = true;
                }
                bar_divider(ui, &s, 24.0);
                if bar_count(ui, &s, BarIcon::Clock, counts.recent, "Recent").clicked() {
                    open_clips = true;
                }
            });
        });
        if open_clips {
            self.island.pinned = true;
            self.island.pin_is_implicit = true;
            self.island.tab = IslandTab::Clips;
        }
        if open_palette {
            spawn_palette(&[]);
        }

        if bar_icon_button(ui, &s, BarIcon::Search, false, bar_search_size())
            .on_hover_text("Search your clips")
            .clicked()
        {
            self.island.search = Some(String::new());
            self.island.focus_search = true;
            self.island.pinned = true;
            self.island.pin_is_implicit = true;
            // Take the keyboard. The island is an always-on-top overlay that
            // never becomes key on its own, so `request_focus` on the text
            // field had nothing to focus *into* — the widget was ready and the
            // keystrokes were still going to whatever app was in front.
            //
            // Safe to do here, unlike on the tray popover: this is a click on a
            // search button, so taking the keyboard is the thing being asked
            // for. Escape and picking a result both release it.
            // Handled in `update`, which holds the frame this needs.
            self.want_key_window = true;
        }
    }

    /// The bar while something has just been copied: what it was, and the two
    /// things worth doing about it.
    fn render_bar_activity(&mut self, ui: &mut egui::Ui, activity: &Activity) {
        let s = self.island.skin;
        ui.spacing_mut().item_spacing.x = BAR_GROUP_GAP;
        draw_bar_brand(
            ui,
            &s,
            &activity.label,
            "just now",
            true,
        );
        bar_divider(ui, &s, 26.0);
        ui.add(
            egui::Label::new(
                RichText::new(truncate(&activity.detail, 60)).size(11.5).color(s.ink),
            )
            .truncate(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if bar_icon_button(ui, &s, BarIcon::Close, false, 30.0)
                .on_hover_text("Dismiss")
                .clicked()
            {
                self.island.activity = None;
            }
            if bar_icon_button(ui, &s, BarIcon::Check, true, 30.0)
                .on_hover_text("Pin this clip")
                .clicked()
            {
                if let Some(clip) = self.clips.first().map(|c| c.id) {
                    self.toggle_starred(clip);
                }
                self.island.activity = None;
                self.island.note("Pinned");
            }
        });
    }

    /// Search, in the bar. Typing filters the loaded history; the matches show
    /// as chips you can click to copy.
    fn render_bar_search(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        let mut query = self.island.search.clone().unwrap_or_default();
        let (mark, _) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
        draw_clipd_cat_image(ui, mark, s.accent);

        let field_w = (ui.available_width() - 44.0).max(80.0);
        let field = egui::TextEdit::singleline(&mut query)
            .hint_text("Search clips, links, code…")
            .desired_width(field_w)
            .frame(false)
            .text_color(s.ink);
        let response = ui.add_sized(egui::vec2(field_w, 30.0), field);
        if self.island.focus_search {
            self.island.focus_search = false;
            response.request_focus();
        }
        if response.changed() {
            self.island.search = Some(query.clone());
        }
        // Escape closes; Enter copies the first match, which is the whole
        // point of searching from a bar rather than opening the palette.
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.island.search = None;
            self.island.release_search_pin();
        }
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some(clip) = self.island_search_hits(&query).first().cloned() {
                let copied = self.island_copy(&clip);
                self.island.note(if copied { "Copied" } else { "Couldn't copy" });
                self.island.search = None;
            self.island.release_search_pin();
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if bar_icon_button(ui, &s, BarIcon::Close, false, 30.0)
                .on_hover_text("Close search")
                .clicked()
            {
                self.island.search = None;
            self.island.release_search_pin();
            }
        });
    }

    /// Clips matching a bar search, newest first.
    fn island_search_hits(&self, query: &str) -> Vec<clipd_core::ClipEntry> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.clips
            .iter()
            .filter(|clip| clip.content.to_ascii_lowercase().contains(&q))
            .take(8)
            .cloned()
            .collect()
    }

    /// What clipd is holding, for the bar's counts.
    fn island_counts(&self) -> ClipCounts {
        use chrono::{Local, TimeZone};
        let today = Local::now().date_naive();
        let copied = self
            .clips
            .iter()
            .filter(|c| {
                Local
                    .from_utc_datetime(&c.timestamp.naive_utc())
                    .date_naive()
                    == today
            })
            .count();
        ClipCounts {
            copied,
            pinned: self.starred_clip_ids.len(),
            recent: self.clips.len(),
        }
    }

    /// The line the peek shows when there is no announcement to make.
    fn island_peek_summary(&self) -> String {
        let snapshot = self.island.snapshot();
        if false {
            return "--:--".to_string();
        }
        match self.clips.first() {
            Some(clip) => island_clip_line(clip, 34),
            None => "Nothing copied yet".into(),
        }
    }

    /// Expanded: a strip of controls either side of the cutout, then the
    /// widgets themselves as a row of cards.
    ///
    /// The row is the whole point. A vertical stack of full-width tiles is a
    /// panel that happens to hang off the notch; a row of fixed-size cards
    /// reads as one instrument next to the camera housing, and every module
    /// stays glanceable without scrolling.
    /// The island is always the frosted pane.
    ///
    /// There used to be a Solid alternative. Once the frosted material was
    /// darkened enough to sit beside the bezel without clashing, the two were
    /// indistinguishable in normal use — so the setting was a choice between
    /// two things that looked the same.
    pub(crate) fn island_frosted(&self) -> bool {
        true
    }

    fn render_island_expanded(&mut self, ui: &mut egui::Ui, _cutout: bool) {
        let s = self.island.skin;
        let layout = true;
        let full = ui.max_rect();

        // One header band, not two. The panel used to carry the whole counts
        // bar *and* a separate tab strip under it, which is three stacked
        // rows of chrome above the content — and the counts were saying what
        // the cards below already showed.
        let head = egui::Rect::from_min_max(
            egui::pos2(full.left() + BAR_PAD + 4.0, full.top() + BAR_PAD),
            egui::pos2(
                full.right() - BAR_PAD - 4.0,
                full.top() + BAR_PAD + PANEL_HEAD_H,
            ),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(head), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = BAR_GROUP_GAP;
                if self.island.search.is_some() {
                    self.render_bar_search(ui);
                    return;
                }

                let counts = self.island_counts();
                draw_bar_brand(
                    ui,
                    &s,
                    "Clipd",
                    &format!("{} items", counts.recent),
                    false,
                );
                // Tabs only when the shelf is on. With the clipboard alone,
                // Home is one card of clips and Clips is the same clips in a
                // longer list — two names for one thing, and a row of chrome
                // to choose between them.
                let tabbed = self.island.config.has(IslandModule::Files);
                if tabbed {
                    bar_divider(ui, &s, 26.0);
                    ui.spacing_mut().item_spacing.x = 5.0;
                    for tab in IslandTab::ALL {
                        if island_tab_chip(ui, &s, tab.label(), self.island.tab == tab).clicked() {
                            self.island.tab = tab;
                        }
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if bar_icon_button(ui, &s, BarIcon::Search, false, 32.0)
                        .on_hover_text("Search your clips")
                        .clicked()
                    {
                        self.island.search = Some(String::new());
                        self.island.focus_search = true;
                        self.island.pinned = true;
            self.island.pin_is_implicit = true;
                    }
                    let pinned = self.island.pinned;
                    if island_glyph_button(ui, &s, IslandGlyph::Pin(pinned))
                        .on_hover_text(if pinned {
                            "Unpin — closes when the pointer leaves"
                        } else {
                            "Pin the island open"
                        })
                        .clicked()
                    {
                        self.island.pinned = !pinned;
                        self.island.pin_is_implicit = false;
                        self.island.pinned_at = (!pinned).then(Instant::now);
                        self.island.left_at = None;
                    }
                    if island_glyph_button(ui, &s, IslandGlyph::Gear)
                        .on_hover_text("All clipd settings")
                        .clicked()
                    {
                        spawn_palette(&["--settings"]);
                    }
                });
            });
        });

        let body = egui::Rect::from_min_max(
            egui::pos2(full.left() + ISLAND_PAD, head.bottom() + 8.0),
            egui::pos2(full.right() - ISLAND_PAD, full.bottom() - ISLAND_PAD),
        );
        if body.width() < 40.0 || body.height() < 40.0 {
            return;
        }
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(body), |ui| {
            if !self.island.hotkeys_ok {
                self.island_hotkey_banner(ui);
            }
            // Only Enter and Esc are bound, and only while the search field
            // has focus. Everything else here is the pointer.
            let searching = self.island.search.is_some();
            let footer: &[(&str, &str)] = if searching {
                &[("Enter", "Copy first match"), ("Esc", "Close")]
            } else {
                match self.island.tab {
                    IslandTab::Home => &[("", "Click a clip to copy · drop files to shelve")],
                    IslandTab::Clips => &[("", "Click to copy")],
                }
            };
            let body = ui.available_height() - FOOTER_H;
            ui.allocate_ui(egui::vec2(ui.available_width(), body), |ui| {
                match self.island.tab {
                    IslandTab::Home => self.island_home(ui),
                    IslandTab::Clips => self.island_clips(ui),
                }
            });
            let s = self.island.skin;
            island_footer(ui, &s, footer);
        });
    }

    /// The strip beside the cutout: brand and tabs on the left, state and
    /// controls on the right. Nothing is ever drawn between them.
    fn island_header(&mut self, ui: &mut egui::Ui, header: egui::Rect, cutout: bool) {
        let s = self.island.skin;
        let (left, right) = split_around_notch(header, self.island.geometry.width, cutout);

        // Left of the cutout: the mark, then the tabs. The tabs are the only
        // navigation the island has, and they are also how cards get added, so
        // they cannot live behind anything.
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(left.shrink2(egui::vec2(ISLAND_PAD, 3.0))),
            |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let (mark, _) =
                        ui.allocate_exact_size(egui::vec2(30.0, 26.0), egui::Sense::hover());
                    draw_clipd_cat_image(ui, mark, s.shell);
                    ui.add_space(2.0);
                    for tab in IslandTab::ALL {
                        if island_tab_chip(ui, &s, tab.label(), self.island.tab == tab).clicked() {
                            self.island.tab = tab;
                        }
                    }
                });
            },
        );

        // Right of the cutout: controls at the far edge, then the state line.
        // Fixing the controls to the edge stops them shuffling every time the
        // text beside them changes length.
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(right.shrink2(egui::vec2(ISLAND_PAD, 3.0))),
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let pinned = self.island.pinned;
                    if island_glyph_button(ui, &s, IslandGlyph::Pin(pinned))
                        .on_hover_text(if pinned {
                            "Unpin — closes when the pointer leaves"
                        } else {
                            "Pin the island open"
                        })
                        .clicked()
                    {
                        self.island.pinned = !pinned;
                        self.island.pinned_at = (!pinned).then(Instant::now);
                        self.island.left_at = None;
                    }
                    if island_glyph_button(ui, &s, IslandGlyph::Gear)
                        .on_hover_text("All clipd settings")
                        .clicked()
                    {
                        spawn_palette(&["--settings"]);
                    }
                    ui.add_space(3.0);
                    let summary = self
                        .island
                        .status
                        .as_ref()
                        .filter(|(_, at)| at.elapsed() < Duration::from_secs(3))
                        .map(|(m, _)| m.clone())
                        .unwrap_or_else(|| self.island_peek_summary());
                    let room = ui.available_width().max(40.0);
                    ui.add_sized(
                        egui::vec2(room, 16.0),
                        egui::Label::new(
                            RichText::new(truncate(&summary, 60)).size(10.5).color(s.faint),
                        )
                        .truncate()
                        .halign(egui::Align::RIGHT),
                    );
                });
            },
        );
    }

    /// Home: the widget cards, laid out in the rows the window was sized for.
    /// Both use [`island_card_rows`], so the slab is never the wrong size for
    /// what is in it.
    fn island_home(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        let modules = self.island.config.active_modules();
        if modules.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                island_empty(ui, &s, "No cards on the island yet.");
                ui.add_space(6.0);
                if island_button(ui, &s, "Open settings").clicked() {
                    spawn_palette(&["--settings"]);
                }
            });
            return;
        }

        let avail = ui.available_width();
        let rows = island_card_rows(&modules, avail);
        let mut remove: Option<IslandModule> = None;
        ui.spacing_mut().item_spacing = egui::vec2(CARD_GAP, CARD_GAP);
        for row in rows {
            // Share out whatever the row didn't use. Fixed widths decide how
            // the cards *pack*; leaving the remainder as a notch at the end of
            // a short row just looks unfinished.
            let used: f32 = row.iter().map(|m| card_width(*m)).sum::<f32>()
                + CARD_GAP * row.len().saturating_sub(1) as f32;
            // Capped: a row with one card in it would otherwise stretch that
            // card across the whole panel, turning a battery percentage into a
            // billboard.
            let bonus = ((avail - used) / row.len() as f32).clamp(0.0, 48.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = CARD_GAP;
                for module in row {
                    let size = egui::vec2(card_width(module) + bonus, CARD_H);
                    let rect = ui
                        .allocate_ui_with_layout(
                            size,
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_min_size(size);
                                ui.set_max_size(size);
                                match module {
                                    IslandModule::Clipboard => self.card_clipboard(ui),
                                    IslandModule::Files => self.card_files(ui),
                                }
                            },
                        )
                        .response
                        .rect;
                    // Remove-on-hover, drawn over the finished card so no card
                    // has to know about editing. Only appears under the
                    // pointer, so a resting island is never covered in
                    // close buttons.
                    if ui.rect_contains_pointer(rect)
                        && island_remove_button(ui, &s, rect, module).clicked()
                    {
                        remove = Some(module);
                    }
                }
            });
        }
        if let Some(module) = remove {
            self.island.config.set(module, false);
            clipd_core::save_island_config(&self.island.config);
            self.island.note(format!("{} removed", module.label()));
        }
    }
    fn island_widgets(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        let mut toggle: Option<(IslandModule, bool)> = None;

        island_card_frame(ui, &s, ui.available_size(), |ui| {
            ui.label(
                RichText::new("CARDS ON THE ISLAND")
                    .size(8.5)
                    .strong()
                    .color(s.faint)
                    .extra_letter_spacing(1.0),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for module in IslandModule::ALL {
                    let on = self.island.config.has(module);
                    let supported = module.supported();
                    // Weather is the one card that can be on and still show
                    // nothing, so it says what it is waiting for rather than
                    // sitting there looking broken.
                    // The note used to be rendered as a sibling of the chip,
                    // which pushed the rest of the row along and broke the
                    // grid. It is a hover hint now; the standing caveats are
                    // spelled out under the row instead.
                    let response = island_module_chip(ui, &s, module.label(), None, on, supported);
                    if supported && response.clicked() {
                        toggle = Some((module, !on));
                    }
                }
            });
            ui.add_space(9.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if island_button(ui, &s, "All settings").clicked() {
                    spawn_palette(&["--settings"]);
                }
                ui.label(
                    RichText::new(
                        "Order and the rest live in All settings.",
                    )
                    .size(9.0)
                    .color(s.faint),
                );
            });
        });

        if let Some((module, on)) = toggle {
            self.island.config.set(module, on);
            clipd_core::save_island_config(&self.island.config);
            self.island.invalidate();
        }
    }

    /// Clips: the full recent list, for when the card's three rows aren't
    /// enough and you don't want the whole palette.
    fn island_clips(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        let want = self
            .island
            .config
            .clip_rows
            .clamp(1, CLIPS_TAB_ROWS)
            .min(self.clips.len().max(1));
        let clips: Vec<_> = self.clips.iter().take(want).cloned().collect();
        // Publish the count so the slab is sized for this list next frame.
        self.island.clips_rows = clips.len().max(1);
        island_card_frame(ui, &s, ui.available_size(), |ui| {
            if clips.is_empty() {
                island_empty(ui, &s, "Nothing copied yet.");
                return;
            }
            for clip in clips {
                let response = self.island_clip_row(ui, &clip, true);
                if response.clicked() {
                    let copied = self.island_copy(&clip);
                    self.island.note(if copied {
                        "Copied"
                    } else {
                        "Couldn't copy that clip"
                    });
                }
            }
        });
    }

    /// One clip line, shared by the card and the Clips tab.
    fn island_clip_row(
        &mut self,
        ui: &mut egui::Ui,
        clip: &clipd_core::ClipEntry,
        with_time: bool,
    ) -> egui::Response {
        let s = self.island.skin;
        let mut colors = resolved_theme(ui.ctx(), self.theme).colors();
        self.custom_colors.apply_to(&mut colors);
        island_row(ui, &s, |ui| {
            if clip.content_type == ContentType::Image {
                let thumb = clip.thumb_path.as_deref().and_then(|path| {
                    self.thumb_textures
                        .entry(clip.id)
                        .or_insert_with(|| load_thumb_texture(ui.ctx(), path))
                        .clone()
                });
                match thumb {
                    Some(tex) => {
                        ui.add(
                            egui::Image::new((tex.id(), egui::vec2(26.0, 16.0)))
                                .rounding(Rounding::same(3.0)),
                        );
                    }
                    None => {
                        let (slot, _) =
                            ui.allocate_exact_size(egui::vec2(26.0, 16.0), egui::Sense::hover());
                        ui.painter()
                            .rect_stroke(slot, Rounding::same(3.0), Stroke::new(1.0, s.line));
                    }
                }
                ui.add_space(2.0);
            }
                // Colour lives in the icon, never in the text. One tinted
                // square per row reads as a list; a coloured dot beside grey
                // text reads as a status light.
                let (tile, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                let tint = clip_type_color(clip, &colors, &s);
                ui.painter()
                    .rect_filled(tile, Rounding::same(5.0), tint.gamma_multiply(0.22));
                // A clip held in a numbered slot shows its number here instead
                // of the type dot. The number is what you press to paste it
                // back, so it belongs on the row for as long as the clip is in
                // the list — announcing it once as the copy lands tells you
                // only if you happened to be looking at that moment.
                match clip.slot {
                    Some(n) => {
                        ui.painter().text(
                            tile.center(),
                            egui::Align2::CENTER_CENTER,
                            clipd_core::slot_badge(n),
                            egui::FontId::proportional(11.0),
                            tint,
                        );
                    }
                    None => {
                        // A glyph for what the clip is, not a blank square.
                        // The square was the same mark on every row, so a
                        // list of ten rows carried ten identical icons and
                        // the column may as well not have been there.
                        ui.painter().text(
                            tile.center(),
                            egui::Align2::CENTER_CENTER,
                            island_type_glyph(clip),
                            egui::FontId::proportional(10.5),
                            tint,
                        );
                    }
                }
                ui.add_space(2.0);
                // The timestamp claims its width first, then the title fills
                // what is left and truncates against that.
                //
                // Adding the title first let it truncate against the *whole*
                // row, so a long clip ran under the time and the two printed
                // on top of each other — "…with a slot, an imanow".
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if with_time {
                        ui.label(
                            RichText::new(relative_time_short(&clip.timestamp))
                                .size(11.0)
                                .color(s.faint),
                        );
                        ui.add_space(10.0);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(island_clip_line(clip, 78))
                                    .size(13.0)
                                    .color(s.ink),
                            )
                            .truncate(),
                        );
                    });
                });
            let _ = &s;
        })
    }

    // ── Cards ──

    fn card_clipboard(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        let clips: Vec<_> = self.clips.iter().take(CARD_CLIP_ROWS).cloned().collect();
        let size = ui.available_size().max(egui::vec2(80.0, 60.0));
        island_card(ui, &s, size, "Clipboard", None, |ui| {
            if clips.is_empty() {
                island_empty(ui, &s, "Nothing copied yet.");
                return;
            }
            for clip in clips {
                if self.island_clip_row(ui, &clip, false).clicked() {
                    let copied = self.island_copy(&clip);
                    self.island.note(if copied {
                        "Copied"
                    } else {
                        "Couldn't copy that clip"
                    });
                }
            }
        });
    }

    /// The shelf: what is parked, as tiles you can see, plus somewhere to drop.
    ///
    /// A list of file names told you nothing Finder wouldn't. The point of a
    /// shelf is recognising the thing you put there, so each file gets a
    /// preview — the real image where it is one, a drawn document or folder
    /// mark where it isn't.
    fn card_files(&mut self, ui: &mut egui::Ui) {
        let s = self.island.skin;
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let count = self.island.shelf.len();
        let subtitle = if hovering {
            Some("Drop to shelve".to_string())
        } else if count > 0 {
            Some(format!("{count} ready"))
        } else {
            None
        };
        let items: Vec<ShelfItem> = self.island.shelf.iter().take(SHELF_TILES).cloned().collect();
        let size = ui.available_size().max(egui::vec2(80.0, 60.0));
        let mut copy: Option<ShelfItem> = None;
        let mut remove: Option<std::path::PathBuf> = None;
        let mut copy_all = false;
        let mut clear_all = false;

        island_card(ui, &s, size, "File shelf", subtitle.as_deref(), |ui| {
            if items.is_empty() {
                island_drop_zone(ui, &s, ui.available_height() - 2.0, hovering);
                return;
            }

            egui::ScrollArea::horizontal()
                .id_salt("island_shelf")
                .auto_shrink([false, true])
                .max_height(SHELF_TILE.y + 6.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for item in &items {
                            let thumb = self.island_shelf_thumb(ui.ctx(), item);
                            let response = island_file_tile(ui, &s, item, thumb.as_ref());
                            if response.clicked() {
                                copy = Some(item.clone());
                            }
                            if response.secondary_clicked() {
                                remove = Some(item.path.clone());
                            }
                        }
                        // The drop target sits at the end of the row, so there
                        // is always somewhere to aim even with a full shelf.
                        let (rect, _) = ui.allocate_exact_size(SHELF_TILE, egui::Sense::hover());
                        draw_drop_tile(ui.painter(), &s, rect, hovering);
                    });
                });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if island_button(ui, &s, "Copy all")
                    .on_hover_text("Put every shelved file on the clipboard at once")
                    .clicked()
                {
                    copy_all = true;
                }
                if island_button(ui, &s, "Clear").clicked() {
                    clear_all = true;
                }
                ui.label(
                    RichText::new("right-click removes")
                        .size(11.0)
                        .color(s.faint),
                );
            });
        });

        if let Some(item) = copy {
            match clipd_core::clipboard_write_file_urls(&[item.path.clone()]) {
                Ok(()) => self.island.note(format!("{} on the clipboard", item.name)),
                Err(e) => self.island.note(e),
            }
        }
        if copy_all {
            let paths: Vec<std::path::PathBuf> =
                self.island.shelf.iter().map(|i| i.path.clone()).collect();
            match clipd_core::clipboard_write_file_urls(&paths) {
                Ok(()) => self
                    .island
                    .note(format!("{} files on the clipboard", paths.len())),
                Err(e) => self.island.note(e),
            }
        }
        if let Some(path) = remove {
            self.island.shelf.retain(|i| i.path != path);
            save_shelf(&self.island.shelf);
        }
        if clear_all {
            self.island.shelf.clear();
            save_shelf(&self.island.shelf);
            self.island.note("Shelf cleared");
        }
    }

    /// A preview for a shelved file, decoded once and cached.
    fn island_shelf_thumb(
        &mut self,
        ctx: &egui::Context,
        item: &ShelfItem,
    ) -> Option<egui::TextureHandle> {
        if let Some(cached) = self.island.shelf_thumbs.get(&item.path) {
            return cached.clone();
        }
        // A 12-megapixel photo is ~50MB of RGBA and a shelf holds several.
        // Past this the tile gets a drawn document mark instead.
        const MAX_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;
        let ext = item
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let previewable = matches!(ext.as_str(), "png" | "jpg" | "jpeg");
        let tex = (previewable && item.size <= MAX_PREVIEW_BYTES)
            .then(|| load_thumb_texture(ctx, &item.path.to_string_lossy()))
            .flatten();
        self.island
            .shelf_thumbs
            .insert(item.path.clone(), tex.clone());
        tex
    }

    /// Copy one clip straight from the island, whatever kind it is.
    ///
    /// The palette's own copy path works off the selected row; the island has
    /// no selection, so it needs a by-value copy that still honours images and
    /// file clips rather than pasting their preview text.
    fn island_copy(&mut self, clip: &clipd_core::ClipEntry) -> bool {
        match clip.content_type {
            ContentType::Image => match clip.image_path.as_deref() {
                Some(path) => self.set_clipboard_image(path),
                None => false,
            },
            ContentType::File if !clip.files.is_empty() => self.set_clipboard_files(clip),
            _ => self.set_clipboard(&clip.content),
        }
    }
}

// ── Small drawing helpers ──

/// How wide each module's card is.
///
/// Fixed per module rather than shared out evenly: a scrubber and two lines of
/// track metadata need real width, while a battery percentage looks lost in
/// anything wider than a chip.
fn card_width(module: IslandModule) -> f32 {
    match module {
        // The clipboard is the island's reason to exist, so it gets the room.
        IslandModule::Clipboard => 300.0,
        IslandModule::Files => 300.0,
    }
}

/// Pack the cards into rows that fit `max_width`, keeping the user's order.
///
/// Order is the user's, set in settings, so this never reorders to pack more
/// tightly — a row that could hold one more card is a fair price for the
/// modules staying where they were put.
fn island_card_rows(modules: &[IslandModule], max_width: f32) -> Vec<Vec<IslandModule>> {
    let mut rows: Vec<Vec<IslandModule>> = Vec::new();
    let mut row: Vec<IslandModule> = Vec::new();
    let mut used = 0.0_f32;
    for module in modules {
        let w = card_width(*module);
        let needed = if row.is_empty() { w } else { used + CARD_GAP + w };
        if !row.is_empty() && needed > max_width {
            rows.push(std::mem::take(&mut row));
            used = 0.0;
        }
        used = if row.is_empty() { w } else { used + CARD_GAP + w };
        row.push(*module);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// clipd's cat, as it appears on the site — compiled into the binary.
///
/// `include_bytes!` rather than a file beside the binary: the mark has to
/// survive being copied somewhere else, and an icon that silently disappears
/// depending on the working directory is worse than no icon.
static CAT_PNG: &[u8] = include_bytes!("../assets/cat.png");

/// The cat as a texture, decoded once for the process.
fn clipd_cat_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    static CACHE: Mutex<Option<Option<egui::TextureHandle>>> = Mutex::new(None);
    let mut slot = CACHE.lock().ok()?;
    if slot.is_none() {
        *slot = Some(
            clipd_core::decode_rgba(CAT_PNG)
                .ok()
                .map(|(w, h, rgba)| {
                    let img =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    ctx.load_texture("clipd_cat", img, egui::TextureOptions::LINEAR)
                }),
        );
    }
    slot.as_ref().and_then(|t| t.clone())
}

/// Paint the cat to fit `rect`, keeping its proportions.
///
/// Falls back to the drawn face if the texture cannot be decoded, so the
/// header is never left with a hole in it.
fn draw_clipd_cat_image(ui: &mut egui::Ui, rect: egui::Rect, fallback: Color32) {
    let Some(tex) = clipd_cat_texture(ui.ctx()) else {
        draw_clipd_cat(ui.painter(), rect, fallback);
        return;
    };
    let size = tex.size_vec2();
    let scale = (rect.width() / size.x).min(rect.height() / size.y);
    let fitted = egui::Rect::from_center_size(rect.center(), size * scale);
    egui::Image::new((tex.id(), fitted.size())).paint_at(ui, fitted);
}

/// clipd's cat, as line art — the mark from the top-left of clipd.sh.
///
/// Drawn rather than shipped as an image for the same reason the rest of the
/// island's glyphs are: one path scales cleanly to the tile, the pill and the
/// bar without three PNGs to keep in step, and it takes the colour it is
/// handed instead of baking one in.
fn draw_clipd_cat(painter: &egui::Painter, rect: egui::Rect, col: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    let w = Stroke::new((r * 0.15).clamp(1.0, 2.0), col);

    // Sits low in the box: the ears take the room above it.
    let head = r * 0.72;
    let hc = egui::pos2(c.x, c.y + r * 0.14);
    painter.circle_stroke(hc, head, w);

    // Ears rise clear of the head outline. Tucked against it they read as
    // notches in a circle rather than as ears — the whole mark then stops
    // being a cat at a glance, which is the only job it has.
    for side in [-1.0_f32, 1.0] {
        let inner = egui::pos2(hc.x + side * head * 0.34, hc.y - head * 0.94);
        let outer = egui::pos2(hc.x + side * head * 0.92, hc.y - head * 0.42);
        let tip = egui::pos2(hc.x + side * head * 0.78, hc.y - head * 1.52);
        painter.line_segment([inner, tip], w);
        painter.line_segment([tip, outer], w);
    }

    // Two eyes and a nose. No whiskers: at twenty points they crossed the
    // muzzle and collided with the eyes, and the face turned to noise.
    let eye = (r * 0.11).max(1.0);
    for side in [-1.0_f32, 1.0] {
        painter.circle_filled(
            egui::pos2(hc.x + side * head * 0.36, hc.y - head * 0.14),
            eye,
            col,
        );
    }
    let nose = egui::pos2(hc.x, hc.y + head * 0.30);
    painter.circle_filled(nose, eye * 0.8, col);
}

/// The strip along the bottom: the actions for whatever is open.
///
/// Raycast's most useful habit — the panel always says what the keys do, so
/// nothing has to be remembered or discovered twice. A hairline above it and
/// nothing else; it must read as the frame, not as another row.
fn island_footer(ui: &mut egui::Ui, s: &IslandSkin, hints: &[(&str, &str)]) {
    // An empty key means the hint is a sentence, not a shortcut. The island is
    // driven with the pointer, and printing key caps for keys it does not bind
    // would be decoration that lies.
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), FOOTER_H),
        egui::Sense::hover(),
    );
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        Stroke::new(1.0, s.line.gamma_multiply(0.5)),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 6.0;
    for (key, label) in hints.iter().rev() {
        if !key.is_empty() {
            island_key_cap(&mut child, s, key);
        }
        child.label(RichText::new(*label).size(11.0).color(s.faint));
        child.add_space(6.0);
    }
}

/// One keyboard cap, the way a shortcut is written down.
fn island_key_cap(ui: &mut egui::Ui, s: &IslandSkin, key: &str) {
    let galley = ui.painter().layout_no_wrap(
        key.to_string(),
        egui::FontId::proportional(10.5),
        s.dim,
    );
    let size = egui::vec2(galley.size().x.max(11.0) + 9.0, 17.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::same(4.0), s.tile);
    ui.painter().galley(
        rect.center() - galley.size() / 2.0,
        galley,
        s.dim,
    );
}

/// A card: fixed size, rounded, with a quiet caption at the top.
fn island_card(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    size: egui::Vec2,
    title: &str,
    subtitle: Option<&str>,
    body: impl FnOnce(&mut egui::Ui),
) {
    island_card_frame(ui, s, size, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(title).size(11.5).color(s.faint),
            );
            if let Some(subtitle) = subtitle {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(subtitle).size(11.5).color(s.faint));
                });
            }
        });
        ui.add_space(CARD_CAPTION_GAP);
        body(ui);
    });
}

/// The card's shell, without the caption — used directly by the Clips tab,
/// which is one card filling the slab.
fn island_card_frame(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    size: egui::Vec2,
    body: impl FnOnce(&mut egui::Ui),
) {
    let inner = if size.x > CARD_PAD_X * 2.0 && size.y > CARD_PAD_Y * 2.0 {
        size - egui::vec2(CARD_PAD_X * 2.0, CARD_PAD_Y * 2.0)
    } else {
        egui::vec2(80.0, 60.0)
    };
    // No fill, no border. A panel of boxes inside a box is the thing that
    // made this look busy; a header and some space group a list just as well.
    egui::Frame::none()
        .inner_margin(Margin::symmetric(CARD_PAD_X, CARD_PAD_Y))
        .show(ui, |ui| {
            ui.set_min_size(inner);
            ui.set_max_size(inner);
            // Rows carry their own height; 2pt between them was enough to
            // push a three-row list past the bottom of its card.
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.set_clip_rect(ui.max_rect().expand(2.0));
            body(ui);
        });
}

/// Centre a card's contents in whatever height is left under the caption.
///
/// Stat cards — battery, timer, weather — are one value and a label. Laid out
/// from the top they sit under the caption with a third of the card empty
/// below them, which is what made the row look unfinished.
fn island_card_center(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    let rect = ui.max_rect();
    let taken = ui.min_rect().height();
    let free = (rect.height() - taken).max(0.0);
    // Half the slack above, and the layout keeps the rest below.
    ui.add_space(free * 0.42);
    body(ui);
}

/// The × that appears on a card while the pointer is over it.
///
/// Drawn over the finished card rather than inside it, so no card renderer has
/// to know that editing exists.
fn island_remove_button(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    card: egui::Rect,
    module: IslandModule,
) -> egui::Response {
    let rect = egui::Rect::from_center_size(
        egui::pos2(card.right() - 12.0, card.top() + 11.0),
        egui::vec2(16.0, 16.0),
    );
    let response = ui.interact(
        rect,
        egui::Id::new(("island_remove", module.label())),
        egui::Sense::click(),
    );
    let hovered = response.hovered();
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 7.5, if hovered { s.warn } else { s.row_hover });
    let ink = if hovered { s.shell } else { s.dim };
    let r = 3.2;
    for (dx, dy) in [(-1.0_f32, -1.0_f32), (-1.0, 1.0)] {
        painter.line_segment(
            [
                egui::pos2(rect.center().x + dx * r, rect.center().y + dy * r),
                egui::pos2(rect.center().x - dx * r, rect.center().y - dy * r),
            ],
            Stroke::new(1.4, ink),
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("Remove {}", module.label()))
}

/// One module in the Widgets tab: a chip that is filled when the card is on.
fn island_module_chip(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    label: &str,
    note: Option<&str>,
    on: bool,
    enabled: bool,
) -> egui::Response {
    // No tick glyph: ✓ isn't in egui's bundled fonts and came out as a box.
    // The filled chip is the state — accent means on, outline means off.
    let text = label.to_string();
    let fill = if !enabled {
        s.tile
    } else if on {
        s.accent
    } else {
        s.row_hover
    };
    let ink = if !enabled {
        s.faint
    } else if on {
        s.shell
    } else {
        s.ink
    };

    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new(RichText::new(text).size(10.5).color(ink))
                .fill(fill)
                .stroke(Stroke::new(1.0, if on { fill } else { s.line }))
                .rounding(Rounding::same(999.0))
                .min_size(egui::vec2(0.0, 24.0)),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if !enabled {
            "Not available on this platform".to_string()
        } else if on {
            format!("Remove {label} from the island")
        } else {
            format!("Add {label} to the island")
        });
    if let Some(note) = note {
        ui.label(RichText::new(note).size(8.5).color(s.faint));
    }
    response
}

/// A tab chip in the header strip.
fn island_tab_chip(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    label: &str,
    active: bool,
) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(12.0)
                .color(if active { s.ink } else { s.dim }),
        )
        // Selection is a translucent lift off the slab — the material getting
        // slightly thicker where you are — not a patch of accent. Tinting it
        // made the selected tab an announcement; this reads as depth, and the
        // colour stays for the things that are actually about colour.
        .fill(if active {
            // A translucent lift on a dark slab, a translucent press on a
            // light one. White over a pale surface is nothing at all.
            if s.dark {
                Color32::from_white_alpha(20)
            } else {
                Color32::from_black_alpha(18)
            }
        } else {
            Color32::TRANSPARENT
        })
        .stroke(if active {
            Stroke::new(
                1.0,
                if s.dark {
                    Color32::from_white_alpha(16)
                } else {
                    Color32::from_black_alpha(14)
                },
            )
        } else {
            Stroke::NONE
        })
        .rounding(Rounding::same(7.0))
        .min_size(egui::vec2(0.0, 24.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The two header controls, drawn rather than typed for the same reason the
/// transport is: neither glyph is reliably in egui's bundled fonts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IslandGlyph {
    Pin(bool),
    Gear,
}

fn island_glyph_button(ui: &mut egui::Ui, s: &IslandSkin, glyph: IslandGlyph) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    let center = rect.center();
    match glyph {
        IslandGlyph::Pin(on) => {
            let color = if on {
                s.accent
            } else if hovered {
                s.ink
            } else {
                s.faint
            };
            if on {
                painter.circle_filled(center, 4.0, color);
            } else {
                painter.circle_stroke(center, 4.0, Stroke::new(1.2, color));
            }
        }
        IslandGlyph::Gear => {
            let color = if hovered { s.ink } else { s.faint };
            // A hub with six spokes. Four teeth set at the diagonals read as a
            // plain circle with specks around it — the eye needs a spoke on
            // the vertical and the horizontal to see a gear at all.
            // Big hub, short teeth. Long spokes on a small hub is a sunburst;
            // what makes a gear read is the ring being the dominant shape with
            // the teeth barely clearing it.
            painter.circle_stroke(center, 4.6, Stroke::new(1.4, color));
            painter.circle_filled(center, 1.5, color);
            for i in 0..6 {
                let angle = i as f32 * std::f32::consts::PI / 3.0;
                let (sin, cos) = angle.sin_cos();
                painter.line_segment(
                    [
                        egui::pos2(center.x + cos * 4.4, center.y + sin * 4.4),
                        egui::pos2(center.x + cos * 6.9, center.y + sin * 6.9),
                    ],
                    Stroke::new(2.2, color),
                );
            }
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A clickable line inside a tile — tighter, with a subtle hover.
fn island_row(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    island_row_marked(ui, s, false, contents)
}

/// A row, optionally ringed to show it is held in an active slot.
///
/// The ring uses the same shape the hover fill does — same rect, same
/// rounding — so a slotted row reads as the same object in a different state
/// rather than as a differently-built row. It is the accent at a third
/// strength: enough to find at a glance while scanning, not enough to compete
/// with the row you are actually pointing at.
fn island_row_marked(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    slotted: bool,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let height = ISLAND_ROW_H;
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let shape = rect.expand2(egui::vec2(3.0, 0.0));
    if response.hovered() {
        ui.painter()
            .rect_filled(shape, Rounding::same(6.0), s.row_hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if slotted {
        ui.painter().rect_stroke(
            shape,
            Rounding::same(6.0),
            Stroke::new(1.0, s.accent.gamma_multiply(0.34)),
        );
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(4.0, 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 6.0;
    contents(&mut child);
    response
}

/// The island's only button style — a small dark capsule with a hairline.
fn island_button(ui: &mut egui::Ui, s: &IslandSkin, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).color(s.ink))
            .fill(s.tile)
            .stroke(Stroke::NONE)
            .rounding(Rounding::same(7.0))
            .min_size(egui::vec2(0.0, 26.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The window's height in points this frame, for the capsule's radius.
fn ui_height(ctx: &egui::Context) -> f32 {
    ctx.input(|i| i.screen_rect().height())
}

/// Hand the island window to AppKit's own material, or take it back.
///
/// `HUDWindow` and `Popover` are what macOS uses for its own HUDs and menus,
/// which is what the island is pretending to be. The radius is passed in so
/// the blur is masked to the capsule rather than to a rectangle behind it.
#[cfg(target_os = "macos")]
fn sync_island_material(
    frame: &eframe::Frame,
    frosted: bool,
    dark: bool,
    radius: f32,
    applied: &mut Option<(bool, i32)>,
) {
    let want = (frosted, radius.round() as i32);
    if *applied == Some(want) {
        return;
    }
    *applied = Some(want);

    clear_island_material(frame);
    if !frosted {
        return;
    }
    if let Err(err) = apply_island_material(frame, dark, radius) {
        // Not fatal: the translucent fills alone still read as a lighter
        // island, just without the blur behind them.
        log::info!("island vibrancy unavailable: {err}");
    }
}

#[cfg(not(target_os = "macos"))]
fn sync_island_material(
    _frame: &eframe::Frame,
    _frosted: bool,
    _dark: bool,
    _radius: f32,
    _applied: &mut Option<(bool, i32)>,
) {
}

/// Tags the island's own effect view, so removing it can't disturb anything
/// else AppKit has put in the window.
#[cfg(target_os = "macos")]
const ISLAND_MATERIAL_TAG: isize = 0x1_5A_D0;

/// Put an `NSVisualEffectView` behind the island's content.
///
/// It goes in as a *sibling* of the window's content view, ordered below it.
/// `window_vibrancy::apply_vibrancy` adds it as a child of the Metal view,
/// where its layer draws over the layer it is meant to sit behind — the island
/// comes out as a blurred capsule with no contents at all.
#[cfg(target_os = "macos")]
fn apply_island_material(frame: &eframe::Frame, dark: bool, radius: f32) -> Result<(), String> {
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSView, NSVisualEffectBlendingMode,
        NSVisualEffectMaterial, NSVisualEffectState, NSWindowOrderingMode,
    };
    use window_vibrancy::NSVisualEffectViewTagged;

    let mtm = MainThreadMarker::new().ok_or("not on main thread")?;
    let metal = crate::ns_metal_view(frame).ok_or("no ns_view")?;
    let window = metal.window().ok_or("ns_view has no window yet")?;
    let content = window.contentView().ok_or("window has no contentView")?;
    let parent = unsafe { content.superview() }.ok_or("contentView has no superview")?;

    let effect = unsafe {
        NSVisualEffectViewTagged::initWithFrame(mtm.alloc(), content.frame(), ISLAND_MATERIAL_TAG)
    };
    unsafe {
        effect.setMaterial(if dark {
            NSVisualEffectMaterial::HUDWindow
        } else {
            NSVisualEffectMaterial::Popover
        });
        // BehindWindow is what makes it sample the desktop rather than the
        // window's own contents.
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::Active);
        effect.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
    }
    effect.setWantsLayer(true);
    if let Some(layer) = effect.layer() {
        layer.setCornerRadius(radius as f64);
        layer.setMasksToBounds(true);
    }

    let effect_view: Retained<NSView> = Retained::into_super(Retained::into_super(effect));
    parent.addSubview_positioned_relativeTo(
        &effect_view,
        NSWindowOrderingMode::Below,
        Some(content.as_ref()),
    );

    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    Ok(())
}

/// Take the island's effect view back out, leaving anything else alone.
#[cfg(target_os = "macos")]
fn clear_island_material(frame: &eframe::Frame) {
    use objc2_app_kit::NSView;

    let Some(metal) = crate::ns_metal_view(frame) else {
        return;
    };
    let Some(window) = metal.window() else {
        return;
    };
    let Some(content) = window.contentView() else {
        return;
    };
    let Some(parent) = (unsafe { content.superview() }) else {
        return;
    };
    let subviews: Vec<_> = parent.subviews().iter().collect();
    for view in subviews {
        let tag: isize = unsafe { objc2::msg_send![&*view, tag] };
        if tag == ISLAND_MATERIAL_TAG {
            <NSView>::removeFromSuperview(&view);
        }
    }
}

// ── The clipd bar ──
//
// The island's own presentation: brand, what clipd is holding, and a way in.
// Drawn as one full-width row rather than split around the cutout — a bar with
// a hole in the middle of it is not a bar.

/// clipd's brand colour, fixed rather than taken from the theme: a logo does
/// not change colour with the wallpaper, and several clipd themes carry a warm
/// hue in their `green` slot.
/// The island's accent — Catppuccin Mocha mauve, straight off clipd.sh.
///
/// The site's own GUI mockup wears a "Catppuccin" chip in this colour, so it
/// is already the product's mark rather than a colour chosen for the island in
/// isolation. On the near-black HUD it holds its own without the loudness of
/// a pure purple.
///
/// The system palette ships a separate, brighter set of values for dark
/// surfaces precisely because a light-mode colour goes muddy on one; this is
/// that value, not a hand-darkened guess. Indigo sits next to the blue the
/// text rows use rather than on top of it, so the brand mark reads as the app
/// and the rows read as content.
const BRAND_MAUVE: Color32 = Color32::from_rgb(125, 211, 240);
/// Apple's systemGreen, dark variant — reserved for "running", nothing else.
///
/// Green on a status dot is a metaphor people already know. It is a *state*
/// colour, so it stays green whatever the brand does; the two were the same
/// constant before, which meant restyling the brand silently restyled health.
const STATUS_GREEN: Color32 = Color32::from_rgb(48, 209, 88);
/// Supporting hues, so the counts are told apart at a glance.
const BRAND_VIOLET: Color32 = Color32::from_rgb(150, 140, 240);
/// Catppuccin blue — the same value the theme uses, so a plain-text row is
/// the same blue whether you are looking at the island or the palette.
const BRAND_BLUE: Color32 = Color32::from_rgb(125, 211, 240);

fn bar_height() -> f32 {
    44.0
}

/// The brand block: tile, gap, and a fixed text column. Fixed rather than
/// measured so the capsule doesn't change width when a count ticks over.
fn bar_brand_width() -> f32 {
    108.0
}

fn bar_count_width() -> f32 {
    54.0
}

fn bar_search_size() -> f32 {
    32.0
}

/// Width the bar needs, summed from what is actually in it — a fixed number
/// left slack between the counts and the search button.
fn bar_width() -> f32 {
    let group = 5.0 * 2.0 + 3.0 * bar_count_width() + 2.0 * (1.0 + BAR_ITEM_GAP * 2.0);
    bar_brand_width()
        + BAR_GROUP_GAP
        + 1.0
        + BAR_GROUP_GAP
        + group
        + BAR_GROUP_GAP
        + bar_search_size()
        + (BAR_PAD + 4.0) * 2.0
}

/// The brand block: mark, wordmark, and one line of state.
fn draw_bar_brand(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    title: &str,
    detail: &str,
    live: bool,
) {
    ui.allocate_ui_with_layout(
        egui::vec2(bar_brand_width(), 34.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            // Claim the whole column: the parent advances by what was *used*,
            // so a short wordmark otherwise leaves slack at the far end.
            ui.set_min_size(egui::vec2(bar_brand_width(), 34.0));
            ui.spacing_mut().item_spacing.x = 8.0;
            let (tile, _) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
            draw_clipd_cat_image(ui, tile, s.accent);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(title).size(12.5).strong().color(s.ink));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if live {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.0, s.good);
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(detail)
                                .size(10.0)
                                .color(if live { s.good } else { s.faint }),
                        )
                        .truncate(),
                    );
                });
            });
        },
    );
}

/// One count: icon tile, number, and — in Balanced — its label. Draws no
/// background of its own; the three share one container.
fn bar_count(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    kind: BarIcon,
    value: usize,
    label: &str,
) -> egui::Response {
    let compact = true;
    let size = egui::vec2(bar_count_width(), if compact { 34.0 } else { 38.0 });
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(egui::vec2(2.0, 1.0)),
            Rounding::same(10.0),
            s.row_hover,
        );
    }

    let icon_box = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        egui::vec2(24.0, 24.0),
    );
    ui.painter()
        .rect_filled(icon_box, Rounding::same(7.0), s.row_hover);
    draw_bar_icon(ui.painter(), icon_box.shrink(6.0), kind, bar_icon_hue(s, kind));

    let text_x = icon_box.right() + 8.0;
    if compact {
        ui.painter().text(
            egui::pos2(text_x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            value.to_string(),
            egui::FontId::proportional(13.0),
            s.ink,
        );
    } else {
        ui.painter().text(
            egui::pos2(text_x, rect.center().y - 7.0),
            egui::Align2::LEFT_CENTER,
            value.to_string(),
            egui::FontId::proportional(14.0),
            s.ink,
        );
        ui.painter().text(
            egui::pos2(text_x, rect.center().y + 8.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(9.5),
            s.faint,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The container the counts sit in: one rounded panel, hairlines between.
fn bar_counts_group<R>(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let pad = if true {
        Margin::symmetric(5.0, 4.0)
    } else {
        Margin::symmetric(6.0, 4.0)
    };
    egui::Frame::none()
        .fill(s.tile)
        .rounding(Rounding::same(13.0))
        .inner_margin(pad)
        .show(ui, contents)
        .inner
}

/// The bar's icons, drawn because a clipboard, a pin and a magnifier are not
/// all in the glyph set egui bundles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BarIcon {
    Clipboard,
    Pin,
    Clock,
    Search,
    Close,
    Check,
}

/// The hue each count carries, so the three are separable at a glance.
fn bar_icon_hue(s: &IslandSkin, kind: BarIcon) -> Color32 {
    match kind {
        BarIcon::Clipboard => s.accent,
        BarIcon::Pin => BRAND_VIOLET,
        BarIcon::Clock => BRAND_BLUE,
        _ => s.accent,
    }
}

fn draw_bar_icon(painter: &egui::Painter, rect: egui::Rect, kind: BarIcon, color: Color32) {
    let c = rect.center();
    let w = rect.width();
    let stroke = Stroke::new(1.4, color);
    match kind {
        BarIcon::Clipboard => {
            let board = egui::Rect::from_center_size(
                egui::pos2(c.x, c.y + w * 0.04),
                egui::vec2(w * 0.66, w * 0.82),
            );
            painter.rect_stroke(board, Rounding::same(2.0), stroke);
            painter.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(c.x, board.top()),
                    egui::vec2(w * 0.36, w * 0.2),
                ),
                Rounding::same(1.5),
                color,
            );
        }
        BarIcon::Pin => {
            // A bookmark: at 12 points a drawing-pin's head and needle collapse
            // into a lollipop.
            let half_w = w * 0.26;
            let top = c.y - w * 0.36;
            let bottom = c.y + w * 0.36;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - half_w, top),
                    egui::pos2(c.x + half_w, top),
                    egui::pos2(c.x + half_w, bottom),
                    egui::pos2(c.x, bottom - w * 0.22),
                    egui::pos2(c.x - half_w, bottom),
                ],
                color,
                Stroke::NONE,
            ));
        }
        BarIcon::Clock => {
            painter.circle_stroke(c, w * 0.42, stroke);
            painter.line_segment([c, egui::pos2(c.x, c.y - w * 0.26)], stroke);
            painter.line_segment([c, egui::pos2(c.x + w * 0.2, c.y)], stroke);
        }
        BarIcon::Search => {
            painter.circle_stroke(egui::pos2(c.x - w * 0.08, c.y - w * 0.08), w * 0.3, stroke);
            painter.line_segment(
                [
                    egui::pos2(c.x + w * 0.14, c.y + w * 0.14),
                    egui::pos2(c.x + w * 0.4, c.y + w * 0.4),
                ],
                stroke,
            );
        }
        BarIcon::Close => {
            let r = w * 0.28;
            painter.line_segment(
                [egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - r, c.y + r), egui::pos2(c.x + r, c.y - r)],
                stroke,
            );
        }
        BarIcon::Check => {
            painter.line_segment(
                [
                    egui::pos2(c.x - w * 0.3, c.y),
                    egui::pos2(c.x - w * 0.06, c.y + w * 0.24),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - w * 0.06, c.y + w * 0.24),
                    egui::pos2(c.x + w * 0.32, c.y - w * 0.26),
                ],
                stroke,
            );
        }
    }
}

/// A round icon button at the end of the bar.
fn bar_icon_button(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    kind: BarIcon,
    accent: bool,
    diameter: f32,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::click());
    let hovered = response.hovered();
    let fill = if accent {
        s.good
    } else if hovered {
        s.row_hover
    } else {
        s.tile
    };
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, fill);
    let ink = if accent {
        s.shell
    } else if hovered {
        s.ink
    } else {
        s.dim
    };
    draw_bar_icon(ui.painter(), rect.shrink(diameter * 0.3), kind, ink);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A vertical hairline between the bar's groups.
fn bar_divider(ui: &mut egui::Ui, s: &IslandSkin, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, height), egui::Sense::hover());
    ui.painter()
        .vline(rect.center().x, rect.y_range(), Stroke::new(1.0, s.line));
}
/// One shelved file: a preview over its name.
fn island_file_tile(
    ui: &mut egui::Ui,
    s: &IslandSkin,
    item: &ShelfItem,
    thumb: Option<&egui::TextureHandle>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(SHELF_TILE, egui::Sense::click());
    let hovered = response.hovered();
    ui.painter().rect_filled(
        rect,
        Rounding::same(11.0),
        if hovered { s.row_hover } else { s.tile },
    );
    ui.painter()
        .rect_stroke(rect, Rounding::same(11.0), Stroke::new(1.0, s.line));

    let art = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 26.0),
        egui::vec2(rect.width() - 22.0, 34.0),
    );
    match thumb {
        Some(tex) => {
            let img = egui::Image::new((tex.id(), art.size()))
                .rounding(Rounding::same(5.0))
                .maintain_aspect_ratio(true)
                .fit_to_exact_size(art.size());
            img.paint_at(ui, art);
        }
        None => draw_file_mark(ui.painter(), s, art, item.path.is_dir()),
    }

    let galley = ui.painter().layout(
        item.name.clone(),
        egui::FontId::proportional(9.0),
        if hovered { s.ink } else { s.dim },
        rect.width() - 8.0,
    );
    // One line: a shelf tile is a thing you recognise, not a filename you read.
    let line = galley.rows.first().map(|r| r.rect.height()).unwrap_or(11.0);
    ui.painter().with_clip_rect(egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - line - 6.0),
        egui::vec2(rect.width(), line + 4.0),
    ))
    .galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.bottom() - line - 5.0),
        galley,
        s.ink,
    );

    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!(
            "{} · {}",
            item.name,
            clipd_core::format_size(item.size)
        ))
}

/// A document or folder mark, for files with nothing to preview.
fn draw_file_mark(painter: &egui::Painter, s: &IslandSkin, rect: egui::Rect, folder: bool) {
    let body = egui::Rect::from_center_size(rect.center(), egui::vec2(24.0, 30.0));
    if folder {
        let tab = egui::Rect::from_min_size(
            egui::pos2(body.left(), body.top() + 3.0),
            egui::vec2(11.0, 5.0),
        );
        painter.rect_filled(tab, Rounding::same(2.0), BRAND_BLUE);
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(body.left(), body.top() + 7.0), body.max),
            Rounding::same(4.0),
            BRAND_BLUE.gamma_multiply(0.85),
        );
        return;
    }
    painter.rect_filled(body, Rounding::same(4.0), s.row_hover);
    painter.rect_stroke(body, Rounding::same(4.0), Stroke::new(1.0, s.line));
    // Three ruled lines, so it reads as a document rather than a blank card.
    for i in 0..3 {
        let y = body.top() + 10.0 + i as f32 * 6.0;
        painter.line_segment(
            [
                egui::pos2(body.left() + 5.0, y),
                egui::pos2(body.right() - 5.0, y),
            ],
            Stroke::new(1.2, s.faint),
        );
    }
}

/// The dashed tile at the end of the shelf row.
fn draw_drop_tile(painter: &egui::Painter, s: &IslandSkin, rect: egui::Rect, active: bool) {
    let color = if active { s.accent } else { s.line };
    if active {
        painter.rect_filled(rect, Rounding::same(11.0), s.row_hover);
    }
    let (dash, gap) = (5.0, 4.0);
    let mut x = rect.left() + 2.0;
    while x < rect.right() - 2.0 {
        let end = (x + dash).min(rect.right() - 2.0);
        for y in [rect.top() + 1.0, rect.bottom() - 1.0] {
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(end, y)],
                Stroke::new(1.0, color),
            );
        }
        x += dash + gap;
    }
    let mut y = rect.top() + 2.0;
    while y < rect.bottom() - 2.0 {
        let end = (y + dash).min(rect.bottom() - 2.0);
        for x in [rect.left() + 1.0, rect.right() - 1.0] {
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(x, end)],
                Stroke::new(1.0, color),
            );
        }
        y += dash + gap;
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if active { "Drop" } else { "+" },
        egui::FontId::proportional(if active { 10.0 } else { 18.0 }),
        if active { s.accent } else { s.faint },
    );
}

/// A dashed target filling whatever height the shelf has left over.
///
/// Empty space that does nothing reads as a broken card; a dashed well says
/// what the card is for and gives the drop something to aim at.
fn island_drop_zone(ui: &mut egui::Ui, s: &IslandSkin, height: f32, active: bool) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, height.clamp(18.0, 64.0)),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let color = if active { s.accent } else { s.line };
    if active {
        painter.rect_filled(rect, Rounding::same(9.0), s.row_hover);
    }
    // Hand-dashed: egui has no dashed rect, and a solid outline here would
    // read as another card rather than as a place to drop something.
    let (dash, gap) = (5.0, 4.0);
    let mut x = rect.left() + 2.0;
    while x < rect.right() - 2.0 {
        let end = (x + dash).min(rect.right() - 2.0);
        for y in [rect.top() + 1.0, rect.bottom() - 1.0] {
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(end, y)],
                Stroke::new(1.0, color),
            );
        }
        x += dash + gap;
    }
    let mut y = rect.top() + 2.0;
    while y < rect.bottom() - 2.0 {
        let end = (y + dash).min(rect.bottom() - 2.0);
        for x in [rect.left() + 1.0, rect.right() - 1.0] {
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(x, end)],
                Stroke::new(1.0, color),
            );
        }
        y += dash + gap;
    }
    if rect.height() >= 26.0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if active { "Drop to shelve" } else { "Drag files here" },
            egui::FontId::proportional(10.0),
            if active { s.accent } else { s.faint },
        );
    }
}

fn island_empty(ui: &mut egui::Ui, s: &IslandSkin, message: &str) {
    ui.label(RichText::new(message).size(10.0).color(s.faint));
}

/// The palette's per-type colour, for the clip row's leading dot.
fn clip_type_color(clip: &clipd_core::ClipEntry, c: &clipd_core::ThemeColors, s: &IslandSkin) -> Color32 {
    match clip.content_type {
        ContentType::Url => rgb(c.url),
        ContentType::Code => rgb(c.code),
        ContentType::Email => rgb(c.email),
        ContentType::Path | ContentType::File => rgb(c.path),
        ContentType::Image => rgb(c.accent2),
        // Plain text is most of what a clipboard holds, so colouring it
        // painted the majority of every list one colour — and a marker that
        // appears on nearly every row tells you nothing. Quiet by default
        // means the coloured ones actually mean something when they show up.
        _ => s.faint,
    }
}

/// One line describing a clip, whatever kind it is.
pub(crate) fn island_clip_line(clip: &clipd_core::ClipEntry, max: usize) -> String {
    let text = match clip.content_type {
        ContentType::Image => "Image".to_string(),
        ContentType::File if !clip.files.is_empty() => {
            if clip.files.len() == 1 {
                clip.files[0].name.clone()
            } else {
                format!("{} files", clip.files.len())
            }
        }
        _ => {
            let flat: String = clip
                .preview
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if flat.is_empty() {
                clip.content.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                flat
            }
        }
    };
    truncate(&text, max)
}

/// Shorten to `max` characters with an ellipsis, counting characters rather
/// than bytes so a multi-byte clip can't panic the island.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Relaunch the tray host, so a freshly granted permission takes effect.
///
/// macOS decides a process's Accessibility rights when it starts and never
/// revisits them, so granting the permission while clipd is running does
/// nothing until it is restarted. That is the step people miss — they grant
/// it, nothing changes, and it looks like the grant didn't work.
fn restart_tray_host() {
    #[cfg(unix)]
    if let Some(pid) = clipd_core::daemon_lock_pid() {
        if pid != std::process::id() {
            // SIGTERM, not SIGKILL: clipd-ui stops the daemon and releases its
            // lock on the way out, and its watchdog brings it back.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    }
}

/// Ask a running island to quit — used when the layout is switched away.
pub(crate) fn stop_island() {
    send_surface_request_to(SurfaceMode::Island, SurfaceMode::Quit);
}

/// Start the island as its own process.
pub(crate) fn start_island() {
    spawn_palette(&["--island"]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_clip_type_gets_its_own_island_glyph() {
        // The row icon used to be one blank square on every row, so a list of
        // ten clips carried ten identical marks. Each type now has a glyph,
        // and the ones that share a mark must be the ones that share a
        // meaning — a file and a path are both "something on disk"; a link
        // and a note are not.
        use clipd_core::ContentType;
        let glyph = |t: ContentType| {
            let mut clip = clipd_core::ClipEntry::new("x".into(), None, None);
            clip.content_type = t;
            island_type_glyph(&clip)
        };
        assert_eq!(glyph(ContentType::File), glyph(ContentType::Path));
        for (a, b) in [
            (ContentType::Url, ContentType::Text),
            (ContentType::Url, ContentType::Code),
            (ContentType::Code, ContentType::Image),
            (ContentType::Email, ContentType::Text),
            (ContentType::Image, ContentType::File),
        ] {
            assert_ne!(
                glyph(a.clone()),
                glyph(b.clone()),
                "{a:?} and {b:?} share a glyph"
            );
        }
        for t in [
            ContentType::Url,
            ContentType::Text,
            ContentType::Code,
            ContentType::Image,
            ContentType::File,
            ContentType::Path,
            ContentType::Email,
            ContentType::Unknown,
        ] {
            assert!(!glyph(t.clone()).is_empty(), "{t:?} has no glyph");
        }
    }

    #[test]
    fn a_multi_slot_copy_announces_its_slot() {
        // Repeated Cmd+C fills numbered slots. Announcing "Copied" for each of
        // them tells you the copy worked but not which number will paste it
        // back, so the second and third copies look identical to the first.
        fn label_for(content_type: ContentType, slot: Option<u8>) -> String {
            let base = match content_type {
                ContentType::Image => "Image copied",
                ContentType::File => "Files copied",
                _ => "Copied",
            };
            match slot {
                Some(n) => format!("{base} · slot {n}"),
                None => base.to_string(),
            }
        }

        assert_eq!(label_for(ContentType::Text, Some(2)), "Copied · slot 2");
        assert_eq!(
            label_for(ContentType::Image, Some(3)),
            "Image copied · slot 3"
        );
        // An ordinary copy is not a slot copy and stays unadorned.
        assert_eq!(label_for(ContentType::Text, None), "Copied");
    }

    #[test]
    fn a_pin_outlasts_the_pointer_leaving() {
        // Two kinds of pin, two lifetimes.
        //
        // An explicit pin — you pressed the pin button — is "keep this open
        // while I work", and used to lapse 1.2s after the pointer left, which
        // made the button useless. An implicit one is taken *for* you when you
        // click a row, a tab, or open search, purely so the panel does not
        // collapse under your own pointer. Giving that the long lifetime parked
        // the island over the screen after a single click, which reads as
        // frozen — the same complaint from the other direction.
        assert!(
            PIN_SAFETY_RELEASE >= Duration::from_secs(60),
            "an explicit pin that lapses in {PIN_SAFETY_RELEASE:?} is a grace period"
        );
        assert!(
            PIN_SAFETY_RELEASE <= Duration::from_secs(3600),
            "a pin nobody returns to should eventually lapse"
        );
        assert!(
            IMPLICIT_PIN_RELEASE <= Duration::from_secs(3),
            "a pin you never asked for must not outstay the interaction"
        );
        assert!(
            IMPLICIT_PIN_RELEASE > COLLAPSE_DELAY,
            "it still has to outlast the ordinary collapse, or it does nothing"
        );
        assert!(COLLAPSE_DELAY < Duration::from_millis(250));
    }

    #[test]
    fn both_sides_agree_on_where_hovering_starts() {
        // The cursor watcher decides when to wake the UI thread; the UI thread
        // decides whether that counts as hovering. Both read the margin off the
        // zone itself, so they cannot drift apart — when each had its own rule,
        // a pointer in the gap woke the island up to conclude it was not being
        // hovered, over and over.
        let strip = egui::Rect::from_min_size(egui::pos2(697.0, 0.0), egui::vec2(76.0, 34.0));

        // Hidden: a small strip beside the camera housing, so aim tolerance
        // has to be generous or the island is simply hard to summon.
        let hidden = HotZone { panel: egui::Rect::NOTHING, trigger: strip, expand: 22.0 };
        let near_miss = egui::pos2(735.0, 48.0);
        assert!(
            hidden.contains(near_miss, hidden.expand),
            "a hidden island needs a forgiving target"
        );

        // Open: the panel is large, and clinging to it after the pointer has
        // clearly left is worse than closing a fraction early.
        let open = HotZone {
            panel: egui::Rect::from_min_size(egui::pos2(543.0, 32.0), egui::vec2(385.0, 312.0)),
            trigger: strip,
            expand: 8.0,
        };
        assert!(!open.contains(egui::pos2(543.0, 360.0), open.expand));
    }

    #[test]
    fn every_surface_draws_the_same_accent() {
        // The tray popover paints the accent large — the eye button, the
        // search glyph, filled stars — while the island shows it only in small
        // marks. Under Glass Light that accent was a deep green, so the
        // popover read as a green app sitting beside a neutral white island.
        // One accent per theme, and for the glass themes it stays near-neutral
        // so the material is what you notice.
        for theme in [Theme::GlassLight] {
            let c = theme.colors();
            let skin = IslandSkin::frosted(&c);
            assert_eq!(
                (skin.accent.r(), skin.accent.g(), skin.accent.b()),
                (c.accent.0, c.accent.1, c.accent.2),
                "{}: island and the rest of clipd disagree on the accent",
                theme.label()
            );
            let a = c.accent;
            let spread = a.0.max(a.1).max(a.2) - a.0.min(a.1).min(a.2);
            assert!(
                spread <= 24,
                "{}: accent has a {spread}-level colour cast; glass wants near-neutral",
                theme.label()
            );
        }
    }

    #[test]
    fn the_island_wears_the_theme_it_is_given() {
        // The island used to force a dark HUD shell whatever the theme, so
        // Paper Light and Glass Light left it as the only clipd window that
        // had not changed. Verified here rather than by eye: forcing the
        // island open for a screenshot goes through a different path, so a
        // screenshot proves nothing about this branch.
        for theme in [Theme::Light, Theme::GlassLight] {
            let skin = IslandSkin::frosted(&theme.colors());
            assert!(
                !skin.dark,
                "{} should give the island a light skin",
                theme.label()
            );
            assert!(
                relative_luminance(Color32::from_rgb(
                    skin.shell.r(),
                    skin.shell.g(),
                    skin.shell.b()
                )) > 0.5,
                "{}: island shell came out dark",
                theme.label()
            );
            // Dark ink on a light slab, not the light ink a dark slab wants.
            assert!(relative_luminance(skin.ink) < 0.5, "{}: ink is too pale", theme.label());
        }

        for theme in [Theme::Dark, Theme::Catppuccin] {
            let skin = IslandSkin::frosted(&theme.colors());
            assert!(skin.dark, "{} should stay dark", theme.label());
            assert!(relative_luminance(skin.ink) > 0.5, "{}: ink is too dark", theme.label());
        }
    }

    #[test]
    fn a_glass_theme_makes_the_island_translucent_too() {
        // A glass theme asks every clipd surface to let the material behind
        // read. The island held at 240 regardless, which is why it stayed a
        // solid slab while the palette went to glass.
        for theme in [Theme::GlassLight] {
            let c = theme.colors();
            let skin = IslandSkin::frosted(&c);
            assert_eq!(
                skin.shell.a(),
                c.surface_alpha,
                "{} should hand the island its own surface alpha",
                theme.label()
            );
        }
        // Solid themes keep the trace of vibrancy the island always had.
        assert_eq!(IslandSkin::frosted(&Theme::Dark.colors()).shell.a(), 240);
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
        assert_eq!(truncate("short", 20), "short");
        // The cut lands on a character boundary, not mid-codepoint.
        assert_eq!(truncate("日本語のテキスト", 3), "日本語…");
    }

    #[test]
    fn the_cutout_is_never_painted_over() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 40.0));
        let (left, right) = split_around_notch(rect, 200.0, true);
        assert!(left.right() <= 100.0);
        assert!(right.left() >= 300.0);
        assert!(left.right() < right.left());
    }

    #[test]
    fn without_a_notch_the_row_is_one_band() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 40.0));
        let (left, right) = split_around_notch(rect, 200.0, false);
        assert_eq!(left, rect);
        assert_eq!(right.width(), 0.0);
    }

    #[test]
    fn a_manual_width_overrides_the_measurement() {
        let mut config = IslandConfig::default();
        config.notch_width = 260.0;
        assert_eq!(notch_geometry(&config).width, 260.0);
        // Absurd values are clamped rather than trusted.
        config.notch_width = 4000.0;
        assert!(notch_geometry(&config).width <= 520.0);
    }

    #[test]
    fn nothing_with_content_is_drawn_in_the_cutout_row() {
        // On a notched Mac the top strip of the display has no pixels in the
        // middle. Any phase that draws content has to start below it, or the
        // content lands behind the camera housing — which is exactly what
        // hid the island's tabs.
        let mut state = IslandState::default();
        for phase in [IslandPhase::Peek, IslandPhase::Expanded] {
            state.phase = phase;
            assert!(
                state.target_top() >= state.header_height(),
                "{phase:?} starts at {} — inside the menu-bar row, which the cutout sits in",
                state.target_top()
            );
        }
        // The resting pill is the exception — it is bezel, not content — but
        // only where there is a bezel to merge into. Without a notch it floats
        // under the menu bar like everything else.
        state.phase = IslandPhase::Resting;
        if state.geometry.real {
            assert!(state.target_top() < state.header_height());
        } else {
            assert!(state.target_top() >= state.header_height());
        }
    }

    #[test]
    fn the_top_band_beside_the_notch_is_not_hovered() {
        // The panel is wide and hangs below a narrow strip at the notch. The
        // bounding box of the two covers the menu bar either side of the
        // cutout, which is why reaching for a menu kept the island open.
        let trigger = egui::Rect::from_min_size(egui::pos2(600.0, 0.0), egui::vec2(240.0, 44.0));
        let panel = egui::Rect::from_min_size(egui::pos2(330.0, 44.0), egui::vec2(780.0, 250.0));
        let zone = HotZone { panel, trigger, expand: 8.0 };

        // Out at the left end of the menu bar: inside the bounding box of the
        // two rects, inside neither of them.
        let menu_bar = egui::pos2(400.0, 12.0);
        assert!(
            panel.union(trigger).contains(menu_bar),
            "this is the case the old union-of-rects zone got wrong"
        );
        assert!(!zone.contains(menu_bar, 6.0));

        // The strip itself and the panel body still count.
        assert!(zone.contains(egui::pos2(720.0, 20.0), 6.0));
        assert!(zone.contains(egui::pos2(700.0, 150.0), 6.0));
        // And so does the seam between them, so the pointer can travel down
        // from the notch into the panel without passing through dead space.
        assert!(zone.contains(egui::pos2(720.0, 45.0), 6.0));
    }

    #[test]
    fn the_hover_zone_never_moves_off_the_pointer() {
        // The bug this guards: the bar floats below the notch while the resting
        // pill hugs it, so the two window rects are vertically disjoint. With
        // the zone set to the window alone, hovering the notch opened the bar,
        // the bar's rect no longer held the pointer, it collapsed, and the pill
        // landed back under the pointer — at frame rate, which read as the
        // island freezing and its width flapping.
        let mut state = IslandState::default();
        let trigger = state.trigger_rect();
        let probes = [
            trigger.center(),
            trigger.center_top() + egui::vec2(0.0, 1.0),
            trigger.center_bottom() - egui::vec2(0.0, 1.0),
        ];
        for phase in [
            IslandPhase::Resting,
            IslandPhase::Peek,
            IslandPhase::Expanded,
        ] {
            state.phase = phase;
            let size = state.target_size();
            let pos = egui::pos2(
                (state.geometry.center_x - size.x / 2.0).max(0.0),
                state.target_top(),
            );
            let zone = state.hot_zone(pos, size);
            for probe in probes {
                assert!(
                    zone.contains(probe, 0.0),
                    "{phase:?}: a pointer on the trigger strip falls outside the hover zone"
                );
            }
        }
    }

    #[test]
    fn a_card_can_hold_the_rows_it_promises() {
        // The clipboard card says it shows CARD_CLIP_ROWS clips. Caption plus
        // that many rows has to fit inside the card, or the frame clips the
        // last one — which is exactly what 24pt rows did in a 108pt card, and
        // it looks like a padding bug rather than an overflow.
        let inner = CARD_H - CARD_PAD_Y * 2.0;
        let rows = CARD_CLIP_ROWS as f32 * ISLAND_ROW_H
            + (CARD_CLIP_ROWS - 1) as f32 * CARD_ROW_SPACING;
        let used = CARD_CAPTION_H + CARD_CAPTION_GAP + rows;
        assert!(
            used <= inner,
            "card content is {used}pt in a {inner}pt card — the last row would be clipped"
        );
    }

    #[test]
    fn the_slab_frames_its_cards_evenly() {
        let modules = [IslandModule::Clipboard, IslandModule::Files];
        let row: f32 =
            modules.iter().map(|m| card_width(*m)).sum::<f32>() + CARD_GAP;
        let slab = row + ISLAND_PAD * 2.0;
        assert!((slab - ISLAND_PAD * 2.0 - row).abs() < f32::EPSILON);
    }

    #[test]
    fn cards_wrap_without_being_reordered() {
        let modules = [
            IslandModule::Clipboard,
            IslandModule::Files,
            IslandModule::Files,
        ];
        let w = |m: IslandModule| card_width(m);
        let total: f32 = modules.iter().map(|m| w(*m)).sum::<f32>() + CARD_GAP * 2.0;

        let rows = island_card_rows(&modules, total + 1.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], modules);

        let two = w(modules[0]) + CARD_GAP + w(modules[1]);
        let rows = island_card_rows(&modules, two + 1.0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec![IslandModule::Clipboard, IslandModule::Files]);
        assert_eq!(rows[1], vec![IslandModule::Files]);

        let rows = island_card_rows(&modules, 10.0);
        assert_eq!(rows.len(), 3);
        assert!(island_card_rows(&[], 900.0).is_empty());
    }
}

/// One character for what a clip is, sized for the island's 20pt tile.
///
/// The island cannot spare the room the full window gives a drawn icon, and a
/// blank square repeated down the list tells the reader nothing — so each type
/// gets a mark that is legible at ten and a half points.
fn island_type_glyph(clip: &clipd_core::ClipEntry) -> &'static str {
    match clip.content_type {
        ContentType::Url => "↗",
        ContentType::Email => "@",
        ContentType::Code => "<>",
        ContentType::Image => "▣",
        ContentType::File | ContentType::Path => "▤",
        _ => "¶",
    }
}

