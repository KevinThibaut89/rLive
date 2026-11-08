//! The session: the project model plus the live objects that mirror it in the engine.
//!
//! Every edit goes through [`Session`] so the project (what gets saved) and the engine (what
//! you hear) never drift apart.

mod arrangement;
mod automation;
mod build;
mod clips;
mod devices;
mod export;
mod graph;
mod info;
mod recording;
mod routing;
mod samples;
mod timing;
mod tracks;
mod transport;
mod undo;

#[cfg(test)]
mod automation_tests;
#[cfg(test)]
mod compensation_tests;
#[cfg(test)]
mod golden;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod sample_tests;
#[cfg(test)]
mod tests;

pub use arrangement::{ArrMove, TrimEdge};
pub use build::{descriptor_for, mix_of, Builder, DeviceRuntime};
pub use clips::ClipRef;
pub use devices::ChainRef;
pub use graph::compile_plan;
pub use info::InfoTarget;
pub use rlive_audio_io::SampleCache;

use crate::recording::Recording;
use crate::settings::ColorSettings;
use rlive_core::{DeviceId, PluginFormat, TrackId};
use rlive_engine::shared::TrackShared;
use rlive_engine::{
    BusLayout, CellIx, EngineCommand, EngineEvent, EngineHandle, GraphOp, ParamCmd, TrackRef,
    TransportCmd, MAX_TRACKS,
};
use rlive_plugin_host::scanner::PluginCache;
use rlive_plugin_host::{HostNotification, PluginDescriptor};
use rlive_project::history::History;
use rlive_project::{Device, DeviceKind, Project, TrackKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct Session {
    pub project: Project,
    pub path: Option<PathBuf>,
    pub engine: EngineHandle,
    pub runtimes: HashMap<DeviceId, DeviceRuntime>,
    pub track_shared: HashMap<TrackId, Arc<TrackShared>>,
    /// Which cell each parameter lives in, per track and the Master.
    pub layouts: HashMap<ChainRef, BusLayout>,
    pub plugins: PluginCache,
    pub samples: SampleCache,
    /// Tracks that receive live MIDI; shared with the MIDI-input thread.
    pub midi_targets: Arc<Mutex<Vec<TrackId>>>,
    pub messages: Vec<String>,
    pub dirty: bool,
    pub history: History,
    /// How new tracks and clips are coloured.
    pub colors: ColorSettings,
    recording: Option<Recording>,
    automation: automation::AutomationRecorder,
    /// The compensation the running plan was compiled for.
    timing: timing::Timing,
    /// Device sample changes waiting for their file to load.
    pending_samples: Vec<samples::PendingSample>,
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
            layouts: HashMap::new(),
            plugins,
            samples: SampleCache::default(),
            midi_targets: Arc::new(Mutex::new(Vec::new())),
            messages: Vec::new(),
            dirty: false,
            history,
            colors: ColorSettings::default(),
            recording: None,
            automation: automation::AutomationRecorder::default(),
            timing: timing::Timing::default(),
            pending_samples: Vec::new(),
        };
        s.preload_samples();
        s.rebuild();
        s
    }

    /// Send a parameter's new value to the engine, keeping the layout's copy up to date.
    pub(super) fn set_cell(&mut self, chain: ChainRef, cell: CellIx, value: f64, gesture: bool) {
        if let Some(l) = self.layouts.get_mut(&chain.owner()) {
            l.note_value(cell, value);
        }
        if gesture {
            self.note_gesture(chain, cell);
        }
        self.send(EngineCommand::Param(ParamCmd::Set {
            track: chain.into(),
            cell,
            value,
            gesture,
        }));
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
        self.automation.abandon();
        self.capture_plugin_states();
        self.runtimes.clear();
        self.project.normalize();
        let mut runtimes = HashMap::new();
        let mut errors = Vec::new();
        let mut layouts = HashMap::new();
        let mut shared_map = HashMap::new();
        let mut tracks = Vec::with_capacity(MAX_TRACKS);
        let (mut b, project) = self.builder();
        let returns: Vec<TrackId> = project.returns.iter().map(|t| t.id).collect();
        for t in project.tracks.iter().chain(&project.returns) {
            let shared = Arc::new(TrackShared::default());
            shared_map.insert(t.id, shared.clone());
            tracks.push(b.track(
                t,
                &returns,
                shared,
                &mut runtimes,
                &mut layouts,
                &mut errors,
            ));
        }
        let master = b.master(project, &mut runtimes, &mut layouts, &mut errors);
        self.runtimes = runtimes;
        self.layouts = layouts;
        self.track_shared = shared_map;
        self.messages.extend(errors);
        self.send_track_edits(vec![GraphOp::ReplaceAll {
            tracks: Box::new(tracks),
            master,
        }]);
        let p = &self.project;
        let cmds = [
            EngineCommand::Transport(TransportCmd::SetTimeMaps(Arc::new(build::time_maps(p)))),
            EngineCommand::Transport(TransportCmd::SetQuantize(p.quantize)),
            EngineCommand::Transport(TransportCmd::SetMetronome(p.metronome)),
        ];
        for c in cmds {
            self.send(c);
        }
        self.send_automation_arm();
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
        self.samples.forget_failed();
        self.history = History::new(&self.project);
        self.pending_samples.clear();
        self.path = None;
        self.dirty = false;
        self.rebuild();
    }

    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        let p = Project::load(path).map_err(|e| e.to_string())?;
        self.project = p;
        self.samples.forget_failed();
        self.history = History::new(&self.project);
        self.pending_samples.clear();
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
        let p = &mut self.project;
        for t in p.tracks.iter_mut().chain(p.returns.iter_mut()) {
            t.devices
                .iter_mut()
                .for_each(|d| d.for_each_mut(&mut capture));
        }
        p.master
            .devices
            .iter_mut()
            .for_each(|d| d.for_each_mut(&mut capture));
    }

    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        self.capture_plugin_states();
        self.record_fingerprints();
        let path = if path.extension().is_none() {
            path.with_extension(rlive_project::FILE_EXTENSION)
        } else {
            path.to_path_buf()
        };
        self.project.save(&path).map_err(|e| e.to_string())?;
        self.path = Some(path);
        self.commit_now();
        self.history.mark_saved();
        self.dirty = false;
        Ok(())
    }

    // ------------------------------------------------------------- housekeeping

    /// Call every UI frame: frees engine garbage and services plugin callbacks.
    pub fn idle(&mut self) {
        self.engine.collect_garbage();
        self.poll_samples();
        while let Some(ev) = self.engine.poll_event() {
            match ev {
                EngineEvent::ParamChanged {
                    target,
                    device,
                    param,
                    value,
                } => {
                    let chain = match target {
                        TrackRef::Track(t) => ChainRef::Track(t),
                        TrackRef::Master => ChainRef::Master,
                    };
                    let chain = self.chain_of_device(device).unwrap_or(chain);
                    self.report_device_param(chain, device, param, value);
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
                    self.automation_jumped(from, to);
                }
                EngineEvent::RecordingStopped { tick } => {
                    if let Some(r) = self.recording.as_mut() {
                        r.finish(tick);
                    }
                    self.flush_recording();
                    self.recording = None;
                    self.finish_automation_pass(tick);
                }
                EngineEvent::AutomationMoved {
                    target,
                    cell,
                    value,
                    before,
                    tick,
                } => self.automation_moved(target, cell, value, before, tick),
                EngineEvent::AutomationReleased { target, cell, tick } => {
                    self.automation_released(target, cell, tick)
                }
            }
        }
        let mut todo = Vec::new();
        for (id, rt) in self.runtimes.iter_mut() {
            if let Some(h) = rt.plugin.as_mut() {
                for n in h.idle() {
                    if n == HostNotification::ParamsRescanned {
                        rt.params = h.params();
                    }
                    todo.push((*id, n));
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
                    self.report_device_param(chain, id, param, value);
                    if let Some(cell) = self.device_cell(chain, id, param) {
                        self.set_cell(chain, cell, value, true);
                    }
                    self.touch();
                }
                HostNotification::ParamsRescanned => self.refit_device_cells(chain, id),
                HostNotification::RestartRequested => self.reload_device(chain, id),
                HostNotification::LatencyChanged => {
                    if let Some(rt) = self.runtimes.get(&id) {
                        let latency = rt.plugin.as_ref().map_or(0, |h| h.latency());
                        rt.latency
                            .store(latency, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                HostNotification::EditorClosed => {}
            }
        }
        self.follow_latency();
    }

    /// Descriptors of all known plugins of a format.
    pub fn plugins_of(&self, format: PluginFormat) -> impl Iterator<Item = &PluginDescriptor> {
        self.plugins
            .plugins
            .iter()
            .filter(move |p| p.format == format)
    }
}
