//! The start and end markers, the loop brace and the scrub area, drawn the same way over the
//! Sample and MIDI Note editors.

use super::{ClipViewState, MarkerSel};
use crate::session::{ClipRef, Session};
use crate::ui::keymap::{self, Platform};
use crate::ui::theme::*;
use crate::ui::widgets::info;
use crate::ui::UiState;
use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use rlive_project::clip_edit as edit;
use rlive_project::Clip;

/// The height of the strip with the markers and the brace on it.
pub const MARKER_H: f32 = 16.0;
/// The height of the scrub area under it.
pub const SCRUB_H: f32 = 8.0;
/// How close the pointer must be to a marker to be on it.
const GRAB: f32 = 6.0;

/// The time axis of an editor: which ticks of the clip show across which pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub left: f32,
    pub width: f32,
    pub start: f64,
    pub ticks: f64,
}

impl Axis {
    pub fn x(&self, tick: f64) -> f32 {
        self.left + ((tick - self.start) / self.ticks.max(1.0)) as f32 * self.width
    }

    pub fn tick(&self, x: f32) -> f64 {
        self.start + ((x - self.left) / self.width.max(1.0)) as f64 * self.ticks
    }
}

/// What's being dragged on the marker strip.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MarkerGrab {
    Start,
    End,
    LoopStart,
    LoopEnd,
    /// The brace, grabbed this far from its start.
    Brace(f64),
}

fn hit(clip: &Clip, axis: &Axis, pos: Pos2, strip: Rect) -> Option<MarkerGrab> {
    let near = |tick: u64| (axis.x(tick as f64) - pos.x).abs() < GRAB;
    let brace = (
        MarkerGrab::LoopStart,
        MarkerGrab::LoopEnd,
        clip.loop_start,
        clip.loop_end(),
    );
    if pos.y < strip.center().y {
        if near(clip.start) {
            return Some(MarkerGrab::Start);
        }
        if near(clip.end_marker()) {
            return Some(MarkerGrab::End);
        }
    }
    if near(brace.2) {
        return Some(brace.0);
    }
    if near(brace.3) {
        return Some(brace.1);
    }
    let at = axis.tick(pos.x);
    (pos.y >= strip.center().y && at >= brace.2 as f64 && at < brace.3 as f64)
        .then_some(MarkerGrab::Brace(at - brace.2 as f64))
}

fn apply_grab(session: &mut Session, cref: ClipRef, grab: MarkerGrab, at: u64) {
    match grab {
        MarkerGrab::Start => {
            session.edit_clips(&[cref], "Move Start Marker", |c| edit::set_start(c, at));
        }
        MarkerGrab::End => {
            session.edit_clips(&[cref], "Move End Marker", |c| edit::set_end(c, at));
        }
        MarkerGrab::LoopStart => {
            session.edit_clips(&[cref], "Move Loop Start", |c| {
                let end = c.loop_end();
                let at = at.min(end.saturating_sub(edit::MIN_LOOP));
                c.length = end - at;
                edit::set_loop_position(c, at);
            });
        }
        MarkerGrab::LoopEnd => {
            session.edit_clips(&[cref], "Move Loop End", |c| edit::set_loop_end(c, at));
        }
        MarkerGrab::Brace(_) => {
            session.edit_clips(&[cref], "Move Loop", |c| edit::set_loop_position(c, at));
        }
    }
}

/// A snapped position, unless the grid-bypass key is held.
fn snapped(tick: f64, grid: u64, free: bool) -> u64 {
    if free || grid == 0 {
        tick.max(0.0).round() as u64
    } else {
        let g = grid as f64;
        ((tick / g).round() * g).max(0.0) as u64
    }
}

fn flag(p: &egui::Painter, x: f32, top: f32, color: Color32, pointing_right: bool, selected: bool) {
    let d = if pointing_right { 7.0 } else { -7.0 };
    let points = vec![
        Pos2::new(x, top),
        Pos2::new(x + d, top + 4.0),
        Pos2::new(x, top + 8.0),
    ];
    p.add(Shape::convex_polygon(
        points,
        color,
        if selected {
            Stroke::new(1.0, pal().text)
        } else {
            Stroke::NONE
        },
    ));
}

