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
//!   lists what is in your slots, each with the key that picks it. Keep
//!   holding ⌘ and press that key (`2`, `A`) to paste the slot.
//! - **Picker** — the same list without the wait: tap ⌘ twice, then press a
//!   slot's key. It opens at once and takes a bare key (C and V included),
//!   because a deliberate tap is not something any shortcut does.
//!
//! Saving into a letter is not a gesture here: asking for a letter at the
//! moment of copying interrupted people. Letters fill themselves or are
//! given later (see `crate::auto_letters`).
//! - **Type to paste** — type `;;` and a few letters, then a space, and the
//!   trigger is replaced by the matching clip: `;;alex` → the latest clip
//!   with "alex" in it, `;;link` / `;;email` / `;;code` → the latest of that
//!   kind. Slots by name: `;;2` / `;;20` → numbered slot 2 / 20, `;;a` →
//!   letter slot A — the one keyboard path to a slot past 9.
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

/// Letter slots are stored as 31..=56 (A..=Z); see `slot_badge`.
const LETTER_SLOT_A: u8 = 31;

/// The slot a bare key picks in the picker (opened by tapping ⌘): `1`–`9`
/// the numbered slots, any letter its letter slot — C and V too, since no
/// ⌘ is held.
pub fn picker_slot_for_key(key: char) -> Option<u8> {
    match key.to_ascii_lowercase() {
        d @ '1'..='9' => Some(d as u8 - b'0'),
        l @ 'a'..='z' => Some(LETTER_SLOT_A + (l as u8 - b'a')),
        _ => None,
    }
}

/// The slot a key picks while the peek is open and ⌘ is held: `1`–`9` the
/// numbered slots, a letter its letter slot. Not C or V — those stay ⌘C and
/// ⌘V, so their letter slots are reached with `;;c` / `;;v` (or ⌃⌥C / ⌃⌥V).
pub fn peek_slot_for_key(key: char) -> Option<u8> {
    match key.to_ascii_lowercase() {
        d @ '1'..='9' => Some(d as u8 - b'0'),
        'c' | 'v' => None,
        l @ 'a'..='z' => Some(LETTER_SLOT_A + (l as u8 - b'a')),
        _ => None,
    }
}

/// How the peek labels `slot`: the key to press with ⌘ held, or the `;;`
/// name for the two letters ⌘ cannot pick.
pub fn peek_key_label(slot: u8) -> String {
    let badge = crate::slot_badge(slot);
    match badge.as_str() {
        "C" | "V" => format!(";;{}", badge.to_lowercase()),
        _ => badge,
    }
}

