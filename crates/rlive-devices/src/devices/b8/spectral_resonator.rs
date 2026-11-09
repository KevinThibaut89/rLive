//! Spectral Resonator: rings a set of tuned partials from the input's spectrum (manual pages
//! 668-673).
//!
//! The manual describes the Internal and MIDI pitch modes, Mono/Poly with up to 16 voices, MIDI
//! Gate, Glide, PB, Freq with its Hertz/Note switch and the Use Current Scale SD Shift, Transp.,
//! Stretch (100% is odd harmonics only), Shift (+-48 semitones on the input only), Decay, HF and
//! LF Damp, the four modulation modes, Harmonics and Quantize, Input Send with its limiter, Unison
//! and Dry/Wet. It gives no numbers for most of them, so the ranges here are ours. Each partial is
//! a decaying resonator of its own, driven by the spectrally shifted input, with an exact pitch
//! and decay; Shift and the spectrogram go through the shared STFT.

use super::common::{
    hz_to_note, knob, menu, note_to_hz, quantize_to_scale, toggle, PeakLimiter, Spectrogram,
    SPECTROGRAM_VIEW_LEN,
};
use crate::devices::b1::common::{int_param, transpose_degrees, transpose_degrees_fractional};
use crate::dsp::fft::Complex32;
use crate::dsp::lookahead::LookaheadBuffer;
use crate::dsp::stft::Stft;
use crate::dsp::util::{db_to_gain, Rng};
use crate::dsp::voice::{
    GlideMode, VoiceAllocator, VoiceEvent, VoiceMode, VoiceSettings, VoiceState,
};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor, ScaleInfo,
    Section, Status, Tail, ViewId,
};
use std::f32::consts::TAU;

pub const PITCH_MODE: u32 = 0;
pub const FREQ_MODE: u32 = 1;
pub const FREQ: u32 = 2;
pub const NOTE: u32 = 3;
pub const USE_SCALE: u32 = 4;
pub const SD_SHIFT: u32 = 5;
pub const TRANSPOSE: u32 = 6;
pub const MONO_POLY: u32 = 7;
pub const POLYPHONY: u32 = 8;
pub const MIDI_GATE: u32 = 9;
pub const GLIDE: u32 = 10;
pub const PB: u32 = 11;
pub const STRETCH: u32 = 12;
pub const SHIFT: u32 = 13;
pub const DECAY: u32 = 14;
pub const HF_DAMP: u32 = 15;
pub const LF_DAMP: u32 = 16;
pub const MOD_MODE: u32 = 17;
pub const MOD_RATE: u32 = 18;
pub const PCH_MOD: u32 = 19;
pub const HARMONICS: u32 = 20;
pub const QUANTIZE: u32 = 21;
pub const SPECTROGRAM: u32 = 22;
pub const INPUT_SEND: u32 = 23;
pub const UNISON: u32 = 24;
pub const UNI_AMT: u32 = 25;
pub const DRY_WET: u32 = 26;
const PARAMS: usize = 27;

pub const MODE_INTERNAL: usize = 0;
pub const MODE_MIDI: usize = 1;
pub const FREQ_HERTZ: usize = 0;
pub const FREQ_NOTE: usize = 1;
pub const VOICES_MONO: usize = 0;
pub const VOICES_POLY: usize = 1;
pub const MOD_NONE: usize = 0;
pub const MOD_CHORUS: usize = 1;
pub const MOD_WANDER: usize = 2;
pub const MOD_GRANULAR: usize = 3;

/// The STFT that carries the input is fixed; this is the device's whole latency.
pub const FRAME: usize = 480;
pub const HOP: usize = 120;
pub const MAX_HARMONICS: usize = 256;
pub const MAX_UNISON: usize = 8;
pub const MAX_VOICES: usize = 16;
const MAX_PARTIALS: usize = MAX_HARMONICS * MAX_UNISON;
const TICK: usize = 32;
const LANES: usize = 8;
const GROUPS: usize = MAX_PARTIALS / LANES;
/// The most a partial is allowed to be pushed sharp, in cents, at full Uni. Amt.
const UNISON_SPREAD_CENTS: f32 = 50.0;

