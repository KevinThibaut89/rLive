//! Auto Shift: monophonic pitch tracking and correction, with formant, vibrato and LFO control
//! (manual pages 566-573).
//!
//! The manual describes the Input section (Pitch Range with its latency differences, Input Gain
//! +-24 dB, Live Mode), Quantizer (Correction Strength, Smooth with a 0-200 ms time, scale, Pitch
//! Shift in scale degrees, scale awareness), MIDI (Mono with Glide, Poly to 2, 4 or 8, Attack
//! 0-1000 ms, Release 0-5 s, Note Latch, PB Range 0-48, four routes from six sources), the LFO
//! (Reset, Delay 0-1.5 s, Attack 0-2 s, nine waves, rate in ms or notes, four depths) and the
//! Pitch and Vibrato sections (Vibrato Amount 0-200 cents, Rate 2-15 Hz, Fade In, Natural,
//! Dry/Wet). Pitch-range boundaries, grain sizes, ranges of the Pitch Shift/Fine/Formant controls
//! and the Mod Depth scaling are ours. Formant Shift's +-100% is an octave of formant movement
//! here; Formant Follow is taken literally: Follow x (Pitch Shift) is added to the formant shift.
//! Shifting is pitch synchronous, in grains a whole period apart, which keeps it low-latency. The
//! Quantizer tab's Pitch Shift moves the corrected note in scale degrees.

use super::common::{hz_to_note, knob, menu, quantize_to_scale, toggle};
use super::psola::{History, ShiftSettings, ShiftVoice};
use crate::devices::b1::common::{int_param, transpose_degrees, SCALES};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings, LfoShape};
use crate::dsp::pitch::PitchDetector;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::dsp::voice::{
    GlideMode, VoiceAllocator, VoiceEvent, VoiceMode, VoiceSettings, VoiceState,
};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, MidiMessage, ParamFlags, ParamInfo, ProcessContext,
    Processor, ScaleInfo, Section, Status, Tail, ViewId,
};

pub const PITCH_RANGE: u32 = 0;
pub const INPUT_GAIN: u32 = 1;
pub const LIVE_MODE: u32 = 2;
pub const MIDI_ON: u32 = 3;
pub const VOICE_MODE: u32 = 4;
pub const VOICE_COUNT: u32 = 5;
pub const GLIDE: u32 = 6;
pub const ATTACK: u32 = 7;
pub const RELEASE: u32 = 8;
pub const NOTE_LATCH: u32 = 9;
pub const PB_RANGE: u32 = 10;
pub const STRENGTH: u32 = 11;
pub const SMOOTH: u32 = 12;
pub const SMOOTH_TIME: u32 = 13;
pub const ROOT: u32 = 14;
pub const SCALE: u32 = 15;
/// Hidden: the custom scale as a 12-bit mask, used when [`SCALE`] is on its Custom entry.
pub const CUSTOM_MASK: u32 = 16;
pub const SCALE_SHIFT: u32 = 17;
pub const USE_SCALE: u32 = 18;
pub const LFO_RESET: u32 = 19;
pub const LFO_DELAY: u32 = 20;
pub const LFO_ATTACK: u32 = 21;
pub const LFO_WAVE: u32 = 22;
pub const LFO_SYNC: u32 = 23;
pub const LFO_RATE: u32 = 24;
pub const LFO_DIVISION: u32 = 25;
pub const LFO_PITCH: u32 = 26;
pub const LFO_FORMANT: u32 = 27;
pub const LFO_VOLUME: u32 = 28;
pub const LFO_PAN: u32 = 29;
pub const PITCH_SHIFT: u32 = 30;
pub const PITCH_FINE: u32 = 31;
pub const FORMANT_SHIFT: u32 = 32;
pub const FORMANT_FOLLOW: u32 = 33;
pub const VIB_AMOUNT: u32 = 34;
pub const VIB_RATE: u32 = 35;
pub const VIB_FADE: u32 = 36;
pub const VIB_NATURAL: u32 = 37;
pub const DRY_WET: u32 = 38;
pub const MOD_SOURCE_PITCH: u32 = 39;
pub const MOD_DEPTH_PITCH: u32 = 40;
pub const MOD_SOURCE_FORMANT: u32 = 41;
pub const MOD_DEPTH_FORMANT: u32 = 42;
pub const MOD_SOURCE_VOLUME: u32 = 43;
pub const MOD_DEPTH_VOLUME: u32 = 44;
pub const MOD_SOURCE_PAN: u32 = 45;
pub const MOD_DEPTH_PAN: u32 = 46;
const PARAMS: usize = 47;

pub const RANGE_HIGH: usize = 0;
pub const RANGE_MID: usize = 1;
pub const RANGE_BASS: usize = 2;
pub const VOICE_MONO: usize = 0;
pub const VOICE_POLY: usize = 1;
pub const VOICE_COUNTS: [usize; 3] = [2, 4, 8];

