//! Launch settings, Follow Actions and the shape of the Session grid: everything about how clips
//! and scenes get launched and where they sit, apart from the launching itself.

use super::{ClipRef, Session};
use crate::session::launch_log::tick_u64;
use rlive_audio_io::AUDIO_EXTENSIONS;
use rlive_core::{FollowAction, FollowActions, LaunchMode, Quantize, TimeSignature, TrackId};
use rlive_engine::{
    ClipCmd, EngineCommand, LaunchCmd, SceneInfo, SlotLaunch, TransportCmd, NO_OLD_SLOT,
};
use rlive_project::{ArrangementClip, Clip, LaunchSettings, Track, TrackKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// How many scenes the engine can hold.
const MAX_SCENES: usize = rlive_engine::MAX_SCENES;

/// What a new clip's launch controls start out as, from the Record, Warp & Launch settings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LaunchDefaults {
    pub mode: LaunchMode,
    /// `None` is Global.
    pub quantize: Option<Quantize>,
}

impl LaunchDefaults {
    pub fn settings(&self) -> LaunchSettings {
        LaunchSettings {
            mode: self.mode,
            quantize: self.quantize,
            ..LaunchSettings::default()
        }
    }
}

/// The engine's copy of a slot's launch settings.
pub(super) fn slot_launch(track: &Track, slot: usize) -> SlotLaunch {
    let clip = track.clips.get(slot).and_then(Option::as_ref);
    let settings = clip.map(|c| c.launch.clone()).unwrap_or_default();
    SlotLaunch {
        mode: settings.mode,
        quantize: settings.quantize,
        legato: settings.legato,
        velocity_amount: settings.velocity_amount,
        active: clip.is_none_or(|c| c.active),
        stop_button: track.has_stop_button(slot),
        follow: settings.follow,
    }
}

fn scene_info(scene: &rlive_project::Scene) -> SceneInfo {
    SceneInfo {
        tempo: scene.tempo,
        signature: scene.signature,
        follow: scene.follow,
    }
}

/// Whether two slots' clips can share a track.
fn fits(track: &Track, clip: &Clip) -> bool {
    match track.kind {
        TrackKind::Midi => clip.is_midi(),
        // A frozen MIDI clip can go to an audio track as its audio (manual p. 986).
        TrackKind::Audio => !clip.is_midi() || clip.frozen.is_some(),
        _ => false,
    }
}

/// Valid for a scene's or the transport's time signature (manual p. 191).
pub fn signature_is_valid(s: TimeSignature) -> bool {
    (1..=99).contains(&s.numerator) && matches!(s.denominator, 1 | 2 | 4 | 8 | 16)
}

impl Session {
    // ------------------------------------------------------------- the engine's copies

    /// Tell the engine what a slot does when launched.
    pub(super) fn sync_launch(&mut self, track: TrackId, slot: usize) {
        let Some(t) = self.project.track(track) else {
            return;
        };
        let launch = slot_launch(t, slot);
        self.send(EngineCommand::Clip(ClipCmd::SetLaunch {
            track,
            slot,
            launch,
        }));
    }

    /// Tell the engine what every scene does when launched.
    pub(super) fn sync_scenes(&mut self) {
        let infos: Vec<SceneInfo> = self.project.scenes.iter().map(scene_info).collect();
        self.send(EngineCommand::Launch(LaunchCmd::SetSceneCount(infos.len())));
        for (index, info) in infos.into_iter().enumerate() {
            self.send(EngineCommand::Launch(LaunchCmd::SetScene { index, info }));
        }
    }

    pub(super) fn send_follow_enabled(&mut self) {
        let on = self.project.follow_actions_on;
        self.send(EngineCommand::Launch(LaunchCmd::SetFollowEnabled(on)));
    }

    /// Rearrange the scenes, in the project and the engine, without touching what's playing.
    /// Entry `new` of `map` is the scene that goes there; an index past the end makes a new one.
    pub(super) fn remap_scenes(&mut self, map: &[usize]) {
        let old = self.project.scenes.len();
        self.project.remap_scenes(map);
        let slots: Vec<u16> = map
            .iter()
            .map(|i| if *i < old { *i as u16 } else { NO_OLD_SLOT })
            .collect();
        self.send(EngineCommand::Clip(ClipCmd::RemapSlots(Arc::new(slots))));
        self.sync_scenes();
        // Session automation addresses clips by their slots.
        let ids: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for id in ids {
            self.sync_clip_routes(id);
        }
    }

