//! Capture MIDI: turning what was just played into a clip.
//!
//! The session keeps every note played on armed and monitored tracks, with the time it was played.
//! When Capture is pressed those notes are read back and a phrase is looked for. In an empty,
//! stopped Set the tempo comes from the notes; otherwise the Set's tempo stands.
//!
//! Everything played is kept in the clip. What falls before the detected phrase ends up before
//! the clip's start, so the markers can be moved back to it later.

use rlive_core::{MidiMessage, TimeSignature, PPQ};
use rlive_project::NoteData;
use std::collections::VecDeque;

/// How far back the buffer reaches, in seconds.
pub const MAX_AGE: f64 = 180.0;
/// The most messages kept for one track.
const MAX_MESSAGES: usize = 20_000;
/// Tempos Capture will settle on when it has to find one (manual p. 423).
pub const MIN_BPM: f64 = 80.0;
pub const MAX_BPM: f64 = 160.0;
/// Phrases in bars that Capture looks for, longest last.
const PHRASE_BARS: [u32; 4] = [1, 2, 4, 8];
/// How close a note has to be to a grid line, in sixteenths, to count as on it.
const GRID_TOLERANCE: f64 = 0.2;

/// One played message and when.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Played {
    pub seconds: f64,
    pub tick: Option<f64>,
    pub message: MidiMessage,
}

/// Recent notes of one track.
#[derive(Debug, Clone, Default)]
pub struct CaptureBuffer {
    messages: VecDeque<Played>,
}

impl CaptureBuffer {
    pub fn push(&mut self, played: Played) {
        self.messages.push_back(played);
        let oldest = played.seconds - MAX_AGE;
        while self.messages.len() > MAX_MESSAGES
            || self.messages.front().is_some_and(|p| p.seconds < oldest)
        {
            self.messages.pop_front();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// The notes played, in the order they started, with their durations.
    pub fn notes(&self) -> Vec<PlayedNote> {
        let mut open: Vec<(u8, usize)> = Vec::new();
        let mut notes: Vec<PlayedNote> = Vec::new();
        for p in &self.messages {
            match p.message {
                MidiMessage::NoteOn { key, velocity, .. } if velocity > 0.0 => {
                    notes.push(PlayedNote {
                        seconds: p.seconds,
                        tick: p.tick,
                        duration: 0.0,
                        key,
                        velocity,
                    });
                    open.push((key, notes.len() - 1));
                }
                MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                    if let Some(at) = open.iter().rposition(|(k, _)| *k == key) {
                        let (_, i) = open.remove(at);
                        notes[i].duration = p.seconds - notes[i].seconds;
                    }
                }
                _ => {}
            }
        }
        // A note still held counts as ending now, as far as phrases go.
        if let Some(end) = self.messages.back().map(|p| p.seconds) {
            for (_, i) in open {
                notes[i].duration = (end - notes[i].seconds).max(0.0);
            }
        }
        notes
    }
}

/// A note as it was played.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayedNote {
    pub seconds: f64,
    pub tick: Option<f64>,
    pub duration: f64,
    pub key: u8,
    pub velocity: f32,
}

/// What Capture found. Notes are in ticks from the bar before the first one played.
#[derive(Debug, Clone, PartialEq)]
pub struct Phrase {
    pub tempo: f64,
    pub notes: Vec<NoteData>,
    /// The loop, in the notes' ticks.
    pub loop_start: u64,
    pub length: u64,
}

fn bpm_in_range(b: f64) -> bool {
    (MIN_BPM..=MAX_BPM).contains(&b)
}

/// Writes `notes` into clip ticks: `origin` seconds is tick 0, at `bpm`.
fn to_ticks(notes: &[PlayedNote], origin: f64, bpm: f64) -> Vec<NoteData> {
    let per_sec = bpm / 60.0 * PPQ as f64;
    let mut out: Vec<NoteData> = notes
        .iter()
        .map(|n| {
            let start = ((n.seconds - origin) * per_sec).round().max(0.0) as u64;
            let length = (n.duration * per_sec).round().max(1.0) as u64;
            NoteData::new(start, length, n.key, n.velocity)
        })
        .collect();
    out.sort_by_key(|n| n.start);
    out
}