pub const SOURCE_NONE: usize = 0;
pub const SOURCE_VELOCITY: usize = 1;
pub const SOURCE_PRESSURE: usize = 2;
pub const SOURCE_MOD_WHEEL: usize = 3;
pub const SOURCE_PITCH_BEND: usize = 4;
pub const SOURCE_NOTE_PB: usize = 5;
pub const SOURCE_SLIDE: usize = 6;
const SOURCES: [&str; 7] = [
    "None",
    "Velocity",
    "Pressure",
    "Mod Wheel",
    "Pitch Bend",
    "Note PB",
    "Slide",
];

const MAX_VOICES: usize = 8;
const TICK: usize = 32;
/// Periods shorter than this are treated as a noisy signal rather than a real pitch.
const VOICED: f32 = 0.78;

/// Lowest and highest pitch a range listens for, in hertz.
pub const RANGE_HZ: [[f32; 2]; 3] = [[260.0, 2_000.0], [110.0, 1_000.0], [45.0, 340.0]];

const WAVES: [&str; 9] = [
    "Sine",
    "Triangle",
    "Triangle 8",
    "Triangle 16",
    "Saw Up",
    "Saw Down",
    "Rectangle",
    "Random",
    "Random S&H",
];

/// `[input hz, input note, input cents, correction cents, target note, LED High, LED Mid, LED
/// Bass, onset 0/1, latency ms, active scale mask from C]` the display reads, then 128 values of held
/// notes.
pub const DISPLAY: ViewId = ViewId(0);
pub const DISPLAY_LEN: usize = 11;
pub const NOTES: ViewId = ViewId(DISPLAY_LEN as u32);
pub const VIEW_LEN: usize = DISPLAY_LEN + 128;

