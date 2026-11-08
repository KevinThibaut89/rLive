//! Multiband Dynamics: compression and expansion, up and down, in up to three bands.

use super::common::{
    knee_db, sidechain_items, sidechain_params, sidechain_settings, sidechain_slice, time_coeff,
    Lr4, MAX_UPWARD_DB, RMS_SECONDS, SC_LEN_BASIC, SC_ON,
};
use crate::dsp::follower::{DetectMode, EnvelopeFollower};
use crate::dsp::sidechain::SidechainDetector;
use crate::dsp::util::{db_to_gain, gain_to_db};
use crate::dsp::BiquadKind;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, AudioInput, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, Ports,
    ProcessContext, Processor, Section, Status, ViewId,
};

pub const HIGH_ON: u32 = 0;
pub const LOW_ON: u32 = 1;
pub const HIGH_FREQ: u32 = 2;
pub const LOW_FREQ: u32 = 3;
pub const OUTPUT: u32 = 4;
pub const TIME: u32 = 5;
pub const AMOUNT: u32 = 6;
pub const SOFT_KNEE: u32 = 7;
pub const DETECTION: u32 = 8;
/// [`DETECTION`] values.
pub const DETECT_PEAK: f64 = 0.0;
pub const DETECT_RMS: f64 = 1.0;
/// Where the bands' own parameters start: see [`band_param`].
pub const FIRST_BAND: u32 = 9;
pub const BAND_STRIDE: u32 = 10;

pub const B_INPUT: u32 = 0;
pub const B_OUTPUT: u32 = 1;
pub const B_ABOVE_THRESHOLD: u32 = 2;
pub const B_ABOVE_RATIO: u32 = 3;
pub const B_BELOW_THRESHOLD: u32 = 4;
pub const B_BELOW_RATIO: u32 = 5;
pub const B_ATTACK: u32 = 6;
pub const B_RELEASE: u32 = 7;
pub const B_ON: u32 = 8;
pub const B_SOLO: u32 = 9;

pub const BANDS: usize = 3;
/// The bands, low to high, as used by [`band_param`].
pub const LOW: usize = 0;
pub const MID: usize = 1;
pub const HIGH: usize = 2;
pub const BAND_NAMES: [&str; BANDS] = ["Low", "Mid", "High"];

/// Where the sidechain parameters start, after the last band.
pub const SIDECHAIN: u32 = FIRST_BAND + BANDS as u32 * BAND_STRIDE;
const PARAM_COUNT: usize = (SIDECHAIN + SC_LEN_BASIC) as usize;

/// The width of Soft Knee, in dB.
pub const KNEE_DB: f32 = 6.0;

/// How long Peak detection holds a crest, in seconds: long enough to bridge a low band's cycles.
pub const PEAK_RELEASE_SECONDS: f32 = 0.02;

/// Each band's input and output level, linear, for the `multiband_display` panel.
pub const LEVELS: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 2 * BANDS;

/// The id of a band's parameter: `band` is [`LOW`], [`MID`] or [`HIGH`], `offset` a `B_` value.
pub const fn band_param(band: usize, offset: u32) -> u32 {
    FIRST_BAND + band as u32 * BAND_STRIDE + offset
}

fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::toggle(HIGH_ON, "High", true),
        ParamInfo::toggle(LOW_ON, "Low", true),
        ParamInfo::skewed(HIGH_FREQ, "High Freq", 20.0, 20_000.0, 2_500.0, "Hz", 3.0),
        ParamInfo::skewed(LOW_FREQ, "Low Freq", 20.0, 20_000.0, 120.0, "Hz", 3.0),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::skewed(TIME, "Time", 0.1, 10.0, 1.0, "x", 3.0),
        ParamInfo::continuous(AMOUNT, "Amount", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(SOFT_KNEE, "Soft Knee", false),
        ParamInfo::stepped(DETECTION, "Detection", &["Peak", "RMS"], 1),
    ];
    for (band, name) in BAND_NAMES.iter().enumerate() {
        let id = |offset: u32| band_param(band, offset);
        let n = |what: &str| format!("{name} {what}");
        p.extend([
            ParamInfo::continuous(id(B_INPUT), &n("Input"), -24.0, 24.0, 0.0, "dB"),
            ParamInfo::continuous(id(B_OUTPUT), &n("Output"), -24.0, 24.0, 0.0, "dB"),
            ParamInfo::continuous(
                id(B_ABOVE_THRESHOLD),
                &n("Above Threshold"),
                -80.0,
                0.0,
                -20.0,
                "dB",
            ),
            ParamInfo::skewed(
                id(B_ABOVE_RATIO),
                &n("Above Ratio"),
                0.1,
                100.0,
                1.0,
                ":1",
                3.0,
            ),
            ParamInfo::continuous(
                id(B_BELOW_THRESHOLD),
                &n("Below Threshold"),
                -80.0,
                0.0,
                -40.0,
                "dB",
            ),
            ParamInfo::skewed(
                id(B_BELOW_RATIO),
                &n("Below Ratio"),
                0.1,
                100.0,
                1.0,
                ":1",
                3.0,
            ),
            ParamInfo::skewed(id(B_ATTACK), &n("Attack"), 0.1, 1_000.0, 10.0, "ms", 3.0),
            ParamInfo::skewed(id(B_RELEASE), &n("Release"), 1.0, 3_000.0, 100.0, "ms", 3.0),
            ParamInfo::toggle(id(B_ON), &n("On"), true),
            ParamInfo::toggle(id(B_SOLO), &n("Solo"), false),
        ]);
    }
    p.extend(sidechain_params(SIDECHAIN, false));
    p
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    let mut sections = vec![
        Section {
            title: "Dynamics",
            items: vec![Item::Panel("multiband_display")],
        },
        Section {
            title: "Bands",
            items: vec![
                toggle(HIGH_ON),
                knob(HIGH_FREQ),
                toggle(LOW_ON),
                knob(LOW_FREQ),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                knob(OUTPUT),
                knob(TIME),
                knob(AMOUNT),
                toggle(SOFT_KNEE),
                Item::Menu(ParamId(DETECTION)),
            ],
        },
    ];
    for (band, name) in BAND_NAMES.iter().enumerate().rev() {
        let id = |offset: u32| band_param(band, offset);
        sections.push(Section {
            title: name,
            items: vec![
                Item::Row(vec![toggle(id(B_ON)), toggle(id(B_SOLO))]),
                knob(id(B_INPUT)),
                knob(id(B_OUTPUT)),
                knob(id(B_ATTACK)),
                knob(id(B_RELEASE)),
                knob(id(B_ABOVE_THRESHOLD)),
                knob(id(B_ABOVE_RATIO)),
                knob(id(B_BELOW_THRESHOLD)),
                knob(id(B_BELOW_RATIO)),
            ],
        });
    }
    sections.push(Section {
        title: "Sidechain",
        items: sidechain_items(SIDECHAIN, false),
    });
    sections
}

/// Splits one channel into three Linkwitz-Riley bands whose sum has a flat magnitude.
#[derive(Clone, Copy, Default)]
struct Crossover {
    low_lp: Lr4,
    low_hp: Lr4,
    high_lp: Lr4,
    high_hp: Lr4,
    /// The low band's copy of the high split, so its phase matches the other two bands.
    align_lp: Lr4,
    align_hp: Lr4,
}

impl Crossover {
    fn set(&mut self, low: f32, high: f32, sr: f32) {
        self.low_lp.set(BiquadKind::LowPass, low, sr);
        self.low_hp.set(BiquadKind::HighPass, low, sr);
        self.high_lp.set(BiquadKind::LowPass, high, sr);
        self.high_hp.set(BiquadKind::HighPass, high, sr);
        self.align_lp.set(BiquadKind::LowPass, high, sr);
        self.align_hp.set(BiquadKind::HighPass, high, sr);
    }

    #[inline]
    fn split(&mut self, x: f32) -> [f32; BANDS] {
        let low = self.low_lp.process(x);
        let upper = self.low_hp.process(x);
        let mid = self.high_lp.process(upper);
        let high = self.high_hp.process(upper);
        let aligned = self.align_lp.process(low) + self.align_hp.process(low);
        [aligned, mid, high]
    }

