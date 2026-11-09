//! Commands of the application itself.

use super::{Entry, Entry::Item, MenuCommand as M};
use crate::ui::RliveApp;

super::domain! {
    /// The app menu.
    App { About, Preferences, Quit }
}

/// Items for menus that list these commands one at a time, in menu order.
pub const ABOUT: Entry = Item("About rLive", M::App(App::About));
pub const PREFERENCES: Entry = Item("Settings…", M::App(App::Preferences));
pub const QUIT: Entry = Item("Quit", M::App(App::Quit));

impl RliveApp {
    pub(super) fn app_command(&mut self, ctx: &egui::Context, c: App) {
        match c {
            App::About => self.ui.show_about = true,
            App::Preferences => {
                self.ui.show_settings = true;
                self.ui.audio_devices = None;
                self.ui.midi_ports = None;
                self.ui.midi_outputs = None;
            }
            App::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}
