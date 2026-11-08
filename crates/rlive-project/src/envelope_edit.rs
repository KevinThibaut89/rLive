//! What the automation lanes do to an envelope, as pure functions from one list of points to
//! another. Points are in plain units and sorted by time; two at one time make a step. Everything
//! that changes a value does so in knob position, which is the space segments run straight in.
//!
//! A drag recomputes from the points it started with, so whatever it passes over comes back if it
//! moves on, and only what's there at the drop is kept.

use crate::envelope::{as_knob, thin_recorded};
use crate::{Breakpoint, Envelope};
use rlive_core::envelope::{curve_shape, segment_value, split_curve, value_at_mapped, Interp};
use rlive_core::param::EnvTarget;
use rlive_core::ParamRange;
use std::collections::BTreeMap;
use std::ops::Range;

/// How far Simplify Envelope may stray from what was there, as a share of the knob's range.
pub const SIMPLIFY_TOLERANCE: f64 = 0.004;
/// How many straight pieces a sine shape is made of.
const SINE_STEPS: usize = 48;
/// How finely the best curve for a run of points is searched for.
const CURVE_SEARCH_STEPS: usize = 28;

/// The envelope's knob position at `tick`, or `None` if it has no points.
pub fn knob_at(points: &[Breakpoint], tick: f64, range: ParamRange) -> Option<f64> {
    value_at_mapped(points, tick, Interp::of(range.quantized), |v| {
        range.to_normalized(v)
    })
}

/// The envelope's plain value at `tick`.
pub fn value_at(points: &[Breakpoint], tick: f64, range: ParamRange) -> Option<f64> {
    knob_at(points, tick, range).map(|k| range.from_normalized(k))
}

/// A plain value moved along the knob, leaving it exactly as it was if it doesn't move.
fn nudged(value: f64, dknob: f64, range: ParamRange) -> f64 {
    if dknob == 0.0 {
        value
    } else {
        range.from_normalized(range.to_normalized(value) + dknob)
    }
}

/// Adds a breakpoint, or moves the one already at `tick`. A curved segment it lands on is cut so
/// the two halves keep the shape. Returns its index.
pub fn insert(points: &mut Vec<Breakpoint>, tick: f64, value: f64, range: ParamRange) -> usize {
    let at = points.partition_point(|p| p.time < tick);
    let value = range.clamp(value);
    if let Some(p) = points.get_mut(at).filter(|p| p.time == tick) {
        p.value = value;
        return at;
    }
    let mut new = Breakpoint::new(tick, value);
    if at > 0 && at < points.len() {
        let (a, b) = (points[at - 1], points[at]);
        let (left, right) = split_curve(a.curve, (tick - a.time) / (b.time - a.time));
        points[at - 1].curve = left;
        new.curve = right;
    }
    points.insert(at, new);
    at
}

/// Makes sure a point stands at `tick` on the envelope as it is, and returns its index. An empty
/// envelope has nothing to cut.
fn cut(points: &mut Vec<Breakpoint>, tick: f64, range: ParamRange) -> usize {
    if let Some(value) = value_at(points, tick, range) {
        insert(points, tick, value, range)
    } else {
        0
    }
}

/// The envelope with points at both ends of a span, and the indices of the points in it.
pub fn with_edges(
    points: &[Breakpoint],
    start: f64,
    end: f64,
    range: ParamRange,
) -> (Vec<Breakpoint>, Range<usize>) {
    let mut out = points.to_vec();
    if out.is_empty() {
        return (out, 0..0);
    }
    cut(&mut out, start, range);
    cut(&mut out, end, range);
    let first = out.partition_point(|p| p.time < start);
    let last = out.partition_point(|p| p.time <= end);
    (out, first..last)
}

/// Moves some points, by indices in order, in ticks and knob position. What they land on or pass
/// over goes, but only in the list this returns.
pub fn move_points(
    base: &[Breakpoint],
    moved: &[usize],
    dt: f64,
    dknob: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let (Some(&first), Some(&last)) = (moved.first(), moved.last()) else {
        return base.to_vec();
    };
    let (t0, t1) = (base[first].time, base[last].time);
    let dt = dt.max(-t0);
    let (n0, n1) = (t0 + dt, t1 + dt);
    let overwritten = |t: f64| {
        if dt > 0.0 {
            (t >= n0 && t <= n1) || (t > t1 && t <= n1)
        } else if dt < 0.0 {
            (t >= n0 && t <= n1) || (t >= n0 && t < t0)
        } else {
            false
        }
    };
    let mut out = Vec::with_capacity(base.len());
    for (i, p) in base.iter().enumerate() {
        if moved.binary_search(&i).is_ok() {
            out.push(Breakpoint {
                time: p.time + dt,
                value: nudged(p.value, dknob, range),
                curve: p.curve,
            });
        } else if !overwritten(p.time) {
            out.push(*p);
        }
    }
    out.sort_by(|a, b| a.time.total_cmp(&b.time));
    out
}

