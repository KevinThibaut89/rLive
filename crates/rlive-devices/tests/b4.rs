//! Batch B4: the EQs, Auto Filter, Auto Pan-Tremolo and Utility, against what their maths and
//! the manual say should happen.

use rlive_core::{
    AudioBuffer, AudioInput, EventQueue, ParamOutQueue, ProcessContext, Processor, TransportInfo,
    ViewChannel, ViewId, ViewWriter,
};
use rlive_devices::devices::b4::{
    auto_filter as af, auto_pan_tremolo as apt, channel_eq as ceq, eq_eight as eq8,
};
use rlive_devices::dsp::filter::{FilterKind, FilterSettings, FilterSlope};
use rlive_devices::dsp::{Biquad, BiquadKind, Smoother};
use rlive_devices::effects::{eq, util, Eq3, Utility};
use rlive_testkit::{assert_no_alloc, peak, rms, sine, tone_amplitude, Harness};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn ready<P: Processor + Default>() -> P {
    let mut p = P::default();
    p.activate(SR, BLOCK);
    p
}

fn set(p: &mut dyn Processor, params: &[(u32, f64)]) {
    for &(id, value) in params {
        p.set_param(id, value);
    }
}

fn render(p: &mut dyn Processor, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        assert_no_alloc(|| h.process(p, cl, cr, &[], &[]));
    }
    (l, r)
}

fn db(linear: f64) -> f64 {
    20.0 * linear.abs().max(1e-12).log10()
}

/// A device's output amplitude for a unit sine, in dB, once settled.
fn tone_gain_db(p: &mut dyn Processor, hz: f64) -> f64 {
    let total = 24_000;
    let input = sine(hz as f32, SR, total);
    let (l, _) = render(p, &input, &input);
    measure(&l, hz)
}

/// The amplitude in dB of `hz` over the last whole cycles of `signal`.
fn measure(signal: &[f32], hz: f64) -> f64 {
    db(amplitude(signal, hz))
}

fn amplitude(signal: &[f32], hz: f64) -> f64 {
    let window = signal.len() / 2;
    let cycles = (window as f64 * hz / SR).round().max(1.0);
    let frames = ((cycles * SR / hz).round() as usize).min(signal.len());
    tone_amplitude(&signal[signal.len() - frames..], hz, SR)
}

fn near(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} != {expected} (±{tol})"
    );
}

/// A repeatable test signal with content everywhere, including near full scale.
fn noise(frames: usize, seed: u32) -> Vec<f32> {
    let mut rng = rlive_devices::dsp::util::Rng::new(seed);
    (0..frames).map(|_| rng.bipolar() * 0.8).collect()
}

/// Runs `p` with a view channel attached and a transport, returning per block what's at `view`.
#[allow(clippy::too_many_arguments)]
fn render_view(
    p: &mut dyn Processor,
    left: &[f32],
    right: &[f32],
    key: Option<(&[f32], &[f32])>,
    view: ViewId,
    values: usize,
    view_len: usize,
    transport: TransportInfo,
) -> (Vec<f32>, Vec<f32>, Vec<Vec<f32>>) {
    let channel = Arc::new(ViewChannel::new(view_len));
    let reader = channel.attach();
    let writer = ViewWriter::new(&channel);
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    let mut seen = Vec::new();
    let beats_per_block = transport.tempo / 60.0 * BLOCK as f64 / SR;
    let ticks_per_block = beats_per_block * rlive_core::PPQ as f64;
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        let n = cl.len();
        let mut events_out = EventQueue::with_capacity(0);
        let mut param_out = ParamOutQueue::with_capacity(0);
        let t = TransportInfo {
            tick: i as f64 * ticks_per_block,
            ..transport
        };
        let sidechain = key.map(|(kl, kr)| AudioInput {
            left: &kl[i * BLOCK..i * BLOCK + n],
            right: &kr[i * BLOCK..i * BLOCK + n],
        });
        let mut ctx = ProcessContext {
            audio: AudioBuffer {
                left: cl,
                right: cr,
            },
            sidechain,
            aux_out: &mut [],
            events: &[],
            events_out: &mut events_out,
            params: &[],
            transport: &t,
            param_out: &mut param_out,
            mod_out: &mut [],
            view: &writer,
        };
        assert_no_alloc(|| p.process(&mut ctx));
        let mut out = vec![0.0; values];
        assert!(reader.read(view, &mut out));
        seen.push(out);
    }
    (l, r, seen)
}

fn playing() -> TransportInfo {
    TransportInfo {
        playing: true,
        sample_rate: SR,
        tempo: 120.0,
        ..Default::default()
    }
}

// ---------------------------------------------------- the original EQ Three and Utility

/// The EQ Three as first shipped, unchanged.
struct OriginalEq3 {
    v: [f64; 8],
    bands: [[Biquad; 3]; 2],
}

impl OriginalEq3 {
    fn new(params: &[(u32, f64)]) -> Self {
        let mut v = [0.0, 200.0, 0.0, 1_000.0, 0.8, 0.0, 4_000.0, 0.0];
        for &(id, value) in params {
            v[id as usize] = value;
        }
        let mut s = Self {
            v,
            bands: [[Biquad::default(); 3]; 2],
        };
        let g = |id: u32| s.v[id as usize] as f32;
        let sr = SR as f32;
        for ch in 0..2 {
            let (low, mid, high) = (
                (g(eq::LOW_FREQ), g(eq::LOW_GAIN)),
                (g(eq::MID_FREQ), g(eq::MID_Q), g(eq::MID_GAIN)),
                (g(eq::HIGH_FREQ), g(eq::HIGH_GAIN)),
            );
            s.bands[ch][0].set(BiquadKind::LowShelf, low.0, 0.707, low.1, sr);
            s.bands[ch][1].set(BiquadKind::Peak, mid.0, mid.1, mid.2, sr);
            s.bands[ch][2].set(BiquadKind::HighShelf, high.0, 0.707, high.1, sr);
        }
        s
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let out = rlive_core::db_to_gain(self.v[eq::OUTPUT as usize] as f32);
        for (ch, buf) in [left, right].into_iter().enumerate() {
            for s in buf.iter_mut() {
                let mut x = *s;
                for b in self.bands[ch].iter_mut() {
                    x = b.process(x);
                }
                *s = x * out;
            }
        }
    }
}

#[test]
fn eq_three_default_path_matches_the_original_sample_for_sample() {
    let (in_l, in_r) = (noise(9_600, 1), noise(9_600, 2));
    let settings: [&[(u32, f64)]; 4] = [
        &[],
        &[
            (eq::LOW_GAIN, -9.0),
            (eq::MID_GAIN, 4.5),
            (eq::HIGH_GAIN, 12.0),
        ],
        &[
            (eq::LOW_FREQ, 50.0),
            (eq::MID_FREQ, 321.0),
            (eq::MID_Q, 3.3),
        ],
        &[
            (eq::HIGH_FREQ, 12_345.0),
            (eq::OUTPUT, -3.5),
            (eq::MID_GAIN, -24.0),
        ],
    ];
    for params in settings {
        let mut original = OriginalEq3::new(params);
        let (mut want_l, mut want_r) = (in_l.clone(), in_r.clone());
        for (l, r) in want_l.chunks_mut(BLOCK).zip(want_r.chunks_mut(BLOCK)) {
            original.render(l, r);
        }
        let mut device = ready::<Eq3>();
        set(&mut device, params);
        let (got_l, got_r) = render(&mut device, &in_l, &in_r);
        assert_eq!(got_l, want_l, "{params:?}");
        assert_eq!(got_r, want_r, "{params:?}");
    }
}

/// The Utility as first shipped, unchanged.
struct OriginalUtility {
    v: [f64; 6],
    gain_l: Smoother,
    gain_r: Smoother,
}

impl OriginalUtility {
    fn new(params: &[(u32, f64)]) -> Self {
        let mut v = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        for &(id, value) in params {
            v[id as usize] = value;
        }
        let mut gain_l = Smoother::new(1.0);
        let mut gain_r = Smoother::new(1.0);
        gain_l.set_time(0.01, SR as f32);
        gain_r.set_time(0.01, SR as f32);
        Self { v, gain_l, gain_r }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let g = rlive_core::db_to_gain(self.v[util::GAIN as usize] as f32);
        let pan = self.v[util::PAN as usize] as f32;
        let width = if self.v[util::MONO as usize] >= 0.5 {
            0.0
        } else {
            self.v[util::WIDTH as usize] as f32
        };
        let inv_l = if self.v[util::INVERT_L as usize] >= 0.5 {
            -1.0
        } else {
            1.0
        };
        let inv_r = if self.v[util::INVERT_R as usize] >= 0.5 {
            -1.0
        } else {
            1.0
        };
        self.gain_l.target = g * (1.0 - pan).min(1.0) * inv_l;
        self.gain_r.target = g * (1.0 + pan).min(1.0) * inv_r;
        for i in 0..left.len() {
            let mid = (left[i] + right[i]) * 0.5;
            let side = (left[i] - right[i]) * 0.5 * width;
            left[i] = (mid + side) * self.gain_l.next();
            right[i] = (mid - side) * self.gain_r.next();
        }
    }
}

#[test]
fn utility_default_path_matches_the_original_sample_for_sample() {
    let (in_l, in_r) = (noise(9_600, 3), noise(9_600, 4));
    let settings: [&[(u32, f64)]; 4] = [
        &[],
        &[(util::GAIN, -6.0), (util::PAN, 0.4)],
        &[(util::WIDTH, 1.7), (util::INVERT_L, 1.0)],
        &[(util::MONO, 1.0), (util::PAN, -1.0), (util::INVERT_R, 1.0)],
    ];
    for params in settings {
        let mut original = OriginalUtility::new(params);
        let (mut want_l, mut want_r) = (in_l.clone(), in_r.clone());
        for (l, r) in want_l.chunks_mut(BLOCK).zip(want_r.chunks_mut(BLOCK)) {
            original.render(l, r);
        }
        let mut device = ready::<Utility>();
        set(&mut device, params);
        let (got_l, got_r) = render(&mut device, &in_l, &in_r);
        assert_eq!(got_l, want_l, "{params:?}");
        assert_eq!(got_r, want_r, "{params:?}");
    }
}

