use arboard::Clipboard;
use clipd_core::{
    apply_transform, find_rules_for_app, generate_embedding, is_embedding_available, load_actions,
    load_paste_rules, load_paste_transform_settings, load_transform_config, release_daemon_lock,
    run_action, save_files, save_hotkey_status, save_rgba_image, suggest_smart_transform,
    try_acquire_daemon_lock, ActionOutput, ClipEntry, ClipEvent, ClipStore, ClipWatcher,
    CtrlSpaceAction, CustomAction, HotkeyStatus, OpenGuiHotkey, PaletteTrigger, PasteRulesConfig,
    PasteTransformSettings, SlotManager, TransformConfig, TransformKind, MAX_CLIP_SLOT,
};
#[cfg(target_os = "macos")]
use clipd_core::{
    available_targets, load_privacy_config, save_secret, SecretEntry, VaultTarget,
};
#[cfg(not(target_os = "macos"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
#[cfg(not(target_os = "macos"))]
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
#[cfg(target_os = "macos")]
use rdev::{grab, listen, Event, EventType, Key as RKey};
#[cfg(target_os = "windows")]
use rdev::{grab, Event, EventType, Key as RKey};
#[cfg(target_os = "macos")]
use std::collections::HashSet;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// In-process hosts (clipd-ui) can fire Settings shortcuts without the
/// CGEventTap — Carbon `RegisterEventHotKey` is enough for discrete chords.
static EXTERNAL_TICK_TX: Mutex<Option<mpsc::Sender<HotkeyTick>>> = Mutex::new(None);

/// Shortcut requests from the tray host's Carbon hotkey registrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutRequest {
    OpenGui,
    SlotMemory,
    CommandPalette,
    SlotPicker,
    /// Send the clipboard to the other Mac — the menu-bar and GUI entry point
    /// to the same action Ctrl+Shift+S triggers.
    Send,
}

/// Ask the running in-process daemon to perform a Settings shortcut action.
pub fn request_shortcut(req: ShortcutRequest) -> bool {
    let tick = match req {
        ShortcutRequest::OpenGui => HotkeyTick::OpenGui,
        ShortcutRequest::SlotMemory => HotkeyTick::SlotMemoryHud,
        ShortcutRequest::CommandPalette => HotkeyTick::CommandPalette,
        ShortcutRequest::SlotPicker => HotkeyTick::SlotPicker,
        ShortcutRequest::Send => HotkeyTick::SendToDevice,
    };
    EXTERNAL_TICK_TX
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().cloned())
        .is_some_and(|tx| tx.send(tick).is_ok())
}

fn install_external_tick_tx(tx: mpsc::Sender<HotkeyTick>) {
    if let Ok(mut guard) = EXTERNAL_TICK_TX.lock() {
        *guard = Some(tx);
    }
}

fn clear_external_tick_tx() {
    if let Ok(mut guard) = EXTERNAL_TICK_TX.lock() {
        *guard = None;
    }
}

/// How long to wait after the last tap before deciding the final slot.
///
/// This is the gap allowed *between* taps, not just the trailing wait — the
/// count commits and resets the moment it lapses. At 350ms, reaching slot 15
/// meant landing five taps in a drum roll; anyone counting deliberately got
/// five separate commits, all to slot 11. The higher banks are the ones people
/// count into, and counting is not something to be hurried.
///
/// The cost is that the slot commits this long after your last tap, which is a
/// background save nobody is waiting on.
const TAP_WINDOW: Duration = Duration::from_millis(650);
/// How long Option+C / Option+V waits for a following A-Z letter slot.
const LETTER_PREFIX_WINDOW: Duration = Duration::from_millis(900);
/// Max gap between two Cmd+C presses to count as a double-tap (quick letter save).
const QUICK_DOUBLE_WINDOW: Duration = Duration::from_millis(400);
/// How long after a double Cmd+C a letter still means "and put it in that slot
/// too".
///
/// This used to be half a second, and it was half a second because the numeric
/// commit was *held* for exactly that long so a following letter could cancel
/// it. That made it a race: press the letter in time and you got slot A, miss
/// and you got slot 2, and you had to know the rule to understand which.
///
/// The letter no longer cancels anything — the content lands in the numeric
/// slot immediately and in the letter slot as well. Nothing is waiting on you,
/// so the window can be long enough that nobody has to hurry.
const QUICK_LETTER_GRACE: Duration = Duration::from_millis(2000);
/// Ignore duplicate key events faster than this (macOS key-repeat / missing KeyRelease on C/V).
const TAP_DEBOUNCE: Duration = Duration::from_millis(65);

pub fn run_daemon() -> Result<(), Box<dyn std::error::Error>> {
    run_daemon_with_stop(Arc::new(AtomicBool::new(false)), true)
}

