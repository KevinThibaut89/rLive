//! Launching and stopping Session clips.

use crate::capture::SlotRec;
use crate::{clip, retire, Engine, EngineEvent, Garbage};
use rlive_core::{FollowActions, LaunchMode, Quantize, TimeSignature, TrackId};
use std::sync::atomic::Ordering::Relaxed;

pub enum LaunchCmd {
    /// As [`Self::ClipDown`] at full velocity.
    LaunchClip {
        track: TrackId,
        slot: usize,
    },
    /// A clip's launch button went down, or a key or note played it.
    ClipDown {
        track: TrackId,
        slot: usize,
        velocity: f32,
    },
    /// A clip's launch button came up.
    ClipUp {
        track: TrackId,
        slot: usize,
    },
    /// Jump in the playing clip by one global quantization step.
    Nudge {
        track: TrackId,
        forward: bool,
    },
    StopTrack(TrackId),
    LaunchScene(usize),
    /// Take back a scene launch that hasn't happened yet.
    CancelSceneLaunch,
    /// What a scene does when launched.
    SetScene {
        index: usize,
        info: SceneInfo,
    },
    /// How many scenes there are, for scene Follow Actions.
    SetSceneCount(usize),
    /// The playing clip carries on as the clip now in `slot`, with no interruption at all.
    Adopt {
        track: TrackId,
        slot: usize,
    },
    /// The Enable Follow Actions Globally button.
    SetFollowEnabled(bool),
    /// Seed the Follow Actions' random numbers.
    SeedFollow(u64),
    StopAllClips,
    /// Put every track that launched a Session clip back on its lane.
    BackToArrangement,
    /// Jump to `position` in the clip at the next launch point, launching it if need be. With
    /// `repeat`, keep going back there every so many ticks until [`Self::ScrubRelease`].
    Scrub {
        track: TrackId,
        slot: usize,
        position: f64,
        repeat: Option<f64>,
    },
    /// The mouse came up: let a scrubbed clip play on from where it is.
    ScrubRelease(TrackId),
    /// Start recording into an empty slot, at the next launch point.
    RecordSlot {
        track: TrackId,
        slot: usize,
    },
    /// End a slot's recording at the next launch point and play the new clip from there.
    CloseRecording {
        track: TrackId,
    },
}

/// What a scene sets and does when launched.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SceneInfo {
    pub tempo: Option<f64>,
    pub signature: Option<TimeSignature>,
    pub follow: FollowActions,
}

impl SceneInfo {
    /// Whether launching the scene does anything beyond launching its clips.
    pub(crate) fn has_timing(&self) -> bool {
        self.tempo.is_some() || self.signature.is_some() || self.follow.enabled
    }
}

