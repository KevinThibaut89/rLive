//! The rLive real-time engine.
//!
//! The [`Engine`] lives on the audio thread; the UI talks to it through an [`EngineHandle`]:
//!
//! * UI → audio: [`EngineCommand`]s over a lock-free SPSC ring.
//! * audio → UI: [`Garbage`] (anything the engine let go of, so it is freed off the audio
//!   thread), [`EngineEvent`]s, and atomics in [`shared`] for meters and playheads.
//!
//! The engine never allocates or frees memory while processing.

#![allow(clippy::vec_box)] // nodes are boxed so the audio thread only moves pointers

pub mod clip;
pub mod shared;
pub mod track;

use clip::BlockTime;
pub use clip::{AudioClip, ClipData, MidiClip, Note};
use rlive_core::{
    time::ticks_per_sample, DeviceId, MidiEvent, MidiMessage, Quantize, TimeSignature, TrackId,
    TransportInfo,
};
use shared::EngineShared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
pub use track::{DeviceSlot, MasterNode, MixSettings, TrackNode};

pub const MAX_TRACKS: usize = 256;
const COMMAND_CAPACITY: usize = 4096;
const GARBAGE_CAPACITY: usize = 4096;
const EVENT_CAPACITY: usize = 4096;
const MIDI_CAPACITY: usize = 2048;

/// Sends live MIDI straight to the audio thread (e.g. from a MIDI-input callback thread),
/// bypassing the UI.
pub struct MidiSender {
    producer: rtrb::Producer<(TrackId, MidiMessage)>,
}

impl MidiSender {
    pub fn send(&mut self, track: TrackId, message: MidiMessage) -> bool {
        self.producer.push((track, message)).is_ok()
    }
}

/// Which device chain a command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainTarget {
    Track(TrackId),
    Master,
}

/// Messages from the UI to the audio thread.
pub enum EngineCommand {
    Play,
    Stop,
    SetTempo(f64),
    SetTimeSignature(TimeSignature),
    SetMetronome(bool),
    SetQuantize(Quantize),

    LaunchClip {
        track: TrackId,
        slot: usize,
    },
    StopTrack(TrackId),
    LaunchScene(usize),
    StopAllClips,
    SetClip {
        track: TrackId,
        slot: usize,
        clip: Option<Arc<ClipData>>,
    },

    AddTrack {
        index: usize,
        track: Box<TrackNode>,
    },
    RemoveTrack(TrackId),
    MoveTrack {
        track: TrackId,
        to: usize,
    },
    SetMix {
        track: TrackId,
        mix: MixSettings,
    },
    SetMasterVolume(f32),

    InsertDevice {
        target: ChainTarget,
        index: usize,
        device: Box<DeviceSlot>,
    },
    RemoveDevice {
        target: ChainTarget,
        device: DeviceId,
    },
    MoveDevice {
        target: ChainTarget,
        device: DeviceId,
        to: usize,
    },
    SetBypass {
        target: ChainTarget,
        device: DeviceId,
        bypass: bool,
    },
    SetParam {
        target: ChainTarget,
        device: DeviceId,
        param: u32,
        value: f64,
    },

    /// Live MIDI (keyboard input) for a track.
    Midi {
        track: TrackId,
        message: MidiMessage,
    },

    /// Replace the whole session (project load / sample-rate change).
    ReplaceAll {
        tracks: Box<Vec<Box<TrackNode>>>,
        master: Box<MasterNode>,
    },
}

/// Things the engine is done with. They are dropped on the UI thread.
pub enum Garbage {
    Track(Box<TrackNode>),
    Tracks(Box<Vec<Box<TrackNode>>>),
    Master(Box<MasterNode>),
    Device(Box<DeviceSlot>),
    Clip(Arc<ClipData>),
}

/// Notifications from the audio thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineEvent {
    /// A processor changed one of its own parameters (e.g. from a plugin editor).
    ParamChanged {
        target: ChainTarget,
        device: DeviceId,
        param: u32,
        value: f64,
    },
}

/// UI-side handle to a running engine.
pub struct EngineHandle {
    commands: rtrb::Producer<EngineCommand>,
    garbage: rtrb::Consumer<Garbage>,
    events: rtrb::Consumer<EngineEvent>,
    pub shared: Arc<EngineShared>,
    pub sample_rate: f64,
    pub max_block: usize,
    midi: Option<MidiSender>,
}

