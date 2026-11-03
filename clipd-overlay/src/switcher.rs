//! The clipboard switcher strip for Windows and Linux: hold Alt, tap V.
//!
//! The macOS strip is drawn by the Swift overlay; this is its counterpart. It
//! runs as one long-lived process the daemon starts and feeds over stdin —
//! one NUL-terminated message per tap, in the same format the Swift overlay
//! reads (`clipd_core::fast_paste::switcher_payload`, `HIDE_PAYLOAD`) — so
//! each tap is a redraw rather than a process launch.
//!
//! The window never takes focus: it is created inactive, ignores the mouse,
//! and when there is nothing to show it is parked off screen rather than
//! hidden and re-shown, because showing a window on Windows activates it —
//! and the app that had focus is the one the clip is about to be pasted into.

use eframe::egui::{self, Color32, FontId, Rounding, Stroke};
use std::io::Read;
use std::sync::{Arc, Mutex};

/// One clip in the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub number: String,
    pub kind: String,
    pub app: String,
    pub source: String,
    pub preview: String,
}

/// What to draw, or nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub cards: Vec<Card>,
    pub selected: usize,
    /// Under the text cursor, in physical screen pixels, when known.
    pub at: Option<(f32, f32)>,
}

/// A message from the daemon: `Some(view)` to show, `None` to take it down.
pub fn parse(message: &str) -> Option<View> {
    let mut style = "";
    let mut view = View {
        cards: Vec::new(),
        selected: 0,
        at: None,
    };
    for line in message.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["STYLE", s, ..] => style = s,
            ["SEL", n, ..] => view.selected = n.parse().unwrap_or(0),
            ["AT", x, y, ..] => {
                view.at = x.parse().ok().zip(y.parse().ok());
            }
            ["CARD", number, kind, app, source, preview, ..] => view.cards.push(Card {
                number: number.to_string(),
                kind: kind.to_string(),
                app: app.to_string(),
                source: source.to_string(),
                preview: preview.to_string(),
            }),
            _ => {}
        }
    }
    (style == "switcher" && !view.cards.is_empty()).then_some(view)
}

const CARD_W: f32 = 150.0;
const CARD_H: f32 = 76.0;
const GAP: f32 = 6.0;
const PAD: f32 = 8.0;
/// Where a window goes when there is nothing to show.
const PARKED: egui::Pos2 = egui::pos2(-20000.0, -20000.0);

struct Strip {
    latest: Arc<Mutex<Option<View>>>,
    shown: Option<View>,
}

impl Strip {
    fn size(cards: usize, screen_w: f32) -> egui::Vec2 {
        let n = cards.max(1) as f32;
        let card_w = CARD_W.min((screen_w - 40.0 - PAD * 2.0 - GAP * (n - 1.0)) / n).max(70.0);
        egui::vec2(PAD * 2.0 + card_w * n + GAP * (n - 1.0), CARD_H + PAD * 2.0)
    }
}

