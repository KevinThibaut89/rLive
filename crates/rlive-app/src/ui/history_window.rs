//! The Undo History window (manual pp. 135-136): every step since the Set was opened, newest at
//! the top, undoing or redoing to whichever is picked.

use super::theme::*;
use super::RliveApp;
use egui::{Key, Sense, Vec2};

impl RliveApp {
    pub(super) fn undo_history_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_undo_history {
            return;
        }
        let steps = self.session.undo_steps();
        let at = self.session.undo_position();
        let mut open = true;
        let mut jump = None;
        let mut selected = self
            .ui
            .undo_history_cursor
            .unwrap_or(at)
            .min(steps.len() - 1);
        egui::Window::new("Undo History")
            .open(&mut open)
            .default_size([240.0, 320.0])
            .default_pos([340.0, 80.0])
            .show(ctx, |ui| {
                let (up, down, enter) = ui.input(|i| {
                    (
                        i.key_pressed(Key::ArrowUp),
                        i.key_pressed(Key::ArrowDown),
                        i.key_pressed(Key::Enter),
                    )
                });
                if up && selected + 1 < steps.len() {
                    selected += 1;
                }
                if down && selected > 0 {
                    selected -= 1;
                }
                if enter {
                    jump = Some(selected);
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, label) in steps.iter().enumerate().rev() {
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 18.0),
                            Sense::click(),
                        );
                        if i == selected {
                            ui.painter().rect_filled(rect, 0.0, shade(0x46525e));
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 0.0, pal().raised);
                        }
                        let undone = i > at;
                        ui.painter().text(
                            rect.left_center() + Vec2::new(6.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            *label,
                            font(11.0),
                            if undone { pal().text_faint } else { pal().text },
                        );
                        if resp.clicked() {
                            selected = i;
                            jump = Some(i);
                        }
                    }
                });
            });
        self.ui.undo_history_cursor = Some(selected);
        if !open {
            self.ui.show_undo_history = false;
            self.ui.undo_history_cursor = None;
        }
        if let Some(step) = jump {
            if self.session.jump_to_step(step) {
                self.ui.undo_history_cursor = Some(step);
            }
        }
    }
}