pub const SPECTROGRAM_VIEW: ViewId = ViewId(0);
/// `[limiter is engaged 0 or 1]` after the spectrogram.
pub const LIMITER_LED: ViewId = ViewId(SPECTROGRAM_VIEW_LEN as u32);
pub const VIEW_LEN: usize = SPECTROGRAM_VIEW_LEN + 1;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(
            PITCH_MODE,
            "Pitch Mode",
            &["Internal", "MIDI"],
            MODE_INTERNAL,
        ),
        ParamInfo::stepped(FREQ_MODE, "Frequency Mode", &["Hertz", "Note"], FREQ_HERTZ),
        ParamInfo::skewed(FREQ, "Freq", 20.0, 8_000.0, 220.0, "Hz", 3.0),
        int_param(NOTE, "Note", 0.0, 127.0, 57.0),
        ParamInfo::toggle(USE_SCALE, "Use Current Scale", false),
        int_param(SD_SHIFT, "SD Shift", -28.0, 28.0, 0.0),
        int_param(TRANSPOSE, "Transp.", -48.0, 48.0, 0.0),
        ParamInfo::stepped(MONO_POLY, "Mono/Poly", &["Mono", "Poly"], VOICES_POLY),
        int_param(POLYPHONY, "Polyphony", 2.0, MAX_VOICES as f64, 8.0),
        ParamInfo::toggle(MIDI_GATE, "MIDI Gate", true),
        ParamInfo::skewed(GLIDE, "Glide", 0.0, 2_000.0, 0.0, "ms", 3.0),
        ParamInfo::continuous(PB, "PB", 0.0, 24.0, 2.0, "st"),
        ParamInfo::continuous(STRETCH, "Stretch", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(SHIFT, "Shift", -48.0, 48.0, 0.0, "st"),
        ParamInfo::skewed(DECAY, "Decay", 10.0, 30_000.0, 2_000.0, "ms", 4.0),
        ParamInfo::continuous(HF_DAMP, "HF Damp", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LF_DAMP, "LF Damp", 0.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(
            MOD_MODE,
            "Modulation",
            &["None", "Chorus", "Wander", "Granular"],
            MOD_NONE,
        ),
        ParamInfo::skewed(MOD_RATE, "Mod Rate", 0.0, 40.0, 1.0, "Hz", 3.0),
        ParamInfo::continuous(PCH_MOD, "Pch. Mod", 0.0, 24.0, 0.0, "st"),
        int_param(HARMONICS, "Harmonics", 1.0, MAX_HARMONICS as f64, 64.0),
        ParamInfo::toggle(QUANTIZE, "Quantize", false),
        ParamInfo::toggle(SPECTROGRAM, "Show Spectrogram", true),
        ParamInfo::continuous(INPUT_SEND, "Input Send", -24.0, 24.0, 0.0, "dB"),
        int_param(UNISON, "Unison", 1.0, MAX_UNISON as f64, 1.0),
        ParamInfo::continuous(UNI_AMT, "Uni. Amt", 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(DRY_WET, "Dry/Wet", 0.0, 100.0, 50.0, "%"),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Pitch Mode",
            items: vec![
                menu(PITCH_MODE),
                menu(MONO_POLY),
                knob(POLYPHONY),
                toggle(MIDI_GATE),
                knob(GLIDE),
                knob(PB),
            ],
        },
        Section {
            title: "Frequency",
            items: vec![
                menu(FREQ_MODE),
                knob(FREQ),
                knob(NOTE),
                toggle(USE_SCALE),
                knob(SD_SHIFT),
                knob(TRANSPOSE),
                knob(STRETCH),
                knob(SHIFT),
                knob(DECAY),
                knob(HF_DAMP),
                knob(LF_DAMP),
            ],
        },
        Section {
            title: "Modulation",
            items: vec![menu(MOD_MODE), knob(MOD_RATE), knob(PCH_MOD)],
        },
        Section {
            title: "Spectrogram",
            items: vec![
                Item::Panel("spectrogram_resonator"),
                knob(HARMONICS),
                toggle(QUANTIZE),
                toggle(SPECTROGRAM),
            ],
        },
        Section {
            title: "Global",
            items: vec![knob(INPUT_SEND), knob(UNISON), knob(UNI_AMT), knob(DRY_WET)],
        },
    ]
}

/// Eight partials side by side, so the compiler can run them as one.
#[derive(Clone, Copy)]
struct Lanes {
    re_l: [f32; LANES],
    im_l: [f32; LANES],
    re_r: [f32; LANES],
    im_r: [f32; LANES],
    c_re: [f32; LANES],
    c_im: [f32; LANES],
    /// Input gain per sample.
    drive: [f32; LANES],
    out_l: [f32; LANES],
    out_r: [f32; LANES],
    out_l_step: [f32; LANES],
    out_r_step: [f32; LANES],
}