fn params() -> Vec<ParamInfo> {
    let mut scales: Vec<&str> = SCALES.iter().map(|(name, _)| *name).collect();
    scales.push("Custom");
    let mut p = vec![
        ParamInfo::stepped(
            PITCH_RANGE,
            "Pitch Range",
            &["High", "Mid", "Bass"],
            RANGE_MID,
        ),
        ParamInfo::continuous(INPUT_GAIN, "Input Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::toggle(LIVE_MODE, "Live Mode", false),
        ParamInfo::toggle(MIDI_ON, "MIDI On", false),
        ParamInfo::stepped(VOICE_MODE, "Voice Mode", &["Mono", "Poly"], VOICE_MONO),
        ParamInfo::stepped(VOICE_COUNT, "Voice Count", &["2", "4", "8"], 2),
        ParamInfo::skewed(GLIDE, "Glide", 0.0, 2_000.0, 0.0, "ms", 3.0),
        ParamInfo::skewed(ATTACK, "Attack Time", 0.0, 1_000.0, 2.0, "ms", 3.0),
        ParamInfo::skewed(RELEASE, "Release Time", 0.0, 5_000.0, 100.0, "ms", 3.0),
        ParamInfo::toggle(NOTE_LATCH, "Note Latch", false),
        ParamInfo::continuous(PB_RANGE, "Pitch Bend Range", 0.0, 48.0, 2.0, "st"),
        ParamInfo::continuous(STRENGTH, "Correction Strength", 0.0, 100.0, 100.0, "%"),
        ParamInfo::toggle(SMOOTH, "Smooth", true),
        ParamInfo::continuous(SMOOTH_TIME, "Smoothing Time", 0.0, 200.0, 40.0, "ms"),
        ParamInfo::stepped(
            ROOT,
            "Root",
            &[
                "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
            ],
            0,
        ),
        ParamInfo::stepped(SCALE, "Scale", &scales, scales.len() - 1),
        ParamInfo::continuous(CUSTOM_MASK, "Custom Scale", 0.0, 4095.0, 4095.0, "")
            .with_flags(ParamFlags::HIDDEN),
        int_param(SCALE_SHIFT, "Pitch Shift Degrees", -24.0, 24.0, 0.0),
        ParamInfo::toggle(USE_SCALE, "Use Current Scale", false),
        ParamInfo::toggle(LFO_RESET, "LFO Reset", false),
        ParamInfo::skewed(LFO_DELAY, "LFO Delay", 0.0, 1_500.0, 0.0, "ms", 2.0),
        ParamInfo::skewed(LFO_ATTACK, "LFO Attack", 0.0, 2_000.0, 0.0, "ms", 2.0),
        ParamInfo::stepped(LFO_WAVE, "LFO Waveform", &WAVES, 0),
        ParamInfo::toggle(LFO_SYNC, "LFO Sync", false),
        ParamInfo::skewed(LFO_RATE, "LFO Rate", 20.0, 10_000.0, 1_000.0, "ms", 3.0),
        ParamInfo::stepped(LFO_DIVISION, "LFO Division", &SyncRate::LABELS, 12),
        ParamInfo::continuous(LFO_PITCH, "LFO Pitch", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_FORMANT, "LFO Formant", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_VOLUME, "LFO Volume", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_PAN, "LFO Pan", 0.0, 100.0, 0.0, "%"),
        int_param(PITCH_SHIFT, "Pitch Shift", -24.0, 24.0, 0.0),
        ParamInfo::continuous(PITCH_FINE, "Pitch Shift Fine", -100.0, 100.0, 0.0, "ct"),
        ParamInfo::continuous(FORMANT_SHIFT, "Formant Shift", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(FORMANT_FOLLOW, "Formant Follow", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(VIB_AMOUNT, "Vibrato Amount", 0.0, 200.0, 0.0, "ct"),
        ParamInfo::continuous(VIB_RATE, "Vibrato Rate", 2.0, 15.0, 5.5, "Hz"),
        ParamInfo::skewed(VIB_FADE, "Vibrato Fade In", 0.0, 2_000.0, 0.0, "ms", 2.0),
        ParamInfo::toggle(VIB_NATURAL, "Natural Vibrato", false),
        ParamInfo::continuous(DRY_WET, "Dry/Wet", 0.0, 100.0, 100.0, "%"),
    ];
    for (source, depth, name) in [
        (MOD_SOURCE_PITCH, MOD_DEPTH_PITCH, "Pitch"),
        (MOD_SOURCE_FORMANT, MOD_DEPTH_FORMANT, "Formant"),
        (MOD_SOURCE_VOLUME, MOD_DEPTH_VOLUME, "Volume"),
        (MOD_SOURCE_PAN, MOD_DEPTH_PAN, "Pan"),
    ] {
        p.push(ParamInfo::stepped(
            source,
            &format!("{name} Mod Source"),
            &SOURCES,
            SOURCE_NONE,
        ));
        p.push(ParamInfo::continuous(
            depth,
            &format!("{name} Mod Depth"),
            -100.0,
            100.0,
            0.0,
            "%",
        ));
    }
    p
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Display",
            items: vec![Item::Panel("auto_shift_display")],
        },
        Section {
            title: "Input",
            items: vec![
                menu(PITCH_RANGE),
                knob(INPUT_GAIN),
                toggle(LIVE_MODE),
                toggle(MIDI_ON),
            ],
        },
        Section {
            title: "Quantizer",
            items: vec![
                knob(STRENGTH),
                toggle(SMOOTH),
                knob(SMOOTH_TIME),
                menu(ROOT),
                menu(SCALE),
                knob(SCALE_SHIFT),
                toggle(USE_SCALE),
            ],
        },
        Section {
            title: "MIDI",
            items: vec![
                menu(VOICE_MODE),
                menu(VOICE_COUNT),
                knob(GLIDE),
                knob(ATTACK),
                knob(RELEASE),
                toggle(NOTE_LATCH),
                knob(PB_RANGE),
                menu(MOD_SOURCE_PITCH),
                knob(MOD_DEPTH_PITCH),
                menu(MOD_SOURCE_FORMANT),
                knob(MOD_DEPTH_FORMANT),
                menu(MOD_SOURCE_VOLUME),
                knob(MOD_DEPTH_VOLUME),
                menu(MOD_SOURCE_PAN),
                knob(MOD_DEPTH_PAN),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                toggle(LFO_RESET),
                knob(LFO_DELAY),
                knob(LFO_ATTACK),
                menu(LFO_WAVE),
                toggle(LFO_SYNC),
                knob(LFO_RATE),
                menu(LFO_DIVISION),
                knob(LFO_PITCH),
                knob(LFO_FORMANT),
                knob(LFO_VOLUME),
                knob(LFO_PAN),
            ],
        },
        Section {
            title: "Pitch",
            items: vec![
                knob(PITCH_SHIFT),
                knob(PITCH_FINE),
                knob(FORMANT_SHIFT),
                knob(FORMANT_FOLLOW),
            ],
        },
        Section {
            title: "Vibrato",
            items: vec![
                knob(VIB_AMOUNT),
                knob(VIB_RATE),
                knob(VIB_FADE),
                toggle(VIB_NATURAL),
                knob(DRY_WET),
            ],
        },
    ]
}

#[derive(Clone, Copy, Default)]
struct ShiftVoiceState {
    active: bool,
    level: f32,
    target: f32,
    velocity: f32,
}

pub struct AutoShift {
    v: [f64; PARAMS],
    sr: f32,
    history: History,
    voices: Vec<ShiftVoice>,
    states: [ShiftVoiceState; MAX_VOICES],
    /// Quantizer's one voice runs in `voices[0]`.
    detectors: Vec<PitchDetector>,
    wide: Option<PitchDetector>,
    alloc: VoiceAllocator,
    lfo: Lfo,
    lfo_level: f32,
    lfo_frames: f32,
    mod_wheel: f32,
    /// The last input pitch that was clearly heard.
    input_note: f32,
    input_hz: f32,
    wide_hz: f32,
    period: f32,
    voiced: bool,
    voiced_run: u32,
    onset_led: f32,
    /// The Quantizer's smoothed correction, in semitones.
    correction: f32,
    target_note: f32,
    /// Bypassing the shifter when there is nothing to shift, crossfaded: 1 is bypass.
    bypass: f32,
    /// Vibrato.
    vibrato_phase: f32,
    vibrato_level: f32,
    natural: Rng,
    natural_rate: f32,
    natural_depth: f32,
    natural_timer: f32,
    /// Sound position of a latched chord.
    last_note_on: u64,
    clock: u64,
    /// How many samples of latency the current settings are, for the bypass and dry paths.
    latency: usize,
    range: usize,
    scale: Option<ScaleInfo>,
    notes: [f32; 128],
}

impl Default for AutoShift {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            history: History::default(),
            voices: vec![ShiftVoice::default(); MAX_VOICES],
            states: [ShiftVoiceState::default(); MAX_VOICES],
            detectors: Vec::new(),
            wide: None,
            alloc: VoiceAllocator::new(MAX_VOICES),
            lfo: Lfo::new(0x51de),
            lfo_level: 0.0,
            lfo_frames: 0.0,
            mod_wheel: 0.0,
            input_note: 69.0,
            input_hz: 0.0,
            wide_hz: 0.0,
            period: 218.0,
            voiced: false,
            voiced_run: 0,
            onset_led: 0.0,
            correction: 0.0,
            target_note: 69.0,
            bypass: 1.0,
            vibrato_phase: 0.0,
            vibrato_level: 1.0,
            natural: Rng::new(0x7713),
            natural_rate: 1.0,
            natural_depth: 1.0,
            natural_timer: 0.0,
            last_note_on: 0,
            clock: 0,
            latency: 0,
            range: RANGE_MID,
            scale: None,
            notes: [0.0; 128],
        }
    }
}

