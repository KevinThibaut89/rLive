//! Samples arriving from the loading threads.
//!
//! The project refers to audio by path; the engine can only be handed audio that has finished
//! loading. Until then a clip is simply absent from the engine and a device has no sample, so
//! nothing can play half-loaded data. When a file finishes, whatever is waiting for it is built
//! and sent through the usual lock-free commands, which retire what they replace as
//! [`Garbage`](rlive_engine::Garbage).

use super::{ChainRef, Session};
use rlive_audio_io::{Cleanup, Fingerprint, SampleEvent};
use rlive_core::{DeviceId, TrackId};
use rlive_engine::GraphOp;
use rlive_project::{ClipContent, Device, DeviceKind, FileRef};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A change of sample that is waiting for the file to finish loading.
pub(super) struct PendingSample {
    chain: ChainRef,
    device: DeviceId,
    slot: usize,
    file: FileRef,
}

impl Session {
    /// Hands what has finished loading to whatever is waiting for it. Called every UI frame.
    pub(super) fn poll_samples(&mut self) {
        for event in self.samples.poll() {
            self.sample_finished(event);
        }
    }

    fn sample_finished(&mut self, event: SampleEvent) {
        let SampleEvent {
            path,
            result,
            notices,
        } = event;
        self.messages.extend(notices);
        match result {
            Ok(sample) => {
                self.record_fingerprint(&path);
                self.swap_in_sample(&path, &sample);
                for p in self.take_pending_samples(&path) {
                    if self.write_device_sample(p.chain, p.device, p.slot, p.file) {
                        self.mark_edit("Change Sample");
                    }
                }
            }
            Err(reason) => {
                log::warn!("{reason}");
                self.note_sample_failed(&path, &reason);
                self.messages.push(reason);
            }
        }
    }

    /// Finishes an event without touching the engine, for when the engine is rebuilt next and
    /// reports any failure itself.
    fn note_sample_finished(&mut self, event: SampleEvent) {
        self.messages.extend(event.notices);
        if event.result.is_ok() {
            self.record_fingerprint(&event.path);
        }
    }

    /// Loads everything the project refers to before going on, for a session being created with
    /// a project already in hand: there is no UI to keep responsive yet. Opening a project once
    /// the application is running goes through [`load`](Self::load), which doesn't wait.
    pub(super) fn preload_samples(&mut self) {
        let paths = self.project_audio_paths();
        self.samples.wait_for(paths.iter().map(PathBuf::as_path));
        for event in self.samples.poll() {
            self.note_sample_finished(event);
        }
    }

