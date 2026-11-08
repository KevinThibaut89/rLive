//! What the dynamics devices share: sidechain controls, static gain curves and time constants.

use crate::dsp::follower::DetectMode;
use crate::dsp::sidechain::{SidechainEq, SidechainSettings};
use crate::dsp::{Biquad, BiquadKind};
use rlive_core::{AudioInput, Item, ParamId, ParamInfo};

// Offsets into a device's block of sidechain parameters.
pub const SC_ON: u32 = 0;
pub const SC_GAIN: u32 = 1;
pub const SC_MIX: u32 = 2;
pub const SC_LISTEN: u32 = 3;
pub const SC_EQ_ON: u32 = 4;
pub const SC_EQ_TYPE: u32 = 5;
pub const SC_EQ_FREQ: u32 = 6;
pub const SC_EQ_Q: u32 = 7;
pub const SC_EQ_GAIN: u32 = 8;

/// How many parameters the sidechain block has without and with its EQ.
pub const SC_LEN_BASIC: u32 = 4;
pub const SC_LEN_EQ: u32 = 9;

/// The window of RMS detection, in seconds: long enough to ignore single peaks.
pub const RMS_SECONDS: f32 = 0.01;

/// The most an upward gain curve may raise a signal, in dB. Upward expansion has no natural
/// limit, and with a high ratio it would otherwise make any loud signal arbitrarily louder. This
/// is the same as the range of the devices' output controls: at a ratio of 1:2 a signal must be
/// 36 dB over the threshold before it matters, so ordinary levels never reach it.
pub const MAX_UPWARD_DB: f32 = 36.0;

/// The sidechain parameters, numbered from `first`. Multiband Dynamics has no sidechain EQ.
pub fn sidechain_params(first: u32, eq: bool) -> Vec<ParamInfo> {
    let mut params = vec![
        ParamInfo::toggle(first + SC_ON, "S/C On", false),
        ParamInfo::continuous(first + SC_GAIN, "S/C Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(first + SC_MIX, "S/C Mix", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(first + SC_LISTEN, "S/C Listen", false),
    ];
    if eq {
        params.extend([
            ParamInfo::toggle(first + SC_EQ_ON, "S/C EQ On", false),
            ParamInfo::stepped(
                first + SC_EQ_TYPE,
                "S/C EQ Type",
                &["Low Pass", "Band Pass", "High Pass", "Notch", "Peak"],
                0,
            ),
            ParamInfo::skewed(
                first + SC_EQ_FREQ,
                "S/C EQ Freq",
                30.0,
                15_000.0,
                1_000.0,
                "Hz",
                3.0,
            ),
            ParamInfo::skewed(first + SC_EQ_Q, "S/C EQ Q", 0.1, 12.0, 0.71, "", 2.0),
            ParamInfo::continuous(first + SC_EQ_GAIN, "S/C EQ Gain", -15.0, 15.0, 0.0, "dB"),
        ]);
    }
    params
}

/// The sidechain controls as a section's items.
pub fn sidechain_items(first: u32, eq: bool) -> Vec<Item> {
    let id = |offset| ParamId(first + offset);
    let mut items = vec![
        Item::Toggle(id(SC_ON)),
        Item::Knob(id(SC_GAIN)),
        Item::Knob(id(SC_MIX)),
        Item::Toggle(id(SC_LISTEN)),
    ];
    if eq {
        items.extend([
            Item::Toggle(id(SC_EQ_ON)),
            Item::Menu(id(SC_EQ_TYPE)),
            Item::Knob(id(SC_EQ_FREQ)),
            Item::Knob(id(SC_EQ_Q)),
            Item::Knob(id(SC_EQ_GAIN)),
        ]);
    }
    items
}

/// A device's sidechain parameters as detector settings. `first` is where its block starts in
/// `v`; `detect` and the times say how the detector's follower behaves.
pub fn sidechain_settings(
    v: &[f64],
    first: u32,
    eq: bool,
    detect: DetectMode,
    attack: f32,
    release: f32,
) -> SidechainSettings {
    let get = |offset: u32| v[(first + offset) as usize];
    let eq_on = eq && get(SC_EQ_ON) >= 0.5;
    SidechainSettings {
        gain_db: get(SC_GAIN) as f32,
        mix: get(SC_MIX) as f32,
        eq: if !eq_on {
            SidechainEq::Off
        } else {
            match get(SC_EQ_TYPE).round() as u32 {
                0 => SidechainEq::LowPass,
                1 => SidechainEq::BandPass,
                2 => SidechainEq::HighPass,
                3 => SidechainEq::Notch,
                _ => SidechainEq::Peak,
            }
        },
        eq_freq: if eq { get(SC_EQ_FREQ) as f32 } else { 1_000.0 },
        eq_q: if eq { get(SC_EQ_Q) as f32 } else { 0.71 },
        eq_gain_db: if eq { get(SC_EQ_GAIN) as f32 } else { 0.0 },
        attack,
        release,
        detect,
        listen: get(SC_LISTEN) >= 0.5,
    }
}

/// How a device's level detector follows its key, given a Peak or RMS choice.
pub fn detector_mode(rms: bool) -> (DetectMode, f32, f32) {
    if rms {
        (DetectMode::Rms, RMS_SECONDS, RMS_SECONDS)
    } else {
        (DetectMode::Peak, 0.0, 0.0)
    }
}

/// The part of a block's sidechain that covers `frames` frames from `start`, if it does.
pub fn sidechain_slice<'a>(
    sidechain: Option<AudioInput<'a>>,
    start: usize,
    frames: usize,
) -> Option<AudioInput<'a>> {
    let s = sidechain?;
    (s.frames() >= start + frames).then(|| AudioInput {
        left: &s.left[start..start + frames],
        right: &s.right[start..start + frames],
    })
}

/// The coefficient of a one-pole smoother with time constant `ms`. Zero follows instantly.
#[inline]
pub fn time_coeff(ms: f32, sample_rate: f32) -> f32 {
    if ms > 0.0 {
        (-1.0 / (ms * 0.001 * sample_rate)).exp()
    } else {
        0.0
    }
}

/// How many dB a soft-knee curve of width `knee` moves a signal `over` dB past its threshold,
/// given the curve's `slope` (dB of change per dB over).
#[inline]
pub fn knee_db(over: f32, knee: f32, slope: f32) -> f32 {
    if knee > 0.0 && over.abs() <= knee / 2.0 {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// The time constant of an automatic release: fast for a short burst, slow when compression has
/// been sustained. `slow` is a slow average of the gain change and `now` the current one.
#[inline]
pub fn auto_release_ms(slow: f32, now: f32, fast_ms: f32, slow_ms: f32) -> f32 {
    let held = (slow.abs() / now.abs().max(0.1)).clamp(0.0, 1.0);
    fast_ms + (slow_ms - fast_ms) * held * held
}

/// How many frames a lookahead of `ms` is at this sample rate.
#[inline]
pub fn ms_to_frames(ms: f32, sample_rate: f32) -> usize {
    (ms * 0.001 * sample_rate).round() as usize
}

/// A Linkwitz-Riley 4th-order low-pass or high-pass: two identical Butterworth sections. A
/// low-pass and high-pass at one frequency are in phase and sum to a flat magnitude.
#[derive(Clone, Copy, Default)]
pub struct Lr4 {
    a: Biquad,
    b: Biquad,
}

impl Lr4 {
    pub fn set(&mut self, kind: BiquadKind, freq: f32, sr: f32) {
        self.a
            .set(kind, freq, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
        self.b
            .set(kind, freq, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.b.process(self.a.process(x))
    }

    pub fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
    }
}
