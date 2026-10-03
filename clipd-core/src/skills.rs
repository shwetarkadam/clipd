//! Skills from your clipboard — workflows you repeat, written down for agents.
//!
//! A clipboard history is a record of how someone works: the build command,
//! then the tag, then the push; the job link, then the email, then the phone
//! number for the form. When the same handful of copies turns up together in
//! several separate work sessions, that is a workflow, and it can be written
//! down as an agent skill (a `SKILL.md` that Claude Code and other agents
//! load) and as clipd snippets.
//!
//! Everything here is local and deterministic: pattern-matching over the
//! history already on disk, no model and no network. Nothing is written until
//! the person reviews the skill and says yes. Secrets never make it in — a
//! clip the secret detector flags, or that looks like a password, is not a
//! step.

use crate::models::{ClipEntry, ContentType};
use crate::privacy::{detect_sensitive, looks_like_password, PrivacyConfig};
use crate::session::compute_sessions;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A workflow needs at least this many distinct steps…
const MIN_STEPS: usize = 3;
/// …seen together in at least this many separate work sessions.
const MIN_SESSIONS: usize = 2;
/// Longer workflows are cut here; a skill is a checklist, not a transcript.
const MAX_STEPS: usize = 12;
/// A clip longer than this is a document, not a step.
const MAX_STEP_CHARS: usize = 1500;
/// At most this many suggestions come out of one scan.
const MAX_CANDIDATES: usize = 5;

/// What kind of thing a step is, which decides how it is written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// A shell command: run it.
    Command,
    /// A link: open it.
    Link,
    /// Code: use it.
    Code,
    /// Anything else: paste it.
    Text,
}

/// One step of a workflow.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillStep {
    pub kind: StepKind,
    /// The most recent version of this step, exactly as copied.
    pub text: String,
    /// True when the step was not the same text every time — a version
    /// number or an ID that changes between runs.
    pub varies: bool,
}

/// A workflow found in the history, ready to review.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillCandidate {
    /// Stable identity of the workflow (its normalised steps), so a skill the
    /// person dismissed or already saved is not offered again.
    pub signature: String,
    /// Folder-safe name: `cargo-git`, `linkedin-apply`.
    pub name: String,
    /// Human title: "Cargo & git workflow".
    pub title: String,
    pub steps: Vec<SkillStep>,
    /// Separate work sessions this workflow turned up in.
    pub sessions: usize,
    pub last_seen: DateTime<Utc>,
    /// Apps the steps were copied from, most frequent first.
    pub apps: Vec<String>,
}

/// Find workflows repeated across work sessions.
///
/// `clips` is a copy timeline, newest first — `ClipStore::copy_timeline`, where
/// a clip copied three times appears three times. `session_minutes` is the
/// gap that ends a work session.
pub fn find_skill_candidates(
    clips: &[ClipEntry],
    privacy: &PrivacyConfig,
    session_minutes: i64,
) -> Vec<SkillCandidate> {
    let usable: Vec<&ClipEntry> = clips.iter().filter(|clip| usable_step(clip, privacy)).collect();
    if usable.len() < MIN_STEPS * MIN_SESSIONS {
        return Vec::new();
    }
    // One entry per copy (see `ClipStore::copy_timeline`), so the same clip
    // can appear many times; each copy gets its own id here.
    let owned: Vec<ClipEntry> = usable
        .iter()
        .enumerate()
        .map(|(i, clip)| {
            let mut copy = (*clip).clone();
            copy.id = i as i64;
            copy
        })
        .collect();
    let by_id: HashMap<i64, &ClipEntry> = owned.iter().map(|clip| (clip.id, clip)).collect();

    // Each session as its ordered, de-duplicated step keys (oldest first).
    let sessions: Vec<Vec<(String, &ClipEntry)>> = compute_sessions(&owned, session_minutes)
        .into_iter()
        .map(|session| {
            let mut seen = HashSet::new();
            let mut steps: Vec<(String, &ClipEntry)> = session
                .clip_ids
                .iter()
                .rev()
                .filter_map(|id| by_id.get(id).copied())
                .filter_map(|clip| {
                    let key = step_key(&clip.content);
                    seen.insert(key.clone()).then_some((key, clip))
                })
                .collect();
            steps.truncate(200);
            steps
        })
        .filter(|steps| steps.len() >= MIN_STEPS)
        .collect();
    let key_sets: Vec<HashSet<&str>> = sessions
        .iter()
        .map(|steps| steps.iter().map(|(key, _)| key.as_str()).collect())
        .collect();

    // Every pair of sessions proposes what they share, in the order the
    // earlier-listed session did it.
    let mut proposals: HashMap<Vec<String>, ()> = HashMap::new();
    for i in 0..sessions.len() {
        for j in (i + 1)..sessions.len() {
            let shared: Vec<String> = sessions[i]
                .iter()
                .filter(|(key, _)| key_sets[j].contains(key.as_str()))
                .map(|(key, _)| key.clone())
                .take(MAX_STEPS)
                .collect();
            if shared.len() >= MIN_STEPS {
                proposals.insert(shared, ());
            }
        }
    }

    let mut candidates: Vec<(Vec<String>, Vec<usize>)> = proposals
        .into_keys()
        .map(|keys| {
            let containing: Vec<usize> = (0..sessions.len())
                .filter(|&s| keys.iter().all(|key| key_sets[s].contains(key.as_str())))
                .collect();
            (keys, containing)
        })
        .filter(|(_, containing)| containing.len() >= MIN_SESSIONS)
        .collect();
    // Most steps first, then most often; a smaller workflow inside a kept one
    // is the same workflow and is dropped.
    candidates.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(b.1.len().cmp(&a.1.len())));
    let mut kept: Vec<(Vec<String>, Vec<usize>)> = Vec::new();
    for (keys, containing) in candidates {
        let inside_kept = kept.iter().any(|(bigger, _)| {
            let bigger: HashSet<&String> = bigger.iter().collect();
            keys.iter().filter(|key| bigger.contains(key)).count() * 2 > keys.len()
        });
        if !inside_kept {
            kept.push((keys, containing));
        }
        if kept.len() == MAX_CANDIDATES {
            break;
        }
    }

    kept.into_iter()
        .map(|(keys, containing)| build_candidate(&keys, &containing, &sessions))
        .collect()
}

