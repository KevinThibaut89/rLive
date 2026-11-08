//! What the mouse and keyboard do in an automation lane.

use super::super::arrangement::TimeSel;
use super::super::keymap::{self, Platform};
use super::super::timeline;
use super::super::widgets;
use super::super::UiState;
use super::geom::{handle_at, hit_test, DragAccum, EnvHit, Handle};
use super::lanes::{self, Arrow, LaneKey, Nav};
use super::popups::{LaneMenu, MenuHit};
use super::{apply_handle, selection_rect, Frame, LaneView};
use crate::session::Session;
use egui::{Modifiers, Pos2, Rect, Sense};
use rlive_core::param::EnvTarget;
use rlive_core::PPQ;
use rlive_project::envelope_edit::{self, can_curve, curve_after_drag, Corners};
use rlive_project::Breakpoint;
use std::collections::BTreeMap;

/// How finely a stroke is sampled without the grid, in screen points.
const SMOOTH_CELL_PX: f32 = 2.0;

/// A drag in progress.
pub(super) enum Gesture {
    /// Breakpoints being moved: a whole selection or the one grabbed.
    Points {
        key: LaneKey,
        target: EnvTarget,
        base: Vec<Breakpoint>,
        grabbed: f64,
        moved: Vec<usize>,
        accum: DragAccum,
    },
    Segment {
        key: LaneKey,
        target: EnvTarget,
        base: Vec<Breakpoint>,
        i: usize,
        /// The time selection it belongs to.
        span: Option<(f64, f64)>,
        accum: DragAccum,
    },
    Curve {
        key: LaneKey,
        target: EnvTarget,
        base: Vec<Breakpoint>,
        i: usize,
        dy: f32,
    },
    Select {
        anchor: f64,
        origin: LaneKey,
    },
    Draw {
        key: LaneKey,
        target: EnvTarget,
        base: Vec<Breakpoint>,
        before: f64,
        cells: BTreeMap<i64, f64>,
        last: Option<(i64, f64)>,
        width: f64,
        grid: bool,
    },
    Handle {
        key: LaneKey,
        target: EnvTarget,
        handle: Handle,
        mirrored: bool,
        start: Box<Frame>,
        accum: DragAccum,
    },
}

/// The modifier that puts a click or drag on the time grid off it.
fn snapping(state: &UiState, m: Modifiers) -> bool {
    state.arr.grid.snapping(m, Platform::current())
}

fn snap_tick(tick: f64, grid: u64, snap: bool) -> f64 {
    timeline::snap(tick, grid, snap).max(0) as f64
}

/// Arrow keys among the lanes, when no text field has them.
pub(super) fn keyboard(
    ui: &egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    order: &[LaneKey],
) {
    if state.focus != keymap::FocusArea::Arrangement || ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let Some(focus) = state.arr.auto.focus else {
        return;
    };
    let arrow = ui.input(|i| {
        if i.key_pressed(egui::Key::ArrowUp) {
            Some(Arrow::Up)
        } else if i.key_pressed(egui::Key::ArrowDown) {
            Some(Arrow::Down)
        } else if i.key_pressed(egui::Key::ArrowLeft) {
            Some(Arrow::Left)
        } else if i.key_pressed(egui::Key::ArrowRight) {
            Some(Arrow::Right)
        } else {
            None
        }
    });
    let Some(arrow) = arrow else { return };
    match lanes::navigate(order, focus, arrow) {
        Some(Nav::Focus(k)) => state.arr.auto.focus = Some(k),
        Some(Nav::HideLanes(chain)) => session.edit_track_ui(chain, |ui| {
            if !ui.automation_lanes_hidden {
                lanes::toggle_hidden(ui);
            }
        }),
        Some(Nav::ShowLanes(chain)) => {
            session.edit_track_ui(chain, |ui| ui.automation_lanes_hidden = false)
        }
        None => {}
    }
}

fn apply(session: &mut Session, view: &LaneView, target: EnvTarget, points: Vec<Breakpoint>) {
    if points != view.points {
        session.set_envelope(view.key.chain, target, points);
    }
}

