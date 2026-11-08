//! Collects played MIDI into clips while recording onto the timeline.

use rlive_core::{ArrClipId, MidiMessage, TimeSignature, TrackId};
use rlive_project::{Clip, ClipContent, NoteData};
use std::collections::HashMap;

/// The shortest note a press and release can make, in ticks.
const MIN_NOTE: u64 = 10;

/// Release velocity of a note that ended with no key-up (a jump or the end of recording).
const DEFAULT_RELEASE: f32 = 0.5;

#[derive(Default)]
struct Take {
    open: HashMap<u8, (f64, f32)>,
    notes: Vec<NoteData>,
    /// The timeline clip holding this take, once it's been placed.
    clip: Option<ArrClipId>,
}

impl Take {
    fn close(&mut self, key: u8, until: f64, release: f32) {
        if let Some((start, velocity)) = self.open.remove(&key) {
            let start = start.max(0.0).round() as u64;
            let length = (until.round() as u64).saturating_sub(start).max(MIN_NOTE);
            let mut note = NoteData::new(start, length, key, velocity);
            note.release = release;
            self.notes.push(note);
        }
    }

    /// Closes held notes that had no key-up, so they get the default release velocity.
    fn close_all(&mut self, until: f64) {
        let keys: Vec<u8> = self.open.keys().copied().collect();
        for key in keys {
            self.close(key, until, DEFAULT_RELEASE);
        }
    }
}

/// One finished stretch of a take, ready to go on the timeline.
pub struct Placement {
    pub track: TrackId,
    pub existing: Option<ArrClipId>,
    pub start: u64,
    pub clip: Clip,
}

pub struct Recording {
    start: u64,
    takes: HashMap<TrackId, Take>,
}

impl Recording {
    pub fn new(start: u64) -> Self {
        Self {
            start,
            takes: HashMap::new(),
        }
    }

    pub fn message(&mut self, track: TrackId, tick: f64, message: MidiMessage) {
        let take = self.takes.entry(track).or_default();
        match message {
            MidiMessage::NoteOn { key, velocity, .. } if velocity > 0.0 => {
                take.close(key, tick, DEFAULT_RELEASE);
                take.open.insert(key, (tick, velocity));
            }
            MidiMessage::NoteOn { key, .. } => take.close(key, tick, DEFAULT_RELEASE),
            MidiMessage::NoteOff { key, velocity, .. } => take.close(key, tick, velocity),
            _ => {}
        }
    }

    /// The playhead jumped: held notes end where it left and continue where it lands.
    pub fn jumped(&mut self, from: f64, to: f64) {
        for take in self.takes.values_mut() {
            let held: Vec<(u8, f32)> = take.open.iter().map(|(k, (_, v))| (*k, *v)).collect();
            take.close_all(from);
            for (key, velocity) in held {
                take.open.insert(key, (to, velocity));
            }
        }
    }

    pub fn finish(&mut self, tick: f64) {
        for take in self.takes.values_mut() {
            take.close_all(tick);
        }
    }

    /// Remembers which timeline clip a take went into.
    pub fn set_clip(&mut self, track: TrackId, id: ArrClipId) {
        if let Some(t) = self.takes.get_mut(&track) {
            t.clip = Some(id);
        }
    }

    /// Every take so far as a clip spanning whole bars. Takes without notes are left out.
    pub fn placements(&self, sig: TimeSignature) -> Vec<Placement> {
        let bar = sig.ticks_per_bar().max(1);
        let start = self.start / bar * bar;
        self.takes
            .iter()
            .filter(|(_, t)| !t.notes.is_empty())
            .map(|(track, take)| {
                let last = take
                    .notes
                    .iter()
                    .map(|n| n.start + n.length)
                    .max()
                    .unwrap_or(start);
                let length = last.saturating_sub(start).div_ceil(bar).max(1) * bar;
                let notes = take
                    .notes
                    .iter()
                    .filter(|n| n.start >= start)
                    .map(|n| NoteData {
                        start: n.start - start,
                        ..n.clone()
                    })
                    .collect();
                Placement {
                    track: *track,
                    existing: take.clip,
                    start,
                    clip: Clip {
                        name: "Recording".into(),
                        color: [255, 255, 255],
                        length,
                        looped: false,
                        content: ClipContent::midi(notes),
                        ..Clip::default()
                    },
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(key: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 0.7,
            note_id: 0,
            tuning: 0.0,
        }
    }

    fn off(key: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity: 0.5,
            note_id: 0,
        }
    }

    fn sig() -> TimeSignature {
        TimeSignature::default()
    }

    fn notes(p: &Placement) -> &[NoteData] {
        p.clip.notes()
    }

    #[test]
    fn a_note_becomes_clip_relative_inside_whole_bars() {
        let t = TrackId::new();
        let mut r = Recording::new(4000);
        r.message(t, 4100.0, on(60));
        r.message(t, 5000.0, off(60));
        let p = &r.placements(sig())[0];
        assert_eq!(p.start, 3840);
        assert_eq!(p.clip.length, 3840);
        assert_eq!(notes(p)[0].start, 260);
        assert_eq!(notes(p)[0].length, 900);
    }

    #[test]
    fn a_key_up_sets_the_release_velocity() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, on(60));
        r.message(
            t,
            500.0,
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.2,
                note_id: 7,
            },
        );
        let p = r.placements(sig());
        assert!((notes(&p[0])[0].release - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_note_held_across_a_wrap_splits() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 3000.0, on(60));
        r.jumped(3840.0, 0.0);
        r.message(t, 500.0, off(60));
        let p = r.placements(sig());
        let mut n = notes(&p[0]).to_vec();
        n.sort_by_key(|x| x.start);
        assert_eq!((n[0].start, n[0].length), (0, 500));
        assert_eq!((n[1].start, n[1].length), (3000, 840));
    }

    #[test]
    fn notes_without_a_key_up_keep_the_default_release() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, on(61));
        r.finish(900.0);
        assert_eq!(notes(&r.placements(sig())[0])[0].release, DEFAULT_RELEASE);
    }

    #[test]
    fn finish_closes_held_notes() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, on(61));
        r.finish(900.0);
        assert_eq!(notes(&r.placements(sig())[0])[0].length, 800);
    }

    #[test]
    fn no_notes_means_no_clip() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, off(60));
        assert!(r.placements(sig()).is_empty());
    }
}
