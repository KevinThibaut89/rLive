//! Batch B3: the dynamics devices, against what their curves and time constants say should happen.

use rlive_core::{
    AudioBuffer, AudioInput, EventQueue, ParamOutQueue, ProcessContext, Processor, TransportInfo,
    ViewWriter,
};
use rlive_devices::devices::b3::{
    common, drum_buss as db, gate, glue_compressor as glue, limiter as lim,
    multiband_dynamics as mb,
};
use rlive_devices::dsp::oversample::{OversampleFactor, Oversampler};
use rlive_devices::effects::{comp, Compressor};
use rlive_testkit::{assert_no_alloc, onset, peak, rms, sine, tone_amplitude, Harness};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

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

/// Runs a stereo signal through `p`, a block at a time, with nothing on the sidechain.
fn render(p: &mut dyn Processor, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        assert_no_alloc(|| h.process(p, cl, cr, &[], &[]));
    }
    (l, r)
}

fn mono(p: &mut dyn Processor, input: &[f32]) -> Vec<f32> {
    render(p, input, input).0
}

/// Runs a signal through `p` with `key` routed to its sidechain.
fn render_keyed(p: &mut dyn Processor, input: &[f32], key: &[f32]) -> Vec<f32> {
    let mut out = input.to_vec();
    let mut right = input.to_vec();
    let transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        ..Default::default()
    };
    let view = ViewWriter::none();
    for (i, (l, r)) in out
        .chunks_mut(BLOCK)
        .zip(right.chunks_mut(BLOCK))
        .enumerate()
    {
        let n = l.len();
        let k = &key[i * BLOCK..i * BLOCK + n];
        let mut events_out = EventQueue::with_capacity(0);
        let mut param_out = ParamOutQueue::with_capacity(0);
        let mut ctx = ProcessContext {
            audio: AudioBuffer { left: l, right: r },
            sidechain: Some(AudioInput { left: k, right: k }),
            aux_out: &mut [],
            events: &[],
            events_out: &mut events_out,
            params: &[],
            transport: &transport,
            param_out: &mut param_out,
            mod_out: &mut [],
            view: &view,
        };
        assert_no_alloc(|| p.process(&mut ctx));
    }
    out
}

fn db(linear: f32) -> f32 {
    20.0 * linear.abs().max(1e-12).log10()
}

fn gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn dc(level_db: f32, frames: usize) -> Vec<f32> {
    vec![gain(level_db); frames]
}

/// Where the loudest sample is.
fn peak_index(buf: &[f32]) -> usize {
    buf.iter()
        .enumerate()
        .fold((0, 0.0f32), |best, (i, s)| {
            if s.abs() > best.1 {
                (i, s.abs())
            } else {
                best
            }
        })
        .0
}

fn assert_near(actual: f32, expected: f32, tol: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} != {expected} (±{tol})"
    );
}

/// The level of a steady signal after `p` has settled on it.
fn settled_db(p: &mut dyn Processor, input: &[f32]) -> f32 {
    let out = mono(p, input);
    db(*out.last().unwrap())
}

// ---------------------------------------------------------------- Compressor

fn fast_compressor(extra: &[(u32, f64)]) -> Compressor {
    let mut c: Compressor = ready();
    set(
        &mut c,
        &[
            (comp::ATTACK, 0.01),
            (comp::RELEASE, 1000.0),
            (comp::KNEE, 0.0),
        ],
    );
    set(&mut c, extra);
    c
}

#[test]
fn compressor_transfer_curve_matches_threshold_and_ratio() {
    for (level, expect) in [(-40.0, -40.0), (-20.0, -20.0), (-8.0, -17.0), (0.0, -15.0)] {
        let mut c = fast_compressor(&[(comp::THRESHOLD, -20.0), (comp::RATIO, 4.0)]);
        assert_near(
            settled_db(&mut c, &dc(level, 48_000)),
            expect,
            0.05,
            "curve",
        );
    }
}

#[test]
fn compressor_ratio_of_one_is_transparent_and_the_top_of_the_range_is_infinite() {
    let mut c = fast_compressor(&[(comp::THRESHOLD, -30.0), (comp::RATIO, 1.0)]);
    assert_near(settled_db(&mut c, &dc(-10.0, 48_000)), -10.0, 0.01, "1:1");
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -12.0),
        (comp::RATIO, comp::RATIO_INF as f64),
    ]);
    assert_near(settled_db(&mut c, &dc(-6.0, 48_000)), -12.0, 0.05, "inf:1");
}

#[test]
fn compressor_knee_starts_early_and_finishes_late() {
    // A 10 dB knee at -20 dB: nothing at -30, full ratio at -10, half way in between.
    let curve = |level: f32| {
        let mut c = fast_compressor(&[
            (comp::THRESHOLD, -20.0),
            (comp::RATIO, 4.0),
            (comp::KNEE, 10.0),
        ]);
        settled_db(&mut c, &dc(level, 48_000)) - level
    };
    assert_near(curve(-31.0), 0.0, 0.05, "below the knee");
    assert_near(curve(-20.0), -0.75 * 25.0 / 20.0, 0.05, "in the knee");
    assert_near(curve(-10.0), -0.75 * 10.0, 0.05, "above the knee");
}

#[test]
fn compressor_attack_follows_its_time_constant() {
    let (t, r) = (-40.0, 4.0);
    let mut c: Compressor = ready();
    set(
        &mut c,
        &[
            (comp::THRESHOLD, t),
            (comp::RATIO, r),
            (comp::KNEE, 0.0),
            (comp::ATTACK, 10.0),
        ],
    );
    let input = dc(-10.0, 4_800);
    let out = mono(&mut c, &input);
    let target = (0.75 * 30.0) as f32;
    let tau = 480;
    let reduced = -10.0 - db(out[tau]);
    assert_near(
        reduced,
        target * (1.0 - (-1.0f32).exp()),
        0.25,
        "63% of the way after one time constant",
    );
}

#[test]
fn compressor_release_follows_its_time_constant() {
    let mut c: Compressor = ready();
    set(
        &mut c,
        &[
            (comp::THRESHOLD, -40.0),
            (comp::RATIO, 4.0),
            (comp::KNEE, 0.0),
            (comp::ATTACK, 0.01),
            (comp::RELEASE, 100.0),
        ],
    );
    let mut input = dc(-10.0, 48_000);
    input.extend(dc(-60.0, 19_200));
    let out = mono(&mut c, &input);
    let target = (0.75 * 30.0) as f32;
    // One time constant into the quiet part.
    let reduced = -60.0 - db(out[48_000 + 4_800]);
    assert_near(reduced, target / std::f32::consts::E, 0.3, "37% left");
}

#[test]
fn compressor_rms_reads_a_sine_lower_than_peak() {
    let tone = sine(1_000.0, SR, 48_000);
    let measure = |mode: f64| {
        let mut c = fast_compressor(&[
            (comp::THRESHOLD, -20.0),
            (comp::RATIO, 4.0),
            (comp::MODE, mode),
        ]);
        let out = mono(&mut c, &tone);
        db(peak(&out[24_000..]))
    };
    // The level a Peak detector sees is 0 dB; an RMS one, 3 dB under it.
    assert_near(measure(comp::MODE_PEAK), -0.75 * 20.0, 0.3, "peak");
    assert_near(measure(comp::MODE_RMS), -0.75 * (20.0 - 3.01), 0.3, "rms");
}

#[test]
fn compressor_rms_ignores_a_short_peak_that_peak_mode_catches() {
    let mut pulse = vec![0.0; 2_000];
    pulse[1_000..1_024].fill(1.0);
    let depth = |mode: f64| {
        let mut c = fast_compressor(&[
            (comp::THRESHOLD, -20.0),
            (comp::RATIO, 4.0),
            (comp::MODE, mode),
        ]);
        let out = mono(&mut c, &pulse);
        db(out[1_023])
    };
    assert!(depth(comp::MODE_PEAK) < -10.0, "{}", depth(comp::MODE_PEAK));
    assert!(depth(comp::MODE_RMS) > -7.0, "{}", depth(comp::MODE_RMS));
}

#[test]
fn compressor_expand_raises_what_is_above_the_threshold() {
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -30.0),
        (comp::RATIO, 2.0),
        (comp::MODE, comp::MODE_EXPAND),
    ]);
    assert_near(
        settled_db(&mut c, &dc(-20.0, 48_000)),
        -10.0,
        0.05,
        "1:2 up",
    );
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -30.0),
        (comp::RATIO, 2.0),
        (comp::MODE, comp::MODE_EXPAND),
    ]);
    assert_near(settled_db(&mut c, &dc(-40.0, 48_000)), -40.0, 0.05, "below");
}