impl Default for Lanes {
    fn default() -> Self {
        Self {
            re_l: [0.0; LANES],
            im_l: [0.0; LANES],
            re_r: [0.0; LANES],
            im_r: [0.0; LANES],
            c_re: [0.0; LANES],
            c_im: [0.0; LANES],
            drive: [0.0; LANES],
            out_l: [0.0; LANES],
            out_r: [0.0; LANES],
            out_l_step: [0.0; LANES],
            out_r_step: [0.0; LANES],
        }
    }
}

/// Where a partial belongs and how its modulation is going.
#[derive(Clone, Copy, Default)]
struct Partial {
    voice: u16,
    harmonic: u16,
    /// Its own detune position in the unison stack, -1 to 1.
    unison_spread: f32,
    /// Pan of this unison copy, -1 to 1.
    pan: f32,
    phase: f32,
    speed: f32,
    amount: f32,
    /// Where the Granular decay stands, and how long until the next grain, in seconds.
    grain: f32,
    wait: f32,
    /// Offset from the partial's nominal pitch, in semitones.
    pitch_offset: f32,
    /// Amplitude from the modulation, 0 to 1.
    amp: f32,
    /// Kept from the last coefficient update.
    audible: bool,
    normal: f32,
}

#[derive(Clone, Copy, Default)]
struct VoicePitch {
    active: bool,
    gate: f32,
    gate_target: f32,
    /// The MIDI note this voice sounds, retuned at every tick.
    note: f32,
    last_note: f32,
    /// Coefficient state last used, so unchanged voices skip the retune.
    retune: bool,
}

pub struct SpectralResonator {
    v: [f64; PARAMS],
    sr: f32,
    stft: Option<[Stft; 2]>,
    dry: LookaheadBuffer<2>,
    scratch: Vec<Complex32>,
    lanes: Vec<Lanes>,
    partials: Vec<Partial>,
    partial_count: usize,
    voices: [VoicePitch; MAX_VOICES],
    voice_harmonics: usize,
    layout_voices: usize,
    layout_harmonics: usize,
    layout_unison: usize,
    alloc: VoiceAllocator,
    rng: Rng,
    limiter: PeakLimiter,
    limiter_led: f32,
    spectrogram: Option<Spectrogram>,
    /// Frames into the current control tick.
    tick_frames: usize,
    scale: Option<ScaleInfo>,
    dirty: bool,
    rotation: usize,
    frame_count: u32,
}

impl Default for SpectralResonator {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            stft: None,
            dry: LookaheadBuffer::new(FRAME + 2),
            scratch: Vec::new(),
            lanes: vec![Lanes::default(); GROUPS],
            partials: vec![Partial::default(); MAX_PARTIALS],
            partial_count: 0,
            voices: [VoicePitch::default(); MAX_VOICES],
            voice_harmonics: 0,
            layout_voices: 0,
            layout_harmonics: 0,
            layout_unison: 0,
            alloc: VoiceAllocator::new(MAX_VOICES),
            rng: Rng::new(0x3a7f),
            limiter: PeakLimiter::new(48_000.0, 0.98, 0.15),
            limiter_led: 0.0,
            spectrogram: None,
            tick_frames: 0,
            scale: None,
            dirty: true,
            rotation: 0,
            frame_count: 0,
        }
    }
}

/// The frequency ratio of the harmonic `k` (1-based) above the fundamental, with Stretch. At
/// +100% only odd harmonics are left.
pub fn stretched_ratio(k: usize, stretch: f32) -> f32 {
    let spacing = 1.0 + stretch.clamp(-1.0, 1.0);
    1.0 + (k.max(1) - 1) as f32 * spacing
}

/// The ring time of a partial at `ratio` above the fundamental, from Decay and the two Damp knobs.
pub fn partial_decay(decay: f32, ratio: f32, hf: f32, lf: f32) -> f32 {
    let high = hf * (ratio.max(1.0) - 1.0) * 0.25;
    let low = lf * 4.0 / ratio.max(1.0);
    decay / (1.0 + high + low)
}

fn apply_voice_event(voices: &mut [VoicePitch; MAX_VOICES], event: VoiceEvent) {
    match event {
        VoiceEvent::Start { voice, .. } | VoiceEvent::Legato { voice, .. } => {
            let v = &mut voices[voice];
            v.active = true;
            v.gate_target = 1.0;
            v.retune = true;
        }
        VoiceEvent::Release { voice } => {
            voices[voice].gate_target = 0.0;
        }
        VoiceEvent::Kill { voice } => {
            let v = &mut voices[voice];
            v.active = false;
            v.gate = 0.0;
            v.gate_target = 0.0;
        }
    }
}