    // ------------------------------------------------------------- launch settings

    /// Changes the launch settings of some clips and tells the engine.
    pub fn edit_launch(&mut self, clips: &[ClipRef], mut edit: impl FnMut(&mut LaunchSettings)) {
        let mut any = false;
        for r in clips {
            let Some(clip) = self.clip_at_mut(*r) else {
                continue;
            };
            edit(&mut clip.launch);
            any = true;
            if let ClipRef::Slot { track, slot } = *r {
                self.sync_launch(track, slot);
                self.clip_edited_while_logging(track, slot);
            }
        }
        if any {
            self.mark_edit("Edit Launch Settings");
        }
    }

    /// The Follow Action button, for these clips.
    pub fn toggle_clip_follow(&mut self, clips: &[ClipRef]) {
        let on = !clips
            .iter()
            .all(|r| self.clip_at(*r).is_some_and(|c| c.launch.follow.enabled));
        self.edit_launch(clips, |l| set_follow_on(&mut l.follow, on));
    }

    /// Sets every selected clip to go on to the next, the last back to the first, so the
    /// selection plays in a loop. It needn't be contiguous.
    pub fn create_follow_chain(&mut self, clips: &[(TrackId, usize)]) {
        let mut tracks: Vec<TrackId> = Vec::new();
        for (t, _) in clips {
            if !tracks.contains(t) {
                tracks.push(*t);
            }
        }
        let mut any = false;
        for track in tracks {
            let mut slots: Vec<usize> = clips
                .iter()
                .filter(|(t, s)| *t == track && self.clip(*t, *s).is_some())
                .map(|(_, s)| *s)
                .collect();
            slots.sort_unstable();
            slots.dedup();
            for (i, slot) in slots.iter().enumerate() {
                let next = slots[(i + 1) % slots.len()] as u32;
                if let Some(c) = self.clip_mut(track, *slot) {
                    c.launch.follow = FollowActions {
                        enabled: true,
                        action_a: FollowAction::Jump(next),
                        chance_a: 1.0,
                        chance_b: 0.0,
                        ..c.launch.follow
                    };
                    any = true;
                }
                self.sync_launch(track, *slot);
            }
        }
        if any {
            self.mark_edit("Create Follow Action Chain");
        }
    }

    pub fn set_follow_actions_on(&mut self, on: bool) {
        self.project.follow_actions_on = on;
        self.send_follow_enabled();
        self.mark_edit("Enable Follow Actions");
    }

    /// Whether any clip or scene in the Set has Follow Actions. Otherwise the global button is
    /// greyed out.
    pub fn has_follow_actions(&self) -> bool {
        let p = &self.project;
        p.scenes.iter().any(|s| s.follow.enabled)
            || p.tracks
                .iter()
                .any(|t| t.clips.iter().flatten().any(|c| c.launch.follow.enabled))
    }

    /// The 0 key: deactivates the clips, or activates them if they all are off.
    pub fn toggle_clip_active(&mut self, clips: &[(TrackId, usize)]) {
        let on = clips
            .iter()
            .all(|(t, s)| self.clip(*t, *s).is_none_or(|c| !c.active));
        let mut any = false;
        for (t, s) in clips {
            if let Some(c) = self.clip_mut(*t, *s) {
                c.active = on;
                any = true;
            }
            self.sync_launch(*t, *s);
        }
        if any {
            self.mark_edit("Deactivate Clip");
        }
    }

    pub fn nudge_clip(&mut self, track: TrackId, forward: bool) {
        self.send(EngineCommand::Launch(LaunchCmd::Nudge { track, forward }));
    }

    pub fn cancel_scene_launch(&mut self) {
        self.send(EngineCommand::Launch(LaunchCmd::CancelSceneLaunch));
    }

    /// Whether a slot has a Clip Stop button.
    pub fn has_stop_button(&self, track: TrackId, slot: usize) -> bool {
        self.project
            .track(track)
            .is_none_or(|t| t.has_stop_button(slot))
    }