#[test]
fn compressor_expand_gain_is_capped_but_only_far_past_normal_levels() {
    // At 1:2, a signal is 36 dB over the threshold before the cap is reached.
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -30.0),
        (comp::RATIO, 2.0),
        (comp::MODE, comp::MODE_EXPAND),
    ]);
    assert_near(
        settled_db(&mut c, &dc(-12.0, 48_000)),
        6.0,
        0.05,
        "uncapped",
    );
    // A huge ratio can't go past it.
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -60.0),
        (comp::RATIO, comp::RATIO_INF as f64),
        (comp::MODE, comp::MODE_EXPAND),
    ]);
    let out = settled_db(&mut c, &dc(-20.0, 48_000));
    assert!(
        out <= -20.0 + common::MAX_UPWARD_DB + 0.01 && out.is_finite(),
        "{out}"
    );
}

/// Half the time to go from `target` dB of reduction to half of it.
fn half_recovery_ms(envelope: f64, target_db: f32) -> f32 {
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -70.0),
        (comp::RATIO, comp::RATIO_INF as f64),
        (comp::ENVELOPE, envelope),
        (comp::RELEASE, 200.0),
    ]);
    let level = -70.0 + target_db;
    let hold = 24_000;
    let mut input = dc(level, hold);
    input.extend(dc(-96.0, 48_000));
    let out = mono(&mut c, &input);
    // The gain in dB the quiet tail is losing, relative to its input.
    let half = target_db / 2.0;
    out[hold..]
        .iter()
        .position(|s| (-96.0 - db(*s)) < half)
        .map(|i| i as f32 / 48.0)
        .expect("recovers")
}

#[test]
fn compressor_lin_release_time_does_not_depend_on_depth() {
    let (shallow, deep) = (
        half_recovery_ms(comp::ENV_LIN, 6.0),
        half_recovery_ms(comp::ENV_LIN, 24.0),
    );
    assert_near(shallow, deep, 3.0, "lin half time");
}

#[test]
fn compressor_log_recovers_deep_reduction_faster() {
    let (shallow, deep) = (
        half_recovery_ms(comp::ENV_LOG, 6.0),
        half_recovery_ms(comp::ENV_LOG, 24.0),
    );
    assert!(deep < shallow, "deep {deep} shallow {shallow}");
    assert!(
        deep < half_recovery_ms(comp::ENV_LIN, 24.0),
        "faster than lin"
    );
}

#[test]
fn compressor_auto_release_is_short_after_a_burst_and_longer_when_sustained() {
    let recovery_samples = |burst: usize| {
        let mut c = fast_compressor(&[
            (comp::THRESHOLD, -30.0),
            (comp::RATIO, 4.0),
            (comp::AUTO_RELEASE, 1.0),
        ]);
        let mut input = dc(-6.0, burst);
        input.extend(dc(-40.0, 96_000));
        let out = mono(&mut c, &input);
        out[burst..]
            .iter()
            .position(|s| (db(*s) + 40.0).abs() < 1.0)
            .expect("recovers")
    };
    let (short, long) = (recovery_samples(480), recovery_samples(48_000));
    assert!(short < long, "short {short} long {long}");
    assert!(long > short * 2, "short {short} long {long}");
}

#[test]
fn compressor_lookahead_reports_its_exact_latency() {
    for (index, frames) in [(0.0, 0usize), (1.0, 48), (2.0, 480)] {
        let mut c: Compressor = ready();
        // A knob away from plain, so lookahead takes the full path.
        set(
            &mut c,
            &[(comp::LOOKAHEAD, index), (comp::ENVELOPE, comp::ENV_LOG)],
        );
        assert_eq!(c.latency() as usize, frames, "declared");
        let mut click = vec![0.0; BLOCK * 2];
        click[10] = 0.5;
        let out = mono(&mut c, &click);
        assert_eq!(onset(&out, 1e-3), Some(10 + frames), "measured");
    }
}

#[test]
fn compressor_lookahead_catches_a_transient_before_it_reaches_the_output() {
    let mut step = vec![0.0; 4_800];
    step.extend(dc(-6.0, 4_800));
    let at_onset = |lookahead: f64, frames: usize| {
        let mut c: Compressor = ready();
        set(
            &mut c,
            &[
                (comp::THRESHOLD, -30.0),
                (comp::RATIO, comp::RATIO_INF as f64),
                (comp::ATTACK, 1.0),
                (comp::KNEE, 0.0),
                (comp::LOOKAHEAD, lookahead),
                (comp::ENVELOPE, comp::ENV_LOG),
            ],
        );
        let out = mono(&mut c, &step);
        db(out[4_800 + frames])
    };
    let without = at_onset(0.0, 0);
    let with = at_onset(1.0, 48);
    let long = at_onset(2.0, 480);
    assert!(without > -8.0, "no lookahead lets it through: {without}");
    assert!(with < without - 10.0, "1 ms: {with}");
    assert!(long < -29.0, "10 ms: {long}");
}

#[test]
fn compressor_auto_makeup_restores_a_full_scale_signal() {
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -30.0),
        (comp::RATIO, 3.0),
        (comp::AUTO_MAKEUP, 1.0),
    ]);
    assert_near(settled_db(&mut c, &dc(0.0, 48_000)), 0.0, 0.05, "makeup");
}

#[test]
fn compressor_auto_makeup_is_off_while_an_external_sidechain_drives_it() {
    let input = dc(0.0, 48_000);
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -30.0),
        (comp::RATIO, 3.0),
        (comp::AUTO_MAKEUP, 1.0),
        (comp::SIDECHAIN, 1.0),
    ]);
    let out = render_keyed(&mut c, &input, &input);
    let reduced = db(*out.last().unwrap());
    assert_near(reduced, -20.0, 0.1, "no makeup with an external key");
}

/// The Compressor as it was before Live's modes were added. If the plain path changes, the
/// golden render's hashes can't hold, and neither can projects saved with it.
fn original_compressor(left: &mut [f32], right: &mut [f32]) {
    use rlive_core::{db_to_gain, gain_to_db};
    let (t, r, k, makeup, mix) = (-18.0f32, 4.0f32, 6.0f32, 0.0f32, 1.0f32);
    let att = (-1.0 / (10.0f32 * 0.001 * SR as f32)).exp();
    let rel = (-1.0 / (120.0f32 * 0.001 * SR as f32)).exp();
    let mut env_db = 0.0f32;
    for i in 0..left.len() {
        let (a, b) = (left[i], right[i]);
        let level = gain_to_db(a.abs().max(b.abs()));
        let over = level - t;
        let slope = 1.0 - 1.0 / r.max(1.0);
        let gr = if k > 0.0 && over.abs() <= k / 2.0 {
            slope * (over + k / 2.0).powi(2) / (2.0 * k)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        };
        let coeff = if gr > env_db { att } else { rel };
        env_db = gr + (env_db - gr) * coeff;
        let g = db_to_gain(makeup - env_db);
        left[i] = a * (1.0 - mix) + a * g * mix;
        right[i] = b * (1.0 - mix) + b * g * mix;
    }
}

#[test]
fn compressor_default_path_is_the_original_sample_for_sample() {
    let mut left = sine(220.0, SR, 48_000);
    let mut right = sine(663.0, SR, 48_000);
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let env = 0.8 * (-(i as f32) / 12_000.0).exp();
        *l *= env;
        *r *= env * 0.6;
    }
    let (mut expected_l, mut expected_r) = (left.clone(), right.clone());
    original_compressor(&mut expected_l, &mut expected_r);
    let mut c: Compressor = ready();
    let (l, r) = render(&mut c, &left, &right);
    assert_eq!(l, expected_l);
    assert_eq!(r, expected_r);
}

#[test]
fn compressor_defaults_keep_their_plain_values() {
    let by_id = |id: u32| {
        comp::compressor_params()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap()
    };
    for (id, default) in [
        (comp::THRESHOLD, -18.0),
        (comp::RATIO, 4.0),
        (comp::ATTACK, 10.0),
        (comp::RELEASE, 120.0),
        (comp::KNEE, 6.0),
        (comp::MAKEUP, 0.0),
        (comp::MIX, 1.0),
    ] {
        assert_eq!(by_id(id).default, default, "{}", by_id(id).name);
    }
    let makeup = by_id(comp::MAKEUP);
    assert_eq!(makeup.name, "Makeup", "the released name stays");
}

// ---------------------------------------------------------------- Sidechain

#[test]
fn a_compressor_keys_off_its_sidechain_instead_of_its_own_signal() {
    let input = dc(-20.0, 48_000);
    let silence = vec![0.0; 48_000];
    let loud = dc(0.0, 48_000);
    let build = || {
        fast_compressor(&[
            (comp::THRESHOLD, -30.0),
            (comp::RATIO, 4.0),
            (comp::SIDECHAIN, 1.0),
        ])
    };
    // A silent key does nothing, a loud one ducks the signal.
    let mut c = build();
    assert_near(
        db(*render_keyed(&mut c, &input, &silence).last().unwrap()),
        -20.0,
        0.05,
        "silent",
    );
    let mut c = build();
    let keyed = db(*render_keyed(&mut c, &input, &loud).last().unwrap());
    assert_near(keyed, -20.0 - 0.75 * 30.0, 0.1, "loud key");
    // Unrouted, it listens to its own input, even with the switch on.
    let mut c = build();
    let own = settled_db(&mut c, &input);
    assert_near(own, -20.0 - 0.75 * 10.0, 0.1, "no route");
}

