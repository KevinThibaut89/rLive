//! MIDI and computer-key mappings.

use crate::defaults::is_default;
use crate::Project;
use rlive_core::param::{ParamOwner, ParamRef};
use rlive_core::{DeviceId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// What a mapping controls. Every variant names the thing itself, not where it's drawn, so a
/// mapping follows its target through track and device moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MapTarget {
    Param(ParamRef),
    ClipSlot {
        track: TrackId,
        slot: usize,
    },
    Scene(usize),
    /// A track's Clip Stop button.
    TrackStop(TrackId),
    /// A track's Arm button.
    TrackArm(TrackId),
    /// The clip at the highlighted scene in a track (Relative Session strip, manual p. 872).
    HighlightedSlot(TrackId),
    /// One of the crossfader's three fields (manual p. 405).
    Crossfader(CrossfaderPoint),
    /// A locator, by where it is.
    Locator(u64),
    /// A button or box with no parameter behind it.
    Control(RemoteControl),
}

/// The left, centre and right fields of the crossfader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CrossfaderPoint {
    Left,
    Center,
    Right,
}

/// Controls that aren't parameters and aren't tied to one track. Names are persisted, so append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RemoteControl {
    Play,
    Stop,
    Continue,
    Record,
    SessionRecord,
    NewTake,
    CaptureMidi,
    MidiOverdub,
    AutomationArm,
    ReEnableAutomation,
    BackToArrangement,
    Metronome,
    TapTempo,
    Loop,
    PunchIn,
    PunchOut,
    Follow,
    FollowActions,
    StopAllClips,
    SceneUp,
    SceneDown,
    SceneScroll,
    LaunchHighlightedScene,
    CancelSceneLaunch,
    PreviousLocator,
    NextLocator,
    SetLocator,
    StepForward,
    StepBack,
    NudgeBack,
    NudgeForward,
    Scrub,
    ClipLoop,
    ClipLoopPosition,
    ClipLoopLength,
    PhaseNudgeUp,
    PhaseNudgeDown,
    ExternalSync,
    TempoFollower,
}

/// How a control answers to messages (manual pp. 870-872).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    /// Has a state: a note toggles it, a controller sets it.
    Switch,
    /// Does something when pressed.
    Button,
    /// A value box: steps from an encoder, or sweeps from a fader.
    Scroll,
}

impl RemoteControl {
    pub fn kind(self) -> ControlKind {
        use RemoteControl as C;
        match self {
            C::Play
            | C::Record
            | C::SessionRecord
            | C::MidiOverdub
            | C::AutomationArm
            | C::Metronome
            | C::Loop
            | C::PunchIn
            | C::PunchOut
            | C::Follow
            | C::FollowActions
            | C::ClipLoop
            | C::ExternalSync
            | C::TempoFollower => ControlKind::Switch,
            C::SceneScroll | C::Scrub | C::ClipLoopPosition | C::ClipLoopLength => {
                ControlKind::Scroll
            }
            _ => ControlKind::Button,
        }
    }

    /// The name the Mapping Browser shows.
    pub fn label(self) -> &'static str {
        use RemoteControl as C;
        match self {
            C::Play => "Play",
            C::Stop => "Stop",
            C::Continue => "Continue",
            C::Record => "Arrangement Record",
            C::SessionRecord => "Session Record",
            C::NewTake => "New",
            C::CaptureMidi => "Capture MIDI",
            C::MidiOverdub => "MIDI Arrangement Overdub",
            C::AutomationArm => "Automation Arm",
            C::ReEnableAutomation => "Re-Enable Automation",
            C::BackToArrangement => "Back to Arrangement",
            C::Metronome => "Metronome",
            C::TapTempo => "Tap Tempo",
            C::Loop => "Arrangement Loop",
            C::PunchIn => "Punch-In",
            C::PunchOut => "Punch-Out",
            C::Follow => "Follow",
            C::FollowActions => "Follow Actions",
            C::StopAllClips => "Stop All Clips",
            C::SceneUp => "Scene Up",
            C::SceneDown => "Scene Down",
            C::SceneScroll => "Scene Select",
            C::LaunchHighlightedScene => "Launch Highlighted Scene",
            C::CancelSceneLaunch => "Cancel Scene Launch",
            C::PreviousLocator => "Previous Locator",
            C::NextLocator => "Next Locator",
            C::SetLocator => "Set Locator",
            C::StepForward => "Step Record Forward",
            C::StepBack => "Step Record Back",
            C::NudgeBack => "Nudge Backward",
            C::NudgeForward => "Nudge Forward",
            C::PhaseNudgeUp => "Phase Nudge Up",
            C::PhaseNudgeDown => "Phase Nudge Down",
            C::ExternalSync => "External Sync",
            C::TempoFollower => "Tempo Follower",
            C::Scrub => "Scrub",
            C::ClipLoop => "Clip Loop",
            C::ClipLoopPosition => "Clip Loop Position",
            C::ClipLoopLength => "Clip Loop Length",
        }
    }
}

