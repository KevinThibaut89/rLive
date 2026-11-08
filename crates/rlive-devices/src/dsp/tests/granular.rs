use super::SR;
use crate::dsp::granular::*;
use rlive_testkit::{max_step, peak, sine};

fn engine(f: impl FnOnce(&mut GrainSettings)) -> GranularEngine {
    let mut s = GrainSettings {
        rate: 40.0,
        size: 0.1,
        position: 0.05,
        ..Default::default()
    };
    f(&mut s);
    let mut e = GranularEngine::new(32, 3);
    e.prepare(SR);
    e.set(&s);
    e
}

fn run_ring(e: &mut GranularEngine, input: &[f32]) -> Vec<f32> {
    let mut ring = GrainRing::new(96_000);
    input
        .iter()
        .map(|x| {
            ring.write(*x);
            e.next(&ring)[0]
        })
        .collect()
}

#[test]
fn grains_fade_from_and_to_silence() {
    // One grain in isolation, over a constant signal: its first and last samples are tiny.
    let mut e = engine(|s| {
        s.rate = 2.0;
        s.size = 0.05;
    });
    let mut ring = GrainRing::new(48_000);
    for _ in 0..4_000 {
        ring.write(1.0);
    }
    let frames = (0.05 * SR) as usize;
    let mut out = Vec::new();
    for _ in 0..frames + 100 {
        ring.write(1.0);
        out.push(e.next(&ring)[0]);
    }
    assert!(out[0].abs() < 0.01, "start {}", out[0]);
    assert!(out[frames - 1].abs() < 0.01, "end {}", out[frames - 1]);
    let top = peak(&out);
    assert!((top - 1.0).abs() < 0.02, "the window reaches unity: {top}");
}

#[test]
fn grain_boundaries_have_no_clicks() {
    // Max step of a smooth window on a smooth signal is bounded by their slopes, grain or not.
    for (rate, size) in [(5.0f32, 0.05f32), (40.0, 0.1), (200.0, 0.05)] {
        for pitch in [0.5f32, 1.0, 1.5, 2.0] {
            let mut e = engine(|s| {
                s.rate = rate;
                s.size = size;
                s.pitch = pitch;
                s.position = 0.3;
            });
            let tone = sine(220.0, SR, 96_000);
            let out = run_ring(&mut e, &tone);
            let tail = &out[24_000..];
            assert!(tail.iter().all(|x| x.is_finite()));
            // A 220 Hz sine moves at most 0.029 per sample and a grain's edge at most a little
            // more; pitch scales the tone. Grains are level-normalised. Anything bigger is a
            // click.
            let limit = (0.029 * pitch.max(1.0) + 0.005) * 1.5;
            assert!(
                max_step(tail) < limit,
                "rate {rate} size {size} pitch {pitch}: {} vs {limit}",
                max_step(tail)
            );
        }
    }
}

#[test]
fn overlapping_grains_keep_the_level_of_a_steady_signal() {
    let mut e = engine(|s| {
        s.rate = 200.0;
        s.size = 0.1;
    });
    let out = run_ring(&mut e, &vec![0.5; 48_000]);
    let tail = &out[24_000..];
    let mean = tail.iter().sum::<f32>() / tail.len() as f32;
    assert!((mean - 0.5).abs() < 0.05, "{mean}");
    let (min, max) = tail
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
    assert!(max - min < 0.06, "{min}..{max}");
}

#[test]
fn pitch_reads_faster_and_stays_inside_the_ring() {
    // Grains 640 samples apart: a whole number of the source's cycles, so they add up.
    let mut e = engine(|s| {
        s.rate = 75.0;
        s.size = 0.05;
        s.pitch = 2.0;
        s.position = 0.2;
    });
    let tone = sine(300.0, SR, 96_000);
    let out = run_ring(&mut e, &tone);
    let tail = &out[24_000..];
    let up = rlive_testkit::tone_amplitude(tail, 600.0, SR);
    let orig = rlive_testkit::tone_amplitude(tail, 300.0, SR);
    assert!(up > orig * 4.0, "{up} {orig}");
    assert!(tail.iter().all(|x| x.is_finite()));
}

#[test]
fn a_short_ring_cannot_be_read_ahead_of_the_write_head() {
    let mut e = engine(|s| {
        s.pitch = 8.0;
        s.size = 0.5;
        s.position = 0.0;
    });
    let mut ring = GrainRing::new(2_000);
    for _ in 0..48_000 {
        ring.write(0.25);
        let [l, r] = e.next(&ring);
        assert!(l.is_finite() && r.is_finite());
    }
}

#[test]
fn grains_play_static_samples_forwards_and_backwards() {
    // On a rising ramp, a forward grain's second half is higher than its first half, and a
    // backward grain's is lower.
    let data: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
    let source = GrainSample { data: &data };
    let halves = |pitch: f32| {
        let mut e = engine(|s| {
            s.pitch = pitch;
            s.position = 0.2;
            s.rate = 1.0;
            s.size = 0.05;
        });
        let out: Vec<f32> = (0..2_400).map(|_| e.next(&source)[0]).collect();
        let first: f32 = out[..1_200].iter().sum();
        let second: f32 = out[1_200..].iter().sum();
        (first, second)
    };
    let (a, b) = halves(1.0);
    assert!(b > a, "{a} {b}");
    let (a, b) = halves(-1.0);
    assert!(b < a, "{a} {b}");
}

#[test]
fn the_grain_pool_limits_density() {
    let mut e = GranularEngine::new(4, 1);
    e.prepare(SR);
    e.set(&GrainSettings {
        rate: 5_000.0,
        size: 0.1,
        ..Default::default()
    });
    let mut ring = GrainRing::new(48_000);
    for _ in 0..5_000 {
        ring.write(0.1);
        e.next(&ring);
    }
    assert_eq!(e.active_grains(), 4);
}

#[test]
fn spray_and_pan_are_repeatable_for_a_seed() {
    let make = |seed| {
        let mut e = GranularEngine::new(16, seed);
        e.prepare(SR);
        e.set(&GrainSettings {
            spray: 0.05,
            pan_spread: 1.0,
            jitter: 0.5,
            ..Default::default()
        });
        let tone = sine(300.0, SR, 24_000);
        let mut ring = GrainRing::new(48_000);
        tone.iter()
            .map(|x| {
                ring.write(*x);
                e.next(&ring)
            })
            .collect::<Vec<[f32; 2]>>()
    };
    assert_eq!(make(1), make(1));
    assert_ne!(make(1), make(2));
}

#[test]
fn reset_silences_every_grain() {
    let mut e = engine(|_| {});
    let mut ring = GrainRing::new(48_000);
    for _ in 0..24_000 {
        ring.write(1.0);
        e.next(&ring);
    }
    assert!(e.active_grains() > 0);
    e.reset();
    assert_eq!(e.active_grains(), 0);
}

#[test]
fn adding_a_block_mixes_into_what_is_there() {
    let data = vec![1.0; 48_000];
    let source = GrainSample { data: &data };
    let mut e = engine(|_| {});
    let (mut l, mut r) = (vec![1.0f32; 64], vec![1.0f32; 64]);
    e.process(&source, &mut l, &mut r);
    assert!(l.iter().all(|x| *x >= 1.0));
}
