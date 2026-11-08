use crate::defaults::{is_default, yes};
use crate::FileRef;
use rlive_core::param::rack::MACRO_COUNT;
use rlive_core::param::ParamRef;
use rlive_core::{ChainId, DeviceId, PluginFormat, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceKind {
    Builtin {
        type_id: String,
        /// One entry per sample slot of the device.
        #[serde(default, skip_serializing_if = "is_default")]
        samples: Vec<Option<FileRef>>,
        /// RON text the device writes and reads itself, versioned by the device.
        #[serde(default, skip_serializing_if = "is_default")]
        state: Option<String>,
    },
    Plugin {
        format: PluginFormat,
        /// Format-specific id (CLAP id, VST3 class id, AU `type:subtype:manufacturer`).
        plugin_id: String,
        path: PathBuf,
        /// Opaque plugin state blob.
        #[serde(default, with = "base64_opt")]
        state: Option<Vec<u8>>,
    },
    /// Parallel chains of devices, which may hold racks in turn.
    Rack {
        kind: RackKind,
        #[serde(default, skip_serializing_if = "is_default")]
        chains: Vec<Chain>,
        /// Return chains, which the chains send to.
        #[serde(default, skip_serializing_if = "is_default")]
        returns: Vec<Chain>,
        /// The rack's macro controls, up to 16. Hidden ones keep working: `visible_macros` only
        /// says how many are shown. Empty means none have been touched yet.
        #[serde(default, skip_serializing_if = "is_default")]
        macros: Vec<Macro>,
        /// How many Macro Controls are shown, 1 to 16 (manual p. 504). 8 on a new rack.
        #[serde(
            default = "default_visible_macros",
            skip_serializing_if = "is_default_visible"
        )]
        visible_macros: u8,
        /// With this on the Chain Selector filters MIDI other than notes as well (p. 499).
        #[serde(default, skip_serializing_if = "is_default")]
        select_filters_ctrl: bool,
        #[serde(default, skip_serializing_if = "is_default")]
        variations: Vec<Variation>,
    },
}

fn default_visible_macros() -> u8 {
    rlive_core::param::rack::DEFAULT_VISIBLE_MACROS
}

fn is_default_visible(n: &u8) -> bool {
    *n == default_visible_macros()
}

/// Which of Live's rack types a rack is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RackKind {
    Instrument,
    Audio,
    Midi,
    Drum,
}

/// One chain of a rack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    pub id: ChainId,
    pub name: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub devices: Vec<Device>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub zones: Zones,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mix: ChainMix,
    #[serde(default, skip_serializing_if = "is_default")]
    pub color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub info: String,
}

impl Chain {
    pub fn new(name: &str) -> Self {
        Self {
            id: ChainId::new(),
            name: name.into(),
            devices: Vec::new(),
            zones: Zones::default(),
            mix: ChainMix::default(),
            color: None,
            info: String::new(),
        }
    }
}

/// A range of MIDI values and the fade range around it, all 0..127.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub low: u8,
    pub high: u8,
    /// Where the fade below `low` starts, at or under `low`.
    pub fade_low: u8,
    /// Where the fade above `high` ends, at or over `high`.
    pub fade_high: u8,
}

impl Zone {
    pub const fn span(low: u8, high: u8) -> Self {
        Self {
            low,
            high,
            fade_low: low,
            fade_high: high,
        }
    }
}

/// Which notes and Chain Selector values reach a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Zones {
    pub key: Zone,
    pub velocity: Zone,
    pub select: Zone,
    /// Set on the chains of a Drum Rack.
    pub drum: Option<DrumPad>,
}

impl Default for Zones {
    fn default() -> Self {
        Self {
            key: Zone::span(0, 127),
            velocity: Zone::span(1, 127),
            select: Zone::span(0, 0),
            drum: None,
        }
    }
}

