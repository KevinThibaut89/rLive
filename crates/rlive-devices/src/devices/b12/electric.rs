//! Electric: a tine electric piano from physical parts (manual pages 736-741).
//!
//! A hammer strikes a fork, the fork's tine and tone bar ring as modal banks, and a pickup turns
//! their motion into a voltage: the magnetic flux's time derivative for R, the capacitance change
//! for W. It's our own design, not a copy of Electric's model, and the manual gives no numbers for
//! the mechanics, so ranges and constants are ours.

use super::common::{
    knob, limit, menu, note_hz, scale_note_bend, stretch_semitones, ModalBank, StealFade, CONTROL,
};
use crate::dsp::oversample::{OversampleFactor, Oversampler};
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::dsp::voice::{NoteInfo, VoiceAllocator, VoiceEvent, VoiceSettings, VoiceState};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamInfo, ProcessContext, Processor, Section,
    Status, Tail, ViewId,
};
use std::f32::consts::TAU;

pub const STIFFNESS: u32 = 0;
pub const STIFF_VEL: u32 = 1;
pub const STIFF_KEY: u32 = 2;
pub const NOISE: u32 = 3;
pub const NOISE_PITCH: u32 = 4;
pub const NOISE_DECAY: u32 = 5;
pub const NOISE_KEY: u32 = 6;
pub const FORCE: u32 = 7;
pub const FORCE_VEL: u32 = 8;
pub const FORCE_KEY: u32 = 9;
pub const TINE_COLOR: u32 = 10;
pub const TINE_DECAY: u32 = 11;
pub const TINE_LEVEL: u32 = 12;
pub const TINE_KEY: u32 = 13;
pub const TONE_DECAY: u32 = 14;
pub const TONE_LEVEL: u32 = 15;
pub const RELEASE: u32 = 16;
pub const SYMMETRY: u32 = 17;
pub const DISTANCE: u32 = 18;
pub const PICKUP_TYPE: u32 = 19;
pub const INPUT: u32 = 20;
pub const OUTPUT: u32 = 21;
pub const OUTPUT_KEY: u32 = 22;
pub const DAMP_TONE: u32 = 23;
pub const DAMP_LEVEL: u32 = 24;
pub const DAMP_ATT_REL: u32 = 25;
pub const VOLUME: u32 = 26;
pub const VOICES: u32 = 27;
pub const SEMI: u32 = 28;
pub const DETUNE: u32 = 29;
pub const STRETCH: u32 = 30;
pub const PITCH_BEND: u32 = 31;
pub const NOTE_PB: u32 = 32;
const PARAMS: usize = 33;

/// A clamped-free bar's overtones.
pub const TINE_RATIOS: [f32; 6] = [1.0, 6.267, 17.55, 34.39, 56.84, 84.91];
/// A free-free bar's overtones.
pub const TONE_RATIOS: [f32; 3] = [1.0, 2.756, 5.404];
const TINE_MODES: usize = TINE_RATIOS.len();
const TOTAL_MODES: usize = TINE_MODES + TONE_RATIOS.len();
const MAX_VOICES: usize = 32;
const VOICE_LABELS: [&str; 8] = ["1", "2", "4", "6", "8", "12", "16", "32"];
const VOICE_COUNTS: [usize; 8] = [1, 2, 4, 6, 8, 12, 16, 32];

pub const PICKUP_R: usize = 0;
pub const PICKUP_W: usize = 1;

pub const VIEW: ViewId = ViewId(0);
/// `[voices playing]`.
pub const VIEW_LEN: usize = 1;

