//! Which automation lanes a track shows, without reference to the screen.

use super::super::keymap::Platform;
use crate::session::automation_edit::volume_target;
use crate::session::ChainRef;
use egui::Modifiers;
use rlive_core::param::EnvTarget;
use rlive_project::{MainLane, TrackUi};

/// A main lane can be duplicated this many times.
const MAX_DUPLICATES: usize = 2;

/// A lane of a track: the main one or an additional one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LaneKey {
    pub chain: ChainRef,
    pub slot: Option<usize>,
    /// A take lane under the track, in place of its clip lane.
    pub take: Option<usize>,
}

pub fn main_target(ui: &TrackUi, chain: ChainRef) -> Option<EnvTarget> {
    match ui.automation_main {
        MainLane::Volume => Some(volume_target(chain)),
        MainLane::None => None,
        MainLane::Show(t) => Some(t),
    }
}

/// What a click on Add Automation Lane does, by what's held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddLane {
    /// The main lane's envelope moves to a lane of its own.
    Move,
    /// The main lane stays and gains a duplicate, up to two.
    Duplicate,
    /// Every automated parameter gets a lane.
    EachAutomated,
}

impl AddLane {
    pub fn of(m: Modifiers, platform: Platform) -> Self {
        let (duplicate, each) = match platform {
            Platform::Mac => (m.alt, m.mac_cmd),
            Platform::Other => (m.ctrl, m.alt),
        };
        if each {
            Self::EachAutomated
        } else if duplicate {
            Self::Duplicate
        } else {
            Self::Move
        }
    }
}

/// Adds lanes, returning the slot of the first new one.
pub fn add_lane(
    ui: &mut TrackUi,
    chain: ChainRef,
    mode: AddLane,
    automated: &[EnvTarget],
) -> Option<usize> {
    let main = main_target(ui, chain);
    let slot = ui.automation_lanes.len();
    match mode {
        AddLane::Move => {
            ui.automation_lanes.push(main?);
            ui.automation_main = MainLane::None;
        }
        AddLane::Duplicate => {
            let target = main?;
            let count = ui.automation_lanes.iter().filter(|t| **t == target).count();
            if count >= MAX_DUPLICATES {
                return None;
            }
            ui.automation_lanes.push(target);
        }
        AddLane::EachAutomated => {
            for target in automated {
                if main != Some(*target) && !ui.automation_lanes.contains(target) {
                    ui.automation_lanes.push(*target);
                }
            }
            if ui.automation_lanes.len() == slot {
                return None;
            }
        }
    }
    ui.automation_lanes_hidden = false;
    Some(slot)
}

/// Removes lanes by slot.
pub fn remove_lanes(ui: &mut TrackUi, slots: &[usize]) {
    let mut sorted = slots.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for slot in sorted.into_iter().rev() {
        if slot < ui.automation_lanes.len() {
            ui.automation_lanes.remove(slot);
        }
    }
}

/// What removing a lane takes with it, given what's held and which headers are selected.
pub fn slots_to_remove(
    clicked: usize,
    count: usize,
    selected: &[usize],
    m: Modifiers,
    platform: Platform,
) -> Vec<usize> {
    let and_below = match platform {
        Platform::Mac => m.mac_cmd,
        Platform::Other => m.alt,
    };
    if and_below {
        (clicked..count).collect()
    } else if selected.contains(&clicked) {
        selected.to_vec()
    } else {
        vec![clicked]
    }
}

/// Hides or shows the additional lanes. Hiding them with nothing in the main lane brings the
/// topmost one up into it.
pub fn toggle_hidden(ui: &mut TrackUi) {
    if !ui.automation_lanes_hidden
        && ui.automation_main == MainLane::None
        && !ui.automation_lanes.is_empty()
    {
        ui.automation_main = MainLane::Show(ui.automation_lanes.remove(0));
    }
    ui.automation_lanes_hidden = !ui.automation_lanes_hidden;
}

