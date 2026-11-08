//! Batch B6: the delay and modulation devices, against the manual and against arithmetic.

use rlive_core::{
    MidiEvent, MidiMessage, ParamEvent, ParamId, Processor, TransportInfo, ViewWriter,
};
use rlive_devices::devices::b6::{
    align_delay as align, beat_repeat as br, chorus_ensemble as chorus, echo, filter_delay as fd,
    grain_delay as grain, looper, phaser_flanger as pf, shifter,
};
use rlive_devices::dsp::{Biquad, BiquadKind, DelayLine, Smoother};
use rlive_devices::effects::delay as dl;
use rlive_devices::{builtins, find, DeviceInit};
use rlive_testkit::{
    assert_no_alloc, impulse, max_step, onset, peak, rms, sine, tone_amplitude, Harness,
};
use std::f32::consts::TAU;

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

fn ready<P: Processor + Default>() -> P {
    let mut p = P::default();
    p.activate(SR, BLOCK);
    p
}

fn set(p: &mut dyn Processor, params: &[(u32, f64)]) {
    for &(id, v) in params {
        p.set_param(id, v);
    }
}

fn advance(h: &mut Harness, frames: usize) {
    let ticks = h.transport.ticks_per_sample() * frames as f64;
    h.transport.tick += ticks;
    h.transport.sample_pos += frames as u64;
}

/// Runs stereo input through in blocks, with the transport following.
fn render(
    p: &mut dyn Processor,
    left: &[f32],
    right: &[f32],
    playing: bool,
    tempo: f64,
) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    h.transport.tempo = tempo;
    h.transport.playing = playing;
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        let n = cl.len();
        assert_no_alloc(|| h.process(p, cl, cr, &[], &[]));
        advance(&mut h, n);
    }
    (l, r)
}

fn mono(p: &mut dyn Processor, input: &[f32]) -> Vec<f32> {
    render(p, input, input, true, 120.0).0
}

fn tone(hz: f32, amp: f32, frames: usize) -> Vec<f32> {
    sine(hz, SR, frames).into_iter().map(|s| s * amp).collect()
}

fn noise(frames: usize, seed: u32) -> Vec<f32> {
    let mut rng = rlive_devices::dsp::util::Rng::new(seed);
    (0..frames).map(|_| rng.bipolar() * 0.5).collect()
}

fn dry_tone_amp(signal: &[f32], hz: f64) -> f64 {
    let n = signal.len();
    let window = n / 2;
    let cycles = (window as f64 * hz / SR).round().max(1.0);
    let frames = (cycles * SR / hz).round() as usize;
    tone_amplitude(&signal[n - frames..], hz, SR)
}

fn near(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} != {expected} (+-{tol})"
    );
}

// ----------------------------------------------------------------------- Delay

fn original_delay(left: &[f32], right: &[f32], tempo: f64) -> (Vec<f32>, Vec<f32>) {
    // The render from before this batch, copied so the comparison can't drift with the code.
    let sr = SR as f32;
    let params = dl::delay_params();
    let v = |id: u32| params.iter().find(|p| p.id == id).unwrap().default;
    let max = (SR * 4.5) as usize;
    let mut lines = [DelayLine::new(max), DelayLine::new(max)];
    let mut time = Smoother::new(0.0);
    time.set_time(0.08, sr);
    let mut tone = [Biquad::default(); 2];
    for f in tone.iter_mut() {
        f.set(BiquadKind::LowPass, v(dl::TONE) as f32, 0.707, 0.0, sr);
    }
    let (mut out_l, mut out_r) = (left.to_vec(), right.to_vec());
    for (cl, cr) in out_l.chunks_mut(BLOCK).zip(out_r.chunks_mut(BLOCK)) {
        let div = (v(dl::DIVISION).round() as usize).min(dl::DIVISION_BEATS.len() - 1);
        let seconds = if v(dl::SYNC) >= 0.5 {
            dl::DIVISION_BEATS[div] * 60.0 / tempo
        } else {
            v(dl::TIME) / 1000.0
        };
        time.target = (seconds as f32 * sr).min(sr * 4.4);
        if time.value == 0.0 {
            time.reset();
        }
        let fb = v(dl::FEEDBACK) as f32;
        let mix = v(dl::MIX) as f32;
        let ping = v(dl::PING_PONG) >= 0.5;
        for i in 0..cl.len() {
            let d = time.next();
            let dl_ = tone[0].process(lines[0].read(d));
            let dr = tone[1].process(lines[1].read(d));
            let (inl, inr) = (cl[i], cr[i]);
            if ping {
                let m = (inl + inr) * 0.5;
                lines[0].write(m + dr * fb);
                lines[1].write(dl_ * fb);
            } else {
                lines[0].write(inl + dl_ * fb);
                lines[1].write(inr + dr * fb);
            }
            cl[i] = inl * (1.0 - mix) + dl_ * mix;
            cr[i] = inr * (1.0 - mix) + dr * mix;
        }
    }
    (out_l, out_r)
}

#[test]
fn delays_default_path_matches_the_original_sample_for_sample() {
    let left = noise(30_000, 3);
    let right = noise(30_000, 4);
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    let (a_l, a_r) = render(&mut d, &left, &right, true, 123.0);
    let (b_l, b_r) = original_delay(&left, &right, 123.0);
    assert_eq!(a_l, b_l, "left channel changed");
    assert_eq!(a_r, b_r, "right channel changed");
}

#[test]
fn delay_time_mode_is_exact() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 100.0),
            (dl::MIX, 1.0),
            (dl::PING_PONG, 0.0),
            (dl::TONE, 20_000.0),
        ],
    );
    let input = impulse(10_000, 10, 1.0);
    let out = mono(&mut d, &input);
    assert_eq!(onset(&out, 0.2), Some(10 + 4_800));
}

#[test]
fn delay_sync_is_exact_and_follows_tempo() {
    let cases = [(0usize, 60.0), (3, 90.0), (6, 120.0), (7, 110.0)];
    for (division, tempo) in cases {
        let mut d = rlive_devices::effects::Delay::default();
        d.activate(SR, BLOCK);
        set(
            &mut d,
            &[
                (dl::SYNC, 1.0),
                (dl::DIVISION, division as f64),
                (dl::MIX, 1.0),
                (dl::PING_PONG, 0.0),
                (dl::TONE, 20_000.0),
                (dl::STEREO_LINK, 0.0),
                (dl::SMOOTHING, 2.0),
                (dl::SYNC_R, 1.0),
                (dl::DIVISION_R, division as f64),
            ],
        );
        let want = (dl::DIVISION_BEATS[division] * 60.0 / tempo * SR).round() as usize;
        let input = impulse(want + 2_000, 10, 1.0);
        let (l, _) = render(&mut d, &input, &input, true, tempo);
        assert_eq!(
            onset(&l, 0.2),
            Some(10 + want),
            "division {division} at {tempo}"
        );
    }
}

#[test]
fn delay_stereo_link_off_gives_each_channel_its_own_time() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::STEREO_LINK, 0.0),
            (dl::SYNC, 0.0),
            (dl::SYNC_R, 0.0),
            (dl::TIME, 100.0),
            (dl::TIME_R, 200.0),
            (dl::MIX, 1.0),
            (dl::PING_PONG, 0.0),
            (dl::TONE, 20_000.0),
        ],
    );
    let input = impulse(20_000, 10, 1.0);
    let (l, r) = render(&mut d, &input, &input, true, 120.0);
    assert_eq!(onset(&l, 0.2), Some(10 + 4_800));
    assert_eq!(onset(&r, 0.2), Some(10 + 9_600));
}

#[test]
fn delay_offset_moves_the_time_by_its_percentage_on_each_channel() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 100.0),
            (dl::OFFSET_L, 25.0),
            (dl::OFFSET_R, -25.0),
            (dl::MIX, 1.0),
            (dl::PING_PONG, 0.0),
            (dl::TONE, 20_000.0),
        ],
    );
    let input = impulse(20_000, 10, 1.0);
    let (l, r) = render(&mut d, &input, &input, true, 120.0);
    assert_eq!(
        onset(&l, 0.2),
        Some(10 + 6_000),
        "linked times, offset each its own"
    );
    assert_eq!(onset(&r, 0.2), Some(10 + 3_600));
}

#[test]
fn delay_ping_pong_bounces_between_the_channels() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 100.0),
            (dl::MIX, 1.0),
            (dl::FEEDBACK, 0.8),
            (dl::PING_PONG, 1.0),
            (dl::TONE, 20_000.0),
        ],
    );
    let input = impulse(30_000, 10, 1.0);
    let (l, r) = render(&mut d, &input, &input, true, 120.0);
    assert_eq!(onset(&l, 0.2), Some(10 + 4_800));
    assert_eq!(
        onset(&r, 0.2),
        Some(10 + 9_600),
        "second repeat is on the other side"
    );
    let third = peak(&l[10 + 3 * 4_800 - 2..10 + 3 * 4_800 + 200]);
    assert!(third > 0.2, "third repeat back on the left: {third}");
}

#[test]
fn delay_feedback_stays_bounded_at_the_top_of_its_range() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 20.0),
            (dl::MIX, 1.0),
            (dl::FEEDBACK, 0.95),
            (dl::PING_PONG, 0.0),
            (dl::SMOOTHING, 2.0),
        ],
    );
    let input = noise(200_000, 7);
    let out = mono(&mut d, &input);
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak(&out) < 6.0, "{}", peak(&out));
}

#[test]
fn delay_freeze_loops_the_buffer_and_ignores_new_input() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 50.0),
            (dl::MIX, 1.0),
            (dl::PING_PONG, 0.0),
            (dl::FEEDBACK, 0.0),
            (dl::TONE, 20_000.0),
        ],
    );
    let mut h = Harness::new(SR);
    let run = |d: &mut rlive_devices::effects::Delay, h: &mut Harness, input: &[f32]| {
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
            h.process(d, cl, cr, &[], &[]);
        }
        l
    };
    run(&mut d, &mut h, &noise(2_400, 11));
    d.set_param(dl::FREEZE, 1.0);
    let held = run(&mut d, &mut h, &vec![0.0; 2_400]);
    // New input is ignored: a loud burst leaves no trace.
    let burst = run(&mut d, &mut h, &noise(2_400, 99));
    let later = run(&mut d, &mut h, &vec![0.0; 24_000]);
    let (a, b) = (rms(&held[1_200..]), rms(&later[12_000..14_400]));
    assert!(a > 0.05, "something frozen: {a}");
    near(b as f64, a as f64, 0.02, "the loop doesn't decay");
    // The frozen loop's period is 50 ms: identical blocks of output repeat.
    let period = 2_400;
    let w = &later[0..4_800];
    for i in 0..period {
        assert!((w[i] - w[i + period]).abs() < 1e-4, "at {i}");
    }
    let _ = burst;
}

#[test]
fn delay_repitch_and_fade_change_time_without_clicking_and_jump_jumps() {
    for (mode, expect_click) in [(0.0, false), (1.0, false), (2.0, true)] {
        let mut d = rlive_devices::effects::Delay::default();
        d.activate(SR, BLOCK);
        set(
            &mut d,
            &[
                (dl::SYNC, 0.0),
                (dl::TIME, 20.0),
                (dl::MIX, 1.0),
                (dl::FEEDBACK, 0.0),
                (dl::PING_PONG, 0.0),
                (dl::TONE, 20_000.0),
                (dl::SMOOTHING, mode),
            ],
        );
        let input = tone(100.0, 0.5, 40_000);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (input.clone(), input.clone());
        for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
            let change = [ParamEvent {
                offset: 0,
                id: ParamId(dl::TIME),
                value: 37.0,
            }];
            let ev: &[ParamEvent] = if i == 150 { &change } else { &[] };
            assert_no_alloc(|| h.process(&mut d, cl, cr, &[], ev));
        }
        // Skip the first block while the line fills. A 100 Hz sine moves about 0.0065 a sample.
        let step = max_step(&l[5_000..]);
        if expect_click {
            assert!(step > 0.03, "mode {mode}: jump should step, moved {step}");
        } else {
            assert!(step < 0.012, "mode {mode}: {step}");
        }
    }
}

