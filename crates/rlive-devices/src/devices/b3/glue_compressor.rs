//! Glue Compressor: a bus compressor with fixed attack, release and ratio steps.

use super::common::{
    knee_db, sidechain_items, sidechain_params, sidechain_settings, sidechain_slice, time_coeff,
    SC_LEN_EQ, SC_ON,
};
use crate::dsp::follower::DetectMode;
use crate::dsp::lookahead::LookaheadBuffer;
use crate::dsp::oversample::{OversampleFactor, Oversampler};
use crate::dsp::sidechain::SidechainDetector;
use crate::dsp::util::{db_to_gain, gain_to_db};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, AudioInput, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, Ports,
    ProcessContext, Processor, Section, Status, ViewId,
};

pub const THRESHOLD: u32 = 0;
pub const RATIO: u32 = 1;
pub const ATTACK: u32 = 2;
pub const RELEASE: u32 = 3;
pub const MAKEUP: u32 = 4;
pub const RANGE: u32 = 5;
pub const MIX: u32 = 6;
pub const SOFT: u32 = 7;
pub const OVERSAMPLING: u32 = 8;
pub const SIDECHAIN: u32 = 9;

pub const RATIOS: [f32; 3] = [2.0, 4.0, 10.0];
pub const ATTACKS_MS: [f32; 7] = [0.01, 0.1, 0.3, 1.0, 3.0, 10.0, 30.0];
pub const RELEASES_S: [f32; 6] = [0.1, 0.2, 0.4, 0.6, 0.8, 1.2];
/// The Release choice meaning Auto.
pub const RELEASE_AUTO: usize = RELEASES_S.len();
/// The widest and narrowest knee, in dB: it sharpens as the ratio rises.
pub const KNEES_DB: [f32; 3] = [12.0, 8.0, 4.0];
/// What the Soft clip waveshaper limits the output to.
pub const SOFT_CEILING: f32 = 0.944_060_9; // -0.5 dB
/// The two times of Auto Release, in ms.
pub const AUTO_FAST_MS: f32 = 80.0;
pub const AUTO_SLOW_MS: f32 = 800.0;
/// How quickly Auto Release's slow base value builds up, in ms.
pub const AUTO_SLOW_ATTACK_MS: f32 = 300.0;

/// The gain reduction in dB, read by the `gain_reduction` panel; output level, then the clip LED.
pub const GAIN_REDUCTION: ViewId = ViewId(0);
pub const OUTPUT: ViewId = ViewId(1);
pub const CLIP_LED: ViewId = ViewId(3);

fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::continuous(THRESHOLD, "Threshold", -40.0, 0.0, -10.0, "dB"),
        ParamInfo::stepped(RATIO, "Ratio", &["2", "4", "10"], 1),
        ParamInfo::stepped(
            ATTACK,
            "Attack",
            &["0.01", "0.1", "0.3", "1", "3", "10", "30"],
            3,
        ),
        ParamInfo::stepped(
            RELEASE,
            "Release",
            &["0.1", "0.2", "0.4", "0.6", "0.8", "1.2", "A"],
            2,
        ),
        ParamInfo::continuous(MAKEUP, "Makeup", 0.0, 20.0, 0.0, "dB"),
        ParamInfo::continuous(RANGE, "Range", -70.0, 0.0, -70.0, "dB"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(SOFT, "Soft", false),
        ParamInfo::toggle(OVERSAMPLING, "Oversampling", false),
    ];
    p.extend(sidechain_params(SIDECHAIN, true));
    p
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Compressor",
            items: vec![
                knob(THRESHOLD),
                Item::Menu(ParamId(RATIO)),
                Item::Menu(ParamId(ATTACK)),
                Item::Menu(ParamId(RELEASE)),
            ],
        },
        Section {
            title: "Output",
            items: vec![
                knob(MAKEUP),
                knob(RANGE),
                knob(MIX),
                Item::Row(vec![Item::Toggle(ParamId(SOFT)), Item::Panel("clip_led")]),
                Item::Toggle(ParamId(OVERSAMPLING)),
                Item::Panel("gain_reduction"),
            ],
        },
        Section {
            title: "Sidechain",
            items: sidechain_items(SIDECHAIN, true),
        },
    ]
}

