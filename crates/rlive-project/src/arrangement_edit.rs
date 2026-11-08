//! Editing the Arrangement without reference to the UI or the engine: fades and their limits,
//! splitting, sliding, the "... Time" commands, and signature markers.

use crate::envelope_edit::{self, RangeClip};
use crate::{ArrangementClip, ClipContent, Envelope, Project, SignatureMarker, Track};
use rlive_core::param::EnvTarget;
use rlive_core::time::BarMap;
use rlive_core::{ArrClipId, ParamRange, TimeSignature, TrackId, PPQ};
use std::collections::HashMap;

/// Reads a typed time signature: a one- or two-digit numerator and a denominator of 1, 2, 4, 8
/// or 16, separated by a slash, comma, period or spaces.
pub fn parse_signature(text: &str) -> Option<TimeSignature> {
    let mut fields = text
        .split(|c: char| c == '/' || c == ',' || c == '.' || c.is_whitespace())
        .filter(|f| !f.is_empty());
    let numerator: u8 = fields.next()?.parse().ok()?;
    let denominator: u8 = fields.next()?.parse().ok()?;
    (fields.next().is_none()
        && (1..=99).contains(&numerator)
        && matches!(denominator, 1 | 2 | 4 | 8 | 16))
    .then_some(TimeSignature {
        numerator,
        denominator,
    })
}

/// How far a clip's fades can reach before they'd cross a loop boundary or run off its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FadeRoom {
    pub fade_in: u64,
    pub fade_out: u64,
    /// How far it can sound before its start.
    pub lead: u64,
    /// How far it can sound after its end.
    pub tail: u64,
}

/// The fades a clip plays with: what it has, with Create Fades on Clip Edges filling in defaults
/// wherever it has none.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EffectiveFades {
    pub fade_in: u64,
    pub fade_out: u64,
    pub in_curve: f32,
    pub out_curve: f32,
    pub lead: u64,
    pub tail: u64,
}

impl ArrangementClip {
    pub fn is_audio(&self) -> bool {
        matches!(self.clip.content, ClipContent::Audio { .. })
    }

    pub fn fade_room(&self) -> FadeRoom {
        // What can sound is decided by the clip's run from its start marker.
        let run = self.run_offset();
        if self.clip.looped {
            let at = self.run_pass(run);
            let end = run + self.length;
            let last = self.run_pass(end.saturating_sub(1));
            FadeRoom {
                fade_in: (at.1 - run).min(self.length),
                fade_out: (end - last.0).min(self.length),
                lead: run - at.0,
                tail: last.1 - end,
            }
        } else {
            // A loop recording keeps earlier passes before its start marker, in the same sample,
            // and a crossfade mustn't dig them up: the run begins at the start marker.
            let end = self.offset.max(self.clip.start) + self.length;
            FadeRoom {
                fade_in: self.length,
                fade_out: self.length,
                lead: run,
                tail: self.clip.end_marker().saturating_sub(end),
            }
        }
    }

    /// The stretch of a looped clip's run that `run` falls in, which plays without a jump: from
    /// the start marker to the end of the loop, or one time round it after that.
    fn run_pass(&self, run: u64) -> (u64, u64) {
        let region = self.clip.region();
        let length = self.clip.length.max(1);
        // A start marker inside the loop makes the first pass short; before it, long.
        let first_end = (region.loop_end() - region.start).round().max(1.0) as u64;
        if run < first_end {
            return (0, first_end);
        }
        let k = (run - first_end) / length;
        (first_end + k * length, first_end + (k + 1) * length)
    }

    /// The most a fade in can be: within its room, and short of the fade out.
    pub fn fade_in_limit(&self) -> u64 {
        self.fade_room()
            .fade_in
            .min(self.length.saturating_sub(self.clip.fades.fade_out))
    }

    /// The most a fade out can be: within its room, and short of the fade in.
    pub fn fade_out_limit(&self) -> u64 {
        self.fade_room()
            .fade_out
            .min(self.length.saturating_sub(self.clip.fades.fade_in))
    }

    pub fn set_fade_in(&mut self, ticks: u64) {
        self.clip.fades.fade_in = ticks.min(self.fade_in_limit());
    }

    pub fn set_fade_out(&mut self, ticks: u64) {
        self.clip.fades.fade_out = ticks.min(self.fade_out_limit());
    }

    /// Cuts the clip at `at`, which must be inside it: the first part keeps the id.
    pub fn split(&self, at: u64) -> Option<(Self, Self)> {
        if at <= self.start || at >= self.end() {
            return None;
        }
        let mut left = self.clone();
        left.length = at - self.start;
        left.clip.fades.fade_out = 0;
        left.clip.fades.out_tail = 0;
        let mut right = self.clone();
        right.id = ArrClipId::new();
        right.start = at;
        right.length = self.end() - at;
        right.offset = self.offset + (at - self.start);
        right.clip.fades.fade_in = 0;
        right.clip.fades.in_tail = 0;
        Some((left, right))
    }

    /// Moves the content under the clip's edges, later for a positive `delta`, as far as the
    /// content allows. Looped content wraps around.
    pub fn slide_content(&mut self, delta: i64) {
        let start = self.clip.start as i64;
        if self.clip.looped {
            let content = self.clip.length.max(1) as i64;
            // Wrapping stays inside the part of the run that repeats.
            let lead = (self.clip.region().lead() as i64).max(0);
            let run = self.run_offset() as i64 - delta;
            let run = if run < lead || run >= lead + content {
                lead + (run - lead).rem_euclid(content)
            } else {
                run
            };
            self.offset = (start + run) as u64;
        } else {
            let max = (self.clip.end_marker() as i64 - self.length as i64).max(start);
            self.offset = (self.offset as i64 - delta).clamp(start, max) as u64;
        }
    }

