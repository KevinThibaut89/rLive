//! Shifter: pitch, frequency or ring modulation, with an LFO, an envelope and a delay.
//!
//! The manual describes the three modes, Coarse and Fine, Spread with Wide, the Window, the
//! delay with Tone in its feedback, the LFO and envelope follower, Drive for Ring mode, and the
//! MIDI pitch source with Glide and Pitch Bend. It gives no ranges beyond the 0 to 24 semitone
//! Bend, and calls the delay slider "Hertz": we made it milliseconds, which is what the delay
//! time is. Pitch mode uses two crossfaded delay taps (the classic pitch shifter); its wet path
//! sits about half a Window behind the input, which is the reported latency, and the dry signal
//! is delayed to match. Freq and Ring have no latency. The Hilbert pair for Freq mode is
//! Olli Niemitalo's published IIR design, flat from about 20 Hz up to near Nyquist.

use super::common::{knob, menu, toggle, values, Wave, WaveLfo, WaveSettings, CONTROL_FRAMES};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::lfo::LfoRate;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{midi_to_hz, sanitize};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, MidiMessage, ParamInfo, ProcessContext, Processor, Section,
    Status, Tail,
};
use std::f32::consts::TAU;

pub const MODE: u32 = 0;
pub const COARSE_ST: u32 = 1;
pub const FINE_CT: u32 = 2;
pub const COARSE_HZ: u32 = 3;
pub const FINE_HZ: u32 = 4;
pub const SPREAD: u32 = 5;
pub const WIDE: u32 = 6;
pub const WINDOW: u32 = 7;
pub const TONE: u32 = 8;
pub const DELAY_ON: u32 = 9;
pub const DELAY_SYNC: u32 = 10;
pub const DELAY_TIME: u32 = 11;
pub const DELAY_DIVISION: u32 = 12;
pub const DELAY_FEEDBACK: u32 = 13;
pub const LFO_WAVE: u32 = 14;
pub const LFO_SYNC: u32 = 15;
pub const LFO_RATE: u32 = 16;
pub const LFO_DIVISION: u32 = 17;
pub const LFO_DUTY: u32 = 18;
pub const LFO_STEREO: u32 = 19;
pub const LFO_PHASE: u32 = 20;
pub const LFO_SPIN: u32 = 21;
pub const LFO_WIDTH: u32 = 22;
pub const LFO_OFFSET: u32 = 23;
pub const LFO_AMOUNT_ST: u32 = 24;
pub const LFO_AMOUNT_HZ: u32 = 25;
pub const ENV_ON: u32 = 26;
pub const ENV_ATTACK: u32 = 27;
pub const ENV_RELEASE: u32 = 28;
pub const ENV_AMOUNT_ST: u32 = 29;
pub const ENV_AMOUNT_HZ: u32 = 30;
pub const DRIVE_ON: u32 = 31;
pub const DRIVE: u32 = 32;
pub const MIX: u32 = 33;
pub const PITCH_MODE: u32 = 34;
pub const GLIDE: u32 = 35;
pub const PB_RANGE: u32 = 36;
const PARAMS: usize = 37;

pub const MODE_PITCH: usize = 0;
pub const MODE_FREQ: usize = 1;
pub const MODE_RING: usize = 2;

const WAVES: [&str; 10] = [
    "Sine",
    "Triangle",
    "Triangle Analog",
    "Triangle 8",
    "Triangle 16",
    "Saw Up",
    "Saw Down",
    "Rectangle",
    "Random",
    "Random S&H",
];