/// Linear gain per sample that gives an amplitude decay to -60 dB in `seconds`.
fn t60_to_pole(seconds: f32, sr: f32) -> f32 {
    10f32.powf(-3.0 / (seconds * sr).max(1.0))
}

impl SpectralResonator {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn index(&self, id: u32) -> usize {
        self.v[id as usize].round().max(0.0) as usize
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn set(&mut self, id: u32, value: f64) {
        if let Some(slot) = self.v.get_mut(id as usize) {
            if *slot == value {
                return;
            }
            *slot = value;
            self.dirty = true;
            if matches!(
                id,
                PITCH_MODE | MONO_POLY | POLYPHONY | GLIDE | PB | MIDI_GATE
            ) {
                self.apply_voice_settings();
            }
        }
    }

    fn midi_mode(&self) -> bool {
        self.index(PITCH_MODE) == MODE_MIDI
    }

    fn voice_mode(&self) -> usize {
        self.index(MONO_POLY).min(VOICES_POLY)
    }

    fn polyphony(&self) -> usize {
        if !self.midi_mode() || self.voice_mode() == VOICES_MONO {
            1
        } else {
            (self.v[POLYPHONY as usize].round() as usize).clamp(2, MAX_VOICES)
        }
    }

    fn apply_voice_settings(&mut self) {
        let mono = self.voice_mode() == VOICES_MONO;
        let polyphony = self.polyphony();
        let glide_time = (self.value(GLIDE) * 0.001).max(0.0);
        self.alloc.set(&VoiceSettings {
            mode: if mono {
                VoiceMode::Legato
            } else {
                VoiceMode::Poly
            },
            polyphony,
            unison: 1,
            glide: if mono && glide_time > 0.0 {
                GlideMode::Always
            } else {
                GlideMode::Off
            },
            glide_time,
            bend_range: self.value(PB).max(0.0),
            ..Default::default()
        });
    }

    fn harmonics_per_voice(&self) -> usize {
        let total = (self.v[HARMONICS as usize].round() as usize).clamp(1, MAX_HARMONICS);
        (total / self.polyphony()).max(1)
    }

    fn unison(&self) -> usize {
        (self.v[UNISON as usize].round() as usize).clamp(1, MAX_UNISON)
    }

    /// Lays out the partials for the current voice, harmonic and unison counts.
    fn rebuild_layout(&mut self) {
        let (voices, harmonics, unison) =
            (self.polyphony(), self.harmonics_per_voice(), self.unison());
        if voices == self.layout_voices
            && harmonics == self.layout_harmonics
            && unison == self.layout_unison
        {
            return;
        }
        self.layout_voices = voices;
        self.layout_harmonics = harmonics;
        self.layout_unison = unison;
        self.voice_harmonics = harmonics;
        let mut count = 0;
        for voice in 0..voices {
            for h in 0..harmonics {
                for u in 0..unison {
                    if count >= MAX_PARTIALS {
                        break;
                    }
                    let spread = if unison > 1 {
                        u as f32 / (unison - 1) as f32 * 2.0 - 1.0
                    } else {
                        0.0
                    };
                    let p = &mut self.partials[count];
                    *p = Partial {
                        voice: voice as u16,
                        harmonic: h as u16 + 1,
                        unison_spread: spread,
                        pan: spread * 0.7,
                        phase: self.rng.unipolar(),
                        speed: 0.7 + self.rng.unipolar() * 0.6,
                        amount: self.rng.unipolar(),
                        grain: 0.0,
                        wait: self.rng.unipolar() * 0.2,
                        pitch_offset: 0.0,
                        amp: 1.0,
                        audible: false,
                        normal: 0.0,
                    };
                    count += 1;
                }
            }
        }
        self.partial_count = count;
        for lanes in self.lanes.iter_mut() {
            *lanes = Lanes::default();
        }
        self.dirty = true;
    }

    /// Internal mode's note: the Freq control, or the root and SD Shift of the current scale.
    fn internal_note(&self, scale: Option<ScaleInfo>) -> f32 {
        let degrees = self.v[SD_SHIFT as usize].round() as i32;
        if self.flag(USE_SCALE) {
            return match scale {
                Some(s) => transpose_degrees(48 + s.root as i32, degrees, s) as f32,
                None => 48.0 + degrees as f32,
            };
        }
        if self.index(FREQ_MODE) == FREQ_NOTE {
            self.value(NOTE).round()
        } else {
            hz_to_note(self.value(FREQ))
        }
    }

