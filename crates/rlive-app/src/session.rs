//! The session: the project model plus the live objects that mirror it in the engine.
//!
//! Every edit goes through [`Session`] so the project (what gets saved) and the engine (what
//! you hear) never drift apart.

use rlive_core::{
    DeviceId, MidiMessage, ParamInfo, PluginFormat, Processor, Quantize, TimeSignature, TrackId,
};
use rlive_devices::{DeviceInit, DeviceRole, SampleBuffer};
use rlive_engine::shared::TrackShared;
use rlive_engine::{
    AudioClip, ChainTarget, ClipData, DeviceSlot, EngineCommand, EngineEvent, EngineHandle,
    MasterNode, MidiClip, MixSettings, Note, NullProcessor, TrackNode, MAX_TRACKS,
};
use rlive_plugin_host::scanner::PluginCache;
use rlive_plugin_host::{HostNotification, PluginDescriptor, PluginHandle};
use rlive_project::{Clip, ClipContent, Device, DeviceKind, Project, Track, TrackKind};
use std::collections::HashMap;
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
}

impl SampleCache {
    pub fn get(&mut self, path: &Path) -> Result<Arc<SampleBuffer>, String> {
        if let Some(s) = self.samples.get(path) {
            return Ok(s.clone());
        }
        let s = Arc::new(
            rlive_devices::load_wav(path).map_err(|e| format!("{}: {e}", path.display()))?,
        );
        self.samples.insert(path.to_path_buf(), s.clone());
        Ok(s)
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
                let mut notes: Vec<Note> = notes
                    .iter()
                    .map(|n| Note {
                        start: n.start,
                        length: n.length,
                        key: n.key,
                        velocity: n.velocity,
                    })
                    .collect();
                notes.sort_by_key(|n| (n.start, n.key));
                ClipData::Midi(MidiClip {
                    length: clip.length,
                    notes,
                    looped: clip.looped,
                })
            }
            ClipContent::Audio { path, gain_db } => {
                let s = self.samples.get(path)?;
                ClipData::Audio(AudioClip {
                    left: s.left.clone(),
                    right: s.right.clone(),
                    sample_rate: s.sample_rate,
                    length: clip.length,
                    looped: clip.looped,
                    gain: rlive_core::db_to_gain(*gain_db),
                })
            }
        }))
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
}

impl Session {
    pub fn new(engine: EngineHandle, project: Project, plugins: PluginCache) -> Self {
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
        self.path = None;
        self.dirty = false;
        self.rebuild();
    }

    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        let p = Project::load(path).map_err(|e| e.to_string())?;
        self.project = p;
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
        self.dirty = true;
    }

    pub fn set_time_signature(&mut self, sig: TimeSignature) {
        self.project.time_signature = sig;
        self.send(EngineCommand::SetTimeSignature(sig));
        self.dirty = true;
    }

    pub fn set_quantize(&mut self, q: Quantize) {
        self.project.quantize = q;
        self.send(EngineCommand::SetQuantize(q));
        self.dirty = true;
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
        let index = index
            .unwrap_or(self.project.tracks.len())
            .min(self.project.tracks.len());
        let shared = Arc::new(TrackShared::default());
        self.track_shared.insert(id, shared.clone());
        let mut errors = Vec::new();
        let mut runtimes = HashMap::new();
        let (mut b, _) = self.builder();
        let node = b.track(&t, shared, &mut runtimes, &mut errors);
        self.runtimes.extend(runtimes);
        self.messages.extend(errors);
        self.project.tracks.insert(index, t);
        self.send(EngineCommand::AddTrack { index, track: node });
        self.dirty = true;
        id
    }

    pub fn delete_track(&mut self, id: TrackId) {
        if let Some(i) = self.project.track_index(id) {
            let t = self.project.tracks.remove(i);
            for d in &t.devices {
                self.runtimes.remove(&d.id);
            }
            self.track_shared.remove(&id);
            self.send(EngineCommand::RemoveTrack(id));
            self.update_midi_targets();
            self.dirty = true;
        }
    }

    pub fn move_track(&mut self, id: TrackId, to: usize) {
        if let Some(i) = self.project.track_index(id) {
            let t = self.project.tracks.remove(i);
            let to = to.min(self.project.tracks.len());
            self.project.tracks.insert(to, t);
            self.send(EngineCommand::MoveTrack { track: id, to });
            self.dirty = true;
        }
    }

    /// Push the track's volume/pan/mute/solo to the engine after editing the project.
    pub fn sync_mix(&mut self, id: TrackId) {
        if let Some(t) = self.project.track(id) {
            let mix = mix_of(t);
            self.send(EngineCommand::SetMix { track: id, mix });
            self.dirty = true;
        }
    }

    pub fn set_master_volume(&mut self, db: f32) {
        self.project.master.volume_db = db;
        self.send(EngineCommand::SetMasterVolume(db));
        self.dirty = true;
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

    // ------------------------------------------------------------- scenes & clips

    pub fn add_scene(&mut self) {
        self.project.add_scene();
        self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
    }

    pub fn clip(&self, track: TrackId, slot: usize) -> Option<&Clip> {
        self.project.track(track)?.clips.get(slot)?.as_ref()
    }

    pub fn clip_mut(&mut self, track: TrackId, slot: usize) -> Option<&mut Clip> {
        self.project.track_mut(track)?.clips.get_mut(slot)?.as_mut()
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
        self.dirty = true;
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
            devs.retain(|d| d.id != id);
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
        self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
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
                    self.dirty = true;
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
        let cmds = vec![
            EngineCommand::ReplaceAll {
                tracks: Box::new(tracks),
                master,
            },
            EngineCommand::SetTempo(p.tempo),
            EngineCommand::SetTimeSignature(p.time_signature),
            EngineCommand::SetQuantize(Quantize::None),
            EngineCommand::LaunchScene(scene),
        ];
        for c in cmds {
            if handle.send(c).is_err() {
                return Err("engine queue full".into());
            }
        }
        let seconds =
            bars as f64 * p.time_signature.ticks_per_bar() as f64 / rlive_core::PPQ as f64 * 60.0
                / p.tempo;
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
