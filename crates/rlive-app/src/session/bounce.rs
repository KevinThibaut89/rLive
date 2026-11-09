//! Bounce to Audio (manual pp. 425-430): committing tracks, clips and time selections to audio.
//!
//! A track is bounced after its devices and before its mixer, whose settings the result inherits.
//! A Group Track is bounced as it reaches the Main output, with what its members send but none of
//! the Main track's own processing.

use super::render::{
    from_tick, group_variant, keep_material, launch_clip, launch_scene, mixer_automation,
    post_fx_variant, snapshot_parameters, Keep, Processed, Render, Rendered,
};
use super::{ChainRef, Session};
use rlive_audio_io::{BitDepth, FileType};
use rlive_core::{ArrClipId, TrackId};
use rlive_devices::DeviceRole;
use rlive_engine::GraphOp;
use rlive_project::{
    ArrangementClip, Clip, ClipContent, LaunchSettings, Monitor, Project, Route, Track, TrackKind,
    Warp, WarpMarker, WarpMode,
};

/// Bounced files keep everything the render made.
const FORMAT: (FileType, BitDepth) = (FileType::Wav, BitDepth::Bits32);

/// The material a bounce or Paste Bounced Audio commits.
#[derive(Debug, Clone, PartialEq)]
pub enum BounceSource {
    /// Session clips. A Group Track here is its whole slot.
    Slots(Vec<(TrackId, usize)>),
    /// A time selection on these tracks.
    Span {
        tracks: Vec<TrackId>,
        start: u64,
        end: u64,
    },
    /// Clips on the timeline.
    Clips(Vec<(TrackId, ArrClipId)>),
}

/// Where Paste Bounced Audio puts its clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteTarget {
    Main {
        track: TrackId,
        at: u64,
    },
    TakeLane {
        track: TrackId,
        lane: usize,
        at: u64,
    },
}

/// How a bounced clip or track is named (manual p. 427).
pub(super) fn bounce_name(name: &str) -> String {
    if name.is_empty() {
        "(Bounce)".into()
    } else if name.ends_with("(Bounce)") {
        name.into()
    } else {
        format!("{name} (Bounce)")
    }
}

/// How long a Session clip plays before it starts repeating, in ticks: what the start marker
/// plays on the way into the loop, then the loop. An unlooped clip has only the first.
pub(super) fn slot_shape(c: &Clip) -> (u64, u64) {
    let region = c.region();
    if region.looped {
        (region.lead().max(0.0) as u64, c.length.max(1))
    } else {
        (0, (region.play_length() as u64).max(1))
    }
}

/// One render and where its audio goes.
#[derive(Debug, Clone)]
enum Goes {
    Lane {
        start: u64,
        ticks: u64,
        name: String,
        color: [u8; 3],
    },
    Slot {
        slot: usize,
        ticks: u64,
        /// How much of `ticks` plays once, before the loop.
        lead: u64,
        looped: bool,
        name: String,
        color: [u8; 3],
        active: bool,
        launch: LaunchSettings,
    },
}

/// What a source track commits.
struct Plan {
    source: TrackId,
    goes: Vec<Goes>,
}

enum Made {
    Lane(Box<ArrangementClip>),
    Slot(usize, Box<Clip>),
}

impl Session {
    /// A track or group, and everything that plays through it.
    fn block_of(&self, id: TrackId) -> Vec<TrackId> {
        self.project.track_block(id)
    }

    fn variant_of(&self, id: TrackId) -> Project {
        match self.project.track(id) {
            Some(t) if t.is_group() => group_variant(&self.project, id),
            _ => post_fx_variant(&self.project, &[id]),
        }
    }

    /// The timeline of `id` and all that's in it, from its first clip to the end of its last.
    fn lane_extent(&self, id: TrackId) -> Option<(u64, u64)> {
        let lanes = self
            .block_of(id)
            .into_iter()
            .filter_map(|t| self.project.track(t));
        let (mut first, mut last) = (u64::MAX, 0);
        for t in lanes {
            if let (Some(a), Some(b)) = (t.arrangement.first(), t.arrangement.last()) {
                first = first.min(a.start);
                last = last.max(b.end());
            }
        }
        (first < last).then_some((first, last))
    }

