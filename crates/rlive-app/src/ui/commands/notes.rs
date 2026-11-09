//! Commands of the MIDI Note Editor (manual ch. 10).

use super::{Entry::Item, MenuCommand as M, MenuState};
use crate::session::ClipRef;
use crate::ui::piano_roll::{self as roll, edit_foreground, edit_selected};
use crate::ui::{Detail, RliveApp, UiState};
use rlive_core::PPQ;
use rlive_project::note_edit::{self as edit, span_of, LaneValue};
use rlive_project::note_select;
use rlive_project::NoteData;

super::domain! {
    /// The note editor's commands.
    Notes {
        Left, Right, Up, Down, OctaveUp, OctaveDown,
        NudgeLeft, NudgeRight,
        ExtendLeft, ExtendRight, ExtendFineLeft, ExtendFineRight,
        ExtendBoundaryLeft, ExtendBoundaryRight,
        KeyTrackLeft, KeyTrackRight,
        SelectPrevious, SelectNext, AddPrevious, AddNext,
        InsertHome, InsertEnd, EnterKey, Escape, InvertSelection,
        ChopOrSplit, Join, FitToRange, FitWidth, FitHeight, Deactivate,
        Quantize, QuantizeSettings,
        ScrollUp, ScrollDown, ScrollUpFine, ScrollDownFine, ScrollLeft, ScrollRight,
        ZoomIn, ZoomOut, ZoomBack,
        VelocityUp, VelocityDown, DeviationUp, DeviationDown, ChanceUp, ChanceDown,
        Group, GroupPlayAll, GroupPlayOne, Ungroup,
        Highlight, FoldNotes, FoldScale, ToggleFilters, FocusMode,
        DuplicateTime, DeleteTime, InsertTime, CropToSelection,
    }
}

pub const EDIT_ITEMS: &[super::Entry] = &[
    Item("Invert Selection", M::Notes(Notes::InvertSelection)),
    Item("Deactivate Note(s)", M::Notes(Notes::Deactivate)),
    Item("Split/Chop Note(s)", M::Notes(Notes::ChopOrSplit)),
    Item("Join Notes", M::Notes(Notes::Join)),
    Item("Fit to Time Range", M::Notes(Notes::FitToRange)),
    Item("Quantize", M::Notes(Notes::Quantize)),
    Item("Group Notes (Play All)", M::Notes(Notes::GroupPlayAll)),
    Item("Group Notes (Play One)", M::Notes(Notes::GroupPlayOne)),
    Item("Ungroup Notes", M::Notes(Notes::Ungroup)),
    Item("Duplicate Time", M::Notes(Notes::DuplicateTime)),
    Item("Delete Time", M::Notes(Notes::DeleteTime)),
    Item("Insert Time", M::Notes(Notes::InsertTime)),
];

pub const VIEW_ITEMS: &[super::Entry] = &[
    Item("Fold to Notes", M::Notes(Notes::FoldNotes)),
    Item("Fold to Scale", M::Notes(Notes::FoldScale)),
    Item("Highlight Scales", M::Notes(Notes::Highlight)),
];

const VELOCITY_STEP: f32 = 10.0 / 127.0;
const CHANCE_STEP: f32 = 0.1;

fn range_or(state: &UiState, notes: &[NoteData]) -> Option<(u64, u64)> {
    state
        .piano
        .time
        .filter(|(a, b)| b > a)
        .or_else(|| span_of(notes, &state.piano.selected))
}

impl RliveApp {
    fn notes_available(&self) -> bool {
        self.ui.show_detail
            && self.ui.detail == Detail::Clip
            && self
                .ui
                .selected_clip
                .and_then(|r| self.session.clip_at(r))
                .is_some_and(|c| c.is_midi())
    }

    pub(super) fn notes_state(&self, s: &mut MenuState) {
        let available = self.notes_available();
        for c in Notes::ALL {
            s.enable(M::Notes(*c), available);
        }
        s.check(M::Notes(Notes::Highlight), self.ui.piano.highlight);
        s.check(M::Notes(Notes::FoldNotes), self.ui.piano.fold_notes);
        s.check(M::Notes(Notes::FoldScale), self.ui.piano.fold_scale);
        s.check(M::Notes(Notes::FocusMode), self.ui.piano.focus_mode);
        s.check(M::Notes(Notes::ToggleFilters), self.ui.piano.filters.open);
    }