/// The latency for a pitch range, in samples. Bass needs a longer grain and so more of it; the
/// period of the lowest note in each range sets it, halved in Live Mode.
pub fn range_latency(range: usize, live: bool, sample_rate: f32) -> usize {
    let lowest = RANGE_HZ[range.min(2)][0];
    let samples = (sample_rate / lowest).ceil() as usize;
    if live {
        samples / 2
    } else {
        samples
    }
}

/// The Quantizer's 12-note mask and root: the clip's scale when asked and there is one.
pub fn quantizer_scale(
    root: usize,
    scale: usize,
    custom: u16,
    use_current: bool,
    current: Option<ScaleInfo>,
) -> ScaleInfo {
    if use_current {
        if let Some(s) = current {
            return s;
        }
    }
    let mask = SCALES.get(scale).map_or(custom & 0x0fff, |s| s.1 & 0x0fff);
    ScaleInfo {
        root: (root % 12) as u8,
        mask: if mask == 0 { 0x0fff } else { mask },
    }
}

fn lfo_shape(wave: usize) -> (LfoShape, f32) {
    match wave {
        1 => (LfoShape::Triangle, 0.0),
        2 => (LfoShape::Triangle, 8.0),
        3 => (LfoShape::Triangle, 16.0),
        4 => (LfoShape::SawUp, 0.0),
        5 => (LfoShape::SawDown, 0.0),
        6 => (LfoShape::Square, 0.0),
        7 => (LfoShape::SmoothRandom, 0.0),
        8 => (LfoShape::SampleHold, 0.0),
        _ => (LfoShape::Sine, 0.0),
    }
}

impl AutoShift {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn index(&self, id: u32) -> usize {
        self.v[id as usize].round().max(0.0) as usize
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn midi_on(&self) -> bool {
        self.flag(MIDI_ON)
    }

    fn set(&mut self, id: u32, value: f64) {
        if let Some(slot) = self.v.get_mut(id as usize) {
            if *slot == value {
                return;
            }
            *slot = value;
            match id {
                PITCH_RANGE | LIVE_MODE => self.update_latency(),
                VOICE_MODE | VOICE_COUNT | GLIDE | PB_RANGE | MIDI_ON => {
                    self.apply_voice_settings()
                }
                LFO_WAVE | LFO_SYNC | LFO_RATE | LFO_DIVISION => self.apply_lfo(),
                _ => {}
            }
        }
    }

