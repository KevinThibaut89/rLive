//! Automation Mode: envelope lanes under each Arrangement track and everything done in them.
//!
//! The Arrangement lays the rows out; this draws the lanes and handles their input. Every change
//! goes through [`Session::set_envelope`], and a whole drag is one undo step because history only
//! commits once the mouse is up. A drag always recomputes from the points it started with.

mod edit;
pub(super) mod geom;
mod gestures;
mod headers;
pub mod lanes;
mod popups;

use super::arrangement::{Rows, TimeSel};
use super::theme::*;
use super::timeline::TimeAxis;
use super::UiState;
use crate::session::{ChainRef, EnvParam, Session};
pub use edit::{delete_focused_envelope, edit_selection, EnvelopeClipboard};
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use geom::{Handle, LaneGeom};
pub use headers::show_headers;
pub use lanes::LaneKey;
use rlive_core::param::EnvTarget;
use rlive_project::envelope_edit::{self, Corners};
use rlive_project::Breakpoint;

pub use popups::{control_zone, show_popups};

/// Height of a track's main automation lane, which has its choosers beside it.
pub const MAIN_LANE_H: f32 = 86.0;
/// Height of an additional automation lane.
pub const EXTRA_LANE_H: f32 = 62.0;
/// Space a clip keeps for its title bar, where it can still be grabbed in Automation Mode.
const CLIP_TITLE: f32 = 13.0;
/// How long B has to be held to be a momentary switch rather than a tap.
const DRAW_HOLD_SECONDS: f64 = 0.25;

/// The frame of a time selection, kept so its handles remember what they've done.
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    key: LaneKey,
    target: EnvTarget,
    base: Vec<Breakpoint>,
    orig: (f64, f64),
    span: (f64, f64),
    corners: Corners,
}

impl Frame {
    fn result(&self, range: rlive_core::ParamRange) -> Vec<Breakpoint> {
        let values = envelope_edit::reshape_values(
            &self.base,
            self.orig.0,
            self.orig.1,
            self.corners,
            range,
        );
        if self.span == self.orig {
            values
        } else {
            envelope_edit::stretch_time(&values, self.orig, self.span, range)
        }
    }
}

/// B taps Draw Mode on and off; held down while off, it only lasts as long as the key.
#[derive(Debug, Default)]
pub struct DrawKey {
    down: Option<(f64, bool)>,
}

impl DrawKey {
    /// The key went down. Returns the new state of Draw Mode.
    pub fn pressed(&mut self, draw: bool, now: f64) -> bool {
        if self.down.is_some() {
            return draw;
        }
        self.down = Some((now, draw));
        !draw
    }

    /// The key came up. Returns the new state of Draw Mode.
    pub fn released(&mut self, draw: bool, now: f64) -> bool {
        match self.down.take() {
            Some((at, false)) if now - at >= DRAW_HOLD_SECONDS => false,
            _ => draw,
        }
    }
}

/// What's going on in the automation lanes.
#[derive(Default)]
pub struct AutomationState {
    pub mode: bool,
    pub draw: bool,
    pub draw_key: DrawKey,
    /// The choosers list only automated parameters.
    pub automated_only: bool,
    pub focus: Option<LaneKey>,
    /// Additional lane headers selected together.
    pub headers: Vec<LaneKey>,
    frame: Option<Frame>,
    gesture: Option<gestures::Gesture>,
    menu: Option<popups::LaneMenu>,
    edit_value: Option<popups::EditValue>,
    param_menu: Option<popups::ParamMenu>,
}

impl AutomationState {
    pub fn dragging(&self) -> bool {
        self.gesture.is_some()
    }
}

/// Whether a lane is covered by the time selection.
fn in_sel(sel: &Option<TimeSel>, key: LaneKey) -> Option<(f64, f64)> {
    let s = sel.as_ref().filter(|s| !s.is_point())?;
    s.lanes.contains(&key).then_some((s.start, s.end))
}

/// How tall a row is in Automation Mode: its main lane, then the additional lanes on show.
pub fn row_heights(session: &Session, chain: ChainRef, track_h: f32) -> (f32, Vec<f32>) {
    let (hidden, count) = session.track_ui(chain).map_or((false, 0), |ui| {
        (ui.automation_lanes_hidden, ui.automation_lanes.len())
    });
    let extra = if hidden { 0 } else { count };
    (track_h.max(MAIN_LANE_H), vec![EXTRA_LANE_H; extra])
}

/// The target of a lane, as the track's lane layout has it now.
pub(super) fn lane_target(session: &Session, key: LaneKey) -> Option<EnvTarget> {
    let ui = session.track_ui(key.chain)?;
    match key.slot {
        None => lanes::main_target(ui, key.chain),
        Some(slot) => ui.automation_lanes.get(slot).copied(),
    }
}

