//! Custom Ableton-style widgets: knobs, faders, meters, flat buttons and the info view.

use super::icons;
use super::theme::{self, *};
use egui::{Align2, Color32, Id, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use rlive_core::{db_to_gain, gain_to_db, ParamInfo, ParamKind};

pub use theme::{ACCENT, PLAY_GREEN, TEXT_DIM};

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

// ------------------------------------------------------------------ info view

fn info_current() -> Id {
    Id::unique("rlive_info_current")
}

fn info_shown() -> Id {
    Id::unique("rlive_info_shown")
}

/// Describe a control in the Info View (bottom-left) while it is hovered or dragged.
pub fn info(resp: Response, text: impl Into<String>) -> Response {
    if resp.hovered() || resp.dragged() {
        let text = text.into();
        resp.ctx.data_mut(|d| d.insert_temp(info_current(), text));
    }
    resp
}

/// Call once at the start of a frame: what was hovered last frame becomes visible.
pub fn info_begin_frame(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        let cur: Option<String> = d.remove_temp(info_current());
        match cur {
            Some(t) => {
                d.insert_temp(info_shown(), t);
            }
            None => d.remove::<String>(info_shown()),
        }
    });
}

pub fn info_text(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp(info_shown()))
}

// ------------------------------------------------------------------ buttons

/// Flat square state button (track activator, solo, …).
pub fn state_button(ui: &mut Ui, size: Vec2, on: bool, on_color: Color32, label: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let fill = if on {
        on_color
    } else if resp.hovered() {
        RAISED_HOVER
    } else {
        RAISED
    };
    p.rect_filled(rect, 1.0, fill);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font(10.5),
        if on { theme::text_on(on_color) } else { TEXT },
    );
    resp
}

/// Square button with a vector icon.
pub fn icon_button(
    ui: &mut Ui,
    size: Vec2,
    on: bool,
    on_color: Color32,
    draw: impl FnOnce(&egui::Painter, Rect, Color32),
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let (fill, fg) = if on {
        (on_color, theme::text_on(on_color))
    } else if resp.hovered() {
        (RAISED_HOVER, hex(0xf0f0f0))
    } else {
        (RAISED, TEXT)
    };
    p.rect_filled(rect, 1.0, fill);
    draw(p, rect, fg);
    resp
}

/// Icon without a background (title-bar style).
pub fn bare_icon(
    ui: &mut Ui,
    size: Vec2,
    draw: impl FnOnce(&egui::Painter, Rect, Color32),
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let c = if resp.hovered() {
        hex(0xffffff)
    } else {
        TEXT_DIM
    };
    draw(ui.painter(), rect, c);
    resp
}

// ------------------------------------------------------------------ knobs

fn arc_points(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let n = ((to - from).abs() * 28.0).ceil().max(2.0) as usize;
    (0..=n)
        .map(|i| {
            let a = start + sweep * (from + (to - from) * i as f32 / n as f32);
            center + Vec2::angled(a) * radius
        })
        .collect()
}

/// Paint a knob arc at `center`. `bipolar` arcs grow from the middle.
pub fn paint_knob(
    p: &egui::Painter,
    center: Pos2,
    radius: f32,
    norm: f32,
    bipolar: bool,
    active: bool,
) {
    p.add(Shape::line(
        arc_points(center, radius, 0.0, 1.0),
        Stroke::new(3.0, KNOB_TRACK),
    ));
    let (a, b) = if bipolar { (0.5, norm) } else { (0.0, norm) };
    if (b - a).abs() > 0.001 {
        p.add(Shape::line(
            arc_points(center, radius, a.min(b), a.max(b)),
            Stroke::new(3.0, if active { hex(0xffc070) } else { KNOB_ARC }),
        ));
    }
    let ang = std::f32::consts::PI * 0.75 + std::f32::consts::PI * 1.5 * norm;
    p.line_segment(
        [
            center + Vec2::angled(ang) * (radius * 0.25),
            center + Vec2::angled(ang) * (radius + 1.0),
        ],
        Stroke::new(1.6, hex(0xf0f0f0)),
    );
}

