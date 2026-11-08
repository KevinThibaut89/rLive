//! Batch B5: the distortion devices, against what their maths and the manual say should happen.

use rlive_core::{
    AudioBuffer, AudioInput, EventQueue, MidiEvent, MidiMessage, ParamEvent, ParamId,
    ParamOutQueue, ProcessContext, Processor, TransportInfo, ViewWriter,
};
use rlive_devices::devices::b5::{
    amp, cabinet, dynamic_tube as tube, erosion, overdrive, pedal, redux, roar,
    vinyl_distortion as vinyl,
};
use rlive_devices::dsp::soft_clip;
use rlive_devices::effects::{sat, Saturator};
use rlive_devices::{builtins, find, DeviceInit};
use rlive_testkit::{assert_no_alloc, onset, peak, rms, sine, tone_amplitude, Harness};

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

/// Runs mono material through and hands back the left channel.
fn mono(p: &mut dyn Processor, input: &[f32]) -> Vec<f32> {
    render(p, input, input).0
}

fn db(linear: f64) -> f64 {
    20.0 * linear.abs().max(1e-12).log10()
}

fn amp_of(signal: &[f32], hz: f64) -> f64 {
    let window = signal.len() / 2;
    let cycles = (window as f64 * hz / SR).round().max(1.0);
    let frames = ((cycles * SR / hz).round() as usize).min(signal.len());
    tone_amplitude(&signal[signal.len() - frames..], hz, SR)
}

fn tone(hz: f32, amp: f32, frames: usize) -> Vec<f32> {
    sine(hz, SR, frames).into_iter().map(|s| s * amp).collect()
}

fn noise(frames: usize, seed: u32) -> Vec<f32> {
    let mut rng = rlive_devices::dsp::util::Rng::new(seed);
    (0..frames).map(|_| rng.bipolar() * 0.8).collect()
}

fn near(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} != {expected} (±{tol})"
    );
}

const LONG: usize = 48_000;

// ------------------------------------------------------------------------- Saturator

/// The algorithm from before this batch, copied here so the check can't drift with the code.
fn original_saturator(input: &[f32], drive_db: f32, out_db: f32, mix: f32) -> Vec<f32> {
    let drive = rlive_core::db_to_gain(drive_db);
    let out = rlive_core::db_to_gain(out_db);
    input
        .iter()
        .map(|s| {
            let wet = soft_clip(*s * drive) * out;
            *s * (1.0 - mix) + wet * mix
        })
        .collect()
}

#[test]
fn saturators_default_path_is_the_original_sample_for_sample() {
    let input = noise(LONG, 7);
    // At default values, including one with drive and mix moved: only appended params differ.
    let mut p: Saturator = ready();
    let got = mono(&mut p, &input);
    assert_eq!(got, original_saturator(&input, 6.0, -3.0, 1.0));

    let mut p: Saturator = ready();
    set(
        &mut p,
        &[(sat::DRIVE, 24.0), (sat::OUTPUT, -9.0), (sat::MIX, 0.4)],
    );
    let got = mono(&mut p, &input);
    assert_eq!(got, original_saturator(&input, 24.0, -9.0, 0.4));
}

#[test]
fn saturators_appended_params_default_to_the_original_path() {
    for p in rlive_devices::effects::saturator_params() {
        if p.id > sat::MIX {
            let mut s: Saturator = ready();
            s.set_param(p.id, p.default);
            let input = noise(2_048, 3);
            assert_eq!(
                mono(&mut s, &input),
                original_saturator(&input, 6.0, -3.0, 1.0),
                "{}",
                p.name
            );
        }
    }
}

#[test]
fn saturators_curves_match_their_shapes() {
    let curve = |kind: usize, threshold_db: f32| sat::CurveShape {
        kind,
        threshold: rlive_core::db_to_gain(threshold_db),
        drive: 1.0,
        curve: 0.5,
        depth: 0.0,
        lin: 0.5,
        damp: 0.0,
        period: 0.0,
    };
    // Digital Clip is linear up to full scale and flat after it.
    let digital = curve(6, 0.0);
    assert_eq!(digital.apply(0.5), 0.5);
    assert_eq!(digital.apply(1.5), 1.0);
    // Soft Sine reaches full scale exactly at 1.
    let soft = curve(1, 0.0);
    near(soft.apply(1.0) as f64, 1.0, 1e-6, "soft sine at 1");
    near(
        soft.apply(0.5) as f64,
        (0.25 * std::f64::consts::PI).sin() * 1.0,
        1e-3,
        "soft sine at 0.5",
    );
    // Sinoid Fold folds back.
    let fold = curve(5, 0.0);
    assert!(fold.apply(1.5) < fold.apply(1.0), "folds over");
    // Medium and Hard saturate to 1 but differently: Hard has a sharper knee at full scale.
    let (medium, hard) = (curve(3, 0.0), curve(4, 0.0));
    assert!(hard.apply(1.0) > medium.apply(1.0));
    assert!((medium.apply(20.0) - 1.0).abs() < 0.01);
    assert!((hard.apply(20.0) - 1.0).abs() < 0.01);
    // Bass Shaper: linear below the threshold, soft clip above.
    let low = curve(2, -24.0);
    let t = rlive_core::db_to_gain(-24.0);
    assert_eq!(low.apply(t * 0.5), t * 0.5, "linear below");
    let above = low.apply(0.5);
    assert!(above > t && above < 0.5, "starts clipping above: {above}");
    // A 0 dB threshold is a hard clip at full scale.
    let high = curve(2, 0.0);
    assert_eq!(high.apply(0.99), 0.99);
    assert_eq!(high.apply(1.2), 1.0);
}

#[test]
fn saturators_waveshaper_controls_do_what_the_manual_says() {
    let run = |params: &[(u32, f64)]| {
        let mut p: Saturator = ready();
        set(
            &mut p,
            &[(sat::CURVE, 7.0), (sat::OUTPUT, 0.0), (sat::DRIVE, 0.0)],
        );
        set(&mut p, params);
        mono(&mut p, &tone(220.0, 0.8, LONG))
    };
    // At 0% Drive the waveshaper leaves the curve straight.
    let flat = run(&[(sat::WS_DRIVE, 0.0)]);
    near(amp_of(&flat, 220.0), 0.8, 1e-3, "no drive is linear");
    near(amp_of(&flat, 660.0), 0.0, 1e-4, "no drive adds nothing");
    // Curve adds mostly third-order harmonics.
    let low = run(&[(sat::WS_CURVE, 0.1), (sat::WS_LIN, 0.5)]);
    let high = run(&[(sat::WS_CURVE, 1.0), (sat::WS_LIN, 0.5)]);
    assert!(amp_of(&high, 660.0) > 2.0 * amp_of(&low, 660.0));
    // Depth superimposes a sine, Period packs its ripples closer.
    let rippled = run(&[(sat::WS_DEPTH, 0.5), (sat::WS_PERIOD, 0.5)]);
    assert!(amp_of(&rippled, 1100.0) > amp_of(&run(&[]), 1100.0) * 1.5);
    // Damp silences what's near zero, like a very fast gate.
    let damped = {
        let mut p: Saturator = ready();
        set(
            &mut p,
            &[
                (sat::CURVE, 7.0),
                (sat::OUTPUT, 0.0),
                (sat::DRIVE, 0.0),
                (sat::WS_DAMP, 1.0),
            ],
        );
        mono(&mut p, &tone(220.0, 0.02, LONG))
    };
    assert!(
        peak(&damped) < 0.005,
        "quiet signal gated away: {}",
        peak(&damped)
    );
}

#[test]
fn saturators_post_clip_never_exceeds_output() {
    for (mode, name) in [(1.0, "soft"), (2.0, "hard")] {
        let mut p: Saturator = ready();
        set(
            &mut p,
            &[
                (sat::POST_CLIP, mode),
                (sat::CURVE, 7.0),
                (sat::WS_DEPTH, 1.0),
                (sat::OUTPUT, -6.0),
                (sat::DRIVE, 36.0),
            ],
        );
        let out = mono(&mut p, &noise(LONG, 5));
        assert!(
            peak(&out) <= rlive_core::db_to_gain(-6.0) + 1e-3,
            "{name}: {}",
            peak(&out)
        );
    }
}

#[test]
fn saturators_color_filters_mirror_each_other() {
    // A curve that's nearly linear at low level, so the mirrored EQ should cancel.
    let mut p: Saturator = ready();
    set(
        &mut p,
        &[
            (sat::COLOR, 1.0),
            (sat::AMT_LO, -1.0),
            (sat::AMT_HI, 1.0),
            (sat::FREQ, 1_000.0),
            (sat::DRIVE, -24.0),
            (sat::OUTPUT, 0.0),
            (sat::CURVE, 6.0),
        ],
    );
    for hz in [80.0, 1_000.0, 8_000.0] {
        let out = mono(&mut p, &tone(hz as f32, 0.5, LONG));
        near(
            db(amp_of(&out, hz)),
            db(0.5 * rlive_core::db_to_gain(-24.0) as f64),
            0.3,
            "mirrored",
        );
    }
}

#[test]
fn saturators_color_takes_saturation_away_from_the_bass_at_negative_amt_lo() {
    let h3 = |params: &[(u32, f64)]| {
        let mut p: Saturator = ready();
        set(
            &mut p,
            &[(sat::COLOR, 1.0), (sat::DRIVE, 12.0), (sat::OUTPUT, 0.0)],
        );
        set(&mut p, params);
        let out = mono(&mut p, &tone(80.0, 0.5, LONG));
        amp_of(&out, 240.0)
    };
    assert!(h3(&[(sat::AMT_LO, -1.0)]) < 0.5 * h3(&[(sat::AMT_LO, 0.0)]));
}

#[test]
fn saturators_pre_dc_filter_removes_a_dc_offset() {
    let mut p: Saturator = ready();
    set(
        &mut p,
        &[(sat::PRE_DC, 1.0), (sat::OUTPUT, 0.0), (sat::DRIVE, 0.0)],
    );
    let mut input = tone(1_000.0, 0.3, LONG);
    for s in &mut input {
        *s += 0.3;
    }
    let out = mono(&mut p, &input);
    let tail = &out[LONG / 2..];
    let mean: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
    assert!(mean.abs() < 2e-3, "mean {mean}");
}