    /// The render of one slot of a track or group, if it holds anything.
    fn plan_slot(&self, id: TrackId, slot: usize) -> Option<(Render, Goes)> {
        let t = self.project.track(id)?;
        let block = self.block_of(id);
        let clips: Vec<&Clip> = block
            .iter()
            .filter_map(|m| self.project.track(*m)?.clips.get(slot)?.as_ref())
            .collect();
        let first = *clips.first()?;
        let shapes: Vec<(u64, u64)> = clips.iter().map(|c| slot_shape(c)).collect();
        // Only a clip on its own can keep a loop inside the render; a group's slot is its longest.
        let (lead, ticks) = if t.is_group() {
            (0, shapes.iter().map(|(l, n)| l + n).max()?)
        } else {
            let (lead, length) = shapes[0];
            (lead, lead + length)
        };
        let mut v = self.variant_of(id);
        keep_material(&mut v, &block, &Keep::Slot(slot));
        snapshot_parameters(&mut v);
        if let Some(scene) = v.scenes.get_mut(slot) {
            // Launching a scene sets the tempo; the render stays at the Set's.
            scene.tempo = None;
            scene.signature = None;
        }
        let (setup, name, color) = if t.is_group() {
            (launch_scene(slot), t.name.clone(), t.color)
        } else {
            (launch_clip(id, slot), first.name.clone(), first.color)
        };
        Some((
            Render {
                project: v,
                setup,
                start: 0,
                ticks,
                tail: false,
            },
            Goes::Slot {
                slot,
                ticks,
                lead,
                looped: first.looped,
                name,
                color,
                active: clips.iter().any(|c| c.active),
                launch: first.launch.clone(),
            },
        ))
    }

    /// The render of what `keep` picks out of the timeline of a track or group.
    fn plan_lane(&self, id: TrackId, keep: Keep, tail: bool) -> Option<(Render, Goes)> {
        let t = self.project.track(id)?;
        let block = self.block_of(id);
        let mut v = self.variant_of(id);
        keep_material(&mut v, &block, &keep);
        let chosen: Vec<&ArrangementClip> = block
            .iter()
            .filter_map(|m| v.track(*m))
            .flat_map(|t| t.arrangement.iter())
            .collect();
        let (start, end) = match keep {
            Keep::Span { start, end } if !chosen.is_empty() => (start, end),
            Keep::Span { .. } => return None,
            _ => (
                chosen.iter().map(|c| c.start).min()?,
                chosen.iter().map(|c| c.end()).max()?,
            ),
        };
        let (name, color) = if t.is_group() {
            (t.name.clone(), t.color)
        } else {
            let first = chosen.iter().min_by_key(|c| c.start)?;
            (first.clip.name.clone(), first.clip.color)
        };
        Some((
            Render {
                project: v,
                setup: from_tick(start),
                start,
                ticks: end - start,
                tail,
            },
            Goes::Lane {
                start,
                ticks: end - start,
                name,
                color,
            },
        ))
    }

    /// Every clip of a track or group, for a bounce that replaces it.
    fn plan_whole(&self, id: TrackId) -> (Vec<Render>, Vec<Goes>) {
        let mut out = (Vec::new(), Vec::new());
        if self.lane_extent(id).is_some() {
            if let Some((r, g)) = self.plan_lane(id, Keep::Whole, true) {
                out.0.push(r);
                out.1.push(g);
            }
        }
        for slot in 0..self.project.scenes.len() {
            if let Some((r, g)) = self.plan_slot(id, slot) {
                out.0.push(r);
                out.1.push(g);
            }
        }
        out
    }

