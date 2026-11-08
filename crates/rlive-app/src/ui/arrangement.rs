//! The Arrangement view: clips on a linear timeline, one lane per track.

use super::icons;
use super::session_view::{self, ClipDrag};
use super::theme::{self, *};
use super::timeline::{self, TimeAxis};
use super::widgets::{self, info, rgb};
use super::{BrowserItem, Detail, UiState};
use crate::session::{ArrMove, ChainRef, ClipRef, Session, TrimEdge};
use egui::{Align2, Color32, CursorIcon, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::{ArrClipId, TrackId, PPQ};
use rlive_project::{ArrangementClip, Clip, ClipContent, TrackKind};
use std::sync::atomic::Ordering;

const RULER_H: f32 = 28.0;
const LOOP_H: f32 = 11.0;
const HEADER_W: f32 = 176.0;
const SCROLLBAR: f32 = 8.0;
const CLIP_TITLE_H: f32 = 13.0;
/// Rows at the bottom for the return tracks and the Master.
const RETURN_ROW_H: f32 = 24.0;
const EDGE_W: f32 = 6.0;
const ADD_ZONE_H: f32 = 64.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Move,
    TrimLeft,
    TrimRight,
    Rubber,
}

struct ClipDragState {
    kind: DragKind,
    origin: Pos2,
    /// The clip grabbed, whose start snaps to the grid.
    anchor: Option<u64>,
    clips: Vec<(TrackId, ArrangementClip)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoopDrag {
    Whole,
    Start,
    End,
}

pub struct ArrangementState {
    pub px_per_beat: f32,
    pub scroll: Vec2,
    pub track_h: f32,
    pub selection: Vec<(TrackId, ArrClipId)>,
    drag: Option<ClipDragState>,
    loop_drag: Option<(LoopDrag, f32, u64, u64)>,
    context: Option<(TrackId, ArrClipId)>,
}

impl Default for ArrangementState {
    fn default() -> Self {
        Self {
            px_per_beat: 16.0,
            scroll: Vec2::ZERO,
            track_h: 54.0,
            selection: Vec::new(),
            drag: None,
            loop_drag: None,
            context: None,
        }
    }
}

/// One clip on screen this frame.
struct Hit {
    track: TrackId,
    id: ArrClipId,
    rect: Rect,
}

/// The vertical position of lane `i`.
fn lane_rect(lanes: Rect, i: usize, h: f32, scroll_y: f32) -> Rect {
    Rect::from_min_size(
        Pos2::new(lanes.left(), lanes.top() + i as f32 * h - scroll_y),
        Vec2::new(lanes.width(), h),
    )
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let full = ui.max_rect();
    ui.allocate_rect(full, Sense::hover());
    ui.painter().rect_filled(full, 0.0, GRID_BG);

    let sig = session.project.time_signature;
    let bar = sig.ticks_per_bar();
    let lanes_right = full.right() - HEADER_W;
    let ruler = Rect::from_min_max(full.min, Pos2::new(lanes_right, full.top() + RULER_H));
    // Return tracks and the Master stay at the bottom, under the scrolling lanes.
    let bottom_rows = session.project.returns.len() + 1;
    let bottom_h = bottom_rows as f32 * RETURN_ROW_H;
    let lanes_bottom = full.bottom() - SCROLLBAR - bottom_h;
    let lanes = Rect::from_min_max(
        Pos2::new(full.left(), ruler.bottom()),
        Pos2::new(lanes_right, lanes_bottom),
    );
    let headers = Rect::from_min_max(
        Pos2::new(lanes_right, ruler.bottom()),
        Pos2::new(full.right(), lanes_bottom),
    );

    wheel(ui, state, full, lanes);
    let visible = session.project.visible_tracks();
    let n = visible.len();
    let track_h = state.arr.track_h;
    let content_h = n as f32 * track_h + ADD_ZONE_H;
    let last = session
        .project
        .arrangement_end()
        .max(session.project.arrangement.loop_end);
    let content_beats = (last + 16 * bar) as f32 / PPQ as f32;
    let content_w = content_beats * state.arr.px_per_beat;
    let max_x = (content_w - lanes.width()).max(0.0);
    let max_y = (content_h - lanes.height()).max(0.0);
    state.arr.scroll.x = state.arr.scroll.x.clamp(0.0, max_x);
    state.arr.scroll.y = state.arr.scroll.y.clamp(0.0, max_y);

    let tick = session.engine.shared.tick.load();
    let playing = session.is_playing();
    follow_playhead(state, lanes, tick, playing);

    let axis = TimeAxis {
        left: lanes.left(),
        px_per_beat: state.arr.px_per_beat,
        scroll: state.arr.scroll.x,
    };
    let grid = timeline::adaptive_grid(axis.px_per_beat, bar);

    paint_lane_backgrounds(ui, session, state, lanes);
    paint_grid(ui, lanes, &axis, grid, bar);
    if session.project.arrangement.loop_enabled {
        let a = &session.project.arrangement;
        let (x0, x1) = (axis.x(a.loop_start as f64), axis.x(a.loop_end as f64));
        ui.painter_at(lanes).rect_filled(
            Rect::from_min_max(Pos2::new(x0, lanes.top()), Pos2::new(x1, lanes.bottom())),
            0.0,
            Color32::from_white_alpha(6),
        );
    }
    let hits = paint_clips(ui, session, state, lanes, &axis);
    lane_interaction(ui, session, state, lanes, &axis, grid, &hits);
    paint_ruler(ui, session, state, ruler, &axis, grid, bar);

    // The playhead and insert marker sit over everything in the lanes.
    let p = ui.painter_at(Rect::from_min_max(ruler.min, lanes.max));
    let marker = session.insert_marker();
    let mx = axis.x(marker);
    p.line_segment(
        [Pos2::new(mx, ruler.bottom()), Pos2::new(mx, lanes.bottom())],
        Stroke::new(1.0, Color32::from_white_alpha(90)),
    );
    if playing {
        let x = axis.x(tick);
        p.line_segment(
            [Pos2::new(x, ruler.top()), Pos2::new(x, lanes.bottom())],
            Stroke::new(1.0, hex(0xf0f0f0)),
        );
    }

    track_headers(ui, session, state, headers);
    return_rows(
        ui,
        session,
        state,
        Rect::from_min_max(
            Pos2::new(full.left(), lanes.bottom()),
            Pos2::new(full.right(), lanes.bottom() + bottom_h),
        ),
        lanes_right,
    );
    let add_y = lanes.top() + n as f32 * track_h - state.arr.scroll.y;
    if add_y < lanes.bottom() {
        let zone = Rect::from_min_max(
            Pos2::new(headers.left(), add_y),
            Pos2::new(headers.right(), lanes.bottom()),
        );
        session_view::new_track_zone(ui, session, state, zone);
    }
    let p = ui.painter();
    p.line_segment(
        [headers.left_top(), Pos2::new(headers.left(), full.bottom())],
        Stroke::new(1.0, SEPARATOR),
    );
    scrollbars(
        ui, state, lanes, headers, full, content_w, content_h, max_x, max_y,
    );
}

fn wheel(ui: &mut egui::Ui, state: &mut UiState, full: Rect, lanes: Rect) {
    if !ui.rect_contains_pointer(full) {
        return;
    }
    let (delta, m, pointer) =
        ui.input(|i| (i.smooth_scroll_delta, i.modifiers, i.pointer.hover_pos()));
    let a = &mut state.arr;
    if m.command && delta.y != 0.0 {
        let axis = TimeAxis {
            left: lanes.left(),
            px_per_beat: a.px_per_beat,
            scroll: a.scroll.x,
        };
        let new = (a.px_per_beat * (1.0 + delta.y * 0.004)).clamp(1.0, 240.0);
        a.scroll.x = axis.scroll_after_zoom(pointer.map_or(lanes.left(), |p| p.x), new);
        a.px_per_beat = new;
    } else if m.alt && delta.y != 0.0 {
        a.track_h = (a.track_h + delta.y * 0.2).clamp(30.0, 160.0);
    } else if m.shift {
        a.scroll.x -= delta.y + delta.x;
    } else {
        a.scroll.x -= delta.x;
        a.scroll.y -= delta.y;
    }
}

/// While playing, turn the page when the playhead leaves the view.
fn follow_playhead(state: &mut UiState, lanes: Rect, tick: f64, playing: bool) {
    if !playing || state.arr.drag.is_some() {
        return;
    }
    let a = &mut state.arr;
    let x = (tick / PPQ as f64) as f32 * a.px_per_beat;
    if x > a.scroll.x + lanes.width() - 24.0 || x < a.scroll.x {
        a.scroll.x = (x - lanes.width() * 0.1).max(0.0);
    }
}

fn paint_lane_backgrounds(ui: &egui::Ui, session: &Session, state: &UiState, lanes: Rect) {
    let p = ui.painter_at(lanes);
    let selected = match state.selected {
        Some(ChainRef::Track(t)) => Some(t),
        _ => None,
    };
    let visible = session.project.visible_tracks();
    for (i, t) in visible
        .iter()
        .map(|&v| &session.project.tracks[v])
        .enumerate()
    {
        let r = lane_rect(lanes, i, state.arr.track_h, state.arr.scroll.y);
        if r.bottom() < lanes.top() || r.top() > lanes.bottom() {
            continue;
        }
        let fill = if selected == Some(t.id) {
            hex(0x2e2e2e)
        } else if i % 2 == 0 {
            hex(0x282828)
        } else {
            hex(0x252525)
        };
        p.rect_filled(r, 0.0, fill);
        p.line_segment(
            [r.left_bottom(), r.right_bottom()],
            Stroke::new(1.0, SEPARATOR),
        );
    }
}

fn paint_grid(ui: &egui::Ui, lanes: Rect, axis: &TimeAxis, grid: u64, bar: u64) {
    let p = ui.painter_at(lanes);
    let first = (axis.tick(lanes.left()).max(0.0) as u64 / grid) * grid;
    let mut t = first;
    while axis.x(t as f64) <= lanes.right() {
        let x = axis.x(t as f64);
        let c = if t.is_multiple_of(bar) {
            hex(0x141414)
        } else {
            hex(0x202020)
        };
        p.line_segment(
            [Pos2::new(x, lanes.top()), Pos2::new(x, lanes.bottom())],
            Stroke::new(1.0, c),
        );
        t += grid;
    }
}

fn paint_clips(
    ui: &egui::Ui,
    session: &Session,
    state: &UiState,
    lanes: Rect,
    axis: &TimeAxis,
) -> Vec<Hit> {
    let p = ui.painter_at(lanes);
    let mut hits = Vec::new();
    let visible = session.project.visible_tracks();
    for (i, t) in visible
        .iter()
        .map(|&v| &session.project.tracks[v])
        .enumerate()
    {
        let lane = lane_rect(lanes, i, state.arr.track_h, state.arr.scroll.y);
        if lane.bottom() < lanes.top() || lane.top() > lanes.bottom() {
            continue;
        }
        if t.is_group() {
            paint_group_overview(&p, session, t.id, lane, lanes, axis);
        }
        for c in &t.arrangement {
            let (x0, x1) = (axis.x(c.start as f64), axis.x(c.end() as f64));
            if x1 < lanes.left() || x0 > lanes.right() {
                continue;
            }
            let rect = Rect::from_min_max(
                Pos2::new(x0, lane.top() + 1.0),
                Pos2::new(x1.max(x0 + 2.0), lane.bottom() - 1.0),
            );
            let selected = state.arr.selection.contains(&(t.id, c.id));
            paint_clip(&p, session, c, rect, lanes, axis, selected, t.mute);
            hits.push(Hit {
                track: t.id,
                id: c.id,
                rect,
            });
        }
    }
    hits
}

#[allow(clippy::too_many_arguments)]
fn paint_clip(
    p: &egui::Painter,
    session: &Session,
    c: &ArrangementClip,
    rect: Rect,
    lanes: Rect,
    axis: &TimeAxis,
    selected: bool,
    muted: bool,
) {
    let mut color = rgb(c.clip.color);
    if muted {
        color = theme::darken(color, 0.45);
    }
    let body = theme::darken(color, 0.55);
    p.rect_filled(rect, 1.0, body);
    let title = Rect::from_min_max(
        rect.min,
        Pos2::new(rect.right(), (rect.top() + CLIP_TITLE_H).min(rect.bottom())),
    );
    p.rect_filled(title, 1.0, color);
    let text_x = rect.left().max(lanes.left()) + 3.0;
    if rect.right() - text_x > 12.0 {
        let galley = p.layout_no_wrap(
            c.clip.name.clone(),
            theme::font(10.0),
            theme::text_on(color),
        );
        p.with_clip_rect(title.intersect(lanes)).galley(
            Pos2::new(text_x, title.center().y - galley.size().y / 2.0),
            galley,
            Color32::WHITE,
        );
    }
    let content = Rect::from_min_max(Pos2::new(rect.left(), title.bottom()), rect.max);
    if content.height() > 6.0 {
        match &c.clip.content {
            ClipContent::Midi { .. } => paint_notes(p, c, content, lanes, axis, color),
            ClipContent::Audio { file, .. } => {
                paint_waveform(p, session, c, &file.path, content, lanes, axis, color)
            }
        }
    }
    p.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0, SEPARATOR),
    );
    if selected {
        p.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0, Color32::WHITE),
            StrokeKind::Inside,
        );
    }
}