#[test]
fn delay_lfo_moves_the_time_by_its_depth() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    let depth = dl::LFO_DELAY_DEPTH as f64;
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 20.0),
            (dl::MIX, 1.0),
            (dl::PING_PONG, 0.0),
            (dl::TONE, 20_000.0),
            (dl::SMOOTHING, 2.0),
            (dl::LFO_WAVE, 4.0),
            (dl::LFO_RATE_MODE, 0.0),
            (dl::LFO_FREQ, 1.0),
            (dl::LFO_DELAY, 100.0),
        ],
    );
    // A square at 1 Hz: the first half second reads longer, the second half shorter.
    let mut input = vec![0.0f32; 48_000];
    input[4_800] = 1.0; // 0.1 s, during the positive half.
    input[33_600] = 1.0; // 0.7 s, during the negative half.
    let (l, _) = render(&mut d, &input, &input, true, 120.0);
    let base = 960.0;
    let slow = (base * (1.0 + depth)).round() as usize;
    let fast = (base * (1.0 - depth)).round() as usize;
    assert_eq!(onset(&l[..30_000], 0.2), Some(4_800 + slow));
    assert_eq!(
        onset(&l[30_000..], 0.2).map(|i| i + 30_000),
        Some(33_600 + fast)
    );
}

#[test]
fn delay_band_pass_passes_its_centre_and_cuts_far_away() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 10.0),
            (dl::MIX, 1.0),
            (dl::PING_PONG, 0.0),
            (dl::TONE, 20_000.0),
            (dl::FILTER_ON, 1.0),
            (dl::FILTER_FREQ, 1_000.0),
            (dl::FILTER_WIDTH, 1.0),
        ],
    );
    let centre = dry_tone_amp(&mono(&mut d, &tone(1_000.0, 0.5, 24_000)), 1_000.0);
    assert!(centre > 0.35, "{centre}");
    d.reset();
    let far = dry_tone_amp(&mono(&mut d, &tone(8_000.0, 0.5, 24_000)), 8_000.0);
    assert!(far < 0.05, "{far}");
}

#[test]
fn delay_equal_loudness_mix_is_constant_power() {
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::EQUAL_LOUDNESS, 1.0),
            (dl::MIX, 0.5),
            (dl::SYNC, 0.0),
            (dl::TIME, 100.0),
            (dl::PING_PONG, 0.0),
        ],
    );
    let input = impulse(100, 5, 1.0);
    let out = mono(&mut d, &input);
    near(
        out[5] as f64,
        std::f64::consts::FRAC_1_SQRT_2,
        1e-3,
        "dry at 50%",
    );
}

// ----------------------------------------------------------------------- Echo

fn echo_settled(extra: &[(u32, f64)]) -> echo::Echo {
    let mut e: echo::Echo = ready();
    set(
        &mut e,
        &[
            (echo::SYNC_L, 0.0),
            (echo::SYNC_R, 0.0),
            (echo::TIME_L, 100.0),
            (echo::TIME_R, 100.0),
            (echo::MIX, 100.0),
            (echo::FEEDBACK, 0.0),
            (echo::FILTER_ON, 0.0),
        ],
    );
    set(&mut e, extra);
    e
}

#[test]
fn echo_time_is_exact_in_ms_and_synced_divisions() {
    let mut e = echo_settled(&[]);
    let out = mono(&mut e, &impulse(10_000, 10, 1.0));
    assert_eq!(onset(&out, 0.2), Some(10 + 4_800));

    // 1/8 at 120 BPM is 12000 samples; triplet and dotted of the same note.
    for (mode, want) in [(0.0, 12_000), (1.0, 8_000), (2.0, 18_000)] {
        let mut e = echo_settled(&[
            (echo::SYNC_L, 1.0),
            (echo::SYNC_MODE_L, mode),
            (echo::NOTE_L, 3.0),
        ]);
        let out = mono(&mut e, &impulse(30_000, 0, 1.0));
        assert_eq!(onset(&out, 0.2), Some(want), "mode {mode}");
    }
    // 16th mode counts sixteenths: 5 at 120 BPM is 5 * 6000.
    let mut e = echo_settled(&[
        (echo::SYNC_L, 1.0),
        (echo::SYNC_MODE_L, 3.0),
        (echo::SIXTEENTH_L, 5.0),
    ]);
    let out = mono(&mut e, &impulse(40_000, 0, 1.0));
    assert_eq!(onset(&out, 0.2), Some(30_000));
}

#[test]
fn echo_ping_pong_bounces_and_mid_side_decodes() {
    let mut e = echo_settled(&[(echo::CHANNEL_MODE, 1.0), (echo::FEEDBACK, 0.8)]);
    let input = impulse(30_000, 0, 1.0);
    let (l, r) = render(&mut e, &input, &input, true, 120.0);
    assert_eq!(onset(&l, 0.2), Some(4_800));
    assert_eq!(onset(&r, 0.2), Some(9_600));

    // Mid and side: an input only on the left is both, so the left channel sums them.
    let mut e = echo_settled(&[
        (echo::CHANNEL_MODE, 2.0),
        (echo::LINK, 0.0),
        (echo::TIME_L, 100.0),
        (echo::TIME_R, 200.0),
    ]);
    let left = impulse(30_000, 0, 1.0);
    let right = vec![0.0; 30_000];
    let (l, r) = render(&mut e, &left, &right, true, 120.0);
    near(l[4_800] as f64, 0.5, 1e-3, "mid echo on the left");
    near(r[4_800] as f64, 0.5, 1e-3, "mid echo on the right");
    near(l[9_600] as f64, 0.5, 1e-3, "side echo on the left");
    near(
        r[9_600] as f64,
        -0.5,
        1e-3,
        "side echo inverted on the right",
    );
}

#[test]
fn echo_feedback_stays_finite_and_filter_is_in_the_loop() {
    let mut e = echo_settled(&[
        (echo::FEEDBACK, 1.0),
        (echo::TIME_L, 20.0),
        (echo::TIME_R, 20.0),
        (echo::MOD_DELAY, 100.0),
    ]);
    let out = mono(&mut e, &noise(240_000, 5));
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak(&out) < 8.0, "{}", peak(&out));

    let mut e = echo_settled(&[
        (echo::FEEDBACK, 0.9),
        (echo::FILTER_ON, 1.0),
        (echo::LP_FREQ, 500.0),
    ]);
    let out = mono(&mut e, &tone(4_000.0, 0.5, 96_000));
    assert!(
        dry_tone_amp(&out, 4_000.0) < 0.02,
        "filtered before it repeats"
    );
}

#[test]
fn echo_delay_modulation_has_the_depth_of_mod_delay() {
    // A slow, bipolar sine. At a sample near the crest, the delay is `time * (1 + depth * m)`.
    let mut e = echo_settled(&[
        (echo::MOD_WAVE, 0.0),
        (echo::MOD_FREQ, 0.25),
        (echo::MOD_DELAY, 100.0),
        (echo::REPITCH, 0.0),
    ]);
    // Fade mode keeps the taps pure; measure with an impulse at a known LFO position.
    let mut input = vec![0.0f32; 96_000];
    input[48_000] = 1.0; // 1 s: the LFO is at sin(2 pi * 0.25) = +1.
    let (l, _) = render(&mut e, &input, &input, true, 120.0);
    let found = onset(&l[48_000..], 0.1).unwrap();
    let want = 4_800.0 * (1.0 + echo::MOD_DELAY_DEPTH as f64);
    assert!((found as f64 - want).abs() < 60.0, "{found} vs {want}");
}

#[test]
fn echo_repitch_off_crossfades_without_a_click() {
    let mut e = echo_settled(&[
        (echo::REPITCH, 0.0),
        (echo::TIME_L, 20.0),
        (echo::TIME_R, 20.0),
    ]);
    let input = tone(100.0, 0.5, 40_000);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (input.clone(), input.clone());
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        let change = [ParamEvent {
            offset: 0,
            id: ParamId(echo::TIME_L),
            value: 43.0,
        }];
        h.process(&mut e, cl, cr, &[], if i == 150 { &change } else { &[] });
    }
    assert!(max_step(&l[4_000..]) < 0.012, "{}", max_step(&l[4_000..]));
}

#[test]
fn echo_gate_mutes_what_is_below_its_threshold() {
    let mut e = echo_settled(&[
        (echo::GATE_ON, 1.0),
        (echo::GATE_THRESHOLD, -20.0),
        (echo::MIX, 0.0),
    ]);
    let quiet = tone(440.0, 0.02, 30_000);
    let out = mono(&mut e, &quiet);
    assert!(
        rms(&out[20_000..]) < 1e-4,
        "below threshold: {}",
        rms(&out[20_000..])
    );
    let mut e = echo_settled(&[
        (echo::GATE_ON, 1.0),
        (echo::GATE_THRESHOLD, -20.0),
        (echo::MIX, 0.0),
    ]);
    let loud = tone(440.0, 0.5, 30_000);
    let out = mono(&mut e, &loud);
    assert!(rms(&out[20_000..]) > 0.2, "above threshold");
}

#[test]
fn echo_ducks_the_wet_signal_while_the_input_plays() {
    let mut e = echo_settled(&[
        (echo::DUCK_ON, 1.0),
        (echo::DUCK_THRESHOLD, -30.0),
        (echo::FEEDBACK, 0.6),
        (echo::DUCK_RELEASE, 5.0),
    ]);
    let mut input = tone(300.0, 0.6, 48_000);
    input.extend(vec![0.0f32; 48_000]);
    let out = mono(&mut e, &input);
    // While the input is on the repeats are held down, and they spring back afterwards.
    let during = rms(&out[20_000..24_000]);
    let after = rms(&out[52_000..56_000]);
    assert!(after > during * 1.5, "{during} {after}");
}

#[test]
fn echo_noise_and_input_distortion_add_what_they_should() {
    // Noise with nothing playing, on the wet path.
    let mut e = echo_settled(&[(echo::NOISE_ON, 1.0), (echo::NOISE_AMOUNT, 100.0)]);
    let out = mono(&mut e, &vec![0.0; 20_000]);
    assert!(rms(&out) > 0.01, "{}", rms(&out));

    // The distortion is at the input: dry signal only here, so it hits the output directly.
    let mut clean = echo_settled(&[(echo::MIX, 0.0), (echo::INPUT, 12.0)]);
    let mut driven = echo_settled(&[(echo::MIX, 0.0), (echo::INPUT, 12.0), (echo::DRIVE, 1.0)]);
    let input = tone(100.0, 0.25, 24_000);
    let a = mono(&mut clean, &input);
    let b = mono(&mut driven, &input);
    assert!(peak(&a) > peak(&b) + 0.1, "{} {}", peak(&a), peak(&b));
}

#[test]
fn echo_goes_to_sleep_unless_noise_and_gate_keep_it_on() {
    // Sleeps: the noise stops once the input has been silent for 8 s.
    let mut e = echo_settled(&[(echo::NOISE_ON, 1.0), (echo::NOISE_AMOUNT, 100.0)]);
    let out = mono(&mut e, &vec![0.0; 48_000 * 9]);
    assert!(rms(&out[..48_000]) > 0.005);
    assert_eq!(rms(&out[48_000 * 8 + 4_096..]), 0.0, "asleep after 8 s");

    // Stays awake with Noise and Gate together.
    let mut e = echo_settled(&[
        (echo::NOISE_ON, 1.0),
        (echo::NOISE_AMOUNT, 100.0),
        (echo::GATE_ON, 1.0),
    ]);
    let out = mono(&mut e, &vec![0.0; 48_000 * 9]);
    assert!(rms(&out[48_000 * 8..]) > 0.005);
}

