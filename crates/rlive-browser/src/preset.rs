//! The preset file format: `.rlpreset`, RON text, stored in the User Library.
//!
//! A preset is a saved device, rack chain, track default or plug-in configuration. Every file
//! says which format it was written in, so the shape can change: [`Preset::parse`] brings any
//! format it knows up to the current one.
//!
//! Ids in the file mean nothing outside it; whoever loads a preset gives everything fresh ones.

use rlive_core::PluginFormat;
use rlive_project::{Chain, CompareSlot, Device, DeviceKind, RackKind, Route, TrackKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::item::Role;

pub const PRESET_EXTENSION: &str = "rlpreset";
/// The format this version writes.
pub const PRESET_FORMAT: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("could not read the preset: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a preset file: {0}")]
    Parse(String),
    #[error("this preset was saved by a newer version of rLive (format {0})")]
    TooNew(u32),
    #[error("unknown preset format {0}")]
    UnknownFormat(u32),
}

/// A track's devices and mixer settings, saved as a default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackTemplate {
    pub kind: TrackKind,
    #[serde(default)]
    pub volume_db: f32,
    #[serde(default)]
    pub pan: f32,
    /// Send levels in dB, by the position of the return track.
    #[serde(default)]
    pub sends: Vec<f32>,
    #[serde(default)]
    pub input: Route,
    #[serde(default)]
    pub output: Route,
    #[serde(default)]
    pub devices: Vec<Device>,
}

/// Which parameters of a plug-in show on its device panel: no values (manual p. 468).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub format: PluginFormat,
    pub plugin_id: String,
    /// What its panel shows, in order.
    pub params: Vec<u32>,
    /// The X and Y parameters of its X-Y field.
    #[serde(default)]
    pub xy: [Option<u32>; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    Device(Box<Device>),
    Chain(Box<Chain>),
    Track(Box<TrackTemplate>),
    PluginConfig(PluginConfig),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub format: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub info: String,
    /// Names of factory tags the preset comes with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub payload: Payload,
}

/// Format 1: a built-in device's settings, flat. Nothing of a device's own structure is in it.
#[derive(Deserialize)]
struct FormatOne {
    name: String,
    type_id: String,
    #[serde(default)]
    info: String,
    #[serde(default)]
    params: BTreeMap<u32, f64>,
    #[serde(default)]
    samples: Vec<Option<rlive_project::FileRef>>,
    #[serde(default)]
    state: Option<String>,
}

impl FormatOne {
    fn upgrade(self) -> Preset {
        let mut device = Device::builtin(&self.type_id, &self.name);
        device.params = self.params;
        device.info = self.info.clone();
        if let DeviceKind::Builtin { samples, state, .. } = &mut device.kind {
            *samples = self.samples;
            *state = self.state;
        }
        Preset {
            format: PRESET_FORMAT,
            name: self.name,
            info: self.info,
            tags: Vec::new(),
            payload: Payload::Device(Box::new(device)),
        }
    }
}

#[derive(Deserialize)]
struct Probe {
    format: u32,
}

impl Preset {
    fn new(name: &str, info: String, payload: Payload) -> Self {
        Self {
            format: PRESET_FORMAT,
            name: name.to_string(),
            info,
            tags: Vec::new(),
            payload,
        }
    }

    /// A preset of a device as it is now. What only means something in this Set stays out.
    pub fn of_device(device: &Device, name: &str) -> Self {
        let mut d = device.clone();
        strip_for_storage(&mut d);
        d.name = name.to_string();
        let info = d.info.clone();
        Self::new(name, info, Payload::Device(Box::new(d)))
    }

    pub fn of_chain(chain: &Chain, name: &str) -> Self {
        let mut c = chain.clone();
        for d in &mut c.devices {
            strip_for_storage(d);
        }
        c.mix.output = None;
        c.mix.sends.clear();
        c.name = name.to_string();
        let info = c.info.clone();
        Self::new(name, info, Payload::Chain(Box::new(c)))
    }

    pub fn of_track(template: TrackTemplate, name: &str) -> Self {
        Self::new(name, String::new(), Payload::Track(Box::new(template)))
    }

    pub fn of_plugin_config(config: PluginConfig, name: &str) -> Self {
        Self::new(name, String::new(), Payload::PluginConfig(config))
    }

    /// Reads a preset of any supported format, upgrading it to the current one.
    pub fn parse(text: &str) -> Result<Self, PresetError> {
        let probe: Probe = ron::from_str(text).map_err(|e| PresetError::Parse(e.to_string()))?;
        match probe.format {
            PRESET_FORMAT => ron::from_str(text).map_err(|e| PresetError::Parse(e.to_string())),
            1 => {
                let one: FormatOne =
                    ron::from_str(text).map_err(|e| PresetError::Parse(e.to_string()))?;
                Ok(one.upgrade())
            }
            n if n > PRESET_FORMAT => Err(PresetError::TooNew(n)),
            n => Err(PresetError::UnknownFormat(n)),
        }
    }

    pub fn to_text(&self) -> String {
        let cfg = ron::ser::PrettyConfig::new().depth_limit(8);
        ron::ser::to_string_pretty(self, cfg).unwrap_or_default()
    }