/// A group shows its members' clips as thin bars in its own lane.
fn paint_group_overview(
    p: &egui::Painter,
    session: &Session,
    group: TrackId,
    lane: Rect,
    lanes: Rect,
    axis: &TimeAxis,
) {
    let tracks = session.project.group_contents(group);
    let tracks: Vec<_> = tracks
        .iter()
        .filter_map(|id| session.project.track(*id))
        .filter(|t| !t.is_group())
        .collect();
    let rows = tracks.len().max(1) as f32;
    let row_h = ((lane.height() - 4.0) / rows).clamp(2.0, 9.0);
    for (row, t) in tracks.iter().enumerate() {
        let y = lane.top() + 2.0 + row as f32 * row_h;
        for c in &t.arrangement {
            let (x0, x1) = (axis.x(c.start as f64), axis.x(c.end() as f64));
            if x1 < lanes.left() || x0 > lanes.right() {
                continue;
            }
            let bar = Rect::from_min_max(
                Pos2::new(x0, y),
                Pos2::new(x1.max(x0 + 2.0), y + (row_h - 1.0).max(1.0)),
            );
            p.rect_filled(bar, 0.0, theme::darken(rgb(c.clip.color), 0.2));
        }
    }
}

/// Where the content at timeline tick `t` comes from, in clip-local ticks.
fn content_tick(c: &ArrangementClip, t: u64) -> Option<u64> {
    let local = c.offset + t.saturating_sub(c.start);
    if c.clip.looped && c.clip.length > 0 {
        Some(local % c.clip.length)
    } else {
        (local < c.clip.length).then_some(local)
    }
}