fn build_candidate(
    keys: &[String],
    containing: &[usize],
    sessions: &[Vec<(String, &ClipEntry)>],
) -> SkillCandidate {
    let mut apps: HashMap<String, usize> = HashMap::new();
    let mut last_seen = DateTime::<Utc>::MIN_UTC;
    let steps: Vec<SkillStep> = keys
        .iter()
        .map(|key| {
            let versions: Vec<&ClipEntry> = containing
                .iter()
                .filter_map(|&s| sessions[s].iter().find(|(k, _)| k == key).map(|(_, clip)| *clip))
                .collect();
            for clip in &versions {
                if let Some(app) = clip.source_app.as_deref().filter(|a| !a.trim().is_empty()) {
                    *apps.entry(app.to_string()).or_default() += 1;
                }
                last_seen = last_seen.max(clip.timestamp);
            }
            let newest = versions.iter().max_by_key(|clip| clip.timestamp).copied();
            let text = newest.map(|clip| clip.content.trim().to_string()).unwrap_or_default();
            let varies = versions.iter().any(|clip| clip.content.trim() != text);
            let kind = newest.map(step_kind).unwrap_or(StepKind::Text);
            SkillStep { kind, text, varies }
        })
        .collect();
    let mut apps: Vec<(String, usize)> = apps.into_iter().collect();
    apps.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let (name, title) = name_for(&steps);
    SkillCandidate {
        signature: signature_of(keys),
        name,
        title,
        steps,
        sessions: containing.len(),
        last_seen,
        apps: apps.into_iter().map(|(app, _)| app).take(3).collect(),
    }
}

/// A clip that can be a step: text-like, not too long, and not a secret.
fn usable_step(clip: &ClipEntry, privacy: &PrivacyConfig) -> bool {
    let text = clip.content.trim();
    matches!(
        clip.content_type,
        ContentType::Text | ContentType::Code | ContentType::Url | ContentType::Path | ContentType::Email | ContentType::Unknown
    ) && text.chars().count() >= 3
        && text.chars().count() <= MAX_STEP_CHARS
        && detect_sensitive(text, privacy).is_empty()
        && !looks_like_password(text)
}

