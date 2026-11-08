//! The Options menu and the computer MIDI keyboard's own keys.

use super::{Entry, Entry::Item, MenuCommand as M, MenuState};
use crate::ui::RliveApp;

super::domain! {
    /// The Options menu.
    Options {
        ToggleComputerKeys,
        OctaveDown,
        OctaveUp,
        VelocityDown,
        VelocityUp,
        ToggleSoloInPlace,
        RescanPlugins,
    }
}

pub const MENU: &[Entry] = &[
    Item(
        "Computer MIDI Keyboard",
        M::Options(Options::ToggleComputerKeys),
    ),
    Item("Solo in Place", M::Options(Options::ToggleSoloInPlace)),
    Item("Rescan Plugins", M::Options(Options::RescanPlugins)),
];

/// How far C and V move the computer keyboard's velocity, as Live's steps of 20 out of 127.
const VELOCITY_STEP: f32 = 20.0 / 127.0;
const MIN_VELOCITY: f32 = 1.0 / 127.0;

impl RliveApp {
    pub(super) fn options_state(&self, s: &mut MenuState) {
        s.enable(
            M::Options(Options::RescanPlugins),
            self.ui.scan_rx.is_none(),
        );
        s.check(
            M::Options(Options::ToggleComputerKeys),
            self.ui.computer_keys,
        );
        s.check(
            M::Options(Options::ToggleSoloInPlace),
            self.session.project.solo_in_place,
        );
    }

    pub(super) fn options_command(&mut self, c: Options) {
        if c == Options::ToggleSoloInPlace {
            let on = !self.session.project.solo_in_place;
            self.session.set_solo_in_place(on);
            return;
        }
        let ui = &mut self.ui;
        match c {
            Options::ToggleComputerKeys => ui.computer_keys = !ui.computer_keys,
            Options::OctaveDown => ui.octave = (ui.octave - 1).max(-2),
            Options::OctaveUp => ui.octave = (ui.octave + 1).min(8),
            Options::VelocityDown => {
                ui.key_velocity = (ui.key_velocity - VELOCITY_STEP).max(MIN_VELOCITY)
            }
            Options::VelocityUp => ui.key_velocity = (ui.key_velocity + VELOCITY_STEP).min(1.0),
            Options::RescanPlugins => self.start_plugin_scan(),
            Options::ToggleSoloInPlace => {}
        }
    }
}
