//! Tuning systems in a Set: what each track plays, and what loading one does to its notes.

use super::Session;
use rlive_core::{MpeEncoder, MpeOutput, TrackId, MPE_SLOT};
use rlive_engine::{EngineCommand, TuningCmd};
use rlive_project::tuning::{equal_hz, retune_notes};
use rlive_project::{
    Clip, ExternalRoute, Project, Route, ScaleNote, Track, TrackKind, Tuning, TuningLayout,
};
use std::path::Path;
use std::sync::Arc;

/// What a loaded tuning would do to the notes already in the Set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetunePreview {
    pub clips: usize,
    pub notes: usize,
}

/// What each MIDI track of a Set is to play: for the running engine and for offline renders.
pub(super) fn tuning_commands(project: &Project) -> Vec<EngineCommand> {
    let tuning = project.tuning.as_ref();
    let table = tuning.map(|t| Arc::new(t.table()));
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Midi)
        .map(|t| {
            let bypassed = t.tuning_bypassed();
            let active = table.is_some() && !bypassed;
            let layout = tuning
                .filter(|_| !bypassed)
                .and_then(|p| t.tuning_layout.keys(p))
                .map(Arc::new);
            EngineCommand::Tuning(TuningCmd::SetTrack {
                track: t.id,
                table: table.clone().unwrap_or_default(),
                active,
                layout,
            })
        })
        .collect()
}

impl Session {
    pub fn tuning(&self) -> Option<&Tuning> {
        self.project.tuning.as_ref()
    }

    /// Whether this track plays 12-TET under a loaded tuning.
    pub fn track_bypasses_tuning(&self, track: TrackId) -> bool {
        self.project.track(track).is_none_or(Track::tuning_bypassed)
    }

    /// The tuning the note ruler of this track's clips shows, if it isn't bypassed.
    pub fn track_tuning(&self, track: TrackId) -> Option<&Tuning> {
        self.tuning().filter(|_| !self.track_bypasses_tuning(track))
    }

    /// Tells every track what it plays now. A track with nothing loaded plays 12-TET and keeps
    /// its keys.
    pub fn send_tuning(&mut self) {
        for c in tuning_commands(&self.project) {
            self.send(c);
        }
    }

    /// Loads a tuning system. With Retune Set on, the Set's notes follow it to keep sounding as
    /// they did, which can cost notes that overlap. The UI asks first.
    pub fn load_tuning(&mut self, tuning: Tuning, retune: bool) {
        let before = self.project.tuning.clone();
        if retune {
            self.retune_all(before.as_ref(), Some(&tuning));
        }
        self.project.tuning = Some(tuning);
        self.after_tuning_change("Load Tuning");
    }

