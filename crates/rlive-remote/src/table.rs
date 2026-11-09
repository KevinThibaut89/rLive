//! The compiled mappings, in the form the MIDI thread reads them.
//!
//! The session compiles every mapping into a [`RemoteTable`] whose cells and ranges are resolved
//! ahead of time, then publishes it as a whole. The MIDI thread looks each message up with no
//! lock and no allocation; what it finds it reports to a [`RemoteSink`]. State that outlives one
//! message (takeover, a 14-bit controller's high byte) is in atomics inside the entry, and is
//! carried from table to table so publishing doesn't disturb a control mid-move.

use crate::decode::{decode_relative, MidiEvent, Reading};
use crate::takeover::{Takeover, TakeoverMode};
use rlive_core::param::{ParamRange, ParamRef};
use rlive_core::TrackId;
use rlive_project::{CcEncoding, MapTarget, MidiSource};
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicU8, Ordering::Relaxed};

/// One step of a relative control, as a fraction of its range.
pub const RELATIVE_STEP: f64 = 1.0 / 127.0;
/// Listens on every port.
pub const ALL_PORTS: u64 = u64::MAX;

const NO_RAW: u16 = u16::MAX;
const ENGAGED: u8 = 1;
const WAS_ON: u8 = 1 << 1;

const NONE: u16 = u16::MAX;
const NOTE_KEYS: usize = 0;
const CC_KEYS: usize = 16 * 128;
const BEND_KEYS: usize = 16 * 128 * 2;
const KEY_COUNT: usize = 16 * 128 * 2 + 16;

fn note_key(channel: u8, key: u8) -> usize {
    NOTE_KEYS + usize::from(channel & 15) * 128 + usize::from(key & 127)
}

fn cc_key(channel: u8, number: u8) -> usize {
    CC_KEYS + usize::from(channel & 15) * 128 + usize::from(number & 127)
}

fn bend_key(channel: u8) -> usize {
    BEND_KEYS + usize::from(channel & 15)
}

/// Where a parameter's value goes in the engine. `track` is `None` for the Main track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellAddr {
    pub track: Option<TrackId>,
    pub cell: u32,
}

/// How a parameter takes its values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShapeKind {
    Continuous,
    /// Lives at 0 or 1. With `invert`, "on" means 0, as a Track Activator does over its Mute cell.
    Toggle {
        invert: bool,
    },
    Stepped,
}

/// A parameter's range together with the part of it a mapping sweeps. For a switch, `min` and
/// `max` are controller values 0 to 127 (manual p. 871).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamShape {
    pub range: ParamRange,
    pub min: f64,
    pub max: f64,
    pub kind: ShapeKind,
}

/// A button, box or switch that isn't a parameter. Worked on the UI thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKind {
    Switch,
    Button,
    Scroll,
}

/// What a message means to a target with no parameter behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// Pressed, with its velocity from 0 to 1.
    Press(f32),
    Release,
    Toggle,
    On,
    Off,
    Step(i32),
    Absolute(f32),
}

/// What a compiled mapping does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Exec {
    /// Moves a parameter. With an address it goes straight to the engine; without one (Solo and
    /// tempo, which do more than set a cell) the UI thread sets it.
    Param {
        target: ParamRef,
        addr: Option<CellAddr>,
        shape: ParamShape,
    },
    Slot {
        track: TrackId,
        slot: u32,
        direct: bool,
    },
    Scene {
        scene: u32,
        direct: bool,
    },
    Ui(UiKind),
    /// Belongs to a control surface script.
    Surface {
        slot: u8,
    },
    /// Taken from the track inputs but doing nothing: its target refuses gestures just now.
    Blocked,
}

/// One mapping, ready to be looked up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Compiled {
    pub channel: u8,
    pub source: MidiSource,
    pub target: MapTarget,
    pub exec: Exec,
    /// Which ports it listens on; bit `n` is port `n`.
    pub ports: u64,
    /// Which ports get its feedback.
    pub feedback: u64,
    /// The normalized value of its parameter when compiled.
    pub value: f64,
}

/// Who hears what a message did.
pub trait RemoteSink {
    fn param(&mut self, target: ParamRef, addr: Option<CellAddr>, value: f64);
    fn slot_down(&mut self, track: TrackId, slot: u32, velocity: f32, transpose: i8, direct: bool);
    fn slot_up(&mut self, track: TrackId, slot: u32, direct: bool);
    fn scene(&mut self, scene: u32, direct: bool);
    fn gesture(&mut self, target: MapTarget, gesture: Gesture);
    fn surface(&mut self, slot: u8, bytes: [u8; 3]);
}

struct EntryState {
    /// The parameter, normalized, as last known.
    value: AtomicU64,
    /// [`Takeover::last_control`], NaN for none.
    control: AtomicU64,
    /// [`Takeover::last_sent`], NaN for none.
    sent: AtomicU64,
    flags: AtomicU8,
    /// A 14-bit controller's high byte, waiting for its low byte.
    msb: AtomicU8,
    /// The last absolute value received.
    last_raw: AtomicU16,
}

