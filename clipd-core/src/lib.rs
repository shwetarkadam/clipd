pub mod actions;
pub mod ask;
pub mod ask_eval;
pub mod enrichment;
#[cfg(target_os = "macos")]
pub mod macos_permissions;
pub mod collections;
pub mod devices;
pub mod embedding;
pub mod files;
pub mod images;
pub mod island;
pub mod lan;
pub mod lan_discovery;
pub mod lan_identity;
pub mod lan_pair;
pub mod lock;
pub mod models;
pub mod paste_rules;
pub mod pasteboard;
pub mod privacy;
pub mod secret_clipboard;
pub mod semantic;
pub mod session;
pub mod slots;
pub mod snippets;
pub mod store;
pub mod sync;
pub mod suggest;
pub mod telemetry;
pub mod theme;
pub mod transform;
pub mod tray;
pub mod vault;
pub mod watcher;

pub use ask::{
    ask, can_synthesize, estimate_tokens, has_api_key, retrieve, AskAnswer, AskConfig, AskFilters,
    AskSource, AskThread, AskTurn, Confidence, RetrievedClip, Retriever,
};
pub use enrichment::{
    detect_language, enrich_clip, fetch_link_preview, load_clip_metadata, load_enrichment_config,
    save_enrichment_config, spawn_enrichment, tag_image, tag_text, translate_to_english,
    ClipMetadata, EnrichmentConfig, PasteContext, PasteContext as PastePredictionContext,
    predict_next_paste,
};
pub use ask_eval::{
    golden_cases, hit_rate_at_k, mean_reciprocal_rank, ndcg_at_k, run_retrieval_eval, seed_eval_store,
    EvalReport, GoldenCase,
};
pub use collections::{
    make_template, refine_prompt, summarize_collection, Collection, CollectionItem,
};
pub use devices::{
    all_devices, device_id, device_name, inbox_dir, peers, register as register_device,
    resolve_peer, sync_root, this_device, Device,
};
pub use embedding::{
    cosine_similarity as embedding_cosine, generate_embedding, generate_embeddings_batch,
    is_embedding_available, search_embeddings, Embedding, EmbeddingResult,
};
pub use actions::{
    ask_action, load_actions, run_action, save_actions, ActionOutput, ActionsConfig, CustomAction,
};
pub use files::{
    delete_file_blobs, files_dir, format_size, hash_file_set, save_file, save_files, FileRef,
    MAX_BLOB_BYTES,
};
pub use images::{images_dir, load_rgba, save_rgba_image, SavedImage, decode_rgba};
pub use island::{
    gui_window_open, refresh_gui_window_claim, set_gui_window_open, slot_badge,
    ISLAND_RESERVED_TOP,
    island_layout_active, load_island_config, load_shelf, save_island_config, save_shelf,
    ClipCounts, IslandAnchor, IslandConfig, IslandModule, IslandSnapshot, ShelfItem,
};
pub use lock::{daemon_lock_pid, surface_is_running, 
    is_daemon_running, load_hotkey_status, release_daemon_lock, save_hotkey_status,
    try_acquire_daemon_lock, HotkeyStatus, ProcessLock,
};
#[cfg(target_os = "macos")]
pub use macos_permissions::{
    accessibility_granted, input_monitoring_granted, keyboard_permissions_granted,
    missing_keyboard_permission_label, open_keyboard_permission_settings,
    request_keyboard_permissions,
};
pub use models::{ClipEntry, ContentType, SearchFilters};
pub use paste_rules::{
    find_rules_for_app, load_paste_rules, save_paste_rules, suggest_smart_transform, PasteRule,
    PasteRulesConfig,
};
pub use pasteboard::{
    read_file_urls as clipboard_read_file_urls, read_text as clipboard_read_text,
    write_file_urls as clipboard_write_file_urls, write_text as clipboard_write_text,
};
pub use privacy::{
    redacted_display,
    detect_sensitive, is_excluded_app, load_privacy_config, looks_like_password,
    save_privacy_config, should_skip_clip, PrivacyConfig, SensitiveKind, SensitiveMatch,
};
pub use secret_clipboard::{
    clipboard_is_concealed, copy_secret, copy_secret_blocking, CONCEALED_TYPE, DEFAULT_CLEAR_AFTER,
};
pub use semantic::{SemanticResult, TfIdfIndex};
pub use session::{compute_sessions, Session, SessionConfig};
pub use slots::{SlotManager, MAX_CLIP_SLOT};
pub use snippets::Snippet;
pub use store::ClipStore;
pub use sync::{
    clip_from_envelope, deliver, encode as encode_envelope, envelope_from_clip, pending,
    Envelope, InlineFile, Payload, ENVELOPE_EXT, MAX_ENVELOPE_BYTES,
};
pub use suggest::{suggest_for, Suggestion, SuggestionKind, VISIBLE_SUGGESTIONS};
pub use theme::{
    load_custom_colors, load_theme, save_custom_colors, save_theme, CustomColors, Rgb, Theme,
    ThemeColors,
};
pub use transform::{
    all_transforms, apply_transform, load_last_active_app, load_paste_transform_settings,
    load_transform_config, paste_transforms, save_last_active_app, save_paste_transform_settings,
    save_transform_config, transform_config_path, CtrlSpaceAction, OpenGuiHotkey, PaletteTrigger,
    PasteTransformSettings, SlotInputMode, TransformConfig, TransformKind,
    GuiLayout,
};
pub use tray::{load_tray_anchor, save_tray_anchor};
pub use vault::{
    available_targets, forget_secret, list_secrets, rename_secret, reveal_secret, save_secret,
    SecretEntry, SecretRef, VaultTarget,
};
pub use watcher::{ClipEvent, ClipWatcher};

/// Fire the anonymous telemetry ping (noop if telemetry is disabled or no endpoint is configured).
pub use telemetry::{
    event as telemetry_event, ping, set_telemetry_enabled, telemetry_configured,
    telemetry_enabled,
};
