//! Batch B12: Operator, Electric, Tension, Collision, against analytic expectations.

use rlive_core::{
    AudioBuffer, EventQueue, ExprKind, MidiEvent, MidiMessage, ParamEvent, ParamId, ParamOutQueue,
    ProcessContext, Processor, TransportInfo, ViewChannel, ViewWriter,
};
use rlive_devices::devices::b12::{
    collision as co, common, electric as el, operator as op, operator_waves as waves, tension as te,
};
use rlive_devices::dsp::fft::{Complex32, Fft};
use rlive_devices::{find, DeviceInit};
use rlive_testkit::{assert_no_alloc, peak, rms, tone_amplitude as goertzel, Harness};
use std::f32::consts::TAU;
use std::f64::consts::PI as PI64;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn device(id: &str) -> Box<dyn Processor> {
    let info = find(id).unwrap();
    let mut p = (info.create)(&DeviceInit::default());
    p.activate(SR, BLOCK);
    p
}

fn on(offset: u32, key: u8) -> MidiEvent {
    on_vel(offset, key, 1.0)
}

fn on_vel(offset: u32, key: u8, velocity: f32) -> MidiEvent {
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

fn off(offset: u32, key: u8) -> MidiEvent {
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

/// Renders `frames` with events at their absolute frame, in blocks, under the allocation guard.
fn play(p: &mut dyn Processor, frames: usize, events: &[MidiEvent]) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    let (mut left, mut right) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    let mut at = 0;
    while at < frames {
        let n = BLOCK.min(frames - at);
        let block: Vec<MidiEvent> = events
            .iter()
            .filter(|e| (e.offset as usize) >= at && (e.offset as usize) < at + n)
            .map(|e| MidiEvent::new(e.offset - at as u32, e.message))
            .collect();
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        assert_no_alloc(|| h.process(p, &mut l, &mut r, &block, &[]));
        h.transport.tick += n as f64 * h.transport.ticks_per_sample();
        h.transport.sample_pos += n as u64;
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
        at += n;
    }
    (left, right)
}

fn left(p: &mut dyn Processor, frames: usize, events: &[MidiEvent]) -> Vec<f32> {
    play(p, frames, events).0
}

/// `goertzel`, Hann-windowed so a strong line doesn't leak into its neighbours.
fn tone_amplitude(buf: &[f32], hz: f64, sr: f64) -> f64 {
    let n = buf.len().max(1) as f64;
    let windowed: Vec<f32> = buf
        .iter()
        .enumerate()
        .map(|(i, s)| (*s as f64 * (1.0 - (2.0 * PI64 * i as f64 / n).cos())) as f32)
        .collect();
    goertzel(&windowed, hz, sr)
}

/// A Hann-windowed magnitude spectrum, one bin per `SR / len`.
fn spectrum(buf: &[f32]) -> Vec<f32> {
    let len = buf.len().next_power_of_two() / 2;
    let mut input: Vec<f32> = buf[..len]
        .iter()
        .enumerate()
        .map(|(i, s)| *s * (0.5 - 0.5 * (TAU * i as f32 / len as f32).cos()))
        .collect();
    let mut fft = Fft::new(len);
    let mut bins = vec![Complex32::new(0.0, 0.0); fft.bins()];
    fft.forward(&mut input, &mut bins);
    bins.iter().map(|c| c.norm()).collect()
}

/// The share of power that isn't within `half_width` Hz of one of `lines`.
fn power_outside(buf: &[f32], lines: &[f64], half_width: f64) -> f64 {
    let mag = spectrum(buf);
    let bin_hz = SR / (mag.len() as f64 - 1.0) / 2.0;
    let (mut outside, mut all) = (0.0, 0.0);
    for (i, m) in mag.iter().enumerate().skip(2) {
        let hz = i as f64 * bin_hz;
        let power = (*m as f64).powi(2);
        all += power;
        if !lines.iter().any(|f| (hz - f).abs() < half_width) {
            outside += power;
        }
    }
    outside / all.max(1e-30)
}

/// The frequency of the strongest line within 3% of `around`, to a fraction of a cent.
fn frequency_near(buf: &[f32], around: f64, sr: f64) -> f64 {
    let window = buf;
    let (mut best, mut best_hz) = (0.0, around);
    let mut hz = around * 0.97;
    while hz < around * 1.03 {
        let a = tone_amplitude(window, hz, sr);
        if a > best {
            best = a;
            best_hz = hz;
        }
        hz *= 1.0 + 0.2f64 / 1200.0;
    }
    let mut fine = best_hz * (1.0 - 0.25 / 1200.0);
    let top = best_hz * (1.0 + 0.25 / 1200.0);
    while fine < top {
        let a = tone_amplitude(window, fine, sr);
        if a > best {
            best = a;
            best_hz = fine;
        }
        fine *= 1.0 + 0.02f64 / 1200.0;
    }
    best_hz
}

fn cents(hz: f64, reference: f64) -> f64 {
    1200.0 * (hz / reference).log2()
}

fn set(p: &mut dyn Processor, id: u32, value: f64) {
    p.set_param(id, value);
}

/// Time for RMS to fall by 60 dB, from where it is at `from`, measured over short windows.
fn t60(buf: &[f32], from: usize) -> f32 {
    const W: usize = 512;
    let reference = rms(&buf[from..from + W]);
    let target = reference * 0.001;
    let mut at = from + W;
    while at + W < buf.len() {
        if rms(&buf[at..at + W]) < target {
            return (at - from) as f32 / SR as f32;
        }
        at += W;
    }
    f32::INFINITY
}

// --------------------------------------------------------------------------------- collision

fn collision(kind: usize) -> Box<dyn Processor> {
    let mut p = device("rlive.collision");
    let base = co::RES_BASE[0];
    set(&mut *p, base + co::R_TYPE, kind as f64);
    set(&mut *p, base + co::R_QUALITY, 3.0);
    set(&mut *p, co::M_STIFF, 100.0);
    set(&mut *p, base + co::R_HIT, 55.0);
    set(&mut *p, base + co::R_POS_L, 55.0);
    set(&mut *p, base + co::R_POS_R, 55.0);
    set(&mut *p, base + co::R_DECAY, 4_000.0);
    p
}

const FIRST_NOTE: f64 = 261.625_56; // C3 = MIDI 60

fn expect_partial(buf: &[f32], hz: f64, floor: f64, label: &str) {
    let level = tone_amplitude(buf, hz, SR);
    assert!(level > floor, "{label}: {hz:.1} Hz at {level:.5}");
}

#[test]
fn collision_mode_tables_match_the_textbook_for_each_body() {
    let mut table = [co::Mode::default(); common::MAX_MODES];
    co::modes(co::BEAM, 5, 1.0, &mut table);
    assert!((table[1].ratio - 2.7565).abs() < 1e-3);
    assert!((table[4].ratio - 13.3444).abs() < 1e-3);
    co::modes(co::MARIMBA, 3, 1.0, &mut table);
    assert_eq!(
        [table[0].ratio, table[1].ratio, table[2].ratio],
        [1.0, 4.0, 10.0]
    );
    co::modes(co::STRING, 4, 1.0, &mut table);
    assert_eq!(table[3].ratio, 4.0);
    co::modes(co::MEMBRANE, 3, 1.0, &mut table);
    assert!((table[1].ratio - (5.0f32 / 2.0).sqrt()).abs() < 1e-3);
    co::modes(co::PLATE, 3, 1.0, &mut table);
    assert!((table[1].ratio - 2.5).abs() < 1e-3);
    // A wider membrane: the (1,2) mode moves down towards the fundamental.
    co::modes(co::MEMBRANE, 3, 2.0, &mut table);
    let expected = ((1.0f32 + 1.0) / (1.0 + 0.25)).sqrt();
    assert!(
        (table[1].ratio - expected).abs() < 1e-3,
        "{}",
        table[1].ratio
    );
}

#[test]
fn collision_rings_at_each_bodys_partials() {
    let cases = [
        (co::BEAM, &[1.0, 2.7565, 5.4039][..]),
        (co::MARIMBA, &[1.0, 4.0, 10.0][..]),
        (co::STRING, &[1.0, 2.0, 3.0][..]),
        (co::MEMBRANE, &[1.0, 1.5811][..]),
        (co::PLATE, &[1.0, 2.5][..]),
    ];
    for (kind, partials) in cases {
        let mut p = collision(kind);
        let out = left(&mut *p, 24_000, &[on(0, 60)]);
        for ratio in partials {
            expect_partial(&out[256..], FIRST_NOTE * *ratio, 3e-3, "partial");
        }
        // Between partials there is nothing to hear.
        let gap = FIRST_NOTE * (partials[0] + 0.13);
        assert!(tone_amplitude(&out[256..], gap, SR) < 5e-3);
    }
}

#[test]
fn collision_hit_at_the_centre_silences_a_beams_antisymmetric_modes() {
    let mut p = collision(co::BEAM);
    set(&mut *p, co::RES_BASE[0] + co::R_HIT, 0.0);
    let centre = left(&mut *p, 24_000, &[on(0, 60)]);
    let mut q = collision(co::BEAM);
    set(&mut *q, co::RES_BASE[0] + co::R_HIT, 60.0);
    let edge = left(&mut *q, 24_000, &[on(0, 60)]);
    let second = FIRST_NOTE * 2.7565;
    assert!(tone_amplitude(&centre[256..], second, SR) < 1e-4);
    assert!(tone_amplitude(&edge[256..], second, SR) > 1e-3);
}

#[test]
fn collision_decay_material_and_note_off_shape_the_ring() {
    let base = co::RES_BASE[0];
    let t60_of = |decay: f64, material: f64, key: u8| {
        let mut p = collision(co::STRING);
        set(&mut *p, base + co::R_DECAY, decay);
        set(&mut *p, base + co::R_MATERIAL, material);
        set(&mut *p, base + co::R_QUALITY, 0.0);
        let hz = FIRST_NOTE * 2f64.powf((key as f64 - 60.0) / 12.0) * 2.0;
        let out = left(&mut *p, 96_000, &[on(0, key)]);
        (out, hz)
    };
    // Decay at the fundamental is what the slider says, near enough.
    let (out, _) = t60_of(1_000.0, 0.0, 60);
    let measured = t60(&out, 2_048);
    assert!((measured - 1.0).abs() < 0.25, "{measured}");
    // Negative Material: higher partials decay faster than the fundamental; positive: slower.
    let (wood, hz2) = t60_of(2_000.0, -100.0, 60);
    let (glass, _) = t60_of(2_000.0, 100.0, 60);
    let early = 2_048..2_048 + 24_000;
    let late = 72_000..96_000;
    let decay_of = |buf: &[f32]| {
        tone_amplitude(&buf[late.clone()], hz2, SR) / tone_amplitude(&buf[early.clone()], hz2, SR)
    };
    assert!(decay_of(&glass) > 2.0 * decay_of(&wood));

    // Note Off at 0% ignores the key; at 100% it mutes at once.
    let mut ring = collision(co::BEAM);
    let held = left(&mut *ring, 12_000, &[on(0, 60), off(2_400, 60)]);
    let mut mute = collision(co::BEAM);
    set(&mut *mute, base + co::R_NOTE_OFF, 100.0);
    let muted = left(&mut *mute, 12_000, &[on(0, 60), off(2_400, 60)]);
    assert!(rms(&held[6_000..12_000]) > 0.05);
    assert!(rms(&muted[6_000..12_000]) < 1e-3);
    assert!(
        rms(&muted[2_000..2_400]) > 0.05,
        "plays before the note off"
    );
}

#[test]
fn collision_tune_key_tracks_the_keyboard_and_the_pitch_envelope_glides_home() {
    let base = co::RES_BASE[0];
    for (key_track, key, expected) in [
        (100.0, 72u8, FIRST_NOTE * 2.0),
        (200.0, 66u8, FIRST_NOTE * 2.0),
        (-100.0, 72u8, FIRST_NOTE * 0.5),
    ] {
        let mut p = collision(co::STRING);
        set(&mut *p, base + co::R_TUNE_KEY, key_track);
        let out = left(&mut *p, 16_384, &[on(0, key)]);
        let hz = frequency_near(&out[4096..], expected, SR);
        assert!(cents(hz, expected).abs() < 3.0, "{key_track}: {hz}");
    }
    let mut p = collision(co::STRING);
    set(&mut *p, base + co::R_PENV, 12.0);
    set(&mut *p, base + co::R_PENV_TIME, 300.0);
    let out = left(&mut *p, 32_768, &[on(0, 60)]);
    let start = frequency_near(&out[..2048], FIRST_NOTE * 2.0, SR);
    let end = frequency_near(&out[22_000..30_000], FIRST_NOTE, SR);
    assert!(start > FIRST_NOTE * 1.6, "{start}");
    assert!(cents(end, FIRST_NOTE).abs() < 6.0, "{end}");
}

#[test]
fn collision_pipe_opening_moves_from_odd_harmonics_to_all_of_them() {
    let base = co::RES_BASE[0];
    let render = |kind: usize, opening: f64| {
        let mut p = device("rlive.collision");
        set(&mut *p, base + co::R_TYPE, kind as f64);
        set(&mut *p, base + co::R_OPENING, opening);
        set(&mut *p, base + co::R_DECAY, 3_000.0);
        set(&mut *p, base + co::R_RADIUS, 80.0);
        set(&mut *p, co::M_STIFF, 100.0);
        left(&mut *p, 24_000, &[on(0, 57)])
    };
    let f = 220.0;
    let closed = render(co::PIPE, 0.0);
    assert!(
        tone_amplitude(&closed[512..], f, SR) > 3.0 * tone_amplitude(&closed[512..], 2.0 * f, SR)
    );
    let open = render(co::PIPE, 100.0);
    assert!(tone_amplitude(&open[512..], 2.0 * f, SR) > 0.3 * tone_amplitude(&open[512..], f, SR));
    let tube = render(co::TUBE, 0.0);
    assert!(tone_amplitude(&tube[512..], 2.0 * f, SR) > 0.3 * tone_amplitude(&tube[512..], f, SR));
    let hz = frequency_near(&closed[2048..18_432], f, SR);
    assert!(cents(hz, f).abs() < 3.0, "{hz}");
}

#[test]
fn collision_quality_sets_how_many_overtones_ring() {
    let base = co::RES_BASE[0];
    let high_partial = FIRST_NOTE * 18.0;
    let level = |quality: f64| {
        let mut p = collision(co::STRING);
        set(&mut *p, base + co::R_QUALITY, quality);
        set(&mut *p, base + co::R_BRIGHT, 100.0);
        let out = left(&mut *p, 24_000, &[on(0, 60)]);
        tone_amplitude(&out, high_partial, SR)
    };
    assert!(level(3.0) > 1e-4, "High has it: {}", level(3.0));
    assert!(
        level(0.0) < level(3.0) * 0.1,
        "Eco stops at 8 partials: {} {}",
        level(0.0),
        level(3.0)
    );
}

#[test]
fn collision_stiffness_shortens_the_impact_and_brightens_it() {
    let bright = |stiffness: f64| {
        let mut p = collision(co::STRING);
        set(&mut *p, co::M_STIFF, stiffness);
        set(&mut *p, co::M_STIFF_VEL, 0.0);
        let out = left(&mut *p, 24_000, &[on(0, 60)]);
        tone_amplitude(&out, FIRST_NOTE * 12.0, SR) / tone_amplitude(&out, FIRST_NOTE, SR)
    };
    assert!(bright(100.0) > bright(0.0) * 2.0);
}

#[test]
fn collision_structure_routes_the_exciters() {
    let (r1, r2) = (co::RES_BASE[0], co::RES_BASE[1]);
    let render = |structure: f64, first_tune: f64| {
        let mut p = collision(co::STRING);
        set(&mut *p, co::STRUCTURE, structure);
        set(&mut *p, r1 + co::R_TUNE, first_tune);
        set(&mut *p, r2 + co::R_ON, 1.0);
        set(&mut *p, r2 + co::R_TYPE, co::STRING as f64);
        set(&mut *p, r2 + co::R_TUNE, 7.0);
        set(&mut *p, r2 + co::R_DECAY, 4_000.0);
        set(&mut *p, r1 + co::R_GAIN, -48.0);
        left(&mut *p, 48_000, &[on(0, 60)])
    };
    let fifth = FIRST_NOTE * 2f64.powf(7.0 / 12.0);
    let late =
        |structure: f64, tune: f64| tone_amplitude(&render(structure, tune)[8_192..], fifth, SR);
    // In parallel the first resonator's tuning can't reach what the second one rings at.
    let (same, moved) = (late(1.0, 0.0), late(1.0, 7.0));
    assert!((same - moved).abs() < same * 0.05, "{same} {moved}");
    // In series it can: the first resonator feeds the second, so matching it lifts the ring.
    assert!(
        late(0.0, 7.0) > late(0.0, 0.0) * 1.5,
        "{} {}",
        late(0.0, 7.0),
        late(0.0, 0.0)
    );
    // With the first resonator off, series falls back on the exciters.
    let bypass = {
        let mut p = collision(co::STRING);
        set(&mut *p, co::STRUCTURE, 0.0);
        set(&mut *p, r1 + co::R_ON, 0.0);
        set(&mut *p, r2 + co::R_ON, 1.0);
        set(&mut *p, r2 + co::R_TYPE, co::STRING as f64);
        set(&mut *p, r2 + co::R_TUNE, 7.0);
        tone_amplitude(&left(&mut *p, 24_000, &[on(0, 60)]), fifth, SR)
    };
    assert!(bypass > 0.01);
}

#[test]
fn collision_note_off_and_mallet_off_kill_sound_when_they_should() {
    let mut p = collision(co::BEAM);
    set(&mut *p, co::M_ON, 0.0);
    set(&mut *p, co::N_ON, 0.0);
    assert!(
        peak(&left(&mut *p, 4_096, &[on(0, 60)])) < 1e-6,
        "no exciter, no sound"
    );
    let mut q = collision(co::BEAM);
    set(&mut *q, co::M_ON, 0.0);
    set(&mut *q, co::N_ON, 1.0);
    assert!(
        peak(&left(&mut *q, 4_096, &[on(0, 60)])) > 1e-3,
        "noise exciter sounds"
    );
}

#[test]
fn collision_lfos_modulate_each_other_and_the_resonators() {
    let base = co::RES_BASE[0];
    let lfo = |p: &mut dyn Processor| {
        set(p, base + co::R_TYPE, co::STRING as f64);
        set(p, co::LFO_BASE[0] + co::L_ON, 1.0);
        set(p, co::LFO_BASE[0] + co::L_RATE, 2.0);
        set(p, co::LFO_BASE[0] + co::L_DEST_A, 12.0); // Res 1 Tune
        set(p, co::LFO_BASE[0] + co::L_AMT_A, 2.0);
    };
    let pitch_spread = |buf: &[f32]| {
        let mut min = f64::MAX;
        let mut max = 0.0f64;
        for w in buf[4_096..].as_chunks::<4_096>().0 {
            let hz = frequency_near(w, FIRST_NOTE, SR);
            min = min.min(hz);
            max = max.max(hz);
        }
        max - min
    };
    let mut plain = collision(co::STRING);
    lfo(&mut *plain);
    let free = left(&mut *plain, 48_000, &[on(0, 60)]);
    assert!(
        pitch_spread(&free) > 2.0,
        "LFO 1 moves the pitch: {}",
        pitch_spread(&free)
    );
    // LFO 2 rises over this second, pulling LFO 1's amount towards nothing.
    let mut with_mod = collision(co::STRING);
    lfo(&mut *with_mod);
    set(&mut *with_mod, co::LFO_BASE[1] + co::L_ON, 1.0);
    set(&mut *with_mod, co::LFO_BASE[1] + co::L_RATE, 0.25);
    set(&mut *with_mod, co::LFO_BASE[1] + co::L_DEST_A, 24.0); // LFO 1 Amount
    set(&mut *with_mod, co::LFO_BASE[1] + co::L_AMT_A, -100.0);
    let tamed = left(&mut *with_mod, 48_000, &[on(0, 60)]);
    assert!(
        pitch_spread(&tamed) < pitch_spread(&free) * 0.85,
        "{} {}",
        pitch_spread(&tamed),
        pitch_spread(&free)
    );
}

// ---------------------------------------------------------------------------------- electric

#[test]
fn electric_is_in_tune_across_the_keyboard() {
    for key in [24u8, 36, 48, 60, 72, 84, 96] {
        let mut p = device("rlive.electric");
        set(&mut *p, el::SYMMETRY, 70.0);
        let out = left(&mut *p, 32_768, &[on(0, key)]);
        let expected = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
        let hz = frequency_near(&out[2048..], expected, SR);
        assert!(cents(hz, expected).abs() < 2.0, "key {key}: {hz}");
    }
}

#[test]
fn electric_stretch_detunes_the_ends_opposite_ways() {
    let render = |stretch: f64, key: u8| {
        let mut p = device("rlive.electric");
        set(&mut *p, el::SYMMETRY, 70.0);
        set(&mut *p, el::STRETCH, stretch);
        let out = left(&mut *p, 32_768, &[on(0, key)]);
        let plain = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
        cents(frequency_near(&out[2048..], plain, SR), plain)
    };
    assert!(render(100.0, 96) > 10.0);
    assert!(render(100.0, 36) < -8.0);
    assert!(render(-100.0, 96) < -10.0);
}

#[test]
fn electric_decays_and_releases_as_set() {
    let render = |decay: f64, release: f64, off_at: Option<u32>| {
        let mut p = device("rlive.electric");
        set(&mut *p, el::SYMMETRY, 70.0);
        set(&mut *p, el::TINE_DECAY, decay);
        set(&mut *p, el::TONE_DECAY, decay);
        set(&mut *p, el::RELEASE, release);
        set(&mut *p, el::DAMP_LEVEL, 0.0);
        set(&mut *p, el::NOISE, 0.0);
        let mut events = vec![on(0, 60)];
        events.extend(off_at.map(|at| off(at, 60)));
        left(&mut *p, 192_000, &events)
    };
    let short = render(500.0, 5_000.0, None);
    let long = render(3_000.0, 5_000.0, None);
    assert!(rms(&short[60_000..90_000]) < rms(&long[60_000..90_000]) * 0.5);
    let held = render(3_000.0, 50.0, None);
    let let_go = render(3_000.0, 50.0, Some(12_000));
    assert!(
        rms(&let_go[20_000..30_000]) < rms(&held[20_000..30_000]) * 0.05,
        "release mutes it"
    );
}

#[test]
fn electric_pickup_position_changes_the_harmonics_and_distance_overdrives() {
    let render = |symmetry: f64, distance: f64, kind: f64| {
        let mut p = device("rlive.electric");
        set(&mut *p, el::SYMMETRY, symmetry);
        set(&mut *p, el::DISTANCE, distance);
        set(&mut *p, el::PICKUP_TYPE, kind);
        set(&mut *p, el::NOISE, 0.0);
        set(&mut *p, el::DAMP_LEVEL, 0.0);
        left(&mut *p, 24_000, &[on(0, 60)])
    };
    let ratio2 = |buf: &[f32]| {
        tone_amplitude(&buf[2_048..], FIRST_NOTE * 2.0, SR)
            / tone_amplitude(&buf[2_048..], FIRST_NOTE, SR)
    };
    assert!(ratio2(&render(50.0, 50.0, 0.0)) > ratio2(&render(90.0, 50.0, 0.0)) * 2.0);
    // Closer in, the fundamental has more upper harmonics riding on it.
    let distortion = |buf: &[f32]| {
        let f = tone_amplitude(&buf[2_048..], FIRST_NOTE, SR);
        let mut upper = 0.0;
        for k in 2..8 {
            upper += tone_amplitude(&buf[2_048..], FIRST_NOTE * k as f64, SR);
        }
        upper / f
    };
    assert!(distortion(&render(70.0, 5.0, 0.0)) > distortion(&render(70.0, 90.0, 0.0)) * 1.3);
    // The two pickups make different sounds from the same fork.
    let (r, w) = (render(70.0, 50.0, 0.0), render(70.0, 50.0, 1.0));
    assert!((ratio2(&r) - ratio2(&w)).abs() > 0.02);
}

#[test]
fn electric_color_tilts_the_partials_and_damper_noise_follows_att_rel() {
    let color = |c: f64| {
        let mut p = device("rlive.electric");
        set(&mut *p, el::TINE_COLOR, c);
        set(&mut *p, el::SYMMETRY, 90.0);
        set(&mut *p, el::DAMP_LEVEL, 0.0);
        set(&mut *p, el::NOISE, 0.0);
        let out = left(&mut *p, 16_384, &[on(0, 60)]);
        tone_amplitude(&out[..4_096], FIRST_NOTE * 6.267, SR)
    };
    assert!(color(100.0) > color(0.0) * 3.0);

    let noise = |att_rel: f64| {
        let mut p = device("rlive.electric");
        set(&mut *p, el::DAMP_LEVEL, 100.0);
        set(&mut *p, el::DAMP_ATT_REL, att_rel);
        set(&mut *p, el::FORCE, 0.0);
        set(&mut *p, el::NOISE, 0.0);
        let out = left(&mut *p, 9_600, &[on(0, 60), off(4_800, 60)]);
        (rms(&out[..1_200]), rms(&out[4_800..6_000]))
    };
    let (att, rel) = noise(-100.0);
    assert!(att > 1e-3 && rel < att * 0.1, "attack only: {att} {rel}");
    let (att, rel) = noise(100.0);
    assert!(rel > 1e-3 && att < rel * 0.1, "release only: {att} {rel}");
    let (att, rel) = noise(0.0);
    assert!(att > 1e-3 && rel > 1e-3, "both");
}

// ----------------------------------------------------------------------------------- tension

fn tension(exciter: usize) -> Box<dyn Processor> {
    let mut p = device("rlive.tension");
    set(&mut *p, te::EX_TYPE, exciter as f64);
    set(&mut *p, te::S_INHARM, 0.0);
    set(&mut *p, te::B_ON, 0.0);
    set(&mut *p, te::D_ON, 0.0);
    set(&mut *p, te::VOLUME, 0.0);
    p
}

#[test]
fn tension_is_in_tune_across_the_keyboard() {
    for exciter in [te::HAMMER, te::PLECTRUM, te::HAMMER_BOUNCE] {
        for key in [36u8, 48, 60, 72, 84, 96] {
            let mut p = tension(exciter);
            let out = left(&mut *p, 96_000, &[on(0, key)]);
            let expected = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
            // The bouncing hammer keeps touching the string for half a second.
            let hz = frequency_near(&out[28_800..61_568], expected, SR);
            assert!(
                cents(hz, expected).abs() < 3.0,
                "ex {exciter} key {key}: {hz} ({:.1})",
                cents(hz, expected)
            );
        }
    }
}

#[test]
fn tension_bow_locks_to_the_string_and_sustains() {
    let mut p = tension(te::BOW);
    set(&mut *p, te::EX_VELOCITY, 60.0);
    let out = left(&mut *p, 48_000, &[on(0, 57)]);
    let hz = frequency_near(&out[12_000..44_000], 220.0, SR);
    assert!(cents(hz, 220.0).abs() < 25.0, "{hz}");
    // A bow keeps it going for as long as it's held.
    assert!(rms(&out[36_000..48_000]) > rms(&out[12_000..24_000]) * 0.5);
    assert!(
        rms(&out[36_000..48_000]) > 1e-3,
        "{}",
        rms(&out[36_000..48_000])
    );
}

#[test]
fn tension_decay_and_ratio_set_onset_and_release_decay() {
    let render = |decay: f64, ratio: f64, release_at: Option<u32>| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::S_DECAY, decay);
        set(&mut *p, te::S_DECAY_KEY, 0.0);
        set(&mut *p, te::S_RATIO, ratio);
        set(&mut *p, te::S_DAMPING, 100.0);
        let mut events = vec![on(0, 60)];
        events.extend(release_at.map(|t| off(t, 60)));
        left(&mut *p, 192_000, &events)
    };
    let out = render(1_000.0, 0.0, None);
    let measured = t60(&out, 4_096);
    assert!((measured - 1.0).abs() < 0.3, "t60 {measured}");
    // Ratio 0: release decays as fast as the held note; at 100% it's nearly immediate.
    let held = render(4_000.0, 0.0, Some(4_800));
    let damped = render(4_000.0, 100.0, Some(4_800));
    assert!(rms(&held[48_000..96_000]) > 0.05 * rms(&held[2_400..4_800]));
    assert!(rms(&damped[20_000..40_000]) < 2e-4);
}

