//! Commands of the Arrangement: splitting and the "... Time" commands, the loop, locators,
//! fades, the grid and how the view is framed.

use super::{ClipBoard, Entry, Entry::Item, Entry::Separator, MenuCommand as M, MenuState};
use crate::session::LoopEdit;
use crate::ui::arrangement::selection::{self, TimeSel};
use crate::ui::arrangement::view;
use crate::ui::automation::LaneKey;
use crate::ui::{MainView, RliveApp};
use rlive_core::TrackId;

super::domain! {
    /// Editing the Arrangement.
    Arrange {
        Split,
        Crop,
        CutTime,
        CopyTime,
        PasteTime,
        DuplicateTime,
        DeleteTime,
        InsertSilence,
        LoopSelection,
        SelectLoop,
        NudgeLeft,
        NudgeRight,
        ExtendLeft,
        ExtendRight,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SnapLeft,
        SnapRight,
        Deactivate,
        AddLocator,
        PreviousLocator,
        NextLocator,
        InsertSignature,
        CreateFade,
        DeleteFades,
        ZoomToSelection,
        ZoomBack,
        OptimizeHeight,
        OptimizeWidth,
        ToggleFold,
        Fold,
        Unfold,
        UnfoldAll,
        Taller,
        Shorter,
        ToggleOverview,
        ToggleFollow,
        GridNarrow,
        GridWiden,
        GridTriplet,
        GridSnap,
        GridFixed,
        LoopNudgeLeft,
        LoopNudgeRight,
        LoopShiftUp,
        LoopShiftDown,
        LoopDouble,
        LoopHalve,
        LoopShorten,
        LoopLengthen,
        PlaySelection,
        PlaySelectionOnce,
        PlayFromInsert,
        InsertToPlayhead,
        InsertToEnd,
        ResizeLeft,
        ResizeRight,
    }
}

pub const EDIT_ITEMS: &[Entry] = &[
    Separator,
    Item("Split", M::Arrange(Arrange::Split)),
    Item("Crop Clip(s)", M::Arrange(Arrange::Crop)),
    Separator,
    Item("Cut Time", M::Arrange(Arrange::CutTime)),
    Item("Copy Time", M::Arrange(Arrange::CopyTime)),
    Item("Paste Time", M::Arrange(Arrange::PasteTime)),
    Item("Duplicate Time", M::Arrange(Arrange::DuplicateTime)),
    Item("Delete Time", M::Arrange(Arrange::DeleteTime)),
    Item("Insert Silence", M::Arrange(Arrange::InsertSilence)),
    Separator,
    Item("Loop Selection", M::Arrange(Arrange::LoopSelection)),
    Item("Select Loop", M::Arrange(Arrange::SelectLoop)),
];

pub const CREATE_ITEMS: &[Entry] = &[
    Separator,
    Item("Add Locator", M::Arrange(Arrange::AddLocator)),
    Item(
        "Insert Time Signature Change",
        M::Arrange(Arrange::InsertSignature),
    ),
    Item("Create Fade or Crossfade", M::Arrange(Arrange::CreateFade)),
];

pub const VIEW_ITEMS: &[Entry] = &[
    Separator,
    Item("Overview", M::Arrange(Arrange::ToggleOverview)),
    Item("Follow", M::Arrange(Arrange::ToggleFollow)),
    Item(
        "Zoom to Arrangement Time Selection",
        M::Arrange(Arrange::ZoomToSelection),
    ),
    Item("Zoom Back", M::Arrange(Arrange::ZoomBack)),
    Item("Optimize Height", M::Arrange(Arrange::OptimizeHeight)),
    Item("Optimize Width", M::Arrange(Arrange::OptimizeWidth)),
];

impl RliveApp {
    fn span(&self) -> Option<(u64, u64)> {
        self.ui
            .arr
            .selected_span(&self.session)
            .filter(|(a, b)| b > a)
    }

    fn arrange_grid(&self) -> u64 {
        let bar = self.session.project.time_signature.ticks_per_bar();
        self.ui.arr.grid.ticks(self.ui.arr.px_per_beat, bar).max(1)
    }

    /// The tracks to work on, or if the selection names none, every track selected.
    fn arrange_tracks(&self) -> Vec<TrackId> {
        let mut out = self.ui.arr.selected_tracks();
        if out.is_empty() {
            out = self.selected_track_ids();
        }
        out
    }

