//! What the pointer does in the MPE tab: on the notes for Pitch, and in the lanes for Slide and
//! Pressure (manual pp. 333-336).

use super::mpe::{self, EnvSelection};
use super::paint::hit;
use super::{Geo, Shown, UiState};
use crate::session::{ClipRef, Session};
use crate::ui::automation::geom::{hit_test, parse_value, DragAccum, EnvHit, LaneGeom};
use crate::ui::keymap::{self, Platform};
use crate::ui::widgets::info;
use egui::{Modifiers, Pos2, Rect, Sense};
use rlive_core::PPQ;
use rlive_project::envelope_edit::{self, can_curve, curve_after_drag};
use rlive_project::mpe_edit::{self as edit, Dim};
use rlive_project::{Breakpoint, NoteData};
use std::collections::BTreeMap;

/// Draw Mode's cells without the grid, in screen points.
const SMOOTH_CELL_PX: f32 = 2.0;
/// How near a grid line or neighbouring point is close enough to snap, in screen points.
const SNAP_PX: f32 = 6.0;

/// A drag in progress.
pub(super) enum EnvGesture {
    Points {
        note: usize,
        dim: Dim,
        base: Vec<Breakpoint>,
        moved: Vec<usize>,
        grabbed: usize,
        accum: DragAccum,
    },
    Segment {
        note: usize,
        dim: Dim,
        base: Vec<Breakpoint>,
        i: usize,
        span: Option<(f64, f64)>,
        accum: DragAccum,
    },
    Curve {
        note: usize,
        dim: Dim,
        base: Vec<Breakpoint>,
        i: usize,
        dy: f32,
    },
    Select {
        note: usize,
        dim: Dim,
        anchor: f64,
    },
    Draw {
        note: usize,
        dim: Dim,
        base: Vec<Breakpoint>,
        before: f64,
        cells: BTreeMap<i64, f64>,
        last: Option<(i64, f64)>,
        width: f64,
        stepped: bool,
    },
    Scale {
        dim: Dim,
        base: Vec<(usize, Vec<Breakpoint>)>,
        height: f32,
        dy: f32,
    },
}

/// What was under the pointer when the context menu opened.
pub(super) struct MenuHit {
    note: Option<usize>,
    dim: Dim,
    hit: EnvHit,
    tick: f64,
    pos: Pos2,
}

/// An exact value being typed.
pub(super) struct Typing {
    pub(super) note: usize,
    pub(super) dim: Dim,
    /// The breakpoint being set; `None` adds a new one at `tick`.
    pub(super) point: Option<usize>,
    pub(super) tick: f64,
    pub(super) text: String,
    pub(super) pos: Pos2,
    pub(super) fresh: bool,
}

/// One envelope, on one note, as it stands on screen.
struct View {
    note: usize,
    dim: Dim,
    n: NoteData,
    geom: LaneGeom,
}

impl View {
    fn points(&self) -> &[Breakpoint] {
        self.dim.points(&self.n)
    }

    fn length(&self) -> f64 {
        self.n.length as f64
    }

    fn hit(&self, pos: Pos2) -> EnvHit {
        if self.dim == Dim::Pitch {
            // Outside the note in time, the envelope isn't there to be grabbed.
            let (a, b) = (self.geom.axis.x(0.0), self.geom.axis.x(self.length()));
            if pos.x < a - 6.0 || pos.x > b + 6.0 {
                return EnvHit::Background;
            }
        }
        hit_test(self.points(), &self.geom, pos, Some(mpe::dashed(self.dim)))
    }

    fn tick(&self, x: f32) -> f64 {
        self.geom.axis.tick(x).clamp(0.0, self.length())
    }

    fn value_at_y(&self, y: f32) -> f64 {
        self.geom.range.from_normalized(self.geom.knob(y))
    }
}