/// The longest Window, in ms. The line needs a little more, for the delay taps.
pub const MAX_WINDOW_MS: f64 = 500.0;
const REST_RATE: f32 = 1.5e-4;
const MAX_DELAY_SECONDS: f64 = 13.0;
const MAX_HELD_NOTES: usize = 16;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(MODE, "Mode", &["Pitch", "Freq", "Ring"], 0),
        ParamInfo::continuous(COARSE_ST, "Coarse", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(FINE_CT, "Fine", -100.0, 100.0, 0.0, "ct"),
        ParamInfo::continuous(COARSE_HZ, "Coarse Hz", -5_000.0, 5_000.0, 0.0, "Hz"),
        ParamInfo::continuous(FINE_HZ, "Fine Hz", -20.0, 20.0, 0.0, "Hz"),
        ParamInfo::continuous(SPREAD, "Spread", -100.0, 100.0, 0.0, ""),
        ParamInfo::toggle(WIDE, "Wide", false),
        ParamInfo::skewed(WINDOW, "Window", 10.0, MAX_WINDOW_MS, 20.0, "ms", 3.0),
        ParamInfo::skewed(TONE, "Tone", 200.0, 20_000.0, 18_000.0, "Hz", 3.0),
        ParamInfo::toggle(DELAY_ON, "Delay", false),
        ParamInfo::toggle(DELAY_SYNC, "Delay Sync", false),
        ParamInfo::skewed(DELAY_TIME, "Delay Time", 1.0, 5_000.0, 250.0, "ms", 2.0),
        ParamInfo::stepped(DELAY_DIVISION, "Delay Division", &SyncRate::LABELS, 10),
        ParamInfo::continuous(DELAY_FEEDBACK, "Delay Feedback", 0.0, 0.95, 0.3, ""),
        ParamInfo::stepped(LFO_WAVE, "LFO Wave", &WAVES, 0),
        ParamInfo::toggle(LFO_SYNC, "LFO Sync", false),
        ParamInfo::skewed(LFO_RATE, "LFO Rate", 0.01, 40.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(LFO_DIVISION, "LFO Division", &SyncRate::LABELS, 12),
        ParamInfo::continuous(LFO_DUTY, "Duty Cycle", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(LFO_STEREO, "LFO Stereo", &["Phase", "Spin"], 0),
        ParamInfo::continuous(LFO_PHASE, "LFO Phase", 0.0, 360.0, 0.0, "deg"),
        ParamInfo::continuous(LFO_SPIN, "LFO Spin", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_WIDTH, "LFO Width", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_OFFSET, "LFO Offset", 0.0, 360.0, 0.0, "deg"),
        ParamInfo::continuous(LFO_AMOUNT_ST, "LFO Amount", 0.0, 24.0, 0.0, "st"),
        ParamInfo::continuous(LFO_AMOUNT_HZ, "LFO Amount Hz", 0.0, 5_000.0, 0.0, "Hz"),
        ParamInfo::toggle(ENV_ON, "Env Fol", false),
        ParamInfo::skewed(ENV_ATTACK, "Env Attack", 0.1, 500.0, 5.0, "ms", 3.0),
        ParamInfo::skewed(ENV_RELEASE, "Env Release", 1.0, 2_000.0, 100.0, "ms", 3.0),
        ParamInfo::continuous(ENV_AMOUNT_ST, "Env Amount", -24.0, 24.0, 0.0, "st"),
        ParamInfo::continuous(ENV_AMOUNT_HZ, "Env Amount Hz", -5_000.0, 5_000.0, 0.0, "Hz"),
        ParamInfo::toggle(DRIVE_ON, "Drive", false),
        ParamInfo::continuous(DRIVE, "Drive Level", 0.0, 100.0, 30.0, "%"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 100.0, 100.0, "%"),
        ParamInfo::stepped(PITCH_MODE, "Pitch Mode", &["Internal", "MIDI"], 0),
        ParamInfo::skewed(GLIDE, "Glide", 0.0, 1_000.0, 0.0, "ms", 3.0),
        ParamInfo::continuous(PB_RANGE, "PB", 0.0, 24.0, 2.0, "st"),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Tuning",
            items: vec![
                menu(MODE),
                knob(COARSE_ST),
                knob(FINE_CT),
                knob(COARSE_HZ),
                knob(FINE_HZ),
                knob(SPREAD),
                toggle(WIDE),
                knob(WINDOW),
            ],
        },
        Section {
            title: "Delay",
            items: vec![
                toggle(DELAY_ON),
                toggle(DELAY_SYNC),
                knob(DELAY_TIME),
                menu(DELAY_DIVISION),
                knob(DELAY_FEEDBACK),
                knob(TONE),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                menu(LFO_WAVE),
                toggle(LFO_SYNC),
                knob(LFO_RATE),
                menu(LFO_DIVISION),
                knob(LFO_DUTY),
                menu(LFO_STEREO),
                knob(LFO_PHASE),
                knob(LFO_SPIN),
                knob(LFO_WIDTH),
                knob(LFO_OFFSET),
                knob(LFO_AMOUNT_ST),
                knob(LFO_AMOUNT_HZ),
            ],
        },
        Section {
            title: "Envelope",
            items: vec![
                toggle(ENV_ON),
                knob(ENV_ATTACK),
                knob(ENV_RELEASE),
                knob(ENV_AMOUNT_ST),
                knob(ENV_AMOUNT_HZ),
            ],
        },
        Section {
            title: "Output",
            items: vec![toggle(DRIVE_ON), knob(DRIVE), knob(MIX)],
        },
        Section {
            title: "MIDI",
            items: vec![menu(PITCH_MODE), knob(GLIDE), knob(PB_RANGE)],
        },
    ]
}