    /// The Add/Remove Stop Button command, for these slots.
    pub fn toggle_stop_buttons(&mut self, slots: &[(TrackId, usize)]) {
        let on = !slots.iter().all(|(t, s)| self.has_stop_button(*t, *s));
        for (t, s) in slots {
            if let Some(track) = self.project.track_mut(*t) {
                track.set_stop_button(*s, on);
            }
            self.sync_launch(*t, *s);
        }
        if !slots.is_empty() {
            self.mark_edit("Add/Remove Stop Button");
        }
    }

    // ------------------------------------------------------------- scenes

    /// Puts a new, empty scene below `below`, and returns where it went.
    pub fn insert_scene(&mut self, below: usize) -> Option<usize> {
        let n = self.project.scenes.len();
        if n >= MAX_SCENES {
            return None;
        }
        let at = (below + 1).min(n);
        let map: Vec<usize> = (0..at).chain([usize::MAX]).chain(at..n).collect();
        self.remap_scenes(&map);
        self.mark_edit("Insert Scene");
        Some(at)
    }

    /// Puts a scene below `below` holding copies of what's playing, and hands playback over to
    /// the copies with no interruption (manual p. 196).
    pub fn capture_and_insert_scene(&mut self, below: usize) -> Option<usize> {
        let running: Vec<(TrackId, usize)> = self
            .project
            .tracks
            .iter()
            .filter_map(|t| {
                let slot = self
                    .track_shared
                    .get(&t.id)?
                    .playing_slot
                    .load(Ordering::Relaxed);
                (slot >= 0).then_some((t.id, slot as usize))
            })
            .collect();
        let at = self.insert_scene(below)?;
        for (track, slot) in running {
            let from = if slot >= at { slot + 1 } else { slot };
            let Some(clip) = self.clip(track, from).cloned() else {
                continue;
            };
            if let Some(t) = self.project.track_mut(track) {
                if let Some(entry) = t.clips.get_mut(at) {
                    *entry = Some(clip);
                }
            }
            self.send_clip(track, at);
            self.send(EngineCommand::Launch(LaunchCmd::Adopt { track, slot: at }));
        }
        self.mark_edit("Capture and Insert Scene");
        Some(at)
    }

    /// Mouse drag: moves the scenes to `to` as one block, non-adjacent ones closing up.
    /// `to` is the index of the scene the block goes before, counted as the list stands now.
    pub fn move_scenes(&mut self, selection: &[usize], to: usize) -> Vec<usize> {
        let n = self.project.scenes.len();
        let mut chosen: Vec<usize> = selection.iter().copied().filter(|s| *s < n).collect();
        chosen.sort_unstable();
        chosen.dedup();
        if chosen.is_empty() {
            return Vec::new();
        }
        let rest: Vec<usize> = (0..n).filter(|s| !chosen.contains(s)).collect();
        let before = rest.iter().filter(|s| **s < to).count();
        let map: Vec<usize> = rest[..before]
            .iter()
            .chain(&chosen)
            .chain(&rest[before..])
            .copied()
            .collect();
        if map.iter().copied().ne(0..n) {
            self.remap_scenes(&map);
            self.mark_edit("Move Scene");
        }
        (before..before + chosen.len()).collect()
    }

    /// Cmd+arrow: moves each scene one place, gaps between them staying.
    pub fn shift_scenes(&mut self, selection: &[usize], delta: isize) -> Vec<usize> {
        let n = self.project.scenes.len();
        let mut chosen: Vec<usize> = selection.iter().copied().filter(|s| *s < n).collect();
        chosen.sort_unstable();
        chosen.dedup();
        let at_edge = match delta {
            -1 => chosen.first() == Some(&0),
            1 => chosen.last() == Some(&(n.saturating_sub(1))),
            _ => true,
        };
        if chosen.is_empty() || at_edge {
            return chosen;
        }
        let mut map: Vec<usize> = (0..n).collect();
        let order: Vec<usize> = if delta < 0 {
            chosen.clone()
        } else {
            chosen.iter().rev().copied().collect()
        };
        for s in order {
            let other = s.wrapping_add_signed(delta);
            map.swap(s, other);
        }
        self.remap_scenes(&map);
        self.mark_edit("Move Scene");
        chosen
            .iter()
            .map(|s| s.wrapping_add_signed(delta))
            .collect()
    }