/// What grid and modifiers mean for a gesture.
#[derive(Clone, Copy)]
struct Snapping {
    grid_on: bool,
    step: u64,
    /// Pitch rounds to semitones (manual p. 335).
    semitone: bool,
    /// Ticks in `SNAP_PX`.
    reach: f64,
}

fn snapping(session: &Session, r: ClipRef, m: Modifiers, ppb: f32) -> Snapping {
    let grid = session.clip_at(r).map(|c| c.mpe_grid).unwrap_or_default();
    Snapping {
        grid_on: grid.on,
        step: grid.step,
        semitone: keymap::ignores_grid(m, Platform::current()) != grid.on,
        reach: SNAP_PX as f64 / ppb.max(1.0) as f64 * PPQ as f64,
    }
}

fn view_of(session: &Session, s: &Shown, note: usize, dim: Dim, g: &Geometry) -> Option<View> {
    let n = session.clip_at(s.r)?.notes().get(note)?.clone();
    let at = mpe::first_at(s, n.start);
    let geom = match g {
        Geometry::Notes(geo) => mpe::pitch_geom(geo, &n, at)?,
        Geometry::Lane(geo, plot) => mpe::lane_geom(geo, *plot, dim, at),
    };
    Some(View { note, dim, n, geom })
}

/// Where the envelopes in question are.
enum Geometry<'a> {
    Notes(&'a Geo),
    Lane(&'a Geo, Rect),
}

fn store(session: &mut Session, r: ClipRef, note: usize, dim: Dim, points: Vec<Breakpoint>) {
    if let Some(n) = session
        .clip_at_mut(r)
        .and_then(|c| c.notes_mut())
        .and_then(|v| v.get_mut(note))
    {
        edit::set_points(n, dim, points);
    }
}

fn editable(state: &UiState) -> bool {
    state.piano.selected.len() == 1 && !state.piano.mpe.scale_mode
}

/// Fold hides Pitch envelopes, so there is nothing to grab on the notes (manual p. 335).
fn envelope_hidden(state: &UiState, dim: Dim) -> bool {
    dim == Dim::Pitch && mpe::pitch_hidden(state)
}

/// The note this position is about: `hit` on the notes, or the one sounding here in a lane.
fn note_under(
    session: &Session,
    s: &Shown,
    g: &Geometry,
    pos: Pos2,
    shown: &[Shown],
    fg: usize,
) -> Option<usize> {
    match g {
        Geometry::Notes(geo) => hit(geo, shown, fg, true, pos).map(|(_, n, _)| n),
        Geometry::Lane(geo, _) => {
            let notes = session.clip_at(s.r)?.notes();
            let t = geo.tick(pos.x);
            notes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    let at = mpe::first_at(s, n.start);
                    t >= at && t <= at + n.length as f64
                })
                .map(|(i, _)| i)
                .next()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn click(
    session: &mut Session,
    state: &mut UiState,
    s: &Shown,
    g: &Geometry,
    dim: Dim,
    pos: Pos2,
    m: Modifiers,
    shown: &[Shown],
    fg: usize,
) {
    let r = s.r;
    let snap = snapping(session, r, m, ppb_of(g));
    if state.arr.auto.draw && !envelope_hidden(state, dim) {
        let target = state.piano.selected.first().copied();
        let target = target.or_else(|| note_under(session, s, g, pos, shown, fg));
        if let Some(view) = target.and_then(|n| view_of(session, s, n, dim, g)) {
            let points = draw_step(&view, pos, snap, session.clip_at(r));
            session.edit_notes(r, "Draw Expression", |n| {
                if let Some(x) = n.get_mut(view.note) {
                    edit::set_points(x, dim, points.clone());
                }
            });
        }
        return;
    }
    if editable(state) && !envelope_hidden(state, dim) {
        let note = state.piano.selected[0];
        if let Some(view) = view_of(session, s, note, dim, g) {
            match view.hit(pos) {
                EnvHit::Point(i) if !m.shift => {
                    let points = edit::delete(view.points(), i);
                    session.edit_notes(r, "Delete Breakpoint", |n| {
                        edit::set_points(&mut n[note], dim, points.clone());
                    });
                    state.piano.mpe.selection = None;
                    return;
                }
                EnvHit::Line(_) if !m.shift && !m.alt => {
                    let t = new_point_tick(&view, pos.x, snap);
                    let value = dim.value_at(view.points(), t);
                    let (points, _) = edit::insert(view.points(), t, value, dim);
                    session.edit_notes(r, "Add Breakpoint", |n| {
                        edit::set_points(&mut n[note], dim, points.clone());
                    });
                    return;
                }
                EnvHit::Line(Some(i)) | EnvHit::Near(i) => {
                    let p = view.points();
                    state.piano.mpe.selection = Some(EnvSelection {
                        note,
                        dim,
                        from: p[i].time,
                        to: p[i + 1].time,
                    });
                    state.piano.mpe.focus_dim = dim;
                    return;
                }
                _ => {}
            }
        }
    }
    let ps = &mut state.piano;
    match note_under(session, s, g, pos, shown, fg) {
        Some(n) => {
            ps.mpe.focus_dim = dim;
            ps.mpe.scale_mode = false;
            ps.mpe.selection = None;
            if m.shift {
                if let Some(at) = ps.selected.iter().position(|x| *x == n) {
                    ps.selected.remove(at);
                } else {
                    ps.selected.push(n);
                }
            } else {
                ps.selected = vec![n];
                ps.others.clear();
            }
        }
        None => {
            // Outside a note: first its envelopes scale, and a second click leaves it.
            if ps.mpe.scale_mode || ps.selected.is_empty() {
                ps.selected.clear();
                ps.mpe.scale_mode = false;
            } else {
                ps.mpe.scale_mode = true;
            }
            ps.mpe.selection = None;
        }
    }
}

fn ppb_of(g: &Geometry) -> f32 {
    match g {
        Geometry::Notes(geo) | Geometry::Lane(geo, _) => geo.ppb,
    }
}

fn new_point_tick(view: &View, x: f32, snap: Snapping) -> f64 {
    let t = view.tick(x);
    if snap.grid_on {
        edit::snap_to_grid(t, view.n.start, snap.step, snap.reach)
    } else {
        t
    }
}

#[allow(clippy::too_many_arguments)]
fn start_drag(
    session: &mut Session,
    state: &mut UiState,
    s: &Shown,
    g: &Geometry,
    dim: Dim,
    pos: Pos2,
    m: Modifiers,
    shown: &[Shown],
    fg: usize,
) {
    if envelope_hidden(state, dim) {
        return;
    }
    let snap = snapping(session, s.r, m, ppb_of(g));
    if state.arr.auto.draw {
        let target = state
            .piano
            .selected
            .first()
            .copied()
            .or_else(|| note_under(session, s, g, pos, shown, fg));
        let Some(view) = target.and_then(|n| view_of(session, s, n, dim, g)) else {
            return;
        };
        let temporary = keymap::ignores_grid(m, Platform::current());
        let stepped = snap.grid_on != temporary;
        let width = if stepped {
            session.clip_at(s.r).map_or(PPQ / 4, |c| c.mpe_grid.step) as f64
        } else {
            view.geom.axis.tick(view.geom.axis.left + SMOOTH_CELL_PX)
                - view.geom.axis.tick(view.geom.axis.left)
        };
        let before = dim.value_at(view.points(), 0.0);
        state.piano.mpe.gesture = Some(EnvGesture::Draw {
            note: view.note,
            dim,
            base: view.points().to_vec(),
            before,
            cells: BTreeMap::new(),
            last: None,
            width: width.max(1.0),
            stepped,
        });
        state.piano.selected = vec![view.note];
        return;
    }
    let multi = state.piano.selected.len() > 1;
    if !editable(state) || multi {
        if dim != Dim::Pitch && !state.piano.selected.is_empty() {
            let base: Vec<(usize, Vec<Breakpoint>)> = state
                .piano
                .selected
                .iter()
                .filter_map(|i| {
                    let n = session.clip_at(s.r)?.notes().get(*i)?;
                    Some((*i, dim.points(n).to_vec()))
                })
                .collect();
            let height = match g {
                Geometry::Lane(_, plot) => plot.height(),
                Geometry::Notes(_) => 1.0,
            };
            state.piano.mpe.gesture = Some(EnvGesture::Scale {
                dim,
                base,
                height,
                dy: 0.0,
            });
        }
        return;
    }
    let note = state.piano.selected[0];
    let Some(view) = view_of(session, s, note, dim, g) else {
        return;
    };
    state.piano.mpe.focus_dim = dim;
    let range = view.geom.range;
    let points = view.points().to_vec();
    let hit = view.hit(pos);
    let sel = state
        .piano
        .mpe
        .selection
        .filter(|x| x.note == note && x.dim == dim);
    state.piano.mpe.gesture = match hit {
        EnvHit::Point(i) => {
            let moved: Vec<usize> = match sel {
                Some(x) if points[i].time >= x.from && points[i].time <= x.to => (0..points.len())
                    .filter(|j| points[*j].time >= x.from && points[*j].time <= x.to)
                    .collect(),
                _ => vec![i],
            };
            Some(EnvGesture::Points {
                note,
                dim,
                base: points,
                moved,
                grabbed: i,
                accum: DragAccum::default(),
            })
        }
        EnvHit::Line(Some(i)) | EnvHit::Near(i) if m.alt && can_curve(&points, i, range) => {
            Some(EnvGesture::Curve {
                note,
                dim,
                base: points,
                i,
                dy: 0.0,
            })
        }
        EnvHit::Line(Some(i)) if m.shift => Some(segment(note, dim, points, i, sel)),
        EnvHit::Near(i) if !m.alt => Some(segment(note, dim, points, i, sel)),
        EnvHit::Line(_) if !m.alt => {
            // Grabbing the line makes a breakpoint and goes on dragging it.
            let t = new_point_tick(&view, pos.x, snap);
            let value = dim.value_at(&points, t);
            let (base, at) = edit::insert(&points, t, value, dim);
            Some(EnvGesture::Points {
                note,
                dim,
                base,
                moved: vec![at],
                grabbed: at,
                accum: DragAccum::default(),
            })
        }
        EnvHit::Background => {
            let anchor = view.tick(pos.x);
            state.piano.mpe.selection = Some(EnvSelection {
                note,
                dim,
                from: anchor,
                to: anchor,
            });
            Some(EnvGesture::Select { note, dim, anchor })
        }
        _ => None,
    };
}

fn segment(
    note: usize,
    dim: Dim,
    base: Vec<Breakpoint>,
    i: usize,
    sel: Option<EnvSelection>,
) -> EnvGesture {
    let mid = (base[i].time + base[i + 1].time) / 2.0;
    let span = sel
        .filter(|x| mid >= x.from && mid <= x.to && x.to > x.from)
        .map(|x| (x.from, x.to));
    EnvGesture::Segment {
        note,
        dim,
        base,
        i,
        span,
        accum: DragAccum::default(),
    }
}

/// Points of `base` not in `moved`, plus the note's ends: where a drag can snap to.
fn neighbours(base: &[Breakpoint], moved: &[usize], length: f64) -> Vec<f64> {
    let mut targets: Vec<f64> = base
        .iter()
        .enumerate()
        .filter(|(i, _)| !moved.contains(i))
        .map(|(_, p)| p.time)
        .collect();
    targets.extend([0.0, length]);
    targets
}

#[allow(clippy::too_many_arguments)]
fn update_drag(
    session: &mut Session,
    state: &mut UiState,
    r: ClipRef,
    resp: &egui::Response,
    g: &Geometry,
    s: &Shown,
    m: Modifiers,
    delta: egui::Vec2,
) {
    let pointer = resp.interact_pointer_pos();
    let snap = snapping(session, r, m, ppb_of(g));
    let per_tick = PPQ as f64 / ppb_of(g).max(1.0) as f64;
    let Some(gesture) = state.piano.mpe.gesture.as_mut() else {
        return;
    };
    let view_for = |session: &Session, note: usize, dim: Dim| view_of(session, s, note, dim, g);
    match gesture {
        EnvGesture::Points {
            note,
            dim,
            base,
            moved,
            grabbed,
            accum,
        } => {
            accum.apply(delta, m.shift);
            let Some(view) = view_for(session, *note, *dim) else {
                return;
            };
            let mut dt = accum.dx as f64 * per_tick;
            if snap.grid_on {
                let targets = neighbours(base, moved, view.length());
                let at = base[*grabbed].time + dt;
                dt = edit::snap_to(at, targets, snap.reach) - base[*grabbed].time;
            }
            let span = view.geom.range.max - view.geom.range.min;
            let mut dv = view.geom.knob_per_px(accum.dy) * span;
            if *dim == Dim::Pitch && snap.semitone {
                let at = base[*grabbed].value + dv;
                dv = edit::snap_semitone(at) - base[*grabbed].value;
            }
            let points = edit::move_points(base, moved, dt, dv, *dim, view.length());
            store(session, r, *note, *dim, points);
        }
        EnvGesture::Segment {
            note,
            dim,
            base,
            i,
            span,
            accum,
        } => {
            accum.apply(delta, m.shift);
            let Some(view) = view_for(session, *note, *dim) else {
                return;
            };
            let mut dt = accum.dx as f64 * per_tick;
            if snap.grid_on {
                let targets = neighbours(base, &[*i, *i + 1], view.length());
                let at = base[*i].time + dt;
                dt = edit::snap_to(at, targets, snap.reach) - base[*i].time;
            }
            let reach = view.geom.range.max - view.geom.range.min;
            let dv = view.geom.knob_per_px(accum.dy) * reach;
            let points = edit::move_segment(base, *i, *span, dt, dv, *dim, view.length());
            store(session, r, *note, *dim, points);
        }
        EnvGesture::Curve {
            note,
            dim,
            base,
            i,
            dy,
        } => {
            *dy += delta.y;
            let (a, b) = (base[*i], base[*i + 1]);
            let mut points = base.clone();
            points[*i].curve = curve_after_drag(a.curve, b.value > a.value, *dy);
            store(session, r, *note, *dim, points);
        }
        EnvGesture::Select { note, dim, anchor } => {
            let Some(pos) = pointer else {
                return;
            };
            if let Some(view) = view_for(session, *note, *dim) {
                let t = view.tick(pos.x);
                state.piano.mpe.selection = Some(EnvSelection {
                    note: *note,
                    dim: *dim,
                    from: anchor.min(t),
                    to: anchor.max(t),
                });
            }
        }
        EnvGesture::Draw {
            note,
            dim,
            base,
            before,
            cells,
            last,
            width,
            stepped,
        } => {
            let Some(pos) = pointer else {
                return;
            };
            let Some(view) = view_for(session, *note, *dim) else {
                return;
            };
            let cell = (view.tick(pos.x) / *width).floor().max(0.0) as i64;
            let mut value = view.value_at_y(pos.y);
            if *dim == Dim::Pitch && snap.semitone != *stepped {
                value = edit::snap_semitone(value);
            }
            envelope_edit::paint_cells(cells, *last, (cell, value));
            *last = Some((cell, value));
            let stroke = if *stepped {
                envelope_edit::grid_stroke(cells, *width)
            } else {
                envelope_edit::smooth_stroke(cells, *width)
            };
            let length = view.length();
            let clipped: Vec<Breakpoint> =
                stroke.into_iter().filter(|p| p.time <= length).collect();
            let points = envelope_edit::overwrite(base, &clipped, *before, view.geom.range);
            store(session, r, *note, *dim, points);
        }
        EnvGesture::Scale {
            dim,
            base,
            height,
            dy,
        } => {
            *dy += delta.y;
            let factor = (1.0 - f64::from(*dy / *height)).max(0.0);
            let dim = *dim;
            let base = base.clone();
            for (note, points) in base {
                store(
                    session,
                    r,
                    note,
                    dim,
                    edit::scale_values(&points, factor, dim),
                );
            }
        }
    }
}

/// What a single click paints in Draw Mode: one step at the pointer.
fn draw_step(
    view: &View,
    pos: Pos2,
    snap: Snapping,
    clip: Option<&rlive_project::Clip>,
) -> Vec<Breakpoint> {
    let width = clip.map_or(PPQ / 4, |c| c.mpe_grid.step) as f64;
    let cell = (view.tick(pos.x) / width).floor().max(0.0) as i64;
    let mut value = view.value_at_y(pos.y);
    if view.dim == Dim::Pitch && snap.semitone {
        value = edit::snap_semitone(value);
    }
    let stroke = envelope_edit::grid_stroke(&[(cell, value)].into(), width);
    envelope_edit::overwrite(
        view.points(),
        &stroke,
        view.dim.value_at(view.points(), 0.0),
        view.geom.range,
    )
}

fn finish(session: &mut Session, state: &mut UiState, r: ClipRef) {
    let Some(gesture) = state.piano.mpe.gesture.take() else {
        return;
    };
    let label = match gesture {
        EnvGesture::Points { .. } => "Move Breakpoint",
        EnvGesture::Segment { .. } => "Move Segment",
        EnvGesture::Curve { .. } => "Curve Segment",
        EnvGesture::Draw { .. } => "Draw Expression",
        EnvGesture::Scale { .. } => "Scale Expression",
        EnvGesture::Select { .. } => return,
    };
    session.history.pending = Some(label);
    session.sync_ref(r);
}

fn hover_values(
    ui: &egui::Ui,
    session: &Session,
    state: &mut UiState,
    s: &Shown,
    g: &Geometry,
    dim: Dim,
    pos: Pos2,
) {
    if !editable(state) || envelope_hidden(state, dim) {
        return;
    }
    let note = state.piano.selected[0];
    let Some(view) = view_of(session, s, note, dim, g) else {
        return;
    };
    let value = match view.hit(pos) {
        EnvHit::Point(i) => Some(view.points()[i].value),
        EnvHit::Line(_) => Some(dim.value_at(view.points(), view.tick(pos.x))),
        EnvHit::Near(_) if state.piano.mpe.busy() => {
            Some(dim.value_at(view.points(), view.tick(pos.x)))
        }
        _ => None,
    };
    if let Some(v) = value {
        mpe::value_tag(ui, pos + egui::vec2(10.0, -18.0), &dim.format(v));
    }
}

#[allow(clippy::too_many_arguments)]
fn envelope_response(
    ui: &mut egui::Ui,
    resp: &egui::Response,
    session: &mut Session,
    state: &mut UiState,
    s: &Shown,
    g: &Geometry,
    dim: Dim,
    shown: &[Shown],
    fg: usize,
) {
    let (m, origin) = ui.input(|i| (i.modifiers, i.pointer.press_origin()));
    if resp.drag_started() {
        let pos = origin.or(resp.interact_pointer_pos());
        if let Some(pos) = pos {
            start_drag(session, state, s, g, dim, pos, m, shown, fg);
        }
    }
    if resp.dragged() {
        // The first frame counts from the press, not from where the drag was noticed.
        let delta = match (resp.drag_started(), origin, resp.interact_pointer_pos()) {
            (true, Some(from), Some(pos)) => pos - from,
            _ => resp.drag_delta(),
        };
        update_drag(session, state, s.r, resp, g, s, m, delta);
    }
    if resp.drag_stopped() {
        finish(session, state, s.r);
    }
    if resp.double_clicked() && m.alt && editable(state) {
        // Option or Alt double-click straightens a curved segment.
        if let Some(pos) = resp.interact_pointer_pos() {
            let note = state.piano.selected[0];
            if let Some(view) = view_of(session, s, note, dim, g) {
                if let EnvHit::Line(Some(i)) | EnvHit::Near(i) = view.hit(pos) {
                    let mut points = view.points().to_vec();
                    points[i].curve = 0.0;
                    session.edit_notes(s.r, "Straighten Segment", |n| {
                        edit::set_points(&mut n[note], dim, points.clone());
                    });
                }
            }
        }
    } else if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            click(session, state, s, g, dim, pos, m, shown, fg);
        }
    }
    if resp.secondary_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let note = if editable(state) && !envelope_hidden(state, dim) {
                state.piano.selected.first().copied()
            } else {
                None
            };
            let view = note.and_then(|n| view_of(session, s, n, dim, g));
            state.piano.mpe.menu = Some(MenuHit {
                note,
                dim,
                hit: view.as_ref().map_or(EnvHit::Background, |v| v.hit(pos)),
                tick: view.as_ref().map_or(0.0, |v| v.tick(pos.x)),
                pos,
            });
        }
    }
    resp.context_menu(|ui| menu(ui, session, state, s));
    if let Some(pos) = resp.hover_pos() {
        hover_values(ui, session, state, s, g, dim, pos);
        let can_scale = !editable(state) && !state.piano.selected.is_empty() && dim != Dim::Pitch;
        state.piano.mpe.scale_hover = can_scale.then_some(dim);
    } else if !resp.dragged() && state.piano.mpe.scale_hover == Some(dim) {
        state.piano.mpe.scale_hover = None;
    }
}

