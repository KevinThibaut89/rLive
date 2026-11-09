//! Selection-based and Time-based editing of the Arrangement, locators, signature markers, the
//! loop and fades. Everything here only changes the project and tells the engine; the rules
//! about what fits are in [`rlive_project::arrangement_edit`].

use super::{ChainRef, Session};
use rlive_core::fade::DEFAULT_EDGE_FADE_MS;
use rlive_core::time::BarMap;
use rlive_core::{ArrClipId, EnvTarget, ParamRange, Quantize, TimeSignature, TrackId};
use rlive_engine::{EngineCommand, TrackRef, TransportCmd};
use rlive_project::arrangement_edit::{ms_to_ticks, ranges_by, Fragment, TimeClip};
use rlive_project::{ArrangementClip, Locator};
use std::collections::{HashMap, HashSet};

/// The default fade length, in ticks at this tempo.
pub fn edge_fade_ticks(bpm: f64) -> u64 {
    ms_to_ticks(DEFAULT_EDGE_FADE_MS, bpm)
}

/// Which end of a clip a fade belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeSide {
    In,
    Out,
}

/// An edit of the Arrangement loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopEdit {
    /// Nudge both ends by this much.
    Nudge(i64),
    /// Move by its own length, this many times over.
    Shift(i32),
    /// Shorten or lengthen the end by this much.
    Resize(i64),
    /// Multiply the length by 2, or divide by 2.
    Double,
    Halve,
}

impl LoopEdit {
    /// The range this edit leaves. A loop is at least one tick long and doesn't start before 0.
    pub fn apply(self, start: u64, end: u64) -> (u64, u64) {
        let len = end.saturating_sub(start).max(1);
        let (s, e) = (start as i64, end as i64);
        match self {
            Self::Nudge(d) => {
                let d = d.max(-s);
                ((s + d) as u64, (e + d) as u64)
            }
            Self::Shift(n) => {
                let d = (n as i64 * len as i64).max(-s);
                ((s + d) as u64, (e + d) as u64)
            }
            Self::Resize(d) => (start, (e + d).max(s + 1) as u64),
            Self::Double => (start, start + len * 2),
            Self::Halve => (start, start + (len / 2).max(1)),
        }
    }
}

/// `tick` quantized to the nearest launch point. With no quantization it stays as it is.
pub fn quantized_tick(tick: f64, q: Quantize, bars: &BarMap) -> u64 {
    if q == Quantize::None {
        return tick.round() as u64;
    }
    let before = bars.prev_boundary(tick, q);
    let after = bars.next_boundary(tick, q);
    let nearest = if tick - before <= after - tick {
        before
    } else {
        after
    };
    nearest.round() as u64
}

/// Where Next jumps to from `from`: the next locator, then the end of the Arrangement.
pub fn next_locator(ticks: &[u64], from: f64, end: u64) -> f64 {
    ticks
        .iter()
        .find(|t| **t as f64 > from + 0.5)
        .map_or(end as f64, |t| *t as f64)
}

/// Where Previous jumps to from `from`: the locator before, then the start.
pub fn prev_locator(ticks: &[u64], from: f64) -> f64 {
    ticks
        .iter()
        .rev()
        .find(|t| (**t as f64) < from - 0.5)
        .map_or(0.0, |t| *t as f64)
}

impl Session {
    fn env_ranges(&self) -> HashMap<(TrackId, EnvTarget), ParamRange> {
        let master = self.project.master.id;
        let mut out = HashMap::new();
        for t in self
            .project
            .tracks
            .iter()
            .chain(&self.project.returns)
            .chain(std::iter::once(&self.project.master))
        {
            let chain = if t.id == master {
                ChainRef::Master
            } else {
                ChainRef::Track(t.id)
            };
            for e in &t.automation {
                if let Some(p) = self.env_param(chain, e.target) {
                    out.insert((t.id, e.target), p.range);
                }
            }
        }
        out
    }

    /// Tells the engine about a change to the whole timeline.
    fn after_time_edit(&mut self, label: &'static str) {
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for t in tracks {
            self.sync_lane(t);
        }
        let others: Vec<TrackRef> = self
            .project
            .tracks
            .iter()
            .chain(&self.project.returns)
            .map(|t| TrackRef::Track(t.id))
            .chain(std::iter::once(TrackRef::Master))
            .collect();
        for r in others {
            self.send_routes(r);
        }
        self.sync_time_maps();
        self.mark_edit(label);
    }