/// Run the daemon using a caller-supplied stop flag.
///
/// This lets an embedding process (e.g. `clipd-ui`) host the daemon — and
/// crucially the macOS keyboard listener — *inside its own process*, instead of
/// spawning a separate `clipd daemon` child. macOS keys Input Monitoring /
/// Accessibility per binary, and with ad-hoc signing the grant given to
/// `clipd-ui` does not reliably propagate to a spawned `clipd` child, so the
/// listener never receives key events. Running in-process makes the binary that
/// *holds* the permission the same one that *uses* it.
///
/// `install_ctrlc`: install a SIGINT handler (standalone CLI use). In-process
/// GUI hosts pass `false` so they keep control of their own signal handling.
pub fn run_daemon_with_stop(
    stop: Arc<AtomicBool>,
    install_ctrlc: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !try_acquire_daemon_lock() {
        log::info!("clipd daemon is already running — skipping duplicate launch");
        return Ok(());
    }

    println!("  ╔═══════════════════════════════════════╗");
    println!(
        "  ║         clipd daemon v{}           ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("  ║   AI clipboard for developers         ║");
    println!("  ╚═══════════════════════════════════════╝");
    println!();

    // Anonymous telemetry — one HTTP GET to the configured endpoint on startup.
    // Completely silent if disabled or endpoint not configured.
    // Runs in a background thread so it never delays startup.
    clipd_core::ping();

    let db_path = ClipStore::default_path();
    println!("  📦 Database: {}", db_path.display());
    let _store = ClipStore::new(&db_path)?;
    // Active slots are process-shared and survive restarts. This is especially
    // important on Windows where tray/GUI upgrades can briefly overlap: the
    // hook that observes paste must see the slot saved by the hook that saw copy.
    let slot_manager = SlotManager::persistent(&db_path)
        .map_err(|e| format!("Failed to open active-slot store: {e}"))?;
    let db_path_clone = db_path.clone();

    let stop_watcher = stop.clone();
    let stop_hotkey = stop.clone();

    let suppress = Arc::new(AtomicBool::new(false));
    let suppress_watcher = suppress.clone();

    let refresh_hash = Arc::new(AtomicBool::new(false));
    let refresh_hash_watcher = refresh_hash.clone();

    if install_ctrlc {
        let stop_ctrlc = stop.clone();
        setup_ctrlc(stop_ctrlc);
    }

    // ── Clipboard Watcher Thread ──
    // Bound the channel to prevent unbounded memory growth if the watcher produces
    // faster than the store-writer can consume. 100 events is ~a few KB at most.
    const CLIP_CHANNEL_CAP: usize = 100;
    let (clip_tx, clip_rx) = mpsc::sync_channel::<ClipEvent>(CLIP_CHANNEL_CAP);
    // Clone for use in execute_copy calls (hotkey path) — watcher consumes clip_tx.
    let persist_tx = clip_tx.clone();
    let watcher = ClipWatcher::new(500);
    let watcher_slot_mgr = slot_manager.clone();
    let watcher_handle = std::thread::Builder::new()
        .name("clipd-watcher".into())
        .spawn(move || {
            watcher.watch(
                clip_tx,
                stop_watcher,
                suppress_watcher,
                refresh_hash_watcher,
                Some(watcher_slot_mgr),
            );
        })?;

    // ── Sync Inbox Thread ──
    // Announce this Mac in the shared iCloud folder and collect anything other
    // Macs have sent it. Polling rather than FSEvents: iCloud materialises
    // downloaded files in ways that do not always produce a filesystem event,
    // and a stat of one small directory every few seconds costs nothing.
    let inbox_db_path = db_path.clone();
    let inbox_stop = stop.clone();
    let _inbox_handle = std::thread::Builder::new()
        .name("clipd-sync-inbox".into())
        .spawn(move || run_sync_inbox(&inbox_db_path, inbox_stop))?;

    // ── LAN Transport ──
    // Direct machine-to-machine sending. Independent of the folder transport
    // above: either can be unavailable without disabling the other.
    let lan_db_path = db_path.clone();
    let lan_stop = stop.clone();
    let _lan_handle = std::thread::Builder::new()
        .name("clipd-lan".into())
        .spawn(move || run_lan_transport(&lan_db_path, lan_stop))?;

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
            // Prune old clips so the DB never grows beyond 10,000 entries.
            const MAX_STORED_CLIPS: usize = 10_000;
            if let Err(e) = store.prune_if_needed(MAX_STORED_CLIPS) {
                log::warn!("Clip pruning failed (non-fatal): {}", e);
            }
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
                    ClipEvent::NewClip(mut entry) => {
                        // Slot 0 (the live OS-clipboard mirror) always tracks the
                        // latest copy so paste works; persisting to searchable
                        // history is what "Remember copied items" gates.
                        slot_writer.copy_to_slot(0, &entry.content).ok();
                        if !load_paste_transform_settings().remember_clipboard {
                            continue;
                        }
                        // Capture the frontmost app at copy time — used both to
                        // tag the clip and to auto-route it into a collection.
                        #[cfg(target_os = "macos")]
                        if entry.source_app.is_none() {
                            entry.source_app = get_frontmost_app_name();
                        }
                        let route_app = entry.source_app.clone();
                        let content_for_embed = entry.content.clone();
                        match store.insert(&entry) {
                            Ok(id) => {
                                // Auto-route into any collection bound to this app.
                                if let Some(app) = route_app {
                                    if let Ok(Some(coll)) = store.collection_for_app(&app) {
                                        let _ = store.add_clip_to_collection(coll.id, id);
                                        log::info!(
                                            "📂 Auto-added clip #{} to collection '{}' (from {})",
                                            id,
                                            coll.name,
                                            app
                                        );
                                    }
                                }
                                log::info!(
                                    "Saved clip #{}: {} [{}] {}",
                                    id,
                                    entry.content_type.icon(),
                                    entry.content_type.as_str(),
                                    truncate(&entry.preview, 60)
                                );
                                run_auto_actions(&entry);
                                // Spawn enrichment (link preview, translation, tagging).
                                clipd_core::spawn_enrichment(
                                    id,
                                    entry.content.clone(),
                                    entry.content_type,
                                );
                                if embed_available {
                                    match generate_embedding(&content_for_embed, &embed_config) {
                                        Ok(emb) => {
                                            if let Err(e) = store.store_embedding(id, &emb) {
                                                log::warn!("Failed to store embedding: {}", e);
                                            } else {
                                                log::debug!(
                                                    "🧠 Embedded clip #{} ({} dims)",
                                                    id,
                                                    emb.len()
                                                );
                                            }
                                        }
                                        Err(e) => log::debug!("Embedding skipped: {}", e),
                                    }
                                }
                            }
                            Err(e) => log::error!("Failed to save clip: {}", e),
                        }
                    }
                    ClipEvent::SensitiveClip {
                        kinds,
                        secret,
                        stored,
                    } => {
                        log::info!("🔐 Password detected ({}) — offering vault save", kinds);
                        #[cfg(target_os = "macos")]
                        offer_vault_save(&kinds, secret, stored);
                        #[cfg(not(target_os = "macos"))]
                        let _ = (secret, stored);
                    }
                    ClipEvent::NewImage {
                        width,
                        height,
                        rgba,
                        mut source_app,
                    } => {
                        // Images only persist when history is on (same gate as text).
                        if !load_paste_transform_settings().remember_clipboard {
                            continue;
                        }
                        #[cfg(target_os = "macos")]
                        if source_app.is_none() {
                            source_app = get_frontmost_app_name();
                        }
                        // 1. Write the PNG + thumbnail to disk.
                        let saved = match save_rgba_image(width, height, &rgba) {
                            Ok(s) => s,
                            Err(e) => {
                                log::error!("Failed to save image clip: {}", e);
                                continue;
                            }
                        };
                        // 2. On-device OCR (Apple Vision on macOS; no-op elsewhere).
                        let ocr_text = run_ocr(&saved.full_path);
                        // 3. Build + insert the clip (dedup by image hash).
                        let entry = ClipEntry::new_image(
                            saved.hash.clone(),
                            saved.full_path.to_string_lossy().to_string(),
                            saved.thumb_path.to_string_lossy().to_string(),
                            ocr_text.clone(),
                            source_app,
                            saved.width,
                            saved.height,
                        );
                        match store.insert(&entry) {
                            Ok(id) => {
                                log::info!(
                                    "Saved image clip #{} ({}×{}){}",
                                    id,
                                    saved.width,
                                    saved.height,
                                    match &ocr_text {
                                        Some(t) if !t.trim().is_empty() =>
                                            format!(" · OCR {} chars", t.len()),
                                        _ => String::new(),
                                    }
                                );
                                // Spawn enrichment for image (tagging, etc.).
                                clipd_core::spawn_enrichment(
                                    id,
                                    ocr_text.clone().unwrap_or_default(),
                                    clipd_core::ContentType::Image,
                                );
                            }
                            Err(e) => log::error!("Failed to save image clip: {}", e),
                        }
                    }
                    ClipEvent::NewFiles {
                        paths,
                        mut source_app,
                        source_title,
                    } => {
                        // Files persist under the same gate as text and images.
                        if !load_paste_transform_settings().remember_clipboard {
                            continue;
                        }
                        #[cfg(target_os = "macos")]
                        if source_app.is_none() {
                            source_app = get_frontmost_app_name();
                        }
                        // Copy the bytes into the blob store. Done here rather
                        // than in the watcher so a large selection cannot stall
                        // clipboard polling.
                        let files = save_files(&paths);
                        if files.is_empty() {
                            log::warn!("File clip skipped — none of the files could be read");
                            continue;
                        }
                        let mut entry = ClipEntry::new_files(files, source_app);
                        entry.source_title = source_title;
                        match store.insert(&entry) {
                            Ok(id) => log::info!(
                                "Saved file clip #{}: {}",
                                id,
                                truncate(&entry.preview, 60)
                            ),
                            Err(e) => log::error!("Failed to save file clip: {}", e),
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
        #[cfg(not(target_os = "windows"))]
        {
            let digit_codes = [
                (Code::Digit1, Code::Numpad1),
                (Code::Digit2, Code::Numpad2),
                (Code::Digit3, Code::Numpad3),
                (Code::Digit4, Code::Numpad4),
                (Code::Digit5, Code::Numpad5),
                (Code::Digit6, Code::Numpad6),
                (Code::Digit7, Code::Numpad7),
                (Code::Digit8, Code::Numpad8),
                (Code::Digit9, Code::Numpad9),
            ];
            for (i, (top_code, numpad_code)) in digit_codes.iter().enumerate() {
                let slot_num = (i + 1) as u8;
                for code in [*top_code, *numpad_code] {
                    let copy_hk = HotKey::new(Some(Modifiers::SUPER | Modifiers::CONTROL), code);
                    if let Err(e) = hotkey_manager.register(copy_hk) {
                        log::warn!(
                            "Failed to register Super+Ctrl+{} ({:?}): {}",
                            slot_num,
                            code,
                            e
                        );
                    } else {
                        registered_hotkeys.push((copy_hk, FinalAction::CopyToSlot(slot_num)));
                    }
                    let paste_hk = HotKey::new(
                        Some(Modifiers::SUPER | Modifiers::CONTROL | Modifiers::ALT),
                        code,
                    );
                    if let Err(e) = hotkey_manager.register(paste_hk) {
                        log::warn!(
                            "Failed to register Super+Ctrl+Alt+{} ({:?}): {}",
                            slot_num,
                            code,
                            e
                        );
                    } else {
                        registered_hotkeys.push((paste_hk, FinalAction::PasteFromSlot(slot_num)));
                    }
                }
            }
        }

        // Windows deliberately avoids direct Win/Ctrl/Alt letter-slot chords.
        // They collide with OS shortcuts, browser menus, AltGr keyboard layouts,
        // and app accelerators. Numeric slots use Ctrl+C/Ctrl+V multi-tap; A-Z
        // slots should be addressed through a Clipd-owned palette/prefix UI.
        // NOTE: plain Ctrl+V is deliberately NOT registered as a global hotkey.
        // RegisterHotKey swallows the key system-wide — including the user's
        // normal pastes — and breaks pasting whenever slot state is empty.
        // Ctrl+V multi-tap lives in the passive grab hook instead.

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
        // Open-GUI chord honors the same "Open clipd shortcut" setting as macOS.
        // Note: registered at daemon startup — changing it in Settings takes
        // effect after a daemon restart. Cmd maps to the Win/Super key here.
        let gui_hotkey_setting = load_paste_transform_settings().open_gui_hotkey;
        let gui_mods = match gui_hotkey_setting {
            OpenGuiHotkey::CtrlG => Some(Modifiers::CONTROL),
            OpenGuiHotkey::AltG => Some(Modifiers::ALT),
            OpenGuiHotkey::CmdShiftG => Some(Modifiers::SUPER | Modifiers::SHIFT),
            OpenGuiHotkey::CtrlShiftG => Some(Modifiers::CONTROL | Modifiers::SHIFT),
            OpenGuiHotkey::CtrlSpace => Some(Modifiers::CONTROL),
            OpenGuiHotkey::OptSpace => Some(Modifiers::ALT),
            OpenGuiHotkey::Disabled => None,
        };
        let gui_code = if gui_hotkey_setting.uses_space_key() {
            Code::Space
        } else {
            Code::KeyG
        };
        if let Some(mods) = gui_mods {
            let gui_hk = HotKey::new(Some(mods), gui_code);
            if let Err(e) = hotkey_manager.register(gui_hk) {
                log::warn!(
                    "Failed to register {} (open GUI): {}",
                    gui_hotkey_setting.label(),
                    e
                );
            } else {
                registered_hotkeys.push((gui_hk, FinalAction::OpenGui));
            }
            // Windows: Alt+G always opens the GUI, even for users whose saved
            // settings predate the AltG option (their config pins CtrlG).
            #[cfg(target_os = "windows")]
            if gui_hotkey_setting != OpenGuiHotkey::AltG {
                let alt_g = HotKey::new(Some(Modifiers::ALT), Code::KeyG);
                if hotkey_manager.register(alt_g).is_ok() {
                    registered_hotkeys.push((alt_g, FinalAction::OpenGui));
                }
            }
        } else {
            log::info!("Open-GUI hotkey disabled in settings");
        }

        // Conflict-free slot picker (the "Clipd-owned palette" letter slots want):
        // one safe leader — Ctrl+` — opens the popup; press a slot key (1-9 / A-Z)
        // to paste it. No Win/Alt chord conflicts, scales to all 35 slots.
        #[cfg(target_os = "windows")]
        {
            let picker_hk = HotKey::new(Some(Modifiers::CONTROL), Code::Backquote);
            if let Err(e) = hotkey_manager.register(picker_hk) {
                log::warn!("Failed to register Ctrl+` slot picker: {}", e);
            } else {
                registered_hotkeys.push((picker_hk, FinalAction::OpenPicker));
            }
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
    println!("       Ctrl+V × 1 → normal paste      Ctrl+C × 1 → save to slot 1");
    println!("       Ctrl+V × 2 → paste slot 2      Ctrl+C × 2 → save to slot 2");
    println!();
    println!("     Multi-tap Cmd/Ctrl + C/V → slots 1..9 (after pause)");
    println!();
    println!("     Ctrl+Shift+V  → smart paste (transform clipboard + paste)");
    println!("     Cmd+Option+Shift+V → sequence paste (auto-increment through slots)");
    println!("     Ctrl+Option+1..9 → paste slot directly");
    println!("     Ctrl+Shift+Option+1..9 → save clipboard to slot directly");
    println!(
        "     Excel/developer mode: Cmd+C/V taps → slots 1..9, Option+C/V taps → slots 11..30"
    );
    println!("     Letter slots A..Z (31..56):");
    println!("       Cmd+Option+C then a letter → copy to that letter slot");
    println!("       Cmd+Option+V then a letter → paste that letter slot");
    println!("       Ctrl+Option+A..Z          → paste, one chord");
    println!("       Ctrl+Shift+Option+A..Z    → copy, one chord");
    println!("     Ctrl+Option+Space → show recent slot memory");
    println!();
    println!("     Collect mode (grab a batch, no slot picking):");
    println!("       Ctrl+Option+`  → toggle on/off (also auto-starts on 2 quick copies)");
    println!("       then just Cmd+C each item → stacks into slots 1..9");
    println!("       Cmd+Shift+V    → pick one to paste from the visual list");
    println!();
    println!(
        "     Ctrl+T → open TUI        {} → open GUI",
        load_paste_transform_settings().open_gui_hotkey.label()
    );
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
        // Tray host (clipd-ui) registers Carbon hotkeys for Settings chords and
        // posts through this channel — works even while the CGEventTap retries.
        install_external_tick_tx(hotkey_tx.clone());
        let stop_listener = stop.clone();

        std::thread::Builder::new()
            .name("clipd-hotkey-listener".into())
            .spawn(move || {
                let fallback_tx = hotkey_tx.clone();
                let fallback_stop = stop_listener.clone();
                run_macos_hotkey_listener_with_retry(
                    hotkey_tx,
                    stop_listener,
                    fallback_tx,
                    fallback_stop,
                );
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
        let mut upper_c_taps: u8 = 0;
        let mut upper_c_deadline: Option<Instant> = None;
        let mut upper_v_taps: u8 = 0;
        let mut upper_v_deadline: Option<Instant> = None;

        let mut last_cmd_c = Instant::now() - Duration::from_secs(10);
        let mut last_cmd_v = Instant::now() - Duration::from_secs(10);
        let mut last_ctrl_c = Instant::now() - Duration::from_secs(10);
        let mut last_ctrl_v = Instant::now() - Duration::from_secs(10);
        let mut last_upper_c = Instant::now() - Duration::from_secs(10);
        let mut last_upper_v = Instant::now() - Duration::from_secs(10);

        let mut sequence_slot: u8 = 1;
        let mut cmd_v_saved_clipboard: Option<String> = None;

        // ── Collect mode ──
        // When collecting, each plain Cmd+C lands in the next numbered slot
        // (1,2,3…) instead of always overwriting slot 1 — so the user can grab
        // a batch without choosing slots. Started explicitly (Ctrl+Option+`) or
        // automatically when two quick single copies happen in a row.
        let mut collecting = false;
        let mut collect_next: u8 = 1;
        let mut last_single_copy: Option<Instant> = None;
        const COLLECT_BANK_MAX: u8 = 9;
        const AUTO_COLLECT_WINDOW: Duration = Duration::from_millis(2500);
        const COLLECT_IDLE_RESET: Duration = Duration::from_secs(30);
        let paste_rules = load_paste_rules();
        let transform_config = load_transform_config();
        let mut paste_transform = load_paste_transform_settings();

        while !stop_hotkey.load(Ordering::Relaxed) {
            if let Ok(tick) = hotkey_rx.recv_timeout(Duration::from_millis(50)) {
                paste_transform = load_paste_transform_settings();
                match tick {
                    HotkeyTick::CmdCTap => {
                        let now = Instant::now();
                        if now.duration_since(last_cmd_c) < TAP_DEBOUNCE {
                            continue;
                        }
                        last_cmd_c = now;
                        cmd_c_taps = (cmd_c_taps + 1).min(primary_tap_slot_limit(&paste_transform));
                        // The numeric slot commits on its own schedule now. It
                        // used to stall for the letter window so a letter could
                        // cancel it, which is what turned a convenience into
                        // something you could get wrong.
                        cmd_c_deadline = Some(now + TAP_WINDOW);
                        if cmd_c_taps >= 1 {
                            let slot = primary_slot_for_taps(cmd_c_taps, &paste_transform);
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
                        cmd_v_taps = (cmd_v_taps + 1).min(primary_tap_slot_limit(&paste_transform));
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
                            let slot = primary_slot_for_taps(cmd_v_taps, &paste_transform);
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
                        ctrl_c_taps =
                            (ctrl_c_taps + 1).min(primary_tap_slot_limit(&paste_transform));
                        ctrl_c_deadline = Some(now + TAP_WINDOW);
                        let slot = primary_slot_for_taps(ctrl_c_taps, &paste_transform);
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
                        ctrl_v_taps =
                            (ctrl_v_taps + 1).min(primary_tap_slot_limit(&paste_transform));
                        ctrl_v_deadline = Some(now + TAP_WINDOW);
                        let slot = primary_slot_for_taps(ctrl_v_taps, &paste_transform);
                        log::info!("⌨️  Ctrl+V tap #{} → slot {}", ctrl_v_taps, slot);
                        #[cfg(target_os = "macos")]
                        show_slot_notification("Paste", slot);
                    }
                    HotkeyTick::UpperCopyTap => {
                        let now = Instant::now();
                        if now.duration_since(last_upper_c) < TAP_DEBOUNCE {
                            continue;
                        }
                        last_upper_c = now;
                        upper_c_taps = (upper_c_taps + 1).min(20);
                        upper_c_deadline = Some(now + TAP_WINDOW);
                        let slot = upper_slot_for_taps(upper_c_taps);
                        log::info!("⌨️  Option+C tap #{} → slot {}", upper_c_taps, slot);
                        #[cfg(target_os = "macos")]
                        show_slot_notification("Copy", slot);
                    }
                    HotkeyTick::UpperPasteTap => {
                        let now = Instant::now();
                        if now.duration_since(last_upper_v) < TAP_DEBOUNCE {
                            continue;
                        }
                        last_upper_v = now;
                        upper_v_taps = (upper_v_taps + 1).min(20);
                        upper_v_deadline = Some(now + TAP_WINDOW);
                        let slot = upper_slot_for_taps(upper_v_taps);
                        log::info!("⌨️  Option+V tap #{} → slot {}", upper_v_taps, slot);
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
                        execute_smart_paste(&suppress, &transform_config, &paste_transform);
                    }
                    HotkeyTick::SlotPicker => {
                        // The palette is the GUI's real type-to-filter search
                        // (recall by content or "from <app>"), not a slot list.
                        #[cfg(target_os = "macos")]
                        if paste_transform.palette_enabled {
                            open_gui();
                        } else {
                            open_slot_picker(&hotkey_slot_mgr, &suppress);
                        }
                        #[cfg(not(target_os = "macos"))]
                        open_slot_picker(&hotkey_slot_mgr, &suppress);
                    }
                    HotkeyTick::CopySlotPicker => {
                        open_copy_slot_picker(&hotkey_slot_mgr, &persist_tx);
                    }
                    HotkeyTick::CopyClipboardToSlot(slot) => {
                        execute_copy(slot, &hotkey_slot_mgr, &persist_tx);
                    }
                    HotkeyTick::CopySelectionToSlot(slot) => {
                        if paste_transform.letter_slots_enabled {
                            // Additive, not exclusive. The content is already on
                            // its way to the numeric slot; this puts it in the
                            // letter slot as well rather than racing to take it
                            // back. Same content in two places is harmless — and
                            // it means there is no deadline to miss.
                            simulate_copy();
                            execute_copy(slot, &hotkey_slot_mgr, &persist_tx);
                        }
                    }
                    HotkeyTick::PasteFromSlot(slot) => {
                        if slot <= 30 || paste_transform.letter_slots_enabled {
                            if slot >= 31 {
                                upper_v_taps = 0;
                                upper_v_deadline = None;
                            }
                            execute_direct_paste(
                                slot,
                                &hotkey_slot_mgr,
                                &suppress,
                                &transform_config,
                                &paste_transform,
                            );
                        }
                    }
                    HotkeyTick::SlotMemoryHud => {
                        #[cfg(target_os = "macos")]
                        show_slot_memory_hud(&hotkey_slot_mgr);
                    }
                    HotkeyTick::CommandPalette => {
                        #[cfg(target_os = "macos")]
                        show_command_palette_hud();
                    }
                    HotkeyTick::SendToDevice => {
                        send_clipboard_to_other_mac();
                    }
                    HotkeyTick::ToggleCollect => {
                        collecting = !collecting;
                        collect_next = 1;
                        last_single_copy = None;
                        log::info!("📦 Collect mode {}", if collecting { "ON" } else { "OFF" });
                        #[cfg(target_os = "macos")]
                        {
                            if collecting {
                                show_collect_hud(&hotkey_slot_mgr, 0, true);
                            } else {
                                show_collect_done_hud();
                            }
                        }
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
            //        2+ taps → save to slot N, optionally restore clipboard to slot 1
            // While collecting, a long pause means the batch is over — drop
            // back to normal slot-1 behavior so a much-later copy isn't appended.
            if collecting {
                if let Some(prev) = last_single_copy {
                    if now.duration_since(prev) > COLLECT_IDLE_RESET {
                        collecting = false;
                        collect_next = 1;
                        last_single_copy = None;
                    }
                }
            }

            if let Some(dl) = cmd_c_deadline {
                if now >= dl && cmd_c_taps > 0 {
                    if cmd_c_taps == 1 {
                        let commit = Instant::now();
                        // Auto-start: a second quick single copy turns collecting on.
                        if !collecting {
                            if let Some(prev) = last_single_copy {
                                if commit.duration_since(prev) <= AUTO_COLLECT_WINDOW {
                                    collecting = true;
                                    // Copy #1 is already in slot 1; this one is #2.
                                    collect_next = 2;
                                }
                            }
                        }
                        if collecting && collect_next <= COLLECT_BANK_MAX {
                            let slot = collect_next;
                            execute_copy(slot, &hotkey_slot_mgr, &persist_tx);
                            log::info!("📦 Collect → slot {}", slot);
                            collect_next += 1;
                            #[cfg(target_os = "macos")]
                            show_collect_hud(&hotkey_slot_mgr, slot, false);
                            if collect_next > COLLECT_BANK_MAX {
                                // Bank full — stop collecting but keep the items.
                                collecting = false;
                            }
                        } else {
                            execute_copy(1, &hotkey_slot_mgr, &persist_tx);
                            log::info!("⌨️  Cmd+C → auto-saved to slot 1");
                        }
                        last_single_copy = Some(commit);
                    } else {
                        let slot = primary_slot_for_taps(cmd_c_taps, &paste_transform);
                        execute_copy(slot, &hotkey_slot_mgr, &persist_tx);
                        // Only restore to slot 1 if the user wants that behavior
                        if paste_transform.copy_multi_tap_restore {
                            restore_clipboard_to_slot(
                                &hotkey_slot_mgr,
                                &suppress,
                                &refresh_hash,
                                1,
                            );
                        }
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
                        let slot = primary_slot_for_taps(cmd_v_taps, &paste_transform);
                        execute_undo_paste(slot, cmd_v_taps.min(2), &hotkey_slot_mgr, &suppress);
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
                    let slot = primary_slot_for_taps(ctrl_c_taps, &paste_transform);
                    execute_copy(slot, &hotkey_slot_mgr, &persist_tx);
                    ctrl_c_taps = 0;
                    ctrl_c_deadline = None;
                }
            }

            // Ctrl+V: 1+ taps → directly paste from slot (taps), no undo needed
            if let Some(dl) = ctrl_v_deadline {
                if now >= dl && ctrl_v_taps > 0 {
                    let slot = primary_slot_for_taps(ctrl_v_taps, &paste_transform);
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

            if let Some(dl) = upper_c_deadline {
                if now >= dl && upper_c_taps > 0 {
                    let slot = upper_slot_for_taps(upper_c_taps);
                    simulate_copy();
                    execute_copy(slot, &hotkey_slot_mgr, &persist_tx);
                    upper_c_taps = 0;
                    upper_c_deadline = None;
                }
            }

            if let Some(dl) = upper_v_deadline {
                if now >= dl && upper_v_taps > 0 {
                    let slot = upper_slot_for_taps(upper_v_taps);
                    execute_direct_paste(
                        slot,
                        &hotkey_slot_mgr,
                        &suppress,
                        &transform_config,
                        &paste_transform,
                    );
                    upper_v_taps = 0;
                    upper_v_deadline = None;
                }
            }
        }
    }

    // ── Main Event Loop (non-macOS) ──
    #[cfg(not(target_os = "macos"))]
    {
        use std::time::Duration;
        let receiver = GlobalHotKeyEvent::receiver();
        let hotkey_slot_mgr = slot_manager.clone();
        // `persist_tx` (cloned above the watcher move) is used only by the macOS
        // event loop; reuse it here so the non-macOS loop can persist slot copies.
        let transform_config = load_transform_config();
        let paste_transform = load_paste_transform_settings();
        #[cfg(target_os = "windows")]
        let sequence_slot = Arc::new(Mutex::new(1u8));
        #[cfg(not(target_os = "windows"))]
        let mut sequence_slot: u8 = 1;

        // Windows: the grab hook owns ALL multi-tap copy/paste (Ctrl+C ×N,
        // Ctrl+V ×N, letter slots). Tap 1 of Ctrl+V always passes through —
        // normal paste is native and can never break.
        #[cfg(target_os = "windows")]
        spawn_windows_grab(slot_manager.clone(), persist_tx.clone(), suppress.clone());
        loop {
            // Windows: RegisterHotKey delivers WM_HOTKEY to *this thread's*
            // message queue (the manager was created here). global-hotkey only
            // converts them to channel events when messages are dispatched —
            // without this pump, no hotkey ever fires on Windows. Short recv
            // timeout keeps pump latency low.
            #[cfg(target_os = "windows")]
            pump_win32_messages();
            #[cfg(target_os = "windows")]
            let recv_wait = Duration::from_millis(25);
            #[cfg(not(target_os = "windows"))]
            let recv_wait = Duration::from_millis(200);

            // Blocking recv — no CPU polling. Wake periodically to check stop.
            let event = match receiver.recv_timeout(recv_wait) {
                Ok(e) => e,
                Err(_) => {
                    // Timeout — check stop and loop
                    if stop_hotkey.load(Ordering::Relaxed) {
                        break;
                    }
                    continue;
                }
            };
            if stop_hotkey.load(Ordering::Relaxed) {
                break;
            }
            if let Some(action) = registered_hotkeys
                .iter()
                .find(|(hk, _)| hk.id() == event.id)
                .map(|(_, action)| *action)
            {
                #[cfg(target_os = "windows")]
                {
                    match (event.state, action) {
                        (global_hotkey::HotKeyState::Pressed, FinalAction::OpenTui) => {
                            open_tui_search()
                        }
                        (global_hotkey::HotKeyState::Pressed, FinalAction::OpenGui) => open_gui(),
                        (global_hotkey::HotKeyState::Pressed, FinalAction::OpenPicker) => {
                            spawn_picker()
                        }
                        // Windows hotkeys fire while their modifiers are still down. If we paste
                        // immediately, the target app can receive the original hotkey modifiers
                        // plus Ctrl+V instead of a clean paste. Run copy/paste only after release,
                        // then add a small delay so the user can release the modifiers too.
                        (
                            global_hotkey::HotKeyState::Released,
                            FinalAction::CopyToSlot(_)
                            | FinalAction::PasteFromSlot(_)
                            | FinalAction::SmartPaste
                            | FinalAction::SequencePaste,
                        ) => execute_windows_deferred_action(
                            action,
                            hotkey_slot_mgr.clone(),
                            persist_tx.clone(),
                            suppress.clone(),
                            transform_config.clone(),
                            paste_transform.clone(),
                            sequence_slot.clone(),
                        ),
                        _ => {}
                    }
                }

                #[cfg(not(target_os = "windows"))]
                {
                    if event.state == global_hotkey::HotKeyState::Pressed {
                        match action {
                            FinalAction::CopyToSlot(s) => {
                                execute_copy(s, &hotkey_slot_mgr, &persist_tx)
                            }
                            FinalAction::PasteFromSlot(s) => execute_direct_paste(
                                s,
                                &hotkey_slot_mgr,
                                &suppress,
                                &transform_config,
                                &paste_transform,
                            ),
                            FinalAction::OpenTui => open_tui_search(),
                            FinalAction::OpenGui => open_gui(),
                            FinalAction::OpenPicker => {}
                            FinalAction::SmartPaste => {
                                execute_smart_paste(&suppress, &transform_config, &paste_transform)
                            }
                            FinalAction::SequencePaste => {
                                execute_sequence_paste(
                                    &mut sequence_slot,
                                    &hotkey_slot_mgr,
                                    &suppress,
                                    &transform_config,
                                    &paste_transform,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    println!("\n  🛑 Shutting down clipd daemon...");
    stop.store(true, Ordering::Relaxed);
    clear_external_tick_tx();
    watcher_handle.join().ok();
    store_handle.join().ok();
    // Release so an in-process host (clipd-ui) can cleanly restart the daemon.
    release_daemon_lock();
    println!("  ✅ Goodbye!");
    Ok(())
}

// ── Messages from rdev listener → main loop ──

#[derive(Debug, Clone)]
enum HotkeyTick {
    CmdCTap,  // Cmd+C (system also copies)
    CmdVTap,  // Cmd+V (system also pastes)
    CtrlCTap, // Ctrl+C only (no system side-effect on macOS GUI apps)
    CtrlVTap, // Ctrl+V only (no system side-effect on macOS GUI apps)
    UpperCopyTap,
    UpperPasteTap,
    OpenTui,
    OpenGui,
    SequencePaste,           // Cmd+Option+V — paste next item in slot sequence
    SmartPaste,              // Ctrl+Shift+V — transform clipboard content and paste
    SlotPicker,              // Cmd+Shift+V — open slot picker for paste
    CopySlotPicker,          // Ctrl+Shift+Option+C — open slot picker for copy
    CopyClipboardToSlot(u8), // Ctrl+Shift+Option+1..9 — save clipboard to slot
    CopySelectionToSlot(u8), // Ctrl+Shift+Option+A..Z — copy selection to slot 31..56
    PasteFromSlot(u8),       // Ctrl+Option+1..9 — paste slot
    SlotMemoryHud,           // Ctrl+Option+Space — show recent slot memory
    ToggleCollect,           // Ctrl+Option+` — toggle Collect mode on/off
    CommandPalette,          // Ctrl+Space → command palette
    SendToDevice,            // Ctrl+Shift+S — send the clipboard to the other Mac
}

#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, Copy)]
enum FinalAction {
    CopyToSlot(u8),
    PasteFromSlot(u8),
    OpenTui,
    OpenGui,
    OpenPicker,
    SmartPaste,
    SequencePaste,
}

// ── Action executors ──

#[cfg(target_os = "windows")]
fn execute_windows_deferred_action(
    action: FinalAction,
    mgr: SlotManager,
    persist_tx: mpsc::SyncSender<ClipEvent>,
    suppress: Arc<AtomicBool>,
    transform_cfg: TransformConfig,
    paste_settings: PasteTransformSettings,
    sequence_slot: Arc<Mutex<u8>>,
) {
    std::thread::spawn(move || {
        // Let the user release Win/Alt/Ctrl before Clipd injects Ctrl+C/Ctrl+V.
        std::thread::sleep(Duration::from_millis(160));
        log::info!("⌨️  Windows hotkey action after release: {:?}", action);
        match action {
            FinalAction::CopyToSlot(slot) => {
                execute_selection_copy_to_slot(slot, &mgr, &persist_tx);
            }
            FinalAction::PasteFromSlot(slot) => {
                execute_direct_paste(slot, &mgr, &suppress, &transform_cfg, &paste_settings);
            }
            FinalAction::SmartPaste => {
                execute_smart_paste(&suppress, &transform_cfg, &paste_settings);
            }
            FinalAction::SequencePaste => {
                if let Ok(mut slot) = sequence_slot.lock() {
                    execute_sequence_paste(
                        &mut *slot,
                        &mgr,
                        &suppress,
                        &transform_cfg,
                        &paste_settings,
                    );
                }
            }
            FinalAction::OpenTui | FinalAction::OpenGui | FinalAction::OpenPicker => {}
        }
    });
}

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

/// Letter-slot index (0-25) for an rdev letter key, else None.
#[cfg(target_os = "windows")]
fn rkey_letter_index(key: RKey) -> Option<u8> {
    use RKey::*;
    Some(match key {
        KeyA => 0,
        KeyB => 1,
        KeyC => 2,
        KeyD => 3,
        KeyE => 4,
        KeyF => 5,
        KeyG => 6,
        KeyH => 7,
        KeyI => 8,
        KeyJ => 9,
        KeyK => 10,
        KeyL => 11,
        KeyM => 12,
        KeyN => 13,
        KeyO => 14,
        KeyP => 15,
        KeyQ => 16,
        KeyR => 17,
        KeyS => 18,
        KeyT => 19,
        KeyU => 20,
        KeyV => 21,
        KeyW => 22,
        KeyX => 23,
        KeyY => 24,
        KeyZ => 25,
        _ => return None,
    })
}

/// Live modifier state straight from the OS. Event-based tracking desyncs on
/// Windows (LL hooks receive nothing while an elevated window is focused, so
/// a Ctrl release can be missed and the state sticks) — GetAsyncKeyState is
/// always truthful.
#[cfg(target_os = "windows")]
fn win_mods_now() -> (bool, bool, bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12; // Alt
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;
    let down = |vk: i32| unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 };
    (
        down(VK_CONTROL),
        down(VK_MENU),
        down(VK_LWIN) || down(VK_RWIN),
    )
}

/// Windows grab keyboard handler — owns ALL multi-tap copy/paste logic.
///
/// Design principles (learned the hard way):
/// - Plain Ctrl+V is NEVER registered as a global hotkey: RegisterHotKey
///   swallows every paste system-wide and breaks normal pasting whenever the
///   slot machinery hiccups. Tap 1 always passes through natively.
/// - Ctrl+V taps 2+ are suppressed; on commit we undo the single native paste
///   and paste slot N (same design as macOS Cmd+V ×N).
/// - Ctrl+C ×N passes through (the app performs the copy) and slot N is filled
///   from the clipboard afterwards.
/// - A letter within the grace after ×2 targets letter slots A–Z; it cancels
///   the numeric commit via the generation counter, so ×2 never clashes.
#[cfg(target_os = "windows")]
fn spawn_windows_grab(
    slot_mgr: SlotManager,
    persist_tx: mpsc::SyncSender<ClipEvent>,
    suppress: Arc<AtomicBool>,
) {
    // Letter must arrive within the grace; paste grace is < the numeric-paste
    // commit (~430ms) so a letter reliably cancels slot N before it fires.
    const COPY_LETTER_GRACE: Duration = Duration::from_millis(650);
    const PASTE_LETTER_GRACE: Duration = Duration::from_millis(450);
    std::thread::spawn(move || {
        #[derive(Default)]
        struct GrabState {
            c_last_tap: Option<Instant>,
            c_last_press: Option<Instant>,
            c_taps: u8,
            c_gen: u64,
            copy_letter_until: Option<Instant>,
            v_last_tap: Option<Instant>,
            v_last_press: Option<Instant>,
            v_taps: u8,
            v_gen: u64,
            paste_letter_until: Option<Instant>,
        }
        let state = Arc::new(Mutex::new(GrabState::default()));

        let callback = move |event: Event| -> Option<Event> {
            // Let our own synthetic paste keystrokes pass through untouched.
            if suppress.load(Ordering::SeqCst) {
                return Some(event);
            }
            match event.event_type {
                // Ctrl+C: numeric copy multi-tap (passes through); arm letter on x2.
                EventType::KeyPress(RKey::KeyC) => {
                    let (ctrl, alt, meta) = win_mods_now();
                    if !ctrl || alt || meta {
                        return Some(event);
                    }
                    let now = Instant::now();
                    let gen = {
                        let Ok(mut s) = state.lock() else {
                            return Some(event);
                        };
                        if s.c_last_press
                            .map_or(false, |p| now.duration_since(p) < TAP_DEBOUNCE)
                        {
                            return Some(event);
                        }
                        s.c_last_press = Some(now);
                        s.c_taps = match s.c_last_tap {
                            Some(p) if now.duration_since(p) < TAP_WINDOW => {
                                s.c_taps.saturating_add(1)
                            }
                            _ => 1,
                        };
                        s.c_last_tap = Some(now);
                        s.c_gen = s.c_gen.wrapping_add(1);
                        if s.c_taps == 2 {
                            s.copy_letter_until = Some(now + COPY_LETTER_GRACE);
                        }
                        s.c_gen
                    };
                    let st = state.clone();
                    let mgr = slot_mgr.clone();
                    let tx = persist_tx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(COPY_LETTER_GRACE + Duration::from_millis(120));
                        let slot = {
                            let Ok(mut s) = st.lock() else {
                                return;
                            };
                            if s.c_gen != gen {
                                return;
                            }
                            let slot = s.c_taps.clamp(1, 9);
                            s.c_taps = 0;
                            s.c_last_tap = None;
                            s.copy_letter_until = None;
                            slot
                        };
                        // execute_copy notifies on success via
                        // save_text_to_slot — no unconditional toast here
                        // (it used to claim "Saved" even when the clipboard
                        // read failed).
                        execute_copy(slot, &mgr, &tx);
                    });
                    Some(event)
                }
                // Ctrl+V: tap 1 passes straight through — the NATIVE paste
                // happens instantly and normal pasting can never break. Taps
                // 2+ are suppressed; when the window closes we undo the one
                // native paste and paste slot N instead (the same
                // undo-then-paste design as macOS Cmd+V ×N).
                EventType::KeyPress(RKey::KeyV) => {
                    let (ctrl, alt, meta) = win_mods_now();
                    if !ctrl || alt || meta {
                        return Some(event);
                    }
                    let now = Instant::now();
                    let (taps, gen) = {
                        let Ok(mut s) = state.lock() else {
                            return Some(event);
                        };
                        if s.v_last_press
                            .map_or(false, |p| now.duration_since(p) < TAP_DEBOUNCE)
                        {
                            // Key auto-repeat while held: keep normal repeat
                            // pasting on tap 1, swallow repeats mid-multi-tap.
                            return if s.v_taps >= 2 { None } else { Some(event) };
                        }
                        s.v_last_press = Some(now);
                        s.v_taps = match s.v_last_tap {
                            Some(p) if now.duration_since(p) < TAP_WINDOW => {
                                s.v_taps.saturating_add(1)
                            }
                            _ => 1,
                        };
                        s.v_last_tap = Some(now);
                        s.v_gen = s.v_gen.wrapping_add(1);
                        if s.v_taps == 2 {
                            s.paste_letter_until = Some(now + PASTE_LETTER_GRACE);
                        }
                        (s.v_taps, s.v_gen)
                    };
                    if taps == 1 {
                        return Some(event); // native paste — zero latency, zero risk
                    }
                    // taps >= 2: suppress this press and schedule the commit.
                    let st = state.clone();
                    let mgr = slot_mgr.clone();
                    let supp = suppress.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(PASTE_LETTER_GRACE + Duration::from_millis(120));
                        let slot = {
                            let Ok(mut s) = st.lock() else {
                                return;
                            };
                            if s.v_gen != gen {
                                return; // another tap or a letter took over
                            }
                            let slot = s.v_taps.clamp(1, 9);
                            s.v_taps = 0;
                            s.v_last_tap = None;
                            s.paste_letter_until = None;
                            slot
                        };
                        // One native paste (tap 1) already landed — undo it,
                        // then paste the requested slot.
                        if execute_undo_paste(slot, 1, &mgr, &supp) {
                            notify_slot_pasted(slot);
                        }
                    });
                    None
                }
                // A bare letter while armed -> letter copy/paste; SUPPRESS it.
                EventType::KeyPress(key) => {
                    let Some(idx) = rkey_letter_index(key) else {
                        return Some(event);
                    };
                    // Users naturally keep Ctrl held through the whole
                    // gesture (Ctrl+C, C, E) — accept the letter with or
                    // without Ctrl. Alt/Win combos stay untouched.
                    let (_ctrl, alt, meta) = win_mods_now();
                    let action = {
                        let Ok(mut s) = state.lock() else {
                            return Some(event);
                        };
                        if alt || meta {
                            None
                        } else {
                            let now = Instant::now();
                            if s.copy_letter_until.map_or(false, |u| now <= u) {
                                s.copy_letter_until = None;
                                s.c_gen = s.c_gen.wrapping_add(1);
                                s.c_taps = 0;
                                s.c_last_tap = None;
                                Some(true)
                            } else if s.paste_letter_until.map_or(false, |u| now <= u) {
                                s.paste_letter_until = None;
                                s.v_taps = 0;
                                s.v_last_tap = None;
                                // Cancel the pending numeric slot-2 commit —
                                // this double-tap was the letter-paste prefix.
                                s.v_gen = s.v_gen.wrapping_add(1);
                                Some(false)
                            } else {
                                None
                            }
                        }
                    };
                    match action {
                        Some(true) => {
                            let slot = 31 + idx;
                            let mgr = slot_mgr.clone();
                            let tx = persist_tx.clone();
                            std::thread::spawn(move || {
                                execute_copy(slot, &mgr, &tx);
                            });
                            None
                        }
                        Some(false) => {
                            let slot = 31 + idx;
                            let mgr = slot_mgr.clone();
                            let supp = suppress.clone();
                            std::thread::spawn(move || {
                                // Tap 1 of the Ctrl+V ×2 prefix pasted natively
                                // — undo it, then paste the letter slot.
                                if execute_undo_paste(slot, 1, &mgr, &supp) {
                                    notify_slot_pasted(slot);
                                }
                            });
                            None
                        }
                        None => Some(event),
                    }
                }
                _ => Some(event),
            }
        };
        if let Err(e) = grab(callback) {
            log::warn!("Windows grab keyboard handler stopped: {:?}", e);
        }
    });
}

/// Slot-save feedback on Windows: prefer clipd's own styled corner overlay
/// (matches the macOS HUD); fall back to a system toast if the overlay binary
/// isn't alongside the daemon.
#[cfg(target_os = "windows")]
fn notify_slot_saved(slot: u8) {
    notify_slot_action("Saved to", slot);
}

#[cfg(target_os = "windows")]
fn notify_slot_pasted(slot: u8) {
    notify_slot_action("Pasted from", slot);
}

#[cfg(target_os = "windows")]
fn notify_slot_action(action: &str, slot: u8) {
    // Honor the tray "Slot notifications" toggle (same setting as macOS HUD).
    if !load_paste_transform_settings().hud_enabled {
        return;
    }
    let label = if (31..=56).contains(&slot) {
        format!("slot {}", (b'A' + (slot - 31)) as char)
    } else {
        format!("slot {}", slot)
    };
    let msg = format!("{} {}", action, label);
    if !spawn_overlay(&msg) {
        let _ = notify_rust::Notification::new()
            .summary("clipd")
            .body(&format!("📋 {}", msg))
            .timeout(notify_rust::Timeout::Milliseconds(1200))
            .show();
    }
}

/// Spawn the `clipd-overlay` toast window next to the daemon binary. Returns
/// false if it isn't found, so the caller can fall back to an OS toast.
#[cfg(target_os = "windows")]
fn spawn_overlay(msg: &str) -> bool {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["clipd-overlay.exe", "clipd-overlay"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return std::process::Command::new(&candidate)
                        .arg(msg)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .is_ok();
                }
            }
        }
    }
    false
}

fn execute_copy(slot: u8, mgr: &SlotManager, persist_tx: &mpsc::SyncSender<ClipEvent>) {
    // Reads go through the serialized helper: the watcher thread polls the
    // same NSPasteboard continuously, and concurrent access corrupts AppKit's
    // type cache (SIGSEGV inside objc_msgSend). Note the sleep between
    // attempts is deliberately *outside* the lock, so a retry loop here never
    // stalls the watcher for a quarter of a second.
    let mut text: Option<String> = None;
    for attempt in 0..6 {
        match clipd_core::pasteboard::read_text() {
            Some(t) if !t.is_empty() => {
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
    if let Some(ref text) = text {
        save_text_to_slot(slot, text, mgr, persist_tx);
    } else {
        // Loud on purpose: this is the "toast said copied but nothing saved"
        // failure mode.
        log::warn!(
            "📋 Copy to slot {} FAILED — clipboard unreadable or empty after retries",
            slot
        );
    }
}

#[cfg(target_os = "windows")]
fn execute_selection_copy_to_slot(
    slot: u8,
    mgr: &SlotManager,
    persist_tx: &mpsc::SyncSender<ClipEvent>,
) {
    // Direct slot hotkeys have no native copy side effect, so copy the current
    // selection first, then save whatever the foreground app put on the clipboard.
    simulate_copy();
    execute_copy(slot, mgr, persist_tx);
}

fn save_text_to_slot(
    slot: u8,
    text: &str,
    mgr: &SlotManager,
    persist_tx: &mpsc::SyncSender<ClipEvent>,
) {
    if let Err(e) = mgr.copy_to_slot(slot, text) {
        log::warn!("📋 Save to slot {} FAILED: {}", slot, e);
        return;
    }
    log::info!("📋 Saved to slot {}: {}", slot, truncate(text, 40));
    #[cfg(target_os = "macos")]
    show_slot_content_notification("Copied", slot, text);
    #[cfg(target_os = "windows")]
    notify_slot_saved(slot);

    let entry = ClipEntry::new(text.to_string(), None, Some(slot));
    let _ = persist_tx.try_send(ClipEvent::NewClip(entry));
}

/// Cmd+V multi-tap path: the foreground app already received the first one or two
/// native Cmd+V events, so validate the slot, undo those native inserts, then paste
/// the selected slot in one sequential operation.
fn execute_undo_paste(
    slot: u8,
    native_paste_count: u8,
    mgr: &SlotManager,
    suppress: &Arc<AtomicBool>,
) -> bool {
    if let Ok(Some(content)) = mgr.get_slot(slot) {
        let mut cb = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Paste from slot {} failed: {}", slot, e);
                return false;
            }
        };

        let original = cb.get_text().ok();
        suppress.store(true, Ordering::SeqCst);

        for _ in 0..native_paste_count {
            simulate_undo();
            std::thread::sleep(Duration::from_millis(35));
        }

        if let Err(e) = cb.set_text(&content) {
            suppress.store(false, Ordering::SeqCst);
            log::warn!("Paste from slot {} failed: {}", slot, e);
            return false;
        }

        std::thread::sleep(Duration::from_millis(50));
        simulate_paste();
        log::info!(
            "📋 Pasted from slot {} after {} undo(s): {}",
            slot,
            native_paste_count,
            truncate(&content, 40)
        );
        #[cfg(target_os = "macos")]
        show_slot_content_notification("Pasted", slot, &content);

        std::thread::sleep(Duration::from_millis(200));

        // Only restore if the user hasn't done Cmd+C during the paste window.
        let clipboard_unchanged = cb
            .get_text()
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
        true
    } else {
        log::info!("📋 Slot {} is empty", slot);
        false
    }
}

/// Ctrl+V path: nothing was pasted by the system, so just set clipboard and send Cmd+V.
/// Suppresses the watcher and restores the original clipboard afterwards.
/// When Transform on Paste is enabled, applies the user's transform pipeline.
fn execute_direct_paste(
    slot: u8,
    mgr: &SlotManager,
    suppress: &Arc<AtomicBool>,
    // Kept in the signature: this used to run the transform chain, and the
    // next person to reach for it should see why it no longer does.
    _transform_cfg: &TransformConfig,
    _paste_settings: &PasteTransformSettings,
) {
    if let Ok(Some(content)) = mgr.get_slot(slot) {
        // A slot paste is verbatim — the transform chain is deliberately not
        // run here.
        //
        // Transforms belong to the explicit smart-paste gesture, and the
        // settings page says so in as many words: "Applied with ⌘⇧V — normal
        // Cmd+V is unchanged". Running them on a slot paste meant asking for
        // slot R and getting your text lowercased and line-numbered, because
        // `AddLineNumbers` and `Lowercase` happened to be in the active list.
        // Someone reaching for a specific slot wants what they put in it.

        let mut cb = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Paste from slot {} failed: {}", slot, e);
                return;
            }
        };

        let original = cb.get_text().ok();
        suppress.store(true, Ordering::SeqCst);

        if let Err(e) = cb.set_text(&content) {
            suppress.store(false, Ordering::SeqCst);
            log::warn!("Paste from slot {} failed: {}", slot, e);
            return;
        }
        sleep_before_injected_paste();
        simulate_paste();
        log::info!("📋 Pasted from slot {}: {}", slot, truncate(&content, 40));
        #[cfg(target_os = "macos")]
        show_slot_content_notification("Pasted", slot, &content);
        #[cfg(target_os = "windows")]
        notify_slot_pasted(slot);

        std::thread::sleep(Duration::from_millis(200));

        // Only restore if the user hasn't done Cmd+C during the paste window.
        // If the clipboard changed, someone else (the user) wrote new content
        // and we must not overwrite it.
        let clipboard_unchanged = cb
            .get_text()
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

#[cfg(not(target_os = "macos"))]
fn execute_sequence_paste(
    sequence_slot: &mut u8,
    mgr: &SlotManager,
    suppress: &Arc<AtomicBool>,
    transform_cfg: &TransformConfig,
    paste_settings: &PasteTransformSettings,
) {
    if mgr.has_content(*sequence_slot) {
        execute_direct_paste(*sequence_slot, mgr, suppress, transform_cfg, paste_settings);
        log::info!(
            "📋 Sequence paste: slot {} (next: {})",
            *sequence_slot,
            *sequence_slot + 1
        );
        *sequence_slot += 1;
        return;
    }

    log::info!("📋 Sequence complete — resetting to slot 1");
    *sequence_slot = 1;
    if mgr.has_content(1) {
        execute_direct_paste(1, mgr, suppress, transform_cfg, paste_settings);
        *sequence_slot = 2;
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
        let suggestions = suggest_smart_transform(&content, &ct, dest_app.as_deref());
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
        let kind = TransformKind::CustomPrompt(fresh_settings.default_ai_prompt.clone());
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
    #[cfg(target_os = "macos")]
    if transformed_any && smart_paste_text_visibly_changed(&original, &content) {
        show_hud("✨ Smart Paste");
    }

    suppress.store(true, Ordering::SeqCst);

    if let Err(e) = cb.set_text(&content) {
        suppress.store(false, Ordering::SeqCst);
        log::warn!("Smart paste: failed to set clipboard: {}", e);
        return;
    }

    sleep_before_injected_paste();

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
                        log::info!("🔄 Auto-transform for {}: {}", app, rule.description);
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
                let suggestions = suggest_smart_transform(&content, &ct, dest_app.as_deref());
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
                let kind = TransformKind::CustomPrompt(paste_settings.default_ai_prompt.clone());
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
fn simulate_undo() {
    let script = r#"tell application "System Events"
  keystroke "z" using command down
end tell"#;
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output();
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

#[cfg(target_os = "macos")]
fn simulate_copy() {
    let script = r#"tell application "System Events"
  keystroke "c" using command down
end tell"#;
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output();
    std::thread::sleep(Duration::from_millis(80));
}

#[cfg(target_os = "macos")]
fn sleep_before_injected_paste() {
    std::thread::sleep(Duration::from_millis(50));
}

#[cfg(not(target_os = "macos"))]
fn sleep_before_injected_paste() {
    #[cfg(target_os = "windows")]
    std::thread::sleep(Duration::from_millis(120));
    #[cfg(not(target_os = "windows"))]
    std::thread::sleep(Duration::from_millis(50));
}

#[cfg(target_os = "windows")]
fn release_windows_shortcut_modifiers(enigo: &mut Enigo) {
    // Global hotkey callbacks can run while Win/Alt/Ctrl/Shift are still logically
    // down. Release them before injecting Ctrl+C/Ctrl+V so target apps receive
    // the plain shortcut, not the original hotkey modifiers plus Ctrl+V.
    for key in [
        Key::LWin,
        Key::RWin,
        Key::Alt,
        Key::Control,
        Key::LControl,
        Key::RControl,
        Key::Shift,
        Key::LShift,
        Key::RShift,
    ] {
        let _ = enigo.key(key, Direction::Release);
    }
    std::thread::sleep(Duration::from_millis(20));
}

#[cfg(not(target_os = "macos"))]
fn simulate_undo() {
    if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
        #[cfg(target_os = "windows")]
        release_windows_shortcut_modifiers(&mut enigo);
        #[cfg(target_os = "windows")]
        let z_key = Key::Z;
        #[cfg(not(target_os = "windows"))]
        let z_key = Key::Unicode('z');
        let _ = enigo.key(Key::Control, Direction::Press);
        let _ = enigo.key(z_key, Direction::Click);
        let _ = enigo.key(Key::Control, Direction::Release);
    }
}

#[cfg(not(target_os = "macos"))]
fn simulate_copy() {
    if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
        #[cfg(target_os = "windows")]
        release_windows_shortcut_modifiers(&mut enigo);
        #[cfg(target_os = "windows")]
        let c_key = Key::C;
        #[cfg(not(target_os = "windows"))]
        let c_key = Key::Unicode('c');
        let _ = enigo.key(Key::Control, Direction::Press);
        let _ = enigo.key(c_key, Direction::Click);
        let _ = enigo.key(Key::Control, Direction::Release);
    }
    std::thread::sleep(Duration::from_millis(80));
}

#[cfg(not(target_os = "macos"))]
fn simulate_paste() {
    if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
        #[cfg(target_os = "windows")]
        {
            release_windows_shortcut_modifiers(&mut enigo);
            // Safe to inject plain Ctrl+V: it is deliberately NOT registered
            // as a global hotkey (tap counting lives in the grab hook, which
            // passes our synthetic events through via the suppress flag), and
            // Ctrl+V has the broadest app support of any paste gesture.
            let _ = enigo.key(Key::Control, Direction::Press);
            let _ = enigo.key(Key::V, Direction::Click);
            let _ = enigo.key(Key::Control, Direction::Release);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let v_key = Key::Unicode('v');
            let _ = enigo.key(Key::Control, Direction::Press);
            let _ = enigo.key(v_key, Direction::Click);
            let _ = enigo.key(Key::Control, Direction::Release);
        }
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
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
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
#[cfg(target_os = "macos")]
fn show_slot_notification(action: &str, slot: u8) {
    show_hud(&slot_hud_payload(action, slot, None).join("\n"));
}

/// The strip of slots to draw around the one that just fired.
///
/// A single number answers "where did that go" but not "how far along am I",
/// which is the question during a multi-tap — so the toast shows the run. Five
/// chips, centred on the live slot and clamped to the 1..9 bank so the window
/// never runs off either end.
#[cfg(target_os = "macos")]
fn slot_strip(slot: u8) -> Option<(u8, u8)> {
    if !(1..=9).contains(&slot) {
        return None;
    }
    let first = slot.saturating_sub(2).max(1).min(5);
    Some((first, first + 4))
}

/// What to press to get this slot back, and how long the count stays open.
///
/// The window is read from TAP_WINDOW rather than written out, so the toast
/// cannot end up quoting a timing the daemon no longer uses.
#[cfg(target_os = "macos")]
fn slot_foot(action: &str, slot: u8) -> String {
    let chord = match action {
        "Pasted" | "Paste" => retrieve_hint(slot),
        _ => match slot {
            1..=9 => format!("⌘C ×{slot}"),
            11..=30 => format!("⌥C ×{}", slot - 10),
            31..=56 => format!("⌃⌥C {}", (b'A' + (slot - 31)) as char),
            _ => "⌘C".to_string(),
        },
    };
    format!(
        "{chord} · fires {:.2}s after the last tap",
        TAP_WINDOW.as_secs_f32()
    )
}

/// The slot toast: pill, slot strip, chord. Falls back to the plain toast for
/// the letter and extended banks, where a five-chip strip would be a window
/// onto twenty-six slots and say nothing.
#[cfg(target_os = "macos")]
fn slot_hud_payload(action: &str, slot: u8, content: Option<&str>) -> Vec<String> {
    let Some((first, last)) = slot_strip(slot) else {
        let mut lines = vec![
            "STYLE\ttoast".to_string(),
            format!("BADGE\t{}", slot_badge(slot)),
            format!("TITLE\t{}", toast_title(action, slot)),
            format!("HINT\t{}", retrieve_hint(slot)),
        ];
        if let Some(text) = content {
            lines.insert(
                3,
                format!("PREVIEW\t{}\t{}", content_icon(text), truncate(text, 52)),
            );
        }
        return lines;
    };

    let mut lines = vec![
        "STYLE\tslot".to_string(),
        format!("BADGE\t{}", slot_badge(slot)),
        format!("TITLE\t{}", toast_title(action, slot)),
        format!("SLOTS\t{slot}\t{first}\t{last}"),
        format!("FOOT\t{}", slot_foot(action, slot)),
    ];
    // The avatar shows where the clip came from; the title already carries the
    // slot, so spending the circle on the number again says nothing new.
    if let Some(app) = get_frontmost_app_name() {
        if let Some(initial) = app.chars().find(|c| c.is_alphanumeric()) {
            lines.push(format!("SOURCE\t{}", initial.to_lowercase()));
        }
    }
    if let Some(text) = content {
        lines.push(format!(
            "PREVIEW\t{}\t{}",
            content_icon(text),
            truncate(text, 52)
        ));
    }
    lines
}

#[cfg(target_os = "macos")]
fn show_slot_content_notification(action: &str, slot: u8, content: &str) {
    remember_slot_for_hud(slot, content);
    show_hud(&slot_hud_payload(action, slot, Some(content)).join("\n"));
}

/// Toast's first line: status + the slot it acted on, e.g. "Copied to Slot 3"
/// or "Copied to Slot 31 (A)". Letter slots especially need the slot spelled
/// out — a lone letter badge is easy to miss.
#[cfg(target_os = "macos")]
fn toast_title(action: &str, slot: u8) -> String {
    match action {
        "Copied" | "Copy" => format!("Copied to {}", slot_label(slot)),
        "Pasted" | "Paste" => format!("Pasted from {}", slot_label(slot)),
        other => format!("{} {}", other, slot_label(slot)),
    }
}

/// The keystroke to paste a slot back, e.g. "⌘V ×3", "⌥V ×2", "⌃⌥V A".
/// Shown muted on the toast so the next action is always obvious.
#[cfg(target_os = "macos")]
fn retrieve_hint(slot: u8) -> String {
    match slot {
        1..=9 => format!("⌘V ×{}", slot),
        11..=30 => format!("⌥V ×{}", slot - 10),
        31..=56 => format!("⌃⌥V {}", (b'A' + (slot - 31)) as char),
        _ => "⌘V".to_string(),
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct HudSlotMemory {
    slot: u8,
    preview: String,
}

#[cfg(target_os = "macos")]
fn remember_slot_for_hud(slot: u8, content: &str) {
    if let Ok(mut items) = hud_slot_memory().lock() {
        items.retain(|item| item.slot != slot);
        items.insert(
            0,
            HudSlotMemory {
                slot,
                preview: truncate(content, 40),
            },
        );
        items.truncate(8);
    }
}

#[cfg(target_os = "macos")]
fn hud_slot_memory() -> &'static std::sync::Mutex<Vec<HudSlotMemory>> {
    use std::sync::{Mutex, OnceLock};

    static RECENT: OnceLock<Mutex<Vec<HudSlotMemory>>> = OnceLock::new();
    RECENT.get_or_init(|| Mutex::new(Vec::new()))
}

/// Build the tagged `ROW` lines for the HUD's "recent stack" — the vertical
/// list of slots the user has touched this session, newest first. The slot
/// matching `active` (if any) is flagged so the HUD highlights it.
#[cfg(target_os = "macos")]
fn slot_memory_rows(active: Option<u8>) -> Vec<String> {
    hud_slot_memory()
        .lock()
        .map(|items| {
            items
                .iter()
                .take(6)
                .map(|item| {
                    let is_active = if Some(item.slot) == active { "1" } else { "0" };
                    format!(
                        "ROW\t{}\t{}\t{}\t{}\t{}",
                        item.slot,
                        slot_badge(item.slot),
                        content_icon(&item.preview),
                        item.preview,
                        is_active,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Short slot badge shown in the HUD's row list: letters for 31..=56, the
/// number otherwise (e.g. `3`, `A`).
#[cfg(target_os = "macos")]
fn slot_badge(slot: u8) -> String {
    if (31..=56).contains(&slot) {
        ((b'A' + (slot - 31)) as char).to_string()
    } else {
        slot.to_string()
    }
}

/// Classify a preview into an icon *kind* (mapped to an SF Symbol by the HUD)
/// so the user recognizes *what* a slot holds at a glance — a link, an email,
/// code… — rather than recalling where they put it.
#[cfg(target_os = "macos")]
fn content_icon(content: &str) -> &'static str {
    let t = content.trim();
    let lower = t.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
        "link"
    } else if t.contains('@') && t.contains('.') && !t.contains(' ') {
        "mail"
    } else if t.contains("=>")
        || t.contains("();")
        || t.contains("){")
        || t.contains(") {")
        || lower.starts_with("fn ")
        || lower.starts_with("def ")
        || lower.starts_with("function ")
        || lower.starts_with("const ")
        || lower.starts_with("import ")
        || lower.starts_with("$ ")
    {
        "code"
    } else if !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_digit() || "+-() .".contains(c))
    {
        "number"
    } else {
        "text"
    }
}

#[cfg(target_os = "macos")]
fn show_slot_memory_hud(mgr: &SlotManager) {
    let mut rows = slot_memory_rows(None);
    if rows.is_empty() {
        // Nothing touched this session yet — fall back to whatever the
        // SlotManager persisted, newest slot first.
        if let Ok(slots) = mgr.list_slots() {
            rows = slots
                .into_iter()
                .filter(|(slot, content)| *slot > 0 && !content.trim().is_empty())
                .rev()
                .take(6)
                .map(|(slot, content)| {
                    let preview = truncate(&content, 40);
                    format!(
                        "ROW\t{}\t{}\t{}\t{}\t0",
                        slot,
                        slot_badge(slot),
                        content_icon(&preview),
                        preview,
                    )
                })
                .collect();
        }
    }

    let mut lines = vec![
        "STYLE\tlist".to_string(),
        "TITLE\tRecent slots".to_string(),
        "HINT\t⌃⌥Space".to_string(),
    ];
    if rows.is_empty() {
        lines.push("PREVIEW\tempty\tNo slots saved yet — copy something to begin".to_string());
    } else {
        lines.extend(rows);
    }
    lines.push("FOOT\tCmd 1-9 · ⌥ 11-30 · Letters A-Z".to_string());
    show_hud(&lines.join("\n"));
}

/// The Collect-mode panel: the growing stack of slots 1..=9 captured this
/// batch, newest highlighted. Replaces the per-copy toast while collecting so
/// the user sees the whole batch, not just the last item.
#[cfg(target_os = "macos")]
fn show_collect_hud(mgr: &SlotManager, active_slot: u8, just_started: bool) {
    let mut rows = Vec::new();
    let mut count = 0;
    if let Ok(slots) = mgr.list_slots() {
        for (slot, content) in slots.into_iter() {
            if (1..=9).contains(&slot) && !content.trim().is_empty() {
                count += 1;
                let preview = truncate(&content, 40);
                let active = if slot == active_slot { "1" } else { "0" };
                rows.push(format!(
                    "ROW\t{}\t{}\t{}\t{}\t{}",
                    slot,
                    slot_badge(slot),
                    content_icon(&preview),
                    preview,
                    active,
                ));
            }
        }
    }

    let title = if just_started || count == 0 {
        "Collecting — just press ⌘C".to_string()
    } else {
        format!(
            "Collecting · {} item{}",
            count,
            if count == 1 { "" } else { "s" }
        )
    };

    let mut lines = vec![
        "STYLE\tlist".to_string(),
        format!("TITLE\t{}", title),
        "HINT\t⌃⌥` done".to_string(),
    ];
    if rows.is_empty() {
        lines.push("PREVIEW\tempty\tCopy anything — it stacks here automatically".to_string());
    } else {
        lines.extend(rows);
    }
    lines.push("FOOT\t⌘⇧V to pick one when pasting".to_string());
    show_hud(&lines.join("\n"));
}

/// Brief confirmation toast when Collect mode is turned off.
#[cfg(target_os = "macos")]
fn show_collect_done_hud() {
    show_hud("STYLE\ttoast\nBADGE\t✓\nTITLE\tCollect mode off\nHINT\t⌘⇧V to pick");
}

/// Show the command palette — a prompt HUD listing all enabled custom actions,
/// then run the chosen one on the current clipboard.
#[cfg(target_os = "macos")]
fn show_command_palette_hud() {
    let cfg = load_actions();
    let actions: Vec<&CustomAction> = cfg.actions.iter().filter(|a| a.enabled).collect();

    if actions.is_empty() {
        show_hud(
            "STYLE\ttoast\nBADGE\t⚠️\nTITLE\tNo actions configured\nHINT\tAdd custom actions in clipd settings",
        );
        return;
    }

    // Build button lines: BTN\tid\tlabel\tprimary(0/1)
    let mut btn_lines = String::new();
    for (i, action) in actions.iter().enumerate() {
        let primary = if i == 0 { "1" } else { "0" };
        btn_lines.push_str(&format!("BTN\t{}\t{}\t{}\n", i, action.name, primary));
    }

    let payload = format!(
        "STYLE\tprompt\nBADGE\t⚡\nTITLE\tCommand Palette\n\
         HINT\tRun a custom action on the clipboard\n\
         {btn_lines}\
         TIMEOUT\t30"
    );

    let choice = run_hud_prompt(&payload);

    let Some(choice) = choice else {
        return;
    };

    let idx = match choice.parse::<usize>() {
        Ok(i) if i < actions.len() => i,
        _ => return,
    };
    let action = actions[idx];

    // Read current clipboard
    let content = match Clipboard::new() {
        Ok(mut cb) => cb.get_text().unwrap_or_default(),
        Err(e) => {
            log::warn!("Command palette: clipboard access failed: {}", e);
            return;
        }
    };

    if content.is_empty() {
        show_hud("STYLE\ttoast\nBADGE\t⚠️\nTITLE\tClipboard is empty\nHINT\tCopy something first");
        return;
    }

    log::info!(
        "⚙️  Command palette: running '{}' on clipboard",
        action.name
    );
    match run_action(&action.command, &content, Duration::from_secs(30)) {
        Ok(out) => {
            if action.output == ActionOutput::Clipboard && !out.trim().is_empty() {
                if let Ok(mut cb) = Clipboard::new() {
                    let _ = cb.set_text(&out);
                }
                show_hud(&format!(
                    "STYLE\ttoast\nBADGE\t✓\nTITLE\t{}\nHINT\tResult on clipboard",
                    action.name
                ));
            } else if action.output == ActionOutput::NewClip && !out.trim().is_empty() {
                show_hud(&format!(
                    "STYLE\ttoast\nBADGE\t✓\nTITLE\t{}\nHINT\tResult saved to history",
                    action.name
                ));
            } else {
                show_hud(&format!(
                    "STYLE\ttoast\nBADGE\t✓\nTITLE\t{}\nHINT\tDone",
                    action.name
                ));
            }
        }
        Err(e) => {
            log::warn!("Command palette: action '{}' failed: {}", action.name, e);
            show_hud(&format!(
                "STYLE\ttoast\nBADGE\t✗\nTITLE\t{}\nHINT\t{}",
                action.name,
                e.chars().take(60).collect::<String>()
            ));
        }
    }
}

#[cfg(target_os = "macos")]
fn slot_label(slot: u8) -> String {
    if (31..=56).contains(&slot) {
        let letter = (b'A' + (slot - 31)) as char;
        format!("Slot {} ({})", slot, letter)
    } else {
        format!("Slot {}", slot)
    }
}

/// How much of the top of the screen the island can occupy when open.
///
/// Shared with the GUI, which uses the same band to keep a palette opened at
/// the cursor from landing under the island. Not macOS-only: the clearance
/// arithmetic below is platform-independent, and gating it here left it
/// undefined for the code that uses it on Windows and Linux.
const ISLAND_RESERVED_TOP: u32 = clipd_core::ISLAND_RESERVED_TOP as u32;

/// The payload, plus the room the island needs if it is the active layout.
///
/// Both surfaces used to open at the top centre and land on top of each
/// other. The island is the one that belongs there — it *is* the top of the
/// screen — so the HUD is the one that moves.
fn hud_payload_with_clearance(text: &str) -> String {
    add_hud_clearance(text, clipd_core::island_layout_active())
}

fn add_hud_clearance(text: &str, island_active: bool) -> String {
    if !island_active {
        return text.to_string();
    }
    format!("{text}\nAVOIDTOP\t{ISLAND_RESERVED_TOP}")
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
    let text = &hud_payload_with_clearance(text);

    use std::sync::Mutex;
    use std::sync::OnceLock;

    static LAST_PID: OnceLock<Mutex<Option<u32>>> = OnceLock::new();
    let pid_lock = LAST_PID.get_or_init(|| Mutex::new(None));

    if let Ok(mut prev) = pid_lock.lock() {
        if let Some(pid) = prev.take() {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .output();
        }
    }

    let hud_bin = find_hud_binary();
    log::info!("HUD: launching {} with text {:?}", hud_bin.display(), text);

    match std::process::Command::new(&hud_bin).arg(text).spawn() {
        Ok(child) => {
            log::info!("HUD: spawned pid {}", child.id());
            if let Ok(mut prev) = pid_lock.lock() {
                *prev = Some(child.id());
            }
        }
        Err(e) => log::warn!(
            "HUD overlay failed: {} (looked for {})",
            e,
            hud_bin.display()
        ),
    }
}

/// CopyQ-style automation: any enabled Custom Action with a non-empty
/// `auto_pattern` runs automatically when a newly captured clip contains that
/// text (case-insensitive). Loop-proofing: clips produced by actions are
/// tagged source "action:<name>" and never re-trigger, and Clipboard-output
/// actions are skipped (writing the clipboard from a clipboard event would
/// self-trigger).
fn run_auto_actions(entry: &clipd_core::ClipEntry) {
    if entry.content.is_empty() {
        return;
    }
    if entry
        .source_app
        .as_deref()
        .map_or(false, |sa| sa.starts_with("action:"))
    {
        return;
    }
    let cfg = clipd_core::load_actions();
    let content_lower = entry.content.to_lowercase();
    for action in cfg.actions {
        if !action.enabled || action.auto_pattern.trim().is_empty() {
            continue;
        }
        if !content_lower.contains(&action.auto_pattern.trim().to_lowercase()) {
            continue;
        }
        if action.output == clipd_core::ActionOutput::Clipboard {
            log::warn!(
                "Auto-action '{}' skipped: clipboard-output actions cannot auto-run",
                action.name
            );
            continue;
        }
        let content = entry.content.clone();
        std::thread::spawn(move || {
            log::info!("⚙️  Auto-action '{}' triggered", action.name);
            match clipd_core::run_action(&action.command, &content, Duration::from_secs(15)) {
                Ok(out) => {
                    if action.output == clipd_core::ActionOutput::NewClip && !out.trim().is_empty()
                    {
                        if let Ok(store) = ClipStore::new(&ClipStore::default_path()) {
                            let result = ClipEntry::new(
                                out.trim_end_matches('\n').to_string(),
                                Some(format!("action:{}", action.name)),
                                None,
                            );
                            let _ = store.insert(&result);
                        }
                    }
                }
                Err(e) => log::warn!("Auto-action '{}' failed: {}", action.name, e),
            }
        });
    }
}

/// Dispatch pending Win32 messages on the current thread. Required for
/// `global-hotkey`: its hidden window lives on the thread that created the
/// manager, and WM_HOTKEY only reaches it through message dispatch.
#[cfg(target_os = "windows")]
fn pump_win32_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    // SAFETY: standard Win32 message pump; MSG is plain data written by
    // PeekMessageW, and null HWND means "all windows on this thread".
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Find the `clipd-hud` binary next to the current executable or the `clipd`
/// daemon binary (which may differ from current_exe when spawned by clipd-gui).
#[cfg(target_os = "macos")]
fn find_hud_binary() -> std::path::PathBuf {
    if let Ok(from_env) = std::env::var("CLIPD_HUD_BIN") {
        let p = std::path::PathBuf::from(from_env);
        if p.is_file() {
            return p;
        }
    }

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();

    // Look next to current_exe (the clipd daemon binary itself).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("clipd-hud"));
        }
    }

    // Also look next to a `clipd` binary in the same directory as this exe
    // (robust when spawned by clipd-gui: current_exe may resolve to the GUI's
    // bundled clipd rather than the CLI wrapper).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let clipd_exe = dir.join("clipd");
            if clipd_exe.is_file() {
                candidates.push(dir.join("clipd-hud"));
            }
        }
    }

    for c in candidates {
        if c.is_file() {
            return c;
        }
    }

    // Fallback: search PATH
    std::path::PathBuf::from("clipd-hud")
}

/// Locate the `clipd-ocr` helper (Apple Vision OCR), next to the current exe.
/// Mirrors `find_hud_binary`. Honors the `CLIPD_OCR_BIN` override.
#[cfg(target_os = "macos")]
fn find_ocr_binary() -> std::path::PathBuf {
    if let Ok(from_env) = std::env::var("CLIPD_OCR_BIN") {
        let p = std::path::PathBuf::from(from_env);
        if p.is_file() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("clipd-ocr");
            if cand.is_file() {
                return cand;
            }
        }
    }
    std::path::PathBuf::from("clipd-ocr")
}

/// Run on-device OCR on an image file, returning recognized text if any.
/// macOS invokes the bundled `clipd-ocr` (Apple Vision, on-device, no network).
#[cfg(target_os = "macos")]
fn run_ocr(image_path: &std::path::Path) -> Option<String> {
    let bin = find_ocr_binary();
    let output = std::process::Command::new(&bin)
        .arg(image_path)
        .output()
        .ok()?;
    if !output.status.success() {
        log::debug!("clipd-ocr exited non-zero for {:?}", image_path);
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// OCR is macOS-only for now (Apple Vision). Other platforms store the image
/// without recognized text.
#[cfg(not(target_os = "macos"))]
fn run_ocr(_image_path: &std::path::Path) -> Option<String> {
    None
}

fn open_tui_search() {
    log::info!("🔍 Opening TUI search...");
    let exe = resolve_clipd_cli_exe();

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
        // DETACHED_PROCESS hides the console window completely. CREATE_NEW_CONSOLE (the old
        // value 0x10) causes a flash of a new terminal — the very thing we want to avoid.
        const DETACHED: u32 = 0x0000_0008;
        let _ = std::process::Command::new(&exe)
            .arg("search")
            .creation_flags(DETACHED)
            .spawn();
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = std::process::Command::new(&exe).arg("search").spawn();
    }
}

/// Resolve the user-facing `clipd` CLI binary, not necessarily `current_exe`.
///
/// On macOS the daemon is commonly hosted in-process by `clipd-ui` so the
/// keyboard tap inherits the app's Input Monitoring permission. In that case
/// `current_exe()` is `clipd-ui`, but Ctrl+R must run `clipd search` in a
/// terminal.
fn resolve_clipd_cli_exe() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    let names = ["clipd.exe", "clipd"];
    #[cfg(not(target_os = "windows"))]
    let names = ["clipd"];

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }

    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let suffix = if cfg!(target_os = "windows") {
        "clipd.exe"
    } else {
        "clipd"
    };
    for candidate in [
        workspace_root.join("target/debug").join(suffix),
        workspace_root.join("target/release").join(suffix),
    ] {
        if candidate.is_file() {
            return candidate;
        }
    }

    for path in [
        std::path::PathBuf::from("/usr/local/bin").join(suffix),
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
            .join(".local/bin")
            .join(suffix),
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
            .join(".cargo/bin")
            .join(suffix),
    ] {
        if path.is_file() {
            return path;
        }
    }

    std::path::PathBuf::from(suffix)
}

/// A password was just copied. Offer to file it into a vault.
///
/// The secret arrives with the event rather than being re-read from the
/// clipboard, so what gets saved is exactly what the prompt is about even if
/// the user copies something else while deciding.
#[cfg(target_os = "macos")]
fn offer_vault_save(kinds: &str, secret: String, stored: bool) {
    let config = load_privacy_config();
    if !config.offer_vault_on_secret || secret.trim().is_empty() {
        return;
    }
    let targets = available_targets();
    if targets.is_empty() {
        log::info!(
            "🔐 Password detected but no vault backend available (install `op`/`bw`; Keychain is built in on macOS)"
        );
        return;
    }
    // Prefer the native system store (Keychain on macOS) — instant, no prompt,
    // no terminal.
    let target = targets
        .iter()
        .copied()
        .find(|t| t.id() == "keychain")
        .unwrap_or(targets[0]);

    let kinds = kinds.to_string();
    let auto = config.vault_auto_save;
    // Off the persist thread: the prompt blocks for as long as the user takes,
    // and clip persistence must not wait on it.
    std::thread::spawn(move || {
        if !auto && !confirm_vault_save(&secret, &kinds, target, stored) {
            log::info!("🔐 User declined to vault the detected password");
            return;
        }

        let entry = SecretEntry {
            title: vault_entry_title(&kinds),
            username: String::new(),
            password: secret,
            url: String::new(),
            notes: format!("Captured by clipd ({kinds})."),
        };
        match save_secret(target, &entry) {
            Ok(_) => {
                log::info!("🔐 Saved detected password to {}", target.label());
                // clipd's own corner HUD — reliable from the daemon, unlike
                // osascript notifications.
                show_hud(&format!(
                    "STYLE\ttoast\nBADGE\t🔐\nTITLE\tPassword saved\nHINT\t{}",
                    target.label()
                ));
                // The banner is passive by necessity: an `osascript`
                // notification belongs to a process that has already exited, so
                // macOS has nothing to deliver a click to. Say where the
                // password went instead of implying the banner is actionable.
                notify(
                    "clipd",
                    &format!(
                        "🔐 Saved to {} — find it in clipd ▸ Saved passwords",
                        target.label()
                    ),
                );
            }
            Err(e) => {
                log::warn!("🔐 Vault save failed: {}", e);
                show_hud(
                    "STYLE\ttoast\nBADGE\t⚠️\nTITLE\tCouldn't save password\nHINT\tsee clipd logs",
                );
                notify("clipd — couldn't save password", &e);
            }
        }
    });
}

/// Name a new vault entry after where it came from, so it is recognisable in
/// clipd's vault list and in Keychain Access months later. Older builds used a
/// bare timestamp, which made every entry look identical.
#[cfg(target_os = "macos")]
fn vault_entry_title(kinds: &str) -> String {
    let stamp = chrono::Local::now().format("%b %-d, %-I:%M %p");
    match frontmost_app_name() {
        Some(app) => format!("{app} — {kinds} ({stamp})"),
        None => format!("clipd — {kinds} ({stamp})"),
    }
}

/// Ask before writing a password to permanent storage. Returns true to save.
///
/// Blocks on the HUD subprocess, which prints the chosen button id. Anything
/// other than an explicit "save" — timeout, dismissal, or a HUD that failed to
/// launch — means don't save.
#[cfg(target_os = "macos")]
fn confirm_vault_save(secret: &str, kinds: &str, target: VaultTarget, stored: bool) -> bool {
    let source = frontmost_app_name();
    let hint = match (&source, stored) {
        (Some(app), true) => format!("Copied from {app} · kept in history"),
        (Some(app), false) => format!("Copied from {app} · not saved to history"),
        (None, true) => "Kept in history".to_string(),
        (None, false) => "Not saved to history".to_string(),
    };
    // Never render the secret itself — the prompt may sit on screen in view of
    // whoever is around. Length alone is enough to identify it.
    let preview = format!("{} · {} characters", kinds, secret.chars().count());

    let payload = format!(
        "STYLE\tprompt\nBADGE\t🔐\nTITLE\tSave this password?\nHINT\t{hint}\n\
         PREVIEW\ttext\t{preview}\nBTN\tskip\tNot now\t0\nBTN\tsave\tSave to {}\t1\nTIMEOUT\t15",
        target.label()
    );

    match run_hud_prompt(&payload) {
        Some(choice) => choice == "save",
        None => {
            // Without a working prompt we cannot get consent, and silently
            // saving is exactly the behaviour this replaced.
            log::warn!("🔐 Couldn't show the vault prompt — not saving the password");
            false
        }
    }
}

/// Run the HUD in interactive mode and wait for the user's choice.
///
/// Deliberately does not go through [`show_hud`]: that kills any previous HUD
/// and returns immediately, whereas a prompt has to outlive passing toasts and
/// be waited on.
#[cfg(target_os = "macos")]
fn run_hud_prompt(payload: &str) -> Option<String> {
    let payload = &hud_payload_with_clearance(payload);
    let hud_bin = find_hud_binary();
    let out = std::process::Command::new(&hud_bin)
        .arg(payload)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| log::warn!("HUD prompt failed to launch ({}): {e}", hud_bin.display()))
        .ok()?;
    let choice = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if choice.is_empty() {
        None
    } else {
        Some(choice)
    }
}

/// Name of the frontmost app, for labelling where a password came from.
#[cfg(target_os = "macos")]
fn frontmost_app_name() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to get name of first process whose frontmost is true")
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() || name.to_lowercase().starts_with("clipd") {
        None
    } else {
        Some(name)
    }
}

/// Non-modal macOS notification banner.
///
/// These are informational only. `osascript` exits as soon as the banner is
/// posted, so macOS has no live process to route a click to — anything the user
/// should be able to act on has to be a HUD prompt, the menu bar, or the vault
/// window instead.
#[cfg(target_os = "macos")]
fn notify(title: &str, body: &str) {
    let body = body.replace('"', "'");
    let title = title.replace('"', "'");
    let script = format!(r#"display notification "{}" with title "{}""#, body, title);
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output();
}

/// Send whatever is on the clipboard right now to the other Mac.
///
/// Deliberately has no picker and no confirmation. With one other Mac there is
/// nothing to choose, and a dialog on every send would cost more than the
/// occasional mistake — feedback is a notification after the fact, not a
/// question before it. Failures are notifications too: this runs from a hotkey
/// with no window in front, so a silent failure would be indistinguishable
/// from the feature not existing.
fn send_clipboard_to_other_mac() {
    let clip = match clipboard_as_clip() {
        Ok(c) => c,
        Err(e) => {
            log::info!("Send skipped: {e}");
            #[cfg(target_os = "macos")]
            notify("Nothing to send", &e);
            return;
        }
    };

    match clipd_core::sync::send_clip(&clip, None) {
        Ok(device) => {
            log::info!("📤 Sent to {}: {}", device.name, truncate(&clip.preview, 60));
            #[cfg(target_os = "macos")]
            notify(
                &format!("Sent to {}", device.name),
                &truncate(&clip.preview, 80),
            );
        }
        Err(e) => {
            log::warn!("Send failed: {e}");
            #[cfg(target_os = "macos")]
            notify("Couldn't send", &e);
        }
    }
}

/// Build a clip from the current clipboard, files first.
///
/// Same ordering as the watcher, and for the same reason: a Finder copy also
/// puts text on the pasteboard, so checking text first would send the filename
/// as a string instead of the file.
fn clipboard_as_clip() -> Result<ClipEntry, String> {
    let files = clipd_core::clipboard_read_file_urls();
    if !files.is_empty() {
        let refs = clipd_core::save_files(&files);
        if refs.is_empty() {
            return Err("The copied files couldn't be read.".into());
        }
        return Ok(ClipEntry::new_files(refs, None));
    }

    match clipd_core::clipboard_read_text() {
        Some(text) if !text.trim().is_empty() => Ok(ClipEntry::new(text, None, None)),
        _ => Err("Copy something first, then press Ctrl+Shift+S.".into()),
    }
}

/// Listen for clips sent directly over the local network, and keep this
/// machine discoverable.
///
/// Binds port 0 so the OS picks a free port — a fixed port would collide with
/// anything else and would tell the network what this machine runs. mDNS
/// carries the real port, so nothing has to guess.
fn run_lan_transport(db_path: &std::path::Path, stop: Arc<AtomicBool>) {
    let identity = match clipd_core::lan_identity::Identity::load_or_create() {
        Ok(i) => i,
        Err(e) => {
            log::warn!("LAN sending is off — couldn't load this machine's key: {e}");
            return;
        }
    };

    let listener = match std::net::TcpListener::bind("0.0.0.0:0") {
        Ok(l) => l,
        Err(e) => {
            log::warn!("LAN sending is off — couldn't open a port: {e}");
            return;
        }
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            log::warn!("LAN sending is off — couldn't read the port: {e}");
            return;
        }
    };

    // Announce + browse on their own thread; the accept loop owns this one.
    let discovery_stop = stop.clone();
    let _ = std::thread::Builder::new()
        .name("clipd-lan-mdns".into())
        .spawn(move || {
            if let Err(e) = clipd_core::lan_discovery::run(port, discovery_stop) {
                log::warn!("Network discovery stopped: {e}");
            }
        });

    let store = match ClipStore::new(db_path) {
        Ok(s) => s,
        Err(e) => {
            log::error!("LAN transport couldn't open the store: {e}");
            return;
        }
    };

    // Unblock accept() periodically so a quit is not stuck waiting for a
    // connection that never comes.
    let _ = listener.set_nonblocking(true);
    log::info!("🔌 Listening for clips on the local network (port {port})");

    while !stop.load(Ordering::Relaxed) {
        let (mut stream, from) = match listener.accept() {
            Ok(pair) => pair,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            Err(e) => {
                log::debug!("LAN accept failed: {e}");
                continue;
            }
        };
        // The socket itself must block, even though the listener does not.
        let _ = stream.set_nonblocking(false);

        let trusted = |device_id: &str, key: &x25519_dalek::PublicKey| {
            clipd_core::lan_identity::is_trusted(device_id, key)
        };

        let mut accept = |envelope: clipd_core::Envelope, from_name: &str| -> Result<i64, String> {
            let clip = clipd_core::clip_from_envelope(&envelope, &clipd_core::files_dir())?;
            let summary = envelope.summary();
            let id = store
                .insert(&clip)
                .map_err(|e| format!("Couldn't save that clip: {e}"))?;
            log::info!("📥 Received clip #{id} over the network: {summary}");
            #[cfg(target_os = "macos")]
            notify(&summary, &truncate(&clip.preview, 80));
            let _ = from_name;
            Ok(id)
        };

        if let Err(e) =
            clipd_core::lan::serve_connection(&mut stream, &identity, &trusted, &mut accept)
        {
            // Refusals are normal and worth seeing, but they are not errors in
            // the daemon — an unpaired machine trying its luck is the system
            // working.
            log::info!("Refused or failed a connection from {from}: {e}");
        }
    }
}

/// How often the inbox is checked for clips sent from another Mac. Fast enough
/// that a link feels like it arrives on its own, slow enough to be invisible.
const INBOX_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// Announce this Mac, then collect envelopes other Macs drop into its inbox.
///
/// Received clips land in history and are **not** put on the live clipboard.
/// Silently replacing what someone has copied on the machine they are sitting
/// at is a good way to destroy their work; the notification is the handle for
/// promoting it deliberately.
fn run_sync_inbox(db_path: &std::path::Path, stop: Arc<AtomicBool>) {
    let Some(root) = clipd_core::sync_root() else {
        log::info!("📡 Sending between Macs is off (iCloud Drive isn't enabled)");
        return;
    };

    let me = match clipd_core::register_device(&root) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("Couldn't announce this Mac for sync: {e}");
            return;
        }
    };
    log::info!("📡 Sync ready — this Mac is \"{}\"", me.name);

    let store = match ClipStore::new(db_path) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Sync inbox couldn't open the store: {e}");
            return;
        }
    };

    while !stop.load(Ordering::Relaxed) {
        clipd_core::sync::request_downloads(&root, &me.id);

        for (path, parsed) in clipd_core::pending(&root, &me.id) {
            let envelope = match parsed {
                Ok(e) => e,
                Err(e) => {
                    // A file we cannot parse will never become parseable.
                    // Leaving it would mean retrying it every 3 seconds forever.
                    log::warn!("Discarding an unreadable envelope: {e}");
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            };

            let clip = match clipd_core::clip_from_envelope(&envelope, &clipd_core::files_dir()) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("Couldn't unpack a clip from {}: {e}", envelope.from_name);
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            };

            let summary = envelope.summary();
            match store.insert(&clip) {
                Ok(id) => {
                    log::info!("📥 Received clip #{id}: {summary}");
                    // Only remove the envelope once it is safely in the store,
                    // so a crash mid-ingest replays rather than loses the clip.
                    let _ = std::fs::remove_file(&path);
                    #[cfg(target_os = "macos")]
                    notify(&summary, &truncate(&clip.preview, 80));
                }
                Err(e) => {
                    // Keep the envelope and try again on the next pass.
                    log::error!("Couldn't save a received clip (will retry): {e}");
                }
            }
        }

        // Refresh our heartbeat so the other Mac keeps offering this one as a
        // target, then sleep in slices so a quit is not held up by a full tick.
        let _ = clipd_core::register_device(&root);
        for _ in 0..INBOX_POLL.as_secs() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

/// Bring an already-running clipd window to the front. Tries the names the
/// eframe app may register under, but only treats a process as the GUI when it
/// owns at least one window. The menu-bar host can also be named "Clipd"; if we
/// match that zero-window process, Ctrl+G appears to do nothing.
/// Returns true if a window was focused.
#[cfg(target_os = "macos")]
fn focus_existing_gui() -> bool {
    let script = r#"tell application "System Events"
  repeat with n in {"clipd-gui", "Clipd", "clipd"}
    set matches to (every process whose name is (n as string))
    repeat with p in matches
      if (count of windows of p) > 0 then
        set frontmost of p to true
        return "ok"
      end if
    end repeat
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

/// Bring the existing Windows palette to the foreground instead of launching
/// another GUI process (and, before the GUI subsystem fix, another console).
#[cfg(target_os = "windows")]
fn focus_existing_gui() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    let title: Vec<u16> = "clipd".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: title is a valid nul-terminated UTF-16 string and HWND is used
    // only with Win32 window-management calls.
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

/// Launch the quick slot picker popup (next to the daemon binary), detached.
#[cfg(target_os = "windows")]
fn spawn_picker() {
    use std::os::windows::process::CommandExt;
    const DETACHED: u32 = 0x0000_0008;
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["clipd-picker.exe", "clipd-picker"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    let _ = std::process::Command::new(&candidate)
                        .creation_flags(DETACHED)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                    return;
                }
            }
        }
    }
    log::warn!("clipd-picker binary not found next to the daemon");
}

fn open_gui() {
    // Coalesce duplicate hotkey delivery and impatient repeated presses while
    // the first GUI process is still creating its window.
    static LAST_OPEN: std::sync::OnceLock<std::sync::Mutex<Option<Instant>>> =
        std::sync::OnceLock::new();
    let gate = LAST_OPEN.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut last) = gate.lock() {
        let now = Instant::now();
        if last.map_or(false, |previous| {
            now.duration_since(previous) < Duration::from_millis(900)
        }) {
            return;
        }
        *last = Some(now);
    }

    log::info!("🖥️  Opening GUI...");

    // Remember which app the user was in (before clipd steals focus) so the GUI
    // can hand focus back after a copy.
    #[cfg(target_os = "macos")]
    if let Some(app) = get_frontmost_app_name() {
        if app != "clipd-gui" && app != "Clipd" && app != "clipd" {
            clipd_core::save_last_active_app(&app);
        }
    }

    // Only front an existing *palette*. The island and the tray popover are
    // also `clipd-gui` processes and they are always alive — the popover is
    // kept resident so hovering the tray icon does not pay for a process
    // launch. `focus_existing_gui` asks System Events for any clipd-gui with a
    // window, so once those two existed it always said yes, and this returned
    // early every time: the shortcut fronted the parked popover instead of
    // opening the palette, which looks exactly like the shortcut being dead.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if clipd_core::surface_is_running("gui-main") && focus_existing_gui() {
        return;
    }

    // Look for clipd-gui next to the current binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED: u32 = 0x0000_0008;
                for name in ["clipd-gui.exe", "clipd-gui"] {
                    let candidate = dir.join(name);
                    if candidate.exists()
                        && std::process::Command::new(&candidate)
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .creation_flags(DETACHED)
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
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
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
        use std::os::windows::process::CommandExt;
        const DETACHED: u32 = 0x0000_0008;
        for name in ["clipd-gui.exe", "clipd-gui"] {
            if std::process::Command::new(name)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(DETACHED)
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
    log::warn!("clipd-gui binary not found — build it with: cargo build --release -p clipd-gui");
}

/// Open a macOS native dialog to pick a slot, then paste its content at cursor.
/// Shows all non-empty slots with truncated previews.
#[cfg(target_os = "macos")]
fn open_slot_picker(mgr: &SlotManager, suppress: &Arc<AtomicBool>) {
    // Collect non-empty slots
    let mut slots: Vec<(u8, String)> = Vec::new();
    for slot_id in 1..=MAX_CLIP_SLOT {
        if let Ok(Some(content)) = mgr.get_slot(slot_id) {
            if !content.trim().is_empty() {
                let preview = content
                    .chars()
                    .take(50)
                    .collect::<String>()
                    .replace('\n', " ")
                    .replace('"', "'");
                slots.push((slot_id, preview));
            }
        }
    }

    if slots.is_empty() {
        log::info!("📋 Slot picker: no non-empty slots");
        let script = r#"display dialog "No slots saved yet. Copy something with Cmd+C first!" buttons {"OK"} with title "clipd""#;
        let _ = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .output();
        return;
    }

    // Build osascript list items
    let items: Vec<String> = slots
        .iter()
        .map(|(id, preview)| format!("\"{}: {}\"", id, preview))
        .collect();

    let items_list = items.join(", ");
    let script = format!(
        r#"set chosen to choose from list {{{}}} with prompt "📋 clipd — Select slot to paste:" with title "clipd slot picker" OK button name "Paste" cancel button name "Cancel"
if chosen is false then
  return ""
end if
set AppleScript's text item delimiters to ": "
set slot_num to text item 1 of (item 1 of chosen)
return slot_num"#,
        items_list
    );

    let output = match std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log::warn!("Slot picker osascript failed: {}", e);
            return;
        }
    };

    let chosen = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if chosen.is_empty() {
        log::info!("📋 Slot picker cancelled");
        return;
    }

    let slot: u8 = match chosen.parse() {
        Ok(n) => n,
        Err(_) => {
            log::warn!("Slot picker: could not parse slot number '{}'", chosen);
            return;
        }
    };

    log::info!("📋 Slot picker: selected slot {}", slot);
    suppress.store(true, Ordering::SeqCst);

    if let Ok(Some(content)) = mgr.get_slot(slot) {
        if let Ok(mut cb) = Clipboard::new() {
            let original = cb.get_text().ok();
            if cb.set_text(&content).is_ok() {
                std::thread::sleep(Duration::from_millis(50));
                simulate_paste();
                std::thread::sleep(Duration::from_millis(200));

                // Restore original clipboard
                if let Some(ref orig) = original {
                    let _ = cb.set_text(orig);
                }
            }
        }
    } else {
        log::info!("📋 Slot {} is empty", slot);
    }

    suppress.store(false, Ordering::SeqCst);
}

// ── Helpers ──

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim().replace(['\n', '\t', '\r'], " ");
    let char_count: usize = trimmed.chars().count();
    if char_count > max {
        let end: String = trimmed.chars().take(max).collect();
        format!("{}…", end)
    } else {
        trimmed
    }
}

/// Non-macOS stub for slot picker — falls back to TUI search.
#[cfg(not(target_os = "macos"))]
fn open_slot_picker(mgr: &SlotManager, suppress: &Arc<AtomicBool>) {
    log::info!("📋 Slot picker not yet available on this platform — try Ctrl+R for TUI search");
    let _ = (mgr, suppress);
}

/// Non-macOS stub for copy slot picker.
#[cfg(not(target_os = "macos"))]
fn open_copy_slot_picker(_mgr: &SlotManager, _persist_tx: &mpsc::SyncSender<ClipEvent>) {
    log::info!(
        "📋 Copy slot picker not yet available on this platform — try Ctrl+R for TUI search"
    );
}

/// Open a macOS native dialog to pick a slot, then copy the current clipboard to that slot.
#[cfg(target_os = "macos")]
fn open_copy_slot_picker(mgr: &SlotManager, persist_tx: &mpsc::SyncSender<ClipEvent>) {
    // First, get the current clipboard content
    let clipboard_content = match Clipboard::new() {
        Ok(mut cb) => cb.get_text().ok(),
        Err(_) => None,
    };

    let clipboard_preview = clipboard_content
        .as_ref()
        .map(|c| {
            c.chars()
                .take(50)
                .collect::<String>()
                .replace('\n', " ")
                .replace('"', "'")
        })
        .unwrap_or_else(|| "".to_string());

    // Build osascript list items for all slots
    let mut items: Vec<String> = Vec::new();
    for slot_id in 1..=MAX_CLIP_SLOT {
        if let Ok(Some(content)) = mgr.get_slot(slot_id) {
            if !content.trim().is_empty() {
                let preview = content
                    .chars()
                    .take(40)
                    .collect::<String>()
                    .replace('\n', " ")
                    .replace('"', "'");
                items.push(format!("\"{}: {} (in slot)\"", slot_id, preview));
            } else {
                items.push(format!("\"{}: (empty)\"", slot_id));
            }
        } else {
            items.push(format!("\"{}: (empty)\"", slot_id));
        }
    }

    // Add "Save to new slot" option at the top if clipboard has content
    let mut prompt = "📋 clipd — Select slot to COPY current clipboard to:".to_string();
    if !clipboard_preview.is_empty() {
        prompt = format!(
            "📋 clipd — Copy to slot (clipboard: \"{}\"):",
            clipboard_preview
        );
    }

    let items_list = items.join(", ");
    let script = format!(
        r#"set chosen to choose from list {{{}}} with prompt "{}" with title "clipd — Copy to Slot" OK button name "Copy" cancel button name "Cancel"
if chosen is false then
  return ""
end if
set AppleScript's text item delimiters to ": "
set slot_num to text item 1 of (item 1 of chosen)
return slot_num"#,
        items_list, prompt
    );

    let output = match std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log::warn!("Copy slot picker osascript failed: {}", e);
            return;
        }
    };

    let chosen = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if chosen.is_empty() {
        log::info!("📋 Copy slot picker cancelled");
        return;
    }

    let slot: u8 = match chosen.parse() {
        Ok(n) => n,
        Err(_) => {
            log::warn!("Copy slot picker: could not parse slot number '{}'", chosen);
            return;
        }
    };

    if let Some(content) = clipboard_content {
        if content.trim().is_empty() {
            log::info!("📋 Copy slot picker: clipboard is empty, nothing to copy");
            return;
        }

        save_text_to_slot(slot, &content, mgr, persist_tx);
        #[cfg(target_os = "macos")]
        show_slot_notification("Copy", slot);
    } else {
        log::info!("📋 Copy slot picker: could not read clipboard");
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

/// Keep trying to install the event tap while permission changes settle.
///
/// macOS often needs the user to flip Accessibility / Input Monitoring in
/// System Settings after the first failed tap. Without a retry loop, multi-slot
/// copy stays dead until a full Clipd restart — even after the toggles are on.
/// Keep the passive retry alive for the lifetime of the tray host. The
/// open-GUI-only listener runs alongside it so Ctrl+Space still works while
/// macOS is waiting for either privacy toggle.
#[cfg(target_os = "macos")]
fn run_macos_hotkey_listener_with_retry(
    hotkey_tx: mpsc::Sender<HotkeyTick>,
    stop_listener: Arc<AtomicBool>,
    fallback_tx: mpsc::Sender<HotkeyTick>,
    fallback_stop: Arc<AtomicBool>,
) {
    const FAST_RETRY_PAUSE: Duration = Duration::from_secs(2);
    const SETTLED_RETRY_PAUSE: Duration = Duration::from_secs(10);
    const FAST_ATTEMPTS: u64 = 45;

    // Start the listen-only fallback immediately so Settings shortcuts
    // (Ctrl+Space / palette) work while the modifying tap is retrying.
    {
        let tx = fallback_tx.clone();
        let stop = fallback_stop.clone();
        let _ = std::thread::Builder::new()
            .name("clipd-hotkey-fallback".into())
            .spawn(move || {
                log::info!(
                    "🎹 Starting listen-only fallback for Settings shortcuts \
                     (open-GUI / palette) while multi-slot tap retries"
                );
                if let Err(fallback_err) = start_macos_open_gui_fallback_listener(tx, stop) {
                    log::error!("Settings-shortcut fallback listener failed: {}", fallback_err);
                }
            });
    }

    let mut attempt = 0_u64;
    loop {
        if stop_listener.load(Ordering::Relaxed) {
            return;
        }

        // Status check only on this background thread. The actual TCC *prompt*
        // is issued on clipd-ui's main thread at launch — macOS ignores
        // AXIsProcessTrustedWithOptions / CGRequestListenEventAccess from here.
        let granted = clipd_core::keyboard_permissions_granted();
        if !granted {
            save_hotkey_status(HotkeyStatus::NeedsAccessibility);
        }

        // On success this blocks forever inside CFRunLoopRun.
        match start_macos_hotkey_listener(hotkey_tx.clone(), stop_listener.clone()) {
            Ok(()) => {
                // Runloop exited (stop/shutdown). Status was set on first event.
                log::info!("🎹 Hotkey listener stopped");
                return;
            }
            Err(e) => {
                attempt = attempt.saturating_add(1);
                // Be loud during startup, then reduce an expected missing-TCC
                // state to one useful line per minute instead of flooding the
                // log forever while still retrying forever.
                if attempt <= 3 || attempt % 6 == 0 {
                    log::error!(
                        "Hotkey listener unavailable (attempt {}): {} (Accessibility={} InputMonitoring={})",
                        attempt,
                        e,
                        clipd_core::accessibility_granted(),
                        clipd_core::input_monitoring_granted(),
                    );
                }
                save_hotkey_status(HotkeyStatus::NeedsAccessibility);

                // Wait for the user to flip the toggles, then try again.
                let retry_pause = if attempt < FAST_ATTEMPTS {
                    FAST_RETRY_PAUSE
                } else {
                    SETTLED_RETRY_PAUSE
                };
                let deadline = Instant::now() + retry_pause;
                while Instant::now() < deadline {
                    if stop_listener.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn start_macos_hotkey_listener(
    tx: mpsc::Sender<HotkeyTick>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    struct State {
        pressed_mods: HashSet<RKey>,
        event_count: u64,
        // macOS key-repeat sends many KeyPress events while V/C is held. Count one tap per
        // physical press→release, not per repeat tick (otherwise tap #16 → wrong slot / empty).
        latch_ctrl_shift_v: bool,
        latch_ctrl_shift_s: bool,
        latch_cmd_shift_v: bool,
        latch_ctrl_shift_opt_c: bool,
        latch_cmd_opt_v: bool,
        latch_cmd_opt_c: bool,
        latch_shift_opt_c: bool,
        latch_shift_opt_v: bool,
        latch_ctrl_opt_slot: [bool; 10],
        latch_ctrl_shift_opt_slot: [bool; 10],
        latch_ctrl_opt_letter_slot: [bool; 26],
        latch_ctrl_shift_opt_letter_slot: [bool; 26],
        letter_copy_prefix_until: Option<Instant>,
        letter_paste_prefix_until: Option<Instant>,
        latch_ctrl_r: bool,
        latch_ctrl_t: bool,
        latch_ctrl_g: bool,
        latch_ctrl_opt_space: bool,
        latch_ctrl_opt_backquote: bool,
        latch_cmd_ctrl_c: bool,
        latch_cmd_ctrl_v: bool,
        latch_cmd_c: bool,
        latch_cmd_v: bool,
        latch_ctrl_c: bool,
        latch_ctrl_v: bool,
        /// Time of the last plain Cmd+C press — for double-tap quick letter save.
        cmd_c_last_press: Option<Instant>,
        /// Last bare Option tap, for the double-tap letter gesture.
        opt_last_tap: Option<Instant>,
        /// A letter copy or paste waiting for the chord's modifiers to be
        /// released. Both inject a keystroke, and both need a clean keyboard.
        pending_letter_paste: Option<u8>,
        pending_letter_copy: Option<u8>,
    }

    let state = std::cell::RefCell::new(State {
        pressed_mods: HashSet::new(),
        event_count: 0,
        latch_ctrl_shift_v: false,
        latch_ctrl_shift_s: false,
        latch_cmd_shift_v: false,
        latch_ctrl_shift_opt_c: false,
        latch_cmd_opt_v: false,
        latch_cmd_opt_c: false,
        latch_shift_opt_c: false,
        latch_shift_opt_v: false,
        latch_ctrl_opt_slot: [false; 10],
        latch_ctrl_shift_opt_slot: [false; 10],
        latch_ctrl_opt_letter_slot: [false; 26],
        latch_ctrl_shift_opt_letter_slot: [false; 26],
        letter_copy_prefix_until: None,
        letter_paste_prefix_until: None,
        latch_ctrl_r: false,
        latch_ctrl_t: false,
        latch_ctrl_g: false,
        latch_ctrl_opt_space: false,
        latch_ctrl_opt_backquote: false,
        latch_cmd_ctrl_c: false,
        latch_cmd_ctrl_v: false,
        latch_cmd_c: false,
        latch_cmd_v: false,
        latch_ctrl_c: false,
        latch_ctrl_v: false,
        cmd_c_last_press: None,
        opt_last_tap: None,
        pending_letter_paste: None,
        pending_letter_copy: None,
    });

    grab(move |event: Event| {
        if stop.load(Ordering::Relaxed) {
            return Some(event);
        }

        let mut s = state.borrow_mut();
        s.event_count += 1;
        if s.event_count == 1 {
            save_hotkey_status(HotkeyStatus::Ok);
            log::info!(
                "🎹 Hotkey listener up — multi-slot copy/paste active \
                 (first event received; Input Monitoring OK)"
            );
        }

        match event.event_type.clone() {
            EventType::KeyPress(key) => {
                if is_modifier_key(key) {
                    // Two taps of Option, nothing else in between, arm a letter
                    // slot save.
                    //
                    // Option *alone* types nothing, which is the whole point.
                    // Option+C and Option+V looked like a tidy mirrored pair
                    // but they emit ç and √ — so every time clipd failed to
                    // swallow one, the character landed in the document. A
                    // modifier on its own cannot do that.
                    if matches!(key, RKey::Alt | RKey::AltGr)
                        && letter_capture_active()
                        && s.pressed_mods.is_empty()
                    {
                        let now = Instant::now();
                        let is_second = s
                            .opt_last_tap
                            .is_some_and(|p| now.duration_since(p) < QUICK_DOUBLE_WINDOW);
                        if is_second {
                            s.letter_copy_prefix_until = Some(now + QUICK_LETTER_GRACE);
                            s.letter_paste_prefix_until = None;
                            s.opt_last_tap = None;
                            log::info!("⌨️  Option ×2 → letter slot copy armed");
                        } else {
                            s.opt_last_tap = Some(now);
                        }
                    }
                    s.pressed_mods.insert(key);
                    return Some(event);
                }

                // Any other key ends a half-finished double-tap, so Option used
                // as an ordinary modifier never drifts into arming the gesture.
                s.opt_last_tap = None;


                // A letter counts whether or not the leader's modifiers are
                // still down.
                //
                // Requiring a *bare* letter meant you had to fully release
                // ⌘⌥ (or ⌃⌥) before pressing it — nobody does that mid-chord,
                // so the leader forms looked broken to anyone who held the
                // modifiers the way a chord invites you to. Windows already
                // accepted the letter either way; macOS did not.
                //
                // Only while a prefix is actually armed, and that window is
                // under a second, so no ordinary chord gets caught by it.
                let prefix_armed = {
                    let now = Instant::now();
                    s.letter_copy_prefix_until.is_some_and(|u| now <= u)
                        || s.letter_paste_prefix_until.is_some_and(|u| now <= u)
                };
                if letter_capture_active()
                    && (is_bare_letter(&s.pressed_mods) || prefix_armed)
                {
                    let now = Instant::now();
                    if let Some(until) = s.letter_copy_prefix_until {
                        if now <= until {
                            if let Some((slot, _idx, letter)) = key_to_letter_slot(key) {
                                s.letter_copy_prefix_until = None;
                                s.letter_paste_prefix_until = None;
                                log::info!(
                                    "⌨️  Ctrl+Option+C then {} → copy to slot {}",
                                    letter,
                                    slot
                                );
                                // Wait for the chord to be let go, exactly as
                                // the paste side does.
                                //
                                // The copy is grabbed by injecting ⌘C, and
                                // macOS adds that to whatever is physically
                                // held — so firing under ⌘⌥ sends ⌘⌥C, the
                                // selection never reaches the clipboard, and
                                // the slot quietly stores whatever was on it
                                // before. The HUD still says "copied", which is
                                // what makes this one hard to spot.
                                if s.pressed_mods.is_empty() {
                                    let _ = tx.send(HotkeyTick::CopySelectionToSlot(slot));
                                } else {
                                    s.pending_letter_copy = Some(slot);
                                }
                                return None;
                            }
                        } else {
                            s.letter_copy_prefix_until = None;
                        }
                    }
                    if let Some(until) = s.letter_paste_prefix_until {
                        if now <= until {
                            if let Some((slot, _idx, letter)) = key_to_letter_slot(key) {
                                s.letter_copy_prefix_until = None;
                                s.letter_paste_prefix_until = None;
                                log::info!(
                                    "⌨️  Option+V leader then {} → paste slot {}",
                                    letter,
                                    slot
                                );
                                // Wait for the chord to be let go before
                                // pasting.
                                //
                                // The paste is injected as ⌘V, and macOS adds
                                // it to whatever is *physically* held — so
                                // firing while ⌘⌥ is still down delivers ⌘⌥V,
                                // which is not a paste. Copy survives this;
                                // paste does not. Windows already releases the
                                // modifiers before injecting; this is the
                                // macOS equivalent.
                                if s.pressed_mods.is_empty() {
                                    let _ = tx.send(HotkeyTick::PasteFromSlot(slot));
                                } else {
                                    s.pending_letter_paste = Some(slot);
                                }
                                return None;
                            }
                        } else {
                            s.letter_paste_prefix_until = None;
                        }
                    }
                }

                // Ctrl+Shift+S → send the clipboard to the other Mac.
                // Returns the event: this chord has no system meaning, and
                // swallowing it would break any app that binds it itself.
                if is_ctrl_shift(&s.pressed_mods) && key == RKey::KeyS {
                    if !s.latch_ctrl_shift_s {
                        s.latch_ctrl_shift_s = true;
                        log::info!("⌨️  Ctrl+Shift+S → send to other Mac");
                        let _ = tx.send(HotkeyTick::SendToDevice);
                    }
                    return Some(event);
                }

                // Ctrl+Shift+V → smart paste (transform clipboard + paste)
                if is_ctrl_shift(&s.pressed_mods) && key == RKey::KeyV {
                    if !s.latch_ctrl_shift_v {
                        s.latch_ctrl_shift_v = true;
                        log::info!("⌨️  Ctrl+Shift+V → smart paste");
                        let _ = tx.send(HotkeyTick::SmartPaste);
                    }
                    return Some(event);
                }

                // Memory palette — chord comes from Settings → Palette shortcut.
                if matches_palette_trigger(key, &s.pressed_mods) {
                    if !s.latch_cmd_shift_v {
                        s.latch_cmd_shift_v = true;
                        log::info!(
                            "⌨️  {} → memory palette",
                            load_paste_transform_settings().palette_trigger.label()
                        );
                        let _ = tx.send(HotkeyTick::SlotPicker);
                    }
                    // Swallow Space variants so they don't type into the focused app.
                    return if key == RKey::Space {
                        None
                    } else {
                        Some(event)
                    };
                }

                // Ctrl+Shift+Option+A..Z → copy selected text to letter slots 31..56.
                // This takes precedence over the old Ctrl+Shift+Option+C picker so every
                // letter maps cleanly to a slot.
                if is_ctrl_shift_opt(&s.pressed_mods) && direct_letter_shortcuts_enabled() {
                    if let Some((slot, idx, letter)) = key_to_letter_slot(key) {
                        if !s.latch_ctrl_shift_opt_letter_slot[idx] {
                            s.latch_ctrl_shift_opt_letter_slot[idx] = true;
                            log::info!("⌨️  Ctrl+Shift+Option+{} → copy to slot {}", letter, slot);
                            let _ = tx.send(HotkeyTick::CopySelectionToSlot(slot));
                        }
                        return None;
                    }
                }

                // Ctrl+Shift+Option+C → copy slot picker
                if is_ctrl_shift_opt(&s.pressed_mods) && key == RKey::KeyC {
                    if !s.latch_ctrl_shift_opt_c {
                        s.latch_ctrl_shift_opt_c = true;
                        log::info!("⌨️  Ctrl+Shift+Option+C → copy slot picker");
                        let _ = tx.send(HotkeyTick::CopySlotPicker);
                    }
                    return Some(event);
                }

                // Option+C/V → upper slot bank in Excel/developer mode.
                // Suppress these events so Option+C does not type into Excel.
                if extended_slots_enabled() && is_opt_only(&s.pressed_mods) && key == RKey::KeyC {
                    if !s.latch_shift_opt_c {
                        s.latch_shift_opt_c = true;
                        let _ = tx.send(HotkeyTick::UpperCopyTap);
                    }
                    return None;
                }
                if extended_slots_enabled() && is_opt_only(&s.pressed_mods) && key == RKey::KeyV {
                    if !s.latch_shift_opt_v {
                        s.latch_shift_opt_v = true;
                        let _ = tx.send(HotkeyTick::UpperPasteTap);
                    }
                    return None;
                }

                // Ctrl+Shift+Option+1..9 → save current clipboard to slot directly
                if is_ctrl_shift_opt(&s.pressed_mods) {
                    if let Some(slot) = key_to_digit_slot(key) {
                        let idx = slot as usize;
                        if !s.latch_ctrl_shift_opt_slot[idx] {
                            s.latch_ctrl_shift_opt_slot[idx] = true;
                            log::info!("⌨️  Ctrl+Shift+Option+{} → save to slot", slot);
                            let _ = tx.send(HotkeyTick::CopyClipboardToSlot(slot));
                        }
                        return Some(event);
                    }
                    if direct_letter_shortcuts_enabled() {
                        if let Some((slot, idx, letter)) = key_to_letter_slot(key) {
                            if !s.latch_ctrl_shift_opt_letter_slot[idx] {
                                s.latch_ctrl_shift_opt_letter_slot[idx] = true;
                                log::info!(
                                    "⌨️  Ctrl+Shift+Option+{} → copy to slot {}",
                                    letter,
                                    slot
                                );
                                let _ = tx.send(HotkeyTick::CopySelectionToSlot(slot));
                            }
                            return None;
                        }
                    }
                }

                // Ctrl+Option+1..9 → paste from slot directly
                if is_ctrl_opt(&s.pressed_mods) {
                    // Ctrl+Option+Space is the memory-palette chord when chosen in
                    // Settings; otherwise keep the legacy slot-memory HUD binding.
                    if key == RKey::Space {
                        if !s.latch_ctrl_opt_space {
                            s.latch_ctrl_opt_space = true;
                            let settings = load_paste_transform_settings();
                            if settings.palette_enabled
                                && settings.palette_trigger == PaletteTrigger::CtrlOptSpace
                            {
                                log::info!("⌨️  Ctrl+Option+Space → memory palette");
                                let _ = tx.send(HotkeyTick::SlotPicker);
                            } else {
                                log::info!("⌨️  Ctrl+Option+Space → slot memory HUD");
                                let _ = tx.send(HotkeyTick::SlotMemoryHud);
                            }
                        }
                        return None;
                    }
                    if key == RKey::BackQuote {
                        if !s.latch_ctrl_opt_backquote {
                            s.latch_ctrl_opt_backquote = true;
                            log::info!("⌨️  Ctrl+Option+` → toggle collect mode");
                            let _ = tx.send(HotkeyTick::ToggleCollect);
                        }
                        return None;
                    }
                    if direct_letter_shortcuts_enabled() && key == RKey::KeyC {
                        s.letter_copy_prefix_until = Some(Instant::now() + LETTER_PREFIX_WINDOW);
                        s.letter_paste_prefix_until = None;
                        log::info!("⌨️  Ctrl+Option+C → letter copy prefix");
                        return None;
                    }
                    if direct_letter_shortcuts_enabled() && key == RKey::KeyV {
                        s.letter_paste_prefix_until = Some(Instant::now() + LETTER_PREFIX_WINDOW);
                        s.letter_copy_prefix_until = None;
                        log::info!("⌨️  Ctrl+Option+V → letter paste prefix");
                        return None;
                    }
                    if let Some(slot) = key_to_digit_slot(key) {
                        let idx = slot as usize;
                        if !s.latch_ctrl_opt_slot[idx] {
                            s.latch_ctrl_opt_slot[idx] = true;
                            log::info!("⌨️  Ctrl+Option+{} → paste slot", slot);
                            let _ = tx.send(HotkeyTick::PasteFromSlot(slot));
                        }
                        return Some(event);
                    }
                    if direct_letter_shortcuts_enabled() {
                        if let Some((slot, idx, letter)) = key_to_letter_slot(key) {
                            if !s.latch_ctrl_opt_letter_slot[idx] {
                                s.latch_ctrl_opt_letter_slot[idx] = true;
                                log::info!("⌨️  Ctrl+Option+{} → paste slot {}", letter, slot);
                                let _ = tx.send(HotkeyTick::PasteFromSlot(slot));
                            }
                            return None;
                        }
                    }
                }

                // Cmd+Option+V → sequence paste
                // Cmd+Option+C / Cmd+Option+V then a letter → letter slots.
                //
                // Cmd suppresses Option's character composition, so unlike
                // Option+C these emit nothing if a swallow is ever missed —
                // which is what ruled that pair out. Checked before batch-drain
                // so the letter leader owns Cmd+Option+V; batch-drain moved to
                // Cmd+Option+Shift+V rather than being quietly overridden.
                if letter_capture_active()
                    && is_cmd_opt(&s.pressed_mods)
                    && matches!(key, RKey::KeyC | RKey::KeyV)
                {
                    let now = Instant::now();
                    if key == RKey::KeyC {
                        if !s.latch_cmd_opt_c {
                            s.latch_cmd_opt_c = true;
                            s.letter_copy_prefix_until = Some(now + LETTER_PREFIX_WINDOW);
                            s.letter_paste_prefix_until = None;
                            log::info!("⌨️  Cmd+Option+C → letter slot copy armed");
                        }
                    } else if !s.latch_cmd_opt_v {
                        s.latch_cmd_opt_v = true;
                        s.letter_paste_prefix_until = Some(now + LETTER_PREFIX_WINDOW);
                        s.letter_copy_prefix_until = None;
                        log::info!("⌨️  Cmd+Option+V → letter slot paste armed");
                    }
                    return None;
                }

                // Batch-drain moved off Cmd+Option+V to make room for the
                // letter leader above.
                if batch_drain_enabled()
                    && is_cmd_opt_shift(&s.pressed_mods)
                    && key == RKey::KeyV
                {
                    if !s.latch_cmd_opt_v {
                        s.latch_cmd_opt_v = true;
                        log::info!("⌨️  Cmd+Option+Shift+V → sequence paste (batch-drain)");
                        let _ = tx.send(HotkeyTick::SequencePaste);
                    }
                    return Some(event);
                }

                // Ctrl+R → open search TUI (check before Ctrl+C/V)
                if is_ctrl_only(&s.pressed_mods) && key == RKey::KeyR {
                    if !s.latch_ctrl_r {
                        s.latch_ctrl_r = true;
                        log::info!("⌨️  Ctrl+R → open search");
                        let _ = tx.send(HotkeyTick::OpenTui);
                    }
                    return Some(event);
                }

                // Ctrl+T → open TUI
                if is_ctrl_only(&s.pressed_mods) && key == RKey::KeyT {
                    if !s.latch_ctrl_t {
                        s.latch_ctrl_t = true;
                        log::info!("⌨️  Ctrl+T → open TUI");
                        let _ = tx.send(HotkeyTick::OpenTui);
                    }
                    return Some(event);
                }

                // Open GUI — configurable chord on either G or Space. Only read
                // the setting when one of those is pressed with a modifier, so
                // bare typing never touches the config file.
                // Ctrl+Option+Space (slot HUD) is handled above and returns
                // early; `is_ctrl_only` keeps Ctrl+Space distinct from it.
                if matches!(key, RKey::KeyG | RKey::Space)
                    && (has_cmd(&s.pressed_mods)
                        || has_ctrl(&s.pressed_mods)
                        // Option+Space is a valid open-clipd binding now, and
                        // this guard would have discarded it before the match
                        // below ever ran.
                        || has_opt(&s.pressed_mods))
                    && !s.latch_ctrl_g
                {
                    let hk = load_paste_transform_settings().open_gui_hotkey;
                    let on_expected_key = hk.uses_space_key() == (key == RKey::Space);
                    let matched = on_expected_key
                        && match hk {
                            OpenGuiHotkey::CtrlG => is_ctrl_only(&s.pressed_mods),
                            // Alt+G is a Windows-only binding (Option+G types © on
                            // macOS keyboards) — treat as unbound here.
                            OpenGuiHotkey::AltG => false,
                            OpenGuiHotkey::CmdShiftG => is_cmd_shift(&s.pressed_mods),
                            OpenGuiHotkey::CtrlShiftG => is_ctrl_shift(&s.pressed_mods),
                            OpenGuiHotkey::CtrlSpace => is_ctrl_only(&s.pressed_mods),
                            OpenGuiHotkey::OptSpace => is_opt_only(&s.pressed_mods),
                            OpenGuiHotkey::Disabled => false,
                        };
                    if matched {
                        s.latch_ctrl_g = true;
                        let action = load_paste_transform_settings().ctrl_space_action;
                        log::info!("⌨️  {} → {:?}", hk.label(), action);
                        let tick = match action {
                            CtrlSpaceAction::OpenGui => HotkeyTick::OpenGui,
                            CtrlSpaceAction::SlotMemory => HotkeyTick::SlotMemoryHud,
                            CtrlSpaceAction::CommandPalette => HotkeyTick::CommandPalette,
                        };
                        let _ = tx.send(tick);
                        // Space must be swallowed or it also types into whatever
                        // had focus. G is passed through, as it always has been.
                        return if key == RKey::Space {
                            None
                        } else {
                            Some(event)
                        };
                    }
                }

                // Cmd+Ctrl+C → treat as Ctrl+C (user transitioning from Cmd+C to save slot)
                if has_cmd(&s.pressed_mods) && has_ctrl(&s.pressed_mods) && key == RKey::KeyC {
                    let _ = tx.send(HotkeyTick::CtrlCTap);
                    return Some(event);
                }
                // Cmd+Ctrl+V → treat as Ctrl+V (user transitioning from Cmd+V)
                if has_cmd(&s.pressed_mods) && has_ctrl(&s.pressed_mods) && key == RKey::KeyV {
                    let _ = tx.send(HotkeyTick::CtrlVTap);
                    return Some(event);
                }

                // Cmd+C (just Cmd, no Ctrl/Shift/Option)
                if is_cmd_only(&s.pressed_mods) && key == RKey::KeyC {
                    // Quick letter save: a deliberate double-tap of Cmd+C arms
                    // the letter prefix, so the next letter saves to that slot.
                    // A single Cmd+C is unaffected — normal copy isn't hampered.
                    // ⌘C stays purely numeric. Hanging letter slots off it
                    // meant one gesture could mean slot 2 or slot A depending
                    // on what you did next, which is exactly the ambiguity the
                    // Option double-tap exists to avoid.
                    let _ = tx.send(HotkeyTick::CmdCTap);
                    return Some(event);
                }
                // Cmd+V (just Cmd, no Ctrl/Shift/Option)
                if is_cmd_only(&s.pressed_mods) && key == RKey::KeyV {
                    // Deliberately no ⌘V ⌘V letter arming here.
                    //
                    // ⌘V ×2 already means "paste slot 2", and unlike the copy
                    // side that clash cannot be deferred: a double ⌘C can hold
                    // its numeric commit for half a second and let a following
                    // letter cancel it, but a paste has already put the content
                    // into your app by then. There is nothing left to take
                    // back. Letter paste stays on Ctrl+Option+letter, which is
                    // one chord, no timing, and collides with nothing.
                    let _ = tx.send(HotkeyTick::CmdVTap);
                    return Some(event);
                }

                // Ctrl+C (just Ctrl, no Cmd/Shift/Option)
                if is_ctrl_only(&s.pressed_mods) && key == RKey::KeyC {
                    // In terminal apps Ctrl+C is interrupt — don't treat as slot copy / HUD.
                    if !is_terminal_frontmost() {
                        let _ = tx.send(HotkeyTick::CtrlCTap);
                    }
                    return Some(event);
                }
                // Ctrl+V (just Ctrl, no Cmd/Shift/Option)
                if is_ctrl_only(&s.pressed_mods) && key == RKey::KeyV {
                    let _ = tx.send(HotkeyTick::CtrlVTap);
                    return Some(event);
                }
            }
            EventType::KeyRelease(key) => {
                if is_modifier_key(key) {
                    s.pressed_mods.remove(&key);
                    // The chord is off the keyboard — a deferred letter paste
                    // can go now, and lands as a clean ⌘V.
                    if s.pressed_mods.is_empty() {
                        if let Some(slot) = s.pending_letter_copy.take() {
                            let _ = tx.send(HotkeyTick::CopySelectionToSlot(slot));
                        }
                        if let Some(slot) = s.pending_letter_paste.take() {
                            let _ = tx.send(HotkeyTick::PasteFromSlot(slot));
                        }
                    }
                    // After Cmd+C the OS often never delivers KeyRelease for C; releasing Command
                    // must arm the next Cmd+C / Cmd+V chord.
                    if matches!(key, RKey::MetaLeft | RKey::MetaRight) {
                        s.latch_cmd_c = false;
                        s.latch_cmd_v = false;
                        s.latch_cmd_shift_v = false;
                        s.latch_cmd_ctrl_c = false;
                        s.latch_cmd_ctrl_v = false;
                    }
                    if matches!(key, RKey::ControlLeft | RKey::ControlRight) {
                        s.latch_ctrl_c = false;
                        s.latch_ctrl_v = false;
                        s.latch_ctrl_t = false;
                        s.latch_ctrl_g = false;
                        s.latch_ctrl_opt_space = false;
                    }
                    if matches!(
                        key,
                        RKey::ControlLeft
                            | RKey::ControlRight
                            | RKey::ShiftLeft
                            | RKey::ShiftRight
                            | RKey::Alt
                            | RKey::AltGr
                    ) {
                        s.latch_shift_opt_c = false;
                        s.latch_shift_opt_v = false;
                        s.latch_ctrl_opt_slot = [false; 10];
                        s.latch_ctrl_shift_opt_slot = [false; 10];
                        s.latch_ctrl_opt_letter_slot = [false; 26];
                        s.latch_ctrl_shift_opt_letter_slot = [false; 26];
                    }
                }
                match key {
                    RKey::KeyS => {
                        s.latch_ctrl_shift_s = false;
                    }
                    RKey::KeyV => {
                        s.latch_ctrl_shift_v = false;
                        s.latch_cmd_shift_v = false;
                        s.latch_cmd_opt_v = false;
                        s.latch_shift_opt_v = false;
                        s.latch_cmd_ctrl_v = false;
                        s.latch_cmd_v = false;
                        s.latch_ctrl_v = false;
                    }
                    RKey::KeyC => {
                        s.latch_cmd_opt_c = false;
                        s.latch_ctrl_shift_opt_c = false;
                        s.latch_shift_opt_c = false;
                        s.latch_cmd_ctrl_c = false;
                        s.latch_cmd_c = false;
                        s.latch_ctrl_c = false;
                    }
                    RKey::KeyR => s.latch_ctrl_r = false,
                    RKey::KeyT => s.latch_ctrl_t = false,
                    RKey::KeyG => s.latch_ctrl_g = false,
                    RKey::Space => {
                        s.latch_ctrl_opt_space = false;
                        // Ctrl+Space shares the open-GUI latch; without this it
                        // would fire once and never again. Palette Space chords
                        // reuse latch_cmd_shift_v.
                        s.latch_ctrl_g = false;
                        s.latch_cmd_shift_v = false;
                    }
                    RKey::BackQuote => s.latch_ctrl_opt_backquote = false,
                    RKey::Num1
                    | RKey::Num2
                    | RKey::Num3
                    | RKey::Num4
                    | RKey::Num5
                    | RKey::Num6
                    | RKey::Num7
                    | RKey::Num8
                    | RKey::Num9 => {
                        if let Some(slot) = key_to_digit_slot(key) {
                            s.latch_ctrl_opt_slot[slot as usize] = false;
                            s.latch_ctrl_shift_opt_slot[slot as usize] = false;
                        }
                    }
                    _ => {
                        if let Some((_slot, idx, _letter)) = key_to_letter_slot(key) {
                            s.latch_ctrl_opt_letter_slot[idx] = false;
                            s.latch_ctrl_shift_opt_letter_slot[idx] = false;
                        }
                    }
                }
            }
            _ => {}
        }
        Some(event)
    })
    .map_err(|e| format!("rdev grab error: {:?}", e))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn start_macos_open_gui_fallback_listener(
    tx: mpsc::Sender<HotkeyTick>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    struct State {
        pressed_mods: HashSet<RKey>,
        latch_ctrl_g: bool,
        latch_palette: bool,
        event_count: u64,
    }

    let mut state = State {
        pressed_mods: HashSet::new(),
        latch_ctrl_g: false,
        latch_palette: false,
        event_count: 0,
    };

    listen(move |event: Event| {
        if stop.load(Ordering::Relaxed) {
            return;
        }

        state.event_count += 1;
        if state.event_count == 1 {
            log::info!(
                "🎹 rdev listen fallback: first event received (open-GUI={}, palette={})",
                load_paste_transform_settings().open_gui_hotkey.label(),
                load_paste_transform_settings().palette_trigger.label()
            );
        }
        // A HID tap that lost its Input Monitoring grant still gets created and
        // still reports a first event, then goes quiet. Without a periodic count
        // that is indistinguishable from "the chord didn't match".
        if state.event_count % 500 == 0 {
            log::debug!("🎹 fallback listener alive: {} events", state.event_count);
        }

        match event.event_type {
            EventType::KeyPress(key) => {
                if is_modifier_key(key) {
                    state.pressed_mods.insert(key);
                    return;
                }

                // Chorded keys only: logging bare keypresses would turn the log
                // into a keylogger. Debug-level so it's opt-in when a hotkey
                // needs diagnosing.
                if has_cmd(&state.pressed_mods)
                    || has_ctrl(&state.pressed_mods)
                    || state.pressed_mods.contains(&RKey::Alt)
                    || state.pressed_mods.contains(&RKey::AltGr)
                {
                    log::debug!("🎹 chord seen: {:?} + {:?}", state.pressed_mods, key);
                }

                if matches_palette_trigger(key, &state.pressed_mods) && !state.latch_palette {
                    state.latch_palette = true;
                    log::info!(
                        "⌨️  {} → memory palette (fallback)",
                        load_paste_transform_settings().palette_trigger.label()
                    );
                    let _ = tx.send(HotkeyTick::SlotPicker);
                    return;
                }

                if matches!(key, RKey::KeyG | RKey::Space)
                    && (has_cmd(&state.pressed_mods) || has_ctrl(&state.pressed_mods))
                    && !state.latch_ctrl_g
                {
                    let hk = load_paste_transform_settings().open_gui_hotkey;
                    let on_expected_key = hk.uses_space_key() == (key == RKey::Space);
                    let matched = on_expected_key
                        && match hk {
                            OpenGuiHotkey::CtrlG => is_ctrl_only(&state.pressed_mods),
                            // Alt+G is Windows-only — unbound on macOS.
                            OpenGuiHotkey::AltG => false,
                            OpenGuiHotkey::CmdShiftG => is_cmd_shift(&state.pressed_mods),
                            OpenGuiHotkey::CtrlShiftG => is_ctrl_shift(&state.pressed_mods),
                            OpenGuiHotkey::CtrlSpace => is_ctrl_only(&state.pressed_mods),
                            OpenGuiHotkey::OptSpace => is_opt_only(&state.pressed_mods),
                            OpenGuiHotkey::Disabled => false,
                        };
                    if matched {
                        state.latch_ctrl_g = true;
                        let action = load_paste_transform_settings().ctrl_space_action;
                        log::info!("⌨️  {} → {:?} (fallback)", hk.label(), action);
                        let tick = match action {
                            CtrlSpaceAction::OpenGui => HotkeyTick::OpenGui,
                            CtrlSpaceAction::SlotMemory => HotkeyTick::SlotMemoryHud,
                            CtrlSpaceAction::CommandPalette => HotkeyTick::CommandPalette,
                        };
                        let _ = tx.send(tick);
                    }
                }
            }
            EventType::KeyRelease(key) => {
                if is_modifier_key(key) {
                    state.pressed_mods.remove(&key);
                }
                if matches!(key, RKey::KeyG | RKey::Space) {
                    state.latch_ctrl_g = false;
                }
                if matches!(key, RKey::KeyV | RKey::Space) {
                    state.latch_palette = false;
                }
            }
            _ => {}
        }
    })
    .map_err(|e| format!("rdev listen error: {:?}", e))?;
    Ok(())
}

/// Whether this key+modifier state matches the configured memory-palette shortcut.
#[cfg(target_os = "macos")]
fn matches_palette_trigger(key: RKey, pressed_mods: &HashSet<RKey>) -> bool {
    let settings = load_paste_transform_settings();
    // `palette_enabled` is honoured for configs written before "Off" existed
    // in the trigger list; new ones express it as the trigger itself.
    if !settings.palette_enabled {
        return false;
    }
    match settings.palette_trigger {
        PaletteTrigger::Off => false,
        PaletteTrigger::CmdShiftV => is_cmd_shift(pressed_mods) && key == RKey::KeyV,
        PaletteTrigger::CtrlOptSpace => is_ctrl_opt(pressed_mods) && key == RKey::Space,
        PaletteTrigger::OptSpace => is_opt_only(pressed_mods) && key == RKey::Space,
    }
}

#[cfg(target_os = "macos")]
fn is_modifier_key(key: RKey) -> bool {
    matches!(
        key,
        RKey::MetaLeft
            | RKey::MetaRight
            | RKey::ShiftLeft
            | RKey::ShiftRight
            | RKey::ControlLeft
            | RKey::ControlRight
            | RKey::Alt
            | RKey::AltGr
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

/// Option (either side) held, regardless of what else is.
#[cfg(target_os = "macos")]
fn has_opt(pressed_mods: &HashSet<RKey>) -> bool {
    pressed_mods.contains(&RKey::Alt) || pressed_mods.contains(&RKey::AltGr)
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

/// Ctrl+Option held (no Cmd, no Shift)
#[cfg(target_os = "macos")]
fn is_ctrl_opt(pressed_mods: &HashSet<RKey>) -> bool {
    has_ctrl(pressed_mods)
        && (pressed_mods.contains(&RKey::Alt) || pressed_mods.contains(&RKey::AltGr))
        && !pressed_mods.contains(&RKey::MetaLeft)
        && !pressed_mods.contains(&RKey::MetaRight)
        && !pressed_mods.contains(&RKey::ShiftLeft)
        && !pressed_mods.contains(&RKey::ShiftRight)
}

/// A bare letter press — no Cmd/Ctrl/Option held (Shift is allowed). The
/// letter-slot prefix only captures these, so holding Cmd and tapping a letter
/// stays a Cmd+<key> chord (e.g. Cmd-held C×3 = numeric slot 3, not letter C).
#[cfg(target_os = "macos")]
fn is_bare_letter(pressed_mods: &HashSet<RKey>) -> bool {
    !has_cmd(pressed_mods)
        && !has_ctrl(pressed_mods)
        && !pressed_mods.contains(&RKey::Alt)
        && !pressed_mods.contains(&RKey::AltGr)
}

/// Option held alone (no Cmd, no Ctrl, no Shift)
#[cfg(target_os = "macos")]
fn is_opt_only(pressed_mods: &HashSet<RKey>) -> bool {
    (pressed_mods.contains(&RKey::Alt) || pressed_mods.contains(&RKey::AltGr))
        && !pressed_mods.contains(&RKey::MetaLeft)
        && !pressed_mods.contains(&RKey::MetaRight)
        && !pressed_mods.contains(&RKey::ControlLeft)
        && !pressed_mods.contains(&RKey::ControlRight)
        && !pressed_mods.contains(&RKey::ShiftLeft)
        && !pressed_mods.contains(&RKey::ShiftRight)
}

/// Cmd+Option+Shift held together.
#[cfg(target_os = "macos")]
fn is_cmd_opt_shift(pressed_mods: &HashSet<RKey>) -> bool {
    (pressed_mods.contains(&RKey::MetaLeft) || pressed_mods.contains(&RKey::MetaRight))
        && (pressed_mods.contains(&RKey::Alt) || pressed_mods.contains(&RKey::AltGr))
        && (pressed_mods.contains(&RKey::ShiftLeft) || pressed_mods.contains(&RKey::ShiftRight))
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

/// Cmd+Shift held (no Ctrl, no Option)
#[cfg(target_os = "macos")]
fn is_cmd_shift(pressed_mods: &HashSet<RKey>) -> bool {
    has_cmd(pressed_mods)
        && (pressed_mods.contains(&RKey::ShiftLeft) || pressed_mods.contains(&RKey::ShiftRight))
        && !pressed_mods.contains(&RKey::ControlLeft)
        && !pressed_mods.contains(&RKey::ControlRight)
        && !pressed_mods.contains(&RKey::Alt)
        && !pressed_mods.contains(&RKey::AltGr)
}

/// Ctrl+Shift+Option held (no Cmd)
#[cfg(target_os = "macos")]
fn is_ctrl_shift_opt(pressed_mods: &HashSet<RKey>) -> bool {
    has_ctrl(pressed_mods)
        && (pressed_mods.contains(&RKey::ShiftLeft) || pressed_mods.contains(&RKey::ShiftRight))
        && (pressed_mods.contains(&RKey::Alt) || pressed_mods.contains(&RKey::AltGr))
        && !pressed_mods.contains(&RKey::MetaLeft)
        && !pressed_mods.contains(&RKey::MetaRight)
}

#[cfg(target_os = "macos")]
fn key_to_digit_slot(key: RKey) -> Option<u8> {
    match key {
        RKey::Num1 => Some(1),
        RKey::Num2 => Some(2),
        RKey::Num3 => Some(3),
        RKey::Num4 => Some(4),
        RKey::Num5 => Some(5),
        RKey::Num6 => Some(6),
        RKey::Num7 => Some(7),
        RKey::Num8 => Some(8),
        RKey::Num9 => Some(9),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn key_to_letter_slot(key: RKey) -> Option<(u8, usize, char)> {
    let idx = match key {
        RKey::KeyA => 0,
        RKey::KeyB => 1,
        RKey::KeyC => 2,
        RKey::KeyD => 3,
        RKey::KeyE => 4,
        RKey::KeyF => 5,
        RKey::KeyG => 6,
        RKey::KeyH => 7,
        RKey::KeyI => 8,
        RKey::KeyJ => 9,
        RKey::KeyK => 10,
        RKey::KeyL => 11,
        RKey::KeyM => 12,
        RKey::KeyN => 13,
        RKey::KeyO => 14,
        RKey::KeyP => 15,
        RKey::KeyQ => 16,
        RKey::KeyR => 17,
        RKey::KeyS => 18,
        RKey::KeyT => 19,
        RKey::KeyU => 20,
        RKey::KeyV => 21,
        RKey::KeyW => 22,
        RKey::KeyX => 23,
        RKey::KeyY => 24,
        RKey::KeyZ => 25,
        _ => return None,
    };
    Some((31 + idx as u8, idx, (b'A' + idx as u8) as char))
}

#[cfg(target_os = "macos")]
/// The global Ctrl+Option letter-slot chords are gated separately from the
/// letter-slot feature itself, so aliases can stay usable via the palette
/// without forcing the chords on everyone (Paste Settings → Advanced).
fn direct_letter_shortcuts_enabled() -> bool {
    let s = load_paste_transform_settings();
    s.letter_slots_enabled && s.direct_letter_shortcuts_enabled
}

/// Excel/developer extended slots 11-30 (Option+C/V multi-tap).
fn extended_slots_enabled() -> bool {
    load_paste_transform_settings().extended_slots_enabled
}

/// Batch-drain / sequence paste (Cmd+Option+V drains collected slots in order).
fn batch_drain_enabled() -> bool {
    load_paste_transform_settings().batch_drain_enabled
}

/// Lighter letter-slot save: a deliberate double-tap of Cmd+C arms the letter
/// prefix (single Cmd+C stays normal copy, so typing after a copy isn't eaten).
fn quick_letter_slots_enabled() -> bool {
    let s = load_paste_transform_settings();
    s.letter_slots_enabled && s.quick_letter_slots_enabled
}

/// Whether a pending letter prefix may capture the next letter into a slot —
/// true if either the Ctrl+Option chords or the quick double-tap path is on.
fn letter_capture_active() -> bool {
    let s = load_paste_transform_settings();
    s.letter_slots_enabled && (s.direct_letter_shortcuts_enabled || s.quick_letter_slots_enabled)
}

fn upper_slot_for_taps(taps: u8) -> u8 {
    // Keep the extended numeric bank disjoint from the named A-Z bank, which
    // starts at slot 31. Without this cap a long Option+C/V multi-tap sequence
    // could silently overwrite letter slots.
    taps.saturating_add(10).min(30)
}

/// Highest slot a Cmd/Ctrl multi-tap can reach. When multi-slot is disabled
/// every tap collapses to slot 1, so Cmd+C/Cmd+V behave like normal copy/paste.
fn primary_tap_slot_limit(settings: &PasteTransformSettings) -> u8 {
    if settings.multi_slot_enabled {
        9
    } else {
        1
    }
}

fn primary_slot_for_taps(taps: u8, settings: &PasteTransformSettings) -> u8 {
    taps.min(primary_tap_slot_limit(settings))
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

#[cfg(test)]
mod slot_mapping_tests {
    use super::*;

    #[test]
    fn counting_taps_is_not_a_drum_roll() {
        // TAP_WINDOW is the gap allowed between taps, and the count resets
        // when it lapses. Reaching slot 15 means five taps in a row, so the
        // window has to suit someone counting rather than someone drumming.
        assert!(
            TAP_WINDOW >= Duration::from_millis(500),
            "at {TAP_WINDOW:?} a deliberate five-tap count commits five times"
        );
        // Still short enough that the save does not feel detached from the act.
        assert!(TAP_WINDOW <= Duration::from_millis(900));
        // And comfortably above the duplicate-event guard, or taps would be
        // dropped before they could be counted.
        assert!(TAP_DEBOUNCE * 4 < TAP_WINDOW);
    }

    #[test]
    fn the_letter_gesture_stays_off_the_numeric_keys() {
        // Letter slots hang off a double-tap of Option, which types nothing on
        // its own and shares no key with ⌘C / ⌘V. Option+C / Option+V were
        // tried and are wrong twice over: they emit ç and √, and they already
        // address the extended 11–30 bank.
        assert!(
            QUICK_DOUBLE_WINDOW <= Duration::from_millis(600),
            "two taps that far apart are two separate presses"
        );
        assert!(
            QUICK_LETTER_GRACE >= Duration::from_millis(1500),
            "the letter that follows should not be a race"
        );
    }

    #[test]
    fn a_quick_letter_save_is_not_a_race() {
        // The letter window used to double as a stall on the numeric commit:
        // press the letter in time and the content went to slot A, miss and it
        // went to slot 2. One gesture, two outcomes, decided by a stopwatch.
        //
        // The letter is additive now — the content goes to both — so nothing is
        // being held back and the window only has to be comfortable.
        assert!(
            QUICK_LETTER_GRACE >= Duration::from_millis(1500),
            "a window you can miss is a window someone has to think about"
        );
        // The numeric path commits on its own timing, untouched by the letter.
        assert!(TAP_WINDOW < QUICK_LETTER_GRACE);
    }

    #[test]
    fn the_hud_steps_aside_only_for_the_island() {
        // Both surfaces open at the top centre. When the island is the layout
        // it owns that space, and the HUD is told how much to keep clear.
        let plain = "STYLE\ttoast\nTITLE\tCopied";
        assert_eq!(
            add_hud_clearance(plain, false),
            plain,
            "palette layout: nothing to avoid"
        );

        let moved = add_hud_clearance(plain, true);
        assert!(moved.starts_with(plain), "the original payload survives intact");
        assert!(moved.ends_with(&format!("\nAVOIDTOP\t{ISLAND_RESERVED_TOP}")));
        // One field per line, tab separated — the HUD's parser wants both.
        assert_eq!(moved.lines().count(), plain.lines().count() + 1);
        assert_eq!(moved.lines().last().unwrap().split('\t').count(), 2);
    }

    #[test]
    fn numbered_multitaps_map_to_slots_one_through_nine() {
        let settings = PasteTransformSettings::default();
        assert!(settings.multi_slot_enabled);
        assert_eq!(primary_slot_for_taps(1, &settings), 1);
        assert_eq!(primary_slot_for_taps(2, &settings), 2);
        assert_eq!(primary_slot_for_taps(9, &settings), 9);
        assert_eq!(primary_slot_for_taps(10, &settings), 9);
    }

    #[test]
    fn disabled_numbered_multitaps_fall_back_to_normal_slot_one() {
        let mut settings = PasteTransformSettings::default();
        settings.multi_slot_enabled = false;
        assert_eq!(primary_slot_for_taps(1, &settings), 1);
        assert_eq!(primary_slot_for_taps(7, &settings), 1);
    }

    #[test]
    fn extended_multitaps_map_to_slots_eleven_through_thirty() {
        assert_eq!(upper_slot_for_taps(1), 11);
        assert_eq!(upper_slot_for_taps(20), 30);
        assert_eq!(upper_slot_for_taps(40), 30);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn alphabet_maps_a_through_z_to_slots_thirty_one_through_fifty_six() {
        assert_eq!(key_to_letter_slot(RKey::KeyA), Some((31, 0, 'A')));
        assert_eq!(key_to_letter_slot(RKey::KeyM), Some((43, 12, 'M')));
        assert_eq!(key_to_letter_slot(RKey::KeyZ), Some((56, 25, 'Z')));
        assert_eq!(key_to_letter_slot(RKey::Num1), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_slot_strip_stays_inside_the_bank() {
        // Five chips centred on the live slot, clamped so the window never
        // runs past either end of 1..9 — a strip that starts at 0 or ends at
        // 11 is showing slots that do not exist.
        assert_eq!(slot_strip(7), Some((5, 9)));
        assert_eq!(slot_strip(5), Some((3, 7)));
        // Near the ends the window stops moving rather than hanging off.
        assert_eq!(slot_strip(1), Some((1, 5)));
        assert_eq!(slot_strip(2), Some((1, 5)));
        assert_eq!(slot_strip(9), Some((5, 9)));
        assert_eq!(slot_strip(8), Some((5, 9)));
        // Every window is five wide and contains the slot it is drawn for.
        for slot in 1..=9u8 {
            let (first, last) = slot_strip(slot).expect("numeric slots have a strip");
            assert_eq!(last - first, 4, "slot {slot} strip is not five wide");
            assert!((first..=last).contains(&slot), "slot {slot} outside its own strip");
            assert!(first >= 1 && last <= 9, "slot {slot} strip leaves the bank");
        }
        // The extended and letter banks get the plain toast: a five-chip
        // window onto twenty-six slots would say nothing.
        assert_eq!(slot_strip(15), None);
        assert_eq!(slot_strip(31), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_slot_foot_quotes_the_window_the_daemon_uses() {
        // The caption is built from TAP_WINDOW rather than written out, so it
        // cannot end up advertising a timing that was changed here.
        let secs = format!("{:.2}s", TAP_WINDOW.as_secs_f32());
        let foot = slot_foot("Copied", 7);
        assert!(foot.contains(&secs), "foot must quote TAP_WINDOW: {foot}");
        assert!(foot.starts_with("⌘C ×7"), "copy names the copy chord: {foot}");
        // Paste is the other direction and must not tell you to press ⌘C.
        let paste = slot_foot("Pasted", 3);
        assert!(paste.starts_with("⌘V ×3"), "paste names the paste chord: {paste}");
    }
}
