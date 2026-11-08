//! What clips and scenes do when launched, shared by the project and the engine.

use crate::PPQ;
use serde::{Deserialize, Serialize};

/// How a clip answers its launch button (manual p. 361).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LaunchMode {
    /// Down starts the clip; up is ignored.
    #[default]
    Trigger,
    /// Down starts the clip; up stops it.
    Gate,
    /// Down starts the clip; the next down stops it.
    Toggle,
    /// While held, the clip is triggered again at the clip quantization rate.
    Repeat,
}

impl LaunchMode {
    pub const ALL: [LaunchMode; 4] = [
        LaunchMode::Trigger,
        LaunchMode::Gate,
        LaunchMode::Toggle,
        LaunchMode::Repeat,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LaunchMode::Trigger => "Trigger",
            LaunchMode::Gate => "Gate",
            LaunchMode::Toggle => "Toggle",
            LaunchMode::Repeat => "Repeat",
        }
    }
}

/// What happens after a clip or scene has played for a while.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FollowActions {
    pub enabled: bool,
    pub action_a: FollowAction,
    pub action_b: FollowAction,
    /// Relative chance of each action, 0..1. The controls keep the two summing to 1, as Chance B
    /// going to 90% makes A come up about once in ten (manual p. 366).
    pub chance_a: f32,
    pub chance_b: f32,
    /// Clips only: fire at the end of the clip (after `multiplier` loops) rather than after `time`.
    pub linked: bool,
    pub multiplier: u32,
    /// Ticks from where play starts, for an unlinked clip or a scene.
    pub time: u64,
}

impl Default for FollowActions {
    fn default() -> Self {
        Self {
            enabled: false,
            action_a: FollowAction::None,
            action_b: FollowAction::None,
            chance_a: 1.0,
            chance_b: 0.0,
            linked: true,
            multiplier: 1,
            time: 4 * PPQ,
        }
    }
}

impl FollowActions {
    /// The action a random number in `0..1` picks. With both chances at zero, nothing happens.
    pub fn choose(&self, unit: f64) -> FollowAction {
        let (a, b) = (self.chance_a.max(0.0) as f64, self.chance_b.max(0.0) as f64);
        if a + b <= 0.0 {
            FollowAction::None
        } else if unit * (a + b) < a {
            self.action_a
        } else {
            self.action_b
        }
    }

    /// Whether either action can do something, which is when its button is striped.
    pub fn assigned(&self) -> bool {
        self.enabled && (self.action_a != FollowAction::None || self.action_b != FollowAction::None)
    }

    /// The Chance slider between A and B: `share` of the chance goes to A.
    pub fn set_chance_share(&mut self, share: f32) {
        let share = share.clamp(0.0, 1.0);
        self.chance_a = share;
        self.chance_b = 1.0 - share;
    }
}

/// One of the ten Follow Actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FollowAction {
    #[default]
    None,
    Stop,
    PlayAgain,
    Previous,
    Next,
    First,
    Last,
    Any,
    Other,
    /// To a slot of the track, or a scene, by its zero-based index. An empty target does nothing.
    Jump(u32),
}

impl FollowAction {
    /// Every action, the Jump target at its default.
    pub const ALL: [FollowAction; 10] = [
        FollowAction::None,
        FollowAction::Stop,
        FollowAction::PlayAgain,
        FollowAction::Previous,
        FollowAction::Next,
        FollowAction::First,
        FollowAction::Last,
        FollowAction::Any,
        FollowAction::Other,
        FollowAction::Jump(0),
    ];

    pub fn label(self) -> &'static str {
        match self {
            FollowAction::None => "No Action",
            FollowAction::Stop => "Stop",
            FollowAction::PlayAgain => "Play Again",
            FollowAction::Previous => "Previous",
            FollowAction::Next => "Next",
            FollowAction::First => "First",
            FollowAction::Last => "Last",
            FollowAction::Any => "Any",
            FollowAction::Other => "Other",
            FollowAction::Jump(_) => "Jump",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chances_split_the_unit_interval() {
        let f = FollowActions {
            action_a: FollowAction::Next,
            action_b: FollowAction::Stop,
            chance_a: 0.1,
            chance_b: 0.9,
            ..FollowActions::default()
        };
        assert_eq!(f.choose(0.05), FollowAction::Next);
        assert_eq!(f.choose(0.5), FollowAction::Stop);
    }

    #[test]
    fn no_chance_at_all_means_no_action() {
        let f = FollowActions {
            action_a: FollowAction::Next,
            chance_a: 0.0,
            chance_b: 0.0,
            ..FollowActions::default()
        };
        assert_eq!(f.choose(0.0), FollowAction::None);
    }

    #[test]
    fn the_slider_keeps_the_chances_summing_to_one() {
        let mut f = FollowActions::default();
        f.set_chance_share(0.25);
        assert_eq!((f.chance_a, f.chance_b), (0.25, 0.75));
    }
}
