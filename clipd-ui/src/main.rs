#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use clipd_core::{
    load_paste_transform_settings, save_paste_transform_settings, CtrlSpaceAction, OpenGuiHotkey,
    PaletteTrigger, SecretRef,
};
use clipd_core::{ClipEntry, ClipStore, ContentType};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{
    IconMenuItem, Menu, MenuEvent, MenuItem, NativeIcon, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, TrayIconId};

#[cfg(target_os = "macos")]
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};

const MENU_ID_DAEMON: &str = "daemon_toggle";
const MENU_ID_SEARCH: &str = "search";
const MENU_ID_HUD: &str = "hud_notifications";
const MENU_ID_HUD_VIEW: &str = "hud_view";
const MENU_ID_TUI_MODE: &str = "tui_mode";
const MENU_ID_QUIT: &str = "quit";
const MENU_ID_SETTINGS: &str = "settings";
const MENU_ID_FIX_KEYBOARD: &str = "fix_keyboard";
const MENU_ID_HOVER: &str = "hover_opens_hud";
/// Menu ids for saved passwords are `vault:<row>`, indexing into the snapshot
/// the menu was last built from.
const MENU_ID_VAULT_PREFIX: &str = "vault:";
const MENU_ID_VAULT_CLEANUP: &str = "vault_cleanup";
const MENU_ID_VAULT_MANAGE: &str = "vault_manage";

/// Menu id prefix for a recent-clip row. The index after the colon maps back to
/// the cached `ClipEntry` so a click can copy that clip's content.
const MENU_ID_CLIP_PREFIX: &str = "clip:";
/// "Open full clipboard" row pinned to the end of the recent-clips block.
const MENU_ID_CLIP_MORE: &str = "clip_more";
/// Top-of-menu "Open clipboard" row that launches the scrollable HUD popover.
const MENU_ID_OPEN_HUD: &str = "open_hud";

/// Stable id for the real clipboard status item. Shield spacers use other ids
/// so their hover/click events never open the HUD.
const MAIN_TRAY_ID: &str = "clipd-main";
/// How many sacrificial left-side status items to park when a menu-heavy app
/// (Sublime, Xcode, …) is frontmost. macOS hides status items from the left
/// when app menus need space — shields absorb that pressure first.
#[cfg(target_os = "macos")]
const MENU_BAR_SHIELD_COUNT: usize = 8;

/// How many recent clips the tray dropdown lists. The menu is a quick-pick
/// shortcut, not a full browser — `Open full clipboard` (or the HUD)
/// handles the long tail. 10 keeps the submenu compact; native macOS
/// submenus scroll if the list ever grows.
const TRAY_CLIP_LIMIT: usize = 10;

/// How many saved passwords the menu lists. The menu is a shortcut for "the one
/// I just saved", not a browser — `clipd vault list` handles the long tail.
const VAULT_MENU_LIMIT: usize = 12;

/// Rebuild the saved-passwords submenu from the system store.
///
/// `rows` is kept in step with the menu items so a click can be resolved back
/// to a secret by position; menu ids can't carry the secret's identity safely.
fn refresh_vault_menu(menu: &Submenu, rows: &mut Vec<SecretRef>) {
    while menu.remove_at(0).is_some() {}
    rows.clear();

    match clipd_core::list_secrets() {
        Ok(secrets) => {
            if secrets.is_empty() {
                let empty = MenuItem::new("No saved passwords yet", false, None);
                let _ = menu.append(&empty);
            } else {
                for (i, secret) in secrets.iter().take(VAULT_MENU_LIMIT).enumerate() {
                    let item = MenuItem::with_id(
                        format!("{MENU_ID_VAULT_PREFIX}{i}"),
                        vault_item_label(secret),
                        true,
                        None,
                    );
                    let _ = menu.append(&item);
                    rows.push(secret.clone());
                }
                if secrets.len() > VAULT_MENU_LIMIT {
                    let more = MenuItem::new(
                        format!("…and {} more", secrets.len() - VAULT_MENU_LIMIT),
                        false,
                        None,
                    );
                    let _ = menu.append(&more);
                }
            }

            let _ = menu.append(&PredefinedMenuItem::separator());
            let legacy = secrets.iter().filter(|s| is_legacy_autosave(s)).count();
            if legacy > 0 {
                let cleanup = MenuItem::with_id(
                    MENU_ID_VAULT_CLEANUP,
                    format!("Clean up {legacy} auto-saved entries…"),
                    true,
                    None,
                );
                let _ = menu.append(&cleanup);
            }
            let manage =
                MenuItem::with_id(MENU_ID_VAULT_MANAGE, "How to manage these…", true, None);
            let _ = menu.append(&manage);
        }
        Err(e) => {
            log::warn!("Couldn't read saved passwords: {e}");
            let failed = MenuItem::new("Couldn't read the Keychain", false, None);
            let _ = menu.append(&failed);
        }
    }
}

fn vault_item_label(secret: &SecretRef) -> String {
    match secret.saved_at.and_then(|t| chrono::DateTime::from_timestamp(t, 0)) {
        Some(dt) => format!(
            "{}  ·  {}",
            secret.title,
            dt.with_timezone(&chrono::Local).format("%b %-d, %-I:%M %p")
        ),
        None => secret.title.clone(),
    }
}

/// Entries written by the old silent auto-save, which named every password
/// `clipd password — <timestamp>` and so produced piles of indistinguishable
/// items. Matching on that exact shape avoids touching anything user-named.
fn is_legacy_autosave(secret: &SecretRef) -> bool {
    secret.service.starts_with("clipd: ") && secret.title.starts_with("clipd password — ")
}

/// Copy a saved password to the clipboard, flagged concealed and set to clear
/// itself. clipd-ui outlives the timeout, so the wipe actually runs.
fn copy_saved_password(secret: &SecretRef) {
    match clipd_core::reveal_secret(secret) {
        Ok(password) => match clipd_core::copy_secret(&password, None) {
            Ok(()) => notify(
                "clipd",
                &format!(
                    "Copied “{}” — clears in {}s",
                    secret.title,
                    clipd_core::DEFAULT_CLEAR_AFTER.as_secs()
                ),
            ),
            Err(e) => notify("clipd — couldn't copy", &e),
        },
        Err(e) => notify("clipd — couldn't read that password", &e),
    }
}

/// Offer to delete the entries the old auto-save behaviour accumulated.
fn cleanup_legacy_secrets() {
    let secrets = match clipd_core::list_secrets() {
        Ok(s) => s,
        Err(e) => return notify("clipd — couldn't read the Keychain", &e),
    };
    let legacy: Vec<_> = secrets.into_iter().filter(is_legacy_autosave).collect();
    if legacy.is_empty() {
        return notify("clipd", "Nothing to clean up.");
    }

    let prompt = format!(
        "Delete {} password{} that clipd auto-saved without asking?\\n\\n\
         They are all named “clipd password — <time>”, so most are false positives. \
         Anything you named yourself is left alone.\\n\\nThis can't be undone.",
        legacy.len(),
        if legacy.len() == 1 { "" } else { "s" }
    );
    if !confirm(&prompt, "Delete them") {
        return;
    }

    let mut deleted = 0usize;
    let mut failed = 0usize;
    for secret in &legacy {
        match clipd_core::forget_secret(secret) {
            Ok(()) => deleted += 1,
            Err(e) => {
                log::warn!("Couldn't delete {}: {e}", secret.title);
                failed += 1;
            }
        }
    }
    let msg = if failed == 0 {
        format!("Deleted {deleted} auto-saved passwords.")
    } else {
        format!("Deleted {deleted}; {failed} couldn't be removed (see logs).")
    };
    notify("clipd", &msg);
}

