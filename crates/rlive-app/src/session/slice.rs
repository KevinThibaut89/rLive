//! Slice to New MIDI Track: an audio clip becomes a Drum Rack of Simplers and a clip that plays
//! them in order (manual pp. 340-342).

use super::rack_edit::fresh_device;
use super::racks::FIRST_PAD;
use super::render::{from_tick, pre_fx_variant, Processed, Render, Rendered};
use super::{ChainRef, ClipRef, Session};
use rlive_audio_io::{BitDepth, FileType};
use rlive_browser::preset::{Payload, Preset};
use rlive_core::param::{ParamOwner, ParamRef};
use rlive_core::{ParamId, TrackId, PPQ};
use rlive_devices::devices::b10::simpler as si;
use rlive_project::warp_edit;
use rlive_project::{
    ArrangementClip, Clip, ClipContent, Device, DeviceKind, NoteData, RackKind, TrackKind,
};
use std::path::{Path, PathBuf};

/// The most chains a Rack can have.
pub const MAX_SLICES: usize = 128;

/// Where the slices are cut.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SliceBy {
    #[default]
    Transients,
    WarpMarkers,
    /// A fixed grid, in ticks.
    Grid(u64),
}

/// The Slicing dialog's choices.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SliceOptions {
    pub by: SliceBy,
    /// Whether the slices play as warped, rather than as the raw audio.
    pub preserve_warped: bool,
    /// A Slicing preset: a Drum Rack whose first Simpler and Macros every slice is built from.
    pub preset: Option<PathBuf>,
}

/// Where a clip gets cut.
#[derive(Debug, Clone, PartialEq)]
pub struct SlicePlan {
    /// Where each slice starts in the file, then where the last one ends, in seconds.
    pub seconds: Vec<f64>,
    /// Where each slice starts in the clip, then the clip's end, in ticks from the region start.
    pub ticks: Vec<u64>,
    /// The clip's region, in clip ticks.
    pub region: (u64, u64),
}

impl SlicePlan {
    pub fn count(&self) -> usize {
        self.seconds.len().saturating_sub(1)
    }
}

/// Macro parameters of the built-in Slicing setup.
const DEFAULT_MACROS: [(&str, u32); 4] = [
    ("Attack", si::A_ATTACK),
    ("Release", si::A_RELEASE),
    ("Volume", si::VOLUME),
    ("Transpose", si::TRANSPOSE),
];

fn param(device: rlive_core::DeviceId, id: u32) -> ParamRef {
    ParamRef {
        owner: ParamOwner::Device(device),
        id: ParamId(id),
    }
}

