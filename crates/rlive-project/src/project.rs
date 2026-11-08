use crate::{Arrangement, Clip, Device, NoteData, Scene, Track, TrackKind};
use rlive_core::{ids::reserve_id, Quantize, TimeSignature, TrackId, PPQ};
use serde::{Deserialize, Serialize};
use std::path::Path;

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
    #[serde(default)]
    pub arrangement: Arrangement,
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
            arrangement: Arrangement::default(),
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
            t.arrangement.sort_by_key(|c| c.start);
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
            for c in &t.arrangement {
                reserve_id(c.id.0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClipContent, DeviceKind};
    use rlive_core::PluginFormat;

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