// --------------------------------------------------------------------- EQ Three

fn eq3_crossover(slope: usize, params: &[(u32, f64)]) -> Eq3 {
    let mut p = ready::<Eq3>();
    p.set_param(eq::SLOPE, slope as f64);
    p.set_param(eq::LOW_FREQ, 500.0);
    p.set_param(eq::HIGH_FREQ, 2_000.0);
    set(&mut p, params);
    p
}

/// The low band's gain in dB, from the filter's sections. At 48 dB the mismatch in the Qs (see
/// `eq.rs`) moves the crossover point up from -6 dB.
fn expected_low(slope: usize, ratio: f64, exponent: f64) -> f64 {
    if slope == eq::SLOPE_24 {
        return db(1.0 / (1.0 + ratio.powf(exponent)));
    }
    let section = |q: f64| {
        let w: f64 = ratio;
        let q = q * eq::Q_48_MISMATCH as f64;
        1.0 / ((1.0 - w * w).powi(2) + (w / q).powi(2)).sqrt()
    };
    db((section(0.541_196) * section(1.306_563)).powi(2))
}

#[test]
fn eq_three_low_band_has_the_crossover_slope_it_says() {
    // |LP| of a Linkwitz-Riley 4 is 1 / (1 + (f/fc)^4), of a 8th-order one 1 / (1 + (f/fc)^8).
    let only_low = [(eq::MID_ON, 0.0), (eq::HIGH_ON, 0.0)];
    for (slope, exponent) in [(eq::SLOPE_24, 4.0), (eq::SLOPE_48, 8.0)] {
        for f in [125.0, 500.0, 1_000.0, 2_000.0] {
            let mut p = eq3_crossover(slope, &only_low);
            // The digital filter's frequency warp, so the sums are exact.
            let warp = |hz: f64| (std::f64::consts::PI * hz / SR).tan();
            let ratio: f64 = warp(f) / warp(500.0);
            let want = expected_low(slope, ratio, exponent);
            near(
                tone_gain_db(&mut p, f),
                want,
                if slope == eq::SLOPE_24 { 0.1 } else { 0.8 },
                &format!("slope {slope} at {f}"),
            );
        }
    }
}

#[test]
fn eq_three_24_db_sums_flat_and_48_db_colours_slightly() {
    let at = |slope: usize, f: f64| tone_gain_db(&mut eq3_crossover(slope, &[]), f);
    let freqs = [
        80.0, 250.0, 400.0, 500.0, 700.0, 1_000.0, 1_400.0, 2_000.0, 3_000.0, 9_000.0,
    ];
    let spread = |slope| {
        freqs
            .iter()
            .map(|f| at(slope, *f).abs())
            .fold(0.0f64, f64::max)
    };
    let flat = spread(eq::SLOPE_24);
    let coloured = spread(eq::SLOPE_48);
    assert!(flat < 0.05, "24 dB should be flat: {flat}");
    assert!(
        coloured > 0.2 && coloured < 1.2,
        "48 dB should be slightly coloured: {coloured}"
    );
}

#[test]
fn eq_three_bands_go_from_silence_to_plus_six_and_the_switch_kills_them() {
    let f = 100.0;
    near(
        tone_gain_db(&mut eq3_crossover(eq::SLOPE_24, &[(eq::LOW_GAIN, 6.0)]), f),
        6.0,
        0.1,
        "+6",
    );
    near(
        tone_gain_db(&mut eq3_crossover(eq::SLOPE_24, &[(eq::LOW_GAIN, 12.0)]), f),
        6.0,
        0.1,
        "the crossover modes stop at +6",
    );
    assert!(
        tone_gain_db(
            &mut eq3_crossover(eq::SLOPE_24, &[(eq::LOW_GAIN, -70.0)]),
            f
        ) < -55.0
    );
    assert!(tone_gain_db(&mut eq3_crossover(eq::SLOPE_24, &[(eq::LOW_ON, 0.0)]), f) < -55.0);
    // The other two bands still pass.
    near(
        tone_gain_db(
            &mut eq3_crossover(eq::SLOPE_24, &[(eq::LOW_ON, 0.0)]),
            8_000.0,
        ),
        0.0,
        0.1,
        "kill leaves the highs",
    );
}

#[test]
fn eq_three_leds_light_at_minus_24_db_even_for_a_band_thats_off() {
    let quiet = sine(100.0, SR, 9_600)
        .iter()
        .map(|s| s * 0.02)
        .collect::<Vec<_>>();
    let loud: Vec<f32> = sine(100.0, SR, 9_600).iter().map(|s| s * 0.2).collect();
    let run = |signal: &[f32]| {
        let mut p = eq3_crossover(eq::SLOPE_24, &[(eq::LOW_ON, 0.0)]);
        let (_, _, seen) = render_view(
            &mut p,
            signal,
            signal,
            None,
            eq::LEDS,
            3,
            eq::VIEW_LEN,
            playing(),
        );
        seen.last().unwrap().clone()
    };
    assert_eq!(run(&loud)[0], 1.0, "-14 dB lights it");
    assert_eq!(run(&quiet)[0], 0.0, "-34 dB doesn't");
}

// -------------------------------------------------------------------- Channel EQ

#[test]
fn channel_eq_high_pass_is_at_80_hz() {
    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::HP_ON, 1.0)]);
    near(tone_gain_db(&mut p, 80.0), -3.01, 0.2, "the corner");
    let mut p = ready::<ceq::ChannelEq>();
    near(tone_gain_db(&mut p, 80.0), 0.0, 0.01, "off");
}

#[test]
fn channel_eq_low_is_a_100_hz_shelf_of_15_db() {
    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::LOW, 15.0)]);
    near(tone_gain_db(&mut p, 20.0), 15.0, 0.5, "deep in the shelf");
    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::LOW, 15.0)]);
    near(tone_gain_db(&mut p, 100.0), 7.5, 0.5, "half at the corner");
    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::LOW, 15.0)]);
    near(tone_gain_db(&mut p, 2_000.0), 0.0, 0.3, "gone above");
}

#[test]
fn channel_eq_low_shelf_curve_changes_with_gain() {
    let q0 = ceq::low_q(0.0);
    let q15 = ceq::low_q(15.0);
    assert!(q15 > q0);
    // A gain-proportional curve would give the same shape at any gain: compare a mid-shelf point.
    let share = |gain: f64| {
        let mut p = ready::<ceq::ChannelEq>();
        set(&mut p, &[(ceq::LOW, gain)]);
        tone_gain_db(&mut p, 200.0) / gain
    };
    assert!((share(3.0) - share(15.0)).abs() > 0.01);
}

#[test]
fn channel_eq_mid_is_a_sweepable_peak_of_12_db() {
    for f in [120.0, 900.0, 7_500.0] {
        let mut p = ready::<ceq::ChannelEq>();
        set(&mut p, &[(ceq::MID, 12.0), (ceq::MID_FREQ, f)]);
        near(tone_gain_db(&mut p, f), 12.0, 0.1, "boost");
        let mut p = ready::<ceq::ChannelEq>();
        set(&mut p, &[(ceq::MID, -12.0), (ceq::MID_FREQ, f)]);
        near(tone_gain_db(&mut p, f), -12.0, 0.1, "cut");
    }
}

#[test]
fn channel_eq_high_boosts_as_a_shelf_and_cuts_with_a_low_pass() {
    assert_eq!(ceq::high_cut_freq(0.0), None);
    assert_eq!(ceq::high_cut_freq(5.0), None);
    let start = ceq::high_cut_freq(-0.000_1).unwrap();
    near(start as f64, 20_000.0, 2.0, "starts at 20 kHz");
    near(
        ceq::high_cut_freq(-15.0).unwrap() as f64,
        8_000.0,
        0.5,
        "ends at 8 kHz",
    );

    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::HIGH, 15.0)]);
    let boosted = tone_gain_db(&mut p, 20_000.0);
    assert!(boosted > 13.0 && boosted < 15.2, "{boosted}");

    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::HIGH, -15.0)]);
    let at_8k = tone_gain_db(&mut p, 8_000.0);
    near(
        at_8k,
        -7.5 - 3.01,
        0.7,
        "shelf half way and low-pass at -3 dB",
    );
    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::HIGH, 3.0)]);
    let low = tone_gain_db(&mut p, 200.0);
    near(low, 0.0, 0.05, "a boost leaves the lows");
}

#[test]
fn channel_eq_output_and_a_flat_setting_are_exact() {
    let mut p = ready::<ceq::ChannelEq>();
    let input = sine(440.0, SR, 4_800);
    let (l, _) = render(&mut p, &input, &input);
    assert_eq!(l, input, "flat is untouched");
    let mut p = ready::<ceq::ChannelEq>();
    set(&mut p, &[(ceq::OUTPUT, -6.0)]);
    near(tone_gain_db(&mut p, 440.0), -6.0, 0.01, "output");
}

// --------------------------------------------------------------------- EQ Eight

const A: usize = 0;
const B: usize = 1;

/// An EQ Eight with every band off except what's set here.
fn eq8_with(bands: &[(usize, usize, usize, f64, f64, f64)], global: &[(u32, f64)]) -> eq8::EqEight {
    let mut p = ready::<eq8::EqEight>();
    for curve in 0..2 {
        for band in 0..eq8::BANDS {
            p.set_param(eq8::band_param(curve, band, eq8::B_ON), 0.0);
        }
    }
    p.set_param(eq8::ADAPTIVE_Q, 0.0);
    for &(curve, band, response, freq, gain, q) in bands {
        p.set_param(eq8::band_param(curve, band, eq8::B_ON), 1.0);
        p.set_param(eq8::band_param(curve, band, eq8::B_TYPE), response as f64);
        p.set_param(eq8::band_param(curve, band, eq8::B_FREQ), freq);
        p.set_param(eq8::band_param(curve, band, eq8::B_GAIN), gain);
        p.set_param(eq8::band_param(curve, band, eq8::B_Q), q);
    }
    set(&mut p, global);
    p
}

