//! Commands that show, hide and move between parts of the window.

use super::{Entry, Entry::Item, Entry::Separator, MenuCommand as M, MenuState};
use crate::session::ChainRef;
use crate::ui::keymap::FocusArea;
use crate::ui::{Detail, MainView, RliveApp};

super::domain! {
    /// The View menu, and the Navigate commands that move keyboard focus.
    View {
        ToggleMainView,
        ToggleAutomationMode,
        SwitchDetail,
        ToggleBrowser,
        ToggleFullHeightBrowser,
        ToggleDetail,
        ToggleMixer,
        ToggleInOut,
        ToggleCrossfader,
        ToggleInfoView,
        ToggleClipView,
        ToggleDeviceView,
        ToggleGroovePool,
        ToggleTuning,
        ToggleFullScreen,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        RulerZoomIn,
        RulerZoomOut,
        FocusControlBar,
        FocusSession,
        FocusArrangement,
        FocusClip,
        FocusDevice,
        FocusBrowser,
        FocusGroovePool,
        CloseDialog,
        TogglePluginWindows,
        ToggleSecondWindow,
        ToggleSends,
        ToggleReturns,
    }
}

pub const MENU: &[Entry] = &[
    Item("Session / Arrangement (Tab)", M::View(View::ToggleMainView)),
    Item("Automation Mode", M::View(View::ToggleAutomationMode)),
    Item(
        "Clip / Device View (Shift+Tab)",
        M::View(View::SwitchDetail),
    ),
    Separator,
    Item("Second Window", M::View(View::ToggleSecondWindow)),
    Item("Browser", M::View(View::ToggleBrowser)),
    Item("Groove Pool", M::View(View::ToggleGroovePool)),
    Item("Tuning", M::View(View::ToggleTuning)),
    Item("File Manager", M::File(super::file::File::ManageFiles)),
    Item("Detail View", M::View(View::ToggleDetail)),
    Item("Mixer", M::View(View::ToggleMixer)),
    Item("In/Out", M::View(View::ToggleInOut)),
    Item("Sends", M::View(View::ToggleSends)),
    Item("Return Tracks", M::View(View::ToggleReturns)),
    Item("Crossfader", M::View(View::ToggleCrossfader)),
    Item("Info View", M::View(View::ToggleInfoView)),
    Item(
        "Full-Height Browser",
        M::View(View::ToggleFullHeightBrowser),
    ),
    Item(
        "Undo History",
        M::Browser(crate::ui::commands::browser::Browser::ToggleUndoHistory),
    ),
    Item(
        "Hide/Show Plug-In Windows",
        M::View(View::TogglePluginWindows),
    ),
    Separator,
    Item("Zoom In", M::View(View::ZoomIn)),
    Item("Zoom Out", M::View(View::ZoomOut)),
    Item("Actual Size", M::View(View::ZoomReset)),
];

impl RliveApp {
    pub(super) fn view_state(&self, s: &mut MenuState) {
        s.check(M::View(View::ToggleAutomationMode), self.ui.arr.auto.mode);
        s.check(M::View(View::ToggleBrowser), self.ui.show_browser);
        s.check(M::View(View::ToggleGroovePool), self.ui.show_groove_pool);
        s.check(M::View(View::ToggleTuning), self.ui.show_tuning);
        s.check(
            M::View(View::ToggleFullHeightBrowser),
            self.settings.full_height_browser,
        );
        s.check(M::View(View::ToggleDetail), self.ui.show_detail);
        s.check(M::View(View::ToggleMixer), self.ui.layout.show_mixer);
        s.check(M::View(View::ToggleInOut), self.ui.layout.show_in_out);
        s.check(
            M::View(View::ToggleCrossfader),
            self.ui.layout.show_crossfader,
        );
        s.check(M::View(View::ToggleInfoView), self.ui.show_info);
        let l = &self.session.project.window_layout;
        s.check(M::View(View::ToggleSecondWindow), l.second_window);
        s.check(M::View(View::ToggleSends), l.show_sends);
        s.check(M::View(View::ToggleReturns), l.show_returns);
    }

    /// Shows the Detail View on one of its two sides, or hides it if that side is showing.
    fn toggle_detail_side(&mut self, side: Detail) {
        if side == Detail::Clip && self.ui.selected_clip.is_none() {
            return;
        }
        // The Clip View goes on top of a Device View that's open (manual p. 200).
        if side == Detail::Clip
            && self.ui.show_detail
            && self.ui.detail == Detail::Devices
            && !self.ui.clip_view.stacked
        {
            self.ui.clip_view.stacked = true;
            return;
        }
        if self.ui.clip_view.stacked {
            self.ui.clip_view.stacked = false;
            self.ui.detail = side;
            return;
        }
        if self.ui.show_detail && self.ui.detail == side {
            self.toggle_detail();
        } else {
            if !self.ui.show_detail {
                self.toggle_detail();
            }
            self.ui.detail = side;
        }
    }

