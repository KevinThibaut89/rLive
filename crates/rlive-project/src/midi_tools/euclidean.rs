//! Euclidean: up to four voices of evenly spread hits (manual pp. 327-329).

use super::{in_scale, Rng, ToolContext};
use crate::NoteData;
use rlive_core::PPQ;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EuclidVoice {
    pub on: bool,
    pub pitch: u8,
    pub velocity: f32,
    /// Steps the voice is moved along the pattern.
    pub rotation: i32,
    /// How many hits the voice spreads across the steps.
    pub pulses: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Euclidean {
    pub voices: [EuclidVoice; 4],
    /// How long the pattern is.
    pub steps: u32,
    /// How many times it repeats in the span; 0 fills the span.
    pub density: u32,
    /// How long a step is.
    pub division: u64,
}

impl Default for Euclidean {
    fn default() -> Self {
        let voice = |on, pitch, pulses| EuclidVoice {
            on,
            pitch,
            velocity: 0.8,
            rotation: 0,
            pulses,
        };
        Self {
            voices: [
                voice(true, 36, 4),
                voice(true, 38, 3),
                voice(false, 42, 5),
                voice(false, 46, 7),
            ],
            steps: 16,
            density: 0,
            division: PPQ / 4,
        }
    }
}

/// `pulses` hits spread as evenly as they go over `steps`.
pub(super) fn euclid(steps: u32, pulses: u32) -> Vec<bool> {
    let pulses = pulses.min(steps);
    (0..steps)
        .map(|i| (i * pulses) % steps.max(1) < pulses)
        .collect()
}

impl Euclidean {
    /// The Randomize button: every voice's rotation, at random.
    pub fn randomize_rotations(&mut self, seed: u64) {
        let mut rng = Rng::new(seed);
        let steps = self.steps.max(1) as usize;
        for v in &mut self.voices {
            v.rotation = rng.below(steps) as i32;
        }
    }

    pub(super) fn run(&self, cx: &ToolContext) -> Vec<NoteData> {
        let span = cx.span();
        if span == 0 {
            return Vec::new();
        }
        let division = self.division.max(1);
        let steps = self.steps.max(1);
        let repeats = if self.density > 0 {
            u64::from(self.density)
        } else {
            (span.div_ceil(u64::from(steps) * division)).max(1)
        };
        let mut out: Vec<NoteData> = Vec::new();
        for v in self.voices.iter().filter(|v| v.on) {
            let key = in_scale(i32::from(v.pitch), cx.scale);
            let pattern = euclid(steps, v.pulses);
            for r in 0..repeats {
                for (i, _) in pattern.iter().enumerate().filter(|(_, hit)| **hit) {
                    let step = r as i64 * i64::from(steps) + i as i64 + i64::from(v.rotation);
                    // What doesn't fit wraps around the span (manual p. 328).
                    let at = (step * division as i64).rem_euclid(span as i64) as u64 + cx.range.0;
                    let length = division.min(cx.range.1 - at).max(1);
                    if out.iter().all(|n| n.key != key || n.start != at) {
                        out.push(NoteData::new(at, length, key, v.velocity));
                    }
                }
            }
        }
        out
    }
}
