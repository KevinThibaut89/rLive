//! Limiter: a lookahead peak limiter that cannot let a sample past its ceiling.

use super::common::{auto_release_ms, ms_to_frames, time_coeff};
use crate::dsp::lookahead::LookaheadBuffer;
use crate::dsp::oversample::{OversampleFactor, Oversampler};
use crate::dsp::util::{db_to_gain, gain_to_db, lerp};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status, ViewId,
};

pub const GAIN: u32 = 0;
pub const CEILING: u32 = 1;
pub const RELEASE: u32 = 2;
pub const AUTO: u32 = 3;
pub const LOOKAHEAD: u32 = 4;
pub const LINK: u32 = 5;
pub const MAXIMIZE: u32 = 6;
pub const THRESHOLD: u32 = 7;
pub const OUTPUT: u32 = 8;
pub const MODE: u32 = 9;
pub const ROUTING: u32 = 10;

const PARAM_COUNT: usize = 11;

/// [`MODE`] values.
pub const MODE_STANDARD: f64 = 0.0;
pub const MODE_SOFT_CLIP: f64 = 1.0;
pub const MODE_TRUE_PEAK: f64 = 2.0;
/// [`ROUTING`] values.
pub const ROUTING_LR: f64 = 0.0;
pub const ROUTING_MS: f64 = 1.0;

/// [`LOOKAHEAD`]'s choices, in ms.
pub const LOOKAHEAD_MS: [f32; 3] = [1.5, 3.0, 6.0];
/// How much True Peak mode oversamples by.
pub const TRUE_PEAK_FACTOR: usize = 4;
/// True Peak limits this much under the ceiling, for what the oversampling can't quite see.
const TRUE_PEAK_MARGIN_DB: f32 = -0.1;
/// How far over its ceiling Soft Clip lets the limiter run: the shaper takes care of the rest.
const SOFT_CLIP_LIMIT_DB: f32 = 1.0;
/// Where Soft Clip's curve begins, as a share of the ceiling.
const SOFT_CLIP_KNEE: f32 = 0.7;

/// Gain reduction in dB, read by the `gain_reduction` panel; the output level, the clip LED.
pub const GAIN_REDUCTION: ViewId = ViewId(0);
pub const OUTPUT_LEVEL: ViewId = ViewId(1);
pub const CLIP_LED: ViewId = ViewId(3);

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(GAIN, "Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(CEILING, "Ceiling", -24.0, 0.0, -0.3, "dB"),
        ParamInfo::skewed(RELEASE, "Release", 1.0, 3_000.0, 100.0, "ms", 3.0),
        ParamInfo::toggle(AUTO, "Auto Release", false),
        ParamInfo::stepped(LOOKAHEAD, "Lookahead", &["1.5 ms", "3 ms", "6 ms"], 1),
        ParamInfo::continuous(LINK, "Link", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(MAXIMIZE, "Maximize", false),
        ParamInfo::continuous(THRESHOLD, "Threshold", -24.0, 0.0, 0.0, "dB"),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 0.0, 0.0, "dB"),
        ParamInfo::stepped(
            MODE,
            "Ceiling Mode",
            &["Standard", "Soft Clip", "True Peak"],
            0,
        ),
        ParamInfo::stepped(ROUTING, "Routing", &["L/R", "M/S"], 0),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Input",
            items: vec![knob(GAIN), Item::Toggle(ParamId(MAXIMIZE)), knob(THRESHOLD)],
        },
        Section {
            title: "Limiter",
            items: vec![
                knob(CEILING),
                Item::Row(vec![Item::Panel("gain_reduction"), Item::Panel("clip_led")]),
                knob(LINK),
                knob(RELEASE),
                Item::Toggle(ParamId(AUTO)),
            ],
        },
        Section {
            title: "Mode",
            items: vec![
                Item::Menu(ParamId(LOOKAHEAD)),
                Item::Menu(ParamId(MODE)),
                Item::Menu(ParamId(ROUTING)),
                knob(OUTPUT),
            ],
        },
    ]
}

