use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use base64::{engine::general_purpose, Engine as _};

// ── Transform Types ──

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransformKind {
    PrettyJson,
    MinifyJson,
    SortLines,
    UniqueLines,
    ReverseLines,
    TrimWhitespace,
    AddLineNumbers,
    RemoveLineNumbers,

    HtmlToMarkdown,
    StripHtml,
    Base64Encode,
    Base64Decode,
    UrlEncode,
    UrlDecode,

    Uppercase,
    Lowercase,
    TitleCase,
    CamelToSnake,
    SnakeToCamel,

    TranslateToEnglish,
    FixGrammar,
    Summarize,
    CodeToTypeScript,
    CodeToPython,
    CodeToRust,
    ExplainCode,
    CustomPrompt(String),
}

impl TransformKind {
    pub fn label(&self) -> &str {
        match self {
            Self::PrettyJson => "Pretty JSON",
            Self::MinifyJson => "Minify JSON",
            Self::SortLines => "Sort Lines",
            Self::UniqueLines => "Unique Lines",
            Self::ReverseLines => "Reverse Lines",
            Self::TrimWhitespace => "Trim Whitespace",
            Self::AddLineNumbers => "Add Line Numbers",
            Self::RemoveLineNumbers => "Remove Line Numbers",
            Self::HtmlToMarkdown => "HTML → Markdown",
            Self::StripHtml => "Strip HTML Tags",
            Self::Base64Encode => "Base64 Encode",
            Self::Base64Decode => "Base64 Decode",
            Self::UrlEncode => "URL Encode",
            Self::UrlDecode => "URL Decode",
            Self::Uppercase => "UPPERCASE",
            Self::Lowercase => "lowercase",
            Self::TitleCase => "Title Case",
            Self::CamelToSnake => "camelCase → snake_case",
            Self::SnakeToCamel => "snake_case → camelCase",
            Self::TranslateToEnglish => "Translate to English",
            Self::FixGrammar => "Fix Grammar & Spelling",
            Self::Summarize => "Summarize",
            Self::CodeToTypeScript => "Code → TypeScript",
            Self::CodeToPython => "Code → Python",
            Self::CodeToRust => "Code → Rust",
            Self::ExplainCode => "Explain Code",
            Self::CustomPrompt(_) => "Custom Prompt…",
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            Self::PrettyJson
            | Self::MinifyJson
            | Self::SortLines
            | Self::UniqueLines
            | Self::ReverseLines
            | Self::TrimWhitespace
            | Self::AddLineNumbers
            | Self::RemoveLineNumbers => "FORMAT",

            Self::HtmlToMarkdown
            | Self::StripHtml
            | Self::Base64Encode
            | Self::Base64Decode
            | Self::UrlEncode
            | Self::UrlDecode => "CONVERT",

            Self::Uppercase
            | Self::Lowercase
            | Self::TitleCase
            | Self::CamelToSnake
            | Self::SnakeToCamel => "CASE",

            _ => "AI ✨",
        }
    }

    pub fn is_ai(&self) -> bool {
        matches!(
            self,
            Self::TranslateToEnglish
                | Self::FixGrammar
                | Self::Summarize
                | Self::CodeToTypeScript
                | Self::CodeToPython
                | Self::CodeToRust
                | Self::ExplainCode
                | Self::CustomPrompt(_)
        )
    }

    pub fn icon(&self) -> &str {
        if self.is_ai() {
            "✨"
        } else {
            "⚡"
        }
    }
}

pub fn all_transforms() -> Vec<TransformKind> {
    vec![
        TransformKind::PrettyJson,
        TransformKind::MinifyJson,
        TransformKind::SortLines,
        TransformKind::UniqueLines,
        TransformKind::ReverseLines,
        TransformKind::TrimWhitespace,
        TransformKind::AddLineNumbers,
        TransformKind::RemoveLineNumbers,
        TransformKind::HtmlToMarkdown,
        TransformKind::StripHtml,
        TransformKind::Base64Encode,
        TransformKind::Base64Decode,
        TransformKind::UrlEncode,
        TransformKind::UrlDecode,
        TransformKind::Uppercase,
        TransformKind::Lowercase,
        TransformKind::TitleCase,
        TransformKind::CamelToSnake,
        TransformKind::SnakeToCamel,
        TransformKind::TranslateToEnglish,
        TransformKind::FixGrammar,
        TransformKind::Summarize,
        TransformKind::CodeToTypeScript,
        TransformKind::CodeToPython,
        TransformKind::CodeToRust,
        TransformKind::ExplainCode,
        TransformKind::CustomPrompt(String::new()),
    ]
}

