//! Clipboard enrichment: link previews, auto-translation, image intelligence,
//! and paste prediction.
//!
//! Each enricher runs on a background thread after a clip is saved, so the
//! clipboard watcher is never blocked. Results are written back to the store
//! as metadata on the clip.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::models::ClipEntry;
use crate::store::ClipStore;
use crate::transform::{load_transform_config, TransformConfig};

/// Settings that control which enrichers run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentConfig {
    /// Fetch page titles for URL clips.
    #[serde(default = "default_true")]
    pub link_preview: bool,
    /// Auto-translate non-English text clips to English.
    #[serde(default = "default_false")]
    pub auto_translate: bool,
    /// Extract text from images via OCR (already partially implemented — this
    /// gates the AI-assisted tagging that runs on top of OCR).
    #[serde(default = "default_true")]
    pub image_ocr: bool,
    /// Generate AI tags for clips (text + images).
    #[serde(default = "default_false")]
    pub auto_tag: bool,
    /// Predict what the user will paste next based on context.
    #[serde(default = "default_false")]
    pub paste_prediction: bool,
}

fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            link_preview: true,
            auto_translate: false,
            image_ocr: true,
            auto_tag: false,
            paste_prediction: false,
        }
    }
}

/// Metadata produced by enrichment, stored alongside the clip.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipMetadata {
    /// Fetched page title for URL clips.
    pub url_title: Option<String>,
    /// Fetched page description / og:description.
    pub url_description: Option<String>,
    /// Fetched favicon URL.
    pub url_favicon: Option<String>,
    /// English translation if the clip was non-English.
    pub translation: Option<String>,
    /// Detected language code (e.g. "ja", "es", "en").
    pub detected_language: Option<String>,
    /// AI-generated tags (e.g. "code", "url", "email", "json").
    pub tags: Vec<String>,
    /// AI-generated one-line summary.
    pub summary: Option<String>,
}

impl ClipMetadata {
    pub fn is_empty(&self) -> bool {
        self.url_title.is_none()
            && self.url_description.is_none()
            && self.url_favicon.is_none()
            && self.translation.is_none()
            && self.detected_language.is_none()
            && self.tags.is_empty()
            && self.summary.is_none()
    }
}

/// Path to the enrichment config file.
fn config_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join("enrichment.json")
}

pub fn load_enrichment_config() -> EnrichmentConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_enrichment_config(config: &EnrichmentConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(path, json);
    }
}

// ── Link Preview ──

/// Fetch the title and description of a URL.
///
/// Uses a simple HTTP GET with a timeout, parses the HTML for `<title>` and
/// `<meta name="description">`. No JavaScript rendering — fast and dependency-free.
pub fn fetch_link_preview(url: &str) -> Option<(String, Option<String>)> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }

    // Use curl for a lightweight fetch with a timeout. This avoids pulling in
    // a full HTTP client dependency — curl is present on every macOS.
    let output = std::process::Command::new("curl")
        .args([
            "-sL",
            "--max-time",
            "5",
            "--connect-timeout",
            "3",
            "-A",
            "clipd/0.4 (clipboard enrichment)",
            url,
        ])
        .output()
        .ok()?;

    let html = String::from_utf8_lossy(&output.stdout);
    if html.is_empty() {
        return None;
    }

    let title = extract_html_title(&html);
    let description = extract_meta_description(&html);

    title.map(|t| (t, description))
}

/// Extract `<title>...</title>` from HTML.
fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let content_start = lower[start..].find('>')? + start + 1;
    let end = lower[content_start..].find("</title>")? + content_start;
    let title = html[content_start..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(html_entities_decode(title))
    }
}

/// Extract `<meta name="description" content="...">` from HTML.
fn extract_meta_description(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let meta_start = lower.find("name=\"description\"")?;
    // Search forward for content="..." from the meta tag.
    let search_region = &lower[meta_start..];
    let content_start = search_region.find("content=\"")? + 9;
    let content_end = search_region[content_start..].find('"')?;
    let desc = html[meta_start + content_start..meta_start + content_start + content_end].trim();
    if desc.is_empty() {
        None
    } else {
        Some(html_entities_decode(desc))
    }
}