/// Moves a stretch of the envelope bounded by a time selection. Points are put on its edges and
/// go with it; moved sideways, the edges also keep points where they were.
pub fn move_span(
    base: &[Breakpoint],
    start: f64,
    end: f64,
    dt: f64,
    dknob: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let (mut points, inner) = with_edges(base, start, end, range);
    if inner.is_empty() {
        return points;
    }
    if dt == 0.0 {
        let moved: Vec<usize> = inner.collect();
        return move_points(&points, &moved, 0.0, dknob, range);
    }
    let left = points[inner.start];
    let right = points[inner.end - 1];
    points.insert(inner.end, right);
    points.insert(inner.start, left);
    let moved: Vec<usize> = (inner.start + 1..inner.end + 1).collect();
    move_points(&points, &moved, dt, dknob, range)
}

/// Whether a segment can be curved: only diagonal lines can.
pub fn can_curve(points: &[Breakpoint], i: usize, range: ParamRange) -> bool {
    let (Some(a), Some(b)) = (points.get(i), points.get(i + 1)) else {
        return false;
    };
    !range.quantized
        && b.time > a.time
        && (range.to_normalized(a.value) - range.to_normalized(b.value)).abs() > 1e-9
}

/// What a drag of `dy_px` screen points (down is positive) makes of a segment's curve, so that
/// the line follows the pointer whichever way the segment slopes.
pub fn curve_after_drag(curve: f32, rising: bool, dy_px: f32) -> f32 {
    const PER_PIXEL: f32 = 0.012;
    let change = dy_px * PER_PIXEL;
    (curve + if rising { change } else { -change }).clamp(-1.0, 1.0)
}

/// Gives a breakpoint an exact value. The others in `group` move by the same distance on the knob.
pub fn set_value_relative(
    base: &[Breakpoint],
    anchor: usize,
    group: &[usize],
    value: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let mut out = base.to_vec();
    let Some(a) = out.get_mut(anchor) else {
        return out;
    };
    let value = range.clamp(value);
    let dknob = range.to_normalized(value) - range.to_normalized(a.value);
    a.value = if range.quantized {
        value.round()
    } else {
        value
    };
    for &i in group {
        if i != anchor {
            if let Some(p) = out.get_mut(i) {
                p.value = nudged(p.value, dknob, range);
            }
        }
    }
    out
}

/// Takes the points out of a span, leaving a line between what the envelope was at its ends.
pub fn delete_range(
    points: &[Breakpoint],
    start: f64,
    end: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let (mut out, inner) = with_edges(points, start, end, range);
    if inner.len() >= 2 {
        out[inner.start].curve = 0.0;
        out.drain(inner.start + 1..inner.end - 1);
    }
    out
}

/// Takes `[start, end)` out of the timeline: what follows moves up to `start`, and the envelope
/// steps from what it was at `start` to what it was at `end`.
pub fn delete_time(
    points: &[Breakpoint],
    start: f64,
    end: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let (cut_points, inner) = with_edges(points, start, end, range);
    if cut_points.is_empty() || inner.is_empty() {
        return cut_points;
    }
    let shift = end - start;
    let mut out: Vec<Breakpoint> = Vec::with_capacity(cut_points.len());
    for (i, p) in cut_points.iter().enumerate() {
        if i < inner.start || i >= inner.end {
            out.push(Breakpoint {
                time: if p.time >= end {
                    p.time - shift
                } else {
                    p.time
                },
                ..*p
            });
        } else if i == inner.start {
            out.push(*p);
        } else if i == inner.end - 1 {
            out.push(Breakpoint { time: start, ..*p });
        }
    }
    out.sort_by(|a, b| a.time.total_cmp(&b.time));
    // A step between equal values is no step at all.
    out.dedup_by(|b, a| a.time == b.time && a.value == b.value);
    out
}

/// Opens up `len` empty ticks at `at`: the envelope holds its value across them.
pub fn insert_time(points: &[Breakpoint], at: f64, len: f64, range: ParamRange) -> Vec<Breakpoint> {
    let mut out = points.to_vec();
    if out.is_empty() {
        return out;
    }
    let i = cut(&mut out, at, range);
    if out[i].time != at {
        // The envelope has nothing of its own at or after `at`.
        for p in &mut out[i..] {
            p.time += len;
        }
        return out;
    }
    let hold = Breakpoint {
        time: at + len,
        curve: out[i].curve,
        ..out[i]
    };
    out[i].curve = 0.0;
    for p in &mut out[i + 1..] {
        p.time += len;
    }
    out.insert(i + 1, hold);
    out
}

/// A stretch of envelope on the clipboard: `points` have times from its start and knob positions
/// for values, so it can go onto a different parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeClip {
    pub length: f64,
    pub points: Vec<Breakpoint>,
}

