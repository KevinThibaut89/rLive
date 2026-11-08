use super::SR;
use crate::dsp::envelope::*;
use crate::dsp::tempo::BeatClock;

fn env(f: impl FnOnce(&mut EnvelopeSettings)) -> Envelope {
    let mut s = EnvelopeSettings {
        attack: 0.01,
        decay: 0.02,
        release: 0.03,
        sustain: 0.5,
        ..Default::default()
    };
    f(&mut s);
    let mut e = Envelope::new();
    e.prepare(SR);
    e.set(&s);
    e
}

fn samples(e: &mut Envelope, n: usize) -> Vec<f32> {
    (0..n).map(|_| e.next()).collect()
}

const ATTACK: usize = 480;
const DECAY: usize = 960;
const RELEASE: usize = 1_440;

#[test]
fn a_linear_envelope_follows_its_stages() {
    let mut e = env(|_| {});
    e.gate_on();
    let v = samples(&mut e, ATTACK + DECAY + 100);
    assert!(v[0].abs() < 1e-3);
    assert!((v[ATTACK / 2] - 0.5).abs() < 1e-3, "attack midpoint");
    assert!((v[ATTACK] - 1.0).abs() < 1e-3, "peak");
    assert!(
        (v[ATTACK + DECAY / 2] - 0.75).abs() < 1e-3,
        "decay midpoint"
    );
    assert!((v[ATTACK + DECAY] - 0.5).abs() < 1e-3, "sustain");
    assert_eq!(e.stage(), EnvStage::Sustain);
    e.gate_off();
    let release = samples(&mut e, RELEASE + 1);
    assert!((release[RELEASE / 2] - 0.25).abs() < 1e-3);
    assert_eq!(e.stage(), EnvStage::Idle);
}

#[test]
fn levels_can_be_set_for_each_stage() {
    let mut e = env(|s| {
        s.initial = 0.2;
        s.peak = 0.9;
        s.sustain = 0.4;
        s.end = 0.1;
    });
    e.gate_on();
    let v = samples(&mut e, ATTACK + DECAY + 10);
    assert!((v[0] - 0.2).abs() < 1e-3);
    assert!((v[ATTACK] - 0.9).abs() < 1e-3);
    assert!((v[ATTACK + DECAY] - 0.4).abs() < 1e-3);
    e.gate_off();
    samples(&mut e, RELEASE + 2);
    assert!((e.level() - 0.1).abs() < 1e-3);
}

#[test]
fn slopes_bend_the_curve_exactly_as_documented() {
    for slope in [-1.0f32, -0.5, 0.5, 1.0] {
        let mut e = env(|s| s.attack_slope = slope);
        e.gate_on();
        let v = samples(&mut e, ATTACK);
        for i in (0..ATTACK).step_by(37) {
            let t = i as f32 / ATTACK as f32;
            let k = -6.0 * slope;
            let want = (k * t).exp_m1() / k.exp_m1();
            assert!((v[i] - want).abs() < 2e-3, "slope {slope} at {t}");
        }
    }
    assert!(slope_curve(0.5, 0.0) == 0.5);
    assert!(slope_curve(0.25, 1.0) > 0.25, "positive moves early");
    assert!(slope_curve(0.25, -1.0) < 0.25, "negative moves late");
}

#[test]
fn slopes_are_monotonic_and_meet_their_ends() {
    for slope in [-1.0f32, -0.3, 0.0, 0.7, 1.0] {
        assert!(slope_curve(0.0, slope).abs() < 1e-6);
        assert!((slope_curve(1.0, slope) - 1.0).abs() < 1e-5);
        let mut last = -1.0;
        for i in 0..=100 {
            let y = slope_curve(i as f32 / 100.0, slope);
            assert!(y >= last);
            last = y;
        }
    }
}

#[test]
fn zero_length_stages_still_arrive() {
    let mut e = env(|s| {
        s.attack = 0.0;
        s.decay = 0.0;
        s.sustain = 0.3;
    });
    e.gate_on();
    let v = samples(&mut e, 10);
    assert!((v[3] - 0.3).abs() < 1e-6);
    e.gate_off();
    samples(&mut e, 2000);
    assert!(!e.is_active());
}

#[test]
fn a_loop_restarts_at_the_end_of_the_decay() {
    let mut e = env(|s| s.loop_mode = LoopMode::Loop);
    e.gate_on();
    let v = samples(&mut e, (ATTACK + DECAY) * 3);
    let period = ATTACK + DECAY;
    // Each cycle starts from the level the last one ended on, and peaks one attack later.
    for cycle in 1..3 {
        let start = cycle * period;
        assert!((v[start] - 0.5).abs() < 2e-3, "start of cycle {cycle}");
        assert!(
            (v[start + ATTACK] - 1.0).abs() < 2e-3,
            "peak of cycle {cycle}"
        );
    }
}

#[test]
fn loop_timing_does_not_drift() {
    let mut e = env(|s| {
        s.loop_mode = LoopMode::Loop;
        s.attack = 0.0101;
        s.decay = 0.0203;
    });
    e.gate_on();
    let period = (0.0101f32 as f64 + 0.0203f32 as f64) * SR;
    let cycles = 200;
    let v = samples(&mut e, (cycles as f64 * period) as usize + 1_000);
    // Find the last few peaks (phase resets jump up across the mean).
    let mut peaks = Vec::new();
    for i in 1..v.len() - 1 {
        if v[i] >= v[i - 1] && v[i] > v[i + 1] && v[i] > 0.9 {
            peaks.push(i);
        }
    }
    let n = peaks.len() - 1;
    let measured = (peaks[n] - peaks[0]) as f64 / n as f64;
    assert!(
        (measured - period).abs() < 0.05,
        "measured {measured} vs {period}"
    );
}