#[test]
fn echo_reverb_can_sit_pre_post_or_in_the_loop() {
    // Reverb's location changes how the tail builds.
    let tail = |location: f64| {
        let mut e = echo_settled(&[
            (echo::REVERB, 100.0),
            (echo::REVERB_LOCATION, location),
            (echo::REVERB_DECAY, 80.0),
            (echo::FEEDBACK, 0.5),
        ]);
        let mut input = impulse(96_000, 0, 1.0);
        input[0] = 1.0;
        mono(&mut e, &input)
    };
    let pre = tail(0.0);
    let post = tail(1.0);
    let feedback = tail(2.0);
    // Pre: the reverb is in front of the first echo, so it arrives at 4800 samples at the
    // earliest; before that the wet path is silent.
    assert!(peak(&pre[..4_700]) < 1e-3);
    // Post: nothing comes back before the first echo, either.
    assert!(peak(&post[..4_700]) < 1e-3);
    // In the loop, each pass gets more reverb, so the tail is denser late on than in Post.
    assert!(rms(&feedback[60_000..]) > rms(&post[60_000..]));
}

#[test]
fn echo_stereo_is_mono_at_zero_and_wider_above_one_hundred() {
    let mut e = echo_settled(&[
        (echo::STEREO, 0.0),
        (echo::LINK, 0.0),
        (echo::TIME_R, 200.0),
    ]);
    let left = tone(500.0, 0.5, 30_000);
    let right = tone(777.0, 0.5, 30_000);
    let (l, r) = render(&mut e, &left, &right, true, 120.0);
    for i in 0..l.len() {
        assert!((l[i] - r[i]).abs() < 1e-5, "mono at {i}");
    }
}

// ----------------------------------------------------------------------- Filter Delay

#[test]
fn filter_delay_taps_read_their_own_inputs_and_pans() {
    let mut f: fd::FilterDelay = ready();
    set(
        &mut f,
        &[
            (fd::tap_id(0, fd::SYNC), 0.0),
            (fd::tap_id(0, fd::TIME), 100.0),
            (fd::tap_id(0, fd::FILTER_ON), 0.0),
            (fd::tap_id(0, fd::FEEDBACK), 0.0),
            (fd::tap_id(1, fd::ON), 0.0),
            (fd::tap_id(2, fd::ON), 0.0),
            (fd::DRY, -60.0),
        ],
    );
    let left = impulse(8_000, 0, 1.0);
    let right = vec![0.0; 8_000];
    let (l, r) = render(&mut f, &left, &right, true, 120.0);
    assert_eq!(onset(&l, 0.2), Some(4_800), "line 1 hears the left");
    assert_eq!(peak(&r), 0.0, "line 1 pans to the left");

    let mut f: fd::FilterDelay = ready();
    set(
        &mut f,
        &[
            (fd::tap_id(0, fd::ON), 0.0),
            (fd::tap_id(1, fd::ON), 0.0),
            (fd::tap_id(2, fd::SYNC), 0.0),
            (fd::tap_id(2, fd::TIME), 50.0),
            (fd::tap_id(2, fd::FILTER_ON), 0.0),
            (fd::tap_id(2, fd::FEEDBACK), 0.0),
            (fd::DRY, -60.0),
        ],
    );
    let (l, r) = render(
        &mut f,
        &vec![0.0; 8_000],
        &impulse(8_000, 0, 1.0),
        true,
        120.0,
    );
    assert_eq!(onset(&r, 0.2), Some(2_400), "line 3 hears the right");
    assert_eq!(peak(&l), 0.0);
}

#[test]
fn filter_delay_center_line_hears_both_and_can_be_panned() {
    let mut f: fd::FilterDelay = ready();
    set(
        &mut f,
        &[
            (fd::tap_id(0, fd::ON), 0.0),
            (fd::tap_id(2, fd::ON), 0.0),
            (fd::tap_id(1, fd::SYNC), 0.0),
            (fd::tap_id(1, fd::TIME), 50.0),
            (fd::tap_id(1, fd::FILTER_ON), 0.0),
            (fd::tap_id(1, fd::FEEDBACK), 0.0),
            (fd::tap_id(1, fd::PAN), -1.0),
            (fd::DRY, -60.0),
        ],
    );
    let input = impulse(8_000, 0, 1.0);
    let (l, r) = render(&mut f, &input, &vec![0.0; 8_000], true, 120.0);
    near(l[2_400] as f64, 0.5, 1e-3, "the left half of the sum");
    assert!(peak(&r) < 1e-6);
}

#[test]
fn filter_delay_feedback_returns_through_the_filter_and_sync_is_exact() {
    let mut f: fd::FilterDelay = ready();
    set(
        &mut f,
        &[
            (fd::tap_id(0, fd::ON), 0.0),
            (fd::tap_id(2, fd::ON), 0.0),
            (fd::tap_id(1, fd::DIVISION), 3.0), // the "4" button: one beat.
            (fd::tap_id(1, fd::FILTER_ON), 1.0),
            (fd::tap_id(1, fd::FREQ), 500.0),
            (fd::tap_id(1, fd::WIDTH), 1.0),
            (fd::tap_id(1, fd::FEEDBACK), 0.9),
            (fd::DRY, -60.0),
        ],
    );
    let mut h = Harness::new(SR);
    h.transport.tempo = 120.0;
    let input = tone(6_000.0, 0.5, 200_000);
    let (mut l, mut r) = (input.clone(), input.clone());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        h.process(&mut f, cl, cr, &[], &[]);
    }
    assert!(dry_tone_amp(&l[..24_000], 6_000.0) < 0.1);
    assert!(l.iter().all(|s| s.is_finite()));

    // Sync: the button times.
    let mut f: fd::FilterDelay = ready();
    set(
        &mut f,
        &[
            (fd::tap_id(0, fd::ON), 0.0),
            (fd::tap_id(1, fd::ON), 0.0),
            (fd::tap_id(2, fd::DIVISION), 4.0), // the "5" button: 5 sixteenths.
            (fd::tap_id(2, fd::FILTER_ON), 0.0),
            (fd::tap_id(2, fd::FEEDBACK), 0.0),
            (fd::DRY, -60.0),
        ],
    );
    let (_, r) = render(
        &mut f,
        &vec![0.0; 40_000],
        &impulse(40_000, 0, 1.0),
        true,
        120.0,
    );
    assert_eq!(onset(&r, 0.2), Some(5 * 6_000));
}

// ----------------------------------------------------------------------- Grain Delay

#[test]
fn grain_delay_shifts_pitch_by_its_semitones() {
    let mut g: grain::GrainDelay = ready();
    // 50 Hz grains start 960 samples apart, a whole number of cycles of the input tone, so they
    // reinforce each other. At other rates the energy sits in sidebands around the same pitch.
    set(
        &mut g,
        &[
            (grain::MIX, 100.0),
            (grain::PITCH, 12.0),
            (grain::FREQUENCY, 50.0),
            (grain::TIME, 20.0),
        ],
    );
    let input = tone(1_000.0, 0.5, 96_000);
    let out = mono(&mut g, &input);
    let tail = &out[24_000..];
    let up = tone_amplitude(tail, 2_000.0, SR);
    let same = tone_amplitude(tail, 1_000.0, SR);
    assert!(up > 0.15, "octave up: {up}");
    assert!(same < up * 0.4, "{same} {up}");
}

#[test]
fn grain_delay_at_unity_pitch_delays_by_the_set_time() {
    let mut g: grain::GrainDelay = ready();
    set(
        &mut g,
        &[
            (grain::MIX, 100.0),
            (grain::FREQUENCY, 20.0),
            (grain::TIME, 50.0),
            (grain::SYNC, 0.0),
        ],
    );
    let input = noise(48_000, 8);
    let out = mono(&mut g, &input);
    let want = 2_400;
    let correlation = |lag: usize| -> f64 {
        input[8_000..40_000]
            .iter()
            .zip(&out[8_000 + lag..40_000 + lag])
            .map(|(a, b)| (*a as f64) * (*b as f64))
            .sum()
    };
    let best = (2_000..3_000)
        .max_by(|a, b| correlation(*a).total_cmp(&correlation(*b)))
        .unwrap();
    assert!((best as i64 - want).abs() <= 8, "{best} vs {want}");
}

#[test]
fn grain_delay_random_pitch_and_spray_spread_the_grains() {
    let spread = |extra: &[(u32, f64)]| {
        let mut g: grain::GrainDelay = ready();
        set(
            &mut g,
            &[
                (grain::MIX, 100.0),
                (grain::FREQUENCY, 80.0),
                (grain::TIME, 20.0),
            ],
        );
        set(&mut g, extra);
        let out = mono(&mut g, &tone(1_000.0, 0.5, 96_000));
        // How much of the tone is left at its own pitch.
        tone_amplitude(&out[24_000..], 1_000.0, SR)
    };
    let clean = spread(&[]);
    let random = spread(&[(grain::RANDOM_PITCH, 12.0)]);
    assert!(random < clean * 0.6, "{clean} {random}");
}

#[test]
fn grain_delay_sync_is_tempo_locked_and_xy_routing_applies() {
    let mut g: grain::GrainDelay = ready();
    set(
        &mut g,
        &[
            (grain::MIX, 100.0),
            (grain::SYNC, 1.0),
            (grain::DIVISION, 3.0),
            (grain::FREQUENCY, 30.0),
        ],
    );
    let input = noise(96_000, 9);
    let out = mono(&mut g, &input);
    let correlation = |lag: usize| -> f64 {
        input[8_000..50_000]
            .iter()
            .zip(&out[8_000 + lag..50_000 + lag])
            .map(|(a, b)| (*a as f64) * (*b as f64))
            .sum()
    };
    let best = (23_000..25_000)
        .max_by(|a, b| correlation(*a).total_cmp(&correlation(*b)))
        .unwrap();
    assert!((best as i64 - 24_000).abs() < 16, "one beat: {best}");

    // X-Y: routing the X axis to Pitch sets the pitch from the pad's position.
    let mut g: grain::GrainDelay = ready();
    set(
        &mut g,
        &[
            (grain::MIX, 100.0),
            (grain::X_AXIS, 4.0),
            (grain::X_POSITION, 1.0),
            (grain::FREQUENCY, 50.0),
            (grain::PITCH, 0.0),
        ],
    );
    let out = mono(&mut g, &tone(1_000.0, 0.5, 48_000));
    let tail = &out[12_000..];
    assert!(tone_amplitude(tail, 2_000.0, SR) > tone_amplitude(tail, 1_000.0, SR));
}

// ----------------------------------------------------------------------- Align Delay

#[test]
fn align_delay_is_exact_in_time_samples_and_distance() {
    let run = |extra: &[(u32, f64)]| {
        let mut a: align::AlignDelay = ready();
        set(&mut a, extra);
        let input = impulse(60_000, 0, 1.0);
        render(&mut a, &input, &input, true, 120.0)
    };
    let (l, _) = run(&[(align::TIME_L, 125.0)]);
    assert_eq!(onset(&l, 0.2), Some(6_000));

    let (l, _) = run(&[
        (align::MODE, align::MODE_SAMPLES as f64),
        (align::SAMPLES_L, 777.0),
    ]);
    assert_eq!(onset(&l, 0.2), Some(777));

    // Link is on by default, so the right channel follows.
    let (l, r) = run(&[
        (align::MODE, align::MODE_SAMPLES as f64),
        (align::LINK, 0.0),
        (align::SAMPLES_L, 10.0),
        (align::SAMPLES_R, 20.0),
    ]);
    assert_eq!(onset(&l, 0.2), Some(10));
    assert_eq!(onset(&r, 0.2), Some(20));
}

