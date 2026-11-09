//! Racks as the session sees them: the rules of each type, grouping, the chain mixer, zones,
//! Drum Rack pads, Macro Controls and their variations (manual ch. 25).
//!
//! Everything here goes through [`Session`], so the project and the engine never drift apart, and
//! every edit lands in the undo history. What the audio thread does with the values is in
//! `rlive_engine::rack`.

use super::{build, ChainRef, Session};
use rlive_core::param::rack::{CHAIN_SELECT, MACRO_COUNT, SCALE};
use rlive_core::param::{chain as controls, ParamId, ParamOwner, ParamRef};
use rlive_core::{ChainId, DeviceId, ParamInfo, TrackId};
use rlive_devices::DeviceRole;
use rlive_engine::{ChainAddr, GraphOp, MacroRoute, MAX_RACK_DEPTH};
use rlive_project::{
    grow_macros, ArrangementClip, Chain, ChainSend, Device, DeviceKind, DrumPad, Macro,
    MacroMapping, RackKind, Track, TrackKind, Variation, Zone, Zones, PALETTE,
};
use std::collections::HashMap;

/// A new Drum Rack chain's notes: C1 and up, the first pads, playing the conventional C3.
pub(super) const FIRST_PAD: u8 = 36;
pub(super) const PLAY_NOTE: u8 = 60;

/// Which zones of a chain an edit is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneKind {
    Key,
    Velocity,
    Select,
}

/// How a macro should be shown: its name, unit and the values it sweeps in that unit.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroDisplay {
    pub name: String,
    pub unit: String,
    pub min: f64,
    pub max: f64,
    /// The macro's value on that scale.
    pub value: f64,
}

/// Whether extracting a Drum Rack chain brings its MIDI notes along (p. 509).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractNotes {
    /// Only the devices, as dragging from the chain list does.
    Without,
    /// The devices and the notes that play the chain, as dragging from the mixer does.
    With,
}

/// Seed the Rand button's numbers from the time.
pub(super) fn random_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64);
    nanos | 1
}

impl Session {
    // ------------------------------------------------------------- finding racks

    pub(super) fn rack(&self, chain: ChainRef, rack: DeviceId) -> Option<&Device> {
        self.devices(chain).iter().find(|d| d.id == rack)
    }

    pub(super) fn rack_mut(&mut self, chain: ChainRef, rack: DeviceId) -> Option<&mut Device> {
        self.chain_mut(chain)?.iter_mut().find(|d| d.id == rack)
    }

    /// The chain or return chain `id` of a rack, for editing.
    pub(super) fn chain_of_rack_mut(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
    ) -> Option<&mut Chain> {
        self.rack_mut(chain, rack)?
            .chains_mut()
            .find(|c| c.id == id)
    }

    // ------------------------------------------------------------- rack rules

    /// Whether a rack of this kind can sit in this chain (manual p. 493).
    fn can_host_rack(&mut self, chain: ChainRef, kind: RackKind, name: &str) -> bool {
        let role = rack_role(kind);
        if !self.accepts_role(chain, role, name) {
            return false;
        }
        let midi_only = matches!(kind, RackKind::Midi | RackKind::Instrument | RackKind::Drum);
        let on_midi_track = match chain.owner() {
            ChainRef::Track(id) => self
                .project
                .track(id)
                .is_some_and(|t| t.kind == TrackKind::Midi),
            _ => false,
        };
        if midi_only && !on_midi_track && chain == chain.owner() {
            self.messages
                .push(format!("{name}: this rack belongs on a MIDI track"));
            return false;
        }
        true
    }

    /// Adds an empty rack of this kind.
    pub fn add_rack(&mut self, chain: ChainRef, kind: RackKind, name: &str) -> Option<DeviceId> {
        if !self.can_host_rack(chain, kind, name) {
            return None;
        }
        let d = Device::rack(kind, name);
        let id = d.id;
        let at = self.instrument_insert_index(chain, rack_role(kind));
        self.add_device(chain, at, d);
        Some(id)
    }

    // ------------------------------------------------------------- group and ungroup

    /// Puts these devices of a chain into a new rack with one chain, in chain order, and returns
    /// the rack. Grouping to a Drum Rack gives that chain a pad (p. 493).
    pub fn group_devices(
        &mut self,
        chain: ChainRef,
        ids: &[DeviceId],
        to_drum_rack: bool,
    ) -> Option<DeviceId> {
        let devices = self.devices(chain);
        let picked: Vec<usize> = devices
            .iter()
            .enumerate()
            .filter(|(_, d)| ids.contains(&d.id))
            .map(|(i, _)| i)
            .collect();
        let first = *picked.first()?;
        let roles: Vec<DeviceRole> = picked
            .iter()
            .filter_map(|&i| self.runtimes.get(&devices[i].id).map(|r| r.role))
            .collect();
        let kind = if to_drum_rack {
            RackKind::Drum
        } else {
            infer_rack_kind(&roles)?
        };
        let nesting = picked
            .iter()
            .map(|&i| nesting_of(&devices[i]))
            .max()
            .unwrap_or(0);
        if chain.depth() + 1 + nesting > MAX_RACK_DEPTH {
            self.messages.push("Racks are nested too deeply".into());
            return None;
        }
        let name = rack_name(kind);
        if !self.can_host_rack(chain, kind, name) {
            return None;
        }
        // The devices themselves stay alive: only their place in the tree changes.
        let moved: Vec<Device> = {
            let devs = self.chain_mut(chain)?;
            let moved = picked
                .iter()
                .rev()
                .map(|&i| devs.remove(i))
                .collect::<Vec<_>>();
            moved.into_iter().rev().collect()
        };
        let mut holder = Chain::new("Chain");
        if kind == RackKind::Drum {
            holder.zones.drum = Some(DrumPad {
                receive: Some(FIRST_PAD),
                play: PLAY_NOTE,
                choke: None,
            });
        }
        let mut rack = Device::rack(kind, name);
        let rack_id = rack.id;
        if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
            chains.push(holder.clone());
        }
        let mut built = HashMap::new();
        let (mut b, _) = self.builder();
        let slot = b.device(&rack, chain.depth(), &mut built);
        self.runtimes.extend(built);