/// One lane on screen this frame.
struct LaneView {
    key: LaneKey,
    target: Option<EnvTarget>,
    param: Option<EnvParam>,
    points: Vec<Breakpoint>,
    /// The whole lane, for drawing.
    rect: Rect,
    /// The part of it that takes clicks.
    hit_rect: Rect,
    geom: LaneGeom,
    base: f64,
}

impl LaneView {
    fn dashed_knob(&self) -> Option<f64> {
        let p = self.param.as_ref()?;
        self.points
            .is_empty()
            .then(|| p.range.to_normalized(self.base))
    }

    fn range(&self) -> Option<rlive_core::ParamRange> {
        self.param.as_ref().map(|p| p.range)
    }
}

fn lane_views(
    session: &Session,
    lanes: Rect,
    axis: &TimeAxis,
    rows: &Rows,
    scroll_y: f32,
) -> Vec<LaneView> {
    let mut out = Vec::new();
    let add = |key: LaneKey, rect: Rect, hit_rect: Rect, out: &mut Vec<LaneView>| {
        // A lane whose parameter has left the project shows nothing, and can't be edited.
        let param = lane_target(session, key).and_then(|t| session.env_param(key.chain, t));
        let target = param.as_ref().map(|p| p.target);
        let points = target
            .map(|t| session.env_points(key.chain, t).to_vec())
            .unwrap_or_default();
        let base = target
            .and_then(|t| session.env_base_value(key.chain, t))
            .or(param.as_ref().map(|p| p.info.default))
            .unwrap_or(0.0);
        let range = param.as_ref().map_or(
            rlive_core::ParamRange {
                min: 0.0,
                max: 1.0,
                skew: 1.0,
                quantized: false,
            },
            |p| p.range,
        );
        let geom = LaneGeom {
            rect: hit_rect,
            axis: *axis,
            range,
        };
        out.push(LaneView {
            key,
            target,
            param,
            points,
            rect,
            hit_rect,
            geom,
            base,
        });
    };
    for (i, row) in rows.rows.iter().enumerate() {
        let main = rows.main(lanes, i, scroll_y);
        let hit = Rect::from_min_max(Pos2::new(main.left(), main.top() + CLIP_TITLE), main.max);
        add(
            LaneKey {
                chain: row.chain,
                slot: None,
                take: None,
            },
            main,
            hit,
            &mut out,
        );
        for n in 0..row.extra.len() {
            let rect = rows.extra(lanes, i, n, scroll_y);
            add(
                LaneKey {
                    chain: row.chain,
                    slot: Some(n),
                    take: None,
                },
                rect,
                rect,
                &mut out,
            );
        }
    }
    out
}

/// Draws the lanes and handles what's done in them.
pub fn show_lanes(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    rows: &Rows,
) {
    if let Some((chain, target)) = session.take_touched_control() {
        popups::show_control(session, state, chain, target);
    }
    if !ui.input(|i| i.pointer.any_down()) {
        state.arr.auto.gesture = None;
    }
    let views = lane_views(session, lanes, axis, rows, state.arr.scroll.y);
    let order: Vec<LaneKey> = views.iter().map(|v| v.key).collect();
    gestures::keyboard(ui, session, state, &order);
    let all: Vec<(LaneKey, f32, f32)> = views
        .iter()
        .map(|v| (v.key, v.rect.top(), v.rect.bottom()))
        .collect();
    let snapped = views
        .iter()
        .map(|v| (v.key, v.target.is_some()))
        .collect::<Vec<_>>();
    for view in &views {
        if view.rect.intersect(lanes).height() <= 0.0 {
            continue;
        }
        paint_lane(ui, state, view, lanes);
        gestures::lane_input(ui, session, state, view, lanes, grid, &all, &snapped);
    }
    paint_time_selection(ui, state, &views, lanes);
    gestures::paint_value_tip(ui, session, state, &views, lanes);
}

fn stroke_color(automated: bool) -> Color32 {
    if automated {
        pal().arm_red
    } else {
        pal().text_faint
    }
}

