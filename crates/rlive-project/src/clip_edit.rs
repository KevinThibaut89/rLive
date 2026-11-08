//! Edits of a clip's markers and notes, as pure functions (manual ch. 8).
//!
//! Nothing here knows about the engine or the undo history: the session runs these on a copy of a
//! clip, keeps the result and tells the engine.

use crate::{ArrangementClip, Clip, ClipContent, MidiProgram, NoteData, Warp, WarpMarker};
use rlive_core::{ClipRegion, PPQ};

/// A chooser in a clip's bank and program controls set to "—".
pub const PROGRAM_UNSET: u8 = 0xff;

/// The shortest a loop can be, in ticks.
pub const MIN_LOOP: u64 = 1;

impl MidiProgram {
    pub const UNSET: MidiProgram = MidiProgram {
        bank: PROGRAM_UNSET,
        sub_bank: PROGRAM_UNSET,
        program: PROGRAM_UNSET,
    };
}

// ------------------------------------------------------------- markers

/// Moves the start marker. An unlooped clip keeps something between its markers.
pub fn set_start(c: &mut Clip, at: u64) {
    c.start = if c.looped {
        at.min(c.loop_end().saturating_sub(1))
    } else {
        at.min(c.end_marker().saturating_sub(1))
    };
}

/// Moves the end marker, which can't reach the start marker.
pub fn set_end(c: &mut Clip, at: u64) {
    let at = at.max(c.start + 1);
    c.end = (at != c.loop_end()).then_some(at);
}

/// Moves the start marker and the end marker together.
pub fn shift_region(c: &mut Clip, by: i64) {
    let end = c.end_marker();
    let by = by.max(-(c.start as i64));
    c.start = (c.start as i64 + by) as u64;
    c.end = Some((end as i64 + by) as u64).filter(|e| *e != c.loop_end());
}

/// Whether the brace is what sets this clip's region: it's unlooped and its markers sit on the
/// brace's ends, as a loop recording's do.
fn brace_is_region(c: &Clip) -> bool {
    !c.looped && c.start == c.loop_start && c.end.is_none()
}

/// Moves the loop brace to start at `at`, keeping its length.
pub fn set_loop_position(c: &mut Clip, at: u64) {
    if brace_is_region(c) {
        c.start = at;
    }
    c.loop_start = at;
}

/// Moves the end of the loop to `at`.
pub fn set_loop_end(c: &mut Clip, at: u64) {
    c.length = at.saturating_sub(c.loop_start).max(MIN_LOOP);
}

pub fn set_loop_length(c: &mut Clip, length: u64) {
    c.length = length.max(MIN_LOOP);
}

/// Nudges the brace, or shifts it in steps of its own length.
pub fn move_loop(c: &mut Clip, by: i64) {
    let at = (c.loop_start as i64 + by).max(0) as u64;
    set_loop_position(c, at);
}

/// Shortens or lengthens the loop by `by`.
pub fn resize_loop(c: &mut Clip, by: i64) {
    c.length = (c.length as i64 + by).max(MIN_LOOP as i64) as u64;
}

/// Doubles or halves the loop's length: `factor` is 2 or 0.5.
pub fn scale_loop(c: &mut Clip, factor: f64) {
    c.length = ((c.length as f64 * factor).round() as u64).max(MIN_LOOP);
}

/// Doubles the loop with what's in it. Notes after the loop are moved along so they stay where
/// they were relative to its end (manual p. 229).
pub fn duplicate_loop(c: &mut Clip) {
    let (from, to, length) = (c.loop_start, c.loop_end(), c.length);
    if let Some(notes) = c.notes_mut() {
        let mut copies = Vec::new();
        for n in notes.iter_mut() {
            if n.start >= to {
                n.start += length;
            } else if n.start >= from {
                copies.push(NoteData {
                    start: n.start + length,
                    ..n.clone()
                });
            }
        }
        notes.extend(copies);
    }
    for e in c.envelopes.iter_mut().filter(|e| e.loop_region.is_none()) {
        let (from, to, length) = (from as f64, to as f64, length as f64);
        let mut copies = Vec::new();
        for p in e.envelope.points.iter_mut() {
            if p.time >= to {
                p.time += length;
            } else if p.time >= from {
                let mut copy = *p;
                copy.time += length;
                copies.push(copy);
            }
        }
        e.envelope.points.extend(copies);
        e.envelope.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    }
    c.length = length * 2;
}

