//! How an envelope gets from one breakpoint to the next. Every envelope in rLive (automation,
//! clip envelopes, tempo, per-note expression) shares this one definition, so a curve drawn
//! anywhere means the same thing everywhere.
//!
//! - Points are sorted by time. Before the first point the value is the first point's, after the
//!   last it is the last point's.
//! - Two points at one time make a step. The envelope arrives at the first and leaves from the
//!   last, so at exactly that time it reads the last.
//! - A segment's `curve` is in `-1..=1`. Zero is a straight line. Above zero the value stays near
//!   its start and rushes to the end; below zero it rushes first and settles into the end. A curve
//!   and its negative are the same shape turned half way round.
//! - The shape is `(e^(k x) - 1) / (e^k - 1)` with `k = curve * CURVE_STEEPNESS`. It meets both
//!   ends exactly, never overshoots, and any part of it is the same shape again, which
//!   [`split_curve`] uses so that cutting a curved segment doesn't change how it sounds.
//! - A [`Interp::Stepped`] envelope, for parameters that only have discrete states, holds each
//!   point's value until the next point. A curve means nothing on it.

/// How hard a curve of 1 bends: the shape's exponent at full curve.
pub const CURVE_STEEPNESS: f64 = 6.0;

/// How an envelope moves between points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interp {
    /// Diagonal segments, straight or curved.
    #[default]
    Smooth,
    /// Holds each value until the next point.
    Stepped,
}

impl Interp {
    /// How the envelope of a parameter moves, given whether it only has whole values.
    pub fn of(quantized: bool) -> Self {
        if quantized {
            Self::Stepped
        } else {
            Self::Smooth
        }
    }
}

/// A point an envelope can be read from.
pub trait EnvPoint {
    fn time(&self) -> f64;
    fn value(&self) -> f64;
    /// Shape of the segment leaving this point.
    fn curve(&self) -> f32 {
        0.0
    }
}