#[test]
fn a_compressor_ignores_the_sidechain_while_its_switch_is_off() {
    let input = dc(-20.0, 48_000);
    let loud = dc(0.0, 48_000);
    let mut c = fast_compressor(&[(comp::THRESHOLD, -30.0), (comp::RATIO, 4.0)]);
    let plain = db(*render_keyed(&mut c, &input, &loud).last().unwrap());
    assert_near(plain, -20.0 - 0.75 * 10.0, 0.1, "switch off");
}

#[test]
fn sidechain_gain_scales_only_the_key() {
    let input = dc(-20.0, 48_000);
    let key = dc(-40.0, 48_000);
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -30.0),
        (comp::RATIO, 4.0),
        (comp::SIDECHAIN, 1.0),
    ]);
    assert_near(
        db(*render_keyed(&mut c, &input, &key).last().unwrap()),
        -20.0,
        0.05,
        "key is quiet",
    );
    let mut c = fast_compressor(&[
        (comp::THRESHOLD, -30.0),
        (comp::RATIO, 4.0),
        (comp::SIDECHAIN, 1.0),
        (comp::SIDECHAIN + common::SC_GAIN, 20.0),
    ]);
    let boosted = db(*render_keyed(&mut c, &input, &key).last().unwrap());
    assert_near(boosted, -20.0 + (-20.0 + 30.0) * -0.75, 0.15, "20 dB up");
}

#[test]
fn sidechain_mix_blends_the_key_with_the_devices_own_signal() {
    let input = dc(-20.0, 48_000);
    let silence = vec![0.0; 48_000];
    let build = |mix: f64| {
        fast_compressor(&[
            (comp::THRESHOLD, -30.0),
            (comp::RATIO, 4.0),
            (comp::SIDECHAIN, 1.0),
            (comp::SIDECHAIN + common::SC_MIX, mix),
        ])
    };
    let mut c = build(0.0);
    let own = db(*render_keyed(&mut c, &input, &silence).last().unwrap());
    assert_near(
        own,
        -20.0 - 0.75 * 10.0,
        0.1,
        "mix 0 is the device's own signal",
    );
    let mut c = build(1.0);
    let keyed = db(*render_keyed(&mut c, &input, &silence).last().unwrap());
    assert_near(keyed, -20.0, 0.05, "mix 1 is the key alone");
}

#[test]
fn sidechain_eq_filters_what_the_compressor_hears() {
    let input = dc(-20.0, 48_000);
    let hiss: Vec<f32> = sine(8_000.0, SR, 48_000);
    let build = |eq: bool| {
        fast_compressor(&[
            (comp::THRESHOLD, -30.0),
            (comp::RATIO, 4.0),
            (comp::SIDECHAIN, 1.0),
            (comp::SIDECHAIN + common::SC_EQ_ON, f64::from(eq)),
            (comp::SIDECHAIN + common::SC_EQ_TYPE, 0.0),
            (comp::SIDECHAIN + common::SC_EQ_FREQ, 100.0),
        ])
    };
    let mut c = build(false);
    let open = db(*render_keyed(&mut c, &input, &hiss).last().unwrap());
    let mut c = build(true);
    let filtered = db(*render_keyed(&mut c, &input, &hiss).last().unwrap());
    assert!(filtered > open + 6.0, "open {open} filtered {filtered}");
    assert_near(
        filtered,
        -20.0,
        0.2,
        "a low-pass removes the hiss from the key",
    );
}

#[test]
fn sidechain_listen_replaces_the_output_with_the_key() {
    let input = dc(-20.0, 48_000);
    let key = sine(500.0, SR, 48_000);
    let mut c = fast_compressor(&[
        (comp::SIDECHAIN, 1.0),
        (comp::SIDECHAIN + common::SC_LISTEN, 1.0),
    ]);
    let out = render_keyed(&mut c, &input, &key);
    assert_near(
        *out.last().unwrap(),
        *key.last().unwrap(),
        1e-5,
        "the key, not the signal",
    );
}

// ---------------------------------------------------------------- Gate

fn gate_with(extra: &[(u32, f64)]) -> gate::Gate {
    let mut g: gate::Gate = ready();
    set(
        &mut g,
        &[
            (gate::THRESHOLD, -20.0),
            (gate::RETURN, 0.0),
            (gate::ATTACK, 0.02),
            (gate::HOLD, 1.0),
            (gate::RELEASE, 1.0),
            (gate::FLOOR, gate::FLOOR_INF_DB),
        ],
    );
    set(&mut g, extra);
    g
}

#[test]
fn gate_passes_what_is_over_its_threshold_and_mutes_the_rest() {
    let mut g = gate_with(&[]);
    assert_near(settled_db(&mut g, &dc(-30.0, 48_000)), -240.0, 1.0, "under");
    let mut g = gate_with(&[]);
    assert_near(settled_db(&mut g, &dc(-10.0, 48_000)), -10.0, 0.01, "over");
}

#[test]
fn gate_floor_sets_how_much_a_closed_gate_lets_through() {
    let mut g = gate_with(&[(gate::FLOOR, -20.0)]);
    assert_near(
        settled_db(&mut g, &dc(-30.0, 48_000)),
        -50.0,
        0.05,
        "attenuated",
    );
    let mut g = gate_with(&[(gate::FLOOR, 0.0)]);
    assert_near(
        settled_db(&mut g, &dc(-30.0, 48_000)),
        -30.0,
        0.01,
        "no effect",
    );
}

#[test]
fn gate_flip_passes_only_what_is_under_the_threshold() {
    let mut g = gate_with(&[(gate::FLIP, 1.0)]);
    assert_near(
        settled_db(&mut g, &dc(-30.0, 48_000)),
        -30.0,
        0.01,
        "under passes",
    );
    let mut g = gate_with(&[(gate::FLIP, 1.0)]);
    assert_near(
        settled_db(&mut g, &dc(-10.0, 48_000)),
        -240.0,
        1.0,
        "over is muted",
    );
}

#[test]
fn gate_return_keeps_it_open_until_the_level_falls_past_the_lower_line() {
    let hold = gate_with(&[(gate::THRESHOLD, -20.0), (gate::RETURN, 6.0)]);
    let mut g = hold;
    let mut input = dc(-15.0, 24_000);
    input.extend(dc(-23.0, 24_000));
    let out = mono(&mut g, &input);
    assert_near(
        db(*out.last().unwrap()),
        -23.0,
        0.05,
        "stays open between the lines",
    );

    let mut g = gate_with(&[(gate::THRESHOLD, -20.0), (gate::RETURN, 6.0)]);
    let mut input = dc(-15.0, 24_000);
    input.extend(dc(-30.0, 24_000));
    let out = mono(&mut g, &input);
    assert!(
        out.last().unwrap().abs() < 1e-6,
        "closes below the lower line"
    );

    let mut g = gate_with(&[(gate::THRESHOLD, -20.0), (gate::RETURN, 6.0)]);
    let out = mono(&mut g, &dc(-23.0, 24_000));
    assert!(
        out.last().unwrap().abs() < 1e-6,
        "doesn't open at a level below the threshold"
    );
}

/// Where a falling level first passes `crossing`: the detector decays from `from` to `to`.
fn crossing_frames(from: f32, to: f32, crossing: f32) -> f32 {
    let t = gate::DETECT_RELEASE * ((from - to) / (crossing - to)).ln();
    t * SR as f32
}

#[test]
fn gate_stays_open_for_hold_after_the_signal_falls_then_releases() {
    let (hold_ms, release_ms) = (50.0, 20.0);
    let mut g = gate_with(&[
        (gate::THRESHOLD, -20.0),
        (gate::HOLD, hold_ms),
        (gate::RELEASE, release_ms),
    ]);
    let (loud, quiet) = (gain(-10.0), gain(-30.0));
    let fall_at = 24_000;
    let mut input = dc(-10.0, fall_at);
    input.extend(dc(-30.0, 24_000));
    let out = mono(&mut g, &input);

    let detect = crossing_frames(loud, quiet, gain(-20.0));
    let closes_at = fall_at as f32 + detect + hold_ms as f32 * 48.0;
    let still_open = (closes_at - 100.0) as usize;
    assert_near(out[still_open] / quiet, 1.0, 0.01, "open through the hold");
    // Half way down its release.
    let half = (closes_at + release_ms as f32 * 24.0) as usize;
    assert_near(out[half] / quiet, 0.5, 0.05, "half closed");
    let closed = (closes_at + release_ms as f32 * 48.0 + 96.0) as usize;
    assert!(out[closed].abs() < 1e-6, "closed");
}

#[test]
fn gate_attack_opens_it_linearly_over_its_time() {
    let attack_ms = 10.0;
    let mut g = gate_with(&[(gate::ATTACK, attack_ms)]);
    let mut input = vec![0.0; 4_800];
    input.extend(dc(-10.0, 4_800));
    let out = mono(&mut g, &input);
    let level = gain(-10.0);
    let halfway = 4_800 + (attack_ms as f32 * 24.0) as usize;
    assert_near(out[halfway] / level, 0.5, 0.03, "half open");
}

