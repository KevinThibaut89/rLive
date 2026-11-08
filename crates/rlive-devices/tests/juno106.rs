//! Checks the Juno-106 model against figures printed in its service notes.

use rlive_core::{MidiEvent, MidiMessage, Processor, TransportInfo};
use rlive_devices::juno106::{p, Juno106};
use rlive_testkit::Harness;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn on(key: u8) -> MidiEvent {
    MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        },
    )
}

fn off(key: u8) -> MidiEvent {
    MidiEvent::new(
        0,
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity: 0.0,
            note_id: 0,
        },
    )
}

fn run(j: &mut Juno106, events: &[MidiEvent], seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        ..Default::default()
    };
    let mut h = Harness::new(transport.sample_rate);
    h.transport = transport;
    let blocks = (seconds * SR / BLOCK as f64).ceil() as usize;
    let (mut ol, mut or) = (Vec::new(), Vec::new());
    for b in 0..blocks {
        let mut l = vec![0.0; BLOCK];
        let mut r = vec![0.0; BLOCK];
        h.process(
            &mut *j,
            &mut l,
            &mut r,
            if b == 0 { events } else { &[] },
            &[],
        );
        ol.extend_from_slice(&l);
        or.extend_from_slice(&r);
    }
    (ol, or)
}

/// A machine with the front panel set from `(param, value)` pairs.
fn machine(settings: &[(u32, f64)]) -> Juno106 {
    let mut j = Juno106::new();
    j.set_param(p::INSTABILITY, 0.0);
    j.set_param(p::HISS, 0.0);
    j.set_param(p::CHORUS, 0.0);
    for (id, v) in settings {
        j.set_param(*id, *v);
    }
    j.activate(SR, BLOCK);
    j
}

/// Sub-sample rising zero crossings: the frequency over the last part of `buf`.
fn frequency(buf: &[f32]) -> f64 {
    let mut first = None;
    let mut last = 0.0;
    let mut n = 0usize;
    for (i, w) in buf.windows(2).enumerate() {
        if w[0] <= 0.0 && w[1] > 0.0 {
            let t = i as f64 + (-w[0] / (w[1] - w[0])) as f64;
            if first.is_none() {
                first = Some(t);
            }
            last = t;
            n += 1;
        }
    }
    (n as f64 - 1.0) * SR / (last - first.unwrap())
}

fn peak_to_peak(buf: &[f32]) -> f32 {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for &x in buf {
        lo = lo.min(x);
        hi = hi.max(x);
    }
    hi - lo
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
}

/// Output samples to volts at one voice's VCA output (test point TP8).
fn to_tp8(pp: f32) -> f32 {
    pp * 8.0 / (100.0 / 47.0) / 0.1
}

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// Test bank 3: self-oscillating VCF, no oscillator, full keyboard follow.
fn bank3(freq: f64) -> Vec<(u32, f64)> {
    vec![
        (p::SAW, 0.0),
        (p::VCF_FREQ, freq),
        (p::VCF_RES, 10.0),
        (p::VCF_ENV, 0.0),
        (p::VCF_KYBD, 10.0),
        (p::ENV_A, 0.0),
        (p::ENV_D, 0.0),
        (p::ENV_S, 10.0),
        (p::ENV_R, 0.0),
    ]
}

fn tail(buf: &[f32], seconds: f64) -> &[f32] {
    &buf[buf.len() - (seconds * SR) as usize..]
}

/// Test bank defaults with a held note and an open filter.
fn open_filter(extra: &[(u32, f64)]) -> Vec<(u32, f64)> {
    let mut v = vec![
        (p::VCF_FREQ, 10.0),
        (p::VCF_RES, 0.0),
        (p::VCF_ENV, 0.0),
        (p::VCF_KYBD, 10.0),
        (p::ENV_A, 0.0),
        (p::ENV_D, 0.0),
        (p::ENV_S, 10.0),
        (p::ENV_R, 0.0),
    ];
    v.extend_from_slice(extra);
    v
}

