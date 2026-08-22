//! Smart Recommend — what clipd should offer to do with a clip.
//!
//! clipd already has the capabilities (transforms, ask, actions); what it has
//! lacked is a moment where it *offers* them. This module is that decision, in
//! one place, so the preview pane, the row hover chips and the HUD all propose
//! the same thing for the same clip instead of drifting apart.
//!
//! Everything here is local and instant — pure heuristics over the clip text,
//! no model call. Deciding *what to offer* must never cost a network round
//! trip, or the suggestion arrives after the user has moved on.

use crate::models::{ClipEntry, ContentType};
use crate::transform::TransformKind;

/// What running a suggestion does.
#[derive(Debug, Clone, PartialEq)]
pub enum SuggestionKind {
    /// Run an existing transform over the clip.
    Transform(TransformKind),
    /// Open ask mode pre-filled with this question.
    Ask(String),
}

/// One offered action, ready to render as a chip.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// Short verb phrase for the chip: "Explain", "Translate".
    pub label: &'static str,
    /// Single glyph shown before the label.
    pub icon: &'static str,
    pub kind: SuggestionKind,
    /// True when this needs an API key; surfaces disable it when none is set.
    pub needs_ai: bool,
}

impl Suggestion {
    fn transform(
        label: &'static str,
        icon: &'static str,
        kind: TransformKind,
        needs_ai: bool,
    ) -> Self {
        Self {
            label,
            icon,
            kind: SuggestionKind::Transform(kind),
            needs_ai,
        }
    }
}

/// How many chips a surface should show before "more".
pub const VISIBLE_SUGGESTIONS: usize = 3;

/// Rank what to offer for this clip, best first.
///
/// Ordering is deliberate: cheap deterministic transforms outrank model calls
/// when both apply, because a local reformat is instant and always correct
/// while a summary is neither. Within the model-backed group, the suggestion
/// keyed to the *strongest* signal about the content wins.
pub fn suggest_for(clip: &ClipEntry) -> Vec<Suggestion> {
    let body = clip_text(clip);
    let trimmed = body.trim();
    let mut out: Vec<Suggestion> = Vec::new();

    if trimmed.is_empty() {
        return out;
    }

    // ── Deterministic wins first ──

    if looks_like_json(trimmed) {
        out.push(Suggestion::transform(
            "Format",
            "{}",
            TransformKind::PrettyJson,
            false,
        ));
    }

    // ── Content-type driven ──

    match clip.content_type {
        ContentType::Code => {
            out.push(Suggestion::transform(
                "Explain",
                "💡",
                TransformKind::ExplainCode,
                true,
            ));
        }
        ContentType::Url => {
            out.push(Suggestion {
                label: "Ask",
                icon: "✨",
                kind: SuggestionKind::Ask(
                    "what is this link and where did I copy it from?".into(),
                ),
                needs_ai: true,
            });
        }
        ContentType::Image => {
            // The OCR pass already ran at capture; the useful next step is
            // making sense of the extracted text, not extracting it again.
            if word_count(trimmed) > 40 {
                out.push(Suggestion::transform(
                    "Summarize",
                    "📝",
                    TransformKind::Summarize,
                    true,
                ));
            }
        }
        _ => {}
    }

    // ── Language and length signals ──

    if has_non_latin(trimmed) {
        out.push(Suggestion::transform(
            "Translate",
            "🌐",
            TransformKind::TranslateToEnglish,
            true,
        ));
    }

    let words = word_count(trimmed);
    if words > 120 && clip.content_type != ContentType::Code {
        out.push(Suggestion::transform(
            "Summarize",
            "📝",
            TransformKind::Summarize,
            true,
        ));
    } else if (12..=120).contains(&words) && looks_like_prose(trimmed) {
        out.push(Suggestion::transform(
            "Polish",
            "✏",
            TransformKind::FixGrammar,
            true,
        ));
    }

    if trimmed.lines().count() > 3 && has_trailing_whitespace(trimmed) {
        out.push(Suggestion::transform(
            "Trim",
            "✂",
            TransformKind::TrimWhitespace,
            false,
        ));
    }

    // Ask is the universal fallback — there is always something to ask about a
    // clip, so it closes out the list rather than competing for the top slot.
    if !out.iter().any(|s| matches!(s.kind, SuggestionKind::Ask(_))) {
        out.push(Suggestion {
            label: "Ask",
            icon: "✨",
            kind: SuggestionKind::Ask("what is this and when did I copy it?".into()),
            needs_ai: true,
        });
    }

    dedupe(out)
}

/// Drop later duplicates of the same action, keeping the highest-ranked.
fn dedupe(items: Vec<Suggestion>) -> Vec<Suggestion> {
    let mut seen: Vec<SuggestionKind> = Vec::new();
    let mut out = Vec::new();
    for item in items {
        if !seen.contains(&item.kind) {
            seen.push(item.kind.clone());
            out.push(item);
        }
    }
    out
}

