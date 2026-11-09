//! Batch B11: Analog, Drift, Meld and Wavetable against the manual, pp. 704-823.

use rlive_core::{
    ExprKind, MidiEvent, MidiMessage, Processor, Resource, SampleBuffer, ScaleInfo, Tail,
};
use rlive_devices::devices::b11::analog::{self as an, Analog};
use rlive_devices::devices::b11::common::{major_scale, poly_blep};
use rlive_devices::devices::b11::drift::{self as dr, Drift};
use rlive_devices::devices::b11::meld::{self as me, Meld};
use rlive_devices::devices::b11::meld_osc as mo;
use rlive_devices::devices::b11::wavetable::{self as wt, WavetableSynth};
use rlive_devices::dsp::fft::{Complex32, Fft};
use rlive_devices::DeviceInit;
use rlive_testkit::{assert_no_alloc, peak, rms, tone_amplitude, Harness};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn note_on_v(offset: u32, key: u8, velocity: f32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity,
            note_id: 0,
            tuning: 0.0,
        },
    )
}

fn note_on(offset: u32, key: u8) -> MidiEvent {
    note_on_v(offset, key, 1.0)
}

fn note_off(offset: u32, key: u8) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity: 0.0,
            note_id: 0,
        },
    )
}

fn expr(offset: u32, key: u8, kind: ExprKind, value: f32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteExpr {
            channel: 0,
            key,
            note_id: 0,
            kind,
            value,
        },
    )
}

fn activate(p: &mut dyn Processor) {
    p.activate(SR, BLOCK);
}

/// Renders `frames` of output, `events` in the first block, under the allocation guard.
fn play(p: &mut dyn Processor, events: &[MidiEvent], frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    let blocks = frames.div_ceil(BLOCK);
    for b in 0..blocks {
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let ev: &[MidiEvent] = if b == 0 { events } else { &[] };
        assert_no_alloc(|| h.process(p, &mut l, &mut r, ev, &[]));
        out_l.extend_from_slice(&l);
        out_r.extend_from_slice(&r);
    }
    out_l.truncate(frames);
    out_r.truncate(frames);
    (out_l, out_r)
}

/// A continuation of a held note: more blocks, starting at `h`'s state.
fn more(p: &mut dyn Processor, h: &mut Harness, events: &[MidiEvent], frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    for b in 0..frames.div_ceil(BLOCK) {
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let ev: &[MidiEvent] = if b == 0 { events } else { &[] };
        assert_no_alloc(|| h.process(p, &mut l, &mut r, ev, &[]));
        out.extend_from_slice(&l);
    }
    out
}

/// The frequency of a near-sine, by interpolated upward zero crossings.
fn zero_cross_hz(x: &[f32]) -> f64 {
    let (mut first, mut last, mut n) = (None, 0.0, 0usize);
    for i in 1..x.len() {
        if x[i - 1] <= 0.0 && x[i] > 0.0 {
            let t = i as f64 - 1.0 + (-x[i - 1] / (x[i] - x[i - 1])) as f64;
            first.get_or_insert(t);
            last = t;
            n += 1;
        }
    }
    (n as f64 - 1.0) * SR / (last - first.unwrap_or(0.0))
}

fn cents(hz: f64, expected: f64) -> f64 {
    1200.0 * (hz / expected).log2()
}

fn hz_of(key: f32) -> f64 {
    440.0 * 2f64.powf((key as f64 - 69.0) / 12.0)
}

/// The magnitude spectrum of the last `N` samples, windowed with a Blackman-Harris.
fn spectrum(x: &[f32]) -> Vec<f32> {
    const N: usize = 1 << 15;
    let slice = &x[x.len() - N..];
    let mut windowed: Vec<f32> = slice
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let t = std::f64::consts::TAU * i as f64 / N as f64;
            let w =
                0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos();
            s * w as f32
        })
        .collect();
    let mut fft = Fft::new(N);
    let mut bins = vec![Complex32::new(0.0, 0.0); fft.bins()];
    fft.forward(&mut windowed, &mut bins);
    bins.iter().map(|c| c.norm()).collect()
}

/// The strongest bin under `max_hz` that isn't near a harmonic of `f0`, in dB below the strongest
/// harmonic.
fn worst_inharmonic_db(spec: &[f32], f0: f64, max_hz: f64) -> f32 {
    let n = (spec.len() - 1) * 2;
    let bin_hz = SR / n as f64;
    let peak = spec.iter().cloned().fold(0.0, f32::max);
    let mut worst = 0.0f32;
    for (i, m) in spec.iter().enumerate().skip(8) {
        let hz = i as f64 * bin_hz;
        if hz > max_hz {
            break;
        }
        let harmonic = hz / f0;
        let distance = (harmonic - harmonic.round()).abs() * f0 / bin_hz;
        if distance > 10.0 {
            worst = worst.max(*m);
        }
    }
    20.0 * (worst.max(1e-12) / peak).log10()
}

fn set_all(p: &mut dyn Processor, list: &[(u32, f64)]) {
    for (id, value) in list {
        p.set_param(*id, *value);
    }
}

// --------------------------------------------------------------------- BLEP kernels

#[test]
fn the_blep_residual_integrates_to_nothing() {
    let mut sum = 0.0;
    for i in -2000..2000 {
        let d = (i as f32 + 0.5) / 1000.0;
        sum += poly_blep(d) * 0.001;
    }
    assert!(sum.abs() < 1e-3, "{sum}");
}

// ---------------------------------------------------------------------------- pitch

fn sine_analog() -> Analog {
    let mut a = Analog::new();
    set_all(
        &mut a,
        &[
            (an::OSC1 + an::O_SHAPE, 0.0),
            (an::FILTER1 + an::F_ON, 0.0),
            (an::AMP1 + an::A_ON, 1.0),
        ],
    );
    activate(&mut a);
    a
}

fn sine_drift() -> Drift {
    let mut d = Drift::new();
    set_all(
        &mut d,
        &[
            (dr::OSC1_WAVE, 0.0),
            (dr::OSC1_FILTER, 0.0),
            (dr::VEL_VOL, 0.0),
        ],
    );
    activate(&mut d);
    d
}

fn sine_meld() -> Meld {
    let mut m = Meld::new();
    set_all(
        &mut m,
        &[
            (me::ENGINE_A + me::E_TYPE, 0.0),
            (me::ENGINE_A + me::E_M1, 0.0),
            (me::ENGINE_A + me::E_M2, 0.0),
            (me::ENGINE_A + me::F_ON, 0.0),
        ],
    );
    activate(&mut m);
    m
}

fn sine_wavetable() -> WavetableSynth {
    let mut w = WavetableSynth::new(&DeviceInit::default());
    set_all(&mut w, &[(wt::FILTER1 + wt::F_ON, 0.0)]);
    activate(&mut w);
    w
}

#[test]
fn every_synth_plays_in_tune_across_the_range() {
    let check = |name: &str, make: &dyn Fn() -> Box<dyn Processor>| {
        for key in [24u8, 36, 48, 60, 72, 84, 96] {
            let mut p = make();
            let (l, _) = play(&mut *p, &[note_on(0, key)], 48_000);
            let hz = zero_cross_hz(&l[4_800..]);
            let err = cents(hz, hz_of(key as f32));
            assert!(
                err.abs() < 1.5,
                "{name} key {key}: {err:.2} cents ({hz:.3} Hz)"
            );
        }
    };
    check("Analog", &|| Box::new(sine_analog()));
    check("Drift", &|| Box::new(sine_drift()));
    check("Meld", &|| Box::new(sine_meld()));
    check("Wavetable", &|| Box::new(sine_wavetable()));
}

#[test]
fn analog_key_scaling_pivots_on_c3() {
    let mut a = sine_analog();
    a.set_param(an::OSC1 + an::O_KEY, 50.0);
    let (l, _) = play(&mut a, &[note_on(0, 60)], 24_000);
    assert!(
        cents(zero_cross_hz(&l[2_400..]), hz_of(60.0)).abs() < 2.0,
        "C3 is the pivot"
    );

    let mut a = sine_analog();
    a.set_param(an::OSC1 + an::O_KEY, 50.0);
    let (l, _) = play(&mut a, &[note_on(0, 72)], 24_000);
    // Half the key scaling: an octave up on the keyboard is half an octave of pitch.
    let want = hz_of(66.0);
    assert!(cents(zero_cross_hz(&l[2_400..]), want).abs() < 2.0);
}

#[test]
fn analog_octave_semi_detune_and_global_transpose_add_up() {
    let mut a = sine_analog();
    set_all(
        &mut a,
        &[
            (an::OSC1 + an::O_OCT, 1.0),
            (an::OSC1 + an::O_SEMI, 2.0),
            (an::OSC1 + an::O_DETUNE, 100.0),
            (an::SEMI, -1.0),
        ],
    );
    let (l, _) = play(&mut a, &[note_on(0, 60)], 24_000);
    assert!(
        cents(
            zero_cross_hz(&l[2_400..]),
            hz_of(60.0 + 12.0 + 2.0 + 1.0 - 1.0)
        )
        .abs()
            < 2.0
    );
}