    pub fn edit_scenes(
        &mut self,
        scenes: &[usize],
        label: &'static str,
        mut edit: impl FnMut(&mut rlive_project::Scene),
    ) {
        let mut any = false;
        for i in scenes {
            if let Some(s) = self.project.scenes.get_mut(*i) {
                edit(s);
                any = true;
            }
        }
        if any {
            self.sync_scenes();
            self.mark_edit(label);
        }
    }

    /// Scene Tempo. `None` disables it.
    pub fn set_scene_tempo(&mut self, scenes: &[usize], tempo: Option<f64>) {
        let tempo = tempo.map(|t| t.clamp(20.0, 999.0));
        self.edit_scenes(scenes, "Scene Tempo", |s| s.tempo = tempo);
    }

    /// Scene Time Signature. `None` disables it; one Live wouldn't allow is ignored.
    pub fn set_scene_signature(&mut self, scenes: &[usize], signature: Option<TimeSignature>) {
        if signature.is_some_and(|s| !signature_is_valid(s)) {
            return;
        }
        self.edit_scenes(scenes, "Scene Time Signature", |s| s.signature = signature);
    }

    /// The Follow Action button, for these scenes.
    pub fn toggle_scene_follow(&mut self, scenes: &[usize]) {
        let on = !scenes.iter().all(|s| {
            self.project
                .scenes
                .get(*s)
                .is_some_and(|s| s.follow.enabled)
        });
        self.edit_scenes(scenes, "Scene Follow Action", |s| {
            set_follow_on(&mut s.follow, on)
        });
    }

    // ------------------------------------------------------------- moving clips around

    /// Moves or copies clips by whole tracks and scenes. Clips land only on tracks that can play
    /// them; anything else, or that would fall off the grid, stays. Returns where they went.
    pub fn move_slots(
        &mut self,
        items: &[(TrackId, usize)],
        tracks: isize,
        scenes: isize,
        copy: bool,
    ) -> Vec<(TrackId, usize)> {
        let order: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        let n = self.project.scenes.len() as isize;
        let mut moves: Vec<(TrackId, usize, TrackId, usize)> = Vec::new();
        for (t, s) in items {
            let Some(i) = order.iter().position(|o| o == t) else {
                continue;
            };
            let Some(clip) = self.clip(*t, *s) else {
                continue;
            };
            let (ti, si) = (i as isize + tracks, *s as isize + scenes);
            if ti < 0 || ti as usize >= order.len() || si < 0 || si >= n {
                continue;
            }
            let to_track = order[ti as usize];
            let fit = self.project.track(to_track).is_some_and(|d| fits(d, clip));
            if fit {
                moves.push((*t, *s, to_track, si as usize));
            }
        }
        let carried: Vec<Clip> = moves
            .iter()
            .filter_map(|(t, s, ..)| self.clip(*t, *s).cloned())
            .collect();
        let mut sync: Vec<(TrackId, usize)> = Vec::new();
        if !copy {
            for (t, s, ..) in &moves {
                if let Some(track) = self.project.track_mut(*t) {
                    track.clips[*s] = None;
                }
                sync.push((*t, *s));
            }
        }
        for ((.., to_t, to_s), clip) in moves.iter().zip(carried) {
            if let Some(track) = self.project.track_mut(*to_t) {
                track.clips[*to_s] = Some(super::arrangement::leaving_frozen_track(clip, track));
            }
            sync.push((*to_t, *to_s));
        }
        for (t, s) in &sync {
            self.send_clip(*t, *s);
        }
        if !sync.is_empty() {
            self.mark_edit("Move Clips");
        }
        moves.iter().map(|m| (m.2, m.3)).collect()
    }