/// Rebuild the "Recent ▸" submenu from the clipd database. Mirrors
/// `refresh_vault_menu`: clear via `remove_at(0)` then re-append so a click
/// resolves back to a `ClipEntry` by position. The submenu keeps the top-level
/// dropdown compact; the scrollable, readable HUD popover (launched by the
/// "Open clipboard" row above it) is the primary surface for browsing clips.
fn refresh_clips_menu(menu: &Submenu, rows: &mut Vec<ClipEntry>) {
    while menu.remove_at(0).is_some() {}
    rows.clear();

    let clips = match ClipStore::new(&ClipStore::default_path()) {
        Ok(store) => store.get_recent(TRAY_CLIP_LIMIT).unwrap_or_default(),
        Err(e) => {
            log::warn!("Couldn't open clip database for tray menu: {e}");
            let failed = MenuItem::new("Couldn't read clipboard history", false, None);
            let _ = menu.append(&failed);
            return;
        }
    };

    if clips.is_empty() {
        let empty = MenuItem::new("No clips yet — copy something", false, None);
        let _ = menu.append(&empty);
        return;
    }

    for (i, clip) in clips.iter().enumerate() {
        let item = IconMenuItem::with_id_and_native_icon(
            format!("{MENU_ID_CLIP_PREFIX}{i}"),
            clip_menu_label(clip),
            true,
            Some(clip_native_icon(&clip.content_type)),
            None,
        );
        let _ = menu.append(&item);
        rows.push(clip.clone());
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let more = IconMenuItem::with_id_and_native_icon(
        MENU_ID_CLIP_MORE,
        "Open full clipboard…",
        true,
        Some(NativeIcon::MultipleDocuments),
        None,
    );
    let _ = menu.append(&more);
}

/// One-line label for a clip row: type glyph, preview, source, time.
fn clip_menu_label(clip: &ClipEntry) -> String {
    let preview = {
        let p = one_line_preview(&clip.preview, 60);
        if p.is_empty() {
            one_line_preview(&clip.content, 60)
        } else {
            p
        }
    };
    let preview = if preview.is_empty() {
        match clip.content_type {
            ContentType::Image => "Image".to_string(),
            _ => "(empty)".to_string(),
        }
    } else {
        preview
    };
    let time = relative_time_short(&clip.timestamp);
    match &clip.source_app {
        Some(app) if !app.trim().is_empty() => format!("{preview}  ·  {app}  ·  {time}"),
        _ => format!("{preview}  ·  {time}"),
    }
}

/// macOS native menu icon for a clip row, picked by content type. muda 0.15
/// has no per-type icon (no Mail/Image/Text), so we map to the closest native
/// template image and let the label's preview carry the rest of the meaning.
fn clip_native_icon(ct: &ContentType) -> NativeIcon {
    match ct {
        ContentType::Url => NativeIcon::FollowLinkFreestanding,
        ContentType::Email => NativeIcon::Share,
        ContentType::Path => NativeIcon::Folder,
        ContentType::Image => NativeIcon::QuickLook,
        ContentType::Code => NativeIcon::Share,
        _ => NativeIcon::Bookmarks,
    }
}

/// Flatten a clip's preview/content to a single line, trimmed to `max` chars.
fn one_line_preview(s: &str, max: usize) -> String {
    let collapsed: String = s.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
    let collapsed = collapsed.trim();
    if collapsed.chars().count() <= max {
        collapsed.to_string()
    } else {
        let head: String = collapsed.chars().take(max).collect();
        format!("{head}…")
    }
}

/// "now", "5m", "2h", "3d", "2w" — mirrors the GUI's relative-time helper.
fn relative_time_short(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let secs = chrono::Utc::now().signed_duration_since(*dt).num_seconds();
    if secs < 60 {
        return "now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    let weeks = days / 7;
    format!("{weeks}w")
}

/// Copy a clip to the system clipboard. Text clips use the clip's `content`;
/// image clips put the PNG on the clipboard so it pastes as an image.
fn copy_clip(clip: &ClipEntry) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let result = if clip.content_type == ContentType::Image {
            clip.image_path
                .as_deref()
                .and_then(|path| {
                    let (w, h, rgba) = clipd_core::load_rgba(std::path::Path::new(path)).ok()?;
                    Some(cb.set_image(arboard::ImageData {
                        width: w as usize,
                        height: h as usize,
                        bytes: rgba.into(),
                    }))
                })
                .unwrap_or_else(|| cb.set_text(&clip.content))
        } else {
            cb.set_text(&clip.content)
        };
        let title = if result.is_ok() { "clipd" } else { "clipd — couldn't copy" };
        notify(title, &one_line_preview(&clip.content, 80));
    } else {
        notify("clipd — couldn't copy", "Clipboard unavailable");
    }
}

fn show_vault_help() {
    notify(
        "clipd — managing saved passwords",
        "Run `clipd vault list`, `rename`, or `rm` in a terminal for the full set.",
    );
}

/// Yes/no dialog. Returns true only on an explicit confirmation.
#[cfg(target_os = "macos")]
fn confirm(message: &str, confirm_label: &str) -> bool {
    let script = format!(
        r#"display dialog "{}" buttons {{"Cancel", "{}"}} default button "Cancel" with icon caution"#,
        message.replace('"', "'"),
        confirm_label.replace('"', "'")
    );
    Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(confirm_label))
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn confirm(_message: &str, _confirm_label: &str) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn notify(title: &str, body: &str) {
    let script = format!(
        r#"display notification "{}" with title "{}""#,
        body.replace('"', "'"),
        title.replace('"', "'")
    );
    let _ = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output();
}

#[cfg(not(target_os = "macos"))]
fn notify(title: &str, body: &str) {
    log::info!("{title}: {body}");
}

