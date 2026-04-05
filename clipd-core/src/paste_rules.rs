use crate::transform::TransformKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteRule {
    pub dest_app: String,
    pub transform: TransformKind,
    pub auto_apply: bool,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteRulesConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_rules")]
    pub rules: Vec<PasteRule>,
}

fn default_true() -> bool {
    true
}

fn default_rules() -> Vec<PasteRule> {
    vec![
        PasteRule {
            dest_app: "Slack".into(),
            transform: TransformKind::StripHtml,
            auto_apply: true,
            description: "Strip rich formatting for Slack".into(),
        },
        PasteRule {
            dest_app: "Terminal".into(),
            transform: TransformKind::TrimWhitespace,
            auto_apply: true,
            description: "Clean whitespace for terminal".into(),
        },
        PasteRule {
            dest_app: "iTerm2".into(),
            transform: TransformKind::TrimWhitespace,
            auto_apply: true,
            description: "Clean whitespace for iTerm".into(),
        },
        PasteRule {
            dest_app: "Warp".into(),
            transform: TransformKind::TrimWhitespace,
            auto_apply: true,
            description: "Clean whitespace for Warp".into(),
        },
        PasteRule {
            dest_app: "Numbers".into(),
            transform: TransformKind::TrimWhitespace,
            auto_apply: false,
            description: "Clean data for spreadsheet".into(),
        },
        PasteRule {
            dest_app: "Microsoft Excel".into(),
            transform: TransformKind::TrimWhitespace,
            auto_apply: false,
            description: "Clean data for Excel".into(),
        },
        PasteRule {
            dest_app: "Notes".into(),
            transform: TransformKind::StripHtml,
            auto_apply: false,
            description: "Strip HTML for Notes".into(),
        },
    ]
}

impl Default for PasteRulesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rules: default_rules(),
        }
    }
}

fn config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("paste_rules.json")
}

pub fn load_paste_rules() -> PasteRulesConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_paste_rules(config: &PasteRulesConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(path, json);
    }
}

/// Find matching rules for the destination app.
/// Returns auto-apply rules first, then manual suggestions.
pub fn find_rules_for_app<'a>(dest_app: &str, config: &'a PasteRulesConfig) -> Vec<&'a PasteRule> {
    if !config.enabled {
        return Vec::new();
    }

    let dest_lower = dest_app.to_lowercase();
    let mut matches: Vec<&PasteRule> = config
        .rules
        .iter()
        .filter(|r| dest_lower.contains(&r.dest_app.to_lowercase()))
        .collect();

    matches.sort_by_key(|r| !r.auto_apply);
    matches
}

/// Suggest a transform based on content type and destination.
pub fn suggest_smart_transform(
    content: &str,
    content_type: &crate::models::ContentType,
    dest_app: Option<&str>,
) -> Vec<TransformKind> {
    let mut suggestions = Vec::new();

    if let Some(app) = dest_app {
        let lower = app.to_lowercase();
        if lower.contains("slack")
            || lower.contains("discord")
            || lower.contains("teams")
        {
            if content.contains('<') && content.contains('>') {
                suggestions.push(TransformKind::StripHtml);
            }
        }

        if lower.contains("terminal")
            || lower.contains("iterm")
            || lower.contains("warp")
            || lower.contains("kitty")
            || lower.contains("alacritty")
        {
            suggestions.push(TransformKind::TrimWhitespace);
        }
    }

    match content_type {
        crate::models::ContentType::Code => {
            if let Ok(_) = serde_json::from_str::<serde_json::Value>(content) {
                suggestions.push(TransformKind::PrettyJson);
            }
        }
        _ => {
            if content.trim_start().starts_with('{') || content.trim_start().starts_with('[') {
                if serde_json::from_str::<serde_json::Value>(content).is_ok() {
                    suggestions.push(TransformKind::PrettyJson);
                }
            }
            if content.contains("<html") || content.contains("<div") || content.contains("<p>") {
                suggestions.push(TransformKind::HtmlToMarkdown);
            }
        }
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_rules() {
        let config = PasteRulesConfig::default();
        let rules = find_rules_for_app("Slack", &config);
        assert!(!rules.is_empty());
        assert_eq!(rules[0].dest_app, "Slack");
    }

    #[test]
    fn test_no_rules() {
        let config = PasteRulesConfig::default();
        let rules = find_rules_for_app("Unknown App", &config);
        assert!(rules.is_empty());
    }

    #[test]
    fn test_smart_suggest_json() {
        let suggestions = suggest_smart_transform(
            "{\"key\": \"value\"}",
            &crate::models::ContentType::Code,
            None,
        );
        assert!(suggestions.contains(&TransformKind::PrettyJson));
    }

    #[test]
    fn test_smart_suggest_html() {
        let suggestions = suggest_smart_transform(
            "<div><p>Hello</p></div>",
            &crate::models::ContentType::Text,
            None,
        );
        assert!(suggestions.contains(&TransformKind::HtmlToMarkdown));
    }
}
