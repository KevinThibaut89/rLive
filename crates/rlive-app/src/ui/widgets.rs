//! Custom Ableton-style widgets: knobs, faders, meters, flat buttons and the info view.

use super::icons;
use super::theme::{self, *};
use egui::{Align2, Color32, Id, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use rlive_core::{db_to_gain, gain_to_db, ParamInfo, ParamKind};

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

// ------------------------------------------------------------------ info view

/// What the Info View shows for a hovered control.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct InfoEntry {
    pub name: String,
    pub purpose: String,
    /// The user's own text, when the thing under the pointer has some.
    pub note: String,
}

impl InfoEntry {
    /// Splits "Name — what it does" into its two parts. Text with no dash is all name.
    pub fn parse(text: &str) -> Self {
        match text.split_once(" — ") {
            Some((name, purpose)) => Self {
                name: name.to_string(),
                purpose: purpose.to_string(),
                note: String::new(),
            },
            None => Self {
                name: text.to_string(),
                ..Self::default()
            },
        }
    }
}

fn info_current() -> Id {
    Id::unique("rlive_info_current")
}

fn info_shown() -> Id {
    Id::unique("rlive_info_shown")
}

/// Describe a control in the Info View while it is hovered or dragged. Write the text as
/// "Name — what it does".
pub fn info(resp: Response, text: impl Into<String>) -> Response {
    if resp.hovered() || resp.dragged() {
        let entry = InfoEntry::parse(&text.into());
        resp.ctx.data_mut(|d| d.insert_temp(info_current(), entry));
    }
    resp
}

/// Adds the user's own text to what [`info`] just recorded for this control.
pub fn info_note(resp: &Response, note: &str) {
    if !note.is_empty() && (resp.hovered() || resp.dragged()) {
        resp.ctx.data_mut(|d| {
            d.get_temp_mut_or_default::<InfoEntry>(info_current()).note = note.to_string();
        });
    }
}

/// Call once at the start of a frame: what was hovered last frame becomes visible.
pub fn info_begin_frame(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        let cur: Option<InfoEntry> = d.remove_temp(info_current());
        match cur {
            Some(t) => {
                d.insert_temp(info_shown(), t);
            }
            None => d.remove::<InfoEntry>(info_shown()),
        }
    });
}

/// What the Info View should show right now.
pub fn info_entry(ctx: &egui::Context) -> Option<InfoEntry> {
    ctx.data(|d| d.get_temp(info_shown()))
}

/// The same, on one line.
pub fn info_text(ctx: &egui::Context) -> Option<String> {
    let e = info_entry(ctx)?;
    Some(if e.purpose.is_empty() {
        e.name
    } else {
        format!("{} — {}", e.name, e.purpose)
    })
}

// ------------------------------------------------------------------ buttons

/// Flat square state button (track activator, solo, …).
pub fn state_button(ui: &mut Ui, size: Vec2, on: bool, on_color: Color32, label: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let fill = if on {
        on_color
    } else if resp.hovered() {
        pal().raised_hover
    } else {
        pal().raised
    };
    p.rect_filled(rect, 1.0, fill);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font(10.5),
        if on {
            theme::text_on(on_color)
        } else {
            pal().text
        },
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
        (pal().raised_hover, shade(0xf0f0f0))
    } else {
        (pal().raised, pal().text)
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
        shade(0xffffff)
    } else {
        pal().text_dim
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

/// How a knob is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KnobTone {
    Normal,
    Active,
    /// Greyed out: something else owns the value, or the user can't touch it.
    Disabled,
}

/// Paint a knob arc at `center`. `bipolar` arcs grow from the middle.
pub fn paint_knob(
    p: &egui::Painter,
    center: Pos2,
    radius: f32,
    norm: f32,
    bipolar: bool,
    tone: KnobTone,
) {
    p.add(Shape::line(
        arc_points(center, radius, 0.0, 1.0),
        Stroke::new(3.0, pal().knob_track),
    ));
    let (a, b) = if bipolar { (0.5, norm) } else { (0.0, norm) };
    let arc = match tone {
        KnobTone::Normal => pal().knob_arc,
        KnobTone::Active => shade(0xffc070),
        KnobTone::Disabled => pal().text_faint,
    };
    if (b - a).abs() > 0.001 {
        p.add(Shape::line(
            arc_points(center, radius, a.min(b), a.max(b)),
            Stroke::new(3.0, arc),
        ));
    }
    let needle = if tone == KnobTone::Disabled {
        pal().text_dim
    } else {
        shade(0xf0f0f0)
    };
    let ang = std::f32::consts::PI * 0.75 + std::f32::consts::PI * 1.5 * norm;
    p.line_segment(
        [
            center + Vec2::angled(ang) * (radius * 0.25),
            center + Vec2::angled(ang) * (radius + 1.0),
        ],
        Stroke::new(1.6, needle),
    );
}