fn params() -> Vec<ParamInfo> {
    let pct = |id, name: &str, d| ParamInfo::continuous(id, name, 0.0, 100.0, d, "%");
    let bi = |id, name: &str, d| ParamInfo::continuous(id, name, -100.0, 100.0, d, "%");
    vec![
        pct(STIFFNESS, "Stiffness", 40.0),
        bi(STIFF_VEL, "Stiffness Vel", 40.0),
        bi(STIFF_KEY, "Stiffness Key", 0.0),
        pct(NOISE, "Noise", 10.0),
        ParamInfo::skewed(
            NOISE_PITCH,
            "Noise Pitch",
            100.0,
            12_000.0,
            2_500.0,
            "Hz",
            3.0,
        ),
        ParamInfo::skewed(NOISE_DECAY, "Noise Decay", 1.0, 500.0, 20.0, "ms", 3.0),
        bi(NOISE_KEY, "Noise Key", 0.0),
        pct(FORCE, "Force", 60.0),
        bi(FORCE_VEL, "Force Vel", 70.0),
        bi(FORCE_KEY, "Force Key", 0.0),
        pct(TINE_COLOR, "Tine Color", 50.0),
        ParamInfo::skewed(TINE_DECAY, "Tine Decay", 50.0, 20_000.0, 3_000.0, "ms", 3.0),
        ParamInfo::continuous(TINE_LEVEL, "Tine Level", -48.0, 6.0, 0.0, "dB"),
        bi(TINE_KEY, "Tine Key", 0.0),
        ParamInfo::skewed(TONE_DECAY, "Tone Decay", 50.0, 20_000.0, 5_000.0, "ms", 3.0),
        ParamInfo::continuous(TONE_LEVEL, "Tone Level", -48.0, 6.0, -6.0, "dB"),
        ParamInfo::skewed(RELEASE, "Release", 5.0, 5_000.0, 80.0, "ms", 3.0),
        pct(SYMMETRY, "Symmetry", 60.0),
        pct(DISTANCE, "Distance", 50.0),
        ParamInfo::stepped(PICKUP_TYPE, "Type", &["R", "W"], PICKUP_R),
        pct(INPUT, "Input", 50.0),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 24.0, 0.0, "dB"),
        bi(OUTPUT_KEY, "Output Key", 0.0),
        pct(DAMP_TONE, "Damper Tone", 40.0),
        pct(DAMP_LEVEL, "Damper Level", 15.0),
        bi(DAMP_ATT_REL, "Att/Rel", 0.0),
        ParamInfo::continuous(VOLUME, "Volume", -48.0, 12.0, -6.0, "dB"),
        ParamInfo::stepped(VOICES, "Voices", &VOICE_LABELS, 5),
        ParamInfo::continuous(SEMI, "Semi", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(DETUNE, "Detune", -50.0, 50.0, 0.0, "ct"),
        bi(STRETCH, "Stretch", 0.0),
        ParamInfo::continuous(PITCH_BEND, "Pitch Bend", 0.0, 24.0, 2.0, "st"),
        ParamInfo::continuous(NOTE_PB, "Note PB", 0.0, 48.0, 48.0, "st"),
    ]
}

