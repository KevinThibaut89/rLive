//! What the MIDI effects share: note bookkeeping that makes hung notes impossible, the block
//! driver, scheduling, scales and curves.
//!
//! [`Core`] is the heart of it. Every note a device sends goes through it, so it knows exactly
//! what is sounding and can release all of it on reset, on transport stop or when a device is
//! bypassed. Devices never touch `events_out` themselves.

use rlive_core::{
    BlockStep, EventQueue, ExprKind, MidiMessage, NoteId, NoteSpace, ParamInfo, ParamKind,
    ProcessContext, ScaleInfo, TransportInfo, ViewWriter,
};
use std::sync::atomic::{AtomicU32, Ordering};

/// Notes one device can keep sounding at once. A note-on past this is dropped.
pub const MAX_SOUNDING: usize = 256;
/// Events one device can have scheduled ahead.
const MAX_TIMELINE: usize = 2048;
/// Matches the engine's per-chain queue, so nothing a device writes gets cut.
const OUT_CAPACITY: usize = 2048;
/// What note-ons and expression may use of the output, leaving the rest for note-offs.
const OUT_SOFT_LIMIT: usize = 1536;
/// Parameter changes a device can report in one block.
const MAX_REPORTS: usize = 32;
/// Swallowed notes remembered at once. The oldest is forgotten past this.
const MAX_BLOCKED: usize = 64;

/// A note as it enters or leaves a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteRef {
    pub channel: u8,
    pub key: u8,
    pub id: NoteId,
}

impl NoteRef {
    /// Whether `self` and `other` are the same note. Plain MIDI has no ids, so those compare by
    /// channel and key.
    pub fn is(&self, other: NoteRef) -> bool {
        if self.id != 0 && other.id != 0 {
            self.id == other.id
        } else {
            self.channel == other.channel && self.key == other.key
        }
    }
}

/// The source of a note a device generated out of nothing, so no incoming note can match it.
pub const GENERATED: NoteRef = NoteRef {
    channel: u8::MAX,
    key: u8::MAX,
    id: u32::MAX,
};

/// A fresh id for a note a device made up. Ids come from one counter shared by every device, so
/// two MIDI effects in a row can't collide; the space in the high bits keeps them non-zero.
pub fn device_note_id() -> NoteId {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    ((NoteSpace::Device as u32) << 28) | (NEXT.fetch_add(1, Ordering::Relaxed) & 0x0fff_ffff)
}

/// Seed for a device's random numbers. Only for use off the audio thread.
pub fn entropy_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU32 = AtomicU32::new(1);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    t ^ ((COUNTER.fetch_add(1, Ordering::Relaxed) as u64) << 32) ^ 0x9e37_79b9_7f4a_7c15
}

/// A small xorshift generator: deterministic under a seed and never allocates.
#[derive(Debug, Clone, Copy)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in `0.0..1.0`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in `0..n`. Zero when `n` is zero.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            ((self.next_u64() >> 32) as usize * n) >> 32
        }
    }
}

#[derive(Clone, Copy)]
struct Sounding {
    out: NoteRef,
    src: NoteRef,
}

#[derive(Clone, Copy)]
struct Timed {
    due: u64,
    msg: MidiMessage,
    src: NoteRef,
}

/// Note bookkeeping and output for one device. See the module docs.
pub struct Core {
    out: EventQueue,
    sounding: Vec<Sounding>,
    timeline: Vec<Timed>,
    reports: Vec<(u32, f64)>,
    /// Notes a device swallowed: their note-offs must be swallowed as well.
    blocked: Vec<NoteRef>,
    /// Frames since the device was made, at `pos`.
    clock: u64,
    /// Where we are in the current block.
    pos: u32,
    sample_rate: f64,
    tempo: f64,
    /// Song position in beats at `pos`. Runs on its own while the transport is stopped.
    beat: f64,
    playing: bool,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            out: EventQueue::with_capacity(OUT_CAPACITY),
            sounding: Vec::with_capacity(MAX_SOUNDING),
            timeline: Vec::with_capacity(MAX_TIMELINE),
            reports: Vec::with_capacity(MAX_REPORTS),
            blocked: Vec::with_capacity(MAX_BLOCKED),
            clock: 0,
            pos: 0,
            sample_rate: 48_000.0,
            tempo: 120.0,
            beat: 0.0,
            playing: false,
        }
    }
}

