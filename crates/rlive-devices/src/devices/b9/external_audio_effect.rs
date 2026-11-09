//! External Audio Effect (manual pp. 610-611): a hardware processor in the device chain.

use super::common::*;
use crate::dsp::util::db_to_gain;
use crate::dsp::DelayLine;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, ParamId, ParamInfo, Ports, ProcessContext, Processor, Section,
    Status, ViewId,
};

pub const GAIN_OUT: u32 = 0;
pub const GAIN_IN: u32 = 1;
pub const DRY_WET: u32 = 2;
pub const INVERT: u32 = 3;
pub const LATENCY_MODE: u32 = 4;
pub const LATENCY_MS: u32 = 5;
pub const LATENCY_SAMPLES: u32 = 6;

/// Peak levels: going out, and coming back.
pub const PEAKS: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 2;

const MAX_DELAY_SECONDS: f64 = 2.5;

fn params() -> Vec<ParamInfo> {
    let mut out = vec![
        ParamInfo::continuous(GAIN_OUT, "Gain Out", -48.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(GAIN_IN, "Gain In", -48.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(DRY_WET, "Dry/Wet", 0.0, 100.0, 100.0, "%"),
        ParamInfo::toggle(INVERT, "Invert", false),
    ];
    out.extend(latency_params(LATENCY_MODE, LATENCY_MS, LATENCY_SAMPLES));
    out
}

fn layout() -> Vec<Section> {
    let mut latency = vec![];
    latency.extend(latency_items(LATENCY_MODE, LATENCY_MS, LATENCY_SAMPLES));
    vec![
        Section {
            title: "I/O",
            items: vec![
                Item::Panel("external_io"),
                Item::Knob(ParamId(GAIN_OUT)),
                Item::Knob(ParamId(GAIN_IN)),
            ],
        },
        Section {
            title: "Mix",
            items: vec![Item::Knob(ParamId(DRY_WET)), Item::Toggle(ParamId(INVERT))],
        },
        Section {
            title: "Latency",
            items: latency,
        },
    ]
}

pub struct ExternalAudioEffect {
    sample_rate: f64,
    gain_out: f32,
    gain_in: f32,
    wet: f32,
    invert: bool,
    latency: Latency,
    dry: [DelayLine; 2],
    peaks: [f32; 2],
}

impl Default for ExternalAudioEffect {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            gain_out: 1.0,
            gain_in: 1.0,
            wet: 1.0,
            invert: false,
            latency: Latency::default(),
            dry: [DelayLine::new(4), DelayLine::new(4)],
            peaks: [0.0; 2],
        }
    }
}

impl ExternalAudioEffect {
    fn apply(&mut self, id: u32, value: f64) {
        match id {
            GAIN_OUT => self.gain_out = db_to_gain(value as f32),
            GAIN_IN => self.gain_in = db_to_gain(value as f32),
            DRY_WET => self.wet = (value * 0.01) as f32,
            INVERT => self.invert = value >= 0.5,
            LATENCY_MODE => self.latency.in_samples = value >= 0.5,
            LATENCY_MS => self.latency.ms = value,
            LATENCY_SAMPLES => self.latency.samples = value,
            _ => {}
        }
    }
}

impl Processor for ExternalAudioEffect {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        let n = (MAX_DELAY_SECONDS * self.sample_rate) as usize + 4;
        self.dry = [DelayLine::new(n), DelayLine::new(n)];
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let ret = ctx.sidechain;
        let out = std::mem::take(&mut ctx.aux_out);
        let mut at = 0usize;
        let frames = ctx.audio.frames();
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.apply(id, value),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(a) => {
                let delay = self.latency.frames(self.sample_rate) as f32;
                let sign = if self.invert { -1.0 } else { 1.0 };
                for i in 0..a.frames() {
                    let (l, r) = (a.left[i], a.right[i]);
                    let send = (l * self.gain_out, r * self.gain_out);
                    self.peaks[0] = self.peaks[0].max(send.0.abs()).max(send.1.abs());
                    if let Some(bus) = out.first_mut() {
                        bus.left[at + i] = send.0;
                        bus.right[at + i] = send.1;
                    }
                    self.dry[0].write(l);
                    self.dry[1].write(r);
                    let dry = (self.dry[0].read(delay + 1.0), self.dry[1].read(delay + 1.0));
                    match ret {
                        Some(input) => {
                            let wet = (
                                input.left.get(at + i).copied().unwrap_or(0.0)
                                    * self.gain_in
                                    * sign,
                                input.right.get(at + i).copied().unwrap_or(0.0)
                                    * self.gain_in
                                    * sign,
                            );
                            self.peaks[1] = self.peaks[1].max(wet.0.abs()).max(wet.1.abs());
                            a.left[i] = dry.0 + (wet.0 - dry.0) * self.wet;
                            a.right[i] = dry.1 + (wet.1 - dry.1) * self.wet;
                        }
                        // Nothing to come back from: the chain stays intact rather than silent.
                        None => {
                            a.left[i] = dry.0;
                            a.right[i] = dry.1;
                        }
                    }
                }
                at += a.frames();
            }
        });
        ctx.aux_out = out;
        let _ = frames;
        if ctx.view.is_active() {
            ctx.view.publish(PEAKS, &self.peaks);
            self.peaks = [0.0; 2];
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.apply(id, value);
    }

    fn latency(&self) -> u32 {
        self.latency.frames(self.sample_rate)
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.external_audio_effect",
    name: "External Audio Effect",
    role: DeviceRole::AudioEffect,
    category: "External",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            sidechain: true,
            aux_out: vec!["To Hardware"],
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<ExternalAudioEffect>::default(),
};