/// The note a Drum Rack chain answers to and what it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrumPad {
    /// The note that triggers the chain; `None` is All Notes.
    pub receive: Option<u8>,
    /// The note sent to the devices in the chain.
    pub play: u8,
    /// Choke group, 1..16.
    pub choke: Option<u8>,
}

/// A chain's strip in the rack's mixer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChainMix {
    pub volume_db: f32,
    pub pan: f32,
    /// The Chain Activator.
    pub active: bool,
    pub solo: bool,
    /// Levels into the rack's return chains.
    pub sends: Vec<ChainSend>,
    /// On a return chain: the Set's return track it feeds, or `None` for the rack's own output.
    pub output: Option<TrackId>,
}

impl Default for ChainMix {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            active: true,
            solo: false,
            sends: Vec::new(),
            output: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChainSend {
    pub to: ChainId,
    pub level_db: f32,
}

/// A rack's macro control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    /// What the user called it. Empty leaves it to its mapping's name, or `Macro n`.
    pub name: String,
    /// 0..127, as on the knob.
    #[serde(default, skip_serializing_if = "is_default")]
    pub value: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mappings: Vec<MacroMapping>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub exclude_random: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub exclude_variations: bool,
    /// The user's colour for the knob, if any (manual p. 505).
    #[serde(default, skip_serializing_if = "is_default")]
    pub color: Option<[u8; 3]>,
    /// The user's info text.
    #[serde(default, skip_serializing_if = "is_default")]
    pub info: String,
}

/// A parameter a macro controls, and the range it sweeps. `max` below `min` inverts it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MacroMapping {
    pub target: ParamRef,
    pub min: f64,
    pub max: f64,
}

impl Default for Macro {
    fn default() -> Self {
        Self {
            name: String::new(),
            value: 0.0,
            mappings: Vec::new(),
            exclude_random: false,
            exclude_variations: false,
            color: None,
            info: String::new(),
        }
    }
}

/// Make sure a rack's macro list holds at least `n` entries. They are created on first touch.
pub fn grow_macros(macros: &mut Vec<Macro>, n: usize) {
    let n = n.min(MACRO_COUNT);
    if macros.len() < n {
        macros.resize_with(n, Macro::default);
    }
}

fn macro_index(param: u32) -> Option<usize> {
    rlive_core::param::rack::macro_index(rlive_core::ParamId(param))
}

/// A stored set of macro values, one per macro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variation {
    pub name: String,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    pub bypass: bool,
    /// Plain parameter values by id (built-ins; plugins also keep their own state).
    #[serde(default)]
    pub params: BTreeMap<u32, f64>,
    /// Whether the device card is expanded in the device view.
    #[serde(default = "yes")]
    pub expanded: bool,
    /// The user's info text.
    #[serde(default, skip_serializing_if = "is_default")]
    pub info: String,
    /// Where the device's own sidechain chooser takes its input from.
    #[serde(default, skip_serializing_if = "is_default")]
    pub sidechain: Option<SidechainSource>,
}

/// The source a device's sidechain input is set to, as its built-in chooser shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidechainSource {
    pub track: TrackId,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tap: crate::Tap,
}

impl Device {
    /// The value a parameter is saved with: a macro's knob for a rack, otherwise `params`.
    pub fn stored_param(&self, param: u32) -> Option<f64> {
        if let (DeviceKind::Rack { macros, .. }, Some(n)) = (&self.kind, macro_index(param)) {
            return Some(macros.get(n).map_or(0.0, |m| m.value));
        }
        self.params.get(&param).copied()
    }

    /// Save a parameter's value. Returns whether it had none before, which a rack's macros never
    /// do. See [`stored_param`](Self::stored_param).
    pub fn store_param(&mut self, param: u32, value: f64) -> bool {
        if let (DeviceKind::Rack { macros, .. }, Some(n)) = (&mut self.kind, macro_index(param)) {
            grow_macros(macros, n + 1);
            macros[n].value = value;
            return false;
        }
        self.params.insert(param, value).is_none()
    }