pub fn copy_range(points: &[Breakpoint], start: f64, end: f64, range: ParamRange) -> RangeClip {
    let (cut, inner) = with_edges(points, start, end, range);
    RangeClip {
        length: end - start,
        points: as_knob(&cut[inner], range)
            .into_iter()
            .map(|p| Breakpoint {
                time: p.time - start,
                ..p
            })
            .collect(),
    }
}

/// Lays a copied stretch down at `at`, replacing what's there. An empty clip changes nothing.
pub fn paste_range(
    points: &[Breakpoint],
    clip: &RangeClip,
    at: f64,
    before: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let body: Vec<Breakpoint> = clip
        .points
        .iter()
        .map(|p| Breakpoint {
            time: at + p.time,
            value: range.from_normalized(p.value),
            curve: p.curve,
        })
        .collect();
    spliced(points, (at, at + clip.length), &body, before, range)
}

fn spliced(
    points: &[Breakpoint],
    span: (f64, f64),
    body: &[Breakpoint],
    before: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let mut envelope = Envelope {
        target: EnvTarget::MidiCtrl(0),
        points: points.to_vec(),
    };
    envelope.splice(span, body, before, range);
    envelope.points
}

/// A freely drawn or recorded stroke overwriting what it covers, thinned as recorded ones are.
pub fn overwrite(
    points: &[Breakpoint],
    stroke: &[Breakpoint],
    before: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let mut envelope = Envelope {
        target: EnvTarget::MidiCtrl(0),
        points: points.to_vec(),
    };
    envelope.overwrite(stroke, before, range);
    envelope.points
}

/// What Draw Mode has painted: a flat step per grid cell, in plain values.
pub fn grid_stroke(cells: &BTreeMap<i64, f64>, width: f64) -> Vec<Breakpoint> {
    let mut out = Vec::with_capacity(cells.len() * 2);
    for (&cell, &value) in cells {
        out.push(Breakpoint::new(cell as f64 * width, value));
        out.push(Breakpoint::new((cell + 1) as f64 * width, value));
    }
    out
}

/// What Draw Mode has painted without the grid: a point per cell of about a pixel.
pub fn smooth_stroke(cells: &BTreeMap<i64, f64>, width: f64) -> Vec<Breakpoint> {
    cells
        .iter()
        .map(|(&cell, &value)| Breakpoint::new(cell as f64 * width, value))
        .collect()
}

/// Paints the cells a stroke crossed between two moments of the drag, however fast it went.
pub fn paint_cells(cells: &mut BTreeMap<i64, f64>, from: Option<(i64, f64)>, to: (i64, f64)) {
    let Some((c0, v0)) = from.filter(|(c, _)| *c != to.0) else {
        cells.insert(to.0, to.1);
        return;
    };
    let (c1, v1) = to;
    let steps = (c1 - c0).abs();
    for i in 1..=steps {
        let x = i as f64 / steps as f64;
        let cell = c0 + (c1 - c0).signum() * i;
        cells.insert(cell, v0 + (v1 - v0) * x);
    }
}

/// The automation shapes of the lane's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    Sine,
    Triangle,
    Saw,
    InverseSaw,
    Square,
    /// From the value on the left of the selection straight to the one on the right.
    RampUp,
    /// From the value on the right of the selection straight back to the one on the left.
    RampDown,
    /// Left to right, starting slowly.
    CurveIn,
    /// Left to right, settling slowly.
    CurveOut,
    Adsr,
}

impl Shape {
    pub const WAVEFORMS: [Shape; 5] = [
        Shape::Sine,
        Shape::Triangle,
        Shape::Saw,
        Shape::InverseSaw,
        Shape::Square,
    ];
    pub const OTHERS: [Shape; 5] = [
        Shape::RampUp,
        Shape::RampDown,
        Shape::CurveIn,
        Shape::CurveOut,
        Shape::Adsr,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Sine => "Sine",
            Shape::Triangle => "Triangle",
            Shape::Saw => "Sawtooth",
            Shape::InverseSaw => "Inverse Sawtooth",
            Shape::Square => "Square",
            Shape::RampUp => "Ramp",
            Shape::RampDown => "Ramp Reversed",
            Shape::CurveIn => "Curved Ramp In",
            Shape::CurveOut => "Curved Ramp Out",
            Shape::Adsr => "ADSR",
        }
    }

    /// Only the square wave makes sense on a parameter with discrete states.
    pub fn allowed(self, quantized: bool) -> bool {
        !quantized || self == Shape::Square
    }

    /// The shape over `0..=1` in time and knob position, given the knob positions on its two
    /// sides. Waveforms ignore those.
    pub fn points(self, left: f64, right: f64) -> Vec<Breakpoint> {
        let p = Breakpoint::new;
        match self {
            Shape::Sine => (0..=SINE_STEPS)
                .map(|i| {
                    let x = i as f64 / SINE_STEPS as f64;
                    p(x, 0.5 + 0.5 * (x * std::f64::consts::TAU).sin())
                })
                .collect(),
            Shape::Triangle => vec![p(0.0, 0.0), p(0.5, 1.0), p(1.0, 0.0)],
            Shape::Saw => vec![p(0.0, 0.0), p(1.0, 1.0)],
            Shape::InverseSaw => vec![p(0.0, 1.0), p(1.0, 0.0)],
            Shape::Square => vec![p(0.0, 1.0), p(0.5, 1.0), p(0.5, 0.0), p(1.0, 0.0)],
            Shape::RampUp => vec![p(0.0, left), p(1.0, right)],
            Shape::RampDown => vec![p(0.0, right), p(1.0, left)],
            Shape::CurveIn => vec![
                Breakpoint {
                    curve: 0.6,
                    ..p(0.0, left)
                },
                p(1.0, right),
            ],
            Shape::CurveOut => vec![
                Breakpoint {
                    curve: -0.6,
                    ..p(0.0, left)
                },
                p(1.0, right),
            ],
            Shape::Adsr => vec![
                p(0.0, 0.0),
                p(0.1, 1.0),
                p(0.3, 0.6),
                p(0.7, 0.6),
                p(1.0, 0.0),
            ],
        }
    }
}

