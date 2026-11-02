//! Letter slots that fill themselves, and letters you give a clip yourself.
//!
//! Asking someone to pick a letter at the moment they copy — a chord, a hold,
//! a tap, then "which letter?" — interrupts exactly the work they are doing,
//! and people disliked every version of it. What letter slots are really for
//! is things you reuse: your email, a link you keep sending, a phone number,
//! a snippet. So:
//!
//! - **Automatic letters.** The latest copy of a kind keeps its letter fresh
//!   with no step at all: **E** the latest email, **L** the latest link, **P**
//!   the latest phone number, **K** the latest code. (Not C: that is ⌘C.)
//! - **Named letters.** Anything worth keeping can be given a letter later,
//!   from Clipd (select it, ⌥ + a letter). A letter you named is yours: the
//!   automatic letters never overwrite it.
//!
//! Pasting is unchanged: ⌃⌥ + the letter, `;;e`, or tap ⌘ twice then the
//! letter. Secrets are never put in a letter automatically.

use crate::models::ContentType;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Letter slots are stored as 31..=56 (A..=Z); see `slot_badge`.
pub fn letter_slot(letter: char) -> Option<u8> {
    let l = letter.to_ascii_uppercase();
    l.is_ascii_uppercase().then(|| 31 + (l as u8 - b'A'))
}

/// The automatic letters, and what keeps each one fresh.
pub const AUTO_LETTERS: [(char, &str); 4] = [
    ('E', "email"),
    ('L', "link"),
    ('P', "phone number"),
    ('K', "code"),
];

/// The automatic letter a copy refreshes, if any.
pub fn auto_letter_for(content: &str, kind: &ContentType) -> Option<char> {
    match kind {
        ContentType::Email => Some('E'),
        ContentType::Url => Some('L'),
        ContentType::Code => Some('K'),
        _ if looks_like_phone(content) => Some('P'),
        _ => None,
    }
}

/// A phone number: 7–15 digits written the way phone numbers are — a leading
/// +, or spaces / dashes / dots / brackets between groups. A bare run of six
/// to eight digits is far more often a one-time code than a phone number, so
/// it does not count unless it is long enough to be one (10+).
pub fn looks_like_phone(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.len() > 24 {
        return false;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '.' | '(' | ')'))
    {
        return false;
    }
    if t.chars().skip(1).any(|c| c == '+') {
        return false;
    }
    let digits = t.chars().filter(char::is_ascii_digit).count();
    if !(7..=15).contains(&digits) {
        return false;
    }
    let formatted = t.starts_with('+') || t.chars().any(|c| matches!(c, ' ' | '-' | '.' | '(' | ')'));
    formatted || digits >= 10
}

fn named_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("named_letters.json")
}

/// Letters the person named themselves. A file of its own rather than a
/// settings field: the window and the daemon both write settings, and a
/// stale copy saved over a fresh one would quietly forget a name.
pub fn named_letters() -> BTreeSet<char> {
    std::fs::read_to_string(named_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Record that `letter` was named by hand, so automatic letters leave it.
pub fn mark_named(letter: char) {
    let mut named = named_letters();
    if named.insert(letter.to_ascii_uppercase()) {
        let path = named_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string(&named) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Give a letter back to the automatic letters (or just stop treating it as
/// named).
pub fn unmark_named(letter: char) {
    let mut named = named_letters();
    if named.remove(&letter.to_ascii_uppercase()) {
        if let Ok(json) = serde_json::to_string(&named) {
            let _ = std::fs::write(named_path(), json);
        }
    }
}

/// Once, on the first run with automatic letters: anything already in E, L,
/// P or K was put there by hand, so it is treated as named and kept. Without
/// this, the first email copied after updating would replace whatever
/// someone had deliberately saved in E.
pub fn protect_existing(slot_holds: impl Fn(u8) -> bool) {
    let marker = named_path().with_file_name("auto_letters_adopted");
    if marker.exists() {
        return;
    }
    for (letter, _) in AUTO_LETTERS {
        if letter_slot(letter).is_some_and(&slot_holds) {
            mark_named(letter);
        }
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(marker, b"");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_map_to_their_letters() {
        assert_eq!(auto_letter_for("ada@example.com", &ContentType::Email), Some('E'));
        assert_eq!(auto_letter_for("https://clipd.sh", &ContentType::Url), Some('L'));
        assert_eq!(auto_letter_for("fn main() {}", &ContentType::Code), Some('K'));
        assert_eq!(auto_letter_for("+91 98765 43210", &ContentType::Text), Some('P'));
        assert_eq!(auto_letter_for("hello there", &ContentType::Text), None);
        assert_eq!(letter_slot('e'), Some(35));
        assert_eq!(letter_slot('K'), Some(41));
        assert_eq!(letter_slot('1'), None);
    }

    #[test]
    fn phone_numbers_but_not_codes_or_numbers() {
        for yes in ["+1 415 555 0132", "(415) 555-0132", "415-555-0132", "+919876543210", "9876543210", "020 7946 0958"] {
            assert!(looks_like_phone(yes), "{yes}");
        }
        for no in ["483920", "12345678", "3.14159", "2026", "1,234", "+1+2", "call me", "", "12-34"] {
            assert!(!looks_like_phone(no), "{no}");
        }
    }
}
