//! Drum Sampler (manual pp. 730-735): a one-shot sample player with an AHD envelope, nine
//! playback effects on an XY pad, a filter and one modulation route.
//!
//! The manual gives these numbers: Gain -70..24 dB, Volume -36..36 dB, Transpose ±48 st, Detune
//! ±50 ct, Sub Osc 30..120 Hz, Pitch Env amount ±100 %. Everything else (grain sizes, the
//! octave span behind each XY axis, pitch-envelope depth) is ours.

use super::common::*;
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::util::{db_to_gain, Rng};
use crate::dsp::voice::{StealMode, VoiceAllocator, VoiceEvent, VoiceSettings};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceInit, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, ExprKind, Item, MidiMessage, ParamId, ParamInfo, ProcessContext,
    Processor, Resource, SampleBuffer, SampleSlot, Section, Status, Tail, ViewId,
};
use std::f32::consts::TAU;
use std::sync::Arc;

pub const START: u32 = 0;
pub const LENGTH: u32 = 1;
pub const GAIN: u32 = 2;
pub const ATTACK: u32 = 3;
pub const HOLD: u32 = 4;
pub const DECAY: u32 = 5;
pub const TRANSPOSE: u32 = 6;
pub const DETUNE: u32 = 7;
pub const ENV_MODE: u32 = 8;
pub const FX_ON: u32 = 9;
pub const FX_TYPE: u32 = 10;
pub const FX1: u32 = 11;
pub const FX2: u32 = 12;
pub const F_ON: u32 = 13;
pub const F_TYPE: u32 = 14;
pub const F_FREQ: u32 = 15;
pub const F_RES: u32 = 16;
pub const F_GAIN: u32 = 17;
pub const VOLUME: u32 = 18;
pub const PAN: u32 = 19;
pub const VEL_VOL: u32 = 20;
pub const MOD_SOURCE: u32 = 21;
pub const MOD_DEST: u32 = 22;
pub const MOD_AMOUNT: u32 = 23;
pub const NOTE_PB: u32 = 24;
pub const ENV_FOLLOWS: u32 = 25;
pub const PARAM_COUNT: usize = 26;

/// The value of Hold that means forever.
pub const HOLD_INF: f64 = 10_000.0;

pub const FX_STRETCH: usize = 0;
pub const FX_LOOP: usize = 1;
pub const FX_PITCH: usize = 2;
pub const FX_PUNCH: usize = 3;
pub const FX_8BIT: usize = 4;
pub const FX_FM: usize = 5;
pub const FX_RING: usize = 6;
pub const FX_SUB: usize = 7;
pub const FX_NOISE: usize = 8;

pub const FX_NAMES: [&str; 9] = [
    "Stretch",
    "Loop",
    "Pitch Env",
    "Punch",
    "8-Bit",
    "FM",
    "Ring Mod",
    "Sub Osc",
    "Noise",
];

/// `(name of parameter one, name of parameter two)` of each effect.
pub const FX_PARAMS: [(&str, &str); 9] = [
    ("Factor", "Grain Size"),
    ("Loop Offset", "Loop Length"),
    ("Pitch Env Amount", "Pitch Env Decay"),
    ("Punch Amount", "Punch Release"),
    ("Sample Rate", "Decay Time"),
    ("FM Amount", "FM Frequency"),
    ("Ring Mod Amount", "Ring Mod Frequency"),
    ("Sub Amount", "Sub Frequency"),
    ("Noise Amount", "Noise Color"),
];

const F_TYPES: [&str; 4] = ["12 dB LP", "24 dB LP", "24 dB HP", "Peak"];

