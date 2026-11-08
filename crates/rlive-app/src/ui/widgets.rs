//! Custom widgets: knobs, meters, small icon buttons.

use egui::{
    Align2, Color32, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};
use rlive_core::{ParamInfo, ParamKind};

pub const ACCENT: Color32 = Color32::from_rgb(255, 150, 30);
pub const PLAY_GREEN: Color32 = Color32::from_rgb(90, 220, 90);
pub const PANEL_BG: Color32 = Color32::from_rgb(38, 38, 40);
pub const SLOT_BG: Color32 = Color32::from_rgb(52, 52, 56);
pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 150, 155);

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A rotary knob bound to a parameter. Drag vertically (shift = fine), double-click resets.
/// Returns the new plain value when it changed.
pub fn knob(ui: &mut Ui, param: &ParamInfo, value: f64, display: &str) -> Option<f64> {
    let size = Vec2::new(52.0, 62.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let mut norm = param.to_normalized(value);
    let mut changed = None;

    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let delta = -resp.drag_delta().y as f64 / if fine { 800.0 } else { 160.0 };
        norm = (norm + delta).clamp(0.0, 1.0);
        let v = param.from_normalized(norm);
        if v != value {
            changed = Some(v);
        }
    }
    if resp.double_clicked() {
        changed = Some(param.default);
        norm = param.to_normalized(param.default);
    }
    if matches!(param.kind, ParamKind::Toggle) && resp.clicked() {
        let v = if value >= 0.5 { 0.0 } else { 1.0 };
        changed = Some(v);
        norm = v;
    }

    let painter = ui.painter_at(rect);
    let center = Pos2::new(rect.center().x, rect.top() + 20.0);
    let radius = 14.0;
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        let n = 24;
        (0..=n)
            .map(|i| {
                let a = start + sweep * (from + (to - from) * i as f32 / n as f32);
                center + Vec2::angled(a) * radius
            })
            .collect()
    };
    let hovered = resp.hovered() || resp.dragged();
    painter.circle_filled(
        center,
        radius - 3.0,
        Color32::from_gray(if hovered { 70 } else { 58 }),
    );
    painter.add(Shape::line(
        arc(0.0, 1.0),
        Stroke::new(3.0, Color32::from_gray(30)),
    ));
    if matches!(param.kind, ParamKind::Toggle) {
        let col = if value >= 0.5 {
            ACCENT
        } else {
            Color32::from_gray(80)
        };
        painter.circle_filled(center, 5.0, col);
    } else {
        painter.add(Shape::line(arc(0.0, norm as f32), Stroke::new(3.0, ACCENT)));
        let a = start + sweep * norm as f32;
        painter.line_segment(
            [center, center + Vec2::angled(a) * (radius - 4.0)],
            Stroke::new(2.0, Color32::WHITE),
        );
    }
    let name: String = param.name.chars().take(10).collect();
    painter.text(
        Pos2::new(rect.center().x, rect.top() + 40.0),
        Align2::CENTER_CENTER,
        name,
        FontId::proportional(10.0),
        Color32::from_gray(200),
    );
    painter.text(
        Pos2::new(rect.center().x, rect.top() + 53.0),
        Align2::CENTER_CENTER,
        display,
        FontId::proportional(9.5),
        if hovered { ACCENT } else { TEXT_DIM },
    );
    resp.on_hover_text(format!(
        "{}: {display}\nDrag to change (Shift = fine), double-click to reset",
        param.name
    ));
    changed
}

/// Vertical stereo peak meter. `peaks` are linear (0..1+).
pub fn meter(ui: &mut Ui, size: Vec2, peaks: (f32, f32)) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 1.0, Color32::from_gray(20));
    let w = (rect.width() - 1.0) / 2.0;
    for (i, p) in [peaks.0, peaks.1].into_iter().enumerate() {
        let db = rlive_core::gain_to_db(p).max(-60.0);
        let frac = ((db + 60.0) / 66.0).clamp(0.0, 1.0);
        let x0 = rect.left() + i as f32 * (w + 1.0);
        let bar = Rect::from_min_max(
            Pos2::new(x0, rect.bottom() - rect.height() * frac),
            Pos2::new(x0 + w, rect.bottom()),
        );
        let color = if p >= 1.0 {
            Color32::from_rgb(240, 60, 50)
        } else if db > -6.0 {
            Color32::from_rgb(230, 200, 60)
        } else {
            Color32::from_rgb(80, 210, 100)
        };
        painter.rect_filled(bar, 0.0, color);
    }
    resp
}

/// Small square toggle button with a letter (M / S / ● ...).
pub fn toggle_box(
    ui: &mut Ui,
    on: bool,
    label: &str,
    on_color: Color32,
    tooltip: &str,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(22.0, 16.0), Sense::click());
    let painter = ui.painter_at(rect);
    let fill = if on {
        on_color
    } else {
        Color32::from_gray(if resp.hovered() { 75 } else { 60 })
    };
    painter.rect_filled(rect, 2.0, fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.0),
        if on {
            Color32::BLACK
        } else {
            Color32::from_gray(210)
        },
    );
    resp.on_hover_text(tooltip)
}

/// Draw a "play" triangle inside `rect`.
pub fn play_triangle(painter: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let s = rect.height().min(rect.width()) * 0.32;
    painter.add(Shape::convex_polygon(
        vec![
            Pos2::new(c.x - s * 0.8, c.y - s),
            Pos2::new(c.x + s, c.y),
            Pos2::new(c.x - s * 0.8, c.y + s),
        ],
        color,
        Stroke::NONE,
    ));
}

pub fn stop_square(painter: &egui::Painter, rect: Rect, color: Color32) {
    let s = rect.height().min(rect.width()) * 0.25;
    painter.rect_filled(
        Rect::from_center_size(rect.center(), Vec2::splat(s * 2.0)),
        0.0,
        color,
    );
}

pub fn outline(painter: &egui::Painter, rect: Rect, color: Color32) {
    painter.rect_stroke(rect, 2.0, Stroke::new(1.5, color), StrokeKind::Inside);
}

/// Decaying peak display values (UI side).
#[derive(Default, Clone, Copy)]
pub struct MeterState {
    pub l: f32,
    pub r: f32,
}

impl MeterState {
    pub fn update(&mut self, peaks: (f32, f32), dt: f32) {
        let decay = (-dt * 8.0).exp();
        self.l = peaks.0.max(self.l * decay);
        self.r = peaks.1.max(self.r * decay);
    }

    pub fn get(&self) -> (f32, f32) {
        (self.l, self.r)
    }
}