impl EntryState {
    fn new(value: f64) -> Self {
        Self {
            value: AtomicU64::new(value.to_bits()),
            control: AtomicU64::new(f64::NAN.to_bits()),
            sent: AtomicU64::new(f64::NAN.to_bits()),
            flags: AtomicU8::new(0),
            msb: AtomicU8::new(0),
            last_raw: AtomicU16::new(NO_RAW),
        }
    }

    fn copy_from(&self, other: &Self) {
        self.value.store(other.value.load(Relaxed), Relaxed);
        self.control.store(other.control.load(Relaxed), Relaxed);
        self.sent.store(other.sent.load(Relaxed), Relaxed);
        self.flags.store(other.flags.load(Relaxed), Relaxed);
        self.msb.store(other.msb.load(Relaxed), Relaxed);
        self.last_raw.store(other.last_raw.load(Relaxed), Relaxed);
    }

    fn value(&self) -> f64 {
        f64::from_bits(self.value.load(Relaxed))
    }

    fn set_value(&self, v: f64) {
        self.value.store(v.to_bits(), Relaxed);
    }

    fn takeover(&self) -> Takeover {
        let opt = |x: &AtomicU64| {
            let v = f64::from_bits(x.load(Relaxed));
            (!v.is_nan()).then_some(v)
        };
        Takeover {
            last_control: opt(&self.control),
            engaged: self.flags.load(Relaxed) & ENGAGED != 0,
            last_sent: opt(&self.sent),
        }
    }

    fn store_takeover(&self, t: Takeover) {
        let bits = |v: Option<f64>| v.unwrap_or(f64::NAN).to_bits();
        self.control.store(bits(t.last_control), Relaxed);
        self.sent.store(bits(t.last_sent), Relaxed);
        let flags = self.flags.load(Relaxed);
        let flags = if t.engaged {
            flags | ENGAGED
        } else {
            flags & !ENGAGED
        };
        self.flags.store(flags, Relaxed);
    }

    /// An absolute value crossed the middle: press or release. A control that repeats the very
    /// same high value counts as pressed again, since a knob only sends when it moves.
    fn edge(&self, raw: u16, top: u16) -> Option<bool> {
        let high = Reading::absolute_is_high(raw, top);
        let repeat = self.last_raw.load(Relaxed) == raw;
        let flags = self.flags.load(Relaxed);
        let was = flags & WAS_ON != 0;
        self.flags.store(
            if high {
                flags | WAS_ON
            } else {
                flags & !WAS_ON
            },
            Relaxed,
        );
        if high && (!was || repeat) {
            Some(true)
        } else if !high && was {
            Some(false)
        } else {
            None
        }
    }
}

/// The compiled mappings, found by message in O(1).
pub struct RemoteTable {
    entries: Vec<Compiled>,
    states: Vec<EntryState>,
    /// For each message key, the first of its links, or [`NONE`].
    heads: Vec<u16>,
    /// Entries, and the next link for the same key.
    links: Vec<(u16, u16)>,
}

impl RemoteTable {
    pub fn empty() -> Self {
        Self::new(Vec::new(), None)
    }

    /// Builds a table. Entries also in `previous` go on with its takeover state.
    pub fn new(entries: Vec<Compiled>, previous: Option<&RemoteTable>) -> Self {
        let entries: Vec<Compiled> = entries.into_iter().take(NONE as usize - 1).collect();
        let states: Vec<EntryState> = entries.iter().map(|e| EntryState::new(e.value)).collect();
        let mut table = Self {
            entries,
            states,
            heads: vec![NONE; KEY_COUNT],
            links: Vec::new(),
        };
        for ix in 0..table.entries.len() {
            let keys = keys_of(&table.entries[ix]);
            for key in keys {
                table.link(key, ix as u16);
            }
        }
        if let Some(old) = previous {
            for (e, s) in table.entries.iter().zip(&table.states) {
                let same = old.entries.iter().position(|o| {
                    o.channel == e.channel && o.source == e.source && o.target == e.target
                });
                if let Some(i) = same {
                    s.copy_from(&old.states[i]);
                }
            }
        }
        table
    }

    fn link(&mut self, key: usize, entry: u16) {
        let next = self.heads[key];
        // Links chain from the newest, so a message reaches its mappings in reverse; a chain is
        // short and the order only matters to nobody.
        self.heads[key] = self.links.len() as u16;
        self.links.push((entry, next));
    }

    pub fn entries(&self) -> &[Compiled] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Tells an entry its parameter's value has changed some other way.
    pub fn set_value(&self, ix: usize, norm: f64) {
        if let Some(s) = self.states.get(ix) {
            s.set_value(norm);
        }
    }

    pub fn value(&self, ix: usize) -> f64 {
        self.states.get(ix).map_or(0.0, EntryState::value)
    }

    /// The last absolute value this entry received.
    pub fn last_raw(&self, ix: usize) -> Option<u16> {
        let raw = self.states.get(ix)?.last_raw.load(Relaxed);
        (raw != NO_RAW).then_some(raw)
    }

