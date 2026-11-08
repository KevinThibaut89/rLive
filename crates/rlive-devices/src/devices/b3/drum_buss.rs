//! Drum Buss: fixed compression, drive and distortion, mid-high shaping and low-end enhancement.

use super::common::{time_coeff, Lr4};
use crate::dsp::follower::{DetectMode, EnvelopeFollower};
use crate::dsp::util::{db_to_gain, gain_to_db, midi_to_hz};
use crate::dsp::{Biquad, BiquadKind, Smoother};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status, ViewId,
};

pub const TRIM: u32 = 0;
pub const COMP: u32 = 1;
pub const DISTORTION: u32 = 2;
pub const DRIVE: u32 = 3;
pub const CRUNCH: u32 = 4;
pub const DAMP: u32 = 5;
pub const TRANSIENTS: u32 = 6;
pub const BOOM: u32 = 7;
pub const FREQ: u32 = 8;
pub const FORCE_TO_NOTE: u32 = 9;
pub const DECAY: u32 = 10;
pub const AUDITION: u32 = 11;
pub const MIX: u32 = 12;
pub const OUTPUT: u32 = 13;

const PARAM_COUNT: usize = 14;

/// [`DISTORTION`] values.
pub const DIST_SOFT: f64 = 0.0;
pub const DIST_MEDIUM: f64 = 1.0;
pub const DIST_HARD: f64 = 2.0;

// The fixed compressor: fast attack, medium release, moderate ratio and ample makeup.
pub const COMP_THRESHOLD_DB: f32 = -18.0;
pub const COMP_RATIO: f32 = 3.0;
pub const COMP_ATTACK_MS: f32 = 1.0;
pub const COMP_RELEASE_MS: f32 = 100.0;
pub const COMP_MAKEUP_DB: f32 = 6.0;

/// The largest Boom boost at the filter's frequency, as a linear amount added to the signal.
pub const BOOM_GAIN: f32 = 3.0;
/// Where the incoming low end is split off for Decay, as a multiple of the Boom frequency.
pub const LOW_SPLIT_RATIO: f32 = 2.0;
/// The frequency above which Transients acts.
pub const TRANSIENT_SPLIT_HZ: f32 = 100.0;
/// The frequency above which Crunch acts.
pub const CRUNCH_SPLIT_HZ: f32 = 1_000.0;
/// Where Damp stops acting.
const DAMP_OFF_HZ: f32 = 19_900.0;
/// How far Hard drives its clipper, so it clips from a level the softer curves only round off.
const HARD_PRE_GAIN: f32 = 1.5;
/// The bass boost of Hard distortion, in dB, at [`HARD_BOOST_HZ`].
pub const HARD_BOOST_DB: f32 = 6.0;
pub const HARD_BOOST_HZ: f32 = 120.0;

