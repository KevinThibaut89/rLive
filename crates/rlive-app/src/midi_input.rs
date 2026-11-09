//! Hardware MIDI via `midir`. Unmapped messages go straight to the audio thread; mapped ones are
//! decoded right here on the MIDI thread by [`RemotePort`] and go straight there too, with the UI
//! told afterwards so the project can catch up.

use rlive_core::param::ParamRef;
use rlive_core::{MidiMessage, MpeDecoder, TrackId};
use rlive_engine::{CellIx, MidiSender, RemoteCmd, TrackRef};
use rlive_project::MapTarget;
use rlive_remote::{
    CellAddr, FeedbackSink, Gesture, MidiEvent, RemoteSink, RemoteTable, TakeoverMode,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::Thread;

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

    /// Takes the System Exclusive messages, for the control surfaces. Clock and the rest stay
    /// where they are for the sync code (A16).
    pub fn take_sysex(&self) -> Vec<Vec<u8>> {
        let mut queue = self.0.lock().unwrap();
        let mut taken = Vec::new();
        queue.retain_mut(|m| {
            if m.first() == Some(&0xf0) {
                taken.push(std::mem::take(m));
                false
            } else {
                true
            }
        });
        taken
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
    _connections: Vec<midir::MidiInputConnection<()>>,
    /// The ports open, by name.
    pub port_names: Vec<String>,
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

/// The Track, Sync and Remote switches of one port, flipped from the settings at any time.
#[derive(Debug, Default)]
pub struct PortSwitches {
    pub track: AtomicBool,
    pub sync: AtomicBool,
    pub remote: AtomicBool,
}

impl PortSwitches {
    pub fn new(track: bool, sync: bool, remote: bool) -> Self {
        Self {
            track: AtomicBool::new(track),
            sync: AtomicBool::new(sync),
            remote: AtomicBool::new(remote),
        }
    }
}

/// Lights for the Control Bar: each is set when something passes and cleared when it's read.
#[derive(Debug, Default)]
pub struct MidiLights {
    pub track_in: AtomicBool,
    pub track_out: AtomicBool,
    pub remote_in: AtomicBool,
    pub remote_out: AtomicBool,
    pub sync_in: AtomicBool,
    pub sync_out: AtomicBool,
}

/// What the ports and the UI thread share.
#[derive(Debug, Default)]
pub struct RemoteShared {
    /// A control is waiting for its mapping: everything goes to it.
    pub learning: AtomicBool,
    pub takeover: AtomicU8,
    pub lights: MidiLights,
    /// Woken when the UI has something to do and might be asleep.
    waker: OnceLock<Thread>,
}

impl RemoteShared {
    fn wake(&self) {
        if let Some(t) = self.waker.get() {
            t.unpark();
        }
    }

    /// Runs `repaint` every time the MIDI thread has something for the UI. It runs on a thread
    /// of its own, because asking egui for a repaint can wait for a frame in progress.
    pub fn set_waker(self: &Arc<Self>, repaint: impl Fn() + Send + 'static) {
        let thread = std::thread::Builder::new()
            .name("rlive-midi-waker".into())
            .spawn(move || loop {
                std::thread::park();
                repaint();
            });
        if let Ok(t) = thread {
            let _ = self.waker.set(t.thread().clone());
        }
    }
}

/// What the UI thread is told about mapped input. `Copy`, so a ring can carry it unbounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemoteEvent {
    /// A parameter moved; if `direct`, the engine has it already.
    Param {
        target: ParamRef,
        value: f64,
        direct: bool,
    },
    SlotDown {
        track: TrackId,
        slot: u32,
        velocity: f32,
        transpose: i8,
        direct: bool,
    },
    SlotUp {
        track: TrackId,
        slot: u32,
        direct: bool,
    },
    Scene {
        scene: u32,
        direct: bool,
    },
    Gesture {
        target: MapTarget,
        gesture: Gesture,
    },
    /// Raw input while a control waits for its mapping.
    Learn(MidiEvent),
    /// A message for a control surface script, from a control it claimed.
    Surface {
        slot: u8,
        bytes: [u8; 3],
    },
}

/// Holds one port's end of everything, on its MIDI callback thread.
pub struct RemotePort {
    port: u32,
    routing: MidiRouting,
    switches: Arc<PortSwitches>,
    shared: Arc<RemoteShared>,
    table: Arc<RemoteTable>,
    tables: rtrb::Consumer<Arc<RemoteTable>>,
    events: rtrb::Producer<RemoteEvent>,
    decoder: MpeDecoder,
}

/// Turns what a mapping did into engine commands and UI events. The UI always hears first, so
/// that a hardware gesture is known before the engine reports any change it causes.
struct PortSink<'a> {
    events: &'a mut rtrb::Producer<RemoteEvent>,
    routing: &'a MidiRouting,
    wake: bool,
}

