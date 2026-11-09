//! What the modulators share: the mapping controls every one of them has, how a modulator's
//! output becomes what its targets see, and the scope feed.
//!
//! A modulator publishes a value and a Remote Control flag per mapping on `mod_out`; see
//! `rlive_engine::modulation`. Everything about Modulation and Remote Control, Depth included,
//! is worked out here, on the audio thread, so all of it can be automated.

use crate::devices::b1::expression_control::{self, slot_param};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::Rng;
use rlive_core::{
    EventQueue, Item, MidiEvent, MidiMessage, ParamId, ParamInfo, Section, ViewId, ViewWriter,
    SCOPE_VALUES,
};
use std::sync::atomic::{AtomicU32, Ordering};

/// How many targets a modulator can have (manual p. 844).
pub const MAPPINGS: usize = 8;

/// Declared outputs: one value per mapping, then its Remote Control flag.
pub const MOD_PORTS: [&str; MAPPINGS * 2] = [
    "Map 1",
    "Map 2",
    "Map 3",
    "Map 4",
    "Map 5",
    "Map 6",
    "Map 7",
    "Map 8",
    "Map 1 Remote",
    "Map 2 Remote",
    "Map 3 Remote",
    "Map 4 Remote",
    "Map 5 Remote",
    "Map 6 Remote",
    "Map 7 Remote",
    "Map 8 Remote",
];

/// Parameters of mapping `n` are numbered from `MAP_FIRST + n * MAP_PARAMS`.
pub const MAP_FIRST: u32 = 200;
pub const MAP_PARAMS: u32 = 5;
pub const MAP_MODE: u32 = 0;
pub const MAP_POLARITY: u32 = 1;
pub const MAP_AMOUNT: u32 = 2;
pub const MAP_MIN: u32 = 3;
pub const MAP_MAX: u32 = 4;

/// Where a modulator's scope is published.
pub const SCOPE: ViewId = ViewId(0);
/// Where its trigger LED is published, after the scope.
pub const LED: ViewId = ViewId(SCOPE_VALUES as u32);
pub const VIEW_LEN: usize = SCOPE_VALUES + 1;

pub const fn map_param(slot: usize, p: u32) -> u32 {
    MAP_FIRST + slot as u32 * MAP_PARAMS + p
}

/// How many outputs a modulator device has to map, and so how many of `Device::modulations` it
/// can keep. Zero means it isn't a modulator.
pub fn map_outputs(type_id: &str) -> usize {
    match type_id {
        "rlive.lfo"
        | "rlive.envelope_follower"
        | "rlive.shaper"
        | "rlive.envelope_midi"
        | "rlive.shaper_midi" => MAPPINGS,
        "rlive.expression_control" => expression_control::SLOTS,
        _ => 0,
    }
}

/// The parameter that says whether a mapping is a Remote Control.
pub fn remote_param(type_id: &str, slot: usize) -> Option<u32> {
    if slot >= map_outputs(type_id) {
        return None;
    }
    Some(match type_id {
        "rlive.expression_control" => slot_param(slot, expression_control::MODULATION),
        _ => map_param(slot, MAP_MODE),
    })
}

pub fn map_info(default_bipolar: bool) -> Vec<ParamInfo> {
    let mut out = Vec::with_capacity(MAPPINGS * MAP_PARAMS as usize);
    for slot in 0..MAPPINGS {
        let id = |p| map_param(slot, p);
        let name = |s: &str| format!("Map {} {s}", slot + 1);
        out.push(ParamInfo::stepped(
            id(MAP_MODE),
            &name("Mod"),
            &["Modulation", "Remote"],
            0,
        ));
        out.push(ParamInfo::stepped(
            id(MAP_POLARITY),
            &name("Polarity"),
            &["Bipolar", "Unipolar"],
            !default_bipolar as usize,
        ));
        out.push(ParamInfo::continuous(
            id(MAP_AMOUNT),
            &name("Amount"),
            -100.0,
            100.0,
            100.0,
            "%",
        ));
        out.push(ParamInfo::continuous(
            id(MAP_MIN),
            &name("Min"),
            0.0,
            100.0,
            0.0,
            "%",
        ));
        out.push(ParamInfo::continuous(
            id(MAP_MAX),
            &name("Max"),
            0.0,
            100.0,
            100.0,
            "%",
        ));
    }
    out
}

/// The Map buttons and each mapping's controls.
pub fn map_section() -> Section {
    Section {
        title: "Map",
        items: vec![Item::Panel("mod_map")],
    }
}