#[test]
fn gate_lookahead_opens_before_the_audio_arrives_and_reports_its_latency() {
    let mut step = vec![0.0; 4_800];
    step.extend(dc(-10.0, 4_800));
    for (index, frames) in [(0.0, 0usize), (1.0, 48), (2.0, 480)] {
        let mut g = gate_with(&[(gate::LOOKAHEAD, index), (gate::ATTACK, 1.0)]);
        assert_eq!(g.latency() as usize, frames, "declared");
        let out = mono(&mut g, &step);
        let at = out[4_800 + frames] / gain(-10.0);
        if frames == 0 {
            assert!(at < 0.5, "no lookahead starts closed: {at}");
        } else {
            assert_near(at, 1.0, 0.02, "open by the time the transient plays");
        }
        let mut click = vec![0.0; BLOCK * 2];
        click[10] = 0.5;
        let mut g2 = gate_with(&[(gate::LOOKAHEAD, index)]);
        let out = mono(&mut g2, &click);
        assert_eq!(onset(&out, 1e-3), Some(10 + frames), "measured");
    }
}

#[test]
fn gate_keys_off_its_sidechain() {
    let input = dc(-40.0, 48_000);
    let mut g = gate_with(&[(gate::SIDECHAIN, 1.0)]);
    let keyed = render_keyed(&mut g, &input, &dc(-10.0, 48_000));
    assert_near(
        db(*keyed.last().unwrap()),
        -40.0,
        0.01,
        "a loud key opens it",
    );
    let mut g = gate_with(&[(gate::SIDECHAIN, 1.0)]);
    let shut = render_keyed(&mut g, &dc(-10.0, 48_000), &vec![0.0; 48_000]);
    assert!(shut.last().unwrap().abs() < 1e-6, "a silent key shuts it");
    let mut g = gate_with(&[]);
    let own = settled_db(&mut g, &input);
    assert_near(own, -240.0, 1.0, "with no route it hears itself");
}

// ---------------------------------------------------------------- Glue Compressor

fn glue_with(extra: &[(u32, f64)]) -> glue::GlueCompressor {
    let mut c: glue::GlueCompressor = ready();
    set(
        &mut c,
        &[
            (glue::ATTACK, 0.0),
            (glue::RELEASE, 0.0),
            (glue::THRESHOLD, -20.0),
        ],
    );
    set(&mut c, extra);
    c
}

#[test]
fn glue_compresses_by_its_ratio_above_the_knee() {
    for (index, ratio) in [(0.0, 2.0f32), (1.0, 4.0), (2.0, 10.0)] {
        let mut c = glue_with(&[(glue::RATIO, index)]);
        let reduced = settled_db(&mut c, &dc(0.0, 48_000));
        let expected = -(1.0 - 1.0 / ratio) * 20.0;
        assert_near(reduced, expected, 0.05, "above the knee");
    }
}

#[test]
fn glue_knee_gets_sharper_as_the_ratio_rises() {
    assert!(glue::KNEES_DB.windows(2).all(|w| w[0] > w[1]));
    let at_threshold = |index: f64| {
        let mut c = glue_with(&[(glue::RATIO, index)]);
        settled_db(&mut c, &dc(-20.0, 48_000))
    };
    let (soft, hard) = (at_threshold(0.0), at_threshold(2.0));
    assert!(soft < 0.0 - 0.01 - 20.0 + 19.0, "soft {soft}");
    assert!(soft < hard, "ratio 2 rounds the corner more: {soft} {hard}");
}

#[test]
fn glue_range_limits_how_much_compression_can_happen() {
    let mut c = glue_with(&[(glue::THRESHOLD, -30.0), (glue::RANGE, -3.0)]);
    assert_near(settled_db(&mut c, &dc(0.0, 48_000)), -3.0, 0.05, "range");
    let mut c = glue_with(&[(glue::THRESHOLD, -30.0), (glue::RANGE, 0.0)]);
    assert_near(
        settled_db(&mut c, &dc(0.0, 48_000)),
        0.0,
        0.01,
        "no compression",
    );
}

#[test]
fn glue_makeup_and_mix_apply_afterward() {
    let mut c = glue_with(&[(glue::MAKEUP, 6.0)]);
    let expect = -(1.0 - 1.0 / 4.0) * 20.0 + 6.0;
    assert_near(settled_db(&mut c, &dc(0.0, 48_000)), expect, 0.05, "makeup");
    let mut c = glue_with(&[(glue::MIX, 0.0)]);
    assert_near(settled_db(&mut c, &dc(-3.0, 48_000)), -3.0, 0.01, "dry");
}

#[test]
fn glue_attack_steps_follow_their_time_constants() {
    let mut c = glue_with(&[
        (glue::ATTACK, 5.0), // 10 ms
        (glue::THRESHOLD, -40.0),
        (glue::RATIO, 0.0),
    ]);
    let out = mono(&mut c, &dc(-10.0, 4_800));
    let target = 0.5 * 30.0f32;
    assert_near(-10.0 - db(out[480]), target * 0.632, 0.2, "63%");
}

#[test]
fn glue_auto_release_has_a_slow_base_under_a_fast_reaction() {
    // After a short burst the fast part alone lets go quickly, after a long one the slow part
    // has built up and carries the reduction for longer.
    let time_to_recover = |burst: usize| {
        let mut c = glue_with(&[
            (glue::ATTACK, 2.0),
            (glue::RELEASE, glue::RELEASE_AUTO as f64),
            (glue::THRESHOLD, -30.0),
        ]);
        let mut input = dc(-6.0, burst);
        input.extend(dc(-40.0, 240_000));
        let out = mono(&mut c, &input);
        out[burst..]
            .iter()
            .position(|s| (db(*s) + 40.0).abs() < 1.0)
            .expect("recovers")
    };
    let (short, long) = (time_to_recover(1_200), time_to_recover(96_000));
    assert!(long > short * 3, "short {short} long {long}");
}

#[test]
fn glue_soft_clip_holds_the_output_under_its_ceiling() {
    let loud: Vec<f32> = sine(100.0, SR, 48_000).iter().map(|s| s * 8.0).collect();
    let mut c = glue_with(&[(glue::SOFT, 1.0), (glue::THRESHOLD, 0.0)]);
    let out = mono(&mut c, &loud);
    assert!(peak(&out) <= glue::SOFT_CEILING + 1e-3, "{}", peak(&out));
}

#[test]
fn glue_oversampling_reports_its_exact_latency_and_keeps_dry_aligned() {
    let mut c = glue_with(&[(glue::OVERSAMPLING, 1.0)]);
    let declared = c.latency() as usize;
    assert!(declared > 0, "oversampling adds latency");
    let mut click = vec![0.0; BLOCK * 2];
    click[10] = 0.5;
    let out = mono(&mut c, &click);
    // The linear-phase filters ring ahead of the click: the delay is where its peak lands.
    assert_eq!(peak_index(&out), 10 + declared, "wet");
    let mut c = glue_with(&[(glue::OVERSAMPLING, 1.0), (glue::MIX, 0.0)]);
    let out = mono(&mut c, &click);
    assert_eq!(onset(&out, 1e-3), Some(10 + declared), "dry");
    assert_near(out[10 + declared], 0.5, 1e-6, "dry is the input delayed");
}

#[test]
fn glue_oversampling_leaves_a_quiet_signal_alone() {
    let tone: Vec<f32> = sine(1_000.0, SR, 48_000).iter().map(|s| s * 0.01).collect();
    let mut c = glue_with(&[(glue::OVERSAMPLING, 1.0)]);
    let out = mono(&mut c, &tone);
    let d = c.latency() as usize;
    for (out, input) in out[d + 1_000..].iter().zip(&tone[1_000..]) {
        assert_near(*out, *input, 1e-4, "unity");
    }
}

// ---------------------------------------------------------------- Limiter

/// Something loud with both sharp transients and sustain.
fn hot_material(frames: usize) -> Vec<f32> {
    let bass = sine(55.0, SR, frames);
    let mid = sine(1_300.0, SR, frames);
    (0..frames)
        .map(|i| {
            let click = if i % 3_000 < 4 { 3.0 } else { 0.0 };
            bass[i] * 1.5 + mid[i] * 0.8 + click
        })
        .collect()
}

