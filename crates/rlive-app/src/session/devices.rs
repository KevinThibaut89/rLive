//! Device chains, nested to any depth: adding, removing and changing devices.

use super::{build, Session};
use rlive_audio_io::SampleStatus;
use rlive_core::param::{mixer, ParamOwner, ParamRef};
use rlive_core::{ChainId, DeviceId, ParamId, ParamInfo, TrackId};
use rlive_devices::DeviceRole;
use rlive_engine::{mix_cell, CellIx, ChainAddr, GraphOp, TrackRef};
use rlive_plugin_host::PluginDescriptor;
use rlive_project::{Chain, Device, DeviceKind, FileRef, RackKind, TrackKind};
use std::collections::HashMap;
use std::path::PathBuf;

/// Where a device lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainRef {
    Track(TrackId),
    Master,
    /// A chain inside a rack, or a return chain of one.
    Nested(ChainAddr),
}

impl ChainRef {
    /// The track or the Master this chain is under, which holds its parameter cells.
    pub fn owner(self) -> ChainRef {
        match self {
            ChainRef::Nested(addr) => match addr.owner {
                TrackRef::Track(t) => ChainRef::Track(t),
                TrackRef::Master => ChainRef::Master,
            },
            root => root,
        }
    }

    /// How many racks this chain is inside.
    pub fn depth(self) -> usize {
        ChainAddr::from(self).path.len()
    }

    /// A chain of the rack `rack`, found in this chain.
    pub fn child(self, rack: DeviceId, chain: ChainId) -> Option<ChainRef> {
        Some(ChainRef::Nested(ChainAddr::from(self).child(rack, chain)?))
    }
}

impl From<ChainRef> for ChainAddr {
    fn from(c: ChainRef) -> Self {
        match c {
            ChainRef::Track(t) => ChainAddr::track(t),
            ChainRef::Master => ChainAddr::MASTER,
            ChainRef::Nested(addr) => addr,
        }
    }
}

impl From<ChainRef> for TrackRef {
    fn from(c: ChainRef) -> Self {
        ChainAddr::from(c).owner
    }
}

impl Session {
    // ------------------------------------------------------------- devices

    fn chain(&self, chain: ChainRef) -> Option<&Vec<Device>> {
        match chain {
            ChainRef::Track(t) => self.project.track(t).map(|t| &t.devices),
            ChainRef::Master => Some(&self.project.master.devices),
            ChainRef::Nested(addr) => {
                let mut devices = self.chain(chain.owner())?;
                for step in addr.path.steps() {
                    let rack = devices.iter().find(|d| d.id == step.rack)?;
                    devices = &rack.chains().find(|c| c.id == step.chain)?.devices;
                }
                Some(devices)
            }
        }
    }

    pub(super) fn chain_mut(&mut self, chain: ChainRef) -> Option<&mut Vec<Device>> {
        match chain {
            ChainRef::Track(t) => self.project.track_mut(t).map(|t| &mut t.devices),
            ChainRef::Master => Some(&mut self.project.master.devices),
            ChainRef::Nested(addr) => {
                let mut devices = self.chain_mut(chain.owner())?;
                for step in addr.path.steps() {
                    let rack = devices.iter_mut().find(|d| d.id == step.rack)?;
                    devices = &mut rack.chains_mut().find(|c| c.id == step.chain)?.devices;
                }
                Some(devices)
            }
        }
    }

    pub fn devices(&self, chain: ChainRef) -> &[Device] {
        self.chain(chain).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn add_device(&mut self, chain: ChainRef, index: Option<usize>, device: Device) {
        let mut built = HashMap::new();
        let (mut b, _) = self.builder();
        let slot = b.device(&device, chain.depth(), &mut built);
        build::note_errors(&device, &built, &mut self.messages);
        let id = device.id;
        let Some(devs) = self.chain_mut(chain) else {
            return;
        };
        let index = index.unwrap_or(devs.len()).min(devs.len());
        devs.insert(index, device);
        self.runtimes.extend(built);
        let swap = self.place_device_cells(chain, id);
        let insert = GraphOp::InsertDevice {
            target: chain.into(),
            index,
            device: slot,
        };
        self.send_edits(swap.into_iter().chain([insert]).collect());
        self.mark_edit("Add Device");
    }

    /// Give a device and everything in it cells. Returns the edit that gives the engine a new bus,
    /// if that took one.
    fn place_device_cells(&mut self, chain: ChainRef, id: DeviceId) -> Option<GraphOp> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?.clone();
        let layout = self.layouts.get_mut(&chain.owner())?;
        build::place_device_tree(layout, &dev, &self.runtimes).then(|| GraphOp::SwapBus {
            target: chain.into(),
            bus: layout.build(),
        })
    }

