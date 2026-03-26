use arboard::Clipboard;
use clipd_core::{ClipEvent, ClipStore, ClipWatcher, SlotManager};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::collections::HashMap;
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

/// Run the clipd daemon. This function blocks forever (until SIGINT).
///
/// Responsibilities:
/// 1. Start clipboard watcher in a background thread
/// 2. Register global hotkeys for multi-slot copy/paste
/// 3. Run the macOS event loop on the main thread
pub fn run_daemon() -> Result<(), Box<dyn std::error::Error>> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║         clipd daemon v0.1.0           ║");
    println!("  ║   AI clipboard for developers         ║");
    println!("  ╚═══════════════════════════════════════╝");
    println!();

    // Initialize store
    let db_path = ClipStore::default_path();
    println!("  📦 Database: {}", db_path.display());
    // Validate the store can be opened (also runs migrations)
    let _store = ClipStore::new(&db_path)?;
    let slot_manager = SlotManager::new();
    let db_path_clone = db_path.clone();

    // Stop signal
    let stop = Arc::new(AtomicBool::new(false));
    let stop_watcher = stop.clone();
    let stop_hotkey = stop.clone();

    // Set up Ctrl+C handler
    let stop_ctrlc = stop.clone();
    setup_ctrlc(stop_ctrlc);

    // ── Clipboard Watcher Thread ──
    let (clip_tx, clip_rx) = mpsc::channel::<ClipEvent>();
    let watcher = ClipWatcher::new(500);
    let watcher_handle = std::thread::Builder::new()
        .name("clipd-watcher".into())
        .spawn(move || {
            watcher.watch(clip_tx, stop_watcher);
        })?;

    // ── Store Writer Thread ──
    // Each thread gets its own SQLite connection (rusqlite Connection is not Sync)
    let slot_writer = slot_manager.clone();
    let store_handle = std::thread::Builder::new()
        .name("clipd-store-writer".into())
        .spawn(move || {
            let store = match ClipStore::new(&db_path_clone) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("Failed to open store in writer thread: {}", e);
                    return;
                }
            };

            for event in clip_rx {
                match event {
                    ClipEvent::NewClip(entry) => {
                        // Always keep slot 0 in sync with OS clipboard
                        slot_writer
                            .copy_to_slot(0, entry.content.clone())
                            .ok();

                        match store.insert(&entry) {
                            Ok(id) => {
                                log::info!(
                                    "Saved clip #{}: {} [{}] {}",
                                    id,
                                    entry.content_type.icon(),
                                    entry.content_type.as_str(),
                                    truncate(&entry.preview, 60)
                                );
                            }
                            Err(e) => {
                                log::error!("Failed to save clip: {}", e);
                            }
                        }
                    }
                }
            }
        })?;

    // ── Global Hotkeys ──
    let hotkey_manager = GlobalHotKeyManager::new()?;
    let mut registered_hotkeys: Vec<(HotKey, HotkeyAction)> = Vec::new();

    // Register Cmd+Shift+1..9 for "copy to slot N"
    // Register Cmd+Option+1..9 for "paste from slot N"
    let digit_codes = [
        Code::Digit1,
        Code::Digit2,
        Code::Digit3,
        Code::Digit4,
        Code::Digit5,
        Code::Digit6,
        Code::Digit7,
        Code::Digit8,
        Code::Digit9,
    ];

    for (i, code) in digit_codes.iter().enumerate() {
        let slot_num = (i + 1) as u8;

        // Cmd+Shift+N → copy to slot N
        let copy_hk = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), *code);
        if let Err(e) = hotkey_manager.register(copy_hk) {
            log::warn!("Failed to register Cmd+Shift+{}: {}", slot_num, e);
        } else {
            registered_hotkeys.push((copy_hk, HotkeyAction::CopyToSlot(slot_num)));
        }

        // Cmd+Option+N → paste from slot N
        let paste_hk = HotKey::new(Some(Modifiers::SUPER | Modifiers::ALT), *code);
        if let Err(e) = hotkey_manager.register(paste_hk) {
            log::warn!("Failed to register Cmd+Option+{}: {}", slot_num, e);
        } else {
            registered_hotkeys.push((paste_hk, HotkeyAction::PasteFromSlot(slot_num)));
        }
    }

    // Cmd+Shift+V → open TUI search
    let tui_hk = HotKey::new(
        Some(Modifiers::SUPER | Modifiers::SHIFT),
        Code::KeyV,
    );
    if let Err(e) = hotkey_manager.register(tui_hk) {
        log::warn!("Failed to register Cmd+Shift+V: {}", e);
    } else {
        registered_hotkeys.push((tui_hk, HotkeyAction::OpenTui));
    }

    println!("  ⌨️  Hotkeys registered:");
    println!("     Cmd+Shift+1..9  → copy to slot");
    println!("     Cmd+Option+1..9 → paste from slot");
    println!("     Cmd+Shift+V     → open search TUI");
    println!();
    println!("  👀 Watching clipboard... (Ctrl+C to stop)");
    println!();

    // ── Main Event Loop ──
    //
    // On macOS, `global-hotkey` expects a main-thread event loop.
    // Using `set_event_handler` + `CFRunLoopRun()` avoids fragile "poll/pump" logic.
    #[cfg(target_os = "macos")]
    {
        println!("  🪄 Starting CFRunLoopRun (hotkeys dispatch)...");

        let mut actions_by_id: HashMap<u32, HotkeyAction> = HashMap::new();
        for (hk, action) in &registered_hotkeys {
            actions_by_id.insert(hk.id(), action.clone());
        }

        let hotkey_slot_mgr = slot_manager.clone();

        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            log::info!("⌨️ Hotkey event: id={}, state={:?}", event.id(), event.state);

            // Only act on key-down.
            if event.state != global_hotkey::HotKeyState::Pressed {
                return;
            }

            let Some(action) = actions_by_id.get(&event.id()) else {
                return;
            };

            match action {
                HotkeyAction::CopyToSlot(slot) => {
                    match copy_selection_into_slot(*slot, &hotkey_slot_mgr) {
                        Ok(Some(text)) => log::info!(
                            "📋 Copied to slot {}: {}",
                            slot,
                            truncate(&text, 40)
                        ),
                        Ok(None) => log::info!("📋 Copy to slot {} skipped (empty)", slot),
                        Err(e) => log::warn!("Copy to slot {} failed: {}", slot, e),
                    }
                }
                HotkeyAction::PasteFromSlot(slot) => {
                    match hotkey_slot_mgr.get_slot(*slot) {
                        Ok(Some(content)) => {
                            if let Err(e) = paste_slot_content(&content) {
                                log::warn!("Paste from slot {} failed: {}", slot, e);
                            } else {
                                log::info!(
                                    "📋 Pasted from slot {}: {}",
                                    slot,
                                    truncate(&content, 40)
                                );
                            }
                        }
                        Ok(None) => log::info!("📋 Slot {} is empty", slot),
                        Err(e) => log::warn!("Paste from slot {} failed: {}", slot, e),
                    }
                }
                HotkeyAction::OpenTui => {
                    log::info!("🔍 Opening TUI search...");
                    if let Ok(exe) = std::env::current_exe() {
                        std::process::Command::new(exe).arg("search").spawn().ok();
                    }
                }
            }
        }));

        // Start the main run loop; Ctrl+C handler will stop it.
        run_cf_run_loop();
    }

    #[cfg(not(target_os = "macos"))]
    {
        let receiver = GlobalHotKeyEvent::receiver();
        let hotkey_slot_mgr = slot_manager.clone();

        loop {
            if stop_hotkey.load(Ordering::Relaxed) {
                break;
            }

            // Check for hotkey events (non-blocking drain)
            if let Ok(event) = receiver.try_recv() {
                log::info!("⌨️ Hotkey event: id={}, state={:?}", event.id(), event.state);

                if event.state == global_hotkey::HotKeyState::Pressed {
                    if let Some((_, action)) = registered_hotkeys
                        .iter()
                        .find(|(hk, _)| hk.id() == event.id)
                    {
                        match action {
                            HotkeyAction::CopyToSlot(slot) => {
                                let _ = copy_selection_into_slot(*slot, &hotkey_slot_mgr);
                            }
                            HotkeyAction::PasteFromSlot(slot) => {
                                if let Ok(Some(content)) = hotkey_slot_mgr.get_slot(*slot) {
                                    let _ = paste_slot_content(&content);
                                }
                            }
                            HotkeyAction::OpenTui => {
                                if let Ok(exe) = std::env::current_exe() {
                                    std::process::Command::new(exe)
                                        .arg("search")
                                        .spawn()
                                        .ok();
                                }
                            }
                        }
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    println!("\n  🛑 Shutting down clipd daemon...");
    stop.store(true, Ordering::Relaxed);

    // Wait for threads to finish
    watcher_handle.join().ok();
    store_handle.join().ok();

    println!("  ✅ Goodbye!");
    Ok(())
}

#[derive(Debug, Clone)]
enum HotkeyAction {
    CopyToSlot(u8),
    PasteFromSlot(u8),
    OpenTui,
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim().replace('\n', " ");
    let char_count: usize = trimmed.chars().count();
    if char_count > max {
        let end: String = trimmed.chars().take(max).collect();
        format!("{}…", end)
    } else {
        trimmed
    }
}

fn setup_ctrlc(stop: Arc<AtomicBool>) {
    ctrlc::set_handler(move || {
        stop.store(true, Ordering::Relaxed);
        #[cfg(target_os = "macos")]
        unsafe {
            use std::ffi::c_void;
            extern "C" {
                fn CFRunLoopGetMain() -> *const c_void;
                fn CFRunLoopStop(rl: *const c_void);
            }
            let rl = CFRunLoopGetMain();
            CFRunLoopStop(rl);
        }
    })
    .expect("Failed to set Ctrl+C handler");
}

fn copy_selection_into_slot(
    slot: u8,
    slot_manager: &SlotManager,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    // The user-triggering hotkey still has modifiers physically pressed.
    // Wait briefly so our synthetic Cmd+C doesn't become Cmd+Shift+C, etc.
    std::thread::sleep(std::time::Duration::from_millis(180));
    trigger_system_copy()?;
    std::thread::sleep(std::time::Duration::from_millis(120));

    let mut cb = Clipboard::new()?;
    let text = cb.get_text().ok();
    if let Some(content) = text {
        slot_manager.copy_to_slot(slot, content.clone())?;
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

fn paste_slot_content(content: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut cb = Clipboard::new()?;
    cb.set_text(content)?;
    // Same modifier-release guard as copy path.
    std::thread::sleep(std::time::Duration::from_millis(180));
    trigger_system_paste()?;
    Ok(())
}

fn trigger_system_copy() -> Result<(), Box<dyn std::error::Error>> {
    trigger_shortcut(Key::Meta, Key::Unicode('c'))
}

fn trigger_system_paste() -> Result<(), Box<dyn std::error::Error>> {
    trigger_shortcut(Key::Meta, Key::Unicode('v'))
}

fn trigger_shortcut(modifier: Key, key: Key) -> Result<(), Box<dyn std::error::Error>> {
    let mut enigo = Enigo::new(&Settings::default())?;
    enigo.key(modifier, Direction::Press)?;
    std::thread::sleep(std::time::Duration::from_millis(8));
    enigo.key(key, Direction::Click)?;
    std::thread::sleep(std::time::Duration::from_millis(8));
    enigo.key(modifier, Direction::Release)?;
    Ok(())
}

/// Process pending macOS events. Required for Carbon global hotkeys to work.
/// CFRunLoopRunInMode dispatches queued events (including hotkey presses) and
/// then waits up to `seconds` for new events before returning.
#[cfg(target_os = "macos")]
fn process_macos_events() {
    use std::ffi::c_void;

    extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(
            mode: *const c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
    }

    unsafe {
        // Default mode is what most Carbon-backed console apps can pump reliably.
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0);
    }
}

#[cfg(target_os = "macos")]
fn run_cf_run_loop() {
    use std::ffi::c_void;
    extern "C" {
        fn CFRunLoopRun() -> i32;
        fn CFRunLoopGetMain() -> *const c_void;
    }

    // Ensure main run loop exists (helps in some console-launch contexts).
    let _ = unsafe { CFRunLoopGetMain() };
    unsafe {
        CFRunLoopRun();
    }
}