/// The range a crop keeps: unlooped, between the markers; looped, from whichever of the start
/// marker and loop start comes first to the end of the loop (manual p. 231).
pub fn crop_range(c: &Clip) -> (u64, u64) {
    if c.looped {
        (c.start.min(c.loop_start), c.loop_end())
    } else {
        (c.start, c.end_marker())
    }
}

/// Moves everything about a clip's time left by `by`, after a crop has taken that much off.
fn shift_markers_left(c: &mut Clip, by: u64) {
    c.start -= by.min(c.start);
    c.loop_start -= by.min(c.loop_start);
    c.end = c.end.map(|e| e.saturating_sub(by));
    let shift = by as f64;
    for e in c.envelopes.iter_mut().filter(|e| e.loop_region.is_none()) {
        e.envelope.points.retain(|p| p.time >= shift);
        for p in &mut e.envelope.points {
            p.time -= shift;
        }
    }
}

/// Crops a MIDI clip to `[from, to)`. Notes that begin inside it survive, cut at its end. The
/// markers are carried along so the clip still plays the same notes.
pub fn crop_midi(c: &mut Clip, from: u64, to: u64) {
    if let Some(notes) = c.notes_mut() {
        notes.retain(|n| n.start >= from && n.start < to);
        for n in notes.iter_mut() {
            n.length = n.length.min(to - n.start);
            n.start -= from;
        }
    }
    shift_markers_left(c, from);
    c.length = c.length.min(
        to.saturating_sub(from)
            .saturating_sub(c.loop_start)
            .max(MIN_LOOP),
    );
    if let Some(end) = c.end {
        c.end = Some(end.min(to - from));
    }
}

/// The markers of an audio clip after cropping its file to `[from, to)` ticks. `seconds` is where
/// `from` falls in the old file, and `new_seconds` the new file's length.
pub fn crop_audio_markers(c: &mut Clip, from: u64, to: u64, seconds: f64, new_seconds: f64) {
    if let ClipContent::Audio { warp, .. } = &mut c.content {
        crop_warp(warp, from as f64, to as f64, seconds, new_seconds);
    }
    shift_markers_left(c, from);
    c.length = c.length.min(
        to.saturating_sub(from)
            .saturating_sub(c.loop_start)
            .max(MIN_LOOP),
    );
    if let Some(end) = c.end {
        c.end = Some(end.min(to - from));
    }
}

fn crop_warp(warp: &mut Warp, from: f64, to: f64, seconds: f64, new_seconds: f64) {
    if !warp.enabled || warp.markers.len() < 2 {
        return;
    }
    let mut markers: Vec<WarpMarker> = warp
        .markers
        .iter()
        .filter(|m| m.tick > from && m.tick < to)
        .map(|m| WarpMarker {
            seconds: m.seconds - seconds,
            tick: m.tick - from,
        })
        .collect();
    markers.insert(
        0,
        WarpMarker {
            seconds: 0.0,
            tick: 0.0,
        },
    );
    markers.push(WarpMarker {
        seconds: new_seconds,
        tick: to - from,
    });
    warp.markers = markers;
}

/// Flips a clip's markers end for end about `axis` ticks, for a reversed sample. Clip envelopes
/// stay where they are in time (manual p. 217).
pub fn flip_region(c: &mut Clip, axis: f64) {
    let flip = |t: f64| (axis - t).round().max(0.0) as u64;
    let region: ClipRegion = c.region();
    let (old_start, old_end) = (region.start, region.content_end());
    let new_loop = flip(region.loop_end());
    let new_start = flip(old_end);
    let new_end = flip(old_start);
    c.loop_start = new_loop;
    c.start = new_start;
    c.end = (!c.looped && new_end != c.loop_end()).then_some(new_end);
}

/// A timeline clip's offset, kept with its start marker after the marker moved from `old`.
pub fn follow_start(a: &mut ArrangementClip, old_start: u64) {
    let trimmed = a.offset.saturating_sub(old_start);
    a.offset = a.clip.start + trimmed;
}

/// Warp Markers stay on the audio they were on, so a marker on the second bar ends up on the
/// second-to-last (manual p. 217).
pub fn flip_warp(warp: &mut Warp, seconds: f64, axis: f64) {
    let mut markers: Vec<WarpMarker> = warp
        .markers
        .iter()
        .map(|m| WarpMarker {
            seconds: seconds - m.seconds,
            tick: axis - m.tick,
        })
        .collect();
    markers.sort_by(|a, b| a.tick.total_cmp(&b.tick));
    warp.markers = markers;
    let (added, removed) = (&mut warp.transients.added, &mut warp.transients.removed);
    for list in [added, removed] {
        for t in list.iter_mut() {
            *t = seconds - *t;
        }
    }
}

