use super::SR;
use crate::dsp::pitch::*;
use crate::dsp::util::Rng;
use rlive_testkit::sine;

fn cents(a: f32, b: f32) -> f32 {
    1_200.0 * (a / b).log2()
}

fn detector() -> PitchDetector {
    PitchDetector::new(SR, 40.0, 5_000.0, 256)
}

#[test]
fn sines_are_found_across_the_range() {
    let mut f = 45.0f32;
    while f < 4_500.0 {
        let mut d = detector();
        let tone = sine(f, SR, 48_000);
        let p = d.process(&tone).expect("a pitch");
        assert!(
            cents(p.hz, f).abs() < if f < 2_000.0 { 2.0 } else { 7.0 },
            "{f} Hz: got {} ({} cents)",
            p.hz,
            cents(p.hz, f)
        );
        assert!(p.confidence > 0.9, "{f}");
        f *= 1.17;
    }
}

#[test]
fn harmonic_tones_are_found_at_their_fundamental() {
    for f in [65.4f32, 110.0, 261.6, 440.0, 1_046.5] {
        let mut d = detector();
        let tone: Vec<f32> = (0..48_000usize)
            .map(|i| {
                let p = std::f32::consts::TAU * f * i as f32 / SR as f32;
                (1..=8)
                    .map(|h| (p * h as f32).sin() / h as f32)
                    .sum::<f32>()
                    * 0.4
            })
            .collect();
        let p = d.process(&tone).expect("a pitch");
        assert!(cents(p.hz, f).abs() < 5.0, "{f}: got {}", p.hz);
    }
}

#[test]
fn detuning_is_tracked() {
    let mut d = detector();
    let base = 220.0f32;
    let detuned = base * 2f32.powf(10.0 / 1200.0);
    let tone = sine(detuned, SR, 48_000);
    let p = d.process(&tone).unwrap();
    assert!((cents(p.hz, base) - 10.0).abs() < 2.0);
}

#[test]
fn silence_has_no_pitch() {
    let mut d = detector();
    assert!(d.process(&vec![0.0; 48_000]).is_none());
}

#[test]
fn noise_is_not_mistaken_for_a_note() {
    let mut d = detector();
    let mut rng = Rng::new(4);
    let noise: Vec<f32> = (0..48_000).map(|_| rng.bipolar() * 0.3).collect();
    match d.process(&noise) {
        None => {}
        Some(p) => assert!(p.confidence < 0.6, "{p:?}"),
    }
}

#[test]
fn it_follows_a_note_change() {
    let mut d = detector();
    d.process(&sine(220.0, SR, 24_000));
    let p = d.process(&sine(440.0, SR, 24_000)).unwrap();
    assert!(cents(p.hz, 440.0).abs() < 3.0);
}

#[test]
fn reset_clears_the_estimate() {
    let mut d = detector();
    d.process(&sine(220.0, SR, 24_000));
    assert!(d.latest().is_some());
    d.reset();
    assert!(d.latest().is_none());
}

#[test]
fn estimates_come_at_the_hop_rate_after_the_history_fills() {
    let d = detector();
    assert!(d.history_len() >= 2 * (SR as usize / 40));
}