impl PortSink<'_> {
    fn ui(&mut self, event: RemoteEvent) {
        if self.events.push(event).is_err() {
            log::warn!("remote event queue full");
        }
        self.wake = true;
    }

    fn engine(&mut self, cmd: RemoteCmd) {
        // Only ever held to push onto a ring, by this port or another.
        if let Some(sender) = self.routing.sender.lock().unwrap().as_mut() {
            sender.send_remote(cmd);
        }
    }
}

impl RemoteSink for PortSink<'_> {
    fn param(&mut self, target: ParamRef, addr: Option<CellAddr>, value: f64) {
        self.ui(RemoteEvent::Param {
            target,
            value,
            direct: addr.is_some(),
        });
        if let Some(a) = addr {
            self.engine(RemoteCmd::SetParam {
                track: a.track.map_or(TrackRef::Master, TrackRef::Track),
                cell: CellIx(a.cell),
                value,
            });
        }
    }

    fn slot_down(&mut self, track: TrackId, slot: u32, velocity: f32, transpose: i8, direct: bool) {
        self.ui(RemoteEvent::SlotDown {
            track,
            slot,
            velocity,
            transpose,
            direct,
        });
        if direct {
            self.engine(RemoteCmd::ClipDown {
                track,
                slot,
                velocity,
                transpose,
            });
        }
    }

    fn slot_up(&mut self, track: TrackId, slot: u32, direct: bool) {
        self.ui(RemoteEvent::SlotUp {
            track,
            slot,
            direct,
        });
        if direct {
            self.engine(RemoteCmd::ClipUp { track, slot });
        }
    }

    fn scene(&mut self, scene: u32, direct: bool) {
        self.ui(RemoteEvent::Scene { scene, direct });
        if direct {
            self.engine(RemoteCmd::LaunchScene(scene));
        }
    }

    fn gesture(&mut self, target: MapTarget, gesture: Gesture) {
        self.ui(RemoteEvent::Gesture { target, gesture });
    }

    fn surface(&mut self, slot: u8, bytes: [u8; 3]) {
        self.ui(RemoteEvent::Surface { slot, bytes });
    }
}

impl RemotePort {
    /// One message from this port. Never waits on the UI thread and never allocates.
    pub fn handle(&mut self, bytes: &[u8]) {
        while let Ok(table) = self.tables.pop() {
            self.table = table;
        }
        let shared = &self.shared;
        if is_system(bytes) {
            let is_sysex = bytes.first() == Some(&0xf0);
            let remote = self.switches.remote.load(Ordering::Relaxed);
            let sync = self.switches.sync.load(Ordering::Relaxed);
            if (is_sysex && remote) || (!is_sysex && sync) {
                if !is_sysex {
                    shared.lights.sync_in.store(true, Ordering::Relaxed);
                }
                self.routing.system.push(bytes);
                shared.wake();
            }
            return;
        }
        if shared.learning.load(Ordering::Relaxed) && self.switches.remote.load(Ordering::Relaxed) {
            if let Some(event) = MidiEvent::parse(bytes) {
                shared.lights.remote_in.store(true, Ordering::Relaxed);
                let _ = self.events.push(RemoteEvent::Learn(event));
                shared.wake();
                return;
            }
        }
        let mode = TakeoverMode::from_u8(shared.takeover.load(Ordering::Relaxed));
        let mut sink = PortSink {
            events: &mut self.events,
            routing: &self.routing,
            wake: false,
        };
        let mapped = self.table.process(self.port, bytes, mode, &mut sink);
        let wake = sink.wake;
        if mapped {
            shared.lights.remote_in.store(true, Ordering::Relaxed);
            if wake {
                shared.wake();
            }
            return;
        }
        if self.switches.track.load(Ordering::Relaxed) {
            shared.lights.track_in.store(true, Ordering::Relaxed);
            self.routing.handle(bytes, &mut self.decoder);
        }
    }
}

/// The UI thread's end of every port.
#[derive(Default)]
pub struct RemoteHub {
    pub shared: Arc<RemoteShared>,
    ports: Vec<HubPort>,
    table: Option<Arc<RemoteTable>>,
}

struct HubPort {
    tables: rtrb::Producer<Arc<RemoteTable>>,
    events: rtrb::Consumer<RemoteEvent>,
    /// Hasn't been given the latest table yet.
    behind: bool,
}

