//! What goes back to a controller's LEDs and motor faders (manual p. 868).

use crate::table::Compiled;
use rlive_project::{CcEncoding, MidiSource};
use std::collections::HashMap;

/// Where feedback goes. `port` is the same numbering as [`Compiled::feedback`].
pub trait FeedbackSink {
    fn send(&mut self, port: u32, bytes: &[u8]);
}

/// The state to show.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeedbackValue {
    /// A position along the control's travel, 0 to 1.
    Position(f64),
    On(bool),
}

/// Key: channel plus the message type and number.
type Key = (u8, u8, u8);

/// What each LED and fader was last told, so only changes go out.
#[derive(Debug, Default)]
pub struct FeedbackState {
    last: HashMap<Key, u16>,
}

impl FeedbackState {
    /// Forget it all, so everything is sent again.
    pub fn reset(&mut self) {
        self.last.clear();
    }

    /// Show `value` on the control of mapping `e`. `received` is the control's own last position
    /// (out of 127 or 16383): a fader shouldn't be told where it was just put.
    pub fn update(
        &mut self,
        e: &Compiled,
        value: FeedbackValue,
        received: Option<u16>,
        ports: u64,
        sink: &mut impl FeedbackSink,
    ) {
        let ports = ports & e.feedback;
        if ports == 0 {
            return;
        }
        let mut emit = |bytes: &[u8]| {
            for port in 0..64 {
                if ports & (1u64 << port) != 0 {
                    sink.send(port, bytes);
                }
            }
        };
        match e.source {
            MidiSource::Note(n) => {
                let on = is_on(value);
                if self.changed((e.channel, 0, n), u16::from(on)) {
                    emit(&[0x90 | (e.channel & 15), n, if on { 127 } else { 0 }]);
                }
            }
            MidiSource::NoteRange { low, high } | MidiSource::ChromaticRange { low, high, .. } => {
                // The key at the value is lit.
                let span = u16::from(high.max(low) - high.min(low));
                let lit = (position(value) * f64::from(span)).round() as u16;
                for (i, key) in (low.min(high)..=low.max(high)).enumerate() {
                    let on = i as u16 == lit;
                    if self.changed((e.channel, 0, key), u16::from(on)) {
                        emit(&[0x90 | (e.channel & 15), key, if on { 127 } else { 0 }]);
                    }
                }
            }
            MidiSource::Cc {
                number,
                encoding: CcEncoding::Absolute14Bit,
            } => {
                let v = (position(value) * 16383.0).round() as u16;
                if received != Some(v) && self.changed((e.channel, 1, number), v) {
                    let ch = e.channel & 15;
                    emit(&[0xb0 | ch, number, (v >> 7) as u8]);
                    emit(&[0xb0 | ch, number + 32, (v & 127) as u8]);
                }
            }
            MidiSource::Cc { number, encoding } => {
                if !encoding.is_relative() {
                    let v = (position(value) * 127.0).round() as u16;
                    if received != Some(v) && self.changed((e.channel, 1, number), v) {
                        emit(&[0xb0 | (e.channel & 15), number, v as u8]);
                    }
                }
            }
            MidiSource::PitchBend => {
                let v = (position(value) * 16383.0).round() as u16;
                if received != Some(v) && self.changed((e.channel, 2, 0), v) {
                    emit(&[0xe0 | (e.channel & 15), (v & 127) as u8, (v >> 7) as u8]);
                }
            }
        }
    }

    fn changed(&mut self, key: Key, value: u16) -> bool {
        self.last.insert(key, value) != Some(value)
    }
}

fn is_on(v: FeedbackValue) -> bool {
    match v {
        FeedbackValue::On(on) => on,
        FeedbackValue::Position(p) => p >= 0.5,
    }
}