pub fn params() -> Vec<ParamInfo> {
    let pct = |id, name, default: f64| ParamInfo::continuous(id, name, 0.0, 100.0, default, "%");
    let ms = |id, name: &str, max: f64, default: f64| {
        ParamInfo::skewed(id, name, 0.0, max, default, "ms", 3.0)
    };
    vec![
        pct(START, "Sample Start", 0.0),
        pct(LENGTH, "Sample Length", 100.0),
        ParamInfo::continuous(GAIN, "Sample Gain", -70.0, 24.0, 0.0, "dB"),
        ms(ATTACK, "Attack", 2_000.0, 0.1),
        ms(HOLD, "Hold", HOLD_INF, HOLD_INF),
        ms(DECAY, "Decay", 10_000.0, 100.0),
        ParamInfo::continuous(TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(DETUNE, "Detune", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::stepped(ENV_MODE, "Envelope Mode", &["Trigger", "Gate"], 0),
        ParamInfo::toggle(FX_ON, "Playback Effect", false),
        ParamInfo::stepped(FX_TYPE, "Playback Effect Type", &FX_NAMES, 0),
        ParamInfo::continuous(FX1, "Effect 1", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(FX2, "Effect 2", 0.0, 1.0, 0.5, ""),
        ParamInfo::toggle(F_ON, "Filter", false),
        ParamInfo::stepped(F_TYPE, "Filter Type", &F_TYPES, 0),
        ParamInfo::skewed(
            F_FREQ,
            "Filter Frequency",
            20.0,
            20_000.0,
            5_000.0,
            "Hz",
            2.5,
        ),
        ParamInfo::continuous(F_RES, "Resonance", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(F_GAIN, "Peak Filter Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(VOLUME, "Volume", -36.0, 36.0, 0.0, "dB"),
        ParamInfo::continuous(PAN, "Pan", -1.0, 1.0, 0.0, ""),
        pct(VEL_VOL, "Velocity to Volume", 50.0),
        ParamInfo::stepped(MOD_SOURCE, "Modulation Source", &["Velocity", "Slide"], 0),
        ParamInfo::stepped(
            MOD_DEST,
            "Modulation Destination",
            &["Filter", "Attack", "Hold", "Decay", "FX1", "FX2"],
            0,
        ),
        ParamInfo::continuous(MOD_AMOUNT, "Modulation Amount", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(NOTE_PB, "Per-Note Pitch Bend", true),
        ParamInfo::toggle(ENV_FOLLOWS, "Envelope Follows Pitch", false),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let menu = |id| Item::Menu(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Sample",
            items: vec![
                Item::Panel("drum_sampler_wave"),
                Item::Row(vec![knob(START), knob(LENGTH), knob(GAIN)]),
                Item::Row(vec![knob(ATTACK), knob(HOLD), knob(DECAY), menu(ENV_MODE)]),
                Item::Row(vec![knob(TRANSPOSE), knob(DETUNE)]),
            ],
        },
        Section {
            title: "Playback Effect",
            items: vec![
                Item::Panel("drum_sampler_xy"),
                Item::Row(vec![toggle(FX_ON), menu(FX_TYPE)]),
                Item::Row(vec![knob(FX1), knob(FX2)]),
            ],
        },
        Section {
            title: "Filter",
            items: vec![
                Item::Row(vec![toggle(F_ON), menu(F_TYPE)]),
                Item::Row(vec![knob(F_FREQ), knob(F_RES), knob(F_GAIN)]),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                Item::Row(vec![knob(VOLUME), knob(PAN), knob(VEL_VOL)]),
                Item::Row(vec![menu(MOD_SOURCE), menu(MOD_DEST), knob(MOD_AMOUNT)]),
                Item::Row(vec![toggle(NOTE_PB), toggle(ENV_FOLLOWS)]),
            ],
        },
    ]
}

pub const VIEW_WAVE: ViewId = ViewId(0);
pub const VIEW_LEN: usize = WAVE_VALUES;

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.drum_sampler",
    name: "Drum Sampler",
    role: DeviceRole::Instrument,
    category: "Samplers",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        samples: vec![SampleSlot { name: "Sample" }],
        ..Default::default()
    },
    create: |init| Box::new(DrumSampler::new(init)),
};

const VOICES: usize = 16;

/// Attack, hold and decay at sample resolution. Times are fixed when a note starts.
#[derive(Clone, Copy, Default, PartialEq)]
enum Stage {
    #[default]
    Idle,
    Attack,
    Hold,
    Decay,
}

#[derive(Clone, Copy, Default)]
struct Ahd {
    stage: Stage,
    level: f32,
    attack_step: f32,
    hold_left: f32,
    infinite: bool,
    decay_step: f32,
}

impl Ahd {
    fn start(&mut self, attack: f32, hold: f32, decay: f32, infinite: bool) {
        self.stage = Stage::Attack;
        self.level = 0.0;
        self.attack_step = if attack > 0.0 { 1.0 / attack } else { 1.0 };
        self.hold_left = hold;
        self.infinite = infinite;
        self.decay_step = if decay > 0.0 { 1.0 / decay } else { 1.0 };
    }

    #[inline]
    fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Hold;
                }
            }
            Stage::Hold => {
                if !self.infinite {
                    self.hold_left -= 1.0;
                    if self.hold_left <= 0.0 {
                        self.stage = Stage::Decay;
                    }
                }
            }
            Stage::Decay => {
                self.level -= self.decay_step;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }

    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Decay;
        }
    }

    fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }
}

