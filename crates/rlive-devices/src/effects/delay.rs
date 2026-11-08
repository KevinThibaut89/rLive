use super::defaults;
use crate::devices::b6::common::{
    knob, menu, mix_gains, toggle, DelayHead, LinkedBand, Transition, Wave, WaveLfo, WaveSettings,
    CONTROL_FRAMES,
};
use crate::dsp::lfo::LfoRate;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::sanitize;
use crate::dsp::{Biquad, BiquadKind, DelayLine, Smoother};
use rlive_core::{
    AudioBuffer, BlockStep, Item, ParamInfo, ProcessContext, Processor, Section, Tail,
};

// ---------------------------------------------------------------- Delay

pub const SYNC: u32 = 0;
pub const TIME: u32 = 1;
pub const DIVISION: u32 = 2;
pub const FEEDBACK: u32 = 3;
pub const MIX: u32 = 4;
pub const PING_PONG: u32 = 5;
pub const TONE: u32 = 6;
// Everything from here on came after the original seven parameters. At their defaults the
// original render path runs and existing projects sound exactly as they did.
pub const STEREO_LINK: u32 = 7;
pub const SYNC_R: u32 = 8;
pub const TIME_R: u32 = 9;
pub const DIVISION_R: u32 = 10;
pub const OFFSET_L: u32 = 11;
pub const OFFSET_R: u32 = 12;
pub const FREEZE: u32 = 13;
pub const FILTER_ON: u32 = 14;
pub const FILTER_FREQ: u32 = 15;
pub const FILTER_WIDTH: u32 = 16;
pub const LFO_WAVE: u32 = 17;
pub const LFO_RATE_MODE: u32 = 18;
pub const LFO_FREQ: u32 = 19;
pub const LFO_TIME: u32 = 20;
pub const LFO_SYNC: u32 = 21;
pub const LFO_SIXTEENTHS: u32 = 22;
pub const LFO_MORPH: u32 = 23;
pub const LFO_DELAY: u32 = 24;
pub const LFO_FILTER: u32 = 25;
pub const SMOOTHING: u32 = 26;
pub const HI_QUALITY: u32 = 27;
pub const EQUAL_LOUDNESS: u32 = 28;

/// A choice of length. The last entry came later, so saved indexes keep their meaning.
pub const DIVISIONS: [&str; 8] = ["1/16", "1/8", "1/8.", "1/4", "1/4.", "1/2", "1 Bar", "5/16"];
/// Division lengths in quarter notes.
pub const DIVISION_BEATS: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 4.0, 1.25];

/// The longest time the line can hold, in seconds: 16 sixteenths at 20 BPM.
const MAX_SECONDS: f64 = 13.0;

pub const SMOOTHING_LABELS: [&str; 3] = ["Repitch", "Fade", "Jump"];
pub const LFO_WAVES: [&str; 7] = [
    "Sine",
    "Triangle",
    "Ramp Up",
    "Ramp Down",
    "Square",
    "S&H",
    "Wander",
];
pub const LFO_RATE_MODES: [&str; 6] = ["Rate", "Time", "Synced", "Triplet", "Dotted", "Sixteenth"];
/// Full LFO Delay: the delay time swings by this fraction either way. The manual has no number.
pub const LFO_DELAY_DEPTH: f32 = 0.5;
/// Full LFO Filter: the filter swings this many octaves either way. The manual has no number.
pub const LFO_FILTER_OCTAVES: f32 = 2.0;

fn lfo_wave(index: f64) -> Wave {
    match index.round() as i32 {
        1 => Wave::Triangle,
        2 => Wave::SawUp,
        3 => Wave::SawDown,
        4 => Wave::Rectangle,
        5 => Wave::RandomHold,
        6 => Wave::Random,
        _ => Wave::Sine,
    }
}

fn division_seconds(index: f64, tempo: f64) -> f64 {
    let i = (index.round().max(0.0) as usize).min(DIVISION_BEATS.len() - 1);
    DIVISION_BEATS[i] * 60.0 / tempo.max(1.0)
}