fn paint_lane(ui: &egui::Ui, state: &UiState, view: &LaneView, lanes: Rect) {
    let p = ui.painter_at(view.rect.intersect(lanes));
    if view.key.slot.is_none() {
        // The envelope sits over the clips, which step back behind it.
        p.rect_filled(view.hit_rect, 0.0, pal().grid_bg.gamma_multiply(0.5));
    }
    p.line_segment(
        [view.rect.left_bottom(), view.rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    if state.arr.auto.focus == Some(view.key) {
        p.rect_filled(
            Rect::from_min_size(view.rect.min, Vec2::new(view.rect.width(), 1.0)),
            0.0,
            pal().accent,
        );
    }
    let Some(_) = view.target else { return };
    let color = stroke_color(!view.points.is_empty());
    if let Some(k) = view.dashed_knob() {
        let y = view.geom.y(k);
        p.add(egui::Shape::dashed_line(
            &[
                Pos2::new(view.rect.left(), y),
                Pos2::new(view.rect.right(), y),
            ],
            Stroke::new(1.0, color),
            5.0,
            4.0,
        ));
        return;
    }
    paint_envelope(&p, view, color, state);
}

fn paint_envelope(p: &egui::Painter, view: &LaneView, color: Color32, state: &UiState) {
    let g = &view.geom;
    let Some((first, last)) = view.points.first().zip(view.points.last()) else {
        return;
    };
    let stroke = Stroke::new(1.5, color);
    let (fp, lp) = (g.pos(first), g.pos(last));
    if fp.x > view.rect.left() {
        p.line_segment([Pos2::new(view.rect.left(), fp.y), fp], stroke);
    }
    if lp.x < view.rect.right() {
        p.line_segment([lp, Pos2::new(view.rect.right(), lp.y)], stroke);
    }
    let sel = in_sel(&state.arr.time, view.key);
    for (i, point) in view.points.iter().enumerate() {
        let at = g.pos(point);
        if i + 1 < view.points.len() {
            let path = geom::segment_path(&view.points, i, g);
            if path.len() == 2 {
                p.line_segment([path[0], path[1]], stroke);
            } else {
                p.add(egui::Shape::line(path, stroke));
            }
        }
        if at.x < view.rect.left() - 4.0 || at.x > view.rect.right() + 4.0 {
            continue;
        }
        let selected = sel.is_some_and(|(a, b)| point.time >= a && point.time <= b);
        p.circle_filled(at, if selected { 3.5 } else { 2.5 }, color);
        if selected {
            p.circle_stroke(at, 4.0, Stroke::new(1.0, pal().select));
        }
    }
}

fn paint_time_selection(ui: &egui::Ui, state: &UiState, views: &[LaneView], lanes: Rect) {
    let auto = &state.arr.auto;
    let Some(sel) = state.arr.time.as_ref().filter(|s| !s.is_point()) else {
        return;
    };
    let pointer = ui.input(|i| i.pointer.hover_pos());
    for view in views.iter().filter(|v| sel.lanes.contains(&v.key)) {
        let rect = selection_rect(view, sel);
        let p = ui.painter_at(view.rect.intersect(lanes));
        p.rect_filled(rect, 0.0, pal().select.gamma_multiply(0.22));
        p.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, pal().select),
            egui::StrokeKind::Inside,
        );
        if let Some(frame) = auto.frame.as_ref().filter(|f| f.key == view.key) {
            paint_outline(&p, view, frame);
        }
        let hovering = pointer.is_some_and(|pos| rect.expand(10.0).contains(pos));
        if hovering || matches!(&auto.gesture, Some(gestures::Gesture::Handle { .. })) {
            paint_handles(&p, rect);
        }
    }
}

fn selection_rect(view: &LaneView, sel: &TimeSel) -> Rect {
    Rect::from_min_max(
        Pos2::new(view.geom.axis.x(sel.start), view.hit_rect.top()),
        Pos2::new(view.geom.axis.x(sel.end), view.hit_rect.bottom()),
    )
}

fn paint_outline(p: &egui::Painter, view: &LaneView, frame: &Frame) {
    let g = &view.geom;
    let (x0, x1) = (g.axis.x(frame.span.0), g.axis.x(frame.span.1));
    let c = frame.corners;
    let outline = vec![
        Pos2::new(x0, g.y(1.0 + c.top_left)),
        Pos2::new(x1, g.y(1.0 + c.top_right)),
        Pos2::new(x1, g.y(c.bottom_right)),
        Pos2::new(x0, g.y(c.bottom_left)),
    ];
    p.add(egui::Shape::closed_line(
        outline,
        Stroke::new(1.0, pal().select),
    ));
}

fn paint_handles(p: &egui::Painter, rect: Rect) {
    let size = Vec2::splat(6.0);
    for at in [
        rect.left_top(),
        rect.center_top(),
        rect.right_top(),
        rect.left_center(),
        rect.right_center(),
        rect.left_bottom(),
        rect.center_bottom(),
        rect.right_bottom(),
    ] {
        p.rect_filled(Rect::from_center_size(at, size), 1.0, pal().select);
    }
}

