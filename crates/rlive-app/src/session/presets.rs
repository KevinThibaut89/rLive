//! Presets, defaults and Hot-Swap: what a device, chain or track is loaded from (manual
//! pp. 458-477).

use super::{ChainRef, Session};
use crate::session::rack_edit::{fresh_chain, fresh_device};
use rlive_browser::defaults::{DefaultsLookup, DropTarget, UserLibrary};
use rlive_browser::item::Role;
use rlive_browser::preset::{
    device_key, device_role, strip_for_storage, unused_path, Payload, PluginConfig, Preset,
    TrackTemplate,
};
use rlive_core::{ChainId, DeviceId, TrackId};
use rlive_devices::DeviceRole;
use rlive_plugin_host::PluginDescriptor;
use rlive_project::{Device, DeviceKind, FileRef, Route, Track};
use std::path::{Path, PathBuf};

/// What Hot-Swap is replacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapTarget {
    Device {
        chain: ChainRef,
        id: DeviceId,
    },
    /// A sample slot of a Simpler, Sampler, Impulse or Drum Sampler.
    Sample {
        chain: ChainRef,
        id: DeviceId,
        slot: usize,
    },
    /// A Drum Rack pad, by the note it plays.
    Pad {
        chain: ChainRef,
        rack: DeviceId,
        note: u8,
    },
    /// What's on one chain of a rack.
    RackChain {
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
    },
}

