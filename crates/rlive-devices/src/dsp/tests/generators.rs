use super::SR;
use crate::dsp::ir::*;
use crate::dsp::wavetable::*;

fn spec(kind: IrKind) -> IrSpec {
    IrSpec {
        kind,
        decay: 0.6,
        damping: 0.4,
        seed: 42,
        ..Default::default()
    }
}

const IR_KINDS: [IrKind; 5] = [
    IrKind::Room,
    IrKind::Hall,
    IrKind::Plate,
    IrKind::Spring,
    IrKind::Cabinet,
];

#[test]
fn responses_are_finite_normalised_and_repeatable() {
    for kind in IR_KINDS {
        let a = generate_ir(&spec(kind), SR);
        let b = generate_ir(&spec(kind), SR);
        assert_eq!(a.left, b.left, "{kind:?} is deterministic");
        assert_eq!(a.left.len(), a.right.len());
        assert!(a.left.iter().chain(&a.right).all(|x| x.is_finite()));
        let peak = a
            .left
            .iter()
            .chain(&a.right)
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 1.0).abs() < 1e-5, "{kind:?} peak {peak}");
    }
}

#[test]
fn a_different_seed_is_a_different_response() {
    let mut other = spec(IrKind::Hall);
    other.seed = 43;
    assert_ne!(
        generate_ir(&spec(IrKind::Hall), SR).left,
        generate_ir(&other, SR).left
    );
}

/// Seconds for the energy in `x` to fall 60 dB, from Schroeder integration, by the slope
/// between -5 and -35 dB.
fn t60(x: &[f32]) -> f32 {
    let mut energy = 0.0f64;
    let mut curve = vec![0.0f64; x.len()];
    for i in (0..x.len()).rev() {
        energy += (x[i] as f64).powi(2);
        curve[i] = energy;
    }
    let total = curve[0];
    let db = |e: f64| 10.0 * (e / total).log10();
    let start = curve.iter().position(|e| db(*e) < -5.0).unwrap();
    let end = curve.iter().position(|e| db(*e) < -35.0).unwrap();
    (end - start) as f32 / SR as f32 * 2.0
}

#[test]
fn reverberant_kinds_decay_at_the_requested_rate() {
    for kind in [IrKind::Room, IrKind::Hall, IrKind::Plate] {
        let mut s = spec(kind);
        s.damping = 0.0;
        for decay in [0.4f32, 1.2] {
            s.decay = decay;
            let ir = generate_ir(&s, SR);
            let got = t60(&ir.left);
            assert!(
                (got - decay).abs() < 0.25 * decay,
                "{kind:?} wanted {decay} got {got}"
            );
        }
    }
}

#[test]
fn the_tail_is_decorrelated_between_channels() {
    for kind in [IrKind::Room, IrKind::Hall, IrKind::Plate, IrKind::Spring] {
        let ir = generate_ir(&spec(kind), SR);
        let cov: f64 = ir
            .left
            .iter()
            .zip(&ir.right)
            .map(|(a, b)| (a * b) as f64)
            .sum();
        let l: f64 = ir.left.iter().map(|a| (a * a) as f64).sum();
        let r: f64 = ir.right.iter().map(|a| (a * a) as f64).sum();
        let corr = cov / (l * r).sqrt();
        assert!(corr.abs() < 0.8, "{kind:?} correlation {corr}");
    }
}

#[test]
fn predelay_shifts_the_response() {
    let mut s = spec(IrKind::Plate);
    s.predelay = 0.05;
    let ir = generate_ir(&s, SR);
    let gap = (0.05 * SR) as usize;
    assert!(ir.left[..gap].iter().all(|x| *x == 0.0));
    assert!(ir.left[gap..gap + 2_000].iter().any(|x| *x != 0.0));
}

#[test]
fn a_cabinet_is_short_and_rolls_off() {
    let ir = generate_ir(&spec(IrKind::Cabinet), SR);
    assert!(ir.left.len() < SR as usize / 10);
    let tone = |hz: f64| -> f64 {
        let w = std::f64::consts::TAU * hz / SR;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, x) in ir.left.iter().enumerate() {
            re += *x as f64 * (w * i as f64).cos();
            im += *x as f64 * (w * i as f64).sin();
        }
        (re * re + im * im).sqrt()
    };
    assert!(tone(1_400.0) > tone(16_000.0) * 8.0);
}