#[test]
fn limiter_output_never_exceeds_its_ceiling() {
    let hot = hot_material(48_000);
    let other: Vec<f32> = hot.iter().rev().copied().collect();
    for ceiling in [0.0, -0.3, -6.0, -18.0] {
        for lookahead in [0.0, 1.0, 2.0] {
            for routing in [lim::ROUTING_LR, lim::ROUTING_MS] {
                for link in [0.0, 0.5, 1.0] {
                    let mut l: lim::Limiter = ready();
                    set(
                        &mut l,
                        &[
                            (lim::GAIN, 12.0),
                            (lim::CEILING, ceiling),
                            (lim::LOOKAHEAD, lookahead),
                            (lim::ROUTING, routing),
                            (lim::LINK, link),
                            (lim::RELEASE, 5.0),
                        ],
                    );
                    let (left, right) = render(&mut l, &hot, &other);
                    let max = gain(ceiling as f32);
                    for s in left.iter().chain(&right) {
                        assert!(
                            s.abs() <= max + 1e-6,
                            "ceiling {ceiling} look {lookahead} routing {routing} link {link}: {s}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn limiter_leaves_a_signal_under_its_ceiling_alone() {
    let tone: Vec<f32> = sine(440.0, SR, 48_000).iter().map(|s| s * 0.5).collect();
    let mut l: lim::Limiter = ready();
    let out = mono(&mut l, &tone);
    let d = l.latency() as usize;
    for i in 0..tone.len() - d {
        assert_near(out[i + d], tone[i], 1e-6, "unity");
    }
}

#[test]
fn limiter_lookahead_stops_a_single_sample_spike_outright() {
    let mut spike = vec![0.2f32; 9_600];
    spike[4_800] = 4.0;
    let mut l: lim::Limiter = ready();
    set(&mut l, &[(lim::CEILING, -6.0), (lim::LOOKAHEAD, 2.0)]);
    let out = mono(&mut l, &spike);
    assert!(peak(&out) <= gain(-6.0) + 1e-6, "{}", peak(&out));
}

fn latency_with(params: &[(u32, f64)]) -> (u32, usize) {
    let mut l: lim::Limiter = ready();
    set(&mut l, params);
    let d = l.latency();
    let mut click = vec![0.0; 2 * BLOCK];
    click[10] = 0.25;
    let out = mono(&mut l, &click);
    (d, peak_index(&out))
}

#[test]
fn limiter_latency_is_exact_for_every_lookahead_and_mode() {
    let cases: &[(f64, f64, f64, usize)] = &[
        (0.0, lim::ROUTING_LR, lim::MODE_STANDARD, 72),
        (1.0, lim::ROUTING_LR, lim::MODE_STANDARD, 144),
        (2.0, lim::ROUTING_LR, lim::MODE_STANDARD, 288),
        (1.0, lim::ROUTING_MS, lim::MODE_STANDARD, 288),
        (1.0, lim::ROUTING_LR, lim::MODE_TRUE_PEAK, 144 + 42),
    ];
    for &(lookahead, routing, mode, expected) in cases {
        let (declared, measured) = latency_with(&[
            (lim::LOOKAHEAD, lookahead),
            (lim::ROUTING, routing),
            (lim::MODE, mode),
        ]);
        assert_eq!(declared as usize, expected, "declared");
        assert_eq!(measured, 10 + expected, "measured");
    }
}

#[test]
fn limiter_default_latency_stays_inside_a_block() {
    let l: lim::Limiter = ready();
    assert!((l.latency() as usize) < BLOCK, "{}", l.latency());
}

#[test]
fn limiter_link_decides_whether_the_other_channel_ducks() {
    let left = dc(6.0, 24_000);
    let right = dc(-20.0, 24_000);
    let run = |link: f64| {
        let mut l: lim::Limiter = ready();
        set(&mut l, &[(lim::CEILING, 0.0), (lim::LINK, link)]);
        let (a, b) = render(&mut l, &left, &right);
        (db(*a.last().unwrap()), db(*b.last().unwrap()))
    };
    let (unlinked_l, unlinked_r) = run(0.0);
    assert_near(unlinked_l, 0.0, 0.05, "L is limited");
    assert_near(unlinked_r, -20.0, 0.05, "R is left alone at 0%");
    let (linked_l, linked_r) = run(1.0);
    assert_near(linked_l, 0.0, 0.05, "L");
    assert_near(linked_r, -26.0, 0.1, "R takes L's reduction at 100%");
}

#[test]
fn limiter_maximize_lifts_by_the_threshold_and_caps_at_output() {
    let run = |threshold: f64, output: f64, level: f32| {
        let mut l: lim::Limiter = ready();
        set(
            &mut l,
            &[
                (lim::MAXIMIZE, 1.0),
                (lim::THRESHOLD, threshold),
                (lim::OUTPUT, output),
            ],
        );
        settled_db(&mut l, &dc(level, 48_000))
    };
    assert_near(
        run(-6.0, 0.0, -12.0),
        -6.0,
        0.05,
        "threshold lifts the level",
    );
    assert_near(run(-6.0, 0.0, -3.0), 0.0, 0.05, "and limits at full scale");
    assert_near(run(-12.0, -3.0, 0.0), -3.0, 0.05, "output sets the top");
}

#[test]
fn limiter_soft_clip_stays_under_the_ceiling() {
    let hot = hot_material(48_000);
    let mut l: lim::Limiter = ready();
    set(
        &mut l,
        &[
            (lim::GAIN, 12.0),
            (lim::CEILING, -6.0),
            (lim::MODE, lim::MODE_SOFT_CLIP),
        ],
    );
    let out = mono(&mut l, &hot);
    assert!(peak(&out) <= gain(-6.0) + 1e-6, "{}", peak(&out));
}

#[test]
fn limiter_soft_clip_curve_leaves_quiet_samples_alone() {
    assert_eq!(lim::soft_clip(0.3, 1.0), (0.3, false));
    let (y, clipped) = lim::soft_clip(0.95, 1.0);
    assert!(clipped && y < 0.95 && y > 0.7, "{y}");
    assert!(lim::soft_clip(50.0, 1.0).0 <= 1.0, "never past the ceiling");
}

#[test]
fn limiter_release_lets_the_gain_recover() {
    let mut l: lim::Limiter = ready();
    set(&mut l, &[(lim::CEILING, -6.0), (lim::RELEASE, 50.0)]);
    let mut input = vec![0.0f32; 48_000];
    input[10_000] = 1.0;
    input[10_001..].fill(0.25);
    let out = mono(&mut l, &input);
    let d = l.latency() as usize;
    // Right after the spike the steady level is still reduced; a long while later it isn't.
    let early = out[10_000 + d + 96];
    let late = out[47_000];
    assert!(early < 0.25 - 0.01, "still reduced: {early}");
    assert_near(late, 0.25, 0.002, "recovered");
    assert!(early > 0.05, "not stuck: {early}");
}

#[test]
fn limiter_auto_release_runs_and_stays_under_the_ceiling() {
    let hot = hot_material(24_000);
    let mut l: lim::Limiter = ready();
    set(
        &mut l,
        &[(lim::GAIN, 12.0), (lim::AUTO, 1.0), (lim::CEILING, -3.0)],
    );
    let out = mono(&mut l, &hot);
    assert!(peak(&out) <= gain(-3.0) + 1e-6, "{}", peak(&out));
}

/// The inter-sample peak of a buffer, measured by upsampling it eight times.
fn true_peak(input: &[f32]) -> f32 {
    let mut os = Oversampler::new(OversampleFactor::X8, input.len());
    let mut out = vec![0.0; input.len()];
    let mut max = 0.0f32;
    os.process(input, &mut out, |hi| {
        max = hi.iter().fold(max, |m, s| m.max(s.abs()));
    });
    max
}

/// A quarter-rate sine at 45 degrees: every sample reads 0.707, but the wave peaks at 1.
fn intersample_tone(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let phase = (i as f32) * std::f32::consts::FRAC_PI_2 + std::f32::consts::FRAC_PI_4;
            phase.sin() * 1.3
        })
        .collect()
}

#[test]
fn true_peak_mode_stops_overs_that_standard_mode_cannot_see() {
    let tone = intersample_tone(48_000);
    let run = |mode: f64| {
        let mut l: lim::Limiter = ready();
        set(&mut l, &[(lim::CEILING, -3.0), (lim::MODE, mode)]);
        let out = mono(&mut l, &tone);
        true_peak(&out[8_000..])
    };
    let ceiling = gain(-3.0);
    let standard = run(lim::MODE_STANDARD);
    assert!(
        standard > ceiling * 1.02,
        "standard only sees samples: {standard}"
    );
    let true_peak_mode = run(lim::MODE_TRUE_PEAK);
    assert!(
        true_peak_mode <= ceiling * 1.003,
        "true peak mode: {true_peak_mode} over {ceiling}"
    );
}

#[test]
fn true_peak_mode_never_exceeds_the_ceiling_on_transients() {
    let hot = hot_material(48_000);
    let mut l: lim::Limiter = ready();
    set(
        &mut l,
        &[
            (lim::GAIN, 12.0),
            (lim::CEILING, -3.0),
            (lim::MODE, lim::MODE_TRUE_PEAK),
        ],
    );
    let out = mono(&mut l, &hot);
    assert!(
        peak(&out) <= gain(-3.0) + 1e-6,
        "sample peak: {}",
        peak(&out)
    );
    let tp = true_peak(&out[4_000..]);
    assert!(tp <= gain(-3.0) * 1.003, "true peak: {tp}");
}

// ---------------------------------------------------------------- Multiband Dynamics

fn multiband(extra: &[(u32, f64)]) -> mb::MultibandDynamics {
    let mut m: mb::MultibandDynamics = ready();
    set(&mut m, &[(mb::LOW_FREQ, 200.0), (mb::HIGH_FREQ, 2_000.0)]);
    set(&mut m, extra);
    m
}

fn band_id(band: usize, offset: u32) -> u32 {
    mb::band_param(band, offset)
}

/// The steady output amplitude of a tone.
fn tone_out(m: &mut dyn Processor, hz: f64) -> f32 {
    let tone = sine(hz as f32, SR, 96_000);
    let out = mono(m, &tone);
    tone_amplitude(&out[48_000..], hz, SR) as f32
}

#[test]
fn multiband_split_sums_flat_at_unity() {
    for hz in [
        30.0, 80.0, 150.0, 200.0, 400.0, 1_000.0, 2_000.0, 5_000.0, 12_000.0,
    ] {
        let mut m = multiband(&[]);
        assert_near(tone_out(&mut m, hz), 1.0, 0.002, "flat");
    }
}

#[test]
fn multiband_stays_flat_with_either_or_both_splits_off() {
    for (low, high) in [(0.0, 1.0), (1.0, 0.0), (0.0, 0.0)] {
        for hz in [80.0, 1_000.0, 6_000.0] {
            let mut m = multiband(&[(mb::LOW_ON, low), (mb::HIGH_ON, high)]);
            assert_near(tone_out(&mut m, hz), 1.0, 0.002, "flat");
        }
    }
}

fn band_mid_ratio(extra: &[(u32, f64)], hz: f64, level_db: f32) -> f32 {
    let tone: Vec<f32> = sine(hz as f32, SR, 96_000)
        .iter()
        .map(|s| s * gain(level_db))
        .collect();
    // The neighbouring bands take no gain, so the crossovers sit far from the tone: what they
    // leak would otherwise be heard as a different result.
    let mut m = multiband(&[
        (mb::LOW_FREQ, 30.0),
        (mb::HIGH_FREQ, 18_000.0),
        (mb::DETECTION, mb::DETECT_PEAK),
        (band_id(mb::MID, mb::B_ATTACK), 0.1),
        (band_id(mb::MID, mb::B_RELEASE), 3_000.0),
    ]);
    set(&mut m, extra);
    let out = mono(&mut m, &tone);
    db(tone_amplitude(&out[48_000..], hz, SR) as f32)
}

#[test]
fn multiband_above_compresses_with_a_ratio_over_one() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -30.0),
            (band_id(mb::MID, mb::B_ABOVE_RATIO), 2.0),
        ],
        1_000.0,
        -6.0,
    );
    assert_near(out, -6.0 - 0.5 * 24.0, 0.3, "downward compression");
}

