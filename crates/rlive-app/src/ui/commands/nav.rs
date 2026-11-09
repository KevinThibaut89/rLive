//! The Navigate menu and everything the keyboard does to focused controls (manual ch. 41).

use super::{Entry, Entry::Item, Entry::Separator, MenuCommand as M, MenuState};
use crate::session::ChainRef;
use crate::ui::keymap::FocusArea;
use crate::ui::nav::{self, Dir, Group, ValueOp};
use crate::ui::{MainView, RliveApp};
use rlive_core::{DeviceId, TrackId};

super::domain! {
    /// Moving the keyboard focus, and entering values on what has it.
    Nav {
        NextControl,
        PrevControl,
        NextNeighbor,
        PrevNeighbor,
        ToggleTabFocus,
        ToggleWrapTab,
        FocusMixer,
        ValueUp,
        ValueDown,
        ValueUpFine,
        ValueDownFine,
        ValueDefault,
        Digit0, Digit1, Digit2, Digit3, Digit4,
        Digit5, Digit6, Digit7, Digit8, Digit9,
        NextField,
        Minus,
        ValueCancel,
        ValueConfirm,
        RenameNext,
        JumpToTitle,
        DeviceLeft,
        DeviceRight,
    }
}

/// The Navigate menu, in the manual's order.
pub const MENU: &[Entry] = &[
    Item("Control Bar", M::View(super::view::View::FocusControlBar)),
    Item("Session View", M::View(super::view::View::FocusSession)),
    Item(
        "Arrangement View",
        M::View(super::view::View::FocusArrangement),
    ),
    Item("Clip View", M::View(super::view::View::FocusClip)),
    Item("Device View", M::View(super::view::View::FocusDevice)),
    Item("Browser", M::View(super::view::View::FocusBrowser)),
    Item("Groove Pool", M::View(super::view::View::FocusGroovePool)),
    Separator,
    Item("Next Neighbor", M::Nav(Nav::NextNeighbor)),
    Item("Previous Neighbor", M::Nav(Nav::PrevNeighbor)),
    Separator,
    Item("Use Tab Key to Move Focus", M::Nav(Nav::ToggleTabFocus)),
    Item("Wrap Tab Navigation", M::Nav(Nav::ToggleWrapTab)),
];

impl RliveApp {
    pub(super) fn nav_state(&self, s: &mut MenuState) {
        s.check(M::Nav(Nav::ToggleTabFocus), self.settings.tab_moves_focus);
        s.check(M::Nav(Nav::ToggleWrapTab), self.settings.wrap_tab);
    }

    fn step_control(&mut self, ctx: &egui::Context, dir: Dir) {
        if self.ui.focus == FocusArea::Arrangement
            && matches!(dir, Dir::Next | Dir::Prev)
            && self.step_locator(ctx, dir == Dir::Next)
        {
            return;
        }
        nav::step_focus(ctx, self.ui.focus, dir, self.settings.wrap_tab);
    }

    /// Tab through the Arrangement's locators (manual p. 1007): from the selected one, or to
    /// the first or last when no control has the keyboard. Returns false to fall through to
    /// the controls when it runs off either end.
    fn step_locator(&mut self, ctx: &egui::Context, forward: bool) -> bool {
        let n = self.session.project.arrangement.locators.len();
        if n == 0 {
            return false;
        }
        let arr = &mut self.ui.arr;
        match arr.selected_locator {
            Some(i) => {
                let next = if forward {
                    (i + 1 < n).then(|| i + 1)
                } else {
                    i.checked_sub(1)
                };
                arr.selected_locator = next;
                nav::clear(ctx);
                next.is_some()
            }
            None if nav::focused(ctx).is_none() => {
                arr.selected_locator = Some(if forward { 0 } else { n - 1 });
                true
            }
            None => false,
        }
    }

    fn digit(ctx: &egui::Context, n: u8) {
        nav::type_char(ctx, char::from(b'0' + n));
    }

    /// Esc on a focused control: drop what was typed, or go up to the track's title bar.
    fn value_cancel(&mut self, ctx: &egui::Context) {
        if nav::cancel_entry(ctx) {
            return;
        }
        let track = nav::focused(ctx).and_then(|i| match i.group {
            Group::Track(t) => Some(t),
            _ => None,
        });
        nav::clear(ctx);
        if let Some(t) = track {
            self.jump_to_title(t);
        }
    }

    fn jump_to_title(&mut self, track: TrackId) {
        self.ui.title_focus = Some(track);
        self.ui.selected_clip = None;
        self.ui.sel.clear();
        self.ui.group_slot = None;
        self.ui.selected = Some(ChainRef::Track(track));
        if self.ui.active_view() == MainView::Arrangement {
            self.ui.arr.header_selected = true;
        }
    }