    // ------------------------------------------------------------- time commands

    pub fn copy_time(&self, start: u64, end: u64) -> TimeClip {
        let ranges = ranges_by(self.env_ranges());
        self.project.copy_time(start, end, &ranges)
    }

    pub fn cut_time(&mut self, start: u64, end: u64) -> TimeClip {
        let clip = self.copy_time(start, end);
        self.delete_time(start, end);
        clip
    }

    pub fn delete_time(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let ranges = ranges_by(self.env_ranges());
        self.project.delete_time(start, end, &ranges);
        self.after_time_edit("Delete Time");
    }

    pub fn paste_time(&mut self, at: u64, clip: &TimeClip) -> Vec<(TrackId, ArrClipId)> {
        let ranges = ranges_by(self.env_ranges());
        let placed = self.project.paste_time(at, clip, &ranges);
        self.after_time_edit("Paste Time");
        placed
    }

    pub fn duplicate_time(&mut self, start: u64, end: u64) -> Vec<(TrackId, ArrClipId)> {
        if end <= start {
            return Vec::new();
        }
        let clip = self.copy_time(start, end);
        self.paste_time(end, &clip)
    }

    pub fn insert_silence(&mut self, at: u64, len: u64) {
        if len == 0 {
            return;
        }
        let ranges = ranges_by(self.env_ranges());
        self.project.insert_time(at, len, &ranges);
        self.after_time_edit("Insert Silence");
    }

    pub fn fragments(&self) -> Vec<Fragment> {
        self.project.fragments()
    }

    pub fn delete_fragment(&mut self, f: Fragment) {
        let ranges = ranges_by(self.env_ranges());
        self.project.delete_fragment(f, &ranges);
        self.after_time_edit("Delete Fragmentary Bar Time");
    }

    pub fn complete_fragment(&mut self, f: Fragment) {
        let ranges = ranges_by(self.env_ranges());
        self.project.complete_fragment(f, &ranges);
        self.after_time_edit("Complete Fragmentary Bar");
    }

    // ------------------------------------------------------------- spans and splits

    fn settle_and_sync(&mut self, tracks: impl IntoIterator<Item = TrackId>) {
        for t in tracks {
            if let Some(track) = self.project.track_mut(t) {
                track.settle_fades();
            }
            self.sync_lane(t);
        }
    }

    pub fn arr_copy_span(
        &self,
        tracks: &[TrackId],
        start: u64,
        end: u64,
    ) -> Vec<(TrackId, ArrangementClip)> {
        tracks
            .iter()
            .filter_map(|id| self.project.track(*id))
            .flat_map(|t| t.copy_span(start, end).into_iter().map(move |c| (t.id, c)))
            .collect()
    }

    pub fn arr_delete_span(&mut self, tracks: &[TrackId], start: u64, end: u64) {
        for id in tracks {
            if let Some(t) = self.project.track_mut(*id) {
                t.delete_span(start, end);
            }
        }
        self.settle_and_sync(tracks.iter().copied());
        self.mark_edit("Delete Selection");
    }

    /// Moves what's in `[start, end)` of `tracks` by `delta`, leaving a gap behind.
    pub fn arr_move_span(
        &mut self,
        tracks: &[TrackId],
        start: u64,
        end: u64,
        delta: i64,
    ) -> Vec<(TrackId, ArrClipId)> {
        let copies = self.arr_copy_span(tracks, start, end);
        for id in tracks {
            if let Some(t) = self.project.track_mut(*id) {
                t.delete_span(start, end);
            }
        }
        let moved_start = (start as i64 + delta).max(0) as u64;
        let mut placed = Vec::new();
        for (track, mut c) in copies {
            c.id = ArrClipId::new();
            c.start += moved_start;
            placed.push((track, c.id));
            if let Some(t) = self.project.track_mut(track) {
                t.place_clip(c);
            }
        }
        self.settle_and_sync(tracks.iter().copied());
        self.mark_edit("Move Selection");
        placed
    }

