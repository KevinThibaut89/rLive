//! The session: the project model plus the live objects that mirror it in the engine.
//!
//! Every edit goes through [`Session`] so the project (what gets saved) and the engine (what
//! you hear) never drift apart.

use crate::peaks::Peaks;
use crate::recording::Recording;
use rlive_core::{
    ArrClipId, DeviceId, MidiMessage, ParamInfo, PluginFormat, Processor, Quantize, TimeSignature,
    TrackId,
};
use rlive_devices::{DeviceInit, DeviceRole, SampleBuffer};
use rlive_engine::shared::TrackShared;
use rlive_engine::{
    ArrangementLane, AudioClip, ChainTarget, ClipData, DeviceSlot, EngineCommand, EngineEvent,
    EngineHandle, LaneClip, MasterNode, MidiClip, MixSettings, Note, NullProcessor, TrackNode,
    MAX_TRACKS,
};
use rlive_plugin_host::scanner::PluginCache;
use rlive_plugin_host::{HostNotification, PluginDescriptor, PluginHandle};
use rlive_project::history::{History, ParkedMix, Snapshot, TrackSnap};
use rlive_project::{
    ArrangementClip, Clip, ClipContent, Device, DeviceKind, Project, Track, TrackKind,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// UI-side state of one device in a chain.
pub struct DeviceRuntime {
    pub params: Vec<ParamInfo>,
    pub plugin: Option<Box<dyn PluginHandle>>,
    pub error: Option<String>,
    pub role: DeviceRole,
}

impl DeviceRuntime {
    pub fn format_value(&self, p: &ParamInfo, value: f64) -> String {
        self.plugin
            .as_ref()
            .and_then(|h| h.format_value(p.id, value))
            .unwrap_or_else(|| p.format(value))
    }
}

/// Loads audio files once and shares them.
#[derive(Default)]
pub struct SampleCache {
    samples: HashMap<PathBuf, Arc<SampleBuffer>>,
    peaks: HashMap<PathBuf, Arc<Peaks>>,
}

impl SampleCache {
    pub fn get(&mut self, path: &Path) -> Result<Arc<SampleBuffer>, String> {
        if let Some(s) = self.samples.get(path) {
            return Ok(s.clone());
        }
        let s = Arc::new(
            rlive_devices::load_wav(path).map_err(|e| format!("{}: {e}", path.display()))?,
        );
        self.peaks
            .insert(path.to_path_buf(), Arc::new(Peaks::build(&s.left)));
        self.samples.insert(path.to_path_buf(), s.clone());
        Ok(s)
    }

    /// The waveform summary of a loaded file, with its sample rate.
    pub fn peaks(&self, path: &Path) -> Option<(Arc<Peaks>, f64)> {
        let sample_rate = self.samples.get(path)?.sample_rate;
        Some((self.peaks.get(path)?.clone(), sample_rate))
    }
}

/// Builds engine nodes from project data.
pub struct Builder<'a> {
    pub sample_rate: f64,
    pub max_block: usize,
    pub plugins: &'a PluginCache,
    pub samples: &'a mut SampleCache,
}

pub fn descriptor_for(
    kind: &DeviceKind,
    name: &str,
    plugins: &PluginCache,
) -> Option<PluginDescriptor> {
    let DeviceKind::Plugin {
        format,
        plugin_id,
        path,
        ..
    } = kind
    else {
        return None;
    };
    Some(
        plugins
            .plugins
            .iter()
            .find(|p| p.format == *format && p.id == *plugin_id)
            .cloned()
            .unwrap_or_else(|| PluginDescriptor {
                format: *format,
                id: plugin_id.clone(),
                name: name.to_string(),
                vendor: String::new(),
                version: String::new(),
                path: path.clone(),
                is_instrument: false,
            }),
    )
}

impl Builder<'_> {
    /// Instantiate a device. Never fails: broken devices become silent placeholders with an error.
    pub fn device(&mut self, dev: &Device) -> (Box<DeviceSlot>, DeviceRuntime) {
        let (processor, runtime) = match &dev.kind {
            DeviceKind::Builtin { type_id, sample } => match rlive_devices::find(type_id) {
                Some(info) => {
                    let mut error = None;
                    let sample = sample.as_ref().and_then(|p| match self.samples.get(p) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            error = Some(e);
                            None
                        }
                    });
                    let mut p = (info.create)(&DeviceInit { sample });
                    p.activate(self.sample_rate, self.max_block);
                    let params = (info.params)();
                    for param in &params {
                        let v = dev.params.get(&param.id).copied().unwrap_or(param.default);
                        p.set_param(param.id, v);
                    }
                    (
                        p,
                        DeviceRuntime {
                            params,
                            plugin: None,
                            error,
                            role: info.role,
                        },
                    )
                }
                None => placeholder(format!("unknown device {type_id}")),
            },
            DeviceKind::Plugin { state, .. } => {
                let desc = descriptor_for(&dev.kind, &dev.name, self.plugins).expect("plugin kind");
                match rlive_plugin_host::instantiate(&desc, self.sample_rate, self.max_block) {
                    Ok(mut inst) => {
                        if let Some(state) = state {
                            if !inst.handle.load_state(state) {
                                log::warn!("{}: could not restore plugin state", dev.name);
                            }
                        }
                        let role = if desc.is_instrument {
                            DeviceRole::Instrument
                        } else {
                            DeviceRole::AudioEffect
                        };
                        (
                            inst.processor,
                            DeviceRuntime {
                                params: inst.handle.params(),
                                plugin: Some(inst.handle),
                                error: None,
                                role,
                            },
                        )
                    }
                    Err(e) => placeholder(e.to_string()),
                }
            }
        };
        let mut slot = DeviceSlot::new(dev.id, processor);
        slot.bypass = dev.bypass;
        (Box::new(slot), runtime)
    }

    pub fn clip(&mut self, clip: &Clip) -> Result<Arc<ClipData>, String> {
        Ok(Arc::new(match &clip.content {
            ClipContent::Midi { notes } => {
                let notes: Vec<Note> = notes
                    .iter()
                    .map(|n| Note {
                        start: n.start,
                        length: n.length,
                        key: n.key,
                        velocity: n.velocity,
                    })
                    .collect();
                ClipData::Midi(MidiClip::new(clip.length, notes, clip.looped))
            }
            ClipContent::Audio { path, gain_db } => {
                let sample = self.samples.get(path)?;
                ClipData::Audio(AudioClip {
                    sample,
                    length: clip.length,
                    looped: clip.looped,
                    gain: rlive_core::db_to_gain(*gain_db),
                })
            }
        }))
    }

    /// Compiles a timeline. Clips that fail to load are left out and reported.
    pub fn lane(
        &mut self,
        clips: &[ArrangementClip],
        errors: &mut Vec<String>,
    ) -> Arc<ArrangementLane> {
        let mut lane = Vec::with_capacity(clips.len());
        for c in clips {
            match self.clip(&c.clip) {
                Ok(data) => lane.push(LaneClip {
                    start: c.start,
                    end: c.end(),
                    offset: c.offset,
                    data,
                }),
                Err(e) => errors.push(e),
            }
        }
        Arc::new(ArrangementLane { clips: lane })
    }

    pub fn track(
        &mut self,
        track: &Track,
        shared: Arc<TrackShared>,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
        errors: &mut Vec<String>,
    ) -> Box<TrackNode> {
        let mut node = TrackNode::new(track.id, self.max_block).with_shared(shared);
        node.mix = mix_of(track);
        for d in &track.devices {
            let (slot, rt) = self.device(d);
            if let Some(e) = &rt.error {
                errors.push(format!("{}: {e}", d.name));
            }
            node.devices.push(slot);
            runtimes.insert(d.id, rt);
        }
        for (i, c) in track.clips.iter().enumerate() {
            if let Some(c) = c {
                match self.clip(c) {
                    Ok(data) => node.set_slot(i, Some(data)),
                    Err(e) => errors.push(e),
                }
            }
        }
        node.set_lane(self.lane(&track.arrangement, errors));
        Box::new(node)
    }

    pub fn master(
        &mut self,
        project: &Project,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
        errors: &mut Vec<String>,
    ) -> Box<MasterNode> {
        let mut node = MasterNode::new(self.max_block);
        node.volume_db = project.master.volume_db;
        for d in &project.master.devices {
            let (slot, rt) = self.device(d);
            if let Some(e) = &rt.error {
                errors.push(format!("{}: {e}", d.name));
            }
            node.devices.push(slot);
            runtimes.insert(d.id, rt);
        }
        Box::new(node)
    }
}