/// Paint the stretch of a knob's travel that modulation is covering, outside the arc.
pub fn paint_mod_ring(p: &egui::Painter, center: Pos2, radius: f32, span: (f32, f32)) {
    p.add(Shape::line(
        arc_points(center, radius + 3.0, span.0, span.1),
        Stroke::new(1.5, pal().solo_blue),
    ));
}

/// The stretch of a control's travel that modulation is covering. `None` when it hasn't moved
/// the value enough to see.
pub fn mod_ring_span(base: f32, effective: f32) -> Option<(f32, f32)> {
    ((effective - base).abs() > 0.002).then(|| (base.min(effective), base.max(effective)))
}

/// What a device control knows beyond its value.
#[derive(Clone, Copy, Default)]
pub struct ControlState<'a> {
    /// The normalized value it is sounding at, which modulation can move off the control.
    pub effective: Option<f32>,
    /// A Macro owns the value, so the control is greyed out and can't be moved.
    pub disabled: bool,
    /// What the LED beside the control shows.
    pub led: Led,
    /// Set when the user chooses Re-Enable Automation. Present only while the control is
    /// overridden, which is the only time that makes sense.
    pub re_enable: Option<&'a std::cell::Cell<bool>>,
}

/// The LED beside a control: red for automated, gray once the user overrides it, blue for
/// modulated. Automation outranks modulation, which only moves what automation sets.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Led {
    #[default]
    Off,
    Automated,
    Overridden,
    Modulated,
}

impl Led {
    pub fn of(state: rlive_engine::CellState) -> Self {
        if state.overridden() {
            Self::Overridden
        } else if state.automated() {
            Self::Automated
        } else if state.modulated() {
            Self::Modulated
        } else {
            Self::Off
        }
    }

    pub fn paint(self, p: &egui::Painter, at: Pos2) {
        let color = match self {
            Self::Off => return,
            Self::Automated => pal().arm_red,
            Self::Overridden => shade(0x8a8a8a),
            Self::Modulated => pal().solo_blue,
        };
        p.circle_filled(at, 2.5, color);
    }
}

/// Offers Re-Enable Automation on a control's context menu, while it applies.
fn re_enable_menu(resp: &Response, state: ControlState) {
    if let Some(flag) = state.re_enable {
        resp.context_menu(|ui| {
            if ui.button("Re-Enable Automation").clicked() {
                flag.set(true);
                ui.close();
            }
        });
    }
}

