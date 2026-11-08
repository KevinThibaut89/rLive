//! MIDI and computer-key mappings.

use crate::defaults::is_default;
use rlive_core::param::ParamRef;
use rlive_core::TrackId;
use serde::{Deserialize, Serialize};

/// What a mapping controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MapTarget {
    Param(ParamRef),
    ClipSlot { track: TrackId, slot: usize },
    Scene(usize),
}

/// How a controller's values are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CcEncoding {
    #[default]
    Absolute,
    Absolute14Bit,
    RelativeSignedBit,
    RelativeSignedBit2,
    RelativeBinOffset,
    RelativeTwosComplement,
}

/// The MIDI message a mapping listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MidiSource {
    Note(u8),
    /// A range of notes, for playing a track's clips chromatically.
    NoteRange {
        low: u8,
        high: u8,
    },
    Cc {
        number: u8,
        encoding: CcEncoding,
    },
    PitchBend,
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