    fn reset(&mut self) {
        self.low_lp.reset();
        self.low_hp.reset();
        self.high_lp.reset();
        self.high_hp.reset();
        self.align_lp.reset();
        self.align_hp.reset();
    }
}

/// One band's dynamics state.
#[derive(Clone, Copy, Default)]
struct BandDynamics {
    follower: EnvelopeFollower,
    above: f32,
    below: f32,
}

pub struct MultibandDynamics {
    v: Vec<f64>,
    sr: f32,
    max_block: usize,
    detector: SidechainDetector,
    /// Audio split, left and right.
    crossover: [Crossover; 2],
    /// The key's own split: a band listens to its own frequencies of the key.
    key_crossover: [Crossover; 2],
    bands: [BandDynamics; BANDS],
    dirty: bool,
    levels: [f32; VIEW_LEN],
}

impl Default for MultibandDynamics {
    fn default() -> Self {
        let params = params();
        let mut v = vec![0.0; PARAM_COUNT];
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
            crossover: [Crossover::default(); 2],
            key_crossover: [Crossover::default(); 2],
            bands: [BandDynamics::default(); BANDS],
            dirty: true,
            levels: [0.0; VIEW_LEN],
        }
    }
}

impl MultibandDynamics {
    fn p(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn sync(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let settings = sidechain_settings(&self.v, SIDECHAIN, false, DetectMode::Peak, 0.0, 0.0);
        self.detector.set(&settings);
        let low = self.p(LOW_FREQ);
        let high = self.p(HIGH_FREQ).max(low);
        for c in self
            .crossover
            .iter_mut()
            .chain(self.key_crossover.iter_mut())
        {
            c.set(low, high, self.sr);
        }
        let rms = self.v[DETECTION as usize].round() == DETECT_RMS;
        // RMS averages; Peak follows the wave's crest, so a band's own attack shapes the gain.
        let (mode, attack, release) = if rms {
            (DetectMode::Rms, RMS_SECONDS, RMS_SECONDS)
        } else {
            (DetectMode::Peak, 0.0, PEAK_RELEASE_SECONDS)
        };
        for b in &mut self.bands {
            b.follower.prepare(self.sr as f64);
            b.follower.set_mode(mode);
            b.follower.set_times(attack, release);
        }
    }

    fn render(&mut self, audio: AudioBuffer, external: Option<AudioInput>) {
        self.sync();
        let external = external.filter(|_| self.flag(SIDECHAIN + SC_ON));
        let low_on = self.flag(LOW_ON);
        let high_on = self.flag(HIGH_ON);
        let knee = if self.flag(SOFT_KNEE) { KNEE_DB } else { 0.0 };
        let amount = self.p(AMOUNT);
        let time = self.p(TIME);
        let output = db_to_gain(self.p(OUTPUT));

        // Solo on a band that's switched off has nothing to play, so it mustn't mute the others.
        let any_solo =
            (0..BANDS).any(|b| band_in_use(b, low_on, high_on) && self.flag(band_param(b, B_SOLO)));

        // Per-band settings, read once for the chunk.
        let mut band = [BandSettings::default(); BANDS];
        for (i, s) in band.iter_mut().enumerate() {
            let id = |offset| band_param(i, offset);
            *s = BandSettings {
                input: db_to_gain(self.p(id(B_INPUT))),
                output: db_to_gain(self.p(id(B_OUTPUT))),
                above_threshold: self.p(id(B_ABOVE_THRESHOLD)),
                above_slope: 1.0 - 1.0 / self.p(id(B_ABOVE_RATIO)).max(0.05),
                below_threshold: self.p(id(B_BELOW_THRESHOLD)),
                below_slope: self.p(id(B_BELOW_RATIO)) - 1.0,
                attack: time_coeff(self.p(id(B_ATTACK)) * time, self.sr),
                release: time_coeff(self.p(id(B_RELEASE)) * time, self.sr),
                active: self.flag(id(B_ON)),
                audible: !any_solo || self.flag(id(B_SOLO)),
            };
        }

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
                let mut key_bands =
                    fold_bands(self.key_crossover[0].split(key.left[i]), low_on, high_on);
                let key_right =
                    fold_bands(self.key_crossover[1].split(key.right[i]), low_on, high_on);
                for (k, r) in key_bands.iter_mut().zip(key_right) {
                    *k = k.abs().max(r.abs());
                }
                let lb = fold_bands(self.crossover[0].split(left[i]), low_on, high_on);
                let rb = fold_bands(self.crossover[1].split(right[i]), low_on, high_on);

                let (mut l_out, mut r_out) = (0.0f32, 0.0f32);
                for b in 0..BANDS {
                    let s = band[b];
                    if !band_in_use(b, low_on, high_on) {
                        continue;
                    }
                    let dyn_state = &mut self.bands[b];
                    let level = dyn_state.follower.process(key_bands[b] * s.input);
                    let db = gain_to_db(level);
                    let gain_db = amount
                        * band_gain_db(&mut dyn_state.above, &mut dyn_state.below, db, knee, &s);
                    let g = if s.active {
                        s.input * s.output * db_to_gain(gain_db)
                    } else {
                        1.0
                    };
                    let (bl, br) = (lb[b] * g, rb[b] * g);
                    let input_peak = lb[b].abs().max(rb[b].abs());
                    let output_peak = bl.abs().max(br.abs());
                    self.levels[b] = self.levels[b].max(input_peak);
                    self.levels[BANDS + b] = self.levels[BANDS + b].max(output_peak);
                    if s.audible {
                        l_out += bl;
                        r_out += br;
                    }
                }
                left[i] = l_out * output;
                right[i] = r_out * output;
            }
            self.detector.listen(&mut AudioBuffer { left, right });
            pos += n;
        }
    }
}