/// Device parameter knob: name above, value below (Live style). The ring around the knob shows
/// where modulation has taken the value, away from the needle. A disabled knob is greyed out and
/// can't be moved. Drag vertically (Shift = fine), double-click resets. Returns the new plain
/// value.
pub fn knob(
    ui: &mut Ui,
    param: &ParamInfo,
    value: f64,
    display: &str,
    state: ControlState,
) -> Option<f64> {
    let size = Vec2::new(58.0, 62.0);
    let sense = if state.disabled {
        Sense::hover()
    } else {
        Sense::click_and_drag()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
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
    let name_color = if state.disabled {
        pal().text_faint
    } else if active {
        pal().text
    } else {
        shade(0xb8b8b8)
    };
    p.text(
        Pos2::new(rect.center().x, rect.top() + 6.0),
        Align2::CENTER_CENTER,
        name,
        font(10.0),
        name_color,
    );
    let center = Pos2::new(rect.center().x, rect.top() + 29.0);
    if toggle {
        let r = Rect::from_center_size(center, Vec2::new(30.0, 15.0));
        let on = value >= 0.5;
        let fill = match (on, state.disabled) {
            (true, false) => pal().accent,
            (true, true) => pal().text_faint,
            (false, _) => pal().raised,
        };
        p.rect_filled(r, 1.0, fill);
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            if on { "On" } else { "Off" },
            font(10.0),
            if on { pal().text_dark } else { pal().text_dim },
        );
    } else {
        let bipolar = param.min < 0.0 && param.max > 0.0;
        let tone = if state.disabled {
            KnobTone::Disabled
        } else if active {
            KnobTone::Active
        } else {
            KnobTone::Normal
        };
        paint_knob(&p, center, 11.0, norm as f32, bipolar, tone);
        // Hidden while dragging: the engine is a block behind the hand, not modulating.
        let span = state
            .effective
            .filter(|_| !resp.dragged())
            .and_then(|e| mod_ring_span(norm as f32, e));
        if let Some(span) = span {
            paint_mod_ring(&p, center, 11.0, span);
        }
    }
    let val = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.bottom() - 8.0),
        Vec2::new(54.0, 13.0),
    );
    if active && !toggle && !state.disabled {
        p.rect_filled(val, 1.0, pal().field_bg);
    }
    // Toggles already show their state on the button.
    let display = if toggle { "" } else { display };
    p.text(
        val.center(),
        Align2::CENTER_CENTER,
        display,
        font(10.0),
        if active && !state.disabled {
            pal().accent
        } else {
            pal().text_dim
        },
    );
    let hint = if state.disabled {
        "controlled by a Macro"
    } else {
        "drag to change, Shift = fine, double-click resets"
    };
    state.led.paint(&p, rect.left_top() + Vec2::new(4.0, 6.0));
    re_enable_menu(&resp, state);
    info(resp, format!("{}: {display}  —  {hint}", param.name));
    changed
}

/// Device parameter slider: name and value above a flat horizontal bar. Same gestures as a knob.
pub fn slider(
    ui: &mut Ui,
    param: &ParamInfo,
    value: f64,
    display: &str,
    state: ControlState,
) -> Option<f64> {
    let sense = if state.disabled {
        Sense::hover()
    } else {
        Sense::click_and_drag()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(116.0, 30.0), sense);
    let mut norm = param.to_normalized(value) as f32;
    let mut changed = None;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let delta = resp.drag_delta().x / if fine { 900.0 } else { rect.width() };
        norm = (norm + delta).clamp(0.0, 1.0);
        let v = param.from_normalized(norm as f64);
        if v != value {
            changed = Some(v);
        }
    }
    if resp.double_clicked() {
        changed = Some(param.default);
        norm = param.to_normalized(param.default) as f32;
    }
    let active = !state.disabled && (resp.hovered() || resp.dragged());
    let p = ui.painter();
    let (name_color, value_color) = if state.disabled {
        (pal().text_faint, pal().text_faint)
    } else if active {
        (pal().text, pal().accent)
    } else {
        (shade(0xb8b8b8), pal().text_dim)
    };
    p.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        &param.name,
        font(10.0),
        name_color,
    );
    p.text(
        rect.right_top(),
        Align2::RIGHT_TOP,
        display,
        font(10.0),
        value_color,
    );
    let bar = Rect::from_min_size(
        Pos2::new(rect.left(), rect.bottom() - 9.0),
        Vec2::new(rect.width(), 5.0),
    );
    p.rect_filled(bar, 0.0, pal().knob_track);
    let fill = Rect::from_min_size(bar.min, Vec2::new(bar.width() * norm, bar.height()));
    p.rect_filled(
        fill,
        0.0,
        if state.disabled {
            pal().text_faint
        } else if active {
            shade(0xffc070)
        } else {
            pal().knob_arc
        },
    );
    if let Some(span) = state
        .effective
        .filter(|_| !resp.dragged())
        .and_then(|e| mod_ring_span(norm, e))
    {
        let ring = Rect::from_min_max(
            Pos2::new(bar.left() + bar.width() * span.0, bar.bottom() + 1.0),
            Pos2::new(bar.left() + bar.width() * span.1, bar.bottom() + 3.0),
        );
        p.rect_filled(ring, 0.0, pal().solo_blue);
    }
    let hint = if state.disabled {
        "controlled by a Macro"
    } else {
        "drag to change, Shift = fine, double-click resets"
    };
    state.led.paint(p, rect.left_top() + Vec2::new(-5.0, 6.0));
    re_enable_menu(&resp, state);
    info(resp, format!("{}: {display}  —  {hint}", param.name));
    changed
}