fn paint_notes(
    p: &egui::Painter,
    c: &ArrangementClip,
    content: Rect,
    lanes: Rect,
    axis: &TimeAxis,
    color: Color32,
) {
    let notes = c.clip.notes();
    let Some((lo, hi)) = notes.iter().fold(None, |acc: Option<(u8, u8)>, n| {
        Some(acc.map_or((n.key, n.key), |(l, h)| (l.min(n.key), h.max(n.key))))
    }) else {
        return;
    };
    let span = (hi - lo).max(11) as f32 + 1.0;
    let row = (content.height() - 2.0) / span;
    let length = c.clip.length.max(1);
    let shape_color = theme::darken(color, 0.15);
    // Draw each repeat of the content that falls inside the clip.
    let mut pass_start = c.start as i64 - (c.offset % length) as i64;
    let end = c.end() as i64;
    while pass_start < end {
        for n in notes {
            let on = pass_start + n.start as i64;
            let off = on + n.length as i64;
            if off <= c.start as i64 || on >= end {
                continue;
            }
            let x0 = axis.x(on.max(c.start as i64) as f64);
            let x1 = axis.x(off.min(end) as f64);
            if x1 < lanes.left() || x0 > lanes.right() {
                continue;
            }
            let y = content.bottom() - 1.0 - ((n.key - lo) as f32 + 1.0) * row;
            p.rect_filled(
                Rect::from_min_size(
                    Pos2::new(x0, y),
                    Vec2::new((x1 - x0).max(1.0), row.max(1.5)),
                ),
                0.0,
                shape_color,
            );
        }
        if !c.clip.looped {
            break;
        }
        pass_start += length as i64;
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_waveform(
    p: &egui::Painter,
    session: &Session,
    c: &ArrangementClip,
    path: &std::path::Path,
    content: Rect,
    lanes: Rect,
    axis: &TimeAxis,
    color: Color32,
) {
    let Some((peaks, sample_rate)) = session.samples.peaks(path) else {
        return;
    };
    let shape = theme::darken(color, 0.15);
    let mid = content.center().y;
    let half = content.height() / 2.0 - 1.0;
    let frames_per_tick = 60.0 / session.project.tempo / PPQ as f64 * sample_rate;
    let (from, to) = (
        content.left().max(lanes.left()) as i32,
        content.right().min(lanes.right()) as i32,
    );
    for px in from..to {
        let t0 = axis.tick(px as f32).max(c.start as f64) as u64;
        let t1 = axis.tick(px as f32 + 1.0).max(c.start as f64) as u64;
        let (Some(a), Some(b)) = (content_tick(c, t0), content_tick(c, t1.max(t0 + 1))) else {
            continue;
        };
        let (a, b) = if b > a { (a, b) } else { (a, a + 1) };
        let (lo, hi) = peaks.min_max(
            (a as f64 * frames_per_tick) as usize,
            (b as f64 * frames_per_tick) as usize + 1,
        );
        p.line_segment(
            [
                Pos2::new(px as f32 + 0.5, mid - hi.clamp(-1.0, 1.0) * half),
                Pos2::new(px as f32 + 0.5, mid - lo.clamp(-1.0, 1.0) * half + 1.0),
            ],
            Stroke::new(1.0, shape),
        );
    }
}

fn track_at(session: &Session, lanes: Rect, y: f32, h: f32, scroll_y: f32) -> Option<usize> {
    if y < lanes.top() {
        return None;
    }
    let i = ((y - lanes.top() + scroll_y) / h).floor() as usize;
    session.project.visible_tracks().get(i).copied()
}

fn lane_interaction(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    hits: &[Hit],
) {
    let resp = ui.interact(
        lanes,
        ui.scope_id().with("arr_lanes"),
        Sense::click_and_drag(),
    );
    let m = ui.input(|i| i.modifiers);
    let snapping = !m.alt;
    let hit = |pos: Pos2| hits.iter().rev().find(|h| h.rect.contains(pos));
    let edge = |h: &Hit, pos: Pos2| {
        if h.rect.width() >= EDGE_W * 2.5 && pos.x < h.rect.left() + EDGE_W {
            DragKind::TrimLeft
        } else if h.rect.width() >= EDGE_W * 2.5 && pos.x > h.rect.right() - EDGE_W {
            DragKind::TrimRight
        } else {
            DragKind::Move
        }
    };
    if let Some(pos) = resp.hover_pos() {
        if let Some(h) = hit(pos) {
            if edge(h, pos) != DragKind::Move {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
        }
    }

    let (h, sy) = (state.arr.track_h, state.arr.scroll.y);
    let pointer = resp.interact_pointer_pos();

    if resp.double_clicked() {
        if let Some(pos) = pointer {
            match hit(pos) {
                Some(hit) => open_clip(state, hit.track, hit.id, true),
                None => create_midi_clip(session, state, lanes, axis, grid, pos),
            }
        }
    } else if resp.clicked() {
        if let Some(pos) = pointer {
            if let Some(i) = track_at(session, lanes, pos.y, h, sy) {
                let id = session.project.tracks[i].id;
                state.selected = Some(ChainRef::Track(id));
                state.selected_tracks = vec![id];
            }
            match hit(pos) {
                Some(hit) => {
                    let key = (hit.track, hit.id);
                    if m.shift || m.command {
                        toggle(&mut state.arr.selection, key);
                    } else {
                        state.arr.selection = vec![key];
                    }
                    open_clip(state, hit.track, hit.id, false);
                }
                None => {
                    state.arr.selection.clear();
                    if !session.is_playing() {
                        let t = timeline::snap(axis.tick(pos.x), grid, snapping).max(0);
                        session.seek(t as f64);
                    }
                }
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(pos) = pointer {
            state.arr.context = hit(pos).map(|h| (h.track, h.id));
            if let Some(h) = hit(pos) {
                if !state.arr.selection.contains(&(h.track, h.id)) {
                    state.arr.selection = vec![(h.track, h.id)];
                }
            }
        }
    }
    resp.context_menu(|ui| clip_context_menu(ui, session, state));

    if resp.drag_started() {
        if let Some(pos) = pointer {
            state.arr.drag = Some(begin_drag(session, state, hit(pos), &edge, pos));
        }
    }

    let mut preview = Vec::new();
    if let (Some(d), Some(pos)) = (&state.arr.drag, pointer) {
        preview = drag_preview(session, state, d, pos, axis, grid, snapping, lanes);
    }
    let p = ui.painter_at(lanes);
    for ghost in &preview {
        p.rect_filled(ghost.rect, 1.0, Color32::from_white_alpha(45));
        p.rect_stroke(
            ghost.rect,
            1.0,
            Stroke::new(1.0, Color32::WHITE),
            StrokeKind::Inside,
        );
    }
    if let (Some(d), Some(pos)) = (&state.arr.drag, pointer) {
        if d.kind == DragKind::Rubber {
            let r = Rect::from_two_pos(d.origin, pos);
            p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(120, 180, 255, 40));
            p.rect_stroke(r, 0.0, Stroke::new(1.0, SELECT), StrokeKind::Inside);
            state.arr.selection = hits
                .iter()
                .filter(|h| h.rect.intersects(r))
                .map(|h| (h.track, h.id))
                .collect();
        }
    }
    if resp.drag_stopped() {
        if let Some(d) = state.arr.drag.take() {
            finish_drag(session, state, d, &preview, m.command);
        }
    }

    drops(session, state, &resp, lanes, axis, grid, snapping);
    info(
        resp,
        "Click to place the insert marker · drag clips to move (Cmd = copy, Alt = no snap) · drag edges to trim · double-click to edit or add · Cmd+wheel zooms · Alt+wheel sets lane height",
    );
}

fn toggle(selection: &mut Vec<(TrackId, ArrClipId)>, key: (TrackId, ArrClipId)) {
    match selection.iter().position(|s| *s == key) {
        Some(i) => {
            selection.remove(i);
        }
        None => selection.push(key),
    }
}

fn open_clip(state: &mut UiState, track: TrackId, id: ArrClipId, reveal: bool) {
    let cref = ClipRef::Arr { track, id };
    if state.selected_clip != Some(cref) {
        state.piano.reset_view = true;
        state.piano.selected.clear();
    }
    state.selected_clip = Some(cref);
    state.detail = Detail::Clip;
    if reveal {
        state.show_detail = true;
    }
}

fn create_midi_clip(
    session: &mut Session,
    state: &mut UiState,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    pos: Pos2,
) {
    let (h, sy) = (state.arr.track_h, state.arr.scroll.y);
    let Some(i) = track_at(session, lanes, pos.y, h, sy) else {
        return;
    };
    let t = &session.project.tracks[i];
    if t.kind != TrackKind::Midi {
        return;
    }
    let (id, color) = (t.id, t.color);
    let sig = session.project.time_signature;
    let start = ((axis.tick(pos.x) / grid as f64).floor().max(0.0) as u64) * grid;
    let placed = ArrangementClip::new(Clip::new_midi("MIDI Clip", color, 1, sig), start);
    let clip_id = placed.id;
    session.arr_place(id, placed);
    state.arr.selection = vec![(id, clip_id)];
    open_clip(state, id, clip_id, true);
}

fn begin_drag(
    session: &Session,
    state: &mut UiState,
    hit: Option<&Hit>,
    edge: &dyn Fn(&Hit, Pos2) -> DragKind,
    pos: Pos2,
) -> ClipDragState {
    let Some(h) = hit else {
        state.arr.selection.clear();
        return ClipDragState {
            kind: DragKind::Rubber,
            origin: pos,
            anchor: None,
            clips: Vec::new(),
        };
    };
    if !state.arr.selection.contains(&(h.track, h.id)) {
        state.arr.selection = vec![(h.track, h.id)];
    }
    let kind = edge(h, pos);
    let find = |t: TrackId, id: ArrClipId| session.project.track(t)?.arr_clip(id).cloned();
    let clips: Vec<(TrackId, ArrangementClip)> = if kind == DragKind::Move {
        state
            .arr
            .selection
            .iter()
            .filter_map(|(t, id)| find(*t, *id).map(|c| (*t, c)))
            .collect()
    } else {
        find(h.track, h.id)
            .map(|c| (h.track, c))
            .into_iter()
            .collect()
    };
    let anchor = find(h.track, h.id).map(|c| match kind {
        DragKind::TrimRight => c.end(),
        _ => c.start,
    });
    ClipDragState {
        kind,
        origin: pos,
        anchor,
        clips,
    }
}

/// Where a clip would land when the drag ends.
struct Ghost {
    rect: Rect,
    from: TrackId,
    to: TrackId,
    clip: ArrangementClip,
}

#[allow(clippy::too_many_arguments)]
fn drag_preview(
    session: &Session,
    state: &UiState,
    d: &ClipDragState,
    pos: Pos2,
    axis: &TimeAxis,
    grid: u64,
    snapping: bool,
    lanes: Rect,
) -> Vec<Ghost> {
    let Some(anchor) = d.anchor else {
        return Vec::new();
    };
    let target = anchor as f64 + ((pos.x - d.origin.x) / axis.px_per_beat) as f64 * PPQ as f64;
    let snapped = timeline::snap(target, grid, snapping).max(0);
    let mut dt = snapped - anchor as i64;
    let visible = session.project.visible_tracks();
    let tracks: Vec<&rlive_project::Track> = visible
        .iter()
        .map(|&v| &session.project.tracks[v])
        .collect();
    let index = |t: TrackId| tracks.iter().position(|o| o.id == t);
    let mut dlane = 0i32;
    if d.kind == DragKind::Move {
        let earliest = d.clips.iter().map(|(_, c)| c.start).min().unwrap_or(0) as i64;
        dt = dt.max(-earliest);
        dlane = ((pos.y - d.origin.y) / state.arr.track_h).round() as i32;
        let fits = d.clips.iter().all(|(t, _)| {
            index(*t).is_some_and(|i| {
                let to = i as i32 + dlane;
                to >= 0
                    && (to as usize) < tracks.len()
                    && tracks[to as usize].kind == tracks[i].kind
            })
        });
        if !fits {
            dlane = 0;
        }
    }
    d.clips
        .iter()
        .filter_map(|(t, c)| {
            let i = (index(*t)? as i32 + dlane) as usize;
            let to = tracks.get(i)?.id;
            let mut clip = c.clone();
            match d.kind {
                DragKind::Move => clip.start = (c.start as i64 + dt).max(0) as u64,
                DragKind::TrimLeft => clip.trim_left((c.start as i64 + dt).max(0) as u64),
                DragKind::TrimRight => clip.trim_right((c.end() as i64 + dt).max(0) as u64),
                DragKind::Rubber => return None,
            }
            let lane = lane_rect(lanes, i, state.arr.track_h, state.arr.scroll.y);
            let rect = Rect::from_min_max(
                Pos2::new(axis.x(clip.start as f64), lane.top() + 1.0),
                Pos2::new(axis.x(clip.end() as f64), lane.bottom() - 1.0),
            );
            Some(Ghost {
                rect,
                from: *t,
                to,
                clip,
            })
        })
        .collect()
}

fn finish_drag(
    session: &mut Session,
    state: &mut UiState,
    d: ClipDragState,
    preview: &[Ghost],
    copy: bool,
) {
    match d.kind {
        DragKind::Rubber => {}
        DragKind::Move => {
            let moves: Vec<ArrMove> = preview
                .iter()
                .map(|g| ArrMove {
                    track: g.from,
                    id: g.clip.id,
                    to_track: g.to,
                    start: g.clip.start,
                })
                .collect();
            let unchanged = !copy
                && preview
                    .iter()
                    .zip(&d.clips)
                    .all(|(g, (_, c))| g.from == g.to && g.clip.start == c.start);
            if !unchanged {
                state.arr.selection = session.arr_move(&moves, copy);
            }
        }
        DragKind::TrimLeft | DragKind::TrimRight => {
            for g in preview {
                let edge = if d.kind == DragKind::TrimLeft {
                    TrimEdge::Left(g.clip.start)
                } else {
                    TrimEdge::Right(g.clip.end())
                };
                session.arr_trim(g.from, g.clip.id, edge);
            }
        }
    }
}

fn clip_context_menu(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let Some((track, id)) = state.arr.context else {
        ui.close();
        return;
    };
    if ui.button("Edit").clicked() {
        open_clip(state, track, id, true);
        ui.close();
    }
    if ui.button("Duplicate").clicked() {
        let items = state.arr.selection.clone();
        state.arr.selection = session.arr_duplicate(&items);
        ui.close();
    }
    if ui.button("Delete").clicked() {
        let items = std::mem::take(&mut state.arr.selection);
        session.arr_delete(&items);
        ui.close();
    }
}

#[allow(clippy::too_many_arguments)]
fn drops(
    session: &mut Session,
    state: &mut UiState,
    resp: &egui::Response,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    snapping: bool,
) {
    let (h, sy) = (state.arr.track_h, state.arr.scroll.y);
    let hovering = resp.dnd_hover_payload::<BrowserItem>().is_some()
        || resp.dnd_hover_payload::<ClipDrag>().is_some();
    if !hovering {
        return;
    }
    let Some(pos) = resp.ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let Some(i) = track_at(session, lanes, pos.y, h, sy) else {
        return;
    };
    let target = session.project.tracks[i].id;
    let kind = session.project.tracks[i].kind;
    let start = timeline::snap(axis.tick(pos.x), grid, snapping).max(0) as u64;
    let x = axis.x(start as f64);
    let row = session
        .project
        .visible_tracks()
        .iter()
        .position(|&v| v == i)
        .unwrap_or(0);
    let lane = lane_rect(lanes, row, h, sy);
    resp.ctx.layer_painter(resp.layer_id).line_segment(
        [Pos2::new(x, lane.top()), Pos2::new(x, lane.bottom())],
        Stroke::new(2.0, ACCENT),
    );
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        match &*item {
            BrowserItem::Sample(path) if kind == TrackKind::Audio => {
                match session.audio_clip_from_file(path) {
                    Ok(c) => place_new(session, state, target, c, start),
                    Err(e) => state.status = e,
                }
            }
            other => session_view::drop_on_track(session, state, target, other, None),
        }
    }
    if let Some(src) = resp.dnd_release_payload::<ClipDrag>() {
        if let Some(c) = session.clip(src.track, src.slot).cloned() {
            if c.is_midi() == (kind == TrackKind::Midi) {
                place_new(session, state, target, c, start);
            } else {
                state.status = "That clip doesn't fit on this kind of track".into();
            }
        }
    }
}

fn place_new(session: &mut Session, state: &mut UiState, track: TrackId, clip: Clip, start: u64) {
    let placed = ArrangementClip::new(clip, start);
    let id = placed.id;
    session.arr_place(track, placed);
    state.arr.selection = vec![(track, id)];
    state.selected = Some(ChainRef::Track(track));
}

fn paint_ruler(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    ruler: Rect,
    axis: &TimeAxis,
    grid: u64,
    bar: u64,
) {
    let p = ui.painter_at(ruler);
    p.rect_filled(ruler, 0.0, hex(0x333333));
    let step = timeline::ruler_bar_step(axis.px_per_beat, bar) * bar;
    let first = (axis.tick(ruler.left()).max(0.0) as u64 / step) * step;
    let mut t = first;
    while axis.x(t as f64) <= ruler.right() {
        let x = axis.x(t as f64);
        p.line_segment(
            [
                Pos2::new(x, ruler.top() + LOOP_H),
                Pos2::new(x, ruler.bottom()),
            ],
            Stroke::new(1.0, hex(0x6a6a6a)),
        );
        p.text(
            Pos2::new(x + 3.0, ruler.bottom() - 2.0),
            Align2::LEFT_BOTTOM,
            (t / bar + 1).to_string(),
            theme::font(10.5),
            TEXT_DIM,
        );
        t += step;
    }

    let a = session.project.arrangement.clone();
    let (x0, x1) = (axis.x(a.loop_start as f64), axis.x(a.loop_end as f64));
    let brace = Rect::from_min_max(
        Pos2::new(x0, ruler.top() + 1.0),
        Pos2::new(x1, ruler.top() + LOOP_H),
    );
    p.rect_filled(
        brace,
        0.0,
        if a.loop_enabled {
            hex(0xb0b0b0)
        } else {
            hex(0x5a5a5a)
        },
    );

    let top = Rect::from_min_max(ruler.min, Pos2::new(ruler.right(), ruler.top() + LOOP_H));
    let loop_resp = ui.interact(top, ui.scope_id().with("arr_loop"), Sense::click_and_drag());
    let hover_kind = |x: f32| {
        if (x - x0).abs() < EDGE_W {
            Some(LoopDrag::Start)
        } else if (x - x1).abs() < EDGE_W {
            Some(LoopDrag::End)
        } else if x > x0 && x < x1 {
            Some(LoopDrag::Whole)
        } else {
            None
        }
    };
    if let Some(pos) = loop_resp.hover_pos() {
        if matches!(hover_kind(pos.x), Some(LoopDrag::Start | LoopDrag::End)) {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }
    if loop_resp.drag_started() {
        if let Some(pos) = loop_resp.interact_pointer_pos() {
            state.arr.loop_drag = hover_kind(pos.x).map(|k| (k, pos.x, a.loop_start, a.loop_end));
        }
    }
    if loop_resp.clicked() {
        session.set_loop(!a.loop_enabled, a.loop_start, a.loop_end);
    }
    if let (Some((kind, origin, s0, e0)), Some(pos)) =
        (state.arr.loop_drag, loop_resp.interact_pointer_pos())
    {
        let snapping = !ui.input(|i| i.modifiers.alt);
        let dt = ((pos.x - origin) / axis.px_per_beat) as f64 * PPQ as f64;
        let snapped = |v: u64| timeline::snap(v as f64 + dt, grid, snapping).max(0) as u64;
        let (s, e) = match kind {
            LoopDrag::Whole => {
                let s = snapped(s0);
                (s, s + (e0 - s0))
            }
            LoopDrag::Start => (snapped(s0).min(e0.saturating_sub(grid.max(1))), e0),
            LoopDrag::End => (s0, snapped(e0).max(s0 + grid.max(1))),
        };
        session.set_loop(a.loop_enabled, s, e);
    }
    if loop_resp.drag_stopped() {
        state.arr.loop_drag = None;
    }
    info(
        loop_resp,
        "Loop brace — click to switch looping on or off, drag to move, drag an end to resize",
    );

    let lower = Rect::from_min_max(Pos2::new(ruler.left(), ruler.top() + LOOP_H), ruler.max);
    let seek = ui.interact(
        lower,
        ui.scope_id().with("arr_seek"),
        Sense::click_and_drag(),
    );
    if seek.clicked() || seek.dragged() {
        if let Some(pos) = seek.interact_pointer_pos() {
            let snapping = !ui.input(|i| i.modifiers.alt);
            let t = timeline::snap(axis.tick(pos.x), grid, snapping).max(0);
            session.seek(t as f64);
        }
    }
    info(seek, "Click to move the song position");

    let marker = axis.x(session.insert_marker());
    let tri = vec![
        Pos2::new(marker - 4.0, ruler.bottom() - 7.0),
        Pos2::new(marker + 4.0, ruler.bottom() - 7.0),
        Pos2::new(marker, ruler.bottom()),
    ];
    p.add(egui::Shape::convex_polygon(
        tri,
        hex(0xe0e0e0),
        Stroke::NONE,
    ));
    p.line_segment(
        [ruler.left_bottom(), ruler.right_bottom()],
        Stroke::new(1.0, SEPARATOR),
    );
}

fn track_headers(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, headers: Rect) {
    let visible = session.project.visible_tracks();
    let ids: Vec<(TrackId, usize)> = visible
        .iter()
        .map(|&v| (session.project.tracks[v].id, v))
        .collect();
    for (row, (id, i)) in ids.iter().enumerate() {
        let lane = lane_rect(headers, row, state.arr.track_h, state.arr.scroll.y);
        if lane.bottom() < headers.top() || lane.top() > headers.bottom() {
            continue;
        }
        track_header(ui, session, state, headers, lane, *id, *i);
    }
}

/// The rows below the lanes: one for each return track, then the Master.
fn return_rows(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    area: Rect,
    lanes_right: f32,
) {
    let p = ui.painter().clone();
    p.rect_filled(area, 0.0, PANEL_DARK);
    p.line_segment(
        [area.left_top(), area.right_top()],
        Stroke::new(2.0, SEPARATOR),
    );
    let returns: Vec<TrackId> = session.project.returns.iter().map(|t| t.id).collect();
    for (i, id) in returns.iter().enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(lanes_right, area.top() + i as f32 * RETURN_ROW_H),
            Vec2::new(area.right() - lanes_right, RETURN_ROW_H),
        );
        return_row(ui, session, state, row, *id, i);
    }
    let row = Rect::from_min_size(
        Pos2::new(lanes_right, area.bottom() - RETURN_ROW_H),
        Vec2::new(area.right() - lanes_right, RETURN_ROW_H),
    );
    let selected = state.selected == Some(ChainRef::Master);
    let resp = ui.allocate_rect(row, Sense::click());
    p.rect_filled(
        row,
        0.0,
        if selected {
            hex(0x3b3b3b)
        } else {
            hex(0x303030)
        },
    );
    p.text(
        row.left_center() + Vec2::new(10.0, 0.0),
        Align2::LEFT_CENTER,
        "Master",
        theme::font_bold(11.0),
        TEXT,
    );
    if resp.clicked() {
        state.selected = Some(ChainRef::Master);
        state.detail = Detail::Devices;
    }
    info(resp, "Master track — click to see its devices");
}

fn return_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    row: Rect,
    id: TrackId,
    index: usize,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (name, color, mute, solo) = (t.name.clone(), t.color, t.mute, t.solo);
    let selected = state.selected == Some(ChainRef::Track(id));
    let resp = ui.allocate_rect(row, Sense::click());
    let p = ui.painter().clone();
    p.rect_filled(
        row,
        0.0,
        if selected {
            hex(0x3b3b3b)
        } else {
            hex(0x303030)
        },
    );
    p.line_segment(
        [row.left_bottom(), row.right_bottom()],
        Stroke::new(1.0, SEPARATOR),
    );
    p.rect_filled(
        Rect::from_min_size(row.min, Vec2::new(6.0, row.height())),
        0.0,
        rgb(color),
    );
    let label = Rect::from_min_size(
        row.min + Vec2::new(10.0, 0.0),
        Vec2::new(row.width() - 80.0, row.height()),
    );
    p.with_clip_rect(label).text(
        label.left_center(),
        Align2::LEFT_CENTER,
        name.as_str(),
        theme::font_bold(10.5),
        TEXT,
    );
    let row_h = RETURN_ROW_H - 6.0;
    let letter = (b'A' + index as u8) as char;
    let mute_r = Rect::from_min_size(
        Pos2::new(row.right() - 68.0, row.top() + 3.0),
        Vec2::new(30.0, row_h),
    );
    let solo_r = Rect::from_min_size(
        Pos2::new(row.right() - 35.0, row.top() + 3.0),
        Vec2::new(30.0, row_h),
    );
    let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(mute_r));
    let r = widgets::state_button(
        &mut mu,
        mute_r.size(),
        !mute,
        ACTIVATOR,
        &letter.to_string(),
    );
    if info(r, "Track Activator — click to mute/unmute").clicked() {
        if let Some(t) = session.project.track_mut(id) {
            t.mute = !mute;
        }
        session.sync_mix(id);
    }
    let mut su = ui.new_child(egui::UiBuilder::new().max_rect(solo_r));
    let r = widgets::state_button(&mut su, solo_r.size(), solo, SOLO_BLUE, "S");
    if info(
        r,
        "Solo the return — every other track's main output goes silent",
    )
    .clicked()
    {
        let additive = su.input(|i| i.modifiers.command);
        session_view::toggle_solo(session, id, solo, additive);
    }
    if resp.clicked() {
        state.selected = Some(ChainRef::Track(id));
        state.detail = Detail::Devices;
    }
    let resp = info(resp, format!("Return track '{name}'"));
    resp.context_menu(|ui| session_view::return_menu(ui, session, state, id, &name));
}

