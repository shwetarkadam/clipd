//! clipd quick slot picker — a small focused popup (opened by a single safe
//! hotkey) that lists the filled slots; press a slot's key (1–9 / A–Z) to paste
//! it into the app you came from. This sidesteps Windows' chord-conflict mess:
//! the popup has keyboard focus, so the slot key is captured cleanly — no
//! system-wide interception, no AltGr/Win conflicts, scales to all 35 slots.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, Key, Margin, RichText, Rounding, Stroke};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Slot {
    label: String, // "1".."9", "A".."Z"
    key: Key,
    preview: String,
    content: String,
}

struct Picker {
    slots: Vec<Slot>,
    /// Set to the chosen slot's content (also placed on the clipboard) so that
    /// `main` can paste it after the window closes and focus returns.
    chosen: Arc<Mutex<Option<String>>>,
    positioned: bool,
}

fn preview(s: &str) -> String {
    let one = s.trim().replace(['\n', '\t'], " ");
    if one.chars().count() > 60 {
        let mut p: String = one.chars().take(60).collect();
        p.push('…');
        p
    } else {
        one
    }
}

fn char_key(c: char) -> Option<Key> {
    Some(match c {
        '1' => Key::Num1,
        '2' => Key::Num2,
        '3' => Key::Num3,
        '4' => Key::Num4,
        '5' => Key::Num5,
        '6' => Key::Num6,
        '7' => Key::Num7,
        '8' => Key::Num8,
        '9' => Key::Num9,
        'A' => Key::A, 'B' => Key::B, 'C' => Key::C, 'D' => Key::D, 'E' => Key::E,
        'F' => Key::F, 'G' => Key::G, 'H' => Key::H, 'I' => Key::I, 'J' => Key::J,
        'K' => Key::K, 'L' => Key::L, 'M' => Key::M, 'N' => Key::N, 'O' => Key::O,
        'P' => Key::P, 'Q' => Key::Q, 'R' => Key::R, 'S' => Key::S, 'T' => Key::T,
        'U' => Key::U, 'V' => Key::V, 'W' => Key::W, 'X' => Key::X, 'Y' => Key::Y,
        'Z' => Key::Z,
        _ => return None,
    })
}

fn load_slots() -> Vec<Slot> {
    let Ok(mgr) = clipd_core::SlotManager::persistent_default() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Numeric slots 1–9, then letter slots A–Z (slots 31..=56).
    let entries = (1u8..=9)
        .map(|n| (n.to_string(), n))
        .chain((0u8..26).map(|i| (((b'A' + i) as char).to_string(), 31 + i)));
    for (label, slot_id) in entries {
        if let Ok(Some(content)) = mgr.get_slot(slot_id) {
            if !content.trim().is_empty() {
                if let Some(key) = label.chars().next().and_then(char_key) {
                    out.push(Slot {
                        label,
                        key,
                        preview: preview(&content),
                        content,
                    });
                }
            }
        }
    }
    out
}

impl eframe::App for Picker {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Center on screen once the monitor size is known.
        if !self.positioned {
            if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
                let w = 360.0;
                let h = 60.0 + self.slots.len().max(1) as f32 * 30.0;
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    (m.x - w) * 0.5,
                    (m.y - h) * 0.35,
                )));
                self.positioned = true;
            }
        }

        // Escape cancels.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // A slot key pastes that slot.
        let mut pick: Option<usize> = None;
        ctx.input(|i| {
            for (idx, s) in self.slots.iter().enumerate() {
                if i.key_pressed(s.key) {
                    pick = Some(idx);
                    break;
                }
            }
        });
        if let Some(idx) = pick {
            let content = self.slots[idx].content.clone();
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(&content);
            }
            *self.chosen.lock().unwrap() = Some(content);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(80));

        let accent = Color32::from_rgb(255, 160, 50);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(28, 30, 38, 244))
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 28)))
                    .inner_margin(Margin::symmetric(16.0, 13.0)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Paste from slot — press a key")
                        .size(12.5)
                        .color(Color32::from_rgb(150, 158, 172)),
                );
                ui.add_space(8.0);
                if self.slots.is_empty() {
                    ui.label(
                        RichText::new("No slots saved yet — Ctrl+C ×N to fill them.")
                            .size(13.0)
                            .color(Color32::from_rgb(150, 158, 172)),
                    );
                }
                for s in &self.slots {
                    ui.horizontal(|ui| {
                        let (tile, _) =
                            ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                        ui.painter().rect_filled(
                            tile,
                            Rounding::same(6.0),
                            Color32::from_rgba_unmultiplied(255, 160, 50, 40),
                        );
                        ui.painter().text(
                            tile.center(),
                            egui::Align2::CENTER_CENTER,
                            &s.label,
                            egui::FontId::proportional(12.0),
                            accent,
                        );
                        ui.add_space(8.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(&s.preview)
                                    .size(13.0)
                                    .color(Color32::from_rgb(228, 232, 240)),
                            )
                            .truncate(),
                        );
                    });
                    ui.add_space(2.0);
                }
            });
    }
}

fn simulate_paste() {
    use enigo::{Direction, Enigo, Key as EKey, Keyboard, Settings};
    if let Ok(mut e) = Enigo::new(&Settings::default()) {
        #[cfg(target_os = "macos")]
        let modk = EKey::Meta;
        #[cfg(not(target_os = "macos"))]
        let modk = EKey::Control;
        let _ = e.key(modk, Direction::Press);
        let _ = e.key(EKey::Unicode('v'), Direction::Click);
        let _ = e.key(modk, Direction::Release);
    }
}

fn main() -> eframe::Result {
    // Hard safety net: never let the popup linger.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(8));
        std::process::exit(0);
    });

    let slots = load_slots();
    let rows = slots.len().max(1) as f32;
    let chosen = Arc::new(Mutex::new(None::<String>));
    let chosen_app = chosen.clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([360.0, 56.0 + rows * 30.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_active(true),
        ..Default::default()
    };

    eframe::run_native(
        "clipd-picker",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(Picker {
                slots,
                chosen: chosen_app,
                positioned: false,
            }))
        }),
    )?;

    // Window closed — if a slot was chosen, focus has returned to the previous
    // app, so paste it there.
    if chosen.lock().unwrap().take().is_some() {
        std::thread::sleep(Duration::from_millis(140));
        simulate_paste();
    }
    Ok(())
}
