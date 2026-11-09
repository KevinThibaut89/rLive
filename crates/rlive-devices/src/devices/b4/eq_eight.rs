//! EQ Eight: eight filter bands per channel, in Stereo, L/R or M/S.
//!
//! Each band can be one of eight responses. L/R and M/S give the two channels their own curve:
//! curve A is left or mid, curve B is right or side; Stereo runs curve A on both.
//!
//! The manual gives no numbers for the controls' ranges or for Adaptive Q and Scale. The ranges
//! here are chosen to be usual for this kind of EQ, and Adaptive Q is [`adaptive_q`].

use super::common::{Ramp, CONTROL_FRAMES};
use crate::dsp::oversample::{OversampleFactor, Oversampler};
use crate::dsp::spectrum::SpectrumFeed;
use crate::dsp::{Biquad, BiquadKind, Smoother};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamFlags, ParamId, ParamInfo,
    ProcessContext, Processor, Section, Status, ViewId, SCOPE_VALUES,
};
use std::f32::consts::FRAC_1_SQRT_2;

pub const BANDS: usize = 8;
pub const CURVES: usize = 2;

// Offsets into a band's parameters.
pub const B_ON: u32 = 0;
pub const B_TYPE: u32 = 1;
pub const B_FREQ: u32 = 2;
pub const B_GAIN: u32 = 3;
pub const B_Q: u32 = 4;
const BAND_PARAMS: u32 = 5;

/// A band's parameter id, for curve 0 (A) or 1 (B).
pub const fn band_param(curve: usize, band: usize, field: u32) -> u32 {
    (curve * BANDS) as u32 * BAND_PARAMS + band as u32 * BAND_PARAMS + field
}

pub const MODE: u32 = 80;
pub const EDIT: u32 = 81;
pub const ADAPTIVE_Q: u32 = 82;
pub const ANALYZE: u32 = 83;
pub const AUDITION: u32 = 84;
/// Which band is being held in Audition, as 1 to 8, or 0 for none. The display sets this.
pub const AUDITION_BAND: u32 = 85;
pub const OVERSAMPLING: u32 = 86;
pub const SCALE: u32 = 87;
pub const GAIN: u32 = 88;

pub const PARAM_COUNT: usize = 89;

pub const SPECTRUM: ViewId = ViewId(0);
pub const SAMPLE_RATE: ViewId = ViewId(SCOPE_VALUES as u32);
pub const VIEW_LEN: usize = SCOPE_VALUES + 1;

pub const MODE_STEREO: usize = 0;
pub const MODE_LR: usize = 1;
pub const MODE_MS: usize = 2;

/// A band's filter shape, in the order of its chooser.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Response {
    LowCut48,
    LowCut12,
    LowShelf,
    Peak,
    Notch,
    HighShelf,
    HighCut12,
    HighCut48,
}

pub const RESPONSE_LABELS: [&str; 8] = [
    "Low Cut 48",
    "Low Cut 12",
    "Low Shelf",
    "Peak",
    "Notch",
    "High Shelf",
    "High Cut 12",
    "High Cut 48",
];

impl Response {
    pub fn from_index(i: f64) -> Self {
        match i.round() as i32 {
            0 => Self::LowCut48,
            1 => Self::LowCut12,
            2 => Self::LowShelf,
            4 => Self::Notch,
            5 => Self::HighShelf,
            6 => Self::HighCut12,
            7 => Self::HighCut48,
            _ => Self::Peak,
        }
    }

    /// Whether Gain and Scale do anything for this shape. Cuts and notches have no gain.
    pub fn has_gain(self) -> bool {
        matches!(self, Self::LowShelf | Self::Peak | Self::HighShelf)
    }
}

/// The band Q after Adaptive Q: it rises with the amount of boost or cut, by half an octave of
/// Q for every 6 dB. The manual says only that it increases.
pub fn adaptive_q(q: f32, gain_db: f32) -> f32 {
    q * 10f32.powf(gain_db.abs() / 40.0)
}

// Q of the four sections of an 8th-order Butterworth filter.
const BUTTERWORTH_8: [f32; 4] = [0.509_796, 0.601_345, 0.899_976, 2.562_915];

/// A band's filter sections.
#[derive(Clone, Copy, Default)]
struct Sections {
    filters: [Biquad; 4],
    count: usize,
}

