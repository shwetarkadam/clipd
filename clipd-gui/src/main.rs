use arboard::Clipboard;
use chrono::{DateTime, Utc};
use clipd_core::{
    compute_sessions, detect_sensitive, generate_embedding, is_embedding_available,
    load_paste_transform_settings, load_privacy_config, load_theme, load_transform_config,
    paste_transforms, save_paste_transform_settings, save_privacy_config, save_theme,
    search_embeddings, ClipEntry, ClipStore, ContentType, Embedding, PasteTransformSettings,
    PrivacyConfig, Rgb, Session, SessionConfig, TfIdfIndex, Theme, TransformConfig, TransformKind,
    MAX_CLIP_SLOT,
};
use eframe::egui::{self, Color32, FontId, Margin, RichText, Rounding, Stroke};
use std::time::{Duration, Instant};

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

fn pill_bg(col: Color32) -> Color32 {
    Color32::from_rgb(
        (col.r() as u16 / 3 + 15).min(255) as u8,
        (col.g() as u16 / 3 + 15).min(255) as u8,
        (col.b() as u16 / 3 + 15).min(255) as u8,
    )
}

enum Action {
    None,
    Copy,
    Delete,
    ToggleTransform,
}

// ── Entry point ──

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    // Spawn daemon as a child process (rdev's keyboard hook conflicts with
    // eframe's event loop if both run in the same process on macOS).
    let daemon_child = spawn_daemon_process();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        "clipd",
        options,
        Box::new(|cc| {
            let theme = load_theme();
            apply_theme(&cc.egui_ctx, theme);
            Ok(Box::new(ClipdGui::new(theme)))
        }),
    );

    // GUI closed — kill the daemon subprocess
    if let Some(mut child) = daemon_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    clipd_core::release_daemon_lock();

    result
}

fn spawn_daemon_process() -> Option<std::process::Child> {
    if clipd_core::is_daemon_running() {
        log::info!("Daemon already running — skipping spawn");
        return None;
    }

    // Find the `clipd` CLI binary next to this executable
    let cli_bin = find_cli_binary()?;

    log::info!("Spawning daemon process: {} daemon", cli_bin.display());
    std::process::Command::new(&cli_bin)
        .arg("daemon")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| log::warn!("Failed to spawn daemon: {}", e))
        .ok()
}

fn find_cli_binary() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(target_os = "windows")]
            for name in ["clipd.exe", "clipd"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let candidate = dir.join("clipd");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("where").arg("clipd").output() {
            if output.status.success() {
                let line = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if !line.is_empty() {
                    return Some(PathBuf::from(line));
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(output) = std::process::Command::new("which").arg("clipd").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    None
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    let c = theme.colors();

    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Heading, FontId::proportional(20.0));
    style.text_styles.insert(egui::TextStyle::Small, FontId::proportional(11.0));
    style.text_styles.insert(egui::TextStyle::Button, FontId::proportional(13.0));
    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.visuals.window_rounding = Rounding::same(12.0);
    style.visuals.menu_rounding = Rounding::same(8.0);
    ctx.set_style(style);

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(rgb(c.text));
    v.panel_fill = rgb(c.bg_base);
    v.window_fill = rgb(c.bg_base);
    v.window_stroke = Stroke::new(1.0, rgb(c.border));
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(60),
    };
    v.window_rounding = Rounding::same(12.0);
    v.extreme_bg_color = rgb(c.bg_base);
    v.faint_bg_color = rgb(c.bg_surface);
    v.widgets.noninteractive.bg_fill = rgb(c.bg_surface);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, rgb(c.text));
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    v.widgets.noninteractive.rounding = Rounding::same(8.0);
    v.widgets.inactive.bg_fill = rgb(c.bg_elevated);
    v.widgets.inactive.rounding = Rounding::same(8.0);
    v.widgets.hovered.bg_fill = rgb(c.bg_hover);
    v.widgets.hovered.rounding = Rounding::same(8.0);
    v.widgets.active.bg_fill = rgb(c.bg_selected);
    v.widgets.active.rounding = Rounding::same(8.0);
    v.selection.bg_fill =
        Color32::from_rgba_premultiplied(c.accent.0, c.accent.1, c.accent.2, 50);
    v.selection.stroke = Stroke::new(1.0, rgb(c.accent));
    ctx.set_visuals(v);
}

// ── App state ──

struct ClipdGui {
    store: ClipStore,
    clips: Vec<ClipEntry>,
    search_query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll_to_selected: bool,
    copied_at: Option<Instant>,
    last_refresh: Instant,
    focus_search: bool,
    theme: Theme,

    show_transforms: bool,
    transforms: Vec<TransformKind>,
    paste_settings: PasteTransformSettings,

    semantic_mode: bool,
    transform_config: TransformConfig,
    cached_embeddings: Vec<(i64, Embedding)>,
    privacy_config: PrivacyConfig,
    sessions: Vec<Session>,
    session_config: SessionConfig,
    show_sessions: bool,

    show_settings: bool,
    new_excluded_app: String,
    new_custom_pattern: String,
    confirm_clear_all: bool,
    export_status: Option<(String, Instant)>,
}

impl ClipdGui {
    fn new(theme: Theme) -> Self {
        let db_path = ClipStore::default_path();
        let store = ClipStore::new(&db_path).expect("Failed to open clip database");
        let clips = store.get_recent(500).unwrap_or_default();
        let count = clips.len();
        let session_config = SessionConfig::default();
        let sessions = compute_sessions(&clips, session_config.window_minutes);
        Self {
            store,
            clips,
            search_query: String::new(),
            filtered: (0..count).collect(),
            selected: 0,
            scroll_to_selected: false,
            copied_at: None,
            last_refresh: Instant::now(),
            focus_search: true,
            theme,
            show_transforms: false,
            transforms: paste_transforms(),
            paste_settings: load_paste_transform_settings(),
            semantic_mode: false,
            transform_config: load_transform_config(),
            cached_embeddings: Vec::new(),
            privacy_config: load_privacy_config(),
            sessions,
            session_config,
            show_sessions: false,
            show_settings: false,
            new_excluded_app: String::new(),
            new_custom_pattern: String::new(),
            confirm_clear_all: false,
            export_status: None,
        }
    }

    fn refresh(&mut self) {
        self.clips = self.store.get_recent(500).unwrap_or_default();
        self.sessions = compute_sessions(&self.clips, self.session_config.window_minutes);
        if self.semantic_mode {
            self.cached_embeddings = self.store.get_all_embeddings().unwrap_or_default();
        }
        self.apply_filter();
        self.last_refresh = Instant::now();
    }

