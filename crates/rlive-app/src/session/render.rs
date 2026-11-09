//! Rendering part of a Set on a private engine, for Export, Bounce, Freeze and Consolidate.
//!
//! A render is a copy of the project, changed so that what reaches the Main output is exactly the
//! signal being committed, played from somewhere for so long. A job is a list of renders and what
//! to do with their audio. It runs a slice at a time between UI frames, so the app stays alive
//! while it works and the project only changes when all of it has finished.

use super::graph::{compile_with, rack_out_ops};
use super::takes::{default_temp_folder, file_name};
use super::timing::{bus_shape, buses_op, compensation_ops, RuntimeLatency, Timing};
use super::{Builder, DeviceRuntime, Session};
use rlive_audio_io::{AudioWriter, BitDepth, FileType, SampleStatus};
use rlive_core::param::{mixer, EnvTarget, ParamOwner, ParamRef, ACTIVATOR};
use rlive_core::time::TempoMap;
use rlive_core::{DeviceId, Quantize, TrackId, PPQ};
use rlive_engine::shared::TrackShared;
use rlive_engine::{
    Engine, EngineCommand, EngineHandle, GraphOp, GraphTxn, LaunchCmd, OutputPair, TransportCmd,
    MAX_TRACKS,
};
use rlive_project::{
    Clip, Crossfade, Device, Monitor, PanMode, Project, Route, Track, TrackDelay, TrackSend,
    WarpMarker, SEND_OFF_DB,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// How much of each UI frame a job may take.
pub(super) const SLICE: Duration = Duration::from_millis(8);
const BLOCK: usize = 512;
/// How long a render waits for its delay compensation to reach its lengths.
const DELAY_SETTLE_BLOCKS: usize = 8;
/// Below this a tail counts as over: about -100 dB.
const TAIL_SILENCE: f32 = 1e-5;
/// How long a tail has to stay silent to be over, in seconds.
const TAIL_QUIET_SECONDS: f64 = 0.5;
/// The most a tail can add to a render, in seconds.
const TAIL_MAX_SECONDS: f64 = 30.0;

/// The folders under Samples/Processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Processed {
    Bounce,
    Consolidate,
    Freeze,
}

impl Processed {
    fn folder(self) -> &'static str {
        match self {
            Processed::Bounce => "Bounce",
            Processed::Consolidate => "Consolidate",
            Processed::Freeze => "Freeze",
        }
    }
}

/// Where processed files go: the Set's own folder, or the temporary folder until it has one.
pub(super) fn processed_folder(
    project_file: Option<&Path>,
    temp: Option<&Path>,
    kind: Processed,
) -> PathBuf {
    let base = match project_file.and_then(Path::parent) {
        Some(dir) => dir.join("Samples"),
        None => temp.map_or_else(default_temp_folder, Path::to_path_buf),
    };
    base.join("Processed").join(kind.folder())
}

/// Audio out of a render.
#[derive(Debug, Clone, Default)]
pub(super) struct Rendered {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Rendered {
    pub fn frames(&self) -> usize {
        self.left.len()
    }
}

/// One thing to render.
pub(super) struct Render {
    pub project: Project,
    pub setup: Vec<EngineCommand>,
    /// The song position the render begins at.
    pub start: u64,
    pub ticks: u64,
    /// Whether to keep going after `ticks` until the sound has died away.
    pub tail: bool,
}

/// Commands that play the timeline from `start`.
pub(super) fn from_tick(start: u64) -> Vec<EngineCommand> {
    vec![
        EngineCommand::Transport(TransportCmd::SetLoop(None)),
        EngineCommand::Transport(TransportCmd::Seek(start as f64)),
        EngineCommand::Transport(TransportCmd::Play),
    ]
}

/// A command that launches one Session clip.
pub(super) fn launch_clip(track: TrackId, slot: usize) -> Vec<EngineCommand> {
    vec![EngineCommand::Launch(LaunchCmd::LaunchClip { track, slot })]
}

/// A command that launches a whole scene.
pub(super) fn launch_scene(scene: usize) -> Vec<EngineCommand> {
    vec![EngineCommand::Launch(LaunchCmd::LaunchScene(scene))]
}

/// A render under way on its private engine.
struct Pass {
    engine: Engine,
    handle: EngineHandle,
    _runtimes: HashMap<DeviceId, DeviceRuntime>,
    left: Vec<f32>,
    right: Vec<f32>,
    at: usize,
    lead_in: usize,
    /// Where the asked-for length ends, in frames including the lead-in.
    nominal: usize,
    /// Still listening for the end of the tail.
    tail: Option<TailWatch>,
}

struct TailWatch {
    /// One past the last frame known to be audible.
    audible_end: usize,
    quiet: usize,
    limit: usize,
}

impl Pass {
    /// Runs until the time is up or the render is done. Returns whether it is done.
    fn run(&mut self, until: Option<Instant>, max_frames: usize) -> bool {
        let mut budget = max_frames;
        loop {
            if self.at >= self.left.len() && !self.extend_tail() {
                return true;
            }
            if budget == 0 {
                return false;
            }
            let end = (self.at + BLOCK).min(self.left.len());
            let (l, r) = (&mut self.left[self.at..end], &mut self.right[self.at..end]);
            self.engine.process_stereo(l, r);
            self.handle.collect_garbage();
            budget = budget.saturating_sub(end - self.at);
            self.at = end;
            if until.is_some_and(|u| Instant::now() >= u) {
                return self.at >= self.left.len() && self.tail.is_none();
            }
        }
    }