#[derive(Clone, Copy, Default)]
struct BandSettings {
    input: f32,
    output: f32,
    above_threshold: f32,
    above_slope: f32,
    below_threshold: f32,
    below_slope: f32,
    attack: f32,
    release: f32,
    active: bool,
    audible: bool,
}

/// A band with a switched-off split neighbour takes its neighbour's signal.
#[inline]
fn fold_bands(mut b: [f32; BANDS], low_on: bool, high_on: bool) -> [f32; BANDS] {
    if !low_on {
        b[MID] += b[LOW];
        b[LOW] = 0.0;
    }
    if !high_on {
        b[MID] += b[HIGH];
        b[HIGH] = 0.0;
    }
    b
}

/// Whether a band exists at all under the current Low and High switches.
#[inline]
fn band_in_use(band: usize, low_on: bool, high_on: bool) -> bool {
    match band {
        LOW => low_on,
        HIGH => high_on,
        _ => true,
    }
}

/// The band's dynamics in dB of gain: the sum of its above and below curves, each with its own
/// attack and release, and limited on the way up by [`MAX_UPWARD_DB`].
#[inline]
fn band_gain_db(above: &mut f32, below: &mut f32, db: f32, knee: f32, s: &BandSettings) -> f32 {
    let target_above = (-knee_db(db - s.above_threshold, knee, s.above_slope)).min(MAX_UPWARD_DB);
    let target_below = (-knee_db(s.below_threshold - db, knee, s.below_slope)).min(MAX_UPWARD_DB);
    let smooth = |state: &mut f32, target: f32| {
        let c = if target.abs() > state.abs() {
            s.attack
        } else {
            s.release
        };
        *state = target + (*state - target) * c;
        *state
    };
    smooth(above, target_above) + smooth(below, target_below)
}

impl Processor for MultibandDynamics {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.detector.prepare(sample_rate, self.max_block);
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let sidechain = ctx.sidechain;
        let mut pos = 0;
        self.levels = [0.0; VIEW_LEN];
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
            ctx.view.publish(LEVELS, &self.levels);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if !(FIRST_BAND..SIDECHAIN).contains(&id) {
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        self.detector.reset();
        for c in self
            .crossover
            .iter_mut()
            .chain(self.key_crossover.iter_mut())
        {
            c.reset();
        }
        for b in &mut self.bands {
            b.above = 0.0;
            b.below = 0.0;
            b.follower.reset();
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.multiband_dynamics",
    name: "Multiband Dynamics",
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
    create: |_| Box::<MultibandDynamics>::default(),
};