fn placeholder(error: String) -> (Box<dyn Processor>, DeviceRuntime) {
    log::warn!("{error}");
    (
        Box::new(NullProcessor),
        DeviceRuntime {
            params: Vec::new(),
            plugin: None,
            error: Some(error),
            role: DeviceRole::AudioEffect,
        },
    )
}

pub fn mix_of(t: &Track) -> MixSettings {
    MixSettings {
        volume_db: t.volume_db,
        pan: t.pan,
        mute: t.mute,
        solo: t.solo,
    }
}

/// Where a device lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainRef {
    Track(TrackId),
    Master,
}

impl From<ChainRef> for ChainTarget {
    fn from(c: ChainRef) -> Self {
        match c {
            ChainRef::Track(t) => ChainTarget::Track(t),
            ChainRef::Master => ChainTarget::Master,
        }
    }
}

/// One clip's destination when moving or copying on the timeline.
#[derive(Debug, Clone, Copy)]
pub struct ArrMove {
    pub track: TrackId,
    pub id: ArrClipId,
    pub to_track: TrackId,
    pub start: u64,
}

/// Which edge of a timeline clip to move, and where to.
#[derive(Debug, Clone, Copy)]
pub enum TrimEdge {
    Left(u64),
    Right(u64),
}

/// A clip being edited: in a Session slot or on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipRef {
    Slot { track: TrackId, slot: usize },
    Arr { track: TrackId, id: ArrClipId },
}

impl ClipRef {
    pub fn slot(track: TrackId, slot: usize) -> Self {
        Self::Slot { track, slot }
    }

    pub fn track(self) -> TrackId {
        match self {
            Self::Slot { track, .. } | Self::Arr { track, .. } => track,
        }
    }

    /// The track and slot, if this clip is in a Session slot.
    pub fn as_slot(self) -> Option<(TrackId, usize)> {
        match self {
            Self::Slot { track, slot } => Some((track, slot)),
            Self::Arr { .. } => None,
        }
    }
}

pub struct Session {
    pub project: Project,
    pub path: Option<PathBuf>,
    pub engine: EngineHandle,
    pub runtimes: HashMap<DeviceId, DeviceRuntime>,
    pub track_shared: HashMap<TrackId, Arc<TrackShared>>,
    pub plugins: PluginCache,
    pub samples: SampleCache,
    /// Tracks that receive live MIDI; shared with the MIDI-input thread.
    pub midi_targets: Arc<Mutex<Vec<TrackId>>>,
    pub messages: Vec<String>,
    pub dirty: bool,
    pub history: History,
    recording: Option<Recording>,
}

impl Session {
    pub fn new(engine: EngineHandle, project: Project, plugins: PluginCache) -> Self {
        let history = History::new(&project);
        let mut s = Self {
            project,
            path: None,
            engine,
            runtimes: HashMap::new(),
            track_shared: HashMap::new(),
            plugins,
            samples: SampleCache::default(),
            midi_targets: Arc::new(Mutex::new(Vec::new())),
            messages: Vec::new(),
            dirty: false,
            history,
            recording: None,
        };
        s.rebuild();
        s
    }