fn plain_at(view: &LaneView, y: f32) -> f64 {
    view.geom.range.from_normalized(view.geom.knob(y))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lane_input(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    view: &LaneView,
    lanes_area: Rect,
    grid: u64,
    all: &[(LaneKey, f32, f32)],
    usable: &[(LaneKey, bool)],
) {
    let area = view.hit_rect.intersect(lanes_area);
    let id = ui.scope_id().with(("auto_lane", view.key));
    let resp = ui.interact(area, id, Sense::click_and_drag());
    let m = ui.input(|i| i.modifiers);
    let snap = snapping(state, m);
    let origin = ui.input(|i| i.pointer.press_origin());
    let pointer = resp.interact_pointer_pos();

    if resp.is_pointer_button_down_on() || resp.clicked() {
        state.arr.auto.focus = Some(view.key);
    }

    if let Some(target) = view.target {
        let draw = state.arr.auto.draw;
        if resp.drag_started() {
            let start = origin.unwrap_or(resp.rect.center());
            begin_drag(session, state, view, target, start, m, grid, snap, draw);
        }
    } else if resp.drag_started() {
        // A lane set to None still takes part in time selections.
        begin_select(session, state, view, origin, grid, snap);
    }
    update_gesture(session, state, view, &resp, m, grid, snap, all, usable);

    if resp.drag_stopped() {
        state.arr.auto.gesture = None;
    }

    let Some(pos) = pointer else {
        resp.context_menu(|ui| super::popups::lane_menu(ui, session, state, view, grid));
        return;
    };
    let hit = view.target.map_or(EnvHit::Background, |_| {
        hit_test(&view.points, &view.geom, pos, view.dashed_knob())
    });
    if let Some(target) = view.target {
        if resp.double_clicked() {
            double_click(session, state, view, target, hit, pos, m, grid, snap);
        } else if resp.clicked() {
            single_click(session, state, view, target, hit, pos, m, grid, snap);
        }
    } else if resp.clicked() {
        background_click(session, state, pos, view, grid, snap);
    }
    if resp.secondary_clicked() {
        let tick = view.geom.axis.tick(pos.x);
        state.arr.auto.menu = Some(LaneMenu {
            key: view.key,
            pos,
            hit: MenuHit::from(hit),
            tick,
        });
    }
    let resp = widgets::info(
        resp,
        "Automation Lane — click the line to add a breakpoint or a point to remove it, drag to move (Cmd = no snap, Shift = fine, Alt = curve), right-click for shapes and values",
    );
    resp.context_menu(|ui| super::popups::lane_menu(ui, session, state, view, grid));
}

#[allow(clippy::too_many_arguments)]
fn begin_drag(
    session: &mut Session,
    state: &mut UiState,
    view: &LaneView,
    target: EnvTarget,
    start: Pos2,
    m: Modifiers,
    grid: u64,
    snap: bool,
    draw: bool,
) {
    let key = view.key;
    let range = view.geom.range;
    if draw {
        let width = if snap {
            grid as f64
        } else {
            view.geom.axis.tick(view.geom.axis.left + SMOOTH_CELL_PX)
                - view.geom.axis.tick(view.geom.axis.left)
        };
        state.arr.auto.gesture = Some(Gesture::Draw {
            key,
            target,
            base: view.points.clone(),
            before: view.base,
            cells: BTreeMap::new(),
            last: None,
            width: width.max(1.0),
            grid: snap,
        });
        return;
    }
    if let Some((s0, s1)) = super::in_sel(&state.arr.time, key) {
        let sel = TimeSel {
            start: s0,
            end: s1,
            lanes: vec![key],
        };
        if let Some(handle) = handle_at(selection_rect(view, &sel), start) {
            begin_handle(session, state, view, target, handle, m, (s0, s1));
            return;
        }
    }
    let hit = hit_test(&view.points, &view.geom, start, view.dashed_knob());
    let accum = DragAccum::default();
    state.arr.auto.gesture = match hit {
        EnvHit::Point(i) => {
            let span = super::in_sel(&state.arr.time, key);
            let moved: Vec<usize> = match span {
                Some((a, b)) if view.points[i].time >= a && view.points[i].time <= b => view
                    .points
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.time >= a && p.time <= b)
                    .map(|(n, _)| n)
                    .collect(),
                _ => vec![i],
            };
            Some(Gesture::Points {
                key,
                target,
                base: view.points.clone(),
                grabbed: view.points[i].time,
                moved,
                accum,
            })
        }
        EnvHit::Line(Some(i)) if m.alt && can_curve(&view.points, i, range) => {
            Some(Gesture::Curve {
                key,
                target,
                base: view.points.clone(),
                i,
                dy: 0.0,
            })
        }
        EnvHit::Line(Some(i)) if m.shift => Some(segment_gesture(state, view, target, i)),
        EnvHit::Line(_) if m.alt => None,
        EnvHit::Line(_) => {
            // Grabbing the line makes a breakpoint and goes on dragging it.
            let tick = snap_tick(view.geom.axis.tick(start.x), grid, snap);
            let value = envelope_edit::value_at(&view.points, tick, range)
                .or(view.dashed_knob().map(|k| range.from_normalized(k)));
            let Some(value) = value else {
                state.arr.auto.gesture = None;
                return;
            };
            let mut base = view.points.clone();
            let at = envelope_edit::insert(&mut base, tick, value, range);
            Some(Gesture::Points {
                key,
                target,
                base,
                grabbed: tick,
                moved: vec![at],
                accum,
            })
        }
        EnvHit::Near(i) if m.alt && can_curve(&view.points, i, range) => Some(Gesture::Curve {
            key,
            target,
            base: view.points.clone(),
            i,
            dy: 0.0,
        }),
        EnvHit::Near(i) if !m.alt => Some(segment_gesture(state, view, target, i)),
        EnvHit::Near(_) => None,
        EnvHit::Background => {
            begin_select(session, state, view, Some(start), grid, snap);
            return;
        }
    };
}

