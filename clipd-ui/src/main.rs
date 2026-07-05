use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use clipd_core::{load_paste_transform_settings, save_paste_transform_settings};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButtonState, TrayIconBuilder, TrayIconEvent};

const MENU_ID_START: &str = "start";
const MENU_ID_STOP: &str = "stop";
const MENU_ID_SEARCH: &str = "search";
const MENU_ID_HUD: &str = "hud_notifications";
const MENU_ID_TUI_MODE: &str = "tui_mode";
const MENU_ID_QUIT: &str = "quit";

fn hud_tray_label(hud_on: bool) -> String {
    // macOS shows the Swift HUD overlay; Windows shows overlay/toast
    // notifications — same setting, platform-accurate name.
    let what = if cfg!(target_os = "macos") {
        "HUD slot overlay"
    } else {
        "Slot notifications"
    };
    if hud_on {
        format!("{}: On — click to turn off", what)
    } else {
        format!("{}: Off — click to turn on", what)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

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
    let item_search = MenuItem::with_id(MENU_ID_SEARCH, "Open clipd search gui", true, None);
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
        // Show the gold tile in color (not a monochrome template) so it matches
        // the clipboard icon in the GUI window header.
        .with_icon_as_template(false)
        .build()?;

    let menu_channel = MenuEvent::receiver();

    // Auto-start daemon on launch — runs IN-PROCESS (see start_daemon docs) so the
    // macOS keyboard listener inherits clipd-ui's Input Monitoring / Accessibility grants.
    let mut daemon: Option<DaemonHandle> = Some(start_daemon());
    item_start.set_enabled(false);
    item_stop.set_enabled(true);

    // Open main window when not in developer (TUI) mode — matches “GUI + tray” expectation.
    if !load_tui_mode() && std::env::var("CLIPD_NO_AUTO_OPEN_GUI").ok().as_deref() != Some("1") {
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
                    TrayIconEvent::Click {
                        button,
                        button_state,
                        ..
                    } if matches!(
                        button_state,
                        MouseButtonState::Down | MouseButtonState::Up
                    ) =>
                    {
                        let s = load_paste_transform_settings();
                        item_hud.set_text(hud_tray_label(s.hud_enabled));
                        item_tui_mode.set_checked(load_tui_mode());
                        // Windows convention: left-click the tray icon opens the
                        // app; the menu stays on right-click. (On macOS the menu
                        // opens on any click, so this never fires there.)
                        #[cfg(target_os = "windows")]
                        if button == tray_icon::MouseButton::Left
                            && button_state == MouseButtonState::Up
                        {
                            open_gui_search();
                        }
                        #[cfg(not(target_os = "windows"))]
                        let _ = button;
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
                            daemon = Some(start_daemon());
                            item_start.set_enabled(false);
                            item_stop.set_enabled(true);
                        }
                    }
                    MENU_ID_STOP => {
                        if let Some(mut handle) = daemon.take() {
                            handle.stop();
                        }
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
                        if let Some(mut handle) = daemon.take() {
                            handle.stop();
                        }
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

/// Windows: open the TUI search in a fresh console window.
#[cfg(target_os = "windows")]
fn open_search_in_terminal() {
    let exe = resolve_clipd_exe();
    let _ = Command::new("cmd")
        .args(["/C", "start", "clipd search", &exe.to_string_lossy(), "search"])
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
    /// Signal the daemon to wind down and wait for its worker thread to finish.
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
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

fn make_icon() -> Icon {
    // Gold rounded tile with a white clipboard.
    let s = 32u32;
    let mut rgba = vec![0u8; (s * s * 4) as usize];
    let set = |x: i32, y: i32, col: (u8, u8, u8, u8), rgba: &mut Vec<u8>| {
        if (0..s as i32).contains(&x) && (0..s as i32).contains(&y) {
            let i = ((y as u32 * s + x as u32) * 4) as usize;
            rgba[i] = col.0;
            rgba[i + 1] = col.1;
            rgba[i + 2] = col.2;
            rgba[i + 3] = col.3;
        }
    };
    let gold = (255u8, 160, 50, 255);
    let white = (255u8, 255, 255, 255);

    let (lo, hi, r) = (3i32, 28i32, 7i32);
    let outside_corner =
        |x: i32, y: i32, cx: i32, cy: i32| (x - cx) * (x - cx) + (y - cy) * (y - cy) > r * r;
    for y in lo..=hi {
        for x in lo..=hi {
            let skip = (x < lo + r && y < lo + r && outside_corner(x, y, lo + r, lo + r))
                || (x > hi - r && y < lo + r && outside_corner(x, y, hi - r, lo + r))
                || (x < lo + r && y > hi - r && outside_corner(x, y, lo + r, hi - r))
                || (x > hi - r && y > hi - r && outside_corner(x, y, hi - r, hi - r));
            if !skip {
                set(x, y, gold, &mut rgba);
            }
        }
    }
    for x in 11..=20 {
        set(x, 10, white, &mut rgba);
        set(x, 23, white, &mut rgba);
    }
    for y in 10..=23 {
        set(11, y, white, &mut rgba);
        set(20, y, white, &mut rgba);
    }
    for x in 13..=18 {
        set(x, 7, white, &mut rgba);
        set(x, 10, white, &mut rgba);
    }
    set(13, 8, white, &mut rgba);
    set(13, 9, white, &mut rgba);
    set(18, 8, white, &mut rgba);
    set(18, 9, white, &mut rgba);
    for x in 13..=18 {
        set(x, 15, white, &mut rgba);
        set(x, 19, white, &mut rgba);
    }

    Icon::from_rgba(rgba, s, s).expect("failed to create tray icon")
}
