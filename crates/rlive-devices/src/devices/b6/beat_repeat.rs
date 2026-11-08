//! Beat Repeat: captures a slice of the incoming audio on a beat grid and loops it.
//!
//! The manual gives Interval, Offset, Chance, Gate, Repeat, Grid, No Triplets, Variation and its
//! modes, Pitch and Pitch Decay, the filter, the three mix modes, Volume and Decay. It gives no
//! ranges and doesn't say what a repetition is while the slice is still being captured; ours
//! follows the manual's "repetitions will occur ... starting at the position defined by Interval
//! and Offset": repetition 1 plays the slice as it arrives, so it adds to the original in Mix
//! mode and replaces it in Insert and Gate. Volume, "the output level of the device" in the
//! manual, applies to the whole output. The depths of Pitch Decay and Variation are ours.

use super::common::{knob, menu, toggle, values, LinkedBand};
use crate::dsp::util::{sanitize, Rng};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor,
    Section, Status, TransportInfo,
};

pub const INTERVAL: u32 = 0;
pub const OFFSET: u32 = 1;
pub const CHANCE: u32 = 2;
pub const GATE: u32 = 3;
pub const GRID: u32 = 4;
pub const NO_TRIPLETS: u32 = 5;
pub const VARIATION: u32 = 6;
pub const VARIATION_MODE: u32 = 7;
pub const REPEAT: u32 = 8;
pub const PITCH: u32 = 9;
pub const PITCH_DECAY: u32 = 10;
pub const FILTER_ON: u32 = 11;
pub const FILTER_FREQ: u32 = 12;
pub const FILTER_WIDTH: u32 = 13;
pub const MIX_MODE: u32 = 14;
pub const VOLUME: u32 = 15;
pub const DECAY: u32 = 16;
const PARAMS: usize = 17;

pub const INTERVALS: [&str; 8] = [
    "1/32", "1/16", "1/8", "1/4", "1/2", "1 Bar", "2 Bars", "4 Bars",
];
const INTERVAL_BEATS: [f64; 8] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
pub const GRIDS: [&str; 10] = [
    "1/32", "1/24", "1/16", "1/12", "1/8", "1/6", "1/4", "1/3", "1/2", "1",
];
/// Grid lengths, in quarter notes.
const GRID_BEATS: [f64; 10] = [
    0.125,
    1.0 / 6.0,
    0.25,
    1.0 / 3.0,
    0.5,
    2.0 / 3.0,
    1.0,
    4.0 / 3.0,
    2.0,
    4.0,
];
/// Which grids stay when triplets are off: every second entry.
const BINARY: [bool; 10] = [
    true, false, true, false, true, false, true, false, true, true,
];
const VARIATION_MODES: [&str; 5] = ["Trigger", "1/4", "1/8", "1/16", "Auto"];
const MIX_MODES: [&str; 3] = ["Mix", "Insert", "Gate"];

const MODE_MIX: usize = 0;
const MODE_GATE: usize = 2;

/// The longest slice the buffer holds, in seconds: one bar at 20 BPM.
const MAX_SECONDS: f64 = 13.0;
/// How long the fades either end of a repetition last, in samples.
pub const FADE_FRAMES: f64 = 128.0;
/// How many semitones each repetition falls at full Pitch Decay. The manual has no number.
pub const PITCH_DECAY_SEMITONES: f64 = 2.0;
/// Grid steps either way at full Variation. The manual has no number.
const VARIATION_STEPS: f64 = 5.0;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(INTERVAL, "Interval", &INTERVALS, 5),
        ParamInfo::continuous(OFFSET, "Offset", 0.0, 16.0, 0.0, "16th"),
        ParamInfo::continuous(CHANCE, "Chance", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(GATE, "Gate", 1.0, 18.0, 4.0, "16th"),
        ParamInfo::stepped(GRID, "Grid", &GRIDS, 2),
        ParamInfo::toggle(NO_TRIPLETS, "No Triplets", false),
        ParamInfo::continuous(VARIATION, "Variation", 0.0, 10.0, 0.0, ""),
        ParamInfo::stepped(VARIATION_MODE, "Variation Mode", &VARIATION_MODES, 0),
        ParamInfo::toggle(REPEAT, "Repeat", false),
        ParamInfo::continuous(PITCH, "Pitch", -48.0, 0.0, 0.0, "st"),
        ParamInfo::continuous(PITCH_DECAY, "Pitch Decay", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(FILTER_ON, "Filter", false),
        ParamInfo::skewed(
            FILTER_FREQ,
            "Filter Freq",
            50.0,
            18_000.0,
            1_000.0,
            "Hz",
            3.0,
        ),
        ParamInfo::continuous(FILTER_WIDTH, "Filter Width", 0.5, 9.0, 9.0, "oct"),
        ParamInfo::stepped(MIX_MODE, "Mix Mode", &MIX_MODES, 1),
        ParamInfo::continuous(VOLUME, "Volume", -60.0, 6.0, 0.0, "dB"),
        ParamInfo::continuous(DECAY, "Decay", 0.0, 100.0, 0.0, "%"),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Timing",
            items: vec![
                menu(INTERVAL),
                knob(OFFSET),
                knob(CHANCE),
                knob(GATE),
                toggle(REPEAT),
            ],
        },
        Section {
            title: "Grid",
            items: vec![
                menu(GRID),
                toggle(NO_TRIPLETS),
                knob(VARIATION),
                menu(VARIATION_MODE),
                knob(PITCH),
                knob(PITCH_DECAY),
            ],
        },
        Section {
            title: "Output",
            items: vec![
                Item::Row(vec![
                    toggle(FILTER_ON),
                    knob(FILTER_FREQ),
                    knob(FILTER_WIDTH),
                ]),
                Item::Row(vec![menu(MIX_MODE), knob(VOLUME), knob(DECAY)]),
            ],
        },
    ]
}

