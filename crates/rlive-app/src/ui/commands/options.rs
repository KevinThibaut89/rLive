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
        ToggleDelayCompensation,
        ToggleReducedLatency,
        RescanPlugins,
        ToggleDrawMode,
        ToggleLockEnvelopes,
        ToggleMidiEnvelopeAutoReset,
        ToggleChaseNotes,
    }
}

pub const MENU: &[Entry] = &[
    Item(
        "Computer MIDI Keyboard",
        M::Options(Options::ToggleComputerKeys),
    ),
    Item("Solo in Place", M::Options(Options::ToggleSoloInPlace)),
    Item(
        "Delay Compensation",
        M::Options(Options::ToggleDelayCompensation),
    ),
    Item(
        "Reduced Latency When Monitoring",
        M::Options(Options::ToggleReducedLatency),
    ),
    Item("Draw Mode", M::Options(Options::ToggleDrawMode)),
    Item("Lock Envelopes", M::Options(Options::ToggleLockEnvelopes)),
    Item(
        "MIDI Envelope Auto-Reset",
        M::Options(Options::ToggleMidiEnvelopeAutoReset),
    ),
    Item("Chase MIDI Notes", M::Options(Options::ToggleChaseNotes)),
    Item("Rescan Plugins", M::Options(Options::RescanPlugins)),
];

/// How far C and V move the computer keyboard's velocity, as Live's steps of 20 out of 127.
const VELOCITY_STEP: f32 = 20.0 / 127.0;
const MIN_VELOCITY: f32 = 1.0 / 127.0;

impl RliveApp {
    pub(super) fn options_state(&self, s: &mut MenuState) {
        s.check(M::Options(Options::ToggleDrawMode), self.ui.arr.auto.draw);
        s.check(
            M::Options(Options::ToggleMidiEnvelopeAutoReset),
            self.settings.midi_envelope_auto_reset,
        );
        s.check(
            M::Options(Options::ToggleLockEnvelopes),
            self.session.project.arrangement.lock_envelopes,
        );
        s.check(
            M::Options(Options::ToggleChaseNotes),
            self.settings.chase_notes,
        );
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
        let project = &self.session.project;
        s.check(
            M::Options(Options::ToggleDelayCompensation),
            project.delay_compensation,
        );
        s.check(
            M::Options(Options::ToggleReducedLatency),
            project.reduced_latency_when_monitoring,
        );
        s.enable(
            M::Options(Options::ToggleReducedLatency),
            project.delay_compensation,
        );
    }

    pub(super) fn options_command(&mut self, c: Options) {
        match c {
            Options::ToggleSoloInPlace => {
                let on = !self.session.project.solo_in_place;
                return self.session.set_solo_in_place(on);
            }
            Options::ToggleDelayCompensation => {
                let on = !self.session.project.delay_compensation;
                return self.session.set_delay_compensation(on);
            }
            Options::ToggleReducedLatency => {
                let on = !self.session.project.reduced_latency_when_monitoring;
                return self.session.set_reduced_latency_when_monitoring(on);
            }
            Options::ToggleLockEnvelopes => {
                let on = !self.session.project.arrangement.lock_envelopes;
                return self.session.set_lock_envelopes(on);
            }
            Options::ToggleMidiEnvelopeAutoReset => {
                let on = !self.settings.midi_envelope_auto_reset;
                self.settings.midi_envelope_auto_reset = on;
                self.settings.save();
                self.session.send_midi_envelope_auto_reset(on);
                return;
            }
            Options::ToggleChaseNotes => {
                self.settings.chase_notes = !self.settings.chase_notes;
                self.session.set_chase(self.settings.chase_notes);
                self.settings.save();
                return;
            }
            _ => {}
        }
        let ui = &mut self.ui;
        match c {
            Options::ToggleComputerKeys => ui.computer_keys = !ui.computer_keys,
            Options::ToggleDrawMode => ui.arr.auto.draw = !ui.arr.auto.draw,
            Options::OctaveDown => ui.octave = (ui.octave - 1).max(-2),
            Options::OctaveUp => ui.octave = (ui.octave + 1).min(8),
            Options::VelocityDown => {
                ui.key_velocity = (ui.key_velocity - VELOCITY_STEP).max(MIN_VELOCITY)
            }
            Options::VelocityUp => ui.key_velocity = (ui.key_velocity + VELOCITY_STEP).min(1.0),
            Options::RescanPlugins => self.start_plugin_scan(),
            Options::ToggleSoloInPlace
            | Options::ToggleDelayCompensation
            | Options::ToggleReducedLatency
            | Options::ToggleLockEnvelopes
            | Options::ToggleMidiEnvelopeAutoReset
            | Options::ToggleChaseNotes => {}
        }
    }
}
