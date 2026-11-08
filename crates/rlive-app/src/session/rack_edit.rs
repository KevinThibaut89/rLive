//! Rack edits made from the Rack UI, beyond the rules in [`racks`](super::racks): names, colours,
//! Preview, duplicating chains, and what the Drum Rack's Pad View does (manual pp. 501-503).

use super::racks::{FIRST_PAD, PLAY_NOTE};
use super::{ChainRef, Session};
use rlive_core::param::rack::CHAIN_SELECT;
use rlive_core::{ChainId, DeviceId, ParamOwner};
use rlive_devices::DeviceRole;
use rlive_engine::shared::ChainShared;
use rlive_engine::{GraphOp, PreviewNote};
use rlive_plugin_host::PluginDescriptor;
use rlive_project::{Chain, Device, DeviceKind, DrumPad, RackKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The device a dropped sample is loaded into (p. 502).
const PAD_SAMPLER: &str = "rlive.simpler";
const PAD_SAMPLER_NAME: &str = "Simpler";
/// Pads built before Simpler existed hold a Sampler. Their source is kept when a sample lands.
const EARLIER_PAD_SAMPLER: &str = "rlive.sampler";

/// Something dropped on a pad.
#[derive(Debug, Clone)]
pub enum PadDrop {
    Builtin(String),
    Plugin(PluginDescriptor),
    Sample(PathBuf),
}

impl Session {
    /// What a chain publishes for the UI.
    pub fn chain_shared(&self, id: ChainId) -> Option<&Arc<ChainShared>> {
        self.chain_shared.get(&id)
    }

    /// The normalized value a control on a chain's strip is sounding at, Macros included.
    pub fn chain_effective_norm(
        &self,
        chain: ChainRef,
        id: ChainId,
        control: rlive_core::ParamId,
    ) -> Option<f32> {
        let layout = self.layouts.get(&chain.owner())?;
        layout.effective(layout.chain_cell(id, control)?)
    }

    /// Renames a device or a rack.
    pub fn rename_device(&mut self, chain: ChainRef, id: DeviceId, name: &str) {
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        else {
            return;
        };
        d.name = name.to_string();
        self.mark_edit("Rename Device");
    }

    pub fn rename_chain(&mut self, chain: ChainRef, rack: DeviceId, id: ChainId, name: &str) {
        if let Some(c) = self.chain_of_rack_mut(chain, rack, id) {
            c.name = name.to_string();
            self.mark_edit("Rename Chain");
        }
    }

    pub fn set_chain_color(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        color: Option<[u8; 3]>,
    ) {
        if let Some(c) = self.chain_of_rack_mut(chain, rack, id) {
            c.color = color;
            self.mark_edit("Change Chain Color");
        }
    }

    pub fn set_macro_color(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        n: usize,
        color: Option<[u8; 3]>,
    ) {
        self.edit_macro(chain, rack, n, "Change Macro Color", |m| m.color = color);
    }

    /// Where the Chain Selector is, 0 to 127.
    pub fn chain_selector(&self, chain: ChainRef, rack: DeviceId) -> f64 {
        self.rack(chain, rack)
            .and_then(|d| d.stored_param(CHAIN_SELECT.0))
            .unwrap_or(0.0)
    }

    /// Fires a note into a rack: into one chain, or for `None` into every chain that answers it,
    /// as a pad does. Starts no undo step: it changes nothing in the Set.
    pub fn preview_note(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        target: Option<ChainId>,
        key: u8,
        velocity: f32,
        on: bool,
    ) {
        self.send_ops(vec![GraphOp::PreviewNote {
            target: chain.into(),
            device: rack,
            note: PreviewNote {
                chain: target,
                key,
                velocity,
                on,
            },
        }]);
    }

    /// Copies a chain next to the original, and returns the copy.
    pub fn duplicate_chain(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
    ) -> Option<ChainId> {
        self.capture_plugin_states();
        let DeviceKind::Rack { chains, .. } = &self.rack(chain, rack)?.kind else {
            return None;
        };
        let at = chains.iter().position(|c| c.id == id)?;
        let copy = fresh_chain(&chains[at]);
        let new = copy.id;
        self.insert_rack_chain(chain, rack, at + 1, copy);
        self.mark_edit("Duplicate Chain");
        Some(new)
    }

    // ------------------------------------------------------------- the Pad View

    /// The chains of a Drum Rack that answer this note, top level only.
    pub fn chains_on_pad(&self, chain: ChainRef, rack: DeviceId, note: u8) -> Vec<ChainId> {
        match self.rack(chain, rack).map(|d| &d.kind) {
            Some(DeviceKind::Rack { chains, .. }) => chains
                .iter()
                .filter(|c| c.zones.drum.is_some_and(|p| p.receive == Some(note)))
                .map(|c| c.id)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn pad_of(&self, chain: ChainRef, rack: DeviceId, id: ChainId) -> Option<DrumPad> {
        self.rack(chain, rack)?
            .chains()
            .find(|c| c.id == id)?
            .zones
            .drum
    }

    /// A new chain playing from this pad.
    pub(super) fn new_pad_chain(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        note: u8,
        name: &str,
    ) -> Option<ChainId> {
        let id = self.add_chain(chain, rack, name)?;
        self.set_drum_pad(
            chain,
            rack,
            id,
            DrumPad {
                receive: Some(note),
                play: PLAY_NOTE,
                choke: None,
            },
        );
        Some(id)
    }

    /// Puts something from the browser on a pad, creating or reconfiguring its chain as needed
    /// (p. 502).
    pub fn drop_on_pad(&mut self, chain: ChainRef, rack: DeviceId, note: u8, item: PadDrop) {
        match item {
            PadDrop::Sample(path) => self.drop_sample_on_pad(chain, rack, note, &path),
            PadDrop::Builtin(type_id) => {
                let Some(info) = rlive_devices::find(&type_id) else {
                    return;
                };
                let device = Device::builtin(info.id, info.name);
                self.drop_device_on_pad(chain, rack, note, device, info.role);
            }
            PadDrop::Plugin(p) => {
                let role = if p.is_instrument {
                    DeviceRole::Instrument
                } else {
                    DeviceRole::AudioEffect
                };
                let device = Device::plugin(p.format, &p.id, p.path.clone(), &p.name);
                self.drop_device_on_pad(chain, rack, note, device, role);
            }
        }
        self.mark_edit("Drop on Pad");
    }

    fn drop_device_on_pad(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        note: u8,
        device: Device,
        role: DeviceRole,
    ) {
        let existing = self.chains_on_pad(chain, rack, note).first().copied();
        let Some(id) =
            existing.or_else(|| self.new_pad_chain(chain, rack, note, &device.name.clone()))
        else {
            return;
        };
        let Some(at) = chain.child(rack, id) else {
            return;
        };
        if !self.accepts_role(at, role, &device.name) {
            return;
        }
        if role == DeviceRole::Instrument {
            self.replace_source(at, device);
        } else {
            let index = self.instrument_insert_index(at, role);
            self.add_device(at, index, device);
        }
    }

    /// Swaps a chain's instrument for this one, leaving its effects where they are.
    fn replace_source(&mut self, at: ChainRef, device: Device) {
        match self.source_of(at) {
            Some((index, old)) => {
                self.remove_device(at, old);
                self.add_device(at, Some(index), device);
            }
            None => {
                let index = self.instrument_insert_index(at, DeviceRole::Instrument);
                self.add_device(at, index, device);
            }
        }
    }

    /// The instrument of a chain, and where it sits.
    fn source_of(&self, at: ChainRef) -> Option<(usize, DeviceId)> {
        self.devices(at)
            .iter()
            .enumerate()
            .find(|(_, d)| {
                self.runtimes
                    .get(&d.id)
                    .is_some_and(|r| r.role == DeviceRole::Instrument)
            })
            .map(|(i, d)| (i, d.id))
    }

    /// A sample on a pad: a new chain with a new sampler on an empty pad; on an occupied one only
    /// the sound source changes, and the effects around it stay (p. 502).
    fn drop_sample_on_pad(&mut self, chain: ChainRef, rack: DeviceId, note: u8, path: &Path) {
        let existing = self.chains_on_pad(chain, rack, note).first().copied();
        let Some(id) = existing.or_else(|| self.new_pad_chain(chain, rack, note, &stem(path)))
        else {
            return;
        };
        let Some(at) = chain.child(rack, id) else {
            return;
        };
        let reusable = self.source_of(at).and_then(|(_, source)| {
            self.devices(at)
                .iter()
                .find(|d| d.id == source && is_sampler(d))
                .map(|d| d.id)
        });
        let sampler = match reusable {
            Some(id) => id,
            None => {
                let device = Device::builtin(PAD_SAMPLER, PAD_SAMPLER_NAME);
                let id = device.id;
                self.replace_source(at, device);
                id
            }
        };
        self.set_device_sample(at, sampler, path.to_path_buf());
        if existing.is_some() {
            self.rename_chain(chain, rack, id, &stem(path));
        }
    }

    /// Several samples on a pad: mapped upwards chromatically from it, or with `layer` all on
    /// this one pad in a nested Instrument Rack (p. 503).
    pub fn drop_samples_on_pad(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        note: u8,
        paths: &[PathBuf],
        layer: bool,
    ) {
        if layer && paths.len() > 1 {
            self.layer_samples(chain, rack, note, paths);
        } else {
            for (i, path) in paths.iter().enumerate() {
                let Some(key) = note.checked_add(i as u8).filter(|k| *k <= 127) else {
                    break;
                };
                self.drop_sample_on_pad(chain, rack, key, path);
            }
        }
        self.mark_edit("Drop on Pad");
    }

    fn layer_samples(&mut self, chain: ChainRef, rack: DeviceId, note: u8, paths: &[PathBuf]) {
        let existing = self.chains_on_pad(chain, rack, note).first().copied();
        let Some(id) = existing.or_else(|| self.new_pad_chain(chain, rack, note, "Layers")) else {
            return;
        };
        let Some(at) = chain.child(rack, id) else {
            return;
        };
        if let Some((_, old)) = self.source_of(at) {
            self.remove_device(at, old);
        }
        let Some(inner) = self.add_rack(at, RackKind::Instrument, "Instrument Rack") else {
            return;
        };
        for path in paths {
            let Some(c) = self.add_chain(at, inner, &stem(path)) else {
                continue;
            };
            let Some(place) = at.child(inner, c) else {
                continue;
            };
            let device = Device::builtin(PAD_SAMPLER, PAD_SAMPLER_NAME);
            let sampler = device.id;
            self.add_device(place, None, device);
            self.set_device_sample(place, sampler, path.clone());
        }
    }

    /// Swaps the notes two pads answer. Clips still play the notes they did, so they trigger the
    /// other sound now (p. 503).
    pub fn swap_pads(&mut self, chain: ChainRef, rack: DeviceId, a: u8, b: u8) {
        if a == b {
            return;
        }
        let from_a = self.chains_on_pad(chain, rack, a);
        let from_b = self.chains_on_pad(chain, rack, b);
        for (ids, to) in [(from_a, b), (from_b, a)] {
            for id in ids {
                if let Some(mut pad) = self.pad_of(chain, rack, id) {
                    pad.receive = Some(to);
                    self.set_drum_pad(chain, rack, id, pad);
                }
            }
        }
        self.mark_edit("Swap Pads");
    }

    /// Layers the chains of one pad with those of another, in a nested Instrument Rack on the
    /// second pad (p. 503).
    pub fn layer_pads(&mut self, chain: ChainRef, rack: DeviceId, from: u8, to: u8) {
        if from == to {
            return;
        }
        let there = self.chains_on_pad(chain, rack, to);
        let here = self.chains_on_pad(chain, rack, from);
        if here.is_empty() {
            return;
        }
        if there.is_empty() {
            self.swap_pads(chain, rack, from, to);
            return;
        }
        let ids: Vec<ChainId> = there.iter().chain(&here).copied().collect();
        let play = self
            .pad_of(chain, rack, ids[0])
            .map_or(PLAY_NOTE, |p| p.play);
        let Some((wrapper, inner)) = self.group_chains_as(chain, rack, &ids, RackKind::Instrument)
        else {
            return;
        };
        self.set_drum_pad(
            chain,
            rack,
            wrapper,
            DrumPad {
                receive: Some(to),
                play,
                choke: None,
            },
        );
        let Some(at) = chain.child(rack, wrapper) else {
            return;
        };
        for id in ids {
            let zones = self
                .rack(at, inner)
                .and_then(|d| d.chains().find(|c| c.id == id))
                .map(|c| c.zones);
            if let Some(mut zones) = zones {
                zones.drum = None;
                self.set_chain_zones(at, inner, id, zones);
            }
        }
        self.mark_edit("Layer Pads");
    }

    /// The first pad in the Drum Rack's default kit that nothing answers, for a new chain.
    pub fn first_free_pad(&self, chain: ChainRef, rack: DeviceId) -> u8 {
        (FIRST_PAD..=127)
            .chain(0..FIRST_PAD)
            .find(|k| self.chains_on_pad(chain, rack, *k).is_empty())
            .unwrap_or(FIRST_PAD)
    }
}

fn stem(path: &Path) -> String {
    path.file_stem().map_or_else(
        || "Sample".to_string(),
        |s| s.to_string_lossy().into_owned(),
    )
}

fn is_sampler(d: &Device) -> bool {
    matches!(
        &d.kind,
        DeviceKind::Builtin { type_id, .. }
            if type_id == PAD_SAMPLER || type_id == EARLIER_PAD_SAMPLER
    )
}

/// A copy of a chain with ids of its own, its macros and sends following along.
fn fresh_chain(chain: &Chain) -> Chain {
    let mut copy = chain.clone();
    let mut chains = HashMap::new();
    let mut devices = HashMap::new();
    refresh_chain(&mut copy, &mut chains, &mut devices);
    remap_chain(&mut copy, &chains, &devices);
    copy
}

fn refresh_chain(
    chain: &mut Chain,
    chains: &mut HashMap<ChainId, ChainId>,
    devices: &mut HashMap<DeviceId, DeviceId>,
) {
    let old = chain.id;
    chain.id = ChainId::new();
    chains.insert(old, chain.id);
    for d in &mut chain.devices {
        let old = d.id;
        d.id = DeviceId::new();
        devices.insert(old, d.id);
        for c in d.chains_mut() {
            refresh_chain(c, chains, devices);
        }
    }
}

fn remap_chain(
    chain: &mut Chain,
    chains: &HashMap<ChainId, ChainId>,
    devices: &HashMap<DeviceId, DeviceId>,
) {
    for send in &mut chain.mix.sends {
        if let Some(to) = chains.get(&send.to) {
            send.to = *to;
        }
    }
    for d in &mut chain.devices {
        if let DeviceKind::Rack { macros, .. } = &mut d.kind {
            for map in macros.iter_mut().flat_map(|m| m.mappings.iter_mut()) {
                match &mut map.target.owner {
                    ParamOwner::Device(id) => {
                        if let Some(new) = devices.get(id) {
                            *id = *new;
                        }
                    }
                    ParamOwner::Chain(id) => {
                        if let Some(new) = chains.get(id) {
                            *id = *new;
                        }
                    }
                    _ => {}
                }
            }
        }
        for c in d.chains_mut() {
            remap_chain(c, chains, devices);
        }
    }
}
