//! Builds engine nodes from project data.

use super::automation::routes_for;
use super::ChainRef;
use rlive_audio_io::{SampleCache, SampleStatus};
use rlive_core::envelope::segment_value;
use rlive_core::param::{chain as chain_controls, rack as rack_params};
use rlive_core::time::{BarMap, TempoMap, TempoPoint, TimeMaps};
use rlive_core::warp::{self, WarpMap};
use rlive_core::{ChainId, DeviceId, ParamInfo, Ports, Processor, ScaleInfo, Section, ViewChannel};
use rlive_devices::devices::b1::common::SCALES;
use rlive_devices::{DeviceInit, DeviceRole};
use rlive_engine::shared::{ChainShared, TrackShared};
use rlive_engine::{
    mix_cell, ArrangementLane, AudioClip, BusLayout, ChainNode, ClipData, DeviceSlot, ExprPoint,
    LaneClip, MidiClip, MixSettings, Note, NoteExpression, NullProcessor,
    RackKind as EngineRackKind, RackNode, TrackNode, WarpedClip, MAX_RACK_DEPTH, MAX_SENDS,
};
use rlive_plugin_host::scanner::PluginCache;
use rlive_plugin_host::{PluginDescriptor, PluginHandle};
use rlive_project::{
    ArrangementClip, BeatsPreserve, Breakpoint, Chain, Clip, ClipContent, ClipScale, Device,
    DeviceKind, DrumPad as ProjectPad, Project, RackKind, Track, TrackKind, Warp,
    WarpMode as ProjectWarpMode, Zone as ProjectZone, Zones as ProjectZones,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

/// A clip's scale as devices see it, or `None` when it's off or isn't a scale rLive knows.
fn clip_scale(scale: &ClipScale) -> Option<ScaleInfo> {
    if !scale.enabled {
        return None;
    }
    let (_, mask) = SCALES.iter().find(|(name, _)| *name == scale.name)?;
    Some(ScaleInfo {
        root: scale.root % 12,
        mask: *mask,
    })
}

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
    /// The sections its panel is drawn from. Empty lays one out from `params`.
    pub layout: Vec<Section>,
    /// Where it publishes data for its panel, if what its layout shows reads any.
    pub view: Option<Arc<ViewChannel>>,
    /// It takes a sidechain input.
    pub sidechain_input: bool,
    /// Names of its extra audio outputs.
    pub aux_names: Vec<String>,
}

impl DeviceRuntime {
    pub fn format_value(&self, p: &ParamInfo, value: f64) -> String {
        self.plugin
            .as_ref()
            .and_then(|h| h.format_value(p.id, value))
            .unwrap_or_else(|| p.format(value))
    }
}

/// How many linear ramps a curved segment of the tempo envelope is drawn with.
const CURVED_TEMPO_STEPS: usize = 32;

/// The song's tempo and meter for the engine.
pub fn time_maps(project: &Project) -> TimeMaps {
    let tempo = &project.arrangement.tempo.points;
    TimeMaps {
        tempo: if tempo.is_empty() {
            TempoMap::constant(project.tempo)
        } else {
            TempoMap::new(tempo_points(tempo))
        },
        bars: BarMap::new(
            project.time_signature,
            project
                .arrangement
                .signatures
                .iter()
                .map(|m| (m.tick, m.signature)),
        ),
    }
}

/// A tempo map only ramps linearly, so a curved segment becomes a run of short ones.
fn tempo_points(points: &[Breakpoint]) -> Vec<TempoPoint> {
    let mut out = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        out.push(TempoPoint::new(p.time, p.value));
        let Some(next) = points.get(i + 1).filter(|_| p.curve != 0.0) else {
            continue;
        };
        for step in 1..CURVED_TEMPO_STEPS {
            let x = step as f64 / CURVED_TEMPO_STEPS as f64;
            let bpm = segment_value(p.value, next.value, x, p.curve);
            out.push(TempoPoint::new(p.time + (next.time - p.time) * x, bpm));
        }
    }
    out
}