fn position(v: FeedbackValue) -> f64 {
    match v {
        FeedbackValue::On(on) => f64::from(u8::from(on)),
        FeedbackValue::Position(p) => p.clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Exec, ALL_PORTS};
    use rlive_project::{CcEncoding, MapTarget};

    #[derive(Default)]
    struct Out(Vec<(u32, Vec<u8>)>);

    impl FeedbackSink for Out {
        fn send(&mut self, port: u32, bytes: &[u8]) {
            self.0.push((port, bytes.to_vec()));
        }
    }

    fn entry(source: MidiSource) -> Compiled {
        Compiled {
            channel: 1,
            source,
            target: MapTarget::Scene(0),
            exec: Exec::Blocked,
            ports: ALL_PORTS,
            feedback: ALL_PORTS,
            value: 0.0,
        }
    }

    fn cc(encoding: CcEncoding) -> MidiSource {
        MidiSource::Cc {
            number: 7,
            encoding,
        }
    }

    #[test]
    fn a_controller_hears_its_value_once() {
        let mut st = FeedbackState::default();
        let mut out = Out::default();
        let e = entry(cc(CcEncoding::Absolute));
        st.update(&e, FeedbackValue::Position(1.0), None, 1, &mut out);
        st.update(&e, FeedbackValue::Position(1.0), None, 1, &mut out);
        assert_eq!(out.0, vec![(0, vec![0xb1, 7, 127])]);
    }

    #[test]
    fn a_fader_is_not_told_where_it_was_just_put() {
        let mut st = FeedbackState::default();
        let mut out = Out::default();
        let e = entry(cc(CcEncoding::Absolute));
        st.update(&e, FeedbackValue::Position(1.0), Some(127), 1, &mut out);
        assert!(out.0.is_empty());
    }

    #[test]
    fn relative_controllers_get_no_value_back() {
        let mut st = FeedbackState::default();
        let mut out = Out::default();
        let e = entry(cc(CcEncoding::RelativeBinOffset));
        st.update(&e, FeedbackValue::Position(1.0), None, 1, &mut out);
        assert!(out.0.is_empty());
    }

    #[test]
    fn notes_light_their_leds() {
        let mut st = FeedbackState::default();
        let mut out = Out::default();
        let e = entry(MidiSource::Note(36));
        st.update(&e, FeedbackValue::On(true), None, 1, &mut out);
        st.update(&e, FeedbackValue::On(false), None, 1, &mut out);
        assert_eq!(
            out.0,
            vec![(0, vec![0x91, 36, 127]), (0, vec![0x91, 36, 0])]
        );
    }

    #[test]
    fn fourteen_bit_and_pitch_bend_are_sent_whole() {
        let mut st = FeedbackState::default();
        let mut out = Out::default();
        let e = entry(cc(CcEncoding::Absolute14Bit));
        st.update(&e, FeedbackValue::Position(1.0), None, 1, &mut out);
        assert_eq!(
            out.0,
            vec![(0, vec![0xb1, 7, 127]), (0, vec![0xb1, 39, 127])]
        );
        let mut out = Out::default();
        let e = entry(MidiSource::PitchBend);
        st.update(&e, FeedbackValue::Position(0.0), None, 1, &mut out);
        assert_eq!(out.0, vec![(0, vec![0xe1, 0, 0])]);
    }

    #[test]
    fn a_note_range_lights_the_key_at_the_value() {
        let mut st = FeedbackState::default();
        let mut out = Out::default();
        let e = entry(MidiSource::NoteRange { low: 10, high: 12 });
        st.update(&e, FeedbackValue::Position(0.5), None, 1, &mut out);
        assert_eq!(
            out.0,
            vec![
                (0, vec![0x91, 10, 0]),
                (0, vec![0x91, 11, 127]),
                (0, vec![0x91, 12, 0])
            ]
        );
    }

    #[test]
    fn only_the_ports_asked_for_get_feedback() {
        let mut st = FeedbackState::default();
        let mut out = Out::default();
        let mut e = entry(cc(CcEncoding::Absolute));
        e.feedback = 0b10;
        st.update(&e, FeedbackValue::Position(1.0), None, 0b11, &mut out);
        assert_eq!(out.0.len(), 1);
        assert_eq!(out.0[0].0, 1);
    }
}