const LOW_CUT_48: usize = 0;
const LOW_CUT_12: usize = 1;
const LOW_SHELF: usize = 2;
const PEAK: usize = 3;
const NOTCH: usize = 4;
const HIGH_SHELF: usize = 5;
const HIGH_CUT_12: usize = 6;
const HIGH_CUT_48: usize = 7;

/// `|H|` in dB of a Butterworth low-pass of the given order.
fn butterworth_db(ratio: f64, order: i32) -> f64 {
    -10.0 * (1.0 + ratio.powi(2 * order)).log10()
}

#[test]
fn eq_eight_defaults_leave_the_signal_alone() {
    let input = noise(4_800, 5);
    let mut p = ready::<eq8::EqEight>();
    let (l, _) = render(&mut p, &input, &input);
    assert_eq!(l, input);
}

#[test]
fn eq_eight_peak_reaches_its_gain_at_its_frequency() {
    let mut p = eq8_with(&[(A, 3, PEAK, 1_000.0, 9.0, 2.0)], &[]);
    near(tone_gain_db(&mut p, 1_000.0), 9.0, 0.05, "peak");
    let mut p = eq8_with(&[(A, 3, PEAK, 1_000.0, -9.0, 2.0)], &[]);
    near(tone_gain_db(&mut p, 1_000.0), -9.0, 0.05, "cut");
}

#[test]
fn eq_eight_shelves_reach_their_gain_past_their_corner() {
    let mut p = eq8_with(&[(A, 0, LOW_SHELF, 1_000.0, 12.0, 0.71)], &[]);
    near(tone_gain_db(&mut p, 30.0), 12.0, 0.2, "low shelf below");
    let mut p = eq8_with(&[(A, 0, LOW_SHELF, 1_000.0, 12.0, 0.71)], &[]);
    near(tone_gain_db(&mut p, 15_000.0), 0.0, 0.1, "low shelf above");
    let mut p = eq8_with(&[(A, 0, HIGH_SHELF, 1_000.0, -9.0, 0.71)], &[]);
    near(
        tone_gain_db(&mut p, 18_000.0),
        -9.0,
        0.3,
        "high shelf above",
    );
    let mut p = eq8_with(&[(A, 0, HIGH_SHELF, 1_000.0, -9.0, 0.71)], &[]);
    near(tone_gain_db(&mut p, 40.0), 0.0, 0.1, "high shelf below");
}

#[test]
fn eq_eight_notch_cuts_only_near_its_frequency() {
    let mut p = eq8_with(&[(A, 3, NOTCH, 1_000.0, 0.0, 3.0)], &[]);
    assert!(tone_gain_db(&mut p, 1_000.0) < -55.0);
    let mut p = eq8_with(&[(A, 3, NOTCH, 1_000.0, 0.0, 3.0)], &[]);
    near(tone_gain_db(&mut p, 250.0), 0.0, 0.3, "far below");
}

#[test]
fn eq_eight_cuts_follow_the_butterworth_response_they_are() {
    let q = std::f64::consts::FRAC_1_SQRT_2;
    let check = |response: usize, order: i32, high_cut: bool| {
        for ratio in [0.5, 1.0, 2.0] {
            let f: f64 = 1_000.0;
            let hz = f * ratio;
            let mut p = eq8_with(&[(A, 3, response, f, 0.0, q)], &[]);
            let measured = tone_gain_db(&mut p, hz);
            let want = butterworth_db(if high_cut { ratio } else { 1.0 / ratio }, order);
            near(measured, want, 0.5, &format!("response {response} at {hz}"));
        }
    };
    check(HIGH_CUT_12, 2, true);
    check(LOW_CUT_12, 2, false);
    check(HIGH_CUT_48, 8, true);
    check(LOW_CUT_48, 8, false);
}

#[test]
fn eq_eight_stereo_runs_curve_a_on_both_channels_and_ignores_b() {
    let mut p = eq8_with(
        &[
            (A, 3, PEAK, 1_000.0, 6.0, 1.0),
            (B, 3, PEAK, 1_000.0, -12.0, 1.0),
        ],
        &[],
    );
    let input = sine(1_000.0, SR, 24_000);
    let (l, r) = render(&mut p, &input, &input);
    near(measure(&l, 1_000.0), 6.0, 0.05, "left");
    near(measure(&r, 1_000.0), 6.0, 0.05, "right");
}

#[test]
fn eq_eight_left_right_mode_gives_each_channel_its_curve() {
    let mut p = eq8_with(
        &[
            (A, 3, PEAK, 1_000.0, 6.0, 1.0),
            (B, 3, PEAK, 1_000.0, -12.0, 1.0),
        ],
        &[(eq8::MODE, eq8::MODE_LR as f64)],
    );
    let input = sine(1_000.0, SR, 24_000);
    let (l, r) = render(&mut p, &input, &input);
    near(measure(&l, 1_000.0), 6.0, 0.05, "A on the left");
    near(measure(&r, 1_000.0), -12.0, 0.05, "B on the right");
}

#[test]
fn eq_eight_mid_side_mode_filters_the_mid_and_side_separately() {
    let make = || {
        eq8_with(
            &[
                (A, 3, PEAK, 1_000.0, 6.0, 1.0),
                (B, 3, PEAK, 1_000.0, -12.0, 1.0),
            ],
            &[(eq8::MODE, eq8::MODE_MS as f64)],
        )
    };
    let tone = sine(1_000.0, SR, 24_000);
    let (l, r) = render(&mut make(), &tone, &tone);
    near(measure(&l, 1_000.0), 6.0, 0.05, "mono is all mid");
    near(measure(&r, 1_000.0), 6.0, 0.05, "and stays centered");
    let inverted: Vec<f32> = tone.iter().map(|s| -s).collect();
    let (l, r) = render(&mut make(), &tone, &inverted);
    near(measure(&l, 1_000.0), -12.0, 0.05, "anti-phase is all side");
    near(measure(&r, 1_000.0), -12.0, 0.05, "");
}

#[test]
fn eq_eight_adaptive_q_narrows_a_band_as_its_gain_grows() {
    assert_eq!(eq8::adaptive_q(1.0, 0.0), 1.0);
    assert!(eq8::adaptive_q(1.0, 12.0) > eq8::adaptive_q(1.0, 6.0));
    let skirt = |adaptive: f64| {
        let mut p = eq8_with(
            &[(A, 3, PEAK, 1_000.0, 12.0, 1.0)],
            &[(eq8::ADAPTIVE_Q, adaptive)],
        );
        tone_gain_db(&mut p, 2_000.0)
    };
    let (fixed, adaptive) = (skirt(0.0), skirt(1.0));
    assert!(adaptive < fixed - 1.0, "fixed {fixed}, adaptive {adaptive}");
    // Its centre doesn't move.
    let mut p = eq8_with(
        &[(A, 3, PEAK, 1_000.0, 12.0, 1.0)],
        &[(eq8::ADAPTIVE_Q, 1.0)],
    );
    near(tone_gain_db(&mut p, 1_000.0), 12.0, 0.05, "centre");
}

#[test]
fn eq_eight_scale_applies_to_gain_bands_only() {
    let mut p = eq8_with(&[(A, 3, PEAK, 1_000.0, 6.0, 1.0)], &[(eq8::SCALE, 50.0)]);
    near(tone_gain_db(&mut p, 1_000.0), 3.0, 0.05, "half");
    let mut p = eq8_with(&[(A, 3, PEAK, 1_000.0, 6.0, 1.0)], &[(eq8::SCALE, 200.0)]);
    near(tone_gain_db(&mut p, 1_000.0), 12.0, 0.05, "double");
    let mut p = eq8_with(&[(A, 3, PEAK, 1_000.0, 6.0, 1.0)], &[(eq8::SCALE, -100.0)]);
    near(tone_gain_db(&mut p, 1_000.0), -6.0, 0.05, "inverted");
    // A cut is unaffected.
    let mut p = eq8_with(
        &[(A, 3, HIGH_CUT_12, 1_000.0, 6.0, 0.707)],
        &[(eq8::SCALE, 200.0)],
    );
    near(
        tone_gain_db(&mut p, 1_000.0),
        -3.01,
        0.2,
        "cut ignores scale",
    );
}

#[test]
fn eq_eight_global_gain_and_a_band_off_do_what_they_say() {
    let mut p = eq8_with(&[], &[(eq8::GAIN, -3.0)]);
    near(tone_gain_db(&mut p, 440.0), -3.0, 0.01, "gain");
    let mut p = eq8_with(&[(A, 3, PEAK, 1_000.0, 12.0, 1.0)], &[]);
    p.set_param(eq8::band_param(A, 3, eq8::B_ON), 0.0);
    near(tone_gain_db(&mut p, 1_000.0), 0.0, 0.01, "off");
}

#[test]
fn eq_eight_audition_hears_only_the_held_band() {
    let make = || {
        eq8_with(
            &[
                (A, 0, PEAK, 300.0, 12.0, 4.0),
                (A, 3, PEAK, 3_000.0, 12.0, 4.0),
            ],
            &[(eq8::AUDITION, 1.0), (eq8::AUDITION_BAND, 4.0)],
        )
    };
    near(
        tone_gain_db(&mut make(), 3_000.0),
        12.0,
        0.1,
        "band 4 stays",
    );
    near(
        tone_gain_db(&mut make(), 300.0),
        0.0,
        0.1,
        "band 1 is silenced",
    );
    // Holding no band (the default) leaves all bands in, with Audition on.
    let mut p = eq8_with(
        &[(A, 0, PEAK, 300.0, 12.0, 4.0)],
        &[(eq8::AUDITION, 1.0), (eq8::AUDITION_BAND, 0.0)],
    );
    near(tone_gain_db(&mut p, 300.0), 12.0, 0.1, "none held");
}

