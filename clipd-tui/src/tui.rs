use chrono::Utc;
use clipd_core::{ClipEntry, ClipStore, ContentType};
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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;

/// Application state for the TUI.
struct App {
    store: ClipStore,
    clips: Vec<ClipEntry>,
    filtered: Vec<usize>, // indices into clips
    search_input: String,
    list_state: ListState,
    selected_clip: Option<ClipEntry>,
    should_quit: bool,
    copied_message: Option<String>,
    matcher: SkimMatcherV2,
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

        App {
            store,
            clips,
            filtered,
            search_input: String::new(),
            list_state,
            selected_clip,
            should_quit: false,
            copied_message: None,
            matcher: SkimMatcherV2::default(),
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

        // Reset selection
        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
            self.update_selected();
        } else {
            self.list_state.select(None);
            self.selected_clip = None;
        }
    }

    fn update_selected(&mut self) {
        if let Some(sel_idx) = self.list_state.selected() {
            if let Some(&clip_idx) = self.filtered.get(sel_idx) {
                self.selected_clip = self.clips.get(clip_idx).cloned();
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, len - 1) as usize;
        self.list_state.select(Some(next));
        self.update_selected();
    }

    fn copy_selected_to_clipboard(&mut self) -> Option<String> {
        if let Some(ref clip) = self.selected_clip {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if cb.set_text(&clip.content).is_ok() {
                    return Some(clip.preview.clone());
                }
            }
        }
        None
    }

    fn delete_selected(&mut self) {
        if let Some(ref clip) = self.selected_clip {
            let id = clip.id;
            if self.store.delete(id).unwrap_or(false) {
                // Reload clips
                self.clips = self.store.get_recent(500).unwrap_or_default();
                self.filter_clips();
                self.copied_message = Some("🗑️  Deleted".to_string());
            }
        }
    }
}

/// Run the interactive TUI search interface.
pub fn run_tui() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = ClipStore::default_path();
    let store = ClipStore::new(&db_path)?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(store);

    // Main loop
    loop {
        terminal.draw(|f| draw_ui(f, &mut app))?;

        if app.should_quit {
            break;
        }

        // Handle input
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                app.copied_message = None; // Clear status on any key

                match key.code {
                    KeyCode::Esc => {
                        app.should_quit = true;
                    }
                    KeyCode::Enter => {
                        if let Some(_preview) = app.copy_selected_to_clipboard() {
                            app.should_quit = true;
                        }
                    }
                    KeyCode::Up => {
                        app.move_selection(-1);
                    }
                    KeyCode::Down => {
                        app.move_selection(1);
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.delete_selected();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Clear search
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

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn draw_ui(f: &mut Frame, app: &mut App) {
    let size = f.area();

    // Main layout: search bar, then content area
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Search bar
            Constraint::Min(10),   // Content
            Constraint::Length(2), // Status bar
        ])
        .split(size);

    draw_search_bar(f, app, main_chunks[0]);

    // Content: clip list (left) + preview (right)
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(45),
            Constraint::Percentage(55),
        ])
        .split(main_chunks[1]);

    draw_clip_list(f, app, content_chunks[0]);
    draw_preview(f, app, content_chunks[1]);
    draw_status_bar(f, app, main_chunks[2]);
}

fn draw_search_bar(f: &mut Frame, app: &App, area: Rect) {
    let cursor_char = "│";
    let display = format!(" {}{}", app.search_input, cursor_char);

    let search_bar = Paragraph::new(display)
        .block(
            Block::default()
                .title(" 🔍 Search clips ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(100, 180, 255))),
        )
        .style(Style::default().fg(Color::Rgb(240, 240, 240)));

    f.render_widget(search_bar, area);
}

fn draw_clip_list(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .map(|&idx| {
            let clip = &app.clips[idx];
            let time_str = format_relative_time(&clip.timestamp);
            let icon = clip.content_type.icon();

            let line = Line::from(vec![
                Span::styled(
                    format!("{} ", icon),
                    Style::default(),
                ),
                Span::styled(
                    truncate_str(&clip.preview, (area.width as usize).saturating_sub(16)),
                    Style::default().fg(Color::Rgb(220, 220, 220)),
                ),
                Span::styled(
                    format!(" {}", time_str),
                    Style::default().fg(Color::Rgb(100, 180, 255)),
                ),
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
                .border_style(Style::default().fg(Color::Rgb(80, 140, 220))),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(50, 80, 140))
                .fg(Color::Rgb(255, 255, 255))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let (title, content, style) = if let Some(ref clip) = app.selected_clip {
        let type_str = format!(
            "{} {} | {} | id:{}",
            clip.content_type.icon(),
            clip.content_type.as_str(),
            clip.source_app.as_deref().unwrap_or("unknown"),
            clip.id
        );

        let content_color = match clip.content_type {
            ContentType::Code => Color::Rgb(140, 220, 140),
            ContentType::Url => Color::Rgb(100, 200, 255),
            ContentType::Email => Color::Rgb(255, 220, 100),
            ContentType::Path => Color::Rgb(200, 150, 255),
            _ => Color::Rgb(230, 230, 230),
        };

        (
            format!(" {} ", type_str),
            clip.content.clone(),
            Style::default().fg(content_color),
        )
    } else {
        (
            " Preview ".to_string(),
            "No clip selected".to_string(),
            Style::default().fg(Color::Rgb(100, 100, 100)),
        )
    };

    let preview = Paragraph::new(content)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(160, 100, 220))),
        )
        .style(style)
        .wrap(Wrap { trim: false });

    f.render_widget(preview, area);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let status = if let Some(ref msg) = app.copied_message {
        Line::from(Span::styled(msg.clone(), Style::default().fg(Color::Rgb(100, 220, 100))))
    } else {
        Line::from(vec![
            Span::styled(" ↑↓ ", Style::default().fg(Color::Rgb(100, 180, 255))),
            Span::styled("Navigate", Style::default().fg(Color::Rgb(170, 170, 170))),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled("Enter ", Style::default().fg(Color::Rgb(100, 180, 255))),
            Span::styled("Copy", Style::default().fg(Color::Rgb(170, 170, 170))),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled("Ctrl+D ", Style::default().fg(Color::Rgb(100, 180, 255))),
            Span::styled("Delete", Style::default().fg(Color::Rgb(170, 170, 170))),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled("Ctrl+U ", Style::default().fg(Color::Rgb(100, 180, 255))),
            Span::styled("Clear", Style::default().fg(Color::Rgb(170, 170, 170))),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled("Esc ", Style::default().fg(Color::Rgb(100, 180, 255))),
            Span::styled("Quit", Style::default().fg(Color::Rgb(170, 170, 170))),
        ])
    };

    let status_bar = Paragraph::new(status);
    f.render_widget(status_bar, area);
}

fn format_relative_time(dt: &chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = now.signed_duration_since(*dt);

    if diff.num_seconds() < 60 {
        "just now".to_string()
    } else if diff.num_minutes() < 60 {
        format!("{}m ago", diff.num_minutes())
    } else if diff.num_hours() < 24 {
        format!("{}h ago", diff.num_hours())
    } else if diff.num_days() < 7 {
        format!("{}d ago", diff.num_days())
    } else {
        format!("{}w ago", diff.num_weeks())
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ").replace('\t', " ");
    let char_count: usize = cleaned.chars().count();
    if char_count > max {
        let end: String = cleaned.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", end)
    } else {
        cleaned
    }
}
