//! The browser's commands, for the keymap and menus.

use super::{Entry, Entry::Item, MenuCommand as M};
use crate::ui::browser::{self, exit_hot_swap, toggle_drum_pad_target, toggle_hot_swap};
use crate::ui::keymap::FocusArea;
use crate::ui::{Detail, RliveApp};
use rlive_browser::model::{Move, Pane};
use rlive_browser::Op;

super::domain! {
    /// The browser, Hot-Swap and device A/B.
    Browser {
        MoveUp,
        MoveDown,
        ExtendUp,
        ExtendDown,
        CloseFolder,
        OpenFolder,
        OpenFolderKeep,
        Load,
        Preview,
        Search,
        Color1,
        Color2,
        Color3,
        Color4,
        Color5,
        Color6,
        Color7,
        ClearColors,
        HistoryBack,
        HistoryForward,
        ToggleFilterView,
        ToggleTagEditor,
        ToggleHotSwap,
        ToggleDrumPad,
        CompareSwitch,
        CompareCopy,
        ToggleUndoHistory,
    }
}

pub const VIEW_ITEMS: &[Entry] = &[Item("Undo History", M::Browser(Browser::ToggleUndoHistory))];

impl RliveApp {
    /// The device a device-level command applies to: the one selected, or the only one.
    fn focused_device(&self) -> Option<(crate::session::ChainRef, rlive_core::DeviceId)> {
        if let Some(&selected) = self.ui.devices.selected_devices.last() {
            return Some(selected);
        }
        let chain = self.ui.selected?;
        let devices = self.session.devices(chain);
        (devices.len() == 1).then(|| (chain, devices[0].id))
    }

    pub(super) fn browser_command(&mut self, c: Browser) {
        // With the sidebar in focus the arrows move through its labels (manual p. 117).
        if self.ui.browser.model.pane == Pane::Sidebar {
            match c {
                Browser::MoveUp => {
                    self.ui.browser.sidebar_step(-1);
                    return;
                }
                Browser::MoveDown => {
                    self.ui.browser.sidebar_step(1);
                    return;
                }
                Browser::OpenFolder => {
                    self.ui.browser.model.pane = Pane::Content;
                    return;
                }
                _ => {}
            }
        }
        let model = &mut self.ui.browser.model;
        match c {
            Browser::MoveUp | Browser::MoveDown | Browser::ExtendUp | Browser::ExtendDown => {
                let delta = if matches!(c, Browser::MoveUp | Browser::ExtendUp) {
                    -1
                } else {
                    1
                };
                let extend = matches!(c, Browser::ExtendUp | Browser::ExtendDown);
                model.move_cursor(delta, extend);
                self.ui.browser.scroll_to_cursor = true;
                self.ui.browser.apply_after_move = !extend;
            }
            Browser::OpenFolder => match model.right(false) {
                Move::Preview => browser::preview_selected(&mut self.session, &mut self.ui),
                _ => self.ui.browser.scroll_to_cursor = true,
            },
            Browser::OpenFolderKeep => {
                model.right(true);
                self.ui.browser.scroll_to_cursor = true;
            }
            Browser::CloseFolder => {
                model.left();
                self.ui.browser.scroll_to_cursor = true;
            }
            Browser::Load => browser::apply_selection(&mut self.session, &mut self.ui),
            Browser::Preview => browser::preview_selected(&mut self.session, &mut self.ui),
            Browser::Search => {
                model.focus_search();
                self.ui.browser.focus_search = true;
                self.ui.show_browser = true;
                self.ui.focus = FocusArea::Browser;
            }
            Browser::Color1 => self.ui.browser.assign_color(Some(0)),
            Browser::Color2 => self.ui.browser.assign_color(Some(1)),
            Browser::Color3 => self.ui.browser.assign_color(Some(2)),
            Browser::Color4 => self.ui.browser.assign_color(Some(3)),
            Browser::Color5 => self.ui.browser.assign_color(Some(4)),
            Browser::Color6 => self.ui.browser.assign_color(Some(5)),
            Browser::Color7 => self.ui.browser.assign_color(Some(6)),
            Browser::ClearColors => self.ui.browser.assign_color(None),
            Browser::HistoryBack => model.back(),
            Browser::HistoryForward => model.forward(),
            Browser::ToggleFilterView => {
                if let Some(lib) = self.ui.browser.db.as_ref().map(|d| d.library().clone()) {
                    self.ui.browser.op(Op::SetShowFilters(!lib.show_filters));
                }
            }
            Browser::ToggleTagEditor => {
                if let Some(lib) = self.ui.browser.db.as_ref().map(|d| d.library().clone()) {
                    self.ui
                        .browser
                        .op(Op::SetShowTagEditor(!lib.show_tag_editor));
                }
            }
            Browser::ToggleHotSwap => toggle_hot_swap(&self.session, &mut self.ui),
            Browser::ToggleDrumPad => toggle_drum_pad_target(&mut self.ui),
            Browser::CompareSwitch => {
                if let Some((chain, id)) = self.focused_device() {
                    self.session.compare_switch(chain, id);
                }
            }
            Browser::CompareCopy => {
                if let Some((chain, id)) = self.focused_device() {
                    self.session.compare_copy(chain, id);
                }
            }
            Browser::ToggleUndoHistory => self.ui.show_undo_history = !self.ui.show_undo_history,
        }
    }

    /// Hot-Swap ends when the keyboard goes off to a view that isn't part of it.
    pub(in crate::ui) fn end_hot_swap_on_navigation(&mut self) {
        let in_flow = matches!(self.ui.focus, FocusArea::Browser | FocusArea::Device);
        let detail_shown = self.ui.show_detail && self.ui.detail == Detail::Devices;
        if self.ui.browser.hot_swap.is_some() && (!in_flow || !detail_shown) {
            exit_hot_swap(&mut self.ui);
        }
    }
}