/// Plays one source through two overlapping Hann-windowed grains: the Stretch effect, also
/// used by Impulse. Two grains half a cycle apart sum to unity gain.
#[derive(Clone, Copy, Default)]
pub struct GrainStretch {
    grain: f32,
    phase: [f32; 2],
    base: [f64; 2],
    pub src: f64,
}

impl GrainStretch {
    pub fn start(&mut self, at: f64) {
        self.phase = [0.0, 0.5];
        self.base = [at, at];
        self.src = at;
    }

    /// One output frame. `grain_frames` is the grain length, `read_step` the pitch rate and
    /// `advance` how far the source moves per output frame.
    #[inline]
    pub fn next(
        &mut self,
        sample: &SampleBuffer,
        grain_frames: f32,
        read_step: f64,
        advance: f64,
    ) -> [f32; 2] {
        self.grain = grain_frames.max(16.0);
        self.src += advance;
        let mut out = [0.0f32; 2];
        for g in 0..2 {
            if self.phase[g] >= 1.0 {
                self.phase[g] -= 1.0;
                self.base[g] = self.src;
            }
            let t = self.phase[g];
            let at = self.base[g] + (t * self.grain) as f64 * read_step;
            let w = (std::f32::consts::PI * t).sin();
            let w = w * w;
            let s = read(sample, at, Interp::Normal);
            out[0] += s[0] * w;
            out[1] += s[1] * w;
            self.phase[g] += 1.0 / self.grain;
        }
        out
    }
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    gated: bool,
    released: bool,
    key: f32,
    velocity: f32,
    region_end: f64,
    pos: f64,
    player: Player,
    amp: Ahd,
    stretch: GrainStretch,
    ctl: usize,
    filter: [Filter; 2],
    peak: [Biquad; 2],
    pitch_env: f32,
    punch: f32,
    fm_phase: f32,
    ring_phase: f32,
    sub_phase: f32,
    noise_state: f32,
    rng: Rng,
    crush_counter: f32,
    crush_hold: [f32; 2],
    crush_lp: [f32; 2],
    lp_env: f32,
    gain: f32,
    frames: u64,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            active: false,
            gated: false,
            released: false,
            key: 60.0,
            velocity: 0.0,
            region_end: 0.0,
            pos: 0.0,
            player: Player::default(),
            amp: Ahd::default(),
            stretch: GrainStretch::default(),
            ctl: 0,
            filter: [Filter::new(); 2],
            peak: [Biquad::default(); 2],
            pitch_env: 0.0,
            punch: 0.0,
            fm_phase: 0.0,
            ring_phase: 0.0,
            sub_phase: 0.0,
            noise_state: 0.0,
            rng: Rng::new(0x5d),
            crush_counter: 0.0,
            crush_hold: [0.0; 2],
            crush_lp: [0.0; 2],
            lp_env: 1.0,
            gain: 1.0,
            frames: 0,
        }
    }
}

/// See the [module docs](self).
pub struct DrumSampler {
    values: Vec<f64>,
    sr: f64,
    sample: Option<Arc<SampleBuffer>>,
    analysis: Option<Arc<Analysis>>,
    alloc: VoiceAllocator,
    voices: Box<[Voice]>,
}

/// One effect parameter mapped from its axis (0..1) to `low..high` on a log scale.
#[inline]
fn expo(x: f32, low: f32, high: f32) -> f32 {
    low * (high / low).powf(x.clamp(0.0, 1.0))
}

impl DrumSampler {
    pub fn new(init: &DeviceInit) -> Self {
        let sample = init.samples.first().cloned().flatten();
        let analysis = sample
            .as_ref()
            .map(|s| Arc::new(Analysis::new(s, Some(&[]))));
        Self {
            values: params().iter().map(|p| p.default).collect(),
            sr: 48_000.0,
            sample,
            analysis,
            alloc: VoiceAllocator::new(VOICES),
            voices: vec![Voice::default(); VOICES].into_boxed_slice(),
        }
    }

    fn v(&self, id: u32) -> f64 {
        self.values[id as usize]
    }

    fn voice_settings() -> VoiceSettings {
        VoiceSettings {
            polyphony: VOICES,
            steal: StealMode::Oldest,
            retrigger: true,
            bend_range: 2.0,
            ..Default::default()
        }
    }

