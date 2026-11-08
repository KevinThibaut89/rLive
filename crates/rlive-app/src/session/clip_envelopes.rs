//! Clip envelopes: editing them, and the clip settings the Envelopes tab offers.

use super::{ClipRef, Session};
use rlive_core::param::EnvTarget;
use rlive_engine::{ClipCmd, EngineCommand};
use rlive_project::clip_envelope::targets_clip_content;
use rlive_project::warp_edit;
use rlive_project::{Breakpoint, ClipContent, EnvLoop, EnvMode, FileRef};
use std::path::Path;

impl Session {
    /// A clip's envelope has changed, in `target`. Parameter envelopes are heard through the
    /// track's routes; the clip's own have to be rebuilt into it.
    fn clip_envelope_changed(&mut self, r: ClipRef, target: EnvTarget, label: &'static str) {
        if targets_clip_content(target) {
            self.sync_ref(r);
        } else {
            self.sync_clip_routes(r.track());
        }
        self.mark_edit(label);
    }

    /// Replace a clip envelope's points. No points deletes the envelope.
    pub fn set_clip_envelope(
        &mut self,
        r: ClipRef,
        target: EnvTarget,
        mode: EnvMode,
        points: Vec<Breakpoint>,
    ) {
        let Some(clip) = self.clip_at_mut(r) else {
            return;
        };
        clip.set_envelope_points(target, mode, points);
        self.clip_envelope_changed(r, target, "Edit Clip Envelope");
    }

    /// Unlink a clip envelope from its clip, or link it again with `None` (manual p. 542).
    pub fn set_clip_envelope_loop(
        &mut self,
        r: ClipRef,
        target: EnvTarget,
        mode: EnvMode,
        region: Option<EnvLoop>,
    ) {
        let Some(e) = self
            .clip_at_mut(r)
            .and_then(|c| c.envelope_mut(target, mode))
        else {
            return;
        };
        e.loop_region = region;
        self.clip_envelope_changed(r, target, "Edit Clip Envelope");
    }

    /// The MIDI Envelope Auto-Reset option (manual p. 534), as the engine must know it.
    pub fn send_midi_envelope_auto_reset(&mut self, on: bool) {
        self.send(EngineCommand::Clip(ClipCmd::SetMidiAutoReset(on)));
    }

    /// Puts a different sample in a clip and leaves everything else about it alone, its envelopes
    /// included (manual p. 537). Returns whether the clip took it.
    pub fn replace_clip_sample(&mut self, r: ClipRef, path: &Path) -> bool {
        let old_seconds = self.clip_at(r).and_then(|c| self.file_seconds_of(c));
        let new_seconds = self.probe_seconds(path);
        let ticks_per_bar = self.project.time_signature.ticks_per_bar();
        let tempo = self.project.tempo;
        let Some(clip) = self.clip_at_mut(r) else {
            return false;
        };
        let ClipContent::Audio { file, warp, .. } = &mut clip.content else {
            return false;
        };
        *file = FileRef::new(path);
        // Warp Markers only mean anything on audio of the same length (manual p. 231).
        if old_seconds != new_seconds {
            warp.markers = match new_seconds.filter(|_| warp.enabled) {
                Some(s) if s > 0.0 => {
                    let bars = warp_edit::suggest_bars(s, ticks_per_bar, tempo);
                    warp_edit::loop_markers(s, bars, ticks_per_bar)
                }
                _ => Vec::new(),
            };
            warp.transients = Default::default();
        }
        self.samples.request(path);
        self.sync_ref(r);
        self.mark_edit("Replace Sample");
        true
    }
}