fn hud_tray_label(hud_on: bool) -> String {
    // macOS shows the Swift HUD overlay; Windows shows overlay/toast
    // notifications — same setting, platform-accurate name.
    let what = if cfg!(target_os = "macos") {
        "Slot copy feedback"
    } else {
        "Slot notifications"
    };
    if hud_on {
        format!("Turn off {}", what.to_lowercase())
    } else {
        format!("Turn on {}", what.to_lowercase())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let event_loop = EventLoop::new();
    // Tray rects arrive in PHYSICAL pixels (`dpi::PhysicalPosition`), but the
    // popover positions itself in logical points. On a 2x Retina display that
    // is a factor-of-two error, which used to park the panel at screen-centre
    // instead of under the extra. Convert here, and if the rect is too wide
    // to be a status item, trust the cursor — it is on the icon.
    let tray_scale = event_loop
        .primary_monitor()
        .map(|m| m.scale_factor())
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);
    let wake_proxy = event_loop.create_proxy();
    let (tray_tx, tray_rx) = mpsc::channel::<TrayIconEvent>();
    TrayIconEvent::set_event_handler(Some(move |ev| {
        let _ = tray_tx.send(ev);
        let _ = wake_proxy.send_event(());
    }));

    let menu = Menu::new();
    let item_daemon = IconMenuItem::with_id_and_native_icon(
        MENU_ID_DAEMON,
        daemon_tray_label(true),
        true,
        Some(NativeIcon::StatusAvailable),
        None,
    );
    let item_search = IconMenuItem::with_id_and_native_icon(
        MENU_ID_SEARCH,
        "Open full clipboard",
        true,
        Some(NativeIcon::MultipleDocuments),
        None,
    );
    let hud_on = load_paste_transform_settings().hud_enabled;
    // Plain MenuItem (not CheckMenuItem): macOS tray checkmarks were drifting from
    // `paste_transform.json`; explicit on/off text + toggle keeps daemon and UI aligned.
    let item_hud = IconMenuItem::with_id_and_native_icon(
        MENU_ID_HUD,
        hud_tray_label(hud_on),
        true,
        Some(NativeIcon::QuickLook),
        None,
    );
    let tui_on = load_tui_mode();
    let item_tui_mode = IconMenuItem::with_id_and_native_icon(
        MENU_ID_TUI_MODE,
        tui_mode_label(tui_on),
        true,
        Some(NativeIcon::Advanced),
        None,
    );
    // Settings must be reachable from here: the palette is normally summoned
    // with a hotkey, and when macOS has not granted Input Monitoring that
    // hotkey is dead — which is precisely when the user needs to open Settings
    // to fix something. The tray menu always works.
    let item_settings = IconMenuItem::with_id_and_native_icon(
        MENU_ID_SETTINGS,
        "Settings…",
        true,
        Some(NativeIcon::PreferencesGeneral),
        None,
    );
    #[cfg(target_os = "macos")]
    let item_fix_keyboard = IconMenuItem::with_id_and_native_icon(
        MENU_ID_FIX_KEYBOARD,
        "Fix keyboard permissions…",
        true,
        Some(NativeIcon::StatusUnavailable),
        None,
    );
    let item_hud_view = IconMenuItem::with_id_and_native_icon(
        MENU_ID_HUD_VIEW,
        hud_view_label(hud_currently_visible()),
        true,
        Some(NativeIcon::ListView),
        None,
    );
    let item_hover = MenuItem::with_id(
        MENU_ID_HOVER,
        hover_tray_label(load_paste_transform_settings().hover_opens_hud),
        true,
        None,
    );
    let item_quit = IconMenuItem::with_id_and_native_icon(
        MENU_ID_QUIT,
        "Quit clipd",
        true,
        Some(NativeIcon::StopProgressFreestanding),
        None,
    );

    // Saved passwords live here rather than in the search window because macOS
    // only lets the process that wrote a Keychain item read it back without an
    // authorisation prompt — and this process, which hosts the daemon, is the
    // one that saved them.
    // muda 0.15 cannot attach a native image to a submenu itself, so keep the
    // lock in the title while every actionable row uses a native menu icon.
    let vault_menu = Submenu::new("Saved passwords", true);
    let mut vault_rows: Vec<SecretRef> = Vec::new();
    refresh_vault_menu(&vault_menu, &mut vault_rows);

    // Left-click the tray icon opens the scrollable HUD popover directly
    // (see the Click handler below) — that is the primary clip surface, so
    // the dropdown menu no longer needs a clips block or an "Open clipboard"
    // row. A small "Recent ▸" submenu stays for users who right-click and
    // want a quick one-click copy without opening the HUD.
    let clips_menu = Submenu::new("Recent", true);
    let mut clip_rows: Vec<ClipEntry> = Vec::new();
    refresh_clips_menu(&clips_menu, &mut clip_rows);
    let mut last_clip_refresh = std::time::Instant::now();
    let mut last_icon_theme = clipd_core::load_theme();

    let item_open_hud = IconMenuItem::with_id_and_native_icon(
        MENU_ID_OPEN_HUD,
        "Open clipboard",
        true,
        Some(NativeIcon::ListView),
        None,
    );

    // Everything secondary lives inside one "Settings ▸" submenu so the
    // dropdown is tiny: Open clipboard, Recent ▸, Settings ▸, Start/Stop, Quit.
    let settings_menu = Submenu::new("Settings", true);
    settings_menu.append(&item_hud_view)?;
    settings_menu.append(&item_hover)?;
    settings_menu.append(&PredefinedMenuItem::separator())?;
    settings_menu.append(&item_settings)?;
    settings_menu.append(&item_search)?;
    settings_menu.append(&vault_menu)?;
    settings_menu.append(&PredefinedMenuItem::separator())?;
    settings_menu.append(&item_hud)?;
    settings_menu.append(&item_tui_mode)?;

    // Four groups, most-used first, destructive last — the shape of every
    // macOS status menu: what you came for, where the rest lives, the app's
    // own state, then the way out.
    //
    // `settings_menu`, `clips_menu` and `vault_menu` were built and filled
    // here but never appended, so Settings, Recent and Saved passwords did
    // not exist in the dropdown at all. That left no way to reach Settings
    // when the hotkey is dead — the one case this menu is here for.
    menu.append(&item_open_hud)?;
    menu.append(&clips_menu)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&settings_menu)?;
    menu.append(&vault_menu)?;
    // A repair only appears when something is broken. A permanent "fix this"
    // row reads as a warning the app can never clear.
    #[cfg(target_os = "macos")]
    if matches!(
        clipd_core::load_hotkey_status(),
        clipd_core::HotkeyStatus::NeedsAccessibility
    ) {
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&item_fix_keyboard)?;
    }
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&item_daemon)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&item_quit)?;

    // Accessory policy + template icon: without these, macOS can drop or
    // wash out a coloured status item when a dark app (Sublime, etc.) becomes
    // frontmost and retints the menu bar. Cursor/Claude stay visible because
    // they use the same agent + template pattern.
    #[cfg(target_os = "macos")]
    adopt_accessory_activation_policy();

    let main_tray_id = TrayIconId::new(MAIN_TRAY_ID);
    let tray_icon = {
        let mut builder = TrayIconBuilder::new()
            .with_id(main_tray_id.clone())
            .with_tooltip("clipd ui  ·  click for clipboard")
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            .with_icon(make_icon());
        // Non-template: keeps the cat's color. macOS won't add a background
        // plate for a non-template icon with transparent corners.
        builder = builder.with_icon_as_template(false);
        match builder.build() {
            Ok(tray) => {
                log::info!("clipd-ui: status item created");
                tray
            }
            Err(e) => {
                log::error!("clipd-ui: could not create the status item: {e}");
                return Err(e.into());
            }
        }
    };

    // On a notched display the item we just created is sitting behind the
    // camera housing. Park blank spacers to its left so it shifts into view;
    // they take the hidden slots instead.
    #[cfg(target_os = "macos")]
    let notch_shields: Vec<TrayIcon> = {
        let count = shields_for_notch();
        if count > 0 {
            log::info!("clipd-ui: notch detected — parking {count} spacers left of the status item");
        }
        build_menu_bar_shields_n(count)
    };

    let menu_channel = MenuEvent::receiver();
    let mut last_tray_keep_alive = std::time::Instant::now();
    // Sacrificial status items parked to the LEFT of Clipd while menu-heavy
    // apps are frontmost. Dropped again when you leave those apps.
    #[cfg(target_os = "macos")]
    let mut menu_bar_shields: Vec<TrayIcon> = Vec::new();
    #[cfg(target_os = "macos")]
    let mut last_frontmost_poll = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    let mut shields_armed = false;

    // TCC prompts must run on the main thread — macOS ignores them from the
    // daemon's background hotkey thread. Offer setup only once per Clipd
    // version. If the app is relaunched while a toggle is still settling (or
    // has been denied), repeatedly opening the system sheet is disruptive and
    // does not make macOS grant the permission any faster.
    #[cfg(target_os = "macos")]
    let keyboard_granted_before_prompt = clipd_core::keyboard_permissions_granted();
    #[cfg(target_os = "macos")]
    let should_offer_keyboard_setup =
        !keyboard_granted_before_prompt && claim_keyboard_permission_offer();
    #[cfg(target_os = "macos")]
    let keyboard_granted = if should_offer_keyboard_setup {
        clipd_core::request_keyboard_permissions()
    } else {
        keyboard_granted_before_prompt
    };

    // Auto-start daemon on launch — runs IN-PROCESS (see start_daemon docs) so the
    // macOS keyboard listener inherits clipd-ui's Input Monitoring / Accessibility grants.
    let mut daemon: Option<DaemonHandle> = Some(start_daemon());
    item_daemon.set_text(daemon_tray_label(daemon.is_some()));

    // Carbon RegisterEventHotKey for Settings shortcuts (open GUI / palette).
    // These do not need the modifying CGEventTap, so Ctrl+Space and the palette
    // chord keep working while multi-slot access is still being granted.
    #[cfg(target_os = "macos")]
    let mut settings_hotkeys = SettingsHotkeys::new();
    #[cfg(target_os = "macos")]
    settings_hotkeys.sync_from_settings();
    #[cfg(target_os = "macos")]
    let mut last_hotkey_sync = std::time::Instant::now();

    // Hover delay: only show the HUD if the pointer rests on the tray icon for
    // a moment, not just flicks past it.
    let mut hover_entered_at: Option<std::time::Instant> = None;
    let mut hide_pending_at: Option<std::time::Instant> = None;
    // Below the ~100ms threshold where a delay registers, so the popover
    // reads as instant while a flick past the icon still does not trigger it.
    const HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(40);
    const HIDE_DELAY: std::time::Duration = std::time::Duration::from_millis(600);

    // Keep one HUD process alive rather than launching a fresh one per hover.
    //
    // Spawning on hover means every single hover pays for a process start, a
    // GPU context and a window — hundreds of milliseconds before anything can
    // appear, which no amount of tuning downstream can recover. Resident, a
    // hover is just a request to a process that is already up.
    let mut hud_child: Option<std::process::Child> = open_gui_hud();

    #[cfg(target_os = "macos")]
    if !keyboard_granted {
        if should_offer_keyboard_setup {
            log::warn!(
                "Keyboard access missing ({}) — opening System Settings once for this version. \
                 Enable Clipd under Accessibility AND Input Monitoring.",
                clipd_core::missing_keyboard_permission_label()
            );
            clipd_core::open_keyboard_permission_settings();
            // Non-blocking: daemon + Carbon hotkeys are already running above.
            let _ = std::process::Command::new("/usr/bin/osascript")
                .args([
                    "-e",
                    r#"display dialog "Clipd shortcuts need two toggles turned ON:

1. System Settings → Privacy & Security → Accessibility
2. System Settings → Privacy & Security → Input Monitoring

Enable Clipd in both lists. Ctrl+Space / palette work after Accessibility; multi-slot copy also needs Input Monitoring." buttons {"OK"} default button "OK" with title "Clipd — keyboard access needed" with icon caution"#,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        } else {
            log::warn!(
                "Keyboard access still missing ({}); automatic setup was already offered for this version",
                clipd_core::missing_keyboard_permission_label()
            );
        }
    }

    // Tray-only startup. The clipboard surface only appears when the user
    // hovers or clicks the tray icon, so clipd never parks a window on top of
    // browser tabs or terminal menus at launch.
    let _auto_open_gui = !load_tui_mode();

    // If the user has selected the Notch Island layout, spawn the island
    // process at startup so it's always visible at the notch. Nothing else
    // would restore it after a logout: the setting survives, the window does
    // not.
    if !load_tui_mode() && clipd_core::island_layout_active() {
        let _ = open_gui_island();
    }

    // No pre-warm — the HUD is spawned fresh on each hover/click. This is
    // simpler and more reliable than keeping a process alive off-screen.

    event_loop.run(move |event, _, control_flow| {
        #[cfg(target_os = "macos")]
        let _ = &notch_shields;
        // Poll at 50ms so the hover-delay timer fires promptly. Wait would
        // sleep until the next OS event, making the delay feel random.
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(50),
        );

        // Hover delay: if the pointer has been resting on the tray icon past
        // the threshold, fire the HUD now. Cleared by Leave/Click.
        if let Some(entered) = hover_entered_at {
            if entered.elapsed() >= HOVER_DELAY && load_paste_transform_settings().hover_opens_hud {
                hover_entered_at = None;
                hide_pending_at = None;
                show_hud(&mut hud_child);
            }
        }

        // Hide delay: if the pointer left the tray icon and hasn't come back
        // within the grace period, hide the HUD.
        if let Some(left) = hide_pending_at {
            if left.elapsed() >= HIDE_DELAY {
                hide_pending_at = None;
                send_surface_request_to("hud", "hidden");
            }
        }

        // Keep the recent-clips submenu warm. macOS shows the tray menu
        // synchronously on click, so refreshing from the click handler runs
        // too late — by the time we rebuild the submenu it is already on
        // screen. Polling every 3s keeps the list fresh without a background
        // thread.
        if last_clip_refresh.elapsed() >= std::time::Duration::from_secs(3) {
            refresh_clips_menu(&clips_menu, &mut clip_rows);
            // The status item wears the theme's accent, so switching theme in
            // Settings has to reach it too — otherwise the one piece of clipd
            // that is always on screen keeps yesterday's colour.
            let theme_now = clipd_core::load_theme();
            if theme_now != last_icon_theme {
                last_icon_theme = theme_now;
                let _ = tray_icon.set_icon(Some(make_icon_for(theme_now.colors())));
            }
            // The window can be closed from the window itself, so the menu has
            // to look rather than remember — otherwise it offers to show
            // something already on screen.
            item_hud_view.set_text(hud_view_label(hud_currently_visible()));
            last_clip_refresh = std::time::Instant::now();
        }

        // Re-assert the status item after app switches. Some macOS versions
        // hide non-agent status items when the frontmost app retints the bar.
        if last_tray_keep_alive.elapsed() >= std::time::Duration::from_secs(2) {
            let _ = tray_icon.set_visible(true);
            last_tray_keep_alive = std::time::Instant::now();
        }

        #[cfg(target_os = "macos")]
        {
            // Arm/disarm left-side shields when a wide-menu app is frontmost.
            // Sublime's Find/Goto/Tools/Project menus are what was hiding Clipd.
            if last_frontmost_poll.elapsed() >= std::time::Duration::from_millis(400) {
                last_frontmost_poll = std::time::Instant::now();
                let heavy = frontmost_app_is_menu_heavy();
                if heavy && !shields_armed {
                    menu_bar_shields = build_menu_bar_shields();
                    shields_armed = true;
                    let _ = tray_icon.set_visible(true);
                    log::info!(
                        "Menu-bar shields armed ({} spacers) — protecting Clipd icon",
                        menu_bar_shields.len()
                    );
                } else if !heavy && shields_armed {
                    menu_bar_shields.clear();
                    shields_armed = false;
                    let _ = tray_icon.set_visible(true);
                    log::info!("Menu-bar shields cleared");
                }
            }

            // Re-read Settings every couple of seconds so combo-box changes apply
            // without restarting the tray host.
            if last_hotkey_sync.elapsed() >= std::time::Duration::from_secs(2) {
                settings_hotkeys.sync_from_settings();
                last_hotkey_sync = std::time::Instant::now();
            }
            settings_hotkeys.poll_and_dispatch();
        }

        if let Event::UserEvent(()) = event {
            while let Ok(tray_ev) = tray_rx.try_recv() {
                // Ignore sacrificial shield icons — only the main clipboard
                // status item drives the HUD / click behaviour.
                if tray_event_id(&tray_ev) != Some(&main_tray_id) {
                    continue;
                }
                match tray_ev {
                    TrayIconEvent::Enter { rect, position, .. } => {
                        clipd_core::save_tray_anchor(logical_tray_anchor(
                            &rect, position.x, tray_scale,
                        ));
                        // Pointer entered the tray icon — show the HUD
                        // immediately (no delay — the user is clearly here).
                        hover_entered_at = None;
                        hide_pending_at = None;
                        if load_paste_transform_settings().hover_opens_hud {
                            show_hud(&mut hud_child);
                        }
                    }
                    TrayIconEvent::Leave { .. } => {
                        // Don't hide immediately — the user is likely moving
                        // into the popover. Start a hide timer; if the HUD
                        // doesn't report "still in use" it will fire.
                        hover_entered_at = None;
                        hide_pending_at = Some(std::time::Instant::now());
                    }
                    TrayIconEvent::Move { rect, position, .. } => {
                        // Refresh anchor while moving over the icon. Don't
                        // re-send "show" — that would reset the HUD each time
                        // and cause flicker.
                        clipd_core::save_tray_anchor(logical_tray_anchor(
                            &rect, position.x, tray_scale,
                        ));
                    }
                    TrayIconEvent::Click {
                        button,
                        button_state,
                        rect,
                        position,
                        ..
                    } if matches!(button_state, MouseButtonState::Down | MouseButtonState::Up) => {
                        clipd_core::save_tray_anchor(logical_tray_anchor(
                            &rect, position.x, tray_scale,
                        ));
                        // The two buttons do opposite things, so they must not
                        // share a code path:
                        //   left  — open the full clipboard palette (not a
                        //           persistent pill; the palette is a normal
                        //           window that closes itself).
                        //   right — the menu, which must not appear on top of
                        //           the popover, so dismiss it first.
                        #[cfg(any(target_os = "macos", target_os = "windows"))]
                        if button_state == MouseButtonState::Up {
                            match button {
                                tray_icon::MouseButton::Left => {
                                    hide_pending_at = None;
                                    hover_entered_at = None;
                                    show_hud(&mut hud_child);
                                }
                                _ => {}
                            }
                        }
                        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                        let _ = button;
                    }
                    _ => {}
                }
            }
        }

        if let Event::NewEvents(_) = event {
            if let Ok(menu_event) = menu_channel.try_recv() {
                match menu_event.id.0.as_str() {
                    MENU_ID_DAEMON => {
                        match daemon.take() {
                            Some(mut handle) => handle.stop(),
                            None => daemon = Some(start_daemon()),
                        }
                        item_daemon.set_text(daemon_tray_label(daemon.is_some()));
                    }
                    MENU_ID_SEARCH => {
                        if load_tui_mode() {
                            open_search_in_terminal();
                        } else {
                            open_gui_search();
                        }
                    }
                    MENU_ID_OPEN_HUD => {
                        if load_tui_mode() {
                            open_search_in_terminal();
                        } else {
                            show_hud(&mut hud_child);
                        }
                    }
                    MENU_ID_HOVER => {
                        let mut s = load_paste_transform_settings();
                        s.hover_opens_hud = !s.hover_opens_hud;
                        save_paste_transform_settings(&s);
                        item_hover.set_text(hover_tray_label(s.hover_opens_hud));
                    }
                    MENU_ID_SETTINGS => {
                        open_gui_settings();
                    }
                    #[cfg(target_os = "macos")]
                    MENU_ID_FIX_KEYBOARD => {
                        // Prompt + open Privacy panes. The daemon retries the
                        // event tap for ~90s, so granting here can revive
                        // multi-slot without a full restart.
                        clipd_core::request_keyboard_permissions();
                        clipd_core::open_keyboard_permission_settings();
                    }
                    MENU_ID_HUD_VIEW => {
                        let showing = hud_currently_visible();
                        if showing {
                            send_surface_request_to("hud", "hidden");
                        } else {
                            show_hud(&mut hud_child);
                        }
                        item_hud_view.set_text(hud_view_label(!showing));
                    }
                    MENU_ID_HUD => {
                        let mut s = load_paste_transform_settings();
                        s.hud_enabled = !s.hud_enabled;
                        save_paste_transform_settings(&s);
                        item_hud.set_text(hud_tray_label(s.hud_enabled));
                    }
                    MENU_ID_TUI_MODE => {
                        let enabled = !load_tui_mode();
                        save_tui_mode(enabled);
                        item_tui_mode.set_text(tui_mode_label(enabled));
                    }
                    MENU_ID_QUIT => {
                        // Order matters. The GUI watchdog restarts this tray
                        // host whenever the daemon looks down, so the windows
                        // must be told to quit *before* the daemon stops —
                        // otherwise clipd resurrects itself seconds later.
                        // Quit every surface process now that they are separate.
                        send_surface_request_to("main", "quit");
                        send_surface_request_to("hud", "quit");
                        send_surface_request_to("island", "quit");
                        // Windows poll for requests ~4x/second; give them one
                        // cycle to see it before the process tree goes away.
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        if let Some(mut handle) = daemon.take() {
                            handle.stop();
                        }
                        *control_flow = ControlFlow::Exit;
                    }
                    MENU_ID_VAULT_CLEANUP => {
                        cleanup_legacy_secrets();
                        refresh_vault_menu(&vault_menu, &mut vault_rows);
                    }
                    MENU_ID_VAULT_MANAGE => show_vault_help(),
                    MENU_ID_CLIP_MORE => {
                        if load_tui_mode() {
                            open_search_in_terminal();
                        } else {
                            open_gui_search();
                        }
                    }
                    id if id.starts_with(MENU_ID_CLIP_PREFIX) => {
                        if let Some(clip) = id
                            .trim_start_matches(MENU_ID_CLIP_PREFIX)
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| clip_rows.get(i))
                        {
                            copy_clip(clip);
                        }
                    }
                    id if id.starts_with(MENU_ID_VAULT_PREFIX) => {
                        if let Some(secret) = id
                            .trim_start_matches(MENU_ID_VAULT_PREFIX)
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| vault_rows.get(i))
                        {
                            copy_saved_password(secret);
                        }
                    }
                    _ => {}
                }
            }
        }
    });
}

