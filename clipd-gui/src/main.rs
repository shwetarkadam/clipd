use arboard::Clipboard;
use chrono::{DateTime, Utc};
use clipd_core::{load_theme, save_theme, ClipEntry, ClipStore, ContentType, Rgb, Theme};
use eframe::egui::{self, Color32, FontId, Margin, RichText, Rounding, Stroke};
use std::time::{Duration, Instant};

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

enum Action {
    None,
    Copy,
    Delete,
}

// ── Entry point ──

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "clipd",
        options,
        Box::new(|cc| {
            let theme = load_theme();
            apply_theme(&cc.egui_ctx, theme);
            Ok(Box::new(ClipdGui::new(theme)))
        }),
    )
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    let c = theme.colors();

    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Heading, FontId::proportional(20.0));
    style.text_styles.insert(egui::TextStyle::Small, FontId::proportional(11.0));
    style.text_styles.insert(egui::TextStyle::Button, FontId::proportional(13.0));
    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    ctx.set_style(style);

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(rgb(c.text));
    v.panel_fill = rgb(c.bg_base);
    v.window_fill = rgb(c.bg_base);
    v.extreme_bg_color = rgb(c.bg_base);
    v.faint_bg_color = rgb(c.bg_surface);
    v.widgets.noninteractive.bg_fill = rgb(c.bg_surface);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, rgb(c.text));
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.bg_fill = rgb(c.bg_elevated);
    v.widgets.hovered.bg_fill = rgb(c.bg_hover);
    v.widgets.active.bg_fill = rgb(c.bg_selected);
    v.selection.bg_fill = Color32::from_rgba_premultiplied(c.accent.0, c.accent.1, c.accent.2, 50);
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
}

impl ClipdGui {
    fn new(theme: Theme) -> Self {
        let db_path = ClipStore::default_path();
        let store = ClipStore::new(&db_path).expect("Failed to open clip database");
        let clips = store.get_recent(500).unwrap_or_default();
        let count = clips.len();
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
        }
    }

    fn refresh(&mut self) {
        self.clips = self.store.get_recent(500).unwrap_or_default();
        self.apply_filter();
        self.last_refresh = Instant::now();
    }

    fn apply_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered = (0..self.clips.len()).collect();
        } else {
            let q = self.search_query.to_lowercase();
            self.filtered = self
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
                    .inner_margin(Margin::symmetric(16.0, 12.0))
                    .stroke(Stroke::new(1.0, rgb(c.border))),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("📋").size(22.0));
                    ui.label(RichText::new("clipd").size(18.0).strong().color(rgb(c.accent)));
                    ui.add_space(16.0);

                    let search = ui.add_sized(
                        [ui.available_width() - 160.0, 28.0],
                        egui::TextEdit::singleline(&mut self.search_query)
                            .id(egui::Id::new("clip_search"))
                            .hint_text("Search clips…")
                            .font(egui::TextStyle::Body),
                    );
                    if self.focus_search {
                        search.request_focus();
                        self.focus_search = false;
                    }
                    if search.changed() {
                        self.apply_filter();
                    }

                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("{} clips", self.filtered.len()))
                            .size(12.0)
                            .color(rgb(c.subtext)),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    });
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
                    for (key, desc) in [
                        ("↑↓", "Navigate"),
                        ("Enter", "Copy"),
                        ("⌘D", "Delete"),
                        ("⌘T", "Theme"),
                        ("Esc", "Close"),
                    ] {
                        ui.label(RichText::new(key).size(11.0).color(rgb(c.accent)).strong());
                        ui.label(RichText::new(desc).size(11.0).color(rgb(c.subtext)));
                        ui.add_space(12.0);
                    }

                    if let Some(t) = self.copied_at {
                        if t.elapsed() < Duration::from_secs(2) {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new("✓ Copied to clipboard!")
                                            .size(12.0)
                                            .color(rgb(c.green))
                                            .strong(),
                                    );
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
            .default_width(340.0)
            .width_range(250.0..=500.0)
            .frame(
                egui::Frame::none()
                    .fill(rgb(c.bg_base))
                    .inner_margin(Margin::symmetric(6.0, 6.0)),
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
                    .inner_margin(Margin::symmetric(20.0, 16.0)),
            )
            .show(ctx, |ui| {
                if let Some(clip) = &preview_data {
                    render_preview(ui, clip, &mut action, &c);
                } else {
                    render_empty_preview(ui, &c);
                }
            });

        match action {
            Action::Copy => self.do_copy(),
            Action::Delete => self.do_delete(),
            Action::None => {}
        }
    }
}

