//! Clip View edits: properties, markers and notes, on one clip or a multi-clip selection.
//!
//! Each edit runs on a copy of each clip, keeps the copies that changed and records one undo step
//! for them all. The arithmetic is in [`rlive_project::clip_edit`].

use super::{ClipRef, Session};
use rlive_core::{GrooveId, TimeSignature, TrackId, PPQ};
use rlive_devices::devices::b1::common::SCALES;
use rlive_engine::{ClipCmd, EngineCommand, LaunchCmd};
use rlive_project::clip_edit::{self as edit, merged, PitchScale};
use rlive_project::{Clip, ClipContent, ClipScale, Groove, MidiProgram, NoteData};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

/// The Launch and Fade options that shape new clips, and how edits reach a running one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipDefaults {
    /// New audio clips get short fades at their edges.
    pub create_fades: bool,
    /// Clip Update Rate, in ticks: edits reach a running clip on this grid. 0 is at once.
    pub update_rate: u64,
}

impl Default for ClipDefaults {
    fn default() -> Self {
        Self {
            create_fades: true,
            update_rate: 0,
        }
    }
}

/// Which marker a Set button puts at the playhead (manual p. 208).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetMarker {
    Start,
    End,
    LoopPosition,
    LoopLength,
}

/// What a multi-clip selection has in common: each property is the lowest and highest of it, so
/// one value means they agree. `None` for what doesn't apply to all of them (manual p. 232).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MultiClip {
    pub count: usize,
    pub all_audio: bool,
    pub all_midi: bool,
    pub active: Option<(bool, bool)>,
    pub looped: Option<(bool, bool)>,
    pub gain_db: Option<(f32, f32)>,
    pub transpose: Option<(i32, i32)>,
    pub detune_cents: Option<(f32, f32)>,
    pub ram: Option<(bool, bool)>,
    pub hi_q: Option<(bool, bool)>,
    pub edge_fade: Option<(bool, bool)>,
    pub warp: Option<(bool, bool)>,
}

/// One audio clip's editable numbers, for what the selection shares.
struct AudioValues {
    gain_db: f32,
    transpose: i32,
    detune_cents: f32,
    ram: bool,
    hi_q: bool,
    edge_fade: bool,
    warp: bool,
}

fn audio_values(c: &Clip) -> Option<AudioValues> {
    match &c.content {
        ClipContent::Audio {
            gain_db,
            transpose,
            detune_cents,
            ram,
            hi_q,
            edge_fade,
            warp,
            ..
        } => Some(AudioValues {
            gain_db: *gain_db,
            transpose: *transpose,
            detune_cents: *detune_cents,
            ram: *ram,
            hi_q: *hi_q,
            edge_fade: *edge_fade,
            warp: warp.enabled,
        }),
        ClipContent::Midi { .. } => None,
    }
}

impl Session {
    /// Clip Update Rate, in ticks. Tells the engine only when it has changed.
    pub fn set_clip_update_rate(&mut self, ticks: u64) {
        if self.clip_defaults.update_rate != ticks {
            self.clip_defaults.update_rate = ticks;
            self.send(EngineCommand::Clip(ClipCmd::SetUpdateRate(ticks)));
        }
    }

    /// Runs `edit` on each clip, sends those that changed and records one undo step for them all.
    /// Moving a timeline clip's start marker moves its left edge with it.
    pub fn edit_clips(
        &mut self,
        targets: &[ClipRef],
        label: &'static str,
        mut f: impl FnMut(&mut Clip),
    ) -> bool {
        let mut changed = Vec::new();
        for r in targets {
            let Some(before) = self.clip_at(*r).cloned() else {
                continue;
            };
            let mut clip = before.clone();
            f(&mut clip);
            if clip == before {
                continue;
            }
            let start_moved = clip.start != before.start;
            match *r {
                ClipRef::Arr { track, id } if start_moved => {
                    if let Some(a) = self
                        .project
                        .track_mut(track)
                        .and_then(|t| t.arr_clip_mut(id))
                    {
                        a.clip = clip;
                        edit::follow_start(a, before.start);
                    }
                }
                _ => {
                    if let Some(slot) = self.clip_at_mut(*r) {
                        *slot = clip;
                    }
                }
            }
            changed.push(*r);
        }
        // Markers decide how far a timeline clip's fades can reach.
        for r in &changed {
            if let ClipRef::Arr { track, .. } = r {
                if let Some(t) = self.project.track_mut(*track) {
                    t.settle_fades();
                }
            }
        }
        for r in &changed {
            self.sync_ref(*r);
        }
        if !changed.is_empty() {
            self.history.pending = Some(label);
        }
        !changed.is_empty()
    }

