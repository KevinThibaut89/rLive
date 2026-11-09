//! Which notes are found and selected: the Find and Select filters and keyboard selection (manual
//! pp. 259-262).

use crate::clip_edit::{in_scale, PitchScale};
use crate::NoteData;
use rlive_core::PPQ;

/// One of the filters. Time and lengths are in ticks; velocity and chance are 0..1.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// Pitch classes, bit 0 = C, in any octave.
    Pitch(u16),
    Time {
        start: u64,
        length: u64,
        /// Zero doesn't repeat.
        repeat: u64,
    },
    Chance {
        min: f32,
        max: f32,
    },
    Condition(Condition),
    Count {
        every: usize,
        offset: usize,
        /// Groups notes by the grid; otherwise a chord counts as one.
        quantized: bool,
    },
    Duration {
        min: u64,
        max: u64,
    },
    Scale(PitchScale),
    Velocity {
        min: f32,
        max: f32,
    },
}

/// Which notes the Condition filter finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Active,
    Chance,
    VelocityDeviation,
}

/// One filter in use.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveFilter {
    pub filter: Filter,
    pub invert: bool,
}

/// Whether a note passes a filter that looks at one note at a time.
fn passes(n: &NoteData, f: &Filter) -> bool {
    match f {
        Filter::Pitch(mask) => mask & (1 << (n.key % 12)) != 0,
        Filter::Time {
            start,
            length,
            repeat,
        } => {
            let t = n.start;
            if t < *start {
                return false;
            }
            let from = t - *start;
            if *repeat > 0 {
                from % *repeat < *length
            } else {
                from < *length
            }
        }
        Filter::Chance { min, max } => n.probability >= *min && n.probability <= *max,
        Filter::Condition(Condition::Active) => !n.muted,
        Filter::Condition(Condition::Chance) => n.probability < 1.0,
        Filter::Condition(Condition::VelocityDeviation) => n.vel_range != 0.0,
        Filter::Duration { min, max } => n.length >= *min && n.length <= *max,
        Filter::Scale(scale) => in_scale(n.key as i32, *scale),
        Filter::Velocity { min, max } => n.velocity >= *min && n.velocity <= *max,
        Filter::Count { .. } => true,
    }
}

/// Every nth note or chord: `every`, counted from `offset`. Quantized, notes in the same grid
/// step count as one (manual p. 261).
fn counted(notes: &[NoteData], every: usize, offset: usize, group: u64) -> Vec<bool> {
    let group = group.max(1);
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by_key(|i| (notes[*i].start, notes[*i].key));
    let mut picked = vec![false; notes.len()];
    let every = every.max(1);
    let mut step = 0;
    let mut last = None;
    for i in order {
        let slot = notes[i].start / group;
        if last.is_some_and(|l| l != slot) {
            step += 1;
        }
        last = Some(slot);
        picked[i] = step >= offset && (step - offset).is_multiple_of(every);
    }
    picked
}

/// The notes that meet every filter, in order. An inverted filter finds what it wouldn't.
pub fn find(notes: &[NoteData], filters: &[ActiveFilter], grid: u64) -> Vec<usize> {
    let mut hit = vec![true; notes.len()];
    for active in filters {
        let found: Vec<bool> = match active.filter {
            Filter::Count {
                every,
                offset,
                quantized,
            } => counted(notes, every, offset, if quantized { grid } else { 1 }),
            _ => notes.iter().map(|n| passes(n, &active.filter)).collect(),
        };
        for (h, f) in hit.iter_mut().zip(found) {
            *h &= f != active.invert;
        }
    }
    hit.iter()
        .enumerate()
        .filter(|(_, h)| **h)
        .map(|(i, _)| i)
        .collect()
}

/// What Invert Selection selects.
pub fn inverted(count: usize, selected: &[usize]) -> Vec<usize> {
    (0..count).filter(|i| !selected.contains(i)).collect()
}

/// Shift-clicking a piano key: the notes of that key track join the selection, or leave it if
/// they were all in it (manual p. 260).
pub fn toggle_key_track(notes: &[NoteData], selected: &mut Vec<usize>, key: u8) {
    let track: Vec<usize> = notes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.key == key)
        .map(|(i, _)| i)
        .collect();
    if !track.is_empty() && track.iter().all(|i| selected.contains(i)) {
        selected.retain(|i| !track.contains(i));
    } else {
        for i in track {
            if !selected.contains(&i) {
                selected.push(i);
            }
        }
    }
}

