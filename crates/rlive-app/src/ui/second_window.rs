//! The second window (manual pp. 199-201, 1014).
//!
//! It shows whichever of the Session and Arrangement Views the first window doesn't, so Tab swaps
//! them. The Detail View is in one window or the other: it opens in the window with the keyboard,
//! and F12 moves it across. The window is an egui immediate viewport drawn inside the first
//! window's frame, so it gets every frame the app gets. Its key events never reach eframe's input
//! hook, which only the first window has, so they are taken through the keymap here.

use super::{FocusRects, MainView, RliveApp};
use crate::ui::{arrangement, keymap::FocusArea, nav, session_view, theme::pal};
use egui::{Event, Key, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};

/// The window's title, which also tells the native menu it is one of ours.
pub const TITLE: &str = "rLive \u{2014} Second Window";

fn id() -> ViewportId {
    ViewportId::from_hash_of("rlive_second_window")
}

/// The other main view.
pub fn other(view: MainView) -> MainView {
    match view {
        MainView::Session => MainView::Arrangement,
        MainView::Arrangement => MainView::Session,
    }
}

impl RliveApp {
    /// Whether the Detail View is showing in the second window.
    pub(super) fn detail_in_second(&self) -> bool {
        let l = &self.session.project.window_layout;
        l.second_window && l.detail_in_second
    }

    pub(super) fn toggle_second_window(&mut self) {
        let l = &mut self.session.project.window_layout;
        l.second_window = !l.second_window;
        if !l.second_window {
            l.detail_in_second = false;
            self.ui.second_focused = false;
            self.ui.second_start = None;
        }
    }

    /// F12, while there are two windows: the Detail View goes to the other one.
    pub(super) fn move_detail_between_windows(&mut self) {
        let l = &mut self.session.project.window_layout;
        if l.second_window {
            l.detail_in_second = !l.detail_in_second;
            self.ui.show_detail = true;
        }
    }

    /// Opening the Detail View puts it in the window with the keyboard.
    pub(super) fn open_detail_in_focused_window(&mut self) {
        let focused = self.ui.second_focused;
        let l = &mut self.session.project.window_layout;
        if l.second_window {
            l.detail_in_second = focused;
        }
    }

    /// Moves the operating system's focus to the window showing `view`.
    pub(super) fn focus_window_of(&self, ctx: &egui::Context, view: MainView) {
        if !self.session.project.window_layout.second_window {
            return;
        }
        let id = if view == self.ui.main_view {
            ViewportId::ROOT
        } else {
            id()
        };
        ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
    }

    /// Shows the second window, if it is open. Call once a frame from the first window.
    pub(super) fn second_window(&mut self, ctx: &egui::Context) {
        self.ui.second_pointer_busy = false;
        let layout = self.session.project.window_layout.clone();
        if !layout.second_window {
            return;
        }
        // Set once: the builder is compared each frame, and a change would fight the user's drags.
        let start = *self
            .ui
            .second_start
            .get_or_insert(layout.second_rect.unwrap_or([120.0, 120.0, 1000.0, 680.0]));
        let builder = ViewportBuilder::default()
            .with_title(TITLE)
            .with_position([start[0], start[1]])
            .with_inner_size([start[2], start[3]])
            .with_min_inner_size([480.0, 320.0]);
        ctx.show_viewport_immediate(id(), builder, |ui, class| self.second_window_ui(ui, class));
    }

    fn second_window_ui(&mut self, ui: &mut egui::Ui, class: ViewportClass) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && class != ViewportClass::EmbeddedWindow {
            self.toggle_second_window();
            return;
        }
        let (outer, inner, focused, pointer_down) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.inner_rect,
                v.focused.unwrap_or(false),
                i.pointer.any_down(),
            )
        });
        if let (Some(o), Some(r)) = (outer, inner) {
            let rect = [o.min.x, o.min.y, r.width(), r.height()];
            if r.width() > 1.0 {
                self.session.project.window_layout.second_rect = Some(rect);
            }
        }
        self.ui.second_focused = focused;
        if focused {
            self.take_second_window_keys(&ctx);
            self.handle_shortcuts(&ctx);
        }

        let view = other(self.ui.main_view);
        let area = match view {
            MainView::Session => FocusArea::Session,
            MainView::Arrangement => FocusArea::Arrangement,
        };
        let mut rects = FocusRects {
            main_view: Some(view),
            ..FocusRects::default()
        };
        if self.detail_in_second() {
            self.detail_panel(ui, &ctx, &mut rects, true);
        }
        let panel = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal().grid_bg))
            .show(ui, |ui| {
                nav::set_area(ui.ctx(), area);
                match view {
                    MainView::Session => session_view::show(ui, &mut self.session, &mut self.ui),
                    MainView::Arrangement => arrangement::show(ui, &mut self.session, &mut self.ui),
                }
            });
        rects.main = Some(panel.response.rect);
        self.track_focus(&ctx, &rects);
        self.paint_focus_outline(&ctx, &rects);
        self.ui.second_pointer_busy = pointer_down || ctx.text_edit_focused();
    }

    /// Runs this window's key events through the keymap, as the input hook does for the first.
    fn take_second_window_keys(&mut self, ctx: &egui::Context) {
        let mut events = ctx.input_mut(|i| std::mem::take(&mut i.events));
        let tab_seen = events
            .iter()
            .any(|e| matches!(e, Event::Key { key: Key::Tab, .. }));
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.mac_menu {
            menu.drain_into(ctx.text_edit_focused(), &mut events, &mut self.pending);
        }
        self.filter_key_events(ctx, &mut events);
        let tab_kept = events
            .iter()
            .any(|e| matches!(e, Event::Key { key: Key::Tab, .. }));
        // egui has already moved its keyboard focus for this frame's Tab. Taking the Tab away
        // afterwards doesn't undo that, and a focused widget silences every bare-key shortcut.
        if tab_seen && !tab_kept && !ctx.text_edit_focused() {
            ctx.memory_mut(|m| {
                if let Some(f) = m.focused() {
                    m.surrender_focus(f);
                }
            });
        }
        ctx.input_mut(|i| i.events = events);
        for c in std::mem::take(&mut self.pending) {
            self.dispatch(ctx, c);
        }
    }
}