/// What the main lane's choosers selected. A parameter already showing in an additional lane
/// isn't shown twice: its lane is returned to take the focus instead.
pub fn choose_main(ui: &mut TrackUi, target: Option<EnvTarget>) -> Option<usize> {
    let Some(target) = target else {
        ui.automation_main = MainLane::None;
        return None;
    };
    if let Some(slot) = ui.automation_lanes.iter().position(|t| *t == target) {
        ui.automation_lanes_hidden = false;
        return Some(slot);
    }
    ui.automation_main = MainLane::Show(target);
    None
}

/// Puts `target` in a new additional lane, unless one already shows it.
pub fn show_in_new_lane(ui: &mut TrackUi, chain: ChainRef, target: EnvTarget) -> Option<usize> {
    ui.automation_lanes_hidden = false;
    if let Some(slot) = ui.automation_lanes.iter().position(|t| *t == target) {
        return Some(slot);
    }
    if main_target(ui, chain) == Some(target) {
        return None;
    }
    ui.automation_lanes.push(target);
    Some(ui.automation_lanes.len() - 1)
}

/// An arrow key among the lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Up,
    Down,
    Left,
    Right,
}

/// What an arrow key does from a focused lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Focus(LaneKey),
    HideLanes(ChainRef),
    ShowLanes(ChainRef),
}

