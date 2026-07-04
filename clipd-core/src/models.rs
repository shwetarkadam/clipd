use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The type of content stored in a clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    Text,
    Url,
    Code,
    Email,
    Path,
    Image,
    Unknown,
}

impl ContentType {
    /// Auto-detect content type from the clip text.
    pub fn detect(content: &str) -> Self {
        let trimmed = content.trim();

        // URL detection
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("ftp://")
        {
            return ContentType::Url;
        }

        // Email detection
        if trimmed.contains('@')
            && trimmed.contains('.')
            && !trimmed.contains(' ')
            && trimmed.len() < 256
        {
            return ContentType::Email;
        }

        // File path detection
        if trimmed.starts_with('/')
            || trimmed.starts_with("~/")
            || trimmed.starts_with("./")
            || (trimmed.len() > 2
                && trimmed.chars().nth(1) == Some(':')
                && trimmed.chars().nth(2) == Some('\\'))
        {
            return ContentType::Path;
        }

        // Code detection heuristics
        let code_indicators = [
            "fn ",
            "pub ",
            "let ",
            "const ",
            "impl ",
            "struct ",
            "enum ", // Rust
            "def ",
            "class ",
            "import ",
            "from ", // Python
            "function ",
            "var ",
            "=>",
            "const ",
            "async ", // JS/TS
            "func ",
            "package ",
            "type ", // Go
            "if (",
            "for (",
            "while (",
            "switch (", // C-style
            "{",
            "}",
            "();",
            "->",
            "::", // General
            "#include",
            "#define",
            "#ifdef", // C/C++
        ];

        let line_count = trimmed.lines().count();
        let has_code_indicator = code_indicators.iter().any(|ind| trimmed.contains(ind));
        let has_indentation = trimmed
            .lines()
            .any(|l| l.starts_with("  ") || l.starts_with('\t'));

        if has_code_indicator && (line_count > 1 || has_indentation) {
            return ContentType::Code;
        }

        // If single line with braces/parens/semicolons, likely code
        if line_count == 1
            && (trimmed.contains(';') || trimmed.contains("()") || trimmed.contains("{}"))
        {
            return ContentType::Code;
        }

        ContentType::Text
    }

    pub fn as_str(&self) -> &str {
        match self {
            ContentType::Text => "text",
            ContentType::Url => "url",
            ContentType::Code => "code",
            ContentType::Email => "email",
            ContentType::Path => "path",
            ContentType::Image => "image",
            ContentType::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "text" => ContentType::Text,
            "url" => ContentType::Url,
            "code" => ContentType::Code,
            "email" => ContentType::Email,
            "path" => ContentType::Path,
            "image" => ContentType::Image,
            _ => ContentType::Unknown,
        }
    }

    /// Return a display emoji for TUI/CLI output.
    pub fn icon(&self) -> &str {
        match self {
            ContentType::Text => "📝",
            ContentType::Url => "🔗",
            ContentType::Code => "💻",
            ContentType::Email => "📧",
            ContentType::Path => "📁",
            ContentType::Image => "🖼",
            ContentType::Unknown => "❓",
        }
    }
}

/// A single clipboard entry with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEntry {
    pub id: i64,
    pub content: String,
    pub content_type: ContentType,
    pub content_hash: String,
    pub source_app: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub preview: String,
    /// Slot this clip was saved to via multi-tap hotkey (None = auto-saved via OS copy).
    pub slot: Option<u8>,
    /// For image clips: path to the full-resolution PNG on disk.
    #[serde(default)]
    pub image_path: Option<String>,
    /// For image clips: path to the small thumbnail PNG (for list rendering).
    #[serde(default)]
    pub thumb_path: Option<String>,
    /// For image clips: text recognized via on-device OCR (Apple Vision).
    /// Mirrored into `content` so it's full-text searchable.
    #[serde(default)]
    pub ocr_text: Option<String>,
}

