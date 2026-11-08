//! Where things are in an automation lane, and what a pointer position is on.

use super::super::timeline::TimeAxis;
use egui::{Pos2, Rect, Vec2};
use rlive_core::envelope::segment_value;
use rlive_core::ParamRange;
use rlive_project::Breakpoint;

/// How close a pointer must be to a breakpoint to grab it.
const POINT_GRAB: f32 = 6.0;
/// How close to a line to be on it.
const LINE_GRAB: f32 = 3.5;
/// How close to be near a segment, which is where dragging moves it.
const NEAR_GRAB: f32 = 14.0;
/// How far a handle of a time selection reaches, in points.
const HANDLE_GRAB: f32 = 7.0;
/// Space kept above and below the envelope inside a lane.
const PAD: f32 = 4.0;
/// How many points a curved segment is drawn with at most.
const MAX_CURVE_STEPS: usize = 64;
/// How much slower a value moves with Shift held.
pub const FINE: f32 = 8.0;

/// A lane's screen space: time across, knob position up.
#[derive(Clone, Copy, Debug)]
pub struct LaneGeom {
    pub rect: Rect,
    pub axis: TimeAxis,
    pub range: ParamRange,
}

impl LaneGeom {
    fn height(&self) -> f32 {
        (self.rect.height() - 2.0 * PAD).max(1.0)
    }

    /// The screen height of a knob position, which may lie outside the lane.
    pub fn y(&self, knob: f64) -> f32 {
        self.rect.bottom() - PAD - knob as f32 * self.height()
    }

    /// The knob position at a screen height, kept on the knob.
    pub fn knob(&self, y: f32) -> f64 {
        (((self.rect.bottom() - PAD - y) / self.height()) as f64).clamp(0.0, 1.0)
    }

    /// How much of the knob a vertical distance is, moving up.
    pub fn knob_per_px(&self, dy: f32) -> f64 {
        -(dy / self.height()) as f64
    }

    pub fn pos(&self, p: &Breakpoint) -> Pos2 {
        Pos2::new(
            self.axis.x(p.time),
            self.y(self.range.to_normalized(p.value)),
        )
    }
}

/// The screen path of the segment after point `i`.
pub fn segment_path(points: &[Breakpoint], i: usize, geom: &LaneGeom) -> Vec<Pos2> {
    let (Some(a), Some(b)) = (points.get(i), points.get(i + 1)) else {
        return Vec::new();
    };
    let (pa, pb) = (geom.pos(a), geom.pos(b));
    if geom.range.quantized {
        return vec![pa, Pos2::new(pb.x, pa.y), pb];
    }
    if a.curve == 0.0 || b.time <= a.time {
        return vec![pa, pb];
    }
    let (ka, kb) = (
        geom.range.to_normalized(a.value),
        geom.range.to_normalized(b.value),
    );
    let steps = (((pb.x - pa.x).abs() / 3.0) as usize).clamp(2, MAX_CURVE_STEPS);
    (0..=steps)
        .map(|n| {
            let x = n as f64 / steps as f64;
            Pos2::new(
                pa.x + (pb.x - pa.x) * x as f32,
                geom.y(segment_value(ka, kb, x, a.curve)),
            )
        })
        .collect()
}

fn distance_to_path(path: &[Pos2], pos: Pos2) -> f32 {
    let mut best = f32::MAX;
    for pair in path.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let d = b - a;
        let t = if d.length_sq() > 0.0 {
            ((pos - a).dot(d) / d.length_sq()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min((a + d * t - pos).length());
    }
    best
}

/// What a pointer position is on in a lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EnvHit {
    Point(usize),
    /// The line, between points `i` and `i + 1`; `None` where it holds beyond the ends, or for
    /// the dashed line of a control without automation.
    Line(Option<usize>),
    /// Close to segment `i` but not on it.
    Near(usize),
    Background,
}