    pub(super) fn notes_command(&mut self, c: Notes) {
        match c {
            Notes::Highlight => return self.ui.piano.highlight = !self.ui.piano.highlight,
            Notes::FoldNotes => return self.ui.piano.fold_notes = !self.ui.piano.fold_notes,
            Notes::FoldScale => return self.ui.piano.fold_scale = !self.ui.piano.fold_scale,
            Notes::ToggleFilters => {
                return self.ui.piano.filters.open = !self.ui.piano.filters.open
            }
            Notes::FocusMode => return self.ui.piano.focus_mode = !self.ui.piano.focus_mode,
            Notes::QuantizeSettings => {
                return self.ui.piano.show_quantize = !self.ui.piano.show_quantize
            }
            _ => {}
        }
        let Some(r) = self.ui.selected_clip else {
            return;
        };
        let Some(clip) = self.session.clip_at(r).cloned() else {
            return;
        };
        if !clip.is_midi() {
            return;
        }
        let grid = self.ui.piano.grid.max(1) as i64;
        let selected = self.ui.piano.has_selection();
        match c {
            Notes::Left | Notes::Right => {
                let by = if c == Notes::Left { -grid } else { grid };
                self.left_right(r, by);
            }
            Notes::NudgeLeft | Notes::NudgeRight => {
                let by = (PPQ / 64) as i64;
                let by = if c == Notes::NudgeLeft { -by } else { by };
                self.left_right(r, by);
            }
            Notes::Up | Notes::Down => {
                let up = c == Notes::Up;
                self.up_down(if up { 1 } else { -1 });
            }
            Notes::OctaveUp | Notes::OctaveDown => {
                let up = c == Notes::OctaveUp;
                self.up_down(if up { 12 } else { -12 });
            }
            Notes::ExtendLeft | Notes::ExtendRight => {
                let by = if c == Notes::ExtendLeft { -grid } else { grid };
                self.extend_notes(by, grid as u64);
            }
            Notes::ExtendFineLeft | Notes::ExtendFineRight => {
                let step = (PPQ / 64) as i64;
                let by = if c == Notes::ExtendFineLeft {
                    -step
                } else {
                    step
                };
                self.extend_notes(by, 1);
            }
            Notes::ExtendBoundaryLeft | Notes::ExtendBoundaryRight => {
                let forward = c == Notes::ExtendBoundaryRight;
                self.extend_to_boundary(clip.notes(), forward);
            }
            Notes::KeyTrackLeft | Notes::KeyTrackRight => {
                let forward = c == Notes::KeyTrackRight;
                self.key_track_step(clip.notes(), forward);
            }
            Notes::SelectPrevious | Notes::SelectNext => {
                let forward = c == Notes::SelectNext;
                self.select_step(clip.notes(), forward, false);
            }
            Notes::AddPrevious | Notes::AddNext => {
                let forward = c == Notes::AddNext;
                self.select_step(clip.notes(), forward, true);
            }
            Notes::InsertHome => {
                self.ui.piano.deselect(false);
                self.ui.piano.insert_tick = Some(0);
            }
            Notes::InsertEnd => {
                self.ui.piano.deselect(false);
                self.ui.piano.insert_tick = Some(clip.loop_end().max(clip.end_marker()));
            }
            Notes::EnterKey => {
                if !roll::commit_typed(&mut self.session, &mut self.ui) {
                    self.toggle_time_or_notes(clip.notes());
                }
            }
            Notes::Escape => {
                self.ui.piano.deselect(false);
                self.ui.piano.filters.clear_applied();
                self.ui.piano.lanes.focus = None;
                self.ui.piano.lanes.typed.clear();
            }
            Notes::InvertSelection => {
                let count = clip.notes().len();
                self.ui.piano.selected = note_select::inverted(count, &self.ui.piano.selected);
            }
            Notes::ChopOrSplit => self.chop_or_split(r, clip.notes()),
            Notes::Join => edit_selected(&mut self.session, &mut self.ui, "Join Notes", |n, s| {
                *s = edit::join(n, s);
            }),
            Notes::FitToRange => {
                if let Some((from, to)) = range_or(&self.ui, clip.notes()) {
                    edit_selected(
                        &mut self.session,
                        &mut self.ui,
                        "Fit to Time Range",
                        |n, s| edit::fit_to_range(n, s, from, to),
                    );
                }
            }
            Notes::FitWidth => {
                let from = clip.notes().iter().map(|n| n.start).min();
                let to = clip.notes().iter().map(|n| n.start + n.length).max();
                match (from, to) {
                    (Some(a), Some(b)) => {
                        let width = self.ui.piano.view_width.max(1.0);
                        roll::zoom_time(&mut self.ui, a, b.max(a + 1), width);
                    }
                    _ => self.ui.piano.reset_view = true,
                }
            }
            Notes::FitHeight => {
                let keys: Vec<u8> = clip.notes().iter().map(|n| n.key).collect();
                let h = self.ui.piano.view_height.max(1.0);
                roll::fit_keys(&mut self.ui, &keys, h);
            }
            Notes::Deactivate => {
                if self.ui.piano.lanes.focus.is_some() {
                    self.ui.piano.lanes.typed.push('0');
                } else {
                    edit_selected(
                        &mut self.session,
                        &mut self.ui,
                        "Deactivate Notes",
                        |n, s| edit::toggle_active(n, s),
                    );
                }
            }
            Notes::Quantize => {
                let q = self.ui.piano.quantize;
                edit_selected(&mut self.session, &mut self.ui, "Quantize", |n, s| {
                    edit::quantize(n, s, q);
                });
            }
            Notes::ScrollUp | Notes::ScrollDown | Notes::ScrollUpFine | Notes::ScrollDownFine => {
                let keys = match c {
                    Notes::ScrollUpFine | Notes::ScrollDownFine => 1.0,
                    _ => 12.0,
                };
                let dir = if matches!(c, Notes::ScrollUp | Notes::ScrollUpFine) {
                    -1.0
                } else {
                    1.0
                };
                self.ui.piano.scroll.y += dir * keys * self.ui.piano.key_h;
            }
            Notes::ScrollLeft | Notes::ScrollRight => {
                let by = self.ui.piano.view_width * 0.8;
                self.ui.piano.scroll.x += if c == Notes::ScrollLeft { -by } else { by };
            }
            Notes::ZoomIn | Notes::ZoomOut => {
                let factor = if c == Notes::ZoomIn { 1.25 } else { 0.8 };
                let ps = &mut self.ui.piano;
                let center = ps.time.map_or(ps.scroll.x + ps.view_width / 2.0, |(a, b)| {
                    ((a + b) / 2) as f32 / PPQ as f32 * ps.px_per_beat
                });
                let px = center - ps.scroll.x;
                let (ppb, scroll) = roll::zoom_about(ps.scroll.x, px, ps.px_per_beat, factor);
                ps.px_per_beat = ppb;
                ps.scroll.x = scroll;
            }
            Notes::ZoomBack => {
                if self.ui.piano.zoom_back.is_empty() {
                    self.ui
                        .clip_view
                        .fit_width(&mut self.ui.piano, &mut self.ui.sample);
                } else {
                    self.ui
                        .clip_view
                        .zoom_back(&mut self.ui.piano, &mut self.ui.sample);
                }
            }
            Notes::VelocityUp | Notes::VelocityDown => {
                let up = c == Notes::VelocityUp;
                if self.adjust_chop(r, up, false) {
                    return;
                }
                if selected {
                    let by = if up { VELOCITY_STEP } else { -VELOCITY_STEP };
                    edit_foreground(
                        &mut self.session,
                        &mut self.ui,
                        "Change Velocity",
                        |n, s| {
                            edit::change_value(n, s, LaneValue::Velocity, by);
                        },
                    );
                }
            }
            Notes::DeviationUp | Notes::DeviationDown => {
                let up = c == Notes::DeviationUp;
                if self.adjust_chop(r, up, true) {
                    return;
                }
                if selected {
                    let by = edit::VELOCITY_STEP * if up { 5.0 } else { -5.0 };
                    edit_foreground(
                        &mut self.session,
                        &mut self.ui,
                        "Velocity Deviation",
                        |n, s| edit::change_deviation(n, s, by),
                    );
                }
            }
            Notes::ChanceUp | Notes::ChanceDown => {
                if selected {
                    let by = if c == Notes::ChanceUp {
                        CHANCE_STEP
                    } else {
                        -CHANCE_STEP
                    };
                    edit_foreground(&mut self.session, &mut self.ui, "Change Chance", |n, s| {
                        edit::change_value(n, s, LaneValue::Chance, by);
                    });
                }
            }
            Notes::Group => {
                let one = self.ui.piano.last_group_one;
                edit_foreground(&mut self.session, &mut self.ui, "Group Notes", |n, s| {
                    edit::group_notes(n, s, one);
                });
            }
            Notes::GroupPlayAll | Notes::GroupPlayOne => {
                let one = c == Notes::GroupPlayOne;
                self.ui.piano.last_group_one = one;
                edit_foreground(&mut self.session, &mut self.ui, "Group Notes", |n, s| {
                    edit::group_notes(n, s, one);
                });
            }
            Notes::Ungroup => {
                edit_foreground(&mut self.session, &mut self.ui, "Ungroup Notes", |n, s| {
                    edit::ungroup(n, s);
                });
            }
            Notes::DuplicateTime | Notes::DeleteTime | Notes::InsertTime => {
                if let Some((a, b)) = range_or(&self.ui, clip.notes()) {
                    self.time_command(r, c, a, b);
                }
            }
            Notes::CropToSelection => {
                let range = range_or(&self.ui, clip.notes());
                self.session.crop_clip(r, range);
                self.ui.piano.deselect(false);
            }
            _ => {}
        }
    }

