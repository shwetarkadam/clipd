//! Faster ways to paste, without opening a window.
//!
//! Most of the time someone goes back for a clip it is not a search: it is
//! "the thing I copied a minute ago in the other tab". Three gestures cover
//! that, each opt-in from Settings (they listen to the keyboard in new ways):
//!
//! - **Switcher** — hold ⌃ and tap V. A strip of recent clips appears at the
//!   text cursor; each tap moves along; letting go of ⌃ pastes the selected
//!   one. ⌘Tab, for the clipboard: the first tap lands on the clip *before*
//!   the current one, because the current one is what ⌘V already pastes.
//! - **Peek** — hold ⌘ on its own for a moment and a tooltip at the cursor
//!   says what ⌘V pastes, what each filled numbered slot (⌘V ×N) and each
//!   filled letter slot (⌃⌥A…) would. Slots stop being invisible state you
//!   have to remember.
//! - **Type to paste** — type `;;` and a few letters, then a space, and the
//!   trigger is replaced by the matching clip: `;;alex` → the latest clip
//!   with "alex" in it, `;;link` / `;;email` / `;;code` → the latest of that
//!   kind, `;;2` → slot 2.
//!
//! This module is the logic, kept pure so it can be tested; the daemon owns
//! the keyboard and the pasting, and `clipd-hud` draws the overlays.

use crate::models::{ClipEntry, ContentType};

/// How many clips the switcher offers: one per number key.
pub const SWITCHER_MAX: usize = 9;

/// One card in the switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitcherItem {
    pub content: String,
    /// "link", "mail", "code" or "text" — the overlay's icon.
    pub kind: &'static str,
    /// The app it was copied from.
    pub app: String,
    /// Where in that app: the page or document title, when known.
    pub source: String,
    pub preview: String,
}

fn kind_of(t: &ContentType) -> &'static str {
    match t {
        ContentType::Url => "link",
        ContentType::Email => "mail",
        ContentType::Code => "code",
        _ => "text",
    }
}

/// A single line of at most `max` characters, safe inside a tab-separated
/// overlay message.
fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// The switcher's cards, newest first: text clips only (an image or a file
/// cannot be pasted by putting text on the clipboard), each text once.
pub fn switcher_items(clips: &[ClipEntry]) -> Vec<SwitcherItem> {
    let mut seen = std::collections::HashSet::new();
    clips
        .iter()
        .filter(|c| !matches!(c.content_type, ContentType::Image | ContentType::File))
        .filter(|c| !c.content.trim().is_empty())
        .filter(|c| seen.insert(c.content.clone()))
        .take(SWITCHER_MAX)
        .map(|c| SwitcherItem {
            content: c.content.clone(),
            kind: kind_of(&c.content_type),
            app: one_line(c.source_app.as_deref().unwrap_or(""), 24),
            source: one_line(c.source_title.as_deref().unwrap_or(""), 40),
            preview: one_line(&c.content, 60),
        })
        .collect()
}

/// Where the first tap lands: on the clip before the current one, when there
/// is one — the current one is what a plain ⌘V already pastes.
pub fn first_selection(len: usize) -> usize {
    usize::from(len > 1)
}

/// The next tap's selection, wrapping.
pub fn next_selection(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (current + 1) % len
    }
}

/// `AT` line for an overlay: the point under the text cursor, top-left
/// origin, in points. Omitted when unknown; the overlay centres itself.
fn at_line(at: Option<(f64, f64)>) -> String {
    match at {
        Some((x, y)) if x.is_finite() && y.is_finite() => format!("AT\t{x:.0}\t{y:.0}\n"),
        _ => String::new(),
    }
}

/// The overlay message for the switcher.
pub fn switcher_payload(items: &[SwitcherItem], selected: usize, at: Option<(f64, f64)>) -> String {
    let mut out = format!("STYLE\tswitcher\n{}SEL\t{}\n", at_line(at), selected);
    for (i, item) in items.iter().enumerate() {
        out.push_str(&format!(
            "CARD\t{}\t{}\t{}\t{}\t{}\n",
            i + 1,
            item.kind,
            item.app,
            item.source,
            item.preview
        ));
    }
    out
}

/// Most lines a peek shows; past this it ends with "+N more".
const PEEK_MAX_LINES: usize = 12;