/// Flat on/off button labelled with a parameter's name. Returns the new plain value.
pub fn toggle(ui: &mut Ui, param: &ParamInfo, value: f64, state: ControlState) -> Option<f64> {
    let mid = (param.min + param.max) / 2.0;
    let on = value > mid;
    let sense = if state.disabled {
        Sense::hover()
    } else {
        Sense::click()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(56.0, 18.0), sense);
    let fill = match (on, state.disabled) {
        (true, false) => pal().accent,
        (true, true) => pal().text_faint,
        (false, _) if resp.hovered() => pal().raised_hover,
        (false, _) => pal().raised,
    };
    let p = ui.painter();
    p.rect_filled(rect, 1.0, fill);
    let name: String = param.name.chars().take(9).collect();
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        name,
        font(10.0),
        if on { theme::text_on(fill) } else { pal().text },
    );
    let hint = if state.disabled {
        "controlled by a Macro"
    } else {
        "click to switch"
    };
    let word = if on { "On" } else { "Off" };
    state.led.paint(p, rect.left_top() + Vec2::new(4.0, 9.0));
    re_enable_menu(&resp, state);
    let resp = info(resp, format!("{}: {word}  —  {hint}", param.name));
    (resp.clicked() && !state.disabled).then_some(if on { param.min } else { param.max })
}

/// Drop-down for a stepped parameter, with its name above. Returns the new plain value.
pub fn menu(ui: &mut Ui, param: &ParamInfo, value: f64, state: ControlState) -> Option<f64> {
    let (labels, offset) = menu_entries(param);
    let selected = (value - offset).round().max(0.0) as usize;
    let current = labels.get(selected).map_or("?", String::as_str);
    let mut chosen = selected;
    let name_color = if state.disabled {
        pal().text_faint
    } else {
        shade(0xb8b8b8)
    };
    let outer = ui.add_enabled_ui(!state.disabled, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(
                egui::RichText::new(&param.name)
                    .font(font(10.0))
                    .color(name_color),
            );
            egui::ComboBox::from_id_salt(param.id)
                .width(92.0)
                .selected_text(egui::RichText::new(current).font(font(10.5)))
                .show_ui(ui, |ui| {
                    for (i, label) in labels.iter().enumerate() {
                        ui.selectable_value(&mut chosen, i, label);
                    }
                });
        });
    });
    let rect = outer.response.rect;
    state
        .led
        .paint(ui.painter(), rect.left_top() + Vec2::new(-5.0, 6.0));
    re_enable_menu(&outer.response, state);
    (chosen != selected && !state.disabled).then_some(chosen as f64 + offset)
}

/// The entries of a menu, and what to add to an entry's position to get its plain value. A
/// parameter with no labels of its own is listed by value.
fn menu_entries(param: &ParamInfo) -> (Vec<String>, f64) {
    if let ParamKind::Stepped { labels } = &param.kind {
        return (labels.clone(), 0.0);
    }
    let (low, high) = (param.min.round(), param.max.round());
    let steps = (high - low).max(0.0) as usize;
    (
        (0..=steps).map(|i| param.format(low + i as f64)).collect(),
        low,
    )
}