    fn insert_lanes(&self) -> Vec<LaneKey> {
        self.ui
            .arr
            .time
            .as_ref()
            .map(|t| t.lanes.clone())
            .unwrap_or_default()
    }

    /// Everything [`Arrange`] needs from the menus' state.
    pub(super) fn arrange_state(&self, s: &mut MenuState) {
        let on = |c: Arrange| M::Arrange(c);
        let span = self.span().is_some();
        let arr = self.ui.main_view == MainView::Arrangement;
        for c in [
            Arrange::Split,
            Arrange::Crop,
            Arrange::CutTime,
            Arrange::CopyTime,
            Arrange::DuplicateTime,
            Arrange::DeleteTime,
            Arrange::LoopSelection,
            Arrange::Deactivate,
            Arrange::CreateFade,
            Arrange::ZoomToSelection,
        ] {
            s.enable(on(c), arr && span);
        }
        s.enable(
            on(Arrange::PasteTime),
            matches!(self.ui.clipboard, Some(ClipBoard::Time(_))),
        );
        s.enable(on(Arrange::ZoomBack), !self.ui.arr.zoom_history.is_empty());
        s.check(on(Arrange::ToggleFollow), self.ui.arr.follow.on);
        s.check(on(Arrange::ToggleOverview), self.ui.arr.show_overview);
    }