    fn apply_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered = (0..self.clips.len()).collect();
        } else if self.semantic_mode {
            // Try vector embeddings first, fall back to TF-IDF
            let mut used_vectors = false;

            if !self.cached_embeddings.is_empty()
                && is_embedding_available(&self.transform_config)
                && self.search_query.len() >= 3
            {
                if let Ok(query_emb) =
                    generate_embedding(&self.search_query, &self.transform_config)
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
                        self.filtered = results
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
                let results = index.search(&self.search_query, 50);
                self.filtered = results.iter().map(|r| r.clip_index).collect();
            }
        } else {
            let q = self.search_query.to_lowercase();
            let mut results: Vec<usize> = self
                .clips
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.content.to_lowercase().contains(&q)
                        || c.preview.to_lowercase().contains(&q)
                        || c.source_app
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&q)
                })
                .map(|(i, _)| i)
                .collect();

            if results.is_empty() && self.search_query.len() >= 3 {
                let docs: Vec<&str> =
                    self.clips.iter().map(|c| c.content.as_str()).collect();
                let index = TfIdfIndex::build(&docs);
                let sem = index.search(&self.search_query, 50);
                results = sem.iter().map(|r| r.clip_index).collect();
            }
            self.filtered = results;
        }
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    fn selected_clip(&self) -> Option<&ClipEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.clips.get(i))
    }

    fn do_copy(&mut self) {
        if let Some(clip) = self.selected_clip() {
            if let Ok(mut cb) = Clipboard::new() {
                if cb.set_text(&clip.content).is_ok() {
                    self.copied_at = Some(Instant::now());
                }
            }
        }
    }

    fn do_delete(&mut self) {
        if let Some(&idx) = self.filtered.get(self.selected) {
            let id = self.clips[idx].id;
            if self.store.delete(id).unwrap_or(false) {
                self.refresh();
            }
        }
    }

    fn cycle_theme(&mut self, ctx: &egui::Context) {
        self.theme = self.theme.next();
        save_theme(self.theme);
        apply_theme(ctx, self.theme);
    }
}

// ── Rendering ──

impl eframe::App for ClipdGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_refresh.elapsed() > Duration::from_secs(3) {
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_secs(3));

        let c = self.theme.colors();
        let mut action = Action::None;

        let search_has_focus = ctx.memory(|m| {
            m.focused()
                .map_or(false, |id| id == egui::Id::new("clip_search"))
        });

        let mut should_cycle_theme = false;
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                    self.scroll_to_selected = true;
                }
            }
            if i.key_pressed(egui::Key::ArrowUp) && self.selected > 0 {
                self.selected -= 1;
                self.scroll_to_selected = true;
            }
            if i.key_pressed(egui::Key::Enter) && !search_has_focus {
                action = Action::Copy;
            }
            if i.key_pressed(egui::Key::Delete)
                || (i.key_pressed(egui::Key::D) && i.modifiers.command)
            {
                action = Action::Delete;
            }
            if i.key_pressed(egui::Key::T) && i.modifiers.command {
                should_cycle_theme = true;
            }
        });
        if should_cycle_theme {
            self.cycle_theme(ctx);
        }

        // ── Top bar ──
        egui::TopBottomPanel::top("top_bar")
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_surface))
                    .inner_margin(Margin::symmetric(16.0, 10.0))
                    .stroke(Stroke::new(1.0, rgb(c.border))),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("📋").size(24.0));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("clipd")
                            .size(20.0)
                            .strong()
                            .color(rgb(c.accent)),
                    );
                    ui.add_space(12.0);

                    let avail = ui.available_width();
                    egui::Frame::none()
                        .fill(rgb(c.bg_elevated))
                        .rounding(Rounding::same(10.0))
                        .inner_margin(Margin::symmetric(10.0, 0.0))
                        .stroke(Stroke::new(1.0, rgb(c.border)))
                        .show(ui, |ui| {
                            ui.set_width((avail - 140.0).max(200.0));
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    RichText::new("🔍").size(14.0).color(rgb(c.overlay)),
                                );
                                let search = ui.add_sized(
                                    [ui.available_width(), 26.0],
                                    egui::TextEdit::singleline(&mut self.search_query)
                                        .id(egui::Id::new("clip_search"))
                                        .hint_text("Search clips…")
                                        .frame(false)
                                        .font(egui::TextStyle::Body),
                                );
                                if self.focus_search {
                                    search.request_focus();
                                    self.focus_search = false;
                                }
                                if search.changed() {
                                    self.apply_filter();
                                }
                            });
                        });

                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let btn = ui.add(
                                egui::Button::new(
                                    RichText::new(format!("● {}", self.theme.label()))
                                        .size(11.0)
                                        .color(rgb(c.accent)),
                                )
                                .fill(rgb(c.bg_elevated))
                                .rounding(Rounding::same(12.0))
                                .stroke(Stroke::new(1.0, rgb(c.border))),
                            );
                            if btn.clicked() {
                                self.cycle_theme(ui.ctx());
                            }
                            btn.on_hover_text("Click or ⌘T to switch theme");
                        },
                    );
                });

                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.add_space(4.0);

                    let pill = |ui: &mut egui::Ui,
                                label: &str,
                                active: bool,
                                active_col: Color32,
                                tooltip: &str|
                     -> bool {
                        let (fill, text_col, stroke_col) = if active {
                            (
                                pill_bg(active_col),
                                Color32::WHITE,
                                active_col,
                            )
                        } else {
                            (rgb(c.bg_elevated), rgb(c.text), rgb(c.border))
                        };
                        ui.add(
                            egui::Button::new(
                                RichText::new(label).size(11.5).color(text_col).strong(),
                            )
                            .fill(fill)
                            .rounding(Rounding::same(14.0))
                            .stroke(Stroke::new(1.0, stroke_col)),
                        )
                        .on_hover_text(tooltip)
                        .clicked()
                    };

                    let sem_label = if self.semantic_mode {
                        "🧠 Semantic"
                    } else {
                        "🔍 Text"
                    };
                    if pill(
                        ui,
                        sem_label,
                        self.semantic_mode,
                        Color32::from_rgb(180, 140, 255),
                        "Toggle semantic (meaning-based) search",
                    ) {
                        self.semantic_mode = !self.semantic_mode;
                        if self.semantic_mode && self.cached_embeddings.is_empty() {
                            self.cached_embeddings =
                                self.store.get_all_embeddings().unwrap_or_default();
                        }
                        self.apply_filter();
                    }

                    ui.add_space(4.0);
                    if pill(
                        ui,
                        "📂 Sessions",
                        self.show_sessions,
                        Color32::from_rgb(100, 200, 160),
                        "Browse clipboard sessions grouped by time",
                    ) {
                        self.sessions =
                            compute_sessions(&self.clips, self.session_config.window_minutes);
                        self.show_sessions = !self.show_sessions;
                    }

                    ui.add_space(4.0);
                    if pill(
                        ui,
                        "🔒 Privacy",
                        self.show_settings,
                        Color32::from_rgb(255, 130, 130),
                        "Configure sensitive content detection",
                    ) {
                        self.show_settings = !self.show_settings;
                    }

                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            egui::Frame::none()
                                .fill(rgb(c.bg_elevated))
                                .rounding(Rounding::same(10.0))
                                .inner_margin(Margin::symmetric(8.0, 3.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} clips",
                                            self.filtered.len()
                                        ))
                                        .size(11.0)
                                        .color(rgb(c.subtext)),
                                    );
                                });
                        },
                    );
                });
            });

        // ── Bottom bar ──
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_surface))
                    .inner_margin(Margin::symmetric(16.0, 8.0))
                    .stroke(Stroke::new(1.0, rgb(c.border))),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let kbd =
                        |ui: &mut egui::Ui, key: &str, desc: &str, ac: &clipd_core::ThemeColors| {
                            egui::Frame::none()
                                .fill(rgb(ac.bg_elevated))
                                .rounding(Rounding::same(4.0))
                                .inner_margin(Margin::symmetric(5.0, 2.0))
                                .stroke(Stroke::new(1.0, rgb(ac.border)))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(key)
                                            .size(10.5)
                                            .color(rgb(ac.accent))
                                            .strong()
                                            .family(egui::FontFamily::Monospace),
                                    );
                                });
                            ui.label(
                                RichText::new(desc).size(10.5).color(rgb(ac.subtext)),
                            );
                            ui.add_space(8.0);
                        };

                    kbd(ui, "↑↓", "Navigate", &c);
                    kbd(ui, "⏎", "Copy", &c);
                    kbd(ui, "⌘D", "Delete", &c);
                    kbd(ui, "⌘T", "Theme", &c);
                    kbd(ui, "Esc", "Close", &c);

                    if let Some(t) = self.copied_at {
                        if t.elapsed() < Duration::from_secs(2) {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    egui::Frame::none()
                                        .fill(pill_bg(rgb(c.green)))
                                        .rounding(Rounding::same(6.0))
                                        .inner_margin(Margin::symmetric(10.0, 4.0))
                                        .stroke(Stroke::new(1.0, rgb(c.green)))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new("✓ Copied!")
                                                    .size(12.0)
                                                    .color(Color32::WHITE)
                                                    .strong(),
                                            );
                                        });
                                },
                            );
                        } else {
                            self.copied_at = None;
                        }
                    }
                });
            });

        // ── Left panel: clip list ──
        egui::SidePanel::left("clip_list")
            .default_width(320.0)
            .width_range(260.0..=500.0)
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::symmetric(8.0, 8.0)),
            )
            .show(ctx, |ui| {
                if self.filtered.is_empty() {
                    self.render_empty_list(ui, &c);
                } else {
                    self.render_clip_list(ui, &mut action, &c);
                }
            });

        // ── Center panel: preview ──
        let preview_data = self.selected_clip().cloned();

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::symmetric(20.0, 14.0)),
            )
            .show(ctx, |ui| {
                if let Some(clip) = &preview_data {
                    render_preview(ui, clip, &mut action, &c);
                } else {
                    render_empty_preview(ui, &c);
                }
            });

        // ── Transform window ──
        if self.show_transforms {
            self.render_transform_window(ctx, &c);
        }

        // ── Sessions window ──
        if self.show_sessions {
            self.render_sessions_window(ctx, &c);
        }

        // ── Settings window ──
        if self.show_settings {
            self.render_settings_window(ctx, &c);
        }

        match action {
            Action::Copy => self.do_copy(),
            Action::Delete => self.do_delete(),
            Action::ToggleTransform => {
                self.show_transforms = !self.show_transforms;
            }
            Action::None => {}
        }
    }
}