impl Core {
    /// Frames since the device was made.
    pub fn now(&self) -> u64 {
        self.clock
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Song position in beats, right now.
    pub fn beat(&self) -> f64 {
        self.beat
    }

    pub fn beats_per_frame(&self) -> f64 {
        (self.tempo / 60.0 / self.sample_rate.max(1.0)).max(1e-12)
    }

    pub fn ms_to_frames(&self, ms: f64) -> f64 {
        ms * 0.001 * self.sample_rate
    }

    pub fn beats_to_frames(&self, beats: f64) -> f64 {
        beats / self.beats_per_frame()
    }

    /// Whether a note this device sent is still sounding.
    pub fn is_sounding(&self, out: NoteRef) -> bool {
        self.sounding.iter().any(|s| s.out == out)
    }

    /// Whether anything is sounding that came from `src`.
    pub fn is_sounding_from(&self, src: NoteRef) -> bool {
        self.sounding.iter().any(|s| s.src.is(src))
    }

    /// A note a device just dropped. Its note-off won't reach anything downstream.
    pub fn block(&mut self, src: NoteRef) {
        if self.blocked.len() >= MAX_BLOCKED {
            self.blocked.swap_remove(0);
        }
        self.blocked.push(src);
    }

    /// Handles everything except a note-on the usual way: note-offs and expression follow their
    /// notes to whatever the device turned them into, and the rest passes straight through. A
    /// note-off for a note the device never saw passes through too, because it might belong to
    /// a note that started before the device was added.
    pub fn pass_other(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOff {
                channel,
                key,
                velocity,
                note_id,
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                if let Some(i) = self.blocked.iter().position(|b| b.is(src)) {
                    self.blocked.swap_remove(i);
                } else if !self.release_from(src, velocity) {
                    self.forward(message);
                }
            }
            MidiMessage::NoteExpr {
                channel,
                key,
                note_id,
                kind,
                value,
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                if !self.expr_from(src, kind, value) {
                    self.forward(message);
                }
            }
            MidiMessage::AllNotesOff => {
                self.flush();
                self.forward(message);
            }
            other => self.forward(other),
        }
    }

