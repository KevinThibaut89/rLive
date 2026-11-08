//! Granular engine: overlapping windowed grains, read from a live ring or a static sample.

use super::util::{cubic, sanitize, Rng};
use std::f32::consts::{FRAC_PI_2, TAU};

const WINDOW_SIZE: usize = 1024;

/// Somewhere grains can be read from, as absolute sample positions.
pub trait GrainSource {
    /// `(oldest, newest)` positions safe to read right now.
    fn range(&self) -> (f64, f64);

    /// How fast the range moves, in samples per output sample. A static sample is 0.
    fn motion(&self) -> f64 {
        0.0
    }

    /// The signal at a position within [`range`](GrainSource::range).
    fn read(&self, position: f64) -> f32;
}

/// A live input history for grains to read, addressed by how long ago a sample was written.
#[derive(Clone, Debug)]
pub struct GrainRing {
    buf: Vec<f32>,
    mask: usize,
    written: u64,
}

impl GrainRing {
    pub fn new(capacity: usize) -> Self {
        let size = (capacity + 8).next_power_of_two();
        Self {
            buf: vec![0.0; size],
            mask: size - 1,
            written: 0,
        }
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        self.written = 0;
    }

    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buf[self.written as usize & self.mask] = sanitize(x);
        self.written += 1;
    }

    /// The position `delay` samples before the latest write.
    pub fn position_ago(&self, delay: f64) -> f64 {
        (self.written as f64 - 1.0) - delay
    }
}

impl GrainSource for GrainRing {
    fn range(&self) -> (f64, f64) {
        let newest = self.written as f64 - 3.0;
        let span = self.buf.len() as f64 - 8.0;
        ((newest - span).max(2.0), newest)
    }

    fn motion(&self) -> f64 {
        1.0
    }

    #[inline]
    fn read(&self, position: f64) -> f32 {
        let i = position.floor();
        let t = (position - i) as f32;
        let i = i as i64;
        let at = |n: i64| self.buf[(n as usize) & self.mask];
        cubic(at(i - 1), at(i), at(i + 1), at(i + 2), t)
    }
}

/// A read-only sample for grains to read.
#[derive(Clone, Copy, Debug)]
pub struct GrainSample<'a> {
    pub data: &'a [f32],
}

impl GrainSource for GrainSample<'_> {
    fn range(&self) -> (f64, f64) {
        (1.0, (self.data.len() as f64 - 3.0).max(1.0))
    }

    #[inline]
    fn read(&self, position: f64) -> f32 {
        if self.data.len() < 4 {
            return 0.0;
        }
        let last = self.data.len() as i64 - 1;
        let i = (position.floor() as i64).clamp(1, last - 2);
        let t = (position - i as f64) as f32;
        let at = |n: i64| self.data[n as usize];
        cubic(at(i - 1), at(i), at(i + 1), at(i + 2), t)
    }
}

/// How grains are scheduled and shaped.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GrainSettings {
    /// New grains per second.
    pub rate: f32,
    /// Length of a grain, in seconds.
    pub size: f32,
    /// Playback-rate ratio. 2 is an octave up; negative plays backwards.
    pub pitch: f32,
    /// Where grains start, in seconds: before the write head for a [`GrainRing`], from the
    /// start of a [`GrainSample`].
    pub position: f32,
    /// Random spread added to `position`, in seconds.
    pub spray: f32,
    /// Random timing of the grain starts, 0..1 of the interval.
    pub jitter: f32,
    /// Random stereo position, 0..1.
    pub pan_spread: f32,
    /// Random detune of each grain, in semitones either way. Zero draws no randomness.
    pub pitch_random: f32,
}