#[test]
fn eq_eight_oversampling_reports_its_latency_and_tracks_high_frequencies_better() {
    let mut p = eq8_with(
        &[(A, 3, PEAK, 18_000.0, 12.0, 1.0)],
        &[(eq8::OVERSAMPLING, 1.0)],
    );
    let latency = p.latency() as usize;
    assert!(latency > 0);
    // Measure the delay of an impulse.
    let mut input = vec![0.0f32; 1024];
    input[10] = 1.0;
    let (l, _) = render(&mut p, &input, &input);
    let peak_at = l
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    near(peak_at as f64, (10 + latency) as f64, 3.0, "impulse peak");

    // The analog peaking filter at 21 kHz: the 2x path is warped much less.
    let analog = analog_peak_db(21_000.0, 18_000.0, 12.0, 1.0);
    let measure_at = |oversample: f64| {
        let mut p = eq8_with(
            &[(A, 3, PEAK, 18_000.0, 12.0, 1.0)],
            &[(eq8::OVERSAMPLING, oversample)],
        );
        tone_gain_db(&mut p, 21_000.0)
    };
    let (off, on) = (measure_at(0.0), measure_at(1.0));
    assert!(
        (on - analog).abs() < (off - analog).abs() - 0.5,
        "analog {analog}, 1x {off}, 2x {on}"
    );
}

/// `|H|` in dB of the analog prototype of the RBJ peaking filter.
fn analog_peak_db(hz: f64, fc: f64, gain_db: f64, q: f64) -> f64 {
    let a = 10f64.powf(gain_db / 40.0);
    let w = hz / fc;
    let num = ((1.0 - w * w).powi(2) + (w * a / q).powi(2)).sqrt();
    let den = ((1.0 - w * w).powi(2) + (w / (a * q)).powi(2)).sqrt();
    20.0 * (num / den).log10()
}

// ------------------------------------------------------------------- Auto Filter

fn af_with(params: &[(u32, f64)]) -> af::AutoFilter {
    let mut p = ready::<af::AutoFilter>();
    set(&mut p, params);
    p
}

fn filter_setting(kind: FilterKind, slope: FilterSlope, cutoff: f32, res: f32) -> FilterSettings {
    FilterSettings {
        kind,
        slope,
        cutoff,
        resonance: res,
        ..Default::default()
    }
}

#[test]
fn auto_filter_types_match_the_library_curves() {
    use af::*;
    let f = 2_000.0;
    let cases = [
        (T_LOW_PASS, FilterKind::LowPass),
        (T_HIGH_PASS, FilterKind::HighPass),
        (T_BAND_PASS, FilterKind::BandPass),
        (T_NOTCH, FilterKind::Notch),
    ];
    for (ty, kind) in cases {
        for (slope_i, slope) in [FilterSlope::Db12, FilterSlope::Db24]
            .into_iter()
            .enumerate()
        {
            for hz in [500.0, 2_000.0, 8_000.0] {
                let mut p = af_with(&[
                    (TYPE, ty as f64),
                    (FREQ, f),
                    (RES, 30.0),
                    (SLOPE, slope_i as f64),
                ]);
                let want = 20.0
                    * filter_setting(kind, slope, f as f32, 0.3)
                        .magnitude(hz as f32, SR as f32)
                        .max(1e-6)
                        .log10() as f64;
                let got = tone_gain_db(&mut p, hz).max(-110.0);
                near(
                    got,
                    want.max(-110.0),
                    0.3,
                    &format!("type {ty} slope {slope_i} at {hz}"),
                );
            }
        }
    }
}

#[test]
fn auto_filter_morph_slopes_are_6_12_24_and_48_db_an_octave() {
    use af::*;
    let measure_slope = |morph_slope: f64, cutoff: f64| {
        let at = |hz| {
            let mut p = af_with(&[
                (TYPE, T_MORPH as f64),
                (FREQ, cutoff),
                (RES, 0.0),
                (MORPH, 0.0),
                (MORPH_SLOPE, morph_slope),
            ]);
            tone_gain_db(&mut p, hz)
        };
        // Well above the cutoff, and well below where the digital filter's warp matters.
        at(cutoff * 5.0) - at(cutoff * 10.0)
    };
    for (index, want) in [(0.0, 6.0), (1.0, 12.0), (2.0, 24.0)] {
        near(
            measure_slope(index, 200.0),
            want,
            2.0,
            &format!("slope {index}"),
        );
    }
    // 48 dB is the 24 dB filter run twice: exactly double the dB.
    let at = |morph_slope: f64| {
        let mut p = af_with(&[
            (TYPE, T_MORPH as f64),
            (FREQ, 1_000.0),
            (RES, 0.0),
            (MORPH, 0.0),
            (MORPH_SLOPE, morph_slope),
        ]);
        tone_gain_db(&mut p, 2_000.0)
    };
    near(at(3.0), 2.0 * at(2.0), 0.1, "doubled");
}

#[test]
fn auto_filter_morph_runs_low_pass_band_pass_high_pass() {
    use af::*;
    let at = |morph: f64, hz: f64| {
        let mut p = af_with(&[
            (TYPE, T_MORPH as f64),
            (FREQ, 1_000.0),
            (RES, 0.0),
            (MORPH, morph),
            (MORPH_SLOPE, 2.0),
        ]);
        tone_gain_db(&mut p, hz)
    };
    assert!(at(0.0, 100.0) > at(0.0, 10_000.0) + 30.0, "low-pass");
    assert!(at(100.0, 10_000.0) > at(100.0, 100.0) + 30.0, "high-pass");
    let (low, mid, high) = (at(50.0, 100.0), at(50.0, 1_000.0), at(50.0, 10_000.0));
    assert!(
        mid > low + 20.0 && mid > high + 20.0,
        "band-pass {low} {mid} {high}"
    );
}

#[test]
fn auto_filter_dj_control_is_bipolar_with_resonance_growing_to_the_ends() {
    use af::*;
    let at = |control: f64, res: f64, hz: f64| {
        let mut p = af_with(&[(TYPE, T_DJ as f64), (CONTROL, control), (RES, res)]);
        tone_gain_db(&mut p, hz)
    };
    near(at(0.0, 0.0, 1_000.0), 0.0, 1e-3, "centre is flat");
    assert!(at(60.0, 0.0, 40.0) < -30.0, "up cuts the lows");
    near(at(60.0, 0.0, 15_000.0), 0.0, 1.0, "and keeps the highs");
    assert!(at(-60.0, 0.0, 15_000.0) < -30.0, "down cuts the highs");
    // The cutoff at a control value, by the device's mapping.
    let boost = |control: f64| {
        let cutoff = DJ_LOW_HZ * 2f32.powf(control as f32 * 0.01 * DJ_OCTAVES);
        at(control, 80.0, cutoff as f64)
    };
    assert!(
        boost(90.0) > boost(20.0) + 1.0,
        "{} {}",
        boost(90.0),
        boost(20.0)
    );
}

#[test]
fn auto_filter_comb_has_peaks_or_notches_at_multiples_of_the_cutoff() {
    use af::*;
    let at = |morph: f64, hz: f64| {
        let mut p = af_with(&[
            (TYPE, T_COMB as f64),
            (FREQ, 1_000.0),
            (RES, 100.0),
            (MORPH, morph),
        ]);
        amplitude_at(&mut p, hz)
    };
    let g: f64 = 0.97;
    let trough = (1.0 - g) / (1.0 + g);
    near(at(0.0, 2_000.0), 1.0, 0.02, "a peak at 2 x cutoff");
    near(at(0.0, 2_500.0), trough, 0.01, "midway is low");
    near(at(100.0, 2_000.0), trough, 0.01, "a notch at 2 x cutoff");
    near(at(100.0, 2_500.0), 1.0, 0.02, "midway passes");
}

fn amplitude_at(p: &mut dyn Processor, hz: f64) -> f64 {
    let input = sine(hz as f32, SR, 96_000);
    let (l, _) = render(p, &input, &input);
    amplitude(&l, hz)
}

#[test]
fn auto_filter_resampling_aliases_and_ignores_resonance() {
    use af::*;
    let input = sine(5_000.0, SR, 48_000);
    let run = |res: f64| {
        let mut p = af_with(&[(TYPE, T_RESAMPLING as f64), (FREQ, 4_000.0), (RES, res)]);
        render(&mut p, &input, &input).0
    };
    let out = run(0.0);
    // Held at 8 kHz, a 5 kHz tone folds to 3 kHz.
    let alias = tone_amplitude(&out[24_000..], 3_000.0, SR);
    assert!(alias > 0.5, "aliases: {alias}");
    assert_eq!(out, run(100.0), "Res does nothing");
}

#[test]
fn auto_filter_notch_plus_lp_moves_its_notch_towards_the_cutoff() {
    use af::*;
    let at = |morph: f64, hz: f64| {
        let mut p = af_with(&[
            (TYPE, T_NOTCH_LP as f64),
            (FREQ, 4_000.0),
            (RES, 0.0),
            (MORPH, morph),
        ]);
        tone_gain_db(&mut p, hz)
    };
    near(
        notch_lp_notch(4_000.0, 1.0) as f64,
        4_000.0,
        1e-3,
        "at the cutoff at 100%",
    );
    let lo = notch_lp_notch(4_000.0, 0.0);
    assert!(lo < 4_000.0);
    assert!(at(0.0, lo as f64) < -40.0, "deep at the far position");
    assert!(at(100.0, 4_000.0) < -40.0, "deep at the cutoff");
    assert!(at(100.0, lo as f64) > -20.0, "and shallow far away");
}

#[test]
fn auto_filter_vowel_peaks_at_its_formants_and_morph_normalises_their_gain() {
    use af::*;
    let at = |formant: f64, pitch: f64, morph: f64, hz: f64| {
        let mut p = af_with(&[
            (TYPE, T_VOWEL as f64),
            (FORMANT, formant),
            (PITCH, pitch),
            (MORPH, morph),
        ]);
        tone_gain_db(&mut p, hz)
    };
    // "a": a formant at 800 Hz, and not at 500.
    assert!(at(0.0, 0.0, 0.0, 800.0) > at(0.0, 0.0, 0.0, 500.0) + 10.0);
    // A semitone-exact shift of twelve doubles it.
    assert!(at(0.0, 12.0, 0.0, 1_600.0) > at(0.0, 12.0, 0.0, 1_000.0) + 10.0);
    // "e" has its second formant at 2 kHz, lower than the first until Morph raises it.
    let flat = at(25.0, 0.0, 100.0, 2_000.0);
    let shaped = at(25.0, 0.0, 0.0, 2_000.0);
    assert!(flat > shaped + 10.0, "{flat} {shaped}");
}

