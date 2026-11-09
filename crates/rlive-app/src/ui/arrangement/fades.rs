//! Fade and crossfade handles on audio clips.

use super::{Hit, Rows};
use crate::session::{FadeSide, Session};
use crate::ui::timeline::TimeAxis;
use crate::ui::{theme::*, UiState};
use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::{ArrClipId, TrackId};
use rlive_project::arrangement_edit::EffectiveFades;
use rlive_project::envelope_edit::curve_after_drag;
use rlive_project::ArrangementClip;

/// Clips shorter than this don't show handles, as the manual says of folded tracks.
pub const MIN_HEIGHT: f32 = 26.0;
const HANDLE: f32 = 7.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeHandle {
    /// Where the fade reaches full level: its length.
    Peak,
    /// The outer end of the fade, which can be dragged over a neighbour to crossfade.
    Edge,
    /// The shape of the ramp.
    Curve,
}

/// One handle of a clip's fades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FadeTarget {
    pub track: TrackId,
    pub clip: ArrClipId,
    pub side: FadeSide,
    pub handle: FadeHandle,
}

/// A handle being dragged.
pub struct FadeDrag {
    target: FadeTarget,
    base: ArrangementClip,
    dy: f32,
}

/// The part of a clip the fades are drawn over: everything under its title bar.
fn body(rect: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(
            rect.left(),
            (rect.top() + super::CLIP_TITLE_H).min(rect.bottom()),
        ),
        rect.max,
    )
}

/// Paints the ramp, dimming what the fade turns down.
pub fn paint_ramps(
    p: &egui::Painter,
    rect: Rect,
    c: &ArrangementClip,
    f: &EffectiveFades,
    axis: &TimeAxis,
    lanes: Rect,
) {
    let rect = body(rect);
    let dim = Color32::from_black_alpha(90);
    let line = Stroke::new(1.0, pal().text_dim);
    // The stretch before a clip's start, or after its end, is where it sounds under a neighbour.
    let start = c.start.saturating_sub(f.lead);
    let peak = c.start + f.fade_in;
    let begin = c.end() - f.fade_out.min(c.length);
    let end = c.end() + f.tail;
    let spans = [
        (FadeSide::In, start, peak, f.in_curve),
        (FadeSide::Out, begin, end, f.out_curve),
    ];
    for (side, from, to, curve) in spans {
        if to <= from {
            continue;
        }
        let (x0, x1) = (axis.x(from as f64), axis.x(to as f64));
        let shown_left = x0.max(lanes.left()).max(rect.left()) as i32;
        let shown_right = (x1.min(lanes.right()).min(rect.right())) as i32;
        let mut last = None;
        for px in shown_left..=shown_right {
            let x = px as f32;
            let progress = ((x - x0) / (x1 - x0).max(1e-3)) as f64;
            let gain = match side {
                FadeSide::In => rlive_core::fade_gain(progress, curve),
                FadeSide::Out => rlive_core::fade_gain(1.0 - progress, curve),
            };
            let y = rect.bottom() - rect.height() * gain;
            p.rect_filled(
                Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(x + 1.0, y)),
                0.0,
                dim,
            );
            if let Some(prev) = last {
                p.line_segment([prev, Pos2::new(x, y)], line);
            }
            last = Some(Pos2::new(x, y));
        }
    }
}

/// Where each handle of a clip sits.
fn handle_rects(
    c: &ArrangementClip,
    rect: Rect,
    axis: &TimeAxis,
) -> [(FadeSide, FadeHandle, Rect); 6] {
    let rect = body(rect);
    let f = c.clip.fades;
    let top = rect.top() + 2.0;
    let bottom = rect.bottom() - HANDLE - 2.0;
    let at = |x: f32, y: f32| Rect::from_min_size(Pos2::new(x, y), Vec2::splat(HANDLE));
    let peak_in = axis.x((c.start + f.fade_in) as f64);
    let edge_in = axis.x(c.start.saturating_sub(f.in_tail) as f64);
    let curve_in = (peak_in + edge_in) / 2.0 - HANDLE / 2.0;
    let peak_out = axis.x((c.end().saturating_sub(f.fade_out)) as f64) - HANDLE;
    let edge_out = axis.x((c.end() + f.out_tail) as f64) - HANDLE;
    let curve_out = (peak_out + edge_out) / 2.0;
    let mid = rect.center().y - HANDLE / 2.0;
    [
        (FadeSide::In, FadeHandle::Peak, at(peak_in, top)),
        (FadeSide::In, FadeHandle::Edge, at(edge_in, bottom)),
        (FadeSide::In, FadeHandle::Curve, at(curve_in, mid)),
        (FadeSide::Out, FadeHandle::Peak, at(peak_out, top)),
        (FadeSide::Out, FadeHandle::Edge, at(edge_out, bottom)),
        (FadeSide::Out, FadeHandle::Curve, at(curve_out, mid)),
    ]
}