/// Inserts a shape over a span, scaled to it in time and to the parameter's range in value.
/// A shape the parameter can't take changes nothing.
pub fn insert_shape(
    base: &[Breakpoint],
    shape: Shape,
    start: f64,
    end: f64,
    before: f64,
    range: ParamRange,
) -> Vec<Breakpoint> {
    if !shape.allowed(range.quantized) || end <= start {
        return base.to_vec();
    }
    let side = |t: f64| knob_at(base, t, range).unwrap_or_else(|| range.to_normalized(before));
    let body: Vec<Breakpoint> = shape
        .points(side(start), side(end))
        .into_iter()
        .map(|p| Breakpoint {
            time: start + p.time * (end - start),
            value: range.from_normalized(p.value),
            curve: p.curve,
        })
        .collect();
    spliced(base, (start, end), &body, before, range)
}

/// Where the corners of a time selection's frame have been dragged to, in knob position, against
/// the frame's natural corners at the top and bottom of the lane.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Corners {
    pub top_left: f64,
    pub top_right: f64,
    pub bottom_left: f64,
    pub bottom_right: f64,
}

/// Stretches and skews the values in a span: both top corners together stretch it, one corner on
/// its own skews it.
pub fn reshape_values(
    base: &[Breakpoint],
    start: f64,
    end: f64,
    corners: Corners,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let (mut out, inner) = with_edges(base, start, end, range);
    if end <= start {
        return out;
    }
    for p in &mut out[inner] {
        let x = (p.time - start) / (end - start);
        let top = 1.0 + corners.top_left * (1.0 - x) + corners.top_right * x;
        let bottom = corners.bottom_left * (1.0 - x) + corners.bottom_right * x;
        let knob = range.to_normalized(p.value);
        p.value = range.from_normalized(bottom + (top - bottom) * knob);
    }
    out
}

/// Stretches a span sideways onto another one. Points outside it that the new span covers go.
pub fn stretch_time(
    base: &[Breakpoint],
    from: (f64, f64),
    to: (f64, f64),
    range: ParamRange,
) -> Vec<Breakpoint> {
    let (mut out, inner) = with_edges(base, from.0, from.1, range);
    let (n0, n1) = (to.0.max(0.0), to.1);
    if n1 <= n0 || from.1 <= from.0 {
        return out;
    }
    let scale = (n1 - n0) / (from.1 - from.0);
    let moved = inner.clone();
    for p in &mut out[inner] {
        p.time = n0 + (p.time - from.0) * scale;
    }
    let mut kept = Vec::with_capacity(out.len());
    for (i, p) in out.iter().enumerate() {
        if moved.contains(&i) || p.time < n0 || p.time > n1 {
            kept.push(*p);
        }
    }
    kept.sort_by(|a, b| a.time.total_cmp(&b.time));
    kept
}

/// Replaces runs of points with single straight or curved segments wherever that stays within
/// [`SIMPLIFY_TOLERANCE`], over a span or the whole envelope.
pub fn simplify(
    base: &[Breakpoint],
    span: Option<(f64, f64)>,
    range: ParamRange,
) -> Vec<Breakpoint> {
    let (mut out, inner) = match span {
        Some((start, end)) => with_edges(base, start, end, range),
        None => (base.to_vec(), 0..base.len()),
    };
    if inner.len() < 3 {
        return out;
    }
    let simplified = simplify_run(&out[inner.clone()], range);
    out.splice(inner, simplified);
    out
}