    /// Makes room for more of the tail. Returns false once there is no more to come.
    fn extend_tail(&mut self) -> bool {
        let Some(watch) = &mut self.tail else {
            return false;
        };
        let from = watch.audible_end.max(self.nominal);
        for i in from..self.left.len() {
            if self.left[i].abs() > TAIL_SILENCE || self.right[i].abs() > TAIL_SILENCE {
                watch.audible_end = i + 1;
            }
        }
        if self.left.len() >= watch.limit
            || (self.left.len() > self.nominal
                && self.left.len() - watch.audible_end.max(self.nominal) >= watch.quiet)
        {
            let keep = watch.audible_end.max(self.nominal);
            self.left.truncate(keep);
            self.right.truncate(keep);
            self.at = keep;
            self.tail = None;
            return false;
        }
        let grown = (self.left.len() + BLOCK * 8).min(watch.limit);
        self.left.resize(grown, 0.0);
        self.right.resize(grown, 0.0);
        true
    }

    fn finish(mut self) -> Rendered {
        self.left.drain(..self.lead_in);
        self.right.drain(..self.lead_in);
        Rendered {
            left: self.left,
            right: self.right,
        }
    }
}

type Apply = Box<dyn FnOnce(&mut Session, Vec<Rendered>) -> Result<(), String>>;

/// A set of renders and what becomes of them.
pub(super) struct RenderJob {
    label: &'static str,
    waiting: VecDeque<Render>,
    open: Option<Pass>,
    done: Vec<Rendered>,
    apply: Option<Apply>,
    total: usize,
    finished: usize,
    /// Set when this has to run no faster than the music: hardware is in the signal path.
    realtime: Option<Instant>,
}

/// What the status display shows about a running job.
#[derive(Debug, Clone, PartialEq)]
pub struct JobStatus {
    pub label: &'static str,
    /// 0 to 1.
    pub fraction: f32,
    pub realtime: bool,
}

/// The seconds of a span of the song.
fn span_seconds(project: &Project, maps: &TempoMap, start: u64, ticks: u64) -> f64 {
    if project.tempo_points_in_force().is_empty() {
        ticks as f64 / PPQ as f64 * 60.0 / project.tempo
    } else {
        maps.tick_to_seconds((start + ticks) as f64) - maps.tick_to_seconds(start as f64)
    }
}

impl Session {
    /// Builds the private engine for a render. Waits for the samples it needs.
    fn open_pass(&mut self, render: Render) -> Result<Pass, String> {
        let Render {
            project,
            setup,
            start,
            ticks,
            tail,
        } = render;
        let mut paths = HashSet::new();
        project.for_each_file_ref(&mut |file| {
            paths.insert(file.path.clone());
        });
        // A render must have every sample in it, so this is the one place that waits for them.
        self.samples.wait_for(paths.iter().map(PathBuf::as_path));
        for path in &paths {
            if let Some(SampleStatus::Failed(reason)) = self.samples.status(path) {
                self.messages
                    .push(format!("Rendered without a sample: {reason}"));
            }
        }
        let sample_rate = self.engine.sample_rate;
        let (mut engine, mut handle) = rlive_engine::create_engine(sample_rate, BLOCK);
        let mut runtimes = HashMap::new();
        let mut layouts = HashMap::new();
        let mut errors = Vec::new();
        let mut b = Builder {
            sample_rate,
            max_block: BLOCK,
            plugins: &self.plugins,
            samples: &mut self.samples,
            wiring: &self.wiring,
            chain_shared: None,
            edge_fade: self
                .edge_fades
                .then(|| super::arrange_edit::edge_fade_ticks(project.tempo)),
            frozen: false,
            grooves: &project.groove_pool,
            tuning_loaded: project.tuning.is_some(),
        };
        let mut tracks = Vec::with_capacity(MAX_TRACKS);
        let returns: Vec<_> = project.returns.iter().map(|t| t.id).collect();
        for t in project.tracks.iter().chain(&project.returns) {
            tracks.push(b.track(
                t,
                &returns,
                Arc::new(TrackShared::default()),
                &mut runtimes,
                &mut layouts,
                &mut errors,
            ));
        }
        let master = b.master(&project, &mut runtimes, &mut layouts, &mut errors);
        self.messages.extend(errors);
        let maps = super::build::time_maps(&project);
        let timing = Timing::measure(&project, sample_rate, &RuntimeLatency(&runtimes));
        let mut plan = compile_with(&project, &timing);
        // Whatever the hardware is, a render is the Main output on two channels.
        plan.main_out = OutputPair::default();
        plan.cue = None;
        // The render starts this much late, and is this much longer, for the audio that leaves
        // the Master in its first frames.
        let lead_in = plan.output_latency as usize;
        let mut ops = vec![GraphOp::ReplaceAll {
            tracks: Box::new(tracks),
            master,
        }];
        ops.push(buses_op(&bus_shape(&plan), BLOCK));
        ops.extend(compensation_ops(&plan, &timing, &project, BLOCK));
        ops.extend(rack_out_ops(&project, BLOCK));
        let cmds = vec![
            EngineCommand::Graph(GraphTxn::new(ops, Some(plan))),
            EngineCommand::Transport(TransportCmd::SetTimeMaps(Arc::new(maps.clone()))),
            EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::None)),
            EngineCommand::Clip(rlive_engine::ClipCmd::SetGrooves(
                super::grooves::groove_bank(&project.groove_pool),
            )),
        ];
        let tuning = super::tuning::tuning_commands(&project);
        for c in cmds.into_iter().chain(tuning) {
            if handle.send(c).is_err() {
                return Err("engine queue full".into());
            }
        }
        // Compensating delays fade to their lengths when a plan goes in. Let them get there
        // while nothing is playing, or the first of the audio leaks through them undelayed.
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        for _ in 0..DELAY_SETTLE_BLOCKS {
            engine.process_stereo(&mut l, &mut r);
            handle.collect_garbage();
        }
        for c in setup {
            if handle.send(c).is_err() {
                return Err("engine queue full".into());
            }
        }
        let seconds = span_seconds(&project, &maps.tempo, start, ticks);
        let nominal = (seconds * sample_rate) as usize + lead_in;
        let tail = tail.then(|| TailWatch {
            audible_end: nominal,
            quiet: (TAIL_QUIET_SECONDS * sample_rate) as usize,
            limit: nominal + (TAIL_MAX_SECONDS * sample_rate) as usize,
        });
        // The engine owns the audio from here; nothing else needs this project.
        drop(project);
        Ok(Pass {
            engine,
            handle,
            _runtimes: runtimes,
            left: vec![0.0; nominal],
            right: vec![0.0; nominal],
            at: 0,
            lead_in,
            nominal,
            tail,
        })
    }

    /// Gets the project ready to be copied for a render: every plug-in's state in it, every file
    /// loaded and analysed, and what that analysis decides (auto-warping) applied to its clips.
    pub(super) fn prepare_render(&mut self) {
        self.capture_plugin_states();
        let paths = self.project_audio_paths();
        self.samples.wait_for(paths.iter().map(PathBuf::as_path));
        self.poll_samples();
    }

    /// Runs one render to its end, there and then.
    pub(super) fn render_now(&mut self, render: Render) -> Result<Rendered, String> {
        let mut pass = self.open_pass(render)?;
        while !pass.run(None, usize::MAX) {}
        Ok(pass.finish())
    }

    /// Starts a job. Its renders use the state of the project as it is now.
    pub(super) fn start_render_job(
        &mut self,
        label: &'static str,
        renders: Vec<Render>,
        realtime: bool,
        apply: Apply,
    ) -> Result<(), String> {
        if self.render_job.is_some() {
            return Err("Something is already being rendered".into());
        }
        let sample_rate = self.engine.sample_rate;
        let total = renders
            .iter()
            .map(|r| {
                let maps = super::build::time_maps(&r.project);
                (span_seconds(&r.project, &maps.tempo, r.start, r.ticks) * sample_rate) as usize
            })
            .sum();
        self.render_job = Some(Box::new(RenderJob {
            label,
            waiting: renders.into(),
            open: None,
            done: Vec::new(),
            apply: Some(apply),
            total,
            finished: 0,
            realtime: realtime.then(Instant::now),
        }));
        Ok(())
    }

    /// What the running job is doing, if there is one.
    pub fn render_status(&self) -> Option<JobStatus> {
        let job = self.render_job.as_deref()?;
        let open = job
            .open
            .as_ref()
            .map_or(0, |p| p.at.saturating_sub(p.lead_in));
        Some(JobStatus {
            label: job.label,
            fraction: ((job.finished + open) as f32 / job.total.max(1) as f32).clamp(0.0, 1.0),
            realtime: job.realtime.is_some(),
        })
    }

    /// Gives up on the running job. Nothing it was working on is kept.
    pub fn cancel_render(&mut self) {
        self.render_job = None;
    }

    /// Does some of the running job, for at most `slice`. A job with hardware in its path goes
    /// no faster than the music. Returns whether it is still running.
    pub fn step_render(&mut self, slice: Duration) -> bool {
        let until = Instant::now() + slice;
        if let Some(Err(e)) = self.advance_job(Some(until), true) {
            self.messages.push(e);
        }
        self.render_job.is_some()
    }

    /// Finishes the running job now, however long that takes, and says how it went.
    pub fn finish_render(&mut self) -> Result<(), String> {
        loop {
            match self.advance_job(None, false) {
                Some(result) => return result,
                None if self.render_job.is_none() => return Ok(()),
                None => {}
            }
        }
    }

    /// Runs the job until `until`. Returns its outcome once it has ended.
    fn advance_job(&mut self, until: Option<Instant>, pace: bool) -> Option<Result<(), String>> {
        let mut job = self.render_job.take()?;
        let sample_rate = self.engine.sample_rate;
        loop {
            if job.open.is_none() {
                let Some(next) = job.waiting.pop_front() else {
                    break;
                };
                match self.open_pass(next) {
                    Ok(pass) => job.open = Some(pass),
                    Err(e) => return Some(Err(e)),
                }
            }
            let allowed = match job.realtime.filter(|_| pace) {
                Some(started) => {
                    let heard = (started.elapsed().as_secs_f64() * sample_rate) as usize;
                    let so_far = job.finished + job.open.as_ref().map_or(0, |p| p.at);
                    heard.saturating_sub(so_far)
                }
                None => usize::MAX,
            };
            let pass = job.open.as_mut().expect("opened above");
            let done = pass.run(until, allowed);
            if done {
                let pass = job.open.take().expect("opened above");
                let rendered = pass.finish();
                job.finished += rendered.frames();
                job.done.push(rendered);
                continue;
            }
            if until.is_some() || allowed == 0 {
                self.render_job = Some(job);
                return None;
            }
        }
        let done = std::mem::take(&mut job.done);
        let apply = job.apply.take().expect("a job applies once");
        Some(apply(self, done))
    }

    /// Writes audio into the folder it belongs in, and has it ready to play.
    pub(super) fn write_processed(
        &mut self,
        kind: Processed,
        stem: &str,
        audio: &Rendered,
        format: (FileType, BitDepth),
    ) -> Result<PathBuf, String> {
        let dir = processed_folder(
            self.path.as_deref(),
            self.record.temp_folder.as_deref(),
            kind,
        );
        // Numbered across the process: every session writing here gets a name of its own.
        let number = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(file_name(stem, number, format.0));
        let fail = |e: std::io::Error| format!("{}: {e}", path.display());
        let rate = self.engine.sample_rate as u32;
        let mut w = AudioWriter::create(&path, format.0, format.1, 2, rate).map_err(fail)?;
        let mut frame = Vec::with_capacity(8192);
        for (l, r) in audio.left.chunks(4096).zip(audio.right.chunks(4096)) {
            frame.clear();
            for (a, b) in l.iter().zip(r) {
                frame.push(*a);
                frame.push(*b);
            }
            w.write(&frame).map_err(fail)?;
        }
        w.finish().map_err(fail)?;
        if self.path.is_none() {
            self.temp_recordings.push((path.clone(), Some(kind)));
        }
        self.samples.wait_for([path.as_path()]);
        Ok(path)
    }

    /// Deletes files queued for it, once nothing can bring them back into use.
    pub(super) fn delete_discarded_files(&mut self) {
        for path in std::mem::take(&mut self.discarded_files) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Warp markers for `[0, length)` of a file rendered from `start`, in clip-local ticks from
    /// `base`. At a steady tempo two markers say it all.
    pub(super) fn markers_for(
        &self,
        origin: f64,
        base: f64,
        length: f64,
        file_zero: f64,
    ) -> Vec<WarpMarker> {
        let map = super::build::time_maps(&self.project).tempo;
        let at = |local: f64| WarpMarker {
            seconds: map.tick_to_seconds(origin + local) - map.tick_to_seconds(file_zero),
            tick: local,
        };
        if map.is_constant() {
            return vec![at(base), at(base + length)];
        }
        let step = PPQ as f64 / 4.0;
        let mut out = Vec::new();
        let mut local = base;
        while local < base + length {
            out.push(at(local));
            local += step;
        }
        out.push(at(base + length));
        out
    }
}