    fn update_latency(&mut self) {
        self.range = self.index(PITCH_RANGE).min(RANGE_BASS);
        self.latency = range_latency(self.range, self.flag(LIVE_MODE), self.sr);
        for voice in self.voices.iter_mut() {
            voice.reset();
        }
    }

    fn apply_voice_settings(&mut self) {
        let mono = self.index(VOICE_MODE) == VOICE_MONO;
        let voices = if mono {
            1
        } else {
            VOICE_COUNTS[self.index(VOICE_COUNT).min(VOICE_COUNTS.len() - 1)]
        };
        let glide_time = self.value(GLIDE) * 0.001;
        self.alloc.set(&VoiceSettings {
            mode: if mono {
                VoiceMode::Legato
            } else {
                VoiceMode::Poly
            },
            polyphony: voices,
            unison: 1,
            glide: if mono && glide_time > 0.0 {
                GlideMode::Always
            } else {
                GlideMode::Off
            },
            glide_time,
            bend_range: self.value(PB_RANGE).max(0.0),
            ..Default::default()
        });
    }

    fn apply_lfo(&mut self) {
        let wave = self.index(LFO_WAVE).min(WAVES.len() - 1);
        let rate = if self.flag(LFO_SYNC) {
            LfoRate::Beats(SyncRate::from_index(self.v[LFO_DIVISION as usize]).beats())
        } else {
            LfoRate::Hz(1_000.0 / self.value(LFO_RATE).max(1.0))
        };
        self.lfo.set(&LfoSettings {
            shape: lfo_shape(wave).0,
            rate,
            ..Default::default()
        });
    }

    fn scale_info(&self) -> ScaleInfo {
        quantizer_scale(
            self.index(ROOT),
            self.index(SCALE),
            self.v[CUSTOM_MASK as usize].round().max(0.0) as u16,
            self.flag(USE_SCALE),
            self.scale,
        )
    }

    fn formant_ratio(&self, pitch_st: f32, extra_percent: f32) -> f32 {
        let shift = self.value(FORMANT_SHIFT) + extra_percent;
        let follow = (self.value(FORMANT_FOLLOW) * 0.01).clamp(0.0, 1.0);
        // +-100% is an octave of formant shift, and Follow adds that much of the pitch shift.
        2f32.powf(shift * 0.01 + follow * pitch_st / 12.0)
            .clamp(0.25, 4.0)
    }

    fn note_on_latch(&mut self) {
        if self.flag(NOTE_LATCH) && self.clock.wrapping_sub(self.last_note_on) > 2_048 {
            let states = &mut self.states;
            self.alloc
                .handle(MidiMessage::AllNotesOff, |e| apply_event(states, e));
        }
        self.last_note_on = self.clock;
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::ControlChange { cc: 1, value, .. } => {
                self.mod_wheel = value as f32 / 127.0;
            }
            MidiMessage::NoteOff { .. } if self.flag(NOTE_LATCH) => return,
            MidiMessage::NoteOn { .. } => {
                self.note_on_latch();
                self.onset();
            }
            _ => {}
        }
        let states = &mut self.states;
        self.alloc.handle(message, |e| apply_event(states, e));
    }

    /// Runs the detector over a tick, and works out whether something pitched is playing.
    fn track(&mut self, mono: &[f32], view_active: bool) {
        let index = self.range;
        let Some(detector) = self.detectors.get_mut(index) else {
            return;
        };
        let estimate = detector.process(mono);
        match estimate.filter(|p| p.confidence >= VOICED) {
            Some(p) => {
                // With MIDI the note-ons are the onsets.
                let new_onset = !self.voiced && self.voiced_run == 0 && !self.midi_on();
                self.input_hz = p.hz;
                self.input_note = hz_to_note(p.hz);
                self.period = self.sr / p.hz;
                self.voiced = true;
                self.voiced_run = 8;
                if new_onset {
                    self.onset();
                }
            }
            None => {
                if self.voiced_run > 0 {
                    self.voiced_run -= 1;
                    if self.voiced_run == 0 {
                        self.voiced = false;
                    }
                }
            }
        }
        if view_active {
            if let Some(wide) = self.wide.as_mut() {
                self.wide_hz = wide
                    .process(mono)
                    .filter(|p| p.confidence >= VOICED)
                    .map_or(0.0, |p| p.hz);
            }
        }
    }

    /// A note has begun: the LFO restarts if asked to and the vibrato fades in.
    fn onset(&mut self) {
        self.onset_led = 1.0;
        self.vibrato_level = 0.0;
        if self.flag(LFO_RESET) {
            self.lfo.retrigger();
            self.lfo_frames = 0.0;
            self.lfo_level = 0.0;
        }
    }

