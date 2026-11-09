//! MIDI and key mapping, as the session sees it: compiling the project's mappings into the table
//! the MIDI thread reads, catching the project up with what that thread did, feedback to the
//! hardware, and control surfaces.
//!
//! Every frame the mappings are compiled again and the result compared with what the ports have.
//! Anything that could change a mapping's meaning (a device added, a parameter frozen, a Macro
//! taking a parameter over) only has to change what compiling produces, so nothing can go stale.

use super::{ChainRef, Session};
use crate::midi_input::{RemoteEvent, RemoteHub};
use rlive_core::param::{mixer, ParamOwner, ParamRange, ParamRef, ACTIVATOR};
use rlive_core::{ChainId, DeviceId, TrackId};
use rlive_engine::CellIx;
use rlive_project::{
    CcEncoding, ControlKind, CrossfaderPoint, Device, DeviceKind, KeyMapping, MapTarget,
    MidiMapping, MidiSource, RemoteControl,
};
use rlive_remote::{
    Assignment, CellAddr, Compiled, Exec, FeedbackSink, FeedbackState, FeedbackValue, Gesture,
    Learner, MidiEvent, ParamShape, RemoteTable, ShapeKind, SurfaceDevice, SurfaceHost,
    SurfaceParam, TakeoverMode, UiKind, ALL_PORTS,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

/// Which kind of mapping the overlays are making.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapMode {
    Midi,
    Key,
}

/// One row of the Mapping Browser.
#[derive(Debug, Clone, PartialEq)]
pub struct MappingRow {
    pub index: usize,
    /// The control's own name: a note, a CC, a key.
    pub control: String,
    pub path: String,
    pub name: String,
    pub min: f64,
    pub max: f64,
    /// The range isn't meaningful for this target.
    pub fixed_range: bool,
    /// The encoding chooser shows, for a controller.
    pub encoding: Option<CcEncoding>,
}

/// Which parameters a Macro owns, and which chain each device and rack chain sits in.
#[derive(Default)]
struct Owners {
    devices: HashMap<DeviceId, ChainRef>,
    chains: HashMap<ChainId, ChainRef>,
    macro_mapped: HashSet<ParamRef>,
}

impl Owners {
    fn add(&mut self, devices: &[Device], owner: ChainRef) {
        for d in devices {
            self.devices.insert(d.id, owner);
            if let DeviceKind::Rack { macros, .. } = &d.kind {
                self.macro_mapped.extend(
                    macros
                        .iter()
                        .flat_map(|m| m.mappings.iter().map(|x| x.target)),
                );
            }
            for chain in d.chains() {
                self.chains.insert(chain.id, owner);
                self.add(&chain.devices, owner);
            }
        }
    }
}

/// Which ports mappings use, as the app last connected them.
#[derive(Debug, Clone, Default)]
pub struct RemotePorts {
    pub in_names: Vec<String>,
    /// Ports with Remote on: bit `n` is port `n`.
    pub in_mask: u64,
    pub out_names: Vec<String>,
    pub out_mask: u64,
}

/// The session's remote-control state.
pub struct RemoteState {
    pub hub: RemoteHub,
    pub surfaces: SurfaceHost,
    pub ports: RemotePorts,
    /// The device the selection is on, for control surfaces to follow.
    pub selected_device: Option<DeviceId>,
    map_mode: Option<MapMode>,
    selected: Option<MapTarget>,
    learner: Learner,
    last: Vec<Compiled>,
    /// The value each entry of the table was last told, normalized.
    seen: Vec<f64>,
    out: Option<Box<dyn FeedbackSink + Send>>,
    feedback: FeedbackState,
    actions: Vec<(MapTarget, Gesture)>,
    /// Values are coming from hardware through the MIDI thread: the engine has them already.
    pub(super) store_only: bool,
}

impl Default for RemoteState {
    fn default() -> Self {
        Self {
            hub: RemoteHub::default(),
            surfaces: SurfaceHost::default(),
            ports: RemotePorts {
                in_mask: ALL_PORTS,
                out_mask: ALL_PORTS,
                ..RemotePorts::default()
            },
            selected_device: None,
            map_mode: None,
            selected: None,
            learner: Learner::default(),
            last: Vec::new(),
            seen: Vec::new(),
            out: None,
            feedback: FeedbackState::default(),
            actions: Vec::new(),
            store_only: false,
        }
    }
}

/// The name of a note as Live shows it: C-2 is note 0.
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = i32::from(note) / 12 - 2;
    format!("{}{octave}", NAMES[usize::from(note) % 12])
}

/// A control's name in the Mapping Browser.
pub fn source_text(channel: u8, source: MidiSource) -> String {
    let ch = u32::from(channel) + 1;
    match source {
        MidiSource::Note(n) => format!("{ch} {}", note_name(n)),
        MidiSource::NoteRange { low, high } => {
            format!("{ch} {}-{}", note_name(low), note_name(high))
        }
        MidiSource::ChromaticRange { root, low, high } => format!(
            "{ch} {} ({}-{})",
            note_name(root),
            note_name(low),
            note_name(high)
        ),
        MidiSource::Cc { number, .. } => format!("{ch} CC {number}"),
        MidiSource::PitchBend => format!("{ch} Pitch Bend"),
    }
}

fn point_position(p: CrossfaderPoint) -> super::CrossfaderField {
    match p {
        CrossfaderPoint::Left => super::CrossfaderField::Left,
        CrossfaderPoint::Center => super::CrossfaderField::Center,
        CrossfaderPoint::Right => super::CrossfaderField::Right,
    }
}

fn is_toggle(range: ParamRange) -> bool {
    range.quantized && range.min == 0.0 && range.max == 1.0
}

impl Session {
    // ------------------------------------------------------------- map modes