pub fn delay_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::toggle(SYNC, "Sync", true),
        ParamInfo::skewed(TIME, "Time", 1.0, 5_000.0, 375.0, "ms", 2.0),
        ParamInfo::stepped(DIVISION, "Division", &DIVISIONS, 2),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 0.95, 0.4, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 0.3, ""),
        ParamInfo::toggle(PING_PONG, "Ping Pong", true),
        ParamInfo::skewed(TONE, "Tone", 200.0, 20_000.0, 6_000.0, "Hz", 3.0),
        ParamInfo::toggle(STEREO_LINK, "Stereo Link", true),
        ParamInfo::toggle(SYNC_R, "Sync R", true),
        ParamInfo::skewed(TIME_R, "Time R", 1.0, 5_000.0, 375.0, "ms", 2.0),
        ParamInfo::stepped(DIVISION_R, "Division R", &DIVISIONS, 2),
        ParamInfo::continuous(OFFSET_L, "Offset L", -33.0, 33.0, 0.0, "%"),
        ParamInfo::continuous(OFFSET_R, "Offset R", -33.0, 33.0, 0.0, "%"),
        ParamInfo::toggle(FREEZE, "Freeze", false),
        ParamInfo::toggle(FILTER_ON, "Filter", false),
        ParamInfo::skewed(FILTER_FREQ, "Freq", 50.0, 18_000.0, 1_000.0, "Hz", 3.0),
        ParamInfo::continuous(FILTER_WIDTH, "Width", 0.5, 9.0, 9.0, "oct"),
        ParamInfo::stepped(LFO_WAVE, "LFO Wave", &LFO_WAVES, 0),
        ParamInfo::stepped(LFO_RATE_MODE, "LFO Rate Mode", &LFO_RATE_MODES, 0),
        ParamInfo::skewed(LFO_FREQ, "LFO Rate", 0.01, 40.0, 0.5, "Hz", 3.0),
        ParamInfo::skewed(LFO_TIME, "LFO Time", 10.0, 60_000.0, 2_000.0, "ms", 3.0),
        ParamInfo::stepped(LFO_SYNC, "LFO Synced", &SyncRate::LABELS, 12),
        ParamInfo::continuous(LFO_SIXTEENTHS, "LFO Sixteenth", 1.0, 64.0, 4.0, ""),
        ParamInfo::continuous(LFO_MORPH, "LFO Morph", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_DELAY, "LFO Delay", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_FILTER, "LFO Filter", 0.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(SMOOTHING, "Smoothing", &SMOOTHING_LABELS, 0),
        ParamInfo::toggle(HI_QUALITY, "Hi-Quality", false),
        ParamInfo::toggle(EQUAL_LOUDNESS, "Equal-Loudness", false),
    ]
}

pub fn delay_layout() -> Vec<Section> {
    let row = Item::Row;
    vec![
        Section {
            title: "Time",
            items: vec![
                row(vec![
                    toggle(SYNC),
                    menu(DIVISION),
                    knob(TIME),
                    knob(OFFSET_L),
                ]),
                row(vec![
                    toggle(SYNC_R),
                    menu(DIVISION_R),
                    knob(TIME_R),
                    knob(OFFSET_R),
                ]),
                row(vec![toggle(STEREO_LINK), toggle(FREEZE), menu(SMOOTHING)]),
            ],
        },
        Section {
            title: "Filter",
            items: vec![
                row(vec![
                    toggle(FILTER_ON),
                    knob(FILTER_FREQ),
                    knob(FILTER_WIDTH),
                ]),
                knob(TONE),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                row(vec![menu(LFO_WAVE), menu(LFO_RATE_MODE), knob(LFO_MORPH)]),
                row(vec![
                    knob(LFO_FREQ),
                    knob(LFO_TIME),
                    menu(LFO_SYNC),
                    knob(LFO_SIXTEENTHS),
                ]),
                row(vec![knob(LFO_DELAY), knob(LFO_FILTER)]),
            ],
        },
        Section {
            title: "Output",
            items: vec![
                toggle(PING_PONG),
                knob(FEEDBACK),
                knob(MIX),
                toggle(HI_QUALITY),
                toggle(EQUAL_LOUDNESS),
            ],
        },
    ]
}

pub struct Delay {
    v: Vec<f64>,
    sr: f32,
    lines: [DelayLine; 2],
    time: Smoother,
    tone: [Biquad; 2],
    tone_dirty: bool,
    heads: [DelayHead; 2],
    band: [LinkedBand; 2],
    band_ready: bool,
    lfo: WaveLfo,
    extended_last: bool,
}

impl Default for Delay {
    fn default() -> Self {
        Self {
            v: defaults(&delay_params()),
            sr: 48_000.0,
            lines: [DelayLine::default(), DelayLine::default()],
            time: Smoother::new(0.0),
            tone: [Biquad::default(); 2],
            tone_dirty: true,
            heads: [DelayHead::default(); 2],
            band: [LinkedBand::default(); 2],
            band_ready: false,
            lfo: WaveLfo::new(0x5eed),
            extended_last: false,
        }
    }
}

