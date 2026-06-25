//! clipd's cross-platform corner toast — a small, styled, always-on-top overlay
//! window (the Windows/Linux stand-in for the macOS Swift HUD). The daemon
//! spawns it with the message as args; it shows for ~1.5s, then exits. It runs
//! as its own short-lived process, never in the daemon's hot path, so it can't
//! affect typing or general performance.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, Margin, RichText, Rounding, Stroke};
use std::time::{Duration, Instant};

struct Overlay {
    text: String,
    start: Instant,
    positioned: bool,
}

impl eframe::App for Overlay {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // transparent — only the rounded card is painted
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Park it in the top-right corner once the monitor size is known.
        if !self.positioned {
            if let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) {
                let x = (monitor.x - 320.0).max(12.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, 46.0)));
                self.positioned = true;
            }
        }

        // Auto-dismiss.
        if self.start.elapsed() > Duration::from_millis(1500) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(100));

        let accent = Color32::from_rgb(255, 160, 50);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(28, 30, 38, 240))
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 28)))
                    .inner_margin(Margin::symmetric(16.0, 13.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Accent clipboard-tile glyph.
                    let (tile, _) =
                        ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        tile,
                        Rounding::same(7.0),
                        Color32::from_rgba_unmultiplied(255, 160, 50, 46),
                    );
                    ui.painter().text(
                        tile.center(),
                        egui::Align2::CENTER_CENTER,
                        "📋",
                        egui::FontId::proportional(13.0),
                        accent,
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(&self.text)
                            .size(14.0)
                            .strong()
                            .color(Color32::from_rgb(240, 243, 248)),
                    );
                });
            });
    }
}

fn main() -> eframe::Result {
    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let text = if text.trim().is_empty() {
        "clipd".to_string()
    } else {
        text
    };

    // Hard safety net: even if the event loop misbehaves (no display, stuck
    // repaint), the process always exits — never a lingering overlay window.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(1900));
        std::process::exit(0);
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([300.0, 58.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false),
        ..Default::default()
    };

    eframe::run_native(
        "clipd-overlay",
        options,
        Box::new(|_cc| {
            Ok(Box::new(Overlay {
                text,
                start: Instant::now(),
                positioned: false,
            }))
        }),
    )
}