    /// Update a device's cells after its parameter list changed.
    pub(super) fn refit_device_cells(&mut self, chain: ChainRef, id: DeviceId) {
        if let Some(swap) = self.place_device_cells(chain, id) {
            self.send_edits(vec![swap]);
        }
    }

    /// The kind of the rack a chain is directly inside, if it is inside one.
    pub(super) fn enclosing_rack_kind(&self, chain: ChainRef) -> Option<RackKind> {
        let ChainRef::Nested(addr) = chain else {
            return None;
        };
        let steps = addr.path.steps();
        let step = *steps.last()?;
        let mut path = rlive_engine::ChainPath::ROOT;
        for s in &steps[..steps.len() - 1] {
            path = path.child(*s)?;
        }
        let parent = ChainAddr {
            owner: addr.owner,
            path,
        };
        match &self
            .chain_at(parent)?
            .iter()
            .find(|d| d.id == step.rack)?
            .kind
        {
            DeviceKind::Rack { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    pub(super) fn chain_at(&self, addr: ChainAddr) -> Option<&Vec<Device>> {
        self.chain(chain_ref_of(addr))
    }

    /// Whether a chain can host this kind of device, by the rules of its track and of the rack
    /// it is in (manual p. 493). Reports it to the user when it can't.
    pub(super) fn accepts_role(&mut self, chain: ChainRef, role: DeviceRole, name: &str) -> bool {
        match self.enclosing_rack_kind(chain) {
            Some(RackKind::Midi) if role != DeviceRole::MidiEffect => {
                self.messages.push(format!(
                    "{name}: a MIDI Effect Rack takes MIDI effects only"
                ));
                return false;
            }
            Some(RackKind::Audio) if role != DeviceRole::AudioEffect => {
                self.messages.push(format!(
                    "{name}: an Audio Effect Rack takes audio effects only"
                ));
                return false;
            }
            _ => {}
        }
        let ChainRef::Track(id) = chain.owner() else {
            return true;
        };
        let audio_only = self
            .project
            .track(id)
            .is_some_and(|t| matches!(t.kind, TrackKind::Group | TrackKind::Return));
        if audio_only && role != DeviceRole::AudioEffect {
            self.messages.push(format!(
                "{name}: group and return tracks take audio effects only"
            ));
            return false;
        }
        true
    }

    pub fn add_builtin(&mut self, chain: ChainRef, type_id: &str) {
        if let Some(info) = rlive_devices::find(type_id) {
            if !self.accepts_role(chain, info.role, info.name) {
                return;
            }
            let d = Device::builtin(info.id, info.name);
            let index = self.instrument_insert_index(chain, info.role);
            self.add_device(chain, index, d);
        }
    }

    pub fn add_plugin(&mut self, chain: ChainRef, desc: &PluginDescriptor) {
        let d = Device::plugin(desc.format, &desc.id, desc.path.clone(), &desc.name);
        let role = if desc.is_instrument {
            DeviceRole::Instrument
        } else {
            DeviceRole::AudioEffect
        };
        if !self.accepts_role(chain, role, &desc.name) {
            return;
        }
        let index = self.instrument_insert_index(chain, role);
        self.add_device(chain, index, d);
    }

    /// MIDI effects go to the front of the chain, instruments after them (and after any other
    /// instrument), audio effects at the end.
    pub(super) fn instrument_insert_index(
        &self,
        chain: ChainRef,
        role: DeviceRole,
    ) -> Option<usize> {
        if role == DeviceRole::AudioEffect {
            return None;
        }
        let devs = self.devices(chain);
        Some(
            devs.iter()
                .take_while(|d| {
                    self.runtimes.get(&d.id).is_some_and(|r| match role {
                        DeviceRole::MidiEffect => r.role == DeviceRole::MidiEffect,
                        _ => r.role != DeviceRole::AudioEffect,
                    })
                })
                .count(),
        )
    }

    pub fn remove_device(&mut self, chain: ChainRef, id: DeviceId) {
        if let Some(devs) = self.chain_mut(chain) {
            if let Some(i) = devs.iter().position(|d| d.id == id) {
                let mut d = devs.remove(i);
                self.retire_device_runtimes(chain, &mut d);
            }
        }
        self.send_edits(vec![GraphOp::RemoveDevice {
            target: chain.into(),
            device: id,
        }]);
        self.mark_edit("Remove Device");
    }

    /// Forgets the runtime side of a device leaving the project, parking it for undo.
    pub(super) fn retire_device_runtimes(&mut self, chain: ChainRef, device: &mut Device) {
        self.park_device(device);
        let mut ids = Vec::new();
        device.for_each(&mut |d| ids.push(d.id));
        let chains = build::chains_of(device);
        self.forget_device_automation(chain, &ids);
        self.forget_chain_automation(chain, &chains);
        self.forget_macro_targets(chain, &ids, &chains);
        if let Some(l) = self.layouts.get_mut(&chain.owner()) {
            for c in chains {
                l.remove_chain(c);
            }
        }
        for id in ids {
            if let Some(mut rt) = self.runtimes.remove(&id) {
                if let Some(h) = rt.plugin.as_mut() {
                    h.close_editor();
                }
                // The handle is dropped now; the processor half comes back as engine garbage.
            }
            if let Some(l) = self.layouts.get_mut(&chain.owner()) {
                l.remove_device(id);
            }
        }
    }

    pub fn move_device(&mut self, chain: ChainRef, id: DeviceId, to: usize) {
        let Some(devs) = self.chain_mut(chain) else {
            return;
        };
        let Some(i) = devs.iter().position(|d| d.id == id) else {
            return;
        };
        let d = devs.remove(i);
        let to = to.min(devs.len());
        devs.insert(to, d);
        self.send_edits(vec![GraphOp::MoveDevice {
            target: chain.into(),
            device: id,
            to,
        }]);
        self.mark_edit("Move Device");
    }

    /// Fold/unfold a device in the device view (UI state stored in the project).
    pub fn set_device_expanded(&mut self, chain: ChainRef, id: DeviceId, expanded: bool) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.expanded = expanded;
            self.touch();
        }
    }

    pub fn set_bypass(&mut self, chain: ChainRef, id: DeviceId, bypass: bool) {
        if self.is_macro_mapped(chain, device_param(id, rlive_core::ACTIVATOR)) {
            return;
        }
        self.write_bypass(chain, id, bypass, true);
        self.mark_edit("Device On/Off");
    }

    /// Switches a device on or off in the project and the engine.
    pub(super) fn write_bypass(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        bypass: bool,
        gesture: bool,
    ) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.bypass = bypass;
        }
        let activator = self.layouts.get(&chain.owner());
        if let Some(cell) = activator.and_then(|l| l.activator_cell(id)) {
            self.set_cell(chain, cell, !bypass as u8 as f64, gesture);
        }
    }

    pub fn param_value(&self, chain: ChainRef, id: DeviceId, param: &ParamInfo) -> f64 {
        if let Some(v) = self
            .chain(chain)
            .and_then(|v| v.iter().find(|d| d.id == id))
            .and_then(|d| d.stored_param(param.id))
        {
            return v;
        }
        self.runtimes
            .get(&id)
            .and_then(|r| r.plugin.as_ref())
            .and_then(|h| h.param_value(param.id))
            .unwrap_or(param.default)
    }

    pub fn set_param(&mut self, chain: ChainRef, id: DeviceId, param: u32, value: f64) {
        // A Macro owns this one. Only a clip envelope can still move it (manual p. 505).
        if self.is_macro_mapped(chain, device_param(id, ParamId(param))) {
            return;
        }
        self.write_param(chain, id, param, value, true);
        self.mark_edit("Change Parameter");
    }

    /// Sets a parameter in the project and sends it down the parameter path to the engine.
    pub(super) fn write_param(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        param: u32,
        value: f64,
        gesture: bool,
    ) {
        let cell = self.device_cell(chain, id, param);
        let before = self
            .layouts
            .get(&chain.owner())
            .zip(cell)
            .and_then(|(l, c)| l.base(c));
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            if d.store_param(param, value) {
                if let (Some(rt), Some(before)) = (self.runtimes.get_mut(&id), before) {
                    rt.unmoved.entry(param).or_insert(before);
                }
            }
        }
        if let Some(h) = self.runtimes.get_mut(&id).and_then(|r| r.plugin.as_mut()) {
            h.on_param_set(param, value);
        }
        if let Some(cell) = cell {
            self.set_cell(chain, cell, value, gesture);
        }
    }

    /// Records a value a device reported, in the project and the layout. Returns whether it was
    /// news, as opposed to the device repeating what it was told.
    fn note_device_param(&mut self, chain: ChainRef, id: DeviceId, param: u32, value: f64) -> bool {
        // A value it was seeded with hasn't reached `params` yet; the layout has it either way.
        let cell = self.device_cell(chain, id, param);
        let seeded = self
            .layouts
            .get(&chain.owner())
            .zip(cell)
            .and_then(|(l, c)| l.base(c));
        let device = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id));
        let known = device
            .as_ref()
            .and_then(|d| d.stored_param(param))
            .or(seeded);
        let changed = known.is_none_or(|old| !nearly_equal(old, value));
        let first_value = device.is_some_and(|d| d.store_param(param, value));
        if let (true, Some(rt), Some(before)) = (first_value, self.runtimes.get_mut(&id), seeded) {
            rt.unmoved.entry(param).or_insert(before);
        }
        if let (Some(l), Some(cell)) = (self.layouts.get_mut(&chain.owner()), cell) {
            l.note_value(cell, value);
        }
        changed
    }

    /// Records a parameter change a device made by itself. Its edits settle into an undo step
    /// once it goes quiet, since its gestures are invisible from here.
    pub(super) fn report_device_param(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        param: u32,
        value: f64,
    ) {
        if self.note_device_param(chain, id, param, value) {
            self.note_reported_edit();
        }
    }

    // ------------------------------------------------------------- parameter cells

    /// The cell of a device parameter, in O(1).
    pub fn device_cell(&self, chain: ChainRef, device: DeviceId, param: u32) -> Option<CellIx> {
        self.layouts.get(&chain.owner())?.device_cell(device, param)
    }

    /// The cell of a device's on/off switch, in O(1).
    pub fn activator_cell(&self, chain: ChainRef, device: DeviceId) -> Option<CellIx> {
        self.layouts.get(&chain.owner())?.activator_cell(device)
    }

    /// The cell of a mixer control, in O(1). The Master has only volume.
    pub fn mixer_cell(&self, chain: ChainRef, control: ParamId) -> Option<CellIx> {
        self.layouts.get(&chain)?;
        mixer_cell_of(chain, control)
    }

    /// The normalized value a parameter is sounding at, after automation and modulation.
    pub fn effective_norm(&self, chain: ChainRef, device: DeviceId, param: u32) -> Option<f32> {
        let layout = self.layouts.get(&chain.owner())?;
        layout.effective(layout.device_cell(device, param)?)
    }

    /// Change the sample of a built-in sampler. Playing voices carry on with the new sample; a
    /// file that won't load leaves the current one in place and reports why.
    pub fn set_device_sample(&mut self, chain: ChainRef, id: DeviceId, path: PathBuf) {
        self.set_device_sample_at(chain, id, 0, path);
    }

    /// [`set_device_sample`](Self::set_device_sample) for a device with more than one slot.
    pub fn set_device_sample_at(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        slot: usize,
        path: PathBuf,
    ) {
        if matches!(self.samples.status(&path), Some(SampleStatus::Failed(_))) {
            // Asking for a file again is a reason to look at it again.
            self.samples.forget(&path);
        }
        let file = FileRef::new(path);
        if matches!(self.samples.request(&file.path), SampleStatus::Loading) {
            // The sample it has now carries on until the new one is ready or turns out not to be.
            self.queue_device_sample(chain, id, slot, file);
        } else if self.write_device_sample(chain, id, slot, file) {
            self.mark_edit("Change Sample");
        }
    }

    /// Puts a file in a device's sample slot, in the project and, once it has loaded, the engine.
    pub(super) fn write_device_sample(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        slot: usize,
        file: FileRef,
    ) -> bool {
        let loaded = self.samples.request(&file.path);
        let error = match &loaded {
            SampleStatus::Failed(e) => Some(e.clone()),
            _ => None,
        };
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.error = error.clone();
        }
        if error.is_some() {
            self.messages.extend(error);
            return false;
        }
        let Some(dev) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        else {
            return false;
        };
        if let DeviceKind::Builtin { samples, .. } = &mut dev.kind {
            if samples.len() <= slot {
                samples.resize(slot + 1, None);
            }
            samples[slot] = Some(file);
        }
        if let SampleStatus::Ready(sample) = loaded {
            self.send_edits(vec![GraphOp::SetResource {
                target: chain.into(),
                device: id,
                slot: slot as u32,
                resource: sample,
            }]);
        }
        true
    }

    /// Re-instantiate a plugin (keeping its state), e.g. after it requested a restart.
    pub fn reload_device(&mut self, chain: ChainRef, id: DeviceId) {
        self.capture_plugin_states();
        self.rebuild_device(chain, id);
    }

    /// Replaces a device's engine side with a fresh one built from the project.
    pub(super) fn rebuild_device(&mut self, chain: ChainRef, id: DeviceId) {
        let Some(devs) = self.chain(chain) else {
            return;
        };
        let Some(pos) = devs.iter().position(|d| d.id == id) else {
            return;
        };
        let d = devs[pos].clone();
        let mut ids = Vec::new();
        d.for_each(&mut |nested| ids.push(nested.id));
        for nested in ids {
            self.runtimes.remove(&nested);
        }
        let mut built = HashMap::new();
        let (mut b, _) = self.builder();
        let slot = b.device(&d, chain.depth(), &mut built);
        self.runtimes.extend(built);
        let swap = self.place_device_cells(chain, id);
        let mut ops = vec![GraphOp::RemoveDevice {
            target: chain.into(),
            device: id,
        }];
        ops.extend(swap);
        ops.push(GraphOp::InsertDevice {
            target: chain.into(),
            index: pos,
            device: slot,
        });
        self.send_edits(ops);
    }

    // ------------------------------------------------------------- rack chains

    /// Adds a chain to the rack `rack`, found in the chain at `chain`.
    pub(super) fn insert_rack_chain(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        index: usize,
        new: Chain,
    ) {
        self.install_chain(chain, rack, index, new, false);
        self.mark_edit("Add Chain");
    }

    /// Adds a return chain to the Drum Rack `rack`, found in the chain at `chain`.
    pub(super) fn insert_return_chain(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        index: usize,
        new: Chain,
    ) {
        self.install_chain(chain, rack, index, new, true);
        self.mark_edit("Add Return Chain");
    }

    fn install_chain(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        index: usize,
        new: Chain,
        is_return: bool,
    ) {
        let depth = chain.depth() + 1;
        let mut built = HashMap::new();
        let (mut b, _) = self.builder();
        let mut node = b.chain(&new, depth, &mut built);
        for d in &new.devices {
            build::note_errors(d, &built, &mut self.messages);
        }
        let devices = new.devices.clone();
        let id = new.id;
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == rack))
        else {
            return;
        };
        let DeviceKind::Rack {
            chains, returns, ..
        } = &mut d.kind
        else {
            return;
        };
        let list = if is_return {
            &mut *returns
        } else {
            &mut *chains
        };
        let index = index.min(list.len());
        list.insert(index, new);
        let returns_now = returns.clone();
        let mix = returns_now
            .iter()
            .chain(chains.iter())
            .find(|c| c.id == id)
            .map(|c| build::chain_values(c, &returns_now));
        let seeded = returns_now
            .iter()
            .chain(chains.iter())
            .find(|c| c.id == id)
            .map(|c| build::engine_mix(c, &returns_now));
        if let Some(seed) = seeded {
            node.seed_mix(seed);
        }
        self.runtimes.extend(built);
        let layout = self.layouts.get_mut(&chain.owner());
        let swap = layout.and_then(|layout| {
            let mut needs_bus = mix.is_some_and(|m| layout.place_chain(id, &m));
            for d in &devices {
                needs_bus |= build::place_device_tree(layout, d, &self.runtimes);
            }
            needs_bus.then(|| GraphOp::SwapBus {
                target: chain.into(),
                bus: layout.build(),
            })
        });
        let insert = if is_return {
            GraphOp::InsertReturn {
                target: chain.into(),
                device: rack,
                index,
                chain: node,
            }
        } else {
            GraphOp::InsertChain {
                target: chain.into(),
                device: rack,
                index,
                chain: node,
            }
        };
        self.send_edits(swap.into_iter().chain([insert]).collect());
        self.push_rack_mix(chain, rack);
    }

    /// Removes a chain or a return chain from the rack `rack`, parking its devices for undo.
    pub(super) fn remove_rack_chain(&mut self, chain: ChainRef, rack: DeviceId, id: ChainId) {
        let removed = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == rack))
            .and_then(|d| {
                let DeviceKind::Rack {
                    chains, returns, ..
                } = &mut d.kind
                else {
                    return None;
                };
                match chains.iter().position(|c| c.id == id) {
                    Some(i) => Some(chains.remove(i)),
                    None => {
                        let i = returns.iter().position(|c| c.id == id)?;
                        // Sends to a return chain that is gone are gone too.
                        let removed = returns.remove(i);
                        for c in chains.iter_mut() {
                            c.mix.sends.retain(|s| s.to != id);
                        }
                        Some(removed)
                    }
                }
            });
        let Some(mut removed) = removed else {
            return;
        };
        for d in &mut removed.devices {
            self.retire_device_runtimes(chain, d);
        }
        self.forget_chain_automation(chain, &[id]);
        self.forget_macro_targets(chain, &[], &[id]);
        if let Some(l) = self.layouts.get_mut(&chain.owner()) {
            l.remove_chain(id);
        }
        self.send_edits(vec![GraphOp::RemoveChain {
            target: chain.into(),
            device: rack,
            chain: id,
        }]);
        self.push_rack_mix(chain, rack);
        self.mark_edit("Delete Chain");
    }

    pub(super) fn rack_chains_mut(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
    ) -> Option<&mut Vec<Chain>> {
        match &mut self
            .chain_mut(chain)?
            .iter_mut()
            .find(|d| d.id == rack)?
            .kind
        {
            DeviceKind::Rack { chains, .. } => Some(chains),
            _ => None,
        }
    }

    pub(super) fn chain_of_device(&self, id: DeviceId) -> Option<ChainRef> {
        find_device(&self.project.master.devices, ChainRef::Master, id).or_else(|| {
            self.project
                .tracks
                .iter()
                .chain(&self.project.returns)
                .find_map(|t| find_device(&t.devices, ChainRef::Track(t.id), id))
        })
    }
}

