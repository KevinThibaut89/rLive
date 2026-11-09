//! Recombine: gives one note's position, pitch, duration or velocity to another (manual
//! pp. 310-312).

use super::{Rng, ToolContext};
use crate::NoteData;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Position,
    Pitch,
    Duration,
    Velocity,
}

impl Dimension {
    pub const ALL: [Dimension; 4] = [Self::Position, Self::Pitch, Self::Duration, Self::Velocity];

    pub fn label(self) -> &'static str {
        match self {
            Self::Position => "Position",
            Self::Pitch => "Pitch",
            Self::Duration => "Duration",
            Self::Velocity => "Velocity",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Recombine {
    pub dimension: Dimension,
    pub shuffle: bool,
    pub mirror: bool,
    /// Rotation Steps: above zero clockwise, below zero the other way.
    pub rotate: i32,
    /// With Position, Rotation Steps are grid cells across the span, not notes.
    pub rotate_on_grid: bool,
}

impl Default for Recombine {
    fn default() -> Self {
        Self {
            dimension: Dimension::Position,
            shuffle: false,
            mirror: false,
            rotate: 0,
            rotate_on_grid: false,
        }
    }
}

/// What a note gives up: one value, of whichever dimension.
fn value(n: &NoteData, d: Dimension) -> f64 {
    match d {
        Dimension::Position => n.start as f64,
        Dimension::Pitch => f64::from(n.key),
        Dimension::Duration => n.length as f64,
        Dimension::Velocity => f64::from(n.velocity),
    }
}

fn set_value(n: &mut NoteData, d: Dimension, v: f64) {
    match d {
        Dimension::Position => n.start = v as u64,
        Dimension::Pitch => n.key = v as u8,
        Dimension::Duration => n.length = v as u64,
        Dimension::Velocity => n.velocity = v as f32,
    }
}

impl Recombine {
    /// How many Rotation Steps there are either way (manual p. 312).
    pub fn most_steps(&self, notes: usize, cx_span: u64, grid: u64) -> i32 {
        if self.rotate_on_grid && self.dimension == Dimension::Position {
            (cx_span / grid.max(1)).saturating_sub(1) as i32
        } else {
            notes.saturating_sub(1) as i32
        }
    }

    /// Shuffle, then Mirror, then Rotate (manual p. 312).
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let mut out = input.to_vec();
        out.sort_by_key(|n| (n.start, n.key));
        let d = self.dimension;
        let mut values: Vec<f64> = out.iter().map(|n| value(n, d)).collect();
        let mut rng: Rng = cx.rng();
        if self.shuffle {
            rng.shuffle(&mut values);
        }
        if self.mirror {
            values.reverse();
        }
        let on_grid = self.rotate_on_grid && d == Dimension::Position;
        if !on_grid && !values.is_empty() {
            let by = self.rotate.rem_euclid(values.len() as i32) as usize;
            values.rotate_right(by);
        }
        for (n, v) in out.iter_mut().zip(values) {
            set_value(n, d, v);
        }
        if on_grid && cx.span() > 0 {
            let by = i128::from(self.rotate) * i128::from(cx.grid.max(1));
            let span = i128::from(cx.span());
            for n in &mut out {
                let rel = i128::from(n.start) - i128::from(cx.range.0);
                n.start = (rel + by).rem_euclid(span) as u64 + cx.range.0;
            }
        }
        out
    }
}