#[test]
fn a4_is_tuned_to_442_hz_and_range_moves_octaves() {
    for (range, expect) in [(0.0, 221.0), (1.0, 442.0), (2.0, 884.0)] {
        let mut j = machine(&open_filter(&[(p::RANGE, range)]));
        let (l, _) = run(&mut j, &[on(69)], 1.0);
        let hz = frequency(tail(&l, 0.5));
        assert!(cents(hz, expect).abs() < 1.0, "{range}: {hz}");
    }
}

#[test]
fn self_oscillation_matches_the_vcf_adjustment_procedure() {
    for (key, expect) in [(60u8, 248.0), (84, 992.0)] {
        let mut j = machine(&bank3(6.3));
        let (l, _) = run(&mut j, &[on(key)], 3.0);
        let t = tail(&l, 1.0);
        let hz = frequency(t);
        assert!(cents(hz, expect).abs() < 10.0, "{key}: {hz}");
    }
}

#[test]
fn self_oscillation_level_matches_the_vca_gain_adjustment() {
    let mut j = machine(&bank3(6.3));
    let (l, _) = run(&mut j, &[on(60)], 3.0);
    let vpp = to_tp8(peak_to_peak(tail(&l, 1.0)));
    assert!((vpp - 6.0).abs() < 0.4, "{vpp}");
}

#[test]
fn noise_level_matches_the_adjustment_procedure() {
    let mut j = machine(&open_filter(&[(p::SAW, 0.0), (p::NOISE, 10.0)]));
    let (l, _) = run(&mut j, &[on(60)], 2.0);
    let vpp = to_tp8(peak_to_peak(tail(&l, 1.0)));
    assert!((3.0..6.0).contains(&vpp), "{vpp}");
}

fn duty(serial: f64, instability: f64, pwm: f64) -> f64 {
    let mut j = machine(&open_filter(&[
        (p::SAW, 0.0),
        (p::PULSE, 1.0),
        (p::PWM_MODE, 1.0),
        (p::PWM, pwm),
        (p::UNIT, serial),
        (p::INSTABILITY, instability),
    ]));
    let (l, _) = run(&mut j, &[on(60)], 1.0);
    let t = tail(&l, 0.5);
    let (lo, hi) = t
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
    let mid = 0.5 * (lo + hi);
    t.iter().filter(|x| **x > mid).count() as f64 / t.len() as f64
}

#[test]
fn pulse_width_matches_the_pwm_adjustment() {
    assert!((duty(1.0, 0.0, 0.0) - 0.50).abs() < 0.01);
    assert!((duty(1.0, 0.0, 10.0) - 0.95).abs() < 0.015);
}

#[test]
fn nominal_units_stay_inside_the_published_pwm_tolerance() {
    for serial in 0..12 {
        let d = duty(serial as f64, 1.0, 0.0);
        assert!((0.47..=0.53).contains(&d), "unit {serial}: {d}");
    }
}

/// RMS of consecutive 10 ms windows.
fn envelope(buf: &[f32]) -> Vec<f32> {
    buf.chunks(480).map(rms).collect()
}

#[test]
fn attack_is_linear_across_the_specified_time() {
    let mut j = machine(&open_filter(&[(p::ENV_A, 10.0), (p::ENV_S, 10.0)]));
    let (l, _) = run(&mut j, &[on(60)], 3.5);
    let e = envelope(&l);
    let full = e[e.len() - 5];
    let half = e[150]; // 1.5 s into a 3 s attack.
    assert!((half / full - 0.5).abs() < 0.08, "{}", half / full);
}

#[test]
fn shortest_attack_is_about_one_cpu_scan() {
    let mut j = machine(&open_filter(&[]));
    let (l, _) = run(&mut j, &[on(60)], 0.2);
    let e = envelope(&l);
    assert!(e[1] > 0.9 * e[10], "{} {}", e[1], e[10]);
}

