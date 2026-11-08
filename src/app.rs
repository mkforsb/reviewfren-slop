//! Application state, messages and update logic.

use crate::claude::{self, Outgoing, PermissionRequest};
use crate::code_view::Selection;
use crate::diff::{self, Row, RowKind};
use crate::git::{self, ChangeKind, ChangedFile, DiffSide, PullRequest, ReviewThread};
use crate::highlight::{self, LineSpans};
use crate::lsp;
use crate::mason::Server;
use crate::settings::{self, PromptVars, Settings, ViewedStore};
use iced::widget::{markdown, operation, text_editor};
use iced::{Point, Subscription, Task};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

pub const REVIEW_SCROLL_ID: &str = "review-scroll";
pub const GENERAL_CHAT_SCROLL_ID: &str = "general-chat-scroll";

pub fn file_section_id(idx: usize) -> String {
    format!("file-section-{idx}")
}

pub fn file_chat_scroll_id(idx: usize) -> String {
    format!("file-chat-scroll-{idx}")
}

pub fn file_chat_id(idx: usize) -> String {
    format!("file-chat-{idx}")
}

pub fn file_chat_input_id(idx: usize) -> String {
    format!("file-chat-input-{idx}")
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct App {
    pub settings: Settings,
    pub viewed_store: ViewedStore,
    pub screen: Screen,
    pub settings_editor: Option<SettingsEditor>,
    pub cursor: Point,
    pub window_size: iced::Size,
    pub modal_error: Option<String>,
    /// The most recently copied text, while its button shows "Copied!", with the
    /// copy's sequence number (so an older timer can't clear a newer copy).
    pub copied: Option<(u64, String)>,
    copy_seq: u64,
}

/// How long a copy button says "Copied!".
const COPIED_FEEDBACK: std::time::Duration = std::time::Duration::from_secs(1);

pub enum Screen {
    RepoSelect(RepoSelect),
    PrList(PrList),
    Loading(Loading),
    Review(Box<Review>),
}

impl Screen {
    fn name(&self) -> &'static str {
        match self {
            Screen::RepoSelect(_) => "RepoSelect",
            Screen::PrList(_) => "PrList",
            Screen::Loading(_) => "Loading",
            Screen::Review(_) => "Review",
        }
    }
}

#[derive(Default)]
pub struct RepoSelect {
    pub path_input: String,
    pub errors: Vec<String>,
    pub busy: bool,
}

pub struct PrList {
    pub repo: PathBuf,
    pub branch: Option<String>,
    pub prs: Vec<PullRequest>,
    pub loading: bool,
    pub errors: Vec<String>,
    pub busy_pr: Option<u64>,
}

pub struct Loading {
    pub repo: PathBuf,
    pub pr: PullRequest,
    pub step: String,
    pub log: String,
}

pub struct SettingsEditor {
    pub command: text_editor::Content,
    pub file_chat_meta: text_editor::Content,
    pub general_chat_meta: text_editor::Content,
    pub annotate_prompt: text_editor::Content,
    pub dark_theme: bool,
    pub wrap_lines: bool,
    pub error: Option<String>,
    pub ui_font: String,
    pub code_font: String,
    pub code_font_size: f32,
    pub ui_scale: f32,
    pub installed_fonts: crate::fonts::Installed,
    pub ui_font_options: iced::widget::combo_box::State<String>,
    pub code_font_options: iced::widget::combo_box::State<String>,
    /// A small highlighted diff used to preview the code font.
    pub sample: CodeModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsField {
    Command,
    FileChatMeta,
    GeneralChatMeta,
    AnnotatePrompt,
}

impl SettingsEditor {
    fn new(s: &Settings) -> Self {
        Self {
            command: text_editor::Content::with_text(&s.claude_command),
            file_chat_meta: text_editor::Content::with_text(&s.file_chat_meta),
            general_chat_meta: text_editor::Content::with_text(&s.general_chat_meta),
            annotate_prompt: text_editor::Content::with_text(&s.annotate_prompt),
            dark_theme: s.dark_theme,
            wrap_lines: s.wrap_lines,
            error: None,
            ui_font: s.ui_font.clone(),
            code_font: s.code_font.clone(),
            code_font_size: s.code_font_size,
            ui_scale: s.ui_scale,
            ui_font_options: iced::widget::combo_box::State::new(Vec::new()),
            code_font_options: iced::widget::combo_box::State::new(Vec::new()),
            installed_fonts: Default::default(),
            sample: sample_model(s.dark_theme),
        }
    }

    /// Fills the font pickers with the installed families (monospace ones first for code).
    fn load_fonts(&mut self) {
        let installed = crate::fonts::installed();
        let mut code: Vec<String> = installed.monospace.clone();
        code.extend(installed.all.iter().filter(|f| !installed.monospace.contains(f)).cloned());
        self.ui_font_options = iced::widget::combo_box::State::new(installed.all.clone());
        self.code_font_options = iced::widget::combo_box::State::new(code);
        self.installed_fonts = installed;
    }

    pub fn field_mut(&mut self, f: SettingsField) -> &mut text_editor::Content {
        match f {
            SettingsField::Command => &mut self.command,
            SettingsField::FileChatMeta => &mut self.file_chat_meta,
            SettingsField::GeneralChatMeta => &mut self.general_chat_meta,
            SettingsField::AnnotatePrompt => &mut self.annotate_prompt,
        }
    }
}

const SAMPLE_OLD: &str = "fn greet(name: &str) -> String {\n    format!(\"Hello, {}!\", name)\n}\n";
const SAMPLE_NEW: &str = "/// Greets someone politely.\nfn greet(name: &str) -> String {\n    format!(\"Hello, {name}! 0O 1lI {{}} => != ->\")\n}\n";
const SAMPLE_DIFF: &str = "@@ -1,3 +1,4 @@ impl Greeter\n+/// Greets someone politely.\n fn greet(name: &str) -> String {\n-    format!(\"Hello, {}!\", name)\n+    format!(\"Hello, {name}! 0O 1lI {{}} => != ->\")\n }\n";

fn sample_model(dark: bool) -> CodeModel {
    let hunks = diff::parse_hunks(SAMPLE_DIFF);
    let rows = diff::compact_rows(&hunks);
    let (spans, _) = highlight_models("sample.rs", SAMPLE_OLD, SAMPLE_NEW, &rows, &[], dark);
    CodeModel { rows, spans }
}

/// Text with a trailing newline stripped (text_editor always has one).
fn editor_text(c: &text_editor::Content) -> String {
    let t = c.text();
    t.strip_suffix('\n').map(str::to_owned).unwrap_or(t)
}

#[derive(Debug, Clone, Default)]
pub struct CodeModel {
    pub rows: Vec<Row>,
    pub spans: Vec<LineSpans>,
}

/// Everything computed for one changed file when loading the review.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    pub file: ChangedFile,
    pub diff_hash: u64,
    pub binary: bool,
    pub old_text: Arc<String>,
    pub new_text: Arc<String>,
    pub compact: CodeModel,
    pub full: CodeModel,
}

#[derive(Debug, Clone)]
pub struct ReviewData {
    pub merge_base: String,
    pub files: Vec<LoadedFile>,
    pub branch: Option<String>,
    /// Review comment threads; failing to load them doesn't stop the review.
    pub threads: Result<Vec<ReviewThread>, String>,
}

pub enum ChatEntry {
    User(String),
    Assistant { msg_id: String, text: String, md: markdown::Content },
    Tool(String),
    Error(String),
}

pub struct Chat {
    pub entries: Vec<ChatEntry>,
    pub input: text_editor::Content,
}

impl Default for Chat {
    fn default() -> Self {
        Self { entries: Vec::new(), input: text_editor::Content::new() }
    }
}

pub struct FileEntry {
    pub data: LoadedFile,
    pub show_full: bool,
    /// Per-file override of [`Settings::wrap_lines`].
    pub wrap: Option<bool>,
    pub collapsed: bool,
    pub viewed: bool,
    pub annotations: Option<BTreeMap<u32, String>>,
    pub annotating: bool,
    pub annotate_error: Option<String>,
    pub chat: Option<Chat>,
    pub threads: Vec<ThreadEntry>,
}