/// The markers and the brace, and dragging them.
pub fn marker_strip(
    ui: &mut egui::Ui,
    strip: Rect,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    axis: &Axis,
    grid: u64,
) {
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let view = &mut state.clip_view;
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, shade(0x2c2c2c));
    let mid = strip.center().y;
    let (x0, x1) = (
        axis.x(clip.loop_start as f64),
        axis.x(clip.loop_end() as f64),
    );
    let brace = Rect::from_x_y_ranges(x0..=x1, mid + 2.0..=strip.bottom() - 1.0);
    let selected = view.marker == Some(MarkerSel::Loop);
    let fill = match (clip.looped, selected) {
        (true, true) => pal().accent,
        (true, false) => shade(0x9a9a9a),
        (false, _) => shade(0x5a5a5a),
    };
    p.rect_filled(brace, 0.0, fill);
    for x in [x0, x1] {
        p.line_segment(
            [Pos2::new(x, mid + 1.0), Pos2::new(x, strip.bottom() - 1.0)],
            Stroke::new(2.0, pal().text_dim),
        );
    }
    let color = pal().play_green;
    flag(
        &p,
        axis.x(clip.start as f64),
        strip.top() + 0.5,
        color,
        true,
        view.marker == Some(MarkerSel::Start),
    );
    if !clip.looped {
        flag(
            &p,
            axis.x(clip.end_marker() as f64),
            strip.top() + 0.5,
            pal().arm_red,
            false,
            view.marker == Some(MarkerSel::End),
        );
    }

    let id = ui.scope_id().with("clip_markers");
    let resp = ui.interact(strip, id, Sense::click_and_drag());
    let modifiers = ui.input(|i| i.modifiers);
    let free = keymap::ignores_grid(modifiers, Platform::current());
    if let Some(pos) = resp.hover_pos() {
        if hit(&clip, axis, pos, strip).is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let at = snapped(axis.tick(pos.x), grid, free);
            if modifiers.command && modifiers.shift {
                session.edit_clips(&[cref], "Move End Marker", |c| edit::set_end(c, at));
            } else if modifiers.command {
                session.edit_clips(&[cref], "Move Start Marker", |c| edit::set_start(c, at));
            } else {
                view.marker = match hit(&clip, axis, pos, strip) {
                    Some(MarkerGrab::Start) => Some(MarkerSel::Start),
                    Some(MarkerGrab::End) => Some(MarkerSel::End),
                    Some(_) => Some(MarkerSel::Loop),
                    None => None,
                };
            }
        }
    }
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(strip.min);
        view.grab = hit(&clip, axis, origin, strip);
        view.marker = view.grab.map(|g| match g {
            MarkerGrab::Start => MarkerSel::Start,
            MarkerGrab::End => MarkerSel::End,
            _ => MarkerSel::Loop,
        });
    }
    if resp.dragged() {
        if let (Some(grab), Some(pos)) = (view.grab, resp.interact_pointer_pos()) {
            let offset = if let MarkerGrab::Brace(from) = grab {
                from
            } else {
                0.0
            };
            let at = snapped(axis.tick(pos.x) - offset, grid, free);
            apply_grab(session, cref, grab, at);
        }
    }
    if resp.drag_stopped() {
        view.grab = None;
    }
    info(
        resp,
        "Clip markers — drag the start and end markers or the loop brace; the arrow keys move what \
         is selected",
    );
}

/// The scrub area: pressing jumps playback there, by the global quantization, and holding keeps
/// repeating a fragment that long (manual p. 228).
pub fn scrub_strip(
    ui: &mut egui::Ui,
    strip: Rect,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    axis: &Axis,
) {
    ui.painter_at(strip)
        .rect_filled(strip, 0.0, shade(0x242424));
    let id = ui.scope_id().with("clip_scrub");
    let resp = ui.interact(strip, id, Sense::click_and_drag());
    resp.clone().on_hover_cursor(CursorIcon::PointingHand);
    scrub_gesture(&resp, session, &mut state.clip_view, cref, axis);
    info(
        resp,
        "Scrub Area — jump the clip here, in steps of the global quantization; hold to repeat",
    );
}

/// Plays what a press on a scrub area asks for. A scrub stays held until the button comes up.
pub fn scrub_gesture(
    resp: &egui::Response,
    session: &mut Session,
    view: &mut ClipViewState,
    cref: ClipRef,
    axis: &Axis,
) {
    if resp.is_pointer_button_down_on() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let at = axis.tick(pos.x);
            let quantized = session.quantized(at);
            if view.scrubbing != Some(quantized) {
                view.scrubbing = Some(quantized);
                session.scrub_clip(cref, at, true);
            }
        }
    } else if view.scrubbing.take().is_some() {
        session.scrub_release(cref);
    }
}

/// A strip showing the whole clip, with the part in the editor outlined. Dragging moves the part
/// shown. Returns where it should now start, if that changed.
pub fn overview_strip(
    ui: &mut egui::Ui,
    strip: Rect,
    total: f64,
    start: f64,
    ticks: f64,
) -> (Option<f64>, f32) {
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, pal().panel_dark);
    let total = total.max(1.0);
    let x = |t: f64| strip.left() + (t / total) as f32 * strip.width();
    let shown = Rect::from_x_y_ranges(x(start)..=x(start + ticks), strip.y_range());
    p.rect_stroke(
        shown,
        0.0,
        Stroke::new(1.0, pal().text),
        egui::StrokeKind::Inside,
    );
    let resp = ui.interact(strip, ui.scope_id().with("clip_overview"), Sense::drag());
    let out = if resp.dragged() {
        let delta = resp.drag_delta().x as f64 / strip.width().max(1.0) as f64 * total;
        Some((start + delta).clamp(0.0, (total - ticks).max(0.0)))
    } else {
        None
    };
    let zoom = if resp.dragged() {
        resp.drag_delta().y
    } else {
        0.0
    };
    info(
        resp,
        "Clip View Selector — the whole clip; drag sideways to scroll, up or down to zoom",
    );
    (out, zoom)
}

/// The vertical line of the playhead, when the clip is playing.
pub fn draw_playhead(ui: &egui::Ui, rect: Rect, session: &Session, cref: ClipRef, axis: &Axis) {
    if let Some(at) = session.clip_playhead(cref) {
        let x = axis.x(at);
        if x >= rect.left() && x <= rect.right() {
            ui.painter_at(rect).line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, pal().text),
            );
        }
    }
}

/// A `Vec2` for a small square of a chosen colour in a menu.
pub(super) const SWATCH: Vec2 = Vec2::new(14.0, 14.0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_axis_maps_ticks_and_pixels_both_ways() {
        let a = Axis {
            left: 100.0,
            width: 400.0,
            start: 960.0,
            ticks: 1920.0,
        };
        assert_eq!(a.x(960.0), 100.0);
        assert_eq!(a.x(960.0 + 1920.0), 500.0);
        assert_eq!(a.tick(300.0), 960.0 + 960.0);
    }

    #[test]
    fn dragging_snaps_to_the_grid_unless_it_is_bypassed() {
        assert_eq!(snapped(250.0, 240, false), 240);
        assert_eq!(snapped(250.0, 240, true), 250);
        assert_eq!(snapped(-10.0, 240, false), 0);
    }
}
