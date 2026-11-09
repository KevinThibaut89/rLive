//! Take lanes, comping and linked tracks (manual ch. 21 and p. 183).
//!
//! The rules are in [`rlive_project::takes`]; this applies them to the project, keeps the engine
//! told, and extends an edit to the tracks linked with the ones it was made on. Everything undoes
//! as one step, a linked edit included, because history commits once a frame.

use super::arrange_edit::{FadeSide, LaneSel};
use super::{ArrMove, Session};
use rlive_core::{ArrClipId, TrackId};
use rlive_project::takes::TakeStep;
use std::collections::HashSet;

impl Session {
    // ------------------------------------------------------------- linked tracks

    /// Whether a linked track's edit can go ahead: a frozen track's lanes hold still.
    fn editable(&self, track: TrackId) -> bool {
        self.project
            .track(track)
            .is_some_and(|t| !t.is_frozen() && t.can_have_take_lanes())
    }

    /// These lanes, and the same lanes of the tracks linked with them.
    pub(super) fn linked_lanes(&self, lanes: &[LaneSel]) -> Vec<LaneSel> {
        let mut out: Vec<LaneSel> = Vec::with_capacity(lanes.len());
        let mut push = |lane: LaneSel| {
            if !out.contains(&lane) {
                out.push(lane);
            }
        };
        for (track, lane) in lanes {
            for t in self.project.linked_with(*track) {
                // A linked track that has no such lane has nothing to do here.
                let exists = self
                    .project
                    .track(t)
                    .is_some_and(|tr| lane.is_none_or(|i| i < tr.take_lanes.len()));
                if exists || t == *track {
                    push((t, *lane));
                }
            }
        }
        out
    }

    /// These lanes, and the same lanes of linked tracks, for what the UI selects after an edit.
    pub fn linked_lanes_of(&self, lanes: &[LaneSel]) -> Vec<LaneSel> {
        self.linked_lanes(lanes)
    }

    /// These tracks and the ones linked with them, each once.
    pub fn linked_track_list(&self, tracks: &[TrackId]) -> Vec<TrackId> {
        let mut out = Vec::with_capacity(tracks.len());
        for t in tracks.iter().flat_map(|t| self.project.linked_with(*t)) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }

    /// The clip that matches `c`'s place on the same lane of a linked track.
    fn twin(
        &self,
        linked: TrackId,
        lane: Option<usize>,
        mut same_place: impl FnMut(&rlive_project::ArrangementClip) -> bool,
    ) -> Option<ArrClipId> {
        let clips = self.project.track(linked)?.lane(lane)?;
        clips.iter().find(|o| same_place(o)).map(|o| o.id)
    }

    /// These clips, and the clips at the same place on the same lane of linked tracks.
    pub fn linked_items(&self, items: &[(TrackId, ArrClipId)]) -> Vec<(TrackId, ArrClipId)> {
        let mut out: Vec<(TrackId, ArrClipId)> = Vec::with_capacity(items.len());
        let mut push = |item: (TrackId, ArrClipId)| {
            if !out.contains(&item) {
                out.push(item);
            }
        };
        for (track, id) in items {
            push((*track, *id));
            let Some(t) = self.project.track(*track) else {
                continue;
            };
            let (Some(c), Some(lane)) = (t.arr_clip(*id), t.lane_of(*id)) else {
                continue;
            };
            let start = c.start;
            for linked in self.project.linked_with(*track) {
                if linked == *track {
                    continue;
                }
                if let Some(twin) = self.twin(linked, lane, |o| o.start == start) {
                    push((linked, twin));
                }
            }
        }
        out
    }

    /// The fade that moves with this one on linked tracks: only one that starts at the same
    /// place on the timeline (manual p. 185).
    pub(super) fn linked_fades(
        &self,
        track: TrackId,
        id: ArrClipId,
        side: FadeSide,
    ) -> Vec<(TrackId, ArrClipId)> {
        let mut out = vec![(track, id)];
        let Some(t) = self.project.track(track) else {
            return out;
        };
        let (Some(c), Some(lane)) = (t.arr_clip(id), t.lane_of(id)) else {
            return Vec::new();
        };
        let fade_start = |o: &rlive_project::ArrangementClip| match side {
            FadeSide::In => o.start,
            FadeSide::Out => o.end().saturating_sub(o.clip.fades.fade_out),
        };
        let at = fade_start(c);
        for linked in self.project.linked_with(track) {
            if linked != track {
                if let Some(twin) = self.twin(linked, lane, |o| fade_start(o) == at) {
                    out.push((linked, twin));
                }
            }
        }
        out
    }

