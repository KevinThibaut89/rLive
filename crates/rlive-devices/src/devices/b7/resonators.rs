//! Resonators: five parallel tuned feedback combs behind one input filter (manual pages 646-647).
//!
//! The manual gives the four filter types, Modes A and B, the Note range (C-1 to C5), the +-24
//! semitone and +-21 scale degree transpositions, Const, Width's behaviour and which resonator
//! hears which channel. It gives no time, cent or gain ranges and doesn't say what Modes A and B
//! are: here A is a plain comb (every harmonic) and B a negative one a half-period long (odd
//! harmonics only, sitting an octave down from where a same-length plain comb would ring, so Note
//! keeps naming the fundamental). The decay law "low notes last longer" is `ring = Decay x
//! REFERENCE_HZ / hz` below.

use super::tank::soft_limit;
use crate::devices::b1::common::transpose_degrees;
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::util::{midi_to_hz, sanitize};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Resource, ScaleInfo, Section, Status, Tail, TransportInfo, TuningTable, TUNING_SLOT,
};
use std::f32::consts::TAU;
use std::sync::Arc;

pub const MODE: u32 = 0;
pub const FILTER_TYPE: u32 = 1;
pub const FILTER_FREQ: u32 = 2;
pub const DECAY: u32 = 3;
pub const CONST: u32 = 4;
pub const COLOR: u32 = 5;
pub const NOTE: u32 = 6;
pub const FINE: u32 = 7;
pub const GAIN1: u32 = 8;
pub const ON1: u32 = 9;
pub const ON2: u32 = 10;
pub const PITCH2: u32 = 11;
pub const FINE2: u32 = 12;
pub const GAIN2: u32 = 13;
pub const ON3: u32 = 14;
pub const PITCH3: u32 = 15;
pub const FINE3: u32 = 16;
pub const GAIN3: u32 = 17;
pub const ON4: u32 = 18;
pub const PITCH4: u32 = 19;
pub const FINE4: u32 = 20;
pub const GAIN4: u32 = 21;
pub const ON5: u32 = 22;
pub const PITCH5: u32 = 23;
pub const FINE5: u32 = 24;
pub const GAIN5: u32 = 25;
pub const WIDTH: u32 = 26;
pub const OUTPUT_GAIN: u32 = 27;
pub const MIX: u32 = 28;
pub const USE_SCALE: u32 = 29;
const PARAMS: usize = 30;

pub const MODE_A: usize = 0;
pub const MODE_B: usize = 1;

/// The pitch at which Decay is the actual ring time when Const is off.
pub const REFERENCE_HZ: f32 = 261.63;
/// In scale-aware mode, Note steps through degrees from this Note value, which names the root.
pub const SCALE_ROOT_NOTE: f64 = 60.0;
/// The MIDI pitch of octave 2 of a scale's root: `SCALE_OCTAVE_BASE + root`.
pub const SCALE_OCTAVE_BASE: i32 = 48;
const MAX_LOOP_GAIN: f32 = 0.9998;
const LONGEST_NOTE: f32 = 12.0;
const CONTROL_FRAMES: usize = 16;

/// `[On, Pitch, Fine, Gain]` of the second to fifth resonators.
pub const VOICE_PARAMS: [[u32; 4]; 4] = [
    [ON2, PITCH2, FINE2, GAIN2],
    [ON3, PITCH3, FINE3, GAIN3],
    [ON4, PITCH4, FINE4, GAIN4],
    [ON5, PITCH5, FINE5, GAIN5],
];

fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::stepped(MODE, "Mode", &["A", "B"], MODE_A),
        ParamInfo::stepped(
            FILTER_TYPE,
            "Filter Type",
            &["Low-Pass", "Band-Pass", "High-Pass", "Notch"],
            0,
        ),
        ParamInfo::skewed(
            FILTER_FREQ,
            "Frequency",
            20.0,
            20_000.0,
            20_000.0,
            "Hz",
            3.0,
        ),
        ParamInfo::skewed(DECAY, "Decay", 1.0, 30_000.0, 3_000.0, "ms", 4.0),
        ParamInfo::toggle(CONST, "Const", false),
        ParamInfo::continuous(COLOR, "Color", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(NOTE, "Note", 12.0, 84.0, 60.0, ""),
        ParamInfo::continuous(FINE, "Fine", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(GAIN1, "Gain I", 0.0, 100.0, 100.0, "%"),
        ParamInfo::toggle(ON1, "On I", true),
        ParamInfo::toggle(ON2, "On II", true),
        ParamInfo::continuous(PITCH2, "Pitch II", -24.0, 24.0, 12.0, "st"),
        ParamInfo::continuous(FINE2, "Fine II", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(GAIN2, "Gain II", 0.0, 100.0, 60.0, "%"),
        ParamInfo::toggle(ON3, "On III", true),
        ParamInfo::continuous(PITCH3, "Pitch III", -24.0, 24.0, 7.0, "st"),
        ParamInfo::continuous(FINE3, "Fine III", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(GAIN3, "Gain III", 0.0, 100.0, 60.0, "%"),
        ParamInfo::toggle(ON4, "On IV", true),
        ParamInfo::continuous(PITCH4, "Pitch IV", -24.0, 24.0, 4.0, "st"),
        ParamInfo::continuous(FINE4, "Fine IV", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(GAIN4, "Gain IV", 0.0, 100.0, 40.0, "%"),
        ParamInfo::toggle(ON5, "On V", true),
        ParamInfo::continuous(PITCH5, "Pitch V", -24.0, 24.0, -12.0, "st"),
        ParamInfo::continuous(FINE5, "Fine V", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(GAIN5, "Gain V", 0.0, 100.0, 40.0, "%"),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(OUTPUT_GAIN, "Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 100.0, 100.0, "%"),
    ];
    p.push(ParamInfo::toggle(USE_SCALE, "Use Current Scale", false));
    p
}

fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

fn layout() -> Vec<Section> {
    let mut sections = vec![
        Section {
            title: "Filter",
            items: vec![menu(FILTER_TYPE), knob(FILTER_FREQ)],
        },
        Section {
            title: "Resonator I",
            items: vec![
                menu(MODE),
                knob(DECAY),
                toggle(CONST),
                knob(COLOR),
                toggle(ON1),
                knob(NOTE),
                knob(FINE),
                knob(GAIN1),
                toggle(USE_SCALE),
            ],
        },
    ];
    for (title, [on, pitch, fine, gain]) in ["II", "III", "IV", "V"].into_iter().zip(VOICE_PARAMS) {
        sections.push(Section {
            title,
            items: vec![toggle(on), knob(pitch), knob(fine), knob(gain)],
        });
    }
    sections.push(Section {
        title: "Output",
        items: vec![knob(WIDTH), knob(OUTPUT_GAIN), knob(MIX)],
    });
    sections
}

/// One comb. Reads the loop's delay one sample before writing it.
struct Voice {
    line: InterpDelay,
    loss: f32,
    delay: f32,
    delay_target: f32,
    /// The loop filter's state.
    state: f32,
    feedback: f32,
    input: f32,
    level: f32,
    on: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            line: InterpDelay::new(16),
            loss: 0.0,
            delay: 1.0,
            delay_target: 1.0,
            state: 0.0,
            feedback: 0.0,
            input: 1.0,
            level: 0.0,
            on: true,
        }
    }
}

pub struct Resonators {
    v: [f64; PARAMS],
    sr: f32,
    filter: [Biquad; 2],
    filter_dirty: bool,
    voices: [Voice; 5],
    /// The MIDI pitch each voice sounds, as last worked out.
    pitches: [f32; 5],
    tuning: Option<Arc<TuningTable>>,
    /// Whether the delay lengths are at their targets yet.
    placed: bool,
    frames: usize,
}

impl Default for Resonators {
    fn default() -> Self {
        let params = params();
        let mut v = [0.0; PARAMS];
        for p in &params {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            filter: [Biquad::default(); 2],
            filter_dirty: true,
            voices: Default::default(),
            pitches: [60.0; 5],
            tuning: None,
            placed: false,
            frames: 0,
        }
    }
}

/// `(gain, phase delay in samples)` of the one-pole `H(z) = (1-a) / (1 - a z^-1)` at `w` rad/sample.
fn one_pole_response(a: f32, w: f32) -> (f32, f32) {
    let (s, c) = w.sin_cos();
    let denominator = (1.0 - 2.0 * a * c + a * a).sqrt();
    (
        (1.0 - a) / denominator.max(1e-6),
        (a * s).atan2(1.0 - a * c) / w,
    )
}

/// A MIDI pitch to hertz: through the tuning system, if the track plays one, so the Note and
/// Pitch controls name notes of the tuning.
fn note_to_hz(note: f32, tuning: Option<&TuningTable>) -> f32 {
    match tuning {
        Some(t) => t.note_hz(note),
        None => midi_to_hz(note),
    }
}

/// The pitches of the five resonators as fractional MIDI notes, from Note, the four Pitch values
/// and their Fine tunings. `scale` is the Use Current Scale result: `None` means semitones.
pub fn voice_pitches(
    note: f64,
    fine: [f64; 5],
    pitch: [f64; 4],
    scale: Option<ScaleInfo>,
) -> [f32; 5] {
    let note = note.round();
    match scale {
        Some(scale) => {
            let root = SCALE_OCTAVE_BASE + i32::from(scale.root);
            let first = transpose_degrees(root, (note - SCALE_ROOT_NOTE) as i32, scale);
            let mut out = [first as f32; 5];
            for (k, p) in pitch.iter().enumerate() {
                out[k + 1] =
                    transpose_degrees(first, (p.round() as i32).clamp(-21, 21), scale) as f32;
            }
            for (o, f) in out.iter_mut().zip(fine) {
                *o += (f * 0.01) as f32;
            }
            out
        }
        None => {
            let mut out = [note as f32; 5];
            for (k, p) in pitch.iter().enumerate() {
                out[k + 1] = (note + p.round().clamp(-24.0, 24.0)) as f32;
            }
            for (o, f) in out.iter_mut().zip(fine) {
                *o += (f * 0.01) as f32;
            }
            out
        }
    }
}

impl Resonators {
    fn resolve_scale(&self, transport: &TransportInfo) -> Option<ScaleInfo> {
        if self.v[USE_SCALE as usize] >= 0.5 {
            transport.scale
        } else {
            None
        }
    }

    fn voice_controls(&self, k: usize) -> (bool, f64, f64, f64) {
        if k == 0 {
            (
                self.v[ON1 as usize] >= 0.5,
                0.0,
                self.v[FINE as usize],
                self.v[GAIN1 as usize],
            )
        } else {
            let [on, pitch, fine, gain] = VOICE_PARAMS[k - 1];
            (
                self.v[on as usize] >= 0.5,
                self.v[pitch as usize],
                self.v[fine as usize],
                self.v[gain as usize],
            )
        }
    }

    fn configure_filter(&mut self) {
        let kind = match self.v[FILTER_TYPE as usize].round() as i32 {
            1 => BiquadKind::BandPass,
            2 => BiquadKind::HighPass,
            3 => BiquadKind::Notch,
            _ => BiquadKind::LowPass,
        };
        let hz = self.v[FILTER_FREQ as usize] as f32;
        for f in self.filter.iter_mut() {
            f.set(kind, hz, 0.707, 0.0, self.sr);
        }
        self.filter_dirty = false;
    }

    fn update_voices(&mut self, scale: Option<ScaleInfo>) {
        let decay = (self.v[DECAY as usize] as f32 * 0.001).max(0.001);
        let constant = self.v[CONST as usize] >= 0.5;
        // Color: 0 is dark, 100 is bright, read as a loop one-pole coefficient.
        let color = (self.v[COLOR as usize] as f32 * 0.01).clamp(0.0, 1.0);
        let loss = (1.0 - color) * 0.9;
        let mode_b = self.v[MODE as usize].round() as usize == MODE_B;
        let mut fine = [0.0; 5];
        let mut pitch = [0.0; 4];
        for (k, f) in fine.iter_mut().enumerate() {
            let (_, p, c, _) = self.voice_controls(k);
            *f = c;
            if k > 0 {
                pitch[k - 1] = p;
            }
        }
        self.pitches = voice_pitches(self.v[NOTE as usize], fine, pitch, scale);
        for k in 0..5 {
            let (on, _, _, gain) = self.voice_controls(k);
            let hz = note_to_hz(self.pitches[k], self.tuning.as_deref()).min(self.sr * 0.45);
            let w = TAU * hz / self.sr;
            // Mode B closes the loop on itself with its sign flipped, half a period long.
            let period = self.sr / hz;
            let wanted_loop = if mode_b { period * 0.5 } else { period };
            let (filter_gain, filter_delay) = one_pole_response(loss, w);
            let delay = (wanted_loop - filter_delay).max(1.0);
            let ring = if constant {
                decay
            } else {
                decay * REFERENCE_HZ / hz
            };
            let per_pass = 10f32.powf(-3.0 * (wanted_loop / self.sr) / ring.max(1e-4));
            let voice = &mut self.voices[k];
            voice.on = on;
            voice.level = gain as f32 * 0.01;
            voice.delay_target = delay.min(self.sr * LONGEST_NOTE);
            voice.loss = loss;
            // The filter's loss at the fundamental is made up for, to keep the ring time, as far
            // as stays stable at DC.
            let mut feedback = (per_pass / filter_gain.max(1e-3)).min(MAX_LOOP_GAIN);
            if mode_b {
                feedback = -feedback;
            }
            voice.feedback = feedback;
            voice.input = (1.0 - per_pass * per_pass).max(1e-6).sqrt();
        }
        if !self.placed {
            for voice in self.voices.iter_mut() {
                voice.delay = voice.delay_target;
            }
            self.placed = true;
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        let glide = 1.0 - (-(CONTROL_FRAMES as f32) / (0.01 * self.sr)).exp();
        let width = (self.v[WIDTH as usize] as f32 * 0.01).clamp(0.0, 1.0);
        let out_gain = rlive_core::db_to_gain(self.v[OUTPUT_GAIN as usize] as f32);
        let mix = (self.v[MIX as usize] as f32 * 0.01).clamp(0.0, 1.0);
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for i in 0..left.len() {
            self.frames += 1;
            if self.frames >= CONTROL_FRAMES {
                self.frames = 0;
                for voice in self.voices.iter_mut() {
                    voice.delay += (voice.delay_target - voice.delay) * glide;
                }
            }
            let (dry_l, dry_r) = (left[i], right[i]);
            let filtered = [self.filter[0].process(dry_l), self.filter[1].process(dry_r)];
            let (mut center, mut lside, mut rside) = (0.0f32, 0.0f32, 0.0f32);
            for (k, voice) in self.voices.iter_mut().enumerate() {
                if !voice.on {
                    continue;
                }
                // I hears both channels; II and IV the left; III and V the right.
                let x = match k {
                    0 => 0.5 * (filtered[0] + filtered[1]),
                    1 | 3 => filtered[0],
                    _ => filtered[1],
                };
                let y = voice.line.read(voice.delay - 1.0, Interpolation::Cubic);
                voice.state += (y - voice.state) * (1.0 - voice.loss);
                let next = sanitize(soft_limit(x * voice.input + voice.feedback * voice.state));
                voice.line.write(next);
                // The value going into the loop, so the input is heard from its first sample.
                let out = next * voice.level;
                match k {
                    0 => center += out,
                    1 | 3 => lside += out,
                    _ => rside += out,
                }
            }
            let mid = 0.5 * (lside + rside);
            let side = 0.5 * (lside - rside) * width;
            let wet_l = (center + mid + side) * out_gain;
            let wet_r = (center + mid - side) * out_gain;
            left[i] = dry_l * (1.0 - mix) + wet_l * mix;
            right[i] = dry_r * (1.0 - mix) + wet_r * mix;
        }
    }
}

impl Processor for Resonators {
    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot != TUNING_SLOT {
            return Some(resource);
        }
        match resource.downcast::<TuningTable>() {
            Ok(table) => {
                let old = self.tuning.replace(table);
                self.filter_dirty = true;
                old.map(|t| t as Resource)
            }
            Err(other) => Some(other),
        }
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let longest = (sample_rate as f32 / midi_to_hz(0.0)) as usize + 16;
        for voice in self.voices.iter_mut() {
            voice.line = InterpDelay::new(longest);
        }
        self.placed = false;
        self.filter_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let scale = self.resolve_scale(ctx.transport);
        self.update_voices(scale);
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                self.set_param(id, value);
                self.update_voices(scale);
            }
            BlockStep::Audio(audio) => {
                if self.filter_dirty {
                    self.configure_filter();
                }
                self.render(audio);
            }
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
            self.filter_dirty |= id == FILTER_TYPE || id == FILTER_FREQ;
        }
    }

    fn reset(&mut self) {
        for voice in self.voices.iter_mut() {
            voice.line.clear();
            voice.state = 0.0;
        }
        self.filter = [Biquad::default(); 2];
        self.filter_dirty = true;
        self.placed = false;
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.resonators",
    name: "Resonators",
    role: DeviceRole::AudioEffect,
    category: "Reverb & Resonance",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Resonators>::default(),
};