        holder.devices = moved;
        let holder_id = holder.id;
        let moved_ids: Vec<DeviceId> = holder.devices.iter().map(|d| d.id).collect();
        if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
            chains[0] = holder;
        }
        self.chain_mut(chain)?.insert(first, rack);

        let swap = self.place_tree(chain, rack_id);
        let addr: ChainAddr = chain.into();
        let inside = addr.child(rack_id, holder_id)?;
        let mut ops: Vec<GraphOp> = swap.into_iter().collect();
        ops.push(GraphOp::InsertDevice {
            target: addr,
            index: first,
            device: slot,
        });
        for (i, device) in moved_ids.into_iter().enumerate() {
            ops.push(GraphOp::TransferDevice {
                from: addr,
                device,
                to: inside,
                index: i,
            });
        }
        self.send_edits(ops);
        self.push_rack_mix(chain, rack_id);
        self.send_routes(chain.into());
        self.mark_edit("Group");
        Some(rack_id)
    }

    /// Cells for a rack in this chain and everything in it. Returns the edit for a new bus, if
    /// that took one.
    fn place_tree(&mut self, chain: ChainRef, rack: DeviceId) -> Option<GraphOp> {
        let dev = self.devices(chain).iter().find(|d| d.id == rack)?.clone();
        let layout = self.layouts.get_mut(&chain.owner())?;
        build::place_device_tree(layout, &dev, &self.runtimes).then(|| GraphOp::SwapBus {
            target: chain.into(),
            bus: layout.build(),
        })
    }

    /// Dismantles a rack: its chains' devices take its place, in chain order. Return chains go
    /// with it, and so do macros (p. 493). Refused when the devices couldn't sit in one chain.
    pub fn ungroup_rack(&mut self, chain: ChainRef, rack: DeviceId) -> bool {
        let Some(devices) = self.rack(chain, rack).map(|r| {
            let DeviceKind::Rack { chains, .. } = &r.kind else {
                return Vec::new();
            };
            chains
                .iter()
                .flat_map(|c| &c.devices)
                .map(|d| d.id)
                .collect::<Vec<_>>()
        }) else {
            return false;
        };
        let instruments = devices
            .iter()
            .filter(|d| {
                self.runtimes
                    .get(d)
                    .is_some_and(|r| r.role == DeviceRole::Instrument)
            })
            .count();
        if instruments > 1 {
            self.messages
                .push("Can't ungroup a rack with several instruments into one chain".into());
            return false;
        }
        let Some(position) = self.devices(chain).iter().position(|d| d.id == rack) else {
            return false;
        };
        let Some(mut dismantled) = self.chain_mut(chain).map(|v| v.remove(position)) else {
            return false;
        };
        let addr: ChainAddr = chain.into();
        let mut chains_out: Vec<(ChainId, Vec<Device>)> = Vec::new();
        if let DeviceKind::Rack { chains, .. } = &mut dismantled.kind {
            for c in chains.iter_mut() {
                chains_out.push((c.id, std::mem::take(&mut c.devices)));
            }
        }
        let mut at = position;
        let mut ops: Vec<GraphOp> = Vec::new();
        let mut moved = Vec::new();
        for (chain_id, devs) in chains_out {
            let Some(from) = addr.child(rack, chain_id) else {
                continue;
            };
            for d in devs {
                ops.push(GraphOp::TransferDevice {
                    from,
                    device: d.id,
                    to: addr,
                    index: at,
                });
                moved.push(d);
                at += 1;
            }
        }
        if let Some(devs) = self.chain_mut(chain) {
            for (i, d) in moved.into_iter().enumerate() {
                devs.insert(position + i, d);
            }
        }
        self.retire_device_runtimes(chain, &mut dismantled);
        ops.push(GraphOp::RemoveDevice {
            target: addr,
            device: rack,
        });
        self.send_edits(ops);
        self.send_routes(chain.into());
        self.mark_edit("Ungroup");
        true
    }

    /// Groups chains of a rack into a new rack of the same kind, nested in a new chain that takes
    /// their place (p. 493).
    pub fn group_chains(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        ids: &[ChainId],
    ) -> Option<(ChainId, DeviceId)> {
        let kind = match &self.rack(chain, rack)?.kind {
            DeviceKind::Rack { kind, .. } => *kind,
            _ => return None,
        };
        self.group_chains_as(chain, rack, ids, kind)
    }

    /// [`group_chains`](Self::group_chains) into a new rack of this kind.
    pub(super) fn group_chains_as(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        ids: &[ChainId],
        inner_kind: RackKind,
    ) -> Option<(ChainId, DeviceId)> {
        let parent = self.rack(chain, rack)?;
        let DeviceKind::Rack { kind, chains, .. } = &parent.kind else {
            return None;
        };
        let parent_kind = *kind;
        let picked: Vec<usize> = chains
            .iter()
            .enumerate()
            .filter(|(_, c)| ids.contains(&c.id))
            .map(|(i, _)| i)
            .collect();
        let first = *picked.first()?;
        let nesting = picked
            .iter()
            .flat_map(|&i| &chains[i].devices)
            .map(nesting_of)
            .max()
            .unwrap_or(0);
        if chain.depth() + 2 + nesting > MAX_RACK_DEPTH {
            self.messages.push("Racks are nested too deeply".into());
            return None;
        }
        let name = rack_name(inner_kind);
        let moved_ids: Vec<ChainId> = picked.iter().map(|&i| chains[i].id).collect();
        let moved: Vec<Chain> = {
            let DeviceKind::Rack { chains, .. } = &mut self.rack_mut(chain, rack)?.kind else {
                return None;
            };
            let taken = picked
                .iter()
                .rev()
                .map(|&i| chains.remove(i))
                .collect::<Vec<_>>();
            taken.into_iter().rev().collect()
        };

        let mut inner = Device::rack(inner_kind, name);
        let inner_id = inner.id;
        let mut wrapper = Chain::new(name);
        let wrapper_id = wrapper.id;
        if parent_kind == RackKind::Drum {
            wrapper.zones.drum = Some(DrumPad {
                receive: None,
                play: PLAY_NOTE,
                choke: None,
            });
        }
        wrapper.devices.push(inner.clone());
        let mut built = HashMap::new();
        let (mut b, _) = self.builder();
        let node = b.chain(&wrapper, chain.depth() + 1, &mut built);
        self.runtimes.extend(built);

        if let DeviceKind::Rack { chains, .. } = &mut inner.kind {
            *chains = moved;
        }
        wrapper.devices[0] = inner;
        let DeviceKind::Rack { chains, .. } = &mut self.rack_mut(chain, rack)?.kind else {
            return None;
        };
        chains.insert(first.min(chains.len()), wrapper);

        let swap = self.place_tree(chain, rack);
        let addr: ChainAddr = chain.into();
        let to = addr.child(rack, wrapper_id)?;
        let mut ops: Vec<GraphOp> = swap.into_iter().collect();
        ops.push(GraphOp::InsertChain {
            target: addr,
            device: rack,
            index: first,
            chain: node,
        });
        for (i, chain_id) in moved_ids.into_iter().enumerate() {
            ops.push(GraphOp::TransferChain {
                from: addr,
                from_device: rack,
                chain: chain_id,
                to,
                to_device: inner_id,
                index: i,
            });
        }
        self.send_edits(ops);
        self.push_rack_mix(chain, rack);
        self.send_routes(chain.into());
        self.mark_edit("Group Chains");
        Some((wrapper_id, inner_id))
    }

    // ------------------------------------------------------------- chains

    /// Adds a chain. On a Drum Rack it gets the next free pad.
    pub fn add_chain(&mut self, chain: ChainRef, rack: DeviceId, name: &str) -> Option<ChainId> {
        let parent = self.rack(chain, rack)?;
        let DeviceKind::Rack { kind, chains, .. } = &parent.kind else {
            return None;
        };
        let at = chains.len();
        let mut new = Chain::new(name);
        if *kind == RackKind::Drum {
            let used = |key: u8| {
                chains
                    .iter()
                    .any(|c| c.zones.drum.is_some_and(|p| p.receive == Some(key)))
            };
            let receive = (FIRST_PAD..=127).chain(0..FIRST_PAD).find(|k| !used(*k));
            new.zones.drum = Some(DrumPad {
                receive: Some(receive.unwrap_or(FIRST_PAD)),
                play: PLAY_NOTE,
                choke: None,
            });
        }
        let id = new.id;
        self.insert_rack_chain(chain, rack, at, new);
        Some(id)
    }

    /// Deletes a chain or return chain.
    pub fn delete_chain(&mut self, chain: ChainRef, rack: DeviceId, id: ChainId) {
        self.remove_rack_chain(chain, rack, id);
    }

    /// Adds a return chain to a Drum Rack, up to six (p. 493).
    pub fn add_return_chain(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        name: &str,
    ) -> Option<ChainId> {
        let parent = self.rack(chain, rack)?;
        let DeviceKind::Rack { kind, returns, .. } = &parent.kind else {
            return None;
        };
        if *kind != RackKind::Drum || returns.len() >= controls::MAX_RETURNS {
            return None;
        }
        let at = returns.len();
        let new = Chain::new(name);
        let id = new.id;
        self.insert_return_chain(chain, rack, at, new);
        Some(id)
    }

    /// A control of a chain's strip: its value in plain units.
    pub fn chain_param(
        &self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        control: ParamId,
    ) -> Option<f64> {
        let c = self.rack(chain, rack)?.chains().find(|c| c.id == id)?;
        let returns = rack_returns(self.rack(chain, rack)?);
        let values = build::chain_values(c, returns);
        Some(values[controls::control_index(control)?])
    }

    /// Moves a control on a chain's strip: volume, pan, activator, solo or a send. A control a
    /// Macro owns stays where the Macro has it.
    pub fn set_chain_param(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        control: ParamId,
        value: f64,
    ) {
        let target = ParamRef {
            owner: ParamOwner::Chain(id),
            id: control,
        };
        if self.is_macro_mapped(chain, target) {
            return;
        }
        self.write_chain_param(chain, rack, id, control, value, true);
        self.mark_edit("Change Chain");
    }

    pub(super) fn write_chain_param(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        control: ParamId,
        value: f64,
        gesture: bool,
    ) {
        let Some(parent) = self.rack_mut(chain, rack) else {
            return;
        };
        let DeviceKind::Rack {
            chains, returns, ..
        } = &mut parent.kind
        else {
            return;
        };
        let return_ids: Vec<ChainId> = returns.iter().map(|r| r.id).collect();
        let Some(c) = chains
            .iter_mut()
            .chain(returns.iter_mut())
            .find(|c| c.id == id)
        else {
            return;
        };
        match control {
            controls::VOLUME => c.mix.volume_db = value as f32,
            controls::PAN => c.mix.pan = value as f32,
            controls::ACTIVATOR => c.mix.active = value >= 0.5,
            controls::SOLO => c.mix.solo = value >= 0.5,
            ParamId(n) if n >= controls::FIRST_SEND.0 => {
                let k = (n - controls::FIRST_SEND.0) as usize;
                let Some(&to) = return_ids.get(k) else {
                    return;
                };
                match c.mix.sends.iter_mut().find(|s| s.to == to) {
                    Some(s) => s.level_db = value as f32,
                    None => c.mix.sends.push(ChainSend {
                        to,
                        level_db: value as f32,
                    }),
                }
            }
            _ => return,
        }
        if let Some(cell) = self.chain_cell(chain, id, control) {
            self.set_cell(chain, cell, value, gesture);
        }
    }

    /// The cell of a control on a chain's strip, in O(1).
    pub fn chain_cell(
        &self,
        chain: ChainRef,
        id: ChainId,
        control: ParamId,
    ) -> Option<rlive_engine::CellIx> {
        self.layouts.get(&chain.owner())?.chain_cell(id, control)
    }

    /// Sends every chain strip of a rack to the engine as the project has it now. Sends move with
    /// the return chains' positions, so this runs after return chains come or go.
    pub(super) fn push_rack_mix(&mut self, chain: ChainRef, rack: DeviceId) {
        let Some(parent) = self.rack(chain, rack) else {
            return;
        };
        let DeviceKind::Rack {
            chains, returns, ..
        } = &parent.kind
        else {
            return;
        };
        let wanted: Vec<(ChainId, [f64; controls::CONTROLS])> = chains
            .iter()
            .chain(returns.iter())
            .map(|c| (c.id, build::chain_values(c, returns)))
            .collect();
        let infos = controls::infos();
        for (id, values) in wanted {
            for (i, info) in infos.iter().enumerate() {
                let Some(cell) = self.chain_cell(chain, id, info.param_id()) else {
                    continue;
                };
                let known = self.layouts.get(&chain.owner()).and_then(|l| l.base(cell));
                if known != Some(values[i]) {
                    self.set_cell(chain, cell, values[i], false);
                }
            }
        }
    }

    /// Routes a Drum Rack return chain to a return track of the Set, or `None` for the rack's own
    /// output (p. 502).
    pub fn set_return_output(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        ret: ChainId,
        output: Option<TrackId>,
    ) {
        if let Some(target) = output {
            if self.project.return_index(target).is_none() {
                return;
            }
        }
        let Some(c) = self.chain_of_rack_mut(chain, rack, ret) else {
            return;
        };
        c.mix.output = output;
        // Reaching another track is a change of plan.
        self.send_track_edits(vec![GraphOp::SetReturnOutput {
            target: chain.into(),
            device: rack,
            chain: ret,
            output,
        }]);
        self.mark_edit("Change Return Output");
    }

    // ------------------------------------------------------------- zones

    /// Sets all of a chain's zones at once.
    pub fn set_chain_zones(&mut self, chain: ChainRef, rack: DeviceId, id: ChainId, zones: Zones) {
        let zones = Zones {
            key: valid_zone(zones.key),
            velocity: valid_zone(zones.velocity),
            select: valid_zone(zones.select),
            drum: zones.drum.map(valid_pad),
        };
        let Some(c) = self.chain_of_rack_mut(chain, rack, id) else {
            return;
        };
        c.zones = zones;
        self.send_edits(vec![GraphOp::SetChainZones {
            target: chain.into(),
            device: rack,
            chain: id,
            zones: build::engine_zones(&zones),
        }]);
        self.mark_edit("Change Zone");
    }

    /// Sets one of a chain's key, velocity or Chain Select zones.
    pub fn set_zone(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        which: ZoneKind,
        zone: Zone,
    ) {
        let Some(c) = self.chain_of_rack_mut(chain, rack, id) else {
            return;
        };
        let mut zones = c.zones;
        let slot = match which {
            ZoneKind::Key => &mut zones.key,
            ZoneKind::Velocity => &mut zones.velocity,
            ZoneKind::Select => &mut zones.select,
        };
        *slot = valid_zone(zone);
        self.set_chain_zones(chain, rack, id, zones);
    }

    /// A Drum Rack chain's Receive note, Play note and choke group (p. 501).
    pub fn set_drum_pad(&mut self, chain: ChainRef, rack: DeviceId, id: ChainId, pad: DrumPad) {
        let Some(c) = self.chain_of_rack_mut(chain, rack, id) else {
            return;
        };
        let mut zones = c.zones;
        zones.drum = Some(pad);
        self.set_chain_zones(chain, rack, id, zones);
    }

    /// Whether a rack's Chain Selector filters MIDI other than notes as well (p. 499).
    pub fn set_select_filters_ctrl(&mut self, chain: ChainRef, rack: DeviceId, on: bool) {
        let Some(d) = self.rack_mut(chain, rack) else {
            return;
        };
        if let DeviceKind::Rack {
            select_filters_ctrl,
            ..
        } = &mut d.kind
        {
            *select_filters_ctrl = on;
        }
        self.send_edits(vec![GraphOp::SetRackOptions {
            target: chain.into(),
            device: rack,
            select_filters_ctrl: on,
        }]);
        self.mark_edit("Change Rack");
    }

    /// Moves the Chain Selector.
    pub fn set_chain_selector(&mut self, chain: ChainRef, rack: DeviceId, value: f64) {
        self.set_param(chain, rack, CHAIN_SELECT.0, value.clamp(0.0, SCALE));
    }

    // ------------------------------------------------------------- macros

    /// How many Macro Controls a rack shows.
    pub fn visible_macros(&self, chain: ChainRef, rack: DeviceId) -> u8 {
        match self.rack(chain, rack).map(|d| &d.kind) {
            Some(DeviceKind::Rack { visible_macros, .. }) => *visible_macros,
            _ => 0,
        }
    }

    /// Shows this many Macro Controls, 1 to 16 (p. 504). The rest keep their mappings.
    pub fn set_visible_macros(&mut self, chain: ChainRef, rack: DeviceId, n: u8) {
        if let Some(d) = self.rack_mut(chain, rack) {
            if let DeviceKind::Rack { visible_macros, .. } = &mut d.kind {
                *visible_macros = n.clamp(1, MACRO_COUNT as u8);
            }
            self.mark_edit("Change Macro Count");
        }
    }

    /// Moves a Macro Control's knob, 0 to 127.
    pub fn set_macro(&mut self, chain: ChainRef, rack: DeviceId, n: usize, value: f64) {
        self.set_param(chain, rack, n as u32, value.clamp(0.0, SCALE));
    }

    /// A macro's current knob value.
    pub fn macro_value(&self, chain: ChainRef, rack: DeviceId, n: usize) -> f64 {
        self.rack(chain, rack)
            .and_then(|d| d.stored_param(n as u32))
            .unwrap_or(0.0)
    }

    /// Calls a macro something else, or by its own mapping's name again with an empty name.
    pub fn set_macro_name(&mut self, chain: ChainRef, rack: DeviceId, n: usize, name: &str) {
        self.edit_macro(chain, rack, n, "Rename Macro", |m| {
            m.name = name.to_string()
        });
    }

    pub(super) fn edit_macro(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        n: usize,
        label: &'static str,
        f: impl FnOnce(&mut Macro),
    ) -> bool {
        let Some(macros) = self.macros_mut(chain, rack) else {
            return false;
        };
        let Some(m) = macro_slot(macros, n) else {
            return false;
        };
        f(m);
        self.mark_edit(label);
        true
    }

    pub(super) fn macros_mut(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
    ) -> Option<&mut Vec<Macro>> {
        match &mut self.rack_mut(chain, rack)?.kind {
            DeviceKind::Rack { macros, .. } => Some(macros),
            _ => None,
        }
    }

    fn macros(&self, chain: ChainRef, rack: DeviceId) -> Option<&Vec<Macro>> {
        match &self.rack(chain, rack)?.kind {
            DeviceKind::Rack { macros, .. } => Some(macros),
            _ => None,
        }
    }

    /// Maps a macro onto a parameter of something inside its rack, over the parameter's whole
    /// range. A first mapping keeps the parameter where it is by moving the macro to match, and a
    /// parameter can only be owned by one macro. Returns whether the mapping was made.
    pub fn map_macro(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        n: usize,
        target: ParamRef,
    ) -> bool {
        if n >= MACRO_COUNT
            || self.is_macro_mapped(chain, target)
            || self.is_remote_controlled(target)
            || !self.rack_contains(chain, rack, target)
        {
            return false;
        }
        let Some(info) = self.param_info(chain, target) else {
            return false;
        };
        let current = self.param_current(chain, target).unwrap_or(info.default);
        let (min, max) = (info.min, info.max);
        let rack_kind = match self.rack(chain, rack).map(|d| &d.kind) {
            Some(DeviceKind::Rack { kind, .. }) => *kind,
            _ => return false,
        };
        let volume = is_volume(&target, &info);
        let mut adopted = None;
        self.edit_macro(chain, rack, n, "Map Macro", |m| {
            if m.mappings.is_empty() {
                // Nothing jumps: the knob takes up where the parameter already is.
                adopted = Some(info.to_normalized(current) * SCALE);
                if volume && rack_kind == RackKind::Instrument {
                    m.exclude_random = true;
                }
            }
            m.mappings.push(MacroMapping { target, min, max });
        });
        if let Some(value) = adopted {
            self.write_param(chain, rack, n as u32, value, false);
        }
        self.send_routes(chain.into());
        true
    }

    /// Removes one of a macro's mappings, leaving the parameter where the macro had it.
    pub fn unmap_macro(&mut self, chain: ChainRef, rack: DeviceId, n: usize, mapping: usize) {
        let value = self.macro_value(chain, rack, n);
        let Some(macros) = self.macros_mut(chain, rack) else {
            return;
        };
        let Some(m) = macros.get_mut(n) else {
            return;
        };
        if mapping >= m.mappings.len() {
            return;
        }
        let map = m.mappings.remove(mapping);
        if let Some(info) = self.param_info(chain, map.target) {
            let settled = route_value(&info, map, value);
            self.write_target(chain, map.target, settled);
        }
        self.send_routes(chain.into());
        self.mark_edit("Unmap Macro");
    }

    /// Changes a mapping's Min and Max. Min above Max makes an inverted mapping.
    pub fn set_macro_range(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        n: usize,
        mapping: usize,
        min: f64,
        max: f64,
    ) {
        let done = self.edit_macro(chain, rack, n, "Edit Mapping", |m| {
            if let Some(map) = m.mappings.get_mut(mapping) {
                map.min = min;
                map.max = max;
            }
        });
        if done {
            self.send_routes(chain.into());
        }
    }

    /// Swaps a mapping's Min and Max, as Invert Range does.
    pub fn invert_macro_range(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        n: usize,
        mapping: usize,
    ) {
        let done = self.edit_macro(chain, rack, n, "Edit Mapping", |m| {
            if let Some(map) = m.mappings.get_mut(mapping) {
                std::mem::swap(&mut map.min, &mut map.max);
            }
        });
        if done {
            self.send_routes(chain.into());
        }
    }

    /// Whether any macro under this chain's track owns this parameter.
    pub fn is_macro_mapped(&self, chain: ChainRef, target: ParamRef) -> bool {
        any_macro_maps(self.devices(chain.owner()), target)
    }

    /// How a macro is shown: named after its one mapping, or `Macro n` when it has several or
    /// none; in its mappings' units when they all share one, or 0 to 127 (p. 505).
    pub fn macro_display(&self, chain: ChainRef, rack: DeviceId, n: usize) -> MacroDisplay {
        let m = self.macros(chain, rack).and_then(|macros| macros.get(n));
        let generic = format!("Macro {}", n + 1);
        let knob = self.macro_value(chain, rack, n);
        let plain = MacroDisplay {
            name: m
                .filter(|m| !m.name.is_empty())
                .map_or_else(|| generic.clone(), |m| m.name.clone()),
            unit: String::new(),
            min: 0.0,
            max: SCALE,
            value: knob,
        };
        let Some(m) = m.filter(|m| !m.mappings.is_empty()) else {
            return plain;
        };
        let infos: Vec<ParamInfo> = m
            .mappings
            .iter()
            .filter_map(|map| self.param_info(chain, map.target))
            .collect();
        if infos.len() != m.mappings.len() {
            return plain;
        }
        let first = &infos[0];
        let same_units = infos
            .iter()
            .all(|i| i.unit == first.unit && i.min == first.min && i.max == first.max);
        if !same_units {
            return plain;
        }
        let map = m.mappings[0];
        let name = if m.mappings.len() == 1 && m.name.is_empty() {
            first.name.clone()
        } else {
            plain.name
        };
        MacroDisplay {
            name,
            unit: first.unit.clone(),
            min: map.min.min(map.max),
            max: map.min.max(map.max),
            value: route_value(first, map, knob),
        }
    }

    pub fn set_macro_exclude_random(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        n: usize,
        exclude: bool,
    ) {
        self.edit_macro(chain, rack, n, "Change Macro", |m| {
            m.exclude_random = exclude
        });
    }

    pub fn set_macro_exclude_variations(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        n: usize,
        exclude: bool,
    ) {
        self.edit_macro(chain, rack, n, "Change Macro", |m| {
            m.exclude_variations = exclude;
        });
    }

    /// Rand: every mapped macro goes somewhere new, except those excluded (p. 505).
    pub fn randomize_macros(&mut self, chain: ChainRef, rack: DeviceId) {
        let mut state = self.random;
        self.randomize_macros_with(chain, rack, &mut || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        });
        self.random = state;
    }

    /// [`randomize_macros`](Self::randomize_macros) with the numbers given, each 0..1.
    pub(super) fn randomize_macros_with(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        next: &mut dyn FnMut() -> f64,
    ) {
        let eligible: Vec<usize> = self
            .macros(chain, rack)
            .map(|macros| {
                macros
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| !m.mappings.is_empty() && !m.exclude_random)
                    .map(|(n, _)| n)
                    .collect()
            })
            .unwrap_or_default();
        for n in eligible {
            let value = (next() * SCALE).clamp(0.0, SCALE);
            self.write_param(chain, rack, n as u32, value, true);
        }
        self.mark_edit("Randomize Macros");
    }

    // ------------------------------------------------------------- variations

    /// New: stores the macros' current values as a variation, and returns its position.
    pub fn store_variation(&mut self, chain: ChainRef, rack: DeviceId) -> Option<usize> {
        let values: Vec<f64> = self.macros(chain, rack)?.iter().map(|m| m.value).collect();
        let DeviceKind::Rack { variations, .. } = &mut self.rack_mut(chain, rack)?.kind else {
            return None;
        };
        let name = format!("Variation {}", variations.len() + 1);
        variations.push(Variation { name, values });
        let at = variations.len() - 1;
        self.mark_edit("Store Variation");
        Some(at)
    }

    /// Launches a variation, skipping any macro excluded from variations.
    pub fn launch_variation(&mut self, chain: ChainRef, rack: DeviceId, index: usize) {
        let Some(macros) = self.macros(chain, rack) else {
            return;
        };
        let excluded: Vec<bool> = macros.iter().map(|m| m.exclude_variations).collect();
        let values = match &self.rack(chain, rack).map(|d| &d.kind) {
            Some(DeviceKind::Rack { variations, .. }) => variations.get(index),
            _ => None,
        };
        let Some(values) = values.map(|v| v.values.clone()) else {
            return;
        };
        for (n, value) in values.into_iter().enumerate() {
            if excluded.get(n).copied().unwrap_or(false) {
                continue;
            }
            self.write_param(chain, rack, n as u32, value, true);
        }
        self.mark_edit("Launch Variation");
    }

    /// Overwrite: replaces a variation's values with the macros' current ones.
    pub fn overwrite_variation(&mut self, chain: ChainRef, rack: DeviceId, index: usize) {
        let Some(values) = self
            .macros(chain, rack)
            .map(|m| m.iter().map(|m| m.value).collect::<Vec<f64>>())
        else {
            return;
        };
        self.edit_variations(chain, rack, "Overwrite Variation", |list| {
            if let Some(v) = list.get_mut(index) {
                v.values = values;
            }
        });
    }

    pub fn rename_variation(&mut self, chain: ChainRef, rack: DeviceId, index: usize, name: &str) {
        self.edit_variations(chain, rack, "Rename Variation", |list| {
            if let Some(v) = list.get_mut(index) {
                v.name = name.to_string();
            }
        });
    }

    /// Duplicates a variation next to the original.
    pub fn duplicate_variation(&mut self, chain: ChainRef, rack: DeviceId, index: usize) {
        self.edit_variations(chain, rack, "Duplicate Variation", |list| {
            if let Some(v) = list.get(index).cloned() {
                list.insert(index + 1, v);
            }
        });
    }

    pub fn delete_variation(&mut self, chain: ChainRef, rack: DeviceId, index: usize) {
        self.edit_variations(chain, rack, "Delete Variation", |list| {
            if index < list.len() {
                list.remove(index);
            }
        });
    }

    fn edit_variations(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        label: &'static str,
        f: impl FnOnce(&mut Vec<Variation>),
    ) {
        if let Some(DeviceKind::Rack { variations, .. }) =
            self.rack_mut(chain, rack).map(|d| &mut d.kind)
        {
            f(variations);
            self.mark_edit(label);
        }
    }

    // ------------------------------------------------------------- extracting

    /// Takes a chain out of its rack and puts its devices on a new track after this one (p. 509).
    pub fn extract_chain_to_track(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        notes: ExtractNotes,
    ) -> Option<TrackId> {
        self.capture_plugin_states();
        let parent = self.rack(chain, rack)?;
        let DeviceKind::Rack { kind, chains, .. } = &parent.kind else {
            return None;
        };
        let extracted = chains.iter().find(|c| c.id == id)?.clone();
        let is_drum = *kind == RackKind::Drum;
        self.mark_edit("Extract Chain");
        self.remove_rack_chain(chain, rack, id);

        let source = match chain.owner() {
            ChainRef::Track(t) => t,
            _ => return self.place_extracted(extracted),
        };
        let carries = is_drum && notes == ExtractNotes::With;
        let moved = carries
            .then(|| {
                extracted
                    .zones
                    .drum
                    .and_then(|p| Some((p.receive?, p.play)))
            })
            .flatten()
            .map(|(receive, play)| self.take_notes(source, receive, play))
            .unwrap_or_default();
        let at = self.project.track_index(source).map_or(0, |i| i + 1);
        let color = self.colors.track_color(&self.project);
        let mut t = Track::new(
            &extracted.name,
            TrackKind::Midi,
            color,
            self.project.scenes.len(),
        );
        t.devices = extracted.devices;
        let (slots, lane) = moved;
        for (slot, clip) in slots {
            if let Some(s) = t.clips.get_mut(slot) {
                *s = Some(clip);
            }
        }
        t.arrangement = lane;
        let new_id = t.id;
        self.install_track(t, at);
        Some(new_id)
    }

    fn place_extracted(&mut self, extracted: Chain) -> Option<TrackId> {
        let color = self.colors.track_color(&self.project);
        let mut t = Track::new(
            &extracted.name,
            TrackKind::Midi,
            color,
            self.project.scenes.len(),
        );
        t.devices = extracted.devices;
        let id = t.id;
        let at = self.project.tracks.len();
        self.install_track(t, at);
        Some(id)
    }

    /// Moves the notes on `receive` out of a track's clips, into clips of the same shape playing
    /// `play`, ready for a new track. The source clips lose them.
    fn take_notes(
        &mut self,
        track: TrackId,
        receive: u8,
        play: u8,
    ) -> (Vec<(usize, rlive_project::Clip)>, Vec<ArrangementClip>) {
        let Some(t) = self.project.track_mut(track) else {
            return (Vec::new(), Vec::new());
        };
        let mut slots = Vec::new();
        let mut changed = Vec::new();
        for (slot, clip) in t.clips.iter_mut().enumerate() {
            let Some(clip) = clip.as_mut() else {
                continue;
            };
            if let Some(extracted) = split_notes(clip, receive, play) {
                slots.push((slot, extracted));
                changed.push(slot);
            }
        }
        let mut lane = Vec::new();
        let mut lane_changed = false;
        for placed in t.arrangement.iter_mut() {
            if let Some(extracted) = split_notes(&mut placed.clip, receive, play) {
                let mut copy = placed.clone();
                copy.id = rlive_core::ArrClipId::new();
                copy.clip = extracted;
                lane.push(copy);
                lane_changed = true;
            }
        }
        for slot in changed {
            self.sync_clip(track, slot);
        }
        if lane_changed {
            self.sync_lane(track);
        }
        (slots, lane)
    }

    /// Makes a Drum Rack return chain a return track of the Set (p. 509).
    pub fn extract_return_to_track(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
    ) -> Option<TrackId> {
        self.capture_plugin_states();
        let parent = self.rack(chain, rack)?;
        let DeviceKind::Rack { returns, .. } = &parent.kind else {
            return None;
        };
        let extracted = returns.iter().find(|c| c.id == id)?.clone();
        if self.project.returns.len() >= rlive_engine::MAX_SENDS {
            self.messages
                .push("A Set can't have more return tracks".into());
            return None;
        }
        self.mark_edit("Extract Return Chain");
        self.remove_rack_chain(chain, rack, id);
        let at = self.project.returns.len();
        let color = PALETTE[(at * 3 + 2) % PALETTE.len()];
        let mut t = Track::new(&extracted.name, TrackKind::Return, color, 0);
        t.devices = extracted.devices;
        let new_id = t.id;
        self.install_return(t, at);
        self.push_all_sends();
        Some(new_id)
    }

    /// Moves a chain into another rack, on this track or another one (p. 509). Keeps playing when
    /// it stays on its track.
    pub fn move_chain(
        &mut self,
        from: ChainRef,
        from_rack: DeviceId,
        id: ChainId,
        to: ChainRef,
        to_rack: DeviceId,
        index: usize,
    ) -> bool {
        let same_owner = from.owner() == to.owner();
        let Some(chain) = self.take_chain_if_valid(from, from_rack, id, to, to_rack) else {
            return false;
        };
        self.mark_edit("Move Chain");
        if same_owner {
            let DeviceKind::Rack { chains, .. } = &mut self
                .rack_mut(to, to_rack)
                .expect("checked by take_chain_if_valid")
                .kind
            else {
                unreachable!();
            };
            let at = index.min(chains.len());
            chains.insert(at, chain);
            self.send_edits(vec![GraphOp::TransferChain {
                from: from.into(),
                from_device: from_rack,
                chain: id,
                to: to.into(),
                to_device: to_rack,
                index: at,
            }]);
        } else {
            self.insert_rack_chain(to, to_rack, index, chain);
        }
        self.send_routes(from.into());
        self.send_routes(to.into());
        true
    }

    /// Takes a chain out of the project for a move, or refuses when the move can't be made.
    fn take_chain_if_valid(
        &mut self,
        from: ChainRef,
        from_rack: DeviceId,
        id: ChainId,
        to: ChainRef,
        to_rack: DeviceId,
    ) -> Option<Chain> {
        let target_kind = match self.rack(to, to_rack).map(|d| &d.kind) {
            Some(DeviceKind::Rack { kind, .. }) => *kind,
            _ => return None,
        };
        let parent = self.rack(from, from_rack)?;
        let DeviceKind::Rack { chains, .. } = &parent.kind else {
            return None;
        };
        let chain = chains.iter().find(|c| c.id == id)?.clone();
        if !self.chain_fits_kind(&chain, target_kind) {
            return None;
        }
        if chain_is_in(&chain, to) {
            return None;
        }
        // Across tracks the chain is rebuilt from the project; on one track it moves.
        if from.owner() != to.owner() {
            self.capture_plugin_states();
            self.remove_rack_chain(from, from_rack, id);
        } else {
            let list = self.rack_chains_mut(from, from_rack)?;
            let i = list.iter().position(|c| c.id == id)?;
            list.remove(i);
        }
        let mut chain = chain;
        if target_kind == RackKind::Drum && chain.zones.drum.is_none() {
            chain.zones.drum = Some(DrumPad {
                receive: Some(FIRST_PAD),
                play: PLAY_NOTE,
                choke: None,
            });
        }
        Some(chain)
    }

    fn chain_fits_kind(&mut self, chain: &Chain, kind: RackKind) -> bool {
        let roles: Vec<DeviceRole> = chain
            .devices
            .iter()
            .filter_map(|d| self.runtimes.get(&d.id).map(|r| r.role))
            .collect();
        match kind {
            RackKind::Midi if roles.iter().any(|r| *r != DeviceRole::MidiEffect) => {
                self.messages
                    .push("A MIDI Effect Rack takes MIDI effects only".into());
                false
            }
            RackKind::Audio if roles.iter().any(|r| *r != DeviceRole::AudioEffect) => {
                self.messages
                    .push("An Audio Effect Rack takes audio effects only".into());
                false
            }
            _ => true,
        }
    }

    // ------------------------------------------------------------- what macros can reach

    fn rack_contains(&self, chain: ChainRef, rack: DeviceId, target: ParamRef) -> bool {
        let Some(parent) = self.rack(chain, rack) else {
            return false;
        };
        let mut found = false;
        parent.for_each(&mut |d| match target.owner {
            ParamOwner::Device(id) if id == d.id && id != rack => found = true,
            ParamOwner::Chain(id) if d.chains().any(|c| c.id == id) => found = true,
            _ => {}
        });
        found
    }

    pub(crate) fn param_info(&self, _chain: ChainRef, target: ParamRef) -> Option<ParamInfo> {
        match target.owner {
            ParamOwner::Device(id) => {
                if target.id == rlive_core::ACTIVATOR {
                    return Some(ParamInfo::toggle(target.id.0, "Device On", true));
                }
                self.runtimes
                    .get(&id)?
                    .params
                    .iter()
                    .find(|p| p.id == target.id.0)
                    .cloned()
            }
            ParamOwner::Chain(_) => controls::infos()
                .into_iter()
                .find(|p| p.param_id() == target.id),
            _ => None,
        }
    }

    fn param_current(&self, chain: ChainRef, target: ParamRef) -> Option<f64> {
        let info = self.param_info(chain, target)?;
        match target.owner {
            ParamOwner::Device(id) => {
                let here = self.chain_of_device(id)?;
                if target.id == rlive_core::ACTIVATOR {
                    let on = self.devices(here).iter().find(|d| d.id == id)?.bypass;
                    return Some(!on as u8 as f64);
                }
                Some(self.param_value(here, id, &info))
            }
            ParamOwner::Chain(id) => {
                let (at, rack) = self.find_chain(chain.owner(), id)?;
                self.chain_param(at, rack, id, target.id)
            }
            _ => None,
        }
    }

    /// Writes a value through to whichever parameter a macro was driving.
    fn write_target(&mut self, chain: ChainRef, target: ParamRef, value: f64) {
        match target.owner {
            ParamOwner::Device(id) => {
                let Some(here) = self.chain_of_device(id) else {
                    return;
                };
                if target.id == rlive_core::ACTIVATOR {
                    self.write_bypass(here, id, value < 0.5, false);
                } else {
                    self.write_param(here, id, target.id.0, value, false);
                }
            }
            ParamOwner::Chain(id) => {
                let Some((at, rack)) = self.find_chain(chain.owner(), id) else {
                    return;
                };
                self.write_chain_param(at, rack, id, target.id, value, false);
            }
            _ => {}
        }
    }

    /// Where a rack chain is: the device chain holding its rack, and the rack.
    pub(super) fn find_chain(&self, owner: ChainRef, id: ChainId) -> Option<(ChainRef, DeviceId)> {
        fn walk(devices: &[Device], here: ChainRef, id: ChainId) -> Option<(ChainRef, DeviceId)> {
            for d in devices {
                if d.chains().any(|c| c.id == id) {
                    return Some((here, d.id));
                }
                for c in d.chains() {
                    let child = here.child(d.id, c.id)?;
                    if let Some(found) = walk(&c.devices, child, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(self.devices(owner), owner, id)
    }

    /// Forgets macro mappings onto things leaving the project.
    pub(super) fn forget_macro_targets(
        &mut self,
        chain: ChainRef,
        devices: &[DeviceId],
        chains: &[ChainId],
    ) {
        fn walk(list: &mut [Device], devices: &[DeviceId], chains: &[ChainId]) -> bool {
            let mut changed = false;
            for d in list.iter_mut() {
                if let DeviceKind::Rack { macros, .. } = &mut d.kind {
                    for m in macros.iter_mut() {
                        let before = m.mappings.len();
                        m.mappings.retain(|map| match map.target.owner {
                            ParamOwner::Device(id) => !devices.contains(&id),
                            ParamOwner::Chain(id) => !chains.contains(&id),
                            _ => true,
                        });
                        changed |= m.mappings.len() != before;
                    }
                }
                for c in d.chains_mut() {
                    changed |= walk(&mut c.devices, devices, chains);
                }
            }
            changed
        }
        if let Some(list) = self.chain_mut(chain.owner()) {
            if walk(list, devices, chains) {
                self.send_routes(chain.into());
            }
        }
    }
}

/// The role a rack of this kind plays in a device chain.
fn rack_role(kind: RackKind) -> DeviceRole {
    match kind {
        RackKind::Midi => DeviceRole::MidiEffect,
        RackKind::Audio => DeviceRole::AudioEffect,
        RackKind::Instrument | RackKind::Drum => DeviceRole::Instrument,
    }
}

fn rack_name(kind: RackKind) -> &'static str {
    match kind {
        RackKind::Midi => "MIDI Effect Rack",
        RackKind::Audio => "Audio Effect Rack",
        RackKind::Instrument => "Instrument Rack",
        RackKind::Drum => "Drum Rack",
    }
}

/// The rack that can hold these devices. All MIDI effects: a MIDI Effect Rack; all audio effects:
/// an Audio Effect Rack; an instrument (after any MIDI effects): an Instrument Rack (p. 493).
fn infer_rack_kind(roles: &[DeviceRole]) -> Option<RackKind> {
    if roles.iter().all(|r| *r == DeviceRole::MidiEffect) {
        return Some(RackKind::Midi);
    }
    if roles.iter().all(|r| *r == DeviceRole::AudioEffect) {
        return Some(RackKind::Audio);
    }
    roles
        .contains(&DeviceRole::Instrument)
        .then_some(RackKind::Instrument)
}

/// How deep this device's racks go.
fn nesting_of(d: &Device) -> usize {
    match &d.kind {
        DeviceKind::Rack { .. } => {
            1 + d
                .chains()
                .flat_map(|c| &c.devices)
                .map(nesting_of)
                .max()
                .unwrap_or(0)
        }
        _ => 0,
    }
}

pub(super) fn macro_slot(macros: &mut Vec<Macro>, n: usize) -> Option<&mut Macro> {
    grow_macros(macros, n + 1);
    macros.get_mut(n)
}

fn rack_returns(d: &Device) -> &[Chain] {
    match &d.kind {
        DeviceKind::Rack { returns, .. } => returns,
        _ => &[],
    }
}

fn valid_zone(z: Zone) -> Zone {
    let (low, high) = (z.low.min(127), z.high.min(127));
    let (low, high) = (low.min(high), low.max(high));
    Zone {
        low,
        high,
        fade_low: z.fade_low.min(low),
        fade_high: z.fade_high.clamp(high, 127),
    }
}

fn valid_pad(p: DrumPad) -> DrumPad {
    DrumPad {
        receive: p.receive.map(|k| k.min(127)),
        play: p.play.min(127),
        choke: p.choke.filter(|g| (1..=16).contains(g)),
    }
}

fn any_macro_maps(devices: &[Device], target: ParamRef) -> bool {
    devices.iter().any(|d| {
        let here = match &d.kind {
            DeviceKind::Rack { macros, .. } => macros
                .iter()
                .any(|m| m.mappings.iter().any(|map| map.target == target)),
            _ => false,
        };
        here || d.chains().any(|c| any_macro_maps(&c.devices, target))
    })
}

/// A Volume parameter, for the Rand button's default exclusion (p. 506).
fn is_volume(target: &ParamRef, info: &ParamInfo) -> bool {
    matches!(target.owner, ParamOwner::Chain(_)) && target.id == controls::VOLUME
        || info.name.eq_ignore_ascii_case("volume")
        || info.name.eq_ignore_ascii_case("gain") && info.unit == "dB"
}

/// What a macro at `knob` makes this mapping's target.
fn route_value(info: &ParamInfo, map: MacroMapping, knob: f64) -> f64 {
    MacroRoute::new(
        rlive_engine::CellIx(0),
        rlive_engine::CellIx(0),
        info.range(),
        map.min,
        map.max,
    )
    .target_value(knob)
}

/// Whether the device chain `to` is inside `chain`, which a move into it would make impossible.
fn chain_is_in(chain: &Chain, to: ChainRef) -> bool {
    let addr: ChainAddr = to.into();
    addr.path.steps().iter().any(|s| s.chain == chain.id)
}

/// Removes the notes on `receive` from a clip. Returns a copy holding them on `play`, or `None`
/// if it has none (or holds no notes at all).
fn split_notes(
    clip: &mut rlive_project::Clip,
    receive: u8,
    play: u8,
) -> Option<rlive_project::Clip> {
    let notes = clip.notes_mut()?;
    let moved: Vec<_> = notes.iter().filter(|n| n.key == receive).cloned().collect();
    if moved.is_empty() {
        return None;
    }
    notes.retain(|n| n.key != receive);
    let mut copy = clip.clone();
    if let Some(list) = copy.notes_mut() {
        *list = moved
            .into_iter()
            .map(|mut n| {
                n.key = play;
                n
            })
            .collect();
    }
    Some(copy)
}