    /// The clips ending at the same place as this one, for a crossfade made across linked tracks.
    pub(super) fn linked_boundaries(
        &self,
        track: TrackId,
        left: ArrClipId,
    ) -> Vec<(TrackId, ArrClipId)> {
        let mut out = vec![(track, left)];
        let Some(t) = self.project.track(track) else {
            return out;
        };
        let (Some(c), Some(lane)) = (t.arr_clip(left), t.lane_of(left)) else {
            return out;
        };
        let end = c.end();
        for linked in self.project.linked_with(track) {
            if linked != track {
                if let Some(twin) = self.twin(linked, lane, |o| o.end() == end) {
                    out.push((linked, twin));
                }
            }
        }
        out
    }

    /// Adds, for each move within a track, the same move of the clip at the same place on the
    /// same lane of each linked track.
    pub(super) fn linked_moves(
        &self,
        moves: &[ArrMove],
        lanes: &[Option<usize>],
    ) -> Vec<(ArrMove, Option<usize>)> {
        let mut out: Vec<(ArrMove, Option<usize>)> = Vec::with_capacity(moves.len());
        let mut push = |m: ArrMove, lane: Option<usize>| {
            if !out.iter().any(|(o, _)| o.id == m.id && o.track == m.track) {
                out.push((m, lane));
            }
        };
        for (m, to_lane) in moves.iter().zip(lanes) {
            push(*m, *to_lane);
            let Some(t) = self.project.track(m.track) else {
                continue;
            };
            let Some(from_lane) = t.lane_of(m.id) else {
                continue;
            };
            // Between different tracks there's no telling where a linked clip should go.
            if m.track != m.to_track {
                continue;
            }
            let Some(start) = t.arr_clip(m.id).map(|c| c.start) else {
                continue;
            };
            for linked in self.project.linked_with(m.track) {
                if linked == m.track {
                    continue;
                }
                if let Some(id) = self.twin(linked, from_lane, |o| o.start == start) {
                    push(
                        ArrMove {
                            track: linked,
                            id,
                            to_track: linked,
                            start: m.start,
                        },
                        *to_lane,
                    );
                }
            }
        }
        out
    }

    /// Links these tracks as one new instance.
    pub fn link_tracks(&mut self, tracks: &[TrackId]) -> bool {
        let done = self.project.link_tracks(tracks);
        if done {
            self.mark_edit("Link Tracks");
        }
        done
    }

    /// Links the tracks inside a Group Track.
    pub fn link_group_tracks(&mut self, group: TrackId) -> bool {
        let contents = self.project.group_contents(group);
        self.link_tracks(&contents)
    }

    /// Adds these tracks to the instance `to` is in.
    pub fn add_tracks_to_link(&mut self, tracks: &[TrackId], to: TrackId) -> bool {
        let done = self.project.add_to_link(tracks, to);
        if done {
            self.mark_edit("Link Tracks");
        }
        done
    }

    /// Takes these tracks out of their instances.
    pub fn unlink_tracks(&mut self, tracks: &[TrackId]) -> bool {
        let done = self.project.unlink_tracks(tracks);
        if done {
            self.mark_edit("Unlink Tracks");
        }
        done
    }

    /// The colour for a new take on `track`: its track colour, or under Clip Color: Random, a
    /// different one for each take (manual p. 434).
    pub(super) fn take_color(&self, track: TrackId) -> [u8; 3] {
        use crate::settings::ClipColor;
        if self.colors.new_clip != ClipColor::Random {
            return self.new_clip_color(track);
        }
        let takes = self
            .project
            .track(track)
            .map_or(0, |t| t.take_lanes.iter().map(|l| l.clips.len()).sum());
        let palette = rlive_project::PALETTE;
        palette[(self.project.tracks.len() * 5 + 9 + takes * 5) % palette.len()]
    }

    // ------------------------------------------------------------- managing lanes

    /// Shows or hides take lanes. Asking leaves Automation Mode to the UI.
    pub fn set_show_take_lanes(&mut self, track: TrackId, show: bool) {
        self.edit_track_ui(super::ChainRef::Track(track), |ui| {
            ui.show_take_lanes = show;
        });
    }

    /// Adds a take lane under `after` (or at the end) on each track and those linked with it, and
    /// unfolds their lanes. Returns where each landed.
    pub fn insert_take_lane(
        &mut self,
        tracks: &[TrackId],
        after: Option<usize>,
    ) -> Vec<(TrackId, usize)> {
        let lanes: Vec<LaneSel> = tracks.iter().map(|t| (*t, None)).collect();
        let lanes = self.linked_lanes(&lanes);
        let mut made = Vec::new();
        for (track, _) in lanes {
            if !self.editable(track) {
                continue;
            }
            if let Some(t) = self.project.track_mut(track) {
                let i = t.insert_take_lane(after);
                t.ui.show_take_lanes = true;
                made.push((track, i));
            }
        }
        if !made.is_empty() {
            self.mark_edit("Insert Take Lane");
        }
        made
    }