    fn start_voice(&mut self, index: usize, key: f32, velocity: f32) {
        let Some(sample) = self.sample.as_deref() else {
            return;
        };
        let n = sample.len() as f64;
        let start = n * (self.v(START) * 0.01).clamp(0.0, 1.0);
        let end = (start + n * (self.v(LENGTH) * 0.01).clamp(0.0, 1.0)).min(n);
        let fx_on = self.v(FX_ON) >= 0.5;
        let fx = self.v(FX_TYPE).round() as usize;
        let mut player = Player::new(start, end.max(start + 1.0), false);
        let rate = sample.sample_rate / self.sr;
        let ratio =
            2f64.powf((key as f64 - 60.0 + self.v(TRANSPOSE) + self.v(DETUNE) * 0.01) / 12.0);
        let sr = self.sr as f32;
        let mod_amount = (self.v(MOD_AMOUNT) * 0.01) as f32;
        let dest = self.v(MOD_DEST).round() as i32;
        let source = if self.v(MOD_SOURCE).round() as i32 == 1 {
            0.0 // Slide starts at zero; it matters from the next control tick.
        } else {
            velocity
        };
        let depth = 1.0 + mod_amount * source;
        let mut times = [
            self.v(ATTACK) as f32 * 0.001 * sr,
            self.v(HOLD) as f32 * 0.001 * sr,
            self.v(DECAY) as f32 * 0.001 * sr,
        ];
        if (1..=3).contains(&dest) {
            times[dest as usize - 1] *= depth.max(0.0);
        }
        if self.v(ENV_FOLLOWS) >= 0.5 {
            let scale = (1.0 / ratio) as f32;
            times = times.map(|t| t * scale);
        }
        let mut infinite = self.v(HOLD) >= HOLD_INF;
        if fx_on && fx == FX_LOOP {
            let offset = (self.v(FX1).clamp(0.0, 1.0) * (end - start)).min(end - start);
            let length =
                expo(self.v(FX2) as f32, 2.0, 2_000.0) as f64 * 0.001 * self.sr * ratio * rate;
            let from = start + offset;
            player.set_sustain(
                (from, (from + length).min(end)),
                LoopKind::Forward,
                0.0,
                true,
                1.0,
            );
            if infinite {
                infinite = false;
                let seconds = (end - start) / sample.sample_rate;
                times[1] = (seconds * self.sr / (ratio * rate).max(1e-6)) as f32;
            }
        }
        let v = &mut self.voices[index];
        v.active = true;
        v.gated = true;
        v.released = false;
        v.key = key;
        v.velocity = velocity;
        v.region_end = end;
        v.pos = start;
        v.player = player;
        let gate_mode = self.values[ENV_MODE as usize] >= 0.5;
        let hold = if gate_mode { 0.0 } else { times[1] };
        v.amp.start(times[0], hold, times[2], infinite || gate_mode);
        v.stretch.start(start);
        v.ctl = 0;
        for f in &mut v.filter {
            f.reset();
        }
        v.peak = [Biquad::default(); 2];
        v.pitch_env = 1.0;
        v.punch = 1.0;
        v.fm_phase = 0.0;
        v.ring_phase = 0.0;
        v.sub_phase = 0.0;
        v.crush_counter = 0.0;
        v.crush_hold = [0.0; 2];
        v.crush_lp = [0.0; 2];
        v.lp_env = 1.0;
        v.frames = 0;
        let sense = (self.values[VEL_VOL as usize] * 0.01) as f32;
        v.gain = 1.0 - sense + sense * velocity;
    }

    fn release_voice(&mut self, index: usize) {
        if !self.voices[index].active {
            // Ran out before the note ended.
            self.alloc.finished(index);
            return;
        }
        let gate_mode = self.v(ENV_MODE) >= 0.5;
        let v = &mut self.voices[index];
        v.gated = false;
        if gate_mode && v.active {
            v.released = true;
            v.amp.release();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let Some(sample) = self.sample.clone() else {
            return;
        };
        if sample.len() < 2 {
            return;
        }
        let frames = left.len();
        self.alloc.advance(frames);
        let (sr, values) = (self.sr as f32, &self.values);
        let get = |id: u32| values[id as usize];
        let volume = db_to_gain(get(VOLUME) as f32) * db_to_gain(get(GAIN) as f32);
        let pan = get(PAN) as f32;
        let (pl, pr) = ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0));
        let fx_on = get(FX_ON) >= 0.5;
        let fx = get(FX_TYPE).round() as usize;
        let filter_on = get(F_ON) >= 0.5;
        let filter_type = get(F_TYPE).round() as i32;
        let (cutoff, res, peak_gain) = (get(F_FREQ) as f32, get(F_RES) as f32, get(F_GAIN) as f32);
        let mod_amount = (get(MOD_AMOUNT) * 0.01) as f32;
        let dest = get(MOD_DEST).round() as i32;
        let use_slide = get(MOD_SOURCE).round() as i32 == 1;
        let base_pitch = get(TRANSPOSE) as f32 + get(DETUNE) as f32 * 0.01;
        let rate = sample.sample_rate / self.sr;