fn menu(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, s: &Shown) {
    if let Some(hit) = &state.piano.mpe.menu {
        let (dim, tick, pos, point) = (
            hit.dim,
            hit.tick,
            hit.pos,
            match hit.hit {
                EnvHit::Point(i) => Some(i),
                _ => None,
            },
        );
        let label = match hit.hit {
            EnvHit::Point(_) => Some("Edit Value"),
            EnvHit::Line(_) | EnvHit::Near(_) => Some("Add Value"),
            EnvHit::Background => None,
        };
        if let (Some(note), Some(label)) = (hit.note, label) {
            if ui.button(label).clicked() {
                let current = point.and_then(|i| point_value(session, s, note, dim, i));
                state.piano.mpe.typing = Some(Typing {
                    note,
                    dim,
                    point,
                    tick,
                    text: current.map_or_else(String::new, |v| typed_text(dim, v)),
                    pos,
                    fresh: true,
                });
                ui.close();
            }
        }
    }
    if ui.button("Clear All Envelopes").clicked() {
        let selected = state.piano.selected.clone();
        session.edit_notes(s.r, "Clear All Envelopes", |n| {
            edit::clear_envelopes(n, &selected);
        });
        state.piano.mpe.selection = None;
        ui.close();
    }
    let grid_on = session.clip_at(s.r).is_some_and(|c| c.mpe_grid.on);
    if ui.selectable_label(grid_on, "Snap to Grid").clicked() {
        set_grid(session, s.r, !grid_on);
        ui.close();
    }
    if ui
        .selectable_label(state.arr.auto.draw, "Draw Mode")
        .clicked()
    {
        state.arr.auto.draw = !state.arr.auto.draw;
        ui.close();
    }
}

