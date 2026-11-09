//! MIDI clip rendering.

use super::data::{ClipData, ExprPoint, MidiClip, Note, NoteExpression};
use super::BlockTime;
use rlive_core::{ClipRegion, ExprKind, MidiEvent, MidiMessage, NoteId, NoteIds, NoteSpace};
use std::sync::atomic::{AtomicU64, Ordering};

/// The softest a note with Velocity Deviation can come out: a velocity of 0 would be a note-off.
const MIN_VELOCITY: f32 = 1.0 / 127.0;

/// How many clip notes can sound at once. A further note simply doesn't start, so that every
/// note which does start can always be released.
const MAX_SOUNDING: usize = 512;

/// Spacing of expression values within a note, in ticks: about 10 ms at 120 BPM.
const EXPR_STEP: f64 = 20.0;

#[derive(Clone, Copy)]
struct Sounding {
    /// Index of the note in its clip.
    note: u32,
    key: u8,
    id: NoteId,
    release: f32,
}

const SILENT: Sounding = Sounding {
    note: 0,
    key: 0,
    id: 0,
    release: 0.0,
};

/// The clip notes currently sounding, so note-offs can be sent on stop and expression can
/// reach the right note. Oldest first.
pub(crate) struct Voices {
    sounding: [Sounding; MAX_SOUNDING],
    len: usize,
    ids: NoteIds,
    /// What this run of playback draws its Chance and Velocity Deviation from. Moves on each time
    /// everything is released, so the next run comes out differently.
    luck: u64,
}

/// Each player starts somewhere different, so two tracks playing the same clip don't agree.
static NEXT_LUCK: AtomicU64 = AtomicU64::new(0x51ed_270b_3f9a_16c7);

impl Voices {
    pub fn new(space: NoteSpace) -> Self {
        Self {
            sounding: [SILENT; MAX_SOUNDING],
            len: 0,
            ids: NoteIds::new(space),
            luck: NEXT_LUCK.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed),
        }
    }

    /// Sets what Chance and Velocity Deviation draw from, so a test can repeat them.
    #[cfg(test)]
    pub fn seed(&mut self, seed: u64) {
        self.luck = seed;
    }

    /// A number in `0..1`, always the same for one pass of one thing in one run of playback.
    /// Nothing is stored per note or group, so it can't allocate and a chord split across blocks,
    /// a chase and a loop all see the same answer.
    fn draw(&self, pass: i64, what: u64, salt: u64) -> f32 {
        let mix = |mut x: u64| {
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^ (x >> 31)
        };
        let h = mix(self
            .luck
            .wrapping_add((pass as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)));
        let h = mix(h
            .wrapping_add(what.wrapping_mul(0xd6e8_feb8_6659_fd93))
            .wrapping_add(salt));
        (h >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Whether note `index` plays on this pass, and how hard.
    pub(super) fn fate(&self, note: &Note, index: u32, pass: i64) -> Option<f32> {
        if note.probability >= 1.0 && note.vel_range == 0.0 && note.group.is_none() {
            return Some(note.velocity);
        }
        let plays = match note.group {
            None => note.probability >= 1.0 || self.draw(pass, index as u64, 0) < note.probability,
            Some(g) => {
                let id = g.id as u64;
                let chance = note.probability >= 1.0 || self.draw(pass, id, 1) < note.probability;
                let picked = !g.one
                    || ((self.draw(pass, id, 2) * g.size as f32) as u16).min(g.size - 1)
                        == g.member;
                chance && picked
            }
        };
        plays.then(|| {
            let spread = self.draw(pass, index as u64, 3) * note.vel_range;
            (note.velocity + spread).clamp(MIN_VELOCITY, 1.0)
        })
    }

    /// Start note `note` of the clip. Returns its new id, or `None` if too many are sounding.
    fn on(&mut self, note: u32, key: u8, release: f32) -> Option<NoteId> {
        if self.len == MAX_SOUNDING {
            return None;
        }
        let id = self.ids.alloc();
        self.sounding[self.len] = Sounding {
            note,
            key,
            id,
            release,
        };
        self.len += 1;
        Some(id)
    }

    /// The id of note `note`, if it is sounding.
    fn id(&self, note: u32) -> Option<NoteId> {
        self.sounding[..self.len]
            .iter()
            .find(|v| v.note == note)
            .map(|v| v.id)
    }

    /// Stop note `note`, returning what it was so a note-off can be sent.
    fn off(&mut self, note: u32) -> Option<Sounding> {
        let i = self.sounding[..self.len]
            .iter()
            .position(|v| v.note == note)?;
        let voice = self.sounding[i];
        self.sounding.copy_within(i + 1..self.len, i);
        self.len -= 1;
        Some(voice)
    }

    /// Emit note-offs for every sounding note at `offset`, in key order.
    pub fn release_all(&mut self, offset: u32, events: &mut Vec<MidiEvent>) {
        let sounding = &mut self.sounding[..self.len];
        sounding.sort_unstable_by_key(|v| (v.key, v.id));
        for v in sounding.iter() {
            push_event(events, MidiEvent::new(offset, note_off(v)));
        }
        self.len = 0;
        self.luck = self
            .luck
            .wrapping_mul(0x2545_f491_4f6c_dd1d)
            .wrapping_add(1);
    }
}

fn note_off(v: &Sounding) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key: v.key,
        velocity: v.release,
        note_id: v.id,
    }
}

