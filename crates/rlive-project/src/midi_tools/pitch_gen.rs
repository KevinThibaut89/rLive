//! Seed and Shape: Generators that spread notes over a range of pitches (manual pp. 323-325).

use super::{in_scale, keys_between, ToolContext};
use crate::NoteData;
use rlive_core::PPQ;

// ------------------------------------------------------------- Seed

#[derive(Clone, Debug, PartialEq)]
pub struct Seed {
    pub pitch: (u8, u8),
    /// Note length in ticks, from 1/128 to a whole note.
    pub duration: (u64, u64),
    /// Velocity, 1/127 to 1.
    pub velocity: (f32, f32),
    /// The most notes sounding at once.
    pub voices: u32,
    /// How many notes, as a share of the keys in the range.
    pub density: f64,
}

impl Default for Seed {
    fn default() -> Self {
        Self {
            pitch: (60, 72),
            duration: (PPQ / 4, PPQ),
            velocity: (0.4, 1.0),
            voices: 3,
            density: 1.0,
        }
    }
}

/// How many tries a note gets to find a place the Voices limit allows.
const TRIES: usize = 48;

impl Seed {
    pub(super) fn run(&self, cx: &ToolContext) -> Vec<NoteData> {
        let span = cx.span();
        let keys = keys_between(self.pitch.0, self.pitch.1, cx.scale);
        if span == 0 || keys.is_empty() {
            return Vec::new();
        }
        let mut rng = cx.rng();
        let count = (self.density.clamp(0.0, 1.0) * keys.len() as f64).round() as usize;
        let limit = self.voices.max(1) as usize;
        let (dur_lo, dur_hi) = (
            self.duration.0.min(self.duration.1).max(1),
            self.duration.0.max(self.duration.1).max(1),
        );
        let (vel_lo, vel_hi) = (
            self.velocity.0.min(self.velocity.1),
            self.velocity.0.max(self.velocity.1),
        );
        let mut order = keys.clone();
        rng.shuffle(&mut order);
        let mut out: Vec<NoteData> = Vec::with_capacity(count);
        for key in order.into_iter().cycle().take(count) {
            for _ in 0..TRIES {
                let length = dur_lo + ((dur_hi - dur_lo) as f64 * rng.unit()) as u64;
                let room = span.saturating_sub(length.min(span));
                let start = cx.range.0 + (room as f64 * rng.unit()) as u64;
                let end = (start + length).min(cx.range.1);
                let sounding = out
                    .iter()
                    .filter(|n| n.start < end && start < n.start + n.length)
                    .count();
                let clash = out
                    .iter()
                    .any(|n| n.key == key && n.start < end && start < n.start + n.length);
                if sounding < limit && !clash {
                    let velocity = vel_lo + (vel_hi - vel_lo) * rng.unit() as f32;
                    out.push(NoteData::new(start, end - start, key, velocity));
                    break;
                }
            }
        }
        out
    }
}

// ------------------------------------------------------------- Shape

/// How many levels a Shape's drawing has.
pub const SHAPE_POINTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapePreset {
    Rise,
    Fall,
    Triangle,
    Valley,
    Sine,
    Saw,
}

impl ShapePreset {
    pub const ALL: [ShapePreset; 6] = [
        Self::Rise,
        Self::Fall,
        Self::Triangle,
        Self::Valley,
        Self::Sine,
        Self::Saw,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Rise => "Rise",
            Self::Fall => "Fall",
            Self::Triangle => "Triangle",
            Self::Valley => "Valley",
            Self::Sine => "Sine",
            Self::Saw => "Saw",
        }
    }

    /// The preset's levels, each 0 to 1.
    pub fn levels(self) -> Vec<f64> {
        let last = (SHAPE_POINTS - 1) as f64;
        (0..SHAPE_POINTS)
            .map(|i| {
                let x = i as f64 / last;
                match self {
                    Self::Rise => x,
                    Self::Fall => 1.0 - x,
                    Self::Triangle => 1.0 - (2.0 * x - 1.0).abs(),
                    Self::Valley => (2.0 * x - 1.0).abs(),
                    Self::Sine => 0.5 + 0.5 * (std::f64::consts::TAU * x).sin(),
                    Self::Saw => (x * 2.0).fract(),
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// The drawing: `SHAPE_POINTS` levels, each 0 to 1.
    pub levels: Vec<f64>,
    pub pitch: (u8, u8),
    /// The shortest a note is.
    pub rate: u64,
    /// The chance a note holds on to the next one.
    pub tie: f64,
    /// The share of the shape to fill with notes.
    pub density: f64,
    /// How far notes stray from the shape, 0 to 1.
    pub jitter: f64,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            levels: ShapePreset::Rise.levels(),
            pitch: (60, 72),
            rate: PPQ / 4,
            tie: 0.0,
            density: 1.0,
            jitter: 0.0,
        }
    }
}

impl Shape {
    pub(super) fn run(&self, cx: &ToolContext) -> Vec<NoteData> {
        let span = cx.span();
        if span == 0 || self.levels.is_empty() {
            return Vec::new();
        }
        let mut rng = cx.rng();
        let rate = self.rate.max(1);
        let (lo, hi) = (
            f64::from(self.pitch.0.min(self.pitch.1)),
            f64::from(self.pitch.0.max(self.pitch.1)),
        );
        let slots = span.div_ceil(rate) as usize;
        let wanted = (self.density.clamp(0.0, 1.0) * slots as f64).round() as usize;
        let mut picked: Vec<usize> = (0..slots).collect();
        rng.shuffle(&mut picked);
        picked.truncate(wanted);
        picked.sort_unstable();
        let mut out: Vec<NoteData> = Vec::with_capacity(picked.len());
        for (at, slot) in picked.iter().enumerate() {
            let start = cx.range.0 + *slot as u64 * rate;
            let x = *slot as f64 / slots as f64;
            let cell = ((x * self.levels.len() as f64) as usize).min(self.levels.len() - 1);
            let level = self.levels[cell].clamp(0.0, 1.0);
            let wobble = rng.signed() * self.jitter.clamp(0.0, 1.0) * (hi - lo);
            let pitch = (lo + level * (hi - lo) + wobble).clamp(lo, hi);
            let key = in_scale(pitch.round() as i32, cx.scale);
            let end = match picked.get(at + 1) {
                Some(next) if rng.chance(self.tie) => cx.range.0 + *next as u64 * rate,
                _ => (start + rate).min(cx.range.1),
            };
            out.push(NoteData::new(start, end - start, key, 0.8));
        }
        out
    }
}