/// The identity of a step across runs: case and spacing folded; for commands
/// and links, digits and hashes generalised and a URL's query dropped, so
/// `git tag v0.4.20` and `git tag v0.4.21` are the same step. Plain text is
/// matched as written — "note 2" and "note 5" are different notes, while the
/// email and phone number a form takes every time are the same.
pub fn step_key(text: &str) -> String {
    let mut text = text.trim().to_lowercase();
    let is_link = text.starts_with("http://") || text.starts_with("https://");
    if is_link {
        if let Some(cut) = text.find(['?', '#']) {
            text.truncate(cut);
        }
    }
    if !is_link && !is_command(&text) {
        return text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            let hexish = word.len() >= 7
                && word.chars().all(|c| c.is_ascii_hexdigit())
                && word.chars().any(|c| c.is_ascii_digit());
            if hexish {
                return "#".to_string();
            }
            let mut out = String::with_capacity(word.len());
            let mut in_digits = false;
            for c in word.chars() {
                if c.is_ascii_digit() {
                    if !in_digits {
                        out.push('#');
                    }
                    in_digits = true;
                } else {
                    out.push(c);
                    in_digits = false;
                }
            }
            out
        })
        .collect();
    words.join(" ")
}

const COMMANDS: &[&str] = &[
    "git", "cargo", "npm", "npx", "pnpm", "yarn", "bun", "node", "deno", "python", "python3",
    "pip", "pip3", "uv", "docker", "kubectl", "helm", "terraform", "brew", "curl", "wget", "ssh",
    "scp", "rsync", "make", "go", "rustup", "gh", "aws", "gcloud", "az", "psql", "mysql",
    "redis-cli", "sqlite3", "cd", "ls", "cat", "grep", "sed", "awk", "open", "swift",
    "xcodebuild", "vercel", "wrangler", "flyctl", "heroku", "supabase", "sudo", "chmod", "mkdir",
    "rm", "cp", "mv", "export", "source", "tar", "unzip", "codesign", "tccutil", "defaults",
    "launchctl", "pkill", "kill", "ps", "claude",
];

fn first_word(text: &str) -> &str {
    let text = text.trim().trim_start_matches("$ ");
    text.split_whitespace().next().unwrap_or("")
}

fn is_command(text: &str) -> bool {
    let line_count = text.trim().lines().count();
    let word = first_word(text);
    line_count <= 3
        && (COMMANDS.contains(&word) || word.starts_with("./") || text.trim().starts_with("$ "))
}

fn step_kind(clip: &ClipEntry) -> StepKind {
    let text = clip.content.trim();
    if is_command(text) {
        StepKind::Command
    } else if clip.content_type == ContentType::Url {
        StepKind::Link
    } else if clip.content_type == ContentType::Code {
        StepKind::Code
    } else {
        StepKind::Text
    }
}

/// A name from what the steps are about: the tools the commands use, else the
/// sites the links go to, else the first words of the first step.
fn name_for(steps: &[SkillStep]) -> (String, String) {
    fn push(parts: &mut Vec<String>, part: String) {
        if !part.is_empty() && !parts.contains(&part) && parts.len() < 2 {
            parts.push(part);
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for step in steps.iter().filter(|s| s.kind == StepKind::Command) {
        push(&mut parts, first_word(&step.text).trim_start_matches("./").to_lowercase());
    }
    if parts.is_empty() {
        for step in steps.iter().filter(|s| s.kind == StepKind::Link) {
            push(&mut parts, site_of(&step.text));
        }
    }
    if parts.is_empty() {
        if let Some(step) = steps.first() {
            for word in step.text.split_whitespace().take(2) {
                push(&mut parts, word.to_lowercase());
            }
        }
    }
    let slug_parts: Vec<String> = parts.iter().map(|p| slug(p)).filter(|p| !p.is_empty()).collect();
    let name = if slug_parts.is_empty() {
        "clipboard-workflow".to_string()
    } else {
        format!("{}-workflow", slug_parts.join("-"))
    };
    let title = if parts.is_empty() {
        "Clipboard workflow".to_string()
    } else {
        let words: Vec<String> = parts.iter().map(|p| capitalise(p)).collect();
        format!("{} workflow", words.join(" & "))
    };
    (name.chars().take(48).collect(), title)
}

fn site_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.trim_start_matches("www.");
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() >= 2 {
        labels[labels.len() - 2].to_string()
    } else {
        host.to_string()
    }
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn signature_of(keys: &[String]) -> String {
    // FNV-1a: stable across runs and builds, unlike the std hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in keys.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A one-line summary of a step for the suggestion and the description.
pub fn step_summary(step: &SkillStep, max: usize) -> String {
    let line = step.text.lines().next().unwrap_or("").trim();
    let line = match step.kind {
        StepKind::Link => line.split("://").nth(1).unwrap_or(line),
        _ => line,
    };
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max.saturating_sub(1)).collect::<String>().trim_end())
    } else {
        line.to_string()
    }
}