fn warp_map(warp: &Warp, sample_rate: f64) -> Option<WarpMap> {
    WarpMap::new(
        warp.markers
            .iter()
            .map(|m| warp::WarpMarker::new(m.seconds * sample_rate, m.tick)),
    )
}

/// Where Beats mode slices a clip, in source frames: the transients, or the grid it asks for. A
/// sample whose transients aren't known yet is sliced on sixteenths, close enough to play until
/// the analysis finishes.
fn beats_slices(
    warp: &Warp,
    map: Option<&WarpMap>,
    sample_rate: f64,
    length: u64,
    found: Option<&[f64]>,
) -> Arc<[f64]> {
    if warp.mode != ProjectWarpMode::Beats {
        return Arc::from(Vec::new());
    }
    let transients: Vec<f64> = found
        .map(|f| rlive_project::warp_edit::resolve_transients(f, &warp.transients))
        .unwrap_or_default();
    let grid = match (warp.params.preserve, map) {
        (BeatsPreserve::Transients, _) if !transients.is_empty() => {
            return transients.iter().map(|s| s * sample_rate).collect();
        }
        (BeatsPreserve::Grid(ticks), _) if ticks > 0 => ticks as u64,
        _ => rlive_core::PPQ / 4,
    };
    let Some(map) = map else {
        return Arc::from(Vec::new());
    };
    (0..)
        .map(|i| i * grid)
        .take_while(|t| *t < length)
        .map(|t| map.tick_to_src(t as f64))
        .collect()
}

fn warp_mode(warp: &Warp) -> warp::WarpMode {
    let p = &warp.params;
    match warp.mode {
        ProjectWarpMode::Beats => warp::WarpMode::Beats {
            preserve: p.preserve,
            beats_loop: p.beats_loop,
            transient_envelope: p.transient_envelope,
        },
        ProjectWarpMode::Tones => warp::WarpMode::Tones {
            grain_size: p.grain_size,
        },
        ProjectWarpMode::Texture => warp::WarpMode::Texture {
            grain_size: p.grain_size,
            flux: p.flux,
        },
        ProjectWarpMode::RePitch => warp::WarpMode::RePitch,
        ProjectWarpMode::Complex => warp::WarpMode::Complex,
        ProjectWarpMode::ComplexPro => warp::WarpMode::ComplexPro {
            formants: p.formants,
            envelope: p.envelope,
        },
    }
}

/// Builds engine nodes from project data.
pub struct Builder<'a> {
    pub sample_rate: f64,
    pub max_block: usize,
    pub plugins: &'a PluginCache,
    pub samples: &'a mut SampleCache,
    pub wiring: &'a PluginWiring,
    /// Where chains publish what the UI reads, if anyone is reading.
    pub chain_shared: Option<&'a mut HashMap<ChainId, Arc<ChainShared>>>,
}

/// Which plug-ins have something routed to or tapped from their extra buses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginWiring {
    pub sidechain: HashSet<DeviceId>,
    pub aux: HashSet<DeviceId>,
}