    fn move_focus(&mut self, ctx: &egui::Context, area: FocusArea) {
        match area {
            FocusArea::Session | FocusArea::Arrangement => {
                let view = if area == FocusArea::Session {
                    MainView::Session
                } else {
                    MainView::Arrangement
                };
                if self.session.project.window_layout.second_window {
                    // Both views are showing: bring up the window that has this one.
                    self.focus_window_of(ctx, view);
                } else {
                    self.ui.main_view = view;
                }
                // The arrow keys need somewhere to start from.
                if area == FocusArea::Session
                    && self.ui.selected_clip.is_none()
                    && self.ui.sel.slots.is_empty()
                    && self.ui.sel.scenes.is_empty()
                    && self.ui.title_focus.is_none()
                {
                    self.ui.title_focus = match self.ui.selected {
                        Some(ChainRef::Track(t)) => Some(t),
                        _ => self.session.project.tracks.first().map(|t| t.id),
                    };
                }
            }
            FocusArea::Clip | FocusArea::Device => {
                if !self.ui.show_detail {
                    self.toggle_detail();
                }
                self.ui.detail = if area == FocusArea::Clip && self.ui.selected_clip.is_some() {
                    Detail::Clip
                } else {
                    Detail::Devices
                };
            }
            FocusArea::Browser => {
                if !self.ui.show_browser {
                    self.toggle_browser();
                }
            }
            FocusArea::ControlBar => {}
        }
        self.ui.focus = area;
    }

    pub(super) fn view_command(&mut self, ctx: &egui::Context, c: View) {
        let zoom = ctx.zoom_factor();
        match c {
            View::TogglePluginWindows => self.session.toggle_plugin_windows(),
            View::ToggleMainView => {
                self.ui.main_view = match self.ui.main_view {
                    MainView::Session => MainView::Arrangement,
                    MainView::Arrangement => MainView::Session,
                };
            }
            View::ToggleAutomationMode => {
                self.ui.arr.auto.mode = !self.ui.arr.auto.mode;
                self.ui.main_view = MainView::Arrangement;
            }
            View::ToggleSecondWindow => self.toggle_second_window(),
            View::ToggleSends => {
                let l = &mut self.session.project.window_layout;
                l.show_sends = !l.show_sends;
            }
            View::ToggleReturns => {
                let l = &mut self.session.project.window_layout;
                l.show_returns = !l.show_returns;
            }
            // With two windows, F12 and Shift+Tab move the Detail View across (manual p. 200).
            View::SwitchDetail if self.session.project.window_layout.second_window => {
                self.move_detail_between_windows();
            }
            View::SwitchDetail => {
                self.ui.show_detail = true;
                self.ui.detail = match self.ui.detail {
                    Detail::Devices if self.ui.selected_clip.is_some() => Detail::Clip,
                    _ => Detail::Devices,
                };
            }
            View::ToggleBrowser => self.toggle_browser(),
            View::ToggleGroovePool => {
                self.ui.show_groove_pool = !self.ui.show_groove_pool;
                if self.ui.show_groove_pool {
                    self.ui.show_browser = true;
                }
            }
            View::ToggleTuning => {
                self.ui.show_tuning = !self.ui.show_tuning;
                if self.ui.show_tuning {
                    self.ui.show_browser = true;
                }
            }
            View::FocusGroovePool => {
                self.ui.show_groove_pool = true;
                self.ui.show_browser = true;
                self.move_focus(ctx, FocusArea::Browser);
            }
            View::ToggleFullHeightBrowser => {
                self.settings.full_height_browser = !self.settings.full_height_browser;
                self.settings.save();
            }
            View::ToggleDetail => self.toggle_detail(),
            View::ToggleMixer => self.ui.layout.show_mixer = !self.ui.layout.show_mixer,
            View::ToggleInOut => self.ui.layout.show_in_out = !self.ui.layout.show_in_out,
            View::ToggleCrossfader => {
                self.ui.layout.show_crossfader = !self.ui.layout.show_crossfader
            }
            View::ToggleInfoView => self.toggle_info_view(),
            View::ToggleClipView => self.toggle_detail_side(Detail::Clip),
            View::ToggleDeviceView => self.toggle_detail_side(Detail::Devices),
            View::ToggleFullScreen => {
                let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
            }
            View::ZoomIn => self.set_zoom(ctx, zoom + 0.1),
            View::ZoomOut => self.set_zoom(ctx, zoom - 0.1),
            View::ZoomReset => self.set_zoom(ctx, 1.0),
            View::RulerZoomIn => self.ui.arr.zoom_ruler(1.25),
            View::RulerZoomOut => self.ui.arr.zoom_ruler(0.8),
            View::FocusControlBar => self.move_focus(ctx, FocusArea::ControlBar),
            View::FocusSession => self.move_focus(ctx, FocusArea::Session),
            View::FocusArrangement => self.move_focus(ctx, FocusArea::Arrangement),
            View::FocusClip => self.move_focus(ctx, FocusArea::Clip),
            View::FocusDevice => self.move_focus(ctx, FocusArea::Device),
            View::FocusBrowser => self.move_focus(ctx, FocusArea::Browser),
            View::CloseDialog => self.close_top_dialog(),
        }
    }

    /// Closes the frontmost window, if one is open.
    fn close_top_dialog(&mut self) {
        let ui = &mut self.ui;
        for open in [
            &mut ui.show_about,
            &mut ui.show_export,
            &mut ui.show_settings,
        ] {
            if std::mem::take(open) {
                return;
            }
        }
    }
}
