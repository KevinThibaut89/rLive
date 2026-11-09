//! The Envelopes tab of the Clip View (manual ch. 27): choosing an envelope, and drawing it the
//! way the automation lanes are drawn, with the same geometry and the same edits.
//!
//! Red envelopes are automation and blue ones modulation (p. 537). An Arrangement clip only has
//! the blue kind.

use super::automation::geom::{hit_test, segment_path, EnvHit, LaneGeom};
use super::keymap::{self, Platform};
use super::theme::*;
use super::timeline::{self, TimeAxis};
use super::widgets::{self, info};
use super::UiState;
use crate::session::{ClipRef, EnvGroup, Session};
use egui::{Color32, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
use rlive_core::param::{ClipParam, EnvTarget, ParamFlags};
use rlive_core::{ParamRange, PPQ};
use rlive_engine::shared::NO_SLOT;
use rlive_project::clip_envelope::{
    clip_param_range, clip_param_rest, modulation_range, MAX_MIDI_CTRL, MIDI_CTRL_RANGE,
};
use rlive_project::envelope_edit::{self, can_curve, curve_after_drag};
use rlive_project::warp_edit::WarpOp;
use rlive_project::{Breakpoint, Clip, ClipContent, EnvLoop, EnvMode};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

const CHOOSER_W: f32 = 176.0;
const RULER_H: f32 = 14.0;
/// How close the pointer must be to a Warp Marker to grab it.
const MARKER_GRAB: f32 = 6.0;

/// A drag in progress in the editor.
enum Drag {
    Point {
        base: Vec<Breakpoint>,
        i: usize,
    },
    Curve {
        base: Vec<Breakpoint>,
        i: usize,
        dy: f32,
    },
    Draw {
        base: Vec<Breakpoint>,
        before: f64,
        cells: BTreeMap<i64, f64>,
        last: Option<(i64, f64)>,
        width: f64,
    },
    Marker {
        tick: f64,
    },
}

/// What the Envelopes tab is showing.
#[derive(Default)]
pub struct ClipEnvState {
    /// Showing Envelopes rather than the Sample or Notes.
    pub open: bool,
    clip: Option<ClipRef>,
    target: Option<EnvTarget>,
    mode: EnvMode,
    /// Show Adjusted Only: the choosers merge into one list of what has envelopes.
    adjusted_only: bool,
    drag: Option<Drag>,
}

impl ClipEnvState {
    /// What's chosen is about one clip; another clip starts over.
    fn follow_clip(&mut self, clip: ClipRef) {
        if self.clip != Some(clip) {
            self.clip = Some(clip);
            self.target = None;
            self.drag = None;
            if clip.as_slot().is_none() {
                self.mode = EnvMode::Modulation;
            }
        }
    }
}

/// One entry in the Control chooser.
#[derive(Clone)]
struct Control {
    device: String,
    label: String,
    target: EnvTarget,
    /// The parameter's own range, when it's a parameter.
    param: Option<(ParamRange, rlive_core::ModMode)>,
    /// Where it sits now, as the dashed line of a control with no automation.
    current: f64,
}

/// Names of the MIDI controllers that have one, for the Control chooser.
fn controller_name(cc: u8) -> String {
    let name = match cc {
        0 => "Bank Select",
        1 => "Modulation",
        2 => "Breath",
        4 => "Foot",
        5 => "Portamento",
        7 => "Volume",
        8 => "Balance",
        10 => "Pan",
        11 => "Expression",
        64 => "Sustain",
        65 => "Portamento On",
        66 => "Sostenuto",
        71 => "Resonance",
        72 => "Release",
        73 => "Attack",
        74 => "Brightness",
        _ => return format!("Controller {cc}"),
    };
    format!("{cc} {name}")
}

/// Everything this clip's envelopes can drive, as the Device and Control choosers list it.
fn controls_of(session: &Session, cref: ClipRef, clip: &Clip) -> Vec<Control> {
    let mut out = Vec::new();
    match &clip.content {
        ClipContent::Audio { .. } => {
            let mut params = vec![
                ("Gain", ClipParam::Gain),
                ("Transposition", ClipParam::Transpose),
            ];
            if clip.sample_offset_available() {
                params.push(("Sample Offset", ClipParam::SampleOffset));
            }
            for (label, param) in params {
                out.push(Control {
                    device: "Clip".into(),
                    label: label.into(),
                    target: EnvTarget::Clip(param),
                    param: None,
                    current: clip_param_rest(param),
                });
            }
        }
        ClipContent::Midi { .. } => {
            for cc in 0..=MAX_MIDI_CTRL {
                out.push(Control {
                    device: "MIDI Ctrl".into(),
                    label: controller_name(cc),
                    target: EnvTarget::MidiCtrl(cc),
                    param: None,
                    current: 0.0,
                });
            }
        }
    }
    let chain = crate::session::ChainRef::Track(cref.track());
    for EnvGroup { label, params, .. } in session.env_catalog(chain) {
        for p in params {
            if p.info.flags.contains(ParamFlags::READ_ONLY) {
                continue;
            }
            let current = session.env_base_value(chain, p.target).unwrap_or(0.0);
            out.push(Control {
                device: label.clone(),
                label: p.info.name.clone(),
                target: p.target,
                param: Some((p.range, p.info.mod_mode)),
                current,
            });
        }
    }
    out
}

/// What the lane's vertical axis runs over in this mode, and where the dashed line sits.
fn scale_of(control: &Control, mode: EnvMode) -> (ParamRange, f64) {
    match (control.target, control.param) {
        (EnvTarget::Clip(p), _) => (clip_param_range(p), control.current),
        (EnvTarget::MidiCtrl(_), _) => (MIDI_CTRL_RANGE, control.current),
        (_, Some((range, _))) if mode == EnvMode::Automation => (range, control.current),
        (_, Some((_, mod_mode))) => {
            let (range, rest) = modulation_range(mod_mode);
            (range, rest)
        }
        (_, None) => (
            ParamRange {
                min: 0.0,
                max: 1.0,
                skew: 1.0,
                quantized: false,
            },
            control.current,
        ),
    }
}

fn mode_color(mode: EnvMode) -> Color32 {
    match mode {
        EnvMode::Automation => pal().arm_red,
        EnvMode::Modulation => pal().solo_blue,
    }
}

/// LEDs next to controls: what kinds of envelope this clip has for it (manual p. 538).
fn led_colors(clip: &Clip, target: EnvTarget) -> (Option<Color32>, Option<Color32>) {
    let has = |mode| {
        clip.envelope(target, mode)
            .is_some_and(|e| !e.envelope.points.is_empty())
    };
    (
        has(EnvMode::Automation).then(|| mode_color(EnvMode::Automation)),
        has(EnvMode::Modulation).then(|| mode_color(EnvMode::Modulation)),
    )
}

/// The entry for a control in a chooser, with its LEDs. Returns whether it was chosen.
fn chooser_entry(
    ui: &mut egui::Ui,
    leds: (Option<Color32>, Option<Color32>),
    selected: bool,
    label: &str,
) -> bool {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 12.0), Sense::hover());
        let p = ui.painter();
        if let Some(c) = leds.0 {
            p.circle_filled(Pos2::new(rect.left() + 3.0, rect.center().y), 2.5, c);
        }
        if let Some(c) = leds.1 {
            p.circle_filled(Pos2::new(rect.left() + 10.0, rect.center().y), 2.5, c);
        }
        ui.selectable_label(selected, label).clicked()
    })
    .inner
}