impl PluginWiring {
    pub fn of(plan: &rlive_engine::RenderPlan) -> Self {
        Self {
            sidechain: plan.sidechains.iter().map(|s| s.device).collect(),
            aux: plan
                .tap_outs
                .iter()
                .filter_map(|t| match t.tap {
                    rlive_engine::Tap::Aux(d, _) => Some(d),
                    _ => None,
                })
                .collect(),
        }
    }
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
        let mut view = None;
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
                            match self.samples.request(&file.path) {
                                SampleStatus::Ready(s) => Some(s),
                                // The sample is handed over once it has finished loading.
                                SampleStatus::Loading => None,
                                SampleStatus::Failed(e) => {
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
                    let sidechain_input = desc.ports.sidechain;
                    let aux_names = desc.ports.aux_out.iter().map(|n| n.to_string()).collect();
                    ports = desc.ports;
                    // Only for what its layout shows: a device nobody can see measures nothing.
                    let view_len = crate::ui::panels::view_len(&desc.layout);
                    view = (view_len > 0).then(|| Arc::new(ViewChannel::new(view_len)));
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
                            layout: desc.layout,
                            view: view.clone(),
                            sidechain_input,
                            aux_names,
                        },
                    )
                }
                None => placeholder(format!("unknown device {type_id}")),
            },
            DeviceKind::Rack { .. } => unreachable!("handled above"),
            DeviceKind::Plugin { state, .. } => {
                let desc = descriptor_for(&dev.kind, &dev.name, self.plugins).expect("plugin kind");
                let buses = rlive_plugin_host::PluginBuses {
                    sidechain: self.wiring.sidechain.contains(&dev.id),
                    aux: self.wiring.aux.contains(&dev.id),
                };
                match rlive_plugin_host::instantiate_with(
                    &desc,
                    self.sample_rate,
                    self.max_block,
                    buses,
                ) {
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
                        let sidechain_input = inst.handle.sidechain_input();
                        let aux_names = inst.handle.aux_outputs();
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
                                layout: Vec::new(),
                                view: None,
                                sidechain_input,
                                aux_names,
                            },
                        )
                    }
                    Err(e) => placeholder(e.to_string()),
                }
            }
        };
        let aux = runtime.aux_names.len();
        let mut slot = DeviceSlot::new(dev.id, processor).with_aux_outputs(aux, self.max_block);
        slot.bypass = dev.bypass;
        slot.midi_effect = ports.midi_out;
        slot.latency = runtime.latency.clone();
        slot.mod_out = vec![0.0; ports.mod_out.len()].into_boxed_slice();
        slot.ports = ports;
        slot.view = view;
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
            select_filters_ctrl,
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
        node.select_filters_ctrl = *select_filters_ctrl;
        node.seed_selector(dev.stored_param(rack_params::CHAIN_SELECT.0).unwrap_or(0.0) as f32);
        for c in chains {
            let mut chain = self.chain(c, depth + 1, runtimes);
            chain.seed_mix(engine_mix(c, returns));
            node.chains.push(chain);
        }
        for c in returns {
            let mut chain = self.chain(c, depth + 1, runtimes);
            chain.seed_mix(engine_mix(c, returns));
            node.returns.push(chain);
        }
        node.seed_gates();
        let mut slot = DeviceSlot::rack(dev.id, node);
        slot.bypass = dev.bypass;
        let latency = Arc::new(AtomicU32::new(0));
        slot.latency = latency.clone();
        runtimes.insert(
            dev.id,
            DeviceRuntime {
                params: rack_params::infos(),
                plugin: None,
                error: None,
                role,
                latency,
                sample_slots: 0,
                unmoved: HashMap::new(),
                layout: Vec::new(),
                view: None,
                sidechain_input: false,
                aux_names: Vec::new(),
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
        node.zones = engine_zones(&chain.zones);
        if let Some(shared) = self.chain_shared.as_deref_mut() {
            node.set_shared(shared.entry(chain.id).or_default().clone());
        }
        for d in &chain.devices {
            let slot = self.device(d, depth, runtimes);
            node.devices.push(slot);
        }
        node
    }

    /// Compiles a clip. `None` means its sample hasn't finished loading or won't: the clip stays
    /// out of the engine, so it can't be played, and the failure has already been reported.
    pub fn clip(&mut self, clip: &Clip) -> Option<Arc<ClipData>> {
        Some(Arc::new(match &clip.content {
            ClipContent::Midi { notes, .. } => {
                // Deactivated notes never reach the engine.
                let notes: Vec<Note> = notes
                    .iter()
                    .filter(|n| !n.muted)
                    .map(|n| Note {
                        start: n.start,
                        length: n.length,
                        key: n.key,
                        velocity: n.velocity,
                        release: n.release,
                        expr: n.expr.as_deref().map(engine_expression).map(Box::new),
                    })
                    .collect();
                ClipData::Midi(
                    MidiClip::new(clip.length, notes, clip.looped)
                        .with_scale(clip_scale(&clip.scale)),
                )
            }
            ClipContent::Audio {
                file,
                gain_db,
                transpose,
                detune_cents,
                warp,
                ..
            } => {
                let SampleStatus::Ready(sample) = self.samples.request(&file.path) else {
                    return None;
                };
                let sample_rate = sample.sample_rate;
                let audio = AudioClip {
                    sample,
                    length: clip.length,
                    looped: clip.looped,
                    gain: rlive_core::db_to_gain(*gain_db),
                    scale: clip_scale(&clip.scale),
                };
                if warp.enabled {
                    let map = warp_map(warp, sample_rate);
                    let found = self.samples.transients(&file.path);
                    ClipData::Warped(WarpedClip {
                        slices: beats_slices(
                            warp,
                            map.as_ref(),
                            sample_rate,
                            clip.length,
                            found.as_ref().map(|f| f.as_slice()),
                        ),
                        audio,
                        map,
                        mode: warp_mode(warp),
                        transpose: *transpose as f32 + *detune_cents / 100.0,
                    })
                } else {
                    ClipData::Audio(audio)
                }
            }
        }))
    }

    /// Compiles a timeline. Clips whose sample isn't there are left out.
    pub fn lane(&mut self, clips: &[ArrangementClip]) -> Arc<ArrangementLane> {
        let mut lane = Vec::with_capacity(clips.len());
        for c in clips {
            if let Some(data) = self.clip(&c.clip) {
                lane.push(LaneClip {
                    start: c.start,
                    end: c.end(),
                    offset: c.offset,
                    data,
                });
            }
        }
        Arc::new(ArrangementLane { clips: lane })
    }

    /// `returns` are the project's return tracks, in order: the sends' positions.
    #[allow(clippy::too_many_arguments)]
    pub fn track(
        &mut self,
        track: &Track,
        returns: &[rlive_core::TrackId],
        shared: Arc<TrackShared>,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
        layouts: &mut HashMap<ChainRef, BusLayout>,
        errors: &mut Vec<String>,
    ) -> Box<TrackNode> {
        let mut node = TrackNode::new(track.id, self.max_block).with_shared(shared);
        if track.kind == TrackKind::Return {
            node = node.with_feedback(self.max_block);
        }
        node.mix = mix_of(track);
        let mut layout = BusLayout::for_track(&node.mix).with_max_block(self.max_block);
        for (n, ret) in returns.iter().take(MAX_SENDS).enumerate() {
            let db = track.send_level(*ret);
            layout.note_value(mix_cell::send(n), db as f64);
            node.seed_send(n, db);
        }
        for d in &track.devices {
            let slot = self.device(d, 0, runtimes);
            note_errors(d, runtimes, errors);
            node.devices.push(slot);
            place_device_tree(&mut layout, d, runtimes);
        }
        node.bus = layout.build();
        node.routes = routes_for(
            &track.automation,
            &track.devices,
            ChainRef::Track(track.id),
            &layout,
        );
        layouts.insert(ChainRef::Track(track.id), layout);
        for (i, c) in track.clips.iter().enumerate() {
            if let Some(data) = c.as_ref().and_then(|c| self.clip(c)) {
                node.set_slot(i, Some(data));
            }
        }
        node.set_lane(self.lane(&track.arrangement));
        Box::new(node)
    }

    pub fn master(
        &mut self,
        project: &Project,
        runtimes: &mut HashMap<DeviceId, DeviceRuntime>,
        layouts: &mut HashMap<ChainRef, BusLayout>,
        errors: &mut Vec<String>,
    ) -> Box<TrackNode> {
        let mut node = TrackNode::master(self.max_block);
        node.id = project.master.id;
        node.mix.volume_db = project.master.volume_db;
        node.mix.crossfader = project.crossfader.position;
        node.mix.cue_db = project.cue.volume_db;
        let mut layout = BusLayout::for_master(node.mix.volume_db).with_max_block(self.max_block);
        layout.note_value(mix_cell::CROSSFADER, project.crossfader.position as f64);
        layout.note_value(mix_cell::CUE_VOLUME, project.cue.volume_db as f64);
        for d in &project.master.devices {
            let slot = self.device(d, 0, runtimes);
            note_errors(d, runtimes, errors);
            node.devices.push(slot);
            place_device_tree(&mut layout, d, runtimes);
        }
        node.bus = layout.build();
        node.routes = routes_for(
            &project.master.automation,
            &project.master.devices,
            ChainRef::Master,
            &layout,
        );
        layouts.insert(ChainRef::Master, layout);
        Box::new(node)
    }
}

