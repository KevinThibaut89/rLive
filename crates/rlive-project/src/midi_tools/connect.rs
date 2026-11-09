//! Connect: fills the gaps between notes with new ones (manual pp. 300-301).

use super::{in_scale, step_key, ToolContext};
use crate::NoteData;
use rlive_core::PPQ;

#[derive(Clone, Debug, PartialEq)]
pub struct Connect {
    /// How far a new note may stray from the line between its neighbours, in steps.
    pub spread: u32,
    /// The share of each gap to fill, 0 to 1.
    pub density: f64,
    /// How long new notes are.
    pub rate: u64,
    /// The chance a new note holds on to the next original note.
    pub tie: f64,
}

impl Default for Connect {
    fn default() -> Self {
        Self {
            spread: 2,
            density: 1.0,
            rate: PPQ / 4,
            tie: 0.0,
        }
    }
}

impl Connect {
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let mut rng = cx.rng();
        let mut sorted = input.to_vec();
        sorted.sort_by_key(|n| (n.start, n.key));
        let rate = self.rate.max(1);
        let mut out = sorted.clone();
        for pair in sorted.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            let gap_from = a.start + a.length;
            if b.start <= gap_from {
                continue;
            }
            let gap = b.start - gap_from;
            let slots = (gap.div_ceil(rate)) as usize;
            let wanted = (self.density.clamp(0.0, 1.0) * slots as f64).round() as usize;
            let mut order: Vec<usize> = (0..slots).collect();
            rng.shuffle(&mut order);
            let mut chosen: Vec<usize> = order.into_iter().take(wanted).collect();
            chosen.sort_unstable();
            for slot in chosen {
                let start = gap_from + slot as u64 * rate;
                let x = (start - a.start) as f64 / (b.start - a.start) as f64;
                let line = f64::from(a.key) + (f64::from(b.key) - f64::from(a.key)) * x;
                let mut key = in_scale(line.round() as i32, cx.scale);
                let spread = self.spread as i32;
                if spread > 0 {
                    let by = rng.below(2 * spread as usize + 1) as i32 - spread;
                    key = step_key(key, by, cx.scale);
                }
                let tied = rng.chance(self.tie);
                let end = if tied {
                    b.start
                } else {
                    (start + rate).min(b.start)
                };
                out.push(NoteData {
                    key,
                    velocity: (a.velocity + b.velocity) / 2.0,
                    ..NoteData::new(start, end - start, key, a.velocity)
                });
                if tied {
                    break;
                }
            }
        }
        out
    }
}