/// The tick a handle stops at, shown while it's selected.
fn limit_tick(session: &Session, t: FadeTarget, c: &ArrangementClip) -> Option<u64> {
    let room = c.fade_room();
    let next = next_clip(session, t.track, c);
    let prev = prev_clip(session, t.track, c);
    match (t.side, t.handle) {
        (FadeSide::In, FadeHandle::Peak) => Some(c.start + c.fade_in_limit()),
        (FadeSide::Out, FadeHandle::Peak) => Some(c.end().saturating_sub(c.fade_out_limit())),
        (FadeSide::In, FadeHandle::Edge) => {
            let reach = prev.map_or(0, |p| p.fade_room().tail.min(room.lead));
            Some(c.start.saturating_sub(reach))
        }
        (FadeSide::Out, FadeHandle::Edge) => {
            let reach = next.map_or(0, |n| n.fade_room().lead.min(room.tail));
            Some(c.end() + reach)
        }
        _ => None,
    }
}

fn prev_clip<'a>(
    session: &'a Session,
    track: TrackId,
    c: &ArrangementClip,
) -> Option<&'a ArrangementClip> {
    let t = session.project.track(track)?;
    let lane = t.lane(t.lane_of(c.id)?)?;
    let i = lane.iter().position(|o| o.id == c.id)?;
    i.checked_sub(1)
        .and_then(|p| lane.get(p))
        .filter(|p| p.end() == c.start && p.is_audio())
}

fn next_clip<'a>(
    session: &'a Session,
    track: TrackId,
    c: &ArrangementClip,
) -> Option<&'a ArrangementClip> {
    let t = session.project.track(track)?;
    let lane = t.lane(t.lane_of(c.id)?)?;
    let i = lane.iter().position(|o| o.id == c.id)?;
    lane.get(i + 1)
        .filter(|n| n.start == c.end() && n.is_audio())
}