/// A moving average of gain over the frames a peak takes to pass through the lookahead.
#[derive(Clone)]
struct Boxcar {
    ring: Vec<f32>,
    sum: f64,
    pos: usize,
    len: usize,
}

impl Boxcar {
    fn new(capacity: usize) -> Self {
        Self {
            ring: vec![1.0; capacity],
            sum: 0.0,
            pos: 0,
            len: 0,
        }
    }

    fn set_len(&mut self, len: usize) {
        self.len = len.clamp(1, self.ring.len());
        self.reset();
    }

    fn reset(&mut self) {
        self.ring[..self.len].fill(1.0);
        self.sum = self.len as f64;
        self.pos = 0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.sum += x as f64 - self.ring[self.pos] as f64;
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % self.len;
        (self.sum / self.len as f64) as f32
    }
}

/// One stereo limiting stage. A peak in the lookahead window sets a required gain, which falls at
/// once and recovers at the release time, then the average over the window ramps toward it.
/// Each frame's gain is the mean of `len + 1` values none of which is above what that frame
/// needs, so the frame can't exceed the ceiling.
struct Stage {
    look: [LookaheadBuffer<1>; 2],
    boxes: [Boxcar; 2],
    held: [f32; 2],
    /// A slow average of the gain reduction, in dB, for Auto Release.
    slow_db: f32,
    sample_rate: f32,
    auto: bool,
    release: f32,
    /// Added up by Auto Release; the gain's depth in dB, shared between channels.
    gr_peak: f32,
}

impl Stage {
    fn new(capacity: usize) -> Self {
        Self {
            look: [
                LookaheadBuffer::new(capacity),
                LookaheadBuffer::new(capacity),
            ],
            boxes: [Boxcar::new(capacity + 1), Boxcar::new(capacity + 1)],
            held: [1.0; 2],
            slow_db: 0.0,
            sample_rate: 48_000.0,
            auto: false,
            release: 0.0,
            gr_peak: 0.0,
        }
    }

    fn set_timing(&mut self, sample_rate: f32, release_ms: f32, auto: bool) {
        self.sample_rate = sample_rate;
        self.auto = auto;
        self.release = time_coeff(release_ms, sample_rate);
    }

    fn set_len(&mut self, len: usize) {
        for look in &mut self.look {
            look.set_len(len);
        }
        for b in &mut self.boxes {
            b.set_len(len + 1);
        }
    }

    fn reset(&mut self) {
        for look in &mut self.look {
            look.reset();
        }
        for b in &mut self.boxes {
            b.reset();
        }
        self.held = [1.0; 2];
        self.slow_db = 0.0;
        self.gr_peak = 0.0;
    }

    #[inline]
    fn process(&mut self, a: &mut f32, b: &mut f32, ceiling: f32, link: f32) {
        let out_a = self.look[0].process([*a])[0];
        let out_b = self.look[1].process([*b])[0];
        let required = |peak: f32| {
            if peak > ceiling {
                ceiling / peak
            } else {
                1.0
            }
        };
        let (ga, gb) = (
            required(self.look[0].window_peak()),
            required(self.look[1].window_peak()),
        );
        let linked = ga.min(gb);
        let wanted = [lerp(ga, linked, link), lerp(gb, linked, link)];
        let release = if self.auto {
            let gr_db = -gain_to_db(self.held[0].min(self.held[1]));
            self.slow_db = gr_db + (self.slow_db - gr_db) * self.slow_coeff();
            time_coeff(
                auto_release_ms(self.slow_db, gr_db, 40.0, 800.0),
                self.sample_rate,
            )
        } else {
            self.release
        };
        for (held, want) in self.held.iter_mut().zip(wanted) {
            *held = if want < *held {
                want
            } else {
                want + (*held - want) * release
            };
        }
        let g_a = self.boxes[0].process(self.held[0]);
        let g_b = self.boxes[1].process(self.held[1]);
        self.gr_peak = self.gr_peak.max(-gain_to_db(g_a.min(g_b)));
        *a = out_a * g_a;
        *b = out_b * g_b;
    }