/// Image clips carry their meaning in OCR text, not `content`.
fn clip_text(clip: &ClipEntry) -> &str {
    match clip.ocr_text.as_deref() {
        Some(ocr) if !ocr.trim().is_empty() && clip.content.trim().is_empty() => ocr,
        _ => &clip.content,
    }
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn looks_like_json(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with('{') && s.ends_with('}')) || (s.starts_with('[') && s.ends_with(']'))
}

/// Any non-ASCII letter is treated as a translation signal. Deliberately
/// coarse: offering Translate on text that turns out to be English is a
/// harmless extra chip, while missing it entirely is the failure that matters.
fn has_non_latin(s: &str) -> bool {
    s.chars()
        .any(|ch| ch.is_alphabetic() && !ch.is_ascii_alphabetic())
}

/// Prose, not code or data: mostly words and sentence punctuation, and none of
/// the density of symbols that marks up source.
fn looks_like_prose(s: &str) -> bool {
    let symbols = s
        .chars()
        .filter(|ch| "{}[]();<>=|&_$#".contains(*ch))
        .count();
    let letters = s.chars().filter(|ch| ch.is_alphabetic()).count();
    letters > 0 && (symbols * 12) < letters
}

fn has_trailing_whitespace(s: &str) -> bool {
    s.lines().any(|line| line.ends_with(' ') || line.ends_with('\t'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn clip(content: &str, ct: ContentType) -> ClipEntry {
        ClipEntry {
            id: 1,
            content: content.to_string(),
            content_type: ct,
            content_hash: "h".into(),
            source_app: None,
            source_title: None,
            timestamp: Utc::now(),
            preview: content.chars().take(40).collect(),
            slot: None,
            image_path: None,
            thumb_path: None,
            ocr_text: None,
            files: Vec::new(),
        }
    }

    fn labels(clip: &ClipEntry) -> Vec<&'static str> {
        suggest_for(clip).into_iter().map(|s| s.label).collect()
    }

    #[test]
    fn code_is_offered_an_explanation() {
        let l = labels(&clip("fn main() { println!(\"hi\"); }", ContentType::Code));
        assert!(l.contains(&"Explain"));
    }

    #[test]
    fn json_gets_a_local_format_before_any_model_call() {
        let l = labels(&clip(r#"{"a":1,"b":2}"#, ContentType::Code));
        assert_eq!(
            l.first(),
            Some(&"Format"),
            "a deterministic transform must outrank a model call"
        );
    }

    #[test]
    fn non_latin_text_is_offered_translation() {
        let l = labels(&clip("这是一段中文文本", ContentType::Text));
        assert!(l.contains(&"Translate"));
    }

    #[test]
    fn plain_english_is_not_offered_translation() {
        let l = labels(&clip("just some ordinary english words here", ContentType::Text));
        assert!(!l.contains(&"Translate"));
    }

    #[test]
    fn long_prose_is_summarized_not_polished() {
        let long = "word ".repeat(200);
        let l = labels(&clip(&long, ContentType::Text));
        assert!(l.contains(&"Summarize"));
        assert!(!l.contains(&"Polish"));
    }

    #[test]
    fn medium_prose_is_offered_a_polish() {
        let text = "this sentence have some grammar problem that could be fixed by a model \
                    and it is long enough to count as prose rather than a fragment";
        let l = labels(&clip(text, ContentType::Text));
        assert!(l.contains(&"Polish"));
    }

    #[test]
    fn dense_code_is_not_mistaken_for_prose() {
        let code = "if (a && b) { c[d] = e(f); } else { g_h($i); }";
        assert!(!looks_like_prose(code));
    }

    #[test]
    fn ask_is_always_available_as_a_fallback() {
        let l = labels(&clip("x", ContentType::Text));
        assert!(l.contains(&"Ask"), "every clip must have something to offer");
    }

    #[test]
    fn a_url_is_offered_ask_only_once() {
        let l = labels(&clip("https://example.com/docs", ContentType::Url));
        assert_eq!(l.iter().filter(|x| **x == "Ask").count(), 1);
    }

    #[test]
    fn empty_clips_offer_nothing() {
        assert!(suggest_for(&clip("   ", ContentType::Text)).is_empty());
    }

    #[test]
    fn ocr_text_drives_suggestions_for_image_clips() {
        let mut c = clip("", ContentType::Image);
        c.ocr_text = Some("word ".repeat(60));
        assert!(labels(&c).contains(&"Summarize"));
    }

    #[test]
    fn local_transforms_are_not_marked_as_needing_ai() {
        let s = suggest_for(&clip(r#"{"a":1}"#, ContentType::Code));
        let fmt = s.iter().find(|s| s.label == "Format").unwrap();
        assert!(!fmt.needs_ai, "pretty-printing JSON must work without a key");
    }
}