fn note_on(velocity: f32, key: u8, note_id: NoteId) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity,
        note_id,
        tuning: 0.0,
    }
}

/// The content window pass `k` of a clip plays, and the song tick its content position 0 falls on
/// in that pass. Pass 0 is the run from the start marker to the end of the region; each later
/// pass is one time round the loop.
fn pass_window(region: &ClipRegion, origin: f64, k: i64) -> (f64, f64, f64) {
    let zero = origin + region.lead();
    let base = zero - region.loop_start + k as f64 * region.loop_len;
    let from = if k == 0 {
        region.start
    } else {
        region.loop_start
    };
    (base, from, region.content_end())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_midi(
    clip: &MidiClip,
    region: ClipRegion,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    voices: &mut Voices,
    ev: &mut Vec<MidiEvent>,
) {
    let len = region.loop_len;
    let zero = origin + region.lead();
    let mut k_first = (((a - zero) / len).floor() as i64 - 1).max(0);
    let mut k_last = ((seg_end - zero) / len).floor() as i64;
    if !region.looped {
        k_first = 0;
        k_last = k_last.min(0);
    }
    // Before the first pass of the loop a clip is still in its first window, running in.
    if region.lead() > 0.0 {
        k_last = k_last.max(0);
    }
    let lookback = clip.max_len as f64;
    for k in k_first..=k_last {
        let (base, window_from, window_to) = pass_window(&region, origin, k);
        // One tick of slack keeps float error in `base` from dropping a boundary note; the
        // exact comparisons below decide what actually fires.
        let scan_from = a - base - lookback - 1.0;
        let scan_to = seg_end - base + 1.0;
        let first = clip.notes.partition_point(|n| (n.start as f64) < scan_from);
        for (i, note) in clip.notes.iter().enumerate().skip(first) {
            let start = note.start as f64;
            if start > scan_to || start >= window_to {
                break;
            }
            if start < window_from {
                continue;
            }
            let index = i as u32;
            let key = note.key & 127;
            let on = base + start;
            let off = base + ((note.start + note.length.max(1)) as f64).min(window_to);
            // Start before stop, or a note shorter than the block never gets its note-off.
            // Notes meeting at a boundary still come off first: the earlier one is earlier here
            // (and in event order, once the block is sorted).
            if on >= a && on < seg_end {
                let velocity = voices.fate(note, index, k);
                if let Some((velocity, id)) =
                    velocity.and_then(|v| Some((v, voices.on(index, key, note.release)?)))
                {
                    let frame = time.event_offset(on);
                    push_event(ev, MidiEvent::new(frame, note_on(velocity, key, id)));
                    if let Some(expr) = &note.expr {
                        push_expression(expr, key, id, 0.0, frame, ev);
                    }
                }
            }
            if let (Some(expr), Some(id)) = (&note.expr, voices.id(index)) {
                push_ramps(expr, key, id, NoteSpan { on, off }, (a, seg_end), time, ev);
            }
            if off >= a && off < seg_end {
                if let Some(voice) = voices.off(index) {
                    push_event(ev, MidiEvent::new(time.event_offset(off), note_off(&voice)));
                }
            }
        }
    }
}

/// Start the MIDI notes that are already sounding at song tick `at`, at frame `offset`.
/// Notes starting exactly at `at` are left to [`render_range`].
pub(crate) fn chase(
    clip: &ClipData,
    region: ClipRegion,
    origin: f64,
    at: f64,
    offset: u32,
    voices: &mut Voices,
    ev: &mut Vec<MidiEvent>,
) {
    let ClipData::Midi(midi) = clip.content() else {
        return;
    };
    let len = region.loop_len;
    let local = at - origin;
    if len <= 0.0 || local < 0.0 || (!region.looped && local >= region.play_length()) {
        return;
    }
    let k = if region.looped {
        ((at - (origin + region.lead())) / len).floor().max(0.0) as i64
    } else {
        0
    };
    let (base, window_from, window_to) = pass_window(&region, origin, k);
    let scan_from = at - base - midi.max_len as f64 - 1.0;
    let first = midi.notes.partition_point(|n| (n.start as f64) < scan_from);
    for (i, note) in midi.notes.iter().enumerate().skip(first) {
        let start = note.start as f64;
        if base + start >= at || start >= window_to {
            break;
        }
        if start < window_from {
            continue;
        }
        let off = base + ((note.start + note.length.max(1)) as f64).min(window_to);
        if off > at {
            let key = note.key & 127;
            let velocity = voices.fate(note, i as u32, k);
            if let Some((velocity, id)) =
                velocity.and_then(|v| Some((v, voices.on(i as u32, key, note.release)?)))
            {
                push_event(ev, MidiEvent::new(offset, note_on(velocity, key, id)));
                if let Some(expr) = &note.expr {
                    push_expression(expr, key, id, at - (base + start), offset, ev);
                }
            }
        }
    }
}

/// Where a note sounds, in song ticks.
struct NoteSpan {
    on: f64,
    off: f64,
}

fn dimensions(expr: &NoteExpression) -> [(ExprKind, &[ExprPoint]); 3] {
    [
        (ExprKind::Pitch, expr.pitch.as_slice()),
        (ExprKind::Slide, expr.slide.as_slice()),
        (ExprKind::Pressure, expr.pressure.as_slice()),
    ]
}

/// Emit every expression value of a note as it stands `position` ticks in, at `frame`.
fn push_expression(
    expr: &NoteExpression,
    key: u8,
    note_id: NoteId,
    position: f64,
    frame: u32,
    ev: &mut Vec<MidiEvent>,
) {
    for (kind, points) in dimensions(expr) {
        if !points.is_empty() {
            push_expr_event(ev, frame, key, note_id, kind, value_at(points, position));
        }
    }
}

/// Emit the changes in a note's expression over `[window.0, window.1)`, on a fixed tick grid
/// from the note's start, so a note sounds the same whatever the block size. The start of the
/// note itself is covered by [`push_expression`].
fn push_ramps(
    expr: &NoteExpression,
    key: u8,
    note_id: NoteId,
    span: NoteSpan,
    (a, b): (f64, f64),
    time: BlockTime,
    ev: &mut Vec<MidiEvent>,
) {
    let end = span.off.min(b);
    for (kind, points) in dimensions(expr) {
        if points.is_empty() {
            continue;
        }
        let mut step = ((a - span.on) / EXPR_STEP).ceil().max(1.0);
        let mut previous = value_at(points, (step - 1.0) * EXPR_STEP);
        loop {
            let position = step * EXPR_STEP;
            let tick = span.on + position;
            if tick >= end {
                break;
            }
            let value = value_at(points, position);
            if value != previous {
                push_expr_event(ev, time.event_offset(tick), key, note_id, kind, value);
            }
            previous = value;
            step += 1.0;
        }
    }
}

/// An envelope's value `position` ticks into the note, straight between points and held
/// beyond them.
fn value_at(points: &[ExprPoint], position: f64) -> f32 {
    let i = points.partition_point(|p| p.time <= position);
    match (i.checked_sub(1).map(|j| points[j]), points.get(i).copied()) {
        (Some(p), Some(next)) => {
            let span = next.time - p.time;
            if span <= 0.0 {
                return next.value;
            }
            let t = ((position - p.time) / span) as f32;
            p.value + (next.value - p.value) * t
        }
        (Some(p), None) | (None, Some(p)) => p.value,
        (None, None) => 0.0,
    }
}

/// Expression leaves room in the block's event list, so a burst of it can never crowd out
/// note-ons and note-offs: a lost value is corrected by the next, a lost note-off sticks.
fn push_expr_event(
    events: &mut Vec<MidiEvent>,
    offset: u32,
    key: u8,
    note_id: NoteId,
    kind: ExprKind,
    value: f32,
) {
    let limit = events.capacity() - events.capacity() / 4;
    if events.len() < limit {
        events.push(MidiEvent::new(
            offset,
            MidiMessage::NoteExpr {
                channel: 0,
                key,
                note_id,
                kind,
                value,
            },
        ));
    }
}

/// Push without ever growing the vector (the audio thread must not allocate).
#[inline]
pub(crate) fn push_event(events: &mut Vec<MidiEvent>, e: MidiEvent) {
    if events.len() < events.capacity() {
        events.push(e);
    }
}