impl eframe::App for Strip {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let latest = self.latest.lock().ok().and_then(|v| v.clone());
        if latest != self.shown {
            self.shown = latest;
            match &self.shown {
                Some(view) => {
                    let ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0).max(0.5);
                    let monitor = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1920.0, 1080.0));
                    let size = Strip::size(view.cards.len(), monitor.x);
                    let pos = match view.at {
                        // The caret is reported in physical pixels; the window
                        // is placed in points. Just below the line.
                        Some((x, y)) => egui::pos2(x / ppp - 18.0, y / ppp + 8.0),
                        None => egui::pos2((monitor.x - size.x) / 2.0, monitor.y * 0.62),
                    };
                    let pos = egui::pos2(
                        pos.x.clamp(6.0, (monitor.x - size.x - 6.0).max(6.0)),
                        if pos.y + size.y > monitor.y - 6.0 { pos.y - size.y - 34.0 } else { pos.y },
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                }
                None => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(PARKED));
                }
            }
        }

        let Some(view) = &self.shown else {
            return;
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let full = ui.max_rect();
                let painter = ui.painter();
                painter.rect(
                    full,
                    Rounding::same(14.0),
                    Color32::from_rgba_unmultiplied(18, 18, 20, 245),
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 20)),
                );
                let n = view.cards.len() as f32;
                let card_w = (full.width() - PAD * 2.0 - GAP * (n - 1.0)) / n;
                for (i, card) in view.cards.iter().enumerate() {
                    let selected = i == view.selected;
                    let min = full.min + egui::vec2(PAD + i as f32 * (card_w + GAP), PAD);
                    let rect = egui::Rect::from_min_size(min, egui::vec2(card_w, CARD_H));
                    painter.rect(
                        rect,
                        Rounding::same(10.0),
                        Color32::from_white_alpha(if selected { 36 } else { 10 }),
                        if selected {
                            Stroke::new(2.0, Color32::from_rgb(115, 158, 242))
                        } else {
                            Stroke::new(1.0, Color32::from_white_alpha(14))
                        },
                    );
                    let dim = |a: u8| Color32::from_white_alpha(a);
                    painter.text(
                        rect.min + egui::vec2(8.0, 6.0),
                        egui::Align2::LEFT_TOP,
                        &card.number,
                        FontId::proportional(11.0),
                        dim(if selected { 230 } else { 110 }),
                    );
                    painter.text(
                        rect.min + egui::vec2(22.0, 6.0),
                        egui::Align2::LEFT_TOP,
                        truncate(&card.app, ((card_w - 30.0) / 6.0) as usize),
                        FontId::proportional(10.5),
                        dim(140),
                    );
                    let preview_rect = egui::Rect::from_min_size(
                        rect.min + egui::vec2(8.0, 22.0),
                        egui::vec2(card_w - 16.0, 34.0),
                    );
                    let galley = ui.fonts(|f| {
                        f.layout(
                            card.preview.clone(),
                            FontId::proportional(12.5),
                            dim(if selected { 250 } else { 205 }),
                            preview_rect.width(),
                        )
                    });
                    painter.with_clip_rect(preview_rect).galley(preview_rect.min, galley, Color32::WHITE);
                    if !card.source.is_empty() {
                        painter.text(
                            egui::pos2(rect.min.x + 8.0, rect.max.y - 6.0),
                            egui::Align2::LEFT_BOTTOM,
                            truncate(&card.source, ((card_w - 16.0) / 5.0) as usize),
                            FontId::proportional(9.5),
                            dim(95),
                        );
                    }
                    let _ = &card.kind;
                }
            });
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max.max(4) {
        format!("{}…", text.chars().take(max.max(4) - 1).collect::<String>())
    } else {
        text.to_string()
    }
}

/// Serve the strip until the daemon closes stdin.
pub fn run() -> eframe::Result {
    let latest: Arc<Mutex<Option<View>>> = Arc::new(Mutex::new(None));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([CARD_W + PAD * 2.0, CARD_H + PAD * 2.0])
            .with_position(PARKED)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_mouse_passthrough(true),
        ..Default::default()
    };
    eframe::run_native(
        "clipd-switcher",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let feed = latest.clone();
            std::thread::spawn(move || {
                let mut stdin = std::io::stdin();
                let mut pending: Vec<u8> = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stdin.read(&mut buf) {
                        Ok(0) | Err(_) => std::process::exit(0),
                        Ok(n) => n,
                    };
                    pending.extend_from_slice(&buf[..n]);
                    while let Some(end) = pending.iter().position(|b| *b == 0) {
                        let message = String::from_utf8_lossy(&pending[..end]).to_string();
                        pending.drain(..=end);
                        if let Ok(mut slot) = feed.lock() {
                            *slot = parse(&message);
                        }
                        ctx.request_repaint();
                    }
                }
            });
            Ok(Box::new(Strip { latest, shown: None }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_same_messages_the_mac_overlay_does() {
        let msg = "STYLE\tswitcher\nAT\t300\t420\nSEL\t1\nCARD\t1\ttext\tSlack\t\tlatest\nCARD\t2\tlink\tEdge\tPR #28\thttps://x\n";
        let view = parse(msg).unwrap();
        assert_eq!(view.selected, 1);
        assert_eq!(view.at, Some((300.0, 420.0)));
        assert_eq!(view.cards.len(), 2);
        assert_eq!(view.cards[1].source, "PR #28");
        assert_eq!(parse("STYLE\thide\n"), None);
        assert_eq!(parse("STYLE\tswitcher\nSEL\t0\n"), None, "nothing to show");
    }
}
