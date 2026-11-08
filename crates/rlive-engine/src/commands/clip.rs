//! Replacing clip content.

use crate::clip::{ClipUpdate, Pending, Playing, SlotLaunch};
use crate::track::MAX_SCENES;
use crate::{retire, ArrangementLane, ClipData, Engine, Garbage};
use rlive_core::TrackId;
use std::sync::Arc;

pub enum ClipCmd {
    SetClip {
        track: TrackId,
        slot: usize,
        clip: Option<Arc<ClipData>>,
    },
    /// What a slot does when launched.
    SetLaunch {
        track: TrackId,
        slot: usize,
        launch: SlotLaunch,
    },
    /// Rearrange every track's slots at once, for scenes being inserted, deleted or moved. Entry
    /// `new` is the old slot that lands there, or [`NO_OLD_SLOT`] for a fresh empty one. Slots
    /// not listed are dropped. What's playing keeps playing, wherever it went.
    RemapSlots(Arc<Vec<u16>>),
    /// Replace a track's Arrangement lane.
    SetLane {
        track: TrackId,
        lane: Arc<ArrangementLane>,
    },
    /// MIDI Envelope Auto-Reset: each new clip sets some controllers back first.
    SetMidiAutoReset(bool),
    /// Clip Update Rate: edits reach a running clip on this grid of ticks. 0 is at once.
    SetUpdateRate(u64),
}

/// In [`ClipCmd::RemapSlots`], a slot that is new.
pub const NO_OLD_SLOT: u16 = u16::MAX;

impl Engine {
    /// When an edit made now reaches a running clip, or `None` for at once.
    fn update_tick(&self) -> Option<f64> {
        let rate = self.clip_update_rate as f64;
        if rate <= 0.0 || !self.transport_rolling() {
            return None;
        }
        let at = (self.command_tick / rate).ceil() * rate;
        (at > self.command_tick).then_some(at)
    }