#[test]
fn tension_string_damping_keeps_or_loses_the_top_end() {
    let brightness = |damping: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::S_DAMPING, damping);
        set(&mut *p, te::S_DAMPING_KEY, 0.0);
        let out = left(&mut *p, 48_000, &[on(0, 48)]);
        tone_amplitude(&out[12_000..], 130.81 * 12.0, SR)
            / tone_amplitude(&out[12_000..], 130.81, SR)
    };
    assert!(brightness(100.0) > brightness(0.0) * 4.0);
}

#[test]
fn tension_inharmonicity_stretches_the_upper_partials() {
    let eighth = |inharm: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::S_INHARM, inharm);
        set(&mut *p, te::EX_POSITION, 20.0);
        let out = left(&mut *p, 65_536, &[on(0, 45)]);
        let f1 = frequency_near(&out[4_096..], 110.0, SR);
        let f8 = frequency_near(&out[4_096..], f1 * 8.0, SR);
        (f1, f8)
    };
    let (f1, f8) = eighth(0.0);
    assert!(cents(f1, 110.0).abs() < 3.0, "{f1}");
    assert!(cents(f8, 880.0).abs() < 6.0, "harmonic: {f8}");
    let (f1b, f8b) = eighth(60.0);
    assert!(cents(f1b, 110.0).abs() < 8.0, "{f1b}");
    assert!(f8b > f1b * 8.0 * 1.003, "stretched: {f8b} from {f1b}");
}

