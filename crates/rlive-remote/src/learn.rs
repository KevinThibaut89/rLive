//! Working out which message a control is being mapped to.

use crate::decode::MidiEvent;
use rlive_project::{CcEncoding, MidiSource};

/// The message a control should listen for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Learned {
    pub channel: u8,
    pub source: MidiSource,
}

/// How many messages all reading ±1 it takes to say an encoder doesn't accelerate.
const LINEAR_AFTER: usize = 16;

/// Follows what arrives while a control waits for its mapping. Each message may refine the
/// answer: a held key plus a key either side makes a range, and a few values make it clear what
/// kind of controller it is.
#[derive(Debug, Default)]
pub struct Learner {
    held: Vec<(u8, u8)>,
    root: Option<(u8, u8)>,
    low: u8,
    high: u8,
    /// The controller being moved, and what it has sent lately.
    cc: Option<(u8, u8)>,
    values: Vec<u8>,
    /// Pairs of controllers 32 apart, seen together.
    wide: Option<(u8, u8)>,
}

impl Learner {
    /// One message. Returns the mapping as it now stands. `chromatic` says the target is a slot,
    /// whose range keeps its root.
    pub fn feed(&mut self, event: MidiEvent, chromatic: bool) -> Option<Learned> {
        match event {
            MidiEvent::Note {
                channel,
                key,
                velocity: 0,
            } => {
                self.held.retain(|h| *h != (channel, key));
                if self.held.is_empty() {
                    self.root = None;
                }
                None
            }
            MidiEvent::Note { channel, key, .. } => Some(self.note_on(channel, key, chromatic)),
            MidiEvent::Cc {
                channel,
                number,
                value,
            } => Some(self.controller(channel, number, value)),
            MidiEvent::PitchBend { channel, .. } => Some(Learned {
                channel,
                source: MidiSource::PitchBend,
            }),
        }
    }

    fn note_on(&mut self, channel: u8, key: u8, chromatic: bool) -> Learned {
        self.held.retain(|h| *h != (channel, key));
        self.held.push((channel, key));
        self.cc = None;
        let Some((_, root)) = self.root.filter(|(c, _)| *c == channel) else {
            self.root = Some((channel, key));
            self.low = key;
            self.high = key;
            return Learned {
                channel,
                source: MidiSource::Note(key),
            };
        };
        if key < root {
            self.low = key;
        } else if key > root {
            self.high = key;
        }
        let source = if self.low == self.high {
            MidiSource::Note(root)
        } else if chromatic {
            MidiSource::ChromaticRange {
                root,
                low: self.low,
                high: self.high,
            }
        } else {
            MidiSource::NoteRange {
                low: self.low,
                high: self.high,
            }
        };
        Learned { channel, source }
    }

    fn controller(&mut self, channel: u8, number: u8, value: u8) -> Learned {
        self.root = None;
        self.held.clear();
        // A controller and the one 32 above it, in step, are the two halves of one 14-bit value.
        if let Some((ch, n)) = self.cc {
            if ch == channel && number >= 32 && number - 32 == n && n < 32 {
                self.wide = Some((channel, n));
            }
        }
        if let Some((ch, msb)) = self.wide {
            if ch == channel && (number == msb || number == msb + 32) {
                return Learned {
                    channel,
                    source: MidiSource::Cc {
                        number: msb,
                        encoding: CcEncoding::Absolute14Bit,
                    },
                };
            }
        }
        if self.cc != Some((channel, number)) {
            self.cc = Some((channel, number));
            self.values.clear();
            self.wide = None;
        }
        self.values.push(value);
        Learned {
            channel,
            source: MidiSource::Cc {
                number,
                encoding: detect(&self.values),
            },
        }
    }
}