    /// Reads one message that arrived on `port`, and returns whether a mapping took it.
    pub fn process(
        &self,
        port: u32,
        bytes: &[u8],
        mode: TakeoverMode,
        sink: &mut impl RemoteSink,
    ) -> bool {
        let Some(event) = MidiEvent::parse(bytes) else {
            return false;
        };
        let key = match event {
            MidiEvent::Note { channel, key, .. } => note_key(channel, key),
            MidiEvent::Cc {
                channel, number, ..
            } => cc_key(channel, number),
            MidiEvent::PitchBend { channel, .. } => bend_key(channel),
        };
        let bit = 1u64 << (port & 63);
        let mut link = self.heads[key];
        let mut consumed = false;
        while link != NONE {
            let (ix, next) = self.links[link as usize];
            link = next;
            let entry = &self.entries[ix as usize];
            if entry.ports & bit == 0 {
                continue;
            }
            consumed = true;
            let state = &self.states[ix as usize];
            if let Exec::Surface { slot } = entry.exec {
                sink.surface(slot, three(bytes));
                continue;
            }
            if let Some(reading) = read(entry, state, event) {
                perform(entry, state, reading, mode, sink);
            }
        }
        consumed
    }
}

fn three(bytes: &[u8]) -> [u8; 3] {
    [
        bytes.first().copied().unwrap_or(0),
        bytes.get(1).copied().unwrap_or(0),
        bytes.get(2).copied().unwrap_or(0),
    ]
}

fn keys_of(e: &Compiled) -> Vec<usize> {
    match e.source {
        MidiSource::Note(n) => vec![note_key(e.channel, n)],
        MidiSource::NoteRange { low, high } | MidiSource::ChromaticRange { low, high, .. } => {
            (low.min(high)..=low.max(high))
                .map(|k| note_key(e.channel, k))
                .collect()
        }
        MidiSource::Cc {
            number,
            encoding: CcEncoding::Absolute14Bit,
        } => vec![
            cc_key(e.channel, number),
            cc_key(e.channel, (number + 32) & 127),
        ],
        MidiSource::Cc { number, .. } => vec![cc_key(e.channel, number)],
        MidiSource::PitchBend => vec![bend_key(e.channel)],
    }
}

/// What a message says to one entry. A 14-bit controller says something only once its low byte
/// has come in.
fn read(e: &Compiled, state: &EntryState, event: MidiEvent) -> Option<Reading> {
    match (event, e.source) {
        (MidiEvent::Note { key, velocity, .. }, _) => Some(Reading::Note { key, velocity }),
        (
            MidiEvent::Cc { number, value, .. },
            MidiSource::Cc {
                number: msb,
                encoding: CcEncoding::Absolute14Bit,
            },
        ) => {
            if number == msb {
                state.msb.store(value, Relaxed);
                None
            } else {
                let high = u16::from(state.msb.load(Relaxed));
                Some(Reading::Absolute {
                    raw: high << 7 | u16::from(value),
                    top: 16383,
                })
            }
        }
        (MidiEvent::Cc { value, .. }, MidiSource::Cc { encoding, .. }) => {
            if encoding.is_relative() {
                Some(Reading::Relative {
                    delta: decode_relative(encoding, value),
                })
            } else {
                Some(Reading::Absolute {
                    raw: u16::from(value),
                    top: 127,
                })
            }
        }
        (MidiEvent::PitchBend { value, .. }, _) => Some(Reading::Absolute {
            raw: value,
            top: 16383,
        }),
        _ => None,
    }
}

fn perform(
    e: &Compiled,
    state: &EntryState,
    reading: Reading,
    mode: TakeoverMode,
    sink: &mut impl RemoteSink,
) {
    match e.exec {
        Exec::Param {
            target,
            addr,
            shape,
        } => {
            if let Some(next) = param_value(&shape, e, state, reading, mode) {
                if (next - state.value()).abs() > 1e-9 {
                    state.set_value(next);
                    sink.param(target, addr, shape.range.from_normalized(next));
                }
            }
        }
        Exec::Slot {
            track,
            slot,
            direct,
        } => match press(state, reading) {
            Some(Pressed::Down(velocity)) => {
                let transpose = chromatic_shift(e.source, reading);
                sink.slot_down(track, slot, velocity, transpose, direct);
            }
            Some(Pressed::Up) => sink.slot_up(track, slot, direct),
            None => {}
        },
        Exec::Scene { scene, direct } => {
            if let Some(Pressed::Down(_)) = press(state, reading) {
                sink.scene(scene, direct);
            }
        }
        Exec::Ui(kind) => {
            if let Some(g) = gesture_of(kind, state, reading) {
                sink.gesture(e.target, g);
            }
        }
        Exec::Surface { .. } | Exec::Blocked => {}
    }
    if let Reading::Absolute { raw, .. } = reading {
        state.last_raw.store(raw, Relaxed);
    }
}

enum Pressed {
    Down(f32),
    Up,
}

/// What a message is to a slot (manual pp. 871-872).
fn press(state: &EntryState, reading: Reading) -> Option<Pressed> {
    match reading {
        Reading::Note { velocity, .. } if velocity > 0 => {
            Some(Pressed::Down(f32::from(velocity) / 127.0))
        }
        Reading::Note { .. } => Some(Pressed::Up),
        Reading::Absolute { raw, top } => state.edge(raw, top).map(|down| {
            if down {
                Pressed::Down(1.0)
            } else {
                Pressed::Up
            }
        }),
        Reading::Relative { delta } if delta > 0 => Some(Pressed::Down(1.0)),
        Reading::Relative { delta } if delta < 0 => Some(Pressed::Up),
        Reading::Relative { .. } => None,
    }
}