#[derive(Clone, Copy, Debug)]
struct MapSlot {
    remote: bool,
    bipolar: bool,
    amount: f32,
    min: f32,
    max: f32,
}

/// Turns a modulator's position, 0 to 1, into what each of its mappings does.
#[derive(Clone, Copy, Debug)]
pub struct MapOutput {
    slots: [MapSlot; MAPPINGS],
}

impl MapOutput {
    pub fn new(default_bipolar: bool) -> Self {
        Self {
            slots: [MapSlot {
                remote: false,
                bipolar: default_bipolar,
                amount: 1.0,
                min: 0.0,
                max: 1.0,
            }; MAPPINGS],
        }
    }

    /// Takes a parameter change if it is one of the mapping controls. Returns whether it was.
    pub fn apply(&mut self, id: u32, value: f64) -> bool {
        let Some(offset) = id.checked_sub(MAP_FIRST) else {
            return false;
        };
        let (slot, p) = ((offset / MAP_PARAMS) as usize, offset % MAP_PARAMS);
        let Some(slot) = self.slots.get_mut(slot) else {
            return false;
        };
        let unit = (value * 0.01) as f32;
        match p {
            MAP_MODE => slot.remote = value >= 0.5,
            MAP_POLARITY => slot.bipolar = value < 0.5,
            MAP_AMOUNT => slot.amount = unit,
            MAP_MIN => slot.min = unit,
            _ => slot.max = unit,
        }
        true
    }

    /// Publishes this position. `position` is 0 to 1, and 0.5 is the middle of a bipolar swing.
    pub fn write(&self, position: f32, mod_out: &mut [f32]) {
        let u = position.clamp(0.0, 1.0);
        for (i, s) in self.slots.iter().enumerate() {
            let (value, flag) = if s.remote {
                (s.min + (s.max - s.min) * u, 1.0)
            } else if s.bipolar {
                ((u * 2.0 - 1.0) * s.amount, 0.0)
            } else {
                (u * s.amount, 0.0)
            };
            if let Some(out) = mod_out.get_mut(i) {
                *out = value;
            }
            if let Some(out) = mod_out.get_mut(MAPPINGS + i) {
                *out = flag;
            }
        }
    }
}

/// Jitter, Smooth, Depth and Offset: what happens to a modulator's wave on its way out (manual
/// pp. 844-845). The manual gives no figures, so Smooth's 0.5 s and the jitter rate are ours.
#[derive(Clone, Copy, Debug)]
pub struct Conditioner {
    pub jitter: f32,
    pub smooth: f32,
    pub depth: f32,
    pub offset: f32,
    state: f32,
    rng: Rng,
    primed: bool,
}

impl Conditioner {
    pub fn new(seed: u32) -> Self {
        Self {
            jitter: 0.0,
            smooth: 0.0,
            depth: 1.0,
            offset: 0.0,
            state: 0.0,
            rng: Rng::new(seed),
            primed: false,
        }
    }

    /// Conditions one control step's wave, in -1..1, into a position 0..1.
    pub fn step(&mut self, wave: f32, dt_seconds: f32) -> f32 {
        let noisy = (wave + self.jitter * self.rng.bipolar()).clamp(-1.0, 1.0);
        let tau = 0.5 * self.smooth * self.smooth;
        if !self.primed || tau <= 0.0 {
            self.state = noisy;
            self.primed = true;
        } else {
            let coef = (-dt_seconds / tau).exp();
            self.state = noisy + (self.state - noisy) * coef;
        }
        ((self.state * self.depth + self.offset).clamp(-1.0, 1.0) + 1.0) * 0.5
    }
}

/// Where a modulator's cycle is: free-running in Hertz, or locked to the song in beats. Hold and
/// Retrigger shift a locked cycle rather than leave the song grid.
#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    pub pos: f64,
    shift: f64,
    pub clock: BeatClock,
}

/// Fractional part, always in `0.0..1.0`.
#[inline]
fn frac(x: f64) -> f64 {
    x - x.floor()
}

impl Clock {
    /// Advance by `n` samples. Returns whether the cycle wrapped.
    pub fn advance(&mut self, n: usize, sample_rate: f64, rate: Rate) -> bool {
        let before = self.pos;
        match rate {
            Rate::Hz(hz) => {
                self.clock.advance(n);
                self.pos = frac(self.pos + f64::from(hz.max(0.0)) * n as f64 / sample_rate);
            }
            Rate::Beats(beats) if self.clock.playing => {
                self.clock.advance(n);
                self.pos = frac(self.clock.beat / beats.max(1e-6) + self.shift);
            }
            Rate::Beats(beats) => {
                let per = self.clock.beats_per_sample / beats.max(1e-6);
                self.clock.advance(n);
                self.pos = frac(self.pos + per * n as f64);
            }
        }
        self.pos < before
    }

