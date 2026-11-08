//! Undo, the clipboard and the other Edit commands.

use super::{Entry, Entry::Item, Entry::Separator, FocusArea, MenuCommand as M, MenuState};
use crate::session::{ChainRef, ClipRef};
use crate::ui::automation::{self, EnvelopeClipboard};
use crate::ui::{piano_roll, session_view, Detail, MainView, RliveApp};
use rlive_core::TrackId;
use rlive_project::{ArrangementClip, Clip, NoteData, RecordQuantize, TrackKind};

super::domain! {
    /// The Edit menu.
    Edit {
        Undo, Redo, Cut, Copy, Paste, Duplicate, Delete, SelectAll, Rename, ReturnToDefault, DeleteEnvelope,
        RecordQuantizeOff, RecordQuantizeQuarter, RecordQuantizeEighth, RecordQuantizeEighthTriplet,
        RecordQuantizeEighthPlus, RecordQuantizeSixteenth, RecordQuantizeSixteenthTriplet,
        RecordQuantizeSixteenthPlus, RecordQuantizeThirtySecond,
    }
}

pub const MENU: &[Entry] = &[
    Item("Undo", M::Edit(Edit::Undo)),
    Item("Redo", M::Edit(Edit::Redo)),
    Separator,
    Item("Cut", M::Edit(Edit::Cut)),
    Item("Copy", M::Edit(Edit::Copy)),
    Item("Paste", M::Edit(Edit::Paste)),
    Item("Duplicate", M::Edit(Edit::Duplicate)),
    Item("Delete", M::Edit(Edit::Delete)),
    Item("Rename", M::Edit(Edit::Rename)),
    Item("Return to Default", M::Edit(Edit::ReturnToDefault)),
    Separator,
    Item("Select All", M::Edit(Edit::SelectAll)),
    Separator,
    Item("Record Quantize: None", M::Edit(Edit::RecordQuantizeOff)),
    Item("Record Quantize: 1/4", M::Edit(Edit::RecordQuantizeQuarter)),
    Item("Record Quantize: 1/8", M::Edit(Edit::RecordQuantizeEighth)),
    Item(
        "Record Quantize: 1/8 Triplet",
        M::Edit(Edit::RecordQuantizeEighthTriplet),
    ),
    Item(
        "Record Quantize: 1/8 + Triplet",
        M::Edit(Edit::RecordQuantizeEighthPlus),
    ),
    Item(
        "Record Quantize: 1/16",
        M::Edit(Edit::RecordQuantizeSixteenth),
    ),
    Item(
        "Record Quantize: 1/16 Triplet",
        M::Edit(Edit::RecordQuantizeSixteenthTriplet),
    ),
    Item(
        "Record Quantize: 1/16 + Triplet",
        M::Edit(Edit::RecordQuantizeSixteenthPlus),
    ),
    Item(
        "Record Quantize: 1/32",
        M::Edit(Edit::RecordQuantizeThirtySecond),
    ),
];

/// The Record Quantization menu items and what they set.
const RECORD_QUANTIZE: [(Edit, RecordQuantize); 9] = [
    (Edit::RecordQuantizeOff, RecordQuantize::Off),
    (Edit::RecordQuantizeQuarter, RecordQuantize::Quarter),
    (Edit::RecordQuantizeEighth, RecordQuantize::Eighth),
    (
        Edit::RecordQuantizeEighthTriplet,
        RecordQuantize::EighthTriplet,
    ),
    (
        Edit::RecordQuantizeEighthPlus,
        RecordQuantize::EighthPlusTriplet,
    ),
    (Edit::RecordQuantizeSixteenth, RecordQuantize::Sixteenth),
    (
        Edit::RecordQuantizeSixteenthTriplet,
        RecordQuantize::SixteenthTriplet,
    ),
    (
        Edit::RecordQuantizeSixteenthPlus,
        RecordQuantize::SixteenthPlusTriplet,
    ),
    (
        Edit::RecordQuantizeThirtySecond,
        RecordQuantize::ThirtySecond,
    ),
];

/// The unadorned title of an item whose title otherwise changes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn plain_title(c: Edit) -> Option<&'static str> {
    match c {
        Edit::Undo => Some("Undo"),
        Edit::Redo => Some("Redo"),
        _ => None,
    }
}