fn track_header(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rect: Rect,
    id: TrackId,
    index: usize,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (name, color, mute, solo, arm, kind, folded) = (
        t.name.clone(),
        t.color,
        t.mute,
        t.solo,
        t.arm,
        t.kind,
        t.ui.folded,
    );
    let holds_solo = kind == TrackKind::Group && session.project.holds_solo(id);
    let depth = session.project.group_depth(id);
    let selected = state.selected == Some(ChainRef::Track(id));
    let mut cu = session_view::child(
        ui,
        headers,
        rect,
        Layout::left_to_right(egui::Align::Center),
    );
    let resp = cu.allocate_rect(rect, Sense::click());
    let p = cu.painter().clone();
    p.rect_filled(
        rect,
        0.0,
        if selected {
            hex(0x3b3b3b)
        } else {
            hex(0x303030)
        },
    );
    p.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, SEPARATOR),
    );
    let swatch = Rect::from_min_size(rect.min, Vec2::new(6.0, rect.height()));
    p.rect_filled(swatch, 0.0, rgb(color));
    let indent = depth as f32 * 8.0;
    let inner = Rect::from_min_max(
        rect.min + Vec2::new(10.0 + indent, 4.0),
        rect.max - Vec2::new(10.0, 4.0),
    );
    let fold_w = if kind == TrackKind::Group {
        icons::fold(
            &p,
            Rect::from_min_size(inner.min, Vec2::splat(16.0)),
            !folded,
            TEXT_DIM,
        );
        16.0
    } else {
        0.0
    };
    let top = Rect::from_min_size(
        inner.min + Vec2::new(fold_w, 0.0),
        Vec2::new(inner.width() - fold_w, 16.0),
    );
    let galley = p.layout_no_wrap(name.clone(), theme::font_bold(11.0), TEXT);
    p.with_clip_rect(top.intersect(headers)).galley(
        top.left_center() - Vec2::new(0.0, galley.size().y / 2.0),
        galley,
        TEXT,
    );

    let btn_h = 16.0;
    if rect.height() >= 36.0 {
        let row = Rect::from_min_size(
            Pos2::new(inner.left(), inner.top() + 18.0),
            Vec2::new(inner.width(), btn_h),
        );
        let mut bu = session_view::child(
            &mut cu,
            headers,
            row,
            Layout::left_to_right(egui::Align::Center),
        );
        bu.spacing_mut().item_spacing.x = 2.0;
        let bw = 30.0;
        let r = widgets::state_button(
            &mut bu,
            Vec2::new(bw, btn_h),
            !mute,
            ACTIVATOR,
            &(index + 1).to_string(),
        );
        if info(r, "Track Activator — click to mute/unmute").clicked() {
            if let Some(t) = session.project.track_mut(id) {
                t.mute = !mute;
            }
            session.sync_mix(id);
        }
        let r = widgets::state_button(&mut bu, Vec2::new(bw, btn_h), solo, SOLO_BLUE, "S");
        if holds_solo && !solo {
            let half =
                Rect::from_min_size(r.rect.min, Vec2::new(r.rect.width() / 2.0, r.rect.height()));
            p.rect_filled(half, 1.0, SOLO_BLUE);
            p.text(
                r.rect.center(),
                Align2::CENTER_CENTER,
                "S",
                theme::font(10.5),
                TEXT,
            );
        }
        if info(r, "Solo — Cmd-click to solo several tracks").clicked() {
            session_view::toggle_solo(session, id, solo, bu.input(|i| i.modifiers.command));
        }
        if kind != TrackKind::Group {
            let r =
                widgets::icon_button(&mut bu, Vec2::new(bw, btn_h), arm, ARM_RED, icons::record);
            if info(
                r,
                "Arm — record MIDI onto this track (Cmd-click to arm several)",
            )
            .clicked()
            {
                let exclusive = !bu.input(|i| i.modifiers.command);
                session.set_arm(id, !arm, exclusive);
            }
        }
    }
    let on_fold = kind == TrackKind::Group
        && resp
            .interact_pointer_pos()
            .is_some_and(|pos| Rect::from_min_size(inner.min, Vec2::splat(16.0)).contains(pos));
    if resp.clicked() && on_fold {
        session.set_folded(id, !folded);
    } else if resp.clicked() {
        let m = cu.input(|i| i.modifiers);
        if m.shift || m.command {
            match state.selected_tracks.iter().position(|t| *t == id) {
                Some(i) => {
                    state.selected_tracks.remove(i);
                }
                None => state.selected_tracks.push(id),
            }
        } else {
            state.selected_tracks = vec![id];
        }
        state.selected = Some(ChainRef::Track(id));
        state.detail = Detail::Devices;
    }
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        session_view::drop_on_track(session, state, id, &item, None);
    }
    let resp = info(
        resp,
        format!("Track '{name}' — click to select, right-click for options"),
    );
    resp.context_menu(|ui| session_view::track_menu(ui, session, state, id, &name));
}