    pub fn map_mode(&self) -> Option<MapMode> {
        self.remote.map_mode
    }

    pub fn map_target(&self) -> Option<MapTarget> {
        self.remote.selected
    }

    pub fn set_map_mode(&mut self, mode: Option<MapMode>) {
        self.remote.map_mode = mode;
        self.select_map_target(None);
    }

    /// Picks the control the next message or key goes to.
    pub fn select_map_target(&mut self, target: Option<MapTarget>) {
        self.remote.selected = target;
        self.remote.learner = Learner::default();
        let learning = self.remote.map_mode == Some(MapMode::Midi) && target.is_some();
        self.remote.hub.shared.learning.store(learning, Relaxed);
    }

    pub fn set_takeover(&mut self, mode: TakeoverMode) {
        self.remote.hub.shared.takeover.store(mode.to_u8(), Relaxed);
    }

    pub fn takeover(&self) -> TakeoverMode {
        TakeoverMode::from_u8(self.remote.hub.shared.takeover.load(Relaxed))
    }

    pub fn set_feedback_output(&mut self, out: Option<Box<dyn FeedbackSink + Send>>) {
        self.remote.out = out;
        self.remote.feedback.reset();
    }

    pub fn set_remote_ports(&mut self, ports: RemotePorts) {
        self.remote.ports = ports;
    }

    // ------------------------------------------------------------- the mappings themselves

    /// Learns a mapping for `target`, in place of any it already had. Its range is kept if it
    /// has one, as a mapping is refined by the messages that follow.
    pub fn set_midi_mapping(&mut self, target: MapTarget, channel: u8, source: MidiSource) {
        let old = self
            .project
            .midi_mappings
            .iter()
            .find(|m| m.target == target);
        if old.is_some_and(|m| m.channel == channel && m.source == source) {
            return;
        }
        let (min, max) = old.map_or_else(|| self.default_range(target), |m| (m.min, m.max));
        self.project.midi_mappings.retain(|m| m.target != target);
        self.project.midi_mappings.push(MidiMapping {
            channel,
            source,
            target,
            min,
            max,
        });
        self.mark_edit("MIDI Mapping");
        // A control is refined by what it sends next, and it all counts as one mapping.
        self.history.settling = super::undo::SETTLE_FRAMES;
    }

    /// Assigns a key to `target`, in place of any it already had.
    pub fn set_key_mapping(&mut self, target: MapTarget, key: &str) {
        let (min, max) = self.key_range(target);
        self.project.key_mappings.retain(|m| m.target != target);
        self.project.key_mappings.push(KeyMapping {
            key: key.to_string(),
            target,
            min,
            max,
            toggle: !matches!(
                target,
                MapTarget::ClipSlot { .. } | MapTarget::Scene(_) | MapTarget::Control(_)
            ),
        });
        self.mark_edit("Key Mapping");
    }

    pub fn delete_midi_mapping(&mut self, index: usize) {
        if index < self.project.midi_mappings.len() {
            self.project.midi_mappings.remove(index);
            self.mark_edit("Delete Mapping");
        }
    }

    pub fn delete_key_mapping(&mut self, index: usize) {
        if index < self.project.key_mappings.len() {
            self.project.key_mappings.remove(index);
            self.mark_edit("Delete Mapping");
        }
    }

    /// Sets the Min and Max of a mapping in the Mapping Browser.
    pub fn set_mapping_range(&mut self, mode: MapMode, index: usize, min: f64, max: f64) {
        let row = match mode {
            MapMode::Midi => self
                .project
                .midi_mappings
                .get_mut(index)
                .map(|m| (&mut m.min, &mut m.max)),
            MapMode::Key => self
                .project
                .key_mappings
                .get_mut(index)
                .map(|m| (&mut m.min, &mut m.max)),
        };
        if let Some((lo, hi)) = row {
            *lo = min;
            *hi = max;
            self.mark_edit("Change Mapping");
        }
    }

    /// Swaps a mapping's Min and Max.
    pub fn invert_mapping(&mut self, mode: MapMode, index: usize) {
        let range = match mode {
            MapMode::Midi => self
                .project
                .midi_mappings
                .get(index)
                .map(|m| (m.min, m.max)),
            MapMode::Key => self.project.key_mappings.get(index).map(|m| (m.min, m.max)),
        };
        if let Some((min, max)) = range {
            self.set_mapping_range(mode, index, max, min);
        }
    }

    /// Picks how a mapping's controller is read.
    pub fn set_mapping_encoding(&mut self, index: usize, encoding: CcEncoding) {
        if let Some(MidiMapping {
            source: MidiSource::Cc { encoding: e, .. },
            ..
        }) = self.project.midi_mappings.get_mut(index)
        {
            *e = encoding;
            self.mark_edit("Change Mapping");
        }
    }

    /// A mapped target's control name, for its overlay.
    pub fn mapped_label(&self, target: MapTarget, mode: MapMode) -> Option<String> {
        match mode {
            MapMode::Midi => self
                .project
                .midi_mappings
                .iter()
                .find(|m| m.target == target)
                .map(|m| source_text(m.channel, m.source)),
            MapMode::Key => self
                .project
                .key_mappings
                .iter()
                .find(|m| m.target == target)
                .map(|m| m.key.clone()),
        }
    }