    /// The part of the clip inside `[start, end)`, placed relative to `origin`.
    fn clipped_to(&self, start: u64, end: u64, origin: u64) -> Option<Self> {
        if self.end() <= start || self.start >= end {
            return None;
        }
        let mut c = self.clone();
        if c.start < start {
            c.trim_left(start);
        }
        if c.end() > end {
            c.trim_right(end);
        }
        c.start -= origin;
        Some(c)
    }
}

impl Track {
    /// After an edit, keeps fades within their limits and drops crossfades with no neighbour.
    pub fn settle_fades(&mut self) {
        for i in 0..self.arrangement.len() {
            let prev = i.checked_sub(1).map(|p| self.arrangement[p].end());
            let c = &mut self.arrangement[i];
            if !c.is_audio() {
                c.clip.fades = Default::default();
                continue;
            }
            if prev != Some(c.start) {
                c.clip.fades.in_tail = 0;
            }
            let room = c.fade_room();
            let f = &mut c.clip.fades;
            f.in_tail = f.in_tail.min(room.lead);
            f.out_tail = f.out_tail.min(room.tail);
            f.fade_in = f.fade_in.min(room.fade_in);
            f.fade_out = f.fade_out.min(room.fade_out);
            let length = c.length;
            let f = &mut c.clip.fades;
            if f.fade_in + f.fade_out > length {
                f.fade_out = length.saturating_sub(f.fade_in);
            }
            let adjacent = self
                .arrangement
                .get(i + 1)
                .map(|n| n.start == self.arrangement[i].end() && n.is_audio())
                == Some(true);
            if !adjacent {
                self.arrangement[i].clip.fades.out_tail = 0;
            }
        }
        // The two halves of a crossfade describe the one span.
        for i in 1..self.arrangement.len() {
            let (ahead, behind) = self.arrangement.split_at_mut(i);
            let (a, b) = (&mut ahead[i - 1].clip.fades, &mut behind[0].clip.fades);
            if a.out_tail > 0 || b.in_tail > 0 {
                let after = a.out_tail.min(b.fade_in);
                let before = b.in_tail.min(a.fade_out);
                a.out_tail = after;
                b.fade_in = after;
                b.in_tail = before;
                a.fade_out = before;
            }
        }
    }

    /// Crossfades the clip at `i` into the next one, over `before` ticks before their boundary
    /// and `after` ticks after it. Returns what fits, or `None` if they can't crossfade.
    pub fn set_crossfade(
        &mut self,
        i: usize,
        before: u64,
        after: u64,
        curve: f32,
    ) -> Option<(u64, u64)> {
        if !self.can_crossfade(i) {
            return None;
        }
        let (a, b) = (&self.arrangement[i], &self.arrangement[i + 1]);
        let (ra, rb) = (a.fade_room(), b.fade_room());
        let before = before
            .min(ra.fade_out)
            .min(rb.lead)
            .min(a.length.saturating_sub(a.clip.fades.fade_in));
        let after = after
            .min(rb.fade_in)
            .min(ra.tail)
            .min(b.length.saturating_sub(b.clip.fades.fade_out));
        if before == 0 && after == 0 {
            return None;
        }
        let f = &mut self.arrangement[i].clip.fades;
        f.fade_out = before;
        f.out_tail = after;
        f.out_curve = curve;
        let f = &mut self.arrangement[i + 1].clip.fades;
        f.fade_in = after;
        f.in_tail = before;
        f.in_curve = curve;
        Some((before, after))
    }

    /// Takes a clip's fade in off, and with it any crossfade it's part of.
    pub fn delete_fade_in(&mut self, id: ArrClipId) {
        let Some(i) = self.arrangement.iter().position(|c| c.id == id) else {
            return;
        };
        let tail = self.arrangement[i].clip.fades.in_tail > 0;
        let f = &mut self.arrangement[i].clip.fades;
        f.fade_in = 0;
        f.in_tail = 0;
        f.in_curve = 0.0;
        if tail || i > 0 && self.arrangement[i - 1].clip.fades.out_tail > 0 {
            if let Some(prev) = i.checked_sub(1) {
                let f = &mut self.arrangement[prev].clip.fades;
                f.fade_out = 0;
                f.out_tail = 0;
            }
        }
    }

    /// Takes a clip's fade out off, and with it any crossfade it's part of.
    pub fn delete_fade_out(&mut self, id: ArrClipId) {
        let Some(i) = self.arrangement.iter().position(|c| c.id == id) else {
            return;
        };
        let tail = self.arrangement[i].clip.fades.out_tail > 0;
        let f = &mut self.arrangement[i].clip.fades;
        f.fade_out = 0;
        f.out_tail = 0;
        f.out_curve = 0.0;
        if tail
            || self
                .arrangement
                .get(i + 1)
                .is_some_and(|n| n.clip.fades.in_tail > 0)
        {
            if let Some(next) = self.arrangement.get_mut(i + 1) {
                let f = &mut next.clip.fades;
                f.fade_in = 0;
                f.in_tail = 0;
            }
        }
    }

