//! Device chains: adding, removing and changing devices.

use super::{build, Session};
use rlive_core::param::mixer;
use rlive_core::{DeviceId, ParamId, ParamInfo, TrackId};
use rlive_devices::DeviceRole;
use rlive_engine::{mix_cell, CellIx, ChainAddr, GraphOp, TrackRef};
use rlive_plugin_host::PluginDescriptor;
use rlive_project::{Device, DeviceKind};
use std::path::PathBuf;

/// Where a device lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainRef {
    Track(TrackId),
    Master,
    /// A chain inside a rack. Racks reach the project with file format 2, so for now this names
    /// a chain the project holds no devices for.
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
            ChainRef::Nested(_) => None,
        }
    }

    pub(super) fn chain_mut(&mut self, chain: ChainRef) -> Option<&mut Vec<Device>> {
        match chain {
            ChainRef::Track(t) => self.project.track_mut(t).map(|t| &mut t.devices),
            ChainRef::Master => Some(&mut self.project.master.devices),
            ChainRef::Nested(_) => None,
        }
    }

    pub fn devices(&self, chain: ChainRef) -> &[Device] {
        self.chain(chain).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn add_device(&mut self, chain: ChainRef, index: Option<usize>, device: Device) {
        let (mut b, _) = self.builder();
        let (slot, rt) = b.device(&device);
        if let Some(e) = &rt.error {
            self.messages.push(format!("{}: {e}", device.name));
        }
        let id = device.id;
        let Some(devs) = self.chain_mut(chain) else {
            return;
        };
        let index = index.unwrap_or(devs.len()).min(devs.len());
        devs.insert(index, device);
        let swap = self.place_device_cells(chain, index, &rt);
        self.runtimes.insert(id, rt);
        let insert = GraphOp::InsertDevice {
            target: chain.into(),
            index,
            device: slot,
        };
        self.send_edits(swap.into_iter().chain([insert]).collect());
        self.mark_edit("Add Device");
    }

    /// Give the device at `index` cells. Returns the edit that gives the engine a new bus, if
    /// that took one.
    fn place_device_cells(
        &mut self,
        chain: ChainRef,
        index: usize,
        rt: &build::DeviceRuntime,
    ) -> Option<GraphOp> {
        let dev = self.chain(chain)?.get(index)?.clone();
        let layout = self.layouts.get_mut(&chain.owner())?;
        build::place_device(layout, &dev, rt).then(|| GraphOp::SwapBus {
            target: chain.into(),
            bus: layout.build(),
        })
    }

    /// Update a device's cells after its parameter list changed.
    pub(super) fn refit_device_cells(&mut self, chain: ChainRef, id: DeviceId) {
        let Some(index) = self.devices(chain).iter().position(|d| d.id == id) else {
            return;
        };
        let Some(rt) = self.runtimes.remove(&id) else {
            return;
        };
        let swap = self.place_device_cells(chain, index, &rt);
        self.runtimes.insert(id, rt);
        if let Some(swap) = swap {
            self.send_edits(vec![swap]);
        }
    }

    pub fn add_builtin(&mut self, chain: ChainRef, type_id: &str) {
        if let Some(info) = rlive_devices::find(type_id) {
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
        let index = self.instrument_insert_index(chain, role);
        self.add_device(chain, index, d);
    }

    /// MIDI effects go to the front of the chain, instruments after them (and after any other
    /// instrument), audio effects at the end.
    fn instrument_insert_index(&self, chain: ChainRef, role: DeviceRole) -> Option<usize> {
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
                self.park_device(&mut d);
            }
        }
        if let Some(mut rt) = self.runtimes.remove(&id) {
            if let Some(h) = rt.plugin.as_mut() {
                h.close_editor();
            }
            // The handle is dropped now; the processor half comes back as engine garbage.
        }
        if let Some(l) = self.layouts.get_mut(&chain.owner()) {
            l.remove_device(id);
        }
        self.send_edits(vec![GraphOp::RemoveDevice {
            target: chain.into(),
            device: id,
        }]);
        self.mark_edit("Remove Device");
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
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.bypass = bypass;
        }
        let activator = self.layouts.get(&chain.owner());
        if let Some(cell) = activator.and_then(|l| l.activator_cell(id)) {
            self.set_cell(chain, cell, !bypass as u8 as f64, true);
        }
        self.touch();
    }

    pub fn param_value(&self, chain: ChainRef, id: DeviceId, param: &ParamInfo) -> f64 {
        if let Some(v) = self
            .chain(chain)
            .and_then(|v| v.iter().find(|d| d.id == id))
            .and_then(|d| d.params.get(&param.id))
        {
            return *v;
        }
        self.runtimes
            .get(&id)
            .and_then(|r| r.plugin.as_ref())
            .and_then(|h| h.param_value(param.id))
            .unwrap_or(param.default)
    }

    pub fn set_param(&mut self, chain: ChainRef, id: DeviceId, param: u32, value: f64) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.params.insert(param, value);
        }
        if let Some(h) = self.runtimes.get_mut(&id).and_then(|r| r.plugin.as_mut()) {
            h.on_param_set(param, value);
        }
        if let Some(cell) = self.device_cell(chain, id, param) {
            self.set_cell(chain, cell, value, true);
        }
        self.touch();
    }

    /// Records a value a device reported, in the project and the layout.
    pub(super) fn note_device_param(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        param: u32,
        value: f64,
    ) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.params.insert(param, value);
        }
        if let Some(l) = self.layouts.get_mut(&chain.owner()) {
            if let Some(cell) = l.device_cell(id, param) {
                l.note_value(cell, value);
            }
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
        match (chain, control) {
            (_, mixer::VOLUME) => Some(mix_cell::VOLUME),
            (ChainRef::Track(_), mixer::PAN) => Some(mix_cell::PAN),
            (ChainRef::Track(_), mixer::MUTE) => Some(mix_cell::MUTE),
            (ChainRef::Track(_), mixer::SOLO) => Some(mix_cell::SOLO),
            _ => None,
        }
    }

    /// The normalized value a parameter is sounding at, after automation and modulation.
    pub fn effective_norm(&self, chain: ChainRef, device: DeviceId, param: u32) -> Option<f32> {
        let layout = self.layouts.get(&chain.owner())?;
        layout.effective(layout.device_cell(device, param)?)
    }

    /// Change the sample of a built-in sampler. Playing voices carry on with the new sample; a
    /// file that won't load leaves the current one in place and reports why.
    pub fn set_device_sample(&mut self, chain: ChainRef, id: DeviceId, path: PathBuf) {
        let loaded = self.samples.get(&path);
        let error = loaded.as_ref().err().cloned();
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.error = error.clone();
        }
        let Ok(sample) = loaded else {
            self.messages.extend(error);
            return;
        };
        let Some(dev) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        else {
            return;
        };
        if let DeviceKind::Builtin {
            sample: current, ..
        } = &mut dev.kind
        {
            *current = Some(path);
        }
        self.send_edits(vec![GraphOp::SetResource {
            target: chain.into(),
            device: id,
            slot: 0,
            resource: sample,
        }]);
        self.touch();
    }

    /// Re-instantiate a plugin (keeping its state), e.g. after it requested a restart.
    pub fn reload_device(&mut self, chain: ChainRef, id: DeviceId) {
        self.capture_plugin_states();
        let Some(devs) = self.chain(chain) else {
            return;
        };
        let Some(pos) = devs.iter().position(|d| d.id == id) else {
            return;
        };
        let d = devs[pos].clone();
        self.runtimes.remove(&id);
        let (mut b, _) = self.builder();
        let (slot, rt) = b.device(&d);
        let swap = self.place_device_cells(chain, pos, &rt);
        self.runtimes.insert(id, rt);
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

    pub(super) fn chain_of_device(&self, id: DeviceId) -> Option<ChainRef> {
        if self.project.master.devices.iter().any(|d| d.id == id) {
            return Some(ChainRef::Master);
        }
        self.project
            .tracks
            .iter()
            .find(|t| t.devices.iter().any(|d| d.id == id))
            .map(|t| ChainRef::Track(t.id))
    }
}