    /// The rows of the Mapping Browser for a mode.
    pub fn mapping_rows(&self, mode: MapMode) -> Vec<MappingRow> {
        match mode {
            MapMode::Midi => self
                .project
                .midi_mappings
                .iter()
                .enumerate()
                .map(|(index, m)| {
                    let (path, name) = self.describe_target(m.target);
                    MappingRow {
                        index,
                        control: source_text(m.channel, m.source),
                        path,
                        name,
                        min: m.min,
                        max: m.max,
                        fixed_range: !matches!(m.target, MapTarget::Param(_)),
                        encoding: match m.source {
                            MidiSource::Cc { encoding, .. } => Some(encoding),
                            _ => None,
                        },
                    }
                })
                .collect(),
            MapMode::Key => self
                .project
                .key_mappings
                .iter()
                .enumerate()
                .map(|(index, m)| {
                    let (path, name) = self.describe_target(m.target);
                    MappingRow {
                        index,
                        control: m.key.clone(),
                        path,
                        name,
                        min: m.min,
                        max: m.max,
                        fixed_range: !matches!(m.target, MapTarget::Param(_)),
                        encoding: None,
                    }
                })
                .collect(),
        }
    }

    /// Where a target is and what it's called, for the Mapping Browser.
    pub fn mapping_describe(&self, t: MapTarget) -> (String, String) {
        self.describe_target(t)
    }

    /// Where a target is and what it's called.
    fn describe_target(&self, t: MapTarget) -> (String, String) {
        let track_name = |id: TrackId| {
            self.project
                .track(id)
                .map_or_else(|| "Missing".to_string(), |t| t.name.clone())
        };
        let scene_name = |n: usize| match self.project.scenes.get(n) {
            Some(s) if !s.name.is_empty() => s.name.clone(),
            _ => format!("Scene {}", n + 1),
        };
        match t {
            MapTarget::Param(r) => self.describe_param(r),
            MapTarget::ClipSlot { track, slot } => (track_name(track), scene_name(slot)),
            MapTarget::Scene(n) => ("Scene".into(), scene_name(n)),
            MapTarget::TrackStop(t) => (track_name(t), "Clip Stop".into()),
            MapTarget::TrackArm(t) => (track_name(t), "Arm".into()),
            MapTarget::HighlightedSlot(t) => (track_name(t), "Highlighted Clip".into()),
            MapTarget::Crossfader(p) => (
                "Mixer".into(),
                match p {
                    CrossfaderPoint::Left => "Crossfader Left",
                    CrossfaderPoint::Center => "Crossfader Center",
                    CrossfaderPoint::Right => "Crossfader Right",
                }
                .into(),
            ),
            MapTarget::Locator(tick) => (
                "Locators".into(),
                self.project
                    .arrangement
                    .locators
                    .iter()
                    .find(|l| l.tick == tick)
                    .map_or_else(String::new, |l| l.name.clone()),
            ),
            MapTarget::Control(c) => ("Control Bar".into(), c.label().to_string()),
        }
    }

    pub(super) fn describe_param(&self, r: ParamRef) -> (String, String) {
        match r.owner {
            ParamOwner::Device(d) => {
                let path = self
                    .chain_of_device(d)
                    .map(|c| self.chain_owner_name(c))
                    .unwrap_or_default();
                let name = self
                    .runtimes
                    .get(&d)
                    .and_then(|rt| rt.params.iter().find(|p| p.id == r.id.0))
                    .map_or_else(
                        || {
                            if r.id == ACTIVATOR {
                                "Device On".to_string()
                            } else {
                                format!("Parameter {}", r.id.0)
                            }
                        },
                        |p| p.name.clone(),
                    );
                let device = self
                    .find_device_named(d)
                    .map_or_else(String::new, |n| format!(" | {n}"));
                (format!("{path}{device}"), name)
            }
            ParamOwner::Track(t) => (
                self.project
                    .track(t)
                    .map_or_else(String::new, |t| t.name.clone()),
                mixer_control_name(r.id).to_string(),
            ),
            ParamOwner::Chain(c) => ("Rack Chain".into(), format!("Control {}", c.0)),
            ParamOwner::Master => ("Main".into(), mixer_control_name(r.id).to_string()),
            ParamOwner::Transport => ("Control Bar".into(), "Tempo".into()),
        }
    }

    fn chain_owner_name(&self, chain: ChainRef) -> String {
        match chain.owner() {
            ChainRef::Track(t) => self
                .project
                .track(t)
                .map_or_else(String::new, |t| t.name.clone()),
            _ => "Main".into(),
        }
    }

    fn find_device_named(&self, id: DeviceId) -> Option<String> {
        let chain = self.chain_of_device(id)?;
        self.devices(chain)
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
    }

    /// What a new MIDI mapping sweeps.
    fn default_range(&self, target: MapTarget) -> (f64, f64) {
        match target {
            MapTarget::Param(r) => match self.param_range(r) {
                Some(range) if is_toggle(range) => (64.0, 127.0),
                Some(range) => (range.min, range.max),
                None => (0.0, 127.0),
            },
            _ => (0.0, 127.0),
        }
    }

    fn key_range(&self, target: MapTarget) -> (f64, f64) {
        match target {
            MapTarget::Param(r) => self
                .param_range(r)
                .map_or((0.0, 1.0), |range| (range.min, range.max)),
            _ => (0.0, 1.0),
        }
    }

    /// A parameter's range, wherever it is.
    pub(crate) fn param_range(&self, r: ParamRef) -> Option<ParamRange> {
        if r.owner == ParamOwner::Transport {
            return Some(ParamRange {
                min: 20.0,
                max: 999.0,
                skew: 1.0,
                quantized: false,
            });
        }
        let owner = self.owner_of(r)?;
        let layout = self.layouts.get(&owner)?;
        let cell = super::automation::cell_of(r, owner, layout)?;
        layout.spec(cell).map(|s| s.range)
    }

    /// The track or the Master a parameter's cell is under.
    fn owner_of(&self, r: ParamRef) -> Option<ChainRef> {
        match r.owner {
            ParamOwner::Device(d) => self.chain_of_device(d).map(ChainRef::owner),
            ParamOwner::Track(t) => Some(ChainRef::Track(t)),
            ParamOwner::Chain(c) => self.owner_of_chain(c),
            ParamOwner::Master => Some(ChainRef::Master),
            ParamOwner::Transport => None,
        }
    }

