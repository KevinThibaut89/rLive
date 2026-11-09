//! MIDI Map Mode, Key Map Mode, and what mapped controls do that needs the UI.

use super::transport::Transport as Tr;
use super::{Entry, Entry::Item, MenuCommand as M};
use crate::session::{ClipRef, MapMode};
use crate::ui::RliveApp;
use rlive_project::{MapTarget, RemoteControl as C};
use rlive_remote::Gesture;

super::domain! {
    Remote { ToggleMidiMap, ToggleKeyMap }
}

pub const MENU: &[Entry] = &[
    Item("MIDI Map Mode", M::Remote(Remote::ToggleMidiMap)),
    Item("Key Map Mode", M::Remote(Remote::ToggleKeyMap)),
];

/// The command a mapped control runs, for those that are exactly a menu command.
pub fn control_command(c: C) -> Option<M> {
    use super::arrange::Arrange as A;
    Some(match c {
        C::Continue => M::Transport(Tr::Continue),
        C::Record => M::Transport(Tr::Record),
        C::SessionRecord => M::Transport(Tr::SessionRecord),
        C::NewTake => M::Transport(Tr::NewTake),
        C::CaptureMidi => M::Transport(Tr::CaptureMidi),
        C::BackToArrangement => M::Transport(Tr::BackToArrangement),
        C::PreviousLocator => M::Arrange(A::PreviousLocator),
        C::NextLocator => M::Arrange(A::NextLocator),
        _ => return None,
    })
}

impl RliveApp {
    pub(super) fn remote_state(&self, s: &mut super::MenuState) {
        s.check(
            M::Remote(Remote::ToggleMidiMap),
            self.session.map_mode() == Some(MapMode::Midi),
        );
        s.check(
            M::Remote(Remote::ToggleKeyMap),
            self.session.map_mode() == Some(MapMode::Key),
        );
    }

    pub(super) fn remote_command(&mut self, c: Remote) {
        let mode = match c {
            Remote::ToggleMidiMap => MapMode::Midi,
            Remote::ToggleKeyMap => MapMode::Key,
        };
        let next = (self.session.map_mode() != Some(mode)).then_some(mode);
        self.session.set_map_mode(next);
    }

    /// What a mapped control says to do, for those depending on the UI's selection.
    pub(crate) fn remote_action(&mut self, target: MapTarget, g: Gesture) {
        match target {
            MapTarget::HighlightedSlot(track) => {
                let slot = self.selected_scene();
                self.session
                    .apply_gesture(MapTarget::ClipSlot { track, slot }, g);
            }
            MapTarget::Control(c) => self.remote_control(c, g),
            _ => {}
        }
    }

    /// Whether a switch should change, given what's said and how it stands.
    fn wants(g: Gesture, on: bool) -> Option<bool> {
        match g {
            Gesture::Toggle | Gesture::Press(_) => Some(!on),
            Gesture::On => Some(true),
            Gesture::Off => Some(false),
            _ => None,
        }
    }