// ── Settings shortcuts via Carbon RegisterEventHotKey (macOS) ──

#[cfg(target_os = "macos")]
struct SettingsHotkeys {
    manager: Option<GlobalHotKeyManager>,
    open_gui: Option<HotKey>,
    palette: Option<HotKey>,
    /// Fingerprint of the last registered settings so we only rebind on change.
    fingerprint: String,
}

#[cfg(target_os = "macos")]
impl SettingsHotkeys {
    fn new() -> Self {
        let manager = match GlobalHotKeyManager::new() {
            Ok(m) => Some(m),
            Err(e) => {
                log::warn!("Carbon hotkey manager unavailable: {e}");
                None
            }
        };
        Self {
            manager,
            open_gui: None,
            palette: None,
            fingerprint: String::new(),
        }
    }

    fn sync_from_settings(&mut self) {
        let Some(manager) = self.manager.as_ref() else {
            return;
        };
        let s = load_paste_transform_settings();
        let fingerprint = format!(
            "{:?}|{:?}|{}|{:?}",
            s.open_gui_hotkey, s.ctrl_space_action, s.palette_enabled, s.palette_trigger
        );
        if fingerprint == self.fingerprint {
            return;
        }

        if let Some(hk) = self.open_gui.take() {
            let _ = manager.unregister(hk);
        }
        if let Some(hk) = self.palette.take() {
            let _ = manager.unregister(hk);
        }

        if let Some((mods, code)) = open_gui_hotkey_binding(s.open_gui_hotkey) {
            let hk = HotKey::new(Some(mods), code);
            match manager.register(hk) {
                Ok(()) => {
                    log::info!("Registered Settings open-GUI hotkey: {}", s.open_gui_hotkey.label());
                    self.open_gui = Some(hk);
                }
                Err(e) => log::warn!(
                    "Failed to register {}: {e}",
                    s.open_gui_hotkey.label()
                ),
            }
        }

        if s.palette_enabled {
            if let Some((mods, code)) = palette_hotkey_binding(s.palette_trigger) {
                let hk = HotKey::new(Some(mods), code);
                match manager.register(hk) {
                    Ok(()) => {
                        log::info!(
                            "Registered Settings palette hotkey: {}",
                            s.palette_trigger.label()
                        );
                        self.palette = Some(hk);
                    }
                    Err(e) => log::warn!(
                        "Failed to register {}: {e}",
                        s.palette_trigger.label()
                    ),
                }
            }
        }

        self.fingerprint = fingerprint;
    }