#[test]
fn tension_exciter_position_notches_the_harmonics_at_that_point() {
    let render = |position: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::EX_POSITION, position);
        set(&mut *p, te::S_DAMPING, 90.0);
        left(&mut *p, 48_000, &[on(0, 45)])
    };
    let f = 110.0;
    let mid = render(50.0);
    let near = render(14.0);
    let ratio = |buf: &[f32]| {
        tone_amplitude(&buf[4_096..], 2.0 * f, SR) / tone_amplitude(&buf[4_096..], f, SR)
    };
    assert!(
        ratio(&mid) < ratio(&near) * 0.3,
        "{} vs {}",
        ratio(&mid),
        ratio(&near)
    );
}

#[test]
fn tension_exciters_respond_to_their_own_parameters() {
    let level = |exciter: usize, id: u32, value: f64| {
        let mut p = tension(exciter);
        set(&mut *p, id, value);
        rms(&left(&mut *p, 24_000, &[on(0, 57)])[..24_000])
    };
    assert!(
        level(te::PLECTRUM, te::PLECTRUM_PROTRUSION, 100.0)
            > level(te::PLECTRUM, te::PLECTRUM_PROTRUSION, 10.0) * 1.5
    );
    assert!(
        level(te::HAMMER, te::EX_VELOCITY, 100.0) > level(te::HAMMER, te::EX_VELOCITY, 20.0) * 1.5
    );
    assert!(level(te::BOW, te::EX_VELOCITY, 100.0) > level(te::BOW, te::EX_VELOCITY, 20.0) * 1.2);
    let brightness = |stiffness: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::HAMMER_STIFF, stiffness);
        let out = left(&mut *p, 48_000, &[on(0, 45)]);
        tone_amplitude(&out[4_096..], 110.0 * 7.0, SR) / tone_amplitude(&out[4_096..], 110.0, SR)
    };
    assert!(
        brightness(100.0) > brightness(5.0) * 1.15,
        "{} {}",
        brightness(100.0),
        brightness(5.0)
    );
    // The bouncing hammer strikes again; the plain one doesn't.
    let plain = {
        let mut p = tension(te::HAMMER);
        let out = left(&mut *p, 24_000, &[on(0, 57)]);
        rms(&out[12_000..24_000]) / rms(&out[..4_800])
    };
    let bouncing = {
        let mut p = tension(te::HAMMER_BOUNCE);
        let out = left(&mut *p, 24_000, &[on(0, 57)]);
        rms(&out[12_000..24_000]) / rms(&out[..4_800])
    };
    assert!(plain.is_finite() && bouncing.is_finite());
    assert!((plain - bouncing).abs() > 0.01, "{plain} {bouncing}");
}