/// Give a device cells in its chain's layout, seeded from the project or the running plugin.
/// Returns whether the engine needs a bus built from the layout.
fn place_device(layout: &mut BusLayout, dev: &Device, rt: &DeviceRuntime) -> bool {
    layout.place_device(dev.id, !dev.bypass, &rt.params, |p| {
        dev.stored_param(p.id)
            .or_else(|| rt.plugin.as_ref().and_then(|h| h.param_value(p.id)))
            .unwrap_or(p.default)
    })
}

/// The engine's copy of a chain's zones.
pub(super) fn engine_zones(z: &ProjectZones) -> rlive_engine::Zones {
    fn zone(z: &ProjectZone) -> rlive_engine::Zone {
        rlive_engine::Zone {
            low: z.low,
            high: z.high,
            fade_low: z.fade_low,
            fade_high: z.fade_high,
        }
    }
    rlive_engine::Zones {
        key: zone(&z.key),
        velocity: zone(&z.velocity),
        select: zone(&z.select),
        drum: z.drum.map(|p: ProjectPad| rlive_engine::DrumPad {
            receive: p.receive,
            play: p.play,
            choke: p.choke,
        }),
    }
}

/// The level of a chain's send to each return chain, by position, in dB.
fn send_levels(c: &Chain, returns: &[Chain]) -> [f32; chain_controls::MAX_RETURNS] {
    let mut levels = [rlive_engine::SEND_MIN_DB as f32; chain_controls::MAX_RETURNS];
    for (i, ret) in returns.iter().take(chain_controls::MAX_RETURNS).enumerate() {
        if let Some(send) = c.mix.sends.iter().find(|s| s.to == ret.id) {
            levels[i] = send.level_db;
        }
    }
    levels
}

