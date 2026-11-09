//! Freeze and Flatten (manual pp. 985-987, 992).
//!
//! Freezing renders every clip of a track to audio, after its devices and before its mixer. The
//! devices stay in place but out of the signal path, so Unfreeze can bring everything back as it
//! was. Each clip carries where its frozen sound is, so moving or trimming a frozen clip works
//! with what it already holds.

use super::bounce::{audio_clip, slot_shape};
use super::render::{
    from_tick, keep_material, launch_clip, post_fx_variant, snapshot_parameters, Keep, Processed,
    Render, Rendered,
};
use super::{ChainRef, ClipRef, Session};
use rlive_audio_io::{BitDepth, FileType};
use rlive_core::{ChainId, DeviceId, TrackId};
use rlive_engine::GraphOp;
use rlive_project::{
    ArrangementClip, Clip, ClipContent, Device, DeviceKind, FileRef, FrozenAudio, Track,
    TrackFreeze, TrackKind, WarpMarker, WarpMode,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Frozen files are 32-bit so they're no worse than what was heard (manual p. 992).
pub(super) const FORMAT: (FileType, BitDepth) = (FileType::Wav, BitDepth::Bits32);
/// A frozen Session clip is always this many cycles long (manual p. 992).
const SESSION_CYCLES: u64 = 2;
/// Below this the gap between clips holds no tail.
const TAIL_SILENCE: f32 = 1e-5;

/// Whether a device needs real time to work: anything talking to hardware (manual p. 986).
fn needs_realtime(devices: &[Device]) -> bool {
    let mut any = false;
    for d in devices {
        d.for_each(&mut |d| {
            if let DeviceKind::Builtin { type_id, .. } = &d.kind {
                any |= matches!(
                    type_id.as_str(),
                    "rlive.external_audio_effect" | "rlive.external_instrument"
                );
            }
        });
    }
    any
}

/// Where one render's audio goes.
enum Target {
    Lane { start: u64 },
    Slot(usize),
}

impl Session {
    /// Whether a device or chain sits on a frozen track, where its settings can't change.
    pub(super) fn devices_locked(&self, chain: ChainRef) -> bool {
        match chain.owner() {
            ChainRef::Track(id) => !self.restoring && self.is_frozen(id),
            _ => false,
        }
    }

    pub fn is_frozen(&self, track: TrackId) -> bool {
        self.project.track(track).is_some_and(Track::is_frozen)
    }

    /// Whether a track could be frozen: only tracks that hold clips can (manual p. 986).
    pub fn can_freeze(&self, track: TrackId) -> bool {
        self.project.track(track).is_some_and(|t| {
            matches!(t.kind, TrackKind::Midi | TrackKind::Audio)
                && !t.is_frozen()
                && (!t.arrangement.is_empty() || t.clips.iter().any(Option::is_some))
        })
    }

    /// Freezes a track. Starts a job, which runs in real time if there is hardware in the path.
    pub fn freeze_track(&mut self, id: TrackId) -> Result<(), String> {
        if !self.can_freeze(id) || self.is_recording() {
            return Err("This track can't be frozen".into());
        }
        self.prepare_render();
        let (mut renders, mut targets) = (Vec::new(), Vec::new());
        let t = self.project.track(id).expect("checked above");
        let realtime = needs_realtime(&t.devices);
        if let (Some(a), Some(b)) = (t.arrangement.first(), t.arrangement.last()) {
            let start = a.start;
            renders.push(Render {
                project: post_fx_variant(&self.project, &[id]),
                setup: from_tick(start),
                start,
                ticks: b.end() - start,
                tail: true,
            });
            targets.push(Target::Lane { start });
        }
        for (slot, clip) in t.clips.iter().enumerate() {
            let Some(clip) = clip else {
                continue;
            };
            let mut v = post_fx_variant(&self.project, &[id]);
            keep_material(&mut v, &[id], &Keep::Slot(slot));
            snapshot_parameters(&mut v);
            renders.push(Render {
                project: v,
                setup: launch_clip(id, slot),
                start: 0,
                ticks: frozen_shape(clip).1,
                tail: false,
            });
            targets.push(Target::Slot(slot));
        }
        self.start_render_job(
            "Freeze Track",
            renders,
            realtime,
            Box::new(move |s, audio| s.finish_freeze(id, realtime, targets, audio)),
        )
    }

    fn finish_freeze(
        &mut self,
        id: TrackId,
        realtime: bool,
        targets: Vec<Target>,
        audio: Vec<Rendered>,
    ) -> Result<(), String> {
        let name = self
            .project
            .track(id)
            .map_or_else(String::new, |t| t.name.clone());
        let mut lane: Option<(u64, PathBuf, Rendered)> = None;
        let mut slots = Vec::new();
        for (target, rendered) in targets.into_iter().zip(audio) {
            let stem = match target {
                Target::Lane { .. } => format!("{name} Freeze"),
                Target::Slot(s) => format!("{name} Freeze {}", s + 1),
            };
            let path = self.write_processed(Processed::Freeze, &stem, &rendered, FORMAT)?;
            match target {
                Target::Lane { start } => lane = Some((start, path, rendered)),
                Target::Slot(s) => slots.push((s, path)),
            }
        }
        let slot_frozen: Vec<(usize, FrozenAudio)> = slots
            .into_iter()
            .filter_map(|(slot, path)| {
                let clip = self.clip(id, slot)?;
                let (lead, ticks) = frozen_shape(clip);
                let looped = clip.looped;
                Some((
                    slot,
                    FrozenAudio {
                        file: FileRef::new(&path),
                        markers: self.steady_markers(ticks),
                        length: ticks,
                        looped,
                        loop_start: lead,
                    },
                ))
            })
            .collect();
        let frozen_lane =
            lane.map(|(start, path, rendered)| self.frozen_lane(id, start, &path, &rendered));
        let Some(t) = self.project.track_mut(id) else {
            return Ok(());
        };
        t.freeze = Some(TrackFreeze { realtime });
        t.arm = false;
        for (slot, frozen) in slot_frozen {
            if let Some(c) = t.clips.get_mut(slot).and_then(Option::as_mut) {
                c.frozen = Some(Box::new(frozen));
            }
        }
        if let Some(clips) = frozen_lane {
            t.arrangement = clips;
        }
        self.mark_edit("Freeze Track");
        self.frozen_changed(id, true);
        Ok(())
    }

    /// The track's timeline with each clip pointing into the frozen file, and tails where there
    /// is sound past a clip's end.
    fn frozen_lane(
        &self,
        id: TrackId,
        start: u64,
        path: &std::path::Path,
        audio: &Rendered,
    ) -> Vec<ArrangementClip> {
        let map = super::build::time_maps(&self.project).tempo;
        let rate = self.engine.sample_rate;
        let t = self.project.track(id).expect("the track being frozen");
        let file_zero = start as f64;
        let frame_of = |tick: u64| {
            ((map.tick_to_seconds(tick as f64) - map.tick_to_seconds(file_zero)) * rate).round()
                as usize
        };
        let end_of_file = start + self.ticks_of_frames(start, audio.frames());
        let mut out = Vec::with_capacity(t.arrangement.len() * 2);
        for (i, c) in t.arrangement.iter().enumerate() {
            let mut clip = c.clone();
            let local_zero = c.start as f64 - c.offset as f64;
            clip.clip.frozen = Some(Box::new(FrozenAudio {
                file: FileRef::new(path),
                markers: self.markers_for(local_zero, c.offset as f64, c.length as f64, file_zero),
                length: c.offset + c.length,
                looped: false,
                loop_start: 0,
            }));
            out.push(clip);
            let gap_end = t
                .arrangement
                .get(i + 1)
                .map_or(end_of_file, |next| next.start);
            let tail = last_audible(
                audio,
                frame_of(c.end()),
                frame_of(gap_end).min(audio.frames()),
            );
            let Some(tail_end) = tail else {
                continue;
            };
            let tail_ticks = self
                .ticks_of_frames(c.end(), tail_end.saturating_sub(frame_of(c.end())) + 1)
                .min(gap_end.saturating_sub(c.end()));
            if tail_ticks == 0 {
                continue;
            }
            let local = c.end() as f64;
            let markers = self.markers_for(local, 0.0, tail_ticks as f64, file_zero);
            let mut tail_clip = audio_clip(
                path,
                markers.clone(),
                "Frozen Tail".into(),
                c.clip.color,
                tail_ticks,
                false,
                WarpMode::Beats,
            );
            tail_clip.frozen_tail = true;
            tail_clip.frozen = Some(Box::new(FrozenAudio {
                file: FileRef::new(path),
                markers,
                length: tail_ticks,
                looped: false,
                loop_start: 0,
            }));
            out.push(ArrangementClip {
                length: tail_ticks,
                ..ArrangementClip::new(tail_clip, c.end())
            });
        }
        out
    }

    /// Tells the engine a track changed state, its clips, lane and compensation with it.
    fn frozen_changed(&mut self, id: TrackId, frozen: bool) {
        let Some(t) = self.project.track(id) else {
            return;
        };
        let slots = t.clips.len();
        self.send_ops(vec![GraphOp::SetFrozen { track: id, frozen }]);
        for slot in 0..slots {
            self.send_clip(id, slot);
        }
        self.sync_lane(id);
        self.send_track_edits(Vec::new());
    }

    /// Unfreeze Track: the devices and clips come back exactly as they were.
    pub fn unfreeze_track(&mut self, id: TrackId) {
        let Some(t) = self.project.track_mut(id) else {
            return;
        };
        let Some(freeze) = t.freeze.take() else {
            return;
        };
        let mut files = Vec::new();
        let mut release = |c: &mut Clip| {
            if let Some(f) = c.frozen.take() {
                files.push(f.file.path.clone());
            }
        };
        t.clips.iter_mut().flatten().for_each(&mut release);
        t.arrangement.iter_mut().for_each(|c| release(&mut c.clip));
        t.arrangement.retain(|c| !c.clip.frozen_tail);
        if freeze.realtime {
            // Freeze files made through hardware can't be remade, so Unfreeze throws them away
            // (p. 987). Not before the next save, so an Undo of this still finds them.
            self.discarded_files.extend(files);
        }
        self.mark_edit("Unfreeze Track");
        self.frozen_changed(id, false);
    }

    /// Flatten: the frozen audio becomes the track's clips and the devices go.
    pub fn flatten_track(&mut self, id: TrackId) -> Result<(), String> {
        if !self.is_frozen(id) {
            return Err("Only a frozen track can be flattened".into());
        }
        self.mark_edit("Flatten Track");
        let t = self.project.track(id).expect("checked above");
        let slots: Vec<(usize, Clip)> = t
            .clips
            .iter()
            .enumerate()
            .filter_map(|(i, c)| Some((i, flatten_clip(c.as_ref()?)?)))
            .collect();
        let lane: Vec<ArrangementClip> = t
            .arrangement
            .iter()
            .filter_map(|c| {
                Some(ArrangementClip {
                    clip: flatten_clip(&c.clip)?,
                    ..c.clone()
                })
            })
            .collect();
        self.become_audio_track(id);
        let Some(t) = self.project.track_mut(id) else {
            return Ok(());
        };
        for (i, c) in slots {
            t.clips[i] = Some(c);
        }
        t.arrangement = lane;
        t.settle_fades();
        let count = t.clips.len();
        for slot in 0..count {
            self.send_clip(id, slot);
        }
        self.sync_lane(id);
        self.send_track_edits(Vec::new());
        Ok(())
    }

    /// A new frozen track holding a frozen clip, or just the part of it in `span` (manual p. 987).
    pub fn frozen_clip_to_new_track(
        &mut self,
        clip: ClipRef,
        span: Option<(u64, u64)>,
    ) -> Result<TrackId, String> {
        let source = clip.track();
        if !self.is_frozen(source) {
            return Err("Only a frozen clip can go to a new frozen track".into());
        }
        self.capture_plugin_states();
        let t = self.project.track(source).expect("checked above");
        let mut new = Track::new(&t.name, t.kind, t.color, self.project.scenes.len());
        new.devices = fresh_devices(&t.devices);
        new.group = t.group;
        new.volume_db = t.volume_db;
        new.pan = t.pan;
        new.pan_mode = t.pan_mode;
        new.routing = t.routing.clone();
        new.sends = t.sends.clone();
        new.freeze = t.freeze;
        match clip {
            ClipRef::Slot { slot, .. } => {
                let c = t.clips.get(slot).cloned().flatten();
                let Some(entry) = new.clips.get_mut(slot) else {
                    return Err("That slot is gone".into());
                };
                *entry = c;
            }
            ClipRef::Arr { id, .. } => {
                let Some(c) = t.arr_clip(id) else {
                    return Err("That clip is gone".into());
                };
                let (start, end) = span.unwrap_or((c.start, c.end()));
                new.arrangement = t
                    .copy_span(start, end)
                    .into_iter()
                    .map(|mut c| {
                        c.start += start;
                        c.id = rlive_core::ArrClipId::new();
                        c
                    })
                    .collect();
            }
        }
        let id = new.id;
        let after = self.project.track_index(source).unwrap_or(0);
        self.install_track(new, after + 1);
        self.mark_edit("New Frozen Track");
        Ok(id)
    }
}

/// A frozen clip as an ordinary audio clip of its frozen file.
pub(super) fn flatten_clip(c: &Clip) -> Option<Clip> {
    let frozen = c.frozen.as_deref()?;
    let markers: Vec<WarpMarker> = frozen.markers.clone();
    // Frozen clips play in Beats mode (manual p. 992), so the flattened ones stay in it.
    let mut flat = audio_clip(
        &frozen.file.path,
        markers,
        c.name.clone(),
        c.color,
        frozen.length,
        frozen.looped,
        WarpMode::Beats,
    );
    flat.active = c.active;
    flat.launch = c.launch.clone();
    if frozen.looped {
        flat.loop_start = frozen.loop_start;
        flat.length = frozen.length.saturating_sub(frozen.loop_start).max(1);
    }
    if let (ClipContent::Audio { file, .. }, f) = (&mut flat.content, frozen) {
        *file = f.file.clone();
    }
    Some(flat)
}

/// What a frozen Session clip holds: how long until its loop starts, and the whole length.
fn frozen_shape(clip: &Clip) -> (u64, u64) {
    let (lead, cycle) = slot_shape(clip);
    (lead, lead + cycle * SESSION_CYCLES)
}

/// The last frame of `[from, to)` that isn't silent.
fn last_audible(audio: &Rendered, from: usize, to: usize) -> Option<usize> {
    (from..to.min(audio.frames()))
        .rev()
        .find(|&i| audio.left[i].abs() > TAIL_SILENCE || audio.right[i].abs() > TAIL_SILENCE)
}

/// Copies of devices with ids of their own, what's inside them following along.
fn fresh_devices(devices: &[Device]) -> Vec<Device> {
    let mut chains = HashMap::new();
    let mut ids = HashMap::new();
    let mut copy = devices.to_vec();
    refresh(&mut copy, &mut chains, &mut ids);
    copy
}

fn refresh(
    devices: &mut [Device],
    chains: &mut HashMap<ChainId, ChainId>,
    ids: &mut HashMap<DeviceId, DeviceId>,
) {
    for d in devices {
        let old = d.id;
        d.id = DeviceId::new();
        ids.insert(old, d.id);
        for c in d.chains_mut() {
            let was = c.id;
            c.id = ChainId::new();
            chains.insert(was, c.id);
            refresh(&mut c.devices, chains, ids);
        }
    }
}
