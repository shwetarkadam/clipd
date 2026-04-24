use arboard::Clipboard;
use clipd_core::{
    apply_transform, find_rules_for_app, generate_embedding, is_embedding_available,
    load_paste_rules, load_paste_transform_settings, load_transform_config,
    release_daemon_lock, suggest_smart_transform, try_acquire_daemon_lock, ClipEvent, ClipStore,
    ClipWatcher, PasteRulesConfig, PasteTransformSettings, SlotManager, TransformConfig,
    TransformKind, MAX_CLIP_SLOT,
};
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
/// Ignore duplicate key events faster than this (macOS key-repeat / missing KeyRelease on C/V).
const TAP_DEBOUNCE: Duration = Duration::from_millis(65);

pub fn run_daemon() -> Result<(), Box<dyn std::error::Error>> {
    if !try_acquire_daemon_lock() {
        log::info!("clipd daemon is already running — skipping duplicate launch");
        return Ok(());
    }

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
            let embed_config = load_transform_config();
            let embed_available = is_embedding_available(&embed_config);
            if embed_available {
                log::info!("🧠 Vector embeddings enabled (API key found)");
                backfill_embeddings(&store, &embed_config);
            } else {
                log::info!("🔍 Semantic search will use TF-IDF (no API key for embeddings)");
            }
            for event in clip_rx {
                match event {
                    ClipEvent::NewClip(entry) => {
                        slot_writer.copy_to_slot(0, entry.content.clone()).ok();
                        let content_for_embed = entry.content.clone();
                        match store.insert(&entry) {
                            Ok(id) => {
                                log::info!(
                                    "Saved clip #{}: {} [{}] {}",
                                    id, entry.content_type.icon(), entry.content_type.as_str(),
                                    truncate(&entry.preview, 60)
                                );
                                if embed_available {
                                    match generate_embedding(&content_for_embed, &embed_config) {
                                        Ok(emb) => {
                                            if let Err(e) = store.store_embedding(id, &emb) {
                                                log::warn!("Failed to store embedding: {}", e);
                                            } else {
                                                log::debug!("🧠 Embedded clip #{} ({} dims)", id, emb.len());
                                            }
                                        }
                                        Err(e) => log::debug!("Embedding skipped: {}", e),
                                    }
                                }
                            }
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
        let smart_hk = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV);
        if let Err(e) = hotkey_manager.register(smart_hk) {
            log::warn!("Failed to register Ctrl+Shift+V: {}", e);
        } else {
            registered_hotkeys.push((smart_hk, FinalAction::SmartPaste));
        }
        let tui_hk_r = HotKey::new(Some(Modifiers::CONTROL), Code::KeyR);
        if let Err(e) = hotkey_manager.register(tui_hk_r) {
            log::warn!("Failed to register Ctrl+R: {}", e);
        } else {
            registered_hotkeys.push((tui_hk_r, FinalAction::OpenTui));
        }
        let tui_hk2 = HotKey::new(Some(Modifiers::CONTROL), Code::KeyT);
        if let Err(e) = hotkey_manager.register(tui_hk2) {
            log::warn!("Failed to register Ctrl+T: {}", e);
        } else {
            registered_hotkeys.push((tui_hk2, FinalAction::OpenTui));
        }
        let gui_hk = HotKey::new(Some(Modifiers::CONTROL), Code::KeyG);
        if let Err(e) = hotkey_manager.register(gui_hk) {
            log::warn!("Failed to register Ctrl+G: {}", e);
        } else {
            registered_hotkeys.push((gui_hk, FinalAction::OpenGui));
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
    println!("     Multi-tap Cmd/Ctrl + C/V → slots 1..={} (after pause)", MAX_CLIP_SLOT);
    println!();
    println!("     Ctrl+Shift+V  → smart paste (transform clipboard + paste)");
    println!("     Cmd+Option+V  → sequence paste (auto-increment through slots)");
    println!();
    println!("     Ctrl+T → open TUI        Ctrl+G → open GUI");
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

        let mut last_cmd_c = Instant::now() - Duration::from_secs(10);
        let mut last_cmd_v = Instant::now() - Duration::from_secs(10);
        let mut last_ctrl_c = Instant::now() - Duration::from_secs(10);
        let mut last_ctrl_v = Instant::now() - Duration::from_secs(10);

        let mut sequence_slot: u8 = 1;
        let mut cmd_v_saved_clipboard: Option<String> = None;
        let paste_rules = load_paste_rules();
        let transform_config = load_transform_config();
        let paste_transform = load_paste_transform_settings();

        while !stop_hotkey.load(Ordering::Relaxed) {
            if let Ok(tick) = hotkey_rx.recv_timeout(Duration::from_millis(50)) {
                match tick {
                    HotkeyTick::CmdCTap => {
                        let now = Instant::now();
                        if now.duration_since(last_cmd_c) < TAP_DEBOUNCE {
                            continue;
                        }
                        last_cmd_c = now;
                        cmd_c_taps = (cmd_c_taps + 1).min(MAX_CLIP_SLOT);
                        cmd_c_deadline = Some(now + TAP_WINDOW);
                        if cmd_c_taps >= 2 {
                            let slot = cmd_c_taps.min(MAX_CLIP_SLOT);
                            log::info!("⌨️  Cmd+C tap #{} → slot {}", cmd_c_taps, slot);
                            #[cfg(target_os = "macos")]
                            show_slot_notification("Copy", slot);
                        }
                    }
                    HotkeyTick::CmdVTap => {
                        let now = Instant::now();
                        if now.duration_since(last_cmd_v) < TAP_DEBOUNCE {
                            continue;
                        }
                        last_cmd_v = now;
                        cmd_v_taps = (cmd_v_taps + 1).min(MAX_CLIP_SLOT);
                        cmd_v_deadline = Some(now + TAP_WINDOW);
                        if cmd_v_taps == 1 {
                            // First tap: just save the clipboard content.
                            // Don't clear yet — the app may still be reading
                            // the clipboard for this paste.
                            if let Ok(mut cb) = Clipboard::new() {
                                cmd_v_saved_clipboard = cb.get_text().ok();
                            }
                        }
                        if cmd_v_taps == 2 {
                            // Second tap confirms multi-tap intent.
                            // NOW clear clipboard so taps 3+ paste nothing.
                            suppress.store(true, Ordering::SeqCst);
                            if let Ok(mut cb) = Clipboard::new() {
                                let _ = cb.set_text("");
                            }
                        }
                        if cmd_v_taps >= 2 {
                            let slot = cmd_v_taps.min(MAX_CLIP_SLOT);
                            log::info!("⌨️  Cmd+V tap #{} → slot {}", cmd_v_taps, slot);
                            #[cfg(target_os = "macos")]
                            show_slot_notification("Paste", slot);
                        }
                    }
                    HotkeyTick::CtrlCTap => {
                        let now = Instant::now();
                        if now.duration_since(last_ctrl_c) < TAP_DEBOUNCE {
                            continue;
                        }
                        last_ctrl_c = now;
                        ctrl_c_taps = (ctrl_c_taps + 1).min(MAX_CLIP_SLOT);
                        ctrl_c_deadline = Some(now + TAP_WINDOW);
                        let slot = ctrl_c_taps.min(MAX_CLIP_SLOT);
                        log::info!("⌨️  Ctrl+C tap #{} → slot {}", ctrl_c_taps, slot);
                        #[cfg(target_os = "macos")]
                        show_slot_notification("Copy", slot);
                    }
                    HotkeyTick::CtrlVTap => {
                        let now = Instant::now();
                        if now.duration_since(last_ctrl_v) < TAP_DEBOUNCE {
                            continue;
                        }
                        last_ctrl_v = now;
                        ctrl_v_taps = (ctrl_v_taps + 1).min(MAX_CLIP_SLOT);
                        ctrl_v_deadline = Some(now + TAP_WINDOW);
                        let slot = ctrl_v_taps.min(MAX_CLIP_SLOT);
                        log::info!("⌨️  Ctrl+V tap #{} → slot {}", ctrl_v_taps, slot);
                        #[cfg(target_os = "macos")]
                        show_slot_notification("Paste", slot);
                    }
                    HotkeyTick::OpenTui => {
                        open_tui_search();
                    }
                    HotkeyTick::OpenGui => {
                        open_gui();
                    }
                    HotkeyTick::SmartPaste => {
                        execute_smart_paste(
                            &suppress,
                            &transform_config,
                            &paste_transform,
                        );
                    }
                    HotkeyTick::SequencePaste => {
                        if hotkey_slot_mgr.has_content(sequence_slot) {
                            execute_context_paste(
                                sequence_slot,
                                &hotkey_slot_mgr,
                                &suppress,
                                &paste_rules,
                                &transform_config,
                                &paste_transform,
                            );
                            log::info!(
                                "📋 Sequence paste: slot {} (next: {})",
                                sequence_slot,
                                sequence_slot + 1
                            );
                            sequence_slot += 1;
                        } else {
                            log::info!("📋 Sequence complete — resetting to slot 1");
                            sequence_slot = 1;
                            if hotkey_slot_mgr.has_content(1) {
                                execute_context_paste(
                                    1,
                                    &hotkey_slot_mgr,
                                    &suppress,
                                    &paste_rules,
                                    &transform_config,
                                    &paste_transform,
                                );
                                sequence_slot = 2;
                            }
                        }
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
                        let slot = cmd_c_taps.min(MAX_CLIP_SLOT);
                        execute_copy(slot, &hotkey_slot_mgr);
                        restore_clipboard_to_slot(&hotkey_slot_mgr, &suppress, &refresh_hash, 1);
                    }
                    cmd_c_taps = 0;
                    cmd_c_deadline = None;
                }
            }

            // Cmd+V: 2+ taps → undo the 2 real pastes, paste from slot.
            // Tap 1 pasted real content, tap 2 also pasted real content
            // (clipboard cleared on tap 2, so taps 3+ pasted nothing).
            if let Some(dl) = cmd_v_deadline {
                if now >= dl && cmd_v_taps > 0 {
                    if cmd_v_taps >= 2 {
                        let slot = cmd_v_taps.min(MAX_CLIP_SLOT);
                        execute_undo_paste(
                            slot, 2, &hotkey_slot_mgr, &suppress,
                        );
                        // Restore original clipboard content
                        if let Some(ref orig) = cmd_v_saved_clipboard {
                            std::thread::sleep(Duration::from_millis(50));
                            if let Ok(mut cb) = Clipboard::new() {
                                let _ = cb.set_text(orig);
                            }
                            refresh_hash.store(true, Ordering::SeqCst);
                        }
                        suppress.store(false, Ordering::SeqCst);
                    }
                    cmd_v_saved_clipboard = None;
                    cmd_v_taps = 0;
                    cmd_v_deadline = None;
                }
            }

            // Ctrl+C: 1+ taps → save to slot (taps)
            if let Some(dl) = ctrl_c_deadline {
                if now >= dl && ctrl_c_taps > 0 {
                    let slot = ctrl_c_taps.min(MAX_CLIP_SLOT);
                    execute_copy(slot, &hotkey_slot_mgr);
                    ctrl_c_taps = 0;
                    ctrl_c_deadline = None;
                }
            }

            // Ctrl+V: 1+ taps → directly paste from slot (taps), no undo needed
            if let Some(dl) = ctrl_v_deadline {
                if now >= dl && ctrl_v_taps > 0 {
                    let slot = ctrl_v_taps.min(MAX_CLIP_SLOT);
                    execute_direct_paste(
                        slot,
                        &hotkey_slot_mgr,
                        &suppress,
                        &transform_config,
                        &paste_transform,
                    );
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
        let transform_config = load_transform_config();
        let paste_transform = load_paste_transform_settings();
        loop {
            if stop_hotkey.load(Ordering::Relaxed) { break; }
            if let Ok(event) = receiver.try_recv() {
                if event.state == global_hotkey::HotKeyState::Pressed {
                    if let Some((_, action)) = registered_hotkeys
                        .iter().find(|(hk, _)| hk.id() == event.id)
                    {
                        match action {
                            FinalAction::CopyToSlot(s) => {
                                show_slot_notification("Copy", *s);
                                execute_copy(*s, &hotkey_slot_mgr);
                            }
                            FinalAction::PasteFromSlot(s) => {
                                show_slot_notification("Paste", *s);
                                execute_direct_paste(
                                    *s,
                                    &hotkey_slot_mgr,
                                    &suppress,
                                    &transform_config,
                                    &paste_transform,
                                );
                            }
                            FinalAction::OpenTui => open_tui_search(),
                            FinalAction::OpenGui => open_gui(),
                            FinalAction::SmartPaste => execute_smart_paste(
                                &suppress,
                                &transform_config,
                                &paste_transform,
                            ),
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
    OpenGui,
    SequencePaste, // Cmd+Option+V — paste next item in slot sequence
    SmartPaste,    // Cmd+Shift+V — transform clipboard content and paste
}

#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone)]
enum FinalAction {
    CopyToSlot(u8),
    PasteFromSlot(u8),
    OpenTui,
    OpenGui,
    SmartPaste,
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
    // After multi-tap Cmd+C the OS can lag slightly before text is readable.
    let mut text: Option<String> = None;
    for attempt in 0..6 {
        match cb.get_text() {
            Ok(t) if !t.is_empty() => {
                text = Some(t);
                break;
            }
            _ => {
                if attempt + 1 < 6 {
                    std::thread::sleep(Duration::from_millis(45));
                }
            }
        }
    }
    if let Some(text) = text {
        mgr.copy_to_slot(slot, text.clone()).ok();
        log::info!("📋 Saved to slot {}: {}", slot, truncate(&text, 40));
    } else {
        log::info!("📋 Copy to slot {} skipped (clipboard empty)", slot);
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

        // Each OS-level Cmd+V tap already pasted. We need to:
        // 1. Undo ALL those pastes (tap_count of them)
        // 2. Paste slot content once
        // Use select-all-undo approach: Cmd+Z × tap_count, then Cmd+V.
        // The first Cmd+V tap always registers; subsequent ones may batch.
        // To be safe, undo tap_count times (matching what the OS did).
        std::thread::sleep(Duration::from_millis(100));
        if let Err(e) = undo_then_paste(tap_count) {
            log::warn!("Auto-paste failed: {}", e);
            #[cfg(target_os = "macos")]
            log::warn!(
                "If paste never works: grant Accessibility to the app running clipd \
                 (System Settings → Privacy & Security → Accessibility), and ensure \
                 Automation is allowed for System Events."
            );
            #[cfg(not(target_os = "macos"))]
            log::warn!(
                "If paste never works: run clipd as the same user as the foreground app; \
                 on Windows, synthesized Ctrl+V may require running the terminal as administrator \
                 in some setups."
            );
            log::info!("Slot {} content is on clipboard — paste manually if needed", slot);
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
/// When Transform on Paste is enabled, applies the user's transform pipeline.
fn execute_direct_paste(
    slot: u8,
    mgr: &SlotManager,
    suppress: &Arc<AtomicBool>,
    transform_cfg: &TransformConfig,
    paste_settings: &PasteTransformSettings,
) {
    if let Ok(Some(mut content)) = mgr.get_slot(slot) {
        // Apply "Transform on Paste" if enabled
        if paste_settings.enabled {
            #[cfg(target_os = "macos")]
            let dest_app = get_frontmost_app_name();
            #[cfg(not(target_os = "macos"))]
            let dest_app: Option<String> = None;

            if paste_settings.smart_mode {
                let ct = clipd_core::ContentType::detect(&content);
                let suggestions =
                    suggest_smart_transform(&content, &ct, dest_app.as_deref());
                for t in &suggestions {
                    if let Ok(transformed) = apply_transform(t, &content, transform_cfg) {
                        log::info!("🧠 Smart transform (Ctrl+V slot {}): {}", slot, t.label());
                        content = transformed;
                        break;
                    }
                }
            }

            for t in &paste_settings.active_transforms {
                if let Ok(transformed) = apply_transform(t, &content, transform_cfg) {
                    log::info!("✨ Paste transform (Ctrl+V slot {}): {}", slot, t.label());
                    content = transformed;
                }
            }

            if !paste_settings.default_ai_prompt.is_empty() {
                let kind =
                    TransformKind::CustomPrompt(paste_settings.default_ai_prompt.clone());
                if let Ok(transformed) = apply_transform(&kind, &content, transform_cfg) {
                    log::info!("✨ AI prompt transform (Ctrl+V slot {})", slot);
                    content = transformed;
                }
            }
        }

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
        simulate_paste();
        log::info!("📋 Pasted from slot {}: {}", slot, truncate(&content, 40));

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

/// True when smart-paste output is meaningfully different from input for HUD purposes.
/// Ignores trailing newlines/whitespace and CR/LF normalization so a no-op trim doesn't flash the HUD.
fn smart_paste_text_visibly_changed(before: &str, after: &str) -> bool {
    if before == after {
        return false;
    }
    let norm = |s: &str| s.replace('\r', "").trim_end().to_string();
    norm(before) != norm(after)
}

/// Smart paste: reads the current system clipboard, applies transform pipeline,
/// sets the transformed content, and pastes via Cmd+V.
/// Re-reads settings from disk each time so GUI changes take effect immediately.
fn execute_smart_paste(
    suppress: &Arc<AtomicBool>,
    _transform_cfg: &TransformConfig,
    _paste_settings: &PasteTransformSettings,
) {
    let fresh_settings = load_paste_transform_settings();
    let fresh_transform_cfg = load_transform_config();

    let mut cb = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("Smart paste: clipboard access failed: {}", e);
            return;
        }
    };

    let original = match cb.get_text() {
        Ok(text) if !text.is_empty() => text,
        _ => {
            log::info!("📋 Smart paste: clipboard is empty");
            return;
        }
    };

    let mut content = original.clone();
    let mut transformed_any = false;

    #[cfg(target_os = "macos")]
    let dest_app = get_frontmost_app_name();
    #[cfg(not(target_os = "macos"))]
    let dest_app: Option<String> = None;

    if fresh_settings.smart_mode {
        let ct = clipd_core::ContentType::detect(&content);
        let suggestions =
            suggest_smart_transform(&content, &ct, dest_app.as_deref());
        for t in &suggestions {
            if let Ok(result) = apply_transform(t, &content, &fresh_transform_cfg) {
                log::info!("🧠 Smart paste transform: {}", t.label());
                content = result;
                transformed_any = true;
                break;
            }
        }
    }

    for t in &fresh_settings.active_transforms {
        if let Ok(result) = apply_transform(t, &content, &fresh_transform_cfg) {
            log::info!("✨ Smart paste active transform: {}", t.label());
            content = result;
            transformed_any = true;
        }
    }

    if !fresh_settings.default_ai_prompt.is_empty() {
        let kind =
            TransformKind::CustomPrompt(fresh_settings.default_ai_prompt.clone());
        if let Ok(result) = apply_transform(&kind, &content, &fresh_transform_cfg) {
            log::info!("✨ Smart paste AI prompt transform");
            content = result;
            transformed_any = true;
        }
    }

    if !transformed_any {
        log::info!("📋 Smart paste: no transforms matched — pasting as-is");
    }

    // HUD only when a transform ran *and* the result isn't trivially the same text
    // (e.g. TrimWhitespace often only strips a trailing newline — looks like a no-op).
    if transformed_any && smart_paste_text_visibly_changed(&original, &content) {
        show_hud("✨ Smart Paste");
    }

    suppress.store(true, Ordering::SeqCst);

    if let Err(e) = cb.set_text(&content) {
        suppress.store(false, Ordering::SeqCst);
        log::warn!("Smart paste: failed to set clipboard: {}", e);
        return;
    }

    std::thread::sleep(Duration::from_millis(50));

    simulate_paste();
    log::info!("📋 Smart pasted: {}", truncate(&content, 60));

    std::thread::sleep(Duration::from_millis(200));
    suppress.store(false, Ordering::SeqCst);
}

/// Context-aware paste: checks destination app for paste rules,
/// applies user's "Transform on Paste" settings, then pastes.
fn execute_context_paste(
    slot: u8,
    mgr: &SlotManager,
    suppress: &Arc<AtomicBool>,
    rules: &PasteRulesConfig,
    transform_cfg: &TransformConfig,
    paste_settings: &PasteTransformSettings,
) {
    if let Ok(Some(mut content)) = mgr.get_slot(slot) {
        #[cfg(target_os = "macos")]
        let dest_app = get_frontmost_app_name();
        #[cfg(not(target_os = "macos"))]
        let dest_app: Option<String> = None;

        // 1. Context-aware paste rules (per-app auto-transforms)
        if let Some(ref app) = dest_app {
            let matching = find_rules_for_app(app, rules);
            for rule in matching {
                if rule.auto_apply {
                    if let Ok(transformed) =
                        apply_transform(&rule.transform, &content, transform_cfg)
                    {
                        log::info!(
                            "🔄 Auto-transform for {}: {}",
                            app,
                            rule.description
                        );
                        content = transformed;
                    }
                    break;
                }
            }
        }

        // 2. User's "Transform on Paste" settings
        if paste_settings.enabled {
            // Smart mode: auto-detect content and pick transforms
            if paste_settings.smart_mode {
                let ct = clipd_core::ContentType::detect(&content);
                let suggestions =
                    suggest_smart_transform(&content, &ct, dest_app.as_deref());
                for t in &suggestions {
                    if let Ok(transformed) = apply_transform(t, &content, transform_cfg) {
                        log::info!("🧠 Smart transform applied: {}", t.label());
                        content = transformed;
                        break;
                    }
                }
            }

            // Apply user-selected active transforms
            for t in &paste_settings.active_transforms {
                if let Ok(transformed) = apply_transform(t, &content, transform_cfg) {
                    log::info!("✨ Paste transform applied: {}", t.label());
                    content = transformed;
                }
            }

            // Apply default AI prompt if set
            if !paste_settings.default_ai_prompt.is_empty() {
                let kind =
                    TransformKind::CustomPrompt(paste_settings.default_ai_prompt.clone());
                if let Ok(transformed) = apply_transform(&kind, &content, transform_cfg) {
                    log::info!("✨ AI prompt transform applied");
                    content = transformed;
                }
            }
        }

        let mut cb = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Context paste failed: {}", e);
                return;
            }
        };

        let original = cb.get_text().ok();
        suppress.store(true, Ordering::SeqCst);

        if let Err(e) = cb.set_text(&content) {
            suppress.store(false, Ordering::SeqCst);
            log::warn!("Context paste failed: {}", e);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
        simulate_paste();

        std::thread::sleep(Duration::from_millis(200));
        let clipboard_unchanged = cb
            .get_text()
            .map(|current| current == content)
            .unwrap_or(false);
        if clipboard_unchanged {
            if let Some(ref orig) = original {
                let _ = cb.set_text(orig);
            }
        }
        suppress.store(false, Ordering::SeqCst);
    } else {
        log::info!("📋 Slot {} is empty", slot);
    }
}

#[cfg(target_os = "macos")]
fn simulate_paste() {
    let script = r#"tell application "System Events"
  keystroke "v" using command down
end tell"#;
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output();
}

#[cfg(not(target_os = "macos"))]
fn simulate_paste() {
    if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
        let _ = enigo.key(Key::Control, Direction::Press);
        let _ = enigo.key(Key::Unicode('v'), Direction::Click);
        let _ = enigo.key(Key::Control, Direction::Release);
    }
}

#[cfg(not(target_os = "macos"))]
fn simulate_undo() {
    if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
        let _ = enigo.key(Key::Control, Direction::Press);
        let _ = enigo.key(Key::Unicode('z'), Direction::Click);
        let _ = enigo.key(Key::Control, Direction::Release);
    }
}

#[cfg(target_os = "macos")]
fn get_frontmost_app_name() -> Option<String> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to get name of first application process whose frontmost is true")
        .output()
        .ok()?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if name.is_empty() { None } else { Some(name) }
    } else {
        None
    }
}

/// Name + bundle of the frontmost app (one AppleScript round-trip).
#[cfg(target_os = "macos")]
fn get_frontmost_app_name_and_bundle() -> Option<(String, String)> {
    const SEP: &str = "\u{241f}"; // SYMBOL FOR UNIT SEPARATOR — won't appear in app names
    let script = format!(
        r#"tell application "System Events"
  set proc to first application process whose frontmost is true
  set procName to name of proc
  try
    set bid to bundle identifier of proc as string
  on error
    set bid to ""
  end try
  return procName & "{sep}" & bid
end tell"#,
        sep = SEP
    );
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut parts = s.splitn(2, SEP);
    let name = parts.next()?.trim().to_string();
    let bundle = parts.next().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some((name, bundle))
}

/// True when plain Ctrl+C should not trigger clipd slot copy / HUD (terminal interrupt, IDE shell, etc.).
#[cfg(target_os = "macos")]
fn is_terminal_frontmost() -> bool {
    let (name, bundle) = match get_frontmost_app_name_and_bundle() {
        Some(p) => p,
        None => {
            // Fall back to name-only if combined query fails
            let Some(n) = get_frontmost_app_name() else {
                return false;
            };
            (n, String::new())
        }
    };
    let nl = name.to_lowercase();
    let bl = bundle.to_lowercase();

    // Bundle IDs (reliable for Terminal, iTerm, VS Code, Warp, …)
    if bl.starts_with("com.apple.terminal") {
        return true;
    }
    if bl == "com.googlecode.iterm2" {
        return true;
    }
    if bl.contains("warp") && (bl.contains("dev.warp") || bl.starts_with("dev.warp")) {
        return true;
    }
    if bl.contains("alacritty") {
        return true;
    }
    if bl.contains("kitty") && bl.contains("kovidgoyal") {
        return true;
    }
    if bl.contains("wezterm") {
        return true;
    }
    if bl.contains("ghostty") {
        return true;
    }
    if bl.contains("tabby") {
        return true;
    }
    if bl == "com.microsoft.vscode" || bl.contains("vscodeinsiders") {
        return true;
    }
    // Cursor (and similar) ship as com.todesktop.* — only exempt when the app name says Cursor
    if bl.starts_with("com.todesktop.") && nl.contains("cursor") {
        return true;
    }
    // JetBrains terminals run inside the IDE bundle
    if bl.contains("jetbrains") {
        return true;
    }

    // Name heuristics (bundle sometimes empty for helper processes)
    nl.contains("terminal")
        || nl.contains("iterm")
        || nl.contains("warp")
        || nl.contains("kitty")
        || nl.contains("alacritty")
        || nl.contains("hyper")
        || nl.contains("wezterm")
        || nl.contains("ghostty")
        || nl.contains("tabby")
        || nl == "rio"
        || nl.contains("cursor")
        || nl.contains("visual studio code")
        || nl == "code"
        || nl.contains("windsurf")
        || nl.contains("zed")
        || nl.contains("fleet")
}

/// Show a brief native HUD overlay so the user sees the slot they're
/// targeting in real-time as they multi-tap. Kills any prior HUD first
/// so rapid taps update the display instead of stacking.
fn show_slot_notification(action: &str, slot: u8) {
    show_hud(&format!("📋 {} → Slot {}", action, slot));
}

/// Show a brief HUD overlay with arbitrary text.
/// Respects the user's hud_enabled setting.
#[cfg(target_os = "macos")]
fn show_hud(text: &str) {
    let settings = load_paste_transform_settings();
    if !settings.hud_enabled {
        log::info!("HUD suppressed (hud_enabled=false)");
        return;
    }

    use std::sync::Mutex;
    use std::sync::OnceLock;

    static LAST_PID: OnceLock<Mutex<Option<u32>>> = OnceLock::new();
    let pid_lock = LAST_PID.get_or_init(|| Mutex::new(None));

    if let Ok(mut prev) = pid_lock.lock() {
        if let Some(pid) = prev.take() {
            let _ = std::process::Command::new("kill").arg(pid.to_string()).output();
        }
    }

    let hud_bin = find_hud_binary();
    log::info!("HUD: launching {} with text {:?}", hud_bin.display(), text);

    match std::process::Command::new(&hud_bin)
        .arg(text)
        .spawn()
    {
        Ok(child) => {
            log::info!("HUD: spawned pid {}", child.id());
            if let Ok(mut prev) = pid_lock.lock() {
                *prev = Some(child.id());
            }
        }
        Err(e) => log::warn!("HUD overlay failed: {} (looked for {})", e, hud_bin.display()),
    }
}

#[cfg(not(target_os = "macos"))]
fn show_hud(text: &str) {
    let settings = load_paste_transform_settings();
    if !settings.hud_enabled {
        return;
    }
    log::info!("HUD: {}", text);
    std::thread::spawn({
        let text = text.to_string();
        move || {
            let _ = notify_rust::Notification::new()
                .summary("clipd")
                .body(&text)
                .timeout(notify_rust::Timeout::Milliseconds(800))
                .show();
        }
    });
}

/// Find the `clipd-hud` binary next to the current executable, or in PATH.
#[cfg(target_os = "macos")]
fn find_hud_binary() -> std::path::PathBuf {
    if let Ok(from_env) = std::env::var("CLIPD_HUD_BIN") {
        let p = std::path::PathBuf::from(from_env);
        if p.is_file() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("clipd-hud"));
        }
        if let Ok(canonical) = exe.canonicalize() {
            if let Some(dir) = canonical.parent() {
                candidates.push(dir.join("clipd-hud"));
            }
        }
        for c in candidates {
            if c.is_file() {
                return c;
            }
        }
    }
    std::path::PathBuf::from("clipd-hud")
}

#[cfg(target_os = "macos")]
fn undo_then_paste(undo_count: u8) -> Result<(), Box<dyn std::error::Error>> {
    // Send undos one at a time with enough delay for the app to process each.
    let undos: String = (0..undo_count)
        .map(|_| "  keystroke \"z\" using command down\n  delay 0.12")
        .collect::<Vec<_>>()
        .join("\n");

    let script = format!(
        r#"tell application "System Events"
{}
  delay 0.15
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

#[cfg(not(target_os = "macos"))]
fn undo_then_paste(undo_count: u8) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..undo_count {
        simulate_undo();
        std::thread::sleep(Duration::from_millis(120));
    }
    std::thread::sleep(Duration::from_millis(150));
    simulate_paste();
    Ok(())
}

fn open_tui_search() {
    log::info!("🔍 Opening TUI search...");
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };

    #[cfg(target_os = "macos")]
    {
        let cmd = format!("cd /tmp && {} search", exe.to_string_lossy());
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

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let _ = std::process::Command::new(&exe)
            .arg("search")
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn();
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = std::process::Command::new(&exe).arg("search").spawn();
    }
}

fn open_gui() {
    log::info!("🖥️  Opening GUI...");
    // Look for clipd-gui next to the current binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(target_os = "windows")]
            {
                for name in ["clipd-gui.exe", "clipd-gui"] {
                    let candidate = dir.join(name);
                    if candidate.exists()
                        && std::process::Command::new(&candidate)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .is_ok()
                    {
                        return;
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let candidate = dir.join("clipd-gui");
                if candidate.exists()
                    && std::process::Command::new(&candidate)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .is_ok()
                {
                    return;
                }
            }
        }
    }
    // Fallback: try PATH
    #[cfg(target_os = "windows")]
    {
        for name in ["clipd-gui.exe", "clipd-gui"] {
            if std::process::Command::new(name)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if std::process::Command::new("clipd-gui")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
    log::warn!("clipd-gui binary not found — build it with: cargo build --release -p clipd-gui");
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
        release_daemon_lock();
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
    // macOS key-repeat sends many KeyPress events while V/C is held. Count one tap per
    // physical press→release, not per repeat tick (otherwise tap #16 → wrong slot / empty).
    let mut latch_cmd_shift_v = false;
    let mut latch_cmd_opt_v = false;
    let mut latch_ctrl_r = false;
    let mut latch_ctrl_t = false;
    let mut latch_ctrl_g = false;
    let mut latch_cmd_ctrl_c = false;
    let mut latch_cmd_ctrl_v = false;
    let mut latch_cmd_c = false;
    let mut latch_cmd_v = false;
    let mut latch_ctrl_c = false;
    let mut latch_ctrl_v = false;

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

                // Ctrl+Shift+V → smart paste (transform clipboard + paste)
                if is_ctrl_shift(&pressed_mods) && key == RKey::KeyV {
                    if !latch_cmd_shift_v {
                        latch_cmd_shift_v = true;
                        log::info!("⌨️  Ctrl+Shift+V → smart paste");
                        let _ = tx.send(HotkeyTick::SmartPaste);
                    }
                    return;
                }

                // Cmd+Option+V → sequence paste
                if is_cmd_opt(&pressed_mods) && key == RKey::KeyV {
                    if !latch_cmd_opt_v {
                        latch_cmd_opt_v = true;
                        log::info!("⌨️  Cmd+Option+V → sequence paste");
                        let _ = tx.send(HotkeyTick::SequencePaste);
                    }
                    return;
                }

                // Ctrl+R → open search TUI (check before Ctrl+C/V)
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyR {
                    if !latch_ctrl_r {
                        latch_ctrl_r = true;
                        log::info!("⌨️  Ctrl+R → open search");
                        let _ = tx.send(HotkeyTick::OpenTui);
                    }
                    return;
                }

                // Ctrl+T → open TUI
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyT {
                    if !latch_ctrl_t {
                        latch_ctrl_t = true;
                        log::info!("⌨️  Ctrl+T → open TUI");
                        let _ = tx.send(HotkeyTick::OpenTui);
                    }
                    return;
                }

                // Ctrl+G → open GUI
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyG {
                    if !latch_ctrl_g {
                        latch_ctrl_g = true;
                        log::info!("⌨️  Ctrl+G → open GUI");
                        let _ = tx.send(HotkeyTick::OpenGui);
                    }
                    return;
                }

                // Cmd+Ctrl+C → treat as Ctrl+C (user transitioning from Cmd+C to save slot)
                if has_cmd(&pressed_mods) && has_ctrl(&pressed_mods) && key == RKey::KeyC {
                    if !latch_cmd_ctrl_c {
                        latch_cmd_ctrl_c = true;
                        let _ = tx.send(HotkeyTick::CtrlCTap);
                    }
                    return;
                }
                // Cmd+Ctrl+V → treat as Ctrl+V (user transitioning from Cmd+V)
                if has_cmd(&pressed_mods) && has_ctrl(&pressed_mods) && key == RKey::KeyV {
                    if !latch_cmd_ctrl_v {
                        latch_cmd_ctrl_v = true;
                        let _ = tx.send(HotkeyTick::CtrlVTap);
                    }
                    return;
                }

                // Cmd+C (just Cmd, no Ctrl/Shift/Option)
                if is_cmd_only(&pressed_mods) && key == RKey::KeyC {
                    if !latch_cmd_c {
                        latch_cmd_c = true;
                        let _ = tx.send(HotkeyTick::CmdCTap);
                    }
                    return;
                }
                // Cmd+V (just Cmd, no Ctrl/Shift/Option)
                if is_cmd_only(&pressed_mods) && key == RKey::KeyV {
                    if !latch_cmd_v {
                        latch_cmd_v = true;
                        let _ = tx.send(HotkeyTick::CmdVTap);
                    }
                    return;
                }

                // Ctrl+C (just Ctrl, no Cmd/Shift/Option)
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyC {
                    if !latch_ctrl_c {
                        latch_ctrl_c = true;
                        // In terminal apps Ctrl+C is interrupt — don't treat as slot copy / HUD.
                        if !is_terminal_frontmost() {
                            let _ = tx.send(HotkeyTick::CtrlCTap);
                        }
                    }
                    return;
                }
                // Ctrl+V (just Ctrl, no Cmd/Shift/Option)
                if is_ctrl_only(&pressed_mods) && key == RKey::KeyV {
                    if !latch_ctrl_v {
                        latch_ctrl_v = true;
                        let _ = tx.send(HotkeyTick::CtrlVTap);
                    }
                    return;
                }
            }
            EventType::KeyRelease(key) => {
                if is_modifier_key(key) {
                    pressed_mods.remove(&key);
                    // After Cmd+C the OS often never delivers KeyRelease for C; releasing Command
                    // must arm the next Cmd+C / Cmd+V chord.
                    if matches!(key, RKey::MetaLeft | RKey::MetaRight) {
                        latch_cmd_c = false;
                        latch_cmd_v = false;
                        latch_cmd_shift_v = false;
                        latch_cmd_ctrl_c = false;
                        latch_cmd_ctrl_v = false;
                    }
                    if matches!(key, RKey::ControlLeft | RKey::ControlRight) {
                        latch_ctrl_c = false;
                        latch_ctrl_v = false;
                        latch_ctrl_t = false;
                        latch_ctrl_g = false;
                    }
                }
                match key {
                    RKey::KeyV => {
                        latch_cmd_shift_v = false;
                        latch_cmd_opt_v = false;
                        latch_cmd_ctrl_v = false;
                        latch_cmd_v = false;
                        latch_ctrl_v = false;
                    }
                    RKey::KeyC => {
                        latch_cmd_ctrl_c = false;
                        latch_cmd_c = false;
                        latch_ctrl_c = false;
                    }
                    RKey::KeyR => latch_ctrl_r = false,
                    RKey::KeyT => latch_ctrl_t = false,
                    RKey::KeyG => latch_ctrl_g = false,
                    _ => {}
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

/// Ctrl+Shift held (no Cmd, no Option)
#[cfg(target_os = "macos")]
fn is_ctrl_shift(pressed_mods: &HashSet<RKey>) -> bool {
    has_ctrl(pressed_mods)
        && (pressed_mods.contains(&RKey::ShiftLeft) || pressed_mods.contains(&RKey::ShiftRight))
        && !pressed_mods.contains(&RKey::MetaLeft)
        && !pressed_mods.contains(&RKey::MetaRight)
        && !pressed_mods.contains(&RKey::Alt)
        && !pressed_mods.contains(&RKey::AltGr)
}

/// Cmd+Option held (no Ctrl, no Shift)
#[cfg(target_os = "macos")]
fn is_cmd_opt(pressed_mods: &HashSet<RKey>) -> bool {
    has_cmd(pressed_mods)
        && (pressed_mods.contains(&RKey::Alt) || pressed_mods.contains(&RKey::AltGr))
        && !pressed_mods.contains(&RKey::ShiftLeft)
        && !pressed_mods.contains(&RKey::ShiftRight)
        && !pressed_mods.contains(&RKey::ControlLeft)
        && !pressed_mods.contains(&RKey::ControlRight)
}

/// Backfill embeddings for clips that don't have them yet.
fn backfill_embeddings(store: &ClipStore, config: &TransformConfig) {
    let unembedded = match store.get_unembedded_clip_ids(100) {
        Ok(ids) => ids,
        Err(_) => return,
    };

    if unembedded.is_empty() {
        log::info!("🧠 All clips already have embeddings");
        return;
    }

    log::info!(
        "🧠 Backfilling embeddings for {} clips...",
        unembedded.len()
    );

    let mut texts = Vec::new();
    let mut ids = Vec::new();
    for id in &unembedded {
        if let Ok(clip) = store.get_by_id(*id) {
            texts.push(clip.content);
            ids.push(*id);
        }
    }

    // Batch in groups of 20 to avoid hitting API limits
    for chunk_start in (0..texts.len()).step_by(20) {
        let chunk_end = (chunk_start + 20).min(texts.len());
        let text_refs: Vec<&str> = texts[chunk_start..chunk_end]
            .iter()
            .map(|s| s.as_str())
            .collect();

        match clipd_core::generate_embeddings_batch(&text_refs, config) {
            Ok(embeddings) => {
                for (i, emb) in embeddings.iter().enumerate() {
                    let clip_id = ids[chunk_start + i];
                    if let Err(e) = store.store_embedding(clip_id, emb) {
                        log::warn!("Failed to store embedding for clip {}: {}", clip_id, e);
                    }
                }
                log::info!(
                    "🧠 Embedded batch {}-{} of {}",
                    chunk_start + 1,
                    chunk_end,
                    texts.len()
                );
            }
            Err(e) => {
                log::warn!("Batch embedding failed: {}", e);
                break;
            }
        }
    }
}