impl ClipdGui {
    fn export_path(ext: &str) -> std::path::PathBuf {
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("clipd_history_{}.{}", ts, ext);
        dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(filename)
    }

    fn do_export_text(&self) -> Result<String, String> {
        let path = Self::export_path("txt");
        let mut out = String::new();
        for (i, clip) in self.clips.iter().enumerate() {
            out.push_str(&format!(
                "=== Clip {} | {} | {} ===\n",
                i + 1,
                clip.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                clip.source_app.as_deref().unwrap_or("Unknown"),
            ));
            out.push_str(&clip.content);
            out.push_str("\n\n");
        }
        std::fs::write(&path, out).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    fn do_export_csv(&self) -> Result<String, String> {
        let path = Self::export_path("csv");
        let mut out = String::from("slot,timestamp,source_app,content_type,content\n");
        for (i, clip) in self.clips.iter().enumerate() {
            let escaped = clip.content.replace('"', "\"\"");
            out.push_str(&format!(
                "{},{},{},{},\"{}\"\n",
                i + 1,
                clip.timestamp.format("%Y-%m-%d %H:%M:%S"),
                clip.source_app.as_deref().unwrap_or(""),
                clip.content_type.as_str(),
                escaped,
            ));
        }
        std::fs::write(&path, out).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    fn render_empty_list(&self, ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.label(RichText::new("📭").size(48.0));
            ui.add_space(12.0);
            if self.search_query.is_empty() {
                ui.label(RichText::new("No clips yet").size(16.0).color(rgb(c.overlay)));
                ui.label(
                    RichText::new("Copy something to get started!")
                        .size(13.0)
                        .color(rgb(c.overlay)),
                );
            } else {
                ui.label(RichText::new("No matching clips").size(16.0).color(rgb(c.overlay)));
            }
        });
    }