    pub(super) fn arrange_command(&mut self, c: Arrange) {
        let grid = self.arrange_grid() as i64;
        match c {
            Arrange::Split => self.split_selection(),
            Arrange::Crop => {
                if let Some((a, b)) = self.span() {
                    let tracks = self.arrange_tracks();
                    self.session.arr_crop(&tracks, a, b);
                }
            }
            Arrange::CutTime => {
                if let Some((a, b)) = self.span() {
                    self.ui.clipboard = Some(ClipBoard::Time(self.session.cut_time(a, b)));
                    self.collapse_selection(a);
                }
            }
            Arrange::CopyTime => {
                if let Some((a, b)) = self.span() {
                    self.ui.clipboard = Some(ClipBoard::Time(self.session.copy_time(a, b)));
                }
            }
            Arrange::PasteTime => {
                if let Some(ClipBoard::Time(clip)) = self.ui.clipboard.clone() {
                    let at = self.session.insert_marker() as u64;
                    self.session.paste_time(at, &clip);
                }
            }
            Arrange::DuplicateTime => {
                if let Some((a, b)) = self.span() {
                    self.session.duplicate_time(a, b);
                    let len = (b - a) as f64;
                    if let Some(t) = self.ui.arr.time.as_mut() {
                        t.start += len;
                        t.end += len;
                    }
                }
            }
            Arrange::DeleteTime => {
                if let Some((a, b)) = self.span() {
                    self.session.delete_time(a, b);
                    self.collapse_selection(a);
                }
            }
            Arrange::InsertSilence => self.start_insert_silence(),
            Arrange::LoopSelection => self.loop_selection(),
            Arrange::SelectLoop => self.ui.arr.select_loop(&self.session),
            Arrange::NudgeLeft => self.nudge(-grid),
            Arrange::NudgeRight => self.nudge(grid),
            Arrange::ExtendLeft => self.extend(-grid),
            Arrange::ExtendRight => self.extend(grid),
            Arrange::MoveLeft => self.move_insert(-grid),
            Arrange::MoveRight => self.move_insert(grid),
            Arrange::MoveUp => self.move_insert_lane(-1),
            Arrange::MoveDown => self.move_insert_lane(1),
            Arrange::SnapLeft => self.snap_insert(false),
            Arrange::SnapRight => self.snap_insert(true),
            Arrange::Deactivate => self.deactivate(),
            Arrange::AddLocator => {
                let at = self
                    .ui
                    .arr
                    .time
                    .as_ref()
                    .map_or(self.session.insert_marker() as u64, |t| t.start as u64);
                self.ui.arr.selected_locator = Some(self.session.set_locator(at));
            }
            Arrange::PreviousLocator => self.session.prev_locator(),
            Arrange::NextLocator => self.session.next_locator(),
            Arrange::InsertSignature => {
                let at = self.session.insert_marker() as u64;
                let sig = self.session.project.bar_map().signature_at(at as f64);
                self.session.add_signature_marker(at, sig);
            }
            Arrange::CreateFade => {
                if let Some((a, b)) = self.span() {
                    let tracks = self.arrange_tracks();
                    self.session.arr_fade_from_span(&tracks, a, b);
                }
            }
            Arrange::DeleteFades => {
                let items = self.ui.arr.selection.clone();
                self.session.arr_delete_fades(&items);
            }
            Arrange::ZoomToSelection => {
                let w = self.ui.arr.lanes_size().x;
                self.ui.arr.zoom_to_selection(&self.session, w);
            }
            Arrange::ZoomBack => self.ui.arr.zoom_back(),
            Arrange::OptimizeHeight => self.optimize_height(),
            Arrange::OptimizeWidth => {
                let w = self.ui.arr.lanes_size().x;
                let bar = self.session.project.time_signature.ticks_per_bar();
                let end = self.session.project.arrangement_end() + bar;
                let (px, scroll) = view::zoom_for(0, end.max(bar), w, 1.0, 240.0);
                self.ui.arr.px_per_beat = px;
                self.ui.arr.scroll.x = scroll;
            }
            Arrange::ToggleFold => self.fold_selected(None),
            Arrange::Fold => self.fold_selected(Some(true)),
            Arrange::Unfold => self.fold_selected(Some(false)),
            Arrange::UnfoldAll => {
                let ids: Vec<TrackId> = self.session.project.tracks.iter().map(|t| t.id).collect();
                for t in ids {
                    self.set_track_folded(t, false);
                }
            }
            Arrange::Taller => self.resize_selected(8.0),
            Arrange::Shorter => self.resize_selected(-8.0),
            Arrange::ToggleOverview => self.ui.arr.show_overview = !self.ui.arr.show_overview,
            Arrange::ToggleFollow => self.ui.arr.follow.on = !self.ui.arr.follow.on,
            Arrange::GridNarrow => self.ui.arr.grid.narrow(),
            Arrange::GridWiden => {
                let bar = self.session.project.time_signature.ticks_per_bar();
                self.ui.arr.grid.widen(bar);
            }
            Arrange::GridTriplet => self.ui.arr.grid.triplet = !self.ui.arr.grid.triplet,
            Arrange::GridSnap => self.ui.arr.grid.snap = !self.ui.arr.grid.snap,
            Arrange::GridFixed => self.ui.arr.grid.fixed = !self.ui.arr.grid.fixed,
            Arrange::LoopNudgeLeft => self.session.loop_edit(LoopEdit::Nudge(-grid)),
            Arrange::LoopNudgeRight => self.session.loop_edit(LoopEdit::Nudge(grid)),
            Arrange::LoopShiftUp => self.session.loop_edit(LoopEdit::Shift(1)),
            Arrange::LoopShiftDown => self.session.loop_edit(LoopEdit::Shift(-1)),
            Arrange::LoopDouble => self.session.loop_edit(LoopEdit::Double),
            Arrange::LoopHalve => self.session.loop_edit(LoopEdit::Halve),
            Arrange::LoopShorten => self.session.loop_edit(LoopEdit::Resize(-grid)),
            Arrange::LoopLengthen => self.session.loop_edit(LoopEdit::Resize(grid)),
            Arrange::PlaySelection => self.play_selection(false),
            Arrange::PlaySelectionOnce => self.play_selection(true),
            Arrange::PlayFromInsert => {
                let at = self.session.insert_marker();
                self.session.seek(at);
                self.session.play();
            }
            Arrange::InsertToPlayhead => {
                let tick = self.session.engine.shared.tick.load();
                self.session.seek(tick);
            }
            Arrange::InsertToEnd => {
                let end = self.session.project.arrangement_end();
                self.session.seek(end as f64);
            }
            Arrange::ResizeLeft => self.resize_at_insert(-grid),
            Arrange::ResizeRight => self.resize_at_insert(grid),
        }
    }

    /// Leaves an insert marker where a span of time was just taken away.
    fn collapse_selection(&mut self, at: u64) {
        self.ui.arr.selection.clear();
        if let Some(t) = self.ui.arr.time.as_mut() {
            t.start = at as f64;
            t.end = at as f64;
        }
        self.session.seek(at as f64);
    }