fn segment_gesture(state: &UiState, view: &LaneView, target: EnvTarget, i: usize) -> Gesture {
    let mid = (view.points[i].time + view.points[i + 1].time) / 2.0;
    let span = super::in_sel(&state.arr.time, view.key).filter(|(a, b)| mid >= *a && mid <= *b);
    Gesture::Segment {
        key: view.key,
        target,
        base: view.points.clone(),
        i,
        span,
        accum: DragAccum::default(),
    }
}

fn begin_select(
    _session: &mut Session,
    state: &mut UiState,
    view: &LaneView,
    start: Option<Pos2>,
    grid: u64,
    snap: bool,
) {
    let Some(start) = start else { return };
    let anchor = snap_tick(view.geom.axis.tick(start.x), grid, snap);
    state.arr.auto.frame = None;
    state.arr.time = Some(TimeSel {
        start: anchor,
        end: anchor,
        lanes: vec![view.key],
    });
    state.arr.auto.gesture = Some(Gesture::Select {
        anchor,
        origin: view.key,
    });
}

fn begin_handle(
    session: &mut Session,
    state: &mut UiState,
    view: &LaneView,
    target: EnvTarget,
    handle: Handle,
    m: Modifiers,
    span: (f64, f64),
) {
    let current = session.env_points(view.key.chain, target).to_vec();
    let range = view.geom.range;
    let kept = state.arr.auto.frame.take().filter(|f| {
        f.key == view.key && f.target == target && f.span == span && f.result(range) == current
    });
    let start = kept.unwrap_or_else(|| Frame {
        key: view.key,
        target,
        base: current,
        orig: span,
        span,
        corners: Corners::default(),
    });
    state.arr.auto.frame = Some(start.clone());
    state.arr.auto.gesture = Some(Gesture::Handle {
        key: view.key,
        target,
        handle,
        mirrored: m.alt,
        start: Box::new(start),
        accum: DragAccum::default(),
    });
}