impl ClipEntry {
    /// Create a new clip entry with auto-detected content type.
    pub fn new(content: String, source_app: Option<String>, slot: Option<u8>) -> Self {
        use sha2::{Digest, Sha256};

        let content_type = ContentType::detect(&content);
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let content_hash = format!("{:x}", hasher.finalize());

        let preview = Self::make_preview(&content, 80);

        ClipEntry {
            id: 0, // assigned by DB
            content,
            content_type,
            content_hash,
            source_app,
            timestamp: Utc::now(),
            preview,
            slot,
            image_path: None,
            thumb_path: None,
            ocr_text: None,
        }
    }

    /// Create an image clip. `content` mirrors the OCR text so images are
    /// searchable by what they contain; `content_hash` is the image hash (used
    /// for dedup); `preview` shows a glanceable label + any OCR snippet.
    #[allow(clippy::too_many_arguments)]
    pub fn new_image(
        content_hash: String,
        image_path: String,
        thumb_path: String,
        ocr_text: Option<String>,
        source_app: Option<String>,
        width: u32,
        height: u32,
    ) -> Self {
        let ocr_clean = ocr_text
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let preview = match ocr_clean {
            Some(text) => {
                let snippet = Self::make_preview(text, 64);
                format!("🖼 {}", snippet)
            }
            None => format!("🖼 Image · {}×{}", width, height),
        };
        ClipEntry {
            id: 0,
            content: ocr_clean.map(|s| s.to_string()).unwrap_or_default(),
            content_type: ContentType::Image,
            content_hash,
            source_app,
            timestamp: Utc::now(),
            preview,
            slot: None,
            image_path: Some(image_path),
            thumb_path: Some(thumb_path),
            ocr_text: ocr_clean.map(|s| s.to_string()),
        }
    }

    /// Generate a short preview of the content (single line, truncated).
    fn make_preview(content: &str, max_len: usize) -> String {
        let first_line = content.lines().next().unwrap_or("");
        let cleaned = first_line.trim().replace('\t', " ");
        let char_count: usize = cleaned.chars().count();
        if char_count > max_len {
            let end: String = cleaned.chars().take(max_len).collect();
            format!("{}…", end)
        } else {
            cleaned
        }
    }
}

/// Search filters for querying clip history.
#[derive(Debug, Default)]
pub struct SearchFilters {
    pub query: Option<String>,
    pub content_type: Option<ContentType>,
    pub source_app: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: usize,
}

/// Basic statistics about the clip store.
#[derive(Debug, Serialize)]
pub struct ClipStats {
    pub total_clips: usize,
    pub unique_apps: usize,
    pub db_size_bytes: u64,
    pub oldest_clip: Option<DateTime<Utc>>,
    pub newest_clip: Option<DateTime<Utc>>,
    pub top_apps: Vec<(String, usize)>,
    pub type_counts: Vec<(String, usize)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_url() {
        assert_eq!(
            ContentType::detect("https://github.com/foo"),
            ContentType::Url
        );
        assert_eq!(ContentType::detect("http://example.com"), ContentType::Url);
    }

    #[test]
    fn test_detect_email() {
        assert_eq!(ContentType::detect("user@example.com"), ContentType::Email);
    }

    #[test]
    fn test_detect_code() {
        assert_eq!(
            ContentType::detect("fn main() {\n    println!(\"hello\");\n}"),
            ContentType::Code
        );
        assert_eq!(
            ContentType::detect("def foo():\n    return 42"),
            ContentType::Code
        );
    }

    #[test]
    fn test_detect_path() {
        assert_eq!(
            ContentType::detect("/usr/local/bin/clipd"),
            ContentType::Path
        );
        assert_eq!(
            ContentType::detect("~/Documents/foo.txt"),
            ContentType::Path
        );
    }

    #[test]
    fn test_detect_text() {
        assert_eq!(
            ContentType::detect("Hello, this is some plain text"),
            ContentType::Text
        );
    }

    #[test]
    fn test_preview_truncation() {
        let long = "a".repeat(200);
        let entry = ClipEntry::new(long, None, None);
        assert!(entry.preview.len() <= 83); // 80 + "…" (3 bytes utf8)
    }
}