impl ClipdGui {
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

                    let bg = if is_selected {
                        rgb(c.bg_selected)
                    } else if is_hovered {
                        rgb(c.bg_hover)
                    } else {
                        Color32::TRANSPARENT
                    };
                    let text_color = if is_selected { Color32::WHITE } else { rgb(c.text) };
                    let meta_color = if is_selected {
                        Color32::from_rgb(c.subtext.0.saturating_add(20), c.subtext.1.saturating_add(20), c.subtext.2.saturating_add(20))
                    } else {
                        rgb(c.subtext)
                    };

                    let frame_resp = egui::Frame::none()
                        .fill(bg)
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::symmetric(10.0, 8.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());

                            let preview = clip.preview.trim().replace('\n', " ");
                            let truncated: String = preview.chars().take(55).collect();
                            let suffix = if preview.chars().count() > 55 { "…" } else { "" };

                            ui.label(
                                RichText::new(format!("{}{}", truncated, suffix))
                                    .size(13.0)
                                    .color(text_color),
                            );

                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(RichText::new(clip.content_type.icon()).size(11.0));
                                ui.label(
                                    RichText::new(clip.content_type.as_str())
                                        .size(11.0)
                                        .color(meta_color),
                                );
                                if let Some(ref app) = clip.source_app {
                                    ui.label(
                                        RichText::new("·").size(11.0).color(rgb(c.overlay)),
                                    );
                                    ui.label(
                                        RichText::new(app).size(11.0).color(meta_color),
                                    );
                                }
                                ui.label(RichText::new("·").size(11.0).color(rgb(c.overlay)));
                                ui.label(
                                    RichText::new(relative_time(&clip.timestamp))
                                        .size(11.0)
                                        .color(meta_color),
                                );
                            });
                        })
                        .response;

                    let resp = ui.interact(
                        frame_resp.rect,
                        clip_id,
                        egui::Sense::click(),
                    );

                    ui.ctx().data_mut(|d| d.insert_temp(clip_id, resp.hovered()));

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

                    ui.add_space(2.0);
                }
                self.scroll_to_selected = false;
            });
    }
}

fn render_preview(
    ui: &mut egui::Ui,
    clip: &ClipEntry,
    action: &mut Action,
    c: &clipd_core::ThemeColors,
) {
    ui.horizontal(|ui| {
        let badge = egui::Frame::none()
            .fill(rgb(c.bg_elevated))
            .rounding(Rounding::same(4.0))
            .inner_margin(Margin::symmetric(6.0, 2.0));
        badge.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(clip.content_type.icon()).size(12.0));
                ui.label(
                    RichText::new(clip.content_type.as_str())
                        .size(12.0)
                        .color(rgb(c.accent)),
                );
            });
        });

        if let Some(ref app) = clip.source_app {
            ui.label(RichText::new("|").size(13.0).color(rgb(c.overlay)));
            ui.label(RichText::new(app).size(13.0).color(rgb(c.subtext)));
        }
        ui.label(RichText::new("|").size(13.0).color(rgb(c.overlay)));
        ui.label(
            RichText::new(format!("id:{}", clip.id))
                .size(13.0)
                .color(rgb(c.overlay)),
        );
        ui.label(RichText::new("|").size(13.0).color(rgb(c.overlay)));
        ui.label(
            RichText::new(relative_time(&clip.timestamp))
                .size(13.0)
                .color(rgb(c.subtext)),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("📋 Copy").size(12.0).color(rgb(c.bg_base)),
                    )
                    .fill(rgb(c.accent2))
                    .rounding(Rounding::same(6.0)),
                )
                .clicked()
            {
                *action = Action::Copy;
            }
        });
    });

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);

    egui::Frame::none()
        .fill(rgb(c.bg_surface))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::same(16.0))
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
                    ui.label(RichText::new(&clip.content).font(font).color(rgb(c.text)));
                });
        });
}

fn render_empty_preview(ui: &mut egui::Ui, c: &clipd_core::ThemeColors) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(100.0);
            ui.label(RichText::new("📋").size(48.0));
            ui.add_space(12.0);
            ui.label(
                RichText::new("Select a clip to preview")
                    .size(16.0)
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