#[allow(clippy::too_many_arguments)]
fn update_gesture(
    session: &mut Session,
    state: &mut UiState,
    view: &LaneView,
    resp: &egui::Response,
    m: Modifiers,
    grid: u64,
    snap: bool,
    all: &[(LaneKey, f32, f32)],
    usable: &[(LaneKey, bool)],
) {
    if !resp.dragged() {
        return;
    }
    let delta = resp.drag_delta();
    let axis = view.geom.axis;
    let range = view.geom.range;
    let pointer = resp.interact_pointer_pos();
    let per_tick = PPQ as f64 / axis.px_per_beat as f64;
    let geom = view.geom;
    let auto = &mut state.arr.auto;
    let Some(gesture) = auto.gesture.as_mut() else {
        return;
    };
    let snap_dt = |anchor: f64, dx: f32| -> f64 {
        if dx == 0.0 {
            0.0
        } else {
            let target = anchor + dx as f64 * per_tick;
            timeline::snap(target, grid, snap).max(0) as f64 - anchor
        }
    };
    match gesture {
        Gesture::Points {
            key,
            target,
            base,
            grabbed,
            moved,
            accum,
        } if *key == view.key => {
            accum.apply(delta, m.shift);
            let dt = snap_dt(*grabbed, accum.dx);
            let points =
                envelope_edit::move_points(base, moved, dt, geom.knob_per_px(accum.dy), range);
            let target = *target;
            apply(session, view, target, points);
        }
        Gesture::Segment {
            key,
            target,
            base,
            i,
            span,
            accum,
        } if *key == view.key => {
            accum.apply(delta, m.shift);
            let anchor = base[*i].time;
            let dt = snap_dt(anchor, accum.dx);
            let dk = geom.knob_per_px(accum.dy);
            let points = match span {
                Some((a, b)) => envelope_edit::move_span(base, *a, *b, dt, dk, range),
                None => envelope_edit::move_points(base, &[*i, *i + 1], dt, dk, range),
            };
            let target = *target;
            apply(session, view, target, points);
        }
        Gesture::Curve {
            key,
            target,
            base,
            i,
            dy,
        } if *key == view.key => {
            *dy += delta.y;
            let (a, b) = (base[*i], base[*i + 1]);
            let rising = range.to_normalized(b.value) > range.to_normalized(a.value);
            let mut points = base.clone();
            points[*i].curve = curve_after_drag(a.curve, rising, *dy);
            let target = *target;
            apply(session, view, target, points);
        }
        Gesture::Select { anchor, origin } if *origin == view.key => {
            let Some(pos) = pointer else { return };
            let tick = snap_tick(axis.tick(pos.x), grid, snap);
            let (start, end) = (anchor.min(tick), anchor.max(tick));
            let lanes = spanned_lanes(all, usable, *origin, pos.y);
            state.arr.time = Some(TimeSel { start, end, lanes });
        }
        Gesture::Draw {
            key,
            target,
            base,
            before,
            cells,
            last,
            width,
            grid: stepped,
        } if *key == view.key => {
            let Some(pos) = pointer else { return };
            let cell = (axis.tick(pos.x) / *width).floor().max(0.0) as i64;
            let value = plain_at(view, pos.y);
            envelope_edit::paint_cells(cells, *last, (cell, value));
            *last = Some((cell, value));
            let stroke = if *stepped {
                envelope_edit::grid_stroke(cells, *width)
            } else {
                envelope_edit::smooth_stroke(cells, *width)
            };
            let points = envelope_edit::overwrite(base, &stroke, *before, range);
            let target = *target;
            apply(session, view, target, points);
        }
        Gesture::Handle {
            key,
            target,
            handle,
            mirrored,
            start,
            accum,
        } if *key == view.key => {
            accum.apply(delta, m.shift);
            let dk = geom.knob_per_px(accum.dy);
            let new_edge = match handle {
                Handle::Left => Some(snap_edge(start.span.0, accum.dx, per_tick, grid, snap)),
                Handle::Right => Some(snap_edge(start.span.1, accum.dx, per_tick, grid, snap)),
                _ => None,
            };
            let frame = apply_handle(start, *handle, *mirrored, dk, new_edge);
            let target = *target;
            let points = frame.result(range);
            let span = frame.span;
            auto.frame = Some(frame);
            if let Some(sel) = state.arr.time.as_mut() {
                sel.start = span.0;
                sel.end = span.1;
            }
            apply(session, view, target, points);
        }
        _ => {}
    }
}

fn snap_edge(edge: f64, dx: f32, per_tick: f64, grid: u64, snap: bool) -> f64 {
    let target = edge + dx as f64 * per_tick;
    timeline::snap(target, grid, snap).max(0) as f64
}