    fn split_selection(&mut self) {
        let tracks = self.arrange_tracks();
        if let Some(t) = self.ui.arr.time.as_ref().filter(|t| !t.is_point()) {
            let (a, b) = t.ticks();
            self.session.arr_split(&tracks, &[a, b]);
            // What was selected is its own clips now, ready to move.
            self.ui.arr.selection = tracks
                .iter()
                .filter_map(|id| self.session.project.track(*id))
                .flat_map(|t| {
                    t.arrangement
                        .iter()
                        .filter(|c| c.start >= a && c.end() <= b)
                        .map(|c| (t.id, c.id))
                        .collect::<Vec<_>>()
                })
                .collect();
            return;
        }
        let at = self
            .ui
            .arr
            .time
            .as_ref()
            .map_or(self.session.insert_marker() as u64, |t| t.start as u64);
        self.session.arr_split(&tracks, &[at]);
    }

    fn start_insert_silence(&mut self) {
        let at = self.session.insert_marker() as u64;
        let bar = self.session.project.time_signature.ticks_per_bar();
        let bars = self
            .span()
            .map_or(1.0, |(a, b)| (b - a) as f64 / bar as f64);
        self.ui.arr.inserting_silence = Some((at, format!("{bars}")));
    }

    fn loop_selection(&mut self) {
        if let Some((a, b)) = self.span() {
            self.session.loop_selection(a, b);
        } else {
            let a = self.session.project.arrangement.clone();
            if a.loop_enabled {
                self.session.set_loop(false, a.loop_start, a.loop_end);
            } else {
                let end = self.session.project.arrangement_end().max(a.loop_end);
                self.session.set_loop(true, 0, end);
            }
        }
    }

    /// Moves the selection by whole grid steps.
    fn nudge(&mut self, delta: i64) {
        if !self.ui.arr.selection.is_empty() {
            let moves: Vec<crate::session::ArrMove> = self
                .ui
                .arr
                .selection
                .iter()
                .filter_map(|(t, id)| {
                    let c = self.session.project.track(*t)?.arr_clip(*id)?;
                    Some(crate::session::ArrMove {
                        track: *t,
                        id: *id,
                        to_track: *t,
                        start: (c.start as i64 + delta).max(0) as u64,
                    })
                })
                .collect();
            self.ui.arr.selection = self.session.arr_move(&moves, false);
            self.ui.arr.time = None;
            return;
        }
        let Some((a, b)) = self.span() else { return };
        let tracks = self.arrange_tracks();
        let placed = self.session.arr_move_span(&tracks, a, b, delta);
        if let Some(t) = self.ui.arr.time.as_mut() {
            t.start = (t.start + delta as f64).max(0.0);
            t.end = (t.end + delta as f64).max(t.start);
        }
        self.ui.arr.selection = placed;
    }

    fn extend(&mut self, delta: i64) {
        if let Some(t) = self.ui.arr.time.as_mut() {
            *t = selection::grown(t, delta as f64);
        }
    }

    fn move_insert(&mut self, delta: i64) {
        if let Some(i) = self.ui.arr.selected_locator {
            let t = self.session.project.arrangement.locators[i].tick;
            let to = (t as i64 + delta).max(0) as u64;
            self.ui.arr.selected_locator = Some(self.session.move_locator(i, to));
            return;
        }
        if let Some(i) = self.ui.arr.selected_signature {
            let t = self.session.project.arrangement.signatures[i].tick;
            let to = (t as i64 + delta).max(0) as u64;
            self.ui.arr.selected_signature = Some(self.session.move_signature_marker(i, to));
            return;
        }
        let from = self.session.insert_marker() as i64;
        let tick = (from + delta).max(0) as f64;
        self.session.seek(tick);
        self.set_insert(tick);
    }

    /// Puts the insert marker at `tick` on the lanes it's already on.
    fn set_insert(&mut self, tick: f64) {
        let lanes = self.insert_lanes();
        if lanes.is_empty() {
            return;
        }
        self.ui.arr.time = Some(TimeSel {
            start: tick,
            end: tick,
            lanes,
        });
    }

