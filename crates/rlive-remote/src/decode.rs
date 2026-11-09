//! What a single MIDI message says (manual pp. 870-872).

use rlive_project::CcEncoding;

/// A channel message, the parts of it mapping cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiEvent {
    /// `velocity` is 0 for a note off, however it was sent.
    Note {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    Cc {
        channel: u8,
        number: u8,
        value: u8,
    },
    /// 0 to 16383, centred on 8192.
    PitchBend {
        channel: u8,
        value: u16,
    },
}

impl MidiEvent {
    /// Reads raw MIDI bytes. `None` for anything mapping doesn't use, such as aftertouch.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let status = *bytes.first()?;
        let channel = status & 0x0f;
        let d1 = *bytes.get(1)? & 0x7f;
        let d2 = bytes.get(2).copied().unwrap_or(0) & 0x7f;
        match status & 0xf0 {
            0x80 => Some(Self::Note {
                channel,
                key: d1,
                velocity: 0,
            }),
            0x90 => Some(Self::Note {
                channel,
                key: d1,
                velocity: d2,
            }),
            0xb0 => Some(Self::Cc {
                channel,
                number: d1,
                value: d2,
            }),
            0xe0 => Some(Self::PitchBend {
                channel,
                value: u16::from(d2) << 7 | u16::from(d1),
            }),
            _ => None,
        }
    }
}

/// A message, read for one mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// A note on or off. `key` is the note that was played.
    Note { key: u8, velocity: u8 },
    /// An absolute value out of `top`.
    Absolute { raw: u16, top: u16 },
    /// A number of steps, negative for a decrement.
    Relative { delta: i32 },
}

impl Reading {
    /// Whether an absolute value is in the top half, which counts as "on" for a slot or an
    /// ordinary switch. The middle of a 14-bit range is at 8191/8192 (manual p. 871).
    pub fn absolute_is_high(raw: u16, top: u16) -> bool {
        u32::from(raw) * 2 > u32::from(top)
    }

    /// A value as a fraction of its range.
    pub fn normalized(raw: u16, top: u16) -> f64 {
        f64::from(raw) / f64::from(top.max(1))
    }
}

/// How many steps a relative controller's value means. The linear encodings count every message
/// as one step in its direction, ignoring the size an accelerating encoder would report.
pub fn decode_relative(encoding: CcEncoding, value: u8) -> i32 {
    let v = i32::from(value & 0x7f);
    match encoding {
        CcEncoding::Absolute | CcEncoding::Absolute14Bit => 0,
        // Bit 6 is the sign: 1 is one up, 65 is one down.
        CcEncoding::RelativeSignedBit => signed_bit(v),
        // The opposite way round: 65 is one up, 1 is one down.
        CcEncoding::RelativeSignedBit2 => -signed_bit(v),
        // 64 is no change; 65 up, 63 down.
        CcEncoding::RelativeBinOffset => v - 64,
        // 1 up, 127 down, 64 is as far down as it goes.
        CcEncoding::RelativeTwosComplement => twos_complement(v),
        CcEncoding::RelativeSignedBitLinear => signed_bit(v).signum(),
        CcEncoding::RelativeSignedBit2Linear => (-signed_bit(v)).signum(),
        CcEncoding::RelativeBinOffsetLinear => (v - 64).signum(),
        CcEncoding::RelativeTwosComplementLinear => twos_complement(v).signum(),
    }
}

fn signed_bit(v: i32) -> i32 {
    if v & 0x40 != 0 {
        -(v & 0x3f)
    } else {
        v & 0x3f
    }
}

fn twos_complement(v: i32) -> i32 {
    if v >= 64 {
        v - 128
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CcEncoding as E;

    #[test]
    fn every_relative_encoding_reads_one_step_each_way() {
        let cases = [
            (E::RelativeSignedBit, 1, 65),
            (E::RelativeSignedBit2, 65, 1),
            (E::RelativeBinOffset, 65, 63),
            (E::RelativeTwosComplement, 1, 127),
            (E::RelativeSignedBitLinear, 1, 65),
            (E::RelativeSignedBit2Linear, 65, 1),
            (E::RelativeBinOffsetLinear, 65, 63),
            (E::RelativeTwosComplementLinear, 1, 127),
        ];
        for (encoding, up, down) in cases {
            assert_eq!(decode_relative(encoding, up), 1, "{encoding:?} up");
            assert_eq!(decode_relative(encoding, down), -1, "{encoding:?} down");
        }
    }

    #[test]
    fn accelerated_values_keep_their_size_and_linear_ones_do_not() {
        assert_eq!(decode_relative(E::RelativeSignedBit, 10), 10);
        assert_eq!(decode_relative(E::RelativeSignedBit, 75), -11);
        assert_eq!(decode_relative(E::RelativeSignedBit2, 75), 11);
        assert_eq!(decode_relative(E::RelativeBinOffset, 70), 6);
        assert_eq!(decode_relative(E::RelativeBinOffset, 58), -6);
        assert_eq!(decode_relative(E::RelativeTwosComplement, 120), -8);
        assert_eq!(decode_relative(E::RelativeTwosComplement, 8), 8);
        assert_eq!(decode_relative(E::RelativeSignedBitLinear, 75), -1);
        assert_eq!(decode_relative(E::RelativeSignedBit2Linear, 75), 1);
        assert_eq!(decode_relative(E::RelativeBinOffsetLinear, 70), 1);
        assert_eq!(decode_relative(E::RelativeTwosComplementLinear, 120), -1);
    }

    #[test]
    fn a_value_of_no_change_is_no_step() {
        assert_eq!(decode_relative(E::RelativeSignedBit, 0), 0);
        assert_eq!(decode_relative(E::RelativeSignedBit, 64), 0);
        assert_eq!(decode_relative(E::RelativeBinOffset, 64), 0);
        assert_eq!(decode_relative(E::RelativeTwosComplement, 0), 0);
    }

    #[test]
    fn parses_notes_controllers_and_pitch_bend() {
        assert_eq!(
            MidiEvent::parse(&[0x92, 60, 100]),
            Some(MidiEvent::Note {
                channel: 2,
                key: 60,
                velocity: 100
            })
        );
        // A note on at velocity zero is a note off, as is the real thing.
        for bytes in [[0x90, 60, 0], [0x80, 60, 77]] {
            assert_eq!(
                MidiEvent::parse(&bytes),
                Some(MidiEvent::Note {
                    channel: 0,
                    key: 60,
                    velocity: 0
                })
            );
        }
        assert_eq!(
            MidiEvent::parse(&[0xb1, 7, 127]),
            Some(MidiEvent::Cc {
                channel: 1,
                number: 7,
                value: 127
            })
        );
        assert_eq!(
            MidiEvent::parse(&[0xe0, 0x7f, 0x7f]),
            Some(MidiEvent::PitchBend {
                channel: 0,
                value: 16383
            })
        );
        assert_eq!(MidiEvent::parse(&[0xd0, 10]), None);
        assert_eq!(MidiEvent::parse(&[0xc0, 10]), None);
    }

    #[test]
    fn absolute_values_divide_at_the_middle_of_their_range() {
        assert!(!Reading::absolute_is_high(63, 127));
        assert!(Reading::absolute_is_high(64, 127));
        assert!(!Reading::absolute_is_high(8191, 16383));
        assert!(Reading::absolute_is_high(8192, 16383));
        assert_eq!(Reading::normalized(127, 127), 1.0);
        assert_eq!(Reading::normalized(16383, 16383), 1.0);
        assert_eq!(Reading::normalized(0, 127), 0.0);
    }
}