impl Sections {
    fn design(&mut self, response: Response, freq: f32, q: f32, gain_db: f32, sr: f32) {
        let f = &mut self.filters;
        self.count = 1;
        match response {
            Response::LowCut12 => f[0].set(BiquadKind::HighPass, freq, q, 0.0, sr),
            Response::HighCut12 => f[0].set(BiquadKind::LowPass, freq, q, 0.0, sr),
            Response::LowCut48 | Response::HighCut48 => {
                let kind = if response == Response::LowCut48 {
                    BiquadKind::HighPass
                } else {
                    BiquadKind::LowPass
                };
                for (i, section) in f.iter_mut().enumerate() {
                    // The band's Q sets the resonance, on the section nearest the cutoff.
                    let scale = if i == 3 { q / FRAC_1_SQRT_2 } else { 1.0 };
                    section.set(kind, freq, BUTTERWORTH_8[i] * scale, 0.0, sr);
                }
                self.count = 4;
            }
            Response::LowShelf => f[0].set(BiquadKind::LowShelf, freq, q, gain_db, sr),
            Response::Peak => f[0].set(BiquadKind::Peak, freq, q, gain_db, sr),
            Response::Notch => f[0].set(BiquadKind::Notch, freq, q, 0.0, sr),
            Response::HighShelf => f[0].set(BiquadKind::HighShelf, freq, q, gain_db, sr),
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.filters[..self.count]
            .iter_mut()
            .fold(x, |y, s| s.process(y))
    }

    fn reset(&mut self) {
        self.filters.iter_mut().for_each(Biquad::reset);
    }
}

/// What a band is set to, after Scale and Adaptive Q.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandSetting {
    pub on: bool,
    pub response: Response,
    pub freq: f32,
    pub q: f32,
    pub gain_db: f32,
}

impl BandSetting {
    /// Reads a band of a curve from its parameters.
    pub fn read(get: impl Fn(u32) -> f64, curve: usize, band: usize, adaptive: bool) -> Self {
        let field = |f| get(band_param(curve, band, f));
        let response = Response::from_index(field(B_TYPE));
        let gain_db = if response.has_gain() {
            (field(B_GAIN) * get(SCALE) * 0.01) as f32
        } else {
            0.0
        };
        let mut q = field(B_Q) as f32;
        if adaptive && response.has_gain() {
            q = adaptive_q(q, gain_db);
        }
        Self {
            on: field(B_ON) >= 0.5,
            response,
            freq: field(B_FREQ) as f32,
            q,
            gain_db,
        }
    }

    /// The band's response at `hz`, in decibels. For drawing a curve.
    pub fn magnitude_db(&self, hz: f32, sr: f32) -> f32 {
        if !self.on {
            return 0.0;
        }
        let mut sections = Sections::default();
        sections.design(self.response, self.freq, self.q, self.gain_db, sr);
        sections.filters[..sections.count]
            .iter()
            .map(|f| 20.0 * f.magnitude(hz, sr).max(1e-6).log10())
            .sum()
    }
}

fn params() -> Vec<ParamInfo> {
    let mut p = Vec::with_capacity(PARAM_COUNT);
    let default_freq: [f64; BANDS] = [
        80.0, 200.0, 500.0, 1_000.0, 2_500.0, 5_000.0, 8_000.0, 12_000.0,
    ];
    for (curve, tag) in ["A", "B"].into_iter().enumerate() {
        for (band, freq) in default_freq.into_iter().enumerate() {
            let id = |f| band_param(curve, band, f);
            let n = band + 1;
            let name = |what: &str| format!("{n} {what} {tag}");
            let response = match band {
                0 => 2,
                7 => 5,
                _ => 3,
            };
            p.push(ParamInfo::toggle(id(B_ON), &name("Filter On"), band < 4));
            p.push(ParamInfo::stepped(
                id(B_TYPE),
                &name("Filter Type"),
                &RESPONSE_LABELS,
                response,
            ));
            p.push(ParamInfo::skewed(
                id(B_FREQ),
                &name("Frequency"),
                20.0,
                22_000.0,
                freq,
                "Hz",
                3.0,
            ));
            p.push(ParamInfo::continuous(
                id(B_GAIN),
                &name("Gain"),
                -15.0,
                15.0,
                0.0,
                "dB",
            ));
            p.push(ParamInfo::skewed(
                id(B_Q),
                &name("Resonance"),
                0.1,
                18.0,
                0.71,
                "",
                3.0,
            ));
        }
    }
    p.extend([
        ParamInfo::stepped(MODE, "Mode", &["Stereo", "L/R", "M/S"], MODE_STEREO),
        ParamInfo::stepped(EDIT, "Edit", &["A", "B"], 0),
        ParamInfo::toggle(ADAPTIVE_Q, "Adaptive Q", true),
        ParamInfo::toggle(ANALYZE, "Analyze", true),
        ParamInfo::toggle(AUDITION, "Audition", false),
        ParamInfo::continuous(AUDITION_BAND, "Audition Band", 0.0, 8.0, 0.0, "")
            .with_flags(ParamFlags::HIDDEN),
        ParamInfo::toggle(OVERSAMPLING, "Oversampling", false),
        ParamInfo::continuous(SCALE, "Scale", -200.0, 200.0, 100.0, "%"),
        ParamInfo::continuous(GAIN, "Gain", -12.0, 12.0, 0.0, "dB"),
    ]);
    p
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Display",
            items: vec![Item::Panel("eq_eight_display")],
        },
        Section {
            title: "Global",
            items: vec![
                Item::Menu(ParamId(MODE)),
                Item::Menu(ParamId(EDIT)),
                Item::Toggle(ParamId(ADAPTIVE_Q)),
                Item::Toggle(ParamId(ANALYZE)),
                Item::Toggle(ParamId(AUDITION)),
                Item::Toggle(ParamId(OVERSAMPLING)),
                Item::Knob(ParamId(SCALE)),
                Item::Knob(ParamId(GAIN)),
            ],
        },
    ]
}