fn simplify_run(run: &[Breakpoint], range: ParamRange) -> Vec<Breakpoint> {
    if range.quantized {
        return thin_recorded(run, range);
    }
    let knob = as_knob(run, range);
    let mut out = vec![run[0]];
    let mut i = 0;
    while i + 1 < run.len() {
        let (next, curve) = furthest_fit(&knob, i);
        if let Some(last) = out.last_mut() {
            last.curve = curve;
        }
        out.push(run[next]);
        i = next;
    }
    out
}

/// The farthest point a single segment from `i` can reach, and that segment's curve.
fn furthest_fit(knob: &[Breakpoint], i: usize) -> (usize, f32) {
    let single = (i + 1, knob[i].curve);
    if knob[i + 1].time == knob[i].time {
        return (i + 1, 0.0);
    }
    // A step can't be part of a longer segment, so runs end at the first one.
    let limit = (i + 1..knob.len() - 1)
        .find(|&k| knob[k + 1].time == knob[k].time)
        .unwrap_or(knob.len() - 1);
    let mut best = single;
    let mut step = 1;
    while best.0 + step <= limit {
        let Some(curve) = fit(knob, i, best.0 + step) else {
            break;
        };
        best = (best.0 + step, curve);
        step *= 2;
    }
    while step > 0 {
        if best.0 + step <= limit {
            if let Some(curve) = fit(knob, i, best.0 + step) {
                best = (best.0 + step, curve);
            }
        }
        step /= 2;
    }
    best
}

