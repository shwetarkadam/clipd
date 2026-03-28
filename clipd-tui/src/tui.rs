use chrono::Utc;
use clipd_core::{load_theme, save_theme, ClipEntry, ClipStore, ContentType, Rgb, Theme};
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
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;

fn color(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
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
        }
    }

    fn filter_clips(&mut self) {
        if self.search_input.is_empty() {
            self.filtered = (0..self.clips.len()).collect();
        } else {
            self.filtered = self
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
        }

        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
            self.sync_selection();
        } else {
            self.list_state.select(None);
            self.selected_clip = None;
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
                app.status_message = None;

                match key.code {
                    KeyCode::Esc => app.should_quit = true,
                    KeyCode::Enter => {
                        if app.copy_selected() {
                            app.should_quit = true;
                        }
                    }
                    KeyCode::Up => app.move_selection(-1),
                    KeyCode::Down => app.move_selection(1),
                    KeyCode::Tab => app.cycle_theme(),
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
                    KeyCode::Backspace => {
                        app.search_input.pop();
                        app.filter_clips();
                    }
                    KeyCode::Char(c) => {
                        app.search_input.push(c);
                        app.filter_clips();
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

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
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let c = app.theme.colors();
    let cursor = "│";
    let text = format!(" {}{}", app.search_input, cursor);

    let block = Block::default()
        .title(" 🔍 Search clips ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color(c.accent)));

    let widget = Paragraph::new(text)
        .block(block)
        .style(Style::default().fg(color(c.text)));

    f.render_widget(widget, area);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let c = app.theme.colors();
    let inner_w = (area.width as usize).saturating_sub(4);

    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .map(|&idx| {
            let clip = &app.clips[idx];
            let icon = clip.content_type.icon();
            let time = relative_time(&clip.timestamp);

            let time_w = time.chars().count();
            let max_preview = inner_w.saturating_sub(time_w + 4);
            let preview = truncate(&clip.preview, max_preview);
            let preview_w = preview.chars().count();
            let gap = inner_w.saturating_sub(preview_w + time_w + 2);

            let line = Line::from(vec![
                Span::styled(format!("{} ", icon), Style::default()),
                Span::styled(preview, Style::default().fg(color(c.text))),
                Span::raw(" ".repeat(gap)),
                Span::styled(time, Style::default().fg(color(c.accent))),
            ]);

            ListItem::new(line)
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
            Span::styled(" ↑↓ ", Style::default().fg(accent)),
            Span::styled("Navigate", Style::default().fg(sub)),
            Span::styled("   ", Style::default()),
            Span::styled("Enter ", Style::default().fg(accent)),
            Span::styled("Copy", Style::default().fg(sub)),
            Span::styled("   ", Style::default()),
            Span::styled("Ctrl+D ", Style::default().fg(accent)),
            Span::styled("Delete", Style::default().fg(sub)),
            Span::styled("   ", Style::default()),
            Span::styled("Tab ", Style::default().fg(accent)),
            Span::styled("Theme", Style::default().fg(sub)),
            Span::styled("   ", Style::default()),
            Span::styled("Esc ", Style::default().fg(accent)),
            Span::styled("Quit", Style::default().fg(sub)),
        ])
    };

    f.render_widget(Paragraph::new(line), area);
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
