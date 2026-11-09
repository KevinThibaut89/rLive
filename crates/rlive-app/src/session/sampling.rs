//! What the sample players need from the Session: saved state, sample analysis, and the
//! commands that turn a Simpler into something else (manual pp. 735, 774, 801).

use super::racks::FIRST_PAD;
use super::{ChainRef, Session};
use rlive_core::{DeviceId, Resource, SampleBuffer, TrackId, PPQ};
use rlive_devices::devices::b10::common::{Analysis, ANALYSIS_SLOTS, STATE_SLOT};
use rlive_devices::devices::b10::multi::{Layer, ParsedZones, ZoneMap};
use rlive_devices::devices::b10::multi_params as sampler_params;
use rlive_devices::devices::b10::{drum_sampler as drum, simpler as si};
use rlive_devices::devices::b12::operator_waves::ParsedWaves;
use rlive_devices::sampler::p as sampler_old;
use rlive_engine::GraphOp;
use rlive_project::{
    Clip, ClipContent, Device, DeviceKind, FileRef, NoteData, RackKind, TrackKind,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Something done to a sample player from its panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingCommand {
    SliceToDrumRack,
    SliceToMidiTrack,
    SimplerToSampler,
    SamplerToSimpler,
    DrumSamplerToSimpler,
    NormalizeGain,
    Crop,
    /// Normalize Volume of one of a Sampler's layers (p. 779).
    NormalizeLayerVolume(usize),
    /// Normalize Pan of one of a Sampler's layers (p. 779).
    NormalizeLayerPan(usize),
}

fn type_of(d: &Device) -> Option<&str> {
    match &d.kind {
        DeviceKind::Builtin { type_id, .. } => Some(type_id),
        _ => None,
    }
}

/// The parsed form of `text` a device of `type_id` swaps in at [`STATE_SLOT`].
fn state_resource(type_id: &str, text: &str) -> Option<Resource> {
    match type_id {
        "rlive.simpler" => Some(Arc::new(si::ParsedState::parse(Some(text)))),
        "rlive.sampler" => Some(Arc::new(ParsedZones {
            map: ZoneMap::parse(Some(text)),
        })),
        "rlive.operator" => Some(Arc::new(ParsedWaves::parse(Some(text)))),
        _ => None,
    }
}

/// Which device kinds want an [`Analysis`] of the sample in a slot, and with what transients.
fn wants_analysis(type_id: &str) -> Option<bool> {
    match type_id {
        "rlive.simpler" => Some(true),
        "rlive.drum_sampler" | "rlive.sampler" => Some(false),
        _ => None,
    }
}

impl Session {
    /// Saves a device's own state and hands it to the engine, with no break in the sound.
    pub fn set_device_state(&mut self, chain: ChainRef, id: DeviceId, text: String) {
        let Some(dev) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        else {
            return;
        };
        let DeviceKind::Builtin { type_id, state, .. } = &mut dev.kind else {
            return;
        };
        *state = Some(text.clone());
        let resource = state_resource(type_id, &text);
        if let Some(resource) = resource {
            self.send_edits(vec![GraphOp::SetResource {
                target: chain.into(),
                device: id,
                slot: STATE_SLOT,
                resource,
            }]);
        }
        self.mark_edit("Change Sample Map");
    }

    /// Hands a sampler the analysis of a sample that just arrived in `slot`.
    pub(super) fn send_sample_analysis(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        slot: usize,
        sample: &Arc<SampleBuffer>,
    ) {
        let Some(dev) = self
            .chain(chain)
            .and_then(|v| v.iter().find(|d| d.id == id))
        else {
            return;
        };
        let Some(wants_transients) = type_of(dev).and_then(wants_analysis) else {
            return;
        };
        let path = dev.sample(slot).map(|f| f.path.clone());
        let found = path.and_then(|p| self.samples.transients(&p));
        let analysis = if wants_transients {
            Analysis::new(sample, found.as_ref().map(|t| t.as_slice()))
        } else {
            Analysis::new(sample, Some(&[]))
        };
        self.send_edits(vec![GraphOp::SetResource {
            target: chain.into(),
            device: id,
            slot: ANALYSIS_SLOTS + slot as u32,
            resource: Arc::new(analysis),
        }]);
    }

    /// Hands every sampler holding `path` its analysis, once the file has finished loading.
    pub(super) fn send_sample_analyses(&mut self, path: &Path, sample: &Arc<SampleBuffer>) {
        let mut found = Vec::new();
        for t in self.project.tracks.iter().chain(&self.project.returns) {
            samplers_using(&t.devices, ChainRef::Track(t.id), path, &mut found);
        }
        samplers_using(
            &self.project.master.devices,
            ChainRef::Master,
            path,
            &mut found,
        );
        for (chain, id, slot) in found {
            self.send_sample_analysis(chain, id, slot, sample);
        }
    }

    /// Where a Simpler would cut its sample right now.
    fn simpler_slices(&self, dev: &Device) -> Option<Vec<f64>> {
        let path = dev.sample(0).map(|f| f.path.clone())?;
        let sample = self.samples.get(&path)?;
        let value = |id: u32| {
            dev.params
                .get(&id)
                .copied()
                .unwrap_or_else(|| si::params()[id as usize].default)
        };
        let n = sample.len() as f64;
        let state = si::ParsedState::parse(match &dev.kind {
            DeviceKind::Builtin { state, .. } => state.as_deref(),
            _ => None,
        });
        let found = self.samples.transients(&path);
        let analysis = Analysis::new(&sample, found.as_ref().map(|t| t.as_slice()));
        let spec = si::SliceSpec {
            by: value(si::SLICE_BY).round() as usize,
            sensitivity: (value(si::SENSITIVITY) * 0.01) as f32,
            division_beats: si::DIVISION_BEATS
                [(value(si::DIVISION).round() as usize).min(si::DIVISION_BEATS.len() - 1)],
            regions: value(si::REGIONS).round() as usize,
            start: value(si::START_FLAG).clamp(0.0, 1.0) * n,
            end: value(si::END_FLAG).clamp(0.0, 1.0) * n,
            beats: value(si::WARP_BEATS),
        };
        let mut out = [0.0; si::MAX_SLICES + 1];
        let count = si::compute_slices(spec, Some(&analysis), &state.state, &mut out);
        Some(out[..=count].to_vec())
    }

    /// Replaces a Simpler with a Drum Rack, one Simpler per slice, starting on the first pad.
    pub fn slice_to_drum_rack(&mut self, chain: ChainRef, id: DeviceId) -> Option<DeviceId> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?.clone();
        let slices = self.simpler_slices(&dev)?;
        let index = self.chain(chain)?.iter().position(|d| d.id == id)?;
        self.remove_device(chain, id);
        let rack = self.add_rack(chain, RackKind::Drum, &dev.name)?;
        self.fill_with_slices(chain, rack, &dev, &slices, index)?;
        self.mark_edit("Slice to Drum Rack");
        Some(rack)
    }

    fn fill_with_slices(
        &mut self,
        chain: ChainRef,
        rack: DeviceId,
        dev: &Device,
        slices: &[f64],
        index: usize,
    ) -> Option<()> {
        let file = dev.sample(0)?.clone();
        let n = self
            .samples
            .get(&file.path)
            .map_or(1.0, |s| s.len() as f64)
            .max(1.0);
        for i in 0..slices.len() - 1 {
            let (a, b) = (slices[i], slices[i + 1]);
            let note = FIRST_PAD.saturating_add(i as u8);
            let name = format!("{} {}", dev.name, i + 1);
            let pad = self.new_pad_chain(chain, rack, note, &name)?;
            let at = chain.child(rack, pad)?;
            let mut d = Device::builtin("rlive.simpler", "Simpler");
            d.params.insert(si::MODE, si::MODE_ONE_SHOT as f64);
            d.params.insert(si::START_FLAG, a / n);
            d.params.insert(si::END_FLAG, b / n);
            if let DeviceKind::Builtin { samples, .. } = &mut d.kind {
                *samples = vec![Some(file.clone())];
            }
            self.add_device(at, None, d);
        }
        let _ = index;
        Some(())
    }

    /// A new track with a Drum Rack of slices and a clip that plays them in order, leaving the
    /// Simpler where it is. Returns the new track.
    pub fn slice_to_midi_track(&mut self, chain: ChainRef, id: DeviceId) -> Option<TrackId> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?.clone();
        let slices = self.simpler_slices(&dev)?;
        let track = self.add_track(TrackKind::Midi, None);
        let new = ChainRef::Track(track);
        if let Some(existing) = self.chain(new)?.first().map(|d| d.id) {
            self.remove_device(new, existing);
        }
        let rack = self.add_rack(new, RackKind::Drum, &dev.name)?;
        self.fill_with_slices(new, rack, &dev, &slices, 0)?;
        let sixteenth = PPQ / 4;
        let count = slices.len() - 1;
        let sig = self.project.time_signature;
        let bars = (count as u64 * sixteenth)
            .div_ceil(sig.ticks_per_bar())
            .max(1);
        let mut clip = Clip::new_midi(&dev.name, [120, 170, 220], bars, sig);
        if let Some(notes) = clip.notes_mut() {
            for i in 0..count {
                notes.push(NoteData::new(
                    i as u64 * sixteenth,
                    sixteenth,
                    FIRST_PAD.saturating_add(i as u8),
                    0.9,
                ));
            }
        }
        self.set_clip(track, 0, Some(clip));
        self.mark_edit("Slice to New MIDI Track");
        Some(track)
    }

    /// Replaces a Simpler with a Sampler that plays the same sample as one layer.
    pub fn simpler_to_sampler(&mut self, chain: ChainRef, id: DeviceId) -> Option<DeviceId> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?.clone();
        let value = |id: u32| {
            dev.params
                .get(&id)
                .copied()
                .unwrap_or_else(|| si::params()[id as usize].default)
        };
        let file = dev.sample(0)?.clone();
        let n = self.samples.get(&file.path).map_or(0.0, |s| s.len() as f64);
        let mut new = Device::builtin("rlive.sampler", "Sampler");
        let mut layer = Layer::new(&dev.name, 0);
        let (start, end) = (value(si::START_FLAG) * n, value(si::END_FLAG) * n);
        layer.start = Some(start);
        layer.end = Some(end);
        if value(si::LOOP_ON) >= 0.5 {
            let len = (end - start) * (value(si::LOOP) * 0.01).clamp(0.0, 1.0);
            layer.sustain = 1;
            layer.loop_start = end - len;
            layer.loop_end = end;
            layer.crossfade = len * (value(si::FADE) * 0.01).clamp(0.0, 1.0);
        }
        let map = ZoneMap {
            layers: vec![layer],
            ..ZoneMap::default()
        };
        if let DeviceKind::Builtin { samples, state, .. } = &mut new.kind {
            *samples = vec![Some(file)];
            *state = Some(map.serialize());
        }
        for (from, to) in [
            (si::VOLUME, sampler_old::VOLUME),
            (si::PAN, sampler_old::PAN),
            (si::TRANSPOSE, sampler_old::TUNE),
            (si::A_ATTACK, sampler_old::ATTACK),
            (si::A_RELEASE, sampler_old::RELEASE),
            (si::A_DECAY, sampler_params::V_DECAY),
            (si::A_SUSTAIN, sampler_params::V_SUSTAIN),
            (si::DETUNE, sampler_params::DETUNE),
            (si::SPREAD, sampler_params::SPREAD),
            (si::F_ON, sampler_params::F_ON),
            (si::F_TYPE, sampler_params::F_TYPE),
            (si::F_CIRCUIT, sampler_params::F_CIRCUIT),
            (si::F_SLOPE, sampler_params::F_SLOPE),
            (si::F_FREQ, sampler_params::F_FREQ),
            (si::F_RES, sampler_params::F_RES),
            (si::F_DRIVE, sampler_params::F_DRIVE),
            (si::F_MORPH, sampler_params::F_MORPH),
        ] {
            new.params.insert(to, value(from));
        }
        new.params
            .insert(sampler_old::VELOCITY, value(si::VEL_VOL) * 0.01);
        let replaced = self.replace_device(chain, id, new);
        self.mark_edit("Convert to Sampler");
        replaced
    }

    /// Replaces a Sampler with a Simpler on its first layer. The rest of a multisample can't
    /// come along: Simpler plays one sample.
    pub fn sampler_to_simpler(&mut self, chain: ChainRef, id: DeviceId) -> Option<DeviceId> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?.clone();
        let DeviceKind::Builtin { state, .. } = &dev.kind else {
            return None;
        };
        let map = ZoneMap::parse(state.as_deref());
        let layer = map.layers.first();
        let slot = layer.map_or(0, |l| l.slot as usize);
        let file = dev.sample(slot)?.clone();
        let n = self
            .samples
            .get(&file.path)
            .map_or(1.0, |s| s.len() as f64)
            .max(1.0);
        let value = |id: u32| {
            dev.params
                .get(&id)
                .copied()
                .unwrap_or_else(|| rlive_devices::sampler::params()[id as usize].default)
        };
        let mut new = Device::builtin("rlive.simpler", "Simpler");
        if let DeviceKind::Builtin { samples, .. } = &mut new.kind {
            *samples = vec![Some(file)];
        }
        new.params.insert(si::VOLUME, value(sampler_old::VOLUME));
        new.params.insert(si::PAN, value(sampler_old::PAN));
        new.params.insert(si::TRANSPOSE, value(sampler_old::TUNE));
        new.params
            .insert(si::VEL_VOL, value(sampler_old::VELOCITY) * 100.0);
        new.params.insert(si::A_ATTACK, value(sampler_old::ATTACK));
        new.params
            .insert(si::A_RELEASE, value(sampler_old::RELEASE));
        new.params
            .insert(si::A_DECAY, value(sampler_params::V_DECAY));
        new.params
            .insert(si::A_SUSTAIN, value(sampler_params::V_SUSTAIN));
        if let Some(l) = layer {
            new.params
                .insert(si::START_FLAG, l.start.unwrap_or(0.0) / n);
            new.params.insert(si::END_FLAG, l.end.unwrap_or(n) / n);
            if l.sustain != 0 {
                new.params.insert(si::LOOP_ON, 1.0);
                new.params.insert(
                    si::LOOP,
                    ((l.loop_end - l.loop_start) / n * 100.0).clamp(0.0, 100.0),
                );
            }
        }
        let replaced = self.replace_device(chain, id, new);
        self.mark_edit("Convert to Simpler");
        replaced
    }

    /// Drum Sampler > Simpler, keeping the sample's start and length (p. 735).
    pub fn drum_sampler_to_simpler(&mut self, chain: ChainRef, id: DeviceId) -> Option<DeviceId> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?.clone();
        let file = dev.sample(0)?.clone();
        let value = |id: u32| {
            dev.params
                .get(&id)
                .copied()
                .unwrap_or_else(|| drum::params()[id as usize].default)
        };
        let mut new = Device::builtin("rlive.simpler", "Simpler");
        if let DeviceKind::Builtin { samples, .. } = &mut new.kind {
            *samples = vec![Some(file)];
        }
        let start = (value(drum::START) * 0.01).clamp(0.0, 1.0);
        let length = (value(drum::LENGTH) * 0.01).clamp(0.0, 1.0);
        new.params.insert(si::MODE, si::MODE_ONE_SHOT as f64);
        new.params.insert(si::START_FLAG, start);
        new.params.insert(si::END_FLAG, (start + length).min(1.0));
        new.params.insert(si::VOLUME, value(drum::VOLUME));
        new.params.insert(si::PAN, value(drum::PAN));
        new.params.insert(si::TRANSPOSE, value(drum::TRANSPOSE));
        new.params.insert(si::DETUNE, value(drum::DETUNE));
        new.params.insert(si::VEL_VOL, value(drum::VEL_VOL));
        let replaced = self.replace_device(chain, id, new);
        self.mark_edit("Convert to Simpler");
        replaced
    }

    /// Swaps one device for another in the same place.
    fn replace_device(&mut self, chain: ChainRef, id: DeviceId, new: Device) -> Option<DeviceId> {
        let index = self.chain(chain)?.iter().position(|d| d.id == id)?;
        let new_id = new.id;
        self.remove_device(chain, id);
        self.add_device(chain, Some(index), new);
        Some(new_id)
    }

    /// Normalize Volumes: Gain raised until the sample's peak is at full scale (p. 801).
    pub fn normalize_sample_gain(&mut self, chain: ChainRef, id: DeviceId) -> Option<f64> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?;
        let path = dev.sample(0)?.path.clone();
        let gain_id = match type_of(dev)? {
            "rlive.simpler" => si::GAIN,
            "rlive.drum_sampler" => drum::GAIN,
            _ => return None,
        };
        let sample = self.samples.get(&path)?;
        let peak = sample
            .left
            .iter()
            .chain(&sample.right)
            .fold(0.0f32, |m, s| m.max(s.abs()));
        if peak <= 0.0 {
            return None;
        }
        let db = (-20.0 * (peak as f64).log10()).clamp(-36.0, 36.0);
        self.set_param(chain, id, gain_id, db);
        Some(db)
    }

    /// Normalizes one layer of a Sampler: its peak to full scale, or its pan so left and right
    /// are balanced.
    pub fn normalize_sampler_layer(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        layer: usize,
        pan: bool,
    ) -> Option<()> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?;
        let DeviceKind::Builtin { state, .. } = &dev.kind else {
            return None;
        };
        let mut map = ZoneMap::parse(state.as_deref());
        let entry = map.layers.get_mut(layer)?;
        let sample = self.samples.get(&dev.sample(entry.slot as usize)?.path)?;
        if pan {
            let rms =
                |c: &[f32]| (c.iter().map(|s| s * s).sum::<f32>() / c.len().max(1) as f32).sqrt();
            let (l, r) = (rms(&sample.left).max(1e-9), rms(&sample.right).max(1e-9));
            entry.pan = if l > r { 1.0 - r / l } else { l / r - 1.0 };
        } else {
            let peak = sample
                .left
                .iter()
                .chain(&sample.right)
                .fold(0.0f32, |m, s| m.max(s.abs()));
            if peak <= 0.0 {
                return None;
            }
            entry.volume_db = -20.0 * peak.log10();
        }
        self.set_device_state(chain, id, map.serialize());
        Some(())
    }

    /// Puts an audio clip's file in a Simpler and keeps its Warp Markers (p. 792): the clip's
    /// section becomes the region between the flags, and it plays warped as before.
    pub fn load_clip_into_simpler(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        track: TrackId,
        slot: usize,
    ) -> Option<()> {
        let clip = self.clip(track, slot)?.clone();
        let ClipContent::Audio { file, warp, .. } = &clip.content else {
            return None;
        };
        let path = file.path.clone();
        self.set_device_sample(chain, id, path.clone());
        let sample = self.samples.get(&path)?;
        let n = (sample.len() as f64).max(1.0);
        let rate = sample.sample_rate.max(1.0);
        let state = si::SimplerState {
            warp_markers: warp
                .markers
                .iter()
                .map(|m| (m.seconds * rate, m.tick))
                .collect(),
            ..Default::default()
        };
        let parsed = si::ParsedState::from_state(state);
        let to_seconds = |tick: f64| {
            parsed
                .warp_map
                .as_ref()
                .map_or(tick / PPQ as f64, |m| m.tick_to_src(tick) / rate)
        };
        let start_seconds = to_seconds(clip.start as f64);
        let end_seconds = to_seconds(clip.start as f64 + clip.length as f64);
        self.set_param(chain, id, si::WARP, 1.0);
        self.set_param(
            chain,
            id,
            si::START_FLAG,
            (start_seconds * rate / n).clamp(0.0, 1.0),
        );
        self.set_param(
            chain,
            id,
            si::END_FLAG,
            (end_seconds * rate / n).clamp(0.0, 1.0),
        );
        self.set_param(chain, id, si::WARP_BEATS, clip.length as f64 / PPQ as f64);
        let text = si::ParsedState::serialize(&parsed.state);
        self.set_device_state(chain, id, text);
        self.mark_edit("Load Clip Into Simpler");
        Some(())
    }

    /// Crop (p. 801): writes the part of a Simpler's sample between its flags to a new file, and
    /// plays that from now on. The original file is untouched.
    pub fn crop_simpler_sample(&mut self, chain: ChainRef, id: DeviceId) -> Option<PathBuf> {
        let dev = self.chain(chain)?.iter().find(|d| d.id == id)?;
        let path = dev.sample(0)?.path.clone();
        let sample = self.samples.get(&path)?;
        let value = |id: u32| {
            dev.params
                .get(&id)
                .copied()
                .unwrap_or(si::params()[id as usize].default)
        };
        let n = sample.len();
        let start = (value(si::START_FLAG) * n as f64) as usize;
        let end = ((value(si::END_FLAG) * n as f64) as usize).clamp(start, n);
        let dir = std::env::temp_dir().join("rLive").join("Cropped");
        std::fs::create_dir_all(&dir).ok()?;
        let stem = path.file_stem().map_or_else(
            || "sample".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let out = dir.join(format!("{stem} [{start}-{end}].wav"));
        rlive_devices::write_wav(
            &out,
            &sample.left[start..end],
            &sample.right[start..end],
            sample.sample_rate as u32,
        )
        .ok()?;
        self.set_param(chain, id, si::START_FLAG, 0.0);
        self.set_param(chain, id, si::END_FLAG, 1.0);
        self.set_device_sample(chain, id, out.clone());
        self.mark_edit("Crop Sample");
        Some(out)
    }

    pub fn sampling_command(&mut self, chain: ChainRef, id: DeviceId, command: SamplingCommand) {
        match command {
            SamplingCommand::SliceToDrumRack => {
                self.slice_to_drum_rack(chain, id);
            }
            SamplingCommand::SliceToMidiTrack => {
                self.slice_to_midi_track(chain, id);
            }
            SamplingCommand::SimplerToSampler => {
                self.simpler_to_sampler(chain, id);
            }
            SamplingCommand::SamplerToSimpler => {
                self.sampler_to_simpler(chain, id);
            }
            SamplingCommand::DrumSamplerToSimpler => {
                self.drum_sampler_to_simpler(chain, id);
            }
            SamplingCommand::NormalizeGain => {
                self.normalize_sample_gain(chain, id);
            }
            SamplingCommand::Crop => {
                self.crop_simpler_sample(chain, id);
            }
            SamplingCommand::NormalizeLayerVolume(layer) => {
                self.normalize_sampler_layer(chain, id, layer, false);
            }
            SamplingCommand::NormalizeLayerPan(layer) => {
                self.normalize_sampler_layer(chain, id, layer, true);
            }
        }
    }
}