/// How a controller's values are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CcEncoding {
    #[default]
    Absolute,
    Absolute14Bit,
    RelativeSignedBit,
    RelativeSignedBit2,
    RelativeBinOffset,
    RelativeTwosComplement,
    /// The linear variants ignore how fast the encoder is turning.
    RelativeSignedBitLinear,
    RelativeSignedBit2Linear,
    RelativeBinOffsetLinear,
    RelativeTwosComplementLinear,
}

impl CcEncoding {
    pub const ALL: [CcEncoding; 10] = [
        Self::Absolute,
        Self::Absolute14Bit,
        Self::RelativeSignedBit,
        Self::RelativeSignedBit2,
        Self::RelativeBinOffset,
        Self::RelativeTwosComplement,
        Self::RelativeSignedBitLinear,
        Self::RelativeSignedBit2Linear,
        Self::RelativeBinOffsetLinear,
        Self::RelativeTwosComplementLinear,
    ];

    pub fn is_relative(self) -> bool {
        !matches!(self, Self::Absolute | Self::Absolute14Bit)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Absolute => "Absolute",
            Self::Absolute14Bit => "Absolute (14-bit)",
            Self::RelativeSignedBit => "Relative (Signed Bit)",
            Self::RelativeSignedBit2 => "Relative (Signed Bit 2)",
            Self::RelativeBinOffset => "Relative (Bin Offset)",
            Self::RelativeTwosComplement => "Relative (Twos Complement)",
            Self::RelativeSignedBitLinear => "Relative (Signed Bit, Linear)",
            Self::RelativeSignedBit2Linear => "Relative (Signed Bit 2, Linear)",
            Self::RelativeBinOffsetLinear => "Relative (Bin Offset, Linear)",
            Self::RelativeTwosComplementLinear => "Relative (Twos Complement, Linear)",
        }
    }
}

