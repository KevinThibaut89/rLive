//! MIDI leaving the engine for hardware ports.
//!
//! Everything bound for a port goes down one ring: a track's Ext. Out and an External Instrument
//! alike. The audio thread only ever pushes [`HwMidi`]s onto it; whatever owns the ports reads
//! them from another thread and sends each at its time, so nothing here can block.

use rlive_core::MidiMessage;
use std::time::{Duration, Instant};

/// A message on its way to a hardware MIDI port.
#[derive(Debug, Clone, Copy)]
pub struct HwMidi {
    pub port: u16,
    /// When the block it belongs to began.
    pub block: Instant,
    /// How far into that block it falls, in seconds.
    pub offset: f64,
    pub message: MidiMessage,
}

/// Where outgoing MIDI goes: a port, by its number in the list of output ports, and the channel
/// it is forced onto, if it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRoute {
    pub port: u16,
    pub channel: Option<u8>,
}

/// Which notes something has started on a port and not ended, so they can be ended if it goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeldNotes([u128; 16]);

impl HeldNotes {
    fn note(&mut self, channel: u8, key: u8, on: bool) {
        let bit = 1u128 << (key & 0x7f);
        let notes = &mut self.0[(channel & 0x0f) as usize];
        if on {
            *notes |= bit;
        } else {
            *notes &= !bit;
        }
    }
}

/// The audio thread's end of the path to the ports.
pub struct MidiOutSink {
    queue: rtrb::Producer<HwMidi>,
    started: Instant,
    block: Instant,
    sample_rate: f64,
}

impl MidiOutSink {
    pub(crate) fn new(queue: rtrb::Producer<HwMidi>, sample_rate: f64) -> Self {
        let now = Instant::now();
        Self {
            queue,
            started: now,
            block: now,
            sample_rate: sample_rate.max(1.0),
        }
    }

    /// A new call from the audio driver.
    pub(crate) fn begin_buffer(&mut self, started: Instant) {
        self.started = started;
        self.block = started;
    }

    /// A new block, `done` frames into the driver's buffer.
    pub(crate) fn begin_block(&mut self, done: usize) {
        self.block = self.started + Duration::from_secs_f64(done as f64 / self.sample_rate);
    }

    /// Send a message exactly as it is, `offset` frames into the block.
    pub(crate) fn push(&mut self, port: u16, message: MidiMessage, offset: u32) {
        let _ = self.queue.push(HwMidi {
            port,
            block: self.block,
            offset: f64::from(offset) / self.sample_rate,
            message,
        });
    }

    fn track(held: &mut HeldNotes, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { channel, key, .. } => held.note(channel, key, true),
            MidiMessage::NoteOff { channel, key, .. } => held.note(channel, key, false),
            _ => {}
        }
    }

    /// Send a message on the route's channel. Per-note expression can't be said on one channel of
    /// MIDI 1.0, so it stays home unless an MPE encoder has spread the notes out first.
    pub(crate) fn send(
        &mut self,
        route: PortRoute,
        held: &mut HeldNotes,
        mut message: MidiMessage,
        offset: u32,
    ) {
        if matches!(message, MidiMessage::NoteExpr { .. }) {
            return;
        }
        if let Some(channel) = route.channel {
            message.set_channel(channel);
        }
        Self::track(held, message);
        self.push(route.port, message, offset);
    }

    /// Send what an MPE encoder made: its channels are the ones it chose.
    pub(crate) fn send_encoded(
        &mut self,
        route: PortRoute,
        held: &mut HeldNotes,
        message: MidiMessage,
        offset: u32,
    ) {
        Self::track(held, message);
        self.push(route.port, message, offset);
    }

    /// End every note in `held`.
    pub(crate) fn release(&mut self, route: PortRoute, held: &mut HeldNotes, offset: u32) {
        for channel in 0..16u8 {
            let mut notes = held.0[channel as usize];
            while notes != 0 {
                let key = notes.trailing_zeros() as u8;
                notes &= notes - 1;
                let message = MidiMessage::NoteOff {
                    channel,
                    key,
                    velocity: 0.0,
                    note_id: 0,
                };
                self.push(route.port, message, offset);
            }
            held.0[channel as usize] = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> (MidiOutSink, rtrb::Consumer<HwMidi>) {
        let (tx, rx) = rtrb::RingBuffer::new(32);
        (MidiOutSink::new(tx, 48_000.0), rx)
    }

    const ROUTE: PortRoute = PortRoute {
        port: 7,
        channel: Some(2),
    };

    fn on(key: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        }
    }

    #[test]
    fn a_message_goes_out_on_the_routes_channel_at_its_time() {
        let (mut s, mut rx) = sink();
        let mut held = HeldNotes::default();
        s.send(ROUTE, &mut held, on(60), 24);
        let e = rx.pop().unwrap();
        assert!((e.offset - 0.0005).abs() < 1e-9);
        assert_eq!(e.port, 7);
        assert!(matches!(e.message, MidiMessage::NoteOn { channel: 2, .. }));
    }

    #[test]
    fn a_later_block_starts_later() {
        let (mut s, mut rx) = sink();
        let start = Instant::now();
        s.begin_buffer(start);
        s.begin_block(48);
        s.push(1, on(60), 0);
        let e = rx.pop().unwrap();
        assert_eq!(e.block, start + Duration::from_millis(1));
    }

    #[test]
    fn what_an_encoder_chose_keeps_its_channel() {
        let (mut s, mut rx) = sink();
        let mut held = HeldNotes::default();
        s.send_encoded(ROUTE, &mut held, on(60), 0);
        assert!(matches!(
            rx.pop().unwrap().message,
            MidiMessage::NoteOn { channel: 0, .. }
        ));
    }

    #[test]
    fn releasing_ends_what_is_held_and_only_that() {
        let (mut s, mut rx) = sink();
        let mut held = HeldNotes::default();
        s.send(ROUTE, &mut held, on(60), 0);
        s.send(ROUTE, &mut held, on(61), 0);
        s.send(
            ROUTE,
            &mut held,
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.0,
                note_id: 0,
            },
            0,
        );
        while rx.pop().is_ok() {}
        s.release(ROUTE, &mut held, 0);
        let e = rx.pop().unwrap();
        assert!(matches!(
            e.message,
            MidiMessage::NoteOff {
                key: 61,
                channel: 2,
                ..
            }
        ));
        assert!(rx.pop().is_err());
        s.release(ROUTE, &mut held, 0);
        assert!(rx.pop().is_err(), "nothing held any more");
    }
}