    /// Lays Arrangement clips out in the Session top to bottom in the order they start, from
    /// scene `first` down, adding scenes as needed. Clips that start together share a scene.
    pub fn paste_arrangement_clips(
        &mut self,
        clips: &[(TrackId, ArrangementClip)],
        first: usize,
    ) -> Vec<(TrackId, usize)> {
        let mut starts: Vec<u64> = clips.iter().map(|(_, c)| c.start).collect();
        starts.sort_unstable();
        starts.dedup();
        let needed = (first + starts.len()).min(MAX_SCENES);
        while self.project.scenes.len() < needed {
            self.project.add_scene();
        }
        self.sync_scenes();
        let mut placed = Vec::new();
        for (track, clip) in clips {
            let Some(row) = starts.iter().position(|s| *s == clip.start) else {
                continue;
            };
            let slot = first + row;
            let fit = self
                .project
                .track(*track)
                .is_some_and(|t| fits(t, &clip.clip));
            if !fit || slot >= self.project.scenes.len() {
                continue;
            }
            if let Some(t) = self.project.track_mut(*track) {
                let landed = super::arrangement::leaving_frozen_track(clip.clip.clone(), t);
                t.clips[slot] = Some(landed);
            }
            self.send_clip(*track, slot);
            placed.push((*track, slot));
        }
        if !placed.is_empty() {
            self.mark_edit("Paste Clips");
        }
        placed
    }

    /// Files dropped on a slot: down one track, or with `spread` across tracks from there.
    /// Ones that aren't audio or don't fit are skipped. Returns where each landed.
    pub fn drop_files(
        &mut self,
        paths: &[PathBuf],
        track: TrackId,
        slot: usize,
        spread: bool,
    ) -> Vec<(TrackId, usize)> {
        let audio: Vec<&PathBuf> = paths.iter().filter(|p| is_audio_file(p)).collect();
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        let Some(mut column) = tracks.iter().position(|t| *t == track) else {
            return Vec::new();
        };
        let mut row = slot;
        let mut placed = Vec::new();
        for path in audio {
            let Some(target) = tracks.get(column).copied() else {
                break;
            };
            if self
                .project
                .track(target)
                .is_none_or(|t| t.kind != TrackKind::Audio)
            {
                column += 1;
                continue;
            }
            while row >= self.project.scenes.len() && self.project.scenes.len() < MAX_SCENES {
                self.project.add_scene();
                self.sync_scenes();
            }
            if row >= self.project.scenes.len() {
                break;
            }
            let Ok(clip) = self.audio_clip_from_file(path, target) else {
                continue;
            };
            if let Some(t) = self.project.track_mut(target) {
                t.clips[row] = Some(clip);
            }
            self.send_clip(target, row);
            placed.push((target, row));
            if spread {
                column += 1;
            } else {
                row += 1;
            }
        }
        if !placed.is_empty() {
            self.mark_edit("Add Clips");
        }
        placed
    }

    // ------------------------------------------------------------- scene timing

    /// A launched scene set the tempo or meter at `tick`.
    pub(super) fn scene_timing(
        &mut self,
        tick: f64,
        tempo: Option<f64>,
        signature: Option<TimeSignature>,
    ) {
        self.timing_held = true;
        let recording = self.is_recording();
        if let Some(bpm) = tempo {
            let was = super::build::time_maps(&self.project)
                .tempo
                .bpm_at(tick - 1.0);
            self.project.tempo = bpm.clamp(20.0, 999.0);
            if recording {
                self.log_tempo(tick_u64(tick) as f64, was, self.project.tempo);
            }
        }
        if let Some(s) = signature {
            if recording {
                self.log_signature(tick_u64(tick), s);
            } else {
                self.project.time_signature = s;
            }
        }
        self.dirty = true;
    }

    /// Once playback is over, the song's tempo and meter are the project's own again.
    pub(super) fn release_scene_timing(&mut self) {
        if self.timing_held && !self.is_playing() {
            self.timing_held = false;
            self.send(EngineCommand::Transport(TransportCmd::SetTimeMaps(
                Arc::new(super::build::time_maps(&self.project)),
            )));
        }
    }
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e)))
}

/// The Follow Action button: turned on with nothing chosen, a clip goes on to the next one.
pub fn set_follow_on(follow: &mut FollowActions, on: bool) {
    follow.enabled = on;
    if on && follow.action_a == FollowAction::None && follow.action_b == FollowAction::None {
        follow.action_a = FollowAction::Next;
    }
}
