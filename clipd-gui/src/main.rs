use arboard::Clipboard;
use chrono::{DateTime, Utc};
use clipd_core::{
    available_targets, compute_sessions, detect_sensitive, load_actions, load_custom_colors,
    load_paste_transform_settings, load_privacy_config, load_theme, load_transform_config,
    paste_transforms, run_action, save_actions, save_custom_colors, save_paste_transform_settings,
    save_privacy_config, save_secret, save_theme, ActionOutput, ActionsConfig, ClipEntry,
    ClipStore, ContentType, CustomAction, CustomColors, OpenGuiHotkey, PaletteTrigger,
    PasteTransformSettings, PrivacyConfig, Rgb, SecretEntry, Session, SessionConfig, TfIdfIndex,
    Theme, TransformKind, VaultTarget,
};
use eframe::egui::{self, Color32, FontId, Margin, RichText, Rounding, Stroke};
use std::collections::HashSet;
use std::io::Write;
use std::time::{Duration, Instant};

/// Maximum clips to keep in memory in the GUI. Reduces RAM vs showing all clips.
const MAX_LOADED_CLIPS: usize = 200;

/// Liquid-glass spacing: roomy rows, big rounding, a leading icon tile.
const CARD_ROUND: f32 = 12.0;
const CARD_PAD_X: f32 = 12.0;
const CARD_PAD_Y: f32 = 8.0;
/// Gap between rows in the list.
const ROW_GAP: f32 = 6.0;
/// Pill (tag) corner radius and padding.
const PILL_ROUND: f32 = 6.0;
const PILL_PAD_X: f32 = 7.0;
const PILL_PAD_Y: f32 = 2.0;
const SETTINGS_MAX_WIDTH: f32 = 640.0;
const SETTINGS_GUTTER_X: f32 = 16.0;
const SETTINGS_GUTTER_Y: f32 = 14.0;
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

/// A boxed on/off setting row (checkbox + title + description). Returns true if
/// the value changed this frame, so the caller can persist.
fn settings_toggle(
    ui: &mut egui::Ui,
    c: &clipd_core::ThemeColors,
    value: &mut bool,
    title: &str,
    subtitle: &str,
) -> bool {
    let mut changed = false;
    egui::Frame::none()
        .inner_margin(Margin {
            left: 0.0,
            right: 0.0,
            top: 3.0,
            bottom: 3.0,
        })
        .show(ui, |ui| {
            // A subtle row — checkbox + title on one line, description muted below.
            // Light border instead of a bright outline keeps the list calm.
            egui::Frame::none()
                .fill(rgb(c.bg_surface))
                .rounding(Rounding::same(CARD_ROUND))
                .inner_margin(Margin::symmetric(12.0, 8.0))
                .stroke(Stroke::new(0.5, rgb(c.border)))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        if ui.checkbox(value, "").changed() {
                            changed = true;
                        }
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add(
                                egui::Label::new(
                                    RichText::new(title).strong().size(12.5).color(rgb(c.text)),
                                )
                                .selectable(false),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(subtitle).size(10.5).color(rgb(c.subtext)),
                                )
                                .selectable(false),
                            );
                        });
                    });
                });
        });
    changed
}

/// A section header inside the settings list — a divider, then a small caption.
fn settings_caption(ui: &mut egui::Ui, c: &clipd_core::ThemeColors, text: &str, note: &str) {
    egui::Frame::none()
        .inner_margin(Margin {
            left: 0.0,
            right: 0.0,
            top: 16.0,
            bottom: 6.0,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new(text).size(11.0).strong().color(rgb(c.accent)));
            if !note.is_empty() {
                ui.label(RichText::new(note).size(10.5).color(rgb(c.subtext)));
            }
        });
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

/// Global mouse position in screen points (top-left origin), used to summon
/// the palette at the cursor. macOS-only; other platforms fall back to the
/// window manager's default placement.
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
fn window_pos_at_cursor(cursor: egui::Pos2, win_width: f32) -> egui::Pos2 {
    egui::pos2(
        (cursor.x - win_width * 0.5).max(8.0),
        (cursor.y - 24.0).max(8.0),
    )
}