/// Which kind of controller these values came from. The manual asks for the control to be moved
/// slowly to the left, and that's what this assumes (p. 871): a relative encoder then keeps
/// sending the code for one step down, where an absolute control never repeats itself.
pub fn detect(values: &[u8]) -> CcEncoding {
    if values.len() < 2 || !values.windows(2).any(|w| w[0] == w[1]) {
        return CcEncoding::Absolute;
    }
    // The code for one step down tells the encodings apart.
    let mut counts = [0usize; 128];
    for v in values {
        counts[usize::from(v & 0x7f)] += 1;
    }
    let common = (0..128u8)
        .max_by_key(|v| counts[usize::from(*v)])
        .unwrap_or(0);
    let same = values.iter().all(|v| *v == common);
    let linear = same && values.len() >= LINEAR_AFTER;
    use CcEncoding as E;
    match common {
        64..=90 => {
            if linear {
                E::RelativeSignedBitLinear
            } else {
                E::RelativeSignedBit
            }
        }
        1..=39 => {
            if linear {
                E::RelativeSignedBit2Linear
            } else {
                E::RelativeSignedBit2
            }
        }
        40..=63 => {
            if linear {
                E::RelativeBinOffsetLinear
            } else {
                E::RelativeBinOffset
            }
        }
        91..=127 => {
            if linear {
                E::RelativeTwosComplementLinear
            } else {
                E::RelativeTwosComplement
            }
        }
        _ => E::Absolute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CcEncoding as E;

    fn cc(number: u8, value: u8) -> MidiEvent {
        MidiEvent::Cc {
            channel: 0,
            number,
            value,
        }
    }

    fn note(key: u8, velocity: u8) -> MidiEvent {
        MidiEvent::Note {
            channel: 0,
            key,
            velocity,
        }
    }

    #[test]
    fn a_fader_is_absolute() {
        assert_eq!(detect(&[64]), E::Absolute);
        assert_eq!(detect(&[64, 63, 62, 61, 60]), E::Absolute);
    }

    #[test]
    fn a_repeated_value_is_an_encoder_and_says_which_kind() {
        assert_eq!(detect(&[65, 65]), E::RelativeSignedBit);
        assert_eq!(detect(&[1, 1]), E::RelativeSignedBit2);
        assert_eq!(detect(&[63, 63]), E::RelativeBinOffset);
        assert_eq!(detect(&[127, 127]), E::RelativeTwosComplement);
        assert_eq!(detect(&[65, 66, 65, 65]), E::RelativeSignedBit);
    }

    #[test]
    fn a_long_steady_turn_is_taken_for_a_linear_encoder() {
        assert_eq!(detect(&[65; LINEAR_AFTER]), E::RelativeSignedBitLinear);
        assert_eq!(detect(&[1; LINEAR_AFTER]), E::RelativeSignedBit2Linear);
        assert_eq!(detect(&[63; LINEAR_AFTER]), E::RelativeBinOffsetLinear);
        assert_eq!(
            detect(&[127; LINEAR_AFTER]),
            E::RelativeTwosComplementLinear
        );
    }

    #[test]
    fn a_controller_refines_as_it_moves() {
        let mut l = Learner::default();
        let first = l.feed(cc(5, 65), false).unwrap();
        assert_eq!(
            first.source,
            MidiSource::Cc {
                number: 5,
                encoding: E::Absolute
            }
        );
        let second = l.feed(cc(5, 65), false).unwrap();
        assert_eq!(
            second.source,
            MidiSource::Cc {
                number: 5,
                encoding: E::RelativeSignedBit
            }
        );
    }

    #[test]
    fn two_controllers_32_apart_make_one_14_bit_value() {
        let mut l = Learner::default();
        l.feed(cc(1, 10), false);
        let two = l.feed(cc(33, 20), false).unwrap();
        assert_eq!(
            two.source,
            MidiSource::Cc {
                number: 1,
                encoding: E::Absolute14Bit
            }
        );
        assert_eq!(l.feed(cc(1, 11), false).unwrap().source, two.source);
    }

    #[test]
    fn pitch_bend_learns_as_itself() {
        let mut l = Learner::default();
        let got = l
            .feed(
                MidiEvent::PitchBend {
                    channel: 3,
                    value: 1,
                },
                false,
            )
            .unwrap();
        assert_eq!(got.source, MidiSource::PitchBend);
        assert_eq!(got.channel, 3);
    }

    #[test]
    fn a_held_root_and_keys_either_side_make_a_chromatic_range() {
        let mut l = Learner::default();
        assert_eq!(
            l.feed(note(60, 100), true).unwrap().source,
            MidiSource::Note(60)
        );
        let low = l.feed(note(55, 100), true).unwrap();
        assert_eq!(
            low.source,
            MidiSource::ChromaticRange {
                root: 60,
                low: 55,
                high: 60
            }
        );
        let both = l.feed(note(67, 100), true).unwrap();
        assert_eq!(
            both.source,
            MidiSource::ChromaticRange {
                root: 60,
                low: 55,
                high: 67
            }
        );
    }

    #[test]
    fn a_parameter_gets_a_plain_range_and_letting_go_starts_again() {
        let mut l = Learner::default();
        l.feed(note(60, 100), false);
        let r = l.feed(note(64, 100), false).unwrap();
        assert_eq!(r.source, MidiSource::NoteRange { low: 60, high: 64 });
        l.feed(note(60, 0), false);
        l.feed(note(64, 0), false);
        assert_eq!(
            l.feed(note(70, 100), false).unwrap().source,
            MidiSource::Note(70)
        );
    }
}