#[test]
fn tension_damper_gates_with_the_key_and_is_silent_when_nothing_acts() {
    let render = |gated: f64, damper: f64, exciter: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::D_ON, damper);
        set(&mut *p, te::D_GATED, gated);
        set(&mut *p, te::EX_ON, exciter);
        set(&mut *p, te::S_DECAY, 6_000.0);
        left(&mut *p, 96_000, &[on(0, 57), off(48_000, 57)])
    };
    let gated = render(1.0, 1.0, 1.0);
    let always = render(0.0, 1.0, 1.0);
    // Held, the gated string rings on; always damped, it dies off fast.
    assert!(rms(&gated[24_000..48_000]) > rms(&always[24_000..48_000]) * 4.0);
    // Off, the damper bites: the gated one falls right after the key.
    assert!(rms(&gated[72_000..96_000]) < rms(&gated[24_000..48_000]) * 0.2);
    // With neither exciter nor damper, nothing moves the string.
    assert!(peak(&render(1.0, 0.0, 0.0)) < 1e-6);
    // With the exciter off, the damper alone sets the string going on release.
    let damper_only = render(1.0, 1.0, 0.0);
    assert!(peak(&damper_only[48_000..]) > 1e-3);
}

#[test]
fn tension_damper_bounces_above_half_and_lets_more_ring() {
    let decay_at = |damping: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::D_ON, 1.0);
        set(&mut *p, te::D_DAMPING, damping);
        set(&mut *p, te::S_DECAY, 6_000.0);
        set(&mut *p, te::S_RATIO, 0.0);
        set(&mut *p, te::D_MASS, 50.0);
        let out = left(&mut *p, 48_000, &[on(0, 57), off(4_800, 57)]);
        rms(&out[24_000..48_000]) / rms(&out[1_000..4_800])
    };
    let quarter = decay_at(25.0);
    let half = decay_at(50.0);
    let high = decay_at(100.0);
    assert!(
        half < quarter,
        "more damping is more loss up to half: {half} {quarter}"
    );
    assert!(
        high > half * 3.0,
        "above half it bounces and rings on: {high} {half}"
    );
}

#[test]
fn tension_body_size_lowers_the_resonance() {
    let spectrum = |size: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::B_ON, 1.0);
        set(&mut *p, te::B_TYPE, 1.0);
        set(&mut *p, te::B_SIZE, size);
        set(&mut *p, te::B_MIX, 100.0);
        set(&mut *p, te::B_DECAY, 80.0);
        let out = left(&mut *p, 48_000, &[on(0, 28)]);
        let f = 41.203;
        // The strongest of the first forty harmonics sits near the body's main resonance.
        let mut best = (0.0, 0.0);
        for k in 1..=40 {
            let hz = f * k as f64;
            let a = tone_amplitude(&out[4_096..], hz, SR);
            if a > best.1 {
                best = (hz, a);
            }
        }
        best.0
    };
    let (xs, xl) = (spectrum(0.0), spectrum(4.0));
    assert!(xs > xl * 1.2, "a bigger body resonates lower: {xs} {xl}");
}

#[test]
fn tension_filter_cuts_and_formant_vowels_differ() {
    let render = |filter_type: f64, res: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::F_ON, 1.0);
        set(&mut *p, te::F_TYPE, filter_type);
        set(&mut *p, te::F_FREQ, 500.0);
        set(&mut *p, te::F_RES, res);
        left(&mut *p, 24_000, &[on(0, 60)])
    };
    let plain = {
        let mut p = tension(te::HAMMER);
        left(&mut *p, 24_000, &[on(0, 60)])
    };
    let high = FIRST_NOTE * 8.0;
    assert!(tone_amplitude(&render(0.0, 0.0), high, SR) < tone_amplitude(&plain, high, SR) * 0.3);
    assert!(
        tone_amplitude(&render(1.0, 0.0), high, SR) < tone_amplitude(&render(0.0, 0.0), high, SR)
    );
    let vowels = (render(8.0, 0.0), render(8.0, 100.0));
    // The first vowel's first formant is the highest of the set, so moving through them changes it.
    let band = |buf: &[f32]| rms(buf).max(1e-9);
    assert!((band(&vowels.0) / band(&vowels.1) - 1.0).abs() > 0.05);
}

#[test]
fn tension_portamento_slides_and_proportional_is_slower_for_bigger_leaps() {
    let glide = |proportional: f64, to: u8| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::PO_ON, 1.0);
        set(&mut *p, te::PO_TIME, 200.0);
        set(&mut *p, te::PO_PROP, proportional);
        let out = left(&mut *p, 24_000, &[on(0, 60), off(4_800, 60), on(4_800, to)]);
        let early = frequency_near(
            &out[4_800..8_000],
            440.0 * 2f64.powf((to as f64 - 69.0) / 12.0),
            SR,
        );
        (early, out)
    };
    let (_, const_out) = glide(0.0, 72);
    let settled = frequency_near(&const_out[20_000..], 523.25, SR);
    assert!(cents(settled, 523.25).abs() < 8.0, "{settled}");
    // Constant time: the same time for an octave as for a fifth.
    let const_mid = frequency_near(&const_out[9_000..11_000], 400.0, SR);
    assert!(
        cents(const_mid, 523.25).abs() > 30.0,
        "still sliding {const_mid}"
    );
    let (_, prop_out) = glide(1.0, 84);
    let prop_mid = frequency_near(&prop_out[9_000..11_000], 500.0, SR);
    let c4 = 261.626;
    let progress_prop = cents(prop_mid, c4);
    let (_, const_again) = glide(0.0, 84);
    let const_mid_oct = frequency_near(&const_again[9_000..11_000], 700.0, SR);
    assert!(
        cents(const_mid_oct, c4) > progress_prop,
        "proportional is slower for a two-octave leap"
    );
}

#[test]
fn tension_tuning_error_stretch_and_unison_behave() {
    let furthest = |error: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::ERROR, error);
        let mut worst = 0.0f64;
        for key in [60u8, 62, 64, 65, 67, 69, 71, 72] {
            p.reset();
            let out = left(&mut *p, 24_576, &[on(0, key)]);
            let expected = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
            worst = worst.max(cents(frequency_near(&out[4_096..], expected, SR), expected).abs());
        }
        worst
    };
    assert!(furthest(0.0) < 3.0, "{}", furthest(0.0));
    assert!(
        furthest(100.0) > 6.0,
        "with Error notes go out of tune: {}",
        furthest(100.0)
    );

    let mut p = tension(te::HAMMER);
    set(&mut *p, te::UN_ON, 1.0);
    set(&mut *p, te::UN_VOICES, 0.0);
    set(&mut *p, te::UN_DETUNE, 100.0);
    set(&mut *p, te::UN_DELAY, 100.0);
    let out = left(&mut *p, 24_000, &[on(0, 60)]);
    // The second stacked voice comes in a Delay later.
    assert!(rms(&out[..2_400]) > 0.0);
    assert!(rms(&out[5_400..7_800]) > rms(&out[..2_400]) * 1.02);
}

#[test]
fn tension_priority_cuts_the_right_note() {
    let run = |priority: f64| {
        let mut p = tension(te::HAMMER);
        set(&mut *p, te::VOICES, 1.0); // two voices
        set(&mut *p, te::PRIORITY, priority);
        set(&mut *p, te::S_DECAY, 8_000.0);
        // C2, C#3 and B3: no line of one sits on a harmonic of another.
        let out = left(&mut *p, 48_000, &[on(0, 36), on(2_400, 49), on(4_800, 59)]);
        let pitch = |f: f64| tone_amplitude(&out[28_000..], f, SR);
        (pitch(65.406), pitch(138.59), pitch(246.94))
    };
    // High priority: the lowest note is the one cut.
    let (low, mid, high) = run(0.0);
    assert!(
        mid > low * 5.0 && high > low * 5.0,
        "High keeps the high ones: {low} {mid} {high}"
    );
    // Low priority: the new note, higher than everything held, is the one refused.
    let (low, mid, high) = run(1.0);
    assert!(
        low > high * 5.0 && mid > high * 5.0,
        "Low keeps the low ones: {low} {mid} {high}"
    );
}

// ---------------------------------------------------------------------------------- operator

fn operator() -> Box<dyn Processor> {
    let mut p = device("rlive.operator");
    set(&mut *p, op::G_ANTIALIAS, 0.0);
    set(&mut *p, op::G_TONE, 100.0);
    for base in op::OSC_BASE {
        set(&mut *p, base + op::O_RETRIG, 1.0);
        set(&mut *p, base + op::O_ENV + op::E_INITIAL, 0.0);
        set(&mut *p, base + op::O_ENV + op::E_SUSTAIN, 0.0);
    }
    p
}

fn enable_osc(p: &mut dyn Processor, osc: usize, level_db: f64, coarse: f64) {
    let base = op::OSC_BASE[osc];
    set(p, base + op::O_ON, 1.0);
    set(p, base + op::O_LEVEL, level_db);
    set(p, base + op::O_COARSE, coarse);
}

/// What the algorithm should produce, worked out by hand from the routing table.
fn algorithm_reference(
    algorithm: usize,
    levels: [f64; 4],
    ratios: [f64; 4],
    hz: f64,
    frames: usize,
) -> Vec<f32> {
    let table = op::ALGORITHMS[algorithm];
    let (mut phase, mut out) = ([0.0f64; 4], vec![0.0f32; frames]);
    for sample in out.iter_mut() {
        let mut values = [0.0f64; 4];
        for o in (0..4).rev() {
            let mut pm = 0.0;
            for (m, value) in values.iter().enumerate() {
                if table.modulators[o] & (1 << m) != 0 {
                    pm += value;
                }
            }
            values[o] =
                (2.0 * PI64 * phase[o] + pm * op::FM_INDEX_AT_FULL as f64).sin() * levels[o];
            phase[o] = (phase[o] + hz * ratios[o] / SR).fract();
        }
        let mut mix = 0.0;
        for (o, v) in values.iter().enumerate() {
            if table.carriers & (1 << o) != 0 {
                mix += v;
            }
        }
        *sample = mix as f32;
    }
    out
}