    /// Splits `tracks` at the points given: one for an insert marker, two around a selection.
    pub fn arr_split(&mut self, tracks: &[TrackId], points: &[u64]) -> Vec<(TrackId, ArrClipId)> {
        let mut made = Vec::new();
        for id in tracks {
            let Some(t) = self.project.track_mut(*id) else {
                continue;
            };
            for at in points {
                if let Some(new) = t.split_at(*at) {
                    made.push((*id, new));
                }
            }
        }
        if !made.is_empty() {
            self.settle_and_sync(tracks.iter().copied());
            self.mark_edit("Split");
        }
        made
    }

    /// Keeps only what's inside `[start, end)` on `tracks`.
    pub fn arr_crop(&mut self, tracks: &[TrackId], start: u64, end: u64) {
        for id in tracks {
            if let Some(t) = self.project.track_mut(*id) {
                t.split_at(start);
                t.split_at(end);
                t.arrangement.retain(|c| c.start >= start && c.end() <= end);
            }
        }
        self.settle_and_sync(tracks.iter().copied());
        self.mark_edit("Crop");
    }

    /// Switches clips off, or on again if they were all off.
    pub fn arr_toggle_active(&mut self, items: &[(TrackId, ArrClipId)]) {
        let all_off = items.iter().all(|(t, id)| {
            self.project
                .track(*t)
                .and_then(|t| t.arr_clip(*id))
                .is_some_and(|c| !c.clip.active)
        });
        let mut touched = HashSet::new();
        for (t, id) in items {
            if let Some(c) = self.project.track_mut(*t).and_then(|t| t.arr_clip_mut(*id)) {
                c.clip.active = all_off;
                touched.insert(*t);
            }
        }
        if !touched.is_empty() {
            self.settle_and_sync(touched);
            self.mark_edit("Deactivate Clips");
        }
    }

    /// Deactivates the material in a time selection, splitting clips at its edges.
    pub fn arr_toggle_span(&mut self, tracks: &[TrackId], start: u64, end: u64) {
        self.arr_split(tracks, &[start, end]);
        let items: Vec<(TrackId, ArrClipId)> = tracks
            .iter()
            .filter_map(|id| self.project.track(*id))
            .flat_map(|t| {
                t.arrangement
                    .iter()
                    .filter(|c| c.start >= start && c.end() <= end)
                    .map(|c| (t.id, c.id))
                    .collect::<Vec<_>>()
            })
            .collect();
        self.arr_toggle_active(&items);
    }

    /// Whether these clips could be reversed together: all audio. Reverse itself is A9's.
    pub fn arr_can_reverse(&self, items: &[(TrackId, ArrClipId)]) -> bool {
        !items.is_empty()
            && items.iter().all(|(t, id)| {
                self.project
                    .track(*t)
                    .and_then(|t| t.arr_clip(*id))
                    .is_some_and(ArrangementClip::is_audio)
            })
    }

    /// Slides a clip's content under its edges by `delta` from where `base` had it.
    pub fn arr_slide(&mut self, track: TrackId, base: &ArrangementClip, delta: i64) {
        let Some(c) = self
            .project
            .track_mut(track)
            .and_then(|t| t.arr_clip_mut(base.id))
        else {
            return;
        };
        c.offset = base.offset;
        c.slide_content(delta);
        self.settle_and_sync([track]);
        self.mark_edit("Slide Clip Content");
    }

    // ------------------------------------------------------------- fades

    fn clip_index(&self, track: TrackId, id: ArrClipId) -> Option<usize> {
        self.project
            .track(track)?
            .arrangement
            .iter()
            .position(|c| c.id == id)
    }

    /// Sets a fade's length, as far as it can go, and its curve.
    pub fn arr_set_fade(
        &mut self,
        track: TrackId,
        id: ArrClipId,
        side: FadeSide,
        ticks: u64,
        curve: Option<f32>,
    ) {
        let Some(c) = self
            .project
            .track_mut(track)
            .and_then(|t| t.arr_clip_mut(id))
        else {
            return;
        };
        match side {
            FadeSide::In => {
                c.set_fade_in(ticks);
                if let Some(curve) = curve {
                    c.clip.fades.in_curve = curve;
                }
            }
            FadeSide::Out => {
                c.set_fade_out(ticks);
                if let Some(curve) = curve {
                    c.clip.fades.out_curve = curve;
                }
            }
        }
        self.settle_and_sync([track]);
        self.mark_edit("Change Fade");
    }