/// How well the onsets of `notes` sit on a sixteenth grid at `bpm`, counted from `origin`. 1 is
/// all of them dead on.
fn grid_fit(notes: &[PlayedNote], origin: f64, bpm: f64) -> f64 {
    if notes.is_empty() {
        return 0.0;
    }
    let sixteenth = 60.0 / bpm / 4.0;
    let on: usize = notes
        .iter()
        .filter(|n| {
            let x = (n.seconds - origin) / sixteenth;
            (x - x.round()).abs() <= GRID_TOLERANCE
        })
        .count();
    on as f64 / notes.len() as f64
}

/// Finds the tempo and phrase of `notes` in a Set with neither (manual p. 423). The player is
/// expected to end on the first beat of the next bar, so a last note on a bar line closes the
/// phrase rather than belonging to it.
pub fn detect_phrase(notes: &[PlayedNote], sig: TimeSignature) -> Option<Phrase> {
    let first = notes.first()?;
    let last = notes.last()?;
    let beats_per_bar = sig.ticks_per_bar() as f64 / PPQ as f64;
    if notes.len() == 1 {
        // A lone note is the loop: its length decides the tempo.
        let duration = first.duration.max(0.01);
        let bars = PHRASE_BARS
            .iter()
            .copied()
            .find(|b| bpm_in_range(60.0 * *b as f64 * beats_per_bar / duration))
            .unwrap_or(1);
        let bpm = (60.0 * bars as f64 * beats_per_bar / duration).clamp(MIN_BPM, MAX_BPM);
        let length = bars as u64 * sig.ticks_per_bar();
        let mut note = NoteData::new(0, length, first.key, first.velocity);
        note.length = length;
        return Some(Phrase {
            tempo: bpm,
            notes: vec![note],
            loop_start: 0,
            length,
        });
    }
    // Every note could start the phrase, and the last one closes it. Take the reading that
    // explains the most notes, then the best on the grid, then the most ordinary tempo.
    let mut best: Option<(f64, f64, usize, u32)> = None;
    for (i, start) in notes[..notes.len() - 1].iter().enumerate() {
        let span = last.seconds - start.seconds;
        if span <= 0.0 {
            continue;
        }
        for bars in PHRASE_BARS {
            let bpm = 60.0 * bars as f64 * beats_per_bar / span;
            if !bpm_in_range(bpm) {
                continue;
            }
            let inside = &notes[i..];
            let fit = grid_fit(inside, start.seconds, bpm);
            let score = fit - (bpm - 120.0).abs() / 1000.0;
            let better = match best {
                None => true,
                Some((old_score, _, old_i, _)) => {
                    i < old_i && fit >= 0.75 || i == old_i && score > old_score
                }
            };
            if fit >= 0.75 && better {
                best = Some((score, bpm, i, bars));
            }
        }
    }
    let (_, bpm, start_at, bars) = match best {
        Some(b) => b,
        None => scan_tempo(notes, beats_per_bar),
    };
    let origin = notes[start_at].seconds;
    let bar_ticks = sig.ticks_per_bar();
    let length = bars as u64 * bar_ticks;
    // Bars of room for what came before, so the notes before the phrase have somewhere to go.
    let per_sec = bpm / 60.0 * PPQ as f64;
    let before = ((origin - first.seconds) * per_sec / bar_ticks as f64).ceil() as u64;
    let loop_start = before * bar_ticks;
    let mut placed = to_ticks(
        notes,
        origin - before as f64 * bar_ticks as f64 / per_sec,
        bpm,
    );
    if placed.len() > 1
        && placed
            .last()
            .is_some_and(|n| n.start >= loop_start + length)
    {
        placed.pop();
    }
    for n in &mut placed {
        n.start = quantize_grid(n.start);
    }
    Some(Phrase {
        tempo: bpm,
        notes: placed,
        loop_start,
        length,
    })
}