#[test]
fn analog_stretch_raises_high_notes_and_lowers_low_ones() {
    let high = {
        let mut a = sine_analog();
        a.set_param(an::STRETCH, 100.0);
        let (l, _) = play(&mut a, &[note_on(0, 96)], 24_000);
        zero_cross_hz(&l[2_400..])
    };
    let low = {
        let mut a = sine_analog();
        a.set_param(an::STRETCH, 100.0);
        let (l, _) = play(&mut a, &[note_on(0, 36)], 24_000);
        zero_cross_hz(&l[2_400..])
    };
    assert!(cents(high, hz_of(96.0)) > 5.0, "high notes sharp");
    assert!(cents(low, hz_of(36.0)) < -5.0, "low notes flat");
}

// ------------------------------------------------------------------------- aliasing

#[test]
fn oscillators_are_band_limited_at_high_notes() {
    // Key 108 is C8, 4186 Hz. Floors below 16 kHz, then across the whole band: the strongest
    // alias of a 4-point polynomial BLEP lands near Nyquist, around 23 kHz at this pitch.
    const KEY: u8 = 108;
    let f0 = hz_of(KEY as f32);
    let floor = |name: &str, x: &[f32], audible: f32, whole: f32| {
        let spec = spectrum(x);
        let (a, b) = (
            worst_inharmonic_db(&spec, f0, 16_000.0),
            worst_inharmonic_db(&spec, f0, 24_000.0),
        );
        println!("{name}: {a:.1} dB below 16 kHz, {b:.1} dB overall");
        assert!(
            a < audible,
            "{name}: {a:.1} dB below 16 kHz, wanted under {audible}"
        );
        assert!(b < whole, "{name}: {b:.1} dB overall, wanted under {whole}");
    };

    let mut a = Analog::new();
    set_all(
        &mut a,
        &[(an::OSC1 + an::O_SHAPE, 1.0), (an::FILTER1 + an::F_ON, 0.0)],
    );
    activate(&mut a);
    let (l, _) = play(&mut a, &[note_on(0, KEY)], 48_000);
    floor("Analog saw", &l, -50.0, -30.0);

    let mut a = Analog::new();
    set_all(
        &mut a,
        &[
            (an::OSC1 + an::O_SHAPE, 2.0),
            (an::OSC1 + an::O_WIDTH, 40.0),
            (an::FILTER1 + an::F_ON, 0.0),
        ],
    );
    activate(&mut a);
    let (l, _) = play(&mut a, &[note_on(0, KEY)], 48_000);
    floor("Analog rect", &l, -45.0, -30.0);

    let mut d = Drift::new();
    set_all(
        &mut d,
        &[
            (dr::OSC1_WAVE, 4.0),
            (dr::OSC1_FILTER, 0.0),
            (dr::VEL_VOL, 0.0),
        ],
    );
    activate(&mut d);
    let (l, _) = play(&mut d, &[note_on(0, KEY)], 48_000);
    floor("Drift saw", &l, -50.0, -30.0);

    let mut m = Meld::new();
    set_all(
        &mut m,
        &[
            (me::ENGINE_A + me::E_M1, 67.0),
            (me::ENGINE_A + me::F_ON, 0.0),
        ],
    );
    activate(&mut m);
    let (l, _) = play(&mut m, &[note_on(0, KEY)], 48_000);
    floor("Meld saw", &l, -45.0, -30.0);

    let mut w = WavetableSynth::new(&DeviceInit::default());
    set_all(
        &mut w,
        &[
            (wt::OSC1 + wt::O_POSITION, 0.7),
            (wt::FILTER1 + wt::F_ON, 0.0),
        ],
    );
    activate(&mut w);
    let (l, _) = play(&mut w, &[note_on(0, KEY)], 48_000);
    floor("Wavetable", &l, -50.0, -45.0);
}

#[test]
fn drift_saturated_wave_stays_below_nyquist_when_high() {
    let mut d = Drift::new();
    set_all(
        &mut d,
        &[
            (dr::OSC1_WAVE, 3.0),
            (dr::OSC1_SHAPE, 100.0),
            (dr::OSC1_FILTER, 0.0),
        ],
    );
    activate(&mut d);
    let (l, _) = play(&mut d, &[note_on(0, 108)], 48_000);
    let db = worst_inharmonic_db(&spectrum(&l), hz_of(108.0), 24_000.0);
    assert!(db < -70.0, "{db:.1} dB");
}

// ------------------------------------------------------------------ filter tracking

fn harmonic_ratio_db(x: &[f32], f0: f64, h: f64) -> f64 {
    let a = tone_amplitude(x, f0, SR);
    let b = tone_amplitude(x, f0 * h, SR);
    20.0 * (b / a).log10()
}

#[test]
fn analog_filter_key_tracking_is_one_to_one_at_100_percent() {
    let render = |key: u8, keytrack: f64| {
        let mut a = Analog::new();
        set_all(
            &mut a,
            &[
                (an::OSC1 + an::O_SHAPE, 1.0),
                (an::FILTER1 + an::F_TYPE, 0.0),
                (an::FILTER1 + an::F_FREQ, 800.0),
                (an::FILTER1 + an::F_FREQ_KEY, keytrack),
            ],
        );
        activate(&mut a);
        play(&mut a, &[note_on(0, key)], 24_000).0
    };
    let f = |k: u8| hz_of(k as f32);
    let tracked = (
        harmonic_ratio_db(&render(48, 100.0), f(48), 3.0),
        harmonic_ratio_db(&render(72, 100.0), f(72), 3.0),
    );
    assert!((tracked.0 - tracked.1).abs() < 1.5, "{tracked:?}");
    let fixed = (
        harmonic_ratio_db(&render(48, 0.0), f(48), 3.0),
        harmonic_ratio_db(&render(72, 0.0), f(72), 3.0),
    );
    assert!(fixed.1 < fixed.0 - 6.0, "{fixed:?}");
}

#[test]
fn drift_key_tracking_opens_the_filter_for_high_notes() {
    let render = |key: u8, track: f64| {
        let mut d = Drift::new();
        set_all(
            &mut d,
            &[
                (dr::OSC1_WAVE, 4.0),
                (dr::LP_FREQ, 500.0),
                (dr::LP_KEY, track),
                (dr::VEL_VOL, 0.0),
            ],
        );
        activate(&mut d);
        play(&mut d, &[note_on(0, key)], 24_000).0
    };
    let tracked = harmonic_ratio_db(&render(84, 1.0), hz_of(84.0), 3.0);
    let fixed = harmonic_ratio_db(&render(84, 0.0), hz_of(84.0), 3.0);
    assert!(tracked > fixed + 6.0, "{tracked} vs {fixed}");
}

#[test]
fn wavetable_note_source_tracks_the_filter_exactly_at_100_percent() {
    let render = |key: u8| {
        let mut w = WavetableSynth::new(&DeviceInit::default());
        set_all(
            &mut w,
            &[
                (wt::OSC1 + wt::O_POSITION, 0.67),
                (wt::FILTER1 + wt::F_FREQ, 800.0),
                (wt::cell(6, 14), 100.0),
            ],
        );
        activate(&mut w);
        play(&mut w, &[note_on(0, key)], 24_000).0
    };
    let a = harmonic_ratio_db(&render(48), hz_of(48.0), 3.0);
    let b = harmonic_ratio_db(&render(72), hz_of(72.0), 3.0);
    assert!((a - b).abs() < 1.5, "{a} vs {b}");
}

#[test]
fn analog_filter_2_follows_filter_1() {
    let render = |follow: f64| {
        let mut a = Analog::new();
        set_all(
            &mut a,
            &[
                (an::OSC1 + an::O_SHAPE, 1.0),
                (an::OSC1 + an::O_BAL, 1.0),
                (an::FILTER1 + an::F_ON, 1.0),
                (an::FILTER1 + an::F_FREQ, 300.0),
                (an::FILTER2 + an::F_ON, 1.0),
                (an::FILTER2 + an::F_TYPE, 0.0),
                (an::FILTER2 + an::F_FREQ, 632.46),
                (an::FILTER2 + an::F_ROUTE, follow),
                (an::AMP1 + an::A_ON, 0.0),
                (an::AMP2 + an::A_ON, 1.0),
            ],
        );
        activate(&mut a);
        play(&mut a, &[note_on(0, 60)], 24_000).0
    };
    // At its centre, with Follow, Filter 2 sits on Filter 1's cutoff, well below 632 Hz.
    let followed = rms(&render(1.0)[4_800..]);
    let plain = rms(&render(0.0)[4_800..]);
    assert!(followed < plain * 0.8, "{followed} vs {plain}");
}

// ------------------------------------------------------------------------ envelopes

/// Peak level per `win` samples.
fn envelope_of(x: &[f32], win: usize) -> Vec<f32> {
    x.chunks(win).map(peak).collect()
}