/// Device parameter knob: name above, value below (Live style). `effective` is the normalized
/// value the parameter is sounding at, which modulation can move away from the knob.
/// Drag vertically (Shift = fine), double-click resets. Returns the new plain value.
pub fn knob(
    ui: &mut Ui,
    param: &ParamInfo,
    value: f64,
    display: &str,
    effective: Option<f32>,
) -> Option<f64> {
    let size = Vec2::new(58.0, 62.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let mut norm = param.to_normalized(value);
    let mut changed = None;
    let toggle = matches!(param.kind, ParamKind::Toggle);
    if resp.dragged() && !toggle {
        let fine = ui.input(|i| i.modifiers.shift);
        let delta = -resp.drag_delta().y as f64 / if fine { 900.0 } else { 180.0 };
        norm = (norm + delta).clamp(0.0, 1.0);
        let v = param.from_normalized(norm);
        if v != value {
            changed = Some(v);
        }
    }
    if resp.double_clicked() && !toggle {
        changed = Some(param.default);
        norm = param.to_normalized(param.default);
    }
    if toggle && resp.clicked() {
        let v = if value >= 0.5 { 0.0 } else { 1.0 };
        changed = Some(v);
        norm = v;
    }
    let active = resp.hovered() || resp.dragged();
    let p = ui.painter_at(rect.expand(2.0));
    let name: String = param.name.chars().take(12).collect();
    p.text(
        Pos2::new(rect.center().x, rect.top() + 6.0),
        Align2::CENTER_CENTER,
        name,
        font(10.0),
        if active { TEXT } else { hex(0xb8b8b8) },
    );
    let center = Pos2::new(rect.center().x, rect.top() + 29.0);
    if toggle {
        let r = Rect::from_center_size(center, Vec2::new(30.0, 15.0));
        let on = value >= 0.5;
        p.rect_filled(r, 1.0, if on { ACCENT } else { RAISED });
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            if on { "On" } else { "Off" },
            font(10.0),
            if on { TEXT_DARK } else { TEXT_DIM },
        );
    } else {
        let bipolar = param.min < 0.0 && param.max > 0.0;
        paint_knob(&p, center, 11.0, norm as f32, bipolar, active);
        if let Some(effective) = effective.filter(|e| (e - norm as f32).abs() > 0.002) {
            // Where the value really is, when modulation has moved it off the knob.
            let ang = std::f32::consts::PI * 0.75 + std::f32::consts::PI * 1.5 * effective;
            p.circle_filled(center + Vec2::angled(ang) * 11.0, 2.0, ACCENT);
        }
    }
    let val = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.bottom() - 8.0),
        Vec2::new(54.0, 13.0),
    );
    if active && !toggle {
        p.rect_filled(val, 1.0, FIELD_BG);
    }
    // Toggles already show their state on the button.
    let display = if toggle { "" } else { display };
    p.text(
        val.center(),
        Align2::CENTER_CENTER,
        display,
        font(10.0),
        if active { ACCENT } else { TEXT_DIM },
    );
    info(
        resp,
        format!(
            "{}: {display}  —  drag to change, Shift = fine, double-click resets",
            param.name
        ),
    );
    changed
}

/// Small bipolar pan knob (-1..1). Returns the new value.
pub fn pan_knob(ui: &mut Ui, pan: f32) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 26.0), Sense::click_and_drag());
    let mut v = pan;
    let mut changed = None;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        v = (v - resp.drag_delta().y / if fine { 400.0 } else { 80.0 }).clamp(-1.0, 1.0);
        changed = Some(v);
    }
    if resp.double_clicked() {
        v = 0.0;
        changed = Some(0.0);
    }
    let active = resp.hovered() || resp.dragged();
    paint_knob(
        ui.painter(),
        rect.center(),
        9.5,
        (v + 1.0) / 2.0,
        true,
        active,
    );
    let label = if v.abs() < 0.005 {
        "C".to_string()
    } else if v < 0.0 {
        format!("{:.0}L", -v * 50.0)
    } else {
        format!("{:.0}R", v * 50.0)
    };
    info(
        resp,
        format!("Pan: {label}  —  drag, double-click to center"),
    );
    changed
}

// ------------------------------------------------------------------ faders & meters

const FADER_MAX_DB: f32 = 6.0;

fn db_to_pos(db: f32) -> f32 {
    if db <= -70.0 {
        return 0.0;
    }
    (db_to_gain(db).powf(0.25) / db_to_gain(FADER_MAX_DB).powf(0.25)).clamp(0.0, 1.0)
}

fn pos_to_db(pos: f32) -> f32 {
    if pos <= 0.001 {
        return -96.0;
    }
    gain_to_db((pos * db_to_gain(FADER_MAX_DB).powf(0.25)).powi(4)).min(FADER_MAX_DB)
}