    fn render_clip_list(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Action,
        c: &clipd_core::ThemeColors,
    ) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (display_idx, &clip_idx) in self.filtered.clone().iter().enumerate() {
                    let clip = &self.clips[clip_idx];
                    let is_selected = display_idx == self.selected;

                    let clip_id = egui::Id::new(("clip", display_idx));
                    let is_hovered: bool =
                        ui.ctx().data(|d| d.get_temp::<bool>(clip_id).unwrap_or(false));

                    let accent_bar_color = if is_selected {
                        rgb(c.accent)
                    } else {
                        Color32::TRANSPARENT
                    };

                    let bg = if is_selected {
                        rgb(c.bg_selected)
                    } else if is_hovered {
                        rgb(c.bg_hover)
                    } else {
                        Color32::TRANSPARENT
                    };

                    let text_color = Color32::WHITE;
                    let meta_color = rgb(c.subtext);

                    let frame_resp = egui::Frame::none()
                        .fill(bg)
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::symmetric(0.0, 0.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());

                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;

                                let (bar_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(3.0, 44.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    bar_rect,
                                    Rounding {
                                        nw: 8.0,
                                        sw: 8.0,
                                        ne: 0.0,
                                        se: 0.0,
                                    },
                                    accent_bar_color,
                                );

                                ui.add_space(10.0);

                                ui.vertical(|ui| {
                                    ui.add_space(6.0);

                                    let preview =
                                        clip.preview.trim().replace('\n', " ");
                                    let truncated: String =
                                        preview.chars().take(50).collect();
                                    let suffix = if preview.chars().count() > 50 {
                                        "…"
                                    } else {
                                        ""
                                    };

                                    ui.label(
                                        RichText::new(format!(
                                            "{}{}",
                                            truncated, suffix
                                        ))
                                        .size(13.0)
                                        .color(text_color),
                                    );

                                    ui.add_space(2.0);

                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;

                                        let type_color = match clip.content_type {
                                            ContentType::Code => rgb(c.code),
                                            ContentType::Url => rgb(c.url),
                                            ContentType::Email => rgb(c.email),
                                            ContentType::Path => rgb(c.path),
                                            _ => rgb(c.overlay),
                                        };

                                        // Slot badge for the most recent clips (up to MAX_CLIP_SLOT)
                                        if clip_idx < MAX_CLIP_SLOT as usize {
                                            let slot = clip_idx + 1;
                                            egui::Frame::none()
                                                .fill(rgb(c.accent))
                                                .rounding(Rounding::same(4.0))
                                                .inner_margin(Margin::symmetric(5.0, 1.0))
                                                .show(ui, |ui| {
                                                    ui.label(
                                                        RichText::new(format!("S{}", slot))
                                                            .size(10.0)
                                                            .color(rgb(c.bg_base))
                                                            .strong(),
                                                    );
                                                });
                                        }

                                        egui::Frame::none()
                                            .fill(pill_bg(type_color))
                                            .rounding(Rounding::same(4.0))
                                            .inner_margin(Margin::symmetric(
                                                5.0, 1.0,
                                            ))
                                            .stroke(Stroke::new(0.5, type_color))
                                            .show(ui, |ui| {
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} {}",
                                                        clip.content_type.icon(),
                                                        clip.content_type.as_str()
                                                    ))
                                                    .size(10.0)
                                                    .color(Color32::WHITE),
                                                );
                                            });

                                        if let Some(ref app) = clip.source_app {
                                            ui.label(
                                                RichText::new(app)
                                                    .size(10.5)
                                                    .color(meta_color),
                                            );
                                        }

                                        ui.label(
                                            RichText::new(
                                                relative_time(&clip.timestamp),
                                            )
                                            .size(10.5)
                                            .color(rgb(c.subtext)),
                                        );

                                        let sensitive = !detect_sensitive(
                                            &clip.content,
                                            &self.privacy_config,
                                        )
                                        .is_empty();
                                        if sensitive {
                                            ui.label(
                                                RichText::new("🔒").size(10.0),
                                            );
                                        }
                                    });

                                    ui.add_space(4.0);
                                });
                            });
                        })
                        .response;

                    let resp = ui.interact(
                        frame_resp.rect,
                        clip_id,
                        egui::Sense::click(),
                    );

                    ui.ctx()
                        .data_mut(|d| d.insert_temp(clip_id, resp.hovered()));

                    if resp.clicked() {
                        self.selected = display_idx;
                    }
                    if resp.double_clicked() {
                        self.selected = display_idx;
                        *action = Action::Copy;
                    }

                    if is_selected && self.scroll_to_selected {
                        resp.scroll_to_me(Some(egui::Align::Center));
                    }

                    ui.add_space(1.0);
                }
                self.scroll_to_selected = false;
            });
    }
}