#[test]
fn analog_attack_decay_sustain_timing() {
    let mut a = sine_analog();
    let e = an::AMP1 + an::A_ENV;
    set_all(
        &mut a,
        &[
            (e, 200.0),
            (e + 2, 300.0),
            (e + 3, 0.5),
            (e + 7, 0.0),
            (e + 8, 0.0),
            (e + 9, 0.0),
        ],
    );
    let (l, _) = play(&mut a, &[note_on(0, 69)], 48_000);
    let env = envelope_of(&l, 480); // 10 ms
    let peak = env.iter().cloned().fold(0.0, f32::max);
    // Half way through the attack, 100 ms: half of the peak.
    assert!((env[10] / peak - 0.5).abs() < 0.1, "{}", env[10] / peak);
    // Sustain is reached by 500 ms, at half the peak.
    assert!((env[60] / peak - 0.5).abs() < 0.06, "{}", env[60] / peak);
}

#[test]
fn analog_s_time_makes_sustain_fall_away_while_held() {
    let mut a = sine_analog();
    let e = an::AMP1 + an::A_ENV;
    set_all(&mut a, &[(e + 2, 10.0), (e + 3, 1.0), (e + 4, 200.0)]);
    let (l, _) = play(&mut a, &[note_on(0, 69)], 24_000);
    let env = envelope_of(&l, 480);
    assert!(env[3] > 0.1, "{}", env[3]);
    assert!(env[40] < env[3] * 0.1, "{} {}", env[40], env[3]);
}

#[test]
fn analog_free_mode_ignores_note_off_until_the_end() {
    let mut a = sine_analog();
    let e = an::AMP1 + an::A_ENV;
    set_all(
        &mut a,
        &[(e + 2, 200.0), (e + 3, 0.5), (e + 5, 50.0), (e + 11, 1.0)],
    );
    let mut h = Harness::new(SR);
    let on = [note_on(0, 69), note_off(100, 69)];
    let all = more(&mut a, &mut h, &on, 24_000);
    let env = envelope_of(&all, 480);
    assert!(env[12] > 0.05, "{}", env[12]);
    assert_eq!(
        a.active_voices(),
        0,
        "it ends once decay and release are done"
    );
}

#[test]
fn analog_adsr_ads_r_replays_attack_on_release() {
    let mut a = sine_analog();
    let e = an::AMP1 + an::A_ENV;
    set_all(
        &mut a,
        &[
            (e, 100.0),
            (e + 2, 50.0),
            (e + 3, 0.3),
            (e + 5, 100.0),
            (e + 12, 3.0),
        ],
    );
    let mut h = Harness::new(SR);
    let held = more(&mut a, &mut h, &[note_on(0, 69)], 12_000);
    let level_held = peak(&held[9_600..]);
    let released = more(&mut a, &mut h, &[note_off(0, 69)], 9_600);
    let lvl = peak(&released[..4_800]);
    assert!(
        lvl > level_held * 1.8,
        "attack again: {lvl} vs {level_held}"
    );
    assert_eq!(a.active_voices(), 0);
}

#[test]
fn analog_ad_r_loops_while_held_and_releases() {
    let mut a = sine_analog();
    let e = an::AMP1 + an::A_ENV;
    set_all(
        &mut a,
        &[
            (e, 40.0),
            (e + 2, 40.0),
            (e + 3, 0.2),
            (e + 7, 0.0),
            (e + 8, 0.0),
            (e + 12, 1.0),
        ],
    );
    let mut h = Harness::new(SR);
    let l = more(&mut a, &mut h, &[note_on(0, 69)], 24_000);
    let env = envelope_of(&l, 240);
    let rises = env.windows(2).filter(|w| w[1] > w[0] + 0.01).count();
    assert!(rises > 8, "keeps retriggering: {rises}");
    let released = more(&mut a, &mut h, &[note_off(0, 69)], 12_000);
    assert!(peak(&released[9_600..]) < 0.01);
    assert_eq!(a.active_voices(), 0);
}

#[test]
fn analog_free_and_loop_keeps_playing_as_if_held_and_says_its_tail_is_endless() {
    let mut a = sine_analog();
    let e = an::AMP1 + an::A_ENV;
    set_all(&mut a, &[(e + 11, 1.0), (e + 12, 1.0)]);
    assert_eq!(a.tail(), Tail::Infinite);
    let mut h = Harness::new(SR);
    more(&mut a, &mut h, &[note_on(0, 69), note_off(100, 69)], 12_000);
    let later = more(&mut a, &mut h, &[], 4_800);
    assert!(peak(&later) > 0.05, "still looping");
    a.reset();
    assert_eq!(a.active_voices(), 0, "and reset ends it");
}

#[test]
fn drift_envelope_shape_and_release() {
    let mut d = sine_drift();
    set_all(
        &mut d,
        &[
            (dr::E1_ATTACK, 100.0),
            (dr::E1_DECAY, 1.0),
            (dr::E1_SUSTAIN, 1.0),
            (dr::E1_RELEASE, 200.0),
        ],
    );
    let mut h = Harness::new(SR);
    let on = more(&mut d, &mut h, &[note_on(0, 69)], 24_000);
    let env = envelope_of(&on, 480);
    let top = env[40];
    assert!((env[5] / top - 0.5).abs() < 0.2, "{}", env[5] / top);
    let off = more(&mut d, &mut h, &[note_off(0, 69)], 24_000);
    assert!(peak(&off[14_400..]) < 0.01);
    assert_eq!(d.active_voices(), 0);
}

#[test]
fn meld_loop_modes_behave_per_the_manual() {
    let render = |loop_mode: f64| {
        let mut m = sine_meld();
        let e = me::ENGINE_A;
        set_all(
            &mut m,
            &[
                (e + me::AE_ATTACK, 20.0),
                (e + me::AE_DECAY, 20.0),
                (e + me::AE_SUSTAIN, 0.2),
                (e + me::AE_RELEASE, 20.0),
                (e + me::AE_LOOP, loop_mode),
            ],
        );
        let mut h = Harness::new(SR);
        let l = more(&mut m, &mut h, &[note_on(0, 69)], 24_000);
        (envelope_of(&l, 240), m)
    };
    // Trigger plays through once, ignoring the held key's sustain.
    let (env, m) = render(1.0);
    assert!(env[80] < 0.02, "{}", env[80]);
    assert_eq!(m.active_voices(), 0);
    // Loop keeps coming back.
    let (env, m) = render(2.0);
    let rises = env.windows(2).filter(|w| w[1] > w[0] + 0.01).count();
    assert!(rises > 6, "{rises}");
    assert_eq!(m.active_voices(), 1);
    // AD Loop too, never falling to zero through a release.
    let (env, m) = render(3.0);
    assert!(env.iter().skip(20).all(|e| *e > 0.02), "stays up");
    assert_eq!(m.active_voices(), 1);
}

#[test]
fn wavetable_trigger_plays_through_and_loop_repeats() {
    let render = |mode: f64| {
        let mut w = sine_wavetable();
        set_all(
            &mut w,
            &[
                (wt::AMP_ENV + wt::E_ATTACK, 20.0),
                (wt::AMP_ENV + wt::E_DECAY, 20.0),
                (wt::AMP_ENV + wt::E_SUSTAIN, 0.2),
                (wt::AMP_ENV + wt::E_RELEASE, 20.0),
                (wt::AMP_ENV + wt::E_LOOP, mode),
            ],
        );
        let mut h = Harness::new(SR);
        let l = more(&mut w, &mut h, &[note_on(0, 69)], 24_000);
        (envelope_of(&l, 240), w)
    };
    let (env, w) = render(1.0);
    assert!(env[80] < 0.02);
    assert_eq!(w.active_voices(), 0);
    let (env, w) = render(2.0);
    assert!(env.windows(2).filter(|p| p[1] > p[0] + 0.01).count() > 6);
    assert_eq!(w.active_voices(), 1);
}

// --------------------------------------------------------------- voices and unison

#[test]
fn analog_unison_stacks_two_or_four_voices_and_detunes_them() {
    for (index, count) in [(0.0, 2), (1.0, 4)] {
        let mut a = sine_analog();
        set_all(
            &mut a,
            &[
                (an::UNI_ON, 1.0),
                (an::UNI_VOICES, index),
                (an::UNI_DETUNE, 100.0),
            ],
        );
        let (l, _) = play(&mut a, &[note_on(0, 69)], 24_000);
        assert_eq!(a.active_voices(), count);
        // Two detuned sines beat: the level is nothing like constant.
        let env = envelope_of(&l, 480);
        let (hi, lo) = env
            .iter()
            .skip(4)
            .fold((0.0f32, 10.0f32), |(h, l), e| (h.max(*e), l.min(*e)));
        assert!(hi > lo * 1.5, "{hi} vs {lo}");
    }
}

#[test]
fn analog_unison_delay_starts_the_stack_in_turn() {
    let mut a = sine_analog();
    set_all(
        &mut a,
        &[
            (an::OSC1 + an::O_SHAPE, 3.0),
            (an::UNI_ON, 1.0),
            (an::UNI_VOICES, 0.0),
            (an::UNI_DETUNE, 0.0),
            (an::UNI_DELAY, 200.0),
        ],
    );
    let (l, _) = play(&mut a, &[note_on(0, 69)], 24_000);
    let before = rms(&l[4_800..9_000]);
    let after = rms(&l[13_000..]);
    // Two independent noise voices add up to about the root of two.
    assert!(after > before * 1.25, "{after} vs {before}");
}