/// One of the two chains of allpass sections behind a Hilbert transform.
#[derive(Clone, Copy, Debug, Default)]
struct AllPassChain {
    x1: [f32; HILBERT_ORDER],
    x2: [f32; HILBERT_ORDER],
    y1: [f32; HILBERT_ORDER],
    y2: [f32; HILBERT_ORDER],
}

const HILBERT_ORDER: usize = 4;

impl AllPassChain {
    #[inline]
    fn process(&mut self, mut x: f32, coefficients: &[f32; HILBERT_ORDER]) -> f32 {
        for (i, c) in coefficients.iter().enumerate() {
            let a = c * c;
            let y = a * (x + self.y2[i]) - self.x2[i];
            self.x2[i] = self.x1[i];
            self.x1[i] = x;
            self.y2[i] = self.y1[i];
            self.y1[i] = sanitize(y);
            x = y;
        }
        x
    }
}

const PATH_A: [f32; 4] = [0.692_387_8, 0.936_065_4, 0.988_229_5, 0.998_748_8];
const PATH_B: [f32; 4] = [0.402_192_1, 0.856_171_1, 0.972_290_95, 0.995_288_5];

/// Splits a signal into two copies in quadrature.
#[derive(Clone, Copy, Debug, Default)]
struct Hilbert {
    a: AllPassChain,
    b: AllPassChain,
    delay: f32,
}

impl Hilbert {
    /// `(in_phase, quadrature)`.
    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        let i = self.a.process(x, &PATH_A);
        let delayed = self.delay;
        self.delay = i;
        let q = self.b.process(x, &PATH_B);
        (delayed, q)
    }
}

pub struct Shifter {
    v: [f64; PARAMS],
    sr: f32,
    lines: [InterpDelay; 2],
    /// Read phase of the first pitch tap, 0..1; the second is half a cycle away.
    phase: [f32; 2],
    dry: [InterpDelay; 2],
    hilbert: [Hilbert; 2],
    osc: [f32; 2],
    lfo: WaveLfo,
    env: EnvelopeFollower,
    env_level: f32,
    delay: [InterpDelay; 2],
    delay_feedback: [f32; 2],
    tone: [Biquad; 2],
    tone_dirty: bool,
    held: [u8; MAX_HELD_NOTES],
    held_count: usize,
    bend: f32,
    /// The MIDI pitch and where it's gliding to, in semitones.
    midi_pitch: f32,
    midi_target: f32,
    midi_seen: bool,
    /// The left and right pitch ratios or frequencies of the last control step.
    last_shift: [f32; 2],
    last_mode: usize,
    placed: bool,
}

impl Default for Shifter {
    fn default() -> Self {
        Self {
            v: values(&params()),
            sr: 48_000.0,
            lines: [InterpDelay::new(16), InterpDelay::new(16)],
            phase: [0.5; 2],
            dry: [InterpDelay::new(16), InterpDelay::new(16)],
            hilbert: [Hilbert::default(); 2],
            osc: [0.0; 2],
            lfo: WaveLfo::new(0x5F),
            env: EnvelopeFollower::new(),
            env_level: 0.0,
            delay: [InterpDelay::new(16), InterpDelay::new(16)],
            delay_feedback: [0.0; 2],
            tone: [Biquad::default(); 2],
            tone_dirty: true,
            held: [0; MAX_HELD_NOTES],
            held_count: 0,
            bend: 0.0,
            midi_pitch: 0.0,
            midi_target: 0.0,
            midi_seen: false,
            last_shift: [0.0; 2],
            last_mode: MODE_PITCH,
            placed: false,
        }
    }
}

