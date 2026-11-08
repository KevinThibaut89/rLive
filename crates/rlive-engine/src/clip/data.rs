//! The clip content the engine plays.

use rlive_core::warp::{WarpMap, WarpMode};
use rlive_core::SampleBuffer;
use std::sync::Arc;

/// A note inside a MIDI clip. Positions are in ticks relative to the clip start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiClip {
    /// Loop length in ticks.
    pub length: u64,
    /// Notes sorted by start.
    pub notes: Vec<Note>,
    pub looped: bool,
    /// Longest note in `notes` (at least 1), the lookback needed to find held notes.
    pub(super) max_len: u64,
}

impl MidiClip {
    /// Build a clip, sorting `notes` by start and key.
    pub fn new(length: u64, mut notes: Vec<Note>, looped: bool) -> Self {
        notes.sort_by_key(|n| (n.start, n.key));
        let max_len = notes.iter().map(|n| n.length.max(1)).max().unwrap_or(1);
        Self {
            length,
            notes,
            looped,
            max_len,
        }
    }
}

/// An audio clip played back without time-stretching ("warp off").
#[derive(Debug, Clone)]
pub struct AudioClip {
    pub sample: Arc<SampleBuffer>,
    /// Loop length in ticks.
    pub length: u64,
    pub looped: bool,
    pub gain: f32,
}

impl PartialEq for AudioClip {
    /// Samples compare by identity, so comparing clips never walks the audio data.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.sample, &other.sample)
            && self.length == other.length
            && self.looped == other.looped
            && self.gain == other.gain
    }
}

/// An audio clip with Warp on. Plays through its mode's stretcher.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpedClip {
    pub audio: AudioClip,
    /// `None` until the clip has two usable markers: it plays at one speed, like warp off.
    pub map: Option<WarpMap>,
    pub mode: WarpMode,
    /// Semitones.
    pub transpose: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClipData {
    Midi(MidiClip),
    Audio(AudioClip),
    Warped(WarpedClip),
}

impl ClipData {
    pub fn length(&self) -> u64 {
        match self {
            ClipData::Midi(c) => c.length,
            ClipData::Audio(c) => c.length,
            ClipData::Warped(c) => c.audio.length,
        }
    }

    pub fn looped(&self) -> bool {
        match self {
            ClipData::Midi(c) => c.looped,
            ClipData::Audio(c) => c.looped,
            ClipData::Warped(c) => c.audio.looped,
        }
    }
}