fn gesture_of(kind: UiKind, state: &EntryState, reading: Reading) -> Option<Gesture> {
    match kind {
        UiKind::Switch => match reading {
            Reading::Note { velocity, .. } if velocity > 0 => Some(Gesture::Toggle),
            Reading::Note { .. } => None,
            Reading::Absolute { raw, top } => Some(if Reading::absolute_is_high(raw, top) {
                Gesture::On
            } else {
                Gesture::Off
            }),
            Reading::Relative { delta } if delta > 0 => Some(Gesture::On),
            Reading::Relative { delta } if delta < 0 => Some(Gesture::Off),
            Reading::Relative { .. } => None,
        },
        UiKind::Button => match press(state, reading)? {
            Pressed::Down(v) => Some(Gesture::Press(v)),
            Pressed::Up => Some(Gesture::Release),
        },
        UiKind::Scroll => match reading {
            Reading::Note { velocity, .. } if velocity > 0 => Some(Gesture::Step(1)),
            Reading::Note { .. } => None,
            Reading::Absolute { raw, top } => {
                Some(Gesture::Absolute(Reading::normalized(raw, top) as f32))
            }
            Reading::Relative { delta } => (delta != 0).then_some(Gesture::Step(delta)),
        },
    }
}

fn chromatic_shift(source: MidiSource, reading: Reading) -> i8 {
    match (source, reading) {
        (MidiSource::ChromaticRange { root, .. }, Reading::Note { key, .. }) => {
            (i16::from(key) - i16::from(root)) as i8
        }
        _ => 0,
    }
}

/// The parameter's new normalized value, if the message gives it one.
fn param_value(
    s: &ParamShape,
    e: &Compiled,
    state: &EntryState,
    reading: Reading,
    mode: TakeoverMode,
) -> Option<f64> {
    match s.kind {
        ShapeKind::Continuous => continuous(s, e, state, reading, mode),
        ShapeKind::Toggle { invert } => switch(s, state, reading, invert),
        ShapeKind::Stepped => stepped(s, state, reading),
    }
}

fn continuous(
    s: &ParamShape,
    e: &Compiled,
    state: &EntryState,
    reading: Reading,
    mode: TakeoverMode,
) -> Option<f64> {
    let (low, high) = (s.range.to_normalized(s.min), s.range.to_normalized(s.max));
    let span = high - low;
    let now = if span == 0.0 {
        0.0
    } else {
        ((state.value() - low) / span).clamp(0.0, 1.0)
    };
    let control = match reading {
        Reading::Note { velocity: 0, .. } => return None,
        Reading::Note { key, .. } => match e.source {
            MidiSource::NoteRange { low, high } if high > low => {
                f64::from(key - low) / f64::from(high - low)
            }
            // A single note swaps the parameter between its two ends.
            _ if now >= 0.5 => 0.0,
            _ => 1.0,
        },
        Reading::Absolute { raw, top } => {
            let control = Reading::normalized(raw, top);
            let mut t = state.takeover();
            let out = t.apply(mode, control, now);
            state.store_takeover(t);
            out?
        }
        Reading::Relative { delta } => (now + f64::from(delta) * RELATIVE_STEP).clamp(0.0, 1.0),
    };
    Some(low + control * span)
}

fn switch(s: &ParamShape, state: &EntryState, reading: Reading, invert: bool) -> Option<f64> {
    let on = match reading {
        Reading::Note { velocity: 0, .. } => return None,
        // A note flips what's there, whichever way round "on" is.
        Reading::Note { .. } => return Some(if state.value() >= 0.5 { 0.0 } else { 1.0 }),
        Reading::Absolute { raw, top } => {
            let v = f64::from(raw) * 127.0 / f64::from(top.max(1));
            let (low, high) = (s.min.min(s.max), s.min.max(s.max));
            let inside = (low..=high).contains(&v);
            if s.min > s.max {
                !inside
            } else {
                inside
            }
        }
        Reading::Relative { delta } if delta > 0 => true,
        Reading::Relative { delta } if delta < 0 => false,
        Reading::Relative { .. } => return None,
    };
    Some(if on != invert { 1.0 } else { 0.0 })
}