/// What the peek says, one (key, text) line each: the clipboard (⌘V), then
/// every numbered slot that holds something (when multi-slot is on) and every
/// letter slot that does (when letter slots are on), each labelled with the
/// key that pastes it. Empty slots are left out: it answers "what have I
/// got", not "what could I".
///
/// `picker`: opened by a ⌘ tap rather than a hold, so keys are pressed bare
/// and every letter is labelled as itself (C and V included).
pub fn peek_lines(
    picker: bool,
    clipboard: Option<&str>,
    slot: impl Fn(u8) -> Option<String>,
    multi_slot: bool,
    letters: bool,
) -> Vec<(String, String)> {
    let now = clipboard
        .map(|c| one_line(c, 70))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "(nothing on the clipboard)".to_string());
    let (lead, how) = if picker {
        ("press", "· esc closes")
    } else {
        ("⌘ +", "")
    };
    let ask = if picker { "a key to paste that slot" } else { "keep holding, press a key to paste" };
    let mut lines = vec![
        (lead.to_string(), format!("{ask} {how}").trim().to_string()),
        (if picker { "⌘V" } else { "V" }.to_string(), now),
    ];
    let label = |n: u8| if picker { crate::slot_badge(n) } else { peek_key_label(n) };
    let filled = |n: u8| slot(n).filter(|t| !t.trim().is_empty());
    let mut more = Vec::new();
    if multi_slot {
        for n in 2..=9u8 {
            if let Some(text) = filled(n) {
                more.push((label(n), one_line(&text, 70)));
            }
        }
    }
    if letters {
        for n in LETTER_SLOT_A..=LETTER_SLOT_A + 25 {
            if let Some(text) = filled(n) {
                more.push((label(n), one_line(&text, 70)));
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
    /// `;;2`, `;;20` — a numbered slot (1–30); `;;a` — a letter slot, stored
    /// as 31–56 (see `slot_badge`).
    Slot(u8),
    /// `;;link`, `;;email`, `;;code` — the latest of a kind.
    Kind(ContentType),
    /// Anything else — the latest clip containing it.
    Text(String),
}

pub fn parse_query(q: &str) -> Query {
    let lower = q.to_lowercase();
    // Numbered slots run 1–30 (⌘V ×N reaches 9; 11–30 are the extended bank).
    if let Ok(n) = lower.parse::<u8>() {
        if (1..=30).contains(&n) {
            return Query::Slot(n);
        }
    }
    // One letter is never a useful search (it matches nearly every clip), so
    // it names a letter slot.
    let mut chars = lower.chars();
    if let (Some(c @ 'a'..='z'), None) = (chars.next(), chars.next()) {
        return Query::Slot(31 + (c as u8 - b'a'));
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
        let slot = |n: u8| match n {
            2 => Some("slot two".to_string()),
            20 => Some("slot twenty".to_string()),
            31 => Some("letter a".to_string()),
            _ => None,
        };
        assert_eq!(resolve("2", &clips, slot).as_deref(), Some("slot two"));
        assert_eq!(resolve("3", &clips, slot), None);
        // Past ⌘V's reach, and the letter bank.
        assert_eq!(resolve("20", &clips, slot).as_deref(), Some("slot twenty"));
        assert_eq!(resolve("a", &clips, slot).as_deref(), Some("letter a"));
        assert_eq!(resolve("A", &clips, slot).as_deref(), Some("letter a"));
        assert_eq!(parse_query("z"), Query::Slot(56));
        assert_eq!(parse_query("31"), Query::Text("31".into()), "31+ are letter slots by number");
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

    fn keys(lines: &[(String, String)]) -> Vec<&str> {
        lines.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn the_paste_peek_labels_each_filled_slot_with_its_key() {
        // A = 31, C = 33, D = 34.
        let slot = |n: u8| match n {
            2 => Some("second\nline".to_string()),
            5 => Some("five".to_string()),
            31 => Some("alpha".to_string()),
            33 => Some("charlie".to_string()),
            34 => Some("delta".to_string()),
            _ => None,
        };
        let lines = peek_lines(false, Some("now"), slot, true, true);
        assert_eq!(keys(&lines), ["⌘ +", "V", "2", "5", "A", ";;c", "D"]);
        assert_eq!(lines[2].1, "second line");
        // Numbered slots only with multi-slot on; letters only with letters on.
        assert_eq!(keys(&peek_lines(false, Some("now"), slot, false, false)), ["⌘ +", "V"]);
        assert!(peek_payload(&lines, None).contains("LINE\tA\talpha\n"));
    }

    #[test]
    fn keys_pick_slots_but_c_and_v_stay_copy_and_paste() {
        assert_eq!(peek_slot_for_key('3'), Some(3));
        assert_eq!(peek_slot_for_key('a'), Some(31));
        assert_eq!(peek_slot_for_key('Z'), Some(56));
        assert_eq!(peek_slot_for_key('c'), None);
        assert_eq!(peek_slot_for_key('v'), None);
        assert_eq!(peek_slot_for_key('0'), None);
        assert_eq!(peek_key_label(54), "X");
        assert_eq!(peek_key_label(52), ";;v");
    }

    #[test]
    fn the_picker_takes_bare_keys_c_and_v_included() {
        assert_eq!(picker_slot_for_key('c'), Some(33));
        assert_eq!(picker_slot_for_key('V'), Some(52));
        assert_eq!(picker_slot_for_key('4'), Some(4));
        assert_eq!(picker_slot_for_key(' '), None);
        let slot = |n: u8| (n == 33).then(|| "charlie".to_string());
        let lines = peek_lines(true, Some("now"), slot, true, true);
        assert_eq!(keys(&lines), ["press", "⌘V", "C"], "C is just C in the picker");
        assert!(lines[0].1.ends_with("· esc closes"));
    }

    #[test]
    fn a_full_peek_stays_short() {
        let every = |_n: u8| Some("x".to_string());
        let lines = peek_lines(false, Some("now"), every, true, true);
        assert_eq!(lines.len(), PEEK_MAX_LINES);
        // 2 header lines + 8 numbered + 26 letters = 34 slots; 9 shown.
        assert_eq!(lines.last().unwrap().1, "+25 more slots in Clipd");
    }
}