/// Paint a scope's view data: samples in -1..1, spectrum bins in 0..1, or left/right pairs.
pub fn paint_scope(p: &egui::Painter, rect: Rect, kind: rlive_core::ScopeKind, data: &[f32]) {
    p.rect_filled(rect, 0.0, shade(0x121212));
    p.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
    let inner = rect.shrink(2.0);
    let points: Vec<Pos2> = match kind {
        rlive_core::ScopeKind::Waveform => data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = inner.left() + inner.width() * i as f32 / data.len().max(2) as f32;
                let y = inner.center().y - v.clamp(-1.0, 1.0) * inner.height() * 0.5;
                Pos2::new(x, y)
            })
            .collect(),
        rlive_core::ScopeKind::Spectrum => data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = inner.left() + inner.width() * i as f32 / data.len().max(2) as f32;
                Pos2::new(x, inner.bottom() - v.clamp(0.0, 1.0) * inner.height())
            })
            .collect(),
        rlive_core::ScopeKind::Vectorscope => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                inner.center()
                    + Vec2::new(
                        pair[0].clamp(-1.0, 1.0) * inner.width() * 0.5,
                        -pair[1].clamp(-1.0, 1.0) * inner.height() * 0.5,
                    )
            })
            .collect(),
    };
    let clipped = p.with_clip_rect(rect);
    if points.len() > 1 && kind != rlive_core::ScopeKind::Vectorscope {
        clipped.add(Shape::line(points, Stroke::new(1.0, pal().accent)));
    } else {
        for point in points {
            clipped.circle_filled(point, 0.75, pal().accent);
        }
    }
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
        if active {
            KnobTone::Active
        } else {
            KnobTone::Normal
        },
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
    p.rect_filled(rect, 0.0, shade(0x121212));
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
        p.rect_filled(green, 0.0, shade(0x46d160));
        if full.top() < y12 {
            p.rect_filled(
                Rect::from_min_max(Pos2::new(x0, full.top().max(y0)), Pos2::new(x0 + w, y12)),
                0.0,
                shade(0xe5d14a),
            );
        }
        if full.top() < y0 {
            p.rect_filled(
                Rect::from_min_max(full.min, Pos2::new(x0 + w, y0)),
                0.0,
                shade(0xff4a3d),
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
    p.rect_filled(track, 1.0, shade(0x141414));
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
        Stroke::new(1.0, pal().text_faint),
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
        if active {
            shade(0xffffff)
        } else {
            shade(0xcfcfcf)
        },
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

/// The small dot under a fader's thumb that says where modulation has actually put the volume
/// (manual p. 539).
pub fn paint_fader_dot(p: &egui::Painter, rect: Rect, db: f32) {
    let x = rect.left() + (rect.width() - 10.0) / 2.0;
    let height = rect.height() - 6.0;
    let y = rect.top() + 3.0 + height * (1.0 - db_to_pos(db));
    p.circle_filled(Pos2::new(x + 8.0, y), 2.0, pal().solo_blue);
}

/// The lowest dot of a MIDI track's meter, blue while per-note expression passes (manual p. 332).
pub fn paint_expression_dot(p: &egui::Painter, rect: Rect, lit: f32) {
    if lit > 0.01 {
        let at = Pos2::new(rect.right() - 5.0, rect.bottom() - 3.0);
        p.circle_filled(at, 2.0, pal().solo_blue.gamma_multiply(lit.min(1.0)));
    }
}

/// Decaying peak display values (UI side).
#[derive(Default, Clone, Copy)]
pub struct MeterState {
    pub l: f32,
    pub r: f32,
    /// The engine's count of blocks with per-note expression, last time it was read.
    expr_seen: u32,
    /// How lit the meter's blue dot is, 0 to 1.
    expr_lit: f32,
}

impl MeterState {
    /// Lights the blue dot whenever per-note expression has gone by since the last look.
    pub fn update_expression(&mut self, pulses: u32, dt: f32) {
        if pulses != self.expr_seen {
            self.expr_seen = pulses;
            self.expr_lit = 1.0;
        } else {
            self.expr_lit *= (-dt * 7.0).exp();
        }
    }

    pub fn expression_lit(&self) -> f32 {
        self.expr_lit
    }

    pub fn update(&mut self, peaks: (f32, f32), dt: f32) {
        let decay = (-dt * 7.0).exp();
        self.l = peaks.0.max(self.l * decay);
        self.r = peaks.1.max(self.r * decay);
    }

    pub fn get(&self) -> (f32, f32) {
        (self.l, self.r)
    }

    pub fn active(&self) -> bool {
        self.l > 0.0005 || self.r > 0.0005 || self.expr_lit > 0.01
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
            .color(pal().text_dim),
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
    fn the_ring_shows_only_a_visible_difference_and_runs_low_to_high() {
        assert_eq!(mod_ring_span(0.5, 0.5), None);
        assert_eq!(mod_ring_span(0.5, 0.501), None);
        assert_eq!(mod_ring_span(0.25, 0.75), Some((0.25, 0.75)));
        assert_eq!(mod_ring_span(0.75, 0.25), Some((0.25, 0.75)));
    }

    #[test]
    fn fader_curve_roundtrip() {
        for db in [-60.0f32, -24.0, -6.0, 0.0, 6.0] {
            assert!((pos_to_db(db_to_pos(db)) - db).abs() < 0.01, "{db}");
        }
        assert_eq!(db_to_pos(-96.0), 0.0);
        assert!((db_to_pos(6.0) - 1.0).abs() < 1e-6);
    }
}
