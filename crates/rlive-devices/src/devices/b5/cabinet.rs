//! Cabinet: partitioned convolution with impulse responses generated in code.
//!
//! From the manual: five speakers, the 4x12 naming scheme, more and larger speakers being louder,
//! the three mic positions and what each sounds like, Dynamic and Condenser, Mono and Dual
//! Output, and Dry/Wet. The manual only names 4x12, so the other four entries are ours, and so
//! are all the responses: built by [`generate_ir`] and shaped with filters. Nothing is sampled
//! from Ableton's product or from any real cabinet. They're not claimed to match it.
//! Latency is one partition of the convolver.

use super::common::DryLine;
use crate::dsp::convolution::{Convolver, ConvolverIr};
use crate::dsp::ir::{generate_ir, IrKind, IrSpec};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status, Tail,
};
use std::sync::Arc;

pub const SPEAKER: u32 = 0;
pub const MICROPHONE: u32 = 1;
pub const MIC_TYPE: u32 = 2;
pub const OUTPUT: u32 = 3;
pub const MIX: u32 = 4;

const PARAMS: usize = 5;

pub const SPEAKERS: [&str; 5] = ["1x12", "2x12", "4x12", "4x10", "4x10 Bass"];
pub const MICS: [&str; 3] = ["Near On-Axis", "Near Off-Axis", "Far"];
pub const MIC_TYPES: [&str; 2] = ["Dynamic", "Condenser"];

const PARTITION: usize = 128;
const DIRECT: f32 = 0.006;
/// How long a change of cabinet crossfades for, in seconds.
const FADE_SECONDS: f32 = 0.02;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(SPEAKER, "Speaker", &SPEAKERS, 2),
        ParamInfo::stepped(MICROPHONE, "Microphone", &MICS, 0),
        ParamInfo::stepped(MIC_TYPE, "Mic Type", &MIC_TYPES, 0),
        ParamInfo::stepped(OUTPUT, "Output", &["Mono", "Dual"], 0),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
    ]
}

fn layout() -> Vec<Section> {
    let menu = |id| Item::Menu(ParamId(id));
    vec![Section {
        title: "Cabinet",
        items: vec![
            menu(SPEAKER),
            menu(MICROPHONE),
            menu(MIC_TYPE),
            menu(OUTPUT),
            Item::Knob(ParamId(MIX)),
        ],
    }]
}

#[derive(Clone, Copy)]
struct SpeakerSpec {
    seed: u32,
    resonance_hz: f32,
    resonance_db: f32,
    high_cut_hz: f32,
    level_db: f32,
}

/// Ours: size sets the resonance and how soon the top falls away; count sets the level.
const SPEAKER_SPECS: [SpeakerSpec; 5] = [
    SpeakerSpec {
        seed: 11,
        resonance_hz: 110.0,
        resonance_db: 5.0,
        high_cut_hz: 5_200.0,
        level_db: -6.0,
    },
    SpeakerSpec {
        seed: 12,
        resonance_hz: 100.0,
        resonance_db: 6.0,
        high_cut_hz: 5_000.0,
        level_db: -3.0,
    },
    SpeakerSpec {
        seed: 13,
        resonance_hz: 85.0,
        resonance_db: 7.0,
        high_cut_hz: 4_600.0,
        level_db: 0.0,
    },
    SpeakerSpec {
        seed: 14,
        resonance_hz: 125.0,
        resonance_db: 5.0,
        high_cut_hz: 6_000.0,
        level_db: -1.0,
    },
    SpeakerSpec {
        seed: 15,
        resonance_hz: 60.0,
        resonance_db: 9.0,
        high_cut_hz: 3_200.0,
        level_db: 0.0,
    },
];

#[derive(Clone, Copy)]
struct MicSpec {
    /// Multiplies the speaker's top end cutoff: on-axis is brighter than off-axis.
    bright: f32,
    resonance_extra_db: f32,
    room_mix: f32,
    damping: f32,
}

const MIC_SPECS: [MicSpec; 3] = [
    MicSpec {
        bright: 1.5,
        resonance_extra_db: 0.0,
        room_mix: 0.0,
        damping: 0.1,
    },
    MicSpec {
        bright: 0.7,
        resonance_extra_db: 4.0,
        room_mix: 0.0,
        damping: 0.55,
    },
    MicSpec {
        bright: 0.9,
        resonance_extra_db: -1.0,
        room_mix: 0.35,
        damping: 0.35,
    },
];

/// Applies `f` to `signal`, in place.
fn filter(signal: &mut [f32], mut f: Biquad) {
    for s in signal.iter_mut() {
        *s = f.process(*s);
    }
}

