//! What the Drum Synths share: a one-shot envelope, the fade a retriggered hit gets, and the
//! processor around a voice.

use crate::devices::b12::common::limit;
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, ProcessContext, Processor,
    Section, Tail,
};
use std::f32::consts::TAU;

pub const fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

pub const fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

pub const fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

pub const fn slider(id: u32) -> Item {
    Item::Slider(ParamId(id))
}

/// Decay time every Drum Synth has, in seconds of fade to silence. The manual gives no figure.
pub fn decay_param(id: u32, default_ms: f64) -> ParamInfo {
    ParamInfo::skewed(id, "Decay", 10.0, 3000.0, default_ms, "ms", 3.0)
}

pub fn volume_param(id: u32) -> ParamInfo {
    ParamInfo::continuous(id, "Volume", -48.0, 6.0, 0.0, "dB")
}

/// The display of a Drum Synth: the envelope, and a click in its upper half to hear it.
pub fn display_section() -> Section {
    Section {
        title: "",
        items: vec![Item::Panel("ds_display")],
    }
}

pub fn desc(params: Vec<ParamInfo>, layout: Vec<Section>) -> DeviceDesc {
    DeviceDesc {
        params,
        ports: Ports::default(),
        layout,
        ..Default::default()
    }
}

/// How quickly the previous hit fades when a new one starts, in samples.
const RETRIGGER_FADE: u32 = 128;
/// A hit is over below this level.
const SILENCE: f32 = 1e-4;

/// A decaying envelope: one attack ramp, then an exponential fall to -80 dB over `time`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OneShot {
    pub level: f32,
    coef: f32,
    attack_step: f32,
    rising: bool,
}

impl OneShot {
    pub fn trigger(&mut self, attack_seconds: f32, decay_seconds: f32, sample_rate: f32) {
        self.rising = attack_seconds > 0.0;
        self.level = if self.rising { 0.0 } else { 1.0 };
        self.attack_step = 1.0 / (attack_seconds * sample_rate).max(1.0);
        self.coef = decay_coefficient(decay_seconds, sample_rate);
    }

    pub fn off(&mut self) {
        self.level = 0.0;
        self.rising = false;
    }

    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f32 {
        if self.rising {
            self.level += self.attack_step;
            if self.level >= 1.0 {
                self.level = 1.0;
                self.rising = false;
            }
        } else {
            self.level *= self.coef;
            if self.level < SILENCE {
                self.level = 0.0;
            }
        }
        self.level
    }

    pub fn is_active(&self) -> bool {
        self.rising || self.level > 0.0
    }
}

/// The per-sample factor that falls by 80 dB over `seconds`.
#[inline]
pub fn decay_coefficient(seconds: f32, sample_rate: f32) -> f32 {
    (-(4.0 * std::f32::consts::LN_10) / (seconds.max(1e-3) * sample_rate)).exp()
}

/// A one-pole high-pass, for a little DC blocking and for noise colour.
#[derive(Clone, Copy, Debug, Default)]
pub struct HighPass {
    coef: f32,
    last_in: f32,
    last_out: f32,
}

impl HighPass {
    pub fn set(&mut self, hz: f32, sample_rate: f32) {
        self.coef = (-TAU * hz.max(1.0) / sample_rate).exp();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.coef * (self.last_out + x - self.last_in);
        self.last_in = x;
        self.last_out = sanitize(y);
        self.last_out
    }

    pub fn reset(&mut self) {
        self.last_in = 0.0;
        self.last_out = 0.0;
    }
}

/// White noise, or pink by Paul Kellet's three-pole economy filter.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    rng: Rng,
    b: [f32; 3],
}

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: Rng::new(seed),
            b: [0.0; 3],
        }
    }

    #[inline]
    pub fn white(&mut self) -> f32 {
        self.rng.bipolar()
    }

    #[inline]
    pub fn pink(&mut self) -> f32 {
        let w = self.white();
        self.b[0] = 0.99765 * self.b[0] + w * 0.099046;
        self.b[1] = 0.96300 * self.b[1] + w * 0.296_516_4;
        self.b[2] = 0.57000 * self.b[2] + w * 1.052_691_3;
        (self.b[0] + self.b[1] + self.b[2] + w * 0.1848) * 0.18
    }

    pub fn reset(&mut self) {
        self.b = [0.0; 3];
    }
}

/// One Drum Synth's sound.
pub trait DrumVoice: Send {
    /// Take a parameter change.
    fn set(&mut self, id: u32, value: f64);
    /// Start a hit. Velocity is 0 to 1.
    fn trigger(&mut self, velocity: f32, sample_rate: f32);
    /// The next stereo frame.
    fn next(&mut self, sample_rate: f32) -> [f32; 2];
    /// Whether anything is still sounding.
    fn is_active(&self) -> bool;
    /// Silence at once.
    fn silence(&mut self);
    /// Output level, linear, for the Volume control.
    fn volume(&self) -> f32;
    /// How long a hit can ring on after it starts, in seconds.
    fn tail_seconds(&self) -> f32;
}

/// The processor shared by every Drum Synth.
pub struct DrumSynth<V: DrumVoice> {
    voice: V,
    sample_rate: f32,
    fade: u32,
    held: [f32; 2],
}

impl<V: DrumVoice> DrumSynth<V> {
    pub fn new(voice: V) -> Self {
        Self {
            voice,
            sample_rate: 48_000.0,
            fade: 0,
            held: [0.0; 2],
        }
    }

    pub fn voice(&self) -> &V {
        &self.voice
    }

    fn trigger(&mut self, velocity: f32) {
        if self.voice.is_active() {
            // Whatever was sounding fades out under the new hit, so a retrigger doesn't click.
            self.fade = RETRIGGER_FADE;
        }
        self.voice.trigger(velocity, self.sample_rate);
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.voice.is_active() && self.fade == 0 {
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let [a, b] = self.voice.next(self.sample_rate);
            let vol = self.voice.volume();
            let (mut a, mut b) = (a * vol, b * vol);
            if self.fade > 0 {
                let t = self.fade as f32 / RETRIGGER_FADE as f32;
                a += self.held[0] * t;
                b += self.held[1] * t;
                self.fade -= 1;
            } else {
                self.held = [0.0; 2];
            }
            if self.fade == 0 {
                self.held = [a, b];
            }
            *l += limit(a);
            *r += limit(b);
        }
    }
}

impl<V: DrumVoice> Processor for DrumSynth<V> {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0) as f32;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.voice.set(id, value),
            BlockStep::Midi(MidiMessage::NoteOn { velocity, .. }) => self.trigger(velocity),
            BlockStep::Midi(MidiMessage::AllNotesOff) => self.voice.silence(),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(a) => self.render(a.left, a.right),
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.voice.set(id, value);
    }

    fn reset(&mut self) {
        self.voice.silence();
        self.fade = 0;
        self.held = [0.0; 2];
    }

    fn tail(&self) -> Tail {
        Tail::Frames((self.voice.tail_seconds() * self.sample_rate) as u32)
    }
}

/// Reads a dB parameter value into a linear gain.
#[inline]
pub fn gain(db: f64) -> f32 {
    db_to_gain(db as f32)
}