/// A chain's strip for the engine: volume, pan, switches, and its sends by position.
pub(super) fn engine_mix(c: &Chain, returns: &[Chain]) -> rlive_engine::ChainMix {
    rlive_engine::ChainMix {
        volume_db: c.mix.volume_db,
        pan: c.mix.pan,
        active: c.mix.active,
        solo: c.mix.solo,
        sends: send_levels(c, returns),
        output: c.mix.output,
    }
}

/// A chain's strip in the cell order of its [`chain_controls`].
pub(super) fn chain_values(c: &Chain, returns: &[Chain]) -> [f64; chain_controls::CONTROLS] {
    let mut values = [0.0; chain_controls::CONTROLS];
    values[0] = c.mix.volume_db as f64;
    values[1] = c.mix.pan as f64;
    values[2] = c.mix.active as u8 as f64;
    values[3] = c.mix.solo as u8 as f64;
    for (i, level) in send_levels(c, returns).iter().enumerate() {
        values[4 + i] = *level as f64;
    }
    values
}

/// The cells of a rack's chains and return chains. Returns whether any were new.
pub(super) fn place_chains(layout: &mut BusLayout, rack: &Device) -> bool {
    let DeviceKind::Rack {
        chains, returns, ..
    } = &rack.kind
    else {
        return false;
    };
    let mut needs_bus = false;
    for c in chains.iter().chain(returns) {
        needs_bus |= layout.place_chain(c.id, &chain_values(c, returns));
    }
    needs_bus
}