/// A review comment thread shown inline in a file's diff.
pub struct ThreadEntry {
    pub thread: ReviewThread,
    pub expanded: bool,
    /// The comments' bodies, parsed once.
    pub bodies: Vec<markdown::Content>,
}

impl ThreadEntry {
    fn new(thread: ReviewThread) -> Self {
        Self {
            // Like GitHub: open threads are shown, resolved ones start collapsed.
            expanded: !thread.resolved,
            bodies: thread.comments.iter().map(|c| markdown::Content::parse(&c.body)).collect(),
            thread,
        }
    }

    /// The row the thread is attached to, if it is part of `rows`.
    pub fn row(&self, rows: &[Row]) -> Option<usize> {
        let t = &self.thread;
        rows.iter().position(|r| match t.side {
            DiffSide::Right => matches!(r.kind, RowKind::Context | RowKind::Added) && r.new_no == Some(t.line),
            DiffSide::Left => matches!(r.kind, RowKind::Context | RowKind::Removed) && r.old_no == Some(t.line),
        })
    }

    /// "R4", or "R10 to R12" for a comment on several lines.
    pub fn lines_label(&self) -> String {
        let t = &self.thread;
        let side = if t.side == DiffSide::Left { "L" } else { "R" };
        match t.start_line {
            Some(s) if s != t.line => format!("lines {side}{s} to {side}{}", t.line),
            _ => format!("line {side}{}", t.line),
        }
    }
}

impl FileEntry {
    pub fn path(&self) -> &str {
        &self.data.file.path
    }

    pub fn wraps(&self, default: bool) -> bool {
        self.wrap.unwrap_or(default)
    }

    pub fn model(&self) -> &CodeModel {
        if self.show_full { &self.data.full } else { &self.data.compact }
    }

    pub fn extension(&self) -> String {
        ext_of(self.path())
    }
}

pub fn ext_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!(".{ext}"),
        _ => "No extension".into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    File(usize),
    General,
    Annotate(usize),
}

pub struct ClaudeJob {
    pub target: Target,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeStatus {
    Starting,
    Ready,
    Exited(String),
}

pub struct ClaudeState {
    pub generation: u64,
    pub sender: Option<UnboundedSender<Outgoing>>,
    pub status: ClaudeStatus,
    pub queue: VecDeque<ClaudeJob>,
    pub in_flight: Option<ClaudeJob>,
    pub model: String,
    pub session_id: String,
    pub permission_mode: String,
    pub cost: f64,
    pub current_msg: String,
    pub activity: String,
    pub interrupting: bool,
    pub permissions: VecDeque<PermissionRequest>,
}

impl ClaudeState {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            sender: None,
            status: ClaudeStatus::Starting,
            queue: VecDeque::new(),
            in_flight: None,
            model: String::new(),
            session_id: String::new(),
            permission_mode: String::new(),
            cost: 0.0,
            current_msg: String::new(),
            activity: String::new(),
            interrupting: false,
            permissions: VecDeque::new(),
        }
    }

    /// Whether a job for `target` is running or queued.
    pub fn pending_for(&self, target: Target) -> Option<usize> {
        if self.in_flight.as_ref().is_some_and(|j| j.target == target) {
            return Some(0);
        }
        self.queue.iter().position(|j| j.target == target).map(|p| p + 1)
    }
}

#[derive(Default)]
pub struct LspState {
    pub sender: Option<UnboundedSender<lsp::Request>>,
    pub status: String,
    pub progress: BTreeMap<String, (String, String, Option<u32>)>,
    pub exited: Option<String>,
}

pub struct ContextMenu {
    pub position: Point,
    pub file: usize,
    pub full: bool,
    pub selection: Selection,
}

pub struct Review {
    pub repo: PathBuf,
    pub pr: PullRequest,
    pub pr_body: markdown::Content,
    pub show_description: bool,
    pub merge_base: String,
    pub branch: Option<String>,
    pub files: Vec<FileEntry>,
    pub errors: Vec<String>,
    pub filter: String,
    pub hidden_exts: BTreeSet<String>,
    pub collapsed_dirs: HashSet<String>,
    pub general_chat: Chat,
    pub claude: ClaudeState,
    pub lsp: BTreeMap<Server, LspState>,
    pub lsp_last_result: Option<String>,
    pub context_menu: Option<ContextMenu>,
    /// Commenters' avatars, by URL.
    pub avatars: HashMap<String, iced::widget::image::Handle>,
}

impl Review {
    pub fn is_visible(&self, f: &FileEntry) -> bool {
        if self.hidden_exts.contains(&f.extension()) {
            return false;
        }
        let q = self.filter.trim().to_lowercase();
        q.is_empty() || f.path().to_lowercase().contains(&q)
    }

    fn prompt_vars(&self, path: &str) -> PromptVars {
        PromptVars {
            path: path.to_string(),
            merge_base: self.merge_base.clone(),
            pr_number: self.pr.number.to_string(),
            pr_title: self.pr.title.clone(),
            head_branch: self.pr.head_ref_name.clone(),
            base_branch: self.pr.base_ref_name.clone(),
        }
    }

    fn chat_mut(&mut self, target: Target) -> Option<&mut Chat> {
        match target {
            Target::General => Some(&mut self.general_chat),
            Target::File(i) => self.files.get_mut(i).and_then(|f| f.chat.as_mut()),
            Target::Annotate(_) => None,
        }
    }

    fn enqueue(&mut self, job: ClaudeJob) {
        self.claude.queue.push_back(job);
        self.pump();
    }

    /// Sends the next queued job if Claude is idle.
    fn pump(&mut self) {
        if self.claude.in_flight.is_some() {
            return;
        }
        let Some(tx) = &self.claude.sender else { return };
        let Some(job) = self.claude.queue.pop_front() else { return };
        if tx.send(Outgoing::User(job.prompt.clone())).is_err() {
            self.claude.queue.push_front(job);
            return;
        }
        self.claude.current_msg.clear();
        self.claude.activity = "thinking…".into();
        self.claude.in_flight = Some(job);
    }

    fn fail_all_jobs(&mut self, reason: &str) {
        let jobs: Vec<ClaudeJob> = self.claude.in_flight.take().into_iter().chain(self.claude.queue.drain(..)).collect();
        for job in jobs {
            self.finish_job_with_error(job.target, reason);
        }
    }

    fn finish_job_with_error(&mut self, target: Target, reason: &str) {
        match target {
            Target::Annotate(i) => {
                if let Some(f) = self.files.get_mut(i) {
                    f.annotating = false;
                    f.annotate_error = Some(reason.to_string());
                }
            }
            t => {
                if let Some(c) = self.chat_mut(t) {
                    c.entries.push(ChatEntry::Error(reason.to_string()));
                }
            }
        }
    }