impl Default for GrainSettings {
    fn default() -> Self {
        Self {
            rate: 20.0,
            size: 0.1,
            pitch: 1.0,
            position: 0.2,
            spray: 0.0,
            jitter: 0.0,
            pan_spread: 0.0,
            pitch_random: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Grain {
    active: bool,
    position: f64,
    speed: f64,
    /// Position in the window, 0..1.
    phase: f32,
    step: f32,
    left: f32,
    right: f32,
}

/// Overlapping Hann-windowed grains, with a fixed pool made at construction. Grains that don't
/// fit in the pool are skipped; a grain's window always reaches zero at both ends, so grains
/// can't click.
#[derive(Clone, Debug)]
pub struct GranularEngine {
    grains: Box<[Grain]>,
    window: Vec<f32>,
    settings: GrainSettings,
    sample_rate: f32,
    until_next: f64,
    rng: Rng,
    gain: f32,
}

impl GranularEngine {
    /// `max_grains` sounding at once. `seed` makes the random spreads repeatable.
    pub fn new(max_grains: usize, seed: u32) -> Self {
        let window = (0..=WINDOW_SIZE)
            .map(|i| 0.5 - 0.5 * (TAU * i as f32 / WINDOW_SIZE as f32).cos())
            .collect();
        Self {
            grains: vec![Grain::default(); max_grains.max(1)].into_boxed_slice(),
            window,
            settings: GrainSettings::default(),
            sample_rate: 48_000.0,
            until_next: 0.0,
            rng: Rng::new(seed),
            gain: 1.0,
        }
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(1.0) as f32;
        self.update_gain();
    }

    pub fn set(&mut self, settings: &GrainSettings) {
        self.settings = *settings;
        self.update_gain();
    }

    pub fn settings(&self) -> &GrainSettings {
        &self.settings
    }

    pub fn reset(&mut self) {
        self.grains.fill(Grain::default());
        self.until_next = 0.0;
    }

    /// How many grains are sounding.
    pub fn active_grains(&self) -> usize {
        self.grains.iter().filter(|g| g.active).count()
    }

    fn update_gain(&mut self) {
        let s = &self.settings;
        let overlap = (s.rate.max(0.0) * s.size.max(0.0)).max(0.0);
        // A Hann window averages one half, so `overlap` grains sum to `overlap / 2`.
        self.gain = if overlap > 2.0 { 2.0 / overlap } else { 1.0 };
    }

    fn trigger(&mut self, source: &impl GrainSource) {
        let Some(slot) = self.grains.iter_mut().find(|g| !g.active) else {
            return;
        };
        let s = &self.settings;
        let sr = self.sample_rate as f64;
        let frames = (s.size.max(0.001) as f64 * sr).max(4.0);
        let mut speed = s.pitch as f64;
        if s.pitch_random > 0.0 {
            speed *= 2f64.powf((self.rng.bipolar() * s.pitch_random) as f64 / 12.0);
        }
        let spray = self.rng.unipolar() as f64 * s.spray.max(0.0) as f64 * sr;
        let (oldest, newest) = source.range();
        let wanted = if source.motion() > 0.0 {
            newest - (s.position.max(0.0) as f64 * sr + spray)
        } else {
            oldest + s.position.max(0.0) as f64 * sr + spray
        };
        // Keep the whole grain inside what will be readable as it plays.
        let drift = (source.motion() - speed) * frames;
        let earliest = oldest + drift.max(0.0);
        let latest = newest + drift.min(0.0);
        if earliest > latest {
            return;
        }
        let pan = 0.5 + 0.5 * self.rng.bipolar() * s.pan_spread.clamp(0.0, 1.0);
        let angle = pan * FRAC_PI_2;
        let equal = std::f32::consts::SQRT_2;
        *slot = Grain {
            active: true,
            position: wanted.clamp(earliest, latest),
            speed,
            phase: 0.0,
            step: 1.0 / frames as f32,
            left: angle.cos() * equal,
            right: angle.sin() * equal,
        };
    }

    #[inline]
    fn window_at(window: &[f32], phase: f32) -> f32 {
        let at = phase.clamp(0.0, 1.0) * WINDOW_SIZE as f32;
        let i = (at as usize).min(WINDOW_SIZE - 1);
        let t = at - i as f32;
        window[i] + (window[i + 1] - window[i]) * t
    }

    /// One output frame as `[left, right]`, reading grains from `source`.
    #[inline]
    pub fn next(&mut self, source: &impl GrainSource) -> [f32; 2] {
        if self.settings.rate > 0.0 {
            self.until_next -= 1.0;
            if self.until_next <= 0.0 {
                self.trigger(source);
                let interval = (self.sample_rate / self.settings.rate.max(0.01)) as f64;
                let jitter =
                    1.0 + self.settings.jitter.clamp(0.0, 1.0) as f64 * self.rng.bipolar() as f64;
                // Never more than a grain a sample, and never catching up on missed ones.
                self.until_next = (self.until_next + interval * jitter).max(1.0);
            }
        }
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for g in self.grains.iter_mut().filter(|g| g.active) {
            let w = Self::window_at(&self.window, g.phase);
            let s = w * source.read(g.position);
            left += s * g.left;
            right += s * g.right;
            g.position += g.speed;
            g.phase += g.step;
            if g.phase >= 1.0 {
                g.active = false;
            }
        }
        [sanitize(left * self.gain), sanitize(right * self.gain)]
    }

    /// Adds a block of grains to `left` and `right`.
    pub fn process(&mut self, source: &impl GrainSource, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let [a, b] = self.next(source);
            *l += a;
            *r += b;
        }
    }
}
