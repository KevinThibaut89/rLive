//! Amp: seven amplifier characters built from one signal path: preamp, interactive tone stack,
//! power stage and Presence.
//!
//! From the manual: the seven model names and their sonic descriptions, Gain and Volume, high
//! Volume also distorting on Blues, Heavy and Bass, EQ controls interacting (treble up pulling
//! bass and mid down, EQ boosts adding distortion), Presence in the power stage, Mono and Dual
//! Output, and Dry/Wet. These are our own simple models, not models of the amplifiers the manual
//! names, and they aren't claimed to match them or Live. The manual gives no numbers: every
//! gain, frequency and curve in [`MODELS`] is ours. Hi-Quality isn't in the manual; it's here
//! because the preamp aliases when driven hard, and it's off by default.

use super::common::{DcBlock, DryLine, Ramp, CONTROL_FRAMES, HI_QUALITY_FACTOR};
use crate::dsp::oversample::Oversampler;
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext,
    Processor, Section, Status,
};

pub const MODEL: u32 = 0;
pub const GAIN: u32 = 1;
pub const BASS: u32 = 2;
pub const MIDDLE: u32 = 3;
pub const TREBLE: u32 = 4;
pub const PRESENCE: u32 = 5;
pub const VOLUME: u32 = 6;
pub const OUTPUT: u32 = 7;
pub const MIX: u32 = 8;
pub const HI_QUALITY: u32 = 9;

const PARAMS: usize = 10;

pub const MODEL_NAMES: [&str; 7] = ["Clean", "Boost", "Blues", "Rock", "Lead", "Heavy", "Bass"];

/// The character of one model. Ours throughout.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    /// Preamp gain at 0% and 100% Gain, in dB.
    pub gain_lo: f32,
    pub gain_hi: f32,
    /// Offset into the preamp curve: more of it means more even harmonics.
    pub asym: f32,
    /// How close the preamp's knee is to a hard clip.
    pub hardness: f32,
    /// Two preamp stages for the high-gain models.
    pub stages: u8,
    /// High-pass before the preamp, keeping the low end tight.
    pub tight_hz: f32,
    pub bass_hz: f32,
    pub mid_hz: f32,
    pub treble_hz: f32,
    /// The power stage's headroom: low values mean Volume itself distorts.
    pub headroom: f32,
    /// A trim that levels the models.
    pub trim_db: f32,
}

pub const MODELS: [Model; 7] = [
    // Clean
    Model {
        gain_lo: 0.0,
        gain_hi: 18.0,
        asym: 0.02,
        hardness: 0.0,
        stages: 1,
        tight_hz: 60.0,
        bass_hz: 120.0,
        mid_hz: 800.0,
        treble_hz: 3_400.0,
        headroom: 8.0,
        trim_db: -3.0,
    },
    // Boost
    Model {
        gain_lo: 6.0,
        gain_hi: 30.0,
        asym: 0.05,
        hardness: 0.2,
        stages: 1,
        tight_hz: 80.0,
        bass_hz: 120.0,
        mid_hz: 1_000.0,
        treble_hz: 3_800.0,
        headroom: 6.0,
        trim_db: -6.0,
    },
    // Blues
    Model {
        gain_lo: 3.0,
        gain_hi: 24.0,
        asym: 0.12,
        hardness: 0.1,
        stages: 1,
        tight_hz: 70.0,
        bass_hz: 110.0,
        mid_hz: 650.0,
        treble_hz: 4_200.0,
        headroom: 1.2,
        trim_db: -5.0,
    },
    // Rock
    Model {
        gain_lo: 8.0,
        gain_hi: 34.0,
        asym: 0.08,
        hardness: 0.35,
        stages: 2,
        tight_hz: 100.0,
        bass_hz: 110.0,
        mid_hz: 900.0,
        treble_hz: 3_600.0,
        headroom: 5.0,
        trim_db: -8.0,
    },
    // Lead
    Model {
        gain_lo: 14.0,
        gain_hi: 48.0,
        asym: 0.06,
        hardness: 0.6,
        stages: 2,
        tight_hz: 180.0,
        bass_hz: 100.0,
        mid_hz: 700.0,
        treble_hz: 3_000.0,
        headroom: 4.0,
        trim_db: -11.0,
    },
    // Heavy
    Model {
        gain_lo: 12.0,
        gain_hi: 42.0,
        asym: 0.1,
        hardness: 0.5,
        stages: 2,
        tight_hz: 140.0,
        bass_hz: 90.0,
        mid_hz: 500.0,
        treble_hz: 3_200.0,
        headroom: 1.0,
        trim_db: -10.0,
    },
    // Bass
    Model {
        gain_lo: -3.0,
        gain_hi: 26.0,
        asym: 0.15,
        hardness: 0.25,
        stages: 1,
        tight_hz: 25.0,
        bass_hz: 80.0,
        mid_hz: 450.0,
        treble_hz: 2_400.0,
        headroom: 0.8,
        trim_db: -5.0,
    },
];

