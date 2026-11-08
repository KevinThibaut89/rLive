//! The rLive project model: the UI thread's source of truth, saved as RON (`.rlive`).

use rlive_core::{ids::reserve_id, DeviceId, PluginFormat, Quantize, TimeSignature, TrackId, PPQ};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE_EXTENSION: &str = "rlive";
pub const FORMAT_VERSION: u32 = 1;

/// Track/clip colour palette (Ableton-ish).
pub const PALETTE: [[u8; 3]; 12] = [
    [255, 148, 166],
    [255, 165, 41],
    [204, 153, 39],
    [247, 244, 124],
    [191, 251, 0],
    [26, 255, 47],
    [37, 255, 168],
    [92, 255, 232],
    [139, 197, 255],
    [84, 128, 228],
    [146, 167, 255],
    [216, 108, 228],
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteData {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipContent {
    Midi { notes: Vec<NoteData> },
    Audio { path: PathBuf, gain_db: f32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub color: [u8; 3],
    /// Loop length in ticks.
    pub length: u64,
    pub looped: bool,
    pub content: ClipContent,
}

impl Clip {
    pub fn new_midi(name: &str, color: [u8; 3], bars: u64, sig: TimeSignature) -> Self {
        Self {
            name: name.into(),
            color,
            length: bars * sig.ticks_per_bar(),
            looped: true,
            content: ClipContent::Midi { notes: Vec::new() },
        }
    }

    pub fn notes(&self) -> &[NoteData] {
        match &self.content {
            ClipContent::Midi { notes } => notes,
            ClipContent::Audio { .. } => &[],
        }
    }

    pub fn notes_mut(&mut self) -> Option<&mut Vec<NoteData>> {
        match &mut self.content {
            ClipContent::Midi { notes } => Some(notes),
            ClipContent::Audio { .. } => None,
        }
    }

    pub fn is_midi(&self) -> bool {
        matches!(self.content, ClipContent::Midi { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceKind {
    Builtin {
        type_id: String,
        #[serde(default)]
        sample: Option<PathBuf>,
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
}

fn yes() -> bool {
    true
}

impl Device {
    pub fn builtin(type_id: &str, name: &str) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            kind: DeviceKind::Builtin {
                type_id: type_id.into(),
                sample: None,
            },
            bypass: false,
            params: BTreeMap::new(),
            expanded: true,
        }
    }

    pub fn plugin(format: PluginFormat, plugin_id: &str, path: PathBuf, name: &str) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            kind: DeviceKind::Plugin {
                format,
                plugin_id: plugin_id.into(),
                path,
                state: None,
            },
            bypass: false,
            params: BTreeMap::new(),
            expanded: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub color: [u8; 3],
    pub kind: TrackKind,
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
    pub devices: Vec<Device>,
    /// One entry per scene.
    pub clips: Vec<Option<Clip>>,
}

impl Track {
    pub fn new(name: &str, kind: TrackKind, color: [u8; 3], scenes: usize) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            color,
            kind,
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            arm: false,
            devices: Vec::new(),
            clips: vec![None; scenes],
        }
    }

    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Master {
    pub volume_db: f32,
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub quantize: Quantize,
    pub metronome: bool,
    pub scenes: Vec<Scene>,
    pub tracks: Vec<Track>,
    pub master: Master,
}

impl Default for Project {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse project: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("could not serialize project: {0}")]
    Serialize(#[from] ron::Error),
    #[error("project was saved by a newer rLive (format {0})")]
    TooNew(u32),
}

impl Project {
    pub fn empty() -> Self {
        Self {
            version: FORMAT_VERSION,
            tempo: 120.0,
            time_signature: TimeSignature::default(),
            quantize: Quantize::Bar,
            metronome: false,
            scenes: (1..=8)
                .map(|i| Scene {
                    name: i.to_string(),
                })
                .collect(),
            tracks: Vec::new(),
            master: Master {
                volume_db: 0.0,
                devices: Vec::new(),
            },
        }
    }

    /// The template a new project starts from: two MIDI tracks with a synth and an audio track.
    pub fn new_default() -> Self {
        let mut p = Self::empty();
        let mut t1 = Track::new("1 Poly", TrackKind::Midi, PALETTE[9], p.scenes.len());
        t1.devices.push(Device::builtin("rlive.poly", "Poly Synth"));
        let mut t2 = Track::new("2 Poly", TrackKind::Midi, PALETTE[1], p.scenes.len());
        t2.devices.push(Device::builtin("rlive.poly", "Poly Synth"));
        t2.devices.push(Device::builtin("rlive.delay", "Delay"));
        let t3 = Track::new("3 Audio", TrackKind::Audio, PALETTE[5], p.scenes.len());
        // A demo clip so pressing play makes sound immediately.
        let mut clip = Clip::new_midi("Chords", PALETTE[9], 2, p.time_signature);
        if let Some(notes) = clip.notes_mut() {
            let bar = p.time_signature.ticks_per_bar();
            for (i, chord) in [[57u8, 60, 64], [53, 57, 60]].iter().enumerate() {
                for &key in chord {
                    notes.push(NoteData {
                        start: i as u64 * bar,
                        length: bar - PPQ / 4,
                        key,
                        velocity: 0.8,
                    });
                }
            }
        }
        t1.clips[0] = Some(clip);
        p.tracks = vec![t1, t2, t3];
        p
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn next_color(&self) -> [u8; 3] {
        PALETTE[(self.tracks.len() * 5 + 9) % PALETTE.len()]
    }

    /// Make every track have exactly one clip slot per scene.
    pub fn normalize(&mut self) {
        let n = self.scenes.len();
        for t in self.tracks.iter_mut() {
            t.clips.resize(n, None);
        }
    }

    pub fn add_scene(&mut self) {
        let name = (self.scenes.len() + 1).to_string();
        self.scenes.push(Scene { name });
        self.normalize();
    }

    pub fn to_ron(&self) -> Result<String, ProjectError> {
        let cfg = ron::ser::PrettyConfig::new().depth_limit(8);
        Ok(ron::ser::to_string_pretty(self, cfg)?)
    }

    pub fn from_ron(s: &str) -> Result<Self, ProjectError> {
        let mut p: Project = ron::from_str(s)?;
        if p.version > FORMAT_VERSION {
            return Err(ProjectError::TooNew(p.version));
        }
        p.normalize();
        // Make sure newly created ids never collide with loaded ones.
        for t in &p.tracks {
            reserve_id(t.id.0);
            for d in &t.devices {
                reserve_id(d.id.0);
            }
        }
        for d in &p.master.devices {
            reserve_id(d.id.0);
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        let s = self.to_ron()?;
        // Write atomically so a crash never leaves a half-written project.
        let tmp = path.with_extension("rlive.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        Self::from_ron(&std::fs::read_to_string(path)?)
    }
}

mod base64_opt {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip() {
        let mut p = Project::new_default();
        p.master
            .devices
            .push(Device::builtin("rlive.reverb", "Reverb"));
        let mut plug = Device::plugin(
            PluginFormat::Clap,
            "com.example.gain",
            "/x/y.clap".into(),
            "Gain",
        );
        if let DeviceKind::Plugin { state, .. } = &mut plug.kind {
            *state = Some(vec![0, 1, 2, 250, 251, 252, 7]);
        }
        plug.params.insert(3, 0.25);
        p.tracks[1].devices.push(plug);
        p.tracks[2].clips[1] = Some(Clip {
            name: "Loop".into(),
            color: PALETTE[2],
            length: 3840,
            looped: true,
            content: ClipContent::Audio {
                path: "/tmp/loop.wav".into(),
                gain_db: -3.0,
            },
        });
        let dir = std::env::temp_dir().join(format!("rlive-proj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.rlive");
        p.save(&path).unwrap();
        let loaded = Project::load(&path).unwrap();
        assert_eq!(p, loaded);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ids_are_reserved_after_load() {
        let mut p = Project::new_default();
        p.tracks[0].id = TrackId(999_999);
        let s = p.to_ron().unwrap();
        let loaded = Project::from_ron(&s).unwrap();
        assert_eq!(loaded.tracks[0].id.0, 999_999);
        assert!(TrackId::new().0 > 999_999);
    }

    #[test]
    fn normalize_pads_clips() {
        let mut p = Project::new_default();
        p.add_scene();
        assert!(p.tracks.iter().all(|t| t.clips.len() == 9));
    }
}
