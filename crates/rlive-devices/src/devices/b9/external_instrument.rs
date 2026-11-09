//! External Instrument (manual pp. 741-742): MIDI out to hardware or another track's multitimbral
//! plug-in, audio back into the chain.

use super::common::*;
use crate::dsp::util::db_to_gain;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, ParamFlags, ParamId, ParamInfo, Ports, ProcessContext, Processor,
    Section, Status, ViewId,
};

pub const GAIN: u32 = 0;
pub const LATENCY_MODE: u32 = 1;
pub const LATENCY_MS: u32 = 2;
pub const LATENCY_SAMPLES: u32 = 3;
/// Set by the router, in samples: the path through a plug-in on another track is a block late.
pub const INTERNAL_LATENCY: u32 = 4;

/// The peak of what comes back.
pub const PEAK: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 1;

fn params() -> Vec<ParamInfo> {
    let mut out = vec![ParamInfo::continuous(GAIN, "Gain", -48.0, 24.0, 0.0, "dB")];
    out.extend(latency_params(LATENCY_MODE, LATENCY_MS, LATENCY_SAMPLES));
    out.push(
        ParamInfo::continuous(
            INTERNAL_LATENCY,
            "Internal Latency",
            0.0,
            MAX_LATENCY_SAMPLES,
            0.0,
            "",
        )
        .with_flags(ParamFlags::HIDDEN),
    );
    out
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "I/O",
            items: vec![Item::Panel("external_io"), Item::Knob(ParamId(GAIN))],
        },
        Section {
            title: "Latency",
            items: latency_items(LATENCY_MODE, LATENCY_MS, LATENCY_SAMPLES),
        },
    ]
}

pub struct ExternalInstrument {
    sample_rate: f64,
    gain: f32,
    latency: Latency,
    peak: f32,
}

impl Default for ExternalInstrument {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            gain: 1.0,
            latency: Latency::default(),
            peak: 0.0,
        }
    }
}

impl ExternalInstrument {
    fn apply(&mut self, id: u32, value: f64) {
        match id {
            GAIN => self.gain = db_to_gain(value as f32),
            LATENCY_MODE => self.latency.in_samples = value >= 0.5,
            LATENCY_MS => self.latency.ms = value,
            LATENCY_SAMPLES => self.latency.samples = value,
            INTERNAL_LATENCY => self.latency.internal = value,
            _ => {}
        }
    }
}

impl Processor for ExternalInstrument {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let ret = ctx.sidechain;
        let mut at = 0usize;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.apply(id, value),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(a) => {
                if let Some(input) = ret {
                    for i in 0..a.frames() {
                        let l = input.left.get(at + i).copied().unwrap_or(0.0) * self.gain;
                        let r = input.right.get(at + i).copied().unwrap_or(0.0) * self.gain;
                        self.peak = self.peak.max(l.abs()).max(r.abs());
                        a.left[i] += l;
                        a.right[i] += r;
                    }
                }
                at += a.frames();
            }
        });
        if ctx.view.is_active() {
            ctx.view.publish(PEAK, &[self.peak]);
            self.peak = 0.0;
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
    id: "rlive.external_instrument",
    name: "External Instrument",
    role: DeviceRole::Instrument,
    category: "External",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            sidechain: true,
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<ExternalInstrument>::default(),
};
