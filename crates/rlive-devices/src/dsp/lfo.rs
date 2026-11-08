//! Stereo LFO with free or tempo-locked rate, Phase and Spin.

use super::tempo::BeatClock;
use super::util::{sanitize, Rng};
use std::f32::consts::TAU;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LfoShape {
    #[default]
    Sine,
    Triangle,
    SawUp,
    SawDown,
    Square,
    /// A new random value each cycle, held.
    SampleHold,
    /// A new random value each cycle, reached with a raised-cosine glide.
    SmoothRandom,
}

/// How fast an LFO runs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LfoRate {
    Hz(f32),
    /// One cycle per this many beats. See [`SyncRate::beats`](super::tempo::SyncRate::beats).
    Beats(f64),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LfoSettings {
    pub shape: LfoShape,
    pub rate: LfoRate,
    /// Start point of the cycle, as a fraction of a cycle. Applies to both channels.
    pub offset: f32,
    /// How far the right channel is ahead of the left, as a fraction of a cycle.
    pub phase: f32,
    /// How much faster the right channel runs, as a ratio: 0.5 is 1.5 times the left's rate.
    pub spin: f32,
}

impl Default for LfoSettings {
    fn default() -> Self {
        Self {
            shape: LfoShape::Sine,
            rate: LfoRate::Hz(1.0),
            offset: 0.0,
            phase: 0.0,
            spin: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RandomState {
    previous: f32,
    current: f32,
    last_phase: f64,
    rng: Rng,
}

impl RandomState {
    fn new(seed: u32) -> Self {
        Self {
            previous: 0.0,
            current: 0.0,
            last_phase: 0.0,
            rng: Rng::new(seed),
        }
    }

    #[inline]
    fn update(&mut self, phase: f64) {
        if phase < self.last_phase {
            self.previous = self.current;
            self.current = self.rng.bipolar();
        }
        self.last_phase = phase;
    }
}

/// A stereo low-frequency oscillator, in `-1.0..=1.0`.
///
/// A [`LfoRate::Beats`] rate is derived from the song position while the transport plays, so it
/// cannot drift. Call [`set_clock`](Lfo::set_clock) at the start of each block.
#[derive(Clone, Debug)]
pub struct Lfo {
    settings: LfoSettings,
    sample_rate: f64,
    clock: BeatClock,
    phase_l: f64,
    phase_r: f64,
    random_l: RandomState,
    random_r: RandomState,
}

impl Lfo {
    /// `seed` makes the random shapes repeatable.
    pub fn new(seed: u32) -> Self {
        Self {
            settings: LfoSettings::default(),
            sample_rate: 48_000.0,
            clock: BeatClock::default(),
            phase_l: 0.0,
            phase_r: 0.0,
            random_l: RandomState::new(seed),
            random_r: RandomState::new(seed.wrapping_add(1)),
        }
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(1.0);
    }

    pub fn set(&mut self, settings: &LfoSettings) {
        self.settings = LfoSettings {
            offset: sanitize(settings.offset),
            phase: sanitize(settings.phase),
            spin: sanitize(settings.spin),
            ..*settings
        };
    }

    /// Follow the transport for the block that is starting.
    pub fn set_clock(&mut self, clock: &BeatClock) {
        self.clock = *clock;
    }

    /// Restart the free-running cycle from `offset`. Has no effect on a tempo-locked rate.
    pub fn retrigger(&mut self) {
        self.phase_l = 0.0;
        self.phase_r = 0.0;
    }

    fn cycles_per_sample(&self) -> f64 {
        let rate = match self.settings.rate {
            LfoRate::Hz(hz) => (hz.max(0.0) as f64) / self.sample_rate,
            LfoRate::Beats(beats) => self.clock.beats_per_sample / beats.max(1e-6),
        };
        rate.min(0.5)
    }

    #[inline]
    fn value(shape: LfoShape, phase: f64, random: &RandomState) -> f32 {
        let p = phase as f32;
        match shape {
            LfoShape::Sine => (p * TAU).sin(),
            LfoShape::Triangle => {
                if p < 0.25 {
                    4.0 * p
                } else if p < 0.75 {
                    2.0 - 4.0 * p
                } else {
                    4.0 * p - 4.0
                }
            }
            LfoShape::SawUp => 2.0 * p - 1.0,
            LfoShape::SawDown => 1.0 - 2.0 * p,
            LfoShape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoShape::SampleHold => random.current,
            LfoShape::SmoothRandom => {
                let t = 0.5 - 0.5 * (p * std::f32::consts::PI).cos();
                random.previous + (random.current - random.previous) * t
            }
        }
    }

    /// The output as `[left, right]`, then one sample of progress.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> [f32; 2] {
        let out = self.current();
        self.step(1);
        out
    }

    /// The output as `[left, right]`, then `frames` samples of progress. For control-rate use.
    pub fn advance(&mut self, frames: usize) -> [f32; 2] {
        let out = self.current();
        self.step(frames);
        out
    }

    /// Where the two channels are in their cycles, as `[left, right]` in `0.0..1.0`, then
    /// `frames` samples of progress. For a device that draws its own wave shapes, with the
    /// rate, Phase, Spin and offset still handled here.
    pub fn advance_phases(&mut self, frames: usize) -> [f32; 2] {
        let s = self.settings;
        let (base_l, base_r) = self.cycle_positions();
        let phase_l = (base_l + s.offset as f64).rem_euclid(1.0);
        let phase_r = (base_r + (s.offset + s.phase) as f64).rem_euclid(1.0);
        self.step(frames);
        [phase_l as f32, phase_r as f32]
    }

    #[inline]
    fn step(&mut self, frames: usize) {
        let n = frames as f64;
        self.clock.beat += self.clock.beats_per_sample * n;
        let dt = self.cycles_per_sample() * n;
        self.phase_l = (self.phase_l + dt).rem_euclid(1.0);
        self.phase_r =
            (self.phase_r + dt * (1.0 + self.settings.spin.max(-1.0) as f64)).rem_euclid(1.0);
    }

    #[inline]
    fn cycle_positions(&self) -> (f64, f64) {
        let s = self.settings;
        match (s.rate, self.clock.playing) {
            (LfoRate::Beats(beats), true) => {
                let cycles = self.clock.beat / beats.max(1e-6);
                (cycles, cycles * (1.0 + s.spin.max(-1.0) as f64))
            }
            _ => (self.phase_l, self.phase_r),
        }
    }

    #[inline]
    fn current(&mut self) -> [f32; 2] {
        let s = self.settings;
        let (base_l, base_r) = self.cycle_positions();
        let phase_l = (base_l + s.offset as f64).rem_euclid(1.0);
        self.random_l.update(phase_l);
        let left = Self::value(s.shape, phase_l, &self.random_l);
        if s.phase == 0.0 && s.spin == 0.0 {
            return [left, left];
        }
        let phase_r = (base_r + (s.offset + s.phase) as f64).rem_euclid(1.0);
        self.random_r.update(phase_r);
        [left, Self::value(s.shape, phase_r, &self.random_r)]
    }
}