#[allow(clippy::too_many_arguments)]
fn scrollbars(
    ui: &mut egui::Ui,
    state: &mut UiState,
    lanes: Rect,
    headers: Rect,
    full: Rect,
    content_w: f32,
    content_h: f32,
    max_x: f32,
    max_y: f32,
) {
    let thumb_color = |r: &egui::Response| {
        Color32::from_white_alpha(if r.hovered() || r.dragged() { 90 } else { 45 })
    };
    if max_x > 0.0 {
        let track = Rect::from_min_max(
            Pos2::new(lanes.left(), full.bottom() - SCROLLBAR + 1.0),
            Pos2::new(lanes.right(), full.bottom() - 1.0),
        );
        let w = (track.width() * lanes.width() / content_w).max(20.0);
        let x = track.left() + (track.width() - w) * (state.arr.scroll.x / max_x);
        let thumb = Rect::from_min_size(Pos2::new(x, track.top()), Vec2::new(w, track.height()));
        let resp = ui.allocate_rect(track, Sense::drag());
        ui.painter().rect_filled(thumb, 3.0, thumb_color(&resp));
        if resp.dragged() {
            state.arr.scroll.x += resp.drag_delta().x / (track.width() - w).max(1.0) * max_x;
        }
    }
    if max_y > 0.0 {
        let track = Rect::from_min_max(
            Pos2::new(headers.right() - SCROLLBAR, headers.top()),
            Pos2::new(headers.right() - 1.0, headers.bottom()),
        );
        let h = (track.height() * lanes.height() / content_h).max(20.0);
        let y = track.top() + (track.height() - h) * (state.arr.scroll.y / max_y);
        let thumb = Rect::from_min_size(Pos2::new(track.left(), y), Vec2::new(track.width(), h));
        let resp = ui.allocate_rect(track, Sense::drag());
        ui.painter().rect_filled(thumb, 3.0, thumb_color(&resp));
        if resp.dragged() {
            state.arr.scroll.y += resp.drag_delta().y / (track.height() - h).max(1.0) * max_y;
        }
    }
}

/// Whether any track with timeline content has been taken over by a Session clip.
pub fn overridden(session: &Session) -> bool {
    session.project.tracks.iter().any(|t| {
        !t.arrangement.is_empty()
            && session
                .track_shared
                .get(&t.id)
                .is_some_and(|s| !s.follow_arrangement.load(Ordering::Relaxed))
    })
}