    /// Splits whatever clip is at `at`, returning the new second half.
    pub fn split_at(&mut self, at: u64) -> Option<ArrClipId> {
        let i = self
            .arrangement
            .iter()
            .position(|c| c.start < at && c.end() > at)?;
        let (left, right) = self.arrangement[i].split(at)?;
        let id = right.id;
        self.arrangement[i] = left;
        self.arrangement.insert(i + 1, right);
        Some(id)
    }

    /// Removes the material in `[start, end)`, leaving a gap.
    pub fn delete_span(&mut self, start: u64, end: u64) {
        self.split_at(start);
        self.split_at(end);
        self.arrangement
            .retain(|c| c.end() <= start || c.start >= end);
    }

    /// Copies of what's in `[start, end)`, placed from zero.
    pub fn copy_span(&self, start: u64, end: u64) -> Vec<ArrangementClip> {
        self.arrangement
            .iter()
            .filter_map(|c| c.clipped_to(start, end, start))
            .collect()
    }

    /// Whether the clips at `i` and `i + 1` touch and can crossfade.
    pub fn can_crossfade(&self, i: usize) -> bool {
        can_crossfade(&self.arrangement, i)
    }

    /// See [`effective_fades`].
    pub fn effective_fades(&self, edge: Option<u64>) -> Vec<EffectiveFades> {
        effective_fades(&self.arrangement, edge)
    }
}

fn can_crossfade(clips: &[ArrangementClip], i: usize) -> bool {
    match (clips.get(i), clips.get(i + 1)) {
        (Some(a), Some(b)) => a.end() == b.start && a.is_audio() && b.is_audio(),
        _ => false,
    }
}

/// Effective fades for every clip on a lane. With `edge` (Create Fades on Clip Edges, in ticks) a
/// clip's fades of length zero become that, and touching clips crossfade.
pub fn effective_fades(clips: &[ArrangementClip], edge: Option<u64>) -> Vec<EffectiveFades> {
    let mut out: Vec<EffectiveFades> = clips
        .iter()
        .map(|c| {
            let f = c.clip.fades;
            let mut e = EffectiveFades {
                fade_in: f.fade_in,
                fade_out: f.fade_out,
                in_curve: f.in_curve,
                out_curve: f.out_curve,
                lead: f.in_tail,
                tail: f.out_tail,
            };
            if let Some(edge) = edge.filter(|_| c.is_audio()) {
                if e.fade_in == 0 && e.lead == 0 {
                    e.fade_in = edge.min(c.fade_in_limit()).min(c.length / 2);
                }
                if e.fade_out == 0 && e.tail == 0 {
                    e.fade_out = edge.min(c.fade_out_limit()).min(c.length / 2);
                }
            }
            e
        })
        .collect();
    let Some(edge) = edge else {
        return out;
    };
    for i in 0..clips.len().saturating_sub(1) {
        if !can_crossfade(clips, i) {
            continue;
        }
        let (fa, fb) = (clips[i].clip.fades, clips[i + 1].clip.fades);
        if fa.fade_out != 0 || fa.out_tail != 0 || fb.fade_in != 0 || fb.in_tail != 0 {
            continue;
        }
        let (ra, rb) = (clips[i].fade_room(), clips[i + 1].fade_room());
        let half = edge / 2;
        let (before, after) = (
            half.min(ra.fade_out).min(rb.lead),
            half.min(rb.fade_in).min(ra.tail),
        );
        if before == 0 && after == 0 {
            continue;
        }
        out[i].fade_out = before;
        out[i].tail = after;
        out[i + 1].fade_in = after;
        out[i + 1].lead = before;
    }
    out
}

/// How long `ms` milliseconds is at `bpm`, in ticks.
pub fn ms_to_ticks(ms: f64, bpm: f64) -> u64 {
    (ms / 60_000.0 * bpm * PPQ as f64).ceil().max(1.0) as u64
}

/// A stretch of the whole Arrangement, as Copy Time and Cut Time capture it.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeClip {
    pub length: u64,
    pub lanes: Vec<(TrackId, Vec<ArrangementClip>)>,
    envelopes: Vec<(TrackId, EnvTarget, RangeClip)>,
    tempo: Option<RangeClip>,
    signatures: Vec<SignatureMarker>,
}

/// A fragmentary bar: one a signature marker cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    /// The marker that cut it, as an index into the signatures.
    pub marker: usize,
    pub start: u64,
    pub end: u64,
    /// Ticks missing from a full bar of the signature it is in.
    pub missing: u64,
}

impl Project {
    /// The meter over the song.
    pub fn bar_map(&self) -> BarMap {
        BarMap::new(
            self.time_signature,
            self.arrangement
                .signatures
                .iter()
                .map(|m| (m.tick, m.signature)),
        )
    }

    /// The fragmentary bars the signature markers have made, in time order.
    pub fn fragments(&self) -> Vec<Fragment> {
        let bars = self.bar_map();
        self.arrangement
            .signatures
            .iter()
            .enumerate()
            .filter_map(|(marker, m)| {
                let bar = bars.bar_at(m.tick as f64 - 0.5);
                (m.tick > 0 && bar.is_fragment() && bar.end == m.tick as f64).then(|| Fragment {
                    marker,
                    start: bar.start as u64,
                    end: m.tick,
                    missing: (bar.start + bar.signature.ticks_per_bar() as f64) as u64 - m.tick,
                })
            })
            .collect()
    }

    fn timelines(&mut self) -> impl Iterator<Item = &mut Track> {
        self.tracks
            .iter_mut()
            .chain(self.returns.iter_mut())
            .chain(std::iter::once(&mut self.master))
    }