    /// What `targets` have in common.
    pub fn multi_clip(&self, targets: &[ClipRef]) -> MultiClip {
        let clips: Vec<&Clip> = targets.iter().filter_map(|r| self.clip_at(*r)).collect();
        let audio: Vec<AudioValues> = clips.iter().filter_map(|c| audio_values(c)).collect();
        let all_audio = !clips.is_empty() && audio.len() == clips.len();
        let shared = |range| if all_audio { range } else { None };
        MultiClip {
            count: clips.len(),
            all_audio,
            all_midi: !clips.is_empty() && audio.is_empty(),
            active: merged(clips.iter().map(|c| c.active)),
            looped: merged(clips.iter().map(|c| c.looped)),
            gain_db: if all_audio {
                merged(audio.iter().map(|a| a.gain_db))
            } else {
                None
            },
            transpose: if all_audio {
                merged(audio.iter().map(|a| a.transpose))
            } else {
                None
            },
            detune_cents: if all_audio {
                merged(audio.iter().map(|a| a.detune_cents))
            } else {
                None
            },
            ram: shared(merged(audio.iter().map(|a| a.ram))),
            hi_q: shared(merged(audio.iter().map(|a| a.hi_q))),
            edge_fade: shared(merged(audio.iter().map(|a| a.edge_fade))),
            warp: shared(merged(audio.iter().map(|a| a.warp))),
        }
    }

    // ------------------------------------------------------------- title bar

    pub fn set_clips_active(&mut self, targets: &[ClipRef], on: bool) -> bool {
        self.edit_clips(targets, "Clip Activator", |c| c.active = on)
    }

    pub fn rename_clips(&mut self, targets: &[ClipRef], name: &str) -> bool {
        self.edit_clips(targets, "Rename Clip", |c| c.name = name.to_string())
    }

    pub fn color_clips(&mut self, targets: &[ClipRef], color: [u8; 3]) -> bool {
        self.edit_clips(targets, "Clip Color", |c| c.color = color)
    }

    /// Assign Track Color to Clips, for the view's own kind of clips only (manual p. 203).
    pub fn assign_track_color(&mut self, track: TrackId, arrangement: bool) -> bool {
        let Some(t) = self.project.track(track) else {
            return false;
        };
        let color = t.color;
        let targets: Vec<ClipRef> = if arrangement {
            t.arrangement
                .iter()
                .map(|c| ClipRef::Arr { track, id: c.id })
                .collect()
        } else {
            (0..t.clips.len())
                .filter(|s| t.clips[*s].is_some())
                .map(|slot| ClipRef::Slot { track, slot })
                .collect()
        };
        self.edit_clips(&targets, "Assign Track Color", |c| c.color = color)
    }

    // ------------------------------------------------------------- markers

    /// Where a playing clip's playhead is in its content, if it's playing.
    pub fn clip_playhead(&self, r: ClipRef) -> Option<f64> {
        match r {
            ClipRef::Slot { track, slot } => {
                let shared = self.track_shared.get(&track)?;
                (shared.playing_slot.load(Ordering::Relaxed) == slot as i32)
                    .then(|| shared.clip_position.load())
            }
            ClipRef::Arr { track, id } => {
                let a = self.project.track(track)?.arr_clip(id)?;
                let tick = self.engine.shared.tick.load();
                (self.is_playing() && tick >= a.start as f64 && tick < a.end() as f64)
                    .then(|| a.content_at(tick))
            }
        }
    }

    /// `at` on the global quantization grid, which Set buttons and scrubbing round to.
    pub fn quantized(&self, at: f64) -> u64 {
        let q = self.project.quantize.ticks(self.project.time_signature) as f64;
        if q > 0.0 {
            ((at / q).round() * q).max(0.0) as u64
        } else {
            at.max(0.0).round() as u64
        }
    }

    /// A Set button: the marker goes to the playhead, rounded to the global quantization. Returns
    /// where, or `None` if the clip isn't playing.
    pub fn set_marker_at_playhead(&mut self, r: ClipRef, which: SetMarker) -> Option<u64> {
        let at = self.quantized(self.clip_playhead(r)?);
        self.set_marker(r, which, at);
        Some(at)
    }

