//! A tuning system as the audio thread reads it: how far each key sits from 12-TET.

use crate::{note_to_hz, ExprKind, MidiEvent, MidiMessage};

/// The resource slot a tuning table is delivered on. See [`Processor::set_resource`]. Far above
/// any slot a device uses for samples, and refused by every device that doesn't care.
///
/// [`Processor::set_resource`]: crate::Processor::set_resource
pub const TUNING_SLOT: u32 = u32::MAX;

/// Every key's distance from its 12-TET pitch, in semitones.
#[derive(Debug, Clone, PartialEq)]
pub struct TuningTable {
    offsets: [f32; 128],
}

impl Default for TuningTable {
    fn default() -> Self {
        Self {
            offsets: [0.0; 128],
        }
    }
}

impl TuningTable {
    /// A table from each key's frequency in hertz. A key that has no real frequency stays put.
    pub fn from_hz(hz: impl Fn(u8) -> f64) -> Self {
        let mut offsets = [0.0; 128];
        for (key, offset) in offsets.iter_mut().enumerate() {
            let f = hz(key as u8);
            if f.is_finite() && f > 0.0 {
                *offset = (69.0 + 12.0 * (f / 440.0).log2() - key as f64) as f32;
            }
        }
        Self { offsets }
    }

    /// Semitones this key sits from 12-TET.
    #[inline]
    pub fn offset(&self, key: u8) -> f32 {
        self.offsets[(key & 127) as usize]
    }

    /// The pitch of a fractional note in hertz, the offset interpolated between keys.
    pub fn note_hz(&self, note: f32) -> f32 {
        let below = note.floor().clamp(0.0, 127.0) as u8;
        let above = (below + 1).min(127);
        let x = (note - f32::from(below)).clamp(0.0, 1.0);
        let offset = self.offset(below) + (self.offset(above) - self.offset(below)) * x;
        note_to_hz(note + offset)
    }

    /// `event` as an instrument should hear it: a note-on carries the key's offset, and so do
    /// per-note pitch values, because plug-ins have one tuning dimension and a later Pitch value
    /// replaces what a note started with.
    #[inline]
    pub fn tune(&self, event: MidiEvent) -> MidiEvent {
        let mut event = event;
        match &mut event.message {
            MidiMessage::NoteOn { key, tuning, .. } => *tuning += self.offset(*key),
            MidiMessage::NoteExpr {
                key,
                kind: ExprKind::Pitch,
                value,
                ..
            } => *value += self.offset(*key),
            _ => {}
        }
        event
    }
}

/// What a controller's keys play under a tuning system's layout: `None` means nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyLayout {
    keys: [Option<u8>; 128],
}

impl KeyLayout {
    pub fn new(map: impl Fn(u8) -> Option<u8>) -> Self {
        Self {
            keys: std::array::from_fn(|k| map(k as u8)),
        }
    }

    #[inline]
    pub fn map(&self, key: u8) -> Option<u8> {
        self.keys[(key & 127) as usize]
    }

    /// `message` with its key moved, or `None` if that key plays nothing. Messages with no key
    /// pass untouched.
    pub fn apply(&self, message: MidiMessage) -> Option<MidiMessage> {
        let mut message = message;
        let key = match &mut message {
            MidiMessage::NoteOn { key, .. }
            | MidiMessage::NoteOff { key, .. }
            | MidiMessage::NoteExpr { key, .. } => key,
            _ => return Some(message),
        };
        *key = self.map(*key)?;
        Some(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(key: u8) -> MidiEvent {
        MidiEvent::new(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                key,
                velocity: 1.0,
                note_id: 1,
                tuning: 0.0,
            },
        )
    }

    #[test]
    fn an_equal_tempered_table_has_no_offsets() {
        let t = TuningTable::from_hz(|k| 440.0 * 2f64.powf((f64::from(k) - 69.0) / 12.0));
        assert!((0..128).all(|k| t.offset(k).abs() < 1e-3));
        assert!((t.note_hz(69.0) - 440.0).abs() < 1e-3);
    }

    #[test]
    fn notes_and_pitch_expression_both_carry_the_offset() {
        let t = TuningTable::from_hz(|k| 440.0 * 2f64.powf((f64::from(k) - 68.0) / 12.0));
        let MidiMessage::NoteOn { tuning, .. } = t.tune(on(60)).message else {
            panic!()
        };
        assert!((tuning - 1.0).abs() < 1e-4);
        let expr = MidiEvent::new(
            0,
            MidiMessage::NoteExpr {
                channel: 0,
                key: 60,
                note_id: 1,
                kind: ExprKind::Pitch,
                value: 2.0,
            },
        );
        let MidiMessage::NoteExpr { value, .. } = t.tune(expr).message else {
            panic!()
        };
        assert!((value - 3.0).abs() < 1e-4);
    }

    #[test]
    fn other_expression_is_left_alone() {
        let t = TuningTable::from_hz(|_| 100.0);
        let slide = MidiEvent::new(
            0,
            MidiMessage::NoteExpr {
                channel: 0,
                key: 60,
                note_id: 1,
                kind: ExprKind::Slide,
                value: 0.5,
            },
        );
        assert_eq!(t.tune(slide), slide);
    }

    #[test]
    fn a_layout_moves_keys_and_silences_the_rest() {
        let l = KeyLayout::new(|k| (k % 2 == 0).then_some(k + 1));
        assert_eq!(
            l.apply(on(60).message),
            Some(
                MidiEvent::new(
                    0,
                    MidiMessage::NoteOn {
                        channel: 0,
                        key: 61,
                        velocity: 1.0,
                        note_id: 1,
                        tuning: 0.0,
                    }
                )
                .message
            )
        );
        assert_eq!(l.apply(on(61).message), None);
        assert!(l.apply(MidiMessage::AllNotesOff).is_some());
    }
}