/// A band of one channel's chain.
#[derive(Clone, Copy)]
struct BandState {
    sections: Sections,
    freq: Ramp,
    gain: Ramp,
    q: Ramp,
    /// How much of the band is in the signal, for switching it without a click.
    mix: f32,
    mix_target: f32,
    response: Response,
}

impl Default for BandState {
    fn default() -> Self {
        Self {
            sections: Sections::default(),
            freq: Ramp::new(1_000f32.log2()),
            gain: Ramp::new(0.0),
            q: Ramp::new(FRAC_1_SQRT_2.log2()),
            mix: 0.0,
            mix_target: 0.0,
            response: Response::Peak,
        }
    }
}

type Chain = [BandState; BANDS];

pub struct EqEight {
    v: [f64; PARAM_COUNT],
    sr: f32,
    chains: [Chain; 2],
    stale: bool,
    /// Whether the next block is the first since activation or a reset.
    fresh: bool,
    oversample: [Oversampler; 2],
    /// Whether oversampling was on for the last block.
    was_oversampling: bool,
    gain: Smoother,
    spectrum: SpectrumFeed,
}

impl Default for EqEight {
    fn default() -> Self {
        let mut v = [0.0; PARAM_COUNT];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            chains: [[BandState::default(); BANDS]; 2],
            stale: true,
            fresh: true,
            oversample: [
                Oversampler::new(OversampleFactor::X2, CONTROL_FRAMES),
                Oversampler::new(OversampleFactor::X2, CONTROL_FRAMES),
            ],
            was_oversampling: false,
            gain: Smoother::new(1.0),
            spectrum: SpectrumFeed::new(48_000.0, 4096, 1024),
        }
    }
}

impl EqEight {
    fn oversampling(&self) -> bool {
        self.v[OVERSAMPLING as usize] >= 0.5
    }

    fn mode(&self) -> usize {
        (self.v[MODE as usize].round() as usize).min(MODE_MS)
    }

    /// Points the band ramps at the parameters.
    fn update_targets(&mut self) {
        let v = &self.v;
        let get = |id: u32| v[id as usize];
        let mode = (get(MODE).round() as usize).min(MODE_MS);
        let adaptive = get(ADAPTIVE_Q) >= 0.5;
        let audition = (get(AUDITION) >= 0.5)
            .then(|| get(AUDITION_BAND).round() as usize)
            .filter(|b| *b > 0);
        let edit = get(EDIT).round() as usize;
        for (ch, chain) in self.chains.iter_mut().enumerate() {
            let curve = if mode == MODE_STEREO { 0 } else { ch };
            for (band, state) in chain.iter_mut().enumerate() {
                let setting = BandSetting::read(get, curve, band, adaptive);
                let solo = match audition {
                    Some(b) => b - 1 == band && (mode == MODE_STEREO || edit == curve),
                    None => true,
                };
                state.freq.target = setting.freq.log2();
                state.gain.target = setting.gain_db;
                state.q.target = setting.q.max(0.05).log2();
                if state.response != setting.response {
                    state.response = setting.response;
                    self.stale = true;
                }
                state.mix_target = if setting.on && solo { 1.0 } else { 0.0 };
            }
        }
    }