/// Transforms available at paste time (excludes developer-only CONVERT category).
pub fn paste_transforms() -> Vec<TransformKind> {
    all_transforms()
        .into_iter()
        .filter(|t| t.category() != "CONVERT")
        .filter(|t| !matches!(t, TransformKind::CustomPrompt(_)))
        .collect()
}

// ── Transform on Paste Settings ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotInputMode {
    #[serde(alias = "LegacyMultiTap")]
    ExcelDeveloper,
}

impl Default for SlotInputMode {
    fn default() -> Self {
        Self::ExcelDeveloper
    }
}

/// Global shortcut that opens the memory palette. A small preset list for now
/// (a free-form custom binding can come later).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaletteTrigger {
    /// Cmd+Shift+V — the familiar "paste special" gesture (recommended default).
    CmdShiftV,
    /// Ctrl+Option+Space — no character conflict, three keys.
    CtrlOptSpace,
    /// Option+Space — two keys, but normally inserts a non-breaking space.
    OptSpace,
}

impl Default for PaletteTrigger {
    fn default() -> Self {
        Self::CmdShiftV
    }
}

impl PaletteTrigger {
    pub fn label(&self) -> &'static str {
        match self {
            Self::CmdShiftV => "Cmd+Shift+V",
            Self::CtrlOptSpace => "Ctrl+Option+Space",
            Self::OptSpace => "Option+Space",
        }
    }
}

/// Global hotkey that opens the clipd window. All options use the `G` key with
/// different modifiers so they never collide with the letter-slot chords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenGuiHotkey {
    CtrlG,
    /// Windows-only: Alt+G avoids the Ctrl+G "find next" clash in browsers.
    AltG,
    CmdShiftG,
    CtrlShiftG,
    Disabled,
}

impl Default for OpenGuiHotkey {
    fn default() -> Self {
        // Windows defaults to Alt+G: Ctrl+G collides with "find next" in
        // browsers/editors, and Alt+G is nearly always free.
        if cfg!(target_os = "windows") {
            Self::AltG
        } else {
            Self::CtrlG
        }
    }
}

