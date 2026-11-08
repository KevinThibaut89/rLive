//! Hardware MIDI input via `midir`. Messages go straight to the audio thread.

use rlive_core::{MidiMessage, TrackId};
use rlive_engine::MidiSender;
use std::sync::{Arc, Mutex};

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
    pub activity: Arc<std::sync::atomic::AtomicBool>,
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
        input.ignore(midir::Ignore::All);
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
        let conn = input
            .connect(
                &port,
                "rlive-in",
                move |_stamp, bytes, _| {
                    let Some(msg) = MidiMessage::from_bytes(bytes) else {
                        return;
                    };
                    routing
                        .activity
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    let mut targets = routing.targets.lock().unwrap().clone();
                    if targets.is_empty() {
                        targets.extend(*routing.selected.lock().unwrap());
                    }
                    if let Some(sender) = routing.sender.lock().unwrap().as_mut() {
                        for t in targets {
                            sender.send(t, msg);
                        }
                    }
                },
                (),
            )
            .map_err(|e| e.to_string())?;
        Ok(Self {
            _connection: Some(conn),
            port_name,
        })
    }
}
