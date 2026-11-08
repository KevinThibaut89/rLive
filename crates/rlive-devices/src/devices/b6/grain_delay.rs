//! Grain Delay: the input is cut into grains, each delayed and repitched on its own.
//!
//! The manual gives the controls: the X-Y routing, Sync with Delay Time and Offset, Spray,
//! Frequency (grain size follows it), Pitch, Random Pitch, Feedback and Dry/Wet. It gives no
//! ranges or defaults for any of them; ours are below. Each grain lasts two periods of
//! Frequency, so two grains always overlap and the Hann windows add to a constant.

use super::common::{knob, menu, numbered_beats, toggle, values, CONTROL_FRAMES, NUMBERED_LABELS};
use crate::dsp::granular::{GrainRing, GrainSettings, GranularEngine};
use crate::dsp::util::sanitize;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, ParamInfo, ParamRange, ProcessContext, Processor, Section,
    Status, Tail,
};

pub const SYNC: u32 = 0;
pub const TIME: u32 = 1;
pub const DIVISION: u32 = 2;
pub const OFFSET: u32 = 3;
pub const SPRAY: u32 = 4;
pub const FREQUENCY: u32 = 5;
pub const PITCH: u32 = 6;
pub const RANDOM_PITCH: u32 = 7;
pub const FEEDBACK: u32 = 8;
pub const MIX: u32 = 9;
pub const X_AXIS: u32 = 10;
pub const Y_AXIS: u32 = 11;
pub const X_POSITION: u32 = 12;
pub const Y_POSITION: u32 = 13;
const PARAMS: usize = 14;

const X_LABELS: [&str; 7] = [
    "Off",
    "Delay Time",
    "Spray",
    "Frequency",
    "Pitch",
    "Random",
    "Feedback",
];
const X_TARGETS: [Option<u32>; 7] = [
    None,
    Some(TIME),
    Some(SPRAY),
    Some(FREQUENCY),
    Some(PITCH),
    Some(RANDOM_PITCH),
    Some(FEEDBACK),
];
const Y_LABELS: [&str; 7] = [
    "Off",
    "Spray",
    "Frequency",
    "Pitch",
    "Random",
    "Feedback",
    "Dry/Wet",
];
const Y_TARGETS: [Option<u32>; 7] = [
    None,
    Some(SPRAY),
    Some(FREQUENCY),
    Some(PITCH),
    Some(RANDOM_PITCH),
    Some(FEEDBACK),
    Some(MIX),
];

/// Largest possible delay: 16 sixteenths at 20 BPM.
const MAX_SECONDS: f64 = 13.0;
const MAX_GRAINS: usize = 32;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::toggle(SYNC, "Sync", false),
        ParamInfo::skewed(TIME, "Delay Time", 1.0, 128.0, 10.0, "ms", 2.0),
        ParamInfo::stepped(DIVISION, "Beat Delay", &NUMBERED_LABELS, 3),
        ParamInfo::continuous(OFFSET, "Offset", -33.0, 33.0, 0.0, "%"),
        ParamInfo::skewed(SPRAY, "Spray", 0.0, 500.0, 0.0, "ms", 3.0),
        ParamInfo::skewed(FREQUENCY, "Frequency", 1.0, 150.0, 60.0, "Hz", 3.0),
        ParamInfo::continuous(PITCH, "Pitch", -36.0, 12.0, 0.0, "st"),
        ParamInfo::continuous(RANDOM_PITCH, "Random Pitch", 0.0, 36.0, 0.0, "st"),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 0.99, 0.0, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 100.0, 50.0, "%"),
        ParamInfo::stepped(X_AXIS, "X-Y X Assign", &X_LABELS, 0),
        ParamInfo::stepped(Y_AXIS, "X-Y Y Assign", &Y_LABELS, 0),
        ParamInfo::continuous(X_POSITION, "X-Y X", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(Y_POSITION, "X-Y Y", 0.0, 1.0, 0.5, ""),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Time",
            items: vec![toggle(SYNC), menu(DIVISION), knob(TIME), knob(OFFSET)],
        },
        Section {
            title: "Grains",
            items: vec![
                knob(SPRAY),
                knob(FREQUENCY),
                knob(PITCH),
                knob(RANDOM_PITCH),
                knob(FEEDBACK),
                knob(MIX),
            ],
        },
        Section {
            title: "X-Y",
            items: vec![
                menu(X_AXIS),
                knob(X_POSITION),
                menu(Y_AXIS),
                knob(Y_POSITION),
            ],
        },
    ]
}