impl OpenGuiHotkey {
    pub fn label(&self) -> &'static str {
        match self {
            Self::CtrlG => "Ctrl+G",
            Self::AltG => "Alt+G",
            // The Cmd key is Super/Win off macOS — label it what users see.
            Self::CmdShiftG => {
                if cfg!(target_os = "macos") {
                    "Cmd+Shift+G"
                } else {
                    "Win+Shift+G"
                }
            }
            Self::CtrlShiftG => "Ctrl+Shift+G",
            Self::Disabled => "Disabled",
        }
    }

    pub const ALL: [OpenGuiHotkey; 5] = [
        Self::CtrlG,
        Self::AltG,
        Self::CmdShiftG,
        Self::CtrlShiftG,
        Self::Disabled,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteTransformSettings {
    #[serde(default = "default_false")]
    pub enabled: bool,

    #[serde(default = "default_true_val")]
    pub smart_mode: bool,

    #[serde(default)]
    pub active_transforms: Vec<TransformKind>,

    #[serde(default)]
    pub default_ai_prompt: String,

    #[serde(default = "default_false")]
    pub onboarding_seen: bool,

    #[serde(default = "default_true_val")]
    pub hud_enabled: bool,

    /// After multi-tap copy (Cmd+C × N), restore clipboard to slot 1's content.
    /// When false, the clipboard keeps the original copied content after multi-tap.
    #[serde(default = "default_true_val")]
    pub copy_multi_tap_restore: bool,

    /// Enables direct A-Z letter slots in addition to Excel/developer numeric slots.
    /// In Paste Settings this is surfaced as "Enable A-Z aliases".
    #[serde(default = "default_true_val")]
    pub letter_slots_enabled: bool,

    #[serde(default)]
    pub slot_input_mode: SlotInputMode,

    // ── Configurable Paste Settings (see SPEC-tier1-ai-memory) ──
    /// Remember copied items in clipd history. Palette recall depends on this.
    #[serde(default = "default_true_val")]
    pub remember_clipboard: bool,

    /// Memory palette (recall over history by content/source/time/alias).
    #[serde(default = "default_true_val")]
    pub palette_enabled: bool,

    /// Which global shortcut opens the memory palette.
    #[serde(default)]
    pub palette_trigger: PaletteTrigger,

    /// Multi-slot copy/paste via Cmd/Ctrl multi-tap (slots 1-9).
    /// On by default — this is clipd's core "multi-slot clipboard" behavior.
    #[serde(default = "default_true_val")]
    pub multi_slot_enabled: bool,

    /// Extended Excel/developer slots 11-30 via Option+C/V multi-tap.
    #[serde(default = "default_false")]
    pub extended_slots_enabled: bool,

    /// Direct global A-Z letter-slot chords (Ctrl+Option+C/V then a letter).
    /// Letter slots themselves are governed by `letter_slots_enabled`; this only
    /// gates the keyboard chords. On by default so existing letter-slot users
    /// keep their workflow; turn it off to free the chords once palette-based
    /// alias paste lands.
    #[serde(default = "default_true_val")]
    pub direct_letter_shortcuts_enabled: bool,

    /// Lighter letter-slot save: double-tap Cmd+C then a letter saves to that
    /// letter slot (single Cmd+C is untouched, so normal copy isn't hampered).
    #[serde(default = "default_true_val")]
    pub quick_letter_slots_enabled: bool,

    /// Secondary alias system: the memory palette lists letter slots as @A rows
    /// so you can recall a letter slot by typing it — no chord.
    #[serde(default = "default_false")]
    pub palette_aliases_enabled: bool,

    /// Batch-drain (sequence) paste: Cmd+Option+V pastes collected slots in order.
    #[serde(default = "default_true_val")]
    pub batch_drain_enabled: bool,

    /// GUI behavior: clicking a row copies it immediately. When disabled,
    /// clicking only selects; double-click and Enter still copy.
    #[serde(default = "default_true_val")]
    pub copy_on_select: bool,

    /// Warn in the GUI before assigning a conflicting/risky shortcut.
    #[serde(default = "default_true_val")]
    pub warn_conflicting_shortcuts: bool,

    /// Global hotkey that opens the clipd window (default Ctrl+G).
    #[serde(default)]
    pub open_gui_hotkey: OpenGuiHotkey,

    /// After copying a clip from the GUI, return focus to the app you were in
    /// (the one focused when you summoned clipd), so you can paste right away.
    #[serde(default = "default_true_val")]
    pub return_focus_after_copy: bool,
}

fn default_false() -> bool {
    false
}
fn default_true_val() -> bool {
    true
}

impl Default for PasteTransformSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            smart_mode: true,
            active_transforms: vec![TransformKind::TrimWhitespace, TransformKind::PrettyJson],
            default_ai_prompt: String::new(),
            onboarding_seen: false,
            hud_enabled: true,
            copy_multi_tap_restore: true,
            letter_slots_enabled: true,
            slot_input_mode: SlotInputMode::default(),
            remember_clipboard: true,
            palette_enabled: true,
            palette_trigger: PaletteTrigger::default(),
            multi_slot_enabled: true,
            extended_slots_enabled: false,
            direct_letter_shortcuts_enabled: true,
            quick_letter_slots_enabled: true,
            palette_aliases_enabled: false,
            batch_drain_enabled: true,
            copy_on_select: true,
            warn_conflicting_shortcuts: true,
            open_gui_hotkey: OpenGuiHotkey::default(),
            return_focus_after_copy: true,
        }
    }
}

fn paste_settings_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("paste_transform.json")
}

pub fn load_paste_transform_settings() -> PasteTransformSettings {
    std::fs::read_to_string(paste_settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_paste_transform_settings(settings: &PasteTransformSettings) {
    let path = paste_settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn last_active_app_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("last_active_app.txt")
}

/// Record the app that was frontmost when clipd was summoned, so the GUI can
/// hand focus back to it after a copy.
pub fn save_last_active_app(name: &str) {
    let path = last_active_app_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, name.trim());
}

pub fn load_last_active_app() -> Option<String> {
    std::fs::read_to_string(last_active_app_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── Config ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_api_url")]
    pub api_url: String,
    #[serde(default = "default_model")]
    pub model: String,
}

fn default_api_url() -> String {
    "https://api.openai.com/v1/chat/completions".to_string()
}

fn default_model() -> String {
    "gpt-4o-mini".to_string()
}

impl Default for TransformConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            api_url: default_api_url(),
            model: default_model(),
        }
    }
}