    pub fn viewed_count(&self) -> usize {
        self.files.iter().filter(|f| f.viewed).count()
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeMenu {
    Annotate,
    Chat,
}

impl std::fmt::Display for ClaudeMenu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ClaudeMenu::Annotate => "Annotate",
            ClaudeMenu::Chat => "Chat",
        })
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Noop,
    WindowEvent(iced::Event),
    CopyText(String),
    CopiedExpired(u64),
    // Settings
    OpenSettings,
    CloseSettings,
    SaveSettings,
    ResetSettings,
    SettingsEdit(SettingsField, text_editor::Action),
    SettingsDarkTheme(bool),
    SettingsWrapLines(bool),
    SettingsUiFont(String),
    SettingsCodeFont(String),
    SettingsCodeSize(f32),
    SettingsUiScale(f32),
    DismissModal,
    // Repo select
    PickRepo,
    RepoPicked(Option<PathBuf>, bool),
    RepoPathChanged(String),
    OpenRepoPath,
    OpenRecent(PathBuf),
    RepoVerified(Result<PathBuf, String>),
    DismissError(usize),
    // PR list
    PrsLoaded(Result<(Vec<PullRequest>, Option<String>), String>),
    RefreshPrs,
    BackToRepoSelect,
    SelectPr(u64),
    CleanChecked(u64, Result<Vec<String>, String>),
    CheckedOut(Result<String, String>),
    ReviewLoaded(Result<ReviewData, String>),
    // Review
    BackToPrList,
    ToggleTree,
    ToggleChat,
    ToggleDescription,
    ToggleCollapsed(usize),
    ToggleViewed(usize, bool),
    ToggleFull(usize),
    ToggleWrap(usize),
    ExpandAll,
    CollapseAll,
    FilterChanged(String),
    ToggleExt(String),
    ToggleDir(String),
    JumpToFile(usize),
    ScrollTo(Option<f32>),
    LinkClicked(markdown::Uri),
    OpenUrl(String),
    ClaudeMenu(usize, ClaudeMenu),
    ClearAnnotations(usize),
    FileChatEdit(usize, text_editor::Action),
    FileChatSend(usize),
    CloseFileChat(usize),
    GeneralChatEdit(text_editor::Action),
    GeneralChatSend,
    StopClaude,
    RestartClaude,
    Claude(claude::Event),
    PermissionDecision { allow: bool, always: bool },
    StartLsp(Server),
    StopLsp(Server),
    Lsp(lsp::Event),
    CodeContext(usize, bool, Selection),
    ToggleThread(usize, usize),
    AvatarLoaded(String, Option<Vec<u8>>),
    CloseContextMenu,
    GotoDefinition,
    CopySelection,
    Rehighlighted(Vec<(usize, Vec<LineSpans>, Vec<LineSpans>)>),
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

fn row_spans(rows: &[Row], old: &[LineSpans], new: &[LineSpans]) -> Vec<LineSpans> {
    rows.iter()
        .map(|r| match r.kind {
            RowKind::HunkHeader => Vec::new(),
            RowKind::Removed => r.old_no.and_then(|n| old.get(n as usize - 1)).cloned().unwrap_or_default(),
            _ => r.new_no.and_then(|n| new.get(n as usize - 1)).cloned().unwrap_or_default(),
        })
        .collect()
}

fn highlight_models(path: &str, old_text: &str, new_text: &str, compact: &[Row], full: &[Row], dark: bool) -> (Vec<LineSpans>, Vec<LineSpans>) {
    let old = if old_text.is_empty() { Vec::new() } else { highlight::highlight_file(path, old_text, dark) };
    let new = if new_text.is_empty() { Vec::new() } else { highlight::highlight_file(path, new_text, dark) };
    (row_spans(compact, &old, &new), row_spans(full, &old, &new))
}

async fn load_file(repo: PathBuf, merge_base: String, file: ChangedFile, dark: bool) -> Result<LoadedFile, String> {
    let diff_text = git::file_diff(&repo, &merge_base, &file).await?;
    let diff_hash = settings::hash_str(&diff_text);
    let hunks = diff::parse_hunks(&diff_text);
    let old_path = file.old_path.clone().unwrap_or_else(|| file.path.clone());
    let mut binary = file.binary;
    let old_text = if file.kind == ChangeKind::Added || binary {
        String::new()
    } else {
        match git::show_file(&repo, &merge_base, &old_path).await? {
            Some(t) => t,
            None => {
                binary = true;
                String::new()
            }
        }
    };
    let new_text = if file.kind == ChangeKind::Deleted || binary {
        String::new()
    } else {
        match tokio::fs::read(repo.join(&file.path)).await.ok().and_then(git::text_or_none) {
            Some(t) => t,
            None => {
                binary = true;
                String::new()
            }
        }
    };
    let compact_rows = diff::compact_rows(&hunks);
    let full_rows = if binary { Vec::new() } else { diff::full_rows(&hunks, &new_text, &old_text) };
    let path = file.path.clone();
    let (old_text, new_text) = (Arc::new(old_text), Arc::new(new_text));
    let (o, n) = (old_text.clone(), new_text.clone());
    let (cr, fr) = (compact_rows.clone(), full_rows.clone());
    let (compact_spans, full_spans) =
        tokio::task::spawn_blocking(move || highlight_models(&path, &o, &n, &cr, &fr, dark))
            .await
            .map_err(|e| e.to_string())?;
    Ok(LoadedFile {
        file,
        diff_hash,
        binary,
        old_text,
        new_text,
        compact: CodeModel { rows: compact_rows, spans: compact_spans },
        full: CodeModel { rows: full_rows, spans: full_spans },
    })
}

async fn load_review(repo: PathBuf, pr: PullRequest, dark: bool) -> Result<ReviewData, String> {
    let merge_base = git::merge_base(&repo, &pr.base_ref_name).await?;
    let files = git::changed_files(&repo, &merge_base).await?;
    let loads = files.into_iter().map(|f| load_file(repo.clone(), merge_base.clone(), f, dark));
    let (files, threads) = futures::join!(futures::future::join_all(loads), git::review_threads(repo.clone(), pr.number));
    let files = files.into_iter().collect::<Result<Vec<_>, _>>()?;
    let branch = git::current_branch(&repo).await;
    Ok(ReviewData { merge_base, files, branch, threads })
}

async fn fetch_avatar(url: String) -> Option<Vec<u8>> {
    let response = reqwest::get(&url).await.ok()?.error_for_status().ok()?;
    response.bytes().await.ok().map(|b| b.to_vec())
}

async fn load_prs(repo: PathBuf) -> Result<(Vec<PullRequest>, Option<String>), String> {
    let prs = git::list_prs(repo.clone()).await?;
    Ok((prs, git::current_branch(&repo).await))
}

// ---------------------------------------------------------------------------
// Scroll-to-file operation
// ---------------------------------------------------------------------------

/// Where a container sits inside a scrollable, in the scrollable's content coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Located {
    /// Top of the target, relative to the top of the scrollable content.
    pub y: f32,
    pub height: f32,
    /// Height of the scrollable's visible area.
    pub viewport: f32,
    /// Current vertical scroll offset.
    pub scroll: f32,
}

impl Located {
    /// Offset that puts the top of the target just below the top of the viewport.
    fn top(self) -> f32 {
        (self.y - 8.0).max(0.0)
    }

    /// Offset that brings the bottom of the target into view, or `None` if the
    /// whole target is already visible.
    fn reveal_bottom(self) -> Option<f32> {
        let bottom = self.y + self.height + 8.0;
        let fully_visible = self.y >= self.scroll && bottom <= self.scroll + self.viewport;
        (!fully_visible).then(|| (bottom - self.viewport).max(0.0))
    }
}

/// Finds a container inside a scrollable.
struct Locate {
    scrollable: iced::widget::Id,
    target: iced::widget::Id,
    viewport: Option<(f32, f32, f32)>,
    target_bounds: Option<iced::Rectangle>,
    /// If set, only report the target while this focusable widget has focus.
    require_focus: Option<iced::widget::Id>,
    has_focus: bool,
}

impl Locate {
    fn new(scrollable: impl Into<iced::widget::Id>, target: impl Into<iced::widget::Id>) -> Self {
        Self {
            scrollable: scrollable.into(),
            target: target.into(),
            viewport: None,
            target_bounds: None,
            require_focus: None,
            has_focus: false,
        }
    }

    fn if_focused(mut self, id: impl Into<iced::widget::Id>) -> Self {
        self.require_focus = Some(id.into());
        self
    }
}