    /// MIDI mode's note for a played pitch: Transp., or SD Shift if the device is scale aware.
    fn midi_note(&self, pitch: f32, scale: Option<ScaleInfo>) -> f32 {
        if self.flag(USE_SCALE) {
            let degrees = self.v[SD_SHIFT as usize].round() as i32;
            return match scale {
                Some(s) => transpose_degrees_fractional(pitch, degrees, s),
                None => pitch + degrees as f32,
            };
        }
        pitch + self.v[TRANSPOSE as usize].round() as f32
    }

    fn midi(&mut self, message: rlive_core::MidiMessage) {
        if !self.midi_mode() {
            return;
        }
        let voices = &mut self.voices;
        self.alloc
            .handle(message, |event| apply_voice_event(voices, event));
    }

    /// Moves each voice's pitch and gate on one control tick.
    fn update_voices(&mut self, frames: usize) {
        let scale = self.scale;
        let tick_secs = frames as f32 / self.sr;
        let gate_up = (tick_secs / 0.005).min(1.0);
        let gate_down = (tick_secs / 0.06).min(1.0);
        let midi = self.midi_mode();
        let mono_free = midi && self.voice_mode() == VOICES_MONO && !self.flag(MIDI_GATE);
        self.alloc.advance(frames);
        for v in 0..self.layout_voices {
            if midi {
                let state = self.alloc.voice(v).state;
                if state != VoiceState::Free {
                    self.voices[v].last_note = self.alloc.pitch(v);
                }
                if v == 0 && mono_free {
                    // No gate: the resonator keeps listening at the last pitch.
                    let note = self.midi_note(self.voices[0].last_note, scale);
                    let vp = &mut self.voices[0];
                    vp.active = true;
                    vp.gate_target = 1.0;
                    if (vp.note - note).abs() > 1e-4 {
                        vp.note = note;
                        vp.retune = true;
                    }
                } else if state != VoiceState::Free {
                    let note = self.midi_note(self.alloc.pitch(v), scale);
                    let vp = &mut self.voices[v];
                    if (vp.note - note).abs() > 1e-4 {
                        vp.note = note;
                        vp.retune = true;
                    }
                }
            } else {
                let note = self.internal_note(scale);
                let vp = &mut self.voices[v];
                vp.active = true;
                vp.gate_target = 1.0;
                if (vp.note - note).abs() > 1e-4 {
                    vp.note = note;
                    vp.retune = true;
                }
            }
            let target = self.voices[v].gate_target;
            let vp = &mut self.voices[v];
            let speed = if target > vp.gate { gate_up } else { gate_down };
            vp.gate += (target - vp.gate) * speed;
            if target <= 0.0 && vp.gate < 1e-3 && vp.active {
                vp.gate = 0.0;
                vp.active = false;
                if midi && !mono_free {
                    self.alloc.finished(v);
                }
            }
        }
        for v in self.layout_voices..MAX_VOICES {
            self.voices[v].active = false;
        }
    }

