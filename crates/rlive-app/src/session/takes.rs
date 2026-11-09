//! Audio takes and Session slot recording.
//!
//! The audio thread records to the frame and says where things happened in the file
//! ([`EngineEvent::CaptureSegment`](rlive_engine::EngineEvent::CaptureSegment)); this file turns
//! that into clips. A take goes on the timeline when the disk thread has finished its file.

use super::Session;
use crate::recording::disk::{DiskEvent, DiskWriter, Mirror, TakeJob};
use crate::recording::Recording;
use rlive_core::{SampleBuffer, TempoMap, TrackId, PPQ};
use rlive_engine::capture::CaptureMode;
use rlive_engine::shared::NO_SLOT;
use rlive_engine::{capture_pair, EngineCommand, LaunchCmd, RecordCmd};
use rlive_project::{
    ArrangementClip, Clip, ClipContent, ExternalRoute, Monitor, Project, Route, Track, TrackKind,
    Warp, WarpMarker,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

/// How long a take can wait for the disk before audio is dropped, in seconds.
pub(super) const RING_SECONDS: f64 = 10.0;

/// What a take is recording for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum TakeKind {
    Arrangement,
    Slot {
        slot: usize,
        /// Where the clip ends on the timeline, once that's known.
        end: Option<f64>,
    },
}

/// A place in the file where the timeline changed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    frame: u64,
    tick: f64,
    jump: bool,
    ticks_per_frame: f64,
}

pub(super) struct AudioTake {
    id: u64,
    path: PathBuf,
    channels: u8,
    kind: TakeKind,
    segments: Vec<Segment>,
    /// Frames the audio thread took, once it has finished.
    frames: Option<u64>,
    /// The disk thread has closed the file.
    file_done: bool,
    /// The take as it arrives, for playing it before the file is finished.
    mirror: Option<Arc<Mirror>>,
}

/// A MIDI take going into a Session slot.
pub(super) struct MidiSlotTake {
    start: Option<f64>,
    pub(super) recording: Recording,
}

/// Notes held on tracks being overdubbed: their key, when they began in the clip, and how hard.
#[derive(Default)]
pub(super) struct OverdubState {
    open: HashMap<(TrackId, u8), (f64, f32)>,
    /// Which pass of its loop each track's clip was on, to see it come around.
    last: HashMap<TrackId, i64>,
}

/// How a take's file lays out on the timeline.
struct Layout {
    /// Each pass: where on the timeline it starts, and where in the clip's content.
    passes: Vec<(f64, f64, f64)>,
    markers: Vec<WarpMarker>,
    total_ticks: f64,
}

/// Lays a take's segments out as passes and Warp Markers. With automated tempo the file doesn't
/// run at one speed through a segment, so markers follow the map, a beat at a time.
fn layout(take: &AudioTake, frames: u64, sample_rate: f64, tempo: &TempoMap) -> Layout {
    let mut passes = Vec::new();
    let mut markers = Vec::new();
    let mut content = 0.0;
    let mut pass_from = content;
    let mut pass_tick = None;
    for (i, s) in take.segments.iter().enumerate() {
        let f1 = take.segments.get(i + 1).map_or(frames, |n| n.frame);
        if s.jump || pass_tick.is_none() {
            if let Some(tick) = pass_tick {
                passes.push((tick, pass_from, content));
            }
            pass_tick = Some(s.tick);
            pass_from = content;
        }
        markers.push(WarpMarker {
            seconds: s.frame as f64 / sample_rate,
            tick: content,
        });
        let span = f1.saturating_sub(s.frame) as f64;
        if tempo.is_constant() {
            content += span * s.ticks_per_frame;
            continue;
        }
        let begin = tempo.tick_to_seconds(s.tick);
        let end_tick = tempo.seconds_to_tick(begin + span / sample_rate);
        let beat = PPQ as f64;
        let mut at = (s.tick / beat).floor() * beat + beat;
        while at < end_tick - 1e-6 {
            markers.push(WarpMarker {
                seconds: s.frame as f64 / sample_rate + (tempo.tick_to_seconds(at) - begin),
                tick: content + at - s.tick,
            });
            at += beat;
        }
        content += end_tick - s.tick;
    }
    if let Some(tick) = pass_tick {
        passes.push((tick, pass_from, content));
    }
    markers.push(WarpMarker {
        seconds: frames as f64 / sample_rate,
        tick: content,
    });
    markers.dedup_by(|a, b| (a.tick - b.tick).abs() < 1e-6);
    Layout {
        passes,
        markers,
        total_ticks: content,
    }
}