    fn owner_of_chain(&self, id: ChainId) -> Option<ChainRef> {
        let mut owners = Owners::default();
        self.index_owners(&mut owners);
        owners.chains.get(&id).copied()
    }

    fn index_owners(&self, o: &mut Owners) {
        let p = &self.project;
        for t in p.tracks.iter().chain(&p.returns) {
            o.add(&t.devices, ChainRef::Track(t.id));
        }
        o.add(&p.master.devices, ChainRef::Master);
    }

    // ------------------------------------------------------------- compiling

    fn compile_target(&self, target: MapTarget, min: f64, max: f64, owners: &Owners) -> Exec {
        match target {
            MapTarget::Param(r) => self.compile_param(r, min, max, owners),
            MapTarget::ClipSlot { track, slot } => {
                let clip = self.clip(track, slot);
                let armed = self.project.track(track).is_some_and(|t| t.arm);
                let direct = !clip.is_some_and(|c| !c.active)
                    && !self.clip_waiting_for_sample(track, slot)
                    && !(armed && clip.is_none());
                Exec::Slot {
                    track,
                    slot: slot as u32,
                    direct,
                }
            }
            MapTarget::Scene(scene) => {
                let recording = self.record.start_recording_on_scene_launch
                    && !self.armed_empty_slots(scene).is_empty();
                let waiting = self
                    .project
                    .tracks
                    .iter()
                    .any(|t| self.clip_waiting_for_sample(t.id, scene));
                Exec::Scene {
                    scene: scene as u32,
                    direct: !recording && !waiting,
                }
            }
            MapTarget::TrackArm(_) => Exec::Ui(UiKind::Switch),
            MapTarget::Control(c) => Exec::Ui(match c.kind() {
                ControlKind::Switch => UiKind::Switch,
                ControlKind::Button => UiKind::Button,
                ControlKind::Scroll => UiKind::Scroll,
            }),
            _ => Exec::Ui(UiKind::Button),
        }
    }

    fn compile_param(&self, r: ParamRef, min: f64, max: f64, owners: &Owners) -> Exec {
        if r.owner == ParamOwner::Transport {
            let range = ParamRange {
                min: 20.0,
                max: 999.0,
                skew: 1.0,
                quantized: false,
            };
            return Exec::Param {
                target: r,
                addr: None,
                shape: ParamShape {
                    range,
                    min,
                    max,
                    kind: ShapeKind::Continuous,
                },
            };
        }
        // Whatever owns it right now, the message stays away from the track inputs.
        if owners.macro_mapped.contains(&r) {
            return Exec::Blocked;
        }
        let owner = match r.owner {
            ParamOwner::Device(d) => owners.devices.get(&d).copied().map(ChainRef::owner),
            ParamOwner::Chain(c) => owners.chains.get(&c).copied(),
            ParamOwner::Track(t) => Some(ChainRef::Track(t)),
            ParamOwner::Master => Some(ChainRef::Master),
            ParamOwner::Transport => None,
        };
        let Some(owner) = owner else {
            return Exec::Blocked;
        };
        if self.devices_locked(owner) {
            return Exec::Blocked;
        }
        let Some(layout) = self.layouts.get(&owner) else {
            return Exec::Blocked;
        };
        let Some(cell) = super::automation::cell_of(r, owner, layout) else {
            return Exec::Blocked;
        };
        let Some(spec) = layout.spec(cell) else {
            return Exec::Blocked;
        };
        let kind = if is_toggle(spec.range) {
            ShapeKind::Toggle {
                invert: is_track_activator(r),
            }
        } else if spec.range.quantized {
            ShapeKind::Stepped
        } else {
            ShapeKind::Continuous
        };
        // Solo does more than set a cell.
        let addr =
            (r.id != mixer::SOLO || !matches!(r.owner, ParamOwner::Track(_))).then_some(CellAddr {
                track: track_of(owner),
                cell: cell.0,
            });
        Exec::Param {
            target: r,
            addr,
            shape: ParamShape {
                range: spec.range,
                min,
                max,
                kind,
            },
        }
    }

    /// Compiles the project's mappings and the surfaces' instant mappings.
    fn compile_remote(&self) -> Vec<Compiled> {
        let mut owners = Owners::default();
        self.index_owners(&mut owners);
        let ports = self.remote.ports.in_mask;
        let feedback = self.remote.ports.out_mask;
        let mut out: Vec<Compiled> = self
            .project
            .midi_mappings
            .iter()
            .map(|m| Compiled {
                channel: m.channel,
                source: m.source,
                target: m.target,
                exec: self.compile_target(m.target, m.min, m.max, &owners),
                ports,
                feedback,
                value: 0.0,
            })
            .collect();
        for (i, slot) in self.remote.surfaces.in_use() {
            let Some(in_port) = self.input_index(slot.input.as_deref()) else {
                continue;
            };
            let feedback = self
                .output_index(slot.output.as_deref())
                .map_or(0, |p| 1u64 << p);
            let ports = 1u64 << in_port;
            for c in slot.surface.claims() {
                out.push(Compiled {
                    channel: c.channel,
                    source: c.source,
                    // Nothing the project can name: a surface claims the control itself.
                    target: MapTarget::Scene(usize::MAX),
                    exec: Exec::Surface { slot: i as u8 },
                    ports,
                    feedback: 0,
                    value: 0.0,
                });
            }
            let Some(device) = self.surface_device(i) else {
                continue;
            };
            for Assignment {
                channel,
                source,
                target,
            } in slot.surface.assign(&device, slot.bank)
            {
                // A manual mapping of the same control takes its place (p. 866).
                if self
                    .project
                    .midi_mappings
                    .iter()
                    .any(|m| m.channel == channel && m.source == source)
                {
                    continue;
                }
                let (min, max) = self.default_range(MapTarget::Param(target));
                out.push(Compiled {
                    channel,
                    source,
                    target: MapTarget::Param(target),
                    exec: self.compile_target(MapTarget::Param(target), min, max, &owners),
                    ports,
                    feedback,
                    value: 0.0,
                });
            }
        }
        out
    }

