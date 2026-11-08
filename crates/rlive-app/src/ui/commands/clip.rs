//! Commands of the Clip View: its tabs and panels, its markers and loop brace, and its grid.

use super::{Entry, Entry::Item, MenuCommand as M, MenuState};
use crate::session::SetMarker;
use crate::ui::clip_view::{self, EditorMode, MarkerSel};
use crate::ui::keymap::FocusArea;
use crate::ui::{Detail, RliveApp};
use rlive_project::clip_edit as edit;
use rlive_project::ClipContent;

super::domain! {
    /// Clip View commands.
    Clip {
        CycleEditorMode,
        ContentTab,
        EnvelopeTab,
        SetStart,
        SetLoopStart,
        SetLoopEnd,
        SetEnd,
        NudgeLeft,
        NudgeRight,
        MoveLoopUp,
        MoveLoopDown,
        DoubleLoop,
        HalveLoop,
        ShortenLoop,
        LengthenLoop,
        SelectLoopContents,
        SelectArrangementLoop,
        ShiftRegionLeft,
        ShiftRegionRight,
        DuplicateLoop,
        ToggleFollow,
        ToggleOverview,
        ToggleFullSize,
        FocusPanels,
        ZoomToSelection,
        ZoomBack,
        FitWidth,
        FitHeight,
        NarrowGrid,
        WidenGrid,
        TripletGrid,
        SnapGrid,
        AdaptiveGrid,
        ArrangePanelsAutomatically,
        ArrangePanelsHorizontally,
        ArrangePanelsVertically,
        CropClip,
        ReverseClip,
    }
}

pub const VIEW_MENU: &[Entry] = &[
    Item(
        "Arrange Clip View Panels Automatically",
        M::Clip(Clip::ArrangePanelsAutomatically),
    ),
    Item(
        "Arrange Clip View Panels Horizontally",
        M::Clip(Clip::ArrangePanelsHorizontally),
    ),
    Item(
        "Arrange Clip View Panels Vertically",
        M::Clip(Clip::ArrangePanelsVertically),
    ),
];

pub const EDIT_MENU: &[Entry] = &[
    Item("Duplicate Loop", M::Clip(Clip::DuplicateLoop)),
    Item("Crop Clip", M::Clip(Clip::CropClip)),
    Item("Reverse Clip", M::Clip(Clip::ReverseClip)),
];

/// The grid steps a Narrow or Widen goes between.
const MIN_GRID: u64 = rlive_core::PPQ / 32;
const MAX_GRID: u64 = rlive_core::PPQ * 4;

impl RliveApp {
    pub(super) fn clip_state(&self, s: &mut MenuState) {
        let has_clip = self
            .ui
            .selected_clip
            .is_some_and(|r| self.session.clip_at(r).is_some());
        for c in [Clip::DuplicateLoop, Clip::CropClip, Clip::ReverseClip] {
            s.enable(M::Clip(c), has_clip);
        }
        let layout = self.ui.clip_view.layout;
        use clip_view::PanelLayout as P;
        s.check(
            M::Clip(Clip::ArrangePanelsAutomatically),
            layout == P::Automatic,
        );
        s.check(
            M::Clip(Clip::ArrangePanelsHorizontally),
            layout == P::Horizontal,
        );
        s.check(
            M::Clip(Clip::ArrangePanelsVertically),
            layout == P::Vertical,
        );
        s.check(M::Clip(Clip::ToggleFollow), self.ui.clip_view.follow);
    }

    /// Where a Set shortcut puts a marker: the playhead while it plays, or the insert marker.
    fn set_position(&self) -> Option<u64> {
        let r = self.ui.selected_clip?;
        if let Some(at) = self.session.clip_playhead(r) {
            return Some(self.session.quantized(at));
        }
        let clip = self.session.clip_at(r)?;
        if clip.is_midi() {
            self.ui.piano.insert_tick
        } else {
            let seconds = self.ui.sample.insert_seconds()?;
            Some(self.session.tick_of_seconds(clip, seconds).round() as u64)
        }
    }