    fn poll_and_dispatch(&self) {
        let receiver = GlobalHotKeyEvent::receiver();
        while let Ok(event) = receiver.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            let s = load_paste_transform_settings();
            if self.open_gui.is_some_and(|hk| hk.id() == event.id) {
                match s.ctrl_space_action {
                    CtrlSpaceAction::OpenGui => {
                        log::info!("⌨️  {} → OpenGui (Carbon)", s.open_gui_hotkey.label());
                        if !clipd_daemon::request_shortcut(clipd_daemon::ShortcutRequest::OpenGui)
                        {
                            open_gui_search();
                        }
                    }
                    CtrlSpaceAction::SlotMemory => {
                        log::info!("⌨️  {} → SlotMemory (Carbon)", s.open_gui_hotkey.label());
                        let _ = clipd_daemon::request_shortcut(
                            clipd_daemon::ShortcutRequest::SlotMemory,
                        );
                    }
                    CtrlSpaceAction::CommandPalette => {
                        log::info!(
                            "⌨️  {} → CommandPalette (Carbon)",
                            s.open_gui_hotkey.label()
                        );
                        let _ = clipd_daemon::request_shortcut(
                            clipd_daemon::ShortcutRequest::CommandPalette,
                        );
                    }
                }
            } else if self.palette.is_some_and(|hk| hk.id() == event.id) {
                log::info!("⌨️  {} → memory palette (Carbon)", s.palette_trigger.label());
                if !clipd_daemon::request_shortcut(clipd_daemon::ShortcutRequest::SlotPicker) {
                    open_gui_search();
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn open_gui_hotkey_binding(hk: OpenGuiHotkey) -> Option<(Modifiers, Code)> {
    match hk {
        OpenGuiHotkey::CtrlG => Some((Modifiers::CONTROL, Code::KeyG)),
        OpenGuiHotkey::AltG => None, // Windows-only
        OpenGuiHotkey::CmdShiftG => Some((Modifiers::SUPER | Modifiers::SHIFT, Code::KeyG)),
        OpenGuiHotkey::CtrlShiftG => Some((Modifiers::CONTROL | Modifiers::SHIFT, Code::KeyG)),
        OpenGuiHotkey::CtrlSpace => Some((Modifiers::CONTROL, Code::Space)),
        OpenGuiHotkey::OptSpace => Some((Modifiers::ALT, Code::Space)),
        OpenGuiHotkey::Disabled => None,
    }
}

#[cfg(target_os = "macos")]
fn palette_hotkey_binding(trigger: PaletteTrigger) -> Option<(Modifiers, Code)> {
    match trigger {
        PaletteTrigger::CmdShiftV => Some((Modifiers::SUPER | Modifiers::SHIFT, Code::KeyV)),
        PaletteTrigger::CtrlOptSpace => {
            Some((Modifiers::CONTROL | Modifiers::ALT, Code::Space))
        }
        PaletteTrigger::OptSpace => Some((Modifiers::ALT, Code::Space)),
        // Nothing to register — the keys stay with whatever else wants them.
        PaletteTrigger::Off => None,
    }
}

// ── TUI mode persistence ──

fn tui_mode_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("use_tui")
}

fn load_tui_mode() -> bool {
    tui_mode_path().exists()
}

fn save_tui_mode(enabled: bool) {
    let path = tui_mode_path();
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, "true").ok();
    } else {
        std::fs::remove_file(&path).ok();
    }
}

// ── Launch search UIs ──

fn process_lock_name(mode: &str) -> &'static str {
    match mode {
        "hud" => "gui-hud",
        _ => "gui-main",
    }
}