    fn input_index(&self, name: Option<&str>) -> Option<u32> {
        let name = name?;
        let names = &self.remote.ports.in_names;
        names.iter().position(|n| n == name).map(|p| p as u32)
    }

    fn output_index(&self, name: Option<&str>) -> Option<u32> {
        let name = name?;
        let names = &self.remote.ports.out_names;
        names.iter().position(|n| n == name).map(|p| p as u32)
    }

    /// What a surface in row `index` should control: its lock, then an armed track's instrument
    /// (manual p. 867), then whatever is selected.
    pub fn surface_device(&self, index: usize) -> Option<SurfaceDevice> {
        let slot = self.remote.surfaces.slots.get(index)?.as_ref()?;
        let id = slot
            .locked
            .or_else(|| self.armed_instrument())
            .or(self.remote.selected_device)?;
        self.describe_device(id)
    }

    fn armed_instrument(&self) -> Option<DeviceId> {
        let t = self
            .project
            .tracks
            .iter()
            .find(|t| t.arm && t.kind == rlive_project::TrackKind::Midi)?;
        t.devices
            .iter()
            .rev()
            .find(|d| super::graph::is_instrument(d) == Some(true))
            .map(|d| d.id)
    }

    fn describe_device(&self, id: DeviceId) -> Option<SurfaceDevice> {
        let chain = self.chain_of_device(id)?;
        let dev = self.devices(chain).iter().find(|d| d.id == id)?;
        let rt = self.runtimes.get(&id)?;
        let params = rt
            .params
            .iter()
            .filter(|p| !p.flags.contains(rlive_core::param::ParamFlags::HIDDEN))
            .map(|p| SurfaceParam {
                target: ParamRef {
                    owner: ParamOwner::Device(id),
                    id: p.param_id(),
                },
                name: p.name.clone(),
            })
            .collect();
        Some(SurfaceDevice {
            id,
            name: dev.name.clone(),
            params,
        })
    }

    /// Locks a surface to a device, or lets it follow the selection again.
    pub fn lock_surface(&mut self, index: usize, device: Option<DeviceId>) {
        let Some(Some(row)) = self.remote.surfaces.slots.get(index) else {
            return;
        };
        if !row.surface.supports_locking() {
            return;
        }
        let locks = &mut self.project.surface_locks;
        locks.retain(|(i, _)| usize::from(*i) != index);
        if let Some(d) = device {
            locks.push((index as u8, d));
        }
        self.sync_surface_locks();
        self.mark_edit("Lock to Device");
    }

    /// Follows the locks saved in the project, which undo and loading a Set can change.
    fn sync_surface_locks(&mut self) {
        let locks = &self.project.surface_locks;
        for (i, row) in self.remote.surfaces.slots.iter_mut().enumerate() {
            if let Some(row) = row {
                row.locked = locks
                    .iter()
                    .find(|(r, _)| usize::from(*r) == i)
                    .map(|(_, d)| *d);
            }
        }
    }

    /// A System Exclusive message for the control surfaces, each of which knows its own.
    pub fn surface_sysex(&mut self, bytes: &[u8]) {
        let indices: Vec<usize> = self.remote.surfaces.in_use().map(|(i, _)| i).collect();
        for i in indices {
            let output = self
                .remote
                .surfaces
                .slots
                .get(i)
                .and_then(|s| s.as_ref())
                .and_then(|s| s.output.clone());
            let port = self.output_index(output.as_deref());
            let RemoteState { surfaces, out, .. } = &mut self.remote;
            if let Some(Some(slot)) = surfaces.slots.get_mut(i) {
                slot.surface.on_sysex(bytes, &mut |b| {
                    if let (Some(p), Some(o)) = (port, out.as_mut()) {
                        o.send(p, b);
                    }
                });
            }
        }
    }

    /// Sends a surface's preset dump to its hardware (the Dump button).
    pub fn dump_surface(&mut self, index: usize) {
        let Some(Some(slot)) = self.remote.surfaces.slots.get(index) else {
            return;
        };
        let Some(dump) = slot.surface.preset_dump() else {
            return;
        };
        let port = self.output_index(slot.output.as_deref());
        if let (Some(port), Some(out)) = (port, self.remote.out.as_mut()) {
            for m in dump {
                out.send(port, &m);
            }
        }
    }

    // ------------------------------------------------------------- every frame

    /// Catches up with the MIDI thread, publishes changed mappings and sends feedback.
    pub(super) fn poll_remote(&mut self) {
        self.sync_surface_locks();
        let moved = self.drain_remote_events();
        if moved {
            self.note_reported_edit();
        }
        self.publish_remote_table();
        self.track_remote_values();
        self.send_remote_feedback();
        self.poll_surfaces();
    }