#[test]
fn auto_filter_circuits_affect_only_the_types_that_have_them_and_differ_with_drive() {
    use af::*;
    let input: Vec<f32> = sine(220.0, SR, 24_000).iter().map(|s| s * 0.5).collect();
    let run = |ty: usize, circuit: f64, drive: f64| {
        let mut p = af_with(&[
            (TYPE, ty as f64),
            (FREQ, 2_000.0),
            (RES, 50.0),
            (CIRCUIT, circuit),
            (DRIVE, drive),
        ]);
        render(&mut p, &input, &input).0
    };
    let diff = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
    };
    assert!(
        diff(&run(T_COMB, 0.0, 0.0), &run(T_COMB, 2.0, 0.0)) == 0.0,
        "Comb has no circuit"
    );
    assert!(
        diff(&run(T_LOW_PASS, 0.0, 50.0), &run(T_LOW_PASS, 3.0, 50.0)) > 0.02,
        "SVF and PRD differ"
    );
    assert!(
        diff(&run(T_LOW_PASS, 1.0, 50.0), &run(T_LOW_PASS, 2.0, 50.0)) > 0.02,
        "DFM and MS2 differ"
    );
}

#[test]
fn auto_filter_drive_is_clean_at_zero_and_adds_harmonics_when_up() {
    use af::*;
    let input: Vec<f32> = sine(1_000.0, SR, 24_000).iter().map(|s| s * 0.5).collect();
    let run = |drive: f64| {
        let mut p = af_with(&[(FREQ, 20_000.0), (RES, 0.0), (DRIVE, drive)]);
        let (l, _) = render(&mut p, &input, &input);
        tone_amplitude(&l[12_000..], 3_000.0, SR)
    };
    assert!(run(0.0) < 1e-4, "clean");
    assert!(run(80.0) > run(0.0) + 0.02, "driven");
}

#[test]
fn auto_filter_clip_output_and_mix_do_what_they_say() {
    use af::*;
    let hot: Vec<f32> = sine(1_000.0, SR, 9_600).iter().map(|s| s * 3.0).collect();
    let mut p = af_with(&[(FREQ, 20_000.0), (RES, 0.0), (CLIP, 1.0)]);
    let (l, _) = render(&mut p, &hot, &hot);
    assert!(peak(&l[4_800..]) <= 1.0001, "{}", peak(&l[4_800..]));

    let mut p = af_with(&[(FREQ, 20_000.0), (RES, 0.0), (CLIP, 1.0)]);
    let (_, _, led) = render_view(
        &mut p,
        &hot,
        &hot,
        None,
        af::CLIP_LED,
        1,
        af::VIEW_LEN,
        playing(),
    );
    assert_eq!(led.last().unwrap()[0], 1.0, "the LED flashes");

    let quiet: Vec<f32> = sine(1_000.0, SR, 9_600).iter().map(|s| s * 0.1).collect();
    let mut p = af_with(&[(FREQ, 20_000.0), (RES, 0.0), (CLIP, 1.0)]);
    let (l, _) = render(&mut p, &quiet, &quiet);
    assert!(peak(&l[4_800..]) < 0.11, "transparent below the knee");

    let mut p = af_with(&[(FREQ, 200.0), (RES, 0.0), (OUTPUT, -6.0)]);
    // Resonance 0 is two real poles at the cutoff: 1 / (1 + (f/fc)^2).
    near(
        tone_gain_db(&mut p, 100.0),
        -6.0 + db(1.0 / 1.25),
        0.5,
        "output on top of the filter",
    );

    let mut p = af_with(&[(FREQ, 200.0), (RES, 0.0), (DRY_WET, 0.0)]);
    near(tone_gain_db(&mut p, 5_000.0), 0.0, 0.01, "dry");
}

/// The cutoffs the device reports per block, given parameters and a signal.
fn cutoffs(params: &[(u32, f64)], seconds: f32, transport: TransportInfo) -> (Vec<f32>, Vec<f32>) {
    let frames = (SR as f32 * seconds) as usize;
    let input: Vec<f32> = vec![0.0; frames];
    let mut p = af_with(params);
    let (_, _, seen) = render_view(
        &mut p,
        &input,
        &input,
        None,
        af::CUTOFFS,
        2,
        af::VIEW_LEN,
        transport,
    );
    (
        seen.iter().map(|v| v[0]).collect(),
        seen.iter().map(|v| v[1]).collect(),
    )
}

#[test]
fn auto_filter_lfo_sweeps_the_cutoff_by_its_amount() {
    use af::*;
    let (l, _) = cutoffs(
        &[(FREQ, 1_000.0), (LFO_AMOUNT, 50.0), (LFO_RATE, 4.0)],
        1.0,
        TransportInfo {
            playing: false,
            ..playing()
        },
    );
    let max = l.iter().cloned().fold(0.0, f32::max);
    let min = l.iter().cloned().fold(f32::MAX, f32::min);
    near(
        max as f64,
        1_000.0 * 2f64.powf(0.5 * LFO_OCTAVES as f64),
        80.0,
        "top",
    );
    near(
        min as f64,
        1_000.0 * 2f64.powf(-0.5 * LFO_OCTAVES as f64),
        15.0,
        "bottom",
    );
}

#[test]
fn auto_filter_lfo_phase_opposes_the_channels_and_spin_detunes_them() {
    use af::*;
    let (l, r) = cutoffs(
        &[
            (LFO_AMOUNT, 100.0),
            (LFO_RATE, 2.0),
            (LFO_PHASE, 180.0),
            (LFO_WAVE, 3.0),
        ],
        1.0,
        TransportInfo {
            playing: false,
            ..playing()
        },
    );
    // A square LFO: one channel high while the other is low, once it's going.
    let opposite = l[20..]
        .iter()
        .zip(&r[20..])
        .filter(|(a, b)| (a.log2() + b.log2() - 2.0 * 1_000f32.log2()).abs() < 0.1)
        .count();
    assert!(opposite > l.len() / 2, "{opposite} of {}", l.len());

    let (l0, r0) = cutoffs(
        &[(LFO_AMOUNT, 100.0), (LFO_RATE, 2.0)],
        1.0,
        TransportInfo {
            playing: false,
            ..playing()
        },
    );
    assert_eq!(l0, r0, "at 0 deg both channels are the same");

    let (ls, rs) = cutoffs(
        &[
            (LFO_AMOUNT, 100.0),
            (LFO_RATE, 2.0),
            (LFO_STEREO, 1.0),
            (LFO_SPIN, 50.0),
        ],
        2.0,
        TransportInfo {
            playing: false,
            ..playing()
        },
    );
    let drifted = ls.iter().zip(&rs).any(|(a, b)| (a - b).abs() > 100.0);
    assert!(drifted, "spin separates them");
}

#[test]
fn auto_filter_lfo_steps_and_sample_and_hold_quantise_it() {
    use af::*;
    let (stepped, _) = cutoffs(
        &[
            (LFO_AMOUNT, 100.0),
            (LFO_RATE, 2.0),
            (LFO_QUANT, QUANT_STEPS as f64),
            (LFO_STEPS, 4.0),
        ],
        1.0,
        TransportInfo {
            playing: false,
            ..playing()
        },
    );
    // Four steps of a sine sit at 0, 1, 0 and -1: three levels, four octaves apart.
    let base = 1_000f32.log2();
    let on_a_level = stepped[20..]
        .iter()
        .filter(|c| {
            let oct = c.log2() - base;
            [0.0, LFO_OCTAVES, -LFO_OCTAVES]
                .iter()
                .any(|level| (oct - level).abs() < 0.05)
        })
        .count();
    let total = stepped.len() - 20;
    assert!(on_a_level > total * 9 / 10, "{on_a_level} of {total}");

    let (held, _) = cutoffs(
        &[
            (LFO_AMOUNT, 100.0),
            (LFO_RATE, 1.5),
            (LFO_QUANT, QUANT_SH as f64),
            (LFO_SH_RATE, 12.0),
        ],
        1.0,
        playing(),
    );
    // Quarter notes at 120 BPM: held for 0.5 s, then a jump as the 1.5 Hz sine is sampled at
    // its trough.
    near(held[40] as f64, 1_000.0, 2.0, "held at first");
    near(held[80] as f64, 1_000.0, 2.0, "still held");
    near(
        *held.last().unwrap() as f64,
        1_000.0 / 2f64.powf(LFO_OCTAVES as f64),
        2.0,
        "the trough, afterwards",
    );
}

#[test]
fn auto_filter_synced_lfo_follows_the_tempo_and_only_then_uses_phase_offset() {
    use af::*;
    // Quarter notes at 120 BPM: one cycle every 0.5 s, so 2 Hz.
    let (l, _) = cutoffs(
        &[(LFO_AMOUNT, 100.0), (LFO_MODE, 2.0), (LFO_BEATS, 12.0)],
        2.0,
        playing(),
    );
    let crossings = l
        .windows(2)
        .filter(|w| w[0] < 1_000.0 && w[1] >= 1_000.0)
        .count();
    assert_eq!(
        crossings, 3,
        "up-crossings in two seconds, the one at the very start excepted"
    );

    let offset_at = |mode: f64, offset: f64| {
        cutoffs(
            &[
                (LFO_AMOUNT, 100.0),
                (LFO_MODE, mode),
                (LFO_BEATS, 12.0),
                (LFO_OFFSET, offset),
            ],
            0.1,
            playing(),
        )
        .0[0]
    };
    assert!(
        (offset_at(2.0, 180.0) - offset_at(2.0, 0.0)).abs() > 10.0,
        "synced uses it"
    );
    assert_eq!(
        offset_at(0.0, 180.0),
        offset_at(0.0, 0.0),
        "free-running doesn't"
    );
}

/// The cutoff in Hz at each block, with an envelope on a signal.
fn envelope_run(
    params: &[(u32, f64)],
    input: &[f32],
    key: Option<&[f32]>,
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut p = af_with(params);
    let k = key.map(|k| (k, k));
    let (l, _, seen) = render_view(
        &mut p,
        input,
        input,
        k,
        af::CUTOFFS,
        2,
        af::VIEW_LEN,
        playing(),
    );
    (
        l,
        seen.iter().map(|v| v[0]).collect(),
        seen.iter().map(|v| v[1]).collect(),
    )
}