/// The skill as a `SKILL.md`: frontmatter agents read to decide when to use
/// it, then the steps in order.
pub fn render_skill_md(candidate: &SkillCandidate, name: &str) -> String {
    let flow: Vec<String> = candidate.steps.iter().take(4).map(|step| step_summary(step, 28)).collect();
    let more = if candidate.steps.len() > 4 { ", and more" } else { "" };
    let description = format!(
        "{}: {}{more}. A sequence the user has repeated in {} separate work sessions. Use when they want to do it again.",
        candidate.title,
        flow.join(" → "),
        candidate.sessions,
    )
    .replace('\n', " ");
    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("name: {name}\n"));
    md.push_str(&format!("description: {}\n", yaml_scalar(&description)));
    md.push_str("---\n\n");
    md.push_str(&format!("# {}\n\n", candidate.title));
    let apps = if candidate.apps.is_empty() {
        String::new()
    } else {
        format!(", in {}", candidate.apps.join(", "))
    };
    md.push_str(&format!(
        "These steps came up together in {} separate work sessions (most recently {}{apps}). \
         clipd wrote this file on this Mac from the clipboard history; nothing was sent anywhere.\n\n",
        candidate.sessions,
        candidate.last_seen.format("%-d %b %Y"),
    ));
    md.push_str("## Steps\n\n");
    for (i, step) in candidate.steps.iter().enumerate() {
        let n = i + 1;
        let note = if step.varies { " _(this changes between runs — this is the latest)_" } else { "" };
        match step.kind {
            StepKind::Command => {
                md.push_str(&format!("{n}. Run{note}:\n\n"));
                md.push_str(&fenced(&step.text, "bash"));
            }
            StepKind::Code => {
                md.push_str(&format!("{n}. Use this code{note}:\n\n"));
                md.push_str(&fenced(&step.text, ""));
            }
            StepKind::Link => {
                md.push_str(&format!("{n}. Open <{}>{note}\n\n", step.text.trim()));
            }
            StepKind::Text => {
                md.push_str(&format!("{n}. Paste{note}:\n\n"));
                for line in step.text.lines() {
                    md.push_str(&format!("   > {line}\n"));
                }
                md.push('\n');
            }
        }
    }
    if candidate.steps.iter().any(|step| step.varies) {
        md.push_str("## Notes\n\n");
        md.push_str(
            "- Steps marked as changing between runs held a different version number, ID or \
             date each time. Check them before running.\n",
        );
    }
    md
}

/// An indented fenced block that cannot be closed early by the content.
fn fenced(text: &str, lang: &str) -> String {
    let fence = if text.contains("```") { "~~~~" } else { "```" };
    let mut out = format!("   {fence}{lang}\n");
    for line in text.trim().lines() {
        out.push_str(&format!("   {line}\n"));
    }
    out.push_str(&format!("   {fence}\n\n"));
    out
}

/// A YAML string that survives colons, quotes and leading symbols.
fn yaml_scalar(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Make a user-typed name safe for a folder and the frontmatter.
pub fn clean_skill_name(name: &str) -> String {
    let cleaned = slug(name);
    if cleaned.is_empty() {
        "clipboard-workflow".to_string()
    } else {
        cleaned.chars().take(48).collect()
    }
}

/// Where agents look for personal skills: `~/.claude/skills`.
pub fn agent_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("skills"))
}

/// Write `SKILL.md` into `<dir>/<name>/`. Refuses to overwrite a skill that is
/// already there — a name clash is the person's to resolve, not ours.
pub fn save_skill(dir: &Path, name: &str, md: &str) -> Result<PathBuf, String> {
    let folder = dir.join(name);
    let file = folder.join("SKILL.md");
    if file.exists() {
        return Err(format!("A skill named \"{name}\" already exists. Pick another name."));
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("Couldn't create {}: {e}", folder.display()))?;
    std::fs::write(&file, md).map_err(|e| format!("Couldn't write {}: {e}", file.display()))?;
    Ok(file)
}

/// Which workflows have been answered: saved, or "don't suggest this".
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SkillState {
    #[serde(default)]
    pub dismissed: Vec<String>,
    #[serde(default)]
    pub created: Vec<String>,
}

impl SkillState {
    pub fn answered(&self, signature: &str) -> bool {
        self.dismissed.iter().chain(&self.created).any(|s| s == signature)
    }
}

fn state_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("skills.json")
}

