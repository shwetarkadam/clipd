use chrono::Utc;
use clipd_core::{
    all_transforms, apply_transform, compute_sessions, detect_sensitive, generate_embedding,
    is_embedding_available, load_privacy_config, load_theme, load_transform_config,
    save_privacy_config, save_theme, search_embeddings, ClipEntry, ClipStore, ContentType,
    Embedding, PrivacyConfig, Rgb, Session, SessionConfig, TfIdfIndex, Theme, TransformConfig,
    TransformKind,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;

fn color(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

#[derive(PartialEq)]
enum TuiMode {
    Normal,
    TransformPicker,
    TransformInput,
    TransformResult,
    SessionView,
    Settings,
}

#[derive(PartialEq, Clone, Copy)]
enum QuickFilter {
    All,
    Code,
    Links,
    Slots,
    Text,
}

impl QuickFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Code,
            Self::Code => Self::Links,
            Self::Links => Self::Slots,
            Self::Slots => Self::Text,
            Self::Text => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Code => "Code",
            Self::Links => "Links",
            Self::Slots => "Slots",
            Self::Text => "Text",
        }
    }

    fn matches(self, clip: &ClipEntry) -> bool {
        match self {
            Self::All => true,
            Self::Code => clip.content_type == ContentType::Code,
            Self::Links => matches!(
                clip.content_type,
                ContentType::Url | ContentType::Email | ContentType::Path
            ),
            Self::Slots => clip.slot.is_some(),
            Self::Text => clip.content_type == ContentType::Text,
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum SettingsRow {
    Enabled,
    DetectApiKeys,
    DetectCredentials,
    DetectCreditCards,
    DetectSsn,
    ExcludedApps,
    CustomPatterns,
    AddExcludedApp,
    AddCustomPattern,
    Save,
    Reset,
}

impl SettingsRow {
    fn label(&self) -> &str {
        match self {
            Self::Enabled => "Privacy Protection",
            Self::DetectApiKeys => "Detect API Keys",
            Self::DetectCredentials => "Detect Passwords / Secrets",
            Self::DetectCreditCards => "Detect Credit Cards",
            Self::DetectSsn => "Detect SSN",
            Self::ExcludedApps => "── Excluded Apps ──",
            Self::CustomPatterns => "── Custom Skip Patterns ──",
            Self::AddExcludedApp => "+ Add excluded app…",
            Self::AddCustomPattern => "+ Add custom pattern…",
            Self::Save => "💾 Save Settings",
            Self::Reset => "↺  Reset to Defaults",
        }
    }
}

struct App {
    store: ClipStore,
    clips: Vec<ClipEntry>,
    filtered: Vec<usize>,
    search_input: String,
    list_state: ListState,
    selected_clip: Option<ClipEntry>,
    should_quit: bool,
    status_message: Option<(String, Color)>,
    matcher: SkimMatcherV2,
    theme: Theme,

    mode: TuiMode,
    transforms: Vec<TransformKind>,
    transform_selected: usize,
    transform_result: Option<String>,
    transform_error: Option<String>,
    transform_config: TransformConfig,
    custom_prompt_input: String,
    result_scroll: u16,

    semantic_mode: bool,
    quick_filter: QuickFilter,
    source_filter: Option<String>,
    cached_embeddings: Vec<(i64, Embedding)>,
    privacy_config: PrivacyConfig,
    sessions: Vec<Session>,
    session_config: SessionConfig,
    session_selected: usize,

    settings_cursor: usize,
    settings_editing: bool,
    settings_input: String,
    settings_dirty: bool,
}

impl App {
    fn new(store: ClipStore) -> Self {
        let clips = store.get_recent(500).unwrap_or_default();
        let filtered: Vec<usize> = (0..clips.len()).collect();
        let mut list_state = ListState::default();
        if !filtered.is_empty() {
            list_state.select(Some(0));
        }
        let selected_clip = clips.first().cloned();
        let theme = load_theme();
        let transform_config = load_transform_config();
        let privacy_config = load_privacy_config();
        let session_config = SessionConfig::default();
        let sessions = compute_sessions(&clips, session_config.window_minutes);

        App {
            store,
            clips,
            filtered,
            search_input: String::new(),
            list_state,
            selected_clip,
            should_quit: false,
            status_message: None,
            matcher: SkimMatcherV2::default(),
            theme,
            mode: TuiMode::Normal,
            transforms: all_transforms(),
            transform_selected: 0,
            transform_result: None,
            transform_error: None,
            transform_config,
            custom_prompt_input: String::new(),
            result_scroll: 0,
            semantic_mode: false,
            quick_filter: QuickFilter::All,
            source_filter: None,
            cached_embeddings: Vec::new(),
            privacy_config,
            sessions,
            session_config,
            session_selected: 0,
            settings_cursor: 0,
            settings_editing: false,
            settings_input: String::new(),
            settings_dirty: false,
        }
    }

    fn filter_clips(&mut self) {
        let mut filtered = if self.search_input.is_empty() {
            (0..self.clips.len()).collect()
        } else if self.semantic_mode {
            let mut used_vectors = false;
            let mut semantic_matches: Vec<usize> = Vec::new();

            if !self.cached_embeddings.is_empty()
                && is_embedding_available(&self.transform_config)
                && self.search_input.len() >= 3
            {
                if let Ok(query_emb) =
                    generate_embedding(&self.search_input, &self.transform_config)
                {
                    let results =
                        search_embeddings(&query_emb, &self.cached_embeddings, 50, 0.3);
                    if !results.is_empty() {
                        let id_to_idx: std::collections::HashMap<i64, usize> = self
                            .clips
                            .iter()
                            .enumerate()
                            .map(|(i, c)| (c.id, i))
                            .collect();
                        semantic_matches = results
                            .iter()
                            .filter_map(|r| id_to_idx.get(&r.clip_id).copied())
                            .collect();
                        used_vectors = true;
                    }
                }
            }

            if !used_vectors {
                let docs: Vec<&str> =
                    self.clips.iter().map(|c| c.content.as_str()).collect();
                let index = TfIdfIndex::build(&docs);
                let results = index.search(&self.search_input, 50);
                semantic_matches = results.iter().map(|r| r.clip_index).collect();
            }

            semantic_matches
        } else {
            let mut fuzzy: Vec<usize> = self
                .clips
                .iter()
                .enumerate()
                .filter(|(_, clip)| {
                    self.matcher
                        .fuzzy_match(&clip.content, &self.search_input)
                        .is_some()
                        || self
                            .matcher
                            .fuzzy_match(&clip.preview, &self.search_input)
                            .is_some()
                })
                .map(|(i, _)| i)
                .collect();

            if fuzzy.is_empty() && self.search_input.len() >= 3 {
                let docs: Vec<&str> = self.clips.iter().map(|c| c.content.as_str()).collect();
                let index = TfIdfIndex::build(&docs);
                let results = index.search(&self.search_input, 50);
                fuzzy = results.iter().map(|r| r.clip_index).collect();
            }

            fuzzy
        };

        filtered.retain(|&idx| {
            let clip = &self.clips[idx];
            self.quick_filter.matches(clip)
                && self
                    .source_filter
                    .as_deref()
                    .map_or(true, |app| clip.source_app.as_deref() == Some(app))
        });
        self.filtered = filtered;

        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
            self.sync_selection();
        } else {
            self.list_state.select(None);
            self.selected_clip = None;
        }
    }

    fn select_visible(&mut self, visible_index: usize) -> bool {
        if visible_index >= self.filtered.len() {
            return false;
        }
        self.list_state.select(Some(visible_index));
        self.sync_selection();
        self.copy_selected()
    }

    fn jump_to_start(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
            self.sync_selection();
        }
    }

    fn jump_to_end(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(self.filtered.len() - 1));
            self.sync_selection();
        }
    }

    fn cycle_quick_filter(&mut self) {
        self.quick_filter = self.quick_filter.next();
        self.filter_clips();
        self.status_message = Some((
            format!("Filter: {}", self.quick_filter.label()),
            color(self.theme.colors().accent),
        ));
    }

    fn cycle_source_filter(&mut self) {
        let sources = self.top_sources();
        if sources.is_empty() {
            self.source_filter = None;
            return;
        }

        self.source_filter = match self.source_filter.as_deref() {
            None => sources.first().cloned(),
            Some(current) => {
                let next = sources
                    .iter()
                    .position(|s| s == current)
                    .map(|idx| idx + 1)
                    .unwrap_or(0);
                sources.get(next).cloned()
            }
        };
        self.filter_clips();
        self.status_message = Some((
            format!(
                "Source: {}",
                self.source_filter.as_deref().unwrap_or("All apps")
            ),
            color(self.theme.colors().accent),
        ));
    }

    fn top_sources(&self) -> Vec<String> {
        let mut sources: Vec<String> = Vec::new();
        for clip in &self.clips {
            let Some(app) = clip.source_app.as_deref() else {
                continue;
            };
            if app.trim().is_empty() || sources.iter().any(|s| s == app) {
                continue;
            }
            sources.push(app.to_string());
            if sources.len() >= 12 {
                break;
            }
        }
        sources
    }

    fn active_filter_label(&self) -> String {
        match self.source_filter.as_deref() {
            Some(app) if self.quick_filter != QuickFilter::All => {
                format!("{} · {}", self.quick_filter.label(), app)
            }
            Some(app) => app.to_string(),
            None => self.quick_filter.label().to_string(),
        }
    }

    fn sync_selection(&mut self) {
        if let Some(sel) = self.list_state.selected() {
            if let Some(&idx) = self.filtered.get(sel) {
                self.selected_clip = self.clips.get(idx).cloned();
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as i32;
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len - 1) as usize;
        self.list_state.select(Some(next));
        self.sync_selection();
    }

    fn copy_selected(&mut self) -> bool {
        if let Some(ref clip) = self.selected_clip {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if cb.set_text(&clip.content).is_ok() {
                    return true;
                }
            }
        }
        false
    }

    fn copy_text(&self, text: &str) -> bool {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            return cb.set_text(text).is_ok();
        }
        false
    }

    fn delete_selected(&mut self) {
        if let Some(ref clip) = self.selected_clip {
            let id = clip.id;
            if self.store.delete(id).unwrap_or(false) {
                self.clips = self.store.get_recent(500).unwrap_or_default();
                self.filter_clips();
                self.status_message = Some((
                    "🗑️  Deleted".into(),
                    color(self.theme.colors().green),
                ));
            }
        }
    }

    fn cycle_theme(&mut self) {
        self.theme = self.theme.next();
        save_theme(self.theme);
        self.status_message = Some((
            format!("Theme: {}", self.theme.label()),
            color(self.theme.colors().accent),
        ));
    }

    fn enter_transform_mode(&mut self) {
        if self.selected_clip.is_some() {
            self.mode = TuiMode::TransformPicker;
            self.transform_selected = 0;
            self.transform_result = None;
            self.transform_error = None;
        }
    }

    fn apply_selected_transform(&mut self) {
        let kind = self.transforms[self.transform_selected].clone();

        if let TransformKind::CustomPrompt(_) = kind {
            self.mode = TuiMode::TransformInput;
            self.custom_prompt_input.clear();
            return;
        }

        self.run_transform(&kind);
    }

    fn run_transform(&mut self, kind: &TransformKind) {
        let input = match &self.selected_clip {
            Some(clip) => clip.content.clone(),
            None => return,
        };

        match apply_transform(kind, &input, &self.transform_config) {
            Ok(result) => {
                self.transform_result = Some(result);
                self.transform_error = None;
                self.result_scroll = 0;
                self.mode = TuiMode::TransformResult;
            }
            Err(err) => {
                self.transform_error = Some(err);
                self.transform_result = None;
            }
        }
    }

    fn move_transform_selection(&mut self, delta: i32) {
        let len = self.transforms.len() as i32;
        let cur = self.transform_selected as i32;
        self.transform_selected = (cur + delta).clamp(0, len - 1) as usize;
    }
}