/// What goes in its place.
#[derive(Debug, Clone, Copy)]
pub enum SwapItem<'a> {
    Builtin(&'a str),
    Plugin(&'a PluginDescriptor),
    Preset(&'a Preset),
    Sample(&'a Path),
}

impl SwapItem<'_> {
    /// Hot-Swap only offers what the target can take.
    fn is_sample(&self) -> bool {
        matches!(self, Self::Sample(_))
    }
}

fn device_role_of(role: Role) -> DeviceRole {
    match role {
        Role::Instrument => DeviceRole::Instrument,
        Role::AudioEffect => DeviceRole::AudioEffect,
        Role::MidiEffect => DeviceRole::MidiEffect,
    }
}

impl Session {
    /// Where defaults come from for the Set that's open.
    pub fn defaults(&self) -> DefaultsLookup {
        let project = self.project_root();
        DefaultsLookup::new(self.library.clone(), project.as_deref())
    }

    // ------------------------------------------------------------- making devices

    /// A device the way its default says: a Project's, then the User Library's, then factory.
    pub(super) fn default_device(&self, info: &rlive_devices::BuiltinInfo) -> Device {
        let role = role_of(info.role);
        self.defaults()
            .device_preset(role, info.name)
            .and_then(|p| Preset::load(&p).ok())
            .and_then(|p| device_of(&p))
            .map(|mut d| {
                d.name = info.name.to_string();
                d
            })
            .unwrap_or_else(|| Device::builtin(info.id, info.name))
    }

    /// A plug-in's device, with its default parameter configuration if it has one.
    pub(super) fn apply_plugin_config(
        &mut self,
        chain: ChainRef,
        desc: &PluginDescriptor,
        id: DeviceId,
    ) {
        let Some(path) = self.defaults().plugin_config(desc.format, &desc.id) else {
            return;
        };
        let Some(Payload::PluginConfig(config)) = Preset::load(&path).ok().map(|p| p.payload)
        else {
            return;
        };
        // Which parameters show and in what order: no values come with it (manual p. 468).
        if let Some(DeviceKind::Plugin { panel, .. }) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
            .map(|d| &mut d.kind)
        {
            panel.params = Some(config.params);
            panel.xy = config.xy;
        }
    }

    /// What a dropped sample loads into at `target`, without the sample.
    pub(super) fn dropped_sample_device(
        &self,
        target: DropTarget,
        name: &str,
        type_id: &str,
    ) -> Device {
        self.defaults()
            .dropping(target)
            .and_then(|p| Preset::load(&p).ok())
            .and_then(|p| device_of(&p))
            .unwrap_or_else(|| Device::builtin(type_id, name))
    }

    /// A sample dropped on a chain, loaded in a new Simpler or whatever the default is.
    pub(crate) fn add_sampled_device(&mut self, chain: ChainRef, path: PathBuf) {
        let device =
            self.dropped_sample_device(DropTarget::OnDeviceView, "Simpler", "rlive.simpler");
        let name = device.name.clone();
        if !self.accepts_role(chain, DeviceRole::Instrument, &name) {
            return;
        }
        let id = device.id;
        let index = self.instrument_insert_index(chain, DeviceRole::Instrument);
        self.add_device(chain, index, device);
        self.set_device_sample(chain, id, path);
    }

    // ------------------------------------------------------------- loading presets

    /// A preset as a device with ids of its own, on a chain.
    pub fn load_preset(
        &mut self,
        chain: ChainRef,
        index: Option<usize>,
        preset: &Preset,
    ) -> Result<DeviceId, String> {
        let device =
            device_of(preset).ok_or_else(|| format!("{} isn't a device preset", preset.name))?;
        let role = self.role_of_device(&device);
        if !self.accepts_role(chain, role, &preset.name) {
            return Err(format!("{} doesn't fit there", preset.name));
        }
        let index = index.or_else(|| self.instrument_insert_index(chain, role));
        let id = device.id;
        self.add_device(chain, index, device);
        Ok(id)
    }

    /// Adds a saved chain to a rack.
    pub fn load_chain_preset(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        index: Option<usize>,
        preset: &Preset,
    ) -> Result<ChainId, String> {
        let Payload::Chain(saved) = &preset.payload else {
            return Err(format!("{} isn't a chain preset", preset.name));
        };
        let new = fresh_chain(saved);
        let id = new.id;
        let count = self.rack(chain, rack).map_or(0, |d| d.chains().count());
        self.insert_rack_chain(chain, rack, index.unwrap_or(count), new);
        Ok(id)
    }

    /// What a device is, by its runtime if it has one or else by what it says it is.
    fn role_of_device(&self, d: &Device) -> DeviceRole {
        device_role(d).map_or(DeviceRole::AudioEffect, device_role_of)
    }

    // ------------------------------------------------------------- Hot-Swap

    /// Replaces what `target` points at with `item`.
    pub fn hot_swap(&mut self, target: &SwapTarget, item: SwapItem<'_>) -> Result<(), String> {
        match *target {
            SwapTarget::Device { chain, id } => match item {
                SwapItem::Sample(path) => {
                    self.set_device_sample(chain, id, path.to_path_buf());
                    Ok(())
                }
                _ => self.swap_device(chain, id, item),
            },
            SwapTarget::Sample { chain, id, slot } => match item {
                SwapItem::Sample(path) => {
                    self.set_device_sample_at(chain, id, slot, path.to_path_buf());
                    Ok(())
                }
                _ => Err("Only a sample can go here".to_string()),
            },
            SwapTarget::Pad { chain, rack, note } => {
                match item {
                    SwapItem::Builtin(t) => {
                        self.drop_on_pad(chain, rack, note, super::PadDrop::Builtin(t.to_string()))
                    }
                    SwapItem::Plugin(p) => {
                        self.drop_on_pad(chain, rack, note, super::PadDrop::Plugin(p.clone()))
                    }
                    SwapItem::Sample(p) => {
                        self.drop_on_pad(chain, rack, note, super::PadDrop::Sample(p.to_path_buf()))
                    }
                    SwapItem::Preset(preset) => {
                        let id = self
                            .chains_on_pad(chain, rack, note)
                            .first()
                            .copied()
                            .or_else(|| self.new_pad_chain(chain, rack, note, &preset.name));
                        let Some(id) = id else {
                            return Err("That pad can't take it".to_string());
                        };
                        self.swap_rack_chain(chain, rack, id, preset)?;
                    }
                }
                Ok(())
            }
            SwapTarget::RackChain { chain, rack, id } => match item {
                SwapItem::Preset(preset) => self.swap_rack_chain(chain, rack, id, preset),
                SwapItem::Sample(_) => Err("Load a preset or a device here".to_string()),
                _ => {
                    let at = chain.child(rack, id).ok_or("That chain is gone")?;
                    let device = self.new_device(&item)?;
                    self.replace_chain_devices(at, vec![device]);
                    self.mark_edit("Hot-Swap Chain");
                    Ok(())
                }
            },
        }
    }

    fn new_device(&self, item: &SwapItem<'_>) -> Result<Device, String> {
        match item {
            SwapItem::Builtin(t) => rlive_devices::find(t)
                .map(|i| self.default_device(i))
                .ok_or_else(|| format!("No device {t}")),
            SwapItem::Plugin(p) => Ok(Device::plugin(p.format, &p.id, p.path.clone(), &p.name)),
            SwapItem::Preset(p) => {
                device_of(p).ok_or_else(|| format!("{} isn't a device preset", p.name))
            }
            SwapItem::Sample(_) => Err("A sample isn't a device".to_string()),
        }
    }

    /// Swaps a device in place. It stays at its place in the chain under the same id, so routing
    /// to it, its sidechain and its mappings all keep working; an automation envelope stays if
    /// the new device has the same parameters.
    fn swap_device(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        item: SwapItem<'_>,
    ) -> Result<(), String> {
        debug_assert!(!item.is_sample());
        if self.devices_locked(chain) {
            return Err("The track is frozen".to_string());
        }
        let mut new = self.new_device(&item)?;
        let Some(old) = self.devices(chain).iter().find(|d| d.id == id).cloned() else {
            return Err("That device is gone".to_string());
        };
        let new_role = match item {
            SwapItem::Plugin(p) => super::build::plugin_role(p),
            _ => self.role_of_device(&new),
        };
        let old_role = self.runtimes.get(&id).map_or(new_role, |rt| rt.role);
        if old_role != new_role {
            return Err("Only a preset of the same device type can be swapped in".to_string());
        }
        new.id = id;
        new.sidechain = old.sidechain;
        new.expanded = old.expanded;
        let same =
            device_key(&old) == device_key(&new) && !matches!(old.kind, DeviceKind::Rack { .. });
        if same {
            self.runtimes.remove(&id);
        } else {
            let mut gone = old;
            self.retire_device_runtimes(chain, &mut gone);
        }
        if let Some(slot) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            *slot = new;
        }
        self.rebuild_device(chain, id);
        self.mark_edit("Hot-Swap Device");
        Ok(())
    }

    fn swap_rack_chain(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        preset: &Preset,
    ) -> Result<(), String> {
        let at = chain.child(rack, id).ok_or("That chain is gone")?;
        let devices = match &preset.payload {
            Payload::Chain(c) => fresh_chain(c).devices,
            Payload::Device(_) => vec![device_of(preset).expect("a device")],
            _ => return Err(format!("{} can't go on a chain", preset.name)),
        };
        self.replace_chain_devices(at, devices);
        self.rename_chain(chain, rack, id, &preset.name);
        self.mark_edit("Hot-Swap Chain");
        Ok(())
    }

    fn replace_chain_devices(&mut self, at: ChainRef, devices: Vec<Device>) {
        let existing: Vec<DeviceId> = self.devices(at).iter().map(|d| d.id).collect();
        for d in existing {
            self.remove_device(at, d);
        }
        for (i, d) in devices.into_iter().enumerate() {
            self.add_device(at, Some(i), d);
        }
    }

    // ------------------------------------------------------------- saving

    fn device_copy(&mut self, chain: ChainRef, id: DeviceId) -> Option<Device> {
        self.capture_plugin_states();
        self.devices(chain).iter().find(|d| d.id == id).cloned()
    }

    /// Puts the files a preset refers to in the User Library, so the preset travels with it
    /// (manual p. 464). What's already there, or can't be copied, stays where it is.
    fn collect_preset_files(&mut self, preset: &mut Preset) {
        if !self.collect_on_export {
            return;
        }
        let Some(library) = self.library.clone() else {
            return;
        };
        let dir = library.imported_samples_dir();
        for file in preset.file_refs_mut() {
            if file.path.starts_with(&library.root) {
                continue;
            }
            if let Some(copied) = copy_into(&file.path, &dir) {
                *file = FileRef::new(copied);
            }
        }
    }

    pub(super) fn library(&self) -> Result<UserLibrary, String> {
        self.library
            .clone()
            .ok_or_else(|| "There is no User Library".to_string())
    }

    /// Saves a device to the User Library's Presets folder and returns where it went.
    pub fn save_device_preset(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        name: Option<&str>,
    ) -> Result<PathBuf, String> {
        let library = self.library()?;
        let device = self.device_copy(chain, id).ok_or("That device is gone")?;
        let name = name.unwrap_or(&device.name).to_string();
        let dir = match (&device.kind, device_role(&device)) {
            (DeviceKind::Rack { .. }, _) => library.rack_presets_dir(&device.name),
            (DeviceKind::Builtin { type_id, .. }, Some(role)) => {
                let shelf = rlive_devices::find(type_id).map_or(device.name.as_str(), |i| i.name);
                library.presets_dir(role, shelf)
            }
            _ => library
                .root
                .join("Presets")
                .join("Plug-Ins")
                .join(&device.name),
        };
        let mut preset = Preset::of_device(&device, &name);
        self.collect_preset_files(&mut preset);
        let path = unused_path(&dir, &name);
        preset.save(&path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Saves a preset to an exact path, for dropping a device onto a folder in the browser.
    pub fn save_device_preset_as(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        path: &Path,
    ) -> Result<(), String> {
        let device = self.device_copy(chain, id).ok_or("That device is gone")?;
        let name = path
            .file_stem()
            .map_or_else(|| device.name.clone(), |n| n.to_string_lossy().into_owned());
        let mut preset = Preset::of_device(&device, &name);
        self.collect_preset_files(&mut preset);
        preset.save(path).map_err(|e| e.to_string())
    }

    /// Where this device's default preset would go, and whether one is already there.
    pub fn default_preset_status(&self, chain: ChainRef, id: DeviceId) -> Option<(PathBuf, bool)> {
        let library = self.library.as_ref()?;
        let device = self.devices(chain).iter().find(|d| d.id == id)?;
        let role = device_role(device)?;
        let name = default_name(device);
        let path = library.device_default_path(role, &name);
        let exists = path.exists();
        Some((path, exists))
    }

    /// Saves a device as its default. It's named after the device, which is how it's found.
    pub fn save_default_preset(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
    ) -> Result<PathBuf, String> {
        let (path, _) = self
            .default_preset_status(chain, id)
            .ok_or("There is no User Library")?;
        let device = self.device_copy(chain, id).ok_or("That device is gone")?;
        let mut preset = Preset::of_device(&device, &default_name(&device));
        self.collect_preset_files(&mut preset);
        preset.save(&path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Saves a plug-in's parameter list, and nothing of their values, as its default.
    pub fn save_default_plugin_config(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
    ) -> Result<PathBuf, String> {
        let library = self.library()?;
        let device = self.device_copy(chain, id).ok_or("That device is gone")?;
        let DeviceKind::Plugin {
            format, plugin_id, ..
        } = &device.kind
        else {
            return Err("Only a plug-in has a parameter configuration".to_string());
        };
        let config = PluginConfig {
            format: *format,
            plugin_id: plugin_id.clone(),
            params: self.shown_params(chain, id),
            xy: self.xy_params(chain, id),
        };
        let path = library.plugin_config_path(*format, plugin_id);
        Preset::of_plugin_config(config, "Default")
            .save(&path)
            .map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Saves a Drum Rack pad's sampler as what samples dropped on pads load into.
    pub fn save_default_pad(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        note: u8,
    ) -> Result<PathBuf, String> {
        let library = self.library()?;
        let id = self
            .chains_on_pad(chain, rack, note)
            .first()
            .copied()
            .ok_or("That pad is empty")?;
        let at = chain.child(rack, id).ok_or("That chain is gone")?;
        let device = self
            .devices(at)
            .iter()
            .find(|d| matches!(&d.kind, DeviceKind::Builtin { .. }))
            .cloned()
            .ok_or("That pad has no sampler")?;
        let mut device = device;
        if let DeviceKind::Builtin { samples, .. } = &mut device.kind {
            samples.clear();
        }
        let name = device.name.clone();
        let preset = Preset::of_device(&device, &name);
        let dir = library.dropping_dir(DropTarget::OnDrumRack);
        let path = unused_path(&dir, &name);
        preset.save(&path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Saves a rack chain as a preset to load into any rack.
    pub fn save_chain_preset(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        id: ChainId,
        name: Option<&str>,
    ) -> Result<PathBuf, String> {
        let library = self.library()?;
        self.capture_plugin_states();
        let saved = self
            .rack(chain, rack)
            .and_then(|d| d.chains().find(|c| c.id == id).cloned())
            .ok_or("That chain is gone")?;
        let name = name.unwrap_or(&saved.name).to_string();
        let mut preset = Preset::of_chain(&saved, &name);
        self.collect_preset_files(&mut preset);
        let path = unused_path(&library.chain_presets_dir(), &name);
        preset.save(&path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Saves a track's devices and mixer settings as what new tracks of its kind start with.
    pub fn save_track_default(&mut self, id: TrackId) -> Result<PathBuf, String> {
        let library = self.library()?;
        self.capture_plugin_states();
        let t = self.project.track(id).ok_or("That track is gone")?.clone();
        let path = library
            .track_default_path(t.kind)
            .ok_or("Only audio and MIDI tracks have defaults")?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let returns: Vec<TrackId> = self.project.returns.iter().map(|r| r.id).collect();
        let template = track_template(&t, &returns);
        let mut preset = Preset::of_track(template, &name);
        self.collect_preset_files(&mut preset);
        preset.save(&path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Dresses a new track from its default, if there is one. Called before it's installed.
    pub(super) fn apply_track_default(&self, t: &mut Track) -> bool {
        let Some(preset) = self
            .defaults()
            .track(t.kind)
            .and_then(|p| Preset::load(&p).ok())
        else {
            return false;
        };
        let Payload::Track(template) = preset.payload else {
            return false;
        };
        t.volume_db = template.volume_db;
        t.pan = template.pan;
        t.routing.input = template.input;
        t.routing.output = template.output;
        t.devices = template
            .devices
            .iter()
            .map(fresh_device_flattened)
            .collect();
        let returns = self.project.returns.iter().map(|r| r.id);
        for (to, level) in returns.zip(template.sends) {
            if let Some(send) = t.sends.iter_mut().find(|s| s.to == to) {
                send.level_db = level;
            }
        }
        true
    }

    // ------------------------------------------------------------- A/B

    /// Which of a device's states is showing, for a device that has them.
    pub fn compare_slot(
        &self,
        chain: ChainRef,
        id: DeviceId,
    ) -> Option<rlive_project::CompareSlot> {
        let d = self.devices(chain).iter().find(|d| d.id == id)?;
        matches!(d.kind, DeviceKind::Builtin { .. }).then(|| {
            d.compare
                .as_ref()
                .map_or_else(Default::default, |c| c.selected)
        })
    }

    /// Copies the state showing to the other one.
    pub fn compare_copy(&mut self, chain: ChainRef, id: DeviceId) {
        if self.devices_locked(chain) {
            return;
        }
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        else {
            return;
        };
        if !matches!(d.kind, DeviceKind::Builtin { .. }) {
            return;
        }
        let current = d.current_state();
        let compare = d.compare.get_or_insert_with(Default::default);
        compare.other = current;
        self.mark_edit("Copy Device State");
    }

    /// Switches a built-in device to its other state. Automation is left off for the parameters
    /// it reaches, each of which the user turns back on (manual p. 456).
    pub fn compare_switch(&mut self, chain: ChainRef, id: DeviceId) {
        use rlive_project::CompareSlot::{A, B};
        if self.devices_locked(chain) {
            return;
        }
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        else {
            return;
        };
        if !matches!(d.kind, DeviceKind::Builtin { .. }) {
            return;
        }
        let mut compare = d.compare.take().unwrap_or_default();
        let shown = d.current_state();
        let target = std::mem::replace(&mut compare.other, shown.clone());
        compare.selected = match compare.selected {
            A => B,
            B => A,
        };
        d.compare = Some(compare);
        d.params = target.params.clone();
        let needs_rebuild = {
            let DeviceKind::Builtin { samples, state, .. } = &mut d.kind else {
                return;
            };
            let changed = *samples != target.samples || *state != target.state;
            *samples = target.samples.clone();
            *state = target.state.clone();
            changed
        };
        let params: Vec<(u32, f64)> = self.runtimes.get(&id).map_or(Vec::new(), |rt| {
            rt.params
                .iter()
                .map(|p| {
                    let value = target
                        .params
                        .get(&p.id)
                        .copied()
                        .or_else(|| rt.unmoved.get(&p.id).copied())
                        .unwrap_or(p.default);
                    (p.id, value)
                })
                .collect()
        });
        if needs_rebuild {
            self.rebuild_device(chain, id);
        }
        for (param, value) in params {
            if let Some(cell) = self.device_cell(chain, id, param) {
                // Marked as a gesture, which is what leaves automation overridden.
                self.set_cell(chain, cell, value, true);
            }
        }
        self.mark_edit("Switch Device State");
    }
}

fn role_of(role: DeviceRole) -> Role {
    match role {
        DeviceRole::Instrument => Role::Instrument,
        DeviceRole::AudioEffect => Role::AudioEffect,
        DeviceRole::MidiEffect => Role::MidiEffect,
    }
}

/// The name a device's default preset has to have: the device's own.
fn default_name(d: &Device) -> String {
    match &d.kind {
        DeviceKind::Builtin { type_id, .. } => {
            rlive_devices::find(type_id).map_or_else(|| d.name.clone(), |i| i.name.to_string())
        }
        _ => d.name.clone(),
    }
}

/// A preset's device with ids of its own, holding the preset in both its A and B.
pub(crate) fn device_of(preset: &Preset) -> Option<Device> {
    let Payload::Device(d) = &preset.payload else {
        return None;
    };
    let mut device = fresh_device_flattened(d);
    device.name.clone_from(&preset.name);
    Some(device)
}

fn fresh_device_flattened(d: &Device) -> Device {
    let mut d = fresh_device(d);
    d.for_each_mut(&mut Device::flatten_compare);
    d
}

fn track_template(t: &Track, returns: &[TrackId]) -> TrackTemplate {
    let portable = |r: &Route| match r {
        Route::Track { .. } | Route::TrackChannel { .. } => Route::Default,
        other => other.clone(),
    };
    let sends = returns
        .iter()
        .map(|r| {
            t.sends
                .iter()
                .find(|s| s.to == *r)
                .map_or(rlive_project::SEND_OFF_DB, |s| s.level_db)
        })
        .collect();
    let mut devices = t.devices.clone();
    for d in &mut devices {
        strip_for_storage(d);
    }
    TrackTemplate {
        kind: t.kind,
        volume_db: t.volume_db,
        pan: t.pan,
        sends,
        input: portable(&t.routing.input),
        output: portable(&t.routing.output),
        devices,
    }
}

/// Copies a file into `dir` unless an identical copy is there, returning where it is now.
fn copy_into(file: &Path, dir: &Path) -> Option<PathBuf> {
    let name = file.file_name()?;
    std::fs::create_dir_all(dir).ok()?;
    let target = dir.join(name);
    if target.exists() {
        let same = std::fs::metadata(&target).ok()?.len() == std::fs::metadata(file).ok()?.len();
        if same {
            return Some(target);
        }
        let stem = file.file_stem()?.to_string_lossy();
        let ext = file
            .extension()
            .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
        let numbered = (2..1000)
            .map(|n| dir.join(format!("{stem} {n}{ext}")))
            .find(|p| !p.exists())?;
        std::fs::copy(file, &numbered).ok()?;
        return Some(numbered);
    }
    std::fs::copy(file, &target).ok()?;
    Some(target)
}