/// The accumulated frame a handle gesture produces from the frame it started with.
fn apply_handle(
    start: &Frame,
    handle: Handle,
    mirrored: bool,
    dknob: f64,
    new_edge: Option<f64>,
) -> Frame {
    let mut f = start.clone();
    let c = &mut f.corners;
    match handle {
        Handle::Top => {
            c.top_left += dknob;
            c.top_right += dknob;
            if mirrored {
                c.bottom_left -= dknob;
                c.bottom_right -= dknob;
            }
        }
        Handle::Bottom => {
            c.bottom_left += dknob;
            c.bottom_right += dknob;
            if mirrored {
                c.top_left -= dknob;
                c.top_right -= dknob;
            }
        }
        Handle::TopLeft => {
            c.top_left += dknob;
            if mirrored {
                c.bottom_right -= dknob;
            }
        }
        Handle::TopRight => {
            c.top_right += dknob;
            if mirrored {
                c.bottom_left -= dknob;
            }
        }
        Handle::BottomLeft => {
            c.bottom_left += dknob;
            if mirrored {
                c.top_right -= dknob;
            }
        }
        Handle::BottomRight => {
            c.bottom_right += dknob;
            if mirrored {
                c.top_left -= dknob;
            }
        }
        Handle::Left | Handle::Right => {
            let (a, b) = start.span;
            let Some(edge) = new_edge else { return f };
            let min_len = 1.0;
            match handle {
                Handle::Left if mirrored => {
                    let d = edge - a;
                    let (left, right) = (a + d, b - d);
                    if right - left >= min_len {
                        f.span = (left.max(0.0), right);
                    }
                }
                Handle::Right if mirrored => {
                    let d = edge - b;
                    let (left, right) = (a - d, b + d);
                    if right - left >= min_len {
                        f.span = (left.max(0.0), right);
                    }
                }
                Handle::Left => f.span = (edge.min(b - min_len).max(0.0), b),
                _ => f.span = (a, edge.max(a + min_len)),
            }
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_project::Breakpoint;

    #[test]
    fn a_tap_on_b_toggles_and_a_hold_is_momentary() {
        let mut k = DrawKey::default();
        let on = k.pressed(false, 0.0);
        assert!(on);
        assert!(k.released(on, 0.05), "a tap stays on");
        let on = k.pressed(true, 1.0);
        assert!(!on, "tapping again turns it off");
        assert!(!k.released(on, 1.05));
        let on = k.pressed(false, 2.0);
        assert!(on);
        assert!(
            !k.released(on, 3.0),
            "held while off, it lasts as long as the key"
        );
    }

    #[test]
    fn repeats_of_a_held_key_do_nothing() {
        let mut k = DrawKey::default();
        assert!(k.pressed(false, 0.0));
        assert!(k.pressed(true, 0.1));
        assert!(k.pressed(true, 0.2));
    }

    fn frame() -> Frame {
        Frame {
            key: LaneKey {
                chain: ChainRef::Master,
                slot: None,
                take: None,
            },
            target: rlive_core::param::transport::tempo_target(),
            base: vec![Breakpoint::new(0.0, 0.5)],
            orig: (100.0, 200.0),
            span: (100.0, 200.0),
            corners: Corners::default(),
        }
    }

    #[test]
    fn a_mirrored_top_handle_moves_the_bottom_the_other_way() {
        let f = apply_handle(&frame(), Handle::Top, true, 0.2, None);
        assert_eq!(f.corners.top_left, 0.2);
        assert_eq!(f.corners.bottom_right, -0.2);
    }

    #[test]
    fn a_corner_moves_only_itself_unless_mirrored() {
        let f = apply_handle(&frame(), Handle::TopLeft, false, 0.2, None);
        assert_eq!(
            f.corners,
            Corners {
                top_left: 0.2,
                ..Corners::default()
            }
        );
        let f = apply_handle(&frame(), Handle::TopLeft, true, 0.2, None);
        assert_eq!(f.corners.bottom_right, -0.2);
    }

    #[test]
    fn the_side_handles_stretch_the_span_and_keep_it_positive() {
        let f = apply_handle(&frame(), Handle::Right, false, 0.0, Some(400.0));
        assert_eq!(f.span, (100.0, 400.0));
        let f = apply_handle(&frame(), Handle::Left, false, 0.0, Some(500.0));
        assert_eq!(f.span, (199.0, 200.0));
    }

    #[test]
    fn a_mirrored_side_handle_moves_both_edges_opposite_ways() {
        let f = apply_handle(&frame(), Handle::Left, true, 0.0, Some(80.0));
        assert_eq!(f.span, (80.0, 220.0));
    }

    #[test]
    fn a_frame_replays_its_transform_from_the_original_points() {
        let mut f = frame();
        f.base = vec![Breakpoint::new(100.0, 0.5), Breakpoint::new(200.0, 0.5)];
        f.corners.top_left = -0.5;
        let range = rlive_core::ParamRange {
            min: 0.0,
            max: 1.0,
            skew: 1.0,
            quantized: false,
        };
        let once = f.result(range);
        assert_eq!(once, f.result(range), "recomputes the same result");
        assert!(once[0].value < 0.5);
        assert_eq!(once.last().unwrap().value, 0.5);
    }
}