impl iced::advanced::widget::Operation<Option<Located>> for Locate {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<Option<Located>>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: iced::Rectangle) {
        if id == Some(&self.target) {
            self.target_bounds = Some(bounds);
        }
    }

    fn focusable(
        &mut self,
        id: Option<&iced::widget::Id>,
        _bounds: iced::Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        if id.is_some() && id == self.require_focus.as_ref() {
            self.has_focus = state.is_focused();
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: iced::Rectangle,
        content_bounds: iced::Rectangle,
        translation: iced::Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&self.scrollable) {
            self.viewport = Some((content_bounds.y, bounds.height, translation.y));
        }
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Option<Located>> {
        if self.require_focus.is_some() && !self.has_focus {
            return iced::advanced::widget::operation::Outcome::Some(None);
        }
        iced::advanced::widget::operation::Outcome::Some(match (self.viewport, self.target_bounds) {
            (Some((content_y, viewport, scroll)), Some(t)) => {
                Some(Located { y: t.y - content_y, height: t.height, viewport, scroll })
            }
            _ => None,
        })
    }
}

/// Scrolls the review page so the whole chat box under file `i`, including its input
/// at the bottom, is visible. With `only_if_focused`, does so only while that chat's
/// input has keyboard focus, so a user who has moved on to the code isn't pulled back.
fn reveal_file_chat(i: usize, only_if_focused: bool) -> Task<Message> {
    let mut locate = Locate::new(REVIEW_SCROLL_ID, file_chat_id(i));
    if only_if_focused {
        locate = locate.if_focused(file_chat_input_id(i));
    }
    iced::advanced::widget::operate(locate).map(|l| Message::ScrollTo(l.and_then(Located::reveal_bottom)))
}