// ------------------------------------------------------------- the project each render hears

/// What of a track's clips a render plays.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Keep {
    /// Everything on the timeline and in the slots.
    Whole,
    /// What is in this part of the timeline.
    Span { start: u64, end: u64 },
    /// These clips on the timeline.
    Clips(Vec<rlive_core::ArrClipId>),
    /// This clip in a slot, which plays whether or not it is switched on.
    Slot(usize),
}

/// Applies a [`Keep`] to some tracks of a variant.
pub(super) fn keep_material(v: &mut Project, tracks: &[TrackId], keep: &Keep) {
    for t in &mut v.tracks {
        if !tracks.contains(&t.id) {
            continue;
        }
        match keep {
            Keep::Whole => {}
            Keep::Span { start, end } => {
                let mut lane = t.copy_span(*start, *end);
                for c in &mut lane {
                    c.start += start;
                }
                t.arrangement = lane;
                t.clips.iter_mut().for_each(|c| *c = None);
            }
            Keep::Clips(ids) => {
                t.arrangement.retain(|c| ids.contains(&c.id));
                t.clips.iter_mut().for_each(|c| *c = None);
            }
            Keep::Slot(slot) => {
                t.arrangement.clear();
                for (i, c) in t.clips.iter_mut().enumerate() {
                    if i != *slot {
                        *c = None;
                    } else if let Some(c) = c {
                        c.active = true;
                        c.launch = Default::default();
                    }
                }
            }
        }
    }
}

