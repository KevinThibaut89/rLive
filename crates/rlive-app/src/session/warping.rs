//! Warping clips: markers, transients, modes and the settings of the Sample tab.
//!
//! Edits go through [`rlive_project::warp_edit`]; this file decides which clips they apply to,
//! records the undo step and sends the result to the engine.

use super::{ClipRef, Session};
use rlive_project::clip_envelope;
use rlive_project::warp_edit::{self, WarpContext, WarpOp};
use rlive_project::{Clip, ClipContent, Warp, WarpMode, WarpParams};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Files at least this long count as long samples, which A10's auto-warp handles. The manual gives
/// no figure; this is long enough for 16 bars at any ordinary tempo.
pub const LONG_SAMPLE_SECONDS: f64 = 60.0;
/// A short sample under this length with Auto selected is a one-shot, not a loop.
const AUTO_ONE_SHOT_SECONDS: f64 = 1.0;

/// What a sample dropped into the Set becomes, under the Loop/Warp Short Samples setting.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ShortSampleMode {
    #[default]
    Auto,
    UnwarpedOneShot,
    WarpedOneShot,
    WarpedLoop,
}

impl ShortSampleMode {
    pub const ALL: [ShortSampleMode; 4] = [
        Self::Auto,
        Self::UnwarpedOneShot,
        Self::WarpedOneShot,
        Self::WarpedLoop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::UnwarpedOneShot => "Unwarped One Shot",
            Self::WarpedOneShot => "Warped One Shot",
            Self::WarpedLoop => "Warped Loop",
        }
    }
}

/// The Warp section of the Record, Warp & Launch settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WarpDefaults {
    pub short_samples: ShortSampleMode,
    pub mode: WarpMode,
}

/// The clip settings saved with a sample, for new clips made from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedClipSettings {
    pub warp: Warp,
    pub transpose: i32,
    pub detune_cents: f32,
    pub gain_db: f32,
    pub length: u64,
    pub looped: bool,
}

/// What the Sample tab shows of a clip.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpView {
    pub warp: Warp,
    /// Length of the audio file.
    pub seconds: f64,
    /// The clip's transients, in seconds.
    pub transients: Vec<f64>,
    pub transpose: i32,
    pub detune_cents: f32,
    pub gain_db: f32,
}

impl Session {
    /// How long a clip's file is, from whatever has been learned about it so far.
    fn file_seconds(&self, path: &Path) -> Option<f64> {
        if let Some(s) = self.samples.get(path) {
            return Some(s.duration_secs());
        }
        let (peaks, rate) = self.samples.peaks(path)?;
        (rate > 0.0).then(|| peaks.frames() as f64 / rate)
    }

    /// How long the file of an audio clip is.
    pub(super) fn file_seconds_of(&self, clip: &Clip) -> Option<f64> {
        self.warp_context_seconds(clip)
    }

    /// How long a file is, by asking it if nothing has loaded it yet.
    pub(super) fn probe_seconds(&self, path: &Path) -> Option<f64> {
        self.file_seconds(path)
            .or_else(|| rlive_audio_io::probe(path).ok()?.duration_secs())
    }

    fn warp_context_seconds(&self, clip: &Clip) -> Option<f64> {
        match &clip.content {
            ClipContent::Audio { file, .. } => self.file_seconds(&file.path),
            ClipContent::Midi { .. } => None,
        }
    }

    fn clip_transients(&self, path: &Path, warp: &Warp) -> Vec<f64> {
        let found = self.samples.transients(path);
        let found = found.as_ref().map_or(&[][..], |f| f.as_slice());
        warp_edit::resolve_transients(found, &warp.transients)
    }

    /// Everything the Sample tab needs of an audio clip whose file has been read.
    pub fn warp_view(&self, r: ClipRef) -> Option<WarpView> {
        let clip = self.clip_at(r)?;
        let ClipContent::Audio {
            file,
            gain_db,
            transpose,
            detune_cents,
            warp,
            ..
        } = &clip.content
        else {
            return None;
        };
        let seconds = self.file_seconds(&file.path)?;
        Some(WarpView {
            warp: warp.clone(),
            seconds,
            transients: self.clip_transients(&file.path, warp),
            transpose: *transpose,
            detune_cents: *detune_cents,
            gain_db: *gain_db,
        })
    }

