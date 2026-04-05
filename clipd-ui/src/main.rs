use std::path::PathBuf;
use std::fs::OpenOptions;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

use clipd_core::{load_paste_transform_settings, save_paste_transform_settings};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, CheckMenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, MouseButtonState, TrayIconEvent};

const MENU_ID_START: &str = "start";
const MENU_ID_STOP: &str = "stop";
const MENU_ID_SEARCH: &str = "search";
const MENU_ID_HUD: &str = "hud_notifications";
const MENU_ID_TUI_MODE: &str = "tui_mode";
const MENU_ID_QUIT: &str = "quit";

fn hud_tray_label(hud_on: bool) -> String {
    if hud_on {
        "HUD slot overlay: On — click to turn off".to_string()
    } else {
        "HUD slot overlay: Off — click to turn on".to_string()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new();
    let wake_proxy = event_loop.create_proxy();
    let (tray_tx, tray_rx) = mpsc::channel::<TrayIconEvent>();
    TrayIconEvent::set_event_handler(Some(move |ev| {
        let _ = tray_tx.send(ev);
        let _ = wake_proxy.send_event(());
    }));

    let menu = Menu::new();
    let item_start = MenuItem::with_id(MENU_ID_START, "Start clipd daemon", true, None);
    let item_stop = MenuItem::with_id(MENU_ID_STOP, "Stop clipd daemon", false, None);
    let item_search = MenuItem::with_id(MENU_ID_SEARCH, "Open clipd search", true, None);
    let hud_on = load_paste_transform_settings().hud_enabled;
    // Plain MenuItem (not CheckMenuItem): macOS tray checkmarks were drifting from
    // `paste_transform.json`; explicit on/off text + toggle keeps daemon and UI aligned.
    let item_hud = MenuItem::with_id(MENU_ID_HUD, hud_tray_label(hud_on), true, None);
    let item_tui_mode = CheckMenuItem::with_id(
        MENU_ID_TUI_MODE,
        "Developer mode (TUI)",
        true,
        load_tui_mode(),
        None,
    );
    let item_quit = MenuItem::with_id(MENU_ID_QUIT, "Quit clipd UI", true, None);

    menu.append(&item_start)?;
    menu.append(&item_stop)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&item_search)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&item_hud)?;
    menu.append(&item_tui_mode)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&item_quit)?;

    let _tray_icon = TrayIconBuilder::new()
        .with_tooltip("clipd ui")
        .with_menu(Box::new(menu))
        .with_icon(make_icon())
        .build()?;

    let menu_channel = MenuEvent::receiver();

    // Auto-start daemon on launch
    let mut daemon: Option<Child> = match start_daemon() {
        Ok(child) => {
            item_start.set_enabled(false);
            item_stop.set_enabled(true);
            Some(child)
        }
        Err(e) => {
            eprintln!("clipd-ui: failed to auto-start daemon: {e}");
            None
        }
    };

    // Open main window when not in developer (TUI) mode — matches “GUI + tray” expectation.
    if daemon.is_some() && !load_tui_mode() {
        open_gui_search();
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::UserEvent(()) = event {
            while let Ok(tray_ev) = tray_rx.try_recv() {
                match tray_ev {
                    TrayIconEvent::Enter { .. } => {
                        let s = load_paste_transform_settings();
                        item_hud.set_text(hud_tray_label(s.hud_enabled));
                        item_tui_mode.set_checked(load_tui_mode());
                    }
                    TrayIconEvent::Click { button_state, .. }
                        if matches!(
                            button_state,
                            MouseButtonState::Down | MouseButtonState::Up
                        ) =>
                    {
                        let s = load_paste_transform_settings();
                        item_hud.set_text(hud_tray_label(s.hud_enabled));
                        item_tui_mode.set_checked(load_tui_mode());
                    }
                    _ => {}
                }
            }
        }

        if let Event::NewEvents(_) = event {
            if let Ok(menu_event) = menu_channel.try_recv() {
                match menu_event.id.0.as_str() {
                    MENU_ID_START => {
                        if daemon.is_none() {
                            match start_daemon() {
                                Ok(child) => {
                                    daemon = Some(child);
                                    item_start.set_enabled(false);
                                    item_stop.set_enabled(true);
                                }
                                Err(e) => eprintln!("clipd-ui: failed to start daemon: {e}"),
                            }
                        }
                    }
                    MENU_ID_STOP => {
                        if let Some(mut child) = daemon.take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        stop_existing_daemons();
                        item_start.set_enabled(true);
                        item_stop.set_enabled(false);
                    }
                    MENU_ID_SEARCH => {
                        if item_tui_mode.is_checked() {
                            open_search_in_terminal();
                        } else {
                            open_gui_search();
                        }
                    }
                    MENU_ID_HUD => {
                        let mut s = load_paste_transform_settings();
                        s.hud_enabled = !s.hud_enabled;
                        save_paste_transform_settings(&s);
                        item_hud.set_text(hud_tray_label(s.hud_enabled));
                    }
                    MENU_ID_TUI_MODE => {
                        save_tui_mode(item_tui_mode.is_checked());
                    }
                    MENU_ID_QUIT => {
                        if let Some(mut child) = daemon.take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        stop_existing_daemons();
                        *control_flow = ControlFlow::Exit;
                    }
                    _ => {}
                }
            }
        }
    });
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

fn open_gui_search() {
    let exe = resolve_clipd_gui_exe();
    eprintln!("clipd-ui: opening GUI search from {}", exe.display());
    let _ = Command::new(&exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

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

// ── Daemon management ──

fn start_daemon() -> Result<Child, Box<dyn std::error::Error>> {
    stop_existing_daemons();

    let exe = resolve_clipd_exe();
    eprintln!("clipd-ui: launching daemon from {}", exe.display());

    let log_path = daemon_log_path();
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_file_err = log_file.try_clone()?;

    let child = Command::new(exe)
        .arg("daemon")
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()?;
    Ok(child)
}

fn stop_existing_daemons() {
    let _ = Command::new("/usr/bin/pkill")
        .arg("-f")
        .arg("clipd daemon")
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
    let logs_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    logs_dir.join("clipd-ui-daemon.log")
}

fn make_icon() -> Icon {
    let width = 16u32;
    let height = 16u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for y in 3..13 {
        for x in 6..10 {
            if x == 6 || x == 9 || y == 3 || y == 12 {
                let idx = ((y * width + x) * 4) as usize;
                rgba[idx] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = 255;
            }
        }
    }

    Icon::from_rgba(rgba, width, height).expect("failed to create tray icon")
}
