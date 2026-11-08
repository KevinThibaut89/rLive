//! Auto Pan-Tremolo: an LFO moves the signal between the channels or changes its level.
//!
//! The controls that the manual doesn't say are shared between the two modes each exist once
//! per mode, so switching modes keeps each mode's own settings. The manual gives the Time range
//! and the 600 Hz Harmonic crossover. It doesn't give the Freq range, the nonlinearity of
//! Vintage, or the transient detector's settings, so those here are our own.

use super::common::{
    saw_down, saw_up, shark_tooth, sine, square, triangle, RandomWave, CONTROL_FRAMES,
};
use crate::devices::b3::common::Lr4;
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::tempo::BeatClock;
use crate::dsp::util::lerp;
use crate::dsp::BiquadKind;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status, TransportInfo, ViewId,
};

pub const MODE: u32 = 0;
pub const AMOUNT: u32 = 1;
pub const TIME_MODE: u32 = 2;
pub const FREQ: u32 = 3;
pub const TIME: u32 = 4;
pub const SYNC: u32 = 5;
pub const SIXTEENTHS: u32 = 6;
pub const ATTACK: u32 = 7;
pub const DYNAMIC_FREQ: u32 = 8;
/// The settings each mode has for itself: the panning block, then the tremolo block.
pub const PAN_BLOCK: u32 = 9;
pub const TREM_BLOCK: u32 = 16;
pub const HARMONIC: u32 = 23;
pub const VINTAGE: u32 = 24;
pub const PARAM_COUNT: usize = 25;

// Offsets within a mode's block.
pub const W_WAVE: u32 = 0;
pub const W_STEREO: u32 = 1;
pub const W_PHASE: u32 = 2;
pub const W_SPIN: u32 = 3;
pub const W_OFFSET: u32 = 4;
pub const W_INVERT: u32 = 5;
pub const W_SHAPE: u32 = 6;

pub const MODE_PANNING: usize = 0;
pub const MODE_TREMOLO: usize = 1;

pub const HARMONIC_CROSSOVER: f32 = 600.0;

pub const WAVE_LABELS: [&str; 9] = [
    "Sine",
    "Triangle",
    "Shark Tooth",
    "Saw Up",
    "Saw Down",
    "Square",
    "Random",
    "Wander",
    "S&H",
];

const WAVE_RANDOM: usize = 6;
const WAVE_WANDER: usize = 7;
const WAVE_SH: usize = 8;

pub const TIME_MODE_LABELS: [&str; 6] = ["Rate", "Time", "Synced", "Dotted", "Triplet", "16th"];
pub const TM_RATE: usize = 0;
pub const TM_TIME: usize = 1;
pub const TM_SYNCED: usize = 2;
pub const TM_DOTTED: usize = 3;
pub const TM_TRIPLET: usize = 4;
pub const TM_16TH: usize = 5;

/// The beat divisions of the Synced, Dotted and Triplet modes, in beats (quarter notes).
pub const SYNC_BEATS: [f64; 9] = [0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
const SYNC_LABELS: [&str; 9] = [
    "1/64", "1/32", "1/16", "1/8", "1/4", "1/2", "1 Bar", "2 Bars", "4 Bars",
];

/// The Random shape takes a new value this many times per cycle. The manual says nothing about
/// how Random, Wander and S&H differ.
pub const RANDOM_STEPS: f32 = 8.0;

/// Values for the meter item: the current left and right gain.
pub const GAINS: ViewId = ViewId(0);

fn mode_block(first: u32, phase: f64, shape_min: f64) -> Vec<ParamInfo> {
    let tag = if first == PAN_BLOCK { "Pan" } else { "Trem" };
    let n = |what: &str| format!("{tag} {what}");
    vec![
        ParamInfo::stepped(first + W_WAVE, &n("Waveform"), &WAVE_LABELS, 0),
        ParamInfo::stepped(first + W_STEREO, &n("Stereo Mode"), &["Phase", "Spin"], 0),
        ParamInfo::continuous(first + W_PHASE, &n("Phase"), 0.0, 360.0, phase, "deg"),
        ParamInfo::continuous(first + W_SPIN, &n("Spin"), 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(first + W_OFFSET, &n("Phase Offset"), 0.0, 360.0, 0.0, "deg"),
        ParamInfo::toggle(first + W_INVERT, &n("Invert"), false),
        ParamInfo::continuous(first + W_SHAPE, &n("Shape"), shape_min, 100.0, 0.0, "%"),
    ]
}

fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::stepped(MODE, "Mode", &["Panning", "Tremolo"], 0),
        ParamInfo::continuous(AMOUNT, "Amount", 0.0, 100.0, 100.0, "%"),
        ParamInfo::stepped(TIME_MODE, "Time Mode", &TIME_MODE_LABELS, TM_RATE),
        ParamInfo::skewed(FREQ, "Freq", 0.01, 50.0, 1.0, "Hz", 3.0),
        ParamInfo::skewed(TIME, "Time", 100.0, 200_000.0, 1_000.0, "ms", 3.0),
        ParamInfo::stepped(SYNC, "Rate", &SYNC_LABELS, 4),
        ParamInfo::continuous(SIXTEENTHS, "16ths", 1.0, 64.0, 4.0, ""),
        ParamInfo::skewed(ATTACK, "Attack Time", 0.0, 2_000.0, 0.0, "ms", 3.0),
        ParamInfo::continuous(DYNAMIC_FREQ, "Dynamic Freq", -100.0, 100.0, 0.0, "%"),
    ];
    p.extend(mode_block(PAN_BLOCK, 180.0, 0.0));
    p.extend(mode_block(TREM_BLOCK, 0.0, -100.0));
    p.extend([
        ParamInfo::toggle(HARMONIC, "Harmonic", false),
        ParamInfo::toggle(VINTAGE, "Vintage", false),
    ]);
    p
}

