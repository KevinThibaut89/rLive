//! The MPE tab (manual ch. 12): each note's Pitch, Slide and Pressure envelopes, and its Velocity
//! and Release Velocity markers. Pitch is drawn on the notes; the rest have lanes below them.
//!
//! The arithmetic is in [`rlive_project::mpe_edit`] and the hit-testing is shared with the
//! automation lanes; what the pointer does is in `mpe_input`.

use super::lanes::paint_markers;
use super::mpe_input::{self, EnvGesture, MenuHit, Typing};
use super::{Geo, Shown, KEYBOARD_W};
use crate::session::Session;
use crate::ui::automation::geom::{segment_path, LaneGeom};
use crate::ui::theme::*;
use crate::ui::timeline::TimeAxis;
use crate::ui::widgets::{info, state_button};
use crate::ui::UiState;
use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, UiBuilder, Vec2};
use rlive_project::mpe_edit::{Dim, BEND_RANGE};
use rlive_project::note_edit::LaneValue;
use rlive_project::{Breakpoint, NoteData};

/// What a lane under the notes shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Lane {
    Envelope(Dim),
    Marker(LaneValue),
}

const LANES: [(Lane, &str); 4] = [
    (Lane::Envelope(Dim::Slide), "Slide"),
    (Lane::Envelope(Dim::Pressure), "Pressure"),
    (Lane::Marker(LaneValue::Velocity), "Velocity"),
    (Lane::Marker(LaneValue::Release), "Release"),
];

/// Which note and envelope the pointer last chose.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EnvSelection {
    pub note: usize,
    pub dim: Dim,
    pub from: f64,
    pub to: f64,
}

pub struct MpeState {
    /// Slide, Pressure, Velocity, Release Velocity (manual p. 331).
    pub shown: [bool; 4],
    pub heights: [f32; 4],
    pub hide_all: bool,
    /// The envelope last clicked, which Select All takes.
    pub focus_dim: Dim,
    /// The last click was outside the note, so a drag on an envelope scales it (manual p. 335).
    pub scale_mode: bool,
    pub selection: Option<EnvSelection>,
    pub(super) gesture: Option<EnvGesture>,
    pub(super) menu: Option<MenuHit>,
    pub(super) typing: Option<Typing>,
    /// The envelope a drag would scale, drawn blue.
    pub(super) scale_hover: Option<Dim>,
}

impl Default for MpeState {
    fn default() -> Self {
        Self {
            shown: [true, true, false, false],
            heights: [52.0; 4],
            hide_all: false,
            focus_dim: Dim::Pitch,
            scale_mode: false,
            selection: None,
            gesture: None,
            menu: None,
            typing: None,
            scale_hover: None,
        }
    }
}

impl MpeState {
    pub fn total_height(&self) -> f32 {
        if self.hide_all {
            return 0.0;
        }
        (0..4)
            .filter(|i| self.shown[*i])
            .map(|i| self.heights[i])
            .sum()
    }

    /// Whether a drag or stroke is in progress.
    pub fn busy(&self) -> bool {
        self.gesture.is_some()
    }

    /// The triangular toggle: hides the lanes, or shows them (all of them when none was on).
    pub fn toggle_lanes(&mut self, show_all: bool) {
        let none_on = !self.shown.iter().any(|s| *s);
        if show_all {
            self.shown = [true; 4];
            self.hide_all = false;
        } else if self.hide_all {
            self.hide_all = false;
            if none_on {
                self.shown = [true; 4];
            }
        } else if none_on {
            self.shown = [true; 4];
        } else {
            self.hide_all = true;
        }
    }
}

/// Fold Mode hides Pitch (manual p. 335), and leaves no even rows to draw it on.
pub(super) fn pitch_hidden(state: &UiState) -> bool {
    state.piano.fold_notes || state.piano.fold_scale
}

fn axis(geo: &Geo, at: f64) -> TimeAxis {
    TimeAxis {
        left: geo.x(at),
        px_per_beat: geo.ppb,
        scroll: 0.0,
    }
}

/// Where a note's Pitch envelope is drawn: zero on the note, a semitone per key track.
pub(super) fn pitch_geom(geo: &Geo, n: &NoteData, at: f64) -> Option<LaneGeom> {
    let row = geo.row_of(n.key)?;
    let centre = geo.y(row) + geo.kh / 2.0;
    let half = BEND_RANGE as f32 * geo.kh + 4.0;
    Some(LaneGeom {
        rect: Rect::from_x_y_ranges(geo.grid.x_range(), centre - half..=centre + half),
        axis: axis(geo, at),
        range: Dim::Pitch.range(),
    })
}