pub(super) fn file_name(track: &str, take: u64, kind: rlive_audio_io::FileType) -> String {
    let safe: String = track
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("{} {stamp}-{take}.{}", safe.trim(), kind.extension())
}

/// The file a take records into: the Set's Samples/Recorded, or the temporary folder until the Set
/// has been saved somewhere.
pub(super) fn recording_folder(project_file: Option<&Path>, temp: Option<&Path>) -> PathBuf {
    match project_file.and_then(Path::parent) {
        Some(dir) => dir.join("Samples").join("Recorded"),
        None => temp.map_or_else(default_temp_folder, Path::to_path_buf),
    }
}

/// Where recordings go until the Set has a place of its own.
pub fn default_temp_folder() -> PathBuf {
    std::env::temp_dir().join("rLive").join("Recorded")
}

impl Session {
    /// How far back of the playhead a take on `t` belongs, in frames. Fully compensated it's the
    /// whole trip from what's heard to what arrives; with Keep Monitoring Latency it's only what
    /// the monitoring path itself doesn't already make up for, so the take sits where it was heard.
    fn record_offset(&self, t: &Track) -> u32 {
        let (input, output) = match t.routing.input {
            Route::External(ExternalRoute::Audio { .. }) | Route::Default => (
                self.engine.shared.input_latency.load(Relaxed),
                self.engine.shared.output_latency.load(Relaxed),
            ),
            // These never leave the machine, so there's nothing to take back.
            _ => (0, 0),
        };
        let plan = self.current_plan_latency();
        let monitored = match t.routing.monitor {
            Monitor::In => true,
            Monitor::Auto => t.arm,
            Monitor::Off => false,
        };
        if monitored && t.keep_monitor_latency {
            let own = self.track_chain_latency(t.id);
            plan.saturating_sub(own)
        } else {
            input + output + plan
        }
    }

    /// Whether this take could record anything: an audio track with an audio input.
    fn take_channels(&self, t: &Track) -> Option<u8> {
        if t.kind != TrackKind::Audio {
            return None;
        }
        match &t.routing.input {
            Route::External(ExternalRoute::Audio { stereo, .. }) => {
                Some(if *stereo { 2 } else { 1 })
            }
            // The default: the first external input, in mono.
            Route::Default => Some(1),
            Route::Resampling | Route::Track { .. } => Some(2),
            _ => None,
        }
    }

