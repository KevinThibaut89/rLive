//! Envelope Follower (manual pp. 840-842): the level of a signal as modulation.
//!
//! The manual gives no ranges: Gain's -24 to +24 dB and the 0 to 1000 ms of Rise, Fall and Delay
//! are ours.

use super::common::*;
use crate::dsp::follower::EnvelopeFollower as Follower;
use crate::dsp::tempo::SyncRate;
use crate::dsp::util::db_to_gain;
use crate::dsp::DelayLine;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, ParamInfo, Ports, ProcessContext, Processor, Section, Status,
    ViewId,
};

pub const GAIN: u32 = 0;
pub const RISE: u32 = 1;
pub const FALL: u32 = 2;
pub const DELAY: u32 = 3;
pub const DELAY_MODE: u32 = 4;
pub const DELAY_SYNC: u32 = 5;
pub const SIDECHAIN: u32 = 6;
pub const SIDECHAIN_MIX: u32 = 7;

/// Direct and sidechain levels, after the scope and LED.
pub const LEVELS: ViewId = ViewId(VIEW_LEN as u32);
pub const VIEWS: usize = VIEW_LEN + 2;

const CONTROL: usize = 32;
const MAX_DELAY_SECONDS: f64 = 1.0;

fn params() -> Vec<ParamInfo> {
    let mut out = vec![
        ParamInfo::continuous(GAIN, "Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::skewed(RISE, "Rise", 0.0, 1000.0, 10.0, "ms", 3.0),
        ParamInfo::skewed(FALL, "Fall", 0.0, 1000.0, 100.0, "ms", 3.0),
        ParamInfo::skewed(DELAY, "Delay", 0.0, 1000.0, 0.0, "ms", 3.0),
        ParamInfo::stepped(DELAY_MODE, "Delay Mode", &["Time", "Sync"], 0),
        ParamInfo::stepped(DELAY_SYNC, "Delay Sync", &SyncRate::LABELS, 6),
        ParamInfo::toggle(SIDECHAIN, "Sidechain", false),
        ParamInfo::continuous(SIDECHAIN_MIX, "Sidechain Mix", 0.0, 100.0, 100.0, "%"),
    ];
    out.extend(map_info(false));
    out
}

fn layout() -> Vec<Section> {
    vec![
        map_section(),
        Section {
            title: "Display",
            items: vec![Item::Panel("modulator_scope")],
        },
        Section {
            title: "Envelope",
            items: vec![
                knob(GAIN),
                knob(RISE),
                knob(FALL),
                knob(DELAY),
                menu(DELAY_MODE),
                menu(DELAY_SYNC),
            ],
        },
        Section {
            title: "Sidechain",
            items: vec![toggle(SIDECHAIN), knob(SIDECHAIN_MIX), Item::Meter(LEVELS)],
        },
    ]
}

pub struct EnvelopeFollower {
    sample_rate: f64,
    gain: f32,
    rise_ms: f32,
    fall_ms: f32,
    delay_ms: f32,
    sync: bool,
    sync_index: f64,
    sidechain: bool,
    mix: f32,
    tempo: f64,
    follower: Follower,
    delay: DelayLine,
    position: f32,
    map: MapOutput,
    scope: ScopeFeed,
    sub: usize,
    peaks: [f32; 2],
}

impl Default for EnvelopeFollower {
    fn default() -> Self {
        let mut f = Follower::new();
        f.set_times(0.01, 0.1);
        Self {
            sample_rate: 48_000.0,
            gain: 1.0,
            rise_ms: 10.0,
            fall_ms: 100.0,
            delay_ms: 0.0,
            sync: false,
            sync_index: 6.0,
            sidechain: false,
            mix: 1.0,
            tempo: 120.0,
            follower: f,
            delay: DelayLine::new(4),
            position: 0.0,
            map: MapOutput::new(false),
            scope: ScopeFeed::default(),
            sub: 0,
            peaks: [0.0; 2],
        }
    }
}

impl EnvelopeFollower {
    fn apply(&mut self, id: u32, value: f64) {
        if self.map.apply(id, value) {
            return;
        }
        match id {
            GAIN => self.gain = db_to_gain(value as f32),
            RISE => {
                self.rise_ms = value as f32;
                self.update_times();
            }
            FALL => {
                self.fall_ms = value as f32;
                self.update_times();
            }
            DELAY => self.delay_ms = value as f32,
            DELAY_MODE => self.sync = value >= 0.5,
            DELAY_SYNC => self.sync_index = value,
            SIDECHAIN => self.sidechain = value >= 0.5,
            SIDECHAIN_MIX => self.mix = (value * 0.01) as f32,
            _ => {}
        }
    }

    fn update_times(&mut self) {
        self.follower
            .set_times(self.rise_ms * 0.001, self.fall_ms * 0.001);
    }

    fn delay_samples(&self) -> f32 {
        let seconds = if self.sync {
            SyncRate::from_index(self.sync_index).beats() * 60.0 / self.tempo.max(1.0)
        } else {
            f64::from(self.delay_ms) * 0.001
        };
        (seconds.min(MAX_DELAY_SECONDS) * self.sample_rate) as f32
    }
}

impl Processor for EnvelopeFollower {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.follower.prepare(self.sample_rate);
        self.update_times();
        self.delay = DelayLine::new((MAX_DELAY_SECONDS * self.sample_rate) as usize + 2);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.tempo = ctx.transport.tempo;
        let delay = self.delay_samples();
        let side = ctx.sidechain;
        let mut at = 0usize;
        let mut seen = 0usize;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.apply(id, value),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(a) => {
                for i in 0..a.frames() {
                    let direct = a.left[i].abs().max(a.right[i].abs());
                    self.peaks[0] = self.peaks[0].max(direct);
                    let key = match side.filter(|_| self.sidechain) {
                        Some(sc) => {
                            let s = sc
                                .left
                                .get(at + i)
                                .map_or(0.0, |l| l.abs())
                                .max(sc.right.get(at + i).map_or(0.0, |r| r.abs()));
                            self.peaks[1] = self.peaks[1].max(s);
                            direct + (s - direct) * self.mix
                        }
                        None => direct,
                    };
                    let level = self.follower.process(key * self.gain);
                    self.delay.write(level);
                    self.sub += 1;
                    if self.sub == CONTROL {
                        self.sub = 0;
                        let delayed = self.delay.read(delay + 1.0);
                        self.position = delayed.clamp(0.0, 1.0);
                        self.scope.push(self.position);
                    }
                }
                at += a.frames();
                seen += a.frames();
            }
        });
        let map = self.map;
        map.write(self.position, ctx.mod_out);
        if ctx.view.is_active() {
            self.scope
                .publish(ctx.view, seen as f32 / self.sample_rate as f32);
            ctx.view.publish(LEVELS, &self.peaks);
            self.peaks = [0.0; 2];
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.apply(id, value);
    }

    fn reset(&mut self) {
        self.follower.reset();
        self.position = 0.0;
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.envelope_follower",
    name: "Envelope Follower",
    role: DeviceRole::AudioEffect,
    category: "Modulators",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            sidechain: true,
            mod_out: MOD_PORTS.to_vec(),
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<EnvelopeFollower>::default(),
};