    /// Writes one piece of audio and builds the clip for it.
    fn make_clip(&mut self, goes: &Goes, audio: &Rendered) -> Result<Made, String> {
        match goes {
            Goes::Lane {
                start,
                ticks,
                name,
                color,
            } => {
                let length = self.ticks_of_frames(*start, audio.frames()).max(*ticks);
                let path = self.write_processed(Processed::Bounce, name, audio, FORMAT)?;
                let at = *start as f64;
                let markers = self.markers_for(at, 0.0, length as f64, at);
                let mode = self.warp_defaults.mode;
                let clip = audio_clip(
                    &path,
                    markers,
                    bounce_name(name),
                    *color,
                    length,
                    false,
                    mode,
                );
                Ok(Made::Lane(Box::new(ArrangementClip::new(clip, *start))))
            }
            Goes::Slot {
                slot,
                ticks,
                lead,
                looped,
                name,
                color,
                active,
                launch,
            } => {
                let path = self.write_processed(Processed::Bounce, name, audio, FORMAT)?;
                let markers = self.steady_markers(*ticks);
                let mode = self.warp_defaults.mode;
                let mut clip = audio_clip(
                    &path,
                    markers,
                    bounce_name(name),
                    *color,
                    *ticks,
                    *looped,
                    mode,
                );
                clip.active = *active;
                clip.launch = launch.clone();
                clip.loop_start = *lead;
                clip.length = (*ticks - *lead).max(1);
                Ok(Made::Slot(*slot, Box::new(clip)))
            }
        }
    }

    /// How many ticks of the song a number of frames lasts, from `start`.
    pub(super) fn ticks_of_frames(&self, start: u64, frames: usize) -> u64 {
        let map = super::build::time_maps(&self.project).tempo;
        let at = map.tick_to_seconds(start as f64) + frames as f64 / self.engine.sample_rate;
        (map.seconds_to_tick(at) - start as f64).round().max(1.0) as u64
    }

    /// Warp markers for a clip that was rendered at the song tempo, from the first frame.
    pub(super) fn steady_markers(&self, ticks: u64) -> Vec<WarpMarker> {
        let seconds = ticks as f64 / rlive_core::PPQ as f64 * 60.0 / self.project.tempo;
        vec![
            WarpMarker {
                seconds: 0.0,
                tick: 0.0,
            },
            WarpMarker {
                seconds,
                tick: ticks as f64,
            },
        ]
    }

    fn make_all(&mut self, goes: &[Goes], audio: &[Rendered]) -> Result<Vec<Made>, String> {
        goes.iter()
            .zip(audio)
            .map(|(g, a)| self.make_clip(g, a))
            .collect()
    }

    /// Turns a track into an empty audio track, keeping its place, mixer and routing.
    pub(super) fn become_audio_track(&mut self, id: TrackId) {
        let Some(t) = self.project.track_mut(id) else {
            return;
        };
        t.kind = TrackKind::Audio;
        t.arm = false;
        t.freeze = None;
        t.routing.input = Route::Default;
        t.routing.tap = Default::default();
        t.routing.monitor = Monitor::default_for(TrackKind::Audio);
        t.take_lanes.clear();
        t.arrangement.clear();
        t.clips.iter_mut().for_each(|c| *c = None);
        let automation = mixer_automation(t, id);
        t.automation = automation;
        let devices: Vec<_> = t.devices.iter().map(|d| d.id).collect();
        for d in devices {
            self.remove_device(ChainRef::Track(id), d);
        }
        self.send_track_edits(vec![GraphOp::SetFrozen {
            track: id,
            frozen: false,
        }]);
    }

    fn install_made(&mut self, id: TrackId, made: Vec<Made>) {
        let mut slots = Vec::new();
        let Some(t) = self.project.track_mut(id) else {
            return;
        };
        for m in made {
            match m {
                Made::Lane(c) => t.place_clip(*c),
                Made::Slot(s, c) => {
                    if let Some(entry) = t.clips.get_mut(s) {
                        *entry = Some(*c);
                        slots.push(s);
                    }
                }
            }
        }
        t.settle_fades();
        self.sync_lane(id);
        for s in slots {
            self.send_clip(id, s);
        }
    }

    // ------------------------------------------------------------- in place

