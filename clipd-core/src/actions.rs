//! Custom Actions — user-defined shell commands that run on a clip.
//!
//! This is clipd's answer to CopyQ's scriptable actions, without inventing a
//! scripting language: an action is just a shell command. The selected clip is
//! piped to the command's **stdin**, and the command's **stdout** is applied
//! according to `output` (replace the clipboard, create a new clip, or nothing).
//!
//! Because the command is run through the system shell, "scripting" is whatever
//! the user wants: `jq .`, `sed 's/foo/bar/'`, `python3 ~/scripts/x.py`,
//! `curl -s -d @- https://…`, and so on.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// What to do with a command's stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionOutput {
    /// Put the output on the clipboard (the default — good for transforms).
    Clipboard,
    /// Save the output as a new clip in history.
    NewClip,
    /// Discard output — the command was run for its side effect.
    None,
}

impl Default for ActionOutput {
    fn default() -> Self {
        ActionOutput::Clipboard
    }
}

impl ActionOutput {
    pub fn label(&self) -> &'static str {
        match self {
            ActionOutput::Clipboard => "Copy result to clipboard",
            ActionOutput::NewClip => "Save result as new clip",
            ActionOutput::None => "Run only (ignore output)",
        }
    }

    pub const ALL: [ActionOutput; 3] = [
        ActionOutput::Clipboard,
        ActionOutput::NewClip,
        ActionOutput::None,
    ];
}

fn default_true() -> bool {
    true
}

/// A single user-defined action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomAction {
    pub name: String,
    /// Shell command. The clip is piped to its stdin.
    pub command: String,
    #[serde(default)]
    pub output: ActionOutput,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When non-empty, this action runs AUTOMATICALLY whenever a newly
    /// captured clip contains this text (case-insensitive). Clipboard-output
    /// actions never auto-run (self-trigger loop risk).
    #[serde(default)]
    pub auto_pattern: String,
}

impl CustomAction {
    pub fn new(name: impl Into<String>, command: impl Into<String>, output: ActionOutput) -> Self {
        CustomAction {
            name: name.into(),
            command: command.into(),
            output,
            enabled: true,
            auto_pattern: String::new(),
        }
    }
}

/// Persisted list of actions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionsConfig {
    #[serde(default)]
    pub actions: Vec<CustomAction>,
}

fn config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("actions.json")
}

/// Load actions. On first run (no file) seeds a couple of useful examples so the
/// feature is discoverable instead of an empty box.
pub fn load_actions() -> ActionsConfig {
    if let Ok(s) = std::fs::read_to_string(config_path()) {
        if let Ok(cfg) = serde_json::from_str::<ActionsConfig>(&s) {
            return cfg;
        }
    }
    ActionsConfig {
        actions: vec![
            CustomAction::new("Pretty-print JSON", "jq .", ActionOutput::Clipboard),
            CustomAction::new("UPPERCASE", "tr '[:lower:]' '[:upper:]'", ActionOutput::Clipboard),
            CustomAction::new(
                "Word count",
                "wc -w | tr -d ' '",
                ActionOutput::Clipboard,
            ),
        ],
    }
}

pub fn save_actions(cfg: &ActionsConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, json);
    }
}

/// Run a shell command, piping `input` to its stdin, returning stdout. Bounded
/// by `timeout` so a hung command can't freeze the caller.
pub fn run_action(command: &str, input: &str, timeout: Duration) -> Result<String, String> {
    let command = command.to_string();
    let input = input.to_string();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let result = run_blocking(&command, &input);
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(format!(
            "Command timed out after {}s",
            timeout.as_secs().max(1)
        )),
    }
}

fn run_blocking(command: &str, input: &str) -> Result<String, String> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Couldn't start command: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        // Ignore write errors: a command like `head` may close stdin early.
        let _ = stdin.write_all(input.as_bytes());
        // Dropping stdin here closes it so the command sees EOF.
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("Command failed: {e}"))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("Command exited with {}", out.status)
        } else {
            err
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn pipes_stdin_and_captures_stdout() {
        let out = run_action("tr '[:lower:]' '[:upper:]'", "hello", Duration::from_secs(5)).unwrap();
        assert_eq!(out.trim(), "HELLO");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn reports_command_failure() {
        let err = run_action("exit 3", "x", Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("exited") || err.contains("3"));
    }
}
