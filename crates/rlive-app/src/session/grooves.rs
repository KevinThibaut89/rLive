//! The Groove Pool: loading and saving grooves, what clips do with them, and Commit and Extract.

use super::{ClipRef, Session};
use rlive_audio_io::SampleStatus;
use rlive_core::{ClipRegion, EnvTarget, GrooveBank, GrooveId, GrooveMap, TrackId, PPQ};
use rlive_engine::{ClipCmd, EngineCommand};
use rlive_project::warp_edit;
use rlive_project::{
    commit_notes, extract_from_events, extract_from_notes, groove_markers, Breakpoint, Clip,
    ClipContent, EnvMode, Groove, GroovePattern, GroovePool, NoteData, Warp, GROOVE_EXTENSION,
};
use std::path::Path;
use std::sync::Arc;

/// How much a groove accent can move an audio clip's volume, at Velocity 100.
const AUDIO_ACCENT: f32 = 0.5;

/// The moments in an audio clip a groove takes hold of: its Warp Markers and transients, as
/// (content tick, file seconds). Without transients, every sixteenth.
pub(super) fn warp_anchors(warp: &Warp, transients: &[f64], end: f64) -> Vec<(f64, f64)> {
    let Some(map) = warp_edit::seconds_map(warp) else {
        return Vec::new();
    };
    let mut anchors: Vec<(f64, f64)> = warp.markers.iter().map(|m| (m.tick, m.seconds)).collect();
    if transients.is_empty() {
        let step = (PPQ / 4) as f64;
        let mut tick = 0.0;
        while tick < end {
            anchors.push((tick, map.tick_to_src(tick)));
            tick += step;
        }
    } else {
        anchors.extend(transients.iter().map(|s| (map.src_to_tick(*s), *s)));
    }
    anchors.retain(|(t, _)| *t >= 0.0 && *t <= end);
    anchors
}

/// A warped clip's markers with `map` written into them: the content it plays moves in time.
/// Random gets its own number for each anchor, the same every time so the clip doesn't jitter
/// when it's rebuilt.
pub(super) fn grooved_warp(warp: &Warp, map: &GrooveMap, transients: &[f64], end: f64) -> Warp {
    let anchors = warp_anchors(warp, transients, end);
    if anchors.len() < 2 {
        return warp.clone();
    }
    Warp {
        markers: groove_markers(map, &anchors, 0x5eed),
        ..warp.clone()
    }
}

/// The Volume clip envelope that says what a groove's velocity does to an audio clip.
fn volume_points(map: &GrooveMap, anchors: &[(f64, f64)]) -> Vec<Breakpoint> {
    const LEVEL: f32 = 0.5;
    let mut points: Vec<Breakpoint> = anchors
        .iter()
        .map(|(tick, _)| {
            let moved = map.place(*tick, LEVEL, 0.0);
            let accent = (moved.1 - LEVEL) / 0.5;
            let gain = (0.7 + accent * AUDIO_ACCENT).clamp(0.0, 1.0);
            Breakpoint::new(moved.0.max(0.0), f64::from(gain))
        })
        .collect();
    points.sort_by(|a, b| a.time.total_cmp(&b.time));
    points
}

/// Every groove of a pool, as the audio thread reads it.
pub(super) fn groove_bank(pool: &GroovePool) -> Arc<GrooveBank> {
    Arc::new(GrooveBank {
        maps: pool.grooves.iter().map(|g| pool.map_of(g)).collect(),
        amount: pool.amount,
    })
}

impl Session {
    /// Tells the engine what every groove in the Set now does.
    pub fn send_grooves(&mut self) {
        let bank = groove_bank(&self.project.groove_pool);
        self.send(EngineCommand::Clip(ClipCmd::SetGrooves(bank)));
    }

    /// Whether any clip in the Set is using `id`. Inactive grooves are grayed out.
    pub fn groove_in_use(&self, id: GrooveId) -> bool {
        self.all_clips().any(|c| c.groove == Some(id))
    }

    /// Whether any clip uses any groove: Global Amount appears in the Control Bar then.
    pub fn any_groove_in_use(&self) -> bool {
        self.all_clips().any(|c| c.groove.is_some())
    }

    fn all_clips(&self) -> impl Iterator<Item = &Clip> {
        self.project.tracks.iter().flat_map(|t| {
            t.clips
                .iter()
                .flatten()
                .chain(t.arrangement.iter().map(|c| &c.clip))
        })
    }