/// Minimal HTML entity decoder for the common entities.
fn html_entities_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Check if a string looks like a URL.
pub fn looks_like_url(text: &str) -> bool {
    let t = text.trim();
    if t.len() < 8 || t.contains(' ') || t.contains('\n') {
        return false;
    }
    t.starts_with("http://") || t.starts_with("https://") || {
        // Bare domain like "github.com/foo/bar"
        let parts: Vec<&str> = t.split('/').collect();
        parts.len() >= 2
            && parts[0].contains('.')
            && !parts[0].contains(' ')
            && parts[0].chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-')
    }
}

/// Normalize a bare domain to a full URL.
pub fn normalize_url(text: &str) -> String {
    let t = text.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("https://{t}")
    }
}

// ── Language Detection + Translation ──

/// Detect if text is likely non-English using a simple heuristic:
/// count non-ASCII characters and common non-English letter patterns.
/// This is fast and doesn't require an API call.
pub fn detect_language(text: &str) -> &'static str {
    let sample: String = text.chars().take(500).collect();
    let total = sample.chars().count().max(1);
    let non_ascii = sample.chars().filter(|c| !c.is_ascii()).count();
    let non_ascii_ratio = non_ascii as f32 / total as f32;

    if non_ascii_ratio < 0.05 {
        return "en";
    }

    // CJK characters (Chinese, Japanese, Korean)
    let cjk = sample
        .chars()
        .filter(|c| {
            let cp = *c as u32;
            (0x4E00..=0x9FFF).contains(&cp) // CJK Unified
            || (0x3040..=0x30FF).contains(&cp) // Hiragana + Katakana
            || (0xAC00..=0xD7AF).contains(&cp) // Hangul
        })
        .count();

    if cjk > 0 {
        // Distinguish Japanese from Chinese by presence of kana.
        let kana = sample
            .chars()
            .filter(|c| {
                let cp = *c as u32;
                (0x3040..=0x30FF).contains(&cp)
            })
            .count();
        if kana > 0 {
            return "ja";
        }
        // Check for Hangul (Korean).
        let hangul = sample
            .chars()
            .filter(|c| (0xAC00..=0xD7AF).contains(&(*c as u32)))
            .count();
        if hangul > cjk / 2 {
            return "ko";
        }
        return "zh";
    }

    // Cyrillic (Russian, etc.)
    let cyrillic = sample
        .chars()
        .filter(|c| (0x0400..=0x04FF).contains(&(*c as u32)))
        .count();
    if cyrillic > total / 10 {
        return "ru";
    }

    // Arabic
    let arabic = sample
        .chars()
        .filter(|c| (0x0600..=0x06FF).contains(&(*c as u32)))
        .count();
    if arabic > total / 10 {
        return "ar";
    }

    // Default: likely a Latin-script language other than English.
    // Check for common accented characters in European languages.
    if non_ascii_ratio > 0.1 {
        return "other";
    }

    "en"
}

/// Translate text to English using the configured AI API.
/// Returns the translated text, or None if translation failed or wasn't needed.
pub fn translate_to_english(text: &str, config: &TransformConfig) -> Option<String> {
    let lang = detect_language(text);
    if lang == "en" {
        return None;
    }

    let api_key = config.api_key.as_deref()?.trim();
    if api_key.is_empty() {
        return None;
    }

    let prompt = format!(
        "Translate the following text to English. Output ONLY the translation, nothing else.\n\nText: {}",
        text.chars().take(2000).collect::<String>()
    );

    let kind = crate::transform::TransformKind::CustomPrompt(prompt);
    crate::transform::apply_transform(&kind, text, config).ok()
}

// ── Image Intelligence ──

