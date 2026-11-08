//! Replacing clip content.

use crate::clip::{AudioCursor, Playing, SESSION_VOICE};
use crate::{retire, ArrangementLane, ClipData, Engine, Garbage};
use rlive_core::TrackId;
use std::sync::Arc;

pub enum ClipCmd {
    SetClip {
        track: TrackId,
        slot: usize,
        clip: Option<Arc<ClipData>>,
    },
    /// Replace a track's Arrangement lane.
    SetLane {
        track: TrackId,
        lane: Arc<ArrangementLane>,
    },
}

impl Engine {
    pub(crate) fn handle_clip(&mut self, cmd: ClipCmd) {
        use ClipCmd as C;
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
                        match &clip {
                            Some(new) => {
                                let old = std::mem::replace(&mut p.clip, new.clone());
                                retire(&mut self.garbage, Garbage::Shared(old));
                                t.player.release_all(0, &mut t.live);
                            }
                            None => {
                                if let Some(p) = t.player.playing.take() {
                                    retire(&mut self.garbage, Garbage::Shared(p.clip));
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
                        t.player.playing = Some(Playing {
                            slot,
                            clip: new.clone(),
                            start_tick: at,
                            audio: AudioCursor::new(SESSION_VOICE),
                        });
                    }
                }
                if let Some(old) = std::mem::replace(entry, clip) {
                    retire(&mut self.garbage, Garbage::Shared(old));
                }
            }
            C::SetLane { track, lane } => {
                let tick = self.tick;
                let old = match self.tracks.iter_mut().find(|t| t.id == track) {
                    Some(t) => t.lane.swap(lane, tick, &mut t.live),
                    None => lane,
                };
                retire(&mut self.garbage, Garbage::Shared(old));
            }
        }
    }
}