    /// Sets a marker at a clip position, as a Set button would.
    pub fn set_marker(&mut self, r: ClipRef, which: SetMarker, at: u64) {
        match which {
            SetMarker::Start => {
                self.edit_clips(&[r], "Set Clip Start", |c| edit::set_start(c, at));
            }
            SetMarker::End => {
                self.edit_clips(&[r], "Set Clip End", |c| edit::set_end(c, at));
            }
            SetMarker::LoopPosition => {
                self.edit_clips(&[r], "Set Loop Position", |c| {
                    c.loop_start = at;
                    c.start = c.start.min(at.max(c.start));
                    c.looped = true;
                });
            }
            SetMarker::LoopLength => {
                self.edit_clips(&[r], "Set Loop Length", |c| {
                    if c.looped {
                        edit::set_loop_end(c, at);
                    } else {
                        // Loop off: it ends here and keeps its preset length.
                        c.loop_start = at.saturating_sub(c.length);
                        edit::set_loop_end(c, at);
                        c.looped = true;
                    }
                });
            }
        }
    }

    pub fn set_clip_looped(&mut self, targets: &[ClipRef], on: bool) -> bool {
        self.edit_clips(targets, "Clip Loop", |c| {
            // Unwarped audio can't loop (manual p. 209).
            let warp_off = !c.is_midi() && !c.is_warped();
            if !(on && warp_off) {
                c.looped = on;
            }
        })
    }

    /// Duplicate Loop: twice the loop with what's in it, and notes after it moved along.
    pub fn duplicate_loop(&mut self, r: ClipRef) -> bool {
        self.edit_clips(&[r], "Duplicate Loop", edit::duplicate_loop)
    }

    // ------------------------------------------------------------- properties

    pub fn set_clip_signature(&mut self, targets: &[ClipRef], sig: Option<TimeSignature>) -> bool {
        self.edit_clips(targets, "Clip Signature", |c| c.signature = sig)
    }

    pub fn set_clip_groove(&mut self, targets: &[ClipRef], groove: Option<GrooveId>) -> bool {
        self.edit_clips(targets, "Clip Groove", |c| c.groove = groove)
    }

    /// Hot-swapping a clip's groove replaces it in the Groove Pool too, so every clip using it
    /// changes with it (manual p. 211). A14's browser calls this with a loaded groove.
    pub fn hot_swap_groove(&mut self, r: ClipRef, new: Groove) -> bool {
        let new_id = new.id;
        let current = self.clip_at(r).and_then(|c| c.groove);
        match current.and_then(|id| {
            self.project
                .groove_pool
                .grooves
                .iter()
                .position(|g| g.id == id)
        }) {
            Some(at) => {
                let old_id = self.project.groove_pool.grooves[at].id;
                self.project.groove_pool.grooves[at] = new;
                self.project.for_each_clip_mut(&mut |c| {
                    if c.groove == Some(old_id) {
                        c.groove = Some(new_id);
                    }
                });
                self.mark_edit("Hot-Swap Groove");
            }
            None => {
                self.project.groove_pool.grooves.push(new);
                self.set_clip_groove(&[r], Some(new_id));
                self.mark_edit("Hot-Swap Groove");
            }
        }
        true
    }

    pub fn set_clip_scale(&mut self, targets: &[ClipRef], scale: ClipScale) -> bool {
        self.edit_clips(targets, "Clip Scale", |c| c.scale = scale.clone())
    }

    /// Remembers the scale of the clip last shown, which new clips take on (manual p. 286).
    pub fn note_recent_scale(&mut self, scale: &ClipScale) {
        if self.recent_scale != *scale {
            self.recent_scale.clone_from(scale);
        }
    }

    /// A clip's scale for the pitch tools, if it's on and one rLive knows.
    pub fn pitch_scale(clip: &Clip) -> Option<PitchScale> {
        if !clip.scale.enabled {
            return None;
        }
        let mask = SCALES.iter().find(|(n, _)| *n == clip.scale.name)?.1;
        Some((clip.scale.root % 12, mask))
    }

    pub fn set_clip_program(&mut self, targets: &[ClipRef], program: MidiProgram) -> bool {
        self.edit_clips(targets, "Bank and Program", |c| {
            if let ClipContent::Midi { program: p, .. } = &mut c.content {
                *p = (program != MidiProgram::UNSET).then_some(program);
            }
        })
    }

    /// Runs `edit` on the audio settings of each audio clip.
    fn edit_audio(
        &mut self,
        targets: &[ClipRef],
        label: &'static str,
        mut f: impl FnMut(&mut bool, &mut bool, &mut bool),
    ) -> bool {
        self.edit_clips(targets, label, |c| {
            if let ClipContent::Audio {
                ram,
                hi_q,
                edge_fade,
                ..
            } = &mut c.content
            {
                f(ram, hi_q, edge_fade);
            }
        })
    }

    pub fn set_clip_ram(&mut self, targets: &[ClipRef], on: bool) -> bool {
        self.edit_audio(targets, "RAM Mode", |ram, _, _| *ram = on)
    }