    /// Whether a track has anything to bounce.
    pub fn can_bounce_track(&self, id: TrackId) -> bool {
        let block = self.block_of(id);
        block
            .iter()
            .filter_map(|t| self.project.track(*t))
            .any(|t| !t.arrangement.is_empty() || t.clips.iter().any(Option::is_some))
    }

    /// Bounce Track in Place: every clip in both views, committed to audio on a track that takes
    /// the source's place. A Group Track goes with everything in it. Starts a job.
    pub fn bounce_in_place(&mut self, id: TrackId) -> Result<(), String> {
        if !self.can_bounce_track(id) {
            return Err("There are no clips on this track to bounce".into());
        }
        self.prepare_render();
        let group = self.project.track(id).is_some_and(Track::is_group);
        let (renders, goes) = self.plan_whole(id);
        let label = if group {
            "Bounce Group in Place"
        } else {
            "Bounce Track in Place"
        };
        self.start_render_job(
            label,
            renders,
            false,
            Box::new(move |s, audio| s.finish_in_place(id, group, label, &goes, &audio)),
        )
    }

    fn finish_in_place(
        &mut self,
        id: TrackId,
        group: bool,
        label: &'static str,
        goes: &[Goes],
        audio: &[Rendered],
    ) -> Result<(), String> {
        let made = self.make_all(goes, audio)?;
        self.mark_edit(label);
        if group {
            let inside = self.project.group_contents(id);
            let ops = inside
                .iter()
                .filter_map(|t| self.forget_track(*t))
                .collect();
            self.send_track_edits(ops);
            self.update_midi_targets();
        }
        self.become_audio_track(id);
        if let Some(t) = self.project.track_mut(id) {
            t.name = bounce_name(&t.name);
            if group {
                // A group's mixer is in the audio now, so it mustn't act on it again. It stays
                // switched off if it was off, though, or the new track would suddenly be heard.
                t.volume_db = 0.0;
                t.pan = 0.0;
                t.pan_mode = Default::default();
                t.crossfade = Default::default();
                t.delay = Default::default();
                t.sends.clear();
                t.automation.clear();
            }
        }
        self.push_mix(id, false);
        self.install_made(id, made);
        self.send_track_edits(Vec::new());
        self.update_midi_targets();
        Ok(())
    }

    // ------------------------------------------------------------- to a new track

    /// What a [`BounceSource`] needs rendering on each source track, one track each.
    fn plan_source(&self, source: &BounceSource) -> Vec<(Plan, Vec<Render>, Muting)> {
        let mut out = Vec::new();
        match source {
            BounceSource::Slots(slots) => {
                let mut tracks: Vec<TrackId> = slots.iter().map(|(t, _)| *t).collect();
                tracks.dedup();
                for id in tracks {
                    let mut renders = Vec::new();
                    let mut goes = Vec::new();
                    let chosen: Vec<usize> = slots
                        .iter()
                        .filter(|(t, _)| *t == id)
                        .map(|(_, s)| *s)
                        .collect();
                    for slot in &chosen {
                        if let Some((r, g)) = self.plan_slot(id, *slot) {
                            renders.push(r);
                            goes.push(g);
                        }
                    }
                    out.push((
                        Plan { source: id, goes },
                        renders,
                        Muting::Slots(id, chosen),
                    ));
                }
            }
            BounceSource::Span { tracks, start, end } => {
                let (start, end) = (*start, *end);
                for id in self.outermost(tracks) {
                    let keep = Keep::Span { start, end };
                    if let Some((r, g)) = self.plan_lane(id, keep, false) {
                        let block = self.block_of(id);
                        out.push((
                            Plan {
                                source: id,
                                goes: vec![g],
                            },
                            vec![r],
                            Muting::Span(block, start, end),
                        ));
                    }
                }
            }
            BounceSource::Clips(items) => {
                let mut tracks: Vec<TrackId> = items.iter().map(|(t, _)| *t).collect();
                tracks.dedup();
                for id in tracks {
                    let ids: Vec<ArrClipId> = items
                        .iter()
                        .filter(|(t, _)| *t == id)
                        .map(|(_, c)| *c)
                        .collect();
                    if let Some((r, g)) = self.plan_lane(id, Keep::Clips(ids.clone()), false) {
                        out.push((
                            Plan {
                                source: id,
                                goes: vec![g],
                            },
                            vec![r],
                            Muting::Clips(id, ids),
                        ));
                    }
                }
            }
        }
        out
    }