    fn move_insert_lane(&mut self, by: i32) {
        let visible = self.session.project.visible_tracks();
        let ids: Vec<TrackId> = visible
            .iter()
            .map(|&v| self.session.project.tracks[v].id)
            .collect();
        let Some(current) = self.arrange_tracks().first().copied() else {
            return;
        };
        let Some(i) = ids.iter().position(|t| *t == current) else {
            return;
        };
        let to = (i as i32 + by).clamp(0, ids.len() as i32 - 1) as usize;
        let tick = self.session.insert_marker();
        let id = ids[to];
        self.ui.arr.selection.clear();
        self.ui.arr.time = Some(TimeSel::point(
            tick,
            LaneKey {
                chain: crate::session::ChainRef::Track(id),
                slot: None,
            },
        ));
        self.ui.selected = Some(crate::session::ChainRef::Track(id));
        self.ui.selected_tracks = vec![id];
    }

    /// Jumps to the next locator or clip edge on the selected tracks.
    fn snap_insert(&mut self, forward: bool) {
        let from = self.session.insert_marker();
        let tracks = self.arrange_tracks();
        let mut points: Vec<u64> = self
            .session
            .project
            .arrangement
            .locators
            .iter()
            .map(|l| l.tick)
            .collect();
        for id in tracks {
            if let Some(t) = self.session.project.track(id) {
                for c in &t.arrangement {
                    points.push(c.start);
                    points.push(c.end());
                }
            }
        }
        points.sort_unstable();
        let target = if forward {
            points.into_iter().find(|p| *p as f64 > from + 0.5)
        } else {
            points.into_iter().rev().find(|p| (*p as f64) < from - 0.5)
        };
        if let Some(t) = target {
            self.session.seek(t as f64);
            self.set_insert(t as f64);
        }
    }

    /// Enter and the arrow keys: moves the end of the clip under the insert marker.
    fn resize_at_insert(&mut self, delta: i64) {
        let at = self.session.insert_marker() as u64;
        let found = self.arrange_tracks().into_iter().find_map(|id| {
            let t = self.session.project.track(id)?;
            let c = t
                .arrangement
                .iter()
                .find(|c| c.start <= at && at < c.end())?;
            Some((id, c.id, c.end()))
        });
        if let Some((track, id, end)) = found {
            let to = (end as i64 + delta).max(0) as u64;
            self.session
                .arr_trim(track, id, crate::session::TrimEdge::Right(to));
        }
    }

    fn deactivate(&mut self) {
        if !self.ui.arr.selection.is_empty() {
            let items = self.ui.arr.selection.clone();
            self.session.arr_toggle_active(&items);
        } else if let Some((a, b)) = self.span() {
            let tracks = self.arrange_tracks();
            self.session.arr_toggle_span(&tracks, a, b);
        }
    }

    fn optimize_height(&mut self) {
        let n = self.session.project.visible_tracks().len();
        let available = self.ui.arr.lanes_size().y;
        let h = view::optimize_height(available, n, 22.0, 420.0);
        let ids: Vec<TrackId> = self.session.project.tracks.iter().map(|t| t.id).collect();
        for t in ids {
            self.session
                .edit_track_ui(crate::session::ChainRef::Track(t), |ui| ui.height = None);
        }
        self.ui.arr.track_h = h;
    }

    fn set_track_folded(&mut self, id: TrackId, folded: bool) {
        self.session
            .edit_track_ui(crate::session::ChainRef::Track(id), |ui| ui.folded = folded);
    }

    fn fold_selected(&mut self, to: Option<bool>) {
        for id in self.selected_track_ids() {
            let folded = self
                .session
                .project
                .track(id)
                .is_some_and(|t| !t.is_group() && t.ui.folded);
            self.set_track_folded(id, to.unwrap_or(!folded));
        }
    }

    fn resize_selected(&mut self, by: f32) {
        let ids = self.selected_track_ids();
        let fallback = self.ui.arr.track_h;
        for id in ids {
            self.session
                .edit_track_ui(crate::session::ChainRef::Track(id), |ui| {
                    let h = ui.height.unwrap_or(fallback) + by;
                    ui.height = Some(h.clamp(22.0, 420.0));
                });
        }
    }

    /// Space with a selection: play it, or with `once`, stop at its end.
    fn play_selection(&mut self, once: bool) {
        if self.session.is_playing() {
            self.session.stop();
            return;
        }
        if let Some((a, b)) = self.span() {
            self.session.seek(a as f64);
            if once {
                self.session.stop_at(Some(b as f64));
            }
            self.session.play();
        }
    }
}
