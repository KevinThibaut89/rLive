//! The crossfader's seven curves (manual pp. 404-405).

/// A crossfader curve. The chart on p. 405 lists them in this order, which is also how they are
/// numbered in the project file; the context menu on p. 404 lists them as [`MENU`](Self::MENU).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossfadeCurve {
    Transition,
    Dipped,
    Intermediate,
    #[default]
    ConstantPower,
    SlowFade,
    SlowCut,
    FastCut,
}

impl CrossfadeCurve {
    pub const ALL: [Self; 7] = [
        Self::Transition,
        Self::Dipped,
        Self::Intermediate,
        Self::ConstantPower,
        Self::SlowFade,
        Self::SlowCut,
        Self::FastCut,
    ];

    /// The curves in the order the crossfader's context menu shows them.
    pub const MENU: [Self; 7] = [
        Self::Dipped,
        Self::Intermediate,
        Self::ConstantPower,
        Self::SlowFade,
        Self::SlowCut,
        Self::FastCut,
        Self::Transition,
    ];

    pub fn from_index(index: u8) -> Self {
        Self::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(Self::ConstantPower)
    }

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Transition => "Transition",
            Self::Dipped => "Dipped",
            Self::Intermediate => "Intermediate",
            Self::ConstantPower => "Constant Power",
            Self::SlowFade => "Slow Fade",
            Self::SlowCut => "Slow Cut",
            Self::FastCut => "Fast Cut",
        }
    }

    /// The gain of a track when the fader is `t` of the way from the far end to the track's own
    /// end: 0 at the far end, where it is muted, 1 at its own end, where it is unattenuated.
    pub fn gain(self, t: f32) -> f32 {
        use std::f32::consts::{FRAC_PI_2, PI};
        let t = t.clamp(0.0, 1.0);
        let sine = |x: f32| (x * FRAC_PI_2).sin();
        let knee = |k: f32| ((t / k).min(1.0) * FRAC_PI_2).sin();
        match self {
            // Both sides are at full level exactly at the centre.
            Self::Transition => {
                if t >= 0.5 {
                    1.0
                } else {
                    (t * PI).sin()
                }
            }
            Self::Dipped => sine(t).powi(2),
            Self::Intermediate => sine(t).powf(1.5),
            Self::ConstantPower => sine(t),
            Self::SlowFade => knee(0.75),
            Self::SlowCut => knee(0.3),
            Self::FastCut => (t / 0.03).min(1.0),
        }
    }

    /// The gains of the A side and the B side at a crossfader position, -1 (A) to 1 (B).
    pub fn gains(self, position: f32) -> (f32, f32) {
        let p = position.clamp(-1.0, 1.0);
        (self.gain((1.0 - p) * 0.5), self.gain((1.0 + p) * 0.5))
    }
}

#[cfg(test)]
mod tests {
    use super::CrossfadeCurve as C;

    #[test]
    fn every_curve_silences_the_far_end_and_opens_the_near_one() {
        for curve in C::ALL {
            assert_eq!(curve.gains(-1.0), (1.0, 0.0), "{}", curve.name());
            assert_eq!(curve.gains(1.0), (0.0, 1.0), "{}", curve.name());
        }
    }

    #[test]
    fn every_curve_rises_and_is_symmetric() {
        for curve in C::ALL {
            let mut last = 0.0;
            for i in 0..=100 {
                let g = curve.gain(i as f32 / 100.0);
                assert!(g >= last - 1e-5, "{}", curve.name());
                last = g;
            }
            let (a, b) = curve.gains(0.4);
            let (mirror_a, mirror_b) = curve.gains(-0.4);
            assert_eq!((a, b), (mirror_b, mirror_a), "{}", curve.name());
        }
    }

    #[test]
    fn the_curves_have_the_power_at_the_centre_of_the_chart() {
        let power = |curve: C| {
            let (a, b) = curve.gains(0.0);
            a * a + b * b
        };
        assert!(power(C::Transition) > 1.9, "both sides full at the centre");
        assert!(power(C::Dipped) < power(C::Intermediate), "the deepest dip");
        assert!(power(C::Intermediate) < 0.95, "dips");
        assert!((power(C::ConstantPower) - 1.0).abs() < 1e-5, "flat");
        assert!(power(C::SlowFade) > 1.0, "a hump");
        assert!(power(C::SlowCut) > power(C::SlowFade), "a broader one");
        assert!(power(C::FastCut) > 1.99, "almost all the way");
    }

    #[test]
    fn a_curve_index_survives_the_file_and_a_bad_one_falls_back() {
        for curve in C::ALL {
            assert_eq!(C::from_index(curve.index()), curve);
        }
        assert_eq!(C::from_index(99), C::ConstantPower);
        assert_eq!(C::MENU.len(), C::ALL.len());
        assert_eq!(C::from_index(3), C::ConstantPower, "the file's default");
    }
}