#[test]
fn full_decay_closes_the_vca_at_the_specified_twelve_seconds() {
    let mut j = machine(&open_filter(&[(p::ENV_D, 10.0), (p::ENV_S, 0.0)]));
    let (l, _) = run(&mut j, &[on(60)], 13.5);
    let e = envelope(&l);
    let db = |i: usize| 20.0 * (e[i] / e[1]).log10();
    assert!((db(600) + 16.5).abs() < 3.0, "6 s: {}", db(600));
    assert!(db(1100) > -60.0, "11 s: {}", db(1100));
    assert!(db(1300) < db(1100) - 10.0, "13 s: {}", db(1300));
}

/// Fundamental of each of six voices, played one after another on rotary assignment.
fn cutoffs(instability: f64, serial: f64) -> Vec<f64> {
    let mut j = machine(&bank3(6.3));
    j.set_param(p::INSTABILITY, instability);
    j.set_param(p::UNIT, serial);
    j.activate(SR, BLOCK);
    (0..6)
        .map(|_| {
            run(&mut j, &[on(60)], 0.1);
            let (l, _) = run(&mut j, &[], 1.0);
            let hz = frequency(tail(&l, 0.5));
            run(&mut j, &[off(60)], 0.05);
            hz
        })
        .collect()
}

fn spread_cents(hz: &[f64]) -> f64 {
    let min = hz.iter().copied().fold(f64::MAX, f64::min);
    let max = hz.iter().copied().fold(f64::MIN, f64::max);
    cents(max, min)
}

#[test]
fn voices_only_diverge_when_the_unit_has_tolerances() {
    let exact = spread_cents(&cutoffs(0.0, 7.0));
    let nominal = spread_cents(&cutoffs(1.0, 7.0));
    let aged = spread_cents(&cutoffs(2.0, 7.0));
    assert!(exact < 2.0, "{exact}");
    assert!(nominal > 4.0, "{nominal}");
    assert!(aged > nominal, "{aged} {nominal}");
}

#[test]
fn a_unit_serial_is_reproducible_and_distinct() {
    let render = |serial: f64| {
        let mut j = Juno106::new();
        j.set_param(p::UNIT, serial);
        j.activate(SR, BLOCK);
        run(&mut j, &[on(60), on(64), on(67)], 1.0).0
    };
    assert_eq!(render(42.0), render(42.0));
    assert_ne!(render(42.0), render(43.0));
}

#[test]
fn a_cold_machine_is_detuned_until_it_warms_up() {
    let measure = |cold: f64| {
        let mut j = machine(&bank3(6.3));
        j.set_param(p::INSTABILITY, 1.0);
        j.set_param(p::COLD_START, cold);
        j.activate(SR, BLOCK);
        let (l, _) = run(&mut j, &[on(60)], 1.5);
        frequency(tail(&l, 0.5))
    };
    let shift = cents(measure(1.0), measure(0.0)).abs();
    assert!(shift > 2.0, "{shift}");
}

#[test]
fn stereo_appears_only_with_the_chorus_on() {
    let settings = open_filter(&[]);
    let (l, r) = run(&mut machine(&settings), &[on(60)], 0.5);
    assert_eq!(l, r);

    let mut j = machine(&settings);
    j.set_param(p::CHORUS, 1.0);
    let (l, r) = run(&mut j, &[on(60)], 2.0);
    let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
    assert!(rms(tail(&diff, 1.0)) > 0.1 * rms(tail(&l, 1.0)));
}

#[test]
fn the_chorus_hisses_even_when_nothing_is_playing() {
    let idle = |chorus: f64| {
        let mut j = Juno106::new();
        j.set_param(p::CHORUS, chorus);
        j.activate(SR, BLOCK);
        let (l, _) = run(&mut j, &[], 3.0);
        rms(tail(&l, 1.0))
    };
    let (off, on) = (idle(0.0), idle(1.0));
    assert!(on > 1.2 * off, "off {off} on {on}");
    assert!(on > 1e-5 && on < 1e-3, "{on}");
}