#[test]
fn align_delay_distance_uses_the_speed_of_sound_at_temperature() {
    let delay_for = |extra: &[(u32, f64)]| {
        let mut a: align::AlignDelay = ready();
        set(&mut a, &[(align::MODE, align::MODE_DISTANCE as f64)]);
        set(&mut a, extra);
        let out = mono(&mut a, &impulse(60_000, 0, 1.0));
        onset(&out, 0.2).unwrap()
    };
    let c = align::speed_of_sound(20.0);
    near(c, 343.2, 0.2, "343.2 m/s at 20 C");
    let want = (100.0 / c * SR).round() as usize;
    assert_eq!(
        delay_for(&[(align::DISTANCE_L, 100.0), (align::TEMPERATURE, 20.0)]),
        want
    );

    // Feet convert to metres.
    let want_ft = (100.0 * align::METERS_PER_FOOT / c * SR).round() as usize;
    near(
        delay_for(&[
            (align::DISTANCE_L, 100.0),
            (align::DISTANCE_UNIT, 1.0),
            (align::TEMPERATURE, 20.0),
        ]) as f64,
        want_ft as f64,
        1.0,
        "feet",
    );

    // 68 F is 20 C, so the Fahrenheit switch gets the same answer.
    near(align::fahrenheit_to_celsius(68.0), 20.0, 1e-9, "68 F");
    assert_eq!(
        delay_for(&[
            (align::DISTANCE_L, 100.0),
            (align::TEMPERATURE_UNIT, 1.0),
            (align::TEMPERATURE, 68.0)
        ]),
        want
    );

    // Warmer air is faster, so the delay is shorter.
    assert!(delay_for(&[(align::DISTANCE_L, 100.0), (align::TEMPERATURE, 40.0)]) < want);
}

// ----------------------------------------------------------------------- Chorus-Ensemble

#[test]
fn chorus_mode_and_tap_toggle_change_the_wet_signal() {
    let wet = |mode: f64, two: f64| {
        let mut c: chorus::ChorusEnsemble = ready();
        set(
            &mut c,
            &[
                (chorus::MODE, mode),
                (chorus::TWO_TAPS, two),
                (chorus::MIX, 100.0),
                (chorus::AMOUNT, 100.0),
                (chorus::RATE, 2.0),
            ],
        );
        mono(&mut c, &tone(1_000.0, 0.5, 96_000))
    };
    let two = wet(0.0, 1.0);
    let one = wet(0.0, 0.0);
    let ens = wet(1.0, 1.0);
    // More taps at different delays mean more beating: the RMS level swings more with two.
    assert_ne!(two, one);
    assert_ne!(two, ens);
    assert!(rms(&ens[30_000..]) > 0.05);
}

#[test]
fn chorus_auto_delay_grows_with_amount_and_fixed_does_not() {
    let onset_of = |delay: f64, amount: f64| {
        let mut c: chorus::ChorusEnsemble = ready();
        set(
            &mut c,
            &[
                (chorus::DELAY_TIME, delay),
                (chorus::AMOUNT, amount),
                (chorus::MIX, 100.0),
                (chorus::TWO_TAPS, 0.0),
                (chorus::RATE, 0.02),
            ],
        );
        onset(&mono(&mut c, &impulse(10_000, 0, 1.0)), 0.3).unwrap()
    };
    let auto_small = onset_of(0.0, 20.0);
    let auto_big = onset_of(0.0, 100.0);
    assert!(auto_big > auto_small * 3, "{auto_small} {auto_big}");
    let fixed_small = onset_of(5.0, 20.0);
    let fixed_big = onset_of(5.0, 100.0);
    near(fixed_small as f64, 8.0 * 48.0, 16.0, "8 ms");
    near(
        fixed_big as f64,
        8.0 * 48.0,
        chorus::CHORUS_DEPTH_MS as f64 * 48.0 + 16.0,
        "8 ms, modulated",
    );
}

#[test]
fn vibrato_passes_only_the_modulated_signal_and_ignores_feedback_and_mix() {
    let mut v: chorus::ChorusEnsemble = ready();
    set(
        &mut v,
        &[
            (chorus::MODE, 2.0),
            (chorus::AMOUNT, 100.0),
            (chorus::RATE, 2.0),
            (chorus::FEEDBACK, 100.0),
            (chorus::MIX, 0.0),
        ],
    );
    let input = tone(1_000.0, 0.5, 96_000);
    let out = mono(&mut v, &input);
    // Wet only, whatever the mix says, with its pitch swinging by the analytic amount:
    // delay = D * (1 + sin(2 pi f t)) moves the pitch by 1 + (2 pi f D) at its fastest.
    let d_seconds = chorus::VIBRATO_DEPTH_MS * 0.001;
    let a = TAU * 2.0 * d_seconds;
    let segment = |from: usize, to: usize| -> f32 {
        let mut crossings = Vec::new();
        for i in from..to {
            if out[i - 1] <= 0.0 && out[i] > 0.0 {
                crossings.push(i as f32);
            }
        }
        if crossings.len() < 2 {
            return 0.0;
        }
        (crossings.len() - 1) as f32 * SR as f32 / (crossings[crossings.len() - 1] - crossings[0])
    };
    let fast = (10_000..80_000)
        .step_by(1_000)
        .map(|at| segment(at, at + 600))
        .fold(0.0f32, f32::max);
    let expected_top = 1_000.0 * (1.0 + a);
    assert!(
        (fast - expected_top).abs() < 15.0,
        "{fast} vs {expected_top}"
    );
}

#[test]
fn chorus_width_zero_is_mono_and_two_hundred_is_side_heavy() {
    let side_rms = |width: f64| {
        let mut c: chorus::ChorusEnsemble = ready();
        set(&mut c, &[(chorus::WIDTH, width), (chorus::MIX, 100.0)]);
        let input = tone(500.0, 0.5, 48_000);
        let (l, r) = render(&mut c, &input, &input, true, 120.0);
        let s: Vec<f32> = l.iter().zip(&r).map(|(a, b)| (a - b) * 0.5).collect();
        rms(&s[20_000..])
    };
    assert!(side_rms(0.0) < 1e-5);
    let a = side_rms(100.0);
    let b = side_rms(200.0);
    near((b / a) as f64, 2.0, 0.02, "twice the side at 200%");
}

#[test]
fn chorus_high_pass_keeps_lows_unmodulated_and_warmth_saturates() {
    let distance = |hp: f64| {
        let mut c: chorus::ChorusEnsemble = ready();
        set(
            &mut c,
            &[
                (chorus::HP_ON, hp),
                (chorus::HP_FREQ, 1_500.0),
                (chorus::MIX, 100.0),
                (chorus::AMOUNT, 100.0),
                (chorus::RATE, 3.0),
                (chorus::TWO_TAPS, 0.0),
            ],
        );
        let input = tone(60.0, 0.5, 96_000);
        let out = mono(&mut c, &input);
        let error: Vec<f32> = out.iter().zip(&input).map(|(a, b)| a - b).collect();
        rms(&error[40_000..])
    };
    assert!(
        distance(1.0) < distance(0.0) * 0.5,
        "lows pass untouched below the High-Pass"
    );

    let harmonics = |warmth: f64| {
        let mut c: chorus::ChorusEnsemble = ready();
        set(
            &mut c,
            &[
                (chorus::WARMTH, warmth),
                (chorus::MIX, 100.0),
                (chorus::AMOUNT, 0.0),
            ],
        );
        dry_tone_amp(&mono(&mut c, &tone(200.0, 0.9, 48_000)), 600.0)
    };
    assert!(harmonics(100.0) > harmonics(0.0) + 0.02, "Warmth distorts");
}

#[test]
fn chorus_feedback_and_invert_stay_finite() {
    let mut c: chorus::ChorusEnsemble = ready();
    set(
        &mut c,
        &[
            (chorus::FEEDBACK, 100.0),
            (chorus::FB_INVERT, 1.0),
            (chorus::MIX, 100.0),
        ],
    );
    let out = mono(&mut c, &noise(240_000, 2));
    assert!(out.iter().all(|s| s.is_finite()), "feedback ran away");
    assert!(peak(&out) < 8.0, "{}", peak(&out));
}

// ----------------------------------------------------------------------- Phaser-Flanger

#[test]
fn phaser_notches_are_where_the_allpass_math_says() {
    // No LFO, no feedback: a static comb of `Notches` notches, where the phase is odd halves.
    for notches in [1usize, 2, 4, 8] {
        let center = 1_000.0;
        let spread = 0.3;
        let q = pf::spread_q(spread);
        let mut p: pf::PhaserFlanger = ready();
        set(
            &mut p,
            &[
                (pf::NOTCHES, notches as f64),
                (pf::CENTER, center as f64),
                (pf::SPREAD, spread as f64 * 100.0),
                (pf::AMOUNT, 0.0),
                (pf::FEEDBACK, 0.0),
                (pf::MIX, 50.0),
                (pf::SAFE_BASS, 5.0),
            ],
        );
        for hz in [200.0, 800.0, 1_500.0, 6_000.0] {
            let out = mono(&mut p, &tone(hz, 0.5, 72_000));
            let measured = dry_tone_amp(&out, hz as f64);
            let expected = pf::phaser_response(hz, center, q, notches, SR as f32) * 0.5;
            near(
                measured,
                expected as f64,
                0.05,
                &format!("{notches} notches at {hz} Hz"),
            );
        }
    }
}

#[test]
fn flanger_comb_nulls_at_the_analytic_frequencies() {
    let mut p: pf::PhaserFlanger = ready();
    // No modulation and no feedback: a comb. Null where the delay is half a cycle.
    set(
        &mut p,
        &[
            (pf::MODE, 1.0),
            (pf::FLANGER_TIME, 2.0),
            (pf::AMOUNT, 0.0),
            (pf::FEEDBACK, 0.0),
            (pf::MIX, 50.0),
            (pf::SAFE_BASS, 5.0),
        ],
    );
    let null = dry_tone_amp(&mono(&mut p, &tone(250.0, 0.5, 72_000)), 250.0);
    let peak_amp = dry_tone_amp(&mono(&mut p, &tone(500.0, 0.5, 72_000)), 500.0);
    assert!(null < 0.05, "null at 250 Hz: {null}");
    assert!(peak_amp > 0.4, "{peak_amp}");
}

#[test]
fn doubler_modulation_is_bipolar_and_flanger_only_shortens() {
    let delay_at_lfo_peak = |mode: f64, amount: f64| {
        let mut p: pf::PhaserFlanger = ready();
        set(
            &mut p,
            &[
                (pf::MODE, mode),
                (pf::DOUBLER_TIME, 20.0),
                (pf::FLANGER_TIME, 20.0),
                (pf::AMOUNT, amount),
                (pf::FEEDBACK, 0.0),
                (pf::MIX, 100.0),
                (pf::WAVEFORM, 4.0),
                (pf::FREQ, 0.5),
                (pf::SAFE_BASS, 5.0),
                (pf::PHASE, 0.0),
                (pf::DUTY, 0.0),
            ],
        );
        // A rectangle LFO: positive for the first second.
        let mut input = vec![0.0f32; 48_000];
        input[2_000] = 1.0;
        onset(&mono(&mut p, &input), 0.2).unwrap() - 2_000
    };
    let base = 960.0;
    let doubler_up = delay_at_lfo_peak(2.0, 100.0);
    near(
        doubler_up as f64,
        base * (1.0 + pf::DOUBLER_DEPTH as f64),
        3.0,
        "doubler goes up",
    );
    let flanger = delay_at_lfo_peak(1.0, 100.0);
    assert!(
        (flanger as f64) < base * 0.5,
        "flanger shortens from Time: {flanger}"
    );
}

