//! "Sampler" — plays a WAV file chromatically or as a one-shot drum hit.

use crate::dsp::Adsr;
use crate::SampleBuffer;
use rlive_core::{
    db_to_gain, BlockStep, MidiMessage, ModMode, ParamInfo, ProcessContext, Processor, Resource,
    Tail,
};
use std::sync::Arc;

const VOICES: usize = 16;
/// The one resource slot: the sample.
const SAMPLE_SLOT: u32 = 0;

pub mod p {
    pub const ROOT: u32 = 0;
    pub const TUNE: u32 = 1;
    pub const VOLUME: u32 = 2;
    pub const ATTACK: u32 = 3;
    pub const RELEASE: u32 = 4;
    pub const ONE_SHOT: u32 = 5;
    pub const START: u32 = 6;
    pub const VELOCITY: u32 = 7;
    pub const PAN: u32 = 8;
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(p::ROOT, "Root Key", 0.0, 127.0, 60.0, ""),
        ParamInfo::continuous(p::TUNE, "Transpose", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(p::VOLUME, "Volume", -48.0, 12.0, -3.0, "dB")
            .with_mod_mode(ModMode::Scale),
        ParamInfo::skewed(p::ATTACK, "Attack", 0.5, 2_000.0, 0.5, "ms", 3.0),
        ParamInfo::skewed(p::RELEASE, "Release", 1.0, 5_000.0, 50.0, "ms", 3.0),
        ParamInfo::toggle(p::ONE_SHOT, "One-shot", false),
        ParamInfo::continuous(p::START, "Start", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(p::VELOCITY, "Velocity", 0.0, 1.0, 0.8, ""),
        ParamInfo::continuous(p::PAN, "Pan", -1.0, 1.0, 0.0, ""),
    ]
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    channel: u8,
    key: u8,
    pos: f64,
    step: f64,
    gain: f32,
    env: Adsr,
    age: u64,
}

pub struct Sampler {
    values: Vec<f64>,
    sample: Option<Arc<SampleBuffer>>,
    sr: f64,
    voices: [Voice; VOICES],
    clock: u64,
}

impl Sampler {
    pub fn new(sample: Option<Arc<SampleBuffer>>) -> Self {
        Self {
            values: params().iter().map(|p| p.default).collect(),
            sample,
            sr: 48_000.0,
            voices: [Voice::default(); VOICES],
            clock: 0,
        }
    }

    fn v(&self, id: u32) -> f64 {
        self.values[id as usize]
    }

    fn note_on(&mut self, channel: u8, key: u8, velocity: f32) {
        let Some(sample) = &self.sample else { return };
        self.clock += 1;
        let semis = key as f64 - self.v(p::ROOT) + self.v(p::TUNE);
        let step = 2f64.powf(semis / 12.0) * sample.sample_rate / self.sr;
        let start = self.v(p::START).clamp(0.0, 1.0) * sample.len() as f64;
        let vel_sens = self.v(p::VELOCITY) as f32;
        let gain = 1.0 - vel_sens + vel_sens * velocity;
        let (a, r, sr) = (
            self.v(p::ATTACK) as f32,
            self.v(p::RELEASE) as f32,
            self.sr as f32,
        );
        let idx = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        let v = &mut self.voices[idx];
        v.active = true;
        v.channel = channel;
        v.key = key;
        v.pos = start;
        v.step = step;
        v.gain = gain;
        v.age = self.clock;
        v.env.set(a, 1.0, 1.0, r, sr);
        v.env.level = 0.0;
        v.env.gate_on();
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        if self.v(p::ONE_SHOT) >= 0.5 {
            return;
        }
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && v.channel == channel && v.key == key)
        {
            v.env.gate_off();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let Some(sample) = &self.sample else { return };
        let n = sample.len();
        if n < 2 {
            return;
        }
        let vol = db_to_gain(self.values[p::VOLUME as usize] as f32);
        let pan = self.values[p::PAN as usize] as f32;
        let (pl, pr) = ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0));
        for v in self.voices.iter_mut().filter(|v| v.active) {
            for i in 0..left.len() {
                let idx = v.pos as usize;
                if idx + 1 >= n {
                    v.active = false;
                    break;
                }
                let frac = (v.pos - idx as f64) as f32;
                let l = sample.left[idx] + (sample.left[idx + 1] - sample.left[idx]) * frac;
                let r = sample.right[idx] + (sample.right[idx + 1] - sample.right[idx]) * frac;
                let e = v.env.next() * v.gain * vol;
                left[i] += l * e * pl;
                right[i] += r * e * pr;
                v.pos += v.step;
                if !v.env.is_active() {
                    v.active = false;
                    break;
                }
            }
        }
    }
}

impl Processor for Sampler {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio.left, audio.right),
            BlockStep::Midi(MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                ..
            }) => self.note_on(channel, key, velocity),
            BlockStep::Midi(MidiMessage::NoteOff { channel, key, .. }) => {
                self.note_off(channel, key)
            }
            BlockStep::Midi(MidiMessage::AllNotesOff) => {
                for v in self.voices.iter_mut() {
                    v.env.gate_off();
                }
            }
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for v in self.voices.iter_mut() {
            v.active = false;
        }
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot != SAMPLE_SLOT {
            return Some(resource);
        }
        match resource.downcast::<SampleBuffer>() {
            Ok(sample) => {
                let old_rate = self.sample.as_ref().map(|s| s.sample_rate);
                let new_rate = sample.sample_rate;
                let old = self.sample.replace(sample);
                // Voices keep their place, and their pitch even if the sample rate differs.
                if let Some(old_rate) = old_rate.filter(|r| *r > 0.0) {
                    for v in self.voices.iter_mut().filter(|v| v.active) {
                        v.step *= new_rate / old_rate;
                    }
                }
                old.map(|o| o as Resource)
            }
            Err(refused) => Some(refused),
        }
    }
}