/// Where a Slide or Pressure envelope is drawn.
pub(super) fn lane_geom(geo: &Geo, plot: Rect, dim: Dim, at: f64) -> LaneGeom {
    LaneGeom {
        rect: plot,
        axis: axis(geo, at),
        range: dim.range(),
    }
}

pub(super) fn first_at(s: &Shown, start: u64) -> f64 {
    let mut times = Vec::new();
    s.span.occurrences(start, &mut times);
    times.first().copied().unwrap_or(start as f64)
}

pub(super) fn dashed(dim: Dim) -> f64 {
    dim.range().to_normalized(dim.rest())
}

/// One envelope. `points` empty draws a dashed line at its rest value.
fn paint_envelope(
    p: &egui::Painter,
    points: &[Breakpoint],
    geom: &LaneGeom,
    dim: Dim,
    length: f64,
    color: Color32,
    knobs: bool,
) {
    let stroke = Stroke::new(1.3, color);
    let (x0, x1) = (geom.axis.x(0.0), geom.axis.x(length));
    if points.is_empty() {
        let y = geom.y(dashed(dim));
        p.add(Shape::dashed_line(
            &[Pos2::new(x0, y), Pos2::new(x1, y)],
            Stroke::new(1.0, color.gamma_multiply(0.5)),
            3.0,
            3.0,
        ));
        return;
    }
    let first = geom.pos(&points[0]);
    let last = geom.pos(&points[points.len() - 1]);
    p.line_segment([Pos2::new(x0, first.y), first], stroke);
    p.line_segment([last, Pos2::new(x1, last.y)], stroke);
    for i in 0..points.len() - 1 {
        let path = segment_path(points, i, geom);
        p.add(Shape::line(path, stroke));
    }
    if knobs {
        for q in points {
            p.circle_filled(geom.pos(q), 2.5, color);
        }
    }
}

/// Pitch envelopes on top of the notes: the selected notes' bright, the others dimmed.
pub(super) fn paint_pitch(
    ui: &egui::Ui,
    geo: &Geo,
    state: &UiState,
    session: &Session,
    shown: &[Shown],
    fg: usize,
) {
    if pitch_hidden(state) {
        return;
    }
    let p = ui.painter_at(geo.grid);
    for (si, s) in shown.iter().enumerate() {
        let notes = session.clip_at(s.r).map_or(&[][..], |c| c.notes());
        for (ni, n) in notes.iter().enumerate() {
            let points = Dim::Pitch.points(n);
            let picked = si == fg && state.piano.selected.contains(&ni);
            if points.is_empty() && !picked {
                continue;
            }
            let Some(geom) = pitch_geom(geo, n, first_at(s, n.start)) else {
                continue;
            };
            let color = if picked {
                pal().text
            } else {
                shade(0x777777).gamma_multiply(0.6)
            };
            paint_envelope(
                &p,
                points,
                &geom,
                Dim::Pitch,
                n.length as f64,
                color,
                picked,
            );
        }
    }
}

/// One of the lanes under the notes: its envelope or markers, and its header.
#[allow(clippy::too_many_arguments)]
fn paint_lane(
    ui: &egui::Ui,
    rect: Rect,
    plot: Rect,
    geo: &Geo,
    state: &UiState,
    session: &Session,
    s: &Shown,
    (lane, name): (Lane, &str),
) {
    let p = ui.painter_at(rect);
    p.rect_filled(plot, 0.0, shade(0x262626));
    p.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, pal().separator),
    );
    p.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(plot.left(), rect.bottom())),
        0.0,
        shade(0x303030),
    );
    p.text(
        rect.min + Vec2::new(3.0, 2.0),
        egui::Align2::LEFT_TOP,
        name,
        font(9.0),
        pal().text_dim,
    );
    let notes = session.clip_at(s.r).map_or(&[][..], |c| c.notes());
    match lane {
        Lane::Marker(what) => {
            let inner = plot.shrink2(Vec2::new(0.0, 4.0));
            paint_markers(&p, plot, inner, geo, s, notes, what, &state.piano.selected);
        }
        Lane::Envelope(dim) => {
            let plot_p = ui.painter_at(plot);
            for (ni, n) in notes.iter().enumerate() {
                let picked = state.piano.selected.contains(&ni);
                let points = dim.points(n);
                if points.is_empty() && !picked {
                    continue;
                }
                let geom = lane_geom(geo, plot, dim, first_at(s, n.start));
                let blue = picked && state.piano.mpe.scale_hover == Some(dim);
                let color = if blue {
                    pal().solo_blue
                } else if picked {
                    s.color
                } else {
                    shade(0x6a6a6a).gamma_multiply(0.6)
                };
                let knobs = picked && !blue;
                paint_envelope(&plot_p, points, &geom, dim, n.length as f64, color, knobs);
            }
        }
    }
}