    /// Every audio file the project refers to.
    pub(super) fn project_audio_paths(&self) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        self.project.for_each_file_ref(&mut |file: &FileRef| {
            seen.insert(file.path.clone());
        });
        seen.into_iter().collect()
    }

    /// Fills in the size and hash of every reference to `path`.
    fn record_fingerprint(&mut self, path: &Path) {
        if let Some(fp) = self.samples.fingerprint(path).cloned() {
            self.project
                .for_each_file_ref_mut(&mut |file: &mut FileRef| {
                    if file.path == path {
                        fill_file_ref(file, &fp);
                    }
                });
        }
    }

    /// Fills in the size and hash of every reference whose file has loaded.
    pub(super) fn record_fingerprints(&mut self) {
        let samples = &self.samples;
        self.project
            .for_each_file_ref_mut(&mut |file: &mut FileRef| {
                if let Some(fp) = samples.fingerprint(&file.path) {
                    fill_file_ref(file, fp);
                }
            });
    }

    /// Sends a finished sample to everything in the engine that was waiting for it.
    fn swap_in_sample(&mut self, path: &Path, sample: &Arc<rlive_core::SampleBuffer>) {
        let mut slots = Vec::new();
        let mut lanes = Vec::new();
        let mut ops = Vec::new();
        for t in self.project.tracks.iter().chain(&self.project.returns) {
            for (slot, clip) in t.clips.iter().enumerate() {
                if clip.as_ref().is_some_and(|c| uses(&c.content, path)) {
                    slots.push((t.id, slot));
                }
            }
            if t.arrangement.iter().any(|c| uses(&c.clip.content, path)) {
                lanes.push(t.id);
            }
            device_sample_ops(&t.devices, ChainRef::Track(t.id), path, sample, &mut ops);
        }
        device_sample_ops(
            &self.project.master.devices,
            ChainRef::Master,
            path,
            sample,
            &mut ops,
        );
        for (track, slot) in slots {
            self.send_clip(track, slot);
        }
        for track in lanes {
            self.sync_lane(track);
        }
        if !ops.is_empty() {
            self.send_edits(ops);
        }
    }

    fn take_pending_samples(&mut self, path: &Path) -> Vec<PendingSample> {
        let (done, waiting) = std::mem::take(&mut self.pending_samples)
            .into_iter()
            .partition(|p| p.file.path == path);
        self.pending_samples = waiting;
        done
    }

    fn note_sample_failed(&mut self, path: &Path, reason: &str) {
        for p in self.take_pending_samples(path) {
            if let Some(rt) = self.runtimes.get_mut(&p.device) {
                rt.error = Some(reason.to_string());
            }
        }
        let mut affected = Vec::new();
        for t in self.project.tracks.iter().chain(&self.project.returns) {
            devices_using(&t.devices, path, &mut affected);
        }
        devices_using(&self.project.master.devices, path, &mut affected);
        for id in affected {
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.error = Some(reason.to_string());
            }
        }
    }

    /// Changes a device's sample once its file has finished loading.
    pub(super) fn queue_device_sample(
        &mut self,
        chain: ChainRef,
        device: DeviceId,
        slot: usize,
        file: FileRef,
    ) {
        self.pending_samples.push(PendingSample {
            chain,
            device,
            slot,
            file,
        });
    }

    /// Whether the clip in this slot exists but its audio can't be played yet.
    pub fn clip_waiting_for_sample(&self, track: TrackId, slot: usize) -> bool {
        self.clip(track, slot)
            .is_some_and(|c| self.content_waiting_for_sample(&c.content))
    }

    /// Whether this content needs a sample that hasn't finished loading or failed to.
    pub fn content_waiting_for_sample(&self, content: &ClipContent) -> bool {
        match content {
            ClipContent::Audio { file, .. } => !self.samples.is_ready(&file.path),
            ClipContent::Midi { .. } => false,
        }
    }

    /// Deletes every decoded file not used by this project.
    pub fn cleanup_decoding_cache(&mut self) -> Cleanup {
        let mut in_use = HashSet::new();
        self.project.for_each_file_ref(&mut |file: &FileRef| {
            if let Some(hash) = &file.hash {
                in_use.insert(hash.clone());
            }
        });
        self.samples.cleanup_decoding_cache(&in_use)
    }
}

fn fill_file_ref(file: &mut FileRef, fp: &Fingerprint) {
    file.size = Some(fp.size);
    file.hash = Some(fp.hash.clone());
}

fn uses(content: &ClipContent, path: &Path) -> bool {
    matches!(content, ClipContent::Audio { file, .. } if file.path == path)
}

fn sample_slots(device: &Device, path: &Path) -> Vec<u32> {
    let DeviceKind::Builtin { samples, .. } = &device.kind else {
        return Vec::new();
    };
    samples
        .iter()
        .enumerate()
        .filter(|(_, file)| file.as_ref().is_some_and(|f| f.path == path))
        .map(|(slot, _)| slot as u32)
        .collect()
}

/// A [`GraphOp::SetResource`] for every sample slot of `devices`, however deep, that holds `path`.
fn device_sample_ops(
    devices: &[Device],
    here: ChainRef,
    path: &Path,
    sample: &Arc<rlive_core::SampleBuffer>,
    ops: &mut Vec<GraphOp>,
) {
    for d in devices {
        for slot in sample_slots(d, path) {
            ops.push(GraphOp::SetResource {
                target: here.into(),
                device: d.id,
                slot,
                resource: sample.clone(),
            });
        }
        for chain in d.chains() {
            if let Some(child) = here.child(d.id, chain.id) {
                device_sample_ops(&chain.devices, child, path, sample, ops);
            }
        }
    }
}

fn devices_using(devices: &[Device], path: &Path, out: &mut Vec<DeviceId>) {
    for d in devices {
        if !sample_slots(d, path).is_empty() {
            out.push(d.id);
        }
        for chain in d.chains() {
            devices_using(&chain.devices, path, out);
        }
    }
}