#[test]
fn auto_filter_envelope_moves_the_cutoff_by_its_amount_and_direction() {
    use af::*;
    let loud = vec![0.5f32; 24_000];
    let (_, up, _) = envelope_run(&[(ENV_AMOUNT, 100.0), (FREQ, 200.0)], &loud, None);
    near(
        *up.last().unwrap() as f64,
        200.0 * 2f64.powf(0.5 * ENV_OCTAVES as f64),
        60.0,
        "up",
    );
    let (_, down, _) = envelope_run(&[(ENV_AMOUNT, -50.0), (FREQ, 2_000.0)], &loud, None);
    near(
        *down.last().unwrap() as f64,
        2_000.0 * 2f64.powf(-0.5 * 0.5 * ENV_OCTAVES as f64),
        80.0,
        "down",
    );
}

#[test]
fn auto_filter_envelope_attack_slows_its_rise_and_release_its_fall() {
    use af::*;
    let mut signal = vec![0.0f32; 12_000];
    signal[1_000..7_000].fill(0.5);
    let (_, fast, _) = envelope_run(
        &[
            (ENV_AMOUNT, 100.0),
            (ENV_ATTACK, 0.1),
            (ENV_RELEASE, 10.0),
            (FREQ, 200.0),
        ],
        &signal,
        None,
    );
    let (_, slow, _) = envelope_run(
        &[
            (ENV_AMOUNT, 100.0),
            (ENV_ATTACK, 100.0),
            (ENV_RELEASE, 10.0),
            (FREQ, 200.0),
        ],
        &signal,
        None,
    );
    let block = 2_000 / BLOCK;
    assert!(
        fast[block] > slow[block] * 1.5,
        "{} {}",
        fast[block],
        slow[block]
    );
    let (_, long, _) = envelope_run(
        &[
            (ENV_AMOUNT, 100.0),
            (ENV_ATTACK, 0.1),
            (ENV_RELEASE, 500.0),
            (FREQ, 200.0),
        ],
        &signal,
        None,
    );
    let after = 8_500 / BLOCK;
    assert!(
        long[after] > fast[after] * 2.0,
        "{} {}",
        long[after],
        fast[after]
    );
}

#[test]
fn auto_filter_envelope_hold_finishes_the_attack_once_started() {
    use af::*;
    let mut burst = vec![0.0f32; 12_000];
    burst[1_000..1_100].fill(0.8);
    let run = |hold: f64| {
        let (_, c, _) = envelope_run(
            &[
                (ENV_AMOUNT, 100.0),
                (ENV_ATTACK, 20.0),
                (ENV_RELEASE, 2_000.0),
                (ENV_HOLD, hold),
                (FREQ, 100.0),
            ],
            &burst,
            None,
        );
        c.iter().cloned().fold(0.0, f32::max)
    };
    let (open, held) = (run(0.0), run(1.0));
    assert!(held > open * 2.0, "without {open}, with {held}");
}

#[test]
fn auto_filter_envelope_sample_and_hold_steps_at_its_beat_grid() {
    use af::*;
    let loud: Vec<f32> = (0..24_000)
        .map(|i| (i as f32 * 0.0005).sin().abs() * 0.5)
        .collect();
    let (_, plain, _) = envelope_run(&[(ENV_AMOUNT, 100.0), (FREQ, 200.0)], &loud, None);
    let (_, stepped, _) = envelope_run(
        &[
            (ENV_AMOUNT, 100.0),
            (FREQ, 200.0),
            (ENV_SH_ON, 1.0),
            (ENV_SH_RATE, 9.0),
        ],
        &loud,
        None,
    );
    let steps = |v: &[f32]| v.windows(2).filter(|w| (w[0] - w[1]).abs() > 0.5).count();
    assert!(
        steps(&stepped) < steps(&plain) / 2 + 1,
        "{} {}",
        steps(&stepped),
        steps(&plain)
    );
}

#[test]
fn auto_filter_sidechain_keys_from_the_external_signal_when_told_to() {
    use af::*;
    let silence = vec![0.0f32; 12_000];
    let key = vec![0.8f32; 12_000];
    let on = [(ENV_AMOUNT, 100.0), (FREQ, 200.0), (SC_EXTERNAL, 1.0)];
    let (_, external, _) = envelope_run(&on, &silence, Some(&key));
    assert!(*external.last().unwrap() > 2_000.0, "follows the key");

    let off = [(ENV_AMOUNT, 100.0), (FREQ, 200.0), (SC_EXTERNAL, 0.0)];
    let (_, internal, _) = envelope_run(&off, &silence, Some(&key));
    near(
        *internal.last().unwrap() as f64,
        200.0,
        2.0,
        "off ignores it",
    );

    let (_, without, _) = envelope_run(&on, &silence, None);
    near(
        *without.last().unwrap() as f64,
        200.0,
        2.0,
        "with no key routed, keys off the input",
    );

    // Mix: at 0 the internal signal is used only.
    let mixed = [
        (ENV_AMOUNT, 100.0),
        (FREQ, 200.0),
        (SC_EXTERNAL, 1.0),
        (SC_MIX, 0.0),
    ];
    let (_, internal_only, _) = envelope_run(&mixed, &silence, Some(&key));
    near(*internal_only.last().unwrap() as f64, 200.0, 2.0, "mix 0");
}

#[test]
fn auto_filter_sidechain_listen_plays_the_key_and_the_filter_shapes_it() {
    use af::*;
    let silence = vec![0.0f32; 4_096];
    let key = sine(500.0, SR, 4_096);
    let (listened, _, _) = envelope_run(
        &[(SC_EXTERNAL, 1.0), (SC_LISTEN, 1.0)],
        &silence,
        Some(&key),
    );
    near(peak(&listened) as f64, 1.0, 0.02, "the key is heard");

    let hum = sine(100.0, SR, 24_000);
    let run = |on: f64| {
        let (_, c, _) = envelope_run(
            &[
                (ENV_AMOUNT, 100.0),
                (FREQ, 100.0),
                (SC_EXTERNAL, 1.0),
                (SC_FILTER_ON, on),
                (SC_FILTER_TYPE, 3.0),
                (SC_FILTER_FREQ, 2_000.0),
                (SC_FILTER_Q, 0.71),
            ],
            &vec![0.0; 24_000],
            Some(&hum),
        );
        *c.last().unwrap()
    };
    assert!(
        run(0.0) > run(1.0) * 0.8,
        "the low-pass on the key keeps a low tone"
    );
    let highs = sine(8_000.0, SR, 24_000);
    let run_high = |on: f64| {
        let (_, c, _) = envelope_run(
            &[
                (ENV_AMOUNT, 100.0),
                (FREQ, 100.0),
                (SC_EXTERNAL, 1.0),
                (SC_FILTER_ON, on),
                (SC_FILTER_TYPE, 3.0),
                (SC_FILTER_FREQ, 300.0),
            ],
            &vec![0.0; 24_000],
            Some(&highs),
        );
        *c.last().unwrap()
    };
    assert!(run_high(0.0) > run_high(1.0) * 4.0, "and cuts a high one");
}

#[test]
fn auto_filter_mono_sidechain_decides_if_the_channels_share_a_level() {
    use af::*;
    let mut l = vec![0.0f32; 12_000];
    l[..12_000].fill(0.8);
    let r = vec![0.0f32; 12_000];
    let run = |mono: f64| {
        let mut p = af_with(&[(ENV_AMOUNT, 100.0), (FREQ, 200.0), (SC_MONO, mono)]);
        let (_, _, seen) = render_view(
            &mut p,
            &l,
            &r,
            None,
            af::CUTOFFS,
            2,
            af::VIEW_LEN,
            playing(),
        );
        seen.last().unwrap().clone()
    };
    let same = run(1.0);
    assert_eq!(
        same[0], same[1],
        "by default both channels share one envelope"
    );
    let separate = run(0.0);
    assert!(separate[0] > separate[1] * 2.0, "{separate:?}");
}

// --------------------------------------------------------------- Auto Pan-Tremolo

fn pan_with(params: &[(u32, f64)]) -> apt::AutoPanTremolo {
    let mut p = ready::<apt::AutoPanTremolo>();
    set(&mut p, params);
    p
}

/// The per-sample gains on a steady signal of 1.
fn pan_gains(params: &[(u32, f64)], seconds: f32) -> (Vec<f32>, Vec<f32>) {
    let frames = (SR as f32 * seconds) as usize;
    let input = vec![1.0f32; frames];
    let mut p = pan_with(params);
    render(&mut p, &input, &input)
}

/// As [`pan_gains`], with the song position moving on from block to block.
fn pan_gains_on_transport(params: &[(u32, f64)], seconds: f32) -> (Vec<f32>, Vec<f32>) {
    let frames = (SR as f32 * seconds) as usize;
    let input = vec![1.0f32; frames];
    let mut p = pan_with(params);
    let (l, r, _) = render_view(&mut p, &input, &input, None, apt::GAINS, 2, 2, playing());
    (l, r)
}

#[test]
fn auto_pan_panning_alternates_the_channels_at_its_rate() {
    use apt::*;
    let (l, r) = pan_gains(&[(AMOUNT, 100.0), (FREQ, 2.0)], 1.0);
    for (a, b) in l.iter().zip(&r).skip(400).step_by(1000) {
        near(
            (*a + *b) as f64,
            1.0,
            0.02,
            "the channels are complementary",
        );
    }
    let (max, min) = (
        l.iter().cloned().fold(0.0, f32::max),
        l.iter().cloned().fold(1.0, f32::min),
    );
    assert!(max > 0.98 && min < 0.02, "{max} {min}");
    near(cycles(&l) as f64, 2.0, 1.0, "cycles in a second");
}

/// How many rising crossings of the midpoint there are.
fn cycles(x: &[f32]) -> usize {
    let mid = (x.iter().cloned().fold(0.0, f32::max) + x.iter().cloned().fold(1.0, f32::min)) * 0.5;
    x.windows(2).filter(|w| w[0] < mid && w[1] >= mid).count()
}

