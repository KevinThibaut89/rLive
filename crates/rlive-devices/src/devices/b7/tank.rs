//! A feedback delay network, shared by Reverb's full engine and Hybrid Reverb's algorithms.
//!
//! Each line is a delay, an allpass, a two-band absorber that sets the decay time, an optional
//! damping low-pass and optional shelves. The lines feed each other through a Householder matrix,
//! which is orthogonal, so with every loop gain at 1 the network loses nothing. Gains are worked
//! out from the actual loop length, so the tail falls 60 dB in the requested time.

use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::util::sanitize;
use crate::dsp::{Biquad, BiquadKind};
use std::f32::consts::TAU;

/// The most delay lines a tank can have.
pub const MAX_LINES: usize = 12;
/// The longest line delay at a scale of 1, in ms.
pub const LONGEST_LINE_MS: f32 = 79.0;

const LINE_MS: [f32; MAX_LINES] = [
    29.7, 37.1, 41.1, 43.7, 47.3, 53.0, 59.0, 61.0, 67.0, 71.0, 73.0, 79.0,
];
const ALLPASS_MS: [f32; MAX_LINES] = [4.3, 5.9, 3.1, 7.3, 2.3, 8.9, 6.1, 3.7, 9.7, 5.3, 2.9, 7.9];
/// Beyond this level the feedback path saturates, which keeps a frozen tank fed with sustained
/// input bounded.
const SOFT_LIMIT: f32 = 2.0;
const CONTROL_FRAMES: usize = 16;
const MIN_SCALE_FOR_ONE_SAMPLE: f32 = 1.0;
const MOD_MARGIN_MS: f32 = 6.0;

/// A smooth ceiling: unchanged up to [`SOFT_LIMIT`], and never above twice that.
#[inline]
pub fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= SOFT_LIMIT {
        return x;
    }
    let over = a - SOFT_LIMIT;
    (SOFT_LIMIT + over / (1.0 + over / SOFT_LIMIT)).copysign(x)
}

/// A parabolic stand-in for sine, for modulation. `phase` is in `0.0..1.0`.
#[inline]
fn soft_sine(phase: f32) -> f32 {
    let p = phase - 0.5;
    8.0 * p * (1.0 - 2.0 * p.abs())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TankSettings {
    /// How many lines run, from 4 to [`MAX_LINES`].
    pub lines: usize,
    /// Multiplies every delay time.
    pub scale: f32,
    /// Multiplies every allpass time.
    pub allpass_scale: f32,
    /// Allpass coefficient, 0 for a pure delay.
    pub diffusion: f32,
    /// Seconds to fall 60 dB below `xover_hz`.
    pub t60_low: f32,
    /// Seconds to fall 60 dB above `xover_hz`.
    pub t60_high: f32,
    pub xover_hz: f32,
    /// One-pole damping in the loop: 0 is off, near 1 is dark.
    pub damp: f32,
    /// Peak modulation of each line's delay, in ms.
    pub mod_ms: f32,
    pub mod_hz: f32,
    /// Loop gains become 1 and absorbers and damping step aside.
    pub freeze: bool,
}

impl Default for TankSettings {
    fn default() -> Self {
        Self {
            lines: 8,
            scale: 1.0,
            allpass_scale: 1.0,
            diffusion: 0.5,
            t60_low: 2.0,
            t60_high: 2.0,
            xover_hz: 250.0,
            damp: 0.0,
            mod_ms: 0.0,
            mod_hz: 0.5,
            freeze: false,
        }
    }
}

/// The in-loop shelving filters. Gains are per trip around the loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TankShelves {
    pub high_on: bool,
    pub high_hz: f32,
    /// 0..1, where 1 leaves the band alone.
    pub high_gain: f32,
    pub low_on: bool,
    pub low_hz: f32,
    pub low_gain: f32,
}

impl Default for TankShelves {
    fn default() -> Self {
        Self {
            high_on: false,
            high_hz: 4_000.0,
            high_gain: 0.75,
            low_on: false,
            low_hz: 100.0,
            low_gain: 0.75,
        }
    }
}

#[derive(Clone, Debug)]
struct Line {
    delay: InterpDelay,
    allpass: InterpDelay,
    delay_len: f32,
    allpass_len: f32,
    gain_low: f32,
    gain_high: f32,
    crossover: f32,
    damped: f32,
    phase: f32,
    high: Biquad,
    low: Biquad,
}

/// See the module documentation.
#[derive(Clone, Debug)]
pub struct Tank {
    sr: f32,
    lines: Vec<Line>,
    settings: TankSettings,
    shelves: TankShelves,
    scale: f32,
    /// Per control step, moves `scale` towards its target. 1 jumps.
    glide: f32,
    xover_coeff: f32,
    damp: f32,
    mod_samples: f32,
    frames: usize,
    active: usize,
    shelf_high: bool,
    shelf_low: bool,
    /// Whether `scale` has been given its first value, which it takes without a glide.
    placed: bool,
}