impl EngineHandle {
    /// Take the (single) live-MIDI sender. Returns `None` if it was already taken.
    pub fn take_midi_sender(&mut self) -> Option<MidiSender> {
        self.midi.take()
    }

    /// Queue a command. Returns it back if the queue is full.
    pub fn send(&mut self, cmd: EngineCommand) -> Result<(), EngineCommand> {
        self.commands
            .push(cmd)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Free everything the audio thread has released. Call regularly from the UI thread.
    pub fn collect_garbage(&mut self) -> usize {
        let mut n = 0;
        while let Ok(g) = self.garbage.pop() {
            drop(g);
            n += 1;
        }
        n
    }

    pub fn poll_event(&mut self) -> Option<EngineEvent> {
        self.events.pop().ok()
    }
}

struct Metronome {
    enabled: bool,
    phase: f32,
    remaining: usize,
    freq: f32,
    amp: f32,
}

impl Metronome {
    fn render(&mut self, time: BlockTime, sig: TimeSignature, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            return;
        }
        let beat = sig.ticks_per_beat() as f64;
        let click_len = (time.sample_rate * 0.03) as usize;
        let first = (time.t0 / beat).ceil();
        let mut next_click = first * beat;
        let t1 = time.t1();
        let mut start = 0;
        loop {
            let click_at = if next_click < t1 {
                Some(time.offset(next_click))
            } else {
                None
            };
            let until = click_at.unwrap_or(time.frames);
            self.write(
                left,
                right,
                start,
                until,
                time.sample_rate as f32,
                click_len,
            );
            match click_at {
                Some(off) => {
                    let downbeat = ((next_click / beat).round() as u64)
                        .is_multiple_of(sig.numerator.max(1) as u64);
                    self.freq = if downbeat { 1760.0 } else { 880.0 };
                    self.amp = if downbeat { 0.5 } else { 0.3 };
                    self.phase = 0.0;
                    self.remaining = click_len;
                    start = off;
                    next_click += beat;
                }
                None => break,
            }
        }
    }

    fn write(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        from: usize,
        to: usize,
        sr: f32,
        len: usize,
    ) {
        for i in from..to {
            if self.remaining == 0 {
                return;
            }
            let env = self.remaining as f32 / len.max(1) as f32;
            let s = (self.phase * std::f32::consts::TAU).sin() * self.amp * env * env;
            self.phase = (self.phase + self.freq / sr).fract();
            left[i] += s;
            right[i] += s;
            self.remaining -= 1;
        }
    }
}

/// The audio-thread half of the engine.
pub struct Engine {
    sample_rate: f64,
    max_block: usize,
    tracks: Vec<Box<TrackNode>>,
    master: Box<MasterNode>,

    playing: bool,
    tempo: f64,
    signature: TimeSignature,
    quantize: Quantize,
    tick: f64,
    sample_pos: u64,
    metronome: Metronome,
    smooth_coeff: f32,

    commands: rtrb::Consumer<EngineCommand>,
    garbage: rtrb::Producer<Garbage>,
    events: rtrb::Producer<EngineEvent>,
    midi: rtrb::Consumer<(TrackId, MidiMessage)>,
    shared: Arc<EngineShared>,
}

/// Create an engine and its UI handle.
pub fn create_engine(sample_rate: f64, max_block: usize) -> (Engine, EngineHandle) {
    let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (gb_tx, gb_rx) = rtrb::RingBuffer::new(GARBAGE_CAPACITY);
    let (ev_tx, ev_rx) = rtrb::RingBuffer::new(EVENT_CAPACITY);
    let (midi_tx, midi_rx) = rtrb::RingBuffer::new(MIDI_CAPACITY);
    let shared = Arc::new(EngineShared::default());
    shared.sample_rate.store(sample_rate);
    let engine = Engine {
        sample_rate,
        max_block,
        tracks: Vec::with_capacity(MAX_TRACKS),
        master: Box::new(MasterNode::new(max_block)),
        playing: false,
        tempo: 120.0,
        signature: TimeSignature::default(),
        quantize: Quantize::Bar,
        tick: 0.0,
        sample_pos: 0,
        metronome: Metronome {
            enabled: false,
            phase: 0.0,
            remaining: 0,
            freq: 880.0,
            amp: 0.3,
        },
        // ~5 ms one-pole smoothing.
        smooth_coeff: 1.0 - (-1.0 / (0.005 * sample_rate as f32)).exp(),
        commands: cmd_rx,
        garbage: gb_tx,
        events: ev_tx,
        midi: midi_rx,
        shared: shared.clone(),
    };
    let handle = EngineHandle {
        commands: cmd_tx,
        garbage: gb_rx,
        events: ev_rx,
        shared,
        sample_rate,
        max_block,
        midi: Some(MidiSender { producer: midi_tx }),
    };
    (engine, handle)
}