/// Paint a stereo peak meter into `rect`.
pub fn paint_meter(p: &egui::Painter, rect: Rect, peaks: (f32, f32)) {
    p.rect_filled(rect, 0.0, hex(0x121212));
    let gap = 1.0;
    let w = ((rect.width() - gap) / 2.0).max(1.0);
    for (i, peak) in [peaks.0, peaks.1].into_iter().enumerate() {
        let pos = db_to_pos(gain_to_db(peak));
        let x0 = rect.left() + i as f32 * (w + gap);
        let top = rect.bottom() - rect.height() * pos;
        let full = Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x0 + w, rect.bottom()));
        // Green body, yellow above -12 dB, red above 0 dB.
        let y12 = rect.bottom() - rect.height() * db_to_pos(-12.0);
        let y0 = rect.bottom() - rect.height() * db_to_pos(0.0);
        let green = Rect::from_min_max(Pos2::new(x0, full.top().max(y12)), full.max);
        p.rect_filled(green, 0.0, hex(0x46d160));
        if full.top() < y12 {
            p.rect_filled(
                Rect::from_min_max(Pos2::new(x0, full.top().max(y0)), Pos2::new(x0 + w, y12)),
                0.0,
                hex(0xe5d14a),
            );
        }
        if full.top() < y0 {
            p.rect_filled(
                Rect::from_min_max(full.min, Pos2::new(x0 + w, y0)),
                0.0,
                hex(0xff4a3d),
            );
        }
    }
}

/// Volume fader with a meter next to it (Live mixer style). Returns new dB.
pub fn fader(ui: &mut Ui, size: Vec2, db: f32, peaks: (f32, f32), color: Color32) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let meter = Rect::from_min_max(Pos2::new(rect.right() - 9.0, rect.top()), rect.max);
    let track = Rect::from_center_size(
        Pos2::new(rect.left() + (rect.width() - 10.0) / 2.0, rect.center().y),
        Vec2::new(3.0, rect.height() - 6.0),
    );
    let mut pos = db_to_pos(db);
    let mut changed = None;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        pos = (pos - resp.drag_delta().y / (track.height() * if fine { 6.0 } else { 1.0 }))
            .clamp(0.0, 1.0);
        changed = Some(pos_to_db(pos));
    }
    if resp.double_clicked() {
        pos = db_to_pos(0.0);
        changed = Some(0.0);
    }
    let p = ui.painter();
    p.rect_filled(track, 1.0, hex(0x141414));
    let y = track.bottom() - track.height() * pos;
    p.rect_filled(
        Rect::from_min_max(Pos2::new(track.left(), y), track.max),
        1.0,
        color.gamma_multiply(0.85),
    );
    // 0 dB tick
    let y0 = track.bottom() - track.height() * db_to_pos(0.0);
    p.line_segment(
        [
            Pos2::new(track.left() - 4.0, y0),
            Pos2::new(track.right() + 4.0, y0),
        ],
        Stroke::new(1.0, TEXT_FAINT),
    );
    // Handle: a triangle pointing at the track (like Live).
    let hx = track.right() + 1.0;
    let active = resp.hovered() || resp.dragged();
    p.add(Shape::convex_polygon(
        vec![
            Pos2::new(hx, y),
            Pos2::new(hx + 7.0, y - 5.0),
            Pos2::new(hx + 7.0, y + 5.0),
        ],
        if active { hex(0xffffff) } else { hex(0xcfcfcf) },
        Stroke::NONE,
    ));
    paint_meter(p, meter, peaks);
    let text = if db <= -70.0 {
        "-inf dB".to_string()
    } else {
        format!("{db:.1} dB")
    };
    info(
        resp,
        format!("Volume: {text}  —  drag, Shift = fine, double-click = 0 dB"),
    );
    changed
}

/// Decaying peak display values (UI side).
#[derive(Default, Clone, Copy)]
pub struct MeterState {
    pub l: f32,
    pub r: f32,
}

impl MeterState {
    pub fn update(&mut self, peaks: (f32, f32), dt: f32) {
        let decay = (-dt * 7.0).exp();
        self.l = peaks.0.max(self.l * decay);
        self.r = peaks.1.max(self.r * decay);
    }

    pub fn get(&self) -> (f32, f32) {
        (self.l, self.r)
    }

    pub fn active(&self) -> bool {
        self.l > 0.0005 || self.r > 0.0005
    }
}

// ------------------------------------------------------------------ misc

pub fn outline(p: &egui::Painter, rect: Rect, color: Color32) {
    p.rect_stroke(rect, 1.0, Stroke::new(1.0, color), StrokeKind::Inside);
}

/// Section header text (small caps style).
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(font_bold(11.0))
            .color(TEXT_DIM),
    );
}

pub fn play_triangle(p: &egui::Painter, rect: Rect, color: Color32) {
    icons::play(p, rect, color);
}

pub fn stop_square(p: &egui::Painter, rect: Rect, color: Color32) {
    icons::stop(p, rect, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_curve_roundtrip() {
        for db in [-60.0f32, -24.0, -6.0, 0.0, 6.0] {
            assert!((pos_to_db(db_to_pos(db)) - db).abs() < 0.01, "{db}");
        }
        assert_eq!(db_to_pos(-96.0), 0.0);
        assert!((db_to_pos(6.0) - 1.0).abs() < 1e-6);
    }
}