    fn send(&mut self, cmd: EngineCommand) {
        let mut cmd = cmd;
        // The ring is large; if it is ever full, wait for the audio thread to drain it.
        for _ in 0..200 {
            match self.engine.send(cmd) {
                Ok(()) => return,
                Err(c) => {
                    cmd = c;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
        log::error!("engine command queue full; dropping command");
    }

    fn builder(&mut self) -> (Builder<'_>, &Project) {
        (
            Builder {
                sample_rate: self.engine.sample_rate,
                max_block: self.engine.max_block,
                plugins: &self.plugins,
                samples: &mut self.samples,
            },
            &self.project,
        )
    }

    /// Re-create every engine object from the project (load, new, sample-rate change).
    pub fn rebuild(&mut self) {
        self.recording = None;
        self.capture_plugin_states();
        self.runtimes.clear();
        self.project.normalize();
        let mut runtimes = HashMap::new();
        let mut errors = Vec::new();
        let mut shared_map = HashMap::new();
        let mut tracks = Vec::with_capacity(MAX_TRACKS);
        let (mut b, project) = self.builder();
        for t in &project.tracks {
            let shared = Arc::new(TrackShared::default());
            shared_map.insert(t.id, shared.clone());
            tracks.push(b.track(t, shared, &mut runtimes, &mut errors));
        }
        let master = b.master(project, &mut runtimes, &mut errors);
        self.runtimes = runtimes;
        self.track_shared = shared_map;
        self.messages.extend(errors);
        self.send(EngineCommand::ReplaceAll {
            tracks: Box::new(tracks),
            master,
        });
        let p = &self.project;
        let cmds = [
            EngineCommand::SetTempo(p.tempo),
            EngineCommand::SetTimeSignature(p.time_signature),
            EngineCommand::SetQuantize(p.quantize),
            EngineCommand::SetMetronome(p.metronome),
        ];
        for c in cmds {
            self.send(c);
        }
        self.sync_loop();
        self.update_midi_targets();
    }

    /// Swap in a new engine (e.g. after changing the audio device) and rebuild.
    pub fn replace_engine(&mut self, engine: EngineHandle) {
        self.capture_plugin_states();
        self.engine = engine;
        self.rebuild();
    }

    // ------------------------------------------------------------- files

    pub fn new_project(&mut self) {
        self.project = Project::new_default();
        self.history = History::new(&self.project);
        self.path = None;
        self.dirty = false;
        self.rebuild();
    }

    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        let p = Project::load(path).map_err(|e| e.to_string())?;
        self.project = p;
        self.history = History::new(&self.project);
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        // Plugin states in the file must win over the (about to be dropped) running instances.
        self.runtimes.clear();
        self.rebuild();
        Ok(())
    }

    /// Store each plugin's current state blob in the project.
    pub fn capture_plugin_states(&mut self) {
        let runtimes = &mut self.runtimes;
        let mut capture = |d: &mut Device| {
            if let DeviceKind::Plugin { state, .. } = &mut d.kind {
                if let Some(h) = runtimes.get_mut(&d.id).and_then(|r| r.plugin.as_mut()) {
                    if let Some(s) = h.save_state() {
                        *state = Some(s);
                    }
                }
            }
        };
        for t in self.project.tracks.iter_mut() {
            t.devices.iter_mut().for_each(&mut capture);
        }
        self.project
            .master
            .devices
            .iter_mut()
            .for_each(&mut capture);
    }

    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        self.capture_plugin_states();
        let path = if path.extension().is_none() {
            path.with_extension(rlive_project::FILE_EXTENSION)
        } else {
            path.to_path_buf()
        };
        self.project.save(&path).map_err(|e| e.to_string())?;
        self.path = Some(path);
        self.commit_edits(false);
        self.history.mark_saved();
        self.dirty = false;
        Ok(())
    }

    // ------------------------------------------------------------- transport

    pub fn is_playing(&self) -> bool {
        self.engine
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn play(&mut self) {
        self.send(EngineCommand::Play);
    }

    pub fn stop(&mut self) {
        self.send(EngineCommand::Stop);
    }

    pub fn set_tempo(&mut self, bpm: f64) {
        self.project.tempo = bpm.clamp(20.0, 999.0);
        self.send(EngineCommand::SetTempo(self.project.tempo));
        self.touch();
    }

    pub fn set_time_signature(&mut self, sig: TimeSignature) {
        self.project.time_signature = sig;
        self.send(EngineCommand::SetTimeSignature(sig));
        self.touch();
    }

    pub fn set_quantize(&mut self, q: Quantize) {
        self.project.quantize = q;
        self.send(EngineCommand::SetQuantize(q));
        self.touch();
    }

    pub fn set_metronome(&mut self, on: bool) {
        self.project.metronome = on;
        self.send(EngineCommand::SetMetronome(on));
    }

    pub fn launch_clip(&mut self, track: TrackId, slot: usize) {
        self.send(EngineCommand::LaunchClip { track, slot });
    }

    pub fn stop_track(&mut self, track: TrackId) {
        self.send(EngineCommand::StopTrack(track));
    }

    pub fn launch_scene(&mut self, scene: usize) {
        self.send(EngineCommand::LaunchScene(scene));
    }

    pub fn stop_all_clips(&mut self) {
        self.send(EngineCommand::StopAllClips);
    }

    // ------------------------------------------------------------- tracks

    pub fn add_track(&mut self, kind: TrackKind, index: Option<usize>) -> TrackId {
        let n = self.project.tracks.len() + 1;
        let (name, device) = match kind {
            TrackKind::Midi => (
                format!("{n} MIDI"),
                Some(Device::builtin("rlive.poly", "Poly Synth")),
            ),
            TrackKind::Audio => (format!("{n} Audio"), None),
        };
        let mut t = Track::new(
            &name,
            kind,
            self.project.next_color(),
            self.project.scenes.len(),
        );
        t.devices.extend(device);
        let id = t.id;
        let index = index.unwrap_or(self.project.tracks.len());
        self.install_track(t, index);
        self.mark_edit("Add Track");
        id
    }

    /// Adds a fully formed track to the project and the engine.
    fn install_track(&mut self, t: Track, index: usize) {
        let index = index.min(self.project.tracks.len());
        let shared = Arc::new(TrackShared::default());
        self.track_shared.insert(t.id, shared.clone());
        let mut errors = Vec::new();
        let mut runtimes = HashMap::new();
        let (mut b, _) = self.builder();
        let node = b.track(&t, shared, &mut runtimes, &mut errors);
        self.runtimes.extend(runtimes);
        self.messages.extend(errors);
        self.project.tracks.insert(index, t);
        self.send(EngineCommand::AddTrack { index, track: node });
    }

    pub fn delete_track(&mut self, id: TrackId) {
        if let Some(i) = self.project.track_index(id) {
            let mut t = self.project.tracks.remove(i);
            self.history.parked_mix.insert(id, ParkedMix::of(&t));
            for d in &mut t.devices {
                self.park_device(d);
                self.runtimes.remove(&d.id);
            }
            self.track_shared.remove(&id);
            self.send(EngineCommand::RemoveTrack(id));
            self.update_midi_targets();
            self.mark_edit("Delete Track");
        }
    }

    pub fn move_track(&mut self, id: TrackId, to: usize) {
        if let Some(i) = self.project.track_index(id) {
            let t = self.project.tracks.remove(i);
            let to = to.min(self.project.tracks.len());
            self.project.tracks.insert(to, t);
            self.send(EngineCommand::MoveTrack { track: id, to });
            self.mark_edit("Move Track");
        }
    }

    /// Push the track's volume/pan/mute/solo to the engine after editing the project.
    pub fn sync_mix(&mut self, id: TrackId) {
        if let Some(t) = self.project.track(id) {
            let mix = mix_of(t);
            self.send(EngineCommand::SetMix { track: id, mix });
            self.touch();
        }
    }

    pub fn set_master_volume(&mut self, db: f32) {
        self.project.master.volume_db = db;
        self.send(EngineCommand::SetMasterVolume(db));
        self.touch();
    }

    pub fn set_arm(&mut self, id: TrackId, arm: bool, exclusive: bool) {
        for t in self.project.tracks.iter_mut() {
            if t.id == id {
                t.arm = arm;
            } else if exclusive && arm {
                t.arm = false;
            }
        }
        self.update_midi_targets();
    }

    /// Armed tracks receive MIDI; if none are armed, the selected track does (set by the UI).
    pub fn update_midi_targets(&mut self) {
        let armed: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.arm)
            .map(|t| t.id)
            .collect();
        *self.midi_targets.lock().unwrap() = armed;
    }

    pub fn midi_in(&mut self, track: TrackId, message: MidiMessage) {
        self.send(EngineCommand::Midi { track, message });
    }

    /// MIDI the user performed: audible, and recorded while recording.
    pub fn play_midi(&mut self, track: TrackId, message: MidiMessage) {
        self.send(EngineCommand::PlayedMidi {
            track,
            message,
            at: std::time::Instant::now(),
        });
    }

    // ------------------------------------------------------------- recording

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Starts recording MIDI onto armed tracks, starting the transport if needed.
    /// Returns false if no MIDI track is armed.
    pub fn start_recording(&mut self) -> bool {
        let armed = self
            .project
            .tracks
            .iter()
            .any(|t| t.arm && t.kind == TrackKind::Midi);
        if !armed {
            return false;
        }
        let at = if self.is_playing() {
            self.engine.shared.tick.load()
        } else {
            self.insert_marker()
        };
        self.recording = Some(Recording::new(at as u64));
        self.send(EngineCommand::SetRecording(true));
        self.play();
        true
    }

    pub fn stop_recording(&mut self) {
        self.send(EngineCommand::SetRecording(false));
    }

    /// Puts what's been recorded so far on the timeline.
    fn flush_recording(&mut self) {
        let Some(recording) = self.recording.as_ref() else {
            return;
        };
        let placements = recording.placements(self.project.time_signature);
        for p in placements {
            let Some(t) = self.project.track_mut(p.track) else {
                continue;
            };
            if let Some(old) = p.existing {
                t.remove_arr_clip(old);
            }
            let mut clip = p.clip;
            clip.color = t.color;
            let mut placed = ArrangementClip::new(clip, p.start);
            if let Some(old) = p.existing {
                placed.id = old;
            }
            let id = placed.id;
            t.place_clip(placed);
            if let Some(r) = self.recording.as_mut() {
                r.set_clip(p.track, id);
            }
            self.sync_lane(p.track);
            self.mark_edit("Record");
        }
    }

    // ------------------------------------------------------------- scenes & clips

    pub fn add_scene(&mut self) {
        self.project.add_scene();
        self.mark_edit("Add Scene");
    }

    pub fn delete_scene(&mut self, index: usize) {
        if index >= self.project.scenes.len() || self.project.scenes.len() <= 1 {
            return;
        }
        self.project.scenes.remove(index);
        for t in self.project.tracks.iter_mut() {
            if index < t.clips.len() {
                t.clips.remove(index);
            }
        }
        self.project.normalize();
        // Slot indices shifted: resend all clips of every track.
        self.resend_all_clips();
        self.mark_edit("Delete Scene");
    }

    fn resend_all_clips(&mut self) {
        let ids: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for id in ids {
            let n = self.project.scenes.len() + 1;
            for slot in 0..n {
                self.sync_clip(id, slot);
            }
        }
    }

    /// Replace the clip in a slot (or clear it with `None`).
    pub fn set_clip(&mut self, track: TrackId, slot: usize, clip: Option<Clip>) {
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        if slot >= t.clips.len() {
            return;
        }
        t.clips[slot] = clip;
        self.sync_clip(track, slot);
        self.mark_edit("Edit Clip");
    }

    /// Recompile a slot's clip and send it to the engine (call after editing notes).
    pub fn sync_clip(&mut self, track: TrackId, slot: usize) {
        let clip = self
            .project
            .track(track)
            .and_then(|t| t.clips.get(slot))
            .cloned()
            .flatten();
        let data = match clip {
            Some(c) => {
                let (mut b, _) = self.builder();
                match b.clip(&c) {
                    Ok(d) => Some(d),
                    Err(e) => {
                        self.messages.push(e);
                        None
                    }
                }
            }
            None => None,
        };
        self.send(EngineCommand::SetClip {
            track,
            slot,
            clip: data,
        });
        self.mark_edit("Edit Clip");
    }

    pub fn clip(&self, track: TrackId, slot: usize) -> Option<&Clip> {
        self.project.track(track)?.clips.get(slot)?.as_ref()
    }

    pub fn clip_mut(&mut self, track: TrackId, slot: usize) -> Option<&mut Clip> {
        self.project.track_mut(track)?.clips.get_mut(slot)?.as_mut()
    }

    pub fn clip_at(&self, r: ClipRef) -> Option<&Clip> {
        match r {
            ClipRef::Slot { track, slot } => self.clip(track, slot),
            ClipRef::Arr { track, id } => Some(&self.project.track(track)?.arr_clip(id)?.clip),
        }
    }

    pub fn clip_at_mut(&mut self, r: ClipRef) -> Option<&mut Clip> {
        match r {
            ClipRef::Slot { track, slot } => self.clip_mut(track, slot),
            ClipRef::Arr { track, id } => {
                Some(&mut self.project.track_mut(track)?.arr_clip_mut(id)?.clip)
            }
        }
    }

    /// Sends a track's timeline to the engine.
    pub fn sync_lane(&mut self, track: TrackId) {
        let Some(clips) = self.project.track(track).map(|t| t.arrangement.clone()) else {
            return;
        };
        let mut errors = Vec::new();
        let (mut b, _) = self.builder();
        let lane = b.lane(&clips, &mut errors);
        self.messages.extend(errors);
        self.send(EngineCommand::SetLane { track, lane });
    }

    /// Sends the loop range to the engine.
    fn sync_loop(&mut self) {
        let a = &self.project.arrangement;
        let range = a
            .loop_enabled
            .then_some((a.loop_start as f64, a.loop_end as f64));
        self.send(EngineCommand::SetLoop(range));
    }

    /// Sends an edited clip to the engine.
    pub fn sync_ref(&mut self, r: ClipRef) {
        match r {
            ClipRef::Slot { track, slot } => self.sync_clip(track, slot),
            ClipRef::Arr { track, .. } => {
                self.sync_lane(track);
                self.mark_edit("Edit Clip");
            }
        }
    }

    // ------------------------------------------------------------- arrangement

    /// Where playback starts and returns to, in ticks.
    pub fn insert_marker(&self) -> f64 {
        self.engine.shared.insert_marker.load()
    }

    pub fn seek(&mut self, tick: f64) {
        self.send(EngineCommand::Seek(tick.max(0.0)));
    }

    pub fn set_loop(&mut self, enabled: bool, start: u64, end: u64) {
        let a = &mut self.project.arrangement;
        if (a.loop_enabled, a.loop_start, a.loop_end) == (enabled, start, end) {
            return;
        }
        a.loop_enabled = enabled;
        a.loop_start = start;
        a.loop_end = end;
        self.sync_loop();
        self.mark_edit("Change Loop");
    }

    pub fn back_to_arrangement(&mut self) {
        self.send(EngineCommand::BackToArrangement);
    }

    fn sync_lanes(&mut self, tracks: HashSet<TrackId>, label: &'static str) {
        for t in tracks {
            self.sync_lane(t);
        }
        self.mark_edit(label);
    }

    pub fn arr_place(&mut self, track: TrackId, clip: ArrangementClip) {
        if let Some(t) = self.project.track_mut(track) {
            t.place_clip(clip);
            self.sync_lane(track);
            self.mark_edit("Add Clip");
        }
    }

    /// Moves or copies clips; returns where they ended up.
    pub fn arr_move(&mut self, moves: &[ArrMove], copy: bool) -> Vec<(TrackId, ArrClipId)> {
        // Lift everything first so clips can't overwrite each other's old places.
        let lifted: Vec<(ArrMove, ArrangementClip)> = moves
            .iter()
            .filter_map(|m| {
                let t = self.project.track_mut(m.track)?;
                let c = if copy {
                    t.arr_clip(m.id).cloned()
                } else {
                    t.remove_arr_clip(m.id)
                }?;
                Some((*m, c))
            })
            .collect();
        let mut placed = Vec::with_capacity(lifted.len());
        let mut touched = HashSet::new();
        for (m, mut c) in lifted {
            c.start = m.start;
            if copy {
                c.id = ArrClipId::new();
            }
            placed.push((m.to_track, c.id));
            touched.insert(m.track);
            touched.insert(m.to_track);
            if let Some(t) = self.project.track_mut(m.to_track) {
                t.place_clip(c);
            }
        }
        self.sync_lanes(touched, if copy { "Copy Clips" } else { "Move Clips" });
        placed
    }

    pub fn arr_trim(&mut self, track: TrackId, id: ArrClipId, edge: TrimEdge) {
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        let Some(mut c) = t.remove_arr_clip(id) else {
            return;
        };
        match edge {
            TrimEdge::Left(to) => c.trim_left(to),
            TrimEdge::Right(to) => c.trim_right(to),
        }
        t.place_clip(c);
        self.sync_lane(track);
        self.mark_edit("Trim Clip");
    }

    pub fn arr_delete(&mut self, items: &[(TrackId, ArrClipId)]) {
        let mut touched = HashSet::new();
        for (track, id) in items {
            if let Some(t) = self.project.track_mut(*track) {
                if t.remove_arr_clip(*id).is_some() {
                    touched.insert(*track);
                }
            }
        }
        if !touched.is_empty() {
            self.sync_lanes(touched, "Delete Clips");
        }
    }

    /// Copies the clips just after the span they cover.
    pub fn arr_duplicate(&mut self, items: &[(TrackId, ArrClipId)]) -> Vec<(TrackId, ArrClipId)> {
        let clips: Vec<(TrackId, ArrangementClip)> = items
            .iter()
            .filter_map(|(t, id)| Some((*t, self.project.track(*t)?.arr_clip(*id)?.clone())))
            .collect();
        let (Some(first), Some(last)) = (
            clips.iter().map(|(_, c)| c.start).min(),
            clips.iter().map(|(_, c)| c.end()).max(),
        ) else {
            return Vec::new();
        };
        let moves: Vec<ArrMove> = clips
            .iter()
            .map(|(t, c)| ArrMove {
                track: *t,
                id: c.id,
                to_track: *t,
                start: c.start + (last - first),
            })
            .collect();
        self.arr_move(&moves, true)
    }

    /// Places clipboard clips with the earliest at `at`, on their original tracks.
    pub fn arr_paste(
        &mut self,
        clips: &[(TrackId, ArrangementClip)],
        at: u64,
    ) -> Vec<(TrackId, ArrClipId)> {
        let mut placed = Vec::with_capacity(clips.len());
        let mut touched = HashSet::new();
        for (track, c) in clips {
            let Some(t) = self.project.track_mut(*track) else {
                continue;
            };
            let id = ArrClipId::new();
            t.place_clip(ArrangementClip {
                id,
                start: c.start + at,
                ..c.clone()
            });
            placed.push((*track, id));
            touched.insert(*track);
        }
        if !touched.is_empty() {
            self.sync_lanes(touched, "Paste Clips");
        }
        placed
    }

    /// Create an audio clip from a WAV file; its loop length follows the file length at the
    /// current tempo (rounded to beats).
    pub fn audio_clip_from_file(&mut self, path: &Path) -> Result<Clip, String> {
        let s = self.samples.get(path)?;
        let beats = (s.duration_secs() * self.project.tempo / 60.0)
            .round()
            .max(1.0);
        Ok(Clip {
            name: s.name.clone(),
            color: self.project.next_color(),
            length: (beats as u64) * rlive_core::PPQ,
            looped: true,
            content: ClipContent::Audio {
                path: path.to_path_buf(),
                gain_db: 0.0,
            },
        })
    }

    // ------------------------------------------------------------- devices

    fn chain(&self, chain: ChainRef) -> Option<&Vec<Device>> {
        match chain {
            ChainRef::Track(t) => self.project.track(t).map(|t| &t.devices),
            ChainRef::Master => Some(&self.project.master.devices),
        }
    }

    fn chain_mut(&mut self, chain: ChainRef) -> Option<&mut Vec<Device>> {
        match chain {
            ChainRef::Track(t) => self.project.track_mut(t).map(|t| &mut t.devices),
            ChainRef::Master => Some(&mut self.project.master.devices),
        }
    }

    pub fn devices(&self, chain: ChainRef) -> &[Device] {
        self.chain(chain).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn add_device(&mut self, chain: ChainRef, index: Option<usize>, device: Device) {
        let (mut b, _) = self.builder();
        let (slot, rt) = b.device(&device);
        if let Some(e) = &rt.error {
            self.messages.push(format!("{}: {e}", device.name));
        }
        let id = device.id;
        let Some(devs) = self.chain_mut(chain) else {
            return;
        };
        let index = index.unwrap_or(devs.len()).min(devs.len());
        devs.insert(index, device);
        self.runtimes.insert(id, rt);
        self.send(EngineCommand::InsertDevice {
            target: chain.into(),
            index,
            device: slot,
        });
        self.mark_edit("Add Device");
    }

    pub fn add_builtin(&mut self, chain: ChainRef, type_id: &str) {
        if let Some(info) = rlive_devices::find(type_id) {
            let d = Device::builtin(info.id, info.name);
            let index = self.instrument_insert_index(chain, info.role);
            self.add_device(chain, index, d);
        }
    }

    pub fn add_plugin(&mut self, chain: ChainRef, desc: &PluginDescriptor) {
        let d = Device::plugin(desc.format, &desc.id, desc.path.clone(), &desc.name);
        let role = if desc.is_instrument {
            DeviceRole::Instrument
        } else {
            DeviceRole::AudioEffect
        };
        let index = self.instrument_insert_index(chain, role);
        self.add_device(chain, index, d);
    }

    /// Instruments go to the front of the chain (after any other instrument), effects at the end.
    fn instrument_insert_index(&self, chain: ChainRef, role: DeviceRole) -> Option<usize> {
        if role != DeviceRole::Instrument {
            return None;
        }
        let devs = self.devices(chain);
        Some(
            devs.iter()
                .take_while(|d| {
                    self.runtimes
                        .get(&d.id)
                        .is_some_and(|r| r.role == DeviceRole::Instrument)
                })
                .count(),
        )
    }

    pub fn remove_device(&mut self, chain: ChainRef, id: DeviceId) {
        if let Some(devs) = self.chain_mut(chain) {
            if let Some(i) = devs.iter().position(|d| d.id == id) {
                let mut d = devs.remove(i);
                self.park_device(&mut d);
            }
        }
        if let Some(mut rt) = self.runtimes.remove(&id) {
            if let Some(h) = rt.plugin.as_mut() {
                h.close_editor();
            }
            // The handle is dropped now; the processor half comes back as engine garbage.
        }
        self.send(EngineCommand::RemoveDevice {
            target: chain.into(),
            device: id,
        });
        self.mark_edit("Remove Device");
    }

    pub fn move_device(&mut self, chain: ChainRef, id: DeviceId, to: usize) {
        let Some(devs) = self.chain_mut(chain) else {
            return;
        };
        let Some(i) = devs.iter().position(|d| d.id == id) else {
            return;
        };
        let d = devs.remove(i);
        let to = to.min(devs.len());
        devs.insert(to, d);
        self.send(EngineCommand::MoveDevice {
            target: chain.into(),
            device: id,
            to,
        });
        self.mark_edit("Move Device");
    }

    /// Fold/unfold a device in the device view (UI state stored in the project).
    pub fn set_device_expanded(&mut self, chain: ChainRef, id: DeviceId, expanded: bool) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.expanded = expanded;
            self.touch();
        }
    }