fn retire(garbage: &mut rtrb::Producer<Garbage>, g: Garbage) {
    if let Err(rtrb::PushError::Full(g)) = garbage.push(g) {
        // Never free on the audio thread; leaking is the lesser evil.
        std::mem::forget(g);
    }
}

impl Engine {
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn max_block(&self) -> usize {
        self.max_block
    }

    fn transport(&self) -> TransportInfo {
        TransportInfo {
            playing: self.playing,
            tempo: self.tempo,
            time_signature: self.signature,
            tick: self.tick,
            sample_pos: self.sample_pos,
            sample_rate: self.sample_rate,
        }
    }

    fn track_mut(&mut self, id: TrackId) -> Option<&mut TrackNode> {
        self.tracks
            .iter_mut()
            .find(|t| t.id == id)
            .map(|b| &mut **b)
    }

    fn chain_mut(&mut self, target: ChainTarget) -> Option<&mut Vec<Box<DeviceSlot>>> {
        match target {
            ChainTarget::Master => Some(&mut self.master.devices),
            ChainTarget::Track(id) => self.track_mut(id).map(|t| &mut t.devices),
        }
    }

    fn launch_point(&self) -> f64 {
        self.quantize.next_boundary(self.tick, self.signature)
    }

    fn start_transport(&mut self) {
        if !self.playing {
            self.playing = true;
            self.tick = 0.0;
            self.sample_pos = 0;
        }
    }

    fn launch(&mut self, track_idx: usize, slot: usize) {
        self.start_transport();
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        let has_clip = matches!(track.slots.get(slot), Some(Some(_)));
        track.player.pending = Some(if has_clip {
            clip::Pending::Launch { slot, at }
        } else {
            clip::Pending::Stop { at }
        });
    }

    fn stop_track(&mut self, track_idx: usize) {
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        if track.player.playing.is_some() || track.player.pending.is_some() {
            track.player.pending = Some(clip::Pending::Stop { at });
        }
    }

    fn stop_transport(&mut self) {
        self.playing = false;
        self.tick = 0.0;
        self.sample_pos = 0;
        for t in self.tracks.iter_mut() {
            t.player.pending = None;
            t.player.release_all(0, &mut t.live);
            if let Some(p) = t.player.playing.take() {
                retire(&mut self.garbage, Garbage::Clip(p.clip));
            }
            for d in t.devices.iter_mut() {
                d.processor.reset();
            }
        }
        for d in self.master.devices.iter_mut() {
            d.processor.reset();
        }
    }