impl Default for Tank {
    fn default() -> Self {
        Self {
            sr: 48_000.0,
            lines: Vec::new(),
            settings: TankSettings::default(),
            shelves: TankShelves::default(),
            scale: 1.0,
            glide: 1.0,
            xover_coeff: 0.0,
            damp: 0.0,
            mod_samples: 0.0,
            frames: 0,
            active: 8,
            shelf_high: false,
            shelf_low: false,
            placed: false,
        }
    }
}

impl Tank {
    /// Makes buffers for delay scales up to `max_scale`. Allocates.
    pub fn prepare(&mut self, sample_rate: f64, max_scale: f32) {
        self.sr = sample_rate as f32;
        let ms = self.sr * 0.001;
        let delay = ((LONGEST_LINE_MS * max_scale + MOD_MARGIN_MS) * ms) as usize + 16;
        let allpass =
            (ALLPASS_MS.iter().fold(0.0f32, |a, b| a.max(*b)) * max_scale * ms) as usize + 16;
        self.lines = (0..MAX_LINES)
            .map(|i| Line {
                delay: InterpDelay::new(delay),
                allpass: InterpDelay::new(allpass),
                delay_len: LINE_MS[i] * ms,
                allpass_len: (ALLPASS_MS[i] * ms).round(),
                gain_low: 1.0,
                gain_high: 1.0,
                crossover: 0.0,
                damped: 0.0,
                phase: i as f32 / MAX_LINES as f32,
                high: Biquad::default(),
                low: Biquad::default(),
            })
            .collect();
        self.apply_scale();
        self.apply_shelves();
    }

    /// Applies settings, jumping straight to a new `scale` when `glide_seconds` is 0 and otherwise
    /// moving to it over about that long.
    pub fn set(&mut self, settings: &TankSettings, glide_seconds: f32) {
        self.settings = *settings;
        self.active = settings.lines.clamp(4, MAX_LINES);
        self.xover_coeff = 1.0 - (-TAU * settings.xover_hz.max(10.0) / self.sr).exp();
        self.damp = if settings.freeze {
            0.0
        } else {
            settings.damp.clamp(0.0, 0.95)
        };
        self.mod_samples =
            (settings.mod_ms.max(0.0) * 0.001 * self.sr).min(MOD_MARGIN_MS * 0.001 * self.sr);
        self.glide = if glide_seconds <= 0.0 {
            1.0
        } else {
            1.0 - (-(CONTROL_FRAMES as f32) / (glide_seconds * self.sr)).exp()
        };
        if self.glide >= 1.0 || !self.placed {
            self.scale = settings.scale;
            self.placed = true;
        }
        self.apply_scale();
    }

    /// Applies in-loop shelves.
    pub fn set_shelves(&mut self, shelves: &TankShelves) {
        self.shelves = *shelves;
        self.apply_shelves();
    }

    fn apply_shelves(&mut self) {
        let s = self.shelves;
        for line in self.lines.iter_mut() {
            line.high.set(
                BiquadKind::HighShelf,
                s.high_hz,
                0.707,
                20.0 * s.high_gain.clamp(0.01, 1.0).log10(),
                self.sr,
            );
            line.low.set(
                BiquadKind::LowShelf,
                s.low_hz,
                0.707,
                20.0 * s.low_gain.clamp(0.01, 1.0).log10(),
                self.sr,
            );
        }
        self.shelf_high = s.high_on;
        self.shelf_low = s.low_on;
    }

    /// Turns the in-loop shelves off regardless of their settings, for Flat under Freeze.
    pub fn bypass_shelves(&mut self, bypass: bool) {
        self.shelf_high = self.shelves.high_on && !bypass;
        self.shelf_low = self.shelves.low_on && !bypass;
    }

    fn apply_scale(&mut self) {
        let ms = self.sr * 0.001;
        let s = self.settings;
        let settled = self.scale == s.scale && self.mod_samples == 0.0;
        for (i, line) in self.lines.iter_mut().enumerate() {
            let scaled = (LINE_MS[i] * self.scale * ms).max(MIN_SCALE_FOR_ONE_SAMPLE);
            let allpass = ALLPASS_MS[i] * s.allpass_scale * self.scale * ms;
            // Whole samples once nothing is moving; fractional on the way there, so no steps.
            if settled {
                line.delay_len = scaled.round();
                line.allpass_len = allpass.round();
            } else {
                line.delay_len = scaled;
                line.allpass_len = allpass;
            }
            let loop_seconds = (line.delay_len + line.allpass_len) / self.sr;
            let (low, high) = if s.freeze {
                (1.0, 1.0)
            } else {
                (
                    10f32.powf(-3.0 * loop_seconds / s.t60_low.max(1e-3)),
                    10f32.powf(-3.0 * loop_seconds / s.t60_high.max(1e-3)),
                )
            };
            line.gain_low = low;
            line.gain_high = high;
        }
    }