        for index in 0..self.voices.len() {
            let v = &mut self.voices[index];
            if !v.active {
                continue;
            }
            let source = if use_slide {
                self.alloc.voice(index).slide
            } else {
                v.velocity
            };
            let mod_for = |target: i32| {
                if dest == target {
                    mod_amount * source
                } else {
                    0.0
                }
            };
            let fx1 = (get(FX1) as f32 + mod_for(4)).clamp(0.0, 1.0);
            let fx2 = (get(FX2) as f32 + mod_for(5)).clamp(0.0, 1.0);
            let pitch = self.alloc.pitch(index) + base_pitch;
            let ratio = 2f32.powf((pitch - 60.0) / 12.0);
            let (fx_pitch_env, fx_pe_step) = if fx_on && fx == FX_PITCH {
                let amount = (fx1 * 2.0 - 1.0) * 24.0;
                let decay = expo(fx2, 5.0, 2_000.0) * 0.001 * sr;
                (amount, (1.0 / decay).min(1.0))
            } else {
                (0.0, 0.0)
            };
            let cut_mod = 2f32.powf(mod_for(0) * 48.0 / 12.0);
            if filter_on && v.ctl == 0 {
                let fc = (cutoff * cut_mod).clamp(20.0, 20_000.0);
                match filter_type {
                    3 => {
                        for b in &mut v.peak {
                            b.set(BiquadKind::Peak, fc, 0.7 + res * 8.0, peak_gain, sr);
                        }
                    }
                    t => {
                        let settings = FilterSettings {
                            circuit: Circuit::Svf,
                            kind: if t == 2 {
                                FilterKind::HighPass
                            } else {
                                FilterKind::LowPass
                            },
                            slope: if t == 0 {
                                FilterSlope::Db12
                            } else {
                                FilterSlope::Db24
                            },
                            cutoff: fc,
                            resonance: res,
                            drive: 0.0,
                            morph: 0.0,
                        };
                        for f in &mut v.filter {
                            f.set(&settings);
                        }
                    }
                }
            }
            for frame in 0..frames {
                let semis = fx_pitch_env * v.pitch_env.max(0.0);
                let mut fm = 0.0f32;
                if fx_on && fx == FX_FM {
                    let depth = fx1 * v.amp.level.min(1.0);
                    let hz = expo(fx2, 1.0, 5_000.0) * ratio;
                    v.fm_phase = (v.fm_phase + hz / sr).fract();
                    fm = (v.fm_phase * TAU).sin() * depth * 12.0;
                }
                let step = 2f64.powf(((pitch - 60.0 + semis + fm) / 12.0) as f64) * rate;
                let [mut l, mut r];
                let mut alive;
                if fx_on && fx == FX_STRETCH {
                    let factor = 2f64.powf((fx1 as f64 * 4.0) - 2.0);
                    let grain = expo(fx2, 5.0, 200.0) * 0.001 * sr / ratio;
                    let advance = step / factor;
                    [l, r] = v.stretch.next(&sample, grain, step, advance);
                    alive = v.stretch.src < v.region_end;
                } else {
                    [l, r] = v.player.sample(&sample, Interp::Good, false);
                    alive = v.player.advance(step, false);
                }
                let amp = v.amp.next();
                if !v.amp.is_active() {
                    alive = false;
                }
                if fx_on {
                    match fx {
                        FX_PITCH => v.pitch_env = (v.pitch_env - fx_pe_step).max(0.0),
                        FX_PUNCH => {
                            let release = expo(fx2, 5.0, 400.0) * 0.001 * sr;
                            let duck = 1.0 - fx1 * v.punch;
                            l *= duck;
                            r *= duck;
                            v.punch = (v.punch - 1.0 / release).max(0.0);
                        }
                        FX_8BIT => {
                            let hz = expo(fx1, 500.0, 22_050.0);
                            let hold = (sr / hz).max(1.0);
                            v.crush_counter += 1.0;
                            if v.crush_counter >= hold {
                                v.crush_counter -= hold;
                                v.crush_hold =
                                    [(l * 127.0).round() / 127.0, (r * 127.0).round() / 127.0];
                            }
                            let decay_frames = expo(fx2, 5.0, 2_000.0) * 0.001 * sr;
                            v.lp_env = (v.lp_env - 1.0 / decay_frames).max(0.0);
                            let coeff = 0.02 + 0.5 * v.lp_env;
                            v.crush_lp[0] += coeff * (v.crush_hold[0] - v.crush_lp[0]);
                            v.crush_lp[1] += coeff * (v.crush_hold[1] - v.crush_lp[1]);
                            l = v.crush_lp[0];
                            r = v.crush_lp[1];
                        }
                        FX_RING => {
                            let hz = expo(fx2, 1.0, 8_000.0) * ratio;
                            let depth = fx1 * v.amp.level;
                            v.ring_phase = (v.ring_phase + hz / sr).fract();
                            let m = 1.0 - depth + depth * (v.ring_phase * TAU).sin();
                            l *= m;
                            r *= m;
                        }
                        FX_SUB => {
                            let hz = (30.0 + fx2 * 90.0) * ratio;
                            v.sub_phase = (v.sub_phase + hz / sr).fract();
                            let sub = (v.sub_phase * TAU).sin() * fx1 * amp;
                            l += sub;
                            r += sub;
                        }
                        FX_NOISE => {
                            let cutoff = expo(fx2, 100.0, 18_000.0) * ratio.min(4.0);
                            let coeff = (cutoff / sr).clamp(0.0, 1.0);
                            let white = v.rng.bipolar();
                            v.noise_state += coeff * (white - v.noise_state);
                            let noise = v.noise_state * fx1 * amp;
                            l += noise;
                            r += noise;
                        }
                        _ => {}
                    }
                }
                if filter_on {
                    if filter_type == 3 {
                        l = v.peak[0].process(l);
                        r = v.peak[1].process(r);
                    } else {
                        l = v.filter[0].process(l);
                        r = v.filter[1].process(r);
                    }
                }
                let gain = amp * v.gain * volume;
                left[frame] += l * gain * pl;
                right[frame] += r * gain * pr;
                v.frames += 1;
                if !alive {
                    v.active = false;
                    self.alloc.finished(index);
                    break;
                }
            }
            v.ctl = (v.ctl + frames) % 32;
        }
    }
}