/// The impulse response for a speaker, mic position and mic type, as samples.
fn build_response(speaker: usize, mic: usize, mic_type: usize, sample_rate: f64) -> Vec<f32> {
    let sr = sample_rate as f32;
    let s = SPEAKER_SPECS[speaker];
    let m = MIC_SPECS[mic];
    let base = generate_ir(
        &IrSpec {
            kind: IrKind::Cabinet,
            decay: 0.0,
            damping: m.damping,
            predelay: 0.0,
            seed: s.seed + mic as u32 * 16,
        },
        sample_rate,
    );
    let frames = base.left.len();
    let mut ir = base.left;
    // A direct arrival keeps the onset at sample zero: the generated body starts from silence.
    ir[0] += 1.0;
    if m.room_mix > 0.0 {
        let room = generate_ir(
            &IrSpec {
                kind: IrKind::Room,
                decay: 0.06,
                damping: 0.6,
                predelay: 0.0,
                seed: 91 + speaker as u32,
            },
            sample_rate,
        );
        ir.resize(frames.max(room.left.len()), 0.0);
        for (a, b) in ir.iter_mut().zip(room.left.iter()) {
            *a += b * m.room_mix;
        }
    }

    let mut b = Biquad::default();
    b.set(BiquadKind::HighPass, s.resonance_hz * 0.6, 0.8, 0.0, sr);
    filter(&mut ir, b);
    b.set(
        BiquadKind::Peak,
        s.resonance_hz,
        1.4,
        s.resonance_db + m.resonance_extra_db,
        sr,
    );
    filter(&mut ir, b);
    b.set(BiquadKind::LowPass, s.high_cut_hz * m.bright, 0.7, 0.0, sr);
    filter(&mut ir, b);
    filter(&mut ir, b);
    if mic_type == 0 {
        // Dynamic: a presence bump, then an early top end roll-off.
        b.set(BiquadKind::Peak, 4_000.0, 1.0, 4.0, sr);
        filter(&mut ir, b);
        b.set(BiquadKind::LowPass, 8_500.0, 0.7, 0.0, sr);
        filter(&mut ir, b);
    } else {
        // Condenser: flatter and extended, with a touch of extra top.
        b.set(BiquadKind::HighShelf, 7_000.0, 0.7, 2.5, sr);
        filter(&mut ir, b);
    }

    // Normalised by energy, so moving the mic changes the tone more than the loudness.
    let energy: f32 = ir.iter().map(|x| x * x).sum::<f32>().max(1e-9);
    let gain = 10f32.powf(s.level_db / 20.0) / energy.sqrt();
    for x in &mut ir {
        *x *= gain;
    }
    // A small direct arrival, so the very first sample of the response carries the transient.
    ir[0] += DIRECT;
    ir
}

fn index(speaker: usize, mic: usize, mic_type: usize) -> usize {
    speaker * 6 + mic * 2 + mic_type
}

/// The prepared impulse responses for every setting, indexed by [`index`].
fn build_all(sample_rate: f64) -> (Vec<Arc<ConvolverIr>>, usize, u32) {
    let mut irs = Vec::with_capacity(30);
    let mut longest = 0;
    for speaker in 0..5 {
        for mic in 0..3 {
            for mic_type in 0..2 {
                let ir = build_response(speaker, mic, mic_type, sample_rate);
                longest = longest.max(ir.len());
                irs.push(Arc::new(ConvolverIr::new(&ir, PARTITION)));
            }
        }
    }
    let partitions = irs.iter().map(|ir| ir.partitions()).max().unwrap_or(1);
    (irs, partitions, longest as u32)
}

/// A pair of convolvers, one per channel, on the same response.
struct Pair {
    left: Convolver,
    right: Convolver,
}

impl Pair {
    fn new(partitions: usize) -> Self {
        Self {
            left: Convolver::new(PARTITION, partitions),
            right: Convolver::new(PARTITION, partitions),
        }
    }

    fn set_ir(&mut self, ir: &Arc<ConvolverIr>) {
        // Every response stays alive in `Cabinet::irs`, so what comes back is never the last
        // reference and dropping it can't free.
        drop(self.left.set_ir(ir.clone()));
        drop(self.right.set_ir(ir.clone()));
    }

    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
    }
}

pub struct Cabinet {
    v: [f64; PARAMS],
    irs: Vec<Arc<ConvolverIr>>,
    /// The setting each of the two convolver pairs is on, or `usize::MAX` for none.
    loaded: [usize; 2],
    active: usize,
    pairs: [Pair; 2],
    fade: f32,
    fade_step: f32,
    dry_line: DryLine,
    scratch: [Vec<f32>; 4],
    max_block: usize,
    latency: u32,
    tail_frames: u32,
}

