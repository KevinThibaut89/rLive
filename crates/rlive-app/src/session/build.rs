//! Builds engine nodes from project data.

use crate::peaks::Peaks;
use rlive_core::{DeviceId, ParamInfo, Processor};
use rlive_devices::{DeviceInit, DeviceRole, SampleBuffer};
use rlive_engine::shared::TrackShared;
use rlive_engine::{
    ArrangementLane, AudioClip, ClipData, DeviceSlot, LaneClip, MasterNode, MidiClip, MixSettings,
    Note, NullProcessor, TrackNode,
};
use rlive_plugin_host::scanner::PluginCache;
use rlive_plugin_host::{PluginDescriptor, PluginHandle};
use rlive_project::{ArrangementClip, Clip, ClipContent, Device, DeviceKind, Project, Track};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// UI-side state of one device in a chain.
pub struct DeviceRuntime {
    pub params: Vec<ParamInfo>,
    pub plugin: Option<Box<dyn PluginHandle>>,
    pub error: Option<String>,
    pub role: DeviceRole,
}

impl DeviceRuntime {
    pub fn format_value(&self, p: &ParamInfo, value: f64) -> String {
        self.plugin
            .as_ref()
            .and_then(|h| h.format_value(p.id, value))
            .unwrap_or_else(|| p.format(value))
    }
}

/// Loads audio files once and shares them.
#[derive(Default)]
pub struct SampleCache {
    samples: HashMap<PathBuf, Arc<SampleBuffer>>,
    peaks: HashMap<PathBuf, Arc<Peaks>>,
}

impl SampleCache {
    pub fn get(&mut self, path: &Path) -> Result<Arc<SampleBuffer>, String> {
        if let Some(s) = self.samples.get(path) {
            return Ok(s.clone());
        }
        let s = Arc::new(
            rlive_devices::load_wav(path).map_err(|e| format!("{}: {e}", path.display()))?,
        );
        self.peaks
            .insert(path.to_path_buf(), Arc::new(Peaks::build(&s.left)));
        self.samples.insert(path.to_path_buf(), s.clone());
        Ok(s)
    }

    /// The waveform summary of a loaded file, with its sample rate.
    pub fn peaks(&self, path: &Path) -> Option<(Arc<Peaks>, f64)> {
        let sample_rate = self.samples.get(path)?.sample_rate;
        Some((self.peaks.get(path)?.clone(), sample_rate))
    }
}

/// Builds engine nodes from project data.
pub struct Builder<'a> {
    pub sample_rate: f64,
    pub max_block: usize,
    pub plugins: &'a PluginCache,
    pub samples: &'a mut SampleCache,
}

pub fn descriptor_for(
    kind: &DeviceKind,
    name: &str,
    plugins: &PluginCache,
) -> Option<PluginDescriptor> {
    let DeviceKind::Plugin {
        format,
        plugin_id,
        path,
        ..
    } = kind
    else {
        return None;
    };
    Some(
        plugins
            .plugins
            .iter()
            .find(|p| p.format == *format && p.id == *plugin_id)
            .cloned()
            .unwrap_or_else(|| PluginDescriptor {
                format: *format,
                id: plugin_id.clone(),
                name: name.to_string(),
                vendor: String::new(),
                version: String::new(),
                path: path.clone(),
                is_instrument: false,
            }),
    )
}

