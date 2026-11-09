//! Recording: collects played MIDI into clips while recording onto the timeline, and the parts
//! that get audio to disk and find phrases in what was played.

pub mod capture_midi;
pub mod disk;

use rlive_core::{ArrClipId, ExprKind, MidiMessage, NoteId, TimeSignature, TrackId};
use rlive_project::{Breakpoint, Clip, ClipContent, NoteData, NoteExpression};
use std::collections::HashMap;

/// The shortest note a press and release can make, in ticks.
const MIN_NOTE: u64 = 10;

/// Release velocity of a note that ended with no key-up (a jump or the end of recording).
const DEFAULT_RELEASE: f32 = 0.5;

/// A key that is down, and what it has done so far.
struct OpenNote {
    start: f64,
    velocity: f32,
    id: NoteId,
    expr: NoteExpression,
}

#[derive(Default)]
struct Take {
    open: HashMap<u8, OpenNote>,
    notes: Vec<NoteData>,
    /// The timeline clip holding this take, once it's been placed.
    clip: Option<ArrClipId>,
    /// With Overdub on, what was already on the timeline, by absolute tick.
    base: Vec<NoteData>,
    /// Passes of the loop that have ended, oldest first, and where the one running began.
    finished: Vec<FinishedPass>,
    pass_start: Option<u64>,
}

/// The notes of one time through the loop, and whether it has gone on the timeline yet.
struct FinishedPass {
    start: u64,
    notes: Vec<NoteData>,
    placed: bool,
}

impl Take {
    fn close(&mut self, key: u8, until: f64, release: f32) {
        if let Some(open) = self.open.remove(&key) {
            let start = open.start.max(0.0).round() as u64;
            let length = (until.round() as u64).saturating_sub(start).max(MIN_NOTE);
            let mut note = NoteData::new(start, length, key, open.velocity);
            note.release = release;
            note.expr = thinned(open.expr).map(Box::new);
            self.notes.push(note);
        }
    }

    /// Closes held notes that had no key-up, so they get the default release velocity.
    fn close_all(&mut self, until: f64) {
        let keys: Vec<u8> = self.open.keys().copied().collect();
        for key in keys {
            self.close(key, until, DEFAULT_RELEASE);
        }
    }
}

/// The finest detail recorded expression keeps, in ticks: about 2 ms at 120 BPM.
const POINT_SPACING: f64 = 4.0;
/// How far a straight line may stray from the played gesture before a point is kept.
const THIN_TOLERANCE: f64 = 0.004;

fn curve_of(expr: &mut NoteExpression, kind: ExprKind) -> &mut Vec<Breakpoint> {
    match kind {
        ExprKind::Pitch => &mut expr.pitch,
        ExprKind::Slide => &mut expr.slide,
        ExprKind::Pressure => &mut expr.pressure,
    }
}

/// One more value of a gesture. Bursts closer than [`POINT_SPACING`] keep their latest.
pub(crate) fn push_point(points: &mut Vec<Breakpoint>, time: f64, value: f64) {
    if points.last().is_some_and(|p| p.value == value) {
        return;
    }
    match points.last_mut() {
        Some(last) if time - last.time < POINT_SPACING => last.value = value,
        _ => points.push(Breakpoint::new(time, value)),
    }
}

