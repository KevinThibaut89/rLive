//! What the EQ and filter devices share: control-rate ramps, LFO wave shapes and a beat grid.

use crate::dsp::util::Rng;
use rlive_core::db_to_gain;
use std::f32::consts::TAU;

/// How often the devices here update coefficients and modulation, in samples.
pub const CONTROL_FRAMES: usize = 16;

/// The gain value that means silence, for a control that reads -inf dB at its minimum.
pub const SILENCE_DB: f64 = -70.0;

/// A decibel control as a gain: at [`SILENCE_DB`] and under, silence.
#[inline]
pub fn gain_or_silence(db: f64) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        db_to_gain(db as f32)
    }
}

/// A value easing towards a target. Ramps are ticked at control rate, so they're used for what
/// sets coefficients; smooth a frequency by ramping its `log2`.
#[derive(Clone, Copy, Debug)]
pub struct Ramp {
    pub value: f32,
    pub target: f32,
}

impl Ramp {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            target: value,
        }
    }

    /// Jump straight to `target`.
    pub fn snap(&mut self, target: f32) {
        self.value = target;
        self.target = target;
    }

    /// Moves on by `frames` samples. Says whether the value changed.
    pub fn tick(&mut self, frames: usize, seconds: f32, sample_rate: f32) -> bool {
        if self.value == self.target {
            return false;
        }
        let k = 1.0 - (-(frames as f32) / (seconds.max(1e-4) * sample_rate)).exp();
        self.value += (self.target - self.value) * k;
        if (self.target - self.value).abs() < 1e-4 {
            self.value = self.target;
        }
        true
    }
}

#[inline]
pub fn sine(p: f32) -> f32 {
    (p * TAU).sin()
}

#[inline]
pub fn triangle(p: f32) -> f32 {
    if p < 0.25 {
        4.0 * p
    } else if p < 0.75 {
        2.0 - 4.0 * p
    } else {
        4.0 * p - 4.0
    }
}

#[inline]
pub fn saw_up(p: f32) -> f32 {
    2.0 * p - 1.0
}

#[inline]
pub fn saw_down(p: f32) -> f32 {
    1.0 - 2.0 * p
}

#[inline]
pub fn square(p: f32) -> f32 {
    if p < 0.5 {
        1.0
    } else {
        -1.0
    }
}

/// A fast concave rise to the top at the middle of the cycle, then the same fall.
#[inline]
pub fn shark_tooth(p: f32) -> f32 {
    if p < 0.5 {
        2.0 * (2.0 * p).sqrt() - 1.0
    } else {
        1.0 - 2.0 * (2.0 * p - 1.0).sqrt()
    }
}

/// A random value per cycle, for the random wave shapes.
#[derive(Clone, Copy, Debug)]
pub struct RandomWave {
    rng: Rng,
    previous: f32,
    current: f32,
    last_phase: f32,
}

impl RandomWave {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: Rng::new(seed),
            previous: 0.0,
            current: 0.0,
            last_phase: 0.0,
        }
    }

    /// Follows the cycle. Takes a new value whenever `phase` wraps.
    #[inline]
    pub fn update(&mut self, phase: f32) {
        if phase < self.last_phase {
            self.previous = self.current;
            self.current = self.rng.bipolar();
        }
        self.last_phase = phase;
    }

    /// The value of this cycle, held.
    #[inline]
    pub fn held(&self) -> f32 {
        self.current
    }

    /// The value of this cycle, reached with a raised-cosine glide.
    #[inline]
    pub fn glide(&self, phase: f32) -> f32 {
        let t = 0.5 - 0.5 * (phase * std::f32::consts::PI).cos();
        self.previous + (self.current - self.previous) * t
    }
}

/// Says when a beat position has moved on to a new step of a repeating grid.
#[derive(Clone, Copy, Debug)]
pub struct StepGrid {
    step: f64,
}

impl Default for StepGrid {
    fn default() -> Self {
        // Not a step any position is in, so the first call always reports a new step.
        Self {
            step: f64::NEG_INFINITY,
        }
    }
}

impl StepGrid {
    /// Whether `position` (beats) is in a different step of `period` beats to the last call.
    #[inline]
    pub fn crossed(&mut self, position: f64, period: f64) -> bool {
        let step = (position / period.max(1e-6)).floor();
        let moved = step != self.step;
        self.step = step;
        moved
    }
}