    /// Drops any track already inside another track in the list.
    fn outermost(&self, tracks: &[TrackId]) -> Vec<TrackId> {
        tracks
            .iter()
            .copied()
            .filter(|t| {
                !self
                    .project
                    .ancestors(*t)
                    .iter()
                    .any(|g| tracks.contains(g))
            })
            .collect()
    }

    /// Bounce to New Track: one new audio track per source, with the source muted. Starts a job.
    pub fn bounce_to_new_track(&mut self, source: BounceSource) -> Result<(), String> {
        self.prepare_render();
        let plans = self.plan_source(&source);
        let mut renders = Vec::new();
        let mut tasks = Vec::new();
        for (plan, rs, mute) in plans {
            if plan.goes.is_empty() {
                continue;
            }
            renders.extend(rs);
            tasks.push((plan, mute));
        }
        if tasks.is_empty() {
            return Err("There is nothing selected to bounce".into());
        }
        self.start_render_job(
            "Bounce to New Track",
            renders,
            false,
            Box::new(move |s, audio| s.finish_new_tracks(tasks, audio)),
        )
    }

    fn finish_new_tracks(
        &mut self,
        tasks: Vec<(Plan, Muting)>,
        audio: Vec<Rendered>,
    ) -> Result<(), String> {
        self.mark_edit("Bounce to New Track");
        let mut audio = audio.into_iter();
        for (plan, mute) in tasks {
            let pieces: Vec<Rendered> = audio.by_ref().take(plan.goes.len()).collect();
            let made = self.make_all(&plan.goes, &pieces)?;
            let Some(source) = self.project.track(plan.source) else {
                continue;
            };
            let new = new_bounce_track(source, self.project.scenes.len());
            let id = new.id;
            let block = self.project.track_block(plan.source);
            let after = block
                .iter()
                .filter_map(|t| self.project.track_index(*t))
                .max()
                .unwrap_or(0);
            self.install_track(new, after + 1);
            self.install_made(id, made);
            self.apply_muting(mute);
        }
        self.mark_edit("Bounce to New Track");
        Ok(())
    }

    fn apply_muting(&mut self, mute: Muting) {
        match mute {
            Muting::Slots(id, slots) => {
                let block = self.block_of(id);
                for t in block {
                    for s in &slots {
                        if let Some(c) = self.clip_mut(t, *s) {
                            c.active = false;
                            self.send_clip(t, *s);
                        }
                    }
                }
            }
            Muting::Span(tracks, start, end) => {
                self.arr_split(&tracks, &[start, end]);
                for t in tracks {
                    self.set_inactive(t, |c| c.start >= start && c.end() <= end);
                }
            }
            Muting::Clips(id, ids) => {
                self.set_inactive(id, |c| ids.contains(&c.id));
            }
        }
    }

    fn set_inactive(&mut self, track: TrackId, pick: impl Fn(&ArrangementClip) -> bool) {
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        for c in t.arrangement.iter_mut().filter(|c| pick(c)) {
            c.clip.active = false;
        }
        self.sync_lane(track);
    }

    // ------------------------------------------------------------- pasting

    /// Whether what was copied still exists (manual p. 430).
    pub fn bounce_source_exists(&self, source: &BounceSource) -> bool {
        match source {
            BounceSource::Slots(slots) => slots.iter().any(|(t, s)| {
                self.block_of(*t)
                    .iter()
                    .any(|m| self.clip(*m, *s).is_some())
            }),
            BounceSource::Span { tracks, start, end } => tracks.iter().any(|t| {
                self.block_of(*t).iter().any(|m| {
                    self.project
                        .track(*m)
                        .is_some_and(|t| !t.copy_span(*start, *end).is_empty())
                })
            }),
            BounceSource::Clips(items) => items.iter().any(|(t, c)| {
                self.project
                    .track(*t)
                    .is_some_and(|t| t.arr_clip(*c).is_some())
            }),
        }
    }

