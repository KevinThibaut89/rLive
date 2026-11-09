//! Impulse (manual pp. 743-745): eight drum slots, each time-stretched, filtered and shaped on
//! its own, with outputs of their own.
//!
//! The manual gives: Start up to 100 ms, Transpose ±48 st, Stretch ±100 %, Decay up to 10 s, C3
//! triggers the leftmost slot with the rest going up the octave (on the white keys, since
//! eight notes span C3 to C4). It gives no filter list, modulation ranges, or the amount of
//! time-change behind Stretch and Time, so those are ours.

use super::common::read;
use super::drum_sampler::GrainStretch;
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::util::{db_to_gain, Rng};
use crate::{BuiltinInfo, DeviceInit, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, ProcessContext, Processor,
    Resource, SampleBuffer, SampleSlot, Section, Status, Tail,
};
use std::sync::Arc;

pub const SLOTS: usize = 8;
/// How many parameters each slot has.
pub const SLOT_PARAMS: u32 = 24;

pub const START: u32 = 0;
pub const TRANSPOSE: u32 = 1;
pub const TRANSP_VEL: u32 = 2;
pub const TRANSP_RND: u32 = 3;
pub const STRETCH: u32 = 4;
pub const STRETCH_MODE: u32 = 5;
pub const STRETCH_VEL: u32 = 6;
pub const FILTER_ON: u32 = 7;
pub const FILTER_TYPE: u32 = 8;
pub const FREQ: u32 = 9;
pub const RES: u32 = 10;
pub const FREQ_VEL: u32 = 11;
pub const FREQ_RND: u32 = 12;
pub const SAT_ON: u32 = 13;
pub const DRIVE: u32 = 14;
pub const DECAY: u32 = 15;
pub const DECAY_MODE: u32 = 16;
pub const PAN: u32 = 17;
pub const PAN_VEL: u32 = 18;
pub const PAN_RND: u32 = 19;
pub const VOLUME: u32 = 20;
pub const VOL_VEL: u32 = 21;
pub const MUTE: u32 = 22;
pub const SOLO: u32 = 23;

pub const GLOBAL_VOLUME: u32 = SLOTS as u32 * SLOT_PARAMS;
pub const GLOBAL_TRANSPOSE: u32 = GLOBAL_VOLUME + 1;
pub const GLOBAL_TIME: u32 = GLOBAL_VOLUME + 2;
pub const LINK: u32 = GLOBAL_VOLUME + 3;
pub const PARAM_COUNT: usize = GLOBAL_VOLUME as usize + 4;

/// The keys of the eight slots, left to right.
pub const SLOT_KEYS: [u8; SLOTS] = [60, 62, 64, 65, 67, 69, 71, 72];

pub const FILTER_TYPES: [&str; 6] = ["LP 12", "LP 24", "HP 12", "HP 24", "BP", "Notch"];

/// The parameter id of a slot's parameter.
pub const fn slot_param(slot: usize, offset: u32) -> u32 {
    slot as u32 * SLOT_PARAMS + offset
}

const SLOT_NAMES: [&str; SLOTS] = ["1", "2", "3", "4", "5", "6", "7", "8"];

const SLOT_PARAM_NAMES: [&str; SLOT_PARAMS as usize] = [
    "Start",
    "Transpose",
    "Transpose < Vel",
    "Transpose < Rnd",
    "Stretch",
    "Stretch Mode",
    "Stretch < Vel",
    "Filter",
    "Filter Type",
    "Frequency",
    "Resonance",
    "Frequency < Vel",
    "Frequency < Rnd",
    "Saturator",
    "Drive",
    "Decay",
    "Decay Mode",
    "Pan",
    "Pan < Vel",
    "Pan < Rnd",
    "Volume",
    "Volume < Vel",
    "Mute",
    "Solo",
];