#[test]
fn hi_quality_reports_latency_and_keeps_the_dry_signal_in_step() {
    let mut p: Saturator = ready();
    set(&mut p, &[(sat::HI_QUALITY, 1.0), (sat::MIX, 0.0)]);
    let lat = p.latency() as usize;
    assert!(lat > 0 && lat < 512, "{lat}");
    let input = noise(LONG / 2, 9);
    let out = mono(&mut p, &input);
    for (a, b) in out[lat..].iter().zip(&input) {
        assert!((a - b).abs() < 1e-6, "dry only and delayed");
    }
}

// ------------------------------------------------------------------------ Oversampling

/// The mean level at the frequencies where the harmonics of a 5.2 kHz tone fold back when they
/// pass Nyquist: nothing belongs at those frequencies, so whatever's there came from aliasing.
fn alias_level(p: &mut dyn Processor) -> f64 {
    const F0: f64 = 5_200.0;
    let out = mono(p, &tone(F0 as f32, 0.7, LONG));
    let (mut total, mut points) = (0.0, 0);
    for k in 2..40 {
        let f = k as f64 * F0;
        if f < SR * 0.5 {
            continue;
        }
        let folded = (f + SR * 0.5).rem_euclid(SR) - SR * 0.5;
        let folded = folded.abs();
        let nearest = (folded / F0).round() * F0;
        if folded > 200.0 && folded < 20_000.0 && (folded - nearest).abs() > 100.0 {
            total += amp_of(&out, folded);
            points += 1;
        }
    }
    total / points.max(1) as f64
}

#[test]
fn hi_quality_rejects_aliasing_on_every_device_that_has_it() {
    let ratio = |lo: f64, hi: f64| db(lo) - db(hi);

    let (mut a, mut b): (Saturator, Saturator) = (ready(), ready());
    let shared = [(sat::DRIVE, 12.0), (sat::OUTPUT, -3.0)];
    set(&mut a, &shared);
    set(&mut b, &shared);
    set(&mut b, &[(sat::HI_QUALITY, 1.0)]);
    let (lo, hi) = (alias_level(&mut a), alias_level(&mut b));
    assert!(ratio(lo, hi) > 20.0, "Saturator {lo} vs {hi}");

    let (mut a, mut b): (tube::DynamicTube, tube::DynamicTube) = (ready(), ready());
    let shared = [(tube::TUBE, 2.0), (tube::DRIVE, 6.0)];
    set(&mut a, &shared);
    set(&mut b, &shared);
    set(&mut b, &[(tube::HI_QUALITY, 1.0)]);
    let (lo, hi) = (alias_level(&mut a), alias_level(&mut b));
    assert!(ratio(lo, hi) > 20.0, "Dynamic Tube {lo} vs {hi}");

    let (mut a, mut b): (pedal::Pedal, pedal::Pedal) = (ready(), ready());
    let shared = [(pedal::TYPE, 0.0), (pedal::GAIN, 0.0)];
    set(&mut a, &shared);
    set(&mut b, &shared);
    set(&mut b, &[(pedal::HI_QUALITY, 1.0)]);
    let (lo, hi) = (alias_level(&mut a), alias_level(&mut b));
    assert!(ratio(lo, hi) > 20.0, "Pedal {lo} vs {hi}");

    let (mut a, mut b): (amp::Amp, amp::Amp) = (ready(), ready());
    let shared = [(amp::MODEL, 0.0), (amp::GAIN, 0.8)];
    set(&mut a, &shared);
    set(&mut b, &shared);
    set(&mut b, &[(amp::HI_QUALITY, 1.0)]);
    let (lo, hi) = (alias_level(&mut a), alias_level(&mut b));
    assert!(ratio(lo, hi) > 12.0, "Amp {lo} vs {hi}");
}

// ------------------------------------------------------------------------- Harmonics

fn harmonics(out: &[f32], f0: f64) -> (f64, f64, f64) {
    (
        amp_of(out, f0),
        amp_of(out, 2.0 * f0),
        amp_of(out, 3.0 * f0),
    )
}

#[test]
fn symmetric_shaping_has_no_even_harmonics_and_asymmetric_does() {
    let mut p: Saturator = ready();
    set(&mut p, &[(sat::DRIVE, 12.0), (sat::CURVE, 6.0)]);
    let (_, h2, h3) = harmonics(&mono(&mut p, &tone(440.0, 0.5, LONG)), 440.0);
    assert!(h3 > 0.02, "odd harmonic present: {h3}");
    assert!(h2 < 1e-4, "no even harmonics: {h2}");

    let mut p: tube::DynamicTube = ready();
    set(
        &mut p,
        &[(tube::TUBE, 2.0), (tube::DRIVE, 0.0), (tube::BIAS, 0.0)],
    );
    let (_, h2, _) = harmonics(&mono(&mut p, &tone(440.0, 0.5, LONG)), 440.0);
    assert!(h2 > 0.02, "asymmetric shaping makes even harmonics: {h2}");
}

// ------------------------------------------------------------------------- Dynamic Tube

#[test]
fn tube_a_is_clean_when_quiet_and_tube_c_never_is() {
    let thd = |t: f64| {
        let mut p: tube::DynamicTube = ready();
        set(
            &mut p,
            &[(tube::TUBE, t), (tube::BIAS, 0.0), (tube::DRIVE, 0.0)],
        );
        let (h1, h2, h3) = harmonics(&mono(&mut p, &tone(440.0, 0.2, LONG)), 440.0);
        (h2 + h3) / h1
    };
    let (a, b, c) = (thd(0.0), thd(1.0), thd(2.0));
    assert!(a < 1e-3, "Tube A is linear below its threshold: {a}");
    assert!(c > 0.02, "Tube C distorts all the time: {c}");
    assert!(b > a && b < c, "B is between: {a} {b} {c}");
}

#[test]
fn tube_a_starts_distorting_above_its_threshold() {
    let mut p: tube::DynamicTube = ready();
    set(
        &mut p,
        &[(tube::TUBE, 0.0), (tube::BIAS, 0.0), (tube::DRIVE, 12.0)],
    );
    let (h1, _, h3) = harmonics(&mono(&mut p, &tone(440.0, 0.5, LONG)), 440.0);
    assert!(h3 / h1 > 0.05, "{h3} {h1}");
}

#[test]
fn tube_envelope_makes_loud_signals_dirtier_when_positive_and_cleaner_when_negative() {
    let thd = |env: f64| {
        let mut p: tube::DynamicTube = ready();
        set(
            &mut p,
            &[
                (tube::TUBE, 0.0),
                (tube::BIAS, 0.3),
                (tube::DRIVE, 6.0),
                (tube::ENVELOPE, env),
                (tube::ATTACK, 1.0),
            ],
        );
        let (h1, _, h3) = harmonics(&mono(&mut p, &tone(440.0, 0.5, LONG)), 440.0);
        h3 / h1
    };
    assert!(thd(1.0) > thd(0.0) * 1.2, "{} {}", thd(1.0), thd(0.0));
    assert!(thd(-1.0) < thd(0.0) * 0.9, "{} {}", thd(-1.0), thd(0.0));
}

#[test]
fn tube_attack_and_release_have_no_effect_with_envelope_at_zero() {
    let render_it = |a: f64, r: f64| {
        let mut p: tube::DynamicTube = ready();
        set(&mut p, &[(tube::ATTACK, a), (tube::RELEASE, r)]);
        mono(&mut p, &noise(8_000, 1))
    };
    assert_eq!(render_it(0.1, 10.0), render_it(100.0, 1_000.0));
}

#[test]
fn tube_tone_tilts_the_distortion_towards_the_highs() {
    let high_low = |tone_value: f64| {
        let mut p: tube::DynamicTube = ready();
        set(
            &mut p,
            &[
                (tube::TUBE, 2.0),
                (tube::DRIVE, 18.0),
                (tube::TONE, tone_value),
            ],
        );
        let out = mono(&mut p, &tone(220.0, 0.5, LONG));
        amp_of(&out, 2_200.0) / amp_of(&out, 660.0)
    };
    assert!(high_low(1.0) > high_low(-1.0) * 2.0);
}

#[test]
fn tube_bias_at_the_extreme_breaks_the_signal_apart() {
    let out_rms = |bias: f64| {
        let mut p: tube::DynamicTube = ready();
        set(
            &mut p,
            &[(tube::TUBE, 0.0), (tube::BIAS, bias), (tube::DRIVE, 12.0)],
        );
        rms(&mono(&mut p, &tone(440.0, 0.5, LONG))[LONG / 2..])
    };
    assert!(
        out_rms(1.0) < out_rms(0.0),
        "{} {}",
        out_rms(1.0),
        out_rms(0.0)
    );
}

// ------------------------------------------------------------------------- Overdrive

#[test]
fn overdrive_at_zero_percent_still_distorts_and_tone_lifts_the_top() {
    let mut p: overdrive::Overdrive = ready();
    set(&mut p, &[(overdrive::DRIVE, 0.0)]);
    let (h1, _, h3) = harmonics(&mono(&mut p, &tone(440.0, 0.8, LONG)), 440.0);
    assert!(h3 / h1 > 0.05, "0% still distorts: {}", h3 / h1);

    let high = |t: f64| {
        let mut p: overdrive::Overdrive = ready();
        set(&mut p, &[(overdrive::TONE, t)]);
        amp_of(&mono(&mut p, &tone(440.0, 0.8, LONG)), 3_960.0)
    };
    assert!(high(1.0) > high(0.0) * 2.0, "{} {}", high(1.0), high(0.0));
}

#[test]
fn overdrive_filter_keeps_out_what_is_outside_its_band() {
    let run = |hz: f32| {
        let mut p: overdrive::Overdrive = ready();
        set(
            &mut p,
            &[
                (overdrive::FREQ, 1_000.0),
                (overdrive::WIDTH, 1.0),
                (overdrive::DRIVE, 0.0),
            ],
        );
        amp_of(&mono(&mut p, &tone(hz, 0.5, LONG)), hz as f64)
    };
    assert!(
        run(1_000.0) > run(60.0) * 6.0,
        "{} {}",
        run(1_000.0),
        run(60.0)
    );
}

#[test]
fn overdrives_dynamics_controls_how_much_compression_goes_in() {
    let level = |dyn_v: f64, amp_v: f32| {
        let mut p: overdrive::Overdrive = ready();
        set(
            &mut p,
            &[(overdrive::DRIVE, 1.0), (overdrive::DYNAMICS, dyn_v)],
        );
        rms(&mono(&mut p, &tone(440.0, amp_v, LONG))[LONG / 2..])
    };
    let squashed = level(0.0, 0.8) / level(0.0, 0.2);
    let dynamic = level(1.0, 0.8) / level(1.0, 0.2);
    assert!(
        dynamic > squashed,
        "more dynamics keeps more contrast: {dynamic} {squashed}"
    );
}