    /// Copies a take lane to just below it.
    pub fn duplicate_take_lane(&mut self, track: TrackId, lane: usize) -> Option<usize> {
        if !self.editable(track) {
            return None;
        }
        let at = self.project.track_mut(track)?.duplicate_take_lane(lane)?;
        self.mark_edit("Duplicate Take Lane");
        Some(at)
    }

    /// Deletes a take lane on the track and any linked with it.
    pub fn delete_take_lane(&mut self, track: TrackId, lane: usize) {
        let lanes = self.linked_lanes(&[(track, Some(lane))]);
        let mut any = false;
        for (t, l) in lanes {
            if let (Some(l), true) = (l, self.editable(t)) {
                if let Some(tr) = self.project.track_mut(t) {
                    if tr.delete_take_lane(l) {
                        any = true;
                        self.sync_lane(t);
                    }
                }
            }
        }
        if any {
            self.mark_edit("Delete Take Lane");
        }
    }

    /// Removes every take lane of the track and any linked with it.
    pub fn delete_all_take_lanes(&mut self, track: TrackId) {
        let mut any = false;
        for t in self.project.linked_with(track) {
            if !self.editable(t) {
                continue;
            }
            if let Some(tr) = self.project.track_mut(t) {
                any |= tr.delete_all_take_lanes();
                self.sync_lane(t);
            }
        }
        if any {
            self.mark_edit("Delete All Take Lanes");
        }
    }

    /// Removes the take lanes with nothing in a comp on the track.
    pub fn delete_unused_take_lanes(&mut self, track: TrackId) {
        if !self.editable(track) {
            return;
        }
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        let removed = t.delete_unused_take_lanes();
        if removed > 0 {
            self.sync_lane(track);
            self.mark_edit("Delete Unused Take Lanes");
        }
    }

    /// Names a take lane on the track and any linked with it that has one there.
    pub fn rename_take_lane(&mut self, track: TrackId, lane: usize, name: String) {
        let lanes = self.linked_lanes(&[(track, Some(lane))]);
        let mut any = false;
        for (t, l) in lanes {
            let Some(tr) = self.project.track_mut(t) else {
                continue;
            };
            if let Some(tl) = l.and_then(|l| tr.take_lanes.get_mut(l)) {
                if tl.name != name {
                    tl.name = name.clone();
                    any = true;
                }
            }
        }
        if any {
            self.mark_edit("Rename Take Lane");
        }
    }

    /// Moves a take lane up or down within its track.
    pub fn move_take_lane(&mut self, track: TrackId, from: usize, to: usize) -> bool {
        let moved = self
            .project
            .track_mut(track)
            .is_some_and(|t| t.move_take_lane(from, to));
        if moved {
            self.sync_lane(track);
            self.mark_edit("Move Take Lane");
        }
        moved
    }

    // ------------------------------------------------------------- audition

    /// Whether a take lane is what the track is playing.
    pub fn audition_of(&self, track: TrackId) -> Option<usize> {
        self.project.track(track)?.auditioned_lane()
    }

    /// Auditions a take lane, on linked tracks too, or none. One lane per track.
    pub fn set_audition(&mut self, track: TrackId, lane: Option<usize>) {
        for t in self.project.linked_with(track) {
            let Some(tr) = self.project.track(t) else {
                continue;
            };
            // A linked track without the lane stays on its main lane.
            let lane = lane.filter(|i| *i < tr.take_lanes.len());
            if tr.ui.audition_lane == lane {
                continue;
            }
            self.edit_track_ui(super::ChainRef::Track(t), |ui| ui.audition_lane = lane);
            self.sync_lane(t);
        }
    }

    /// The Audition button: on, or off if this lane is already being auditioned.
    pub fn toggle_audition(&mut self, track: TrackId, lane: usize) {
        let on = self.project.track(track).and_then(|t| t.ui.audition_lane) == Some(lane);
        self.set_audition(track, (!on).then_some(lane));
    }

    // ------------------------------------------------------------- comping