    fn drain_remote_events(&mut self) -> bool {
        let events = self.remote.hub.poll();
        let mut moved = false;
        for e in events {
            match e {
                RemoteEvent::Param {
                    target,
                    value,
                    direct,
                } => {
                    moved = true;
                    self.apply_remote_param(target, value, direct);
                }
                RemoteEvent::SlotDown {
                    track,
                    slot,
                    velocity,
                    transpose,
                    direct,
                } => {
                    let slot = slot as usize;
                    if direct {
                        self.re_enable_clip_automation(track, slot);
                    } else {
                        self.press_slot(track, slot, velocity, transpose);
                    }
                }
                RemoteEvent::SlotUp {
                    track,
                    slot,
                    direct,
                } => {
                    if !direct {
                        self.clip_up(track, slot as usize);
                    }
                }
                RemoteEvent::Scene { scene, direct } => {
                    let scene = scene as usize;
                    if direct {
                        let tracks: Vec<TrackId> =
                            self.project.tracks.iter().map(|t| t.id).collect();
                        for t in tracks {
                            self.re_enable_clip_automation(t, scene);
                        }
                    } else {
                        self.launch_scene(scene);
                    }
                }
                RemoteEvent::Gesture { target, gesture } => self.apply_gesture(target, gesture),
                RemoteEvent::Learn(event) => self.learn(event),
                RemoteEvent::Surface { slot, bytes } => self.surface_message(slot, bytes),
            }
        }
        moved
    }

    /// A slot was played from a mapped message that the MIDI thread couldn't send straight on.
    fn press_slot(&mut self, track: TrackId, slot: usize, velocity: f32, transpose: i8) {
        let armed = self.project.track(track).is_some_and(|t| t.arm);
        if armed && self.clip(track, slot).is_none() {
            self.record_slot(track, slot);
            return;
        }
        if transpose != 0 {
            self.send(rlive_engine::EngineCommand::Launch(
                rlive_engine::LaunchCmd::Transpose {
                    track,
                    semitones: transpose,
                },
            ));
        }
        self.clip_down(track, slot, velocity);
    }

    /// A parameter a controller moved. If `direct`, the engine has it already; everything else
    /// about the change happens the same as for a knob, including latching (A2).
    fn apply_remote_param(&mut self, target: ParamRef, value: f64, direct: bool) {
        let norm = self
            .param_range(target)
            .map_or(value, |r| r.to_normalized(value));
        self.mark_table_value(target, norm);
        if let (Some(owner), Some(cell)) = (self.owner_of(target), self.cell_for(target)) {
            let chain = match target.owner {
                ParamOwner::Device(d) => self.chain_of_device(d).unwrap_or(owner),
                _ => owner,
            };
            self.note_hardware_gesture(chain, cell);
        }
        self.remote.store_only = direct;
        self.write_mapped_param(target, value);
        self.remote.store_only = false;
    }

    fn cell_for(&self, target: ParamRef) -> Option<CellIx> {
        let owner = self.owner_of(target)?;
        let layout = self.layouts.get(&owner)?;
        super::automation::cell_of(target, owner, layout)
    }

    fn write_mapped_param(&mut self, target: ParamRef, value: f64) {
        match target.owner {
            ParamOwner::Device(d) => {
                let Some(chain) = self.chain_of_device(d) else {
                    return;
                };
                if target.id == ACTIVATOR {
                    self.set_bypass(chain, d, value < 0.5);
                } else {
                    self.write_param(chain, d, target.id.0, value, true);
                }
            }
            ParamOwner::Track(id) => {
                if target.id == mixer::SOLO {
                    self.mapped_solo(id, value >= 0.5);
                    return;
                }
                if target.id == mixer::CROSSFADE_ASSIGN {
                    let side = match value as i64 {
                        1 => rlive_project::Crossfade::A,
                        2 => rlive_project::Crossfade::B,
                        _ => rlive_project::Crossfade::None,
                    };
                    self.set_crossfade_assign(&[id], side);
                    return;
                }
                let returns: Vec<TrackId> = self.project.returns.iter().map(|t| t.id).collect();
                if let Some(t) = self.project.track_mut(id) {
                    super::render::set_mixer_value(t, &returns, target.id, value);
                }
                self.push_mix(id, true);
            }
            ParamOwner::Chain(c) => {
                let owner = self.owner_of_chain(c);
                if let Some((chain, rack)) = owner.and_then(|o| self.find_chain(o, c)) {
                    self.write_chain_param(chain, rack, c, target.id, value, true);
                }
            }
            ParamOwner::Master => {
                if target.id == mixer::VOLUME {
                    self.set_master_volume(value as f32);
                } else if target.id == mixer::CROSSFADER {
                    self.set_crossfader(value as f32);
                } else if target.id == mixer::CUE_VOLUME {
                    self.set_cue_volume(value as f32);
                }
            }
            ParamOwner::Transport => self.set_tempo(value),
        }
    }

    /// Solo from a controller, exclusive as a plain click on the button is.
    fn mapped_solo(&mut self, id: TrackId, on: bool) {
        if on {
            let others: Vec<TrackId> = self
                .project
                .tracks
                .iter()
                .chain(&self.project.returns)
                .filter(|t| t.id != id && t.solo)
                .map(|t| t.id)
                .collect();
            for other in others {
                if let Some(t) = self.project.track_mut(other) {
                    t.solo = false;
                }
                self.sync_mix(other);
            }
        }
        if let Some(t) = self.project.track_mut(id) {
            t.solo = on;
        }
        self.sync_mix(id);
    }

    /// Records in the table that a value came from there, so it isn't reported straight back.
    fn mark_table_value(&mut self, target: ParamRef, norm: f64) {
        let Some(table) = self.remote.hub.table().cloned() else {
            return;
        };
        for (i, e) in table.entries().iter().enumerate() {
            if matches!(e.exec, Exec::Param { target: t, .. } if t == target) {
                if let Some(seen) = self.remote.seen.get_mut(i) {
                    *seen = norm;
                }
            }
        }
    }

