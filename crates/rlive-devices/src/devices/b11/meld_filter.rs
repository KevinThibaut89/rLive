//! Meld's seventeen filter types (manual pp. 752-754).

use super::common::{nearest_in_scale, soft_clip};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::util::{lerp, sanitize};
use crate::dsp::{Biquad, BiquadKind};
use rlive_core::ScaleInfo;
use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

pub const FILTER_TYPES: [&str; 17] = [
    "SVF 12dB",
    "SVF 24dB",
    "MS2 LP",
    "MS2 HP",
    "OSR BP",
    "LP Crunch 12dB",
    "LP Switched Res",
    "Filther",
    "Eq Peak",
    "Eq Notch",
    "Phaser",
    "Redux",
    "Vowel",
    "Comb +",
    "Comb -",
    "Plate Resonator",
    "Membrane Resonator",
];

/// The labels of each filter's two macros.
pub const FILTER_MACROS: [(&str, &str); 17] = [
    ("Q", "L-B-H-N"),
    ("Q", "L-B-H-N"),
    ("Q", "Drive"),
    ("Q", "Drive"),
    ("Q", "Drive"),
    ("Q", "Drive"),
    ("Q", "Lofi"),
    ("Q", "Drive"),
    ("Boost", "Width"),
    ("Cut", "Width"),
    ("Feedback", "Spread"),
    ("Crush", "Lofi"),
    ("Morph", "Q"),
    ("Feedback", "Damp"),
    ("Feedback", "Damp"),
    ("Resonance", "Ratio"),
    ("Resonance", "Damping"),
];

pub const PLATE: usize = 15;
pub const MEMBRANE: usize = 16;
const MODES: usize = 32;
const VOWEL_FORMANTS: [[f32; 3]; 5] = [
    [730.0, 1_090.0, 2_440.0],
    [530.0, 1_840.0, 2_480.0],
    [270.0, 2_290.0, 3_010.0],
    [570.0, 840.0, 2_410.0],
    [300.0, 870.0, 2_240.0],
];

/// `J_m(x)`, by Bessel's integral.
fn bessel_j(m: u32, x: f32) -> f32 {
    const STEPS: usize = 200;
    let dt = PI / STEPS as f32;
    let mut sum = 0.0;
    for i in 0..=STEPS {
        let t = i as f32 * dt;
        let w = if i == 0 || i == STEPS { 0.5 } else { 1.0 };
        sum += w * (m as f32 * t - x * t.sin()).cos();
    }
    sum * dt / PI
}

/// The first 32 circular-membrane modes, as ratios to the fundamental. Worked out once.
pub fn membrane_ratios() -> &'static [f32; MODES] {
    static RATIOS: OnceLock<[f32; MODES]> = OnceLock::new();
    RATIOS.get_or_init(|| {
        let mut zeros = Vec::new();
        for m in 0..12u32 {
            let (mut x, mut prev) = (0.1f32, bessel_j(m, 0.1));
            while x < 40.0 {
                let next = x + 0.1;
                let v = bessel_j(m, next);
                if v.signum() != prev.signum() && x > m as f32 * 0.5 {
                    let (mut a, mut b, mut fa) = (x, next, prev);
                    for _ in 0..30 {
                        let c = 0.5 * (a + b);
                        let fc = bessel_j(m, c);
                        if fa.signum() == fc.signum() {
                            a = c;
                            fa = fc;
                        } else {
                            b = c;
                        }
                    }
                    zeros.push(0.5 * (a + b));
                }
                prev = v;
                x = next;
            }
        }
        zeros.sort_by(|a, b| a.total_cmp(b));
        let base = zeros[0];
        let mut out = [1.0; MODES];
        for (o, z) in out.iter_mut().zip(zeros.iter()) {
            *o = z / base;
        }
        out
    })
}

#[derive(Clone, Copy, Default)]
struct Mode {
    a1: f32,
    a2: f32,
    y1: f32,
    y2: f32,
    gain: f32,
}