    /// Starts an audio take on a track, whose audio goes to `kind`'s clip when finished.
    pub(super) fn open_take(&mut self, track: TrackId, mode: CaptureMode) -> bool {
        let Some(t) = self.project.track(track).cloned() else {
            return false;
        };
        let Some(channels) = self.take_channels(&t) else {
            return false;
        };
        let sample_rate = self.engine.sample_rate;
        let capacity = (sample_rate * self.take_ring_seconds) as usize;
        let offset = self.record_offset(&t);
        let (sink, tail) = capture_pair(channels, capacity, mode, offset);
        let folder = recording_folder(self.path.as_deref(), self.record.temp_folder.as_deref());
        self.next_take += 1;
        let id = self.next_take;
        let path = folder.join(file_name(&t.name, id, self.record.file_type));
        let mirror = (mode == CaptureMode::Slot).then(|| Arc::new(Mirror::default()));
        self.disk
            .get_or_insert_with(DiskWriter::new)
            .start(TakeJob {
                id,
                path: path.clone(),
                tail,
                file_type: self.record.file_type,
                bit_depth: self.record.bit_depth,
                sample_rate: sample_rate as u32,
                mirror: mirror.clone(),
            });
        self.takes.insert(
            track,
            AudioTake {
                id,
                path,
                channels,
                kind: match mode {
                    CaptureMode::Arrangement => TakeKind::Arrangement,
                    CaptureMode::Slot => TakeKind::Slot { slot: 0, end: None },
                },
                segments: Vec::new(),
                frames: None,
                file_done: false,
                mirror,
            },
        );
        self.send(EngineCommand::Record(RecordCmd::InstallCapture {
            track,
            sink: Box::new(sink),
        }));
        true
    }

    fn current_plan_latency(&self) -> u32 {
        super::graph::compile_with(&self.project, &self.timing).output_latency
    }

    fn track_chain_latency(&self, track: TrackId) -> u32 {
        let index = self.project.track_index(track).unwrap_or(0);
        self.timing.own_latency(index)
    }

    /// Takes recording in progress are dropped: the Set they were for has gone.
    pub(super) fn abandon_takes(&mut self) {
        self.takes.clear();
        self.slot_takes.clear();
        self.overdubs = OverdubState::default();
    }

    pub(super) fn capture_segment(
        &mut self,
        track: TrackId,
        frame: u64,
        tick: f64,
        jump: bool,
        ticks_per_frame: f64,
    ) {
        if let Some(take) = self.takes.get_mut(&track) {
            take.segments.push(Segment {
                frame,
                tick,
                jump,
                ticks_per_frame,
            });
        }
    }

    pub(super) fn capture_closed(&mut self, track: TrackId, frames: u64) {
        if let Some(take) = self.takes.get_mut(&track) {
            take.frames = Some(frames);
        }
        self.place_if_ready(track);
    }

    /// What the disk thread has finished.
    pub(super) fn poll_disk(&mut self) {
        let Some(disk) = self.disk.as_ref() else {
            return;
        };
        let events: Vec<DiskEvent> = disk.events.try_iter().collect();
        for event in events {
            match event {
                DiskEvent::Finished { id, dropped, .. } => {
                    if dropped > 0 {
                        self.dropped_frames += dropped;
                        self.messages.push(format!(
                            "Recording could not keep up with the disk: {dropped} frames were \
                             lost and replaced with silence"
                        ));
                    }
                    let track = self
                        .takes
                        .iter()
                        .find(|(_, t)| t.id == id)
                        .map(|(track, _)| *track);
                    if let Some(track) = track {
                        if let Some(take) = self.takes.get_mut(&track) {
                            take.file_done = true;
                        }
                        self.place_if_ready(track);
                    }
                }
                DiskEvent::Failed { reason, id } => {
                    log::error!("recording failed: {reason}");
                    self.messages.push(format!("Recording failed: {reason}"));
                    self.takes.retain(|_, t| t.id != id);
                }
            }
        }
    }

    /// Where a track's take started on the timeline, while one is running on it.
    pub fn take_recording_from(&self, track: TrackId) -> Option<f64> {
        let take = self.takes.get(&track)?;
        (take.frames.is_none() && take.kind == TakeKind::Arrangement)
            .then(|| take.segments.first().map(|s| s.tick))
            .flatten()
    }

    /// Frames lost in the takes running now.
    pub fn take_dropped(&self) -> u64 {
        self.dropped_frames
    }

    /// A take goes on the timeline once the audio thread has closed it and the file is complete.
    fn place_if_ready(&mut self, track: TrackId) {
        let ready = self
            .takes
            .get(&track)
            .is_some_and(|t| t.file_done && t.frames.is_some());
        if !ready {
            return;
        }
        let Some(take) = self.takes.remove(&track) else {
            return;
        };
        match take.kind {
            TakeKind::Arrangement => self.place_arrangement_take(track, take),
            TakeKind::Slot { slot, end } => self.finish_slot_take(track, take, slot, end),
        }
    }

