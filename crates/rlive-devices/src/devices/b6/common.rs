//! What the delay and modulation devices share: note lengths, dry/wet laws, delay transitions,
//! the wide LFO several of them use, and the small filters and feedback guards.

use crate::devices::b4::common::{saw_down, saw_up, sine, square, triangle, RandomWave};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::tempo::BeatClock;
use crate::dsp::util::{sanitize, Rng};
use crate::dsp::{Biquad, BiquadKind};
use rlive_core::{Item, ParamId, ParamInfo};
use std::f32::consts::{FRAC_PI_2, PI};

pub use crate::devices::b4::common::CONTROL_FRAMES;

/// The numbered sixteenth-note buttons of the synced delays.
pub const NUMBERED: [f64; 8] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 16.0];
pub const NUMBERED_LABELS: [&str; 8] = ["1", "2", "3", "4", "5", "6", "8", "16"];

/// A numbered button's length in beats, shortened or stretched by an Offset in percent.
pub fn numbered_beats(index: f64, offset_percent: f64) -> f64 {
    let i = (index.round().max(0.0) as usize).min(NUMBERED.len() - 1);
    NUMBERED[i] * 0.25 * (1.0 + offset_percent * 0.01)
}

/// Copies a parameter list's defaults into a dense array of values.
pub fn values<const N: usize>(params: &[ParamInfo]) -> [f64; N] {
    let mut v = [0.0; N];
    for p in params {
        v[p.id as usize] = p.default;
    }
    v
}

/// A knob in a layout.
pub fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

/// A toggle in a layout.
pub fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

/// A chooser in a layout.
pub fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

/// `(dry, wet)` gains for a 0..1 mix. Equal-loudness is the constant-power law.
#[inline]
pub fn mix_gains(mix: f32, equal_loudness: bool) -> (f32, f32) {
    let mix = mix.clamp(0.0, 1.0);
    if equal_loudness {
        let a = mix * FRAC_PI_2;
        (a.cos(), a.sin())
    } else {
        (1.0 - mix, mix)
    }
}

/// How a read head reaches a new delay time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Transition {
    /// Glides, so the pitch bends on the way there.
    #[default]
    Repitch,
    /// Crossfades from the old time to the new one.
    Fade,
    /// Moves there at once.
    Jump,
}

/// Length of a Fade crossfade, in seconds. The manual doesn't give one.
pub const FADE_SECONDS: f32 = 0.05;
/// The Repitch glide's time constant, in seconds, shared with the original Delay's.
pub const REPITCH_SECONDS: f32 = 0.08;
const SNAP_SAMPLES: f32 = 1e-3;

/// A delay-line read head with the Repitch, Fade and Jump behaviours.
#[derive(Clone, Copy, Debug)]
pub struct DelayHead {
    mode: Transition,
    tape: bool,
    target: f32,
    current: f32,
    tape_stage: f32,
    fading_from: f32,
    fade: f32,
    fade_step: f32,
    coeff: f32,
    placed: bool,
}

impl Default for DelayHead {
    fn default() -> Self {
        Self {
            mode: Transition::Repitch,
            tape: false,
            target: 0.0,
            current: 0.0,
            tape_stage: 0.0,
            fading_from: 0.0,
            fade: 1.0,
            fade_step: 1.0,
            coeff: 1.0,
            placed: false,
        }
    }
}

impl DelayHead {
    pub fn prepare(&mut self, sample_rate: f32) {
        self.coeff = 1.0 - (-1.0 / (REPITCH_SECONDS * sample_rate)).exp();
        self.fade_step = 1.0 / (FADE_SECONDS * sample_rate).max(1.0);
    }

    pub fn set_mode(&mut self, mode: Transition, tape: bool) {
        if mode != self.mode {
            self.fade = 1.0;
        }
        self.mode = mode;
        self.tape = tape;
    }

    /// Where the head is headed, in samples. The first call places it there outright.
    pub fn set_target(&mut self, samples: f32) {
        self.target = samples.max(0.0);
        if !self.placed {
            self.place(self.target);
        }
    }

    /// Moves the head without any transition.
    pub fn place(&mut self, samples: f32) {
        self.target = samples;
        self.current = samples;
        self.tape_stage = samples;
        self.fade = 1.0;
        self.placed = true;
    }

    /// The time the head reads at, in samples.
    pub fn value(&self) -> f32 {
        self.current
    }

    pub fn is_settled(&self) -> bool {
        self.current == self.target && self.fade >= 1.0
    }