/// Run the interactive TUI search interface.
pub fn run_tui() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = ClipStore::default_path();
    let store = ClipStore::new(&db_path)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(store);

    loop {
        terminal.draw(|f| draw_ui(f, &mut app))?;

        if app.should_quit {
            break;
        }

        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match app.mode {
                    TuiMode::Normal => handle_normal_keys(&mut app, key),
                    TuiMode::TransformPicker => handle_transform_picker_keys(&mut app, key),
                    TuiMode::TransformInput => handle_transform_input_keys(&mut app, key),
                    TuiMode::TransformResult => handle_transform_result_keys(&mut app, key),
                    TuiMode::SessionView => handle_session_keys(&mut app, key),
                    TuiMode::Settings => handle_settings_keys(&mut app, key),
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn handle_normal_keys(app: &mut App, key: crossterm::event::KeyEvent) {
    app.status_message = None;

    match key.code {
        KeyCode::Esc => app.should_quit = true,
        KeyCode::Enter => {
            if app.copy_selected() {
                app.should_quit = true;
            }
        }
        KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('k') if key.modifiers.is_empty() => app.move_selection(-1),
        KeyCode::Down => app.move_selection(1),
        KeyCode::Char('j') if key.modifiers.is_empty() => app.move_selection(1),
        KeyCode::PageUp => app.move_selection(-10),
        KeyCode::PageDown => app.move_selection(10),
        KeyCode::Home => app.jump_to_start(),
        KeyCode::Char('g') if key.modifiers.is_empty() => app.jump_to_start(),
        KeyCode::End => app.jump_to_end(),
        KeyCode::Char('G') if key.modifiers.is_empty() => app.jump_to_end(),
        KeyCode::Tab => app.cycle_quick_filter(),
        KeyCode::BackTab => app.cycle_source_filter(),
        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cycle_theme();
        }
        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.enter_transform_mode();
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.delete_selected();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.search_input.clear();
            app.filter_clips();
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.sessions = compute_sessions(&app.clips, app.session_config.window_minutes);
            app.session_selected = 0;
            app.mode = TuiMode::SessionView;
        }
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.semantic_mode = !app.semantic_mode;
            if app.semantic_mode && app.cached_embeddings.is_empty() {
                app.cached_embeddings = app.store.get_all_embeddings().unwrap_or_default();
            }
            let has_vectors = !app.cached_embeddings.is_empty();
            let label = if app.semantic_mode {
                if has_vectors { "Semantic (vectors)" } else { "Semantic (TF-IDF)" }
            } else {
                "Fuzzy"
            };
            app.status_message = Some((
                format!("🔍 Search mode: {}", label),
                color(app.theme.colors().accent),
            ));
            app.filter_clips();
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.quick_filter = QuickFilter::All;
            app.source_filter = None;
            app.filter_clips();
            app.status_message = Some((
                "Filters cleared".into(),
                color(app.theme.colors().accent),
            ));
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.settings_cursor = 0;
            app.settings_editing = false;
            app.settings_input.clear();
            app.settings_dirty = false;
            app.mode = TuiMode::Settings;
        }
        KeyCode::Backspace => {
            app.search_input.pop();
            app.filter_clips();
        }
        KeyCode::Char(c)
            if app.search_input.is_empty()
                && c.is_ascii_digit()
                && c != '0'
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let visible_index = (c as u8 - b'1') as usize;
            if app.select_visible(visible_index) {
                app.should_quit = true;
            }
        }
        KeyCode::Char(c) => {
            app.search_input.push(c);
            app.filter_clips();
        }
        _ => {}
    }
}

