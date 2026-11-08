use super::SR;
use crate::dsp::delay::*;
use crate::dsp::util::Rng;
use rlive_testkit::sine;

#[test]
fn whole_sample_reads_are_exact() {
    let mut d = InterpDelay::new(1_024);
    let mut rng = Rng::new(1);
    let input: Vec<f32> = (0..2_000).map(|_| rng.bipolar()).collect();
    for (i, x) in input.iter().enumerate() {
        d.write(*x);
        for delay in [0usize, 1, 100, 1_000] {
            if i >= delay {
                assert_eq!(
                    d.read(delay as f32, Interpolation::Linear),
                    input[i - delay]
                );
                if delay >= 1 {
                    assert_eq!(d.read(delay as f32, Interpolation::Cubic), input[i - delay]);
                }
            }
        }
    }
}

#[test]
fn fractional_reads_interpolate() {
    let mut d = InterpDelay::new(16);
    d.write(0.0);
    d.write(1.0);
    assert_eq!(d.read(0.5, Interpolation::Linear), 0.5);
}

#[test]
fn a_modulated_cubic_read_of_a_sine_is_clean() {
    let f = 200.0f32;
    let tone = sine(f, SR, 48_000);
    let mut d = InterpDelay::new(4_800);
    let mut worst = 0.0f32;
    for (i, x) in tone.iter().enumerate() {
        d.write(*x);
        if i < 4_800 {
            continue;
        }
        let delay = 2_400.0 + 800.0 * (i as f32 * 0.0007).sin();
        let got = d.read(delay, Interpolation::Cubic);
        let t = (i as f32 - delay) / SR as f32;
        let want = (std::f32::consts::TAU * f * t).sin();
        worst = worst.max((got - want).abs());
    }
    assert!(worst < 2e-3, "{worst}");
}

#[test]
fn reads_stay_in_range() {
    let mut d = InterpDelay::new(32);
    d.write(1.0);
    assert!(d.read(1e9, Interpolation::Cubic).is_finite());
    assert!(d.read(-3.0, Interpolation::Linear).is_finite());
}

#[test]
fn no_smear_is_an_exact_delay() {
    let mut d = SmearDelay::new(1_024, 256);
    d.set_smear(0.0);
    for i in 0..500 {
        d.write(i as f32);
    }
    assert_eq!(d.read(100.0), 399.0);
}

#[test]
fn smear_keeps_the_level_of_a_steady_signal() {
    let mut d = SmearDelay::new(1_024, 256);
    d.set_smear(64.0);
    for _ in 0..2_000 {
        d.write(0.5);
    }
    assert!((d.read(300.0) - 0.5).abs() < 1e-5);
}

#[test]
fn smear_spreads_an_impulse_around_the_delay_time() {
    let mut d = SmearDelay::new(1_024, 256);
    d.set_smear(200.0);
    let mut out = Vec::new();
    for i in 0..1_000 {
        d.write(if i == 0 { 1.0 } else { 0.0 });
        out.push(d.read(500.0));
    }
    let nonzero: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, x)| x.abs() > 1e-3)
        .map(|(i, _)| i)
        .collect();
    let (first, last) = (nonzero[0], nonzero[nonzero.len() - 1]);
    assert!(first < 460 && last > 540, "{first}..{last}");
    assert!(first > 380 && last < 620, "{first}..{last}");
    let sum: f32 = out.iter().sum();
    assert!((sum - 1.0).abs() < 1e-2, "energy {sum}");
}