    pub fn load_tuning_file(&mut self, path: &Path, retune: bool) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Tuning");
        let tuning = Tuning::parse(&text, stem)?;
        self.load_tuning(tuning, retune);
        Ok(())
    }

    /// Removes the tuning and goes back to 12-TET.
    pub fn clear_tuning(&mut self, retune: bool) {
        let before = self.project.tuning.clone();
        if retune {
            self.retune_all(before.as_ref(), None);
        }
        self.project.tuning = None;
        self.after_tuning_change("Remove Tuning");
    }

    /// Writes the loaded tuning as an `.ascl` file.
    pub fn save_tuning_file(&self, path: &Path) -> Result<(), String> {
        let t = self.tuning().ok_or("no tuning loaded")?;
        std::fs::write(path.with_extension("ascl"), t.to_ascl()).map_err(|e| e.to_string())
    }

    /// Changes a setting of the Tuning section.
    pub fn edit_tuning(&mut self, f: impl FnOnce(&mut Tuning)) {
        if let Some(t) = self.project.tuning.as_mut() {
            f(t);
            t.reference_hz = t.reference_hz.clamp(1.0, 20_000.0);
            self.after_tuning_change("Tuning");
        }
    }

    pub fn set_tuning_reference_octave(&mut self, octave: i32) {
        self.edit_tuning(|t| {
            let degree = t.reference.degree;
            t.set_reference_note(ScaleNote { octave, degree });
        });
    }

    pub fn set_tuning_reference_degree(&mut self, degree: u32) {
        self.edit_tuning(|t| {
            let octave = t.reference.octave;
            t.set_reference_note(ScaleNote { octave, degree });
        });
    }

    fn after_tuning_change(&mut self, label: &'static str) {
        self.send_tuning();
        self.mark_edit(label);
    }

    fn retune_all(&mut self, before: Option<&Tuning>, after: Option<&Tuning>) {
        let hz_before = |k: u8| before.map_or_else(|| equal_hz(k), |t| t.key_hz(k));
        let hz_after = |k: u8| after.map_or_else(|| equal_hz(k), |t| t.key_hz(k));
        let tracks: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Midi && !t.tuning_bypassed())
            .map(|t| t.id)
            .collect();
        for track in tracks {
            let Some(t) = self.project.track_mut(track) else {
                continue;
            };
            for c in t.clips.iter_mut().flatten() {
                retune_clip(c, &hz_before, &hz_after);
            }
            for c in t.arrangement.iter_mut() {
                retune_clip(&mut c.clip, &hz_before, &hz_after);
            }
            for slot in 0..t.clips.len() {
                self.send_clip(track, slot);
            }
            self.sync_lane(track);
        }
    }

    /// What loading would change, for the confirmation.
    pub fn retune_preview(&self) -> RetunePreview {
        let mut p = RetunePreview::default();
        for t in self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Midi && !t.tuning_bypassed())
        {
            for c in t
                .clips
                .iter()
                .flatten()
                .chain(t.arrangement.iter().map(|c| &c.clip))
            {
                if c.is_midi() && !c.notes().is_empty() {
                    p.clips += 1;
                    p.notes += c.notes().len();
                }
            }
        }
        p
    }

    pub fn set_bypass_tuning(&mut self, track: TrackId, on: bool) {
        if let Some(t) = self.project.track_mut(track) {
            t.bypass_tuning = on;
            self.after_tuning_change("Bypass Tuning");
        }
    }

    pub fn set_tuning_layout(&mut self, track: TrackId, layout: TuningLayout) {
        if let Some(t) = self.project.track_mut(track) {
            t.tuning_layout = layout;
            self.after_tuning_change("MIDI Controller Layout");
        }
    }

    /// The MPE/Multi-channel Settings of a track's MIDI output. `None` plays on one channel.
    pub fn set_track_mpe_out(&mut self, track: TrackId, out: Option<MpeOutput>) {
        if let Some(t) = self.project.track_mut(track) {
            t.mpe_out = out;
            self.send_midi_output(track);
            self.mark_edit("MPE Settings");
        }
    }

    /// Tells the engine where a track's MIDI goes and how it's spread over channels.
    pub fn send_midi_output(&mut self, track: TrackId) {
        let Some(t) = self.project.track(track) else {
            return;
        };
        let port = match &t.routing.output {
            Route::External(ExternalRoute::Midi { port: Some(p), .. }) => self
                .midi_out_ports
                .iter()
                .position(|n| n == p)
                .map(|i| i as u16),
            _ => None,
        };
        let encoder = t.mpe_out.map(|o| Box::new(MpeEncoder::configured(o)));
        self.send(EngineCommand::Tuning(TuningCmd::SetOutput {
            track,
            port,
            encoder,
        }));
    }

    /// A plug-in's MPE Mode and channels, if it's on.
    pub fn device_mpe(
        &self,
        chain: super::ChainRef,
        device: rlive_core::DeviceId,
    ) -> Option<MpeOutput> {
        self.chain(chain)
            .and_then(|v| v.iter().find(|d| d.id == device))
            .and_then(|d| d.mpe)
    }

    /// A plug-in's MPE Mode and channels. Takes effect at once.
    pub fn set_device_mpe(
        &mut self,
        chain: super::ChainRef,
        device: rlive_core::DeviceId,
        out: Option<MpeOutput>,
    ) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == device))
        {
            d.mpe = out;
        }
        self.send_edits(vec![rlive_engine::GraphOp::SetResource {
            target: chain.into(),
            device,
            slot: MPE_SLOT,
            resource: Arc::new(out),
        }]);
        self.mark_edit("MPE Mode");
    }
}

fn retune_clip(c: &mut Clip, before: &impl Fn(u8) -> f64, after: &impl Fn(u8) -> f64) {
    if let Some(notes) = c.notes_mut() {
        retune_notes(notes, before, after);
    }
}