/// The lanes between the one a selection started in and the pointer, usable or not.
fn spanned_lanes(
    all: &[(LaneKey, f32, f32)],
    usable: &[(LaneKey, bool)],
    origin: LaneKey,
    y: f32,
) -> Vec<LaneKey> {
    let Some(from) = all.iter().position(|(k, ..)| *k == origin) else {
        return vec![origin];
    };
    let to = all
        .iter()
        .position(|(_, top, bottom)| y >= *top && y < *bottom)
        .unwrap_or(if all.first().is_some_and(|(_, top, _)| y < *top) {
            0
        } else {
            all.len().saturating_sub(1)
        });
    let (a, b) = (from.min(to), from.max(to));
    let keep = |k: &LaneKey| usable.iter().any(|(u, ok)| u == k && *ok);
    all[a..=b]
        .iter()
        .map(|(k, ..)| *k)
        .filter(|k| keep(k))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn double_click(
    session: &mut Session,
    _state: &mut UiState,
    view: &LaneView,
    target: EnvTarget,
    hit: EnvHit,
    pos: Pos2,
    m: Modifiers,
    grid: u64,
    snap: bool,
) {
    if m.alt {
        // Option or Alt double-click straightens a curved segment.
        let segment = match hit {
            EnvHit::Line(Some(i)) | EnvHit::Near(i) => Some(i),
            _ => None,
        };
        if let Some(i) = segment {
            let mut points = view.points.clone();
            points[i].curve = 0.0;
            apply(session, view, target, points);
        }
        return;
    }
    if matches!(hit, EnvHit::Near(_) | EnvHit::Background) {
        let mut points = view.points.clone();
        let tick = snap_tick(view.geom.axis.tick(pos.x), grid, snap);
        let value = view.geom.range.from_normalized(view.geom.knob(pos.y));
        envelope_edit::insert(&mut points, tick, value, view.geom.range);
        apply(session, view, target, points);
    }
}

#[allow(clippy::too_many_arguments)]
fn single_click(
    session: &mut Session,
    state: &mut UiState,
    view: &LaneView,
    target: EnvTarget,
    hit: EnvHit,
    pos: Pos2,
    m: Modifiers,
    grid: u64,
    snap: bool,
) {
    if state.arr.auto.draw {
        let width = grid as f64;
        let cell = (view.geom.axis.tick(pos.x) / width).floor().max(0.0) as i64;
        let cells: BTreeMap<i64, f64> = [(cell, plain_at(view, pos.y))].into();
        let stroke = envelope_edit::grid_stroke(&cells, width);
        let points = envelope_edit::overwrite(&view.points, &stroke, view.base, view.geom.range);
        apply(session, view, target, points);
        return;
    }
    let range = view.geom.range;
    match hit {
        EnvHit::Point(i) => {
            let mut points = view.points.clone();
            points.remove(i);
            apply(session, view, target, points);
        }
        EnvHit::Line(_) if !m.alt => {
            let tick = snap_tick(view.geom.axis.tick(pos.x), grid, snap);
            let value = envelope_edit::value_at(&view.points, tick, range)
                .or(view.dashed_knob().map(|k| range.from_normalized(k)));
            if let Some(value) = value {
                let mut points = view.points.clone();
                envelope_edit::insert(&mut points, tick, value, range);
                apply(session, view, target, points);
            }
        }
        EnvHit::Near(_) | EnvHit::Background => {
            background_click(session, state, pos, view, grid, snap)
        }
        EnvHit::Line(_) => {}
    }
}

fn background_click(
    session: &mut Session,
    state: &mut UiState,
    pos: Pos2,
    view: &LaneView,
    grid: u64,
    snap: bool,
) {
    state.arr.time = None;
    state.arr.auto.frame = None;
    if !session.is_playing() {
        session.seek(snap_tick(view.geom.axis.tick(pos.x), grid, snap));
    }
}

/// The parameter value under the pointer, as a small tag beside it.
pub(super) fn paint_value_tip(
    ui: &egui::Ui,
    session: &Session,
    state: &UiState,
    views: &[LaneView],
    lanes_area: Rect,
) {
    let Some(pos) = ui
        .input(|i| i.pointer.hover_pos())
        .or_else(|| ui.input(|i| i.pointer.interact_pos()))
    else {
        return;
    };
    let Some(view) = views
        .iter()
        .find(|v| v.hit_rect.intersect(lanes_area).contains(pos))
    else {
        return;
    };
    let (Some(target), Some(range)) = (view.target, view.range()) else {
        return;
    };
    let hit = hit_test(&view.points, &view.geom, pos, view.dashed_knob());
    let dragging = state.arr.auto.dragging();
    let value = match hit {
        EnvHit::Point(i) => Some(view.points[i].value),
        EnvHit::Line(_) if dragging || !state.arr.auto.draw => {
            envelope_edit::value_at(&view.points, view.geom.axis.tick(pos.x), range)
                .or_else(|| view.dashed_knob().map(|k| range.from_normalized(k)))
        }
        EnvHit::Near(i) if !state.arr.auto.draw => {
            let (a, b) = (view.points[i], view.points[i + 1]);
            let closer = if (view.geom.axis.x(a.time) - pos.x).abs()
                <= (view.geom.axis.x(b.time) - pos.x).abs()
            {
                a
            } else {
                b
            };
            Some(closer.value)
        }
        _ => None,
    };
    if let Some(value) = value {
        let text = session.env_format(view.key.chain, target, value);
        super::popups::value_tag(ui, pos + egui::vec2(10.0, -18.0), &text);
    }
    guide_line(ui, state, view, lanes_area, pos);
}

/// A thin vertical line at a dragged point, to read it against the grid.
fn guide_line(ui: &egui::Ui, state: &UiState, view: &LaneView, lanes_area: Rect, pos: Pos2) {
    if !matches!(state.arr.auto.gesture, Some(Gesture::Points { .. })) {
        return;
    }
    ui.painter_at(view.rect.intersect(lanes_area)).line_segment(
        [
            Pos2::new(pos.x, view.rect.top()),
            Pos2::new(pos.x, view.rect.bottom()),
        ],
        egui::Stroke::new(1.0, super::super::theme::pal().separator),
    );
}