    /// Back to the start of the cycle.
    pub fn retrigger(&mut self, rate: Rate) {
        self.resume_at(0.0, rate);
    }

    /// Carry on from `pos`.
    pub fn resume_at(&mut self, pos: f64, rate: Rate) {
        self.pos = pos;
        if let Rate::Beats(beats) = rate {
            self.shift = pos - frac(self.clock.beat / beats.max(1e-6));
        }
    }

    /// One-shot time: `n` samples of progress towards 1, in cycles.
    pub fn increment(&self, n: usize, sample_rate: f64, rate: Rate) -> f64 {
        match rate {
            Rate::Hz(hz) => f64::from(hz.max(0.0)) * n as f64 / sample_rate,
            Rate::Beats(beats) => self.clock.beats_per_sample / beats.max(1e-6) * n as f64,
        }
    }
}

/// How fast a modulator runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rate {
    Hz(f32),
    Beats(f64),
}

impl Rate {
    pub fn of(sync: bool, hz: f32, sync_index: f64) -> Self {
        if sync {
            Self::Beats(SyncRate::from_index(sync_index).beats())
        } else {
            Self::Hz(hz)
        }
    }
}

/// Keeps the last stretch of a modulator's output for its display.
pub struct ScopeFeed {
    values: [f32; SCOPE_VALUES],
    next: usize,
    led: f32,
}

impl Default for ScopeFeed {
    fn default() -> Self {
        Self {
            values: [0.0; SCOPE_VALUES],
            next: 0,
            led: 0.0,
        }
    }
}

impl ScopeFeed {
    /// Store one position, 0 to 1.
    #[inline]
    pub fn push(&mut self, position: f32) {
        self.values[self.next] = position * 2.0 - 1.0;
        self.next = (self.next + 1) % SCOPE_VALUES;
    }

    pub fn trigger(&mut self) {
        self.led = 1.0;
    }

    /// Publish the history, oldest first.
    pub fn publish(&mut self, view: &ViewWriter, dt: f32) {
        self.led = (self.led - dt * 8.0).max(0.0);
        if !view.is_active() {
            return;
        }
        let mut out = [0.0f32; SCOPE_VALUES];
        let (old, new) = self.values.split_at(self.next);
        out[..new.len()].copy_from_slice(new);
        out[new.len()..].copy_from_slice(old);
        view.publish(SCOPE, &out);
        view.publish(LED, &[self.led]);
    }
}

/// Notes a MIDI modulator passed along and hasn't seen end, so it can end them when it's reset,
/// the way any MIDI effect has to.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThroughNotes {
    held: [u128; 16],
    release: bool,
    was_playing: bool,
}

impl ThroughNotes {
    pub fn note(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { channel, key, .. } => {
                self.held[(channel & 0x0f) as usize] |= 1u128 << (key & 0x7f);
            }
            MidiMessage::NoteOff { channel, key, .. } => {
                self.held[(channel & 0x0f) as usize] &= !(1u128 << (key & 0x7f));
            }
            MidiMessage::AllNotesOff => self.held = [0; 16],
            _ => {}
        }
    }

    pub fn reset(&mut self) {
        self.release = true;
    }

    /// Hand over the note-offs a reset, or the transport stopping, owes the chain.
    pub fn flush(&mut self, playing: bool, out: &mut EventQueue) {
        if self.was_playing && !playing {
            self.release = true;
        }
        self.was_playing = playing;
        if !std::mem::take(&mut self.release) {
            return;
        }
        for (channel, held) in self.held.iter_mut().enumerate() {
            let mut notes = *held;
            *held = 0;
            while notes != 0 {
                let key = notes.trailing_zeros() as u8;
                notes &= notes - 1;
                out.push(MidiEvent::new(
                    0,
                    MidiMessage::NoteOff {
                        channel: channel as u8,
                        key,
                        velocity: 0.0,
                        note_id: 0,
                    },
                ));
            }
        }
    }
}

/// A seed that differs between devices, so two free-running modulators don't move together.
pub fn next_seed() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0x2b1d);
    NEXT.fetch_add(0x9e37, Ordering::Relaxed)
}

/// Layout items for a parameter id.
pub fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

pub fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

pub fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

pub fn slider(id: u32) -> Item {
    Item::Slider(ParamId(id))
}
