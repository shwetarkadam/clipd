#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use arboard::Clipboard;
use chrono::{DateTime, Utc};
use clipd_core::{
    available_targets, compute_sessions, detect_sensitive, load_actions, load_custom_colors,
    load_paste_transform_settings, load_privacy_config, load_theme, load_transform_config,
    paste_transforms, run_action, save_actions, save_custom_colors, save_paste_transform_settings,
    save_privacy_config, save_secret, save_theme, save_transform_config, ActionOutput,
    ActionsConfig, AskAnswer, AskConfig, AskFilters, AskThread, ClipEntry, ClipStore, ContentType,
    load_hotkey_status, CtrlSpaceAction, CustomAction, CustomColors, GuiLayout, HotkeyStatus,
    OpenGuiHotkey, PaletteTrigger, PasteTransformSettings, PrivacyConfig, Rgb, SecretEntry, Session,
    SessionConfig, TfIdfIndex, Theme, TransformConfig, TransformKind, VaultTarget,
};
use eframe::egui::{self, Color32, FontId, Margin, RichText, Rounding, Stroke};

mod island;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::time::{Duration, Instant};

/// Maximum clips to keep in memory in the GUI. Reduces RAM vs showing all clips.
const MAX_LOADED_CLIPS: usize = 200;

/// Compact spacing: tight rows, moderate rounding, a leading icon tile.
const CARD_ROUND: f32 = 10.0;
const CARD_PAD_X: f32 = 10.0;
const CARD_PAD_Y: f32 = 5.0;
/// Gap between rows in the list (mockup: ~8–10px between cards).
const ROW_GAP: f32 = 8.0;
/// Pill (tag) corner radius and padding.
const PILL_ROUND: f32 = 6.0;
const PILL_PAD_X: f32 = 7.0;
const PILL_PAD_Y: f32 = 2.0;
const SETTINGS_MAX_WIDTH: f32 = 740.0;
/// Window width while the Settings tab is showing — wide enough that endpoint
/// URLs and colour rows fit without truncating.
const SETTINGS_W: f32 = 780.0;
const SETTINGS_GUTTER_X: f32 = 16.0;
const SETTINGS_GUTTER_Y: f32 = 14.0;
// Compact palette by default (mockup proportions — tall, readable, not wide).
// Double-clicking a row expands to EXPANDED_W with the preview on the right.
const COMPACT_W: f32 = 600.0;
const EXPANDED_W: f32 = 980.0;
const WIN_H: f32 = 740.0;
const SHELL_ROUND: f32 = 18.0;

// ── Tray popover (HUD) ──
//
// Menu-bar clipboard popover that opens under the tray icon on hover/click.
const HUD_W: f32 = 380.0;
const HUD_H: f32 = 480.0;
/// Gap from the top of the screen. Must clear the macOS menu bar (~25pt): the
/// menu bar sits at a higher window level, so anything above this is drawn
/// behind it and simply never appears.
const HUD_TOP_MARGIN: f32 = 30.0;
/// Grace period before collapsing, so crossing a gap between chips or
/// overshooting the edge by a pixel doesn't slam the panel shut mid-read.
const HUD_COLLAPSE_DELAY: Duration = Duration::from_millis(200);
/// Keep the popover this far from the screen edges when the tray icon sits
/// near a corner.
const POPOVER_EDGE_PAD: f32 = 8.0;
/// Height the popover reserves for its footer: divider, padding, and the row
/// of 30pt buttons. Every view above the footer subtracts this, so it lives in
/// one place rather than being re-guessed per view.
/// Room for the footer's controls plus the hairline above them. Grew with the
/// buttons — at the old height a 38pt control had nowhere to sit.
const POPOVER_FOOTER_H: f32 = 58.0;
const PINNED_COLLECTION_NAME: &str = "Pinned";
const LEGACY_STARRED_COLLECTION_NAME: &str = "Starred";
fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

fn rgba(c: Rgb, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, alpha)
}

fn pill_bg(col: Color32) -> Color32 {
    Color32::from_rgb(
        (col.r() as u16 / 3 + 15).min(255) as u8,
        (col.g() as u16 / 3 + 15).min(255) as u8,
        (col.b() as u16 / 3 + 15).min(255) as u8,
    )
}

/// A boxed on/off setting row. Returns true if the value changed this frame.
///
/// Lives on the same row language as the tray popover: icon, title, detail,
/// switch. The card around a run of these is `settings_card`.
fn settings_toggle(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    value: &mut bool,
    title: &str,
    subtitle: &str,
) -> bool {
    settings_toggle_row(ui, c, FooterIcon::Gear, value, title, subtitle)
}

/// Quiet section label — kept for the unused settings window and pairing copy.
fn settings_caption(ui: &mut egui::Ui, c: &clipd_core::ThemeColors, text: &str, note: &str) {
    settings_section(ui, c, text);
    if !note.is_empty() {
        ui.label(RichText::new(note).size(10.5).color(rgb(c.subtext)));
        ui.add_space(4.0);
    }
}

/// One labeled swatch row for the custom-palette editor. Returns true if the
/// user changed the color this frame.
fn color_row(ui: &mut egui::Ui, c: &clipd_core::ThemeColors, label: &str, val: &mut Rgb) -> bool {
    let mut arr = [val.0, val.1, val.2];
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui.color_edit_button_srgb(&mut arr).changed() {
            *val = Rgb(arr[0], arr[1], arr[2]);
            changed = true;
        }
        ui.add_space(4.0);
        ui.label(RichText::new(label).size(12.0).color(rgb(c.text)));
    });
    changed
}

/// Draw a magnifier icon into a fixed slot in the current layout. Vector-drawn
/// so it always renders (the `⌕`/`🔍` glyphs are missing in egui's font → tofu).
// ── Glass affordances ──
//
// macOS materials are translucent over their backdrop with a hairline edge
// catching the light. egui has no blur, so the illusion comes from a
// low-alpha fill plus a brighter top-edge stroke — enough that these read as
// floating *over* the list rather than as flat buttons stamped into it.

/// A compact circular control used by the bottom command bar and contextual
/// row actions. Keeping these icon-only makes the palette feel like a utility,
/// not a toolbar-heavy application.
/// Left edge for a popover of `width`, sitting under the menu-bar extra.
///
/// The card is centred on the cat icon so the tail points at it. It is only
/// nudged inward when that would run off a screen edge — never parked against
/// the right of the display just because the extra is not in a guessed zone.
fn popover_left(width: f32, screen: egui::Vec2, anchored: bool) -> f32 {
    let max_left = (screen.x - width - POPOVER_EDGE_PAD).max(POPOVER_EDGE_PAD);
    let anchor = if anchored {
        clipd_core::load_tray_anchor().map(|x| x as f32)
    } else {
        None
    };
    match anchor {
        Some(x) => (x - width * 0.5).clamp(POPOVER_EDGE_PAD, max_left),
        None => max_left,
    }
}

/// Launch another copy of this binary as the full palette window.
///
/// The popover deliberately stays small; anything that needs room (settings,
/// the full history) opens the real window rather than growing the tray panel
/// into a second application.
fn spawn_palette(args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// One settings row inside the popover: title, explanation, optional state
/// pill on the right. Returns true when clicked.
///
/// `state: None` marks an action (it navigates or quits); `Some(bool)` marks a
/// toggle and shows On/Off, so the two never look alike.
/// A small pill action, ranked so a row's three verbs are not all equal.
///
/// `Copy` is what you came for, `Reveal` is a peek, `Forget` destroys
/// something — three identical grey rectangles said none of that.
fn vault_action(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    label: &str,
    primary: bool,
    destructive: bool,
) -> egui::Response {
    let (fill, text, border) = if primary {
        (
            rgb(c.accent).gamma_multiply(0.22),
            rgb(c.accent),
            rgb(c.accent).gamma_multiply(0.5),
        )
    } else if destructive {
        (
            Color32::TRANSPARENT,
            rgb(c.subtext),
            rgb(c.border).gamma_multiply(0.7),
        )
    } else {
        (
            surf(c, c.bg_elevated),
            rgb(c.text),
            rgb(c.border).gamma_multiply(0.8),
        )
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).color(text))
            .fill(fill)
            .stroke(Stroke::new(0.9, border))
            .rounding(Rounding::same(7.0))
            .min_size(egui::vec2(0.0, 26.0)),
    )
}

/// Trailing control on a settings row.
#[derive(Clone, Copy, PartialEq)]
enum RowControl {
    Toggle(bool),
    Chevron,
}

/// A small-caps section heading, as the reference groups its settings.
fn popover_section_header(ui: &mut egui::Ui, c: &clipd_core::ThemeColors, text: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .size(10.5)
            .color(rgb(c.overlay))
            .strong(),
    );
    ui.add_space(5.0);
}

/// One settings row: icon tile, title over subtitle, and a control on the
/// right — a switch for something you turn on, a chevron for somewhere you go.
///
/// The rows used to be individually bordered cards with the word "On" or "Off"
/// as their only control, which reads as a list of labels rather than as
/// settings you operate.
fn popover_setting_row(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    icon: FooterIcon,
    title: &str,
    detail: &str,
    control: RowControl,
) -> bool {
    let height = 56.0;
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, Rounding::same(10.0), surf(c, c.bg_hover));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Icon tile.
    let tile = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 34.0, rect.center().y),
        egui::vec2(38.0, 38.0),
    );
    let lit = matches!(control, RowControl::Toggle(true));
    painter.rect_filled(
        tile,
        Rounding::same(10.0),
        if lit {
            rgb(c.accent).gamma_multiply(0.20)
        } else {
            surf(c, c.bg_elevated)
        },
    );
    paint_footer_icon(
        painter,
        egui::Rect::from_center_size(tile.center(), egui::vec2(18.0, 18.0)),
        icon,
        if lit { rgb(c.accent) } else { rgb(c.subtext) },
    );

    let text_left = rect.left() + 62.0;
    painter.text(
        egui::pos2(text_left, rect.center().y - 9.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.5),
        rgb(c.text),
    );
    painter.text(
        egui::pos2(text_left, rect.center().y + 9.0),
        egui::Align2::LEFT_CENTER,
        detail,
        egui::FontId::proportional(11.0),
        rgb(c.subtext),
    );

    match control {
        RowControl::Toggle(on) => {
            let track = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 34.0, rect.center().y),
                egui::vec2(42.0, 24.0),
            );
            painter.rect_filled(
                track,
                Rounding::same(12.0),
                if on {
                    rgb(c.accent)
                } else {
                    surf(c, c.bg_selected)
                },
            );
            let knob = if on {
                track.right() - 12.0
            } else {
                track.left() + 12.0
            };
            painter.circle_filled(
                egui::pos2(knob, track.center().y),
                9.0,
                if on { rgb(c.bg_base) } else { rgb(c.subtext) },
            );
        }
        RowControl::Chevron => {
            let x = rect.right() - 26.0;
            let y = rect.center().y;
            let col = rgb(c.overlay);
            for (dx, dy) in [(-4.0_f32, -5.0_f32), (-4.0, 5.0)] {
                painter.line_segment(
                    [egui::pos2(x + dx, y + dy), egui::pos2(x, y)],
                    Stroke::new(1.8, col),
                );
            }
        }
    }
    response.clicked()
}

/// Small-caps section heading used by the full Settings pages.
fn settings_section(ui: &mut egui::Ui, c: &clipd_core::ThemeColors, text: &str) {
    if ui.cursor().top() > ui.max_rect().top() + 6.0 {
        ui.add_space(14.0);
    } else {
        ui.add_space(2.0);
    }
    ui.label(
        RichText::new(text.to_uppercase())
            .size(10.5)
            .color(rgb(c.overlay))
            .strong()
            .extra_letter_spacing(0.8),
    );
    ui.add_space(5.0);
}

/// The grouped card every Settings section sits in — same language as the
/// tray popover: raised surface, 12pt corners, a quiet border.
fn settings_card<R>(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let inner = egui::Frame::none()
        .fill(surf(c, c.bg_surface))
        .rounding(Rounding::same(12.0))
        .stroke(Stroke::new(0.7, rgb(c.border).gamma_multiply(0.8)))
        .inner_margin(Margin::symmetric(6.0, 4.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
    ui.add_space(2.0);
    inner.inner
}

/// Hairline between rows inside a card. Inset so it does not run under the
/// icon tile or out to the card's rounded corners.
fn settings_card_divider(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    let left = rect.left() + 56.0;
    let right = rect.right() - 8.0;
    if left < right {
        ui.painter().hline(
            left..=right,
            rect.center().y,
            Stroke::new(1.0, rgb(c.border).gamma_multiply(0.45)),
        );
    }
}

/// Padding for freeform content (fields, lists, notes) sitting inside a card.
fn settings_card_body(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.vertical(|ui| {
            let w = (ui.available_width() - 8.0).max(80.0);
            ui.set_width(w);
            add(ui);
        });
    });
    ui.add_space(6.0);
}

fn settings_card_copy(ui: &mut egui::Ui, c: &clipd_core::ThemeColors, title: &str, note: &str) {
    settings_card_body(ui, |ui| {
        ui.label(RichText::new(title).size(13.0).color(rgb(c.text)));
        if !note.is_empty() {
            ui.add_space(2.0);
            ui.label(RichText::new(note).size(11.0).color(rgb(c.subtext)));
        }
    });
}

/// The letter-slot key bindings, written out where the toggle for them lives.
///
/// These are chords with no discoverable surface — nothing in the UI hints
/// that Ctrl+Option+C is a leader key, and a feature you cannot find is a
/// feature nobody uses. The two platforms genuinely differ: macOS binds the
/// chords directly, while Windows deliberately does not, because Win/Ctrl/Alt
/// letter combinations collide with OS shortcuts, browser menus, AltGr layouts
/// and app accelerators. So Windows routes A–Z through one leader instead.
fn letter_slot_bindings() -> &'static [(&'static str, &'static str)] {
    #[cfg(target_os = "windows")]
    {
        &[
            ("Ctrl+`  then a letter", "Paste that letter slot"),
            ("Ctrl+`  then Shift+letter", "Save the clipboard to it"),
            ("Ctrl+C ×N", "Numeric slots 1–9 by tap count"),
        ]
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Shortest first. The two-key path is the one worth learning, and it
        // was buried under a four-key chord that looks like the main way in.
        //
        // Copy has no three-key form because Ctrl+Option+letter is already
        // paste, and Cmd+Option+letter is claimed across macOS — ⌘⌥I opens web
        // inspectors, ⌘⌥J consoles. Taking those would break the apps clipd is
        // used inside, which is why copy carries the extra Shift.
        // Cmd+Option+C / V, then the letter. Cmd suppresses Option's character
        // composition, so these emit nothing even if a swallow is missed —
        // which is what ruled out plain Option+C / Option+V (ç and √), on top
        // of those already addressing the extended 11–30 bank.
        //
        // Cmd+Option+V used to be batch-drain paste; that moved to
        // Cmd+Option+Shift+V rather than being quietly overridden.
        &[
            ("Cmd+Option+C  then a letter", "Copy to that letter slot"),
            ("Cmd+Option+V  then a letter", "Paste that letter slot"),
            ("Ctrl+Option+letter", "Paste — one chord, no timing"),
            ("Ctrl+Shift+Option+letter", "Copy — one chord, no timing"),
        ]
    }
}

/// A read-only row that names a shortcut and says what it does.
fn settings_shortcut_help(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    keys: &str,
    what: &str,
) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::hover());
    let painter = ui.painter();
    // The chord in a monospace pill, so it reads as keys rather than prose.
    let galley = painter.layout_no_wrap(
        keys.to_string(),
        egui::FontId::monospace(11.0),
        rgb(c.text),
    );
    let pill = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 14.0, rect.center().y - 11.0),
        egui::vec2(galley.size().x + 16.0, 22.0),
    );
    painter.rect_filled(pill, Rounding::same(6.0), surf(c, c.bg_elevated));
    painter.text(
        pill.center(),
        egui::Align2::CENTER_CENTER,
        keys,
        egui::FontId::monospace(11.0),
        rgb(c.text),
    );
    painter.text(
        egui::pos2(pill.right() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        what,
        egui::FontId::proportional(11.0),
        rgb(c.subtext),
    );
}

fn settings_toggle_row(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    icon: FooterIcon,
    value: &mut bool,
    title: &str,
    detail: &str,
) -> bool {
    if popover_setting_row(ui, c, icon, title, detail, RowControl::Toggle(*value)) {
        *value = !*value;
        true
    } else {
        false
    }
}

/// Shared chrome for a settings row: icon tile, title, detail. Returns the
/// allocated rect so the caller can paint a trailing control into it.
fn settings_pref_shell(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    icon: FooterIcon,
    title: &str,
    detail: &str,
    lit: bool,
    clickable: bool,
) -> (egui::Rect, egui::Response) {
    let height = 56.0;
    let width = ui.available_width();
    let sense = if clickable {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), sense);
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, Rounding::same(10.0), surf(c, c.bg_hover));
        if clickable {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    let tile = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 34.0, rect.center().y),
        egui::vec2(38.0, 38.0),
    );
    painter.rect_filled(
        tile,
        Rounding::same(10.0),
        if lit {
            rgb(c.accent).gamma_multiply(0.20)
        } else {
            surf(c, c.bg_elevated)
        },
    );
    paint_footer_icon(
        painter,
        egui::Rect::from_center_size(tile.center(), egui::vec2(18.0, 18.0)),
        icon,
        if lit { rgb(c.accent) } else { rgb(c.subtext) },
    );
    let text_left = rect.left() + 62.0;
    if detail.is_empty() {
        painter.text(
            egui::pos2(text_left, rect.center().y),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(13.5),
            rgb(c.text),
        );
    } else {
        painter.text(
            egui::pos2(text_left, rect.center().y - 9.0),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(13.5),
            rgb(c.text),
        );
        painter.text(
            egui::pos2(text_left, rect.center().y + 9.0),
            egui::Align2::LEFT_CENTER,
            detail,
            egui::FontId::proportional(11.0),
            rgb(c.subtext),
        );
    }
    (rect, response)
}

/// A settings row whose trailing side is a combo, slider, or other widget.
fn settings_value_row(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    icon: FooterIcon,
    title: &str,
    detail: &str,
    trailing_w: f32,
    add_trailing: impl FnOnce(&mut egui::Ui),
) {
    let (rect, _) = settings_pref_shell(ui, c, icon, title, detail, false, false);
    let trail = egui::Rect::from_min_size(
        egui::pos2((rect.right() - trailing_w - 10.0).max(rect.left() + 160.0), rect.top()),
        egui::vec2(trailing_w.min(rect.width() - 170.0).max(80.0), rect.height()),
    );
    ui.allocate_ui_at_rect(trail, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add_trailing);
    });
}

/// The line icons the footer draws instead of emoji.
///
/// Emoji arrive with their own weight, colour and baseline — a mixed set of
/// them under a list of clips reads as clip art, not as controls, which is
/// exactly what looked wrong however the buttons were sized. These are drawn
/// from strokes at one weight so the row is consistent.
#[derive(Clone, Copy, PartialEq)]
enum FooterIcon {
    Sparkle,
    List,
    Gear,
    Eye,
    Power,
    Clipboard,
    Lock,
    Send,
    Window,
    Shield,
    Key,
    Palette,
    Keyboard,
    Sliders,
    App,
}

fn paint_footer_icon(painter: &egui::Painter, rect: egui::Rect, icon: FooterIcon, col: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.5;
    let w = 1.9;
    let stroke = Stroke::new(w, col);
    match icon {
        FooterIcon::Sparkle => {
            // A four-point star: two crossed strokes with the diagonals short.
            for (dx, dy, len) in [(0.0, 1.0, 0.95), (1.0, 0.0, 0.95)] {
                painter.line_segment(
                    [
                        egui::pos2(c.x - dx * r * len, c.y - dy * r * len),
                        egui::pos2(c.x + dx * r * len, c.y + dy * r * len),
                    ],
                    stroke,
                );
            }
            for (dx, dy) in [(0.7, 0.7), (0.7, -0.7)] {
                painter.line_segment(
                    [
                        egui::pos2(c.x - dx * r * 0.55, c.y - dy * r * 0.55),
                        egui::pos2(c.x + dx * r * 0.55, c.y + dy * r * 0.55),
                    ],
                    Stroke::new(w * 0.75, col),
                );
            }
        }
        FooterIcon::List => {
            for i in -1..=1 {
                let y = c.y + i as f32 * r * 0.52;
                painter.line_segment(
                    [egui::pos2(c.x - r * 0.72, y), egui::pos2(c.x + r * 0.72, y)],
                    stroke,
                );
            }
        }
        FooterIcon::Gear => {
            // Big ring, short teeth. Long spokes on a small hub read as a
            // sunburst, and a small ring with tiny teeth reads as a smudge —
            // the same balance the island's gear needed.
            painter.circle_stroke(c, r * 0.60, stroke);
            painter.circle_filled(c, r * 0.20, col);
            for i in 0..6 {
                let a = i as f32 * std::f32::consts::PI / 3.0;
                let (sin, cos) = a.sin_cos();
                painter.line_segment(
                    [
                        egui::pos2(c.x + cos * r * 0.58, c.y + sin * r * 0.58),
                        egui::pos2(c.x + cos * r * 0.92, c.y + sin * r * 0.92),
                    ],
                    Stroke::new(w * 1.2, col),
                );
            }
        }
        FooterIcon::Clipboard => {
            let (hw, hh) = (r * 0.52, r * 0.72);
            painter.rect_stroke(
                egui::Rect::from_center_size(
                    egui::pos2(c.x, c.y + r * 0.08),
                    egui::vec2(hw * 2.0, hh * 2.0),
                ),
                Rounding::same(2.5),
                stroke,
            );
            // The clip at the top, drawn wider than the board's shoulder.
            painter.rect_stroke(
                egui::Rect::from_center_size(
                    egui::pos2(c.x, c.y - r * 0.62),
                    egui::vec2(hw * 1.1, r * 0.38),
                ),
                Rounding::same(1.5),
                Stroke::new(w * 0.9, col),
            );
        }
        FooterIcon::Lock => {
            let body = egui::Rect::from_center_size(
                egui::pos2(c.x, c.y + r * 0.28),
                egui::vec2(r * 1.30, r * 0.95),
            );
            painter.rect_stroke(body, Rounding::same(2.5), stroke);
            // Shackle: a half-arc rising out of the body.
            let steps = 12;
            let pts: Vec<egui::Pos2> = (0..=steps)
                .map(|i| {
                    let a = std::f32::consts::PI * (i as f32 / steps as f32);
                    egui::pos2(
                        c.x - a.cos() * r * 0.44,
                        body.top() - a.sin() * r * 0.52,
                    )
                })
                .collect();
            for pair in pts.windows(2) {
                painter.line_segment([pair[0], pair[1]], Stroke::new(w * 0.9, col));
            }
        }
        FooterIcon::Power => {
            // An arc with a gap at the top, and the stem rising through it.
            // Drawing a closed circle with a line poking into it — which is
            // what this was — reads as a clock hand, not a power symbol.
            let steps = 22;
            let (start, sweep) = (-std::f32::consts::FRAC_PI_2 + 0.62, std::f32::consts::TAU - 1.24);
            let pts: Vec<egui::Pos2> = (0..=steps)
                .map(|i| {
                    let a = start + sweep * (i as f32 / steps as f32);
                    let (sin, cos) = a.sin_cos();
                    egui::pos2(c.x + cos * r * 0.78, c.y + sin * r * 0.78)
                })
                .collect();
            for pair in pts.windows(2) {
                painter.line_segment([pair[0], pair[1]], stroke);
            }
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y - r * 0.92),
                    egui::pos2(c.x, c.y - r * 0.18),
                ],
                stroke,
            );
        }
        FooterIcon::Eye => {
            // Two arcs meeting at the corners, approximated by short chords.
            let (hw, hh) = (r * 0.85, r * 0.5);
            let steps = 10;
            for sign in [1.0_f32, -1.0] {
                let pts: Vec<egui::Pos2> = (0..=steps)
                    .map(|i| {
                        let t = i as f32 / steps as f32 * 2.0 - 1.0;
                        egui::pos2(c.x + t * hw, c.y + sign * (1.0 - t * t) * hh)
                    })
                    .collect();
                for pair in pts.windows(2) {
                    painter.line_segment([pair[0], pair[1]], stroke);
                }
            }
            painter.circle_stroke(c, r * 0.24, Stroke::new(w, col));
        }
        FooterIcon::Send => {
            // Arrow pointing up-right out of a tray.
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.15, c.y + r * 0.20),
                    egui::pos2(c.x + r * 0.55, c.y - r * 0.55),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x + r * 0.08, c.y - r * 0.55),
                    egui::pos2(c.x + r * 0.55, c.y - r * 0.55),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x + r * 0.55, c.y - r * 0.08),
                    egui::pos2(c.x + r * 0.55, c.y - r * 0.55),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.72, c.y + r * 0.55),
                    egui::pos2(c.x + r * 0.20, c.y + r * 0.55),
                ],
                stroke,
            );
        }
        FooterIcon::Window => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(r * 1.55, r * 1.20)),
                Rounding::same(2.5),
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.78, c.y - r * 0.22),
                    egui::pos2(c.x + r * 0.78, c.y - r * 0.22),
                ],
                stroke,
            );
        }
        FooterIcon::Shield => {
            let pts = [
                egui::pos2(c.x, c.y - r * 0.88),
                egui::pos2(c.x + r * 0.72, c.y - r * 0.42),
                egui::pos2(c.x + r * 0.62, c.y + r * 0.28),
                egui::pos2(c.x, c.y + r * 0.88),
                egui::pos2(c.x - r * 0.62, c.y + r * 0.28),
                egui::pos2(c.x - r * 0.72, c.y - r * 0.42),
            ];
            for i in 0..pts.len() {
                painter.line_segment([pts[i], pts[(i + 1) % pts.len()]], stroke);
            }
        }
        FooterIcon::Key => {
            painter.circle_stroke(
                egui::pos2(c.x - r * 0.38, c.y),
                r * 0.38,
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y),
                    egui::pos2(c.x + r * 0.82, c.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x + r * 0.82, c.y),
                    egui::pos2(c.x + r * 0.82, c.y + r * 0.32),
                ],
                Stroke::new(w * 0.9, col),
            );
            painter.line_segment(
                [
                    egui::pos2(c.x + r * 0.52, c.y),
                    egui::pos2(c.x + r * 0.52, c.y + r * 0.22),
                ],
                Stroke::new(w * 0.9, col),
            );
        }
        FooterIcon::Palette => {
            painter.circle_stroke(c, r * 0.78, stroke);
            painter.circle_filled(egui::pos2(c.x - r * 0.28, c.y - r * 0.18), r * 0.16, col);
            painter.circle_filled(egui::pos2(c.x + r * 0.22, c.y - r * 0.22), r * 0.14, col);
            painter.circle_filled(egui::pos2(c.x + r * 0.08, c.y + r * 0.28), r * 0.14, col);
        }
        FooterIcon::Keyboard => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(r * 1.70, r * 1.10)),
                Rounding::same(2.5),
                stroke,
            );
            for i in 0..3 {
                let x = c.x - r * 0.42 + i as f32 * r * 0.42;
                painter.circle_filled(egui::pos2(x, c.y - r * 0.12), r * 0.10, col);
            }
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.42, c.y + r * 0.22),
                    egui::pos2(c.x + r * 0.42, c.y + r * 0.22),
                ],
                Stroke::new(w * 0.9, col),
            );
        }
        FooterIcon::Sliders => {
            for (i, h) in [0.55_f32, 0.85, 0.40].iter().enumerate() {
                let x = c.x + (i as f32 - 1.0) * r * 0.55;
                painter.line_segment(
                    [
                        egui::pos2(x, c.y - r * h),
                        egui::pos2(x, c.y + r * 0.78),
                    ],
                    stroke,
                );
                painter.circle_filled(egui::pos2(x, c.y - r * (h - 0.22)), r * 0.18, col);
            }
        }
        FooterIcon::App => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(r * 1.35, r * 1.35)),
                Rounding::same(4.0),
                stroke,
            );
            painter.circle_filled(
                egui::pos2(c.x - r * 0.22, c.y - r * 0.18),
                r * 0.16,
                col,
            );
        }
    }
}

/// An icon button that paints its glyph rather than rendering a character.
fn glass_line_button(
    ui: &mut egui::Ui,
    icon: FooterIcon,
    active: bool,
    c: &clipd_core::ThemeColors,
) -> egui::Response {
    let size = egui::vec2(38.0, 38.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = response.hovered();
    let (fill, col) = if active {
        (rgb(c.accent).gamma_multiply(0.20), rgb(c.accent))
    } else if hovered {
        (rgb(c.bg_hover), rgb(c.text))
    } else {
        (rgb(c.bg_elevated), rgb(c.subtext))
    };
    ui.painter().circle_filled(rect.center(), size.x * 0.5, fill);
    ui.painter().circle_stroke(
        rect.center(),
        size.x * 0.5,
        Stroke::new(1.0, rgb(c.border).gamma_multiply(0.7)),
    );
    paint_footer_icon(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), egui::vec2(17.0, 17.0)),
        icon,
        col,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn glass_icon_button(
    ui: &mut egui::Ui,
    icon: &str,
    active: bool,
    c: &clipd_core::ThemeColors,
) -> egui::Response {
    glass_icon_button_colored(ui, icon, active, c, rgb(c.accent))
}

/// Pin/star control — uses the theme's spot `green` so concept themes keep
/// their chrome neutral while the mark still matches the mockups.
fn glass_pin_button(
    ui: &mut egui::Ui,
    pinned: bool,
    c: &clipd_core::ThemeColors,
) -> egui::Response {
    glass_icon_button_colored(
        ui,
        if pinned { "★" } else { "☆" },
        pinned,
        c,
        rgb(c.green),
    )
}

fn glass_icon_button_colored(
    ui: &mut egui::Ui,
    icon: &str,
    active: bool,
    c: &clipd_core::ThemeColors,
    active_col: Color32,
) -> egui::Response {
    let (fill, stroke, text) = if active {
        (
            pill_bg(active_col),
            active_col.gamma_multiply(0.62),
            active_col,
        )
    } else {
        (surf(c, c.bg_elevated), rgb(c.border), rgb(c.subtext))
    };
    let response = ui.add(
        egui::Button::new(RichText::new(icon).size(16.0).color(text))
            .fill(fill)
            .rounding(Rounding::same(999.0))
            .stroke(Stroke::new(0.75, stroke))
            // Bigger, rounder targets. At 30pt these read as small chips
            // crowded into the corner; the reference sits closer to 38 with
            // real space between them, which is what makes a footer look like
            // a set of controls rather than leftovers under the list.
            .min_size(egui::vec2(38.0, 38.0)),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A frosted pill button. `active` fills it with the accent instead.
fn glass_chip(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    active: bool,
    enabled: bool,
    c: &clipd_core::ThemeColors,
) -> egui::Response {
    let text = if icon.is_empty() {
        label.to_string()
    } else if label.is_empty() {
        icon.to_string()
    } else {
        format!("{}  {}", icon, label)
    };

    let (fill, stroke_col, text_col) = if active {
        (
            pill_bg(rgb(c.accent)),
            rgb(c.accent).gamma_multiply(0.62),
            rgb(c.text),
        )
    } else if enabled {
        (surf(c, c.bg_elevated), rgb(c.border), rgb(c.subtext))
    } else {
        (surf(c, c.bg_surface), rgb(c.border), rgb(c.overlay))
    };

    let button = egui::Button::new(RichText::new(text).size(11.0).color(text_col))
        .fill(fill)
        .rounding(Rounding::same(999.0))
        .stroke(Stroke::new(0.8, stroke_col))
        .min_size(egui::vec2(0.0, 22.0));

    let resp = ui.add_enabled(enabled, button);
    if resp.hovered() && enabled {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// A selectable card for one GUI layout. Returns true when clicked.
///
/// Two cards rather than a combo: the choice changes what clipd *is* on screen,
/// which deserves more than a dropdown row the eye slides past.
fn layout_card(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    layout: GuiLayout,
    selected: bool,
) -> bool {
    // Content width, not card width: the frame adds its own 12pt margins on
    // each side, and forgetting them made the second card wrap onto its own
    // line at every window size.
    const CARD_MARGIN: f32 = 12.0;
    const CARD_GAP: f32 = 8.0;
    let width = ((ui.available_width() - CARD_GAP) / 2.0 - CARD_MARGIN * 2.0 - 2.0)
        .clamp(150.0, 320.0);
    let mut clicked = false;
    let response = egui::Frame::none()
        .fill(if selected {
            surf(c, c.bg_selected)
        } else {
            surf(c, c.bg_elevated)
        })
        .rounding(Rounding::same(14.0))
        .stroke(Stroke::new(
            if selected { 1.6 } else { 0.7 },
            if selected { rgb(c.accent) } else { rgb(c.border) },
        ))
        .inner_margin(Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 3.0;
            let (preview, _) =
                ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::hover());
            draw_layout_preview(ui.painter(), preview, layout, c);
            ui.add_space(3.0);
            ui.add(
                egui::Label::new(
                    RichText::new(layout.label())
                        .size(12.5)
                        .strong()
                        .color(rgb(c.text)),
                )
                .selectable(false),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(layout.detail()).size(10.5).color(rgb(c.subtext)),
                )
                .selectable(false),
            );
        })
        .response;
    let hit = ui.interact(
        response.rect,
        egui::Id::new(("layout_card", layout.label())),
        egui::Sense::click(),
    );
    if hit.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        clicked = true;
    }
    clicked
}

/// A tiny wireframe of each layout, so the difference is visible before the
/// switch rather than after it.
fn draw_layout_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    layout: GuiLayout,
    c: &clipd_core::ThemeColors,
) {
    let screen = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width() * 0.72, 30.0));
    painter.rect_stroke(screen, Rounding::same(4.0), Stroke::new(1.0, rgb(c.border)));
    match layout {
        GuiLayout::Palette => {
            // A card floating in the middle of the display.
            let card = egui::Rect::from_center_size(
                screen.center(),
                egui::vec2(screen.width() * 0.42, 18.0),
            );
            painter.rect_filled(card, Rounding::same(3.0), rgb(c.accent));
        }
        GuiLayout::Notch => {
            // A slab hanging off the top edge, notch-shaped.
            let slab = egui::Rect::from_min_size(
                egui::pos2(screen.center().x - screen.width() * 0.16, screen.top()),
                egui::vec2(screen.width() * 0.32, 7.0),
            );
            painter.rect_filled(
                slab,
                Rounding {
                    nw: 0.0,
                    ne: 0.0,
                    sw: 3.0,
                    se: 3.0,
                },
                rgb(c.accent),
            );
        }
    }
}

fn draw_search_icon(ui: &mut egui::Ui, col: Color32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(13.0, 16.0), egui::Sense::hover());
    let center = egui::pos2(r.left() + 5.5, r.center().y - 0.5);
    let stroke = Stroke::new(1.4, col);
    ui.painter().circle_stroke(center, 4.0, stroke);
    ui.painter().line_segment(
        [
            egui::pos2(center.x + 3.0, center.y + 3.0),
            egui::pos2(center.x + 6.5, center.y + 6.5),
        ],
        stroke,
    );
}

/// The brand mark: clipd's cat, the same art the island shows.
///
/// This was a lettered tile — a coloured square with a "C" in it, which is
/// what an app uses when it has no mark of its own. clipd has one, and the
/// island was already wearing it while this window introduced the product as
/// a different app entirely.
fn draw_brand_mark(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 22.0), egui::Sense::hover());
    draw_clipd_mark(ui, rect, rgb(c.accent));
}

/// Source-app tile: muted rounded square with the app's initial.
fn draw_source_tile(ui: &mut egui::Ui, source: &str, c: &clipd_core::ThemeColors) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::same(8.0), surf(c, c.bg_selected));
    let letter = source
        .chars()
        .find(|ch| ch.is_alphanumeric())
        .unwrap_or('·')
        .to_ascii_uppercase()
        .to_string();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        letter,
        FontId::proportional(12.0),
        rgb(c.subtext),
    );
}


/// The row's leading glyph: what the clip *is*, drawn as a bare line icon.
///
/// It used to be a letter — the initial of the app it came from — inside a
/// filled rounded tile. The letter repeated what the meta line already says,
/// and the tile made a plain list look like a grid of buttons. The reference
/// draws the glyph alone, with nothing behind it.
fn draw_type_tile(
    ui: &mut egui::Ui,
    kind: &ContentType,
    sensitive: bool,
    boxed: bool,
    c: &clipd_core::ThemeColors,
) {
    let (rect, _) = ui.allocate_exact_size(
        if boxed {
            egui::vec2(34.0, 34.0)
        } else {
            egui::vec2(26.0, 30.0)
        },
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    if boxed {
        // A frosted tile, as in the reference: the glyph's own small pane.
        // Without it the icon floats and the row loses its left edge.
        painter.rect_filled(rect, Rounding::same(10.0), surf(c, c.bg_elevated));
        painter.rect_stroke(rect, Rounding::same(10.0), Stroke::new(0.8, rgb(c.border)));
    }
    let ink = rgb(c.text).gamma_multiply(0.78);
    let s = Stroke::new(1.5, ink);
    let (cx, cy) = (rect.center().x, rect.center().y);
    // A secret is a key whatever its content type — it is the thing you want
    // to recognise before you read the row, not after.
    if sensitive {
        // A key lies flat: bow on the left, shaft to the right, teeth down
        // off the end. Drawn on the diagonal — bow up-left, stem down-right —
        // it is a magnifying glass, which is what this was and what it read
        // as next to a search field.
        painter.circle_stroke(egui::pos2(cx - 6.0, cy), 4.2, s);
        painter.line_segment([egui::pos2(cx - 1.8, cy), egui::pos2(cx + 8.5, cy)], s);
        painter.line_segment(
            [egui::pos2(cx + 4.5, cy), egui::pos2(cx + 4.5, cy + 4.0)],
            s,
        );
        painter.line_segment(
            [egui::pos2(cx + 7.6, cy), egui::pos2(cx + 7.6, cy + 3.0)],
            s,
        );
        return;
    }
    match kind {
        ContentType::Url => {
            // Two links of a chain meeting on the diagonal.
            for (dx, dy) in [(-3.0_f32, 3.0_f32), (3.0, -3.0)] {
                let r = egui::Rect::from_center_size(
                    egui::pos2(cx + dx, cy + dy),
                    egui::vec2(11.0, 7.0),
                );
                painter.rect_stroke(r, Rounding::same(3.5), s);
            }
        }
        ContentType::Code => {
            // A terminal: a rounded frame with a prompt in it.
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 16.0));
            painter.rect_stroke(r, Rounding::same(3.5), s);
            painter.line_segment(
                [
                    egui::pos2(r.left() + 4.0, r.top() + 4.5),
                    egui::pos2(r.left() + 7.5, cy),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(r.left() + 7.5, cy),
                    egui::pos2(r.left() + 4.0, r.bottom() - 4.5),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(r.left() + 9.5, r.bottom() - 4.5),
                    egui::pos2(r.right() - 4.0, r.bottom() - 4.5),
                ],
                s,
            );
        }
        ContentType::Image => {
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 15.0));
            painter.rect_stroke(r, Rounding::same(3.0), s);
            painter.circle_filled(egui::pos2(r.left() + 4.8, r.top() + 4.4), 1.6, ink);
            painter.line_segment(
                [
                    egui::pos2(r.left() + 2.0, r.bottom() - 2.0),
                    egui::pos2(cx - 0.5, r.center().y),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(cx - 0.5, r.center().y),
                    egui::pos2(r.right() - 2.0, r.bottom() - 2.0),
                ],
                s,
            );
        }
        ContentType::File | ContentType::Path => {
            // A page with its corner turned.
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(14.0, 18.0));
            let fold = 5.5;
            painter.add(egui::Shape::closed_line(
                vec![
                    egui::pos2(r.left(), r.top()),
                    egui::pos2(r.right() - fold, r.top()),
                    egui::pos2(r.right(), r.top() + fold),
                    egui::pos2(r.right(), r.bottom()),
                    egui::pos2(r.left(), r.bottom()),
                ],
                s,
            ));
            painter.line_segment(
                [
                    egui::pos2(r.right() - fold, r.top()),
                    egui::pos2(r.right() - fold, r.top() + fold),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(r.right() - fold, r.top() + fold),
                    egui::pos2(r.right(), r.top() + fold),
                ],
                s,
            );
        }
        _ => {
            // Text: a stack of sheets, the reference's glyph for plain copy.
            for (i, w) in [16.0_f32, 16.0, 10.0].iter().enumerate() {
                let y = cy - 6.0 + i as f32 * 6.0;
                painter.line_segment(
                    [egui::pos2(cx - 8.0, y), egui::pos2(cx - 8.0 + w, y)],
                    s,
                );
            }
        }
    }
}

/// The reference's per-row copy button — two stacked sheets in a quiet
/// outlined tile. The row already copies when clicked; this gives that action
/// a target you can hit without first making the row the selected one.
fn row_copy_button(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let hovered = resp.hovered();
    let face = if hovered {
        surf(c, c.bg_hover)
    } else {
        surf(c, c.bg_elevated)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(8.0), face);
    painter.rect_stroke(rect, Rounding::same(8.0), Stroke::new(0.7, rgb(c.border)));
    let s = Stroke::new(1.2, rgb(c.subtext));
    let back = egui::Rect::from_min_size(
        egui::pos2(rect.center().x - 6.0, rect.center().y - 7.0),
        egui::vec2(8.5, 8.5),
    );
    let front = egui::Rect::from_min_size(
        egui::pos2(rect.center().x - 2.5, rect.center().y - 3.5),
        egui::vec2(8.5, 8.5),
    );
    painter.rect_stroke(back, Rounding::same(2.2), s);
    painter.rect_filled(front, Rounding::same(2.2), face);
    painter.rect_stroke(front, Rounding::same(2.2), s);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text("Copy")
}

/// Minimal header/footer icon — no filled pill, just the glyph.
fn chrome_icon_button(ui: &mut egui::Ui, icon: &str, active: bool, c: &clipd_core::ThemeColors) -> egui::Response {
    let col = if active {
        rgb(c.green)
    } else {
        rgb(c.subtext)
    };
    let response = ui.add(
        egui::Button::new(RichText::new(icon).size(14.0).color(col))
            .fill(Color32::TRANSPARENT)
            .rounding(Rounding::same(6.0))
            .stroke(Stroke::NONE)
            .min_size(egui::vec2(26.0, 26.0)),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Tiny filter pill used under the search bar in the full GUI.
/// Active = solid green; idle = outline only (mockup).
fn tiny_filter_chip(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    theme: Theme,
    c: &clipd_core::ThemeColors,
) -> bool {
    let spotlight = theme == Theme::GlassLight;
    // Light themes get the same treatment, darkening instead of lifting.
    let (text_col, fill, stroke) = if active && spotlight {
        // On glass the selected segment is its own frosted pane with an edge,
        // matching the rows. A smudge of black is a shadow, and there are no
        // shadows inside glass.
        (
            rgb(c.text),
            Color32::from_rgba_unmultiplied(252, 253, 255, 200),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(188, 198, 214, 180)),
        )
    } else if active && theme.is_light() {
        // Filled, unoutlined — the reference's selected segment. The hairline
        // that used to ring it made every pill look like a control; only the
        // active one is meant to.
        (
            rgb(c.text),
            Color32::from_black_alpha(20),
            Stroke::NONE,
        )
    } else if active && !spotlight {
        // A tint, not a slab. These accents are built to be read *as text* on
        // a dark ground — painted as a saturated fill behind dark text they
        // jump ~150 luminance levels off the surface and become the loudest
        // thing in the window, louder than the clips they are filtering.
        (
            rgb(c.text),
            Color32::from_white_alpha(20),
            Stroke::new(1.0, Color32::from_white_alpha(16)),
        )
    } else if active {
        (
            if spotlight { Color32::WHITE } else { rgb(c.bg_base) },
            rgb(c.green),
            Stroke::NONE,
        )
    } else if spotlight {
        // No ring. macOS leaves unselected segments as plain grey text and
        // spends the contrast on the selected one; a hairline round every
        // chip competes with the pill that actually means something — and
        // this one was blue-grey, part of the cast the theme is losing.
        (rgb(c.subtext), Color32::TRANSPARENT, Stroke::NONE)
    } else if theme.is_light() {
        // Unselected filters are plain grey text in the reference, on every
        // light material — not just on glass.
        (rgb(c.subtext), Color32::TRANSPARENT, Stroke::NONE)
    } else {
        (
            rgb(c.subtext),
            Color32::TRANSPARENT,
            Stroke::new(0.9, rgb(c.border)),
        )
    };
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(12.0, 4.0);
        ui.add(
            egui::Button::new(RichText::new(label).size(11.5).color(text_col))
                .fill(fill)
                // Softly rounded, not a capsule: the reference's segments are
                // rectangles with the corners taken off.
                .rounding(Rounding::same(9.0))
                .stroke(stroke)
                .min_size(egui::vec2(0.0, 26.0)),
        )
        .clicked()
    })
    .inner
}

/// Simple outline clock glyph (mockup footer centre).
fn draw_clock_icon_at(painter: &egui::Painter, center: egui::Pos2, col: Color32) {
    let stroke = Stroke::new(1.35, col);
    painter.circle_stroke(center, 7.0, stroke);
    painter.line_segment(
        [center, egui::pos2(center.x, center.y - 4.0)],
        stroke,
    );
    painter.line_segment(
        [center, egui::pos2(center.x + 3.2, center.y + 1.8)],
        stroke,
    );
}

/// Footer shortcut chip — quiet outline box like the mockup's ⌘⇧V.
fn footer_shortcut_badge(ui: &mut egui::Ui, text: &str, c: &clipd_core::ThemeColors) {
    egui::Frame::none()
        .fill(Color32::TRANSPARENT)
        .rounding(Rounding::same(6.0))
        .stroke(Stroke::new(0.85, rgb(c.border)))
        .inner_margin(Margin::symmetric(7.0, 3.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(11.0)
                    .family(egui::FontFamily::Monospace)
                    .color(rgb(c.subtext)),
            );
        });
}

/// Quiet row star: solid green when pinned, outline otherwise.
/// The "Capturing" dot stays green even where the theme's spot mark is not.
///
/// In the Light and Glass Light references the dot is green and the pinned
/// star is orange — two different marks saying two different things, and the
/// spot colour can only be one of them. The spot goes to the star, which is
/// the one the user chose; liveness keeps the green it has in every other
/// theme.
fn capture_dot_color(theme: Theme, c: &clipd_core::ThemeColors) -> Color32 {
    match theme {
        Theme::Light | Theme::GlassLight => Color32::from_rgb(52, 168, 83),
        _ => rgb(c.green),
    }
}

fn row_star_quiet(
    ui: &mut egui::Ui,
    starred: bool,
    c: &clipd_core::ThemeColors,
) -> egui::Response {
    let (icon, col) = if starred {
        // Held back from the full accent. A pinned row already reads as
        // pinned from its filled star; at full strength a couple of them
        // become the brightest marks in the list, which is more emphasis than
        // "I saved this" deserves.
        ("★", rgb(c.green).gamma_multiply(0.72))
    } else {
        ("☆", rgb(c.overlay))
    };
    ui.add(
        egui::Button::new(RichText::new(icon).size(15.0).color(col))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .min_size(egui::vec2(24.0, 24.0)),
    )
    .on_hover_text(if starred { "Unpin" } else { "Pin" })
}

/// Global mouse position in screen points (top-left origin), used to summon
/// the palette at the cursor. macOS-only; other platforms fall back to the
/// window manager's default placement.
/// Compact relative time for list rows: "now", "5m", "2h", "3d", "2w".
fn relative_time_short(dt: &DateTime<Utc>) -> String {
    let secs = Utc::now().signed_duration_since(*dt).num_seconds();
    if secs < 60 {
        return "now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h", hours);
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{}d", days);
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    dt.format("%b %d").to_string()
}

fn clip_group_label(_clip: &ClipEntry, starred: bool) -> &'static str {
    // Full GUI mockup: two sections only — pinned first, everything else Recent.
    if starred {
        "Pinned"
    } else {
        "Recent"
    }
}

fn content_type_label(kind: &ContentType) -> &'static str {
    match kind {
        ContentType::Text | ContentType::Unknown => "Text",
        ContentType::Url => "Link",
        ContentType::Code => "Code",
        ContentType::Email => "Mail",
        // "Path" is a path-shaped string; "File" is a real copied file. Both
        // exist now, so they can no longer share a label.
        ContentType::Path => "Path",
        ContentType::Image => "Image",
        ContentType::File => "File",
    }
}

/// Human-readable byte size for the preview meta rows.
fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Meta label for a clip type as shown in rows ("link" reads better than "url").
fn type_label(kind: &ContentType) -> &'static str {
    match kind {
        ContentType::Text => "text",
        ContentType::Url => "link",
        ContentType::Code => "code",
        ContentType::Email => "email",
        ContentType::Path => "path",
        ContentType::Image => "image",
        ContentType::File => "file",
        ContentType::Unknown => "text",
    }
}

#[cfg(target_os = "macos")]
fn global_cursor_position() -> Option<egui::Pos2> {
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let event = CGEvent::new(source).ok()?;
    let p = event.location();
    Some(egui::pos2(p.x as f32, p.y as f32))
}

/// Windows: GetCursorPos returns *physical* pixels; callers on Windows must
/// divide by the native scale factor before using it as egui points.
#[cfg(target_os = "windows")]
fn global_cursor_position() -> Option<egui::Pos2> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos only writes into the POINT we hand it.
    if unsafe { GetCursorPos(&mut p) } != 0 {
        Some(egui::pos2(p.x as f32, p.y as f32))
    } else {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn global_cursor_position() -> Option<egui::Pos2> {
    None
}

/// Cursor position in egui points, correcting for the display scale factor on
/// Windows (macOS already reports points).
fn cursor_in_points(ctx: &egui::Context) -> Option<egui::Pos2> {
    let p = global_cursor_position()?;
    if cfg!(target_os = "windows") {
        let scale = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0)
            .max(0.5);
        Some(egui::pos2(p.x / scale, p.y / scale))
    } else {
        Some(p)
    }
}

/// Where to place the window so it feels like it popped up at the cursor:
/// search bar centered under the pointer, just below it.
fn window_pos_at_cursor(
    cursor: egui::Pos2,
    win_size: egui::Vec2,
    screen: Option<egui::Vec2>,
) -> egui::Pos2 {
    let mut x = cursor.x - win_size.x * 0.5;
    // Keep clear of the island when it owns the top of the screen. Opening
    // Settings from the island's own gear puts the cursor inside that band,
    // so without this the window opens directly underneath it.
    let min_top = if clipd_core::island_layout_active() {
        clipd_core::ISLAND_RESERVED_TOP
    } else {
        8.0
    };
    let mut y = (cursor.y - 24.0).max(min_top);
    // Clamp so the whole card stays on the display (never opens half-cut at
    // a screen edge). Screen size is best-effort; without it just avoid <0.
    if let Some(s) = screen {
        x = x.min(s.x - win_size.x - 8.0);
        // A window too tall to fit below the band sits as low as it can
        // instead — off the bottom of the screen would be worse than overlap.
        y = y.min((s.y - win_size.y - 8.0).max(8.0));
    }
    egui::pos2(x.max(8.0), y.max(8.0))
}

/// Size of the main display in *points* (the same space as `OuterPosition`).
///
/// `CGDisplayBounds` is pixels on Retina, which made the HUD think the screen
/// was twice as wide and dock itself to the right edge. `NSScreen.frame` is
/// the coordinate space the window actually uses.
#[cfg(target_os = "macos")]
fn main_display_size() -> Option<egui::Vec2> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;
    let mtm = MainThreadMarker::new()?;
    let screen = NSScreen::mainScreen(mtm)?;
    let f = screen.frame();
    Some(egui::vec2(f.size.width as f32, f.size.height as f32))
}
#[cfg(not(target_os = "macos"))]
fn main_display_size() -> Option<egui::Vec2> {
    None
}

/// Tiny clipd logo mark: a clipboard outline with its clip tab, vector-drawn
/// so it's crisp at any size and always renders (no font glyphs).
/// The cat, shared with the island — one mark for the whole product.
///
/// The island had it and the palette drew a clipboard outline, so the two
/// windows introduced clipd as two different apps.
static CAT_PNG: &[u8] = include_bytes!("../assets/cat.png");

fn clipd_cat_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    static CACHE: std::sync::Mutex<Option<Option<egui::TextureHandle>>> =
        std::sync::Mutex::new(None);
    let mut slot = CACHE.lock().ok()?;
    if slot.is_none() {
        *slot = Some(clipd_core::decode_rgba(CAT_PNG).ok().map(|(w, h, rgba)| {
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            ctx.load_texture("clipd_cat_palette", img, egui::TextureOptions::LINEAR)
        }));
    }
    slot.as_ref().and_then(|t| t.clone())
}

/// Paint the cat inside `rect`, keeping its proportions; falls back to the
/// drawn clipboard if the texture cannot be decoded.
pub(crate) fn draw_clipd_mark(ui: &mut egui::Ui, rect: egui::Rect, col: Color32) {
    let Some(tex) = clipd_cat_texture(ui.ctx()) else {
        draw_clipd_logo(ui.painter(), rect, col);
        return;
    };
    let size = tex.size_vec2();
    let scale = (rect.width() / size.x).min(rect.height() / size.y);
    let fitted = egui::Rect::from_center_size(rect.center(), size * scale);
    egui::Image::new((tex.id(), fitted.size())).paint_at(ui, fitted);
}

fn draw_clipd_logo(painter: &egui::Painter, rect: egui::Rect, col: Color32) {
    let center = rect.center();
    let h = rect.height().min(rect.width() * 1.3);
    let board = egui::Rect::from_center_size(
        egui::pos2(center.x, center.y + h * 0.04),
        egui::vec2(h * 0.72, h * 0.88),
    );
    painter.rect_stroke(board, Rounding::same(2.0), Stroke::new(1.2, col));
    // The clip tab across the top edge.
    let clip = egui::Rect::from_center_size(
        egui::pos2(center.x, board.top()),
        egui::vec2(h * 0.38, h * 0.22),
    );
    painter.rect_filled(clip, Rounding::same(1.5), col);
    // A hint of "content": one short line inside the board.
    painter.line_segment(
        [
            egui::pos2(board.left() + h * 0.16, center.y + h * 0.08),
            egui::pos2(board.right() - h * 0.16, center.y + h * 0.08),
        ],
        Stroke::new(1.0, col.gamma_multiply(0.75)),
    );
}

/// Decode a thumbnail PNG off disk and upload it as an egui texture.
fn load_thumb_texture(ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
    let (w, h, rgba) = clipd_core::load_rgba(std::path::Path::new(path)).ok()?;
    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    Some(ctx.load_texture(
        format!("clipd_thumb_{path}"),
        img,
        egui::TextureOptions::LINEAR,
    ))
}

fn tag_pill(ui: &mut egui::Ui, label: &str, col: Color32, c: &clipd_core::ThemeColors) {
    egui::Frame::none()
        .fill(pill_bg(col))
        .rounding(Rounding::same(PILL_ROUND))
        .inner_margin(Margin::symmetric(PILL_PAD_X, PILL_PAD_Y))
        .stroke(Stroke::new(0.5, col.gamma_multiply(0.85)))
        .show(ui, |ui| {
            ui.label(RichText::new(label).size(10.5).color(rgb(c.text)));
        });
}

/// A compact, calm pill button used for row actions (Copy / Refine / Remove …).
fn pill_button(ui: &mut egui::Ui, label: &str, c: &clipd_core::ThemeColors) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).color(rgb(c.text)))
            .fill(surf(c, c.bg_surface))
            .rounding(Rounding::same(PILL_ROUND))
            .stroke(Stroke::new(0.5, rgb(c.border)))
            .min_size(egui::vec2(0.0, 23.0)),
    )
}

fn outline_button(
    ui: &mut egui::Ui,
    label: &str,
    accent: Color32,
    _c: &clipd_core::ThemeColors,
) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.0).color(accent))
            .fill(Color32::TRANSPARENT)
            .rounding(Rounding::same(5.0))
            .stroke(Stroke::new(0.7, accent))
            .min_size(egui::vec2(54.0, 22.0)),
    )
}

fn star_button(ui: &mut egui::Ui, starred: bool, c: &clipd_core::ThemeColors) -> egui::Response {
    let (label, hover) = if starred {
        ("📌", "Unpin clip")
    } else {
        ("📌", "Pin clip")
    };
    // Spot colour from the concept sheet — not chrome `accent`.
    let col = if starred {
        rgb(c.green)
    } else {
        rgb(c.overlay)
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(14.0).color(col))
            .fill(if starred {
                rgb(c.green).gamma_multiply(0.12)
            } else {
                Color32::TRANSPARENT
            })
            .rounding(Rounding::same(6.0))
            .stroke(Stroke::new(
                if starred { 0.8 } else { 0.0 },
                rgb(c.green).gamma_multiply(0.7),
            ))
            .min_size(egui::vec2(30.0, 28.0)),
    )
    .on_hover_text(hover)
}

fn row_star_button(
    ui: &mut egui::Ui,
    starred: bool,
    c: &clipd_core::ThemeColors,
) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(4.0, 4.0);
        star_button(ui, starred, c)
    })
    .inner
}

/// Full-width hairline divider in the theme's border color.
fn hairline(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, rgb(c.border).gamma_multiply(0.7)),
    );
}

/// Small iOS-style switch. Returns true when clicked (caller flips the value).
fn mini_switch(ui: &mut egui::Ui, on: bool, c: &clipd_core::ThemeColors) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 19.0), egui::Sense::click());
    let track = if on {
        rgb(c.accent)
    } else {
        surf(c, c.bg_elevated)
    };
    ui.painter().rect_filled(rect, Rounding::same(9.5), track);
    let knob_x = if on {
        rect.right() - 10.0
    } else {
        rect.left() + 10.0
    };
    let knob_col = if on { rgb(c.bg_base) } else { rgb(c.subtext) };
    ui.painter()
        .circle_filled(egui::pos2(knob_x, rect.center().y), 7.0, knob_col);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Compact keyboard hint used by the always-visible core workflow card.
fn shortcut_badge(ui: &mut egui::Ui, text: &str, c: &clipd_core::ThemeColors) {
    egui::Frame::none()
        .fill(surf(c, c.bg_elevated))
        .rounding(Rounding::same(6.0))
        .stroke(Stroke::new(0.7, rgb(c.border)))
        .inner_margin(Margin::symmetric(6.0, 2.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(10.0)
                    .strong()
                    .family(egui::FontFamily::Monospace)
                    .color(rgb(c.text)),
            );
        });
}

/// Warning-banner colours that follow the theme.
///
/// These were hardcoded to a dark-theme amber, which on a light theme rendered
/// as tan text on a tan block — the message was there but unreadable, which is
/// the worst possible outcome for a banner whose whole job is to be read.
/// Returns `(fill, title, body, button_fill, button_text)`.
fn warning_colors(light: bool) -> (Color32, Color32, Color32, Color32, Color32) {
    if light {
        (
            Color32::from_rgb(253, 240, 219),
            Color32::from_rgb(124, 61, 6),
            Color32::from_rgb(146, 84, 22),
            Color32::from_rgb(180, 105, 30),
            Color32::from_rgb(255, 249, 240),
        )
    } else {
        (
            Color32::from_rgb(90, 50, 20).gamma_multiply(0.55),
            Color32::from_rgb(255, 200, 120),
            Color32::from_rgb(230, 190, 140),
            Color32::from_rgb(120, 70, 30),
            Color32::from_rgb(255, 220, 160),
        )
    }
}

/// A background surface, honouring the theme's `surface_alpha`.
///
/// Solid themes report 255 and this is identical to `rgb`. The glass themes
/// report less, which is what turns cards and rows into frosted panes the
/// shell's blur shows through — without it a translucent window still reads as
/// an opaque white panel, because everything drawn on it is opaque.
/// Takes the palette by borrow *or* by value: `ThemeColors` is `Copy`, and
/// call sites hold it both ways.
fn surf(c: impl std::borrow::Borrow<clipd_core::ThemeColors>, col: clipd_core::Rgb) -> Color32 {
    Color32::from_rgba_unmultiplied(col.0, col.1, col.2, c.borrow().surface_alpha)
}

fn tab_chip(ui: &mut egui::Ui, label: &str, active: bool, c: &clipd_core::ThemeColors) -> bool {
    // Fully-rounded pills, the active one filled and outlined so the current
    // filter is legible at a glance rather than by a faint tint alone.
    let (text_col, fill, stroke) = if active {
        (
            rgb(c.text),
            rgb(c.green).gamma_multiply(0.20),
            Stroke::new(0.9, rgb(c.green).gamma_multiply(0.55)),
        )
    } else {
        (
            rgb(c.subtext),
            surf(c, c.bg_elevated),
            Stroke::new(0.7, rgb(c.border)),
        )
    };
    let response = ui.add(
        egui::Button::new(RichText::new(label).size(11.5).color(text_col))
            .fill(fill)
            .rounding(Rounding::same(999.0))
            .stroke(stroke)
            .min_size(egui::vec2(0.0, 28.0)),
    );
    response.clicked()
}

/// A tab chip with a trailing count badge (e.g. "Pins  2"). Same look as
/// `tab_chip`, but the count sits in a small muted pill so the tab still reads
/// as one control.
fn tab_chip_count(
    ui: &mut egui::Ui,
    label: &str,
    count: usize,
    active: bool,
    c: &clipd_core::ThemeColors,
) -> bool {
    let text_col = if active {
        rgb(c.green)
    } else {
        rgb(c.subtext)
    };
    let inner = egui::Frame::none()
        .fill(if active {
            rgb(c.green).gamma_multiply(0.14)
        } else {
            Color32::TRANSPARENT
        })
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::symmetric(10.0, 6.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new(label).size(11.5).color(text_col));
                egui::Frame::none()
                    .fill(surf(c, c.bg_elevated))
                    .rounding(Rounding::same(5.0))
                    .inner_margin(Margin::symmetric(5.0, 1.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(count.to_string())
                                .size(10.0)
                                .color(rgb(c.subtext)),
                        );
                    });
            });
        });
    let resp = inner
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    resp.clicked()
}

enum Action {
    None,
    /// Copy to the clipboard only — clipd stays in front (single-click select).
    Copy,
    /// Copy, return focus to the previous app, and paste (Enter / double-click).
    Paste,
    Delete,
    ToggleStar(i64),
    /// Run custom action at this index on the selected clip.
    RunAction(usize),
    /// Send the `?`-prefixed search bar contents to the ask engine.
    Ask,
    /// Select the clip behind a clicked `[#id]` citation.
    JumpToClip(i64),
    /// Jump to Settings so a missing API key can be fixed where it's noticed.
    OpenAiSettings,
    /// Start an ask scoped to one clip (the row's ✦ chip).
    AskAboutClip(i64),
    /// Run the Smart Recommend suggestion at this index on the selected clip.
    RunSuggestion(usize),
    /// Send the selected clip to the other Mac (`S`). This is the one thing
    /// Universal Clipboard can't do: send from *history*, not just whatever is
    /// on the clipboard right now.
    Send,
    /// Take back the last send (`U`), while the other Mac hasn't collected it.
    UndoSend,
}

/// Where a pairing has got to.
///
/// Discovery blocks for up to a minute, which an immediate-mode UI cannot do,
/// so it runs on a worker thread and reports back through a channel. This enum
/// is what the window draws from.
enum PairingState {
    Idle,
    Searching {
        /// Set to stop the worker when the user cancels or closes the window.
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        result: std::sync::mpsc::Receiver<Result<clipd_core::lan_pair::PairingOffer, String>>,
    },
    /// Both machines are showing a code and the user has to compare them.
    Confirming(clipd_core::lan_pair::PairingOffer),
    Done(String),
    Failed(String),
}

impl PairingState {
    fn is_busy(&self) -> bool {
        matches!(self, PairingState::Searching { .. } | PairingState::Confirming(_))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MainTab {
    Text,
    Collections,
    Settings,
    /// Encrypted vault — API keys and secrets stored in macOS Keychain.
    Vault,
}

/// Top-level Settings pages — one category at a time so nothing is a long scroll.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsCategory {
    General,
    Clipboard,
    Ai,
    Appearance,
    Privacy,
}

impl SettingsCategory {
    const ALL: [SettingsCategory; 5] = [
        SettingsCategory::General,
        SettingsCategory::Clipboard,
        SettingsCategory::Ai,
        SettingsCategory::Appearance,
        SettingsCategory::Privacy,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingsCategory::General => "General",
            SettingsCategory::Clipboard => "Clipboard",
            SettingsCategory::Ai => "AI",
            SettingsCategory::Appearance => "Appearance",
            SettingsCategory::Privacy => "Privacy",
        }
    }

    /// Jump to a category from the settings search box.
    fn from_query(q: &str) -> Option<Self> {
        let q = q.trim().to_ascii_lowercase();
        if q.is_empty() {
            return None;
        }
        let hits: &[(SettingsCategory, &[&str])] = &[
            (
                SettingsCategory::General,
                &["general", "hud", "surface", "send", "pair", "vault", "snippet", "action"],
            ),
            (
                SettingsCategory::Clipboard,
                &[
                    "clipboard",
                    "paste",
                    "slot",
                    "transform",
                    "shortcut",
                    "palette",
                    "multi",
                    "letter",
                ],
            ),
            (SettingsCategory::Ai, &["ai", "model", "openai", "ollama", "ask"]),
            (
                SettingsCategory::Appearance,
                &["appearance", "theme", "color", "colour", "dark", "light", "paper"],
            ),
            (
                SettingsCategory::Privacy,
                &["privacy", "secret", "exclude", "credit", "ssn", "password"],
            ),
        ];
        for (cat, keys) in hits {
            if keys.iter().any(|k| q.contains(k)) {
                return Some(*cat);
            }
        }
        None
    }
}

/// The secondary rail mirrors Finder's content filters.  Keeping this separate
/// from the primary Clipboard/Pins/Settings navigation makes the palette feel
/// like a small macOS utility rather than a command launcher.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ContentFilter {
    All,
    Favorites,
    Slots,
    Text,
    Links,
    Code,
    Images,
    Files,
    /// Keys, tokens and private keys — the things you copy out of a dashboard
    /// once and then hunt for twenty minutes later. They are scattered across
    /// Text and Code by content type, so no existing filter collects them.
    ApiKeys,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SurfaceMode {
    Main,
    Settings,
    Hud,
    /// The notch island: a resident black slab at the top of the display that
    /// hugs the MacBook notch and hosts modules. See `island.rs`.
    Island,
    /// Dismiss the popover entirely — used when the tray dropdown opens, so
    /// the native menu doesn't draw on top of a panel it has no relation to.
    Hidden,
    /// Shut this window down. Sent by "Quit clipd" so every surface goes at
    /// once — and, critically, before the tray host exits, since a surviving
    /// window would otherwise restart it within seconds.
    Quit,
}

impl SurfaceMode {
    fn from_args(args: &[String]) -> Self {
        if args.iter().any(|argument| argument == "--hud") {
            Self::Hud
        } else if args.iter().any(|argument| argument == "--island") {
            Self::Island
        } else if args.iter().any(|argument| argument == "--settings") {
            Self::Settings
        } else {
            Self::Main
        }
    }

    fn request_value(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Settings => "settings",
            Self::Hud => "hud",
            Self::Island => "island",
            Self::Hidden => "hidden",
            Self::Quit => "quit",
        }
    }

    fn from_request(value: &str) -> Option<Self> {
        match value.trim() {
            "main" => Some(Self::Main),
            "settings" => Some(Self::Settings),
            "hud" => Some(Self::Hud),
            "island" => Some(Self::Island),
            "hidden" => Some(Self::Hidden),
            "quit" => Some(Self::Quit),
            _ => None,
        }
    }
}

fn process_lock_name(mode: SurfaceMode) -> &'static str {
    match mode {
        SurfaceMode::Hud => "gui-hud",
        SurfaceMode::Island => "gui-island",
        // Main, Settings, Hidden and Quit all target the same primary palette process.
        _ => "gui-main",
    }
}

fn surface_request_path(mode: SurfaceMode) -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join(format!("{}.request", process_lock_name(mode)))
}

fn send_surface_request(mode: SurfaceMode) {
    let path = surface_request_path(mode);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, mode.request_value());
}

fn send_surface_request_to(target: SurfaceMode, mode: SurfaceMode) {
    let path = surface_request_path(target);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, mode.request_value());
}

fn take_surface_request_for(mode: SurfaceMode) -> Option<SurfaceMode> {
    let path = surface_request_path(mode);
    let request = std::fs::read_to_string(&path).ok();
    if request.is_some() {
        let _ = std::fs::remove_file(path);
    }
    request.and_then(|value| SurfaceMode::from_request(&value))
}

/// Persisted current surface so other processes (the tray menu) can tell
/// whether a given surface is currently visible and toggle it off rather than
/// re-launching it. Written by `switch_surface` whenever the mode changes.
fn surface_state_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join(format!("{}.state", process_lock_name(SurfaceMode::Main)))
}

fn hud_state_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join(format!("{}.state", process_lock_name(SurfaceMode::Hud)))
}

fn surface_state_path_for(mode: SurfaceMode) -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join(format!("{}.state", process_lock_name(mode)))
}

fn save_surface_state(mode: SurfaceMode) {
    let path = match mode {
        SurfaceMode::Hud => hud_state_path(),
        SurfaceMode::Island => surface_state_path_for(SurfaceMode::Island),
        _ => surface_state_path(),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, mode.request_value());
}

impl ContentFilter {
    /// Full-GUI filter row — includes Images so screenshots aren't buried.
    const MAIN: [(ContentFilter, &'static str); 7] = [
        (ContentFilter::All, "All"),
        (ContentFilter::Links, "Links"),
        (ContentFilter::Text, "Text"),
        (ContentFilter::Code, "Code"),
        (ContentFilter::Images, "Images"),
        (ContentFilter::ApiKeys, "API keys"),
        (ContentFilter::Favorites, "Pinned"),
    ];

    /// Extended set kept for keyboard / settings access (Slots, Files).
    const ALL: [(ContentFilter, &'static str); 9] = [
        (ContentFilter::All, "All"),
        (ContentFilter::Links, "Links"),
        (ContentFilter::Text, "Text"),
        (ContentFilter::Code, "Code"),
        (ContentFilter::Images, "Images"),
        (ContentFilter::ApiKeys, "API keys"),
        (ContentFilter::Favorites, "Pinned"),
        (ContentFilter::Slots, "Slots"),
        (ContentFilter::Files, "Files"),
    ];
}

// ── Entry point ──

fn theme_named(name: &str) -> Option<Theme> {
    match name.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "system" => Some(Theme::System),
        "light" | "paper-light" | "light-minimal" => Some(Theme::Light),
        "dark" | "black" | "mac-black" => Some(Theme::Dark),
        "midnight" => Some(Theme::Midnight),
        // Retired: the nearest survivors, so old configs and scripts keep working.
        "paper" | "paper-dark" => Some(Theme::Dark),
        "forest" => Some(Theme::Forest),
        "cocoa" => Some(Theme::Slate),
        "slate" => Some(Theme::Slate),
        "glass-light" | "glasslight" => Some(Theme::GlassLight),
        // Retired: Glass Dark's job — a dark surface with no colour in it —
        // is what Dark already does, without a translucency layer to fight.
        "glass-dark" | "glassdark" | "glass" | "glass-minimal" | "glassminimal" => {
            Some(Theme::Dark)
        }
        // Legacy names map to the closest curated theme so existing configs/cli
        // calls don't break. The old colorful themes are gone, but users land in
        // a readable palette instead of an error.
        "catppuccin" | "mocha" => Some(Theme::Catppuccin),
        "monokai" | "nord" | "dracula" => Some(Theme::Dark),
        _ => None,
    }
}

fn requested_theme(args: &[String]) -> Option<Result<Theme, String>> {
    let position = args.iter().position(|arg| arg == "--set-theme")?;
    let name = args.get(position + 1).map(String::as_str).unwrap_or("");
    Some(theme_named(name).ok_or_else(|| {
        format!(
            "Unknown theme '{name}'. Use system, light, dark, midnight, forest, slate, catppuccin, glass-light, or glass-dark."
        )
    }))
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(theme_request) = requested_theme(&args) {
        match theme_request {
            Ok(theme) => {
                save_theme(theme);
                println!("clipd theme set to {}", theme.label());
            }
            Err(message) => eprintln!("{message}"),
        }
        return Ok(());
    }

    let requested_mode = SurfaceMode::from_args(&args);
    // Each surface mode has its own process lease so the tray HUD popover
    // and the main palette can coexist. A later invocation of the same mode
    // posts a tiny request and exits; the existing process handles it.
    // Different modes run as separate processes.
    let lock_name = process_lock_name(requested_mode);
    let _instance_guard = match clipd_core::ProcessLock::try_acquire(lock_name) {
        Some(guard) => guard,
        None => {
            send_surface_request(requested_mode);
            if matches!(requested_mode, SurfaceMode::Main | SurfaceMode::Settings) {
                let _ = focus_existing_instance();
            }
            log::info!("requested {requested_mode:?} from the existing clipd GUI — exiting");
            return Ok(());
        }
    };
    // A request left by a launcher racing the first process is superseded by
    // the explicit mode that won the lease.
    let _ = take_surface_request_for(requested_mode);
    save_surface_state(requested_mode);

    // Spawn daemon as a child process (rdev's keyboard hook conflicts with
    // eframe's event loop if both run in the same process on macOS).
    // Only clipd-ui owns the daemon/hotkey host. GUI processes (main, HUD)
    // must NOT spawn their own — that would create multiple clipd-ui instances,
    // each needing separate Accessibility/Input Monitoring permissions.
    let daemon_child: Option<std::process::Child> = None;

    let hud = requested_mode == SurfaceMode::Hud;
    let island = requested_mode == SurfaceMode::Island;
    let open_settings = requested_mode == SurfaceMode::Settings;

    let mut viewport = if island {
        // The notch island opens at its resting size: the cutout plus a sliver
        // either side. Everything after this is driven from `drive_island`.
        let config = clipd_core::load_island_config();
        let geometry = island::notch_geometry(&config);
        egui::ViewportBuilder::default()
            .with_inner_size([geometry.width + 28.0, geometry.height.max(24.0)])
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_transparent(true)
    } else if hud {
        // Tray-anchored clipboard popover: opens straight to full size under
        // the tray icon.
        egui::ViewportBuilder::default()
            .with_inner_size([HUD_W, HUD_H])
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_transparent(true)
    } else {
        egui::ViewportBuilder::default()
            // Compact, borderless floating-palette card (expands for preview).
            .with_inner_size([COMPACT_W, WIN_H])
            .with_min_inner_size([480.0, 560.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_transparent(true)
    };

    if hud {
        // Resident HUD starts off-screen. Showing it at the computed tray
        // position on launch put a clipboard card in the middle of the menu
        // bar before anyone had hovered the extra. `show_hud_onscreen` moves
        // it under the icon when clipd-ui asks.
        viewport = viewport.with_position([0.0, -4000.0]);
    } else if island {
        let config = clipd_core::load_island_config();
        let geometry = island::notch_geometry(&config);
        let screen = main_display_size().unwrap_or(egui::vec2(1440.0, 900.0));
        let width = geometry.width + 28.0;
        let left = (geometry.center_x - width / 2.0).clamp(0.0, (screen.x - width).max(0.0));
        // A notched display gets the island flush with the top edge; anywhere
        // else it tucks under the menu bar so it can't cover the clock.
        let top = if geometry.real { 0.0 } else { geometry.height + 4.0 };
        viewport = viewport.with_position([left, top]);
    } else if cfg!(target_os = "macos") {
        // Open where the user is working: palette appears at the mouse cursor.
        // macOS only at startup (CG reports points directly); on Windows the
        // scale factor isn't known until the first frame, where the focus-gain
        // handler repositions to the cursor anyway.
        if let Some(cursor) = global_cursor_position() {
            let pos =
                window_pos_at_cursor(cursor, egui::vec2(COMPACT_W, WIN_H), main_display_size());
            viewport = viewport.with_position([pos.x, pos.y]);
        }
    }
    let options = eframe::NativeOptions {
        viewport,
        // Without multisampling every rounded corner, pill and hairline border
        // is drawn with hard stair-stepped edges — which reads as "pixelated"
        // and is genuinely tiring to look at on a window this dense.
        multisampling: 4,
        ..Default::default()
    };

    let result = eframe::run_native(
        "clipd",
        options,
        Box::new(|cc| {
            let theme = load_theme();
            apply_theme(&cc.egui_ctx, theme);
            // The island yields the top of the screen while a clipd window is
            // *visible*. The HUD is resident and hidden for most of its life,
            // so it claims this only when it actually shows itself — claiming
            // it at startup kept the island hidden permanently.
            if !island && !hud {
                // Come forward. The bundle sets LSUIElement, so clipd is a
                // menu-bar agent and its windows do not activate on their own:
                // the palette would spawn, exist, and stay behind whatever the
                // user was looking at. The shortcut fired, a process started,
                // and nothing appeared to happen.
                //
                // Only this surface. The island and the tray popover are
                // summoned by pointing at them and must never steal the front.
                activate_for_keyboard_input();
                clipd_core::set_gui_window_open(true);
                // Only one way of looking at the clipboard at a time. The
                // palette, the tray popover and the island are three views of
                // the same clips; two of them on screen at once is not extra
                // information, just two windows fighting for the same corner.
                // The island stands down on the flag above; the popover needs
                // telling, because it is resident and may be showing already.
                send_surface_request_to(SurfaceMode::Hud, SurfaceMode::Hidden);
            }
            Ok(Box::new(ClipdGui::new(theme, hud, island, open_settings)))
        }),
    );

    // GUI closed — kill the daemon subprocess
    if let Some(mut child) = daemon_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    clipd_core::release_daemon_lock();
    let _ = std::fs::remove_file(surface_state_path());
    let _ = std::fs::remove_file(hud_state_path());
    let _ = std::fs::remove_file(surface_state_path_for(SurfaceMode::Island));

    result
}

/// Returns true if another clipd-gui process is already running and raises its
/// window on platforms where native focusing is available.
#[cfg(target_os = "macos")]
fn focus_existing_instance() -> bool {
    // At this point (before run_native) this process is not yet a UI app, so it
    // isn't in the System Events process list — any match is a prior instance.
    // Tries every name the eframe app may register under; never errors.
    let script = r#"tell application "System Events"
  repeat with n in {"clipd-gui", "Clipd", "clipd"}
    set matches to (every process whose name is (n as string))
    if (count of matches) > 0 then
      set frontmost of (item 1 of matches) to true
      return "ok"
    end if
  end repeat
end tell
return """#;
    std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "ok")
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn focus_existing_instance() -> bool {
    focus_windows_gui_window()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn focus_existing_instance() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn focus_windows_gui_window() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    let title: Vec<u16> = "clipd".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: title is a valid nul-terminated UTF-16 string. The returned HWND
    // is only passed back to Win32 window-management functions.
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
    true
}

/// Return focus to the app the user came from (recorded by the daemon when clipd
/// was summoned) — this both hides clipd behind that app and puts the cursor back
/// where it was, so a plain Cmd+V pastes the clip they just picked. No synthetic
/// keystroke, so it needs no Accessibility permission and is instant.
#[cfg(target_os = "macos")]
fn return_focus_to_previous_app() {
    let Some(app) = clipd_core::load_last_active_app() else {
        return;
    };
    let app = app.replace('"', "'");
    let script = format!(
        r#"tell application "System Events"
  set ps to (every process whose name is "{app}")
  if (count of ps) > 0 then set frontmost of (item 1 of ps) to true
end tell"#,
        app = app
    );
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(not(target_os = "macos"))]
fn return_focus_to_previous_app() {}

#[cfg(target_os = "macos")]
fn spawn_daemon_process() -> Option<std::process::Child> {
    if clipd_core::is_daemon_running() {
        log::info!("Daemon already running — skipping hotkey host launch");
        return None;
    }

    let Some(ui_bin) = find_ui_binary() else {
        log::warn!("clipd-ui binary not found — Ctrl+G hotkey host was not started");
        return None;
    };

    log::info!("Starting macOS hotkey host: {}", ui_bin.display());
    let _ = std::process::Command::new(&ui_bin)
        .env("CLIPD_NO_AUTO_OPEN_GUI", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| log::warn!("Failed to start clipd-ui hotkey host: {}", e));

    // Do not return the child. The tray/hotkey host should survive this search
    // window closing; otherwise Ctrl+G works only while a GUI window is open.
    None
}

#[cfg(not(target_os = "macos"))]
fn spawn_daemon_process() -> Option<std::process::Child> {
    if clipd_core::is_daemon_running() {
        log::info!("Daemon already running — skipping spawn");
        return None;
    }

    // Find the `clipd` CLI binary next to this executable
    let cli_bin = find_cli_binary()?;

    log::info!("Spawning daemon process: {} daemon", cli_bin.display());
    let mut command = std::process::Command::new(&cli_bin);
    command
        .arg("daemon")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // clipd.exe is a console binary for CLI use. When the GUI needs to
        // bootstrap its daemon, keep that console completely hidden.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .map_err(|e| log::warn!("Failed to spawn daemon: {}", e))
        .ok()
}

#[cfg(target_os = "macos")]
fn find_ui_binary() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("clipd-ui");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    for candidate in [
        workspace_root.join("target/debug/clipd-ui"),
        workspace_root.join("target/release/clipd-ui"),
    ] {
        if candidate.exists() {
            return Some(candidate);
        }
    }

    if let Ok(output) = std::process::Command::new("which").arg("clipd-ui").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    None
}

#[cfg(not(target_os = "macos"))]
fn find_cli_binary() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(target_os = "windows")]
            for name in ["clipd.exe", "clipd"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let candidate = dir.join("clipd");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("where").arg("clipd").output() {
            if output.status.success() {
                let line = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if !line.is_empty() {
                    return Some(PathBuf::from(line));
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(output) = std::process::Command::new("which").arg("clipd").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    None
}

/// Split a recall query into (content terms, source-app filter). Supports
/// "from chrome", "json from chrome", and "app:chrome" so users recall by where
/// a clip came from instead of memorizing slots.
fn split_from_query(raw: &str) -> (String, String) {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("from ") {
        return (String::new(), rest.trim().to_string());
    }
    if let Some(rest) = raw.strip_prefix("app:") {
        return (String::new(), rest.trim().to_string());
    }
    if let Some(idx) = raw.find(" from ") {
        return (
            raw[..idx].trim().to_string(),
            raw[idx + 6..].trim().to_string(),
        );
    }
    (raw.to_string(), String::new())
}

/// Bring clipd forward so one of its windows can take the keyboard.
///
/// clipd runs as a menu-bar agent, and an accessory app's windows cannot
/// become key while the app itself is not active — so `ViewportCommand::Focus`
/// on the island had nothing to grant. The search field would call
/// `request_focus`, look focused, and every keystroke would still go to
/// whatever app was in front.
///
/// Only for deliberate, click-initiated input like opening search. Never on
/// hover: taking the keyboard because a pointer passed over something is how
/// the tray popover started swallowing what people were typing.
/// Make this frame's window the key window.
///
/// Measured, not assumed: the island's window reports `canBecomeKey=true` and
/// `isKey=false`, so it is allowed the keyboard and simply never given it —
/// `ViewportCommand::Focus` did not land for it. Asking AppKit directly does.
#[cfg(target_os = "macos")]
pub(crate) fn make_window_key(frame: &eframe::Frame) {
    use objc2_app_kit::NSView;
    if let Some(window) = ns_metal_view(frame).and_then(|v| v.window()) {
        window.makeKeyAndOrderFront(None);
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn make_window_key(_frame: &eframe::Frame) {}

#[cfg(target_os = "macos")]
pub(crate) fn activate_for_keyboard_input() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        unsafe {
            app.activateIgnoringOtherApps(true)
        };
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn activate_for_keyboard_input() {}

fn resolved_theme(ctx: &egui::Context, theme: Theme) -> Theme {
    if theme != Theme::System {
        return theme;
    }
    match ctx.system_theme() {
        Some(egui::Theme::Light) => Theme::Light,
        _ => Theme::Dark,
    }
}

/// Load the macOS system font (San Francisco) into egui so the UI matches
/// native Mac apps. Falls back silently on non-macOS or if the font is missing.
fn install_system_font(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    {
        // SF Pro first — the face every other macOS window is set in.
        //
        // Optima led this list, and it is a thin humanist face with high
        // stroke contrast: at 13pt on a pale surface its verticals thin out
        // until the text looks washed rather than quiet, which is what made
        // the light themes read as faded. The fallbacks stay for machines
        // that lack SFNS.
        let paths = [
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/HelveticaNeue.ttc",
            "/System/Library/Fonts/Geneva.ttf",
            "/System/Library/Fonts/Optima.ttc",
        ];
        for path in &paths {
            if let Ok(data) = std::fs::read(path) {
                let mut fonts = egui::FontDefinitions::default();
                fonts.font_data.insert(
                    "system".to_string(),
                    egui::FontData::from_owned(data).into(),
                );
                // Set as the default proportional font family.
                fonts.families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "system".to_string());
                // SF Mono, if it loads. The family must name the *key* the
                // data was registered under — naming the file path here left
                // the Monospace family pointing at a key with no data behind
                // it, and epaint panics the moment anything renders monospace,
                // which took the whole window down.
                if let Ok(mono_data) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
                    fonts.font_data.insert(
                        "sfmono".to_string(),
                        egui::FontData::from_owned(mono_data).into(),
                    );
                    fonts.families
                        .entry(egui::FontFamily::Monospace)
                        .or_default()
                        .insert(0, "sfmono".to_string());
                }
                ctx.set_fonts(fonts);
                return;
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ctx;
    }
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::System => egui::ThemePreference::System,
        t if t.is_light() => egui::ThemePreference::Light,
        _ => egui::ThemePreference::Dark,
    });

    let effective = resolved_theme(ctx, theme);
    let mut c = effective.colors();
    load_custom_colors().apply_to(&mut c);

    // Soften every edge the tessellator produces. Feathering is what turns a
    // hard 1px boundary into a blended one; without it the glass themes' low
    // contrast makes aliasing *more* visible, not less, because the eye has
    // nothing else to lock onto.
    ctx.tessellation_options_mut(|t| {
        t.feathering = true;
        t.feathering_size_in_pixels = 1.4;
    });

    let mut style = (*ctx.style()).clone();
    // Paper Light (and any light theme): slightly larger type — warm ivory
    // softens mid-size glyphs and made the old 14/11.5 stack feel washed out.
    let (body, heading, small, button) = if effective.is_light() {
        (15.0, 18.0, 12.5, 13.5)
    } else {
        (14.0, 18.0, 11.5, 13.0)
    };
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(body));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(heading));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(small));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(button));
    style.spacing.item_spacing = egui::vec2(10.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.window_margin = Margin::symmetric(12.0, 10.0);
    style.visuals.window_rounding = Rounding::same(12.0);
    style.visuals.menu_rounding = Rounding::same(8.0);
    ctx.set_style(style);

    let mut v = if effective.is_light() {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    v.override_text_color = Some(rgb(c.text));
    v.panel_fill = Color32::TRANSPARENT;
    // Glass themes keep the native window clear so the frosted shell + desktop
    // show through; opaque themes paint a solid base.
    v.window_fill = if effective.is_glass() {
        Color32::TRANSPARENT
    } else {
        rgb(c.bg_base)
    };
    v.window_stroke = if effective.is_glass() {
        Stroke::NONE
    } else {
        Stroke::new(0.8, rgb(c.border))
    };
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: if effective.is_glass() { 28.0 } else { 16.0 },
        spread: 0.0,
        color: Color32::from_black_alpha(if effective.is_glass() { 80 } else { 60 }),
    };
    v.window_rounding = Rounding::same(16.0);
    v.extreme_bg_color = rgb(c.bg_base);
    v.faint_bg_color = surf(c, c.bg_surface);
    v.widgets.noninteractive.bg_fill = surf(c, c.bg_surface);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, rgb(c.text));
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    v.widgets.noninteractive.rounding = Rounding::same(8.0);
    v.widgets.inactive.bg_fill = surf(c, c.bg_elevated);
    v.widgets.inactive.rounding = Rounding::same(8.0);
    v.widgets.hovered.bg_fill = surf(c, c.bg_hover);
    v.widgets.hovered.rounding = Rounding::same(8.0);
    v.widgets.active.bg_fill = surf(c, c.bg_selected);
    v.widgets.active.rounding = Rounding::same(8.0);
    // Text-selection fill sits *behind* glyphs that keep their ink colour, so
    // the wash has to contrast with that ink. Near-white accents (Dark) and
    // wrongly-premultiplied greens (Paper Light) both made selected text
    // unreadable — pick a clear tint per family instead.
    let (sel_bg, sel_stroke) = text_selection_style(effective, &c);
    v.selection.bg_fill = sel_bg;
    v.selection.stroke = sel_stroke;
    ctx.set_visuals(v);
}

/// Colours for egui's text caret/selection. Glyphs keep `text` colour, so the
/// fill must never be near that colour (light-on-light or dark-on-dark).
fn text_selection_style(theme: Theme, c: &clipd_core::ThemeColors) -> (Color32, Stroke) {
    if theme.is_glass() {
        // Cool slate wash — keep mint off the selection (chips/pins only).
        let (r, g, b, a) = if theme == Theme::GlassLight {
            // selectedTextBackgroundColor — the exact pale blue macOS puts
            // behind selected text in light mode.
            (179, 215, 255, 200)
        } else {
            (90, 120, 170, 140)
        };
        (
            Color32::from_rgba_unmultiplied(r, g, b, a),
            Stroke::new(1.0, Color32::from_rgb(140, 175, 220)),
        )
    } else if theme.is_light() {
        // Paper Light: solid-enough sage so selected search text is obvious
        // against ivory (glyphs stay near-black).
        (
            Color32::from_rgba_unmultiplied(c.green.0, c.green.1, c.green.2, 160),
            Stroke::new(1.2, rgb(c.green)),
        )
    } else if c.accent.0 > 200 && c.accent.1 > 200 && c.accent.2 > 200 {
        // Neutral/off-white accent themes (Dark): slate-blue under light glyphs.
        (
            Color32::from_rgba_unmultiplied(55, 105, 190, 170),
            Stroke::new(1.2, Color32::from_rgb(130, 180, 255)),
        )
    } else {
        // Forest / Paper Dark / Midnight / concepts.
        (
            Color32::from_rgba_unmultiplied(c.green.0, c.green.1, c.green.2, 150),
            Stroke::new(1.2, rgb(c.green)),
        )
    }
}

/// Paint the palette as one continuous floating material. Individual panels
/// stay transparent so opening the preview never produces visible seams or a
/// stack of differently coloured rectangles.
///
/// Glassmorphism shell: native frosted blur + dual soft gradient blooms
/// (cyan/sky + magenta/lavender) + rim. Not flat transparency, not white orbs.
fn paint_glass_shell(
    ctx: &egui::Context,
    theme: Theme,
    c: &clipd_core::ThemeColors,
    native_glass: bool,
) {
    let rect = ctx.screen_rect().shrink(0.5);
    let painter = ctx.layer_painter(egui::LayerId::background());
    let rounding = Rounding::same(SHELL_ROUND);

    if theme.is_glass() {
        let light = theme == Theme::GlassLight;
        // Frosted base — translucent enough for blur + blooms to read.
        let veil = if light {
            // A real frost plate, not bare transparency. The native clear
            // material still shows through, but the wash is now near-white:
            // the old grey-blue (130,136,146) was doing the same thing the
            // panel frost was, and two coats of grey is what silver is.
            //
            // A white plate over a white document does lose its edge — that
            // is what the rim and the shadow below are for, and it is how
            // macOS lands its own light material on a white page.
            // Without the native material there is no blur at all, so the
            // veil has to carry the whole surface and stays heavier there.
            // The whole surface, in one even coat. Clear glass does almost
            // no diffusion of its own, so this layer is what stands between
            // the reader and whatever is behind the window.
            // Between the two measured settings. At (126,133,144)@50 the panel
            // sat at rgb(156,163,171) — clean, but heavy over a dark desktop.
            // At (168,175,186)@44 it washed to rgb(250,255,255), i.e. back to
            // white: light paint on this material loses very quickly, so the
            // usable range between "grey" and "white" is narrow.
            Color32::from_rgba_unmultiplied(146, 153, 165, if native_glass { 47 } else { 136 })
        } else {
            // Neutral to match the grounds: a blue veil over neutral surfaces
            // just puts the cast back on top of the fix.
            Color32::from_rgba_unmultiplied(12, 12, 14, if native_glass { 142 } else { 186 })
        };
        painter.rect_filled(rect, rounding, veil);

        // Dual glassmorphism blooms (clipped to the rounded plate).
        if let Some((glow_a, glow_b)) = theme.shell_glows() {
            let glow_painter = painter.with_clip_rect(rect);
            // A broad corner-to-corner tint keeps Glass Light visibly glassy
            // even over a white document. Backdrop blur alone necessarily
            // becomes white over white; the reference carries a persistent
            // sky-to-lavender wash across the whole plate.
            paint_glass_corner_gradient(&glow_painter, rect, glow_a, glow_b, light);
            let radius = rect.width().max(rect.height()) * 0.92;
            // Strong enough that the plate carries its own tint. Glass shows what
            // is behind it, so over a plain light desktop a purely backdrop-driven
            // pane just looks white — the blooms are what keep it looking like
            // glass regardless of what happens to be underneath.
            // Same reason as the corner wash: a white bloom at 0.18 is 18%
            // of the plate's opacity spent on nothing but white.
            let strength = if light { 0.05 } else { 0.30 };
            paint_soft_radial_glow(
                &glow_painter,
                egui::pos2(
                    rect.left() + rect.width() * 0.08,
                    rect.top() + rect.height() * 0.12,
                ),
                radius,
                glow_a,
                strength,
            );
            paint_soft_radial_glow(
                &glow_painter,
                egui::pos2(
                    rect.right() - rect.width() * 0.04,
                    rect.bottom() - rect.height() * 0.08,
                ),
                radius * 0.95,
                glow_b,
                strength * 0.92,
            );
        }

        // Soft frost veil on top of blooms — diffuses them into glass, not spots.
        painter.rect_filled(
            rect,
            rounding,
            Color32::from_rgba_unmultiplied(
                if light { 255 } else { 255 },
                if light { 255 } else { 255 },
                if light { 255 } else { 255 },
                if light {
                    if native_glass { 5 } else { 16 }
                } else if native_glass {
                    6
                } else {
                    12
                },
            ),
        );

        // Glass edge + top catch-light.
        //
        // Light keeps a real rim: a pale plate over a pale desktop needs an
        // edge or it has no shape at all. Dark does not — a white line all the
        // way round a dark plate reads as a drawn border sitting on top of the
        // material rather than as light catching its edge. The catch-light
        // along the top does that job on its own.
        // Light draws its edge the way macOS does — a dark hairline at about
        // ten percent, not a white one. White-on-white had no edge at all
        // once the plate stopped being grey, and the shape of the window came
        // entirely from the shadow.
        // Glass catches light along its edge, and that highlight is most of
        // what makes a translucent panel read as a pane rather than a hole in
        // the screen. The dark hairline here was drawn for an opaque white
        // plate, where it was the only way to find the edge; on a translucent
        // one it is a pencil line round the window. Depth comes from the
        // shadow underneath instead.
        let rim = if light {
            Color32::from_rgba_unmultiplied(255, 255, 255, 190)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 12)
        };
        painter.rect_stroke(rect, rounding, Stroke::new(if light { 1.05 } else { 0.6 }, rim));
        painter.hline(
            (rect.left() + SHELL_ROUND)..=(rect.right() - SHELL_ROUND),
            rect.top() + 1.0,
            Stroke::new(
                1.15,
                Color32::from_rgba_unmultiplied(255, 255, 255, if light { 120 } else { 58 }),
            ),
        );
    } else {
        painter.rect_filled(rect, rounding, rgb(c.bg_base));
        painter.rect_filled(rect, rounding, surf(c, c.bg_surface));
        painter.rect_stroke(rect, rounding, Stroke::new(0.8, rgb(c.border)));
        painter.hline(
            (rect.left() + SHELL_ROUND)..=(rect.right() - SHELL_ROUND),
            rect.top() + 1.0,
            Stroke::new(0.7, rgb(c.text).gamma_multiply(0.12)),
        );
    }
}

/// One continuous sky-to-lavender wash beneath the frost. Vertex colours are
/// interpolated by the GPU, so this stays smooth at every window size.
fn paint_glass_corner_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    glow_a: Rgb,
    glow_b: Rgb,
    light: bool,
) {
    // Light's blooms were written when they were coloured — a sky and a
    // mauve, where 34/255 reads as a tint. They are near-whites now, so the
    // same alpha is simply more white paint on a surface that already has too
    // much. A third of it still gives the plate a direction of light.
    let alpha = if light { [11, 8, 9, 12] } else { [40, 26, 28, 44] };
    let vertex = |pos: egui::Pos2, color: Rgb, a: u8| egui::epaint::Vertex {
        pos,
        uv: egui::epaint::WHITE_UV,
        color: Color32::from_rgba_unmultiplied(color.0, color.1, color.2, a),
    };

    let mut mesh = egui::Mesh::default();
    mesh.vertices.push(vertex(rect.left_top(), glow_a, alpha[0]));
    mesh.vertices.push(vertex(rect.right_top(), glow_b, alpha[1]));
    mesh.vertices.push(vertex(rect.left_bottom(), glow_a, alpha[2]));
    mesh.vertices.push(vertex(rect.right_bottom(), glow_b, alpha[3]));
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(2, 1, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// Soft glassmorphism bloom — many concentric discs with quadratic falloff
/// so it reads as a gradient wash, not a hard white circle.
fn paint_soft_radial_glow(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: Rgb,
    strength: f32,
) {
    // Drawn as one vertex-coloured mesh rather than a stack of filled circles.
    //
    // Stacking circles quantises the gradient into visible concentric bands —
    // at this radius even 22 layers puts a hard step every ~40px, which reads
    // as the whole window being pixelated. A mesh lets the GPU interpolate
    // alpha smoothly between vertices, so the falloff is continuous no matter
    // how large the bloom is, and it costs one draw call instead of 22.
    const SEGMENTS: usize = 96;
    // Rings are spaced to follow the quadratic falloff the banded version had.
    // Interpolation handles everything between them, so a handful is plenty.
    const RINGS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

    let alpha_at = |t: f32| -> u8 {
        let falloff = (1.0 - t) * (1.0 - t);
        (255.0 * strength * falloff).round().clamp(0.0, 255.0) as u8
    };
    let vertex = |pos: egui::Pos2, a: u8| egui::epaint::Vertex {
        pos,
        uv: egui::epaint::WHITE_UV,
        color: Color32::from_rgba_unmultiplied(color.0, color.1, color.2, a),
    };

    let mut mesh = egui::Mesh::default();

    // Ring 0 is a single centre vertex; the rest are full circles of vertices.
    mesh.vertices.push(vertex(center, alpha_at(0.0)));
    for &ring in &RINGS[1..] {
        let a = alpha_at(ring);
        for seg in 0..SEGMENTS {
            let angle = seg as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            mesh.vertices.push(vertex(
                center + egui::vec2(angle.cos(), angle.sin()) * radius * ring,
                a,
            ));
        }
    }

    // Fan from the centre out to the first ring.
    for seg in 0..SEGMENTS {
        let next = (seg + 1) % SEGMENTS;
        mesh.add_triangle(0, 1 + seg as u32, 1 + next as u32);
    }
    // Quad strips between each pair of rings.
    for band in 0..RINGS.len() - 2 {
        let inner = 1 + (band * SEGMENTS) as u32;
        let outer = inner + SEGMENTS as u32;
        for seg in 0..SEGMENTS as u32 {
            let next = (seg + 1) % SEGMENTS as u32;
            mesh.add_triangle(inner + seg, outer + seg, outer + next);
            mesh.add_triangle(inner + seg, outer + next, inner + next);
        }
    }

    painter.add(egui::Shape::mesh(mesh));
}

/// Apply / clear native macOS glass for Glass Light / Glass Dark.
///
/// On Tahoe and newer, use AppKit's real Liquid Glass first. It must be a
/// sibling behind winit's render view: putting `NSGlassEffectView` inside the
/// render view makes it refract clipd's own foreground, washing out the text.
/// The old implementation used the correct sibling position but chose classic
/// light vibrancy first, which resolves to the flat white plate we are avoiding.
/// Older macOS releases fall back to classic vibrancy.
#[cfg(target_os = "macos")]
fn sync_glass_native(frame: &eframe::Frame, theme: Theme, on: &mut Option<bool>) {
    let want = theme.is_glass().then_some(theme == Theme::GlassLight);
    if want == *on {
        return;
    }

    clear_sibling_glass(frame);
    let _ = window_vibrancy::clear_liquid_glass(frame);
    let _ = window_vibrancy::clear_vibrancy(frame);
    *on = None;

    let Some(light) = want else {
        write_glass_status("off", false);
        return;
    };

    force_view_transparent(frame);

    match apply_sibling_liquid_glass(frame, light) {
        Ok(()) => {
            *on = Some(light);
            write_glass_status("liquid-regular", light);
        }
        Err(err) => {
            log::info!("Liquid Glass unavailable ({err}); using classic vibrancy");
            let material = if light {
                window_vibrancy::NSVisualEffectMaterial::UnderWindowBackground
            } else {
                window_vibrancy::NSVisualEffectMaterial::HudWindow
            };
            match window_vibrancy::apply_vibrancy(
                frame,
                material,
                Some(window_vibrancy::NSVisualEffectState::Active),
                Some(SHELL_ROUND as f64),
            ) {
                Ok(()) => {
                    *on = Some(light);
                    write_glass_status("vibrancy", light);
                }
                Err(err) => {
                    log::warn!("native glass unavailable: {err}");
                    write_glass_status("failed", light);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn write_glass_status(applied: &str, light: bool) {
    let status = format!(
        "glass_native={applied} theme={} light={light}\n",
        if light { "GlassLight" } else { "Dark" }
    );
    if let Some(dir) = dirs::data_dir() {
        let _ = std::fs::write(dir.join("clipd/glass_native.status"), &status);
    }
    eprintln!("[clipd] {status}");
}

#[cfg(target_os = "macos")]
fn ns_metal_view(frame: &eframe::Frame) -> Option<&'static objc2_app_kit::NSView> {
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = frame.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    Some(unsafe { appkit.ns_view.cast::<NSView>().as_ref() })
}

#[cfg(target_os = "macos")]
fn force_view_transparent(frame: &eframe::Frame) {
    use objc2_app_kit::NSColor;

    let Some(view) = ns_metal_view(frame) else {
        return;
    };
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setOpaque(false);
        // The egui panels are independently painted regions. Mask the native
        // render surface as well as rounding those regions so a resized frame,
        // antialiased edge, or one-frame panel transition can never leak a
        // square pixel into the transparent window corners.
        layer.setCornerRadius(SHELL_ROUND as f64);
        layer.setMasksToBounds(true);
    }
    if let Some(window) = view.window() {
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
    }
}

const SIBLING_LIQUID_TAG: isize = 96945937;

/// Tahoe Liquid Glass behind (never around) the egui render surface.
#[cfg(target_os = "macos")]
fn apply_sibling_liquid_glass(frame: &eframe::Frame, light: bool) -> Result<(), String> {
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSView, NSWindowOrderingMode,
    };
    use window_vibrancy::NSGlassEffectViewTagged;

    let mtm = MainThreadMarker::new().ok_or("not on main thread")?;
    let metal = ns_metal_view(frame).ok_or("no ns_view")?;
    let window = metal.window().ok_or("ns_view has no window yet")?;
    let content = window.contentView().ok_or("window has no contentView")?;
    let parent = unsafe { content.superview() }.ok_or("contentView has no superview")?;

    let glass = unsafe {
        NSGlassEffectViewTagged::initWithFrame(mtm.alloc(), content.frame(), SIBLING_LIQUID_TAG)
    };
    // Regular for both. Clear is the lensing variant — it bends and passes the
    // backdrop through with very little diffusion, so over a window full of
    // text you read that text straight through the panel and the surface looks
    // mottled rather than frosted. The same reasoning that put Light on
    // Regular applies to Dark; it was only ever Clear because a dark tint hid
    // the problem over a dark desktop.
    // Regular for both — the diffusing style. Clear was tried for light and
    // is a lens, not frost: it passes the backdrop through with so little
    // diffusion that browser tabs and photo captions behind the window stay
    // legible through the panel, which reads as a dirty, uneven surface.
    // Regular resolves toward white only when what is painted on top of it is
    // near-white too; with translucent rows and one even frost it stays a
    // proper frosted sheet.
    let style = objc2_app_kit::NSGlassEffectViewStyle(
        window_vibrancy::NSGlassEffectViewStyle::Regular as isize,
    );
    let tint = if light {
        // AppKit's untinted light style approaches pure white, and clipd is
        // nearly always over a white document — so at 0.24 the panel was
        // white with a hint of grey, which is what "still white, no glass"
        // means. Deeper and cooler: the material now has a colour of its own
        // that survives a white backdrop, and over anything darker it still
        // passes the backdrop through.
        NSColor::colorWithRed_green_blue_alpha(0.71, 0.74, 0.79, 0.35)
    } else {
        // Denser than it was. A thin tint over Regular glass still let bright
        // windows behind punch through as patches, which is what made the
        // panel look uneven rather than like one piece of material.
        //
        // Near-equal channels: this tint covers the whole plate, so a blue
        // ratio here (it was 0.06/0.065/0.08 — a third more blue than red)
        // tinted everything behind it no matter how neutral the surfaces were.
        NSColor::colorWithRed_green_blue_alpha(0.048, 0.048, 0.054, 0.42)
    };
    glass.setStyle(style);
    glass.setTintColor(Some(tint.as_ref()));
    glass.setCornerRadius(SHELL_ROUND as f64);
    glass.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    let glass_view: Retained<NSView> = Retained::into_super(Retained::into_super(glass));
    parent.addSubview_positioned_relativeTo(
        &glass_view,
        NSWindowOrderingMode::Below,
        Some(content.as_ref()),
    );

    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    force_view_transparent(frame);
    Ok(())
}

#[cfg(target_os = "macos")]
fn clear_sibling_glass(frame: &eframe::Frame) {
    let Some(metal) = ns_metal_view(frame) else {
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
    if let Some(view) = parent.viewWithTag(SIBLING_LIQUID_TAG) {
        view.removeFromSuperview();
    }
}

#[cfg(not(target_os = "macos"))]
fn sync_glass_native(_frame: &eframe::Frame, _theme: Theme, _on: &mut Option<bool>) {}

/// Glass selection / hover — soft neutral wash (mint stays on chips/pins).
fn glass_row_fill(theme: Theme, selected: bool, hovered: bool) -> Option<Color32> {
    let light = theme == Theme::GlassLight;
    if light {
        // Every row is its own frosted pane, lit by how much white it holds:
        // resting, under the pointer, selected. Rows used to be transparent
        // until selected, so the list was floating text on one undifferentiated
        // sheet — and the selected row then had to be marked with black, which
        // is a shadow on glass rather than light in it.
        // Cool white, not plain white. Over a photo this is frost; over a
        // white window it still separates from the plate, because it is
        // fractionally bluer than the paper behind it.
        // Measured, not guessed. Every value before this was subtle enough to
        // vanish: a white pane at alpha 62 over a plate that was itself
        // near-white differs from it by three levels, so the list read as one
        // flat sheet no matter how correct the intent was.
        return Some(Color32::from_rgba_unmultiplied(
            255,
            255,
            255,
            if selected {
                72
            } else if hovered {
                52
            } else {
                34
            },
        ));
    }
    if selected {
        Some(if light {
            // Grey, by lightness alone — Spotlight's selected row, and what
            // macOS falls back to for any list it is not actively focused on.
            // A systemBlue wash was tried here and had to come out: over a
            // white plate it blended toward cyan, and a coloured band across
            // the row is the loudest thing in a window this pale.
            //
            // A white wash on a white plate is not a selection either, which
            // is why this cannot simply go back to what it was.
            // Measured, not guessed: at alpha 26 the composited row came out
            // 14 levels under the plate, because the row's own card is drawn
            // over part of this wash. macOS's unemphasized selection sits
            // about 25 under its window colour, and this alpha lands there.
            Color32::from_black_alpha(46)
        } else {
            // Keep keyboard focus readable even when Liquid Glass is sampling
            // a bright window behind clipd. A white wash can turn the row into
            // a pale slab under white text; this teal-black anchor still lets
            // the material move while preserving contrast.
            Color32::from_rgba_unmultiplied(16, 42, 46, 156)
        })
    } else if hovered {
        Some(if light {
            Color32::from_rgba_unmultiplied(0, 0, 0, 10)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 12)
        })
    } else {
        None
    }
}

fn glass_row_stroke(theme: Theme, selected: bool) -> Stroke {
    if theme == Theme::GlassLight {
        // The lit edge of each pane. Every row carries one: a rim is what
        // separates two translucent surfaces stacked on each other, where a
        // fill alone only makes a slightly brighter fog.
        // The edge does the work. A white highlight reads as glass only when
        // something darker sits behind it — over a white app it is invisible
        // and the whole list flattens into one sheet. A cool grey rim is an
        // edge on both.
        return Stroke::new(
            1.0,
            if selected {
                Color32::from_rgba_unmultiplied(144, 160, 186, 225)
            } else {
                Color32::from_rgba_unmultiplied(176, 188, 208, 205)
            },
        );
    }
    if selected {
        if theme == Theme::GlassLight {
            // A hairline the fill alone cannot provide: 9% grey on white is
            // legible as a band but has no edge, and the rows around it are
            // white cards with edges of their own.
            Stroke::new(1.0, Color32::from_black_alpha(30))
        } else {
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(160, 170, 165, 40))
        }
    } else {
        Stroke::NONE
    }
}

/// Stable frost carried by the actual egui panels. Native light glass adapts
/// toward white over bright apps; this translucent plate keeps the theme from
/// being erased by that adaptation while preserving backdrop movement.
fn glass_panel_frost(theme: Theme) -> Color32 {
    match theme {
        // Every layout region receives the exact same veil, so adjacent
        // regions resolve as one continuous sheet.
        //
        // This was a mid-slate (126,134,146) at 70% — two thirds of a grey
        // card laid over the entire window, and the main reason the theme
        // read as brushed metal. A near-white at slightly higher opacity does
        // the stabilising job just as well: over a dark app behind, the
        // composite still lands light instead of collapsing to grey.
        // Thin. This is the layer that decides whether the theme is glass at
        // all, and it had been pushed to 214/255 of a near-white — a sheet of
        // paint over the blur, which is why the window came out flat white
        // whatever was behind it. At this alpha the native material and the
        // desktop behind actually reach the eye; the rims and row fills below
        // are what keep dark ink legible instead.
        // Nothing. The shell's veil is the single frost layer now; painting
        // it again per panel is what made the surface uneven from region to
        // region, because the panels do not all cover the same area.
        Theme::GlassLight => Color32::TRANSPARENT,
        _ => Color32::TRANSPARENT,
    }
}

/// Repaint one low-chroma ambient wash within each panel, underneath its
/// widgets. Colours are sampled in whole-window coordinates, so a header,
/// list, preview and footer meet without restarting the gradient at a seam.
fn paint_panel_glass_gradient(ui: &egui::Ui, theme: Theme) {
    // Glass Light's reflection is painted once by `paint_glass_shell`; repeating
    // it in every panel creates seams and visible colour restarts.
    if theme == Theme::GlassLight {
        return;
    }
    let Some((left, right)) = theme.shell_glows() else {
        return;
    };
    let rect = ui.max_rect();
    let screen = ui.ctx().screen_rect();
    let alpha = if theme == Theme::GlassLight { 26 } else { 35 };
    let sample = |pos: egui::Pos2| {
        let x = ((pos.x - screen.left()) / screen.width().max(1.0)).clamp(0.0, 1.0);
        let y = ((pos.y - screen.top()) / screen.height().max(1.0)).clamp(0.0, 1.0);
        // A gentle diagonal drift; keeping some vertical influence prevents a
        // large window from looking like two rigid colour columns.
        let t = (x * 0.72 + y * 0.28).clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(mix(left.0, right.0), mix(left.1, right.1), mix(left.2, right.2))
    };
    let vertex = |pos: egui::Pos2| egui::epaint::Vertex {
        pos,
        uv: egui::epaint::WHITE_UV,
        color: {
            let color = sample(pos);
            Color32::from_rgba_unmultiplied(color.0, color.1, color.2, alpha)
        },
    };
    let mut mesh = egui::Mesh::default();
    mesh.vertices.push(vertex(rect.left_top()));
    mesh.vertices.push(vertex(rect.left_bottom()));
    mesh.vertices.push(vertex(rect.right_top()));
    mesh.vertices.push(vertex(rect.right_bottom()));
    mesh.add_triangle(0, 2, 1);
    mesh.add_triangle(1, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

// ── App state ──

struct ClipdGui {
    store: ClipStore,
    clips: Vec<ClipEntry>,
    search_query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll_to_selected: bool,
    copied_at: Option<Instant>,
    last_refresh: Instant,
    focus_search: bool,
    theme: Theme,
    /// User-defined palette that overrides the active theme when enabled.
    custom_colors: CustomColors,
    /// Full palette and tray HUD are separate long-lived processes. Poll the
    /// shared appearance preference at a low rate so either surface reflects a
    /// theme change made in the other without needing to be relaunched.
    last_shared_appearance_check: Instant,

    show_transforms: bool,
    /// On-demand preview pane (Space toggles it). Off = clean single column.
    show_preview: bool,
    /// Inline quick-settings (gear): theme swatches + paste-on-select.
    show_quick_settings: bool,
    /// Active Settings page (General / Clipboard / AI / …).
    settings_category: SettingsCategory,
    /// Search box while Settings is open — jumps to a matching category.
    settings_query: String,
    /// Header pin — keep the full GUI above other windows.
    window_pinned: bool,
    /// Tracks window focus so summoning clipd lands the cursor in search.
    was_focused: bool,
    /// Vault (1Password / Bitwarden / Keychain) "save clipboard as a password" form.
    vault_targets: Vec<VaultTarget>,
    vault_selected: Option<VaultTarget>,
    vault_title: String,
    vault_username: String,
    vault_url: String,
    /// API key the user typed/pasted directly into the vault save form.
    vault_password_input: String,
    /// (is_success, message) of the last vault save attempt.
    vault_status: Option<(bool, String)>,
    /// Cached list of vault secrets (labels only — no plaintext).
    vault_secrets: Vec<clipd_core::SecretRef>,
    /// Secret currently being revealed (plaintext shown temporarily).
    vault_revealed: Option<(usize, String, Instant)>,
    /// Confirm-delete state for a secret index.
    vault_confirm_delete: Option<usize>,
    /// Reusable text snippets and the ones matching the current search.
    snippets: Vec<clipd_core::Snippet>,
    matched_snippets: Vec<clipd_core::Snippet>,
    new_snippet_trigger: String,
    new_snippet_name: String,
    new_snippet_body: String,
    /// Custom Actions — user-defined shell commands run on a clip.
    custom_actions: Vec<CustomAction>,
    new_action_name: String,
    new_action_command: String,
    new_action_auto: String,
    new_action_output: ActionOutput,
    /// Last action result banner in the preview pane: (ok, message).
    action_status: Option<(bool, String)>,
    /// Where the machine-pairing flow has got to.
    pairing: PairingState,
    /// Machines seen on the network, refreshed on a timer rather than every
    /// frame — it reads files, and Settings repaints constantly.
    nearby: Vec<clipd_core::sync::Reachable>,
    nearby_checked: Option<Instant>,
    transforms: Vec<TransformKind>,
    paste_settings: PasteTransformSettings,

    cached_tfidf: Option<TfIdfIndex>, // built lazily once per refresh, reused for all searches
    privacy_config: PrivacyConfig,
    sessions: Vec<Session>,
    session_config: SessionConfig,
    active_tab: MainTab,
    content_filter: ContentFilter,
    show_active_slots_only: bool,
    new_excluded_app: String,
    new_custom_pattern: String,
    confirm_clear_all: bool,
    export_status: Option<(String, Instant)>,

    // Collections
    collections: Vec<clipd_core::Collection>,
    starred_collection_id: Option<i64>,
    starred_clip_ids: HashSet<i64>,
    /// GPU textures for image-clip thumbnails, keyed by clip id. `None` means we
    /// tried to load and failed (missing/corrupt file) — don't retry every frame.
    thumb_textures: std::collections::HashMap<i64, Option<egui::TextureHandle>>,
    new_collection_name: String,
    new_collection_app: String,
    ai_result: Option<String>,

    /// AI provider settings (Ask, embeddings, transform-on-paste all share these).
    /// Edited in Settings; previously only reachable by hand-writing JSON, which
    /// is why Ask looked broken out of the box.
    ai_config: TransformConfig,
    /// The key as typed. Kept separate from `ai_config.api_key` so an untouched
    /// field can show a masked placeholder without overwriting the stored key.
    ai_key_input: String,
    /// Result of the last "Test connection", as (ok, message).
    ai_test_status: Option<(bool, String)>,
    ai_test_rx: Option<std::sync::mpsc::Receiver<(bool, String)>>,

    /// Ask mode — engaged by a leading `?` in the search bar.
    ask: AskState,
    /// Smart Recommend transform currently running, if any.
    transform_job: TransformJob,

    /// Set once a quit is in flight, so the daemon watchdog stops reviving the
    /// tray host we are deliberately shutting down.
    quitting: bool,
    /// Gear pressed in the popover: settings replace the clip list in place,
    /// rather than opening a second window over the first.
    popover_settings_open: bool,
    /// Menu-bar clipboard HUD: tray popover that opens expanded on hover.
    hud: bool,
    /// The notch island layout. Its own process, its own resident window.
    island_surface: bool,
    /// Whether the HUD's request-file watcher thread has been started.
    hud_watcher_started: bool,
    /// Clips whose preview is a redaction, so their tooltip stays hidden.
    masked_clip_ids: HashSet<i64>,
    /// Set when something asked for the keyboard mid-frame.
    want_key_window: bool,
    /// Secret-scan results by clip id, so a reload only scans what is new.
    secret_scan_cache: HashMap<i64, Option<String>>,
    /// Last time this window renewed its on-screen claim.
    last_claim_refresh: Instant,
    /// Island configuration, live readings and shelf. Present in every process
    /// because Settings edits it from the palette and the island reads it back
    /// off disk.
    island: island::IslandState,
    /// Whether the HUD pill is currently expanded.
    hud_expanded: bool,
    /// When the pointer left the HUD; drives the collapse grace period.
    hud_left_at: Option<Instant>,
    /// Grace period after a tray "show" request — don't check hover-leave
    /// until this instant, so the HUD doesn't instantly hide while the
    /// cursor is still on the tray icon.
    hud_grace_until: Option<Instant>,
    /// Last size we sent to the window manager, to avoid spamming resize commands.
    last_sent_size: Option<egui::Vec2>,
    /// Last position we sent to the window manager, to avoid spamming move commands.
    last_sent_pos: Option<egui::Pos2>,
    /// Polls the tiny cross-process mode request file at a low frequency. This
    /// replaces multiple resident GUI processes with one reusable window.
    last_surface_request_check: Instant,
    /// Restart the macOS tray/hotkey host if it died while this window stayed open.
    #[cfg(target_os = "macos")]
    last_daemon_check: Instant,
    /// Native macOS glass currently applied. `Some(is_light)` tracks Light vs Dark
    /// so we re-tint when cycling between Glass Light and Glass Dark.
    #[cfg(target_os = "macos")]
    glass_native: Option<bool>,
}

/// State for `?`-prefixed questions. The request runs on a worker thread and
/// reports back through `rx`; egui is immediate-mode, so a blocking HTTP call
/// on the UI thread would freeze the window for the whole round trip.
#[derive(Default)]
struct AskState {
    /// The question that produced `answer`, or is currently in flight.
    question: String,
    running: bool,
    rx: Option<std::sync::mpsc::Receiver<Result<AskAnswer, String>>>,
    answer: Option<AskAnswer>,
    error: Option<String>,
    /// Conversation so far. Follow-ups replay it; it is also written to SQLite.
    thread: AskThread,
}

/// A Smart Recommend transform in flight. Same reasoning as `AskState`: an
/// AI transform is an HTTP round trip, and egui redraws on the calling thread.
#[derive(Default)]
struct TransformJob {
    running: bool,
    /// Chip label, so the spinner can say what it's doing.
    label: String,
    rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    result: Option<Result<String, String>>,
}

impl AskState {
    /// Clear the answer but keep the conversation — used when the query
    /// changes so a stale answer never sits under a new question.
    fn clear_answer(&mut self) {
        self.answer = None;
        self.error = None;
    }

    /// Drop everything, including conversation history.
    fn reset(&mut self) {
        self.clear_answer();
        self.question.clear();
        self.thread = AskThread::new();
    }
}

/// The question in the search bar, if it is one. `?` alone is not a question.
/// Extract the question from the search box. Two spellings count as asking:
/// a leading `?` (the documented gesture) and a natural trailing `?` on a
/// multi-word query ("how do I install clipd?") — people type questions the
/// second way instinctively, and treating that as a search made Enter fall
/// through to paste-and-hide, which looked like a crash.
fn ask_query(search: &str) -> Option<&str> {
    let t = search.trim();
    if let Some(rest) = t.strip_prefix('?') {
        let rest = rest.trim();
        return (!rest.is_empty()).then_some(rest);
    }
    if t.ends_with('?') && t.split_whitespace().count() >= 2 {
        let rest = t.trim_end_matches('?').trim_end();
        return (!rest.is_empty()).then_some(rest);
    }
    None
}

impl ClipdGui {
    fn native_glass_active(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.glass_native.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn new(
        theme: Theme,
        hud: bool,
        island_surface: bool,
        open_settings: bool,
    ) -> Self {
        let db_path = ClipStore::default_path();
        let store = ClipStore::new(&db_path).expect("Failed to open clip database");
        let mut clips = store.get_recent(MAX_LOADED_CLIPS).unwrap_or_default();
        sync_active_slot_labels(&store, &mut clips);
        let mut secret_scan_cache: HashMap<i64, Option<String>> = HashMap::new();
        let masked_clip_ids = mask_secret_previews(&mut clips, &mut secret_scan_cache);
        let count = clips.len();
        let session_config = SessionConfig::default();
        let sessions = compute_sessions(&clips, session_config.window_minutes);
        let mut app = Self {
            store,
            clips,
            search_query: String::new(),
            filtered: (0..count).collect(),
            selected: 0,
            scroll_to_selected: false,
            copied_at: None,
            last_refresh: Instant::now(),
            focus_search: true,
            theme,
            custom_colors: load_custom_colors(),
            last_shared_appearance_check: Instant::now() - Duration::from_secs(1),
            show_transforms: false,
            show_preview: false,
            show_quick_settings: false,
            settings_category: SettingsCategory::Clipboard,
            settings_query: String::new(),
            window_pinned: false,
            was_focused: true,
            vault_targets: available_targets(),
            vault_selected: available_targets().first().copied(),
            vault_title: String::new(),
            vault_username: String::new(),
            vault_url: String::new(),
            vault_password_input: String::new(),
            vault_status: None,
            vault_secrets: Vec::new(),
            vault_revealed: None,
            vault_confirm_delete: None,
            snippets: Vec::new(),
            matched_snippets: Vec::new(),
            new_snippet_trigger: String::new(),
            new_snippet_name: String::new(),
            new_snippet_body: String::new(),
            custom_actions: load_actions().actions,
            new_action_name: String::new(),
            new_action_command: String::new(),
            new_action_auto: String::new(),
            new_action_output: ActionOutput::Clipboard,
            action_status: None,
            pairing: PairingState::Idle,
            nearby: Vec::new(),
            nearby_checked: None,
            transforms: paste_transforms(),
            paste_settings: load_paste_transform_settings(),
            cached_tfidf: None,
            privacy_config: load_privacy_config(),
            sessions,
            session_config,
            active_tab: if open_settings {
                MainTab::Settings
            } else {
                MainTab::Text
            },
            content_filter: ContentFilter::All,
            show_active_slots_only: false,
            new_excluded_app: String::new(),
            new_custom_pattern: String::new(),
            confirm_clear_all: false,
            export_status: None,
            collections: Vec::new(),
            starred_collection_id: None,
            starred_clip_ids: HashSet::new(),
            thumb_textures: std::collections::HashMap::new(),
            new_collection_name: String::new(),
            new_collection_app: String::new(),
            ai_result: None,
            ai_config: load_transform_config(),
            ai_key_input: String::new(),
            ai_test_status: None,
            ai_test_rx: None,
            ask: AskState::default(),
            transform_job: TransformJob::default(),
            hud,
            island_surface,
            hud_watcher_started: false,
            masked_clip_ids,
            want_key_window: false,
            secret_scan_cache,
            last_claim_refresh: Instant::now() - Duration::from_secs(60),
            island: island::IslandState::default(),
            quitting: false,
            popover_settings_open: false,
            // Resident HUD starts hidden; clipd-ui's show request is what
            // parks it under the tray extra.
            hud_expanded: false,
            hud_left_at: None,
            hud_grace_until: None,
            last_sent_size: None,
            last_sent_pos: None,
            last_surface_request_check: Instant::now() - Duration::from_secs(1),
            #[cfg(target_os = "macos")]
            last_daemon_check: Instant::now(),
            #[cfg(target_os = "macos")]
            glass_native: None,
        };
        app.refresh_collections();
        app.refresh_starred();
        app.apply_filter();
        app
    }

    /// Reload the list of collections from the store.
    fn refresh_collections(&mut self) {
        self.collections = self.store.list_collections().unwrap_or_default();
    }

    fn refresh_starred(&mut self) {
        self.starred_clip_ids.clear();
        self.starred_collection_id = self
            .store
            .get_collection_by_name(PINNED_COLLECTION_NAME)
            .ok()
            .flatten()
            .or_else(|| {
                self.store
                    .get_collection_by_name(LEGACY_STARRED_COLLECTION_NAME)
                    .ok()
                    .flatten()
            })
            .map(|collection| collection.id);
        if let Some(collection_id) = self.starred_collection_id {
            self.starred_clip_ids = self
                .store
                .collection_items(collection_id)
                .unwrap_or_default()
                .into_iter()
                .map(|item| item.clip_id)
                .collect();
        }
    }

    fn ensure_starred_collection(&mut self) -> Option<i64> {
        if let Some(id) = self.starred_collection_id {
            return Some(id);
        }
        let id = match self.store.get_collection_by_name(PINNED_COLLECTION_NAME) {
            Ok(Some(collection)) => Some(collection.id),
            Ok(None) => match self
                .store
                .get_collection_by_name(LEGACY_STARRED_COLLECTION_NAME)
            {
                Ok(Some(collection)) => Some(collection.id),
                _ => self
                    .store
                    .create_collection(PINNED_COLLECTION_NAME, None)
                    .ok(),
            },
            Err(_) => None,
        }?;
        self.starred_collection_id = Some(id);
        self.refresh_collections();
        Some(id)
    }

    fn toggle_starred(&mut self, clip_id: i64) {
        if self.starred_clip_ids.contains(&clip_id) {
            if let Some(collection_id) = self.starred_collection_id {
                let _ = self.store.remove_collection_item(collection_id, clip_id);
            }
            self.starred_clip_ids.remove(&clip_id);
        } else if let Some(collection_id) = self.ensure_starred_collection() {
            let _ = self.store.add_clip_to_collection(collection_id, clip_id);
            self.starred_clip_ids.insert(clip_id);
        }
        self.refresh_collections();
        self.apply_filter();
    }

    fn refresh(&mut self) {
        // Resolve the selection to a clip id *before* `clips` is replaced —
        // afterwards `filtered` holds indices into a vector that no longer
        // exists, and reading through them can be out of bounds.
        let selected_id = self
            .filtered
            .get(self.selected)
            .and_then(|&i| self.clips.get(i))
            .map(|c| c.id);

        self.clips = self.store.get_recent(MAX_LOADED_CLIPS).unwrap_or_default();
        sync_active_slot_labels(&self.store, &mut self.clips);
        self.masked_clip_ids =
            mask_secret_previews(&mut self.clips, &mut self.secret_scan_cache);
        self.sessions = compute_sessions(&self.clips, self.session_config.window_minutes);
        self.cached_tfidf = None; // invalidate — will be rebuilt lazily on next search
        self.refresh_snippets();
        self.apply_filter();

        // Ask mode keeps its own list (the retrieved clips), which apply_filter
        // deliberately leaves alone — so re-derive those indices here against
        // the clips we just reloaded, holding the user's place.
        if let Some(answer) = self.ask.answer.take() {
            self.show_retrieved(&answer, selected_id);
            self.ask.answer = Some(answer);
        }

        self.last_refresh = Instant::now();
    }

    fn refresh_snippets(&mut self) {
        self.snippets = self.store.list_snippets().unwrap_or_default();
    }

    /// Whether the search bar currently holds a question.
    fn surface_mode(&self) -> SurfaceMode {
        if self.island_surface {
            SurfaceMode::Island
        } else if self.hud {
            SurfaceMode::Hud
        } else if self.active_tab == MainTab::Settings {
            SurfaceMode::Settings
        } else {
            SurfaceMode::Main
        }
    }

    fn switch_surface(&mut self, ctx: &egui::Context, mode: SurfaceMode) {
        self.hud = mode == SurfaceMode::Hud;
        self.hud_expanded = false;
        self.hud_left_at = None;
        self.show_preview = false;
        self.show_quick_settings = false;
        self.search_query.clear();
        self.ask.reset();
        self.apply_filter();
        save_surface_state(mode);

        if self.hud {
            let size = self.panel_expanded_size();
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
            ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            self.last_sent_size = Some(size);
            if let Some(screen) = ctx
                .input(|i| i.viewport().monitor_size)
                .or_else(main_display_size)
            {
                let left = popover_left(size.x, screen, true);
                let pos = egui::pos2(left, HUD_TOP_MARGIN);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                self.last_sent_pos = Some(pos);
            }
        } else {
            self.active_tab = if mode == SurfaceMode::Settings {
                MainTab::Settings
            } else {
                MainTab::Text
            };
            let width = if mode == SurfaceMode::Settings {
                SETTINGS_W
            } else {
                COMPACT_W
            };
            let size = egui::vec2(width, WIN_H);
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::Normal,
            ));
            ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            if let Some(cursor) = global_cursor_position() {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(window_pos_at_cursor(
                    cursor,
                    size,
                    main_display_size(),
                )));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_search = mode == SurfaceMode::Main;
        }
        ctx.request_repaint();
    }

    /// Size of the open HUD panel.
    fn panel_expanded_size(&self) -> egui::Vec2 {
        egui::vec2(HUD_W, HUD_H)
    }

    /// Where the surface sits on screen, in points, for the current state.
    fn surface_screen_rect(&self) -> Option<egui::Rect> {
        let screen = main_display_size()?;
        let size = self.panel_expanded_size();
        // Use the actual window position if available; otherwise fall back to
        // the tray-anchored position the HUD opens at.
        let pos = self.last_sent_pos.unwrap_or_else(|| {
            let left = popover_left(size.x, screen, true);
            egui::pos2(left, HUD_TOP_MARGIN)
        });
        Some(egui::Rect::from_min_size(pos, size))
    }

    /// The HUD process is controlled by the tray for show/hide, but also
    /// does its own hover-leave detection so it stays open while the cursor
    /// is anywhere on the tray icon or the popover, and hides cleanly when
    /// the cursor leaves both.
    /// Wake the UI thread the moment the tray writes a show request.
    ///
    /// Without this the hidden HUD had to poll for the request itself, and
    /// polling means repainting: at a responsive interval that cost a quarter
    /// of a core to sit there doing nothing, and at a cheap interval the
    /// popover opened a fifth of a second late. A thread that stats one path
    /// costs neither — the UI thread idles, and this asks for a frame only
    /// when there is something to act on.
    fn ensure_hud_request_watcher(&mut self, ctx: &egui::Context) {
        if self.hud_watcher_started || !self.hud {
            return;
        }
        self.hud_watcher_started = true;
        let ctx = ctx.clone();
        let path = surface_request_path(SurfaceMode::Hud);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(15));
            if path.exists() {
                ctx.request_repaint();
            }
        });
    }

    fn drive_hud_hover(&mut self, ctx: &egui::Context) {
        self.ensure_hud_request_watcher(ctx);
        // Check for a "show" request from the tray.
        if let Some(mode) = take_surface_request_for(SurfaceMode::Hud) {
            if mode == SurfaceMode::Quit {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            } else if mode == SurfaceMode::Hidden {
                let cursor = global_cursor_position();
                let still_inside = match (cursor, self.surface_screen_rect()) {
                    (Some(cur), Some(rect)) => {
                        let mut hot = rect.expand(10.0);
                        hot.min.y = 0.0;
                        hot.contains(cur)
                    }
                    _ => false,
                };
                if !still_inside {
                    self.hide_hud_offscreen(ctx);
                    self.hud_left_at = None;
                }
                return;
            } else {
                self.search_query.clear();
                self.ask.reset();
                self.apply_filter();
                self.hud_left_at = None;
                self.hud_grace_until = Some(Instant::now() + Duration::from_millis(400));
                self.show_hud_onscreen(ctx);
                return;
            }
        }

        // Already hidden and nothing asked for it: there is nothing to drive.
        //
        // The leave-detection below only means anything while the popover is
        // up. Left running against a hidden window it cycled forever — start
        // the leave timer, wait out the collapse delay, hide an already-hidden
        // window, reset, repeat — repainting every 60ms the whole time. That
        // was a sixth of a core spent deciding to do nothing.
        if !self.hud_expanded {
            self.hud_left_at = None;
            return;
        }

        // Grace period right after showing — don't check hover-leave yet.
        if let Some(until) = self.hud_grace_until {
            if Instant::now() < until {
                self.animate_hud(ctx);
                return;
            }
            self.hud_grace_until = None;
        }

        let cursor = global_cursor_position();
        let pointer_inside = match (cursor, self.surface_screen_rect()) {
            (Some(cur), Some(rect)) => {
                let mut hot = rect.expand(10.0);
                hot.min.y = 0.0;
                let global_hit = hot.contains(cur);
                let egui_hit = ctx.input(|i| i.pointer.hover_pos()).is_some();
                global_hit || egui_hit
            }
            _ => ctx.input(|i| i.pointer.hover_pos()).is_some(),
        };

        if pointer_inside {
            self.hud_left_at = None;
            self.animate_hud(ctx);
            return;
        }

        let busy = self.ask.running
            || self.transform_job.running
            || !self.search_query.trim().is_empty()
            || self.ask.answer.is_some();
        if busy {
            self.hud_left_at = None;
            self.animate_hud(ctx);
            return;
        }

        match self.hud_left_at {
            None => {
                self.hud_left_at = Some(Instant::now());
                self.animate_hud(ctx);
            }
            Some(left) if left.elapsed() >= HUD_COLLAPSE_DELAY => {
                self.search_query.clear();
                self.ask.reset();
                self.apply_filter();
                self.hide_hud_offscreen(ctx);
                self.hud_left_at = None;
            }
            Some(_) => {
                self.animate_hud(ctx);
            }
        }
    }

    /// No animation — macOS window server can't handle 60 resizes/second
    /// without glitching. The HUD appears at full size instantly.
    fn animate_hud(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(Duration::from_millis(60));
    }

    fn hide_hud_offscreen(&mut self, ctx: &egui::Context) {
        // Hide the window, keep the process.
        //
        // This used to close outright and let clipd-ui spawn a fresh one per
        // hover, which meant every single open paid for a process start, a GPU
        // context and a window before anything could be drawn — hundreds of
        // milliseconds that no tuning further down the path can win back.
        // `show_hud_onscreen` was already written for a resident process: it
        // sets Visible(true) and repositions. This is the other half of that.
        self.hud_expanded = false;
        clipd_core::set_gui_window_open(false);
        // Park it off-screen rather than toggling Visible.
        //
        // Hiding the window and showing it again brought it back inert on
        // macOS — no hover highlight, clicks landing on nothing — because a
        // re-shown window does not become key the way a freshly created one
        // does, and Focus alone did not recover it. Moving it off-screen keeps
        // it a live, ordinary window the whole time, which is what the rest of
        // this file already assumes: the poll interval decides "hidden" by
        // testing whether the last sent position was above the screen.
        if let Some(screen) = main_display_size() {
            let parked = egui::pos2(0.0, -(screen.y + 200.0));
            self.last_sent_pos = Some(parked);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(parked));
        }
    }

    /// Show the HUD at full size, positioned under the tray icon. No animation —
    /// instant appearance to avoid macOS window resize glitching.
    fn show_hud_onscreen(&mut self, ctx: &egui::Context) {
        self.hud_expanded = true;
        // Catch up on everything copied while this window was parked.
        self.refresh();
        clipd_core::set_gui_window_open(true);
        if let Some(screen) = ctx
            .input(|i| i.viewport().monitor_size)
            .or_else(main_display_size)
        {
            let size = egui::vec2(HUD_W, HUD_H);
            let left = popover_left(HUD_W, screen, true);
            let pos = egui::pos2(left, HUD_TOP_MARGIN);
            self.last_sent_size = Some(size);
            self.last_sent_pos = Some(pos);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
            // Deliberately no Focus command here. It looked like the fix for
            // the popover coming back inert, but parking the window off-screen
            // was the actual cure — and taking focus on a *hover* means the
            // keys you are typing into your editor land in this search box
            // instead. A popover you summoned by pointing at something should
            // not take the keyboard away from you.
            self.focus_search = true;
            ctx.request_repaint();
        }
    }

    /// Render the HUD popover — always full expanded UI under the tray.
    fn render_hud_popover(&mut self, ctx: &egui::Context, c: &clipd_core::ThemeColors) {
        let mut action = Action::None;

        paint_glass_shell(ctx, self.theme, c, self.native_glass_active());

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(glass_panel_frost(self.theme))
                    .rounding(Rounding::same(SHELL_ROUND)),
            )
            .show(ctx, |ui| {
                paint_panel_glass_gradient(ui, self.theme);
                self.draw_popover_tail(ui, c);

                let card = egui::Frame::none()
                    .fill(Color32::TRANSPARENT)
                    .rounding(Rounding::same(18.0))
                    .stroke(Stroke::NONE)
                    // Top padding matches the tail; bottom padding clears the
                    // shell's rounded corner so the footer doesn't clip.
                    .inner_margin(Margin {
                        left: 12.0,
                        right: 12.0,
                        top: 7.0,
                        bottom: 14.0,
                    });

                card.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.render_hud_expanded(ui, &mut action, c);
                });
            });

        self.dispatch(action, ctx);
    }

    /// The little triangle that points from the popover up at the menu bar.
    fn draw_popover_tail(&self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        const TAIL_H: f32 = 8.0;
        const TAIL_HALF_W: f32 = 9.0;

        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, TAIL_H), egui::Sense::hover());

        // Point at the tray icon, not at the middle of the card. Near a screen
        // edge the card is clamped inward, so those two are not the same place
        // — and a tail aimed at nothing is worse than no tail.
        let cx = match (clipd_core::load_tray_anchor(), main_display_size()) {
            (Some(anchor), Some(screen)) if self.hud => {
                let card_left = popover_left(self.panel_expanded_size().x, screen, true);
                // Keep the tail on the card, with room for its own base.
                (anchor as f32 - card_left + rect.left())
                    .clamp(rect.left() + 16.0, rect.right() - 16.0)
            }
            _ => rect.center().x,
        };
        // Overlap the card by a hair so the shared edge has no seam.
        let base = rect.bottom() + 1.0;

        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(cx, rect.top()),
                egui::pos2(cx - TAIL_HALF_W, base),
                egui::pos2(cx + TAIL_HALF_W, base),
            ],
            surf(c, c.bg_surface),
            Stroke::new(0.8, rgb(c.border)),
        ));
    }

    /// Shut clipd down completely: other windows, the tray host, the daemon.
    ///
    /// The GUI cannot reach clipd-ui through the surface-request channel (that
    /// is GUI-only), so the tray host is signalled through the daemon lock it
    /// holds. Without that step clipd-ui survives, and its watchdog would put
    /// a GUI straight back.
    fn quit_everything(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        // Quit every surface process, not just the current one.
        send_surface_request_to(SurfaceMode::Main, SurfaceMode::Quit);
        send_surface_request_to(SurfaceMode::Hud, SurfaceMode::Quit);
        send_surface_request_to(SurfaceMode::Island, SurfaceMode::Quit);

        #[cfg(unix)]
        if let Some(pid) = clipd_core::daemon_lock_pid() {
            if pid != std::process::id() {
                // SIGTERM, not SIGKILL: clipd-ui stops the daemon and releases
                // its lock on the way out.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Footer, pinned to the bottom of whichever view is showing.
    fn render_hud_footer_row(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Action,
        c: &clipd_core::ThemeColors,
    ) {
        // Push the row to the bottom so it sits in the same place whether the
        // view above it fills the panel or not. The reserved height covers the
        // divider, the breathing room either side of it, and the 30pt buttons.
        let slack = (ui.available_height() - POPOVER_FOOTER_H).max(0.0);
        ui.add_space(slack.min(ui.available_height()));
        hairline(ui, c);
        // The list already ends with a rule, so the footer does not need to
        // draw its own — adding one stacked two hairlines a few points apart,
        // which reads as a mistake rather than as a divider.
        ui.add_space(9.0);
        self.render_hud_footer(ui, action, c);
        // Keep the buttons off the card's rounded edge.
        ui.add_space(3.0);
    }

    /// Settings, rendered in place of the clip list.
    ///
    /// These are the same controls the tray dropdown carries. Holding them
    /// here means the popover is the only surface: no second window landing on
    /// top of this one, and no two copies of the same toggle to keep in step.
    fn render_hud_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        egui::ScrollArea::vertical()
            .id_salt("hud_settings")
            // Leave room for the footer. With too little reserved the scroll
            // area eats it, taking the gear — the one control that gets you
            // back out of settings — with it.
            .max_height((ui.available_height() - POPOVER_FOOTER_H).max(60.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(2.0);

                // Grouped like the reference: a small-caps heading, then the
                // rows that belong under it in one card.
                let mut settings_dirty = false;

                popover_section_header(ui, c, "Behavior");
                let group = |ui: &mut egui::Ui| {
                    egui::Frame::none()
                        .fill(surf(c, c.bg_surface))
                        .rounding(Rounding::same(12.0))
                        .stroke(Stroke::new(0.7, rgb(c.border).gamma_multiply(0.8)))
                        .inner_margin(Margin::symmetric(6.0, 4.0))
                };

                let hover_on = self.paste_settings.hover_opens_hud;
                let feedback_on = self.paste_settings.hud_enabled;
                group(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if popover_setting_row(
                        ui,
                        c,
                        FooterIcon::Eye,
                        "Show clips on hover",
                        "Open the clipboard palette on hover",
                        RowControl::Toggle(hover_on),
                    ) {
                        self.paste_settings.hover_opens_hud = !hover_on;
                        settings_dirty = true;
                    }
                    hairline(ui, c);
                    if popover_setting_row(
                        ui,
                        c,
                        FooterIcon::Sparkle,
                        "Slot copy feedback",
                        "Flash a small confirmation when copied",
                        RowControl::Toggle(feedback_on),
                    ) {
                        self.paste_settings.hud_enabled = !feedback_on;
                        settings_dirty = true;
                    }
                    hairline(ui, c);
                    // Two things to flip, not one: the layout lives in
                    // paste_transform.json, but the island is its own process
                    // — saving the setting alone would leave the old surface
                    // on screen until the next login.
                    let island_on = self.paste_settings.gui_layout == GuiLayout::Notch;
                    if popover_setting_row(
                        ui,
                        c,
                        FooterIcon::List,
                        "Dynamic Island",
                        "Show the slab that hugs the notch",
                        RowControl::Toggle(island_on),
                    ) {
                        self.paste_settings.gui_layout = if island_on {
                            GuiLayout::Palette
                        } else {
                            GuiLayout::Notch
                        };
                        settings_dirty = true;
                        if island_on {
                            island::stop_island();
                        } else {
                            island::start_island();
                        }
                    }
                });

                if settings_dirty {
                    save_paste_transform_settings(&self.paste_settings);
                }

                popover_section_header(ui, c, "Preferences");
                group(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if popover_setting_row(
                        ui,
                        c,
                        FooterIcon::Gear,
                        "All settings",
                        "Themes, privacy, snippets, actions, AI model",
                        RowControl::Chevron,
                    ) {
                        spawn_palette(&["--settings"]);
                        self.popover_settings_open = false;
                    }
                });

                popover_section_header(ui, c, "Clipboard");
                group(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if popover_setting_row(
                        ui,
                        c,
                        FooterIcon::Clipboard,
                        "Open full clipboard",
                        "View all clips, collections & previews",
                        RowControl::Chevron,
                    ) {
                        spawn_palette(&[]);
                        self.popover_settings_open = false;
                    }
                });

                popover_section_header(ui, c, "App");
                group(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if popover_setting_row(
                        ui,
                        c,
                        FooterIcon::Power,
                        "Quit Clipd",
                        "Close all windows and stop the daemon",
                        RowControl::Chevron,
                    ) {
                        self.quit_everything(ui.ctx());
                    }
                });

                ui.add_space(4.0);
            });
    }

    /// Footer: one row of round glass buttons, the way a menu-bar popover ends.
    ///
    /// The old footer mixed a text label with word chips, which made the panel
    /// bottom-heavy and read like a status bar. Icons keep the weight on the
    /// clips above, and every button here maps to something clipd actually
    /// does — no decorative controls.
    fn render_hud_footer(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Action,
        c: &clipd_core::ThemeColors,
    ) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;

            if glass_line_button(ui, FooterIcon::Sparkle, self.in_ask_mode(), c)
                .on_hover_text("Ask about your clips")
                .clicked()
            {
                self.toggle_ask_mode();
            }

            if glass_line_button(ui, FooterIcon::List, false, c)
                .on_hover_text("Open the full clipboard window")
                .clicked()
            {
                spawn_palette(&[]);
            }

            // Settings open *inside* the popover. Spawning the palette here
            // put a second window in the same corner of the screen as this
            // one, which is the overlap the tray dropdown already caused.
            if glass_line_button(ui, FooterIcon::Gear, self.popover_settings_open, c)
                .on_hover_text(if self.popover_settings_open {
                    "Back to clips"
                } else {
                    "Settings"
                })
                .clicked()
            {
                self.popover_settings_open = !self.popover_settings_open;
            }

            let feedback_on = self.paste_settings.hud_enabled;
            if glass_line_button(ui, FooterIcon::Eye, feedback_on, c)
                .on_hover_text(if feedback_on {
                    "Slot copy feedback is on"
                } else {
                    "Slot copy feedback is off"
                })
                .clicked()
            {
                self.paste_settings.hud_enabled = !feedback_on;
                save_paste_transform_settings(&self.paste_settings);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Built exactly like its neighbours. It used to allocate 30pt
                // against their 38 and paint no circle behind it, so it sat off
                // the row's centre line and read as a stray mark rather than a
                // button — which is what made the footer look crooked.
                let resp = glass_line_button(ui, FooterIcon::Power, false, c)
                    .on_hover_text("Quit clipd — closes every window and stops the daemon");
                if resp.clicked() {
                    self.quit_everything(ui.ctx());
                }
            });
        });
        let _ = action;
    }

    fn render_hud_expanded(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Action,
        c: &clipd_core::ThemeColors,
    ) {
        // Search + Ask chip, held in one quiet inset surface like a native
        // menu-bar popover search field.
        egui::Frame::none()
            .fill(surf(c, c.bg_elevated))
            .rounding(Rounding::same(12.0))
            .stroke(Stroke::new(0.7, rgb(c.border)))
            .inner_margin(Margin::symmetric(10.0, 7.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    draw_search_icon(ui, rgb(c.accent).gamma_multiply(0.9));
                    ui.add_space(6.0);

                    let asking = self.in_ask_mode();
                    let field_w = (ui.available_width() - 68.0).max(80.0);
                    let search = ui.add_sized(
                        [field_w, 20.0],
                        egui::TextEdit::singleline(&mut self.search_query)
                            .id(egui::Id::new("hud_search"))
                            .hint_text(if asking {
                                "Ask, then press Enter"
                            } else {
                                "Search clipboard…"
                            })
                            .frame(false)
                            .font(egui::TextStyle::Body),
                    );
                    if self.focus_search {
                        search.request_focus();
                        self.focus_search = false;
                    }
                    if search.changed() {
                        if asking {
                            self.ask.clear_answer();
                        }
                        self.apply_filter();
                    }
                    if search.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && asking
                    {
                        *action = Action::Ask;
                    }
                });
            });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        // The body swaps between three views; the footer belongs to all of
        // them. Returning early here left settings and ask mode with no
        // visible way back — the gear that toggles them lives in the footer.
        if self.in_ask_mode() {
            render_ask_panel(ui, &self.ask, action, c);
            self.render_hud_footer_row(ui, action, c);
            return;
        }

        if self.popover_settings_open {
            self.render_hud_settings(ui, c);
            self.render_hud_footer_row(ui, action, c);
            return;
        }

        // Compact clip list.
        // Stop short of the card's bottom edge. Without this the list scrolls
        // right off the window and the rounded corner is sliced away by a
        // half-drawn row.
        let list_h = (ui.available_height() - POPOVER_FOOTER_H).max(60.0);
        egui::ScrollArea::vertical()
            .id_salt("hud_list")
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.filtered.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Nothing here yet — copy something.")
                                .size(12.0)
                                .color(rgb(c.subtext)),
                        );
                    });
                    return;
                }

                // Type and sensitivity travel with the row now: the
                // reference leads every row with a glyph for what the clip is,
                // and that cannot be worked out from the preview text.
                let rows: Vec<(
                    usize,
                    i64,
                    String,
                    String,
                    String,
                    Option<u8>,
                    ContentType,
                    bool,
                )> = self
                    .filtered
                    .iter()
                    .take(40)
                    .filter_map(|&i| self.clips.get(i).map(|clip| (i, clip)))
                    .map(|(i, clip)| {
                        // Some clips (images, whitespace-only copies) carry an
                        // empty preview. A blank row reads as a rendering bug,
                        // so fall back to the content, then to a label.
                        let mut text = one_line_preview(&clip.preview, 58);
                        if text.is_empty() {
                            text = one_line_preview(&clip.content, 58);
                        }
                        if text.is_empty() {
                            text = match clip.content_type {
                                ContentType::Image => "Image".to_string(),
                                _ => "(empty)".to_string(),
                            };
                        }
                        (
                            i,
                            clip.id,
                            text,
                            clip.source_app.clone().unwrap_or_default(),
                            relative_time_short(&clip.timestamp),
                            clip.slot,
                            clip.content_type.clone(),
                            !detect_sensitive(&clip.content, &self.privacy_config).is_empty(),
                        )
                    })
                    .collect();

                // Ruled rows that share edges, so the list reads as one sheet.
                let row_count = rows.len();
                ui.spacing_mut().item_spacing.y = 0.0;
                for (pos, (_idx, clip_id, preview, _app, time, slot, kind, sensitive)) in
                    rows.into_iter().enumerate()
                {
                    let selected = self.selected == pos;
                    let starred = self.starred_clip_ids.contains(&clip_id);
                    let mut star_clicked = false;
                    let mut copy_clicked = false;
                    // Hover decides what the row's right-hand end shows, so it
                    // has to be read before the row is built.
                    let hover_id = egui::Id::new(("hud_row_hover", clip_id));
                    let hovered = ui
                        .ctx()
                        .read_response(hover_id)
                        .map_or(false, |r| r.contains_pointer());
                    let lit = selected || hovered;
                    // Flat ruled rows on an opaque theme; frosted panes with
                    // lit rims on glass. On glass a full-width fill with no
                    // edge is just a brighter patch of fog — the rim is what
                    // makes it a surface.
                    let glass_rows = self.theme.is_glass();
                    let (row_fill, row_stroke) = if glass_rows {
                        (
                            glass_row_fill(self.theme, selected, hovered)
                                .unwrap_or(Color32::TRANSPARENT),
                            glass_row_stroke(self.theme, selected),
                        )
                    } else if lit {
                        (surf(c, c.bg_selected), Stroke::NONE)
                    } else {
                        (Color32::TRANSPARENT, Stroke::NONE)
                    };
                    if glass_rows {
                        ui.add_space(3.0);
                    }
                    let frame = egui::Frame::none()
                        .fill(row_fill)
                        .rounding(if glass_rows {
                            Rounding::same(10.0)
                        } else {
                            Rounding::ZERO
                        })
                        .stroke(row_stroke)
                        .inner_margin(Margin::symmetric(12.0, 9.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                draw_type_tile(ui, &kind, sensitive, false, c);
                                ui.add_space(10.0);
                                // One line, not two. The source app and the
                                // time used to sit under the title; the
                                // reference puts the time at the right-hand
                                // end and drops the app, which halves the row
                                // height and lets the list show twice as much.
                                //
                                // The title goes in a sub-ui of its own. It
                                // was written straight into this horizontal
                                // layout after a `set_width`, and `set_width`
                                // on a horizontal ui re-anchors that ui rather
                                // than reserving a slot in it — so the label
                                // was laid out from the row's left edge and
                                // printed on top of the glyph, with Copy and
                                // the pin landing on top of the title in turn.
                                let trailing = if lit { 96.0 } else { 52.0 };
                                let content_w = (ui.available_width() - trailing).max(70.0);
                                ui.allocate_ui(egui::vec2(content_w, 22.0), |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&preview)
                                                    .size(13.4)
                                                    .color(rgb(c.text)),
                                            )
                                            .truncate(),
                                        );
                                        if starred {
                                            let (dot, _) = ui.allocate_exact_size(
                                                egui::vec2(6.0, 6.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().circle_filled(
                                                dot.center(),
                                                2.6,
                                                rgb(c.overlay),
                                            );
                                        }
                                    });
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if lit {
                                            // The live row trades its
                                            // timestamp for what you can do
                                            // to it — the reference's Copy
                                            // and pin.
                                            // The small star, not the
                                            // footer's circular pin: that one
                                            // is built at footer size and put
                                            // a 44pt disc in the middle of a
                                            // 34pt row.
                                            if row_star_quiet(ui, starred, c).clicked() {
                                                star_clicked = true;
                                            }
                                            ui.add_space(2.0);
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new("Copy")
                                                            .size(11.5)
                                                            .color(rgb(c.subtext)),
                                                    )
                                                    .fill(Color32::TRANSPARENT)
                                                    .stroke(Stroke::NONE),
                                                )
                                                .clicked()
                                            {
                                                copy_clicked = true;
                                            }
                                        } else if let Some(n) = slot {
                                            // A slotted row keeps its number:
                                            // that is the key you press to
                                            // paste it back.
                                            ui.label(
                                                RichText::new(format!(
                                                    "⌘{}",
                                                    clipd_core::slot_badge(n)
                                                ))
                                                .size(11.0)
                                                .color(rgb(c.accent)),
                                            );
                                        } else {
                                            ui.label(
                                                RichText::new(&time)
                                                    .size(11.5)
                                                    .color(rgb(c.subtext)),
                                            );
                                        }
                                    },
                                );
                            });
                        });

                    // The rule between rows, drawn on every row but the last
                    // — and only where the rows share edges. On glass each
                    // pane has its own rim, so a rule as well would draw two
                    // lines in the same place.
                    {
                        let r = frame.response.rect;
                        if pos + 1 < row_count && !glass_rows {
                            ui.painter().hline(
                                (r.left() + 12.0)..=(r.right() - 12.0),
                                r.bottom(),
                                Stroke::new(0.6, rgb(c.border)),
                            );
                        }
                        ui.interact(r, hover_id, egui::Sense::hover());
                    }

                    if copy_clicked {
                        self.selected = pos;
                        *action = Action::Copy;
                    }
                    if star_clicked {
                        self.selected = pos;
                        *action = Action::ToggleStar(clip_id);
                    }
                    let hit = ui.interact(
                        egui::Rect::from_min_max(
                            frame.response.rect.min,
                            egui::pos2(
                                frame.response.rect.right() - 96.0,
                                frame.response.rect.bottom(),
                            ),
                        ),
                        egui::Id::new(("hud_row", clip_id)),
                        egui::Sense::click(),
                    );
                    if hit.clicked() && !star_clicked && !copy_clicked {
                        self.selected = pos;
                        *action = Action::Copy;
                    }
                    if hit.double_clicked() && !star_clicked && !copy_clicked {
                        self.selected = pos;
                        *action = Action::Paste;
                    }
                }
            });

        self.render_hud_footer_row(ui, action, c);
    }

    /// Run a transform off the UI thread and report back through a channel.
    fn start_transform(&mut self, kind: TransformKind, input: String, ctx: &egui::Context) {
        if self.transform_job.running {
            return;
        }
        let config = load_transform_config();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let job_kind = kind.clone();

        std::thread::spawn(move || {
            let result = clipd_core::apply_transform(&job_kind, &input, &config);
            let _ = tx.send(result);
            ctx.request_repaint();
        });

        self.transform_job = TransformJob {
            running: true,
            label: kind.label().to_string(),
            rx: Some(rx),
            result: None,
        };
    }

    /// Collect a finished transform, if one landed since the last frame.
    fn poll_transform(&mut self) {
        let Some(rx) = &self.transform_job.rx else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.transform_job.running = false;
                self.transform_job.rx = None;
                self.transform_job.result = Some(result);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.transform_job.running = false;
                self.transform_job.rx = None;
                self.transform_job.result =
                    Some(Err("The transform worker stopped unexpectedly.".into()));
            }
        }
    }

    /// The ✦ chip on a row: ask about that specific clip. Seeds the question
    /// from the clip's own preview so the retriever has real terms to match on
    /// — "tell me about this" alone retrieves nothing.
    fn ask_about_clip(&mut self, clip_id: i64, ctx: &egui::Context) {
        let Some(clip) = self.clips.iter().find(|c| c.id == clip_id) else {
            return;
        };
        let hint = one_line_preview(&clip.preview, 60);
        self.search_query = format!("? what is this, and where did I copy it from: {}", hint);
        self.ask.reset();
        self.start_ask(ctx);
    }

    /// Run a Smart Recommend chip. Transforms go through the existing
    /// background-action path; Ask suggestions just steer the search bar.
    fn run_suggestion(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(clip) = self.selected_clip().cloned() else {
            return;
        };
        let suggestions = clipd_core::suggest_for(&clip);
        let Some(suggestion) = suggestions.get(idx).cloned() else {
            return;
        };

        match suggestion.kind {
            clipd_core::SuggestionKind::Ask(question) => {
                self.search_query = format!("? {}", question);
                self.ask.reset();
                self.start_ask(ctx);
            }
            clipd_core::SuggestionKind::Transform(kind) => {
                self.start_transform(kind, clip.content.clone(), ctx);
            }
        }
    }

    /// Flip between search and ask, preserving whatever the user has typed —
    /// a half-typed query is usually the thing they want to ask about.
    fn toggle_ask_mode(&mut self) {
        if self.in_ask_mode() {
            self.search_query = self
                .search_query
                .trim_start_matches('?')
                .trim_start()
                .to_string();
            self.ask.reset();
            self.apply_filter();
        } else {
            self.search_query = format!("? {}", self.search_query.trim());
        }
        self.focus_search = true;
    }

    fn in_ask_mode(&self) -> bool {
        self.search_query.trim().starts_with('?') || ask_query(&self.search_query).is_some()
    }

    /// Fire the question on a worker thread. The worker opens its own store
    /// handle — SQLite is happy with concurrent readers, and `ClipStore` is
    /// not shareable across threads.
    fn start_ask(&mut self, ctx: &egui::Context) {
        let Some(question) = ask_query(&self.search_query) else {
            return;
        };
        if self.ask.running {
            return;
        }

        let question = question.to_string();
        let worker_question = question.clone();
        let thread = self.ask.thread.clone();
        let filters = AskFilters::default();
        let cfg = AskConfig::default();
        let api = load_transform_config();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let result = match ClipStore::new(&ClipStore::default_path()) {
                Ok(store) => {
                    clipd_core::ask(&store, &worker_question, &thread, &filters, &cfg, &api)
                }
                Err(e) => Err(format!("Could not open the clip database: {}", e)),
            };
            // If the window closed mid-flight the receiver is gone; that's a
            // normal shutdown, not an error worth surfacing.
            let _ = tx.send(result);
            ctx.request_repaint();
        });

        self.ask.question = question;
        self.ask.running = true;
        self.ask.clear_answer();
        self.ask.rx = Some(rx);
    }

    /// Collect a finished ask, if one landed since the last frame.
    fn poll_ask(&mut self) {
        let Some(rx) = &self.ask.rx else { return };
        let received = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ask.running = false;
                self.ask.rx = None;
                self.ask.error = Some("The ask worker stopped unexpectedly.".into());
                return;
            }
        };

        self.ask.running = false;
        self.ask.rx = None;

        match received {
            Ok(answer) => {
                // Show the evidence: the list narrows to exactly the clips the
                // answer was allowed to draw on, so citations and rows agree.
                self.show_retrieved(&answer, None);
                if !answer.retrieval_only {
                    self.ask.thread.record(&self.store, &answer);
                }
                self.ask.answer = Some(answer);
            }
            Err(e) => self.ask.error = Some(e),
        }
    }

    /// Narrow the visible list to the retrieved clips, ranked order preserved.
    /// `keep_selected` holds the user's place across a background refresh;
    /// `None` means this is a fresh answer, so jump to the top hit.
    fn show_retrieved(&mut self, answer: &AskAnswer, keep_selected: Option<i64>) {
        let ranked: Vec<usize> = answer
            .retrieved
            .iter()
            .filter_map(|r| self.clips.iter().position(|c| c.id == r.clip.id))
            .collect();

        if ranked.is_empty() {
            return;
        }
        self.filtered = ranked;

        match keep_selected
            .and_then(|id| self.filtered.iter().position(|&i| self.clips[i].id == id))
        {
            // Held our place — don't yank the scroll position out from under
            // someone who is reading.
            Some(pos) => self.selected = pos,
            None => {
                self.selected = 0;
                self.scroll_to_selected = true;
            }
        }
    }

    /// Jump the list selection to a cited clip. Returns false when the clip is
    /// no longer in the loaded window (history is capped at MAX_LOADED_CLIPS).
    fn jump_to_clip(&mut self, clip_id: i64) -> bool {
        let Some(idx) = self.clips.iter().position(|c| c.id == clip_id) else {
            return false;
        };
        if !self.filtered.contains(&idx) {
            self.filtered.push(idx);
        }
        if let Some(pos) = self.filtered.iter().position(|&i| i == idx) {
            self.selected = pos;
            self.scroll_to_selected = true;
        }
        true
    }

    fn apply_filter(&mut self) {
        // In ask mode the query is a sentence, not a filter — running it
        // through the substring matcher would empty the list on every
        // keystroke. The list is repopulated with the retrieved clips once an
        // answer lands; until then it keeps showing recent history. Note this
        // must not clear the answer: refresh() calls apply_filter on a timer.
        if self.in_ask_mode() {
            return;
        }

        // ── Slot filter: only show clips saved to a slot ──
        let mut base_indices: Vec<usize> = if self.show_active_slots_only {
            self.clips
                .iter()
                .enumerate()
                .filter(|(_, c)| c.slot.is_some())
                .map(|(i, _)| i)
                .collect()
        } else {
            (0..self.clips.len()).collect()
        };

        // Content rail. Favorites is intentionally a filter rather than a
        // separate data screen so search, keyboard navigation and paste retain
        // identical behaviour in every category.
        let content_filter = self.content_filter;
        let starred = &self.starred_clip_ids;
        // Borrowed alongside the others: the API-keys filter asks the same
        // detector that decides whether a row wears the key glyph, so the two
        // can never disagree about what counts as a key.
        let privacy = &self.privacy_config;
        base_indices.retain(|&i| {
            let clip = &self.clips[i];
            match content_filter {
                ContentFilter::All => true,
                ContentFilter::Favorites => starred.contains(&clip.id),
                ContentFilter::Slots => clip.slot.is_some(),
                ContentFilter::Text => {
                    matches!(clip.content_type, ContentType::Text | ContentType::Unknown)
                }
                ContentFilter::Links => {
                    matches!(clip.content_type, ContentType::Url | ContentType::Email)
                }
                ContentFilter::Code => clip.content_type == ContentType::Code,
                ContentFilter::Images => clip.content_type == ContentType::Image,
                ContentFilter::Files => clip.content_type == ContentType::Path,
                ContentFilter::ApiKeys => detect_sensitive(&clip.content, privacy)
                    .iter()
                    .any(|m| m.kind.is_api_key()),
            }
        });

        // Recall-by-source: "from chrome", "json from chrome", or "app:chrome"
        // filter to clips copied from that app — so you never need a slot number.
        let (content_q, app_q) = split_from_query(&self.search_query.to_lowercase());
        if !app_q.is_empty() {
            base_indices.retain(|&i| {
                let clip = &self.clips[i];
                // Provenance match: app name OR window title, so "from
                // datagrip" and "from jira" both work.
                clip.source_app
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&app_q)
                    || clip
                        .source_title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&app_q)
            });
        }

        if content_q.is_empty() {
            self.filtered = base_indices;
        } else {
            // Hybrid search by default — exact keyword matches first, then local
            // semantic (TF-IDF) matches appended. Both are instant and offline
            // (no per-keystroke network calls), so search "just works".
            let base_set: HashSet<usize> = base_indices.iter().copied().collect();
            let q = content_q.clone();
            let mut ordered: Vec<usize> = Vec::new();
            let mut seen: HashSet<usize> = HashSet::new();

            // 1) Exact keyword matches (content / preview / source app), in order.
            for &i in &base_indices {
                let c = &self.clips[i];
                let hit = c.content.to_lowercase().contains(&q)
                    || c.preview.to_lowercase().contains(&q)
                    || c.source_app
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
                    || c.source_title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q);
                if hit && seen.insert(i) {
                    ordered.push(i);
                }
            }

            // 2) Semantic (meaning-based) matches via TF-IDF, appended.
            if content_q.len() >= 2 {
                if self.cached_tfidf.is_none() {
                    let docs: Vec<&str> = self.clips.iter().map(|c| c.content.as_str()).collect();
                    self.cached_tfidf = Some(TfIdfIndex::build(&docs));
                }
                if let Some(ref index) = self.cached_tfidf {
                    for r in index.search(&content_q, 50) {
                        let i = r.clip_index;
                        if base_set.contains(&i) && seen.insert(i) {
                            ordered.push(i);
                        }
                    }
                }
            }

            self.filtered = ordered;
        }

        // Pinned clips form the first visual section, matching the reference.
        // sort_by_key is stable, so recency is preserved within both groups.
        self.filtered
            .sort_by_key(|&i| !self.starred_clip_ids.contains(&self.clips[i].id));
        // Top result is selected so Enter pastes the best match immediately.
        self.selected = 0;
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }

        // Snippet recall: a snippet whose trigger/name matches the typed query
        // surfaces at the top of the palette (Enter pastes its body). Exact
        // trigger matches rank first.
        let q = content_q.trim().to_lowercase();
        self.matched_snippets = if q.is_empty() {
            Vec::new()
        } else {
            let mut hits: Vec<clipd_core::Snippet> = self
                .snippets
                .iter()
                .filter(|s| {
                    let t = s.trigger.to_lowercase();
                    t.contains(&q) || s.name.to_lowercase().contains(&q)
                })
                .cloned()
                .collect();
            hits.sort_by_key(|s| s.trigger.to_lowercase() != q); // exact trigger first
            hits
        };
    }

    fn selected_clip(&self) -> Option<&ClipEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.clips.get(i))
    }

    /// Copy the selected clip to the clipboard. clipd stays in front.
    fn set_clipboard(&mut self, text: &str) -> bool {
        if let Ok(mut cb) = Clipboard::new() {
            if cb.set_text(text).is_ok() {
                self.copied_at = Some(Instant::now());
                return true;
            }
        }
        false
    }

    /// Put an image clip's PNG on the clipboard (so it pastes as an image).
    fn set_clipboard_image(&mut self, path: &str) -> bool {
        let Ok((w, h, rgba)) = clipd_core::load_rgba(std::path::Path::new(path)) else {
            return false;
        };
        if let Ok(mut cb) = Clipboard::new() {
            let img = arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: rgba.into(),
            };
            if cb.set_image(img).is_ok() {
                self.copied_at = Some(Instant::now());
                return true;
            }
        }
        false
    }

    /// Put a file clip's files back on the clipboard, so Cmd+V in Finder
    /// pastes the actual files rather than their paths as text.
    fn set_clipboard_files(&mut self, clip: &ClipEntry) -> bool {
        // Files whose blob was pruned *and* whose original has moved can't be
        // pasted; paste the ones that survive rather than failing outright.
        let paths: Vec<std::path::PathBuf> =
            clip.files.iter().filter_map(|f| f.resolve()).collect();
        if paths.is_empty() {
            return false;
        }
        if clipd_core::clipboard_write_file_urls(&paths).is_ok() {
            self.copied_at = Some(Instant::now());
            return true;
        }
        false
    }

    /// The "Sending" settings group: which machines this one can send to, and
    /// the pairing flow that authorises new ones.
    fn render_sending_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        match &self.pairing {
            PairingState::Searching { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Looking for the other machine…")
                            .size(12.0)
                            .color(rgb(c.text)),
                    );
                });
                settings_card_copy(
                    ui,
                    c,
                    "Start pairing on the other machine too",
                    "Open clipd there and click Pair a machine, or run `clipd pair`.",
                );
                ui.add_space(6.0);
                if vault_action(ui, c, "Cancel", false, false).clicked() {
                    self.cancel_pairing();
                }
            }

            PairingState::Confirming(offer) => {
                let (code, name) = (offer.confirmation_code.clone(), offer.name.clone());
                ui.label(
                    RichText::new(format!("Found {name}"))
                        .size(12.0)
                        .color(rgb(c.text)),
                );
                ui.add_space(8.0);
                // The code is the security of the whole step, so it gets the
                // visual weight of the thing the user is actually meant to do.
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(&code)
                            .size(34.0)
                            .strong()
                            .monospace()
                            .color(rgb(c.text)),
                    );
                });
                ui.add_space(8.0);
                settings_card_copy(
                    ui,
                    c,
                    &format!("This code must be showing on {name} right now"),
                    "If the two don't match, something is intercepting — choose Don't match.",
                );
                ui.add_space(8.0);

                let mut decision: Option<bool> = None;
                ui.horizontal(|ui| {
                    if ui.button("They match — pair").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Don't match").clicked() {
                        decision = Some(false);
                    }
                });

                match decision {
                    Some(true) => {
                        // Borrow ends here, so the offer can be consumed.
                        let PairingState::Confirming(offer) =
                            std::mem::replace(&mut self.pairing, PairingState::Idle)
                        else {
                            unreachable!("just matched Confirming")
                        };
                        self.pairing = match offer.accept() {
                            Ok(()) => PairingState::Done(format!("Paired with {name}.")),
                            Err(e) => PairingState::Failed(e),
                        };
                    }
                    Some(false) => {
                        self.pairing = PairingState::Failed(
                            "Cancelled — nothing was paired. Try again somewhere \
                             you trust the network."
                                .to_string(),
                        );
                    }
                    None => {}
                }
            }

            PairingState::Done(msg) | PairingState::Failed(msg) => {
                let ok = matches!(self.pairing, PairingState::Done(_));
                ui.label(
                    RichText::new(format!("{} {msg}", if ok { "✅" } else { "⚠" }))
                        .size(12.0)
                        .color(rgb(if ok { c.text } else { c.subtext })),
                );
                ui.add_space(6.0);
                if ui.button("OK").clicked() {
                    self.pairing = PairingState::Idle;
                }
            }

            PairingState::Idle => {
                let paired = clipd_core::lan_identity::trusted_peers();
                let mut forget: Option<(String, String)> = None;

                if paired.is_empty() {
                    settings_card_copy(
                        ui,
                        c,
                        "No machines paired yet",
                        "Pair another computer to send clips, links and files straight to it.",
                    );
                } else {
                    let mut first = true;
                    for p in paired.values() {
                        if !first {
                            settings_card_divider(ui, c);
                        }
                        first = false;
                        settings_value_row(
                            ui,
                            c,
                            FooterIcon::Send,
                            &p.name,
                            "Paired — clips send here",
                            80.0,
                            |ui| {
                                if ui.small_button("Forget").clicked() {
                                    forget = Some((p.device_id.clone(), p.name.clone()));
                                }
                            },
                        );
                    }
                }

                if let Some((device_id, name)) = forget {
                    self.pairing = match clipd_core::lan_identity::forget_peer(&device_id) {
                        Ok(_) => PairingState::Done(format!(
                            "Forgot {name}. Pair again to send to it."
                        )),
                        Err(e) => PairingState::Failed(e),
                    };
                }

                // Surface machines that are visible but unusable. Without this
                // the only symptom is a send that fails, long after the moment
                // when pairing would have been obvious.
                let unpaired: Vec<String> = self
                    .nearby_machines()
                    .iter()
                    .filter(|r| r.lan.is_some() && !paired.contains_key(&r.device_id))
                    .map(|r| r.name.clone())
                    .collect();
                if !unpaired.is_empty() {
                    ui.add_space(6.0);
                    settings_card_copy(
                        ui,
                        c,
                        &format!("{} on this network, not paired", unpaired.join(", ")),
                        "Pair to send clips there.",
                    );
                }

                ui.add_space(4.0);
                settings_card_divider(ui, c);
                if popover_setting_row(
                    ui,
                    c,
                    FooterIcon::Send,
                    "Pair a machine…",
                    "Both machines show a six-digit code — they must match",
                    RowControl::Chevron,
                ) {
                    self.begin_pairing();
                }
            }
        }
    }

    /// Start looking for another machine to pair with.
    ///
    /// The search runs on a worker thread — it blocks for up to a minute, and
    /// the window has to keep drawing (and offer Cancel) throughout.
    fn begin_pairing(&mut self) {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_stop = stop.clone();

        match std::thread::Builder::new()
            .name("clipd-gui-pairing".into())
            .spawn(move || {
                let _ = tx.send(clipd_core::lan_pair::discover_and_exchange(worker_stop));
            }) {
            Ok(_) => self.pairing = PairingState::Searching { stop, result: rx },
            Err(e) => self.pairing = PairingState::Failed(format!("Couldn't start pairing: {e}")),
        }
    }

    /// Advance the pairing state machine. Called every frame while Settings is
    /// showing; cheap unless a search is actually running.
    fn poll_pairing(&mut self, ctx: &egui::Context) {
        let PairingState::Searching { result, .. } = &self.pairing else {
            return;
        };
        match result.try_recv() {
            Ok(Ok(offer)) => self.pairing = PairingState::Confirming(offer),
            Ok(Err(e)) => self.pairing = PairingState::Failed(e),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Keep animating the spinner and re-checking, even with no
                // input events arriving.
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pairing =
                    PairingState::Failed("Pairing stopped unexpectedly.".to_string());
            }
        }
    }

    /// Cancel a search in flight. Dropping the state also drops the offer, and
    /// with it the machine-wide pairing lock.
    fn cancel_pairing(&mut self) {
        if let PairingState::Searching { stop, .. } = &self.pairing {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.pairing = PairingState::Idle;
    }

    /// Machines reachable right now, re-read at most every couple of seconds.
    fn nearby_machines(&mut self) -> &[clipd_core::sync::Reachable] {
        let stale = self
            .nearby_checked
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(2));
        if stale {
            self.nearby = clipd_core::sync::reachable_devices();
            self.nearby_checked = Some(Instant::now());
        }
        &self.nearby
    }

    /// Send the selected clip to the other Mac.
    ///
    /// Result goes to the status banner rather than a modal: a send that needs
    /// dismissing costs more attention than the send itself saved.
    fn do_send(&mut self) {
        let Some(clip) = self.selected_clip().cloned() else {
            self.action_status = Some((false, "No clip selected.".into()));
            return;
        };
        match clipd_core::sync::send_clip(&clip, None) {
            Ok(device) => {
                self.action_status =
                    Some((true, format!("Sent to {} · U to undo", device.name)));
            }
            Err(e) => self.action_status = Some((false, e)),
        }
    }

    /// Take back the last send, if the other Mac hasn't collected it yet.
    fn do_undo_send(&mut self) {
        match clipd_core::sync::recall_last() {
            Ok((last, true)) => {
                self.action_status =
                    Some((true, format!("Took it back before {} got it", last.device_name)));
            }
            Ok((last, false)) => {
                // Honest rather than reassuring: it is in their history now,
                // and pretending otherwise would be worse than saying so.
                self.action_status =
                    Some((false, format!("Too late — {} already has it", last.device_name)));
            }
            Err(e) => self.action_status = Some((false, e)),
        }
    }

    fn do_copy(&mut self) -> bool {
        let Some(clip) = self.selected_clip().cloned() else {
            return false;
        };
        // Image clips go to the clipboard as pixels, file clips as file URLs;
        // everything else as text.
        if clip.content_type == ContentType::Image {
            if let Some(path) = clip.image_path.as_deref() {
                return self.set_clipboard_image(path);
            }
            return false;
        }
        if clip.content_type == ContentType::File && !clip.files.is_empty() {
            return self.set_clipboard_files(&clip);
        }
        self.set_clipboard(&clip.content)
    }

    /// Copy, then hand focus back to the app the user came from and paste there.
    /// This is the deliberate "pick" gesture (Enter / double-click). A snippet
    /// matching the current search wins over the selected clip.
    fn do_paste(&mut self) {
        let pasted = if let Some(body) = self.matched_snippets.first().map(|s| s.body.clone()) {
            self.set_clipboard(&body)
        } else {
            self.do_copy()
        };
        if pasted && self.paste_settings.return_focus_after_copy {
            return_focus_to_previous_app();
        }
    }

    fn persist_actions(&self) {
        save_actions(&ActionsConfig {
            actions: self.custom_actions.clone(),
        });
    }

    /// Run custom action `idx` on the selected clip, then apply its output.
    fn run_custom_action(&mut self, idx: usize) {
        let Some(action) = self.custom_actions.get(idx).cloned() else {
            return;
        };
        let Some(clip) = self.selected_clip().cloned() else {
            self.action_status = Some((false, "No clip selected.".into()));
            return;
        };
        // Feed text content; for images feed the OCR text (may be empty).
        let input = if clip.content_type == ContentType::Image {
            clip.ocr_text.clone().unwrap_or_default()
        } else {
            clip.content.clone()
        };
        match run_action(&action.command, &input, std::time::Duration::from_secs(15)) {
            Ok(out) => {
                let out = out.trim_end_matches('\n').to_string();
                match action.output {
                    ActionOutput::Clipboard => {
                        self.set_clipboard(&out);
                        self.action_status = Some((true, format!("{} → clipboard", action.name)));
                    }
                    ActionOutput::NewClip => {
                        if !out.is_empty() {
                            let entry = ClipEntry::new(out, Some("clipd action".into()), None);
                            let _ = self.store.insert(&entry);
                            self.refresh();
                        }
                        self.action_status = Some((true, format!("{} → new clip", action.name)));
                    }
                    ActionOutput::None => {
                        self.action_status = Some((true, format!("{} ran", action.name)));
                    }
                }
            }
            Err(e) => {
                self.action_status = Some((false, format!("{}: {}", action.name, e)));
            }
        }
    }

    fn do_delete(&mut self) {
        if let Some(&idx) = self.filtered.get(self.selected) {
            let id = self.clips[idx].id;
            if self.store.delete(id).unwrap_or(false) {
                self.refresh();
            }
        }
    }

    /// Save a secret to the vault. Uses the password field if filled;
    /// otherwise falls back to the live clipboard.
    fn save_clipboard_to_vault(&mut self) {
        let Some(target) = self.vault_selected else {
            self.vault_status = Some((false, "No vault backend available.".into()));
            return;
        };
        // Prefer the explicit password field; fall back to clipboard.
        let password = if !self.vault_password_input.trim().is_empty() {
            std::mem::take(&mut self.vault_password_input)
        } else {
            match Clipboard::new().and_then(|mut c| c.get_text()) {
                Ok(t) if !t.trim().is_empty() => t,
                Ok(_) => {
                    self.vault_status = Some((
                        false,
                        "Paste your API key into the field above, or copy it to clipboard first."
                            .into(),
                    ));
                    return;
                }
                Err(e) => {
                    self.vault_status = Some((false, format!("Couldn't read clipboard: {e}")));
                    return;
                }
            }
        };
        let entry = SecretEntry {
            title: self.vault_title.clone(),
            username: self.vault_username.clone(),
            password,
            url: self.vault_url.clone(),
            notes: "Saved from clipd".into(),
        };
        match save_secret(target, &entry) {
            Ok(msg) => {
                self.vault_status = Some((true, msg));
                self.vault_title.clear();
                self.vault_username.clear();
                self.vault_url.clear();
                self.vault_password_input.clear();
                self.refresh_vault_secrets();
            }
            Err(e) => self.vault_status = Some((false, e)),
        }
    }

    /// Refresh the cached list of vault secrets (labels only — no plaintext).
    fn refresh_vault_secrets(&mut self) {
        match clipd_core::list_secrets() {
            Ok(secrets) => self.vault_secrets = secrets,
            Err(e) => {
                self.vault_status = Some((false, format!("Couldn't list vault: {e}")));
                self.vault_secrets.clear();
            }
        }
    }

    /// Render the Vault tab — list, reveal, copy, forget, and save new secrets.
    /// All secrets are encrypted in the macOS Keychain; clipd never stores
    /// plaintext keys in its database.
    fn render_vault_panel(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        ui.add_space(4.0);

        // ── Security banner ──
        egui::Frame::none()
            .fill(surf(c, c.bg_elevated))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new("🔐").size(16.0));
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Encrypted Vault")
                                .size(13.0)
                                .strong()
                                .color(rgb(c.text)),
                        );
                        ui.label(
                            RichText::new("API keys and secrets are stored in macOS Keychain. They are never saved to clipd's clipboard history or database.")
                                .size(11.0)
                                .color(rgb(c.subtext)),
                        );
                    });
                });
            });

        ui.add_space(12.0);

        // ── Save new secret ──
        let save_open = ui.collapsing("Save new secret", |ui| {
            ui.add_space(4.0);
            egui::Frame::none()
                .fill(surf(c, c.bg_elevated))
                .rounding(Rounding::same(10.0))
                .inner_margin(Margin::symmetric(12.0, 10.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Title").size(12.0).color(rgb(c.subtext)));
                        ui.add_space(4.0);
                        ui.text_edit_singleline(&mut self.vault_title);
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("API key").size(12.0).color(rgb(c.subtext)));
                        ui.add_space(4.0);
                        ui.add_sized(
                            [ui.available_width().max(120.0), 18.0],
                            egui::TextEdit::singleline(&mut self.vault_password_input)
                                .password(true)
                                .hint_text("Paste your API key here")
                                .frame(true),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("URL").size(12.0).color(rgb(c.subtext)));
                        ui.add_space(28.0);
                        ui.add_space(4.0);
                        ui.text_edit_singleline(&mut self.vault_url);
                    });
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Backend").size(12.0).color(rgb(c.subtext)),
                        );
                        ui.add_space(4.0);
                        egui::ComboBox::from_id_salt("vault_backend")
                            .selected_text(
                                self.vault_selected
                                    .map(|t| t.label().to_string())
                                    .unwrap_or_default(),
                            )
                            .show_ui(ui, |ui| {
                                for t in clipd_core::available_targets() {
                                    ui.selectable_value(
                                        &mut self.vault_selected,
                                        Some(t),
                                        t.label(),
                                    );
                                }
                            });
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save to vault").clicked() {
                            self.save_clipboard_to_vault();
                        }
                        if let Some((ok, msg)) = &self.vault_status {
                            let col = if *ok { rgb(c.green) } else { rgb(c.accent) };
                            ui.label(RichText::new(msg).size(11.5).color(col));
                        }
                    });
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Copy your API key first, then click Save — the clipboard content is encrypted and stored in the Keychain, not in clipd's history.")
                            .size(10.5)
                            .color(rgb(c.subtext)),
                    );
                });
        });

        ui.add_space(12.0);

        // ── Secrets list ──
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Saved secrets ({})", self.vault_secrets.len()))
                    .size(13.0)
                    .strong()
                    .color(rgb(c.text)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_vault_secrets();
                }
            });
        });
        ui.add_space(6.0);

        if self.vault_secrets.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No saved secrets yet")
                        .size(13.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Copy an API key, then expand \"Save new secret\" above.")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
            });
            return;
        }

        // Clear expired reveal.
        if let Some((_, _, ts)) = &self.vault_revealed {
            if ts.elapsed() > Duration::from_secs(30) {
                self.vault_revealed = None;
            }
        }

        let scroll_h = ui.available_height() - 20.0;
        let vault_revealed = self.vault_revealed.clone();
        let vault_confirm = self.vault_confirm_delete;
        let secrets = self.vault_secrets.clone();
        egui::ScrollArea::vertical()
            .id_salt("vault_list")
            .max_height(scroll_h.max(80.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, secret) in secrets.iter().enumerate() {
                    let is_revealed = vault_revealed.as_ref().map_or(false, |(idx, _, _)| *idx == i);
                    let is_confirm = vault_confirm == Some(i);

                    let frame = egui::Frame::none()
                        .fill(surf(c, c.bg_elevated))
                        .rounding(Rounding::same(10.0))
                        .stroke(Stroke::new(0.5, rgb(c.border)))
                        .inner_margin(Margin::symmetric(12.0, 8.0));

                    frame.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let (tile, _) =
                                ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                            ui.painter().rect_filled(
                                tile,
                                Rounding::same(8.0),
                                rgb(c.accent).gamma_multiply(0.16),
                            );
                            paint_footer_icon(
                                ui.painter(),
                                egui::Rect::from_center_size(tile.center(), egui::vec2(15.0, 15.0)),
                                FooterIcon::Lock,
                                rgb(c.accent),
                            );
                            ui.vertical(|ui| {
                                ui.set_width(ui.available_width() - 120.0);
                                ui.label(
                                    RichText::new(&secret.title)
                                        .size(13.0)
                                        .strong()
                                        .color(rgb(c.text)),
                                );
                                // Skip the note when it only restates the
                                // title. Every captured secret carries
                                // "Captured by clipd (OpenAI)." under a title
                                // that already ends in "— OpenAI", so the
                                // second line told you nothing and made each
                                // row twice as tall. Suppressed at display
                                // time because it is baked into every entry
                                // already saved.
                                let boilerplate = secret.note.starts_with("Captured by clipd");
                                if !boilerplate && !secret.note.trim().is_empty() {
                                    ui.add_space(1.0);
                                    ui.label(
                                        RichText::new(&secret.note)
                                            .size(10.5)
                                            .color(rgb(c.subtext)),
                                    );
                                }
                                if is_revealed {
                                    ui.add_space(4.0);
                                    let (_, plaintext, _) = self.vault_revealed.as_ref().unwrap();
                                    ui.label(
                                        RichText::new(plaintext)
                                            .size(12.0)
                                            .family(egui::FontFamily::Monospace)
                                            .color(rgb(c.accent)),
                                    );
                                    ui.label(
                                        RichText::new("Auto-hides in 30s")
                                            .size(9.5)
                                            .color(rgb(c.overlay)),
                                    );
                                }
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if is_confirm {
                                        if vault_action(ui, c, "Confirm delete", true, false).clicked() {
                                            if let Err(e) = clipd_core::forget_secret(secret) {
                                                self.vault_status =
                                                    Some((false, format!("Delete failed: {e}")));
                                            } else {
                                                self.vault_status =
                                                    Some((true, "Secret deleted.".into()));
                                                self.refresh_vault_secrets();
                                            }
                                            self.vault_confirm_delete = None;
                                        }
                                        if ui.button("Cancel").clicked() {
                                            self.vault_confirm_delete = None;
                                        }
                                    } else {
                                        if vault_action(ui, c, "Copy", true, false).clicked() {
                                            match clipd_core::reveal_secret(secret) {
                                                Ok(plaintext) => {
                                                    let _ = Clipboard::new()
                                                        .and_then(|mut c| c.set_text(&plaintext));
                                                    self.vault_revealed =
                                                        Some((i, plaintext, Instant::now()));
                                                }
                                                Err(e) => {
                                                    self.vault_status =
                                                        Some((false, format!("Reveal failed: {e}")));
                                                }
                                            }
                                        }
                                        if vault_action(ui, c, "Reveal", false, false).clicked() {
                                            match clipd_core::reveal_secret(secret) {
                                                Ok(plaintext) => {
                                                    self.vault_revealed =
                                                        Some((i, plaintext, Instant::now()));
                                                }
                                                Err(e) => {
                                                    self.vault_status = Some((
                                                        false,
                                                        format!("Reveal failed: {e}"),
                                                    ));
                                                }
                                            }
                                        }
                                        if vault_action(ui, c, "Forget", false, true).clicked() {
                                            self.vault_confirm_delete = Some(i);
                                        }
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(4.0);
                }
            });
    }

    /// Open/close the preview inspector, resizing the window so the palette
    /// is compact without it and wide with it — one card, never two windows.
    fn set_preview_open(&mut self, ctx: &egui::Context, on: bool) {
        if self.show_preview == on {
            return;
        }
        self.show_preview = on;
        let cur_h = ctx.input(|i| i.screen_rect().height());
        let w = if on { EXPANDED_W } else { COMPACT_W };
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, cur_h)));
        // Widening can push the card off the right screen edge — nudge it back.
        if on {
            let outer = ctx.input(|i| i.viewport().outer_rect);
            let monitor = ctx
                .input(|i| i.viewport().monitor_size)
                .or_else(main_display_size);
            if let (Some(outer), Some(mon)) = (outer, monitor) {
                let x = (outer.min.x).min(mon.x - w - 8.0).max(8.0);
                if (x - outer.min.x).abs() > 0.5 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                        x,
                        outer.min.y,
                    )));
                }
            }
        }
    }

    fn cycle_theme(&mut self, ctx: &egui::Context) {
        self.theme = self.theme.next();
        save_theme(self.theme);
        apply_theme(ctx, self.theme);
    }

    /// Pull appearance changes made by another Clipd GUI process.
    ///
    /// The tray HUD is intentionally kept alive for instant opening, so its
    /// `self.theme` otherwise stays at the value loaded during startup. The
    /// 150ms interval is fast enough to feel immediate while avoiding a disk
    /// read on every HUD repaint.
    fn sync_shared_appearance(&mut self, ctx: &egui::Context) {
        const APPEARANCE_POLL: Duration = Duration::from_millis(150);
        if self.last_shared_appearance_check.elapsed() < APPEARANCE_POLL {
            return;
        }
        self.last_shared_appearance_check = Instant::now();

        let shared_theme = load_theme();
        let shared_custom_colors = load_custom_colors();
        if shared_theme == self.theme && shared_custom_colors == self.custom_colors {
            return;
        }

        self.theme = shared_theme;
        self.custom_colors = shared_custom_colors;
        apply_theme(ctx, self.theme);
        // `sync_glass_native` runs immediately after this method. A changed
        // Glass Light/Dark/solid state therefore clears and reapplies the
        // correct AppKit material in the same frame.
        ctx.request_repaint();
    }
}

// ── Rendering ──

impl eframe::App for ClipdGui {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Hand the top of the screen back to the island on the way out.
        if !self.island_surface {
            clipd_core::set_gui_window_open(false);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Fully transparent so the rounded card corners show the desktop behind
        // (the card surface is painted by the panels).
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // A surface asked for the keyboard while it was drawing, where the
        // frame is not in hand. Grant it here.
        if self.want_key_window {
            self.want_key_window = false;
            activate_for_keyboard_input();
            make_window_key(frame);
        }
        self.sync_shared_appearance(ctx);
        // Keep our "a window is on screen" claim from expiring. It carries a
        // timestamp so a crashed window cannot pin the island shut, which
        // means a live one has to say so periodically.
        if !self.island_surface
            && (!self.hud || self.hud_expanded)
            && self.last_claim_refresh.elapsed() >= Duration::from_secs(1)
        {
            self.last_claim_refresh = Instant::now();
            clipd_core::refresh_gui_window_claim();
        }
        // Re-apply every frame so selection/ink colours can't stick on a stale
        // visuals snapshot after a theme change or System appearance flip.
        apply_theme(ctx, self.theme);
        // Native Liquid Glass / vibrancy for Glass Light & Glass Dark. Must
        // stay in sync as the user cycles themes or blur leaks onto solids.
        #[cfg(target_os = "macos")]
        sync_glass_native(frame, self.theme, &mut self.glass_native);
        // Every frame, not just while Settings is showing: a pairing started
        // there must still complete if the user navigates away mid-search.
        self.poll_pairing(ctx);

        // One-shot render-scale diagnostic. "Everything looks pixelated"
        // usually means the framebuffer is 1x on a 2x display, which no amount
        // of anti-aliasing can fix — so measure it rather than guess.
        if std::env::var("CLIPD_DEBUG_SCALE").is_ok() {
            static LOGGED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                let native = ctx.input(|i| i.viewport().native_pixels_per_point);
                eprintln!(
                    "[clipd scale] native_pixels_per_point={:?} ctx.pixels_per_point={} zoom={}",
                    native,
                    ctx.pixels_per_point(),
                    ctx.zoom_factor()
                );
            }
        }
        // The HUD process polls for show/hide requests at 30ms so the tray
        // popover feels instant. The main window keeps the slower 250ms poll.
        // Hidden is now a visibility flag, not a position: the HUD stays put
        // and toggles Visible. Testing the old off-screen position here meant
        // a hidden HUD looked "shown" and took the fast poll — repainting an
        // invisible window 30 times a second for the life of the process.
        let hud_hidden = self.hud && !self.hud_expanded;
        let poll_interval = if hud_hidden {
            // Hidden, waiting to be summoned. `ensure_hud_request_watcher`
            // wakes this thread when a request lands, so this only has to be a
            // backstop — polling fast enough to feel instant meant repainting
            // a hidden window 30 times a second for the life of the process.
            400
        } else if self.hud {
            30 // Tray-driven surface: a show request has to land instantly.
        } else {
            // The island and the main window are resident. Nothing waits on
            // their request file, and a 30ms poll here pinned a repaint at
            // 33fps for the whole life of the process — which cost the island
            // about a tenth of a core while it was doing nothing at all.
            250
        };
        if self.last_surface_request_check.elapsed() >= Duration::from_millis(poll_interval) {
            self.last_surface_request_check = Instant::now();
            // Don't process surface requests here for HUD — drive_hud_hover
            // handles show/hide/quit itself. Processing them here would close
            // the HUD before drive_hud_hover can hide it off-screen.
            if !self.hud {
                if let Some(mode) = take_surface_request_for(self.surface_mode()) {
                    if mode == SurfaceMode::Quit {
                        self.quitting = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else if mode == SurfaceMode::Hidden {
                        // No persistent surface to hide now that the pill is
                        // gone; the HUD handles its own hide via drive_hud_hover.
                    } else {
                        self.switch_surface(ctx, mode);
                    }
                }
            }
        }
        // Only clipd-ui owns the daemon. GUI processes don't restart it.
        #[cfg(target_os = "macos")]
        if self.last_daemon_check.elapsed() >= Duration::from_secs(5) {
            self.last_daemon_check = Instant::now();
        }
        // Keep the repaint ticking at the poll interval so the HUD wakes up
        // fast enough to catch tray requests.
        ctx.request_repaint_after(Duration::from_millis(poll_interval));
        // A parked popover does not need fresh clips.
        //
        // `refresh()` reloads the whole 200-clip window from SQLite, rebuilds
        // sessions and re-runs the filter. Doing that every three seconds in a
        // window nobody can see is pure cost — and it is paid twice, because
        // the island is a second resident process doing the same. The popover
        // reloads when it is shown instead, which is the only moment its
        // contents can matter.
        //
        // The island keeps polling: it is how a new copy gets noticed and
        // announced, so for that surface the work is the feature.
        let needs_fresh_clips = !self.hud || self.hud_expanded;
        if needs_fresh_clips && self.last_refresh.elapsed() > Duration::from_secs(3) {
            // The island polls for one reason: to notice a new copy and
            // announce it. Loading two hundred rows to answer "is there a new
            // one?" asks a cheap question expensively — and it is the only
            // work this process does at rest. Read the newest row, and reload
            // the rest only when the answer changed.
            let only_watching = self.island_surface
                && self.island.phase == island::IslandPhase::Hidden;
            if only_watching {
                let newest = self
                    .store
                    .get_recent(1)
                    .ok()
                    .and_then(|mut v| v.pop())
                    .map(|c| c.id);
                if newest != self.clips.first().map(|c| c.id) {
                    self.refresh();
                } else {
                    self.last_refresh = Instant::now();
                }
            } else {
                self.refresh();
            }
        }
        if needs_fresh_clips {
            ctx.request_repaint_after(Duration::from_secs(3));
        }

        self.poll_ask();
        self.poll_transform();
        if self.transform_job.running {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.ask.running {
            // Keep the spinner moving and pick the answer up promptly.
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        self.poll_ai_test();
        if self.ai_test_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        // The island owns its whole window: it sizes, positions and paints
        // itself, and shares nothing with the palette chrome below.
        if self.island_surface {
            self.drive_island(ctx, frame);
            if !self.quitting {
                self.render_island(ctx);
            }
            return;
        }

        // The HUD is a tray-anchored popover that opens directly expanded and
        // closes when the pointer leaves.
        if self.hud {
            let mut c = resolved_theme(ctx, self.theme).colors();
            self.custom_colors.apply_to(&mut c);
            self.drive_hud_hover(ctx);
            self.render_hud_popover(ctx, &c);
            return;
        }

        // When the window is summoned (gains focus), drop the cursor into search
        // with a clean query — so the palette is "type to recall" every time.
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if focused && !self.was_focused {
            self.focus_search = true;
            self.search_query.clear();
            self.apply_filter();
            // Summoned (Ctrl+G): jump to the mouse cursor — but NEVER while a
            // mouse button is down (that's a click-to-focus: moving the window
            // mid-click makes every button unclickable), and never when the
            // cursor is already inside the window. On Windows we also require a
            // known scale factor — without it the physical→points conversion is
            // wrong on scaled displays and the containment check lies.
            let pointer_down = ctx.input(|i| i.pointer.any_down());
            let scale_known = !cfg!(target_os = "windows")
                || ctx
                    .input(|i| i.viewport().native_pixels_per_point)
                    .is_some();
            if !pointer_down && scale_known {
                if let Some(cursor) = cursor_in_points(ctx) {
                    let outside = ctx
                        .input(|i| i.viewport().outer_rect)
                        .map_or(true, |r| !r.contains(cursor));
                    if outside {
                        let size = ctx.input(|i| i.screen_rect().size());
                        let monitor = ctx
                            .input(|i| i.viewport().monitor_size)
                            .or_else(main_display_size);
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                            window_pos_at_cursor(cursor, size, monitor),
                        ));
                    }
                }
            }
        }
        self.was_focused = focused;

        // Heal dropped preview-resize commands: a wide window with the preview
        // closed (or narrow with it open) paints a partial card over a
        // transparent slab — re-send the intended size until it sticks.
        {
            let win_w = ctx.input(|i| i.screen_rect().width());
            let want_wide = self.show_preview && self.active_tab == MainTab::Text;
            // Settings needs its own width: at the compact palette size the
            // endpoint URL and colour rows are clipped mid-field, which reads
            // as a broken form rather than a narrow window.
            let want_settings = self.active_tab == MainTab::Settings;
            let win_h = ctx.input(|i| i.screen_rect().height());
            let target_w = if want_wide {
                EXPANDED_W
            } else if want_settings {
                SETTINGS_W
            } else {
                COMPACT_W
            };
            // Settings/preview keep whatever height the user has; the compact
            // palette snaps to the mockup size so it stays tall-and-narrow.
            let target_h = if want_settings || want_wide {
                win_h.max(WIN_H)
            } else {
                WIN_H
            };
            if (win_w - target_w).abs() > 40.0 || (win_h - target_h).abs() > 40.0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    target_w, target_h,
                )));
            }
        }

        let mut c = resolved_theme(ctx, self.theme).colors();
        self.custom_colors.apply_to(&mut c);
        let c = c;
        let mut action = Action::None;

        let search_has_focus = ctx.memory(|m| {
            m.focused()
                .map_or(false, |id| id == egui::Id::new("clip_search"))
        });

        let mut should_cycle_theme = false;
        let mut esc_back = false;
        let mut toggle_preview = false;
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                if self.show_quick_settings {
                    // Quick settings close first — one level at a time.
                    self.show_quick_settings = false;
                } else {
                    // Esc = back out one level (handled after this closure so
                    // the preview resize can read input state without
                    // re-locking).
                    esc_back = true;
                }
            }
            // Cmd+, (Ctrl+, on Windows/Linux) — the standard "preferences"
            // chord — toggles Settings, so the gear never needs the mouse.
            if i.key_pressed(egui::Key::Comma) && i.modifiers.command {
                self.active_tab = if self.active_tab == MainTab::Settings {
                    MainTab::Text
                } else {
                    MainTab::Settings
                };
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                    self.scroll_to_selected = true;
                }
            }
            if i.key_pressed(egui::Key::ArrowUp) && self.selected > 0 {
                self.selected -= 1;
                self.scroll_to_selected = true;
            }
            if i.key_pressed(egui::Key::Enter) && !search_has_focus {
                // Never paste-and-hide out of a question — re-ask instead.
                action = if self.in_ask_mode() {
                    Action::Ask
                } else {
                    Action::Paste
                };
            }
            // Space toggles the on-demand preview pane (single column stays clean by default).
            if i.key_pressed(egui::Key::Space) && !search_has_focus {
                toggle_preview = true;
            }
            // `/` focuses search — matches the hint badge in the search field.
            if i.key_pressed(egui::Key::Slash) && !search_has_focus && !i.modifiers.any() {
                self.focus_search = true;
            }
            if i.key_pressed(egui::Key::Delete)
                || (i.key_pressed(egui::Key::D) && i.modifiers.command)
            {
                action = Action::Delete;
            }
            if i.key_pressed(egui::Key::T) && i.modifiers.command {
                should_cycle_theme = true;
            }
            // Cmd+C copies the selected clip without closing the palette
            // (replaces the old preview-pane Copy button).
            if i.key_pressed(egui::Key::C) && i.modifiers.command && !search_has_focus {
                action = Action::Copy;
            }
            // P pins/unpins the selected clip.
            if i.key_pressed(egui::Key::P) && !search_has_focus {
                if let Some(clip) = self.selected_clip() {
                    action = Action::ToggleStar(clip.id);
                }
            }
            // S sends the selected clip to the other Mac. No picker and no
            // confirmation — with one other Mac there is nothing to choose.
            if i.key_pressed(egui::Key::S) && !i.modifiers.command && !search_has_focus {
                action = Action::Send;
            }
            // U takes the last send back. Undo is what buys the send its lack
            // of a confirmation dialog, so it has to be as cheap as the send.
            if i.key_pressed(egui::Key::U) && !i.modifiers.command && !search_has_focus {
                action = Action::UndoSend;
            }
            // Cmd+1-9 pastes the numbered row — the badge/footer promise.
            if i.modifiers.command && self.active_tab == MainTab::Text {
                const DIGITS: [egui::Key; 9] = [
                    egui::Key::Num1,
                    egui::Key::Num2,
                    egui::Key::Num3,
                    egui::Key::Num4,
                    egui::Key::Num5,
                    egui::Key::Num6,
                    egui::Key::Num7,
                    egui::Key::Num8,
                    egui::Key::Num9,
                ];
                for (n, key) in DIGITS.iter().enumerate() {
                    if i.key_pressed(*key) && n < self.filtered.len() {
                        self.selected = n;
                        action = Action::Paste;
                    }
                }
            }
        });
        if should_cycle_theme {
            self.cycle_theme(ctx);
        }
        // Esc backs out one level: Pins/Settings → Clips; preview → list only;
        // bare list → hide. Keyboard must always work even if the mouse is
        // misbehaving (borderless-window quirks on Windows).
        if esc_back {
            if self.active_tab != MainTab::Text {
                self.active_tab = MainTab::Text;
            } else if self.in_ask_mode() {
                // Leaving ask mode ends the conversation — a later `?` starts
                // fresh rather than silently inheriting old turns as context.
                self.search_query.clear();
                self.ask.reset();
                self.apply_filter();
                self.set_preview_open(ctx, false);
            } else if self.show_preview {
                self.set_preview_open(ctx, false);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if toggle_preview {
            self.set_preview_open(ctx, !self.show_preview);
        }

        let preview_data = self.selected_clip().cloned();
        // Ensure the selected image clip's thumbnail is loaded so the preview
        // pane can show it (reuses the list's cache).
        let preview_thumb: Option<egui::TextureHandle> = preview_data.as_ref().and_then(|clip| {
            if clip.content_type != ContentType::Image {
                return None;
            }
            if !self.thumb_textures.contains_key(&clip.id) {
                if let Some(p) = clip.thumb_path.clone() {
                    let tex = load_thumb_texture(ctx, &p);
                    self.thumb_textures.insert(clip.id, tex);
                }
            }
            self.thumb_textures.get(&clip.id).cloned().flatten()
        });

        paint_glass_shell(ctx, self.theme, &c, self.native_glass_active());

        // ── Full-GUI chrome: brand → search → tiny filters (mockup stack). ──
        egui::TopBottomPanel::top("search_header")
            .show_separator_line(self.theme != Theme::GlassLight)
            .frame(
                egui::Frame::none()
                    .fill(glass_panel_frost(self.theme))
                    // This panel owns the two upper window corners. Leaving
                    // its fill square covers the rounded shell underneath.
                    .rounding(egui::Rounding {
                        nw: SHELL_ROUND,
                        ne: SHELL_ROUND,
                        sw: 0.0,
                        se: 0.0,
                    })
                    .inner_margin(Margin::symmetric(16.0, 14.0)),
            )
            .show(ctx, |ui| {
                paint_panel_glass_gradient(ui, self.theme);
                self.render_brand_header(ui, &c);
                ui.add_space(12.0);
                self.render_search_bar(ui, &mut action, &c);
                if self.active_tab == MainTab::Text {
                    ui.add_space(12.0);
                    self.render_filter_pills(ui, &c);
                } else if self.active_tab == MainTab::Settings {
                    ui.add_space(12.0);
                    self.render_settings_category_tabs(ui, &c);
                }
            });

        // ── Footer: Capturing · clock · ⌘⇧V (mockup minimal bar) ──
        egui::TopBottomPanel::bottom("footer_hints")
            .show_separator_line(self.theme != Theme::GlassLight)
            .exact_height(44.0)
            .frame(
                egui::Frame::none()
                    .fill(glass_panel_frost(self.theme))
                    // Mirror the header: the footer owns the lower corners.
                    .rounding(egui::Rounding {
                        nw: 0.0,
                        ne: 0.0,
                        sw: SHELL_ROUND,
                        se: SHELL_ROUND,
                    })
                    .inner_margin(Margin::symmetric(16.0, 0.0)),
            )
            .show(ctx, |ui| {
                paint_panel_glass_gradient(ui, self.theme);
                self.render_bottom_bar(ui, &mut action, &c);
            });

        // Ask mode always needs the inspector — that's where the answer goes.
        if self.active_tab == MainTab::Text && self.in_ask_mode() && !self.show_preview {
            self.set_preview_open(ctx, true);
        }

        // ── Right inspector: on-demand preview (Text tab, toggled with Space) ──
        if self.active_tab == MainTab::Text && self.show_preview {
            egui::SidePanel::right("clip_inspector")
                .resizable(false)
                .exact_width(380.0)
                .frame(
                    egui::Frame::none()
                        .fill(glass_panel_frost(self.theme))
                        .inner_margin(Margin::symmetric(16.0, 14.0))
                        // The inspector sits between the rounded header and
                        // footer; rounding it creates seams rather than an
                        // outer window corner.
                        .rounding(Rounding::same(0.0)),
                )
                .show(ctx, |ui| {
                    paint_panel_glass_gradient(ui, self.theme);
                    if self.in_ask_mode() {
                        render_ask_panel(ui, &self.ask, &mut action, &c);
                    } else if let Some(clip) = &preview_data {
                        let is_starred = self.starred_clip_ids.contains(&clip.id);
                        // Prefer the real multi-slot number when this clip is
                        // saved to one; otherwise fall back to list position.
                        let preview_slot = clip
                            .slot
                            .map(|s| s as usize)
                            .unwrap_or(self.selected + 1);
                        render_preview(
                            ui,
                            clip,
                            preview_slot,
                            is_starred,
                            preview_thumb.clone(),
                            &self.custom_actions,
                            self.action_status.clone(),
                            &self.transform_job,
                            clipd_core::can_synthesize(&self.ai_config),
                            &mut action,
                            &c,
                        );
                    } else {
                        render_empty_preview(ui, &c);
                    }
                });
        }

        // ── Center panel — search bar is a fixed header; clip list scrolls below ──
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(glass_panel_frost(self.theme))
                    // The center region is sandwiched between the header and
                    // footer, so it must meet them flush. Only those outer
                    // panels own the actual window corners.
                    .rounding(Rounding::same(0.0))
                    .inner_margin(Margin::symmetric(
                        if self.active_tab == MainTab::Text {
                            16.0
                        } else {
                            0.0
                        },
                        8.0,
                    )),
            )
            .show(ctx, |ui| {
                paint_panel_glass_gradient(ui, self.theme);
                // Accessibility / quick-settings banners stay below the chrome.
                // Brand, search, and filters live in the top panel now.
                if self.active_tab == MainTab::Text {
                    self.render_text_banners(ui, &c);
                }

                match self.active_tab {
                    MainTab::Text => {
                        if self.filtered.is_empty() && self.matched_snippets.is_empty() {
                            self.render_empty_list(ui, &c);
                        } else {
                            self.render_clip_list(ui, &mut action, &c);
                        }
                    }
                    MainTab::Collections => {
                        self.render_collections_panel(ui, &c);
                    }
                    MainTab::Settings => {
                        self.render_settings_panel(ui, &c);
                    }
                    MainTab::Vault => {
                        self.render_vault_panel(ui, &c);
                    }
                }
            });

        if self.show_transforms {
            self.render_transform_window(ctx, &c);
        }

        self.dispatch(action, ctx);
    }
}

impl ClipdGui {
    /// Apply one user action. Shared by the palette window and the HUD so
    /// both surfaces behave identically.
    fn dispatch(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Copy => {
                self.do_copy();
            }
            Action::Paste => {
                // Pick = copy + get out of the way: hide clipd so the user is
                // back where they were, ready to Cmd+V.
                self.do_paste();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::Delete => self.do_delete(),
            Action::Send => self.do_send(),
            Action::UndoSend => self.do_undo_send(),
            Action::ToggleStar(clip_id) => {
                self.toggle_starred(clip_id);
                ctx.request_repaint();
            }
            Action::RunAction(idx) => {
                self.run_custom_action(idx);
                ctx.request_repaint();
            }
            Action::Ask => {
                self.start_ask(ctx);
                // Enter moved focus out of the field; put it back so a
                // follow-up can be typed straight away.
                self.focus_search = true;
                ctx.request_repaint();
            }
            Action::JumpToClip(clip_id) => {
                if !self.jump_to_clip(clip_id) {
                    self.ask.error = Some(format!(
                        "Clip #{} is no longer in the loaded history window.",
                        clip_id
                    ));
                }
                ctx.request_repaint();
            }
            Action::OpenAiSettings => {
                // Re-read from disk in case the file changed since launch.
                self.ai_config = load_transform_config();
                self.active_tab = MainTab::Settings;
                ctx.request_repaint();
            }
            Action::AskAboutClip(clip_id) => {
                self.ask_about_clip(clip_id, ctx);
                ctx.request_repaint();
            }
            Action::RunSuggestion(idx) => {
                self.run_suggestion(idx, ctx);
                ctx.request_repaint();
            }
            Action::None => {}
        }
    }

    fn export_path(ext: &str) -> std::path::PathBuf {
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("clipd_history_{}.{}", ts, ext);
        dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(filename)
    }

    fn do_export_text(&self) -> Result<String, String> {
        let path = Self::export_path("txt");
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut w = std::io::BufWriter::new(file);
        for (i, clip) in self.clips.iter().enumerate() {
            writeln!(
                w,
                "=== Clip {} | {} | {} ===",
                i + 1,
                clip.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                clip.source_app.as_deref().unwrap_or("Unknown"),
            )
            .map_err(|e| e.to_string())?;
            writeln!(w, "{}", clip.content).map_err(|e| e.to_string())?;
            writeln!(w).map_err(|e| e.to_string())?;
        }
        w.flush().map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    fn do_export_csv(&self) -> Result<String, String> {
        let path = Self::export_path("csv");
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut w = std::io::BufWriter::new(file);
        writeln!(w, "slot,timestamp,source_app,content_type,content").map_err(|e| e.to_string())?;
        for (i, clip) in self.clips.iter().enumerate() {
            let escaped = clip.content.replace('"', "\"\"");
            writeln!(
                w,
                "{},{},{},{},\"{}\"",
                i + 1,
                clip.timestamp.format("%Y-%m-%d %H:%M:%S"),
                clip.source_app.as_deref().unwrap_or(""),
                clip.content_type.as_str(),
                escaped,
            )
            .map_err(|e| e.to_string())?;
        }
        w.flush().map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    fn render_empty_list(&self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        ui.vertical_centered(|ui| {
            ui.add_space(72.0);
            if self.search_query.is_empty() {
                ui.label(
                    RichText::new("No clips yet")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.overlay)),
                );
                ui.label(
                    RichText::new("Copy something to get started.")
                        .size(11.0)
                        .color(rgb(c.overlay)),
                );
            } else {
                ui.label(
                    RichText::new("No matching clips")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.overlay)),
                );
            }
        });
    }

    fn render_clip_list(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Action,
        c: &clipd_core::ThemeColors,
    ) {
        // Breath between filter row and first section header (mockup rhythm).
        ui.add_space(10.0);

        let visible_indices = self.filtered.clone();
        let snippets = self.matched_snippets.clone();

        // Pre-load thumbnails for any visible image clips before the render
        // closure borrows self.clips (avoids a borrow conflict inside the loop).
        let ctx = ui.ctx().clone();
        let to_load: Vec<(i64, String)> = visible_indices
            .iter()
            .filter_map(|&idx| {
                let clip = self.clips.get(idx)?;
                if clip.content_type == ContentType::Image
                    && !self.thumb_textures.contains_key(&clip.id)
                {
                    // Prefer the list thumb; fall back to the full image so
                    // older clips without a thumb still show something.
                    clip.thumb_path
                        .clone()
                        .or_else(|| clip.image_path.clone())
                        .map(|p| (clip.id, p))
                } else {
                    None
                }
            })
            .collect();
        for (id, path) in to_load {
            let tex = load_thumb_texture(&ctx, &path);
            self.thumb_textures.insert(id, tex);
        }

        // Explicit id: the Text/Settings/Pins tabs each build a ScrollArea in
        // the same panel — with auto ids they can collide and share scroll
        // state, so returning from Settings left the list scrolled mid-air.
        egui::ScrollArea::vertical()
            .id_salt("clip_list_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Matched snippets first — typing a trigger recalls one to paste.
                for (si, snip) in snippets.iter().enumerate() {
                    let fr = egui::Frame::none()
                        .fill(rgb(c.accent).gamma_multiply(0.14))
                        .rounding(Rounding::same(CARD_ROUND))
                        .stroke(Stroke::new(1.0, rgb(c.accent).gamma_multiply(0.45)))
                        .inner_margin(Margin::symmetric(CARD_PAD_X, CARD_PAD_Y))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                tag_pill(
                                    ui,
                                    &format!("snippet · {}", snip.trigger),
                                    rgb(c.accent),
                                    c,
                                );
                                ui.add_space(8.0);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(snip.preview()).size(14.0).color(rgb(c.text)),
                                    )
                                    .truncate(),
                                );
                            });
                        });
                    let resp = ui.interact(
                        fr.response.rect,
                        egui::Id::new(("snippet", si)),
                        egui::Sense::click(),
                    );
                    if resp.clicked() || resp.double_clicked() {
                        if self.set_clipboard(&snip.body)
                            && self.paste_settings.return_focus_after_copy
                        {
                            return_focus_to_previous_app();
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    ui.add_space(ROW_GAP);
                }

                // Rows in a run share edges, so nothing may be inserted
                // between them. Section headers add their own space.
                ui.spacing_mut().item_spacing.y = 0.0;
                for (display_idx, &clip_idx) in visible_indices.iter().enumerate() {
                    let clip = &self.clips[clip_idx];
                    let clip_id_value = clip.id;
                    let is_selected = display_idx == self.selected;
                    let is_starred = self.starred_clip_ids.contains(&clip_id_value);
                    let group = clip_group_label(clip, is_starred);
                    let previous_group = display_idx.checked_sub(1).and_then(|previous| {
                        let previous_clip = self.clips.get(visible_indices[previous])?;
                        Some(clip_group_label(
                            previous_clip,
                            self.starred_clip_ids.contains(&previous_clip.id),
                        ))
                    });
                    if previous_group != Some(group) {
                        // Mockup: roomy gap before "Recent", then header, then cards.
                        ui.add_space(if display_idx == 0 { 4.0 } else { 18.0 });
                        ui.label(
                            RichText::new(group)
                                .size(12.0)
                                .strong()
                                .color(rgb(c.overlay)),
                        );
                        ui.add_space(8.0);
                    }
                    let mut star_clicked = false;
                    let mut copy_clicked = false;
                    let mut delete_clicked = false;
                    // Where this row sits in its run decides its corners. The
                    // reference draws a section as one card with the rows
                    // ruled inside it, not as a stack of separate cards — so
                    // only the first and last row of a run are rounded, and
                    // the rest butt together into a single edge.
                    let next_group = visible_indices.get(display_idx + 1).and_then(|&next| {
                        let next_clip = self.clips.get(next)?;
                        Some(clip_group_label(
                            next_clip,
                            self.starred_clip_ids.contains(&next_clip.id),
                        ))
                    });
                    let first_in_group = previous_group != Some(group);
                    let last_in_group = next_group != Some(group);
                    // Glass rows are separate panes, each with its own lit
                    // rim, so they keep all four corners and stand apart.
                    // Opaque themes rule them together into one card.
                    let glass_rows = self.theme.is_glass();
                    let row_rounding = if glass_rows {
                        Rounding::same(10.0)
                    } else {
                        Rounding {
                            nw: if first_in_group { 12.0 } else { 0.0 },
                            ne: if first_in_group { 12.0 } else { 0.0 },
                            sw: if last_in_group { 12.0 } else { 0.0 },
                            se: if last_in_group { 12.0 } else { 0.0 },
                        }
                    };
                    let clip_id = egui::Id::new(("clip", display_idx));
                    let hover_id = egui::Id::new(("cliphover", display_idx));
                    // Was the pointer over this row last frame? Read from a
                    // full-width hover region so the star stays visible while
                    // you move onto it.
                    let row_hovered = ui
                        .ctx()
                        .read_response(hover_id)
                        .map_or(false, |r| r.contains_pointer());

                    // A clip held in an active slot is ringed in the accent.
                    //
                    // Same shape and weight as the row's own selection edge, so
                    // it reads as this row in a different state rather than as
                    // a differently-built row — the accent at a third strength
                    // is enough to find while scanning without competing with
                    // the row under the pointer. Selection and hover still win:
                    // where you are now matters more than where a slot is.
                    let slot_ring = clip
                        .slot
                        .is_some()
                        .then(|| Stroke::new(1.0, rgb(c.accent).gamma_multiply(0.34)));

                    // Almost-flat cards: soft fill + hairline. Glass themes
                    // keep a translucent cool selection wash.
                    let (bg, border) = if self.theme.is_glass() {
                        match glass_row_fill(self.theme, is_selected, row_hovered) {
                            Some(fill) => {
                                let stroke = glass_row_stroke(self.theme, is_selected);
                                match slot_ring {
                                    Some(ring) if !is_selected && !row_hovered => (fill, ring),
                                    _ => (fill, stroke),
                                }
                            }
                            None => match slot_ring {
                                Some(ring) if !is_selected && !row_hovered => {
                                    (Color32::TRANSPARENT, ring)
                                }
                                _ => (Color32::TRANSPARENT, Stroke::NONE),
                            },
                        }
                    } else if is_selected {
                        // A fill and the same hairline every other row has.
                        // The old heavy accent edge was drawn for rows that
                        // floated separately; inside one ruled card it boxes
                        // a single row and reads as a dialog, not a selection.
                        (
                            surf(c, c.bg_selected),
                            Stroke::new(0.7, rgb(c.border)),
                        )
                    } else if row_hovered {
                        (surf(c, c.bg_hover), Stroke::new(0.7, rgb(c.border)))
                    } else if let Some(ring) = slot_ring {
                        (surf(c, c.bg_elevated), ring)
                    } else {
                        // Almost-flat: soft card fill, quiet separator edge.
                        (
                            surf(c, c.bg_elevated),
                            Stroke::new(0.5, rgb(c.border).gamma_multiply(0.55)),
                        )
                    };

                    let is_image = clip.content_type == ContentType::Image;
                    let preview = if is_image {
                        let ocr = clip.ocr_text.as_deref().map(str::trim).unwrap_or("");
                        if !ocr.is_empty() {
                            ocr.replace('\n', " ")
                        } else if !clip.preview.trim().is_empty() {
                            clip.preview.trim().replace('\n', " ")
                        } else {
                            "Image".into()
                        }
                    } else {
                        clip.preview.trim().replace('\n', " ")
                    };
                    let truncated: String = preview.chars().take(200).collect();
                    let suffix = if preview.chars().count() > 200 {
                        "…"
                    } else {
                        ""
                    };
                    let time = relative_time_short(&clip.timestamp);
                    let source = clip
                        .source_app
                        .as_deref()
                        .filter(|source| !source.trim().is_empty())
                        .unwrap_or(content_type_label(&clip.content_type));
                    let is_sensitive =
                        !detect_sensitive(&clip.content, &self.privacy_config).is_empty();
                    let thumb_tex = if is_image {
                        self.thumb_textures.get(&clip_id_value).cloned().flatten()
                    } else {
                        None
                    };

                    // No gap inside a run — the rows are meant to share
                    // edges. The space before a section header is added with
                    // the header itself. Glass panes do not share edges, so
                    // they get a gap of their own.
                    if glass_rows {
                        ui.add_space(4.0);
                    } else if first_in_group {
                        ui.add_space(2.0);
                    }

                    // Reference row: type tile · title/meta · copy · pin · ⋮
                    let frame_resp = egui::Frame::none()
                        .fill(bg)
                        .rounding(row_rounding)
                        .stroke(border)
                        .inner_margin(Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;

                                // No selection rail. The reference marks the
                                // selected row by filling it, and inside a
                                // single ruled card a bar at the leading edge
                                // reads as a fourth vertical line rather than
                                // as emphasis.
                                draw_type_tile(ui, &clip.content_type, is_sensitive, true, c);

                                let thumb_slot = if is_image { 52.0 } else { 0.0 };
                                // Copy (28) + pin (24) + ⋮ (22) + the spacing
                                // between them. This was still reserving the
                                // 28pt the lone star used to need, so a long
                                // title ran underneath the new controls.
                                let right_w = 104.0
                                    + thumb_slot
                                    + if is_sensitive { 12.0 } else { 0.0 };
                                let content_w = (ui.available_width() - right_w).max(60.0);
                                ui.allocate_ui(egui::vec2(content_w, 36.0), |ui| {
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 1.0;
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(format!("{}{}", truncated, suffix))
                                                    .size(13.0)
                                                    .strong()
                                                    .color(rgb(c.text)),
                                            )
                                            .truncate(),
                                        );
                                        let meta = if let Some(slot) = clip.slot {
                                            format!("slot {}  ·  {}  ·  {}", slot, source, time)
                                        } else if is_image {
                                            format!("Image  ·  {}  ·  {}", source, time)
                                        } else {
                                            format!("{}  ·  {}", source, time)
                                        };
                                        ui.label(
                                            RichText::new(meta)
                                                .size(10.5)
                                                .color(rgb(c.subtext)),
                                        );
                                    });
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // Right-to-left: added first sits
                                        // rightmost, so this is ⋮ · pin · copy
                                        // on screen, the reference's order.
                                        // A blank button with the dots
                                        // painted on: the ⋮ character is not
                                        // in the bundled font and came out as
                                        // an empty box.
                                        let more = egui::menu::menu_custom_button(
                                            ui,
                                            egui::Button::new("")
                                                .fill(Color32::TRANSPARENT)
                                                .stroke(Stroke::NONE)
                                                .min_size(egui::vec2(22.0, 28.0)),
                                            |ui| {
                                                if ui.button("Copy").clicked() {
                                                    copy_clicked = true;
                                                    ui.close_menu();
                                                }
                                                if ui
                                                    .button(if is_starred { "Unpin" } else { "Pin" })
                                                    .clicked()
                                                {
                                                    star_clicked = true;
                                                    ui.close_menu();
                                                }
                                                if ui.button("Delete").clicked() {
                                                    delete_clicked = true;
                                                    ui.close_menu();
                                                }
                                            },
                                        );
                                        {
                                            let r = more.response.rect;
                                            for dy in [-4.6_f32, 0.0, 4.6] {
                                                ui.painter().circle_filled(
                                                    egui::pos2(r.center().x, r.center().y + dy),
                                                    1.5,
                                                    rgb(c.overlay),
                                                );
                                            }
                                        }
                                        // The pin holds its place whether or
                                        // not it is filled: a control that
                                        // appears on hover moves the two
                                        // beside it every time the pointer
                                        // crosses a row.
                                        if row_star_quiet(ui, is_starred, c).clicked() {
                                            star_clicked = true;
                                        }
                                        if row_copy_button(ui, c).clicked() {
                                            copy_clicked = true;
                                        }
                                        if is_image {
                                            let (tile, _) = ui.allocate_exact_size(
                                                egui::vec2(48.0, 36.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                tile,
                                                Rounding::same(7.0),
                                                surf(c, c.bg_selected),
                                            );
                                            if let Some(tex) = &thumb_tex {
                                                let size = tex.size_vec2();
                                                let scale = (tile.width() / size.x)
                                                    .min(tile.height() / size.y);
                                                let draw =
                                                    egui::vec2(size.x * scale, size.y * scale);
                                                let img_rect = egui::Rect::from_center_size(
                                                    tile.center(),
                                                    draw,
                                                );
                                                ui.painter().image(
                                                    tex.id(),
                                                    img_rect,
                                                    egui::Rect::from_min_max(
                                                        egui::pos2(0.0, 0.0),
                                                        egui::pos2(1.0, 1.0),
                                                    ),
                                                    Color32::WHITE,
                                                );
                                            } else {
                                                ui.painter().text(
                                                    tile.center(),
                                                    egui::Align2::CENTER_CENTER,
                                                    "IMG",
                                                    FontId::proportional(10.0),
                                                    rgb(c.overlay),
                                                );
                                            }
                                        }
                                        if is_sensitive {
                                            ui.label(
                                                RichText::new("•").size(11.0).color(rgb(c.accent2)),
                                            );
                                        }
                                    },
                                );
                            });
                        });

                    if star_clicked {
                        self.selected = display_idx;
                        *action = Action::ToggleStar(clip_id_value);
                    }
                    // Copy and Delete both act on the selected clip, so the
                    // row has to become the selection before the action runs
                    // — otherwise the button on row four copies row one.
                    if copy_clicked {
                        self.selected = display_idx;
                        *action = Action::Copy;
                    }
                    if delete_clicked {
                        self.selected = display_idx;
                        *action = Action::Delete;
                    }

                    // Whole row minus the controls is clickable. The excluded
                    // strip grew with them: at 36pt the copy button and the ⋮
                    // sat inside the row's own click target, so pressing
                    // either one also fired the row.
                    let full = frame_resp.response.rect;
                    let row_rect = egui::Rect::from_min_max(
                        full.min,
                        egui::pos2(full.max.x - 104.0, full.max.y),
                    );
                    let resp = ui.interact(row_rect, clip_id, egui::Sense::click());
                    ui.interact(full, hover_id, egui::Sense::hover());

                    // Single click selects (and copies, per the "copy on select"
                    // setting) but keeps the palette open — closing on first
                    // click would make double-click-to-preview impossible.
                    // Enter / ⌘1-9 / the Paste button do the paste-and-hide.
                    if resp.clicked() && !star_clicked {
                        self.selected = display_idx;
                        if self.paste_settings.copy_on_select {
                            *action = Action::Copy;
                        }
                    }
                    // Double-click a row = inspect it: open the preview pane
                    // attached to the palette (Esc or Space collapses it).
                    if resp.double_clicked() && !star_clicked {
                        let was_selected = self.selected == display_idx;
                        self.selected = display_idx;
                        let ctx = ui.ctx().clone();
                        // Toggle: double-click the same row again to close the
                        // preview; a different row switches the preview to it.
                        if self.show_preview && was_selected {
                            self.set_preview_open(&ctx, false);
                        } else {
                            self.set_preview_open(&ctx, true);
                        }
                    }
                    if is_selected && self.scroll_to_selected {
                        resp.scroll_to_me(Some(egui::Align::Center));
                    }
                }
                self.scroll_to_selected = false;
            });
    }

    #[allow(dead_code)]
    fn render_sessions_panel(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        ui.label(
            RichText::new("Sessions")
                .size(18.0)
                .strong()
                .color(rgb(c.text)),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("Clipboard bursts grouped by time — open a session to filter clips.")
                .size(12.0)
                .color(rgb(c.subtext)),
        );
        ui.add_space(12.0);

        if self.sessions.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(
                    RichText::new("No sessions yet")
                        .size(14.0)
                        .color(rgb(c.overlay)),
                );
            });
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("sessions_panel_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut filter_session: Option<usize> = None;
                let session_color = rgb(c.green);

                for (i, session) in self.sessions.iter().enumerate() {
                    let dur = session.duration_mins();
                    let dur_str = if dur < 1 {
                        "instant".into()
                    } else if dur < 60 {
                        format!("{} min", dur)
                    } else {
                        let h = dur / 60;
                        let m = dur % 60;
                        if m == 0 {
                            format!("{}h", h)
                        } else {
                            format!("{}h {}m", h, m)
                        }
                    };

                    egui::Frame::none()
                        .fill(surf(c, c.bg_surface))
                        .rounding(Rounding::same(10.0))
                        .inner_margin(Margin::symmetric(14.0, 12.0))
                        .stroke(Stroke::new(1.0, rgb(c.border)))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&session.name)
                                        .size(14.0)
                                        .strong()
                                        .color(rgb(c.text)),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if outline_button(ui, "View clips", session_color, c)
                                            .clicked()
                                        {
                                            filter_session = Some(i);
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let n = session.clip_count();
                                tag_pill(
                                    ui,
                                    &format!("{} {}", n, if n == 1 { "clip" } else { "clips" }),
                                    session_color,
                                    c,
                                );
                                tag_pill(ui, &dur_str, surf(c, c.bg_elevated), c);
                                if !session.top_apps.is_empty() {
                                    tag_pill(
                                        ui,
                                        &session.top_apps.join(", "),
                                        surf(c, c.bg_elevated),
                                        c,
                                    );
                                }
                            });
                        });
                    ui.add_space(8.0);
                }

                if let Some(idx) = filter_session {
                    let session_ids: std::collections::HashSet<i64> =
                        self.sessions[idx].clip_ids.iter().copied().collect();
                    self.search_query.clear();
                    self.filtered = self
                        .clips
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| session_ids.contains(&c.id))
                        .map(|(i, _)| i)
                        .collect();
                    self.selected = 0;
                    self.scroll_to_selected = true;
                    self.active_tab = MainTab::Text;
                }
            });
    }

    fn render_clipboard_behavior_settings(
        &mut self,
        ui: &mut egui::Ui,
        c: &clipd_core::ThemeColors,
    ) {
        let mut dirty = false;

        settings_section(ui, c, "General");
        settings_card(ui, c, |ui| {
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Clipboard,
                &mut self.paste_settings.return_focus_after_copy,
                "Paste into previous app",
                "Jump back and paste after you pick a clip",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::List,
                &mut self.paste_settings.copy_on_select,
                "Copy when selecting a row",
                "Single-click copies. Double-click / Enter still work",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Clipboard,
                &mut self.paste_settings.remember_clipboard,
                "Remember copied items",
                "Save Cmd+C history in Clipd",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Sliders,
                &mut self.paste_settings.multi_slot_enabled,
                "Multi-slot copy/paste",
                "Repeated Cmd+C / Cmd+V fills numbered slots",
            );
        });

        settings_section(ui, c, "Shortcuts");
        settings_card(ui, c, |ui| {
            settings_value_row(ui, c, FooterIcon::Keyboard, "Open Clipd", "Hotkey for the palette", 180.0, |ui| {
                let prev = self.paste_settings.open_gui_hotkey;
                egui::ComboBox::from_id_salt("settings_open_gui_hotkey")
                    .selected_text(self.paste_settings.open_gui_hotkey.label())
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for hk in OpenGuiHotkey::ALL {
                            if hk == OpenGuiHotkey::AltG && !cfg!(target_os = "windows") {
                                continue;
                            }
                            ui.selectable_value(
                                &mut self.paste_settings.open_gui_hotkey,
                                hk,
                                hk.label(),
                            );
                        }
                    });
                if self.paste_settings.open_gui_hotkey != prev {
                    dirty = true;
                }
            });
            settings_card_divider(ui, c);
            settings_value_row(
                ui,
                c,
                FooterIcon::Sparkle,
                "Memory palette",
                "Searchable recall by content, source, or time",
                180.0,
                |ui| {
                    let prev = self.paste_settings.palette_trigger;
                    egui::ComboBox::from_id_salt("settings_palette_trigger")
                        .selected_text(self.paste_settings.palette_trigger.label())
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for t in [
                                PaletteTrigger::CmdShiftV,
                                PaletteTrigger::CtrlOptSpace,
                                PaletteTrigger::OptSpace,
                                PaletteTrigger::Off,
                            ] {
                                ui.selectable_value(
                                    &mut self.paste_settings.palette_trigger,
                                    t,
                                    t.label(),
                                );
                            }
                        });
                    if self.paste_settings.palette_trigger != prev {
                        dirty = true;
                    }
                },
            );
        });

        settings_section(ui, c, "Paste transforms");
        settings_card(ui, c, |ui| {
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Sparkle,
                &mut self.paste_settings.enabled,
                "Transform on paste",
                "Applied with ⌘⇧V — normal Cmd+V is unchanged",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Sparkle,
                &mut self.paste_settings.smart_mode,
                "Smart mode",
                "Pick a transform from the content type",
            );
            settings_card_divider(ui, c);
            settings_card_body(ui, |ui| {
                ui.label(
                    RichText::new("Active transforms")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(6.0);
                let transforms = self.transforms.clone();
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for t in &transforms {
                        let is_active = self.paste_settings.active_transforms.contains(t);
                        let fill = if is_active {
                            rgb(c.green).gamma_multiply(0.22)
                        } else {
                            surf(c, c.bg_elevated)
                        };
                        let stroke = if is_active {
                            Stroke::new(0.9, rgb(c.green).gamma_multiply(0.7))
                        } else {
                            Stroke::new(0.6, rgb(c.border))
                        };
                        let text_col = if is_active {
                            rgb(c.text)
                        } else {
                            rgb(c.subtext)
                        };
                        let label = t.label();
                        if ui
                            .add(
                                egui::Button::new(RichText::new(label).size(11.5).color(text_col))
                                    .fill(fill)
                                    .stroke(stroke)
                                    .rounding(Rounding::same(8.0))
                                    .min_size(egui::vec2(0.0, 28.0)),
                            )
                            .clicked()
                        {
                            if is_active {
                                self.paste_settings.active_transforms.retain(|x| x != t);
                            } else {
                                self.paste_settings.active_transforms.push(t.clone());
                            }
                            dirty = true;
                        }
                    }
                });
                ui.add_space(8.0);
                let resp = ui.add_sized(
                    [ui.available_width(), 26.0],
                    egui::TextEdit::singleline(&mut self.paste_settings.default_ai_prompt)
                        .hint_text("Optional AI prompt for transform…")
                        .font(egui::TextStyle::Body),
                );
                if resp.changed() || resp.lost_focus() {
                    dirty = true;
                }
            });
        });

        settings_section(ui, c, "Advanced");
        settings_card(ui, c, |ui| {
            #[cfg(target_os = "macos")]
            if load_hotkey_status() == HotkeyStatus::NeedsAccessibility {
                settings_card_body(ui, |ui| {
                    ui.label(
                        RichText::new("Global shortcuts need keyboard access in System Settings.")
                            .size(11.0)
                            .color(rgb(c.overlay)),
                    );
                    ui.add_space(6.0);
                    if ui.button("Open Privacy Settings").clicked() {
                        clipd_core::request_keyboard_permissions();
                        clipd_core::open_keyboard_permission_settings();
                    }
                });
                settings_card_divider(ui, c);
            }
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Keyboard,
                &mut self.paste_settings.letter_slots_enabled,
                "Letter slots A–Z",
                "Named slots for faster recall",
            );
            if self.paste_settings.letter_slots_enabled {
                for (keys, what) in letter_slot_bindings() {
                    settings_shortcut_help(ui, c, keys, what);
                }
            }
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Sliders,
                &mut self.paste_settings.extended_slots_enabled,
                "Extended slots 11–30",
                "Option+C/V multi-tap for higher slots",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Eye,
                &mut self.paste_settings.hud_enabled,
                "HUD notifications",
                "Flash a small confirmation when copying to a slot",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::List,
                &mut self.paste_settings.palette_aliases_enabled,
                "Letter aliases in palette",
                "List letter slots in the palette as @A rows",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Keyboard,
                &mut self.paste_settings.quick_letter_slots_enabled,
                "Quick letter save",
                "Double-tap Cmd+C then a letter",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Keyboard,
                &mut self.paste_settings.direct_letter_shortcuts_enabled,
                "Direct A–Z shortcuts",
                "Ctrl+Option+C/V then A–Z",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Sliders,
                &mut self.paste_settings.batch_drain_enabled,
                "Batch-drain paste",
                "Paste every filled slot in order",
            );
            settings_card_divider(ui, c);
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Clipboard,
                &mut self.paste_settings.copy_multi_tap_restore,
                "Restore clipboard after multi-tap copy",
                "After Cmd+C × N, restore slot 1's content",
            );
            if self.paste_settings.open_gui_hotkey == OpenGuiHotkey::CtrlSpace {
                settings_card_divider(ui, c);
                settings_value_row(
                    ui,
                    c,
                    FooterIcon::Keyboard,
                    "Ctrl+Space action",
                    "What Ctrl+Space does when it opens Clipd",
                    180.0,
                    |ui| {
                        let prev = self.paste_settings.ctrl_space_action;
                        egui::ComboBox::from_id_salt("settings_ctrl_space_action")
                            .selected_text(self.paste_settings.ctrl_space_action.label())
                            .width(160.0)
                            .show_ui(ui, |ui| {
                                for action in CtrlSpaceAction::ALL {
                                    ui.selectable_value(
                                        &mut self.paste_settings.ctrl_space_action,
                                        action,
                                        action.label(),
                                    );
                                }
                            });
                        if self.paste_settings.ctrl_space_action != prev {
                            dirty = true;
                        }
                    },
                );
            }
        });

        if dirty {
            save_paste_transform_settings(&self.paste_settings);
        }
    }

    fn render_actions_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_section(ui, c, "Custom actions");
        settings_card(ui, c, |ui| {
            settings_card_body(ui, |ui| {
                ui.label(
                    RichText::new("Shell command on a clip — run from preview (Space).")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(8.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_action_name)
                        .hint_text("name (e.g. Pretty JSON)")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(5.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_action_command)
                        .hint_text("command — e.g.  jq .   ·   tr a-z A-Z   ·   python3 ~/x.py")
                        .desired_width(ui.available_width())
                        .font(egui::TextStyle::Monospace),
                );
                ui.add_space(5.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_action_auto)
                        .hint_text("auto-run when a new clip contains… (optional)")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("action_output")
                        .selected_text(self.new_action_output.label())
                        .show_ui(ui, |ui| {
                            for o in ActionOutput::ALL {
                                ui.selectable_value(&mut self.new_action_output, o, o.label());
                            }
                        });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_save = !self.new_action_name.trim().is_empty()
                            && !self.new_action_command.trim().is_empty();
                        if ui
                            .add_enabled(
                                can_save,
                                egui::Button::new(
                                    RichText::new("Add action").size(12.0).color(rgb(c.bg_base)),
                                )
                                .fill(rgb(c.accent))
                                .rounding(Rounding::same(8.0)),
                            )
                            .clicked()
                        {
                            let mut action = CustomAction::new(
                                self.new_action_name.trim(),
                                self.new_action_command.trim(),
                                self.new_action_output,
                            );
                            action.auto_pattern = self.new_action_auto.trim().to_string();
                            self.custom_actions.push(action);
                            self.persist_actions();
                            self.new_action_name.clear();
                            self.new_action_command.clear();
                            self.new_action_auto.clear();
                        }
                    });
                });
            });
        });

        if self.custom_actions.is_empty() {
            return;
        }

        let mut to_delete: Option<usize> = None;
        let mut changed = false;
        settings_card(ui, c, |ui| {
            let count = self.custom_actions.len();
            for i in 0..count {
                if i > 0 {
                    settings_card_divider(ui, c);
                }
                let enabled = self.custom_actions[i].enabled;
                let name = self.custom_actions[i].name.clone();
                let command = self.custom_actions[i].command.clone();
                let output = self.custom_actions[i].output.label().to_string();
                settings_value_row(
                    ui,
                    c,
                    FooterIcon::Sparkle,
                    &name,
                    &format!("{command}  ·  {output}"),
                    90.0,
                    |ui| {
                        if pill_button(ui, "Delete", c).clicked() {
                            to_delete = Some(i);
                        }
                        if mini_switch(ui, enabled, c) {
                            self.custom_actions[i].enabled = !enabled;
                            changed = true;
                        }
                    },
                );
            }
        });
        if let Some(i) = to_delete {
            self.custom_actions.remove(i);
            changed = true;
        }
        if changed {
            self.persist_actions();
        }
    }

    fn render_snippets_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_section(ui, c, "Snippets");
        settings_card(ui, c, |ui| {
            settings_card_body(ui, |ui| {
                ui.label(
                    RichText::new("Reusable text. Type its trigger in search, then Enter to paste it.")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_snippet_trigger)
                            .hint_text("trigger (e.g. sig)")
                            .desired_width(130.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_snippet_name)
                            .hint_text("name (optional)")
                            .desired_width(ui.available_width()),
                    );
                });
                ui.add_space(5.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.new_snippet_body)
                        .hint_text("Snippet text…")
                        .desired_rows(2)
                        .desired_width(ui.available_width()),
                );
                ui.add_space(5.0);
                let can_save = !self.new_snippet_trigger.trim().is_empty()
                    && !self.new_snippet_body.trim().is_empty();
                if ui
                    .add_enabled(
                        can_save,
                        egui::Button::new(
                            RichText::new("Save snippet")
                                .size(12.0)
                                .color(rgb(c.bg_base)),
                        )
                        .fill(rgb(c.accent))
                        .rounding(Rounding::same(8.0)),
                    )
                    .clicked()
                {
                    let _ = self.store.upsert_snippet(
                        self.new_snippet_trigger.trim(),
                        self.new_snippet_name.trim(),
                        self.new_snippet_body.trim_end(),
                    );
                    self.new_snippet_trigger.clear();
                    self.new_snippet_name.clear();
                    self.new_snippet_body.clear();
                    self.refresh_snippets();
                }
            });
        });

        let snippets = self.snippets.clone();
        if snippets.is_empty() {
            return;
        }
        let mut delete_id: Option<i64> = None;
        settings_card(ui, c, |ui| {
            for (i, s) in snippets.iter().enumerate() {
                if i > 0 {
                    settings_card_divider(ui, c);
                }
                let preview = s.preview();
                settings_value_row(
                    ui,
                    c,
                    FooterIcon::List,
                    &s.trigger,
                    &preview,
                    80.0,
                    |ui| {
                        if pill_button(ui, "Delete", c).clicked() {
                            delete_id = Some(s.id);
                        }
                    },
                );
            }
        });
        if let Some(id) = delete_id {
            let _ = self.store.delete_snippet(id);
            self.refresh_snippets();
        }
    }


    /// AI provider settings. Ask, embeddings and transform-on-paste all read the
    /// same `transform.json`; before this existed the only way to set a key was
    /// to hand-write that file, so Ask silently fell back to retrieval-only and
    /// looked broken.
    fn render_ai_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        let has_key = self
            .ai_config
            .api_key
            .as_deref()
            .is_some_and(|k| !k.is_empty());
        let local = clipd_core::transform::is_local_endpoint(&self.ai_config.api_url);

        settings_section(ui, c, "Provider");
        settings_card(ui, c, |ui| {
            if popover_setting_row(
                ui,
                c,
                FooterIcon::Sparkle,
                "Hosted",
                "OpenAI-compatible API — needs a key",
                if local {
                    RowControl::Chevron
                } else {
                    RowControl::Toggle(true)
                },
            ) && local
            {
                self.ai_config.api_url = "https://api.openai.com/v1/chat/completions".into();
                self.ai_config.model = "gpt-4o-mini".into();
                self.ai_test_status = None;
            }
            settings_card_divider(ui, c);
            if popover_setting_row(
                ui,
                c,
                FooterIcon::Window,
                "Local model",
                "Ollama or LM Studio — nothing leaves this machine",
                if local {
                    RowControl::Toggle(true)
                } else {
                    RowControl::Chevron
                },
            ) && !local
            {
                self.ai_config.api_url = "http://localhost:11434/v1/chat/completions".into();
                self.ai_config.model = "llama3.2".into();
                self.ai_test_status = None;
            }
        });

        settings_section(ui, c, "Connection");
        settings_card(ui, c, |ui| {
            settings_card_body(ui, |ui| {
                if local {
                    ui.label(
                        RichText::new(
                            "Local endpoints need no API key — leave it blank. Install Ollama, run \
                             `ollama pull llama3.2`, and Test connection.",
                        )
                        .size(10.5)
                        .color(rgb(c.subtext)),
                    );
                    ui.add_space(8.0);
                }
                let row = |ui: &mut egui::Ui, label: &str| {
                    ui.add_sized(
                        [76.0, 18.0],
                        egui::Label::new(RichText::new(label).size(11.5).color(rgb(c.subtext))),
                    );
                };

                ui.horizontal(|ui| {
                    row(ui, "API key");
                    let hint = if has_key {
                        "saved — type to replace"
                    } else if local {
                        "not needed for a local model"
                    } else {
                        "sk-..."
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ai_key_input)
                            .desired_width(ui.available_width())
                            .password(true)
                            .hint_text(hint),
                    );
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    row(ui, "Endpoint");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ai_config.api_url)
                            .desired_width(ui.available_width())
                            .hint_text("https://api.openai.com/v1/chat/completions"),
                    );
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    row(ui, "Model");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ai_config.model)
                            .desired_width(ui.available_width())
                            .hint_text("gpt-4o-mini"),
                    );
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        let typed = self.ai_key_input.trim();
                        if !typed.is_empty() {
                            self.ai_config.api_key = Some(typed.to_string());
                            self.ai_key_input.clear();
                        }
                        if self.ai_config.api_url.trim().is_empty() {
                            self.ai_config.api_url =
                                "https://api.openai.com/v1/chat/completions".into();
                        }
                        if self.ai_config.model.trim().is_empty() {
                            self.ai_config.model = "gpt-4o-mini".into();
                        }
                        save_transform_config(&self.ai_config);
                        self.ai_test_status = Some((true, "Saved.".into()));
                    }

                    let can_test = has_key || !self.ai_key_input.trim().is_empty() || local;
                    if ui
                        .add_enabled(
                            can_test && self.ai_test_rx.is_none(),
                            egui::Button::new("Test connection"),
                        )
                        .on_hover_text("Send a one-token request to check the key and endpoint")
                        .clicked()
                    {
                        self.start_ai_test(ui.ctx());
                    }

                    if self.ai_test_rx.is_some() {
                        ui.spinner();
                    }

                    if ui
                        .add_enabled(has_key, egui::Button::new("Remove key"))
                        .clicked()
                    {
                        self.ai_config.api_key = None;
                        self.ai_key_input.clear();
                        save_transform_config(&self.ai_config);
                        self.ai_test_status = Some((true, "Key removed.".into()));
                    }
                });

                if let Some((ok, msg)) = &self.ai_test_status {
                    ui.add_space(4.0);
                    let color = if *ok { rgb(c.green) } else { rgb(c.accent2) };
                    ui.label(RichText::new(msg).size(11.0).color(color));
                }

                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "Any OpenAI-compatible endpoint works. Stored in {}",
                        clipd_core::transform_config_path().display()
                    ))
                    .size(10.5)
                    .color(rgb(c.subtext)),
                );
            });
        });
    }


    /// Verify the key and endpoint with a minimal request, on a worker thread so
    /// the window doesn't freeze for the round trip.
    fn start_ai_test(&mut self, ctx: &egui::Context) {
        let mut api = self.ai_config.clone();
        let typed = self.ai_key_input.trim();
        if !typed.is_empty() {
            api.api_key = Some(typed.to_string());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = match clipd_core::transform::probe_api(&api) {
                Ok(model) => (true, format!("Works — responded as “{model}”.")),
                Err(e) => (false, e),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.ai_test_rx = Some(rx);
        self.ai_test_status = None;
    }

    fn poll_ai_test(&mut self) {
        let Some(rx) = &self.ai_test_rx else { return };
        match rx.try_recv() {
            Ok(status) => {
                self.ai_test_status = Some(status);
                self.ai_test_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ai_test_status = Some((false, "The test stopped unexpectedly.".into()));
                self.ai_test_rx = None;
            }
        }
    }

    fn render_vault_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_section(ui, c, "Saved passwords");
        settings_card(ui, c, |ui| {
            if settings_toggle_row(
                ui,
                c,
                FooterIcon::Lock,
                &mut self.privacy_config.offer_vault_on_secret,
                "Offer to vault detected passwords",
                "When a copied password is detected, prompt to save it",
            ) {
                save_privacy_config(&self.privacy_config);
            }

            if self.vault_targets.is_empty() {
                settings_card_divider(ui, c);
                settings_card_copy(
                    ui,
                    c,
                    "No vault backend found",
                    "Install the 1Password CLI (`op`) or Bitwarden CLI (`bw`). Keychain is available on macOS.",
                );
                return;
            }

            settings_card_divider(ui, c);
            settings_card_body(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Save to").size(12.0).color(rgb(c.text)));
                    let selected_label = self
                        .vault_selected
                        .map(|t| t.label())
                        .unwrap_or("Pick one");
                    egui::ComboBox::from_id_salt("vault_target")
                        .selected_text(selected_label)
                        .show_ui(ui, |ui| {
                            for t in self.vault_targets.clone() {
                                ui.selectable_value(&mut self.vault_selected, Some(t), t.label());
                            }
                        });
                });

                ui.add_space(4.0);
                let field = |ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str| {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [70.0, 18.0],
                            egui::Label::new(RichText::new(label).size(11.5).color(rgb(c.subtext))),
                        );
                        ui.add(
                            egui::TextEdit::singleline(value)
                                .desired_width(220.0)
                                .hint_text(hint),
                        );
                    });
                };
                field(ui, "Title", &mut self.vault_title, "e.g. GitHub");
                field(ui, "Username", &mut self.vault_username, "e.g. me@example.com");
                field(ui, "URL", &mut self.vault_url, "optional");

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let save = ui.add(
                        egui::Button::new(
                            RichText::new("Save clipboard to vault")
                                .size(12.0)
                                .color(rgb(c.bg_base)),
                        )
                        .fill(rgb(c.accent))
                        .rounding(Rounding::same(8.0)),
                    );
                    if save.clicked() {
                        self.save_clipboard_to_vault();
                    }
                    save.on_hover_text(
                        "Reads the current clipboard and stores it as a login in the selected vault.",
                    );
                });

                if let Some((ok, msg)) = &self.vault_status {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(if *ok {
                            format!("✓ {msg}")
                        } else {
                            format!("✗ {msg}")
                        })
                        .size(11.0)
                        .color(if *ok { rgb(c.green) } else { rgb(c.accent2) }),
                    );
                }

                ui.label(
                    RichText::new(
                        "The password is read from the clipboard at save time — it is never written to clipd's history.",
                    )
                    .size(10.5)
                    .color(rgb(c.subtext)),
                );
            });
        });
    }

    /// "Build your own palette" — accent / background / text pickers that
    /// override whatever base theme is active. Saved and applied live.
    fn render_custom_colors_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        let mut changed = false;
        settings_section(ui, c, "Custom colors");
        settings_card(ui, c, |ui| {
            changed |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Palette,
                &mut self.custom_colors.enabled,
                "Use custom colors",
                "Overrides the theme swatches above",
            );
            if self.custom_colors.enabled {
                settings_card_divider(ui, c);
                settings_card_body(ui, |ui| {
                    changed |= color_row(ui, c, "Accent", &mut self.custom_colors.accent);
                    changed |= color_row(ui, c, "Background", &mut self.custom_colors.background);
                    changed |= color_row(ui, c, "Text", &mut self.custom_colors.text);
                    ui.add_space(4.0);
                    if ui
                        .add(egui::Button::new(
                            RichText::new("Reset colors")
                                .size(11.5)
                                .color(rgb(c.subtext)),
                        ))
                        .clicked()
                    {
                        let enabled = self.custom_colors.enabled;
                        self.custom_colors = CustomColors {
                            enabled,
                            ..Default::default()
                        };
                        changed = true;
                    }
                });
            }
        });
        if changed {
            save_custom_colors(&self.custom_colors);
            apply_theme(ui.ctx(), self.theme);
        }
    }

    /// Quiet navigation rail. Search is intentionally kept in the bottom
    /// command bar so the content begins immediately under this header.
    /// Compact brand row: green "C" + Clipd · pin / settings / close.
    /// In Settings: "Clipd" + "Settings" title, close returns to the list.
    fn render_brand_header(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            draw_brand_mark(ui, c);
            ui.label(
                RichText::new("Clipd")
                    .size(15.0)
                    .strong()
                    .color(rgb(c.text)),
            );
            if self.active_tab == MainTab::Settings {
                ui.label(RichText::new("Settings").size(15.0).color(rgb(c.subtext)));
            }
            if self.active_tab == MainTab::Vault {
                ui.label(RichText::new("Vault").size(15.0).color(rgb(c.subtext)));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let close = chrome_icon_button(ui, "×", false, c).on_hover_text(if self
                    .active_tab
                    == MainTab::Settings
                {
                    "Back to clipboard  ·  Esc"
                } else {
                    "Close  ·  Esc"
                });
                if close.clicked() {
                    if self.active_tab != MainTab::Text || self.show_preview {
                        self.active_tab = MainTab::Text;
                        let ctx = ui.ctx().clone();
                        self.set_preview_open(&ctx, false);
                    } else {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                if self.active_tab != MainTab::Settings && self.active_tab != MainTab::Vault {
                    let vault =
                        chrome_icon_button(ui, "🔐", false, c).on_hover_text("Vault  ·  encrypted API keys");
                    if vault.clicked() {
                        self.active_tab = MainTab::Vault;
                        self.refresh_vault_secrets();
                    }
                    let settings =
                        chrome_icon_button(ui, "⚙", false, c).on_hover_text("Settings  ·  ⌘,");
                    if settings.clicked() {
                        self.active_tab = MainTab::Settings;
                    }
                    let pin = chrome_icon_button(ui, "📌", self.window_pinned, c).on_hover_text(
                        if self.window_pinned {
                            "Unpin window"
                        } else {
                            "Keep window on top"
                        },
                    );
                    if pin.clicked() {
                        self.window_pinned = !self.window_pinned;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                            if self.window_pinned {
                                egui::WindowLevel::AlwaysOnTop
                            } else {
                                egui::WindowLevel::Normal
                            },
                        ));
                    }
                }
            });
        });
    }

    /// Settings category pills — General / Clipboard / AI / Appearance / Privacy.
    fn render_settings_category_tabs(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            for cat in SettingsCategory::ALL {
                if tiny_filter_chip(ui, cat.label(), self.settings_category == cat, self.theme, c) {
                    self.settings_category = cat;
                }
            }
        });
    }

    /// Filter row — All / Links / Text / Code / Images / Pinned.
    fn render_filter_pills(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            for (filter, label) in ContentFilter::MAIN {
                if tiny_filter_chip(ui, label, self.content_filter == filter, self.theme, c) {
                    self.content_filter = filter;
                    self.apply_filter();
                }
            }
        });
    }

    /// Accessibility + quick-settings banners under the chrome (Text tab).
    fn render_text_banners(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        #[cfg(target_os = "macos")]
        if load_hotkey_status() == HotkeyStatus::NeedsAccessibility {
            let (warn_fill, warn_title, warn_body, warn_btn_fill, warn_btn_text) =
                warning_colors(self.theme.is_light());
            egui::Frame::none()
                .fill(warn_fill)
                .rounding(Rounding::same(8.0))
                .inner_margin(Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("Multi-slot copy & HUD need keyboard access")
                            .size(11.5)
                            .strong()
                            .color(warn_title),
                    );
                    ui.label(
                        RichText::new(format!(
                            "Enable Clipd under {} in System Settings → Privacy & Security. \
                             The daemon retries automatically once toggled on.",
                            clipd_core::missing_keyboard_permission_label()
                        ))
                        .size(10.5)
                        .color(warn_body),
                    );
                    ui.add_space(4.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Open Privacy Settings")
                                    .size(11.0)
                                    .color(warn_btn_text),
                            )
                            .fill(warn_btn_fill),
                        )
                        .clicked()
                    {
                        clipd_core::request_keyboard_permissions();
                        clipd_core::open_keyboard_permission_settings();
                    }
                });
            ui.add_space(8.0);
        }

        if self.show_quick_settings {
            self.render_quick_settings(ui, c);
        }
        let _ = c;
    }

    /// Search field with magnifier and a trailing `/` shortcut hint.
    /// In Settings this searches settings categories instead of clips.
    fn render_search_bar(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Action,
        c: &clipd_core::ThemeColors,
    ) {
        let search_w = ui.available_width();
        let asking = self.in_ask_mode();
        let in_settings = self.active_tab == MainTab::Settings;
        let spotlight = self.theme == Theme::GlassLight;
        let search_frame = egui::Frame::none()
            .fill(if spotlight {
                // Frosted, not solid: at 216 alpha this was a white slab sunk
                // into the glass. Its rim is what separates it.
                Color32::from_white_alpha(96)
            } else {
                surf(c, c.bg_elevated)
            })
            .rounding(Rounding::same(if spotlight { 13.0 } else { 10.0 }))
            .stroke(Stroke::new(
                if spotlight { 0.7 } else { 0.8 },
                if asking && !in_settings {
                    rgb(c.accent).gamma_multiply(0.72)
                } else if spotlight {
                    Color32::from_rgba_unmultiplied(188, 198, 214, 190)
                } else {
                    rgb(c.border)
                },
            ))
            .inner_margin(Margin::symmetric(10.0, 7.0));

        search_frame.show(ui, |ui| {
            ui.set_width(search_w);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                draw_search_icon(ui, rgb(c.subtext));
                let hint = match self.active_tab {
                    MainTab::Collections => "Search pins and collections…",
                    MainTab::Settings => "Search settings...",
                    MainTab::Vault => "Search vault…",
                    MainTab::Text if asking => "Ask, then press Enter",
                    MainTab::Text => "Search clips, links, code...",
                };
                let slash_w = if in_settings { 0.0 } else { 28.0 };
                // Constrain the text field so it never pushes the `/` badge
                // off the edge or overflows the search frame.
                let field_w = (ui.available_width() - slash_w).max(60.0).min(search_w - 40.0);
                if in_settings {
                    let search = ui.add_sized(
                        [field_w, 18.0],
                        egui::TextEdit::singleline(&mut self.settings_query)
                            .id(egui::Id::new("settings_search"))
                            .hint_text(hint)
                            .frame(false)
                            .font(egui::TextStyle::Body),
                    );
                    if self.focus_search {
                        search.request_focus();
                        self.focus_search = false;
                    }
                    if search.changed() {
                        if let Some(cat) = SettingsCategory::from_query(&self.settings_query) {
                            self.settings_category = cat;
                        }
                    }
                } else {
                    let search = ui.add_sized(
                        [field_w, 18.0],
                        egui::TextEdit::singleline(&mut self.search_query)
                            .id(egui::Id::new("clip_search"))
                            .hint_text(hint)
                            .frame(false)
                            .font(egui::TextStyle::Body),
                    );
                    if self.focus_search {
                        search.request_focus();
                        self.focus_search = false;
                    }
                    if search.changed() {
                        if self.in_ask_mode() {
                            self.ask.clear_answer();
                        }
                        self.apply_filter();
                    }
                    if self.active_tab == MainTab::Text
                        && search.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        *action = if self.in_ask_mode() {
                            Action::Ask
                        } else {
                            Action::Paste
                        };
                    }
                    egui::Frame::none()
                        .fill(surf(c, c.bg_selected))
                        .rounding(Rounding::same(5.0))
                        .inner_margin(Margin::symmetric(5.0, 1.0))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("/")
                                    .size(10.5)
                                    .strong()
                                    .family(egui::FontFamily::Monospace)
                                    .color(rgb(c.overlay)),
                            );
                        });
                }
            });
        });
    }

    fn render_bottom_bar(
        &mut self,
        ui: &mut egui::Ui,
        _action: &mut Action,
        c: &clipd_core::ThemeColors,
    ) {
        // Mockup footer: green Capturing (left) · outline clock (true centre) ·
        // ⌘⇧V chip (right). One slim row, space-between alignment.
        let row_h = 28.0;
        let full_w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(full_w, row_h), egui::Sense::hover());

        // Left — status.
        let left = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top()),
            egui::vec2(full_w * 0.4, row_h),
        );
        ui.allocate_ui_at_rect(left, |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (dot, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 3.5, capture_dot_color(self.theme, c));
                ui.label(
                    RichText::new("Capturing")
                        .size(12.0)
                        .color(rgb(c.subtext)),
                );
            });
        });

        // Centre — clock, painted at the exact midpoint.
        draw_clock_icon_at(ui.painter(), rect.center(), rgb(c.overlay));

        // Right — shortcut hint.
        let right = egui::Rect::from_min_size(
            egui::pos2(rect.right() - full_w * 0.4, rect.top()),
            egui::vec2(full_w * 0.4, row_h),
        );
        ui.allocate_ui_at_rect(right, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                footer_shortcut_badge(ui, "⌘ ⇧ V", c);
            });
        });
    }

    /// Inline quick-settings rows (mockup style): theme swatches, the
    /// paste-on-select switch, and a link to the full settings page.
    fn render_quick_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        let row = |ui: &mut egui::Ui, label: &str, f: &mut dyn FnMut(&mut egui::Ui)| {
            ui.add_space(9.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(12.5).color(rgb(c.text)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), f);
            });
            ui.add_space(9.0);
        };

        row(ui, "Theme", &mut |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            // Wrap so Glass Light / Glass Dark never clip off a narrow HUD.
            ui.horizontal_wrapped(|ui| {
                for theme in Theme::ALL {
                    let tc = theme.colors();
                    let active = self.theme == theme;
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                    let center = rect.center();
                    ui.painter().circle_filled(center, 8.0, rgb(tc.bg_base));
                    if theme.is_glass() {
                        if let Some((tl, br)) = theme.shell_glows() {
                            ui.painter().circle_filled(
                                egui::pos2(center.x - 2.5, center.y - 2.0),
                                3.5,
                                rgba(tl, 200),
                            );
                            ui.painter().circle_filled(
                                egui::pos2(center.x + 2.5, center.y + 2.0),
                                3.5,
                                rgba(br, 200),
                            );
                        }
                        ui.painter().circle_filled(center, 2.4, rgb(tc.green));
                        // Pale rim so glass swatches don't look like solid darks.
                        ui.painter().circle_stroke(
                            center,
                            8.0,
                            Stroke::new(1.2, Color32::from_rgba_unmultiplied(220, 230, 245, 160)),
                        );
                    } else {
                        ui.painter().circle_filled(center, 3.0, rgb(tc.green));
                    }
                    ui.painter().circle_stroke(
                        center,
                        8.0,
                        if active {
                            Stroke::new(2.0, rgb(c.accent))
                        } else if !theme.is_glass() {
                            Stroke::new(1.0, rgb(c.border))
                        } else {
                            Stroke::NONE
                        },
                    );
                    let resp = resp
                        .on_hover_text(theme.label())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.clicked() {
                        self.theme = theme;
                        save_theme(self.theme);
                        apply_theme(ui.ctx(), self.theme);
                    }
                }
            });
        });
        hairline(ui, c);

        let mut toggled = false;
        row(ui, "Paste on select", &mut |ui| {
            if mini_switch(ui, self.paste_settings.copy_on_select, c) {
                toggled = true;
            }
        });
        if toggled {
            self.paste_settings.copy_on_select = !self.paste_settings.copy_on_select;
            save_paste_transform_settings(&self.paste_settings);
        }
        hairline(ui, c);

        let mut open_all = false;
        row(ui, "All settings", &mut |ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new("open").size(11.5).color(rgb(c.accent)))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::NONE),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                open_all = true;
            }
        });
        if open_all {
            self.show_quick_settings = false;
            self.active_tab = MainTab::Settings;
        }
        hairline(ui, c);
    }

    fn render_surface_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_section(ui, c, "Surface");
        settings_card(ui, c, |ui| {
            if popover_setting_row(
                ui,
                c,
                FooterIcon::Clipboard,
                "Clipboard HUD",
                "Search and pick recent clips from the menu bar",
                RowControl::Chevron,
            ) {
                self.switch_surface(ui.ctx(), SurfaceMode::Hud);
            }
        });
    }

    fn render_settings_panel(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        egui::Frame::none()
            .inner_margin(Margin {
                left: SETTINGS_GUTTER_X,
                right: SETTINGS_GUTTER_X,
                top: 4.0,
                bottom: SETTINGS_GUTTER_Y,
            })
            .show(ui, |ui| {
                let content_w = ui.available_width().min(SETTINGS_MAX_WIDTH);
                ui.set_max_width(content_w);

                egui::ScrollArea::vertical()
                    .id_salt(("settings_scroll", self.settings_category.label()))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let content_w = ui.available_width().min(SETTINGS_MAX_WIDTH);
                        ui.set_max_width(content_w);
                        match self.settings_category {
                            SettingsCategory::General => self.render_settings_general(ui, c),
                            SettingsCategory::Clipboard => {
                                self.render_clipboard_behavior_settings(ui, c);
                            }
                            SettingsCategory::Ai => self.render_ai_settings(ui, c),
                            SettingsCategory::Appearance => {
                                self.render_settings_appearance(ui, c);
                            }
                            SettingsCategory::Privacy => self.render_settings_privacy(ui, c),
                        }
                    });
            });
    }

    fn render_settings_general(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        self.render_surface_settings(ui, c);

        settings_section(ui, c, "Sending");
        settings_card(ui, c, |ui| {
            self.render_sending_settings(ui, c);
        });

        self.render_snippets_settings(ui, c);
        self.render_actions_settings(ui, c);
        self.render_vault_settings(ui, c);
    }

    /// The layout switch, and everything the island needs configuring.
    ///
    /// Lives in Appearance because that is where someone goes to change how
    /// clipd *looks*; the island is a different-shaped clipd, not a different
    /// clipboard.
    fn render_layout_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        let current = self.paste_settings.gui_layout;
        let mut chosen: Option<GuiLayout> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            for layout in GuiLayout::ALL {
                if layout_card(ui, c, layout, current == layout) {
                    chosen = Some(layout);
                }
            }
        });
        if let Some(layout) = chosen {
            if layout != current {
                self.set_gui_layout(layout);
            }
        }
    }

    /// Switch layouts, and start or stop the island process to match.
    ///
    /// The island is a separate process (it owns a window that outlives the
    /// palette), so the setting alone would leave the old layout on screen.
    fn set_gui_layout(&mut self, layout: GuiLayout) {
        self.paste_settings.gui_layout = layout;
        save_paste_transform_settings(&self.paste_settings);
        match layout {
            GuiLayout::Notch => island::start_island(),
            GuiLayout::Palette => island::stop_island(),
        }
    }

    fn render_island_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        let mut dirty = false;

        settings_section(ui, c, "Island");
        settings_card(ui, c, |ui| {
            let mut hover = self.island.config.expand_on_hover;
            if settings_toggle_row(
                ui,
                c,
                FooterIcon::Eye,
                &mut hover,
                "Open on hover",
                "Off means the island only opens when you click it",
            ) {
                self.island.config.expand_on_hover = hover;
                dirty = true;
            }
            settings_card_divider(ui, c);
            let mut live = self.island.config.live_activity;
            if settings_toggle_row(
                ui,
                c,
                FooterIcon::Sparkle,
                &mut live,
                "Announce copies",
                "Flash each new clip in the island for a couple of seconds",
            ) {
                self.island.config.live_activity = live;
                dirty = true;
            }
            settings_card_divider(ui, c);
            let mut anchor = self.island.config.anchor;
            settings_value_row(ui, c, FooterIcon::Window, "Position", "Where the island sits", 160.0, |ui| {
                let resp = egui::ComboBox::from_id_salt("island_anchor")
                    .selected_text(anchor.label())
                    .show_ui(ui, |ui| {
                        for option in clipd_core::IslandAnchor::ALL {
                            ui.selectable_value(&mut anchor, option, option.label());
                        }
                    });
                if resp.response.changed() {
                    dirty = true;
                }
            });
            if anchor != self.island.config.anchor {
                self.island.config.anchor = anchor;
                dirty = true;
            }
            settings_card_divider(ui, c);
            let mut width = self.island.config.notch_width;
            settings_value_row(
                ui,
                c,
                FooterIcon::Sliders,
                "Resting width",
                "Auto measures the real notch",
                180.0,
                |ui| {
                    if ui
                        .add(
                            egui::Slider::new(&mut width, 0.0..=420.0)
                                .step_by(2.0)
                                .custom_formatter(|v, _| {
                                    if v < 1.0 {
                                        "Auto".to_string()
                                    } else {
                                        format!("{v:.0} pt")
                                    }
                                }),
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                },
            );
            if (width - self.island.config.notch_width).abs() > f32::EPSILON {
                self.island.config.notch_width = width;
                dirty = true;
            }
            settings_card_divider(ui, c);
            let mut rows = self.island.config.clip_rows as u32;
            settings_value_row(ui, c, FooterIcon::List, "Clips shown", "How many clips the island lists", 120.0, |ui| {
                if ui.add(egui::Slider::new(&mut rows, 1..=8)).changed() {
                    dirty = true;
                }
            });
            if rows as usize != self.island.config.clip_rows {
                self.island.config.clip_rows = rows as usize;
                dirty = true;
            }
        });

        settings_section(ui, c, "Modules");
        let mut toggle: Option<(clipd_core::IslandModule, bool)> = None;
        let mut shift: Option<(clipd_core::IslandModule, isize)> = None;
        settings_card(ui, c, |ui| {
            for (i, module) in clipd_core::IslandModule::ALL.iter().copied().enumerate() {
                if i > 0 {
                    settings_card_divider(ui, c);
                }
                let on = self.island.config.has(module);
                let supported = module.supported();
                let detail = if supported {
                    module.detail().to_string()
                } else {
                    format!("{} macOS only.", module.detail())
                };
                let title = if module.uses_network() {
                    format!("{}  ·  network", module.label())
                } else {
                    module.label().to_string()
                };
                ui.add_enabled_ui(supported, |ui| {
                    settings_value_row(ui, c, FooterIcon::App, &title, &detail, 110.0, |ui| {
                        if mini_switch(ui, on, c) {
                            toggle = Some((module, !on));
                        }
                        if on {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("▼").size(10.0).color(rgb(c.subtext)),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text("Move down")
                                .clicked()
                            {
                                shift = Some((module, 1));
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("▲").size(10.0).color(rgb(c.subtext)),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text("Move up")
                                .clicked()
                            {
                                shift = Some((module, -1));
                            }
                        }
                    });
                });
            }
        });
        if let Some((module, on)) = toggle {
            self.island.config.set(module, on);
            dirty = true;
        }
        if let Some((module, delta)) = shift {
            self.island.config.shift(module, delta);
            dirty = true;
        }

        settings_card(ui, c, |ui| {
            if popover_setting_row(
                ui,
                c,
                FooterIcon::Power,
                "Restart island",
                "Recover a wedged window, or move it after a display change",
                RowControl::Chevron,
            ) {
                island::stop_island();
                island::start_island();
            }
        });

        if dirty {
            clipd_core::save_island_config(&self.island.config);
            self.island.invalidate();
        }
    }

    fn render_settings_appearance(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_section(ui, c, "Layout");
        settings_card(ui, c, |ui| {
            settings_card_body(ui, |ui| {
                ui.label(
                    RichText::new("Which surface clipd lives in. The hotkey still opens the palette either way.")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(8.0);
                self.render_layout_settings(ui, c);
            });
        });

        if self.paste_settings.gui_layout == GuiLayout::Notch {
            self.render_island_settings(ui, c);
        }

        settings_section(ui, c, "Theme");
        settings_card(ui, c, |ui| {
            settings_card_body(ui, |ui| {
                ui.label(
                    RichText::new("Cmd+T cycles themes.")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                    for theme in Theme::ALL {
                        let tc = theme.colors();
                        let active = self.theme == theme;
                        ui.vertical(|ui| {
                            ui.set_min_width(70.0);
                            let (rect, resp) =
                                ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
                            let center = rect.center();
                            ui.painter().circle_filled(center, 12.0, rgb(tc.bg_base));
                            if theme.is_glass() {
                                if let Some((tl, br)) = theme.shell_glows() {
                                    ui.painter().circle_filled(
                                        egui::pos2(center.x - 3.5, center.y - 3.0),
                                        5.0,
                                        rgba(tl, 210),
                                    );
                                    ui.painter().circle_filled(
                                        egui::pos2(center.x + 3.5, center.y + 3.0),
                                        5.0,
                                        rgba(br, 200),
                                    );
                                }
                                ui.painter().circle_stroke(
                                    center,
                                    12.0,
                                    Stroke::new(1.4, Color32::from_rgba_unmultiplied(220, 230, 245, 170)),
                                );
                            }
                            ui.painter().circle_filled(center, 4.5, rgb(tc.green));
                            ui.painter().circle_stroke(
                                center,
                                12.0,
                                if active {
                                    Stroke::new(2.0, rgb(c.green))
                                } else if !theme.is_glass() {
                                    Stroke::new(1.0, rgb(c.border))
                                } else {
                                    Stroke::NONE
                                },
                            );
                            if resp
                                .on_hover_text(theme.label())
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                self.theme = theme;
                                save_theme(self.theme);
                                apply_theme(ui.ctx(), self.theme);
                            }
                            ui.label(
                                RichText::new(theme.label())
                                    .size(10.0)
                                    .color(if active { rgb(c.text) } else { rgb(c.subtext) }),
                            );
                        });
                    }
                });
            });
        });

        self.render_custom_colors_settings(ui, c);
    }

    fn render_settings_privacy(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        let mut dirty = false;
        settings_section(ui, c, "Protection");
        settings_card(ui, c, |ui| {
            dirty |= settings_toggle_row(
                ui,
                c,
                FooterIcon::Lock,
                &mut self.privacy_config.enabled,
                "Privacy protection",
                "Detect secrets and skip excluded apps",
            );
        });

        settings_section(ui, c, "Detect");
        settings_card(ui, c, |ui| {
            ui.add_enabled_ui(self.privacy_config.enabled, |ui| {
                dirty |= settings_toggle_row(
                    ui,
                    c,
                    FooterIcon::Key,
                    &mut self.privacy_config.detect_api_keys,
                    "API keys",
                    "Flag copied tokens and credentials that look like keys",
                );
                settings_card_divider(ui, c);
                dirty |= settings_toggle_row(
                    ui,
                    c,
                    FooterIcon::Lock,
                    &mut self.privacy_config.detect_credentials,
                    "Passwords & secrets",
                    "Catch password-like strings before they land in history",
                );
                settings_card_divider(ui, c);
                dirty |= settings_toggle_row(
                    ui,
                    c,
                    FooterIcon::Shield,
                    &mut self.privacy_config.detect_credit_cards,
                    "Credit cards",
                    "Skip PAN-looking numbers",
                );
                settings_card_divider(ui, c);
                dirty |= settings_toggle_row(
                    ui,
                    c,
                    FooterIcon::Shield,
                    &mut self.privacy_config.detect_ssn,
                    "SSNs",
                    "Skip social-security-number patterns",
                );
            });
        });

        settings_section(ui, c, "Excluded apps");
        settings_card(ui, c, |ui| {
            let mut remove_app: Option<usize> = None;
            let apps = self.privacy_config.excluded_apps.clone();
            if apps.is_empty() {
                settings_card_copy(
                    ui,
                    c,
                    "None yet",
                    "Never save copies from these apps.",
                );
            } else {
                for (i, app_name) in apps.iter().enumerate() {
                    if i > 0 {
                        settings_card_divider(ui, c);
                    }
                    settings_value_row(
                        ui,
                        c,
                        FooterIcon::App,
                        app_name,
                        "Copies from this app are not saved",
                        80.0,
                        |ui| {
                            if ui.small_button("Remove").clicked() {
                                remove_app = Some(i);
                            }
                        },
                    );
                }
            }
            if let Some(i) = remove_app {
                self.privacy_config.excluded_apps.remove(i);
                dirty = true;
            }
            settings_card_divider(ui, c);
            settings_card_body(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_excluded_app)
                            .hint_text("App name…")
                            .desired_width(220.0),
                    );
                    if ui.button("Add").clicked() && !self.new_excluded_app.trim().is_empty() {
                        self.privacy_config
                            .excluded_apps
                            .push(self.new_excluded_app.trim().to_string());
                        self.new_excluded_app.clear();
                        dirty = true;
                    }
                });
            });
        });

        if dirty {
            save_privacy_config(&self.privacy_config);
        }
    }
}

impl ClipdGui {
    #[allow(dead_code)]
    fn render_sessions_window(&mut self, ctx: &egui::Context, c: &clipd_core::ThemeColors) {
        let mut open = true;
        egui::Window::new("📂 Sessions")
            .id(egui::Id::new("sessions_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([440.0, 500.0])
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::new(1.0, rgb(c.border)))
                    .rounding(Rounding::same(12.0)),
            )
            .show(ctx, |ui| {
                if self.sessions.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label(RichText::new("📭").size(40.0));
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("No sessions found")
                                .size(14.0)
                                .color(rgb(c.subtext)),
                        );
                    });
                    return;
                }

                ui.label(
                    RichText::new(format!("{} sessions", self.sessions.len()))
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(6.0);

                egui::ScrollArea::vertical()
                    .id_salt("sessions_window_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut filter_session: Option<usize> = None;
                        let session_color = Color32::from_rgb(100, 200, 160);

                        for (i, session) in self.sessions.iter().enumerate() {
                            let dur = session.duration_mins();
                            let dur_str = if dur < 1 {
                                "instant".into()
                            } else if dur < 60 {
                                format!("{} min", dur)
                            } else {
                                let h = dur / 60;
                                let m = dur % 60;
                                if m == 0 {
                                    format!("{}h", h)
                                } else {
                                    format!("{}h {}m", h, m)
                                }
                            };

                            egui::Frame::none()
                                .fill(surf(c, c.bg_surface))
                                .rounding(Rounding::same(10.0))
                                .inner_margin(Margin::symmetric(12.0, 10.0))
                                .stroke(Stroke::new(1.0, session_color))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("📂").size(14.0));
                                        ui.label(
                                            RichText::new(&session.name)
                                                .size(13.0)
                                                .strong()
                                                .color(rgb(c.text)),
                                        );
                                    });
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;

                                        let meta_pill = |ui: &mut egui::Ui, text: &str| {
                                            egui::Frame::none()
                                                .fill(surf(c, c.bg_elevated))
                                                .rounding(Rounding::same(4.0))
                                                .inner_margin(Margin::symmetric(5.0, 1.0))
                                                .stroke(Stroke::new(0.5, rgb(c.border)))
                                                .show(ui, |ui| {
                                                    ui.label(
                                                        RichText::new(text)
                                                            .size(10.5)
                                                            .color(rgb(c.text)),
                                                    );
                                                });
                                        };

                                        let n = session.clip_count();
                                        meta_pill(
                                            ui,
                                            &format!(
                                                "{} {}",
                                                n,
                                                if n == 1 { "clip" } else { "clips" }
                                            ),
                                        );
                                        meta_pill(ui, &dur_str);
                                        if !session.top_apps.is_empty() {
                                            meta_pill(ui, &session.top_apps.join(", "));
                                        }

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("View >>")
                                                                .size(11.0)
                                                                .strong()
                                                                .color(session_color),
                                                        )
                                                        .fill(pill_bg(session_color))
                                                        .stroke(Stroke::new(1.0, session_color))
                                                        .rounding(Rounding::same(6.0)),
                                                    )
                                                    .clicked()
                                                {
                                                    filter_session = Some(i);
                                                }
                                            },
                                        );
                                    });
                                });
                            ui.add_space(4.0);
                        }

                        if let Some(idx) = filter_session {
                            let session_ids: std::collections::HashSet<i64> =
                                self.sessions[idx].clip_ids.iter().copied().collect();
                            self.search_query.clear();
                            self.filtered = self
                                .clips
                                .iter()
                                .enumerate()
                                .filter(|(_, c)| session_ids.contains(&c.id))
                                .map(|(i, _)| i)
                                .collect();
                            self.selected = 0;
                            self.scroll_to_selected = true;
                            self.active_tab = MainTab::Text;
                        }
                    });
            });

        if !open {
            let _ = ();
        }
    }

    #[allow(dead_code)]
    fn render_settings_window(&mut self, ctx: &egui::Context, c: &clipd_core::ThemeColors) {
        let mut open = true;
        egui::Window::new("🔒 Privacy Settings")
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([440.0, 560.0])
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::new(1.0, rgb(c.border)))
                    .rounding(Rounding::same(12.0)),
            )
            .show(ctx, |ui| {
                let mut dirty = false;

                // ── Master toggle ──
                ui.add_space(4.0);
                if ui
                    .checkbox(
                        &mut self.privacy_config.enabled,
                        "Enable Privacy Protection",
                    )
                    .changed()
                {
                    dirty = true;
                }

                ui.add_space(8.0);
                ui.separator();

                // ── Detection toggles ──
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Detection Rules")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.accent)),
                );
                ui.add_space(4.0);

                ui.add_enabled_ui(self.privacy_config.enabled, |ui| {
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_api_keys,
                            "API Keys (OpenAI, AWS, GitHub, Stripe, Slack…)",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_credentials,
                            "Passwords, Secrets & Database URLs",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_credit_cards,
                            "Credit Card Numbers (Luhn validated)",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_ssn,
                            "Social Security Numbers (SSN)",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                });

                ui.add_space(8.0);
                ui.separator();

                // ── Excluded apps ──
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Excluded Apps")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.accent)),
                );
                ui.label(
                    RichText::new("Copies from these apps are never saved to history")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(4.0);

                let mut remove_app: Option<usize> = None;
                for (i, app_name) in self.privacy_config.excluded_apps.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("  • {}", app_name))
                                .size(12.0)
                                .color(rgb(c.text)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("✕").size(11.0).color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(180, 60, 60))
                                    .rounding(Rounding::same(4.0)),
                                )
                                .clicked()
                            {
                                remove_app = Some(i);
                            }
                        });
                    });
                }
                if let Some(idx) = remove_app {
                    self.privacy_config.excluded_apps.remove(idx);
                    dirty = true;
                }

                ui.horizontal(|ui| {
                    let resp = ui.add_sized(
                        [ui.available_width() - 60.0, 24.0],
                        egui::TextEdit::singleline(&mut self.new_excluded_app)
                            .hint_text("App name…")
                            .font(egui::TextStyle::Small),
                    );
                    if ui
                        .add(
                            egui::Button::new(RichText::new("+ Add").size(11.0))
                                .rounding(Rounding::same(4.0)),
                        )
                        .clicked()
                        || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        let name = self.new_excluded_app.trim().to_string();
                        if !name.is_empty() {
                            self.privacy_config.excluded_apps.push(name);
                            self.new_excluded_app.clear();
                            dirty = true;
                        }
                    }
                });

                ui.add_space(8.0);
                ui.separator();

                // ── Custom skip patterns ──
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Custom Skip Patterns")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.accent)),
                );
                ui.label(
                    RichText::new("Clips containing these strings are never saved")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(4.0);

                let mut remove_pat: Option<usize> = None;
                for (i, pattern) in self.privacy_config.custom_skip_patterns.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("  • {}", pattern))
                                .size(12.0)
                                .color(rgb(c.text)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("✕").size(11.0).color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(180, 60, 60))
                                    .rounding(Rounding::same(4.0)),
                                )
                                .clicked()
                            {
                                remove_pat = Some(i);
                            }
                        });
                    });
                }
                if let Some(idx) = remove_pat {
                    self.privacy_config.custom_skip_patterns.remove(idx);
                    dirty = true;
                }

                ui.horizontal(|ui| {
                    let resp = ui.add_sized(
                        [ui.available_width() - 60.0, 24.0],
                        egui::TextEdit::singleline(&mut self.new_custom_pattern)
                            .hint_text("Pattern…")
                            .font(egui::TextStyle::Small),
                    );
                    if ui
                        .add(
                            egui::Button::new(RichText::new("+ Add").size(11.0))
                                .rounding(Rounding::same(4.0)),
                        )
                        .clicked()
                        || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        let pat = self.new_custom_pattern.trim().to_string();
                        if !pat.is_empty() {
                            self.privacy_config.custom_skip_patterns.push(pat);
                            self.new_custom_pattern.clear();
                            dirty = true;
                        }
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                // ── Action buttons ──
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("💾 Save Settings")
                                    .size(13.0)
                                    .color(Color32::WHITE),
                            )
                            .fill(rgb(c.green))
                            .rounding(Rounding::same(6.0)),
                        )
                        .clicked()
                    {
                        save_privacy_config(&self.privacy_config);
                    }

                    ui.add_space(8.0);

                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("↺ Reset to Defaults")
                                    .size(12.0)
                                    .color(rgb(c.text)),
                            )
                            .fill(surf(c, c.bg_elevated))
                            .rounding(Rounding::same(6.0)),
                        )
                        .clicked()
                    {
                        self.privacy_config = PrivacyConfig::default();
                        dirty = true;
                    }
                });

                if dirty {
                    save_privacy_config(&self.privacy_config);
                }

                ui.add_space(4.0);
            });

        if !open {}
    }

    fn render_transform_window(&mut self, ctx: &egui::Context, c: &clipd_core::ThemeColors) {
        let mut open = true;
        egui::Window::new("✨ Transform on Paste")
            .id(egui::Id::new("transform_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([500.0, 600.0])
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::same(0.0))
                    .stroke(Stroke::new(1.0, rgb(c.border)))
                    .rounding(Rounding::same(12.0)),
            )
            .show(ctx, |ui| {
                let accent = Color32::from_rgb(255, 160, 50);

                // Onboarding hero (shown until dismissed)
                if !self.paste_settings.onboarding_seen {
                    egui::Frame::none()
                        .fill(pill_bg(accent))
                        .inner_margin(Margin::symmetric(20.0, 16.0))
                        .rounding(Rounding {
                            nw: 12.0,
                            ne: 12.0,
                            sw: 0.0,
                            se: 0.0,
                        })
                        .stroke(Stroke::new(1.0, accent))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());

                            ui.horizontal(|ui| {
                                ui.label(RichText::new("✨").size(28.0));
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new("Transform on Paste")
                                            .size(18.0)
                                            .strong()
                                            .color(accent),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "Like PowerToys Advanced Paste — for macOS",
                                        )
                                        .size(12.0)
                                        .color(Color32::WHITE),
                                    );
                                });
                            });

                            ui.add_space(6.0);

                            let tips = [
                                ("📋 Copy anything", "HTML, code, JSON, messy text"),
                                (
                                    "Ctrl+Shift+V to paste",
                                    "Content is auto-cleaned before it hits your doc",
                                ),
                                (
                                    "🧠 AI-powered",
                                    "Fix grammar, translate, convert code — hands-free",
                                ),
                            ];

                            for (title, desc) in tips {
                                ui.horizontal(|ui| {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(title)
                                            .size(12.5)
                                            .strong()
                                            .color(Color32::WHITE),
                                    );
                                    ui.label(
                                        RichText::new(format!("— {}", desc))
                                            .size(12.0)
                                            .color(Color32::from_rgb(200, 200, 200)),
                                    );
                                });
                            }

                            ui.add_space(8.0);

                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Got it, let's set it up →")
                                            .size(12.0)
                                            .strong()
                                            .color(Color32::WHITE),
                                    )
                                    .fill(accent)
                                    .rounding(Rounding::same(8.0)),
                                )
                                .clicked()
                            {
                                self.paste_settings.onboarding_seen = true;
                                save_paste_transform_settings(&self.paste_settings);
                            }
                        });

                    ui.add_space(4.0);
                }

                // Whole settings body scrolls as one unit.
                egui::ScrollArea::vertical()
                    .id_salt("transform_settings_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {

                // Settings body
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(20.0, 12.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());

                        // Master toggle
                        ui.horizontal(|ui| {
                            let toggle_color = if self.paste_settings.enabled {
                                rgb(c.green)
                            } else {
                                rgb(c.subtext)
                            };

                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(if self.paste_settings.enabled {
                                            "● ON"
                                        } else {
                                            "○ OFF"
                                        })
                                        .size(12.0)
                                        .strong()
                                        .color(if self.paste_settings.enabled {
                                            Color32::WHITE
                                        } else {
                                            rgb(c.subtext)
                                        }),
                                    )
                                    .fill(if self.paste_settings.enabled {
                                        rgb(c.green)
                                    } else {
                                        surf(c, c.bg_elevated)
                                    })
                                    .rounding(Rounding::same(12.0))
                                    .stroke(Stroke::new(
                                        1.0,
                                        if self.paste_settings.enabled {
                                            rgb(c.green)
                                        } else {
                                            rgb(c.border)
                                        },
                                    )),
                                )
                                .clicked()
                            {
                                self.paste_settings.enabled = !self.paste_settings.enabled;
                                save_paste_transform_settings(&self.paste_settings);
                            }

                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("Transform on Paste")
                                    .size(14.0)
                                    .strong()
                                    .color(toggle_color),
                            );

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let hk_col = rgb(c.accent);
                                    egui::Frame::none()
                                        .fill(pill_bg(hk_col))
                                        .rounding(Rounding::same(6.0))
                                        .inner_margin(Margin::symmetric(6.0, 2.0))
                                        .stroke(Stroke::new(0.5, hk_col))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new("Ctrl+Shift+V")
                                                    .size(11.0)
                                                    .strong()
                                                    .color(Color32::WHITE)
                                                    .family(egui::FontFamily::Monospace),
                                            );
                                        });
                                    ui.label(
                                        RichText::new("Hotkey:")
                                            .size(11.0)
                                            .color(rgb(c.subtext)),
                                    );
                                },
                            );
                        });

                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "When enabled, Ctrl+Shift+V pastes with auto-transforms applied. \
                                 Regular Cmd+V still pastes normally.",
                            )
                            .size(11.5)
                            .color(rgb(c.subtext)),
                        );

                        ui.add_space(12.0);

                        // Smart mode toggle
                        egui::Frame::none()
                            .fill(surf(c, c.bg_surface))
                            .rounding(Rounding::same(10.0))
                            .inner_margin(Margin::symmetric(14.0, 10.0))
                            .stroke(Stroke::new(
                                1.0,
                                Color32::from_rgb(180, 140, 255),
                            ))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut self.paste_settings.smart_mode,
                                            "",
                                        )
                                        .changed()
                                    {
                                        save_paste_transform_settings(&self.paste_settings);
                                    }
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new("🧠 Smart Mode")
                                                .size(13.0)
                                                .strong()
                                                .color(Color32::from_rgb(180, 140, 255)),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "Auto-detects content type and picks the best transform. \
                                                 JSON → pretty-print, HTML → markdown, code → format.",
                                            )
                                            .size(11.0)
                                            .color(rgb(c.subtext)),
                                        );
                                    });
                                });
                            });
                    });

                ui.add_space(4.0);

                settings_caption(ui, c, "SLOTS & FEEDBACK", "");
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.hud_enabled,
                    "HUD notifications",
                    "Show a floating overlay when copying/pasting to slots.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.letter_slots_enabled,
                    "Letter slots A-Z",
                    "Adds 26 letter slots: Ctrl+Option+C then A-Z copies to slots 31-56, Ctrl+Option+V then A-Z pastes them.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.copy_multi_tap_restore,
                    "Restore clipboard after multi-tap copy",
                    "After Cmd+C x N (N>1), restore clipboard to slot 1's content. When off, clipboard keeps your original copied content.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }

                // ── Configurable Paste Settings (SPEC-tier1-ai-memory) ──
                settings_caption(ui, c, "CLIPBOARD MEMORY", "");
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.remember_clipboard,
                    "Remember copied items automatically",
                    "Cmd+C stores items in clipd memory so the palette can recall them. Off = system copy only, no history.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }

                settings_caption(ui, c, "MEMORY PALETTE", "");
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.palette_enabled,
                    "Enable memory palette",
                    "Open a searchable palette to recall any copied item by content, source, time, or alias.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(20.0, 6.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Palette shortcut").size(12.0).color(rgb(c.text)),
                            );
                            let prev = self.paste_settings.palette_trigger;
                            egui::ComboBox::from_id_salt("palette_trigger")
                                .selected_text(self.paste_settings.palette_trigger.label())
                                .show_ui(ui, |ui| {
                                    for t in [
                                        PaletteTrigger::CmdShiftV,
                                        PaletteTrigger::CtrlOptSpace,
                                        PaletteTrigger::OptSpace,
                                        PaletteTrigger::Off,
                                    ] {
                                        ui.selectable_value(
                                            &mut self.paste_settings.palette_trigger,
                                            t,
                                            t.label(),
                                        );
                                    }
                                });
                            if self.paste_settings.palette_trigger != prev {
                                save_paste_transform_settings(&self.paste_settings);
                            }
                        });
                        if self.paste_settings.palette_trigger == PaletteTrigger::OptSpace {
                            ui.label(
                                RichText::new("⚠ Option+Space normally inserts a non-breaking space on macOS. Using it as a global shortcut may prevent typing that character while clipd is active.")
                                    .size(10.5)
                                    .color(Color32::from_rgb(230, 170, 60)),
                            );
                        }
                    });
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.palette_aliases_enabled,
                    "Letter aliases in palette",
                    "Secondary: lists your letter slots in the palette as @A rows. Type @a then Enter to paste letter slot A — no chord. Recall a saved letter slot without keyboard shortcuts.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }

                settings_caption(ui, c, "LETTER SLOTS (KEYBOARD)", "");
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.quick_letter_slots_enabled,
                    "Quick letter save (double-tap Cmd+C)",
                    "Double-tap Cmd+C then a letter saves to that letter slot. A single Cmd+C is unaffected, so normal copy isn't hampered.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }

                settings_caption(
                    ui,
                    c,
                    "ADVANCED PASTE SHORTCUTS",
                    "Optional convenience for power users — not needed for normal use.",
                );
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.multi_slot_enabled,
                    "Multi-slot copy/paste (slots 1-9)",
                    "Cmd+C x2 copies to slot 2, Cmd+V x2 pastes it, and so on. Off = Cmd+C/Cmd+V behave normally.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.extended_slots_enabled,
                    "Extended slots 11-30 (Excel/dev)",
                    "Option+C/V multi-tap reaches slots 11-30. Off = Option+C/V type normally.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.direct_letter_shortcuts_enabled,
                    "Direct A-Z paste shortcuts",
                    "Enables the global Ctrl+Option+C/V then A-Z chords. Off = letter aliases still work in the palette, but the keyboard chords do nothing. Requires Letter Slots A-Z enabled above.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }
                if settings_toggle(
                    ui,
                    c,
                    &mut self.paste_settings.batch_drain_enabled,
                    "Batch-drain paste",
                    "Cmd+Option+V pastes collected clips one at a time in order — for filling multiple form fields without the palette.",
                ) {
                    save_paste_transform_settings(&self.paste_settings);
                }

                // Transform selection
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(20.0, 0.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());

                        ui.label(
                            RichText::new("ACTIVE TRANSFORMS")
                                .size(11.0)
                                .strong()
                                .color(rgb(c.text)),
                        );
                        ui.label(
                            RichText::new("Selected transforms are applied when you Ctrl+Shift+V")
                                .size(11.0)
                                .color(rgb(c.subtext)),
                        );
                        ui.add_space(6.0);
                    });

                        egui::Frame::none()
                            .inner_margin(Margin::symmetric(20.0, 0.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());

                                let transforms = self.transforms.clone();

                                let categories: &[(&str, Color32)] = &[
                                    ("FORMAT", Color32::from_rgb(130, 170, 255)),
                                    ("CASE", Color32::from_rgb(100, 200, 160)),
                                    ("AI ✨", Color32::from_rgb(255, 180, 80)),
                                ];

                                for (cat_key, cat_color) in categories {
                                    let cat_transforms: Vec<&TransformKind> = transforms
                                        .iter()
                                        .filter(|t| t.category() == *cat_key)
                                        .collect();

                                    if cat_transforms.is_empty() {
                                        continue;
                                    }

                                    ui.add_space(4.0);

                                    for t in &cat_transforms {
                                        let is_active =
                                            self.paste_settings.active_transforms.contains(t);

                                        let (fill, border_col) = if is_active {
                                            (
                                                pill_bg(*cat_color),
                                                *cat_color,
                                            )
                                        } else {
                                            (surf(c, c.bg_surface), rgb(c.border))
                                        };

                                        egui::Frame::none()
                                            .fill(fill)
                                            .rounding(Rounding::same(8.0))
                                            .inner_margin(Margin::symmetric(12.0, 7.0))
                                            .stroke(Stroke::new(1.0, border_col))
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.horizontal(|ui| {
                                                    let check_text = if is_active {
                                                        "✓"
                                                    } else {
                                                        "○"
                                                    };
                                                    let check_col = if is_active {
                                                        Color32::WHITE
                                                    } else {
                                                        rgb(c.overlay)
                                                    };

                                                    ui.label(
                                                        RichText::new(check_text)
                                                            .size(14.0)
                                                            .color(check_col)
                                                            .strong(),
                                                    );
                                                    ui.add_space(4.0);

                                                    let label_col = if is_active {
                                                        Color32::WHITE
                                                    } else {
                                                        rgb(c.subtext)
                                                    };
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{} {}",
                                                            t.icon(),
                                                            t.label()
                                                        ))
                                                        .size(12.5)
                                                        .color(label_col),
                                                    );

                                                    if t.is_ai() {
                                                        ui.with_layout(
                                                            egui::Layout::right_to_left(
                                                                egui::Align::Center,
                                                            ),
                                                            |ui| {
                                                                let ai_col = Color32::from_rgb(255, 180, 80);
                                                                egui::Frame::none()
                                                                    .fill(pill_bg(ai_col))
                                                                    .rounding(Rounding::same(4.0))
                                                                    .inner_margin(
                                                                        Margin::symmetric(
                                                                            5.0, 1.0,
                                                                        ),
                                                                    )
                                                                    .stroke(Stroke::new(0.5, ai_col))
                                                                    .show(ui, |ui| {
                                                                        ui.label(
                                                                            RichText::new("AI")
                                                                                .size(9.0)
                                                                                .strong()
                                                                                .color(Color32::WHITE),
                                                                        );
                                                                    });
                                                            },
                                                        );
                                                    }
                                                });
                                            });

                                        let last_rect = ui.min_rect();
                                        let resp = ui.interact(
                                            last_rect,
                                            egui::Id::new(("tf_toggle", t.label())),
                                            egui::Sense::click(),
                                        );

                                        if resp.clicked() {
                                            if is_active {
                                                self.paste_settings
                                                    .active_transforms
                                                    .retain(|x| x != *t);
                                            } else {
                                                self.paste_settings
                                                    .active_transforms
                                                    .push((*t).clone());
                                            }
                                            save_paste_transform_settings(&self.paste_settings);
                                        }

                                        ui.add_space(2.0);
                                    }

                                    ui.add_space(4.0);
                                }

                                // Optional AI step on paste (not the slot HUD — separate feature)
                                ui.add_space(8.0);
                                egui::Frame::none()
                                    .fill(surf(c, c.bg_surface))
                                    .rounding(Rounding::same(10.0))
                                    .inner_margin(Margin::symmetric(14.0, 10.0))
                                    .stroke(Stroke::new(1.0, accent))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());

                                        ui.label(
                                            RichText::new("✨ Optional: AI text transform on paste")
                                                .size(12.0)
                                                .strong()
                                                .color(accent),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "If you enter instructions here, clipd sends clipboard text \
                                                 to your configured LLM before pasting (smart paste Ctrl+Shift+V, \
                                                 and transform-on-paste). Leave empty to disable.",
                                            )
                                            .size(11.0)
                                            .color(rgb(c.subtext)),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "Needs an API key — set one under Settings ▸ Ask AI. \
                                                 Not related to the slot HUD.",
                                            )
                                            .size(10.0)
                                            .color(rgb(c.subtext)),
                                        );
                                        ui.add_space(4.0);

                                        egui::Frame::none()
                                            .fill(surf(c, c.bg_elevated))
                                            .rounding(Rounding::same(8.0))
                                            .inner_margin(Margin::symmetric(8.0, 6.0))
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                let resp = ui.add_sized(
                                                    [ui.available_width(), 28.0],
                                                    egui::TextEdit::singleline(
                                                        &mut self.paste_settings.default_ai_prompt,
                                                    )
                                                    .hint_text(
                                                        "e.g. Fix grammar — or leave empty",
                                                    )
                                                    .frame(false)
                                                    .font(egui::TextStyle::Body),
                                                );
                                                if resp.changed() || resp.lost_focus() {
                                                    save_paste_transform_settings(
                                                        &self.paste_settings,
                                                    );
                                                }
                                            });
                                    });

                                ui.add_space(12.0);

                                // ── Export History ──
                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("EXPORT HISTORY")
                                        .size(11.0)
                                        .strong()
                                        .color(rgb(c.text)),
                                );
                                ui.label(
                                    RichText::new(format!("{} clips saved to your Documents folder", self.clips.len()))
                                        .size(11.0)
                                        .color(rgb(c.subtext)),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    let txt_col = Color32::from_rgb(100, 180, 255);
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("📄 Export .txt").size(12.0).color(Color32::WHITE),
                                            )
                                            .fill(pill_bg(txt_col))
                                            .rounding(Rounding::same(6.0))
                                            .stroke(Stroke::new(1.0, txt_col)),
                                        )
                                        .clicked()
                                    {
                                        match self.do_export_text() {
                                            Ok(path) => self.export_status = Some((format!("✓ Saved: {}", path), Instant::now())),
                                            Err(e) => self.export_status = Some((format!("✗ {}", e), Instant::now())),
                                        }
                                    }
                                    ui.add_space(6.0);
                                    let csv_col = Color32::from_rgb(100, 210, 140);
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("📊 Export .csv").size(12.0).color(Color32::WHITE),
                                            )
                                            .fill(pill_bg(csv_col))
                                            .rounding(Rounding::same(6.0))
                                            .stroke(Stroke::new(1.0, csv_col)),
                                        )
                                        .clicked()
                                    {
                                        match self.do_export_csv() {
                                            Ok(path) => self.export_status = Some((format!("✓ Saved: {}", path), Instant::now())),
                                            Err(e) => self.export_status = Some((format!("✗ {}", e), Instant::now())),
                                        }
                                    }
                                });
                                if let Some((msg, t)) = &self.export_status {
                                    if t.elapsed() < Duration::from_secs(6) {
                                        ui.add_space(4.0);
                                        let col = if msg.starts_with('✗') {
                                            Color32::from_rgb(255, 100, 100)
                                        } else {
                                            Color32::from_rgb(100, 210, 140)
                                        };
                                        ui.label(RichText::new(msg).size(11.0).color(col));
                                    } else {
                                        self.export_status = None;
                                    }
                                }

                                ui.add_space(12.0);

                                // ── Danger zone: clear all history ──
                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("DANGER ZONE")
                                        .size(11.0)
                                        .strong()
                                        .color(Color32::from_rgb(200, 60, 60)),
                                );
                                ui.add_space(4.0);

                                if self.confirm_clear_all {
                                    ui.label(
                                        RichText::new("Delete all clipboard history? This cannot be undone.")
                                            .size(12.0)
                                            .color(Color32::from_rgb(255, 100, 100)),
                                    );
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("✕ Yes, delete everything")
                                                        .size(12.0)
                                                        .strong()
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(Color32::from_rgb(180, 40, 40))
                                                .rounding(Rounding::same(6.0)),
                                            )
                                            .clicked()
                                        {
                                            let _ = self.store.clear_all();
                                            self.confirm_clear_all = false;
                                            self.refresh();
                                        }
                                        ui.add_space(8.0);
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("Cancel").size(12.0).color(rgb(c.text)),
                                                )
                                                .fill(surf(c, c.bg_elevated))
                                                .rounding(Rounding::same(6.0)),
                                            )
                                            .clicked()
                                        {
                                            self.confirm_clear_all = false;
                                        }
                                    });
                                } else if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("🗑 Clear All History")
                                                .size(12.0)
                                                .color(Color32::from_rgb(255, 100, 100)),
                                        )
                                        .fill(surf(c, c.bg_elevated))
                                        .rounding(Rounding::same(6.0))
                                        .stroke(Stroke::new(1.0, Color32::from_rgb(180, 40, 40))),
                                    )
                                    .clicked()
                                {
                                    self.confirm_clear_all = true;
                                }

                                ui.add_space(8.0);
                            });
                    });
            });

        if !open {
            self.show_transforms = false;
        }
    }
}

impl ClipdGui {
    /// Inline panel: your clips grouped under each collection, in the main view.
    fn render_collections_panel(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        let collections = self.collections.clone();
        let query = self.search_query.trim().to_lowercase();

        let mut pinned_collection_id = None;
        let mut pinned_items = Vec::new();
        let mut other_collections = Vec::new();

        for coll in &collections {
            let items = self.store.collection_items(coll.id).unwrap_or_default();
            if is_pinned_collection_name(&coll.name) {
                pinned_collection_id = Some(coll.id);
                pinned_items.extend(items);
            } else {
                other_collections.push((coll.clone(), items));
            }
        }

        egui::ScrollArea::vertical()
            .id_salt("collections_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                let visible_pins: Vec<_> = pinned_items
                    .iter()
                    .filter(|it| query.is_empty() || collection_item_matches(it, &query))
                    .cloned()
                    .collect();
                self.render_pin_shelf(
                    ui,
                    pinned_collection_id,
                    &pinned_items,
                    &visible_pins,
                    &query,
                    c,
                );

                ui.add_space(6.0);

                egui::CollapsingHeader::new(
                    RichText::new("Other collections")
                        .size(12.0)
                        .color(rgb(c.subtext)),
                )
                .default_open(!query.is_empty())
                .show(ui, |ui| {
                    self.render_secondary_collections(ui, &other_collections, &query, c);
                    ui.add_space(8.0);
                    self.render_new_collection_form(ui, c);
                });

                if !query.is_empty()
                    && visible_pins.is_empty()
                    && !other_collections.iter().any(|(coll, items)| {
                        collection_matches_query(coll, items, &query)
                            || items.iter().any(|it| collection_item_matches(it, &query))
                    })
                {
                    egui::Frame::none()
                        .inner_margin(Margin::symmetric(14.0, 8.0))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("No collections match this search.")
                                    .size(12.0)
                                    .color(rgb(c.subtext)),
                            );
                        });
                }

                // ── AI result ──
                if let Some(result) = self.ai_result.clone() {
                    egui::Frame::none()
                        .inner_margin(Margin::symmetric(14.0, 6.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            egui::Frame::none()
                                .fill(surf(c, c.bg_elevated))
                                .rounding(Rounding::same(CARD_ROUND))
                                .inner_margin(Margin::symmetric(12.0, 10.0))
                                .stroke(Stroke::new(0.7, rgb(c.accent).gamma_multiply(0.4)))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("AI result")
                                                .strong()
                                                .size(12.5)
                                                .color(rgb(c.accent)),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if pill_button(ui, "Dismiss", c).clicked() {
                                                    self.ai_result = None;
                                                }
                                                if pill_button(ui, "Copy", c).clicked() {
                                                    if let Ok(mut cb) = Clipboard::new() {
                                                        let _ = cb.set_text(&result);
                                                    }
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(7.0);
                                    ui.label(RichText::new(&result).size(12.5).color(rgb(c.text)));
                                });
                        });
                }
            });
    }

    fn render_pin_shelf(
        &mut self,
        ui: &mut egui::Ui,
        collection_id: Option<i64>,
        all_items: &[clipd_core::CollectionItem],
        visible_items: &[clipd_core::CollectionItem],
        query: &str,
        c: &clipd_core::ThemeColors,
    ) {
        egui::Frame::none()
            .inner_margin(Margin::symmetric(14.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Pinned")
                            .strong()
                            .size(16.0)
                            .color(rgb(c.text)),
                    );
                    let meta = if query.is_empty() {
                        format!("{} saved", all_items.len())
                    } else {
                        format!("{} of {} saved", visible_items.len(), all_items.len())
                    };
                    ui.label(RichText::new(meta).size(11.5).color(rgb(c.subtext)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("search filters pins")
                                .size(10.5)
                                .color(rgb(c.overlay)),
                        );
                    });
                });
            });

        if collection_id.is_none() || visible_items.is_empty() {
            egui::Frame::none()
                .inner_margin(Margin::symmetric(14.0, 4.0))
                .show(ui, |ui| {
                    egui::Frame::none()
                        .fill(rgba(c.bg_elevated, 88))
                        .rounding(Rounding::same(12.0))
                        .inner_margin(Margin::symmetric(14.0, 12.0))
                        .stroke(Stroke::new(0.5, rgb(c.border).gamma_multiply(0.45)))
                        .show(ui, |ui| {
                            let text = if query.is_empty() {
                                "Pin important clips from the Text tab. They will appear here grouped by type."
                            } else {
                                "No pinned clips match this search."
                            };
                            ui.label(RichText::new(text).size(12.0).color(rgb(c.subtext)));
                        });
                });
            return;
        }

        let collection_id = collection_id.unwrap();
        for group in 0..PIN_GROUP_COUNT {
            let group_items: Vec<_> = visible_items
                .iter()
                .filter(|it| pin_group_index(&it.content) == group)
                .collect();
            if group_items.is_empty() {
                continue;
            }

            egui::Frame::none()
                .inner_margin(Margin {
                    left: 14.0,
                    right: 14.0,
                    top: 8.0,
                    bottom: 2.0,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(pin_group_label(group))
                                .size(11.0)
                                .strong()
                                .color(rgb(c.accent)),
                        );
                        ui.label(
                            RichText::new(format!("{}", group_items.len()))
                                .size(10.5)
                                .color(rgb(c.overlay)),
                        );
                    });
                });

            for item in group_items {
                self.render_collection_item_row(ui, collection_id, item, true, c);
            }
        }
    }

    fn render_secondary_collections(
        &mut self,
        ui: &mut egui::Ui,
        collections: &[(clipd_core::Collection, Vec<clipd_core::CollectionItem>)],
        query: &str,
        c: &clipd_core::ThemeColors,
    ) {
        if collections.is_empty() {
            ui.label(
                RichText::new("No extra collections.")
                    .size(11.5)
                    .color(rgb(c.overlay)),
            );
            return;
        }

        let mut rendered = false;
        for (coll, items) in collections {
            let collection_match = collection_matches_query(coll, items, query);
            let visible_items: Vec<_> = if query.is_empty() || collection_match {
                items.clone()
            } else {
                items
                    .iter()
                    .filter(|it| collection_item_matches(it, query))
                    .cloned()
                    .collect()
            };
            if !query.is_empty() && visible_items.is_empty() && !collection_match {
                continue;
            }
            if query.is_empty() && items.is_empty() {
                continue;
            }
            rendered = true;

            egui::Frame::none()
                .inner_margin(Margin {
                    left: 10.0,
                    right: 10.0,
                    top: 6.0,
                    bottom: 2.0,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&coll.name)
                                .strong()
                                .size(12.5)
                                .color(rgb(c.text)),
                        );
                        let unit = if items.len() == 1 { "item" } else { "items" };
                        let meta = if let Some(app) = &coll.source_app {
                            format!("{} {} · from {}", items.len(), unit, app)
                        } else {
                            format!("{} {}", items.len(), unit)
                        };
                        ui.label(RichText::new(meta).size(10.5).color(rgb(c.overlay)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Delete").size(10.5))
                                        .fill(Color32::TRANSPARENT)
                                        .stroke(Stroke::NONE),
                                )
                                .clicked()
                            {
                                let _ = self.store.delete_collection(coll.id);
                                self.refresh_collections();
                            }
                            if !items.is_empty() {
                                ui.menu_button(
                                    RichText::new("AI").size(10.5).color(rgb(c.subtext)),
                                    |ui| {
                                        if ui.button("Summarize collection").clicked() {
                                            let cfg = load_transform_config();
                                            self.ai_result = Some(
                                                match clipd_core::summarize_collection(items, &cfg)
                                                {
                                                    Ok(s) => s,
                                                    Err(e) => format!("⚠ {}", e),
                                                },
                                            );
                                            ui.close_menu();
                                        }
                                    },
                                );
                            }
                        });
                    });
                });

            if visible_items.is_empty() {
                ui.label(
                    RichText::new("Empty.")
                        .size(11.0)
                        .italics()
                        .color(rgb(c.overlay)),
                );
            } else {
                for item in &visible_items {
                    self.render_collection_item_row(ui, coll.id, item, false, c);
                }
            }
        }

        if !rendered {
            ui.label(
                RichText::new(if query.is_empty() {
                    "Only pinned clips are active right now."
                } else {
                    "No other collections match."
                })
                .size(11.5)
                .color(rgb(c.overlay)),
            );
        }
    }

    fn render_new_collection_form(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        egui::CollapsingHeader::new(
            RichText::new("+ New collection")
                .size(12.0)
                .color(rgb(c.subtext)),
        )
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.new_collection_name)
                    .hint_text("Collection name")
                    .desired_width(ui.available_width()),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let btn_w = 88.0;
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_collection_app)
                        .hint_text("Auto-collect app")
                        .desired_width((ui.available_width() - btn_w).max(80.0)),
                );
                let create = ui.add_sized(
                    [ui.available_width(), 26.0],
                    egui::Button::new(RichText::new("Create").size(12.0).color(rgb(c.bg_base)))
                        .fill(rgb(c.accent))
                        .rounding(Rounding::same(8.0)),
                );
                if create.clicked() && !self.new_collection_name.trim().is_empty() {
                    let app = self.new_collection_app.trim().to_string();
                    let app_opt = if app.is_empty() {
                        None
                    } else {
                        Some(app.as_str())
                    };
                    let _ = self
                        .store
                        .create_collection(self.new_collection_name.trim(), app_opt);
                    self.new_collection_name.clear();
                    self.new_collection_app.clear();
                    self.refresh_collections();
                }
            });
        });
    }

    fn render_collection_item_row(
        &mut self,
        ui: &mut egui::Ui,
        collection_id: i64,
        item: &clipd_core::CollectionItem,
        pinned: bool,
        c: &clipd_core::ThemeColors,
    ) {
        let kind = ContentType::detect(&item.content);
        let type_color = match kind {
            ContentType::Code => rgb(c.code),
            ContentType::Url => rgb(c.url),
            ContentType::Email => rgb(c.email),
            ContentType::Path => rgb(c.path),
            _ => rgb(c.overlay),
        };

        egui::Frame::none()
            .inner_margin(Margin {
                left: 14.0,
                right: 14.0,
                top: 2.0,
                bottom: 2.0,
            })
            .show(ui, |ui| {
                egui::Frame::none()
                    .fill(rgba(c.bg_elevated, 106))
                    .rounding(Rounding::same(10.0))
                    .inner_margin(Margin::symmetric(10.0, 7.0))
                    .stroke(Stroke::new(0.5, rgb(c.border).gamma_multiply(0.42)))
                    .show(ui, |ui| {
                        let row_width = ui.available_width();
                        let row_height = 40.0;
                        let badge_width = 46.0;
                        let action_width = if pinned { 116.0 } else { 126.0 };
                        let gap = 8.0;
                        let text_width =
                            (row_width - badge_width - action_width - (gap * 2.0)).max(120.0);

                        ui.allocate_ui_with_layout(
                            egui::vec2(row_width, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(badge_width, 26.0),
                                    egui::Layout::top_down(egui::Align::Center),
                                    |ui| {
                                        egui::Frame::none()
                                            .fill(pill_bg(type_color).gamma_multiply(0.8))
                                            .rounding(Rounding::same(7.0))
                                            .inner_margin(Margin::symmetric(6.0, 4.0))
                                            .show(ui, |ui| {
                                                ui.label(
                                                    RichText::new(collection_item_icon(&kind))
                                                        .size(12.0)
                                                        .color(type_color),
                                                );
                                            });
                                    },
                                );
                                ui.add_space(gap);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(text_width, row_height),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.y = 1.0;
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(collection_item_title(item))
                                                    .size(12.5)
                                                    .color(rgb(c.text)),
                                            )
                                            .truncate(),
                                        );
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 5.0;
                                            ui.label(
                                                RichText::new(kind.as_str())
                                                    .size(10.0)
                                                    .color(type_color),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "· {}",
                                                    relative_time(&item.added_at)
                                                ))
                                                .size(10.0)
                                                .color(rgb(c.overlay)),
                                            );
                                        });
                                    },
                                );
                                ui.add_space(gap);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(action_width, 28.0),
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        let remove_label = if pinned { "Unpin" } else { "Remove" };
                                        if pill_button(ui, remove_label, c).clicked() {
                                            let _ = self.store.remove_collection_item(
                                                collection_id,
                                                item.clip_id,
                                            );
                                            self.refresh_collections();
                                        }
                                        if pill_button(ui, "Copy", c).clicked() {
                                            if let Ok(mut cb) = Clipboard::new() {
                                                let _ = cb.set_text(&item.content);
                                            }
                                        }
                                    },
                                );
                            },
                        );
                    });
            });
    }
}

/// Keep the GUI's slot badges/filter in sync with the authoritative
/// `active_slots` table — and pull slotted clips into the loaded window even
/// when they fall outside the recent-N history cap (otherwise a slot saved
/// weeks ago silently vanishes from the palette).
/// Replace the preview of any clip that holds a secret.
///
/// Scanning is cached by clip id. `refresh()` runs every three seconds on the
/// UI thread and hands this the whole loaded window — 200 clips — and secret
/// detection is not cheap: it tests 22 key prefixes against every whitespace
/// word of every clip, then looks for credentials, card numbers and SSNs on
/// top. Re-deriving that for clips it had already seen stalled all three
/// windows on a three-second beat.
///
/// A clip's content never changes once stored, so a result keyed by id stays
/// correct for the life of the process.
fn mask_secret_previews(
    clips: &mut [ClipEntry],
    cache: &mut HashMap<i64, Option<String>>,
) -> HashSet<i64> {
    let cfg = clipd_core::load_privacy_config();
    let mut masked = HashSet::new();
    if !cfg.enabled {
        cache.clear();
        return masked;
    }
    // Keep the cache near the size of the window it serves; ids that scrolled
    // out of history are never asked about again.
    if cache.len() > MAX_LOADED_CLIPS * 4 {
        cache.clear();
    }
    for clip in clips.iter_mut() {
        let safe = cache
            .entry(clip.id)
            .or_insert_with(|| clipd_core::redacted_display(&clip.content, &cfg));
        if let Some(safe) = safe {
            clip.preview = safe.clone();
            masked.insert(clip.id);
        }
    }
    masked
}

fn sync_active_slot_labels(store: &ClipStore, clips: &mut Vec<ClipEntry>) {
    let active = store.list_active_slots().unwrap_or_default();
    let active_by_content: std::collections::HashMap<String, u8> = active
        .iter()
        .filter(|(slot, _)| *slot > 0)
        .map(|(slot, content)| (content.clone(), *slot))
        .collect();

    for clip in clips.iter_mut() {
        clip.slot = active_by_content.get(&clip.content).copied();
    }

    // Pull slotted clips that fell outside the recent-N window back into the
    // loaded set so badges / the Slots filter still find them. Insert at the
    // front (by slot number) without reshuffling the rest of history.
    let present: HashSet<&str> = clips.iter().map(|c| c.content.as_str()).collect();
    let mut extras: Vec<ClipEntry> = Vec::new();
    for (slot, content) in &active {
        if *slot == 0 || present.contains(content.as_str()) {
            continue;
        }
        if let Ok(Some(mut clip)) = store.find_by_content(content) {
            clip.slot = Some(*slot);
            extras.push(clip);
        }
    }
    extras.sort_by_key(|c| c.slot.unwrap_or(u8::MAX));
    for (i, clip) in extras.into_iter().enumerate() {
        clips.insert(i, clip);
    }
}

const PIN_GROUP_COUNT: usize = 6;

fn is_pinned_collection_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(PINNED_COLLECTION_NAME)
        || name.eq_ignore_ascii_case(LEGACY_STARRED_COLLECTION_NAME)
}

fn collection_matches_query(
    coll: &clipd_core::Collection,
    items: &[clipd_core::CollectionItem],
    query: &str,
) -> bool {
    query.is_empty()
        || coll.name.to_lowercase().contains(query)
        || coll
            .source_app
            .as_deref()
            .map(|app| app.to_lowercase().contains(query))
            .unwrap_or(false)
        || items.iter().any(|it| collection_item_matches(it, query))
}

fn collection_item_matches(item: &clipd_core::CollectionItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    item.preview.to_lowercase().contains(&q)
        || item.content.to_lowercase().contains(&q)
        || collection_item_title(item).to_lowercase().contains(&q)
        || ContentType::detect(&item.content).as_str().contains(&q)
}

fn pin_group_index(content: &str) -> usize {
    match ContentType::detect(content) {
        ContentType::Url => 0,
        ContentType::Code => 1,
        ContentType::Text => 2,
        ContentType::Path | ContentType::File => 3,
        ContentType::Email => 4,
        ContentType::Image | ContentType::Unknown => 5,
    }
}

fn pin_group_label(group: usize) -> &'static str {
    match group {
        0 => "Links",
        1 => "Code",
        2 => "Text",
        3 => "Files",
        4 => "Emails",
        _ => "Other",
    }
}

fn collection_item_icon(kind: &ContentType) -> &'static str {
    match kind {
        ContentType::Url => "URL",
        ContentType::Code => "{ }",
        ContentType::Email => "@",
        ContentType::Path => "PATH",
        ContentType::Text => "TXT",
        ContentType::Image => "IMG",
        ContentType::File => "FILE",
        ContentType::Unknown => "...",
    }
}

fn collection_item_title(item: &clipd_core::CollectionItem) -> String {
    let content = item.content.trim();
    let title = match ContentType::detect(content) {
        ContentType::Url => compact_url_title(content),
        ContentType::Path => content
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .map(|s| s.to_string()),
        _ => None,
    }
    .unwrap_or_else(|| item.preview.trim().to_string());

    if title.is_empty() {
        "Untitled clip".to_string()
    } else {
        title
    }
}

fn compact_url_title(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let without_www = without_scheme
        .strip_prefix("www.")
        .unwrap_or(without_scheme);
    let without_query = without_www.split(['?', '#']).next().unwrap_or(without_www);
    let mut parts = without_query.split('/').filter(|part| !part.is_empty());
    let host = parts.next()?.trim();
    if host.is_empty() {
        return None;
    }
    let path = parts.next().unwrap_or("").trim();
    if path.is_empty() {
        Some(host.to_string())
    } else {
        Some(format!("{}/{}", host, path))
    }
}

#[allow(clippy::too_many_arguments)]
fn render_preview(
    ui: &mut egui::Ui,
    clip: &ClipEntry,
    slot_no: usize,
    is_starred: bool,
    thumb: Option<egui::TextureHandle>,
    actions: &[CustomAction],
    action_status: Option<(bool, String)>,
    job: &TransformJob,
    can_ai: bool,
    action: &mut Action,
    c: &clipd_core::ThemeColors,
) {
    // ── Header: "PREVIEW · SLOT N" caption + type chip on the right ──
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("PREVIEW · SLOT {}", slot_no))
                .size(10.5)
                .strong()
                .color(rgb(c.subtext)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::Frame::none()
                .fill(surf(c, c.bg_elevated))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(8.0, 3.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(type_label(&clip.content_type))
                            .size(10.0)
                            .color(rgb(c.accent)),
                    );
                });
        });
    });
    ui.add_space(10.0);

    // ── Smart Recommend: what clipd offers to do with THIS clip ──
    //
    // This row is the whole point of the affordance work. The capabilities
    // already existed; without a chip sitting next to the content nobody ever
    // found them.
    let suggestions = clipd_core::suggest_for(clip);
    if !suggestions.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for (idx, s) in suggestions
                .iter()
                .take(clipd_core::VISIBLE_SUGGESTIONS)
                .enumerate()
            {
                // A model-backed chip with no key configured stays visible but
                // disabled — showing what clipd *could* do is the nudge to go
                // set a key, whereas hiding it teaches nothing.
                let usable = !s.needs_ai || can_ai;
                let chip = glass_chip(ui, s.icon, s.label, false, usable && !job.running, c);
                if chip.clicked() {
                    *action = Action::RunSuggestion(idx);
                }
                if usable {
                    chip.on_hover_text(match &s.kind {
                        clipd_core::SuggestionKind::Ask(q) => format!("Ask: {}", q),
                        clipd_core::SuggestionKind::Transform(t) => t.label().to_string(),
                    });
                } else {
                    chip.on_hover_text("Needs an API key — set one in Settings");
                }
            }
        });
        ui.add_space(8.0);
    }

    // ── Result of the last Smart Recommend run ──
    if job.running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("{}…", job.label))
                    .size(11.5)
                    .color(rgb(c.subtext)),
            );
        });
        ui.add_space(8.0);
    } else if let Some(result) = &job.result {
        let (body, ok) = match result {
            Ok(text) => (text.clone(), true),
            Err(e) => (e.clone(), false),
        };
        egui::Frame::none()
            .fill(surf(c, c.bg_elevated))
            .rounding(Rounding::same(CARD_ROUND))
            .inner_margin(Margin::symmetric(10.0, 8.0))
            .stroke(Stroke::new(
                0.7,
                if ok {
                    rgb(c.accent).gamma_multiply(0.4)
                } else {
                    rgb(c.overlay)
                },
            ))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(&job.label)
                        .size(10.0)
                        .strong()
                        .color(rgb(c.subtext)),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("suggestion_result")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(&body).size(12.0).color(rgb(c.text)));
                    });
                if ok {
                    ui.add_space(6.0);
                    if glass_chip(ui, "📋", "Copy result", false, true, c).clicked() {
                        if let Ok(mut cb) = Clipboard::new() {
                            let _ = cb.set_text(body.clone());
                        }
                    }
                }
            });
        ui.add_space(8.0);
    }

    // ── Content box: fixed-height inset card; everything below it (meta,
    // actions, buttons) keeps a stable position at the bottom of the pane. ──
    let enabled: Vec<(usize, &CustomAction)> = actions
        .iter()
        .enumerate()
        .filter(|(_, a)| a.enabled)
        .collect();
    let mut reserved = 148.0; // meta rows + paste/pin buttons + spacing
    if !enabled.is_empty() {
        reserved += 46.0;
    }
    if action_status.is_some() {
        reserved += 18.0;
    }
    let box_h = (ui.available_height() - reserved).max(120.0);

    egui::Frame::none()
        .fill(rgba(c.bg_base, 170))
        .stroke(Stroke::new(0.8, rgb(c.border).gamma_multiply(0.9)))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(box_h);
            egui::ScrollArea::both()
                .id_salt("preview_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());

                    if clip.content_type == ContentType::Image {
                        // Show the image scaled to fit the pane width.
                        if let Some(tex) = &thumb {
                            let avail = ui.available_width();
                            let size = tex.size_vec2();
                            let scale = (avail / size.x).min(1.6);
                            let draw = egui::vec2(size.x * scale, size.y * scale);
                            ui.add(
                                egui::Image::new((tex.id(), draw)).rounding(Rounding::same(8.0)),
                            );
                        } else {
                            ui.label(
                                RichText::new("🖼 Image (preview unavailable)")
                                    .size(13.0)
                                    .color(rgb(c.subtext)),
                            );
                        }
                        let ocr = clip.ocr_text.as_deref().map(str::trim).unwrap_or("");
                        ui.add_space(6.0);
                        if ocr.is_empty() {
                            ui.label(
                                RichText::new("No text recognized.")
                                    .size(11.5)
                                    .italics()
                                    .color(rgb(c.overlay)),
                            );
                        } else {
                            ui.label(
                                RichText::new("Recognized text")
                                    .size(11.0)
                                    .strong()
                                    .color(rgb(c.accent)),
                            );
                        ui.add_space(2.0);
                            ui.label(
                                RichText::new(ocr)
                                    .font(FontId::monospace(12.5))
                                    .color(rgb(c.text))
                                    .line_height(Some(19.0)),
                            );
                        }
                        return;
                    }

                    ui.label(
                        RichText::new(&clip.content)
                            .font(FontId::monospace(12.5))
                            .color(rgb(c.text))
                            .line_height(Some(20.0)),
                    );
                });
        });

    ui.add_space(12.0);

    // ── Meta rows: label left, value right ──
    let meta_row = |ui: &mut egui::Ui, label: &str, value: &str| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(11.0).color(rgb(c.overlay)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(value).size(11.5).color(rgb(c.text)));
            });
        });
        ui.add_space(2.0);
    };
    let src = clip
        .source_app
        .as_deref()
        .unwrap_or("unknown")
        .to_lowercase();
    meta_row(ui, "source", &src);
    if let Some(title) = clip.source_title.as_deref().filter(|t| !t.is_empty()) {
        let t: String = title.chars().take(44).collect();
        meta_row(ui, "window", &t);
    }
    meta_row(ui, "copied", &relative_time(&clip.timestamp));
    meta_row(ui, "size", &format_size(clip.content.len()));

    // ── Custom actions: run a shell command/script on this clip ──
    if !enabled.is_empty() {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for (i, a) in &enabled {
                let btn = ui.add(
                    egui::Button::new(RichText::new(&a.name).size(11.0).color(rgb(c.text)))
                        .fill(surf(c, c.bg_elevated))
                        .rounding(Rounding::same(7.0))
                        .stroke(Stroke::new(0.6, rgb(c.border))),
                );
                if btn.on_hover_text(&a.command).clicked() {
                    *action = Action::RunAction(*i);
                }
            }
        });
    }
    if let Some((ok, msg)) = &action_status {
        ui.add_space(4.0);
        ui.label(RichText::new(msg).size(11.0).color(if *ok {
            rgb(c.green)
        } else {
            rgb(c.accent2)
        }));
    }

    ui.add_space(12.0);

    // ── Primary actions: big accent Paste + quiet Pin/Unpin ──
    ui.horizontal(|ui| {
        let pin_w = 76.0;
        let paste_w = (ui.available_width() - pin_w - ui.spacing().item_spacing.x).max(120.0);
        let paste = ui.add_sized(
            [paste_w, 36.0],
            egui::Button::new(
                RichText::new("Paste  ↩")
                    .size(13.0)
                    .strong()
                    .color(rgb(c.bg_base)),
            )
            .fill(rgb(c.accent))
            .rounding(Rounding::same(10.0))
            .stroke(Stroke::NONE),
        );
        if paste.clicked() {
            *action = Action::Paste;
        }
        let pin_label = if is_starred { "Unpin" } else { "Pin" };
        let pin = ui.add_sized(
            [pin_w, 36.0],
            egui::Button::new(RichText::new(pin_label).size(12.0).color(rgb(c.text)))
                .fill(surf(c, c.bg_elevated))
                .rounding(Rounding::same(10.0))
                .stroke(Stroke::new(0.6, rgb(c.border))),
        );
        if pin.clicked() {
            *action = Action::ToggleStar(clip.id);
        }
    });
}

// ── Ask panel ──

/// One piece of an answer: prose, or a citation that resolves to a real clip.
enum AnswerPart<'a> {
    Text(&'a str),
    Citation(i64),
}

/// Split an answer on `[#id]` so citations can be rendered as buttons rather
/// than as literal text. Ids are already validated by the core engine, so
/// anything still in `[#…]` form here is known to point at a real clip.
fn split_citations(answer: &str) -> Vec<AnswerPart<'_>> {
    let mut parts = Vec::new();
    let mut rest = answer;

    while let Some(start) = rest.find("[#") {
        let after = &rest[start + 2..];
        let digits: String = after.chars().take_while(|ch| ch.is_ascii_digit()).collect();

        if digits.is_empty() || !after[digits.len()..].starts_with(']') {
            // Not a citation after all — keep scanning past this bracket.
            let (head, tail) = rest.split_at(start + 2);
            parts.push(AnswerPart::Text(head));
            rest = tail;
            continue;
        }

        if start > 0 {
            parts.push(AnswerPart::Text(&rest[..start]));
        }
        if let Ok(id) = digits.parse::<i64>() {
            parts.push(AnswerPart::Citation(id));
        }
        rest = &rest[start + 2 + digits.len() + 1..];
    }

    if !rest.is_empty() {
        parts.push(AnswerPart::Text(rest));
    }
    parts
}

fn confidence_color(conf: clipd_core::Confidence, c: &clipd_core::ThemeColors) -> Color32 {
    match conf {
        clipd_core::Confidence::High => rgb(c.accent),
        clipd_core::Confidence::Medium => rgb(c.text),
        clipd_core::Confidence::Low | clipd_core::Confidence::None => rgb(c.subtext),
    }
}

fn render_ask_panel(
    ui: &mut egui::Ui,
    ask: &AskState,
    action: &mut Action,
    c: &clipd_core::ThemeColors,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("ASK")
                .size(10.5)
                .strong()
                .color(rgb(c.accent)),
        );
        if !ask.thread.turns.is_empty() {
            ui.label(
                RichText::new(format!("· {} turns", ask.thread.turns.len()))
                    .size(10.5)
                    .color(rgb(c.subtext)),
            );
        }
    });
    ui.add_space(8.0);

    if ask.running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.add_space(4.0);
            ui.label(
                RichText::new("Searching your clips…")
                    .size(12.5)
                    .color(rgb(c.subtext)),
            );
        });
        return;
    }

    if let Some(err) = &ask.error {
        egui::Frame::none()
            .fill(surf(c, c.bg_elevated))
            .rounding(Rounding::same(CARD_ROUND))
            .inner_margin(Margin::symmetric(12.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(err).size(12.0).color(rgb(c.text)));
            });
        return;
    }

    let Some(answer) = &ask.answer else {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("Ask about anything you've copied")
                    .size(13.0)
                    .color(rgb(c.subtext)),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("? what was that postgres URL")
                    .size(11.5)
                    .italics()
                    .color(rgb(c.overlay)),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("Enter to ask · Esc to leave ask mode")
                    .size(11.0)
                    .color(rgb(c.overlay)),
            );
        });
        return;
    };

    egui::ScrollArea::vertical()
        .id_salt("ask_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(
                RichText::new(&answer.question)
                    .size(12.0)
                    .italics()
                    .color(rgb(c.subtext)),
            );
            ui.add_space(6.0);

            if answer.retrieval_only {
                egui::Frame::none()
                    .fill(surf(c, c.bg_elevated))
                    .rounding(Rounding::same(CARD_ROUND))
                    .inner_margin(Margin::symmetric(12.0, 10.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(
                                "No model configured — these are the clips clipd found, \
                                 ranked. Nothing left your machine.",
                            )
                            .size(11.5)
                            .color(rgb(c.subtext)),
                        );
                        ui.add_space(6.0);
                        // The single most common reason Ask looks broken, so make
                        // the fix reachable from where the user notices it.
                        if ui
                            .button(
                                RichText::new("Set up a model in Settings")
                                    .size(11.5)
                                    .color(rgb(c.accent)),
                            )
                            .on_hover_text("Use an API key, or a local model that needs none")
                            .clicked()
                        {
                            *action = Action::OpenAiSettings;
                        }
                    });
                ui.add_space(6.0);
            } else {
                // Prose with inline, clickable citations.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for part in split_citations(&answer.answer) {
                        match part {
                            AnswerPart::Text(text) => {
                                ui.label(RichText::new(text).size(13.0).color(rgb(c.text)));
                            }
                            AnswerPart::Citation(id) => {
                                let chip = ui.add(
                                    egui::Button::new(
                                        RichText::new(format!("#{}", id))
                                            .size(11.0)
                                            .color(rgb(c.accent)),
                                    )
                                    .fill(surf(c, c.bg_elevated))
                                    .rounding(Rounding::same(PILL_ROUND))
                                    .stroke(Stroke::new(0.7, rgb(c.accent).gamma_multiply(0.5))),
                                );
                                if chip.clicked() {
                                    *action = Action::JumpToClip(id);
                                }
                                chip.on_hover_text("Show this clip");
                            }
                        }
                    }
                });
                ui.add_space(14.0);
            }

            // ── Sources ──
            let heading = if answer.retrieval_only {
                "RETRIEVED"
            } else {
                "SOURCES"
            };
            ui.label(
                RichText::new(heading)
                    .size(10.0)
                    .strong()
                    .color(rgb(c.subtext)),
            );
            ui.add_space(6.0);

            if answer.retrieval_only {
                for r in &answer.retrieved {
                    render_source_row(
                        ui,
                        r.clip.id,
                        &r.clip.preview,
                        r.clip.source_app.as_deref(),
                        &r.matched_by(),
                        r.withheld.as_deref(),
                        action,
                        c,
                    );
                }
            } else if answer.sources.is_empty() {
                ui.label(
                    RichText::new("Nothing cited — treat this answer with suspicion.")
                        .size(11.5)
                        .color(rgb(c.subtext)),
                );
            } else {
                for s in &answer.sources {
                    render_source_row(
                        ui,
                        s.clip_id,
                        &s.preview,
                        s.source_app.as_deref(),
                        &s.matched_by,
                        None,
                        action,
                        c,
                    );
                }
            }

            // ── Footer: confidence and what was held back ──
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);

            if !answer.retrieval_only {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Confidence").size(10.5).color(rgb(c.subtext)));
                    ui.label(
                        RichText::new(answer.confidence.label())
                            .size(11.0)
                            .strong()
                            .color(confidence_color(answer.confidence, c)),
                    );
                })
                .response
                .on_hover_text(
                    "high = cited clips that more than one retriever found independently",
                );
            }

            ui.label(
                RichText::new(format!(
                    "{} clips retrieved · {} cited",
                    answer.retrieved.len(),
                    answer.sources.len()
                ))
                .size(10.5)
                .color(rgb(c.subtext)),
            );

            if answer.withheld_count > 0 {
                ui.label(
                    RichText::new(format!(
                        "🔒 {} clip(s) held back — they contain secrets",
                        answer.withheld_count
                    ))
                    .size(10.5)
                    .color(rgb(c.subtext)),
                )
                .on_hover_text("Clips matching the secret detectors are never sent to the API");
            }

            if !answer.invalid_citations.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "⚠ {} fabricated citation(s) removed",
                        answer.invalid_citations.len()
                    ))
                    .size(10.5)
                    .color(rgb(c.subtext)),
                )
                .on_hover_text("The model cited clip ids that were never in its context");
            }

            ui.add_space(6.0);
        });
}

#[allow(clippy::too_many_arguments)]
fn render_source_row(
    ui: &mut egui::Ui,
    clip_id: i64,
    preview: &str,
    source_app: Option<&str>,
    matched_by: &str,
    withheld: Option<&str>,
    action: &mut Action,
    c: &clipd_core::ThemeColors,
) {
    let row = egui::Frame::none()
        .fill(surf(c, c.bg_elevated))
        .rounding(Rounding::same(PILL_ROUND))
        .inner_margin(Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("#{}", clip_id))
                        .size(10.5)
                        .strong()
                        .color(rgb(c.accent)),
                );
                ui.label(
                    RichText::new(one_line_preview(preview, 40))
                        .size(11.5)
                        .color(rgb(c.text)),
                );
            });
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(source_app.unwrap_or("unknown app"))
                        .size(10.0)
                        .color(rgb(c.subtext)),
                );
                ui.label(RichText::new("·").size(10.0).color(rgb(c.overlay)));
                ui.label(RichText::new(matched_by).size(10.0).color(rgb(c.subtext)));
                if let Some(kind) = withheld {
                    ui.label(
                        RichText::new(format!("· 🔒 {}", kind))
                            .size(10.0)
                            .color(rgb(c.subtext)),
                    );
                }
            });
        });

    let hit = ui.interact(
        row.response.rect,
        egui::Id::new(("ask_source", clip_id)),
        egui::Sense::click(),
    );
    if hit.clicked() {
        *action = Action::JumpToClip(clip_id);
    }
    if hit.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(5.0);
}

/// Flatten a preview to a single line, ellipsised. Clip previews can carry
/// newlines and control characters straight from the source app.
fn one_line_preview(s: &str, max: usize) -> String {
    let flat = s
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!(
            "{}…",
            flat.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

fn render_empty_preview(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
    ui.label(
        RichText::new("PREVIEW")
            .size(10.5)
            .strong()
            .color(rgb(c.subtext)),
    );
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(96.0);
            ui.label(
                RichText::new("Select a clip to preview")
                    .size(13.0)
                    .strong()
                    .color(rgb(c.overlay)),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("Use ↑↓ arrows or click from the list")
                    .size(11.0)
                    .color(rgb(c.overlay)),
            );
        });
    });
}

// ── Helpers ──

fn relative_time(dt: &DateTime<Utc>) -> String {
    let secs = Utc::now().signed_duration_since(*dt).num_seconds();
    if secs < 60 {
        return "just now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{}d ago", days);
    }
    if days < 30 {
        return format!("{}w ago", days / 7);
    }
    dt.format("%b %d").to_string()
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten parsed parts back to a debuggable shape.
    fn parts(answer: &str) -> Vec<String> {
        split_citations(answer)
            .into_iter()
            .map(|p| match p {
                AnswerPart::Text(t) => format!("T:{}", t),
                AnswerPart::Citation(id) => format!("C:{}", id),
            })
            .collect()
    }

    #[test]
    fn ask_query_requires_content_after_the_marker() {
        assert_eq!(ask_query("?what did I copy"), Some("what did I copy"));
        assert_eq!(ask_query("?   spaced  "), Some("spaced"));
        assert_eq!(ask_query("?"), None, "bare ? is not yet a question");
        assert_eq!(ask_query("?   "), None);
        assert_eq!(ask_query("stripe"), None, "plain search is not ask mode");
    }

    #[test]
    fn theme_switch_accepts_curated_and_legacy_names() {
        assert_eq!(theme_named("dark"), Some(Theme::Dark));
        assert_eq!(theme_named("black"), Some(Theme::Dark));
        assert_eq!(theme_named("mac_black"), Some(Theme::Dark));
        assert_eq!(theme_named("midnight"), Some(Theme::Midnight));
        assert_eq!(theme_named("forest"), Some(Theme::Forest));
        assert_eq!(theme_named("cocoa"), Some(Theme::Slate));
        assert_eq!(theme_named("slate"), Some(Theme::Slate));
        assert_eq!(theme_named("paper-light"), Some(Theme::Light));
        assert_eq!(theme_named("paper-dark"), Some(Theme::Dark));
        assert_eq!(theme_named("light-minimal"), Some(Theme::Light));
        // `catppuccin` used to fall back to Dark, from when no such theme
        // existed. It does now, and a config asking for it by name should get
        // the real thing rather than a stand-in.
        assert_eq!(theme_named("catppuccin"), Some(Theme::Catppuccin));
        assert_eq!(theme_named("mocha"), Some(Theme::Catppuccin));
        // The other retired names still land somewhere readable.
        assert_eq!(theme_named("nord"), Some(Theme::Dark));
        assert_eq!(theme_named("dracula"), Some(Theme::Dark));
    }

    #[test]
    fn a_window_opened_at_the_cursor_stays_below_the_island() {
        let screen = Some(egui::vec2(1280.0, 832.0));
        let size = egui::vec2(420.0, 360.0);
        // The gear that opens Settings lives *in* the island, so the pointer
        // is near the top of the screen when the window is placed.
        let at_the_notch = egui::pos2(640.0, 40.0);
        let pos = window_pos_at_cursor(at_the_notch, size, screen);
        if clipd_core::island_layout_active() {
            assert!(
                pos.y >= clipd_core::ISLAND_RESERVED_TOP,
                "opened at {} — inside the island's band",
                pos.y
            );
        }
        // Whatever the layout, the window stays fully on the display.
        assert!(pos.x >= 8.0 && pos.x + size.x <= 1280.0);
        assert!(pos.y + size.y <= 832.0);
    }

    #[test]
    fn letter_slot_help_matches_the_platform_that_ships_it() {
        let rows = letter_slot_bindings();
        assert!(!rows.is_empty(), "the chords are undiscoverable without this");

        let keys: String = rows.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(" | ");
        if cfg!(target_os = "windows") {
            // Windows deliberately binds no direct Win/Ctrl/Alt letter chords —
            // they collide with OS shortcuts, browser menus and AltGr layouts —
            // so A–Z goes through the Ctrl+` leader instead.
            assert!(keys.contains("Ctrl+`"), "Windows routes letters through the leader");
            assert!(
                !keys.contains("Option"),
                "Option is not a Windows modifier: {keys}"
            );
        } else {
            // Both leaders are named, whether on one row or two.
            assert!(keys.contains("Ctrl+Option+C"), "the copy leader");
            assert!(
                keys.contains("Ctrl+Option+V") || keys.contains("Ctrl+Option+C / V"),
                "the paste leader must be named: {keys}"
            );
            // Copy has a double-tap form; paste deliberately does not, because
            // ⌘V ×2 already means slot 2 and a paste cannot be taken back.
            // Letter slots must not borrow the numeric multi-tap keys. A
            // gesture built on ⌘C or ⌘V can always be mistaken for a slot
            // count, whichever way the timing is tuned.
            assert!(
                !keys.contains("\u{2318}C \u{2318}C") && !keys.contains("\u{2318}V \u{2318}V"),
                "letter slots must stay off the numeric keys: {keys}"
            );
            // The gesture must not be built on a key that types something.
            // Option+C emits ç and Option+V emits √, so a missed swallow puts
            // a character in the user's document.
            // Never build a letter gesture on plain Option+letter: Option+C
            // emits ç and Option+V emits √, so a missed swallow types into the
            // user's document. Cmd in the chord suppresses that.
            assert!(
                !keys.contains("Option+C  Option+C") && !keys.contains("Option+V  Option+V"),
                "letter gestures must not use keys that emit characters: {keys}"
            );
            assert!(keys.contains("Cmd+Option+C"), "letter copy leader");
            assert!(keys.contains("Cmd+Option+V"), "letter paste leader");
            // The two-key path leads, because it is the one worth learning.
            assert!(
                rows[0].0.starts_with("\u{2318}C \u{2318}C"),
                "the shortest path should be first, got {}",
                rows[0].0
            );
            assert!(!keys.contains("Ctrl+`"), "that leader is Windows-only");
        }
        // Every row explains itself; a bare chord list teaches nothing.
        assert!(rows.iter().all(|(_, what)| !what.trim().is_empty()));
    }

    #[test]
    fn theme_switch_reports_unknown_names() {
        let args = vec!["clipd-gui".into(), "--set-theme".into(), "neon".into()];
        assert!(requested_theme(&args).expect("theme flag").is_err());
    }

    #[test]
    fn citations_become_their_own_parts() {
        assert_eq!(
            parts("You copied it from [#42] earlier."),
            vec!["T:You copied it from ", "C:42", "T: earlier."]
        );
    }

    #[test]
    fn adjacent_citations_both_parse() {
        assert_eq!(parts("[#1][#2]"), vec!["C:1", "C:2"]);
    }

    #[test]
    fn a_citation_at_the_very_end_is_not_dropped() {
        assert_eq!(parts("see [#7]"), vec!["T:see ", "C:7"]);
    }

    #[test]
    fn plain_prose_is_a_single_part() {
        assert_eq!(parts("no citations here"), vec!["T:no citations here"]);
    }

    #[test]
    fn malformed_brackets_do_not_swallow_the_rest_of_the_answer() {
        // `[#` with no digits must not eat the trailing real citation — the
        // scanner has to advance past it rather than give up.
        assert_eq!(
            parts("array[#] then [#5]"),
            vec!["T:array[#", "T:] then ", "C:5"]
        );
    }

    #[test]
    fn unterminated_citation_stays_as_text() {
        assert_eq!(parts("[#12 no close"), vec!["T:[#", "T:12 no close"]);
    }

    #[test]
    fn empty_answer_yields_no_parts() {
        assert!(split_citations("").is_empty());
    }

    #[test]
    fn previews_are_flattened_to_one_line() {
        assert_eq!(one_line_preview("a\nb\tc", 40), "a b c");
    }

    #[test]
    fn long_previews_are_ellipsised() {
        let out = one_line_preview(&"x".repeat(100), 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }
}