impl ClipdGui {
    fn render_sessions_window(&mut self, ctx: &egui::Context, c: &clipd_core::ThemeColors) {
        let mut open = true;
        egui::Window::new("📂 Sessions")
            .id(egui::Id::new("sessions_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([440.0, 500.0])
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::new(1.0, rgb(c.border)))
                    .rounding(Rounding::same(12.0)),
            )
            .show(ctx, |ui| {
                if self.sessions.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label(RichText::new("📭").size(40.0));
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("No sessions found")
                                .size(14.0)
                                .color(rgb(c.subtext)),
                        );
                    });
                    return;
                }

                ui.label(
                    RichText::new(format!("{} sessions", self.sessions.len()))
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(6.0);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut filter_session: Option<usize> = None;
                    let session_color = Color32::from_rgb(100, 200, 160);

                    for (i, session) in self.sessions.iter().enumerate() {
                        let dur = session.duration_mins();
                        let dur_str = if dur < 1 {
                            "instant".into()
                        } else if dur < 60 {
                            format!("{} min", dur)
                        } else {
                            let h = dur / 60;
                            let m = dur % 60;
                            if m == 0 { format!("{}h", h) } else { format!("{}h {}m", h, m) }
                        };

                        egui::Frame::none()
                            .fill(rgb(c.bg_surface))
                            .rounding(Rounding::same(10.0))
                            .inner_margin(Margin::symmetric(12.0, 10.0))
                            .stroke(Stroke::new(1.0, session_color))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("📂")
                                            .size(14.0),
                                    );
                                    ui.label(
                                        RichText::new(&session.name)
                                            .size(13.0)
                                            .strong()
                                            .color(rgb(c.text)),
                                    );
                                });
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;

                                    let meta_pill = |ui: &mut egui::Ui, text: &str| {
                                        egui::Frame::none()
                                            .fill(rgb(c.bg_elevated))
                                            .rounding(Rounding::same(4.0))
                                            .inner_margin(Margin::symmetric(5.0, 1.0))
                                            .stroke(Stroke::new(0.5, rgb(c.border)))
                                            .show(ui, |ui| {
                                                ui.label(
                                                    RichText::new(text)
                                                        .size(10.5)
                                                        .color(rgb(c.text)),
                                                );
                                            });
                                    };

                                    let n = session.clip_count();
                                    meta_pill(
                                        ui,
                                        &format!("{} {}", n, if n == 1 { "clip" } else { "clips" }),
                                    );
                                    meta_pill(ui, &dur_str);
                                    if !session.top_apps.is_empty() {
                                        meta_pill(
                                            ui,
                                            &session.top_apps.join(", "),
                                        );
                                    }

                                    ui.with_layout(
                                        egui::Layout::right_to_left(
                                            egui::Align::Center,
                                        ),
                                        |ui| {
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new("View >>")
                                                            .size(11.0)
                                                            .strong()
                                                            .color(session_color),
                                                    )
                                                    .fill(pill_bg(session_color))
                                                    .stroke(Stroke::new(1.0, session_color))
                                                    .rounding(Rounding::same(6.0)),
                                                )
                                                .clicked()
                                            {
                                                filter_session = Some(i);
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(4.0);
                    }

                    if let Some(idx) = filter_session {
                        let session_ids: std::collections::HashSet<i64> =
                            self.sessions[idx].clip_ids.iter().copied().collect();
                        self.search_query.clear();
                        self.filtered = self
                            .clips
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| session_ids.contains(&c.id))
                            .map(|(i, _)| i)
                            .collect();
                        self.selected = 0;
                        self.scroll_to_selected = true;
                        self.show_sessions = false;
                    }
                });
            });

        if !open {
            self.show_sessions = false;
        }
    }

    fn render_settings_window(&mut self, ctx: &egui::Context, c: &clipd_core::ThemeColors) {
        let mut open = true;
        egui::Window::new("🔒 Privacy Settings")
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([440.0, 560.0])
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::new(1.0, rgb(c.border)))
                    .rounding(Rounding::same(12.0)),
            )
            .show(ctx, |ui| {
                let mut dirty = false;

                // ── Master toggle ──
                ui.add_space(4.0);
                if ui
                    .checkbox(&mut self.privacy_config.enabled, "Enable Privacy Protection")
                    .changed()
                {
                    dirty = true;
                }

                ui.add_space(8.0);
                ui.separator();

                // ── Detection toggles ──
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Detection Rules")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.accent)),
                );
                ui.add_space(4.0);

                ui.add_enabled_ui(self.privacy_config.enabled, |ui| {
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_api_keys,
                            "API Keys (OpenAI, AWS, GitHub, Stripe, Slack…)",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_credentials,
                            "Passwords, Secrets & Database URLs",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_credit_cards,
                            "Credit Card Numbers (Luhn validated)",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.privacy_config.detect_ssn,
                            "Social Security Numbers (SSN)",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                });

                ui.add_space(8.0);
                ui.separator();

                // ── Excluded apps ──
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Excluded Apps")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.accent)),
                );
                ui.label(
                    RichText::new("Copies from these apps are never saved to history")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(4.0);

                let mut remove_app: Option<usize> = None;
                for (i, app_name) in self.privacy_config.excluded_apps.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("  • {}", app_name))
                                .size(12.0)
                                .color(rgb(c.text)),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("✕").size(11.0).color(Color32::WHITE),
                                        )
                                        .fill(Color32::from_rgb(180, 60, 60))
                                        .rounding(Rounding::same(4.0)),
                                    )
                                    .clicked()
                                {
                                    remove_app = Some(i);
                                }
                            },
                        );
                    });
                }
                if let Some(idx) = remove_app {
                    self.privacy_config.excluded_apps.remove(idx);
                    dirty = true;
                }

                ui.horizontal(|ui| {
                    let resp = ui.add_sized(
                        [ui.available_width() - 60.0, 24.0],
                        egui::TextEdit::singleline(&mut self.new_excluded_app)
                            .hint_text("App name…")
                            .font(egui::TextStyle::Small),
                    );
                    if ui
                        .add(
                            egui::Button::new(RichText::new("+ Add").size(11.0))
                                .rounding(Rounding::same(4.0)),
                        )
                        .clicked()
                        || (resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        let name = self.new_excluded_app.trim().to_string();
                        if !name.is_empty() {
                            self.privacy_config.excluded_apps.push(name);
                            self.new_excluded_app.clear();
                            dirty = true;
                        }
                    }
                });

                ui.add_space(8.0);
                ui.separator();

                // ── Custom skip patterns ──
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Custom Skip Patterns")
                        .size(13.0)
                        .strong()
                        .color(rgb(c.accent)),
                );
                ui.label(
                    RichText::new("Clips containing these strings are never saved")
                        .size(11.0)
                        .color(rgb(c.subtext)),
                );
                ui.add_space(4.0);

                let mut remove_pat: Option<usize> = None;
                for (i, pattern) in self.privacy_config.custom_skip_patterns.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("  • {}", pattern))
                                .size(12.0)
                                .color(rgb(c.text)),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("✕").size(11.0).color(Color32::WHITE),
                                        )
                                        .fill(Color32::from_rgb(180, 60, 60))
                                        .rounding(Rounding::same(4.0)),
                                    )
                                    .clicked()
                                {
                                    remove_pat = Some(i);
                                }
                            },
                        );
                    });
                }
                if let Some(idx) = remove_pat {
                    self.privacy_config.custom_skip_patterns.remove(idx);
                    dirty = true;
                }

                ui.horizontal(|ui| {
                    let resp = ui.add_sized(
                        [ui.available_width() - 60.0, 24.0],
                        egui::TextEdit::singleline(&mut self.new_custom_pattern)
                            .hint_text("Pattern…")
                            .font(egui::TextStyle::Small),
                    );
                    if ui
                        .add(
                            egui::Button::new(RichText::new("+ Add").size(11.0))
                                .rounding(Rounding::same(4.0)),
                        )
                        .clicked()
                        || (resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        let pat = self.new_custom_pattern.trim().to_string();
                        if !pat.is_empty() {
                            self.privacy_config.custom_skip_patterns.push(pat);
                            self.new_custom_pattern.clear();
                            dirty = true;
                        }
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                // ── Action buttons ──
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("💾 Save Settings")
                                    .size(13.0)
                                    .color(Color32::WHITE),
                            )
                            .fill(rgb(c.green))
                            .rounding(Rounding::same(6.0)),
                        )
                        .clicked()
                    {
                        save_privacy_config(&self.privacy_config);
                    }

                    ui.add_space(8.0);

                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("↺ Reset to Defaults").size(12.0).color(rgb(c.text)),
                            )
                            .fill(rgb(c.bg_elevated))
                            .rounding(Rounding::same(6.0)),
                        )
                        .clicked()
                    {
                        self.privacy_config = PrivacyConfig::default();
                        dirty = true;
                    }
                });

                if dirty {
                    save_privacy_config(&self.privacy_config);
                }

                ui.add_space(4.0);
            });

        if !open {
            self.show_settings = false;
        }
    }

    fn render_transform_window(&mut self, ctx: &egui::Context, c: &clipd_core::ThemeColors) {
        let mut open = true;
        egui::Window::new("✨ Transform on Paste")
            .id(egui::Id::new("transform_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([500.0, 600.0])
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::same(0.0))
                    .stroke(Stroke::new(1.0, rgb(c.border)))
                    .rounding(Rounding::same(12.0)),
            )
            .show(ctx, |ui| {
                let accent = Color32::from_rgb(255, 160, 50);

                // Onboarding hero (shown until dismissed)
                if !self.paste_settings.onboarding_seen {
                    egui::Frame::none()
                        .fill(pill_bg(accent))
                        .inner_margin(Margin::symmetric(20.0, 16.0))
                        .rounding(Rounding {
                            nw: 12.0,
                            ne: 12.0,
                            sw: 0.0,
                            se: 0.0,
                        })
                        .stroke(Stroke::new(1.0, accent))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());

                            ui.horizontal(|ui| {
                                ui.label(RichText::new("✨").size(28.0));
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new("Transform on Paste")
                                            .size(18.0)
                                            .strong()
                                            .color(accent),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "Like PowerToys Advanced Paste — for macOS",
                                        )
                                        .size(12.0)
                                        .color(Color32::WHITE),
                                    );
                                });
                            });

                            ui.add_space(10.0);

                            let tips = [
                                ("📋 Copy anything", "HTML, code, JSON, messy text"),
                                (
                                    "⌃⇧V to paste",
                                    "Content is auto-cleaned before it hits your doc",
                                ),
                                (
                                    "🧠 AI-powered",
                                    "Fix grammar, translate, convert code — hands-free",
                                ),
                            ];

                            for (title, desc) in tips {
                                ui.horizontal(|ui| {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(title)
                                            .size(12.5)
                                            .strong()
                                            .color(Color32::WHITE),
                                    );
                                    ui.label(
                                        RichText::new(format!("— {}", desc))
                                            .size(12.0)
                                            .color(Color32::from_rgb(200, 200, 200)),
                                    );
                                });
                            }

                            ui.add_space(8.0);

                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Got it, let's set it up →")
                                            .size(12.0)
                                            .strong()
                                            .color(Color32::WHITE),
                                    )
                                    .fill(accent)
                                    .rounding(Rounding::same(8.0)),
                                )
                                .clicked()
                            {
                                self.paste_settings.onboarding_seen = true;
                                save_paste_transform_settings(&self.paste_settings);
                            }
                        });

                    ui.add_space(4.0);
                }

                // Settings body
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(20.0, 12.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());

                        // Master toggle
                        ui.horizontal(|ui| {
                            let toggle_color = if self.paste_settings.enabled {
                                rgb(c.green)
                            } else {
                                rgb(c.subtext)
                            };

                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(if self.paste_settings.enabled {
                                            "● ON"
                                        } else {
                                            "○ OFF"
                                        })
                                        .size(12.0)
                                        .strong()
                                        .color(if self.paste_settings.enabled {
                                            Color32::WHITE
                                        } else {
                                            rgb(c.subtext)
                                        }),
                                    )
                                    .fill(if self.paste_settings.enabled {
                                        rgb(c.green)
                                    } else {
                                        rgb(c.bg_elevated)
                                    })
                                    .rounding(Rounding::same(12.0))
                                    .stroke(Stroke::new(
                                        1.0,
                                        if self.paste_settings.enabled {
                                            rgb(c.green)
                                        } else {
                                            rgb(c.border)
                                        },
                                    )),
                                )
                                .clicked()
                            {
                                self.paste_settings.enabled = !self.paste_settings.enabled;
                                save_paste_transform_settings(&self.paste_settings);
                            }

                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("Transform on Paste")
                                    .size(14.0)
                                    .strong()
                                    .color(toggle_color),
                            );

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let hk_col = rgb(c.accent);
                                    egui::Frame::none()
                                        .fill(pill_bg(hk_col))
                                        .rounding(Rounding::same(6.0))
                                        .inner_margin(Margin::symmetric(6.0, 2.0))
                                        .stroke(Stroke::new(0.5, hk_col))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new("⌃⇧V")
                                                    .size(11.0)
                                                    .strong()
                                                    .color(Color32::WHITE)
                                                    .family(egui::FontFamily::Monospace),
                                            );
                                        });
                                    ui.label(
                                        RichText::new("Hotkey:")
                                            .size(11.0)
                                            .color(rgb(c.subtext)),
                                    );
                                },
                            );
                        });

                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "When enabled, ⌃⇧V pastes with auto-transforms applied. \
                                 Regular ⌘V still pastes normally.",
                            )
                            .size(11.5)
                            .color(rgb(c.subtext)),
                        );

                        ui.add_space(12.0);

                        // Smart mode toggle
                        egui::Frame::none()
                            .fill(rgb(c.bg_surface))
                            .rounding(Rounding::same(10.0))
                            .inner_margin(Margin::symmetric(14.0, 10.0))
                            .stroke(Stroke::new(
                                1.0,
                                Color32::from_rgb(180, 140, 255),
                            ))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut self.paste_settings.smart_mode,
                                            "",
                                        )
                                        .changed()
                                    {
                                        save_paste_transform_settings(&self.paste_settings);
                                    }
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new("🧠 Smart Mode")
                                                .size(13.0)
                                                .strong()
                                                .color(Color32::from_rgb(180, 140, 255)),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "Auto-detects content type and picks the best transform. \
                                                 JSON → pretty-print, HTML → markdown, code → format.",
                                            )
                                            .size(11.0)
                                            .color(rgb(c.subtext)),
                                        );
                                    });
                                });
                            });
                    });

                ui.add_space(4.0);

                // HUD overlay toggle
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(20.0, 0.0))
                    .show(ui, |ui| {
                        egui::Frame::none()
                            .fill(rgb(c.bg_surface))
                            .rounding(Rounding::same(10.0))
                            .inner_margin(Margin::symmetric(14.0, 10.0))
                            .stroke(Stroke::new(
                                1.0,
                                Color32::from_rgb(100, 200, 160),
                            ))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut self.paste_settings.hud_enabled,
                                            "",
                                        )
                                        .changed()
                                    {
                                        save_paste_transform_settings(&self.paste_settings);
                                    }
                                    ui.vertical(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new("HUD Notifications")
                                                    .strong()
                                                    .size(13.0)
                                                    .color(rgb(c.text)),
                                            )
                                            .selectable(false),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(
                                                    "Show a floating overlay when copying/pasting to slots.",
                                                )
                                                .size(11.0)
                                                .color(rgb(c.subtext)),
                                            )
                                            .selectable(false),
                                        );
                                    });
                                });
                            });
                    });

                ui.add_space(4.0);

                // Transform selection
                egui::Frame::none()
                    .inner_margin(Margin::symmetric(20.0, 0.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());

                        ui.label(
                            RichText::new("ACTIVE TRANSFORMS")
                                .size(11.0)
                                .strong()
                                .color(rgb(c.text)),
                        );
                        ui.label(
                            RichText::new("Selected transforms are applied when you ⌃⇧V")
                                .size(11.0)
                                .color(rgb(c.subtext)),
                        );
                        ui.add_space(6.0);
                    });

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::none()
                            .inner_margin(Margin::symmetric(20.0, 0.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());

                                let transforms = self.transforms.clone();

                                let categories: &[(&str, Color32)] = &[
                                    ("FORMAT", Color32::from_rgb(130, 170, 255)),
                                    ("CASE", Color32::from_rgb(100, 200, 160)),
                                    ("AI ✨", Color32::from_rgb(255, 180, 80)),
                                ];

                                for (cat_key, cat_color) in categories {
                                    let cat_transforms: Vec<&TransformKind> = transforms
                                        .iter()
                                        .filter(|t| t.category() == *cat_key)
                                        .collect();

                                    if cat_transforms.is_empty() {
                                        continue;
                                    }

                                    ui.add_space(4.0);

                                    for t in &cat_transforms {
                                        let is_active =
                                            self.paste_settings.active_transforms.contains(t);

                                        let (fill, border_col) = if is_active {
                                            (
                                                pill_bg(*cat_color),
                                                *cat_color,
                                            )
                                        } else {
                                            (rgb(c.bg_surface), rgb(c.border))
                                        };

                                        egui::Frame::none()
                                            .fill(fill)
                                            .rounding(Rounding::same(8.0))
                                            .inner_margin(Margin::symmetric(12.0, 7.0))
                                            .stroke(Stroke::new(1.0, border_col))
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.horizontal(|ui| {
                                                    let check_text = if is_active {
                                                        "✓"
                                                    } else {
                                                        "○"
                                                    };
                                                    let check_col = if is_active {
                                                        Color32::WHITE
                                                    } else {
                                                        rgb(c.overlay)
                                                    };

                                                    ui.label(
                                                        RichText::new(check_text)
                                                            .size(14.0)
                                                            .color(check_col)
                                                            .strong(),
                                                    );
                                                    ui.add_space(4.0);

                                                    let label_col = if is_active {
                                                        Color32::WHITE
                                                    } else {
                                                        rgb(c.subtext)
                                                    };
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{} {}",
                                                            t.icon(),
                                                            t.label()
                                                        ))
                                                        .size(12.5)
                                                        .color(label_col),
                                                    );

                                                    if t.is_ai() {
                                                        ui.with_layout(
                                                            egui::Layout::right_to_left(
                                                                egui::Align::Center,
                                                            ),
                                                            |ui| {
                                                                let ai_col = Color32::from_rgb(255, 180, 80);
                                                                egui::Frame::none()
                                                                    .fill(pill_bg(ai_col))
                                                                    .rounding(Rounding::same(4.0))
                                                                    .inner_margin(
                                                                        Margin::symmetric(
                                                                            5.0, 1.0,
                                                                        ),
                                                                    )
                                                                    .stroke(Stroke::new(0.5, ai_col))
                                                                    .show(ui, |ui| {
                                                                        ui.label(
                                                                            RichText::new("AI")
                                                                                .size(9.0)
                                                                                .strong()
                                                                                .color(Color32::WHITE),
                                                                        );
                                                                    });
                                                            },
                                                        );
                                                    }
                                                });
                                            });

                                        let last_rect = ui.min_rect();
                                        let resp = ui.interact(
                                            last_rect,
                                            egui::Id::new(("tf_toggle", t.label())),
                                            egui::Sense::click(),
                                        );

                                        if resp.clicked() {
                                            if is_active {
                                                self.paste_settings
                                                    .active_transforms
                                                    .retain(|x| x != *t);
                                            } else {
                                                self.paste_settings
                                                    .active_transforms
                                                    .push((*t).clone());
                                            }
                                            save_paste_transform_settings(&self.paste_settings);
                                        }

                                        ui.add_space(2.0);
                                    }

                                    ui.add_space(4.0);
                                }

                                // Optional AI step on paste (not the slot HUD — separate feature)
                                ui.add_space(8.0);
                                egui::Frame::none()
                                    .fill(rgb(c.bg_surface))
                                    .rounding(Rounding::same(10.0))
                                    .inner_margin(Margin::symmetric(14.0, 10.0))
                                    .stroke(Stroke::new(1.0, accent))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());

                                        ui.label(
                                            RichText::new("✨ Optional: AI text transform on paste")
                                                .size(12.0)
                                                .strong()
                                                .color(accent),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "If you enter instructions here, clipd sends clipboard text \
                                                 to your configured LLM before pasting (smart paste ⌃⇧V, \
                                                 and transform-on-paste). Leave empty to disable.",
                                            )
                                            .size(11.0)
                                            .color(rgb(c.subtext)),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "Requires api_key in ~/.local/share/clipd/transform.json \
                                                 (OpenAI-compatible endpoint). Not related to the slot HUD.",
                                            )
                                            .size(10.0)
                                            .color(rgb(c.subtext)),
                                        );
                                        ui.add_space(4.0);

                                        egui::Frame::none()
                                            .fill(rgb(c.bg_elevated))
                                            .rounding(Rounding::same(8.0))
                                            .inner_margin(Margin::symmetric(8.0, 6.0))
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                let resp = ui.add_sized(
                                                    [ui.available_width(), 28.0],
                                                    egui::TextEdit::singleline(
                                                        &mut self.paste_settings.default_ai_prompt,
                                                    )
                                                    .hint_text(
                                                        "e.g. Fix grammar — or leave empty",
                                                    )
                                                    .frame(false)
                                                    .font(egui::TextStyle::Body),
                                                );
                                                if resp.changed() || resp.lost_focus() {
                                                    save_paste_transform_settings(
                                                        &self.paste_settings,
                                                    );
                                                }
                                            });
                                    });

                                ui.add_space(12.0);

                                // ── Export History ──
                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("EXPORT HISTORY")
                                        .size(11.0)
                                        .strong()
                                        .color(rgb(c.text)),
                                );
                                ui.label(
                                    RichText::new(format!("{} clips saved to your Documents folder", self.clips.len()))
                                        .size(11.0)
                                        .color(rgb(c.subtext)),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    let txt_col = Color32::from_rgb(100, 180, 255);
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("📄 Export .txt").size(12.0).color(Color32::WHITE),
                                            )
                                            .fill(pill_bg(txt_col))
                                            .rounding(Rounding::same(6.0))
                                            .stroke(Stroke::new(1.0, txt_col)),
                                        )
                                        .clicked()
                                    {
                                        match self.do_export_text() {
                                            Ok(path) => self.export_status = Some((format!("✓ Saved: {}", path), Instant::now())),
                                            Err(e) => self.export_status = Some((format!("✗ {}", e), Instant::now())),
                                        }
                                    }
                                    ui.add_space(6.0);
                                    let csv_col = Color32::from_rgb(100, 210, 140);
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("📊 Export .csv").size(12.0).color(Color32::WHITE),
                                            )
                                            .fill(pill_bg(csv_col))
                                            .rounding(Rounding::same(6.0))
                                            .stroke(Stroke::new(1.0, csv_col)),
                                        )
                                        .clicked()
                                    {
                                        match self.do_export_csv() {
                                            Ok(path) => self.export_status = Some((format!("✓ Saved: {}", path), Instant::now())),
                                            Err(e) => self.export_status = Some((format!("✗ {}", e), Instant::now())),
                                        }
                                    }
                                });
                                if let Some((msg, t)) = &self.export_status {
                                    if t.elapsed() < Duration::from_secs(6) {
                                        ui.add_space(4.0);
                                        let col = if msg.starts_with('✗') {
                                            Color32::from_rgb(255, 100, 100)
                                        } else {
                                            Color32::from_rgb(100, 210, 140)
                                        };
                                        ui.label(RichText::new(msg).size(11.0).color(col));
                                    } else {
                                        self.export_status = None;
                                    }
                                }

                                ui.add_space(12.0);

                                // ── Danger zone: clear all history ──
                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("DANGER ZONE")
                                        .size(11.0)
                                        .strong()
                                        .color(Color32::from_rgb(200, 60, 60)),
                                );
                                ui.add_space(4.0);

                                if self.confirm_clear_all {
                                    ui.label(
                                        RichText::new("Delete all clipboard history? This cannot be undone.")
                                            .size(12.0)
                                            .color(Color32::from_rgb(255, 100, 100)),
                                    );
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("✕ Yes, delete everything")
                                                        .size(12.0)
                                                        .strong()
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(Color32::from_rgb(180, 40, 40))
                                                .rounding(Rounding::same(6.0)),
                                            )
                                            .clicked()
                                        {
                                            let _ = self.store.clear_all();
                                            self.confirm_clear_all = false;
                                            self.refresh();
                                        }
                                        ui.add_space(8.0);
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("Cancel").size(12.0).color(rgb(c.text)),
                                                )
                                                .fill(rgb(c.bg_elevated))
                                                .rounding(Rounding::same(6.0)),
                                            )
                                            .clicked()
                                        {
                                            self.confirm_clear_all = false;
                                        }
                                    });
                                } else if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("🗑 Clear All History")
                                                .size(12.0)
                                                .color(Color32::from_rgb(255, 100, 100)),
                                        )
                                        .fill(rgb(c.bg_elevated))
                                        .rounding(Rounding::same(6.0))
                                        .stroke(Stroke::new(1.0, Color32::from_rgb(180, 40, 40))),
                                    )
                                    .clicked()
                                {
                                    self.confirm_clear_all = true;
                                }

                                ui.add_space(8.0);
                            });
                    });
            });

        if !open {
            self.show_transforms = false;
        }
    }
}

