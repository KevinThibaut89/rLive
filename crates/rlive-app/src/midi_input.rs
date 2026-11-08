//! Hardware MIDI input via `midir`. Messages go straight to the audio thread.

use rlive_core::{MidiMessage, MpeDecoder, TrackId};
use rlive_engine::MidiSender;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// System messages kept for later use. Older ones are dropped once it fills.
const MAX_SYSTEM_MESSAGES: usize = 1024;

/// Clock and SysEx bytes from the controller, awaiting whichever milestone consumes them
/// (MIDI clock sync, control surfaces). They can't travel as [`MidiMessage`], which is `Copy`.
#[derive(Default)]
pub struct SystemMessages(Mutex<VecDeque<Vec<u8>>>);

impl SystemMessages {
    fn push(&self, bytes: &[u8]) {
        let mut queue = self.0.lock().unwrap();
        if queue.len() == MAX_SYSTEM_MESSAGES {
            queue.pop_front();
        }
        queue.push_back(bytes.to_vec());
    }

    /// Takes every message received since the last drain.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.0.lock().unwrap().drain(..).collect()
    }
}

/// True for status bytes that belong to the whole system rather than one channel.
fn is_system(bytes: &[u8]) -> bool {
    bytes.first().is_some_and(|&status| status >= 0xf0)
}

pub struct MidiInput {
    _connection: Option<midir::MidiInputConnection<()>>,
    pub port_name: Option<String>,
}

/// State shared with the MIDI callback thread.
#[derive(Clone)]
pub struct MidiRouting {
    pub sender: Arc<Mutex<Option<MidiSender>>>,
    pub targets: Arc<Mutex<Vec<TrackId>>>,
    /// Fallback target when no track is armed (the selected track).
    pub selected: Arc<Mutex<Option<TrackId>>>,
    pub activity: Arc<AtomicBool>,
    pub system: Arc<SystemMessages>,
    /// Treat the controller as MPE: per-note channels become note expression. Off by default,
    /// because an ordinary keyboard on a channel other than 1 is not MPE.
    pub mpe: Arc<AtomicBool>,
}

impl MidiRouting {
    fn handle(&self, bytes: &[u8], decoder: &mut MpeDecoder) {
        if is_system(bytes) {
            self.system.push(bytes);
            return;
        }
        let Some(msg) = MidiMessage::from_bytes(bytes) else {
            return;
        };
        self.activity.store(true, Ordering::Relaxed);
        let mut targets = self.targets.lock().unwrap().clone();
        if targets.is_empty() {
            targets.extend(*self.selected.lock().unwrap());
        }
        let mut sender = self.sender.lock().unwrap();
        let Some(sender) = sender.as_mut() else {
            return;
        };
        let mut send = |m: MidiMessage| {
            for &t in &targets {
                sender.send(t, m);
            }
        };
        if self.mpe.load(Ordering::Relaxed) {
            decoder.feed(msg, &mut send);
        } else {
            send(msg);
        }
    }
}

pub fn port_names() -> Vec<String> {
    let Ok(input) = midir::MidiInput::new("rLive") else {
        return Vec::new();
    };
    input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .collect()
}

impl MidiInput {
    pub fn none() -> Self {
        Self {
            _connection: None,
            port_name: None,
        }
    }

    /// Connect to a port by name (or the first available port).
    pub fn connect(name: Option<&str>, routing: MidiRouting) -> Result<Self, String> {
        let mut input = midir::MidiInput::new("rLive").map_err(|e| e.to_string())?;
        // Active sensing only says the cable is plugged in. Keep everything else.
        input.ignore(midir::Ignore::ActiveSense);
        let ports = input.ports();
        let port = match name {
            Some(n) => ports
                .iter()
                .find(|p| input.port_name(p).map(|x| x == n).unwrap_or(false)),
            None => ports.first(),
        }
        .ok_or("no MIDI input port")?
        .clone();
        let port_name = input.port_name(&port).ok();
        let mut decoder = MpeDecoder::new();
        let conn = input
            .connect(
                &port,
                "rlive-in",
                move |_stamp, bytes, _| routing.handle(bytes, &mut decoder),
                (),
            )
            .map_err(|e| e.to_string())?;
        Ok(Self {
            _connection: Some(conn),
            port_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routing() -> MidiRouting {
        MidiRouting {
            sender: Arc::new(Mutex::new(None)),
            targets: Arc::new(Mutex::new(Vec::new())),
            selected: Arc::new(Mutex::new(None)),
            activity: Arc::new(AtomicBool::new(false)),
            system: Arc::new(SystemMessages::default()),
            mpe: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn clock_and_sysex_are_kept_and_do_not_flash_the_activity_light() {
        let r = routing();
        let mut decoder = MpeDecoder::new();
        r.handle(&[0xf8], &mut decoder);
        r.handle(&[0xf0, 1, 2, 0xf7], &mut decoder);
        assert!(!r.activity.load(Ordering::Relaxed));
        assert_eq!(r.system.drain(), vec![vec![0xf8], vec![0xf0, 1, 2, 0xf7]]);
    }

    #[test]
    fn channel_messages_light_the_activity_indicator_and_stay_out_of_the_system_queue() {
        let r = routing();
        let mut decoder = MpeDecoder::new();
        for bytes in [
            &[0xa0u8, 60, 10][..],
            &[0xc0, 5][..],
            &[0xd0, 30][..],
            &[0x90, 60, 64][..],
        ] {
            r.handle(bytes, &mut decoder);
        }
        assert!(r.activity.load(Ordering::Relaxed));
        assert!(r.system.drain().is_empty());
    }

    #[test]
    fn the_system_queue_keeps_the_newest_when_full() {
        let r = routing();
        let mut decoder = MpeDecoder::new();
        for _ in 0..MAX_SYSTEM_MESSAGES + 3 {
            r.handle(&[0xf8], &mut decoder);
        }
        r.handle(&[0xfa], &mut decoder);
        let drained = r.system.drain();
        assert_eq!(drained.len(), MAX_SYSTEM_MESSAGES);
        assert_eq!(drained.last().unwrap(), &vec![0xfa]);
    }
}