/// A tempo for notes that didn't close a bar: whatever puts most of them on the grid, with the
/// phrase rounded out to whole bars.
fn scan_tempo(notes: &[PlayedNote], beats_per_bar: f64) -> (f64, f64, usize, u32) {
    let origin = notes[0].seconds;
    let (bpm, _) = (0..=((MAX_BPM - MIN_BPM) * 2.0) as usize)
        .map(|i| MIN_BPM + i as f64 / 2.0)
        .map(|b| (b, grid_fit(notes, origin, b) - (b - 120.0).abs() / 2000.0))
        .fold((120.0, -1.0), |best, c| if c.1 > best.1 { c } else { best });
    let bar_secs = 60.0 / bpm * beats_per_bar;
    let span = notes.last().map_or(0.0, |n| n.seconds + n.duration) - origin;
    let bars = (span / bar_secs).ceil().max(1.0) as u32;
    let bars = PHRASE_BARS
        .iter()
        .copied()
        .find(|b| *b >= bars)
        .unwrap_or(8);
    (0.0, bpm, 0, bars)
}

/// Snaps a tick to the nearest sixteenth.
fn quantize_grid(tick: u64) -> u64 {
    let sixteenth = PPQ / 4;
    (tick + sixteenth / 2) / sixteenth * sixteenth
}