    fn new(kind: DeviceKind, name: &str) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            kind,
            bypass: false,
            params: BTreeMap::new(),
            expanded: true,
            info: String::new(),
            sidechain: None,
        }
    }

    pub fn builtin(type_id: &str, name: &str) -> Self {
        Self::new(
            DeviceKind::Builtin {
                type_id: type_id.into(),
                samples: Vec::new(),
                state: None,
            },
            name,
        )
    }

    pub fn plugin(format: PluginFormat, plugin_id: &str, path: PathBuf, name: &str) -> Self {
        Self::new(
            DeviceKind::Plugin {
                format,
                plugin_id: plugin_id.into(),
                path,
                state: None,
            },
            name,
        )
    }

    /// A rack with no chains yet.
    pub fn rack(kind: RackKind, name: &str) -> Self {
        Self::new(
            DeviceKind::Rack {
                kind,
                chains: Vec::new(),
                returns: Vec::new(),
                macros: Vec::new(),
                visible_macros: default_visible_macros(),
                select_filters_ctrl: false,
                variations: Vec::new(),
            },
            name,
        )
    }

    /// The file loaded in a sample slot of a built-in device.
    pub fn sample(&self, slot: usize) -> Option<&FileRef> {
        match &self.kind {
            DeviceKind::Builtin { samples, .. } => samples.get(slot)?.as_ref(),
            _ => None,
        }
    }

    /// The chains of this device: a rack's chains then its returns, none for anything else.
    pub fn chains(&self) -> impl Iterator<Item = &Chain> {
        let (chains, returns): (&[Chain], &[Chain]) = match &self.kind {
            DeviceKind::Rack {
                chains, returns, ..
            } => (chains, returns),
            _ => (&[], &[]),
        };
        chains.iter().chain(returns)
    }

    pub fn chains_mut(&mut self) -> impl Iterator<Item = &mut Chain> {
        let (chains, returns): (&mut [Chain], &mut [Chain]) = match &mut self.kind {
            DeviceKind::Rack {
                chains, returns, ..
            } => (chains, returns),
            _ => (&mut [], &mut []),
        };
        chains.iter_mut().chain(returns)
    }

    /// Calls `f` on this device and everything nested in it.
    pub fn for_each(&self, f: &mut impl FnMut(&Device)) {
        f(self);
        for chain in self.chains() {
            for d in &chain.devices {
                d.for_each(f);
            }
        }
    }

    pub fn for_each_mut(&mut self, f: &mut impl FnMut(&mut Device)) {
        f(self);
        for chain in self.chains_mut() {
            for d in &mut chain.devices {
                d.for_each_mut(f);
            }
        }
    }
}

pub(crate) mod base64_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        let vals: Vec<u8> = s
            .bytes()
            .filter(|c| !c.is_ascii_whitespace())
            .take_while(|&c| c != b'=')
            .map(|c| ALPHABET.iter().position(|&a| a == c).map(|p| p as u8))
            .collect::<Option<_>>()?;
        for chunk in vals.chunks(4) {
            let mut n = 0u32;
            for (i, v) in chunk.iter().enumerate() {
                n |= (*v as u32) << (18 - 6 * i);
            }
            let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            out.extend_from_slice(&bytes[..chunk.len().saturating_sub(1)]);
        }
        Some(out)
    }

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_some(&encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        match s {
            Some(s) => decode(&s)
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("invalid base64 plugin state")),
            None => Ok(None),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn roundtrip() {
            for len in 0..20 {
                let data: Vec<u8> = (0..len).map(|i| (i * 37 + 5) as u8).collect();
                assert_eq!(super::decode(&super::encode(&data)).unwrap(), data);
            }
            assert_eq!(super::encode(b"Man"), "TWFu");
            assert_eq!(super::encode(b"Ma"), "TWE=");
        }
    }
}