    /// Sets every partial's coefficients and levels, at control rate.
    fn update_partials(&mut self, frames: usize) {
        let mode = self.index(MOD_MODE).min(MOD_GRANULAR);
        let moving = mode != MOD_NONE && self.partial_count > 0;
        let any_retune = self.voices.iter().any(|v| v.retune);
        let rate = self.value(MOD_RATE).max(0.0);
        let pch = self.value(PCH_MOD).max(0.0);
        let stretch = self.value(STRETCH) * 0.01;
        let decay = self.value(DECAY).max(1.0) * 0.001;
        let hf = (self.value(HF_DAMP) * 0.01).clamp(0.0, 1.0);
        let lf = (self.value(LF_DAMP) * 0.01).clamp(0.0, 1.0);
        let quantize = self.flag(QUANTIZE);
        let scale = self.scale;
        let uni_cents = (self.value(UNI_AMT) * 0.01).clamp(0.0, 1.0) * UNISON_SPREAD_CENTS;
        let unison_gain = 1.0 / (self.unison() as f32).sqrt();
        let count_gain = 1.0 / (self.voice_harmonics.max(1) as f32).sqrt();
        let nyquist = self.sr * 0.45;
        let tick = frames as f32 / self.sr;
        let refresh = self.dirty || any_retune;
        self.rotation = (self.rotation + 1) % 4;
        let this_rotation = self.rotation;
        let mut voice_hz = [0.0f32; MAX_VOICES];
        for (hz, v) in voice_hz.iter_mut().zip(self.voices.iter()) {
            if v.active {
                *hz = note_to_hz(v.note);
            }
        }
        let wave_recalc = rate * tick;
        let sr = self.sr;
        let frames_f = frames as f32;
        let voices = self.voices;
        let mut rng = self.rng;
        for g in 0..self.partial_count.div_ceil(LANES) {
            let lanes = &mut self.lanes[g];
            for lane in 0..LANES {
                let index = g * LANES + lane;
                if index >= self.partial_count {
                    lanes.c_re[lane] = 0.0;
                    lanes.c_im[lane] = 0.0;
                    lanes.drive[lane] = 0.0;
                    lanes.out_l_step[lane] = (0.0 - lanes.out_l[lane]) / frames_f;
                    lanes.out_r_step[lane] = (0.0 - lanes.out_r[lane]) / frames_f;
                    continue;
                }
                let p = &mut self.partials[index];
                let voice = voices[p.voice as usize];
                if !voice.active && lanes.out_l[lane] == 0.0 && lanes.out_r[lane] == 0.0 {
                    lanes.drive[lane] = 0.0;
                    lanes.c_re[lane] *= 0.5;
                    lanes.c_im[lane] *= 0.5;
                    lanes.out_l_step[lane] = 0.0;
                    lanes.out_r_step[lane] = 0.0;
                    continue;
                }
                // The partial's own modulation.
                match mode {
                    MOD_CHORUS => {
                        p.phase = (p.phase + wave_recalc * p.speed).fract();
                        let tri = 1.0 - 4.0 * (p.phase - 0.5).abs();
                        p.pitch_offset = if rate > 0.0 { pch * tri } else { 0.0 };
                        p.amp = 0.6 + 0.4 * tri;
                    }
                    MOD_WANDER => {
                        p.phase += wave_recalc * p.speed;
                        if p.phase >= 1.0 {
                            p.phase -= 1.0;
                            p.amount = rng.unipolar();
                            p.speed = 0.5 + rng.unipolar();
                        }
                        let saw = 2.0 * p.phase - 1.0;
                        p.pitch_offset = pch * saw * p.amount;
                        p.amp = 0.5 + 0.5 * (1.0 - p.phase) * p.amount;
                    }
                    MOD_GRANULAR => {
                        p.wait -= tick;
                        if p.wait <= 0.0 {
                            // Irregular grains, denser with a higher rate.
                            let mean = 1.0 / rate.max(0.05);
                            p.wait = mean * (0.2 + 1.8 * rng.unipolar());
                            p.grain = rng.unipolar() * 0.7 + 0.3;
                            p.pitch_offset = pch * rng.unipolar();
                        }
                        p.grain *= (-tick / (0.15 + 0.1 / rate.max(0.5))).exp();
                        p.amp = p.grain;
                    }
                    _ => {
                        p.pitch_offset = 0.0;
                        p.amp = 1.0;
                    }
                }
                // Moving partials update in rotation, which spreads the work across four ticks.
                if refresh || (moving && index % 4 == this_rotation) {
                    let ratio = stretched_ratio(p.harmonic as usize, stretch);
                    let base = voice_hz[p.voice as usize] * ratio;
                    let hz_base = if quantize {
                        note_to_hz(quantize_to_scale(hz_to_note(base), scale))
                    } else {
                        base
                    };
                    let cents = p.pitch_offset * 100.0 + uni_cents * p.unison_spread;
                    let hz = if cents != 0.0 {
                        hz_base * 2f32.powf(cents / 1200.0)
                    } else {
                        hz_base
                    };
                    p.audible = hz < nyquist;
                    let pole = t60_to_pole(partial_decay(decay, ratio, hf, lf), sr);
                    // Energy-normalised drive: white noise in rings at about its own level.
                    p.normal = (1.0 - pole * pole).max(0.0).sqrt();
                    if p.audible {
                        let (s, c) = (TAU * hz / sr).sin_cos();
                        lanes.c_re[lane] = pole * c;
                        lanes.c_im[lane] = pole * s;
                    } else {
                        lanes.c_re[lane] = 0.0;
                        lanes.c_im[lane] = 0.0;
                        lanes.re_l[lane] = 0.0;
                        lanes.im_l[lane] = 0.0;
                        lanes.re_r[lane] = 0.0;
                        lanes.im_r[lane] = 0.0;
                    }
                }
                let gate = voice.gate;
                lanes.drive[lane] = p.normal * gate;
                let amp = if p.audible {
                    p.amp * gate * unison_gain * count_gain
                } else {
                    0.0
                };
                let (l, r) = (amp * (1.0 - p.pan.max(0.0)), amp * (1.0 + p.pan.min(0.0)));
                lanes.out_l_step[lane] = (l - lanes.out_l[lane]) / frames_f;
                lanes.out_r_step[lane] = (r - lanes.out_r[lane]) / frames_f;
            }
        }
        self.rng = rng;
        if refresh {
            for v in self.voices.iter_mut() {
                v.retune = false;
            }
        }
        self.dirty = false;
    }

