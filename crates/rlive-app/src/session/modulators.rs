//! Modulator mappings: which device output drives which parameter, wherever either of them is.
//!
//! Mappings live on the modulator device as stable [`ParamRef`]s. They compile into each target
//! track's routes (`rlive_engine::ModRoute`), which read the source from the engine's `ModBus`
//! at one block of latency, so a mapping never touches the render plan.

use super::devices::mixer_cell_of;
use super::{ChainRef, Session};
use rlive_core::param::{ParamOwner, ParamRef, ACTIVATOR};
use rlive_core::DeviceId;
use rlive_devices::devices::b2::common::{map_outputs, remote_param};
use rlive_engine::{BusLayout, CellIx, ModRoute, TrackRef, MAX_MOD_VALUES};
use rlive_project::{Device, DeviceKind, ModMapping, Project};
use std::collections::{HashMap, HashSet};

/// Where each modulator's values sit on the engine's bus.
#[derive(Debug, Clone, Default)]
pub struct ModSlots {
    bases: HashMap<DeviceId, (u32, u32)>,
    free: Vec<(u32, u32)>,
    next: u32,
}

impl ModSlots {
    /// A place on the bus for a device with `values` values, the one it has if it has one.
    pub fn ensure(&mut self, device: DeviceId, values: u32) -> u32 {
        if let Some(&(base, len)) = self.bases.get(&device) {
            if len == values {
                return base;
            }
        }
        if values == 0 {
            return rlive_engine::NO_MOD_BASE;
        }
        let reused = self.free.iter().position(|&(_, l)| l == values);
        let base = match reused {
            Some(i) => self.free.swap_remove(i).0,
            None if (self.next + values) as usize <= MAX_MOD_VALUES => {
                let base = self.next;
                self.next += values;
                base
            }
            None => return rlive_engine::NO_MOD_BASE,
        };
        self.bases.insert(device, (base, values));
        base
    }

    pub fn base(&self, device: DeviceId) -> Option<u32> {
        self.bases.get(&device).map(|&(b, _)| b)
    }

    /// A device is going away for good: its place can be used again.
    pub fn release(&mut self, device: DeviceId) {
        if let Some(range) = self.bases.remove(&device) {
            self.free.push(range);
        }
    }
}

/// One mapping in the project, addressed to nothing yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModEntry {
    source: DeviceId,
    outputs: u32,
    slot: u32,
    target: ParamRef,
}

/// Every mapping in a project. Cells get worked out per track.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModPlan(Vec<ModEntry>);

impl ModPlan {
    pub fn of(project: &Project) -> Self {
        let mut entries = Vec::new();
        for t in project
            .tracks
            .iter()
            .chain(&project.returns)
            .chain(std::iter::once(&project.master))
        {
            for d in &t.devices {
                d.for_each(&mut |d| {
                    let DeviceKind::Builtin { type_id, .. } = &d.kind else {
                        return;
                    };
                    let outputs = map_outputs(type_id) as u32;
                    for m in &d.modulations {
                        if u32::from(m.slot) < outputs {
                            entries.push(ModEntry {
                                source: d.id,
                                outputs,
                                slot: u32::from(m.slot),
                                target: m.target,
                            });
                        }
                    }
                });
            }
        }
        Self(entries)
    }

    /// What drives the cells of this track. A cell is driven by the first mapping to it, and a
    /// Macro-owned cell by no modulator.
    pub fn routes(
        &self,
        owner: ChainRef,
        layout: &BusLayout,
        slots: &mut ModSlots,
        macros: &HashSet<CellIx>,
    ) -> Vec<ModRoute> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in &self.0 {
            let Some(cell) = target_cell(e.target, owner, layout) else {
                continue;
            };
            if macros.contains(&cell) || !seen.insert(cell) {
                continue;
            }
            let base = slots.ensure(e.source, e.outputs * 2);
            if base == rlive_engine::NO_MOD_BASE {
                continue;
            }
            out.push(ModRoute::new(
                base + e.slot,
                base + e.outputs + e.slot,
                cell,
            ));
        }
        out
    }
}