#[test]
fn generated_responses_work_with_the_convolver() {
    use crate::dsp::convolution::*;
    use std::sync::Arc;
    let ir = generate_ir(&spec(IrKind::Room), SR);
    let mut c = Convolver::with_ir(Arc::new(ConvolverIr::new(&ir.left, 256)));
    let mut input = vec![0.0; 48_000];
    input[0] = 1.0;
    let mut output = vec![0.0; 48_000];
    c.process(&input, &mut output);
    assert!(output.iter().all(|x| x.is_finite()));
    assert!(output.iter().any(|x| x.abs() > 0.1));
}

const TABLE_KINDS: [TableKind; 6] = [
    TableKind::BasicShapes,
    TableKind::Harmonics,
    TableKind::PulseWidth,
    TableKind::Fm,
    TableKind::Formants,
    TableKind::Spectral,
];

#[test]
fn tables_are_bounded_finite_and_repeatable() {
    for kind in TABLE_KINDS {
        let table = generate_wavetable(kind, 8, 5);
        let other = generate_wavetable(kind, 8, 5);
        for p in 0..1_000 {
            let phase = p as f32 / 1_000.0;
            let a = table.read(phase, phase, 0.001);
            assert!(a.is_finite() && a.abs() <= 1.05, "{kind:?} {a}");
            assert_eq!(a, other.read(phase, phase, 0.001), "{kind:?}");
        }
    }
}

#[test]
fn a_table_has_no_dc_and_a_real_signal() {
    for kind in TABLE_KINDS {
        let table = generate_wavetable(kind, 4, 5);
        let v: Vec<f32> = (0..4_096)
            .map(|i| table.read(i as f32 / 4_096.0, 0.5, 0.002))
            .collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        assert!(mean.abs() < 0.02, "{kind:?} dc {mean}");
        assert!(rlive_testkit::rms(&v) > 0.05, "{kind:?}");
    }
}

#[test]
fn a_high_note_does_not_alias() {
    // A saw at 5 kHz: harmonics 5 and up (25 kHz+) must not fold back in.
    let table = generate_wavetable(TableKind::BasicShapes, 16, 0);
    let dt = 5_000.0f32 / SR as f32;
    let mut osc = WavetableOsc::default();
    // Full saw is at position 2/3 of the sweep.
    let v: Vec<f32> = (0..48_000)
        .map(|_| osc.next(&table, 2.0 / 3.0, dt))
        .collect();
    let fundamental = rlive_testkit::tone_amplitude(&v, 5_000.0, SR);
    let alias_bin = 48_000.0 - 25_000.0;
    let alias = rlive_testkit::tone_amplitude(&v, alias_bin, SR);
    assert!(fundamental > 0.3, "{fundamental}");
    assert!(alias < fundamental * 1e-3, "{alias}");
}

#[test]
fn low_notes_keep_their_harmonics() {
    let table = generate_wavetable(TableKind::BasicShapes, 16, 0);
    let dt = 100.0f32 / SR as f32;
    let mut osc = WavetableOsc::default();
    let v: Vec<f32> = (0..48_000)
        .map(|_| osc.next(&table, 2.0 / 3.0, dt))
        .collect();
    // 100 Hz saw: the 50th harmonic at 5 kHz should still be there, at about 1/50.
    let h1 = rlive_testkit::tone_amplitude(&v, 100.0, SR);
    let h50 = rlive_testkit::tone_amplitude(&v, 5_000.0, SR);
    assert!(h50 > h1 / 80.0, "{h1} {h50}");
}

#[test]
fn sweeping_the_position_changes_the_wave_smoothly() {
    let table = generate_wavetable(TableKind::Harmonics, 16, 0);
    let start = table.read(0.13, 0.0, 0.001);
    let end = table.read(0.13, 1.0, 0.001);
    assert_ne!(start, end);
    let mut last = table.read(0.13, 0.0, 0.001);
    for i in 1..=1_000 {
        let v = table.read(0.13, i as f32 / 1_000.0, 0.001);
        assert!((v - last).abs() < 0.05, "at {i}");
        last = v;
    }
}

#[test]
fn from_frames_accepts_custom_cycles() {
    let cycle: Vec<f32> = (0..2_048)
        .map(|i| (std::f32::consts::TAU * i as f32 / 2_048.0).sin())
        .collect();
    let table = Wavetable::from_frames(&[cycle], 2_048);
    assert_eq!(table.frames(), 1);
    assert!((table.read(0.25, 0.0, 0.001) - 1.0).abs() < 0.01);
}
