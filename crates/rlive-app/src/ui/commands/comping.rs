//! Commands of take lanes and comping (manual ch. 21).

use super::edit::Edit;
use crate::session::{ChainRef, LaneSel};
use crate::ui::{FocusArea, MainView, RliveApp};
use rlive_core::TrackId;
use rlive_project::takes::TakeStep;

super::domain! {
    /// The take lanes and the comp being built from them.
    Comp {
        ToggleShow,
        InsertLane,
        CopyToMain,
        Audition,
        DuplicateLane,
        DeleteLane,
        RenameLane,
        NextTake,
        PreviousTake,
        MoveLaneUp,
        MoveLaneDown,
        FoldToMain,
    }
}

impl RliveApp {
    /// Whether a take lane or material on one has the keyboard.
    pub(in crate::ui) fn take_lane_active(&self) -> bool {
        self.ui.focus == FocusArea::Arrangement && !self.selected_take_lanes().is_empty()
    }

    /// Whether Next and Previous Take have something to work on.
    pub(in crate::ui) fn take_area_active(&self) -> bool {
        let arr = &self.ui.arr;
        if self.ui.focus != FocusArea::Arrangement
            || arr.take_header.is_some()
            || arr.loop_selected
            || !arr.has_selection()
        {
            return false;
        }
        self.ui
            .arr
            .selected_lanes(&self.session)
            .iter()
            .any(|(t, _)| {
                self.session
                    .project
                    .track(*t)
                    .is_some_and(|t| !t.take_lanes.is_empty())
            })
    }

    /// The take lanes selected: by header, by a time selection, or by clips on them.
    fn selected_take_lanes(&self) -> Vec<(TrackId, usize)> {
        if let Some(h) = self.ui.arr.take_header {
            return vec![h];
        }
        // An insert marker on a lane isn't material to act on.
        if !self.ui.arr.has_selection() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (t, lane) in self.ui.arr.selected_lanes(&self.session) {
            if let Some(lane) = lane {
                if !out.contains(&(t, lane)) {
                    out.push((t, lane));
                }
            }
        }
        out
    }

    /// The span of the take lanes selected: the time selection, or the header's lane as a whole.
    fn take_span(&self, lanes: &[(TrackId, usize)]) -> Option<(u64, u64)> {
        if let Some(span) = self
            .ui
            .arr
            .selected_span(&self.session)
            .filter(|(a, b)| b > a)
        {
            return Some(span);
        }
        let (track, lane) = *lanes.first()?;
        let clips = self.session.project.track(track)?.lane(Some(lane))?;
        let first = clips.first()?;
        let last = clips.last()?;
        Some((first.start, last.end()))
    }

    /// One track from each instance of linked tracks, for commands that would otherwise run
    /// once per track of an instance.
    fn one_per_link(&self, lanes: Vec<LaneSel>) -> Vec<LaneSel> {
        let mut out: Vec<LaneSel> = Vec::new();
        let mut seen: Vec<TrackId> = Vec::new();
        for (t, l) in lanes {
            if seen.contains(&t) {
                continue;
            }
            seen.extend(self.session.project.linked_with(t));
            out.push((t, l));
        }
        out
    }

    fn set_take_selection(&mut self, track: TrackId, lane: usize) {
        if let Some(t) = self.ui.arr.time.as_mut() {
            for k in &mut t.lanes {
                if k.chain == ChainRef::Track(track) {
                    k.take = Some(lane);
                }
            }
        }
    }

    fn step_selected_takes(&mut self, step: TakeStep) {
        let lanes = self.one_per_link(self.ui.arr.selected_lanes(&self.session));
        let span = self
            .ui
            .arr
            .time
            .as_ref()
            .filter(|t| !t.is_point())
            .map(|t| t.ticks());
        if let Some((a, b)) = span {
            for (track, lane) in lanes {
                let moved = self.session.step_take(track, lane, a, b, step);
                if let (Some(target), Some(_)) = (moved, lane) {
                    // A selection on a take lane goes with it.
                    for t in self.session.project.linked_with(track) {
                        self.set_take_selection(t, target);
                    }
                }
            }
            return;
        }
        // Clip headers selected: each clip's span steps.
        let items: Vec<_> = self.ui.arr.selection.clone();
        let mut seen = Vec::new();
        for (track, id) in items {
            if seen.contains(&track) {
                continue;
            }
            let Some(t) = self.session.project.track(track) else {
                continue;
            };
            let (Some(lane), Some(c)) = (t.lane_of(id), t.arr_clip(id)) else {
                continue;
            };
            let (a, b) = (c.start, c.end());
            seen.extend(self.session.project.linked_with(track));
            self.session.step_take(track, lane, a, b, step);
        }
    }