#[test]
fn multiband_above_expands_upward_with_a_ratio_under_one() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -24.0),
            (band_id(mb::MID, mb::B_ABOVE_RATIO), 0.8),
        ],
        1_000.0,
        -12.0,
    );
    assert_near(out, -12.0 + 0.25 * 12.0, 0.3, "upward expansion");
}

#[test]
fn multiband_below_expands_downward_with_a_ratio_over_one() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_BELOW_THRESHOLD), -40.0),
            (band_id(mb::MID, mb::B_BELOW_RATIO), 2.0),
            (band_id(mb::MID, mb::B_RELEASE), 10.0),
        ],
        1_000.0,
        -50.0,
    );
    assert_near(out, -60.0, 0.5, "downward expansion");
}

#[test]
fn multiband_below_compresses_upward_with_a_ratio_under_one() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_BELOW_THRESHOLD), -40.0),
            (band_id(mb::MID, mb::B_BELOW_RATIO), 0.5),
            (band_id(mb::MID, mb::B_RELEASE), 10.0),
        ],
        1_000.0,
        -50.0,
    );
    assert_near(out, -45.0, 0.5, "upward compression");
}

#[test]
fn multiband_upward_gain_is_capped() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_BELOW_THRESHOLD), -10.0),
            (band_id(mb::MID, mb::B_BELOW_RATIO), 0.1),
            (band_id(mb::MID, mb::B_RELEASE), 10.0),
        ],
        1_000.0,
        -60.0,
    );
    assert_near(out, -60.0 + common::MAX_UPWARD_DB, 0.5, "capped");
}

#[test]
fn multiband_amount_zero_has_an_effective_ratio_of_one() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -30.0),
            (band_id(mb::MID, mb::B_ABOVE_RATIO), 4.0),
            (mb::AMOUNT, 0.0),
        ],
        1_000.0,
        -6.0,
    );
    assert_near(out, -6.0, 0.01, "amount 0");
}

#[test]
fn multiband_soft_knee_starts_early() {
    let at_threshold = |soft: f64| {
        band_mid_ratio(
            &[
                (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -12.0),
                (band_id(mb::MID, mb::B_ABOVE_RATIO), 4.0),
                (mb::SOFT_KNEE, soft),
                // A slow release would hold on to the filters' start-up overshoot.
                (band_id(mb::MID, mb::B_RELEASE), 10.0),
            ],
            1_000.0,
            -12.0,
        )
    };
    assert_near(at_threshold(0.0), -12.0, 0.05, "hard knee at the threshold");
    let expected = -0.75 * (mb::KNEE_DB / 2.0).powi(2) / (2.0 * mb::KNEE_DB);
    assert_near(
        at_threshold(1.0),
        -12.0 + expected,
        0.2,
        "soft knee at the threshold",
    );
}

#[test]
fn multiband_rms_reads_a_sine_lower_than_peak() {
    let hard = |detect: f64| {
        band_mid_ratio(
            &[
                (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -30.0),
                (band_id(mb::MID, mb::B_ABOVE_RATIO), 2.0),
                (mb::DETECTION, detect),
            ],
            1_000.0,
            -6.0,
        )
    };
    let (peak, rms) = (hard(mb::DETECT_PEAK), hard(mb::DETECT_RMS));
    assert!(rms > peak + 1.0, "peak {peak} rms {rms}");
}

#[test]
fn multiband_time_scales_every_attack_and_release() {
    // A step's reduction after the band's attack time, at 1x and 3x Time.
    let measure = |time: f64| {
        let tone: Vec<f32> = sine(1_000.0, SR, 9_600).iter().map(|s| s * 0.25).collect();
        let mut m = multiband(&[
            (mb::TIME, time),
            (mb::DETECTION, mb::DETECT_PEAK),
            (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -40.0),
            (band_id(mb::MID, mb::B_ABOVE_RATIO), 2.0),
            (band_id(mb::MID, mb::B_ATTACK), 2.0),
            (band_id(mb::MID, mb::B_RELEASE), 1_000.0),
        ]);
        let out = mono(&mut m, &tone);
        db(tone_amplitude(&out[200..600], 1_000.0, SR) as f32)
    };
    let (fast, slow) = (measure(1.0), measure(3.0));
    assert!(fast < slow - 1.0, "fast {fast} slow {slow}");
}

#[test]
fn multiband_input_and_output_gains_apply_per_band() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_INPUT), 6.0),
            (band_id(mb::MID, mb::B_OUTPUT), -3.0),
        ],
        1_000.0,
        -20.0,
    );
    assert_near(out, -17.0, 0.05, "mid band gains");
    let mut m = multiband(&[(band_id(mb::LOW, mb::B_INPUT), 6.0)]);
    assert_near(
        db(tone_out(&mut m, 1_000.0)),
        0.0,
        0.05,
        "a band's gain stays in its band",
    );
}

#[test]
fn multiband_output_is_global() {
    let mut m = multiband(&[(mb::OUTPUT, 6.0)]);
    assert_near(db(tone_out(&mut m, 1_000.0)), 6.0, 0.05, "global");
}

#[test]
fn multiband_activator_bypasses_a_band() {
    let out = band_mid_ratio(
        &[
            (band_id(mb::MID, mb::B_ON), 0.0),
            (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -30.0),
            (band_id(mb::MID, mb::B_ABOVE_RATIO), 4.0),
            (band_id(mb::MID, mb::B_INPUT), 12.0),
        ],
        1_000.0,
        -6.0,
    );
    assert_near(out, -6.0, 0.01, "a band that's off isn't processed");
}

#[test]
fn multiband_solo_mutes_the_other_bands() {
    let mut m = multiband(&[(band_id(mb::LOW, mb::B_SOLO), 1.0)]);
    assert_near(tone_out(&mut m, 60.0), 1.0, 0.02, "soloed band plays");
    let mut m = multiband(&[(band_id(mb::LOW, mb::B_SOLO), 1.0)]);
    assert!(tone_out(&mut m, 6_000.0) < 0.01, "others are muted");
}

#[test]
fn multiband_solo_on_a_switched_off_band_mutes_nothing() {
    let mut m = multiband(&[(band_id(mb::LOW, mb::B_SOLO), 1.0), (mb::LOW_ON, 0.0)]);
    assert_near(tone_out(&mut m, 1_000.0), 1.0, 0.01, "still plays");
}