#[test]
fn operator_each_algorithm_routes_as_its_table_says() {
    for algorithm in 0..op::ALGORITHMS.len() {
        let levels_db = [-6.0, -18.0, -12.0, -24.0];
        let ratios = [1.0, 2.0, 3.0, 5.0];
        let mut p = operator();
        set(&mut *p, op::G_ALGORITHM, algorithm as f64);
        for o in 0..4 {
            enable_osc(&mut *p, o, levels_db[o], ratios[o]);
        }
        let out = left(&mut *p, 8_192, &[on(0, 45)]);
        let levels = levels_db.map(|db: f64| 10f64.powf(db / 20.0));
        let reference = algorithm_reference(algorithm, levels, ratios, 110.0, 8_192);
        // Volume is -6 dB and the note is panned centre.
        let volume = 10f64.powf(-6.0 / 20.0) as f32;
        let error: f32 = out[2_000..]
            .iter()
            .zip(&reference[2_000..])
            .map(|(a, b)| (a - b * volume).powi(2))
            .sum::<f32>()
            / 6_192.0;
        assert!(
            error.sqrt() < 0.04 * volume,
            "algorithm {algorithm}: {:?} {}",
            op::ALGORITHMS[algorithm],
            error.sqrt()
        );
    }
}

/// Bessel function of the first kind, by its series.
fn bessel_j(n: i32, x: f64) -> f64 {
    let mut sum = 0.0;
    for k in 0..30 {
        let term =
            (-1.0f64).powi(k) * (x / 2.0).powi(2 * k + n) / (factorial(k) * factorial(k + n));
        sum += term;
    }
    sum
}

fn factorial(n: i32) -> f64 {
    (1..=n).map(|i| i as f64).product()
}

#[test]
fn operator_sidebands_sit_at_bessel_amplitudes() {
    let carrier_hz = 440.0;
    let ratio = 3.0;
    let level = 0.5; // -6 dB
    let index = level * op::FM_INDEX_AT_FULL as f64;
    let mut p = operator();
    set(&mut *p, op::G_ALGORITHM, 7.0); // B modulates A, D modulates C; C and D off
    enable_osc(&mut *p, 0, 0.0, 1.0);
    enable_osc(&mut *p, 1, 20.0 * level.log10(), ratio);
    let out = left(&mut *p, 48_000, &[on(0, 69)]);
    let volume = 10f64.powf(-6.0 / 20.0);
    let measure = |hz: f64| tone_amplitude(&out[4_096..], hz, SR) / volume;
    for n in 0..4 {
        let expected = bessel_j(n, index).abs();
        let upper = measure(carrier_hz + n as f64 * carrier_hz * ratio);
        assert!(
            (upper - expected).abs() < 0.02 + expected * 0.05,
            "J{n} {index}: {upper} vs {expected}"
        );
    }
    let lower = carrier_hz * ratio - carrier_hz;
    assert!(
        (measure(lower) - bessel_j(1, index).abs()).abs() < 0.03,
        "{}",
        measure(lower)
    );
}

#[test]
fn operator_feedback_applies_only_to_an_oscillator_with_no_modulator() {
    let harmonic_content = |feedback: f64, modulated: bool| {
        let mut p = operator();
        set(&mut *p, op::OSC_BASE[0] + op::O_FEEDBACK, feedback);
        set(&mut *p, op::G_ALGORITHM, 10.0);
        if modulated {
            set(&mut *p, op::G_ALGORITHM, 7.0);
            enable_osc(&mut *p, 1, -70.0, 1.0);
        }
        let out = left(&mut *p, 24_000, &[on(0, 57)]);
        tone_amplitude(&out[4_096..], 440.0, SR) / tone_amplitude(&out[4_096..], 220.0, SR)
    };
    assert!(harmonic_content(0.0, false) < 1e-3);
    assert!(harmonic_content(100.0, false) > 0.05);
    assert!(
        harmonic_content(100.0, true) < 1e-3,
        "a modulated oscillator doesn't feed back"
    );
}

/// Publishes the view while running, one block at a time.
fn view_run(
    p: &mut dyn Processor,
    channel: &Arc<ViewChannel>,
    transport: &mut TransportInfo,
    frames: usize,
    events: &[MidiEvent],
) -> Vec<f32> {
    let reader = channel.attach();
    let mut out = Vec::new();
    let mut at = 0;
    let (mut eo, mut po) = (
        EventQueue::with_capacity(16),
        ParamOutQueue::with_capacity(16),
    );
    while at < frames {
        let n = BLOCK.min(frames - at);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        let block: Vec<MidiEvent> = events
            .iter()
            .filter(|e| (e.offset as usize) >= at && (e.offset as usize) < at + n)
            .map(|e| MidiEvent::new(e.offset - at as u32, e.message))
            .collect();
        let writer = ViewWriter::new(channel);
        p.process(&mut ProcessContext {
            audio: AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            sidechain: None,
            aux_out: &mut [],
            events: &block,
            events_out: &mut eo,
            params: &[],
            transport,
            param_out: &mut po,
            mod_out: &mut [],
            view: &writer,
        });
        transport.tick += n as f64 * transport.ticks_per_sample();
        let mut view = [0.0f32; op::VIEW_LEN];
        reader.read(op::VIEW, &mut view);
        out.push(view[0]);
        at += n;
    }
    out
}

fn level_track(
    events: &[MidiEvent],
    frames: usize,
    setup: impl Fn(&mut dyn Processor),
) -> Vec<f32> {
    let mut p = operator();
    setup(&mut *p);
    let channel = Arc::new(ViewChannel::new(op::VIEW_LEN));
    let mut transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        ..Default::default()
    };
    view_run(&mut *p, &channel, &mut transport, frames, events)
}

#[test]
fn operator_loop_mode_repeats_from_the_sustain_level_at_the_loop_rate() {
    let env = op::OSC_BASE[0] + op::O_ENV;
    let track = level_track(&[on(0, 60)], 24_000, |p| {
        set(p, env + op::E_INITIAL, -70.0);
        set(p, env + op::E_ATTACK, 2.0);
        set(p, env + op::E_DECAY, 40.0);
        set(p, env + op::E_SUSTAIN, -30.0);
        set(p, env + op::E_LOOP, op::ENV_LOOP_LOOP as f64);
        set(p, env + op::E_LOOP_TIME, 40.0);
    });
    // Attack, decay then 40 ms back to the initial level: about 82 ms a cycle, ten blocks.
    let mut rises = 0;
    for w in track.windows(2) {
        if w[1] > 0.2 && w[0] <= 0.2 {
            rises += 1;
        }
    }
    assert!((4..=7).contains(&rises), "{rises}: {:?}", &track[..20]);
}

#[test]
fn operator_beat_mode_repeats_after_the_beat_and_sync_lands_on_sixteenths() {
    let env = op::OSC_BASE[0] + op::O_ENV;
    let run = |mode: usize, start_at: u32| {
        let mut p = operator();
        set(&mut *p, env + op::E_INITIAL, -70.0);
        set(&mut *p, env + op::E_ATTACK, 1.0);
        set(&mut *p, env + op::E_DECAY, 40.0);
        set(&mut *p, env + op::E_SUSTAIN, -70.0);
        set(&mut *p, env + op::E_LOOP, mode as f64);
        set(&mut *p, env + op::E_REPEAT, 9.0); // 1/8 note
        let channel = Arc::new(ViewChannel::new(op::VIEW_LEN));
        let mut transport = TransportInfo {
            playing: true,
            sample_rate: SR,
            tempo: 120.0,
            ..Default::default()
        };
        // 1/8 note at 120 is 0.25 s, 12 000 samples.
        view_run(
            &mut *p,
            &channel,
            &mut transport,
            40_000,
            &[on(start_at, 60)],
        )
    };
    let block_ms = BLOCK as f32;
    let rises = |t: &[f32]| -> Vec<f32> {
        let mut v = Vec::new();
        for (i, w) in t.windows(2).enumerate() {
            if w[1] > 0.5 && w[0] <= 0.5 {
                v.push((i as f32 + 1.0) * block_ms);
            }
        }
        v
    };
    let beat = run(op::ENV_LOOP_BEAT, 1_000);
    let hits = rises(&beat);
    assert!(hits.len() >= 2, "{hits:?}");
    let gap = hits[1] - hits[0];
    assert!((gap - 12_000.0).abs() <= block_ms * 1.5, "{hits:?}");
    // Off the grid in Beat mode it stays off the grid; Sync's first repeat snaps to a 16th.
    let sync = run(op::ENV_LOOP_SYNC, 1_000);
    let hits = rises(&sync);
    let sixteenth = 6_000.0;
    let first = hits.iter().find(|h| **h > 1_500.0).copied().unwrap();
    let off_grid = (first / sixteenth - (first / sixteenth).round()).abs() * sixteenth;
    assert!(off_grid <= block_ms * 1.5, "{hits:?} {off_grid}");
}

#[test]
fn operator_trigger_mode_ignores_note_off() {
    let env = op::OSC_BASE[0] + op::O_ENV;
    let render = |trigger: bool, off_at: Option<u32>| {
        let mut p = operator();
        set(&mut *p, env + op::E_INITIAL, -70.0);
        set(&mut *p, env + op::E_DECAY, 2_000.0);
        set(&mut *p, env + op::E_SUSTAIN, -70.0);
        set(&mut *p, env + op::E_RELEASE, 5.0);
        if trigger {
            set(&mut *p, env + op::E_LOOP, op::ENV_LOOP_TRIGGER as f64);
        }
        let mut events = vec![on(0, 60)];
        events.extend(off_at.map(|t| off(t, 60)));
        left(&mut *p, 24_000, &events)
    };
    assert!(
        rms(&render(true, Some(4_800))[9_600..14_400]) > 0.05,
        "keeps sounding"
    );
    assert!(
        rms(&render(false, Some(4_800))[9_600..14_400]) < 0.01,
        "release cuts it"
    );
}