/// `order` is every lane on screen, top to bottom.
pub fn navigate(order: &[LaneKey], focus: LaneKey, arrow: Arrow) -> Option<Nav> {
    let at = order.iter().position(|k| *k == focus)?;
    match arrow {
        Arrow::Up => at.checked_sub(1).map(|i| Nav::Focus(order[i])),
        Arrow::Down => order.get(at + 1).map(|k| Nav::Focus(*k)),
        Arrow::Left if focus.slot.is_some() => Some(Nav::Focus(LaneKey {
            chain: focus.chain,
            slot: None,
            take: None,
        })),
        Arrow::Left => Some(Nav::HideLanes(focus.chain)),
        Arrow::Right if focus.slot.is_none() => Some(Nav::ShowLanes(focus.chain)),
        Arrow::Right => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::param::{ParamOwner, ParamRef};
    use rlive_core::{ParamId, TrackId};

    fn target(n: u32) -> EnvTarget {
        EnvTarget::Param(ParamRef {
            owner: ParamOwner::Transport,
            id: ParamId(n),
        })
    }

    fn chain() -> ChainRef {
        ChainRef::Track(TrackId::new())
    }

    #[test]
    fn a_track_shows_its_volume_until_told_otherwise() {
        let c = chain();
        assert_eq!(main_target(&TrackUi::default(), c), Some(volume_target(c)));
    }

    #[test]
    fn adding_a_lane_moves_the_main_lanes_envelope_out() {
        let c = chain();
        let mut ui = TrackUi::default();
        let volume = volume_target(c);
        assert_eq!(add_lane(&mut ui, c, AddLane::Move, &[]), Some(0));
        assert_eq!(ui.automation_lanes, [volume]);
        assert_eq!(main_target(&ui, c), None);
        // With nothing in the main lane there's nothing to add.
        assert_eq!(add_lane(&mut ui, c, AddLane::Move, &[]), None);
    }

    #[test]
    fn a_lane_can_be_duplicated_twice_only() {
        let c = chain();
        let mut ui = TrackUi {
            automation_main: MainLane::Show(target(1)),
            ..TrackUi::default()
        };
        assert_eq!(add_lane(&mut ui, c, AddLane::Duplicate, &[]), Some(0));
        assert_eq!(add_lane(&mut ui, c, AddLane::Duplicate, &[]), Some(1));
        assert_eq!(add_lane(&mut ui, c, AddLane::Duplicate, &[]), None);
        assert_eq!(ui.automation_lanes.len(), 2);
        assert_eq!(main_target(&ui, c), Some(target(1)), "the main lane stays");
    }

    #[test]
    fn each_automated_envelope_gets_one_lane() {
        let c = chain();
        let mut ui = TrackUi {
            automation_main: MainLane::Show(target(1)),
            ..TrackUi::default()
        };
        let automated = [target(1), target(2), target(3)];
        add_lane(&mut ui, c, AddLane::EachAutomated, &automated);
        assert_eq!(ui.automation_lanes, [target(2), target(3)]);
        assert_eq!(
            add_lane(&mut ui, c, AddLane::EachAutomated, &automated),
            None
        );
    }

    #[test]
    fn modifiers_choose_how_a_lane_is_added() {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(AddLane::of(alt, Platform::Mac), AddLane::Duplicate);
        assert_eq!(AddLane::of(alt, Platform::Other), AddLane::EachAutomated);
        let cmd = Modifiers {
            mac_cmd: true,
            ..Modifiers::NONE
        };
        assert_eq!(AddLane::of(cmd, Platform::Mac), AddLane::EachAutomated);
        assert_eq!(AddLane::of(Modifiers::NONE, Platform::Mac), AddLane::Move);
    }

    #[test]
    fn removing_takes_the_selection_or_everything_below() {
        let none = Modifiers::NONE;
        assert_eq!(slots_to_remove(1, 4, &[], none, Platform::Mac), [1]);
        assert_eq!(slots_to_remove(1, 4, &[1, 3], none, Platform::Mac), [1, 3]);
        let cmd = Modifiers {
            mac_cmd: true,
            ..Modifiers::NONE
        };
        assert_eq!(slots_to_remove(1, 4, &[], cmd, Platform::Mac), [1, 2, 3]);
        let mut ui = TrackUi {
            automation_lanes: vec![target(0), target(1), target(2), target(3)],
            ..TrackUi::default()
        };
        remove_lanes(&mut ui, &[3, 1]);
        assert_eq!(ui.automation_lanes, [target(0), target(2)]);
    }

    #[test]
    fn hiding_with_an_empty_main_lane_brings_the_top_lane_up() {
        let c = chain();
        let mut ui = TrackUi {
            automation_main: MainLane::None,
            automation_lanes: vec![target(1), target(2)],
            ..TrackUi::default()
        };
        toggle_hidden(&mut ui);
        assert!(ui.automation_lanes_hidden);
        assert_eq!(main_target(&ui, c), Some(target(1)));
        assert_eq!(ui.automation_lanes, [target(2)]);
    }

    #[test]
    fn choosing_a_parameter_in_another_lane_goes_to_that_lane() {
        let c = chain();
        let mut ui = TrackUi {
            automation_lanes: vec![target(2)],
            ..TrackUi::default()
        };
        assert_eq!(choose_main(&mut ui, Some(target(2))), Some(0));
        assert_eq!(
            main_target(&ui, c),
            Some(volume_target(c)),
            "main unchanged"
        );
        assert_eq!(choose_main(&mut ui, Some(target(5))), None);
        assert_eq!(main_target(&ui, c), Some(target(5)));
        assert_eq!(choose_main(&mut ui, None), None);
        assert_eq!(main_target(&ui, c), None);
    }

    #[test]
    fn arrows_walk_the_lanes() {
        let c = chain();
        let main = LaneKey {
            chain: c,
            slot: None,
            take: None,
        };
        let extra = LaneKey {
            chain: c,
            slot: Some(0),
            take: None,
        };
        let order = [main, extra];
        assert_eq!(navigate(&order, main, Arrow::Down), Some(Nav::Focus(extra)));
        assert_eq!(navigate(&order, extra, Arrow::Up), Some(Nav::Focus(main)));
        assert_eq!(navigate(&order, extra, Arrow::Left), Some(Nav::Focus(main)));
        assert_eq!(navigate(&order, main, Arrow::Left), Some(Nav::HideLanes(c)));
        assert_eq!(
            navigate(&order, main, Arrow::Right),
            Some(Nav::ShowLanes(c))
        );
        assert_eq!(navigate(&order, main, Arrow::Up), None);
    }
}