    /// Clears a lane's denormals at the top of each tick.
    fn flush_denormals(&mut self) {
        const TINY: f32 = 1e-18;
        let groups = self.partial_count.div_ceil(LANES);
        for lanes in self.lanes[..groups].iter_mut() {
            for lane in 0..LANES {
                if lanes.re_l[lane].abs() < TINY {
                    lanes.re_l[lane] = 0.0;
                }
                if lanes.im_l[lane].abs() < TINY {
                    lanes.im_l[lane] = 0.0;
                }
                if lanes.re_r[lane].abs() < TINY {
                    lanes.re_r[lane] = 0.0;
                }
                if lanes.im_r[lane].abs() < TINY {
                    lanes.im_r[lane] = 0.0;
                }
            }
        }
    }

    /// One control tick of the partials, adding into the output.
    fn resonate(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let groups = self.partial_count.div_ceil(LANES);
        for lanes in self.lanes[..groups].iter_mut() {
            for i in 0..in_l.len() {
                let (x_l, x_r) = (in_l[i], in_r[i]);
                let mut sum_l = 0.0;
                let mut sum_r = 0.0;
                for lane in 0..LANES {
                    let c = lanes.c_re[lane];
                    let s = lanes.c_im[lane];
                    let g = lanes.drive[lane];
                    let re = c * lanes.re_l[lane] - s * lanes.im_l[lane] + g * x_l;
                    let im = c * lanes.im_l[lane] + s * lanes.re_l[lane];
                    lanes.re_l[lane] = re;
                    lanes.im_l[lane] = im;
                    let re2 = c * lanes.re_r[lane] - s * lanes.im_r[lane] + g * x_r;
                    let im2 = c * lanes.im_r[lane] + s * lanes.re_r[lane];
                    lanes.re_r[lane] = re2;
                    lanes.im_r[lane] = im2;
                    lanes.out_l[lane] += lanes.out_l_step[lane];
                    lanes.out_r[lane] += lanes.out_r_step[lane];
                    sum_l += re * lanes.out_l[lane];
                    sum_r += re2 * lanes.out_r[lane];
                }
                out_l[i] += sum_l;
                out_r[i] += sum_r;
            }
        }
    }

