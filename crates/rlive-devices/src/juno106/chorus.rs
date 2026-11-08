//! Jack board chorus: two MN3009 256-stage bucket brigades run from one triangle LFO.
//!
//! The BBD is simulated at its real, swept clock rather than as a delay line with interpolation, so
//! clock-rate imaging, aliasing at the slow end of the sweep and the two-phase output tap
//! summing all arise from the model. Pre and post filters use the schematic's R and C values.

use super::analog::{Biquad, OnePole, Rng};

const STAGES_PER_CLOCK: usize = 128; // 256 stages hold one sample per two stages.

// Jack board schematic: Sallen-Key sections either side of the BBD.
const R_SK: f64 = 22e3;
const SK1: (f64, f64) = (820e-12, 680e-12);
const SK2: (f64, f64) = (1800e-12, 270e-12);

/// BBD clock excursion, derived from the commonly quoted 1.66 ms to 5.35 ms sweep. The service
/// notes give the clock circuit but no figure for it.
const CLOCK_MIN_HZ: f64 = STAGES_PER_CLOCK as f64 / 5.35e-3;
const CLOCK_MAX_HZ: f64 = STAGES_PER_CLOCK as f64 / 1.66e-3;

/// LFO rates. The RC network calculates to 0.55 Hz (I) and 0.90 Hz (II); these are the commonly
/// quoted measured values, which sit within component tolerance of that.
pub const RATE_I: f64 = 0.513;
pub const RATE_II: f64 = 0.863;

/// BBD clip point in volts: the service notes bias it by driving it into visible clipping.
const CLIP_V: f32 = 1.25;

/// Wet and dry summing gains into IC6 (100 k feedback; 39 k wet, 47 k dry).
pub const WET_GAIN: f32 = 100.0 / 39.0;
pub const DRY_GAIN: f32 = 100.0 / 47.0;

/// Per-unit tolerances applied to one chorus channel.
#[derive(Clone, Copy, Default)]
pub struct ChannelTrim {
    pub clock: f64,
    pub filter: f64,
    /// BIAS trimmer error in volts: makes clipping asymmetric.
    pub bias: f32,
}

#[derive(Clone)]
struct Bbd {
    ring: [f32; STAGES_PER_CLOCK],
    write: usize,
    phase: f64,
    odd: bool,
    tap_a: f32,
    tap_b: f32,
    hold: f32,
    x_prev: f32,
    centre: f64,
    depth: f64,
    bias: f32,
    pre_rc: OnePole,
    post_rc: OnePole,
    post: [Biquad; 2],
    ac: OnePole,
}

impl Bbd {
    fn new(trim: ChannelTrim, sr: f64) -> Self {
        let centre = 0.5 * (CLOCK_MIN_HZ + CLOCK_MAX_HZ) * (1.0 + trim.clock);
        let depth = 0.5 * (CLOCK_MAX_HZ - CLOCK_MIN_HZ) / (0.5 * (CLOCK_MIN_HZ + CLOCK_MAX_HZ));
        let tol = 1.0 + trim.filter;
        Self {
            ring: [0.0; STAGES_PER_CLOCK],
            write: 0,
            phase: 0.0,
            odd: false,
            tap_a: 0.0,
            tap_b: 0.0,
            hold: 0.0,
            x_prev: 0.0,
            centre,
            depth,
            bias: trim.bias,
            // R122 10 k into C52 2.2 nF.
            pre_rc: OnePole::new(tol / (std::f64::consts::TAU * 10e3 * 2.2e-9), sr),
            // Paralleled 3.3 k tap resistors into C45 2.2 nF.
            post_rc: OnePole::new(tol / (std::f64::consts::TAU * 1.65e3 * 2.2e-9), sr),
            post: [
                Biquad::sallen_key(R_SK, SK1.0, SK1.1, tol, sr),
                Biquad::sallen_key(R_SK, SK2.0, SK2.1, tol, sr),
            ],
            // C28 1 µF into R103 22 k.
            ac: OnePole::new(7.2, sr),
        }
    }

    #[inline]
    fn saturate(&self, x: f32) -> f32 {
        // Flat-topped like the scope trace in the BIAS adjustment; a sixth-order knee.
        let v = x + self.bias;
        let r = v / CLIP_V;
        let r2 = r * r;
        let out = v / (1.0 + r2 * r2 * r2).powf(1.0 / 6.0);
        // MOS bucket stages add a little second harmonic.
        out - 0.02 * out * out - self.bias
    }

    #[inline]
    fn process(&mut self, x: f32, lfo: f64, dt: f64, hiss: f32) -> f32 {
        let filtered = self.pre_rc.lowpass(x);
        let clock = self.centre * (1.0 + self.depth * lfo);
        let half_ticks = 2.0 * clock * dt;
        self.phase += half_ticks;

        let mut acc = 0.0f32;
        let mut covered = 0.0f64;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            let t = 1.0 - self.phase / half_ticks;
            let at = (t.clamp(0.0, 1.0)) as f32;
            acc += self.hold * (t - covered) as f32;
            covered = t;
            self.odd = !self.odd;
            if self.odd {
                let sample = self.x_prev + (filtered - self.x_prev) * at;
                self.ring[self.write] = self.saturate(sample);
                self.write = (self.write + 1) % STAGES_PER_CLOCK;
                self.tap_a = self.ring[self.write];
            } else {
                self.tap_b = self.ring[(self.write + 1) % STAGES_PER_CLOCK];
            }
            // The two output taps are summed through R118/R119, hiding the clock.
            self.hold = 0.5 * (self.tap_a + self.tap_b);
        }
        acc += self.hold * (1.0 - covered) as f32;
        self.x_prev = filtered;