/// Draws and handles the fade handles of the clip under the pointer.
pub(super) fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    axis: &TimeAxis,
    hits: &[Hit],
    _rows: &Rows,
    lanes: Rect,
) {
    let visible = !state.arr.auto.mode || state.arr.fades_key;
    if !visible {
        return;
    }
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let dragged = state.arr.fade_drag.as_ref().map(|d| d.target);
    let mut hover = pointer
        .filter(|p| lanes.contains(*p))
        .and_then(|p| hits.iter().rev().find(|h| h.rect.contains(p)).copied());
    if let Some(t) = dragged {
        hover = hits
            .iter()
            .find(|h| h.track == t.track && h.id == t.clip)
            .copied();
    }
    if let Some(sel) = state.arr.selected_fade {
        paint_limit(ui, session, axis, hits, lanes, sel);
    }
    let Some(hit) = hover else { return };
    let Some(c) = session
        .project
        .track(hit.track)
        .and_then(|t| t.arr_clip(hit.id))
        .filter(|c| c.is_audio() && hit.rect.height() >= MIN_HEIGHT)
        .cloned()
    else {
        return;
    };
    let p = ui.painter_at(lanes);
    for (side, handle, rect) in handle_rects(&c, hit.rect, axis) {
        let target = FadeTarget {
            track: hit.track,
            clip: hit.id,
            side,
            handle,
        };
        let resp = ui.interact(
            rect.expand(2.0),
            ui.scope_id()
                .with(("fade", hit.id, side as u8, handle as u8)),
            Sense::click_and_drag(),
        );
        let selected = state.arr.selected_fade == Some(target);
        let color = if selected { pal().select } else { pal().text };
        if handle == FadeHandle::Curve {
            p.circle_filled(rect.center(), HANDLE / 2.0, color);
        } else {
            p.rect_filled(rect, 1.0, color);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if resp.clicked() {
            state.arr.selected_fade = Some(target);
        }
        if resp.drag_started() {
            state.arr.selected_fade = Some(target);
            state.arr.fade_drag = Some(FadeDrag {
                target,
                base: c.clone(),
                dy: 0.0,
            });
        }
        if resp.dragged() {
            if let Some(mut drag) = state.arr.fade_drag.take() {
                drag.dy += resp.drag_delta().y;
                let tick = resp
                    .interact_pointer_pos()
                    .map(|pos| axis.tick(pos.x))
                    .unwrap_or(0.0);
                apply_drag(session, &drag, tick);
                state.arr.fade_drag = Some(drag);
            }
        }
        if resp.drag_stopped() {
            state.arr.fade_drag = None;
        }
    }
}

fn paint_limit(
    ui: &egui::Ui,
    session: &Session,
    axis: &TimeAxis,
    hits: &[Hit],
    lanes: Rect,
    t: FadeTarget,
) {
    let Some(hit) = hits.iter().find(|h| h.track == t.track && h.id == t.clip) else {
        return;
    };
    let Some(c) = session
        .project
        .track(hit.track)
        .and_then(|tr| tr.arr_clip(hit.id))
    else {
        return;
    };
    let Some(limit) = limit_tick(session, t, c) else {
        return;
    };
    let x = axis.x(limit as f64);
    ui.painter_at(lanes).add(egui::Shape::dashed_line(
        &[
            Pos2::new(x, hit.rect.top()),
            Pos2::new(x, hit.rect.bottom()),
        ],
        Stroke::new(1.0, Color32::BLACK),
        4.0,
        3.0,
    ));
}

fn apply_drag(session: &mut Session, drag: &FadeDrag, tick: f64) {
    let t = drag.target;
    let c = &drag.base;
    let f = c.clip.fades;
    let (start, end) = (c.start as f64, c.end() as f64);
    let prev = prev_clip(session, t.track, c).map(|p| p.id);
    let next = next_clip(session, t.track, c).map(|n| n.id);
    let to_ticks = |v: f64| v.max(0.0).round() as u64;
    match (t.side, t.handle) {
        (FadeSide::In, FadeHandle::Peak) => {
            let ticks = to_ticks(tick - start);
            match (f.in_tail > 0, prev) {
                (true, Some(p)) => {
                    session.arr_crossfade(t.track, p, f.in_tail, ticks, f.in_curve);
                }
                _ => session.arr_set_fade(t.track, t.clip, FadeSide::In, ticks, None),
            }
        }
        (FadeSide::Out, FadeHandle::Peak) => {
            let ticks = to_ticks(end - tick);
            match (f.out_tail > 0, next) {
                (true, Some(_)) => {
                    session.arr_crossfade(t.track, t.clip, ticks, f.out_tail, f.out_curve);
                }
                _ => session.arr_set_fade(t.track, t.clip, FadeSide::Out, ticks, None),
            }
        }
        (FadeSide::In, FadeHandle::Edge) => {
            if let Some(p) = prev {
                let before = to_ticks(start - tick);
                let after = f.fade_in;
                session.arr_crossfade(t.track, p, before, after, f.in_curve);
            }
        }
        (FadeSide::Out, FadeHandle::Edge) => {
            let after = to_ticks(tick - end);
            let before = f.fade_out;
            if next.is_some() {
                session.arr_crossfade(t.track, t.clip, before, after, f.out_curve);
            }
        }
        (FadeSide::In, FadeHandle::Curve) => {
            let curve = curve_after_drag(f.in_curve, true, drag.dy);
            match (f.in_tail > 0, prev) {
                (true, Some(p)) => {
                    session.arr_crossfade(t.track, p, f.in_tail, f.fade_in, curve);
                }
                _ => session.arr_set_fade(t.track, t.clip, FadeSide::In, f.fade_in, Some(curve)),
            }
        }
        (FadeSide::Out, FadeHandle::Curve) => {
            let curve = curve_after_drag(f.out_curve, false, drag.dy);
            match (f.out_tail > 0, next) {
                (true, Some(_)) => {
                    session.arr_crossfade(t.track, t.clip, f.fade_out, f.out_tail, curve);
                }
                _ => session.arr_set_fade(t.track, t.clip, FadeSide::Out, f.fade_out, Some(curve)),
            }
        }
    }
}
