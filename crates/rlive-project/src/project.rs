use crate::defaults::is_default;
use crate::id_walk::walk_ids;
use crate::migrate;
use crate::recording::{MetronomeOptions, RecordQuantize};
use crate::{
    Arrangement, Clip, ClipContent, Device, DeviceKind, ExternalRoute, FileRef, GroovePool,
    KeyMapping, MidiMapping, NoteData, Scene, Track, TrackKind, Tuning,
};
use rlive_core::{ids::reserve_id, Quantize, TimeSignature, TrackId, PPQ};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const FILE_EXTENSION: &str = "rlive";
/// The format this build writes. An added field only needs `serde(default)`; a new version is
/// needed only for a rename or a change of meaning (see `migrate/mod.rs`).
pub const FORMAT_VERSION: u32 = 2;

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

pub(crate) const MASTER_COLOR: [u8; 3] = [154, 154, 154];

/// The crossfader's state; tracks say which side they are on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Crossfader {
    /// -1 is full A, 1 is full B.
    pub position: f32,
    /// One of Live's seven curves, by its index in the list.
    pub curve: u8,
}

impl Default for Crossfader {
    fn default() -> Self {
        Self {
            position: 0.0,
            curve: 3,
        }
    }
}

/// The Solo/Cue switch and the Cue Out (manual pp. 406-407).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CueSettings {
    /// Solo switches are replaced by Cue switches.
    pub cue_mode: bool,
    /// The hardware output for cueing: one channel, or the first of a stereo pair.
    pub out: Option<ExternalRoute>,
    pub volume_db: f32,
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
    /// Always a track of kind [`TrackKind::Master`], with no clip slots.
    pub master: Track,
    #[serde(default)]
    pub arrangement: Arrangement,
    /// Tracks of kind [`TrackKind::Return`].
    #[serde(default, skip_serializing_if = "is_default")]
    pub returns: Vec<Track>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub crossfader: Crossfader,
    #[serde(default, skip_serializing_if = "is_default")]
    pub cue: CueSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub groove_pool: GroovePool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tuning: Option<Tuning>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub midi_mappings: Vec<MidiMapping>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub key_mappings: Vec<KeyMapping>,
    /// Soloing a clip or group track leaves the return tracks audible.
    #[serde(
        default = "crate::defaults::yes",
        skip_serializing_if = "crate::defaults::is_true"
    )]
    pub solo_in_place: bool,
    /// Keep tracks in sync through the latency of devices and plug-ins, and allow Track Delay.
    #[serde(
        default = "crate::defaults::yes",
        skip_serializing_if = "crate::defaults::is_true"
    )]
    pub delay_compensation: bool,
    /// The Enable Follow Actions Globally button.
    #[serde(
        default = "crate::defaults::yes",
        skip_serializing_if = "crate::defaults::is_true"
    )]
    pub follow_actions_on: bool,
    /// Tracks with input monitoring on skip compensation for the lowest possible latency.
    #[serde(default, skip_serializing_if = "is_default")]
    pub reduced_latency_when_monitoring: bool,
    /// The metronome's menu.
    #[serde(default, skip_serializing_if = "is_default")]
    pub metronome_options: MetronomeOptions,
    /// New MIDI takes are snapped to this grid as they are recorded.
    #[serde(default, skip_serializing_if = "is_default")]
    pub record_quantize: RecordQuantize,
    /// MIDI recorded into the Arrangement mixes with what is there, rather than replacing it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub midi_arrangement_overdub: bool,
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
    #[error("project format {0} is not one rLive has ever written")]
    UnknownVersion(u32),
    #[error("could not back up {path} before upgrading it: {source}")]
    Backup {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Copies a file about to be upgraded to `<name>.v<old>.bak`. An existing backup is the older,
/// more valuable one, so it is never replaced; any other failure stops the load.
fn back_up(path: &Path, version: u32, text: &str) -> Result<(), ProjectError> {
    let backup = path.with_extension(format!("v{version}.bak"));
    let failed = |source| ProjectError::Backup {
        path: backup.clone(),
        source,
    };
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
    {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(failed(e)),
    };
    if let Err(e) = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&backup);
        return Err(failed(e));
    }
    Ok(())
}