    /// Every clip using a groove, for when what it does to audio has to be rebuilt.
    fn clips_using(&self, id: GrooveId) -> Vec<ClipRef> {
        let mut out = Vec::new();
        for t in &self.project.tracks {
            for (slot, c) in t.clips.iter().enumerate() {
                if c.as_ref().is_some_and(|c| c.groove == Some(id)) {
                    out.push(ClipRef::slot(t.id, slot));
                }
            }
            for c in &t.arrangement {
                if c.clip.groove == Some(id) {
                    out.push(ClipRef::Arr {
                        track: t.id,
                        id: c.id,
                    });
                }
            }
        }
        out
    }

    /// Adds a groove to the pool, and returns its id. Clips aren't touched.
    pub fn add_groove(&mut self, groove: Groove) -> GrooveId {
        let id = groove.id;
        self.project.groove_pool.grooves.push(groove);
        self.send_grooves();
        self.mark_edit("Add Groove");
        id
    }

    /// A groove's index moved because the pool changed: clips hold those indices.
    fn resend_all_midi_clips(&mut self) {
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for id in tracks {
            let slots = self.project.track(id).map_or(0, |t| t.clips.len());
            for slot in 0..slots {
                if self.clip(id, slot).is_some_and(|c| c.groove.is_some()) {
                    self.send_clip(id, slot);
                }
            }
            self.sync_lane(id);
        }
    }

