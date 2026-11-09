//! How a groove moves notes: shared by the player, Commit and the warping of audio clips, so that
//! what Commit writes is what was heard.
//!
//! A groove is a pattern of hits. Each hit is measured against the nearest line of the Base grid,
//! giving every grid slot an offset in time and an accent. A note then goes through three steps:
//! it is pulled to its grid line by Quantize, moved by its slot's offset scaled by Timing, and
//! nudged by a random amount scaled by Random. Its velocity moves by the slot's accent scaled by
//! Velocity. Global Amount scales Timing, Random and Velocity, but not Quantize.

use crate::PPQ;
use serde::{Deserialize, Serialize};

/// How far a note can be nudged with Random at 100%: a 32nd note either way.
pub const RANDOM_RANGE: f64 = PPQ as f64 / 8.0;

/// The softest note a groove can leave, since a velocity of 0 would be a note-off.
const MIN_VELOCITY: f32 = 1.0 / 127.0;

/// One note of a groove pattern.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GrooveHit {
    /// Ticks from the start of the pattern.
    pub tick: f64,
    /// 0..1.
    pub velocity: f32,
}

/// The Groove Pool's knobs as fractions, after Global Amount.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GrooveAmounts {
    pub quantize: f32,
    pub timing: f32,
    pub random: f32,
    /// -1..1. Negative turns loud hits into quiet notes.
    pub velocity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Slot {
    offset: f32,
    accent: f32,
}

/// A groove measured against its Base grid, ready to place notes.
#[derive(Debug, Clone, PartialEq)]
pub struct GrooveMap {
    base: f64,
    slots: Box<[Slot]>,
    amounts: GrooveAmounts,
}

impl GrooveMap {
    /// Measures `hits`, a pattern `length` ticks long, against a grid of `base` ticks.
    pub fn new(hits: &[GrooveHit], length: f64, base: f64, amounts: GrooveAmounts) -> Self {
        let base = base.max(1.0);
        let count = ((length / base).round() as usize).max(1);
        let mut slots = vec![Slot::default(); count];
        let mut best = vec![f64::INFINITY; count];
        let mean = if hits.is_empty() {
            0.0
        } else {
            hits.iter().map(|h| h.velocity).sum::<f32>() / hits.len() as f32
        };
        for hit in hits {
            let line = (hit.tick / base).round();
            let offset = hit.tick - line * base;
            let slot = (line as i64).rem_euclid(count as i64) as usize;
            // Two hits on one line: the one nearer the grid speaks for it.
            if offset.abs() < best[slot] {
                best[slot] = offset.abs();
                slots[slot] = Slot {
                    offset: offset as f32,
                    accent: hit.velocity - mean,
                };
            }
        }
        Self {
            base,
            slots: slots.into_boxed_slice(),
            amounts,
        }
    }

    /// A map that moves nothing.
    pub fn neutral() -> Self {
        Self::new(&[], PPQ as f64, PPQ as f64, GrooveAmounts::default())
    }

    pub fn amounts(&self) -> GrooveAmounts {
        self.amounts
    }

    /// Where a note at content tick `start` lands and how hard it plays. `roll` is the note's own
    /// random number, in -1..1.
    pub fn place(&self, start: f64, velocity: f32, roll: f32) -> (f64, f32) {
        let a = &self.amounts;
        let line = (start / self.base).round();
        let snapped = start + (line * self.base - start) * f64::from(a.quantize);
        let slot = self.slots[(line as i64).rem_euclid(self.slots.len() as i64) as usize];
        let moved = snapped
            + f64::from(slot.offset * a.timing)
            + f64::from(roll.clamp(-1.0, 1.0) * a.random) * RANDOM_RANGE;
        let played = (velocity + slot.accent * a.velocity).clamp(MIN_VELOCITY, 1.0);
        (moved, played)
    }

    /// The furthest any note can be moved, so a player knows how wide to look.
    pub fn reach(&self) -> f64 {
        let a = &self.amounts;
        let largest = self
            .slots
            .iter()
            .map(|s| s.offset.abs())
            .fold(0.0f32, f32::max);
        f64::from(largest * a.timing.abs())
            + self.base * 0.5 * f64::from(a.quantize.abs())
            + RANDOM_RANGE * f64::from(a.random.abs())
    }
}

/// Every groove of a Set, in the form the audio thread reads. Clips point into it by index.
#[derive(Debug, Clone, PartialEq)]
pub struct GrooveBank {
    pub maps: Vec<GrooveMap>,
    /// Global Amount, for devices with grooves of their own to scale by.
    pub amount: f32,
}

impl Default for GrooveBank {
    fn default() -> Self {
        Self {
            maps: Vec::new(),
            amount: 1.0,
        }
    }
}