    /// Crossfades a clip into the next, returning what fit.
    pub fn arr_crossfade(
        &mut self,
        track: TrackId,
        left: ArrClipId,
        before: u64,
        after: u64,
        curve: f32,
    ) -> Option<(u64, u64)> {
        let i = self.clip_index(track, left)?;
        let fit = self
            .project
            .track_mut(track)?
            .set_crossfade(i, before, after, curve);
        if fit.is_some() {
            self.settle_and_sync([track]);
            self.mark_edit("Change Crossfade");
        }
        fit
    }

    /// Takes a fade off, back to the default if Create Fades on Clip Edges is on.
    pub fn arr_delete_fade(&mut self, track: TrackId, id: ArrClipId, side: FadeSide) {
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        match side {
            FadeSide::In => t.delete_fade_in(id),
            FadeSide::Out => t.delete_fade_out(id),
        }
        self.settle_and_sync([track]);
        self.mark_edit("Delete Fade");
    }

    /// Clears both fades of each clip.
    pub fn arr_delete_fades(&mut self, items: &[(TrackId, ArrClipId)]) {
        let mut touched = HashSet::new();
        for (track, id) in items {
            if let Some(t) = self.project.track_mut(*track) {
                t.delete_fade_in(*id);
                t.delete_fade_out(*id);
                touched.insert(*track);
            }
        }
        if !touched.is_empty() {
            self.settle_and_sync(touched);
            self.mark_edit("Delete Fades");
        }
    }

    /// What Create Fade does on one track for a time selection: a crossfade over a boundary
    /// inside it, otherwise a fade at any clip edge it touches.
    pub fn arr_fade_from_span(&mut self, tracks: &[TrackId], start: u64, end: u64) {
        let mut touched = HashSet::new();
        for id in tracks {
            let Some(t) = self.project.track_mut(*id) else {
                continue;
            };
            let boundary = (0..t.arrangement.len().saturating_sub(1)).find(|&i| {
                let at = t.arrangement[i].end();
                t.can_crossfade(i) && at > start && at < end
            });
            if let Some(i) = boundary {
                let at = t.arrangement[i].end();
                t.set_crossfade(i, at - start, end - at, 0.0);
                touched.insert(*id);
                continue;
            }
            for c in &mut t.arrangement {
                if !c.is_audio() {
                    continue;
                }
                let (cs, ce) = (c.start, c.end());
                if start <= cs && end > cs && end < ce {
                    c.set_fade_in(end - cs);
                    touched.insert(*id);
                } else if start > cs && start < ce && end >= ce {
                    c.set_fade_out(ce - start);
                    touched.insert(*id);
                }
            }
        }
        if !touched.is_empty() {
            self.settle_and_sync(touched);
            self.mark_edit("Create Fade");
        }
    }

    // ------------------------------------------------------------- locators

    fn locator_ticks(&self) -> Vec<u64> {
        let mut t: Vec<u64> = self
            .project
            .arrangement
            .locators
            .iter()
            .map(|l| l.tick)
            .collect();
        t.sort_unstable();
        t
    }

    pub fn add_locator(&mut self, tick: u64) -> usize {
        let a = &mut self.project.arrangement;
        let name = (a.locators.len() + 1).to_string();
        a.locators.push(Locator {
            tick,
            name,
            info: String::new(),
        });
        a.locators.sort_by_key(|l| l.tick);
        let at = a.locators.iter().position(|l| l.tick == tick).unwrap_or(0);
        self.mark_edit("Add Locator");
        at
    }

    /// Adds a locator now: quantized while playing, else at the insert marker or `at`.
    pub fn set_locator(&mut self, at: u64) -> usize {
        if self.is_playing() {
            let tick = self.engine.shared.tick.load();
            let snapped = quantized_tick(tick, self.project.quantize, &self.project.bar_map());
            self.add_locator(snapped)
        } else {
            self.add_locator(at)
        }
    }

    pub fn delete_locator(&mut self, index: usize) {
        if index < self.project.arrangement.locators.len() {
            self.project.arrangement.locators.remove(index);
            self.mark_edit("Delete Locator");
        }
    }