/// What the peek says: the chord, and what it pastes. ⌘V is the clipboard;
/// then every numbered slot that holds something (⌘V ×N, when multi-slot is
/// on) and every letter slot that does (when `letter_chord` names how to
/// paste one — "⌃⌥" for the direct chord, "⌘⌥V " for the leader). Empty
/// slots are left out: the peek answers "what have I got", not "what could I".
pub fn peek_lines(
    clipboard: Option<&str>,
    slot: impl Fn(u8) -> Option<String>,
    multi_slot: bool,
    letter_chord: Option<&str>,
) -> Vec<(String, String)> {
    let mut lines = Vec::new();
    let now = clipboard
        .map(|c| one_line(c, 70))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "(nothing on the clipboard)".to_string());
    lines.push(("⌘V".to_string(), now));
    let filled = |n: u8| slot(n).filter(|t| !t.trim().is_empty());
    let mut more = Vec::new();
    if multi_slot {
        for n in 2..=9u8 {
            if let Some(text) = filled(n) {
                more.push((format!("⌘V ×{n}"), one_line(&text, 70)));
            }
        }
    }
    if let Some(chord) = letter_chord {
        // Letter slots are stored as 31..=56 (A..=Z); see `slot_badge`.
        for n in 31..=56u8 {
            if let Some(text) = filled(n) {
                more.push((format!("{chord}{}", crate::slot_badge(n)), one_line(&text, 70)));
            }
        }
    }
    let room = PEEK_MAX_LINES - lines.len();
    if more.len() > room {
        let hidden = more.len() - (room - 1);
        more.truncate(room - 1);
        more.push(("…".to_string(), format!("+{hidden} more slots in Clipd")));
    }
    lines.extend(more);
    lines
}

/// The overlay message for the peek.
pub fn peek_payload(lines: &[(String, String)], at: Option<(f64, f64)>) -> String {
    let mut out = format!("STYLE\tpeek\n{}", at_line(at));
    for (keys, text) in lines {
        out.push_str(&format!("LINE\t{keys}\t{text}\n"));
    }
    out
}

/// The overlay message that takes down a switcher or peek.
pub const HIDE_PAYLOAD: &str = "STYLE\thide\n";

/// The trigger for type-to-paste.
pub const TRIGGER: &str = ";;";
/// Longest query worth matching; anything longer is ordinary typing.
const MAX_QUERY: usize = 32;
/// How much typing the buffer keeps.
const BUFFER_CAP: usize = 48;

/// The last few characters typed, for spotting `;;query`.
///
/// Held in memory only, never logged or stored, and cleared on anything that
/// is not plain typing (a shortcut, an arrow, a click), so it only ever holds
/// the run of characters at the cursor.
#[derive(Debug, Default, Clone)]
pub struct TypedBuffer {
    text: String,
}

impl TypedBuffer {
    pub fn push(&mut self, c: char) {
        self.text.push(c);
        let extra = self.text.chars().count().saturating_sub(BUFFER_CAP);
        if extra > 0 {
            self.text = self.text.chars().skip(extra).collect();
        }
    }

    pub fn backspace(&mut self) {
        self.text.pop();
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// When a space or tab is typed: the query after the last `;;`, and how
    /// many characters the trigger took (`;;` plus the query) — what has to
    /// be erased before pasting. `None` when the run does not end in one.
    pub fn trigger(&self) -> Option<(String, usize)> {
        let at = self.text.rfind(TRIGGER)?;
        let query = &self.text[at + TRIGGER.len()..];
        let ok = !query.is_empty()
            && query.chars().count() <= MAX_QUERY
            && query
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '/' | ':'));
        ok.then(|| (query.to_string(), TRIGGER.len() + query.chars().count()))
    }
}

/// What a query asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// `;;2` — slot 2.
    Slot(u8),
    /// `;;link`, `;;email`, `;;code` — the latest of a kind.
    Kind(ContentType),
    /// Anything else — the latest clip containing it.
    Text(String),
}

pub fn parse_query(q: &str) -> Query {
    let lower = q.to_lowercase();
    if let Ok(n) = lower.parse::<u8>() {
        if (1..=9).contains(&n) {
            return Query::Slot(n);
        }
    }
    match lower.as_str() {
        "link" | "url" => Query::Kind(ContentType::Url),
        "email" | "mail" => Query::Kind(ContentType::Email),
        "code" => Query::Kind(ContentType::Code),
        _ => Query::Text(lower),
    }
}

