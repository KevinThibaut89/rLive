use super::SR;
use crate::dsp::lfo::*;
use crate::dsp::tempo::{BeatClock, SyncRate};
use rlive_core::TransportInfo;

fn lfo(f: impl FnOnce(&mut LfoSettings)) -> Lfo {
    let mut s = LfoSettings::default();
    f(&mut s);
    let mut l = Lfo::new(1);
    l.prepare(SR);
    l.set(&s);
    l
}

#[test]
fn a_free_lfo_has_the_right_frequency() {
    let mut l = lfo(|s| s.rate = LfoRate::Hz(2.0));
    let v: Vec<f32> = (0..96_000).map(|_| l.next()[0]).collect();
    for i in (0..v.len()).step_by(1000) {
        let want = (std::f32::consts::TAU * 2.0 * i as f32 / SR as f32).sin();
        assert!((v[i] - want).abs() < 1e-3, "at {i}");
    }
}

#[test]
fn a_sync_lfo_locks_to_the_song_position() {
    let mut l = lfo(|s| s.rate = LfoRate::Beats(1.0));
    let mut clock = BeatClock::new(120.0, SR);
    clock.playing = true;
    let mut beat = 0.0;
    for _ in 0..3_000 {
        l.set_clock(&clock);
        // Block-sized steps with a changing tempo: the phase comes only from the song position.
        for _ in 0..512 {
            let want = (std::f64::consts::TAU * beat).sin() as f32;
            let got = l.next()[0];
            assert!((got - want).abs() < 1e-3);
            beat += clock.beats_per_sample;
        }
        clock.advance(512);
    }
}

#[test]
fn a_sync_lfo_follows_tempo_and_song_jumps() {
    let mut l = lfo(|s| s.rate = LfoRate::Beats(4.0));
    let transport = TransportInfo {
        playing: true,
        tempo: 90.0,
        tick: 960.0 * 3.0, // beat 3, three-quarters of a cycle
        sample_rate: SR,
        ..Default::default()
    };
    l.set_clock(&BeatClock::from_transport(&transport));
    let got = l.next()[0];
    let want = (std::f64::consts::TAU * 0.75).sin() as f32;
    assert!((got - want).abs() < 1e-3, "got {got}");

    // A quarter of a cycle more is one beat at 90 BPM: 32 000 samples.
    let mut last = 0.0;
    for _ in 0..32_000 {
        last = l.next()[0];
    }
    assert!(last.abs() < 2e-3, "got {last}");
}

#[test]
fn every_sync_rate_is_the_right_length() {
    let beats = |label: &str| {
        let i = SyncRate::LABELS.iter().position(|l| *l == label).unwrap();
        SyncRate::ALL[i].beats()
    };
    assert_eq!(beats("1/4"), 1.0);
    assert_eq!(beats("1/8"), 0.5);
    assert_eq!(beats("1/16"), 0.25);
    assert!((beats("1/8T") - 1.0 / 3.0).abs() < 1e-9);
    assert_eq!(beats("1/8D"), 0.75);
    assert_eq!(beats("1 Bar"), 4.0);
    assert_eq!(beats("4 Bars"), 16.0);
}

#[test]
fn a_stopped_transport_free_runs_at_the_tempo() {
    let mut l = lfo(|s| s.rate = LfoRate::Beats(1.0));
    l.set_clock(&BeatClock::new(120.0, SR));
    let v: Vec<f32> = (0..24_001).map(|_| l.next()[0]).collect();
    assert!(v[24_000].abs() < 1e-3);
    assert!((v[6_000] - 1.0).abs() < 1e-3, "a quarter of a cycle");
}

#[test]
fn phase_offsets_the_right_channel_and_spin_changes_its_rate() {
    let mut l = lfo(|s| {
        s.rate = LfoRate::Hz(10.0);
        s.phase = 0.25;
    });
    let [left, right] = l.next();
    assert!(left.abs() < 1e-3);
    assert!((right - 1.0).abs() < 1e-3);

    // A 10 Hz left channel and a 15 Hz right one line up again after a fifth of a second.
    let mut l = lfo(|s| {
        s.rate = LfoRate::Hz(10.0);
        s.spin = 0.5;
    });
    let frames: Vec<[f32; 2]> = (0..48_000).map(|_| l.next()).collect();
    let zero_crossings = |f: &dyn Fn(&[f32; 2]) -> f32| {
        frames
            .windows(2)
            .filter(|w| f(&w[0]) <= 0.0 && f(&w[1]) > 0.0)
            .count()
    };
    assert!((9..=11).contains(&zero_crossings(&|f| f[0])));
    assert!((14..=16).contains(&zero_crossings(&|f| f[1])));
}

#[test]
fn shapes_cover_their_range_and_stay_bounded() {
    for shape in [
        LfoShape::Sine,
        LfoShape::Triangle,
        LfoShape::SawUp,
        LfoShape::SawDown,
        LfoShape::Square,
        LfoShape::SampleHold,
        LfoShape::SmoothRandom,
    ] {
        let mut l = lfo(|s| {
            s.shape = shape;
            s.rate = LfoRate::Hz(100.0);
        });
        let v: Vec<f32> = (0..48_000).map(|_| l.next()[0]).collect();
        assert!(v.iter().all(|x| x.abs() <= 1.0 + 1e-6), "{shape:?}");
        let (min, max) = v
            .iter()
            .fold((1.0f32, -1.0f32), |(a, b), x| (a.min(*x), b.max(*x)));
        assert!(min < -0.5 && max > 0.5, "{shape:?} {min} {max}");
    }
}

#[test]
fn sample_and_hold_changes_once_a_cycle_and_repeats_for_a_seed() {
    let make = |seed: u32| {
        let mut l = Lfo::new(seed);
        l.prepare(SR);
        l.set(&LfoSettings {
            shape: LfoShape::SampleHold,
            rate: LfoRate::Hz(100.0),
            ..Default::default()
        });
        (0..4_800).map(|_| l.next()[0]).collect::<Vec<f32>>()
    };
    let v = make(7);
    assert_eq!(v, make(7));
    assert_ne!(v, make(8));
    let changes = v.windows(2).filter(|w| w[0] != w[1]).count();
    assert!((9..=11).contains(&changes), "{changes}");
}

#[test]
fn smooth_random_does_not_jump() {
    let mut l = lfo(|s| {
        s.shape = LfoShape::SmoothRandom;
        s.rate = LfoRate::Hz(10.0);
    });
    let v: Vec<f32> = (0..48_000).map(|_| l.next()[0]).collect();
    assert!(rlive_testkit::max_step(&v) < 0.02);
}

#[test]
fn control_rate_steps_match_sample_rate_steps() {
    let mut a = lfo(|s| s.rate = LfoRate::Hz(3.0));
    let mut b = lfo(|s| s.rate = LfoRate::Hz(3.0));
    for _ in 0..1000 {
        a.next();
    }
    b.advance(1000);
    assert!((a.next()[0] - b.next()[0]).abs() < 1e-5);
}
