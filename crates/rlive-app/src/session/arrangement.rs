//! Editing the timeline.

use super::Session;
use rlive_core::{ArrClipId, TrackId};
use rlive_engine::{ClipCmd, EngineCommand, LaunchCmd, TransportCmd};
use rlive_project::ArrangementClip;
use std::collections::HashSet;

/// One clip's destination when moving or copying on the timeline.
#[derive(Debug, Clone, Copy)]
pub struct ArrMove {
    pub track: TrackId,
    pub id: ArrClipId,
    pub to_track: TrackId,
    pub start: u64,
}

/// Which edge of a timeline clip to move, and where to.
#[derive(Debug, Clone, Copy)]
pub enum TrimEdge {
    Left(u64),
    Right(u64),
}

impl Session {
    // ------------------------------------------------------------- arrangement

    /// Where playback starts and returns to, in ticks.
    pub fn insert_marker(&self) -> f64 {
        self.engine.shared.insert_marker.load()
    }

    pub fn seek(&mut self, tick: f64) {
        self.send(EngineCommand::Transport(TransportCmd::Seek(tick.max(0.0))));
    }

    pub fn set_loop(&mut self, enabled: bool, start: u64, end: u64) {
        let a = &mut self.project.arrangement;
        if (a.loop_enabled, a.loop_start, a.loop_end) == (enabled, start, end) {
            return;
        }
        a.loop_enabled = enabled;
        a.loop_start = start;
        a.loop_end = end;
        self.sync_loop();
        self.mark_edit("Change Loop");
    }

    pub fn back_to_arrangement(&mut self) {
        self.send(EngineCommand::Launch(LaunchCmd::BackToArrangement));
    }

    fn sync_lanes(&mut self, tracks: HashSet<TrackId>, label: &'static str) {
        for t in tracks {
            self.sync_lane(t);
        }
        self.mark_edit(label);
    }

    pub fn arr_place(&mut self, track: TrackId, clip: ArrangementClip) {
        if let Some(t) = self.project.track_mut(track) {
            t.place_clip(clip);
            self.sync_lane(track);
            self.mark_edit("Add Clip");
        }
    }

    /// Moves or copies clips; returns where they ended up.
    pub fn arr_move(&mut self, moves: &[ArrMove], copy: bool) -> Vec<(TrackId, ArrClipId)> {
        // Lift everything first so clips can't overwrite each other's old places.
        let lifted: Vec<(ArrMove, ArrangementClip)> = moves
            .iter()
            .filter_map(|m| {
                let t = self.project.track_mut(m.track)?;
                let c = if copy {
                    t.arr_clip(m.id).cloned()
                } else {
                    t.remove_arr_clip(m.id)
                }?;
                Some((*m, c))
            })
            .collect();
        let mut placed = Vec::with_capacity(lifted.len());
        let mut touched = HashSet::new();
        for (m, mut c) in lifted {
            c.start = m.start;
            if copy {
                c.id = ArrClipId::new();
            }
            placed.push((m.to_track, c.id));
            touched.insert(m.track);
            touched.insert(m.to_track);
            if let Some(t) = self.project.track_mut(m.to_track) {
                t.place_clip(c);
            }
        }
        self.sync_lanes(touched, if copy { "Copy Clips" } else { "Move Clips" });
        placed
    }

    pub fn arr_trim(&mut self, track: TrackId, id: ArrClipId, edge: TrimEdge) {
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        let Some(mut c) = t.remove_arr_clip(id) else {
            return;
        };
        match edge {
            TrimEdge::Left(to) => c.trim_left(to),
            TrimEdge::Right(to) => c.trim_right(to),
        }
        t.place_clip(c);
        self.sync_lane(track);
        self.mark_edit("Trim Clip");
    }

    pub fn arr_delete(&mut self, items: &[(TrackId, ArrClipId)]) {
        let mut touched = HashSet::new();
        for (track, id) in items {
            if let Some(t) = self.project.track_mut(*track) {
                if t.remove_arr_clip(*id).is_some() {
                    touched.insert(*track);
                }
            }
        }
        if !touched.is_empty() {
            self.sync_lanes(touched, "Delete Clips");
        }
    }

    /// Copies the clips just after the span they cover.
    pub fn arr_duplicate(&mut self, items: &[(TrackId, ArrClipId)]) -> Vec<(TrackId, ArrClipId)> {
        let clips: Vec<(TrackId, ArrangementClip)> = items
            .iter()
            .filter_map(|(t, id)| Some((*t, self.project.track(*t)?.arr_clip(*id)?.clone())))
            .collect();
        let (Some(first), Some(last)) = (
            clips.iter().map(|(_, c)| c.start).min(),
            clips.iter().map(|(_, c)| c.end()).max(),
        ) else {
            return Vec::new();
        };
        let moves: Vec<ArrMove> = clips
            .iter()
            .map(|(t, c)| ArrMove {
                track: *t,
                id: c.id,
                to_track: *t,
                start: c.start + (last - first),
            })
            .collect();
        self.arr_move(&moves, true)
    }

    /// Places clipboard clips with the earliest at `at`, on their original tracks.
    pub fn arr_paste(
        &mut self,
        clips: &[(TrackId, ArrangementClip)],
        at: u64,
    ) -> Vec<(TrackId, ArrClipId)> {
        let mut placed = Vec::with_capacity(clips.len());
        let mut touched = HashSet::new();
        for (track, c) in clips {
            let Some(t) = self.project.track_mut(*track) else {
                continue;
            };
            let id = ArrClipId::new();
            t.place_clip(ArrangementClip {
                id,
                start: c.start + at,
                ..c.clone()
            });
            placed.push((*track, id));
            touched.insert(*track);
        }
        if !touched.is_empty() {
            self.sync_lanes(touched, "Paste Clips");
        }
        placed
    }

    /// Sends a track's timeline to the engine.
    pub fn sync_lane(&mut self, track: TrackId) {
        let Some(clips) = self.project.track(track).map(|t| t.arrangement.clone()) else {
            return;
        };
        let (mut b, _) = self.builder();
        let lane = b.lane(&clips);
        self.send(EngineCommand::Clip(ClipCmd::SetLane { track, lane }));
    }

    /// Sends the loop range to the engine.
    pub(super) fn sync_loop(&mut self) {
        let a = &self.project.arrangement;
        let range = a
            .loop_enabled
            .then_some((a.loop_start as f64, a.loop_end as f64));
        self.send(EngineCommand::Transport(TransportCmd::SetLoop(range)));
    }
}