/// Tiny clipd logo mark: a clipboard outline with its clip tab, vector-drawn
/// so it's crisp at any size and always renders (no font glyphs).
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
            .fill(rgb(c.bg_surface))
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
    let col = if starred {
        rgb(c.accent)
    } else {
        rgb(c.overlay)
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(14.0).color(col))
            .fill(if starred {
                rgb(c.accent).gamma_multiply(0.12)
            } else {
                Color32::TRANSPARENT
            })
            .rounding(Rounding::same(6.0))
            .stroke(Stroke::new(
                if starred { 0.8 } else { 0.0 },
                rgb(c.accent).gamma_multiply(0.7),
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

fn tab_chip(ui: &mut egui::Ui, label: &str, active: bool, c: &clipd_core::ThemeColors) -> bool {
    let text_col = if active {
        rgb(c.accent)
    } else {
        rgb(c.subtext)
    };
    let response = ui.add(
        egui::Button::new(RichText::new(label).size(11.5).color(text_col))
            .fill(if active {
                rgb(c.accent).gamma_multiply(0.14)
            } else {
                Color32::TRANSPARENT
            })
            .rounding(Rounding::same(8.0))
            .stroke(Stroke::NONE)
            .min_size(egui::vec2(0.0, 30.0)),
    );
    response.clicked()
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MainTab {
    Text,
    Collections,
    Settings,
}

// ── Entry point ──

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    // Single instance: if a clipd-gui window is already open, focus it and
    // exit instead of spawning a duplicate. This backstops the daemon's own
    // focus-existing logic against rapid Ctrl+G presses (where a second launch
    // can race before the first window registers).
    if focus_existing_instance() {
        log::info!("clipd-gui already running — focused existing window, exiting");
        return Ok(());
    }

    // Spawn daemon as a child process (rdev's keyboard hook conflicts with
    // eframe's event loop if both run in the same process on macOS).
    let daemon_child = spawn_daemon_process();

    let mut viewport = egui::ViewportBuilder::default()
        // Compact, borderless floating-palette card.
        .with_inner_size([520.0, 560.0])
        .with_min_inner_size([420.0, 340.0])
        .with_decorations(false)
        .with_resizable(true)
        .with_transparent(true);
    // Open where the user is working: palette appears at the mouse cursor.
    // macOS only at startup (CG reports points directly); on Windows the scale
    // factor isn't known until the first frame, where the focus-gain handler
    // repositions to the cursor anyway.
    if cfg!(target_os = "macos") {
        if let Some(cursor) = global_cursor_position() {
            let pos = window_pos_at_cursor(cursor, 520.0);
            viewport = viewport.with_position([pos.x, pos.y]);
        }
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let result = eframe::run_native(
        "clipd",
        options,
        Box::new(|cc| {
            let theme = load_theme();
            apply_theme(&cc.egui_ctx, theme);
            Ok(Box::new(ClipdGui::new(theme)))
        }),
    );

    // GUI closed — kill the daemon subprocess
    if let Some(mut child) = daemon_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    clipd_core::release_daemon_lock();

    result
}

/// Returns true if another clipd-gui process is already running. On macOS it
/// also raises that instance's window to the front before returning.
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

#[cfg(not(target_os = "macos"))]
fn focus_existing_instance() -> bool {
    false
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
    std::process::Command::new(&cli_bin)
        .arg("daemon")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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

fn resolved_theme(ctx: &egui::Context, theme: Theme) -> Theme {
    if theme != Theme::System {
        return theme;
    }
    match ctx.system_theme() {
        Some(egui::Theme::Light) => Theme::Light,
        _ => Theme::Dark,
    }
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        _ => egui::ThemePreference::Dark,
    });

    let effective = resolved_theme(ctx, theme);
    let mut c = effective.colors();
    load_custom_colors().apply_to(&mut c);

    let mut style = (*ctx.style()).clone();
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(18.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(11.5));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.0));
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
    v.panel_fill = rgba(c.bg_base, 198);
    v.window_fill = rgba(c.bg_base, 205);
    v.window_stroke = Stroke::new(1.0, rgb(c.border));
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(60),
    };
    v.window_rounding = Rounding::same(12.0);
    v.extreme_bg_color = rgba(c.bg_base, 198);
    v.faint_bg_color = rgba(c.bg_surface, 190);
    v.widgets.noninteractive.bg_fill = rgba(c.bg_surface, 198);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, rgb(c.text));
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    v.widgets.noninteractive.rounding = Rounding::same(8.0);
    v.widgets.inactive.bg_fill = rgb(c.bg_elevated);
    v.widgets.inactive.rounding = Rounding::same(8.0);
    v.widgets.hovered.bg_fill = rgb(c.bg_hover);
    v.widgets.hovered.rounding = Rounding::same(8.0);
    v.widgets.active.bg_fill = rgb(c.bg_selected);
    v.widgets.active.rounding = Rounding::same(8.0);
    v.selection.bg_fill = Color32::from_rgba_premultiplied(c.accent.0, c.accent.1, c.accent.2, 50);
    v.selection.stroke = Stroke::new(1.0, rgb(c.accent));
    ctx.set_visuals(v);
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

    show_transforms: bool,
    /// On-demand preview pane (Space toggles it). Off = clean single column.
    show_preview: bool,
    /// Tracks window focus so summoning clipd lands the cursor in search.
    was_focused: bool,
    /// Vault (1Password / Bitwarden / Keychain) "save clipboard as a password" form.
    vault_targets: Vec<VaultTarget>,
    vault_selected: Option<VaultTarget>,
    vault_title: String,
    vault_username: String,
    vault_url: String,
    /// (is_success, message) of the last vault save attempt.
    vault_status: Option<(bool, String)>,
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
    new_action_output: ActionOutput,
    /// Last action result banner in the preview pane: (ok, message).
    action_status: Option<(bool, String)>,
    transforms: Vec<TransformKind>,
    paste_settings: PasteTransformSettings,

    cached_tfidf: Option<TfIdfIndex>, // built lazily once per refresh, reused for all searches
    privacy_config: PrivacyConfig,
    sessions: Vec<Session>,
    session_config: SessionConfig,
    active_tab: MainTab,
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
}