/// The MPE tab's own grid, kept with the clip.
pub fn set_grid(session: &mut Session, r: ClipRef, on: bool) {
    session.edit_clips(&[r], "MPE Grid", |c| c.mpe_grid.on = on);
}

fn point_value(session: &Session, s: &Shown, note: usize, dim: Dim, i: usize) -> Option<f64> {
    let n = session.clip_at(s.r)?.notes().get(note)?;
    dim.points(n).get(i).map(|p| p.value)
}

fn typed_text(dim: Dim, v: f64) -> String {
    match dim {
        Dim::Pitch => format!("{v:.2}"),
        Dim::Slide | Dim::Pressure => format!("{:.0}", v * 127.0),
    }
}

pub(super) fn apply_typed(session: &mut Session, state: &mut UiState, r: ClipRef, t: &Typing) {
    let Some(typed) = parse_value(&t.text) else {
        return;
    };
    let value = t.dim.from_typed(typed);
    let Some(n) = session
        .clip_at(r)
        .and_then(|c| c.notes().get(t.note))
        .cloned()
    else {
        return;
    };
    let base = t.dim.points(&n).to_vec();
    let points = match t.point {
        Some(i) => {
            let sel = state
                .piano
                .mpe
                .selection
                .filter(|x| x.note == t.note && x.dim == t.dim);
            let group: Vec<usize> = match sel {
                Some(x) => (0..base.len())
                    .filter(|j| base[*j].time >= x.from && base[*j].time <= x.to)
                    .collect(),
                None => vec![i],
            };
            edit::set_value(&base, i, &group, value, t.dim)
        }
        None => edit::insert(&base, t.tick, value, t.dim).0,
    };
    let (note, dim) = (t.note, t.dim);
    session.edit_notes(r, "Edit Value", |v| {
        edit::set_points(&mut v[note], dim, points.clone());
    });
}