fn scroll_chat_to_end(target: Target) -> Task<Message> {
    let id = match target {
        Target::General => GENERAL_CHAT_SCROLL_ID.to_string(),
        Target::File(i) => file_chat_scroll_id(i),
        Target::Annotate(_) => return Task::none(),
    };
    // The chat logs use `anchor_bottom()`, which measures the offset from the bottom:
    // relative offset 0 is the newest entry (and keeps following it as the log grows),
    // while `snap_to_end` (offset 1.0) would jump to the oldest one.
    let snap = operation::snap_to(id, operation::RelativeOffset::START);
    match target {
        // The chat box grows with the log until the log hits its height cap, which can
        // push the input below the window edge.
        Target::File(i) => Task::batch([snap, reveal_file_chat(i, true)]),
        _ => snap,
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let settings = Settings::load();
        // Allow `reviewfren <repo>` to jump straight to the PR list.
        let arg = std::env::args().nth(1).map(PathBuf::from);
        let app = App {
            settings,
            viewed_store: ViewedStore::load(),
            screen: Screen::RepoSelect(RepoSelect::default()),
            settings_editor: None,
            cursor: Point::ORIGIN,
            window_size: iced::Size::new(1500.0, 950.0),
            modal_error: None,
            copied: None,
            copy_seq: 0,
        };
        let task = match arg {
            Some(p) => Task::perform(git::verify_repo(p), Message::RepoVerified),
            None => Task::none(),
        };
        (app, task)
    }

    pub fn title(&self) -> String {
        match &self.screen {
            Screen::Review(r) => format!("PR #{} {} — PR Review", r.pr.number, r.pr.title),
            Screen::PrList(p) => format!("{} — PR Review", p.repo.display()),
            _ => "PR Review".into(),
        }
    }

    /// Text whose copy button currently says "Copied!".
    pub fn copied(&self) -> Option<&str> {
        self.copied.as_ref().map(|(_, t)| t.as_str())
    }

    pub fn scale_factor(&self) -> f32 {
        self.settings.ui_scale.clamp(0.5, 3.0)
    }

    fn relaunch(&self) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let repo = match &self.screen {
            Screen::PrList(p) => Some(p.repo.clone()),
            Screen::Loading(l) => Some(l.repo.clone()),
            Screen::Review(r) => Some(r.repo.clone()),
            Screen::RepoSelect(_) => None,
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.args(repo);
        cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn theme(&self) -> iced::Theme {
        crate::style::theme(self.settings.dark_theme)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![iced::event::listen_with(|event, _status, _window| match &event {
            iced::Event::Mouse(iced::mouse::Event::CursorMoved { .. })
            | iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Right))
            | iced::Event::Window(iced::window::Event::Resized(_))
            | iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            }) => Some(Message::WindowEvent(event)),
            _ => None,
        })];
        if let Screen::Review(r) = &self.screen {
            let cfg = claude::Config {
                command: self.settings.claude_command.clone(),
                repo: r.repo.clone(),
                generation: r.claude.generation,
            };
            subs.push(Subscription::run_with(cfg, claude::connect).map(Message::Claude));
            for (server, st) in &r.lsp {
                if st.exited.is_none() {
                    let cfg = lsp::Config { server: *server, root: r.repo.clone() };
                    subs.push(Subscription::run_with(cfg, lsp::connect).map(Message::Lsp));
                }
            }
        }
        Subscription::batch(subs)
    }

    fn open_repo(&mut self, repo: PathBuf) -> Task<Message> {
        self.screen = Screen::PrList(PrList {
            repo: repo.clone(),
            branch: None,
            prs: Vec::new(),
            loading: true,
            errors: Vec::new(),
            busy_pr: None,
        });
        Task::perform(load_prs(repo), Message::PrsLoaded)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let quiet = matches!(
            message,
            Message::WindowEvent(iced::Event::Mouse(iced::mouse::Event::CursorMoved { .. }))
        );
        if !quiet {
            let mut text = format!("{message:?}");
            if text.len() > 200 {
                let mut end = 200;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push('…');
            }
            crate::debug::dlog!("[app] message: {text}");
        }
        let before = self.screen.name();
        let task = self.update_inner(message);
        let after = self.screen.name();
        if before != after {
            crate::debug::dlog!("[app] screen: {before} -> {after}");
        }
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::WindowEvent(event) => {
                match event {
                    iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => self.cursor = position,
                    iced::Event::Mouse(_) => {
                        // Widget messages are processed before subscription events of the same
                        // batch, so re-anchor a just-opened context menu at the real cursor.
                        if let Screen::Review(r) = &mut self.screen
                            && let Some(m) = &mut r.context_menu {
                                m.position = self.cursor;
                            }
                    }
                    iced::Event::Window(iced::window::Event::Resized(size)) => self.window_size = size,
                    iced::Event::Keyboard(_) => {
                        // Escape closes the topmost transient UI.
                        if let Screen::Review(r) = &mut self.screen
                            && r.context_menu.take().is_some() {
                                return Task::none();
                            }
                        if self.modal_error.take().is_none() && self.settings_editor.is_some() {
                            self.settings_editor = None;
                        }
                    }
                    _ => {}
                }
                Task::none()
            }
            Message::DismissModal => {
                self.modal_error = None;
                Task::none()
            }

            // ----------------------------------------------------------- settings
            Message::OpenSettings => {
                let mut ed = SettingsEditor::new(&self.settings);
                ed.load_fonts();
                self.settings_editor = Some(ed);
                Task::none()
            }
            Message::CloseSettings => {
                self.settings_editor = None;
                Task::none()
            }
            Message::ResetSettings => {
                let defaults = Settings { dark_theme: self.settings.dark_theme, ..Settings::default() };
                let mut ed = SettingsEditor::new(&defaults);
                ed.load_fonts();
                self.settings_editor = Some(ed);
                Task::none()
            }
            Message::SettingsEdit(field, action) => {
                if let Some(ed) = &mut self.settings_editor {
                    ed.field_mut(field).perform(action);
                }
                Task::none()
            }
            Message::SettingsDarkTheme(dark) => {
                if let Some(ed) = &mut self.settings_editor {
                    ed.dark_theme = dark;
                    ed.sample = sample_model(dark);
                }
                Task::none()
            }
            Message::SettingsWrapLines(wrap) => {
                if let Some(ed) = &mut self.settings_editor {
                    ed.wrap_lines = wrap;
                }
                Task::none()
            }
            Message::SettingsUiFont(name) => {
                if let Some(ed) = &mut self.settings_editor {
                    ed.ui_font = name;
                }
                Task::none()
            }
            Message::SettingsCodeFont(name) => {
                if let Some(ed) = &mut self.settings_editor {
                    ed.code_font = name;
                }
                Task::none()
            }
            Message::SettingsCodeSize(size) => {
                if let Some(ed) = &mut self.settings_editor {
                    ed.code_font_size = size;
                }
                Task::none()
            }
            Message::SettingsUiScale(scale) => {
                if let Some(ed) = &mut self.settings_editor {
                    ed.ui_scale = scale;
                }
                Task::none()
            }
            Message::SaveSettings => {
                let Some(ed) = &mut self.settings_editor else { return Task::none() };
                let command = editor_text(&ed.command).trim().to_string();
                if command.is_empty() {
                    ed.error = Some("The Claude command must not be empty.".into());
                    return Task::none();
                }
                let old = self.settings.clone();
                self.settings.claude_command = command;
                self.settings.file_chat_meta = editor_text(&ed.file_chat_meta);
                self.settings.general_chat_meta = editor_text(&ed.general_chat_meta);
                self.settings.annotate_prompt = editor_text(&ed.annotate_prompt);
                self.settings.dark_theme = ed.dark_theme;
                self.settings.wrap_lines = ed.wrap_lines;
                self.settings.ui_font = ed.ui_font.trim().to_string();
                self.settings.code_font = ed.code_font.trim().to_string();
                self.settings.code_font_size = (ed.code_font_size * 2.0).round() / 2.0;
                self.settings.ui_scale = (ed.ui_scale * 20.0).round() / 20.0;
                if let Err(e) = self.settings.save() {
                    ed.error = Some(e);
                    return Task::none();
                }
                crate::fonts::set_code(&self.settings.code_font, self.settings.code_font_size);
                if old.ui_font != self.settings.ui_font {
                    // iced only reads the default font at startup: relaunch on the same repo.
                    let relaunched = self.relaunch();
                    match relaunched {
                        Ok(()) => return iced::exit(),
                        Err(e) => {
                            if let Some(ed) = &mut self.settings_editor {
                                ed.error = Some(format!("Settings saved, but restarting failed: {e}. Restart manually to apply the UI font."));
                            }
                            return Task::none();
                        }
                    }
                }
                self.settings_editor = None;
                let mut tasks = Vec::new();
                if let Screen::Review(r) = &mut self.screen {
                    if old.claude_command != self.settings.claude_command {
                        // The subscription identity changes, which restarts Claude.
                        r.fail_all_jobs("Claude was restarted because its command changed.");
                        r.claude = ClaudeState::new(r.claude.generation + 1);
                    }
                    if old.dark_theme != self.settings.dark_theme {
                        tasks.push(rehighlight(r, self.settings.dark_theme));
                    }
                }
                Task::batch(tasks)
            }

            // -------------------------------------------------------- repo select
            Message::PickRepo => {
                if let Screen::RepoSelect(s) = &mut self.screen {
                    s.busy = true;
                }
                Task::perform(
                    async {
                        let started = std::time::Instant::now();
                        let picked = rfd::AsyncFileDialog::new()
                            .set_title("Select a Git repository")
                            .pick_folder()
                            .await
                            .map(|h| h.path().to_path_buf());
                        // A dialog the user cancelled stays open for a while; an immediate `None`
                        // means no dialog could be shown at all.
                        (picked, started.elapsed() < std::time::Duration::from_millis(400))
                    },
                    |(picked, instant)| Message::RepoPicked(picked, instant),
                )
            }
            Message::RepoPicked(path, instant) => {
                if let Screen::RepoSelect(s) = &mut self.screen {
                    s.busy = false;
                    if path.is_none() && instant {
                        s.errors.push(
                            "The system folder picker could not be opened (is xdg-desktop-portal running?). Type or paste the repository path instead.".into(),
                        );
                    }
                    if let Some(p) = path {
                        s.path_input = p.display().to_string();
                        s.busy = true;
                        return Task::perform(git::verify_repo(p), Message::RepoVerified);
                    }
                }
                Task::none()
            }
            Message::RepoPathChanged(p) => {
                if let Screen::RepoSelect(s) = &mut self.screen {
                    s.path_input = p;
                }
                Task::none()
            }
            Message::OpenRepoPath => {
                if let Screen::RepoSelect(s) = &mut self.screen {
                    let raw = s.path_input.trim();
                    if raw.is_empty() {
                        return Task::none();
                    }
                    let p = if let Some(rest) = raw.strip_prefix("~/") {
                        dirs::home_dir().unwrap_or_default().join(rest)
                    } else {
                        PathBuf::from(raw)
                    };
                    s.busy = true;
                    return Task::perform(git::verify_repo(p), Message::RepoVerified);
                }
                Task::none()
            }
            Message::OpenRecent(p) => {
                let mut s = RepoSelect { path_input: p.display().to_string(), busy: true, ..Default::default() };
                if let Screen::RepoSelect(old) = &self.screen {
                    s.errors = old.errors.clone();
                }
                self.screen = Screen::RepoSelect(s);
                Task::perform(git::verify_repo(p), Message::RepoVerified)
            }
            Message::RepoVerified(result) => match result {
                Ok(repo) => self.open_repo(repo),
                Err(e) => {
                    let mut s = match std::mem::replace(&mut self.screen, Screen::RepoSelect(RepoSelect::default())) {
                        Screen::RepoSelect(s) => s,
                        _ => RepoSelect::default(),
                    };
                    s.busy = false;
                    s.errors.push(e);
                    self.screen = Screen::RepoSelect(s);
                    Task::none()
                }
            },
            Message::DismissError(i) => {
                let errors = match &mut self.screen {
                    Screen::RepoSelect(s) => &mut s.errors,
                    Screen::PrList(s) => &mut s.errors,
                    Screen::Review(r) => &mut r.errors,
                    Screen::Loading(_) => return Task::none(),
                };
                if i < errors.len() {
                    errors.remove(i);
                }
                Task::none()
            }

            // ------------------------------------------------------------ PR list
            Message::PrsLoaded(result) => {
                let Screen::PrList(s) = &mut self.screen else { return Task::none() };
                s.loading = false;
                match result {
                    Ok((prs, branch)) => {
                        s.prs = prs;
                        s.branch = branch;
                        // Only repos that actually work end up in the recent list.
                        self.settings.add_recent_repo(s.repo.clone());
                        let _ = self.settings.save();
                        Task::none()
                    }
                    Err(e) => {
                        // Failing to list PRs sends the user back to the repo select screen.
                        self.screen = Screen::RepoSelect(RepoSelect {
                            path_input: s.repo.display().to_string(),
                            errors: vec![e],
                            busy: false,
                        });
                        Task::none()
                    }
                }
            }
            Message::RefreshPrs => {
                let Screen::PrList(s) = &mut self.screen else { return Task::none() };
                s.loading = true;
                Task::perform(load_prs(s.repo.clone()), Message::PrsLoaded)
            }
            Message::BackToRepoSelect => {
                self.screen = Screen::RepoSelect(RepoSelect::default());
                Task::none()
            }
            Message::SelectPr(n) => {
                let Screen::PrList(s) = &mut self.screen else { return Task::none() };
                if s.busy_pr.is_some() {
                    return Task::none();
                }
                s.busy_pr = Some(n);
                s.errors.clear();
                Task::perform(git::uncommitted_changes(s.repo.clone()), move |r| Message::CleanChecked(n, r))
            }
            Message::CleanChecked(n, result) => {
                let Screen::PrList(s) = &mut self.screen else { return Task::none() };
                match result {
                    Ok(changes) if changes.is_empty() => {
                        let Some(pr) = s.prs.iter().find(|p| p.number == n).cloned() else { return Task::none() };
                        let repo = s.repo.clone();
                        self.screen = Screen::Loading(Loading {
                            repo: repo.clone(),
                            pr: pr.clone(),
                            step: format!("Checking out {}…", pr.head_ref_name),
                            log: String::new(),
                        });
                        Task::perform(git::checkout_pr(repo, pr), Message::CheckedOut)
                    }
                    Ok(changes) => {
                        s.busy_pr = None;
                        let mut list = changes.iter().take(10).cloned().collect::<Vec<_>>().join("\n");
                        if changes.len() > 10 {
                            list.push_str(&format!("\n… and {} more", changes.len() - 10));
                        }
                        s.errors.push(format!(
                            "The repository has uncommitted changes. Commit or stash them before reviewing a PR:\n{list}"
                        ));
                        Task::none()
                    }
                    Err(e) => {
                        s.busy_pr = None;
                        s.errors.push(e);
                        Task::none()
                    }
                }
            }
            Message::CheckedOut(result) => {
                let Screen::Loading(l) = &mut self.screen else { return Task::none() };
                match result {
                    Ok(log) => {
                        l.log = log;
                        l.step = "Computing diff and highlighting files…".into();
                        Task::perform(load_review(l.repo.clone(), l.pr.clone(), self.settings.dark_theme), Message::ReviewLoaded)
                    }
                    Err(e) => self.back_to_pr_list_with_error(e),
                }
            }
            Message::ReviewLoaded(result) => {
                let Screen::Loading(l) = &mut self.screen else { return Task::none() };
                match result {
                    Ok(data) => {
                        let repo = l.repo.clone();
                        let pr = l.pr.clone();
                        let (mut threads, mut errors) = match data.threads {
                            Ok(t) => (t, Vec::new()),
                            Err(e) => (Vec::new(), vec![format!("Could not load review comments.\n\n{e}")]),
                        };
                        let avatar_urls: BTreeSet<String> = threads
                            .iter()
                            .flat_map(|t| &t.comments)
                            .filter_map(|c| c.author.as_ref().map(|a| a.avatar_url.clone()))
                            .filter(|u| !u.is_empty())
                            .collect();
                        let files: Vec<FileEntry> = data
                            .files
                            .into_iter()
                            .map(|f| {
                                let key = ViewedStore::key(&repo, pr.number, &f.file.path);
                                let viewed = self.viewed_store.entries.get(&key) == Some(&f.diff_hash);
                                let file_threads = threads.extract_if(.., |t| t.path == f.file.path).map(ThreadEntry::new).collect();
                                FileEntry {
                                    data: f,
                                    show_full: false,
                                    wrap: None,
                                    collapsed: viewed,
                                    viewed,
                                    annotations: None,
                                    annotating: false,
                                    annotate_error: None,
                                    chat: None,
                                    threads: file_threads,
                                }
                            })
                            .collect();
                        if !threads.is_empty() {
                            errors.push(format!("{} review comment thread(s) are on files that aren't part of the local diff.", threads.len()));
                        }
                        let body = if pr.body.trim().is_empty() { "_No description provided._".to_string() } else { pr.body.clone() };
                        let generation = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        self.screen = Screen::Review(Box::new(Review {
                            repo,
                            pr_body: markdown::Content::parse(&body),
                            pr,
                            show_description: true,
                            merge_base: data.merge_base,
                            branch: data.branch,
                            files,
                            errors,
                            filter: String::new(),
                            hidden_exts: BTreeSet::new(),
                            collapsed_dirs: HashSet::new(),
                            general_chat: Chat::default(),
                            claude: ClaudeState::new(generation),
                            lsp: BTreeMap::new(),
                            lsp_last_result: None,
                            context_menu: None,
                            avatars: HashMap::new(),
                        }));
                        Task::batch(avatar_urls.into_iter().map(|url| {
                            Task::perform(fetch_avatar(url.clone()), move |bytes| Message::AvatarLoaded(url.clone(), bytes))
                        }))
                    }
                    Err(e) => self.back_to_pr_list_with_error(e),
                }
            }
            Message::BackToPrList => {
                let repo = match &self.screen {
                    Screen::Review(r) => r.repo.clone(),
                    Screen::Loading(l) => l.repo.clone(),
                    _ => return Task::none(),
                };
                self.open_repo(repo)
            }

            Message::ToggleTree => {
                self.settings.show_tree = !self.settings.show_tree;
                let _ = self.settings.save();
                Task::none()
            }
            Message::ToggleChat => {
                self.settings.show_chat = !self.settings.show_chat;
                let _ = self.settings.save();
                Task::none()
            }
            Message::CopyText(t) => {
                self.copy_seq += 1;
                let seq = self.copy_seq;
                self.copied = Some((seq, t.clone()));
                Task::batch([
                    iced::clipboard::write(t),
                    Task::perform(tokio::time::sleep(COPIED_FEEDBACK), move |_| Message::CopiedExpired(seq)),
                ])
            }
            Message::CopiedExpired(seq) => {
                if self.copied.as_ref().is_some_and(|(s, _)| *s == seq) {
                    self.copied = None;
                }
                Task::none()
            }

            // -------------------------------------------------------------- review
            other => {
                let dark = self.settings.dark_theme;
                let Screen::Review(r) = &mut self.screen else { return Task::none() };
                review_update(r, other, &self.settings, &mut self.viewed_store, &mut self.modal_error, self.cursor, dark)
            }
        }
    }

    fn back_to_pr_list_with_error(&mut self, e: String) -> Task<Message> {
        let repo = match &self.screen {
            Screen::Loading(l) => l.repo.clone(),
            Screen::Review(r) => r.repo.clone(),
            Screen::PrList(p) => p.repo.clone(),
            Screen::RepoSelect(_) => return Task::none(),
        };
        let task = self.open_repo(repo);
        if let Screen::PrList(s) = &mut self.screen {
            s.errors.push(e);
        }
        task
    }
}