    fn sort_markers(&mut self) {
        let a = &mut self.arrangement;
        a.signatures.sort_by_key(|m| m.tick);
        a.locators.sort_by_key(|l| l.tick);
    }

    /// Captures `[start, end)` of every lane, automation envelope and signature marker.
    pub fn copy_time(
        &self,
        start: u64,
        end: u64,
        range_of: &dyn Fn(TrackId, &EnvTarget) -> ParamRange,
    ) -> TimeClip {
        let (a, b) = (start as f64, end as f64);
        let lanes = self
            .tracks
            .iter()
            .map(|t| (t.id, t.copy_span(start, end)))
            .collect();
        let mut envelopes = Vec::new();
        for t in self
            .tracks
            .iter()
            .chain(&self.returns)
            .chain(std::iter::once(&self.master))
        {
            for e in &t.automation {
                envelopes.push((
                    t.id,
                    e.target,
                    envelope_edit::copy_range(&e.points, a, b, range_of(t.id, &e.target)),
                ));
            }
        }
        let tempo = (!self.arrangement.tempo.points.is_empty()).then(|| {
            envelope_edit::copy_range(&self.arrangement.tempo.points, a, b, self.tempo_range())
        });
        let signatures = self
            .arrangement
            .signatures
            .iter()
            .filter(|m| m.tick >= start && m.tick < end)
            .map(|m| SignatureMarker {
                tick: m.tick - start,
                ..*m
            })
            .collect();
        TimeClip {
            length: end - start,
            lanes,
            envelopes,
            tempo,
            signatures,
        }
    }

    fn tempo_range(&self) -> ParamRange {
        let t = &self.arrangement.tempo;
        ParamRange {
            min: t.min,
            max: t.max,
            skew: 1.0,
            quantized: false,
        }
    }

    /// Removes `[start, end)` from the whole Arrangement, closing the gap.
    pub fn delete_time(
        &mut self,
        start: u64,
        end: u64,
        range_of: &dyn Fn(TrackId, &EnvTarget) -> ParamRange,
    ) {
        if end <= start {
            return;
        }
        let len = end - start;
        for t in &mut self.tracks {
            t.delete_span(start, end);
            for c in &mut t.arrangement {
                if c.start >= end {
                    c.start -= len;
                }
            }
            t.arrangement.sort_by_key(|c| c.start);
            t.settle_fades();
        }
        let tempo = self.tempo_range();
        let tempo_points = envelope_edit::delete_time(
            &self.arrangement.tempo.points,
            start as f64,
            end as f64,
            tempo,
        );
        self.arrangement.tempo.points = tempo_points;
        for t in self.timelines() {
            let id = t.id;
            for e in &mut t.automation {
                let range = range_of(id, &e.target);
                e.points = envelope_edit::delete_time(&e.points, start as f64, end as f64, range);
            }
        }
        let a = &mut self.arrangement;
        a.signatures.retain(|m| m.tick < start || m.tick >= end);
        for m in &mut a.signatures {
            if m.tick >= end {
                m.tick -= len;
            }
        }
        a.locators.retain(|l| l.tick < start || l.tick >= end);
        for l in &mut a.locators {
            if l.tick >= end {
                l.tick -= len;
            }
        }
        if let Some(s) = a.song_start.filter(|s| *s >= end) {
            a.song_start = Some(s - len);
        } else if a.song_start.is_some_and(|s| s >= start) {
            a.song_start = Some(start);
        }
        self.sort_markers();
    }

    /// Inserts empty time at `at`. Clips across it are split, and everything after it moves on.
    pub fn insert_time(
        &mut self,
        at: u64,
        len: u64,
        range_of: &dyn Fn(TrackId, &EnvTarget) -> ParamRange,
    ) {
        if len == 0 {
            return;
        }
        for t in &mut self.tracks {
            t.split_at(at);
            for c in &mut t.arrangement {
                if c.start >= at {
                    c.start += len;
                }
            }
            t.arrangement.sort_by_key(|c| c.start);
            t.settle_fades();
        }
        let tempo = self.tempo_range();
        let moved = envelope_edit::insert_time(
            &self.arrangement.tempo.points,
            at as f64,
            len as f64,
            tempo,
        );
        self.arrangement.tempo.points = moved;
        for t in self.timelines() {
            let id = t.id;
            for e in &mut t.automation {
                let range = range_of(id, &e.target);
                e.points = envelope_edit::insert_time(&e.points, at as f64, len as f64, range);
            }
        }
        let a = &mut self.arrangement;
        for m in &mut a.signatures {
            if m.tick >= at {
                m.tick += len;
            }
        }
        for l in &mut a.locators {
            if l.tick >= at {
                l.tick += len;
            }
        }
        if let Some(s) = a.song_start.filter(|s| *s >= at) {
            a.song_start = Some(s + len);
        }
        self.sort_markers();
    }

