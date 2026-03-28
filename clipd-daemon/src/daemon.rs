use arboard::Clipboard;
use clipd_core::{ClipEvent, ClipStore, ClipWatcher, SlotManager};
#[cfg(target_os = "macos")]
use std::collections::HashSet;
#[cfg(not(target_os = "macos"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
#[cfg(not(target_os = "macos"))]
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use rdev::{listen, EventType, Key as RKey};

/// How long to wait after the last tap before deciding the final slot.
const TAP_WINDOW: Duration = Duration::from_millis(350);

pub fn run_daemon() -> Result<(), Box<dyn std::error::Error>> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║         clipd daemon v0.1.0           ║");
    println!("  ║   AI clipboard for developers         ║");
    println!("  ╚═══════════════════════════════════════╝");
    println!();

    let db_path = ClipStore::default_path();
    println!("  📦 Database: {}", db_path.display());
    let _store = ClipStore::new(&db_path)?;
    let slot_manager = SlotManager::new();
    let db_path_clone = db_path.clone();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_watcher = stop.clone();
    let stop_hotkey = stop.clone();

    let suppress = Arc::new(AtomicBool::new(false));
    let suppress_watcher = suppress.clone();

    let refresh_hash = Arc::new(AtomicBool::new(false));
    let refresh_hash_watcher = refresh_hash.clone();

    let stop_ctrlc = stop.clone();
    setup_ctrlc(stop_ctrlc);

    // ── Clipboard Watcher Thread ──
    let (clip_tx, clip_rx) = mpsc::channel::<ClipEvent>();
    let watcher = ClipWatcher::new(500);
    let watcher_handle = std::thread::Builder::new()
        .name("clipd-watcher".into())
        .spawn(move || {
            watcher.watch(clip_tx, stop_watcher, suppress_watcher, refresh_hash_watcher);
        })?;

    // ── Store Writer Thread ──
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
                        slot_writer.copy_to_slot(0, entry.content.clone()).ok();
                        match store.insert(&entry) {
                            Ok(id) => log::info!(
                                "Saved clip #{}: {} [{}] {}",
                                id, entry.content_type.icon(), entry.content_type.as_str(),
                                truncate(&entry.preview, 60)
                            ),
                            Err(e) => log::error!("Failed to save clip: {}", e),
                        }
                    }
                }
            }
        })?;

    // ── Non-macOS: global-hotkey crate ──
    #[cfg(not(target_os = "macos"))]
    let hotkey_manager = GlobalHotKeyManager::new()?;
    #[cfg(not(target_os = "macos"))]
    let mut registered_hotkeys: Vec<(HotKey, FinalAction)> = Vec::new();
    #[cfg(not(target_os = "macos"))]
    {
        let digit_codes = [
            Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4, Code::Digit5,
            Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9,
        ];
        for (i, code) in digit_codes.iter().enumerate() {
            let slot_num = (i + 1) as u8;
            let copy_hk = HotKey::new(Some(Modifiers::SUPER | Modifiers::CONTROL), *code);
            if let Err(e) = hotkey_manager.register(copy_hk) {
                log::warn!("Failed to register Cmd+Ctrl+{}: {}", slot_num, e);
            } else {
                registered_hotkeys.push((copy_hk, FinalAction::CopyToSlot(slot_num)));
            }
            let paste_hk = HotKey::new(
                Some(Modifiers::SUPER | Modifiers::CONTROL | Modifiers::ALT), *code,
            );
            if let Err(e) = hotkey_manager.register(paste_hk) {
                log::warn!("Failed to register Cmd+Ctrl+Option+{}: {}", slot_num, e);
            } else {
                registered_hotkeys.push((paste_hk, FinalAction::PasteFromSlot(slot_num)));
            }
        }
        let tui_hk = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyV);
        if let Err(e) = hotkey_manager.register(tui_hk) {
            log::warn!("Failed to register Cmd+Shift+V: {}", e);
        } else {
            registered_hotkeys.push((tui_hk, FinalAction::OpenTui));
        }
    }

    println!("  ⌨️  Hotkeys (two ways to save/paste):");
    println!();
    println!("     Cmd+C        → auto-saved to slot 1 (most recent)");
    println!();
    println!("     Option A — Cmd multi-tap:");
    println!("       Cmd+C × 2  → save to slot 2    Cmd+V × 2  → paste slot 2");
    println!("       Cmd+C × 3  → save to slot 3    Cmd+V × 3  → paste slot 3");
    println!();
    println!("     Option B — Ctrl tap:");
    println!("       Ctrl+V × 1 → paste slot 1      Ctrl+C × 1 → save to slot 1");
    println!("       Ctrl+V × 2 → paste slot 2      Ctrl+C × 2 → save to slot 2");
    println!();
    println!("     Ctrl+R → open search TUI");
    println!("     (action fires 0.35s after last tap)");
    println!();
    println!("  👀 Watching clipboard... (Ctrl+C to stop)");
    println!();

    // ── Main Event Loop (macOS) ──
    #[cfg(target_os = "macos")]
    {
        println!("  🪄 Starting rdev hotkey listener...");
        let (hotkey_tx, hotkey_rx) = mpsc::channel::<HotkeyTick>();
        let stop_listener = stop.clone();

        std::thread::Builder::new()
            .name("clipd-hotkey-listener".into())
            .spawn(move || {
                if let Err(e) = start_macos_hotkey_listener(hotkey_tx, stop_listener) {
                    log::error!("Hotkey listener failed: {}", e);
                }
            })?;

        let hotkey_slot_mgr = slot_manager.clone();

        // Cmd+C / Cmd+V multi-tap counters (slot = taps - 1, minimum 2 taps)
        let mut cmd_c_taps: u8 = 0;
        let mut cmd_c_deadline: Option<Instant> = None;
        let mut cmd_v_taps: u8 = 0;
        let mut cmd_v_deadline: Option<Instant> = None;

        // Ctrl+C / Ctrl+V tap counters (slot = taps, minimum 1 tap)
        let mut ctrl_c_taps: u8 = 0;
        let mut ctrl_c_deadline: Option<Instant> = None;
        let mut ctrl_v_taps: u8 = 0;
        let mut ctrl_v_deadline: Option<Instant> = None;

        while !stop_hotkey.load(Ordering::Relaxed) {
            if let Ok(tick) = hotkey_rx.recv_timeout(Duration::from_millis(50)) {
                match tick {
                    HotkeyTick::CmdCTap => {
                        cmd_c_taps = cmd_c_taps.saturating_add(1);
                        cmd_c_deadline = Some(Instant::now() + TAP_WINDOW);
                        if cmd_c_taps >= 2 {
                            log::info!("⌨️  Cmd+C tap #{} → slot {}", cmd_c_taps, cmd_c_taps);
                        }
                    }
                    HotkeyTick::CmdVTap => {
                        cmd_v_taps = cmd_v_taps.saturating_add(1);
                        cmd_v_deadline = Some(Instant::now() + TAP_WINDOW);
                        if cmd_v_taps >= 2 {
                            log::info!("⌨️  Cmd+V tap #{} → slot {}", cmd_v_taps, cmd_v_taps);
                        }
                    }
                    HotkeyTick::CtrlCTap => {
                        ctrl_c_taps = ctrl_c_taps.saturating_add(1);
                        ctrl_c_deadline = Some(Instant::now() + TAP_WINDOW);
                        log::info!("⌨️  Ctrl+C tap #{} → slot {}", ctrl_c_taps, ctrl_c_taps);
                    }
                    HotkeyTick::CtrlVTap => {
                        ctrl_v_taps = ctrl_v_taps.saturating_add(1);
                        ctrl_v_deadline = Some(Instant::now() + TAP_WINDOW);
                        log::info!("⌨️  Ctrl+V tap #{} → slot {}", ctrl_v_taps, ctrl_v_taps);
                    }
                    HotkeyTick::OpenTui => {
                        open_tui_search();
                    }
                }
            }

            let now = Instant::now();

            // Cmd+C: 1 tap → auto-save to slot 1 (most recent),
            //        2+ taps → save to slot N, then restore clipboard to slot 1
            if let Some(dl) = cmd_c_deadline {
                if now >= dl && cmd_c_taps > 0 {
                    if cmd_c_taps == 1 {
                        execute_copy(1, &hotkey_slot_mgr);
                        log::info!("⌨️  Cmd+C → auto-saved to slot 1");
                    } else {
                        let slot = cmd_c_taps.min(9);
                        execute_copy(slot, &hotkey_slot_mgr);
                        restore_clipboard_to_slot(&hotkey_slot_mgr, &suppress, &refresh_hash, 1);
                    }
                    cmd_c_taps = 0;
                    cmd_c_deadline = None;
                }
            }

            // Cmd+V: 2+ taps → undo normal pastes, paste from slot (taps)
            if let Some(dl) = cmd_v_deadline {
                if now >= dl && cmd_v_taps > 0 {
                    if cmd_v_taps >= 2 {
                        let slot = cmd_v_taps.min(9);
                        execute_undo_paste(slot, cmd_v_taps, &hotkey_slot_mgr, &suppress);
                    }
                    cmd_v_taps = 0;
                    cmd_v_deadline = None;
                }
            }

            // Ctrl+C: 1+ taps → save to slot (taps)
            if let Some(dl) = ctrl_c_deadline {
                if now >= dl && ctrl_c_taps > 0 {
                    let slot = ctrl_c_taps.min(9);
                    execute_copy(slot, &hotkey_slot_mgr);
                    ctrl_c_taps = 0;
                    ctrl_c_deadline = None;
                }
            }

            // Ctrl+V: 1+ taps → directly paste from slot (taps), no undo needed
            if let Some(dl) = ctrl_v_deadline {
                if now >= dl && ctrl_v_taps > 0 {
                    let slot = ctrl_v_taps.min(9);
                    execute_direct_paste(slot, &hotkey_slot_mgr, &suppress);
                    ctrl_v_taps = 0;
                    ctrl_v_deadline = None;
                }
            }
        }
    }

    // ── Main Event Loop (non-macOS) ──
    #[cfg(not(target_os = "macos"))]
    {
        let receiver = GlobalHotKeyEvent::receiver();
        let hotkey_slot_mgr = slot_manager.clone();
        loop {
            if stop_hotkey.load(Ordering::Relaxed) { break; }
            if let Ok(event) = receiver.try_recv() {
                if event.state == global_hotkey::HotKeyState::Pressed {
                    if let Some((_, action)) = registered_hotkeys
                        .iter().find(|(hk, _)| hk.id() == event.id)
                    {
                        match action {
                            FinalAction::CopyToSlot(s) => execute_copy(*s, &hotkey_slot_mgr),
                            FinalAction::PasteFromSlot(s) => execute_direct_paste(*s, &hotkey_slot_mgr, &suppress),
                            FinalAction::OpenTui => open_tui_search(),
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    println!("\n  🛑 Shutting down clipd daemon...");
    stop.store(true, Ordering::Relaxed);
    watcher_handle.join().ok();
    store_handle.join().ok();
    println!("  ✅ Goodbye!");
    Ok(())
}

// ── Messages from rdev listener → main loop ──

#[derive(Debug, Clone)]
enum HotkeyTick {
    CmdCTap,   // Cmd+C (system also copies)
    CmdVTap,   // Cmd+V (system also pastes)
    CtrlCTap,  // Ctrl+C only (no system side-effect on macOS GUI apps)
    CtrlVTap,  // Ctrl+V only (no system side-effect on macOS GUI apps)
    OpenTui,
}

#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone)]
enum FinalAction {
    CopyToSlot(u8),
    PasteFromSlot(u8),
    OpenTui,
}

// ── Action executors ──

/// After a multi-tap copy, restore the OS clipboard to the given slot's content
/// so that a normal Cmd+V pastes the "first" copy rather than the multi-tap content.
fn restore_clipboard_to_slot(
    mgr: &SlotManager,
    suppress: &Arc<AtomicBool>,
    refresh_hash: &Arc<AtomicBool>,
    slot: u8,
) {
    if let Ok(Some(content)) = mgr.get_slot(slot) {
        suppress.store(true, Ordering::SeqCst);
        if let Ok(mut cb) = Clipboard::new() {
            if let Err(e) = cb.set_text(&content) {
                log::warn!("Failed to restore clipboard to slot {}: {}", slot, e);
            }
        }
        refresh_hash.store(true, Ordering::SeqCst);
        suppress.store(false, Ordering::SeqCst);
        log::debug!("Clipboard restored to slot {} content", slot);
    }
}

fn execute_copy(slot: u8, mgr: &SlotManager) {
    let mut cb = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => { log::warn!("Copy to slot {} failed: {}", slot, e); return; }
    };
    match cb.get_text() {
        Ok(text) => {
            mgr.copy_to_slot(slot, text.clone()).ok();
            log::info!("📋 Saved to slot {}: {}", slot, truncate(&text, 40));
        }
        Err(_) => log::info!("📋 Copy to slot {} skipped (clipboard empty)", slot),
    }
}

/// Cmd+V multi-tap path: system already pasted N times, so undo then re-paste slot content.
/// Suppresses the watcher and restores the original clipboard afterwards.
fn execute_undo_paste(slot: u8, tap_count: u8, mgr: &SlotManager, suppress: &Arc<AtomicBool>) {
    if let Ok(Some(content)) = mgr.get_slot(slot) {
        let mut cb = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => { log::warn!("Paste from slot {} failed: {}", slot, e); return; }
        };

        let original = cb.get_text().ok();
        suppress.store(true, Ordering::SeqCst);

        if let Err(e) = cb.set_text(&content) {
            suppress.store(false, Ordering::SeqCst);
            log::warn!("Paste from slot {} failed: {}", slot, e);
            return;
        }
        std::thread::sleep(Duration::from_millis(80));
        if let Err(e) = undo_then_paste(tap_count) {
            log::warn!("Auto-paste failed: {}", e);
            log::info!("Slot {} content is on clipboard — press Cmd+V", slot);
        } else {
            log::info!("📋 Pasted from slot {}: {}", slot, truncate(&content, 40));
        }

        std::thread::sleep(Duration::from_millis(200));

        let clipboard_unchanged = cb.get_text()
            .map(|current| current == content)
            .unwrap_or(false);
        if clipboard_unchanged {
            if let Some(ref orig) = original {
                if let Err(e) = cb.set_text(orig) {
                    log::warn!("Failed to restore clipboard after paste: {}", e);
                }
            }
        }

        suppress.store(false, Ordering::SeqCst);
    } else {
        log::info!("📋 Slot {} is empty", slot);
    }
}

/// Ctrl+V path: nothing was pasted by the system, so just set clipboard and send Cmd+V.
/// Suppresses the watcher and restores the original clipboard afterwards.
fn execute_direct_paste(slot: u8, mgr: &SlotManager, suppress: &Arc<AtomicBool>) {
    if let Ok(Some(content)) = mgr.get_slot(slot) {
        let mut cb = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => { log::warn!("Paste from slot {} failed: {}", slot, e); return; }
        };

        let original = cb.get_text().ok();
        suppress.store(true, Ordering::SeqCst);

        if let Err(e) = cb.set_text(&content) {
            suppress.store(false, Ordering::SeqCst);
            log::warn!("Paste from slot {} failed: {}", slot, e);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
        let script = r#"tell application "System Events"
  keystroke "v" using command down
end tell"#;
        let output = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .output();
        match output {
            Ok(o) if o.status.success() => {
                log::info!("📋 Pasted from slot {}: {}", slot, truncate(&content, 40));
            }
            _ => {
                log::info!("Slot {} content is on clipboard — press Cmd+V", slot);
            }
        }

        std::thread::sleep(Duration::from_millis(200));

        // Only restore if the user hasn't done Cmd+C during the paste window.
        // If the clipboard changed, someone else (the user) wrote new content
        // and we must not overwrite it.
        let clipboard_unchanged = cb.get_text()
            .map(|current| current == content)
            .unwrap_or(false);
        if clipboard_unchanged {
            if let Some(ref orig) = original {
                if let Err(e) = cb.set_text(orig) {
                    log::warn!("Failed to restore clipboard after paste: {}", e);
                }
            }
        }

        suppress.store(false, Ordering::SeqCst);
    } else {
        log::info!("📋 Slot {} is empty", slot);
    }
}

fn undo_then_paste(undo_count: u8) -> Result<(), Box<dyn std::error::Error>> {
    let undos: String = (0..undo_count)
        .map(|_| "  keystroke \"z\" using command down\n  delay 0.05")
        .collect::<Vec<_>>()
        .join("\n");

    let script = format!(
        r#"tell application "System Events"
{}
  delay 0.05
  keystroke "v" using command down
end tell"#,
        undos
    );
    let output = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output()?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("osascript undo+paste failed: {}", err).into());
    }
    Ok(())
}

fn open_tui_search() {
    log::info!("🔍 Opening TUI search...");
    let exe = match std::env::current_exe() {
        Ok(e) => e.to_string_lossy().to_string(),
        Err(_) => return,
    };
    let cmd = format!("cd /tmp && {} search", exe);
    let script = format!(
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
    let result = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output();
    if result.map_or(true, |o| !o.status.success()) {
        let fallback = format!(
            "tell application \"Terminal\"\n  activate\n  do script \"{}\"\nend tell",
            cmd
        );
        std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&fallback)
            .spawn()
            .ok();
    }
}

// ── Helpers ──

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
    })
    .expect("Failed to set Ctrl+C handler");
}

