//! The Info View: what the thing under the pointer is, and the user's own notes on it.

use super::theme::{self, pal};
use super::{widgets, RliveApp, UiState};
use crate::session::{InfoTarget, Session};
use egui::{RichText, Stroke};

/// Height of the Info View pane.
pub const HEIGHT: f32 = 108.0;

impl UiState {
    /// Opens the Info View's text editor on `target`.
    pub fn edit_info_text(&mut self, session: &Session, target: InfoTarget) {
        if let Some(text) = session.info_text(target) {
            self.info_edit = Some((target, text.to_string()));
            self.info_edit_focus = true;
        }
    }
}

/// The context-menu item that starts editing `target`'s info text.
pub fn edit_info_item(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    target: InfoTarget,
) {
    if ui.button("Edit Info Text").clicked() {
        state.edit_info_text(session, target);
        ui.close();
    }
}

impl RliveApp {
    pub(super) fn info_view(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter().line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, pal().separator),
        );
        let frame = egui::Frame::new().inner_margin(egui::Margin::same(6));
        frame.show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if self.ui.info_edit.is_some() {
                self.info_editor(ui);
            } else {
                describe_hovered(ui);
            }
        });
    }

    fn info_editor(&mut self, ui: &mut egui::Ui) {
        let Some((target, text)) = &mut self.ui.info_edit else {
            return;
        };
        let target = *target;
        ui.label(
            RichText::new("Info Text")
                .font(theme::font_bold(11.0))
                .color(pal().text),
        );
        let resp = ui.add(
            egui::TextEdit::multiline(text)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text("Type a note about this item…"),
        );
        if std::mem::take(&mut self.ui.info_edit_focus) {
            resp.request_focus();
        }
        if resp.lost_focus() {
            if let Some((_, text)) = self.ui.info_edit.take() {
                self.session.set_info_text(target, text);
            }
        }
    }
}

fn describe_hovered(ui: &mut egui::Ui) {
    let entry = widgets::info_entry(ui.ctx());
    let dim = pal().text_dim;
    let Some(e) = entry else {
        ui.label(
            RichText::new("Info View")
                .font(theme::font_bold(11.0))
                .color(dim),
        );
        ui.label(
            RichText::new("Hover over a control to see what it does.")
                .font(theme::font(11.0))
                .color(dim),
        );
        return;
    };
    ui.label(
        RichText::new(e.name)
            .font(theme::font_bold(11.0))
            .color(pal().text),
    );
    if !e.purpose.is_empty() {
        ui.label(RichText::new(e.purpose).font(theme::font(11.0)).color(dim));
    }
    if !e.note.is_empty() {
        ui.add_space(3.0);
        ui.label(
            RichText::new(e.note)
                .font(theme::font(11.0))
                .color(pal().accent),
        );
    }
}
