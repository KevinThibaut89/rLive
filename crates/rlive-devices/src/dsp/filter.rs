//! Filter library: seven circuits, each with low-pass, band-pass, high-pass, notch and a morph,
//! at 12 or 24 dB per octave, with drive.
//!
//! All of them are zero-delay-feedback filters (trapezoidal integration), so they keep their
//! tuning near Nyquist and stay stable under fast modulation. These are circuit-inspired models
//! of our own, not copies of anyone's filters.
//!
//! | Circuit | Structure | What limits the resonance |
//! |---|---|---|
//! | [`Svf`](Circuit::Svf) | State-variable (Clean) | Nothing needed: linear, stops short of self-oscillation |
//! | [`Ladder`](Circuit::Ladder) | Four-pole transistor ladder | Saturation in the stages and the feedback |
//! | [`Ms2`](Circuit::Ms2) | Two-pole, Sallen-Key response | Damping that rises with level (soft clipped resonance) |
//! | [`Dfm`](Circuit::Dfm) | Four-pole ladder, linear stages | A saturator in the feedback path only: gritty resonance |
//! | [`Prd`](Circuit::Prd) | Four-pole ladder | Mostly the stages, so resonance is loud |
//! | [`Osr`](Circuit::Osr) | State-variable | Damping that jumps past a level, like a hard-clipping diode |
//! | [`Smp`](Circuit::Smp) | Two two-poles in series | The first section as the MS2, the second not at all |
//!
//! The non-linear circuits are solved by linearising around the previous state and then solving
//! the feedback exactly, which is a fixed amount of work per sample.

use super::util::{db_to_gain, lerp, sanitize};
use std::f32::consts::PI;

/// Which circuit a [`Filter`] models. See the [module docs](self).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Circuit {
    #[default]
    Svf,
    Ladder,
    Ms2,
    Dfm,
    Prd,
    Osr,
    Smp,
}