    /// The clips an edit of `primary` also applies to: the selected ones of its length playing a
    /// file of its length, it first.
    pub fn warp_group(&self, primary: ClipRef, selected: &[ClipRef]) -> Vec<ClipRef> {
        let mut group = vec![primary];
        let Some(clip) = self.clip_at(primary) else {
            return group;
        };
        let (length, seconds) = (clip.length, self.warp_context_seconds(clip));
        for r in selected {
            if *r == primary {
                continue;
            }
            let matches = self.clip_at(*r).is_some_and(|c| {
                c.length == length && seconds.is_some() && self.warp_context_seconds(c) == seconds
            });
            if matches {
                group.push(*r);
            }
        }
        group
    }

    /// Runs `edit` on each clip's warp settings, sends the clips that changed and records one
    /// undo step for them all.
    pub(super) fn edit_warp(
        &mut self,
        targets: &[ClipRef],
        label: &'static str,
        mut edit: impl FnMut(&mut Session, ClipRef, &mut Clip) -> bool,
    ) -> bool {
        let mut changed = Vec::new();
        for r in targets {
            let Some(mut clip) = self.clip_at(*r).cloned() else {
                continue;
            };
            if edit(self, *r, &mut clip) {
                if let Some(slot) = self.clip_at_mut(*r) {
                    *slot = clip;
                    changed.push(*r);
                }
            }
        }
        if !changed.is_empty() {
            self.mark_edit(label);
        }
        for r in &changed {
            self.sync_ref(*r);
        }
        !changed.is_empty()
    }

    /// Applies a marker or transient edit to every clip in `targets`.
    pub fn apply_warp_op(&mut self, targets: &[ClipRef], op: &WarpOp) -> bool {
        let label = match op {
            WarpOp::AddTransient { .. }
            | WarpOp::DeleteTransient { .. }
            | WarpOp::MoveTransient { .. }
            | WarpOp::ResetTransients => "Edit Transients",
            WarpOp::Quantize { .. } => "Quantize",
            _ => "Edit Warp Markers",
        };
        let ticks_per_bar = self.project.time_signature.ticks_per_bar();
        let set_bpm = self.project.tempo;
        self.edit_warp(targets, label, |s, _, clip| {
            let Some(seconds) = s.warp_context_seconds(clip) else {
                return false;
            };
            let mut length = clip.length;
            let ClipContent::Audio { file, warp, .. } = &mut clip.content else {
                return false;
            };
            let transients = s.clip_transients(&file.path, warp);
            let cx = WarpContext {
                seconds,
                ticks_per_bar,
                set_bpm,
                transients: &transients,
            };
            let markers = |w: &Warp| -> Vec<(f64, f64)> {
                w.markers.iter().map(|m| (m.seconds, m.tick)).collect()
            };
            let before = markers(warp);
            let changed = warp_edit::apply(warp, op, &cx, &mut length);
            let after = markers(warp);
            clip.length = length;
            // Linked envelopes stay over the audio they were drawn on.
            clip_envelope::retime_linked(&mut clip.envelopes, &before, &after);
            changed
        })
    }

    /// Turns warping on or off. A clip being warped for the first time is assumed to be a well-cut
    /// loop of 1, 2, 4, 8 or 16 bars.
    pub fn set_warp_enabled(&mut self, targets: &[ClipRef], on: bool) -> bool {
        let ticks_per_bar = self.project.time_signature.ticks_per_bar();
        let set_bpm = self.project.tempo;
        self.edit_warp(targets, "Warp", |s, _, clip| {
            let seconds = s.warp_context_seconds(clip);
            let mut length = clip.length;
            let ClipContent::Audio { warp, .. } = &mut clip.content else {
                return false;
            };
            if warp.enabled == on {
                return false;
            }
            warp.enabled = on;
            if on && warp.markers.len() < 2 {
                if let Some(seconds) = seconds.filter(|s| *s > 0.0) {
                    let bars = warp_edit::suggest_bars(seconds, ticks_per_bar, set_bpm);
                    warp.markers = warp_edit::loop_markers(seconds, bars, ticks_per_bar);
                    length = bars as u64 * ticks_per_bar;
                }
            }
            clip.length = length;
            true
        })
    }