#[test]
fn operator_slopes_bend_filter_and_pitch_envelopes_and_time_scales_every_rate() {
    let env = op::F_ENV;
    let track_at = |slope: f64, time: f64, key: u8, time_key: f64, frames: usize| {
        let mut p = device("rlive.operator");
        set(&mut *p, op::F_ON, 1.0);
        set(&mut *p, env + op::E_INITIAL, 0.0);
        set(&mut *p, env + op::E_PEAK, 100.0);
        set(&mut *p, env + op::E_ATTACK, 20.0);
        set(&mut *p, env + op::E_A_SLOPE, slope);
        set(&mut *p, env + op::E_SUSTAIN, 0.0);
        set(&mut *p, env + op::E_DECAY, 200.0);
        set(&mut *p, op::G_TIME, time);
        set(&mut *p, op::G_TIME_KEY, time_key);
        let channel = Arc::new(ViewChannel::new(op::VIEW_LEN));
        let transport = TransportInfo::default();
        let reader = channel.attach();
        let mut track = Vec::new();
        for i in 0..frames {
            let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
            let (mut eo, mut po) = (
                EventQueue::with_capacity(16),
                ParamOutQueue::with_capacity(16),
            );
            let on_event = [on(0, key)];
            let events: &[MidiEvent] = if i == 0 { &on_event } else { &[] };
            let writer = ViewWriter::new(&channel);
            p.process(&mut ProcessContext {
                audio: AudioBuffer {
                    left: &mut l,
                    right: &mut r,
                },
                sidechain: None,
                aux_out: &mut [],
                events,
                events_out: &mut eo,
                params: &[],
                transport: &transport,
                param_out: &mut po,
                mod_out: &mut [],
                view: &writer,
            });
            let mut view = [0.0f32; op::VIEW_LEN];
            reader.read(op::VIEW, &mut view);
            track.push(view[4]);
        }
        track
    };
    // Halfway through the attack: positive slope moves early, negative late, zero is linear.
    let mid = 7usize;
    let fast = track_at(100.0, 0.0, 60, 0.0, 20)[mid];
    let linear = track_at(0.0, 0.0, 60, 0.0, 20)[mid];
    let late = track_at(-100.0, 0.0, 60, 0.0, 20)[mid];
    assert!(
        fast > linear * 1.2 && linear > late * 1.2,
        "{fast} {linear} {late}"
    );
    // A longer Time stretches the envelope; a higher key with Time < Key shortens it.
    let slow = track_at(0.0, 100.0, 60, 0.0, 20)[mid];
    assert!(slow < linear * 0.5, "{slow} {linear}");
    let high = track_at(0.0, 0.0, 84, 100.0, 20)[mid];
    assert!(high > linear * 1.5, "{high} {linear}");
}

#[test]
fn operator_built_in_waves_have_the_named_harmonics() {
    let spectrum = |wave: usize| {
        let mut p = operator();
        set(&mut *p, op::OSC_BASE[0] + op::O_WAVE, wave as f64);
        let out = left(&mut *p, 48_000, &[on(0, 33)]);
        let f = 55.0;
        move |k: usize| tone_amplitude(&out[4_096..], f * k as f64, SR)
    };
    let saw = spectrum(waves::W_SAW_8);
    let first = saw(1);
    for k in 2..=8 {
        let expected = first / k as f64;
        assert!(
            (saw(k) - expected).abs() < expected * 0.15 + 2e-4,
            "saw {k}"
        );
    }
    assert!(
        saw(12) < first * 0.01,
        "saw 8 stops at 8 harmonics: {}",
        saw(12)
    );
    let square = spectrum(waves::W_SQUARE_6);
    let first = square(1);
    assert!((square(3) - first / 3.0).abs() < first / 3.0 * 0.15 + 2e-4);
    assert!(square(2) < first * 0.02, "no even harmonics");
    assert!(square(11) > first * 0.02, "the sixth odd harmonic");
    assert!(square(13) < first * 0.02, "stops after six");
}

#[test]
fn operator_user_harmonics_repeat_and_normalize() {
    let mut partials = vec![0.0; waves::PARTIALS];
    partials[0] = 1.0;
    partials[2] = 0.5;
    let base = waves::UserWave {
        count: 16,
        repeat: 0,
        normalize: true,
        restrict: 0,
        partials: partials.clone(),
    };
    let render = |wave: &waves::UserWave| {
        let state = waves::WavesState {
            ops: [
                wave.clone(),
                Default::default(),
                Default::default(),
                Default::default(),
            ],
        };
        let info = find("rlive.operator").unwrap();
        let mut p = (info.create)(&DeviceInit {
            state: Some(state.to_text()),
            ..Default::default()
        });
        p.activate(SR, BLOCK);
        set(&mut *p, op::G_ANTIALIAS, 0.0);
        set(&mut *p, op::G_TONE, 100.0);
        set(&mut *p, op::OSC_BASE[0] + op::O_RETRIG, 1.0);
        set(&mut *p, op::OSC_BASE[0] + op::O_ENV + op::E_INITIAL, 0.0);
        set(&mut *p, op::OSC_BASE[0] + op::O_WAVE, waves::W_USER as f64);
        left(&mut *p, 48_000, &[on(0, 33)])
    };
    let plain = render(&base);
    let f = 55.0;
    let a = |buf: &[f32], k: usize| tone_amplitude(&buf[4_096..], f * k as f64, SR);
    assert!((a(&plain, 3) - a(&plain, 1) * 0.5).abs() < a(&plain, 1) * 0.05);
    assert!(a(&plain, 5) < a(&plain, 1) * 0.01);
    assert!(a(&plain, 17) < a(&plain, 1) * 0.01, "Repeat off truncates");
    let repeated = waves::UserWave {
        repeat: 2,
        ..base.clone()
    };
    let rendered = render(&repeated);
    assert!(
        a(&rendered, 17) > a(&rendered, 1) * 0.1,
        "copy at harmonic 17"
    );
    assert!(a(&rendered, 17) < a(&rendered, 1) * 0.9, "fades");
    // Without Normalize more harmonics add level.
    let mut loud = base.clone();
    loud.normalize = false;
    loud.partials[0] = 1.0;
    loud.partials[1] = 1.0;
    loud.partials[2] = 1.0;
    let mut quiet = loud.clone();
    quiet.normalize = true;
    assert!(peak(&render(&loud)) > peak(&render(&quiet)) * 1.5);
}

#[test]
fn operator_ams_round_trips_and_reads_a_plain_wave() {
    let mut partials = vec![0.0; waves::PARTIALS];
    partials[0] = 1.0;
    partials[3] = 0.25;
    partials[15] = 0.75;
    let wave = waves::UserWave {
        count: 32,
        repeat: 3,
        normalize: false,
        restrict: 2,
        partials,
    };
    let file = waves::to_ams(&wave);
    assert_eq!(waves::from_ams(&file).unwrap(), wave);
    assert_eq!(&file[0..4], b"RIFF");

    // A one-cycle plain WAVE of a square wave imports as its harmonics.
    let frame: Vec<f32> = (0..1024)
        .map(|i| if i < 512 { 0.8 } else { -0.8 })
        .collect();
    let mut plain = Vec::new();
    let data = frame.len() * 4;
    plain.extend_from_slice(b"RIFF");
    plain.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    plain.extend_from_slice(b"WAVEfmt ");
    plain.extend_from_slice(&16u32.to_le_bytes());
    plain.extend_from_slice(&3u16.to_le_bytes());
    plain.extend_from_slice(&1u16.to_le_bytes());
    plain.extend_from_slice(&44_100u32.to_le_bytes());
    plain.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    plain.extend_from_slice(&4u16.to_le_bytes());
    plain.extend_from_slice(&32u16.to_le_bytes());
    plain.extend_from_slice(b"data");
    plain.extend_from_slice(&(data as u32).to_le_bytes());
    for s in &frame {
        plain.extend_from_slice(&s.to_le_bytes());
    }
    let imported = waves::from_ams(&plain).unwrap();
    assert!((imported.partials[2] - 1.0 / 3.0).abs() < 0.02);
    assert!(imported.partials[1] < 0.02);
    assert!(waves::from_ams(b"not a wav").is_none());
}

#[test]
fn operator_antialias_keeps_fm_folding_down() {
    let render = |antialias: f64, tone: f64| {
        let mut p = device("rlive.operator");
        set(&mut *p, op::G_ALGORITHM, 0.0);
        set(&mut *p, op::G_ANTIALIAS, antialias);
        set(&mut *p, op::G_TONE, tone);
        set(&mut *p, op::OSC_BASE[0] + op::O_ENV + op::E_INITIAL, 0.0);
        set(&mut *p, op::OSC_BASE[0] + op::O_ENV + op::E_SUSTAIN, 0.0);
        set(&mut *p, op::G_ALGORITHM, 7.0);
        enable_osc(&mut *p, 1, -6.0, 7.0);
        set(&mut *p, op::OSC_BASE[1] + op::O_ENV + op::E_INITIAL, 0.0);
        set(&mut *p, op::OSC_BASE[1] + op::O_ENV + op::E_SUSTAIN, 0.0);
        left(&mut *p, 96_000, &[on(0, 96)])
    };
    // The only lines belonging to the note are carrier +/- multiples of seven times 2093 Hz;
    // anything else audible has folded back.
    let f = 2093.005f64;
    let mut lines: Vec<f64> = Vec::new();
    for k in -8..8 {
        lines.push((f + 7.0 * f * k as f64).abs());
    }
    let alias_power = |buf: &[f32]| power_outside(&buf[8_192..], &lines, 40.0);
    let on_ = render(1.0, 100.0);
    let off_ = render(0.0, 100.0);
    assert!(
        alias_power(&on_) < alias_power(&off_) * 0.25,
        "{} vs {}",
        alias_power(&on_),
        alias_power(&off_)
    );
    let duller = render(1.0, 10.0);
    assert!(alias_power(&duller) <= alias_power(&on_) * 1.2);
}

#[test]
fn operator_saw_wave_stays_clean_at_high_pitch() {
    let render = |antialias: f64| {
        let mut p = device("rlive.operator");
        set(&mut *p, op::G_ANTIALIAS, antialias);
        set(&mut *p, op::G_TONE, 100.0);
        set(
            &mut *p,
            op::OSC_BASE[0] + op::O_WAVE,
            waves::W_SAW_64 as f64,
        );
        set(&mut *p, op::OSC_BASE[0] + op::O_ENV + op::E_INITIAL, 0.0);
        set(&mut *p, op::OSC_BASE[0] + op::O_ENV + op::E_SUSTAIN, 0.0);
        left(&mut *p, 96_000, &[on(0, 96)])
    };
    let f = 2093.005f64;
    let lines: Vec<f64> = (1..=64).map(|k| k as f64 * f).collect();
    let measure = |buf: &[f32]| power_outside(&buf[8_192..], &lines, 40.0).sqrt();
    assert!(measure(&render(1.0)) < 0.003, "{}", measure(&render(1.0)));
    assert!(
        measure(&render(1.0)) < measure(&render(0.0)) * 0.5,
        "off aliases more"
    );
}