    /// Opens up the clip's length at `at` and lays it into the gap.
    pub fn paste_time(
        &mut self,
        at: u64,
        clip: &TimeClip,
        range_of: &dyn Fn(TrackId, &EnvTarget) -> ParamRange,
    ) -> Vec<(TrackId, ArrClipId)> {
        self.insert_time(at, clip.length, range_of);
        let mut placed = Vec::new();
        for (id, clips) in &clip.lanes {
            let Some(t) = self.tracks.iter_mut().find(|t| t.id == *id) else {
                continue;
            };
            for c in clips {
                let new = ArrangementClip {
                    id: ArrClipId::new(),
                    start: c.start + at,
                    ..c.clone()
                };
                placed.push((*id, new.id));
                t.place_clip(new);
            }
            t.settle_fades();
        }
        for (id, target, range_clip) in &clip.envelopes {
            let range = range_of(*id, target);
            let Some(e) = self.track_envelope_mut(*id, target) else {
                continue;
            };
            let before = envelope_edit::value_at(&e.points, at as f64, range).unwrap_or(0.0);
            e.points = envelope_edit::paste_range(&e.points, range_clip, at as f64, before, range);
        }
        if let Some(range_clip) = &clip.tempo {
            let range = self.tempo_range();
            let points = &self.arrangement.tempo.points;
            let before = envelope_edit::value_at(points, at as f64, range).unwrap_or(0.0);
            self.arrangement.tempo.points =
                envelope_edit::paste_range(points, range_clip, at as f64, before, range);
        }
        self.arrangement
            .signatures
            .extend(clip.signatures.iter().map(|m| SignatureMarker {
                tick: m.tick + at,
                ..*m
            }));
        self.sort_markers();
        placed
    }

    fn track_envelope_mut(&mut self, id: TrackId, target: &EnvTarget) -> Option<&mut Envelope> {
        self.timelines()
            .find(|t| t.id == id)?
            .automation
            .iter_mut()
            .find(|e| e.target == *target)
    }

    /// Removes a fragmentary bar's time. The marker that cut it falls on the barline before it.
    pub fn delete_fragment(
        &mut self,
        fragment: Fragment,
        range_of: &dyn Fn(TrackId, &EnvTarget) -> ParamRange,
    ) {
        self.delete_time(fragment.start, fragment.end, range_of);
    }

    /// Inserts time at the start of a fragmentary bar, to make it a whole one.
    pub fn complete_fragment(
        &mut self,
        fragment: Fragment,
        range_of: &dyn Fn(TrackId, &EnvTarget) -> ParamRange,
    ) {
        self.insert_time(fragment.start, fragment.missing, range_of);
    }

    /// Time signatures read from an imported MIDI file become markers. For A MIDI file import
    /// to call when its option is on; nothing imports MIDI files yet.
    pub fn import_signatures(&mut self, markers: &[(u64, TimeSignature)]) {
        for (tick, signature) in markers {
            self.add_signature(*tick, *signature);
        }
    }

    /// Adds a time signature marker, unquantized, keeping them in time order. One already
    /// at the tick is replaced.
    pub fn add_signature(&mut self, tick: u64, signature: TimeSignature) {
        let a = &mut self.arrangement;
        match a.signatures.iter_mut().find(|m| m.tick == tick) {
            Some(m) => m.signature = signature,
            None => a.signatures.push(SignatureMarker { tick, signature }),
        }
        a.signatures.sort_by_key(|m| m.tick);
    }
}

/// Ranges for tests, and anywhere that doesn't care.
pub fn flat_range(_: TrackId, _: &EnvTarget) -> ParamRange {
    ParamRange {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        quantized: false,
    }
}