/// The MIDI message a mapping listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiSource {
    Note(u8),
    /// A range of notes. On a variable parameter, each note is one value.
    NoteRange {
        low: u8,
        high: u8,
    },
    Cc {
        number: u8,
        encoding: CcEncoding,
    },
    PitchBend,
    /// A range of notes playing a slot's clip chromatically: `root` plays it as written.
    ChromaticRange {
        root: u8,
        low: u8,
        high: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiMapping {
    pub channel: u8,
    pub source: MidiSource,
    pub target: MapTarget,
    /// The range the control sweeps. `max` below `min` inverts it.
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyMapping {
    /// The key's name, as egui spells it.
    pub key: String,
    pub target: MapTarget,
    /// What the key sets `target` to; toggles between them for a parameter.
    pub min: f64,
    pub max: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub toggle: bool,
}

impl Project {
    /// Drops the mappings whose target no longer exists. Returns how many went.
    pub fn prune_mappings(&mut self) -> usize {
        self.prune_modulations();
        if self.midi_mappings.is_empty()
            && self.key_mappings.is_empty()
            && self.surface_locks.is_empty()
        {
            return 0;
        }
        let known = KnownTargets::of(self);
        let before = self.midi_mappings.len() + self.key_mappings.len();
        self.midi_mappings.retain(|m| known.has(m.target));
        self.key_mappings.retain(|m| known.has(m.target));
        self.surface_locks
            .retain(|(_, d)| known.devices.contains(d));
        before - self.midi_mappings.len() - self.key_mappings.len()
    }

    /// Drops a modulator's mapping when what it drove has gone.
    fn prune_modulations(&mut self) {
        let any = self
            .tracks
            .iter()
            .chain(&self.returns)
            .chain(std::iter::once(&self.master))
            .any(|t| {
                t.devices.iter().any(|d| {
                    let mut mapped = false;
                    d.for_each(&mut |d| mapped |= !d.modulations.is_empty());
                    mapped
                })
            });
        if !any {
            return;
        }
        let known = KnownTargets::of(self);
        for t in self
            .tracks
            .iter_mut()
            .chain(self.returns.iter_mut())
            .chain(std::iter::once(&mut self.master))
        {
            for d in &mut t.devices {
                d.for_each_mut(&mut |d| {
                    d.modulations
                        .retain(|m| known.has(MapTarget::Param(m.target)));
                });
            }
        }
    }

    /// Moves mappings on scenes and slots as scenes move. `old_to_new` says where each old scene
    /// went, or `None` if it's gone.
    pub fn remap_scene_mappings(&mut self, old_to_new: &dyn Fn(usize) -> Option<usize>) {
        let map = |t: MapTarget| -> Option<MapTarget> {
            match t {
                MapTarget::Scene(s) => old_to_new(s).map(MapTarget::Scene),
                MapTarget::ClipSlot { track, slot } => {
                    old_to_new(slot).map(|slot| MapTarget::ClipSlot { track, slot })
                }
                other => Some(other),
            }
        };
        self.midi_mappings.retain_mut(|m| match map(m.target) {
            Some(t) => {
                m.target = t;
                true
            }
            None => false,
        });
        self.key_mappings.retain_mut(|m| match map(m.target) {
            Some(t) => {
                m.target = t;
                true
            }
            None => false,
        });
    }

    /// A locator moved: mappings on it go with it.
    pub fn move_locator_mappings(&mut self, from: u64, to: u64) {
        for t in self
            .midi_mappings
            .iter_mut()
            .map(|m| &mut m.target)
            .chain(self.key_mappings.iter_mut().map(|m| &mut m.target))
        {
            if *t == MapTarget::Locator(from) {
                *t = MapTarget::Locator(to);
            }
        }
    }
}

/// What a Set has that mappings can point at.
struct KnownTargets {
    tracks: HashSet<TrackId>,
    devices: HashSet<DeviceId>,
    chains: HashSet<rlive_core::ChainId>,
    scenes: usize,
    locators: HashSet<u64>,
}

impl KnownTargets {
    fn of(p: &Project) -> Self {
        let mut known = Self {
            tracks: HashSet::new(),
            devices: HashSet::new(),
            chains: HashSet::new(),
            scenes: p.scenes.len(),
            locators: p.arrangement.locators.iter().map(|l| l.tick).collect(),
        };
        for t in p.tracks.iter().chain(&p.returns) {
            known.tracks.insert(t.id);
            for d in &t.devices {
                known.add_device(d);
            }
        }
        for d in &p.master.devices {
            known.add_device(d);
        }
        known
    }

    fn add_device(&mut self, d: &crate::Device) {
        self.devices.insert(d.id);
        for chain in d.chains() {
            self.chains.insert(chain.id);
            for child in &chain.devices {
                self.add_device(child);
            }
        }
    }

    fn has(&self, t: MapTarget) -> bool {
        match t {
            MapTarget::Param(ParamRef { owner, .. }) => match owner {
                ParamOwner::Device(d) => self.devices.contains(&d),
                ParamOwner::Track(t) => self.tracks.contains(&t),
                ParamOwner::Chain(c) => self.chains.contains(&c),
                ParamOwner::Master | ParamOwner::Transport => true,
            },
            MapTarget::ClipSlot { track, slot } => {
                self.tracks.contains(&track) && slot < self.scenes
            }
            MapTarget::Scene(s) => s < self.scenes,
            MapTarget::TrackStop(t) | MapTarget::TrackArm(t) | MapTarget::HighlightedSlot(t) => {
                self.tracks.contains(&t)
            }
            MapTarget::Locator(tick) => self.locators.contains(&tick),
            MapTarget::Crossfader(_) | MapTarget::Control(_) => true,
        }
    }
}