#[test]
fn analog_priority_high_steals_the_lowest() {
    let mut a = sine_analog();
    set_all(&mut a, &[(an::VOICES, 1.0), (an::PRIORITY, 0.0)]);
    let (l, _) = play(
        &mut a,
        &[note_on(0, 72), note_on(1, 76), note_on(2, 79)],
        24_000,
    );
    let at = |key: f32| tone_amplitude(&l[4_800..], hz_of(key), SR);
    assert!(at(79.0) > 0.05 && at(76.0) > 0.05, "the top two stay");
    assert!(at(72.0) < 0.01, "the lowest was stolen: {}", at(72.0));
}

#[test]
fn analog_priority_low_steals_the_highest() {
    let mut a = sine_analog();
    set_all(&mut a, &[(an::VOICES, 1.0), (an::PRIORITY, 1.0)]);
    let (l, _) = play(
        &mut a,
        &[note_on(0, 72), note_on(1, 76), note_on(2, 79)],
        24_000,
    );
    let at = |key: f32| tone_amplitude(&l[4_800..], hz_of(key), SR);
    assert!(
        at(72.0) > 0.05 && at(79.0) > 0.05,
        "the newest and lowest stay"
    );
    assert!(at(76.0) < 0.01, "the middle one was stolen: {}", at(76.0));
}

#[test]
fn analog_glide_constant_and_proportional() {
    let mid_hz = |mode: f64, target: u8| {
        let mut a = sine_analog();
        set_all(
            &mut a,
            &[
                (an::VOICES, 0.0),
                (an::GLIDE_ON, 1.0),
                (an::GLIDE_TIME, 200.0),
                (an::GLIDE_MODE, mode),
            ],
        );
        let mut h = Harness::new(SR);
        more(&mut a, &mut h, &[note_on(0, 60)], 4_800);
        let l = more(&mut a, &mut h, &[note_on(0, target)], 4_800);
        zero_cross_hz(&l[4_320..4_800]) // about 95 ms in
    };
    let (start, end) = (hz_of(60.0), hz_of(72.0));
    let hz = mid_hz(0.0, 72);
    assert!(hz > start * 1.05 && hz < end * 0.95, "mid-glide: {hz}");
    // Proportional: an octave takes the set time, so 48 st takes four times as long; at
    // 200 ms per octave, it's still early in the slide at 100 ms.
    let far = mid_hz(1.0, 84);
    assert!(far < hz_of(72.0), "still early: {far}");
}

#[test]
fn drift_voice_modes_use_one_two_or_four_voices() {
    for (mode, count) in [(0.0, 1), (1.0, 4), (2.0, 2), (3.0, 4)] {
        let mut d = sine_drift();
        d.set_param(dr::VOICE_MODE, mode);
        play(&mut d, &[note_on(0, 60)], 4_800);
        assert_eq!(d.active_voices(), count, "mode {mode}");
    }
}

#[test]
fn drift_stereo_mode_pans_its_two_voices_apart() {
    let mut d = sine_drift();
    set_all(&mut d, &[(dr::VOICE_MODE, 2.0), (dr::SPREAD, 100.0)]);
    let (l, r) = play(&mut d, &[note_on(0, 60)], 12_000);
    let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
    assert!(diff > 1.0, "{diff}");
}

#[test]
fn drift_mono_thickness_zero_uses_only_one_voice_of_the_four() {
    let level = |thickness: f64| {
        let mut d = sine_drift();
        set_all(&mut d, &[(dr::VOICE_MODE, 1.0), (dr::THICKNESS, thickness)]);
        let (l, _) = play(&mut d, &[note_on(0, 69)], 12_000);
        rms(&l[4_800..])
    };
    assert!(level(100.0) > level(0.0) * 1.5);
}

#[test]
fn drift_polyphony_follows_the_voices_setting() {
    let mut d = sine_drift();
    d.set_param(dr::VOICES, 0.0);
    let notes: Vec<MidiEvent> = (0..8).map(|i| note_on(i, 60 + i as u8)).collect();
    play(&mut d, &notes, 4_800);
    assert_eq!(d.active_voices(), 4, "the Voices menu caps it");
}

#[test]
fn wavetable_poly_voices_cap_and_mono_is_legato() {
    let mut w = sine_wavetable();
    w.set_param(wt::VOICES, 0.0);
    let notes: Vec<MidiEvent> = (0..6).map(|i| note_on(i, 60 + i as u8)).collect();
    play(&mut w, &notes, 4_800);
    assert_eq!(w.active_voices(), 2);

    let mut w = sine_wavetable();
    w.set_param(wt::MONO, 1.0);
    let (l, _) = play(&mut w, &[note_on(0, 60), note_on(4_800, 72)], 24_000);
    assert_eq!(w.active_voices(), 1);
    assert!(
        cents(zero_cross_hz(&l[9_600..]), hz_of(72.0)).abs() < 2.0,
        "legato to the new note"
    );
}

#[test]
fn wavetable_mono_glide_slides() {
    let mut w = sine_wavetable();
    set_all(&mut w, &[(wt::MONO, 1.0), (wt::GLIDE, 300.0)]);
    let mut h = Harness::new(SR);
    more(&mut w, &mut h, &[note_on(0, 60)], 4_800);
    let l = more(&mut w, &mut h, &[note_on(0, 72)], 4_800);
    let hz = zero_cross_hz(&l[3_360..4_800]);
    assert!(hz > hz_of(60.0) * 1.05 && hz < hz_of(72.0) * 0.98, "{hz}");
}

#[test]
fn wavetable_classic_unison_detunes_and_alternates_pan() {
    let mut w = sine_wavetable();
    set_all(
        &mut w,
        &[
            (wt::UNISON, 1.0),
            (wt::UNISON_VOICES, 2.0),
            (wt::UNISON_AMOUNT, 1.0),
        ],
    );
    let (l, r) = play(&mut w, &[note_on(0, 69)], 48_000);
    assert_ne!(rms(&l[9_600..]).to_bits(), rms(&r[9_600..]).to_bits());
    let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
    assert!(diff > 10.0, "{diff}");
}

#[test]
fn wavetable_phase_sync_starts_voices_together_where_classic_does_not() {
    let onset_level = |mode: f64| {
        let mut w = sine_wavetable();
        set_all(
            &mut w,
            &[
                (wt::UNISON, mode),
                (wt::UNISON_VOICES, 6.0),
                (wt::UNISON_AMOUNT, 0.1),
            ],
        );
        let (l, _) = play(&mut w, &[note_on(0, 69)], 4_800);
        rms(&l[480..2_400])
    };
    // In phase the voices add up coherently.
    assert!(onset_level(4.0) > onset_level(1.0));
}

#[test]
fn wavetable_position_spread_mode_runs() {
    for mode in 1..=6 {
        let mut w = sine_wavetable();
        set_all(
            &mut w,
            &[(wt::UNISON, mode as f64), (wt::UNISON_VOICES, 6.0)],
        );
        let (l, _) = play(&mut w, &[note_on(0, 69)], 12_000);
        assert!(rms(&l[4_800..]) > 0.01, "mode {mode}");
        assert!(l.iter().all(|s| s.is_finite()));
    }
}

#[test]
fn meld_stacked_voices_duplicate_each_note() {
    let mut m = sine_meld();
    m.set_param(me::STACKED, 3.0); // 4 voices
    play(&mut m, &[note_on(0, 60)], 4_800);
    assert_eq!(m.active_voices(), 4);
}

#[test]
fn meld_voice_count_two_to_twelve() {
    let mut m = sine_meld();
    m.set_param(me::VOICES, 10.0); // 12
    let notes: Vec<MidiEvent> = (0..16).map(|i| note_on(i, 50 + i as u8)).collect();
    play(&mut m, &notes, 4_800);
    assert_eq!(m.active_voices(), 12);
}

// ---------------------------------------------------------------------- expression

#[test]
fn analog_note_pb_and_pressure_reach_their_destinations() {
    let mut a = sine_analog();
    set_all(
        &mut a,
        &[
            (an::PRESS_DEST_A, 9.0),
            (an::PRESS_AMT_A, 100.0),
            (an::NOTE_PB, 12.0),
        ],
    );
    let mut h = Harness::new(SR);
    more(&mut a, &mut h, &[note_on(0, 69)], 9_600);
    let quiet = more(&mut a, &mut h, &[], 4_800);
    // 48 st of bend, scaled by a Note PB range of 12 st, is one octave up.
    let bent = more(&mut a, &mut h, &[expr(0, 69, ExprKind::Pitch, 48.0)], 9_600);
    assert!(cents(zero_cross_hz(&bent[4_800..]), hz_of(81.0)).abs() < 3.0);
    let pressed = more(
        &mut a,
        &mut h,
        &[expr(0, 69, ExprKind::Pressure, 1.0)],
        4_800,
    );
    assert!(rms(&pressed[2_400..]) > rms(&quiet) * 1.5);
}

