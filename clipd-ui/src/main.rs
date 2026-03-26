use std::path::PathBuf;
use std::fs::OpenOptions;
use std::process::{Child, Command, Stdio};

use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};

const MENU_ID_START: &str = "start";
const MENU_ID_STOP: &str = "stop";
const MENU_ID_SEARCH: &str = "search";
const MENU_ID_QUIT: &str = "quit";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new();

    let menu = Menu::new();
    let item_start = MenuItem::with_id(MENU_ID_START, "Start clipd daemon", true, None);
    let item_stop = MenuItem::with_id(MENU_ID_STOP, "Stop clipd daemon", false, None);
    let item_search = MenuItem::with_id(MENU_ID_SEARCH, "Open clipd search", true, None);
    let item_quit = MenuItem::with_id(MENU_ID_QUIT, "Quit clipd UI", true, None);

    menu.append_items(&[&item_start, &item_stop, &item_search, &item_quit])?;

    let _tray_icon = TrayIconBuilder::new()
        .with_tooltip("clipd ui")
        .with_menu(Box::new(menu))
        .with_icon(make_icon())
        .build()?;

    let menu_channel = MenuEvent::receiver();
    let mut daemon: Option<Child> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

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
                                Err(e) => eprintln!("❌ Failed to start daemon: {e}"),
                            }
                        }
                    }
                    MENU_ID_STOP => {
                        if let Some(mut child) = daemon.take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        item_start.set_enabled(true);
                        item_stop.set_enabled(false);
                    }
                    MENU_ID_SEARCH => {
                        let exe = resolve_clipd_exe();
                        let _ = Command::new(exe)
                            .arg("search")
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn();
                    }
                    MENU_ID_QUIT => {
                        if let Some(mut child) = daemon.take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        *control_flow = ControlFlow::Exit;
                    }
                    _ => {}
                }
            }
        }
    });
}

fn start_daemon() -> Result<Child, Box<dyn std::error::Error>> {
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

fn resolve_clipd_exe() -> PathBuf {
    // 1) Prefer workspace dev build (cargo run flow)
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dev_clipd = workspace_root.join("target/debug/clipd");
    if dev_clipd.exists() {
        return dev_clipd;
    }

    // 2) Then workspace release build
    let rel_clipd = workspace_root.join("target/release/clipd");
    if rel_clipd.exists() {
        return rel_clipd;
    }

    // 3) Then installed cargo binary
    let cargo_bin = PathBuf::from("/Users/shwetakadam/.cargo/bin/clipd");
    if cargo_bin.exists() {
        return cargo_bin;
    }

    // 4) Fallback to PATH
    PathBuf::from("clipd")
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

    // Simple paperclip-like white glyph on transparent background.
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