/// Where the clipd-gui process parks its current surface mode so the tray can
/// tell whether a given surface is currently shown and toggle it off rather than
/// re-launching it. Mirrors `surface_state_path` / `read_surface_state` in
/// clipd-gui — kept duplicated so clipd-ui does not depend on egui/clipd-gui.
fn surface_state_path(mode: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join(format!("{}.state", process_lock_name(mode)))
}

fn surface_request_path(mode: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join(format!("{}.request", process_lock_name(mode)))
}

fn daemon_tray_label(running: bool) -> &'static str {
    // Menu items are named for what the click does, not for the state they
    // report — the same reason macOS says "Hide Dock" rather than "Dock: On".
    // "daemon" is our word for it; the user just has clipd running or not.
    if running {
        "Pause clipd"
    } else {
        "Resume clipd"
    }
}

fn hover_tray_label(on: bool) -> &'static str {
    if on {
        "✓ Hover shows clipboard — disable"
    } else {
        "Hover shows clipboard — enable"
    }
}

fn hud_currently_visible() -> bool {
    matches!(
        std::fs::read_to_string(surface_state_path("hud"))
            .ok()
            .as_deref()
            .map(str::trim),
        Some("hud")
    )
}

/// Horizontal centre of the extra, in logical points.
///
/// tray-icon reports a physical rect. A real status item is ~22pt wide; if
/// the rect is much larger (a full-bar window, a zero-size one) the centre
/// is meaningless and we use the cursor, which is on the icon.
fn logical_tray_anchor(rect: &tray_icon::Rect, cursor_x: f64, scale: f64) -> f64 {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let width = f64::from(rect.size.width) / scale;
    let centre = (rect.position.x + f64::from(rect.size.width) / 2.0) / scale;
    let cursor = cursor_x / scale;
    if (12.0..56.0).contains(&width) {
        centre
    } else {
        cursor
    }
}

/// Ask the resident HUD to open, spawning it if it has died.
fn show_hud(hud_child: &mut Option<std::process::Child>) {
    send_surface_request_to("hud", "hud");
    let alive = match hud_child.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    };
    if !alive {
        *hud_child = open_gui_hud();
    }
}

/// Ask the running clipd-gui to switch surfaces by dropping a request file.
/// Mirrors `send_surface_request` in clipd-gui. `target` selects which process
/// receives the request; defaults to the main palette process.
fn send_surface_request_to(target: &str, mode: &str) {
    let path = surface_request_path(target);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, mode);
}

// Kept for callers that don't know which process to target; defaults to the
// main palette process.
#[allow(dead_code)]
fn send_surface_request(mode: &str) {
    send_surface_request_to("main", mode);
}

fn hud_view_label(on: bool) -> &'static str {
    if on {
        "Hide the clipboard window"
    } else {
        "Show the clipboard window"
    }
}

fn tui_mode_label(on: bool) -> &'static str {
    if on {
        "Turn off Terminal mode"
    } else {
        "Turn on Terminal mode"
    }
}