/// What a set of values has in common: its lowest and highest. `None` if there are no values.
pub fn merged<T: PartialOrd + Copy>(values: impl IntoIterator<Item = T>) -> Option<(T, T)> {
    values.into_iter().fold(None, |range, v| match range {
        None => Some((v, v)),
        Some((lo, hi)) => Some((if v < lo { v } else { lo }, if v > hi { v } else { hi })),
    })
}

// ------------------------------------------------------------- notes

/// The notes a tool works on: the selected ones, or all of them with nothing selected.
fn chosen(notes: &[NoteData], selected: &[usize]) -> Vec<usize> {
    if selected.is_empty() {
        (0..notes.len()).collect()
    } else {
        selected
            .iter()
            .copied()
            .filter(|i| *i < notes.len())
            .collect()
    }
}

/// A clip's scale for pitch tools: root, and the mask of its notes with the root as bit 0.
pub type PitchScale = (u8, u16);

fn in_scale(key: i32, (root, mask): PitchScale) -> bool {
    mask & (1 << (key - root as i32).rem_euclid(12)) != 0
}

/// `key` moved by `steps` scale degrees: its nearest note in the scale, then along the scale.
fn step_in_scale(key: u8, steps: i32, scale: PitchScale) -> u8 {
    let mut k = key as i32;
    while !in_scale(k, scale) && k > 0 {
        k -= 1;
    }
    let dir = steps.signum();
    for _ in 0..steps.abs() {
        loop {
            k += dir;
            if !(0..=127).contains(&k) || in_scale(k, scale) {
                break;
            }
        }
    }
    k.clamp(0, 127) as u8
}

fn moved(key: u8, by: i32, scale: Option<PitchScale>) -> u8 {
    match scale {
        Some(s) => step_in_scale(key, by, s),
        None => (key as i32 + by).clamp(0, 127) as u8,
    }
}

/// Transposes by semitones, or scale degrees if a scale is on.
pub fn transpose(notes: &mut [NoteData], selected: &[usize], by: i32, scale: Option<PitchScale>) {
    for i in chosen(notes, selected) {
        notes[i].key = moved(notes[i].key, by, scale);
    }
}

/// Moves each note to the closest pitch in the scale.
pub fn fit_to_scale(notes: &mut [NoteData], selected: &[usize], scale: PitchScale) {
    for i in chosen(notes, selected) {
        let key = notes[i].key as i32;
        let fitted = (0..12)
            .flat_map(|d| [key - d, key + d])
            .find(|k| (0..=127).contains(k) && in_scale(*k, scale));
        if let Some(k) = fitted {
            notes[i].key = k as u8;
        }
    }
}

/// Flips the selection upside down, so the highest note and the lowest swap.
pub fn invert(notes: &mut [NoteData], selected: &[usize]) {
    let set = chosen(notes, selected);
    let Some((lo, hi)) = merged(set.iter().map(|i| notes[*i].key)) else {
        return;
    };
    for i in set {
        notes[i].key = hi - (notes[i].key - lo);
    }
}

/// A new note at `interval` from each one chosen. The new notes are at the end of the list.
pub fn add_interval(
    notes: &mut Vec<NoteData>,
    selected: &[usize],
    interval: i32,
    scale: Option<PitchScale>,
) {
    let added: Vec<NoteData> = chosen(notes, selected)
        .into_iter()
        .map(|i| NoteData {
            key: moved(notes[i].key, interval, scale),
            ..notes[i].clone()
        })
        .collect();
    notes.extend(added);
}

/// Stretches the notes' timing and lengths around the first of them.
pub fn stretch(notes: &mut [NoteData], selected: &[usize], factor: f64) {
    let set = chosen(notes, selected);
    let Some(from) = set.iter().map(|i| notes[*i].start).min() else {
        return;
    };
    for i in set {
        let n = &mut notes[i];
        n.start = from + ((n.start - from) as f64 * factor).round() as u64;
        n.length = ((n.length as f64 * factor).round() as u64).max(1);
    }
}