/// The Boom signal, left and right, for the Bass Meter.
pub const BASS_METER: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 2;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(TRIM, "Trim", -24.0, 0.0, 0.0, "dB"),
        ParamInfo::toggle(COMP, "Comp", false),
        ParamInfo::stepped(DISTORTION, "Type", &["Soft", "Medium", "Hard"], 0),
        ParamInfo::continuous(DRIVE, "Drive", 0.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(CRUNCH, "Crunch", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(DAMP, "Damp", 500.0, 20_000.0, 20_000.0, "Hz", 3.0),
        ParamInfo::continuous(TRANSIENTS, "Transients", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(BOOM, "Boom", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(FREQ, "Freq", 30.0, 90.0, 60.0, "Hz"),
        ParamInfo::toggle(FORCE_TO_NOTE, "Force To Note", false),
        ParamInfo::continuous(DECAY, "Decay", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(AUDITION, "Boom Audition", false),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(OUTPUT, "Output Gain", -24.0, 12.0, 0.0, "dB"),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Drive",
            items: vec![
                knob(TRIM),
                toggle(COMP),
                Item::Menu(ParamId(DISTORTION)),
                knob(DRIVE),
            ],
        },
        Section {
            title: "Shaping",
            items: vec![knob(CRUNCH), knob(DAMP), knob(TRANSIENTS)],
        },
        Section {
            title: "Boom",
            items: vec![
                knob(BOOM),
                knob(FREQ),
                toggle(FORCE_TO_NOTE),
                knob(DECAY),
                toggle(AUDITION),
                Item::Meter(BASS_METER),
            ],
        },
        Section {
            title: "Output",
            items: vec![knob(MIX), knob(OUTPUT)],
        },
    ]
}

/// The distortion curve of a [`DISTORTION`] type. Each is harder than the last.
#[inline]
pub fn distort(x: f32, kind: f64) -> f32 {
    if kind == DIST_SOFT {
        x.tanh()
    } else if kind == DIST_MEDIUM {
        x / (1.0 + x * x).sqrt()
    } else {
        (x * HARD_PRE_GAIN).clamp(-1.0, 1.0)
    }
}

/// The Boom filter's frequency, with Force To Note snapping it to the nearest MIDI note.
pub fn boom_frequency(freq: f32, force_to_note: bool) -> f32 {
    if force_to_note {
        midi_to_hz((69.0 + 12.0 * (freq / 440.0).log2()).round())
    } else {
        freq
    }
}

#[derive(Clone, Copy, Default)]
struct Channel {
    hard_boost: Biquad,
    crunch_split: Biquad,
    damp: Biquad,
    transient_split: Biquad,
    low_lp: Lr4,
    low_hp: Lr4,
    boom: Biquad,
}

impl Channel {
    fn reset(&mut self) {
        self.hard_boost.reset();
        self.crunch_split.reset();
        self.damp.reset();
        self.transient_split.reset();
        self.low_lp.reset();
        self.low_hp.reset();
        self.boom.reset();
    }
}

pub struct DrumBuss {
    v: Vec<f64>,
    sr: f32,
    channels: [Channel; 2],
    comp_env_db: f32,
    transient_fast: EnvelopeFollower,
    transient_slow: EnvelopeFollower,
    /// The low end's envelope with no decay shaping, and with it.
    decay_natural: EnvelopeFollower,
    decay_shaped: EnvelopeFollower,
    /// The decay shaper's gain, smoothed so a steady bass tone doesn't ripple it.
    tail: f32,
    trim: Smoother,
    drive: Smoother,
    wet: Smoother,
    output: Smoother,
    dirty: bool,
    boom_peak: [f32; 2],
}

impl Default for DrumBuss {
    fn default() -> Self {
        let params = params();
        let mut v = vec![0.0; PARAM_COUNT];
        for p in &params {
            v[p.id as usize] = p.default;
        }
        let mut d = Self {
            v,
            sr: 48_000.0,
            channels: [Channel::default(); 2],
            comp_env_db: 0.0,
            transient_fast: EnvelopeFollower::new(),
            transient_slow: EnvelopeFollower::new(),
            decay_natural: EnvelopeFollower::new(),
            decay_shaped: EnvelopeFollower::new(),
            tail: 1.0,
            trim: Smoother::new(1.0),
            drive: Smoother::new(1.0),
            wet: Smoother::new(1.0),
            output: Smoother::new(1.0),
            dirty: true,
            boom_peak: [0.0; 2],
        };
        d.prepare_smoothers();
        d.sync();
        d
    }
}

impl DrumBuss {
    fn p(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn prepare_smoothers(&mut self) {
        for s in [
            &mut self.trim,
            &mut self.drive,
            &mut self.wet,
            &mut self.output,
        ] {
            s.set_time(0.005, self.sr);
        }
        for f in [
            &mut self.transient_fast,
            &mut self.transient_slow,
            &mut self.decay_natural,
            &mut self.decay_shaped,
        ] {
            f.prepare(self.sr as f64);
            f.set_mode(DetectMode::Peak);
        }
        self.transient_fast.set_times(0.0005, 0.03);
        self.transient_slow.set_times(0.02, 0.2);
    }

    fn sync(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let sr = self.sr;
        let boom_f = boom_frequency(self.p(FREQ), self.v[FORCE_TO_NOTE as usize] >= 0.5);
        let decay = self.p(DECAY).clamp(0.0, 1.0);
        let damp = self.p(DAMP);
        for c in &mut self.channels {
            c.hard_boost
                .set(BiquadKind::LowShelf, HARD_BOOST_HZ, 0.71, HARD_BOOST_DB, sr);
            c.crunch_split
                .set(BiquadKind::LowPass, CRUNCH_SPLIT_HZ, 0.71, 0.0, sr);
            c.damp
                .set(BiquadKind::LowPass, damp.min(sr * 0.45), 0.71, 0.0, sr);
            c.transient_split
                .set(BiquadKind::LowPass, TRANSIENT_SPLIT_HZ, 0.71, 0.0, sr);
            c.low_lp
                .set(BiquadKind::LowPass, boom_f * LOW_SPLIT_RATIO, sr);
            c.low_hp
                .set(BiquadKind::HighPass, boom_f * LOW_SPLIT_RATIO, sr);
            c.boom
                .set(BiquadKind::BandPass, boom_f, 2.0 + 10.0 * decay, 0.0, sr);
        }
        // Decay 1 leaves the tail alone; lower settings pull it down faster.
        let tail = 0.01 + (0.4 - 0.01) * decay * decay;
        self.decay_natural.set_times(0.001, 0.4);
        self.decay_shaped.set_times(0.001, tail);
    }

    fn render(&mut self, audio: AudioBuffer) {
        self.sync();
        self.trim.target = db_to_gain(self.p(TRIM));
        self.drive.target = db_to_gain(self.p(DRIVE));
        self.wet.target = self.p(MIX).clamp(0.0, 1.0);
        self.output.target = db_to_gain(self.p(OUTPUT));
        let comp_on = self.v[COMP as usize] >= 0.5;
        let kind = self.v[DISTORTION as usize].round();
        let crunch = self.p(CRUNCH);
        let damp_on = self.p(DAMP) < DAMP_OFF_HZ;
        let transients = self.p(TRANSIENTS);
        let boom = self.p(BOOM) * BOOM_GAIN;
        let audition = self.v[AUDITION as usize] >= 0.5;
        let attack = time_coeff(COMP_ATTACK_MS, self.sr);
        let release = time_coeff(COMP_RELEASE_MS, self.sr);
        let slope = 1.0 - 1.0 / COMP_RATIO;
        let makeup = COMP_MAKEUP_DB;
        let tail_coeff = time_coeff(8.0, self.sr);

        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for i in 0..left.len() {
            let (dry_l, dry_r) = (left[i], right[i]);
            let trim = self.trim.next();
            let (mut l, mut r) = (dry_l * trim, dry_r * trim);

            if comp_on {
                let level = gain_to_db(l.abs().max(r.abs()));
                let target = (slope * (level - COMP_THRESHOLD_DB)).max(0.0);
                let c = if target > self.comp_env_db {
                    attack
                } else {
                    release
                };
                self.comp_env_db = target + (self.comp_env_db - target) * c;
                let g = db_to_gain(makeup - self.comp_env_db);
                l *= g;
                r *= g;
            }

            let drive = self.drive.next();
            l *= drive;
            r *= drive;
            if kind == DIST_HARD {
                l = self.channels[0].hard_boost.process(l);
                r = self.channels[1].hard_boost.process(r);
            }
            l = distort(l, kind);
            r = distort(r, kind);

            // Mid-high shaping.
            let shape = |c: &mut Channel, x: f32| {
                let low = c.crunch_split.process(x);
                let high = x - low;
                let k = std::f32::consts::FRAC_PI_2 * (1.0 + 3.0 * crunch);
                let sine = (high.clamp(-1.0, 1.0) * k).sin() / k;
                low + high + crunch * (sine - high)
            };
            l = shape(&mut self.channels[0], l);
            r = shape(&mut self.channels[1], r);
            if damp_on {
                l = self.channels[0].damp.process(l);
                r = self.channels[1].damp.process(r);
            }

            // Transients, above the split so they don't push the low end around.
            let low_l = self.channels[0].transient_split.process(l);
            let low_r = self.channels[1].transient_split.process(r);
            let (high_l, high_r) = (l - low_l, r - low_r);
            let key = high_l.abs().max(high_r.abs());
            let fast = self.transient_fast.process(key);
            let slow = self.transient_slow.process(key);
            let shaped = transient_gain(transients, fast, slow);
            l = low_l + high_l * shaped;
            r = low_r + high_r * shaped;

            // Low end: split with matched phase, shape the decay, resonate.
            let low_l = self.channels[0].low_lp.process(l);
            let low_r = self.channels[1].low_lp.process(r);
            let high_l = self.channels[0].low_hp.process(l);
            let high_r = self.channels[1].low_hp.process(r);
            let key = low_l.abs().max(low_r.abs());
            let natural = self.decay_natural.process(key);
            let shaped = self.decay_shaped.process(key);
            let tail_target = if natural > 1e-9 {
                (shaped / natural).min(1.0)
            } else {
                1.0
            };
            self.tail = tail_target + (self.tail - tail_target) * tail_coeff;
            let tail = self.tail;
            // Feed the resonator the split signal, so the two paths stay in phase and add.
            let boom_l = self.channels[0].boom.process(high_l + low_l) * boom * tail;
            let boom_r = self.channels[1].boom.process(high_r + low_r) * boom * tail;
            self.boom_peak[0] = self.boom_peak[0].max(boom_l.abs());
            self.boom_peak[1] = self.boom_peak[1].max(boom_r.abs());
            l = high_l + low_l * tail + boom_l;
            r = high_r + low_r * tail + boom_r;

            if audition {
                l = boom_l;
                r = boom_r;
            }

            let wet = self.wet.next();
            let out = self.output.next();
            left[i] = dry_l * (1.0 - wet) + l * wet * out;
            right[i] = dry_r * (1.0 - wet) + r * wet * out;
        }
    }
}

/// How much Transients moves the band above its split, as a linear gain. Positive values add
/// attack and sustain; negative values also add attack but pull down sustain.
#[inline]
fn transient_gain(amount: f32, fast: f32, slow: f32) -> f32 {
    if amount == 0.0 {
        return 1.0;
    }
    let delta = gain_to_db(fast) - gain_to_db(slow);
    let db = if delta >= 0.0 {
        amount.abs() * delta * 0.7
    } else {
        amount * -delta * 0.8
    };
    db_to_gain(db.clamp(-18.0, 12.0))
}

impl Processor for DrumBuss {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate as f32;
        self.prepare_smoothers();
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.boom_peak = [0.0; 2];
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            ctx.view.publish(BASS_METER, &self.boom_peak);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if matches!(id, FREQ | FORCE_TO_NOTE | DECAY | DAMP) {
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        for c in &mut self.channels {
            c.reset();
        }
        self.comp_env_db = 0.0;
        self.tail = 1.0;
        for f in [
            &mut self.transient_fast,
            &mut self.transient_slow,
            &mut self.decay_natural,
            &mut self.decay_shaped,
        ] {
            f.reset();
        }
        self.trim.reset();
        self.drive.reset();
        self.wet.reset();
        self.output.reset();
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.drum_buss",
    name: "Drum Buss",
    role: DeviceRole::AudioEffect,
    category: "Dynamics",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<DrumBuss>::default(),
};