/// `dashed` is the knob position of the dashed line, when there are no points.
pub fn hit_test(points: &[Breakpoint], geom: &LaneGeom, pos: Pos2, dashed: Option<f64>) -> EnvHit {
    if points.is_empty() {
        return match dashed {
            Some(k) if (geom.y(k) - pos.y).abs() <= LINE_GRAB => EnvHit::Line(None),
            _ => EnvHit::Background,
        };
    }
    let near = |p: &Breakpoint| (geom.axis.x(p.time) - pos.x).abs() <= NEAR_GRAB;
    let first_near = points.partition_point(|p| geom.axis.x(p.time) < pos.x - NEAR_GRAB);
    let mut closest: Option<(usize, f32)> = None;
    for (i, p) in points.iter().enumerate().skip(first_near) {
        if !near(p) {
            break;
        }
        let d = (geom.pos(p) - pos).length();
        if d <= POINT_GRAB && closest.is_none_or(|(_, best)| d < best) {
            closest = Some((i, d));
        }
    }
    if let Some((i, _)) = closest {
        return EnvHit::Point(i);
    }
    let mut segment: Option<(usize, f32)> = None;
    let from = first_near.saturating_sub(1);
    for i in from..points.len().saturating_sub(1) {
        if geom.axis.x(points[i].time) > pos.x + NEAR_GRAB {
            break;
        }
        let d = distance_to_path(&segment_path(points, i, geom), pos);
        if segment.is_none_or(|(_, best)| d < best) {
            segment = Some((i, d));
        }
    }
    let (first, last) = (geom.pos(&points[0]), geom.pos(&points[points.len() - 1]));
    if (pos.x < first.x && (pos.y - first.y).abs() <= LINE_GRAB)
        || (pos.x > last.x && (pos.y - last.y).abs() <= LINE_GRAB)
    {
        return EnvHit::Line(None);
    }
    match segment {
        Some((i, d)) if d <= LINE_GRAB => EnvHit::Line(Some(i)),
        Some((i, d)) if d <= NEAR_GRAB => EnvHit::Near(i),
        _ => EnvHit::Background,
    }
}

/// A handle of a time selection's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Which handle of a selection's rectangle a position is on.
pub fn handle_at(rect: Rect, pos: Pos2) -> Option<Handle> {
    let expanded = rect.expand(HANDLE_GRAB);
    if !expanded.contains(pos) {
        return None;
    }
    let on_left = (pos.x - rect.left()).abs() <= HANDLE_GRAB;
    let on_right = (pos.x - rect.right()).abs() <= HANDLE_GRAB;
    let on_top = (pos.y - rect.top()).abs() <= HANDLE_GRAB;
    let on_bottom = (pos.y - rect.bottom()).abs() <= HANDLE_GRAB;
    let mid_x = (pos.x - rect.center().x).abs() <= HANDLE_GRAB;
    let mid_y = (pos.y - rect.center().y).abs() <= HANDLE_GRAB;
    Some(match (on_left, on_right, on_top, on_bottom) {
        (true, _, true, _) => Handle::TopLeft,
        (_, true, true, _) => Handle::TopRight,
        (true, _, _, true) => Handle::BottomLeft,
        (_, true, _, true) => Handle::BottomRight,
        (_, _, true, _) if mid_x => Handle::Top,
        (_, _, _, true) if mid_x => Handle::Bottom,
        (true, ..) if mid_y => Handle::Left,
        (_, true, ..) if mid_y => Handle::Right,
        _ => return None,
    })
}

/// How far a drag has gone, with Shift applied: it either locks the drag to the time axis or
/// slows the value down, whichever way the pointer is going when Shift comes down.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DragAccum {
    pub dx: f32,
    pub dy: f32,
    shift_moved: Vec2,
    fine: Option<bool>,
}

impl DragAccum {
    pub fn apply(&mut self, delta: Vec2, shift: bool) {
        if !shift {
            self.fine = None;
            self.shift_moved = Vec2::ZERO;
            self.dx += delta.x;
            self.dy += delta.y;
            return;
        }
        self.shift_moved += delta;
        if self.fine.is_none() && self.shift_moved.length() >= 2.0 {
            self.fine = Some(self.shift_moved.y.abs() > self.shift_moved.x.abs());
        }
        match self.fine {
            Some(true) => self.dy += delta.y / FINE,
            Some(false) => self.dx += delta.x,
            None => {}
        }
    }
}