/// The chooser's waveform at `index`.
fn wave_from(index: f64) -> Wave {
    match index.round() as i32 {
        1 => Wave::Triangle,
        2 => Wave::TriangleAnalog,
        3 => Wave::Triangle8,
        4 => Wave::Triangle16,
        5 => Wave::SawUp,
        6 => Wave::SawDown,
        7 => Wave::Rectangle,
        8 => Wave::Random,
        9 => Wave::RandomHold,
        _ => Wave::Sine,
    }
}

impl Shifter {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn mode(&self) -> usize {
        (self.v[MODE as usize].round() as usize).min(MODE_RING)
    }

    /// The Window, as a whole, even number of samples.
    fn window_samples(&self) -> f32 {
        ((self.v[WINDOW as usize] * 0.001 * self.sr as f64 * 0.5).round() * 2.0).max(4.0) as f32
    }

    fn configure_lfo(&mut self) {
        let wave = wave_from(self.v[LFO_WAVE as usize]);
        let rate = if self.flag(LFO_SYNC) {
            LfoRate::Beats(SyncRate::from_index(self.v[LFO_DIVISION as usize]).beats())
        } else {
            LfoRate::Hz(self.value(LFO_RATE))
        };
        let spinning = self.v[LFO_STEREO as usize].round() as i32 == 1
            && !matches!(wave, Wave::Random | Wave::RandomHold);
        let phase = if wave == Wave::RandomHold || spinning {
            0.0
        } else {
            self.value(LFO_PHASE) / 360.0
        };
        self.lfo.set(&WaveSettings {
            wave,
            rate,
            offset: if self.flag(LFO_SYNC) {
                self.value(LFO_OFFSET) / 360.0
            } else {
                0.0
            },
            phase,
            spin: if spinning {
                self.value(LFO_SPIN) * 0.01
            } else {
                0.0
            },
            duty: self.value(LFO_DUTY) * 0.01,
            width: self.value(LFO_WIDTH) * 0.01,
        });
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { key, .. } => {
                self.note_off(key);
                if self.held_count == MAX_HELD_NOTES {
                    self.held.copy_within(1.., 0);
                    self.held_count -= 1;
                }
                self.held[self.held_count] = key;
                self.held_count += 1;
                self.midi_target = key as f32;
                if !self.midi_seen {
                    self.midi_pitch = self.midi_target;
                    self.midi_seen = true;
                }
            }
            MidiMessage::NoteOff { key, .. } => {
                self.note_off(key);
                if self.held_count > 0 {
                    self.midi_target = self.held[self.held_count - 1] as f32;
                }
            }
            MidiMessage::PitchBend { value, .. } => self.bend = value,
            MidiMessage::AllNotesOff => self.held_count = 0,
            _ => {}
        }
    }

    fn note_off(&mut self, key: u8) {
        if let Some(at) = self.held[..self.held_count].iter().position(|k| *k == key) {
            self.held.copy_within(at + 1..self.held_count, at);
            self.held_count -= 1;
        }
    }

    /// Moves the MIDI pitch towards its target, in semitones.
    fn glide(&mut self, frames: usize) {
        let ms = self.value(GLIDE);
        if ms <= 0.0 {
            self.midi_pitch = self.midi_target;
            return;
        }
        let rate = 24.0 / (ms * 0.001 * self.sr);
        let delta = self.midi_target - self.midi_pitch;
        let step = (rate * frames as f32).max(1e-3);
        if delta.abs() <= step {
            self.midi_pitch = self.midi_target;
        } else {
            self.midi_pitch += step.copysign(delta);
        }
    }

    /// The pitch offset this step in semitones, or the frequency in Hz in Freq and Ring.
    fn shifts(&self, mode: usize, mods: [f32; 2], env: f32) -> [f32; 2] {
        let midi = self.v[PITCH_MODE as usize].round() as i32 == 1 && self.midi_seen;
        let bend = self.bend * self.value(PB_RANGE);
        let (base, amount, env_amount, spread_scale) = if mode == MODE_PITCH {
            let base = if midi {
                self.midi_pitch - 60.0 + bend
            } else {
                self.value(COARSE_ST) + self.value(FINE_CT) * 0.01
            };
            (
                base,
                self.value(LFO_AMOUNT_ST),
                self.value(ENV_AMOUNT_ST),
                0.01,
            )
        } else {
            let base = if midi {
                midi_to_hz(self.midi_pitch + bend)
            } else {
                self.value(COARSE_HZ) + self.value(FINE_HZ)
            };
            (
                base,
                self.value(LFO_AMOUNT_HZ),
                self.value(ENV_AMOUNT_HZ),
                1.0,
            )
        };
        let spread = self.value(SPREAD) * spread_scale;
        let wide = self.flag(WIDE);
        let right_spread = if wide { -spread } else { spread };
        [
            base + spread + mods[0] * amount + env * env_amount,
            base + right_spread + mods[1] * amount + env * env_amount,
        ]
    }

    fn render(&mut self, audio: AudioBuffer, tempo: f64) {
        if self.tone_dirty {
            let tone = self.value(TONE);
            for f in self.tone.iter_mut() {
                f.set(BiquadKind::LowPass, tone, 0.707, 0.0, self.sr);
            }
            self.tone_dirty = false;
        }
        self.configure_lfo();
        self.env.set_times(
            self.value(ENV_ATTACK) * 0.001,
            self.value(ENV_RELEASE) * 0.001,
        );
        let mode = self.mode();
        if mode != self.last_mode {
            // Semitones and hertz don't interpolate into one another.
            self.placed = false;
            self.last_mode = mode;
        }
        let window = self.window_samples();
        let latency = if mode == MODE_PITCH {
            window * 0.5
        } else {
            0.0
        };
        let env_on = self.flag(ENV_ON);
        let drive_on = self.flag(DRIVE_ON);
        let drive = 1.0 + 9.0 * self.value(DRIVE) * 0.01;
        let mix = self.value(MIX) * 0.01;
        let delay_on = self.flag(DELAY_ON);
        let delay_time = if self.flag(DELAY_SYNC) {
            SyncRate::from_index(self.v[DELAY_DIVISION as usize]).beats() * 60.0 / tempo.max(1.0)
        } else {
            self.v[DELAY_TIME as usize] * 0.001
        };
        let delay_samples = ((delay_time * self.sr as f64) as f32)
            .clamp(1.0, (self.delay[0].capacity() - 2) as f32);
        let delay_fb = self.value(DELAY_FEEDBACK);

        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (lc, rc) in left
            .chunks_mut(CONTROL_FRAMES)
            .zip(right.chunks_mut(CONTROL_FRAMES))
        {
            let n = lc.len();
            let mods = self.lfo.advance(n);
            if env_on {
                for x in lc.iter().zip(rc.iter()).map(|(l, r)| (l + r) * 0.5) {
                    self.env_level = self.env.process(x);
                }
            }
            self.glide(n);
            let env = if env_on { self.env_level } else { 0.0 };
            let targets = self.shifts(mode, mods, env);
            if !self.placed {
                self.last_shift = targets;
                self.placed = true;
            }
            let start = self.last_shift;
            let step = [
                (targets[0] - start[0]) / n as f32,
                (targets[1] - start[1]) / n as f32,
            ];
            for (i, (l, r)) in lc.iter_mut().zip(rc.iter_mut()).enumerate() {
                let inputs = [*l, *r];
                let mut outs = [0.0f32; 2];
                for ch in 0..2 {
                    let x = inputs[ch];
                    let shift = start[ch] + step[ch] * (i + 1) as f32;
                    let y = match mode {
                        MODE_PITCH => self.pitch_voice(ch, x, shift, window),
                        MODE_FREQ => self.freq_voice(ch, x, shift),
                        _ => self.ring_voice(ch, x, shift, drive_on, drive),
                    };
                    outs[ch] = if delay_on {
                        let d = self.delay[ch].read(delay_samples - 1.0, Interpolation::Linear);
                        self.delay_feedback[ch] = self.tone[ch].process(d);
                        self.delay[ch].write(sanitize(y + self.delay_feedback[ch] * delay_fb));
                        d
                    } else {
                        y
                    };
                }
                let dry = if latency > 0.0 {
                    self.dry[0].write(inputs[0]);
                    self.dry[1].write(inputs[1]);
                    [
                        self.dry[0].read(latency, Interpolation::Linear),
                        self.dry[1].read(latency, Interpolation::Linear),
                    ]
                } else {
                    inputs
                };
                *l = dry[0] * (1.0 - mix) + outs[0] * mix;
                *r = dry[1] * (1.0 - mix) + outs[1] * mix;
            }
            self.last_shift = targets;
        }
    }

    #[inline]
    fn pitch_voice(&mut self, ch: usize, x: f32, semitones: f32, window: f32) -> f32 {
        let ratio = 2f32.powf(semitones.clamp(-96.0, 96.0) / 12.0);
        let line = &mut self.lines[ch];
        line.write(sanitize(x));
        let phase = &mut self.phase[ch];
        if (ratio - 1.0).abs() < 1e-5 {
            // Unity: ease back to the taps' resting place, so a straight signal is one tap.
            let diff = 0.5 - *phase;
            *phase += diff.clamp(-REST_RATE, REST_RATE);
        } else {
            *phase = (*phase + (1.0 - ratio) / window).rem_euclid(1.0);
        }
        let a = *phase;
        let b = (a + 0.5).fract();
        let wa = 0.5 - 0.5 * (a * TAU).cos();
        line.read(window * a, Interpolation::Cubic) * wa
            + line.read(window * b, Interpolation::Cubic) * (1.0 - wa)
    }

    #[inline]
    fn freq_voice(&mut self, ch: usize, x: f32, hz: f32) -> f32 {
        let (i, q) = self.hilbert[ch].process(x);
        let phase = &mut self.osc[ch];
        let (sin, cos) = (*phase).sin_cos();
        *phase = (*phase + TAU * hz / self.sr).rem_euclid(TAU);
        i * cos + q * sin
    }

    #[inline]
    fn ring_voice(&mut self, ch: usize, x: f32, hz: f32, drive_on: bool, drive: f32) -> f32 {
        let phase = &mut self.osc[ch];
        let carrier = phase.cos();
        *phase = (*phase + TAU * hz / self.sr).rem_euclid(TAU);
        let y = x * carrier;
        if drive_on {
            (y * drive).tanh()
        } else {
            y
        }
    }
}