    /// One sample of output, reading with `read(delay)`.
    #[inline]
    pub fn read(&mut self, read: impl Fn(f32) -> f32) -> f32 {
        match self.mode {
            Transition::Jump => {
                self.current = self.target;
                read(self.current)
            }
            Transition::Repitch => {
                if self.tape {
                    self.tape_stage += (self.target - self.tape_stage) * self.coeff;
                    self.current += (self.tape_stage - self.current) * self.coeff;
                } else {
                    self.current += (self.target - self.current) * self.coeff;
                }
                if (self.target - self.current).abs() < SNAP_SAMPLES {
                    self.current = self.target;
                    self.tape_stage = self.target;
                }
                read(self.current)
            }
            Transition::Fade => {
                if self.fade >= 1.0 && (self.target - self.current).abs() > 0.25 {
                    self.fading_from = self.current;
                    self.current = self.target;
                    self.fade = 0.0;
                }
                if self.fade >= 1.0 {
                    return read(self.current);
                }
                self.fade = (self.fade + self.fade_step).min(1.0);
                let g = 0.5 - 0.5 * (self.fade * PI).cos();
                read(self.fading_from) * (1.0 - g) + read(self.current) * g
            }
        }
    }
}

/// The waveforms of the wide LFO, in the order of Shifter's chooser.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Wave {
    #[default]
    Sine,
    Triangle,
    /// A low-passed rectangle, so its shape and level change with the rate.
    TriangleAnalog,
    Triangle8,
    Triangle16,
    SawUp,
    SawDown,
    Rectangle,
    /// A new random value each cycle, glided to.
    Random,
    /// A new random value each cycle, held.
    RandomHold,
    /// A new random value each control step.
    Noise,
}

impl Wave {
    pub const ALL: [Wave; 11] = [
        Wave::Sine,
        Wave::Triangle,
        Wave::TriangleAnalog,
        Wave::Triangle8,
        Wave::Triangle16,
        Wave::SawUp,
        Wave::SawDown,
        Wave::Rectangle,
        Wave::Random,
        Wave::RandomHold,
        Wave::Noise,
    ];

    pub fn is_random(self) -> bool {
        matches!(self, Wave::Random | Wave::RandomHold | Wave::Noise)
    }
}

/// Stretches a cycle's first half to `knee` of its length. `duty` in -1..1: 1 compresses the
/// cycle towards its start, -1 towards its end, 0 leaves it alone.
#[inline]
pub fn duty_warp(p: f32, duty: f32) -> f32 {
    if duty == 0.0 {
        return p;
    }
    let knee = (0.5 * (1.0 - duty.clamp(-0.98, 0.98))).clamp(0.01, 0.99);
    if p < knee {
        0.5 * p / knee
    } else {
        0.5 + 0.5 * (p - knee) / (1.0 - knee)
    }
}

/// How long [`Wave::TriangleAnalog`]'s low-pass takes to respond, in seconds.
const ANALOG_SECONDS: f32 = 0.08;

/// What a [`WaveLfo`] should do.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WaveSettings {
    pub wave: Wave,
    pub rate: LfoRate,
    /// Start of the cycle, as a fraction of one, for both channels.
    pub offset: f32,
    /// How far the right channel is ahead, as a fraction of a cycle.
    pub phase: f32,
    /// How much faster the right channel runs, as a ratio.
    pub spin: f32,
    /// See [`duty_warp`]. Has no effect on the random waves.
    pub duty: f32,
    /// [`Wave::RandomHold`]'s stereo width, 0..1. At 1 the right is the left upside down.
    pub width: f32,
}

impl Default for WaveSettings {
    fn default() -> Self {
        Self {
            wave: Wave::Sine,
            rate: LfoRate::Hz(1.0),
            offset: 0.0,
            phase: 0.0,
            spin: 0.0,
            duty: 0.0,
            width: 0.0,
        }
    }
}

/// A stereo LFO in `-1..1`, built on [`Lfo`] for its phase handling, with the extra shapes and
/// the Duty Cycle these devices ask for.
#[derive(Clone, Debug)]
pub struct WaveLfo {
    lfo: Lfo,
    settings: WaveSettings,
    random: [RandomWave; 2],
    noise: Rng,
    analog: [f32; 2],
    sample_rate: f32,
}

impl WaveLfo {
    pub fn new(seed: u32) -> Self {
        Self {
            lfo: Lfo::new(seed),
            settings: WaveSettings::default(),
            // The same seed in both, so with no phase between them the channels agree.
            random: [RandomWave::new(seed); 2],
            noise: Rng::new(seed.wrapping_add(7)),
            analog: [0.0; 2],
            sample_rate: 48_000.0,
        }
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.lfo.prepare(sample_rate);
        self.sample_rate = sample_rate.max(1.0) as f32;
    }