/// The field a value is typed into.
pub(super) fn typing_field(ui: &egui::Ui, session: &mut Session, state: &mut UiState, r: ClipRef) {
    let Some(mut t) = state.piano.mpe.typing.take() else {
        return;
    };
    let mut done = false;
    let mut submit = false;
    egui::Area::new(egui::Id::unique("mpe_edit_value"))
        .fixed_pos(t.pos)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut t.text).desired_width(56.0));
                if std::mem::take(&mut t.fresh) {
                    field.request_focus();
                }
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                submit = enter;
                done = enter || escape || field.lost_focus();
            });
        });
    if submit {
        apply_typed(session, state, r, &t);
    }
    if !done {
        state.piano.mpe.typing = Some(t);
    }
}

/// The notes grid in the MPE tab: Pitch envelopes on the notes.
pub(super) fn grid(
    ui: &mut egui::Ui,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    shown: &[Shown],
    fg: usize,
) {
    let resp = ui.interact(
        geo.grid,
        ui.scope_id().with("mpe_grid"),
        Sense::click_and_drag(),
    );
    let g = Geometry::Notes(geo);
    envelope_response(
        ui,
        &resp,
        session,
        state,
        &shown[fg],
        &g,
        Dim::Pitch,
        shown,
        fg,
    );
    info(
        resp,
        "MPE Editor — click a note to see its envelopes; click a breakpoint to remove it, the \
         line to add one; drag either to move them; Alt-drag a segment to curve it",
    );
}

/// A Slide or Pressure lane.
pub(super) fn lane(
    ui: &mut egui::Ui,
    plot: Rect,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    s: &Shown,
    dim: Dim,
) {
    let id = ui.scope_id().with(("mpe_lane", dim as usize));
    let resp = ui.interact(plot, id, Sense::click_and_drag());
    let g = Geometry::Lane(geo, plot);
    let list = [Shown {
        r: s.r,
        clip: s.clip.clone(),
        span: s.span,
        color: s.color,
    }];
    envelope_response(ui, &resp, session, state, s, &g, dim, &list, 0);
    info(
        resp,
        "Expression Lane — click a note to edit its envelope; click outside it, then drag here \
         to scale",
    );
}