    fn publish_remote_table(&mut self) {
        let empty =
            self.project.midi_mappings.is_empty() && self.remote.surfaces.in_use().next().is_none();
        if empty && self.remote.last.is_empty() {
            return;
        }
        let compiled = self.compile_remote();
        if compiled == self.remote.last {
            return;
        }
        let table = {
            let previous = self.remote.hub.table().cloned();
            Arc::new(RemoteTable::new(compiled.clone(), previous.as_deref()))
        };
        self.remote.hub.publish(table);
        self.remote.last = compiled;
        self.remote.seen = vec![f64::NAN; self.remote.last.len()];
    }

    /// Tells the table about values that have changed since it last heard: a knob moved in the
    /// UI, an undo, automation.
    fn track_remote_values(&mut self) {
        let Some(table) = self.remote.hub.table().cloned() else {
            return;
        };
        for (i, e) in table.entries().iter().enumerate() {
            let Exec::Param { target, .. } = e.exec else {
                continue;
            };
            let Some(norm) = self.mapped_param_norm(target) else {
                continue;
            };
            let seen = &mut self.remote.seen[i];
            if !same(*seen, norm) {
                *seen = norm;
                table.set_value(i, norm);
            }
        }
    }

    /// A mapped parameter's value as a controller should take it, normalized: where automation
    /// has it, if automation is what's being heard.
    fn mapped_param_norm(&self, target: ParamRef) -> Option<f64> {
        match target.owner {
            ParamOwner::Transport => {
                let range = self.param_range(target)?;
                return Some(range.to_normalized(self.project.tempo));
            }
            ParamOwner::Track(id) if target.id == mixer::SOLO => {
                return Some(f64::from(u8::from(self.project.track(id)?.solo)));
            }
            _ => {}
        }
        let owner = self.owner_of(target)?;
        let layout = self.layouts.get(&owner)?;
        let cell = super::automation::cell_of(target, owner, layout)?;
        let spec = layout.spec(cell)?;
        let auto = (layout.state(cell).automated() && !layout.state(cell).overridden())
            .then(|| self.automated_cell_value(owner, cell))
            .flatten();
        let plain = auto.or(layout.base(cell))?;
        Some(spec.range.to_normalized(plain))
    }

    fn send_remote_feedback(&mut self) {
        if self.remote.out.is_none() {
            return;
        }
        let Some(table) = self.remote.hub.table().cloned() else {
            return;
        };
        let mask = self.remote.ports.out_mask;
        let entries = table.entries().to_vec();
        for (i, e) in entries.iter().enumerate() {
            let Some(value) = self.feedback_value(e, table.value(i)) else {
                continue;
            };
            let received = table.last_raw(i);
            let RemoteState { feedback, out, .. } = &mut self.remote;
            if let Some(out) = out {
                feedback.update(e, value, received, mask, &mut SinkRef(out.as_mut()));
            }
        }
    }

    fn feedback_value(&self, e: &Compiled, norm: f64) -> Option<FeedbackValue> {
        match e.exec {
            Exec::Param { shape, .. } => Some(match shape.kind {
                ShapeKind::Toggle { invert } => FeedbackValue::On((norm >= 0.5) != invert),
                _ => {
                    let (low, high) = (
                        shape.range.to_normalized(shape.min),
                        shape.range.to_normalized(shape.max),
                    );
                    FeedbackValue::Position(if low == high {
                        0.0
                    } else {
                        ((norm - low) / (high - low)).clamp(0.0, 1.0)
                    })
                }
            }),
            Exec::Slot { track, slot, .. } => Some(FeedbackValue::On(
                self.track_shared
                    .get(&track)
                    .is_some_and(|s| s.playing_slot.load(Relaxed) == slot as i32),
            )),
            Exec::Ui(UiKind::Switch) => {
                let MapTarget::Control(c) = e.target else {
                    return None;
                };
                self.control_state(c).map(FeedbackValue::On)
            }
            _ => None,
        }
    }

    /// Whether a switch in the Control Bar is on.
    pub fn control_state(&self, c: RemoteControl) -> Option<bool> {
        let a = &self.project.arrangement;
        match c {
            RemoteControl::Play => Some(self.is_playing()),
            RemoteControl::Record => Some(self.is_recording()),
            RemoteControl::MidiOverdub => Some(self.project.midi_arrangement_overdub),
            RemoteControl::AutomationArm => Some(self.automation_arm()),
            RemoteControl::Metronome => Some(self.project.metronome),
            RemoteControl::Loop => Some(a.loop_enabled),
            RemoteControl::PunchIn => Some(a.punch_in),
            RemoteControl::PunchOut => Some(a.punch_out),
            _ => None,
        }
    }

    fn poll_surfaces(&mut self) {
        let indices: Vec<usize> = self.remote.surfaces.in_use().map(|(i, _)| i).collect();
        for i in indices {
            let device = self.surface_device(i);
            let output = self
                .remote
                .surfaces
                .slots
                .get(i)
                .and_then(|s| s.as_ref())
                .and_then(|s| s.output.clone());
            let port = self.output_index(output.as_deref());
            let RemoteState { surfaces, out, .. } = &mut self.remote;
            if let Some(Some(slot)) = surfaces.slots.get_mut(i) {
                slot.surface.poll(device.as_ref(), &mut |bytes| {
                    if let (Some(p), Some(o)) = (port, out.as_mut()) {
                        o.send(p, bytes);
                    }
                });
            }
        }
    }

