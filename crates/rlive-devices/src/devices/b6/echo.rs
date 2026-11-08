//! Echo: two modulated delay lines with filters, a character section, and a small reverb.
//!
//! The manual gives the controls and what they do. It gives no ranges or depths. Ours are in the
//! constants below. It says the Input knob sets the gain of the dry signal, and the Gate sits at
//! Echo's input: both are applied ahead of the delay lines and the dry path, so a stronger input
//! is louder everywhere it goes. Noise and wobble are our own designs, since the manual only
//! says they simulate tape and vintage equipment. Echo switches itself off 8 seconds after the
//! input stops (the manual says "at least eight seconds"), once its repeats have died away, and
//! stays on while Noise and Gate are both engaged.

use super::common::{
    guard, knob, menu, mix_gains, toggle, values, DelayHead, Transition, Wave, WaveLfo,
    WaveSettings, CONTROL_FRAMES,
};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::filter::{Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::lfo::LfoRate;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{sanitize, Rng};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor,
    Section, Status, Tail, ViewId,
};

pub const CHANNEL_MODE: u32 = 0;
pub const LINK: u32 = 1;
pub const SYNC_L: u32 = 2;
pub const SYNC_MODE_L: u32 = 3;
pub const NOTE_L: u32 = 4;
pub const SIXTEENTH_L: u32 = 5;
pub const TIME_L: u32 = 6;
pub const OFFSET_L: u32 = 7;
pub const SYNC_R: u32 = 8;
pub const SYNC_MODE_R: u32 = 9;
pub const NOTE_R: u32 = 10;
pub const SIXTEENTH_R: u32 = 11;
pub const TIME_R: u32 = 12;
pub const OFFSET_R: u32 = 13;
pub const INPUT: u32 = 14;
pub const DRIVE: u32 = 15;
pub const FEEDBACK: u32 = 16;
pub const INVERT: u32 = 17;
pub const FILTER_ON: u32 = 18;
pub const HP_FREQ: u32 = 19;
pub const HP_RES: u32 = 20;
pub const LP_FREQ: u32 = 21;
pub const LP_RES: u32 = 22;
pub const MOD_WAVE: u32 = 23;
pub const MOD_SYNC: u32 = 24;
pub const MOD_RATE: u32 = 25;
pub const MOD_FREQ: u32 = 26;
pub const MOD_PHASE: u32 = 27;
pub const MOD_DELAY: u32 = 28;
pub const MOD_X4: u32 = 29;
pub const MOD_FILTER: u32 = 30;
pub const ENV_MIX: u32 = 31;
pub const GATE_ON: u32 = 32;
pub const GATE_THRESHOLD: u32 = 33;
pub const GATE_RELEASE: u32 = 34;
pub const DUCK_ON: u32 = 35;
pub const DUCK_THRESHOLD: u32 = 36;
pub const DUCK_RELEASE: u32 = 37;
pub const NOISE_ON: u32 = 38;
pub const NOISE_AMOUNT: u32 = 39;
pub const NOISE_MORPH: u32 = 40;
pub const WOBBLE_ON: u32 = 41;
pub const WOBBLE_AMOUNT: u32 = 42;
pub const WOBBLE_MORPH: u32 = 43;
pub const REPITCH: u32 = 44;
pub const REVERB: u32 = 45;
pub const REVERB_LOCATION: u32 = 46;
pub const REVERB_DECAY: u32 = 47;
pub const STEREO: u32 = 48;
pub const OUTPUT: u32 = 49;
pub const MIX: u32 = 50;
pub const EQUAL_LOUDNESS: u32 = 51;
const PARAMS: usize = 52;