fn config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("transform.json")
}

pub fn load_transform_config() -> TransformConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_transform_config(config: &TransformConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(path, json);
    }
}

// ── Apply Transform ──

pub fn apply_transform(
    kind: &TransformKind,
    input: &str,
    config: &TransformConfig,
) -> Result<String, String> {
    match kind {
        TransformKind::PrettyJson => pretty_json(input),
        TransformKind::MinifyJson => minify_json(input),
        TransformKind::SortLines => Ok(sort_lines(input)),
        TransformKind::UniqueLines => Ok(unique_lines(input)),
        TransformKind::ReverseLines => Ok(reverse_lines(input)),
        TransformKind::TrimWhitespace => Ok(trim_whitespace(input)),
        TransformKind::AddLineNumbers => Ok(add_line_numbers(input)),
        TransformKind::RemoveLineNumbers => Ok(remove_line_numbers(input)),
        TransformKind::HtmlToMarkdown => Ok(html_to_markdown(input)),
        TransformKind::StripHtml => Ok(strip_html(input)),
        TransformKind::Base64Encode => Ok(base64_encode(input)),
        TransformKind::Base64Decode => base64_decode(input),
        TransformKind::UrlEncode => Ok(url_encode(input)),
        TransformKind::UrlDecode => url_decode(input),
        TransformKind::Uppercase => Ok(input.to_uppercase()),
        TransformKind::Lowercase => Ok(input.to_lowercase()),
        TransformKind::TitleCase => Ok(title_case(input)),
        TransformKind::CamelToSnake => Ok(camel_to_snake(input)),
        TransformKind::SnakeToCamel => Ok(snake_to_camel(input)),
        _ => ai_transform(kind, input, config),
    }
}

// ── Built-in Implementations ──

fn strip_code_fence(input: &str) -> &str {
    let s = input.trim();
    if s.starts_with("```") {
        let after_fence = &s[3..];
        // skip optional language tag on the first line
        let body = after_fence.find('\n').map_or("", |i| &after_fence[i + 1..]);
        let body = body.trim_end();
        if body.ends_with("```") {
            return body[..body.len() - 3].trim();
        }
    }
    input
}

fn pretty_json(input: &str) -> Result<String, String> {
    let src = strip_code_fence(input);
    let value: serde_json::Value =
        serde_json::from_str(src).map_err(|e| format!("Invalid JSON: {}", e))?;
    serde_json::to_string_pretty(&value).map_err(|e| format!("JSON formatting failed: {}", e))
}

fn minify_json(input: &str) -> Result<String, String> {
    let src = strip_code_fence(input);
    let value: serde_json::Value =
        serde_json::from_str(src).map_err(|e| format!("Invalid JSON: {}", e))?;
    serde_json::to_string(&value).map_err(|e| format!("JSON minification failed: {}", e))
}

fn sort_lines(input: &str) -> String {
    let mut lines: Vec<&str> = input.lines().collect();
    lines.sort_unstable();
    lines.join("\n")
}

fn unique_lines(input: &str) -> String {
    let mut seen = HashSet::new();
    input
        .lines()
        .filter(|line| seen.insert(*line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn reverse_lines(input: &str) -> String {
    input.lines().rev().collect::<Vec<_>>().join("\n")
}

fn trim_whitespace(input: &str) -> String {
    let mut result = Vec::new();
    let mut prev_blank = false;
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_blank {
                result.push("");
            }
            prev_blank = true;
        } else {
            result.push(trimmed);
            prev_blank = false;
        }
    }
    result.join("\n").trim().to_string()
}

fn add_line_numbers(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let width = lines.len().to_string().len();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>w$}│ {}", i + 1, line, w = width))
        .collect::<Vec<_>>()
        .join("\n")
}