pub fn params() -> Vec<ParamInfo> {
    let mut params = Vec::with_capacity(PARAM_COUNT);
    for (slot, slot_name) in SLOT_NAMES.iter().enumerate() {
        let id = |offset: u32| slot_param(slot, offset);
        let name = |offset: u32| format!("{} {}", slot_name, SLOT_PARAM_NAMES[offset as usize]);
        let bipolar = |offset: u32, default: f64| {
            ParamInfo::continuous(id(offset), &name(offset), -100.0, 100.0, default, "%")
        };
        params.extend([
            ParamInfo::continuous(id(START), &name(START), 0.0, 100.0, 0.0, "ms"),
            ParamInfo::continuous(id(TRANSPOSE), &name(TRANSPOSE), -48.0, 48.0, 0.0, "st"),
            bipolar(TRANSP_VEL, 0.0),
            bipolar(TRANSP_RND, 0.0),
            bipolar(STRETCH, 0.0),
            ParamInfo::stepped(id(STRETCH_MODE), &name(STRETCH_MODE), &["A", "B"], 0),
            bipolar(STRETCH_VEL, 0.0),
            ParamInfo::toggle(id(FILTER_ON), &name(FILTER_ON), false),
            ParamInfo::stepped(id(FILTER_TYPE), &name(FILTER_TYPE), &FILTER_TYPES, 0),
            ParamInfo::skewed(id(FREQ), &name(FREQ), 20.0, 20_000.0, 2_000.0, "Hz", 2.5),
            ParamInfo::continuous(id(RES), &name(RES), 0.0, 1.0, 0.0, ""),
            bipolar(FREQ_VEL, 0.0),
            bipolar(FREQ_RND, 0.0),
            ParamInfo::toggle(id(SAT_ON), &name(SAT_ON), false),
            ParamInfo::continuous(id(DRIVE), &name(DRIVE), 0.0, 36.0, 0.0, "dB"),
            ParamInfo::skewed(id(DECAY), &name(DECAY), 1.0, 10_000.0, 2_000.0, "ms", 3.0),
            ParamInfo::stepped(id(DECAY_MODE), &name(DECAY_MODE), &["Trigger", "Gate"], 0),
            ParamInfo::continuous(id(PAN), &name(PAN), -1.0, 1.0, 0.0, ""),
            bipolar(PAN_VEL, 0.0),
            bipolar(PAN_RND, 0.0),
            ParamInfo::continuous(id(VOLUME), &name(VOLUME), -60.0, 6.0, 0.0, "dB"),
            bipolar(VOL_VEL, 0.0),
            ParamInfo::toggle(id(MUTE), &name(MUTE), false),
            ParamInfo::toggle(id(SOLO), &name(SOLO), false),
        ]);
    }
    params.extend([
        ParamInfo::continuous(GLOBAL_VOLUME, "Volume", -60.0, 12.0, 0.0, "dB"),
        ParamInfo::continuous(GLOBAL_TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(GLOBAL_TIME, "Time", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(LINK, "Link 7+8", false),
    ]);
    params
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Slots",
            items: vec![Item::Panel("impulse_slots")],
        },
        Section {
            title: "Global",
            items: vec![
                Item::Row(vec![
                    Item::Knob(ParamId(GLOBAL_VOLUME)),
                    Item::Knob(ParamId(GLOBAL_TRANSPOSE)),
                    Item::Knob(ParamId(GLOBAL_TIME)),
                ]),
                Item::Toggle(ParamId(LINK)),
            ],
        },
    ]
}

const AUX_NAMES: [&str; SLOTS] = [
    "Impulse 1",
    "Impulse 2",
    "Impulse 3",
    "Impulse 4",
    "Impulse 5",
    "Impulse 6",
    "Impulse 7",
    "Impulse 8",
];

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.impulse",
    name: "Impulse",
    role: DeviceRole::Instrument,
    category: "Samplers",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        samples: vec![SampleSlot { name: "Slot" }; SLOTS],
        ports: Ports {
            aux_out: AUX_NAMES.to_vec(),
            ..Default::default()
        },
    },
    create: |init| Box::new(Impulse::new(init)),
};

/// One playing slot. Everything about how it plays is fixed at the note-on: the manual says
/// parameter changes only take effect on the next note.
#[derive(Clone, Copy, Default)]
struct SlotVoice {
    active: bool,
    gated: bool,
    releasing: bool,
    choking: bool,
    choke_gain: f32,
    pos: f64,
    step: f64,
    gain_l: f32,
    gain_r: f32,
    level: f32,
    decay_step: f32,
    stretch: GrainStretch,
    use_stretch: bool,
    grain: f32,
    read_step: f64,
    advance: f64,
    filter: [Filter; 2],
    filter_on: bool,
    sat_gain: f32,
}