/// Ours: the reach of the EQ and Presence controls.
pub const EQ_RANGE_DB: f32 = 12.0;
pub const PRESENCE_HZ: f32 = 3_500.0;
pub const PRESENCE_RANGE_DB: f32 = 9.0;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(MODEL, "Model", &MODEL_NAMES, 0),
        ParamInfo::continuous(GAIN, "Gain", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(BASS, "Bass", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(MIDDLE, "Middle", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(TREBLE, "Treble", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(PRESENCE, "Presence", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(VOLUME, "Volume", 0.0, 1.0, 0.5, ""),
        ParamInfo::stepped(OUTPUT, "Output", &["Mono", "Dual"], 0),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(HI_QUALITY, "Hi-Quality", false),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Amp",
            items: vec![Item::Menu(ParamId(MODEL)), knob(GAIN), knob(VOLUME)],
        },
        Section {
            title: "EQ",
            items: vec![knob(BASS), knob(MIDDLE), knob(TREBLE), knob(PRESENCE)],
        },
        Section {
            title: "Output",
            items: vec![
                Item::Menu(ParamId(OUTPUT)),
                knob(MIX),
                Item::Toggle(ParamId(HI_QUALITY)),
            ],
        },
    ]
}

/// The effective tone-stack gains in dB for knob positions in `0..1`: turning one control up
/// takes energy from the others, so more Treble costs some Bass and Middle.
pub fn stack_gains(bass: f32, middle: f32, treble: f32) -> (f32, f32, f32) {
    let (b, m, t) = (bass - 0.5, middle - 0.5, treble - 0.5);
    let range = EQ_RANGE_DB * 2.0;
    (
        (b - 0.3 * t.max(0.0) - 0.15 * m.max(0.0)) * range,
        (m - 0.3 * t.max(0.0) - 0.15 * b.max(0.0)) * range,
        (t - 0.15 * b.max(0.0) - 0.15 * m.max(0.0)) * range,
    )
}

/// One tube stage: an offset soft clip, moving towards a hard clip with `hardness`.
#[inline]
fn tube(v: f32, asym: f32, hardness: f32) -> f32 {
    let soft = (v + asym).tanh() - asym.tanh();
    let hard = (v + asym).clamp(-1.0, 1.0) - asym.clamp(-1.0, 1.0);
    soft + (hard - soft) * hardness
}

/// The power stage: clean up to `headroom`, then squashing into it.
#[inline]
fn power_stage(x: f32, headroom: f32) -> f32 {
    headroom * (x / headroom).tanh()
}

#[derive(Clone)]
struct Chan {
    os: Oversampler,
    voice: Voice,
}

/// Everything that runs at the oversampled rate.
#[derive(Clone, Default)]
struct Voice {
    tight: Biquad,
    bass: Biquad,
    mid: Biquad,
    treble: Biquad,
    presence: Biquad,
    dc: DcBlock,
}

impl Chan {
    fn new(max_block: usize) -> Self {
        Self {
            os: Oversampler::new(HI_QUALITY_FACTOR, max_block),
            voice: Voice::default(),
        }
    }
}

impl Voice {
    #[inline]
    fn tick(&mut self, x: f32, m: &Model, gain: f32, volume: f32) -> f32 {
        let mut y = self.tight.process(x) * gain;
        y = tube(y, m.asym, m.hardness);
        if m.stages > 1 {
            y = tube(y * 3.0, m.asym, m.hardness);
        }
        y = self.bass.process(y);
        y = self.mid.process(y);
        y = self.treble.process(y);
        y = power_stage(y * volume, m.headroom);
        y = self.presence.process(y);
        self.dc.process(y)
    }
}

pub struct Amp {
    v: [f64; PARAMS],
    sr: f32,
    max_block: usize,
    chans: [Chan; 2],
    dry_line: DryLine,
    dry: [Vec<f32>; 2],
    latency: u32,
    ramps: [Ramp; 4],
    tuned_for: (i32, f32),
    fresh: bool,
}

impl Default for Amp {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            max_block: 512,
            chans: [Chan::new(512), Chan::new(512)],
            dry_line: DryLine::new(0),
            dry: [Vec::new(), Vec::new()],
            latency: 0,
            ramps: [Ramp::new(0.5); 4],
            tuned_for: (-1, 0.0),
            fresh: true,
        }
    }
}

impl Amp {
    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn rate(&self) -> f32 {
        if self.on(HI_QUALITY) {
            self.sr * HI_QUALITY_FACTOR.multiplier() as f32
        } else {
            self.sr
        }
    }