    fn time_command(&mut self, r: ClipRef, c: Notes, a: u64, b: u64) {
        let len = b - a;
        let label = match c {
            Notes::DuplicateTime => "Duplicate Time",
            Notes::DeleteTime => "Delete Time",
            _ => "Insert Time",
        };
        self.session.edit_notes(r, label, |n| match c {
            Notes::DuplicateTime => edit::duplicate_time(n, a, b),
            Notes::DeleteTime => edit::delete_time(n, a, b),
            _ => edit::insert_time(n, a, len),
        });
        let ps = &mut self.ui.piano;
        ps.selected.clear();
        ps.time = match c {
            Notes::DuplicateTime => Some((b, b + len)),
            _ => Some((a, a + len)),
        };
        ps.time_only = true;
    }

    /// Left and right: the selected notes move, or the insert marker does and any time selection
    /// collapses. Armed with Preview on and stopped, it steps the recording instead.
    fn left_right(&mut self, r: ClipRef, by: i64) {
        let (session, ui) = (&mut self.session, &mut self.ui);
        let ps = &mut ui.piano;
        if ps.has_selection() {
            edit_selected(session, ui, "Move Notes", |n, s| {
                for &i in s.iter() {
                    if let Some(note) = n.get_mut(i) {
                        note.start = (note.start as i64 + by).max(0) as u64;
                    }
                }
                *s = edit::resolve_overlaps(n, s);
            });
            return;
        }
        let at = ps.insert_tick.unwrap_or(0);
        let armed = ps.preview_on
            && !session.is_playing()
            && session.project.track(r.track()).is_some_and(|t| t.arm);
        ps.insert_tick = Some(if armed {
            let grid = ps.grid.max(1);
            if by > 0 {
                session.step_forward(r, at, grid)
            } else {
                session.step_back(r, at, grid)
            }
        } else {
            (at as i64 + by).max(0) as u64
        });
        ps.time = None;
    }