fn layout() -> Vec<Section> {
    let knobs = |ids: &[u32]| ids.iter().map(|&id| knob(id)).collect::<Vec<_>>();
    vec![
        Section {
            title: "Hammer",
            items: {
                let mut v = vec![Item::Panel("electric_display")];
                v.extend(knobs(&[
                    STIFFNESS,
                    STIFF_VEL,
                    STIFF_KEY,
                    NOISE,
                    NOISE_PITCH,
                    NOISE_DECAY,
                    NOISE_KEY,
                    FORCE,
                    FORCE_VEL,
                    FORCE_KEY,
                ]));
                v
            },
        },
        Section {
            title: "Fork",
            items: knobs(&[
                TINE_COLOR, TINE_DECAY, TINE_LEVEL, TINE_KEY, TONE_DECAY, TONE_LEVEL, RELEASE,
            ]),
        },
        Section {
            title: "Pickup",
            items: vec![
                knob(SYMMETRY),
                knob(DISTANCE),
                menu(PICKUP_TYPE),
                knob(INPUT),
                knob(OUTPUT),
                knob(OUTPUT_KEY),
            ],
        },
        Section {
            title: "Damper",
            items: knobs(&[DAMP_TONE, DAMP_LEVEL, DAMP_ATT_REL]),
        },
        Section {
            title: "Global",
            items: vec![
                knob(VOLUME),
                menu(VOICES),
                knob(SEMI),
                knob(DETUNE),
                knob(STRETCH),
                knob(PITCH_BEND),
                knob(NOTE_PB),
            ],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.electric",
    name: "Electric",
    role: DeviceRole::Instrument,
    category: "Physical Models",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Electric::new()),
};

/// The pickup's response to a displacement `u`, as `(flux-style output, DC-free)`. R is the time
/// derivative of the flux through the coil, W is the displacement seen through the capacitance.
#[inline]
pub fn pickup(
    kind: usize,
    u: f32,
    offset: f32,
    distance: f32,
    previous: &mut f32,
    scale: f32,
) -> f32 {
    let at = u + offset;
    if kind == PICKUP_W {
        let y = at / (1.0 + (at / distance).powi(2)).sqrt();
        y * scale * 0.25
    } else {
        let flux = distance * distance / (at * at + distance * distance).sqrt();
        // On the first sample of a note there is no previous flux to difference against.
        let last = if previous.is_nan() { flux } else { *previous };
        let v = (flux - last) * scale;
        *previous = flux;
        v
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Tuning {
    pitch: f32,
    color: f32,
    tine_decay: f32,
    tone_decay: f32,
    release: f32,
    released: bool,
}

struct Voice {
    active: bool,
    note: NoteInfo,
    released: bool,
    bank: ModalBank,
    tuning: Option<Tuning>,
    os: Oversampler,
    prev_flux: f32,
    dc: f32,
    hammer_at: u32,
    hammer_len: u32,
    hammer_gain: f32,
    noise_env: f32,
    noise_coef: f32,
    noise_gain: f32,
    noise_band: Biquad,
    damper_env: f32,
    damper_coef: f32,
    damper_gain: f32,
    damper_lp: f32,
    damper_state: f32,
    rng: Rng,
    fade: StealFade,
    quiet: u32,
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            active: false,
            note: NoteInfo {
                key: 0,
                channel: 0,
                note_id: 0,
                velocity: 0.0,
                unison_index: 0,
                unison_count: 1,
                spread: 0.0,
            },
            released: false,
            bank: ModalBank::with_count(TOTAL_MODES),
            tuning: None,
            os: Oversampler::new(OversampleFactor::X2, CONTROL),
            prev_flux: f32::NAN,
            dc: 0.0,
            hammer_at: 0,
            hammer_len: 0,
            hammer_gain: 0.0,
            noise_env: 0.0,
            noise_coef: 0.0,
            noise_gain: 0.0,
            noise_band: Biquad::default(),
            damper_env: 0.0,
            damper_coef: 0.0,
            damper_gain: 0.0,
            damper_lp: 0.0,
            damper_state: 0.0,
            rng: Rng::new(seed),
            fade: StealFade::default(),
            quiet: 0,
        }
    }

    fn silence(&mut self) {
        self.active = false;
        self.bank.clear();
        self.tuning = None;
        self.os.reset();
        self.prev_flux = f32::NAN;
        self.dc = 0.0;
        self.hammer_at = 0;
        self.hammer_len = 0;
        self.noise_env = 0.0;
        self.noise_band.reset();
        self.damper_env = 0.0;
        self.damper_state = 0.0;
        self.fade.clear();
    }
}

/// Electric. See the [module docs](self).
pub struct Electric {
    values: [f64; PARAMS],
    sample_rate: f32,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
}

impl Default for Electric {
    fn default() -> Self {
        Self::new()
    }
}

impl Electric {
    pub fn new() -> Self {
        let mut values = [0.0; PARAMS];
        for p in params() {
            values[p.id as usize] = p.default;
        }
        Self {
            values,
            sample_rate: 48_000.0,
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices: Vec::new(),
        }
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize] as f32
    }

    fn sync_settings(&mut self) {
        let voices = VOICE_COUNTS[(self.v(VOICES).round() as usize).min(VOICE_COUNTS.len() - 1)];
        self.alloc.set(&VoiceSettings {
            polyphony: voices,
            bend_range: self.v(PITCH_BEND),
            ..Default::default()
        });
    }

    fn start(&mut self, i: usize, note: NoteInfo) {
        let sr = self.sample_rate;
        let key = (note.key as f32 - 60.0) / 60.0;
        let velocity = note.velocity;
        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;
        let voice = &mut self.voices[i];
        voice.silence();
        voice.active = true;
        voice.note = note;
        voice.released = false;
        voice.quiet = 0;
        let stiff = (pc(STIFFNESS) + pc(STIFF_VEL) * (velocity - 1.0) + pc(STIFF_KEY) * key)
            .clamp(0.0, 1.0);
        let force =
            (pc(FORCE) * (1.0 + pc(FORCE_VEL) * (velocity - 1.0)) * (1.0 + pc(FORCE_KEY) * key))
                .clamp(0.0, 2.0);
        // Harder hammers and harder blows stay in contact for less time.
        let seconds = 0.006 * (0.000_2f32 / 0.006).powf(stiff * (0.7 + 0.3 * force.min(1.0)));
        voice.hammer_len = ((seconds * sr) as u32).max(2);
        voice.hammer_at = 0;
        voice.hammer_gain = force;
        let noise_key = 1.0 + pc(NOISE_KEY) * key;
        voice.noise_gain = pc(NOISE) * noise_key * force * 0.5;
        voice.noise_env = 1.0;
        voice.noise_coef = (-6.9 / (g(NOISE_DECAY) * 0.001 * sr).max(1.0)).exp();
        voice.noise_band.set(
            BiquadKind::BandPass,
            g(NOISE_PITCH).min(sr * 0.45),
            1.5,
            0.0,
            sr,
        );
        voice.noise_band.reset();
        let att = (1.0 - pc(DAMP_ATT_REL)).min(1.0);
        voice.damper_gain = pc(DAMP_LEVEL) * att;
        voice.damper_env = 1.0;
        voice.damper_coef = (-6.9 / (0.03 * sr)).exp();
        let hz = 150.0 * 2f32.powf(pc(DAMP_TONE) * 6.0);
        voice.damper_lp = (-TAU * hz.min(sr * 0.45) / sr).exp();
    }

    fn release(&mut self, i: usize) {
        let rel = (1.0 + self.v(DAMP_ATT_REL) * 0.01).min(1.0);
        let level = self.v(DAMP_LEVEL) * 0.01;
        let voice = &mut self.voices[i];
        voice.released = true;
        voice.damper_gain = level * rel;
        voice.damper_env = 1.0;
    }

    fn control(&mut self, i: usize) -> (f32, f32) {
        let sr = self.sample_rate;
        let key_note = self.voices[i].note.key as f32;
        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;
        let key = (key_note - 60.0) / 60.0;
        // The tuning seam: the allocator's pitch has channel bend and per-note bend in it.
        let played = self.alloc.pitch(i);
        let pitch = played + g(SEMI) + g(DETUNE) * 0.01 + stretch_semitones(played, g(STRETCH));
        let tuning = Tuning {
            pitch,
            color: pc(TINE_COLOR),
            tine_decay: g(TINE_DECAY) * 0.001,
            tone_decay: g(TONE_DECAY) * 0.001,
            release: g(RELEASE) * 0.001,
            released: self.voices[i].released,
        };
        let voice = &mut self.voices[i];
        let hz = note_hz(pitch);
        if voice.tuning != Some(tuning) {
            voice.tuning = Some(tuning);
            let retime = |t: f32| {
                if tuning.released {
                    t.min(tuning.release)
                } else {
                    t
                }
            };
            let tine_level = db_to_gain(g(TINE_LEVEL)) * (1.0 + pc(TINE_KEY) * key).max(0.0);
            let tone_level = db_to_gain(g(TONE_LEVEL));
            let tilt = -1.2 + 1.5 * tuning.color;
            for (m, ratio) in TINE_RATIOS.iter().enumerate() {
                let f = hz * ratio;
                voice.bank.tune(
                    m,
                    f.min(sr * 0.49),
                    retime(tuning.tine_decay / ratio.sqrt()),
                    sr,
                );
                let amp = if f < sr * 0.45 { ratio.powf(tilt) } else { 0.0 };
                voice.bank.weights(m, amp, tine_level, 0.0);
            }
            for (m, ratio) in TONE_RATIOS.iter().enumerate() {
                let f = hz * ratio;
                let at = TINE_MODES + m;
                voice
                    .bank
                    .tune(at, f.min(sr * 0.49), retime(tuning.tone_decay / ratio), sr);
                let amp = if f < sr * 0.45 { 0.5 / ratio } else { 0.0 };
                voice.bank.weights(at, amp, 0.0, tone_level);
            }
        }
        voice.bank.sanitize();
        let quiet = voice.bank.level() < 3e-6
            && voice.hammer_at >= voice.hammer_len
            && voice.noise_env < 1e-4
            && voice.damper_env < 1e-4;
        voice.quiet = if quiet { voice.quiet + 1 } else { 0 };
        (hz, 1.0 + pc(OUTPUT_KEY) * key)
    }

    fn render_voice(
        &mut self,
        i: usize,
        hz: f32,
        key_gain: f32,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let sr = self.sample_rate;
        let n = left.len().min(right.len());
        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;
        let kind = g(PICKUP_TYPE).round() as usize;
        let input = 0.2 + 1.8 * pc(INPUT);
        let offset = (pc(SYMMETRY) - 0.5) * 2.0 * 0.8;
        let distance = 0.04 + 0.96 * pc(DISTANCE).powf(1.5);
        let output = db_to_gain(g(OUTPUT)) * key_gain.max(0.0);
        let volume = db_to_gain(g(VOLUME)) * 2.0;
        let voice = &mut self.voices[i];

        let mut buffer = [0.0f32; CONTROL];
        for sample in buffer[..n].iter_mut() {
            let mut x = 0.0;
            if voice.hammer_at < voice.hammer_len {
                let t = voice.hammer_at as f32 / voice.hammer_len as f32;
                x = voice.hammer_gain * (1.0 - (TAU * t).cos()) / voice.hammer_len as f32;
                voice.hammer_at += 1;
            }
            let [tine, tone] = voice.bank.tick(x);
            *sample = (tine + tone) * input;
        }

        // The pickup bends the waveform, so it runs oversampled. Its output is scaled so that a
        // given displacement has the same level at every pitch.
        let scale = 2.0 * sr / (TAU * hz.max(20.0));
        let previous = &mut voice.prev_flux;
        voice.os.process_in_place(&mut buffer[..n], |hi| {
            for s in hi.iter_mut() {
                *s = pickup(kind, *s, offset, distance, previous, scale);
            }
        });

        let dc_coef = 1.0 - (TAU * 12.0 / sr);
        for (at, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let y = buffer[at] * output;
            voice.dc = dc_coef * voice.dc + (1.0 - dc_coef) * y;
            let picked = y - voice.dc;
            let hammer =
                voice.noise_band.process(voice.rng.bipolar()) * voice.noise_env * voice.noise_gain;
            voice.noise_env *= voice.noise_coef;
            voice.damper_state = voice.damper_lp * voice.damper_state
                + (1.0 - voice.damper_lp) * voice.rng.bipolar();
            let damper = voice.damper_state * voice.damper_env * voice.damper_gain * 0.5;
            voice.damper_env *= voice.damper_coef;
            let out = sanitize((picked + hammer + damper) * voice.fade.gain() * volume);
            *l += out;
            *r += out;
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mut at = 0;
        while at < frames {
            let n = (frames - at).min(CONTROL);
            self.alloc.advance(n);
            let (mut mix_l, mut mix_r) = ([0.0f32; CONTROL], [0.0f32; CONTROL]);
            for i in 0..self.voices.len() {
                if !self.voices[i].active {
                    continue;
                }
                if let Some(next) = self.voices[i].fade.poll() {
                    match next {
                        Some(note) => self.start(i, note),
                        None => {
                            self.voices[i].silence();
                            continue;
                        }
                    }
                }
                let (hz, key_gain) = self.control(i);
                self.render_voice(i, hz, key_gain, &mut mix_l[..n], &mut mix_r[..n]);
                let held = self.alloc.voice(i).state == VoiceState::Held;
                let voice = &mut self.voices[i];
                if !voice.fade.is_fading() && voice.quiet > 8 && !held {
                    voice.silence();
                    self.alloc.finished(i);
                }
            }
            for (out, mix) in left[at..at + n].iter_mut().zip(&mix_l[..n]) {
                *out += limit(*mix);
            }
            for (out, mix) in right[at..at + n].iter_mut().zip(&mix_r[..n]) {
                *out += limit(*mix);
            }
            at += n;
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        let message = scale_note_bend(message, self.v(NOTE_PB));
        let mut starts: [Option<(usize, NoteInfo, bool)>; 16] = [None; 16];
        let mut releases: [Option<usize>; 32] = [None; 32];
        let (mut started, mut released) = (0, 0);
        let (alloc, voices) = (&mut self.alloc, &mut self.voices);
        alloc.handle(message, |event| match event {
            VoiceEvent::Start {
                voice,
                note,
                stolen,
            } => {
                if started < starts.len() {
                    starts[started] = Some((voice, note, stolen));
                    started += 1;
                }
            }
            VoiceEvent::Release { voice } => {
                if released < releases.len() {
                    releases[released] = Some(voice);
                    released += 1;
                }
            }
            VoiceEvent::Kill { voice } => voices[voice].fade.start(None),
            VoiceEvent::Legato { .. } => {}
        });
        for voice in releases.into_iter().flatten() {
            self.release(voice);
        }
        for (voice, note, stolen) in starts.into_iter().flatten() {
            if stolen && self.voices[voice].active {
                self.voices[voice].fade.start(Some(note));
            } else {
                self.start(voice, note);
            }
        }
    }
}

impl Processor for Electric {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sample_rate = sample_rate as f32;
        self.alloc.prepare(sample_rate);
        self.voices = (0..MAX_VOICES).map(|i| Voice::new(i as u32 + 11)).collect();
        self.sync_settings();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let this = &mut *self;
        ctx.for_each_step(|step| match step {
            BlockStep::Audio(a) => this.render(a.left, a.right),
            BlockStep::Param { id, value } => this.set_param(id, value),
            BlockStep::Midi(m) => this.midi(m),
        });
        if ctx.view.is_active() {
            let playing = self.voices.iter().filter(|v| v.active).count();
            ctx.view.publish(VIEW, &[playing as f32]);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
            if matches!(id, VOICES | PITCH_BEND) {
                self.sync_settings();
            }
        }
    }

    fn reset(&mut self) {
        self.alloc.reset();
        for v in &mut self.voices {
            v.silence();
        }
    }

    fn latency(&self) -> u32 {
        self.voices.first().map_or(0, |v| v.os.latency())
    }

    fn tail(&self) -> Tail {
        let seconds = self.v(RELEASE) * 0.001 * 8.0 + 0.1;
        Tail::Frames((seconds * self.sample_rate) as u32)
    }
}
