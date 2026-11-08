//! Scenes and clips in Session slots.

use super::Session;
use crate::settings::ClipColor;
use rlive_audio_io::SampleStatus;
use rlive_core::{ArrClipId, TrackId};
use rlive_engine::{ClipCmd, EngineCommand};
use rlive_project::{Clip, ClipContent};
use std::path::Path;

/// A clip being edited: in a Session slot or on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipRef {
    Slot { track: TrackId, slot: usize },
    Arr { track: TrackId, id: ArrClipId },
}

impl ClipRef {
    pub fn slot(track: TrackId, slot: usize) -> Self {
        Self::Slot { track, slot }
    }

    pub fn track(self) -> TrackId {
        match self {
            Self::Slot { track, .. } | Self::Arr { track, .. } => track,
        }
    }

    /// The track and slot, if this clip is in a Session slot.
    pub fn as_slot(self) -> Option<(TrackId, usize)> {
        match self {
            Self::Slot { track, slot } => Some((track, slot)),
            Self::Arr { .. } => None,
        }
    }
}

impl Session {
    // ------------------------------------------------------------- scenes & clips

    pub fn add_scene(&mut self) {
        self.project.add_scene();
        self.mark_edit("Add Scene");
    }

    pub fn delete_scene(&mut self, index: usize) {
        if index >= self.project.scenes.len() || self.project.scenes.len() <= 1 {
            return;
        }
        self.project.scenes.remove(index);
        for t in self.project.tracks.iter_mut() {
            if index < t.clips.len() {
                t.clips.remove(index);
            }
        }
        self.project.normalize();
        // Slot indices shifted: resend all clips of every track.
        self.resend_all_clips();
        self.mark_edit("Delete Scene");
    }

    fn resend_all_clips(&mut self) {
        let ids: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for id in ids {
            let n = self.project.scenes.len() + 1;
            for slot in 0..n {
                self.sync_clip(id, slot);
            }
        }
    }

    /// Replace the clip in a slot (or clear it with `None`).
    pub fn set_clip(&mut self, track: TrackId, slot: usize, clip: Option<Clip>) {
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        if slot >= t.clips.len() {
            return;
        }
        t.clips[slot] = clip;
        self.sync_clip(track, slot);
        self.mark_edit("Edit Clip");
    }

    /// Recompile a slot's clip and send it to the engine (call after editing notes).
    pub fn sync_clip(&mut self, track: TrackId, slot: usize) {
        self.send_clip(track, slot);
        self.mark_edit("Edit Clip");
    }

    /// Sends a slot's clip to the engine as the project has it, without recording an edit.
    pub(super) fn send_clip(&mut self, track: TrackId, slot: usize) {
        let clip = self
            .project
            .track(track)
            .and_then(|t| t.clips.get(slot))
            .cloned()
            .flatten();
        let (mut b, _) = self.builder();
        let data = clip.and_then(|c| b.clip(&c));
        self.send(EngineCommand::Clip(ClipCmd::SetClip {
            track,
            slot,
            clip: data,
        }));
        self.sync_clip_routes(track);
    }

    pub fn clip(&self, track: TrackId, slot: usize) -> Option<&Clip> {
        self.project.track(track)?.clips.get(slot)?.as_ref()
    }

    pub fn clip_mut(&mut self, track: TrackId, slot: usize) -> Option<&mut Clip> {
        self.project.track_mut(track)?.clips.get_mut(slot)?.as_mut()
    }

    pub fn clip_at(&self, r: ClipRef) -> Option<&Clip> {
        match r {
            ClipRef::Slot { track, slot } => self.clip(track, slot),
            ClipRef::Arr { track, id } => Some(&self.project.track(track)?.arr_clip(id)?.clip),
        }
    }

    pub fn clip_at_mut(&mut self, r: ClipRef) -> Option<&mut Clip> {
        match r {
            ClipRef::Slot { track, slot } => self.clip_mut(track, slot),
            ClipRef::Arr { track, id } => {
                Some(&mut self.project.track_mut(track)?.arr_clip_mut(id)?.clip)
            }
        }
    }

    /// Sends an edited clip to the engine.
    pub fn sync_ref(&mut self, r: ClipRef) {
        match r {
            ClipRef::Slot { track, slot } => self.sync_clip(track, slot),
            ClipRef::Arr { track, .. } => {
                self.sync_lane(track);
                self.mark_edit("Edit Clip");
            }
        }
    }

    /// The colour for a new clip on `track`.
    pub fn new_clip_color(&self, track: TrackId) -> [u8; 3] {
        match self.colors.new_clip {
            ClipColor::Track => self
                .project
                .track(track)
                .map_or_else(|| self.project.next_color(), |t| t.color),
            ClipColor::Random => self.project.next_color(),
        }
    }

    /// Creates an audio clip from a file, and starts loading the file if nothing has yet. Its
    /// loop length follows the file's length at the current tempo, rounded to beats. The length
    /// comes from the file's header, which is all this reads; a clip made before its audio has
    /// loaded can't play until it has.
    pub fn audio_clip_from_file(&mut self, path: &Path, track: TrackId) -> Result<Clip, String> {
        // Asking for a file again is a reason to look at it again.
        if matches!(self.samples.status(path), Some(SampleStatus::Failed(_))) {
            self.samples.forget(path);
        }
        let seconds = match self.samples.get(path) {
            Some(s) => s.duration_secs(),
            None => {
                let info =
                    rlive_audio_io::probe(path).map_err(|e| format!("{}: {e}", path.display()))?;
                self.samples.request(path);
                // A format that doesn't state its length gets a one-beat clip.
                info.duration_secs().unwrap_or(0.0)
            }
        };
        let beats = (seconds * self.project.tempo / 60.0).round().max(1.0);
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut clip = Clip {
            name,
            color: self.new_clip_color(track),
            length: (beats as u64) * rlive_core::PPQ,
            looped: true,
            content: ClipContent::audio(path, 0.0),
            ..Clip::default()
        };
        self.apply_new_clip_defaults(&mut clip, path, seconds);
        Ok(clip)
    }
}