fn target_cell(target: ParamRef, owner: ChainRef, layout: &BusLayout) -> Option<CellIx> {
    match target.owner {
        ParamOwner::Device(device) if target.id == ACTIVATOR => layout.activator_cell(device),
        ParamOwner::Device(device) => layout.device_cell(device, target.id.0),
        ParamOwner::Chain(chain) => layout.chain_cell(chain, target.id),
        ParamOwner::Track(t) if owner == ChainRef::Track(t) => mixer_cell_of(owner, target.id),
        ParamOwner::Master if owner == ChainRef::Master => mixer_cell_of(owner, target.id),
        _ => None,
    }
}

impl Session {
    /// The device that can be mapped from, and its chain.
    fn modulator_device(&self, source: DeviceId) -> Option<(ChainRef, &Device, usize)> {
        let chain = self.chain_of_device(source)?;
        let d = self.devices(chain).iter().find(|d| d.id == source)?;
        let DeviceKind::Builtin { type_id, .. } = &d.kind else {
            return None;
        };
        Some((chain, d, map_outputs(type_id)))
    }

    /// How many outputs a device has to map from.
    pub fn modulator_outputs(&self, source: DeviceId) -> usize {
        self.modulator_device(source).map_or(0, |(_, _, n)| n)
    }

    /// Where a parameter lives, if it is there.
    fn owner_of_target(&self, target: ParamRef) -> Option<ChainRef> {
        match target.owner {
            ParamOwner::Device(d) => self.chain_of_device(d),
            ParamOwner::Track(t) => self.project.track(t).map(|_| ChainRef::Track(t)),
            ParamOwner::Master => Some(ChainRef::Master),
            ParamOwner::Chain(c) => self
                .project
                .tracks
                .iter()
                .chain(&self.project.returns)
                .find_map(|t| {
                    self.find_chain(ChainRef::Track(t.id), c)
                        .map(|_| ChainRef::Track(t.id))
                }),
            ParamOwner::Transport => None,
        }
    }

    /// Whether a modulator can be pointed at this parameter.
    pub fn can_modulate(&self, target: ParamRef) -> bool {
        let Some(chain) = self.owner_of_target(target) else {
            return false;
        };
        let owner = chain.owner();
        let automatable = self
            .layouts
            .get(&owner)
            .and_then(|l| target_cell(target, owner, l).and_then(|c| l.spec(c)))
            .is_some_and(|s| s.automatable);
        automatable && !self.is_macro_mapped(chain, target)
    }