/// Which part of the app Edit commands act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditFocus {
    Text,
    Notes,
    ArrClips,
    SessionClip,
    /// A time selection in the automation lanes.
    Envelopes,
    None,
}

/// Internal clipboard for musical content.
#[derive(Clone)]
pub enum ClipBoard {
    Notes(Vec<NoteData>),
    Slot(Box<Clip>),
    /// Clips with their tracks, their starts relative to the earliest.
    ArrClips(Vec<(TrackId, ArrangementClip)>),
    /// Stretches of envelope, which can be pasted onto other parameters.
    Envelopes(EnvelopeClipboard),
}

/// Resolves focus to an Edit target; pure so the precedence can be tested.
fn resolve_focus(
    text_focused: bool,
    area: FocusArea,
    notes_open: bool,
    slot_selected: bool,
) -> EditFocus {
    if text_focused {
        EditFocus::Text
    } else if area == FocusArea::Clip && notes_open {
        EditFocus::Notes
    } else if area == FocusArea::Arrangement {
        EditFocus::ArrClips
    } else if slot_selected {
        EditFocus::SessionClip
    } else if notes_open {
        EditFocus::Notes
    } else {
        EditFocus::None
    }
}

impl RliveApp {
    fn notes_open(&self) -> bool {
        self.ui.show_detail
            && self.ui.detail == Detail::Clip
            && self
                .ui
                .selected_clip
                .and_then(|r| self.session.clip_at(r))
                .is_some_and(|c| c.is_midi())
    }

    /// Whether Edit commands in the Arrangement act on the automation lanes.
    fn lanes_have_selection(&self) -> bool {
        let auto = &self.ui.arr.auto;
        auto.mode
            && (auto.sel.is_some()
                || (auto.focus.is_some()
                    && matches!(self.ui.clipboard, Some(ClipBoard::Envelopes(_)))))
    }

    pub(in crate::ui) fn edit_focus(&self, ctx: &egui::Context) -> EditFocus {
        // Clicking away from an editor doesn't change what Edit commands act on.
        let area = match self.ui.focus {
            FocusArea::ControlBar | FocusArea::Device | FocusArea::Browser => {
                match self.ui.main_view {
                    MainView::Arrangement => FocusArea::Arrangement,
                    MainView::Session => FocusArea::Session,
                }
            }
            other => other,
        };
        if !ctx.text_edit_focused() && area == FocusArea::Arrangement && self.lanes_have_selection()
        {
            return EditFocus::Envelopes;
        }
        resolve_focus(
            ctx.text_edit_focused(),
            area,
            self.notes_open(),
            self.ui.selected_clip.and_then(|r| r.as_slot()).is_some(),
        )
    }

    pub(super) fn edit_state(&self, ctx: &egui::Context, s: &mut MenuState) {
        let focus = self.edit_focus(ctx);
        let (has_selection, can_paste) = match focus {
            EditFocus::Text => (true, true),
            EditFocus::Notes => (
                self.ui.piano.has_selection(),
                matches!(self.ui.clipboard, Some(ClipBoard::Notes(_))),
            ),
            EditFocus::SessionClip => (
                self.ui
                    .selected_clip
                    .is_some_and(|r| self.session.clip_at(r).is_some()),
                matches!(self.ui.clipboard, Some(ClipBoard::Slot(_))),
            ),
            EditFocus::ArrClips => (
                !self.ui.arr.selection.is_empty(),
                matches!(self.ui.clipboard, Some(ClipBoard::ArrClips(_))),
            ),
            EditFocus::Envelopes => (
                self.ui.arr.auto.sel.is_some(),
                matches!(self.ui.clipboard, Some(ClipBoard::Envelopes(_))),
            ),
            EditFocus::None => (false, false),
        };
        let on = |c: Edit| M::Edit(c);
        s.enable(on(Edit::Cut), has_selection);
        s.enable(on(Edit::Copy), has_selection);
        s.enable(on(Edit::Paste), can_paste);
        s.enable(
            on(Edit::Duplicate),
            focus != EditFocus::Text && has_selection,
        );
        s.enable(on(Edit::Delete), focus != EditFocus::Text && has_selection);
        s.enable(
            on(Edit::Rename),
            focus != EditFocus::Text && matches!(self.ui.selected, Some(ChainRef::Track(_))),
        );
        s.enable(
            on(Edit::ReturnToDefault),
            self.ui.layout.show_in_out && !self.selected_track_ids().is_empty(),
        );
        s.enable(
            on(Edit::SelectAll),
            matches!(
                focus,
                EditFocus::Text | EditFocus::Notes | EditFocus::ArrClips
            ),
        );
        let (undo, redo) = if focus == EditFocus::Text {
            (true, true)
        } else {
            s.undo_label = self.session.history.undo_label();
            s.redo_label = self.session.history.redo_label();
            (s.undo_label.is_some(), s.redo_label.is_some())
        };
        s.enable(on(Edit::Undo), undo);
        s.enable(on(Edit::Redo), redo);
        for (cmd, q) in RECORD_QUANTIZE {
            s.check(on(cmd), self.session.project.record_quantize == q);
            s.enable(on(cmd), !self.session.is_recording());
        }
    }