#[test]
fn overdrive_dry_wet_passes_dry_exactly() {
    let mut p: overdrive::Overdrive = ready();
    set(&mut p, &[(overdrive::MIX, 0.0)]);
    let input = noise(4_096, 2);
    assert_eq!(mono(&mut p, &input), input);
}

// ------------------------------------------------------------------------- Redux

#[test]
fn redux_quantizer_matches_the_arithmetic() {
    // Bits 4 has 8 levels each side: 0.3 lands on round(2.4) / 8.
    assert_eq!(redux::quantize(0.3, 4.0, 0.0, false), 0.25);
    assert_eq!(redux::quantize(0.0, 4.0, 0.0, false), 0.0);
    // DC Shift offsets by half a step before quantizing, so zero no longer stays zero.
    assert!(redux::quantize(0.0, 4.0, 0.0, true) > 0.0);
}

#[test]
fn redux_shape_gives_small_values_finer_steps() {
    // A small value has more error at Shape 0 than at Shape 1.
    let err = |shape: f32| (redux::quantize(0.02, 4.0, shape, false) - 0.02).abs();
    assert!(err(1.0) < err(0.0) * 0.5, "{} {}", err(1.0), err(0.0));
    // A large value needn't be better.
    let e_small = (redux::quantize(0.9, 4.0, 1.0, false) - 0.9).abs();
    assert!(e_small > 0.0);
}

#[test]
fn redux_bits_reduce_to_the_expected_step_count() {
    let mut p: redux::Redux = ready();
    set(&mut p, &[(redux::BITS, 3.0)]);
    let ramp: Vec<f32> = (0..LONG)
        .map(|i| (i as f32 / LONG as f32) * 2.0 - 1.0)
        .collect();
    let out = mono(&mut p, &ramp);
    let mut levels: Vec<i32> = out.iter().map(|s| (s * 1_000.0).round() as i32).collect();
    levels.sort_unstable();
    levels.dedup();
    assert_eq!(
        levels.len(),
        9,
        "2 * 2^(3-1) + 1 rounded levels: {levels:?}"
    );
}

#[test]
fn redux_rate_holds_for_the_right_length() {
    let mut p: redux::Redux = ready();
    set(&mut p, &[(redux::RATE, 4_800.0)]);
    let out = mono(&mut p, &noise(4_800, 11));
    let mut runs = Vec::new();
    let mut count = 1usize;
    for w in out.windows(2) {
        if w[0] == w[1] {
            count += 1;
        } else {
            runs.push(count);
            count = 1;
        }
    }
    let middle: Vec<_> = runs[2..runs.len() - 2].to_vec();
    assert!(
        middle.iter().all(|r| *r == 10),
        "{:?}",
        &middle[..20.min(middle.len())]
    );
}

#[test]
fn redux_post_filter_reduces_imaging_and_octave_moves_it() {
    let image = |post: f64, oct: f64| {
        let mut p: redux::Redux = ready();
        set(
            &mut p,
            &[
                (redux::RATE, 4_000.0),
                (redux::POST, post),
                (redux::OCTAVE, oct),
            ],
        );
        amp_of(&mono(&mut p, &tone(1_000.0, 0.5, LONG)), 3_000.0)
    };
    let (no, yes) = (image(0.0, 0.0), image(1.0, -1.0));
    assert!(no > yes * 4.0, "{no} {yes}");
}

#[test]
fn redux_pre_filter_stops_aliasing_into_the_baseband() {
    let image = |pre: f64| {
        let mut p: redux::Redux = ready();
        set(&mut p, &[(redux::RATE, 4_000.0), (redux::PRE, pre)]);
        // 3.1 kHz folds to 900 Hz at a 4 kHz rate.
        amp_of(&mono(&mut p, &tone(3_100.0, 0.5, LONG)), 900.0)
    };
    assert!(
        image(0.0) > image(1.0) * 2.0,
        "{} {}",
        image(0.0),
        image(1.0)
    );
}

#[test]
fn redux_jitter_adds_stereo_width() {
    let width = |j: f64| {
        let mut p: redux::Redux = ready();
        set(&mut p, &[(redux::RATE, 4_000.0), (redux::JITTER, j)]);
        let input = tone(1_000.0, 0.5, LONG);
        let (l, r) = render(&mut p, &input, &input);
        rms(&l.iter().zip(&r).map(|(a, b)| a - b).collect::<Vec<_>>())
    };
    assert_eq!(width(0.0), 0.0, "no jitter is mono");
    assert!(width(1.0) > 0.05);
}

#[test]
fn redux_dc_shift_leaves_no_dc_but_changes_the_sound() {
    let run = |dc: f64| {
        let mut p: redux::Redux = ready();
        set(&mut p, &[(redux::BITS, 3.0), (redux::DC_SHIFT, dc)]);
        mono(&mut p, &tone(440.0, 0.5, LONG))
    };
    let (off, on) = (run(0.0), run(1.0));
    let mean: f32 = on[LONG / 2..].iter().sum::<f32>() / (LONG / 2) as f32;
    assert!(mean.abs() < 0.01, "{mean}");
    assert_ne!(off, on);
}

// ------------------------------------------------------------------------- Erosion

#[test]
fn erosion_at_zero_amount_is_an_exact_delay_of_its_latency() {
    let mut p: erosion::Erosion = ready();
    let lat = p.latency() as usize;
    let input = noise(8_000, 4);
    let out = mono(&mut p, &input);
    assert_eq!(&out[lat..], &input[..input.len() - lat]);
}

#[test]
fn erosion_noise_blend_moves_from_sine_to_noise() {
    let spread = |blend: f64| {
        let mut p: erosion::Erosion = ready();
        set(
            &mut p,
            &[
                (erosion::AMOUNT, 0.6),
                (erosion::NOISE_BLEND, blend),
                (erosion::FREQ, 2_000.0),
            ],
        );
        let out = mono(&mut p, &tone(440.0, 0.5, LONG));
        // Sine modulation makes sharp sidebands at carrier ± mod; noise spreads energy instead.
        (amp_of(&out, 2_440.0), rms(&out[LONG / 2..]))
    };
    let (sine_side, _) = spread(0.0);
    let (noise_side, _) = spread(1.0);
    assert!(sine_side > noise_side * 3.0, "{sine_side} {noise_side}");
}

#[test]
fn erosion_filter_width_has_an_audible_effect_only_on_noise() {
    let run = |blend: f64, width: f64| {
        let mut p: erosion::Erosion = ready();
        set(
            &mut p,
            &[
                (erosion::AMOUNT, 0.8),
                (erosion::NOISE_BLEND, blend),
                (erosion::FILTER_WIDTH, width),
                (erosion::FREQ, 3_000.0),
            ],
        );
        mono(&mut p, &tone(440.0, 0.5, LONG))
    };
    assert_eq!(run(0.0, 0.1), run(0.0, 0.9), "sine-only is unaffected");
    assert_ne!(run(1.0, 0.1), run(1.0, 0.9));
}

#[test]
fn erosion_stereo_width_decorrelates_the_channels() {
    let diff = |w: f64| {
        let mut p: erosion::Erosion = ready();
        set(
            &mut p,
            &[
                (erosion::AMOUNT, 0.8),
                (erosion::NOISE_BLEND, 1.0),
                (erosion::STEREO_WIDTH, w),
            ],
        );
        let input = tone(440.0, 0.5, LONG);
        let (l, r) = render(&mut p, &input, &input);
        rms(&l.iter().zip(&r).map(|(a, b)| a - b).collect::<Vec<_>>())
    };
    assert!(diff(0.0) < 1e-3, "{}", diff(0.0));
    assert!(diff(1.0) > 0.03, "{}", diff(1.0));
}

// ------------------------------------------------------------------------- Vinyl

#[test]
fn vinyl_is_transparent_when_nothing_is_turned_up() {
    let mut p: vinyl::VinylDistortion = ready();
    let input = noise(8_000, 6);
    assert_eq!(mono(&mut p, &input), input);
}

#[test]
fn vinyl_tracing_is_even_and_pinch_is_odd() {
    let run = |id: u32| {
        let mut p: vinyl::VinylDistortion = ready();
        set(&mut p, &[(id, 1.0)]);
        harmonics(&mono(&mut p, &tone(880.0, 0.4, LONG)), 880.0)
    };
    let (_, t2, t3) = run(vinyl::TRACING_DRIVE);
    assert!(t2 > t3 * 8.0, "tracing: {t2} {t3}");
    let (_, p2, p3) = run(vinyl::PINCH_DRIVE);
    assert!(p3 > p2 * 8.0, "pinch: {p2} {p3}");
}

#[test]
fn vinyl_pinch_stereo_is_out_of_phase_and_mono_is_not() {
    let run = |stereo: f64| {
        let mut p: vinyl::VinylDistortion = ready();
        set(
            &mut p,
            &[
                (vinyl::PINCH_DRIVE, 1.0),
                (vinyl::PINCH_FREQ, 2_000.0),
                (vinyl::PINCH_STEREO, stereo),
            ],
        );
        let input = tone(2_000.0, 0.4, LONG);
        let (l, r) = render(&mut p, &input, &input);
        // The tracing path is off, so the difference from the dry input is all pinch.
        let side: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        rms(&side)
    };
    assert_eq!(run(0.0), 0.0, "mono pinch stays mono");
    assert!(run(1.0) > 0.03, "{}", run(1.0));
}

#[test]
fn vinyl_modes_differ_and_drive_scales_the_distortion() {
    let h2 = |mode: f64, drive: f64| {
        let mut p: vinyl::VinylDistortion = ready();
        set(
            &mut p,
            &[
                (vinyl::TRACING_DRIVE, 1.0),
                (vinyl::MODE, mode),
                (vinyl::DRIVE, drive),
            ],
        );
        amp_of(&mono(&mut p, &tone(880.0, 0.8, LONG)), 1_760.0)
    };
    assert_ne!(h2(0.0, 0.0), h2(1.0, 0.0));
    assert!(h2(0.0, 12.0) > h2(0.0, -12.0) * 2.0);
}

