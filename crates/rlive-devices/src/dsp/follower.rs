//! Envelope follower.

use super::util::sanitize;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DetectMode {
    #[default]
    Peak,
    Rms,
}

/// Tracks the level of a signal with separate attack and release times.
///
/// Times are one-pole time constants: a step reaches 63% of its height after `attack` seconds.
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeFollower {
    mode: DetectMode,
    attack: f32,
    release: f32,
    attack_coeff: f32,
    release_coeff: f32,
    sample_rate: f32,
    state: f32,
}

impl Default for EnvelopeFollower {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvelopeFollower {
    pub fn new() -> Self {
        let mut f = Self {
            mode: DetectMode::Peak,
            attack: 0.01,
            release: 0.1,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            sample_rate: 48_000.0,
            state: 0.0,
        };
        f.update();
        f
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(1.0) as f32;
        self.update();
    }

    pub fn set_mode(&mut self, mode: DetectMode) {
        self.mode = mode;
    }

    /// Attack and release in seconds. Zero follows instantly.
    pub fn set_times(&mut self, attack: f32, release: f32) {
        self.attack = attack.max(0.0);
        self.release = release.max(0.0);
        self.update();
    }

    fn update(&mut self) {
        let coeff = |t: f32, sr: f32| {
            if t > 0.0 {
                (-1.0 / (t * sr)).exp()
            } else {
                0.0
            }
        };
        self.attack_coeff = coeff(self.attack, self.sample_rate);
        self.release_coeff = coeff(self.release, self.sample_rate);
    }

    /// The current level.
    pub fn level(&self) -> f32 {
        match self.mode {
            DetectMode::Peak => self.state,
            DetectMode::Rms => self.state.sqrt(),
        }
    }

    pub fn reset(&mut self) {
        self.state = 0.0;
    }

    /// Follows one sample and returns the level.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = sanitize(x);
        let input = match self.mode {
            DetectMode::Peak => x.abs(),
            DetectMode::Rms => x * x,
        };
        let coeff = if input > self.state {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.state = sanitize(input + (self.state - input) * coeff);
        self.level()
    }

    /// Follows a block and returns the level at its end.
    pub fn process_block(&mut self, input: &[f32]) -> f32 {
        for &x in input {
            self.process(x);
        }
        self.level()
    }
}
