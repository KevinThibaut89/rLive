//! MIDI clip rendering.

use super::data::{ClipData, MidiClip};
use super::BlockTime;
use rlive_core::{MidiEvent, MidiMessage};

/// Counts sounding instances of each key, so note-offs can be sent on stop.
pub(crate) struct Voices([u8; 128]);

impl Voices {
    pub fn new() -> Self {
        Self([0; 128])
    }

    fn on(&mut self, key: usize) {
        self.0[key] = self.0[key].saturating_add(1);
    }

    /// Returns whether the key was sounding (and a note-off should be sent).
    fn off(&mut self, key: usize) -> bool {
        if self.0[key] > 0 {
            self.0[key] -= 1;
            true
        } else {
            false
        }
    }

    /// Emit note-offs for every sounding note at `offset`.
    pub fn release_all(&mut self, offset: u32, events: &mut Vec<MidiEvent>) {
        for key in 0..128 {
            if self.0[key] > 0 {
                self.0[key] = 0;
                push_event(events, MidiEvent::new(offset, note_off(key as u8)));
            }
        }
    }
}

fn note_off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
    }
}

pub(super) fn render_midi(
    clip: &MidiClip,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    voices: &mut Voices,
    ev: &mut Vec<MidiEvent>,
) {
    let len = clip.length as f64;
    let k_first = (((a - origin) / len).floor() as i64 - 1).max(0);
    let mut k_last = ((seg_end - origin) / len).floor() as i64;
    if !clip.looped {
        k_last = k_last.min(0);
    }
    let lookback = clip.max_len as f64;
    for k in k_first..=k_last {
        let base = origin + k as f64 * len;
        // One tick of slack keeps float error in `base` from dropping a boundary note; the
        // exact comparisons below decide what actually fires.
        let scan_from = a - base - lookback - 1.0;
        let scan_to = seg_end - base + 1.0;
        let first = clip.notes.partition_point(|n| (n.start as f64) < scan_from);
        for note in &clip.notes[first..] {
            let start = note.start as f64;
            if start > scan_to || start >= len {
                break;
            }
            let key = note.key as usize & 127;
            let on = base + start;
            let off = base + ((note.start + note.length.max(1)) as f64).min(len);
            // Offs first: a note ending exactly where the next begins must not cut the new one.
            if off >= a && off < seg_end && voices.off(key) {
                push_event(
                    ev,
                    MidiEvent::new(time.event_offset(off), note_off(key as u8)),
                );
            }
            if on >= a && on < seg_end {
                voices.on(key);
                push_event(
                    ev,
                    MidiEvent::new(
                        time.event_offset(on),
                        MidiMessage::NoteOn {
                            channel: 0,
                            key: key as u8,
                            velocity: note.velocity,
                        },
                    ),
                );
            }
        }
    }
}

/// Start the MIDI notes that are already sounding at song tick `at`, at frame `offset`.
/// Notes starting exactly at `at` are left to [`render_range`].
pub(crate) fn chase(
    clip: &ClipData,
    origin: f64,
    at: f64,
    offset: u32,
    voices: &mut Voices,
    ev: &mut Vec<MidiEvent>,
) {
    let ClipData::Midi(clip) = clip else {
        return;
    };
    let len = clip.length as f64;
    let local = at - origin;
    if len <= 0.0 || local < 0.0 || (!clip.looped && local >= len) {
        return;
    }
    let base = origin + (local / len).floor() * len;
    let scan_from = at - base - clip.max_len as f64 - 1.0;
    let first = clip.notes.partition_point(|n| (n.start as f64) < scan_from);
    for note in &clip.notes[first..] {
        let start = note.start as f64;
        if base + start >= at || start >= len {
            break;
        }
        let off = base + ((note.start + note.length.max(1)) as f64).min(len);
        if off > at {
            let key = note.key as usize & 127;
            voices.on(key);
            push_event(
                ev,
                MidiEvent::new(
                    offset,
                    MidiMessage::NoteOn {
                        channel: 0,
                        key: key as u8,
                        velocity: note.velocity,
                    },
                ),
            );
        }
    }
}

/// Push without ever growing the vector (the audio thread must not allocate).
#[inline]
pub(crate) fn push_event(events: &mut Vec<MidiEvent>, e: MidiEvent) {
    if events.len() < events.capacity() {
        events.push(e);
    }
}