/// Gives every note the same length.
pub fn set_length(notes: &mut [NoteData], selected: &[usize], length: u64) {
    for i in chosen(notes, selected) {
        notes[i].length = length.max(1);
    }
}

/// Moves each note by up to half a grid division, `amount` of the way. `random` gives -1..1.
pub fn humanize(
    notes: &mut [NoteData],
    selected: &[usize],
    amount: f64,
    grid: u64,
    mut random: impl FnMut() -> f64,
) {
    let reach = grid as f64 / 2.0 * amount.clamp(0.0, 1.0);
    for i in chosen(notes, selected) {
        let start = notes[i].start as f64 + reach * random().clamp(-1.0, 1.0);
        notes[i].start = start.round().max(0.0) as u64;
    }
}

/// Plays the selection backwards, mirrored within its own span.
pub fn reverse(notes: &mut [NoteData], selected: &[usize]) {
    let set = chosen(notes, selected);
    let Some(from) = set.iter().map(|i| notes[*i].start).min() else {
        return;
    };
    let Some(to) = set.iter().map(|i| notes[*i].start + notes[*i].length).max() else {
        return;
    };
    for i in set {
        let n = &mut notes[i];
        n.start = from + to - (n.start + n.length);
    }
}

/// Lengthens or shortens each note to reach the next one. The last note keeps its length.
pub fn legato(notes: &mut [NoteData], selected: &[usize]) {
    let mut set = chosen(notes, selected);
    set.sort_by_key(|i| notes[*i].start);
    let starts: Vec<u64> = set.iter().map(|i| notes[*i].start).collect();
    for (at, i) in set.iter().enumerate() {
        if let Some(next) = starts[at + 1..].iter().find(|s| **s > starts[at]) {
            notes[*i].length = next - starts[at];
        }
    }
}

