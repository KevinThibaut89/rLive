//! The session: the project model plus the live objects that mirror it in the engine.
//!
//! Every edit goes through [`Session`] so the project (what gets saved) and the engine (what
//! you hear) never drift apart.

#[cfg(test)]
mod a14_tests;
mod arrange_edit;
mod arrangement;
mod automation;
pub mod automation_edit;
mod bounce;
mod build;
mod capture;
mod clip_edit;
mod clip_envelopes;
mod clip_files;
#[cfg(test)]
mod clip_view_tests;
mod clips;
mod consolidate;
mod devices;
mod export;
mod external;
mod freeze;
mod graph;
mod grooves;
mod info;
mod io;
mod launch;
mod launch_log;
mod modulators;
mod presets;
mod rack_edit;
mod racks;
mod recording;
pub mod remote;
mod render;
mod routing;
mod samples;
mod sampling;
mod takes;
mod timing;
mod tracks;
mod transport;
pub mod tuning;
mod undo;
mod warping;
#[cfg(test)]
mod warping_tests;

#[cfg(test)]
mod arrange_tests;
#[cfg(test)]
mod automation_edit_tests;
#[cfg(test)]
mod automation_tests;
#[cfg(test)]
mod bounce_tests;
#[cfg(test)]
mod clip_envelope_tests;
#[cfg(test)]
mod compensation_tests;
#[cfg(test)]
mod external_tests;
#[cfg(test)]
mod golden;
#[cfg(test)]
mod io_tests;
#[cfg(test)]
mod launch_tests;
#[cfg(test)]
mod modulator_tests;
#[cfg(test)]
mod preset_tests;
#[cfg(test)]
mod rack_tests;
#[cfg(test)]
mod remote_tests;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod sample_tests;
#[cfg(test)]
mod takes_tests;
#[cfg(test)]
mod tests;

pub use arrange_edit::{
    edge_fade_ticks, next_locator, prev_locator, quantized_tick, FadeSide, LoopEdit,
};
pub use arrangement::{ArrMove, TrimEdge};
pub use automation_edit::{EnvGroup, EnvParam};
pub use bounce::{BounceSource, PasteTarget};
pub use build::{descriptor_for, mix_of, time_maps, Builder, DeviceRuntime, PluginWiring};
pub use clip_edit::{ClipDefaults, MultiClip, SetMarker};
pub use clips::ClipRef;
pub use devices::ChainRef;
pub use graph::compile_plan;
pub use info::InfoTarget;
pub use io::{can_arm, AudioHardware, CrossfaderField, CrossfaderKeys, RouteChoice};
pub use launch::{set_follow_on, signature_is_valid, LaunchDefaults};
pub use presets::{SwapItem, SwapTarget};
pub use rack_edit::PadDrop;
pub use racks::{ExtractNotes, MacroDisplay, ZoneKind};
pub use remote::{MapMode, MappingRow, RemotePorts, RemoteState};
pub use render::JobStatus;
pub use rlive_audio_io::SampleCache;
pub use sampling::SamplingCommand;
pub use warping::{ShortSampleMode, WarpDefaults, WarpView, LONG_SAMPLE_SECONDS};