    fn surface_message(&mut self, slot: u8, bytes: [u8; 3]) {
        let Some(event) = MidiEvent::parse(&bytes) else {
            return;
        };
        let index = usize::from(slot);
        let output = self
            .remote
            .surfaces
            .slots
            .get(index)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.output.clone());
        let port = self.output_index(output.as_deref());
        let RemoteState { surfaces, out, .. } = &mut self.remote;
        surfaces.on_midi(index, event, &mut |b| {
            if let (Some(p), Some(o)) = (port, out.as_mut()) {
                o.send(p, b);
            }
        });
    }

    // ------------------------------------------------------------- learning

    fn learn(&mut self, event: MidiEvent) {
        if self.remote.map_mode != Some(MapMode::Midi) {
            return;
        }
        let Some(target) = self.remote.selected else {
            return;
        };
        let chromatic = matches!(target, MapTarget::ClipSlot { .. });
        if let Some(learned) = self.remote.learner.feed(event, chromatic) {
            self.set_midi_mapping(target, learned.channel, learned.source);
        }
    }

    // ------------------------------------------------------------- what mappings do

    /// Runs what a message or key said to a target with no parameter of its own. Anything that
    /// needs the UI's selection is left for [`Self::take_remote_actions`].
    pub fn apply_gesture(&mut self, target: MapTarget, gesture: Gesture) {
        match (target, gesture) {
            (MapTarget::ClipSlot { track, slot }, Gesture::Press(v)) => {
                self.press_slot(track, slot, v, 0);
            }
            (MapTarget::ClipSlot { track, slot }, Gesture::Release) => self.clip_up(track, slot),
            (MapTarget::Scene(s), Gesture::Press(_)) => self.launch_scene(s),
            (MapTarget::TrackStop(t), Gesture::Press(_)) => self.stop_track(t),
            (MapTarget::TrackArm(t), g) => self.gesture_arm(t, g),
            (MapTarget::Crossfader(p), Gesture::Press(_)) => {
                self.crossfader_key_down(point_position(p));
            }
            (MapTarget::Crossfader(p), Gesture::Release) => {
                self.crossfader_key_up(point_position(p));
            }
            (MapTarget::Locator(tick), Gesture::Press(_)) => self.seek(tick as f64),
            (MapTarget::Scene(_) | MapTarget::TrackStop(_) | MapTarget::Locator(_), _) => {}
            (MapTarget::ClipSlot { .. } | MapTarget::Crossfader(_), _) => {}
            (MapTarget::Param(_), _) => {}
            (MapTarget::HighlightedSlot(_) | MapTarget::Control(_), _) => {
                self.remote.actions.push((target, gesture));
            }
        }
    }

    fn gesture_arm(&mut self, track: TrackId, g: Gesture) {
        let armed = self.project.track(track).is_some_and(|t| t.arm);
        let exclusive = self.record.exclusive_arm;
        match g {
            Gesture::Toggle => self.set_arm(track, !armed, exclusive),
            Gesture::On => self.set_arm(track, true, exclusive),
            Gesture::Off => self.set_arm(track, false, exclusive),
            _ => {}
        }
    }

    /// Targets the UI has to work on, since they depend on what it has selected.
    pub fn take_remote_actions(&mut self) -> Vec<(MapTarget, Gesture)> {
        std::mem::take(&mut self.remote.actions)
    }

    /// A mapped key went down or came up.
    pub fn key_mapping_event(&mut self, m: &KeyMapping, down: bool) {
        match m.target {
            MapTarget::Param(r) => {
                if down {
                    self.key_toggles_param(r, m.min, m.max);
                }
            }
            MapTarget::TrackArm(t) => {
                if down {
                    self.gesture_arm(t, Gesture::Toggle);
                }
            }
            MapTarget::Control(c) if c.kind() == ControlKind::Switch => {
                if down {
                    self.remote.actions.push((m.target, Gesture::Toggle));
                }
            }
            MapTarget::Control(c) if c.kind() == ControlKind::Scroll => {
                if down {
                    self.remote.actions.push((m.target, Gesture::Step(1)));
                }
            }
            target => {
                let g = if down {
                    Gesture::Press(1.0)
                } else {
                    Gesture::Release
                };
                self.apply_gesture(target, g);
            }
        }
    }

    /// A key on a parameter: a switch flips, a radio button moves on, anything else swaps
    /// between the mapping's two ends.
    fn key_toggles_param(&mut self, r: ParamRef, min: f64, max: f64) {
        let Some(range) = self.param_range(r) else {
            return;
        };
        let Some(norm) = self.mapped_param_norm(r) else {
            return;
        };
        let now = range.from_normalized(norm);
        let next = if is_toggle(range) {
            if now >= 0.5 {
                0.0
            } else {
                1.0
            }
        } else if range.quantized {
            if now + 1.0 > range.max {
                range.min
            } else {
                now + 1.0
            }
        } else if (now - max).abs() < (now - min).abs() {
            min
        } else {
            max
        };
        self.apply_remote_param(r, next, false);
        self.note_reported_edit();
    }
}

/// Whether two normalized values are the same to within what anyone could hear.
fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn is_track_activator(r: ParamRef) -> bool {
    r.id == mixer::MUTE && matches!(r.owner, ParamOwner::Track(_))
}

fn track_of(owner: ChainRef) -> Option<TrackId> {
    match owner {
        ChainRef::Track(t) => Some(t),
        _ => None,
    }
}

fn mixer_control_name(id: rlive_core::ParamId) -> &'static str {
    match id {
        mixer::VOLUME => "Volume",
        mixer::PAN => "Pan",
        mixer::MUTE => "Activator",
        mixer::SOLO => "Solo",
        mixer::CROSSFADE_ASSIGN => "Crossfade Assign",
        mixer::CROSSFADER => "Crossfader",
        mixer::CUE_VOLUME => "Cue Volume",
        rlive_core::ParamId(n) if n >= mixer::FIRST_SEND.0 => "Send",
        _ => "Control",
    }
}

/// Lets a boxed sink be passed where `impl FeedbackSink` is asked for.
struct SinkRef<'a>(&'a mut (dyn FeedbackSink + Send));

impl FeedbackSink for SinkRef<'_> {
    fn send(&mut self, port: u32, bytes: &[u8]) {
        self.0.send(port, bytes);
    }
}
