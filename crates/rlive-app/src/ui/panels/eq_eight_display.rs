//! EQ Eight's display: the curves of the two channels over the output spectrum, a dot per band,
//! and a strip of band buttons below.
//!
//! Drag a dot to change its frequency and gain; hold Alt to change its Q instead, as vertical
//! dragging does for the low cut, notch and high cut. Drag in an empty part of the plot to
//! select several dots and move them together. In Audition mode, hold a dot to hear that filter.

use super::{plot, PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::{ParamId, SCOPE_VALUES};
use rlive_devices::devices::b4::eq_eight as eq;

pub const PANEL: PanelDef = PanelDef {
    name: "eq_eight_display",
    size: Vec2::new(430.0, 150.0),
    views: eq::VIEW_LEN,
    draw,
};

const RANGE_DB: f32 = 18.0;
const STRIP_H: f32 = 28.0;
const DOT_RADIUS: f32 = 5.0;
/// How close the pointer must be to a dot to take it, in points.
const GRAB_RADIUS: f32 = 10.0;

#[derive(Clone, Copy)]
struct DragStart {
    /// The band's values when the drag started.
    freq: f64,
    gain: f64,
    q: f64,
}

#[derive(Clone, Default)]
struct State {
    selected: [bool; eq::BANDS],
    /// While a drag is under way, what it started from, for the bands it moves.
    drag: Option<[Option<DragStart>; eq::BANDS]>,
    press: Option<Pos2>,
    box_select: bool,
    auditioning: bool,
}

/// Where a band's dot is.
fn dot(rect: Rect, setting: &eq::BandSetting, gain_db: f64) -> Pos2 {
    let y = if setting.response.has_gain() {
        plot::db_to_y(rect, gain_db as f32, RANGE_DB)
    } else {
        rect.center().y
    };
    Pos2::new(plot::hz_to_x(rect, setting.freq), y)
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let full = ui.max_rect();
    let plot_rect = Rect::from_min_max(
        full.min,
        Pos2::new(full.right(), full.bottom() - STRIP_H - 2.0),
    );
    let strip = Rect::from_min_max(Pos2::new(full.left(), plot_rect.bottom() + 2.0), full.max);

    let id = ui.scope_id().with(("eq_eight_state", panel.device.id));
    let mut state: State = ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or_default());

    // A copy, so changes can be queued while it's read.
    let values: [f64; eq::PARAM_COUNT] =
        std::array::from_fn(|id| panel.value(ParamId(id as u32)).unwrap_or(0.0));
    let value = |id: u32| values[id as usize];
    let mode = value(eq::MODE).round() as usize;
    let edit = if mode == eq::MODE_STEREO {
        0
    } else {
        value(eq::EDIT).round() as usize
    };
    let adaptive = value(eq::ADAPTIVE_Q) >= 0.5;
    let audition = value(eq::AUDITION) >= 0.5;
    let settings: [eq::BandSetting; eq::BANDS] =
        std::array::from_fn(|b| eq::BandSetting::read(value, edit, b, adaptive));
    let other: Option<[eq::BandSetting; eq::BANDS]> = (mode != eq::MODE_STEREO)
        .then(|| std::array::from_fn(|b| eq::BandSetting::read(value, 1 - edit, b, adaptive)));

    let response = ui.interact(
        plot_rect,
        ui.scope_id().with(("eq_eight_plot", panel.device.id)),
        Sense::click_and_drag(),
    );
    let modifiers = ui.input(|i| i.modifiers);

    // Taking a dot happens on the press, so Audition works without moving it.
    let pressed = response.hovered() && ui.input(|i| i.pointer.primary_pressed());
    if pressed {
        if let Some(pos) = response.hover_pos() {
            let nearest = (0..eq::BANDS)
                .filter(|b| settings[*b].on)
                .map(|b| {
                    let gain = value(eq::band_param(edit, b, eq::B_GAIN));
                    (b, dot(plot_rect, &settings[b], gain).distance(pos))
                })
                .filter(|(_, d)| *d < GRAB_RADIUS)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            state.press = Some(pos);
            if let Some((band, _)) = nearest {
                if !state.selected[band] {
                    state.selected = [false; eq::BANDS];
                    state.selected[band] = true;
                }
                state.box_select = false;
                state.drag = Some(std::array::from_fn(|b| {
                    state.selected[b].then(|| {
                        let field = |f| value(eq::band_param(edit, b, f));
                        DragStart {
                            freq: field(eq::B_FREQ),
                            gain: field(eq::B_GAIN),
                            q: field(eq::B_Q),
                        }
                    })
                }));
                if audition {
                    panel.set(ParamId(eq::AUDITION_BAND), (band + 1) as f64);
                    state.auditioning = true;
                }
            } else {
                state.box_select = true;
            }
        }
    }

    if response.dragged() {
        if let (Some(start), Some(now)) = (state.press, response.interact_pointer_pos()) {
            if state.box_select {
                let area = Rect::from_two_pos(start, now);
                for (b, selected) in state.selected.iter_mut().enumerate() {
                    let gain = value(eq::band_param(edit, b, eq::B_GAIN));
                    *selected = settings[b].on && area.contains(dot(plot_rect, &settings[b], gain));
                }
            } else if let Some(drag) = &state.drag {
                let octaves = ((now.x - start.x) / plot_rect.width()) as f64
                    * (plot::MAX_HZ / plot::MIN_HZ).log2() as f64;
                let db = -((now.y - start.y) / plot_rect.height()) as f64 * (2.0 * RANGE_DB) as f64;
                for (b, band_start) in drag.iter().enumerate() {
                    let Some(s) = band_start else { continue };
                    let param = |f| ParamId(eq::band_param(edit, b, f));
                    panel.set(
                        param(eq::B_FREQ),
                        (s.freq * 2f64.powf(octaves)).clamp(20.0, 22_000.0),
                    );
                    if modifiers.alt || !settings[b].response.has_gain() {
                        panel.set(param(eq::B_Q), (s.q * 2f64.powf(db / 6.0)).clamp(0.1, 18.0));
                    } else {
                        panel.set(param(eq::B_GAIN), (s.gain + db).clamp(-15.0, 15.0));
                    }
                }
            }
        }
    }

    if ui.input(|i| i.pointer.primary_released()) {
        state.drag = None;
        state.press = None;
        state.box_select = false;
        if state.auditioning {
            panel.set(ParamId(eq::AUDITION_BAND), 0.0);
            state.auditioning = false;
        }
    }

    let mut spectrum = [0.0f32; SCOPE_VALUES];
    let mut sample_rate = [48_000.0f32];
    let read = panel.view.is_some_and(|v| {
        v.read(eq::SPECTRUM, &mut spectrum) && v.read(eq::SAMPLE_RATE, &mut sample_rate)
    });
    let sr = if sample_rate[0] > 0.0 {
        sample_rate[0]
    } else {
        48_000.0
    };

    let total = |bands: &[eq::BandSetting; eq::BANDS], hz: f32| -> f32 {
        bands.iter().map(|b| b.magnitude_db(hz, sr)).sum()
    };
    let p = ui.painter();
    plot::background(p, plot_rect, RANGE_DB);
    if read && value(eq::ANALYZE) >= 0.5 {
        plot::spectrum(p, plot_rect, &spectrum, sr);
    }
    if let Some(other) = &other {
        plot::curve(
            p,
            plot_rect,
            RANGE_DB,
            Stroke::new(1.0, pal().text_dim),
            |hz| total(other, hz),
        );
    }
    plot::curve(
        p,
        plot_rect,
        RANGE_DB,
        Stroke::new(1.5, pal().accent),
        |hz| total(&settings, hz),
    );
    for (b, setting) in settings.iter().enumerate() {
        if !setting.on {
            continue;
        }
        let gain = value(eq::band_param(edit, b, eq::B_GAIN));
        let at = dot(plot_rect, setting, gain);
        let colour = if state.selected[b] {
            pal().accent
        } else {
            pal().text_dim
        };
        p.circle_filled(at, DOT_RADIUS, colour);
        p.text(
            at,
            Align2::CENTER_CENTER,
            format!("{}", b + 1),
            theme::font(8.0),
            pal().text_dark,
        );
    }
    if state.box_select {
        if let (Some(start), Some(now)) = (state.press, response.interact_pointer_pos()) {
            p.rect_stroke(
                Rect::from_two_pos(start, now),
                0.0,
                Stroke::new(1.0, pal().text_dim),
                StrokeKind::Inside,
            );
        }
    }

    draw_strip(ui, panel, strip, &settings, edit, &mut state);
    ui.ctx().data_mut(|d| d.insert_temp(id, state));
}