fn block_items(first: u32) -> Vec<Item> {
    vec![
        Item::Menu(ParamId(first + W_WAVE)),
        Item::Menu(ParamId(first + W_STEREO)),
        Item::Knob(ParamId(first + W_PHASE)),
        Item::Knob(ParamId(first + W_SPIN)),
        Item::Knob(ParamId(first + W_OFFSET)),
        Item::Knob(ParamId(first + W_SHAPE)),
        Item::Toggle(ParamId(first + W_INVERT)),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let mut tremolo = block_items(TREM_BLOCK);
    tremolo.push(Item::Toggle(ParamId(HARMONIC)));
    tremolo.push(Item::Toggle(ParamId(VINTAGE)));
    vec![
        Section {
            title: "Mode",
            items: vec![
                Item::Menu(ParamId(MODE)),
                Item::Menu(ParamId(TIME_MODE)),
                Item::Meter(GAINS),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                knob(AMOUNT),
                knob(FREQ),
                knob(TIME),
                Item::Menu(ParamId(SYNC)),
                knob(SIXTEENTHS),
                knob(ATTACK),
                knob(DYNAMIC_FREQ),
            ],
        },
        Section {
            title: "Panning",
            items: block_items(PAN_BLOCK),
        },
        Section {
            title: "Tremolo",
            items: tremolo,
        },
    ]
}

/// How the tremolo's Shape bends a modulator in `0..=1`. Positive values round the peaks,
/// negative values sharpen them.
pub fn tremolo_shape(m: f32, shape: f32, phase: f32) -> f32 {
    let amount = shape.abs();
    if amount == 0.0 {
        return m;
    }
    let exponent = if shape > 0.0 {
        1.0 / (1.0 + 7.0 * amount)
    } else {
        1.0 + 7.0 * amount
    };
    let curved = m.clamp(0.0, 1.0).powf(exponent);
    // The last stretch of the control turns the wave into a ramp, for gating and pumping.
    let ramp_part = ((amount - 0.75) * 4.0).max(0.0);
    if ramp_part > 0.0 {
        lerp(curved, (1.0 - phase).powf(exponent), ramp_part)
    } else {
        curved
    }
}

/// What the LFO does this block.
#[derive(Clone, Copy, Debug)]
struct Setup {
    mode: usize,
    wave: usize,
    amount: f32,
    invert: bool,
    shape: f32,
    /// How far the right channel's random values depart from the left's, 0..1.
    random_width: f32,
    dynamic: f32,
    attack_frames: f32,
    harmonic: bool,
    vintage: bool,
    settings: LfoSettings,
    /// The free-running rate, in Hz, for use when Dynamic Freq is on.
    base_hz: f32,
}

pub struct AutoPanTremolo {
    v: [f64; PARAM_COUNT],
    sr: f32,
    lfo: Lfo,
    random: [RandomWave; 2],
    setup: Option<Setup>,
    /// The gain of each channel, or of the low band in Harmonic mode, at the end of the last
    /// control block.
    gain: [f32; 2],
    high_gain: [f32; 2],
    fast: EnvelopeFollower,
    slow: EnvelopeFollower,
    level: EnvelopeFollower,
    attack_pos: f32,
    split_lp: [Lr4; 2],
    split_hp: [Lr4; 2],
    split_ready: bool,
    tempo: f64,
}

impl Default for AutoPanTremolo {
    fn default() -> Self {
        let mut v = [0.0; PARAM_COUNT];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            lfo: Lfo::new(1),
            random: [RandomWave::new(11), RandomWave::new(29)],
            setup: None,
            gain: [1.0; 2],
            high_gain: [1.0; 2],
            fast: EnvelopeFollower::new(),
            slow: EnvelopeFollower::new(),
            level: EnvelopeFollower::new(),
            attack_pos: f32::MAX,
            split_lp: [Lr4::default(); 2],
            split_hp: [Lr4::default(); 2],
            split_ready: false,
            tempo: 120.0,
        }
    }
}