// ── macOS rdev listener ──

#[cfg(target_os = "macos")]
fn start_macos_hotkey_listener(
    tx: mpsc::Sender<HotkeyTick>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut pressed_mods: HashSet<RKey> = HashSet::new();
    let mut event_count: u64 = 0;

    listen(move |event| {
        if stop.load(Ordering::Relaxed) {
            return;
        }

        event_count += 1;
        if event_count == 1 {
            log::info!("🎹 rdev: first event received — Input Monitoring permissions OK");
        }

        match event.event_type {
            EventType::KeyPress(key) => {
                if is_modifier_key(key) {
                    pressed_mods.insert(key);
                    return;
                }

                // Cmd+Shift+V → open search TUI (check before Cmd+V)
                if is_cmd_shift(&pressed_mods) && key == RKey::KeyV {
                    log::info!("⌨️  Cmd+Shift+V → open search");
                    let _ = tx.send(HotkeyTick::OpenTui);
                    return;
                }

                // Ctrl+R → open search TUI (check before Ctrl+C/V)
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyR {
                    log::info!("⌨️  Ctrl+R → open search");
                    let _ = tx.send(HotkeyTick::OpenTui);
                    return;
                }

                // Cmd+Ctrl+C → treat as Ctrl+C (user transitioning from Cmd+C to save slot)
                if has_cmd(&pressed_mods) && has_ctrl(&pressed_mods) && key == RKey::KeyC {
                    let _ = tx.send(HotkeyTick::CtrlCTap);
                    return;
                }
                // Cmd+Ctrl+V → treat as Ctrl+V (user transitioning from Cmd+V)
                if has_cmd(&pressed_mods) && has_ctrl(&pressed_mods) && key == RKey::KeyV {
                    let _ = tx.send(HotkeyTick::CtrlVTap);
                    return;
                }

                // Cmd+C (just Cmd, no Ctrl/Shift/Option)
                if is_cmd_only(&pressed_mods) && key == RKey::KeyC {
                    let _ = tx.send(HotkeyTick::CmdCTap);
                    return;
                }
                // Cmd+V (just Cmd, no Ctrl/Shift/Option)
                if is_cmd_only(&pressed_mods) && key == RKey::KeyV {
                    let _ = tx.send(HotkeyTick::CmdVTap);
                    return;
                }

                // Ctrl+C (just Ctrl, no Cmd/Shift/Option)
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyC {
                    let _ = tx.send(HotkeyTick::CtrlCTap);
                    return;
                }
                // Ctrl+V (just Ctrl, no Cmd/Shift/Option)
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyV {
                    let _ = tx.send(HotkeyTick::CtrlVTap);
                    return;
                }
            }
            EventType::KeyRelease(key) => {
                if is_modifier_key(key) {
                    pressed_mods.remove(&key);
                }
            }
            _ => {}
        }
    })
    .map_err(|e| format!("rdev listen error: {:?}", e))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_modifier_key(key: RKey) -> bool {
    matches!(
        key,
        RKey::MetaLeft | RKey::MetaRight
            | RKey::ShiftLeft | RKey::ShiftRight
            | RKey::ControlLeft | RKey::ControlRight
            | RKey::Alt | RKey::AltGr
    )
}

/// Cmd held alone (no Ctrl, no Shift, no Option)
#[cfg(target_os = "macos")]
fn is_cmd_only(pressed_mods: &HashSet<RKey>) -> bool {
    (pressed_mods.contains(&RKey::MetaLeft) || pressed_mods.contains(&RKey::MetaRight))
        && !pressed_mods.contains(&RKey::ControlLeft)
        && !pressed_mods.contains(&RKey::ControlRight)
        && !pressed_mods.contains(&RKey::ShiftLeft)
        && !pressed_mods.contains(&RKey::ShiftRight)
        && !pressed_mods.contains(&RKey::Alt)
        && !pressed_mods.contains(&RKey::AltGr)
}

/// Ctrl held alone (no Cmd, no Shift, no Option)
#[cfg(target_os = "macos")]
fn is_ctrl_only(pressed_mods: &HashSet<RKey>) -> bool {
    (pressed_mods.contains(&RKey::ControlLeft) || pressed_mods.contains(&RKey::ControlRight))
        && !pressed_mods.contains(&RKey::MetaLeft)
        && !pressed_mods.contains(&RKey::MetaRight)
        && !pressed_mods.contains(&RKey::ShiftLeft)
        && !pressed_mods.contains(&RKey::ShiftRight)
        && !pressed_mods.contains(&RKey::Alt)
        && !pressed_mods.contains(&RKey::AltGr)
}

#[cfg(target_os = "macos")]
fn has_cmd(pressed_mods: &HashSet<RKey>) -> bool {
    pressed_mods.contains(&RKey::MetaLeft) || pressed_mods.contains(&RKey::MetaRight)
}

#[cfg(target_os = "macos")]
fn has_ctrl(pressed_mods: &HashSet<RKey>) -> bool {
    pressed_mods.contains(&RKey::ControlLeft) || pressed_mods.contains(&RKey::ControlRight)
}

#[cfg(target_os = "macos")]
fn is_cmd_shift(pressed_mods: &HashSet<RKey>) -> bool {
    has_cmd(pressed_mods)
        && (pressed_mods.contains(&RKey::ShiftLeft) || pressed_mods.contains(&RKey::ShiftRight))
}
