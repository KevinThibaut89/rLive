//! The user's info text on tracks, clips, scenes, devices and locators, shown in the Info View.

use super::racks::macro_slot;
use super::{ChainRef, ClipRef, Session};
use rlive_core::{ChainId, DeviceId, TrackId};
use rlive_project::DeviceKind;

/// Something that can carry the user's info text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InfoTarget {
    Track(TrackId),
    Clip(ClipRef),
    Scene(usize),
    Device(ChainRef, DeviceId),
    /// A chain of the rack found in that device chain.
    Chain(ChainRef, DeviceId, ChainId),
    /// A Macro Control of the rack found in that device chain.
    Macro(ChainRef, DeviceId, usize),
    Locator(usize),
}

impl Session {
    /// The info text on `target`, if it still exists.
    pub fn info_text(&self, target: InfoTarget) -> Option<&str> {
        let project = &self.project;
        Some(match target {
            InfoTarget::Track(id) => &project.track(id)?.info,
            InfoTarget::Clip(r) => &self.clip_at(r)?.info,
            InfoTarget::Scene(i) => &project.scenes.get(i)?.info,
            InfoTarget::Device(chain, id) => &self.devices(chain).iter().find(|d| d.id == id)?.info,
            InfoTarget::Chain(chain, rack, id) => {
                let rack = self.devices(chain).iter().find(|d| d.id == rack)?;
                &rack.chains().find(|c| c.id == id)?.info
            }
            InfoTarget::Macro(chain, rack, n) => {
                let rack = self.devices(chain).iter().find(|d| d.id == rack)?;
                let DeviceKind::Rack { macros, .. } = &rack.kind else {
                    return None;
                };
                macros.get(n).map_or("", |m| m.info.as_str())
            }
            InfoTarget::Locator(i) => &project.arrangement.locators.get(i)?.info,
        })
    }

    /// Replaces the info text on `target`, as one undoable step.
    pub fn set_info_text(&mut self, target: InfoTarget, text: String) {
        if self.info_text(target) == Some(text.as_str()) {
            return;
        }
        let slot = match target {
            InfoTarget::Track(id) => self.project.track_mut(id).map(|t| &mut t.info),
            InfoTarget::Clip(r) => self.clip_at_mut(r).map(|c| &mut c.info),
            InfoTarget::Scene(i) => self.project.scenes.get_mut(i).map(|s| &mut s.info),
            InfoTarget::Device(chain, id) => self
                .chain_mut(chain)
                .and_then(|v| v.iter_mut().find(|d| d.id == id))
                .map(|d| &mut d.info),
            InfoTarget::Chain(chain, rack, id) => self
                .rack_mut(chain, rack)
                .and_then(|d| d.chains_mut().find(|c| c.id == id))
                .map(|c| &mut c.info),
            InfoTarget::Macro(chain, rack, n) => self
                .macros_mut(chain, rack)
                .and_then(|macros| macro_slot(macros, n))
                .map(|m| &mut m.info),
            InfoTarget::Locator(i) => self
                .project
                .arrangement
                .locators
                .get_mut(i)
                .map(|l| &mut l.info),
        };
        if let Some(slot) = slot {
            *slot = text;
            self.mark_edit("Edit Info Text");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_project::{Device, Project, TrackKind};

    fn session() -> (rlive_engine::Engine, Session, TrackId) {
        let (engine, handle) = rlive_engine::create_engine(48_000.0, 512);
        let mut s = Session::new(handle, Project::new_default(), Default::default());
        let id = s.add_track(TrackKind::Midi, None);
        (engine, s, id)
    }

    #[test]
    fn info_text_round_trips_on_every_target_and_undoes() {
        let (_engine, mut s, id) = session();
        s.add_device(
            ChainRef::Track(id),
            None,
            Device::builtin("rlive.gain", "Gain"),
        );
        let device = s.devices(ChainRef::Track(id))[0].id;
        s.project.arrangement.locators.push(rlive_project::Locator {
            tick: 0,
            name: "Intro".into(),
            info: String::new(),
        });
        let sig = s.project.time_signature;
        s.set_clip(
            id,
            0,
            Some(rlive_project::Clip::new_midi("x", [1, 2, 3], 1, sig)),
        );
        s.commit_edits(false);
        let targets = [
            InfoTarget::Track(id),
            InfoTarget::Clip(ClipRef::slot(id, 0)),
            InfoTarget::Scene(0),
            InfoTarget::Device(ChainRef::Track(id), device),
            InfoTarget::Locator(0),
        ];
        for target in targets {
            s.set_info_text(target, "a note".into());
            assert_eq!(s.info_text(target), Some("a note"), "{target:?}");
            s.commit_edits(false);
            assert!(s.undo());
            assert_eq!(s.info_text(target), Some(""), "{target:?} undo");
            assert!(s.redo());
            assert_eq!(s.info_text(target), Some("a note"), "{target:?} redo");
        }
    }

    #[test]
    fn setting_the_same_text_changes_nothing() {
        let (_engine, mut s, id) = session();
        s.commit_edits(false);
        s.set_info_text(InfoTarget::Track(id), String::new());
        assert!(s.history.pending.is_none());
    }

    #[test]
    fn new_clips_take_the_track_colour_or_a_random_one() {
        let (_engine, mut s, id) = session();
        let track = s.project.track(id).unwrap().color;
        s.colors.new_clip = crate::settings::ClipColor::Track;
        assert_eq!(s.new_clip_color(id), track);
        s.colors.new_clip = crate::settings::ClipColor::Random;
        assert_eq!(s.new_clip_color(id), s.project.next_color());
    }

    #[test]
    fn a_target_that_has_gone_reads_as_none() {
        let (_engine, s, _) = session();
        assert_eq!(s.info_text(InfoTarget::Scene(999)), None);
    }
}