#[test]
fn vinyl_crackle_density_and_volume_control_the_noise() {
    let level = |density: f64, vol: f64| {
        let mut p: vinyl::VinylDistortion = ready();
        set(
            &mut p,
            &[
                (vinyl::CRACKLE_DENSITY, density),
                (vinyl::CRACKLE_VOLUME, vol),
            ],
        );
        let (l, _) = render(&mut p, &vec![0.0; LONG], &vec![0.0; LONG]);
        rms(&l)
    };
    assert_eq!(level(0.0, -6.0), 0.0, "no density, no crackle");
    assert!(level(1.0, -6.0) > level(0.2, -6.0));
    assert!(level(0.5, -6.0) > level(0.5, -30.0) * 3.0);
}

// ------------------------------------------------------------------------- Pedal

/// A small-signal response: small enough that the curve is close to linear.
fn pedal_gain_db(params: &[(u32, f64)], hz: f64) -> f64 {
    let mut p: pedal::Pedal = ready();
    set(&mut p, params);
    db(amp_of(&mono(&mut p, &tone(hz as f32, 0.004, LONG)), hz))
}

#[test]
fn pedal_eq_matches_the_manuals_frequencies() {
    let base = |hz| pedal_gain_db(&[], hz);
    // Bass peaks at 100 Hz.
    let bass = pedal_gain_db(&[(pedal::BASS, 1.0)], 100.0) - base(100.0);
    near(
        bass,
        pedal::BASS_RANGE_DB as f64,
        1.5,
        "Bass peak at 100 Hz",
    );
    let beside = pedal_gain_db(&[(pedal::BASS, 1.0)], 1_000.0) - base(1_000.0);
    assert!(beside.abs() < 0.5, "and leaves 1 kHz alone: {beside}");
    // Mid has three centres.
    for (slot, hz) in [(0.0, 500.0), (1.0, 1_000.0), (2.0, 2_000.0)] {
        let boosted = pedal_gain_db(&[(pedal::MID, 1.0), (pedal::MID_FREQ, slot)], hz);
        near(
            boosted - base(hz),
            pedal::MID_RANGE_DB as f64,
            1.5,
            "Mid at its centre",
        );
    }
    // Treble lifts above 3.3 kHz and leaves the lows.
    let up_high = pedal_gain_db(&[(pedal::TREBLE, 1.0)], 10_000.0) - base(10_000.0);
    near(up_high, pedal::TREBLE_RANGE_DB as f64, 1.5, "Treble shelf");
    let up_low = pedal_gain_db(&[(pedal::TREBLE, 1.0)], 200.0) - base(200.0);
    assert!(up_low.abs() < 0.5, "{up_low}");
    // Sub lifts below 250 Hz.
    let sub = pedal_gain_db(&[(pedal::SUB, 1.0)], 60.0) - base(60.0);
    near(sub, pedal::SUB_DB as f64, 1.5, "Sub shelf");
}

#[test]
fn pedal_mid_is_narrower_at_the_low_setting_and_wider_at_the_high() {
    // A fixed offset from the centre, as a ratio: how much of the boost is left there.
    let skirt = |slot: f64, hz: f64| {
        pedal_gain_db(&[(pedal::MID, 1.0), (pedal::MID_FREQ, slot)], hz * 0.6)
            - pedal_gain_db(&[(pedal::MID_FREQ, slot)], hz * 0.6)
    };
    assert!(
        skirt(0.0, 500.0) < skirt(2.0, 2_000.0),
        "{} {}",
        skirt(0.0, 500.0),
        skirt(2.0, 2_000.0)
    );
}

#[test]
fn pedal_adaptive_q_rises_with_boost() {
    assert!(pedal::adaptive_q(0.7, 1.0) > pedal::adaptive_q(0.7, 0.3));
    assert_eq!(pedal::adaptive_q(0.7, -1.0), 0.7);
}

#[test]
fn pedal_gain_zero_still_distorts_and_output_is_only_a_level() {
    let mut p: pedal::Pedal = ready();
    let out = mono(&mut p, &tone(220.0, 0.8, LONG));
    assert!(amp_of(&out, 660.0) / amp_of(&out, 220.0) > 0.02);
    let (mut a, mut b): (pedal::Pedal, pedal::Pedal) = (ready(), ready());
    set(&mut b, &[(pedal::OUTPUT, -12.0)]);
    let input = noise(LONG, 8);
    let (x, y) = (mono(&mut a, &input), mono(&mut b, &input));
    near(
        rms(&y) as f64 / rms(&x) as f64,
        rlive_core::db_to_gain(-12.0) as f64,
        0.01,
        "Output scales",
    );
}

#[test]
fn pedal_types_are_three_different_sounds() {
    let spectrum = |kind: f64| {
        let mut p: pedal::Pedal = ready();
        set(&mut p, &[(pedal::TYPE, kind), (pedal::GAIN, 0.0)]);
        let out = mono(&mut p, &tone(220.0, 0.3, LONG));
        [
            amp_of(&out, 220.0),
            amp_of(&out, 660.0),
            amp_of(&out, 1_100.0),
        ]
    };
    let (od, dist, fuzz) = (spectrum(0.0), spectrum(1.0), spectrum(2.0));
    for (a, b) in [(od, dist), (dist, fuzz), (od, fuzz)] {
        assert!((a[1] / a[0] - b[1] / b[0]).abs() > 0.01, "{a:?} {b:?}");
    }
}

#[test]
fn pedal_hi_quality_keeps_dry_in_step() {
    let mut p: pedal::Pedal = ready();
    set(&mut p, &[(pedal::HI_QUALITY, 1.0), (pedal::MIX, 0.0)]);
    let lat = p.latency() as usize;
    assert!(lat > 0);
    let input = noise(8_000, 3);
    let out = mono(&mut p, &input);
    for (a, b) in out[lat..].iter().zip(&input) {
        assert!((a - b).abs() < 1e-6);
    }
}

// ------------------------------------------------------------------------- Amp

#[test]
fn amp_models_are_all_different_and_gain_adds_distortion() {
    let reading = |model: f64, gain: f64| {
        let mut p: amp::Amp = ready();
        set(&mut p, &[(amp::MODEL, model), (amp::GAIN, gain)]);
        let (h1, h2, h3) = harmonics(&mono(&mut p, &tone(220.0, 0.3, LONG)), 220.0);
        (h1, h2 / h1, h3 / h1)
    };
    for m in 0..7 {
        let (_, _, low) = reading(m as f64, 0.0);
        let (_, _, high) = reading(m as f64, 1.0);
        assert!(high > low, "model {m}: {low} -> {high}");
        for other in m + 1..7 {
            assert_ne!(
                reading(m as f64, 0.5),
                reading(other as f64, 0.5),
                "{m} {other}"
            );
        }
    }
}

#[test]
fn amps_volume_also_distorts_on_blues_heavy_and_bass() {
    let thd = |model: f64, vol: f64| {
        let mut p: amp::Amp = ready();
        set(
            &mut p,
            &[(amp::MODEL, model), (amp::GAIN, 0.0), (amp::VOLUME, vol)],
        );
        let (h1, _, h3) = harmonics(&mono(&mut p, &tone(220.0, 0.1, LONG)), 220.0);
        h3 / h1
    };
    for model in [2.0, 5.0, 6.0] {
        assert!(thd(model, 1.0) > thd(model, 0.3) * 1.3, "model {model}");
    }
    // Clean has far more headroom than the three that distort.
    let extra = |model: f64| thd(model, 1.0) - thd(model, 0.3);
    assert!(
        extra(2.0) > extra(0.0) * 1.3,
        "{} {}",
        extra(2.0),
        extra(0.0)
    );
}

#[test]
fn amp_stack_takes_bass_and_mid_away_as_treble_goes_up() {
    let (b0, m0, t0) = amp::stack_gains(0.5, 0.5, 0.5);
    assert_eq!((b0, m0, t0), (0.0, 0.0, 0.0));
    let (b, m, t) = amp::stack_gains(0.5, 0.5, 1.0);
    assert!(t > 0.0 && b < 0.0 && m < 0.0, "{b} {m} {t}");
    // Treble up, measured through the device: lower bass.
    let low = |treble: f64| {
        let mut p: amp::Amp = ready();
        set(&mut p, &[(amp::GAIN, 0.0), (amp::TREBLE, treble)]);
        amp_of(&mono(&mut p, &tone(120.0, 0.02, LONG)), 120.0)
    };
    assert!(low(1.0) < low(0.5) * 0.9, "{} {}", low(1.0), low(0.5));
}

#[test]
fn amp_bass_mid_treble_move_their_bands() {
    let band = |id: u32, value: f64, hz: f64| {
        let mut p: amp::Amp = ready();
        set(&mut p, &[(amp::GAIN, 0.0), (id, value)]);
        amp_of(&mono(&mut p, &tone(hz as f32, 0.02, LONG)), hz)
    };
    assert!(band(amp::BASS, 1.0, 80.0) > band(amp::BASS, 0.0, 80.0) * 2.0);
    assert!(band(amp::MIDDLE, 1.0, 800.0) > band(amp::MIDDLE, 0.0, 800.0) * 2.0);
    assert!(band(amp::TREBLE, 1.0, 8_000.0) > band(amp::TREBLE, 0.0, 8_000.0) * 2.0);
    assert!(band(amp::PRESENCE, 1.0, 8_000.0) > band(amp::PRESENCE, 0.0, 8_000.0) * 1.5);
}

#[test]
fn amp_mono_sums_and_dual_keeps_channels_apart() {
    let left = tone(220.0, 0.3, LONG);
    let right = tone(330.0, 0.3, LONG);
    let run = |output: f64| {
        let mut p: amp::Amp = ready();
        set(&mut p, &[(amp::OUTPUT, output)]);
        render(&mut p, &left, &right)
    };
    let (ml, mr) = run(0.0);
    assert_eq!(ml, mr, "mono output");
    let (dl, dr) = run(1.0);
    assert!(amp_of(&dl, 220.0) > amp_of(&dl, 330.0) * 3.0);
    assert!(amp_of(&dr, 330.0) > amp_of(&dr, 220.0) * 3.0);
}

#[test]
fn amp_dry_wet_passes_dry_at_zero() {
    let mut p: amp::Amp = ready();
    set(&mut p, &[(amp::MIX, 0.0)]);
    let input = noise(8_000, 10);
    assert_eq!(mono(&mut p, &input), input);
}

// ------------------------------------------------------------------------- Cabinet