    /// Passes a note or message through unchanged, tracking notes so a reset can end them.
    pub fn pass_through(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                note_id,
                tuning,
            } => {
                let note = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                self.note_on(note, note, velocity, tuning);
            }
            other => self.pass_other(other),
        }
    }

    /// Hand a message on untouched, keeping no record of it.
    pub fn forward(&mut self, msg: MidiMessage) {
        self.push(self.pos, msg);
    }

    /// Send a note-on, now. `src` is the incoming note it belongs to, or itself for a note that
    /// isn't part of one.
    pub fn note_on(&mut self, src: NoteRef, out: NoteRef, velocity: f32, tuning: f32) {
        self.deliver(self.pos, Self::on_msg(out, velocity, tuning), src);
    }

    /// Send a note-on `delay` frames from now.
    pub fn note_on_after(
        &mut self,
        delay: u64,
        src: NoteRef,
        out: NoteRef,
        velocity: f32,
        tuning: f32,
    ) {
        if delay == 0 {
            self.note_on(src, out, velocity, tuning);
        } else {
            self.schedule(delay, Self::on_msg(out, velocity, tuning), src);
        }
    }

    fn on_msg(out: NoteRef, velocity: f32, tuning: f32) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: out.channel,
            key: out.key,
            velocity: velocity.clamp(1.0 / 127.0, 1.0),
            note_id: out.id,
            tuning,
        }
    }

    /// End a note `delay` frames from now. Zero means now.
    pub fn note_off_after(&mut self, delay: u64, out: NoteRef, velocity: f32) {
        let msg = MidiMessage::NoteOff {
            channel: out.channel,
            key: out.key,
            velocity,
            note_id: out.id,
        };
        if delay == 0 {
            self.deliver(self.pos, msg, out);
        } else {
            self.schedule(delay, msg, out);
        }
    }

    /// End every note that came from `src`. Returns whether there were any.
    pub fn release_from(&mut self, src: NoteRef, velocity: f32) -> bool {
        let mut found = false;
        let mut i = 0;
        while i < self.sounding.len() {
            let s = self.sounding[i];
            if s.src.is(src) {
                found = true;
                self.sounding.swap_remove(i);
                self.push(
                    self.pos,
                    MidiMessage::NoteOff {
                        channel: s.out.channel,
                        key: s.out.key,
                        velocity,
                        note_id: s.out.id,
                    },
                );
            } else {
                i += 1;
            }
        }
        found
    }

    /// Send expression to every note that came from `src`. Returns whether there were any.
    pub fn expr_from(&mut self, src: NoteRef, kind: ExprKind, value: f32) -> bool {
        let mut found = false;
        for i in 0..self.sounding.len() {
            let s = self.sounding[i];
            if s.src.is(src) {
                found = true;
                self.push(
                    self.pos,
                    MidiMessage::NoteExpr {
                        channel: s.out.channel,
                        key: s.out.key,
                        note_id: s.out.id,
                        kind,
                        value,
                    },
                );
            }
        }
        found
    }

    /// Deliver `msg` `delay` frames from now. A note-off that doesn't fit goes out at once, so
    /// it can never be lost.
    pub fn schedule(&mut self, delay: u64, msg: MidiMessage, src: NoteRef) {
        if self.timeline.len() < MAX_TIMELINE {
            self.timeline.push(Timed {
                due: self.clock + delay,
                msg,
                src,
            });
        } else if matches!(msg, MidiMessage::NoteOff { .. }) {
            self.deliver(self.pos, msg, src);
        }
    }

    /// Release everything sounding and forget everything planned.
    pub fn flush(&mut self) {
        self.timeline.clear();
        self.blocked.clear();
        for i in 0..self.sounding.len() {
            let s = self.sounding[i];
            self.push(
                self.pos,
                MidiMessage::NoteOff {
                    channel: s.out.channel,
                    key: s.out.key,
                    velocity: 0.0,
                    note_id: s.out.id,
                },
            );
        }
        self.sounding.clear();
    }

    /// Tell the UI a parameter changed under the device's own hand.
    pub fn report(&mut self, id: u32, value: f64) {
        if self.reports.len() < MAX_REPORTS {
            self.reports.push((id, value));
        }
    }

    /// Move time on, sending whatever falls due.
    pub fn advance(&mut self, frames: u64) {
        let end = self.clock + frames;
        // In time order, so a note-on is always delivered before its own note-off.
        while let Some(i) = (0..self.timeline.len())
            .filter(|&i| self.timeline[i].due < end)
            .min_by_key(|&i| self.timeline[i].due)
        {
            let t = self.timeline.swap_remove(i);
            let offset = self.pos + t.due.saturating_sub(self.clock) as u32;
            self.deliver(offset, t.msg, t.src);
        }
        self.clock = end;
        self.pos += frames as u32;
        self.beat += frames as f64 * self.beats_per_frame();
    }

    fn deliver(&mut self, offset: u32, msg: MidiMessage, src: NoteRef) {
        match msg {
            MidiMessage::NoteOn {
                channel,
                key,
                note_id,
                ..
            } => {
                if self.sounding.len() >= MAX_SOUNDING {
                    return;
                }
                let out = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                self.sounding.push(Sounding { out, src });
                self.push(offset, msg);
            }
            MidiMessage::NoteOff {
                channel,
                key,
                note_id,
                ..
            } => {
                let out = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                if let Some(i) = self.sounding.iter().position(|s| s.out == out) {
                    self.sounding.swap_remove(i);
                    self.push(offset, msg);
                }
            }
            _ => self.push(offset, msg),
        }
    }

    fn push(&mut self, offset: u32, msg: MidiMessage) {
        let room = matches!(msg, MidiMessage::NoteOff { .. } | MidiMessage::AllNotesOff)
            || self.out.len() < OUT_SOFT_LIMIT;
        if room {
            self.out.push(rlive_core::MidiEvent::new(offset, msg));
        }
    }

    /// Start a block. Returns `true` when the transport has just stopped.
    pub(crate) fn begin_block(&mut self, transport: &TransportInfo) -> bool {
        let stopped = self.playing && !transport.playing;
        self.playing = transport.playing;
        self.sample_rate = transport.sample_rate;
        self.tempo = transport.tempo;
        if transport.playing {
            self.beat = transport.beats();
        }
        stopped
    }

    pub(crate) fn finish_block(&mut self, ctx: &mut ProcessContext) {
        ctx.events_out.extend(self.out.as_slice());
        self.out.clear();
        for (id, value) in self.reports.drain(..) {
            ctx.param_out.push(id, value);
        }
        self.pos = 0;
    }
}