#[test]
fn drift_per_note_pitch_obeys_the_note_pb_switch() {
    let pitch = |on: f64| {
        let mut d = sine_drift();
        d.set_param(dr::NOTE_PB, on);
        let mut h = Harness::new(SR);
        more(&mut d, &mut h, &[note_on(0, 69)], 4_800);
        let l = more(&mut d, &mut h, &[expr(0, 69, ExprKind::Pitch, 12.0)], 9_600);
        zero_cross_hz(&l[4_800..])
    };
    assert!(cents(pitch(1.0), hz_of(81.0)).abs() < 3.0, "per-note bend");
    assert!(
        cents(pitch(0.0), hz_of(69.0)).abs() < 3.0,
        "ignored when off"
    );
}

#[test]
fn drift_slide_reaches_a_modulation_destination() {
    let mut d = sine_drift();
    set_all(
        &mut d,
        &[
            (dr::MOD_BASE, 7.0),
            (dr::MOD_BASE + 1, 10.0),
            (dr::MOD_BASE + 2, 100.0),
        ],
    );
    let mut h = Harness::new(SR);
    more(&mut d, &mut h, &[note_on(0, 69)], 4_800);
    let flat = more(&mut d, &mut h, &[], 4_800);
    let slid = more(&mut d, &mut h, &[expr(0, 69, ExprKind::Slide, 1.0)], 9_600);
    assert!(rms(&slid[4_800..]) > rms(&flat) * 3.0);
}

#[test]
fn meld_note_bend_and_mpe_press_route_through_the_matrix() {
    let mut m = sine_meld();
    m.set_param(me::cell(0, 14, 0), 100.0); // MPE Press > Volume, multiplicative.
    let mut h = Harness::new(SR);
    let before = more(&mut m, &mut h, &[note_on(0, 69)], 4_800);
    assert!(rms(&before[2_400..]) < 0.01, "silent with no pressure");
    let pressed = more(
        &mut m,
        &mut h,
        &[expr(0, 69, ExprKind::Pressure, 1.0)],
        4_800,
    );
    assert!(rms(&pressed[2_400..]) > 0.05);
    let bent = more(&mut m, &mut h, &[expr(0, 69, ExprKind::Pitch, 12.0)], 9_600);
    assert!(cents(zero_cross_hz(&bent[4_800..]), hz_of(81.0)).abs() < 3.0);
}

#[test]
fn wavetable_aftertouch_and_per_note_pitch_land_in_the_matrix() {
    let mut w = sine_wavetable();
    w.set_param(wt::cell(8, 0), 25.0); // Aftertouch > Osc 1 Pitch: +12 st at full.
    let mut h = Harness::new(SR);
    more(&mut w, &mut h, &[note_on(0, 69)], 4_800);
    let up = more(
        &mut w,
        &mut h,
        &[expr(0, 69, ExprKind::Pressure, 1.0)],
        9_600,
    );
    assert!(cents(zero_cross_hz(&up[4_800..]), hz_of(81.0)).abs() < 3.0);
}

#[test]
fn expression_reaches_only_the_note_it_names() {
    let mut m = sine_meld();
    m.set_param(me::VOICES, 4.0);
    let mut h = Harness::new(SR);
    more(&mut m, &mut h, &[note_on(0, 69), note_on(0, 76)], 4_800);
    let l = more(&mut m, &mut h, &[expr(0, 69, ExprKind::Pitch, 12.0)], 9_600);
    let at = |key: f32| tone_amplitude(&l[4_800..], hz_of(key), SR);
    assert!(at(81.0) > 0.01, "the bent note moved");
    assert!(at(76.0) > 0.01, "the other didn't");
    assert!(at(69.0) < at(81.0) * 0.1, "and nothing is left behind");
}

// ------------------------------------------------------------------ modulation slots

#[test]
fn drift_mod_slots_reach_each_destination() {
    let render = |dest: f64, amount: f64| {
        let mut d = Drift::new();
        set_all(
            &mut d,
            &[
                (dr::OSC1_WAVE, 4.0),
                (dr::OSC1_SHAPE, 30.0),
                (dr::OSC2_ON, 1.0),
                (dr::NOISE_ON, 1.0),
                (dr::CYC_ON, 1.0),
                // So the LFO's and the Cycling Env's speeds can be heard, on the cutoff.
                (dr::LP_SRC_A, 2.0),
                (dr::LP_AMT_A, 15.0),
                (dr::LP_SRC_B, 1.0),
                (dr::LP_AMT_B, 15.0),
                (dr::MOD_BASE, 4.0), // Velocity.
                (dr::MOD_BASE + 1, dest),
                (dr::MOD_BASE + 2, amount),
            ],
        );
        activate(&mut d);
        play(&mut d, &[note_on(0, 60)], 24_000).0
    };
    for dest in 0..dr::DESTINATIONS.len() {
        let (a, b) = (render(dest as f64, 0.0), render(dest as f64, 80.0));
        let moved: f32 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
        assert!(moved > 0.5, "{}: {moved}", dr::DESTINATIONS[dest]);
    }
}

#[test]
fn meld_matrix_reaches_filter_macro_and_pan() {
    let render = |cell: u32, amount: f64| {
        let mut m = sine_meld();
        set_all(
            &mut m,
            &[
                (me::ENGINE_A + me::F_ON, 1.0),
                (me::ENGINE_A + me::F_FREQ, 2_000.0),
            ],
        );
        m.set_param(cell, amount);
        play(&mut m, &[note_on_v(0, 69, 1.0)], 12_000)
    };
    let (l, _) = render(me::cell(0, 5, 6), -100.0); // Velocity > Filter Freq, down.
    let (open, _) = render(me::cell(0, 5, 6), 0.0);
    assert!(rms(&l[4_800..]) < rms(&open[4_800..]) * 0.8);
    let (l, r) = render(me::cell(0, 5, 1), 100.0); // Velocity > Pan, full right.
    assert!(rms(&r[4_800..]) > rms(&l[4_800..]) * 3.0);
}

#[test]
fn wavetable_time_slider_changes_envelope_speed() {
    let length = |time: f64| {
        let mut w = sine_wavetable();
        set_all(
            &mut w,
            &[
                (wt::AMP_ENV + wt::E_ATTACK, 20.0),
                (wt::AMP_ENV + wt::E_DECAY, 100.0),
                (wt::AMP_ENV + wt::E_SUSTAIN, 0.0),
                (wt::MOD_TIME, time),
            ],
        );
        let (l, _) = play(&mut w, &[note_on(0, 69)], 48_000);
        envelope_of(&l, 240).iter().filter(|e| **e > 0.02).count()
    };
    assert!(length(0.5) > length(0.0) * 3 / 2, "positive is slower");
    assert!(length(-0.5) < length(0.0) * 3 / 4, "negative is faster");
}

#[test]
fn wavetable_additive_and_multiplicative_targets() {
    let render = |list: &[(u32, f64)]| {
        let mut w = sine_wavetable();
        set_all(&mut w, list);
        play(&mut w, &[note_on_v(0, 69, 0.5)], 12_000).0
    };
    // Velocity into Volume is multiplicative: a half-velocity note at 100% is half the level.
    let plain = rms(&render(&[])[4_800..]);
    let scaled = rms(&render(&[(wt::cell(5, 18), 100.0)])[4_800..]);
    assert!((scaled / plain - 0.5).abs() < 0.05, "{}", scaled / plain);
    // Global Amount at 0 removes every assignment.
    let off = rms(&render(&[(wt::cell(5, 18), 100.0), (wt::MOD_AMOUNT, 0.0)])[4_800..]);
    assert!((off / plain - 1.0).abs() < 0.05, "{}", off / plain);
}

// --------------------------------------------------------------------------- Meld

#[test]
fn every_meld_oscillator_is_distinct_and_stable() {
    let render = |kind: usize, m1: f64, m2: f64| {
        let mut m = Meld::new();
        set_all(
            &mut m,
            &[
                (me::ENGINE_A + me::E_TYPE, kind as f64),
                (me::ENGINE_A + me::E_M1, m1),
                (me::ENGINE_A + me::E_M2, m2),
                (me::ENGINE_A + me::F_ON, 0.0),
                (me::ENGINE_A + me::AE_SUSTAIN, 1.0),
            ],
        );
        activate(&mut m);
        play(&mut m, &[note_on(0, 60)], 96_000).0
    };
    let mut spectra: Vec<Vec<f32>> = Vec::new();
    for kind in 0..mo::OSC_TYPES.len() {
        let x = render(kind, 50.0, 50.0);
        assert!(x.iter().all(|s| s.is_finite()), "{}", mo::OSC_TYPES[kind]);
        let late = &x[48_000..];
        let level = rms(late);
        assert!(level > 1e-3, "{} is silent: {level}", mo::OSC_TYPES[kind]);
        assert!(peak(late) < 3.0, "{} runs away", mo::OSC_TYPES[kind]);
        spectra.push(spectrum(&x).iter().map(|m| m.max(1e-6).ln()).collect());
    }
    for i in 0..spectra.len() {
        for j in 0..i {
            let d: f32 = spectra[i]
                .iter()
                .zip(&spectra[j])
                .map(|(a, b)| (a - b).abs())
                .sum::<f32>()
                / spectra[i].len() as f32;
            assert!(
                d > 0.1,
                "{} and {} are the same: {d}",
                mo::OSC_TYPES[i],
                mo::OSC_TYPES[j]
            );
        }
    }
}

const C_MAJOR: ScaleInfo = major_scale(0);