    pub(super) fn edit_dispatch(&mut self, ctx: &egui::Context, c: Edit) {
        if let Some(q) = RECORD_QUANTIZE
            .iter()
            .find(|(cmd, _)| *cmd == c)
            .map(|(_, q)| *q)
        {
            self.session.set_record_quantize(q);
            return;
        }
        match c {
            Edit::Undo | Edit::Redo => self.undo_redo(ctx, c),
            Edit::Rename if self.rename_in_rack() => {}
            Edit::ReturnToDefault => {
                let ids = self.selected_track_ids();
                self.session.reset_monitor(&ids);
            }
            Edit::DeleteEnvelope => {
                automation::delete_focused_envelope(&mut self.session, &self.ui);
            }
            Edit::Rename => {
                if let Some(ChainRef::Track(id)) = self.ui.selected {
                    if let Some(t) = self.session.project.track(id) {
                        self.ui.renaming = Some((id, t.name.clone()));
                    }
                }
            }
            _ => self.edit_command(ctx, c),
        }
    }

    fn undo_redo(&mut self, ctx: &egui::Context, c: Edit) {
        // Text fields keep their own undo inside egui.
        if self.edit_focus(ctx) == EditFocus::Text {
            return;
        }
        let changed = if c == Edit::Undo {
            self.session.undo()
        } else {
            self.session.redo()
        };
        if !changed {
            return;
        }
        self.ui.piano.selected.clear();
        let project = &self.session.project;
        self.ui
            .arr
            .selection
            .retain(|(t, id)| project.track(*t).is_some_and(|t| t.arr_clip(*id).is_some()));
        if self
            .ui
            .selected_clip
            .is_some_and(|r| self.session.clip_at(r).is_none() && r.as_slot().is_none())
        {
            self.ui.selected_clip = None;
        }
        let selected_exists = match self.ui.selected {
            Some(ChainRef::Track(t)) => self.session.project.track(t).is_some(),
            _ => true,
        };
        if !selected_exists {
            self.ui.selected = self
                .session
                .project
                .tracks
                .first()
                .map(|t| ChainRef::Track(t.id));
        }
    }

    fn edit_command(&mut self, ctx: &egui::Context, c: Edit) {
        match self.edit_focus(ctx) {
            EditFocus::Notes => self.edit_notes(c),
            EditFocus::ArrClips => self.edit_arr_clips(c),
            EditFocus::SessionClip => self.edit_session_clip(c),
            EditFocus::Envelopes => automation::edit_selection(&mut self.session, &mut self.ui, c),
            // Text fields handle their own editing inside egui.
            EditFocus::Text | EditFocus::None => {}
        }
    }

    fn edit_notes(&mut self, c: Edit) {
        let (session, ui) = (&mut self.session, &mut self.ui);
        match c {
            Edit::Copy | Edit::Cut => {
                if let Some(notes) = piano_roll::copy_selection(session, ui) {
                    ui.clipboard = Some(ClipBoard::Notes(notes));
                    if c == Edit::Cut {
                        piano_roll::delete_selected(session, ui);
                    }
                }
            }
            Edit::Paste => {
                if let Some(ClipBoard::Notes(notes)) = ui.clipboard.clone() {
                    piano_roll::paste_notes(session, ui, &notes);
                }
            }
            Edit::Duplicate => piano_roll::duplicate_selection(session, ui),
            Edit::Delete => piano_roll::delete_selected(session, ui),
            Edit::SelectAll => piano_roll::select_all(session, ui),
            _ => {}
        }
    }