pub fn load_skill_state() -> SkillState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_skill_state(state: &SkillState) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn clip(id: i64, text: &str, kind: ContentType, at: DateTime<Utc>) -> ClipEntry {
        let mut c = ClipEntry::new(text.to_string(), Some("Terminal".into()), None);
        c.id = id;
        c.content_type = kind;
        c.timestamp = at;
        c
    }

    /// Newest first, like the store: three release sessions a day apart, with
    /// noise, plus a secret that must never become a step.
    fn history() -> Vec<ClipEntry> {
        let base = Utc::now() - Duration::days(10);
        let mut clips = Vec::new();
        let mut id = 0;
        for (day, version) in [(0, "0.4.19"), (2, "0.4.20"), (5, "0.4.21")] {
            let t = base + Duration::days(day);
            for (minute, text, kind) in [
                (0, "cargo build --release".to_string(), ContentType::Code),
                (2, format!("git tag v{version}"), ContentType::Text),
                (3, "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789".to_string(), ContentType::Text),
                (4, format!("git push origin v{version}"), ContentType::Text),
                (6, "https://github.com/acme/app/releases".to_string(), ContentType::Url),
                (7, format!("unrelated note {day}"), ContentType::Text),
            ] {
                id += 1;
                clips.push(clip(id, &text, kind, t + Duration::minutes(minute)));
            }
        }
        clips.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        clips
    }

    #[test]
    fn a_workflow_repeated_across_sessions_is_found() {
        let found = find_skill_candidates(&history(), &PrivacyConfig::default(), 30);
        assert_eq!(found.len(), 1, "{found:#?}");
        let skill = &found[0];
        assert_eq!(skill.sessions, 3);
        let texts: Vec<&str> = skill.steps.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "cargo build --release",
                "git tag v0.4.21",
                "git push origin v0.4.21",
                "https://github.com/acme/app/releases"
            ],
            "newest version of each step, in the order they were done"
        );
        assert!(skill.steps[1].varies && !skill.steps[0].varies);
        assert_eq!(skill.steps[0].kind, StepKind::Command);
        assert_eq!(skill.steps[3].kind, StepKind::Link);
        assert_eq!(skill.name, "cargo-git-workflow");
    }

    #[test]
    fn a_secret_is_never_a_step() {
        let found = find_skill_candidates(&history(), &PrivacyConfig::default(), 30);
        let md = render_skill_md(&found[0], &found[0].name);
        assert!(!md.contains("sk-proj"), "{md}");
    }

    #[test]
    fn one_session_is_not_a_workflow() {
        let one: Vec<ClipEntry> = history().into_iter().take(6).collect();
        assert!(find_skill_candidates(&one, &PrivacyConfig::default(), 30).is_empty());
    }

    #[test]
    fn the_skill_file_has_frontmatter_and_runnable_steps() {
        let found = find_skill_candidates(&history(), &PrivacyConfig::default(), 30);
        let md = render_skill_md(&found[0], "my-release");
        assert!(md.starts_with("---\nname: my-release\ndescription: \""), "{md}");
        assert!(md.contains("```bash\n   cargo build --release\n   ```"), "{md}");
        assert!(md.contains("Open <https://github.com/acme/app/releases>"), "{md}");
        assert!(md.contains("nothing was sent anywhere"));
        assert!(md.contains("changes between runs"));
    }

    #[test]
    fn versions_and_ids_fold_into_one_step() {
        assert_eq!(step_key("git tag v0.4.20"), step_key("git tag v0.4.21"));
        assert_eq!(step_key("git checkout 3f9a2c1d"), step_key("git checkout 0b17e9ff"));
        assert_eq!(
            step_key("https://x.com/a?utm=1"),
            step_key("https://x.com/a?utm=2")
        );
        assert_ne!(step_key("git push"), step_key("git pull"));
    }

    #[test]
    fn names_are_folder_safe_and_saving_never_overwrites() {
        assert_eq!(clean_skill_name("My Release!! flow"), "my-release-flow");
        assert_eq!(clean_skill_name("///"), "clipboard-workflow");
        let dir = std::env::temp_dir().join(format!("clipd-skill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = save_skill(&dir, "demo", "---\nname: demo\n---\n").unwrap();
        assert!(path.ends_with("demo/SKILL.md"));
        assert!(save_skill(&dir, "demo", "other").is_err(), "an existing skill is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn answered_workflows_are_remembered() {
        let state = SkillState { dismissed: vec!["a".into()], created: vec!["b".into()] };
        assert!(state.answered("a") && state.answered("b") && !state.answered("c"));
    }
}