impl Session {
    /// Where the options would cut a clip, or why they can't.
    pub fn slice_plan(&mut self, r: ClipRef, by: SliceBy) -> Result<SlicePlan, String> {
        let clip = self.clip_at(r).ok_or("That clip is gone")?.clone();
        let ClipContent::Audio { file, warp, .. } = &clip.content else {
            return Err("Only audio clips can be sliced".into());
        };
        self.samples.wait_for([file.path.as_path()]);
        self.poll_samples();
        let sample = self
            .samples
            .get(&file.path)
            .ok_or("The clip's audio isn't available")?;
        let duration = sample.len() as f64 / sample.sample_rate.max(1.0);
        let (a, b) = if clip.looped {
            (clip.loop_start, clip.loop_end())
        } else {
            (clip.start, clip.end_marker())
        };
        let map = warp_edit::seconds_map(warp);
        let tempo = self.project.tempo;
        let tick_to_sec = |tick: f64| -> f64 {
            match &map {
                Some(m) => m.tick_to_src(tick),
                None => tick / PPQ as f64 * 60.0 / tempo,
            }
        };
        let sec_to_tick = |sec: f64| -> f64 {
            match &map {
                Some(m) => m.src_to_tick(sec),
                None => sec * tempo / 60.0 * PPQ as f64,
            }
        };
        let (first, last) = (tick_to_sec(a as f64), tick_to_sec(b as f64));
        let last = last.min(duration);
        if last <= first {
            return Err("The clip has no audio in its region".into());
        }
        // Cuts inside the region, in seconds of the file.
        let mut cuts: Vec<f64> = match by {
            SliceBy::Grid(step) => (1u64..)
                .map(|k| a + k * step.max(1))
                .take_while(|t| *t < b)
                .map(|t| tick_to_sec(t as f64))
                .collect(),
            SliceBy::WarpMarkers => warp
                .markers
                .iter()
                .map(|m| m.seconds)
                .filter(|s| *s > first && *s < last)
                .collect(),
            SliceBy::Transients => {
                let found = self.samples.transients(&file.path);
                let found = found.as_ref().map_or(&[][..], |f| f.as_slice());
                warp_edit::resolve_transients(found, &warp.transients)
                    .into_iter()
                    .filter(|s| *s > first && *s < last)
                    .collect()
            }
        };
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|x, y| (*x - *y).abs() < 1e-4);
        let mut seconds = Vec::with_capacity(cuts.len() + 2);
        seconds.push(first);
        seconds.extend(cuts);
        seconds.push(last);
        let ticks = seconds
            .iter()
            .map(|s| (sec_to_tick(*s) - a as f64).round().max(0.0) as u64)
            .collect();
        Ok(SlicePlan {
            seconds,
            ticks,
            region: (a, b),
        })
    }

    /// Slices a clip onto a new MIDI track. With Preserve Warped Timing on a warped clip the
    /// warped audio is rendered first, and the track arrives when that has finished: this returns
    /// `None` then. A plan of more than [`MAX_SLICES`] is refused.
    pub fn slice_to_new_track(
        &mut self,
        r: ClipRef,
        options: &SliceOptions,
    ) -> Result<Option<TrackId>, String> {
        let plan = self.slice_plan(r, options.by)?;
        if plan.count() > MAX_SLICES {
            return Err(format!(
                "Slicing would make {} slices, and a Rack holds {MAX_SLICES}",
                plan.count()
            ));
        }
        let clip = self.clip_at(r).ok_or("That clip is gone")?.clone();
        let name = clip.name.clone();
        if options.preserve_warped && clip.is_warped() {
            self.start_warped_slice(r, clip, plan, options.preset.clone())?;
            return Ok(None);
        }
        let ClipContent::Audio { file, .. } = &clip.content else {
            return Err("Only audio clips can be sliced".into());
        };
        let tempo = self.project.tempo;
        // Raw audio, played at the speed it was recorded.
        let at_tempo: Vec<u64> = plan
            .seconds
            .iter()
            .map(|s| ((s - plan.seconds[0]) * tempo / 60.0 * PPQ as f64).round() as u64)
            .collect();
        let track = self.build_slice_track(
            &name,
            &file.path,
            &plan.seconds,
            if clip.is_warped() {
                &at_tempo
            } else {
                &plan.ticks
            },
            options.preset.as_deref(),
        )?;
        Ok(Some(track))
    }

    fn start_warped_slice(
        &mut self,
        r: ClipRef,
        clip: Clip,
        plan: SlicePlan,
        preset: Option<PathBuf>,
    ) -> Result<(), String> {
        self.prepare_render();
        let (a, b) = plan.region;
        let track = r.track();
        let mut v = pre_fx_variant(&self.project, &[track]);
        let mut only = clip.clone();
        only.looped = false;
        only.start = a;
        only.loop_start = a;
        only.length = b - a;
        only.end = Some(b);
        only.frozen = None;
        for t in &mut v.tracks {
            t.arrangement.clear();
            t.clips.iter_mut().for_each(|c| *c = None);
        }
        let name = clip.name.clone();
        if let Some(t) = v.track_mut(track) {
            t.arrangement.push(ArrangementClip::new(only, 0));
        }
        let render = Render {
            project: v,
            setup: from_tick(0),
            start: 0,
            ticks: b - a,
            tail: false,
            rate: None,
        };
        let ticks = plan.ticks.clone();
        self.start_render_job(
            "Slice",
            vec![render],
            false,
            Box::new(move |s, audio: Vec<Rendered>| {
                let audio = audio.first().ok_or("The render went missing")?;
                let path = s.write_processed(
                    Processed::Consolidate,
                    &name,
                    audio,
                    (FileType::Wav, BitDepth::Bits24),
                )?;
                let seconds_per_tick = 60.0 / s.project.tempo / PPQ as f64;
                let seconds: Vec<f64> =
                    ticks.iter().map(|t| *t as f64 * seconds_per_tick).collect();
                s.build_slice_track(&name, &path, &seconds, &ticks, preset.as_deref())?;
                Ok(())
            }),
        )?;
        Ok(())
    }

    /// The new track: one note, chain and Simpler for each slice, with the Macros mapped.
    fn build_slice_track(
        &mut self,
        name: &str,
        path: &Path,
        seconds: &[f64],
        ticks: &[u64],
        preset: Option<&Path>,
    ) -> Result<TrackId, String> {
        self.samples.wait_for([path]);
        self.poll_samples();
        let sample = self
            .samples
            .get(path)
            .ok_or("The sliced audio isn't available")?;
        let duration = (sample.len() as f64 / sample.sample_rate.max(1.0)).max(1e-9);
        let preset_device = preset
            .map(|p| Preset::load(p).map_err(|e| e.to_string()))
            .transpose()?
            .and_then(|p| match p.payload {
                Payload::Device(d) => Some(*d),
                _ => None,
            });
        let template = preset_device
            .as_ref()
            .and_then(|d| d.chains().next().map(|c| c.devices.clone()))
            .and_then(|devices| devices.into_iter().find(is_simpler));
        let template_macros: Vec<(usize, String, u32)> = match (&preset_device, &template) {
            (
                Some(Device {
                    kind: DeviceKind::Rack { macros, .. },
                    ..
                }),
                Some(t),
            ) => macros
                .iter()
                .enumerate()
                .flat_map(|(n, m)| {
                    m.mappings
                        .iter()
                        .filter(|map| map.target.owner == ParamOwner::Device(t.id))
                        .map(move |map| (n, m.name.clone(), map.target.id.0))
                })
                .collect(),
            _ => DEFAULT_MACROS
                .iter()
                .enumerate()
                .map(|(n, (name, id))| (n, name.to_string(), *id))
                .collect(),
        };
        let count = seconds.len() - 1;
        let track = self.add_track(TrackKind::Midi, None);
        let chain = ChainRef::Track(track);
        while let Some(existing) = self.devices(chain).first().map(|d| d.id) {
            self.remove_device(chain, existing);
        }
        let rack = self
            .add_rack(chain, RackKind::Drum, name)
            .ok_or("Could not make the Drum Rack")?;
        let base = FIRST_PAD.min(128usize.saturating_sub(count) as u8);
        let mut simplers = Vec::with_capacity(count);
        for i in 0..count {
            let note = base + i as u8;
            let label = format!("{name} {}", i + 1);
            let pad = self
                .new_pad_chain(chain, rack, note, &label)
                .ok_or("Could not make a chain")?;
            let at = chain.child(rack, pad).ok_or("Could not reach the chain")?;
            let mut device = match &template {
                Some(t) => fresh_device(t),
                None => Device::builtin("rlive.simpler", "Simpler"),
            };
            device.params.insert(si::MODE, si::MODE_ONE_SHOT as f64);
            device.params.insert(si::START_FLAG, seconds[i] / duration);
            device
                .params
                .insert(si::END_FLAG, (seconds[i + 1] / duration).min(1.0));
            if let DeviceKind::Builtin { samples, .. } = &mut device.kind {
                *samples = vec![Some(rlive_project::FileRef::new(path))];
            }
            let id = device.id;
            self.add_device(at, None, device);
            simplers.push(id);
        }
        for (n, label, param_id) in template_macros {
            for id in &simplers {
                self.map_macro(chain, rack, n, param(*id, param_id));
            }
            if let Some(m) = self.macros_mut(chain, rack).and_then(|m| m.get_mut(n)) {
                m.name = label;
            }
        }
        let last_tick = ticks.last().copied().unwrap_or(0).max(1);
        let sig = self.project.time_signature;
        let bars = last_tick.div_ceil(sig.ticks_per_bar()).max(1);
        let color = self.new_clip_color(track);
        let mut clip = Clip::new_midi(name, color, bars, sig);
        if let Some(notes) = clip.notes_mut() {
            for i in 0..count {
                let (a, b) = (ticks[i], ticks[i + 1].max(ticks[i] + 1));
                notes.push(NoteData::new(a, b - a, base + i as u8, 1.0));
            }
        }
        self.set_clip(track, 0, Some(clip));
        if let Some(t) = self.project.track_mut(track) {
            t.name = format!("Sliced {name}");
        }
        self.mark_edit("Slice to New MIDI Track");
        Ok(track)
    }
}

fn is_simpler(d: &Device) -> bool {
    matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.simpler")
}