#[test]
fn auto_pan_tremolo_follows_one_lfo_and_keeps_channels_together() {
    use apt::*;
    let (l, r) = pan_gains(&[(MODE, 1.0), (AMOUNT, 60.0), (FREQ, 3.0)], 1.0);
    assert_eq!(l, r);
    let (max, min) = (
        l.iter().cloned().fold(0.0, f32::max),
        l.iter().cloned().fold(1.0, f32::min),
    );
    near(max as f64, 1.0, 0.02, "peak level");
    near(min as f64, 0.4, 0.02, "trough");
    near(cycles(&l) as f64, 3.0, 1.0, "cycles in a second");
}

#[test]
fn auto_pan_time_modes_set_the_period() {
    use apt::*;
    let (l, _) = pan_gains(
        &[(MODE, 1.0), (TIME_MODE, TM_TIME as f64), (TIME, 250.0)],
        1.0,
    );
    near(cycles(&l) as f64, 4.0, 1.0, "250 ms");
    // At 120 BPM: 1/4 is 2 Hz, dotted 1.33, triplet 3, four sixteenths 2.
    let cases = [
        (TM_SYNCED, 2.0),
        (TM_DOTTED, 4.0 / 3.0),
        (TM_TRIPLET, 3.0),
        (TM_16TH, 2.0),
    ];
    for (mode, hz) in cases {
        let (l, _) = pan_gains_on_transport(
            &[
                (MODE, 1.0),
                (TIME_MODE, mode as f64),
                (SYNC, 4.0),
                (SIXTEENTHS, 4.0),
            ],
            3.0,
        );
        near(cycles(&l) as f64 / 3.0, hz, 0.4, &format!("mode {mode}"));
    }
}

#[test]
fn auto_pan_phase_offset_only_shifts_beat_division_modes() {
    use apt::*;
    // A quarter of the way through the 1/4-note cycle, where a sine is at its top.
    let at = |mode: usize, offset: f64| {
        pan_gains_on_transport(
            &[
                (MODE, 1.0),
                (TIME_MODE, mode as f64),
                (FREQ, 2.0),
                (TREM_BLOCK + W_OFFSET, offset),
                (AMOUNT, 100.0),
            ],
            0.2,
        )
        .0[6_000]
    };
    assert!((at(TM_SYNCED, 180.0) - at(TM_SYNCED, 0.0)).abs() > 0.2);
    assert_eq!(at(TM_RATE, 180.0), at(TM_RATE, 0.0));
}

#[test]
fn auto_pan_invert_turns_the_wave_over() {
    use apt::*;
    let normal = pan_gains(&[(MODE, 1.0), (AMOUNT, 100.0)], 0.5).0;
    let flipped = pan_gains(
        &[(MODE, 1.0), (AMOUNT, 100.0), (TREM_BLOCK + W_INVERT, 1.0)],
        0.5,
    )
    .0;
    for i in (500..24_000).step_by(2_000) {
        near((normal[i] + flipped[i]) as f64, 1.0, 0.05, "complementary");
    }
}

#[test]
fn auto_pan_stereo_offset_phase_and_spin() {
    use apt::*;
    let (l, r) = pan_gains(
        &[(MODE, 1.0), (AMOUNT, 100.0), (TREM_BLOCK + W_PHASE, 180.0)],
        1.0,
    );
    let opposite = l
        .iter()
        .zip(&r)
        .skip(500)
        .step_by(500)
        .all(|(a, b)| (a + b - 1.0).abs() < 0.05);
    assert!(opposite, "Phase 180 is anti-phase");

    let spin = [
        (MODE, 1.0),
        (AMOUNT, 100.0),
        (FREQ, 2.0),
        (TREM_BLOCK + W_STEREO, 1.0),
        (TREM_BLOCK + W_SPIN, 50.0),
    ];
    let (l, r) = pan_gains(&spin, 2.0);
    assert_ne!(cycles(&l), cycles(&r), "Spin runs them at different rates");
}

#[test]
fn auto_pan_shark_tooth_rises_fast_and_falls_back() {
    use apt::*;
    let (l, _) = pan_gains(
        &[
            (MODE, 1.0),
            (AMOUNT, 100.0),
            (FREQ, 4.0),
            (TREM_BLOCK + W_WAVE, 2.0),
        ],
        1.0,
    );
    let top = *l
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(_, v)| v)
        .unwrap();
    near(top as f64, 1.0, 0.03, "reaches the top");
    // Half a cycle after the trough, it's at the top already; a sine would still be climbing.
    let climbed = l[6_000 / 2];
    assert!(climbed > 0.5, "{climbed}");
}

#[test]
fn auto_pan_random_waves_use_phase_offset_for_width_and_ignore_spin() {
    use apt::*;
    let base = [
        (MODE, 1.0),
        (FREQ, 8.0),
        (AMOUNT, 100.0),
        (TREM_BLOCK + W_WAVE, 8.0),
    ];
    let (l, r) = pan_gains(&base, 1.0);
    assert_eq!(l, r, "no width by default");

    let mut wide = base.to_vec();
    wide.push((TREM_BLOCK + W_OFFSET, 360.0));
    let (lw, rw) = pan_gains(&wide, 1.0);
    assert!(
        lw.iter().zip(&rw).any(|(a, b)| (a - b).abs() > 0.05),
        "width separates them"
    );

    let mut spun = base.to_vec();
    spun.extend([(TREM_BLOCK + W_STEREO, 1.0), (TREM_BLOCK + W_SPIN, 50.0)]);
    assert_eq!(pan_gains(&spun, 1.0).0, l, "Spin does nothing to S&H");
}

#[test]
fn auto_pan_shape_rounds_or_sharpens_the_tremolo_and_pushes_the_panning() {
    use apt::*;
    let base = |shape: f64| {
        pan_gains(
            &[
                (MODE, 1.0),
                (AMOUNT, 100.0),
                (FREQ, 2.0),
                (TREM_BLOCK + W_SHAPE, shape),
            ],
            0.5,
        )
        .0
    };
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    let (flat, round, sharp) = (mean(&base(0.0)), mean(&base(70.0)), mean(&base(-70.0)));
    assert!(round > flat + 0.05, "{round} {flat}");
    assert!(sharp < flat - 0.05, "{sharp} {flat}");

    // At the maximum a sine is mostly a ramp over the cycle.
    let ramp = base(100.0);
    let cycle = 24_000usize;
    let start = 1_000;
    assert!(
        ramp[start] > ramp[start + cycle / 2 - 2_000],
        "{} {}",
        ramp[start],
        ramp[start + cycle / 2 - 2_000]
    );

    let panned = |shape: f64| {
        pan_gains(
            &[(AMOUNT, 100.0), (FREQ, 2.0), (PAN_BLOCK + W_SHAPE, shape)],
            0.5,
        )
        .0
    };
    let extreme = |v: &[f32]| v.iter().filter(|s| **s < 0.05 || **s > 0.95).count();
    assert!(
        extreme(&panned(100.0)) > extreme(&panned(0.0)) * 3,
        "the transition is abrupt"
    );
}

#[test]
fn auto_pan_harmonic_alternates_low_and_high_bands_at_600_hz() {
    use apt::*;
    let at = |hz: f32| {
        let input = sine(hz, SR, 96_000);
        let mut p = pan_with(&[(MODE, 1.0), (AMOUNT, 100.0), (FREQ, 2.0), (HARMONIC, 1.0)]);
        render(&mut p, &input, &input).0
    };
    let envelope = |signal: &[f32], index: usize| rms(&signal[index..index + 1_000]);
    let (low, high) = (at(100.0), at(4_000.0));
    let quarter = 48_000 / 8;
    // Sine at 2 Hz: its positive peak and trough, at 0.125 and 0.375 s.
    near(
        envelope(&low, quarter) as f64,
        0.707,
        0.1,
        "low band at the LFO top",
    );
    assert!(
        envelope(&high, quarter) < 0.15,
        "high band at the same moment"
    );
    near(
        envelope(&high, quarter * 3) as f64,
        0.707,
        0.1,
        "high band at the bottom",
    );
    assert!(
        envelope(&low, quarter * 3) < 0.15,
        "low band at the same moment"
    );
}

#[test]
fn auto_pan_vintage_shapes_the_signal_and_the_curve() {
    use apt::*;
    let input: Vec<f32> = sine(1_000.0, SR, 48_000).iter().map(|s| s * 0.9).collect();
    let run = |vintage: f64| {
        let mut p = pan_with(&[(MODE, 1.0), (AMOUNT, 0.0), (VINTAGE, vintage)]);
        render(&mut p, &input, &input).0
    };
    assert!(tone_amplitude(&run(0.0), 3_000.0, SR) < 1e-3, "clean");
    assert!(tone_amplitude(&run(1.0), 3_000.0, SR) > 0.01, "harmonics");
}

#[test]
fn auto_pan_attack_leaves_a_transient_unmodulated_at_first() {
    use apt::*;
    let mut signal = vec![0.0f32; 48_000];
    signal[10_000..].fill(0.8);
    let mut p = pan_with(&[
        (MODE, 1.0),
        (AMOUNT, 100.0),
        (FREQ, 1.0),
        (ATTACK, 200.0),
        (TREM_BLOCK + W_INVERT, 1.0),
    ]);
    let (l, _) = render(&mut p, &signal, &signal);
    // Just after the transient: modulation is held off, so the level is the input level.
    near(l[10_100] as f64, 0.8, 0.02, "transient level");
    // Long after it, the LFO has taken over.
    let later = l[10_000 + 19_200];
    assert!((later - 0.8).abs() > 0.1, "{later}");
}

#[test]
fn auto_pan_dynamic_frequency_speeds_up_with_level() {
    use apt::*;
    let frames = 48_000 * 2;
    let run = |dynamic: f64, level: f32| {
        let input = vec![level; frames];
        let mut p = pan_with(&[
            (MODE, 1.0),
            (AMOUNT, 100.0),
            (FREQ, 4.0),
            (DYNAMIC_FREQ, dynamic),
        ]);
        cycles(&render(&mut p, &input, &input).0) as f64 / 2.0
    };
    near(run(0.0, 0.9), 4.0, 0.5, "no effect at 0");
    assert!(
        run(100.0, 0.9) > 6.0,
        "loud goes faster: {}",
        run(100.0, 0.9)
    );
    assert!(
        run(-100.0, 0.9) < 3.0,
        "negative goes slower: {}",
        run(-100.0, 0.9)
    );
}

