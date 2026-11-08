//! Small pieces the sample-player panels share.

use super::PanelCtx;
use crate::ui::theme::*;
use crate::ui::widgets;
use egui::{ComboBox, Id, Pos2, Rect, RichText, Sense, Slider, Stroke, Vec2};
use rlive_core::{ParamId, ParamKind};

/// Draws a sample's overview, `[min, max]` pairs across the rect.
pub fn draw_wave(ui: &mut egui::Ui, rect: Rect, overview: &[f32]) {
    let p = ui.painter();
    let n = overview.len() / 2;
    if n < 2 {
        return;
    }
    let mid = rect.center().y;
    let half = rect.height() * 0.5;
    for i in 0..n {
        let x = rect.left() + (i as f32 + 0.5) / n as f32 * rect.width();
        let (low, high) = (overview[i * 2], overview[i * 2 + 1]);
        p.line_segment(
            [
                Pos2::new(x, mid - high * half),
                Pos2::new(x, mid - low * half),
            ],
            Stroke::new(1.0, pal().accent),
        );
    }
}

/// A one-line control for a parameter: a menu, a toggle or a slider, labelled, in `width`.
pub fn param_control(ui: &mut egui::Ui, panel: &mut PanelCtx, id: u32, label: &str, width: f32) {
    let param_id = ParamId(id);
    let Some(info) = panel.param(param_id) else {
        return;
    };
    let value = panel.value(param_id).unwrap_or(info.default);
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(width * 0.42, 15.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(RichText::new(label).size(10.0).color(pal().text_dim));
                },
            );
            match &info.kind {
                ParamKind::Toggle => {
                    let on = value >= 0.5;
                    let resp = widgets::state_button(
                        ui,
                        Vec2::new(width * 0.5, 15.0),
                        on,
                        pal().accent,
                        if on { "On" } else { "Off" },
                    );
                    if resp.clicked() {
                        panel.set(param_id, if on { 0.0 } else { 1.0 });
                    }
                }
                ParamKind::Stepped { labels } if !labels.is_empty() => {
                    let current = (value.round() as usize).min(labels.len() - 1);
                    let mut next = current;
                    ComboBox::from_id_salt("choice")
                        .width(width * 0.5)
                        .selected_text(RichText::new(&labels[current]).size(10.0))
                        .show_ui(ui, |ui| {
                            for (i, l) in labels.iter().enumerate() {
                                ui.selectable_value(&mut next, i, l);
                            }
                        });
                    if next != current {
                        panel.set(param_id, next as f64);
                    }
                }
                _ => {
                    let mut norm = info.to_normalized(value);
                    let response = ui.add_sized(
                        Vec2::new(width * 0.5, 15.0),
                        Slider::new(&mut norm, 0.0..=1.0)
                            .show_value(false)
                            .trailing_fill(true),
                    );
                    if response.changed() {
                        panel.set(param_id, info.from_normalized(norm));
                    }
                    response.on_hover_text(info.format(value));
                }
            }
        });
    });
}

/// A tab strip. Returns which tab was clicked, if any.
pub fn tab_strip(ui: &mut egui::Ui, labels: &[&str], current: usize) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, label) in labels.iter().enumerate() {
            let on = i == current;
            let w = 8.0 + label.len() as f32 * 6.0;
            if widgets::state_button(ui, Vec2::new(w, 16.0), on, pal().accent, label).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// What a panel keeps between frames, in egui's memory.
pub fn memory<T: Default + Clone + Send + Sync + 'static>(ui: &egui::Ui, id: Id) -> T {
    ui.ctx().data(|d| d.get_temp(id).unwrap_or_default())
}

pub fn remember<T: Default + Clone + Send + Sync + 'static>(ui: &egui::Ui, id: Id, value: T) {
    ui.ctx().data_mut(|d| d.insert_temp(id, value));
}

/// A button the same height as everything else.
pub fn small_button(ui: &mut egui::Ui, label: &str, width: f32) -> egui::Response {
    widgets::state_button(ui, Vec2::new(width, 15.0), false, pal().accent, label)
}

/// An interaction area over `rect`.
pub fn drag_area(ui: &mut egui::Ui, rect: Rect, id: Id) -> egui::Response {
    ui.interact(rect, id, Sense::click_and_drag())
}