/// The Envelopes tab: the choosers on the left and the envelope on the right.
pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    area: Rect,
) {
    state.env.follow_clip(cref);
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let controls = controls_of(session, cref, &clip);
    let chosen = state
        .env
        .target
        .and_then(|t| controls.iter().find(|c| c.target == t))
        .or_else(|| controls.first())
        .cloned();
    let Some(control) = chosen else {
        return;
    };
    state.env.target = Some(control.target);
    let chooser = Rect::from_min_max(area.min, Pos2::new(area.left() + CHOOSER_W, area.bottom()));
    ui.painter().line_segment(
        [chooser.right_top(), chooser.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    let mut cu = ui.new_child(UiBuilder::new().max_rect(chooser.shrink(5.0)));
    cu.set_clip_rect(chooser.intersect(ui.clip_rect()));
    choosers(&mut cu, session, state, cref, &clip, &controls, &control);
    let editor = Rect::from_min_max(Pos2::new(chooser.right() + 1.0, area.top()), area.max);
    lane(ui, session, state, cref, &clip, &control, editor);
}

#[allow(clippy::too_many_arguments)]
fn choosers(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    clip: &Clip,
    controls: &[Control],
    control: &Control,
) {
    ui.spacing_mut().item_spacing.y = 3.0;
    let env = &mut state.env;
    let chosen_before = env.target;
    if env.adjusted_only {
        let adjusted: Vec<&Control> = controls
            .iter()
            .filter(|c| {
                let (a, m) = led_colors(clip, c.target);
                a.is_some() || m.is_some() || c.target == control.target
            })
            .collect();
        let text = format!("{}: {}", control.device, control.label);
        egui::ComboBox::from_id_salt("env_adjusted")
            .width(ui.available_width())
            .selected_text(text)
            .show_ui(ui, |ui| {
                for c in adjusted {
                    let label = format!("{}: {}", c.device, c.label);
                    if chooser_entry(
                        ui,
                        led_colors(clip, c.target),
                        c.target == control.target,
                        &label,
                    ) {
                        env.target = Some(c.target);
                    }
                }
            });
    } else {
        let mut devices: Vec<&str> = Vec::new();
        for c in controls {
            if !devices.contains(&c.device.as_str()) {
                devices.push(&c.device);
            }
        }
        egui::ComboBox::from_id_salt("env_device")
            .width(ui.available_width())
            .selected_text(control.device.clone())
            .show_ui(ui, |ui| {
                for d in &devices {
                    let adjusted =
                        controls
                            .iter()
                            .filter(|c| c.device == *d)
                            .fold((None, None), |acc, c| {
                                let leds = led_colors(clip, c.target);
                                (acc.0.or(leds.0), acc.1.or(leds.1))
                            });
                    if chooser_entry(ui, adjusted, *d == control.device, d) {
                        if let Some(c) = controls.iter().find(|c| c.device == *d) {
                            env.target = Some(c.target);
                        }
                    }
                }
            });
        egui::ComboBox::from_id_salt("env_control")
            .width(ui.available_width())
            .selected_text(control.label.clone())
            .show_ui(ui, |ui| {
                for c in controls.iter().filter(|c| c.device == control.device) {
                    if chooser_entry(
                        ui,
                        led_colors(clip, c.target),
                        c.target == control.target,
                        &c.label,
                    ) {
                        env.target = Some(c.target);
                    }
                }
            });
    }
    if env.target != chosen_before {
        session.envelope_chooser_picked(env.target);
    }
    if cref.as_slot().is_some() && matches!(control.target, EnvTarget::Param(_)) {
        ui.horizontal(|ui| {
            for (label, mode) in [
                ("Automation", EnvMode::Automation),
                ("Modulation", EnvMode::Modulation),
            ] {
                if widgets::state_button(
                    ui,
                    Vec2::new(78.0, 15.0),
                    env.mode == mode,
                    mode_color(mode),
                    label,
                )
                .clicked()
                {
                    env.mode = mode;
                }
            }
        });
    }
    let r = ui.checkbox(&mut env.adjusted_only, "Show Adjusted Only");
    info(
        r,
        "Show Adjusted Only — list only the envelopes this clip has",
    );
    let mode = effective_mode(cref, env.mode, control.target);
    region_controls(ui, session, cref, clip, control, mode);
}

/// A non-parameter envelope isn't one of the two kinds, and an Arrangement clip has only one.
fn effective_mode(cref: ClipRef, mode: EnvMode, target: EnvTarget) -> EnvMode {
    if cref.as_slot().is_none() || !matches!(target, EnvTarget::Param(_)) {
        EnvMode::Modulation
    } else {
        mode
    }
}

fn beats_of(ticks: u64) -> f64 {
    ticks as f64 / PPQ as f64
}

/// The Linked switch and an unlinked envelope's own loop, region and start marker (p. 543).
fn region_controls(
    ui: &mut egui::Ui,
    session: &mut Session,
    cref: ClipRef,
    clip: &Clip,
    control: &Control,
    mode: EnvMode,
) {
    let target = control.target;
    let Some(env) = clip.envelope(target, mode) else {
        return;
    };
    let region = env.loop_region;
    let linked = region.is_none();
    let r = widgets::state_button(
        ui,
        Vec2::new(78.0, 15.0),
        linked,
        pal().accent,
        if linked { "Linked" } else { "Unlinked" },
    );
    if info(
        r,
        "Linked — off to give this envelope its own loop, region and start marker",
    )
    .clicked()
    {
        let next = if linked {
            Some(EnvLoop::new(0, clip.length.max(1)))
        } else {
            None
        };
        session.set_clip_envelope_loop(cref, target, mode, next);
    }
    let Some(mut r) = region else {
        return;
    };
    let before = r;
    egui::Grid::new("env_region")
        .num_columns(2)
        .spacing([6.0, 2.0])
        .show(ui, |ui| {
            let (mut start, mut length, mut marker) =
                (beats_of(r.start), beats_of(r.length), beats_of(r.marker));
            ui.label(RichText::new("Position").color(pal().text_dim));
            ui.add(
                egui::DragValue::new(&mut start)
                    .range(0.0..=4096.0)
                    .speed(0.05)
                    .suffix(" b"),
            );
            ui.end_row();
            ui.label(RichText::new("Length").color(pal().text_dim));
            ui.add(
                egui::DragValue::new(&mut length)
                    .range(0.05..=4096.0)
                    .speed(0.05)
                    .suffix(" b"),
            );
            ui.end_row();
            ui.label(RichText::new("Start").color(pal().text_dim));
            ui.add(
                egui::DragValue::new(&mut marker)
                    .range(0.0..=4096.0)
                    .speed(0.05)
                    .suffix(" b"),
            );
            ui.end_row();
            ui.label(RichText::new("Loop").color(pal().text_dim));
            ui.checkbox(&mut r.looped, "");
            ui.end_row();
            r.start = (start * PPQ as f64).round() as u64;
            r.length = ((length * PPQ as f64).round() as u64).max(1);
            r.marker = (marker * PPQ as f64).round() as u64;
        });
    if r != before {
        session.set_clip_envelope_loop(cref, target, mode, Some(r));
    }
}

/// The envelope itself: a ruler, the lane and its gestures.
#[allow(clippy::too_many_arguments)]
fn lane(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    clip: &Clip,
    control: &Control,
    area: Rect,
) {
    let area = area.shrink(4.0);
    let mode = effective_mode(cref, state.env.mode, control.target);
    let env = clip.envelope(control.target, mode);
    let points = env.map_or_else(Vec::new, |e| e.envelope.points.clone());
    let region = env.and_then(|e| e.loop_region);
    let length = region
        .map_or(clip.length, |r| r.end().max(clip.length))
        .max(1);
    let sig = session.project.time_signature;
    let (scale_range, current) = scale_of(control, mode);
    let ruler = Rect::from_min_size(area.min, Vec2::new(area.width(), RULER_H));
    let rect = Rect::from_min_max(Pos2::new(area.left(), ruler.bottom()), area.max);
    let beats = length as f32 / PPQ as f32;
    let px_per_beat = (rect.width() / beats.max(1.0)).max(1.0);
    let axis = TimeAxis {
        left: rect.left(),
        px_per_beat,
        scroll: 0.0,
    };
    let geom = LaneGeom {
        rect,
        axis,
        range: scale_range,
    };
    let grid = timeline::adaptive_grid(px_per_beat, sig.ticks_per_bar());
    let p = ui.painter_at(area);
    p.rect_filled(rect, 0.0, pal().grid_bg);
    paint_grid(&p, rect, &axis, grid, sig.ticks_per_bar(), length);
    if let Some(r) = region {
        // The envelope's own loop, drawn in the accent to say it's unlinked.
        let (a, b) = (axis.x(r.start as f64), axis.x(r.end() as f64));
        let brace = Rect::from_min_max(
            Pos2::new(a, ruler.bottom() - 3.0),
            Pos2::new(b, ruler.bottom()),
        );
        p.rect_filled(
            brace,
            0.0,
            if r.looped {
                pal().accent
            } else {
                pal().text_faint
            },
        );
        let marker = axis.x(r.marker as f64);
        p.line_segment(
            [
                Pos2::new(marker, ruler.top()),
                Pos2::new(marker, rect.bottom()),
            ],
            Stroke::new(1.0, pal().accent),
        );
    } else {
        paint_markers(&p, clip, &axis, ruler);
    }
    let color = mode_color(mode);
    let dashed = points
        .is_empty()
        .then(|| scale_range.to_normalized(current));
    if let Some(knob) = dashed {
        let y = geom.y(knob);
        p.add(egui::Shape::dashed_line(
            &[Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0, pal().text_faint),
            5.0,
            4.0,
        ));
    } else {
        paint_points(&p, &geom, &points, color);
    }
    paint_playhead(&p, session, cref, clip, &axis, rect);

    input(
        ui, session, state, cref, clip, control, mode, &geom, ruler, grid, &points, current,
    );
}

fn paint_grid(p: &egui::Painter, rect: Rect, axis: &TimeAxis, grid: u64, bar: u64, length: u64) {
    let mut t = 0;
    while t <= length {
        let x = axis.x(t as f64);
        let strong = t % bar == 0;
        p.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(
                1.0,
                if strong {
                    pal().separator
                } else {
                    shade(0x2a2a2a)
                },
            ),
        );
        t += grid.max(1);
    }
}

fn paint_points(p: &egui::Painter, g: &LaneGeom, points: &[Breakpoint], color: Color32) {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return;
    };
    let stroke = Stroke::new(1.5, color);
    let (fp, lp) = (g.pos(first), g.pos(last));
    if fp.x > g.rect.left() {
        p.line_segment([Pos2::new(g.rect.left(), fp.y), fp], stroke);
    }
    if lp.x < g.rect.right() {
        p.line_segment([lp, Pos2::new(g.rect.right(), lp.y)], stroke);
    }
    for (i, point) in points.iter().enumerate() {
        if i + 1 < points.len() {
            let path = segment_path(points, i, g);
            p.add(egui::Shape::line(path, stroke));
        }
        let at = g.pos(point);
        p.circle_filled(at, 2.5, color);
    }
}