/// A phrase from notes played against the Set's tempo: the shortest stretch of whole bars,
/// ending at the bar line after the last note, that holds the last run of notes.
pub fn phrase_with_tempo(
    notes: &[PlayedNote],
    tempo: f64,
    sig: TimeSignature,
    tick_of: impl Fn(&PlayedNote) -> f64,
) -> Option<Phrase> {
    let last = notes.last()?;
    let bar = sig.ticks_per_bar() as f64;
    let end_tick = tick_of(last) + last.duration * tempo / 60.0 * PPQ as f64;
    let end = (end_tick / bar).ceil().max(1.0) * bar;
    // The run of notes the phrase is about: back to the first gap of over a bar.
    let mut start = tick_of(last);
    for n in notes.iter().rev() {
        let t = tick_of(n);
        if start - t > bar {
            break;
        }
        start = t;
    }
    let bars_needed = ((end - start) / bar).ceil().max(1.0) as u32;
    let bars = PHRASE_BARS
        .iter()
        .copied()
        .find(|b| *b >= bars_needed)
        .unwrap_or(8);
    let length = bars as f64 * bar;
    let first_tick = notes.first().map_or(start, &tick_of).min(end - length);
    let origin = (first_tick / bar).floor().max(0.0) * bar;
    let loop_start = (end - length - origin).max(0.0) as u64;
    let placed = notes
        .iter()
        .map(|n| {
            let start = (tick_of(n) - origin).round().max(0.0) as u64;
            let length = (n.duration * tempo / 60.0 * PPQ as f64).round().max(1.0) as u64;
            NoteData::new(start, length, n.key, n.velocity)
        })
        .collect();
    Some(Phrase {
        tempo,
        notes: placed,
        loop_start,
        length: length as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn four_four() -> TimeSignature {
        TimeSignature::default()
    }

    /// Notes at these beats, played at `bpm`, each `length` beats long.
    fn played(beats: &[f64], bpm: f64, length: f64) -> Vec<PlayedNote> {
        let beat = 60.0 / bpm;
        beats
            .iter()
            .map(|b| PlayedNote {
                seconds: 5.0 + b * beat,
                tick: None,
                duration: length * beat,
                key: 60,
                velocity: 0.8,
            })
            .collect()
    }

    #[test]
    fn a_two_bar_phrase_ending_on_the_next_downbeat_finds_its_tempo() {
        let beats: Vec<f64> = (0..=8).map(|b| b as f64).collect();
        for bpm in [85.0, 100.0, 130.0, 155.0] {
            let p = detect_phrase(&played(&beats, bpm, 0.5), four_four()).unwrap();
            assert!((p.tempo - bpm).abs() < 0.5, "{bpm}: got {}", p.tempo);
            assert_eq!(p.length, 2 * four_four().ticks_per_bar());
            assert_eq!(p.loop_start, 0);
            assert_eq!(p.notes.len(), 8, "the closing downbeat is not kept");
            assert_eq!(p.notes[4].start, 4 * PPQ);
        }
    }

    #[test]
    fn eighth_notes_land_on_the_grid() {
        let beats: Vec<f64> = (0..=8).map(|b| b as f64 * 0.5).collect();
        let p = detect_phrase(&played(&beats, 110.0, 0.25), four_four()).unwrap();
        assert!((p.tempo - 110.0).abs() < 0.5);
        assert_eq!(p.length, four_four().ticks_per_bar());
        assert!(p.notes.iter().all(|n| n.start % (PPQ / 2) == 0));
    }

    #[test]
    fn notes_before_the_phrase_stay_in_the_clip_before_its_start() {
        // A warm-up note six and a half beats before the phrase proper.
        let mut beats = vec![0.0];
        beats.extend((0..=8).map(|b| 6.5 + b as f64));
        let p = detect_phrase(&played(&beats, 100.0, 0.5), four_four()).unwrap();
        assert!((p.tempo - 100.0).abs() < 1.0, "got {}", p.tempo);
        assert!(p.loop_start > 0, "the warm-up comes before the start");
        assert_eq!(p.notes.len(), 9);
        assert!(p.notes[0].start < p.loop_start);
        assert_eq!(p.notes[1].start, p.loop_start);
    }

    #[test]
    fn one_note_is_the_whole_loop() {
        // Held two seconds: one bar of 4/4 at 120.
        let p = detect_phrase(&played(&[0.0], 120.0, 4.0), four_four()).unwrap();
        assert!((p.tempo - 120.0).abs() < 1e-6);
        assert_eq!(p.length, four_four().ticks_per_bar());
        assert_eq!(p.notes.len(), 1);
        assert_eq!(p.notes[0].length, p.length);
        // Held four seconds: two bars at 120 rather than one at 60.
        let p = detect_phrase(&played(&[0.0], 120.0, 8.0), four_four()).unwrap();
        assert!((p.tempo - 120.0).abs() < 1e-6);
        assert_eq!(p.length, 2 * four_four().ticks_per_bar());
    }

    #[test]
    fn with_a_set_tempo_the_phrase_is_whole_bars_up_to_the_next_line() {
        let notes = played(&[0.0, 1.0, 2.0, 3.5], 120.0, 0.25);
        let tick = |n: &PlayedNote| (n.seconds - 5.0) * 2.0 * PPQ as f64 + 16.0 * PPQ as f64;
        let p = phrase_with_tempo(&notes, 120.0, four_four(), tick).unwrap();
        assert_eq!(p.length, four_four().ticks_per_bar());
        assert_eq!(p.notes[1].start, PPQ);
        assert_eq!(p.tempo, 120.0);
    }

    #[test]
    fn the_buffer_reports_durations_and_forgets_what_is_too_old() {
        let on = MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0.5,
            note_id: 0,
            tuning: 0.0,
        };
        let off = MidiMessage::NoteOff {
            channel: 0,
            key: 60,
            velocity: 0.5,
            note_id: 0,
        };
        let mut b = CaptureBuffer::default();
        let mut push = |seconds, message| {
            b.push(Played {
                seconds,
                tick: None,
                message,
            })
        };
        push(1.0, on);
        push(1.5, off);
        assert_eq!(b.notes()[0].duration, 0.5);
        b.push(Played {
            seconds: 500.0,
            tick: None,
            message: on,
        });
        assert_eq!(b.notes().len(), 1, "the old one is gone");
    }
}
