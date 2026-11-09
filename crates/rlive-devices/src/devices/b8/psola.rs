//! Pitch-synchronous grain shifting for Auto Shift.
//!
//! Grains two analysis periods long, taken a whole number of periods apart, are overlapped at the
//! new period: the pitch moves, the waveform's shape, and so the formants, stay. Reading each grain
//! faster or slower moves the formants on their own, by `formant`. The grain is centred on the
//! output sample `latency` behind the input, so the time alignment is kept to about a period
//! at most.

use crate::dsp::util::{cubic, sanitize};

const RING_BITS: u32 = 16;
pub const RING: usize = 1 << RING_BITS;
const MASK: usize = RING - 1;
const MAX_GRAINS: usize = 12;
/// A ring holds up to this many periods of history, generous for any shift.
const SAFETY: f64 = 8.0;
/// Below this overlap, output is left as is rather than amplified.
const WINDOW_FLOOR: f32 = 0.25;

/// The input, kept for grains to read and for the dry and bypass paths to tap.
#[derive(Clone, Debug)]
pub struct History {
    left: Vec<f32>,
    right: Vec<f32>,
    write: u64,
}

impl Default for History {
    fn default() -> Self {
        Self {
            left: vec![0.0; RING],
            right: vec![0.0; RING],
            write: 0,
        }
    }
}

impl History {
    pub fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
    }

    #[inline]
    pub fn push(&mut self, left: f32, right: f32) {
        let at = self.write as usize & MASK;
        self.left[at] = sanitize(left);
        self.right[at] = sanitize(right);
        self.write += 1;
    }

    /// The absolute position of the next sample to be written.
    pub fn now(&self) -> u64 {
        self.write
    }

    /// The sample at an exact `delay` behind the most recent one.
    #[inline]
    pub fn tap(&self, delay: usize) -> (f32, f32) {
        let at = self.write.wrapping_sub(1 + delay as u64) as usize & MASK;
        (self.left[at], self.right[at])
    }

    #[inline]
    fn read(&self, position: f64) -> (f32, f32) {
        let floor = position.floor();
        let frac = (position - floor) as f32;
        let i = floor as i64;
        let at = |k: i64| (i + k) as u64 as usize & MASK;
        let (a, b, c, d) = (at(-1), at(0), at(1), at(2));
        (
            cubic(self.left[a], self.left[b], self.left[c], self.left[d], frac),
            cubic(
                self.right[a],
                self.right[b],
                self.right[c],
                self.right[d],
                frac,
            ),
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Grain {
    active: bool,
    /// Where the grain reads next, in absolute input samples.
    position: f64,
    speed: f32,
    /// How far through its window, 0 to 1.
    phase: f32,
    increment: f32,
}

/// What a voice is asked to do at its next grain.
#[derive(Clone, Copy, Debug)]
pub struct ShiftSettings {
    /// The period of the input now, in samples.
    pub period: f32,
    /// Output pitch over input pitch.
    pub ratio: f32,
    /// Formant frequency ratio: 1 leaves them where they are.
    pub formant: f32,
    /// How far behind the input the output stays, in samples.
    pub latency: f32,
    /// How long a grain may last, in samples of input on either side of its centre.
    pub max_half_width: f32,
    /// The period is a real pitch period; grains can be placed pitch synchronously.
    pub periodic: bool,
}

/// One pitch-shifted copy of the input.
#[derive(Clone, Debug, Default)]
pub struct ShiftVoice {
    grains: [Grain; MAX_GRAINS],
    /// Output samples until the next grain starts.
    next_grain: f32,
    /// Where the last grain was centred.
    last_centre: Option<f64>,
}

#[inline]
fn hann(phase: f32) -> f32 {
    let s = (std::f32::consts::PI * phase).sin();
    s * s
}

impl ShiftVoice {
    pub fn reset(&mut self) {
        self.grains = [Grain::default(); MAX_GRAINS];
        self.next_grain = 0.0;
        self.last_centre = None;
    }

    pub fn is_idle(&self) -> bool {
        self.grains.iter().all(|g| !g.active)
    }

    fn spawn(&mut self, history: &History, settings: &ShiftSettings) {
        let period = settings.period.max(4.0);
        let ratio = settings.ratio.clamp(0.1, 16.0);
        let formant = settings.formant.clamp(0.25, 4.0);
        let t_s = period / ratio;
        // Down shifts need overlapping windows longer than the period; up shifts just overlap.
        let half = period.max(0.5 * t_s).min(settings.max_half_width);
        let out_half = half / formant;
        let now = history.now() as f64;
        // Align the grain's centre with what should sound in `out_half` samples' time.
        let target = now + out_half as f64 - settings.latency as f64;
        let centre = match self.last_centre {
            Some(last) if settings.periodic => {
                let turns = ((target - last) / period as f64).round();
                last + turns * period as f64
            }
            _ => target,
        };
        // A grain can't read samples that have not arrived, however it's sped up, and can't
        // reach behind the history.
        let latest =
            now + (half as f64).min(2.0 * half as f64 / formant as f64 - half as f64) - 3.0;
        let earliest = now - (RING as f64 - 16.0).min(SAFETY * period as f64 + half as f64);
        let centre = centre.min(latest).max(earliest + half as f64);
        self.last_centre = Some(centre);
        let grain = Grain {
            active: true,
            position: centre - half as f64,
            speed: formant,
            phase: 0.0,
            increment: formant / (2.0 * half),
        };
        if let Some(slot) = self.grains.iter_mut().find(|g| !g.active) {
            *slot = grain;
        } else if let Some(slot) = self
            .grains
            .iter_mut()
            .max_by(|a, b| a.phase.total_cmp(&b.phase))
        {
            *slot = grain;
        }
        self.next_grain += t_s.max(1.0);
    }

    /// One output sample, advancing the grains. Returns `(left, right, window sum)`.
    #[inline]
    pub fn next(&mut self, history: &History, settings: &ShiftSettings) -> (f32, f32) {
        self.next_grain -= 1.0;
        while self.next_grain <= 0.0 {
            self.spawn(history, settings);
        }
        let (mut left, mut right, mut sum) = (0.0f32, 0.0f32, 0.0f32);
        for g in self.grains.iter_mut() {
            if !g.active {
                continue;
            }
            let w = hann(g.phase);
            let (l, r) = history.read(g.position);
            left += l * w;
            right += r * w;
            sum += w;
            g.position += g.speed as f64;
            g.phase += g.increment;
            if g.phase >= 1.0 {
                g.active = false;
            }
        }
        let norm = 1.0 / sum.max(WINDOW_FLOOR);
        (left * norm, right * norm)
    }
}