/// The notes a dragged rectangle encloses: any part of them within `[t0, t1)` and `k0..=k1`.
pub fn notes_in_box(notes: &[NoteData], t0: u64, t1: u64, k0: u8, k1: u8) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.start < t1 && n.start + n.length > t0 && n.key >= k0 && n.key <= k1)
        .map(|(i, _)| i)
        .collect()
}

/// Notes in time order: starts, then pitches.
fn in_time_order(notes: &[NoteData]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by_key(|i| (notes[*i].start, notes[*i].key));
    order
}

/// The note nearest the insert marker.
pub fn nearest_note(notes: &[NoteData], at: u64) -> Option<usize> {
    in_time_order(notes)
        .into_iter()
        .min_by_key(|i| notes[*i].start.abs_diff(at))
}

/// The next or previous note in time.
pub fn step_in_time(notes: &[NoteData], from: usize, forward: bool) -> Option<usize> {
    let order = in_time_order(notes);
    let at = order.iter().position(|i| *i == from)?;
    let to = if forward { at + 1 } else { at.checked_sub(1)? };
    order.get(to).copied()
}

/// The next or previous note on the same key.
pub fn step_in_key(notes: &[NoteData], from: usize, forward: bool) -> Option<usize> {
    let key = notes.get(from)?.key;
    let order: Vec<usize> = in_time_order(notes)
        .into_iter()
        .filter(|i| notes[*i].key == key)
        .collect();
    let at = order.iter().position(|i| *i == from)?;
    let to = if forward { at + 1 } else { at.checked_sub(1)? };
    order.get(to).copied()
}

/// The next note edge after `at`, or the previous one before it.
pub fn note_boundary(notes: &[NoteData], at: u64, forward: bool) -> Option<u64> {
    let edges = notes.iter().flat_map(|n| [n.start, n.start + n.length]);
    if forward {
        edges.filter(|e| *e > at).min()
    } else {
        edges.filter(|e| *e < at).max()
    }
}

