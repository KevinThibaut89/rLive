use super::SR;
use crate::dsp::follower::*;
use rlive_testkit::sine;

fn follower(attack: f32, release: f32, mode: DetectMode) -> EnvelopeFollower {
    let mut f = EnvelopeFollower::new();
    f.prepare(SR);
    f.set_mode(mode);
    f.set_times(attack, release);
    f
}

#[test]
fn a_step_reaches_63_percent_in_the_attack_time() {
    let mut f = follower(0.01, 0.1, DetectMode::Peak);
    let mut y = 0.0;
    for _ in 0..480 {
        y = f.process(1.0);
    }
    assert!((y - (1.0 - (-1.0f32).exp())).abs() < 0.01, "{y}");
}

#[test]
fn it_falls_to_37_percent_in_the_release_time() {
    let mut f = follower(0.0, 0.1, DetectMode::Peak);
    f.process(1.0);
    let mut y = 0.0;
    for _ in 0..4_800 {
        y = f.process(0.0);
    }
    assert!((y - (-1.0f32).exp()).abs() < 0.01, "{y}");
}

#[test]
fn peak_detection_follows_a_sines_level_and_rms_its_rms() {
    let s = sine(440.0, SR, 48_000);
    let mut peak = follower(0.001, 0.2, DetectMode::Peak);
    let mut rms = follower(0.05, 0.05, DetectMode::Rms);
    assert!((peak.process_block(&s) - 1.0).abs() < 0.05);
    assert!((rms.process_block(&s) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02);
}

#[test]
fn bad_input_is_ignored() {
    let mut f = follower(0.01, 0.01, DetectMode::Peak);
    f.process(0.5);
    assert!(f.process(f32::NAN).is_finite());
    f.reset();
    assert_eq!(f.level(), 0.0);
}