/// Generate AI tags for an image based on its OCR text (if available)
/// and basic heuristics about the content.
pub fn tag_image(ocr_text: Option<&str>, width: u32, height: u32) -> Vec<String> {
    let mut tags = Vec::new();

    // Aspect ratio based tags.
    let ratio = width as f32 / height.max(1) as f32;
    if ratio > 1.8 {
        tags.push("screenshot".to_string());
    } else if ratio < 0.6 {
        tags.push("portrait".to_string());
    } else if (0.9..=1.1).contains(&ratio) {
        tags.push("square".to_string());
    }

    // OCR-based tags.
    if let Some(ocr) = ocr_text {
        let ocr_lower = ocr.to_ascii_lowercase();
        if ocr_lower.contains("error") || ocr_lower.contains("exception") {
            tags.push("error".to_string());
        }
        if ocr_lower.contains("function") || ocr_lower.contains("class") || ocr_lower.contains("def ") {
            tags.push("code".to_string());
        }
        if ocr_lower.contains("http://") || ocr_lower.contains("https://") {
            tags.push("url".to_string());
        }
        if ocr_lower.contains('@') && ocr_lower.contains('.') {
            tags.push("email".to_string());
        }
        if ocr_lower.contains("password") || ocr_lower.contains("token") || ocr_lower.contains("secret") {
            tags.push("sensitive".to_string());
        }
        if ocr_lower.contains("$") || ocr_lower.contains("total") || ocr_lower.contains("invoice") {
            tags.push("receipt".to_string());
        }
        if ocr_lower.contains("login") || ocr_lower.contains("sign in") || ocr_lower.contains("sign up") {
            tags.push("login".to_string());
        }
        if ocr_lower.contains("chat") || ocr_lower.contains("message") || ocr_lower.contains("reply") {
            tags.push("chat".to_string());
        }
    }

    tags
}

/// Generate AI tags for a text clip based on content analysis.
pub fn tag_text(text: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let lower = text.to_ascii_lowercase();

    if looks_like_url(text.trim()) {
        tags.push("url".to_string());
    }
    if lower.contains("```") || lower.contains("fn ") || lower.contains("function ")
        || lower.contains("class ") || lower.contains("def ")
        || lower.contains("import ") || lower.contains("const ")
        || lower.contains("public ") || lower.contains("private ")
    {
        tags.push("code".to_string());
    }
    if lower.trim_start().starts_with('{') || lower.trim_start().starts_with('[') {
        tags.push("json".to_string());
    }
    if lower.contains("<html") || lower.contains("<div") || lower.contains("<p>") {
        tags.push("html".to_string());
    }
    if lower.contains('@') && lower.contains('.') && !lower.contains(' ') {
        tags.push("email".to_string());
    }
    if lower.contains("password") || lower.contains("token") || lower.contains("secret") {
        tags.push("sensitive".to_string());
    }
    if text.lines().count() > 10 {
        tags.push("long".to_string());
    }
    if text.trim().len() < 20 {
        tags.push("short".to_string());
    }

    tags
}

/// Generate a one-line AI summary of a clip.
pub fn summarize_clip(text: &str, config: &TransformConfig) -> Option<String> {
    let api_key = config.api_key.as_deref()?.trim();
    if api_key.is_empty() {
        return None;
    }

    let prompt = format!(
        "Summarize the following in one short sentence (max 80 chars):\n\n{}",
        text.chars().take(2000).collect::<String>()
    );

    let kind = crate::transform::TransformKind::CustomPrompt(prompt);
    crate::transform::apply_transform(&kind, text, config).ok()
}

// ── Paste Prediction ──

/// Paste prediction context: what app the user is in and what's on their clipboard.
#[derive(Debug, Clone)]
pub struct PasteContext {
    pub active_app: String,
    pub recent_clip_ids: Vec<i64>,
    pub recent_clip_types: Vec<String>,
}

/// Predict what the user might paste next, based on context.
/// Returns a list of clip IDs that are likely candidates, ranked by relevance.
pub fn predict_next_paste(
    store: &ClipStore,
    context: &PasteContext,
    limit: usize,
) -> Vec<i64> {
    let recent = store.get_recent(50).unwrap_or_default();
    if recent.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(i64, f32)> = Vec::new();

    for clip in &recent {
        let mut score: f32 = 0.0;

        // Recent clips are more likely to be pasted again.
        let age_minutes = (chrono::Utc::now() - clip.timestamp).num_minutes() as f32;
        score += (60.0 - age_minutes.min(60.0)) / 60.0 * 0.3;

        // Clips from the same app are more relevant.
        if let Some(ref app) = clip.source_app {
            if *app == context.active_app {
                score += 0.2;
            }
        }

        // URL clips are often re-pasted.
        if clip.content_type == crate::models::ContentType::Text
            && looks_like_url(&clip.content)
        {
            score += 0.1;
        }

        // Code clips are often re-pasted.
        let lower = clip.content.to_ascii_lowercase();
        if lower.contains("fn ") || lower.contains("function ") || lower.contains("class ") {
            score += 0.1;
        }

        // Penalize very short clips (usually not worth re-pasting).
        if clip.content.trim().len() < 5 {
            score -= 0.2;
        }

        // Penalize clips already in recent history (avoid suggesting what
        // the user just pasted).
        if context.recent_clip_ids.contains(&clip.id) {
            score -= 0.3;
        }

        scored.push((clip.id, score));
    }

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(limit)
        .map(|(id, _)| id)
        .collect()
}