    fn copy_arr_clips(&self) -> Vec<(TrackId, ArrangementClip)> {
        let mut clips: Vec<(TrackId, ArrangementClip)> = self
            .ui
            .arr
            .selection
            .iter()
            .filter_map(|(t, id)| {
                let c = self.session.project.track(*t)?.arr_clip(*id)?;
                Some((*t, c.clone()))
            })
            .collect();
        let first = clips.iter().map(|(_, c)| c.start).min().unwrap_or(0);
        for (_, c) in &mut clips {
            c.start -= first;
        }
        clips
    }

    fn edit_arr_clips(&mut self, c: Edit) {
        match c {
            Edit::Copy | Edit::Cut => {
                let clips = self.copy_arr_clips();
                if !clips.is_empty() {
                    self.ui.clipboard = Some(ClipBoard::ArrClips(clips));
                }
                if c == Edit::Cut {
                    let items = std::mem::take(&mut self.ui.arr.selection);
                    self.session.arr_delete(&items);
                }
            }
            Edit::Paste => {
                if let Some(ClipBoard::ArrClips(clips)) = self.ui.clipboard.clone() {
                    let at = self.session.insert_marker() as u64;
                    self.ui.arr.selection = self.session.arr_paste(&clips, at);
                }
            }
            Edit::Duplicate => {
                let items = self.ui.arr.selection.clone();
                self.ui.arr.selection = self.session.arr_duplicate(&items);
            }
            Edit::Delete => {
                let items = std::mem::take(&mut self.ui.arr.selection);
                self.session.arr_delete(&items);
            }
            Edit::SelectAll => {
                self.ui.arr.selection = self
                    .session
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| t.arrangement.iter().map(|c| (t.id, c.id)))
                    .collect();
            }
            _ => {}
        }
    }

    fn edit_session_clip(&mut self, c: Edit) {
        let Some((track, slot)) = self.ui.selected_clip.and_then(|r| r.as_slot()) else {
            return;
        };
        match c {
            Edit::Copy | Edit::Cut => {
                if let Some(clip) = self.session.clip(track, slot).cloned() {
                    self.ui.clipboard = Some(ClipBoard::Slot(Box::new(clip)));
                    if c == Edit::Cut {
                        self.session.set_clip(track, slot, None);
                    }
                }
            }
            Edit::Paste => {
                if let Some(ClipBoard::Slot(clip)) = self.ui.clipboard.clone() {
                    self.paste_slot(track, slot, *clip);
                }
            }
            Edit::Duplicate => session_view::duplicate_clip(&mut self.session, track, slot),
            Edit::Delete => {
                self.session.set_clip(track, slot, None);
                self.ui.selected_clip = None;
            }
            _ => {}
        }
    }

    fn paste_slot(&mut self, track: rlive_core::TrackId, slot: usize, clip: Clip) {
        let kind_matches = self
            .session
            .project
            .track(track)
            .is_some_and(|t| (t.kind == TrackKind::Midi) == clip.is_midi());
        if !kind_matches {
            self.ui.status = "That clip can't be pasted on this kind of track".into();
            return;
        }
        let target = if self.session.clip(track, slot).is_none() {
            slot
        } else {
            session_view::next_free_slot(&mut self.session, track, slot)
        };
        self.session.set_clip(track, target, Some(clip));
        self.ui.selected_clip = Some(ClipRef::slot(track, target));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_focus_wins_then_last_clicked_area() {
        use EditFocus as E;
        use FocusArea as A;
        assert_eq!(resolve_focus(true, A::Clip, true, true), E::Text);
        assert_eq!(resolve_focus(false, A::Clip, true, true), E::Notes);
        assert_eq!(resolve_focus(false, A::Session, true, true), E::SessionClip);
        assert_eq!(resolve_focus(false, A::Clip, false, true), E::SessionClip);
        assert_eq!(resolve_focus(false, A::Session, false, false), E::None);
        assert_eq!(
            resolve_focus(false, A::Arrangement, true, true),
            E::ArrClips
        );
    }
}