impl AutoPanTremolo {
    fn setup(&self) -> Setup {
        let get = |id: u32| self.v[id as usize];
        let mode = (get(MODE).round() as usize).min(MODE_TREMOLO);
        let block = if mode == MODE_PANNING {
            PAN_BLOCK
        } else {
            TREM_BLOCK
        };
        let wave = (get(block + W_WAVE).round() as usize).min(WAVE_SH);
        let time_mode = (get(TIME_MODE).round() as usize).min(TM_16TH);
        let division = SYNC_BEATS[(get(SYNC).round() as usize).min(SYNC_BEATS.len() - 1)];
        let beats = match time_mode {
            TM_SYNCED => Some(division),
            TM_DOTTED => Some(division * 1.5),
            TM_TRIPLET => Some(division * 2.0 / 3.0),
            TM_16TH => Some(get(SIXTEENTHS).round().max(1.0) * 0.25),
            _ => None,
        };
        let tempo = self.tempo;
        let hz = match (time_mode, beats) {
            (_, Some(b)) => (tempo / 60.0 / b) as f32,
            (TM_TIME, None) => 1_000.0 / get(TIME).max(1.0) as f32,
            _ => get(FREQ).max(0.0) as f32,
        };
        let random = wave >= WAVE_RANDOM;
        let spin_mode = !random && get(block + W_STEREO).round() as usize == 1;
        let offset_deg = get(block + W_OFFSET) as f32;
        let dynamic = get(DYNAMIC_FREQ) as f32 * 0.01;
        let rate = match beats {
            Some(b) if dynamic == 0.0 => LfoRate::Beats(b),
            _ => LfoRate::Hz(hz),
        };
        let settings = LfoSettings {
            rate,
            offset: if beats.is_some() && !random {
                offset_deg / 360.0
            } else {
                0.0
            },
            phase: if spin_mode {
                0.0
            } else {
                get(block + W_PHASE) as f32 / 360.0
            },
            spin: if spin_mode {
                get(block + W_SPIN) as f32 * 0.01
            } else {
                0.0
            },
            ..Default::default()
        };
        Setup {
            mode,
            wave,
            amount: (get(AMOUNT) as f32 * 0.01).clamp(0.0, 1.0),
            invert: get(block + W_INVERT) >= 0.5,
            shape: (get(block + W_SHAPE) as f32 * 0.01).clamp(-1.0, 1.0),
            random_width: if random {
                (offset_deg / 360.0).clamp(0.0, 1.0)
            } else {
                0.0
            },
            dynamic,
            attack_frames: get(ATTACK) as f32 * 0.001 * self.sr,
            harmonic: get(HARMONIC) >= 0.5 && mode == MODE_TREMOLO,
            vintage: get(VINTAGE) >= 0.5 && mode == MODE_TREMOLO,
            settings,
            base_hz: hz,
        }
    }

    /// The LFO's bipolar value for a channel at cycle position `p`.
    fn wave(&mut self, wave: usize, ch: usize, p: f32, width: f32) -> f32 {
        let v = |random: &RandomWave| match wave {
            WAVE_WANDER => random.glide(p),
            _ => random.held(),
        };
        match wave {
            0 => sine(p),
            1 => triangle(p),
            2 => shark_tooth(p),
            3 => saw_up(p),
            4 => saw_down(p),
            5 => square(p),
            _ => {
                let cycle = if wave == WAVE_RANDOM {
                    (p * RANDOM_STEPS).fract()
                } else {
                    p
                };
                self.random[ch].update(cycle);
                let own = v(&self.random[ch]);
                if ch == 0 {
                    own
                } else {
                    lerp(v(&self.random[0]), own, width)
                }
            }
        }
    }

