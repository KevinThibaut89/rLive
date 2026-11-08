//! Small DSP building blocks shared by the built-in devices.
#![allow(clippy::should_implement_trait)]

use std::f32::consts::{PI, TAU};

/// One-pole parameter smoother.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    pub value: f32,
    pub target: f32,
    coeff: f32,
}

impl Smoother {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            target: value,
            coeff: 1.0,
        }
    }

    pub fn set_time(&mut self, seconds: f32, sample_rate: f32) {
        self.coeff = 1.0 - (-1.0 / (seconds.max(1e-5) * sample_rate)).exp();
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        self.value += (self.target - self.value) * self.coeff;
        self.value
    }

    pub fn reset(&mut self) {
        self.value = self.target;
    }
}

/// Envelope stages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// ADSR envelope: linear attack, exponential decay and release.
#[derive(Clone, Copy, Debug)]
pub struct Adsr {
    pub stage: Stage,
    pub level: f32,
    attack_step: f32,
    decay_coeff: f32,
    release_coeff: f32,
    pub sustain: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            stage: Stage::Idle,
            level: 0.0,
            attack_step: 1.0,
            decay_coeff: 0.0,
            release_coeff: 0.0,
            sustain: 1.0,
        }
    }
}

fn exp_coeff(ms: f32, sr: f32) -> f32 {
    // Reach ~-60 dB in the given time.
    (-6.9 / (ms.max(0.5) * 0.001 * sr)).exp()
}

impl Adsr {
    pub fn set(&mut self, a_ms: f32, d_ms: f32, s: f32, r_ms: f32, sr: f32) {
        self.attack_step = 1.0 / (a_ms.max(0.5) * 0.001 * sr);
        self.decay_coeff = exp_coeff(d_ms, sr);
        self.release_coeff = exp_coeff(r_ms, sr);
        self.sustain = s.clamp(0.0, 1.0);
    }

    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay_coeff;
                if (self.level - self.sustain).abs() < 1e-4 {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = self.sustain,
            Stage::Release => {
                self.level *= self.release_coeff;
                if self.level < 1e-4 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Waveform {
    Saw,
    Square,
    Triangle,
    Sine,
}

impl Waveform {
    pub const LABELS: [&'static str; 4] = ["Saw", "Square", "Triangle", "Sine"];

    pub fn from_index(i: f64) -> Self {
        match i.round() as i32 {
            1 => Waveform::Square,
            2 => Waveform::Triangle,
            3 => Waveform::Sine,
            _ => Waveform::Saw,
        }
    }
}

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// Band-limited (PolyBLEP) oscillator.
#[derive(Clone, Copy, Debug, Default)]
pub struct Oscillator {
    pub phase: f32,
}

impl Oscillator {
    /// `dt` = frequency / sample rate.
    #[inline]
    pub fn next(&mut self, wave: Waveform, dt: f32) -> f32 {
        let dt = dt.clamp(1e-6, 0.49);
        let t = self.phase;
        let out = match wave {
            Waveform::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Waveform::Square => {
                let mut s = if t < 0.5 { 1.0 } else { -1.0 };
                s += poly_blep(t, dt);
                s -= poly_blep((t + 0.5).fract(), dt);
                s
            }
            Waveform::Triangle => 1.0 - 4.0 * (t - 0.5).abs(),
            Waveform::Sine => (t * TAU).sin(),
        };
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterMode {
    LowPass,
    HighPass,
    BandPass,
}

impl FilterMode {
    pub const LABELS: [&'static str; 3] = ["Low-pass", "High-pass", "Band-pass"];

    pub fn from_index(i: f64) -> Self {
        match i.round() as i32 {
            1 => FilterMode::HighPass,
            2 => FilterMode::BandPass,
            _ => FilterMode::LowPass,
        }
    }
}

/// Zero-delay-feedback state variable filter (Cytomic / Simper).
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

impl Svf {
    /// `resonance` in 0..1.
    pub fn set(&mut self, cutoff: f32, resonance: f32, sr: f32) {
        let fc = cutoff.clamp(10.0, sr * 0.49);
        let g = (PI * fc / sr).tan();
        self.k = 2.0 - 1.96 * resonance.clamp(0.0, 1.0);
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    #[inline]
    pub fn process(&mut self, v0: f32, mode: FilterMode) -> f32 {
        let v3 = v0 - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        match mode {
            FilterMode::LowPass => v2,
            FilterMode::BandPass => v1,
            FilterMode::HighPass => v0 - self.k * v1 - v2,
        }
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }
}

/// RBJ biquad (transposed direct form II).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum BiquadKind {
    LowShelf,
    Peak,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

impl Biquad {
    pub fn set(&mut self, kind: BiquadKind, freq: f32, q: f32, gain_db: f32, sr: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = TAU * freq.clamp(10.0, sr * 0.49) / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.05));
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::Peak => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            BiquadKind::LowShelf => {
                let sq = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + sq),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - sq),
                    (a + 1.0) + (a - 1.0) * cos + sq,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - sq,
                )
            }
            BiquadKind::HighShelf => {
                let sq = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + sq),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - sq),
                    (a + 1.0) - (a - 1.0) * cos + sq,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - sq,
                )
            }
            BiquadKind::LowPass => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            BiquadKind::HighPass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            BiquadKind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            BiquadKind::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Fixed-size circular delay line with fractional reads.
#[derive(Clone, Debug, Default)]
pub struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    pub fn new(max_samples: usize) -> Self {
        Self {
            buf: vec![0.0; max_samples.max(4)],
            pos: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[inline]
    pub fn write(&mut self, x: f32) {
        if self.buf.is_empty() {
            return;
        }
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % self.buf.len();
    }

    /// Read `delay` samples behind the last write (linear interpolation).
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let n = self.buf.len();
        if n == 0 {
            return 0.0;
        }
        let d = delay.clamp(1.0, (n - 2) as f32);
        let idx = d.floor();
        let frac = d - idx;
        let i0 = (self.pos + n - idx as usize) % n;
        let i1 = (i0 + n - 1) % n;
        self.buf[i0] * (1.0 - frac) + self.buf[i1] * frac
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// Fast-ish soft clipper.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    x.tanh()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adsr_goes_idle() {
        let mut env = Adsr::default();
        env.set(1.0, 10.0, 0.5, 10.0, 48_000.0);
        env.gate_on();
        for _ in 0..2000 {
            env.next();
        }
        assert!((env.level - 0.5).abs() < 0.01);
        env.gate_off();
        for _ in 0..5000 {
            env.next();
        }
        assert!(!env.is_active());
    }

    #[test]
    fn svf_lowpass_attenuates_highs() {
        let sr = 48_000.0;
        let mut f = Svf::default();
        f.set(200.0, 0.0, sr);
        let mut osc = Oscillator::default();
        let mut peak: f32 = 0.0;
        for i in 0..48_000 {
            let y = f.process(osc.next(Waveform::Sine, 5000.0 / sr), FilterMode::LowPass);
            if i > 1000 {
                peak = peak.max(y.abs());
            }
        }
        assert!(peak < 0.01, "{peak}");
    }

    #[test]
    fn delay_line_reads_back() {
        let mut d = DelayLine::new(100);
        for i in 0..50 {
            d.write(i as f32);
        }
        assert_eq!(d.read(1.0), 49.0);
        assert_eq!(d.read(10.0), 40.0);
    }
}