#[test]
fn operator_lfo_reaches_audio_rate_and_pitch_envelope_follows_its_amount() {
    let mut p = operator();
    set(&mut *p, op::L_ON, 1.0);
    set(&mut *p, op::L_WAVE, 0.0);
    set(&mut *p, op::L_RANGE, 1.0);
    set(&mut *p, op::L_RATE, 100.0);
    set(&mut *p, op::L_AMT_A, 100.0);
    set(&mut *p, op::L_ENV + op::E_DECAY, 60_000.0);
    let out = left(&mut *p, 48_000, &[on(0, 60)]);
    // 12 kHz sidebands around the carrier mean frequency modulation at audio rate.
    let side = tone_amplitude(&out[4_096..], 12_000.0 - FIRST_NOTE, SR)
        + tone_amplitude(&out[4_096..], 12_000.0 + FIRST_NOTE, SR);
    assert!(side > 2e-3, "{side}");

    let pitched = |amount: f64| {
        let mut p = operator();
        set(&mut *p, op::P_ON, 1.0);
        set(&mut *p, op::P_AMOUNT, amount);
        let env = op::P_ENV;
        set(&mut *p, env + op::E_INITIAL, 12.0);
        set(&mut *p, env + op::E_PEAK, 12.0);
        set(&mut *p, env + op::E_SUSTAIN, 12.0);
        let out = left(&mut *p, 32_768, &[on(0, 60)]);
        frequency_near(&out[4_096..], FIRST_NOTE * 2.0, SR)
    };
    assert!(cents(pitched(100.0), FIRST_NOTE * 2.0).abs() < 3.0);
    let inverted = {
        let mut p = operator();
        set(&mut *p, op::P_ON, 1.0);
        set(&mut *p, op::P_AMOUNT, -100.0);
        let env = op::P_ENV;
        set(&mut *p, env + op::E_INITIAL, 12.0);
        set(&mut *p, env + op::E_PEAK, 12.0);
        set(&mut *p, env + op::E_SUSTAIN, 12.0);
        let out = left(&mut *p, 32_768, &[on(0, 60)]);
        frequency_near(&out[4_096..], FIRST_NOTE * 0.5, SR)
    };
    assert!(
        cents(inverted, FIRST_NOTE * 0.5).abs() < 3.0,
        "-100% inverts"
    );
}

#[test]
fn operator_fixed_frequency_ignores_the_key_and_quantised_velocity_jumps() {
    let render = |key: u8, velocity: f32| {
        let mut p = operator();
        let base = op::OSC_BASE[0];
        set(&mut *p, base + op::O_FIXED, 1.0);
        set(&mut *p, base + op::O_FREQ, 500.0);
        let out = left(&mut *p, 32_768, &[on_vel(0, key, velocity)]);
        frequency_near(&out[4_096..], 500.0, SR)
    };
    assert!(cents(render(40, 1.0), 500.0).abs() < 2.0);
    assert!(cents(render(90, 1.0), 500.0).abs() < 2.0);

    let velocity_ratio = |quantise: f64, velocity: f32| {
        let mut p = operator();
        let base = op::OSC_BASE[0];
        set(&mut *p, base + op::O_COARSE, 2.0);
        set(&mut *p, base + op::O_FREQ_VEL, -60.0);
        set(&mut *p, base + op::O_FREQ_Q, quantise);
        let out = left(&mut *p, 65_536, &[on_vel(0, 60, velocity)]);
        let mut best = (0.0, 0.0);
        let mut f = 100.0f64;
        while f < 900.0 {
            let a = tone_amplitude(&out[4_096..], f, SR);
            if a > best.1 {
                best = (f, a);
            }
            f += 1.0;
        }
        best.0
    };
    let q = velocity_ratio(1.0, 0.5);
    let ratio = q / FIRST_NOTE;
    assert!(
        (ratio - ratio.round()).abs() < 0.02
            || (ratio - 1.5).abs() < 0.02
            || (ratio - 0.5).abs() < 0.02,
        "whole-number: {ratio}"
    );
    let free = velocity_ratio(0.0, 0.5);
    let ratio_free = free / FIRST_NOTE;
    assert!((ratio_free - 1.4).abs() < 0.05, "unquantised: {ratio_free}");
}

#[test]
fn operator_filter_key_tracking_envelope_range_and_drive_work() {
    let cutoff_amp = |freq: f64, key_track: f64, key: u8, env: f64| {
        let mut p = operator();
        set(&mut *p, op::F_ON, 1.0);
        set(&mut *p, op::F_TYPE, 0.0);
        set(&mut *p, op::F_SLOPE, 1.0);
        set(&mut *p, op::F_RES, 0.0);
        set(&mut *p, op::F_FREQ, freq);
        set(&mut *p, op::F_FREQ_KEY, key_track);
        set(&mut *p, op::F_ENV_AMT, env);
        let env_base = op::F_ENV;
        set(&mut *p, env_base + op::E_SUSTAIN, 100.0);
        set(
            &mut *p,
            op::OSC_BASE[0] + op::O_WAVE,
            waves::W_SAW_64 as f64,
        );
        left(&mut *p, 48_000, &[on(0, key)])
    };
    let f = 440.0 * 2f64.powf((60.0 - 69.0) / 12.0);
    let amp = |buf: &[f32], harmonic: usize, key_f: f64| {
        tone_amplitude(&buf[8_192..], key_f * harmonic as f64, SR)
    };
    let tracked = cutoff_amp(500.0, 100.0, 72, 0.0);
    let untracked = cutoff_amp(500.0, 0.0, 72, 0.0);
    assert!(
        amp(&tracked, 2, f * 2.0) > amp(&untracked, 2, f * 2.0) * 1.5,
        "Freq < Key opens"
    );
    // 100% Envelope opens the filter by about 9 octaves: from 50 Hz past the top of the audio band.
    let closed = cutoff_amp(50.0, 0.0, 60, 0.0);
    let open = cutoff_amp(50.0, 0.0, 60, 100.0);
    assert!(
        amp(&open, 3, f) > amp(&closed, 3, f) * 10.0,
        "{} {}",
        amp(&open, 3, f),
        amp(&closed, 3, f)
    );
}

#[test]
fn operator_spread_widens_the_note_and_is_decided_at_note_on() {
    let mut p = operator();
    set(&mut *p, op::P_SPREAD, 100.0);
    let (l, r) = play(&mut *p, 65_536, &[on(0, 60)]);
    let left_hz = frequency_near(&l[8_192..], FIRST_NOTE * 0.98, SR);
    let right_hz = frequency_near(&r[8_192..], FIRST_NOTE * 1.02, SR);
    assert!(left_hz < right_hz, "{left_hz} {right_hz}");
    assert!(left_hz < FIRST_NOTE && right_hz > FIRST_NOTE);
}

#[test]
fn operator_transpose_is_live_and_voices_one_plays_legato() {
    let mut p = operator();
    set(&mut *p, op::P_TRANSPOSE, 12.0);
    let out = left(&mut *p, 32_768, &[on(0, 48)]);
    assert!(cents(frequency_near(&out[4_096..], FIRST_NOTE, SR), FIRST_NOTE).abs() < 3.0);

    let mut mono = operator();
    set(&mut *mono, op::G_VOICES, 1.0);
    set(
        &mut *mono,
        op::OSC_BASE[0] + op::O_ENV + op::E_INITIAL,
        -70.0,
    );
    set(&mut *mono, op::OSC_BASE[0] + op::O_ENV + op::E_ATTACK, 50.0);
    let out = left(&mut *mono, 24_000, &[on(0, 60), on(7_200, 72)]);
    let hz = frequency_near(&out[12_000..], FIRST_NOTE * 2.0, SR);
    assert!(cents(hz, FIRST_NOTE * 2.0).abs() < 3.0);
    // Legato: the envelope doesn't restart, so the level never dips at the change.
    let before = rms(&out[6_000..7_200]);
    let after = rms(&out[7_400..8_400]);
    assert!(after > before * 0.8, "{before} {after}");
}

// ---------------------------------------------------------------- what all four must do

fn all_devices() -> Vec<(&'static str, Box<dyn Processor>)> {
    vec![
        ("operator", operator()),
        ("electric", device("rlive.electric")),
        ("tension", tension(te::HAMMER)),
        ("collision", collision(co::BEAM)),
    ]
}

#[test]
fn every_device_cuts_off_a_stolen_voice_without_a_click() {
    for (name, mut p) in all_devices() {
        let param = match name {
            "operator" => op::G_VOICES,
            "electric" => el::VOICES,
            "tension" => te::VOICES,
            _ => co::VOICES,
        };
        // Two voices, so a third note has to take one over.
        let value = if name == "operator" { 2.0 } else { 1.0 };
        p.set_param(param, value);
        let steal_at = 12_000u32;
        let out = left(
            &mut *p,
            24_000,
            &[on(0, 60), on(4_800, 64), on(steal_at, 67)],
        );
        // The oldest note is the one that went.
        let before = tone_amplitude(&out[9_000..12_000], FIRST_NOTE, SR);
        let after = tone_amplitude(&out[16_000..22_000], FIRST_NOTE, SR);
        assert!(
            after < before * 0.25,
            "{name}: the stolen note rings on: {before} {after}"
        );
        let around = (steal_at as usize + 2_000).min(out.len());
        let silence_start = steal_at as usize;
        assert!(peak(&out[silence_start..around]) > 1e-4, "{name} sounds");
        let neighbours: Vec<f32> = out[silence_start..silence_start + 2_048]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .collect();
        let big = neighbours.iter().cloned().fold(0.0f32, f32::max);
        assert!(big < 0.5, "{name} clicks at steal: {big}");
    }
}

#[test]
fn every_device_bends_with_per_note_pitch_and_note_ids_keep_notes_apart() {
    for (name, mut p) in all_devices() {
        if name == "operator" {
            p.set_param(op::G_PER_NOTE_PB, 1.0);
        }
        let note_id = 7;
        let start = MidiEvent::new(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id,
                tuning: 0.0,
            },
        );
        let bend = MidiEvent::new(
            1_024,
            MidiMessage::NoteExpr {
                channel: 0,
                key: 60,
                note_id,
                kind: ExprKind::Pitch,
                value: 1.0,
            },
        );
        let other = MidiEvent::new(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                key: 76,
                velocity: 1.0,
                note_id: 9,
                tuning: 0.0,
            },
        );
        let out = left(&mut *p, 32_768, &[start, other, bend]);
        let target = FIRST_NOTE * 2f64.powf(1.0 / 12.0);
        let hz = frequency_near(&out[4_096..], target, SR);
        // Whatever follows the note's own pitch moves by exactly the semitone it was sent.
        assert!(cents(hz, target).abs() < 8.0, "{name}: {hz}");
        // The other held note, with its own id, is untouched.
        let e5 = 659.255;
        assert!(
            cents(frequency_near(&out[4_096..], e5, SR), e5).abs() < 8.0,
            "{name} other note"
        );
    }
}

#[test]
fn every_device_is_silent_after_reset_and_plays_again() {
    for (name, mut p) in all_devices() {
        let out = left(&mut *p, 4_096, &[on(0, 60)]);
        assert!(peak(&out) > 1e-4, "{name}");
        p.reset();
        // As with bypass: voices are forgotten, and the key coming up later finds nothing to do.
        let quiet = left(&mut *p, 2_048, &[off(1_024, 60)]);
        assert!(peak(&quiet) < 1e-9, "{name} rings after reset");
        let again = left(&mut *p, 4_096, &[on(0, 60)]);
        assert!(peak(&again) > 1e-4, "{name} plays again");
    }
}

#[test]
fn all_notes_off_lets_every_voice_finish() {
    for (name, mut p) in all_devices() {
        let all_off = MidiEvent::new(2_400, MidiMessage::AllNotesOff);
        let out = left(
            &mut *p,
            960_000,
            &[on(0, 60), on(0, 64), on(0, 67), all_off],
        );
        let tail = peak(&out[out.len() - 2_048..]);
        assert!(tail < 1e-3, "{name} hangs after all notes off: {tail}");
    }
}