/// What a MIDI effect implements. [`run`] does the rest.
pub trait MidiFx {
    fn core(&mut self) -> &mut Core;

    /// Set a parameter's value.
    fn apply(&mut self, id: u32, value: f64);

    /// A parameter change arriving while playing. Devices with buttons act on them here.
    fn param_changed(&mut self, id: u32, value: f64) {
        self.apply(id, value);
    }

    fn midi(&mut self, message: MidiMessage);

    /// Move time on. Devices that generate on a clock override this.
    fn advance(&mut self, frames: usize) {
        self.core().advance(frames as u64);
    }

    /// Forget played notes: the transport stopped, or the device was reset or bypassed.
    fn clear(&mut self) {}

    fn begin(&mut self, _transport: &TransportInfo) {}

    /// The block is done: publish meters and modulation.
    fn end(&mut self, _mod_out: &mut [f32], _view: &ViewWriter) {}
}

/// Runs one block through a MIDI effect.
pub fn run<D: MidiFx>(device: &mut D, ctx: &mut ProcessContext) {
    if device.core().begin_block(ctx.transport) {
        reset(device);
    }
    device.begin(ctx.transport);
    ctx.for_each_step(|step| match step {
        BlockStep::Audio(audio) => device.advance(audio.frames()),
        BlockStep::Param { id, value } => device.param_changed(id, value),
        BlockStep::Midi(message) => device.midi(message),
    });
    device.end(ctx.mod_out, ctx.view);
    device.core().finish_block(ctx);
}

/// Release everything the device has sounding and forget played notes. The note-offs go out at
/// the start of the next block the device runs.
pub fn reset<D: MidiFx>(device: &mut D) {
    device.core().flush();
    device.clear();
}

/// Implements [`Processor`](rlive_core::Processor) for a struct with a `core: Core` field, by way
/// of its [`MidiFx`] impl.
macro_rules! midi_fx_processor {
    ($ty:ty) => {
        impl rlive_core::Processor for $ty {
            fn activate(&mut self, _sample_rate: f64, _max_block: usize) {}

            fn process(&mut self, ctx: &mut rlive_core::ProcessContext) {
                $crate::devices::b1::common::run(self, ctx);
            }

            fn set_param(&mut self, id: u32, value: f64) {
                $crate::devices::b1::common::MidiFx::apply(self, id, value);
            }

            fn reset(&mut self) {
                $crate::devices::b1::common::reset(self);
            }
        }
    };
}

pub(crate) use midi_fx_processor;