    /// Up and down: selected notes transpose. In the Chance lane, its markers change instead.
    fn up_down(&mut self, by: i32) {
        let (session, ui) = (&mut self.session, &mut self.ui);
        if ui.piano.lanes.focus == Some(LaneValue::Chance) {
            let step = by.signum() as f32 * if by.abs() == 12 { 0.01 } else { CHANCE_STEP };
            edit_foreground(session, ui, "Change Chance", |n, s| {
                edit::change_value(n, s, LaneValue::Chance, step);
            });
            return;
        }
        if ui.piano.has_selection() {
            edit_selected(session, ui, "Transpose", |n, s| {
                for &i in s.iter() {
                    if let Some(note) = n.get_mut(i) {
                        note.key = (note.key as i32 + by).clamp(0, 127) as u8;
                    }
                }
            });
        }
    }

    /// Shift and an arrow: selected notes get longer or shorter; or the time selection grows.
    fn extend_notes(&mut self, by: i64, min: u64) {
        let (session, ui) = (&mut self.session, &mut self.ui);
        if ui.piano.has_selection() {
            edit_selected(session, ui, "Change Note Length", |n, s| {
                for &i in s.iter() {
                    if let Some(note) = n.get_mut(i) {
                        note.length = (note.length as i64 + by).max(min as i64) as u64;
                    }
                }
            });
            return;
        }
        let ps = &mut ui.piano;
        let anchor = ps.insert_tick.unwrap_or(0);
        let (a, b) = ps.time.unwrap_or((anchor, anchor));
        let b = (b as i64 + by).max(a as i64) as u64;
        ps.time = Some((a, b));
    }