    fn insert_take_lane_on_selection(&mut self) {
        self.ui.arr.auto.mode = false;
        self.ui.main_view = MainView::Arrangement;
        if let Some((track, lane)) = self.ui.arr.take_header {
            self.session.insert_take_lane(&[track], Some(lane));
            return;
        }
        let tracks = self.selected_track_ids();
        self.session.insert_take_lane(&tracks, None);
    }

    fn toggle_take_lanes_on_selection(&mut self) {
        self.ui.arr.auto.mode = false;
        let tracks = self.selected_track_ids();
        let on = !tracks
            .first()
            .and_then(|t| self.session.project.track(*t))
            .is_some_and(|t| t.ui.show_take_lanes);
        for t in tracks {
            self.session.set_show_take_lanes(t, on);
        }
    }

    fn audition_selected(&mut self) {
        // The last lane selected on each track, per instance of linked tracks.
        let mut chosen: Vec<LaneSel> = Vec::new();
        for (t, lane) in self.selected_take_lanes() {
            match chosen.iter().position(|(o, _)| *o == t) {
                Some(i) => chosen[i].1 = Some(lane),
                None => chosen.push((t, Some(lane))),
            }
        }
        let chosen = self.one_per_link(chosen);
        let all_on = chosen
            .iter()
            .all(|(t, lane)| lane.is_some() && self.session.audition_of(*t) == *lane);
        for (t, lane) in chosen {
            self.session
                .set_audition(t, if all_on { None } else { lane });
        }
    }

    fn step_lane_position(&mut self, delta: i32) {
        let Some((track, lane)) = self.ui.arr.take_header else {
            return;
        };
        let to = lane as i32 + delta;
        if to < 0 {
            return;
        }
        if self.session.move_take_lane(track, lane, to as usize) {
            self.ui.arr.take_header = Some((track, to as usize));
        }
    }

    pub(super) fn comp_command(&mut self, c: Comp) {
        match c {
            Comp::ToggleShow => self.toggle_take_lanes_on_selection(),
            Comp::InsertLane => self.insert_take_lane_on_selection(),
            Comp::CopyToMain => {
                let lanes = self.selected_take_lanes();
                if let Some((a, b)) = self.take_span(&lanes) {
                    self.session.comp_selection(&lanes, a, b);
                }
            }
            Comp::Audition => self.audition_selected(),
            Comp::DuplicateLane => {
                if let Some((track, lane)) = self.ui.arr.take_header {
                    if let Some(at) = self.session.duplicate_take_lane(track, lane) {
                        self.ui.arr.take_header = Some((track, at));
                    }
                }
            }
            Comp::DeleteLane => self.delete_take_header(),
            Comp::RenameLane => {
                self.rename_take_header();
            }
            Comp::NextTake => self.step_selected_takes(TakeStep::Next),
            Comp::PreviousTake => self.step_selected_takes(TakeStep::Previous),
            Comp::MoveLaneUp => self.step_lane_position(-1),
            Comp::MoveLaneDown => self.step_lane_position(1),
            Comp::FoldToMain => {
                if let Some((track, _)) = self.ui.arr.take_header.take() {
                    self.session.set_show_take_lanes(track, false);
                    self.ui.selected = Some(ChainRef::Track(track));
                }
            }
        }
    }

    fn delete_take_header(&mut self) {
        if let Some((track, lane)) = self.ui.arr.take_header.take() {
            self.session.delete_take_lane(track, lane);
            let left = self
                .session
                .project
                .track(track)
                .map_or(0, |t| t.take_lanes.len());
            if left > 0 {
                self.ui.arr.take_header = Some((track, lane.min(left - 1)));
            }
        }
    }

    /// Cmd+R with a take lane's header selected.
    pub(super) fn rename_take_header(&mut self) -> bool {
        let Some((track, lane)) = self.ui.arr.take_header else {
            return false;
        };
        let name = self
            .session
            .project
            .track(track)
            .and_then(|t| t.take_lanes.get(lane))
            .map_or_else(String::new, |l| l.name.clone());
        self.ui.arr.renaming_take = Some((track, lane, name));
        true
    }

    /// The Edit commands' effect on a selected take lane header.
    pub(super) fn take_header_edit(&mut self, c: Edit) -> bool {
        if self.ui.arr.take_header.is_none() {
            return false;
        }
        match c {
            Edit::Duplicate => self.comp_command(Comp::DuplicateLane),
            Edit::Delete => self.comp_command(Comp::DeleteLane),
            _ => return false,
        }
        true
    }

    /// Alt +/- with a take lane's header selected.
    pub(super) fn resize_take_header(&mut self, by: f32) -> bool {
        let Some((track, lane)) = self.ui.arr.take_header else {
            return false;
        };
        crate::ui::arrangement::takes::resize_lane(&mut self.session, track, lane, by);
        true
    }
}