        let y = self.post_rc.lowpass(acc + hiss);
        let y = self.post[0].process(y);
        let y = self.post[1].process(y);
        self.ac.highpass(y)
    }
}

pub struct Chorus {
    pre: [Biquad; 2],
    left: Bbd,
    right: Bbd,
    /// Triangle phase in `0..1`.
    phase: f64,
    rng: Rng,
    hiss_scale: f32,
    /// Current wet FET conduction, 0 to 1. Ramps because the gate is driven through an RC.
    wet: OnePole,
    dt: f64,
}

impl Chorus {
    pub fn new(trim: [ChannelTrim; 2], seed: u32, sr: f64) -> Self {
        let tol = 1.0 + 0.5 * (trim[0].filter + trim[1].filter);
        Self {
            pre: [
                Biquad::sallen_key(R_SK, SK1.0, SK1.1, tol, sr),
                Biquad::sallen_key(R_SK, SK2.0, SK2.1, tol, sr),
            ],
            left: Bbd::new(trim[0], sr),
            right: Bbd::new(trim[1], sr),
            phase: 0.25,
            rng: Rng::new(seed ^ 0xB0CC),
            hiss_scale: 4.0e-3,
            // About 0.25 s: the gate of Tr11/Tr12 is driven through a large RC.
            wet: OnePole::new(0.64, sr),
            dt: 1.0 / sr,
        }
    }

    /// `enabled` opens the wet FETs, `rate` is the LFO in Hz, `hiss` scales the BBD noise.
    /// Returns the two wet signals in volts, before the summing resistors.
    #[inline]
    pub fn process(&mut self, x: f32, enabled: bool, rate: f64, hiss: f32) -> (f32, f32) {
        let first = self.pre[0].process(x);
        let pre = self.pre[1].process(first);

        self.phase += rate * self.dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        let tri = 1.0 - 4.0 * (self.phase - 0.5).abs();

        let amount = self.wet.lowpass(if enabled { 1.0 } else { 0.0 });
        let h = hiss * self.hiss_scale;
        let l_noise = self.rng.uniform() * h;
        let r_noise = self.rng.uniform() * h;
        let l = self.left.process(pre, tri, self.dt, l_noise);
        let r = self.right.process(pre, -tri, self.dt, r_noise);
        (l * amount, r * amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delay of the left channel measured from zero crossings of a 100 Hz tone.
    fn delays(rate: f64) -> Vec<(f64, f64)> {
        let sr = 192_000.0;
        let mut c = Chorus::new([ChannelTrim::default(); 2], 1, sr);
        let hz = 100.0;
        let mut last = 0.0f32;
        let mut out = Vec::new();
        for i in 0..(sr * 8.0) as usize {
            let t = i as f64 / sr;
            let (l, _) = c.process(
                (std::f64::consts::TAU * hz * t).sin() as f32 * 0.5,
                true,
                rate,
                0.0,
            );
            if last <= 0.0 && l > 0.0 && t > 2.0 {
                let crossing = t - (l / (l - last)) as f64 / sr;
                out.push((t, (crossing * hz).fract() / hz));
            }
            last = l;
        }
        out
    }

    #[test]
    fn delay_sweeps_the_expected_range() {
        let d = delays(RATE_I);
        let min = d.iter().map(|x| x.1).fold(f64::MAX, f64::min);
        let max = d.iter().map(|x| x.1).fold(f64::MIN, f64::max);
        // Includes the fixed group delay of the surrounding filters (about 0.1 ms).
        assert!((min - 1.76e-3).abs() < 0.3e-3, "{min}");
        assert!((max - 5.45e-3).abs() < 0.4e-3, "{max}");
    }

    #[test]
    fn mode_two_sweeps_faster_than_mode_one() {
        let count = |rate: f64| {
            let d = delays(rate);
            let mean = d.iter().map(|x| x.1).sum::<f64>() / d.len() as f64;
            d.windows(2)
                .filter(|w| w[0].1 < mean && w[1].1 >= mean)
                .count()
        };
        assert!(count(RATE_II) > count(RATE_I));
    }

    #[test]
    fn overdriving_the_bbd_clips_it() {
        let sr = 192_000.0;
        let mut c = Chorus::new([ChannelTrim::default(); 2], 1, sr);
        let mut peak = 0.0f32;
        for i in 0..(sr * 1.0) as usize {
            let x = (std::f64::consts::TAU * 1000.0 * i as f64 / sr).sin() as f32 * 5.0;
            let (l, _) = c.process(x, true, RATE_I, 0.0);
            if i > 96_000 {
                peak = peak.max(l.abs());
            }
        }
        assert!(peak < 2.0, "{peak}");
    }
}