#[test]
fn every_device_stays_bounded_and_finite_at_extreme_settings() {
    for (id, params) in [
        (
            "rlive.operator",
            rlive_devices::find("rlive.operator").unwrap(),
        ),
        (
            "rlive.electric",
            rlive_devices::find("rlive.electric").unwrap(),
        ),
        (
            "rlive.tension",
            rlive_devices::find("rlive.tension").unwrap(),
        ),
        (
            "rlive.collision",
            rlive_devices::find("rlive.collision").unwrap(),
        ),
    ] {
        let info = params;
        let desc = (info.desc)();
        for pass in 0..3 {
            let mut p = device(id);
            let mut rng = rlive_devices::dsp::util::Rng::new(7 + pass);
            for param in &desc.params {
                let value = if pass == 0 {
                    param.max
                } else if pass == 1 {
                    param.min
                } else if rng.unipolar() > 0.5 {
                    param.max
                } else {
                    param.min
                };
                p.set_param(param.id, value);
            }
            // Bombard it: fast notes, some held across blocks, at both ends of the keyboard.
            let mut events = Vec::new();
            for i in 0..40u32 {
                let key = 21 + (i * 7 % 88) as u8;
                events.push(on(i * 1_200, key));
                if i % 3 != 0 {
                    events.push(off(i * 1_200 + 900, key));
                }
            }
            events.sort_by_key(|e| e.offset);
            let (l, r) = play(&mut *p, 48_000, &events);
            for (name, buf) in [("left", &l), ("right", &r)] {
                assert!(buf.iter().all(|s| s.is_finite()), "{id} {name} not finite");
                assert!(peak(buf) <= 1.01, "{id} {name} peak {}", peak(buf));
            }
        }
    }
}

#[test]
fn every_device_survives_automation_of_every_parameter_at_offsets() {
    for id in [
        "rlive.operator",
        "rlive.electric",
        "rlive.tension",
        "rlive.collision",
    ] {
        let info = find(id).unwrap();
        let desc = (info.desc)();
        let mut p = device(id);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let notes = [on(0, 60), on(4, 67)];
        h.process(&mut *p, &mut l, &mut r, &notes, &[]);
        for (i, param) in desc.params.iter().enumerate() {
            let changes = [ParamEvent {
                offset: (i as u32) % BLOCK as u32,
                id: ParamId(param.id),
                value: param.from_normalized(if i % 2 == 0 { 0.8 } else { 0.1 }),
            }];
            let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
            assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &[], &changes));
            assert!(
                l.iter().chain(&r).all(|s| s.is_finite()),
                "{id} {}",
                param.name
            );
        }
    }
}

#[test]
fn latency_is_reported_and_small_and_tail_covers_the_release() {
    let p = {
        let mut p = device("rlive.operator");
        set(&mut *p, op::G_ANTIALIAS, 1.0);
        p
    };
    assert!(p.latency() > 0 && p.latency() < 512, "{}", p.latency());
    let q = operator();
    assert_eq!(q.latency(), 0, "antialias off is zero latency");
    let electric = device("rlive.electric");
    assert!(electric.latency() > 0 && electric.latency() < 512);
    assert_eq!(device("rlive.tension").latency(), 0);
    assert_eq!(device("rlive.collision").latency(), 0);
    for id in [
        "rlive.operator",
        "rlive.electric",
        "rlive.tension",
        "rlive.collision",
    ] {
        let p = device(id);
        assert!(
            matches!(p.tail(), rlive_core::Tail::Frames(n) if n > 0),
            "{id}"
        );
    }
}

/// Prints the CPU cost of a voice. `cargo test -p rlive-devices --test b12 cpu_per_voice -- --ignored --nocapture`
#[test]
#[ignore = "a measurement, not a check"]
fn cpu_per_voice() {
    let cases: [(&str, Box<dyn Processor>); 4] = [
        ("operator (4 sine, antialias on)", {
            let mut p = device("rlive.operator");
            set(&mut *p, op::G_ALGORITHM, 0.0);
            for o in 1..4 {
                enable_osc(&mut *p, o, -18.0, (o + 1) as f64);
            }
            for base in op::OSC_BASE {
                set(&mut *p, base + op::O_ENV + op::E_INITIAL, 0.0);
                set(&mut *p, base + op::O_ENV + op::E_SUSTAIN, 0.0);
            }
            p
        }),
        ("electric", device("rlive.electric")),
        ("tension (hammer)", tension(te::HAMMER)),
        ("collision (beam mid quality)", collision(co::BEAM)),
    ];
    for (name, mut p) in cases {
        let notes: Vec<MidiEvent> = (0..8).map(|i| on(0, 48 + i * 3)).collect();
        let mut h = Harness::new(SR);
        let mut total = std::time::Duration::ZERO;
        let frames = 48_000 * 2;
        let mut first = true;
        let mut at = 0;
        while at < frames {
            let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
            let events: &[MidiEvent] = if first { &notes } else { &[] };
            let t = std::time::Instant::now();
            h.process(&mut *p, &mut l, &mut r, events, &[]);
            total += t.elapsed();
            at += BLOCK;
            first = false;
        }
        let seconds = at as f64 / SR;
        println!(
            "{name}: {:.2}% of one core per voice",
            100.0 * total.as_secs_f64() / seconds / 8.0
        );
    }
}

#[test]
fn every_device_is_in_tune_at_other_sample_rates() {
    for rate in [44_100.0, 96_000.0] {
        for id in [
            "rlive.electric",
            "rlive.tension",
            "rlive.collision",
            "rlive.operator",
        ] {
            let info = find(id).unwrap();
            let mut p = (info.create)(&DeviceInit::default());
            p.activate(rate, BLOCK);
            if id == "rlive.electric" {
                set(&mut *p, el::SYMMETRY, 70.0);
            }
            if id == "rlive.collision" {
                set(&mut *p, co::RES_BASE[0] + co::R_TYPE, co::STRING as f64);
                set(&mut *p, co::RES_BASE[0] + co::R_DECAY, 4_000.0);
            }
            let out = left(&mut *p, 65_536, &[on(0, 60)]);
            let hz = frequency_near(&out[8_192..], FIRST_NOTE, rate);
            assert!(cents(hz, FIRST_NOTE).abs() < 3.0, "{id} at {rate}: {hz}");
        }
    }
}

#[test]
fn pressure_and_slide_reach_their_destinations() {
    let expr = |offset, kind, value| {
        MidiEvent::new(
            offset,
            MidiMessage::NoteExpr {
                channel: 0,
                key: 60,
                note_id: 0,
                kind,
                value,
            },
        )
    };
    // Operator: per-note pressure is Aftertouch, here routed to Volume.
    let mut p = operator();
    set(&mut *p, op::G_MIDI_BASE[2], 20.0);
    set(&mut *p, op::G_MIDI_BASE[2] + 1, 100.0);
    let out = left(
        &mut *p,
        24_000,
        &[on(0, 60), expr(8_000, ExprKind::Pressure, 1.0)],
    );
    assert!(
        rms(&out[10_000..18_000]) > rms(&out[2_000..7_000]) * 1.5,
        "operator"
    );

    // Tension: slide to Volume, pressure to Filter Freq.
    let mut p = tension(te::BOW);
    set(&mut *p, te::SLIDE_DEST_A, 13.0);
    set(&mut *p, te::SLIDE_AMT_A, 100.0);
    let out = left(
        &mut *p,
        36_000,
        &[on(0, 60), expr(18_000, ExprKind::Slide, 1.0)],
    );
    assert!(
        rms(&out[24_000..34_000]) > rms(&out[10_000..17_000]) * 1.3,
        "tension"
    );

    // Collision: pressure to Res 1 Gain, heard in the ring after it arrives.
    let run = |pressed: bool| {
        let mut p = collision(co::STRING);
        set(&mut *p, co::MIDI_BASE[2], 14.0);
        set(&mut *p, co::MIDI_BASE[2] + 1, 100.0);
        let mut events = vec![on(0, 60)];
        if pressed {
            events.push(expr(4_096, ExprKind::Pressure, 1.0));
        }
        rms(&left(&mut *p, 24_000, &events)[8_192..])
    };
    assert!(run(true) > run(false) * 1.5, "collision");
}

#[test]
fn the_sustain_pedal_holds_a_released_note() {
    let pedal = |offset, value| {
        MidiEvent::new(
            offset,
            MidiMessage::ControlChange {
                channel: 0,
                cc: 64,
                value,
            },
        )
    };
    let mut p = operator();
    let out = left(
        &mut *p,
        48_000,
        &[pedal(0, 127), on(0, 60), off(2_400, 60), pedal(24_000, 0)],
    );
    assert!(rms(&out[12_000..23_000]) > 0.1, "held by the pedal");
    assert!(rms(&out[36_000..48_000]) < 1e-3, "released with it");
}

#[test]
fn operator_time_vel_interpolation_and_glide_do_what_they_say() {
    let env = op::OSC_BASE[0] + op::O_ENV;
    let decays = |velocity: f32, time_vel: f64| {
        let mut p = operator();
        set(&mut *p, env + op::E_SUSTAIN, -70.0);
        set(&mut *p, env + op::E_DECAY, 800.0);
        set(&mut *p, env + op::E_TIME_VEL, time_vel);
        rms(&left(&mut *p, 24_000, &[on_vel(0, 60, velocity)])[12_000..])
    };
    assert!(
        decays(1.0, 100.0) < decays(0.1, 100.0) * 0.8,
        "harder notes run faster"
    );
    assert!(
        (decays(1.0, 0.0) - decays(0.1, 0.0)).abs() < 0.02,
        "Time < Vel at zero does nothing"
    );

    // Interpolation off is a coarser read of the same noise.
    let noise = |interp: f64| {
        let mut p = operator();
        set(
            &mut *p,
            op::OSC_BASE[0] + op::O_WAVE,
            waves::W_NOISE_LOOP as f64,
        );
        set(&mut *p, op::G_INTERP, interp);
        left(&mut *p, 8_192, &[on(0, 40)])
    };
    assert_ne!(noise(1.0), noise(0.0));

    let mut p = operator();
    set(&mut *p, op::P_GLIDE, 1.0);
    set(&mut *p, op::P_GLIDE_TIME, 200.0);
    let out = left(&mut *p, 24_000, &[on(0, 60), on(4_800, 72)]);
    let c4 = FIRST_NOTE * 2.0;
    let settled = frequency_near(&out[20_000..], c4, SR);
    assert!(cents(settled, c4).abs() < 3.0, "{settled}");
    let early = frequency_near(&out[6_000..8_000], c4 * 0.8, SR);
    assert!(early < c4 * 0.97, "still on its way: {early}");
}