fn rehighlight(r: &Review, dark: bool) -> Task<Message> {
    let inputs: Vec<_> = r
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            (
                i,
                f.data.file.path.clone(),
                f.data.old_text.clone(),
                f.data.new_text.clone(),
                f.data.compact.rows.clone(),
                f.data.full.rows.clone(),
            )
        })
        .collect();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                inputs
                    .into_iter()
                    .map(|(i, path, o, n, cr, fr)| {
                        let (c, f) = highlight_models(&path, &o, &n, &cr, &fr, dark);
                        (i, c, f)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default()
        },
        Message::Rehighlighted,
    )
}

fn review_update(
    r: &mut Review,
    message: Message,
    settings: &Settings,
    viewed_store: &mut ViewedStore,
    modal_error: &mut Option<String>,
    cursor: Point,
    dark: bool,
) -> Task<Message> {
    match message {
        Message::ToggleDescription => {
            r.show_description = !r.show_description;
            Task::none()
        }
        Message::ToggleCollapsed(i) => {
            if let Some(f) = r.files.get_mut(i) {
                f.collapsed = !f.collapsed;
            }
            Task::none()
        }
        Message::ToggleViewed(i, viewed) => {
            if let Some(f) = r.files.get_mut(i) {
                f.viewed = viewed;
                f.collapsed = viewed;
                let key = ViewedStore::key(&r.repo, r.pr.number, f.path());
                if viewed {
                    viewed_store.entries.insert(key, f.data.diff_hash);
                } else {
                    viewed_store.entries.remove(&key);
                }
                viewed_store.save();
            }
            Task::none()
        }
        Message::ToggleFull(i) => {
            if let Some(f) = r.files.get_mut(i)
                && !f.data.binary {
                    f.show_full = !f.show_full;
                }
            Task::none()
        }
        Message::ToggleWrap(i) => {
            if let Some(f) = r.files.get_mut(i) {
                f.wrap = Some(!f.wraps(settings.wrap_lines));
            }
            Task::none()
        }
        Message::ExpandAll | Message::CollapseAll => {
            let collapse = matches!(message, Message::CollapseAll);
            for f in &mut r.files {
                f.collapsed = collapse || f.viewed;
            }
            Task::none()
        }
        Message::FilterChanged(s) => {
            r.filter = s;
            Task::none()
        }
        Message::ToggleExt(ext) => {
            if !r.hidden_exts.remove(&ext) {
                r.hidden_exts.insert(ext);
            }
            Task::none()
        }
        Message::ToggleDir(d) => {
            if !r.collapsed_dirs.remove(&d) {
                r.collapsed_dirs.insert(d);
            }
            Task::none()
        }
        Message::JumpToFile(i) => {
            if let Some(f) = r.files.get_mut(i)
                && !f.viewed {
                    f.collapsed = false;
                }
            iced::advanced::widget::operate(Locate::new(REVIEW_SCROLL_ID, file_section_id(i)))
                .map(|l| Message::ScrollTo(l.map(Located::top)))
        }
        Message::ScrollTo(Some(y)) => operation::scroll_to(REVIEW_SCROLL_ID, operation::AbsoluteOffset { x: 0.0, y }),
        Message::ScrollTo(None) => Task::none(),
        Message::LinkClicked(uri) => {
            open_url(&uri.to_string());
            Task::none()
        }
        Message::OpenUrl(url) => {
            open_url(&url);
            Task::none()
        }
        Message::Rehighlighted(results) => {
            for (i, c, f) in results {
                if let Some(e) = r.files.get_mut(i) {
                    e.data.compact.spans = c;
                    e.data.full.spans = f;
                }
            }
            Task::none()
        }

        // ------------------------------------------------------------- Claude
        Message::ClaudeMenu(i, choice) => {
            let Some(path) = r.files.get(i).map(|f| f.path().to_string()) else { return Task::none() };
            match choice {
                ClaudeMenu::Annotate => {
                    if r.claude.pending_for(Target::Annotate(i)).is_some() {
                        return Task::none();
                    }
                    let prompt = settings::render_template(&settings.annotate_prompt, &r.prompt_vars(&path), "");
                    let f = &mut r.files[i];
                    f.annotating = true;
                    f.annotate_error = None;
                    if !f.viewed {
                        f.collapsed = false;
                    }
                    r.enqueue(ClaudeJob { target: Target::Annotate(i), prompt });
                    Task::none()
                }
                ClaudeMenu::Chat => {
                    let f = &mut r.files[i];
                    if f.chat.is_none() {
                        f.chat = Some(Chat::default());
                    }
                    f.collapsed = false;
                    Task::batch([reveal_file_chat(i, false), operation::focus(file_chat_input_id(i))])
                }
            }
        }
        Message::ClearAnnotations(i) => {
            if let Some(f) = r.files.get_mut(i) {
                f.annotations = None;
                f.annotate_error = None;
            }
            Task::none()
        }
        Message::FileChatEdit(i, action) => {
            if let Some(c) = r.files.get_mut(i).and_then(|f| f.chat.as_mut()) {
                c.input.perform(action);
            }
            Task::none()
        }
        Message::GeneralChatEdit(action) => {
            r.general_chat.input.perform(action);
            Task::none()
        }
        Message::FileChatSend(i) => send_chat(r, Target::File(i), settings),
        Message::GeneralChatSend => send_chat(r, Target::General, settings),
        Message::CloseFileChat(i) => {
            if r.claude.pending_for(Target::File(i)).is_none()
                && let Some(f) = r.files.get_mut(i) {
                    f.chat = None;
                }
            Task::none()
        }
        Message::StopClaude => {
            if let (Some(tx), Some(_)) = (&r.claude.sender, &r.claude.in_flight) {
                let _ = tx.send(Outgoing::Interrupt);
                r.claude.activity = "interrupting…".into();
                r.claude.interrupting = true;
            }
            Task::none()
        }
        Message::RestartClaude => {
            r.fail_all_jobs("Claude was restarted.");
            r.claude = ClaudeState::new(r.claude.generation + 1);
            Task::none()
        }
        Message::Claude(event) => claude_event(r, event, modal_error),
        Message::PermissionDecision { allow, always } => {
            if let Some(p) = r.claude.permissions.pop_front() {
                if let Some(tx) = &r.claude.sender {
                    let _ = tx.send(Outgoing::Permission {
                        request_id: p.request_id,
                        allow,
                        input: p.input,
                        updated_permissions: if always { p.suggestions } else { None },
                    });
                }
                r.claude.activity = if allow { format!("running {}…", p.tool_name) } else { "permission denied".into() };
            }
            Task::none()
        }

        // ---------------------------------------------------------------- LSP
        Message::StartLsp(server) => {
            let st = r.lsp.entry(server).or_default();
            if st.exited.is_some() {
                // Restart after an exit.
                *st = LspState::default();
            }
            if st.status.is_empty() {
                st.status = "starting…".into();
            }
            Task::none()
        }
        Message::StopLsp(server) => {
            r.lsp.remove(&server);
            Task::none()
        }
        Message::Lsp(event) => {
            match event {
                lsp::Event::Status(s, msg) => {
                    if let Some(st) = r.lsp.get_mut(&s) {
                        st.status = msg;
                    }
                }
                lsp::Event::Ready(s, tx) => {
                    if let Some(st) = r.lsp.get_mut(&s) {
                        st.sender = Some(tx);
                    }
                }
                lsp::Event::Progress { server, token, title, message, percentage, done } => {
                    if let Some(st) = r.lsp.get_mut(&server) {
                        if done {
                            st.progress.remove(&token);
                        } else {
                            let e = st.progress.entry(token).or_insert_with(|| (String::new(), String::new(), None));
                            if !title.is_empty() {
                                e.0 = title;
                            }
                            e.1 = message;
                            if percentage.is_some() {
                                e.2 = percentage;
                            }
                        }
                    }
                }
                lsp::Event::Definition { server, summary } => {
                    r.lsp_last_result = Some(format!("{server}: {summary}"));
                }
                lsp::Event::Exited(s, msg) => {
                    if let Some(st) = r.lsp.get_mut(&s) {
                        st.sender = None;
                        st.progress.clear();
                        st.status = "stopped".into();
                        st.exited = Some(msg.clone());
                    }
                    r.errors.push(format!("Language server stopped: {msg}"));
                }
            }
            Task::none()
        }
        Message::CodeContext(file, full, selection) => {
            r.context_menu = Some(ContextMenu { position: cursor, file, full, selection });
            Task::none()
        }
        Message::ToggleThread(i, t) => {
            if let Some(th) = r.files.get_mut(i).and_then(|f| f.threads.get_mut(t)) {
                th.expanded = !th.expanded;
            }
            Task::none()
        }
        Message::AvatarLoaded(url, bytes) => {
            if let Some(bytes) = bytes {
                r.avatars.insert(url, iced::widget::image::Handle::from_bytes(bytes));
            }
            Task::none()
        }
        Message::CloseContextMenu => {
            r.context_menu = None;
            Task::none()
        }
        Message::CopySelection => {
            match r.context_menu.take() {
                Some(m) => iced::clipboard::write(m.selection.text),
                None => Task::none(),
            }
        }
        Message::GotoDefinition => {
            let Some(menu) = r.context_menu.take() else { return Task::none() };
            goto_definition(r, menu);
            Task::none()
        }
        _ => {
            let _ = dark;
            Task::none()
        }
    }
}

fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = std::process::Command::new(opener).arg(url).spawn() {
        eprintln!("could not open {url}: {e}");
    }
}

fn send_chat(r: &mut Review, target: Target, settings: &Settings) -> Task<Message> {
    let (path, template) = match target {
        Target::File(i) => match r.files.get(i) {
            Some(f) => (f.path().to_string(), &settings.file_chat_meta),
            None => return Task::none(),
        },
        _ => (String::new(), &settings.general_chat_meta),
    };
    let vars = r.prompt_vars(&path);
    let Some(chat) = r.chat_mut(target) else { return Task::none() };
    let text = editor_text(&chat.input).trim().to_string();
    if text.is_empty() {
        return Task::none();
    }
    chat.input = text_editor::Content::new();
    // The user's own message is shown; the meta prompt wrapped around it is not.
    chat.entries.push(ChatEntry::User(text.clone()));
    let prompt = settings::render_template(template, &vars, &text);
    r.enqueue(ClaudeJob { target, prompt });
    scroll_chat_to_end(target)
}

fn assistant_entry<'a>(chat: &'a mut Chat, msg_id: &str) -> &'a mut ChatEntry {
    let pos = chat
        .entries
        .iter()
        .rposition(|e| matches!(e, ChatEntry::Assistant { msg_id: id, .. } if id == msg_id));
    // Only reuse the entry if it is the last one (otherwise tool calls happened in between).
    match pos {
        Some(p) if p + 1 == chat.entries.len() => &mut chat.entries[p],
        _ => {
            chat.entries.push(ChatEntry::Assistant {
                msg_id: msg_id.to_string(),
                text: String::new(),
                md: markdown::Content::new(),
            });
            chat.entries.last_mut().expect("just pushed")
        }
    }
}