    #[inline]
    fn slow_coeff(&self) -> f32 {
        time_coeff(400.0, self.sample_rate)
    }
}

/// A shaper that is transparent up to [`SOFT_CLIP_KNEE`] of the ceiling and approaches the
/// ceiling but never reaches it. The second value says whether the curve was in use.
#[inline]
pub fn soft_clip(x: f32, ceiling: f32) -> (f32, bool) {
    let knee = ceiling * SOFT_CLIP_KNEE;
    let mag = x.abs();
    if mag <= knee {
        return (x, false);
    }
    let over = (mag - knee) / (ceiling - knee);
    let shaped = knee + (ceiling - knee) * over.tanh();
    (shaped.copysign(x), true)
}

pub struct Limiter {
    v: Vec<f64>,
    sr: f32,
    max_block: usize,
    stages: [Stage; 2],
    os: [Oversampler; 2],
    /// Lookahead frames at the rate the stages run at; matches [`Self::latency`] once `factor`
    /// is taken into account.
    frames: u32,
    /// The input gain and output trim at the end of the last chunk, so changes ramp across it.
    in_gain: f32,
    out_gain: f32,
    led: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        let params = params();
        let mut v = vec![0.0; PARAM_COUNT];
        for p in &params {
            v[p.id as usize] = p.default;
        }
        let mut d = Self {
            v,
            sr: 48_000.0,
            max_block: 4096,
            stages: [Stage::new(1), Stage::new(1)],
            os: [
                Oversampler::new(OversampleFactor::X4, 1),
                Oversampler::new(OversampleFactor::X4, 1),
            ],
            frames: 0,
            in_gain: 1.0,
            out_gain: 1.0,
            led: 0.0,
        };
        d.allocate(48_000.0, 4096);
        d
    }
}

