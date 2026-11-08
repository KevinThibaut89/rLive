use crate::{ChainId, DeviceId, TrackId};
use serde::{Deserialize, Serialize};

/// Persisted identity of a parameter within its owner. Explicit and append-only: an id once
/// shipped keeps its meaning forever. Dense for built-ins; plugin ids are whatever the plugin says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ParamId(pub u32);

/// Every device's on/off switch. 1 means on.
pub const ACTIVATOR: ParamId = ParamId(u32::MAX);

/// What a parameter belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParamOwner {
    Device(DeviceId),
    Track(TrackId),
    Chain(ChainId),
    /// The Master track, which has no [`TrackId`] of its own.
    Master,
    Transport,
}

/// A parameter anywhere in a Set. This is what automation, Macros and MIDI mappings store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParamRef {
    pub owner: ParamOwner,
    pub id: ParamId,
}

/// How modulation applies to a parameter (manual p. 536): pitch adds, volume scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ModMode {
    /// The modulation amount is added to the value, e.g. transposition.
    #[default]
    Additive,
    /// The modulation amount multiplies the value, e.g. gain.
    Scale,
}

/// Capabilities of a parameter, as a set of bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParamFlags(pub u8);

impl ParamFlags {
    pub const NONE: Self = Self(0);
    pub const AUTOMATABLE: Self = Self(1);
    pub const MODULATABLE: Self = Self(1 << 1);
    pub const HIDDEN: Self = Self(1 << 2);
    pub const READ_ONLY: Self = Self(1 << 3);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The default flags, plus [`HIDDEN`](Self::HIDDEN) for a parameter a plugin keeps out of
    /// its host's UI. Hidden parameters still have cells, so a plugin's editor can change them.
    pub fn for_plugin(hidden: bool) -> Self {
        if hidden {
            Self::default().union(Self::HIDDEN)
        } else {
            Self::default()
        }
    }
}

impl Default for ParamFlags {
    fn default() -> Self {
        Self::AUTOMATABLE.union(Self::MODULATABLE)
    }
}

/// A parameter change at a position inside a block, in plain units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamEvent {
    /// Frames from the start of the block.
    pub offset: u32,
    pub id: ParamId,
    pub value: f64,
}

/// A clip property an envelope can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipParam {
    Gain,
    Transpose,
    SampleOffset,
}

/// What an automation or clip envelope drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnvTarget {
    Param(ParamRef),
    Clip(ClipParam),
    /// A MIDI controller, by number.
    MidiCtrl(u8),
}

/// Mixer controls as persisted parameter ids of a [`ParamOwner::Track`] or [`ParamOwner::Master`].
/// The controls come first and are dense; sends get their own block so more controls can be
/// appended without colliding with them.
pub mod mixer {
    use super::{ModMode, ParamId, ParamInfo};

    pub const VOLUME: ParamId = ParamId(0);
    pub const PAN: ParamId = ParamId(1);
    pub const MUTE: ParamId = ParamId(2);
    pub const SOLO: ParamId = ParamId(3);
    /// Send `n` is `ParamId(FIRST_SEND.0 + n)`.
    pub const FIRST_SEND: ParamId = ParamId(0x100);

    /// Lowest volume the fader reaches, which is silence.
    pub const VOLUME_MIN_DB: f64 = -96.0;
    /// Highest volume the fader reaches.
    pub const VOLUME_MAX_DB: f64 = 6.0;

    /// A description of a track's volume, pan, mute and solo, in that order.
    pub fn infos() -> [ParamInfo; 4] {
        [
            ParamInfo::continuous(VOLUME.0, "Volume", VOLUME_MIN_DB, VOLUME_MAX_DB, 0.0, "dB")
                .with_mod_mode(ModMode::Scale),
            ParamInfo::continuous(PAN.0, "Pan", -1.0, 1.0, 0.0, ""),
            ParamInfo::toggle(MUTE.0, "Mute", false),
            ParamInfo::toggle(SOLO.0, "Solo", false),
        ]
    }
}

/// How a parameter's plain values map to knob positions. Small and `Copy` so the audio thread
/// can resolve values without touching a [`ParamInfo`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamRange {
    pub min: f64,
    pub max: f64,
    /// `1` is linear; see [`ParamKind::Continuous`].
    pub skew: f64,
    /// Plain values are whole numbers (stepped parameters and toggles).
    pub quantized: bool,
}

impl ParamRange {
    /// Keep a plain value inside the range. A degenerate range leaves it alone.
    #[inline]
    pub fn clamp(&self, value: f64) -> f64 {
        if self.max > self.min {
            value.clamp(self.min, self.max)
        } else {
            value
        }
    }

    /// Map a plain value to `0..=1` knob position.
    pub fn to_normalized(&self, value: f64) -> f64 {
        if self.max <= self.min {
            return 0.0;
        }
        let lin = ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        if self.skew != 1.0 {
            lin.powf(1.0 / self.skew)
        } else {
            lin
        }
    }