fn claude_event(r: &mut Review, event: claude::Event, modal_error: &mut Option<String>) -> Task<Message> {
    use claude::Event as E;
    let target = r.claude.in_flight.as_ref().map(|j| j.target);
    match event {
        E::Started(tx) => {
            r.claude.sender = Some(tx);
            r.claude.status = ClaudeStatus::Ready;
            r.pump();
            Task::none()
        }
        E::Init { session_id, model, permission_mode } => {
            r.claude.session_id = session_id;
            r.claude.model = model;
            r.claude.permission_mode = permission_mode;
            Task::none()
        }
        E::MessageStart { id } => {
            r.claude.current_msg = id;
            r.claude.activity = "writing…".into();
            Task::none()
        }
        E::TextDelta { text } => {
            let msg_id = r.claude.current_msg.clone();
            if let Some(t @ (Target::File(_) | Target::General)) = target
                && let Some(chat) = r.chat_mut(t) {
                    if let ChatEntry::Assistant { text: buf, md, .. } = assistant_entry(chat, &msg_id) {
                        buf.push_str(&text);
                        md.push_str(&text);
                    }
                    return scroll_chat_to_end(t);
                }
            Task::none()
        }
        E::AssistantText { id, text } => {
            if let Some(t @ (Target::File(_) | Target::General)) = target
                && let Some(chat) = r.chat_mut(t) {
                    let streamed = chat
                        .entries
                        .iter()
                        .any(|e| matches!(e, ChatEntry::Assistant { msg_id, text, .. } if *msg_id == id && !text.is_empty()));
                    if !streamed {
                        if let ChatEntry::Assistant { text: buf, md, .. } = assistant_entry(chat, &id) {
                            buf.push_str(&text);
                            *md = markdown::Content::parse(buf);
                        }
                        return scroll_chat_to_end(t);
                    }
                }
            Task::none()
        }
        E::ToolUse { name, input, .. } => {
            let summary = claude::tool_summary(&name, &input);
            r.claude.activity = format!("using {summary}");
            if let Some(t @ (Target::File(_) | Target::General)) = target
                && let Some(chat) = r.chat_mut(t) {
                    chat.entries.push(ChatEntry::Tool(summary));
                    return scroll_chat_to_end(t);
                }
            Task::none()
        }
        E::ToolResult { is_error, content, .. } => {
            if is_error
                && let Some(t @ (Target::File(_) | Target::General)) = target
                    && let Some(chat) = r.chat_mut(t) {
                        let first = content.lines().next().unwrap_or_default().chars().take(200).collect::<String>();
                        chat.entries.push(ChatEntry::Tool(format!("✗ {first}")));
                    }
            r.claude.activity = "thinking…".into();
            Task::none()
        }
        E::Result { is_error, subtype, text, cost_usd } => {
            if let Some(c) = cost_usd {
                r.claude.cost = c;
            }
            r.claude.activity.clear();
            let interrupted = std::mem::take(&mut r.claude.interrupting);
            let Some(job) = r.claude.in_flight.take() else { return Task::none() };
            let mut task = Task::none();
            match job.target {
                Target::Annotate(i) => {
                    if let Some(f) = r.files.get_mut(i) {
                        f.annotating = false;
                        if interrupted {
                            f.annotate_error = Some("Annotation was interrupted.".into());
                        } else if is_error {
                            f.annotate_error = Some(format!("Claude reported an error ({subtype}): {text}"));
                        } else {
                            match claude::parse_annotations(&text) {
                                Ok(map) if !map.is_empty() => f.annotations = Some(map),
                                Ok(_) => f.annotate_error = Some("Claude returned no annotations.".into()),
                                Err(e) => {
                                    eprintln!("[annotate] could not parse response:\n{text}");
                                    f.annotate_error = Some(e);
                                }
                            }
                        }
                    }
                }
                t => {
                    if let Some(chat) = r.chat_mut(t) {
                        if interrupted {
                            chat.entries.push(ChatEntry::Tool("■ interrupted".into()));
                        } else if is_error {
                            let detail = if text.is_empty() { subtype } else { text };
                            chat.entries.push(ChatEntry::Error(format!("Claude: {detail}")));
                        } else if !chat.entries.iter().rev().take_while(|e| !matches!(e, ChatEntry::User(_))).any(|e| matches!(e, ChatEntry::Assistant { .. })) && !text.is_empty() {
                            // No streamed text arrived (e.g. streaming disabled): show the final result.
                            chat.entries.push(ChatEntry::Assistant { msg_id: String::new(), md: markdown::Content::parse(&text), text });
                        }
                    }
                    task = scroll_chat_to_end(t);
                }
            }
            r.pump();
            task
        }
        E::Permission(p) => {
            r.claude.activity = format!("waiting for permission: {}", p.tool_name);
            r.claude.permissions.push_back(p);
            Task::none()
        }
        E::CancelPermission { request_id } => {
            r.claude.permissions.retain(|p| p.request_id != request_id);
            Task::none()
        }
        E::Stderr(line) => {
            let _ = line;
            Task::none()
        }
        E::Exited(msg) => {
            r.claude.sender = None;
            r.claude.status = ClaudeStatus::Exited(msg.clone());
            r.claude.permissions.clear();
            r.fail_all_jobs("The Claude process exited.");
            *modal_error = Some(format!(
                "{msg}\n\nCheck the Claude command in Settings, then use \"Restart\" in the status bar."
            ));
            Task::none()
        }
    }
}

fn goto_definition(r: &mut Review, menu: ContextMenu) {
    let Some(f) = r.files.get(menu.file) else { return };
    let path = f.path().to_string();
    let model = if menu.full { &f.data.full } else { &f.data.compact };
    let sel = &menu.selection;
    // Skip leading whitespace of the selection to land on the identifier.
    let lead = sel.text.chars().take_while(|c| c.is_whitespace() && *c != '\n').count();
    let (row_idx, col) = if sel.text.starts_with('\n') { (sel.start.row + 1, 0) } else { (sel.start.row, sel.start.col + lead) };
    let word = sel.text.trim().lines().next().unwrap_or_default().to_string();
    let Some(row) = model.rows.get(row_idx) else { return };
    let Some(new_no) = row.new_no.filter(|_| row.kind != RowKind::Removed) else {
        println!("[lsp] go to definition: `{word}` is on a removed line (old file only); only lines present in the PR's version can be resolved.");
        r.lsp_last_result = Some("Go to definition: only works on lines present in the new version".into());
        return;
    };
    let Some((server, tx)) = r
        .lsp
        .iter()
        .find(|(s, st)| s.handles(&path) && st.exited.is_none())
        .map(|(s, st)| (*s, st.sender.clone()))
    else {
        println!("[lsp] go to definition: no running language server handles {path}. Start one from the LSP menu.");
        r.lsp_last_result = Some(format!("No running language server for {path}"));
        return;
    };
    let char_idx = diff::display_col_to_char(&row.original, col);
    let character = diff::char_to_utf16(&row.original, char_idx);
    let req = lsp::Request::GotoDefinition { path: path.clone(), line: new_no - 1, character, word: word.clone() };
    match tx {
        Some(tx) => {
            println!("[lsp] {server}: textDocument/definition `{word}` at {path}:{new_no}:{}", character + 1);
            let _ = tx.send(req);
            r.lsp_last_result = Some(format!("{server}: looking up `{word}`…"));
        }
        None => {
            println!("[lsp] {server} is still starting; try again in a moment.");
            r.lsp_last_result = Some(format!("{server} is still starting"));
        }
    }
}