    fn shift_ratio(&self) -> f32 {
        2f32.powf(self.value(SHIFT) / 12.0)
    }
}

/// Moves a frame's bins by `ratio`: the input's spectrum, not the partials. With a hop of a
/// quarter of the frame, each bin's phase carries on correctly by quarter turns.
fn shift_spectrum(spec: &mut [Complex32], scratch: &mut [Complex32], ratio: f32, frame: u32) {
    let bins = spec.len();
    for (k, out) in scratch.iter_mut().enumerate() {
        let source = (k as f32 / ratio).round() as usize;
        if source >= bins {
            *out = Complex32::new(0.0, 0.0);
            continue;
        }
        let quarter = (k as i64 - source as i64).rem_euclid(4) as u32 * (frame % 4) % 4;
        let c = spec[source];
        *out = match quarter {
            0 => c,
            1 => Complex32::new(-c.im, c.re),
            2 => Complex32::new(-c.re, -c.im),
            _ => Complex32::new(c.im, -c.re),
        };
    }
    spec.copy_from_slice(&scratch[..bins]);
}

impl Processor for SpectralResonator {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate.max(1.0) as f32;
        let stft = [Stft::new(FRAME, HOP), Stft::new(FRAME, HOP)];
        self.scratch = vec![Complex32::new(0.0, 0.0); stft[0].bins()];
        self.stft = Some(stft);
        self.dry = LookaheadBuffer::new(FRAME + 2);
        self.dry.set_len(FRAME);
        self.limiter = PeakLimiter::new(self.sr, 0.98, 0.15);
        self.spectrogram = Some(Spectrogram::new(sample_rate, SPECTROGRAM_VIEW));
        self.alloc.prepare(sample_rate);
        self.apply_voice_settings();
        self.dirty = true;
        self.layout_voices = 0;
        self.rebuild_layout();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let view = ctx.view;
        self.scale = ctx.transport.scale;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set(id, value),
            BlockStep::Midi(message) => self.midi(message),
            BlockStep::Audio(AudioBuffer { left, right }) => {
                let n = left.len();
                if let Some(s) = self.spectrogram.as_mut() {
                    if view.is_active() {
                        s.dry(left, right, view);
                    }
                }
                let mix = (self.value(DRY_WET) * 0.01).clamp(0.0, 1.0);
                let send = db_to_gain(self.value(INPUT_SEND));
                let ratio = self.shift_ratio();
                let mut pos = 0;
                while pos < n {
                    let chunk = (TICK - self.tick_frames).min(n - pos);
                    if self.tick_frames == 0 {
                        self.rebuild_layout();
                        self.flush_denormals();
                        self.update_voices(TICK);
                        self.update_partials(TICK);
                    }
                    let mut input = [[0.0f32; TICK]; 2];
                    let mut shifted = [[0.0f32; TICK]; 2];
                    input[0][..chunk].copy_from_slice(&left[pos..pos + chunk]);
                    input[1][..chunk].copy_from_slice(&right[pos..pos + chunk]);
                    if let Some(stft) = self.stft.as_mut() {
                        let scratch = &mut self.scratch;
                        let [sl, sr] = stft;
                        let bypass = (ratio - 1.0).abs() < 1e-4;
                        let frame = &mut self.frame_count;
                        sl.process(&input[0][..chunk], &mut shifted[0][..chunk], |spec| {
                            if !bypass {
                                shift_spectrum(spec, scratch, ratio, *frame);
                            }
                            *frame = frame.wrapping_add(1);
                        });
                        let second = self.frame_count.wrapping_sub(1);
                        sr.process(&input[1][..chunk], &mut shifted[1][..chunk], |spec| {
                            if !bypass {
                                shift_spectrum(spec, scratch, ratio, second);
                            }
                        });
                    }
                    let mut wet = [[0.0f32; TICK]; 2];
                    {
                        let (wl, wr) = wet.split_at_mut(1);
                        self.resonate(
                            &shifted[0][..chunk],
                            &shifted[1][..chunk],
                            &mut wl[0][..chunk],
                            &mut wr[0][..chunk],
                        );
                    }
                    for i in 0..chunk {
                        let [dl, dr] = self.dry.process([input[0][i], input[1][i]]);
                        let (wl, wr) = self.limiter.process(wet[0][i] * send, wet[1][i] * send);
                        left[pos + i] = dl * (1.0 - mix) + wl * mix;
                        right[pos + i] = dr * (1.0 - mix) + wr * mix;
                    }
                    self.tick_frames = (self.tick_frames + chunk) % TICK;
                    pos += chunk;
                }
                if let Some(s) = self.spectrogram.as_mut() {
                    if view.is_active() {
                        s.wet(left, right, view);
                    }
                }
                if self.limiter.gain() < 0.99 {
                    self.limiter_led = 1.0;
                }
            }
        });
        if view.is_active() {
            view.publish(LIMITER_LED, &[self.limiter_led]);
            self.limiter_led = (self.limiter_led - 0.2).max(0.0);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.set(id, value);
    }

    fn reset(&mut self) {
        if let Some(stft) = self.stft.as_mut() {
            for s in stft.iter_mut() {
                s.reset();
            }
        }
        for lanes in self.lanes.iter_mut() {
            *lanes = Lanes::default();
        }
        self.dry.reset();
        self.limiter.reset();
        self.voices = [VoicePitch::default(); MAX_VOICES];
        self.alloc.reset();
        self.tick_frames = 0;
        self.frame_count = 0;
        self.dirty = true;
        if let Some(s) = self.spectrogram.as_mut() {
            s.reset();
        }
    }

    fn latency(&self) -> u32 {
        FRAME as u32
    }

    fn tail(&self) -> Tail {
        Tail::Frames((self.value(DECAY) * 0.001 * self.sr * 1.3) as u32 + FRAME as u32)
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.spectral_resonator",
    name: "Spectral Resonator",
    role: DeviceRole::AudioEffect,
    category: "Spectral",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(SpectralResonator::default()),
};