    /// Points output `slot` of a modulator at a parameter, taking it from any other modulator
    /// that had it (manual p. 844). Refused for something that can't be automated or that a
    /// Macro already owns. Returns whether the mapping was made.
    pub fn map_modulator(&mut self, source: DeviceId, slot: usize, target: ParamRef) -> bool {
        let Some((chain, _, outputs)) = self.modulator_device(source) else {
            return false;
        };
        if slot >= outputs || !self.can_modulate(target) {
            return false;
        }
        for owner in self.all_owners() {
            if let Some(devs) = self.chain_mut(owner) {
                for d in devs.iter_mut() {
                    d.for_each_mut(&mut |d| d.modulations.retain(|m| m.target != target));
                }
            }
        }
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == source))
        else {
            return false;
        };
        d.modulations.retain(|m| usize::from(m.slot) != slot);
        d.modulations.push(ModMapping {
            slot: slot as u8,
            target,
        });
        d.modulations.sort_by_key(|m| m.slot);
        self.sync_mod_routes();
        self.mark_edit("Map Modulator");
        true
    }

    /// Lets go of what output `slot` of a modulator drives.
    pub fn unmap_modulator(&mut self, source: DeviceId, slot: usize) {
        let Some((chain, _, _)) = self.modulator_device(source) else {
            return;
        };
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == source))
        else {
            return;
        };
        let before = d.modulations.len();
        d.modulations.retain(|m| usize::from(m.slot) != slot);
        if d.modulations.len() != before {
            self.sync_mod_routes();
            self.mark_edit("Unmap Modulator");
        }
    }

    /// The parameter output `slot` of a modulator drives.
    pub fn modulator_target(&self, source: DeviceId, slot: usize) -> Option<ParamRef> {
        self.modulator_device(source)
            .and_then(|(_, d, _)| d.modulations.iter().find(|m| usize::from(m.slot) == slot))
            .map(|m| m.target)
    }

    /// What a modulator's mapping is called in its Map button.
    pub fn modulator_target_name(&self, target: ParamRef) -> String {
        self.describe_param(target).1
    }

    /// Whether a Remote Control has this parameter, so the user can't change it (manual p. 844).
    pub fn is_remote_controlled(&self, target: ParamRef) -> bool {
        let matches = |d: &Device| {
            let DeviceKind::Builtin { type_id, .. } = &d.kind else {
                return false;
            };
            d.modulations.iter().any(|m| {
                m.target == target
                    && remote_param(type_id, usize::from(m.slot))
                        .and_then(|p| d.params.get(&p))
                        .is_some_and(|v| *v >= 0.5)
            })
        };
        let mut found = false;
        for owner in self.all_owners() {
            for d in self.devices(owner) {
                d.for_each(&mut |d| found |= matches(d));
            }
        }
        found
    }

    fn all_owners(&self) -> Vec<ChainRef> {
        self.project
            .tracks
            .iter()
            .chain(&self.project.returns)
            .map(|t| ChainRef::Track(t.id))
            .chain(std::iter::once(ChainRef::Master))
            .collect()
    }

    /// Mappings changed, or the devices around them did: every track affected gets new routes.
    pub(super) fn sync_mod_routes(&mut self) {
        self.mods_dirty = false;
        let plan = ModPlan::of(&self.project);
        for owner in self.all_owners() {
            let Some(layout) = self.layouts.get(&owner) else {
                continue;
            };
            let track: TrackRef = owner.into();
            let macros = self.macro_cells(track);
            let routes = plan.routes(owner, layout, &mut self.mod_slots, &macros);
            if self.sent_mods.get(&owner) != Some(&routes) {
                self.sent_mods.insert(owner, routes);
                self.send_routes(track);
            }
        }
        self.mod_plan = plan;
    }

    /// The Map button waiting for a parameter, if one is.
    pub fn armed_mod_map(&self) -> Option<(DeviceId, usize)> {
        self.armed_mod_map
    }

    /// Arms or disarms a Map button.
    pub fn arm_mod_map(&mut self, armed: Option<(DeviceId, usize)>) {
        self.armed_mod_map = armed;
    }

    /// A parameter was clicked while a Map button waited.
    pub fn finish_mod_map(&mut self, target: ParamRef) {
        if let Some((source, slot)) = self.armed_mod_map.take() {
            if !self.map_modulator(source, slot, target) {
                self.messages
                    .push("That parameter can't be mapped to a modulator".to_string());
            }
        }
    }

    /// Every parameter a Remote Control has right now.
    pub fn remote_controlled(&self) -> Vec<ParamRef> {
        let mut out = Vec::new();
        for owner in self.all_owners() {
            for d in self.devices(owner) {
                d.for_each(&mut |d| {
                    let DeviceKind::Builtin { type_id, .. } = &d.kind else {
                        return;
                    };
                    for m in &d.modulations {
                        let remote = remote_param(type_id, usize::from(m.slot))
                            .and_then(|p| d.params.get(&p))
                            .is_some_and(|v| *v >= 0.5);
                        if remote {
                            out.push(m.target);
                        }
                    }
                });
            }
        }
        out
    }

    /// The cells a Macro owns on a track.
    fn macro_cells(&self, track: TrackRef) -> HashSet<CellIx> {
        self.compiled_routes(track)
            .map(|r| r.macros().iter().map(|m| m.target()).collect())
            .unwrap_or_default()
    }
}
