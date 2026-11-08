//! Device chains: adding, removing and changing devices.

use super::Session;
use rlive_core::{DeviceId, ParamInfo, TrackId};
use rlive_devices::DeviceRole;
use rlive_engine::{ChainTarget, EngineCommand, GraphCmd, ParamCmd};
use rlive_plugin_host::PluginDescriptor;
use rlive_project::{Device, DeviceKind};
use std::path::PathBuf;

/// Where a device lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainRef {
    Track(TrackId),
    Master,
}

impl From<ChainRef> for ChainTarget {
    fn from(c: ChainRef) -> Self {
        match c {
            ChainRef::Track(t) => ChainTarget::Track(t),
            ChainRef::Master => ChainTarget::Master,
        }
    }
}

impl Session {
    // ------------------------------------------------------------- devices

    fn chain(&self, chain: ChainRef) -> Option<&Vec<Device>> {
        match chain {
            ChainRef::Track(t) => self.project.track(t).map(|t| &t.devices),
            ChainRef::Master => Some(&self.project.master.devices),
        }
    }

    pub(super) fn chain_mut(&mut self, chain: ChainRef) -> Option<&mut Vec<Device>> {
        match chain {
            ChainRef::Track(t) => self.project.track_mut(t).map(|t| &mut t.devices),
            ChainRef::Master => Some(&mut self.project.master.devices),
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
        self.runtimes.insert(id, rt);
        self.send(EngineCommand::Graph(GraphCmd::InsertDevice {
            target: chain.into(),
            index,
            device: slot,
        }));
        self.mark_edit("Add Device");
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

    /// Instruments go to the front of the chain (after any other instrument), effects at the end.
    fn instrument_insert_index(&self, chain: ChainRef, role: DeviceRole) -> Option<usize> {
        if role != DeviceRole::Instrument {
            return None;
        }
        let devs = self.devices(chain);
        Some(
            devs.iter()
                .take_while(|d| {
                    self.runtimes
                        .get(&d.id)
                        .is_some_and(|r| r.role == DeviceRole::Instrument)
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
        self.send(EngineCommand::Graph(GraphCmd::RemoveDevice {
            target: chain.into(),
            device: id,
        }));
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
        self.send(EngineCommand::Graph(GraphCmd::MoveDevice {
            target: chain.into(),
            device: id,
            to,
        }));
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
        self.send(EngineCommand::Param(ParamCmd::SetBypass {
            target: chain.into(),
            device: id,
            bypass,
        }));
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
        self.send(EngineCommand::Param(ParamCmd::SetParam {
            target: chain.into(),
            device: id,
            param,
            value,
        }));
        self.touch();
    }

    /// Change the sample of a built-in sampler (re-creates the device).
    pub fn set_device_sample(&mut self, chain: ChainRef, id: DeviceId, path: PathBuf) {
        let Some(devs) = self.chain_mut(chain) else {
            return;
        };
        let Some(pos) = devs.iter().position(|d| d.id == id) else {
            return;
        };
        if let DeviceKind::Builtin { sample, .. } = &mut devs[pos].kind {
            *sample = Some(path);
        }
        let d = devs[pos].clone();
        // Replace: remove old, insert new with same id at the same position.
        self.send(EngineCommand::Graph(GraphCmd::RemoveDevice {
            target: chain.into(),
            device: id,
        }));
        let (mut b, _) = self.builder();
        let (slot, rt) = b.device(&d);
        if let Some(e) = &rt.error {
            self.messages.push(e.clone());
        }
        self.runtimes.insert(id, rt);
        self.send(EngineCommand::Graph(GraphCmd::InsertDevice {
            target: chain.into(),
            index: pos,
            device: slot,
        }));
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
        self.send(EngineCommand::Graph(GraphCmd::RemoveDevice {
            target: chain.into(),
            device: id,
        }));
        let (mut b, _) = self.builder();
        let (slot, rt) = b.device(&d);
        self.runtimes.insert(id, rt);
        self.send(EngineCommand::Graph(GraphCmd::InsertDevice {
            target: chain.into(),
            index: pos,
            device: slot,
        }));
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