impl ClipdGui {
    fn new(theme: Theme) -> Self {
        let db_path = ClipStore::default_path();
        let store = ClipStore::new(&db_path).expect("Failed to open clip database");
        let clips = store.get_recent(MAX_LOADED_CLIPS).unwrap_or_default();
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
            show_transforms: false,
            show_preview: false,
            was_focused: true,
            vault_targets: available_targets(),
            vault_selected: available_targets().first().copied(),
            vault_title: String::new(),
            vault_username: String::new(),
            vault_url: String::new(),
            vault_status: None,
            snippets: Vec::new(),
            matched_snippets: Vec::new(),
            new_snippet_trigger: String::new(),
            new_snippet_name: String::new(),
            new_snippet_body: String::new(),
            custom_actions: load_actions().actions,
            new_action_name: String::new(),
            new_action_command: String::new(),
            new_action_output: ActionOutput::Clipboard,
            action_status: None,
            transforms: paste_transforms(),
            paste_settings: load_paste_transform_settings(),
            cached_tfidf: None,
            privacy_config: load_privacy_config(),
            sessions,
            session_config,
            active_tab: MainTab::Text,
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
        };
        app.refresh_collections();
        app.refresh_starred();
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
    }

    fn refresh(&mut self) {
        self.clips = self.store.get_recent(MAX_LOADED_CLIPS).unwrap_or_default();
        self.sessions = compute_sessions(&self.clips, self.session_config.window_minutes);
        self.cached_tfidf = None; // invalidate — will be rebuilt lazily on next search
        self.refresh_snippets();
        self.apply_filter();
        self.last_refresh = Instant::now();
    }

    fn refresh_snippets(&mut self) {
        self.snippets = self.store.list_snippets().unwrap_or_default();
    }

    fn apply_filter(&mut self) {
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

        // Recall-by-source: "from chrome", "json from chrome", or "app:chrome"
        // filter to clips copied from that app — so you never need a slot number.
        let (content_q, app_q) = split_from_query(&self.search_query.to_lowercase());
        if !app_q.is_empty() {
            base_indices.retain(|&i| {
                self.clips[i]
                    .source_app
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

    fn do_copy(&mut self) -> bool {
        let Some(clip) = self.selected_clip().cloned() else {
            return false;
        };
        // Image clips go to the clipboard as pixels; everything else as text.
        if clip.content_type == ContentType::Image {
            if let Some(path) = clip.image_path.as_deref() {
                return self.set_clipboard_image(path);
            }
            return false;
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

    /// Read the live clipboard and save it as a password to the selected vault.
    /// clipd stores nothing — the secret goes straight to the vault backend.
    fn save_clipboard_to_vault(&mut self) {
        let Some(target) = self.vault_selected else {
            self.vault_status = Some((false, "No vault backend available.".into()));
            return;
        };
        let password = match Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) if !t.trim().is_empty() => t,
            Ok(_) => {
                self.vault_status =
                    Some((false, "Clipboard is empty — copy a password first.".into()));
                return;
            }
            Err(e) => {
                self.vault_status = Some((false, format!("Couldn't read clipboard: {e}")));
                return;
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
            }
            Err(e) => self.vault_status = Some((false, e)),
        }
    }

    fn cycle_theme(&mut self, ctx: &egui::Context) {
        self.theme = self.theme.next();
        save_theme(self.theme);
        apply_theme(ctx, self.theme);
    }
}

// ── Rendering ──

impl eframe::App for ClipdGui {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Fully transparent so the rounded card corners show the desktop behind
        // (the card surface is painted by the panels).
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.theme == Theme::System {
            apply_theme(ctx, self.theme);
        }
        if self.last_refresh.elapsed() > Duration::from_secs(3) {
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_secs(3));

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
                        let w = ctx.input(|i| i.screen_rect().width());
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                            window_pos_at_cursor(cursor, w),
                        ));
                    }
                }
            }
        }
        self.was_focused = focused;

        let mut c = resolved_theme(ctx, self.theme).colors();
        self.custom_colors.apply_to(&mut c);
        let c = c;
        let mut action = Action::None;

        let search_has_focus = ctx.memory(|m| {
            m.focused()
                .map_or(false, |id| id == egui::Id::new("clip_search"))
        });

        let mut should_cycle_theme = false;
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                // Esc = back out one level: Pins/Settings → Clips; Clips → hide.
                // Keyboard navigation must always work even if the mouse is
                // misbehaving (borderless-window quirks on Windows).
                if self.active_tab != MainTab::Text {
                    self.active_tab = MainTab::Text;
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                action = Action::Paste;
            }
            // Space toggles the on-demand preview pane (single column stays clean by default).
            if i.key_pressed(egui::Key::Space) && !search_has_focus {
                self.show_preview = !self.show_preview;
            }
            if i.key_pressed(egui::Key::Delete)
                || (i.key_pressed(egui::Key::D) && i.modifiers.command)
            {
                action = Action::Delete;
            }
            if i.key_pressed(egui::Key::T) && i.modifiers.command {
                should_cycle_theme = true;
            }
            // P pins/unpins the selected clip.
            if i.key_pressed(egui::Key::P) && !search_has_focus {
                if let Some(clip) = self.selected_clip() {
                    action = Action::ToggleStar(clip.id);
                }
            }
        });
        if should_cycle_theme {
            self.cycle_theme(ctx);
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

        // ── Right inspector: on-demand preview (Text tab, toggled with Space) ──
        if self.active_tab == MainTab::Text && self.show_preview {
            egui::SidePanel::right("clip_inspector")
                .resizable(false)
                .exact_width(380.0)
                .frame(
                    egui::Frame::none()
                        .fill(rgba(c.bg_surface, 198))
                        .inner_margin(Margin::symmetric(16.0, 14.0))
                        .stroke(Stroke::new(0.7, rgb(c.border).gamma_multiply(0.65))),
                )
                .show(ctx, |ui| {
                    if let Some(clip) = &preview_data {
                        let is_starred = self.starred_clip_ids.contains(&clip.id);
                        render_preview(
                            ui,
                            clip,
                            is_starred,
                            preview_thumb.clone(),
                            &self.custom_actions,
                            self.action_status.clone(),
                            &mut action,
                            &c,
                        );
                    } else {
                        render_empty_preview(ui, &c);
                    }
                });
        }

        // ── Footer bar — always rendered so the card's bottom corners stay
        // rounded on every tab; gesture hints only show on the Text list. ──
        egui::TopBottomPanel::bottom("footer_hints")
            .frame(
                egui::Frame::none()
                    .fill(rgba(c.bg_surface, 236))
                    .rounding(egui::Rounding {
                        nw: 0.0,
                        ne: 0.0,
                        sw: 18.0,
                        se: 18.0,
                    })
                    .inner_margin(Margin::symmetric(14.0, 8.0)),
            )
            .show(ctx, |ui| {
                // Hints on the left (Text tab only) + a watermark-subtle
                // "clipd" wordmark on the right: identity without chrome.
                ui.horizontal(|ui| {
                    // "cmd" is the Cmd key on macOS; egui maps the same
                    // modifier to Ctrl on Windows/Linux, so label accordingly.
                    let modk = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
                    let hints = if self.active_tab == MainTab::Text {
                        format!("enter  paste     space  preview     {modk} ,  settings")
                    } else {
                        format!("esc  back to clips     {modk} ,  toggle settings")
                    };
                    ui.label(
                        RichText::new(hints)
                            .size(10.0)
                            .color(rgb(c.overlay).gamma_multiply(0.85)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("clipd")
                                .size(10.0)
                                .strong()
                                .color(rgb(c.overlay).gamma_multiply(0.7)),
                        );
                        ui.add_space(1.0);
                        let (mark, _) =
                            ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        draw_clipd_logo(ui.painter(), mark, rgb(c.accent).gamma_multiply(0.9));
                    });
                });
            });

        // ── Center panel — search bar is a fixed header; clip list scrolls below ──
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(rgba(c.bg_surface, 236))
                    .rounding(egui::Rounding {
                        nw: 18.0,
                        ne: 18.0,
                        sw: 0.0,
                        se: 0.0,
                    })
                    .inner_margin(Margin::symmetric(
                        if self.active_tab == MainTab::Text {
                            14.0
                        } else {
                            0.0
                        },
                        10.0,
                    )),
            )
            .show(ctx, |ui| {
                // The header (search + Pins + gear) renders on EVERY tab —
                // without it there is no way back from Pins/Settings.
                // The panel has no horizontal margin off the Text tab, so pad
                // the header itself there to keep it aligned.
                let header_pad = if self.active_tab == MainTab::Text {
                    0.0
                } else {
                    14.0
                };
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(header_pad, 0.0))
                    .show(ui, |ui| {
                        self.render_top_bar(ui, &mut action, &c);
                    });

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
                }
            });

        if self.show_transforms {
            self.render_transform_window(ctx, &c);
        }

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
            Action::ToggleStar(clip_id) => {
                self.toggle_starred(clip_id);
                ctx.request_repaint();
            }
            Action::RunAction(idx) => {
                self.run_custom_action(idx);
                ctx.request_repaint();
            }
            Action::None => {}
        }
    }
}