/// Whether an envelope drives this track's own mixer.
fn is_mixer(target: EnvTarget, id: TrackId) -> bool {
    matches!(target, EnvTarget::Param(ParamRef { owner: ParamOwner::Track(t), .. }) if t == id)
}

/// Copies an automation envelope onto another track's mixer.
pub(super) fn mixer_automation(t: &Track, to: TrackId) -> Vec<rlive_project::Envelope> {
    t.automation
        .iter()
        .filter(|e| is_mixer(e.target, t.id))
        .map(|e| {
            let mut e = e.clone();
            if let EnvTarget::Param(p) = &mut e.target {
                p.owner = ParamOwner::Track(to);
            }
            e
        })
        .collect()
}

fn sends_off(t: &mut Track, returns: &[TrackId]) {
    t.sends = returns
        .iter()
        .map(|to| TrackSend {
            to: *to,
            level_db: SEND_OFF_DB,
            enabled: false,
        })
        .collect();
}

/// Everything every render leaves out: the Main track's processing, solos and live input.
fn base_variant(p: &Project) -> Project {
    let mut v = p.clone();
    if v.has_tempo_leader() {
        // The tempo stays as the Set's leaders have it, even without the tracks they are on.
        v.arrangement.tempo.points = p.tempo_points_in_force().into_owned();
        for c in v.tracks.iter_mut().flat_map(|t| t.arrangement.iter_mut()) {
            if let rlive_project::ClipContent::Audio { warp, .. } = &mut c.clip.content {
                warp.tempo_leader = false;
            }
        }
    }
    v.master.devices.clear();
    v.master.automation.clear();
    v.master.volume_db = 0.0;
    v.arrangement.loop_enabled = false;
    for t in v.tracks.iter_mut().chain(v.returns.iter_mut()) {
        t.solo = false;
        t.arm = false;
        t.routing.monitor = Monitor::Off;
    }
    v
}