/// A Linked envelope follows the clip's Warp Markers, so they're shown over it and can be moved
/// from here (p. 544).
fn paint_markers(p: &egui::Painter, clip: &Clip, axis: &TimeAxis, ruler: Rect) {
    let ClipContent::Audio { warp, .. } = &clip.content else {
        return;
    };
    if !warp.enabled {
        return;
    }
    for m in &warp.markers {
        let x = axis.x(m.tick);
        let tip = Pos2::new(x, ruler.bottom());
        p.add(egui::Shape::convex_polygon(
            vec![
                tip,
                Pos2::new(x - 4.0, ruler.top() + 2.0),
                Pos2::new(x + 4.0, ruler.top() + 2.0),
            ],
            pal().activator,
            Stroke::NONE,
        ));
    }
}

fn paint_playhead(
    p: &egui::Painter,
    session: &Session,
    cref: ClipRef,
    clip: &Clip,
    axis: &TimeAxis,
    rect: Rect,
) {
    let Some(shared) = session.track_shared.get(&cref.track()) else {
        return;
    };
    let playing = shared.playing_slot.load(Ordering::Relaxed);
    let matched = cref
        .as_slot()
        .is_some_and(|(_, slot)| playing != NO_SLOT && playing == slot as i32);
    if !matched || clip.length == 0 {
        return;
    }
    let x = axis.x(shared.clip_position.load());
    p.line_segment(
        [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
        Stroke::new(1.0, pal().play_green),
    );
}

#[allow(clippy::too_many_arguments)]
fn input(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    clip: &Clip,
    control: &Control,
    mode: EnvMode,
    geom: &LaneGeom,
    ruler: Rect,
    grid: u64,
    points: &[Breakpoint],
    current: f64,
) {
    let target = control.target;
    let id = ui.scope_id().with(("clip_env_lane", target));
    let resp = ui.interact(geom.rect, id, Sense::click_and_drag());
    let marker_resp = ui.interact(ruler, id.with("markers"), Sense::click_and_drag());
    let m = ui.input(|i| i.modifiers);
    let snap = !keymap::ignores_grid(m, Platform::current());
    let axis = geom.axis;
    let snapped = |x: f32| timeline::snap(axis.tick(x), grid, snap).max(0) as f64;
    let range = geom.range;
    let value_at = |y: f32| range.from_normalized(geom.knob(y));
    let dashed = points.is_empty().then(|| range.to_normalized(current));
    let set = |session: &mut Session, points: Vec<Breakpoint>| {
        session.set_clip_envelope(cref, target, mode, points);
    };

    if let Some(pos) = marker_resp.hover_pos() {
        if marker_at(clip, geom, pos.x).is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }
    if marker_resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin());
        if let Some(tick) = origin.and_then(|o| marker_at(clip, geom, o.x)) {
            state.env.drag = Some(Drag::Marker { tick });
        }
    }
    if let (Some(Drag::Marker { tick }), true, Some(pos)) = (
        state.env.drag.as_mut(),
        marker_resp.dragged(),
        marker_resp.interact_pointer_pos(),
    ) {
        let to = snapped(pos.x);
        if to != *tick {
            let from = std::mem::replace(tick, to);
            session.apply_warp_op(&[cref], &WarpOp::Move { tick: from, to });
        }
    }

    if resp.drag_started() {
        let start = ui
            .input(|i| i.pointer.press_origin())
            .unwrap_or(resp.rect.center());
        let hit = hit_test(points, geom, start, dashed);
        let draw = state.arr.auto.draw;
        state.env.drag = match hit {
            _ if draw => Some(Drag::Draw {
                base: points.to_vec(),
                before: current,
                cells: BTreeMap::new(),
                last: None,
                width: if snap {
                    grid as f64
                } else {
                    (axis.tick(axis.left + 2.0) - axis.tick(axis.left)).max(1.0)
                },
            }),
            EnvHit::Point(i) => Some(Drag::Point {
                base: points.to_vec(),
                i,
            }),
            EnvHit::Line(Some(i)) | EnvHit::Near(i) if m.alt && can_curve(points, i, range) => {
                Some(Drag::Curve {
                    base: points.to_vec(),
                    i,
                    dy: 0.0,
                })
            }
            _ => {
                let mut with_point = points.to_vec();
                let i = envelope_edit::insert(
                    &mut with_point,
                    snapped(start.x),
                    value_at(start.y),
                    range,
                );
                set(session, with_point.clone());
                Some(Drag::Point {
                    base: with_point,
                    i,
                })
            }
        };
    }
    if resp.dragged() {
        if let (Some(drag), Some(pos)) = (state.env.drag.as_mut(), resp.interact_pointer_pos()) {
            update_drag(
                session,
                cref,
                target,
                mode,
                drag,
                geom,
                pos,
                resp.drag_delta(),
                &snapped,
                snap,
            );
        }
    }
    if resp.drag_stopped() || marker_resp.drag_stopped() {
        state.env.drag = None;
    }
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            match hit_test(points, geom, pos, dashed) {
                EnvHit::Point(i) => {
                    let mut next = points.to_vec();
                    next.remove(i);
                    set(session, next);
                }
                EnvHit::Line(_) => {
                    let mut next = points.to_vec();
                    envelope_edit::insert(&mut next, snapped(pos.x), value_at(pos.y), range);
                    set(session, next);
                }
                _ => {}
            }
        }
    }
    if resp.double_clicked() && !points.is_empty() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if hit_test(points, geom, pos, dashed) == EnvHit::Background {
                let mut next = points.to_vec();
                envelope_edit::insert(&mut next, snapped(pos.x), value_at(pos.y), range);
                set(session, next);
            }
        }
    }
    let resp = info(
        resp,
        "Clip Envelope — click the line to add a breakpoint or a point to remove it, drag to move (Alt = curve), right-click to clear",
    );
    resp.context_menu(|ui| {
        if ui.button("Clear Envelope").clicked() {
            set(session, Vec::new());
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn update_drag(
    session: &mut Session,
    cref: ClipRef,
    target: EnvTarget,
    mode: EnvMode,
    drag: &mut Drag,
    geom: &LaneGeom,
    pos: Pos2,
    delta: Vec2,
    snapped: &impl Fn(f32) -> f64,
    snap: bool,
) {
    let range = geom.range;
    let commit = |session: &mut Session, points: Vec<Breakpoint>| {
        session.set_clip_envelope(cref, target, mode, points);
    };
    match drag {
        Drag::Point { base, i } => {
            let p0 = base[*i];
            let dt = snapped(pos.x) - p0.time;
            let dknob = geom.knob(pos.y) - range.to_normalized(p0.value);
            let moved = envelope_edit::move_points(base, &[*i], dt, dknob, range);
            commit(session, moved);
        }
        Drag::Curve { base, i, dy } => {
            *dy += delta.y;
            let (a, b) = (base[*i], base[*i + 1]);
            let rising = range.to_normalized(b.value) >= range.to_normalized(a.value);
            let mut next = base.clone();
            next[*i].curve = curve_after_drag(a.curve, rising, *dy);
            commit(session, next);
        }
        Drag::Draw {
            base,
            before,
            cells,
            last,
            width,
        } => {
            let cell = (geom.axis.tick(pos.x) / *width).floor() as i64;
            let value = range.from_normalized(geom.knob(pos.y));
            let at = (cell, value);
            envelope_edit::paint_cells(cells, *last, at);
            *last = Some(at);
            let stroke = if snap {
                envelope_edit::grid_stroke(cells, *width)
            } else {
                envelope_edit::smooth_stroke(cells, *width)
            };
            let drawn = envelope_edit::overwrite(base, &stroke, *before, range);
            commit(session, drawn);
        }
        Drag::Marker { .. } => {}
    }
}

fn marker_at(clip: &Clip, geom: &LaneGeom, x: f32) -> Option<f64> {
    let ClipContent::Audio { warp, .. } = &clip.content else {
        return None;
    };
    if !warp.enabled {
        return None;
    }
    warp.markers
        .iter()
        .map(|m| m.tick)
        .find(|t| (geom.axis.x(*t) - x).abs() <= MARKER_GRAB)
}