pub struct GrainDelay {
    v: [f64; PARAMS],
    ranges: [ParamRange; PARAMS],
    sr: f32,
    rings: [GrainRing; 2],
    grains: [GranularEngine; 2],
    last: f32,
}

impl GrainDelay {
    fn seeded(seed: u32) -> Self {
        let p = params();
        let mut ranges = [ParamRange {
            min: 0.0,
            max: 1.0,
            skew: 1.0,
            quantized: false,
        }; PARAMS];
        for param in &p {
            ranges[param.id as usize] = param.range();
        }
        Self {
            v: values(&p),
            ranges,
            sr: 48_000.0,
            rings: [GrainRing::new(16), GrainRing::new(16)],
            grains: [
                GranularEngine::new(MAX_GRAINS, seed),
                GranularEngine::new(MAX_GRAINS, seed.wrapping_add(1)),
            ],
            last: 0.0,
        }
    }

    /// A value with the X-Y pad's routing applied: an assigned axis overrides the control.
    fn effective(&self, id: u32) -> f32 {
        let plain = self.v[id as usize];
        let x = self.v[X_AXIS as usize].round() as usize;
        if X_TARGETS.get(x).copied().flatten() == Some(id) {
            return self.ranges[id as usize].from_normalized(self.v[X_POSITION as usize]) as f32;
        }
        let y = self.v[Y_AXIS as usize].round() as usize;
        if Y_TARGETS.get(y).copied().flatten() == Some(id) {
            return self.ranges[id as usize].from_normalized(self.v[Y_POSITION as usize]) as f32;
        }
        plain as f32
    }

    fn render(&mut self, audio: AudioBuffer, tempo: f64) {
        let seconds = if self.v[SYNC as usize] >= 0.5 {
            numbered_beats(self.v[DIVISION as usize], self.v[OFFSET as usize]) * 60.0
                / tempo.max(1.0)
        } else {
            self.effective(TIME) as f64 * 0.001
        };
        let freq = self.effective(FREQUENCY).max(0.1);
        let pitch = 2f32.powf(self.effective(PITCH) / 12.0);
        let spray = self.effective(SPRAY) * 0.001;
        let random_pitch = self.effective(RANDOM_PITCH);
        let feedback = self.effective(FEEDBACK);
        let mix = self.effective(MIX) * 0.01;
        let settings = GrainSettings {
            rate: freq,
            size: 2.0 / freq,
            pitch,
            position: seconds as f32,
            spray,
            jitter: 0.0,
            pan_spread: 0.0,
            pitch_random: random_pitch,
        };
        for g in self.grains.iter_mut() {
            g.set(&settings);
        }
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (lc, rc) in left
            .chunks_mut(CONTROL_FRAMES)
            .zip(right.chunks_mut(CONTROL_FRAMES))
        {
            for (l, r) in lc.iter_mut().zip(rc.iter_mut()) {
                let in_l = *l;
                let in_r = *r;
                // Feedback returns the mono sum of the wet signal into both lines.
                self.rings[0].write(sanitize(in_l + self.last * feedback));
                self.rings[1].write(sanitize(in_r + self.last * feedback));
                let [wl_a, wr_a] = self.grains[0].next(&self.rings[0]);
                let [wl_b, wr_b] = self.grains[1].next(&self.rings[1]);
                let wet_l = (wl_a + wr_a) * 0.5;
                let wet_r = (wl_b + wr_b) * 0.5;
                self.last = sanitize((wet_l + wet_r) * 0.5);
                *l = in_l * (1.0 - mix) + wet_l * mix;
                *r = in_r * (1.0 - mix) + wet_r * mix;
            }
        }
    }
}

impl Default for GrainDelay {
    fn default() -> Self {
        Self::seeded(0xA11)
    }
}

impl Processor for GrainDelay {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let capacity = (sample_rate * MAX_SECONDS) as usize;
        self.rings = [GrainRing::new(capacity), GrainRing::new(capacity)];
        for g in self.grains.iter_mut() {
            g.prepare(sample_rate);
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let tempo = ctx.transport.tempo;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio, tempo),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        self.rings.iter_mut().for_each(GrainRing::clear);
        self.grains.iter_mut().for_each(GranularEngine::reset);
        self.last = 0.0;
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.grain_delay",
    name: "Grain Delay",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<GrainDelay>::default(),
};