use crate::recording::capture_midi::CaptureBuffer;
use crate::recording::disk::DiskWriter;
use crate::recording::Recording;
use crate::settings::{ColorSettings, RecordSettings};
use rlive_core::{ChainId, DeviceId, PluginFormat, TrackId};
use rlive_engine::shared::{ChainShared, TrackShared};
use rlive_engine::{
    BusLayout, CellIx, ClipCmd, EngineCommand, EngineEvent, EngineHandle, GraphOp, ParamCmd,
    RecordCmd, TrackRef, TransportCmd, MAX_TRACKS,
};
use rlive_plugin_host::scanner::PluginCache;
use rlive_plugin_host::{HostNotification, PluginDescriptor};
use rlive_project::history::History;
#[cfg(test)]
use rlive_project::TrackKind;
use rlive_project::{Device, DeviceKind, Project};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct Session {
    pub project: Project,
    pub path: Option<PathBuf>,
    pub engine: EngineHandle,
    pub runtimes: HashMap<DeviceId, DeviceRuntime>,
    pub track_shared: HashMap<TrackId, Arc<TrackShared>>,
    /// What each rack chain publishes for the UI, by chain.
    pub chain_shared: HashMap<ChainId, Arc<ChainShared>>,
    /// Which cell each parameter lives in, per track and the Master.
    pub layouts: HashMap<ChainRef, BusLayout>,
    pub plugins: PluginCache,
    pub samples: SampleCache,
    /// Tracks that receive live MIDI; shared with the MIDI-input thread.
    pub midi_targets: Arc<Mutex<Vec<TrackId>>>,
    /// The hardware MIDI outputs tracks can play out of, by the number the engine uses.
    pub midi_out_ports: Vec<String>,
    /// Controllers with MPE Mode on: tracks taking input from one hear all its channels.
    pub mpe_ports: std::collections::HashSet<String>,
    pub messages: Vec<String>,
    pub dirty: bool,
    pub history: History,
    /// How new tracks and clips are coloured.
    pub colors: ColorSettings,
    pub warp_defaults: WarpDefaults,
    /// How new clips launch and how edits reach a running one.
    pub clip_defaults: ClipDefaults,
    /// The program Edit opens a sample in.
    pub sample_editor: Option<PathBuf>,
    /// The scale of the clip last shown in the Clip View, which new clips take on.
    pub recent_scale: rlive_project::ClipScale,
    /// The file Manage Sample File asked the File Manager (C13) to show.
    pub file_manager_target: Option<PathBuf>,
    /// Reversing and cropping running off the audio thread, and what they've learned.
    clip_jobs: clip_files::ClipJobs,
    /// A held scrub on a timeline clip: the song tick to go back to and every how many ticks.
    arr_scrub: Option<(f64, f64)>,
    /// The tempo automation the Arrangement's leader clips generate, if there are any.
    leader_points: Vec<rlive_project::Breakpoint>,
    /// Long samples whose new clips are waiting for beat tracking to finish.
    auto_warp_waiting: std::collections::HashSet<PathBuf>,
    /// The Record, Warp & Launch options that recording follows.
    pub record: RecordSettings,
    /// Create Fades on Clip Edges, from the settings. Off until the app says otherwise, so a
    /// project rendered without settings sounds exactly as it is written.
    pub(crate) edge_fades: bool,
    recording: Option<Recording>,
    disk: Option<DiskWriter>,
    /// Audio takes in progress, by track.
    takes: HashMap<TrackId, takes::AudioTake>,
    /// Session takes that are waiting for the disk to catch up.
    next_take: u64,
    /// MIDI takes going into Session slots, by track.
    slot_takes: HashMap<TrackId, takes::MidiSlotTake>,
    /// Keys held on a track being overdubbed.
    overdubs: takes::OverdubState,
    /// The last few minutes of what was played on each armed or monitored track.
    captures: HashMap<TrackId, CaptureBuffer>,
    /// Keys held right now for step recording, by track.
    step_keys: HashMap<TrackId, std::collections::BTreeMap<u8, f32>>,
    /// New files in the Temporary Folder waiting for the Set to be saved, and which folder under
    /// Samples/Processed each belongs in: none means Samples/Recorded.
    temp_recordings: Vec<(PathBuf, Option<render::Processed>)>,
    /// The bounce, freeze or consolidate being rendered.
    render_job: Option<Box<render::RenderJob>>,
    /// Files no step of the history needs any more, to delete with the next save.
    discarded_files: Vec<PathBuf>,
    /// Undo or redo is putting things back, which frozen tracks allow.
    restoring: bool,
    /// Audio the disk couldn't keep up with, in frames.
    pub dropped_frames: u64,
    /// How much a take's ring holds, in seconds of audio.
    take_ring_seconds: f64,
    automation: automation::AutomationRecorder,
    /// The compensation the running plan was compiled for.
    timing: timing::Timing,
    /// The buses the engine has been given.
    bus_shape: timing::BusShape,
    /// Which plug-ins have their extra buses in use.
    wiring: build::PluginWiring,
    /// Where each modulator publishes on the engine's bus.
    mod_slots: modulators::ModSlots,
    /// Every mapping of every modulator, as last compiled.
    mod_plan: modulators::ModPlan,
    /// What the engine has been told drives each track's cells.
    sent_mods: HashMap<ChainRef, Vec<rlive_engine::ModRoute>>,
    /// Devices moved or went: mappings have to be looked at again.
    mods_dirty: bool,
    /// What the engine has been told about each External Instrument's MIDI port.
    sent_device_outputs: HashMap<DeviceId, external::DeviceOutput>,
    /// The Map button waiting for a parameter to be clicked: the modulator and its output.
    armed_mod_map: Option<(DeviceId, usize)>,
    /// Keys held on the crossfader's left, centre and right positions.
    crossfader_keys: io::CrossfaderKeys,
    /// A file waiting to be previewed once it has loaded.
    pending_preview: Option<io::PendingPreview>,
    /// The User Library, where presets and defaults are kept. None in a bare session.
    pub library: Option<rlive_browser::defaults::UserLibrary>,
    /// Keep the last saves of a Set in its Backup folder.
    pub backups: bool,
    /// Device sample changes waiting for their file to load.
    pending_samples: Vec<samples::PendingSample>,
    /// Where the Rand button's numbers come from.
    random: u64,
    /// What new clips start with.
    pub launch_defaults: LaunchDefaults,
    /// The running song's tempo or meter comes from a launched scene: stopping undoes it.
    timing_held: bool,
    /// What Arrangement Record is writing down.
    launch_log: launch_log::LaunchLog,
    /// MIDI and key mapping, and control surfaces.
    pub remote: remote::RemoteState,
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
            chain_shared: HashMap::new(),
            layouts: HashMap::new(),
            plugins,
            samples: SampleCache::default(),
            midi_targets: Arc::new(Mutex::new(Vec::new())),
            midi_out_ports: Vec::new(),
            mpe_ports: std::collections::HashSet::new(),
            messages: Vec::new(),
            dirty: false,
            history,
            colors: ColorSettings::default(),
            warp_defaults: WarpDefaults::default(),
            clip_defaults: ClipDefaults::default(),
            sample_editor: None,
            file_manager_target: None,
            clip_jobs: clip_files::ClipJobs::default(),
            arr_scrub: None,
            leader_points: Vec::new(),
            auto_warp_waiting: Default::default(),
            record: RecordSettings::default(),
            edge_fades: false,
            recording: None,
            disk: None,
            takes: HashMap::new(),
            next_take: 0,
            slot_takes: HashMap::new(),
            overdubs: takes::OverdubState::default(),
            captures: HashMap::new(),
            step_keys: HashMap::new(),
            recent_scale: rlive_project::ClipScale::default(),
            temp_recordings: Vec::new(),
            render_job: None,
            discarded_files: Vec::new(),
            restoring: false,
            dropped_frames: 0,
            take_ring_seconds: takes::RING_SECONDS,
            automation: automation::AutomationRecorder::default(),
            timing: timing::Timing::default(),
            bus_shape: Vec::new(),
            wiring: build::PluginWiring::default(),
            mod_slots: Default::default(),
            mod_plan: Default::default(),
            sent_mods: HashMap::new(),
            mods_dirty: true,
            sent_device_outputs: HashMap::new(),
            armed_mod_map: None,
            crossfader_keys: io::CrossfaderKeys::default(),
            pending_preview: None,
            library: None,
            backups: false,
            pending_samples: Vec::new(),
            random: racks::random_seed(),
            launch_defaults: LaunchDefaults::default(),
            timing_held: false,
            launch_log: launch_log::LaunchLog::default(),
            remote: remote::RemoteState::default(),
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
        // A controller's value got there first, straight from the MIDI thread.
        if self.remote.store_only {
            return;
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
        self.mod_plan = modulators::ModPlan::of(&self.project);
        (
            Builder {
                sample_rate: self.engine.sample_rate,
                max_block: self.engine.max_block,
                plugins: &self.plugins,
                samples: &mut self.samples,
                wiring: &self.wiring,
                mod_slots: &mut self.mod_slots,
                mod_plan: &self.mod_plan,
                chain_shared: Some(&mut self.chain_shared),
                edge_fade: self
                    .edge_fades
                    .then(|| arrange_edit::edge_fade_ticks(self.project.tempo)),
                frozen: false,
                grooves: &self.project.groove_pool,
                tuning_loaded: self.project.tuning.is_some(),
            },
            &self.project,
        )
    }

    /// Re-create every engine object from the project (load, new, sample-rate change).
    pub fn rebuild(&mut self) {
        self.sent_device_outputs.clear();
        self.mods_dirty = true;
        self.abandon_takes();
        self.recording = None;
        self.automation.abandon();
        self.capture_plugin_states();
        self.runtimes.clear();
        self.project.normalize();
        self.wiring = build::PluginWiring::of(&compile_plan(&self.project));
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
        self.note_clip_routes();
        self.messages.extend(errors);
        self.send_track_edits(vec![GraphOp::ReplaceAll {
            tracks: Box::new(tracks),
            master,
        }]);
        self.leader_points = self.project.leader_tempo_points();
        let p = &self.project;
        let rate = self.clip_defaults.update_rate;
        let cmds = [
            EngineCommand::Clip(ClipCmd::SetUpdateRate(rate)),
            EngineCommand::Transport(TransportCmd::SetTimeMaps(Arc::new(build::time_maps(p)))),
            EngineCommand::Transport(TransportCmd::SetQuantize(p.quantize)),
            EngineCommand::Transport(TransportCmd::SetMetronome(p.metronome)),
            EngineCommand::Transport(TransportCmd::SetMetronomeConfig(build::metronome_config(p))),
            EngineCommand::Record(RecordCmd::SetCountIn(p.metronome_options.count_in)),
        ];
        for c in cmds {
            self.send(c);
        }
        self.send_grooves();
        self.send_tuning();
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for track in tracks {
            self.send_midi_output(track);
        }
        self.send_automation_arm();
        self.sync_scenes();
        self.send_follow_enabled();
        let seed = racks::random_seed();
        self.send(EngineCommand::Launch(rlive_engine::LaunchCmd::SeedFollow(
            seed,
        )));
        self.timing_held = false;
        self.launch_log = launch_log::LaunchLog::default();
        self.sync_loop();
        self.update_midi_targets();
    }

    /// Swap in a new engine (e.g. after changing the audio device) and rebuild.
    pub fn replace_engine(&mut self, engine: EngineHandle) {
        self.capture_plugin_states();
        self.engine = engine;
        self.bus_shape.clear();
        self.rebuild();
    }

    // ------------------------------------------------------------- files

    pub fn new_project(&mut self) {
        self.render_job = None;
        self.delete_discarded_files();
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
        self.render_job = None;
        self.delete_discarded_files();
        self.project = p;
        self.find_relative_paths(path);
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
        self.adopt_temp_recordings(&path);
        self.adopt_temp_processed(&path);
        self.note_relative_paths(&path);
        if self.backups && path.exists() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let trash = rlive_browser::backup::trash_dir();
            if let Err(e) = rlive_browser::backup::back_up(&path, now, trash.as_deref()) {
                log::warn!("could not back the Set up: {e}");
            }
        }
        self.project.save(&path).map_err(|e| e.to_string())?;
        self.path = Some(path);
        self.commit_now();
        self.history.mark_saved();
        self.dirty = false;
        self.delete_discarded_files();
        Ok(())
    }

    // ------------------------------------------------------------- housekeeping

    /// Call every UI frame: frees engine garbage and services plugin callbacks.
    pub fn idle(&mut self) {
        if self.mods_dirty {
            self.sync_mod_routes();
            self.sync_device_outputs();
        }
        self.poll_remote();
        self.engine.collect_garbage();
        self.poll_samples();
        self.poll_preview();
        while let Some(note) = self.engine.poll_played() {
            self.played_midi(note);
        }
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
                } => self.recorded_midi(track, tick, message),
                EngineEvent::OverdubMidi {
                    track,
                    slot,
                    position,
                    tick,
                    message,
                } => self.overdub_midi(track, slot, position, tick, message),
                EngineEvent::CaptureSegment {
                    track,
                    frame,
                    tick,
                    jump,
                    ticks_per_frame,
                } => self.capture_segment(track, frame, tick, jump, ticks_per_frame),
                EngineEvent::CaptureClosed { track, frames } => self.capture_closed(track, frames),
                EngineEvent::SlotRecordStarted { track, tick, .. } => {
                    self.slot_record_started(track, tick)
                }
                EngineEvent::SlotClosing {
                    track,
                    slot,
                    at,
                    launch,
                    frames,
                } => self.slot_closing(track, slot, at, launch, frames),
                EngineEvent::SlotRecordEnded { track, slot, tick } => {
                    self.slot_record_ended(track, slot, tick)
                }
                EngineEvent::Jumped { from, to } => {
                    self.launch_log_jumped(from, to);
                    if let Some(r) = self.recording.as_mut() {
                        r.jumped(from, to);
                    }
                    self.flush_recording();
                    self.automation_jumped(from, to);
                }
                EngineEvent::RecordingStopped { tick } => {
                    self.close_launch_log(tick);
                    self.recording_stopped(tick);
                    self.finish_automation_pass(tick);
                }
                EngineEvent::SceneTiming {
                    tick,
                    tempo,
                    signature,
                    ..
                } => self.scene_timing(tick, tempo, signature),
                EngineEvent::SessionPlayback {
                    track,
                    slot,
                    at,
                    start_tick,
                } => self.session_playback(track, slot, at, start_tick),
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
                EngineEvent::ClipAutomationMoved {
                    track,
                    slot,
                    cell,
                    value,
                    before,
                    position,
                } => self.clip_automation_moved(track, slot, cell, (value, before), position),
                EngineEvent::ClipAutomationReleased {
                    track,
                    cell,
                    position,
                    ..
                } => self.clip_automation_released(track, cell, position),
                EngineEvent::ClipAutomationEnded {
                    target,
                    cell,
                    value,
                } => self.clip_automation_ended(target, cell, value),
            }
        }
        self.release_scene_timing();
        self.poll_clip_strokes();
        self.sync_session_tracks();
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
        self.step_render(render::SLICE);
        self.poll_disk();
        self.poll_clip_jobs();
        self.poll_external_edits();
        self.poll_scrub();
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