impl Mode {
    fn set(&mut self, hz: f32, r: f32, gain: f32, sr: f32) {
        let w = TAU * hz.min(sr * 0.47) / sr;
        self.a1 = 2.0 * r * w.cos();
        self.a2 = -r * r;
        self.gain = gain * (1.0 - r);
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let y = x * self.gain + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = sanitize(y);
        self.y1
    }
}

/// One engine's filter in one voice.
#[derive(Clone)]
pub struct MeldFilter {
    kind: usize,
    drive: f32,
    filter: Filter,
    formants: [Filter; 3],
    eq: Biquad,
    allpass: [f32; 6],
    phaser_a: [f32; 6],
    feedback: f32,
    hold: f32,
    counter: f32,
    rate: f32,
    bits: f32,
    lofi: f32,
    lp: f32,
    comb: InterpDelay,
    comb_delay: f32,
    damp: f32,
    modes: [Mode; MODES],
    mode_count: usize,
    last: (f32, f32, f32, bool),
}

impl MeldFilter {
    /// `max_comb` is the longest comb delay needed, in samples.
    pub fn new(max_comb: usize) -> Self {
        Self {
            kind: 0,
            drive: 1.0,
            filter: Filter::new(),
            formants: [Filter::new(); 3],
            eq: Biquad::default(),
            allpass: [0.0; 6],
            phaser_a: [0.0; 6],
            feedback: 0.0,
            hold: 0.0,
            counter: 0.0,
            rate: 1.0,
            bits: 16.0,
            lofi: 0.0,
            lp: 0.0,
            comb: InterpDelay::new(max_comb.max(16)),
            comb_delay: 100.0,
            damp: 0.0,
            modes: [Mode::default(); MODES],
            mode_count: 0,
            last: (-1.0, -1.0, -1.0, false),
        }
    }

    pub fn prepare(&mut self, sr: f64) {
        self.filter.prepare(sr);
        self.formants.iter_mut().for_each(|f| f.prepare(sr));
    }