/// Every chain under a device, the device's own and any nested deeper.
pub(super) fn chains_of(dev: &Device) -> Vec<ChainId> {
    let mut chains = Vec::new();
    dev.for_each(&mut |d| chains.extend(d.chains().map(|c| c.id)));
    chains
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
        needs_bus |= place_chains(layout, d);
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
            layout: Vec::new(),
            view: None,
            sidechain_input: false,
            aux_names: Vec::new(),
        },
    )
}

pub fn mix_of(t: &Track) -> MixSettings {
    MixSettings {
        volume_db: t.volume_db,
        pan: t.pan,
        mute: t.mute,
        solo: t.solo,
        crossfade: match t.crossfade {
            rlive_project::Crossfade::None => 0,
            rlive_project::Crossfade::A => 1,
            rlive_project::Crossfade::B => 2,
        },
        crossfader: 0.0,
        cue_db: 0.0,
    }
}

fn engine_expression(expr: &rlive_project::NoteExpression) -> NoteExpression {
    let points = |list: &[rlive_project::Breakpoint]| {
        list.iter()
            .map(|p| ExprPoint {
                time: p.time,
                value: p.value as f32,
            })
            .collect()
    };
    NoteExpression {
        pitch: points(&expr.pitch),
        slide: points(&expr.slide),
        pressure: points(&expr.pressure),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_clip_scale_reaches_devices_only_when_on_and_known() {
        let scale = |enabled, name: &str| ClipScale {
            enabled,
            root: 14,
            name: name.into(),
        };
        let on = super::clip_scale(&scale(true, "Major")).unwrap();
        assert_eq!(on.root, 2, "wrapped to a pitch class");
        assert_eq!(on.mask, SCALES[0].1);
        assert_eq!(super::clip_scale(&scale(false, "Major")), None);
        assert_eq!(super::clip_scale(&scale(true, "Nonsense")), None);
    }

    use super::*;
    use rlive_core::warp::{BeatsLoop, WarpMode as CoreWarpMode};
    use rlive_core::{TimeSignature, PPQ};
    use rlive_project::{SignatureMarker, WarpMarker as ProjectMarker, WarpParams};
    use std::path::Path;

    fn sig(numerator: u8, denominator: u8) -> TimeSignature {
        TimeSignature {
            numerator,
            denominator,
        }
    }

    #[test]
    fn a_plain_project_has_one_tempo_and_signature() {
        let mut p = Project::empty();
        p.tempo = 133.0;
        p.time_signature = sig(7, 8);
        let maps = time_maps(&p);
        assert!(maps.tempo.is_constant());
        assert_eq!(maps.tempo.bpm_at(9.0), 133.0);
        assert_eq!(maps.bars, BarMap::constant(sig(7, 8)));
    }

    #[test]
    fn the_project_timeline_converts_to_maps() {
        let mut p = Project::empty();
        p.tempo = 133.0;
        p.time_signature = sig(4, 4);
        p.arrangement.tempo.points = vec![
            Breakpoint::new(0.0, 100.0),
            Breakpoint::new(1920.0, 200.0),
            Breakpoint::new(1920.0, 60.0),
        ];
        p.arrangement.signatures = vec![SignatureMarker {
            tick: 2 * 3840,
            signature: sig(3, 4),
        }];
        let maps = time_maps(&p);
        assert!(!maps.tempo.is_constant());
        assert_eq!(maps.tempo.bpm_at(1919.999).round(), 200.0);
        assert_eq!(maps.tempo.bpm_at(1920.0), 60.0);
        assert_eq!(maps.bars.bar_at(2.0 * 3840.0).index, 2);
        assert_eq!(maps.bars.signature_at(2.0 * 3840.0), sig(3, 4));
    }

    #[test]
    fn a_curved_tempo_segment_follows_the_shared_curve() {
        let mut p = Project::empty();
        let mut start = Breakpoint::new(0.0, 100.0);
        start.curve = 1.0;
        p.arrangement.tempo.points = vec![start, Breakpoint::new(1920.0, 200.0)];
        let maps = time_maps(&p);
        let want = segment_value(100.0, 200.0, 0.5, 1.0);
        assert!((maps.tempo.bpm_at(960.0) - want).abs() < 0.5);
        assert_eq!(maps.tempo.bpm_at(1920.0), 200.0);
    }

    #[test]
    fn warp_settings_convert_per_mode() {
        let warp = Warp {
            enabled: true,
            mode: ProjectWarpMode::ComplexPro,
            markers: vec![
                ProjectMarker {
                    seconds: 0.0,
                    tick: 0.0,
                },
                ProjectMarker {
                    seconds: 2.0,
                    tick: 1920.0,
                },
            ],
            params: WarpParams {
                formants: 12.0,
                envelope: 64.0,
                preserve: BeatsPreserve::Grid(240),
                beats_loop: BeatsLoop::Forward,
                transient_envelope: 50.0,
                grain_size: 40.0,
                flux: 7.0,
            },
            ..Warp::default()
        };
        assert_eq!(
            warp_mode(&warp),
            CoreWarpMode::ComplexPro {
                formants: 12.0,
                envelope: 64.0,
            }
        );
        let beats = Warp {
            mode: ProjectWarpMode::Beats,
            ..warp.clone()
        };
        assert_eq!(
            warp_mode(&beats),
            CoreWarpMode::Beats {
                preserve: BeatsPreserve::Grid(240),
                beats_loop: BeatsLoop::Forward,
                transient_envelope: 50.0,
            }
        );
        let map = warp_map(&warp, 44_100.0).unwrap();
        assert_eq!(map.tick_to_src(1920.0), 88_200.0);
        let one = Warp {
            markers: warp.markers[..1].to_vec(),
            ..warp
        };
        assert!(warp_map(&one, 44_100.0).is_none());
    }

    #[test]
    fn a_warped_clip_builds_warped_clip_data() {
        let dir = std::env::temp_dir().join(format!("rlive-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path: &Path = &dir.join("loop.wav");
        let frames = vec![0.5f32; 4800];
        rlive_devices::write_wav(path, &frames, &frames, 48_000).unwrap();

        let mut samples = SampleCache::default();
        samples.wait_for([path]);
        let plugins = PluginCache::default();
        let mut b = Builder {
            sample_rate: 48_000.0,
            max_block: 256,
            plugins: &plugins,
            samples: &mut samples,
            wiring: &PluginWiring::default(),
            chain_shared: None,
        };
        let mut clip = Clip {
            length: 4 * PPQ,
            content: ClipContent::audio(path, -3.0),
            ..Clip::default()
        };
        // Warp off, the common case, stays the plain variant.
        assert!(matches!(b.clip(&clip).as_deref(), Some(ClipData::Audio(_))));
        if let ClipContent::Audio { warp, .. } = &mut clip.content {
            warp.enabled = true;
            warp.mode = ProjectWarpMode::Complex;
        }
        let built = b.clip(&clip).expect("loaded above");
        std::fs::remove_dir_all(&dir).ok();
        let ClipData::Warped(w) = &*built else {
            panic!("expected warped clip data");
        };
        assert_eq!(w.mode, CoreWarpMode::Complex);
        assert!(w.map.is_none(), "no markers yet");
        assert_eq!(built.length(), 4 * PPQ);
    }
}