    fn render(&mut self, audio: AudioBuffer, clock: &mut BeatClock) {
        let setup = self.setup.unwrap_or_else(|| self.setup());
        self.setup = Some(setup);
        if setup.harmonic && !self.split_ready {
            for ch in 0..2 {
                self.split_lp[ch].set(BiquadKind::LowPass, HARMONIC_CROSSOVER, self.sr);
                self.split_hp[ch].set(BiquadKind::HighPass, HARMONIC_CROSSOVER, self.sr);
            }
            self.split_ready = true;
        }
        self.lfo.set(&setup.settings);
        let frames = audio.frames();
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES);
            if setup.dynamic != 0.0 {
                let level = self.level.level().min(1.0);
                let hz = setup.base_hz * 4f32.powf(setup.dynamic * level);
                self.lfo.set(&LfoSettings {
                    rate: LfoRate::Hz(hz),
                    ..setup.settings
                });
            }
            self.lfo.set_clock(clock);
            let phases = self.lfo.advance_phases(n);
            clock.advance(n);

            let mut target = [1.0f32; 2];
            let mut high_target = [1.0f32; 2];
            for ch in 0..2 {
                let p = phases[ch];
                let mut b = self.wave(setup.wave, ch, p, setup.random_width);
                if setup.invert {
                    b = -b;
                }
                if setup.mode == MODE_PANNING {
                    let pushed = (b * (1.0 + 8.0 * setup.shape)).clamp(-1.0, 1.0);
                    target[ch] = 1.0 - setup.amount * 0.5 * (1.0 + pushed);
                } else {
                    let m = tremolo_shape(0.5 * (b + 1.0), setup.shape, p);
                    target[ch] = 1.0 - setup.amount * (1.0 - m);
                    high_target[ch] = 1.0 - setup.amount * m;
                }
            }

            for i in pos..pos + n {
                let t = (i - pos + 1) as f32 / n as f32;
                let (l, r) = (left[i], right[i]);
                let input = l.abs().max(r.abs());
                self.level.process(input);
                let fast = self.fast.process(input);
                let slow = self.slow.process(input);
                if fast > slow * 2.0 && fast > 1e-3 {
                    self.attack_pos = 0.0;
                }
                let attack = if setup.attack_frames > 0.0 {
                    (self.attack_pos / setup.attack_frames).min(1.0)
                } else {
                    1.0
                };
                self.attack_pos = (self.attack_pos + 1.0).min(1e9);

                let mut out = [l, r];
                for ch in 0..2 {
                    let gain = lerp(self.gain[ch], target[ch], t);
                    let gain = 1.0 + (gain - 1.0) * attack;
                    if setup.harmonic {
                        let high_gain = lerp(self.high_gain[ch], high_target[ch], t);
                        let high_gain = 1.0 + (high_gain - 1.0) * attack;
                        let low = self.split_lp[ch].process(out[ch]);
                        let high = self.split_hp[ch].process(out[ch]);
                        out[ch] = low * shaped(gain, setup.vintage)
                            + high * shaped(high_gain, setup.vintage);
                    } else {
                        out[ch] *= shaped(gain, setup.vintage);
                    }
                    if setup.vintage {
                        out[ch] = (out[ch] * VINTAGE_DRIVE).tanh() / VINTAGE_DRIVE;
                    }
                }
                left[i] = out[0];
                right[i] = out[1];
            }
            self.gain = target;
            self.high_gain = high_target;
            pos += n;
        }
    }
}

/// How hard the Vintage mode's output saturates.
const VINTAGE_DRIVE: f32 = 1.5;
/// How the Vintage mode bends the amplitude.
const VINTAGE_CURVE: f32 = 1.6;

#[inline]
fn shaped(gain: f32, vintage: bool) -> f32 {
    if vintage {
        gain.max(0.0).powf(VINTAGE_CURVE)
    } else {
        gain
    }
}

impl Processor for AutoPanTremolo {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.lfo.prepare(sample_rate);
        for f in [&mut self.fast, &mut self.slow, &mut self.level] {
            f.prepare(sample_rate);
        }
        self.fast.set_times(0.000_2, 0.01);
        self.slow.set_times(0.03, 0.03);
        self.level.set_times(0.005, 0.1);
        self.split_ready = false;
        self.setup = None;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.tempo = ctx.transport.tempo;
        self.setup = None;
        let mut clock = beat_clock(ctx.transport);
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio, &mut clock),
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            ctx.view.publish(GAINS, &self.gain);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
            self.setup = None;
        }
    }

    fn reset(&mut self) {
        self.gain = [1.0; 2];
        self.high_gain = [1.0; 2];
        self.fast.reset();
        self.slow.reset();
        self.level.reset();
        self.attack_pos = f32::MAX;
        self.lfo.retrigger();
        for ch in 0..2 {
            self.split_lp[ch].reset();
            self.split_hp[ch].reset();
        }
    }
}

fn beat_clock(transport: &TransportInfo) -> BeatClock {
    BeatClock::from_transport(transport)
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.auto_pan_tremolo",
    name: "Auto Pan-Tremolo",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<AutoPanTremolo>::default(),
};
