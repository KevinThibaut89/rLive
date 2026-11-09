//! Format 1, the model as of commit af1ab13. Frozen: never edit these types.
//!
//! Ids, [`Quantize`], [`TimeSignature`] and [`PluginFormat`] did not change in format 2, so they
//! are not copied here, and neither is the base64 codec for plugin state. Whichever format next
//! changes one of them must freeze it.

use crate::device::base64_opt;
use crate::id_walk::walk_ids;
use crate::{FileRef, Track as TrackV2, TrackKind as TrackKindV2, FORMAT_VERSION};
use rlive_core::{ArrClipId, DeviceId, PluginFormat, Quantize, TimeSignature, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteData {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipContent {
    Midi { notes: Vec<NoteData> },
    Audio { path: PathBuf, gain_db: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub color: [u8; 3],
    pub length: u64,
    pub looped: bool,
    pub content: ClipContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrangementClip {
    pub id: ArrClipId,
    pub start: u64,
    pub length: u64,
    pub offset: u64,
    pub clip: Clip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arrangement {
    pub loop_enabled: bool,
    pub loop_start: u64,
    pub loop_end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceKind {
    Builtin {
        type_id: String,
        #[serde(default)]
        sample: Option<PathBuf>,
    },
    Plugin {
        format: PluginFormat,
        plugin_id: String,
        path: PathBuf,
        #[serde(default, with = "base64_opt")]
        state: Option<Vec<u8>>,
    },
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    pub bypass: bool,
    #[serde(default)]
    pub params: BTreeMap<u32, f64>,
    #[serde(default = "yes")]
    pub expanded: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub clips: Vec<Option<Clip>>,
    #[serde(default)]
    pub arrangement: Vec<ArrangementClip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Master {
    pub volume_db: f32,
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub quantize: Quantize,
    pub metronome: bool,
    pub scenes: Vec<Scene>,
    pub tracks: Vec<Track>,
    pub master: Master,
    #[serde(default = "default_arrangement")]
    pub arrangement: Arrangement,
}

fn default_arrangement() -> Arrangement {
    Arrangement {
        loop_enabled: false,
        loop_start: 0,
        loop_end: 16 * rlive_core::PPQ,
    }
}

fn upgrade_note(n: NoteData) -> crate::NoteData {
    crate::NoteData::new(n.start, n.length, n.key, n.velocity)
}

fn upgrade_clip(c: Clip) -> crate::Clip {
    let content = match c.content {
        ClipContent::Midi { notes } => {
            crate::ClipContent::midi(notes.into_iter().map(upgrade_note).collect())
        }
        ClipContent::Audio { path, gain_db } => crate::ClipContent::audio(path, gain_db),
    };
    crate::Clip {
        name: c.name,
        color: c.color,
        length: c.length,
        looped: c.looped,
        content,
        ..crate::Clip::default()
    }
}

fn upgrade_arrangement_clip(c: ArrangementClip) -> crate::ArrangementClip {
    crate::ArrangementClip {
        id: c.id,
        start: c.start,
        length: c.length,
        offset: c.offset,
        clip: upgrade_clip(c.clip),
    }
}

fn upgrade_device(d: Device) -> crate::Device {
    let kind = match d.kind {
        DeviceKind::Builtin { type_id, sample } => crate::DeviceKind::Builtin {
            type_id,
            samples: sample
                .map(|p| vec![Some(FileRef::new(p))])
                .unwrap_or_default(),
            state: None,
        },
        DeviceKind::Plugin {
            format,
            plugin_id,
            path,
            state,
        } => crate::DeviceKind::Plugin {
            format,
            plugin_id,
            path,
            state,
        },
    };
    crate::Device {
        id: d.id,
        name: d.name,
        kind,
        bypass: d.bypass,
        params: d.params,
        expanded: d.expanded,
        info: String::new(),
        sidechain: None,
        mpe: None,
    }
}

/// A blank track of `kind` to build on.
fn blank_track(id: TrackId, kind: TrackKindV2) -> TrackV2 {
    let mut track = TrackV2::new("", kind, [0; 3], 0);
    track.id = id;
    track
}

fn upgrade_track(t: Track) -> TrackV2 {
    let kind = match t.kind {
        TrackKind::Midi => TrackKindV2::Midi,
        TrackKind::Audio => TrackKindV2::Audio,
    };
    TrackV2 {
        name: t.name,
        color: t.color,
        volume_db: t.volume_db,
        pan: t.pan,
        mute: t.mute,
        solo: t.solo,
        arm: t.arm,
        devices: t.devices.into_iter().map(upgrade_device).collect(),
        clips: t.clips.into_iter().map(|c| c.map(upgrade_clip)).collect(),
        arrangement: t
            .arrangement
            .into_iter()
            .map(upgrade_arrangement_clip)
            .collect(),
        ..blank_track(t.id, kind)
    }
}

/// A fresh id for the Master, which format 1 didn't give one, that can't collide with the file's.
fn master_id(old: &Project) -> TrackId {
    let mut highest = 0;
    walk_ids(old, |id| highest = highest.max(id));
    TrackId(highest + 1)
}

pub fn upgrade(old: Project) -> crate::Project {
    let master = TrackV2 {
        name: "Master".into(),
        color: crate::project::MASTER_COLOR,
        volume_db: old.master.volume_db,
        devices: old
            .master
            .devices
            .iter()
            .cloned()
            .map(upgrade_device)
            .collect(),
        ..blank_track(master_id(&old), TrackKindV2::Master)
    };
    crate::Project {
        version: FORMAT_VERSION,
        tempo: old.tempo,
        time_signature: old.time_signature,
        quantize: old.quantize,
        metronome: old.metronome,
        scenes: old
            .scenes
            .into_iter()
            .map(|s| crate::Scene::new(s.name))
            .collect(),
        tracks: old.tracks.into_iter().map(upgrade_track).collect(),
        master,
        arrangement: crate::Arrangement {
            loop_enabled: old.arrangement.loop_enabled,
            loop_start: old.arrangement.loop_start,
            loop_end: old.arrangement.loop_end,
            ..crate::Arrangement::default()
        },
        ..crate::Project::empty()
    }
}
