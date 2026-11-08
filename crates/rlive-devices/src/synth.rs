//! "Poly" — a subtractive polyphonic synthesizer.

use crate::dsp::{Adsr, FilterMode, Oscillator, Svf, Waveform};
use rlive_core::{
    db_to_gain, note_to_hz, BlockStep, MidiMessage, ModMode, ParamInfo, ProcessContext, Processor,
    Tail,
};

const VOICES: usize = 16;
const CONTROL_RATE: usize = 16;

pub mod p {
    pub const OSC1_WAVE: u32 = 0;
    pub const OSC2_WAVE: u32 = 1;
    pub const OSC2_SEMI: u32 = 2;
    pub const OSC2_FINE: u32 = 3;
    pub const OSC_MIX: u32 = 4;
    pub const NOISE: u32 = 5;
    pub const CUTOFF: u32 = 6;
    pub const RESONANCE: u32 = 7;
    pub const FILTER_MODE: u32 = 8;
    pub const FILTER_ENV: u32 = 9;
    pub const KEY_TRACK: u32 = 10;
    pub const AMP_A: u32 = 11;
    pub const AMP_D: u32 = 12;
    pub const AMP_S: u32 = 13;
    pub const AMP_R: u32 = 14;
    pub const FLT_A: u32 = 15;
    pub const FLT_D: u32 = 16;
    pub const FLT_S: u32 = 17;
    pub const FLT_R: u32 = 18;
    pub const VELOCITY: u32 = 19;
    pub const GLIDE: u32 = 20;
    pub const VOLUME: u32 = 21;
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(p::OSC1_WAVE, "Osc 1", &Waveform::LABELS, 0),
        ParamInfo::stepped(p::OSC2_WAVE, "Osc 2", &Waveform::LABELS, 1),
        ParamInfo::continuous(p::OSC2_SEMI, "Osc 2 Semi", -24.0, 24.0, 0.0, "st"),
        ParamInfo::continuous(p::OSC2_FINE, "Osc 2 Fine", -50.0, 50.0, 7.0, "ct"),
        ParamInfo::continuous(p::OSC_MIX, "Osc Mix", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(p::NOISE, "Noise", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(p::CUTOFF, "Cutoff", 20.0, 20_000.0, 2_000.0, "Hz", 3.0),
        ParamInfo::continuous(p::RESONANCE, "Resonance", 0.0, 1.0, 0.2, ""),
        ParamInfo::stepped(p::FILTER_MODE, "Filter", &FilterMode::LABELS, 0),
        ParamInfo::continuous(p::FILTER_ENV, "Env Amount", -8.0, 8.0, 2.0, "oct"),
        ParamInfo::continuous(p::KEY_TRACK, "Key Track", 0.0, 1.0, 0.5, ""),
        ParamInfo::skewed(p::AMP_A, "Attack", 0.5, 5_000.0, 2.0, "ms", 3.0),
        ParamInfo::skewed(p::AMP_D, "Decay", 1.0, 10_000.0, 300.0, "ms", 3.0),
        ParamInfo::continuous(p::AMP_S, "Sustain", 0.0, 1.0, 0.7, ""),
        ParamInfo::skewed(p::AMP_R, "Release", 1.0, 10_000.0, 250.0, "ms", 3.0),
        ParamInfo::skewed(p::FLT_A, "F Attack", 0.5, 5_000.0, 2.0, "ms", 3.0),
        ParamInfo::skewed(p::FLT_D, "F Decay", 1.0, 10_000.0, 400.0, "ms", 3.0),
        ParamInfo::continuous(p::FLT_S, "F Sustain", 0.0, 1.0, 0.2, ""),
        ParamInfo::skewed(p::FLT_R, "F Release", 1.0, 10_000.0, 300.0, "ms", 3.0),
        ParamInfo::continuous(p::VELOCITY, "Velocity", 0.0, 1.0, 0.6, ""),
        ParamInfo::skewed(p::GLIDE, "Glide", 0.0, 2_000.0, 0.0, "ms", 3.0),
        ParamInfo::continuous(p::VOLUME, "Volume", -48.0, 6.0, -9.0, "dB")
            .with_mod_mode(ModMode::Scale),
    ]
}

#[derive(Clone, Copy, Default)]
struct Voice {
    channel: u8,
    key: u8,
    gate: bool,
    velocity: f32,
    age: u64,
    pitch: f32,
    target_pitch: f32,
    osc1: Oscillator,
    osc2: Oscillator,
    amp: Adsr,
    flt: Adsr,
    filter: Svf,
    control_counter: usize,
}

pub struct PolySynth {
    values: Vec<f64>,
    sr: f32,
    voices: [Voice; VOICES],
    clock: u64,
    noise_state: u32,
    bend: f32,
    last_pitch: f32,
    dirty: bool,
}

impl Default for PolySynth {
    fn default() -> Self {
        Self::new()
    }
}

impl PolySynth {
    pub fn new() -> Self {
        let values = params().iter().map(|p| p.default).collect();
        Self {
            values,
            sr: 48_000.0,
            voices: [Voice::default(); VOICES],
            clock: 0,
            noise_state: 0x1234_5678,
            bend: 0.0,
            last_pitch: 60.0,
            dirty: true,
        }
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize] as f32
    }