/// Keeps the points that matter to the shape of a gesture: a played bend is thousands of them,
/// most of them on the way from one to the next.
fn thin_points(points: &[Breakpoint], tolerance: f64) -> Vec<Breakpoint> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut spans = vec![(0, points.len() - 1)];
    while let Some((from, to)) = spans.pop() {
        if to <= from + 1 {
            continue;
        }
        let (a, b) = (points[from], points[to]);
        let span = (b.time - a.time).max(1e-9);
        let (mut worst, mut at) = (0.0, from);
        for (i, p) in points.iter().enumerate().take(to).skip(from + 1) {
            let line = a.value + (b.value - a.value) * (p.time - a.time) / span;
            let error = (p.value - line).abs();
            if error > worst {
                (worst, at) = (error, i);
            }
        }
        if worst > tolerance {
            keep[at] = true;
            spans.push((from, at));
            spans.push((at, to));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// A note's expression as it should be kept, or nothing if it has none.
pub(crate) fn thinned(expr: NoteExpression) -> Option<NoteExpression> {
    let thin = NoteExpression {
        pitch: thin_points(&expr.pitch, 0.01),
        slide: thin_points(&expr.slide, THIN_TOLERANCE),
        pressure: thin_points(&expr.pressure, THIN_TOLERANCE),
    };
    (!thin.pitch.is_empty() || !thin.slide.is_empty() || !thin.pressure.is_empty()).then_some(thin)
}

/// One finished stretch of a take, ready to go on the timeline.
pub struct Placement {
    pub track: TrackId,
    pub existing: Option<ArrClipId>,
    pub start: u64,
    pub clip: Clip,
}

/// A pass of the loop that has finished, ready to be a take.
pub struct TakePass {
    pub track: TrackId,
    pub start: u64,
    pub clip: Clip,
}

pub struct Recording {
    start: u64,
    takes: HashMap<TrackId, Take>,
    /// The clips made from this recording so far, for what happens to them afterwards.
    placed: Vec<(TrackId, ArrClipId)>,
    /// Recording has ended: the pass running is finished too.
    ended: bool,
}

impl Recording {
    pub fn new(start: u64) -> Self {
        Self {
            start,
            takes: HashMap::new(),
            placed: Vec::new(),
            ended: false,
        }
    }

    pub fn message(&mut self, track: TrackId, tick: f64, message: MidiMessage) {
        let take = self.takes.entry(track).or_default();
        match message {
            MidiMessage::NoteOn {
                key,
                velocity,
                note_id,
                ..
            } if velocity > 0.0 => {
                take.close(key, tick, DEFAULT_RELEASE);
                take.open.insert(
                    key,
                    OpenNote {
                        start: tick,
                        velocity,
                        id: note_id,
                        expr: NoteExpression::default(),
                    },
                );
            }
            MidiMessage::NoteOn { key, .. } => take.close(key, tick, DEFAULT_RELEASE),
            MidiMessage::NoteOff { key, velocity, .. } => take.close(key, tick, velocity),
            MidiMessage::NoteExpr {
                key,
                note_id,
                kind,
                value,
                ..
            } => {
                if let Some(open) = take.open.get_mut(&key) {
                    if note_id == 0 || open.id == 0 || open.id == note_id {
                        let time = (tick - open.start).max(0.0);
                        push_point(curve_of(&mut open.expr, kind), time, f64::from(value));
                    }
                }
            }
            _ => {}
        }
    }

    /// The playhead jumped: held notes end where it left and continue where it lands.
    pub fn jumped(&mut self, from: f64, to: f64) {
        for take in self.takes.values_mut() {
            let held: Vec<(u8, f32, NoteId)> = take
                .open
                .iter()
                .map(|(k, n)| (*k, n.velocity, n.id))
                .collect();
            take.close_all(from);
            // Going back is the loop coming round: everything so far is one pass.
            if to < from && !take.notes.is_empty() {
                let start = take.pass_start.unwrap_or(self.start);
                take.finished.push(FinishedPass {
                    start,
                    notes: std::mem::take(&mut take.notes),
                    placed: false,
                });
            }
            if to < from {
                take.pass_start = Some(to.max(0.0) as u64);
            }
            for (key, velocity, id) in held {
                take.open.insert(
                    key,
                    OpenNote {
                        start: to,
                        velocity,
                        id,
                        expr: NoteExpression::default(),
                    },
                );
            }
        }
    }

    pub fn finish(&mut self, tick: f64) {
        for take in self.takes.values_mut() {
            take.close_all(tick);
        }
        self.ended = true;
    }

    /// Starts a take for `track` holding what `base` plays, so a new clip keeps it.
    pub fn overdub_onto(&mut self, track: TrackId, base: Vec<NoteData>) {
        self.takes.entry(track).or_default().base = base;
    }

    /// Whether anything has been recorded for this track.
    pub fn has_notes(&self, track: TrackId) -> bool {
        self.takes
            .get(&track)
            .is_some_and(|t| !t.notes.is_empty() || t.finished.iter().any(|p| !p.notes.is_empty()))
    }

    /// Remembers a clip that this recording made.
    pub fn note_placed(&mut self, track: TrackId, id: ArrClipId) {
        self.placed.push((track, id));
    }

    /// Every clip this recording has made: its takes and their copies on the main lane, or with
    /// Overdub on, the clip it wrote the notes into.
    pub fn placed_clips(&self, sig: TimeSignature) -> Vec<(TrackId, ArrClipId)> {
        let mut out = self.placed.clone();
        for p in self.placements(sig) {
            if let Some(id) = p.existing {
                out.push((p.track, id));
            }
        }
        out
    }

    /// The passes that have ended and not gone on the timeline yet, as clips of whole bars. Takes
    /// without notes are left out. Each is handed out once.
    pub fn take_finished_passes(&mut self, sig: TimeSignature) -> Vec<TakePass> {
        let bar = sig.ticks_per_bar().max(1);
        let (ended, start) = (self.ended, self.start);
        let mut out = Vec::new();
        for (track, take) in &mut self.takes {
            if ended && !take.notes.is_empty() {
                take.finished.push(FinishedPass {
                    start: take.pass_start.unwrap_or(start),
                    notes: std::mem::take(&mut take.notes),
                    placed: false,
                });
            }
            for pass in take.finished.iter_mut().filter(|p| !p.placed) {
                pass.placed = true;
                let from = pass.start / bar * bar;
                let last = pass
                    .notes
                    .iter()
                    .map(|n| n.start + n.length)
                    .max()
                    .unwrap_or(from);
                let length = last.saturating_sub(from).div_ceil(bar).max(1) * bar;
                let notes = pass
                    .notes
                    .iter()
                    .filter(|n| n.start >= from)
                    .map(|n| NoteData {
                        start: n.start - from,
                        ..n.clone()
                    })
                    .collect();
                out.push(TakePass {
                    track: *track,
                    start: from,
                    clip: Clip {
                        name: "Recording".into(),
                        color: [255, 255, 255],
                        length,
                        looped: false,
                        content: ClipContent::midi(notes),
                        ..Clip::default()
                    },
                });
            }
        }
        // Lowest track first for a stable order; a map doesn't keep one.
        out.sort_by_key(|p| p.track.0);
        out
    }

    /// Remembers which timeline clip a take went into.
    pub fn set_clip(&mut self, track: TrackId, id: ArrClipId) {
        if let Some(t) = self.takes.get_mut(&track) {
            t.clip = Some(id);
        }
    }

    /// Every take so far as a clip spanning whole bars. Takes without notes are left out.
    pub fn placements(&self, sig: TimeSignature) -> Vec<Placement> {
        let bar = sig.ticks_per_bar().max(1);
        let start = self.start / bar * bar;
        self.takes
            .iter()
            .filter(|(_, t)| !t.notes.is_empty() || t.finished.iter().any(|p| !p.notes.is_empty()))
            .map(|(track, take)| {
                let played = || {
                    take.finished
                        .iter()
                        .flat_map(|p| p.notes.iter())
                        .chain(take.notes.iter())
                };
                let last = played().map(|n| n.start + n.length).max().unwrap_or(start);
                let length = last.saturating_sub(start).div_ceil(bar).max(1) * bar;
                let end = start + length;
                let notes = played()
                    .chain(take.base.iter().filter(|n| (start..end).contains(&n.start)))
                    .map(|n| NoteData {
                        start: n.start - start,
                        ..n.clone()
                    })
                    .collect();
                Placement {
                    track: *track,
                    existing: take.clip,
                    start,
                    clip: Clip {
                        name: "Recording".into(),
                        color: [255, 255, 255],
                        length,
                        looped: false,
                        content: ClipContent::midi(notes),
                        ..Clip::default()
                    },
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(key: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 0.7,
            note_id: 0,
            tuning: 0.0,
        }
    }

    fn off(key: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity: 0.5,
            note_id: 0,
        }
    }

    fn sig() -> TimeSignature {
        TimeSignature::default()
    }

    fn notes(p: &Placement) -> &[NoteData] {
        p.clip.notes()
    }

    #[test]
    fn a_note_becomes_clip_relative_inside_whole_bars() {
        let t = TrackId::new();
        let mut r = Recording::new(4000);
        r.message(t, 4100.0, on(60));
        r.message(t, 5000.0, off(60));
        let p = &r.placements(sig())[0];
        assert_eq!(p.start, 3840);
        assert_eq!(p.clip.length, 3840);
        assert_eq!(notes(p)[0].start, 260);
        assert_eq!(notes(p)[0].length, 900);
    }

    #[test]
    fn a_key_up_sets_the_release_velocity() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, on(60));
        r.message(
            t,
            500.0,
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.2,
                note_id: 7,
            },
        );
        let p = r.placements(sig());
        assert!((notes(&p[0])[0].release - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_note_held_across_a_wrap_splits() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 3000.0, on(60));
        r.jumped(3840.0, 0.0);
        r.message(t, 500.0, off(60));
        let p = r.placements(sig());
        let mut n = notes(&p[0]).to_vec();
        n.sort_by_key(|x| x.start);
        assert_eq!((n[0].start, n[0].length), (0, 500));
        assert_eq!((n[1].start, n[1].length), (3000, 840));
    }

    #[test]
    fn notes_without_a_key_up_keep_the_default_release() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, on(61));
        r.finish(900.0);
        assert_eq!(notes(&r.placements(sig())[0])[0].release, DEFAULT_RELEASE);
    }

    #[test]
    fn finish_closes_held_notes() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, on(61));
        r.finish(900.0);
        assert_eq!(notes(&r.placements(sig())[0])[0].length, 800);
    }

    #[test]
    fn no_notes_means_no_clip() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 100.0, off(60));
        assert!(r.placements(sig()).is_empty());
    }
    fn expr(key: u8, id: rlive_core::NoteId, kind: ExprKind, value: f32) -> MidiMessage {
        MidiMessage::NoteExpr {
            channel: 0,
            key,
            note_id: id,
            kind,
            value,
        }
    }

    #[test]
    fn played_expression_lands_in_the_notes_dimensions() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 0.0, on(60));
        r.message(t, 100.0, expr(60, 0, ExprKind::Pitch, 2.0));
        r.message(t, 200.0, expr(60, 0, ExprKind::Slide, 0.25));
        r.message(t, 300.0, expr(60, 0, ExprKind::Pressure, 0.5));
        r.message(t, 400.0, off(60));
        let p = &r.placements(sig())[0];
        let e = notes(p)[0].expr.as_deref().unwrap();
        assert_eq!((e.pitch[0].time, e.pitch[0].value), (100.0, 2.0));
        assert_eq!((e.slide[0].time, e.slide[0].value), (200.0, 0.25));
        assert_eq!((e.pressure[0].time, e.pressure[0].value), (300.0, 0.5));
    }

    #[test]
    fn a_plain_note_gets_no_expression_at_all() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 0.0, on(60));
        r.message(t, 100.0, off(60));
        assert!(notes(&r.placements(sig())[0])[0].expr.is_none());
    }

    #[test]
    fn expression_for_a_note_id_stays_with_that_note() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(
            t,
            0.0,
            MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 0.7,
                note_id: 11,
                tuning: 0.0,
            },
        );
        r.message(t, 100.0, expr(60, 99, ExprKind::Pitch, 5.0));
        r.message(t, 200.0, expr(60, 11, ExprKind::Pitch, 3.0));
        r.message(t, 300.0, off(60));
        let e = notes(&r.placements(sig())[0])[0].expr.clone().unwrap();
        assert_eq!(e.pitch.len(), 1);
        assert_eq!(e.pitch[0].value, 3.0);
    }

    #[test]
    fn a_flood_of_values_thins_to_a_gesture() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        r.message(t, 0.0, on(60));
        for i in 0..960 {
            let v = (i as f32 / 960.0).sin();
            r.message(t, i as f64 + 1.0, expr(60, 0, ExprKind::Pressure, v));
        }
        r.message(t, 1000.0, off(60));
        let e = notes(&r.placements(sig())[0])[0].expr.clone().unwrap();
        assert!(
            e.pressure.len() > 2 && e.pressure.len() < 60,
            "{}",
            e.pressure.len()
        );
    }

    #[test]
    fn mpe_input_decodes_into_recorded_expression() {
        let t = TrackId::new();
        let mut r = Recording::new(0);
        let mut d = rlive_core::MpeDecoder::new();
        let mut feed = |tick: f64, m: MidiMessage, r: &mut Recording| {
            d.feed(m, &mut |out| r.message(t, tick, out));
        };
        let pre_bend = MidiMessage::PitchBend {
            channel: 3,
            value: 0.25,
        };
        feed(0.0, pre_bend, &mut r);
        feed(
            0.0,
            MidiMessage::NoteOn {
                channel: 3,
                key: 62,
                velocity: 0.8,
                note_id: 0,
                tuning: 0.0,
            },
            &mut r,
        );
        feed(
            480.0,
            MidiMessage::ChannelPressure {
                channel: 3,
                value: 0.5,
            },
            &mut r,
        );
        feed(
            960.0,
            MidiMessage::NoteOff {
                channel: 3,
                key: 62,
                velocity: 0.5,
                note_id: 0,
            },
            &mut r,
        );
        let e = notes(&r.placements(sig())[0])[0].expr.clone().unwrap();
        assert_eq!(e.pitch[0].value, 12.0, "a quarter of the 48-semitone range");
        assert_eq!(e.pitch[0].time, 0.0);
        assert_eq!((e.pressure[0].time, e.pressure[0].value), (480.0, 0.5));
    }
}