fn meld_scale_harness() -> Harness {
    let mut h = Harness::new(SR);
    h.transport.scale = Some(C_MAJOR);
    h
}

#[test]
fn meld_semitones_become_scale_degrees_with_use_current_scale() {
    let mut m = sine_meld();
    set_all(
        &mut m,
        &[(me::USE_SCALE, 1.0), (me::ENGINE_A + me::E_SEMI, 2.0)],
    );
    let mut h = meld_scale_harness();
    let l = more(&mut m, &mut h, &[note_on(0, 60)], 24_000);
    // Two degrees above C in C major is E, not D.
    assert!(cents(zero_cross_hz(&l[2_400..]), hz_of(64.0)).abs() < 3.0);
}

#[test]
fn meld_falls_back_to_semitones_without_a_scale() {
    let mut m = sine_meld();
    set_all(
        &mut m,
        &[(me::USE_SCALE, 1.0), (me::ENGINE_A + me::E_SEMI, 2.0)],
    );
    let (l, _) = play(&mut m, &[note_on(0, 60)], 24_000);
    assert!(cents(zero_cross_hz(&l[2_400..]), hz_of(62.0)).abs() < 3.0);
}

#[test]
fn meld_key_tracking_off_plays_c3_or_the_scale_root() {
    let mut m = sine_meld();
    m.set_param(me::ENGINE_A + me::KEY_TRACK, 0.0);
    let (l, _) = play(&mut m, &[note_on(0, 72)], 24_000);
    assert!(cents(zero_cross_hz(&l[2_400..]), hz_of(60.0)).abs() < 3.0);

    let mut m = sine_meld();
    set_all(
        &mut m,
        &[(me::ENGINE_A + me::KEY_TRACK, 0.0), (me::USE_SCALE, 1.0)],
    );
    let mut h = Harness::new(SR);
    h.transport.scale = Some(major_scale(7));
    let l = more(&mut m, &mut h, &[note_on(0, 72)], 24_000);
    assert!(
        cents(zero_cross_hz(&l[2_400..]), hz_of(67.0)).abs() < 3.0,
        "the root in the C3 octave"
    );
}

#[test]
fn meld_chord_follows_the_scale_or_builds_a_major_on_the_note() {
    let meld = |use_scale: f64| {
        let mut m = Meld::new();
        set_all(
            &mut m,
            &[
                (me::ENGINE_A + me::E_TYPE, mo::CHORD as f64),
                (me::ENGINE_A + me::E_M1, 0.0),
                (me::ENGINE_A + me::E_M2, 0.0),
                (me::ENGINE_A + me::F_ON, 0.0),
                (me::USE_SCALE, use_scale),
            ],
        );
        activate(&mut m);
        m
    };
    // C, on C major, in a C major chord shape: the third is E.
    let mut m = meld(1.0);
    let mut h = meld_scale_harness();
    let c_major = more(&mut m, &mut h, &[note_on(0, 60)], 24_000);
    // On D, with C major set, the shape picks the degrees of C major: F natural, not F sharp.
    let mut m = meld(1.0);
    let mut h = meld_scale_harness();
    let on_d = more(&mut m, &mut h, &[note_on(0, 62)], 24_000);
    let at = |x: &[f32], key: f32| tone_amplitude(&x[4_800..], hz_of(key), SR);
    assert!(at(&c_major, 64.0) > 0.02, "E");
    assert!(
        at(&on_d, 65.0) > 3.0 * at(&on_d, 66.0),
        "the scale's F, not F sharp"
    );
}

#[test]
fn meld_swarm_is_scale_aware() {
    let sum_in_vs_out = |aware: f64| {
        let mut m = Meld::new();
        set_all(
            &mut m,
            &[
                (me::ENGINE_A + me::E_TYPE, mo::SWARM_SINE as f64),
                (me::ENGINE_A + me::E_M1, 0.0),
                (me::ENGINE_A + me::E_M2, 100.0),
                (me::ENGINE_A + me::F_ON, 0.0),
                (me::ENGINE_A + me::OSC_SCALE, aware),
                (me::USE_SCALE, 1.0),
            ],
        );
        activate(&mut m);
        let mut h = Harness::new(SR);
        h.transport.scale = Some(major_scale(0));
        more(&mut m, &mut h, &[note_on(0, 60)], 48_000)
    };
    let x = sum_in_vs_out(1.0);
    let at = |x: &[f32], key: f32| tone_amplitude(&x[9_600..], hz_of(key), SR);
    // The widest set has a major 7th-ish chord; aware, it lands on scale notes.
    assert!(at(&x, 71.0) > at(&x, 70.0) * 2.0, "B over B flat");
    let y = sum_in_vs_out(0.0);
    assert!(rms(&y[9_600..]) > 0.0);
}

#[test]
fn meld_filter_types_all_run_and_differ() {
    use rlive_devices::devices::b11::meld_filter::FILTER_TYPES;
    let render = |kind: usize| {
        let mut m = Meld::new();
        set_all(
            &mut m,
            &[
                (me::ENGINE_A + me::E_TYPE, 8.0),
                (me::ENGINE_A + me::E_M1, 0.0),
                (me::ENGINE_A + me::E_M2, 0.0),
                (me::ENGINE_A + me::F_ON, 1.0),
                (me::ENGINE_A + me::F_TYPE, kind as f64),
                (me::ENGINE_A + me::F_M1, 70.0),
                (me::ENGINE_A + me::F_M2, 40.0),
                (me::ENGINE_A + me::F_FREQ, 1_000.0),
            ],
        );
        activate(&mut m);
        play(&mut m, &[note_on(0, 48)], 48_000).0
    };
    let mut out: Vec<Vec<f32>> = Vec::new();
    for (kind, name) in FILTER_TYPES.iter().enumerate() {
        let x = render(kind);
        assert!(x.iter().all(|s| s.is_finite()), "{name}");
        assert!(rms(&x[24_000..]) > 1e-3, "{name} is silent");
        out.push(x);
    }
    for i in 0..out.len() {
        for j in 0..i {
            let d: f32 = out[i].iter().zip(&out[j]).map(|(a, b)| (a - b).abs()).sum();
            assert!(d > 1.0, "{} = {}", FILTER_TYPES[i], FILTER_TYPES[j]);
        }
    }
}

#[test]
fn meld_filter_scale_awareness_retunes_the_plate() {
    let render = |aware: f64| {
        let mut m = Meld::new();
        set_all(
            &mut m,
            &[
                (me::ENGINE_A + me::E_TYPE, 19.0),
                (me::ENGINE_A + me::F_ON, 1.0),
                (me::ENGINE_A + me::F_TYPE, 15.0),
                (me::ENGINE_A + me::F_M1, 80.0),
                (me::ENGINE_A + me::FILTER_SCALE, aware),
                (me::USE_SCALE, 1.0),
            ],
        );
        activate(&mut m);
        let mut h = Harness::new(SR);
        h.transport.scale = Some(major_scale(0));
        more(&mut m, &mut h, &[note_on(0, 60)], 24_000)
    };
    let (a, b) = (render(1.0), render(0.0));
    let d: f32 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
    assert!(d > 1.0, "{d}");
}

#[test]
fn meld_links_envelopes_when_asked() {
    let run = |link: f64| {
        let mut m = sine_meld();
        set_all(
            &mut m,
            &[
                (me::ENGINE_B + me::E_ON, 1.0),
                (me::ENGINE_B + me::F_ON, 0.0),
                (me::ENGINE_A + me::E_ON, 0.0),
                (me::ENGINE_A + me::AE_ATTACK, 200.0),
                (me::ENGINE_B + me::AE_ATTACK, 2.0),
                (me::LINK_ENV, link),
            ],
        );
        let (l, _) = play(&mut m, &[note_on(0, 69)], 4_800);
        rms(&l[960..2_400])
    };
    assert!(run(1.0) < run(0.0) * 0.8, "B follows A's slow attack");
}

#[test]
fn meld_gliss_steps_through_semitones() {
    let render = |gliss: f64| {
        let mut m = sine_meld();
        set_all(
            &mut m,
            &[
                (me::MONO, 1.0),
                (me::LEGATO, 1.0),
                (me::ENGINE_A + me::GLIDE_TIME, 600.0),
                (me::ENGINE_A + me::GLIDE_MODE, gliss),
            ],
        );
        let mut h = Harness::new(SR);
        more(&mut m, &mut h, &[note_on(0, 60)], 4_800);
        more(&mut m, &mut h, &[note_on(0, 72)], 28_800)
    };
    let cents_in_semitone = |x: &[f32]| {
        let mut furthest = 0.0f64;
        for window in x.chunks(1_200).skip(1).take(22) {
            let hz = zero_cross_hz(window);
            let semis = 12.0 * (hz / 440.0).log2();
            furthest = furthest.max(((semis - semis.round()) * 100.0).abs());
        }
        furthest
    };
    let porta = cents_in_semitone(&render(0.0));
    let gliss = cents_in_semitone(&render(1.0));
    assert!(gliss < porta * 0.6, "porta {porta}, gliss {gliss}");
}

