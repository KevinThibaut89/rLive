//! Editing the timeline.

use super::Session;
use rlive_core::{ArrClipId, TrackId};
use rlive_engine::{ClipCmd, EngineCommand, LaunchCmd, TransportCmd};
use rlive_project::{ArrangementClip, Clip, Track, TrackKind};
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

/// A clip landing on `track`, which only frozen tracks play frozen. A frozen clip dragged to an
/// audio track becomes audio (manual p. 986), MIDI clips included.
pub(super) fn leaving_frozen_track(clip: Clip, track: &Track) -> Clip {
    if track.is_frozen() || clip.frozen.is_none() {
        return clip;
    }
    if track.kind == TrackKind::Audio {
        return super::freeze::flatten_clip(&clip).unwrap_or(clip);
    }
    Clip {
        frozen: None,
        frozen_tail: false,
        ..clip
    }
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
            if let Some(track) = self.project.track_mut(t) {
                track.settle_fades();
            }
            self.sync_lane(t);
        }
        self.mark_edit(label);
    }

    pub fn arr_place(&mut self, track: TrackId, clip: ArrangementClip) {
        if let Some(t) = self.project.track_mut(track) {
            t.place_clip(clip);
            t.settle_fades();
            self.sync_lane(track);
            self.mark_edit("Add Clip");
        }
    }

    /// Moves or copies clips, each staying on the lane it's on; returns where they ended up.
    pub fn arr_move(&mut self, moves: &[ArrMove], copy: bool) -> Vec<(TrackId, ArrClipId)> {
        let lanes: Vec<Option<usize>> = moves
            .iter()
            .map(|m| {
                let lane = self.project.track(m.track).and_then(|t| t.lane_of(m.id));
                (m.track == m.to_track).then(|| lane.flatten()).flatten()
            })
            .collect();
        self.arr_move_to_lanes(moves, &lanes, copy)
    }

    /// Moves or copies clips onto the lane given for each; returns where they ended up.
    pub fn arr_move_to_lanes(
        &mut self,
        moves: &[ArrMove],
        lanes: &[Option<usize>],
        copy: bool,
    ) -> Vec<(TrackId, ArrClipId)> {
        let all = self.linked_moves(moves, lanes);
        // Lift everything first so clips can't overwrite each other's old places.
        let lifted: Vec<(ArrMove, Option<usize>, bool, ArrangementClip)> = all
            .into_iter()
            .filter_map(|(m, to_lane)| {
                let t = self.project.track_mut(m.track)?;
                let main = t.lane_of(m.id)?.is_none();
                let c = if copy {
                    t.arr_clip(m.id).cloned()
                } else {
                    t.remove_arr_clip(m.id)
                }?;
                Some((m, to_lane, main, c))
            })
            .collect();
        // Envelopes go with clips on the lane that plays.
        let spans: Vec<(ArrMove, f64, f64)> = lifted
            .iter()
            .filter(|(.., main, _)| *main)
            .map(|(m, .., c)| (*m, c.start as f64, c.end() as f64))
            .collect();
        let mut placed = Vec::with_capacity(lifted.len());
        let mut touched = HashSet::new();
        for (m, to_lane, _, mut c) in lifted {
            c.start = m.start;
            if copy {
                c.id = ArrClipId::new();
            }
            placed.push((m.to_track, c.id));
            touched.insert(m.track);
            touched.insert(m.to_track);
            if let Some(t) = self.project.track_mut(m.to_track) {
                c.clip = leaving_frozen_track(c.clip, t);
                let lane = to_lane.filter(|i| *i < t.take_lanes.len());
                t.place_on_lane(lane, c);
            }
        }
        self.carry_automation(&spans, copy);
        self.sync_lanes(touched, if copy { "Copy Clips" } else { "Move Clips" });
        placed
    }

    /// Trims a clip, and in step with it the same clips on linked tracks.
    pub fn arr_trim(&mut self, track: TrackId, id: ArrClipId, edge: TrimEdge) {
        let mut touched = HashSet::new();
        for (track, id) in self.linked_items(&[(track, id)]) {
            let Some(t) = self.project.track_mut(track) else {
                continue;
            };
            let Some(lane) = t.lane_of(id) else {
                continue;
            };
            let Some(mut c) = t.remove_arr_clip(id) else {
                continue;
            };
            match edge {
                TrimEdge::Left(to) => c.trim_left(to),
                TrimEdge::Right(to) => c.trim_right(to),
            }
            t.place_on_lane(lane, c);
            touched.insert(track);
        }
        if !touched.is_empty() {
            self.sync_lanes(touched, "Trim Clip");
        }
    }

    pub fn arr_delete(&mut self, items: &[(TrackId, ArrClipId)]) {
        let items = self.linked_items(items);
        let mut touched = HashSet::new();
        for (track, id) in &items {
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
            let mut copy = ArrangementClip {
                id,
                start: c.start + at,
                ..c.clone()
            };
            copy.clip = leaving_frozen_track(copy.clip, t);
            t.place_clip(copy);
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
        // An auditioned take lane plays in the main lane's place.
        let Some(clips) = self.project.track(track).map(|t| t.playing_lane().to_vec()) else {
            return;
        };
        let frozen = self.is_frozen(track);
        let (mut b, _) = self.builder();
        b.frozen = frozen;
        let lane = b.lane(&clips);
        self.send(EngineCommand::Clip(ClipCmd::SetLane { track, lane }));
        self.sync_clip_routes(track);
    }

    /// Sends the loop range to the engine.
    pub(super) fn sync_loop(&mut self) {
        let a = &self.project.arrangement;
        let range = a
            .loop_enabled
            .then_some((a.loop_start as f64, a.loop_end as f64));
        self.send(EngineCommand::Transport(TransportCmd::SetLoop(range)));
        self.send_punch();
    }
}