    /// Whether bounced audio can be pasted here: an audio track, a take lane, or a MIDI track
    /// with nothing on it.
    pub fn can_paste_bounced(&self, target: PasteTarget) -> bool {
        match target {
            PasteTarget::Main { track, .. } => self.project.track(track).is_some_and(|t| {
                !t.is_frozen()
                    && match t.kind {
                        TrackKind::Audio => true,
                        TrackKind::Midi => {
                            t.arrangement.is_empty() && t.clips.iter().all(Option::is_none)
                        }
                        _ => false,
                    }
            }),
            PasteTarget::TakeLane { track, lane, .. } => self
                .project
                .track(track)
                .is_some_and(|t| lane < t.take_lanes.len() && !t.is_frozen()),
        }
    }

    /// Paste Bounced Audio: renders the source as it is now, and pastes the result. Starts a job.
    pub fn paste_bounced(
        &mut self,
        source: &BounceSource,
        target: PasteTarget,
    ) -> Result<(), String> {
        if !self.can_paste_bounced(target) {
            return Err("Bounced audio can't be pasted here".into());
        }
        if !self.bounce_source_exists(source) {
            return Err("The material copied for bouncing is gone".into());
        }
        self.prepare_render();
        let (renders, goes) = self.plan_paste(source);
        if renders.is_empty() {
            return Err("There is nothing to bounce".into());
        }
        self.start_render_job(
            "Paste Bounced Audio",
            renders,
            false,
            Box::new(move |s, audio| s.finish_paste(target, &goes, &audio)),
        )
    }

    fn plan_paste(&self, source: &BounceSource) -> (Vec<Render>, Vec<Goes>) {
        let mut out = (Vec::new(), Vec::new());
        let mut add = |planned: Option<(Render, Goes)>| {
            if let Some((r, g)) = planned {
                out.0.push(r);
                out.1.push(g);
            }
        };
        match source {
            BounceSource::Slots(slots) => {
                for (t, s) in slots {
                    add(self.plan_slot(*t, *s));
                }
            }
            BounceSource::Span { tracks, start, end } => {
                let keep = Keep::Span {
                    start: *start,
                    end: *end,
                };
                add(self.plan_tracks_together(tracks, keep));
            }
            BounceSource::Clips(items) => {
                let mut tracks: Vec<TrackId> = items.iter().map(|(t, _)| *t).collect();
                tracks.dedup();
                let ids = items.iter().map(|(_, c)| *c).collect();
                add(self.plan_tracks_together(&tracks, Keep::Clips(ids)));
            }
        }
        out
    }

    /// One render of a selection across several tracks, or of a Group Track's lane.
    fn plan_tracks_together(&self, tracks: &[TrackId], keep: Keep) -> Option<(Render, Goes)> {
        if let [one] = tracks {
            if self.project.track(*one).is_some_and(Track::is_group) {
                return self.plan_lane(*one, keep, false);
            }
        }
        let mut v = post_fx_variant(&self.project, tracks);
        keep_material(&mut v, tracks, &keep);
        let chosen: Vec<&ArrangementClip> = tracks
            .iter()
            .filter_map(|m| v.track(*m))
            .flat_map(|t| t.arrangement.iter())
            .collect();
        let (start, end) = match keep {
            Keep::Span { start, end } if !chosen.is_empty() => (start, end),
            Keep::Span { .. } => return None,
            _ => (
                chosen.iter().map(|c| c.start).min()?,
                chosen.iter().map(|c| c.end()).max()?,
            ),
        };
        let first = chosen.iter().min_by_key(|c| c.start)?;
        let (name, color) = (first.clip.name.clone(), first.clip.color);
        Some((
            Render {
                project: v,
                setup: from_tick(start),
                start,
                ticks: end - start,
                tail: false,
            },
            Goes::Lane {
                start,
                ticks: end - start,
                name,
                color,
            },
        ))
    }