    pub fn move_locator(&mut self, index: usize, tick: u64) -> usize {
        let a = &mut self.project.arrangement;
        if index >= a.locators.len() {
            return index;
        }
        let name = a.locators[index].name.clone();
        let from = a.locators[index].tick;
        a.locators[index].tick = tick;
        a.locators.sort_by_key(|l| l.tick);
        self.project.move_locator_mappings(from, tick);
        let a = &mut self.project.arrangement;
        let at = a
            .locators
            .iter()
            .position(|l| l.tick == tick && l.name == name)
            .unwrap_or(index);
        self.mark_edit("Move Locator");
        at
    }

    pub fn rename_locator(&mut self, index: usize, name: String) {
        if let Some(l) = self.project.arrangement.locators.get_mut(index) {
            l.name = name;
            self.mark_edit("Rename Locator");
        }
    }

    /// Jumps to the next locator, quantized.
    pub fn next_locator(&mut self) {
        let from = self.position();
        let target = next_locator(&self.locator_ticks(), from, self.project.arrangement_end());
        self.trigger(target, false);
    }

    /// Jumps to the previous locator, quantized.
    pub fn prev_locator(&mut self) {
        let from = self.position();
        let target = prev_locator(&self.locator_ticks(), from);
        self.trigger(target, false);
    }

    fn position(&self) -> f64 {
        if self.is_playing() {
            self.engine.shared.tick.load()
        } else {
            self.insert_marker()
        }
    }

    /// Loops between a locator and the one after it, or the end of the Arrangement.
    pub fn loop_to_next_locator(&mut self, index: usize) {
        let ticks = self.locator_ticks();
        let Some(&start) = self
            .project
            .arrangement
            .locators
            .get(index)
            .map(|l| &l.tick)
        else {
            return;
        };
        let end = next_locator(&ticks, start as f64, self.project.arrangement_end()) as u64;
        if end > start {
            self.set_loop(true, start, end);
        }
    }

    /// Makes `tick` the place playback starts, or clears it if it already is.
    pub fn toggle_song_start(&mut self, tick: u64) {
        let a = &mut self.project.arrangement;
        a.song_start = (a.song_start != Some(tick)).then_some(tick);
        self.mark_edit("Set Song Start Time");
    }

    // ------------------------------------------------------------- signature markers

    fn sync_signatures(&mut self, label: &'static str) {
        self.sync_time_maps();
        self.mark_edit(label);
    }

    pub fn add_signature_marker(&mut self, tick: u64, signature: TimeSignature) {
        self.project.add_signature(tick, signature);
        self.sync_signatures("Insert Time Signature");
    }

    pub fn set_signature_marker(&mut self, index: usize, signature: TimeSignature) {
        if let Some(m) = self.project.arrangement.signatures.get_mut(index) {
            m.signature = signature;
            self.sync_signatures("Edit Time Signature");
        }
    }

    /// Moves a marker, unquantized, and returns where it ended up.
    pub fn move_signature_marker(&mut self, index: usize, tick: u64) -> usize {
        let a = &mut self.project.arrangement;
        if index >= a.signatures.len() {
            return index;
        }
        let m = a.signatures.remove(index);
        a.signatures.retain(|o| o.tick != tick);
        a.signatures
            .push(rlive_project::SignatureMarker { tick, ..m });
        a.signatures.sort_by_key(|o| o.tick);
        let at = a
            .signatures
            .iter()
            .position(|o| o.tick == tick)
            .unwrap_or(0);
        self.sync_signatures("Move Time Signature");
        at
    }

    pub fn delete_signature_marker(&mut self, index: usize) {
        if index < self.project.arrangement.signatures.len() {
            self.project.arrangement.signatures.remove(index);
            self.sync_signatures("Delete Time Signature");
        }
    }

    /// Changes the signature in force at `tick`: its marker if there is one, else the song's.
    pub fn set_signature_at(&mut self, tick: f64, signature: TimeSignature) {
        let marker = self
            .project
            .arrangement
            .signatures
            .iter()
            .rposition(|m| m.tick as f64 <= tick);
        match marker {
            Some(i) => self.set_signature_marker(i, signature),
            None => self.set_time_signature(signature),
        }
    }

    // ------------------------------------------------------------- transport

    /// Plays on from where the transport last stopped.
    pub fn continue_play(&mut self) {
        self.send(EngineCommand::Transport(TransportCmd::Continue));
    }