fn step(v: f64, len: usize) -> usize {
    (v.round().max(0.0) as usize).min(len - 1)
}

pub struct GlueCompressor {
    v: Vec<f64>,
    sr: f32,
    max_block: usize,
    detector: SidechainDetector,
    os: [Oversampler; 2],
    /// Delays the dry signal to match the oversampled path.
    dry_delay: LookaheadBuffer<2>,
    gains: Vec<f32>,
    dry: [Vec<f32>; 2],
    /// Gain reduction in dB. With Auto Release the fast part of it.
    env_db: f32,
    slow_db: f32,
    /// The gain at the end of the last block, for interpolating at the higher rate.
    last_gain: f32,
    dirty: bool,
    clip_led: f32,
}

impl Default for GlueCompressor {
    fn default() -> Self {
        let mut d = Self {
            v: Vec::new(),
            sr: 48_000.0,
            max_block: 0,
            detector: SidechainDetector::new(),
            os: [
                Oversampler::new(OversampleFactor::X2, 1),
                Oversampler::new(OversampleFactor::X2, 1),
            ],
            dry_delay: LookaheadBuffer::new(1),
            gains: Vec::new(),
            dry: [Vec::new(), Vec::new()],
            env_db: 0.0,
            slow_db: 0.0,
            last_gain: 1.0,
            dirty: true,
            clip_led: 0.0,
        };
        let params = params();
        d.v = vec![0.0; (SIDECHAIN + SC_LEN_EQ) as usize];
        for p in &params {
            d.v[p.id as usize] = p.default;
        }
        d.allocate(48_000.0, 4096);
        d
    }
}