/// A typed value: a plain number, with whatever unit the field showed. `-inf` is silence.
pub fn parse_value(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("-inf") {
        return Some(f64::NEG_INFINITY);
    }
    let end = t
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || (*i == 0 && (*c == '-' || *c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    t[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RANGE: ParamRange = ParamRange {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        quantized: false,
    };

    fn geom() -> LaneGeom {
        LaneGeom {
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 108.0)),
            axis: TimeAxis {
                left: 0.0,
                px_per_beat: 100.0,
                scroll: 0.0,
            },
            range: RANGE,
        }
    }

    fn line() -> Vec<Breakpoint> {
        vec![
            Breakpoint::new(0.0, 0.0),
            Breakpoint::new(960.0, 1.0),
            Breakpoint::new(1920.0, 0.5),
        ]
    }

    #[test]
    fn a_breakpoint_outranks_the_line_through_it() {
        let g = geom();
        assert_eq!(
            hit_test(&line(), &g, g.pos(&line()[1]), None),
            EnvHit::Point(1)
        );
    }

    #[test]
    fn on_the_line_is_the_line_and_beside_it_is_near() {
        let g = geom();
        let points = line();
        let mid = Pos2::new(g.axis.x(480.0), g.y(0.5));
        assert_eq!(hit_test(&points, &g, mid, None), EnvHit::Line(Some(0)));
        let near = mid + Vec2::new(0.0, 12.0);
        assert_eq!(hit_test(&points, &g, near, None), EnvHit::Near(0));
        let far = mid + Vec2::new(0.0, 40.0);
        assert_eq!(hit_test(&points, &g, far, None), EnvHit::Background);
    }

    #[test]
    fn the_line_holds_past_its_ends() {
        let g = geom();
        let points = vec![Breakpoint::new(960.0, 0.5)];
        let before = Pos2::new(g.axis.x(100.0), g.y(0.5));
        assert_eq!(hit_test(&points, &g, before, None), EnvHit::Line(None));
    }

    #[test]
    fn a_dashed_line_is_hit_like_a_line() {
        let g = geom();
        let on = Pos2::new(50.0, g.y(0.25));
        assert_eq!(hit_test(&[], &g, on, Some(0.25)), EnvHit::Line(None));
        assert_eq!(
            hit_test(&[], &g, on + Vec2::new(0.0, 20.0), Some(0.25)),
            EnvHit::Background
        );
    }

    #[test]
    fn a_stepped_segment_is_hit_along_its_hold() {
        let mut g = geom();
        g.range.quantized = true;
        let points = vec![Breakpoint::new(0.0, 0.0), Breakpoint::new(960.0, 1.0)];
        let hold = Pos2::new(g.axis.x(480.0), g.y(0.0));
        assert_eq!(hit_test(&points, &g, hold, None), EnvHit::Line(Some(0)));
    }

    #[test]
    fn hit_positions_are_the_inverse_of_drawing() {
        let g = geom();
        for k in [0.0, 0.3, 0.7, 1.0] {
            assert!((g.knob(g.y(k)) - k).abs() < 1e-4);
        }
    }

    #[test]
    fn handles_are_at_the_corners_and_the_centres() {
        let r = Rect::from_min_max(Pos2::new(100.0, 0.0), Pos2::new(300.0, 100.0));
        assert_eq!(handle_at(r, Pos2::new(100.0, 0.0)), Some(Handle::TopLeft));
        assert_eq!(
            handle_at(r, Pos2::new(300.0, 100.0)),
            Some(Handle::BottomRight)
        );
        assert_eq!(handle_at(r, Pos2::new(200.0, 0.0)), Some(Handle::Top));
        assert_eq!(handle_at(r, Pos2::new(100.0, 50.0)), Some(Handle::Left));
        assert_eq!(handle_at(r, Pos2::new(200.0, 50.0)), None);
    }

    #[test]
    fn shift_locks_sideways_drags_and_slows_vertical_ones() {
        let mut a = DragAccum::default();
        a.apply(Vec2::new(10.0, 4.0), false);
        assert_eq!((a.dx, a.dy), (10.0, 4.0));
        // Shift, moving mostly sideways: the value stays where it was.
        a.apply(Vec2::new(5.0, 1.0), true);
        a.apply(Vec2::new(5.0, 1.0), true);
        assert_eq!((a.dx, a.dy), (20.0, 4.0));
        // Off and on again, moving vertically: the value moves slowly.
        a.apply(Vec2::ZERO, false);
        a.apply(Vec2::new(0.0, 16.0), true);
        a.apply(Vec2::new(0.0, 8.0), true);
        assert_eq!((a.dx, a.dy), (20.0, 7.0));
    }

    #[test]
    fn typed_values_can_carry_units() {
        assert_eq!(parse_value("-6.5 dB"), Some(-6.5));
        assert_eq!(parse_value("440"), Some(440.0));
        assert_eq!(parse_value("120.00 BPM"), Some(120.0));
        assert_eq!(parse_value(" -inf "), Some(f64::NEG_INFINITY));
        assert_eq!(parse_value("off"), None);
    }
}