/// How far along `0..=1` a value has got at `x` of the way along a segment.
pub fn curve_shape(x: f64, curve: f32) -> f64 {
    let x = x.clamp(0.0, 1.0);
    let curve = if curve.is_finite() {
        curve.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let k = f64::from(curve) * CURVE_STEEPNESS;
    if k.abs() < 1e-9 {
        x
    } else {
        (k * x).exp_m1() / k.exp_m1()
    }
}

/// The value `x` of the way along a segment from `from` to `to`.
#[inline]
pub fn segment_value(from: f64, to: f64, x: f64, curve: f32) -> f64 {
    from + (to - from) * curve_shape(x, curve)
}

/// The curves of the two parts of a segment cut `x` of the way along it. They trace exactly what
/// the whole segment did.
pub fn split_curve(curve: f32, x: f64) -> (f32, f32) {
    let x = x.clamp(0.0, 1.0) as f32;
    (curve * x, curve * (1.0 - x))
}

/// The envelope's value at `time`, or `None` if it has no points. `points` must be sorted by
/// time. Allocates nothing, so the audio thread can read envelopes with it.
pub fn value_at<P: EnvPoint>(points: &[P], time: f64, interp: Interp) -> Option<f64> {
    value_at_mapped(points, time, interp, |value| value)
}

/// [`value_at`], with each point's value passed through `map` first. Parameters use this to run
/// their segments straight in knob position while their points stay in plain units.
pub fn value_at_mapped<P: EnvPoint>(
    points: &[P],
    time: f64,
    interp: Interp,
    map: impl Fn(f64) -> f64,
) -> Option<f64> {
    let next = points.partition_point(|p| p.time() <= time);
    let Some(prev) = next.checked_sub(1).map(|i| &points[i]) else {
        return points.first().map(|p| map(p.value()));
    };
    let Some(to) = points.get(next).filter(|_| interp == Interp::Smooth) else {
        return Some(map(prev.value()));
    };
    let x = (time - prev.time()) / (to.time() - prev.time());
    Some(segment_value(
        map(prev.value()),
        map(to.value()),
        x,
        prev.curve(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct P(f64, f64, f32);

    impl EnvPoint for P {
        fn time(&self) -> f64 {
            self.0
        }
        fn value(&self) -> f64 {
            self.1
        }
        fn curve(&self) -> f32 {
            self.2
        }
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn a_straight_segment_is_a_line() {
        for x in [0.0, 0.25, 0.5, 0.9, 1.0] {
            close(curve_shape(x, 0.0), x);
        }
        close(segment_value(10.0, 20.0, 0.5, 0.0), 15.0);
        close(segment_value(10.0, -10.0, 0.25, 0.0), 5.0);
    }

    #[test]
    fn a_curve_meets_both_ends_and_never_overshoots() {
        for curve in [-1.0, -0.5, 0.25, 1.0] {
            close(curve_shape(0.0, curve), 0.0);
            close(curve_shape(1.0, curve), 1.0);
            let mut last = 0.0;
            for i in 1..=100 {
                let y = curve_shape(i as f64 / 100.0, curve);
                assert!((0.0..=1.0).contains(&y), "{y}");
                assert!(y >= last, "falls back at {i} with curve {curve}");
                last = y;
            }
        }
    }

    #[test]
    fn positive_curves_start_slow_and_negative_ones_fast() {
        assert!(curve_shape(0.5, 0.5) < 0.5);
        assert!(curve_shape(0.5, -0.5) > 0.5);
        assert!(curve_shape(0.5, 1.0) < curve_shape(0.5, 0.5));
    }

    #[test]
    fn a_curve_and_its_negative_are_a_half_turn_apart() {
        for curve in [0.1f32, 0.5, 1.0] {
            for x in [0.1, 0.3, 0.7] {
                close(curve_shape(x, curve), 1.0 - curve_shape(1.0 - x, -curve));
            }
        }
    }

    #[test]
    fn nonsense_curves_are_harmless() {
        close(curve_shape(0.5, f32::NAN), 0.5);
        close(curve_shape(0.5, 7.0), curve_shape(0.5, 1.0));
        close(curve_shape(0.5, f32::INFINITY), 0.5);
        close(curve_shape(0.5, 1e-12), 0.5);
    }

    #[test]
    fn cutting_a_curved_segment_does_not_move_it() {
        let (curve, cut) = (0.8, 0.3);
        let (left, right) = split_curve(curve, cut);
        let at_cut = segment_value(2.0, 10.0, cut, curve);
        for i in 0..=10 {
            let x = i as f64 / 10.0;
            // Curves are `f32`, which is the limit of how exact this can be.
            let whole = segment_value(2.0, 10.0, x * cut, curve);
            assert!((segment_value(2.0, at_cut, x, left) - whole).abs() < 1e-6);
            let whole = segment_value(2.0, 10.0, cut + x * (1.0 - cut), curve);
            assert!((segment_value(at_cut, 10.0, x, right) - whole).abs() < 1e-6);
        }
    }

    #[test]
    fn an_envelope_holds_past_its_ends() {
        let points = [P(10.0, 1.0, 0.0), P(20.0, 3.0, 0.0)];
        assert_eq!(value_at(&points, 0.0, Interp::Smooth), Some(1.0));
        assert_eq!(value_at(&points, 25.0, Interp::Smooth), Some(3.0));
        close(value_at(&points, 15.0, Interp::Smooth).unwrap(), 2.0);
        assert_eq!(value_at::<P>(&[], 1.0, Interp::Smooth), None);
    }

    #[test]
    fn two_points_at_one_time_make_a_step_that_reads_the_last() {
        let points = [
            P(0.0, 0.0, 0.0),
            P(10.0, 5.0, 0.0),
            P(10.0, 9.0, 0.0),
            P(20.0, 9.0, 0.0),
        ];
        assert_eq!(value_at(&points, 10.0, Interp::Smooth), Some(9.0));
        close(value_at(&points, 9.0, Interp::Smooth).unwrap(), 4.5);
        close(value_at(&points, 11.0, Interp::Smooth).unwrap(), 9.0);
    }

    #[test]
    fn a_stepped_envelope_holds_and_ignores_curves() {
        let points = [P(0.0, 0.0, 0.7), P(10.0, 1.0, 0.0), P(20.0, 2.0, 0.0)];
        assert_eq!(value_at(&points, 9.9, Interp::Stepped), Some(0.0));
        assert_eq!(value_at(&points, 10.0, Interp::Stepped), Some(1.0));
        assert_eq!(value_at(&points, 30.0, Interp::Stepped), Some(2.0));
    }

    #[test]
    fn the_segment_after_a_point_uses_that_points_curve() {
        let points = [P(0.0, 0.0, 1.0), P(10.0, 1.0, 0.0)];
        close(
            value_at(&points, 5.0, Interp::Smooth).unwrap(),
            curve_shape(0.5, 1.0),
        );
    }
}