fn handle_transform_picker_keys(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.mode = TuiMode::Normal;
            app.transform_error = None;
        }
        KeyCode::Up => app.move_transform_selection(-1),
        KeyCode::Down => app.move_transform_selection(1),
        KeyCode::Enter => app.apply_selected_transform(),
        _ => {}
    }
}

fn handle_transform_input_keys(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc => app.mode = TuiMode::TransformPicker,
        KeyCode::Enter => {
            if !app.custom_prompt_input.is_empty() {
                let kind = TransformKind::CustomPrompt(app.custom_prompt_input.clone());
                app.run_transform(&kind);
            }
        }
        KeyCode::Backspace => {
            app.custom_prompt_input.pop();
        }
        KeyCode::Char(c) => {
            app.custom_prompt_input.push(c);
        }
        _ => {}
    }
}

fn handle_transform_result_keys(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.mode = TuiMode::TransformPicker;
            app.transform_result = None;
        }
        KeyCode::Enter => {
            if let Some(ref result) = app.transform_result.clone() {
                if app.copy_text(result) {
                    app.should_quit = true;
                }
            }
        }
        KeyCode::Up => app.result_scroll = app.result_scroll.saturating_sub(1),
        KeyCode::Down => app.result_scroll = app.result_scroll.saturating_add(1),
        _ => {}
    }
}