    pub(crate) fn handle_clip(&mut self, cmd: ClipCmd) {
        use ClipCmd as C;
        let update_at = self.update_tick();
        let tick = self.command_tick;
        match cmd {
            C::SetClip { track, slot, clip } => {
                let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) else {
                    if let Some(c) = clip {
                        retire(&mut self.garbage, Garbage::Shared(c));
                    }
                    return;
                };
                while t.slots.len() <= slot && t.slots.len() < t.slots.capacity() {
                    t.slots.push(None);
                }
                let Some(entry) = t.slots.get_mut(slot) else {
                    if let Some(c) = clip {
                        retire(&mut self.garbage, Garbage::Shared(c));
                    }
                    return;
                };
                // Live-update a playing clip (e.g. edits in the piano roll).
                if let Some(p) = t.player.playing.as_mut() {
                    if p.slot == slot {
                        match (&clip, update_at) {
                            (Some(new), Some(at)) => {
                                let update = ClipUpdate {
                                    clip: new.clone(),
                                    at,
                                };
                                if let Some(old) = t.player.update.replace(update) {
                                    retire(&mut self.garbage, Garbage::Shared(old.clip));
                                }
                            }
                            (Some(new), None) => {
                                if let Some(old) = t.player.replace_playing(new.clone(), tick) {
                                    retire(&mut self.garbage, Garbage::Shared(old));
                                }
                                t.player.release_all(0, &mut t.live);
                            }
                            (None, _) => {
                                if let Some(p) = t.player.playing.take() {
                                    retire(&mut self.garbage, Garbage::Shared(p.clip));
                                }
                                if let Some(u) = t.player.update.take() {
                                    retire(&mut self.garbage, Garbage::Shared(u.clip));
                                }
                                t.player.release_all(0, &mut t.live);
                            }
                        }
                    }
                }
                // A recording closed before its clip was ready: it starts now, in position.
                if let (Some((want, at)), Some(new)) = (t.player.late, &clip) {
                    if want == slot {
                        t.player.late = None;
                        t.player.playing = Some(Playing::new(slot, new.clone(), at, 1.0));
                    }
                }
                if let Some(old) = std::mem::replace(entry, clip) {
                    retire(&mut self.garbage, Garbage::Shared(old));
                }
            }
            C::SetLaunch {
                track,
                slot,
                launch,
            } => {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) {
                    t.set_launch(slot, launch);
                }
            }
            C::RemapSlots(map) => {
                self.remap_slots(&map);
                retire(&mut self.garbage, Garbage::Shared(map));
            }
            C::SetMidiAutoReset(on) => self.midi_auto_reset = on,
            C::SetUpdateRate(rate) => self.clip_update_rate = rate,
            C::SetLane { track, lane } => {
                let tick = self.tick;
                let old = match self.tracks.iter_mut().find(|t| t.id == track) {
                    Some(t) => match update_at.filter(|_| t.lane.in_clip()) {
                        Some(at) => t.lane.update.replace((lane, at)).map(|(old, _)| old),
                        None => {
                            let waiting = t.lane.update.take().map(|(old, _)| old);
                            if let Some(waiting) = waiting {
                                retire(&mut self.garbage, Garbage::Shared(waiting));
                            }
                            Some(t.lane.swap(lane, tick, &mut t.live))
                        }
                    },
                    None => Some(lane),
                };
                if let Some(old) = old {
                    retire(&mut self.garbage, Garbage::Shared(old));
                }
            }
        }
    }

    fn remap_slots(&mut self, map: &[u16]) {
        // Where old slot `old` went, if anywhere.
        let moved = |old: usize| map.iter().position(|o| *o as usize == old);
        let n = map.len().min(MAX_SCENES);
        for t in self.tracks.iter_mut() {
            self.slot_scratch.clear();
            self.launch_scratch.clear();
            for old in map[..n].iter() {
                let old = *old as usize;
                self.slot_scratch
                    .push(t.slots.get_mut(old).and_then(Option::take));
                self.launch_scratch
                    .push(t.launch.get(old).copied().unwrap_or_default());
            }
            // What wasn't carried over has been deleted.
            for clip in t.slots.iter_mut() {
                if let Some(c) = clip.take() {
                    retire(&mut self.garbage, Garbage::Shared(c));
                }
            }
            t.slots.clear();
            t.slots.append(&mut self.slot_scratch);
            t.launch[..n].copy_from_slice(&self.launch_scratch);
            t.launch[n..].fill(SlotLaunch::default());

            let p = &mut t.player;
            if let Some(playing) = p.playing.as_mut() {
                match moved(playing.slot) {
                    Some(to) => playing.slot = to,
                    None => {
                        p.release_all(0, &mut t.live);
                        if let Some(old) = p.playing.take() {
                            retire(&mut self.garbage, Garbage::Shared(old.clip));
                        }
                    }
                }
            }
            match p.pending {
                Some(Pending::Launch { slot, at, velocity }) => {
                    p.pending = moved(slot).map(|slot| Pending::Launch { slot, at, velocity });
                }
                Some(Pending::Scrub {
                    slot,
                    at,
                    position,
                    repeat,
                }) => {
                    p.pending = moved(slot).map(|slot| Pending::Scrub {
                        slot,
                        at,
                        position,
                        repeat,
                    });
                }
                Some(Pending::Stop { .. }) | None => {}
            }
            p.follow = p.follow.and_then(|mut f| {
                f.slot = moved(f.slot)?;
                Some(f)
            });
            p.repeat = p.repeat.and_then(|mut r| {
                r.slot = moved(r.slot)?;
                Some(r)
            });
            p.late = p.late.and_then(|(slot, at)| Some((moved(slot)?, at)));
            if let Some(rec) = t.slot_rec.as_mut() {
                if let Some(to) = moved(rec.slot) {
                    rec.slot = to;
                }
            }
        }
        self.scene_follow = self.scene_follow.and_then(|mut f| {
            f.slot = moved(f.slot)?;
            Some(f)
        });
        self.pending_scene = self.pending_scene.and_then(|mut s| {
            s.scene = moved(s.scene)?;
            Some(s)
        });
    }
}