impl ClipdGui {
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
        // Header labels removed for a cleaner, search-first list (clip count
        // lives in the top bar; the click hint is in the footer).
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
                    clip.thumb_path.clone().map(|p| (clip.id, p))
                } else {
                    None
                }
            })
            .collect();
        for (id, path) in to_load {
            let tex = load_thumb_texture(&ctx, &path);
            self.thumb_textures.insert(id, tex);
        }

        egui::ScrollArea::vertical()
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

                for (display_idx, &clip_idx) in visible_indices.iter().enumerate() {
                    let clip = &self.clips[clip_idx];
                    let clip_id_value = clip.id;
                    let is_selected = display_idx == self.selected;
                    let is_starred = self.starred_clip_ids.contains(&clip_id_value);
                    let mut star_clicked = false;
                    let clip_id = egui::Id::new(("clip", display_idx));
                    let hover_id = egui::Id::new(("cliphover", display_idx));
                    // Was the pointer over this row last frame? Read from a
                    // full-width hover region (see below) so the pin stays
                    // visible while you move onto it. contains_pointer ignores
                    // occlusion by the pin button on top.
                    let row_hovered = ui
                        .ctx()
                        .read_response(hover_id)
                        .map_or(false, |r| r.contains_pointer());

                    let type_color = match clip.content_type {
                        ContentType::Code => rgb(c.code),
                        ContentType::Url => rgb(c.url),
                        ContentType::Email => rgb(c.email),
                        ContentType::Path => rgb(c.path),
                        _ => rgb(c.overlay),
                    };

                    // Selected row reads as a soft accent-tinted glass card.
                    let (bg, border) = if is_selected {
                        (
                            rgb(c.accent).gamma_multiply(0.20),
                            Stroke::new(1.0, rgb(c.accent).gamma_multiply(0.5)),
                        )
                    } else {
                        (Color32::TRANSPARENT, Stroke::NONE)
                    };

                    let preview = clip.preview.trim().replace('\n', " ");
                    let truncated: String = preview.chars().take(200).collect();
                    let suffix = if preview.chars().count() > 200 {
                        "…"
                    } else {
                        ""
                    };
                    let time = relative_time(&clip.timestamp);
                    let is_sensitive =
                        !detect_sensitive(&clip.content, &self.privacy_config).is_empty();
                    // Image clips draw a thumbnail tile in place of the type dot.
                    let thumb_tex = if clip.content_type == ContentType::Image {
                        self.thumb_textures.get(&clip_id_value).cloned().flatten()
                    } else {
                        None
                    };

                    // Gap between rows.
                    ui.add_space(ROW_GAP);

                    // One clean line per clip: a small type dot, the content, and a
                    // muted time on the right. Pin only shows on the active row.
                    let frame_resp = egui::Frame::none()
                        .fill(bg)
                        .rounding(Rounding::same(CARD_ROUND))
                        .stroke(border)
                        .inner_margin(Margin::symmetric(CARD_PAD_X, 9.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;

                                if let Some(tex) = &thumb_tex {
                                    // Thumbnail tile: fit the image into a small
                                    // rounded rect, preserving aspect ratio.
                                    let (tile, _) = ui.allocate_exact_size(
                                        egui::vec2(34.0, 23.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        tile,
                                        Rounding::same(5.0),
                                        rgb(c.bg_elevated),
                                    );
                                    let size = tex.size_vec2();
                                    let scale = (tile.width() / size.x).min(tile.height() / size.y);
                                    let draw = egui::vec2(size.x * scale, size.y * scale);
                                    let img_rect =
                                        egui::Rect::from_center_size(tile.center(), draw);
                                    ui.painter().image(
                                        tex.id(),
                                        img_rect,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        Color32::WHITE,
                                    );
                                    ui.add_space(8.0);
                                } else {
                                    // A single quiet dot — the active row uses the
                                    // accent, everything else a muted neutral so the
                                    // list stays calm instead of a rainbow.
                                    let dot_col = if is_selected {
                                        rgb(c.accent)
                                    } else {
                                        type_color.gamma_multiply(0.55)
                                    };
                                    let (dot, _) = ui.allocate_exact_size(
                                        egui::vec2(8.0, 19.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().circle_filled(
                                        egui::pos2(dot.left() + 3.0, dot.center().y),
                                        2.5,
                                        dot_col,
                                    );
                                    ui.add_space(10.0);
                                }

                                let right_w = 84.0;
                                let content_w = (ui.available_width() - right_w).max(60.0);
                                ui.allocate_ui(egui::vec2(content_w, 19.0), |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!("{}{}", truncated, suffix))
                                                .size(13.0)
                                                .color(if is_selected {
                                                    rgb(c.text)
                                                } else {
                                                    rgb(c.text).gamma_multiply(0.92)
                                                }),
                                        )
                                        .truncate(),
                                    );
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if is_starred || is_selected || row_hovered {
                                            if row_star_button(ui, is_starred, c).clicked() {
                                                star_clicked = true;
                                            }
                                            ui.add_space(4.0);
                                        }
                                        if let Some(slot) = clip.slot {
                                            ui.label(
                                                RichText::new(format!("{}", slot))
                                                    .size(10.5)
                                                    .color(rgb(c.accent)),
                                            );
                                            ui.add_space(6.0);
                                        }
                                        ui.label(
                                            RichText::new(&time).size(10.5).color(rgb(c.overlay)),
                                        );
                                        if is_sensitive {
                                            ui.add_space(6.0);
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

                    // Whole row (minus the pin zone on the right) is clickable.
                    let full = frame_resp.response.rect;
                    let row_rect = egui::Rect::from_min_max(
                        full.min,
                        egui::pos2(full.max.x - 34.0, full.max.y),
                    );
                    let resp = ui.interact(row_rect, clip_id, egui::Sense::click());
                    // Hover is tracked over the *full* row (including the pin
                    // zone) so moving onto the pin doesn't make it vanish.
                    ui.interact(full, hover_id, egui::Sense::hover());

                    // Click a row = pick it: copy, then hide clipd and return to
                    // where you were (Cmd+V pastes it at your cursor). Single-click
                    // honors the "copy on select" setting; double-click always picks.
                    if resp.clicked() && !star_clicked {
                        self.selected = display_idx;
                        if self.paste_settings.copy_on_select {
                            *action = Action::Paste;
                        }
                    }
                    if resp.double_clicked() && !star_clicked {
                        self.selected = display_idx;
                        *action = Action::Paste;
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
                        .fill(rgb(c.bg_surface))
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
                                tag_pill(ui, &dur_str, rgb(c.bg_elevated), c);
                                if !session.top_apps.is_empty() {
                                    tag_pill(
                                        ui,
                                        &session.top_apps.join(", "),
                                        rgb(c.bg_elevated),
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
        let accent = Color32::from_rgb(255, 160, 50);

        settings_caption(
            ui,
            c,
            "PASTE TRANSFORM",
            "Configure smart paste here. Normal Cmd+V remains unchanged.",
        );
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.enabled,
            "Transform on paste",
            "Use Ctrl+Shift+V to apply selected transforms before pasting.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.smart_mode,
            "Smart mode",
            "Auto-detect content type and choose the best transform for JSON, HTML, code, and text.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }

        egui::Frame::none()
            .inner_margin(Margin::symmetric(0.0, 6.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Active transforms")
                            .size(12.0)
                            .strong()
                            .color(rgb(c.text)),
                    );
                    ui.label(
                        RichText::new("applied on Ctrl+Shift+V")
                            .size(10.5)
                            .color(rgb(c.subtext)),
                    );
                });
                ui.add_space(4.0);

                let transforms = self.transforms.clone();
                let categories: &[(&str, Color32)] = &[
                    ("FORMAT", Color32::from_rgb(130, 170, 255)),
                    ("CASE", Color32::from_rgb(100, 200, 160)),
                    ("AI ✨", accent),
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
                    ui.label(
                        RichText::new(*cat_key)
                            .size(9.5)
                            .strong()
                            .color(rgb(c.overlay)),
                    );

                    for t in cat_transforms {
                        let is_active = self.paste_settings.active_transforms.contains(t);
                        let fill = if is_active {
                            pill_bg(*cat_color)
                        } else {
                            rgb(c.bg_surface)
                        };
                        let text_col = if is_active {
                            rgb(c.text)
                        } else {
                            rgb(c.subtext)
                        };

                        let resp = egui::Frame::none()
                            .fill(fill)
                            .rounding(Rounding::same(CARD_ROUND))
                            .inner_margin(Margin::symmetric(CARD_PAD_X, CARD_PAD_Y))
                            .stroke(Stroke::new(
                                0.7,
                                if is_active { *cat_color } else { rgb(c.border) },
                            ))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(if is_active { "✓" } else { "○" })
                                            .size(12.0)
                                            .color(if is_active {
                                                *cat_color
                                            } else {
                                                rgb(c.overlay)
                                            }),
                                    );
                                    ui.label(
                                        RichText::new(format!("{} {}", t.icon(), t.label()))
                                            .size(11.5)
                                            .color(text_col),
                                    );
                                    if t.is_ai() {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    RichText::new("AI")
                                                        .size(9.5)
                                                        .strong()
                                                        .color(accent),
                                                );
                                            },
                                        );
                                    }
                                });
                            })
                            .response;

                        if ui
                            .interact(
                                resp.rect,
                                egui::Id::new(("settings_transform", t.label())),
                                egui::Sense::click(),
                            )
                            .clicked()
                        {
                            if is_active {
                                self.paste_settings.active_transforms.retain(|x| x != t);
                            } else {
                                self.paste_settings.active_transforms.push((*t).clone());
                            }
                            save_paste_transform_settings(&self.paste_settings);
                        }
                        ui.add_space(2.0);
                    }
                }
            });

        egui::Frame::none()
            .inner_margin(Margin::symmetric(0.0, 6.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("AI transform prompt")
                        .size(12.0)
                        .strong()
                        .color(rgb(c.text)),
                );
                ui.label(
                    RichText::new("Optional. Leave empty to keep smart paste fully rule-based.")
                        .size(10.5)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(4.0);
                let resp = ui.add_sized(
                    [ui.available_width(), 26.0],
                    egui::TextEdit::singleline(&mut self.paste_settings.default_ai_prompt)
                        .hint_text("e.g. Fix grammar, convert to table, summarize")
                        .font(egui::TextStyle::Body),
                );
                if resp.changed() || resp.lost_focus() {
                    save_paste_transform_settings(&self.paste_settings);
                }
            });

        settings_caption(
            ui,
            c,
            "CLIPBOARD MEMORY",
            "Control what clipd remembers and how quickly you can recall it.",
        );
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.remember_clipboard,
            "Remember copied items automatically",
            "Cmd+C stores items in clipd memory. Off means system copy only, no history.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.palette_enabled,
            "Enable memory palette",
            "Open a searchable palette to recall copied items by content, source, time, or alias.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        egui::Frame::none()
            .inner_margin(Margin::symmetric(0.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Palette shortcut")
                            .size(12.0)
                            .color(rgb(c.text)),
                    );
                    let prev = self.paste_settings.palette_trigger;
                    egui::ComboBox::from_id_salt("settings_palette_trigger")
                        .selected_text(self.paste_settings.palette_trigger.label())
                        .show_ui(ui, |ui| {
                            for t in [
                                PaletteTrigger::CmdShiftV,
                                PaletteTrigger::CtrlOptSpace,
                                PaletteTrigger::OptSpace,
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
                        RichText::new(
                            "Option+Space normally inserts a non-breaking space on macOS.",
                        )
                        .size(10.5)
                        .color(Color32::from_rgb(230, 170, 60)),
                    );
                }
            });
        egui::Frame::none()
            .inner_margin(Margin::symmetric(0.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Open clipd shortcut")
                            .size(12.0)
                            .color(rgb(c.text)),
                    );
                    let prev = self.paste_settings.open_gui_hotkey;
                    egui::ComboBox::from_id_salt("settings_open_gui_hotkey")
                        .selected_text(self.paste_settings.open_gui_hotkey.label())
                        .show_ui(ui, |ui| {
                            for hk in OpenGuiHotkey::ALL {
                                // Alt+G is Windows-only (Option+G types © on
                                // macOS) — hide it where it can't work.
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
                        save_paste_transform_settings(&self.paste_settings);
                    }
                });
                ui.label(
                    RichText::new(
                        "Global hotkey to summon this window from anywhere. Takes effect immediately.",
                    )
                    .size(10.5)
                    .color(rgb(c.subtext)),
                );
            });
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.palette_aliases_enabled,
            "Letter aliases in palette",
            "Show letter slots as @A rows so you can recall them without memorizing chords.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }

        settings_caption(
            ui,
            c,
            "SHORTCUTS & FEEDBACK",
            "Power-user paste modes. Keep these off if you only want normal copy/paste plus search.",
        );
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.hud_enabled,
            "HUD notifications",
            "Show a floating overlay when copying or pasting to slots.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.copy_on_select,
            "Copy when selecting a row",
            "Single-clicking a history row copies it immediately. Turn off for select-only rows; double-click and Enter still copy.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.return_focus_after_copy,
            "Paste into previous app on select",
            "After you pick a clip, jump back to the app you summoned clipd from (e.g. Cursor) and paste it. Needs Accessibility permission for clipd; otherwise it just returns focus and you press Cmd+V.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.multi_slot_enabled,
            "Multi-slot copy/paste",
            "Cmd+C x2 copies to slot 2, Cmd+V x2 pastes it, and so on.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.extended_slots_enabled,
            "Extended slots 11-30",
            "Option+C/V multi-tap reaches higher-numbered slots.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.letter_slots_enabled,
            "Letter slots A-Z",
            "Adds letter slots for faster named recall.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.quick_letter_slots_enabled,
            "Quick letter save",
            "Double-tap Cmd+C then a letter to save to that letter slot.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.direct_letter_shortcuts_enabled,
            "Direct A-Z shortcuts",
            "Enable global Ctrl+Option+C/V then A-Z chords.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.batch_drain_enabled,
            "Batch-drain paste",
            "Cmd+Option+V pastes collected clips one at a time in order.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
        if settings_toggle(
            ui,
            c,
            &mut self.paste_settings.copy_multi_tap_restore,
            "Restore clipboard after multi-tap copy",
            "After Cmd+C x N, restore the clipboard to slot 1's content.",
        ) {
            save_paste_transform_settings(&self.paste_settings);
        }
    }

    fn render_actions_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_caption(
            ui,
            c,
            "CUSTOM ACTIONS",
            "Run any shell command or script on a clip. The clip is piped in as \
             input; the output can replace your clipboard or become a new clip. \
             Run them from a clip's preview pane (press Space on a clip).",
        );
        egui::Frame::none()
            .inner_margin(Margin::symmetric(0.0, 6.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
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
                            self.custom_actions.push(CustomAction::new(
                                self.new_action_name.trim(),
                                self.new_action_command.trim(),
                                self.new_action_output,
                            ));
                            self.persist_actions();
                            self.new_action_name.clear();
                            self.new_action_command.clear();
                        }
                    });
                });
            });

        if self.custom_actions.is_empty() {
            ui.label(
                RichText::new("No actions yet — add one above.")
                    .size(11.0)
                    .color(rgb(c.subtext)),
            );
            return;
        }

        let mut to_delete: Option<usize> = None;
        let mut changed = false;
        for (i, a) in self.custom_actions.iter_mut().enumerate() {
            egui::Frame::none()
                .inner_margin(Margin::symmetric(0.0, 3.0))
                .show(ui, |ui| {
                    egui::Frame::none()
                        .fill(rgb(c.bg_surface))
                        .rounding(Rounding::same(CARD_ROUND))
                        .inner_margin(Margin::symmetric(12.0, 8.0))
                        .stroke(Stroke::new(0.5, rgb(c.border)))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut a.enabled, "").changed() {
                                    changed = true;
                                }
                                ui.add_space(2.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(&a.name)
                                            .size(12.5)
                                            .strong()
                                            .color(rgb(c.text)),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&a.command)
                                                .font(FontId::monospace(11.0))
                                                .color(rgb(c.subtext)),
                                        )
                                        .truncate(),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(ui, "Delete", c).clicked() {
                                            to_delete = Some(i);
                                        }
                                        ui.add_space(6.0);
                                        ui.label(
                                            RichText::new(a.output.label())
                                                .size(10.0)
                                                .color(rgb(c.overlay)),
                                        );
                                    },
                                );
                            });
                        });
                });
        }
        if let Some(i) = to_delete {
            self.custom_actions.remove(i);
            changed = true;
        }
        if changed {
            self.persist_actions();
        }
    }

    fn render_snippets_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_caption(
            ui,
            c,
            "SNIPPETS",
            "Reusable text. Type its trigger in search, then Enter to paste it.",
        );
        egui::Frame::none()
            .inner_margin(Margin::symmetric(0.0, 6.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
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

        let snippets = self.snippets.clone();
        if snippets.is_empty() {
            ui.label(
                RichText::new("No snippets yet.")
                    .size(11.0)
                    .color(rgb(c.subtext)),
            );
        } else {
            for s in &snippets {
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(0.0, 3.0))
                    .show(ui, |ui| {
                        egui::Frame::none()
                            .fill(rgb(c.bg_surface))
                            .rounding(Rounding::same(CARD_ROUND))
                            .inner_margin(Margin::symmetric(12.0, 8.0))
                            .stroke(Stroke::new(0.5, rgb(c.border)))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    tag_pill(ui, &s.trigger, rgb(c.accent), c);
                                    ui.add_space(6.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(s.preview())
                                                .size(12.0)
                                                .color(rgb(c.subtext)),
                                        )
                                        .truncate(),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if pill_button(ui, "Delete", c).clicked() {
                                                let _ = self.store.delete_snippet(s.id);
                                                self.refresh_snippets();
                                            }
                                        },
                                    );
                                });
                            });
                    });
            }
        }
    }

    fn render_vault_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_caption(
            ui,
            c,
            "PASSWORD VAULT",
            "clipd never stores passwords. Save a copied password straight into a real vault instead.",
        );

        if settings_toggle(
            ui,
            c,
            &mut self.privacy_config.offer_vault_on_secret,
            "Offer to vault detected passwords",
            "When clipd detects a copied password, pop a prompt to save it to a vault.",
        ) {
            save_privacy_config(&self.privacy_config);
        }

        if self.vault_targets.is_empty() {
            ui.label(
                RichText::new(
                    "No vault backend found. Install the 1Password CLI (`op`) or Bitwarden CLI (`bw`). The macOS Keychain is available on macOS.",
                )
                .size(11.5)
                .color(rgb(c.subtext)),
            );
            return;
        }

        egui::Frame::none()
            .inner_margin(Margin::symmetric(0.0, 6.0))
            .show(ui, |ui| {
                // Backend picker (only those usable on this machine).
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
                // Optional metadata — the password itself comes from the clipboard.
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
                            RichText::new("🔐 Save clipboard to vault")
                                .size(12.0)
                                .color(rgb(c.bg_base)),
                        )
                        .fill(rgb(c.accent))
                        .rounding(Rounding::same(6.0)),
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
    }

    /// "Build your own palette" — accent / background / text pickers that
    /// override whatever base theme is active. Saved and applied live.
    fn render_custom_colors_settings(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        settings_caption(
            ui,
            c,
            "CUSTOM COLORS",
            "Build your own palette. When on, these override the theme above.",
        );
        let mut changed = settings_toggle(
            ui,
            c,
            &mut self.custom_colors.enabled,
            "Use custom colors",
            "Pick your own accent, background, and text — surfaces are derived to match.",
        );
        if self.custom_colors.enabled {
            egui::Frame::none()
                .inner_margin(Margin::symmetric(0.0, 4.0))
                .show(ui, |ui| {
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
        if changed {
            save_custom_colors(&self.custom_colors);
            apply_theme(ui.ctx(), self.theme);
        }
    }

    /// Search bar + Pins chip + Settings gear. Rendered on every tab so
    /// navigation is always available (gear and Pins both toggle back).
    fn render_top_bar(&mut self, ui: &mut egui::Ui, action: &mut Action, c: &clipd_core::ThemeColors) {
        ui.horizontal(|ui| {
            let controls_width = 96.0;
            let gap = 8.0;
            let search_width = (ui.available_width() - controls_width - gap).max(120.0);

            ui.allocate_ui_with_layout(
                egui::vec2(search_width, 36.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    egui::Frame::none()
                        .fill(rgb(c.bg_elevated))
                        .rounding(Rounding::same(10.0))
                        .inner_margin(Margin::symmetric(10.0, 6.0))
                        .stroke(Stroke::new(0.6, rgb(c.border).gamma_multiply(0.75)))
                        .show(ui, |ui| {
                            ui.set_width((search_width - 22.0).max(80.0));
                            ui.horizontal(|ui| {
                                draw_search_icon(ui, rgb(c.overlay));
                                ui.add_space(6.0);
                                let hint = match self.active_tab {
                                    MainTab::Collections => "Search pins and collections...",
                                    MainTab::Settings => "Settings",
                                    MainTab::Text => "Search, or type 'from chrome'...",
                                };
                                let search = ui.add_sized(
                                    [ui.available_width(), 18.0],
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
                                    self.apply_filter();
                                }
                                // Enter pastes the top match — Text tab only.
                                if self.active_tab == MainTab::Text
                                    && search.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                    && !self.filtered.is_empty()
                                {
                                    *action = Action::Paste;
                                }
                            });
                        });
                },
            );
            ui.add_space(gap);

            ui.allocate_ui_with_layout(
                egui::vec2(controls_width, 36.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let settings_active = self.active_tab == MainTab::Settings;
                    let gear_col = if settings_active {
                        rgb(c.accent)
                    } else {
                        rgb(c.overlay)
                    };
                    let gear = ui.add(
                        egui::Button::new(RichText::new("⚙").size(15.0).color(gear_col))
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE)
                            .min_size(egui::vec2(26.0, 30.0)),
                    );
                    if gear.clicked() {
                        self.active_tab = if settings_active {
                            MainTab::Text
                        } else {
                            MainTab::Settings
                        };
                    }
                    gear.on_hover_text(if settings_active {
                        "Back to clips"
                    } else {
                        "Settings"
                    });
                    let pins_active = self.active_tab == MainTab::Collections;
                    if tab_chip(ui, "Pins", pins_active, &c) {
                        if pins_active {
                            self.active_tab = MainTab::Text;
                        } else {
                            self.active_tab = MainTab::Collections;
                            self.refresh_collections();
                        }
                    }
                },
            );
        });
    }

    fn render_settings_panel(&mut self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        egui::Frame::none()
            .inner_margin(Margin {
                left: SETTINGS_GUTTER_X,
                right: SETTINGS_GUTTER_X,
                top: SETTINGS_GUTTER_Y,
                bottom: SETTINGS_GUTTER_Y,
            })
            .show(ui, |ui| {
                // Clamp to the *actual* available width (never wider than the
                // window) so the compact window doesn't clip cards on the right.
                let content_w = ui.available_width().min(SETTINGS_MAX_WIDTH);
                ui.set_max_width(content_w);
                ui.label(
                    RichText::new("Settings")
                        .size(18.0)
                        .strong()
                        .color(rgb(c.text)),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Appearance, privacy, and clipboard behavior.")
                        .size(12.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(8.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let content_w = ui.available_width().min(SETTINGS_MAX_WIDTH);
                        ui.set_max_width(content_w);
                        let mut dirty = false;

                        settings_caption(
                            ui,
                            c,
                            "APPEARANCE",
                            "Use System to follow macOS, or choose an explicit Light/Dark theme.",
                        );
                        egui::Frame::none()
                            .inner_margin(Margin::symmetric(0.0, 6.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Theme").size(12.0).color(rgb(c.text)));
                                    let prev = self.theme;
                                    egui::ComboBox::from_id_salt("theme_selector")
                                        .selected_text(self.theme.label())
                                        .show_ui(ui, |ui| {
                                            for theme in Theme::ALL {
                                                ui.selectable_value(
                                                    &mut self.theme,
                                                    theme,
                                                    theme.label(),
                                                );
                                            }
                                        });
                                    if self.theme != prev {
                                        save_theme(self.theme);
                                        apply_theme(ui.ctx(), self.theme);
                                    }
                                });
                                ui.label(
                                    RichText::new(
                                        "Shortcut: Cmd+T cycles System, Light, Dark, and named themes.",
                                    )
                                    .size(10.5)
                                    .color(rgb(c.subtext)),
                                );
                            });

                        self.render_custom_colors_settings(ui, c);

                        self.render_clipboard_behavior_settings(ui, c);

                        self.render_snippets_settings(ui, c);

                        self.render_actions_settings(ui, c);

                        self.render_vault_settings(ui, c);

                        settings_caption(
                            ui,
                            c,
                            "PRIVACY",
                            "Control what clipd saves and how sensitive content is handled.",
                        );
                        if settings_toggle(
                            ui,
                            c,
                            &mut self.privacy_config.enabled,
                            "Enable privacy protection",
                            "Detect secrets and prevent excluded apps from being stored.",
                        ) {
                            dirty = true;
                        }

                        ui.label(
                            RichText::new("Detection rules")
                                .size(13.0)
                                .strong()
                                .color(rgb(c.accent)),
                        );
                        ui.add_space(6.0);

                        ui.add_enabled_ui(self.privacy_config.enabled, |ui| {
                            if ui
                                .checkbox(
                                    &mut self.privacy_config.detect_api_keys,
                                    "API keys (OpenAI, AWS, GitHub, Stripe, Slack…)",
                                )
                                .changed()
                            {
                                dirty = true;
                            }
                            if ui
                                .checkbox(
                                    &mut self.privacy_config.detect_credentials,
                                    "Passwords, secrets & database URLs",
                                )
                                .changed()
                            {
                                dirty = true;
                            }
                            if ui
                                .checkbox(
                                    &mut self.privacy_config.detect_credit_cards,
                                    "Credit card numbers",
                                )
                                .changed()
                            {
                                dirty = true;
                            }
                            if ui
                                .checkbox(
                                    &mut self.privacy_config.detect_ssn,
                                    "Social Security numbers",
                                )
                                .changed()
                            {
                                dirty = true;
                            }
                        });

                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(8.0);

                        ui.label(
                            RichText::new("Excluded apps")
                                .size(13.0)
                                .strong()
                                .color(rgb(c.accent)),
                        );
                        ui.label(
                            RichText::new("Copies from these apps are never saved.")
                                .size(11.0)
                                .color(rgb(c.subtext)),
                        );
                        ui.add_space(6.0);

                        let mut remove_app: Option<usize> = None;
                        for (i, app_name) in self.privacy_config.excluded_apps.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(app_name).size(12.0).color(rgb(c.text)));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Remove").clicked() {
                                            remove_app = Some(i);
                                        }
                                    },
                                );
                            });
                        }
                        if let Some(i) = remove_app {
                            self.privacy_config.excluded_apps.remove(i);
                            dirty = true;
                        }

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.new_excluded_app)
                                    .hint_text("App name…")
                                    .desired_width(200.0),
                            );
                            if ui.button("Add").clicked()
                                && !self.new_excluded_app.trim().is_empty()
                            {
                                self.privacy_config
                                    .excluded_apps
                                    .push(self.new_excluded_app.trim().to_string());
                                self.new_excluded_app.clear();
                                dirty = true;
                            }
                        });

                        if dirty {
                            save_privacy_config(&self.privacy_config);
                        }
                    });
            });
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
                                .fill(rgb(c.bg_surface))
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
                                                .fill(rgb(c.bg_elevated))
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
                            .fill(rgb(c.bg_elevated))
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

                            ui.add_space(10.0);

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
                                        rgb(c.bg_elevated)
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
                            .fill(rgb(c.bg_surface))
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
                                            (rgb(c.bg_surface), rgb(c.border))
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
                                    .fill(rgb(c.bg_surface))
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
                                                "Requires api_key in ~/.local/share/clipd/transform.json \
                                                 (OpenAI-compatible endpoint). Not related to the slot HUD.",
                                            )
                                            .size(10.0)
                                            .color(rgb(c.subtext)),
                                        );
                                        ui.add_space(4.0);

                                        egui::Frame::none()
                                            .fill(rgb(c.bg_elevated))
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
                                                .fill(rgb(c.bg_elevated))
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
                                        .fill(rgb(c.bg_elevated))
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

                ui.add_space(10.0);

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
                                .fill(rgb(c.bg_elevated))
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
        ContentType::Path => 3,
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
    is_starred: bool,
    thumb: Option<egui::TextureHandle>,
    actions: &[CustomAction],
    action_status: Option<(bool, String)>,
    action: &mut Action,
    c: &clipd_core::ThemeColors,
) {
    let type_color = match clip.content_type {
        ContentType::Code => rgb(c.code),
        ContentType::Url => rgb(c.url),
        ContentType::Email => rgb(c.email),
        ContentType::Path => rgb(c.path),
        _ => rgb(c.accent),
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Preview")
                .size(13.0)
                .strong()
                .color(rgb(c.text)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if outline_button(ui, "Copy", rgb(c.text), c).clicked() {
                *action = Action::Copy;
            }
            if star_button(ui, is_starred, c).clicked() {
                *action = Action::ToggleStar(clip.id);
            }
        });
    });

    ui.add_space(5.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(
            RichText::new(clip.content_type.as_str())
                .size(10.5)
                .color(type_color)
                .strong(),
        );
        if let Some(ref app) = clip.source_app {
            ui.label(
                RichText::new(format!("· {}", app))
                    .size(10.5)
                    .color(rgb(c.overlay)),
            );
        }
        ui.label(
            RichText::new(format!("· {}", relative_time(&clip.timestamp)))
                .size(10.5)
                .color(rgb(c.overlay)),
        );
        if let Some(slot) = clip.slot {
            ui.label(
                RichText::new(format!("· slot {}", slot))
                    .size(10.5)
                    .color(rgb(c.accent)),
            );
        }
    });

    // ── Custom actions: run a shell command/script on this clip ──
    let enabled: Vec<(usize, &CustomAction)> = actions
        .iter()
        .enumerate()
        .filter(|(_, a)| a.enabled)
        .collect();
    if !enabled.is_empty() {
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(
            RichText::new("ACTIONS")
                .size(10.0)
                .strong()
                .color(rgb(c.accent)),
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for (i, a) in enabled {
                let btn = ui.add(
                    egui::Button::new(RichText::new(&a.name).size(11.5).color(rgb(c.text)))
                        .fill(rgb(c.bg_elevated))
                        .rounding(Rounding::same(7.0))
                        .stroke(Stroke::new(0.6, rgb(c.border))),
                );
                if btn.on_hover_text(&a.command).clicked() {
                    *action = Action::RunAction(i);
                }
            }
        });
        if let Some((ok, msg)) = &action_status {
            ui.add_space(4.0);
            ui.label(RichText::new(msg).size(11.0).color(if *ok {
                rgb(c.green)
            } else {
                rgb(c.accent2)
            }));
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);

    egui::ScrollArea::both()
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
                    ui.add(egui::Image::new((tex.id(), draw)).rounding(Rounding::same(8.0)));
                } else {
                    ui.label(
                        RichText::new("🖼 Image (preview unavailable)")
                            .size(13.0)
                            .color(rgb(c.subtext)),
                    );
                }
                let ocr = clip.ocr_text.as_deref().map(str::trim).unwrap_or("");
                ui.add_space(10.0);
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
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(ocr)
                            .font(FontId::proportional(13.5))
                            .color(rgb(c.text))
                            .line_height(Some(19.0)),
                    );
                }
                return;
            }

            let font = if clip.content_type == ContentType::Code {
                FontId::monospace(13.5)
            } else {
                FontId::proportional(14.0)
            };
            ui.label(
                RichText::new(&clip.content)
                    .font(font)
                    .color(rgb(c.text))
                    .line_height(Some(20.0)),
            );
        });
}

fn render_empty_preview(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
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
