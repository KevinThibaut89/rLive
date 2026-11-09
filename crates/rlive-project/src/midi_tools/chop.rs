//! Chop: divides notes into a pattern of parts and gaps (manual pp. 298-299).

use super::{piece, ToolContext};
use crate::NoteData;

/// The most parts a pattern holds; with more, it repeats.
const PATTERN_MAX: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct Chop {
    /// How many parts, 2 to 64.
    pub parts: u32,
    /// The Gaps control. `None` when the Pattern toggles have been changed by hand.
    pub gaps: Option<i32>,
    /// Each part of the pattern: a note if true, a gap if false.
    pub pattern: [bool; PATTERN_MAX],
    /// Emphasised parts are longer than the rest.
    pub emphasis: [bool; PATTERN_MAX],
    /// How much longer an emphasised part is, 2 to 8 times.
    pub stretch: u32,
    /// Random movement of each part's start and end, 0 to 1.
    pub variation: f64,
}

impl Default for Chop {
    fn default() -> Self {
        Self {
            parts: 4,
            gaps: Some(0),
            pattern: [true; PATTERN_MAX],
            emphasis: [false; PATTERN_MAX],
            stretch: 2,
            variation: 0.0,
        }
    }
}

/// The pattern the Gaps control makes: above zero, how many notes come before each gap; below
/// zero, how many gaps come after each note.
fn gaps_pattern(gaps: i32) -> [bool; PATTERN_MAX] {
    let mut p = [true; PATTERN_MAX];
    if gaps > 0 {
        let cycle = gaps as usize + 1;
        for (i, v) in p.iter_mut().enumerate() {
            *v = i % cycle != cycle - 1;
        }
    } else if gaps < 0 {
        let cycle = gaps.unsigned_abs() as usize + 1;
        for (i, v) in p.iter_mut().enumerate() {
            *v = i % cycle == 0;
        }
    }
    p
}

impl Chop {
    /// How many of the pattern's parts are in use.
    pub fn pattern_len(&self) -> usize {
        (self.parts as usize).clamp(2, PATTERN_MAX)
    }

    /// How far the Gaps control reaches with this many parts.
    pub fn gaps_range(&self) -> (i32, i32) {
        let most = self.pattern_len() as i32 - 1;
        (-most, most)
    }

    /// Sets the Gaps control, replacing any gaps put in by hand.
    pub fn set_gaps(&mut self, gaps: i32) {
        let (lo, hi) = self.gaps_range();
        let gaps = gaps.clamp(lo, hi);
        self.gaps = Some(gaps);
        self.pattern = gaps_pattern(gaps);
    }

    /// Flips one Pattern toggle. The Gaps control then has no value of its own.
    pub fn toggle_part(&mut self, part: usize) {
        if let Some(v) = self.pattern.get_mut(part) {
            *v = !*v;
            self.gaps = None;
        }
    }

    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let parts = self.parts.clamp(2, 64) as usize;
        let len = self.pattern_len();
        let weight = |i: usize| {
            if self.emphasis[i % len] {
                f64::from(self.stretch.clamp(2, 8))
            } else {
                1.0
            }
        };
        let total: f64 = (0..parts).map(weight).sum();
        let mut rng = cx.rng();
        let mut out = Vec::new();
        for n in input {
            let mut at = 0.0;
            for part in 0..parts {
                let width = weight(part) / total * n.length as f64;
                let (a, b) = (at, at + width);
                at = b;
                if !self.pattern[part % len] {
                    continue;
                }
                let reach = self.variation.clamp(0.0, 1.0) * width / 2.0;
                let start = (a + rng.signed() * reach).clamp(a, b);
                let end = (b + rng.signed() * reach).clamp(start + 1.0, b.max(start + 1.0));
                let tick = |x: f64| n.start + x.round().max(0.0) as u64;
                let (s, e) = (tick(start), tick(end));
                out.push(piece(n, s, e.saturating_sub(s)));
            }
        }
        out
    }
}