    pub fn set(&mut self, s: &WaveSettings) {
        self.settings = *s;
        self.lfo.set(&LfoSettings {
            rate: s.rate,
            offset: s.offset,
            phase: s.phase,
            spin: s.spin,
            ..Default::default()
        });
    }

    pub fn set_clock(&mut self, clock: &BeatClock) {
        self.lfo.set_clock(clock);
    }

    pub fn retrigger(&mut self) {
        self.lfo.retrigger();
    }

    fn value(&mut self, channel: usize, phase: f32, frames: usize) -> f32 {
        let s = self.settings;
        let random = s.wave.is_random();
        let p = if random {
            phase
        } else {
            duty_warp(phase, s.duty)
        };
        match s.wave {
            Wave::Sine => sine(p),
            Wave::Triangle => triangle(p),
            Wave::Triangle8 => triangle((p * 8.0).floor() / 8.0),
            Wave::Triangle16 => triangle((p * 16.0).floor() / 16.0),
            Wave::SawUp => saw_up(p),
            Wave::SawDown => saw_down(p),
            Wave::Rectangle => square(p),
            Wave::TriangleAnalog => {
                let k = 1.0 - (-(frames as f32) / (ANALOG_SECONDS * self.sample_rate)).exp();
                self.analog[channel] += (square(p) - self.analog[channel]) * k;
                self.analog[channel]
            }
            Wave::Random | Wave::RandomHold => {
                self.random[channel].update(phase);
                if s.wave == Wave::Random {
                    self.random[channel].glide(phase)
                } else {
                    self.random[channel].held()
                }
            }
            Wave::Noise => self.noise.bipolar(),
        }
    }

    /// The output as `[left, right]`, then `frames` samples of progress.
    pub fn advance(&mut self, frames: usize) -> [f32; 2] {
        let [pl, pr] = self.lfo.advance_phases(frames);
        let left = self.value(0, pl, frames);
        let right = if self.settings.wave == Wave::RandomHold {
            self.random[1].update(pr);
            left * (1.0 - 2.0 * self.settings.width.clamp(0.0, 1.0))
        } else {
            self.value(1, pr, frames)
        };
        [left, right]
    }
}

/// A low-pass and a high-pass linked into a band: `width` octaves around `center`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinkedBand {
    lp: Biquad,
    hp: Biquad,
}

impl LinkedBand {
    pub fn set(&mut self, center: f32, width_octaves: f32, sample_rate: f32) {
        let half = 2f32.powf(width_octaves.max(0.05) * 0.5);
        self.lp.set(
            BiquadKind::LowPass,
            center * half,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            sample_rate,
        );
        self.hp.set(
            BiquadKind::HighPass,
            center / half,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            sample_rate,
        );
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.hp.process(self.lp.process(x))
    }

    pub fn reset(&mut self) {
        self.lp.reset();
        self.hp.reset();
    }
}

/// Splits a signal in two around a frequency so that the halves always add back to the input.
#[derive(Clone, Copy, Debug, Default)]
pub struct BandSplit {
    lp: Biquad,
}

impl BandSplit {
    pub fn set(&mut self, hz: f32, sample_rate: f32) {
        self.lp.set(
            BiquadKind::LowPass,
            hz,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            sample_rate,
        );
    }

    /// `(low, high)`.
    #[inline]
    pub fn split(&mut self, x: f32) -> (f32, f32) {
        let low = self.lp.process(x);
        (low, x - low)
    }

    pub fn reset(&mut self) {
        self.lp.reset();
    }
}

/// The Warmth control: a soft curve, then a low-pass that closes as it is turned up.
#[derive(Clone, Copy, Debug, Default)]
pub struct Warmth {
    state: f32,
}

impl Warmth {
    #[inline]
    pub fn process(&mut self, x: f32, amount: f32, sample_rate: f32) -> f32 {
        if amount <= 0.0 {
            return x;
        }
        let shaped = lerp_drive(x, amount);
        let hz = 20_000.0 * (1.0 - 0.8 * amount.clamp(0.0, 1.0));
        let k = 1.0 - (-std::f32::consts::TAU * hz / sample_rate).exp();
        self.state += (shaped - self.state) * k;
        sanitize(self.state)
    }

    pub fn reset(&mut self) {
        self.state = 0.0;
    }
}

#[inline]
fn lerp_drive(x: f32, amount: f32) -> f32 {
    let drive = 1.0 + 4.0 * amount;
    (x * drive).tanh() / drive.tanh().max(1e-3)
}

/// Keeps a feedback path finite: untouched in normal use, saturating above full scale.
#[inline]
pub fn guard(x: f32) -> f32 {
    let x = sanitize(x);
    if x.abs() < 1.5 {
        x
    } else {
        x.signum() * (1.5 + (x.abs() - 1.5).tanh())
    }
}