/// A whole-number parameter over `min..=max`, with no label for each value.
pub fn int_param(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParamInfo {
    ParamInfo {
        kind: ParamKind::Stepped { labels: Vec::new() },
        ..ParamInfo::continuous(id, name, min, max, default, "")
    }
}

/// A percentage knob: `min..=max` plain percent.
pub fn percent_param(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParamInfo {
    ParamInfo::continuous(id, name, min, max, default, "%")
}

/// A scale's name and its notes, as a 12-bit mask above the root.
pub const SCALES: &[(&str, u16)] = &[
    ("Major", 0b1010_1011_0101),
    ("Minor", 0b0101_1010_1101),
    ("Dorian", 0b0110_1010_1101),
    ("Mixolydian", 0b0110_1011_0101),
    ("Lydian", 0b1010_1101_0101),
    ("Phrygian", 0b0101_1010_1011),
    ("Locrian", 0b0101_0110_1011),
    ("Whole Tone", 0b0101_0101_0101),
    ("Half-Whole Dim.", 0b0110_1101_1011),
    ("Whole-Half Dim.", 0b1011_0110_1101),
    ("Minor Blues", 0b0100_1110_1001),
    ("Minor Pentatonic", 0b0100_1010_1001),
    ("Major Pentatonic", 0b0010_1001_0101),
    ("Harmonic Minor", 0b1001_1010_1101),
    ("Harmonic Major", 0b1001_1011_0101),
    ("Dorian #4", 0b0110_1100_1101),
    ("Phrygian Dominant", 0b0101_1011_0011),
    ("Melodic Minor", 0b1010_1010_1101),
    ("Lydian Augmented", 0b1011_0101_0101),
    ("Lydian Dominant", 0b0110_1101_0101),
    ("Super Locrian", 0b0101_0101_1011),
    ("Bhairav", 0b1001_1011_0011),
    ("Hungarian Minor", 0b1001_1100_1101),
    ("Hirajoshi", 0b0001_1000_1101),
    ("In-Sen", 0b0100_1010_0011),
    ("Iwato", 0b0100_0110_0011),
    ("Kumoi", 0b0010_1000_1101),
    ("Pelog Selisir", 0b0001_1000_1011),
    ("Pelog Tembung", 0b0001_1010_0011),
    ("8-Tone Spanish", 0b0101_0111_1011),
];

/// Transposes `key` by `degrees` of `scale`. A key outside the scale keeps its distance above
/// the scale note below it.
pub fn transpose_degrees(key: i32, degrees: i32, scale: ScaleInfo) -> i32 {
    let mask = scale.mask & 0x0fff;
    let count = mask.count_ones() as i32;
    if count == 0 {
        return key + degrees;
    }
    let root = scale.root as i32;
    let offset = key - root;
    let octave = offset.div_euclid(12);
    let pc = offset.rem_euclid(12);
    let mut degree = 0;
    let mut interval = 0;
    for n in 0..12 {
        if mask & (1 << n) != 0 {
            if n > pc {
                break;
            }
            interval = n;
            degree = (0..n).filter(|b| mask & (1 << b) != 0).count() as i32;
        }
    }
    let rest = pc - interval;
    let target = degree + degrees;
    let index = target.rem_euclid(count);
    let mut pitch = 0;
    let mut seen = 0;
    for n in 0..12 {
        if mask & (1 << n) != 0 {
            if seen == index {
                pitch = n;
                break;
            }
            seen += 1;
        }
    }
    root + (octave + target.div_euclid(count)) * 12 + pitch + rest
}

/// [`transpose_degrees`] for a pitch between keys: moves it as the keys either side of it move.
pub fn transpose_degrees_fractional(pitch: f32, degrees: i32, scale: ScaleInfo) -> f32 {
    let floor = pitch.floor();
    let frac = pitch - floor;
    let low = transpose_degrees(floor as i32, degrees, scale) as f32;
    let high = transpose_degrees(floor as i32 + 1, degrees, scale) as f32;
    low + (high - low) * frac
}

/// The notes a [`Transposer`] moves by, for devices that can work in scale degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transposer {
    #[default]
    Chromatic,
    Scale(ScaleInfo),
}

impl Transposer {
    pub fn apply(self, key: i32, amount: i32) -> i32 {
        match self {
            Self::Chromatic => key + amount,
            Self::Scale(scale) => transpose_degrees(key, amount, scale),
        }
    }

    /// Picks what a Use Current Scale toggle resolves to: the clip's scale when the toggle is
    /// on and there is one, otherwise semitones.
    pub fn resolve(use_current: bool, transport: &TransportInfo) -> Self {
        match (use_current, transport.scale) {
            (true, Some(scale)) => Self::Scale(scale),
            _ => Self::Chromatic,
        }
    }
}

/// Bends `x` (0..=1). Positive shape lifts the middle, negative lowers it.
#[inline]
pub fn shape(x: f32, s: f32) -> f32 {
    x.clamp(0.0, 1.0).powf((-s.clamp(-1.0, 1.0) * 2.08).exp())
}

/// The curve Expression Control and MPE Control share.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    pub s_shaped: bool,
    pub a: f32,
    pub b: f32,
    pub link: bool,
    pub min: f32,
    pub max: f32,
    pub x: f32,
    pub y: f32,
}