/// Open the palette straight on the Settings tab.
fn open_gui_settings() {
    let exe = resolve_clipd_gui_exe();
    let _ = Command::new(&exe)
        .arg("--settings")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Launch the Dynamic Notch Island as a separate process.
///
/// Its own process because it owns a window that has to outlive any palette
/// the user opens and closes.
fn open_gui_island() -> Option<std::process::Child> {
    let exe = resolve_clipd_gui_exe();
    Command::new(&exe)
        .arg("--island")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Launch the searchable menu-bar clipboard HUD, returning its child handle
/// so the tray item can behave like a real show/hide toggle.
fn open_gui_hud() -> Option<std::process::Child> {
    let exe = resolve_clipd_gui_exe();
    Command::new(&exe)
        .arg("--hud")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn open_gui_search() {
    let exe = resolve_clipd_gui_exe();
    eprintln!("clipd-ui: opening GUI search from {}", exe.display());
    let _ = Command::new(&exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Windows: open the TUI search in a fresh console window.
#[cfg(target_os = "windows")]
fn open_search_in_terminal() {
    let exe = resolve_clipd_exe();
    let _ = Command::new("cmd")
        .args([
            "/C",
            "start",
            "clipd search",
            &exe.to_string_lossy(),
            "search",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Linux: best-effort default terminal.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_search_in_terminal() {
    let exe = resolve_clipd_exe();
    let _ = Command::new("x-terminal-emulator")
        .arg("-e")
        .arg(format!("{} search", exe.to_string_lossy()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(target_os = "macos")]
fn open_search_in_terminal() {
    let exe = resolve_clipd_exe();
    let exe_str = exe.to_string_lossy().to_string();
    let cmd = format!("cd /tmp && {} search", exe_str);

    let warp_script = format!(
        r#"tell application "Warp"
  activate
  delay 0.3
  tell application "System Events"
    keystroke "t" using command down
    delay 0.4
    keystroke "{}"
    delay 0.1
    key code 36
  end tell
end tell"#,
        cmd
    );
    let warp_result = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&warp_script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if warp_result.map_or(true, |s| !s.success()) {
        let terminal_script = format!(
            "tell application \"Terminal\"\n  activate\n  do script \"{}\"\nend tell",
            cmd
        );
        let _ = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&terminal_script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

// ── Logging ──

/// Route the in-process daemon's `log::*` output to the same file the old
/// child-process daemon wrote to (`~/Library/Logs/clipd-ui-daemon.log`), so
/// existing troubleshooting steps keep working.
fn init_logging() {
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log_path())
    {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .format_timestamp(None)
            .format_target(false)
            .target(env_logger::Target::Pipe(Box::new(file)))
            .try_init();
    }
}

// ── Daemon management ──

/// Handle to the in-process daemon: a shared stop flag plus its worker thread.
struct DaemonHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl DaemonHandle {
    /// Signal the daemon to wind down. Join on a helper thread so the menu-bar
    /// event loop never freezes if macOS's keyboard hook takes time to unwind.
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = std::thread::Builder::new()
                .name("clipd-ui-daemon-stop".into())
                .spawn(move || {
                    let _ = join.join();
                });
        }
    }
}

/// Start the daemon **inside this process** on a background thread.
///
/// The macOS keyboard listener (rdev) must run in the binary that actually
/// holds the Input Monitoring / Accessibility grants. clipd-ui is that binary
/// (it's `Clipd.app`'s `CFBundleExecutable`), so hosting the daemon here — rather
/// than spawning a separate `clipd daemon` child — is what makes multi-slot
/// copy and the HUD work under ad-hoc signing.
fn start_daemon() -> DaemonHandle {
    // Kill any stale *external* `clipd daemon` process so the PID lock is free
    // and only one keyboard tap is ever active.
    stop_existing_daemons();
    std::thread::sleep(std::time::Duration::from_millis(150));

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = std::thread::Builder::new()
        .name("clipd-ui-daemon".into())
        .spawn(move || {
            if let Err(e) = clipd_daemon::run_daemon_with_stop(stop_thread, false) {
                log::error!("clipd-ui: in-process daemon exited with error: {e}");
            }
        })
        .ok();

    DaemonHandle { stop, join }
}

#[cfg(not(target_os = "windows"))]
fn stop_existing_daemons() {
    let _ = Command::new("/usr/bin/pkill")
        .arg("-f")
        .arg("clipd daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "windows")]
fn stop_existing_daemons() {
    // Kill a stale external `clipd.exe daemon`. The CLI shares the image name
    // but is short-lived, so force-killing by name is an acceptable sweep.
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "clipd.exe"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Also sweep stray clipd-ui.exe instances from earlier sessions (each
    // hosts an in-process daemon → duplicate hooks and split slot state).
    // Filter excludes ourselves.
    let self_pid = std::process::id().to_string();
    let _ = Command::new("taskkill")
        .args([
            "/F",
            "/IM",
            "clipd-ui.exe",
            "/FI",
            &format!("PID ne {}", self_pid),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// ── Path resolution ──

/// Same directory as clipd-ui (e.g. Clipd.app/Contents/MacOS/) — release / .app bundles.
fn resolve_sibling_exe(names: &[&str]) -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for name in names {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn resolve_clipd_exe() -> PathBuf {
    #[cfg(target_os = "windows")]
    let clipd_names = ["clipd.exe", "clipd"];
    #[cfg(not(target_os = "windows"))]
    let clipd_names = ["clipd"];

    if let Some(p) = resolve_sibling_exe(&clipd_names) {
        return p;
    }

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let dev = workspace_root.join("target/debug/clipd");
    if dev.exists() {
        return dev;
    }

    let rel = workspace_root.join("target/release/clipd");
    if rel.exists() {
        return rel;
    }

    let cargo_bin = PathBuf::from("/Users/shwetakadam/.cargo/bin/clipd");
    if cargo_bin.exists() {
        return cargo_bin;
    }

    PathBuf::from("clipd")
}

fn resolve_clipd_gui_exe() -> PathBuf {
    #[cfg(target_os = "windows")]
    let gui_names = ["clipd-gui.exe", "clipd-gui"];
    #[cfg(not(target_os = "windows"))]
    let gui_names = ["clipd-gui"];

    if let Some(p) = resolve_sibling_exe(&gui_names) {
        return p;
    }

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let dev = workspace_root.join("target/debug/clipd-gui");
    if dev.exists() {
        return dev;
    }

    let rel = workspace_root.join("target/release/clipd-gui");
    if rel.exists() {
        return rel;
    }

    if let Some(home) = dirs::home_dir() {
        let cargo_bin = home.join(".cargo/bin/clipd-gui");
        if cargo_bin.exists() {
            return cargo_bin;
        }
    }

    PathBuf::from("clipd-gui")
}

fn daemon_log_path() -> PathBuf {
    let logs_dir = if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Logs")
    } else {
        // Windows: %LOCALAPPDATA%\clipd\logs · Linux: ~/.local/share/clipd/logs
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("clipd")
            .join("logs")
    };
    let _ = std::fs::create_dir_all(&logs_dir);
    logs_dir.join("clipd-ui-daemon.log")
}

/// Claim the one automatic keyboard-permission offer for this Clipd version.
///
/// The file is intentionally persistent across tray restarts. A denied or
/// not-yet-settled TCC grant should not turn a watchdog restart into an endless
/// series of macOS permission sheets. Users can always retry deliberately from
/// the tray's keyboard-access item or the Settings screen.
#[cfg(target_os = "macos")]
fn claim_keyboard_permission_offer() -> bool {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let marker = dir.join(format!(
        "keyboard-permission-offered-{}",
        env!("CARGO_PKG_VERSION")
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .is_ok()
}

/// macOS menu-bar template: pure black strokes on transparent. The system
/// recolors it for light/dark (and wallpaper-tinted) menu bars — a gold tile
/// with `template=false` could vanish when a dark app like Sublime is focused.
/// The menu-bar icon: clipd's cat, the same art the island and palette use.
///
/// It used to be a hand-plotted tile with a clipboard glyph, which meant the
/// one piece of clipd that is always on screen was the only piece not wearing
/// the product's own mark. Rendered onto the accent tile so it still reads as
/// a solid target at 22 points rather than a thin sketch.
static CAT_PNG: &[u8] = include_bytes!("../assets/cat.png");

fn make_icon() -> Icon {
    let cat_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/cat.png");
    if cat_path.exists() {
        if let Ok(data) = std::fs::read(&cat_path) {
            if let Ok(img) = image::load_from_memory(&data) {
                // Crop to the non-transparent bounding box, then resize to
                // fill the icon — no transparent padding, no background.
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();

                // Find bounding box of non-transparent pixels.
                let mut min_x = w;
                let mut min_y = h;
                let mut max_x = 0u32;
                let mut max_y = 0u32;
                for y in 0..h {
                    for x in 0..w {
                        if rgba.get_pixel(x, y).0[3] > 10 {
                            if x < min_x { min_x = x; }
                            if x > max_x { max_x = x; }
                            if y < min_y { min_y = y; }
                            if y > max_y { max_y = y; }
                        }
                    }
                }

                if max_x >= min_x && max_y >= min_y {
                    // Crop to the cat, then resize to 44x44 (Retina menu bar).
                    let cropped = image::imageops::crop_imm(
                        &rgba,
                        min_x,
                        min_y,
                        max_x - min_x + 1,
                        max_y - min_y + 1,
                    )
                    .to_image();

                    // Create a 44x44 RGBA buffer and center the cropped cat
                    // with fully transparent background (alpha=0), so no
                    // background color shows through on any theme.
                    let target_size = 44u32;
                    let mut buf = image::RgbaImage::new(target_size, target_size);
                    // Fill with transparent.
                    for px in buf.pixels_mut() {
                        *px = image::Rgba([0, 0, 0, 0]);
                    }

                    // Compute centered placement.
                    let cw = cropped.width();
                    let ch = cropped.height();
                    let resized = image::imageops::resize(
                        &cropped,
                        target_size,
                        target_size,
                        image::imageops::FilterType::Lanczos3,
                    );
                    // Overwrite buf with the resized image (which preserves
                    // transparency from the crop).
                    image::imageops::overlay(&mut buf, &resized, 0, 0);

                    // Snap alpha: any pixel < 50% transparent becomes fully
                    // transparent, any pixel >= 50% becomes fully opaque.
                    // This prevents semi-transparent edge pixels from showing
                    // the theme background color through the icon.
                    for px in buf.pixels_mut() {
                        let a = px.0[3];
                        if a >= 128 {
                            px.0[3] = 255;
                        } else {
                            px.0[3] = 0;
                        }
                    }

                    let pixels = buf.into_raw();
                    if let Ok(icon) = Icon::from_rgba(pixels, target_size, target_size) {
                        return icon;
                    }
                }
            }
        }
    }
    make_icon_for(clipd_core::load_theme().colors())
}

/// The icon in a given theme, so it can be rebuilt when the theme changes.
///
/// The tile is the theme's raised surface, not its accent. Painting it with
/// the accent worked while accents were colourful, but a theme whose accent is
/// deliberately near-neutral — the glass pair — put a near-white tile in the
/// menu bar above a near-black island. The tray icon should read as a small
/// piece of the same app, so it takes the same surface the island slab does
/// and keeps the accent for a thin ring.
fn make_icon_for(c: clipd_core::ThemeColors) -> Icon {
    // 64px, not 32. The menu bar is ~22pt, which is 44px on a Retina display,
    // so a 32px source was being scaled *up* — the blur was in the asset, not
    // in macOS.
    const S: u32 = 64;
    let mut rgba = vec![0u8; (S * S * 4) as usize];

    // Rounded accent tile, so the icon holds a shape in a busy menu bar.
    let clipd_core::Rgb(tr, tg, tb) = c.bg_elevated;
    let accent = [tr, tg, tb, 255];
    let clipd_core::Rgb(rr, rg, rb) = c.accent;
    let ring = [rr, rg, rb, 150];
    let (lo, hi, r) = (4i32, 59i32, 14i32);
    let outside = |x: i32, y: i32, cx: i32, cy: i32| (x - cx).pow(2) + (y - cy).pow(2) > r * r;
    for y in lo..=hi {
        for x in lo..=hi {
            let skip = (x < lo + r && y < lo + r && outside(x, y, lo + r, lo + r))
                || (x > hi - r && y < lo + r && outside(x, y, hi - r, lo + r))
                || (x < lo + r && y > hi - r && outside(x, y, lo + r, hi - r))
                || (x > hi - r && y > hi - r && outside(x, y, hi - r, hi - r));
            if !skip {
                let i = ((y as u32 * S + x as u32) * 4) as usize;
                // A hairline of accent around the edge: a dark tile on a dark
                // menu bar needs an edge, and this is the one place the theme's
                // colour still belongs.
                let edge = x <= lo + 1 || x >= hi - 1 || y <= lo + 1 || y >= hi - 1;
                rgba[i..i + 4].copy_from_slice(if edge { &ring } else { &accent });
            }
        }
    }

    if let Ok((cw, ch, cat)) = clipd_core::decode_rgba(CAT_PNG) {
        let scale = (52.0 / cw as f32).min(46.0 / ch as f32);
        let (dw, dh) = ((cw as f32 * scale) as u32, (ch as f32 * scale) as u32);
        let (ox, oy) = ((S - dw) / 2, (S - dh) / 2);
        // Box filter rather than nearest neighbour. Point-sampling a 192px
        // drawing down to fifty-odd pixels drops whole whiskers and leaves
        // the edges ragged; averaging the block each pixel covers keeps the
        // line weight even.
        let step_x = cw as f32 / dw as f32;
        let step_y = ch as f32 / dh as f32;
        for y in 0..dh {
            for x in 0..dw {
                let (x0, x1) = ((x as f32 * step_x) as u32, ((x + 1) as f32 * step_x) as u32);
                let (y0, y1) = ((y as f32 * step_y) as u32, ((y + 1) as f32 * step_y) as u32);
                let (mut acc, mut n) = ([0u32; 4], 0u32);
                for sy in y0..y1.max(y0 + 1).min(ch) {
                    for sx in x0..x1.max(x0 + 1).min(cw) {
                        let si = ((sy * cw + sx) * 4) as usize;
                        // Weight colour by alpha so transparent pixels do not
                        // drag the edges toward black.
                        let a = cat[si + 3] as u32;
                        for k in 0..3 {
                            acc[k] += cat[si + k] as u32 * a;
                        }
                        acc[3] += a;
                        n += 1;
                    }
                }
                if n == 0 || acc[3] == 0 {
                    continue;
                }
                let a = acc[3] / n;
                let di = (((y + oy) * S + (x + ox)) * 4) as usize;
                for k in 0..3 {
                    let src = acc[k] / acc[3];
                    let dst = rgba[di + k] as u32;
                    rgba[di + k] = ((src * a + dst * (255 - a)) / 255) as u8;
                }
                rgba[di + 3] = 255;
            }
        }
    }

    Icon::from_rgba(rgba, S, S).expect("failed to create tray icon")
}

/// Keep clipd-ui as a menu-bar agent so the status item is not tied to a
/// regular app activation cycle (Dock / Cmd-Tab), which is when macOS has been
/// observed to drop coloured status items after focusing Sublime Text.
#[cfg(target_os = "macos")]
fn adopt_accessory_activation_policy() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if std::env::var("CLIPD_NO_ACCESSORY").is_ok() {
        log::info!("clipd-ui: accessory policy skipped (CLIPD_NO_ACCESSORY)");
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("clipd-ui: not on the main thread — activation policy not set");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}

fn tray_event_id(ev: &TrayIconEvent) -> Option<&TrayIconId> {
    match ev {
        TrayIconEvent::Click { id, .. }
        | TrayIconEvent::DoubleClick { id, .. }
        | TrayIconEvent::Enter { id, .. }
        | TrayIconEvent::Move { id, .. }
        | TrayIconEvent::Leave { id, .. } => Some(id),
        _ => None,
    }
}

/// Apps whose menu titles routinely shove leftmost status items off-screen.
#[cfg(target_os = "macos")]
fn frontmost_app_is_menu_heavy() -> bool {
    use objc2_app_kit::NSWorkspace;

    let Some(app) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
        return false;
    };
    let Some(name) = app.localizedName() else {
        return false;
    };
    let n = name.to_string().to_ascii_lowercase();
    // Sublime is the reported culprit (Find/Goto/Tools/Project). Xcode and
    // JetBrains IDEs have similarly wide menu bars.
    n.contains("sublime")
        || n.contains("xcode")
        || n.contains("intellij")
        || n.contains("webstorm")
        || n.contains("pycharm")
        || n.contains("android studio")
}

/// Build near-invisible status items parked to the left of Clipd. macOS hides
/// status items from the left when space is tight, so these get culled first.
#[cfg(target_os = "macos")]
/// Width of the notch on the main display, in points, if it has one.
///
/// macOS parks the leftmost menu-bar item *behind* the camera housing rather
/// than dropping it, so on a notched Mac the newest status item — clipd's —
/// is created invisible. It is present and clickable-in-theory, just under an
/// opaque piece of aluminium.
#[cfg(target_os = "macos")]
fn notch_width() -> Option<f32> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let mtm = MainThreadMarker::new()?;
    let screen = NSScreen::mainScreen(mtm)?;
    if screen.safeAreaInsets().top <= 0.0 {
        return None;
    }
    let frame = screen.frame();
    let left = screen.auxiliaryTopLeftArea();
    let right = screen.auxiliaryTopRightArea();
    let width = (frame.size.width - left.size.width - right.size.width) as f32;
    (60.0..=520.0).contains(&width).then_some(width)
}

/// How many blank spacers it takes to fill the notch.
///
/// The spacers are created *after* the clipd item, so macOS puts them to its
/// left — they take the hidden slots and clipd shifts right into daylight.
/// Nothing else on the bar moves, because everything else is already to the
/// right of clipd.
#[cfg(target_os = "macos")]
fn shields_for_notch() -> usize {
    // Off by default. Each shield is a real status item, so parking 17 of them
    // means the menu bar has to find room for eighteen items instead of one —
    // and when it runs out, macOS drops items from the left, which is where
    // clipd's own icon sits. The cure was removing the icon it was meant to
    // protect. Set CLIPD_NOTCH_SHIELDS=<n> to bring them back.
    let Ok(want) = std::env::var("CLIPD_NOTCH_SHIELDS") else {
        return 0;
    };
    if want.trim() != "auto" {
        return want.trim().parse().unwrap_or(0);
    }
    match notch_width() {
        // Each thin-space item is roughly 24pt of bar, plus one to spare.
        // A blank status item is narrower than a real one, so this is a
        // generous over-estimate on purpose: spare spacers are invisible and
        // cost nothing, while one too few leaves the icon behind the housing.
        Some(width) => ((width / 12.0).ceil() as usize + 2).clamp(1, 22),
        None => 0,
    }
}

fn build_menu_bar_shields_n(count: usize) -> Vec<TrayIcon> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        // Thin space title reserves a few points of width without a visible glyph.
        let built = TrayIconBuilder::new()
            .with_id(format!("clipd-shield-{i}"))
            .with_title("\u{2009}")
            .with_tooltip("")
            .with_menu_on_left_click(false)
            .build();
        match built {
            Ok(icon) => out.push(icon),
            Err(e) => {
                log::warn!("Failed to create menu-bar shield {i}: {e}");
                break;
            }
        }
    }
    out
}

/// Shields exist to fight a macOS-specific behaviour — the system hiding
/// status items from the left when app menus need room. The count they use is
/// already macOS-only; this wrapper was not, so a Windows build could not
/// resolve it.
#[cfg(target_os = "macos")]
fn build_menu_bar_shields() -> Vec<TrayIcon> {
    build_menu_bar_shields_n(MENU_BAR_SHIELD_COUNT)
}

#[cfg(not(target_os = "macos"))]
fn build_menu_bar_shields() -> Vec<TrayIcon> {
    Vec::new()
}