/// The lanes under the notes, top to bottom.
pub(super) fn lanes(
    ui: &mut egui::Ui,
    area: Rect,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    s: &Shown,
) {
    let mut top = area.top();
    for (i, &(lane, name)) in LANES.iter().enumerate() {
        if !state.piano.mpe.shown[i] {
            continue;
        }
        let height = state.piano.mpe.heights[i];
        let rect =
            Rect::from_min_size(Pos2::new(area.left(), top), Vec2::new(area.width(), height));
        top += height;
        let plot = Rect::from_min_max(Pos2::new(geo.grid.left(), rect.top()), rect.max);
        paint_lane(ui, rect, plot, geo, state, session, s, (lane, name));
        match lane {
            Lane::Marker(what) => {
                let inner = plot.shrink2(Vec2::new(0.0, 4.0));
                super::lanes::marker_input(ui, plot, inner, geo, session, state, s, what, 10 + i);
            }
            Lane::Envelope(dim) => {
                mpe_input::lane(ui, plot, geo, session, state, s, dim);
            }
        }
        // The line between this lane and what's above it resizes it (manual p. 332).
        let split = Rect::from_center_size(rect.center_top(), Vec2::new(rect.width(), 4.0));
        let resp = ui.interact(split, ui.scope_id().with(("mpe_split", i)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resp.dragged() {
            let h = &mut state.piano.mpe.heights[i];
            *h = (*h - resp.drag_delta().y).clamp(24.0, 240.0);
        }
    }
}

/// The toggles above the lanes, which also takes the drag that resizes them all.
pub(super) fn bar(ui: &mut egui::Ui, rect: Rect, state: &mut UiState) {
    ui.painter().rect_filled(rect, 0.0, shade(0x303030));
    let grip = ui.interact(
        rect.shrink2(Vec2::new(KEYBOARD_W, 0.0)),
        ui.scope_id().with("mpe_bar"),
        Sense::drag(),
    );
    if grip.hovered() || grip.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if grip.dragged() {
        let m = &mut state.piano.mpe;
        let count = m.shown.iter().filter(|s| **s).count().max(1) as f32;
        let dy = grip.drag_delta().y / count;
        for h in &mut m.heights {
            *h = (*h - dy).clamp(24.0, 240.0);
        }
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(3.0, 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing = Vec2::new(3.0, 2.0);
    let hide = state.piano.mpe.hide_all;
    let label = if hide { ">" } else { "v" };
    let r = state_button(
        &mut child,
        Vec2::new(16.0, 15.0),
        !hide,
        pal().accent,
        label,
    );
    let alt = child.input(|i| i.modifiers.alt);
    if info(r, "Show/Hide Expression Editors — Alt-click shows them all").clicked() {
        state.piano.mpe.toggle_lanes(alt);
    }
    if hide {
        return;
    }
    for (i, (_, name)) in LANES.iter().enumerate() {
        let on = state.piano.mpe.shown[i];
        let r = state_button(&mut child, Vec2::new(58.0, 15.0), on, pal().accent, name);
        if info(r, "Show or hide this lane").clicked() {
            state.piano.mpe.shown[i] = !on;
        }
    }
}

/// The tag showing an expression value beside the pointer.
pub(super) fn value_tag(ui: &egui::Ui, pos: Pos2, text: &str) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::unique("mpe_value_tag"),
    ));
    let galley = painter.layout_no_wrap(text.to_string(), font(10.0), pal().text);
    let rect = Rect::from_min_size(pos, galley.size() + Vec2::new(8.0, 4.0));
    painter.rect_filled(rect, 2.0, pal().raised);
    painter.galley(rect.min + Vec2::new(4.0, 2.0), galley, pal().text);
}

/// Cmd+A in the MPE tab: every breakpoint of the envelope last clicked (manual p. 335).
pub fn select_all_breakpoints(session: &Session, state: &mut UiState) -> bool {
    let Some(r) = state.selected_clip else {
        return false;
    };
    let Some(&note) = state.piano.selected.first() else {
        return false;
    };
    let Some(n) = session.clip_at(r).and_then(|c| c.notes().get(note)) else {
        return false;
    };
    let dim = state.piano.mpe.focus_dim;
    state.piano.mpe.selection = Some(EnvSelection {
        note,
        dim,
        from: 0.0,
        to: n.length as f64,
    });
    true
}