    fn handle_command(&mut self, cmd: EngineCommand) {
        use EngineCommand as C;
        match cmd {
            C::Play => self.start_transport(),
            C::Stop => self.stop_transport(),
            C::SetTempo(t) => self.tempo = t.clamp(20.0, 999.0),
            C::SetTimeSignature(s) => self.signature = s,
            C::SetMetronome(on) => self.metronome.enabled = on,
            C::SetQuantize(q) => self.quantize = q,
            C::LaunchClip { track, slot } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    self.launch(i, slot);
                }
            }
            C::StopTrack(track) => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    self.stop_track(i);
                }
            }
            C::LaunchScene(slot) => {
                for i in 0..self.tracks.len() {
                    self.launch(i, slot);
                }
            }
            C::StopAllClips => {
                for i in 0..self.tracks.len() {
                    self.stop_track(i);
                }
            }
            C::SetClip { track, slot, clip } => {
                let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) else {
                    if let Some(c) = clip {
                        retire(&mut self.garbage, Garbage::Clip(c));
                    }
                    return;
                };
                while t.slots.len() <= slot && t.slots.len() < t.slots.capacity() {
                    t.slots.push(None);
                }
                let Some(entry) = t.slots.get_mut(slot) else {
                    if let Some(c) = clip {
                        retire(&mut self.garbage, Garbage::Clip(c));
                    }
                    return;
                };
                // Live-update a playing clip (e.g. edits in the piano roll).
                if let Some(p) = t.player.playing.as_mut() {
                    if p.slot == slot {
                        match &clip {
                            Some(new) => {
                                let old = std::mem::replace(&mut p.clip, new.clone());
                                retire(&mut self.garbage, Garbage::Clip(old));
                                t.player.release_all(0, &mut t.live);
                            }
                            None => {
                                if let Some(p) = t.player.playing.take() {
                                    retire(&mut self.garbage, Garbage::Clip(p.clip));
                                }
                                t.player.release_all(0, &mut t.live);
                            }
                        }
                    }
                }
                if let Some(old) = std::mem::replace(entry, clip) {
                    retire(&mut self.garbage, Garbage::Clip(old));
                }
            }
            C::AddTrack { index, track } => {
                if self.tracks.len() < self.tracks.capacity() {
                    let idx = index.min(self.tracks.len());
                    self.tracks.insert(idx, track);
                } else {
                    retire(&mut self.garbage, Garbage::Track(track));
                }
            }
            C::RemoveTrack(id) => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == id) {
                    let mut t = self.tracks.remove(i);
                    for d in t.devices.iter_mut() {
                        d.processor.stop_processing();
                    }
                    if let Some(p) = t.player.playing.take() {
                        retire(&mut self.garbage, Garbage::Clip(p.clip));
                    }
                    retire(&mut self.garbage, Garbage::Track(t));
                }
            }
            C::MoveTrack { track, to } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    let t = self.tracks.remove(i);
                    let to = to.min(self.tracks.len());
                    self.tracks.insert(to, t);
                }
            }
            C::SetMix { track, mix } => {
                if let Some(t) = self.track_mut(track) {
                    t.mix = mix;
                }
            }
            C::SetMasterVolume(db) => self.master.volume_db = db,
            C::InsertDevice {
                target,
                index,
                device,
            } => match self.chain_mut(target) {
                Some(chain) if chain.len() < chain.capacity() => {
                    let idx = index.min(chain.len());
                    chain.insert(idx, device);
                }
                _ => retire(&mut self.garbage, Garbage::Device(device)),
            },
            C::RemoveDevice { target, device } => {
                let removed = self.chain_mut(target).and_then(|chain| {
                    let i = chain.iter().position(|d| d.id == device)?;
                    Some(chain.remove(i))
                });
                if let Some(mut slot) = removed {
                    slot.processor.stop_processing();
                    retire(&mut self.garbage, Garbage::Device(slot));
                }
            }
            C::MoveDevice { target, device, to } => {
                if let Some(chain) = self.chain_mut(target) {
                    if let Some(i) = chain.iter().position(|d| d.id == device) {
                        let d = chain.remove(i);
                        let to = to.min(chain.len());
                        chain.insert(to, d);
                    }
                }
            }
            C::SetBypass {
                target,
                device,
                bypass,
            } => {
                if let Some(chain) = self.chain_mut(target) {
                    if let Some(d) = chain.iter_mut().find(|d| d.id == device) {
                        d.bypass = bypass;
                        if bypass {
                            d.processor.reset();
                        }
                    }
                }
            }
            C::SetParam {
                target,
                device,
                param,
                value,
            } => {
                if let Some(chain) = self.chain_mut(target) {
                    if let Some(d) = chain.iter_mut().find(|d| d.id == device) {
                        d.processor.set_param(param, value);
                    }
                }
            }
            C::Midi { track, message } => {
                if let Some(t) = self.track_mut(track) {
                    if t.live.len() < t.live.capacity() {
                        t.live.push(MidiEvent::new(0, message));
                    }
                }
            }
            C::ReplaceAll {
                mut tracks,
                mut master,
            } => {
                for t in self.tracks.iter_mut() {
                    for d in t.devices.iter_mut() {
                        d.processor.stop_processing();
                    }
                    if let Some(p) = t.player.playing.take() {
                        retire(&mut self.garbage, Garbage::Clip(p.clip));
                    }
                }
                for d in self.master.devices.iter_mut() {
                    d.processor.stop_processing();
                }
                std::mem::swap(&mut self.tracks, &mut *tracks);
                std::mem::swap(&mut self.master, &mut master);
                retire(&mut self.garbage, Garbage::Tracks(tracks));
                retire(&mut self.garbage, Garbage::Master(master));
                self.playing = false;
                self.tick = 0.0;
                self.sample_pos = 0;
            }
        }
    }

    /// Render `left.len()` frames of output. Handles any buffer size by splitting into
    /// `max_block` chunks.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let started = std::time::Instant::now();
        let total = left.len().min(right.len());
        let mut done = 0;
        while done < total {
            let n = (total - done).min(self.max_block);
            self.process_block(&mut left[done..done + n], &mut right[done..done + n]);
            done += n;
        }
        if total > 0 {
            let budget = total as f64 / self.sample_rate;
            let load = (started.elapsed().as_secs_f64() / budget) as f32;
            let prev = self.shared.cpu_load.load();
            self.shared.cpu_load.store(prev + (load - prev) * 0.1);
        }
    }

    fn process_block(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        while let Ok(cmd) = self.commands.pop() {
            self.handle_command(cmd);
        }
        while let Ok((track, message)) = self.midi.pop() {
            if let Some(t) = self.track_mut(track) {
                if t.live.len() < t.live.capacity() {
                    t.live.push(MidiEvent::new(0, message));
                }
            }
        }
        let frames = out_l.len();
        let transport = self.transport();
        let time = BlockTime {
            t0: self.tick,
            ticks_per_sample: ticks_per_sample(self.tempo, self.sample_rate),
            frames,
            sample_rate: self.sample_rate,
            tempo: self.tempo,
        };

        let garbage = &mut self.garbage;
        let events = &mut self.events;
        let mut retire_clip = |c: Arc<ClipData>| retire(garbage, Garbage::Clip(c));
        let mut on_param = |t: TrackId, d: DeviceId, p: u32, v: f64| {
            let _ = events.push(EngineEvent::ParamChanged {
                target: ChainTarget::Track(t),
                device: d,
                param: p,
                value: v,
            });
        };
        for t in self.tracks.iter_mut() {
            t.render(time, &transport, &mut retire_clip, &mut on_param);
        }

        let any_solo = self.tracks.iter().any(|t| t.mix.solo);
        let ml = &mut self.master.left[..frames];
        let mr = &mut self.master.right[..frames];
        ml.fill(0.0);
        mr.fill(0.0);
        for t in self.tracks.iter_mut() {
            let audible = !t.mix.mute && (!any_solo || t.mix.solo);
            t.apply_mix(frames, audible, self.smooth_coeff);
            let (l, r) = t.output(frames);
            for i in 0..frames {
                ml[i] += l[i];
                mr[i] += r[i];
            }
        }
        if transport.playing {
            self.metronome.render(time, self.signature, ml, mr);
        }
        let mut on_master_param = |d: DeviceId, p: u32, v: f64| {
            let _ = events.push(EngineEvent::ParamChanged {
                target: ChainTarget::Master,
                device: d,
                param: p,
                value: v,
            });
        };
        self.master.finish(
            frames,
            &transport,
            &self.shared.master_meter,
            self.smooth_coeff,
            &mut on_master_param,
        );
        out_l.copy_from_slice(&self.master.left[..frames]);
        out_r.copy_from_slice(&self.master.right[..frames]);

        if self.playing {
            self.tick = time.t1();
            self.sample_pos += frames as u64;
        }
        self.shared.playing.store(self.playing, Ordering::Relaxed);
        self.shared.tick.store(self.tick);
        self.shared.tempo.store(self.tempo);
    }
}

/// Does nothing. Used as a placeholder when moving device slots in and out of boxes.
pub struct NullProcessor;

impl rlive_core::Processor for NullProcessor {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, _: &mut rlive_core::ProcessContext) {}
    fn set_param(&mut self, _: u32, _: f64) {}
}
