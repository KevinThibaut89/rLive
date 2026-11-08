//! The gain of a clip's fades. They use the shared envelope curve, so a curve means the same on a
//! fade as on any other envelope, and they read the same on the audio thread and in the UI.

use crate::envelope::curve_shape;

/// How long a default fade or crossfade is, when Create Fades on Clip Edges is on.
pub const DEFAULT_EDGE_FADE_MS: f64 = 4.0;

/// How far a fade in has got at `x` of the way along it. A fade out is the same shape read
/// backwards, so a crossfade is two of them meeting in the middle.
#[inline]
pub fn fade_gain(x: f64, curve: f32) -> f32 {
    curve_shape(x, curve) as f32
}

/// Fades of one clip, in clip-local ticks. Before `in_from` it is silent, and likewise after
/// `out_to`. The default has no fades.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeFades {
    pub in_from: f64,
    pub in_to: f64,
    pub in_curve: f32,
    pub out_from: f64,
    pub out_to: f64,
    pub out_curve: f32,
}

impl EdgeFades {
    /// The gain at clip-local tick `local`. No allocation, no branches on the common path but
    /// two comparisons.
    #[inline]
    pub fn gain(&self, local: f64) -> f32 {
        let mut gain = 1.0;
        if local < self.in_to && self.in_to > self.in_from {
            let span = self.in_to - self.in_from;
            gain *= fade_gain(((local - self.in_from) / span).max(0.0), self.in_curve);
        }
        if local > self.out_from && self.out_to > self.out_from {
            let span = self.out_to - self.out_from;
            gain *= fade_gain(((self.out_to - local) / span).max(0.0), self.out_curve);
        }
        gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_fade_is_a_straight_line() {
        assert_eq!(fade_gain(0.0, 0.0), 0.0);
        assert_eq!(fade_gain(0.25, 0.0), 0.25);
        assert_eq!(fade_gain(1.0, 0.0), 1.0);
    }

    #[test]
    fn a_curve_bends_the_fade_but_not_its_ends() {
        for curve in [-1.0, -0.4, 0.3, 1.0] {
            assert!((fade_gain(0.0, curve)).abs() < 1e-6);
            assert!((fade_gain(1.0, curve) - 1.0).abs() < 1e-6);
        }
        assert!(fade_gain(0.5, 0.8) < 0.5, "stays near the start");
        assert!(fade_gain(0.5, -0.8) > 0.5, "rushes first");
    }

    #[test]
    fn no_fades_read_as_unity_everywhere() {
        let f = EdgeFades::default();
        for local in [-10.0, 0.0, 123.4, 1e9] {
            assert_eq!(f.gain(local), 1.0);
        }
    }

    #[test]
    fn in_and_out_regions_each_ramp_and_multiply_where_they_touch() {
        let f = EdgeFades {
            in_from: 0.0,
            in_to: 100.0,
            out_from: 300.0,
            out_to: 400.0,
            ..EdgeFades::default()
        };
        assert_eq!(f.gain(50.0), 0.5);
        assert_eq!(f.gain(200.0), 1.0);
        assert_eq!(f.gain(350.0), 0.5);
        assert_eq!(f.gain(-5.0), 0.0);
        assert_eq!(f.gain(500.0), 0.0);
    }
}