const TABLE_QUEUE: usize = 8;
const EVENT_QUEUE: usize = 4096;

impl RemoteHub {
    /// A new port's end of the shared state. `port` is the number mappings' port masks use.
    pub fn open_port(
        &mut self,
        port: u32,
        routing: MidiRouting,
        switches: Arc<PortSwitches>,
    ) -> RemotePort {
        let (table_tx, table_rx) = rtrb::RingBuffer::new(TABLE_QUEUE);
        let (event_tx, event_rx) = rtrb::RingBuffer::new(EVENT_QUEUE);
        self.ports.push(HubPort {
            tables: table_tx,
            events: event_rx,
            behind: false,
        });
        RemotePort {
            port,
            routing,
            switches,
            shared: self.shared.clone(),
            table: self
                .table
                .clone()
                .unwrap_or_else(|| Arc::new(RemoteTable::empty())),
            tables: table_rx,
            events: event_tx,
            decoder: MpeDecoder::new(),
        }
    }

    /// Forget every port, whose callbacks are about to be dropped.
    pub fn close_ports(&mut self) {
        self.ports.clear();
    }

    /// Hands every port a new table. A port too busy to take it gets the next one.
    pub fn publish(&mut self, table: Arc<RemoteTable>) {
        for p in &mut self.ports {
            p.behind = p.tables.push(table.clone()).is_err();
        }
        self.table = Some(table);
    }

    /// The table the ports were last handed.
    pub fn table(&self) -> Option<&Arc<RemoteTable>> {
        self.table.as_ref()
    }

    /// Everything the ports have said since the last call.
    pub fn poll(&mut self) -> Vec<RemoteEvent> {
        let mut events = Vec::new();
        for p in &mut self.ports {
            if p.behind {
                if let Some(table) = &self.table {
                    p.behind = p.tables.push(table.clone()).is_err();
                }
            }
            while let Ok(e) = p.events.pop() {
                events.push(e);
            }
        }
        events
    }
}

/// The open output ports, by the same numbers feedback uses. Used from the UI thread only.
pub struct OutputBank {
    outputs: Vec<Option<midir::MidiOutputConnection>>,
    lights: Arc<RemoteShared>,
}

impl OutputBank {
    pub fn new(lights: Arc<RemoteShared>) -> Self {
        Self {
            outputs: Vec::new(),
            lights,
        }
    }

    pub fn push(&mut self, c: Option<midir::MidiOutputConnection>) {
        self.outputs.push(c);
    }
}

impl FeedbackSink for OutputBank {
    fn send(&mut self, port: u32, bytes: &[u8]) {
        if let Some(Some(c)) = self.outputs.get_mut(port as usize) {
            if c.send(bytes).is_ok() {
                self.lights.lights.remote_out.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// The names of the output ports.
pub fn output_names() -> Vec<String> {
    let Ok(out) = midir::MidiOutput::new("rLive") else {
        return Vec::new();
    };
    out.ports()
        .iter()
        .filter_map(|p| out.port_name(p).ok())
        .collect()
}

/// Opens an output port by name, for feedback and control surfaces.
pub fn connect_output(name: &str) -> Result<midir::MidiOutputConnection, String> {
    let out = midir::MidiOutput::new("rLive").map_err(|e| e.to_string())?;
    let ports = out.ports();
    let port = ports
        .iter()
        .find(|p| out.port_name(p).map(|x| x == name).unwrap_or(false))
        .ok_or("no MIDI output port")?;
    out.connect(port, "rlive-out").map_err(|e| e.to_string())
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
            _connections: Vec::new(),
            port_names: Vec::new(),
        }
    }

    /// Connects to one port by name, reading it through `port`.
    pub fn connect_port(&mut self, name: &str, mut port: RemotePort) -> Result<(), String> {
        let mut input = midir::MidiInput::new("rLive").map_err(|e| e.to_string())?;
        // Active sensing only says the cable is plugged in. Keep everything else.
        input.ignore(midir::Ignore::ActiveSense);
        let ports = input.ports();
        let found = ports
            .iter()
            .find(|p| input.port_name(p).map(|x| x == name).unwrap_or(false))
            .ok_or("no MIDI input port")?
            .clone();
        let conn = input
            .connect(
                &found,
                "rlive-in",
                move |_stamp, bytes, _| port.handle(bytes),
                (),
            )
            .map_err(|e| e.to_string())?;
        self._connections.push(conn);
        self.port_names.push(name.to_string());
        Ok(())
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