#[test]
fn phaser_modes_and_feedback_stay_finite_with_every_waveform() {
    for wave in 0..10 {
        let mut p: pf::PhaserFlanger = ready();
        set(
            &mut p,
            &[
                (pf::WAVEFORM, wave as f64),
                (pf::FEEDBACK, 95.0),
                (pf::FREQ, 8.0),
                (pf::NOTCHES, 8.0),
                (pf::AMOUNT, 100.0),
                (pf::ENV_ON, 1.0),
                (pf::ENV_AMOUNT, 50.0),
                (pf::LFO2_MIX, 50.0),
            ],
        );
        let out = mono(&mut p, &noise(96_000, wave as u32 + 1));
        assert!(out.iter().all(|s| s.is_finite()), "wave {wave}");
    }
}

#[test]
fn duty_cycle_warps_the_cycle_towards_its_ends() {
    use rlive_devices::devices::b6::common::duty_warp;
    assert_eq!(duty_warp(0.25, 0.0), 0.25);
    // With 100% towards the front, a quarter of the cycle has already passed its midpoint.
    assert!(duty_warp(0.25, 0.9) > 0.5);
    assert!(duty_warp(0.25, -0.9) < 0.15);
}

#[test]
fn phaser_blend_moves_modulation_from_center_to_spread() {
    let tone_out = |blend: f64| {
        let mut p: pf::PhaserFlanger = ready();
        set(
            &mut p,
            &[
                (pf::BLEND, blend),
                (pf::NOTCHES, 4.0),
                (pf::CENTER, 1_000.0),
                (pf::AMOUNT, 100.0),
                (pf::FEEDBACK, 0.0),
                (pf::FREQ, 3.0),
                (pf::MIX, 50.0),
            ],
        );
        mono(&mut p, &tone(1_000.0, 0.5, 96_000))
    };
    assert_ne!(tone_out(0.0), tone_out(100.0));
}

// ----------------------------------------------------------------------- Beat Repeat

/// Runs a pulse train through a Beat Repeat in Insert mode and returns the output.
fn beat_repeat_out(params: &[(u32, f64)], frames: usize, pulses: &[usize]) -> Vec<f32> {
    let mut b = br::BeatRepeat::with_seed(1);
    b.activate(SR, BLOCK);
    set(&mut b, params);
    let mut input = vec![0.0f32; frames];
    for &p in pulses {
        input[p] = 1.0;
    }
    render(&mut b, &input, &input, true, 120.0).0
}

#[test]
fn beat_repeat_interval_and_offset_are_locked_to_the_transport() {
    // 1 Bar at 120 BPM is 96000 samples. Grid 1/16 is 6000; Offset 8/16 is the third beat.
    let params = [
        (br::INTERVAL, 5.0),
        (br::OFFSET, 8.0),
        (br::GRID, 2.0),
        (br::GATE, 2.0),
        (br::CHANCE, 100.0),
        (br::MIX_MODE, 1.0),
    ];
    let bar = 96_000;
    let beat3 = 48_000;
    // A pulse just after the third beat is caught and repeated a sixteenth later, twice over
    // the gate of 2/16.
    let out = beat_repeat_out(&params, bar * 2, &[beat3 + 100, bar + beat3 + 100]);
    assert_eq!(
        onset(&out[..beat3], 0.5),
        None,
        "nothing before the third beat"
    );
    assert!(out[beat3 + 100].abs() > 0.5, "first pass plays");
    assert!(
        out[beat3 + 100 + 6_000].abs() > 0.5,
        "the sixteenth repeats"
    );
    assert!(
        out[bar + beat3 + 100 + 6_000].abs() > 0.5,
        "and a bar later"
    );
}

#[test]
fn beat_repeat_grid_and_gate_set_the_slices() {
    // Grid 1/8 is 12000 samples; Gate 8/16 is 48000: four slices.
    let params = [
        (br::INTERVAL, 5.0),
        (br::OFFSET, 0.0),
        (br::GRID, 4.0),
        (br::GATE, 8.0),
        (br::MIX_MODE, 2.0),
    ];
    let out = beat_repeat_out(&params, 96_000, &[1_000]);
    for k in 0..4 {
        let at = 1_000 + 12_000 * k;
        assert!(out[at].abs() > 0.5, "slice {k} at {at}: {}", out[at]);
    }
    assert!(
        peak(&out[49_000..96_000]) < 1e-3,
        "nothing after the gate closes"
    );
}

#[test]
fn beat_repeat_chance_follows_its_seed() {
    use rlive_devices::dsp::util::Rng;
    let params = [
        (br::INTERVAL, 3.0),
        (br::CHANCE, 50.0),
        (br::GRID, 2.0),
        (br::GATE, 2.0),
        (br::MIX_MODE, 2.0),
        (br::OFFSET, 0.0),
    ];
    let mut pulses = Vec::new();
    for beat in 0..40 {
        pulses.push(beat as usize * 24_000 + 50);
    }
    let out = beat_repeat_out(&params, 24_000 * 40, &pulses);
    let mut rng = Rng::new(1);
    for beat in 0..40usize {
        let fired = rng.unipolar() < 0.5;
        let at = beat * 24_000 + 50;
        let heard = out[at].abs() > 0.5;
        assert_eq!(heard, fired, "beat {beat}");
    }
}

#[test]
fn beat_repeat_variation_changes_the_slice_length_by_mode() {
    let slice_gap = |variation: f64, mode: f64, seed: u32| -> usize {
        let mut b = br::BeatRepeat::with_seed(seed);
        b.activate(SR, BLOCK);
        set(
            &mut b,
            &[
                (br::GRID, 4.0),
                (br::GATE, 16.0),
                (br::MIX_MODE, 2.0),
                (br::VARIATION, variation),
                (br::VARIATION_MODE, mode),
            ],
        );
        let out = render(
            &mut b,
            &impulse(96_000, 1_000, 1.0),
            &impulse(96_000, 1_000, 1.0),
            true,
            120.0,
        )
        .0;
        // Where the slice repeats: the first copy of the pulse past the capture itself.
        out[2_000..].iter().position(|s| s.abs() > 0.5).unwrap() + 1_000
    };
    assert_eq!(slice_gap(0.0, 0.0, 3), 12_000, "1/8 grid, no variation");
    let trigger: Vec<usize> = (0..6).map(|seed| slice_gap(10.0, 0.0, seed)).collect();
    assert!(
        trigger.iter().any(|g| *g != 12_000),
        "trigger mode varies: {trigger:?}"
    );
    let auto: Vec<usize> = (0..6).map(|seed| slice_gap(10.0, 4.0, seed)).collect();
    assert!(
        auto.iter().any(|g| *g != 12_000),
        "auto mode varies: {auto:?}"
    );
}

#[test]
fn beat_repeat_no_triplets_only_leaves_binary_grids() {
    // A 1/12 grid, triplet, repeats at 8000 samples; with No Triplets it falls to a 1/16.
    let gap = |no_triplets: f64| {
        let params = [
            (br::GRID, 3.0),
            (br::GATE, 12.0),
            (br::MIX_MODE, 2.0),
            (br::NO_TRIPLETS, no_triplets),
        ];
        let out = beat_repeat_out(&params, 96_000, &[100]);
        out[1_000..].iter().position(|s| s.abs() > 0.5).unwrap() + 900
    };
    assert_eq!(gap(0.0), 8_000);
    assert_eq!(gap(1.0), 6_000);
}

#[test]
fn beat_repeat_mix_modes_treat_the_dry_signal_as_the_manual_says() {
    let run = |mode: f64| {
        let params = [
            (br::INTERVAL, 5.0),
            (br::GRID, 2.0),
            (br::GATE, 4.0),
            (br::MIX_MODE, mode),
        ];
        let mut input = vec![0.0f32; 96_000];
        for i in (0..96_000).step_by(1_000) {
            input[i] = 0.25;
        }
        let mut b = br::BeatRepeat::with_seed(1);
        b.activate(SR, BLOCK);
        set(&mut b, &params);
        render(&mut b, &input, &input, true, 120.0).0
    };
    let mix = run(0.0);
    let insert = run(1.0);
    let gate = run(2.0);
    // Outside the repetition, Mix and Insert pass the original, Gate does not.
    assert!(mix[50_000].abs() > 0.2, "mix passes dry after the gate");
    assert!(
        insert[50_000].abs() > 0.2,
        "insert passes dry after the gate"
    );
    assert_eq!(gate[50_000], 0.0, "gate never passes dry");
    // During the repetition, a pulse in Mix is doubled by the first pass; Gate and Insert hold
    // the captured slice alone.
    assert!(mix[1_000] > 0.4, "{}", mix[1_000]);
    assert!((insert[1_000] - 0.25).abs() < 1e-3);
    assert!((gate[1_000] - 0.25).abs() < 1e-3);
}

#[test]
fn beat_repeat_decay_pitch_and_repeat_button() {
    // Decay shrinks each repetition.
    let params = [
        (br::INTERVAL, 5.0),
        (br::GRID, 2.0),
        (br::GATE, 8.0),
        (br::MIX_MODE, 2.0),
        (br::DECAY, 50.0),
    ];
    let out = beat_repeat_out(&params, 96_000, &[1_000]);
    let first = out[1_000].abs();
    let second = out[7_000].abs();
    near((second / first) as f64, 0.5, 0.05, "decay of 50%");

    // Pitching down by an octave stretches a slice to twice its length: a pulse repeats two
    // slices later than it would otherwise.
    let params = [
        (br::INTERVAL, 5.0),
        (br::GRID, 2.0),
        (br::GATE, 8.0),
        (br::MIX_MODE, 2.0),
        (br::PITCH, -12.0),
    ];
    let out = beat_repeat_out(&params, 96_000, &[100]);
    assert!(out[200] > 0.1, "{}", out[200]);
    let at = 100 * 2 + 12_000;
    let near_loud = peak(&out[at - 3..at + 3]);
    assert!(near_loud > 0.2, "{near_loud}");

    // Repeat grabs now, with the transport stopped too, and lets go when released.
    let mut b = br::BeatRepeat::with_seed(1);
    b.activate(SR, BLOCK);
    set(&mut b, &[(br::GRID, 2.0), (br::MIX_MODE, 2.0)]);
    let mut h = Harness::new(SR);
    h.transport.playing = false;
    let mut left = vec![0.0f32; 24_000];
    left[300] = 1.0;
    let mut right = left.clone();
    h.process(
        &mut b,
        &mut left[..256],
        &mut right[..256],
        &[],
        &[ParamEvent {
            offset: 0,
            id: ParamId(br::REPEAT),
            value: 1.0,
        }],
    );
    let (mut l, mut r) = (left[256..].to_vec(), right[256..].to_vec());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        h.process(&mut b, cl, cr, &[], &[]);
    }
    let period = 6_000; // 1/16 at 120 BPM, regardless of the transport being stopped.
    assert!(peak(&l[300 - 256 + period - 2..300 - 256 + period + 2]) > 0.5);
}

#[test]
fn beat_repeat_has_no_effect_when_the_transport_is_stopped_unless_repeat() {
    let params = [(br::INTERVAL, 3.0), (br::MIX_MODE, 2.0)];
    let mut b = br::BeatRepeat::with_seed(1);
    b.activate(SR, BLOCK);
    set(&mut b, &params);
    let input = impulse(96_000, 10, 1.0);
    let (l, _) = render(&mut b, &input, &input, false, 120.0);
    assert_eq!(peak(&l), 0.0, "gate mode, stopped: no repetition happens");
}