#[test]
fn multiband_crossovers_split_the_signal_where_they_say() {
    let mut m = multiband(&[(band_id(mb::MID, mb::B_SOLO), 1.0)]);
    assert_near(tone_out(&mut m, 700.0), 1.0, 0.03, "mid plays");
    let mut m = multiband(&[(band_id(mb::MID, mb::B_SOLO), 1.0)]);
    assert!(tone_out(&mut m, 30.0) < 0.01, "below the low crossover");
    let mut m = multiband(&[(band_id(mb::MID, mb::B_SOLO), 1.0)]);
    assert!(
        tone_out(&mut m, 12_000.0) < 0.02,
        "above the high crossover"
    );
    // A crossover is -6 dB in each band.
    let mut m = multiband(&[(band_id(mb::HIGH, mb::B_SOLO), 1.0)]);
    assert_near(
        tone_out(&mut m, 2_000.0),
        0.5,
        0.03,
        "-6 dB at the crossover",
    );
}

#[test]
fn multiband_keys_off_its_sidechain() {
    let input: Vec<f32> = sine(1_000.0, SR, 96_000).iter().map(|s| s * 0.1).collect();
    let build = || {
        multiband(&[
            (mb::DETECTION, mb::DETECT_PEAK),
            (band_id(mb::MID, mb::B_ATTACK), 0.1),
            (band_id(mb::MID, mb::B_RELEASE), 1_000.0),
            (band_id(mb::MID, mb::B_ABOVE_THRESHOLD), -30.0),
            (band_id(mb::MID, mb::B_ABOVE_RATIO), 4.0),
            (mb::SIDECHAIN, 1.0),
        ])
    };
    let silence = vec![0.0; 96_000];
    let mut m = build();
    let idle = render_keyed(&mut m, &input, &silence);
    let level = |out: &[f32]| db(tone_amplitude(&out[48_000..], 1_000.0, SR) as f32);
    assert_near(level(&idle), -20.0, 0.1, "a silent key does nothing");
    let loud: Vec<f32> = sine(1_000.0, SR, 96_000);
    let mut m = build();
    let keyed = render_keyed(&mut m, &input, &loud);
    assert!(
        level(&keyed) < -30.0,
        "a loud key ducks the band: {}",
        level(&keyed)
    );
}

#[test]
fn multiband_listen_outputs_the_key() {
    let input = vec![0.0f32; 48_000];
    let key = sine(1_000.0, SR, 48_000);
    let mut m = multiband(&[
        (mb::SIDECHAIN, 1.0),
        (mb::SIDECHAIN + common::SC_LISTEN, 1.0),
    ]);
    let out = render_keyed(&mut m, &input, &key);
    assert_near(*out.last().unwrap(), *key.last().unwrap(), 1e-5, "the key");
}

// ---------------------------------------------------------------- Drum Buss

fn buss(extra: &[(u32, f64)]) -> db::DrumBuss {
    let mut d: db::DrumBuss = ready();
    set(&mut d, extra);
    d
}

/// A short drum-like hit at `hz`: an instant rise and an exponential decay.
fn hit(hz: f32, level: f32, tau_ms: f32, frames: usize) -> Vec<f32> {
    let tone = sine(hz, SR, frames);
    tone.iter()
        .enumerate()
        .map(|(i, s)| s * level * (-(i as f32) / (tau_ms * 48.0)).exp())
        .collect()
}

#[test]
fn drum_buss_trim_turns_the_input_down_first() {
    let mut d = buss(&[(db::TRIM, -12.0)]);
    let level = settled_db(&mut d, &dc(-12.0, 48_000));
    // Trim lands before the waveshaper, so -24 dB in is well inside its linear range.
    assert_near(level, -24.0, 0.1, "trimmed");
}

#[test]
fn drum_buss_comp_compresses_with_fixed_ratio_and_makeup() {
    let level = -6.0f32;
    // After the compressor the level lands on the fixed curve, then the waveshaper.
    let gr = (1.0 - 1.0 / db::COMP_RATIO) * (level - db::COMP_THRESHOLD_DB);
    let after = gain(level - gr + db::COMP_MAKEUP_DB).tanh();
    let mut d = buss(&[(db::COMP, 1.0)]);
    assert_near(
        settled_db(&mut d, &dc(level, 48_000)),
        db(after),
        0.05,
        "comp",
    );
}

/// Distortion of a tone: third, fifth and seventh harmonics against the fundamental.
fn thd(output: &[f32], hz: f64) -> f64 {
    let fundamental = tone_amplitude(output, hz, SR);
    let harmonics: f64 = [3.0, 5.0, 7.0]
        .iter()
        .map(|h| tone_amplitude(output, hz * h, SR).powi(2))
        .sum();
    harmonics.sqrt() / fundamental
}

#[test]
fn drum_buss_distortion_types_are_progressively_harder() {
    let tone: Vec<f32> = sine(440.0, SR, 48_000).iter().map(|s| s * 0.25).collect();
    let distortion = |kind: f64| {
        let mut d = buss(&[(db::DISTORTION, kind), (db::DRIVE, 12.0)]);
        let out = mono(&mut d, &tone);
        thd(&out[8_000..], 440.0)
    };
    let (soft, medium, hard) = (
        distortion(db::DIST_SOFT),
        distortion(db::DIST_MEDIUM),
        distortion(db::DIST_HARD),
    );
    assert!(soft < medium, "soft {soft} medium {medium}");
    assert!(medium < hard, "medium {medium} hard {hard}");
}

#[test]
fn drum_buss_hard_mode_boosts_the_bass() {
    let tone: Vec<f32> = sine(60.0, SR, 96_000).iter().map(|s| s * 0.05).collect();
    let level = |kind: f64| {
        let mut d = buss(&[(db::DISTORTION, kind)]);
        let out = mono(&mut d, &tone);
        db(tone_amplitude(&out[48_000..], 60.0, SR) as f32)
    };
    let soft = level(db::DIST_SOFT);
    let hard = level(db::DIST_HARD);
    assert!(hard > soft + 3.0, "soft {soft} hard {hard}");
}

#[test]
fn drum_buss_crunch_distorts_only_the_mid_highs() {
    let distortion = |hz: f32, crunch: f64| {
        let tone: Vec<f32> = sine(hz, SR, 96_000).iter().map(|s| s * 0.3).collect();
        let mut d = buss(&[(db::CRUNCH, crunch)]);
        let out = mono(&mut d, &tone);
        thd(&out[48_000..], hz as f64)
    };
    assert!(
        distortion(3_000.0, 1.0) > distortion(3_000.0, 0.0) * 10.0,
        "highs get crunchy"
    );
    let lows = (distortion(60.0, 1.0) - distortion(60.0, 0.0)).abs();
    assert!(lows < 0.02, "lows stay clean: {lows}");
}

#[test]
fn drum_buss_damp_is_a_low_pass() {
    let level = |damp: f64, hz: f64| {
        let tone: Vec<f32> = sine(hz as f32, SR, 96_000)
            .iter()
            .map(|s| s * 0.1)
            .collect();
        let mut d = buss(&[(db::DAMP, damp)]);
        let out = mono(&mut d, &tone);
        db(tone_amplitude(&out[48_000..], hz, SR) as f32)
    };
    assert_near(level(20_000.0, 500.0), -20.0, 0.5, "open");
    assert!(level(1_000.0, 8_000.0) < -40.0, "damped");
    assert_near(level(1_000.0, 200.0), -20.0, 0.5, "below stays");
}

/// The level of the attack and of the tail of a hit, in dB.
fn attack_and_tail(transients: f64) -> (f32, f32) {
    let signal = hit(1_000.0, 0.2, 40.0, 24_000);
    let mut d = buss(&[(db::TRANSIENTS, transients)]);
    let out = mono(&mut d, &signal);
    (db(rms(&out[10..200])), db(rms(&out[4_000..9_000])))
}

#[test]
fn drum_buss_positive_transients_add_attack_and_sustain() {
    let (attack, tail) = attack_and_tail(1.0);
    let (flat_attack, flat_tail) = attack_and_tail(0.0);
    assert!(attack > flat_attack + 0.5, "{attack} vs {flat_attack}");
    assert!(tail > flat_tail + 0.5, "{tail} vs {flat_tail}");
}

#[test]
fn drum_buss_negative_transients_add_attack_but_cut_sustain() {
    let (attack, tail) = attack_and_tail(-1.0);
    let (flat_attack, flat_tail) = attack_and_tail(0.0);
    assert!(attack > flat_attack + 0.5, "{attack} vs {flat_attack}");
    assert!(tail < flat_tail - 0.5, "{tail} vs {flat_tail}");
}

#[test]
fn drum_buss_boom_boosts_at_its_frequency() {
    let level = |boom: f64, hz: f64| {
        let tone: Vec<f32> = sine(hz as f32, SR, 96_000)
            .iter()
            .map(|s| s * 0.05)
            .collect();
        let mut d = buss(&[(db::BOOM, boom), (db::FREQ, 60.0), (db::DECAY, 1.0)]);
        let out = mono(&mut d, &tone);
        tone_amplitude(&out[48_000..], hz, SR) as f32
    };
    let flat = level(0.0, 60.0);
    let boosted = level(1.0, 60.0);
    assert_near(
        boosted / flat,
        1.0 + db::BOOM_GAIN,
        0.5,
        "Boom at resonance",
    );
    assert_near(
        level(1.0, 1_000.0) / level(0.0, 1_000.0),
        1.0,
        0.02,
        "leaves the highs",
    );
}

