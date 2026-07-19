use crate::models::ClipEntry;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub clip_ids: Vec<i64>,
    pub top_apps: Vec<String>,
}

impl Session {
    pub fn clip_count(&self) -> usize {
        self.clip_ids.len()
    }

    pub fn duration_mins(&self) -> i64 {
        self.ended_at
            .signed_duration_since(self.started_at)
            .num_minutes()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_window")]
    pub window_minutes: i64,
    #[serde(default = "default_true")]
    pub auto_name: bool,
}

fn default_window() -> i64 {
    30
}
fn default_true() -> bool {
    true
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            window_minutes: 30,
            auto_name: true,
        }
    }
}

/// Group clips into sessions based on time gaps.
/// Clips should be sorted newest-first (as returned by `get_recent`).
pub fn compute_sessions(clips: &[ClipEntry], window_minutes: i64) -> Vec<Session> {
    if clips.is_empty() {
        return Vec::new();
    }

    let mut sessions: Vec<Session> = Vec::new();
    let mut current_ids: Vec<i64> = Vec::new();
    let mut current_apps: Vec<String> = Vec::new();
    let mut session_start: DateTime<Utc> = clips[0].timestamp;
    let mut session_end: DateTime<Utc> = clips[0].timestamp;
    let mut prev_time = clips[0].timestamp;

    for clip in clips {
        let gap = prev_time
            .signed_duration_since(clip.timestamp)
            .num_minutes();

        if gap > window_minutes && !current_ids.is_empty() {
            sessions.push(build_session(
                &current_ids,
                &current_apps,
                session_start,
                session_end,
            ));
            current_ids.clear();
            current_apps.clear();
            session_start = clip.timestamp;
        }

        current_ids.push(clip.id);
        if let Some(ref app) = clip.source_app {
            if !current_apps.contains(app) {
                current_apps.push(app.clone());
            }
        }
        session_end = clip.timestamp;
        prev_time = clip.timestamp;
    }

    if !current_ids.is_empty() {
        sessions.push(build_session(
            &current_ids,
            &current_apps,
            session_start,
            session_end,
        ));
    }

    sessions
}

fn build_session(
    ids: &[i64],
    apps: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Session {
    let name = auto_name_from_apps(apps, ids.len(), start);
    Session {
        name,
        started_at: start,
        ended_at: end,
        clip_ids: ids.to_vec(),
        top_apps: apps.to_vec(),
    }
}

fn auto_name_from_apps(apps: &[String], count: usize, start: DateTime<Utc>) -> String {
    let time_str = start.format("%b %d, %H:%M").to_string();
    let clip_word = if count == 1 { "clip" } else { "clips" };

    if apps.is_empty() {
        return format!("{} ({} {})", time_str, count, clip_word);
    }

    let app_str = if apps.len() <= 2 {
        apps.join(" + ")
    } else {
        format!("{} + {} more", apps[0], apps.len() - 1)
    };

    format!("{} — {} ({} {})", time_str, app_str, count, clip_word)
}

/// Generate a richer session name using AI (requires TransformConfig with API key).
pub fn ai_name_session(
    clips: &[ClipEntry],
    config: &crate::TransformConfig,
) -> Result<String, String> {
    let previews: Vec<&str> = clips.iter().take(10).map(|c| c.preview.as_str()).collect();
    let combined = previews.join("\n");

    let kind = crate::TransformKind::CustomPrompt(
        "Generate a short (3-6 word) descriptive session name for this group of clipboard items. \
         Return only the name, no quotes or punctuation."
            .into(),
    );

    crate::apply_transform(&kind, &combined, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ClipEntry;
    use chrono::Duration;

    fn make_clip(id: i64, mins_ago: i64, app: &str) -> ClipEntry {
        ClipEntry {
            id,
            content: format!("clip {}", id),
            content_type: crate::models::ContentType::Text,
            content_hash: format!("hash{}", id),
            source_app: Some(app.into()),
            source_title: None,
            timestamp: Utc::now() - Duration::minutes(mins_ago),
            preview: format!("clip {}", id),
            slot: None,
            image_path: None,
            thumb_path: None,
            ocr_text: None,
        }
    }

    #[test]
    fn test_single_session() {
        let clips = vec![
            make_clip(1, 0, "Chrome"),
            make_clip(2, 5, "Chrome"),
            make_clip(3, 10, "VS Code"),
        ];
        let sessions = compute_sessions(&clips, 30);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].clip_count(), 3);
    }

    #[test]
    fn test_multiple_sessions() {
        let clips = vec![
            make_clip(1, 0, "Chrome"),
            make_clip(2, 5, "Chrome"),
            make_clip(3, 60, "VS Code"),
            make_clip(4, 65, "VS Code"),
        ];
        let sessions = compute_sessions(&clips, 30);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].clip_count(), 2);
        assert_eq!(sessions[1].clip_count(), 2);
    }

    #[test]
    fn test_empty_clips() {
        let sessions = compute_sessions(&[], 30);
        assert!(sessions.is_empty());
    }
}