    /// Copies a selection on take lanes to their main lanes (Enter, or a Draw Mode gesture). The
    /// last lane on each track wins, matching audition's choice. Linked tracks follow.
    pub fn comp_selection(&mut self, lanes: &[(TrackId, usize)], start: u64, end: u64) -> bool {
        let selected: Vec<LaneSel> = lanes.iter().map(|(t, l)| (*t, Some(*l))).collect();
        let all = self.linked_lanes(&selected);
        let mut touched = HashSet::new();
        for (track, lane) in all {
            let Some(lane) = lane.filter(|_| self.editable(track)) else {
                continue;
            };
            if let Some(t) = self.project.track_mut(track) {
                if !t.comp_from_lane(lane, start, end).is_empty() {
                    touched.insert(track);
                }
            }
        }
        let any = !touched.is_empty();
        for t in touched {
            self.sync_lane(t);
        }
        if any {
            self.mark_edit("Copy Selection to Main Lane");
        }
        any
    }

    /// Replaces the main lane's `[start, end)` with the next or previous take. `from` is the
    /// take lane the selection is on; `None` works it out from the comp. Returns the new lane.
    pub fn step_take(
        &mut self,
        track: TrackId,
        from: Option<usize>,
        start: u64,
        end: u64,
        step: TakeStep,
    ) -> Option<usize> {
        if !self.editable(track) {
            return None;
        }
        let made = self
            .project
            .track_mut(track)?
            .step_take(from, start, end, step)?;
        let mut touched = vec![track];
        // Linked tracks follow to the same lane, if they have material on it.
        for t in self.project.linked_with(track) {
            if t == track || !self.editable(t) {
                continue;
            }
            if let Some(tr) = self.project.track_mut(t) {
                if tr.lane_has_material(made, start, end) {
                    tr.delete_span(start, end);
                    tr.comp_from_lane(made, start, end);
                    touched.push(t);
                }
            }
        }
        for t in &touched {
            self.sync_lane(*t);
        }
        self.mark_edit(match step {
            TakeStep::Next => "Next Take",
            TakeStep::Previous => "Previous Take",
        });
        Some(made)
    }

    /// Replaces each selected main-lane clip with the next or previous take there.
    pub fn step_take_clips(&mut self, items: &[(TrackId, ArrClipId)], step: TakeStep) -> bool {
        let mut any = false;
        for (track, id) in items {
            let span = self
                .project
                .track(*track)
                .and_then(|t| t.arr_clip(*id))
                .map(|c| (c.start, c.end()));
            if let Some((a, b)) = span {
                any |= self.step_take(*track, None, a, b, step).is_some();
            }
        }
        any
    }

    /// Moves the boundary between two comp parts, on linked tracks too. Returns where it went.
    pub fn move_comp_split(
        &mut self,
        track: TrackId,
        left: ArrClipId,
        right: ArrClipId,
        to: u64,
    ) -> Option<u64> {
        let boundary = self
            .project
            .track(track)
            .and_then(|t| t.arr_clip(left))
            .map(|c| c.end())?;
        let mut at = None;
        for t in self.project.linked_with(track) {
            let pair = if t == track {
                Some((left, right))
            } else {
                self.project.track(t).and_then(|tr| {
                    let l = tr.arrangement.iter().find(|c| c.end() == boundary)?;
                    let r = tr.arrangement.iter().find(|c| c.start == boundary)?;
                    Some((l.id, r.id))
                })
            };
            let Some((l, r)) = pair else {
                continue;
            };
            let Some(tr) = self.project.track_mut(t) else {
                continue;
            };
            if let Some(got) = tr.move_comp_split(l, r, to) {
                self.sync_lane(t);
                if t == track {
                    at = Some(got);
                }
            }
        }
        if at.is_some() {
            self.mark_edit("Adjust Comp Split");
        }
        at
    }

    /// Lays a clip on a take lane, over what it overlaps there.
    pub fn take_place(
        &mut self,
        track: TrackId,
        lane: usize,
        clip: rlive_project::ArrangementClip,
    ) -> bool {
        let Some(t) = self.project.track_mut(track) else {
            return false;
        };
        if !t.place_on_lane(Some(lane), clip) {
            return false;
        }
        t.settle_fades();
        self.sync_lane(track);
        self.mark_edit("Add Clip");
        true
    }

    /// Pastes clips with the earliest at `at`, on take lane `lane` of their own tracks.
    pub fn take_paste(
        &mut self,
        clips: &[(TrackId, rlive_project::ArrangementClip)],
        at: u64,
        lane: usize,
    ) -> Vec<(TrackId, ArrClipId)> {
        let mut placed = Vec::new();
        for (track, c) in clips {
            let copy = rlive_project::ArrangementClip {
                id: ArrClipId::new(),
                start: c.start + at,
                ..c.clone()
            };
            let id = copy.id;
            if self.take_place(*track, lane, copy) {
                placed.push((*track, id));
            }
        }
        placed
    }
}