    pub fn set_clip_hi_q(&mut self, targets: &[ClipRef], on: bool) -> bool {
        self.edit_audio(targets, "Hi-Q", |_, hi_q, _| *hi_q = on)
    }

    pub fn set_clip_edge_fade(&mut self, targets: &[ClipRef], on: bool) -> bool {
        self.edit_audio(targets, "Clip Fade", |_, _, fade| *fade = on)
    }

    /// Moves every selected clip's gain by `by` dB. Reaching an end of the range brings them all
    /// to the same value, as Live's range sliders do (manual p. 232).
    pub fn shift_clip_gain(&mut self, targets: &[ClipRef], by: f32) -> bool {
        self.edit_clips(targets, "Clip Gain", |c| {
            if let ClipContent::Audio { gain_db, .. } = &mut c.content {
                *gain_db = (*gain_db + by).clamp(-70.0, 24.0);
            }
        })
    }

    pub fn shift_clip_transpose(&mut self, targets: &[ClipRef], by: i32) -> bool {
        self.edit_clips(targets, "Clip Pitch", |c| {
            if let ClipContent::Audio { transpose, .. } = &mut c.content {
                *transpose = (*transpose + by).clamp(-48, 48);
            }
        })
    }

    pub fn shift_clip_detune(&mut self, targets: &[ClipRef], by: f32) -> bool {
        self.edit_clips(targets, "Clip Pitch", |c| {
            if let ClipContent::Audio { detune_cents, .. } = &mut c.content {
                *detune_cents = (*detune_cents + by).clamp(-50.0, 50.0);
            }
        })
    }

    // ------------------------------------------------------------- note utilities

    /// Runs a pitch or time tool on one clip's notes.
    pub fn edit_notes(
        &mut self,
        r: ClipRef,
        label: &'static str,
        mut f: impl FnMut(&mut Vec<NoteData>),
    ) -> bool {
        self.edit_clips(&[r], label, |c| {
            if let Some(notes) = c.notes_mut() {
                f(notes);
            }
        })
    }

    /// The ×2 and /2 buttons: notes if any are chosen, else the loop region and what's in it.
    pub fn scale_time(&mut self, r: ClipRef, selected: &[usize], factor: f64) -> bool {
        if selected.is_empty() {
            self.edit_clips(&[r], "Time Scale", |c| {
                edit::scale_loop(c, factor);
                if let Some(notes) = c.notes_mut() {
                    edit::stretch(notes, &[], factor);
                }
            })
        } else {
            self.edit_notes(r, "Time Scale", |n| edit::stretch(n, selected, factor))
        }
    }

    /// A source of numbers from -1 to 1, for Humanize.
    pub fn tool_random(&mut self) -> impl FnMut() -> f64 {
        let mut state = self.random | 1;
        self.random = self
            .random
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(1);
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 2001) as f64 / 1000.0 - 1.0
        }
    }

    // ------------------------------------------------------------- scrubbing

    /// Jumps a clip's playback to `position`, rounded to the global quantization and at its next
    /// boundary. With `hold` the fragment keeps repeating until [`Self::scrub_release`].
    pub fn scrub_clip(&mut self, r: ClipRef, position: f64, hold: bool) {
        let q = self.project.quantize.ticks(self.project.time_signature);
        let at = self.quantized(position) as f64;
        // With no quantization the fragment is short enough to scrub through the music.
        let repeat = hold.then_some(if q > 0 { q as f64 } else { PPQ as f64 / 16.0 });
        match r {
            ClipRef::Slot { track, slot } => {
                self.send(EngineCommand::Launch(LaunchCmd::Scrub {
                    track,
                    slot,
                    position: at,
                    repeat,
                }));
            }
            ClipRef::Arr { track, id } => {
                let Some(a) = self.project.track(track).and_then(|t| t.arr_clip(id)) else {
                    return;
                };
                let Some(run) = a.clip.region().run_of(at) else {
                    return;
                };
                let tick = a.start as f64 + run - a.run_offset() as f64;
                self.seek(tick);
                self.arr_scrub = repeat.map(|len| (tick, len));
            }
        }
    }

    /// The mouse came up after a scrub: playback goes on from where it is.
    pub fn scrub_release(&mut self, r: ClipRef) {
        self.arr_scrub = None;
        if let ClipRef::Slot { track, .. } = r {
            self.send(EngineCommand::Launch(LaunchCmd::ScrubRelease(track)));
        }
    }

    /// A held scrub on a timeline clip goes back to its point each time it has played through.
    pub(super) fn poll_scrub(&mut self) {
        if let Some((tick, len)) = self.arr_scrub {
            if self.engine.shared.tick.load() >= tick + len {
                self.seek(tick);
            }
        }
    }
}