impl Processor for DrumSampler {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        self.alloc.prepare(sample_rate);
        self.alloc.set(&Self::voice_settings());
        for v in self.voices.iter_mut() {
            for f in &mut v.filter {
                f.prepare(sample_rate);
            }
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                if let Some(v) = self.values.get_mut(id as usize) {
                    *v = value;
                }
            }
            BlockStep::Audio(audio) => self.render(audio.left, audio.right),
            BlockStep::Midi(message) => {
                if let MidiMessage::NoteExpr {
                    kind: ExprKind::Pitch,
                    ..
                } = message
                {
                    if self.values[NOTE_PB as usize] < 0.5 {
                        return;
                    }
                }
                let mut events = [None; 8];
                let mut n = 0;
                self.alloc.handle(message, |event| {
                    if n < events.len() {
                        events[n] = Some(event);
                        n += 1;
                    }
                });
                for event in events.into_iter().flatten() {
                    match event {
                        VoiceEvent::Start { voice, note, .. } => {
                            self.start_voice(voice, note.key as f32, note.velocity)
                        }
                        VoiceEvent::Legato { .. } => {}
                        VoiceEvent::Release { voice } => self.release_voice(voice),
                        VoiceEvent::Kill { voice } => self.voices[voice].active = false,
                    }
                }
            }
        });
        if ctx.view.is_active() {
            if let Some(a) = &self.analysis {
                ctx.view.publish(VIEW_WAVE, &a.overview);
            }
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        self.alloc.reset();
        for v in self.voices.iter_mut() {
            v.active = false;
        }
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot == ANALYSIS_SLOTS {
            return match resource.downcast::<Analysis>() {
                Ok(a) => self.analysis.replace(a).map(|o| o as Resource),
                Err(refused) => Some(refused),
            };
        }
        if slot != 0 {
            return Some(resource);
        }
        match resource.downcast::<SampleBuffer>() {
            Ok(s) => self.sample.replace(s).map(|o| o as Resource),
            Err(refused) => Some(refused),
        }
    }
}
