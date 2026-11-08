use crate::{ArrangementClip, Clip, Device};
use rlive_core::{DeviceId, TrackId};
use serde::{Deserialize, Serialize};

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
    /// Clips on the timeline, sorted by start and not overlapping.
    #[serde(default)]
    pub arrangement: Vec<ArrangementClip>,
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
            arrangement: Vec::new(),
        }
    }

    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }
}