/// Ticks in a sixteenth, the finest step a marker moves by without a grid.
pub const SIXTEENTH: u64 = PPQ / 4;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClipScale;

    const BAR: u64 = 4 * PPQ;
    const MAJOR: PitchScale = (0, 0b1010_1011_0101);

    fn clip(length: u64) -> Clip {
        Clip {
            length,
            ..Clip::default()
        }
    }

    fn note(start: u64, key: u8) -> NoteData {
        NoteData::new(start, PPQ, key, 0.8)
    }

    #[test]
    fn a_start_marker_cannot_leave_the_clip() {
        let mut c = clip(BAR);
        set_start(&mut c, BAR * 9);
        assert_eq!(c.start, BAR - 1);
        c.looped = false;
        set_end(&mut c, PPQ);
        assert_eq!(c.end_marker(), BAR, "the end marker stops at the start");
        assert_eq!(c.region().play_length(), 1.0);
    }

    #[test]
    fn duplicate_loop_doubles_the_notes_and_pushes_the_rest_along() {
        let mut c = clip(BAR);
        c.notes_mut().unwrap().extend([note(0, 60), note(BAR, 62)]);
        duplicate_loop(&mut c);
        assert_eq!(c.length, 2 * BAR);
        let mut at: Vec<(u64, u8)> = c.notes().iter().map(|n| (n.start, n.key)).collect();
        at.sort();
        assert_eq!(at, vec![(0, 60), (BAR, 60), (2 * BAR, 62)]);
    }

    #[test]
    fn crop_range_follows_the_loop_or_the_markers() {
        let mut c = clip(BAR);
        c.loop_start = 2 * BAR;
        c.start = BAR;
        assert_eq!(crop_range(&c), (BAR, 3 * BAR));
        c.looped = false;
        c.end = Some(2 * BAR);
        assert_eq!(crop_range(&c), (BAR, 2 * BAR));
    }

    #[test]
    fn a_midi_crop_keeps_what_was_in_range_and_moves_it_to_the_start() {
        let mut c = clip(BAR);
        c.start = BAR;
        c.loop_start = BAR;
        c.notes_mut().unwrap().extend([
            note(0, 40),
            note(BAR, 60),
            NoteData::new(BAR + BAR / 2, BAR * 4, 62, 0.8),
            note(2 * BAR, 70),
        ]);
        let (from, to) = crop_range(&c);
        crop_midi(&mut c, from, to);
        assert_eq!(c.start, 0);
        assert_eq!(c.loop_start, 0);
        assert_eq!(c.length, BAR);
        let kept: Vec<(u64, u8, u64)> = c
            .notes()
            .iter()
            .map(|n| (n.start, n.key, n.length))
            .collect();
        assert_eq!(kept, vec![(0, 60, PPQ), (BAR / 2, 62, BAR / 2)]);
    }

    #[test]
    fn flipping_turns_the_region_end_for_end() {
        let mut c = clip(BAR);
        c.loop_start = BAR;
        c.start = BAR;
        flip_region(&mut c, 4.0 * BAR as f64);
        assert_eq!(c.loop_start, 2 * BAR);
        assert_eq!(c.start, 2 * BAR);
        assert_eq!(c.length, BAR);
        // Twice is where it began.
        flip_region(&mut c, 4.0 * BAR as f64);
        assert_eq!((c.start, c.loop_start), (BAR, BAR));
    }

    #[test]
    fn a_warp_marker_stays_on_its_audio_when_flipped() {
        let mut w = Warp {
            enabled: true,
            markers: vec![
                WarpMarker {
                    seconds: 0.0,
                    tick: 0.0,
                },
                WarpMarker {
                    seconds: 2.0,
                    tick: BAR as f64,
                },
                WarpMarker {
                    seconds: 8.0,
                    tick: 4.0 * BAR as f64,
                },
            ],
            ..Warp::default()
        };
        flip_warp(&mut w, 8.0, 4.0 * BAR as f64);
        assert_eq!(w.markers[1].tick, 3.0 * BAR as f64);
        assert_eq!(w.markers[1].seconds, 6.0);
        assert_eq!(w.markers[0].seconds, 0.0);
    }

    #[test]
    fn a_new_arrangement_clip_starts_from_its_start_marker() {
        let mut c = clip(BAR);
        c.start = PPQ;
        assert_eq!(ArrangementClip::new(c, 0).offset, PPQ);
    }

    #[test]
    fn extending_a_timeline_clip_left_takes_its_start_marker_along() {
        let mut c = clip(BAR);
        c.looped = false;
        c.start = PPQ;
        let mut a = ArrangementClip::new(c, 2 * PPQ);
        a.trim_left(PPQ + PPQ / 2);
        assert_eq!(a.offset, PPQ / 2);
        assert_eq!(a.clip.start, PPQ / 2);
        assert_eq!(a.run_offset(), 0);
    }

    #[test]
    fn scale_pitches_step_along_the_scale() {
        let mut n = vec![note(0, 60)];
        transpose(&mut n, &[], 2, Some(MAJOR));
        assert_eq!(n[0].key, 64, "two degrees above C is E");
        fit_to_scale(&mut n, &[], (1, 0b1010_1011_0101));
        assert_eq!(n[0].key, 63, "to the nearest note of D-flat major");
        let _ = ClipScale::default();
    }

    #[test]
    fn invert_swaps_top_and_bottom_and_interval_adds_notes() {
        let mut n = vec![note(0, 60), note(1, 64), note(2, 72)];
        invert(&mut n, &[]);
        assert_eq!((n[0].key, n[1].key, n[2].key), (72, 68, 60));
        add_interval(&mut n, &[0], 3, None);
        assert_eq!(n.len(), 4);
        assert_eq!(n[3].key, 75);
    }

    #[test]
    fn the_time_tools_act_on_the_selection() {
        let mut n = vec![note(0, 60), note(PPQ, 62)];
        stretch(&mut n, &[], 2.0);
        assert_eq!(n[1].start, 2 * PPQ);
        assert_eq!(n[0].length, 2 * PPQ);
        set_length(&mut n, &[1], 7);
        assert_eq!((n[0].length, n[1].length), (2 * PPQ, 7));
        legato(&mut n, &[]);
        assert_eq!(n[0].length, 2 * PPQ, "reaches the next note");
        let mut n = vec![note(0, 60), NoteData::new(PPQ, 2 * PPQ, 62, 0.8)];
        reverse(&mut n, &[]);
        assert_eq!(n[0].start, 2 * PPQ);
        assert_eq!(n[1].start, 0);
    }

    #[test]
    fn humanize_stays_within_half_a_grid() {
        let mut n = vec![note(PPQ, 60)];
        let grid = PPQ / 4;
        humanize(&mut n, &[], 1.0, grid, || 1.0);
        assert_eq!(n[0].start, PPQ + grid / 2);
    }

    #[test]
    fn merged_is_the_range_of_what_it_was_given() {
        assert_eq!(merged([3, 1, 2]), Some((1, 3)));
        assert_eq!(merged(Vec::<i32>::new()), None);
    }
}