/// The text a query pastes, from `clips` (newest first) and the slots.
pub fn resolve(
    query: &str,
    clips: &[ClipEntry],
    slot: impl Fn(u8) -> Option<String>,
) -> Option<String> {
    let pasteable = |c: &&ClipEntry| {
        !matches!(c.content_type, ContentType::Image | ContentType::File) && !c.content.trim().is_empty()
    };
    match parse_query(query) {
        Query::Slot(n) => slot(n).filter(|t| !t.trim().is_empty()),
        Query::Kind(kind) => clips
            .iter()
            .filter(pasteable)
            .find(|c| c.content_type == kind)
            .map(|c| c.content.clone()),
        Query::Text(needle) => clips
            .iter()
            .filter(pasteable)
            .find(|c| c.content.to_lowercase().contains(&needle))
            .map(|c| c.content.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(content: &str, app: &str) -> ClipEntry {
        ClipEntry::new(content.to_string(), Some(app.to_string()), None)
    }

    #[test]
    fn the_buffer_spots_a_trigger_and_how_much_to_erase() {
        let mut b = TypedBuffer::default();
        for c in "hi there ;;alex".chars() {
            b.push(c);
        }
        assert_eq!(b.trigger(), Some(("alex".to_string(), 6)));
        b.backspace();
        assert_eq!(b.trigger(), Some(("ale".to_string(), 5)));
        b.clear();
        assert_eq!(b.trigger(), None);
    }

    #[test]
    fn ordinary_typing_never_triggers() {
        let mut b = TypedBuffer::default();
        for c in "for (;;) { x; y }".chars() {
            b.push(c);
        }
        assert_eq!(b.trigger(), None, "a C for-loop is not a query");
        let mut b = TypedBuffer::default();
        for c in ";;".chars() {
            b.push(c);
        }
        assert_eq!(b.trigger(), None, "a bare ;; asks for nothing");
        let mut b = TypedBuffer::default();
        for c in format!(";;{}", "x".repeat(40)).chars() {
            b.push(c);
        }
        assert_eq!(b.trigger(), None, "too long to be a query");
    }

    #[test]
    fn queries_resolve_to_slots_kinds_and_text() {
        let clips = vec![
            clip("see you at 5", "Messages"),
            clip("https://github.com/shwetarkadam/clipd/pull/28", "Brave Browser"),
            clip("alex@morgan.dev", "Mail"),
            clip("Alex Morgan, 12 High St", "Notes"),
        ];
        let clips: Vec<ClipEntry> = clips;
        let slot = |n: u8| (n == 2).then(|| "slot two".to_string());
        assert_eq!(resolve("2", &clips, slot).as_deref(), Some("slot two"));
        assert_eq!(resolve("3", &clips, slot), None);
        assert_eq!(
            resolve("link", &clips, slot).as_deref(),
            Some("https://github.com/shwetarkadam/clipd/pull/28")
        );
        assert_eq!(resolve("email", &clips, slot).as_deref(), Some("alex@morgan.dev"));
        // Newest match wins, case-insensitive.
        assert_eq!(resolve("ALEX", &clips, slot).as_deref(), Some("alex@morgan.dev"));
        assert_eq!(resolve("nothing like this", &clips, slot), None);
    }

    #[test]
    fn the_switcher_offers_each_text_once_and_starts_one_back() {
        let clips = vec![
            clip("latest", "Slack"),
            clip("older", "Brave Browser"),
            clip("latest", "Slack"),
            clip("", "Notes"),
        ];
        let items = switcher_items(&clips);
        assert_eq!(items.iter().map(|i| i.preview.as_str()).collect::<Vec<_>>(), ["latest", "older"]);
        assert_eq!(first_selection(items.len()), 1, "the first tap lands one back, like ⌘Tab");
        assert_eq!(first_selection(1), 0);
        assert_eq!(next_selection(1, 2), 0);
        let payload = switcher_payload(&items, 1, Some((100.0, 200.0)));
        assert!(payload.starts_with("STYLE\tswitcher\nAT\t100\t200\nSEL\t1\n"));
        assert!(payload.contains("CARD\t2\ttext\tBrave Browser\t\tolder\n"));
    }

    #[test]
    fn the_peek_shows_slots_only_when_they_apply() {
        let slot = |n: u8| (n == 2).then(|| "second\nline".to_string());
        let lines = peek_lines(Some("now"), slot, true, None);
        assert_eq!(lines, vec![("⌘V".into(), "now".into()), ("⌘V ×2".into(), "second line".into())]);
        assert_eq!(peek_lines(None, slot, false, None).len(), 1);
        assert!(peek_payload(&lines, None).contains("LINE\t⌘V ×2\tsecond line\n"));
    }

    #[test]
    fn the_peek_shows_filled_letter_slots_with_the_chord_that_pastes_them() {
        // A = 31, D = 34.
        let slot = |n: u8| match n {
            5 => Some("five".to_string()),
            31 => Some("alpha".to_string()),
            34 => Some("delta".to_string()),
            _ => None,
        };
        let lines = peek_lines(Some("now"), slot, true, Some("⌃⌥"));
        let keys: Vec<&str> = lines.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["⌘V", "⌘V ×5", "⌃⌥A", "⌃⌥D"]);
        // The leader chord when the direct one is off.
        let leader = peek_lines(Some("now"), slot, false, Some("⌘⌥V "));
        assert_eq!(leader[1], ("⌘⌥V A".to_string(), "alpha".to_string()));
        // Letters off: none shown.
        assert_eq!(peek_lines(Some("now"), slot, false, None).len(), 1);
    }

    #[test]
    fn a_full_peek_stays_short() {
        let every = |_n: u8| Some("x".to_string());
        let lines = peek_lines(Some("now"), every, true, Some("⌃⌥"));
        assert_eq!(lines.len(), PEEK_MAX_LINES);
        // 8 numbered + 26 letters = 34; 10 shown, the rest summarised.
        assert_eq!(lines.last().unwrap().1, "+24 more slots in Clipd");
    }
}
