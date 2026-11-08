/// A MIDI-style message. Velocities are normalised to `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidiMessage {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: f32,
    },
    NoteOff {
        channel: u8,
        key: u8,
        velocity: f32,
    },
    ControlChange {
        channel: u8,
        cc: u8,
        value: u8,
    },
    /// Pitch bend in the range -1.0..=1.0.
    PitchBend {
        channel: u8,
        value: f32,
    },
    AllNotesOff,
}

impl MidiMessage {
    /// Parse a raw MIDI 1.0 message (as delivered by `midir`).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let status = *bytes.first()?;
        let channel = status & 0x0f;
        match status & 0xf0 {
            0x80 => Some(Self::NoteOff {
                channel,
                key: *bytes.get(1)? & 0x7f,
                velocity: (*bytes.get(2).unwrap_or(&64) & 0x7f) as f32 / 127.0,
            }),
            0x90 => {
                let key = *bytes.get(1)? & 0x7f;
                let vel = *bytes.get(2)? & 0x7f;
                if vel == 0 {
                    Some(Self::NoteOff {
                        channel,
                        key,
                        velocity: 0.5,
                    })
                } else {
                    Some(Self::NoteOn {
                        channel,
                        key,
                        velocity: vel as f32 / 127.0,
                    })
                }
            }
            0xb0 => {
                let cc = *bytes.get(1)? & 0x7f;
                let value = *bytes.get(2)? & 0x7f;
                if cc == 123 {
                    Some(Self::AllNotesOff)
                } else {
                    Some(Self::ControlChange { channel, cc, value })
                }
            }
            0xe0 => {
                let lsb = (*bytes.get(1)? & 0x7f) as i32;
                let msb = (*bytes.get(2)? & 0x7f) as i32;
                let raw = (msb << 7 | lsb) - 8192;
                Some(Self::PitchBend {
                    channel,
                    value: raw as f32 / 8192.0,
                })
            }
            _ => None,
        }
    }

    /// Encode as raw MIDI 1.0 bytes. Returns the bytes and the number used.
    pub fn to_bytes(&self) -> ([u8; 3], usize) {
        match *self {
            Self::NoteOn {
                channel,
                key,
                velocity,
            } => (
                [
                    0x90 | (channel & 0x0f),
                    key & 0x7f,
                    ((velocity * 127.0).round() as u8).clamp(1, 127),
                ],
                3,
            ),
            Self::NoteOff {
                channel,
                key,
                velocity,
            } => (
                [
                    0x80 | (channel & 0x0f),
                    key & 0x7f,
                    ((velocity * 127.0).round() as u8).min(127),
                ],
                3,
            ),
            Self::ControlChange { channel, cc, value } => {
                ([0xb0 | (channel & 0x0f), cc & 0x7f, value & 0x7f], 3)
            }
            Self::PitchBend { channel, value } => {
                let raw = ((value.clamp(-1.0, 1.0) * 8191.0) as i32 + 8192) as u16;
                (
                    [
                        0xe0 | (channel & 0x0f),
                        (raw & 0x7f) as u8,
                        (raw >> 7) as u8 & 0x7f,
                    ],
                    3,
                )
            }
            Self::AllNotesOff => ([0xb0, 123, 0], 3),
        }
    }

    /// Sort priority so that note-offs at the same timestamp come before note-ons
    /// (otherwise a retriggered note would be cut immediately).
    pub fn order(&self) -> u8 {
        match self {
            Self::AllNotesOff => 0,
            Self::NoteOff { .. } => 1,
            Self::ControlChange { .. } | Self::PitchBend { .. } => 2,
            Self::NoteOn { .. } => 3,
        }
    }
}

/// A timestamped MIDI message. `offset` is the sample frame within the current block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiEvent {
    pub offset: u32,
    pub message: MidiMessage,
}

impl MidiEvent {
    pub fn new(offset: u32, message: MidiMessage) -> Self {
        Self { offset, message }
    }

    pub fn sort_key(&self) -> (u32, u8) {
        (self.offset, self.message.order())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        let on = MidiMessage::from_bytes(&[0x91, 60, 127]).unwrap();
        assert_eq!(
            on,
            MidiMessage::NoteOn {
                channel: 1,
                key: 60,
                velocity: 1.0
            }
        );
        let (bytes, n) = on.to_bytes();
        assert_eq!(&bytes[..n], &[0x91, 60, 127]);
        // Note-on with velocity 0 is a note-off.
        assert!(matches!(
            MidiMessage::from_bytes(&[0x90, 60, 0]),
            Some(MidiMessage::NoteOff { key: 60, .. })
        ));
        let bend = MidiMessage::from_bytes(&[0xe0, 0, 64]).unwrap();
        assert_eq!(
            bend,
            MidiMessage::PitchBend {
                channel: 0,
                value: 0.0
            }
        );
    }
}
