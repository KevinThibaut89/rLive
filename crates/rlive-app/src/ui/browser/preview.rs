//! The Preview Tab (manual pp. 117-120).

use super::super::theme::*;
use super::super::widgets::{self, info};
use super::super::UiState;
use crate::session::Session;
use egui::{Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use std::path::PathBuf;

pub(super) const HEIGHT: f32 = 44.0;
const WAVE_POINTS: usize = 240;

fn selected_sample(state: &UiState) -> Option<PathBuf> {
    let row = state.browser.model.cursor_row()?;
    (row.item.kind == rlive_browser::ItemKind::Sample)
        .then(|| row.item.path.clone())
        .flatten()
}

/// Starts previewing the selection if there's anything to hear.
pub fn start(session: &mut Session, state: &mut UiState) {
    if let Some(path) = selected_sample(state) {
        let raw = state.browser.raw;
        session.preview_file(&path, raw);
        state.browser.previewing = Some(path);
    }
}

/// Called when the selection changes through a click or arrow key.
pub(super) fn selected(session: &mut Session, state: &mut UiState) {
    if state.browser.preview_on {
        start(session, state);
    }
}

/// Keeps the preview on the selected file while Preview is on.
pub(super) fn follow_selection(session: &mut Session, state: &mut UiState) {
    if !state.browser.preview_on {
        return;
    }
    let now = selected_sample(state);
    if now.is_some() && now != state.browser.previewing {
        start(session, state);
    }
}

fn peaks_of(sample: &rlive_core::SampleBuffer) -> Vec<f32> {
    let n = sample.len();
    if n == 0 {
        return vec![0.0; WAVE_POINTS];
    }
    (0..WAVE_POINTS)
        .map(|i| {
            let a = i * n / WAVE_POINTS;
            let b = ((i + 1) * n / WAVE_POINTS).max(a + 1).min(n);
            sample.left[a..b].iter().fold(0f32, |m, v| m.max(v.abs()))
        })
        .collect()
}

pub(super) fn show(ui: &mut egui::Ui, rect: Rect, session: &mut Session, state: &mut UiState) {
    ui.painter().rect_filled(rect, 0.0, pal().panel_dark);
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, pal().separator),
    );
    let controls = Rect::from_min_size(rect.min + Vec2::new(4.0, 3.0), Vec2::new(90.0, 18.0));
    let mut cu = ui.new_child(
        UiBuilder::new()
            .max_rect(controls)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    cu.spacing_mut().item_spacing.x = 3.0;
    let on = state.browser.preview_on;
    let r = widgets::state_button(&mut cu, Vec2::new(40.0, 16.0), on, pal().accent, "Prev");
    if info(
        r,
        "Preview — hear files as you select them (Shift+Enter or Right arrow to preview one)",
    )
    .clicked()
    {
        state.browser.preview_on = !on;
        if on {
            session.stop_preview();
            state.browser.previewing = None;
        } else {
            start(session, state);
        }
    }
    let raw = state.browser.raw;
    let r = widgets::state_button(&mut cu, Vec2::new(32.0, 16.0), raw, pal().accent, "Raw");
    if info(r, "Raw — play at the file's own tempo, once, straight away. Off, it starts on the next bar, in time, and loops").clicked() {
        state.browser.raw = !raw;
    }
    let wave_rect = Rect::from_min_max(
        Pos2::new(controls.right() + 4.0, rect.top() + 3.0),
        rect.max - Vec2::new(4.0, 3.0),
    );
    let showing = state
        .browser
        .previewing
        .clone()
        .or_else(|| selected_sample(state));
    if let Some(path) = showing {
        if state.browser.wave.as_ref().is_none_or(|(p, _)| *p != path) {
            if let Some(sample) = session.samples.get(&path) {
                state.browser.wave = Some((path.clone(), peaks_of(&sample)));
            } else if state.browser.preview_on {
                session.samples.request(&path);
            }
        }
    }
    ui.painter().rect_filled(wave_rect, 1.0, pal().field_bg);
    let mut bars = Vec::new();
    if let Some((_, peaks)) = &state.browser.wave {
        let w = wave_rect.width() / peaks.len().max(1) as f32;
        for (i, peak) in peaks.iter().enumerate() {
            let x = wave_rect.left() + i as f32 * w;
            let h = peak.min(1.0) * wave_rect.height() * 0.5;
            let y = wave_rect.center().y;
            bars.push(egui::Shape::line_segment(
                [Pos2::new(x, y - h), Pos2::new(x, y + h)],
                Stroke::new(1.0, pal().accent),
            ));
        }
    }
    ui.painter().add(egui::Shape::Vec(bars));
    let resp = ui.interact(
        wave_rect,
        ui.scope_id().with("preview_scrub"),
        Sense::click_and_drag(),
    );
    if let Some(at) = session.preview_position() {
        let x = wave_rect.left() + at * wave_rect.width();
        ui.painter().line_segment(
            [
                Pos2::new(x, wave_rect.top()),
                Pos2::new(x, wave_rect.bottom()),
            ],
            Stroke::new(1.0, shade(0xffffff)),
        );
        ui.ctx().request_repaint();
    }
    if !state.browser.raw && (resp.clicked() || resp.dragged()) {
        if let Some(pos) = resp.interact_pointer_pos() {
            let f = ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0);
            if session.preview_position().is_some() {
                session.scrub_preview(f as f64);
            } else if let Some(path) = selected_sample(state) {
                let raw = state.browser.raw;
                session.preview_file(&path, raw);
                session.scrub_preview(f as f64);
            }
        }
    }
    let hover = if state.browser.raw {
        "Preview Tab — scrubbing is off in Raw"
    } else {
        "Preview Tab — click to jump in the file while it plays"
    };
    let _ = info(resp, hover);
}