/// The tracks that have to stay for those in `keep` to sound right: anything feeding a sidechain.
fn needed_by(v: &Project, keep: &HashSet<TrackId>) -> HashSet<TrackId> {
    let mut needed = keep.clone();
    loop {
        let mut grew = false;
        for t in v.tracks.iter().chain(&v.returns) {
            let feeds = matches!(
                &t.routing.output,
                Route::Track { track, device: Some(_) } if needed.contains(track)
            );
            if feeds {
                grew |= needed.insert(t.id);
            }
            if !needed.contains(&t.id) {
                continue;
            }
            for d in &t.devices {
                d.for_each(&mut |d| {
                    if let Some(sc) = d.sidechain {
                        grew |= needed.insert(sc.track);
                    }
                });
            }
        }
        if !grew {
            return needed;
        }
    }
}

/// Keeps other tracks out of the mix, leaving what they feed sidechains with.
fn silence_others(v: &mut Project, keep: &HashSet<TrackId>) {
    let needed = needed_by(v, keep);
    let returns: Vec<TrackId> = v.returns.iter().map(|t| t.id).collect();
    v.tracks.retain(|t| needed.contains(&t.id));
    for t in &mut v.tracks {
        if keep.contains(&t.id) {
            continue;
        }
        let feeds_device = matches!(
            &t.routing.output,
            Route::Track { track, device: Some(_) } if needed.contains(track)
        );
        if !feeds_device {
            t.routing.output = Route::None;
        }
        sends_off(t, &returns);
    }
}

