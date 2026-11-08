use super::SR;
use crate::dsp::follower::DetectMode;
use crate::dsp::sidechain::*;
use rlive_core::{AudioBuffer, AudioInput};
use rlive_testkit::{rms, sine};

fn detector(f: impl FnOnce(&mut SidechainSettings)) -> SidechainDetector {
    let mut s = SidechainSettings::default();
    f(&mut s);
    let mut d = SidechainDetector::new();
    d.prepare(SR, 4_800);
    d.set(&s);
    d.reset();
    d
}

#[test]
fn without_an_external_signal_the_main_signal_is_the_key() {
    let mut d = detector(|_| {});
    let main = sine(1_000.0, SR, 4_800);
    let key = d.process(&main, &main, None);
    assert_eq!(key.left, &main[..]);
}

#[test]
fn gain_and_mix_blend_main_and_external() {
    let mut d = detector(|s| {
        s.gain_db = -6.020_6;
        s.mix = 0.25;
    });
    let main = vec![1.0f32; 4_800];
    let ext = vec![-1.0f32; 4_800];
    let key = d.process(
        &main,
        &main,
        Some(AudioInput {
            left: &ext,
            right: &ext,
        }),
    );
    // 0.75 of the main signal plus 0.25 of half the external one.
    assert!(
        (key.left[4_799] - 0.625).abs() < 1e-3,
        "{}",
        key.left[4_799]
    );
}

#[test]
fn moving_the_gain_ramps_across_the_block() {
    let mut d = detector(|s| s.mix = 1.0);
    let main = vec![0.0f32; 64];
    let ext = vec![1.0f32; 64];
    d.process(
        &main,
        &main,
        Some(AudioInput {
            left: &ext,
            right: &ext,
        }),
    );
    let mut s = *d.settings();
    s.gain_db = -20.0;
    d.set(&s);
    let key = d.process(
        &main,
        &main,
        Some(AudioInput {
            left: &ext,
            right: &ext,
        }),
    );
    assert!(key.left[0] > 0.9 && key.left[63] < 0.11);
    assert!(rlive_testkit::max_step(key.left) < 0.05);
}

#[test]
fn the_eq_shapes_the_key() {
    let measure = |eq: SidechainEq, hz: f32| {
        let mut d = detector(|s| {
            s.eq = eq;
            s.eq_freq = 500.0;
            s.eq_q = 1.0;
            s.eq_gain_db = 12.0;
        });
        let main = sine(hz, SR, 4_800);
        let mut last = 0.0;
        for _ in 0..4 {
            let key = d.process(&main, &main, None);
            last = rms(&key.left[2_400..]);
        }
        last / std::f32::consts::FRAC_1_SQRT_2
    };
    assert!((measure(SidechainEq::Off, 5_000.0) - 1.0).abs() < 0.02);
    assert!(measure(SidechainEq::LowPass, 5_000.0) < 0.05);
    assert!(measure(SidechainEq::HighPass, 50.0) < 0.05);
    assert!((measure(SidechainEq::BandPass, 500.0) - 1.0).abs() < 0.05);
    assert!(measure(SidechainEq::BandPass, 8_000.0) < 0.1);
    assert!(measure(SidechainEq::Notch, 500.0) < 0.05);
    assert!(measure(SidechainEq::Peak, 500.0) > 3.0);
    assert!(measure(SidechainEq::LowShelf, 50.0) > 3.0);
    assert!(measure(SidechainEq::HighShelf, 15_000.0) > 3.0);
}

#[test]
fn the_detector_follows_the_key_level() {
    let mut d = detector(|s| {
        s.attack = 0.001;
        s.release = 0.05;
        s.detect = DetectMode::Peak;
    });
    let main = sine(1_000.0, SR, 4_800);
    let key = d.process(&main, &main, None);
    assert!(
        (key.level[4_799] - 1.0).abs() < 0.05,
        "{}",
        key.level[4_799]
    );
}

#[test]
fn listen_replaces_the_output_with_the_key() {
    let mut d = detector(|s| s.listen = true);
    let ext = vec![0.25f32; 16];
    d.process(
        &[0.0; 16],
        &[0.0; 16],
        Some(AudioInput {
            left: &ext,
            right: &ext,
        }),
    );
    let (mut l, mut r) = (vec![1.0f32; 16], vec![1.0f32; 16]);
    d.listen(&mut AudioBuffer {
        left: &mut l,
        right: &mut r,
    });
    assert_eq!(l, vec![0.25; 16]);

    let mut d = detector(|s| s.listen = false);
    d.process(&ext, &ext, None);
    let (mut l, mut r) = (vec![1.0f32; 16], vec![1.0f32; 16]);
    d.listen(&mut AudioBuffer {
        left: &mut l,
        right: &mut r,
    });
    assert_eq!(l, vec![1.0; 16]);
}

#[test]
fn bad_input_is_contained() {
    let mut d = detector(|s| s.eq = SidechainEq::LowPass);
    let main = vec![f32::NAN; 64];
    let key = d.process(&main, &main, None);
    assert!(key.left.iter().chain(key.level).all(|x| x.is_finite()));
}