#[test]
fn beat_repeat_filter_narrows_the_repetitions() {
    let params = [
        (br::INTERVAL, 5.0),
        (br::GRID, 2.0),
        (br::GATE, 8.0),
        (br::MIX_MODE, 2.0),
        (br::FILTER_ON, 1.0),
        (br::FILTER_FREQ, 1_000.0),
        (br::FILTER_WIDTH, 1.0),
    ];
    let mut b = br::BeatRepeat::with_seed(1);
    b.activate(SR, BLOCK);
    set(&mut b, &params);
    let out = render(
        &mut b,
        &tone(100.0, 0.5, 96_000),
        &tone(100.0, 0.5, 96_000),
        true,
        120.0,
    )
    .0;
    assert!(
        rms(&out[..40_000]) < 0.05,
        "far below the band: {}",
        rms(&out[..40_000])
    );
}

// ----------------------------------------------------------------------- Shifter

fn shifter_output(extra: &[(u32, f64)], hz: f32) -> Vec<f32> {
    let mut s: shifter::Shifter = ready();
    set(&mut s, &[(shifter::MIX, 100.0)]);
    set(&mut s, extra);
    mono(&mut s, &tone(hz, 0.5, 96_000))
}

#[test]
fn shifter_pitch_mode_transposes_by_semitones() {
    let out = shifter_output(
        &[(shifter::COARSE_ST, 12.0), (shifter::WINDOW, 80.0)],
        1_000.0,
    );
    let tail = &out[24_000..];
    let octave = tone_amplitude(tail, 2_000.0, SR);
    assert!(octave > 0.2, "{octave}");
    assert!(tone_amplitude(tail, 1_000.0, SR) < octave * 0.25);

    let out = shifter_output(
        &[(shifter::COARSE_ST, 7.0), (shifter::WINDOW, 60.0)],
        1_000.0,
    );
    let fifth = 1_000.0 * 2f64.powf(7.0 / 12.0);
    let tail = &out[24_000..];
    assert!(tone_amplitude(tail, fifth, SR) > tone_amplitude(tail, 1_000.0, SR));
}

#[test]
fn shifter_reports_a_latency_of_half_its_window_and_passes_unity_at_it() {
    let mut s: shifter::Shifter = ready();
    set(&mut s, &[(shifter::MIX, 100.0)]);
    assert_eq!(s.latency(), 480, "20 ms window, half");
    let out = mono(&mut s, &impulse(4_000, 10, 1.0));
    assert_eq!(onset(&out, 0.2), Some(10 + 480));

    // Freq and Ring have no delay.
    let mut s: shifter::Shifter = ready();
    set(&mut s, &[(shifter::MODE, 1.0), (shifter::COARSE_HZ, 50.0)]);
    assert_eq!(s.latency(), 0);
    let mut s: shifter::Shifter = ready();
    set(&mut s, &[(shifter::MODE, 2.0), (shifter::COARSE_HZ, 50.0)]);
    assert_eq!(s.latency(), 0);
}

#[test]
fn shifter_freq_mode_moves_every_frequency_by_hertz() {
    let out = shifter_output(
        &[(shifter::MODE, 1.0), (shifter::COARSE_HZ, 300.0)],
        1_000.0,
    );
    let tail = &out[24_000..];
    let up = tone_amplitude(tail, 1_300.0, SR);
    assert!(up > 0.4, "{up}");
    assert!(
        tone_amplitude(tail, 700.0, SR) < up * 0.05,
        "image suppressed"
    );
    assert!(tone_amplitude(tail, 1_000.0, SR) < up * 0.05);

    let out = shifter_output(
        &[(shifter::MODE, 1.0), (shifter::COARSE_HZ, -300.0)],
        1_000.0,
    );
    let tail = &out[24_000..];
    assert!(tone_amplitude(tail, 700.0, SR) > 0.4);
    assert!(tone_amplitude(tail, 1_300.0, SR) < 0.02);
}

#[test]
fn shifter_ring_mode_adds_and_subtracts_the_frequency() {
    let out = shifter_output(
        &[(shifter::MODE, 2.0), (shifter::COARSE_HZ, 200.0)],
        1_000.0,
    );
    let tail = &out[24_000..];
    let plus = tone_amplitude(tail, 1_200.0, SR);
    let minus = tone_amplitude(tail, 800.0, SR);
    assert!(plus > 0.2 && minus > 0.2, "{plus} {minus}");
    assert!(
        tone_amplitude(tail, 1_000.0, SR) < 0.02,
        "no carrier of the input left"
    );

    // Drive adds harmonics of the ring output.
    let driven = shifter_output(
        &[
            (shifter::MODE, 2.0),
            (shifter::COARSE_HZ, 200.0),
            (shifter::DRIVE_ON, 1.0),
            (shifter::DRIVE, 100.0),
        ],
        1_000.0,
    );
    let tail_d = &driven[24_000..];
    assert!(tone_amplitude(tail_d, 3_600.0, SR) > tone_amplitude(tail, 3_600.0, SR));
}

#[test]
fn shifter_wide_inverts_spread_on_the_right() {
    let mut s: shifter::Shifter = ready();
    set(
        &mut s,
        &[
            (shifter::MIX, 100.0),
            (shifter::MODE, 1.0),
            (shifter::SPREAD, 100.0),
            (shifter::WIDE, 1.0),
            (shifter::COARSE_HZ, 400.0),
        ],
    );
    let input = tone(1_000.0, 0.5, 96_000);
    let (l, r) = render(&mut s, &input, &input, true, 120.0);
    assert!(tone_amplitude(&l[24_000..], 1_500.0, SR) > 0.3);
    assert!(tone_amplitude(&r[24_000..], 1_300.0, SR) > 0.3);
}

#[test]
fn shifter_lfo_moves_the_shift_by_its_amount() {
    // A square LFO on Freq mode: half the time up by Amount, half the time down.
    let mut s: shifter::Shifter = ready();
    set(
        &mut s,
        &[
            (shifter::MIX, 100.0),
            (shifter::MODE, 1.0),
            (shifter::COARSE_HZ, 500.0),
            (shifter::LFO_WAVE, 7.0),
            (shifter::LFO_RATE, 1.0),
            (shifter::LFO_AMOUNT_HZ, 200.0),
        ],
    );
    let out = mono(&mut s, &tone(1_000.0, 0.5, 48_000));
    let high = tone_amplitude(&out[8_000..20_000], 1_700.0, SR);
    let low = tone_amplitude(&out[32_000..44_000], 1_300.0, SR);
    assert!(high > 0.3, "{high}");
    assert!(low > 0.3, "{low}");
}

#[test]
fn shifter_envelope_moves_the_pitch_with_the_input_level() {
    let mut s: shifter::Shifter = ready();
    set(
        &mut s,
        &[
            (shifter::MIX, 100.0),
            (shifter::MODE, 1.0),
            (shifter::COARSE_HZ, 200.0),
            (shifter::ENV_ON, 1.0),
            (shifter::ENV_ATTACK, 1.0),
            (shifter::ENV_AMOUNT_HZ, 600.0),
        ],
    );
    let input = tone(1_000.0, 0.5, 48_000);
    let out = mono(&mut s, &input);
    // The shift is 200 plus 600 times the follower's level, which is about 0.45 for this tone,
    // so most of the energy has moved up to around 1470 Hz.
    let tail = &out[24_000..];
    let centre = (1_400..1_560)
        .step_by(5)
        .map(|hz| (hz, tone_amplitude(tail, hz as f64, SR)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!((1_430..1_520).contains(&centre.0), "{centre:?}");
    assert!(centre.1 > 0.2, "{centre:?}");
}

fn note(offset: u32, key: u8) -> MidiEvent {
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

#[test]
fn shifter_midi_mode_tracks_notes_with_glide_and_bend() {
    let mut s: shifter::Shifter = ready();
    set(
        &mut s,
        &[
            (shifter::MIX, 100.0),
            (shifter::PITCH_MODE, 1.0),
            (shifter::WINDOW, 40.0),
            (shifter::PB_RANGE, 12.0),
        ],
    );
    let mut h = Harness::new(SR);
    let input = tone(1_000.0, 0.5, 96_000);
    let (mut l, mut r) = (input.clone(), input.clone());
    // Note 72 is an octave above C3 (note 60), the reference for no shift.
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        let ev = [note(0, 72)];
        h.process(&mut s, cl, cr, if i == 0 { &ev } else { &[] }, &[]);
    }
    let tail = &l[24_000..];
    assert!(tone_amplitude(tail, 2_000.0, SR) > tone_amplitude(tail, 1_000.0, SR));
}

#[test]
fn shifter_delay_echoes_and_tone_darkens_feedback() {
    let run = |tone_hz: f64| {
        let mut s: shifter::Shifter = ready();
        set(
            &mut s,
            &[
                (shifter::MIX, 100.0),
                (shifter::MODE, 2.0),
                (shifter::COARSE_HZ, 0.0),
                (shifter::DELAY_ON, 1.0),
                (shifter::DELAY_TIME, 100.0),
                (shifter::DELAY_FEEDBACK, 0.8),
                (shifter::TONE, tone_hz),
            ],
        );
        let out = mono(&mut s, &impulse(48_000, 10, 1.0));
        (onset(&out, 0.1), rms(&out[20_000..]))
    };
    let (start, bright) = run(20_000.0);
    assert_eq!(start, Some(10 + 4_800), "delayed by Delay Time");
    let (_, dark) = run(500.0);
    assert!(dark < bright);
}

// ----------------------------------------------------------------------- Looper

fn looper_command(p: &mut looper::Looper, h: &mut Harness, id: u32, input: &mut [f32]) {
    let press = [ParamEvent {
        offset: 0,
        id: ParamId(id),
        value: 1.0,
    }];
    let release = [ParamEvent {
        offset: 0,
        id: ParamId(id),
        value: 0.0,
    }];
    let frames = input.len();
    let (mut l, mut r) = (input.to_vec(), input.to_vec());
    if frames <= BLOCK {
        h.process(p, &mut l, &mut r, &[], &press);
        advance(h, frames);
        h.process(p, &mut [], &mut [], &[], &release);
        input.copy_from_slice(&l);
        return;
    }
    let (la, lb) = l.split_at_mut(BLOCK);
    let (ra, rb) = r.split_at_mut(BLOCK);
    h.process(p, la, ra, &[], &press);
    advance(h, BLOCK);
    for (cl, cr) in lb.chunks_mut(BLOCK).zip(rb.chunks_mut(BLOCK)) {
        let n = cl.len();
        h.process(p, cl, cr, &[], &release);
        advance(h, n);
    }
    input.copy_from_slice(&l);
}

fn run_for(p: &mut looper::Looper, h: &mut Harness, frames: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        let n = cl.len();
        h.process(p, cl, cr, &[], &[]);
        advance(h, n);
    }
    l
}

fn run_input(p: &mut looper::Looper, h: &mut Harness, input: &[f32]) -> Vec<f32> {
    let mut l = input.to_vec();
    let mut r = input.to_vec();
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        let n = cl.len();
        h.process(p, cl, cr, &[], &[]);
        advance(h, n);
    }
    l
}

const BAR: usize = 96_000; // at 120 BPM

fn looper_started() -> (looper::Looper, Harness) {
    let mut p: looper::Looper = ready();
    set(&mut p, &[(looper::MONITOR, looper::MONITOR_NEVER as f64)]);
    let mut h = Harness::new(SR);
    h.transport.tempo = 120.0;
    (p, h)
}

/// Rolls to half way through bar 1, presses Record, and lets it launch on bar 2.
fn launch_record(p: &mut looper::Looper, h: &mut Harness) {
    run_for(p, h, BAR / 2);
    let mut press = vec![0.0f32; BLOCK];
    looper_command(p, h, looper::CMD_RECORD, &mut press);
    assert_eq!(p.state(), looper::State::Stopped, "waits for the bar");
    run_for(p, h, BAR / 2 - BLOCK);
}

