//! Shared building blocks for rLive.
//!
//! Everything in here is used by both the real-time audio thread and the UI thread,
//! so the types are small, `Copy` where possible and never allocate in the hot path.

pub mod clip_region;
pub mod crossfade;
pub mod device;
pub mod envelope;
pub mod fade;
pub mod ids;
pub mod launch;
pub mod midi;
pub mod mpe;

pub mod param;
pub mod processor;
pub mod sample;
pub mod time;
pub mod view;
pub mod warp;

pub use clip_region::ClipRegion;
pub use crossfade::CrossfadeCurve;
pub use device::{DeviceDesc, Item, Ports, SampleSlot, ScopeKind, Section, Status};
pub use fade::{fade_gain, EdgeFades};
pub use ids::{ArrClipId, ChainId, DeviceId, GrooveId, TrackId};
pub use launch::{FollowAction, FollowActions, LaunchMode};
pub use midi::{
    ExprKind, MidiEvent, MidiMessage, NoteId, NoteIds, NoteSpace, MPE_BEND_RANGE, SLIDE_CC,
};
pub use mpe::{MpeDecoder, MpeEncoder, MpeZone};
pub use param::{
    ClipParam, EnvTarget, ModMode, ParamEvent, ParamFlags, ParamId, ParamInfo, ParamKind,
    ParamOwner, ParamRange, ParamRef, ACTIVATOR,
};
pub use processor::{
    AudioBuffer, AudioInput, AudioOutput, BlockStep, EventQueue, ParamOutQueue, ProcessContext,
    Processor, Resource, Tail,
};
pub use sample::SampleBuffer;
pub use time::{
    BarMap, Quantize, ScaleInfo, TempoMap, TempoPoint, TimeMaps, TimeSignature, TransportInfo, PPQ,
};
pub use view::{ViewChannel, ViewId, ViewReader, ViewWriter, METER_VALUES, SCOPE_VALUES};

/// Third-party plugin formats rLive can host.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum PluginFormat {
    Clap,
    Vst3,
    AudioUnit,
}

impl PluginFormat {
    pub const ALL: [PluginFormat; 3] = [
        PluginFormat::Clap,
        PluginFormat::Vst3,
        PluginFormat::AudioUnit,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            PluginFormat::Clap => "CLAP",
            PluginFormat::Vst3 => "VST3",
            PluginFormat::AudioUnit => "AU",
        }
    }

    /// Short lowercase name used on the scanner command line.
    pub fn key(&self) -> &'static str {
        match self {
            PluginFormat::Clap => "clap",
            PluginFormat::Vst3 => "vst3",
            PluginFormat::AudioUnit => "au",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == key)
    }
}

/// Convert decibels to linear gain. Anything at or below -96 dB is treated as silence.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -96.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Convert linear gain to decibels (clamped at -96 dB).
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.000_015_85 {
        -96.0
    } else {
        20.0 * gain.log10()
    }
}

/// Frequency in Hz of a MIDI note number (A4 = 69 = 440 Hz).
#[inline]
pub fn note_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Human readable note name, e.g. `C3` for 60 (Ableton convention: middle C = C3).
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = note as i32 / 12 - 2;
    format!("{}{}", NAMES[note as usize % 12], octave)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_roundtrip() {
        for db in [-60.0f32, -12.0, -6.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-3);
        }
        assert_eq!(db_to_gain(-200.0), 0.0);
    }

    #[test]
    fn note_names() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(69), "A3");
        assert!((note_to_hz(69.0) - 440.0).abs() < 1e-3);
    }
}