/// The cell of a mixer control of a track or the Master. The Master has only volume.
pub(super) fn mixer_cell_of(chain: ChainRef, control: ParamId) -> Option<CellIx> {
    match (chain, control) {
        (_, mixer::VOLUME) => Some(mix_cell::VOLUME),
        (ChainRef::Track(_), mixer::PAN) => Some(mix_cell::PAN),
        (ChainRef::Track(_), mixer::MUTE) => Some(mix_cell::MUTE),
        (ChainRef::Track(_), mixer::SOLO) => Some(mix_cell::SOLO),
        (ChainRef::Track(_), ParamId(id)) if id >= mixer::FIRST_SEND.0 => {
            let n = (id - mixer::FIRST_SEND.0) as usize;
            (n < rlive_engine::MAX_SENDS).then(|| mix_cell::send(n))
        }
        _ => None,
    }
}

/// The `ChainRef` an address came from.
pub(super) fn chain_ref_of(addr: ChainAddr) -> ChainRef {
    if addr.path.is_empty() {
        match addr.owner {
            TrackRef::Track(t) => ChainRef::Track(t),
            TrackRef::Master => ChainRef::Master,
        }
    } else {
        ChainRef::Nested(addr)
    }
}

fn device_param(device: DeviceId, id: ParamId) -> ParamRef {
    ParamRef {
        owner: ParamOwner::Device(device),
        id,
    }
}

/// The chain holding the device `id`, searching `devices` and everything nested in them.
fn find_device(devices: &[Device], here: ChainRef, id: DeviceId) -> Option<ChainRef> {
    for d in devices {
        if d.id == id {
            return Some(here);
        }
        for chain in d.chains() {
            let found = here
                .child(d.id, chain.id)
                .and_then(|child| find_device(&chain.devices, child, id));
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

/// Whether a device reporting `b` is only repeating `a`, give or take a float conversion.
fn nearly_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(1.0)
}