    /// Launches from `tick`, at the next launch point if already playing.
    pub fn trigger(&mut self, tick: f64, hold: bool) {
        self.send(EngineCommand::Transport(TransportCmd::Trigger {
            to: tick,
            hold,
        }));
    }

    pub fn release_hold(&mut self) {
        self.send(EngineCommand::Transport(TransportCmd::ReleaseHold));
    }

    /// A click in the scrub area: playback from the quantized position under it.
    pub fn scrub(&mut self, tick: f64, hold: bool) {
        let to = self
            .project
            .bar_map()
            .prev_boundary(tick.max(0.0), self.project.quantize);
        self.trigger(to, hold);
    }

    pub fn stop_at(&mut self, tick: Option<f64>) {
        self.send(EngineCommand::Transport(TransportCmd::StopAt(tick)));
    }

    pub fn set_chase(&mut self, on: bool) {
        self.send(EngineCommand::Transport(TransportCmd::SetChase(on)));
    }

    /// Whether to apply the default fades. Reapplies every lane when it changes.
    pub fn set_edge_fades(&mut self, on: bool) {
        if self.edge_fades == on {
            return;
        }
        self.edge_fades = on;
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for t in tracks {
            self.sync_lane(t);
        }
    }

    pub fn loop_edit(&mut self, edit: LoopEdit) {
        let a = &self.project.arrangement;
        let (enabled, start, end) = (a.loop_enabled, a.loop_start, a.loop_end);
        let (s, e) = edit.apply(start, end);
        self.set_loop(enabled, s, e);
    }

    /// Turns the loop on over a selection; again over the same one, switches it off.
    pub fn loop_selection(&mut self, start: u64, end: u64) {
        let a = &self.project.arrangement;
        if a.loop_enabled && a.loop_start == start && a.loop_end == end {
            self.set_loop(false, start, end);
        } else if end > start {
            self.set_loop(true, start, end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::PPQ;

    #[test]
    fn nudging_keeps_the_length_and_stops_at_zero() {
        assert_eq!(LoopEdit::Nudge(240).apply(960, 1920), (1200, 2160));
        assert_eq!(LoopEdit::Nudge(-5000).apply(960, 1920), (0, 960));
    }

    #[test]
    fn shifting_moves_by_whole_lengths() {
        assert_eq!(LoopEdit::Shift(1).apply(960, 1920), (1920, 2880));
        assert_eq!(LoopEdit::Shift(-1).apply(960, 1920), (0, 960));
    }

    #[test]
    fn resizing_keeps_a_tick() {
        assert_eq!(LoopEdit::Resize(240).apply(960, 1920), (960, 2160));
        assert_eq!(LoopEdit::Resize(-9999).apply(960, 1920), (960, 961));
    }

    #[test]
    fn doubling_and_halving_keep_the_start() {
        assert_eq!(LoopEdit::Double.apply(960, 1920), (960, 2880));
        assert_eq!(LoopEdit::Halve.apply(960, 2880), (960, 1920));
        assert_eq!(LoopEdit::Halve.apply(10, 11), (10, 11));
    }

    #[test]
    fn locators_quantize_to_the_nearest_launch_point() {
        let bars = BarMap::default();
        let bar = (PPQ * 4) as f64;
        assert_eq!(quantized_tick(bar * 2.2, Quantize::Bar, &bars), 2 * 4 * PPQ);
        assert_eq!(quantized_tick(bar * 2.8, Quantize::Bar, &bars), 3 * 4 * PPQ);
        assert_eq!(quantized_tick(1234.0, Quantize::None, &bars), 1234);
        assert_eq!(quantized_tick(1234.0, Quantize::Beat, &bars), 960);
    }

    #[test]
    fn next_goes_through_the_locators_to_the_end() {
        let t = [960, 1920];
        assert_eq!(next_locator(&t, 0.0, 9000), 960.0);
        assert_eq!(next_locator(&t, 960.0, 9000), 1920.0);
        assert_eq!(next_locator(&t, 1920.0, 9000), 9000.0);
    }

    #[test]
    fn previous_goes_through_the_locators_to_the_start() {
        let t = [960, 1920];
        assert_eq!(prev_locator(&t, 3000.0), 1920.0);
        assert_eq!(prev_locator(&t, 1920.0), 960.0);
        assert_eq!(prev_locator(&t, 960.0), 0.0);
    }
}