    fn warp_of(&self, markers: Vec<WarpMarker>) -> Warp {
        Warp {
            enabled: true,
            mode: self.warp_defaults.mode,
            markers,
            ..Warp::default()
        }
    }

    fn clip_for(&self, track: TrackId, path: &Path, warp: Warp, length: u64, looped: bool) -> Clip {
        let name = self
            .project
            .track(track)
            .map_or_else(String::new, |t| t.name.clone());
        let mut content = ClipContent::audio(path, 0.0);
        if let ClipContent::Audio { warp: w, .. } = &mut content {
            *w = warp;
        }
        Clip {
            name,
            color: self.new_clip_color(track),
            length,
            looped,
            content,
            launch: self.launch_defaults.settings(),
            ..Clip::default()
        }
    }

    /// One clip for the take. A loop recording keeps every pass in its one sample, showing the
    /// last, and each pass before it is one undo step further back.
    fn place_arrangement_take(&mut self, track: TrackId, take: AudioTake) {
        let frames = take.frames.unwrap_or(0);
        if frames == 0 || take.segments.is_empty() {
            let _ = std::fs::remove_file(&take.path);
            return;
        }
        self.note_new_recording(&take.path);
        let tempo = super::build::time_maps(&self.project).tempo;
        let layout = layout(&take, frames, self.engine.sample_rate, &tempo);
        let total = layout.total_ticks.round().max(1.0) as u64;
        let warp = self.warp_of(layout.markers);
        let mut clip = self.clip_for(track, &take.path, warp, total, false);
        let id = rlive_core::ArrClipId::new();
        for (tick, from, to) in layout.passes {
            // The brace is around this pass, so moving it back auditions the ones before.
            clip.start = from.round() as u64;
            clip.loop_start = clip.start;
            clip.length = (to - from).round().max(1.0) as u64;
            let placed = ArrangementClip {
                id,
                start: tick.round().max(0.0) as u64,
                length: (to - from).round().max(1.0) as u64,
                offset: from.round() as u64,
                clip: clip.clone(),
            };
            if let Some(t) = self.project.track_mut(track) {
                t.remove_arr_clip(id);
                t.place_clip(placed);
                t.settle_fades();
            }
            self.sync_lane(track);
            self.mark_edit("Record");
            self.commit_now();
        }
    }

    fn note_new_recording(&mut self, path: &Path) {
        if self.path.is_none() {
            self.temp_recordings.push((path.to_path_buf(), None));
        }
    }

