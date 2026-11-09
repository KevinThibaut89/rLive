//! Rhythm: a step pattern at one pitch, repeated to fill the span (manual pp. 321-323).

use super::{in_scale, ToolContext};
use crate::NoteData;
use rlive_core::PPQ;

/// The most steps a pattern can have.
pub const MAX_STEPS: u32 = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct Rhythm {
    pub pitch: u8,
    pub steps: u32,
    /// Which arrangement of the notes among the steps.
    pub pattern: u64,
    /// How many of the steps have a note.
    pub density: u32,
    pub step_duration: u64,
    /// The chance a step is divided in half.
    pub split: f64,
    /// Steps the pattern moves: right above zero, left below it.
    pub shift: i32,
    pub velocity: f32,
    pub accent: f32,
    /// Every this-many-th note is accented; 0 accents none.
    pub accent_frequency: u32,
    pub accent_offset: i32,
}

impl Default for Rhythm {
    fn default() -> Self {
        Self {
            pitch: 36,
            steps: 16,
            pattern: 0,
            density: 8,
            step_duration: PPQ / 4,
            split: 0.0,
            shift: 0,
            velocity: 0.8,
            accent: 1.0,
            accent_frequency: 0,
            accent_offset: 0,
        }
    }
}

/// How many ways `density` notes can go among `steps` steps.
pub fn pattern_count(steps: u32, density: u32) -> u64 {
    let (n, k) = (u64::from(steps), u64::from(density.min(steps)));
    let k = k.min(n - k.min(n));
    (0..k).fold(1, |acc, i| acc * (n - i) / (i + 1))
}

/// The `index`-th way of placing `density` notes among `steps` steps, in dictionary order.
fn arrangement(steps: u32, density: u32, index: u64) -> Vec<bool> {
    let n = steps as usize;
    let mut out = vec![false; n];
    let mut left = density.min(steps);
    let mut index = index % pattern_count(steps, left).max(1);
    let mut step = 0;
    while left > 0 && step < n {
        let without = pattern_count(steps - step as u32 - 1, left - 1);
        if index < without {
            out[step] = true;
            left -= 1;
        } else {
            index -= without;
        }
        step += 1;
    }
    out
}

impl Rhythm {
    /// The most steps there's room for, given how long a step is (manual p. 322).
    pub fn most_steps(&self, span: u64) -> u32 {
        let fits = (span / self.step_duration.max(1)).max(1);
        fits.min(u64::from(MAX_STEPS)) as u32
    }

    /// Which steps have a note.
    pub fn steps_on(&self, span: u64) -> Vec<bool> {
        let steps = self.steps.clamp(1, self.most_steps(span));
        let mut on = arrangement(steps, self.density.clamp(1, steps), self.pattern);
        let by = self.shift.rem_euclid(steps as i32) as usize;
        on.rotate_right(by);
        on
    }

    pub(super) fn run(&self, cx: &ToolContext) -> Vec<NoteData> {
        let (from, to) = cx.range;
        let span = cx.span();
        let on = self.steps_on(span);
        let dur = self.step_duration.max(1);
        let key = in_scale(i32::from(self.pitch), cx.scale);
        let mut rng = cx.rng();
        let mut out = Vec::new();
        let mut count = 0i64;
        let mut at = from;
        'fill: loop {
            for hit in &on {
                if at >= to {
                    break 'fill;
                }
                if *hit {
                    let f = i64::from(self.accent_frequency);
                    let accented =
                        f > 0 && count.rem_euclid(f) == i64::from(self.accent_offset).rem_euclid(f);
                    let velocity = if accented { self.accent } else { self.velocity };
                    count += 1;
                    let length = dur.min(to - at);
                    if length > 1 && rng.chance(self.split) {
                        let half = length / 2;
                        out.push(NoteData::new(at, half, key, velocity));
                        out.push(NoteData::new(at + half, length - half, key, velocity));
                    } else {
                        out.push(NoteData::new(at, length, key, velocity));
                    }
                }
                at += dur;
            }
        }
        out
    }
}