impl Engine {
    pub(crate) fn handle_launch(&mut self, cmd: LaunchCmd) {
        use LaunchCmd as C;
        match cmd {
            C::LaunchClip { track, slot } => {
                if let Some(i) = self.track_index(track) {
                    self.press(i, slot, 1.0);
                }
            }
            C::ClipDown {
                track,
                slot,
                velocity,
            } => {
                if let Some(i) = self.track_index(track) {
                    self.press(i, slot, velocity);
                }
            }
            C::ClipUp { track, slot } => {
                if let Some(i) = self.track_index(track) {
                    self.release(i, slot);
                }
            }
            C::Nudge { track, forward } => {
                if let Some(i) = self.track_index(track) {
                    self.nudge(i, forward);
                }
            }
            C::StopTrack(track) => {
                if let Some(i) = self.track_index(track) {
                    self.stop_track(i);
                }
            }
            C::LaunchScene(scene) => self.launch_scene(scene),
            C::CancelSceneLaunch => self.cancel_scene_launch(),
            C::SetScene { index, info } => {
                if let Some(s) = self.scenes.get_mut(index) {
                    *s = info;
                }
            }
            C::SetSceneCount(n) => self.scene_count = n.min(self.scenes.len()),
            C::Adopt { track, slot } => {
                if let Some(i) = self.track_index(track) {
                    self.adopt(i, slot);
                }
            }
            C::SetFollowEnabled(on) => self.follow_enabled = on,
            C::SeedFollow(seed) => self.follow_random = crate::commands::follow::Random::new(seed),
            C::StopAllClips => {
                self.scene_follow = None;
                self.pending_scene = None;
                for i in 0..self.tracks.len() {
                    self.stop_track(i);
                }
            }
            C::RecordSlot { track, slot } => {
                if let Some(i) = self.track_index(track) {
                    self.record_slot(i, slot);
                }
            }
            C::CloseRecording { track } => {
                if let Some(i) = self.track_index(track) {
                    self.close_recording(i, true);
                }
            }
            C::Scrub {
                track,
                slot,
                position,
                repeat,
            } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    self.scrub(i, slot, position, repeat);
                }
            }
            C::ScrubRelease(track) => {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) {
                    if matches!(t.player.pending, Some(clip::Pending::Scrub { .. })) {
                        t.player.pending = None;
                    }
                }
            }
            C::BackToArrangement => {
                self.scene_follow = None;
                self.pending_scene = None;
                for t in self.tracks.iter_mut() {
                    t.player.pending = None;
                    t.player.repeat = None;
                    if !t.follow {
                        t.player.follow = None;
                        t.player.release_all(0, &mut t.live);
                        if let Some(p) = t.player.playing.take() {
                            retire(&mut self.garbage, Garbage::Shared(p.clip));
                        }
                        t.follow = true;
                        t.lane.release_and_resync(0, &mut t.live);
                    }
                }
            }
        }
    }

    fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// The next launch point on a slot's quantization: its own, or the global one.
    pub(crate) fn launch_point_of(&self, quantize: Option<Quantize>) -> f64 {
        self.maps
            .bars
            .next_boundary(self.tick, quantize.unwrap_or(self.quantize))
    }

    /// The first launch point after `tick` on this quantization, if it has any.
    fn repeat_point(&self, tick: f64, q: Quantize) -> Option<f64> {
        (q != Quantize::None).then(|| self.maps.bars.next_boundary(tick + 1e-3, q))
    }

    /// A clip's launch button went down: what happens depends on its Launch Mode.
    fn press(&mut self, track_idx: usize, slot: usize, velocity: f32) {
        let settings = self.tracks[track_idx].launch_of(slot);
        let has_clip = matches!(self.tracks[track_idx].slots.get(slot), Some(Some(_)));
        if !has_clip {
            self.launch(track_idx, slot, velocity);
            return;
        }
        if settings.mode == LaunchMode::Toggle && self.tracks[track_idx].player.is_on(slot) {
            let at = self.launch_point_of(settings.quantize);
            let player = &mut self.tracks[track_idx].player;
            player.pending = Some(clip::Pending::Stop { at });
            player.from_scene = false;
            return;
        }
        self.launch(track_idx, slot, velocity);
        if settings.mode == LaunchMode::Repeat {
            let q = settings.quantize.unwrap_or(self.quantize);
            let from = self.tracks[track_idx].player.pending.map(|p| p.at());
            let next = from.and_then(|at| self.repeat_point(at, q));
            self.tracks[track_idx].player.repeat = next.map(|next| clip::Repeat {
                slot,
                quantize: q,
                velocity,
                next,
            });
        }
    }

    /// A clip's launch button came up. Gate and Repeat clips stop on their quantization.
    fn release(&mut self, track_idx: usize, slot: usize) {
        let settings = self.tracks[track_idx].launch_of(slot);
        if !matches!(settings.mode, LaunchMode::Gate | LaunchMode::Repeat) {
            return;
        }
        let at = self.launch_point_of(settings.quantize);
        let player = &mut self.tracks[track_idx].player;
        player.repeat = None;
        match player.pending {
            Some(clip::Pending::Launch { slot: s, .. }) if s == slot => {
                player.released_early = true;
            }
            Some(_) => {}
            None => {
                if player.playing.as_ref().is_some_and(|p| p.slot == slot) {
                    player.pending = Some(clip::Pending::Stop { at });
                }
            }
        }
    }

    /// Queue a launch of `slot` on its quantization, starting the transport if it isn't running.
    fn launch(&mut self, track_idx: usize, slot: usize, velocity: f32) {
        self.start_transport();
        let settings = self.tracks[track_idx].launch_of(slot);
        let at = self.launch_point_of(settings.quantize);
        if let Some(rec) = self.tracks[track_idx].slot_rec {
            if rec.close.is_none() {
                self.close_recording(track_idx, rec.slot == slot);
                if rec.slot == slot {
                    return;
                }
            }
        }
        let track = &mut self.tracks[track_idx];
        let has_clip = matches!(track.slots.get(slot), Some(Some(_)));
        if has_clip && !settings.active {
            return;
        }
        if !has_clip && !settings.stop_button {
            return;
        }
        let player = &mut track.player;
        player.released_early = false;
        player.repeat = None;
        player.from_scene = false;
        player.pending = Some(if has_clip {
            clip::Pending::Launch { slot, at, velocity }
        } else {
            clip::Pending::Stop { at }
        });
    }

    fn launch_scene(&mut self, scene: usize) {
        self.start_transport();
        let at = self.launch_point();
        for i in 0..self.tracks.len() {
            self.launch(i, scene, 1.0);
            self.tracks[i].player.from_scene = self.tracks[i].player.pending.is_some();
        }
        let info = self.scenes.get(scene).copied().unwrap_or_default();
        self.pending_scene = info
            .has_timing()
            .then_some(crate::commands::follow::PendingScene { scene, at });
    }

    fn cancel_scene_launch(&mut self) {
        self.pending_scene = None;
        for t in self.tracks.iter_mut() {
            if t.player.from_scene {
                t.player.pending = None;
                t.player.from_scene = false;
                t.player.released_early = false;
            }
        }
    }

    /// Jump within the playing clip by the global quantization (manual p. 364).
    fn nudge(&mut self, track_idx: usize, forward: bool) {
        let step = self.quantize.ticks(self.maps.bars.signature_at(self.tick)) as f64;
        if step <= 0.0 {
            return;
        }
        let tick = self.tick;
        let track = &mut self.tracks[track_idx];
        let player = &mut track.player;
        let Some(playing) = player.playing.as_mut() else {
            return;
        };
        let len = playing.clip.length() as f64;
        if len <= 0.0 {
            return;
        }
        let position = (tick - playing.start_tick).max(0.0);
        let moved = position + if forward { step } else { -step };
        // Before the start marker there's only the loop to wrap into, if the run is in it.
        let moved = if moved >= 0.0 {
            moved
        } else if playing.region.looped && playing.region.lead() <= 0.0 {
            moved.rem_euclid(len)
        } else {
            0.0
        };
        let delta = position - moved;
        playing.start_tick += delta;
        // Linked Follow Actions keep to the end of the clip, which has just moved.
        if let Some(f) = player.follow.as_mut().filter(|f| f.rule.linked) {
            f.at += delta;
        }
        player.release_all(0, &mut track.live);
    }

    fn adopt(&mut self, track_idx: usize, slot: usize) {
        let track = &mut self.tracks[track_idx];
        let Some(Some(clip)) = track.slots.get(slot).cloned() else {
            return;
        };
        let player = &mut track.player;
        let tick = self.tick;
        if player.playing.is_none() {
            retire(&mut self.garbage, Garbage::Shared(clip));
            return;
        }
        if let Some(old) = player.replace_playing(clip, tick) {
            retire(&mut self.garbage, Garbage::Shared(old));
        }
        if let Some(playing) = player.playing.as_mut() {
            playing.slot = slot;
        }
        if let Some(f) = player.follow.as_mut() {
            f.slot = slot;
        }
    }

    fn scrub(&mut self, track_idx: usize, slot: usize, position: f64, repeat: Option<f64>) {
        self.start_transport();
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        if matches!(track.slots.get(slot), Some(Some(_))) && track.launch_of(slot).active {
            track.player.pending = Some(clip::Pending::Scrub {
                slot,
                at,
                position,
                repeat,
            });
        }
    }

    fn stop_track(&mut self, track_idx: usize) {
        if self.tracks[track_idx]
            .slot_rec
            .is_some_and(|r| r.close.is_none())
        {
            self.close_recording(track_idx, false);
            return;
        }
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        track.player.repeat = None;
        track.player.from_scene = false;
        if track.player.playing.is_some() || track.player.pending.is_some() {
            track.player.pending = Some(clip::Pending::Stop { at });
        }
    }

    fn record_slot(&mut self, track_idx: usize, slot: usize) {
        let from = self.launch_point();
        let track = &mut self.tracks[track_idx];
        if track.slot_rec.is_some() {
            return;
        }
        track.slot_rec = Some(SlotRec {
            slot,
            from,
            started: false,
            close: None,
        });
        track.shared.record_queued_slot.store(slot as i32, Relaxed);
        if track.player.playing.is_some() || track.player.pending.is_some() {
            track.player.pending = Some(clip::Pending::Stop { at: from });
        }
        // After the track is waiting, so a stopped transport waits for the count-in with it.
        self.start_transport();
    }

    /// End a slot's recording at the next launch point; with `launch` its clip plays from there.
    fn close_recording(&mut self, track_idx: usize, launch: bool) {
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        let Some(rec) = track.slot_rec.as_mut() else {
            return;
        };
        if rec.close.is_some() {
            return;
        }
        // Closing before it has started records nothing, and the slot stays empty.
        let at = at.max(rec.from);
        rec.close = Some((at, launch));
        let slot = rec.slot;
        track.player.pending = Some(if launch {
            clip::Pending::Launch {
                slot,
                at,
                velocity: 1.0,
            }
        } else {
            clip::Pending::Stop { at }
        });
        track.player.hold_for_arrival = launch;
        let frames = track.capture.as_ref().map(|c| c.frames());
        let id = track.id;
        let tps = self.tempo_run().ticks_per_sample;
        let frames_until = ((at - self.tick) / tps).max(0.0).round() as u64;
        let _ = self.events.push(EngineEvent::SlotClosing {
            track: id,
            slot,
            at,
            launch,
            frames: frames.map(|f| f + frames_until),
        });
    }
}