    pub fn clear(&mut self) {
        for line in self.lines.iter_mut() {
            line.delay.clear();
            line.allpass.clear();
            line.crossover = 0.0;
            line.damped = 0.0;
            line.high.reset();
            line.low.reset();
        }
    }

    #[inline]
    fn control(&mut self) {
        self.frames += 1;
        if self.frames < CONTROL_FRAMES {
            return;
        }
        self.frames = 0;
        let target = self.settings.scale;
        if self.scale != target {
            self.scale += (target - self.scale) * self.glide;
            if (self.scale - target).abs() < 1e-4 * target.abs().max(1e-3) {
                self.scale = target;
            }
            self.apply_scale();
        }
    }

    /// Runs one stereo frame. Out comes the left and right of the diffuse field.
    #[inline]
    pub fn process(&mut self, left: f32, right: f32) -> [f32; 2] {
        self.control();
        let n = self.active;
        let mut y = [0.0f32; MAX_LINES];
        let g = self.settings.diffusion;
        let (depth, hz) = (self.mod_samples, self.settings.mod_hz);
        let (xover, damp, sr) = (self.xover_coeff, self.damp, self.sr);
        let (shelf_high, shelf_low) = (self.shelf_high, self.shelf_low);
        let mut sum = 0.0f32;
        for (line, out) in self.lines[..n].iter_mut().zip(y.iter_mut()) {
            let d = if depth > 0.0 {
                line.phase += hz * (1.0 + line.delay_len / 40_000.0) / sr;
                if line.phase >= 1.0 {
                    line.phase -= 1.0;
                }
                line.delay.read(
                    (line.delay_len - 1.0 + depth * (1.0 + soft_sine(line.phase))).max(0.0),
                    Interpolation::Linear,
                )
            } else {
                line.delay.read(line.delay_len - 1.0, Interpolation::Linear)
            };
            let mut x = if line.allpass_len >= 1.0 {
                let feedback = line
                    .allpass
                    .read(line.allpass_len - 1.0, Interpolation::Linear);
                let v = d + g * feedback;
                line.allpass.write(v);
                feedback - g * v
            } else {
                d
            };
            line.crossover += xover * (x - line.crossover);
            let low = line.crossover;
            x = line.gain_low * low + line.gain_high * (x - low);
            if damp > 0.0 {
                line.damped += (1.0 - damp) * (x - line.damped);
                x = line.damped;
            }
            if shelf_high {
                x = line.high.process(x);
            }
            if shelf_low {
                x = line.low.process(x);
            }
            *out = x;
            sum += x;
        }
        let s = sum * 2.0 / n as f32;
        let unit = 1.0 / ((n / 2) as f32).sqrt();
        let (mut out_l, mut out_r) = (0.0f32, 0.0f32);
        for (i, line) in self.lines[..n].iter_mut().enumerate() {
            let input = if i % 2 == 0 { left } else { right };
            let sign = if i % 4 < 2 { 1.0 } else { -1.0 };
            line.delay
                .write(sanitize(soft_limit(y[i] - s + input * unit * sign)));
            if i % 2 == 0 {
                out_l += y[i] * sign;
            } else {
                out_r += y[i] * sign;
            }
        }
        [out_l * unit, out_r * unit]
    }
}

/// A chain of Schroeder allpasses, for smearing an input in time without colouring it.
#[derive(Clone, Debug, Default)]
pub struct Diffuser {
    stages: Vec<(InterpDelay, usize)>,
}

impl Diffuser {
    /// One stage for each time in `times_ms`. Allocates.
    pub fn prepare(&mut self, sample_rate: f64, times_ms: &[f32]) {
        let ms = sample_rate as f32 * 0.001;
        self.stages = times_ms
            .iter()
            .map(|t| {
                let n = (t * ms).max(1.0) as usize;
                (InterpDelay::new(n + 4), n)
            })
            .collect();
    }

    pub fn clear(&mut self) {
        for (line, _) in self.stages.iter_mut() {
            line.clear();
        }
    }

    /// `coefficient` 0 makes the chain a plain delay.
    #[inline]
    pub fn process(&mut self, mut x: f32, coefficient: f32) -> f32 {
        for (line, n) in self.stages.iter_mut() {
            let feedback = line.read((*n - 1) as f32, Interpolation::Linear);
            let v = x + coefficient * feedback;
            line.write(v);
            x = feedback - coefficient * v;
        }
        x
    }
}