pub const MODE_STEREO: usize = 0;
pub const MODE_PING_PONG: usize = 1;
pub const MODE_MID_SIDE: usize = 2;
const CHANNEL_MODES: [&str; 3] = ["Stereo", "Ping Pong", "Mid/Side"];
const SYNC_MODES: [&str; 4] = ["Notes", "Triplet", "Dotted", "16th"];
const NOTE_LABELS: [&str; 8] = [
    "1/64", "1/32", "1/16", "1/8", "1/4", "1/2", "1 Bar", "2 Bars",
];
const NOTE_BEATS: [f64; 8] = [0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
const WAVES: [&str; 6] = ["Sine", "Triangle", "Saw Up", "Saw Down", "Square", "Noise"];
const REVERB_LOCATIONS: [&str; 3] = ["Pre", "Post", "Feedback"];

/// Full Mod Delay: the delay time swings this fraction either way, times four with the x4
/// switch. The manual has no number.
pub const MOD_DELAY_DEPTH: f32 = 0.1;
/// Full Mod Filter: the filters swing this many octaves either way. The manual has no number.
pub const MOD_FILTER_OCTAVES: f32 = 3.0;
const WOBBLE_DEPTH: f32 = 0.02;
const MAX_SECONDS: f64 = 13.0;
const SILENT: f32 = 1e-5;
/// How long the input has to be silent before the device goes to sleep, per the manual.
pub const SLEEP_SECONDS: f64 = 8.0;

/// What the tunnel display reads: the left and right delay times in beats, and `[level]` of the
/// wet signal.
pub const TUNNEL_VIEW: ViewId = ViewId(0);
pub const TUNNEL_VIEW_LEN: usize = 3;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(CHANNEL_MODE, "Channel Mode", &CHANNEL_MODES, 0),
        ParamInfo::toggle(LINK, "Stereo Link", true),
        ParamInfo::toggle(SYNC_L, "L Sync", true),
        ParamInfo::stepped(SYNC_MODE_L, "L Sync Mode", &SYNC_MODES, 0),
        ParamInfo::stepped(NOTE_L, "L Division", &NOTE_LABELS, 2),
        ParamInfo::continuous(SIXTEENTH_L, "L 16th", 1.0, 16.0, 3.0, ""),
        ParamInfo::skewed(TIME_L, "L Time", 1.0, 5_000.0, 300.0, "ms", 2.0),
        ParamInfo::continuous(OFFSET_L, "L Offset", -33.0, 33.0, 0.0, "%"),
        ParamInfo::toggle(SYNC_R, "R Sync", true),
        ParamInfo::stepped(SYNC_MODE_R, "R Sync Mode", &SYNC_MODES, 0),
        ParamInfo::stepped(NOTE_R, "R Division", &NOTE_LABELS, 2),
        ParamInfo::continuous(SIXTEENTH_R, "R 16th", 1.0, 16.0, 3.0, ""),
        ParamInfo::skewed(TIME_R, "R Time", 1.0, 5_000.0, 300.0, "ms", 2.0),
        ParamInfo::continuous(OFFSET_R, "R Offset", -33.0, 33.0, 0.0, "%"),
        ParamInfo::continuous(INPUT, "Input", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::toggle(DRIVE, "Input Distortion", false),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 1.0, 0.5, ""),
        ParamInfo::toggle(INVERT, "Feedback Invert", false),
        ParamInfo::toggle(FILTER_ON, "Filter", true),
        ParamInfo::skewed(HP_FREQ, "HP", 20.0, 10_000.0, 100.0, "Hz", 3.0),
        ParamInfo::continuous(HP_RES, "HP Res", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(LP_FREQ, "LP", 200.0, 20_000.0, 10_000.0, "Hz", 3.0),
        ParamInfo::continuous(LP_RES, "LP Res", 0.0, 1.0, 0.0, ""),
        ParamInfo::stepped(MOD_WAVE, "Waveform", &WAVES, 0),
        ParamInfo::toggle(MOD_SYNC, "Mod Sync", false),
        ParamInfo::stepped(MOD_RATE, "Mod Rate", &SyncRate::LABELS, 12),
        ParamInfo::skewed(MOD_FREQ, "Mod Freq", 0.01, 40.0, 0.5, "Hz", 3.0),
        ParamInfo::continuous(MOD_PHASE, "Phase", 0.0, 360.0, 0.0, "deg"),
        ParamInfo::continuous(MOD_DELAY, "Mod Delay", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(MOD_X4, "Modulation x4", false),
        ParamInfo::continuous(MOD_FILTER, "Mod Filter", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(ENV_MIX, "Env Mix", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(GATE_ON, "Gate", false),
        ParamInfo::continuous(GATE_THRESHOLD, "Gate Threshold", -70.0, 0.0, -40.0, "dB"),
        ParamInfo::skewed(GATE_RELEASE, "Gate Release", 1.0, 2_000.0, 50.0, "ms", 3.0),
        ParamInfo::toggle(DUCK_ON, "Ducking", false),
        ParamInfo::continuous(DUCK_THRESHOLD, "Duck Threshold", -70.0, 0.0, -30.0, "dB"),
        ParamInfo::skewed(DUCK_RELEASE, "Duck Release", 1.0, 2_000.0, 200.0, "ms", 3.0),
        ParamInfo::toggle(NOISE_ON, "Noise", false),
        ParamInfo::continuous(NOISE_AMOUNT, "Noise Amount", 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(NOISE_MORPH, "Noise Morph", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(WOBBLE_ON, "Wobble", false),
        ParamInfo::continuous(WOBBLE_AMOUNT, "Wobble Amount", 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(WOBBLE_MORPH, "Wobble Morph", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(REPITCH, "Repitch", true),
        ParamInfo::continuous(REVERB, "Reverb", 0.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(REVERB_LOCATION, "Reverb Location", &REVERB_LOCATIONS, 1),
        ParamInfo::continuous(REVERB_DECAY, "Decay", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(STEREO, "Stereo", 0.0, 200.0, 100.0, "%"),
        ParamInfo::continuous(OUTPUT, "Output", -36.0, 12.0, 0.0, "dB"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 100.0, 50.0, "%"),
        ParamInfo::toggle(EQUAL_LOUDNESS, "Equal-Loudness", false),
    ]
}

fn layout() -> Vec<Section> {
    let row = Item::Row;
    vec![
        Section {
            title: "Tunnel",
            items: vec![Item::Panel("echo_tunnel")],
        },
        Section {
            title: "Channels",
            items: vec![
                row(vec![menu(CHANNEL_MODE), toggle(LINK)]),
                row(vec![
                    toggle(SYNC_L),
                    menu(SYNC_MODE_L),
                    menu(NOTE_L),
                    knob(SIXTEENTH_L),
                    knob(TIME_L),
                    knob(OFFSET_L),
                ]),
                row(vec![
                    toggle(SYNC_R),
                    menu(SYNC_MODE_R),
                    menu(NOTE_R),
                    knob(SIXTEENTH_R),
                    knob(TIME_R),
                    knob(OFFSET_R),
                ]),
                row(vec![
                    knob(INPUT),
                    toggle(DRIVE),
                    knob(FEEDBACK),
                    toggle(INVERT),
                ]),
            ],
        },
        Section {
            title: "Filter",
            items: vec![
                toggle(FILTER_ON),
                knob(HP_FREQ),
                knob(HP_RES),
                knob(LP_FREQ),
                knob(LP_RES),
            ],
        },
        Section {
            title: "Modulation",
            items: vec![
                row(vec![
                    menu(MOD_WAVE),
                    toggle(MOD_SYNC),
                    menu(MOD_RATE),
                    knob(MOD_FREQ),
                    knob(MOD_PHASE),
                ]),
                row(vec![
                    knob(MOD_DELAY),
                    toggle(MOD_X4),
                    knob(MOD_FILTER),
                    knob(ENV_MIX),
                ]),
            ],
        },
        Section {
            title: "Character",
            items: vec![
                row(vec![
                    toggle(GATE_ON),
                    knob(GATE_THRESHOLD),
                    knob(GATE_RELEASE),
                    toggle(DUCK_ON),
                    knob(DUCK_THRESHOLD),
                    knob(DUCK_RELEASE),
                ]),
                row(vec![
                    toggle(NOISE_ON),
                    knob(NOISE_AMOUNT),
                    knob(NOISE_MORPH),
                    toggle(WOBBLE_ON),
                    knob(WOBBLE_AMOUNT),
                    knob(WOBBLE_MORPH),
                    toggle(REPITCH),
                ]),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                row(vec![
                    knob(REVERB),
                    menu(REVERB_LOCATION),
                    knob(REVERB_DECAY),
                ]),
                row(vec![
                    knob(STEREO),
                    knob(OUTPUT),
                    knob(MIX),
                    toggle(EQUAL_LOUDNESS),
                ]),
            ],
        },
    ]
}

const COMB_SAMPLES: [usize; 4] = [1117, 1279, 1423, 1559];
const ALLPASS_SAMPLES: [usize; 2] = [347, 113];

#[derive(Clone, Debug, Default)]
struct Line {
    buf: Vec<f32>,
    pos: usize,
    damp: f32,
}

impl Line {
    fn new(frames: usize) -> Self {
        Self {
            buf: vec![0.0; frames.max(4)],
            pos: 0,
            damp: 0.0,
        }
    }

    #[inline]
    fn comb(&mut self, x: f32, feedback: f32) -> f32 {
        let y = self.buf[self.pos];
        self.damp += (y - self.damp) * 0.4;
        self.buf[self.pos] = sanitize(x + self.damp * feedback);
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }

    #[inline]
    fn allpass(&mut self, x: f32) -> f32 {
        let y = self.buf[self.pos];
        self.buf[self.pos] = sanitize(x + y * 0.5);
        self.pos = (self.pos + 1) % self.buf.len();
        y - x * 0.5
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.damp = 0.0;
    }
}

/// A small reverb: four combs in parallel, then two allpasses, for colour rather than space.
#[derive(Clone, Debug, Default)]
struct SmallReverb {
    combs: Vec<Line>,
    allpasses: Vec<Line>,
    feedback: f32,
}

impl SmallReverb {
    fn new(sample_rate: f64, stretch: usize) -> Self {
        let scale = sample_rate / 44_100.0;
        let frames = |n: usize| ((n + stretch) as f64 * scale) as usize;
        Self {
            combs: COMB_SAMPLES.iter().map(|n| Line::new(frames(*n))).collect(),
            allpasses: ALLPASS_SAMPLES
                .iter()
                .map(|n| Line::new(frames(*n)))
                .collect(),
            feedback: 0.7,
        }
    }

    fn set_decay(&mut self, amount: f32) {
        self.feedback = 0.5 + 0.45 * amount.clamp(0.0, 1.0);
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let mut y = 0.0;
        for c in self.combs.iter_mut() {
            y += c.comb(x, self.feedback);
        }
        y *= 0.25;
        for a in self.allpasses.iter_mut() {
            y = a.allpass(y);
        }
        sanitize(y)
    }

    fn clear(&mut self) {
        self.combs.iter_mut().for_each(Line::clear);
        self.allpasses.iter_mut().for_each(Line::clear);
    }
}

/// Everything read from the parameters each block.
#[derive(Clone, Copy, Debug)]
struct Setup {
    mode: usize,
    times: [f32; 2],
    input: f32,
    drive: bool,
    feedback: f32,
    filter_on: bool,
    mod_delay: f32,
    mod_filter: f32,
    env_mix: f32,
    gate_on: bool,
    gate_threshold: f32,
    duck_on: bool,
    duck_threshold: f32,
    noise_on: bool,
    noise_amount: f32,
    noise_morph: f32,
    wobble_on: bool,
    wobble_amount: f32,
    wobble_morph: f32,
    reverb: f32,
    reverb_location: usize,
    width: f32,
    output: f32,
    dry: f32,
    wet: f32,
}

pub struct Echo {
    v: [f64; PARAMS],
    sr: f32,
    lines: [InterpDelay; 2],
    heads: [DelayHead; 2],
    lfo: WaveLfo,
    env: EnvelopeFollower,
    gate: EnvelopeFollower,
    duck: EnvelopeFollower,
    gate_gain: f32,
    hp: [Filter; 2],
    lp: [Filter; 2],
    reverbs: [SmallReverb; 2],
    rng: Rng,
    noise_state: f32,
    wobble_phase: f32,
    wobble_random: [f32; 2],
    wobble_timer: f32,
    idle_frames: f64,
    asleep: bool,
    last_peak: f32,
}

impl Default for Echo {
    fn default() -> Self {
        Self {
            v: values(&params()),
            sr: 48_000.0,
            lines: [InterpDelay::new(16), InterpDelay::new(16)],
            heads: [DelayHead::default(); 2],
            lfo: WaveLfo::new(0xEC0),
            env: EnvelopeFollower::new(),
            gate: EnvelopeFollower::new(),
            duck: EnvelopeFollower::new(),
            gate_gain: 1.0,
            hp: [Filter::new(), Filter::new()],
            lp: [Filter::new(), Filter::new()],
            reverbs: [SmallReverb::default(), SmallReverb::default()],
            rng: Rng::new(0xEC0),
            noise_state: 0.0,
            wobble_phase: 0.0,
            wobble_random: [0.0; 2],
            wobble_timer: 0.0,
            idle_frames: 0.0,
            asleep: false,
            last_peak: 0.0,
        }
    }
}

impl Echo {
    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    /// One channel's delay as it reads to the ear, in beats or milliseconds.
    fn seconds(&self, channel: usize, tempo: f64) -> f64 {
        let linked = channel == 1 && self.flag(LINK);
        let use_right = channel == 1 && !linked;
        let (sync, sync_mode, note, sixteenth, time) = if use_right {
            (SYNC_R, SYNC_MODE_R, NOTE_R, SIXTEENTH_R, TIME_R)
        } else {
            (SYNC_L, SYNC_MODE_L, NOTE_L, SIXTEENTH_L, TIME_L)
        };
        let offset = self.v[if channel == 1 { OFFSET_R } else { OFFSET_L } as usize];
        let base = if self.flag(sync) {
            let note_index = self.v[note as usize].round().max(0.0) as usize;
            let index = note_index.min(NOTE_BEATS.len() - 1);
            let beats = match self.v[sync_mode as usize].round() as i32 {
                1 => NOTE_BEATS[index] * 2.0 / 3.0,
                2 => NOTE_BEATS[index] * 1.5,
                3 => numbered_beats_free(self.v[sixteenth as usize]),
                _ => NOTE_BEATS[index],
            };
            beats * 60.0 / tempo.max(1.0)
        } else {
            self.v[time as usize] * 0.001
        };
        base * (1.0 + offset * 0.01)
    }

    fn mod_settings(&self) -> WaveSettings {
        let wave = match self.v[MOD_WAVE as usize].round() as i32 {
            1 => Wave::Triangle,
            2 => Wave::SawUp,
            3 => Wave::SawDown,
            4 => Wave::Rectangle,
            5 => Wave::Noise,
            _ => Wave::Sine,
        };
        let rate = if self.flag(MOD_SYNC) {
            LfoRate::Beats(SyncRate::from_index(self.v[MOD_RATE as usize]).beats())
        } else {
            LfoRate::Hz(self.value(MOD_FREQ))
        };
        WaveSettings {
            wave,
            rate,
            phase: self.value(MOD_PHASE) / 360.0,
            ..Default::default()
        }
    }

    fn setup(&self, tempo: f64) -> Setup {
        let (dry, wet) = mix_gains(self.value(MIX) * 0.01, self.flag(EQUAL_LOUDNESS));
        let sign = if self.flag(INVERT) { -1.0 } else { 1.0 };
        let mut mod_delay = self.value(MOD_DELAY) * 0.01 * MOD_DELAY_DEPTH;
        if self.flag(MOD_X4) {
            mod_delay *= 4.0;
        }
        Setup {
            mode: self.v[CHANNEL_MODE as usize].round() as usize,
            times: [
                (self.seconds(0, tempo) * self.sr as f64) as f32,
                (self.seconds(1, tempo) * self.sr as f64) as f32,
            ],
            input: db_to_gain(self.value(INPUT)),
            drive: self.flag(DRIVE),
            feedback: self.value(FEEDBACK) * sign,
            filter_on: self.flag(FILTER_ON),
            mod_delay,
            mod_filter: self.value(MOD_FILTER) * 0.01 * MOD_FILTER_OCTAVES,
            env_mix: self.value(ENV_MIX) * 0.01,
            gate_on: self.flag(GATE_ON),
            gate_threshold: db_to_gain(self.value(GATE_THRESHOLD)),
            duck_on: self.flag(DUCK_ON),
            duck_threshold: db_to_gain(self.value(DUCK_THRESHOLD)),
            noise_on: self.flag(NOISE_ON),
            noise_amount: 0.1 * (self.value(NOISE_AMOUNT) * 0.01).powi(2),
            noise_morph: self.value(NOISE_MORPH) * 0.01,
            wobble_on: self.flag(WOBBLE_ON),
            wobble_amount: self.value(WOBBLE_AMOUNT) * 0.01,
            wobble_morph: self.value(WOBBLE_MORPH) * 0.01,
            reverb: self.value(REVERB) * 0.01,
            reverb_location: self.v[REVERB_LOCATION as usize].round() as usize,
            width: self.value(STEREO) * 0.01,
            output: db_to_gain(self.value(OUTPUT)),
            dry,
            wet,
        }
    }

    fn set_filter(&mut self, channel: usize, modulation: f32) {
        let octave = 2f32.powf(modulation);
        self.hp[channel].set(&FilterSettings {
            kind: FilterKind::HighPass,
            slope: FilterSlope::Db12,
            cutoff: (self.value(HP_FREQ) * octave).clamp(10.0, self.sr * 0.45),
            resonance: self.value(HP_RES),
            ..Default::default()
        });
        self.lp[channel].set(&FilterSettings {
            kind: FilterKind::LowPass,
            slope: FilterSlope::Db12,
            cutoff: (self.value(LP_FREQ) * octave).clamp(10.0, self.sr * 0.45),
            resonance: self.value(LP_RES),
            ..Default::default()
        });
    }

    fn wobble(&mut self, frames: usize, s: &Setup) -> [f32; 2] {
        if !s.wobble_on {
            return [0.0; 2];
        }
        let dt = frames as f32 / self.sr;
        self.wobble_phase = (self.wobble_phase + dt * 0.8).fract();
        self.wobble_timer -= dt;
        if self.wobble_timer <= 0.0 {
            self.wobble_timer = 0.09;
            self.wobble_random = [self.rng.bipolar(), self.rng.bipolar()];
        }
        let slow = (self.wobble_phase * std::f32::consts::TAU).sin();
        let wow = |r: f32| slow * (1.0 - s.wobble_morph) + r * s.wobble_morph;
        [
            wow(self.wobble_random[0]) * s.wobble_amount * WOBBLE_DEPTH,
            wow(self.wobble_random[1]) * s.wobble_amount * WOBBLE_DEPTH,
        ]
    }

    #[inline]
    fn noise(&mut self, s: &Setup) -> f32 {
        if !s.noise_on {
            return 0.0;
        }
        let white = self.rng.bipolar();
        self.noise_state += (white - self.noise_state) * 0.05;
        (white * (1.0 - s.noise_morph) + self.noise_state * 3.0 * s.noise_morph) * s.noise_amount
    }

    fn render(&mut self, audio: AudioBuffer, tempo: f64) {
        let s = self.setup(tempo);
        let wave = self.mod_settings();
        self.lfo.set(&wave);
        let repitch = if self.flag(REPITCH) {
            Transition::Repitch
        } else {
            Transition::Fade
        };
        self.heads
            .iter_mut()
            .for_each(|h| h.set_mode(repitch, false));
        let decay = self.value(REVERB_DECAY) * 0.01;
        self.reverbs.iter_mut().for_each(|r| r.set_decay(decay));
        self.gate.set_times(0.001, self.value(GATE_RELEASE) * 0.001);
        self.duck.set_times(0.002, self.value(DUCK_RELEASE) * 0.001);
        let frames = audio.frames();
        let input_quiet = audio.left[..frames]
            .iter()
            .chain(audio.right[..frames].iter())
            .all(|x| x.abs() < SILENT);
        let mut wet_peak = 0.0f32;

        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (lc, rc) in left
            .chunks_mut(CONTROL_FRAMES)
            .zip(right.chunks_mut(CONTROL_FRAMES))
        {
            let n = lc.len();
            let mods = self.lfo.advance(n);
            let envelope = self.env.level();
            let m = [
                mods[0] + (envelope - mods[0]) * s.env_mix,
                mods[1] + (envelope - mods[1]) * s.env_mix,
            ];
            let wobble = self.wobble(n, &s);
            for ch in 0..2 {
                let d = s.times[ch] * (1.0 + m[ch] * s.mod_delay + wobble[ch]);
                self.heads[ch].set_target(d.max(1.0));
                if s.filter_on {
                    self.set_filter(ch, m[ch] * s.mod_filter);
                }
            }
            for (l, r) in lc.iter_mut().zip(rc.iter_mut()) {
                let (mut in_l, mut in_r) = (*l, *r);
                // The input stage: gate, gain, distortion.
                if s.gate_on {
                    let level = self.gate.process(in_l.abs().max(in_r.abs()));
                    let target = if level > s.gate_threshold { 1.0 } else { 0.0 };
                    self.gate_gain += (target - self.gate_gain) * 0.02;
                    in_l *= self.gate_gain;
                    in_r *= self.gate_gain;
                }
                in_l *= s.input;
                in_r *= s.input;
                if s.drive {
                    in_l = in_l.tanh();
                    in_r = in_r.tanh();
                }
                self.env.process(in_l.abs().max(in_r.abs()));

                // Into the coded channels the two lines carry.
                let (a_in, b_in) = match s.mode {
                    MODE_PING_PONG => ((in_l + in_r) * 0.5, 0.0),
                    MODE_MID_SIDE => ((in_l + in_r) * 0.5, (in_l - in_r) * 0.5),
                    _ => (in_l, in_r),
                };

                let lines = &self.lines;
                let y_a = self.heads[0].read(|d| lines[0].read(d - 1.0, Interpolation::Cubic));
                let y_b = self.heads[1].read(|d| lines[1].read(d - 1.0, Interpolation::Cubic));
                let (mut fb_a, mut fb_b) = (y_a, y_b);
                if s.filter_on {
                    fb_a = self.lp[0].process(self.hp[0].process(fb_a));
                    fb_b = self.lp[1].process(self.hp[1].process(fb_b));
                }
                let (reverb_a, reverb_b) = if s.reverb > 0.0 && s.reverb_location == 2 {
                    (self.reverbs[0].process(fb_a), self.reverbs[1].process(fb_b))
                } else {
                    (0.0, 0.0)
                };
                let (loop_a, loop_b) = (fb_a + reverb_a * s.reverb, fb_b + reverb_b * s.reverb);
                let (pre_a, pre_b) = if s.reverb > 0.0 && s.reverb_location == 0 {
                    (self.reverbs[0].process(a_in), self.reverbs[1].process(b_in))
                } else {
                    (0.0, 0.0)
                };
                let (write_a, write_b) = if s.mode == MODE_PING_PONG {
                    (
                        a_in + pre_a * s.reverb + loop_b * s.feedback,
                        pre_b * s.reverb + loop_a * s.feedback,
                    )
                } else {
                    (
                        a_in + pre_a * s.reverb + loop_a * s.feedback,
                        b_in + pre_b * s.reverb + loop_b * s.feedback,
                    )
                };
                self.lines[0].write(guard(write_a));
                self.lines[1].write(guard(write_b));

                // The wet signal, decoded.
                let (mut wet_l, mut wet_r) = if s.mode == MODE_MID_SIDE {
                    (fb_a + fb_b, fb_a - fb_b)
                } else {
                    (fb_a, fb_b)
                };
                if s.reverb > 0.0 && s.reverb_location == 1 {
                    wet_l += self.reverbs[0].process(wet_l) * s.reverb;
                    wet_r += self.reverbs[1].process(wet_r) * s.reverb;
                }
                wet_peak = wet_peak.max(wet_l.abs()).max(wet_r.abs());
                if s.duck_on {
                    let level = self.duck.process(in_l.abs().max(in_r.abs()));
                    if level > s.duck_threshold {
                        let g = s.duck_threshold / level;
                        wet_l *= g;
                        wet_r *= g;
                    }
                }
                let noise = self.noise(&s);
                wet_l += noise;
                wet_r += self.noise(&s);

                let mid = (wet_l + wet_r) * 0.5;
                let side = (wet_l - wet_r) * 0.5 * s.width;
                let out_l = (mid + side) * s.output;
                let out_r = (mid - side) * s.output;
                *l = in_l * s.dry + out_l * s.wet;
                *r = in_r * s.dry + out_r * s.wet;
            }
        }
        self.last_peak = wet_peak;
        // Ask to sleep only if the repeats have died away; Noise doesn't count.
        if input_quiet && wet_peak < SILENT {
            self.idle_frames += frames as f64;
        } else {
            self.idle_frames = 0.0;
        }
    }

    fn hold_on(&self) -> bool {
        self.flag(NOISE_ON) && self.flag(GATE_ON)
    }

    fn step(&mut self, audio: AudioBuffer, tempo: f64) {
        let quiet = audio
            .left
            .iter()
            .chain(audio.right.iter())
            .all(|x| x.abs() < SILENT);
        if !quiet {
            self.asleep = false;
            self.idle_frames = 0.0;
        }
        if self.asleep {
            return;
        }
        self.render(audio, tempo);
        if self.idle_frames >= SLEEP_SECONDS * self.sr as f64 && !self.hold_on() {
            self.asleep = true;
        }
    }
}

/// `sixteenths` at their written length, in beats.
fn numbered_beats_free(sixteenths: f64) -> f64 {
    sixteenths.max(0.1) * 0.25
}

impl Processor for Echo {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let capacity = (sample_rate * MAX_SECONDS) as usize;
        self.lines = [InterpDelay::new(capacity), InterpDelay::new(capacity)];
        self.heads.iter_mut().for_each(|h| h.prepare(self.sr));
        self.lfo.prepare(sample_rate);
        for e in [&mut self.env, &mut self.gate, &mut self.duck] {
            e.prepare(sample_rate);
        }
        self.env.set_times(0.005, 0.15);
        for f in self.hp.iter_mut().chain(self.lp.iter_mut()) {
            f.prepare(sample_rate);
        }
        for ch in 0..2 {
            self.set_filter(ch, 0.0);
        }
        self.reverbs = [
            SmallReverb::new(sample_rate, 0),
            SmallReverb::new(sample_rate, 23),
        ];
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let tempo = ctx.transport.tempo;
        self.lfo
            .set_clock(&BeatClock::from_transport(ctx.transport));
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.step(audio, tempo),
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            let beats = [
                (self.seconds(0, tempo) * tempo / 60.0) as f32,
                (self.seconds(1, tempo) * tempo / 60.0) as f32,
                self.last_peak,
            ];
            ctx.view.publish(TUNNEL_VIEW, &beats);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(InterpDelay::clear);
        for f in self.hp.iter_mut().chain(self.lp.iter_mut()) {
            f.reset();
        }
        self.reverbs.iter_mut().for_each(SmallReverb::clear);
        self.env.reset();
        self.gate.reset();
        self.duck.reset();
        self.noise_state = 0.0;
        self.idle_frames = 0.0;
        self.asleep = false;
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.echo",
    name: "Echo",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Echo>::default(),
};