    /// Map a `0..=1` knob position back to a plain value (quantised for stepped params).
    pub fn from_normalized(&self, norm: f64) -> f64 {
        let norm = norm.clamp(0.0, 1.0);
        let lin = if self.skew != 1.0 {
            norm.powf(self.skew)
        } else {
            norm
        };
        let v = self.min + lin * (self.max - self.min);
        if self.quantized {
            v.round()
        } else {
            v
        }
    }
}

/// How a parameter should be presented and quantised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamKind {
    /// Continuous value. `skew` > 1 gives more knob resolution near `min`
    /// (useful for frequencies and times).
    Continuous { skew: f64 },
    /// Integer steps between min and max, optionally with names.
    Stepped { labels: Vec<String> },
    /// On/off switch (0.0 / 1.0).
    Toggle,
}

/// Description of an automatable parameter, in *plain* units (Hz, dB, ms, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: String,
    pub kind: ParamKind,
    #[serde(default)]
    pub mod_mode: ModMode,
    #[serde(default)]
    pub flags: ParamFlags,
}

impl ParamInfo {
    pub fn continuous(id: u32, name: &str, min: f64, max: f64, default: f64, unit: &str) -> Self {
        Self {
            id,
            name: name.into(),
            min,
            max,
            default,
            unit: unit.into(),
            kind: ParamKind::Continuous { skew: 1.0 },
            mod_mode: ModMode::default(),
            flags: ParamFlags::default(),
        }
    }

    /// Set how modulation applies to this parameter.
    pub fn with_mod_mode(mut self, mod_mode: ModMode) -> Self {
        self.mod_mode = mod_mode;
        self
    }

    pub fn with_flags(mut self, flags: ParamFlags) -> Self {
        self.flags = flags;
        self
    }

    /// The persisted id.
    pub fn param_id(&self) -> ParamId {
        ParamId(self.id)
    }

    /// Continuous parameter with logarithmic-feeling response (frequencies, times).
    pub fn skewed(
        id: u32,
        name: &str,
        min: f64,
        max: f64,
        default: f64,
        unit: &str,
        skew: f64,
    ) -> Self {
        Self {
            kind: ParamKind::Continuous { skew },
            ..Self::continuous(id, name, min, max, default, unit)
        }
    }

    pub fn stepped(id: u32, name: &str, labels: &[&str], default: usize) -> Self {
        Self {
            id,
            name: name.into(),
            min: 0.0,
            max: (labels.len().max(1) - 1) as f64,
            default: default as f64,
            unit: String::new(),
            kind: ParamKind::Stepped {
                labels: labels.iter().map(|s| s.to_string()).collect(),
            },
            mod_mode: ModMode::default(),
            flags: ParamFlags::default(),
        }
    }

    pub fn toggle(id: u32, name: &str, default: bool) -> Self {
        Self {
            id,
            name: name.into(),
            min: 0.0,
            max: 1.0,
            default: if default { 1.0 } else { 0.0 },
            unit: String::new(),
            kind: ParamKind::Toggle,
            mod_mode: ModMode::default(),
            flags: ParamFlags::default(),
        }
    }

    /// The value mapping as a small `Copy` value.
    pub fn range(&self) -> ParamRange {
        ParamRange {
            min: self.min,
            max: self.max,
            skew: match self.kind {
                ParamKind::Continuous { skew } => skew,
                _ => 1.0,
            },
            quantized: !matches!(self.kind, ParamKind::Continuous { .. }),
        }
    }

    /// Map a plain value to `0..=1` knob position.
    pub fn to_normalized(&self, value: f64) -> f64 {
        self.range().to_normalized(value)
    }

    /// Map a `0..=1` knob position back to a plain value (quantised for stepped params).
    pub fn from_normalized(&self, norm: f64) -> f64 {
        self.range().from_normalized(norm)
    }

    /// Human readable value, e.g. `"440 Hz"` or `"Saw"`.
    pub fn format(&self, value: f64) -> String {
        match &self.kind {
            ParamKind::Stepped { labels } => labels
                .get(value.round().max(0.0) as usize)
                .cloned()
                .unwrap_or_else(|| format!("{}", value.round())),
            ParamKind::Toggle => if value >= 0.5 { "On" } else { "Off" }.to_string(),
            ParamKind::Continuous { .. } => {
                let abs = value.abs();
                let num = if abs >= 1000.0 && self.unit == "Hz" {
                    return format!("{:.2} kHz", value / 1000.0);
                } else if abs >= 100.0 {
                    format!("{value:.0}")
                } else if abs >= 10.0 {
                    format!("{value:.1}")
                } else {
                    format!("{value:.2}")
                };
                if self.unit.is_empty() {
                    num
                } else {
                    format!("{num} {}", self.unit)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_roundtrip() {
        let p = ParamInfo::skewed(0, "Cutoff", 20.0, 20000.0, 1000.0, "Hz", 3.0);
        for v in [20.0, 100.0, 1000.0, 20000.0] {
            let n = p.to_normalized(v);
            assert!((p.from_normalized(n) - v).abs() / v < 1e-9);
        }
        let s = ParamInfo::stepped(1, "Wave", &["Saw", "Square", "Sine"], 0);
        assert_eq!(s.from_normalized(0.6), 1.0);
        assert_eq!(s.format(2.0), "Sine");
        assert_eq!(p.format(2500.0), "2.50 kHz");
    }
}