    pub fn reset(&mut self) {
        self.filter.reset();
        self.formants.iter_mut().for_each(Filter::reset);
        self.eq = Biquad::default();
        self.allpass = [0.0; 6];
        self.feedback = 0.0;
        self.hold = 0.0;
        self.lp = 0.0;
        self.comb.clear();
        for m in self.modes.iter_mut() {
            m.y1 = 0.0;
            m.y2 = 0.0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn set_common(
        &mut self,
        circuit: Circuit,
        kind: FilterKind,
        slope: FilterSlope,
        cutoff: f32,
        q: f32,
        drive_db: f32,
        morph: f32,
    ) {
        self.filter.set(&FilterSettings {
            circuit,
            kind,
            slope,
            cutoff,
            resonance: q.clamp(0.0, 1.0),
            drive: drive_db,
            morph,
        });
    }

    /// Update for this control tick. `m1` and `m2` are the macros, 0..1.
    pub fn control(
        &mut self,
        kind: usize,
        cutoff: f32,
        m1: f32,
        m2: f32,
        sr: f32,
        scale: Option<ScaleInfo>,
    ) {
        self.kind = kind;
        let cutoff = cutoff.clamp(10.0, sr * 0.48);
        let (m1, m2) = (m1.clamp(0.0, 1.0), m2.clamp(0.0, 1.0));
        match kind {
            0 => self.set_common(
                Circuit::Svf,
                FilterKind::Morph,
                FilterSlope::Db12,
                cutoff,
                m1,
                0.0,
                m2 * 0.75,
            ),
            1 => self.set_common(
                Circuit::Svf,
                FilterKind::Morph,
                FilterSlope::Db24,
                cutoff,
                m1,
                0.0,
                m2 * 0.75,
            ),
            2 => self.set_common(
                Circuit::Ms2,
                FilterKind::LowPass,
                FilterSlope::Db12,
                cutoff,
                m1,
                m2 * 30.0,
                0.0,
            ),
            3 => self.set_common(
                Circuit::Ms2,
                FilterKind::HighPass,
                FilterSlope::Db12,
                cutoff,
                m1,
                m2 * 30.0,
                0.0,
            ),
            4 => self.set_common(
                Circuit::Osr,
                FilterKind::BandPass,
                FilterSlope::Db12,
                cutoff,
                m1,
                m2 * 30.0,
                0.0,
            ),
            5 => self.set_common(
                Circuit::Dfm,
                FilterKind::LowPass,
                FilterSlope::Db12,
                cutoff,
                m1,
                m2 * 30.0,
                0.0,
            ),
            6 => {
                self.set_common(
                    Circuit::Svf,
                    FilterKind::LowPass,
                    FilterSlope::Db12,
                    cutoff,
                    m1,
                    0.0,
                    0.0,
                );
                self.rate = lerp(1.0, 32.0, m2 * m2);
            }
            7 => {
                self.set_common(
                    Circuit::Svf,
                    FilterKind::LowPass,
                    FilterSlope::Db12,
                    cutoff,
                    m1,
                    0.0,
                    0.0,
                );
                self.drive = lerp(1.0, 16.0, m2 * m2);
            }
            8 => self
                .eq
                .set(BiquadKind::Peak, cutoff, lerp(4.0, 0.3, m2), m1 * 24.0, sr),
            9 => self
                .eq
                .set(BiquadKind::Peak, cutoff, lerp(4.0, 0.3, m2), -m1 * 36.0, sr),
            10 => {
                let spread = lerp(1.0, 2.0, m2);
                for i in 0..6 {
                    let f = (cutoff * spread.powf(i as f32 - 2.5)).min(sr * 0.48);
                    self.phaser_a[i] = (PI * f / sr).tan();
                }
                self.damp = m1 * 0.95;
            }
            11 => {
                self.rate = (sr / cutoff.max(10.0)).max(1.0);
                self.bits = lerp(16.0, 2.0, m1);
                self.lofi = m2;
            }
            12 => {
                let v = m1 * (VOWEL_FORMANTS.len() - 1) as f32;
                let i = (v as usize).min(VOWEL_FORMANTS.len() - 2);
                let t = v - i as f32;
                let scale = (cutoff / 1_000.0).clamp(0.25, 4.0);
                for (f, filter) in self.formants.iter_mut().enumerate() {
                    let hz = lerp(VOWEL_FORMANTS[i][f], VOWEL_FORMANTS[i + 1][f], t) * scale;
                    filter.set(&FilterSettings {
                        kind: FilterKind::BandPass,
                        cutoff: hz,
                        resonance: lerp(0.5, 0.95, m2),
                        ..Default::default()
                    });
                }
            }
            13 | 14 => {
                self.comb_delay = (sr / cutoff.max(20.0)).min(self.comb.capacity() as f32);
                self.damp = m1 * 0.98;
                self.lofi = 1.0 - (-TAU * lerp(20_000.0, 500.0, m2) / sr).exp();
            }
            PLATE | MEMBRANE => self.set_modal(kind, cutoff, m1, m2, sr, scale),
            _ => {}
        }
    }

    fn set_modal(
        &mut self,
        kind: usize,
        cutoff: f32,
        res: f32,
        shape: f32,
        sr: f32,
        scale: Option<ScaleInfo>,
    ) {
        let key = (cutoff, res, shape, scale.is_some());
        if key == self.last && kind == PLATE {
            return;
        }
        self.last = key;
        let mut ratios = [1.0f32; MODES];
        if kind == MEMBRANE {
            ratios = *membrane_ratios();
        } else {
            let mut grid = [0.0f32; 49];
            let r = lerp(0.5, 2.0, shape);
            let mut n = 0;
            for a in 1..=7 {
                for b in 1..=7 {
                    let (a, b) = (a as f32, b as f32);
                    grid[n] = (a * a + b * b * r * r) / (1.0 + r * r);
                    n += 1;
                }
            }
            grid.sort_unstable_by(|a, b| a.total_cmp(b));
            ratios.copy_from_slice(&grid[..MODES]);
        }
        let radius = lerp(0.9, 0.9998, res);
        let norm = 1.0 / (MODES as f32).sqrt();
        let damping = if kind == MEMBRANE { shape } else { 0.0 };
        let mut count = 0;
        for (i, mode) in self.modes.iter_mut().enumerate() {
            let mut hz = cutoff * ratios[i];
            if let Some(s) = scale {
                let note = 69.0 + 12.0 * (hz.max(1.0) / 440.0).log2();
                hz = 440.0 * 2f32.powf((nearest_in_scale(note, s) - 69.0) / 12.0);
            }
            if hz < sr * 0.47 {
                let fall = 1.0 / (1.0 + i as f32 * damping * 0.3);
                mode.set(
                    hz,
                    1.0 - (1.0 - radius) * (1.0 + i as f32 * damping),
                    fall * norm * 6.0,
                    sr,
                );
                count = i + 1;
            }
        }
        self.mode_count = count;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        match self.kind {
            6 => {
                self.counter += 1.0;
                if self.counter >= self.rate {
                    self.counter -= self.rate;
                    self.hold = self.filter.process(x);
                }
                self.hold
            }
            7 => {
                let clipped = (x * self.drive).clamp(-1.0, 1.0);
                soft_clip(self.filter.process(clipped) * 1.5)
            }
            8 | 9 => self.eq.process(x),
            10 => self.phaser(x),
            11 => self.redux(x),
            12 => {
                let mut sum = 0.0;
                for (i, f) in self.formants.iter_mut().enumerate() {
                    sum += f.process(x) * [1.0, 0.7, 0.4][i];
                }
                sum
            }
            13 => {
                let delayed = self.comb.read(self.comb_delay, Interpolation::Linear);
                self.comb.write(x);
                self.lp += self.lofi * (x + self.damp * delayed - self.lp);
                sanitize(self.lp)
            }
            14 => {
                let delayed = self.comb.read(self.comb_delay, Interpolation::Linear);
                let y = x + self.damp * delayed;
                self.comb.write(y);
                self.lp += self.lofi * (y - self.lp);
                sanitize(self.lp)
            }
            PLATE | MEMBRANE => {
                let mut sum = 0.0;
                for m in self.modes[..self.mode_count].iter_mut() {
                    sum += m.tick(x);
                }
                sum
            }
            _ => self.filter.process(x),
        }
    }

    /// Six all-passes with the feedback solved at the sample, so there's no delay in the loop.
    #[inline]
    fn phaser(&mut self, x: f32) -> f32 {
        let (mut g_total, mut s_total) = (1.0, 0.0);
        for i in 0..6 {
            let g = self.phaser_a[i];
            let a = (g - 1.0) / (g + 1.0);
            // y = a * in + (1 - a^2)-scaled state.
            s_total = a * s_total + self.allpass[i];
            g_total *= a;
        }
        let fb = -self.damp;
        let input = (x - fb * s_total) / (1.0 + fb * g_total);
        let mut stage = input;
        for i in 0..6 {
            let g = self.phaser_a[i];
            let a = (g - 1.0) / (g + 1.0);
            let y = a * stage + self.allpass[i];
            self.allpass[i] = sanitize(stage - a * y);
            stage = y;
        }
        (x + stage) * 0.5
    }

    #[inline]
    fn redux(&mut self, x: f32) -> f32 {
        self.counter += 1.0;
        if self.counter >= self.rate {
            self.counter -= self.rate;
            self.hold = x;
        }
        let levels = 2f32.powf(self.bits);
        let crushed = (self.hold * levels).round() / levels;
        self.lp += 0.2 * (crushed - self.lp);
        lerp(self.lp, crushed, self.lofi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_membrane_modes_start_where_the_bessel_zeros_do() {
        let r = membrane_ratios();
        assert!((r[1] - 1.593).abs() < 0.01, "{}", r[1]);
        assert!((r[2] - 2.135).abs() < 0.01, "{}", r[2]);
        assert!((r[3] - 2.295).abs() < 0.01, "{}", r[3]);
        assert!(r.windows(2).all(|w| w[0] <= w[1]));
    }
}