/// The band buttons: click the number to pick a band, its corner to switch it, and the name to
/// step through the responses.
fn draw_strip(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    strip: Rect,
    settings: &[eq::BandSetting; eq::BANDS],
    edit: usize,
    state: &mut State,
) {
    let width = strip.width() / eq::BANDS as f32;
    for (b, setting) in settings.iter().enumerate() {
        let cell = Rect::from_min_size(
            Pos2::new(strip.left() + b as f32 * width, strip.top()),
            Vec2::new(width - 2.0, strip.height()),
        );
        let on_id = ui.scope_id().with(("eq_eight_on", b, panel.device.id));
        let on_rect = Rect::from_min_size(cell.min, Vec2::new(12.0, cell.height()));
        if ui.interact(on_rect, on_id, Sense::click()).clicked() {
            panel.set(
                ParamId(eq::band_param(edit, b, eq::B_ON)),
                if setting.on { 0.0 } else { 1.0 },
            );
        }
        let name_id = ui.scope_id().with(("eq_eight_name", b, panel.device.id));
        let name_rect = Rect::from_min_max(Pos2::new(on_rect.right(), cell.top()), cell.max);
        let name = ui.interact(name_rect, name_id, Sense::click());
        let current = response_index(setting.response);
        if name.clicked() {
            state.selected = [false; eq::BANDS];
            state.selected[b] = true;
            panel.set(
                ParamId(eq::band_param(edit, b, eq::B_TYPE)),
                ((current + 1) % eq::RESPONSE_LABELS.len()) as f64,
            );
        }
        let p = ui.painter();
        let fill = if state.selected[b] {
            pal().raised
        } else {
            pal().knob_track
        };
        p.rect_filled(cell, 2.0, fill);
        p.circle_filled(
            Pos2::new(cell.left() + 6.0, cell.center().y),
            3.0,
            if setting.on {
                pal().activator
            } else {
                pal().text_dim
            },
        );
        p.text(
            Pos2::new(cell.left() + 14.0, cell.top() + 8.0),
            Align2::LEFT_CENTER,
            format!("{}", b + 1),
            theme::font(9.0),
            pal().text,
        );
        p.text(
            Pos2::new(cell.left() + 14.0, cell.bottom() - 8.0),
            Align2::LEFT_CENTER,
            eq::RESPONSE_LABELS[current],
            theme::font(7.0),
            pal().text_dim,
        );
    }
}

fn response_index(response: eq::Response) -> usize {
    use eq::Response::*;
    match response {
        LowCut48 => 0,
        LowCut12 => 1,
        LowShelf => 2,
        Peak => 3,
        Notch => 4,
        HighShelf => 5,
        HighCut12 => 6,
        HighCut48 => 7,
    }
}