/// Every sample slot of a Simpler, Drum Sampler or Sampler anywhere under `devices` holding
/// `path`.
fn samplers_using(
    devices: &[Device],
    here: ChainRef,
    path: &Path,
    out: &mut Vec<(ChainRef, DeviceId, usize)>,
) {
    for d in devices {
        if type_of(d).is_some_and(|t| wants_analysis(t).is_some()) {
            if let DeviceKind::Builtin { samples, .. } = &d.kind {
                for (slot, file) in samples.iter().enumerate() {
                    if file.as_ref().is_some_and(|f: &FileRef| f.path == path) {
                        out.push((here, d.id, slot));
                    }
                }
            }
        }
        for chain in d.chains() {
            if let Some(child) = here.child(d.id, chain.id) {
                samplers_using(&chain.devices, child, path, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::ChainId;
    use rlive_devices::write_wav;
    use rlive_engine::create_engine;
    use rlive_plugin_host::scanner::PluginCache;
    use rlive_project::Project;

    fn session() -> (rlive_engine::Engine, Session, TrackId) {
        let (engine, handle) = create_engine(48_000.0, 512);
        let mut p = Project::empty();
        let t = rlive_project::Track::new("T", TrackKind::Midi, [1, 2, 3], p.scenes.len());
        let id = t.id;
        p.tracks = vec![t];
        (engine, Session::new(handle, p, PluginCache::default()), id)
    }

    fn sample_file(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rlive-sampling-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.wav"));
        let mut left = vec![0.0f32; 96_000];
        for hit in 0..8 {
            let at = hit * 12_000;
            for i in 0..480 {
                left[at + i] = (1.0 - i as f32 / 480.0) * (i as f32 * 0.5).sin();
            }
        }
        write_wav(&path, &left, &left, 48_000).unwrap();
        path
    }

    fn with_simpler(s: &mut Session, chain: ChainRef, path: &Path) -> DeviceId {
        s.add_builtin(chain, "rlive.simpler");
        let id = s.devices(chain)[0].id;
        s.set_device_sample(chain, id, path.to_path_buf());
        s.samples.wait_for([path]);
        s.poll_samples();
        id
    }

    fn pads(s: &Session, chain: ChainRef, rack: DeviceId) -> Vec<(ChainId, Option<u8>)> {
        match &s.rack(chain, rack).unwrap().kind {
            DeviceKind::Rack { chains, .. } => chains
                .iter()
                .map(|c| (c.id, c.zones.drum.and_then(|p| p.receive)))
                .collect(),
            _ => panic!("not a rack"),
        }
    }

    #[test]
    fn slicing_to_a_drum_rack_replaces_the_simpler_with_one_pad_per_slice() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        let path = sample_file("to-rack");
        let id = with_simpler(&mut s, chain, &path);
        s.set_param(chain, id, si::MODE, si::MODE_SLICING as f64);
        s.set_param(chain, id, si::SLICE_BY, si::SLICE_REGION as f64);
        s.set_param(chain, id, si::REGIONS, 4.0);
        let rack = s.slice_to_drum_rack(chain, id).unwrap();
        assert_eq!(s.devices(chain).len(), 1, "the Simpler is gone");
        let found = pads(&s, chain, rack);
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].1, Some(FIRST_PAD));
        assert_eq!(found[3].1, Some(FIRST_PAD + 3));
        // Each pad's Simpler plays exactly its slice, as a one-shot.
        let at = chain.child(rack, found[2].0).unwrap();
        let d = &s.devices(at)[0];
        assert_eq!(type_of(d), Some("rlive.simpler"));
        assert_eq!(d.params.get(&si::MODE), Some(&(si::MODE_ONE_SHOT as f64)));
        assert!((d.params.get(&si::START_FLAG).unwrap() - 0.5).abs() < 1e-3);
        assert!((d.params.get(&si::END_FLAG).unwrap() - 0.75).abs() < 1e-3);
        assert_eq!(d.sample(0).map(|f| f.path.clone()), Some(path));
    }

    #[test]
    fn slicing_to_a_new_midi_track_keeps_the_simpler_and_writes_the_staircase() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        let path = sample_file("to-track");
        let id = with_simpler(&mut s, chain, &path);
        s.set_param(chain, id, si::MODE, si::MODE_SLICING as f64);
        s.set_param(chain, id, si::SLICE_BY, si::SLICE_REGION as f64);
        s.set_param(chain, id, si::REGIONS, 3.0);
        let track = s.slice_to_midi_track(chain, id).unwrap();
        assert_eq!(s.devices(chain).len(), 1, "the original stays");
        assert_ne!(track, t);
        let new = ChainRef::Track(track);
        let rack = s.devices(new)[0].id;
        assert_eq!(pads(&s, new, rack).len(), 3);
        let clip = s.clip(track, 0).expect("a clip that plays the slices");
        let keys: Vec<u8> = clip.notes().iter().map(|n| n.key).collect();
        assert_eq!(keys, [FIRST_PAD, FIRST_PAD + 1, FIRST_PAD + 2]);
    }

    #[test]
    fn a_simpler_converts_to_a_sampler_with_its_region_and_loop_and_back() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        let path = sample_file("convert");
        let id = with_simpler(&mut s, chain, &path);
        s.set_param(chain, id, si::START_FLAG, 0.25);
        s.set_param(chain, id, si::END_FLAG, 0.75);
        s.set_param(chain, id, si::LOOP_ON, 1.0);
        s.set_param(chain, id, si::LOOP, 50.0);
        s.set_param(chain, id, si::VOLUME, -6.0);
        let sampler = s.simpler_to_sampler(chain, id).unwrap();
        let d = &s.devices(chain)[0];
        assert_eq!(d.id, sampler);
        assert_eq!(type_of(d), Some("rlive.sampler"));
        let DeviceKind::Builtin { state, .. } = &d.kind else {
            panic!("a built-in");
        };
        let map = ZoneMap::parse(state.as_deref());
        let layer = &map.layers[0];
        assert_eq!(layer.sustain, 1, "the loop came along");
        assert_eq!(layer.start, Some(24_000.0));
        assert_eq!(layer.end, Some(72_000.0));
        assert_eq!(layer.loop_end, 72_000.0);
        assert_eq!(layer.loop_start, 48_000.0);
        assert_eq!(d.params.get(&sampler_old::VOLUME), Some(&-6.0));

        let back = s.sampler_to_simpler(chain, sampler).unwrap();
        let d = &s.devices(chain)[0];
        assert_eq!(d.id, back);
        assert_eq!(type_of(d), Some("rlive.simpler"));
        assert_eq!(d.params.get(&si::START_FLAG), Some(&0.25));
        assert_eq!(d.params.get(&si::END_FLAG), Some(&0.75));
        assert_eq!(d.params.get(&si::VOLUME), Some(&-6.0));
    }

    #[test]
    fn a_drum_sampler_converts_keeping_its_start_and_length() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        let path = sample_file("drum");
        s.add_builtin(chain, "rlive.drum_sampler");
        let id = s.devices(chain)[0].id;
        s.set_device_sample(chain, id, path.clone());
        s.samples.wait_for([path.as_path()]);
        s.poll_samples();
        s.set_param(chain, id, drum::START, 25.0);
        s.set_param(chain, id, drum::LENGTH, 50.0);
        let new = s.drum_sampler_to_simpler(chain, id).unwrap();
        let d = &s.devices(chain)[0];
        assert_eq!(d.id, new);
        assert_eq!(d.params.get(&si::START_FLAG), Some(&0.25));
        assert_eq!(d.params.get(&si::END_FLAG), Some(&0.75));
        assert_eq!(d.params.get(&si::MODE), Some(&(si::MODE_ONE_SHOT as f64)));
    }

    #[test]
    fn saving_a_device_state_keeps_it_and_undo_brings_it_back() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        s.add_builtin(chain, "rlive.sampler");
        let id = s.devices(chain)[0].id;
        let map = ZoneMap {
            layers: vec![Layer::new("A", 0)],
            ..ZoneMap::default()
        };
        s.set_device_state(chain, id, map.serialize());
        let stored = |s: &Session| match s.devices(chain).first().map(|d| &d.kind) {
            Some(DeviceKind::Builtin { state, .. }) => state.clone(),
            _ => None,
        };
        assert_eq!(ZoneMap::parse(stored(&s).as_deref()), map);
        s.undo();
        assert_eq!(stored(&s), None, "gone with the edit");
    }

    #[test]
    fn a_sample_finishing_loading_gives_a_simpler_its_analysis() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        let path = sample_file("analysis");
        let id = with_simpler(&mut s, chain, &path);
        assert_eq!(s.devices(chain)[0].id, id);
        s.poll_samples();
        // Nothing to assert on the audio thread here; the device tests cover what it does.
    }

    #[test]
    fn normalize_raises_gain_to_the_peak() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        let path = sample_file("normalize");
        let id = with_simpler(&mut s, chain, &path);
        let db = s.normalize_sample_gain(chain, id).unwrap();
        assert!(
            db.abs() < 1.0 && db > -1.0,
            "the test tone peaks near full scale: {db}"
        );
        assert_eq!(s.devices(chain)[0].params.get(&si::GAIN), Some(&db));
    }

    #[test]
    fn crop_writes_a_new_file_and_leaves_the_original() {
        let (_e, mut s, t) = session();
        let chain = ChainRef::Track(t);
        let path = sample_file("crop");
        let id = with_simpler(&mut s, chain, &path);
        s.set_param(chain, id, si::START_FLAG, 0.5);
        let out = s.crop_simpler_sample(chain, id).unwrap();
        assert!(path.exists(), "the original is untouched");
        assert!(out.exists());
        let cropped = rlive_devices::load_wav(&out).unwrap();
        assert_eq!(cropped.len(), 48_000);
    }
}