    /// Reads a groove file into the pool.
    pub fn load_groove_file(&mut self, path: &Path) -> Result<GrooveId, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut groove = Groove::from_file(&text)?;
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            groove.name = stem.to_string();
        }
        groove.file = Some(rlive_project::FileRef::new(path));
        Ok(self.add_groove(groove))
    }

    /// Writes a pool groove to a file.
    pub fn save_groove_file(&self, id: GrooveId, path: &Path) -> Result<(), String> {
        let groove = self.project.groove_pool.get(id).ok_or("no such groove")?;
        let path = path.with_extension(GROOVE_EXTENSION);
        std::fs::write(path, groove.to_file()?).map_err(|e| e.to_string())
    }

    /// Takes a groove out of the pool. Clips using it go back to none.
    pub fn remove_groove(&mut self, id: GrooveId) {
        let targets = self.clips_using(id);
        self.edit_clips(&targets, "Remove Groove", |c| c.groove = None);
        self.project.groove_pool.grooves.retain(|g| g.id != id);
        self.send_grooves();
        self.resend_all_midi_clips();
        self.mark_edit("Remove Groove");
    }

    /// Changes a groove's settings. MIDI hears it at once, with not a note sent anywhere.
    /// Audio goes through its warping, so those clips are rebuilt.
    pub fn edit_groove(&mut self, id: GrooveId, f: impl FnOnce(&mut Groove)) {
        let Some(g) = self.project.groove_pool.get_mut(id) else {
            return;
        };
        f(g);
        g.quantize = g.quantize.clamp(0.0, 100.0);
        g.timing = g.timing.clamp(0.0, 100.0);
        g.random = g.random.clamp(0.0, 100.0);
        g.velocity = g.velocity.clamp(-100.0, 100.0);
        self.send_grooves();
        self.resend_audio_clips(id);
        self.mark_edit("Groove Settings");
    }

    fn resend_audio_clips(&mut self, id: GrooveId) {
        for r in self.clips_using(id) {
            if self.clip_at(r).is_some_and(|c| !c.is_midi()) {
                match r {
                    ClipRef::Slot { track, slot } => self.send_clip(track, slot),
                    ClipRef::Arr { track, .. } => self.sync_lane(track),
                }
            }
        }
    }

    /// Global Amount, 0..1.3.
    pub fn set_groove_amount(&mut self, amount: f32) {
        self.project.groove_pool.amount = amount.clamp(0.0, 1.3);
        self.send_grooves();
        let ids: Vec<GrooveId> = self
            .project
            .groove_pool
            .grooves
            .iter()
            .map(|g| g.id)
            .collect();
        for id in ids {
            self.resend_audio_clips(id);
        }
        self.mark_edit("Global Amount");
    }

    /// Puts a groove on the clips: what dropping one on a clip does.
    pub fn apply_groove(&mut self, targets: &[ClipRef], id: Option<GrooveId>) -> bool {
        self.set_clip_groove(targets, id)
    }

    /// Replaces the groove `old` in the pool: Hot-Swap in the Groove Pool.
    pub fn swap_groove(&mut self, old: GrooveId, new: Groove) {
        let Some(at) = self.project.groove_pool.index_of(old) else {
            return;
        };
        let new_id = new.id;
        let targets = self.clips_using(old);
        self.project.groove_pool.grooves[at as usize] = new;
        self.edit_clips(&targets, "Hot-Swap Groove", |c| c.groove = Some(new_id));
        self.send_grooves();
        self.mark_edit("Hot-Swap Groove");
    }

    /// Writes a clip's groove into it: MIDI notes move, audio gets Warp Markers, and a positive
    /// Velocity on audio becomes the clip's Volume envelope. The chooser goes back to none.
    pub fn commit_groove(&mut self, r: ClipRef) -> bool {
        let Some(clip) = self.clip_at(r) else {
            return false;
        };
        let Some(groove) = clip.groove.and_then(|id| self.project.groove_pool.get(id)) else {
            return false;
        };
        let map = self.project.groove_pool.map_of(groove);
        let velocity = groove.velocity;
        let seed = groove.id.0;
        let audio = match &clip.content {
            ClipContent::Audio { file, warp, .. } if warp.enabled => {
                let transients = self.samples.transients(&file.path);
                let found = transients.as_ref().map_or(&[][..], |f| f.as_slice());
                Some(rlive_project::warp_edit::resolve_transients(
                    found,
                    &warp.transients,
                ))
            }
            ClipContent::Audio { .. } => return false,
            ClipContent::Midi { .. } => None,
        };
        let end = clip.loop_end() as f64;
        self.edit_clips(&[r], "Commit Groove", |c| {
            let mut volume = None;
            match &mut c.content {
                ClipContent::Midi { notes, .. } => commit_notes(&map, notes, seed),
                ClipContent::Audio { warp, .. } => {
                    let transients = audio.as_deref().unwrap_or(&[]);
                    let anchors = warp_anchors(warp, transients, end);
                    if velocity > 0.0 && anchors.len() >= 2 {
                        volume = Some(volume_points(&map, &anchors));
                    }
                    let grooved = grooved_warp(warp, &map, transients, end);
                    *warp = grooved;
                }
            }
            if let Some(points) = volume {
                c.set_envelope_points(
                    EnvTarget::Clip(rlive_core::ClipParam::Gain),
                    EnvMode::Modulation,
                    points,
                );
            }
            c.groove = None;
        })
    }

    /// Makes a new groove from what plays in a clip, as dropping the clip on the Groove Pool does.
    /// `None` if there's nothing to go on.
    pub fn extract_groove(&mut self, r: ClipRef) -> Option<GrooveId> {
        let clip = self.clip_at(r)?.clone();
        let region = clip.region();
        let pattern = match &clip.content {
            ClipContent::Midi { notes, .. } => extract_from_notes(notes, &region),
            ClipContent::Audio { file, warp, .. } => {
                let events = self.audio_events(&file.path, warp, &region)?;
                extract_from_events(&events, &region)
            }
        };
        if pattern.hits.is_empty() {
            return None;
        }
        Some(self.add_groove(Groove::new(clip.name.clone(), pattern)))
    }

    /// The transients of a clip as (content tick, level) from the loaded audio.
    fn audio_events(
        &self,
        path: &Path,
        warp: &Warp,
        region: &ClipRegion,
    ) -> Option<Vec<(f64, f32)>> {
        let map = warp_edit::seconds_map(warp)?;
        let SampleStatus::Ready(sample) = self.samples.status(path)? else {
            return None;
        };
        let found = self.samples.transients(path)?;
        let transients = warp_edit::resolve_transients(found.as_slice(), &warp.transients);
        let window = (sample.sample_rate * 0.03) as usize;
        let events = transients
            .iter()
            .map(|s| {
                let at = (s * sample.sample_rate) as usize;
                let end = at.saturating_add(window).min(sample.left.len());
                let level = sample
                    .left
                    .get(at..end)
                    .map_or(0.0, |w| w.iter().fold(0.0f32, |p, x| p.max(x.abs())));
                (map.src_to_tick(*s), level)
            })
            .collect::<Vec<_>>();
        let _ = region;
        Some(events)
    }

    /// Puts a groove's notes in a new MIDI clip on `track`, in `slot`, so it can be edited and
    /// extracted again. Every note is C3, as a groove has only timing and volume.
    pub fn groove_to_clip(&mut self, id: GrooveId, track: TrackId, slot: usize) -> bool {
        let Some(groove) = self.project.groove_pool.get(id) else {
            return false;
        };
        let GroovePattern { length, hits } = groove.pattern.clone();
        let name = groove.name.clone();
        let notes: Vec<NoteData> = hits
            .iter()
            .map(|h| NoteData::new(h.tick.max(0.0) as u64, PPQ / 8, 60, h.velocity))
            .collect();
        let color = self.new_clip_color(track);
        let clip = Clip {
            name,
            color,
            length,
            looped: true,
            content: ClipContent::midi(notes),
            ..Clip::default()
        };
        self.set_clip(track, slot, Some(clip));
        true
    }
}