    pub fn set_warp_mode(&mut self, targets: &[ClipRef], mode: WarpMode) -> bool {
        self.edit_warp(targets, "Warp Mode", |_, _, clip| match &mut clip.content {
            ClipContent::Audio { warp, .. } if warp.mode != mode => {
                warp.mode = mode;
                true
            }
            _ => false,
        })
    }

    pub fn set_warp_params(&mut self, targets: &[ClipRef], params: WarpParams) -> bool {
        self.edit_warp(targets, "Warp Settings", |_, _, clip| {
            match &mut clip.content {
                ClipContent::Audio { warp, .. } if warp.params != params => {
                    warp.params = params;
                    true
                }
                _ => false,
            }
        })
    }

    pub fn set_clip_pitch(&mut self, targets: &[ClipRef], semitones: i32, cents: f32) -> bool {
        self.edit_warp(targets, "Clip Pitch", |_, _, clip| {
            match &mut clip.content {
                ClipContent::Audio {
                    transpose,
                    detune_cents,
                    ..
                } if *transpose != semitones || *detune_cents != cents => {
                    *transpose = semitones;
                    *detune_cents = cents;
                    true
                }
                _ => false,
            }
        })
    }

    pub fn set_clip_gain(&mut self, targets: &[ClipRef], db: f32) -> bool {
        self.edit_warp(targets, "Clip Gain", |_, _, clip| match &mut clip.content {
            ClipContent::Audio { gain_db, .. } if *gain_db != db => {
                *gain_db = db;
                true
            }
            _ => false,
        })
    }

    /// Stores a clip's settings with its sample, so new clips of the file start from them.
    pub fn save_clip_with_sample(&mut self, r: ClipRef) -> Result<(), String> {
        let clip = self.clip_at(r).ok_or("no clip selected")?;
        let ClipContent::Audio {
            file,
            gain_db,
            transpose,
            detune_cents,
            warp,
            ..
        } = &clip.content
        else {
            return Err("only an audio clip can be saved with its sample".into());
        };
        let saved = SavedClipSettings {
            warp: warp.clone(),
            transpose: *transpose,
            detune_cents: *detune_cents,
            gain_db: *gain_db,
            length: clip.length,
            looped: clip.looped,
        };
        let path = file.path.clone();
        let value = serde_json::to_value(saved).map_err(|e| e.to_string())?;
        self.samples.save_default_clip(&path, value)
    }

    /// Gives a new clip of `path` what its file has saved, or what the settings say a sample of
    /// its length should be.
    pub(super) fn apply_new_clip_defaults(&self, clip: &mut Clip, path: &Path, seconds: f64) {
        let saved = rlive_audio_io::analysis::default_clip_of(path)
            .and_then(|v| serde_json::from_value::<SavedClipSettings>(v).ok());
        let ClipContent::Audio {
            gain_db,
            transpose,
            detune_cents,
            warp,
            ..
        } = &mut clip.content
        else {
            return;
        };
        if let Some(s) = saved {
            *gain_db = s.gain_db;
            *transpose = s.transpose;
            *detune_cents = s.detune_cents;
            *warp = s.warp;
            clip.length = s.length;
            clip.looped = s.looped;
            return;
        }
        // Long samples are for auto-warp, which comes with beat tracking in A10.
        if seconds <= 0.0 || seconds >= LONG_SAMPLE_SECONDS {
            return;
        }
        warp.mode = self.warp_defaults.mode;
        let short = match self.warp_defaults.short_samples {
            ShortSampleMode::Auto if seconds < AUTO_ONE_SHOT_SECONDS => {
                ShortSampleMode::UnwarpedOneShot
            }
            ShortSampleMode::Auto => ShortSampleMode::WarpedLoop,
            other => other,
        };
        match short {
            ShortSampleMode::UnwarpedOneShot => clip.looped = false,
            ShortSampleMode::WarpedOneShot | ShortSampleMode::WarpedLoop => {
                let ticks_per_bar = self.project.time_signature.ticks_per_bar();
                let bars = warp_edit::suggest_bars(seconds, ticks_per_bar, self.project.tempo);
                warp.enabled = true;
                warp.markers = warp_edit::loop_markers(seconds, bars, ticks_per_bar);
                clip.length = bars as u64 * ticks_per_bar;
                clip.looped = short == ShortSampleMode::WarpedLoop;
            }
            ShortSampleMode::Auto => {}
        }
    }
}