fn handle_session_keys(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc => app.mode = TuiMode::Normal,
        KeyCode::Up => {
            if app.session_selected > 0 {
                app.session_selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.session_selected + 1 < app.sessions.len() {
                app.session_selected += 1;
            }
        }
        KeyCode::Enter => {
            let sel = app.session_selected;
            if let Some(session) = app.sessions.get(sel) {
                let session_ids: std::collections::HashSet<i64> =
                    session.clip_ids.iter().copied().collect();
                let session_name = session.name.clone();
                app.search_input.clear();
                app.filtered = app
                    .clips
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| session_ids.contains(&c.id))
                    .map(|(i, _)| i)
                    .collect();
                if !app.filtered.is_empty() {
                    app.list_state.select(Some(0));
                    app.sync_selection();
                }
                app.mode = TuiMode::Normal;
                app.status_message = Some((
                    format!("📂 Session: {}", session_name),
                    color(app.theme.colors().accent),
                ));
            }
        }
        _ => {}
    }
}

/// Build the flattened list of interactive rows for the settings panel.
/// Each row is an index into this virtual list. The structure:
///   0..4  = toggle rows (Enabled, ApiKeys, Credentials, CreditCards, SSN)
///   5     = "── Excluded Apps ──" header
///   6..6+N-1 = one row per excluded app (deletable)
///   6+N   = "+ Add excluded app…"
///   6+N+1 = "── Custom Skip Patterns ──" header
///   6+N+2..6+N+1+M = one row per custom pattern (deletable)
///   6+N+2+M = "+ Add custom pattern…"
///   last-1 = Save
///   last   = Reset
fn settings_rows(cfg: &PrivacyConfig) -> Vec<SettingsRowKind> {
    let mut rows: Vec<SettingsRowKind> = Vec::new();
    rows.push(SettingsRowKind::Toggle(SettingsRow::Enabled));
    rows.push(SettingsRowKind::Toggle(SettingsRow::DetectApiKeys));
    rows.push(SettingsRowKind::Toggle(SettingsRow::DetectCredentials));
    rows.push(SettingsRowKind::Toggle(SettingsRow::DetectCreditCards));
    rows.push(SettingsRowKind::Toggle(SettingsRow::DetectSsn));
    rows.push(SettingsRowKind::Header(SettingsRow::ExcludedApps));
    for (i, app) in cfg.excluded_apps.iter().enumerate() {
        rows.push(SettingsRowKind::ListItem(ListKind::ExcludedApp, i, app.clone()));
    }
    rows.push(SettingsRowKind::Action(SettingsRow::AddExcludedApp));
    rows.push(SettingsRowKind::Header(SettingsRow::CustomPatterns));
    for (i, pat) in cfg.custom_skip_patterns.iter().enumerate() {
        rows.push(SettingsRowKind::ListItem(ListKind::CustomPattern, i, pat.clone()));
    }
    rows.push(SettingsRowKind::Action(SettingsRow::AddCustomPattern));
    rows.push(SettingsRowKind::Action(SettingsRow::Save));
    rows.push(SettingsRowKind::Action(SettingsRow::Reset));
    rows
}

#[derive(Clone)]
enum SettingsRowKind {
    Toggle(SettingsRow),
    Header(SettingsRow),
    ListItem(ListKind, usize, String),
    Action(SettingsRow),
}

#[derive(Clone, Copy, PartialEq)]
enum ListKind {
    ExcludedApp,
    CustomPattern,
}