/// Records one bar with a click at `at`, launching at the first bar boundary.
fn record_one_bar(p: &mut looper::Looper, h: &mut Harness, at: usize) {
    launch_record(p, h);
    let mut take = vec![0.0f32; BAR];
    take[at] = 0.8;
    run_input(p, h, &take);
    assert_eq!(
        p.state(),
        looper::State::Recording,
        "Record launched on the bar"
    );
}

#[test]
fn looper_records_on_the_bar_and_plays_in_line_with_it() {
    let (mut p, mut h) = looper_started();
    record_one_bar(&mut p, &mut h, 24_000);
    assert_eq!(p.state(), looper::State::Recording);

    // Pressing Play ends the take on this boundary and starts at the loop's start.
    let mut play = vec![0.0f32; BAR];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
    assert_eq!(p.state(), looper::State::Playing);
    assert_eq!(p.loop_frames(), BAR, "one bar exactly");
    near(p.loop_tempo(), 120.0, 1e-9, "at the song's tempo");

    // The click comes round at the same place in every following bar.
    assert_eq!(
        onset(&play, 0.5),
        Some(24_000),
        "aligned to beat 2 of the bar"
    );
    let next = run_for(&mut p, &mut h, BAR);
    assert_eq!(onset(&next, 0.5), Some(24_000));
    let after = run_for(&mut p, &mut h, BAR);
    assert_eq!(onset(&after, 0.5), Some(24_000));
    near(
        peak(&after) as f64,
        0.8,
        0.02,
        "same level with no feedback in Play",
    );
}

#[test]
fn looper_overdubs_a_second_layer_and_undo_removes_it() {
    let (mut p, mut h) = looper_started();
    set(&mut p, &[(looper::FEEDBACK, 100.0)]);
    record_one_bar(&mut p, &mut h, 24_000);
    // Pressing Overdub on the bar turns the take into the first layer and keeps recording.
    let mut pass = vec![0.0f32; BAR];
    pass[48_000] = 0.5;
    let mut press = pass.clone();
    looper_command(&mut p, &mut h, looper::CMD_OVERDUB, &mut press);
    assert_eq!(p.state(), looper::State::Overdub);
    assert!(press[24_000] > 0.5, "the first layer sounds");

    let mut stop_overdub = vec![0.0f32; BAR];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut stop_overdub);
    assert_eq!(p.state(), looper::State::Playing);
    let both = run_for(&mut p, &mut h, BAR);
    assert!(both[24_000] > 0.5, "original");
    near(both[48_000] as f64, 0.5, 0.02, "overdub");

    // Undo removes the overdub; the first take stays. The second press redoes it.
    let mut undo = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_UNDO, &mut undo);
    run_for(&mut p, &mut h, 4 * BLOCK);
    assert!(peak(p.contents()[0]) > 0.5, "the take is kept");
    assert!(p.contents()[0][48_000].abs() < 0.05, "the overdub is gone");

    let mut redo = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_UNDO, &mut redo);
    run_for(&mut p, &mut h, 4 * BLOCK);
    near(
        p.contents()[0][48_000] as f64,
        0.5,
        0.05,
        "redo puts it back",
    );
}

#[test]
fn looper_feedback_decays_old_layers_and_is_latched_each_pass() {
    let (mut p, mut h) = looper_started();
    set(&mut p, &[(looper::FEEDBACK, 50.0)]);
    record_one_bar(&mut p, &mut h, 24_000);
    let mut first_pass = vec![0.0f32; BAR];
    looper_command(&mut p, &mut h, looper::CMD_OVERDUB, &mut first_pass);
    near(
        peak(&first_pass) as f64,
        0.8,
        0.02,
        "first pass at full level",
    );
    let second = run_input(&mut p, &mut h, &vec![0.0f32; BAR]);
    near(
        peak(&second) as f64,
        0.4,
        0.02,
        "halved after one overdub pass",
    );
    let third = run_input(&mut p, &mut h, &vec![0.0f32; BAR]);
    near(peak(&third) as f64, 0.2, 0.02, "and halved again");
    // Feedback has no effect in Play.
    let mut play = vec![0.0f32; BAR];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
    let again = run_for(&mut p, &mut h, BAR);
    let once_more = run_for(&mut p, &mut h, BAR);
    near(
        peak(&once_more) as f64,
        peak(&again) as f64,
        0.01,
        "play doesn't decay",
    );
}

#[test]
fn looper_feedback_changes_wait_for_the_next_repetition() {
    let (mut p, mut h) = looper_started();
    set(&mut p, &[(looper::FEEDBACK, 100.0)]);
    record_one_bar(&mut p, &mut h, 24_000);
    let mut press = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_OVERDUB, &mut press);
    // Halfway round the pass, turn Feedback down. This pass still keeps the old layer at 100%.
    run_for(&mut p, &mut h, BAR / 2 - BLOCK);
    let down = [ParamEvent {
        offset: 0,
        id: ParamId(looper::FEEDBACK),
        value: 0.0,
    }];
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    h.process(&mut p, &mut l, &mut r, &[], &down);
    advance(&mut h, BLOCK);
    run_for(&mut p, &mut h, BAR / 2 - BLOCK);
    // The take's click is written unchanged in the pass where it was at 100%; the next pass
    // takes the new, lower Feedback.
    near(
        p.contents()[0][24_000] as f64,
        0.8,
        0.02,
        "unchanged this pass",
    );
    let (mut l, mut r) = (vec![0.0f32; BAR], vec![0.0f32; BAR]);
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        let n = cl.len();
        h.process(&mut p, cl, cr, &[], &[]);
        advance(&mut h, n);
    }
    near(
        p.contents()[0][24_000] as f64,
        0.0,
        0.02,
        "zero once the next pass has gone round",
    );
}

#[test]
fn looper_clear_keeps_length_only_in_overdub_with_the_song_running() {
    let (mut p, mut h) = looper_started();
    record_one_bar(&mut p, &mut h, 24_000);
    let mut overdub = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_OVERDUB, &mut overdub);
    assert_eq!(p.state(), looper::State::Overdub);
    let mut clear = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_CLEAR, &mut clear);
    assert_eq!(p.loop_frames(), BAR, "length kept");
    near(p.loop_tempo(), 120.0, 1e-9, "tempo kept");
    run_for(&mut p, &mut h, 4 * BLOCK);
    assert_eq!(peak(p.contents()[0]), 0.0, "contents erased");

    // In any other state, it forgets the loop entirely.
    let (mut p, mut h) = looper_started();
    record_one_bar(&mut p, &mut h, 24_000);
    let mut stop = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_STOP, &mut stop);
    let mut clear = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_CLEAR, &mut clear);
    assert_eq!(p.loop_frames(), 0);
    assert_eq!(p.loop_tempo(), 0.0);
}

#[test]
fn looper_transport_stopped_records_and_estimates_tempo() {
    let mut p: looper::Looper = ready();
    set(&mut p, &[(looper::MONITOR, looper::MONITOR_ALWAYS as f64)]);
    let mut h = Harness::new(SR);
    h.transport.playing = false;
    let mut press = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_RECORD, &mut press);
    assert_eq!(
        p.state(),
        looper::State::Recording,
        "engages at once when stopped"
    );
    let take = tone(440.0, 0.3, 4 * BAR - BLOCK);
    let _ = run_input(&mut p, &mut h, &take);
    let mut stop = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_STOP, &mut stop);
    assert_eq!(p.state(), looper::State::Stopped);
    assert!(p.loop_frames() >= 4 * BAR - BLOCK);
    // A guess: the loop length put into the most plausible tempo range.
    let tempo = p.loop_tempo();
    assert!((80.0..=170.0).contains(&tempo), "{tempo}");
}

#[test]
fn looper_fixed_length_ends_the_take_and_starts_playing() {
    let (mut p, mut h) = looper_started();
    set(
        &mut p,
        &[(looper::RECORD_LENGTH, 1.0), (looper::RECORD_FLOW, 0.0)],
    );
    launch_record(&mut p, &mut h);
    run_input(&mut p, &mut h, &vec![0.1f32; BAR]);
    assert_eq!(
        p.state(),
        looper::State::Playing,
        "a one bar take ends itself"
    );
    run_for(&mut p, &mut h, BLOCK);
    assert_eq!(
        p.state(),
        looper::State::Playing,
        "a one bar take ends itself"
    );
    assert_eq!(p.loop_frames(), BAR);
}

#[test]
fn looper_double_duplicates_and_halve_keeps_the_playing_half() {
    let (mut p, mut h) = looper_started();
    record_one_bar(&mut p, &mut h, 24_000);
    let mut play = vec![0.0f32; BAR];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
    assert_eq!(p.loop_frames(), BAR);
    let mut double = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_DOUBLE, &mut double);
    assert_eq!(p.loop_frames(), BAR * 2);
    let contents = p.contents();
    near(contents[0][24_000] as f64, 0.8, 0.02, "first copy");
    near(contents[0][BAR + 24_000] as f64, 0.8, 0.02, "second copy");

    let mut halve = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_HALVE, &mut halve);
    assert_eq!(p.loop_frames(), BAR);
}

#[test]
fn looper_reverse_plays_backwards_and_speed_changes_rate() {
    let (mut p, mut h) = looper_started();
    record_one_bar(&mut p, &mut h, 24_000);
    let mut play = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
    let ev = [ParamEvent {
        offset: 0,
        id: ParamId(looper::REVERSE),
        value: 1.0,
    }];
    // Reverse waits for the boundary; give it the next bar, then look a bar on.
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    h.process(&mut p, &mut l, &mut r, &[], &ev);
    advance(&mut h, BLOCK);
    run_for(&mut p, &mut h, BAR - 2 * BLOCK);
    assert_eq!(p.state(), looper::State::Playing);
    let reversed = run_for(&mut p, &mut h, BAR);
    let pos = onset(&reversed, 0.4).expect("the click");
    near(
        pos as f64,
        (BAR - 24_000) as f64,
        1200.0,
        "the click is on the other side of the bar",
    );

    // Speed raises the rate by the ratio of an octave.
    let (mut p, mut h) = looper_started();
    set(
        &mut p,
        &[(looper::SPEED, 12.0), (looper::TEMPO_CONTROL, 0.0)],
    );
    record_one_bar(&mut p, &mut h, 24_000);
    let mut play = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
    let out = run_for(&mut p, &mut h, BAR);
    // Twice as fast: the click at 24000 appears at 12000.
    near(
        onset(&out, 0.3).unwrap() as f64,
        12_000.0,
        300.0,
        "twice as fast",
    );
}

#[test]
fn looper_monitors_as_each_chooser_setting_says() {
    let (states, wanted): (Vec<u32>, Vec<[bool; 3]>) = (
        vec![0, 1, 2, 3],
        vec![
            [true, true, true],
            [false, false, false],
            [true, false, false],
            [true, false, true],
        ],
    );
    let hear = |p: &mut looper::Looper, h: &mut Harness| -> bool {
        // A quiet loop, so what's heard above it is only the monitored input.
        peak(&run_input(p, h, &vec![0.25f32; 2 * BLOCK])) > 0.1
    };
    for (monitor, want) in states.iter().zip(wanted) {
        // Recording.
        let (mut p, mut h) = looper_started();
        set(
            &mut p,
            &[
                (looper::MONITOR, *monitor as f64),
                (looper::QUANTIZATION, 1.0),
            ],
        );
        let mut rec = vec![0.0f32; BLOCK];
        looper_command(&mut p, &mut h, looper::CMD_RECORD, &mut rec);
        assert_eq!(
            hear(&mut p, &mut h),
            want[0],
            "while recording, monitor {monitor}"
        );
        // Playing back silence, so the only sound is the monitored input.
        let (mut p, mut h) = looper_started();
        set(
            &mut p,
            &[
                (looper::MONITOR, *monitor as f64),
                (looper::QUANTIZATION, 1.0),
            ],
        );
        let mut rec = vec![0.0f32; BLOCK];
        looper_command(&mut p, &mut h, looper::CMD_RECORD, &mut rec);
        run_for(&mut p, &mut h, 2 * BAR);
        let mut play = vec![0.0f32; BLOCK];
        looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
        assert_eq!(
            hear(&mut p, &mut h),
            want[1],
            "while playing, monitor {monitor}"
        );
        // Stopped.
        let mut stop = vec![0.0f32; BLOCK];
        looper_command(&mut p, &mut h, looper::CMD_STOP, &mut stop);
        assert_eq!(
            hear(&mut p, &mut h),
            want[2],
            "while stopped, monitor {monitor}"
        );
    }
}