/// The curve of one segment from `i` to `j` that stands in for what's between them, if any does.
fn fit(knob: &[Breakpoint], i: usize, j: usize) -> Option<f32> {
    let (a, b) = (knob[i], knob[j]);
    let span = b.time - a.time;
    // Each original segment is checked along its length as well as at its ends.
    let samples: Vec<(f64, f64)> = (i..j)
        .flat_map(|k| {
            let (p, q) = (knob[k], knob[k + 1]);
            let end = (k + 1 < j).then_some(((q.time - a.time) / span, q.value));
            [
                Some((
                    ((p.time + q.time) / 2.0 - a.time) / span,
                    segment_value(p.value, q.value, 0.5, p.curve),
                )),
                end,
            ]
        })
        .flatten()
        .collect();
    let error = |curve: f32| {
        samples
            .iter()
            .map(|&(x, y)| (a.value + (b.value - a.value) * curve_shape(x, curve) - y).abs())
            .fold(0.0, f64::max)
    };
    if error(0.0) <= SIMPLIFY_TOLERANCE {
        return Some(0.0);
    }
    if (a.value - b.value).abs() < 1e-9 {
        return None;
    }
    // Golden-section search over the curve.
    const GOLD: f32 = 0.618_034;
    let (mut lo, mut hi) = (-1.0_f32, 1.0_f32);
    let mut c = hi - GOLD * (hi - lo);
    let mut d = lo + GOLD * (hi - lo);
    let (mut fc, mut fd) = (error(c), error(d));
    for _ in 0..CURVE_SEARCH_STEPS {
        if fc < fd {
            hi = d;
            d = c;
            fd = fc;
            c = hi - GOLD * (hi - lo);
            fc = error(c);
        } else {
            lo = c;
            c = d;
            fc = fd;
            d = lo + GOLD * (hi - lo);
            fd = error(d);
        }
    }
    let curve = (lo + hi) / 2.0;
    (error(curve) <= SIMPLIFY_TOLERANCE).then_some(curve)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINEAR: ParamRange = ParamRange {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        quantized: false,
    };
    const TOGGLE: ParamRange = ParamRange {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        quantized: true,
    };

    fn pts(list: &[(f64, f64)]) -> Vec<Breakpoint> {
        list.iter().map(|&(t, v)| Breakpoint::new(t, v)).collect()
    }

    fn times(points: &[Breakpoint]) -> Vec<f64> {
        points.iter().map(|p| p.time).collect()
    }

    fn pairs(points: &[Breakpoint]) -> Vec<(f64, f64)> {
        points.iter().map(|p| (p.time, p.value)).collect()
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    #[test]
    fn inserting_keeps_a_curved_segment_where_it_was() {
        let mut points = vec![
            Breakpoint {
                curve: 0.7,
                ..Breakpoint::new(0.0, 0.0)
            },
            Breakpoint::new(100.0, 1.0),
        ];
        let original = points.clone();
        let value = value_at(&points, 30.0, LINEAR).unwrap();
        let at = insert(&mut points, 30.0, value, LINEAR);
        assert_eq!(at, 1);
        assert_eq!(points.len(), 3);
        for t in [5.0, 20.0, 29.0, 45.0, 80.0, 99.0] {
            let (a, b) = (
                value_at(&points, t, LINEAR).unwrap(),
                value_at(&original, t, LINEAR).unwrap(),
            );
            assert!((a - b).abs() < 1e-5, "at {t}: {a} {b}");
        }
    }

    #[test]
    fn inserting_on_a_breakpoint_just_moves_it() {
        let mut points = pts(&[(0.0, 0.0), (100.0, 1.0)]);
        assert_eq!(insert(&mut points, 100.0, 0.25, LINEAR), 1);
        assert_eq!(pairs(&points), [(0.0, 0.0), (100.0, 0.25)]);
    }

    #[test]
    fn a_new_point_is_held_to_the_parameter_range() {
        let mut points = Vec::new();
        insert(&mut points, 10.0, 7.0, LINEAR);
        assert_eq!(pairs(&points), [(10.0, 1.0)]);
    }

    #[test]
    fn moving_a_point_changes_only_it() {
        let base = pts(&[(0.0, 0.2), (100.0, 0.5), (200.0, 0.8)]);
        let moved = move_points(&base, &[1], 20.0, 0.1, LINEAR);
        assert_eq!(pairs(&moved), [(0.0, 0.2), (120.0, 0.6), (200.0, 0.8)]);
    }

    #[test]
    fn dragging_through_neighbours_only_removes_them_at_the_drop() {
        let base = pts(&[(0.0, 0.2), (100.0, 0.5), (200.0, 0.8), (300.0, 0.1)]);
        let across = move_points(&base, &[1], 150.0, 0.0, LINEAR);
        assert_eq!(times(&across), [0.0, 250.0, 300.0]);
        // Dragged back, the neighbour is there again: the drag recomputes from where it started.
        let back = move_points(&base, &[1], 0.0, 0.0, LINEAR);
        assert_eq!(back, base);
    }

    #[test]
    fn a_group_cant_leave_the_timeline_and_values_stay_on_the_knob() {
        let base = pts(&[(50.0, 0.9), (100.0, 0.95)]);
        let moved = move_points(&base, &[0, 1], -80.0, 0.5, LINEAR);
        assert_eq!(pairs(&moved), [(0.0, 1.0), (50.0, 1.0)]);
    }

    #[test]
    fn a_selected_segment_brings_its_edges_along_when_it_moves_vertically() {
        let base = pts(&[(0.0, 0.5), (400.0, 0.5)]);
        let moved = move_span(&base, 100.0, 300.0, 0.0, 0.25, LINEAR);
        assert_eq!(
            pairs(&moved),
            [(0.0, 0.5), (100.0, 0.75), (300.0, 0.75), (400.0, 0.5)]
        );
    }

    #[test]
    fn moving_a_segment_out_of_the_selection_leaves_points_at_its_edges() {
        let base = pts(&[(0.0, 0.5), (1000.0, 0.5)]);
        let moved = move_span(&base, 100.0, 200.0, 400.0, 0.2, LINEAR);
        assert_eq!(
            pairs(&moved),
            [
                (0.0, 0.5),
                (100.0, 0.5),
                (200.0, 0.5),
                (500.0, 0.7),
                (600.0, 0.7),
                (1000.0, 0.5)
            ]
        );
    }

    #[test]
    fn only_diagonal_segments_can_curve() {
        let points = pts(&[(0.0, 0.0), (100.0, 1.0), (200.0, 1.0), (200.0, 0.0)]);
        assert!(can_curve(&points, 0, LINEAR));
        assert!(!can_curve(&points, 1, LINEAR), "flat");
        assert!(!can_curve(&points, 2, LINEAR), "vertical");
        assert!(!can_curve(&points, 0, TOGGLE), "stepped");
        assert!(!can_curve(&points, 9, LINEAR));
    }

    #[test]
    fn a_curve_follows_the_pointer_whichever_way_the_segment_slopes() {
        // Dragging up bows the line up: a rising line rushes, a falling one lingers.
        assert!(curve_after_drag(0.0, true, -30.0) < 0.0);
        assert!(curve_after_drag(0.0, false, -30.0) > 0.0);
        assert_eq!(curve_after_drag(0.9, false, -1000.0), 1.0);
    }

    #[test]
    fn typing_a_value_moves_the_rest_of_the_selection_relatively() {
        let base = pts(&[(0.0, 0.2), (100.0, 0.3), (200.0, 0.8)]);
        let out = set_value_relative(&base, 1, &[0, 1], 0.5, LINEAR);
        close(out[1].value, 0.5);
        close(out[0].value, 0.4);
        close(out[2].value, 0.8);
    }

    #[test]
    fn typed_values_are_kept_in_range_and_whole_on_a_stepped_parameter() {
        let base = pts(&[(0.0, 0.0)]);
        assert_eq!(
            set_value_relative(&base, 0, &[0], 3.0, TOGGLE)[0].value,
            1.0
        );
    }

    #[test]
    fn deleting_a_span_leaves_a_line_between_its_ends() {
        let base = pts(&[(0.0, 0.0), (100.0, 1.0), (200.0, 0.0), (400.0, 1.0)]);
        let out = delete_range(&base, 50.0, 300.0, LINEAR);
        assert_eq!(
            pairs(&out),
            [(0.0, 0.0), (50.0, 0.5), (300.0, 0.5), (400.0, 1.0)]
        );
    }

    #[test]
    fn copy_and_paste_round_trips_and_leaves_the_neighbours() {
        let base = pts(&[(0.0, 0.0), (100.0, 1.0), (200.0, 0.0), (1000.0, 0.5)]);
        let clip = copy_range(&base, 0.0, 200.0, LINEAR);
        assert_eq!(clip.length, 200.0);
        assert_eq!(
            pairs(&clip.points),
            [(0.0, 0.0), (100.0, 1.0), (200.0, 0.0)]
        );
        let out = paste_range(&base, &clip, 400.0, 0.0, LINEAR);
        close(value_at(&out, 500.0, LINEAR).unwrap(), 1.0);
        close(value_at(&out, 400.0, LINEAR).unwrap(), 0.0);
        // At its very end a paste hands back to the old envelope, as any step does.
        close(value_at(&out, 600.0, LINEAR).unwrap(), 0.25);
        assert!(value_at(&out, 599.0, LINEAR).unwrap() < 0.02);
        // Outside the paste, the envelope is what it was.
        close(value_at(&out, 300.0, LINEAR).unwrap(), 0.0625);
        close(value_at(&out, 800.0, LINEAR).unwrap(), 0.375);
    }

    #[test]
    fn a_copied_span_can_go_onto_another_parameter() {
        let gain = ParamRange {
            min: -10.0,
            max: 10.0,
            skew: 1.0,
            quantized: false,
        };
        let clip = copy_range(&pts(&[(0.0, 0.0), (100.0, 0.5)]), 0.0, 100.0, LINEAR);
        let out = paste_range(&[], &clip, 0.0, 0.0, gain);
        assert_eq!(pairs(&out), [(0.0, -10.0), (100.0, 0.0)]);
    }

    #[test]
    fn a_stroke_paints_each_grid_cell_it_crossed() {
        let mut cells = BTreeMap::new();
        paint_cells(&mut cells, None, (2, 0.0));
        paint_cells(&mut cells, Some((2, 0.0)), (5, 0.3));
        assert_eq!(cells.keys().copied().collect::<Vec<_>>(), [2, 3, 4, 5]);
        close(cells[&4], 0.2);
    }

    #[test]
    fn drawn_steps_are_as_wide_as_the_grid() {
        let cells: BTreeMap<i64, f64> = [(1, 0.25), (2, 0.75)].into();
        assert_eq!(
            pairs(&grid_stroke(&cells, 240.0)),
            [(240.0, 0.25), (480.0, 0.25), (480.0, 0.75), (720.0, 0.75)]
        );
    }

    #[test]
    fn drawing_without_the_grid_is_smooth() {
        let cells: BTreeMap<i64, f64> = [(10, 0.25), (11, 0.3)].into();
        assert_eq!(
            pairs(&smooth_stroke(&cells, 2.0)),
            [(20.0, 0.25), (22.0, 0.3)]
        );
    }

    #[test]
    fn waveforms_fill_the_range_and_the_span() {
        for shape in Shape::WAVEFORMS {
            let out = insert_shape(&[], shape, 100.0, 300.0, 0.5, LINEAR);
            let (first, last) = (out.first().unwrap(), out.last().unwrap());
            assert!(first.time >= 0.0 && last.time <= 300.0, "{shape:?}");
            let knobs: Vec<f64> = out.iter().map(|p| p.value).collect();
            let max = knobs.iter().copied().fold(0.0, f64::max);
            let min = knobs.iter().copied().fold(1.0, f64::min);
            assert!(max > 0.9 && min < 0.1, "{shape:?}: {min} {max}");
        }
    }

    #[test]
    fn ramps_start_and_end_on_the_surrounding_values() {
        let base = pts(&[(0.0, 0.25), (100.0, 0.25), (300.0, 0.75), (400.0, 0.75)]);
        let up = insert_shape(&base, Shape::RampUp, 100.0, 300.0, 0.0, LINEAR);
        close(value_at(&up, 100.0, LINEAR).unwrap(), 0.25);
        close(value_at(&up, 200.0, LINEAR).unwrap(), 0.5);
        close(value_at(&up, 300.0, LINEAR).unwrap(), 0.75);
        // Elsewhere the same ramp comes out differently, because its surroundings do.
        let flat = pts(&[(0.0, 0.5), (400.0, 0.5)]);
        let elsewhere = insert_shape(&flat, Shape::RampUp, 100.0, 300.0, 0.0, LINEAR);
        close(value_at(&elsewhere, 200.0, LINEAR).unwrap(), 0.5);
    }

    #[test]
    fn the_curved_ramps_bow_opposite_ways() {
        let flat = pts(&[(0.0, 0.0), (100.0, 0.0), (300.0, 1.0)]);
        let slow = insert_shape(&flat, Shape::CurveIn, 100.0, 300.0, 0.0, LINEAR);
        let fast = insert_shape(&flat, Shape::CurveOut, 100.0, 300.0, 0.0, LINEAR);
        assert!(value_at(&slow, 200.0, LINEAR).unwrap() < 0.5);
        assert!(value_at(&fast, 200.0, LINEAR).unwrap() > 0.5);
    }

    #[test]
    fn adsr_goes_up_down_holds_and_releases() {
        let out = insert_shape(&[], Shape::Adsr, 0.0, 1000.0, 0.0, LINEAR);
        close(value_at(&out, 100.0, LINEAR).unwrap(), 1.0);
        close(value_at(&out, 500.0, LINEAR).unwrap(), 0.6);
        close(value_at(&out, 1000.0, LINEAR).unwrap(), 0.0);
    }

    #[test]
    fn a_discrete_parameter_only_takes_the_square() {
        for shape in Shape::WAVEFORMS.iter().chain(&Shape::OTHERS) {
            let out = insert_shape(&[], *shape, 0.0, 100.0, 0.0, TOGGLE);
            assert_eq!(out.is_empty(), *shape != Shape::Square, "{shape:?}");
        }
    }

    #[test]
    fn stretching_values_scales_towards_the_opposite_edge() {
        let base = pts(&[(0.0, 0.5), (100.0, 1.0)]);
        // The top comes down by a quarter: everything shrinks towards the bottom of the lane.
        let corners = Corners {
            top_left: -0.25,
            top_right: -0.25,
            ..Corners::default()
        };
        let out = reshape_values(&base, 0.0, 100.0, corners, LINEAR);
        assert_eq!(pairs(&out), [(0.0, 0.375), (100.0, 0.75)]);
    }

    #[test]
    fn a_corner_skews_the_values_along_the_selection() {
        let base = pts(&[(0.0, 1.0), (100.0, 1.0)]);
        let corners = Corners {
            top_left: -0.5,
            ..Corners::default()
        };
        let out = reshape_values(&base, 0.0, 100.0, corners, LINEAR);
        close(out[0].value, 0.5);
        close(out[1].value, 1.0);
    }

    #[test]
    fn stretching_time_moves_the_edge_and_removes_what_it_covers() {
        let base = pts(&[(0.0, 0.0), (100.0, 0.5), (200.0, 1.0), (300.0, 0.2)]);
        let out = stretch_time(&base, (0.0, 200.0), (0.0, 400.0), LINEAR);
        assert_eq!(pairs(&out), [(0.0, 0.0), (200.0, 0.5), (400.0, 1.0)]);
    }

    #[test]
    fn simplifying_a_line_leaves_its_ends() {
        let base: Vec<_> = (0..=200)
            .map(|i| Breakpoint::new(i as f64, i as f64 / 200.0))
            .collect();
        let out = simplify(&base, None, LINEAR);
        assert_eq!(pairs(&out), [(0.0, 0.0), (200.0, 1.0)]);
    }

    #[test]
    fn simplifying_finds_a_single_curved_segment() {
        let base: Vec<_> = (0..=200)
            .map(|i| Breakpoint::new(i as f64, curve_shape(i as f64 / 200.0, -0.6)))
            .collect();
        let out = simplify(&base, None, LINEAR);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!((out[0].curve + 0.6).abs() < 0.05, "{}", out[0].curve);
    }

    #[test]
    fn simplifying_stays_within_tolerance_of_the_original() {
        let base: Vec<_> = (0..=2000)
            .map(|i| {
                let t = i as f64;
                Breakpoint::new(t, 0.5 + 0.4 * (t * 0.01).sin())
            })
            .collect();
        let out = simplify(&base, None, LINEAR);
        assert!(out.len() < base.len() / 10, "{}", out.len());
        for p in &base {
            let v = value_at(&out, p.time, LINEAR).unwrap();
            assert!(
                (v - p.value).abs() < SIMPLIFY_TOLERANCE * 1.5,
                "at {}",
                p.time
            );
        }
    }

    #[test]
    fn simplifying_keeps_steps_and_works_inside_a_span() {
        let mut base = pts(&[(0.0, 0.0), (100.0, 0.25), (200.0, 0.5), (300.0, 0.75)]);
        base.extend(pts(&[(300.0, 0.0), (400.0, 0.0)]));
        let out = simplify(&base, None, LINEAR);
        assert_eq!(times(&out), [0.0, 300.0, 300.0, 400.0]);
        let within = simplify(&base[..4], Some((100.0, 300.0)), LINEAR);
        assert_eq!(times(&within), [0.0, 100.0, 300.0]);
    }

    #[test]
    fn simplifying_a_stepped_envelope_keeps_only_changes() {
        let base = pts(&[(0.0, 0.0), (10.0, 0.0), (20.0, 1.0), (30.0, 1.0)]);
        assert_eq!(times(&simplify(&base, None, TOGGLE)), [0.0, 20.0]);
    }
}