impl GrooveBank {
    pub fn get(&self, index: u32) -> Option<&GrooveMap> {
        self.maps.get(index as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXTEENTH: f64 = PPQ as f64 / 4.0;

    fn amounts(quantize: f32, timing: f32, random: f32, velocity: f32) -> GrooveAmounts {
        GrooveAmounts {
            quantize,
            timing,
            random,
            velocity,
        }
    }

    fn hit(tick: f64, velocity: f32) -> GrooveHit {
        GrooveHit { tick, velocity }
    }

    fn map(hits: &[GrooveHit], a: GrooveAmounts) -> GrooveMap {
        GrooveMap::new(hits, PPQ as f64, SIXTEENTH, a)
    }

    #[test]
    fn a_note_moves_by_its_slots_offset_scaled_by_timing() {
        let hits = [hit(0.0, 0.5), hit(SIXTEENTH + 30.0, 0.5)];
        let (at, _) = map(&hits, amounts(0.0, 1.0, 0.0, 0.0)).place(SIXTEENTH, 0.5, 0.0);
        assert_eq!(at, SIXTEENTH + 30.0);
        let (at, _) = map(&hits, amounts(0.0, 0.5, 0.0, 0.0)).place(SIXTEENTH, 0.5, 0.0);
        assert_eq!(at, SIXTEENTH + 15.0);
    }

    #[test]
    fn hits_on_the_grid_move_nothing() {
        let hits = [hit(0.0, 1.0), hit(SIXTEENTH, 1.0)];
        let (at, _) = map(&hits, amounts(0.0, 1.0, 0.0, 0.0)).place(SIXTEENTH, 0.5, 0.0);
        assert_eq!(at, SIXTEENTH);
    }

    #[test]
    fn the_pattern_repeats_along_the_clip() {
        let hits = [hit(SIXTEENTH + 20.0, 0.5)];
        let m = map(&hits, amounts(0.0, 1.0, 0.0, 0.0));
        let later = PPQ as f64 * 8.0 + SIXTEENTH;
        assert_eq!(m.place(later, 0.5, 0.0).0, later + 20.0);
    }

    #[test]
    fn quantize_snaps_to_the_base_before_the_groove() {
        let hits = [hit(SIXTEENTH + 30.0, 0.5)];
        let m = map(&hits, amounts(1.0, 0.0, 0.0, 0.0));
        assert_eq!(m.place(SIXTEENTH + 50.0, 0.5, 0.0).0, SIXTEENTH);
        let m = map(&hits, amounts(0.5, 0.0, 0.0, 0.0));
        assert_eq!(m.place(SIXTEENTH + 50.0, 0.5, 0.0).0, SIXTEENTH + 25.0);
        let m = map(&hits, amounts(1.0, 1.0, 0.0, 0.0));
        assert_eq!(m.place(SIXTEENTH + 50.0, 0.5, 0.0).0, SIXTEENTH + 30.0);
    }

    #[test]
    fn a_coarser_base_measures_the_same_hits_differently() {
        let quarter = PPQ as f64;
        let hits = [hit(quarter + 40.0, 0.5)];
        let m = GrooveMap::new(&hits, quarter * 2.0, quarter, amounts(0.0, 1.0, 0.0, 0.0));
        assert_eq!(m.place(quarter, 0.5, 0.0).0, quarter + 40.0);
        // The note a 16th later is nearest the same line, so it moves with it.
        assert_eq!(
            m.place(quarter + SIXTEENTH, 0.5, 0.0).0,
            quarter + SIXTEENTH + 40.0
        );
    }

    #[test]
    fn quantize_alone_ignores_the_pattern() {
        let hits = [hit(SIXTEENTH + 30.0, 1.0)];
        let (at, vel) = map(&hits, amounts(1.0, 0.0, 0.0, 0.0)).place(SIXTEENTH + 10.0, 0.5, 1.0);
        assert_eq!((at, vel), (SIXTEENTH, 0.5));
    }

    #[test]
    fn random_scales_the_roll() {
        let m = map(&[], amounts(0.0, 0.0, 1.0, 0.0));
        assert_eq!(m.place(100.0, 0.5, 1.0).0, 100.0 + RANDOM_RANGE);
        assert_eq!(m.place(100.0, 0.5, -1.0).0, 100.0 - RANDOM_RANGE);
        let m = map(&[], amounts(0.0, 0.0, 0.5, 0.0));
        assert_eq!(m.place(100.0, 0.5, 1.0).0, 100.0 + RANDOM_RANGE * 0.5);
    }

    #[test]
    fn velocity_follows_the_accent_and_a_negative_amount_turns_it_over() {
        let hits = [hit(0.0, 1.0), hit(SIXTEENTH, 0.5)];
        let up = map(&hits, amounts(0.0, 0.0, 0.0, 1.0));
        assert_eq!(up.place(0.0, 0.5, 0.0).1, 0.75);
        assert_eq!(up.place(SIXTEENTH, 0.5, 0.0).1, 0.25);
        let down = map(&hits, amounts(0.0, 0.0, 0.0, -1.0));
        assert_eq!(down.place(0.0, 0.5, 0.0).1, 0.25);
        assert_eq!(down.place(SIXTEENTH, 0.5, 0.0).1, 0.75);
    }

    #[test]
    fn velocity_never_reaches_silence_or_clips() {
        let hits = [hit(0.0, 1.0), hit(SIXTEENTH, 0.0)];
        let m = map(&hits, amounts(0.0, 0.0, 0.0, 1.0));
        assert_eq!(m.place(0.0, 1.0, 0.0).1, 1.0);
        assert_eq!(m.place(SIXTEENTH, 0.1, 0.0).1, MIN_VELOCITY);
    }

    #[test]
    fn reach_covers_the_largest_possible_move() {
        let hits = [hit(SIXTEENTH + 30.0, 0.5)];
        let m = map(&hits, amounts(1.0, 1.0, 1.0, 0.0));
        let (at, _) = m.place(SIXTEENTH + SIXTEENTH / 2.0 - 1.0, 0.5, 1.0);
        assert!((at - (SIXTEENTH + SIXTEENTH / 2.0 - 1.0)).abs() <= m.reach());
    }

    #[test]
    fn a_map_with_no_settings_moves_nothing() {
        let m = GrooveMap::neutral();
        assert_eq!(m.place(123.0, 0.4, 0.7), (123.0, 0.4));
        assert_eq!(m.reach(), 0.0);
    }
}