impl Default for Curve {
    fn default() -> Self {
        Self {
            s_shaped: false,
            a: 0.0,
            b: 0.0,
            link: true,
            min: 0.0,
            max: 1.0,
            x: 0.5,
            y: 0.5,
        }
    }
}

impl Curve {
    /// Maps `input` (0..=1) to the output range, `min..=max`.
    pub fn apply(&self, input: f32) -> f32 {
        let x = input.clamp(0.0, 1.0);
        let y = if self.s_shaped {
            let (bx, by) = (self.x.clamp(0.001, 0.999), self.y.clamp(0.0, 1.0));
            let lower = if self.link { -self.a } else { self.b };
            if x < bx {
                by * shape(x / bx, lower)
            } else {
                by + (1.0 - by) * shape((x - bx) / (1.0 - bx), self.a)
            }
        } else {
            shape(x, self.a)
        };
        self.min + y * (self.max - self.min)
    }
}

/// Follows a target value over a rise or fall time, linearly or logarithmically.
#[derive(Debug, Clone, Copy, Default)]
pub struct Smoother {
    pub value: f32,
    target: f32,
    slope: f32,
}

impl Smoother {
    pub fn set_target(&mut self, target: f32, rise_frames: f64, fall_frames: f64, linear: bool) {
        if target == self.target && self.value != target {
            return;
        }
        self.target = target;
        let frames = if target >= self.value {
            rise_frames
        } else {
            fall_frames
        };
        if frames < 1.0 {
            self.value = target;
            self.slope = 0.0;
        } else if linear {
            self.slope = ((target - self.value).abs() / frames as f32).max(f32::EPSILON);
        } else {
            self.slope = -1.0 / (frames as f32 / 4.6);
        }
    }

    /// Moves on by `frames`. Returns whether the value changed.
    pub fn tick(&mut self, frames: usize) -> bool {
        let before = self.value;
        if self.slope > 0.0 {
            let step = self.slope * frames as f32;
            self.value = if (self.target - self.value).abs() <= step {
                self.target
            } else {
                self.value + step.copysign(self.target - self.value)
            };
        } else if self.slope < 0.0 {
            let ratio = (self.slope * frames as f32).exp();
            self.value = self.target + (self.value - self.target) * ratio;
            if (self.value - self.target).abs() < 1e-4 {
                self.value = self.target;
            }
        }
        self.value != before
    }

    /// Whether it still has somewhere to go.
    pub fn moving(&self) -> bool {
        self.value != self.target
    }

    pub fn jump(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.slope = 0.0;
    }
}

/// The controllers worth naming in a CC menu. The rest are shown by number.
const CC_NAMES: &[(usize, &str)] = &[
    (0, "Bank Select"),
    (1, "Mod Wheel"),
    (2, "Breath"),
    (4, "Foot"),
    (5, "Portamento Time"),
    (6, "Data MSB"),
    (7, "Volume"),
    (8, "Balance"),
    (10, "Pan"),
    (11, "Expression"),
    (64, "Sustain"),
    (65, "Portamento"),
    (66, "Sostenuto"),
    (67, "Soft Pedal"),
    (68, "Legato"),
    (69, "Hold 2"),
    (71, "Resonance"),
    (72, "Release"),
    (73, "Attack"),
    (74, "Brightness"),
];

/// A menu of the 128 controllers.
pub fn cc_labels() -> Vec<String> {
    (0..128)
        .map(|cc| match CC_NAMES.iter().find(|(n, _)| *n == cc) {
            Some((_, name)) => format!("{cc} {name}"),
            None => cc.to_string(),
        })
        .collect()
}
