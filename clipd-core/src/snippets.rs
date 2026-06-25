//! User-authored reusable text (signatures, boilerplate, prompt templates).
//!
//! Unlike captured clips, snippets are created by the user and recalled by a
//! short `trigger`. clipd surfaces a matching snippet at the top of the search
//! palette, so typing the trigger and pressing Enter pastes its body — a
//! lightweight "text expander" without a system-wide keystroke hook.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct Snippet {
    pub id: i64,
    /// Short keyword used to recall the snippet (e.g. "sig", "addr").
    pub trigger: String,
    /// Optional human label; falls back to the trigger when empty.
    pub name: String,
    /// The text that gets pasted.
    pub body: String,
    pub created_at: DateTime<Utc>,
}

impl Snippet {
    /// A one-line preview of the body for list rows.
    pub fn preview(&self) -> String {
        let oneline = self.body.trim().replace(['\n', '\t'], " ");
        if oneline.chars().count() > 80 {
            let mut s: String = oneline.chars().take(80).collect();
            s.push('…');
            s
        } else {
            oneline
        }
    }

    /// Display label — the name if set, otherwise the trigger.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.trigger
        } else {
            &self.name
        }
    }
}