fn stepped(s: &ParamShape, state: &EntryState, reading: Reading) -> Option<f64> {
    let (low, high) = (s.min.min(s.max), s.min.max(s.max));
    let plain = s.range.from_normalized(state.value());
    let next = match reading {
        Reading::Note { velocity: 0, .. } => return None,
        Reading::Note { .. } => {
            if plain + 1.0 > high {
                low
            } else {
                plain + 1.0
            }
        }
        Reading::Absolute { raw, top } => {
            let c = Reading::normalized(raw, top);
            (s.min + c * (s.max - s.min)).round()
        }
        Reading::Relative { delta } if delta != 0 => {
            (plain + f64::from(delta.signum())).clamp(low, high)
        }
        Reading::Relative { .. } => return None,
    };
    Some(s.range.to_normalized(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::param::{ParamId, ParamOwner};
    use rlive_project::RemoteControl;

    const P: ParamRef = ParamRef {
        owner: ParamOwner::Master,
        id: ParamId(0),
    };
    const R: ParamRange = ParamRange {
        min: 0.0,
        max: 100.0,
        skew: 1.0,
        quantized: false,
    };

    #[derive(Debug, Default)]
    struct Log {
        params: Vec<f64>,
        slots: Vec<String>,
        gestures: Vec<(MapTarget, Gesture)>,
    }

    impl RemoteSink for Log {
        fn param(&mut self, _: ParamRef, _: Option<CellAddr>, value: f64) {
            self.params.push(value);
        }
        fn slot_down(&mut self, _: TrackId, slot: u32, v: f32, t: i8, _: bool) {
            self.slots.push(format!("down {slot} v{v:.2} t{t}"));
        }
        fn slot_up(&mut self, _: TrackId, slot: u32, _: bool) {
            self.slots.push(format!("up {slot}"));
        }
        fn scene(&mut self, s: u32, _: bool) {
            self.slots.push(format!("scene {s}"));
        }
        fn gesture(&mut self, t: MapTarget, g: Gesture) {
            self.gestures.push((t, g));
        }
        fn surface(&mut self, slot: u8, _: [u8; 3]) {
            self.slots.push(format!("surface {slot}"));
        }
    }

    fn cont(source: MidiSource, min: f64, max: f64, value: f64) -> Compiled {
        Compiled {
            channel: 0,
            source,
            target: MapTarget::Param(P),
            exec: Exec::Param {
                target: P,
                addr: Some(CellAddr {
                    track: None,
                    cell: 0,
                }),
                shape: ParamShape {
                    range: R,
                    min,
                    max,
                    kind: ShapeKind::Continuous,
                },
            },
            ports: ALL_PORTS,
            feedback: ALL_PORTS,
            value,
        }
    }

    fn toggle(source: MidiSource, min: f64, max: f64, value: f64, invert: bool) -> Compiled {
        let mut e = cont(source, min, max, value);
        if let Exec::Param { shape, .. } = &mut e.exec {
            shape.range = ParamRange {
                min: 0.0,
                max: 1.0,
                skew: 1.0,
                quantized: true,
            };
            shape.kind = ShapeKind::Toggle { invert };
        }
        e
    }

    fn stepped_entry(source: MidiSource, n: f64, value: f64) -> Compiled {
        let mut e = cont(source, 0.0, n, value);
        if let Exec::Param { shape, .. } = &mut e.exec {
            shape.range = ParamRange {
                min: 0.0,
                max: n,
                skew: 1.0,
                quantized: true,
            };
            shape.kind = ShapeKind::Stepped;
        }
        e
    }

    fn cc(number: u8, encoding: CcEncoding) -> MidiSource {
        MidiSource::Cc { number, encoding }
    }

    fn run(table: &RemoteTable, msgs: &[&[u8]], mode: TakeoverMode) -> Log {
        let mut log = Log::default();
        for m in msgs {
            table.process(0, m, mode, &mut log);
        }
        log
    }

    #[test]
    fn absolute_seven_bit_sweeps_the_mapping_range() {
        let t = RemoteTable::new(
            vec![cont(cc(7, CcEncoding::Absolute), 20.0, 60.0, 0.0)],
            None,
        );
        let log = run(
            &t,
            &[&[0xb0, 7, 0], &[0xb0, 7, 127], &[0xb0, 7, 64]],
            TakeoverMode::None,
        );
        assert_eq!(log.params[0], 20.0);
        assert_eq!(log.params[1], 60.0);
        assert!((log.params[2] - 40.157).abs() < 0.01);
    }

    #[test]
    fn a_reversed_range_runs_backwards() {
        let t = RemoteTable::new(
            vec![cont(cc(7, CcEncoding::Absolute), 100.0, 0.0, 0.5)],
            None,
        );
        let log = run(&t, &[&[0xb0, 7, 0], &[0xb0, 7, 127]], TakeoverMode::None);
        assert_eq!(log.params, vec![100.0, 0.0]);
    }

    #[test]
    fn fourteen_bit_needs_both_bytes_and_uses_the_full_resolution() {
        let t = RemoteTable::new(
            vec![cont(cc(1, CcEncoding::Absolute14Bit), 0.0, 100.0, 0.0)],
            None,
        );
        let mut log = Log::default();
        assert!(t.process(0, &[0xb0, 1, 127], TakeoverMode::None, &mut log));
        assert!(log.params.is_empty(), "waits for the low byte");
        assert!(
            t.process(0, &[0xb0, 33, 127], TakeoverMode::None, &mut log),
            "the low byte's controller is taken too"
        );
        assert_eq!(log.params, vec![100.0]);
        t.process(0, &[0xb0, 1, 64], TakeoverMode::None, &mut log);
        t.process(0, &[0xb0, 33, 0], TakeoverMode::None, &mut log);
        assert!((log.params[1] - 8192.0 / 16383.0 * 100.0).abs() < 1e-6);
    }

    #[test]
    fn pitch_bend_is_absolute_over_fourteen_bits() {
        let t = RemoteTable::new(vec![cont(MidiSource::PitchBend, 0.0, 100.0, 0.5)], None);
        let log = run(
            &t,
            &[&[0xe0, 0, 0], &[0xe0, 0x7f, 0x7f], &[0xe0, 0, 64]],
            TakeoverMode::None,
        );
        assert_eq!(log.params[0], 0.0);
        assert_eq!(log.params[1], 100.0);
        assert!((log.params[2] - 8192.0 / 16383.0 * 100.0).abs() < 1e-6);
    }

    #[test]
    fn each_relative_encoding_nudges_from_the_current_value() {
        let cases = [
            (CcEncoding::RelativeSignedBit, 1, 65),
            (CcEncoding::RelativeSignedBit2, 65, 1),
            (CcEncoding::RelativeBinOffset, 65, 63),
            (CcEncoding::RelativeTwosComplement, 1, 127),
            (CcEncoding::RelativeSignedBitLinear, 1, 65),
            (CcEncoding::RelativeSignedBit2Linear, 65, 1),
            (CcEncoding::RelativeBinOffsetLinear, 65, 63),
            (CcEncoding::RelativeTwosComplementLinear, 1, 127),
        ];
        for (encoding, up, down) in cases {
            let t = RemoteTable::new(vec![cont(cc(9, encoding), 0.0, 100.0, 0.5)], None);
            let log = run(
                &t,
                &[&[0xb0, 9, up], &[0xb0, 9, up], &[0xb0, 9, down]],
                TakeoverMode::PickUp,
            );
            let step = 100.0 / 127.0;
            assert!((log.params[0] - (50.0 + step)).abs() < 1e-6, "{encoding:?}");
            assert!((log.params[1] - (50.0 + 2.0 * step)).abs() < 1e-6);
            assert!((log.params[2] - (50.0 + step)).abs() < 1e-6);
        }
    }

    #[test]
    fn takeover_applies_to_absolute_values_only() {
        let t = RemoteTable::new(
            vec![cont(cc(7, CcEncoding::Absolute), 0.0, 100.0, 0.5)],
            None,
        );
        let log = run(&t, &[&[0xb0, 7, 10]], TakeoverMode::PickUp);
        assert!(log.params.is_empty());
        let log = run(&t, &[&[0xb0, 7, 90]], TakeoverMode::PickUp);
        assert!((log.params[0] - 90.0 / 127.0 * 100.0).abs() < 1e-6);
    }

    #[test]
    fn value_scaling_and_none_differ_on_the_same_stream() {
        let msgs: Vec<[u8; 3]> = vec![[0xb0, 7, 100], [0xb0, 7, 110]];
        let refs: Vec<&[u8]> = msgs.iter().map(|m| &m[..]).collect();
        let none = RemoteTable::new(
            vec![cont(cc(7, CcEncoding::Absolute), 0.0, 100.0, 0.1)],
            None,
        );
        let log = run(&none, &refs, TakeoverMode::None);
        assert!((log.params[1] - 110.0 / 127.0 * 100.0).abs() < 1e-6);
        let scaling = RemoteTable::new(
            vec![cont(cc(7, CcEncoding::Absolute), 0.0, 100.0, 0.1)],
            None,
        );
        let log = run(&scaling, &refs, TakeoverMode::ValueScaling);
        assert_eq!(
            log.params.len(),
            1,
            "the first move only places the control"
        );
        let moved = log.params[0];
        assert!(moved > 10.0 && moved < 50.0, "converges, not {moved}");
    }

    #[test]
    fn note_mapped_to_a_parameter_swaps_between_min_and_max() {
        let t = RemoteTable::new(vec![cont(MidiSource::Note(60), 25.0, 75.0, 0.25)], None);
        let log = run(
            &t,
            &[
                &[0x90, 60, 100],
                &[0x80, 60, 0],
                &[0x90, 60, 100],
                &[0x80, 60, 0],
            ],
            TakeoverMode::None,
        );
        assert_eq!(log.params, vec![75.0, 25.0]);
    }

    #[test]
    fn a_note_range_gives_each_note_its_own_value() {
        let t = RemoteTable::new(
            vec![cont(
                MidiSource::NoteRange { low: 60, high: 64 },
                0.0,
                100.0,
                0.0,
            )],
            None,
        );
        let log = run(
            &t,
            &[&[0x90, 62, 90], &[0x90, 64, 90], &[0x90, 60, 90]],
            TakeoverMode::None,
        );
        assert_eq!(log.params, vec![50.0, 100.0, 0.0]);
    }

    #[test]
    fn switches_follow_the_mapping_range_and_reversed_ranges_invert_them() {
        let t = RemoteTable::new(
            vec![toggle(cc(1, CcEncoding::Absolute), 64.0, 127.0, 0.0, false)],
            None,
        );
        let log = run(
            &t,
            &[&[0xb0, 1, 100], &[0xb0, 1, 10], &[0xb0, 1, 64]],
            TakeoverMode::None,
        );
        assert_eq!(log.params, vec![1.0, 0.0, 1.0]);
        let t = RemoteTable::new(
            vec![toggle(cc(1, CcEncoding::Absolute), 127.0, 64.0, 1.0, false)],
            None,
        );
        let log = run(&t, &[&[0xb0, 1, 100], &[0xb0, 1, 10]], TakeoverMode::None);
        assert_eq!(log.params, vec![0.0, 1.0], "inside the range is off");
    }

    #[test]
    fn a_note_toggles_a_switch_and_relative_values_set_it() {
        let t = RemoteTable::new(
            vec![toggle(MidiSource::Note(1), 64.0, 127.0, 0.0, false)],
            None,
        );
        let log = run(&t, &[&[0x90, 1, 1], &[0x90, 1, 1]], TakeoverMode::None);
        assert_eq!(log.params, vec![1.0, 0.0]);
        let t = RemoteTable::new(
            vec![toggle(
                cc(1, CcEncoding::RelativeBinOffset),
                64.0,
                127.0,
                0.0,
                false,
            )],
            None,
        );
        let log = run(&t, &[&[0xb0, 1, 65], &[0xb0, 1, 63]], TakeoverMode::None);
        assert_eq!(log.params, vec![1.0, 0.0]);
    }

    #[test]
    fn an_inverted_toggle_means_its_cell_the_other_way_round() {
        let t = RemoteTable::new(
            vec![toggle(cc(1, CcEncoding::Absolute), 64.0, 127.0, 1.0, true)],
            None,
        );
        let log = run(&t, &[&[0xb0, 1, 100], &[0xb0, 1, 10]], TakeoverMode::None);
        assert_eq!(log.params, vec![0.0, 1.0]);
    }

    #[test]
    fn radio_buttons_are_stepped_through_by_notes_swept_by_controllers() {
        let t = RemoteTable::new(vec![stepped_entry(MidiSource::Note(1), 2.0, 0.0)], None);
        let log = run(
            &t,
            &[&[0x90, 1, 1], &[0x90, 1, 1], &[0x90, 1, 1]],
            TakeoverMode::None,
        );
        assert_eq!(log.params, vec![1.0, 2.0, 0.0], "wraps to the first");
        let t = RemoteTable::new(
            vec![stepped_entry(cc(2, CcEncoding::Absolute), 2.0, 0.0)],
            None,
        );
        let log = run(
            &t,
            &[&[0xb0, 2, 64], &[0xb0, 2, 127], &[0xb0, 2, 0]],
            TakeoverMode::None,
        );
        assert_eq!(log.params, vec![1.0, 2.0, 0.0]);
        let t = RemoteTable::new(
            vec![stepped_entry(
                cc(3, CcEncoding::RelativeTwosComplement),
                2.0,
                0.0,
            )],
            None,
        );
        let log = run(
            &t,
            &[&[0xb0, 3, 1], &[0xb0, 3, 1], &[0xb0, 3, 1], &[0xb0, 3, 127]],
            TakeoverMode::None,
        );
        assert_eq!(log.params, vec![1.0, 2.0, 1.0], "stops at the end");
    }

    fn slot(source: MidiSource) -> Compiled {
        let track = TrackId(1);
        Compiled {
            channel: 0,
            source,
            target: MapTarget::ClipSlot { track, slot: 2 },
            exec: Exec::Slot {
                track,
                slot: 2,
                direct: true,
            },
            ports: ALL_PORTS,
            feedback: ALL_PORTS,
            value: 0.0,
        }
    }

    #[test]
    fn notes_and_controllers_work_a_slot() {
        let t = RemoteTable::new(vec![slot(MidiSource::Note(36))], None);
        let log = run(&t, &[&[0x90, 36, 127], &[0x80, 36, 0]], TakeoverMode::None);
        assert_eq!(log.slots, vec!["down 2 v1.00 t0", "up 2"]);
        let t = RemoteTable::new(vec![slot(cc(20, CcEncoding::Absolute))], None);
        let log = run(
            &t,
            &[
                &[0xb0, 20, 100],
                &[0xb0, 20, 110],
                &[0xb0, 20, 40],
                &[0xb0, 20, 127],
                &[0xb0, 20, 127],
            ],
            TakeoverMode::None,
        );
        assert_eq!(
            log.slots,
            vec![
                "down 2 v1.00 t0",
                "up 2",
                "down 2 v1.00 t0",
                "down 2 v1.00 t0"
            ],
            "a sweep past 64 presses once; a repeated value is a new press"
        );
    }

    #[test]
    fn a_chromatic_range_plays_a_slot_transposed() {
        let t = RemoteTable::new(
            vec![slot(MidiSource::ChromaticRange {
                root: 60,
                low: 48,
                high: 72,
            })],
            None,
        );
        let log = run(
            &t,
            &[&[0x90, 64, 64], &[0x90, 60, 127], &[0x90, 48, 127]],
            TakeoverMode::None,
        );
        assert_eq!(
            log.slots,
            vec!["down 2 v0.50 t4", "down 2 v1.00 t0", "down 2 v1.00 t-12"]
        );
    }

    #[test]
    fn relative_values_press_and_release_slots_and_buttons() {
        let t = RemoteTable::new(vec![slot(cc(20, CcEncoding::RelativeTwosComplement))], None);
        let log = run(&t, &[&[0xb0, 20, 1], &[0xb0, 20, 127]], TakeoverMode::None);
        assert_eq!(log.slots, vec!["down 2 v1.00 t0", "up 2"]);
    }

    fn ui(kind: UiKind, source: MidiSource, c: RemoteControl) -> Compiled {
        Compiled {
            channel: 0,
            source,
            target: MapTarget::Control(c),
            exec: Exec::Ui(kind),
            ports: ALL_PORTS,
            feedback: ALL_PORTS,
            value: 0.0,
        }
    }

    #[test]
    fn switches_toggle_from_notes_and_follow_the_middle_from_controllers() {
        let t = RemoteTable::new(
            vec![ui(
                UiKind::Switch,
                MidiSource::Note(1),
                RemoteControl::Metronome,
            )],
            None,
        );
        let log = run(&t, &[&[0x90, 1, 90], &[0x80, 1, 0]], TakeoverMode::None);
        assert_eq!(log.gestures.len(), 1);
        assert_eq!(log.gestures[0].1, Gesture::Toggle);
        let t = RemoteTable::new(
            vec![ui(
                UiKind::Switch,
                cc(5, CcEncoding::Absolute),
                RemoteControl::Metronome,
            )],
            None,
        );
        let log = run(&t, &[&[0xb0, 5, 64], &[0xb0, 5, 63]], TakeoverMode::None);
        assert_eq!(log.gestures[0].1, Gesture::On);
        assert_eq!(log.gestures[1].1, Gesture::Off);
    }

    #[test]
    fn a_scroll_box_takes_steps_and_positions() {
        let t = RemoteTable::new(
            vec![ui(
                UiKind::Scroll,
                cc(5, CcEncoding::RelativeTwosComplement),
                RemoteControl::SceneScroll,
            )],
            None,
        );
        let log = run(&t, &[&[0xb0, 5, 3], &[0xb0, 5, 127]], TakeoverMode::None);
        assert_eq!(log.gestures[0].1, Gesture::Step(3));
        assert_eq!(log.gestures[1].1, Gesture::Step(-1));
        let t = RemoteTable::new(
            vec![ui(
                UiKind::Scroll,
                cc(5, CcEncoding::Absolute),
                RemoteControl::SceneScroll,
            )],
            None,
        );
        let log = run(&t, &[&[0xb0, 5, 127]], TakeoverMode::None);
        assert_eq!(log.gestures[0].1, Gesture::Absolute(1.0));
    }

    #[test]
    fn unmapped_messages_are_left_alone() {
        let t = RemoteTable::new(vec![cont(MidiSource::Note(60), 0.0, 100.0, 0.0)], None);
        let mut log = Log::default();
        assert!(!t.process(0, &[0x90, 61, 90], TakeoverMode::None, &mut log));
        assert!(!t.process(0, &[0xb0, 60, 90], TakeoverMode::None, &mut log));
        assert!(!t.process(0, &[0x91, 60, 90], TakeoverMode::None, &mut log));
        assert!(t.process(0, &[0x90, 60, 90], TakeoverMode::None, &mut log));
        assert!(t.process(0, &[0x80, 60, 0], TakeoverMode::None, &mut log));
    }

    #[test]
    fn a_mapping_only_listens_on_its_ports() {
        let mut e = cont(cc(1, CcEncoding::Absolute), 0.0, 100.0, 0.0);
        e.ports = 1 << 3;
        let t = RemoteTable::new(vec![e], None);
        let mut log = Log::default();
        assert!(!t.process(0, &[0xb0, 1, 10], TakeoverMode::None, &mut log));
        assert!(t.process(3, &[0xb0, 1, 10], TakeoverMode::None, &mut log));
    }

    #[test]
    fn a_controller_can_drive_several_targets() {
        let mut other = cont(cc(1, CcEncoding::Absolute), 0.0, 10.0, 0.0);
        other.target = MapTarget::Scene(9);
        let t = RemoteTable::new(
            vec![cont(cc(1, CcEncoding::Absolute), 0.0, 100.0, 0.0), other],
            None,
        );
        let log = run(&t, &[&[0xb0, 1, 127]], TakeoverMode::None);
        assert_eq!(log.params.len(), 2);
    }

    #[test]
    fn publishing_a_new_table_keeps_takeover_state() {
        let source = cc(7, CcEncoding::Absolute);
        let a = RemoteTable::new(vec![cont(source, 0.0, 100.0, 0.5)], None);
        run(&a, &[&[0xb0, 7, 64]], TakeoverMode::PickUp);
        let b = RemoteTable::new(vec![cont(source, 0.0, 100.0, 0.5)], Some(&a));
        let log = run(&b, &[&[0xb0, 7, 80]], TakeoverMode::PickUp);
        assert_eq!(log.params.len(), 1, "already picked up");
    }

    #[test]
    fn a_blocked_mapping_is_taken_but_does_nothing() {
        let mut e = cont(cc(7, CcEncoding::Absolute), 0.0, 100.0, 0.0);
        e.exec = Exec::Blocked;
        let t = RemoteTable::new(vec![e], None);
        let mut log = Log::default();
        assert!(t.process(0, &[0xb0, 7, 90], TakeoverMode::None, &mut log));
        assert!(log.params.is_empty());
    }
}
