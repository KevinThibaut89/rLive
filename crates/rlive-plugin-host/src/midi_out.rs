//! What plug-ins of every format send out as MIDI, on its way to the next device.

use rlive_core::{EventQueue, MidiEvent, MidiMessage, MpeDecoder, MpeOutput, MpeZone};

/// Puts a plug-in's output in the queue. With MPE Mode on, what it plays on member channels is
/// turned back into note expression, so it means the same to whatever is next.
#[derive(Default)]
pub(crate) struct MidiOutput {
    decoder: Option<MpeDecoder>,
}

impl MidiOutput {
    /// Set up for an MPE Mode. Doesn't allocate, so the audio thread can do it.
    pub fn new_for(mode: Option<MpeOutput>) -> Self {
        let decoder = mode.map(|output| {
            let mut d = MpeDecoder::new();
            if let MpeOutput::Zone { zone, members } = output {
                d.set_zone(MpeZone::Lower, 0);
                d.set_zone(zone, members);
            }
            d
        });
        Self { decoder }
    }

    pub fn push(&mut self, queue: &mut EventQueue, offset: u32, message: MidiMessage) {
        match self.decoder.as_mut() {
            Some(d) => d.feed(message, &mut |m| queue.push(MidiEvent::new(offset, m))),
            None => queue.push(MidiEvent::new(offset, message)),
        }
    }
}