impl Project {
    pub fn empty() -> Self {
        Self {
            version: FORMAT_VERSION,
            tempo: 120.0,
            time_signature: TimeSignature::default(),
            quantize: Quantize::Bar,
            metronome: false,
            scenes: (0..8).map(|_| Scene::new("")).collect(),
            tracks: Vec::new(),
            master: Track::new("Master", TrackKind::Master, MASTER_COLOR, 0),
            arrangement: Arrangement::default(),
            returns: Vec::new(),
            crossfader: Crossfader::default(),
            cue: CueSettings::default(),
            groove_pool: GroovePool::default(),
            tuning: None,
            midi_mappings: Vec::new(),
            key_mappings: Vec::new(),
            solo_in_place: true,
            delay_compensation: true,
            follow_actions_on: true,
            reduced_latency_when_monitoring: false,
            metronome_options: MetronomeOptions::default(),
            record_quantize: RecordQuantize::default(),
            midi_arrangement_overdub: false,
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
                    notes.push(NoteData::new(i as u64 * bar, bar - PPQ / 4, key, 0.8));
                }
            }
        }
        t1.clips[0] = Some(clip);
        p.tracks = vec![t1, t2, t3];
        p
    }

    /// A clip or group track, or a return track.
    /// Calls `f` for every reference to an audio file in the project: clips in Session slots, on
    /// the Arrangement and in take lanes, and the samples of devices at any depth.
    pub fn for_each_file_ref(&self, f: &mut impl FnMut(&FileRef)) {
        for t in self.tracks.iter().chain(&self.returns) {
            for c in t.clips.iter().flatten() {
                if let ClipContent::Audio { file, .. } = &c.content {
                    f(file);
                }
            }
            for c in t
                .arrangement
                .iter()
                .chain(t.take_lanes.iter().flat_map(|l| &l.clips))
            {
                if let ClipContent::Audio { file, .. } = &c.clip.content {
                    f(file);
                }
            }
            for d in &t.devices {
                d.for_each(&mut |d| {
                    if let DeviceKind::Builtin { samples, .. } = &d.kind {
                        samples.iter().flatten().for_each(&mut *f);
                    }
                });
            }
        }
        for d in &self.master.devices {
            d.for_each(&mut |d| {
                if let DeviceKind::Builtin { samples, .. } = &d.kind {
                    samples.iter().flatten().for_each(&mut *f);
                }
            });
        }
    }

    /// [`for_each_file_ref`](Self::for_each_file_ref), for filling references in.
    pub fn for_each_file_ref_mut(&mut self, f: &mut impl FnMut(&mut FileRef)) {
        for t in self.tracks.iter_mut().chain(self.returns.iter_mut()) {
            for c in t.clips.iter_mut().flatten() {
                if let ClipContent::Audio { file, .. } = &mut c.content {
                    f(file);
                }
            }
            for c in t
                .arrangement
                .iter_mut()
                .chain(t.take_lanes.iter_mut().flat_map(|l| &mut l.clips))
            {
                if let ClipContent::Audio { file, .. } = &mut c.clip.content {
                    f(file);
                }
            }
            for d in &mut t.devices {
                d.for_each_mut(&mut |d| {
                    if let DeviceKind::Builtin { samples, .. } = &mut d.kind {
                        samples.iter_mut().flatten().for_each(&mut *f);
                    }
                });
            }
        }
        for d in &mut self.master.devices {
            d.for_each_mut(&mut |d| {
                if let DeviceKind::Builtin { samples, .. } = &mut d.kind {
                    samples.iter_mut().flatten().for_each(&mut *f);
                }
            });
        }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().chain(&self.returns).find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks
            .iter_mut()
            .chain(self.returns.iter_mut())
            .find(|t| t.id == id)
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
        self.scenes.push(Scene::new(""));
        self.normalize();
    }

    /// Rearrange the scenes and every track's slots: entry `new` of `map` is the scene that was
    /// at that index, or an empty new one for an index past the end. Scenes not listed are gone.
    pub fn remap_scenes(&mut self, map: &[usize]) {
        let mut old = std::mem::take(&mut self.scenes);
        self.scenes = map
            .iter()
            .map(|i| {
                old.get_mut(*i)
                    .map_or_else(|| Scene::new(""), |s| std::mem::replace(s, Scene::new("")))
            })
            .collect();
        for t in self.tracks.iter_mut() {
            let mut clips = std::mem::take(&mut t.clips);
            t.clips = map
                .iter()
                .map(|i| clips.get_mut(*i).and_then(Option::take))
                .collect();
            let no_stop = std::mem::take(&mut t.no_stop);
            t.no_stop = map
                .iter()
                .enumerate()
                .filter(|(_, old)| no_stop.binary_search(&(**old as u32)).is_ok())
                .map(|(new, _)| new as u32)
                .collect();
        }
    }

    pub fn to_ron(&self) -> Result<String, ProjectError> {
        let cfg = ron::ser::PrettyConfig::new().depth_limit(8);
        Ok(ron::ser::to_string_pretty(self, cfg)?)
    }

    /// Parses a project of any supported format, upgrading it to the current one.
    pub fn from_ron(s: &str) -> Result<Self, ProjectError> {
        let version = migrate::probe_version(s)?;
        let mut p = migrate::to_current(s, version)?;
        p.finish_loading();
        Ok(p)
    }

    /// Tidies a just-parsed project, and makes sure new ids never collide with loaded ones.
    fn finish_loading(&mut self) {
        self.normalize();
        walk_ids(self, reserve_id);
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        let s = self.to_ron()?;
        // Write atomically so a crash never leaves a half-written project.
        let tmp = path.with_extension("rlive.tmp");
        fs::write(&tmp, s)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Loads a project. A file in an older format is first copied to `<name>.v<old>.bak` so the
    /// upgrade can never lose it; the load fails if that copy can't be made.
    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let text = fs::read_to_string(path)?;
        let version = migrate::probe_version(&text)?;
        if (migrate::OLDEST_VERSION..FORMAT_VERSION).contains(&version) {
            back_up(path, version, &text)?;
        }
        let mut p = migrate::to_current(&text, version)?;
        p.finish_loading();
        Ok(p)
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
            content: ClipContent::audio("/tmp/loop.wav", -3.0),
            ..Clip::default()
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
    fn nested_ids_are_reserved_after_load() {
        let mut p = Project::new_default();
        let mut rack = Device::rack(crate::RackKind::Instrument, "r");
        let mut chain = crate::Chain::new("c");
        let mut inner = Device::builtin("rlive.poly", "p");
        inner.id = rlive_core::DeviceId(2_000_000);
        chain.devices.push(inner);
        chain.id = rlive_core::ChainId(2_000_100);
        if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
            chains.push(chain);
        }
        p.master.devices.push(rack);
        let s = p.to_ron().unwrap();
        let loaded = Project::from_ron(&s).unwrap();
        assert_eq!(p, loaded);
        assert!(rlive_core::DeviceId::new().0 > 2_000_100);
    }

    #[test]
    fn a_new_file_is_at_the_current_version() {
        assert_eq!(Project::empty().version, FORMAT_VERSION);
    }

    #[test]
    fn remapping_scenes_moves_their_slots_and_stop_buttons() {
        let mut p = Project::new_default();
        p.scenes[0].name = "a".into();
        p.scenes[1].name = "b".into();
        p.tracks[0].set_stop_button(1, false);
        p.remap_scenes(&[1, usize::MAX, 0]);
        assert_eq!(p.scenes.len(), 3);
        assert_eq!(p.scenes[0].name, "b");
        assert_eq!(p.scenes[1].name, "");
        assert_eq!(p.scenes[2].name, "a");
        assert!(!p.tracks[0].has_stop_button(0));
        assert!(p.tracks[0].has_stop_button(1));
        assert_eq!(p.tracks[0].clips.len(), 3);
    }

    #[test]
    fn normalize_pads_clips() {
        let mut p = Project::new_default();
        p.add_scene();
        assert!(p.tracks.iter().all(|t| t.clips.len() == 9));
    }
}