fn cab_response(speaker: f64, mic: f64, mic_type: f64) -> Vec<f64> {
    let mut p: cabinet::Cabinet = ready();
    set(
        &mut p,
        &[
            (cabinet::SPEAKER, speaker),
            (cabinet::MICROPHONE, mic),
            (cabinet::MIC_TYPE, mic_type),
        ],
    );
    let input = noise(LONG, 20);
    let (l, _) = render(&mut p, &input, &input);
    [100.0, 400.0, 1_500.0, 3_000.0, 6_000.0, 10_000.0]
        .iter()
        .map(|hz| db(amp_of(&l, *hz)))
        .collect()
}

fn cab_difference(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
}

#[test]
fn cabinet_response_differs_by_speaker_mic_and_mic_type() {
    let base = cab_response(2.0, 0.0, 0.0);
    for speaker in [0.0, 1.0, 3.0, 4.0] {
        let d = cab_difference(&cab_response(speaker, 0.0, 0.0), &base);
        assert!(d > 1.0, "speaker {speaker}: {d} dB");
    }
    for mic in [1.0, 2.0] {
        let d = cab_difference(&cab_response(2.0, mic, 0.0), &base);
        assert!(d > 1.0, "mic {mic}: {d} dB");
    }
    let d = cab_difference(&cab_response(2.0, 0.0, 1.0), &base);
    assert!(d > 0.5, "mic type: {d} dB");
}

#[test]
fn cabinet_near_on_axis_is_brighter_than_off_axis() {
    let bright = |mic: f64| {
        let tone_at = |hz: f32| {
            let mut p: cabinet::Cabinet = ready();
            set(&mut p, &[(cabinet::MICROPHONE, mic)]);
            db(amp_of(&mono(&mut p, &tone(hz, 0.2, LONG)), hz as f64))
        };
        tone_at(6_000.0) - tone_at(500.0)
    };
    assert!(
        bright(0.0) > bright(1.0) + 1.0,
        "{} {}",
        bright(0.0),
        bright(1.0)
    );
}

#[test]
fn cabinet_is_only_as_late_as_it_says() {
    let mut p: cabinet::Cabinet = ready();
    let lat = p.latency() as usize;
    let mut click = vec![0.0f32; 4_096];
    click[10] = 1.0;
    let (l, _) = render(&mut p, &click, &click);
    assert_eq!(onset(&l, 1e-3), Some(10 + lat));
}

#[test]
fn cabinet_mono_sums_and_dual_keeps_channels_apart() {
    let (l, r) = (tone(220.0, 0.3, LONG), tone(1_000.0, 0.3, LONG));
    let run = |output: f64| {
        let mut p: cabinet::Cabinet = ready();
        set(&mut p, &[(cabinet::OUTPUT, output)]);
        render(&mut p, &l, &r)
    };
    let (ml, mr) = run(0.0);
    assert_eq!(ml, mr);
    let (dl, dr) = run(1.0);
    assert!(amp_of(&dl, 220.0) > amp_of(&dl, 1_000.0) * 2.0);
    assert!(amp_of(&dr, 1_000.0) > amp_of(&dr, 220.0) * 2.0);
}

#[test]
fn cabinet_dry_wet_at_zero_is_the_dry_signal_in_step() {
    let mut p: cabinet::Cabinet = ready();
    set(&mut p, &[(cabinet::MIX, 0.0)]);
    let lat = p.latency() as usize;
    let input = noise(8_000, 30);
    let out = mono(&mut p, &input);
    for (a, b) in out[lat..].iter().zip(&input) {
        assert!((a - b).abs() < 1e-6, "{a} {b}");
    }
}

#[test]
fn switching_cabinets_mid_block_doesnt_allocate_or_step() {
    let mut p: cabinet::Cabinet = ready();
    let mut h = Harness::new(SR);
    let input = tone(220.0, 0.5, BLOCK);
    let mut prev = 0.0f32;
    let mut biggest = 0.0f32;
    for b in 0..80 {
        let mut l = input.clone();
        let mut r = input.clone();
        let change = if b % 13 == 7 {
            Some(ParamEvent {
                offset: 30,
                id: ParamId(cabinet::SPEAKER),
                value: (b % 5) as f64,
            })
        } else {
            None
        };
        let events: &[ParamEvent] = match &change {
            Some(c) => std::slice::from_ref(c),
            None => &[],
        };
        assert_no_alloc(|| h.process(&mut p, &mut l, &mut r, &[], events));
        if b > 2 {
            for s in &l {
                biggest = biggest.max((s - prev).abs());
                prev = *s;
            }
        } else {
            prev = *l.last().unwrap();
        }
    }
    assert!(biggest < 0.5, "no step: {biggest}");
}

// ------------------------------------------------------------------------- Roar

fn roar_default() -> roar::Roar {
    ready()
}

fn stage(s: usize, offset: u32) -> u32 {
    roar::stage_param(s, offset)
}

#[test]
fn roar_defaults_are_transparent_and_latency_is_true() {
    let mut p = roar_default();
    let lat = p.latency() as usize;
    assert!(lat > 0 && lat < 512, "{lat}");
    let input = tone(440.0, 0.5, LONG);
    let out = mono(&mut p, &input);
    let (a, b) = (&out[lat + 4_000..], &input[4_000..input.len() - lat]);
    let err = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(err < 2e-3, "{err}");
}

#[test]
fn roar_shapers_each_do_what_the_manual_says() {
    // Full wave doubles the frequency.
    let run = |kind: f64, amount: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (stage(0, roar::STAGE_SHAPER), kind),
                (stage(0, roar::STAGE_AMOUNT), amount),
            ],
        );
        mono(&mut p, &tone(220.0, 0.5, LONG))
    };
    let full = run(6.0, 0.5);
    assert!(
        amp_of(&full, 440.0) > amp_of(&full, 220.0) * 3.0,
        "full wave"
    );
    let half = run(5.0, 0.15);
    assert!(amp_of(&half, 440.0) > 0.05, "half wave has even harmonics");
    // Digital Clip makes the odd series.
    let clip = run(1.0, 1.0);
    assert!(amp_of(&clip, 660.0) > 0.05);
    assert!(amp_of(&clip, 440.0) < 0.05 * amp_of(&clip, 660.0).max(1e-9) + 0.02);
    // Bit Crusher sounds different from everything else, and Fractal and Tri Fold are bright.
    let fractal = run(8.0, 0.7);
    let soft = run(0.0, 0.7);
    assert!(amp_of(&fractal, 5_500.0) > amp_of(&soft, 5_500.0) * 2.0);
    let fold = run(9.0, 0.8);
    assert!(amp_of(&fold, 5_500.0) > amp_of(&soft, 5_500.0) * 2.0);
    // Noise Injection adds something inharmonic.
    let noisy = run(10.0, 0.6);
    assert_ne!(noisy, run(0.0, 0.6));
    // Shards keeps moving: not the same a block later.
    let shard = run(11.0, 0.8);
    let (a, b) = (&shard[10_000..11_000], &shard[20_000..21_000]);
    assert_ne!(a, b);
    // Every shaper is finite at full.
    for kind in 0..12 {
        let out = run(kind as f64, 1.0);
        assert!(out.iter().all(|s| s.is_finite()), "shaper {kind}");
    }
}

#[test]
fn roar_bias_is_asymmetric_and_goes_quiet_at_extremes() {
    let run = |bias: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (stage(0, roar::STAGE_AMOUNT), 0.2),
                (stage(0, roar::STAGE_BIAS), bias),
                (stage(0, roar::STAGE_SHAPER), 1.0),
            ],
        );
        mono(&mut p, &tone(220.0, 0.5, LONG))
    };
    let biased = run(0.5);
    assert!(
        amp_of(&biased, 440.0) > amp_of(&run(0.0), 440.0) * 3.0,
        "adds even"
    );
    assert!(
        rms(&run(1.0)[LONG / 2..]) < rms(&run(0.0)[LONG / 2..]) * 0.5,
        "quiets"
    );
}

#[test]
fn roar_serial_blend_goes_from_stage_one_to_both() {
    let render_with = |routing: f64, blend: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::ROUTING, routing),
                (roar::BLEND, blend),
                (stage(1, roar::STAGE_ON), 1.0),
                (stage(1, roar::STAGE_AMOUNT), 0.3),
                (stage(1, roar::STAGE_SHAPER), 6.0),
            ],
        );
        mono(&mut p, &tone(220.0, 0.5, LONG))
    };
    // At Blend 0 stage 1 alone, which is the identity by default.
    let none = render_with(1.0, 0.0);
    let both = render_with(1.0, 1.0);
    assert!(amp_of(&none, 440.0) < 0.05, "{}", amp_of(&none, 440.0));
    assert!(amp_of(&both, 440.0) > 0.1, "{}", amp_of(&both, 440.0));
}

#[test]
fn roar_parallel_blend_crossfades_between_the_stages() {
    let render_with = |blend: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::ROUTING, 2.0),
                (roar::BLEND, blend),
                (stage(0, roar::STAGE_AMOUNT), 0.8),
                (stage(0, roar::STAGE_SHAPER), 6.0),
                (stage(1, roar::STAGE_ON), 1.0),
                (stage(1, roar::STAGE_AMOUNT), 0.8),
                (stage(1, roar::STAGE_SHAPER), 5.0),
                // Stays under the hard clip, which isn't linear.
                (roar::OUTPUT, -12.0),
            ],
        );
        mono(&mut p, &tone(220.0, 0.5, LONG))
    };
    assert_ne!(render_with(0.0), render_with(1.0));
    let mid = render_with(0.5);
    let (a, b) = (render_with(0.0), render_with(1.0));
    let expected: Vec<f32> = a.iter().zip(&b).map(|(x, y)| 0.5 * (x + y)).collect();
    let tail = LONG / 2;
    let diff = mid[tail..]
        .iter()
        .zip(&expected[tail..])
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(diff < 0.02, "{diff}");
}

#[test]
fn roar_multiband_processes_each_band_separately() {
    let render_with = |band: usize| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::ROUTING, 3.0),
                (roar::XOVER_LO, 300.0),
                (roar::XOVER_HI, 3_000.0),
                (stage(1, roar::STAGE_ON), 1.0),
                (stage(2, roar::STAGE_ON), 1.0),
                (stage(band, roar::STAGE_AMOUNT), 1.0),
                (stage(band, roar::STAGE_SHAPER), 1.0),
            ],
        );
        mono(&mut p, &tone(1_000.0, 0.5, LONG))
    };
    // A 1 kHz tone is in the middle band: only stage 2's drive should clip it.
    assert!(amp_of(&render_with(1), 3_000.0) > 0.05, "mid band distorts");
    assert!(
        amp_of(&render_with(0), 3_000.0) < 0.05,
        "low band leaves it alone"
    );
    assert!(
        amp_of(&render_with(2), 3_000.0) < 0.05,
        "high band leaves it alone"
    );
}