    fn update_envelopes(&mut self) {
        let (a, d, s, r) = (
            self.v(p::AMP_A),
            self.v(p::AMP_D),
            self.v(p::AMP_S),
            self.v(p::AMP_R),
        );
        let (fa, fd, fs, fr) = (
            self.v(p::FLT_A),
            self.v(p::FLT_D),
            self.v(p::FLT_S),
            self.v(p::FLT_R),
        );
        let sr = self.sr;
        for v in self.voices.iter_mut() {
            v.amp.set(a, d, s, r, sr);
            v.flt.set(fa, fd, fs, fr, sr);
        }
        self.dirty = false;
    }

    fn note_on(&mut self, channel: u8, key: u8, velocity: f32) {
        self.clock += 1;
        // Reuse a voice already playing this key, else a free one, else the oldest.
        let idx = self
            .voices
            .iter()
            .position(|v| v.channel == channel && v.key == key && v.amp.is_active())
            .or_else(|| self.voices.iter().position(|v| !v.amp.is_active()))
            .unwrap_or_else(|| {
                // Prefer stealing released voices.
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| (v.gate, v.age))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        let glide = self.v(p::GLIDE) > 0.5;
        let v = &mut self.voices[idx];
        let was_active = v.amp.is_active();
        v.channel = channel;
        v.key = key;
        v.gate = true;
        v.velocity = velocity;
        v.age = self.clock;
        v.target_pitch = key as f32;
        v.pitch = if glide { self.last_pitch } else { key as f32 };
        if !was_active {
            v.osc1.phase = 0.0;
            v.osc2.phase = 0.25;
            v.filter.reset();
            v.amp.level = 0.0;
            v.flt.level = 0.0;
        }
        v.amp.gate_on();
        v.flt.gate_on();
        v.control_counter = 0;
        self.last_pitch = key as f32;
    }

    /// A MIDI channel holds one note per key, so channel and key name a voice: MPE notes that
    /// share a key stay apart on their member channels.
    fn note_off(&mut self, channel: u8, key: u8) {
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.channel == channel && v.key == key && v.gate)
        {
            v.gate = false;
            v.amp.gate_off();
            v.flt.gate_off();
        }
    }

    fn all_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.gate = false;
            v.amp.gate_off();
            v.flt.gate_off();
        }
    }

    #[inline]
    fn noise(&mut self) -> f32 {
        // xorshift32
        let mut x = self.noise_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise_state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.dirty {
            self.update_envelopes();
        }
        let sr = self.sr;
        let w1 = Waveform::from_index(self.values[p::OSC1_WAVE as usize]);
        let w2 = Waveform::from_index(self.values[p::OSC2_WAVE as usize]);
        let detune = self.v(p::OSC2_SEMI) + self.v(p::OSC2_FINE) / 100.0;
        let mix = self.v(p::OSC_MIX);
        let noise_amt = self.v(p::NOISE);
        let cutoff = self.v(p::CUTOFF);
        let res = self.v(p::RESONANCE);
        let mode = FilterMode::from_index(self.values[p::FILTER_MODE as usize]);
        let env_amt = self.v(p::FILTER_ENV);
        let key_track = self.v(p::KEY_TRACK);
        let vel_sens = self.v(p::VELOCITY);
        let volume = db_to_gain(self.v(p::VOLUME));
        let glide_ms = self.v(p::GLIDE);
        let glide_coeff = if glide_ms > 0.5 {
            1.0 - (-1.0 / (glide_ms * 0.001 * sr)).exp()
        } else {
            1.0
        };
        let bend = self.bend * 2.0;

        for vi in 0..VOICES {
            if !self.voices[vi].amp.is_active() {
                continue;
            }
            for i in 0..left.len() {
                let n = self.noise() * noise_amt;
                let v = &mut self.voices[vi];
                v.pitch += (v.target_pitch - v.pitch) * glide_coeff;
                let fenv = v.flt.next();
                if v.control_counter == 0 {
                    let oct = env_amt * fenv + key_track * (v.pitch - 60.0) / 12.0;
                    v.filter.set(cutoff * 2f32.powf(oct), res, sr);
                }
                v.control_counter = (v.control_counter + 1) % CONTROL_RATE;
                let f1 = note_to_hz(v.pitch + bend);
                let f2 = note_to_hz(v.pitch + bend + detune);
                let osc = v.osc1.next(w1, f1 / sr) * (1.0 - mix) + v.osc2.next(w2, f2 / sr) * mix;
                let x = v.filter.process(osc + n, mode);
                let amp = v.amp.next();
                let vel = 1.0 - vel_sens + vel_sens * v.velocity;
                let s = x * amp * vel * volume;
                left[i] += s;
                right[i] += s;
                if !v.amp.is_active() {
                    break;
                }
            }
        }
    }
}

impl Processor for PolySynth {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate as f32;
        self.dirty = true;
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
            BlockStep::Midi(MidiMessage::AllNotesOff) => self.all_off(),
            BlockStep::Midi(MidiMessage::PitchBend { value, .. }) => self.bend = value,
            BlockStep::Midi(MidiMessage::ControlChange { cc: 120, .. }) => self.reset(),
            BlockStep::Midi(
                MidiMessage::ControlChange { .. }
                | MidiMessage::NoteExpr { .. }
                | MidiMessage::ChannelPressure { .. }
                | MidiMessage::ProgramChange { .. },
            ) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        for v in self.voices.iter_mut() {
            *v = Voice::default();
        }
        self.dirty = true;
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}