/// What's playing right now.
#[derive(Clone, Copy, Debug, Default)]
struct Repetition {
    active: bool,
    /// Where the slice is being read from, in frames.
    read: f64,
    /// How far the capture has got, in frames.
    captured: usize,
    /// The slice's length, in frames.
    slice: f64,
    /// Which repetition this is, from 0.
    index: u32,
    /// Frames of gate left, or infinite while Repeat is held.
    remaining: f64,
}

pub struct BeatRepeat {
    v: [f64; PARAMS],
    sr: f32,
    capture: [Vec<f32>; 2],
    rep: Repetition,
    rng: Rng,
    band: [LinkedBand; 2],
    band_dirty: bool,
    repeat_held: bool,
    /// Beats per sample of the block being rendered, and the position in beats so far.
    clock: (f64, f64),
    bps: f64,
    playing: bool,
}

impl BeatRepeat {
    /// A device whose Chance and Variation follow this seed.
    pub fn with_seed(seed: u32) -> Self {
        Self {
            v: values(&params()),
            sr: 48_000.0,
            capture: [Vec::new(), Vec::new()],
            rep: Repetition::default(),
            rng: Rng::new(seed),
            band: [LinkedBand::default(); 2],
            band_dirty: true,
            repeat_held: false,
            clock: (0.0, 0.0),
            bps: 0.0,
            playing: false,
        }
    }

    fn value(&self, id: u32) -> f64 {
        self.v[id as usize]
    }

    fn flag(&self, id: u32) -> bool {
        self.value(id) >= 0.5
    }

    fn frames_per_beat(&self, transport: &TransportInfo) -> f64 {
        self.sr as f64 * 60.0 / transport.tempo.max(1.0)
    }

    fn grid_index(&mut self) -> usize {
        let base = self.value(GRID).round().clamp(0.0, 9.0) as i32;
        let variation = self.value(VARIATION) * 0.1 * VARIATION_STEPS;
        let wobble = if variation > 0.0 {
            (self.rng.bipolar() as f64 * variation).round() as i32
        } else {
            0
        };
        let mut i = (base + wobble).clamp(0, 9) as usize;
        if self.flag(NO_TRIPLETS) && !BINARY[i] {
            // Round down to the next shorter binary grid.
            i -= 1;
        }
        i
    }

    /// Picks a new slice length, within what the capture holds so far.
    fn new_grid(&mut self, frames_per_beat: f64) {
        let index = self.grid_index();
        let frames = GRID_BEATS[index] * frames_per_beat;
        self.rep.slice = frames
            .min(self.capture[0].len() as f64 - 1.0)
            .min(self.rep.captured.max(1) as f64)
            .max(2.0);
    }

    fn trigger(&mut self, frames_per_beat: f64, remaining: f64) {
        self.rep = Repetition {
            active: true,
            read: 0.0,
            captured: 0,
            slice: GRID_BEATS[self.grid_index()] * frames_per_beat,
            index: 0,
            remaining,
        };
        self.rep.slice = self
            .rep
            .slice
            .min(self.capture[0].len() as f64 - 1.0)
            .max(2.0);
    }

    fn gate_frames(&self, frames_per_beat: f64) -> f64 {
        self.value(GATE) * 0.25 * frames_per_beat
    }

    /// The read rate of repetition `index`, from Pitch and Pitch Decay.
    fn rate(&self, index: u32) -> f64 {
        let semitones = self.value(PITCH)
            - self.value(PITCH_DECAY) * 0.01 * PITCH_DECAY_SEMITONES * index as f64;
        2f64.powf(semitones.max(-96.0) / 12.0)
    }

    fn reading_gain(&self) -> f64 {
        let decay = (1.0 - self.value(DECAY) * 0.01).clamp(0.0, 1.0);
        decay.powi(self.rep.index as i32)
    }

    #[inline]
    fn read(buffer: &[f32], position: f64) -> f32 {
        let last = buffer.len() - 1;
        let i = (position as usize).min(last);
        let t = (position - i as f64) as f32;
        let a = buffer[i];
        let b = buffer[(i + 1).min(last)];
        a + (b - a) * t
    }