    /// Moves recordings and processed files out of the temporary folder into the Set's own, now
    /// that it has one. Undo steps that used them follow them to where they went.
    pub(super) fn adopt_temp_recordings(&mut self, project_file: &Path) {
        let waiting = std::mem::take(&mut self.temp_recordings);
        if waiting.is_empty() {
            return;
        }
        let mut moved = HashMap::new();
        for (from, processed) in waiting {
            let Some(name) = from.file_name() else {
                continue;
            };
            let dir = match processed {
                Some(kind) => super::render::processed_folder(Some(project_file), None, kind),
                None => recording_folder(Some(project_file), None),
            };
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.messages.push(format!("{}: {e}", dir.display()));
                continue;
            }
            let to = dir.join(name);
            if let Err(e) =
                std::fs::rename(&from, &to).or_else(|_| std::fs::copy(&from, &to).map(|_| ()))
            {
                self.messages.push(format!("{}: {e}", from.display()));
                continue;
            }
            let _ = std::fs::remove_file(&from);
            self.samples.rename(&from, &to);
            self.project.for_each_file_ref_mut(&mut |file| {
                if file.path == from {
                    file.path = to.clone();
                }
            });
            moved.insert(from, to);
        }
        self.history.move_files(&moved);
    }

    // ------------------------------------------------------------- Session slots

    /// Armed tracks with an empty slot in `scene`.
    pub(super) fn armed_empty_slots(&self, scene: usize) -> Vec<TrackId> {
        self.project
            .tracks
            .iter()
            .filter(|t| t.arm && t.clips.get(scene).is_some_and(|c| c.is_none()))
            .map(|t| t.id)
            .collect()
    }

    /// Records into an empty slot on an armed track, at the next launch point.
    pub fn record_slot(&mut self, track: TrackId, slot: usize) -> bool {
        let Some(t) = self.project.track(track) else {
            return false;
        };
        if !t.arm || t.clips.get(slot).is_none_or(|c| c.is_some()) {
            return false;
        }
        match t.kind {
            TrackKind::Audio => {
                if !self.open_take(track, CaptureMode::Slot) {
                    return false;
                }
                if let Some(take) = self.takes.get_mut(&track) {
                    take.kind = TakeKind::Slot { slot, end: None };
                }
            }
            TrackKind::Midi => {
                self.slot_takes.insert(
                    track,
                    MidiSlotTake {
                        start: None,
                        recording: Recording::new(0),
                    },
                );
            }
            _ => return false,
        }
        self.send(EngineCommand::Launch(LaunchCmd::RecordSlot { track, slot }));
        true
    }

    /// The slot this track is recording into right now.
    pub fn recording_slot(&self, track: TrackId) -> Option<usize> {
        let shared = self.track_shared.get(&track)?;
        let slot = shared.recording_slot.load(Relaxed);
        (slot != NO_SLOT).then_some(slot as usize)
    }

    /// Whether any track is recording into a Session slot, or waiting to.
    pub fn any_slot_recording(&self) -> bool {
        !self.takes_in_slots().is_empty()
    }

    fn takes_in_slots(&self) -> Vec<TrackId> {
        self.project
            .tracks
            .iter()
            .filter(|t| {
                self.track_shared.get(&t.id).is_some_and(|s| {
                    s.recording_slot.load(Relaxed) != NO_SLOT
                        || s.record_queued_slot.load(Relaxed) != NO_SLOT
                })
            })
            .map(|t| t.id)
            .collect()
    }

    /// The Session Record button: end takes straight into loop playback; flip overdub if a
    /// MIDI clip is looping on an armed track; otherwise record the selected scene (or the
    /// next scene that has room) on every armed track.
    ///
    /// With Automation Arm on, the same button records gestures into the Session clips playing,
    /// and the press after that stops doing so and leaves everything playing (manual p. 512).
    pub fn session_record(&mut self, scene: usize) {
        let automation_was_on = self.session_automation_record();
        let closing = self.takes_in_slots();
        if !closing.is_empty() {
            for track in closing {
                self.send(EngineCommand::Launch(LaunchCmd::CloseRecording { track }));
            }
            self.set_session_automation_record(false);
            return;
        }
        if automation_was_on {
            self.set_session_automation_record(false);
            let overdubbing: Vec<TrackId> = self
                .project
                .tracks
                .iter()
                .filter(|t| self.is_overdubbing(t.id))
                .map(|t| t.id)
                .collect();
            for track in overdubbing {
                self.set_overdub(track, false);
            }
            return;
        }
        let automation = self.automation_arm() && self.any_clip_takes_automation();
        let overdub: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.arm && t.kind == TrackKind::Midi && self.plays_looping_midi(t.id))
            .map(|t| t.id)
            .collect();
        if !overdub.is_empty() {
            let on = !overdub.iter().any(|t| self.is_overdubbing(*t));
            for track in overdub {
                self.set_overdub(track, on);
            }
            if on && automation {
                self.set_session_automation_record(true);
            }
            return;
        }
        if automation {
            self.set_session_automation_record(true);
            return;
        }
        let armed: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.arm)
            .map(|t| t.id)
            .collect();
        if armed.is_empty() {
            return;
        }
        let scene = self.scene_with_room(scene, &armed);
        for track in armed {
            self.record_slot(track, scene);
        }
    }

    /// Stop what's playing on armed tracks and clear a scene to record into (the New button).
    /// Returns the scene, which is now selected as the place for the next take.
    pub fn new_take(&mut self, from_scene: usize) -> Option<usize> {
        let armed: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.arm)
            .map(|t| t.id)
            .collect();
        if armed.is_empty() {
            return None;
        }
        for track in &armed {
            self.stop_track(*track);
        }
        Some(self.scene_with_room(from_scene, &armed))
    }

    /// The first scene from `from` on where every track in `tracks` has an empty slot, making
    /// one at the end if there is none.
    fn scene_with_room(&mut self, from: usize, tracks: &[TrackId]) -> usize {
        let free = |project: &Project, scene: usize| {
            tracks.iter().all(|t| {
                project
                    .track(*t)
                    .and_then(|t| t.clips.get(scene))
                    .is_some_and(Option::is_none)
            })
        };
        let n = self.project.scenes.len();
        if let Some(scene) = (from..n).find(|s| free(&self.project, *s)) {
            return scene;
        }
        self.add_scene();
        self.project.scenes.len() - 1
    }

    /// Closes a track's slot recording and plays the new clip from the next launch point.
    pub fn close_slot_recording(&mut self, track: TrackId) {
        self.send(EngineCommand::Launch(LaunchCmd::CloseRecording { track }));
    }

    pub(super) fn slot_record_started(&mut self, track: TrackId, tick: f64) {
        if let Some(take) = self.slot_takes.get_mut(&track) {
            take.start = Some(tick);
            take.recording = Recording::new(tick as u64);
        }
    }

    /// A slot's close has been scheduled. Whatever can be played by then is handed to the engine
    /// now, so the first pass starts on time rather than when the file is done.
    pub(super) fn slot_closing(
        &mut self,
        track: TrackId,
        slot: usize,
        at: f64,
        launch: bool,
        frames: Option<u64>,
    ) {
        if let Some(take) = self.takes.get_mut(&track) {
            take.kind = TakeKind::Slot {
                slot,
                end: Some(at),
            };
            if launch {
                if let Some(frames) = frames {
                    self.play_slot_take_early(track, slot, at, frames);
                }
            }
        }
        if self.slot_takes.contains_key(&track) && launch {
            self.write_midi_slot(track, slot, at);
        }
    }

    fn play_slot_take_early(&mut self, track: TrackId, slot: usize, at: f64, frames: u64) {
        let Some(take) = self.takes.get(&track) else {
            return;
        };
        let Some(mirror) = take.mirror.as_ref() else {
            return;
        };
        let (mut left, mut right) = {
            let c = mirror.channels.lock().unwrap();
            let right = if take.channels == 2 {
                c[1].clone()
            } else {
                c[0].clone()
            };
            (c[0].clone(), right)
        };
        left.resize(frames as usize, 0.0);
        right.resize(frames as usize, 0.0);
        let segments = take.segments.clone();
        let path = take.path.clone();
        let sample_rate = self.engine.sample_rate;
        self.samples.install(
            &path,
            Arc::new(SampleBuffer {
                name: "Recording".into(),
                left,
                right,
                sample_rate,
            }),
        );
        let start = segments.first().map_or(at, |s| s.tick);
        let tempo = super::build::time_maps(&self.project).tempo;
        let layout = layout_of(&segments, frames, sample_rate, &tempo);
        let warp = self.warp_of(layout.markers);
        let length = (at - start).round().max(1.0) as u64;
        let clip = self.clip_for(track, &path, warp, length, true);
        self.put_in_slot(track, slot, clip);
    }

    fn put_in_slot(&mut self, track: TrackId, slot: usize, clip: Clip) {
        if let Some(t) = self.project.track_mut(track) {
            if slot < t.clips.len() {
                t.clips[slot] = Some(clip);
            }
        }
        self.send_clip(track, slot);
    }

    fn finish_slot_take(&mut self, track: TrackId, take: AudioTake, slot: usize, end: Option<f64>) {
        let frames = take.frames.unwrap_or(0);
        if frames == 0 || take.segments.is_empty() {
            let _ = std::fs::remove_file(&take.path);
            self.put_in_slot_nothing(track, slot);
            return;
        }
        self.note_new_recording(&take.path);
        let sample_rate = self.engine.sample_rate;
        if let Some(mirror) = &take.mirror {
            let c = mirror.channels.lock().unwrap();
            self.samples.install(
                &take.path,
                Arc::new(SampleBuffer {
                    name: "Recording".into(),
                    left: c[0].clone(),
                    right: if take.channels == 2 {
                        c[1].clone()
                    } else {
                        c[0].clone()
                    },
                    sample_rate,
                }),
            );
        }
        let start = take.segments.first().map_or(0.0, |s| s.tick);
        let tempo = super::build::time_maps(&self.project).tempo;
        let layout = layout(&take, frames, sample_rate, &tempo);
        let warp = self.warp_of(layout.markers);
        let length = end
            .map_or(layout.total_ticks, |e| e - start)
            .round()
            .max(1.0) as u64;
        let clip = self.clip_for(track, &take.path, warp, length, true);
        self.put_in_slot(track, slot, clip);
        self.mark_edit("Record");
        self.commit_now();
    }

    fn put_in_slot_nothing(&mut self, track: TrackId, slot: usize) {
        if self.clip(track, slot).is_none() {
            self.send(EngineCommand::Clip(rlive_engine::ClipCmd::SetClip {
                track,
                slot,
                clip: None,
            }));
        }
    }

    /// Puts what a MIDI take has so far in its slot, as a clip reaching `end`.
    fn write_midi_slot(&mut self, track: TrackId, slot: usize, end: f64) {
        let sig = self.project.time_signature;
        let q = self.project.record_quantize;
        let color = self.new_clip_color(track);
        let name = self
            .project
            .track(track)
            .map_or_else(String::new, |t| t.name.clone());
        let Some(take) = self.slot_takes.get(&track) else {
            return;
        };
        let start = take.start.unwrap_or(end);
        let length = (end - start).round().max(1.0) as u64;
        let mut notes = Vec::new();
        for p in take.recording.placements(sig) {
            if p.track == track {
                notes.extend(p.clip.notes().iter().cloned().map(|mut n| {
                    n.start = (n.start + p.start).saturating_sub(start as u64);
                    n
                }));
            }
        }
        for n in &mut notes {
            n.start = q.apply(n.start);
        }
        notes.retain(|n| n.start < length);
        let clip = Clip {
            name,
            color,
            length,
            looped: true,
            content: ClipContent::midi(notes),
            launch: self.launch_defaults.settings(),
            ..Clip::default()
        };
        self.put_in_slot(track, slot, clip);
    }

    pub(super) fn slot_record_ended(&mut self, track: TrackId, slot: usize, tick: f64) {
        let Some(mut take) = self.slot_takes.remove(&track) else {
            return;
        };
        take.recording.finish(tick);
        let has_notes = take.recording.has_notes(track);
        let started = take.start.is_some();
        self.slot_takes.insert(track, take);
        if started && has_notes {
            self.write_midi_slot(track, slot, tick);
            self.mark_edit("Record");
            self.commit_now();
        } else if self
            .clip(track, slot)
            .is_some_and(|c| c.notes().is_empty() && c.is_midi())
        {
            // A clip written ahead for the launch, and nothing played into it after all.
            self.write_midi_slot(track, slot, tick);
        }
        self.slot_takes.remove(&track);
    }

    // ------------------------------------------------------------- pattern overdub

    /// Whether this track is playing a looping MIDI clip.
    fn plays_looping_midi(&self, track: TrackId) -> bool {
        let Some(shared) = self.track_shared.get(&track) else {
            return false;
        };
        let slot = shared.playing_slot.load(Relaxed);
        slot >= 0
            && self
                .clip(track, slot as usize)
                .is_some_and(|c| c.is_midi() && c.looped)
    }

    pub fn is_overdubbing(&self, track: TrackId) -> bool {
        self.track_shared
            .get(&track)
            .is_some_and(|s| s.overdubbing.load(Relaxed))
    }

    /// Whether any track's playing clip is taking notes, so edits stay one undo step per pass.
    pub fn any_overdubbing(&self) -> bool {
        self.project
            .tracks
            .iter()
            .any(|t| self.is_overdubbing(t.id))
    }

    /// Whether notes played on `track` go into its playing clip. Each change is its own undo step.
    pub fn set_overdub(&mut self, track: TrackId, on: bool) {
        self.send(EngineCommand::Record(RecordCmd::SetOverdub { track, on }));
        self.overdubs.last.remove(&track);
        if !on {
            self.commit_now();
        }
    }

    /// Double-click with Alt: a new clip, armed, launched and listening.
    pub fn create_arm_and_launch(&mut self, track: TrackId, slot: usize) {
        if self.clip(track, slot).is_some() {
            return;
        }
        let sig = self.project.time_signature;
        let color = self.new_clip_color(track);
        let clip = self.new_midi_clip(color, sig);
        self.set_clip(track, slot, Some(clip));
        self.set_arm(track, true, false);
        self.launch_clip(track, slot);
        self.set_overdub(track, true);
    }

    pub(super) fn overdub_midi(
        &mut self,
        track: TrackId,
        slot: usize,
        position: f64,
        tick: f64,
        message: rlive_core::MidiMessage,
    ) {
        use rlive_core::MidiMessage as M;
        let len = self.clip(track, slot).map_or(1, |c| c.length.max(1)) as f64;
        // Coming around again ends a take, so undo takes off one pass at a time.
        let pass = ((tick - position) / len).round() as i64;
        if self
            .overdubs
            .last
            .insert(track, pass)
            .is_some_and(|last| last != pass)
        {
            self.commit_now();
        }
        match message {
            M::NoteOn { key, velocity, .. } if velocity > 0.0 => {
                self.overdubs
                    .open
                    .insert((track, key), (position, velocity));
            }
            M::NoteOn { key, .. } | M::NoteOff { key, .. } => {
                let q = self.project.record_quantize;
                let Some((start, velocity)) = self.overdubs.open.remove(&(track, key)) else {
                    return;
                };
                let Some(clip) = self.clip_mut(track, slot) else {
                    return;
                };
                let len = clip.length.max(1);
                let start_tick = q.apply(start.round() as u64);
                let length = {
                    let d = position - start;
                    if d >= 0.0 {
                        d
                    } else {
                        d + len as f64
                    }
                }
                .round()
                .max(10.0) as u64;
                if let Some(notes) = clip.notes_mut() {
                    notes.push(rlive_project::NoteData::new(
                        start_tick % len,
                        length,
                        key,
                        velocity,
                    ));
                }
                self.sync_clip(track, slot);
                self.mark_edit("Record");
            }
            _ => {}
        }
    }
}

fn layout_of(segments: &[Segment], frames: u64, sample_rate: f64, tempo: &TempoMap) -> Layout {
    let take = AudioTake {
        id: 0,
        path: PathBuf::new(),
        channels: 1,
        kind: TakeKind::Arrangement,
        segments: segments.to_vec(),
        frames: Some(frames),
        file_done: true,
        mirror: None,
    };
    layout(&take, frames, sample_rate, tempo)
}