#[test]
fn roar_multiband_with_amount_zero_is_flat_across_the_crossover() {
    let mut p = roar_default();
    set(
        &mut p,
        &[
            (roar::ROUTING, 3.0),
            (roar::XOVER_LO, 300.0),
            (roar::XOVER_HI, 3_000.0),
        ],
    );
    for hz in [100.0, 300.0, 1_000.0, 3_000.0, 10_000.0] {
        let out = mono(&mut p, &tone(hz as f32, 0.2, LONG));
        near(amp_of(&out, hz), 0.2, 0.01, "flat");
    }
}

#[test]
fn roar_mid_side_processes_the_two_independently() {
    let run = |amount_mid: f64, amount_side: f64, left: &[f32], right: &[f32]| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::ROUTING, 4.0),
                (stage(0, roar::STAGE_AMOUNT), amount_mid),
                (stage(0, roar::STAGE_SHAPER), 1.0),
                (stage(1, roar::STAGE_ON), 1.0),
                (stage(1, roar::STAGE_AMOUNT), amount_side),
                (stage(1, roar::STAGE_SHAPER), 1.0),
            ],
        );
        render(&mut p, left, right)
    };
    // A mono input has no side: the side stage can't change it.
    let input = tone(220.0, 0.5, LONG);
    let (l, r) = run(0.0, 1.0, &input, &input);
    assert_eq!(l, r, "still mono");
    assert!(
        amp_of(&l, 660.0) < 0.01,
        "side's distortion didn't touch the mid"
    );
    // And a mid-only amount distorts a mono input.
    let (l, _) = run(1.0, 0.0, &input, &input);
    assert!(amp_of(&l, 660.0) > 0.05);
}

#[test]
fn roar_feedback_time_mode_echoes_at_its_delay() {
    let mut p = roar_default();
    set(
        &mut p,
        &[
            (roar::FB_MODE, 0.0),
            (roar::FB_AMOUNT, 0.5),
            (roar::FB_TIME, 50.0),
            (roar::FB_WIDTH, 8.0),
        ],
    );
    let lat = p.latency() as usize;
    let mut click = vec![0.0f32; LONG];
    click[1_000] = 1.0;
    let (l, _) = render(&mut p, &click, &click);
    let at = 1_000 + lat + 2_400;
    let found = l[at - 400..at + 400]
        .iter()
        .map(|s| s.abs())
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i + at - 400)
        .unwrap();
    assert!(found.abs_diff(at) < 40, "echo near {at}: {found}");
    assert!(l[found].abs() > 0.05, "echo level {}", l[found]);
}

#[test]
fn roar_feedback_note_mode_rings_at_the_note() {
    let spectrum = |note: f64, hz: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::FB_MODE, 4.0),
                (roar::FB_AMOUNT, 0.97),
                (roar::FB_NOTE, note),
            ],
        );
        let input = noise(LONG, 40);
        let out = mono(&mut p, &input);
        amp_of(&out[LONG / 2..], hz)
    };
    // MIDI note 69 is 440 Hz: more there than at a frequency away from its harmonics.
    let at_note = spectrum(69.0, 440.0);
    let away = spectrum(69.0, 620.0);
    assert!(at_note > away * 2.0, "{at_note} {away}");
}

#[test]
fn roar_feedback_invert_flips_the_echoes() {
    let echo = |invert: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::FB_MODE, 0.0),
                (roar::FB_AMOUNT, 0.5),
                (roar::FB_TIME, 40.0),
                (roar::FB_INVERT, invert),
            ],
        );
        let lat = p.latency() as usize;
        let mut input = vec![0.0f32; LONG];
        for s in input[1_000..1_400].iter_mut() {
            *s = 0.5;
        }
        let out = mono(&mut p, &input);
        out[1_000 + lat + 1_920 + 200]
    };
    let (normal, inverted) = (echo(0.0), echo(1.0));
    assert!(normal * inverted < 0.0, "{normal} {inverted}");
}

#[test]
fn roar_feedback_gate_fades_when_the_input_stops() {
    let tail = |gate: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::FB_MODE, 0.0),
                (roar::FB_AMOUNT, 0.95),
                (roar::FB_TIME, 30.0),
                (roar::FB_GATE, gate),
            ],
        );
        let mut input = noise(LONG, 41);
        for s in input[LONG / 4..].iter_mut() {
            *s = 0.0;
        }
        let out = mono(&mut p, &input);
        rms(&out[LONG - 4_000..])
    };
    let (open, gated) = (tail(0.0), tail(1.0));
    assert!(open > 0.01, "keeps ringing: {open}");
    assert!(gated < open * 0.1, "gated tail: {open} {gated}");
}

#[test]
fn roar_feedback_is_bounded_and_finite_in_every_mode() {
    for mode in 0..5 {
        for routing in 0..7 {
            let mut p = roar_default();
            set(
                &mut p,
                &[
                    (roar::ROUTING, routing as f64),
                    (roar::FB_MODE, mode as f64),
                    (roar::FB_AMOUNT, 1.0),
                    (roar::FB_TIME, 10.0),
                    (roar::FB_NOTE, 90.0),
                    (stage(0, roar::STAGE_AMOUNT), 1.0),
                    (stage(0, roar::STAGE_SHAPER), 8.0),
                    (stage(1, roar::STAGE_ON), 1.0),
                    (stage(1, roar::STAGE_AMOUNT), 1.0),
                    (stage(1, roar::STAGE_SHAPER), 9.0),
                    (stage(2, roar::STAGE_ON), 1.0),
                    (stage(2, roar::STAGE_AMOUNT), 1.0),
                    (roar::DRIVE, 36.0),
                    (roar::OUTPUT, 12.0),
                    (roar::COMP_AMOUNT, 0.3),
                ],
            );
            let out = mono(&mut p, &noise(LONG, 7));
            assert!(
                out.iter().all(|s| s.is_finite()),
                "mode {mode} routing {routing}"
            );
            assert!(
                peak(&out) <= 1.0 + 1e-3,
                "mode {mode} routing {routing}: {}",
                peak(&out)
            );
        }
    }
}

#[test]
fn roar_delay_routing_gives_one_slapback_then_feedback_gives_more() {
    let echoes = |fb: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::ROUTING, 6.0),
                (roar::BLEND, 1.0),
                (stage(1, roar::STAGE_ON), 1.0),
                (roar::FB_MODE, 0.0),
                (roar::FB_AMOUNT, fb),
                (roar::FB_TIME, 40.0),
            ],
        );
        let lat = p.latency() as usize;
        let mut click = vec![0.0f32; 24_000];
        click[1_000] = 1.0;
        let (l, _) = render(&mut p, &click, &click);
        let window =
            |t: usize| peak(&l[1_000 + lat + t * 1_920 - 300..1_000 + lat + t * 1_920 + 300]);
        (window(1), window(2))
    };
    let (first, second) = echoes(0.0);
    assert!(first > 0.05, "the slapback: {first}");
    assert!(
        second < first * 0.1,
        "one repetition only: {first} {second}"
    );
    let (_, again) = echoes(0.6);
    assert!(again > 0.02, "feedback repeats: {again}");
}

#[test]
fn roar_feedback_routing_processes_the_loop_separately() {
    // Blend 1 hears only the stage in the loop, which is silent with no feedback.
    let with = |fb: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::ROUTING, 5.0),
                (roar::BLEND, 1.0),
                (stage(1, roar::STAGE_ON), 1.0),
                (roar::FB_AMOUNT, fb),
                (roar::FB_TIME, 20.0),
            ],
        );
        rms(&mono(&mut p, &tone(440.0, 0.5, LONG))[LONG / 2..])
    };
    assert!(with(0.0) < 1e-4, "{}", with(0.0));
    assert!(with(0.7) > 1e-3, "{}", with(0.7));
}

#[test]
fn roar_filters_each_do_what_the_manual_says() {
    let through = |kind: f64, params: &[(u32, f64)], hz: f32| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (stage(0, roar::STAGE_FILTER_ON), 1.0),
                (stage(0, roar::STAGE_FILTER), kind),
                (stage(0, roar::STAGE_FREQ), 1_000.0),
            ],
        );
        set(&mut p, params);
        amp_of(&mono(&mut p, &tone(hz, 0.3, LONG)), hz as f64)
    };
    assert!(
        through(0.0, &[], 100.0) > through(0.0, &[], 8_000.0) * 3.0,
        "LP"
    );
    assert!(
        through(1.0, &[], 1_000.0) > through(1.0, &[], 8_000.0) * 2.0,
        "BP"
    );
    assert!(
        through(2.0, &[], 8_000.0) > through(2.0, &[], 100.0) * 3.0,
        "HP"
    );
    assert!(
        through(3.0, &[], 1_000.0) < through(3.0, &[], 8_000.0) * 0.3,
        "Notch"
    );
    let peak_boost = &[(stage(0, roar::STAGE_PEAK), 12.0)];
    assert!(
        through(4.0, peak_boost, 1_000.0) > through(4.0, peak_boost, 8_000.0) * 2.0,
        "Peak"
    );
    // Morph goes LP to HP, BP in the middle: 0 passes lows, 1 passes highs.
    let (lows, highs) = (
        through(5.0, &[(stage(0, roar::STAGE_MORPH), 0.0)], 8_000.0),
        through(5.0, &[(stage(0, roar::STAGE_MORPH), 1.0)], 8_000.0),
    );
    assert!(highs > lows * 3.0, "Morph {lows} {highs}");
    // Comb notches: at the frequency of the first notch it's quieter than at the comb peak.
    let notch = through(6.0, &[(stage(0, roar::STAGE_RES), 0.0)], 500.0);
    let peak_hz = through(6.0, &[(stage(0, roar::STAGE_RES), 0.0)], 1_000.0);
    assert!(notch < peak_hz * 0.3, "Comb {notch} {peak_hz}");
    // Resampling adds images above the input.
    let mut p = roar_default();
    set(
        &mut p,
        &[
            (stage(0, roar::STAGE_FILTER_ON), 1.0),
            (stage(0, roar::STAGE_FILTER), 7.0),
            (stage(0, roar::STAGE_FREQ), 4_000.0),
        ],
    );
    let out = mono(&mut p, &tone(1_000.0, 0.3, LONG));
    assert!(amp_of(&out, 3_000.0) > 0.01, "resampling image");
    // Dispersion keeps the level at every frequency.
    for hz in [200.0, 1_000.0, 5_000.0] {
        let level = through(8.0, &[(stage(0, roar::STAGE_RES), 0.8)], hz);
        near(level, 0.3, 0.01, "Dispersion");
    }
}