fn strip_mixer(t: &mut Track, returns: &[TrackId]) {
    let id = t.id;
    t.group = None;
    t.routing.output = Route::Default;
    t.volume_db = 0.0;
    t.pan = 0.0;
    t.pan_mode = PanMode::Stereo;
    t.mute = false;
    t.crossfade = Crossfade::None;
    t.delay = TrackDelay::None;
    sends_off(t, returns);
    t.automation.retain(|e| !is_mixer(e.target, id));
    let strip_clip = |c: &mut Clip| c.envelopes.retain(|e| !is_mixer(e.envelope.target, id));
    t.clips.iter_mut().flatten().for_each(strip_clip);
    t.arrangement
        .iter_mut()
        .for_each(|c| strip_clip(&mut c.clip));
}

/// The project with only these tracks audible, after their devices and before their mixers.
pub(super) fn post_fx_variant(p: &Project, keep: &[TrackId]) -> Project {
    let mut v = base_variant(p);
    let keep: HashSet<TrackId> = keep.iter().copied().collect();
    let returns: Vec<TrackId> = v.returns.iter().map(|t| t.id).collect();
    silence_others(&mut v, &keep);
    for t in &mut v.tracks {
        if keep.contains(&t.id) {
            strip_mixer(t, &returns);
        }
    }
    let needed = needed_by(&v, &keep);
    for r in &mut v.returns {
        r.routing.output = Route::None;
        if !needed.contains(&r.id) {
            r.devices.clear();
            r.automation.clear();
        }
    }
    v
}