    pub fn set_bypass(&mut self, chain: ChainRef, id: DeviceId, bypass: bool) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.bypass = bypass;
        }
        self.send(EngineCommand::SetBypass {
            target: chain.into(),
            device: id,
            bypass,
        });
        self.touch();
    }

    pub fn param_value(&self, chain: ChainRef, id: DeviceId, param: &ParamInfo) -> f64 {
        if let Some(v) = self
            .chain(chain)
            .and_then(|v| v.iter().find(|d| d.id == id))
            .and_then(|d| d.params.get(&param.id))
        {
            return *v;
        }
        self.runtimes
            .get(&id)
            .and_then(|r| r.plugin.as_ref())
            .and_then(|h| h.param_value(param.id))
            .unwrap_or(param.default)
    }

    pub fn set_param(&mut self, chain: ChainRef, id: DeviceId, param: u32, value: f64) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.params.insert(param, value);
        }
        if let Some(h) = self.runtimes.get_mut(&id).and_then(|r| r.plugin.as_mut()) {
            h.on_param_set(param, value);
        }
        self.send(EngineCommand::SetParam {
            target: chain.into(),
            device: id,
            param,
            value,
        });
        self.touch();
    }

    /// Change the sample of a built-in sampler (re-creates the device).
    pub fn set_device_sample(&mut self, chain: ChainRef, id: DeviceId, path: PathBuf) {
        let Some(devs) = self.chain_mut(chain) else {
            return;
        };
        let Some(pos) = devs.iter().position(|d| d.id == id) else {
            return;
        };
        if let DeviceKind::Builtin { sample, .. } = &mut devs[pos].kind {
            *sample = Some(path);
        }
        let d = devs[pos].clone();
        // Replace: remove old, insert new with same id at the same position.
        self.send(EngineCommand::RemoveDevice {
            target: chain.into(),
            device: id,
        });
        let (mut b, _) = self.builder();
        let (slot, rt) = b.device(&d);
        if let Some(e) = &rt.error {
            self.messages.push(e.clone());
        }
        self.runtimes.insert(id, rt);
        self.send(EngineCommand::InsertDevice {
            target: chain.into(),
            index: pos,
            device: slot,
        });
        self.touch();
    }

    /// Re-instantiate a plugin (keeping its state), e.g. after it requested a restart.
    pub fn reload_device(&mut self, chain: ChainRef, id: DeviceId) {
        self.capture_plugin_states();
        let Some(devs) = self.chain(chain) else {
            return;
        };
        let Some(pos) = devs.iter().position(|d| d.id == id) else {
            return;
        };
        let d = devs[pos].clone();
        self.runtimes.remove(&id);
        self.send(EngineCommand::RemoveDevice {
            target: chain.into(),
            device: id,
        });
        let (mut b, _) = self.builder();
        let (slot, rt) = b.device(&d);
        self.runtimes.insert(id, rt);
        self.send(EngineCommand::InsertDevice {
            target: chain.into(),
            index: pos,
            device: slot,
        });
    }

    fn chain_of_device(&self, id: DeviceId) -> Option<ChainRef> {
        if self.project.master.devices.iter().any(|d| d.id == id) {
            return Some(ChainRef::Master);
        }
        self.project
            .tracks
            .iter()
            .find(|t| t.devices.iter().any(|d| d.id == id))
            .map(|t| ChainRef::Track(t.id))
    }

    // ------------------------------------------------------------- undo

    /// Records that an undoable edit happened.
    fn mark_edit(&mut self, label: &'static str) {
        self.dirty = true;
        self.history.pending.get_or_insert(label);
    }

    /// Records a change that undo doesn't cover.
    fn touch(&mut self) {
        self.dirty = true;
        self.history.untracked_dirty = true;
    }

    /// Keeps a removed device, with its plugin state, in case undo brings it back.
    fn park_device(&mut self, d: &mut Device) {
        if let DeviceKind::Plugin { state, .. } = &mut d.kind {
            if let Some(h) = self.runtimes.get_mut(&d.id).and_then(|r| r.plugin.as_mut()) {
                if let Some(s) = h.save_state() {
                    *state = Some(s);
                }
            }
        }
        self.history.parked_devices.insert(d.id, d.clone());
    }

    /// Turns finished edits into an undo step; call once per frame. While
    /// `busy` (a drag or text entry in progress) edits accumulate into one step.
    pub fn commit_edits(&mut self, busy: bool) {
        if busy {
            self.history.was_busy = true;
            return;
        }
        let finished = std::mem::take(&mut self.history.was_busy);
        if self.history.pending.is_none() && !finished {
            return;
        }
        let label = self.history.pending.take().unwrap_or("Edit");
        if self.history.commit(&self.project, label) {
            self.dirty = true;
        }
    }

    fn after_restore(&mut self) {
        self.history.pending = None;
        self.history.was_busy = false;
        self.dirty = self.history.untracked_dirty || !self.history.at_saved();
    }

    pub fn undo(&mut self) -> bool {
        if self.is_recording() {
            return false;
        }
        self.commit_edits(false);
        let Some(target) = self.history.undo_target() else {
            return false;
        };
        self.restore(&target);
        self.after_restore();
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.is_recording() {
            return false;
        }
        self.commit_edits(false);
        let Some(target) = self.history.redo_target() else {
            return false;
        };
        self.restore(&target);
        self.after_restore();
        true
    }

    /// Brings the project to `target` by applying only what differs, so
    /// everything else keeps playing and no unaffected plugin is touched.
    fn restore(&mut self, target: &Snapshot) {
        self.project.scenes = (*target.scenes).clone();
        let keep: HashSet<TrackId> = target.tracks.iter().map(|t| t.id).collect();
        let gone: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .map(|t| t.id)
            .filter(|id| !keep.contains(id))
            .collect();
        for id in gone {
            self.delete_track(id);
        }
        for (i, s) in target.tracks.iter().enumerate() {
            if self.project.track(s.id).is_none() {
                let t = self.track_from_snapshot(s);
                self.install_track(t, i);
            }
        }
        for (i, s) in target.tracks.iter().enumerate() {
            if self.project.track_index(s.id) != Some(i) {
                self.move_track(s.id, i);
            }
        }
        for s in &target.tracks {
            self.restore_track(s);
        }
        self.restore_chain(ChainRef::Master, &target.master_devices);
        self.project.arrangement = target.arrangement.clone();
        self.sync_loop();
        self.project.normalize();
        self.update_midi_targets();
    }

    fn track_from_snapshot(&self, s: &TrackSnap) -> Track {
        let mut t = Track::new(&s.name, s.kind, s.color, s.clips.len());
        t.id = s.id;
        if let Some(m) = self.history.parked_mix.get(&s.id) {
            t.volume_db = m.volume_db;
            t.pan = m.pan;
            t.mute = m.mute;
            t.solo = m.solo;
            t.arm = m.arm;
        }
        t.devices = s
            .device_ids
            .iter()
            .filter_map(|id| self.history.parked_devices.get(id).cloned())
            .collect();
        t.clips = s.clips.iter().map(|c| c.as_deref().cloned()).collect();
        t.arrangement = s.lane.iter().map(|c| (**c).clone()).collect();
        t
    }

    fn restore_track(&mut self, s: &TrackSnap) {
        let Some(t) = self.project.track_mut(s.id) else {
            return;
        };
        t.name.clone_from(&s.name);
        t.color = s.color;
        let slots = t.clips.len().max(s.clips.len());
        let mut changed = Vec::new();
        for i in 0..slots {
            let want = s.clips.get(i).and_then(|c| c.as_deref());
            if t.clips.get(i).and_then(Option::as_ref) != want {
                changed.push(i);
            }
        }
        t.clips = s.clips.iter().map(|c| c.as_deref().cloned()).collect();
        let lane_changed = t.arrangement.len() != s.lane.len()
            || t.arrangement.iter().zip(&s.lane).any(|(a, b)| *a != **b);
        if lane_changed {
            t.arrangement = s.lane.iter().map(|c| (**c).clone()).collect();
        }
        self.restore_chain(ChainRef::Track(s.id), &s.device_ids);
        for slot in changed {
            self.sync_clip(s.id, slot);
        }
        if lane_changed {
            self.sync_lane(s.id);
        }
    }

    fn restore_chain(&mut self, chain: ChainRef, ids: &[DeviceId]) {
        let current: Vec<DeviceId> = self.devices(chain).iter().map(|d| d.id).collect();
        for id in current.iter().filter(|id| !ids.contains(id)) {
            self.remove_device(chain, *id);
        }
        for (i, id) in ids.iter().enumerate() {
            if !current.contains(id) {
                if let Some(d) = self.history.parked_devices.get(id).cloned() {
                    self.add_device(chain, Some(i), d);
                }
            }
        }
        for (i, id) in ids.iter().enumerate() {
            if self.devices(chain).get(i).map(|d| d.id) != Some(*id) {
                self.move_device(chain, *id, i);
            }
        }
    }

    // ------------------------------------------------------------- housekeeping

    /// Call every UI frame: frees engine garbage and services plugin callbacks.
    pub fn idle(&mut self) {
        self.engine.collect_garbage();
        while let Some(ev) = self.engine.poll_event() {
            match ev {
                EngineEvent::ParamChanged {
                    target,
                    device,
                    param,
                    value,
                } => {
                    let chain = match target {
                        ChainTarget::Track(t) => ChainRef::Track(t),
                        ChainTarget::Master => ChainRef::Master,
                    };
                    if let Some(d) = self
                        .chain_mut(chain)
                        .and_then(|v| v.iter_mut().find(|d| d.id == device))
                    {
                        d.params.insert(param, value);
                    }
                }
                EngineEvent::RecordedMidi {
                    track,
                    tick,
                    message,
                } => {
                    let armed = self
                        .project
                        .track(track)
                        .is_some_and(|t| t.arm && t.kind == TrackKind::Midi);
                    if let (true, Some(r)) = (armed, self.recording.as_mut()) {
                        r.message(track, tick, message);
                    }
                }
                EngineEvent::Jumped { from, to } => {
                    if let Some(r) = self.recording.as_mut() {
                        r.jumped(from, to);
                    }
                    self.flush_recording();
                }
                EngineEvent::RecordingStopped { tick } => {
                    if let Some(r) = self.recording.as_mut() {
                        r.finish(tick);
                    }
                    self.flush_recording();
                    self.recording = None;
                }
            }
        }
        let mut todo = Vec::new();
        for (id, rt) in self.runtimes.iter_mut() {
            if let Some(h) = rt.plugin.as_mut() {
                for n in h.idle() {
                    match n {
                        HostNotification::ParamsRescanned => rt.params = h.params(),
                        other => todo.push((*id, other)),
                    }
                }
            }
        }
        for (id, n) in todo {
            let Some(chain) = self.chain_of_device(id) else {
                continue;
            };
            match n {
                HostNotification::ParamChanged { id: param, value } => {
                    // Editor moved a knob on the controller side: forward to the processor.
                    if let Some(d) = self
                        .chain_mut(chain)
                        .and_then(|v| v.iter_mut().find(|d| d.id == id))
                    {
                        d.params.insert(param, value);
                    }
                    self.send(EngineCommand::SetParam {
                        target: chain.into(),
                        device: id,
                        param,
                        value,
                    });
                    self.touch();
                }
                HostNotification::RestartRequested => self.reload_device(chain, id),
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------- export

    /// Render `bars` bars of `scene` offline into a WAV file at the engine's sample rate.
    /// Plugins are instantiated afresh (with their current state) for the render.
    pub fn export_scene(&mut self, scene: usize, bars: u32, path: &Path) -> Result<(), String> {
        let ticks = bars as u64 * self.project.time_signature.ticks_per_bar();
        self.render_offline(vec![EngineCommand::LaunchScene(scene)], ticks, path)
    }

    /// Render the Arrangement to a WAV file: the loop range if looping, otherwise
    /// from the start to the end of the last clip.
    pub fn export_arrangement(&mut self, path: &Path) -> Result<(), String> {
        let a = &self.project.arrangement;
        let (start, end) = if a.loop_enabled {
            (a.loop_start, a.loop_end)
        } else {
            (0, self.project.arrangement_end())
        };
        if end <= start {
            return Err("the Arrangement is empty".into());
        }
        let setup = vec![
            EngineCommand::SetLoop(None),
            EngineCommand::Seek(start as f64),
            EngineCommand::Play,
        ];
        self.render_offline(setup, end - start, path)
    }

    /// Renders `ticks` of the project on a private engine, after `setup` commands.
    fn render_offline(
        &mut self,
        setup: Vec<EngineCommand>,
        ticks: u64,
        path: &Path,
    ) -> Result<(), String> {
        self.capture_plugin_states();
        let sample_rate = self.engine.sample_rate;
        let max_block = 512;
        let (mut engine, mut handle) = rlive_engine::create_engine(sample_rate, max_block);
        let mut runtimes = HashMap::new();
        let mut errors = Vec::new();
        let mut b = Builder {
            sample_rate,
            max_block,
            plugins: &self.plugins,
            samples: &mut self.samples,
        };
        let p = &self.project;
        let mut tracks = Vec::with_capacity(MAX_TRACKS);
        for t in &p.tracks {
            tracks.push(b.track(
                t,
                Arc::new(TrackShared::default()),
                &mut runtimes,
                &mut errors,
            ));
        }
        let master = b.master(p, &mut runtimes, &mut errors);
        let mut cmds = vec![
            EngineCommand::ReplaceAll {
                tracks: Box::new(tracks),
                master,
            },
            EngineCommand::SetTempo(p.tempo),
            EngineCommand::SetTimeSignature(p.time_signature),
            EngineCommand::SetQuantize(Quantize::None),
        ];
        cmds.extend(setup);
        for c in cmds {
            if handle.send(c).is_err() {
                return Err("engine queue full".into());
            }
        }
        let seconds = ticks as f64 / rlive_core::PPQ as f64 * 60.0 / p.tempo;
        let frames = (seconds * sample_rate) as usize;
        let mut left = vec![0.0; frames];
        let mut right = vec![0.0; frames];
        for (l, r) in left.chunks_mut(max_block).zip(right.chunks_mut(max_block)) {
            engine.process(l, r);
            handle.collect_garbage();
        }
        rlive_devices::write_wav(path, &left, &right, sample_rate as u32)
            .map_err(|e| e.to_string())?;
        drop(engine);
        drop(runtimes);
        Ok(())
    }

    /// Descriptors of all known plugins of a format.
    pub fn plugins_of(&self, format: PluginFormat) -> impl Iterator<Item = &PluginDescriptor> {
        self.plugins
            .plugins
            .iter()
            .filter(move |p| p.format == format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_engine::create_engine;

    fn session() -> (rlive_engine::Engine, Session) {
        let (engine, handle) = create_engine(48_000.0, 512);
        (
            engine,
            Session::new(handle, Project::new_default(), PluginCache::default()),
        )
    }

    fn render(engine: &mut rlive_engine::Engine, s: &mut Session, frames: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut done = 0;
        while done < frames {
            let mut l = vec![0.0; 512];
            let mut r = vec![0.0; 512];
            engine.process(&mut l, &mut r);
            s.idle();
            out.extend_from_slice(&l);
            done += 512;
        }
        out
    }

    fn commit(s: &mut Session) {
        s.commit_edits(false);
    }

    fn poly_param(s: &Session, t: TrackId) -> (DeviceId, u32) {
        let d = s.devices(ChainRef::Track(t))[0].id;
        (d, s.runtimes[&d].params[0].id)
    }

    #[test]
    fn undo_restores_an_edited_clip_and_leaves_others_playing() {
        let (mut engine, mut s) = session();
        let (playing, other) = (s.project.tracks[0].id, s.project.tracks[1].id);
        s.launch_clip(playing, 0);
        render(&mut engine, &mut s, 4_800);
        let shared = s.track_shared[&playing].clone();
        let clip = Clip::new_midi("new", [1, 2, 3], 1, s.project.time_signature);
        s.set_clip(other, 1, Some(clip));
        commit(&mut s);
        assert!(s.undo());
        assert!(s.clip(other, 1).is_none());
        assert!(
            Arc::ptr_eq(&shared, &s.track_shared[&playing]),
            "no rebuild"
        );
        render(&mut engine, &mut s, 4_800);
        assert_eq!(
            shared
                .playing_slot
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(s.redo());
        assert_eq!(s.clip(other, 1).unwrap().name, "new");
    }

    #[test]
    fn knob_moves_make_no_step_and_survive_undo() {
        let (_engine, mut s) = session();
        let t = s.project.tracks[0].id;
        let clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
        s.set_clip(t, 2, Some(clip));
        commit(&mut s);
        let (dev, param) = poly_param(&s, t);
        s.set_param(ChainRef::Track(t), dev, param, 0.25);
        commit(&mut s);
        assert_eq!(s.history.undo_label(), Some("Edit Clip"));
        assert!(s.undo());
        assert_eq!(s.project.tracks[0].devices[0].params[&param], 0.25);
        assert!(!s.undo(), "the knob move must not be a step");
    }

    #[test]
    fn deleting_a_track_is_undoable_with_its_devices() {
        let (_engine, mut s) = session();
        let (t, survivor) = (s.project.tracks[1].id, s.project.tracks[0].id);
        let shared = s.track_shared[&survivor].clone();
        let (dev, param) = poly_param(&s, t);
        s.set_param(ChainRef::Track(t), dev, param, 0.75);
        s.project.track_mut(t).unwrap().volume_db = -9.0;
        let ids: Vec<DeviceId> = s.devices(ChainRef::Track(t)).iter().map(|d| d.id).collect();
        s.delete_track(t);
        commit(&mut s);
        assert!(s.project.track(t).is_none());
        assert!(s.undo());
        let restored = s.project.track(t).expect("track is back");
        assert_eq!(
            restored.devices.iter().map(|d| d.id).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(restored.devices[0].params[&param], 0.75);
        assert_eq!(restored.volume_db, -9.0);
        assert_eq!(s.project.track_index(t), Some(1));
        assert!(s.runtimes.contains_key(&dev));
        assert!(
            Arc::ptr_eq(&shared, &s.track_shared[&survivor]),
            "no rebuild"
        );
    }

    #[test]
    fn removing_a_device_is_undoable_in_place() {
        let (_engine, mut s) = session();
        let t = s.project.tracks[1].id;
        let chain = ChainRef::Track(t);
        let ids: Vec<DeviceId> = s.devices(chain).iter().map(|d| d.id).collect();
        s.remove_device(chain, ids[0]);
        commit(&mut s);
        assert_eq!(s.devices(chain).len(), ids.len() - 1);
        assert!(s.undo());
        let now: Vec<DeviceId> = s.devices(chain).iter().map(|d| d.id).collect();
        assert_eq!(now, ids);
    }

    #[test]
    fn a_drag_of_direct_edits_is_one_step() {
        let (_engine, mut s) = session();
        let t = s.project.tracks[0].id;
        for name in ["a", "ab", "abc"] {
            s.project.track_mut(t).unwrap().name = name.into();
            s.commit_edits(true);
        }
        s.commit_edits(false);
        assert_eq!(s.history.undo_label(), Some("Edit"));
        s.undo();
        assert_eq!(s.project.track(t).unwrap().name, "1 Poly");
        assert!(!s.undo());
    }

    #[test]
    fn dirty_clears_at_the_saved_state_unless_a_knob_moved() {
        let (_engine, mut s) = session();
        let t = s.project.tracks[0].id;
        let clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
        s.set_clip(t, 2, Some(clip.clone()));
        commit(&mut s);
        assert!(s.dirty);
        s.undo();
        assert!(!s.dirty, "back at the state the session started from");

        s.set_clip(t, 2, Some(clip));
        commit(&mut s);
        let (dev, param) = poly_param(&s, t);
        s.set_param(ChainRef::Track(t), dev, param, 0.5);
        s.undo();
        assert!(s.dirty, "the knob move still needs saving");
    }

    fn on(key: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 0.8,
        }
    }

    fn off(key: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity: 0.5,
        }
    }

    fn silent_project(s: &mut Session) -> TrackId {
        for i in 0..s.project.tracks.len() {
            s.project.tracks[i].clips.iter_mut().for_each(|c| *c = None);
        }
        s.project.tracks[0].id
    }

    #[test]
    fn a_timeline_clip_is_audible_from_the_marker() {
        let (mut engine, mut s) = session();
        let t = silent_project(&mut s);
        let mut clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
        clip.notes_mut().unwrap().push(rlive_project::NoteData {
            start: 0,
            length: 3000,
            key: 60,
            velocity: 1.0,
        });
        s.arr_place(t, ArrangementClip::new(clip, 3840));
        s.seek(3840.0);
        render(&mut engine, &mut s, 1024);
        s.play();
        let out = render(&mut engine, &mut s, 24_000);
        assert!(out.iter().any(|x| x.abs() > 0.01), "the clip should sound");
    }

    #[test]
    fn trims_moves_and_deletes_are_one_undo_step_each() {
        let (_engine, mut s) = session();
        let t = silent_project(&mut s);
        let clip = Clip::new_midi("x", [1, 2, 3], 2, s.project.time_signature);
        let placed = ArrangementClip::new(clip, 0);
        let id = placed.id;
        s.arr_place(t, placed);
        commit(&mut s);
        s.arr_trim(t, id, TrimEdge::Right(1920));
        commit(&mut s);
        assert_eq!(
            s.project.track(t).unwrap().arr_clip(id).unwrap().length,
            1920
        );
        s.undo();
        assert_eq!(
            s.project.track(t).unwrap().arr_clip(id).unwrap().length,
            7680
        );
        s.arr_delete(&[(t, id)]);
        commit(&mut s);
        assert!(s.project.track(t).unwrap().arrangement.is_empty());
        s.undo();
        assert!(s.project.track(t).unwrap().arr_clip(id).is_some());
    }

    #[test]
    fn duplicating_lands_right_after_the_selection() {
        let (_engine, mut s) = session();
        let t = silent_project(&mut s);
        let clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
        let placed = ArrangementClip::new(clip, 960);
        let id = placed.id;
        s.arr_place(t, placed);
        let new = s.arr_duplicate(&[(t, id)]);
        let c = s.project.track(t).unwrap().arr_clip(new[0].1).unwrap();
        assert_eq!(c.start, 960 + 3840);
    }

    #[test]
    fn exporting_the_arrangement_covers_its_extent_or_loop() {
        let (mut engine, mut s) = session();
        let t = silent_project(&mut s);
        let clip = Clip::new_midi("x", [1, 2, 3], 2, s.project.time_signature);
        s.arr_place(t, ArrangementClip::new(clip, 0));
        render(&mut engine, &mut s, 1024);
        let dir = std::env::temp_dir().join(format!("rlive-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");

        s.export_arrangement(&path).unwrap();
        let whole = rlive_devices::load_wav(&path).unwrap();
        assert_eq!(whole.len(), 192_000, "two bars at 120 BPM and 48 kHz");

        s.set_loop(true, 0, 1920);
        s.export_arrangement(&path).unwrap();
        let part = rlive_devices::load_wav(&path).unwrap();
        assert_eq!(part.len(), 48_000, "one beat-pair at 120 BPM");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_empty_arrangement_does_not_export() {
        let (_engine, mut s) = session();
        let path = std::env::temp_dir().join("rlive-never-written.wav");
        assert!(s.export_arrangement(&path).is_err());
    }

    #[test]
    fn recording_needs_an_armed_midi_track() {
        let (_engine, mut s) = session();
        assert!(!s.start_recording());
        assert!(!s.is_recording());
    }

    #[test]
    fn a_recorded_take_lands_on_the_armed_track_as_one_undo_step() {
        let (mut engine, mut s) = session();
        let t = silent_project(&mut s);
        let unarmed = s.project.tracks[1].id;
        s.set_arm(t, true, true);
        assert!(s.start_recording());
        render(&mut engine, &mut s, 4_096);
        s.play_midi(t, on(60));
        s.play_midi(unarmed, on(72));
        s.midi_in(t, on(64)); // a piano-roll preview, which must not be recorded
        s.commit_edits(s.is_recording());
        render(&mut engine, &mut s, 12_000);
        s.play_midi(t, off(60));
        s.play_midi(unarmed, off(72));
        s.midi_in(t, off(64));
        render(&mut engine, &mut s, 4_096);
        s.stop();
        render(&mut engine, &mut s, 1_024);
        assert!(!s.is_recording());
        commit(&mut s);

        let lane = &s.project.track(t).unwrap().arrangement;
        assert_eq!(lane.len(), 1);
        assert_eq!(lane[0].clip.notes().len(), 1);
        assert_eq!(lane[0].clip.notes()[0].key, 60);
        assert!(s.project.track(unarmed).unwrap().arrangement.is_empty());

        assert_eq!(s.history.undo_label(), Some("Record"));
        assert!(s.undo());
        assert!(s.project.track(t).unwrap().arrangement.is_empty());
        assert!(!s.undo(), "the take is a single step");
    }

    #[test]
    fn loop_recording_overdubs_into_one_clip() {
        let (mut engine, mut s) = session();
        let t = silent_project(&mut s);
        s.set_arm(t, true, true);
        s.set_loop(true, 0, 3840); // two seconds at 120 BPM
        commit(&mut s);
        assert!(s.start_recording());
        render(&mut engine, &mut s, 4_096);
        s.play_midi(t, on(60));
        render(&mut engine, &mut s, 4_096);
        s.play_midi(t, off(60));
        render(&mut engine, &mut s, 96_000); // past the wrap
        s.play_midi(t, on(62));
        render(&mut engine, &mut s, 4_096);
        s.play_midi(t, off(62));
        render(&mut engine, &mut s, 4_096);
        s.stop();
        render(&mut engine, &mut s, 1_024);
        let lane = &s.project.track(t).unwrap().arrangement;
        assert_eq!(lane.len(), 1);
        let keys: Vec<u8> = lane[0].clip.notes().iter().map(|n| n.key).collect();
        assert!(keys.contains(&60) && keys.contains(&62), "got {keys:?}");
    }

    #[test]
    fn default_project_plays_demo_clip() {
        let (mut engine, mut s) = session();
        let id = s.project.tracks[0].id;
        s.launch_clip(id, 0);
        let out = render(&mut engine, &mut s, 48_000);
        assert!(
            out.iter().any(|x| x.abs() > 0.01),
            "demo clip should make sound"
        );
        assert!(s.is_playing());
    }

    #[test]
    fn edits_reach_engine() {
        let (mut engine, mut s) = session();
        let t = s.add_track(TrackKind::Midi, None);
        assert_eq!(s.project.tracks.len(), 4);
        let mut clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
        clip.notes_mut().unwrap().push(rlive_project::NoteData {
            start: 0,
            length: 960,
            key: 60,
            velocity: 1.0,
        });
        s.set_clip(t, 0, Some(clip));
        s.add_builtin(ChainRef::Track(t), "rlive.reverb");
        assert_eq!(s.devices(ChainRef::Track(t)).len(), 2);
        s.launch_clip(t, 0);
        let out = render(&mut engine, &mut s, 24_000);
        assert!(out.iter().any(|x| x.abs() > 0.01));

        // Muting everything silences the output.
        for i in 0..s.project.tracks.len() {
            s.project.tracks[i].mute = true;
            let id = s.project.tracks[i].id;
            s.sync_mix(id);
        }
        let out = render(&mut engine, &mut s, 24_000);
        assert!(out[out.len() - 1000..].iter().all(|x| x.abs() < 1e-3));

        let dev = s.devices(ChainRef::Track(t))[1].id;
        s.remove_device(ChainRef::Track(t), dev);
        s.delete_track(t);
        render(&mut engine, &mut s, 2048);
        assert_eq!(s.project.tracks.len(), 3);
    }

    #[test]
    fn save_load_roundtrip_and_export() {
        let (mut engine, mut s) = session();
        let dir = std::env::temp_dir().join(format!("rlive-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.rlive");
        s.set_tempo(100.0);
        s.save(&path).unwrap();
        s.new_project();
        assert_eq!(s.project.tempo, 120.0);
        s.load(&path).unwrap();
        assert_eq!(s.project.tempo, 100.0);
        render(&mut engine, &mut s, 1024);

        let wav = dir.join("bounce.wav");
        s.export_scene(0, 1, &wav).unwrap();
        let b = rlive_devices::load_wav(&wav).unwrap();
        // 1 bar at 100 BPM = 2.4 s.
        assert!((b.duration_secs() - 2.4).abs() < 0.01);
        assert!(b.left.iter().any(|x| x.abs() > 0.01));
        std::fs::remove_dir_all(&dir).ok();
    }
}
