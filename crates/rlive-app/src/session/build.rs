//! Builds engine nodes from project data.

use super::ChainRef;
use crate::peaks::Peaks;
use rlive_core::{DeviceId, ParamInfo, Ports, Processor};
use rlive_devices::{DeviceInit, DeviceRole, SampleBuffer};
use rlive_engine::shared::TrackShared;
use rlive_engine::{
    ArrangementLane, AudioClip, BusLayout, ChainNode, ClipData, DeviceSlot, LaneClip, MasterNode,
    MidiClip, MixSettings, Note, NullProcessor, RackKind as EngineRackKind, RackNode, TrackNode,
    MAX_RACK_DEPTH,
};
use rlive_plugin_host::scanner::PluginCache;
use rlive_plugin_host::{PluginDescriptor, PluginHandle};
use rlive_project::{
    ArrangementClip, Chain, Clip, ClipContent, Device, DeviceKind, Project, RackKind, Track,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

/// UI-side state of one device in a chain.
pub struct DeviceRuntime {
    pub params: Vec<ParamInfo>,
    pub plugin: Option<Box<dyn PluginHandle>>,
    pub error: Option<String>,
    pub role: DeviceRole,
    /// The device's latency in frames, shared with the engine.
    pub latency: Arc<AtomicU32>,
    /// How many samples it can hold; any shows the sample row in the device view.
    pub sample_slots: usize,
    /// Where parameters that the project has no value for started out, so undo can put them
    /// back. A plugin's values come from its own state, not its defaults.
    pub unmoved: HashMap<u32, f64>,
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
    /// Instantiate a device and everything nested in it, recording their runtimes. `depth` is how
    /// many racks it is inside. Never fails: broken devices become silent placeholders with an
    /// error.
    pub fn device(
        &mut self,
        dev: &Device,
        depth: usize,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
    ) -> Box<DeviceSlot> {
        if let DeviceKind::Rack { .. } = &dev.kind {
            return self.rack(dev, depth, runtimes);
        }
        let mut ports = Ports::default();
        let (processor, runtime) = match &dev.kind {
            DeviceKind::Builtin {
                type_id,
                samples,
                state,
            } => match rlive_devices::find(type_id) {
                Some(info) => {
                    let mut error = None;
                    let desc = (info.desc)();
                    let sample_slots = desc.samples.len();
                    let loaded = (0..sample_slots)
                        .map(|slot| {
                            let file = samples.get(slot).and_then(Option::as_ref)?;
                            match self.samples.get(&file.path) {
                                Ok(s) => Some(s),
                                Err(e) => {
                                    error = Some(e);
                                    None
                                }
                            }
                        })
                        .collect();
                    let mut p = (info.create)(&DeviceInit {
                        samples: loaded,
                        state: state.clone(),
                    });
                    p.activate(self.sample_rate, self.max_block);
                    for param in &desc.params {
                        let v = dev.params.get(&param.id).copied().unwrap_or(param.default);
                        p.set_param(param.id, v);
                    }
                    let latency = Arc::new(AtomicU32::new(p.latency()));
                    ports = desc.ports;
                    (
                        p,
                        DeviceRuntime {
                            params: desc.params,
                            plugin: None,
                            error,
                            role: info.role,
                            latency,
                            sample_slots,
                            unmoved: HashMap::new(),
                        },
                    )
                }
                None => placeholder(format!("unknown device {type_id}")),
            },
            DeviceKind::Rack { .. } => unreachable!("handled above"),
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
                        let latency = Arc::new(AtomicU32::new(inst.handle.latency()));
                        (
                            inst.processor,
                            DeviceRuntime {
                                params: inst.handle.params(),
                                plugin: Some(inst.handle),
                                error: None,
                                role,
                                latency,
                                sample_slots: 0,
                                unmoved: HashMap::new(),
                            },
                        )
                    }
                    Err(e) => placeholder(e.to_string()),
                }
            }
        };
        let mut slot = DeviceSlot::new(dev.id, processor);
        slot.bypass = dev.bypass;
        slot.midi_effect = ports.midi_out;
        slot.latency = runtime.latency.clone();
        slot.mod_out = vec![0.0; ports.mod_out.len()].into_boxed_slice();
        slot.ports = ports;
        runtimes.insert(dev.id, runtime);
        Box::new(slot)
    }

    /// A rack with everything in it. Beyond the engine's nesting limit it is a silent stand-in.
    fn rack(
        &mut self,
        dev: &Device,
        depth: usize,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
    ) -> Box<DeviceSlot> {
        if depth >= MAX_RACK_DEPTH {
            let (processor, runtime) = placeholder("racks are nested too deeply".to_string());
            let mut slot = DeviceSlot::new(dev.id, processor);
            slot.bypass = dev.bypass;
            slot.latency = runtime.latency.clone();
            runtimes.insert(dev.id, runtime);
            return Box::new(slot);
        }
        let DeviceKind::Rack {
            kind,
            chains,
            returns,
            ..
        } = &dev.kind
        else {
            unreachable!("only called for racks");
        };
        let (kind, role) = match kind {
            RackKind::Instrument => (EngineRackKind::Instrument, DeviceRole::Instrument),
            RackKind::Audio => (EngineRackKind::Audio, DeviceRole::AudioEffect),
            RackKind::Midi => (EngineRackKind::Midi, DeviceRole::MidiEffect),
            RackKind::Drum => (EngineRackKind::Drum, DeviceRole::Instrument),
        };
        let mut node = Box::new(RackNode::new(kind));
        for c in chains {
            let chain = self.chain(c, depth + 1, runtimes);
            node.chains.push(chain);
        }
        for c in returns {
            let chain = self.chain(c, depth + 1, runtimes);
            node.returns.push(chain);
        }
        let mut slot = DeviceSlot::rack(dev.id, node);
        slot.bypass = dev.bypass;
        let latency = Arc::new(AtomicU32::new(0));
        slot.latency = latency.clone();
        runtimes.insert(
            dev.id,
            DeviceRuntime {
                params: Vec::new(),
                plugin: None,
                error: None,
                role,
                latency,
                sample_slots: 0,
                unmoved: HashMap::new(),
            },
        );
        Box::new(slot)
    }

    /// One chain of a rack with everything in it.
    pub fn chain(
        &mut self,
        chain: &Chain,
        depth: usize,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
    ) -> Box<ChainNode> {
        let mut node = Box::new(ChainNode::new(chain.id, self.max_block));
        for d in &chain.devices {
            let slot = self.device(d, depth, runtimes);
            node.devices.push(slot);
        }
        node
    }

    pub fn clip(&mut self, clip: &Clip) -> Result<Arc<ClipData>, String> {
        Ok(Arc::new(match &clip.content {
            ClipContent::Midi { notes, .. } => {
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
            ClipContent::Audio { file, gain_db, .. } => {
                let sample = self.samples.get(&file.path)?;
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
        layouts: &mut HashMap<ChainRef, BusLayout>,
        errors: &mut Vec<String>,
    ) -> Box<TrackNode> {
        let mut node = TrackNode::new(track.id, self.max_block).with_shared(shared);
        node.mix = mix_of(track);
        let mut layout = BusLayout::for_track(&node.mix).with_max_block(self.max_block);
        for d in &track.devices {
            let slot = self.device(d, 0, runtimes);
            note_errors(d, runtimes, errors);
            node.devices.push(slot);
            place_device_tree(&mut layout, d, runtimes);
        }
        node.bus = layout.build();
        layouts.insert(ChainRef::Track(track.id), layout);
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
        layouts: &mut HashMap<ChainRef, BusLayout>,
        errors: &mut Vec<String>,
    ) -> Box<MasterNode> {
        let mut node = MasterNode::new(self.max_block);
        node.volume_db = project.master.volume_db;
        let mut layout = BusLayout::for_master(node.volume_db).with_max_block(self.max_block);
        for d in &project.master.devices {
            let slot = self.device(d, 0, runtimes);
            note_errors(d, runtimes, errors);
            node.devices.push(slot);
            place_device_tree(&mut layout, d, runtimes);
        }
        node.bus = layout.build();
        layouts.insert(ChainRef::Master, layout);
        Box::new(node)
    }
}

/// Give a device cells in its chain's layout, seeded from the project or the running plugin.
/// Returns whether the engine needs a bus built from the layout.
fn place_device(layout: &mut BusLayout, dev: &Device, rt: &DeviceRuntime) -> bool {
    layout.place_device(dev.id, !dev.bypass, &rt.params, |p| {
        dev.params
            .get(&p.id)
            .copied()
            .or_else(|| rt.plugin.as_ref().and_then(|h| h.param_value(p.id)))
            .unwrap_or(p.default)
    })
}

/// [`place_device`] for a device and everything nested in it.
pub(super) fn place_device_tree(
    layout: &mut BusLayout,
    dev: &Device,
    runtimes: &HashMap<DeviceId, DeviceRuntime>,
) -> bool {
    let mut needs_bus = false;
    dev.for_each(&mut |d| {
        if let Some(rt) = runtimes.get(&d.id) {
            needs_bus |= place_device(layout, d, rt);
        }
    });
    needs_bus
}

/// Reports what went wrong building a device and everything nested in it.
pub(super) fn note_errors(
    dev: &Device,
    runtimes: &HashMap<DeviceId, DeviceRuntime>,
    errors: &mut Vec<String>,
) {
    dev.for_each(&mut |d| {
        if let Some(e) = runtimes.get(&d.id).and_then(|rt| rt.error.as_ref()) {
            errors.push(format!("{}: {e}", d.name));
        }
    });
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
            latency: Arc::new(AtomicU32::new(0)),
            sample_slots: 0,
            unmoved: HashMap::new(),
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