/// The Time filter's settings for a dragged repeated time selection: the drag, and as much gap as
/// it is long (manual p. 262).
pub fn repeated_time(from: u64, to: u64) -> Filter {
    let length = to.saturating_sub(from).max(PPQ / 32);
    Filter::Time {
        start: from,
        length,
        repeat: 2 * length,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: u64 = PPQ / 4;

    fn note(start: u64, key: u8) -> NoteData {
        NoteData::new(start, GRID, key, 0.5)
    }

    fn select(notes: &[NoteData], f: Filter) -> Vec<usize> {
        find(
            notes,
            &[ActiveFilter {
                filter: f,
                invert: false,
            }],
            GRID,
        )
    }

    #[test]
    fn the_pitch_filter_finds_every_octave() {
        let n = vec![note(0, 60), note(1, 72), note(2, 62)];
        assert_eq!(select(&n, Filter::Pitch(1)), vec![0, 1]);
    }

    #[test]
    fn the_time_filter_can_repeat() {
        let n = vec![
            note(0, 60),
            note(PPQ, 60),
            note(2 * PPQ, 60),
            note(3 * PPQ, 60),
        ];
        let f = Filter::Time {
            start: 0,
            length: PPQ,
            repeat: 0,
        };
        assert_eq!(select(&n, f), vec![0]);
        let f = Filter::Time {
            start: 0,
            length: PPQ,
            repeat: 2 * PPQ,
        };
        assert_eq!(select(&n, f), vec![0, 2]);
    }

    #[test]
    fn chance_condition_duration_and_velocity_filter_by_their_values() {
        let mut n = vec![note(0, 60), note(PPQ, 62), note(2 * PPQ, 64)];
        n[0].probability = 0.5;
        n[1].vel_range = 0.2;
        n[2].muted = true;
        n[2].length = 3 * PPQ;
        n[1].velocity = 0.9;
        assert_eq!(select(&n, Filter::Chance { min: 0.0, max: 0.6 }), vec![0]);
        assert_eq!(select(&n, Filter::Condition(Condition::Chance)), vec![0]);
        assert_eq!(
            select(&n, Filter::Condition(Condition::VelocityDeviation)),
            vec![1]
        );
        assert_eq!(select(&n, Filter::Condition(Condition::Active)), vec![0, 1]);
        assert_eq!(
            select(
                &n,
                Filter::Duration {
                    min: PPQ,
                    max: u64::MAX
                }
            ),
            vec![2]
        );
        assert_eq!(select(&n, Filter::Velocity { min: 0.8, max: 1.0 }), vec![1]);
    }

    #[test]
    fn count_takes_every_nth_note_or_chord() {
        let n = vec![note(0, 60), note(0, 64), note(PPQ, 60), note(2 * PPQ, 60)];
        let f = Filter::Count {
            every: 2,
            offset: 0,
            quantized: false,
        };
        assert_eq!(select(&n, f), vec![0, 1, 3], "a chord counts as one");
        let f = Filter::Count {
            every: 2,
            offset: 1,
            quantized: false,
        };
        assert_eq!(select(&n, f), vec![2], "offset moves the first");
    }

    #[test]
    fn a_quantized_count_groups_by_the_grid() {
        let n = vec![
            note(0, 60),
            note(10, 62),
            note(GRID, 60),
            note(2 * GRID, 60),
        ];
        let f = Filter::Count {
            every: 2,
            offset: 0,
            quantized: true,
        };
        assert_eq!(select(&n, f), vec![0, 1, 3]);
    }

    #[test]
    fn the_scale_filter_uses_root_and_mask() {
        let n = vec![note(0, 60), note(1, 61)];
        assert_eq!(select(&n, Filter::Scale((0, 0b1010_1011_0101))), vec![0]);
    }

    #[test]
    fn filters_combine_and_invert() {
        let n = vec![note(0, 60), note(PPQ, 60), note(PPQ, 62)];
        let time = ActiveFilter {
            filter: Filter::Time {
                start: PPQ,
                length: PPQ,
                repeat: 0,
            },
            invert: false,
        };
        let pitch = ActiveFilter {
            filter: Filter::Pitch(1),
            invert: false,
        };
        assert_eq!(find(&n, &[time.clone(), pitch.clone()], GRID), vec![1]);
        let not_c = ActiveFilter {
            invert: true,
            ..pitch
        };
        assert_eq!(find(&n, &[time, not_c], GRID), vec![2]);
    }

    #[test]
    fn invert_selection_swaps_selected_and_not() {
        assert_eq!(inverted(4, &[1, 2]), vec![0, 3]);
    }

    #[test]
    fn shift_clicking_a_key_adds_then_removes_its_track() {
        let n = vec![note(0, 60), note(1, 60), note(2, 62)];
        let mut s = vec![2];
        toggle_key_track(&n, &mut s, 60);
        assert_eq!(s, vec![2, 0, 1]);
        toggle_key_track(&n, &mut s, 60);
        assert_eq!(s, vec![2]);
    }

    #[test]
    fn a_box_selects_what_it_touches() {
        let n = vec![note(0, 60), note(PPQ, 62), note(2 * PPQ, 70)];
        assert_eq!(notes_in_box(&n, 10, PPQ + 1, 60, 64), vec![0, 1]);
    }

    #[test]
    fn keyboard_selection_walks_through_time_and_key_tracks() {
        let n = vec![note(PPQ, 62), note(0, 60), note(2 * PPQ, 60)];
        assert_eq!(nearest_note(&n, 10), Some(1));
        assert_eq!(step_in_time(&n, 1, true), Some(0));
        assert_eq!(step_in_time(&n, 1, false), None);
        assert_eq!(step_in_key(&n, 1, true), Some(2), "skips the other key");
    }

    #[test]
    fn note_boundaries_are_the_nearest_edges() {
        let n = vec![note(PPQ, 60)];
        assert_eq!(note_boundary(&n, 0, true), Some(PPQ));
        assert_eq!(note_boundary(&n, PPQ, true), Some(PPQ + GRID));
        assert_eq!(note_boundary(&n, PPQ, false), None);
    }

    #[test]
    fn a_repeated_time_selection_leaves_as_much_gap() {
        assert_eq!(
            repeated_time(PPQ, 2 * PPQ),
            Filter::Time {
                start: PPQ,
                length: PPQ,
                repeat: 2 * PPQ
            }
        );
    }
}