    /// The track the keyboard is working in.
    pub(super) fn keyboard_track(&self) -> Option<TrackId> {
        self.ui
            .title_focus
            .or_else(|| self.ui.selected_clip.map(crate::session::ClipRef::track))
            .or_else(|| self.ui.sel.slots.last().map(|s| s.0))
            .or(match self.ui.selected {
                Some(ChainRef::Track(t)) => Some(t),
                _ => None,
            })
    }

    /// Tab while renaming: the name so far goes in, and the next track's name opens.
    fn rename_next(&mut self) {
        let Some((id, text)) = self.ui.renaming.take() else {
            return;
        };
        if let Some(t) = self.session.project.track_mut(id) {
            t.name = text;
        }
        self.session.dirty = true;
        // Through the selected tracks, or down every track when just one is selected.
        let selected = self.selected_track_ids();
        let order: Vec<TrackId> = if selected.len() > 1 {
            selected
        } else {
            self.session
                .project
                .visible_tracks()
                .into_iter()
                .map(|i| self.session.project.tracks[i].id)
                .chain(self.session.project.returns.iter().map(|t| t.id))
                .collect()
        };
        let next = order
            .iter()
            .position(|t| *t == id)
            .and_then(|i| order.get(i + 1))
            .copied();
        if let Some(next) = next {
            if let Some(t) = self.session.project.track(next) {
                self.ui.selected = Some(ChainRef::Track(next));
                self.ui.renaming = Some((next, t.name.clone()));
            }
        }
    }

    fn step_device(&mut self, delta: isize) {
        let Some(chain) = self.ui.selected else {
            return;
        };
        let ids: Vec<DeviceId> = self
            .session
            .devices_of(chain)
            .iter()
            .map(|d| d.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let at = self
            .ui
            .devices
            .selected_devices
            .last()
            .and_then(|(_, id)| ids.iter().position(|d| d == id));
        let next = match at {
            Some(i) => i.wrapping_add_signed(delta).min(ids.len() - 1),
            None => 0,
        };
        self.ui.devices.selected_devices = vec![(chain, ids[next])];
    }

    pub(super) fn nav_command(&mut self, ctx: &egui::Context, c: Nav) {
        match c {
            Nav::NextControl => self.step_control(ctx, Dir::Next),
            Nav::PrevControl => self.step_control(ctx, Dir::Prev),
            Nav::NextNeighbor => self.step_control(ctx, Dir::NextNeighbor),
            Nav::PrevNeighbor => self.step_control(ctx, Dir::PrevNeighbor),
            Nav::ToggleTabFocus => {
                self.settings.tab_moves_focus = !self.settings.tab_moves_focus;
                self.settings.save();
            }
            Nav::ToggleWrapTab => {
                self.settings.wrap_tab = !self.settings.wrap_tab;
                self.settings.save();
            }
            Nav::FocusMixer => {
                // The Mixer's controls are in the Session grid; focus goes to wherever they're drawn.
                if let Some(track) = self.keyboard_track() {
                    let group = Group::Track(track);
                    for area in [FocusArea::Arrangement, FocusArea::Session] {
                        if nav::focus_group(ctx, group, area) {
                            self.ui.focus = area;
                            break;
                        }
                    }
                }
            }
            Nav::ValueUp => nav::queue(
                ctx,
                ValueOp::Step {
                    up: true,
                    fine: false,
                },
            ),
            Nav::ValueDown => nav::queue(
                ctx,
                ValueOp::Step {
                    up: false,
                    fine: false,
                },
            ),
            Nav::ValueUpFine => nav::queue(
                ctx,
                ValueOp::Step {
                    up: true,
                    fine: true,
                },
            ),
            Nav::ValueDownFine => nav::queue(
                ctx,
                ValueOp::Step {
                    up: false,
                    fine: true,
                },
            ),
            Nav::ValueDefault => nav::queue(ctx, ValueOp::Default),
            Nav::Digit0 => Self::digit(ctx, 0),
            Nav::Digit1 => Self::digit(ctx, 1),
            Nav::Digit2 => Self::digit(ctx, 2),
            Nav::Digit3 => Self::digit(ctx, 3),
            Nav::Digit4 => Self::digit(ctx, 4),
            Nav::Digit5 => Self::digit(ctx, 5),
            Nav::Digit6 => Self::digit(ctx, 6),
            Nav::Digit7 => Self::digit(ctx, 7),
            Nav::Digit8 => Self::digit(ctx, 8),
            Nav::Digit9 => Self::digit(ctx, 9),
            Nav::NextField => nav::type_char(ctx, nav::normalize_entry_char('.')),
            Nav::Minus => {
                if nav::entry(ctx).is_none() {
                    nav::type_char(ctx, '-');
                }
            }
            Nav::ValueCancel => self.value_cancel(ctx),
            Nav::ValueConfirm => nav::confirm(ctx),
            Nav::RenameNext => self.rename_next(),
            Nav::JumpToTitle => {
                if let Some(track) = self.keyboard_track() {
                    self.jump_to_title(track);
                }
            }
            Nav::DeviceLeft => self.step_device(-1),
            Nav::DeviceRight => self.step_device(1),
        }
    }
}