fn handle_settings_keys(app: &mut App, key: crossterm::event::KeyEvent) {
    let rows = settings_rows(&app.privacy_config);
    let row_count = rows.len();

    if app.settings_editing {
        match key.code {
            KeyCode::Esc => {
                app.settings_editing = false;
                app.settings_input.clear();
            }
            KeyCode::Enter => {
                let input = app.settings_input.trim().to_string();
                if !input.is_empty() {
                    if let Some(row) = rows.get(app.settings_cursor) {
                        match row {
                            SettingsRowKind::Action(SettingsRow::AddExcludedApp) => {
                                app.privacy_config.excluded_apps.push(input);
                                app.settings_dirty = true;
                            }
                            SettingsRowKind::Action(SettingsRow::AddCustomPattern) => {
                                app.privacy_config.custom_skip_patterns.push(input);
                                app.settings_dirty = true;
                            }
                            _ => {}
                        }
                    }
                }
                app.settings_editing = false;
                app.settings_input.clear();
            }
            KeyCode::Backspace => {
                app.settings_input.pop();
            }
            KeyCode::Char(c) => {
                app.settings_input.push(c);
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Esc => {
            if app.settings_dirty {
                save_privacy_config(&app.privacy_config);
            }
            app.mode = TuiMode::Normal;
            if app.settings_dirty {
                app.status_message = Some((
                    "🔒 Privacy settings saved".into(),
                    color(app.theme.colors().green),
                ));
            }
        }
        KeyCode::Up => {
            if app.settings_cursor > 0 {
                app.settings_cursor -= 1;
                // skip headers
                if let Some(SettingsRowKind::Header(_)) = rows.get(app.settings_cursor) {
                    if app.settings_cursor > 0 {
                        app.settings_cursor -= 1;
                    }
                }
            }
        }
        KeyCode::Down => {
            if app.settings_cursor + 1 < row_count {
                app.settings_cursor += 1;
                if let Some(SettingsRowKind::Header(_)) = rows.get(app.settings_cursor) {
                    if app.settings_cursor + 1 < row_count {
                        app.settings_cursor += 1;
                    }
                }
            }
        }
        KeyCode::Char(' ') | KeyCode::Enter => {
            if let Some(row) = rows.get(app.settings_cursor) {
                match row {
                    SettingsRowKind::Toggle(SettingsRow::Enabled) => {
                        app.privacy_config.enabled = !app.privacy_config.enabled;
                        app.settings_dirty = true;
                    }
                    SettingsRowKind::Toggle(SettingsRow::DetectApiKeys) => {
                        app.privacy_config.detect_api_keys = !app.privacy_config.detect_api_keys;
                        app.settings_dirty = true;
                    }
                    SettingsRowKind::Toggle(SettingsRow::DetectCredentials) => {
                        app.privacy_config.detect_credentials =
                            !app.privacy_config.detect_credentials;
                        app.settings_dirty = true;
                    }
                    SettingsRowKind::Toggle(SettingsRow::DetectCreditCards) => {
                        app.privacy_config.detect_credit_cards =
                            !app.privacy_config.detect_credit_cards;
                        app.settings_dirty = true;
                    }
                    SettingsRowKind::Toggle(SettingsRow::DetectSsn) => {
                        app.privacy_config.detect_ssn = !app.privacy_config.detect_ssn;
                        app.settings_dirty = true;
                    }
                    SettingsRowKind::Action(SettingsRow::AddExcludedApp)
                    | SettingsRowKind::Action(SettingsRow::AddCustomPattern) => {
                        app.settings_editing = true;
                        app.settings_input.clear();
                    }
                    SettingsRowKind::Action(SettingsRow::Save) => {
                        save_privacy_config(&app.privacy_config);
                        app.settings_dirty = false;
                        app.mode = TuiMode::Normal;
                        app.status_message = Some((
                            "🔒 Privacy settings saved".into(),
                            color(app.theme.colors().green),
                        ));
                    }
                    SettingsRowKind::Action(SettingsRow::Reset) => {
                        app.privacy_config = PrivacyConfig::default();
                        app.settings_dirty = true;
                    }
                    _ => {}
                }
            }
        }
        KeyCode::Delete | KeyCode::Char('x') => {
            if let Some(row) = rows.get(app.settings_cursor).cloned() {
                match row {
                    SettingsRowKind::ListItem(ListKind::ExcludedApp, idx, _) => {
                        if idx < app.privacy_config.excluded_apps.len() {
                            app.privacy_config.excluded_apps.remove(idx);
                            app.settings_dirty = true;
                            let new_rows = settings_rows(&app.privacy_config);
                            if app.settings_cursor >= new_rows.len() {
                                app.settings_cursor = new_rows.len().saturating_sub(1);
                            }
                        }
                    }
                    SettingsRowKind::ListItem(ListKind::CustomPattern, idx, _) => {
                        if idx < app.privacy_config.custom_skip_patterns.len() {
                            app.privacy_config.custom_skip_patterns.remove(idx);
                            app.settings_dirty = true;
                            let new_rows = settings_rows(&app.privacy_config);
                            if app.settings_cursor >= new_rows.len() {
                                app.settings_cursor = new_rows.len().saturating_sub(1);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

// ── Drawing ──

fn draw_ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(f.area());

    draw_search(f, app, chunks[0]);

    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);

    draw_list(f, app, content[0]);
    draw_preview(f, app, content[1]);
    draw_status(f, app, chunks[2]);

    match app.mode {
        TuiMode::TransformPicker => draw_transform_picker(f, app),
        TuiMode::TransformInput => draw_transform_input(f, app),
        TuiMode::TransformResult => draw_transform_result(f, app),
        TuiMode::SessionView => draw_sessions(f, app),
        TuiMode::Settings => draw_settings(f, app),
        TuiMode::Normal => {}
    }
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let c = app.theme.colors();
    let cursor = "│";
    let mode_badge = if app.semantic_mode { " 🧠 Semantic " } else { " 🔍 Fuzzy " };
    let title = format!("{} clipd ", mode_badge);

    let query = if app.search_input.is_empty() {
        " type to search".to_string()
    } else {
        format!(" {}", app.search_input)
    };
    let query_style = if app.search_input.is_empty() {
        Style::default().fg(color(c.overlay))
    } else {
        Style::default().fg(color(c.text))
    };
    let filter_label = app.active_filter_label();
    let spans: Vec<Span> = vec![
        Span::styled(format!("{}{}", query, cursor), query_style),
        Span::raw("  "),
        Span::styled(" Tab ", Style::default().fg(color(c.bg_base)).bg(color(c.accent))),
        Span::styled("type", Style::default().fg(color(c.subtext))),
        Span::raw("  "),
        Span::styled(" S-Tab ", Style::default().fg(color(c.bg_base)).bg(color(c.accent2))),
        Span::styled("app", Style::default().fg(color(c.subtext))),
        Span::raw("  "),
        Span::styled(
            format!(" {} ", filter_label),
            Style::default()
                .fg(color(c.bg_base))
                .bg(color(c.green))
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color(c.accent)));

    let widget = Paragraph::new(Line::from(spans)).block(block);

    f.render_widget(widget, area);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let c = app.theme.colors();
    let inner_w = (area.width as usize).saturating_sub(4);

    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .enumerate()
        .map(|(visible_idx, &idx)| {
            let clip = &app.clips[idx];
            let icon = clip.content_type.icon();
            let time = relative_time(&clip.timestamp);

            let sensitive = !detect_sensitive(&clip.content, &app.privacy_config).is_empty();
            let badge = if sensitive { "🔒" } else { "" };

            let time_w = time.chars().count();
            let badge_w = if sensitive { 3 } else { 0 };
            let pick_w = if visible_idx < 9 { 4 } else { 2 };
            let max_preview = inner_w.saturating_sub(time_w + badge_w + pick_w + 4);
            let preview = truncate(&clip.preview, max_preview);
            let preview_w = preview.chars().count();
            let gap = inner_w.saturating_sub(preview_w + time_w + badge_w + pick_w + 2);

            let pick = if visible_idx < 9 {
                Span::styled(
                    format!("{} ", visible_idx + 1),
                    Style::default().fg(color(c.accent)),
                )
            } else {
                Span::raw("  ")
            };

            let mut spans = vec![
                pick,
                Span::styled(format!("{} ", icon), Style::default()),
                Span::styled(preview, Style::default().fg(color(c.text))),
            ];
            if sensitive {
                spans.push(Span::styled(
                    format!(" {}", badge),
                    Style::default().fg(Color::Yellow),
                ));
            }
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(time, Style::default().fg(color(c.accent))));

            ListItem::new(Line::from(spans))
        })
        .collect();

    let count = app.filtered.len();
    let total = app.clips.len();
    let title = if count == total {
        format!(" 📋 Clips ({}) ", total)
    } else {
        format!(" 📋 Clips ({}/{}) ", count, total)
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color(c.border))),
        )
        .highlight_style(
            Style::default()
                .bg(color(c.bg_selected))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let c = app.theme.colors();

    if let Some(ref clip) = app.selected_clip {
        let source = clip.source_app.as_deref().unwrap_or("unknown");
        let title = format!(
            " {} {} | {} | id:{} ",
            clip.content_type.icon(),
            clip.content_type.as_str(),
            source,
            clip.id,
        );

        let content_color = match clip.content_type {
            ContentType::Code => color(c.code),
            ContentType::Url => color(c.url),
            ContentType::Email => color(c.email),
            ContentType::Path => color(c.path),
            _ => color(c.text),
        };

        let widget = Paragraph::new(clip.content.clone())
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(color(c.accent2))),
            )
            .style(Style::default().fg(content_color))
            .wrap(Wrap { trim: false });

        f.render_widget(widget, area);
    } else {
        let widget = Paragraph::new("  No clip selected")
            .block(
                Block::default()
                    .title(" Preview ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(color(c.border))),
            )
            .style(Style::default().fg(color(c.overlay)));

        f.render_widget(widget, area);
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let c = app.theme.colors();
    let accent = color(c.accent);
    let sub = color(c.subtext);

    let line = if let Some((ref msg, col)) = app.status_message {
        Line::from(Span::styled(
            format!(" {}", msg),
            Style::default().fg(col),
        ))
    } else {
        Line::from(vec![
            Span::styled(" j/k↑↓ ", Style::default().fg(accent)),
            Span::styled("Nav", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("1-9 ", Style::default().fg(accent)),
            Span::styled("Copy row", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("⏎ ", Style::default().fg(accent)),
            Span::styled("Copy", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("Tab ", Style::default().fg(accent)),
            Span::styled("Type filter", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("S-Tab ", Style::default().fg(accent)),
            Span::styled("App", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("^A ", Style::default().fg(accent)),
            Span::styled("All", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("^T ", Style::default().fg(accent)),
            Span::styled("Transform", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("^G ", Style::default().fg(accent)),
            Span::styled("Semantic", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("^D ", Style::default().fg(accent)),
            Span::styled("Del", Style::default().fg(sub)),
            Span::styled("  ", Style::default()),
            Span::styled("Esc ", Style::default().fg(accent)),
            Span::styled("Quit", Style::default().fg(sub)),
        ])
    };

    f.render_widget(Paragraph::new(line), area);
}

// ── Transform Overlays ──

fn draw_transform_picker(f: &mut Frame, app: &App) {
    let popup = centered_rect(55, 80, f.area());
    f.render_widget(Clear, popup);

    let c = app.theme.colors();
    let transforms = &app.transforms;

    let mut lines: Vec<Line> = Vec::new();
    let mut prev_cat = "";
    let mut selected_line: usize = 0;
    let mut current_line: usize = 0;

    for (i, t) in transforms.iter().enumerate() {
        let cat = t.category();
        if cat != prev_cat {
            if !prev_cat.is_empty() {
                lines.push(Line::from(""));
                current_line += 1;
            }
            lines.push(Line::from(Span::styled(
                format!("  {}", cat),
                Style::default()
                    .fg(color(c.accent))
                    .add_modifier(Modifier::BOLD),
            )));
            current_line += 1;
            prev_cat = cat;
        }

        if i == app.transform_selected {
            selected_line = current_line;
        }

        let is_sel = i == app.transform_selected;
        let marker = if is_sel { "▸" } else { " " };
        let style = if is_sel {
            Style::default()
                .fg(Color::White)
                .bg(color(c.bg_selected))
                .add_modifier(Modifier::BOLD)
        } else if t.is_ai() {
            Style::default().fg(color(c.accent2))
        } else {
            Style::default().fg(color(c.text))
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {} {} ", marker, t.icon()), style),
            Span::styled(t.label().to_string(), style),
        ]));
        current_line += 1;
    }

    if let Some(ref err) = app.transform_error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {}", truncate(err, 50)),
            Style::default().fg(Color::Red),
        )));
    }

    let inner_height = popup.height.saturating_sub(4) as usize;
    let scroll = if selected_line >= inner_height {
        (selected_line - inner_height + 3) as u16
    } else {
        0
    };

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" ✨ Transform ")
                .title_bottom(Line::from(vec![
                    Span::styled(" ↑↓ ", Style::default().fg(color(c.accent))),
                    Span::styled("Navigate", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Enter ", Style::default().fg(color(c.accent))),
                    Span::styled("Apply", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Esc ", Style::default().fg(color(c.accent))),
                    Span::styled("Back ", Style::default().fg(color(c.subtext))),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color(c.accent2)))
                .style(Style::default().bg(color(c.bg_base))),
        )
        .scroll((scroll, 0));

    f.render_widget(widget, popup);
}

fn draw_transform_input(f: &mut Frame, app: &App) {
    let popup = centered_rect(55, 25, f.area());
    f.render_widget(Clear, popup);

    let c = app.theme.colors();
    let cursor = "│";
    let text = format!("\n  {}{}\n", app.custom_prompt_input, cursor);

    let widget = Paragraph::new(text)
        .block(
            Block::default()
                .title(" ✨ Custom Prompt ")
                .title_bottom(Line::from(vec![
                    Span::styled(" Enter ", Style::default().fg(color(c.accent))),
                    Span::styled("Apply", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Esc ", Style::default().fg(color(c.accent))),
                    Span::styled("Back ", Style::default().fg(color(c.subtext))),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color(c.accent2)))
                .style(Style::default().bg(color(c.bg_base))),
        )
        .style(Style::default().fg(color(c.text)));

    f.render_widget(widget, popup);
}

fn draw_transform_result(f: &mut Frame, app: &App) {
    let popup = centered_rect(70, 80, f.area());
    f.render_widget(Clear, popup);

    let c = app.theme.colors();
    let label = app.transforms[app.transform_selected].label();

    let content = app
        .transform_result
        .as_deref()
        .unwrap_or("(no result)");

    let widget = Paragraph::new(content)
        .block(
            Block::default()
                .title(format!(" ✨ {} — Result ", label))
                .title_bottom(Line::from(vec![
                    Span::styled(" Enter ", Style::default().fg(color(c.accent))),
                    Span::styled("Copy & Close", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("↑↓ ", Style::default().fg(color(c.accent))),
                    Span::styled("Scroll", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Esc ", Style::default().fg(color(c.accent))),
                    Span::styled("Back ", Style::default().fg(color(c.subtext))),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color(c.green)))
                .style(Style::default().bg(color(c.bg_base))),
        )
        .style(Style::default().fg(color(c.text)))
        .wrap(Wrap { trim: false })
        .scroll((app.result_scroll, 0));

    f.render_widget(widget, popup);
}

fn draw_sessions(f: &mut Frame, app: &App) {
    let popup = centered_rect(60, 80, f.area());
    f.render_widget(Clear, popup);

    let c = app.theme.colors();
    let mut lines: Vec<Line> = Vec::new();

    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No sessions found",
            Style::default().fg(color(c.subtext)),
        )));
    } else {
        for (i, session) in app.sessions.iter().enumerate() {
            let is_sel = i == app.session_selected;
            let marker = if is_sel { "▸" } else { " " };
            let style = if is_sel {
                Style::default()
                    .fg(Color::White)
                    .bg(color(c.bg_selected))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color(c.text))
            };

            let dur = session.duration_mins();
            let dur_str = if dur < 1 {
                "< 1min".into()
            } else {
                format!("{}min", dur)
            };

            lines.push(Line::from(vec![
                Span::styled(format!("  {} 📂 ", marker), style),
                Span::styled(session.name.clone(), style),
            ]));
            lines.push(Line::from(vec![
                Span::raw("       "),
                Span::styled(
                    format!(
                        "{} clips · {} · {}",
                        session.clip_count(),
                        dur_str,
                        session.top_apps.join(", "),
                    ),
                    Style::default().fg(color(c.subtext)),
                ),
            ]));

            if i + 1 < app.sessions.len() {
                lines.push(Line::from(""));
            }
        }
    }

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!(" 📂 Sessions ({}) ", app.sessions.len()))
                .title_bottom(Line::from(vec![
                    Span::styled(" ↑↓ ", Style::default().fg(color(c.accent))),
                    Span::styled("Navigate", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Enter ", Style::default().fg(color(c.accent))),
                    Span::styled("Filter", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Esc ", Style::default().fg(color(c.accent))),
                    Span::styled("Back ", Style::default().fg(color(c.subtext))),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color(c.accent2)))
                .style(Style::default().bg(color(c.bg_base))),
        )
        .scroll((
            if app.session_selected > 5 {
                ((app.session_selected - 5) * 3) as u16
            } else {
                0
            },
            0,
        ));

    f.render_widget(widget, popup);
}

fn draw_settings(f: &mut Frame, app: &App) {
    let popup = centered_rect(65, 85, f.area());
    f.render_widget(Clear, popup);

    let c = app.theme.colors();
    let rows = settings_rows(&app.privacy_config);
    let mut lines: Vec<Line> = Vec::new();

    for (i, row) in rows.iter().enumerate() {
        let is_sel = i == app.settings_cursor;
        let sel_style = if is_sel {
            Style::default()
                .fg(Color::White)
                .bg(color(c.bg_selected))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color(c.text))
        };

        match row {
            SettingsRowKind::Toggle(kind) => {
                let val = match kind {
                    SettingsRow::Enabled => app.privacy_config.enabled,
                    SettingsRow::DetectApiKeys => app.privacy_config.detect_api_keys,
                    SettingsRow::DetectCredentials => app.privacy_config.detect_credentials,
                    SettingsRow::DetectCreditCards => app.privacy_config.detect_credit_cards,
                    SettingsRow::DetectSsn => app.privacy_config.detect_ssn,
                    _ => false,
                };
                let indicator = if val { "[✓]" } else { "[ ]" };
                let marker = if is_sel { "▸" } else { " " };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {} {} ", marker, indicator), sel_style),
                    Span::styled(kind.label().to_string(), sel_style),
                ]));
            }
            SettingsRowKind::Header(kind) => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  {}", kind.label()),
                    Style::default()
                        .fg(color(c.accent))
                        .add_modifier(Modifier::BOLD),
                )));
            }
            SettingsRowKind::ListItem(_kind, _idx, value) => {
                let marker = if is_sel { "▸" } else { " " };
                let del_hint = if is_sel { "  [x to delete]" } else { "" };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}   • ", marker), sel_style),
                    Span::styled(value.clone(), sel_style),
                    Span::styled(
                        del_hint.to_string(),
                        Style::default().fg(color(c.subtext)),
                    ),
                ]));
            }
            SettingsRowKind::Action(kind) => {
                let marker = if is_sel { "▸" } else { " " };

                if app.settings_editing
                    && is_sel
                    && matches!(
                        kind,
                        SettingsRow::AddExcludedApp | SettingsRow::AddCustomPattern
                    )
                {
                    let cursor = "│";
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {}   ", marker),
                            Style::default().fg(color(c.accent)),
                        ),
                        Span::styled(
                            format!("{}{}", app.settings_input, cursor),
                            Style::default().fg(Color::White).bg(color(c.bg_elevated)),
                        ),
                    ]));
                } else {
                    let style = match kind {
                        SettingsRow::Save => {
                            if is_sel {
                                sel_style
                            } else {
                                Style::default()
                                    .fg(color(c.green))
                                    .add_modifier(Modifier::BOLD)
                            }
                        }
                        SettingsRow::Reset => {
                            if is_sel {
                                sel_style
                            } else {
                                Style::default().fg(Color::Yellow)
                            }
                        }
                        _ => {
                            if is_sel {
                                sel_style
                            } else {
                                Style::default().fg(color(c.accent2))
                            }
                        }
                    };

                    if matches!(kind, SettingsRow::Save | SettingsRow::Reset) {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}   ", marker), style),
                        Span::styled(kind.label().to_string(), style),
                    ]));
                }
            }
        }
    }

    let dirty_label = if app.settings_dirty { " (unsaved) " } else { "" };

    let inner_height = popup.height.saturating_sub(4) as usize;
    let selected_line = lines
        .iter()
        .enumerate()
        .take(app.settings_cursor + 10)
        .count()
        .min(lines.len());
    let scroll = if selected_line >= inner_height {
        (selected_line - inner_height + 3) as u16
    } else {
        0
    };

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!(" 🔒 Privacy Settings{}", dirty_label))
                .title_bottom(Line::from(vec![
                    Span::styled(" ↑↓ ", Style::default().fg(color(c.accent))),
                    Span::styled("Navigate", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Space/⏎ ", Style::default().fg(color(c.accent))),
                    Span::styled("Toggle/Act", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("x ", Style::default().fg(color(c.accent))),
                    Span::styled("Delete", Style::default().fg(color(c.subtext))),
                    Span::styled("  ", Style::default()),
                    Span::styled("Esc ", Style::default().fg(color(c.accent))),
                    Span::styled("Save & Back ", Style::default().fg(color(c.subtext))),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color(c.accent2)))
                .style(Style::default().bg(color(c.bg_base))),
        )
        .scroll((scroll, 0));

    f.render_widget(widget, popup);
}

// ── Helpers ──

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vert[1])[1]
}

fn relative_time(dt: &chrono::DateTime<Utc>) -> String {
    let secs = Utc::now().signed_duration_since(*dt).num_seconds();
    if secs < 60 {
        "now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 604800 {
        format!("{}d ago", secs / 86400)
    } else {
        format!("{}w ago", secs / 604800)
    }
}

fn truncate(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    let count = cleaned.chars().count();
    if count > max {
        let end: String = cleaned.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", end)
    } else {
        cleaned
    }
}