    fn model(&self) -> &'static Model {
        &MODELS[(self.v[MODEL as usize].round() as usize).min(MODELS.len() - 1)]
    }

    fn retune(&mut self) {
        let m = self.model();
        let rate = self.rate();
        let (bass, middle, treble) = stack_gains(
            self.ramps[0].value,
            self.ramps[1].value,
            self.ramps[2].value,
        );
        let presence = (self.ramps[3].value - 0.5) * 2.0 * PRESENCE_RANGE_DB;
        for chan in &mut self.chans {
            let v = &mut chan.voice;
            v.dc.set(10.0, rate);
            v.tight
                .set(BiquadKind::HighPass, m.tight_hz, 0.707, 0.0, rate);
            v.bass
                .set(BiquadKind::LowShelf, m.bass_hz, 0.707, bass, rate);
            v.mid.set(BiquadKind::Peak, m.mid_hz, 0.8, middle, rate);
            v.treble
                .set(BiquadKind::HighShelf, m.treble_hz, 0.707, treble, rate);
            v.presence
                .set(BiquadKind::HighShelf, PRESENCE_HZ, 0.707, presence, rate);
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        for (ramp, id) in self.ramps.iter_mut().zip([BASS, MIDDLE, TREBLE, PRESENCE]) {
            ramp.target = self.v[id as usize] as f32;
        }
        let tuned_for = (self.v[MODEL as usize].round() as i32, self.rate());
        let mut stale = tuned_for != self.tuned_for;
        self.tuned_for = tuned_for;
        if self.fresh {
            self.fresh = false;
            for ramp in &mut self.ramps {
                ramp.snap(ramp.target);
            }
            stale = true;
        }
        let m = *self.model();
        let gain = db_to_gain(m.gain_lo + (m.gain_hi - m.gain_lo) * self.v[GAIN as usize] as f32);
        // Volume at 50% is unity through the power stage, and the top adds 12 dB.
        let volume = db_to_gain(((self.v[VOLUME as usize] as f32) - 0.5) * 24.0);
        let trim = db_to_gain(m.trim_db);
        let mix = self.v[MIX as usize] as f32;
        let dual = self.v[OUTPUT as usize] >= 0.5;
        let hq = self.on(HI_QUALITY);

        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES).min(self.max_block);
            let mut moved = stale;
            stale = false;
            for ramp in &mut self.ramps {
                moved |= ramp.tick(n, 0.02, self.sr);
            }
            if moved {
                self.retune();
            }
            let (l, r) = (&mut left[pos..pos + n], &mut right[pos..pos + n]);
            self.dry[0][..n].copy_from_slice(l);
            self.dry[1][..n].copy_from_slice(r);
            if !dual {
                for (a, b) in l.iter_mut().zip(r.iter()) {
                    *a = 0.5 * (*a + *b);
                }
            }
            let [cl, cr] = &mut self.chans;
            for (chan, block) in
                [(cl, &mut *l), (cr, &mut *r)]
                    .into_iter()
                    .take(if dual { 2 } else { 1 })
            {
                let voice = &mut chan.voice;
                if hq {
                    chan.os.process_in_place(block, |hi| {
                        for s in hi.iter_mut() {
                            *s = voice.tick(*s, &m, gain, volume);
                        }
                    });
                } else {
                    for s in block.iter_mut() {
                        *s = voice.tick(*s, &m, gain, volume);
                    }
                }
            }
            if !dual {
                r.copy_from_slice(l);
            }
            let [dry_l, dry_r] = &mut self.dry;
            let line = &mut self.dry_line;
            for (y, x) in l.iter_mut().zip(&dry_l[..n]) {
                let d = if hq { line.left(*x) } else { *x };
                *y = d * (1.0 - mix) + *y * trim * mix;
            }
            for (y, x) in r.iter_mut().zip(&dry_r[..n]) {
                let d = if hq { line.right(*x) } else { *x };
                *y = d * (1.0 - mix) + *y * trim * mix;
            }
            pos += n;
        }
    }
}

impl Processor for Amp {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.chans = [Chan::new(self.max_block), Chan::new(self.max_block)];
        for chan in &mut self.chans {
            chan.voice.dc.set(10.0, self.sr);
        }
        self.latency = self.chans[0].os.latency();
        self.dry_line = DryLine::new(self.latency as usize);
        self.dry = [vec![0.0; self.max_block], vec![0.0; self.max_block]];
        self.tuned_for = (-1, 0.0);
        self.fresh = true;
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
        for chan in &mut self.chans {
            chan.os.reset();
            let v = &mut chan.voice;
            for f in [
                &mut v.tight,
                &mut v.bass,
                &mut v.mid,
                &mut v.treble,
                &mut v.presence,
            ] {
                f.reset();
            }
            v.dc.reset();
        }
        self.dry_line.clear();
        self.fresh = true;
    }

    fn latency(&self) -> u32 {
        if self.on(HI_QUALITY) {
            self.latency
        } else {
            0
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.amp",
    name: "Amp",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Amp>::default(),
};