/// The project with only these tracks' clips audible, before their devices and mixers: what
/// Consolidate commits.
pub(super) fn pre_fx_variant(p: &Project, keep: &[TrackId]) -> Project {
    let mut v = post_fx_variant(p, keep);
    let keep: HashSet<TrackId> = keep.iter().copied().collect();
    for t in &mut v.tracks {
        if !keep.contains(&t.id) {
            continue;
        }
        t.devices.clear();
        t.automation.clear();
        t.freeze = None;
        t.arrangement.retain(|c| !c.clip.frozen_tail);
        let own = |c: &mut Clip| {
            c.frozen = None;
            c.envelopes
                .retain(|e| !matches!(e.envelope.target, EnvTarget::Param(_)));
        };
        t.clips.iter_mut().flatten().for_each(own);
        t.arrangement.iter_mut().for_each(|c| own(&mut c.clip));
    }
    v
}

/// Whether a track's output finds its way into `group`.
fn reaches(p: &Project, from: &Track, group: TrackId) -> bool {
    let mut at = from;
    for _ in 0..64 {
        let next = match &at.routing.output {
            Route::Default => p.parent_group(at.id),
            Route::Track {
                track,
                device: None,
            } => Some(*track),
            _ => None,
        };
        match next {
            Some(t) if t == group => return true,
            Some(t) => match p.track(t) {
                Some(t) => at = t,
                None => return false,
            },
            None => return false,
        }
    }
    false
}

/// The project as a Group Track reaches the Main output, with what its members send: everything
/// else is out, and so is anything in it that doesn't end up in the group (manual p. 428).
pub(super) fn group_variant(p: &Project, group: TrackId) -> Project {
    let mut v = base_variant(p);
    let members = p.group_contents(group);
    let returns: Vec<TrackId> = v.returns.iter().map(|t| t.id).collect();
    let mut keep: HashSet<TrackId> = members.iter().copied().collect();
    keep.insert(group);
    silence_others(&mut v, &keep);
    let audible: HashSet<TrackId> = members
        .iter()
        .copied()
        .filter(|id| p.track(*id).is_some_and(|t| reaches(p, t, group)))
        .collect();
    for t in &mut v.tracks {
        if t.id == group {
            // Its place inside another group is the replaced track's to keep, not this sound's.
            t.group = None;
            t.mute = false;
        } else if keep.contains(&t.id) && !audible.contains(&t.id) {
            t.routing.output = Route::None;
            sends_off(t, &returns);
        }
    }
    v
}

/// Replaces automation with where it stands at the very start: what a Session clip hears, since
/// the Arrangement is punched out while it plays (manual p. 992).
pub(super) fn snapshot_parameters(v: &mut Project) {
    v.arrangement.tempo.points.clear();
    let returns: Vec<TrackId> = v.returns.iter().map(|t| t.id).collect();
    for t in v.tracks.iter_mut().chain(v.returns.iter_mut()) {
        for e in std::mem::take(&mut t.automation) {
            let EnvTarget::Param(target) = e.target else {
                continue;
            };
            let Some(value) = e
                .points
                .iter()
                .rev()
                .find(|p| p.time <= 0.0)
                .or(e.points.first())
            else {
                continue;
            };
            let value = value.value;
            match target.owner {
                ParamOwner::Device(id) => set_device_value(&mut t.devices, id, target, value),
                ParamOwner::Track(_) => set_mixer_value(t, &returns, target.id, value),
                _ => {}
            }
        }
    }
}

fn set_device_value(devices: &mut [Device], id: DeviceId, target: ParamRef, value: f64) {
    for d in devices {
        d.for_each_mut(&mut |d| {
            if d.id != id {
                return;
            }
            if target.id == ACTIVATOR {
                d.bypass = value < 0.5;
            } else {
                d.store_param(target.id.0, value);
            }
        });
    }
}

pub(super) fn set_mixer_value(
    t: &mut Track,
    returns: &[TrackId],
    id: rlive_core::ParamId,
    value: f64,
) {
    if id == mixer::VOLUME {
        t.volume_db = value as f32;
    } else if id == mixer::PAN {
        t.pan = value as f32;
    } else if id == mixer::MUTE {
        t.mute = value >= 0.5;
    } else if id.0 >= mixer::FIRST_SEND.0 {
        let n = (id.0 - mixer::FIRST_SEND.0) as usize;
        let Some(to) = returns.get(n) else {
            return;
        };
        match t.sends.iter_mut().find(|s| s.to == *to) {
            Some(send) => send.level_db = value as f32,
            None => t.sends.push(TrackSend {
                to: *to,
                level_db: value as f32,
                enabled: true,
            }),
        }
    }
}