#[test]
fn roar_dispersion_smears_a_click_in_time() {
    let mut p = roar_default();
    set(
        &mut p,
        &[
            (stage(0, roar::STAGE_FILTER_ON), 1.0),
            (stage(0, roar::STAGE_FILTER), 8.0),
            (stage(0, roar::STAGE_FREQ), 2_000.0),
            (stage(0, roar::STAGE_RES), 1.0),
        ],
    );
    let lat = p.latency() as usize;
    let mut click = vec![0.0f32; 4_096];
    click[100] = 0.5;
    let (l, _) = render(&mut p, &click, &click);
    let centre = l[100 + lat - 3..100 + lat + 3]
        .iter()
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    let tail: f32 = l[100 + lat + 20..].iter().map(|s| s * s).sum();
    assert!(
        tail > centre * centre * 0.1,
        "energy moved later: {tail} {centre}"
    );
}

#[test]
fn roar_tone_tilts_and_color_compensation_undoes_it() {
    let response = |tone_amt: f64, color: f64, hz: f32| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::TONE_AMOUNT, tone_amt),
                (roar::TONE_FREQ, 500.0),
                (roar::COLOR_COMP, color),
            ],
        );
        amp_of(&mono(&mut p, &tone(hz, 0.2, LONG)), hz as f64)
    };
    // Positive: highs up, lows down.
    assert!(response(1.0, 0.0, 8_000.0) > response(0.0, 0.0, 8_000.0));
    assert!(response(1.0, 0.0, 100.0) < response(0.0, 0.0, 100.0));
    // With Color Compensation and a linear stage, the two filters cancel.
    for hz in [100.0, 8_000.0] {
        near(
            response(1.0, 1.0, hz),
            response(0.0, 0.0, hz),
            0.01,
            "compensated",
        );
    }
}

#[test]
fn roar_compression_reduces_the_output_and_the_feedback() {
    let level = |comp: f64| {
        let mut p = roar_default();
        set(&mut p, &[(roar::COMP_AMOUNT, comp)]);
        rms(&mono(&mut p, &tone(220.0, 0.9, LONG))[LONG / 2..])
    };
    assert!(
        level(1.0) < level(0.0) * 0.7,
        "{} {}",
        level(1.0),
        level(0.0)
    );
}

#[test]
fn roar_compressor_sidechain_hp_ignores_the_bass() {
    let level = |hp: f64| {
        let mut p = roar_default();
        set(&mut p, &[(roar::COMP_AMOUNT, 1.0), (roar::COMP_HP, hp)]);
        rms(&mono(&mut p, &tone(60.0, 0.9, LONG))[LONG / 2..])
    };
    assert!(
        level(1.0) > level(0.0) * 1.2,
        "{} {}",
        level(1.0),
        level(0.0)
    );
}

#[test]
fn roar_output_hard_clips_before_dry_wet() {
    let mut p = roar_default();
    set(&mut p, &[(roar::OUTPUT, 30.0)]);
    let out = mono(&mut p, &tone(440.0, 0.9, LONG));
    assert!(peak(&out) <= 1.0, "{}", peak(&out));
    let mut p = roar_default();
    set(&mut p, &[(roar::DRY_WET, 0.0), (roar::OUTPUT, 30.0)]);
    let lat = p.latency() as usize;
    let input = noise(8_000, 42);
    let out = mono(&mut p, &input);
    for (a, b) in out[lat..].iter().zip(&input) {
        assert!((a - b).abs() < 1e-6, "dry path untouched");
    }
}

#[test]
fn roar_matrix_lfo_modulates_a_target() {
    // A slow LFO moving Output should change the level over time.
    let run = |cell: f64, global: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::LFO1_BASE + roar::LFO_RATE, 2.0),
                (roar::cell_param(0, roar::DEST_OUTPUT), cell),
                (roar::GLOBAL_MOD, global),
            ],
        );
        let out = mono(&mut p, &tone(1_000.0, 0.5, LONG));
        let frames: Vec<f32> = out[4_000..].chunks(2_000).map(rms).collect();
        let lo = frames.iter().cloned().fold(f32::MAX, f32::min);
        let hi = frames.iter().cloned().fold(0.0f32, f32::max);
        hi / lo.max(1e-9)
    };
    assert!(run(0.5, 1.0) > 2.0, "{}", run(0.5, 1.0));
    assert!(
        run(0.5, 0.0) < 1.1,
        "global 0 disables it: {}",
        run(0.5, 0.0)
    );
    assert!(run(0.0, 1.0) < 1.1, "no cell no modulation");
}

/// How much Roar's Output has been opened by a route, as output level over input level.
fn roar_gain(p: &mut roar::Roar, input: &[f32], window: std::ops::Range<usize>) -> f32 {
    let out = mono(p, input);
    let lat = p.latency() as usize;
    rms(&out[window.start + lat..window.end + lat]) / rms(&input[window]).max(1e-9)
}

#[test]
fn roar_envelope_source_follows_the_input() {
    let mut input = tone(1_000.0, 0.8, LONG);
    for s in input[LONG / 2..].iter_mut() {
        *s *= 0.05;
    }
    let settings = [
        (roar::ENV_ATTACK, 1.0),
        (roar::ENV_RELEASE, 20.0),
        (roar::ENV_THRESHOLD, -60.0),
        (roar::cell_param(2, roar::DEST_OUTPUT), 1.0),
        (roar::OUTPUT, -24.0),
    ];
    let mut p = roar_default();
    set(&mut p, &settings);
    let loud = roar_gain(&mut p, &input, 12_000..20_000);
    let mut p = roar_default();
    set(&mut p, &settings);
    let quiet = roar_gain(&mut p, &input, 36_000..44_000);
    assert!(loud > quiet * 3.0, "{loud} {quiet}");
}

#[test]
fn roar_envelope_hold_finishes_the_attack_before_releasing() {
    // A short burst over a quiet bed: without Hold the envelope turns round when the burst ends.
    let mut input = tone(1_000.0, 0.05, LONG);
    for (s, loud) in input[12_000..12_240]
        .iter_mut()
        .zip(tone(1_000.0, 0.8, 240))
    {
        *s = loud;
    }
    let reading = |hold: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::ENV_ATTACK, 50.0),
                (roar::ENV_RELEASE, 5.0),
                (roar::ENV_HOLD, hold),
                (roar::cell_param(2, roar::DEST_OUTPUT), 1.0),
                (roar::OUTPUT, -24.0),
            ],
        );
        roar_gain(&mut p, &input, 13_200..13_680)
    };
    assert!(
        reading(1.0) > reading(0.0) * 1.5,
        "{} {}",
        reading(1.0),
        reading(0.0)
    );
}

#[test]
fn roar_lfo_waves_have_the_expected_shape() {
    let run = |wave: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::LFO1_BASE + roar::LFO_WAVE, wave),
                (roar::LFO1_BASE + roar::LFO_RATE, 1.0),
                (roar::cell_param(0, roar::DEST_OUTPUT), 0.5),
                (roar::OUTPUT, -12.0),
            ],
        );
        let input = tone(1_000.0, 0.5, LONG);
        let out = mono(&mut p, &input);
        // Level in dB at a quarter and three quarters of the cycle.
        let at = |t: usize| db(rms(&out[t..t + 960]) as f64 / 0.5f64.sqrt() / 0.5);
        (at(11_000), at(35_000))
    };
    // A 1 Hz square is at its top for the first half and its bottom for the second.
    let (a, b) = run(2.0);
    assert!(a - b > 15.0, "square {a} {b}");
    // A falling saw is high early and low late, and a rising one the other way round.
    let (a, b) = run(4.0);
    assert!(a > b, "down {a} {b}");
    let (a, b) = run(3.0);
    assert!(b > a, "up {a} {b}");
}

#[test]
fn roar_lfo_smooth_and_morph_change_its_shape() {
    let level_jump = |smooth: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::LFO1_BASE + roar::LFO_WAVE, 2.0),
                (roar::LFO1_BASE + roar::LFO_RATE, 2.0),
                (roar::LFO1_BASE + roar::LFO_SMOOTH, smooth),
                (roar::cell_param(0, roar::DEST_OUTPUT), 0.5),
                (roar::OUTPUT, -12.0),
            ],
        );
        let out = mono(&mut p, &tone(1_000.0, 0.5, LONG));
        out.chunks(480)
            .map(rms)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    };
    assert!(
        level_jump(0.0) > level_jump(1.0) * 2.0,
        "{} {}",
        level_jump(0.0),
        level_jump(1.0)
    );
    let level_at = |morph: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::LFO1_BASE + roar::LFO_WAVE, 3.0),
                (roar::LFO1_BASE + roar::LFO_RATE, 1.0),
                (roar::LFO1_BASE + roar::LFO_MORPH, morph),
                (roar::cell_param(0, roar::DEST_OUTPUT), 0.5),
                (roar::OUTPUT, -12.0),
            ],
        );
        rms(&mono(&mut p, &tone(1_000.0, 0.5, LONG))[11_000..12_000])
    };
    assert!((level_at(0.1) - level_at(0.9)).abs() > 0.02);
}

#[test]
fn roar_noise_types_all_move_and_differ() {
    let run = |kind: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::NOISE_TYPE, kind),
                (roar::NOISE_RATE, 20.0),
                (roar::cell_param(3, roar::DEST_OUTPUT), 0.5),
                (roar::OUTPUT, -12.0),
            ],
        );
        let out = mono(&mut p, &tone(1_000.0, 0.5, LONG));
        out.chunks(480).map(rms).collect::<Vec<f32>>()
    };
    let curves: Vec<Vec<f32>> = (0..4).map(|k| run(k as f64)).collect();
    for (k, c) in curves.iter().enumerate() {
        let (lo, hi) = c[8..]
            .iter()
            .fold((f32::MAX, 0.0f32), |(l, h), v| (l.min(*v), h.max(*v)));
        assert!(hi > lo * 1.5, "noise {k} moves: {lo} {hi}");
    }
    for a in 0..4 {
        for b in a + 1..4 {
            assert_ne!(curves[a], curves[b], "{a} {b}");
        }
    }
}

