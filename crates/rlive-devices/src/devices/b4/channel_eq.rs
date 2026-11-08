//! Channel EQ: a console-style three-band EQ with a high-pass switch.
//!
//! Numbers the manual gives: the 100 Hz low shelf at ±15 dB, the mid at ±12 dB between 120 Hz
//! and 7.5 kHz, the high at +15 dB with a low-pass from 20 kHz to 8 kHz as it's cut, and the
//! 80 Hz high-pass. The constants below are ours: the manual doesn't state the shelf's slope
//! curve, the high shelf's frequency, the mid's Q or the filters' orders.

use super::common::{Ramp, CONTROL_FRAMES};
use crate::dsp::spectrum::SpectrumFeed;
use crate::dsp::{Biquad, BiquadKind, Smoother};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext,
    Processor, Section, Status, ViewId, SCOPE_VALUES,
};

pub const HP_ON: u32 = 0;
pub const LOW: u32 = 1;
pub const MID: u32 = 2;
pub const MID_FREQ: u32 = 3;
pub const HIGH: u32 = 4;
pub const OUTPUT: u32 = 5;

pub const SPECTRUM: ViewId = ViewId(0);
/// Where the sample rate sits in the view, for drawing the curve.
pub const SAMPLE_RATE: ViewId = ViewId(SCOPE_VALUES as u32);
pub const VIEW_LEN: usize = SCOPE_VALUES + 1;

/// From the manual.
pub const HP_FREQ: f32 = 80.0;
pub const LOW_FREQ: f32 = 100.0;
pub const LOW_RANGE: f64 = 15.0;
pub const MID_RANGE: f64 = 12.0;
pub const HIGH_RANGE: f64 = 15.0;
pub const LP_OPEN: f32 = 20_000.0;
pub const LP_CLOSED: f32 = 8_000.0;

/// Ours; the manual doesn't say.
pub const HIGH_SHELF_FREQ: f32 = 8_000.0;
pub const MID_Q: f32 = 0.7;

/// The low shelf's Q at a given gain: the curve steepens as the gain grows, staying broad for
/// small moves.
pub fn low_q(gain_db: f32) -> f32 {
    0.5 + 0.4 * (gain_db.abs() / LOW_RANGE as f32).min(1.0)
}

/// The low-pass used when the High control is cut, or `None` when it's at 0 dB or above.
pub fn high_cut_freq(gain_db: f32) -> Option<f32> {
    (gain_db < 0.0)
        .then(|| LP_OPEN * (LP_CLOSED / LP_OPEN).powf((-gain_db / HIGH_RANGE as f32).min(1.0)))
}

/// How much of the low-pass is mixed in: nothing at 0 dB, all of it a decibel below.
fn high_cut_mix(gain_db: f32) -> f32 {
    (-gain_db).clamp(0.0, 1.0)
}

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::toggle(HP_ON, "HP 80 Hz", false),
        ParamInfo::continuous(LOW, "Low", -LOW_RANGE, LOW_RANGE, 0.0, "dB"),
        ParamInfo::continuous(MID, "Mid", -MID_RANGE, MID_RANGE, 0.0, "dB"),
        ParamInfo::skewed(MID_FREQ, "Mid Freq", 120.0, 7_500.0, 1_000.0, "Hz", 2.5),
        ParamInfo::continuous(HIGH, "High", -HIGH_RANGE, HIGH_RANGE, 0.0, "dB"),
        ParamInfo::continuous(OUTPUT, "Output", -12.0, 12.0, 0.0, "dB"),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Display",
            items: vec![Item::Panel("channel_eq_display")],
        },
        Section {
            title: "EQ",
            items: vec![
                Item::Toggle(ParamId(HP_ON)),
                knob(LOW),
                knob(MID_FREQ),
                knob(MID),
                knob(HIGH),
                knob(OUTPUT),
            ],
        },
    ]
}

/// The filter response at `hz`, in decibels, for drawing.
pub fn response_db(
    hp_on: bool,
    low: f32,
    mid: f32,
    mid_freq: f32,
    high: f32,
    hz: f32,
    sr: f32,
) -> f32 {
    let mut b = Biquad::default();
    let mut db = 0.0;
    let mut take = |b: &Biquad| db += 20.0 * b.magnitude(hz, sr).max(1e-6).log10();
    if hp_on {
        b.set(BiquadKind::HighPass, HP_FREQ, 0.707, 0.0, sr);
        take(&b);
    }
    b.set(BiquadKind::LowShelf, LOW_FREQ, low_q(low), low, sr);
    take(&b);
    b.set(BiquadKind::Peak, mid_freq, MID_Q, mid, sr);
    take(&b);
    b.set(BiquadKind::HighShelf, HIGH_SHELF_FREQ, 0.707, high, sr);
    take(&b);
    if let Some(freq) = high_cut_freq(high) {
        b.set(BiquadKind::LowPass, freq, 0.707, 0.0, sr);
        let mix = high_cut_mix(high);
        let gain = 1.0 + mix * (b.magnitude(hz, sr) - 1.0);
        db += 20.0 * gain.abs().max(1e-6).log10();
    }
    db
}