    fn extend_to_boundary(&mut self, notes: &[NoteData], forward: bool) {
        let ps = &mut self.ui.piano;
        let anchor = ps.insert_tick.unwrap_or(0);
        let (a, b) = ps.time.unwrap_or((anchor, anchor));
        if let Some(edge) = note_select::note_boundary(notes, b, forward) {
            ps.time = Some((a, edge.max(a)));
        }
    }

    /// Alt with left or right: the next note in the key track, or, with nothing selected, the
    /// insert marker moves to the next note boundary.
    fn key_track_step(&mut self, notes: &[NoteData], forward: bool) {
        let ps = &mut self.ui.piano;
        match ps.selected.last().copied() {
            Some(from) => {
                if let Some(next) = note_select::step_in_key(notes, from, forward) {
                    ps.selected = vec![next];
                }
            }
            None => {
                let at = ps.insert_tick.unwrap_or(0);
                if let Some(edge) = note_select::note_boundary(notes, at, forward) {
                    ps.insert_tick = Some(edge);
                    ps.time = None;
                }
            }
        }
    }

    fn select_step(&mut self, notes: &[NoteData], forward: bool, add: bool) {
        let ps = &mut self.ui.piano;
        let next = match ps.selected.last().copied() {
            Some(from) => note_select::step_in_time(notes, from, forward),
            None => note_select::nearest_note(notes, ps.insert_tick.unwrap_or(0)),
        };
        if let Some(n) = next {
            if add {
                if !ps.selected.contains(&n) {
                    ps.selected.push(n);
                }
            } else {
                ps.selected = vec![n];
            }
        }
    }

    /// Enter swaps between the time span and the notes in it (manual p. 259).
    fn toggle_time_or_notes(&mut self, notes: &[NoteData]) {
        let ps = &mut self.ui.piano;
        if ps.time_only {
            if let Some((a, b)) = ps.time {
                ps.selected = note_select::notes_in_box(notes, a, b.max(a + 1), 0, 127);
            }
            ps.time_only = false;
        } else if ps.time.is_some() {
            ps.selected.clear();
            ps.others.clear();
            ps.time_only = true;
        }
    }

    fn chop_or_split(&mut self, r: ClipRef, notes: &[NoteData]) {
        let (session, ui) = (&mut self.session, &mut self.ui);
        let ps = &mut ui.piano;
        if !ps.selected.is_empty() {
            let parts = ps
                .selected
                .first()
                .and_then(|i| notes.get(*i))
                .map_or(2, |n| edit::parts_on_grid(n, ps.grid));
            ps.chop = Some(roll::Chop {
                base: notes.to_vec(),
                picks: ps.selected.clone(),
                parts,
            });
            let grid = ps.grid;
            edit_selected(session, ui, "Chop Notes", |n, s| {
                *s = edit::chop_on_grid(n, s, grid);
            });
            return;
        }
        let points: Vec<u64> = match ps.time {
            Some((a, b)) => vec![a, b],
            None => vec![ps.insert_tick.unwrap_or(0)],
        };
        let hit = points
            .iter()
            .any(|at| !edit::notes_crossing(notes, *at).is_empty());
        if hit {
            session.edit_notes(r, "Split Notes", |n| {
                for at in &points {
                    let crossing = edit::notes_crossing(n, *at);
                    edit::split_at(n, &crossing, *at);
                }
            });
        }
    }

    /// Cmd up and down straight after a chop change the number of parts.
    fn adjust_chop(&mut self, r: ClipRef, up: bool, power: bool) -> bool {
        let Some(parts) = self.ui.piano.chop.as_ref().map(|c| c.parts) else {
            return false;
        };
        let next = edit::next_parts(parts, up, power);
        roll::apply_chop(&mut self.session, &mut self.ui, r, next);
        true
    }
}