/// A map of ranges, as a function for the Time commands.
pub fn ranges_by(
    ranges: HashMap<(TrackId, EnvTarget), ParamRange>,
) -> impl Fn(TrackId, &EnvTarget) -> ParamRange {
    move |id, target| {
        ranges.get(&(id, *target)).copied().unwrap_or(ParamRange {
            min: 0.0,
            max: 1.0,
            skew: 1.0,
            quantized: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Breakpoint, Clip, ClipContent, Project, TrackKind, PALETTE};

    fn audio(length: u64, looped: bool) -> Clip {
        Clip {
            name: "a".into(),
            color: PALETTE[0],
            length,
            looped,
            content: ClipContent::audio("x.wav", 0.0),
            ..Clip::default()
        }
    }

    fn placed(start: u64, length: u64, offset: u64, content: u64, looped: bool) -> ArrangementClip {
        ArrangementClip {
            id: ArrClipId::new(),
            start,
            length,
            offset,
            clip: audio(content, looped),
        }
    }

    fn track_with(clips: Vec<ArrangementClip>) -> Track {
        let mut t = Track::new("t", TrackKind::Audio, PALETTE[0], 1);
        for c in clips {
            t.place_clip(c);
        }
        t
    }

    #[test]
    fn signatures_parse_every_delimiter() {
        for (text, n, d) in [
            ("3/4", 3, 4),
            ("6,8", 6, 8),
            ("5.16", 5, 16),
            ("7   8", 7, 8),
            ("99/1", 99, 1),
            ("12/16", 12, 16),
        ] {
            assert_eq!(
                parse_signature(text),
                Some(TimeSignature {
                    numerator: n,
                    denominator: d
                }),
                "{text}"
            );
        }
    }

    #[test]
    fn signatures_reject_what_the_manual_rules_out() {
        for text in ["4", "0/4", "100/4", "3/3", "3/32", "3/4/4", "a/b", ""] {
            assert_eq!(parse_signature(text), None, "{text}");
        }
    }

    #[test]
    fn looped_fades_cannot_cross_a_loop_boundary() {
        let c = placed(0, 2000, 300, 1000, true);
        let room = c.fade_room();
        assert_eq!(room.fade_in, 700, "to the next wrap");
        assert_eq!(room.fade_out, 300, "back to the wrap before the end");
        assert_eq!(room.lead, 300);
        assert_eq!(room.tail, 700);
    }

    #[test]
    fn unlooped_fades_stay_on_the_content() {
        let c = placed(0, 500, 200, 1000, false);
        let room = c.fade_room();
        assert_eq!((room.fade_in, room.fade_out), (500, 500));
        assert_eq!((room.lead, room.tail), (200, 300));
    }

    #[test]
    fn a_recorded_pass_cannot_lead_into_the_pass_before_it() {
        // The brace is around the pass; the sample has every pass.
        let mut c = placed(0, 1000, 2000, 1000, false);
        c.clip.start = 2000;
        c.clip.loop_start = 2000;
        assert_eq!(c.fade_room().lead, 0);
        assert_eq!(c.fade_room().tail, 0);
        let (l, r) = c.split(400).unwrap();
        assert_eq!(l.fade_room().lead, 0);
        assert_eq!(l.fade_room().tail, 600, "the rest of the same pass");
        assert_eq!(r.fade_room().lead, 400, "the part split off before it");
        assert_eq!(r.fade_room().tail, 0);
    }

    #[test]
    fn a_looped_clips_room_runs_from_its_start_marker() {
        // Loop of beats 2-4, run in from the top: the run-in and first pass have no seam.
        let mut c = placed(0, 500, 1000, 1000, true);
        c.clip.loop_start = 2000;
        assert_eq!(c.fade_room().lead, 1000, "back to the start marker");
        assert_eq!(c.fade_room().tail, 1500, "on to the end of the loop");
        // A start marker inside the loop: the first pass is short.
        let mut c = placed(0, 100, 2600, 1000, true);
        c.clip.loop_start = 2000;
        c.clip.start = 2500;
        assert_eq!(c.fade_room().lead, 100);
        assert_eq!(c.fade_room().tail, 300);
        // Past it, the same room as any other pass.
        let mut c = placed(0, 100, 3100, 1000, true);
        c.clip.loop_start = 2000;
        c.clip.start = 2500;
        assert_eq!(c.fade_room().lead, 100, "just round the loop");
    }

    #[test]
    fn an_unlooped_clips_room_stops_at_its_markers() {
        let mut c = placed(0, 500, 1000, 4000, false);
        c.clip.start = 800;
        c.clip.end = Some(2000);
        assert_eq!(c.fade_room().lead, 200);
        assert_eq!(c.fade_room().tail, 500);
    }

    #[test]
    fn sliding_stays_inside_a_clips_markers() {
        let mut c = placed(0, 500, 1000, 4000, false);
        c.clip.start = 800;
        c.clip.end = Some(2000);
        c.slide_content(10_000);
        assert_eq!(c.offset, 800, "not before the start marker");
        c.slide_content(-10_000);
        assert_eq!(
            c.offset, 1500,
            "nor so far its right edge is past the end marker"
        );
    }

    #[test]
    fn a_clips_fades_cannot_overlap() {
        let mut c = placed(0, 1000, 0, 1000, false);
        c.set_fade_in(700);
        c.set_fade_out(700);
        assert_eq!(c.clip.fades.fade_in, 700);
        assert_eq!(c.clip.fades.fade_out, 300);
    }

    #[test]
    fn split_keeps_each_fades_at_its_own_edge() {
        let mut c = placed(0, 1000, 100, 2000, false);
        c.clip.fades.fade_in = 50;
        c.clip.fades.fade_out = 60;
        let id = c.id;
        let (l, r) = c.split(400).unwrap();
        assert_eq!(l.id, id);
        assert_eq!((l.start, l.length, l.offset), (0, 400, 100));
        assert_eq!(l.clip.fades.fade_in, 50);
        assert_eq!(l.clip.fades.fade_out, 0);
        assert_eq!((r.start, r.length, r.offset), (400, 600, 500));
        assert_eq!(r.clip.fades.fade_in, 0);
        assert_eq!(r.clip.fades.fade_out, 60);
        assert_ne!(r.id, id);
        assert!(c.split(0).is_none() && c.split(1000).is_none());
    }

    #[test]
    fn sliding_keeps_edges_and_stays_on_the_content() {
        let mut c = placed(100, 400, 200, 1000, false);
        c.slide_content(500);
        assert_eq!(c.offset, 0, "content moved later ran out at the start");
        assert_eq!((c.start, c.length), (100, 400));
        c.slide_content(-5000);
        assert_eq!(c.offset, 600, "stops at the end of the content");
    }

    #[test]
    fn looped_content_slides_around() {
        let mut c = placed(0, 3000, 100, 1000, true);
        c.slide_content(150);
        assert_eq!(c.offset, 950);
    }

    #[test]
    fn a_crossfade_fits_inside_both_clips_loops() {
        let a = placed(0, 1000, 0, 1500, false);
        let b = placed(1000, 1000, 400, 1500, false);
        let mut t = track_with(vec![a, b]);
        assert_eq!(t.set_crossfade(0, 9999, 9999, 0.2), Some((400, 500)));
        let (fa, fb) = (t.arrangement[0].clip.fades, t.arrangement[1].clip.fades);
        assert_eq!((fa.fade_out, fa.out_tail), (400, 500));
        assert_eq!((fb.fade_in, fb.in_tail), (500, 400));
        assert_eq!(fa.out_curve, 0.2);
        assert_eq!(fb.in_curve, 0.2);
    }

    #[test]
    fn crossfades_need_touching_audio_clips() {
        let a = placed(0, 1000, 0, 1500, false);
        let b = placed(2000, 1000, 400, 1500, false);
        let mut t = track_with(vec![a, b]);
        assert_eq!(t.set_crossfade(0, 100, 100, 0.0), None);
    }

    #[test]
    fn moving_a_clip_away_drops_its_crossfade() {
        let a = placed(0, 1000, 0, 1500, false);
        let b = placed(1000, 1000, 400, 1500, false);
        let mut t = track_with(vec![a, b]);
        t.set_crossfade(0, 100, 200, 0.0);
        t.arrangement[1].start = 5000;
        t.settle_fades();
        assert_eq!(t.arrangement[0].clip.fades.out_tail, 0);
        assert_eq!(t.arrangement[1].clip.fades.in_tail, 0);
    }

    #[test]
    fn a_crossfade_keeps_its_halves_consistent_when_one_edge_shortens() {
        let a = placed(0, 1000, 0, 1500, false);
        let b = placed(1000, 1000, 400, 1500, false);
        let mut t = track_with(vec![a, b]);
        t.set_crossfade(0, 100, 200, 0.0);
        t.arrangement[1].clip.fades.fade_in = 50;
        t.settle_fades();
        assert_eq!(t.arrangement[0].clip.fades.out_tail, 50);
        assert_eq!(t.arrangement[1].clip.fades.fade_in, 50);
    }

    #[test]
    fn deleting_either_half_removes_the_whole_crossfade() {
        let a = placed(0, 1000, 0, 1500, false);
        let b = placed(1000, 1000, 400, 1500, false);
        let (ida, idb) = (a.id, b.id);
        let mut t = track_with(vec![a, b]);
        t.set_crossfade(0, 100, 200, 0.0);
        t.delete_fade_out(ida);
        assert_eq!(t.arrangement[0].clip.fades, Default::default());
        assert_eq!(t.arrangement[1].clip.fades.in_tail, 0);
        t.set_crossfade(0, 100, 200, 0.0);
        t.delete_fade_in(idb);
        assert_eq!(t.arrangement[1].clip.fades.fade_in, 0);
        assert_eq!(t.arrangement[0].clip.fades.out_tail, 0);
    }

    #[test]
    fn edge_fades_fill_in_only_what_the_user_left_at_zero() {
        let mut a = placed(0, 1000, 0, 1500, false);
        a.clip.fades.fade_in = 30;
        let t = track_with(vec![a]);
        let e = t.effective_fades(Some(8));
        assert_eq!(e[0].fade_in, 30);
        assert_eq!(e[0].fade_out, 8);
        assert_eq!(t.effective_fades(None)[0].fade_out, 0);
    }

    #[test]
    fn edge_fades_crossfade_touching_clips_over_the_default_length() {
        let a = placed(0, 1000, 0, 1500, false);
        let b = placed(1000, 1000, 400, 1500, false);
        let t = track_with(vec![a, b]);
        let e = t.effective_fades(Some(8));
        assert_eq!((e[0].fade_out, e[0].tail), (4, 4));
        assert_eq!((e[1].fade_in, e[1].lead), (4, 4));
    }

    #[test]
    fn edge_fades_leave_a_clip_with_no_content_past_its_end_alone() {
        let a = placed(0, 1000, 0, 1000, false);
        let b = placed(1000, 1000, 0, 1500, false);
        let t = track_with(vec![a, b]);
        let e = t.effective_fades(Some(8));
        assert_eq!((e[0].tail, e[1].lead), (0, 0));
        assert_eq!(e[0].fade_out, 8, "no crossfade, so an ordinary short fade");
    }

    fn project() -> (Project, TrackId, EnvTarget) {
        let mut p = Project::new_default();
        let t = &mut p.tracks[0];
        let id = t.id;
        let target = EnvTarget::MidiCtrl(1);
        t.automation.push(Envelope {
            target,
            points: vec![Breakpoint::new(0.0, 0.0), Breakpoint::new(1000.0, 1.0)],
        });
        t.place_clip(placed(100, 800, 0, 2000, false));
        (p, id, target)
    }

    fn clip_spans(p: &Project) -> Vec<(u64, u64)> {
        p.tracks[0]
            .arrangement
            .iter()
            .map(|c| (c.start, c.end()))
            .collect()
    }

    fn times(p: &Project, target: &EnvTarget) -> Vec<f64> {
        p.tracks[0]
            .automation
            .iter()
            .find(|e| e.target == *target)
            .unwrap()
            .points
            .iter()
            .map(|e| e.time)
            .collect()
    }

    #[test]
    fn delete_time_closes_the_gap_in_clips_automation_and_markers() {
        let (mut p, _, target) = project();
        p.add_signature(600, TimeSignature::default());
        p.add_signature(800, TimeSignature::default());
        p.arrangement.locators.push(crate::Locator {
            tick: 900,
            name: "x".into(),
            info: String::new(),
        });
        p.delete_time(400, 700, &flat_range);
        assert_eq!(clip_spans(&p), vec![(100, 400), (400, 600)]);
        assert_eq!(p.tracks[0].arrangement[1].offset, 600);
        let ticks: Vec<u64> = p.arrangement.signatures.iter().map(|m| m.tick).collect();
        assert_eq!(
            ticks,
            vec![500],
            "the one inside is gone, the one after moved"
        );
        assert_eq!(p.arrangement.locators[0].tick, 600);
        assert_eq!(times(&p, &target), vec![0.0, 400.0, 400.0, 700.0]);
    }

    #[test]
    fn insert_time_splits_clips_and_pushes_everything_later() {
        let (mut p, _, target) = project();
        p.add_signature(600, TimeSignature::default());
        p.insert_time(400, 100, &flat_range);
        assert_eq!(clip_spans(&p), vec![(100, 400), (500, 1000)]);
        assert_eq!(p.arrangement.signatures[0].tick, 700);
        assert_eq!(times(&p, &target), vec![0.0, 400.0, 500.0, 1100.0]);
    }

    #[test]
    fn cut_and_paste_time_round_trips() {
        let (mut p, id, target) = project();
        let before = clip_spans(&p);
        p.add_signature(500, TimeSignature::default());
        let clip = p.copy_time(300, 700, &flat_range);
        p.delete_time(300, 700, &flat_range);
        assert_eq!(p.arrangement.signatures.len(), 0);
        let placed = p.paste_time(300, &clip, &flat_range);
        assert_eq!(placed[0].0, id);
        let mut joined = clip_spans(&p);
        joined.dedup();
        assert_eq!(joined.first().unwrap().0, before[0].0);
        assert_eq!(joined.last().unwrap().1, before[0].1);
        assert_eq!(p.arrangement.signatures[0].tick, 500);
        let t = times(&p, &target);
        assert_eq!(t.first(), Some(&0.0));
        assert_eq!(t.last(), Some(&1000.0));
    }

    #[test]
    fn duplicate_time_doubles_the_selection() {
        let (mut p, _, _) = project();
        let clip = p.copy_time(100, 500, &flat_range);
        p.paste_time(500, &clip, &flat_range);
        assert_eq!(
            clip_spans(&p),
            vec![(100, 500), (500, 900), (900, 1300)],
            "original, copy, then what followed"
        );
    }

    #[test]
    fn the_empty_range_does_nothing() {
        let (mut p, _, _) = project();
        let before = clip_spans(&p);
        p.delete_time(300, 300, &flat_range);
        p.insert_time(300, 0, &flat_range);
        assert_eq!(clip_spans(&p), before);
    }

    #[test]
    fn a_marker_mid_bar_makes_a_fragment() {
        let mut p = Project::new_default();
        p.add_signature(
            PPQ * 6,
            TimeSignature {
                numerator: 3,
                denominator: 4,
            },
        );
        let f = p.fragments();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].start, f[0].end), (PPQ * 4, PPQ * 6));
        assert_eq!(f[0].missing, PPQ * 2);
    }

    #[test]
    fn a_marker_on_a_barline_makes_no_fragment() {
        let mut p = Project::new_default();
        p.add_signature(
            PPQ * 8,
            TimeSignature {
                numerator: 3,
                denominator: 4,
            },
        );
        assert!(p.fragments().is_empty());
    }

    #[test]
    fn delete_fragment_time_puts_the_marker_on_the_barline() {
        let mut p = Project::new_default();
        p.add_signature(
            PPQ * 6,
            TimeSignature {
                numerator: 3,
                denominator: 4,
            },
        );
        let f = p.fragments()[0];
        p.delete_fragment(f, &flat_range);
        assert_eq!(p.arrangement.signatures[0].tick, PPQ * 4);
        assert!(p.fragments().is_empty());
    }

    #[test]
    fn complete_fragment_inserts_what_is_missing_at_its_start() {
        let mut p = Project::new_default();
        p.add_signature(
            PPQ * 6,
            TimeSignature {
                numerator: 3,
                denominator: 4,
            },
        );
        let f = p.fragments()[0];
        p.complete_fragment(f, &flat_range);
        assert_eq!(p.arrangement.signatures[0].tick, PPQ * 8);
        assert!(p.fragments().is_empty());
    }

    #[test]
    fn a_new_marker_replaces_one_at_the_same_tick() {
        let mut p = Project::new_default();
        p.add_signature(960, TimeSignature::default());
        p.add_signature(
            960,
            TimeSignature {
                numerator: 6,
                denominator: 8,
            },
        );
        assert_eq!(p.arrangement.signatures.len(), 1);
        assert_eq!(p.arrangement.signatures[0].signature.numerator, 6);
    }

    #[test]
    fn imported_signatures_become_markers_in_order() {
        let mut p = Project::new_default();
        let sig = TimeSignature {
            numerator: 7,
            denominator: 8,
        };
        p.import_signatures(&[(1920, sig), (960, TimeSignature::default())]);
        let ticks: Vec<u64> = p.arrangement.signatures.iter().map(|m| m.tick).collect();
        assert_eq!(ticks, vec![960, 1920]);
    }

    #[test]
    fn a_span_comes_out_without_moving_the_rest() {
        let mut t = track_with(vec![placed(0, 1000, 0, 2000, false)]);
        t.delete_span(200, 300);
        let spans: Vec<(u64, u64)> = t.arrangement.iter().map(|c| (c.start, c.end())).collect();
        assert_eq!(spans, vec![(0, 200), (300, 1000)]);
    }

    #[test]
    fn a_copied_span_starts_from_zero() {
        let t = track_with(vec![placed(100, 1000, 0, 2000, false)]);
        let copy = t.copy_span(50, 300);
        assert_eq!(
            (copy[0].start, copy[0].length, copy[0].offset),
            (50, 200, 0)
        );
    }

    #[test]
    fn ms_convert_at_the_tempo() {
        assert_eq!(ms_to_ticks(4.0, 120.0), 8);
        assert_eq!(ms_to_ticks(500.0, 120.0), 960);
    }
}