pub struct ChannelEq {
    v: [f64; 6],
    sr: f32,
    /// Per channel: high-pass, low shelf, peak, high shelf, low-pass.
    filters: [[Biquad; 5]; 2],
    low_gain: Ramp,
    mid_gain: Ramp,
    mid_freq: Ramp,
    high_gain: Ramp,
    stale: bool,
    fresh: bool,
    output: Smoother,
    spectrum: SpectrumFeed,
}

impl Default for ChannelEq {
    fn default() -> Self {
        let mut v = [0.0; 6];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            filters: [[Biquad::default(); 5]; 2],
            low_gain: Ramp::new(0.0),
            mid_gain: Ramp::new(0.0),
            mid_freq: Ramp::new(1_000f32.log2()),
            high_gain: Ramp::new(0.0),
            stale: true,
            fresh: true,
            output: Smoother::new(1.0),
            spectrum: SpectrumFeed::new(48_000.0, 4096, 1024),
        }
    }
}

impl ChannelEq {
    fn retune(&mut self) {
        let high = self.high_gain.value;
        let low = self.low_gain.value;
        let mid = self.mid_gain.value;
        let freq = 2f32.powf(self.mid_freq.value);
        let sr = self.sr;
        for f in &mut self.filters {
            f[0].set(BiquadKind::HighPass, HP_FREQ, 0.707, 0.0, sr);
            f[1].set(BiquadKind::LowShelf, LOW_FREQ, low_q(low), low, sr);
            f[2].set(BiquadKind::Peak, freq, MID_Q, mid, sr);
            f[3].set(BiquadKind::HighShelf, HIGH_SHELF_FREQ, 0.707, high, sr);
            if let Some(cut) = high_cut_freq(high) {
                f[4].set(BiquadKind::LowPass, cut, 0.707, 0.0, sr);
            }
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        self.low_gain.target = self.v[LOW as usize] as f32;
        self.mid_gain.target = self.v[MID as usize] as f32;
        self.mid_freq.target = (self.v[MID_FREQ as usize] as f32).log2();
        self.high_gain.target = self.v[HIGH as usize] as f32;
        self.output.target = db_to_gain(self.v[OUTPUT as usize] as f32);
        if self.fresh {
            self.fresh = false;
            for ramp in [
                &mut self.low_gain,
                &mut self.mid_gain,
                &mut self.mid_freq,
                &mut self.high_gain,
            ] {
                ramp.snap(ramp.target);
            }
            self.output.reset();
            self.stale = true;
        }
        let hp = self.v[HP_ON as usize] >= 0.5;
        let frames = audio.frames();
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES);
            let mut moved = self.stale;
            for ramp in [
                &mut self.low_gain,
                &mut self.mid_gain,
                &mut self.mid_freq,
                &mut self.high_gain,
            ] {
                moved |= ramp.tick(n, 0.02, self.sr);
            }
            if moved {
                self.retune();
                self.stale = false;
            }
            let lp_mix = high_cut_mix(self.high_gain.value);
            let [fl, fr] = &mut self.filters;
            for i in pos..pos + n {
                let gain = self.output.next();
                for (filters, sample) in [(&mut *fl, &mut left[i]), (&mut *fr, &mut right[i])] {
                    let mut x = *sample;
                    if hp {
                        x = filters[0].process(x);
                    }
                    x = filters[1].process(x);
                    x = filters[2].process(x);
                    x = filters[3].process(x);
                    if lp_mix > 0.0 {
                        x += (filters[4].process(x) - x) * lp_mix;
                    }
                    *sample = x * gain;
                }
            }
            pos += n;
        }
    }
}

impl Processor for ChannelEq {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.output.set_time(0.01, self.sr);
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
            self.spectrum
                .process_stereo(ctx.audio.left, ctx.audio.right, ctx.view, SPECTRUM);
            ctx.view.publish(SAMPLE_RATE, &[self.sr]);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.filters {
            channel.iter_mut().for_each(Biquad::reset);
        }
        self.spectrum.reset();
        self.fresh = true;
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.channel_eq",
    name: "Channel EQ",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<ChannelEq>::default(),
};