// ----------------------------------------------------------------------- Utility

#[test]
fn utility_channel_mode_takes_one_channel() {
    use util::*;
    let (l, r) = (vec![0.5f32; 4_800], vec![-0.25f32; 4_800]);
    let run = |mode: f64| {
        let mut p = ready::<Utility>();
        p.set_param(CHANNEL_MODE, mode);
        render(&mut p, &l, &r)
    };
    let (a, b) = run(1.0);
    assert_eq!(a[4_000], 0.5);
    assert_eq!(b[4_000], 0.5, "left on both");
    let (a, b) = run(2.0);
    assert_eq!(a[4_000], -0.25);
    assert_eq!(b[4_000], -0.25, "right on both");
    let (a, b) = run(3.0);
    assert_eq!((a[4_000], b[4_000]), (-0.25, 0.5), "swapped");
}

#[test]
fn utility_width_sets_the_stereo_image() {
    use util::*;
    let (l, r) = (vec![0.5f32; 4_800], vec![-0.25f32; 4_800]);
    let run = |width: f64| {
        let mut p = ready::<Utility>();
        p.set_param(WIDTH, width);
        let (a, b) = render(&mut p, &l, &r);
        (a[4_000], b[4_000])
    };
    let (a, b) = run(0.0);
    near(a as f64, 0.125, 1e-6, "mono");
    near(b as f64, 0.125, 1e-6, "mono");
    let (a, b) = run(4.0);
    near((a - b) as f64, 4.0 * 0.75, 1e-5, "wider");
}

#[test]
fn utility_mid_side_control_goes_from_mono_to_only_side() {
    use util::*;
    let (l, r) = (vec![0.5f32; 4_800], vec![-0.25f32; 4_800]);
    let run = |amount: f64| {
        let mut p = ready::<Utility>();
        p.set_param(MID_SIDE_MODE, 1.0);
        p.set_param(MID_SIDE, amount);
        let (a, b) = render(&mut p, &l, &r);
        (a[4_000], b[4_000])
    };
    let (a, b) = run(0.0);
    near(a as f64, 0.5, 1e-5, "0 leaves the signal alone");
    near(b as f64, -0.25, 1e-5, "");
    let (a, b) = run(-100.0);
    near(a as f64, 0.125, 1e-5, "100M sums to mono");
    near(b as f64, 0.125, 1e-5, "");
    let (a, b) = run(100.0);
    near(a as f64, 0.375, 1e-5, "100S is only the side");
    near(b as f64, -0.375, 1e-5, "with the channels out of phase");
}

#[test]
fn utility_phase_buttons_invert_each_channel() {
    use util::*;
    let tone = sine(1_000.0, SR, 24_000);
    let mut p = ready::<Utility>();
    set(&mut p, &[(INVERT_L, 1.0)]);
    let (l, r) = render(&mut p, &tone, &tone);
    // The inversion is a gain change, so it eases in over a few time constants.
    for i in (12_000..24_000).step_by(137) {
        near(l[i] as f64, -tone[i] as f64, 1e-4, "left inverted");
        near(r[i] as f64, tone[i] as f64, 1e-4, "right untouched");
    }
}

#[test]
fn utility_dc_filter_removes_an_offset_and_keeps_audio() {
    use util::*;
    let input = vec![0.5f32; 96_000];
    let mut p = ready::<Utility>();
    p.set_param(DC, 1.0);
    let (l, _) = render(&mut p, &input, &input);
    assert!(l[90_000].abs() < 0.01, "{}", l[90_000]);
    let mut p = ready::<Utility>();
    p.set_param(DC, 1.0);
    near(tone_gain_db(&mut p, 200.0), 0.0, 0.05, "audio passes");
}

#[test]
fn utility_bass_mono_narrows_the_lows_only() {
    use util::*;
    let tone = |hz: f32| sine(hz, SR, 24_000);
    let wide = |hz: f32| {
        let l = tone(hz);
        let r: Vec<f32> = l.iter().map(|s| -s).collect();
        (l, r)
    };
    let (l, r) = wide(40.0);
    let mut p = ready::<Utility>();
    set(&mut p, &[(BASS_MONO, 1.0), (BASS_FREQ, 200.0)]);
    let (ol, _) = render(&mut p, &l, &r);
    assert!(measure(&ol, 40.0) < -30.0, "a wide low tone cancels");
    let (l, r) = wide(4_000.0);
    let mut p = ready::<Utility>();
    set(&mut p, &[(BASS_MONO, 1.0), (BASS_FREQ, 200.0)]);
    let (ol, or) = render(&mut p, &l, &r);
    near(measure(&ol, 4_000.0), 0.0, 0.2, "a wide high tone survives");
    near(measure(&or, 4_000.0), 0.0, 0.2, "");
}

#[test]
fn utility_bass_audition_hears_only_the_lows() {
    use util::*;
    let run = |hz: f32| {
        let input = sine(hz, SR, 24_000);
        let mut p = ready::<Utility>();
        set(
            &mut p,
            &[(BASS_MONO, 1.0), (BASS_FREQ, 200.0), (BASS_AUDITION, 1.0)],
        );
        measure(&render(&mut p, &input, &input).0, hz as f64)
    };
    near(run(40.0), 0.0, 0.2, "low passes");
    assert!(run(4_000.0) < -60.0, "high is cut");
}

#[test]
fn utility_gain_reaches_plus_35_and_silence() {
    use util::*;
    let mut p = ready::<Utility>();
    p.set_param(GAIN, 35.0);
    let input: Vec<f32> = sine(440.0, SR, 24_000).iter().map(|s| s * 0.001).collect();
    let (l, _) = render(&mut p, &input, &input);
    near(measure(&l, 440.0), -60.0 + 35.0, 0.05, "+35");
    let mut p = ready::<Utility>();
    p.set_param(GAIN, rlive_devices::devices::b4::common::SILENCE_DB);
    assert!(tone_gain_db(&mut p, 440.0) < -100.0, "-inf");
}

#[test]
fn utility_balance_and_mute_do_what_they_say() {
    use util::*;
    let input = vec![0.5f32; 4_800];
    let mut p = ready::<Utility>();
    p.set_param(PAN, 1.0);
    let (l, r) = render(&mut p, &input, &input);
    assert!(l[4_000].abs() < 1e-3, "full right silences left");
    assert!((r[4_000] - 0.5).abs() < 1e-3);
    let mut p = ready::<Utility>();
    p.set_param(MUTE, 1.0);
    let (l, _) = render(&mut p, &input, &input);
    assert!(l[4_000].abs() < 1e-3, "muted");
}

// ----------------------------------------------------- allocation and extremes

#[test]
fn b4_devices_stay_finite_and_never_allocate_at_extreme_settings() {
    let (hot_l, hot_r) = (noise(24_000, 7), noise(24_000, 8));
    let hot_l: Vec<f32> = hot_l.iter().map(|s| s * 5.0).collect();
    let hot_r: Vec<f32> = hot_r.iter().map(|s| s * 5.0).collect();
    for name in [
        "rlive.eq3",
        "rlive.utility",
        "rlive.eq_eight",
        "rlive.channel_eq",
        "rlive.auto_filter",
        "rlive.auto_pan_tremolo",
    ] {
        let info = rlive_devices::find(name).expect("registered");
        let desc = (info.desc)();
        for (pick, salt) in [(0usize, 0u32), (1, 1), (2, 2), (3, 3)] {
            let mut p = (info.create)(&rlive_devices::DeviceInit {
                samples: Vec::new(),
                state: None,
            });
            p.activate(SR, BLOCK);
            // Set every parameter to one of its extremes.
            for (i, param) in desc.params.iter().enumerate() {
                let hi = !(i as u32 + salt).is_multiple_of(pick as u32 + 2);
                let value = if hi { param.max } else { param.min };
                p.set_param(param.id, value);
            }
            let mut h = Harness::new(SR);
            let (mut l, mut r) = (hot_l.clone(), hot_r.clone());
            for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
                assert_no_alloc(|| h.process(&mut *p, cl, cr, &[], &[]));
            }
            assert!(
                l.iter().chain(&r).all(|s| s.is_finite()),
                "{name} went non-finite at setting {pick}"
            );
            p.reset();
            let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
            h.process(&mut *p, &mut l, &mut r, &[], &[]);
            assert!(
                peak(&l) < 1e-6 && peak(&r) < 1e-6,
                "{name} rang on after reset"
            );
        }
    }
}

#[test]
fn a_parameter_change_at_an_offset_lands_on_its_frame() {
    use apt::*;
    let mut p = pan_with(&[(MODE, 1.0), (AMOUNT, 0.0)]);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.5f32; 512], vec![0.5f32; 512]);
    let change = rlive_core::ParamEvent {
        offset: 100,
        id: rlive_core::ParamId(AMOUNT),
        value: 100.0,
    };
    h.process(&mut p, &mut l, &mut r, &[], &[change]);
    assert_eq!(l[99], 0.5, "no modulation before the change");
    assert!(l[160] < 0.5, "and afterwards");
}

#[test]
fn eq_eight_filter_coefficients_ease_rather_than_jump() {
    let mut p = eq8_with(&[(A, 3, PEAK, 1_000.0, 0.0, 1.0)], &[]);
    let tone = sine(1_000.0, SR, 1_024);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (tone.clone(), tone.clone());
    let change = rlive_core::ParamEvent {
        offset: 64,
        id: rlive_core::ParamId(eq8::band_param(A, 3, eq8::B_GAIN)),
        value: 15.0,
    };
    h.process(&mut p, &mut l, &mut r, &[], &[change]);
    // The largest step between samples is hardly more than the tone's own.
    let steps = |v: &[f32]| v.windows(2).fold(0.0f32, |m, w| m.max((w[0] - w[1]).abs()));
    assert!(
        steps(&l) < steps(&tone) * 3.0,
        "{} {}",
        steps(&l),
        steps(&tone)
    );
}