#[test]
fn resonance_thins_the_bass() {
    let level = |res: f64| {
        let mut j = machine(&open_filter(&[
            (p::VCF_RES, res),
            (p::VCF_FREQ, 6.3),
            (p::VCF_KYBD, 0.0),
        ]));
        let (l, _) = run(&mut j, &[on(36)], 1.5);
        rms(tail(&l, 1.0))
    };
    assert!(level(8.0) < 0.75 * level(0.0));
}

#[test]
fn six_saws_stay_out_of_digital_clipping_at_default_level() {
    let mut j = Juno106::new();
    j.activate(SR, BLOCK);
    let chord: Vec<MidiEvent> = [48u8, 55, 60, 64, 67, 72].iter().map(|k| on(*k)).collect();
    let (l, _) = run(&mut j, &chord, 2.0);
    let peak = l.iter().fold(0.0f32, |a, x| a.max(x.abs()));
    assert!(peak < 1.0, "{peak}");
    assert!(peak > 0.2, "{peak}");
}

#[test]
fn every_host_rate_produces_correct_pitch() {
    let sr_run = |sr: f64| {
        let mut j = machine(&open_filter(&[]));
        j.activate(sr, BLOCK);
        let transport = TransportInfo {
            playing: true,
            sample_rate: sr,
            ..Default::default()
        };
        let mut h = Harness::new(transport.sample_rate);
        h.transport = transport;
        let mut out = Vec::new();
        let ev = [on(69)];
        for b in 0..(sr / BLOCK as f64) as usize {
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            h.process(&mut j, &mut l, &mut r, if b == 0 { &ev } else { &[] }, &[]);
            out.extend_from_slice(&l);
        }
        let second_half = &out[out.len() / 2..];
        let n = second_half
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        n as f64 * sr / second_half.len() as f64
    };
    for sr in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
        let hz = sr_run(sr);
        assert!((hz - 442.0).abs() < 3.0, "{sr}: {hz}");
    }
}

#[test]
fn first_note_after_loading_has_no_startup_thump() {
    let mut j = machine(&open_filter(&[]));
    let (l, _) = run(&mut j, &[on(60)], 1.0);
    let early = rms(&l[480..960]);
    let later = rms(tail(&l, 0.2));
    assert!((early / later - 1.0).abs() < 0.15, "{early} {later}");
}

/// Power outside the harmonics of `f0` below 20 kHz, relative to total, in dB.
fn inharmonic_db(x: &[f32], f0: f64) -> f64 {
    let n = x.len();
    let bin_hz = SR / n as f64;
    let (mut total, mut outside) = (0.0, 0.0);
    for b in 1..(20_000.0 / bin_hz) as usize {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, s) in x.iter().enumerate() {
            // Blackman-Harris: a Hann window's sidelobes would read as false aliasing.
            let c = std::f64::consts::TAU * i as f64 / n as f64;
            let w =
                0.35875 - 0.48829 * c.cos() + 0.14128 * (2.0 * c).cos() - 0.01168 * (3.0 * c).cos();
            let a = std::f64::consts::TAU * (b * i) as f64 / n as f64;
            re += *s as f64 * w * a.cos();
            im -= *s as f64 * w * a.sin();
        }
        let p = re * re + im * im;
        total += p;
        let f = b as f64 * bin_hz;
        let h = (f / f0).round();
        if (f - h * f0).abs() > 6.0 * bin_hz {
            outside += p;
        }
    }
    10.0 * (outside / total).log10()
}

#[test]
fn the_saw_is_clean_of_audible_aliasing_at_the_top_of_the_keyboard() {
    let mut j = machine(&open_filter(&[(p::RANGE, 2.0)]));
    let (l, _) = run(&mut j, &[on(96)], 1.0);
    // C7 at 4' is 4.2 kHz: the hardest case on the panel.
    let db = inharmonic_db(tail(&l, 4096.0 / SR), 4.0e6 / (2.0e6f64 / 2102.5).round());
    assert!(db < -60.0, "{db}");
}
