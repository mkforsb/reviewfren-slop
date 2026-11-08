//! Persistent application settings and small persisted state (viewed files).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub const DEFAULT_CLAUDE_COMMAND: &str = "claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --permission-mode default --disallowedTools AskUserQuestion";

pub const DEFAULT_FILE_CHAT_META: &str = "[Meta: the following message was entered into a chat box specifically for @{path} (see `git diff {merge_base} -- {path}`; the PR branch is checked out, so the working tree contains the new version). Please respond keeping in mind that the user expects your output to follow the conversation in this specific chat box and that the conversation will be centered on the specific file. Keep answers focused and concise.]\n\n{message}";

pub const DEFAULT_GENERAL_CHAT_META: &str = "[Meta: the following message was entered into the general chat panel of a PR review tool. This query isn't about a specific file. The PR under review is #{pr_number} \"{pr_title}\" ({head_branch} -> {base_branch}); the PR branch is checked out and `git diff {merge_base}` shows the full change.]\n\n{message}";

pub const DEFAULT_ANNOTATE_PROMPT: &str = "[Meta: this request comes from the \"Annotate\" button of a PR review tool. Your answer is parsed by a program, not read by a human.]\n\nPlease read @{path} (the version currently in the working tree) and look at its changes in `git diff {merge_base} -- {path}`. Then write a short end-of-line comment for EVERY line of the file, changed or not, that would help a reviewer understand the code and the change (what the line does, why it changed, potential issues). Keep each comment under ~100 characters. Blank lines and lines with only braces may get an empty comment. Do NOT edit any files.\n\nRespond with ONLY a JSON object, no prose and no code fences, of the form:\n{\"annotations\": {\"1\": \"comment for line 1\", \"2\": \"comment for line 2\"}}\nwhere the keys are 1-based line numbers in the current working-tree version of the file.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Full shell command used to launch Claude Code (run via `sh -c` in the repo dir).
    /// `{repo}` is replaced with the shell-quoted repository path.
    pub claude_command: String,
    pub file_chat_meta: String,
    pub general_chat_meta: String,
    pub annotate_prompt: String,
    pub dark_theme: bool,
    /// Soft-wrap long lines in diffs by default (each file can override it).
    pub wrap_lines: bool,
    pub recent_repos: Vec<PathBuf>,
    pub show_tree: bool,
    pub show_chat: bool,
    /// UI font family ("" = system default). Applied at startup.
    pub ui_font: String,
    /// Code font family ("" = system monospace). Applied live.
    pub code_font: String,
    pub code_font_size: f32,
    /// Zoom factor for the whole UI.
    pub ui_scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            claude_command: DEFAULT_CLAUDE_COMMAND.into(),
            file_chat_meta: DEFAULT_FILE_CHAT_META.into(),
            general_chat_meta: DEFAULT_GENERAL_CHAT_META.into(),
            annotate_prompt: DEFAULT_ANNOTATE_PROMPT.into(),
            dark_theme: false,
            wrap_lines: false,
            recent_repos: Vec::new(),
            show_tree: true,
            show_chat: true,
            ui_font: String::new(),
            code_font: String::new(),
            code_font_size: crate::fonts::DEFAULT_CODE_SIZE,
            ui_scale: 1.0,
        }
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("reviewfren")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("reviewfren")
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| format!("Could not save {}: {e}", path.display()))
    }

    pub fn add_recent_repo(&mut self, repo: PathBuf) {
        self.recent_repos.retain(|r| r != &repo);
        self.recent_repos.insert(0, repo);
        self.recent_repos.truncate(10);
    }
}

/// Values substituted into prompt templates.
#[derive(Debug, Clone, Default)]
pub struct PromptVars {
    pub path: String,
    pub merge_base: String,
    pub pr_number: String,
    pub pr_title: String,
    pub head_branch: String,
    pub base_branch: String,
}

/// Fills `{placeholders}` in a template; appends the message if `{message}` is absent.
pub fn render_template(template: &str, vars: &PromptVars, message: &str) -> String {
    let mut out = template
        .replace("{path}", &vars.path)
        .replace("{merge_base}", &vars.merge_base)
        .replace("{pr_number}", &vars.pr_number)
        .replace("{pr_title}", &vars.pr_title)
        .replace("{head_branch}", &vars.head_branch)
        .replace("{base_branch}", &vars.base_branch);
    if out.contains("{message}") {
        out = out.replace("{message}", message);
    } else if !message.is_empty() {
        out.push_str("\n\n");
        out.push_str(message);
    }
    out
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Persisted "Viewed" marks: key is "<repo>#<pr>:<path>", value is a hash of the
/// file's diff so that a mark is dropped automatically when the file changes.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ViewedStore {
    pub entries: HashMap<String, u64>,
}

impl ViewedStore {
    fn path() -> PathBuf {
        data_dir().join("viewed.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn key(repo: &std::path::Path, pr: u64, path: &str) -> String {
        format!("{}#{pr}:{path}", repo.display())
    }
}

pub fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template() {
        let vars = PromptVars { path: "src/a.rs".into(), merge_base: "abc".into(), ..Default::default() };
        let s = render_template("see {path} {merge_base}\n\n{message}", &vars, "hi");
        assert_eq!(s, "see src/a.rs abc\n\nhi");
        assert_eq!(render_template("x", &vars, "hi"), "x\n\nhi");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