impl Limiter {
    fn allocate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        // Room for the longest lookahead at the rate True Peak runs the stages at.
        let capacity = ms_to_frames(6.0, self.sr) * TRUE_PEAK_FACTOR + 8;
        self.stages = [Stage::new(capacity), Stage::new(capacity)];
        self.os = [
            Oversampler::new(OversampleFactor::X4, self.max_block),
            Oversampler::new(OversampleFactor::X4, self.max_block),
        ];
        self.sync();
    }

    fn mode(&self) -> f64 {
        self.v[MODE as usize].round()
    }

    fn true_peak(&self) -> bool {
        self.mode() == MODE_TRUE_PEAK
    }

    fn mid_side(&self) -> bool {
        self.v[ROUTING as usize].round() == ROUTING_MS
    }

    fn sync(&mut self) {
        let i = (self.v[LOOKAHEAD as usize].round() as usize).min(LOOKAHEAD_MS.len() - 1);
        let base = ms_to_frames(LOOKAHEAD_MS[i], self.sr);
        let factor = if self.true_peak() {
            TRUE_PEAK_FACTOR
        } else {
            1
        };
        self.frames = (base * factor) as u32;
        let rate = self.sr * factor as f32;
        let release = self.v[RELEASE as usize] as f32;
        let auto = self.v[AUTO as usize] >= 0.5;
        for stage in &mut self.stages {
            stage.set_len(self.frames as usize);
            stage.set_timing(rate, release, auto);
        }
    }

    /// The input gain, the limiter's ceiling and the output trim.
    fn targets(&self) -> (f32, f32, f32) {
        if self.v[MAXIMIZE as usize] >= 0.5 {
            let threshold = self.v[THRESHOLD as usize] as f32;
            (
                db_to_gain(-threshold),
                1.0,
                db_to_gain(self.v[OUTPUT as usize] as f32),
            )
        } else {
            (
                db_to_gain(self.v[GAIN as usize] as f32),
                db_to_gain(self.v[CEILING as usize] as f32),
                1.0,
            )
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        let (in_target, ceiling, out_target) = self.targets();
        let link = self.v[LINK as usize] as f32;
        let mode = self.mode();
        let soft = mode == MODE_SOFT_CLIP;
        let true_peak = self.true_peak();
        let mid_side = self.mid_side();
        // The stages see this ceiling; Soft Clip and True Peak adjust it.
        let stage_ceiling = if soft {
            ceiling * db_to_gain(SOFT_CLIP_LIMIT_DB)
        } else if true_peak {
            ceiling * db_to_gain(TRUE_PEAK_MARGIN_DB)
        } else {
            ceiling
        };

        let frames = audio.frames();
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(self.max_block);
            let (left, right) = (
                &mut audio.left[pos..pos + n],
                &mut audio.right[pos..pos + n],
            );
            let dt = 1.0 / n as f32;

            // Input gain.
            let (from, to) = (self.in_gain, in_target);
            for i in 0..n {
                let g = lerp(from, to, (i + 1) as f32 * dt);
                left[i] *= g;
                right[i] *= g;
            }
            self.in_gain = to;

            let [first, second] = &mut self.stages;
            let mut run = |l: &mut [f32], r: &mut [f32]| {
                for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                    if mid_side {
                        let (mut m, mut s) = ((*a + *b) * 0.5, (*a - *b) * 0.5);
                        first.process(&mut m, &mut s, stage_ceiling, link);
                        *a = m + s;
                        *b = m - s;
                        second.process(a, b, stage_ceiling, 1.0);
                    } else {
                        first.process(a, b, stage_ceiling, link);
                    }
                }
            };
            if true_peak {
                let [os_l, os_r] = &mut self.os;
                os_l.process_in_place(left, |hl| {
                    os_r.process_in_place(right, |hr| run(hl, hr));
                });
            } else {
                run(left, right);
            }

            let (from, to) = (self.out_gain, out_target);
            for i in 0..n {
                let g = lerp(from, to, (i + 1) as f32 * dt);
                let (mut l, mut r) = (left[i], right[i]);
                if soft {
                    let (nl, cl) = soft_clip(l, ceiling);
                    let (nr, cr) = soft_clip(r, ceiling);
                    l = nl;
                    r = nr;
                    if cl || cr {
                        self.led = 2.0;
                    }
                }
                // Nothing leaves past the ceiling, whatever rounding left behind.
                let limit = ceiling * from.max(to);
                left[i] = (l * g).clamp(-limit, limit);
                right[i] = (r * g).clamp(-limit, limit);
            }
            self.out_gain = to;
            pos += n;
        }
    }

    /// Frames of lookahead a stage takes, at the base rate.
    fn lookahead_frames(&self) -> u32 {
        let factor = if self.true_peak() {
            TRUE_PEAK_FACTOR
        } else {
            1
        } as u32;
        self.frames / factor
    }
}

impl Processor for Limiter {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.allocate(sample_rate, max_block);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.led = 0.0;
        for stage in &mut self.stages {
            stage.gr_peak = 0.0;
        }
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let (l, r) = (peak(ctx.audio.left), peak(ctx.audio.right));
            let gr = self.stages[0].gr_peak.max(self.stages[1].gr_peak);
            ctx.view.publish(GAIN_REDUCTION, &[gr, l, r, self.led]);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if matches!(id, LOOKAHEAD | RELEASE | AUTO | MODE) {
            self.sync();
        }
    }

    fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
        for os in &mut self.os {
            os.reset();
        }
    }

    fn latency(&self) -> u32 {
        let stages = if self.mid_side() { 2 } else { 1 };
        let os = if self.true_peak() {
            self.os[0].latency()
        } else {
            0
        };
        self.lookahead_frames() * stages + os
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.limiter",
    name: "Limiter",
    role: DeviceRole::AudioEffect,
    category: "Dynamics",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Limiter>::default(),
};
