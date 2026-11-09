//! Capture MIDI and step recording.
//!
//! Both work from what is played, with no recording running. Capture looks back through the
//! buffer kept for each armed or monitored track and makes a clip of it; step recording writes
//! whichever keys are down each time the insert marker moves.

use super::{ClipRef, Session};
use crate::recording::capture_midi::{
    detect_phrase, phrase_with_tempo, Phrase, Played, PlayedNote as CapturedNote,
};
use rlive_core::{MidiMessage, TrackId, PPQ};
use rlive_engine::PlayedNote;
use rlive_project::{ArrangementClip, Clip, ClipContent, Monitor, NoteData, TrackKind};

impl Session {
    pub(super) fn played_midi(&mut self, note: PlayedNote) {
        let PlayedNote {
            track,
            seconds,
            tick,
            message,
        } = note;
        let Some(t) = self.project.track(track) else {
            return;
        };
        if t.kind != TrackKind::Midi {
            return;
        }
        let listening = t.arm || t.routing.monitor == Monitor::In;
        if !listening {
            return;
        }
        match message {
            MidiMessage::NoteOn { key, velocity, .. } if velocity > 0.0 => {
                self.step_keys
                    .entry(track)
                    .or_default()
                    .insert(key, velocity);
            }
            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                if let Some(held) = self.step_keys.get_mut(&track) {
                    held.remove(&key);
                }
            }
            _ => {}
        }
        self.captures.entry(track).or_default().push(Played {
            seconds,
            tick,
            message,
        });
    }

    /// Whether Capture has anything to work on.
    pub fn can_capture(&self) -> bool {
        self.captures.values().any(|b| !b.is_empty())
    }

    /// Capture MIDI: makes a clip of what was just played on each listening track, in the Session
    /// or the Arrangement depending on which is in view.
    pub fn capture_midi(&mut self, into_arrangement: bool, scene: usize) {
        let empty_set = !self.is_playing()
            && self
                .project
                .tracks
                .iter()
                .all(|t| t.arrangement.is_empty() && t.clips.iter().all(Option::is_none))
            && self.project.tempo_points_in_force().is_empty();
        let tracks: Vec<TrackId> = self.captures.keys().copied().collect();
        let tempo = self.project.tempo;
        let sig = self.project.time_signature;
        let now = self.engine.shared.tick.load();
        let mut set_tempo = None;
        let mut any = false;
        for track in tracks {
            let notes = match self.captures.get(&track) {
                Some(b) => b.notes(),
                None => continue,
            };
            let Some(phrase) = self.detect(&notes, empty_set, tempo, sig, now) else {
                continue;
            };
            if empty_set && set_tempo.is_none() {
                set_tempo = Some(phrase.tempo);
            }
            if self.overdub_capture(track, &notes, tempo) {
                any = true;
                continue;
            }
            any |= self.place_capture(track, phrase, into_arrangement, scene, empty_set, now);
        }
        if !any {
            return;
        }
        for t in self.project.tracks.clone() {
            self.captures.remove(&t.id);
        }
        if let Some(bpm) = set_tempo {
            self.set_tempo(bpm);
        }
        self.mark_edit("Capture MIDI");
        self.commit_now();
        if empty_set {
            self.play();
        }
    }

    fn detect(
        &self,
        notes: &[CapturedNote],
        empty_set: bool,
        tempo: f64,
        sig: rlive_core::TimeSignature,
        now: f64,
    ) -> Option<Phrase> {
        if notes.is_empty() {
            return None;
        }
        if empty_set {
            detect_phrase(notes, sig)
        } else {
            let last = notes.last()?.seconds;
            let bar = sig.ticks_per_bar() as f64;
            let origin = (now / bar).floor() * bar;
            phrase_with_tempo(notes, tempo, sig, |n| {
                n.tick
                    .unwrap_or(origin + (n.seconds - last) * tempo / 60.0 * PPQ as f64)
            })
        }
    }

    /// Adds what was played to a clip already playing on the track. Returns whether it did.
    fn overdub_capture(&mut self, track: TrackId, notes: &[CapturedNote], tempo: f64) -> bool {
        let Some(shared) = self.track_shared.get(&track).cloned() else {
            return false;
        };
        if !self.is_playing() {
            return false;
        }
        let slot = shared
            .playing_slot
            .load(std::sync::atomic::Ordering::Relaxed);
        if slot < 0 {
            return false;
        }
        let slot = slot as usize;
        let now = self.engine.shared.tick.load();
        let position = shared.clip_position.load();
        let Some(clip) = self.clip(track, slot) else {
            return false;
        };
        if !clip.is_midi() {
            return false;
        }
        let length = clip.length.max(1) as f64;
        let mut new = Vec::new();
        for n in notes {
            let Some(tick) = n.tick else {
                continue;
            };
            let at = (position - (now - tick)).rem_euclid(length);
            let duration = (n.duration * tempo / 60.0 * PPQ as f64).round().max(10.0) as u64;
            new.push(NoteData::new(
                at.round() as u64,
                duration,
                n.key,
                n.velocity,
            ));
        }
        if new.is_empty() {
            return false;
        }
        if let Some(clip) = self.clip_mut(track, slot) {
            if let Some(existing) = clip.notes_mut() {
                existing.extend(new);
            }
        }
        self.sync_clip(track, slot);
        true
    }

    fn place_capture(
        &mut self,
        track: TrackId,
        phrase: Phrase,
        into_arrangement: bool,
        scene: usize,
        empty_set: bool,
        now: f64,
    ) -> bool {
        let color = self.new_clip_color(track);
        let name = self
            .project
            .track(track)
            .map_or_else(String::new, |t| t.name.clone());
        let clip = Clip {
            name,
            color,
            length: phrase.length,
            looped: true,
            start: phrase.loop_start,
            loop_start: phrase.loop_start,
            content: ClipContent::midi(phrase.notes),
            launch: self.launch_defaults.settings(),
            ..Clip::default()
        };
        if into_arrangement {
            let sig = self.project.time_signature;
            let bar = sig.ticks_per_bar();
            let at = if empty_set {
                0
            } else {
                (now.max(0.0) as u64) / bar * bar
            };
            let placed = ArrangementClip::new(clip, at);
            self.arr_place(track, placed);
            return true;
        }
        let Some(slot) = self.project.track(track).and_then(|t| {
            (scene..t.clips.len())
                .chain(0..scene)
                .find(|s| t.clips[*s].is_none())
        }) else {
            return false;
        };
        self.set_clip(track, slot, Some(clip));
        self.launch_clip(track, slot);
        true
    }

    // ------------------------------------------------------------- step recording

    /// The keys held right now on an armed track, ready to be stepped in.
    pub fn step_keys_held(&self, track: TrackId) -> bool {
        self.step_keys.get(&track).is_some_and(|h| !h.is_empty())
    }

    /// Right arrow with keys held: adds them at `at` for one grid step, or lengthens them if they
    /// were held through the step before. Returns where the insert marker goes.
    pub fn step_forward(&mut self, clip: ClipRef, at: u64, grid: u64) -> u64 {
        let held: Vec<(u8, f32)> = self
            .step_keys
            .get(&clip.track())
            .map(|h| h.iter().map(|(k, v)| (*k, *v)).collect())
            .unwrap_or_default();
        if held.is_empty() {
            return at + grid;
        }
        let Some(c) = self.clip_at_mut(clip) else {
            return at + grid;
        };
        let Some(notes) = c.notes_mut() else {
            return at + grid;
        };
        for (key, velocity) in held {
            if let Some(n) = notes
                .iter_mut()
                .find(|n| n.key == key && n.start + n.length == at)
            {
                n.length += grid;
            } else {
                notes.push(NoteData::new(at, grid, key, velocity));
            }
        }
        notes.sort_by_key(|n| n.start);
        self.sync_ref(clip);
        self.mark_edit("Step Record");
        self.commit_now();
        at + grid
    }

    /// Left arrow with keys held: takes back the last step of the held keys, moving the marker
    /// back over it.
    pub fn step_back(&mut self, clip: ClipRef, at: u64, grid: u64) -> u64 {
        let back = at.saturating_sub(grid);
        let keys: Vec<u8> = self
            .step_keys
            .get(&clip.track())
            .map(|h| h.keys().copied().collect())
            .unwrap_or_default();
        if keys.is_empty() {
            return back;
        }
        let Some(c) = self.clip_at_mut(clip) else {
            return back;
        };
        let Some(notes) = c.notes_mut() else {
            return back;
        };
        for n in notes.iter_mut() {
            if keys.contains(&n.key) && n.start + n.length == at {
                n.length = n.length.saturating_sub(grid);
            }
        }
        notes.retain(|n| n.length > 0);
        self.sync_ref(clip);
        self.mark_edit("Step Record");
        self.commit_now();
        back
    }
}