impl Processor for Shifter {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let window = (sample_rate * (MAX_WINDOW_MS + 5.0) * 0.001) as usize;
        self.lines = [InterpDelay::new(window), InterpDelay::new(window)];
        self.dry = [InterpDelay::new(window), InterpDelay::new(window)];
        let delay = (sample_rate * MAX_DELAY_SECONDS) as usize;
        self.delay = [InterpDelay::new(delay), InterpDelay::new(delay)];
        self.lfo.prepare(sample_rate);
        self.env.prepare(sample_rate);
        self.tone_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let tempo = ctx.transport.tempo;
        self.lfo
            .set_clock(&BeatClock::from_transport(ctx.transport));
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio, tempo),
            BlockStep::Midi(message) => self.midi(message),
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == TONE {
            self.tone_dirty = true;
        }
    }

    fn reset(&mut self) {
        for l in self.lines.iter_mut().chain(self.dry.iter_mut()) {
            l.clear();
        }
        self.delay.iter_mut().for_each(InterpDelay::clear);
        self.delay_feedback = [0.0; 2];
        self.tone.iter_mut().for_each(Biquad::reset);
        self.hilbert = [Hilbert::default(); 2];
        self.phase = [0.5; 2];
        self.env.reset();
        self.env_level = 0.0;
        self.placed = false;
    }

    fn latency(&self) -> u32 {
        if self.mode() == MODE_PITCH {
            (self.window_samples() * 0.5) as u32
        } else {
            0
        }
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.shifter",
    name: "Shifter",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Shifter>::default(),
};