/// See the [module docs](self).
pub struct Impulse {
    values: Vec<f64>,
    sr: f64,
    samples: [Option<Arc<SampleBuffer>>; SLOTS],
    voices: [SlotVoice; SLOTS],
    /// One voice each slot lets ring out (fading) when it restarts.
    tails: [SlotVoice; SLOTS],
    rng: Rng,
    routed: u32,
    left: Vec<f32>,
    right: Vec<f32>,
}

/// A choke or retrigger fades over this long, instead of clicking.
const CHOKE_SECONDS: f32 = 0.003;
/// Grain length of Mode A (low sounds) and Mode B (high sounds), in milliseconds.
const GRAIN_A_MS: f32 = 90.0;
const GRAIN_B_MS: f32 = 12.0;

impl Impulse {
    pub fn new(init: &DeviceInit) -> Self {
        Self {
            values: params().iter().map(|p| p.default).collect(),
            sr: 48_000.0,
            samples: std::array::from_fn(|i| init.samples.get(i).cloned().flatten()),
            voices: [SlotVoice::default(); SLOTS],
            tails: [SlotVoice::default(); SLOTS],
            rng: Rng::new(0x1b_0000),
            routed: 0,
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    fn slot_value(&self, slot: usize, offset: u32) -> f64 {
        self.values[slot_param(slot, offset) as usize]
    }

    fn hard_stop(&mut self, slot: usize) {
        if self.voices[slot].active {
            let mut tail = self.voices[slot];
            tail.choking = true;
            tail.choke_gain = 1.0;
            self.tails[slot] = tail;
            self.voices[slot].active = false;
        }
    }

    fn start_slot(&mut self, slot: usize, velocity: f32) {
        // Solo and Mute don't drop the note: they silence it at the mix below.
        let Some(sample) = self.samples[slot].clone() else {
            return;
        };
        if self.values[LINK as usize] >= 0.5 && slot >= 6 {
            let other = if slot == 6 { 7 } else { 6 };
            if self.voices[other].active {
                self.hard_stop(other);
            }
        }
        let vel = velocity as f64;
        let random = self.rng.bipolar() as f64;
        let transpose = self.slot_value(slot, TRANSPOSE)
            + self.values[GLOBAL_TRANSPOSE as usize]
            + self.slot_value(slot, TRANSP_VEL) * 0.01 * 12.0 * (vel - 0.5) * 2.0
            + self.slot_value(slot, TRANSP_RND) * 0.01 * 12.0 * random;
        let time = self.values[GLOBAL_TIME as usize] * 0.01;
        let stretch = (self.slot_value(slot, STRETCH)
            + self.slot_value(slot, STRETCH_VEL) * (vel - 0.5) * 2.0)
            .clamp(-100.0, 100.0)
            * 0.01;
        let time_factor = 2f64.powf(time);
        let stretch_factor = 2f64.powf(stretch) * time_factor;
        let pitch_ratio = 2f64.powf(transpose / 12.0);
        let rate = sample.sample_rate / self.sr;
        let mode_b = self.slot_value(slot, STRETCH_MODE).round() as i32 == 1;
        let grain_ms = if mode_b { GRAIN_B_MS } else { GRAIN_A_MS };
        let filter_on = self.slot_value(slot, FILTER_ON) >= 0.5;
        let cutoff = self.slot_value(slot, FREQ) as f32
            * 2f32.powf(
                (self.slot_value(slot, FREQ_VEL) as f32 * 0.01 * (velocity - 0.5) * 2.0
                    + self.slot_value(slot, FREQ_RND) as f32 * 0.01 * random as f32)
                    * 4.0,
            );
        let filter = FilterSettings {
            circuit: Circuit::Svf,
            kind: match self.slot_value(slot, FILTER_TYPE).round() as i32 {
                2 | 3 => FilterKind::HighPass,
                4 => FilterKind::BandPass,
                5 => FilterKind::Notch,
                _ => FilterKind::LowPass,
            },
            slope: if matches!(self.slot_value(slot, FILTER_TYPE).round() as i32, 1 | 3) {
                FilterSlope::Db24
            } else {
                FilterSlope::Db12
            },
            cutoff: cutoff.clamp(20.0, 20_000.0),
            resonance: self.slot_value(slot, RES) as f32,
            drive: 0.0,
            morph: 0.0,
        };
        let decay_seconds =
            (self.slot_value(slot, DECAY) * 0.001 * time_factor).clamp(0.001, 40.0) as f32;
        let decay_step = 1.0 / (decay_seconds * self.sr as f32);
        let pan = (self.slot_value(slot, PAN)
            + self.slot_value(slot, PAN_VEL) * 0.01 * (vel - 0.5) * 2.0
            + self.slot_value(slot, PAN_RND) * 0.01 * random)
            .clamp(-1.0, 1.0) as f32;
        let volume_db = self.slot_value(slot, VOLUME) as f32;
        let vel_gain = 1.0
            + (self.slot_value(slot, VOL_VEL) as f32 * 0.01 * (velocity - 1.0)).clamp(-1.0, 1.0);
        let gain = db_to_gain(volume_db) * vel_gain.max(0.0);
        let drive = self.slot_value(slot, DRIVE) as f32;
        let sat_on = self.slot_value(slot, SAT_ON) >= 0.5;
        let start = (self.slot_value(slot, START) * 0.001 * sample.sample_rate)
            .min((sample.len().saturating_sub(1)) as f64);
        if self.voices[slot].active {
            self.hard_stop(slot);
        }
        let mut stretch_state = GrainStretch::default();
        stretch_state.start(start);
        let releasing = self.slot_value(slot, DECAY_MODE).round() as i32 == 0;
        let v = &mut self.voices[slot];
        *v = SlotVoice {
            active: true,
            gated: true,
            releasing,
            choking: false,
            choke_gain: 1.0,
            pos: start,
            step: pitch_ratio * rate,
            gain_l: gain * (1.0 - pan).min(1.0),
            gain_r: gain * (1.0 + pan).min(1.0),
            level: 1.0,
            decay_step,
            stretch: stretch_state,
            use_stretch: (stretch_factor - 1.0).abs() > 1e-4,
            grain: grain_ms * 0.001 * self.sr as f32,
            read_step: pitch_ratio * rate,
            advance: pitch_ratio * rate / stretch_factor,
            filter: [Filter::new(); 2],
            filter_on,
            sat_gain: if sat_on {
                db_to_gain(drive).max(1.0)
            } else {
                0.0
            },
        };
        for f in &mut v.filter {
            f.prepare(self.sr);
            f.set(&filter);
        }
    }

    fn note_off(&mut self, slot: usize) {
        if self.voices[slot].active && self.slot_value(slot, DECAY_MODE).round() as i32 == 1 {
            self.voices[slot].releasing = true;
        }
        self.voices[slot].gated = false;
    }

    /// One voice into its slot's bus. Returns false when it has finished.
    #[inline]
    fn render_one(
        v: &mut SlotVoice,
        sample: &SampleBuffer,
        choke_step: f32,
        i: usize,
        left: &mut [f32],
        right: &mut [f32],
    ) -> bool {
        let n = sample.len() as f64;
        let [mut l, mut r] = if v.use_stretch {
            let s = v.stretch.next(sample, v.grain, v.read_step, v.advance);
            if v.stretch.src >= n {
                return false;
            }
            s
        } else {
            if v.pos + 1.0 >= n {
                return false;
            }
            let s = read(sample, v.pos, super::common::Interp::Normal);
            v.pos += v.step;
            s
        };
        if v.releasing {
            v.level -= v.decay_step;
            if v.level <= 0.0 {
                return false;
            }
        }
        if v.choking {
            v.choke_gain -= choke_step;
            if v.choke_gain <= 0.0 {
                return false;
            }
        }
        if v.filter_on {
            l = v.filter[0].process(l);
            r = v.filter[1].process(r);
        }
        if v.sat_gain > 0.0 {
            l = (l * v.sat_gain).tanh();
            r = (r * v.sat_gain).tanh();
        }
        let shape = v.level.max(0.0) * if v.choking { v.choke_gain } else { 1.0 };
        left[i] += l * v.gain_l * shape;
        right[i] += r * v.gain_r * shape;
        true
    }

    /// Renders `range` of the block into each slot's bus.
    fn render(&mut self, range: std::ops::Range<usize>) {
        if range.is_empty() || self.left.len() < SLOTS * range.end {
            return;
        }
        let choke_step = 1.0 / (CHOKE_SECONDS * self.sr as f32);
        for slot in 0..SLOTS {
            let Some(sample) = self.samples[slot].clone() else {
                self.voices[slot].active = false;
                self.tails[slot].active = false;
                continue;
            };
            let block = self.left.len() / SLOTS;
            let start = slot * block;
            let (bus_l, bus_r) = (
                &mut self.left[start..start + block],
                &mut self.right[start..start + block],
            );
            for i in range.clone() {
                let voice = &mut self.voices[slot];
                if voice.active && !Self::render_one(voice, &sample, choke_step, i, bus_l, bus_r) {
                    voice.active = false;
                }
                let tail = &mut self.tails[slot];
                if tail.active && !Self::render_one(tail, &sample, choke_step, i, bus_l, bus_r) {
                    tail.active = false;
                }
            }
        }
    }

    /// Hands each slot's bus to its aux output and, unless that output is tapped, the mix. A
    /// muted slot is silent everywhere; solo goes through Mute's path too.
    fn mix(
        &self,
        frames: usize,
        left: &mut [f32],
        right: &mut [f32],
        aux: &mut [rlive_core::AudioOutput],
    ) {
        let solo_any = (0..SLOTS).any(|s| self.slot_value(s, SOLO) >= 0.5);
        let volume = db_to_gain(self.values[GLOBAL_VOLUME as usize] as f32);
        let block = self.left.len() / SLOTS;
        for slot in 0..SLOTS {
            let muted =
                self.slot_value(slot, MUTE) >= 0.5 || solo_any && self.slot_value(slot, SOLO) < 0.5;
            if muted {
                continue;
            }
            let start = slot * block;
            let (bus_l, bus_r) = (
                &self.left[start..start + frames],
                &self.right[start..start + frames],
            );
            if let Some(a) = aux.get_mut(slot) {
                for (o, s) in a.left.iter_mut().zip(bus_l) {
                    *o += s * volume;
                }
                for (o, s) in a.right.iter_mut().zip(bus_r) {
                    *o += s * volume;
                }
            }
            if self.routed & (1 << slot) == 0 {
                for (o, s) in left.iter_mut().zip(bus_l) {
                    *o += s * volume;
                }
                for (o, s) in right.iter_mut().zip(bus_r) {
                    *o += s * volume;
                }
            }
        }
    }
}

impl Processor for Impulse {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate;
        self.left = vec![0.0; max_block * SLOTS];
        self.right = vec![0.0; max_block * SLOTS];
    }

    fn set_aux_routed(&mut self, mask: u32) {
        self.routed = mask;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let frames = ctx.audio.frames();
        self.left[..frames * SLOTS].fill(0.0);
        self.right[..frames * SLOTS].fill(0.0);
        for a in ctx.aux_out.iter_mut() {
            a.clear();
        }
        let this = &mut *self;
        let mut at = 0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => this.set_param(id, value),
            BlockStep::Midi(message) => this.midi(message),
            BlockStep::Audio(audio) => {
                let n = audio.frames();
                this.render(at..at + n);
                at += n;
            }
        });
        let (l, r) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
        self.mix(frames, l, r, ctx.aux_out);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for v in self.voices.iter_mut().chain(self.tails.iter_mut()) {
            v.active = false;
        }
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot as usize >= SLOTS {
            return Some(resource);
        }
        match resource.downcast::<SampleBuffer>() {
            Ok(s) => self.samples[slot as usize]
                .replace(s)
                .map(|o| o as Resource),
            Err(refused) => Some(refused),
        }
    }
}

impl Impulse {
    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { key, velocity, .. } => {
                if let Some(slot) = SLOT_KEYS.iter().position(|k| *k == key) {
                    if velocity > 0.0 {
                        self.start_slot(slot, velocity);
                    } else {
                        self.note_off(slot);
                    }
                }
            }
            MidiMessage::NoteOff { key, .. } => {
                if let Some(slot) = SLOT_KEYS.iter().position(|k| *k == key) {
                    self.note_off(slot);
                }
            }
            MidiMessage::AllNotesOff => {
                for slot in 0..SLOTS {
                    self.note_off(slot);
                }
            }
            _ => {}
        }
    }
}