    #[inline]
    fn envelope(&self) -> f32 {
        let r = &self.rep;
        // The first pass sits on top of the live signal, so it doesn't fade in.
        let attack = if r.index == 0 {
            1.0
        } else {
            r.read / FADE_FRAMES
        };
        let tail = (r.slice - r.read) / FADE_FRAMES;
        let end = r.remaining / FADE_FRAMES;
        attack.min(tail).min(end).clamp(0.0, 1.0) as f32
    }

    fn render(&mut self, audio: AudioBuffer, transport: &TransportInfo) {
        if self.band_dirty {
            let freq = self.value(FILTER_FREQ) as f32;
            let width = self.value(FILTER_WIDTH) as f32;
            for b in self.band.iter_mut() {
                b.set(freq, width, self.sr);
            }
            self.band_dirty = false;
        }
        let interval = INTERVAL_BEATS[(self.value(INTERVAL).round() as usize).min(7)];
        let offset = self.value(OFFSET) * 0.25;
        let chance = (self.value(CHANCE) * 0.01) as f32;
        let fpb = self.frames_per_beat(transport);
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        let mode = (self.value(MIX_MODE).round() as usize).min(MODE_GATE);
        let filter_on = self.flag(FILTER_ON);
        let volume = {
            let db = self.value(VOLUME);
            if db <= -60.0 {
                0.0
            } else {
                db_to_gain(db as f32)
            }
        };
        let repeat = self.flag(REPEAT);
        if repeat != self.repeat_held {
            self.repeat_held = repeat;
            if repeat {
                self.trigger(fpb, f64::INFINITY);
            } else {
                // Let go with a short fade.
                self.rep.remaining = self.rep.remaining.min(FADE_FRAMES);
            }
        }
        let var_mode = self.value(VARIATION_MODE).round() as i32;
        let var_period = match var_mode {
            1 => Some(1.0),
            2 => Some(0.5),
            3 => Some(0.25),
            _ => None,
        };
        let capacity = self.capture[0].len();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (beat, per_sample) = (self.clock.0 + self.clock.1 * self.bps, self.bps);
            self.clock.1 += 1.0;
            if self.playing && !repeat {
                let pos = beat - offset;
                let previous = pos - per_sample;
                let a = (previous / interval + 1e-9).floor();
                let b = (pos / interval + 1e-9).floor();
                if a != b && self.rng.unipolar() < chance {
                    let gate = self.gate_frames(fpb);
                    self.trigger(fpb, gate);
                }
            }
            let (dry_l, dry_r) = (*l, *r);
            if let Some(period) = var_period {
                if self.rep.active && self.playing {
                    let a = ((beat - per_sample) / period).floor();
                    let b = (beat / period).floor();
                    if a != b {
                        self.new_grid(fpb);
                    }
                }
            }
            let mut wet = None;
            if self.rep.active {
                if self.rep.captured < capacity {
                    self.capture[0][self.rep.captured] = sanitize(dry_l);
                    self.capture[1][self.rep.captured] = sanitize(dry_r);
                    self.rep.captured += 1;
                }
                let g = self.envelope() * self.reading_gain() as f32;
                let mut a = Self::read(&self.capture[0], self.rep.read) * g;
                let mut b = Self::read(&self.capture[1], self.rep.read) * g;
                if filter_on {
                    a = self.band[0].process(a);
                    b = self.band[1].process(b);
                }
                wet = Some((a, b));
                self.rep.read += self.rate(self.rep.index);
                self.rep.remaining -= 1.0;
                if self.rep.read >= self.rep.slice {
                    self.rep.index += 1;
                    self.rep.read = 0.0;
                    if var_mode == 4 {
                        self.new_grid(fpb);
                    }
                }
                if self.rep.remaining <= 0.0 {
                    self.rep.active = false;
                }
            }
            let (out_l, out_r) = match (mode, wet) {
                (MODE_MIX, Some((a, b))) => (dry_l + a, dry_r + b),
                (_, Some((a, b))) => (a, b),
                (MODE_GATE, None) => (0.0, 0.0),
                _ => (dry_l, dry_r),
            };
            *l = out_l * volume;
            *r = out_r * volume;
        }
    }
}

impl Default for BeatRepeat {
    fn default() -> Self {
        Self::with_seed(0xB5)
    }
}

impl Processor for BeatRepeat {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let n = (sample_rate * MAX_SECONDS) as usize;
        self.capture = [vec![0.0; n], vec![0.0; n]];
        self.band_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let transport = *ctx.transport;
        let beat = transport.beats();
        self.playing = transport.playing;
        self.bps = transport.tempo / 60.0 / transport.sample_rate.max(1.0);
        self.clock = (beat, 0.0);
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio, &transport),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == FILTER_FREQ || id == FILTER_WIDTH {
            self.band_dirty = true;
        }
    }

    fn reset(&mut self) {
        self.rep = Repetition::default();
        self.band.iter_mut().for_each(LinkedBand::reset);
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.beat_repeat",
    name: "Beat Repeat",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<BeatRepeat>::default(),
};