    fn set_marker_shortcut(&mut self, which: SetMarker) {
        if let (Some(r), Some(at)) = (self.ui.selected_clip, self.set_position()) {
            self.session.set_marker(r, which, at);
        }
    }

    fn set_layout(&mut self, layout: clip_view::PanelLayout) {
        self.ui.clip_view.layout = layout;
        self.settings.clip_panel_layout = layout;
        self.settings.save();
    }

    /// Selects what's inside the loop brace.
    fn select_loop_contents(&mut self) {
        let Some(r) = self.ui.selected_clip else {
            return;
        };
        let Some(clip) = self.session.clip_at(r).cloned() else {
            return;
        };
        let (from, to) = (clip.loop_start, clip.loop_end());
        if clip.is_midi() {
            self.ui.piano.selected = clip
                .notes()
                .iter()
                .enumerate()
                .filter(|(_, n)| n.start >= from && n.start < to)
                .map(|(i, _)| i)
                .collect();
        } else if let ClipContent::Audio { .. } = clip.content {
            let a = self.session.tick_seconds_of(&clip, from as f64);
            let b = self.session.tick_seconds_of(&clip, to as f64);
            self.ui.sample.select(a, b);
        }
    }

    fn zoom_to_selection(&mut self) {
        let Some(r) = self.ui.selected_clip else {
            return;
        };
        let Some((from, to)) = clip_view::time_selection(&self.session, &self.ui, r) else {
            return;
        };
        if to > from {
            self.ui
                .clip_view
                .zoom_to(&mut self.ui.piano, &mut self.ui.sample, from, to);
        }
    }

