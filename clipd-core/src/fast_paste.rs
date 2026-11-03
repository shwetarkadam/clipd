//! Faster ways to paste, without opening a window.
//!
//! Most of the time someone goes back for a clip it is not a search: it is
//! "the thing I copied a minute ago in the other tab". These gestures cover
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
//! Copying into a letter slot is ⌘C ⌘C then the letter, with the other ⌘C
//! taps in the daemon; E, L, P and K also fill themselves (see
//! `crate::auto_letters`).
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
/// ⌘V, so their letter slots are reached with ⌃⌥C / ⌃⌥V.
pub fn peek_slot_for_key(key: char) -> Option<u8> {
    match key.to_ascii_lowercase() {
        d @ '1'..='9' => Some(d as u8 - b'0'),
        'c' | 'v' => None,
        l @ 'a'..='z' => Some(LETTER_SLOT_A + (l as u8 - b'a')),
        _ => None,
    }
}

/// How the peek labels `slot`: the key to press with ⌘ held, or — for the
/// two letters ⌘ cannot pick, since ⌘C and ⌘V stay copy and paste — the
/// chord that pastes them.
pub fn peek_key_label(slot: u8) -> String {
    let badge = crate::slot_badge(slot);
    match badge.as_str() {
        "C" | "V" => format!("⌃⌥{badge}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(content: &str, app: &str) -> ClipEntry {
        ClipEntry::new(content.to_string(), Some(app.to_string()), None)
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
        assert_eq!(keys(&lines), ["⌘ +", "V", "2", "5", "A", "⌃⌥C", "D"]);
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
        assert_eq!(peek_key_label(52), "⌃⌥V");
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
