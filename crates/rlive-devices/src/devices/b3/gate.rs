//! Gate: passes only what's over a threshold, or only what's under it with Flip on.

use super::common::{
    ms_to_frames, sidechain_items, sidechain_params, sidechain_settings, sidechain_slice,
    SC_LEN_EQ, SC_ON,
};
use crate::dsp::follower::DetectMode;
use crate::dsp::lookahead::LookaheadBuffer;
use crate::dsp::sidechain::SidechainDetector;
use crate::dsp::util::{db_to_gain, gain_to_db};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, AudioInput, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, Ports,
    ProcessContext, Processor, Section, Status, ViewId,
};

pub const THRESHOLD: u32 = 0;
pub const RETURN: u32 = 1;
pub const ATTACK: u32 = 2;
pub const HOLD: u32 = 3;
pub const RELEASE: u32 = 4;
pub const FLOOR: u32 = 5;
pub const FLIP: u32 = 6;
pub const LOOKAHEAD: u32 = 7;
pub const SIDECHAIN: u32 = 8;

/// The Floor value that means a closed gate is silent.
pub const FLOOR_INF_DB: f64 = -70.0;
/// [`LOOKAHEAD`]'s choices, in ms.
pub const LOOKAHEAD_MS: [f32; 3] = [0.0, 1.0, 10.0];
/// How fast the level detector lets go, in seconds. Without this it would see a low-frequency
/// wave's zero crossings as the signal stopping.
pub const DETECT_RELEASE: f32 = 0.01;

/// Input and output level (linear), gate gain and key level, for the `gate_display` panel.
pub const LEVELS: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 4;

fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::continuous(THRESHOLD, "Threshold", -70.0, 6.0, -40.0, "dB"),
        ParamInfo::continuous(RETURN, "Return", 0.0, 24.0, 3.0, "dB"),
        ParamInfo::skewed(ATTACK, "Attack", 0.02, 150.0, 0.1, "ms", 3.0),
        ParamInfo::skewed(HOLD, "Hold", 1.0, 1_000.0, 10.0, "ms", 3.0),
        ParamInfo::skewed(RELEASE, "Release", 1.0, 1_000.0, 50.0, "ms", 3.0),
        ParamInfo::continuous(FLOOR, "Floor", FLOOR_INF_DB, 0.0, -40.0, "dB"),
        ParamInfo::toggle(FLIP, "Flip", false),
        ParamInfo::stepped(LOOKAHEAD, "Lookahead", &["0 ms", "1 ms", "10 ms"], 0),
    ];
    p.extend(sidechain_params(SIDECHAIN, true));
    p
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Display",
            items: vec![Item::Panel("gate_display")],
        },
        Section {
            title: "Gate",
            items: vec![
                knob(THRESHOLD),
                knob(RETURN),
                knob(ATTACK),
                knob(HOLD),
                knob(RELEASE),
                knob(FLOOR),
                Item::Toggle(ParamId(FLIP)),
                Item::Menu(ParamId(LOOKAHEAD)),
            ],
        },
        Section {
            title: "Sidechain",
            items: sidechain_items(SIDECHAIN, true),
        },
    ]
}

pub struct Gate {
    v: Vec<f64>,
    sr: f32,
    max_block: usize,
    detector: SidechainDetector,
    look: LookaheadBuffer<2>,
    /// Whether the key is over the threshold, including while Hold runs.
    detected: bool,
    hold_left: u32,
    /// How open the gate is, 0..1.
    env: f32,
    dirty: bool,
    key_peak: f32,
}

impl Default for Gate {
    fn default() -> Self {
        let params = params();
        let mut v = vec![0.0; (SIDECHAIN + SC_LEN_EQ) as usize];
        for p in &params {
            v[p.id as usize] = p.default;
        }
        let mut detector = SidechainDetector::new();
        detector.prepare(48_000.0, 4096);
        Self {
            v,
            sr: 48_000.0,
            max_block: 4096,
            detector,
            look: LookaheadBuffer::new(ms_to_frames(10.0, 48_000.0)),
            detected: false,
            hold_left: 0,
            env: 0.0,
            dirty: true,
            key_peak: 0.0,
        }
    }
}

impl Gate {
    fn sync(&mut self) {
        if self.dirty {
            self.dirty = false;
            let settings = sidechain_settings(
                &self.v,
                SIDECHAIN,
                true,
                DetectMode::Peak,
                0.0,
                DETECT_RELEASE,
            );
            self.detector.set(&settings);
        }
    }

    fn sync_lookahead(&mut self) {
        let i = (self.v[LOOKAHEAD as usize].round() as usize).min(LOOKAHEAD_MS.len() - 1);
        self.look.set_len(ms_to_frames(LOOKAHEAD_MS[i], self.sr));
    }

    fn render(&mut self, audio: AudioBuffer, external: Option<AudioInput>) {
        self.sync();
        let external = external.filter(|_| self.v[(SIDECHAIN + SC_ON) as usize] >= 0.5);
        let open_db = self.v[THRESHOLD as usize] as f32;
        let close_db = open_db - self.v[RETURN as usize] as f32;
        let attack_frames = (self.v[ATTACK as usize] as f32 * 0.001 * self.sr).max(1.0);
        let release_frames = (self.v[RELEASE as usize] as f32 * 0.001 * self.sr).max(1.0);
        let hold_frames = (self.v[HOLD as usize] as f32 * 0.001 * self.sr) as u32;
        let floor = if self.v[FLOOR as usize] <= FLOOR_INF_DB {
            0.0
        } else {
            db_to_gain(self.v[FLOOR as usize] as f32)
        };
        let flip = self.v[FLIP as usize] >= 0.5;

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
                let level = key.level[i];
                self.key_peak = self.key_peak.max(level);
                let level_db = gain_to_db(level);
                if level_db >= open_db {
                    self.detected = true;
                    self.hold_left = hold_frames;
                } else if self.detected {
                    if level_db >= close_db {
                        self.hold_left = hold_frames;
                    } else if self.hold_left > 0 {
                        self.hold_left -= 1;
                    } else {
                        self.detected = false;
                    }
                }
                let open = self.detected != flip;
                self.env = if open {
                    (self.env + 1.0 / attack_frames).min(1.0)
                } else {
                    (self.env - 1.0 / release_frames).max(0.0)
                };
                let g = floor + (1.0 - floor) * self.env;
                let [dl, dr] = self.look.process([left[i], right[i]]);
                left[i] = dl * g;
                right[i] = dr * g;
            }
            self.detector.listen(&mut AudioBuffer { left, right });
            pos += n;
        }
    }
}

impl Processor for Gate {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.detector.prepare(sample_rate, self.max_block);
        self.look = LookaheadBuffer::new(ms_to_frames(10.0, self.sr));
        self.sync_lookahead();
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let sidechain = ctx.sidechain;
        let mut pos = 0;
        let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let input = peak(ctx.audio.left).max(peak(ctx.audio.right));
        self.key_peak = 0.0;
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
            let output = peak(ctx.audio.left).max(peak(ctx.audio.right));
            ctx.view
                .publish(LEVELS, &[input, output, self.env, self.key_peak]);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == LOOKAHEAD {
            self.sync_lookahead();
        }
        if id >= SIDECHAIN {
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        self.detector.reset();
        self.look.reset();
        self.detected = false;
        self.hold_left = 0;
        self.env = 0.0;
    }

    fn latency(&self) -> u32 {
        self.look.latency()
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.gate",
    name: "Gate",
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
    create: |_| Box::<Gate>::default(),
};
