//! MIDI leaving the engine for hardware ports.
//!
//! Everything bound for a port goes down one ring: a track's Ext. Out and an External Instrument
//! alike. The audio thread only ever pushes [`HwMidi`]s onto it; whatever owns the ports reads
//! them from another thread and sends each at its time, so nothing here can block.
//!
//! While tracks render on several threads the ring has a single producer to keep, so a track
//! renders into a [`MidiOutBuffer`] of its own, and when every track has finished the block's
//! buffers are merged in sample order, track by track, in plan order, and sent ([`MidiMerge`]).

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
    fn track(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { channel, key, .. } => self.note(channel, key, true),
            MidiMessage::NoteOff { channel, key, .. } => self.note(channel, key, false),
            _ => {}
        }
    }

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

/// Where outgoing MIDI goes on the audio thread or a worker. [`MidiOutSink`] is the ring itself;
/// [`MidiOutBuffer`] holds a track's messages until the block's end.
pub trait MidiPush {
    /// Send a message exactly as it is, `offset` frames into the block.
    fn push_midi(&mut self, port: u16, message: MidiMessage, offset: u32);

    /// Send a message on the route's channel. Per-note expression can't be said on one channel of
    /// MIDI 1.0, so it stays home unless an MPE encoder has spread the notes out first.
    fn send(
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
        held.track(message);
        self.push_midi(route.port, message, offset);
    }

    /// Send what an MPE encoder made: its channels are the ones it chose.
    fn send_encoded(
        &mut self,
        route: PortRoute,
        held: &mut HeldNotes,
        message: MidiMessage,
        offset: u32,
    ) {
        held.track(message);
        self.push_midi(route.port, message, offset);
    }

    /// End every note in `held`.
    fn release(&mut self, route: PortRoute, held: &mut HeldNotes, offset: u32) {
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
                self.push_midi(route.port, message, offset);
            }
            held.0[channel as usize] = 0;
        }
    }
}

/// A MIDI message a track made this block, on its way to the merge.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingMidi {
    pub(crate) port: u16,
    pub(crate) message: MidiMessage,
    pub(crate) offset: u32,
}

/// The MIDI one track made this block. Fixed size; what doesn't fit is dropped, as a full ring is.
#[derive(Default)]
pub struct MidiOutBuffer(Vec<PendingMidi>);

impl MidiOutBuffer {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    pub(crate) fn drain(&mut self) -> std::vec::Drain<'_, PendingMidi> {
        self.0.drain(..)
    }
}

impl MidiPush for MidiOutBuffer {
    fn push_midi(&mut self, port: u16, message: MidiMessage, offset: u32) {
        if self.0.len() < self.0.capacity() {
            self.0.push(PendingMidi {
                port,
                message,
                offset,
            });
        }
    }
}

/// Room to sort a block's MIDI on its way to the ring. Fixed size: what doesn't fit is dropped.
pub(crate) struct MidiMerge(Vec<(u32, u32, PendingMidi)>);

impl MidiMerge {
    pub(crate) fn new(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    /// One more message, in the order the engine rendered it.
    pub(crate) fn add(&mut self, message: PendingMidi) {
        if self.0.len() < self.0.capacity() {
            let seq = self.0.len() as u32;
            self.0.push((message.offset, seq, message));
        }
    }

    /// Hand the block's MIDI to the ring, in sample order, then in the order it was made.
    pub(crate) fn drain_into(&mut self, sink: &mut MidiOutSink) {
        // Unstable is fine: the sequence number makes every key distinct, and it doesn't allocate.
        self.0
            .sort_unstable_by_key(|&(offset, seq, _)| (offset, seq));
        for (_, _, m) in self.0.drain(..) {
            sink.push_midi(m.port, m.message, m.offset);
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
    fn push_ring(&mut self, port: u16, message: MidiMessage, offset: u32) {
        let _ = self.queue.push(HwMidi {
            port,
            block: self.block,
            offset: f64::from(offset) / self.sample_rate,
            message,
        });
    }
}

impl MidiPush for MidiOutSink {
    fn push_midi(&mut self, port: u16, message: MidiMessage, offset: u32) {
        self.push_ring(port, message, offset);
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
    fn the_merge_puts_a_blocks_messages_in_sample_order() {
        let (mut s, mut rx) = sink();
        let mut merge = MidiMerge::new(8);
        for (offset, key) in [(300u32, 60u8), (100, 61), (100, 62), (200, 63)] {
            merge.add(PendingMidi {
                port: 3,
                message: on(key),
                offset,
            });
        }
        merge.drain_into(&mut s);
        let mut out = Vec::new();
        while let Ok(e) = rx.pop() {
            let MidiMessage::NoteOn { key, .. } = e.message else {
                panic!("not a note");
            };
            out.push(key);
        }
        assert_eq!(out, [61, 62, 63, 60], "by offset, then as they were made");
    }

    #[test]
    fn the_merge_drops_what_it_has_no_room_for() {
        let mut merge = MidiMerge::new(2);
        for offset in 0..5 {
            merge.add(PendingMidi {
                port: 0,
                message: on(60),
                offset,
            });
        }
        assert_eq!(merge.0.len(), 2);
    }

    #[test]
    fn a_later_block_starts_later() {
        let (mut s, mut rx) = sink();
        let start = Instant::now();
        s.begin_buffer(start);
        s.begin_block(48);
        s.push_midi(1, on(60), 0);
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