impl Circuit {
    pub const LABELS: [&'static str; 7] = ["SVF", "Ladder", "MS2", "DFM", "PRD", "OSR", "SMP"];

    pub fn from_index(i: f64) -> Self {
        match i.round() as i32 {
            1 => Self::Ladder,
            2 => Self::Ms2,
            3 => Self::Dfm,
            4 => Self::Prd,
            5 => Self::Osr,
            6 => Self::Smp,
            _ => Self::Svf,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FilterKind {
    #[default]
    LowPass,
    BandPass,
    HighPass,
    Notch,
    /// Sweeps low-pass, band-pass, high-pass, notch and back to low-pass over `morph` 0..1.
    Morph,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FilterSlope {
    #[default]
    Db12,
    Db24,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FilterSettings {
    pub circuit: Circuit,
    pub kind: FilterKind,
    pub slope: FilterSlope,
    pub cutoff: f32,
    /// 0..1. The non-linear circuits self-oscillate near 1.
    pub resonance: f32,
    /// Decibels of input gain, partly made up at the output. 0 is the circuit's normal level.
    pub drive: f32,
    /// 0..1, used by [`FilterKind::Morph`].
    pub morph: f32,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            circuit: Circuit::Svf,
            kind: FilterKind::LowPass,
            slope: FilterSlope::Db12,
            cutoff: 1_000.0,
            resonance: 0.0,
            drive: 0.0,
            morph: 0.0,
        }
    }
}

/// `tanh(x) / x`, for linearising a saturator around `x`.
#[inline]
fn tanh_ratio(x: f32) -> f32 {
    let x2 = x * x;
    ((x2 + 105.0) * x2 + 945.0) / ((15.0 * x2 + 420.0) * x2 + 945.0)
}

#[inline]
fn soft_saturate(x: f32) -> f32 {
    x * tanh_ratio(x)
}

// Damping of the two sections that make up a 24 dB/oct Butterworth response.
const BUTTERWORTH_LOW_Q: f32 = 1.847_759;
const BUTTERWORTH_HIGH_Q: f32 = 0.765_367;

// Where an integrator's state stops, far past where any sensible signal takes it.
const STATE_LIMIT: f32 = 16.0;

// Where the output is capped, for the same reason.
const OUTPUT_LIMIT: f32 = 32.0;

// How far the internal level sits below full scale in the saturating circuits.
const NONLINEAR_HEADROOM: f32 = 0.5;

// Where the OSR circuit's clipping diode starts conducting, and how quickly it finishes.
const OSR_KNEE: f32 = 0.7;
const OSR_KNEE_WIDTH: f32 = 0.15;

// -------------------------------------------------------------------- two-pole section

#[derive(Clone, Copy, Debug, Default)]
struct TwoPole {
    ic1: f32,
    ic2: f32,
}

#[inline]
fn two_pole_gains(g: f32, k: f32) -> (f32, f32, f32) {
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    (a1, a2, g * a2)
}

impl TwoPole {
    /// Runs one sample and returns the low-pass, band-pass (unscaled), and high-pass outputs.
    #[inline]
    fn tick(&mut self, x: f32, g: f32, k: f32) -> [f32; 3] {
        let (a1, a2, a3) = two_pole_gains(g, k);
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = clamp_state(2.0 * v1 - self.ic1);
        self.ic2 = clamp_state(2.0 * v2 - self.ic2);
        [v2, v1, x - k * v1 - v2]
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[inline]
fn clamp_state(x: f32) -> f32 {
    sanitize(x).clamp(-STATE_LIMIT, STATE_LIMIT)
}

/// Blends low-pass, unity-peak band-pass, high-pass and notch outputs.
#[inline]
fn mix_outputs(weights: [f32; 4], [lp, bp, hp, notch]: [f32; 4]) -> f32 {
    weights[0] * lp + weights[1] * bp + weights[2] * hp + weights[3] * notch
}

fn morph_weights(morph: f32) -> [f32; 4] {
    let m = morph.rem_euclid(1.0) * 4.0;
    let segment = m.floor() as usize;
    let t = m - m.floor();
    let mut w = [0.0; 4];
    w[segment % 4] = 1.0 - t;
    w[(segment + 1) % 4] = t;
    w
}

// ------------------------------------------------------------------------ ladder core

#[derive(Clone, Copy, Debug, Default)]
struct LadderCore {
    s: [f32; 4],
}

/// How the stages and feedback of a ladder are saturated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LadderSat {
    /// Input and feedback saturation in the first stage, plus the other stages (Ladder).
    StagesAndFeedback,
    /// The stages, with only a light touch on the feedback: loud resonance that stays in tune.
    StagesOnly,
    /// The feedback alone (Dfm).
    FeedbackOnly,
}

impl LadderCore {
    /// One sample of the ladder at cutoff gain `f`, feedback `k`. Returns the input to the
    /// first stage and the four stage outputs.
    #[inline]
    fn tick(&mut self, x: f32, f: f32, k: f32, sat: LadderSat) -> [f32; 5] {
        let s = self.s;
        let stage_sat = sat != LadderSat::FeedbackOnly;
        let t1 = if stage_sat { tanh_ratio(s[0]) } else { 1.0 };
        let t2 = if stage_sat { tanh_ratio(s[1]) } else { 1.0 };
        let t3 = if stage_sat { tanh_ratio(s[2]) } else { 1.0 };
        let t4 = if stage_sat { tanh_ratio(s[3]) } else { 1.0 };
        // How much of the first stage's input gets through, and of the feedback.
        let (t0, k) = match sat {
            LadderSat::StagesAndFeedback => (tanh_ratio(x - k * s[3]), k),
            LadderSat::StagesOnly => (tanh_ratio(0.5 * (x - k * s[3])), k),
            LadderSat::FeedbackOnly => (1.0, k * tanh_ratio(s[3])),
        };

        let g1 = 1.0 / (1.0 + f * t1);
        let g2 = 1.0 / (1.0 + f * t2);
        let g3 = 1.0 / (1.0 + f * t3);
        let g4 = 1.0 / (1.0 + f * t4);
        let c1 = f * t0 * g1;
        let c2 = f * t1 * g2;
        let c3 = f * t2 * g3;
        let c4 = f * t3 * g4;

        // y4 = sum + gain * u, with u = x - k * y4.
        let gain = c4 * c3 * c2 * c1;
        let sum = g4 * s[3] + c4 * (g3 * s[2] + c3 * (g2 * s[1] + c2 * g1 * s[0]));
        let y4 = (sum + gain * x) / (1.0 + k * gain);
        let u = x - k * y4;
        let y1 = g1 * s[0] + c1 * u;
        let y2 = g2 * s[1] + c2 * y1;
        let y3 = g3 * s[2] + c3 * y2;

        self.s[0] = clamp_state(2.0 * y1 - s[0]);
        self.s[1] = clamp_state(2.0 * y2 - s[1]);
        self.s[2] = clamp_state(2.0 * y3 - s[2]);
        self.s[3] = clamp_state(2.0 * y4 - s[3]);
        [t0 * u, y1, y2, y3, y4]
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The ladder's outputs combined per slope as low-pass, band-pass, high-pass and notch.
#[inline]
fn ladder_outputs([x, y1, y2, y3, y4]: [f32; 5], slope: FilterSlope) -> [f32; 4] {
    match slope {
        FilterSlope::Db12 => {
            let bp = 2.0 * (y1 - y2);
            [y2, bp, x - 2.0 * y1 + y2, x - bp]
        }
        FilterSlope::Db24 => [
            y4,
            4.0 * (y2 - 2.0 * y3 + y4),
            x - 4.0 * y1 + 6.0 * y2 - 4.0 * y3 + y4,
            x - 4.0 * y1 + 8.0 * y2 - 8.0 * y3 + 4.0 * y4,
        ],
    }
}

// ---------------------------------------------------------------------- per-circuit maps

#[derive(Clone, Copy, Debug)]
struct Tuning {
    /// Prewarped cutoff.
    g: f32,
    /// Two-pole damping, or ladder feedback.
    k: f32,
    /// Damping the MS2's resonance falls back to at high levels.
    k_open: f32,
    /// Two-pole damping of the first section at 24 dB.
    k_low: f32,
    /// Input gain.
    gain_in: f32,
    /// Output gain.
    gain_out: f32,
    /// Passband compensation of a ladder.
    comp: f32,
}

fn tuning(settings: &FilterSettings, sample_rate: f32) -> Tuning {
    let fc = settings.cutoff.clamp(5.0, sample_rate * 0.49);
    let g = (PI * fc / sample_rate).tan();
    let res = settings.resonance.clamp(0.0, 1.0);
    let drive = db_to_gain(settings.drive.clamp(-24.0, 60.0));
    let (k, k_open, comp) = match (settings.circuit, settings.slope) {
        (Circuit::Svf, FilterSlope::Db12) => (2.0 - 1.96 * res, 0.0, 0.0),
        (Circuit::Svf, FilterSlope::Db24) => (lerp(BUTTERWORTH_HIGH_Q, 0.02, res), 0.0, 0.0),
        (Circuit::Ms2, FilterSlope::Db12) => (2.0 - 2.2 * res, 2.0, 0.0),
        (Circuit::Ms2, FilterSlope::Db24) => (
            BUTTERWORTH_HIGH_Q - (BUTTERWORTH_HIGH_Q + 0.2) * res,
            BUTTERWORTH_HIGH_Q,
            0.0,
        ),
        (Circuit::Ladder, _) => (4.2 * res, 0.0, 0.5),
        (Circuit::Dfm, _) => (4.4 * res, 0.0, 0.25),
        (Circuit::Prd, _) => (4.1 * res, 0.0, 0.0),
        (Circuit::Osr, FilterSlope::Db12) => (2.0 - 2.05 * res, 2.0, 0.0),
        (Circuit::Osr, FilterSlope::Db24) => (
            BUTTERWORTH_HIGH_Q - (BUTTERWORTH_HIGH_Q + 0.08) * res,
            BUTTERWORTH_HIGH_Q,
            0.0,
        ),
        (Circuit::Smp, FilterSlope::Db12) => (2.0 - 2.15 * res, 2.0, 0.0),
        (Circuit::Smp, FilterSlope::Db24) => (
            BUTTERWORTH_HIGH_Q - (BUTTERWORTH_HIGH_Q + 0.12) * res,
            BUTTERWORTH_HIGH_Q,
            0.0,
        ),
    };
    // Drive saturates harder going in. Half of it, in decibels, is taken back at the output.
    let (gain_in, gain_out) = match settings.circuit {
        Circuit::Svf => {
            let drive = drive.max(1.0);
            (drive, 1.0 / drive.sqrt())
        }
        _ => (
            NONLINEAR_HEADROOM * drive,
            1.0 / (NONLINEAR_HEADROOM * drive.sqrt()),
        ),
    };
    Tuning {
        g,
        k,
        k_open,
        k_low: BUTTERWORTH_LOW_Q,
        gain_in,
        gain_out,
        comp,
    }
}

fn kind_weights(kind: FilterKind, morph: f32) -> [f32; 4] {
    match kind {
        FilterKind::LowPass => [1.0, 0.0, 0.0, 0.0],
        FilterKind::BandPass => [0.0, 1.0, 0.0, 0.0],
        FilterKind::HighPass => [0.0, 0.0, 1.0, 0.0],
        FilterKind::Notch => [0.0, 0.0, 0.0, 1.0],
        FilterKind::Morph => morph_weights(morph),
    }
}

// ----------------------------------------------------------------------------- filter

/// One channel of a filter. `set` can be called every sample, but every few samples is plenty.
#[derive(Clone, Copy, Debug)]
pub struct Filter {
    sample_rate: f32,
    settings: FilterSettings,
    tuning: Tuning,
    weights: [f32; 4],
    drive_active: bool,
    first: TwoPole,
    second: TwoPole,
    ladder: LadderCore,
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter {
    pub fn new() -> Self {
        let settings = FilterSettings::default();
        let sample_rate = 48_000.0;
        Self {
            sample_rate,
            settings,
            tuning: tuning(&settings, sample_rate),
            weights: kind_weights(settings.kind, settings.morph),
            drive_active: false,
            first: TwoPole::default(),
            second: TwoPole::default(),
            ladder: LadderCore::default(),
        }
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(1.0) as f32;
        let settings = self.settings;
        self.set(&settings);
        self.reset();
    }

    pub fn set(&mut self, settings: &FilterSettings) {
        self.settings = *settings;
        self.tuning = tuning(settings, self.sample_rate);
        self.weights = kind_weights(settings.kind, settings.morph);
        self.drive_active = settings.drive > 0.0;
    }

    pub fn settings(&self) -> &FilterSettings {
        &self.settings
    }

    pub fn reset(&mut self) {
        self.first.reset();
        self.second.reset();
        self.ladder.reset();
    }

    /// Filters one sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = sanitize(x).clamp(-OUTPUT_LIMIT, OUTPUT_LIMIT);
        let Tuning {
            g,
            k,
            k_open,
            k_low,
            gain_in,
            gain_out,
            comp,
        } = self.tuning;
        let weights = self.weights;
        let slope = self.settings.slope;
        let output = match self.settings.circuit {
            Circuit::Svf => {
                let x = if self.drive_active {
                    soft_saturate(x * gain_in)
                } else {
                    x
                };
                match slope {
                    FilterSlope::Db12 => {
                        let [lp, bp, hp] = self.first.tick(x, g, k);
                        mix_outputs(weights, [lp, bp * k, hp, lp + hp])
                    }
                    FilterSlope::Db24 => {
                        let [lp, bp, hp] = self.first.tick(x, g, k_low);
                        let y = mix_outputs(weights, [lp, bp * k_low, hp, lp + hp]);
                        let [lp, bp, hp] = self.second.tick(y, g, k);
                        mix_outputs(weights, [lp, bp * k, hp, lp + hp])
                    }
                }
            }
            Circuit::Ms2 => {
                let x = Self::driven(x, gain_in, self.drive_active);
                // Resonance falls away as the state grows, which is what clips it softly.
                let open = k_open - (k_open - k) * tanh_ratio(self.second_state_or_first());
                match slope {
                    FilterSlope::Db12 => {
                        let [lp, bp, hp] = self.first.tick(x, g, open);
                        mix_outputs(weights, [lp, bp * k_open, hp, lp + hp])
                    }
                    FilterSlope::Db24 => {
                        let [lp, bp, hp] = self.first.tick(x, g, k_low);
                        let y = mix_outputs(weights, [lp, bp * k_low, hp, lp + hp]);
                        let [lp, bp, hp] = self.second.tick(y, g, open);
                        mix_outputs(weights, [lp, bp * k_open, hp, lp + hp])
                    }
                }
            }
            Circuit::Osr => {
                let x = Self::driven(x, gain_in, self.drive_active);
                // Below the knee resonance is untouched; past it the diode cuts it off quickly.
                let level = self.second_state_or_first().abs();
                let open = lerp(
                    k,
                    k_open,
                    ((level - OSR_KNEE) / OSR_KNEE_WIDTH).clamp(0.0, 1.0),
                );
                match slope {
                    FilterSlope::Db12 => {
                        let [lp, bp, hp] = self.first.tick(x, g, open);
                        mix_outputs(weights, [lp, bp * k_open, hp, lp + hp])
                    }
                    FilterSlope::Db24 => {
                        let [lp, bp, hp] = self.first.tick(x, g, k_low);
                        let y = mix_outputs(weights, [lp, bp * k_low, hp, lp + hp]);
                        let [lp, bp, hp] = self.second.tick(y, g, open);
                        mix_outputs(weights, [lp, bp * k_open, hp, lp + hp])
                    }
                }
            }
            Circuit::Smp => {
                let x = Self::driven(x, gain_in, self.drive_active);
                match slope {
                    FilterSlope::Db12 => {
                        let open = k_open - (k_open - k) * tanh_ratio(self.first.ic1);
                        let [lp, bp, hp] = self.first.tick(x, g, open);
                        mix_outputs(weights, [lp, bp * k_open, hp, lp + hp])
                    }
                    FilterSlope::Db24 => {
                        // Soft-limited in the first section, then a free-running second one, which
                        // is what gives it the character of both the MS2 and the PRD.
                        let open = k_open - (k_open - k) * tanh_ratio(self.first.ic1);
                        let [lp, bp, hp] = self.first.tick(x, g, k_low.min(open + k_low * 0.5));
                        let y = mix_outputs(weights, [lp, bp * k_low, hp, lp + hp]);
                        let [lp, bp, hp] = self.second.tick(y, g, k);
                        mix_outputs(weights, [lp, bp * k_open, hp, lp + hp])
                    }
                }
            }
            Circuit::Ladder | Circuit::Dfm | Circuit::Prd => {
                let sat = match self.settings.circuit {
                    Circuit::Ladder => LadderSat::StagesAndFeedback,
                    Circuit::Dfm => LadderSat::FeedbackOnly,
                    _ => LadderSat::StagesOnly,
                };
                // The stages of the other ladders saturate; the DFM's are linear.
                let driven = self.drive_active && sat == LadderSat::FeedbackOnly;
                let input = Self::driven(x, gain_in, driven) * (1.0 + comp * k);
                let stages = self.ladder.tick(input, g, k, sat);
                mix_outputs(weights, ladder_outputs(stages, slope))
            }
        };
        sanitize(output * gain_out).clamp(-OUTPUT_LIMIT, OUTPUT_LIMIT)
    }

    /// Input gain, saturating when the drive is up.
    #[inline]
    fn driven(x: f32, gain: f32, saturate: bool) -> f32 {
        if saturate {
            soft_saturate(x * gain)
        } else {
            x * gain
        }
    }

    #[inline]
    fn second_state_or_first(&self) -> f32 {
        match self.settings.slope {
            FilterSlope::Db12 => self.first.ic1,
            FilterSlope::Db24 => self.second.ic1,
        }
    }

    /// Filters a block in place.
    pub fn process_block(&mut self, block: &mut [f32]) {
        for x in block {
            *x = self.process(*x);
        }
    }
}

// ----------------------------------------------------------------- analytic response

#[derive(Clone, Copy)]
struct C(f32, f32);

impl C {
    fn abs(self) -> f32 {
        self.0.hypot(self.1)
    }
    fn add(self, o: C) -> C {
        C(self.0 + o.0, self.1 + o.1)
    }
    fn mul(self, o: C) -> C {
        C(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
    }
    fn div(self, o: C) -> C {
        let d = o.0 * o.0 + o.1 * o.1;
        C(
            (self.0 * o.0 + self.1 * o.1) / d,
            (self.1 * o.0 - self.0 * o.1) / d,
        )
    }
    fn scale(self, a: f32) -> C {
        C(self.0 * a, self.1 * a)
    }
}

const ONE: C = C(1.0, 0.0);

impl FilterSettings {
    /// The small-signal magnitude at `hz`, as a linear gain. For drawing a filter's curve, and
    /// for checking the filter against it.
    pub fn magnitude(&self, hz: f32, sample_rate: f32) -> f32 {
        let t = tuning(self, sample_rate);
        // The bilinear frequency warp: this is what the digital filter does at `hz`.
        let w = (PI * hz.clamp(0.0, sample_rate * 0.499) / sample_rate).tan() / t.g;
        let s = C(0.0, w);
        let weights = kind_weights(self.kind, self.morph);
        let two_pole = |k: f32, peak: f32| {
            let d = s.mul(s).add(s.scale(k)).add(ONE);
            let lp = ONE.div(d);
            let hp = s.mul(s).div(d);
            let bp = s.scale(peak).div(d);
            let notch = s.mul(s).add(ONE).div(d);
            lp.scale(weights[0])
                .add(bp.scale(weights[1]))
                .add(hp.scale(weights[2]))
                .add(notch.scale(weights[3]))
        };
        let h = match self.circuit {
            Circuit::Svf => match self.slope {
                FilterSlope::Db12 => two_pole(t.k, t.k),
                FilterSlope::Db24 => two_pole(t.k_low, t.k_low).mul(two_pole(t.k, t.k)),
            },
            Circuit::Ms2 | Circuit::Osr | Circuit::Smp => match self.slope {
                FilterSlope::Db12 => two_pole(t.k, t.k_open),
                FilterSlope::Db24 => two_pole(t.k_low, t.k_low).mul(two_pole(t.k, t.k_open)),
            },
            Circuit::Ladder | Circuit::Dfm | Circuit::Prd => {
                let p = ONE.add(s);
                let p2 = p.mul(p);
                let p4 = p2.mul(p2);
                let denom = p4.add(C(t.k, 0.0));
                let (lp, bp, hp, notch) = match self.slope {
                    FilterSlope::Db12 => (
                        p2,
                        s.scale(2.0).mul(p2),
                        s.mul(s).mul(p2),
                        s.mul(s).add(ONE).mul(p2),
                    ),
                    FilterSlope::Db24 => {
                        let s2 = s.mul(s);
                        let n = s2.add(ONE);
                        (ONE, s2.scale(4.0), s2.mul(s2), n.mul(n))
                    }
                };
                lp.scale(weights[0])
                    .add(bp.scale(weights[1]))
                    .add(hp.scale(weights[2]))
                    .add(notch.scale(weights[3]))
                    .div(denom)
                    .scale(1.0 + t.comp * t.k)
            }
        };
        h.abs() * (t.gain_in * t.gain_out)
    }
}