fn remove_line_numbers(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let digit_end = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
            if digit_end > 0 {
                let rest = &trimmed[digit_end..];
                for sep in ["│ ", "| ", ". ", ": ", ") "] {
                    if rest.starts_with(sep) {
                        return rest[sep.len()..].to_string();
                    }
                }
                for sep in ["│", "|", ".", ":", ")"] {
                    if rest.starts_with(sep) {
                        return rest[sep.len()..].trim_start().to_string();
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn base64_encode(input: &str) -> String {
    general_purpose::STANDARD.encode(input.as_bytes())
}

fn base64_decode(input: &str) -> Result<String, String> {
    let bytes = general_purpose::STANDARD
        .decode(input.trim())
        .map_err(|e| format!("Invalid base64: {}", e))?;
    String::from_utf8(bytes).map_err(|e| format!("Decoded bytes are not valid UTF-8: {}", e))
}

fn url_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

fn url_decode(input: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let h1 = chars
                .next()
                .ok_or_else(|| "Incomplete percent encoding".to_string())?;
            let h2 = chars
                .next()
                .ok_or_else(|| "Incomplete percent encoding".to_string())?;
            let hex = format!("{}{}", h1, h2);
            let byte = u8::from_str_radix(&hex, 16)
                .map_err(|_| format!("Invalid hex in URL encoding: %{}", hex))?;
            bytes.push(byte);
        } else if ch == '+' {
            bytes.push(b' ');
        } else {
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            bytes.extend_from_slice(s.as_bytes());
        }
    }
    String::from_utf8(bytes).map_err(|e| format!("Invalid UTF-8 after URL decoding: {}", e))
}

fn title_case(input: &str) -> String {
    input
        .split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(f) => {
                    let upper: String = f.to_uppercase().collect();
                    let rest: String = chars.collect::<String>().to_lowercase();
                    format!("{}{}", upper, rest)
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn camel_to_snake(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let mut result = String::new();
            let chars: Vec<char> = line.chars().collect();
            for (i, &ch) in chars.iter().enumerate() {
                if ch.is_uppercase() && i > 0 {
                    if chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit() {
                        result.push('_');
                    }
                }
                result.extend(ch.to_lowercase());
            }
            result
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn snake_to_camel(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let mut result = String::new();
            let mut capitalize_next = false;
            for ch in line.chars() {
                if ch == '_' {
                    capitalize_next = true;
                } else if capitalize_next {
                    result.extend(ch.to_uppercase());
                    capitalize_next = false;
                } else {
                    result.push(ch);
                }
            }
            result
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── HTML Conversion ──

fn html_to_markdown(html: &str) -> String {
    let mut s = html.to_string();

    s = convert_html_links(&s);
    s = convert_html_images(&s);

    for level in 1..=6usize {
        let tag = format!("h{}", level);
        let prefix = "#".repeat(level);
        for open in [format!("<{}>", tag), format!("<{}>", tag.to_uppercase())] {
            s = s.replace(&open, &format!("\n{} ", prefix));
        }
        for close in [format!("</{}>", tag), format!("</{}>", tag.to_uppercase())] {
            s = s.replace(&close, "\n");
        }
    }

    for (open, close) in [
        ("<strong>", "</strong>"),
        ("<b>", "</b>"),
        ("<STRONG>", "</STRONG>"),
        ("<B>", "</B>"),
    ] {
        s = s.replace(open, "**").replace(close, "**");
    }

    for (open, close) in [
        ("<em>", "</em>"),
        ("<i>", "</i>"),
        ("<EM>", "</EM>"),
        ("<I>", "</I>"),
    ] {
        s = s.replace(open, "*").replace(close, "*");
    }

    for (open, close) in [("<code>", "</code>"), ("<CODE>", "</CODE>")] {
        s = s.replace(open, "`").replace(close, "`");
    }
    for (open, close) in [("<pre>", "</pre>"), ("<PRE>", "</PRE>")] {
        s = s.replace(open, "\n```\n").replace(close, "\n```\n");
    }

    for br in ["<br>", "<br/>", "<br />", "<BR>", "<BR/>", "<BR />"] {
        s = s.replace(br, "\n");
    }

    for (open, close) in [("<p>", "</p>"), ("<P>", "</P>")] {
        s = s.replace(open, "\n\n").replace(close, "");
    }

    for (open, close) in [("<li>", "</li>"), ("<LI>", "</LI>")] {
        s = s.replace(open, "\n- ").replace(close, "");
    }
    for tag in ["ul", "ol", "UL", "OL"] {
        s = s
            .replace(&format!("<{}>", tag), "")
            .replace(&format!("</{}>", tag), "\n");
    }

    for (open, close) in [
        ("<blockquote>", "</blockquote>"),
        ("<BLOCKQUOTE>", "</BLOCKQUOTE>"),
    ] {
        s = s.replace(open, "\n> ").replace(close, "\n");
    }

    for hr in ["<hr>", "<hr/>", "<hr />", "<HR>", "<HR/>", "<HR />"] {
        s = s.replace(hr, "\n---\n");
    }

    s = decode_html_entities(&s);
    s = strip_tags(&s);

    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    s.trim().to_string()
}

fn strip_html(html: &str) -> String {
    let decoded = decode_html_entities(html);
    let mut stripped = strip_tags(&decoded);
    while stripped.contains("\n\n\n") {
        stripped = stripped.replace("\n\n\n", "\n\n");
    }
    stripped.trim().to_string()
}

fn strip_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&ndash;", "\u{2013}")
        .replace("&mdash;", "\u{2014}")
        .replace("&hellip;", "\u{2026}")
        .replace("&copy;", "\u{00A9}")
        .replace("&reg;", "\u{00AE}")
        .replace("&trade;", "\u{2122}")
}

fn convert_html_links(html: &str) -> String {
    let mut result = String::new();
    let mut pos = 0;
    let lower = html.to_lowercase();

    while pos < html.len() {
        if let Some(offset) = lower[pos..].find("<a ") {
            let abs_start = pos + offset;
            result.push_str(&html[pos..abs_start]);

            let lower_rest = &lower[abs_start..];

            if let Some(href_off) = lower_rest.find("href=\"") {
                let href_val_start = abs_start + href_off + 6;
                if let Some(href_end) = html[href_val_start..].find('"') {
                    let href = &html[href_val_start..href_val_start + href_end];
                    if let Some(close_bracket) = html[abs_start..].find('>') {
                        let content_start = abs_start + close_bracket + 1;
                        if let Some(close_off) = lower[content_start..].find("</a>") {
                            let content = &html[content_start..content_start + close_off];
                            result.push_str(&format!("[{}]({})", content.trim(), href));
                            pos = content_start + close_off + 4;
                            continue;
                        }
                    }
                }
            }
            result.push_str(&html[abs_start..abs_start + 3]);
            pos = abs_start + 3;
        } else {
            result.push_str(&html[pos..]);
            break;
        }
    }
    result
}

fn convert_html_images(html: &str) -> String {
    let mut result = String::new();
    let mut pos = 0;
    let lower = html.to_lowercase();

    while pos < html.len() {
        if let Some(offset) = lower[pos..].find("<img ") {
            let abs_start = pos + offset;
            result.push_str(&html[pos..abs_start]);

            if let Some(end_off) = html[abs_start..].find('>') {
                let tag = &html[abs_start..abs_start + end_off + 1];
                let lower_tag = tag.to_lowercase();
                let src = extract_attr_val(&lower_tag, tag, "src").unwrap_or_default();
                let alt = extract_attr_val(&lower_tag, tag, "alt").unwrap_or_default();
                result.push_str(&format!("![{}]({})", alt, src));
                pos = abs_start + end_off + 1;
            } else {
                result.push_str(&html[abs_start..abs_start + 5]);
                pos = abs_start + 5;
            }
        } else {
            result.push_str(&html[pos..]);
            break;
        }
    }
    result
}

fn extract_attr_val(lower_tag: &str, original_tag: &str, attr: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr);
    if let Some(start) = lower_tag.find(&pattern) {
        let val_start = start + pattern.len();
        if let Some(end) = original_tag[val_start..].find('"') {
            return Some(original_tag[val_start..val_start + end].to_string());
        }
    }
    None
}

// ── AI Transform ──

fn ai_transform(
    kind: &TransformKind,
    input: &str,
    config: &TransformConfig,
) -> Result<String, String> {
    let api_key = config
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            let path = config_path();
            format!(
                "AI transforms require an API key.\n\n\
                 Configure it in:\n  {}\n\n\
                 Example:\n\
                 {{\n\
                 \x20 \"api_key\": \"sk-...\",\n\
                 \x20 \"api_url\": \"https://api.openai.com/v1/chat/completions\",\n\
                 \x20 \"model\": \"gpt-4o-mini\"\n\
                 }}\n\n\
                 Also works with Ollama, LM Studio, or any OpenAI-compatible API.",
                path.display()
            )
        })?;

    let system_prompt = match kind {
        TransformKind::TranslateToEnglish => {
            "You are a translator. Translate the following text to English. \
             Return only the translation, no explanations or preamble."
        }
        TransformKind::FixGrammar => {
            "Fix the grammar, spelling, and punctuation in the following text. \
             Return only the corrected text, no explanations."
        }
        TransformKind::Summarize => {
            "Summarize the following text concisely in a few sentences. \
             Return only the summary."
        }
        TransformKind::CodeToTypeScript => {
            "Translate the following code to TypeScript. \
             Return only the code, no explanations or markdown fences."
        }
        TransformKind::CodeToPython => {
            "Translate the following code to Python. \
             Return only the code, no explanations or markdown fences."
        }
        TransformKind::CodeToRust => {
            "Translate the following code to Rust. \
             Return only the code, no explanations or markdown fences."
        }
        TransformKind::ExplainCode => {
            "Explain what the following code does in clear, concise terms. \
             Include the language, purpose, and key logic."
        }
        TransformKind::CustomPrompt(prompt) => prompt.as_str(),
        _ => return Err("Not an AI transform".to_string()),
    };

    let (sys_msg, user_msg) = if let TransformKind::CustomPrompt(_) = kind {
        (
            "You are a helpful assistant that transforms text according to instructions. \
             Return only the transformed result unless the instruction asks for explanation."
                .to_string(),
            format!("Instruction: {}\n\nText:\n{}", system_prompt, input),
        )
    } else {
        (system_prompt.to_string(), input.to_string())
    };

    let body = serde_json::json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": sys_msg},
            {"role": "user", "content": user_msg}
        ],
        "max_tokens": 4096,
        "temperature": 0.3
    });

    let mut request = ureq::post(&config.api_url).set("Content-Type", "application/json");

    if !api_key.is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", api_key));
    }

    let response = request
        .send_json(body)
        .map_err(|e| format!("API request failed: {}", e))?;

    let resp_body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse API response: {}", e))?;

    resp_body["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| {
            if let Some(err) = resp_body["error"]["message"].as_str() {
                format!("API error: {}", err)
            } else {
                "Unexpected API response format".to_string()
            }
        })
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pretty_json() {
        let input = r#"{"name":"clipd","version":"0.1"}"#;
        let result = pretty_json(input).unwrap();
        assert!(result.contains("  \"name\": \"clipd\""));
    }

    #[test]
    fn test_minify_json() {
        let input = "{\n  \"name\": \"clipd\",\n  \"version\": \"0.1\"\n}";
        let result = minify_json(input).unwrap();
        assert_eq!(result, r#"{"name":"clipd","version":"0.1"}"#);
    }

    #[test]
    fn test_sort_lines() {
        assert_eq!(sort_lines("banana\napple\ncherry"), "apple\nbanana\ncherry");
    }

    #[test]
    fn test_unique_lines() {
        assert_eq!(unique_lines("a\nb\na\nc\nb"), "a\nb\nc");
    }

    #[test]
    fn test_reverse_lines() {
        assert_eq!(reverse_lines("1\n2\n3"), "3\n2\n1");
    }

    #[test]
    fn test_base64_roundtrip() {
        let input = "Hello, clipd!";
        let encoded = base64_encode(input);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn test_url_roundtrip() {
        let input = "hello world & foo=bar";
        let encoded = url_encode(input);
        let decoded = url_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn test_title_case() {
        assert_eq!(title_case("hello world"), "Hello World");
    }

    #[test]
    fn test_camel_to_snake() {
        assert_eq!(camel_to_snake("myVariableName"), "my_variable_name");
    }

    #[test]
    fn test_snake_to_camel() {
        assert_eq!(snake_to_camel("my_variable_name"), "myVariableName");
    }

    #[test]
    fn test_html_to_markdown() {
        let html = "<h1>Title</h1><p>Hello <strong>world</strong></p>";
        let md = html_to_markdown(html);
        assert!(md.contains("# Title"));
        assert!(md.contains("**world**"));
    }

    #[test]
    fn test_strip_html() {
        let html = "<p>Hello <b>world</b></p>";
        assert_eq!(strip_html(html), "Hello world");
    }

    #[test]
    fn test_html_links() {
        let html = r#"Click <a href="https://example.com">here</a> now"#;
        let result = convert_html_links(html);
        assert!(result.contains("[here](https://example.com)"));
    }

    #[test]
    fn test_line_numbers_roundtrip() {
        let input = "fn main() {\n    println!(\"hi\");\n}";
        let numbered = add_line_numbers(input);
        assert!(numbered.contains("1│ fn main()"));
        let restored = remove_line_numbers(&numbered);
        assert_eq!(restored, input);
    }
}