// ── Orchestrator ──

/// Enrich a single clip. Runs all enabled enrichers and returns the metadata.
/// This is the function called from the background thread.
pub fn enrich_clip(
    clip: &ClipEntry,
    config: &EnrichmentConfig,
    api_config: &TransformConfig,
) -> ClipMetadata {
    let mut meta = ClipMetadata::default();

    // Link preview for URL clips.
    if config.link_preview {
        let text = clip.content.trim();
        if looks_like_url(text) {
            let url = normalize_url(text);
            if let Some((title, desc)) = fetch_link_preview(&url) {
                meta.url_title = Some(title);
                meta.url_description = desc;
            }
        }
    }

    // Auto-translate non-English text.
    if config.auto_translate && clip.content_type == crate::models::ContentType::Text {
        let lang = detect_language(&clip.content);
        if lang != "en" {
            meta.detected_language = Some(lang.to_string());
            if let Some(translation) = translate_to_english(&clip.content, api_config) {
                meta.translation = Some(translation);
            }
        }
    }

    // Auto-tagging.
    if config.auto_tag {
        match clip.content_type {
            crate::models::ContentType::Text => {
                meta.tags = tag_text(&clip.content);
            }
            crate::models::ContentType::Image => {
                meta.tags = tag_image(
                    clip.ocr_text.as_deref(),
                    0, // width/height not available here without loading the image
                    0,
                );
            }
            _ => {}
        }
    }

    // Summary (only for longer clips).
    if config.auto_tag && clip.content.len() > 200 {
        if let Some(summary) = summarize_clip(&clip.content, api_config) {
            meta.summary = Some(summary);
        }
    }

    meta
}

/// Spawn a background thread to enrich a clip and update the store.
pub fn spawn_enrichment(clip_id: i64, clip_content: String, clip_type: crate::models::ContentType) {
    std::thread::spawn(move || {
        let config = load_enrichment_config();
        if config.is_empty() {
            return;
        }

        let api_config = load_transform_config();

        // Reconstruct a minimal ClipEntry for enrichment.
        let clip = ClipEntry {
            id: clip_id,
            content: clip_content.clone(),
            content_type: clip_type,
            content_hash: String::new(),
            source_app: None,
            source_title: None,
            timestamp: chrono::Utc::now(),
            preview: String::new(),
            slot: None,
            image_path: None,
            thumb_path: None,
            ocr_text: None,
            files: Vec::new(),
        };

        let meta = enrich_clip(&clip, &config, &api_config);
        if meta.is_empty() {
            return;
        }

        // Write metadata to the clip's sidecar file.
        let meta_path = enrichment_path(clip_id);
        if let Ok(json) = serde_json::to_string_pretty(&meta) {
            let _ = std::fs::write(meta_path, json);
        }
    });
}

/// Path to a clip's enrichment metadata sidecar file.
fn enrichment_path(clip_id: i64) -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clipd")
        .join("enrichment")
        .join(format!("{clip_id}.json"))
}

/// Load enrichment metadata for a clip, if it exists.
pub fn load_clip_metadata(clip_id: i64) -> Option<ClipMetadata> {
    let path = enrichment_path(clip_id);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

impl EnrichmentConfig {
    pub fn is_empty(&self) -> bool {
        !self.link_preview && !self.auto_translate && !self.image_ocr && !self.auto_tag && !self.paste_prediction
    }
}