fn render_preview(
    ui: &mut egui::Ui,
    clip: &ClipEntry,
    action: &mut Action,
    c: &clipd_core::ThemeColors,
) {
    let type_color = match clip.content_type {
        ContentType::Code => rgb(c.code),
        ContentType::Url => rgb(c.url),
        ContentType::Email => rgb(c.email),
        ContentType::Path => rgb(c.path),
        _ => rgb(c.accent),
    };

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;

        let pill = |ui: &mut egui::Ui, text: &str, col: Color32| {
            egui::Frame::none()
                .fill(pill_bg(col))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(8.0, 3.0))
                .stroke(Stroke::new(1.0, col))
                .show(ui, |ui| {
                    ui.label(RichText::new(text).size(11.5).color(Color32::WHITE).strong());
                });
        };

        pill(
            ui,
            &format!("{} {}", clip.content_type.icon(), clip.content_type.as_str()),
            type_color,
        );

        if let Some(ref app) = clip.source_app {
            pill(ui, app, rgb(c.subtext));
        }
        pill(ui, &format!("id:{}", clip.id), rgb(c.overlay));
        pill(ui, &relative_time(&clip.timestamp), rgb(c.subtext));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;

            let copy_col = rgb(c.green);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("📋 Copy")
                            .size(12.5)
                            .strong()
                            .color(copy_col),
                    )
                    .fill(pill_bg(copy_col))
                    .rounding(Rounding::same(8.0))
                    .stroke(Stroke::new(1.0, copy_col)),
                )
                .clicked()
            {
                *action = Action::Copy;
            }

            let ps_col = rgb(c.accent);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("⚙ Paste Settings")
                            .size(12.5)
                            .strong()
                            .color(ps_col),
                    )
                    .fill(pill_bg(ps_col))
                    .rounding(Rounding::same(8.0))
                    .stroke(Stroke::new(1.0, ps_col)),
                )
                .clicked()
            {
                *action = Action::ToggleTransform;
            }
        });
    });

    ui.add_space(10.0);

    let line_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(ui.available_width(), 1.0),
    );
    ui.painter().rect_filled(
        line_rect,
        Rounding::ZERO,
        pill_bg(type_color),
    );
    ui.allocate_space(egui::vec2(ui.available_width(), 1.0));

    ui.add_space(10.0);

    egui::Frame::none()
        .fill(rgb(c.bg_surface))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::same(16.0))
        .stroke(Stroke::new(1.0, rgb(c.border)))
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let font = if clip.content_type == ContentType::Code {
                        FontId::monospace(13.0)
                    } else {
                        FontId::proportional(14.0)
                    };
                    ui.label(
                        RichText::new(&clip.content).font(font).color(rgb(c.text)),
                    );
                });
        });
}

fn render_empty_preview(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(100.0);
            ui.label(RichText::new("📋").size(56.0));
            ui.add_space(16.0);
            ui.label(
                RichText::new("Select a clip to preview")
                    .size(16.0)
                    .color(rgb(c.overlay)),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("Use ↑↓ arrows or click from the list")
                    .size(12.0)
                    .color(rgb(c.overlay)),
            );
        });
    });
}

// ── Helpers ──

fn relative_time(dt: &DateTime<Utc>) -> String {
    let secs = Utc::now().signed_duration_since(*dt).num_seconds();
    if secs < 60 {
        return "just now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{}d ago", days);
    }
    if days < 30 {
        return format!("{}w ago", days / 7);
    }
    dt.format("%b %d").to_string()
}