#[test]
fn drum_buss_force_to_note_lands_on_a_midi_note() {
    assert_eq!(db::boom_frequency(61.0, false), 61.0);
    // MIDI 35 is the nearest note to 61 Hz.
    assert_near(
        db::boom_frequency(61.0, true),
        440.0 * 2f32.powf((35.0 - 69.0) / 12.0),
        0.01,
        "snapped",
    );
    for hz in [30.0f32, 47.0, 73.0, 90.0] {
        let snapped = db::boom_frequency(hz, true);
        let semis = 69.0 + 12.0 * (snapped / 440.0).log2();
        assert_near(semis, semis.round(), 0.001, "a whole note");
    }
}

#[test]
fn drum_buss_decay_shortens_the_low_tail() {
    let signal = hit(60.0, 0.5, 250.0, 72_000);
    let tail = |decay: f64| {
        let mut d = buss(&[(db::DECAY, decay), (db::FREQ, 60.0), (db::BOOM, 1.0)]);
        let out = mono(&mut d, &signal);
        rms(&out[24_000..48_000])
    };
    let (long, short) = (tail(1.0), tail(0.0));
    assert!(short < long * 0.5, "short {short} long {long}");
}

#[test]
fn drum_buss_decay_affects_the_incoming_signal_even_without_boom() {
    let signal = hit(60.0, 0.5, 250.0, 72_000);
    let tail = |decay: f64| {
        let mut d = buss(&[(db::DECAY, decay), (db::FREQ, 60.0), (db::BOOM, 0.0)]);
        let out = mono(&mut d, &signal);
        rms(&out[24_000..48_000])
    };
    assert!(tail(0.0) < tail(1.0) * 0.5);
}

#[test]
fn drum_buss_decay_at_maximum_leaves_a_steady_tone_alone() {
    let tone: Vec<f32> = sine(60.0, SR, 96_000).iter().map(|s| s * 0.05).collect();
    let mut d = buss(&[(db::DECAY, 1.0)]);
    let out = mono(&mut d, &tone);
    let level = db(tone_amplitude(&out[48_000..], 60.0, SR) as f32);
    assert_near(level, db(0.05), 0.1, "flat");
}

#[test]
fn drum_buss_audition_plays_only_the_boom_signal() {
    let tone: Vec<f32> = sine(60.0, SR, 96_000).iter().map(|s| s * 0.05).collect();
    let mut d = buss(&[(db::BOOM, 1.0), (db::AUDITION, 1.0), (db::FREQ, 60.0)]);
    let out = mono(&mut d, &tone);
    let amp = tone_amplitude(&out[48_000..], 60.0, SR) as f32;
    assert_near(amp / 0.05, db::BOOM_GAIN, 0.5, "the boost alone");
    let high: Vec<f32> = sine(4_000.0, SR, 96_000).iter().map(|s| s * 0.05).collect();
    let mut d = buss(&[(db::BOOM, 1.0), (db::AUDITION, 1.0), (db::FREQ, 60.0)]);
    let out = mono(&mut d, &high);
    assert!(
        tone_amplitude(&out[48_000..], 4_000.0, SR) < 1e-3,
        "and nothing else"
    );
}

#[test]
fn drum_buss_dry_wet_and_output() {
    let tone: Vec<f32> = sine(500.0, SR, 48_000).iter().map(|s| s * 0.1).collect();
    let mut d = buss(&[(db::MIX, 0.0), (db::DRIVE, 12.0)]);
    let out = mono(&mut d, &tone);
    for (a, b) in out[24_000..].iter().zip(&tone[24_000..]) {
        assert_near(*a, *b, 1e-4, "dry is untouched");
    }
    let mut d = buss(&[(db::OUTPUT, -12.0)]);
    let out = mono(&mut d, &tone);
    let level = db(tone_amplitude(&out[24_000..], 500.0, SR) as f32);
    assert_near(level, db(0.1) - 12.0, 0.2, "output gain");
}

// ---------------------------------------------------------------- Every device

fn all_devices() -> Vec<(&'static str, Box<dyn Processor>)> {
    let mut devices: Vec<(&'static str, Box<dyn Processor>)> = vec![
        ("compressor", Box::new(ready::<Compressor>())),
        ("gate", Box::new(ready::<gate::Gate>())),
        ("glue", Box::new(ready::<glue::GlueCompressor>())),
        ("limiter", Box::new(ready::<lim::Limiter>())),
        ("multiband", Box::new(ready::<mb::MultibandDynamics>())),
        ("drum buss", Box::new(ready::<db::DrumBuss>())),
    ];
    // Keep the borrow checker out of what follows: `Box<dyn Processor>` is enough.
    for (_, p) in &mut devices {
        p.reset();
    }
    devices
}

/// Sets every parameter of a device description to one end of its range.
fn extreme(p: &mut dyn Processor, info: &rlive_devices::BuiltinInfo, top: bool) {
    for param in (info.desc)().params {
        p.set_param(param.id, if top { param.max } else { param.min });
    }
}

const REGISTRY: &[&str] = &[
    "rlive.compressor",
    "rlive.gate",
    "rlive.glue_compressor",
    "rlive.limiter",
    "rlive.multiband_dynamics",
    "rlive.drum_buss",
];

#[test]
fn every_device_is_registered_as_beta_with_a_layout_of_its_own_parameters() {
    for id in REGISTRY {
        let info = rlive_devices::find(id).unwrap_or_else(|| panic!("{id} is registered"));
        assert_eq!(info.status, rlive_core::Status::Beta, "{id}");
        let desc = (info.desc)();
        assert!(!desc.params.is_empty(), "{id} has parameters");
        assert!(!desc.layout.is_empty(), "{id} declares a layout");
    }
}

#[test]
fn every_device_stays_finite_at_the_ends_of_every_parameter() {
    let loud = hot_material(4_800);
    for id in REGISTRY {
        let info = rlive_devices::find(id).unwrap();
        for top in [false, true] {
            let mut p = (info.create)(&Default::default());
            p.activate(SR, BLOCK);
            extreme(&mut *p, info, top);
            let (l, r) = render(&mut *p, &loud, &loud);
            assert!(
                l.iter().chain(&r).all(|s| s.is_finite()),
                "{id} went non-finite at the {} of every range",
                if top { "top" } else { "bottom" }
            );
        }
    }
}

#[test]
fn every_device_handles_a_step_past_full_scale_without_allocating() {
    for (name, mut p) in all_devices() {
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (dc(20.0, BLOCK), dc(20.0, BLOCK));
        assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &[], &[]));
        assert!(l.iter().chain(&r).all(|s| s.is_finite()), "{name}");
    }
}

#[test]
fn every_device_is_silent_on_silence() {
    for (name, mut p) in all_devices() {
        let silence = vec![0.0; 9_600];
        let (l, r) = render(&mut *p, &silence, &silence);
        assert!(l.iter().chain(&r).all(|s| s.abs() < 1e-9), "{name}");
    }
}

#[test]
fn every_mode_of_the_compressor_runs_without_allocating() {
    for (mode, envelope, look, auto, sidechain) in [
        (0.0, 0.0, 0.0, 0.0, 0.0),
        (0.0, 1.0, 1.0, 1.0, 0.0),
        (1.0, 1.0, 2.0, 0.0, 1.0),
        (2.0, 0.0, 0.0, 1.0, 1.0),
    ] {
        let mut c = fast_compressor(&[
            (comp::MODE, mode),
            (comp::ENVELOPE, envelope),
            (comp::LOOKAHEAD, look),
            (comp::AUTO_RELEASE, auto),
            (comp::SIDECHAIN, sidechain),
            (comp::THRESHOLD, -40.0),
        ]);
        let tone: Vec<f32> = sine(440.0, SR, BLOCK * 8).iter().map(|s| s * 0.5).collect();
        let key = sine(1_000.0, SR, BLOCK * 8);
        assert!(render_keyed(&mut c, &tone, &key)
            .iter()
            .all(|s| s.is_finite()));
        let out = render(&mut c, &tone, &tone).0;
        assert!(out.iter().all(|s| s.is_finite()));
    }
}

#[test]
fn an_offset_parameter_change_takes_effect_at_its_frame_on_the_full_path() {
    // The compressor's lookahead path must still walk the block in steps.
    let mut c = fast_compressor(&[(comp::MODE, comp::MODE_RMS), (comp::MIX, 1.0)]);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.1f32; BLOCK], vec![0.1f32; BLOCK]);
    let change = rlive_core::ParamEvent {
        offset: 200,
        id: rlive_core::ParamId(comp::MAKEUP),
        value: -12.0,
    };
    h.process(&mut c, &mut l, &mut r, &[], &[change]);
    assert!((l[199] - 0.1).abs() < 1e-5, "{}", l[199]);
    assert_near(l[200], 0.1 * gain(-12.0), 1e-3, "after the change");
}