    fn vibrato_st(&mut self, tick: usize) -> f32 {
        let dt = tick as f32 / self.sr;
        let amount = self.value(VIB_AMOUNT) * 0.01;
        let fade = (self.value(VIB_FADE) * 0.001).max(1e-3);
        self.vibrato_level = (self.vibrato_level + dt / fade).min(1.0);
        let mut rate = self.value(VIB_RATE);
        let mut depth = 1.0;
        if self.flag(VIB_NATURAL) {
            self.natural_timer -= dt;
            if self.natural_timer <= 0.0 {
                self.natural_timer = 0.25 + self.natural.unipolar() * 0.5;
                self.natural_rate = 1.0 + self.natural.bipolar() * 0.12;
                self.natural_depth = 1.0 + self.natural.bipolar() * 0.25;
            }
            rate *= self.natural_rate;
            depth *= self.natural_depth;
        }
        self.vibrato_phase = (self.vibrato_phase + rate * dt).fract();
        let wave = (self.vibrato_phase * std::f32::consts::TAU).sin();
        amount * depth * wave * self.vibrato_level
    }

    fn lfo_value(&mut self, tick: usize) -> f32 {
        let delay = (self.value(LFO_DELAY) * 0.001 * self.sr).max(0.0);
        let attack = (self.value(LFO_ATTACK) * 0.001 * self.sr).max(1.0);
        self.lfo_frames += tick as f32;
        let t = ((self.lfo_frames - delay) / attack).clamp(0.0, 1.0);
        self.lfo_level = t;
        let [raw, _] = self.lfo.advance(tick);
        let (_, steps) = lfo_shape(self.index(LFO_WAVE).min(WAVES.len() - 1));
        let value = if steps > 0.0 {
            (raw * steps * 0.5).round() / (steps * 0.5)
        } else {
            raw
        };
        value * self.lfo_level
    }

    /// The source a Mod Routing row reads, for a voice.
    fn source(&self, source: usize, voice: usize) -> f32 {
        let v = self.alloc.voice(voice);
        match source {
            SOURCE_VELOCITY => v.note.velocity,
            SOURCE_PRESSURE => v.pressure.max(self.alloc.channel_pressure(voice)),
            SOURCE_MOD_WHEEL => self.mod_wheel,
            SOURCE_PITCH_BEND => self.alloc.channel_bend(voice),
            SOURCE_NOTE_PB => (self.alloc.note_bend(voice) / 48.0).clamp(-1.0, 1.0),
            SOURCE_SLIDE => v.slide,
            _ => 0.0,
        }
    }

    fn voice_mod(&self, source_id: u32, depth_id: u32, voice: usize) -> f32 {
        self.source(self.index(source_id), voice) * self.value(depth_id) * 0.01
    }
}

fn apply_event(states: &mut [ShiftVoiceState; MAX_VOICES], event: VoiceEvent) {
    match event {
        VoiceEvent::Start { voice, note, .. } => {
            let s = &mut states[voice];
            s.active = true;
            s.target = 1.0;
            s.velocity = note.velocity;
        }
        VoiceEvent::Legato { voice, note } => {
            let s = &mut states[voice];
            s.active = true;
            s.target = 1.0;
            s.velocity = note.velocity;
        }
        VoiceEvent::Release { voice } => states[voice].target = 0.0,
        VoiceEvent::Kill { voice } => {
            states[voice] = ShiftVoiceState::default();
        }
    }
}

impl Processor for AutoShift {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate.max(1.0) as f32;
        self.detectors = RANGE_HZ
            .iter()
            .map(|[low, high]| PitchDetector::new(sample_rate, *low, *high, 128))
            .collect();
        self.wide = Some(PitchDetector::new(sample_rate, 40.0, 2_500.0, 512));
        self.alloc.prepare(sample_rate);
        self.apply_voice_settings();
        self.lfo.prepare(sample_rate);
        self.apply_lfo();
        self.update_latency();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let view = ctx.view;
        self.scale = ctx.transport.scale;
        self.lfo
            .set_clock(&BeatClock::from_transport(ctx.transport));
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set(id, value),
            BlockStep::Midi(message) => self.midi(message),
            BlockStep::Audio(AudioBuffer { left, right }) => {
                self.render(left, right, view.is_active());
            }
        });
        if view.is_active() {
            let note = self.input_note;
            let display = [
                if self.voiced { self.input_hz } else { 0.0 },
                note.round(),
                (note - note.round()) * 100.0,
                self.correction * 100.0,
                self.target_note,
                in_range(self.wide_hz, RANGE_HIGH),
                in_range(self.wide_hz, RANGE_MID),
                in_range(self.wide_hz, RANGE_BASS),
                self.onset_led,
                self.latency as f32 * 1000.0 / self.sr,
                scale_mask_from_c(self.scale_info()) as f32,
            ];
            view.publish(DISPLAY, &display);
            self.notes.fill(0.0);
            for v in self.alloc.voices() {
                if v.state != VoiceState::Free {
                    self.notes[v.note.key as usize] = v.note.velocity.max(0.2);
                }
            }
            view.publish(NOTES, &self.notes);
            self.onset_led = (self.onset_led - 0.15).max(0.0);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.set(id, value);
    }

    fn reset(&mut self) {
        self.history.reset();
        for voice in self.voices.iter_mut() {
            voice.reset();
        }
        self.states = [ShiftVoiceState::default(); MAX_VOICES];
        for detector in self.detectors.iter_mut() {
            detector.reset();
        }
        if let Some(wide) = self.wide.as_mut() {
            wide.reset();
        }
        self.alloc.reset();
        self.voiced = false;
        self.voiced_run = 0;
        self.correction = 0.0;
        self.bypass = 1.0;
        self.lfo_frames = 0.0;
    }

    fn latency(&self) -> u32 {
        self.latency as u32
    }

    fn tail(&self) -> Tail {
        Tail::Frames(self.latency as u32 + (self.value(RELEASE) * 0.001 * self.sr) as u32)
    }
}