    pub(super) fn clip_command(&mut self, c: Clip) {
        match c {
            Clip::ArrangePanelsAutomatically => {
                return self.set_layout(clip_view::PanelLayout::Automatic)
            }
            Clip::ArrangePanelsHorizontally => {
                return self.set_layout(clip_view::PanelLayout::Horizontal)
            }
            Clip::ArrangePanelsVertically => {
                return self.set_layout(clip_view::PanelLayout::Vertical)
            }
            Clip::ToggleFollow => {
                self.ui.clip_view.follow = !self.ui.clip_view.follow;
                return;
            }
            Clip::ToggleOverview => {
                self.ui.clip_view.show_overview = !self.ui.clip_view.show_overview;
                return;
            }
            Clip::ToggleFullSize => {
                self.ui.clip_view.full_size = !self.ui.clip_view.full_size;
                return;
            }
            Clip::FocusPanels => {
                self.ui.show_detail = true;
                self.ui.detail = Detail::Clip;
                self.ui.clip_view.folded = false;
                self.ui.focus = FocusArea::Clip;
                return;
            }
            Clip::NarrowGrid => {
                self.ui.piano.grid = (self.ui.piano.grid / 2).max(MIN_GRID);
                return;
            }
            Clip::WidenGrid => {
                self.ui.piano.grid = (self.ui.piano.grid * 2).min(MAX_GRID);
                return;
            }
            Clip::TripletGrid => {
                let g = self.ui.piano.grid;
                let triplet =
                    rlive_core::PPQ.is_multiple_of(g) && (rlive_core::PPQ / g).is_multiple_of(3);
                self.ui.piano.grid = if triplet {
                    g * 3 / 2
                } else {
                    (g * 2 / 3).max(1)
                };
                return;
            }
            Clip::SnapGrid => {
                self.ui.piano.snap_off = !self.ui.piano.snap_off;
                return;
            }
            Clip::AdaptiveGrid => {
                self.ui.piano.fixed_grid = !self.ui.piano.fixed_grid;
                return;
            }
            Clip::SelectArrangementLoop => {
                let a = &self.session.project.arrangement;
                let (from, to) = (a.loop_start, a.loop_end);
                self.ui.arr.selection = self
                    .session
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| {
                        t.arrangement
                            .iter()
                            .filter(move |c| c.start < to && c.end() > from)
                            .map(move |c| (t.id, c.id))
                    })
                    .collect();
                return;
            }
            Clip::ZoomToSelection => return self.zoom_to_selection(),
            Clip::ZoomBack => {
                self.ui
                    .clip_view
                    .zoom_back(&mut self.ui.piano, &mut self.ui.sample);
                return;
            }
            Clip::FitWidth => {
                self.ui
                    .clip_view
                    .fit_width(&mut self.ui.piano, &mut self.ui.sample);
                return;
            }
            Clip::FitHeight => {
                self.ui.piano.reset_view = true;
                return;
            }
            _ => {}
        }
        let Some(r) = self.ui.selected_clip else {
            return;
        };
        let Some(clip) = self.session.clip_at(r).cloned() else {
            return;
        };
        let group = clip_view::targets(&self.session, &self.ui, r);
        let grid = self.ui.piano.grid as i64;
        let view = &mut self.ui.clip_view;
        match c {
            Clip::CycleEditorMode => view.mode = view.mode.next(clip.is_midi()),
            Clip::ContentTab => view.mode = EditorMode::Content,
            Clip::EnvelopeTab => view.mode = EditorMode::Envelope,
            Clip::SetStart => self.set_marker_shortcut(SetMarker::Start),
            Clip::SetEnd => self.set_marker_shortcut(SetMarker::End),
            Clip::SetLoopStart => self.set_marker_shortcut(SetMarker::LoopPosition),
            Clip::SetLoopEnd => self.set_marker_shortcut(SetMarker::LoopLength),
            Clip::NudgeLeft | Clip::NudgeRight => {
                let by = if c == Clip::NudgeLeft { -grid } else { grid };
                match view.marker {
                    Some(MarkerSel::Start) => {
                        let at = (clip.start as i64 + by).max(0) as u64;
                        self.session
                            .edit_clips(&[r], "Move Start Marker", |c| edit::set_start(c, at));
                    }
                    Some(MarkerSel::End) => {
                        let at = (clip.end_marker() as i64 + by).max(0) as u64;
                        self.session
                            .edit_clips(&[r], "Move End Marker", |c| edit::set_end(c, at));
                    }
                    _ => {
                        self.session
                            .edit_clips(&[r], "Move Loop", |c| edit::move_loop(c, by));
                    }
                }
            }
            Clip::MoveLoopUp | Clip::MoveLoopDown => {
                let by = clip.length as i64;
                let by = if c == Clip::MoveLoopUp { by } else { -by };
                self.session
                    .edit_clips(&[r], "Move Loop", |c| edit::move_loop(c, by));
            }
            Clip::DoubleLoop | Clip::HalveLoop => {
                let factor = if c == Clip::DoubleLoop { 2.0 } else { 0.5 };
                self.session
                    .edit_clips(&[r], "Loop Length", |c| edit::scale_loop(c, factor));
            }
            Clip::ShortenLoop | Clip::LengthenLoop => {
                let by = if c == Clip::ShortenLoop { -grid } else { grid };
                self.session
                    .edit_clips(&[r], "Loop Length", |c| edit::resize_loop(c, by));
            }
            Clip::ShiftRegionLeft | Clip::ShiftRegionRight => {
                let by = if c == Clip::ShiftRegionLeft {
                    -grid
                } else {
                    grid
                };
                self.session
                    .edit_clips(&[r], "Move Clip Region", |c| edit::shift_region(c, by));
            }
            Clip::SelectLoopContents => self.select_loop_contents(),
            Clip::DuplicateLoop => {
                self.session.duplicate_loop(r);
            }
            Clip::CropClip => {
                self.session.crop_clip(r, None);
            }
            Clip::ReverseClip => {
                for target in group {
                    self.session.reverse_clip(target);
                }
            }
            _ => {}
        }
    }
}