#[test]
fn loop_time_holds_before_restarting() {
    let mut e = env(|s| {
        s.loop_mode = LoopMode::Loop;
        s.loop_span = LoopSpan::AttackDecaySustain;
        s.loop_time = 0.01;
    });
    e.gate_on();
    let v = samples(&mut e, ATTACK + DECAY + 480 + ATTACK + 1);
    let hold_mid = ATTACK + DECAY + 240;
    assert!((v[hold_mid] - 0.5).abs() < 1e-3);
    assert!(
        (v[ATTACK + DECAY + 480 + ATTACK] - 1.0).abs() < 2e-3,
        "restarted"
    );
}

#[test]
fn an_adr_loop_runs_the_release_too() {
    let mut e = env(|s| {
        s.loop_mode = LoopMode::Loop;
        s.loop_span = LoopSpan::AttackDecayRelease;
    });
    e.gate_on();
    let v = samples(&mut e, ATTACK + DECAY + RELEASE + ATTACK + 1);
    assert!((v[ATTACK + DECAY + RELEASE] - 0.0).abs() < 2e-3, "released");
    assert!(
        (v[ATTACK + DECAY + RELEASE + ATTACK] - 1.0).abs() < 2e-3,
        "restarted"
    );
}

#[test]
fn looping_stops_when_the_gate_closes() {
    let mut e = env(|s| s.loop_mode = LoopMode::Loop);
    e.gate_on();
    samples(&mut e, ATTACK);
    e.gate_off();
    samples(&mut e, RELEASE + 2);
    assert_eq!(e.stage(), EnvStage::Idle);
}

#[test]
fn trigger_ignores_the_gate_and_runs_through() {
    let mut e = env(|s| s.loop_mode = LoopMode::Trigger);
    e.gate_on();
    samples(&mut e, 100);
    e.gate_off();
    assert_eq!(e.stage(), EnvStage::Attack);
    let v = samples(&mut e, ATTACK + DECAY + RELEASE);
    assert!(v.iter().any(|x| *x > 0.99));
    assert_eq!(e.stage(), EnvStage::Idle, "doesn't wait for a gate");
}

#[test]
fn beat_mode_repeats_at_the_beat_interval_from_the_note() {
    let mut clock = BeatClock::new(120.0, SR);
    clock.playing = false;
    let mut e = env(|s| {
        s.loop_mode = LoopMode::Beat;
        s.repeat_beats = 0.5;
        s.sustain = 0.0;
    });
    e.set_clock(&clock);
    e.gate_on();
    // Half a beat at 120 BPM is 12 000 samples.
    let v = samples(&mut e, 36_001);
    for repeat in [12_000usize, 24_000, 36_000] {
        assert!(v[repeat - 1] < 0.001, "silent before repeat {repeat}");
        assert!(v[repeat + ATTACK / 2 - 1000] < 1.0);
    }
    assert!(v[12_000 + 480] > 0.99);
    assert!(v[24_000 + 480] > 0.99);
    assert!(v[35_000] < 0.001);
}

#[test]
fn sync_mode_restarts_on_the_song_grid() {
    let mut clock = BeatClock::new(120.0, SR);
    clock.playing = true;
    clock.beat = 0.3;
    let mut e = env(|s| {
        s.loop_mode = LoopMode::Sync;
        s.repeat_beats = 1.0;
        s.sustain = 0.0;
    });
    e.set_clock(&clock);
    e.gate_on();
    // The next whole beat is 0.7 beats away.
    let to_grid = (0.7 * 24_000.0f64).round() as usize;
    let v = samples(&mut e, to_grid + 1_000);
    assert!(v[to_grid - 1] < 0.001);
    assert!(v[to_grid + 480] > 0.99, "peaked one attack after the grid");
}

#[test]
fn sync_falls_back_to_beat_while_stopped() {
    let clock = BeatClock::new(120.0, SR);
    let mut e = env(|s| {
        s.loop_mode = LoopMode::Sync;
        s.repeat_beats = 0.5;
        s.sustain = 0.0;
    });
    e.set_clock(&clock);
    e.gate_on();
    let v = samples(&mut e, 12_000 + 500);
    assert!(v[12_000 + 480] > 0.99);
}

#[test]
fn retriggering_starts_from_the_current_level() {
    let mut e = env(|_| {});
    e.gate_on();
    let before = samples(&mut e, ATTACK / 2);
    e.gate_on();
    let after = e.next();
    assert!((after - before[ATTACK / 2 - 1]).abs() < 0.01);
}

#[test]
fn release_starts_from_the_current_level() {
    let mut e = env(|_| {});
    e.gate_on();
    let before = samples(&mut e, ATTACK / 2);
    e.gate_off();
    let after = e.next();
    assert!((after - before[ATTACK / 2 - 1]).abs() < 0.01);
}

#[test]
fn stage_times_can_change_while_running() {
    let mut e = env(|_| {});
    e.gate_on();
    samples(&mut e, 100);
    let mut s = *e.settings();
    s.attack = 1.0;
    e.set(&s);
    let v = e.next();
    assert!(v.is_finite() && v < 1.0);
}

#[test]
fn kill_silences_at_once() {
    let mut e = env(|_| {});
    e.gate_on();
    samples(&mut e, ATTACK);
    e.kill();
    assert_eq!(e.next(), 0.0);
    assert!(!e.is_active());
}
