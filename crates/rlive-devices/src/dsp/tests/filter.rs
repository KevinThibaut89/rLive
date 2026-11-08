use super::SR;
use crate::dsp::filter::*;
use rlive_testkit::{peak, tone_amplitude};

const CIRCUITS: [Circuit; 5] = [
    Circuit::Svf,
    Circuit::Ladder,
    Circuit::Ms2,
    Circuit::Dfm,
    Circuit::Prd,
];

const KINDS: [FilterKind; 4] = [
    FilterKind::LowPass,
    FilterKind::BandPass,
    FilterKind::HighPass,
    FilterKind::Notch,
];

const SLOPES: [FilterSlope; 2] = [FilterSlope::Db12, FilterSlope::Db24];

fn filter(s: &FilterSettings) -> Filter {
    let mut f = Filter::new();
    f.prepare(SR);
    f.set(s);
    f
}

/// The measured steady-state gain at `hz`.
fn measured_gain(s: &FilterSettings, hz: f32) -> f32 {
    let mut f = filter(s);
    // Small enough that every circuit is in its linear region.
    const AMP: f32 = 1e-4;
    let settle = 24_000;
    let n = 48_000;
    let mut out = Vec::with_capacity(n);
    for i in 0..settle + n {
        let x = AMP * (std::f64::consts::TAU * hz as f64 * i as f64 / SR).sin() as f32;
        let y = f.process(x);
        if i >= settle {
            out.push(y);
        }
    }
    (tone_amplitude(&out, hz as f64, SR) / AMP as f64) as f32
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

#[test]
fn every_circuit_matches_its_analytic_magnitude_response() {
    for circuit in CIRCUITS {
        for slope in SLOPES {
            for kind in KINDS {
                for cutoff in [300.0f32, 2_000.0, 9_000.0] {
                    for resonance in [0.0f32, 0.5] {
                        let s = FilterSettings {
                            circuit,
                            slope,
                            kind,
                            cutoff,
                            resonance,
                            ..Default::default()
                        };
                        for ratio in [0.25f32, 0.5, 1.0, 2.0, 4.0] {
                            let hz = (cutoff * ratio).min(20_000.0);
                            let want = s.magnitude(hz, SR as f32);
                            // Notches read as very low numbers; only compare where it matters.
                            if want < 1e-4 {
                                continue;
                            }
                            let got = measured_gain(&s, hz);
                            assert!(
                                (db(got) - db(want)).abs() < 0.25,
                                "{circuit:?} {slope:?} {kind:?} fc {cutoff} res {resonance} at \
                                 {hz} Hz: measured {} dB, analytic {} dB",
                                db(got),
                                db(want)
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn the_analytic_response_is_the_textbook_one() {
    // A 12 dB/oct state-variable low-pass is -6 dB at cutoff (damping 2, no resonance).
    let svf = FilterSettings {
        circuit: Circuit::Svf,
        cutoff: 1_000.0,
        ..Default::default()
    };
    assert!((db(svf.magnitude(1_000.0, SR as f32)) + 6.02).abs() < 0.05);

    // A ladder low-pass is 1 / |1 + j|^4 at cutoff: -12 dB, 24 dB/oct above.
    let ladder = FilterSettings {
        circuit: Circuit::Ladder,
        slope: FilterSlope::Db24,
        cutoff: 500.0,
        ..Default::default()
    };
    assert!((db(ladder.magnitude(500.0, SR as f32)) + 12.04).abs() < 0.05);
    let octave =
        db(ladder.magnitude(4_000.0, SR as f32)) - db(ladder.magnitude(8_000.0, SR as f32));
    assert!((octave - 24.0).abs() < 3.0, "{octave} dB/oct");

    // A two-pole Butterworth pair is -3 dB at cutoff.
    let butter = FilterSettings {
        circuit: Circuit::Svf,
        slope: FilterSlope::Db24,
        cutoff: 500.0,
        ..Default::default()
    };
    assert!((db(butter.magnitude(500.0, SR as f32)) + 3.01).abs() < 0.05);
}

#[test]
fn slopes_are_twelve_and_twenty_four_db_per_octave() {
    for circuit in CIRCUITS {
        for (slope, per_octave) in [(FilterSlope::Db12, 12.0), (FilterSlope::Db24, 24.0)] {
            let s = FilterSettings {
                circuit,
                slope,
                cutoff: 200.0,
                ..Default::default()
            };
            let drop = db(s.magnitude(3_200.0, SR as f32)) - db(s.magnitude(6_400.0, SR as f32));
            assert!(
                (drop - per_octave).abs() < 2.5,
                "{circuit:?} {slope:?}: {drop} dB/oct"
            );
        }
    }
}

#[test]
fn resonance_peaks_at_the_cutoff() {
    for circuit in CIRCUITS {
        for slope in SLOPES {
            let flat = FilterSettings {
                circuit,
                slope,
                cutoff: 1_000.0,
                ..Default::default()
            };
            let resonant = FilterSettings {
                resonance: 0.8,
                ..flat
            };
            assert!(
                resonant.magnitude(1_000.0, SR as f32) > 1.5 * flat.magnitude(1_000.0, SR as f32),
                "{circuit:?} {slope:?}"
            );
        }
    }
}

#[test]
fn morph_passes_through_each_kind() {
    for circuit in CIRCUITS {
        for slope in SLOPES {
            let base = FilterSettings {
                circuit,
                slope,
                kind: FilterKind::Morph,
                cutoff: 800.0,
                resonance: 0.3,
                ..Default::default()
            };
            for (morph, kind) in [
                (0.0, FilterKind::LowPass),
                (0.25, FilterKind::BandPass),
                (0.5, FilterKind::HighPass),
                (0.75, FilterKind::Notch),
            ] {
                let fixed = FilterSettings { kind, ..base };
                for hz in [200.0, 800.0, 3_000.0] {
                    let got = FilterSettings { morph, ..base }.magnitude(hz, SR as f32);
                    let want = fixed.magnitude(hz, SR as f32);
                    assert!((got - want).abs() < 1e-4, "{circuit:?} {slope:?} {morph}");
                }
            }
        }
    }
}

#[test]
fn morph_sweeps_without_a_jump() {
    let mut f = filter(&FilterSettings {
        circuit: Circuit::Ladder,
        slope: FilterSlope::Db24,
        kind: FilterKind::Morph,
        cutoff: 500.0,
        resonance: 0.5,
        ..Default::default()
    });
    let mut steps = Vec::new();
    let mut last = 0.0f32;
    for i in 0..48_000usize {
        let morph = i as f32 / 12_000.0;
        let mut s = *f.settings();
        s.morph = morph;
        f.set(&s);
        let x = 0.1 * (std::f32::consts::TAU * 300.0 * i as f32 / SR as f32).sin();
        let y = f.process(x);
        if i > 100 {
            steps.push((y - last).abs());
        }
        last = y;
    }
    let worst = steps.iter().copied().fold(0.0f32, f32::max);
    assert!(worst < 0.3, "{worst}");
}

fn resonance_sweep(circuit: Circuit, slope: FilterSlope, kind: FilterKind, drive: f32) -> Vec<f32> {
    let mut f = filter(&FilterSettings {
        circuit,
        slope,
        kind,
        resonance: 1.0,
        drive,
        ..Default::default()
    });
    let mut rng = crate::dsp::util::Rng::new(3);
    let mut out = Vec::with_capacity(96_000);
    for i in 0..96_000usize {
        if i % 48 == 0 {
            // Sweep 20 Hz to 24 kHz and back, fast.
            let t = (i as f32 / 96_000.0 * std::f32::consts::TAU * 40.0).sin() * 0.5 + 0.5;
            let mut s = *f.settings();
            s.cutoff = 20.0 * 1_200f32.powf(t);
            f.set(&s);
        }
        let x = if i < 4 { 1.0 } else { rng.bipolar() * 0.5 };
        out.push(f.process(x));
    }
    out
}

#[test]
fn full_resonance_stays_bounded_while_sweeping() {
    for circuit in CIRCUITS {
        for slope in SLOPES {
            for kind in [FilterKind::LowPass, FilterKind::Morph] {
                for drive in [0.0, 24.0] {
                    let out = resonance_sweep(circuit, slope, kind, drive);
                    assert!(out.iter().all(|x| x.is_finite()), "{circuit:?} {slope:?}");
                    let p = peak(&out);
                    assert!(p < 64.0, "{circuit:?} {slope:?} {kind:?} {drive} peak {p}");
                }
            }
        }
    }
}

#[test]
fn extreme_inputs_do_not_break_any_filter() {
    for circuit in CIRCUITS {
        for slope in SLOPES {
            let mut f = filter(&FilterSettings {
                circuit,
                slope,
                resonance: 1.0,
                cutoff: 23_500.0,
                drive: 60.0,
                ..Default::default()
            });
            for i in 0..48_000usize {
                let x = match i % 7 {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    2 => -1e20,
                    3 => 1e-40,
                    _ => 1e3,
                };
                assert!(f.process(x).is_finite(), "{circuit:?} {slope:?}");
            }
            // Recovers: quiet input gets quiet output.
            f.reset();
            let v: Vec<f32> = (0..1000).map(|_| f.process(0.0)).collect();
            assert!(peak(&v) < 1e-3, "{circuit:?} {slope:?}");
        }
    }
}

#[test]
fn self_oscillating_circuits_ring_at_the_cutoff() {
    for circuit in [Circuit::Ladder, Circuit::Ms2, Circuit::Dfm, Circuit::Prd] {
        for slope in SLOPES {
            let mut f = filter(&FilterSettings {
                circuit,
                slope,
                resonance: 1.0,
                cutoff: 800.0,
                ..Default::default()
            });
            let mut out = vec![0.0; 96_000];
            for (i, y) in out.iter_mut().enumerate() {
                *y = f.process(if i == 0 { 1.0 } else { 0.0 });
            }
            let tail = &out[48_000..];
            let level = peak(tail);
            assert!(level > 0.02, "{circuit:?} {slope:?} level {level}");
            let crossings = tail
                .windows(2)
                .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
                .count();
            let hz = crossings as f32;
            assert!(
                (hz - 800.0).abs() < 100.0,
                "{circuit:?} {slope:?} rings at {hz} Hz"
            );
        }
    }
}

#[test]
fn drive_adds_harmonics_to_the_circuits_that_saturate() {
    let thd = |circuit, drive| {
        let mut f = filter(&FilterSettings {
            circuit,
            slope: FilterSlope::Db24,
            cutoff: 8_000.0,
            drive,
            ..Default::default()
        });
        let input = rlive_testkit::sine(200.0, SR, 96_000);
        let out: Vec<f32> = input.iter().map(|x| f.process(*x * 0.8)).collect();
        let tail = &out[48_000..];
        (tone_amplitude(tail, 600.0, SR) / tone_amplitude(tail, 200.0, SR)) as f32
    };
    for circuit in CIRCUITS {
        assert!(thd(circuit, 36.0) > thd(circuit, 0.0) * 1.5, "{circuit:?}");
    }
}

#[test]
fn cutoff_stays_tuned_near_nyquist() {
    // The trapezoidal warp pins the -3 dB/-6 dB point exactly, even at 20 kHz of 48 kHz.
    for cutoff in [1_000.0f32, 20_000.0] {
        let s = FilterSettings {
            circuit: Circuit::Svf,
            cutoff,
            ..Default::default()
        };
        assert!((db(s.magnitude(cutoff, SR as f32)) + 6.02).abs() < 0.05);
        assert!(
            (db(measured_gain(&s, cutoff)) + 6.02).abs() < 0.15,
            "{cutoff}"
        );
    }
}