fn scale_mask_from_c(scale: ScaleInfo) -> u16 {
    let mut out = 0u16;
    for n in 0..12 {
        if scale.mask & (1 << n) != 0 {
            out |= 1 << ((n + scale.root as usize) % 12);
        }
    }
    out
}

fn in_range(hz: f32, range: usize) -> f32 {
    let [low, high] = RANGE_HZ[range];
    if hz >= low && hz <= high {
        1.0
    } else {
        0.0
    }
}

impl AutoShift {
    fn render(&mut self, left: &mut [f32], right: &mut [f32], view_active: bool) {
        let n = left.len();
        let mut pos = 0;
        let gain = db_to_gain(self.value(INPUT_GAIN));
        let mix = (self.value(DRY_WET) * 0.01).clamp(0.0, 1.0);
        let midi = self.midi_on();
        while pos < n {
            let chunk = TICK.min(n - pos);
            // The pitch of this stretch.
            let mut mono = [0.0f32; TICK];
            for i in 0..chunk {
                mono[i] = 0.5 * (left[pos + i] + right[pos + i]) * gain;
            }
            self.track(&mono[..chunk], view_active);
            self.clock += chunk as u64;
            let (lfo, vibrato) = {
                let v = self.vibrato_st(chunk);
                (self.lfo_value(chunk), v)
            };
            let pitch_section =
                self.v[PITCH_SHIFT as usize].round() as f32 + self.value(PITCH_FINE) * 0.01;
            let base_shift = pitch_section + vibrato + lfo * self.value(LFO_PITCH) * 0.24;
            let formant_lfo = lfo * self.value(LFO_FORMANT);
            let pan_lfo = lfo * self.value(LFO_PAN) * 0.01;
            let volume_lfo = 1.0 + lfo * self.value(LFO_VOLUME) * 0.005;

            // Voice 0 (or each MIDI voice) gets a ratio and a formant.
            let dt = chunk as f32 / self.sr;
            let latency_samples = self.latency as f32;
            let periodic = self.voiced;
            let period = self.period;
            let max_half = if self.flag(LIVE_MODE) {
                latency_samples.max(32.0) * 2.0
            } else {
                latency_samples.max(32.0)
            };
            let mut settings = [ShiftSettings {
                period: if periodic { period } else { self.sr / 200.0 },
                ratio: 1.0,
                formant: 1.0,
                latency: latency_samples,
                max_half_width: max_half,
                periodic,
            }; MAX_VOICES];
            let mut levels = [0.0f32; MAX_VOICES];
            let mut pans = [0.0f32; MAX_VOICES];
            let mut active = 0usize;

            if midi {
                self.alloc.advance(chunk);
                let up = (dt / (self.value(ATTACK).max(0.0) * 0.001 + 1e-4)).min(1.0);
                let down = (dt / (self.value(RELEASE).max(0.0) * 0.001 + 1e-4)).min(1.0);
                for v in 0..MAX_VOICES {
                    let state = self.alloc.voice(v).state;
                    let target = self.states[v].target;
                    let speed = if target > self.states[v].level {
                        up
                    } else {
                        down
                    };
                    self.states[v].level += (target - self.states[v].level) * speed;
                    if state == VoiceState::Free {
                        self.states[v].level = 0.0;
                    }
                    if target <= 0.0 && self.states[v].level < 1e-3 && state != VoiceState::Free {
                        self.alloc.finished(v);
                        self.states[v] = ShiftVoiceState::default();
                    }
                    if self.alloc.voice(v).state == VoiceState::Free {
                        continue;
                    }
                    // How far this input note is from the played note.
                    let pitch = self.alloc.pitch(v);
                    let mod_pitch = self.voice_mod(MOD_SOURCE_PITCH, MOD_DEPTH_PITCH, v);
                    let shift = (pitch - self.input_note) + base_shift + mod_pitch * 24.0;
                    let mod_formant = self.voice_mod(MOD_SOURCE_FORMANT, MOD_DEPTH_FORMANT, v);
                    settings[v].ratio = 2f32.powf(shift / 12.0);
                    settings[v].formant =
                        self.formant_ratio(pitch_section, formant_lfo + mod_formant * 100.0);
                    let volume = {
                        let m = self.voice_mod(MOD_SOURCE_VOLUME, MOD_DEPTH_VOLUME, v);
                        (1.0 + m).max(0.0)
                    };
                    levels[v] = self.states[v].level * volume;
                    pans[v] = self.voice_mod(MOD_SOURCE_PAN, MOD_DEPTH_PAN, v);
                    active += 1;
                }
            } else {
                let scale = self.scale_info();
                let strength = (self.value(STRENGTH) * 0.01).clamp(0.0, 1.0);
                if self.voiced {
                    let target = quantize_to_scale(self.input_note, Some(scale));
                    self.target_note = target;
                    let raw = (target - self.input_note) * strength;
                    let smooth = if self.flag(SMOOTH) {
                        let tau = (self.value(SMOOTH_TIME) * 0.001).max(0.0);
                        if tau > 0.0 {
                            1.0 - (-dt / tau).exp()
                        } else {
                            1.0
                        }
                    } else {
                        1.0
                    };
                    self.correction += (raw - self.correction) * smooth;
                }
                let degrees = self.v[SCALE_SHIFT as usize].round() as i32;
                let degree_shift = if degrees != 0 {
                    let key = self.target_note.round() as i32;
                    (transpose_degrees(key, degrees, scale) - key) as f32
                } else {
                    0.0
                };
                let shift = self.correction + base_shift + degree_shift;
                settings[0].ratio = 2f32.powf(shift / 12.0);
                settings[0].formant = self.formant_ratio(pitch_section + degree_shift, formant_lfo);
                levels[0] = 1.0;
                active = 1;
            }

            // Whether the shifter has anything to do. If not, a plain delay is exact.
            let shifting = midi
                || (settings[0].ratio - 1.0).abs() > 5e-4
                || (settings[0].formant - 1.0).abs() > 5e-4;
            let bypass_target = if shifting { 0.0 } else { 1.0 };
            let (bypass_start, bypass_step) = (
                self.bypass,
                (bypass_target - self.bypass) / chunk as f32 * (dt * 200.0).min(1.0),
            );
            let voice_gain = 1.0 / (active.max(1) as f32).sqrt();
            let mut bypass = bypass_start;

            for i in 0..chunk {
                let (input_l, input_r) = (left[pos + i] * gain, right[pos + i] * gain);
                self.history.push(input_l, input_r);
                let (dry_l, dry_r) = self.history.tap(self.latency);
                let (mut wet_l, mut wet_r) = (0.0f32, 0.0f32);
                if bypass < 1.0 {
                    for v in 0..MAX_VOICES {
                        if levels[v] <= 0.0 && midi {
                            continue;
                        }
                        let (l, r) = self.voices[v].next(&self.history, &settings[v]);
                        if midi || v == 0 {
                            let level = if midi { levels[v] * voice_gain } else { 1.0 };
                            let (pl, pr) = (1.0 - pans[v].max(0.0), 1.0 + pans[v].min(0.0));
                            wet_l += l * level * pl;
                            wet_r += r * level * pr;
                        }
                    }
                }
                let (wl, wr) = if bypass > 0.0 {
                    let mixed = (bypass, 1.0 - bypass);
                    (
                        dry_l * mixed.0 + wet_l * mixed.1,
                        dry_r * mixed.0 + wet_r * mixed.1,
                    )
                } else {
                    (wet_l, wet_r)
                };
                bypass = (bypass + bypass_step).clamp(0.0, 1.0);
                let (pl, pr) = (
                    (1.0 - pan_lfo.max(0.0)) * volume_lfo,
                    (1.0 + pan_lfo.min(0.0)) * volume_lfo,
                );
                let out_l = if midi && active == 0 { 0.0 } else { wl * pl };
                let out_r = if midi && active == 0 { 0.0 } else { wr * pr };
                left[pos + i] = sanitize(dry_l * (1.0 - mix) + out_l * mix);
                right[pos + i] = sanitize(dry_r * (1.0 - mix) + out_r * mix);
            }
            self.bypass = bypass;
            pos += chunk;
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.auto_shift",
    name: "Auto Shift",
    role: DeviceRole::AudioEffect,
    category: "Spectral",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(AutoShift::default()),
};