#[test]
fn roar_view_reports_every_source_for_the_matrix_leds() {
    use rlive_core::ViewChannel;
    use std::sync::Arc;
    let channel = Arc::new(ViewChannel::new(roar::VIEW_LEN));
    let reader = channel.attach();
    let writer = ViewWriter::new(&channel);
    let mut p = roar_default();
    set(&mut p, &[(roar::LFO1_BASE + roar::LFO_RATE, 8.0)]);
    let transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        tempo: 120.0,
        ..Default::default()
    };
    let input = tone(1_000.0, 0.5, BLOCK * 8);
    let (mut l, mut r) = (input.clone(), input.clone());
    let mut events = EventQueue::with_capacity(0);
    let mut param_out = ParamOutQueue::with_capacity(0);
    let mut seen = Vec::new();
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        let mut ctx = ProcessContext {
            audio: AudioBuffer {
                left: cl,
                right: cr,
            },
            sidechain: None,
            aux_out: &mut [],
            events: &[],
            events_out: &mut events,
            params: &[],
            transport: &transport,
            param_out: &mut param_out,
            mod_out: &mut [],
            view: &writer,
        };
        assert_no_alloc(|| p.process(&mut ctx));
        let mut levels = [0.0f32; roar::VIEW_LEN];
        assert!(reader.read(roar::MOD_LEVELS, &mut levels));
        seen.push(levels);
    }
    assert!(seen.windows(2).any(|w| w[0][0] != w[1][0]), "LFO 1 moves");
    assert!(
        seen.iter().any(|l| l[2] > 0.1),
        "Env sees the input: {seen:?}"
    );
}

#[test]
fn roar_rates_lock_to_the_beat_clock_in_sync_modes() {
    // A Synced 1/4 LFO repeats at a beat. At 120 BPM that's 24000 samples.
    let mut p = roar_default();
    set(
        &mut p,
        &[
            (roar::LFO1_BASE + roar::LFO_MODE, 1.0),
            (roar::LFO1_BASE + roar::LFO_SYNC, 4.0),
            (roar::cell_param(0, roar::DEST_OUTPUT), 0.5),
        ],
    );
    let mut h = Harness::new(SR);
    h.transport = TransportInfo {
        tempo: 120.0,
        playing: true,
        ..h.transport
    };
    let input = tone(1_000.0, 0.5, 72_000);
    let (mut l, mut r) = (input.clone(), input.clone());
    let block_beats = 120.0 / 60.0 * BLOCK as f64 / SR;
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        h.transport.tick = i as f64 * block_beats * rlive_core::PPQ as f64;
        assert_no_alloc(|| h.process(&mut p, cl, cr, &[], &[]));
    }
    let gain = |t: usize| rms(&l[t..t + 480]) / rms(&input[t..t + 480]);
    near(
        gain(30_000) as f64,
        gain(54_000) as f64,
        0.1,
        "same level a beat later",
    );
    assert!(
        (gain(30_000) - gain(36_000)).abs() > 0.05,
        "moves within the beat"
    );
}

#[test]
fn roar_midi_sets_the_feedback_note_when_enabled() {
    fn note_on(offset: u32, key: u8) -> MidiEvent {
        MidiEvent::new(
            offset,
            MidiMessage::NoteOn {
                channel: 0,
                key,
                velocity: 1.0,
                note_id: 0,
                tuning: 0.0,
            },
        )
    }
    let spectrum = |midi: f64, key: u8, hz: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::MIDI_FB, midi),
                (roar::FB_NOTE, 40.0),
                (roar::FB_AMOUNT, 0.0),
            ],
        );
        let mut h = Harness::new(SR);
        let input = noise(LONG, 50);
        let (mut l, mut r) = (input.clone(), input.clone());
        let events = [note_on(0, key)];
        for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
            let e: &[MidiEvent] = if i == 0 { &events } else { &[] };
            assert_no_alloc(|| h.process(&mut p, cl, cr, e, &[]));
        }
        amp_of(&l[LONG / 2..], hz)
    };
    let (on, off) = (spectrum(1.0, 69, 440.0), spectrum(0.0, 69, 440.0));
    assert!(on > off * 2.0, "{on} {off}");
}

#[test]
fn roar_external_sidechain_drives_the_envelope_at_its_mix() {
    let render_sc = |mix: f64| {
        let mut p = roar_default();
        set(
            &mut p,
            &[
                (roar::EXT_SC, 1.0),
                (roar::SC_MIX, mix),
                (roar::ENV_RELEASE, 5.0),
                (roar::ENV_THRESHOLD, -40.0),
                (roar::cell_param(2, roar::DEST_DRY_WET), 1.0),
                (roar::DRY_WET, 0.0),
                (roar::OUTPUT, -60.0),
            ],
        );
        // External tone only in the second half.
        let main = tone(1_000.0, 0.3, LONG);
        let mut key = vec![0.0f32; LONG];
        for (k, s) in key[LONG / 2..].iter_mut().zip(tone(1_000.0, 0.8, LONG / 2)) {
            *k = s;
        }
        let (mut l, mut r) = (main.clone(), main.clone());
        let mut events = EventQueue::with_capacity(0);
        let mut param_out = ParamOutQueue::with_capacity(0);
        let view = ViewWriter::none();
        let transport = TransportInfo {
            playing: true,
            sample_rate: SR,
            ..Default::default()
        };
        for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
            let n = cl.len();
            let mut ctx = ProcessContext {
                audio: AudioBuffer {
                    left: cl,
                    right: cr,
                },
                sidechain: Some(AudioInput {
                    left: &key[i * BLOCK..i * BLOCK + n],
                    right: &key[i * BLOCK..i * BLOCK + n],
                }),
                aux_out: &mut [],
                events: &[],
                events_out: &mut events,
                params: &[],
                transport: &transport,
                param_out: &mut param_out,
                mod_out: &mut [],
                view: &view,
            };
            assert_no_alloc(|| p.process(&mut ctx));
        }
        l
    };
    // The Dry/Wet route opens the wet (Output -60 dB) path wider in the second half when the key
    // is heard, attenuating the dry; at Mix 0 the key is ignored and the level doesn't change.
    let change = |out: &[f32]| rms(&out[LONG * 3 / 4..]) - rms(&out[LONG / 8..LONG / 4]);
    let heard = change(&render_sc(1.0));
    assert!(heard.abs() > 0.03, "{heard}");
    let ignored = change(&render_sc(0.0));
    assert!(ignored.abs() < 0.01, "{ignored}");
}

#[test]
fn roar_listens_to_the_envelope_input() {
    let mut p = roar_default();
    set(&mut p, &[(roar::ENV_LISTEN, 1.0)]);
    let input = tone(1_000.0, 0.5, LONG);
    let out = mono(&mut p, &input);
    // The listen output is the key, not the wet path.
    assert!(rms(&out[LONG / 2..]) > 0.1);
}

// ------------------------------------------------------------------------- Whole batch

const BATCH: [&str; 10] = [
    "rlive.saturator",
    "rlive.overdrive",
    "rlive.dynamic_tube",
    "rlive.pedal",
    "rlive.amp",
    "rlive.cabinet",
    "rlive.erosion",
    "rlive.redux",
    "rlive.vinyl_distortion",
    "rlive.roar",
];

fn make(id: &str) -> Box<dyn Processor> {
    let info = find(id).expect(id);
    let mut p = (info.create)(&DeviceInit {
        samples: vec![],
        state: None,
    });
    p.activate(SR, BLOCK);
    p
}

#[test]
fn every_device_in_the_batch_is_built_and_lists_its_own_panels() {
    for id in BATCH {
        let info = find(id).unwrap();
        assert_eq!(info.status, rlive_core::Status::Beta, "{id}");
        assert!(!(info.desc)().layout.is_empty(), "{id}");
    }
    assert!(builtins().iter().filter(|b| BATCH.contains(&b.id)).count() == 10);
}

#[test]
fn extreme_settings_stay_finite_and_never_allocate_at_any_block_size() {
    for id in BATCH {
        for pick_max in [false, true] {
            let mut p = make(id);
            let info = find(id).unwrap();
            for param in (info.desc)().params {
                p.set_param(param.id, if pick_max { param.max } else { param.min });
            }
            let input = noise(16_384, 9);
            for chunk in [17usize, 64, 128, BLOCK] {
                let mut h = Harness::new(SR);
                let (mut l, mut r) = (input.clone(), input.clone());
                for (cl, cr) in l.chunks_mut(chunk).zip(r.chunks_mut(chunk)) {
                    assert_no_alloc(|| h.process(&mut *p, cl, cr, &[], &[]));
                }
                assert!(
                    l.iter().chain(&r).all(|s| s.is_finite()),
                    "{id} went non-finite (max {pick_max}, block {chunk})"
                );
            }
        }
    }
}

#[test]
fn stepping_every_menu_position_is_stable_and_doesnt_allocate() {
    for id in BATCH {
        let info = find(id).unwrap();
        for param in (info.desc)().params {
            let rlive_core::ParamKind::Stepped { labels } = &param.kind else {
                continue;
            };
            for step in 0..labels.len() {
                let mut p = make(id);
                let mut h = Harness::new(SR);
                let input = noise(BLOCK * 3, 12);
                let (mut l, mut r) = (input.clone(), input.clone());
                let event = ParamEvent {
                    offset: 30,
                    id: ParamId(param.id),
                    value: step as f64,
                };
                assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &[], &[event]));
                assert!(
                    l.iter().chain(&r).all(|s| s.is_finite()),
                    "{id} {}={step}",
                    param.name
                );
            }
        }
    }
}

#[test]
fn every_device_in_the_batch_stops_ringing_after_reset() {
    for id in BATCH {
        let mut p = make(id);
        let mut h = Harness::new(SR);
        let input = noise(BLOCK * 10, 13);
        let (mut l, mut r) = (input.clone(), input.clone());
        for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
            h.process(&mut *p, cl, cr, &[], &[]);
        }
        p.reset();
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        for _ in 0..4 {
            h.process(&mut *p, &mut l, &mut r, &[], &[]);
            assert!(peak(&l) < 1e-6, "{id} rings on after reset: {}", peak(&l));
        }
    }
}