#[test]
fn meld_drive_and_limiter_shape_the_voice() {
    let render = |list: &[(u32, f64)]| {
        let mut m = sine_meld();
        set_all(&mut m, list);
        play(&mut m, &[note_on(0, 60)], 12_000).0
    };
    let plain = peak(&render(&[])[2_400..]);
    let driven = peak(&render(&[(me::DRIVE, 100.0)])[2_400..]);
    assert!(driven > plain, "drive adds level: {driven} {plain}");
    let limited =
        peak(&render(&[(me::DRIVE, 100.0), (me::VOLUME, 12.0), (me::LIMIT, 1.0)])[2_400..]);
    assert!(limited <= 1.01 * db_to_gain(12.0), "{limited}");
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

// ---------------------------------------------------------------------- Wavetable

#[test]
fn wavetable_position_moves_through_the_table() {
    let level3 = |pos: f64| {
        let mut w = sine_wavetable();
        w.set_param(wt::OSC1 + wt::O_POSITION, pos);
        let (l, _) = play(&mut w, &[note_on(0, 60)], 12_000);
        let f = hz_of(60.0);
        20.0 * (tone_amplitude(&l, f * 3.0, SR) / tone_amplitude(&l, f, SR)).log10()
    };
    // Basic Shapes starts as a sine and ends as a square, a third of whose third harmonic.
    assert!(level3(0.0) < -40.0, "{}", level3(0.0));
    assert!(level3(1.0) > -12.0, "{}", level3(1.0));
}

fn two_cycle_sample(dc: f32, seam_step: bool) -> Arc<SampleBuffer> {
    let n = 2048;
    let mut data = vec![0.0f32; n];
    for (i, s) in data.iter_mut().enumerate() {
        *s = (std::f32::consts::TAU * i as f32 / n as f32).sin() * 0.25 + dc;
    }
    if seam_step {
        // Make the end of the frame jump.
        for s in data.iter_mut().skip(n - 200) {
            *s += 0.5;
        }
    }
    let frames = [data.clone(), data.clone()].concat();
    Arc::new(SampleBuffer {
        name: "test".into(),
        left: frames.clone(),
        right: frames,
        sample_rate: SR,
    })
}

fn import(sample: Arc<SampleBuffer>, raw: bool) -> WavetableSynth {
    let mut w = WavetableSynth::new(&DeviceInit::default());
    set_all(
        &mut w,
        &[
            (wt::OSC1 + wt::O_CATEGORY, 3.0),
            (wt::OSC1 + wt::O_RAW, raw as u8 as f64),
            (wt::FILTER1 + wt::F_ON, 0.0),
        ],
    );
    activate(&mut w);
    let resource: Resource = sample;
    let back = assert_no_alloc(|| w.set_resource(0, resource));
    assert!(back.is_none());
    let mut h = Harness::new(SR);
    more(&mut w, &mut h, &[], BLOCK * 64);
    assert!(w.user_table_ready(0));
    w
}

#[test]
fn import_splits_frames_by_the_file_length() {
    assert_eq!(wt::frame_layout(2048 * 4), (2048, 4));
    assert_eq!(wt::frame_layout(1024 * 3), (1024, 3));
    assert_eq!(wt::frame_layout(4096), (2048, 2));
    assert_eq!(wt::frame_layout(0).1, 0);
    assert_eq!(wt::frame_layout(2048 * 1000).1, wt::MAX_USER_FRAMES);
    assert_eq!(wt::frame_layout(5000), (2048, 2));
}

#[test]
fn import_cleanup_removes_dc_and_closes_the_seam_and_raw_keeps_them() {
    let sample = two_cycle_sample(0.3, true);
    let mut clean = [0.0f32; 2048];
    let (size, frames) = wt::frame_layout(sample.len());
    wt::read_frame(&sample, frames, size, 0, false, &mut clean);
    let mean: f32 = clean.iter().sum::<f32>() / 2048.0;
    assert!(mean.abs() < 0.01, "{mean}");
    let seam = (clean[2047] - clean[0]).abs();
    assert!(seam < 0.05, "{seam}");

    let mut raw = [0.0f32; 2048];
    wt::read_frame(&sample, frames, size, 0, true, &mut raw);
    let mean_raw: f32 = raw.iter().sum::<f32>() / 2048.0;
    assert!(mean_raw > 0.2, "raw keeps DC: {mean_raw}");
    assert!((raw[2047] - raw[0]).abs() > 0.3, "raw keeps the seam");
}

#[test]
fn a_loaded_file_plays_at_the_right_pitch() {
    let mut w = import(two_cycle_sample(0.0, false), false);
    let (l, _) = play(&mut w, &[note_on(0, 69)], 24_000);
    assert!(cents(zero_cross_hz(&l[2_400..]), 440.0).abs() < 2.0);
}

#[test]
fn raw_mode_rebuilds_the_table() {
    let mut w = import(two_cycle_sample(0.3, true), false);
    let (l, _) = play(&mut w, &[note_on(0, 45)], 24_000);
    let clean = rms(&l[2_400..]);
    w.reset();
    let raw = {
        w.set_param(wt::OSC1 + wt::O_RAW, 1.0);
        let mut h = Harness::new(SR);
        more(&mut w, &mut h, &[], BLOCK * 64);
        let (l, _) = play(&mut w, &[note_on(0, 45)], 24_000);
        rms(&l[2_400..])
    };
    assert!((clean - raw).abs() > 1e-4, "{clean} {raw}");
}

#[test]
fn a_new_file_hands_back_the_old_one() {
    let mut w = import(two_cycle_sample(0.0, false), false);
    let second: Resource = two_cycle_sample(0.1, false);
    let old = assert_no_alloc(|| w.set_resource(0, second));
    let old = old
        .expect("the first file comes back")
        .downcast::<SampleBuffer>();
    assert!(old.is_ok());
    assert!(
        w.set_resource(5, Arc::new(1u8)).is_some(),
        "an unknown slot refuses"
    );
}

#[test]
fn a_file_loaded_at_creation_is_ready_at_once() {
    let w = WavetableSynth::new(&DeviceInit {
        samples: vec![Some(two_cycle_sample(0.0, false)), None],
        state: None,
    });
    assert!(w.user_table_ready(0) && !w.user_table_ready(1));
}

#[test]
fn wavetable_filter_routing() {
    let render = |routing: f64| {
        let mut w = sine_wavetable();
        set_all(
            &mut w,
            &[
                (wt::OSC2 + wt::O_ON, 1.0),
                (wt::OSC2 + wt::O_SEMI, 12.0),
                (wt::FILTER1 + wt::F_ON, 1.0),
                (wt::FILTER1 + wt::F_TYPE, 0.0),
                (wt::FILTER1 + wt::F_FREQ, 300.0),
                (wt::FILTER2 + wt::F_ON, 1.0),
                (wt::FILTER2 + wt::F_TYPE, 1.0),
                (wt::FILTER2 + wt::F_FREQ, 300.0),
                (wt::ROUTING, routing),
            ],
        );
        play(&mut w, &[note_on(0, 60)], 24_000).0
    };
    let at = |x: &[f32], h: f64| tone_amplitude(&x[4_800..], hz_of(60.0) * h, SR);
    // Split: Osc 1 through the low-pass, Osc 2 through the high-pass, so the octave up survives.
    let split = render(2.0);
    assert!(at(&split, 2.0) > at(&split, 1.0) * 0.5);
    // Serial: both through both, band-limited from both sides.
    let serial = render(0.0);
    assert!(rms(&serial[4_800..]) < rms(&split[4_800..]) * 0.7);
}

#[test]
fn wavetable_fm_classic_and_modern_change_the_tone() {
    let render = |fx: f64, a: f64, b: f64| {
        let mut w = sine_wavetable();
        set_all(
            &mut w,
            &[
                (wt::OSC1 + wt::O_FX, fx),
                (wt::OSC1 + wt::O_FX1, a),
                (wt::OSC1 + wt::O_FX2, b),
            ],
        );
        play(&mut w, &[note_on(0, 60)], 24_000).0
    };
    let base = render(0.0, 0.0, 0.0);
    for (fx, a, b) in [(1.0, 60.0, 50.0), (2.0, 80.0, 30.0), (3.0, 50.0, 60.0)] {
        let changed = render(fx, a, b);
        let d: f32 = base.iter().zip(&changed).map(|(x, y)| (x - y).abs()).sum();
        assert!(d > 10.0, "effect {fx}");
    }
    // FM at Tune 50% has a modulator an octave up: expect sidebands at f0 and f0 * 2.
    let fm = render(1.0, 60.0, 50.0);
    let f0 = hz_of(60.0);
    assert!(tone_amplitude(&fm[4_800..], f0 * 3.0, SR) > 0.005);
}

#[test]
fn wavetable_sub_can_drop_two_octaves() {
    let mut w = sine_wavetable();
    set_all(
        &mut w,
        &[
            (wt::OSC1 + wt::O_ON, 0.0),
            (wt::SUB_ON, 1.0),
            (wt::SUB_OCT, 2.0),
        ],
    );
    let (l, _) = play(&mut w, &[note_on(0, 69)], 24_000);
    assert!(cents(zero_cross_hz(&l[2_400..]), hz_of(45.0)).abs() < 3.0);
}

#[test]
fn hi_quality_still_plays_in_tune() {
    let mut w = sine_wavetable();
    w.set_param(wt::HI_QUALITY, 1.0);
    let (l, _) = play(&mut w, &[note_on(0, 69)], 24_000);
    assert!(cents(zero_cross_hz(&l[2_400..]), 440.0).abs() < 2.0);
}

// ---------------------------------------------------------------- hung notes, extremes

fn assert_silent_after_reset(p: &mut dyn Processor, notes: &[MidiEvent]) {
    let (_, _) = play(p, notes, 4_800);
    p.reset();
    let (l, r) = play(p, &[], 4_800);
    assert!(
        peak(&l) < 1e-9 && peak(&r) < 1e-9,
        "rings on: {} {}",
        peak(&l),
        peak(&r)
    );
    // The note-offs of voices the reset already ended, arriving late, are harmless.
    let (l, _) = play(p, &[note_off(0, 60), note_off(1, 64)], 4_800);
    assert!(
        peak(&l) < 1e-9,
        "a late note-off made a sound: {}",
        peak(&l)
    );
}

#[test]
fn reset_silences_every_synth() {
    let notes = [note_on(0, 60), note_on(0, 64)];
    assert_silent_after_reset(&mut sine_analog(), &notes);
    assert_silent_after_reset(&mut sine_drift(), &notes);
    assert_silent_after_reset(&mut sine_meld(), &notes);
    assert_silent_after_reset(&mut sine_wavetable(), &notes);
}

#[test]
fn all_notes_off_releases_every_voice() {
    let release = [MidiEvent::new(0, MidiMessage::AllNotesOff)];
    let finish = |p: &mut dyn Processor| {
        let mut h = Harness::new(SR);
        more(p, &mut h, &[note_on(0, 60)], 4_800);
        more(p, &mut h, &release, 24_000);
    };
    let mut a = sine_analog();
    finish(&mut a);
    assert_eq!(a.active_voices(), 0);
    let mut d = sine_drift();
    finish(&mut d);
    assert_eq!(d.active_voices(), 0);
    let mut m = sine_meld();
    finish(&mut m);
    assert_eq!(m.active_voices(), 0);
    let mut w = sine_wavetable();
    finish(&mut w);
    assert_eq!(w.active_voices(), 0);
}

fn assert_finite_at_extremes(make: &dyn Fn() -> Box<dyn Processor>, name: &str) {
    let hit = |p: &mut dyn Processor| {
        let mut h = Harness::new(SR);
        h.transport.scale = Some(C_MAJOR);
        let mut ev: Vec<MidiEvent> = vec![note_on(0, 0), note_on(1, 127), note_on(2, 60)];
        ev.push(MidiEvent::new(
            3,
            MidiMessage::PitchBend {
                channel: 0,
                value: 1.0,
            },
        ));
        ev.push(expr(4, 60, ExprKind::Pitch, 48.0));
        ev.push(expr(5, 60, ExprKind::Pressure, 1.0));
        ev.push(expr(6, 60, ExprKind::Slide, 1.0));
        let l = more(p, &mut h, &ev, BLOCK * 40);
        assert!(l.iter().all(|s| s.is_finite()), "{name} went non-finite");
        assert!(peak(&l) < 512.0, "{name} ran away: {}", peak(&l));
    };
    // Each parameter pushed to one end, then the other.
    for end in [0.0, 1.0] {
        let mut p = make();
        for param in params_of(name) {
            p.set_param(param.id, param.from_normalized(end));
        }
        hit(&mut *p);
    }
    // And a spread of alternating extremes.
    let mut p = make();
    for (i, param) in params_of(name).iter().enumerate() {
        p.set_param(
            param.id,
            param.from_normalized(if i % 3 == 0 { 0.0 } else { 1.0 }),
        );
    }
    hit(&mut *p);
}

fn params_of(name: &str) -> Vec<rlive_core::ParamInfo> {
    match name {
        "Analog" => an::params(),
        "Drift" => dr::params(),
        "Meld" => me::params(),
        _ => wt::params(),
    }
}

#[test]
fn every_synth_stays_finite_at_the_extremes() {
    assert_finite_at_extremes(&|| Box::new(sine_analog()), "Analog");
    assert_finite_at_extremes(&|| Box::new(sine_drift()), "Drift");
    assert_finite_at_extremes(&|| Box::new(sine_meld()), "Meld");
    assert_finite_at_extremes(&|| Box::new(sine_wavetable()), "Wavetable");
}

#[test]
fn tails_cover_the_release() {
    let check = |p: &dyn Processor, ms: f64| match p.tail() {
        Tail::Frames(n) => assert!(n as f64 >= ms * 0.048, "{n} for {ms} ms"),
        other => panic!("{other:?}"),
    };
    let mut a = sine_analog();
    a.set_param(an::AMP1 + an::A_ENV + 5, 400.0);
    check(&a, 400.0);
    let mut d = sine_drift();
    d.set_param(dr::E1_RELEASE, 300.0);
    check(&d, 300.0);
    let mut m = sine_meld();
    m.set_param(me::ENGINE_A + me::AE_RELEASE, 200.0);
    check(&m, 200.0);
    let mut w = sine_wavetable();
    w.set_param(wt::AMP_ENV + wt::E_RELEASE, 500.0);
    check(&w, 500.0);
}

#[test]
fn nothing_reports_latency() {
    assert_eq!(sine_analog().latency(), 0);
    assert_eq!(sine_drift().latency(), 0);
    assert_eq!(sine_meld().latency(), 0);
    assert_eq!(sine_wavetable().latency(), 0);
}

#[test]
fn the_analog_quick_routings_set_the_documented_paths() {
    let map = |i: usize| {
        an::quick_routing(i)
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    };
    let serial = map(3);
    assert_eq!(
        serial[&(an::FILTER1 + an::F_ROUTE)],
        100.0,
        "Filter 1 into Filter 2"
    );
    assert_eq!(serial[&(an::AMP1 + an::A_ON)], 0.0, "Amp 1 out of the way");
    let single = map(2);
    assert_eq!(single[&(an::FILTER2 + an::F_ON)], 0.0);
    assert_eq!(single[&(an::AMP2 + an::A_ON)], 0.0);
    let parallel = map(0);
    assert_eq!(parallel[&(an::OSC1 + an::O_BAL)], -1.0);
    assert_eq!(parallel[&(an::OSC2 + an::O_BAL)], 1.0);
    let shared = map(1);
    assert_eq!(shared[&(an::OSC1 + an::O_BAL)], 0.0);
}

/// `cargo test -p rlive-devices --test b11 cpu_per_voice -- --ignored --nocapture`
#[test]
#[ignore]
fn cpu_per_voice() {
    let measure = |name: &str, make: &dyn Fn() -> Box<dyn Processor>| {
        let mut p = make();
        let voices = 8;
        let events: Vec<MidiEvent> = (0..voices)
            .map(|i| note_on(i as u32, 48 + i as u8 * 2))
            .collect();
        let mut h = Harness::new(SR);
        more(&mut *p, &mut h, &events, 4_800);
        let blocks = 2_000;
        let start = std::time::Instant::now();
        for _ in 0..blocks {
            let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
            h.process(&mut *p, &mut l, &mut r, &[], &[]);
        }
        let us = start.elapsed().as_secs_f64() * 1e6 / blocks as f64;
        let budget = BLOCK as f64 / SR * 1e6;
        println!(
            "{name}: {:.2} us per voice per block ({:.2}% CPU per voice at {} kHz)",
            us / voices as f64,
            us / voices as f64 / budget * 100.0,
            SR / 1000.0
        );
    };
    let full_wavetable = || {
        let mut w = WavetableSynth::new(&DeviceInit::default());
        set_all(
            &mut w,
            &[
                (wt::OSC2 + wt::O_ON, 1.0),
                (wt::UNISON, 1.0),
                (wt::UNISON_VOICES, 2.0),
                (wt::SUB_ON, 1.0),
            ],
        );
        activate(&mut w);
        Box::new(w) as Box<dyn Processor>
    };
    measure("Analog (both osc, default filters)", &|| {
        let mut a = Analog::new();
        set_all(
            &mut a,
            &[
                (an::OSC2 + an::O_ON, 1.0),
                (an::AMP2 + an::A_ON, 1.0),
                (an::FILTER2 + an::F_ON, 1.0),
            ],
        );
        activate(&mut a);
        Box::new(a)
    });
    measure("Drift (two oscillators and noise)", &|| {
        let mut d = Drift::new();
        set_all(&mut d, &[(dr::OSC2_ON, 1.0), (dr::NOISE_ON, 1.0)]);
        activate(&mut d);
        Box::new(d)
    });
    measure("Meld (two engines, Swarm and plate)", &|| {
        let mut m = Meld::new();
        set_all(
            &mut m,
            &[
                (me::ENGINE_B + me::E_ON, 1.0),
                (me::ENGINE_A + me::E_TYPE, 8.0),
                (me::ENGINE_B + me::F_TYPE, 15.0),
            ],
        );
        activate(&mut m);
        Box::new(m)
    });
    measure("Wavetable (2 osc, 4x unison, sub)", &full_wavetable);
}