impl Builder<'_> {
    /// Instantiate a device. Never fails: broken devices become silent placeholders with an error.
    pub fn device(&mut self, dev: &Device) -> (Box<DeviceSlot>, DeviceRuntime) {
        let (processor, runtime) = match &dev.kind {
            DeviceKind::Builtin { type_id, sample } => match rlive_devices::find(type_id) {
                Some(info) => {
                    let mut error = None;
                    let sample = sample.as_ref().and_then(|p| match self.samples.get(p) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            error = Some(e);
                            None
                        }
                    });
                    let mut p = (info.create)(&DeviceInit { sample });
                    p.activate(self.sample_rate, self.max_block);
                    let params = (info.params)();
                    for param in &params {
                        let v = dev.params.get(&param.id).copied().unwrap_or(param.default);
                        p.set_param(param.id, v);
                    }
                    (
                        p,
                        DeviceRuntime {
                            params,
                            plugin: None,
                            error,
                            role: info.role,
                        },
                    )
                }
                None => placeholder(format!("unknown device {type_id}")),
            },
            DeviceKind::Plugin { state, .. } => {
                let desc = descriptor_for(&dev.kind, &dev.name, self.plugins).expect("plugin kind");
                match rlive_plugin_host::instantiate(&desc, self.sample_rate, self.max_block) {
                    Ok(mut inst) => {
                        if let Some(state) = state {
                            if !inst.handle.load_state(state) {
                                log::warn!("{}: could not restore plugin state", dev.name);
                            }
                        }
                        let role = if desc.is_instrument {
                            DeviceRole::Instrument
                        } else {
                            DeviceRole::AudioEffect
                        };
                        (
                            inst.processor,
                            DeviceRuntime {
                                params: inst.handle.params(),
                                plugin: Some(inst.handle),
                                error: None,
                                role,
                            },
                        )
                    }
                    Err(e) => placeholder(e.to_string()),
                }
            }
        };
        let mut slot = DeviceSlot::new(dev.id, processor);
        slot.bypass = dev.bypass;
        (Box::new(slot), runtime)
    }

    pub fn clip(&mut self, clip: &Clip) -> Result<Arc<ClipData>, String> {
        Ok(Arc::new(match &clip.content {
            ClipContent::Midi { notes } => {
                let notes: Vec<Note> = notes
                    .iter()
                    .map(|n| Note {
                        start: n.start,
                        length: n.length,
                        key: n.key,
                        velocity: n.velocity,
                    })
                    .collect();
                ClipData::Midi(MidiClip::new(clip.length, notes, clip.looped))
            }
            ClipContent::Audio { path, gain_db } => {
                let sample = self.samples.get(path)?;
                ClipData::Audio(AudioClip {
                    sample,
                    length: clip.length,
                    looped: clip.looped,
                    gain: rlive_core::db_to_gain(*gain_db),
                })
            }
        }))
    }

    /// Compiles a timeline. Clips that fail to load are left out and reported.
    pub fn lane(
        &mut self,
        clips: &[ArrangementClip],
        errors: &mut Vec<String>,
    ) -> Arc<ArrangementLane> {
        let mut lane = Vec::with_capacity(clips.len());
        for c in clips {
            match self.clip(&c.clip) {
                Ok(data) => lane.push(LaneClip {
                    start: c.start,
                    end: c.end(),
                    offset: c.offset,
                    data,
                }),
                Err(e) => errors.push(e),
            }
        }
        Arc::new(ArrangementLane { clips: lane })
    }

    pub fn track(
        &mut self,
        track: &Track,
        shared: Arc<TrackShared>,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
        errors: &mut Vec<String>,
    ) -> Box<TrackNode> {
        let mut node = TrackNode::new(track.id, self.max_block).with_shared(shared);
        node.mix = mix_of(track);
        for d in &track.devices {
            let (slot, rt) = self.device(d);
            if let Some(e) = &rt.error {
                errors.push(format!("{}: {e}", d.name));
            }
            node.devices.push(slot);
            runtimes.insert(d.id, rt);
        }
        for (i, c) in track.clips.iter().enumerate() {
            if let Some(c) = c {
                match self.clip(c) {
                    Ok(data) => node.set_slot(i, Some(data)),
                    Err(e) => errors.push(e),
                }
            }
        }
        node.set_lane(self.lane(&track.arrangement, errors));
        Box::new(node)
    }

    pub fn master(
        &mut self,
        project: &Project,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
        errors: &mut Vec<String>,
    ) -> Box<MasterNode> {
        let mut node = MasterNode::new(self.max_block);
        node.volume_db = project.master.volume_db;
        for d in &project.master.devices {
            let (slot, rt) = self.device(d);
            if let Some(e) = &rt.error {
                errors.push(format!("{}: {e}", d.name));
            }
            node.devices.push(slot);
            runtimes.insert(d.id, rt);
        }
        Box::new(node)
    }
}

fn placeholder(error: String) -> (Box<dyn Processor>, DeviceRuntime) {
    log::warn!("{error}");
    (
        Box::new(NullProcessor),
        DeviceRuntime {
            params: Vec::new(),
            plugin: None,
            error: Some(error),
            role: DeviceRole::AudioEffect,
        },
    )
}

pub fn mix_of(t: &Track) -> MixSettings {
    MixSettings {
        volume_db: t.volume_db,
        pan: t.pan,
        mute: t.mute,
        solo: t.solo,
    }
}