    fn finish_paste(
        &mut self,
        target: PasteTarget,
        goes: &[Goes],
        audio: &[Rendered],
    ) -> Result<(), String> {
        let made = self.make_all(goes, audio)?;
        self.mark_edit("Paste Bounced Audio");
        let (track, mut at) = match target {
            PasteTarget::Main { track, at } | PasteTarget::TakeLane { track, at, .. } => {
                (track, at)
            }
        };
        if self
            .project
            .track(track)
            .is_some_and(|t| t.kind == TrackKind::Midi)
            && matches!(target, PasteTarget::Main { .. })
        {
            self.midi_track_to_audio(track);
        }
        for m in made {
            let mut placed = match m {
                Made::Lane(c) => *c,
                Made::Slot(_, c) => ArrangementClip::new(*c, 0),
            };
            placed.start = at;
            at += placed.length;
            let Some(t) = self.project.track_mut(track) else {
                continue;
            };
            match target {
                PasteTarget::Main { .. } => {
                    t.place_clip(placed);
                    t.settle_fades();
                }
                PasteTarget::TakeLane { lane, .. } => {
                    if let Some(l) = t.take_lanes.get_mut(lane) {
                        l.clips.push(placed);
                        l.clips.sort_by_key(|c| c.start);
                    }
                }
            }
        }
        self.sync_lane(track);
        self.send_track_edits(Vec::new());
        Ok(())
    }

    /// An empty MIDI track becomes an audio track, losing what can't take audio.
    fn midi_track_to_audio(&mut self, id: TrackId) {
        let takes_audio: Vec<_> = self
            .project
            .track(id)
            .map(|t| t.devices.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter(|d| {
                self.runtimes
                    .get(&d.id)
                    .is_none_or(|rt| rt.role != DeviceRole::AudioEffect)
            })
            .map(|d| d.id)
            .collect();
        for d in takes_audio {
            self.remove_device(ChainRef::Track(id), d);
        }
        let Some(t) = self.project.track_mut(id) else {
            return;
        };
        t.kind = TrackKind::Audio;
        t.arm = false;
        t.routing.input = Route::Default;
        t.routing.monitor = Monitor::default_for(TrackKind::Audio);
    }
}

/// How the source of a Bounce to New Track goes silent.
enum Muting {
    Slots(TrackId, Vec<usize>),
    Span(Vec<TrackId>, u64, u64),
    Clips(TrackId, Vec<ArrClipId>),
}

/// An audio clip of a whole processed file, warped to follow the tempo.
#[allow(clippy::too_many_arguments)]
pub(super) fn audio_clip(
    path: &std::path::Path,
    markers: Vec<WarpMarker>,
    name: String,
    color: [u8; 3],
    length: u64,
    looped: bool,
    mode: WarpMode,
) -> Clip {
    let mut content = ClipContent::audio(path, 0.0);
    if let ClipContent::Audio { warp, .. } = &mut content {
        *warp = Warp {
            enabled: true,
            mode,
            markers,
            ..Warp::default()
        };
    }
    Clip {
        name,
        color,
        length: length.max(1),
        looped,
        content,
        ..Clip::default()
    }
}

/// The audio track that takes a bounce: the source's colour and mixer. A group's mixer is in the
/// audio, so a track bounced from one starts at unity (manual pp. 426, 428).
fn new_bounce_track(source: &Track, scenes: usize) -> Track {
    let mut t = Track::new(
        &bounce_name(&source.name),
        TrackKind::Audio,
        source.color,
        scenes,
    );
    t.group = source.group;
    t.routing.output = source.routing.output.clone();
    t.mute = source.mute;
    if !source.is_group() {
        t.volume_db = source.volume_db;
        t.pan = source.pan;
        t.pan_mode = source.pan_mode;
        t.crossfade = source.crossfade;
        t.delay = source.delay;
        t.sends = source.sends.clone();
        t.automation = mixer_automation(source, t.id);
    }
    t
}
