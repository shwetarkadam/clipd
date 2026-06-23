//! Collections — named, persistent buckets of clips the user curates
//! (e.g. "Cursor prompts"). Slice 1 is the local data model (see `store.rs`
//! for the CRUD); Slice 2 adds AI actions that require an API key.

use crate::transform::TransformConfig;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A named collection. `source_app`, when set, auto-routes any clip copied
/// while that app is frontmost into this collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub source_app: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Number of items (populated by list/get; 0 when not computed).
    #[serde(default)]
    pub item_count: usize,
}

/// One clip inside a collection, with its content joined in for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionItem {
    pub clip_id: i64,
    pub content: String,
    pub preview: String,
    pub position: i64,
    pub added_at: DateTime<Utc>,
}

// ── Slice 2: AI actions (require an API key in transform.json) ──

/// Minimal OpenAI-compatible chat call. Returns the assistant's text.
/// Errors clearly when no API key is configured.
fn chat_complete(system: &str, user: &str, config: &TransformConfig) -> Result<String, String> {
    let api_key = config
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or("AI actions require an API key — set it in transform.json")?;

    let body = serde_json::json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": 2048,
        "temperature": 0.3,
    });

    let response = ureq::post(&config.api_url)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", api_key))
        .send_json(body)
        .map_err(|e| format!("API request failed: {}", e))?;

    let resp: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse API response: {}", e))?;

    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| {
            resp["error"]["message"]
                .as_str()
                .map(|e| format!("API error: {}", e))
                .unwrap_or_else(|| "Unexpected API response format".to_string())
        })
}

/// Rewrite a saved prompt to be clearer and more effective. Never applied
/// silently — callers preview the result before replacing anything.
pub fn refine_prompt(prompt: &str, config: &TransformConfig) -> Result<String, String> {
    chat_complete(
        "You are a prompt engineer. Rewrite the user's prompt to be clearer, more \
         specific, and more effective for an AI coding assistant, preserving their \
         intent. Return only the improved prompt — no commentary, no markdown fences.",
        prompt,
        config,
    )
}

/// Turn a prompt into a reusable template, replacing changeable parts with
/// {placeholder} variables the user fills in at paste time.
pub fn make_template(prompt: &str, config: &TransformConfig) -> Result<String, String> {
    chat_complete(
        "Turn the user's prompt into a reusable template by replacing the specific, \
         changeable parts with {placeholder} variables (descriptive names). Keep the \
         structure. Return only the template.",
        prompt,
        config,
    )
}

/// Summarize what a collection of saved prompts/snippets is about.
pub fn summarize_collection(
    items: &[CollectionItem],
    config: &TransformConfig,
) -> Result<String, String> {
    if items.is_empty() {
        return Ok("Collection is empty.".to_string());
    }
    let joined = items
        .iter()
        .enumerate()
        .map(|(i, it)| format!("{}. {}", i + 1, it.preview))
        .collect::<Vec<_>>()
        .join("\n");
    chat_complete(
        "These are saved prompts/snippets from one collection. Summarize what the \
         collection is about in 2-3 sentences and note common themes.",
        &joined,
        config,
    )
}
