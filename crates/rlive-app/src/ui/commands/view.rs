//! Commands that show, hide and move between parts of the window.

use super::{Entry, Entry::Item, Entry::Separator, MenuCommand as M, MenuState};
use crate::ui::keymap::FocusArea;
use crate::ui::{Detail, MainView, RliveApp};

super::domain! {
    /// The View menu, and the Navigate commands that move keyboard focus.
    View {
        ToggleMainView,
        ToggleAutomationMode,
        SwitchDetail,
        ToggleBrowser,
        ToggleDetail,
        ToggleMixer,
        ToggleInOut,
        ToggleCrossfader,
        ToggleInfoView,
        ToggleClipView,
        ToggleDeviceView,
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
        CloseDialog,
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
    Item("Browser", M::View(View::ToggleBrowser)),
    Item("Detail View", M::View(View::ToggleDetail)),
    Item("Mixer", M::View(View::ToggleMixer)),
    Item("In/Out", M::View(View::ToggleInOut)),
    Item("Crossfader", M::View(View::ToggleCrossfader)),
    Item("Info View", M::View(View::ToggleInfoView)),
    Separator,
    Item("Zoom In", M::View(View::ZoomIn)),
    Item("Zoom Out", M::View(View::ZoomOut)),
    Item("Actual Size", M::View(View::ZoomReset)),
];

impl RliveApp {
    pub(super) fn view_state(&self, s: &mut MenuState) {
        s.check(M::View(View::ToggleAutomationMode), self.ui.arr.auto.mode);
        s.check(M::View(View::ToggleBrowser), self.ui.show_browser);
        s.check(M::View(View::ToggleDetail), self.ui.show_detail);
        s.check(M::View(View::ToggleMixer), self.ui.layout.show_mixer);
        s.check(M::View(View::ToggleInOut), self.ui.layout.show_in_out);
        s.check(
            M::View(View::ToggleCrossfader),
            self.ui.layout.show_crossfader,
        );
        s.check(M::View(View::ToggleInfoView), self.ui.show_info);
    }

    /// Shows the Detail View on one of its two sides, or hides it if that side is showing.
    fn toggle_detail_side(&mut self, side: Detail) {
        if side == Detail::Clip && self.ui.selected_clip.is_none() {
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

    fn move_focus(&mut self, area: FocusArea) {
        match area {
            FocusArea::Session => self.ui.main_view = MainView::Session,
            FocusArea::Arrangement => self.ui.main_view = MainView::Arrangement,
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
            View::SwitchDetail => {
                self.ui.show_detail = true;
                self.ui.detail = match self.ui.detail {
                    Detail::Devices if self.ui.selected_clip.is_some() => Detail::Clip,
                    _ => Detail::Devices,
                };
            }
            View::ToggleBrowser => self.toggle_browser(),
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
            View::FocusControlBar => self.move_focus(FocusArea::ControlBar),
            View::FocusSession => self.move_focus(FocusArea::Session),
            View::FocusArrangement => self.move_focus(FocusArea::Arrangement),
            View::FocusClip => self.move_focus(FocusArea::Clip),
            View::FocusDevice => self.move_focus(FocusArea::Device),
            View::FocusBrowser => self.move_focus(FocusArea::Browser),
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