    fn remote_control(&mut self, c: C, g: Gesture) {
        let pressed = matches!(g, Gesture::Press(_));
        if let Some(command) = control_command(c) {
            let changes = match c {
                C::Record => self
                    .session
                    .control_state(c)
                    .is_some_and(|on| Self::wants(g, on).is_some_and(|w| w != on)),
                C::SessionRecord => !matches!(g, Gesture::Release),
                _ => pressed,
            };
            if changes {
                self.pending.push(command);
            }
            return;
        }
        let a = self.session.project.arrangement.clone();
        match c {
            C::Play => {
                let on = self.session.is_playing();
                match Self::wants(g, on) {
                    Some(true) if !on => self.session.play(),
                    Some(false) if on => self.session.stop(),
                    _ => {}
                }
            }
            C::Stop if pressed => self.session.stop(),
            C::MidiOverdub => {
                let on = self.session.project.midi_arrangement_overdub;
                if let Some(w) = Self::wants(g, on) {
                    self.session.set_midi_arrangement_overdub(w);
                }
            }
            C::AutomationArm => {
                let on = self.session.automation_arm();
                if let Some(w) = Self::wants(g, on) {
                    self.session.set_automation_arm(w);
                }
            }
            C::ReEnableAutomation if pressed => self.session.re_enable_all(),
            C::Metronome => {
                let on = self.session.project.metronome;
                if let Some(w) = Self::wants(g, on) {
                    self.session.set_metronome(w);
                }
            }
            C::TapTempo if pressed => {
                let now = self.ui.frame_time;
                self.tap_tempo(now);
            }
            C::Loop => {
                if let Some(w) = Self::wants(g, a.loop_enabled) {
                    self.session.set_loop(w, a.loop_start, a.loop_end);
                }
            }
            C::PunchIn => {
                if let Some(w) = Self::wants(g, a.punch_in) {
                    self.session.set_punch(w, a.punch_out);
                }
            }
            C::PunchOut => {
                if let Some(w) = Self::wants(g, a.punch_out) {
                    self.session.set_punch(a.punch_in, w);
                }
            }
            C::Follow => {
                let on = self.ui.arr.follow.on;
                if let Some(w) = Self::wants(g, on) {
                    self.ui.arr.follow.on = w;
                }
            }
            C::FollowActions => {
                let on = self.session.project.follow_actions_on;
                if let Some(w) = Self::wants(g, on) {
                    self.session.set_follow_actions_on(w);
                }
            }
            C::StopAllClips if pressed => self.session.stop_all_clips(),
            C::SceneUp if pressed => self.step_scene(-1),
            C::SceneDown if pressed => self.step_scene(1),
            C::SceneScroll => match g {
                Gesture::Step(d) => self.step_scene(d),
                Gesture::Absolute(v) => {
                    let last = self.session.project.scenes.len().saturating_sub(1);
                    self.select_scene((v as f64 * last as f64).round() as usize);
                }
                _ => {}
            },
            C::LaunchHighlightedScene if pressed => {
                let scene = self.selected_scene();
                crate::ui::session_view::launch_selected_scene(
                    &mut self.session,
                    &mut self.ui,
                    scene,
                );
            }
            C::CancelSceneLaunch if pressed => self.session.cancel_scene_launch(),
            C::SetLocator if pressed => {
                let at = self.session.engine.shared.tick.load() as u64;
                self.session.set_locator(at);
            }
            C::StepForward if pressed => self.step_record(true),
            C::StepBack if pressed => self.step_record(false),
            C::PhaseNudgeUp | C::PhaseNudgeDown => {
                let held = match g {
                    Gesture::Press(_) => true,
                    Gesture::Absolute(v) => v > 0.5,
                    _ => false,
                };
                let direction = i8::from(held) * if c == C::PhaseNudgeUp { 1 } else { -1 };
                self.session.set_phase_nudge(direction);
            }
            C::ExternalSync => {
                let on = self.session.external_sync();
                if let Some(w) = Self::wants(g, on) {
                    self.set_external_sync(w);
                }
            }
            C::TempoFollower => {
                let on = self.session.tempo_follower();
                if let Some(w) = Self::wants(g, on) {
                    self.set_tempo_follower(w);
                }
            }
            C::NudgeBack if pressed => self.nudge_selected(false, 1),
            C::NudgeForward if pressed => self.nudge_selected(true, 1),
            C::Scrub => {
                if let Gesture::Step(d) = g {
                    self.nudge_selected(d > 0, d.unsigned_abs().min(8));
                }
            }
            C::ClipLoop => {
                if let Some(r) = self.ui.selected_clip {
                    let on = self.session.clip_at(r).is_some_and(|c| c.looped);
                    if let Some(w) = Self::wants(g, on) {
                        self.session.set_clip_looped(&[r], w);
                    }
                }
            }
            C::ClipLoopPosition | C::ClipLoopLength => self.nudge_clip_loop(c, g),
            _ => {}
        }
    }

    fn step_scene(&mut self, delta: i32) {
        let last = self.session.project.scenes.len().saturating_sub(1) as i64;
        let at = self.selected_scene() as i64 + i64::from(delta);
        self.select_scene(at.clamp(0, last) as usize);
    }

    fn step_record(&mut self, forward: bool) {
        let Some(r) = self.ui.selected_clip else {
            return;
        };
        let ps = &mut self.ui.piano;
        let at = ps.insert_tick.unwrap_or(0);
        ps.insert_tick = Some(if forward {
            self.session.step_forward(r, at, ps.grid)
        } else {
            self.session.step_back(r, at, ps.grid)
        });
    }

    fn nudge_selected(&mut self, forward: bool, times: u32) {
        let Some(track) = self.ui.selected_clip.map(ClipRef::track) else {
            return;
        };
        for _ in 0..times {
            self.session.nudge_clip(track, forward);
        }
    }

    /// An encoder moves the selected clip's loop by the grid; a fader isn't meaningful here.
    fn nudge_clip_loop(&mut self, c: C, g: Gesture) {
        let Gesture::Step(d) = g else {
            return;
        };
        let Some(r) = self.ui.selected_clip else {
            return;
        };
        let grid = self.ui.piano.grid.max(1) as i64;
        self.session.edit_clips(&[r], "Clip Loop", |clip| {
            let moved = |v: u64| (v as i64 + i64::from(d) * grid).max(0) as u64;
            if c == C::ClipLoopPosition {
                clip.loop_start = moved(clip.loop_start);
            } else {
                clip.length = moved(clip.length).max(grid as u64);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_with_a_menu_command_runs_it() {
        assert_eq!(control_command(C::NewTake), Some(M::Transport(Tr::NewTake)));
        assert_eq!(control_command(C::Record), Some(M::Transport(Tr::Record)));
        assert_eq!(control_command(C::Play), None, "Play has its own logic");
    }

    #[test]
    fn map_modes_are_on_cmd_m_and_cmd_k() {
        use crate::ui::keymap::menu_shortcut;
        use egui::{Key, Modifiers};
        let cmd_m = menu_shortcut(M::Remote(Remote::ToggleMidiMap)).unwrap();
        assert_eq!(cmd_m.logical_key, Key::M);
        assert_eq!(cmd_m.modifiers, Modifiers::COMMAND);
        let cmd_k = menu_shortcut(M::Remote(Remote::ToggleKeyMap)).unwrap();
        assert_eq!(cmd_k.logical_key, Key::K);
    }

    #[test]
    fn a_switch_only_changes_when_it_should() {
        assert_eq!(RliveApp::wants(Gesture::On, false), Some(true));
        assert_eq!(RliveApp::wants(Gesture::Off, false), Some(false));
        assert_eq!(RliveApp::wants(Gesture::Toggle, true), Some(false));
        assert_eq!(RliveApp::wants(Gesture::Release, true), None);
    }
}