impl Delay {
    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    /// Whether anything added after the original device is in use.
    fn extended(&self) -> bool {
        !self.flag(STEREO_LINK)
            || self.flag(FREEZE)
            || self.flag(FILTER_ON)
            || self.flag(HI_QUALITY)
            || self.flag(EQUAL_LOUDNESS)
            || self.value(OFFSET_L) != 0.0
            || self.value(OFFSET_R) != 0.0
            || self.value(LFO_DELAY) != 0.0
            || self.value(LFO_FILTER) != 0.0
            || self.value(SMOOTHING).round() != 0.0
            || self.v[TIME as usize] > 2_000.0
    }

    /// Dispatches to the original path or the full one, keeping the read position across the
    /// change so turning a new control on doesn't jump.
    fn render_any(&mut self, audio: AudioBuffer, tempo: f64) {
        let extended = self.extended();
        if extended != self.extended_last {
            if extended {
                if self.time.value > 0.0 {
                    let d = self.time.value;
                    self.heads.iter_mut().for_each(|h| h.place(d));
                }
            } else {
                self.time.value = self.heads[0].value();
            }
            self.extended_last = extended;
        }
        if extended {
            self.render_extended(audio, tempo);
        } else {
            self.render(audio, tempo);
        }
    }

    /// The original algorithm. Existing projects hash against this, so it must stay as it is.
    fn render(&mut self, audio: AudioBuffer, tempo: f64) {
        if self.tone_dirty {
            for f in self.tone.iter_mut() {
                f.set(
                    BiquadKind::LowPass,
                    self.v[TONE as usize] as f32,
                    0.707,
                    0.0,
                    self.sr,
                );
            }
            self.tone_dirty = false;
        }
        let seconds = if self.v[SYNC as usize] >= 0.5 {
            let div = (self.v[DIVISION as usize].round() as usize).min(DIVISION_BEATS.len() - 1);
            DIVISION_BEATS[div] * 60.0 / tempo
        } else {
            self.v[TIME as usize] / 1000.0
        };
        self.time.target = (seconds as f32 * self.sr).min(self.sr * 4.4);
        if self.time.value == 0.0 {
            self.time.reset();
        }
        let fb = self.v[FEEDBACK as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let ping = self.v[PING_PONG as usize] >= 0.5;
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for i in 0..left.len() {
            let d = self.time.next();
            let dl = self.tone[0].process(self.lines[0].read(d));
            let dr = self.tone[1].process(self.lines[1].read(d));
            let (inl, inr) = (left[i], right[i]);
            if ping {
                let mono = (inl + inr) * 0.5;
                self.lines[0].write(mono + dr * fb);
                self.lines[1].write(dl * fb);
            } else {
                self.lines[0].write(inl + dl * fb);
                self.lines[1].write(inr + dr * fb);
            }
            left[i] = inl * (1.0 - mix) + dl * mix;
            right[i] = inr * (1.0 - mix) + dr * mix;
        }
    }

    fn channel_seconds(&self, right: bool, tempo: f64) -> f64 {
        let linked = right && self.flag(STEREO_LINK);
        let side = right && !linked;
        let (sync, time, division) = if side {
            (self.flag(SYNC_R), TIME_R, DIVISION_R)
        } else {
            (self.flag(SYNC), TIME, DIVISION)
        };
        let base = if sync {
            division_seconds(self.v[division as usize], tempo)
        } else {
            self.v[time as usize] * 0.001
        };
        // Offset is each channel's own, linked or not.
        let offset = self.v[if right { OFFSET_R } else { OFFSET_L } as usize];
        base * (1.0 + offset * 0.01)
    }

    fn lfo_settings(&self) -> WaveSettings {
        let beats = |r: f64| LfoRate::Beats(r);
        let synced = SyncRate::from_index(self.v[LFO_SYNC as usize]);
        let rate = match self.v[LFO_RATE_MODE as usize].round() as i32 {
            1 => LfoRate::Hz(1000.0 / self.value(LFO_TIME).max(1.0)),
            2 => beats(synced.beats()),
            3 => beats(synced.beats() * 2.0 / 3.0),
            4 => beats(synced.beats() * 1.5),
            5 => beats(self.v[LFO_SIXTEENTHS as usize].max(0.25) * 0.25),
            _ => LfoRate::Hz(self.value(LFO_FREQ)),
        };
        WaveSettings {
            wave: lfo_wave(self.v[LFO_WAVE as usize]),
            rate,
            duty: self.value(LFO_MORPH) * 0.01,
            ..Default::default()
        }
    }

    fn set_band(&mut self, center: f32) {
        let width = self.value(FILTER_WIDTH);
        for b in self.band.iter_mut() {
            b.set(center, width, self.sr);
        }
    }

    fn render_extended(&mut self, audio: AudioBuffer, tempo: f64) {
        if self.tone_dirty {
            for f in self.tone.iter_mut() {
                f.set(
                    BiquadKind::LowPass,
                    self.v[TONE as usize] as f32,
                    0.707,
                    0.0,
                    self.sr,
                );
            }
            self.tone_dirty = false;
        }
        let smoothing = match self.v[SMOOTHING as usize].round() as i32 {
            1 => Transition::Fade,
            2 => Transition::Jump,
            _ => Transition::Repitch,
        };
        // Hi-Quality's better interpolation is for Repitch and Fade only, as the manual says.
        let hq = self.flag(HI_QUALITY) && smoothing != Transition::Jump;
        self.heads
            .iter_mut()
            .for_each(|h| h.set_mode(smoothing, hq));
        let wave = self.lfo_settings();
        self.lfo.set(&wave);
        let times = [
            (self.channel_seconds(false, tempo) * self.sr as f64) as f32,
            (self.channel_seconds(true, tempo) * self.sr as f64) as f32,
        ];
        let fb = self.value(FEEDBACK);
        let (dry, wet) = mix_gains(self.value(MIX), self.flag(EQUAL_LOUDNESS));
        let ping = self.flag(PING_PONG);
        let freeze = self.flag(FREEZE);
        let filter_on = self.flag(FILTER_ON);
        let center = self.value(FILTER_FREQ);
        let lfo_delay = self.value(LFO_DELAY) * 0.01 * LFO_DELAY_DEPTH;
        let lfo_filter = self.value(LFO_FILTER) * 0.01 * LFO_FILTER_OCTAVES;
        if filter_on && !self.band_ready {
            self.set_band(center);
            self.band_ready = true;
        }

        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (lc, rc) in left
            .chunks_mut(CONTROL_FRAMES)
            .zip(right.chunks_mut(CONTROL_FRAMES))
        {
            let mods = self.lfo.advance(lc.len());
            for ch in 0..2 {
                let mut d = times[ch] * (1.0 + lfo_delay * mods[ch]);
                if freeze {
                    d = d.round();
                }
                self.heads[ch].set_target(d.max(1.0));
            }
            if filter_on && lfo_filter != 0.0 {
                let m = (mods[0] + mods[1]) * 0.5;
                self.set_band(center * 2f32.powf(lfo_filter * m));
            }
            for (l, r) in lc.iter_mut().zip(rc.iter_mut()) {
                let read = |line: &DelayLine, d: f32| {
                    if hq {
                        line.read_cubic(d)
                    } else {
                        line.read(d)
                    }
                };
                let lines = &self.lines;
                let raw_l = self.heads[0].read(|d| read(&lines[0], d));
                let raw_r = self.heads[1].read(|d| read(&lines[1], d));
                let dl = self.tone[0].process(raw_l);
                let dr = self.tone[1].process(raw_r);
                let (inl, inr) = (*l, *r);
                *l = inl * dry + dl * wet;
                *r = inr * dry + dr * wet;

                let (write_l, write_r) = if freeze {
                    // The line keeps what it held: no input, no filtering, no decay.
                    if ping {
                        (raw_r, raw_l)
                    } else {
                        (raw_l, raw_r)
                    }
                } else {
                    let (a, b) = if ping {
                        ((inl + inr) * 0.5 + dr * fb, dl * fb)
                    } else {
                        (inl + dl * fb, inr + dr * fb)
                    };
                    if filter_on {
                        (self.band[0].process(a), self.band[1].process(b))
                    } else {
                        (a, b)
                    }
                };
                self.lines_write(write_l, write_r);
            }
        }
    }

    #[inline]
    fn lines_write(&mut self, left: f32, right: f32) {
        self.lines[0].write(sanitize(left));
        self.lines[1].write(sanitize(right));
    }
}

impl Processor for Delay {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let max = (sample_rate * MAX_SECONDS) as usize;
        self.lines = [DelayLine::new(max), DelayLine::new(max)];
        self.time.set_time(0.08, self.sr);
        self.tone_dirty = true;
        self.heads.iter_mut().for_each(|h| h.prepare(self.sr));
        self.lfo.prepare(sample_rate);
        self.band_ready = false;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let tempo = ctx.transport.tempo;
        self.lfo
            .set_clock(&BeatClock::from_transport(ctx.transport));
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render_any(audio, tempo),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        match id {
            TONE => self.tone_dirty = true,
            FILTER_FREQ | FILTER_WIDTH => self.band_ready = false,
            _ => {}
        }
    }

    fn reset(&mut self) {
        for l in self.lines.iter_mut() {
            l.clear();
        }
        for f in self.tone.iter_mut() {
            f.reset();
        }
        self.band.iter_mut().for_each(LinkedBand::reset);
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}