impl GlueCompressor {
    fn allocate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.detector.prepare(sample_rate, self.max_block);
        self.os = [
            Oversampler::new(OversampleFactor::X2, self.max_block),
            Oversampler::new(OversampleFactor::X2, self.max_block),
        ];
        let latency = self.os[0].latency() as usize;
        self.dry_delay = LookaheadBuffer::new(latency);
        self.gains = vec![0.0; self.max_block];
        self.dry = [vec![0.0; self.max_block], vec![0.0; self.max_block]];
        self.sync_oversampling();
    }

    fn oversampling(&self) -> bool {
        self.v[OVERSAMPLING as usize] >= 0.5
    }

    fn sync_oversampling(&mut self) {
        let len = if self.oversampling() {
            self.os[0].latency() as usize
        } else {
            0
        };
        self.dry_delay.set_len(len);
    }

    fn sync(&mut self) {
        if self.dirty {
            self.dirty = false;
            let s = sidechain_settings(&self.v, SIDECHAIN, true, DetectMode::Peak, 0.0, 0.0);
            self.detector.set(&s);
        }
    }

    fn render(&mut self, audio: AudioBuffer, external: Option<AudioInput>) {
        self.sync();
        let external = external.filter(|_| self.v[(SIDECHAIN + SC_ON) as usize] >= 0.5);
        let threshold = self.v[THRESHOLD as usize] as f32;
        let ratio_i = step(self.v[RATIO as usize], RATIOS.len());
        let slope = 1.0 - 1.0 / RATIOS[ratio_i];
        let knee = KNEES_DB[ratio_i];
        let range = (-self.v[RANGE as usize] as f32).max(0.0);
        let attack = time_coeff(
            ATTACKS_MS[step(self.v[ATTACK as usize], ATTACKS_MS.len())],
            self.sr,
        );
        let release_i = step(self.v[RELEASE as usize], RELEASE_AUTO + 1);
        let auto = release_i == RELEASE_AUTO;
        let release = if auto {
            time_coeff(AUTO_FAST_MS, self.sr)
        } else {
            time_coeff(RELEASES_S[release_i] * 1000.0, self.sr)
        };
        let slow_attack = time_coeff(AUTO_SLOW_ATTACK_MS, self.sr);
        let slow_release = time_coeff(AUTO_SLOW_MS, self.sr);
        let makeup = db_to_gain(self.v[MAKEUP as usize] as f32);
        let mix = self.v[MIX as usize] as f32;
        let soft = self.v[SOFT as usize] >= 0.5;
        let oversampling = self.oversampling();

        let frames = audio.frames();
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(self.max_block);
            let ext = sidechain_slice(external, pos, n);
            let (left, right) = (
                &mut audio.left[pos..pos + n],
                &mut audio.right[pos..pos + n],
            );
            let key = self.detector.process(left, right, ext);
            for i in 0..n {
                let over = gain_to_db(key.level[i]) - threshold;
                let target = knee_db(over, knee, slope).min(range);
                let c = if target > self.env_db {
                    attack
                } else {
                    release
                };
                self.env_db = target + (self.env_db - target) * c;
                if auto {
                    let c = if target > self.slow_db {
                        slow_attack
                    } else {
                        slow_release
                    };
                    self.slow_db = target + (self.slow_db - target) * c;
                }
                let gr = if auto {
                    self.env_db.max(self.slow_db)
                } else {
                    self.env_db
                };
                self.gains[i] = db_to_gain(-gr) * makeup;
            }

            let mut clipped = false;
            let shape = |x: f32, clipped: &mut bool| -> f32 {
                if soft {
                    *clipped |= x.abs() > SOFT_CEILING;
                    SOFT_CEILING * (x / SOFT_CEILING).tanh()
                } else {
                    x
                }
            };

            if oversampling {
                for i in 0..n {
                    let [dl, dr] = self.dry_delay.process([left[i], right[i]]);
                    self.dry[0][i] = dl;
                    self.dry[1][i] = dr;
                }
                let gains = &self.gains[..n];
                let mut last = self.last_gain;
                let [os_l, os_r] = &mut self.os;
                os_l.process_in_place(left, |hl| {
                    os_r.process_in_place(right, |hr| {
                        // Up to the frame's gain, then along to the next, at the higher rate.
                        for (i, &gain) in gains.iter().enumerate() {
                            let g0 = last + (gain - last) * 0.5;
                            let (a, b) = (i * 2, i * 2 + 1);
                            hl[a] = shape(hl[a] * g0, &mut clipped);
                            hr[a] = shape(hr[a] * g0, &mut clipped);
                            hl[b] = shape(hl[b] * gain, &mut clipped);
                            hr[b] = shape(hr[b] * gain, &mut clipped);
                            last = gain;
                        }
                    });
                });
                self.last_gain = last;
                for i in 0..n {
                    left[i] = self.dry[0][i] * (1.0 - mix) + left[i] * mix;
                    right[i] = self.dry[1][i] * (1.0 - mix) + right[i] * mix;
                }
            } else {
                for i in 0..n {
                    let (l, r) = (left[i], right[i]);
                    let (wl, wr) = (
                        shape(l * self.gains[i], &mut clipped),
                        shape(r * self.gains[i], &mut clipped),
                    );
                    left[i] = l * (1.0 - mix) + wl * mix;
                    right[i] = r * (1.0 - mix) + wr * mix;
                }
                if n > 0 {
                    self.last_gain = self.gains[n - 1];
                }
            }

            if clipped {
                self.clip_led = 2.0;
            } else if self.clip_led < 1.0 && left.iter().chain(right.iter()).any(|s| s.abs() > 1.0)
            {
                self.clip_led = 1.0;
            }

            self.detector.listen(&mut AudioBuffer { left, right });
            pos += n;
        }
    }
}

impl Processor for GlueCompressor {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.allocate(sample_rate, max_block);
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let sidechain = ctx.sidechain;
        let mut pos = 0;
        self.clip_led = 0.0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => {
                let frames = audio.frames();
                self.render(audio, sidechain_slice(sidechain, pos, frames));
                pos += frames;
            }
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let (l, r) = (peak(ctx.audio.left), peak(ctx.audio.right));
            ctx.view.publish(
                GAIN_REDUCTION,
                &[self.env_db.max(self.slow_db), l, r, self.clip_led],
            );
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == OVERSAMPLING {
            self.sync_oversampling();
        }
        if id >= SIDECHAIN {
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        self.env_db = 0.0;
        self.slow_db = 0.0;
        self.last_gain = 1.0;
        self.detector.reset();
        self.dry_delay.reset();
        for os in &mut self.os {
            os.reset();
        }
    }

    fn latency(&self) -> u32 {
        self.dry_delay.latency()
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.glue_compressor",
    name: "Glue Compressor",
    role: DeviceRole::AudioEffect,
    category: "Dynamics",
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
    create: |_| Box::<GlueCompressor>::default(),
};