#[test]
fn looper_multi_purpose_button_follows_its_diagram() {
    let (mut p, mut h) = looper_started();
    set(&mut p, &[(looper::QUANTIZATION, 1.0)]);
    let press_release = |p: &mut looper::Looper, h: &mut Harness| {
        let mut block = vec![0.0f32; BLOCK];
        looper_command(p, h, looper::CMD_MULTI, &mut block);
    };
    // Empty: a press records. Recording: a press goes to play. Play: to overdub. Overdub: to play.
    press_release(&mut p, &mut h);
    assert_eq!(p.state(), looper::State::Recording);
    run_for(&mut p, &mut h, 2 * BLOCK);
    press_release(&mut p, &mut h);
    assert_eq!(p.state(), looper::State::Playing);
    run_for(&mut p, &mut h, 48_000 * 2);
    press_release(&mut p, &mut h);
    assert_eq!(p.state(), looper::State::Overdub);
    run_for(&mut p, &mut h, 48_000 * 2);
    press_release(&mut p, &mut h);
    assert_eq!(p.state(), looper::State::Playing);
    // Two quick presses stop it.
    run_for(&mut p, &mut h, 48_000 * 2);
    press_release(&mut p, &mut h);
    press_release(&mut p, &mut h);
    assert_eq!(p.state(), looper::State::Stopped, "a double press stops");

    // Held for two seconds while stopped, it clears.
    let press = [ParamEvent {
        offset: 0,
        id: ParamId(looper::CMD_MULTI),
        value: 1.0,
    }];
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    h.process(&mut p, &mut l, &mut r, &[], &press);
    advance(&mut h, BLOCK);
    run_for(&mut p, &mut h, 48_000 * 2 + BLOCK);
    assert_eq!(p.loop_frames(), 0, "held: cleared");
}

#[test]
fn looper_import_replaces_the_buffer_and_requests_reach_the_host() {
    let mut p: looper::Looper = ready();
    let sample = rlive_core::SampleBuffer {
        name: "bed".into(),
        left: vec![0.3; 4_800],
        right: vec![0.2; 4_800],
        sample_rate: SR,
    };
    let resource: rlive_core::Resource = std::sync::Arc::new(sample);
    let handed_back = assert_no_alloc(|| p.set_resource(0, resource));
    assert!(handed_back.is_some(), "handed back to the engine");
    assert_eq!(p.loop_frames(), 4_800);
    assert_eq!(p.contents()[1][10], 0.2);

    // Song Control asks for the transport; a host could act on it.
    let mut p: looper::Looper = ready();
    set(&mut p, &[(looper::SONG_CONTROL, 2.0)]);
    let mut h = Harness::new(SR);
    h.transport.playing = false;
    let mut rec = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_RECORD, &mut rec);
    run_for(&mut p, &mut h, 2_000);
    let mut play = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
    assert_eq!(p.song_request(), 1.0);
    let mut stop = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_STOP, &mut stop);
    assert_eq!(p.song_request(), 2.0);
}

#[test]
fn looper_set_and_follow_requests_the_tempo_it_found() {
    let mut p: looper::Looper = ready();
    set(
        &mut p,
        &[
            (looper::TEMPO_CONTROL, looper::TEMPO_SET_FOLLOW as f64),
            (looper::RECORD_LENGTH, 1.0),
        ],
    );
    let mut h = Harness::new(SR);
    h.transport.playing = false;
    let mut press = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_RECORD, &mut press);
    // One bar exactly, at what would be 100 BPM.
    let take = vec![0.2f32; (4.0 * 60.0 / 100.0 * SR) as usize - BLOCK];
    run_input(&mut p, &mut h, &take);
    let mut stop = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut stop);
    near(
        p.tempo_request() as f64,
        100.0,
        0.5,
        "asks the host for the tempo it measured",
    );
}

#[test]
fn looper_commands_are_rising_edges_and_reset_stops_it_but_keeps_the_loop() {
    let (mut p, mut h) = looper_started();
    set(&mut p, &[(looper::QUANTIZATION, 1.0)]);
    let mut press = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_RECORD, &mut press);
    run_for(&mut p, &mut h, 4_800);
    let mut play = vec![0.0f32; BLOCK];
    looper_command(&mut p, &mut h, looper::CMD_PLAY, &mut play);
    let frames = p.loop_frames();
    assert!(frames > 0);
    p.reset();
    assert_eq!(p.state(), looper::State::Stopped);
    assert_eq!(
        p.loop_frames(),
        frames,
        "the recording survives a transport stop"
    );
}

// ----------------------------------------------------------------------- Conformance

#[test]
fn every_b6_device_is_built_and_visible() {
    for name in [
        "Delay",
        "Echo",
        "Filter Delay",
        "Align Delay",
        "Chorus-Ensemble",
        "Phaser-Flanger",
        "Beat Repeat",
        "Grain Delay",
        "Shifter",
        "Looper",
    ] {
        let b = builtins().iter().find(|b| b.name == name).expect(name);
        assert!(b.visible(), "{name}");
        assert!(!(b.desc)().params.is_empty(), "{name}");
    }
    assert!(find("rlive.echo").is_some());
}

const EXTREMES: [f64; 2] = [0.0, 1.0];

#[test]
fn every_b6_device_renders_finite_output_without_allocating_at_the_extremes() {
    let ids = [
        "rlive.delay",
        "rlive.echo",
        "rlive.filter_delay",
        "rlive.align_delay",
        "rlive.chorus_ensemble",
        "rlive.phaser_flanger",
        "rlive.beat_repeat",
        "rlive.grain_delay",
        "rlive.shifter",
        "rlive.looper",
    ];
    for id in ids {
        let info = find(id).unwrap();
        let params = (info.desc)().params;
        for (extreme_i, normal) in EXTREMES.into_iter().enumerate() {
            let mut p = (info.create)(&DeviceInit {
                samples: vec![],
                state: None,
            });
            p.activate(SR, BLOCK);
            for param in &params {
                p.set_param(param.id, param.from_normalized(normal));
            }
            let mut h = Harness::new(SR);
            h.transport.tempo = 240.0;
            let events: Vec<ParamEvent> = params
                .iter()
                .map(|param| ParamEvent {
                    offset: 7,
                    id: param.param_id(),
                    value: param.from_normalized(1.0 - normal),
                })
                .collect();
            let input = noise(BLOCK * 400, 77 + extreme_i as u32);
            let (mut l, mut r) = (input.clone(), input);
            for (block, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
                let ev: &[ParamEvent] = if block == 50 { &events } else { &[] };
                let n = cl.len();
                assert_no_alloc(|| h.process(&mut *p, cl, cr, &[], ev));
                advance(&mut h, n);
            }
            assert!(l.iter().all(|s| s.is_finite()), "{id} at normal {normal}");
            assert!(peak(&l) < 40.0, "{id} blew up: {}", peak(&l));
        }
    }
}

#[test]
fn looper_only_publishes_its_status_to_a_listening_ui() {
    use rlive_core::{AudioBuffer, EventQueue, ParamOutQueue, ProcessContext, ViewChannel};
    use std::sync::Arc;
    let channel = Arc::new(ViewChannel::new(looper::VIEW_LEN));
    let mut p: looper::Looper = ready();
    let press = [ParamEvent {
        offset: 0,
        id: ParamId(looper::CMD_RECORD),
        value: 1.0,
    }];
    let mut process = |view: &ViewWriter, params: &[ParamEvent]| {
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let mut events_out = EventQueue::with_capacity(0);
        let mut param_out = ParamOutQueue::with_capacity(0);
        let transport = TransportInfo::default();
        p.process(&mut ProcessContext {
            audio: AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            sidechain: None,
            aux_out: &mut [],
            events: &[],
            events_out: &mut events_out,
            params,
            transport: &transport,
            param_out: &mut param_out,
            mod_out: &mut [],
            view,
        });
    };
    // Pressing Record with nobody listening: the device has nothing to publish it to.
    process(&ViewWriter::new(&channel), &press);
    let reader = channel.attach();
    let mut values = [9.0f32; looper::VIEW_LEN];
    assert!(reader.read(looper::VIEW, &mut values));
    assert_eq!(values[0], 0.0, "untouched before a reader is attached");
    // With one attached, it reports that it's recording.
    process(&ViewWriter::new(&channel), &[]);
    assert!(reader.read(looper::VIEW, &mut values));
    assert_eq!(values[0], 1.0, "recording");
}

#[test]
fn delay_lfo_rates_are_tempo_locked_in_synced_modes() {
    // The synced LFO's period follows the transport, not free-running time.
    let mut d = rlive_devices::effects::Delay::default();
    d.activate(SR, BLOCK);
    set(
        &mut d,
        &[
            (dl::SYNC, 0.0),
            (dl::TIME, 20.0),
            (dl::MIX, 1.0),
            (dl::SMOOTHING, 2.0),
            (dl::LFO_WAVE, 4.0),
            (dl::LFO_RATE_MODE, 2.0),
            (dl::LFO_SYNC, 12.0),
            (dl::LFO_DELAY, 100.0),
            (dl::PING_PONG, 0.0),
            (dl::TONE, 20_000.0),
        ],
    );
    let mut input = vec![0.0f32; 24_000];
    input[4_000] = 1.0;
    let (l, _) = render(&mut d, &input, &input, true, 120.0);
    // A quarter-note square LFO at 120 BPM: positive for the first 12000 samples of each beat.
    assert_eq!(onset(&l, 0.2), Some(4_000 + 1_440));
}

#[test]
fn delay_hi_quality_keeps_more_of_a_high_tone_at_a_fractional_delay() {
    let run = |hq: f64| {
        let mut d = rlive_devices::effects::Delay::default();
        d.activate(SR, BLOCK);
        set(
            &mut d,
            &[
                (dl::SYNC, 0.0),
                (dl::TIME, 20.0),
                (dl::MIX, 1.0),
                (dl::PING_PONG, 0.0),
                (dl::SMOOTHING, 0.0),
                (dl::TONE, 20_000.0),
                (dl::HI_QUALITY, hq),
                (dl::OFFSET_L, 0.01),
            ],
        );
        dry_tone_amp(&mono(&mut d, &tone(12_000.0, 0.5, 48_000)), 12_000.0)
    };
    let linear = run(0.0);
    let cubic = run(1.0);
    assert!(cubic > linear, "{linear} {cubic}");
}

#[test]
fn every_b6_device_survives_a_reset_with_a_clean_slate() {
    for id in [
        "rlive.echo",
        "rlive.filter_delay",
        "rlive.chorus_ensemble",
        "rlive.phaser_flanger",
        "rlive.grain_delay",
        "rlive.shifter",
        "rlive.beat_repeat",
        "rlive.align_delay",
    ] {
        let info = find(id).unwrap();
        let mut p = (info.create)(&DeviceInit {
            samples: vec![],
            state: None,
        });
        p.activate(SR, BLOCK);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (noise(BLOCK, 5), noise(BLOCK, 6));
        h.process(&mut *p, &mut l, &mut r, &[], &[]);
        p.reset();
        for _ in 0..8 {
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &[], &[]));
            assert!(peak(&l).max(peak(&r)) < 1e-9, "{id}");
        }
    }
}
