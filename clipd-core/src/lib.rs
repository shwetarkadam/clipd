pub mod collections;
pub mod embedding;
pub mod lock;
pub mod models;
pub mod paste_rules;
pub mod privacy;
pub mod semantic;
pub mod session;
pub mod slots;
pub mod store;
pub mod telemetry;
pub mod theme;
pub mod transform;
pub mod watcher;

pub use collections::{
    make_template, refine_prompt, summarize_collection, Collection, CollectionItem,
};
pub use embedding::{
    cosine_similarity as embedding_cosine, generate_embedding, generate_embeddings_batch,
    is_embedding_available, search_embeddings, Embedding, EmbeddingResult,
};
pub use lock::{is_daemon_running, release_daemon_lock, try_acquire_daemon_lock};
pub use models::{ClipEntry, ContentType, SearchFilters};
pub use paste_rules::{
    find_rules_for_app, load_paste_rules, save_paste_rules, suggest_smart_transform, PasteRule,
    PasteRulesConfig,
};
pub use privacy::{
    detect_sensitive, is_excluded_app, load_privacy_config, save_privacy_config, should_skip_clip,
    PrivacyConfig, SensitiveKind, SensitiveMatch,
};
pub use semantic::{SemanticResult, TfIdfIndex};
pub use session::{compute_sessions, Session, SessionConfig};
pub use slots::{SlotManager, MAX_CLIP_SLOT};
pub use store::ClipStore;
pub use theme::{load_theme, save_theme, Rgb, Theme, ThemeColors};
pub use transform::{
    all_transforms, apply_transform, load_paste_transform_settings, load_transform_config,
    paste_transforms, save_paste_transform_settings, save_transform_config, PaletteTrigger,
    PasteTransformSettings, SlotInputMode, TransformConfig, TransformKind,
};
pub use watcher::{ClipEvent, ClipWatcher};

/// Fire the anonymous telemetry ping (noop if telemetry is disabled or no endpoint is configured).
pub use telemetry::ping;