    pub fn load(path: &Path) -> Result<Self, PresetError> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// Writes the file whole or not at all, creating its folder if it has to.
    pub fn save(&self, path: &Path) -> Result<(), PresetError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_atomic(path, self.to_text().as_bytes())?;
        Ok(())
    }

    /// The device whose browser folder this preset sits in.
    pub fn device_key(&self) -> Option<String> {
        let Payload::Device(d) = &self.payload else {
            return None;
        };
        Some(device_key(d))
    }

    pub fn role(&self) -> Option<Role> {
        let Payload::Device(d) = &self.payload else {
            return None;
        };
        device_role(d)
    }

    /// Every audio file the preset refers to.
    pub fn file_refs_mut(&mut self) -> Vec<&mut rlive_project::FileRef> {
        fn walk<'a>(d: &'a mut Device, out: &mut Vec<&'a mut rlive_project::FileRef>) {
            match &mut d.kind {
                DeviceKind::Builtin { samples, .. } => {
                    out.extend(samples.iter_mut().flatten());
                }
                DeviceKind::Rack {
                    chains, returns, ..
                } => {
                    for c in chains.iter_mut().chain(returns.iter_mut()) {
                        for d in &mut c.devices {
                            walk(d, out);
                        }
                    }
                }
                DeviceKind::Plugin { .. } => {}
            }
        }
        let mut out = Vec::new();
        match &mut self.payload {
            Payload::Device(d) => walk(d, &mut out),
            Payload::Chain(c) => {
                for d in &mut c.devices {
                    walk(d, &mut out);
                }
            }
            Payload::Track(t) => {
                for d in &mut t.devices {
                    walk(d, &mut out);
                }
            }
            Payload::PluginConfig(_) => {}
        }
        out
    }
}

/// The key a device's presets are filed under.
pub fn device_key(d: &Device) -> String {
    match &d.kind {
        DeviceKind::Builtin { type_id, .. } => type_id.clone(),
        DeviceKind::Plugin {
            format, plugin_id, ..
        } => format!("plugin:{}:{plugin_id}", format.label()),
        DeviceKind::Rack { kind, .. } => format!("rack:{kind:?}"),
    }
}

/// What a device does, for what the kind of device says: a plug-in's is only known to the host.
pub fn device_role(d: &Device) -> Option<Role> {
    match &d.kind {
        DeviceKind::Builtin { type_id, .. } => rlive_devices::find(type_id).map(|i| match i.role {
            rlive_devices::DeviceRole::Instrument => Role::Instrument,
            rlive_devices::DeviceRole::AudioEffect => Role::AudioEffect,
            rlive_devices::DeviceRole::MidiEffect => Role::MidiEffect,
        }),
        DeviceKind::Rack { kind, .. } => Some(match kind {
            RackKind::Instrument | RackKind::Drum => Role::Instrument,
            RackKind::Audio => Role::AudioEffect,
            RackKind::Midi => Role::MidiEffect,
        }),
        DeviceKind::Plugin { .. } => None,
    }
}

/// Takes out what only means something in the Set the device is in, and flattens its A/B: a
/// preset holds the selected state in both (manual p. 457).
pub fn strip_for_storage(d: &mut Device) {
    d.for_each_mut(&mut |d| {
        d.sidechain = None;
        d.expanded = true;
        d.compare = None;
    });
    for c in d.chains_mut() {
        c.mix.output = None;
    }
}

/// What a `.rlclip` holds, RON text: the clip, and the track's devices that come with it when it's
/// dropped on an empty track (manual p. 134). The file records nothing about where it came from
/// beyond that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipFile {
    pub format: u32,
    pub clip: rlive_project::Clip,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<Device>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackKind>,
}

/// The format [`ClipFile`] writes.
pub const CLIP_FORMAT: u32 = 2;

impl ClipFile {
    pub fn new(clip: rlive_project::Clip) -> Self {
        Self {
            format: CLIP_FORMAT,
            clip,
            devices: Vec::new(),
            track: None,
        }
    }

    /// Every audio file the clip and its devices refer to.
    pub fn for_each_file_ref_mut(&mut self, f: &mut impl FnMut(&mut rlive_project::FileRef)) {
        match &mut self.clip.content {
            rlive_project::ClipContent::Audio { file, .. } => f(file),
            rlive_project::ClipContent::Midi { .. } => {}
        }
        if let Some(frozen) = &mut self.clip.frozen {
            f(&mut frozen.file);
        }
        for d in &mut self.devices {
            d.for_each_mut(&mut |d| {
                if let DeviceKind::Builtin { samples, .. } = &mut d.kind {
                    samples.iter_mut().flatten().for_each(&mut *f);
                }
            });
        }
    }
}

/// A clip file, of this format or the bare clip that came before it.
pub fn load_clip_file(path: &Path) -> Result<ClipFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    ron::from_str::<ClipFile>(&text)
        .or_else(|_| ron::from_str::<rlive_project::Clip>(&text).map(ClipFile::new))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes a `.rlclip`.
pub fn save_clip_file(path: &Path, file: &ClipFile) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = ron::ser::to_string_pretty(file, ron::ser::PrettyConfig::default())
        .map_err(std::io::Error::other)?;
    write_atomic(path, text.as_bytes())
}

/// A saved clip, without its devices.
pub fn load_clip(path: &Path) -> Result<rlive_project::Clip, String> {
    load_clip_file(path).map(|f| f.clip)
}

/// Writes a file so that a crash leaves either the old one or the new one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// A filename for a preset of this name, safe on every platform.
pub fn file_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c => c,
        })
        .collect();
    format!("{}.{PRESET_EXTENSION}", safe.trim())
}

/// A path for a new preset in `dir` that doesn't overwrite one that's there.
pub fn unused_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(file_name(name));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(file_name(&format!("{name} {n}"))))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Which A/B state a preset of this device's values represents.
pub fn selected_state(d: &Device) -> CompareSlot {
    d.compare.as_ref().map_or(CompareSlot::A, |c| c.selected)
}