impl Default for Cabinet {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            irs: Vec::new(),
            loaded: [usize::MAX; 2],
            active: 0,
            pairs: [Pair::new(1), Pair::new(1)],
            fade: 1.0,
            fade_step: 1.0,
            dry_line: DryLine::new(PARTITION),
            scratch: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            max_block: 0,
            latency: PARTITION as u32,
            tail_frames: 0,
        }
    }
}

impl Cabinet {
    fn wanted(&self) -> usize {
        index(
            (self.v[SPEAKER as usize].round() as usize).min(4),
            (self.v[MICROPHONE as usize].round() as usize).min(2),
            (self.v[MIC_TYPE as usize].round() as usize).min(1),
        )
    }

    fn render(&mut self, audio: AudioBuffer) {
        if self.irs.is_empty() {
            return;
        }
        let want = self.wanted();
        if self.loaded[self.active] == usize::MAX {
            self.pairs[self.active].set_ir(&self.irs[want]);
            self.loaded[self.active] = want;
        } else if self.fade >= 1.0 && self.loaded[self.active] != want {
            // Fade to the other pair, whose history starts clean.
            let next = 1 - self.active;
            self.pairs[next].set_ir(&self.irs[want]);
            self.loaded[next] = want;
            self.active = next;
            self.fade = 0.0;
        }
        let mix = self.v[MIX as usize] as f32;
        let dual = self.v[OUTPUT as usize] >= 0.5;
        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(self.max_block);
            let (l, r) = (&mut left[pos..pos + n], &mut right[pos..pos + n]);
            let [wet_l, wet_r, old_l, old_r] = &mut self.scratch;
            let (active, previous) = (self.active, 1 - self.active);
            let fading = self.fade < 1.0;
            if dual {
                self.pairs[active].left.process(l, &mut wet_l[..n]);
                self.pairs[active].right.process(r, &mut wet_r[..n]);
                if fading {
                    self.pairs[previous].left.process(l, &mut old_l[..n]);
                    self.pairs[previous].right.process(r, &mut old_r[..n]);
                }
            } else {
                for ((m, a), b) in wet_r[..n].iter_mut().zip(l.iter()).zip(r.iter()) {
                    *m = 0.5 * (*a + *b);
                }
                let (mono, _) = wet_r.split_at(n);
                self.pairs[active].left.process(mono, &mut wet_l[..n]);
                if fading {
                    self.pairs[previous].left.process(mono, &mut old_l[..n]);
                }
            }
            let fade_step = self.fade_step;
            let mut fade = self.fade;
            let line = &mut self.dry_line;
            for i in 0..n {
                let (dry_l, dry_r) = (line.left(l[i]), line.right(r[i]));
                let (mut y_l, mut y_r) = if dual {
                    (wet_l[i], wet_r[i])
                } else {
                    (wet_l[i], wet_l[i])
                };
                if fading {
                    let (o_l, o_r) = if dual {
                        (old_l[i], old_r[i])
                    } else {
                        (old_l[i], old_l[i])
                    };
                    y_l = o_l + (y_l - o_l) * fade;
                    y_r = o_r + (y_r - o_r) * fade;
                    fade = (fade + fade_step).min(1.0);
                }
                l[i] = dry_l * (1.0 - mix) + y_l * mix;
                r[i] = dry_r * (1.0 - mix) + y_r * mix;
            }
            self.fade = fade;
            pos += n;
        }
    }
}

impl Processor for Cabinet {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        let (irs, partitions, longest) = build_all(sample_rate);
        self.irs = irs;
        self.pairs = [Pair::new(partitions), Pair::new(partitions)];
        self.loaded = [usize::MAX; 2];
        self.active = 0;
        self.fade = 1.0;
        self.fade_step = 1.0 / (FADE_SECONDS * sample_rate as f32).max(1.0);
        self.max_block = max_block.max(1);
        self.scratch = [
            vec![0.0; self.max_block],
            vec![0.0; self.max_block],
            vec![0.0; self.max_block],
            vec![0.0; self.max_block],
        ];
        self.latency = self.pairs[0].left.latency();
        self.dry_line = DryLine::new(self.latency as usize);
        self.tail_frames = longest;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for pair in &mut self.pairs {
            pair.reset();
        }
        self.dry_line.clear();
        self.fade = 1.0;
        self.loaded[1 - self.active] = usize::MAX;
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn tail(&self) -> Tail {
        Tail::from_samples(self.tail_frames)
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.cabinet",
    name: "Cabinet",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Cabinet>::default(),
};