    fn retune(&mut self, rate: f32, frames: usize, force: bool) {
        for chain in &mut self.chains {
            for b in chain.iter_mut() {
                let mut moved = force;
                moved |= b.freq.tick(frames, 0.02, self.sr);
                moved |= b.gain.tick(frames, 0.02, self.sr);
                moved |= b.q.tick(frames, 0.02, self.sr);
                if moved {
                    b.sections.design(
                        b.response,
                        2f32.powf(b.freq.value),
                        2f32.powf(b.q.value),
                        b.gain.value,
                        rate,
                    );
                }
            }
        }
    }

    /// Starts on the parameters' values, instead of easing towards them.
    fn snap(&mut self) {
        self.update_targets();
        for chain in &mut self.chains {
            for b in chain.iter_mut() {
                b.freq.snap(b.freq.target);
                b.gain.snap(b.gain.target);
                b.q.snap(b.q.target);
                b.mix = b.mix_target;
            }
        }
        self.gain.reset();
        self.stale = true;
    }

    fn render(&mut self, audio: AudioBuffer) {
        let oversampling = self.oversampling();
        if oversampling != self.was_oversampling {
            self.was_oversampling = oversampling;
            self.oversample.iter_mut().for_each(Oversampler::reset);
            self.stale = true;
        }
        self.gain.target = db_to_gain(self.v[GAIN as usize] as f32);
        if self.fresh {
            self.fresh = false;
            self.snap();
        }
        self.update_targets();
        let rate = self.sr * if oversampling { 2.0 } else { 1.0 };
        let mode = self.mode();
        let frames = audio.frames();
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        if mode == MODE_MS {
            for i in 0..frames {
                let (l, r) = (left[i], right[i]);
                left[i] = (l + r) * 0.5;
                right[i] = (l - r) * 0.5;
            }
        }
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES);
            let stale = self.stale;
            self.retune(rate, n, stale);
            self.stale = false;
            let [chain_l, chain_r] = &mut self.chains;
            let [os_l, os_r] = &mut self.oversample;
            let oversampled = oversampling;
            let run_channel = |chain: &mut Chain, os: &mut Oversampler, buf: &mut [f32]| {
                if oversampled {
                    os.process_in_place(buf, |hi| run_bands(chain, hi));
                } else {
                    run_bands(chain, buf);
                }
            };
            run_channel(chain_l, os_l, &mut left[pos..pos + n]);
            run_channel(chain_r, os_r, &mut right[pos..pos + n]);
            pos += n;
        }
        if mode == MODE_MS {
            for i in 0..frames {
                let (m, s) = (left[i], right[i]);
                left[i] = m + s;
                right[i] = m - s;
            }
        }
        for i in 0..frames {
            let g = self.gain.next();
            left[i] *= g;
            right[i] *= g;
        }
    }
}

/// Runs a block through the bands that are in the signal.
fn run_bands(chain: &mut Chain, buf: &mut [f32]) {
    let step = 1.0 / buf.len().max(1) as f32;
    for band in chain.iter_mut() {
        if band.mix == 0.0 && band.mix_target == 0.0 {
            continue;
        }
        let mix_step = (band.mix_target - band.mix) * step;
        for s in buf.iter_mut() {
            band.mix += mix_step;
            let wet = band.sections.process(*s);
            *s += (wet - *s) * band.mix;
        }
        band.mix = band.mix_target;
    }
}

impl Processor for EqEight {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.gain.set_time(0.01, self.sr);
        self.spectrum = SpectrumFeed::new(sample_rate, 4096, 1024);
        self.spectrum.set_smoothing(0.7);
        self.stale = true;
        self.fresh = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            if self.v[ANALYZE as usize] >= 0.5 {
                self.spectrum
                    .process_stereo(ctx.audio.left, ctx.audio.right, ctx.view, SPECTRUM);
            }
            ctx.view.publish(SAMPLE_RATE, &[self.sr]);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for chain in &mut self.chains {
            for b in chain.iter_mut() {
                b.sections.reset();
            }
        }
        self.oversample.iter_mut().for_each(Oversampler::reset);
        self.spectrum.reset();
        self.fresh = true;
    }

    fn latency(&self) -> u32 {
        if self.oversampling() {
            self.oversample[0].latency()
        } else {
            0
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.eq_eight",
    name: "EQ Eight",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<EqEight>::default(),
};
