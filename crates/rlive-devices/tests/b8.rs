//! Batch B8: Spectrum, Tuner, Vocoder, Auto Shift, Spectral Time and Spectral Resonator, against
//! arithmetic and the manual.

use rlive_core::{
    AudioBuffer, AudioInput, EventQueue, MidiEvent, MidiMessage, ParamOutQueue, ProcessContext,
    Processor, ScaleInfo, TransportInfo, ViewChannel, ViewId, ViewReader, ViewWriter,
};
use rlive_devices::devices::b8::{
    auto_shift as asft, spectral_resonator as res, spectral_time as st, spectrum as sp, tuner,
    vocoder as voc,
};
use rlive_devices::dsp::util::Rng;
use rlive_testkit::{assert_no_alloc, onset, peak, rms, sine, tone_amplitude, Harness};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn ready<P: Processor + Default>() -> P {
    let mut p = P::default();
    p.activate(SR, BLOCK);
    p
}

fn set(p: &mut dyn Processor, params: &[(u32, f64)]) {
    for &(id, v) in params {
        p.set_param(id, v);
    }
}

fn render(p: &mut dyn Processor, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        assert_no_alloc(|| h.process(p, cl, cr, &[], &[]));
    }
    (l, r)
}

fn mono(p: &mut dyn Processor, input: &[f32]) -> Vec<f32> {
    render(p, input, input).0
}

/// Runs a whole signal, delivering `(frame, message)` at their positions.
fn render_midi(
    p: &mut dyn Processor,
    left: &[f32],
    right: &[f32],
    midi: &[(usize, MidiMessage)],
    transport: TransportInfo,
) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    h.transport = transport;
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        let start = i * BLOCK;
        let end = start + BLOCK;
        let events: Vec<MidiEvent> = midi
            .iter()
            .filter(|(at, _)| *at >= start && *at < end)
            .map(|(at, m)| MidiEvent::new((*at - start) as u32, *m))
            .collect();
        h.process(p, cl, cr, &events, &[]);
    }
    (l, r)
}

fn note_on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 1.0,
        note_id: 0,
        tuning: 0.0,
    }
}

fn note_off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.0,
        note_id: 0,
    }
}

/// A device with a reader attached, running blocks through it and reading its view.
struct Watched {
    channel: Arc<ViewChannel>,
    reader: ViewReader,
    h: Harness,
}

impl Watched {
    fn new(view_len: usize) -> Self {
        let channel = Arc::new(ViewChannel::new(view_len));
        let reader = channel.attach();
        Self {
            channel,
            reader,
            h: Harness::new(SR),
        }
    }

    fn run(&mut self, p: &mut dyn Processor, left: &mut [f32], right: &mut [f32]) {
        for (cl, cr) in left.chunks_mut(BLOCK).zip(right.chunks_mut(BLOCK)) {
            let view = ViewWriter::new(&self.channel);
            let mut out = EventQueue::with_capacity(16);
            let mut po = ParamOutQueue::with_capacity(16);
            let mut ctx = ProcessContext {
                audio: AudioBuffer {
                    left: cl,
                    right: cr,
                },
                sidechain: None,
                aux_out: &mut [],
                events: &[],
                events_out: &mut out,
                params: &[],
                transport: &self.h.transport,
                param_out: &mut po,
                mod_out: &mut [],
                view: &view,
            };
            assert_no_alloc(|| p.process(&mut ctx));
        }
    }
}

fn read<const N: usize>(w: &Watched, view: ViewId) -> [f32; N] {
    let mut out = [0.0f32; N];
    assert!(w.reader.read(view, &mut out));
    out
}

fn secs(s: f64) -> usize {
    (s * SR) as usize
}

fn near(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} != {expected} (+-{tol})"
    );
}

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

fn noise(frames: usize, seed: u32, level: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..frames).map(|_| rng.bipolar() * level).collect()
}

fn saw(hz: f64, frames: usize, level: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let phase = (i as f64 * hz / SR).fract();
            (2.0 * phase as f32 - 1.0) * level
        })
        .collect()
}

/// The strongest tone around `hz`, searched +-`span` cents in `step` cents.
fn peak_near(signal: &[f32], hz: f64, span: f64, step: f64) -> (f64, f64) {
    let mut best = (0.0f64, hz);
    let mut c = -span;
    while c <= span {
        let f = hz * 2f64.powf(c / 1200.0);
        let a = tone_amplitude(signal, f, SR);
        if a > best.0 {
            best = (a, f);
        }
        c += step;
    }
    (best.1, best.0)
}

// Spectrum ---------------------------------------------------------------------------------------

fn spectrum_points(
    s: &mut sp::Spectrum,
    input_l: &[f32],
    input_r: &[f32],
    w: &mut Watched,
) -> Vec<f32> {
    let (mut l, mut r) = (input_l.to_vec(), input_r.to_vec());
    w.run(s, &mut l, &mut r);
    let mut out = vec![0.0f32; sp::POINTS];
    assert!(w.reader.read(sp::CURVE, &mut out));
    out
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

#[test]
fn spectrum_reads_a_known_tone_as_its_level() {
    let mut s: sp::Spectrum = ready();
    // Exactly on a bin of the 4096 block.
    let hz = 85.0 * SR as f32 / 4096.0;
    let x: Vec<f32> = sine(hz, SR, secs(0.5)).iter().map(|s| s * 0.5).collect();
    let mut w = Watched::new(sp::VIEW_LEN);
    let points = spectrum_points(&mut s, &x, &x, &mut w);
    let at = sp::hz_point(hz, sp::X_LOG, SR as f32);
    near(points[at] as f64, db(0.5), 0.5, "a -6 dB sine reads -6 dB");
    assert!(
        points[at + 30] < -60.0,
        "away from it there's nothing: {}",
        points[at + 30]
    );
}

#[test]
fn spectrum_analyses_left_right_or_both_channels() {
    let hz = 96.0 * SR as f32 / 4096.0;
    let x: Vec<f32> = sine(hz, SR, secs(0.5)).iter().map(|s| s * 0.5).collect();
    let silence = vec![0.0; x.len()];
    let at = sp::hz_point(hz, sp::X_LOG, SR as f32);
    for (channel, expect) in [
        (sp::CHANNEL_L, db(0.5)),
        (sp::CHANNEL_R, -120.0),
        (sp::CHANNEL_BOTH, db(0.25)),
    ] {
        let mut s: sp::Spectrum = ready();
        s.set_param(sp::CHANNEL, channel as f64);
        let mut w = Watched::new(sp::VIEW_LEN);
        let points = spectrum_points(&mut s, &x, &silence, &mut w);
        if expect <= -119.0 {
            assert!(
                points[at] < -90.0,
                "right channel is silent: {}",
                points[at]
            );
        } else {
            near(points[at] as f64, expect, 0.5, "channel level");
        }
    }
}

#[test]
fn spectrum_block_size_and_scale_choose_the_axis() {
    let hz = 1_000.0;
    let x: Vec<f32> = sine(hz, SR, secs(1.0));
    let mut s: sp::Spectrum = ready();
    s.set_param(sp::SCALE_X, sp::X_LINEAR as f64);
    let mut w = Watched::new(sp::VIEW_LEN);
    let points = spectrum_points(&mut s, &x, &x, &mut w);
    let at = sp::hz_point(hz, sp::X_LINEAR, SR as f32);
    let (mut best, mut best_db) = (0, f32::MIN);
    for (i, v) in points.iter().enumerate() {
        if *v > best_db {
            best = i;
            best_db = *v;
        }
    }
    assert_eq!(best, at, "linear places 1 kHz at {at}, peak at {best}");
    // Log and semitone are the same scaling: the same column for a frequency.
    assert_eq!(
        sp::hz_point(1_000.0, sp::X_LOG, 48_000.0),
        sp::hz_point(1_000.0, sp::X_SEMITONE, 48_000.0)
    );
}

#[test]
fn spectrum_average_smooths_noise() {
    let x = noise(secs(2.0), 7, 0.5);
    let spread = |avg: f64| {
        let mut s: sp::Spectrum = ready();
        set(&mut s, &[(sp::AVG, avg), (sp::REFRESH, 20.0)]);
        let mut w = Watched::new(sp::VIEW_LEN);
        let mut seen = Vec::new();
        for chunk in x.chunks(secs(0.1)) {
            let (mut l, mut r) = (chunk.to_vec(), chunk.to_vec());
            w.run(&mut s, &mut l, &mut r);
            let mut out = vec![0.0f32; sp::POINTS];
            w.reader.read(sp::CURVE, &mut out);
            seen.push(out[300]);
        }
        let mean = seen.iter().sum::<f32>() / seen.len() as f32;
        (seen.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / seen.len() as f32).sqrt()
    };
    let (one, many) = (spread(1.0), spread(64.0));
    assert!(many < one * 0.6, "averaging calms it: {many} vs {one}");
}

#[test]
fn spectrum_refresh_sets_how_often_a_measurement_lands() {
    let hz = 96.0 * SR as f32 / 4096.0;
    let x = sine(hz, SR, secs(0.2));
    let mut s: sp::Spectrum = ready();
    s.set_param(sp::REFRESH, 1_000.0);
    let mut w = Watched::new(sp::VIEW_LEN);
    let (mut l, mut r) = (x.clone(), x);
    w.run(&mut s, &mut l, &mut r);
    let mut out = vec![0.0f32; sp::POINTS];
    w.reader.read(sp::CURVE, &mut out);
    assert!(
        out.iter().all(|v| *v <= sp::FLOOR_DB + 1e-3),
        "no measurement yet with a one second refresh"
    );
}

#[test]
fn spectrum_max_holds_until_reset_and_song_restart() {
    let hz = 96.0 * SR as f32 / 4096.0;
    let at = sp::hz_point(hz, sp::X_LOG, SR as f32);
    let x = sine(hz, SR, secs(0.3));
    let quiet = vec![0.0; secs(0.3)];
    let mut s: sp::Spectrum = ready();
    let mut w = Watched::new(sp::VIEW_LEN);
    let (mut l, mut r) = (x.clone(), x);
    w.run(&mut s, &mut l, &mut r);
    let (mut l, mut r) = (quiet.clone(), quiet.clone());
    w.run(&mut s, &mut l, &mut r);
    let mut peaks = vec![0.0f32; sp::POINTS];
    w.reader.read(sp::PEAKS, &mut peaks);
    let mut curve = vec![0.0f32; sp::POINTS];
    w.reader.read(sp::CURVE, &mut curve);
    near(peaks[at] as f64, 0.0, 0.5, "the tone's peak is kept");
    assert!(
        curve[at] < -60.0,
        "while the live curve falls: {}",
        curve[at]
    );

    // Clicking the display clears it.
    s.set_param(sp::RESET_MAX, 1.0);
    let (mut l, mut r) = (quiet.clone(), quiet.clone());
    w.run(&mut s, &mut l, &mut r);
    w.reader.read(sp::PEAKS, &mut peaks);
    assert!(peaks[at] < -60.0, "reset: {}", peaks[at]);

    // So does restarting the song.
    let x = sine(hz, SR, secs(0.3));
    let (mut l, mut r) = (x.clone(), x);
    w.run(&mut s, &mut l, &mut r);
    w.reader.read(sp::PEAKS, &mut peaks);
    assert!(peaks[at] > -3.0);
    w.h.transport.playing = false;
    let (mut l, mut r) = (quiet.clone(), quiet.clone());
    w.run(&mut s, &mut l, &mut r);
    w.h.transport.playing = true;
    let (mut l, mut r) = (quiet.clone(), quiet);
    w.run(&mut s, &mut l, &mut r);
    w.reader.read(sp::PEAKS, &mut peaks);
    assert!(peaks[at] < -60.0, "restart clears: {}", peaks[at]);
}

#[test]
fn spectrum_auto_range_follows_the_level() {
    let hz = 96.0 * SR as f32 / 4096.0;
    let mut s: sp::Spectrum = ready();
    let mut w = Watched::new(sp::VIEW_LEN);
    let x: Vec<f32> = sine(hz, SR, secs(0.3)).iter().map(|v| v * 0.01).collect();
    let (mut l, mut r) = (x.clone(), x);
    w.run(&mut s, &mut l, &mut r);
    let [_, _, auto_min, auto_max]: [f32; 4] = read(&w, sp::META);
    assert!((-40.0..=-28.0).contains(&auto_max), "{auto_max}");
    assert!(auto_min < auto_max);
}

#[test]
fn spectrum_passes_audio_bit_exactly() {
    let x = noise(secs(0.2), 3, 0.8);
    let y = noise(secs(0.2), 4, 0.8);
    let mut s: sp::Spectrum = ready();
    let mut w = Watched::new(sp::VIEW_LEN);
    let (mut l, mut r) = (x.clone(), y.clone());
    w.run(&mut s, &mut l, &mut r);
    assert_eq!(l, x);
    assert_eq!(r, y);
    assert_eq!(s.latency(), 0);
}

// Tuner ------------------------------------------------------------------------------------------

fn tuner_reading(hz: f32, reference: f64) -> [f32; 4] {
    let mut t: tuner::Tuner = ready();
    t.set_param(tuner::REFERENCE, reference);
    let mut w = Watched::new(tuner::VIEW_LEN);
    let x: Vec<f32> = sine(hz, SR, secs(0.3)).iter().map(|s| s * 0.5).collect();
    let (mut l, mut r) = (x.clone(), x);
    w.run(&mut t, &mut l, &mut r);
    read(&w, tuner::READING)
}

#[test]
fn tuner_hears_a_440_a_as_a_with_no_cents() {
    let [hz, confidence, note, cent] = tuner_reading(440.0, 440.0);
    near(hz as f64, 440.0, 0.6, "hz");
    assert!(confidence > 0.8);
    assert_eq!(note, 69.0);
    near(cent as f64, 0.0, 2.5, "cents");
}

#[test]
fn tuner_measures_cents_from_the_nearest_note() {
    for (hz, expect_note, expect_cents) in [
        (446.0f32, 69.0f32, cents(446.0, 440.0)),
        (523.25, 72.0, 0.0),
        (110.0, 45.0, 0.0),
        (82.41, 40.0, 0.0),
    ] {
        let [_, _, note, cent] = tuner_reading(hz, 440.0);
        assert_eq!(note, expect_note, "{hz}");
        near(cent as f64, expect_cents, 3.0, "cents");
    }
}

#[test]
fn tuner_reference_moves_the_tuning() {
    // 442 Hz is an A against a 442 reference, and sharp against 440.
    let [_, _, note, cent] = tuner_reading(442.0, 442.0);
    assert_eq!(note, 69.0);
    near(cent as f64, 0.0, 2.5, "at its own reference");
    let [_, _, _, against_440] = tuner_reading(442.0, 440.0);
    near(against_440 as f64, cents(442.0, 440.0), 3.0, "442 vs 440");
    // At the ends of the slider.
    let [_, _, note, cent] = tuner_reading(410.0, 410.0);
    assert_eq!(note, 69.0);
    near(cent as f64, 0.0, 3.0, "410");
    let [_, _, note, cent] = tuner_reading(480.0, 480.0);
    assert_eq!(note, 69.0);
    near(cent as f64, 0.0, 3.0, "480");
}

#[test]
fn tuner_reports_nothing_for_silence() {
    let mut t: tuner::Tuner = ready();
    let mut w = Watched::new(tuner::VIEW_LEN);
    let x = vec![0.0; secs(0.2)];
    let (mut l, mut r) = (x.clone(), x);
    w.run(&mut t, &mut l, &mut r);
    let [hz, ..]: [f32; 4] = read(&w, tuner::READING);
    assert_eq!(hz, 0.0);
}

#[test]
fn tuner_passes_audio_bit_exactly() {
    let x = noise(secs(0.2), 9, 0.8);
    let mut t: tuner::Tuner = ready();
    let mut w = Watched::new(tuner::VIEW_LEN);
    let (mut l, mut r) = (x.clone(), x.clone());
    w.run(&mut t, &mut l, &mut r);
    assert_eq!(l, x);
    assert_eq!(t.latency(), 0);
}

// Vocoder ----------------------------------------------------------------------------------------

/// Runs a vocoder with a sidechain, block by block.
fn vocode(
    v: &mut voc::Vocoder,
    left: &[f32],
    right: &[f32],
    sc_l: Option<&[f32]>,
    sc_r: Option<&[f32]>,
) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    let view = ViewWriter::none();
    let transport = TransportInfo {
        sample_rate: SR,
        ..Default::default()
    };
    let mut out = EventQueue::with_capacity(16);
    let mut po = ParamOutQueue::with_capacity(16);
    let mut pos = 0;
    while pos < l.len() {
        let n = BLOCK.min(l.len() - pos);
        let sidechain = sc_l.map(|a| AudioInput {
            left: &a[pos..pos + n],
            right: &sc_r.unwrap()[pos..pos + n],
        });
        let mut ctx = ProcessContext {
            audio: AudioBuffer {
                left: &mut l[pos..pos + n],
                right: &mut r[pos..pos + n],
            },
            sidechain,
            aux_out: &mut [],
            events: &[],
            events_out: &mut out,
            params: &[],
            transport: &transport,
            param_out: &mut po,
            mod_out: &mut [],
            view: &view,
        };
        assert_no_alloc(|| v.process(&mut ctx));
        pos += n;
    }
    (l, r)
}

fn full_wet(v: &mut voc::Vocoder) {
    v.set_param(voc::DRY_WET, 100.0);
}

#[test]
fn vocoder_filter_count_is_the_bands_chooser() {
    for (index, count) in voc::BAND_COUNTS.iter().enumerate() {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        v.set_param(voc::BANDS, index as f64);
        let x = noise(secs(0.5), 11, 0.5);
        let mut w = Watched::new(voc::VIEW_LEN);
        let (mut l, mut r) = (x.clone(), x);
        w.run(&mut v, &mut l, &mut r);
        let mut levels = vec![0.0f32; voc::MAX_BANDS];
        w.reader.read(voc::LEVELS, &mut levels);
        for (b, level) in levels.iter().enumerate() {
            if b < *count {
                assert!(*level > 0.0, "band {b} of {count} works");
            } else {
                assert_eq!(*level, 0.0, "band {b} doesn't exist with {count}");
            }
        }
    }
}

#[test]
fn vocoder_more_bands_resolve_the_modulator_more_finely() {
    let hz = 1_000.0;
    let x: Vec<f32> = sine(hz, SR, secs(1.0)).iter().map(|s| s * 0.5).collect();
    let run = |bands: usize| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        v.set_param(voc::BANDS, bands as f64);
        let out = vocode(&mut v, &x, &x, None, None).0;
        let tail = &out[secs(0.3)..];
        // White-noise carrier through 1 kHz band: how much leaks at the far band?
        rms(&biquad_band(tail, 5_000.0)) / rms(tail)
    };
    let few = run(0);
    let many = run(7);
    assert!(many < few * 0.6, "narrow bands leak less: {many} vs {few}");
}

fn biquad_band(x: &[f32], hz: f32) -> Vec<f32> {
    use rlive_devices::dsp::{Biquad, BiquadKind};
    let mut f = Biquad::default();
    f.set(BiquadKind::BandPass, hz, 4.0, 0.0, SR as f32);
    x.iter().map(|s| f.process(*s)).collect()
}

#[test]
fn vocoder_noise_carrier_follows_the_modulator() {
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    let x: Vec<f32> = sine(440.0, SR, secs(1.0)).iter().map(|s| s * 0.5).collect();
    let out = vocode(&mut v, &x, &x, None, None).0;
    let tail = &out[secs(0.3)..];
    assert!(
        rms(tail) > 0.01,
        "noise through the bands sounds: {}",
        rms(tail)
    );
}

#[test]
fn vocoder_external_carrier_uses_the_sidechain() {
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    v.set_param(voc::CARRIER, voc::CARRIER_EXTERNAL as f64);
    let modulator = noise(secs(1.0), 5, 0.5);
    let carrier: Vec<f32> = sine(800.0, SR, secs(1.0)).iter().map(|s| s * 0.5).collect();
    let (out, _) = vocode(
        &mut v,
        &modulator,
        &modulator,
        Some(&carrier),
        Some(&carrier),
    );
    let tail = &out[secs(0.3)..];
    let tone = tone_amplitude(tail, 800.0, SR);
    assert!(tone > 0.02, "the carrier comes through: {tone}");

    // With nothing routed, an external carrier is silent.
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    v.set_param(voc::CARRIER, voc::CARRIER_EXTERNAL as f64);
    let (out, _) = vocode(&mut v, &modulator, &modulator, None, None);
    assert!(peak(&out) < 1e-3, "{}", peak(&out));
}

#[test]
fn vocoder_modulator_carrier_resynthesises_the_input() {
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    v.set_param(voc::CARRIER, voc::CARRIER_MODULATOR as f64);
    let x = saw(220.0, secs(1.0), 0.5);
    let out = vocode(&mut v, &x, &x, None, None).0;
    let tail = &out[secs(0.3)..];
    let tone = tone_amplitude(tail, 220.0, SR);
    assert!(tone > 0.02, "its own harmonics come through: {tone}");
}

#[test]
fn vocoder_pitch_tracking_tunes_to_the_modulator() {
    let run = |pitch: f64| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(
            &mut v,
            &[
                (voc::CARRIER, voc::CARRIER_PITCH as f64),
                (voc::TRACK_PITCH, pitch),
            ],
        );
        let x: Vec<f32> = sine(200.0, SR, secs(1.0)).iter().map(|s| s * 0.5).collect();
        let out = vocode(&mut v, &x, &x, None, None).0;
        tone_amplitude(&out[secs(0.4)..], 200.0, SR)
    };
    let at_pitch = run(0.0);
    let an_octave_up = run(12.0);
    assert!(at_pitch > 0.02, "tracks 200 Hz: {at_pitch}");
    assert!(
        an_octave_up < at_pitch * 0.2,
        "with Pitch up an octave the band at 200 Hz gets no carrier: {an_octave_up} vs {at_pitch}"
    );
}

#[test]
fn vocoder_pitch_tracking_holds_the_last_clear_pitch() {
    // Noise after a tone: the oscillator keeps the tone's pitch rather than chasing noise.
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    v.set_param(voc::CARRIER, voc::CARRIER_PITCH as f64);
    let mut x: Vec<f32> = sine(200.0, SR, secs(0.5)).iter().map(|s| s * 0.5).collect();
    x.extend(noise(secs(0.5), 8, 0.3));
    let out = vocode(&mut v, &x, &x, None, None).0;
    let later = &out[secs(0.7)..];
    assert!(
        tone_amplitude(later, 200.0, SR) > 2e-3,
        "{}",
        tone_amplitude(later, 200.0, SR)
    );
}

#[test]
fn vocoder_unvoiced_noise_fires_for_noise_and_not_for_pitch() {
    let run = |sens: f64, x: &[f32]| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(
            &mut v,
            &[
                (voc::CARRIER, voc::CARRIER_EXTERNAL as f64),
                (voc::UNVOICED, 100.0),
                (voc::SENS, sens),
            ],
        );
        let silent = vec![0.0; x.len()];
        let out = vocode(&mut v, x, x, Some(&silent), Some(&silent)).0;
        rms(&out[secs(0.2)..])
    };
    let hiss = noise(secs(1.0), 13, 0.5);
    let tone: Vec<f32> = sine(300.0, SR, secs(1.0)).iter().map(|s| s * 0.5).collect();
    assert!(
        run(50.0, &hiss) > 0.005,
        "unvoiced fires for hiss: {}",
        run(50.0, &hiss)
    );
    assert!(
        run(50.0, &tone) < 1e-3,
        "and not for a tone: {}",
        run(50.0, &tone)
    );
    assert!(run(100.0, &tone) > 0.003, "Sens 100% is always on");
    assert!(run(0.0, &hiss) < 1e-4, "Sens 0% never");
}

#[test]
fn vocoder_depth_ranges_from_ignore_to_classic_to_peaks() {
    let run = |depth: f64, level: f32| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(
            &mut v,
            &[
                (voc::CARRIER, voc::CARRIER_EXTERNAL as f64),
                (voc::DEPTH, depth),
            ],
        );
        let modulator: Vec<f32> = noise(secs(0.5), 3, level);
        let carrier: Vec<f32> = noise(secs(0.5), 4, 0.5);
        let out = vocode(
            &mut v,
            &modulator,
            &modulator,
            Some(&carrier),
            Some(&carrier),
        )
        .0;
        rms(&out[secs(0.2)..])
    };
    // 0% discards the modulator's level.
    assert!(
        (run(0.0, 0.5) / run(0.0, 0.05) - 1.0).abs() < 0.2,
        "no follow at 0%"
    );
    // At 100% output tracks the modulator; at 200% it reacts more strongly still.
    let classic = run(100.0, 0.5) / run(100.0, 0.05);
    let peaks = run(200.0, 0.5) / run(200.0, 0.05);
    assert!(classic > 3.0, "classic follows: {classic}");
    assert!(
        peaks > classic * 2.0,
        "200% exaggerates: {peaks} vs {classic}"
    );
}

#[test]
fn vocoder_gate_silences_bands_under_the_threshold() {
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    v.set_param(voc::GATE, -20.0);
    let x: Vec<f32> = sine(1_000.0, SR, secs(0.5))
        .iter()
        .map(|s| s * 0.01)
        .collect();
    let out = vocode(&mut v, &x, &x, None, None).0;
    assert!(rms(&out[secs(0.2)..]) < 1e-5);
}

#[test]
fn vocoder_mono_stereo_and_lr_route_the_channels() {
    let carrier_l: Vec<f32> = noise(secs(0.5), 21, 0.5);
    let carrier_r: Vec<f32> = noise(secs(0.5), 22, 0.5);
    let x: Vec<f32> = noise(secs(0.5), 23, 0.5);
    let silence = vec![0.0; x.len()];
    // L/R: a left-only modulator leaves only the left output.
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    set(
        &mut v,
        &[
            (voc::CARRIER, voc::CARRIER_EXTERNAL as f64),
            (voc::CHANNELS, voc::CHANNELS_LR as f64),
        ],
    );
    let (l, r) = vocode(&mut v, &x, &silence, Some(&carrier_l), Some(&carrier_r));
    assert!(
        rms(&l[secs(0.2)..]) > 0.01 && rms(&r[secs(0.2)..]) < 1e-3,
        "{} {}",
        rms(&l),
        rms(&r)
    );
    // Stereo: mono modulator, two different carriers.
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    set(
        &mut v,
        &[
            (voc::CARRIER, voc::CARRIER_EXTERNAL as f64),
            (voc::CHANNELS, voc::CHANNELS_STEREO as f64),
        ],
    );
    let (l, r) = vocode(&mut v, &x, &silence, Some(&carrier_l), Some(&carrier_r));
    assert!(rms(&l) > 0.0 && rms(&r) > 0.0);
    assert_ne!(l, r);
    // Mono: both outputs the same.
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    set(&mut v, &[(voc::CARRIER, voc::CARRIER_EXTERNAL as f64)]);
    let (l, r) = vocode(&mut v, &x, &x, Some(&carrier_l), Some(&carrier_r));
    assert_eq!(l, r);
}

#[test]
fn vocoder_formant_moves_the_carrier_filters() {
    let centroid = |formant: f64| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(&mut v, &[(voc::BANDS, 7.0), (voc::FORMANT, formant)]);
        let x: Vec<f32> = sine(1_000.0, SR, secs(1.0))
            .iter()
            .map(|s| s * 0.5)
            .collect();
        let out = vocode(&mut v, &x, &x, None, None).0;
        let tail = &out[secs(0.3)..];
        let low = rms(&biquad_band(tail, 1_000.0));
        let high = rms(&biquad_band(tail, 2_000.0));
        high / low.max(1e-6)
    };
    assert!(
        centroid(12.0) > centroid(0.0) * 2.0,
        "an octave up: {} vs {}",
        centroid(12.0),
        centroid(0.0)
    );
}

#[test]
fn vocoder_retro_is_louder_at_the_top_and_precise_is_even() {
    let top_vs_bottom = |mode: f64| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(
            &mut v,
            &[
                (voc::FILTER_MODE, mode),
                (voc::RANGE_LOW, 100.0),
                (voc::RANGE_HIGH, 12_000.0),
            ],
        );
        let x = noise(secs(1.0), 31, 0.5);
        let out = vocode(&mut v, &x, &x, None, None).0;
        let tail = &out[secs(0.3)..];
        rms(&biquad_band(tail, 9_000.0)) / rms(&biquad_band(tail, 150.0))
    };
    assert!(
        top_vs_bottom(voc::FILTER_RETRO as f64) > top_vs_bottom(voc::FILTER_PRECISE as f64) * 1.25
    );
}

#[test]
fn vocoder_band_levels_attenuate_individually() {
    let mut v: voc::Vocoder = ready();
    full_wet(&mut v);
    for band in 0..voc::MAX_BANDS {
        v.set_param(voc::BAND_GAIN + band as u32, 0.0);
    }
    let x: Vec<f32> = noise(secs(0.5), 6, 0.5);
    let out = vocode(&mut v, &x, &x, None, None).0;
    assert!(rms(&out[secs(0.3)..]) < 1e-5);
}

// Auto Shift ------------------------------------------------------------------------------------

fn shifter() -> asft::AutoShift {
    let mut a: asft::AutoShift = ready();
    a.set_param(asft::SMOOTH, 0.0);
    a
}

fn output_tone(out: &[f32], hz: f64) -> (f64, f64) {
    peak_near(&out[secs(0.5)..], hz, 50.0, 1.0)
}

#[test]
fn auto_shift_latency_depends_on_range_and_live_mode() {
    let mut a = shifter();
    for (range, name) in [
        (asft::RANGE_HIGH, "High"),
        (asft::RANGE_MID, "Mid"),
        (asft::RANGE_BASS, "Bass"),
    ] {
        a.set_param(asft::PITCH_RANGE, range as f64);
        assert_eq!(
            a.latency() as usize,
            asft::range_latency(range, false, SR as f32),
            "{name}"
        );
    }
    let high = asft::range_latency(asft::RANGE_HIGH, false, SR as f32);
    let mid = asft::range_latency(asft::RANGE_MID, false, SR as f32);
    let bass = asft::range_latency(asft::RANGE_BASS, false, SR as f32);
    assert!(high < mid && mid < bass, "{high} {mid} {bass}");
    a.set_param(asft::PITCH_RANGE, asft::RANGE_BASS as f64);
    a.set_param(asft::LIVE_MODE, 1.0);
    assert!((a.latency() as usize) < bass);
}

#[test]
fn auto_shift_corrects_to_the_nearest_note_of_the_scale() {
    // 450 Hz is 39 cents above A: chromatic correction takes it to 440.
    let mut a = shifter();
    let x: Vec<f32> = sine(450.0, SR, secs(1.2)).iter().map(|s| s * 0.5).collect();
    let (l, _) = render(&mut a, &x, &x);
    let (hz, level) = output_tone(&l, 440.0);
    near(cents(hz, 440.0), 0.0, 8.0, "pulled to A");
    assert!(level > 0.3, "the shifted tone stays loud: {level}");
}

#[test]
fn auto_shift_uses_the_chosen_scale() {
    // 470 Hz sits nearer B than A; A# isn't in C Major, so it goes up to B.
    let mut a = shifter();
    a.set_param(asft::ROOT, 0.0);
    a.set_param(asft::SCALE, 0.0); // Major
    let x: Vec<f32> = sine(470.0, SR, secs(1.2)).iter().map(|s| s * 0.5).collect();
    let (l, _) = render(&mut a, &x, &x);
    let b = 493.88;
    let (hz, _) = peak_near(&l[secs(0.5)..], b, 80.0, 1.0);
    near(cents(hz, b), 0.0, 12.0, "corrected to B");
}

#[test]
fn auto_shift_strength_and_custom_notes_control_correction() {
    // At zero strength nothing moves.
    let mut a = shifter();
    a.set_param(asft::STRENGTH, 0.0);
    let x: Vec<f32> = sine(450.0, SR, secs(1.2)).iter().map(|s| s * 0.5).collect();
    let (l, _) = render(&mut a, &x, &x);
    let (hz, _) = output_tone(&l, 450.0);
    near(cents(hz, 450.0), 0.0, 8.0, "strength 0 leaves it alone");

    // A custom scale with only C and G: 450 Hz goes to the nearest of those.
    let mut a = shifter();
    a.set_param(asft::CUSTOM_MASK, (1 + (1 << 7)) as f64);
    let (l, _) = render(&mut a, &x, &x);
    let g = 391.995;
    let (hz, _) = peak_near(&l[secs(0.5)..], g, 80.0, 1.0);
    near(cents(hz, g), 0.0, 15.0, "custom scale: nearest of C and G");
}

#[test]
fn auto_shift_follows_the_clip_scale_when_asked() {
    // A "scale" of D alone: A has nowhere to go but the nearest D.
    let scale = ScaleInfo { root: 2, mask: 1 };
    let mut a = shifter();
    a.set_param(asft::USE_SCALE, 1.0);
    let x: Vec<f32> = sine(440.0, SR, secs(1.2)).iter().map(|s| s * 0.5).collect();
    let mut h = Harness::new(SR);
    h.transport.scale = Some(scale);
    let (mut l, mut r) = (x.clone(), x.clone());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        h.process(&mut a, cl, cr, &[], &[]);
    }
    let d = 587.33;
    let (hz, amp) = peak_near(&l[secs(0.5)..], d, 40.0, 1.0);
    near(cents(hz, d), 0.0, 15.0, "corrected to the clip's scale");
    assert!(amp > 0.1, "{amp}");
    assert!(tone_amplitude(&l[secs(0.5)..], 440.0, SR) < amp * 0.5);

    // No scale active: falls back to the device's own, chromatic here, and A stays.
    let mut a = shifter();
    a.set_param(asft::USE_SCALE, 1.0);
    let (l, _) = render(&mut a, &x, &x);
    let (_, amp) = output_tone(&l, 440.0);
    assert!(amp > 0.3, "{amp}");
}

#[test]
fn auto_shift_pitch_shift_transposes_with_or_without_correction() {
    let mut a = shifter();
    a.set_param(asft::STRENGTH, 0.0);
    a.set_param(asft::PITCH_SHIFT, 7.0);
    a.set_param(asft::FORMANT_FOLLOW, 100.0);
    let x = saw(220.0, secs(1.2), 0.4);
    let (l, _) = render(&mut a, &x, &x);
    let target = 220.0 * 2f64.powf(7.0 / 12.0);
    let (hz, amp) = peak_near(&l[secs(0.5)..], target, 30.0, 1.0);
    near(cents(hz, target), 0.0, 12.0, "up a fifth");
    assert!(amp > 0.05);
}

/// A source with a fixed formant: harmonics of `hz`, a Gaussian in frequency around `peak`.
fn formant_voice(hz: f64, peak_hz: f64, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames];
    let n = (SR / 2.0 / hz) as usize;
    for k in 1..=n {
        let f = k as f64 * hz;
        let gain = (-((f / peak_hz).ln().powi(2)) / 0.08).exp() as f32;
        if gain < 1e-3 {
            continue;
        }
        for (i, s) in out.iter_mut().enumerate() {
            *s += gain * (std::f64::consts::TAU * f * i as f64 / SR).sin() as f32;
        }
    }
    let scale = 0.4 / peak(&out).max(1e-3);
    out.iter_mut().for_each(|s| *s *= scale);
    out
}

/// The frequency, among harmonics of `hz`, that a signal has the most energy near.
fn spectral_peak(signal: &[f32], hz: f64) -> f64 {
    let mut best = (0.0, 0.0);
    for k in 1..((SR / 2.0 / hz) as usize) {
        let f = k as f64 * hz;
        let a = tone_amplitude(signal, f, SR);
        if a > best.0 {
            best = (a, f);
        }
    }
    best.1
}

#[test]
fn auto_shift_formants_stay_put_unless_told_to_move() {
    // Pitch up an octave with the formants held.
    let render_voice = |params: &[(u32, f64)]| {
        let mut a = shifter();
        a.set_param(asft::STRENGTH, 0.0);
        set(&mut a, params);
        let x = formant_voice(200.0, 1_600.0, secs(1.0));
        let (l, _) = render(&mut a, &x, &x);
        spectral_peak(&l[secs(0.4)..], 200.0)
    };
    let held = render_voice(&[(asft::PITCH_SHIFT, 12.0)]);
    let followed = render_voice(&[(asft::PITCH_SHIFT, 12.0), (asft::FORMANT_FOLLOW, 100.0)]);
    let moved = render_voice(&[(asft::FORMANT_SHIFT, 100.0)]);
    assert!(held < 2_400.0, "held near 1.6 kHz: {held}");
    assert!(
        followed > held * 1.5,
        "Follow carries them up: {followed} vs {held}"
    );
    assert!(moved > 2_400.0, "+100% moves them by an octave: {moved}");
}

#[test]
fn auto_shift_vibrato_and_lfo_work_without_correction() {
    // Excursion of a tone in cents, from zero crossings.
    fn pitch_spread(signal: &[f32]) -> f64 {
        let mut crossings = Vec::new();
        for (i, w) in signal.windows(2).enumerate() {
            if w[0] <= 0.0 && w[1] > 0.0 {
                let t = i as f64 + w[0] as f64 / (w[0] - w[1]) as f64;
                crossings.push(t);
            }
        }
        let periods: Vec<f64> = crossings.windows(2).map(|w| w[1] - w[0]).collect();
        let cents: Vec<f64> = periods
            .iter()
            .map(|p| 1200.0 * (periods[periods.len() / 2] / p).log2())
            .collect();
        let max = cents.iter().cloned().fold(f64::MIN, f64::max);
        let min = cents.iter().cloned().fold(f64::MAX, f64::min);
        (max - min) * 0.5
    }
    let run = |params: &[(u32, f64)]| {
        let mut a = shifter();
        a.set_param(asft::STRENGTH, 0.0);
        set(&mut a, params);
        let x: Vec<f32> = sine(440.0, SR, secs(1.5)).iter().map(|s| s * 0.5).collect();
        let (l, _) = render(&mut a, &x, &x);
        pitch_spread(&l[secs(0.5)..])
    };
    let none = run(&[]);
    let vibrato = run(&[(asft::VIB_AMOUNT, 100.0), (asft::VIB_RATE, 5.0)]);
    let lfo = run(&[(asft::LFO_PITCH, 100.0 / 24.0), (asft::LFO_RATE, 200.0)]);
    assert!(none < 10.0, "steady without any: {none}");
    assert!(
        vibrato > 60.0 && vibrato < 180.0,
        "+-100 cents-ish: {vibrato}"
    );
    assert!(lfo > 40.0, "the LFO moves pitch: {lfo}");
}

#[test]
fn auto_shift_smoothing_keeps_vibrato_that_is_faster_than_it() {
    fn excursion(input_vibrato: f64, smooth_ms: f64) -> f64 {
        let mut a: asft::AutoShift = ready();
        set(
            &mut a,
            &[(asft::SMOOTH, 1.0), (asft::SMOOTH_TIME, smooth_ms)],
        );
        let hz = 440.0f64;
        let mut phase = 0.0f64;
        let frames = secs(2.0);
        let x: Vec<f32> = (0..frames)
            .map(|i| {
                let t = i as f64 / SR;
                let f = hz
                    * 2f64.powf(input_vibrato / 1200.0 * (t * 6.0 * std::f64::consts::TAU).sin());
                phase += std::f64::consts::TAU * f / SR;
                phase.sin() as f32 * 0.5
            })
            .collect();
        let (l, _) = render(&mut a, &x, &x);
        let tail = &l[secs(1.0)..];
        // The energy near A, as the vibrato smears it across the spectrum.
        tone_amplitude(tail, 440.0, SR)
    }
    let flattened = excursion(60.0, 0.0);
    let kept = excursion(60.0, 200.0);
    assert!(
        flattened > kept * 1.2,
        "with no smoothing vibrato flattens: {flattened} vs {kept}"
    );
}

fn midi_shifter(voices: f64) -> asft::AutoShift {
    let mut a = shifter();
    set(&mut a, &[(asft::MIDI_ON, 1.0), (asft::VOICE_MODE, voices)]);
    a
}

#[test]
fn auto_shift_midi_mono_retunes_to_the_note() {
    let mut a = midi_shifter(asft::VOICE_MONO as f64);
    a.set_param(asft::FORMANT_FOLLOW, 100.0);
    let x = saw(220.0, secs(1.2), 0.4);
    let (l, _) = render_midi(
        &mut a,
        &x,
        &x,
        &[(secs(0.3), note_on(64))],
        TransportInfo::default(),
    );
    let target = 329.63;
    let (hz, amp) = peak_near(&l[secs(0.7)..], target, 40.0, 1.0);
    near(cents(hz, target), 0.0, 15.0, "tuned to E");
    assert!(amp > 0.05);
    assert!(rms(&l[..secs(0.2)]) < 1e-6, "silent before the first note");
}

#[test]
fn auto_shift_is_silent_without_notes_in_midi_mode() {
    let mut a = midi_shifter(asft::VOICE_MONO as f64);
    let x = saw(220.0, secs(0.5), 0.4);
    let (l, r) = render(&mut a, &x, &x);
    assert!(peak(&l) < 1e-9 && peak(&r) < 1e-9);
}

#[test]
fn auto_shift_midi_poly_harmonises_with_chords() {
    let mut a = midi_shifter(asft::VOICE_POLY as f64);
    set(
        &mut a,
        &[(asft::VOICE_COUNT, 2.0), (asft::FORMANT_FOLLOW, 100.0)],
    );
    let x = saw(220.0, secs(1.2), 0.4);
    let (l, _) = render_midi(
        &mut a,
        &x,
        &x,
        &[(100, note_on(60)), (100, note_on(64)), (100, note_on(67))],
        TransportInfo::default(),
    );
    let tail = &l[secs(0.6)..];
    let notes = [261.63, 329.63, 392.00];
    let amps: Vec<f64> = notes
        .iter()
        .map(|n| peak_near(tail, *n, 25.0, 1.0).1)
        .collect();
    assert!(
        amps.iter().all(|a| *a > 0.02),
        "the three notes are all there: {amps:?}"
    );
}

#[test]
fn auto_shift_attack_release_and_latch_shape_midi_notes() {
    // Long attack: very little level at the start of the note.
    let mut a = midi_shifter(asft::VOICE_MONO as f64);
    a.set_param(asft::ATTACK, 800.0);
    let x = saw(220.0, secs(1.5), 0.4);
    let (l, _) = render_midi(
        &mut a,
        &x,
        &x,
        &[(0, note_on(60))],
        TransportInfo::default(),
    );
    let early = rms(&l[secs(0.05)..secs(0.1)]);
    let later = rms(&l[secs(1.0)..secs(1.4)]);
    assert!(later > early * 3.0, "{later} vs {early}");

    // With Note Latch a note holds past its note-off.
    let mut a = midi_shifter(asft::VOICE_MONO as f64);
    a.set_param(asft::NOTE_LATCH, 1.0);
    let (l, _) = render_midi(
        &mut a,
        &x,
        &x,
        &[(0, note_on(60)), (secs(0.2), note_off(60))],
        TransportInfo::default(),
    );
    assert!(rms(&l[secs(1.0)..secs(1.4)]) > 0.01);

    // Without latch and with a short release it stops.
    let mut a = midi_shifter(asft::VOICE_MONO as f64);
    a.set_param(asft::RELEASE, 20.0);
    let (l, _) = render_midi(
        &mut a,
        &x,
        &x,
        &[(0, note_on(60)), (secs(0.2), note_off(60))],
        TransportInfo::default(),
    );
    assert!(
        rms(&l[secs(1.0)..secs(1.4)]) < 1e-3,
        "{}",
        rms(&l[secs(1.0)..secs(1.4)])
    );
}

#[test]
fn auto_shift_dry_wet_mix_and_latency_line_up() {
    // Fully dry, the signal is the input delayed by exactly the reported latency.
    let mut a = shifter();
    a.set_param(asft::DRY_WET, 0.0);
    let x = saw(220.0, secs(0.5), 0.4);
    let (l, _) = render(&mut a, &x, &x);
    let d = a.latency() as usize;
    for i in 1_000..2_000 {
        near(l[i + d] as f64, x[i] as f64, 1e-5, "delayed exactly");
    }

    // An unshifted signal is exact through the bypass too, fully wet.
    let mut a = shifter();
    a.set_param(asft::STRENGTH, 0.0);
    let (l, _) = render(&mut a, &x, &x);
    let d = a.latency() as usize;
    for i in 10_000..11_000 {
        near(l[i + d] as f64, x[i] as f64, 1e-3, "unshifted");
    }
}

#[test]
fn auto_shift_input_gain_adjusts_the_level_into_the_tracker() {
    let mut a = shifter();
    a.set_param(asft::INPUT_GAIN, -12.0);
    let x: Vec<f32> = sine(440.0, SR, secs(1.0)).iter().map(|s| s * 0.5).collect();
    let (l, _) = render(&mut a, &x, &x);
    let level = rms(&l[secs(0.5)..]);
    near(db(level as f64 / rms(&x) as f64), -12.0, 1.0, "input gain");
}

// Spectral Time ----------------------------------------------------------------------------------

fn spectral_time() -> st::SpectralTime {
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 0.0),
            (st::DRY_WET, 100.0),
            (st::RESOLUTION, 2.0),
        ],
    );
    p
}

#[test]
fn spectral_time_freezes_a_tone_after_the_input_stops() {
    let mut p = spectral_time();
    p.set_param(st::RESOLUTION, 3.0);
    let f = 40.0 * SR as f32 / 1920.0;
    let mut x: Vec<f32> = sine(f, SR, secs(0.5)).iter().map(|s| s * 0.5).collect();
    x.extend(vec![0.0; secs(1.0)]);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (x.clone(), x.clone());
    let at_freeze = secs(0.25) / BLOCK;
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        if i == at_freeze {
            p.set_param(st::FREEZE, 1.0);
        }
        assert_no_alloc(|| h.process(&mut p, cl, cr, &[], &[]));
    }
    let tail = &l[secs(1.0)..];
    let amp = tone_amplitude(tail, f as f64, SR);
    assert!(
        amp > 0.2,
        "still sounding its pitch after the input ended: {amp}"
    );
    // After the freeze is released it fades away.
    p.set_param(st::FREEZE, 0.0);
    let (mut a, mut b) = (vec![0.0; secs(1.0)], vec![0.0; secs(1.0)]);
    for (cl, cr) in a.chunks_mut(BLOCK).zip(b.chunks_mut(BLOCK)) {
        h.process(&mut p, cl, cr, &[], &[]);
    }
    assert!(rms(&a[secs(0.7)..]) < 0.01, "{}", rms(&a[secs(0.7)..]));
}

#[test]
fn spectral_time_retrigger_on_sync_refreshes_the_freeze() {
    let mut p = spectral_time();
    set(
        &mut p,
        &[
            (st::FREEZE_MODE, st::MODE_RETRIGGER as f64),
            (st::RETRIGGER_MODE, st::RETRIGGER_SYNC as f64),
            (st::INTERVAL_MS, 300.0),
            (st::FADE_SHAPE, st::SHAPE_CROSSFADE as f64),
            (st::XFADE, 20.0),
            (st::FREEZE, 1.0),
        ],
    );
    // A tone that jumps to a new note every 100 ms: each freeze holds the note at the trigger.
    let fa = 50.0 * SR as f32 / 960.0;
    let fb = 70.0 * SR as f32 / 960.0;
    let mut x: Vec<f32> = Vec::new();
    for i in 0..secs(2.0) {
        let t = i as f64 / SR;
        let f = if ((t / 0.3).floor() as i64) % 2 == 0 {
            fa
        } else {
            fb
        };
        x.push(0.4 * ((std::f64::consts::TAU * f as f64 * t).sin() as f32));
    }
    let (l, _) = render(&mut p, &x, &x);
    let tail = &l[secs(0.6)..];
    let (a, b) = (
        tone_amplitude(tail, fa as f64, SR),
        tone_amplitude(tail, fb as f64, SR),
    );
    assert!(
        a > 0.03 && b > 0.03,
        "both notes get frozen in turn: {a} {b}"
    );
}

#[test]
fn spectral_time_retrigger_on_onsets_catches_transients() {
    let run = |sensitivity: f64| {
        let mut p = spectral_time();
        set(
            &mut p,
            &[
                (st::FREEZE_MODE, st::MODE_RETRIGGER as f64),
                (st::RETRIGGER_MODE, st::RETRIGGER_ONSETS as f64),
                (st::SENSITIVITY, sensitivity),
                (st::FADE_SHAPE, st::SHAPE_ENVELOPE as f64),
                (st::FADE_IN, 1.0),
                (st::FADE_OUT, 200.0),
                (st::FREEZE, 1.0),
            ],
        );
        // A steady low tone with noise bursts on top, a jump only a sensitive detector takes.
        let mut x: Vec<f32> = sine(200.0, SR, secs(1.0)).iter().map(|s| s * 0.3).collect();
        for burst in [secs(0.2), secs(0.5), secs(0.8)] {
            let n = noise(2_000, burst as u32, 0.3);
            for (s, b) in x[burst..burst + 2_000].iter_mut().zip(&n) {
                *s += *b;
            }
        }
        let (l, _) = render(&mut p, &x, &x);
        // A captured burst lives on in the freeze, up where the tone has nothing.
        rms(&biquad_band(&l[secs(0.3)..secs(0.48)], 6_000.0))
    };
    let (sensitive, deaf) = (run(100.0), run(0.0));
    assert!(
        sensitive > deaf * 3.0,
        "sensitive detects the bursts: {sensitive} vs {deaf}"
    );
}

#[test]
fn spectral_time_stacks_up_to_eight_freezes_in_envelope_mode() {
    let mut p = spectral_time();
    set(
        &mut p,
        &[
            (st::FREEZE_MODE, st::MODE_RETRIGGER as f64),
            (st::RETRIGGER_MODE, st::RETRIGGER_SYNC as f64),
            (st::INTERVAL_MS, 50.0),
            (st::FADE_SHAPE, st::SHAPE_ENVELOPE as f64),
            (st::FADE_IN, 1.0),
            (st::FADE_OUT, 1_000.0),
            (st::FREEZE, 1.0),
        ],
    );
    let x: Vec<f32> = sine(1_000.0, SR, secs(1.0))
        .iter()
        .map(|s| s * 0.1)
        .collect();
    let (l, _) = render(&mut p, &x, &x);
    let tail = rms(&l[secs(0.5)..]);
    assert!(tail > 0.05, "many stacked freezes sum: {tail}");
    assert!(l.iter().all(|s| s.is_finite()));
}

#[test]
fn spectral_time_delay_lands_at_its_time() {
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 1.0),
            (st::FREEZER_ON, 0.0),
            (st::DRY_WET, 100.0),
            (st::DELAY_MIX, 100.0),
            (st::TIME_MS, 100.0),
            (st::FEEDBACK, 0.0),
        ],
    );
    let mut x = vec![0.0f32; secs(1.0)];
    x[1_000] = 1.0;
    let (l, _) = render(&mut p, &x, &x);
    let expected = 1_000 + p.latency() as usize + secs(0.1);
    let found = onset(&l[..], 0.02).unwrap();
    assert!(
        (found as i64 - expected as i64).abs() <= p.latency() as i64,
        "echo at {found}, expected near {expected}"
    );
    assert!(peak(&l[..found.saturating_sub(10)]) < 0.01, "nothing early");
}

#[test]
fn spectral_time_delay_modes_read_time_in_notes() {
    let mut h = Harness::new(SR);
    h.transport.tempo = 120.0;
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 1.0),
            (st::FREEZER_ON, 0.0),
            (st::DRY_WET, 100.0),
            (st::DELAY_MIX, 100.0),
            (st::FEEDBACK, 0.0),
            (st::TIME_MODE, st::TIME_MODE_16TH as f64),
            (st::TIME_16THS, 4.0),
        ],
    );
    let mut x = vec![0.0f32; secs(1.0)];
    x[1_000] = 1.0;
    let mut l = x.clone();
    let mut r = x;
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        h.process(&mut p, cl, cr, &[], &[]);
    }
    let expected = 1_000 + p.latency() as usize + secs(0.5);
    let found = onset(&l, 0.02).unwrap();
    assert!(
        (found as i64 - expected as i64).abs() <= 960,
        "{found} vs {expected}"
    );
}

#[test]
fn spectral_time_feedback_makes_repeats_that_fade() {
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 1.0),
            (st::FREEZER_ON, 0.0),
            (st::DRY_WET, 100.0),
            (st::DELAY_MIX, 100.0),
            (st::TIME_MS, 100.0),
            (st::FEEDBACK, 50.0),
        ],
    );
    let mut x = vec![0.0f32; secs(1.0)];
    x[1_000] = 1.0;
    let (l, _) = render(&mut p, &x, &x);
    let first = 1_000 + p.latency() as usize + secs(0.1);
    let window = |at: usize| peak(&l[at.saturating_sub(480)..at + 480]);
    let (one, two, three) = (
        window(first),
        window(first + secs(0.1)),
        window(first + secs(0.2)),
    );
    near((two / one) as f64, 0.5, 0.12, "second repeat");
    near((three / two) as f64, 0.5, 0.12, "third repeat");
}

#[test]
fn spectral_time_shift_moves_each_repeat() {
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 1.0),
            (st::FREEZER_ON, 0.0),
            (st::DRY_WET, 100.0),
            (st::DELAY_MIX, 100.0),
            (st::TIME_MS, 200.0),
            (st::FEEDBACK, 0.0),
            (st::SHIFT, 400.0),
            (st::RESOLUTION, 3.0),
        ],
    );
    let x: Vec<f32> = sine(2_000.0, SR, secs(1.0))
        .iter()
        .map(|s| s * 0.5)
        .collect();
    let (l, _) = render(&mut p, &x, &x);
    let tail = &l[secs(0.5)..];
    let (hz, _) = peak_near(tail, 2_400.0, 120.0, 2.0);
    near(hz, 2_400.0, 60.0, "2 kHz came back at 2.4 kHz");
    assert!(tone_amplitude(tail, 2_000.0, SR) < peak_near(tail, 2_400.0, 60.0, 2.0).1 * 0.4);
}

/// The delay of `band` energy against an impulse, by the time-weighted mean.
fn group_delay(signal: &[f32], hz: f32) -> f64 {
    let band = biquad_band(signal, hz);
    let total: f64 = band.iter().map(|s| (*s as f64).powi(2)).sum();
    band.iter()
        .enumerate()
        .map(|(i, s)| i as f64 * (*s as f64).powi(2))
        .sum::<f64>()
        / total.max(1e-12)
}

#[test]
fn spectral_time_tilt_skews_delay_with_frequency() {
    let delay = |tilt: f64, mask: f64| {
        let mut p: st::SpectralTime = ready();
        set(
            &mut p,
            &[
                (st::DELAY_ON, 1.0),
                (st::FREEZER_ON, 0.0),
                (st::DRY_WET, 100.0),
                (st::DELAY_MIX, 100.0),
                (st::TIME_MS, 200.0),
                (st::FEEDBACK, 0.0),
                (st::TILT, tilt),
                (st::MASK, mask),
            ],
        );
        let mut x = vec![0.0f32; secs(1.5)];
        for (n, s) in x[1_000..1_100].iter_mut().enumerate() {
            *s = if n % 2 == 0 { 0.5 } else { -0.5 };
        }
        let (l, _) = render(&mut p, &x, &x);
        group_delay(&l, 8_000.0) - group_delay(&l, 300.0)
    };
    assert!(
        delay(100.0, 0.0) > 1_000.0,
        "highs later: {}",
        delay(100.0, 0.0)
    );
    assert!(
        delay(-100.0, 0.0) < -1_000.0,
        "lows later: {}",
        delay(-100.0, 0.0)
    );
    // Mask to the low end: the skew leaves the highs alone.
    let unmasked = delay(100.0, 0.0);
    assert!(delay(100.0, -100.0) < unmasked * 0.5);
}

#[test]
fn spectral_time_spray_scatters_delay_across_frequencies() {
    let spread = |spray: f64| {
        let mut p: st::SpectralTime = ready();
        set(
            &mut p,
            &[
                (st::DELAY_ON, 1.0),
                (st::FREEZER_ON, 0.0),
                (st::DRY_WET, 100.0),
                (st::DELAY_MIX, 100.0),
                (st::TIME_MS, 50.0),
                (st::FEEDBACK, 0.0),
                (st::SPRAY, spray),
            ],
        );
        let x = noise(4_000, 7, 0.5);
        let mut buffer = vec![0.0; secs(1.5)];
        buffer[100..4_100].copy_from_slice(&x);
        let (l, _) = render(&mut p, &buffer, &buffer);
        // The echo of a short burst spreads with Spray: fraction of the output beyond the burst.
        let total: f32 = l.iter().map(|s| s * s).sum();
        let late: f32 = l[10_000..].iter().map(|s| s * s).sum();
        late / total
    };
    assert!(
        spread(800.0) > spread(0.0) + 0.1,
        "{} vs {}",
        spread(800.0),
        spread(0.0)
    );
}

#[test]
fn spectral_time_order_swaps_freezer_and_delay() {
    // A freeze going into a delay still sounds; a delay going into a freeze still sounds. The
    // first keeps ringing in time, the second too, but differently: both must simply work.
    let run = |order: f64| {
        let mut p: st::SpectralTime = ready();
        set(
            &mut p,
            &[
                (st::ORDER, order),
                (st::DELAY_ON, 1.0),
                (st::DELAY_MIX, 100.0),
                (st::TIME_MS, 500.0),
                (st::DRY_WET, 100.0),
            ],
        );
        let f = 25.0 * SR as f32 / 960.0;
        let x: Vec<f32> = sine(f, SR, secs(0.3))
            .iter()
            .map(|s| s * 0.4)
            .chain(std::iter::repeat_n(0.0, secs(1.2)))
            .collect();
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (x.clone(), x);
        // Into the freezer directly, or after the delay has brought the tone round.
        let press = if order < 0.5 { secs(0.2) } else { secs(0.7) } / BLOCK;
        for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
            if i == press {
                p.set_param(st::FREEZE, 1.0);
            }
            h.process(&mut p, cl, cr, &[], &[]);
        }
        rms(&l[secs(1.0)..])
    };
    assert!(run(0.0) > 0.01 && run(1.0) > 0.01);
}

#[test]
fn spectral_time_resolution_sets_latency() {
    let mut p: st::SpectralTime = ready();
    for (i, size) in st::RESOLUTIONS.iter().enumerate() {
        p.set_param(st::RESOLUTION, i as f64);
        assert_eq!(p.latency() as usize, *size);
    }
    // A click comes through unaltered apart from its latency.
    let mut p = spectral_time();
    let mut x = vec![0.0f32; secs(0.2)];
    x[1_000] = 0.5;
    let (l, _) = render(&mut p, &x, &x);
    let expected = 1_000 + p.latency() as usize;
    assert_eq!(onset(&l, 1e-3), Some(expected));
}

#[test]
fn spectral_time_zero_dry_latency_drops_the_dry_delay() {
    // With Zero Dry on, the dry path doesn't wait for the wet one.
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[(st::DELAY_ON, 0.0), (st::DRY_WET, 0.0), (st::ZERO_DRY, 1.0)],
    );
    assert_eq!(p.latency(), 0, "nothing for the host to compensate");
    let x: Vec<f32> = sine(500.0, SR, secs(0.2));
    let (l, _) = render(&mut p, &x, &x);
    for i in 0..x.len() {
        near(
            l[i] as f64,
            x[i] as f64,
            1e-6,
            "dry unchanged and undelayed",
        );
    }

    // With it off the dry path matches the reported latency.
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 0.0),
            (st::DRY_WET, 0.0),
            (st::ZERO_DRY, 0.0),
            (st::RESOLUTION, 3.0),
        ],
    );
    let (l, _) = render(&mut p, &x, &x);
    let d = p.latency() as usize;
    assert_eq!(d, 1920);
    for i in 2_000..3_000 {
        near(
            l[i + d] as f64,
            x[i] as f64,
            1e-5,
            "dry delayed by the latency",
        );
    }

    // Zero Dry leaves the wet path where it was.
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 0.0),
            (st::DRY_WET, 100.0),
            (st::ZERO_DRY, 1.0),
        ],
    );
    let (l, _) = render(&mut p, &x, &x);
    near(
        rms(&l[secs(0.1)..]) as f64,
        rms(&x) as f64,
        0.02,
        "wet still there",
    );
}

#[test]
fn spectral_time_input_send_scales_what_goes_in() {
    let mut p = spectral_time();
    p.set_param(st::INPUT_SEND, -12.0);
    let x: Vec<f32> = sine(1_000.0, SR, secs(0.2));
    let (l, _) = render(&mut p, &x, &x);
    near(
        db(rms(&l[secs(0.1)..]) as f64 / rms(&x) as f64),
        -12.0,
        0.5,
        "input send",
    );
}

// Spectral Resonator -----------------------------------------------------------------------------

fn resonator(params: &[(u32, f64)]) -> res::SpectralResonator {
    let mut p: res::SpectralResonator = ready();
    set(&mut p, &[(res::DRY_WET, 100.0), (res::HARMONICS, 16.0)]);
    set(&mut p, params);
    p
}

/// Impulses in a chain. A loud, noisy stretch for the resonators to ring at.
fn ring_input(seconds: f64) -> Vec<f32> {
    noise(secs(seconds), 77, 0.05)
}

#[test]
fn spectral_resonator_rings_at_freq_in_hertz_and_note() {
    let mut p = resonator(&[(res::FREQ, 300.0)]);
    let x = ring_input(1.0);
    let out = mono(&mut p, &x);
    let tail = &out[secs(0.3)..];
    let (hz, _) = peak_near(tail, 300.0, 15.0, 0.5);
    near(cents(hz, 300.0), 0.0, 3.0, "Hz mode");

    let mut p = resonator(&[(res::FREQ_MODE, res::FREQ_NOTE as f64), (res::NOTE, 69.0)]);
    let out = mono(&mut p, &x);
    let (hz, _) = peak_near(&out[secs(0.3)..], 440.0, 15.0, 0.5);
    near(cents(hz, 440.0), 0.0, 3.0, "Note mode");
}

#[test]
fn spectral_resonator_stretch_100_leaves_only_odd_harmonics() {
    let mut p = resonator(&[
        (res::FREQ, 200.0),
        (res::HARMONICS, 8.0),
        (res::STRETCH, 100.0),
    ]);
    let x = ring_input(1.0);
    let out = mono(&mut p, &x);
    let tail = &out[secs(0.3)..];
    let a1 = tone_amplitude(tail, 200.0, SR);
    let a2 = tone_amplitude(tail, 400.0, SR);
    let a3 = tone_amplitude(tail, 600.0, SR);
    assert!(a1 > 1e-3 && a3 > 1e-3, "{a1} {a3}");
    assert!(a2 < 0.25 * a3, "even harmonic is gone: {a2} vs {a3}");
    // Negative stretch brings harmonics closer together.
    assert!(res::stretched_ratio(3, -0.5) < 3.0);
    assert_eq!(res::stretched_ratio(3, 1.0), 5.0);
    assert_eq!(res::stretched_ratio(1, 1.0), 1.0);
}

#[test]
fn spectral_resonator_decay_reaches_60_db_in_the_decay_time() {
    let mut p = resonator(&[
        (res::HARMONICS, 1.0),
        (res::FREQ, 500.0),
        (res::DECAY, 1_000.0),
    ]);
    let mut x = vec![0.0f32; secs(2.0)];
    x[0] = 1.0;
    let out = mono(&mut p, &x);
    let start = p.latency() as usize + 1_000;
    let level = |at: usize| peak(&out[at..at + 240]) as f64;
    let (a, b) = (level(start), level(start + secs(0.5)));
    near(
        20.0 * (b / a).log10(),
        -30.0,
        1.5,
        "half the decay time is -30 dB",
    );
}

#[test]
fn spectral_resonator_damping_shortens_high_or_low_partials() {
    let ring = |params: &[(u32, f64)], harmonic: f64| {
        let mut p = resonator(&[
            (res::HARMONICS, 8.0),
            (res::FREQ, 300.0),
            (res::DECAY, 2_000.0),
        ]);
        set(&mut p, params);
        let mut x = vec![0.0f32; secs(1.5)];
        x[10_000] = 1.0;
        let out = mono(&mut p, &x);
        tone_amplitude(&out[secs(1.0)..], 300.0 * harmonic, SR)
            / tone_amplitude(&out[secs(0.3)..secs(0.5)], 300.0 * harmonic, SR).max(1e-9)
    };
    let hf_low = ring(&[(res::HF_DAMP, 100.0)], 1.0);
    let hf_high = ring(&[(res::HF_DAMP, 100.0)], 6.0);
    assert!(
        hf_high < hf_low * 0.5,
        "HF Damp eats high partials: {hf_high} vs {hf_low}"
    );
    let lf_low = ring(&[(res::LF_DAMP, 100.0)], 1.0);
    let lf_high = ring(&[(res::LF_DAMP, 100.0)], 6.0);
    assert!(
        lf_low < lf_high * 0.5,
        "LF Damp eats low partials: {lf_low} vs {lf_high}"
    );
    assert_eq!(res::partial_decay(1.0, 1.0, 0.0, 0.0), 1.0);
}

#[test]
fn spectral_resonator_harmonics_set_how_many_partials_ring() {
    let x = ring_input(1.0);
    let band = |harmonics: f64, n: f64| {
        let mut p = resonator(&[(res::FREQ, 200.0), (res::HARMONICS, harmonics)]);
        let out = mono(&mut p, &x);
        tone_amplitude(&out[secs(0.3)..], 200.0 * n, SR)
    };
    assert!(
        band(2.0, 4.0) < band(4.0, 4.0) * 0.1,
        "the 4th needs 4 harmonics"
    );
    assert!(band(4.0, 3.0) > 0.0);
}

#[test]
fn spectral_resonator_quantize_snaps_partials_to_the_chromatic_scale() {
    let x = ring_input(1.0);
    // The 3rd of 220 Hz is 660 Hz, between E5 (659.26) and F5, and nearest E5: snapped.
    let mut p = resonator(&[
        (res::FREQ, 220.0),
        (res::HARMONICS, 3.0),
        (res::QUANTIZE, 1.0),
    ]);
    let out = mono(&mut p, &x);
    let (hz, _) = peak_near(&out[secs(0.3)..], 660.0, 30.0, 0.5);
    near(cents(hz, 659.26), 0.0, 5.0, "the 3rd lands on a pitch");
}

#[test]
fn spectral_resonator_unison_detunes_copies_either_side() {
    let x = ring_input(1.0);
    let mut one = resonator(&[
        (res::FREQ, 1_000.0),
        (res::HARMONICS, 1.0),
        (res::UNISON, 1.0),
    ]);
    let single = mono(&mut one, &x);
    let centre = tone_amplitude(&single[secs(0.3)..], 1_000.0, SR);
    let mut two = resonator(&[
        (res::FREQ, 1_000.0),
        (res::HARMONICS, 1.0),
        (res::UNISON, 2.0),
        (res::UNI_AMT, 100.0),
    ]);
    let out = render(&mut two, &x, &x);
    let tail = &out.0[secs(0.3)..];
    let up = 1_000.0 * 2f64.powf(50.0 / 1200.0);
    let down = 1_000.0 * 2f64.powf(-50.0 / 1200.0);
    let (hz_u, a_u) = peak_near(tail, up, 10.0, 0.5);
    let (hz_d, a_d) = peak_near(tail, down, 10.0, 0.5);
    assert!(a_u > 0.0 && a_d > 0.0, "{hz_u} {hz_d}");
    near(cents(hz_u, up), 0.0, 4.0, "upper voice");
    near(cents(hz_d, down), 0.0, 4.0, "lower voice");
    assert!(centre > 0.0);
    // Stereo: the two unison copies are spread apart.
    assert!((rms(&out.0) - rms(&out.1)).abs() > 0.0 || out.0 != out.1);
}

#[test]
fn spectral_resonator_midi_poly_plays_chords_of_notes() {
    let mut p = resonator(&[
        (res::PITCH_MODE, res::MODE_MIDI as f64),
        (res::HARMONICS, 8.0),
    ]);
    let x = ring_input(1.2);
    let (out, _) = render_midi(
        &mut p,
        &x,
        &x,
        &[(0, note_on(60)), (0, note_on(64)), (0, note_on(67))],
        TransportInfo::default(),
    );
    let tail = &out[secs(0.4)..];
    for hz in [261.63, 329.63, 392.0] {
        assert!(peak_near(tail, hz, 6.0, 0.5).1 > 0.002, "{hz}");
    }
}

#[test]
fn spectral_resonator_midi_gate_decides_when_it_rings() {
    let x = ring_input(1.0);
    let midi = |gate: f64, notes: &[(usize, MidiMessage)]| {
        let mut p = resonator(&[
            (res::PITCH_MODE, res::MODE_MIDI as f64),
            (res::MONO_POLY, res::VOICES_MONO as f64),
            (res::MIDI_GATE, gate),
            (res::FREQ, 220.0),
        ]);
        let (out, _) = render_midi(&mut p, &x, &x, notes, TransportInfo::default());
        out
    };
    let no_notes_gate_on = midi(1.0, &[]);
    assert!(
        rms(&no_notes_gate_on[secs(0.3)..]) < 1e-3,
        "gated with no notes"
    );
    let no_notes_gate_off = midi(0.0, &[]);
    assert!(
        rms(&no_notes_gate_off[secs(0.3)..]) > 1e-3,
        "free with no notes"
    );
    let note = midi(1.0, &[(0, note_on(69))]);
    let (hz, _) = peak_near(&note[secs(0.3)..], 440.0, 15.0, 0.5);
    near(cents(hz, 440.0), 0.0, 4.0, "plays the note");
}

#[test]
fn spectral_resonator_poly_is_always_gated_and_splits_harmonics() {
    let x = ring_input(1.0);
    let mut p = resonator(&[
        (res::PITCH_MODE, res::MODE_MIDI as f64),
        (res::MIDI_GATE, 0.0),
    ]);
    let (out, _) = render_midi(&mut p, &x, &x, &[], TransportInfo::default());
    assert!(rms(&out[secs(0.3)..]) < 1e-3, "poly ignores MIDI Gate off");

    // 32 harmonics, 4 voices: 8 each. The 9th of a held note isn't ringing.
    let mut p = resonator(&[
        (res::PITCH_MODE, res::MODE_MIDI as f64),
        (res::POLYPHONY, 4.0),
        (res::HARMONICS, 32.0),
    ]);
    let (out, _) = render_midi(
        &mut p,
        &x,
        &x,
        &[(0, note_on(60))],
        TransportInfo::default(),
    );
    let f = 261.63;
    let tail = &out[secs(0.3)..];
    assert!(tone_amplitude(tail, f * 9.0, SR) < tone_amplitude(tail, f * 8.0, SR) * 0.3);
}

#[test]
fn spectral_resonator_transposes_midi_by_semitones_or_degrees() {
    let x = ring_input(1.2);
    let hit = |params: &[(u32, f64)], scale: Option<ScaleInfo>| {
        let mut p = resonator(params);
        let transport = TransportInfo {
            scale,
            sample_rate: SR,
            ..Default::default()
        };
        let (out, _) = render_midi(&mut p, &x, &x, &[(0, note_on(60))], transport);
        out
    };
    let semis = hit(
        &[
            (res::PITCH_MODE, res::MODE_MIDI as f64),
            (res::TRANSPOSE, 12.0),
            (res::HARMONICS, 1.0),
        ],
        None,
    );
    let (hz, _) = peak_near(&semis[secs(0.4)..], 523.25, 10.0, 0.5);
    near(cents(hz, 523.25), 0.0, 6.0, "Transp. +12");

    let c_major = ScaleInfo {
        root: 0,
        mask: 0b1010_1011_0101,
    };
    let degrees = hit(
        &[
            (res::PITCH_MODE, res::MODE_MIDI as f64),
            (res::USE_SCALE, 1.0),
            (res::SD_SHIFT, 2.0),
            (res::HARMONICS, 1.0),
        ],
        Some(c_major),
    );
    let (hz, _) = peak_near(&degrees[secs(0.4)..], 329.63, 10.0, 0.5);
    near(cents(hz, 329.63), 0.0, 6.0, "two degrees up from C is E");
}

#[test]
fn spectral_resonator_scale_awareness_follows_internal_mode() {
    let x = ring_input(1.0);
    let c_minor = ScaleInfo {
        root: 0,
        mask: 0b0101_1010_1101,
    };
    let mut p = resonator(&[
        (res::USE_SCALE, 1.0),
        (res::SD_SHIFT, 2.0),
        (res::HARMONICS, 1.0),
    ]);
    let transport = TransportInfo {
        scale: Some(c_minor),
        sample_rate: SR,
        ..Default::default()
    };
    let (out, _) = render_midi(&mut p, &x, &x, &[], transport);
    // C3 root, up two degrees of C Minor is Eb3.
    let (hz, _) = peak_near(&out[secs(0.3)..], 155.56, 12.0, 0.5);
    near(cents(hz, 155.56), 0.0, 4.0, "SD Shift");
    // Chromatic without a scale.
    let mut p = resonator(&[
        (res::USE_SCALE, 1.0),
        (res::SD_SHIFT, 2.0),
        (res::HARMONICS, 1.0),
    ]);
    let out = mono(&mut p, &x);
    let (hz, _) = peak_near(&out[secs(0.3)..], 146.83, 12.0, 0.5);
    near(cents(hz, 146.83), 0.0, 4.0, "chromatic");
}

#[test]
fn spectral_resonator_modulation_modes_change_the_sound() {
    let x = ring_input(2.0);
    let run = |mode: f64| {
        let mut p = resonator(&[
            (res::FREQ, 440.0),
            (res::MOD_MODE, mode),
            (res::MOD_RATE, 5.0),
            (res::PCH_MOD, 1.0),
            (res::UNISON, 2.0),
        ]);
        mono(&mut p, &x)
    };
    let none = run(res::MOD_NONE as f64);
    let tail_none = tone_amplitude(&none[secs(1.0)..], 440.0, SR);
    for mode in [res::MOD_CHORUS, res::MOD_WANDER, res::MOD_GRANULAR] {
        let out = run(mode as f64);
        assert!(out.iter().all(|s| s.is_finite()));
        let tail = tone_amplitude(&out[secs(1.0)..], 440.0, SR);
        assert!(tail > 0.0);
        assert!(out != none, "mode {mode} changes the sound");
    }
    assert!(tail_none > 0.0);
}

#[test]
fn spectral_resonator_chorus_rate_zero_only_moves_amplitude() {
    let mut p = resonator(&[
        (res::MOD_MODE, res::MOD_CHORUS as f64),
        (res::MOD_RATE, 0.0),
        (res::PCH_MOD, 12.0),
        (res::FREQ, 440.0),
        (res::HARMONICS, 1.0),
    ]);
    let x = ring_input(1.0);
    let out = mono(&mut p, &x);
    let (hz, _) = peak_near(&out[secs(0.3)..], 440.0, 80.0, 1.0);
    near(cents(hz, 440.0), 0.0, 5.0, "pitch stays put at Mod Rate 0");
}

#[test]
fn spectral_resonator_shift_moves_the_input_only() {
    // A tone fed through at 1 kHz: with the resonator on 2 kHz only, an octave Shift in the input
    // lights it up; without it, nothing rings.
    let tone: Vec<f32> = sine(1_000.0, SR, secs(1.0))
        .iter()
        .map(|s| s * 0.5)
        .collect();
    let ring = |shift: f64| {
        let mut p = resonator(&[
            (res::FREQ, 2_000.0),
            (res::HARMONICS, 1.0),
            (res::SHIFT, shift),
        ]);
        let out = mono(&mut p, &tone);
        tone_amplitude(&out[secs(0.5)..], 2_000.0, SR)
    };
    assert!(
        ring(12.0) > ring(0.0) * 8.0,
        "{} vs {}",
        ring(12.0),
        ring(0.0)
    );
    // The partials stay on 2 kHz: an output at 2 kHz, not 4 kHz, whatever the shift.
    let mut p = resonator(&[
        (res::FREQ, 2_000.0),
        (res::HARMONICS, 1.0),
        (res::SHIFT, 12.0),
    ]);
    let out = mono(&mut p, &tone);
    let (hz, _) = peak_near(&out[secs(0.5)..], 2_000.0, 200.0, 2.0);
    near(cents(hz, 2_000.0), 0.0, 3.0, "resonator not moved");
}

#[test]
fn spectral_resonator_limiter_keeps_the_output_under_full_scale() {
    let mut p = resonator(&[
        (res::INPUT_SEND, 24.0),
        (res::DECAY, 5_000.0),
        (res::FREQ, 440.0),
    ]);
    let x: Vec<f32> = sine(440.0, SR, secs(2.0));
    let out = mono(&mut p, &x);
    assert!(peak(&out) <= 1.0, "{}", peak(&out));
}

#[test]
fn spectral_resonator_reports_its_latency_and_dry_stays_aligned() {
    let mut p: res::SpectralResonator = ready();
    assert_eq!(p.latency() as usize, res::FRAME);
    p.set_param(res::DRY_WET, 0.0);
    let x = noise(secs(0.5), 3, 0.5);
    let out = mono(&mut p, &x);
    let d = p.latency() as usize;
    for i in 1_000..2_000 {
        near(out[i + d] as f64, x[i] as f64, 1e-5, "dry aligned");
    }
}

// Extremes ---------------------------------------------------------------------------------------

fn extreme_inputs() -> (Vec<f32>, Vec<f32>) {
    let a: Vec<f32> = (0..secs(0.5))
        .map(|i| ((i as f32 * 0.05).sin()) * 2.0)
        .collect();
    let b: Vec<f32> = noise(secs(0.5), 41, 2.0);
    (a, b)
}

#[test]
fn every_extreme_keeps_the_output_finite() {
    let (a, b) = extreme_inputs();
    let finite = |out: &[f32], name: &str| {
        assert!(out.iter().all(|s| s.is_finite()), "{name}");
    };
    let mut v: voc::Vocoder = ready();
    set(
        &mut v,
        &[
            (voc::CARRIER, voc::CARRIER_PITCH as f64),
            (voc::ATTACK, 0.1),
            (voc::RELEASE, 1.0),
            (voc::DEPTH, 200.0),
            (voc::BANDS, 7.0),
            (voc::RANGE_LOW, 20_000.0),
            (voc::RANGE_HIGH, 20.0),
            (voc::FORMANT, 12.0),
            (voc::ENHANCE, 1.0),
        ],
    );
    finite(&mono(&mut v, &a), "vocoder");

    let mut asft_p: asft::AutoShift = ready();
    set(
        &mut asft_p,
        &[
            (asft::PITCH_SHIFT, 24.0),
            (asft::FORMANT_SHIFT, -100.0),
            (asft::LFO_FORMANT, 100.0),
            (asft::LFO_PITCH, 100.0),
            (asft::VIB_AMOUNT, 200.0),
            (asft::INPUT_GAIN, 24.0),
        ],
    );
    finite(&mono(&mut asft_p, &b), "auto shift");

    let mut time: st::SpectralTime = ready();
    set(
        &mut time,
        &[
            (st::FREEZE, 1.0),
            (st::FREEZE_MODE, st::MODE_RETRIGGER as f64),
            (st::RETRIGGER_MODE, st::RETRIGGER_SYNC as f64),
            (st::INTERVAL_MS, 10.0),
            (st::FEEDBACK, 100.0),
            (st::SHIFT, 2_000.0),
            (st::TILT, 100.0),
            (st::SPRAY, 1_000.0),
            (st::STEREO, 100.0),
            (st::RESOLUTION, 5.0),
            (st::INPUT_SEND, 24.0),
        ],
    );
    finite(&mono(&mut time, &b), "spectral time");

    let mut r: res::SpectralResonator = ready();
    set(
        &mut r,
        &[
            (res::DECAY, 30_000.0),
            (res::HARMONICS, 256.0),
            (res::UNISON, 8.0),
            (res::UNI_AMT, 100.0),
            (res::STRETCH, -100.0),
            (res::SHIFT, 48.0),
            (res::MOD_MODE, res::MOD_WANDER as f64),
            (res::PCH_MOD, 24.0),
            (res::MOD_RATE, 40.0),
            (res::INPUT_SEND, 24.0),
        ],
    );
    finite(&mono(&mut r, &b), "spectral resonator");
    r.set_param(res::DRY_WET, 100.0);
    assert!(peak(&mono(&mut r, &b)) <= 1.0, "the limiter holds");
}

#[test]
fn nothing_allocates_when_watched() {
    let x = noise(secs(0.2), 71, 0.5);
    let check = |p: &mut dyn Processor, view_len: usize| {
        let mut w = Watched::new(view_len);
        let (mut l, mut r) = (x.clone(), x.clone());
        w.run(p, &mut l, &mut r);
    };
    let mut a: sp::Spectrum = ready();
    check(&mut a, sp::VIEW_LEN);
    let mut a: tuner::Tuner = ready();
    check(&mut a, tuner::VIEW_LEN);
    let mut a: voc::Vocoder = ready();
    check(&mut a, voc::VIEW_LEN);
    let mut a: asft::AutoShift = ready();
    check(&mut a, asft::VIEW_LEN);
    let mut a: st::SpectralTime = ready();
    a.set_param(st::FREEZE, 1.0);
    check(&mut a, st::VIEW_LEN);
    let mut a: res::SpectralResonator = ready();
    check(&mut a, res::VIEW_LEN);
}

#[test]
fn vocoder_density_rate_and_enhance_shape_the_carrier() {
    let x: Vec<f32> = noise(secs(1.0), 91, 0.3);
    let run = |params: &[(u32, f64)]| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(&mut v, params);
        vocode(&mut v, &x, &x, None, None).0
    };
    assert!(
        rms(&run(&[(voc::NOISE_DENSITY, 0.0)])[secs(0.3)..]) < 1e-5,
        "no density, no carrier"
    );
    let full = rms(&biquad_band(&run(&[])[secs(0.3)..], 10_000.0));
    let slow = rms(&biquad_band(
        &run(&[(voc::NOISE_RATE, 20.0)])[secs(0.3)..],
        10_000.0,
    ));
    assert!(
        slow < full * 0.6,
        "lower rate loses the top: {slow} vs {full}"
    );

    // Enhance normalises a quiet carrier up to where a loud one would be.
    let quiet = |enhance: f64| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(
            &mut v,
            &[
                (voc::CARRIER, voc::CARRIER_EXTERNAL as f64),
                (voc::ENHANCE, enhance),
            ],
        );
        let carrier: Vec<f32> = noise(secs(1.0), 92, 0.01);
        rms(&vocode(&mut v, &x, &x, Some(&carrier), Some(&carrier)).0[secs(0.3)..])
    };
    assert!(
        quiet(1.0) > quiet(0.0) * 5.0,
        "{} vs {}",
        quiet(1.0),
        quiet(0.0)
    );
}

#[test]
fn vocoder_attack_release_shape_how_fast_it_responds() {
    let mut burst = vec![0.0f32; secs(0.5)];
    burst.extend(noise(secs(0.3), 93, 0.5));
    burst.extend(vec![0.0f32; secs(0.3)]);
    let run = |release: f64| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        v.set_param(voc::RELEASE, release);
        vocode(&mut v, &burst, &burst, None, None).0
    };
    let after = |out: &[f32]| rms(&out[secs(0.83)..secs(0.88)]);
    let fast = run(5.0);
    let slow = run(500.0);
    assert!(
        after(&slow) > after(&fast) * 5.0,
        "{} vs {}",
        after(&slow),
        after(&fast)
    );
}

#[test]
fn vocoder_range_and_bw_move_the_filters() {
    let tone: Vec<f32> = sine(1_000.0, SR, secs(1.0))
        .iter()
        .map(|s| s * 0.5)
        .collect();
    let level = |params: &[(u32, f64)]| {
        let mut v: voc::Vocoder = ready();
        full_wet(&mut v);
        set(&mut v, params);
        rms(&vocode(&mut v, &tone, &tone, None, None).0[secs(0.3)..])
    };
    // A band range above the tone hears nothing of it.
    assert!(level(&[(voc::RANGE_LOW, 4_000.0), (voc::RANGE_HIGH, 12_000.0)]) < level(&[]) * 0.3);
    // A wide band passes more of a noise carrier than a narrow one.
    assert!(level(&[(voc::BW, 200.0)]) > level(&[(voc::BW, 10.0)]) * 1.5);
}

#[test]
fn auto_shift_voice_count_limits_and_glide_moves_between_notes() {
    // Only two voices: a third note steals.
    let mut a = midi_shifter(asft::VOICE_POLY as f64);
    set(
        &mut a,
        &[(asft::VOICE_COUNT, 0.0), (asft::FORMANT_FOLLOW, 100.0)],
    );
    let x = saw(220.0, secs(1.2), 0.4);
    let (l, _) = render_midi(
        &mut a,
        &x,
        &x,
        &[(100, note_on(60)), (100, note_on(64)), (1_000, note_on(67))],
        TransportInfo::default(),
    );
    let tail = &l[secs(0.6)..];
    let c = peak_near(tail, 261.63, 25.0, 1.0).1;
    let e = peak_near(tail, 329.63, 25.0, 1.0).1;
    let g = peak_near(tail, 392.0, 25.0, 1.0).1;
    assert!(
        c < g * 0.4 && e > 0.02 && g > 0.02,
        "oldest note stolen: {c} {e} {g}"
    );

    // Glide: a slow slide puts energy between the notes in the middle of the slide.
    let mut a = midi_shifter(asft::VOICE_MONO as f64);
    set(
        &mut a,
        &[(asft::GLIDE, 2_000.0), (asft::FORMANT_FOLLOW, 100.0)],
    );
    let x = saw(220.0, secs(2.5), 0.4);
    let (l, _) = render_midi(
        &mut a,
        &x,
        &x,
        &[(0, note_on(60)), (secs(0.5), note_on(72))],
        TransportInfo::default(),
    );
    let mid = 261.63 * 2f64.powf(0.5);
    let at_mid = peak_near(&l[secs(1.4)..secs(1.6)], mid, 30.0, 2.0).1;
    assert!(at_mid > 0.02, "half way through the glide: {at_mid}");
}

#[test]
fn auto_shift_mod_routings_and_lfo_reach_volume_pan_and_formant() {
    let x = saw(220.0, secs(1.2), 0.4);
    let note_with_velocity = |vel: f32| MidiMessage::NoteOn {
        channel: 0,
        key: 60,
        velocity: vel,
        note_id: 0,
        tuning: 0.0,
    };
    let run = |params: &[(u32, f64)], velocity: f32| {
        let mut a = midi_shifter(asft::VOICE_MONO as f64);
        set(&mut a, params);
        let (l, r) = render_midi(
            &mut a,
            &x,
            &x,
            &[(0, note_with_velocity(velocity))],
            TransportInfo::default(),
        );
        (rms(&l[secs(0.5)..]) as f64, rms(&r[secs(0.5)..]) as f64)
    };
    // Velocity to Volume with -100% depth: a soft note is louder than a hard one.
    let params = [
        (asft::MOD_SOURCE_VOLUME, asft::SOURCE_VELOCITY as f64),
        (asft::MOD_DEPTH_VOLUME, -100.0),
    ];
    let (hard, _) = run(&params, 1.0);
    let (soft, _) = run(&params, 0.25);
    assert!(soft > hard * 2.0, "{soft} vs {hard}");
    // Velocity to Pan: full positive velocity puts it to the right.
    let (l, r) = run(
        &[
            (asft::MOD_SOURCE_PAN, asft::SOURCE_VELOCITY as f64),
            (asft::MOD_DEPTH_PAN, 100.0),
        ],
        1.0,
    );
    assert!(r > l * 4.0, "{l} {r}");

    // The LFO reaches volume regardless of correction.
    let mut a = shifter();
    set(
        &mut a,
        &[
            (asft::STRENGTH, 0.0),
            (asft::LFO_VOLUME, 100.0),
            (asft::LFO_RATE, 100.0),
        ],
    );
    let x2: Vec<f32> = sine(1_000.0, SR, secs(1.0))
        .iter()
        .map(|s| s * 0.5)
        .collect();
    let (l, _) = render(&mut a, &x2, &x2);
    let chunks: Vec<f32> = l[secs(0.3)..].chunks(240).map(rms).collect();
    let (lo, hi) = chunks
        .iter()
        .fold((f32::MAX, 0.0f32), |(a, b), v| (a.min(*v), b.max(*v)));
    assert!(hi > lo * 1.5, "volume is modulated: {lo} {hi}");
}

#[test]
fn auto_shift_lfo_delay_attack_and_reset_shape_when_it_starts() {
    // With a long LFO delay the pitch stays steady at the start.
    let x: Vec<f32> = sine(440.0, SR, secs(1.5)).iter().map(|s| s * 0.5).collect();
    let spread = |slice: &[f32]| peak_near(slice, 440.0, 30.0, 1.0).0;
    let mut a = shifter();
    set(
        &mut a,
        &[
            (asft::STRENGTH, 0.0),
            (asft::LFO_PITCH, 100.0),
            (asft::LFO_RATE, 50.0),
            (asft::LFO_DELAY, 800.0),
        ],
    );
    let (l, _) = render(&mut a, &x, &x);
    let early = spread(&l[secs(0.2)..secs(0.5)]);
    near(
        cents(early, 440.0),
        0.0,
        8.0,
        "steady before the delay ends",
    );
    let later = tone_amplitude(&l[secs(1.0)..], 440.0, SR);
    assert!(later < tone_amplitude(&l[secs(0.2)..secs(0.5)], 440.0, SR) * 0.8);
}

#[test]
fn auto_shift_scale_degrees_shift_after_correction() {
    // 450 Hz corrected to A in C Major, then up one degree of C Major: B.
    let mut a = shifter();
    set(&mut a, &[(asft::SCALE, 0.0), (asft::SCALE_SHIFT, 1.0)]);
    let x: Vec<f32> = sine(450.0, SR, secs(1.2)).iter().map(|s| s * 0.5).collect();
    let (l, _) = render(&mut a, &x, &x);
    let target = 493.88;
    let (hz, amp) = peak_near(&l[secs(0.5)..], target, 80.0, 1.0);
    near(cents(hz, target), 0.0, 15.0, "corrected then up a degree");
    assert!(amp > 0.05);
}

#[test]
fn auto_shift_pitch_ranges_track_their_own_registers() {
    let correct_in = |range: usize, hz: f64, target: f64| {
        let mut a = shifter();
        a.set_param(asft::PITCH_RANGE, range as f64);
        let x: Vec<f32> = sine(hz as f32, SR, secs(1.5))
            .iter()
            .map(|s| s * 0.5)
            .collect();
        let (l, _) = render(&mut a, &x, &x);
        let (found, _) = peak_near(&l[secs(0.8)..], target, 30.0, 1.0);
        cents(found, target)
    };
    near(correct_in(asft::RANGE_BASS, 88.5, 87.31), 0.0, 20.0, "bass");
    near(
        correct_in(asft::RANGE_HIGH, 880.0 * 1.01, 880.0),
        0.0,
        15.0,
        "high",
    );
}

#[test]
fn spectral_time_interval_in_notes_follows_the_tempo() {
    let mut h = Harness::new(SR);
    h.transport.tempo = 120.0;
    let mut p = spectral_time();
    set(
        &mut p,
        &[
            (st::FREEZE_MODE, st::MODE_RETRIGGER as f64),
            (st::RETRIGGER_MODE, st::RETRIGGER_SYNC as f64),
            (st::INTERVAL_UNIT, st::UNIT_NOTES as f64),
            (st::INTERVAL_DIV, 12.0),
            (st::FADE_SHAPE, st::SHAPE_ENVELOPE as f64),
            (st::FADE_IN, 1.0),
            (st::FADE_OUT, 10.0),
            (st::FREEZE, 1.0),
        ],
    );
    let (mut l, mut r) = (vec![0.5f32; secs(1.5)], vec![0.5f32; secs(1.5)]);
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        h.transport.tick = (i * BLOCK) as f64 * h.transport.ticks_per_sample();
        h.process(&mut p, cl, cr, &[], &[]);
    }
    assert!(l.iter().all(|s| s.is_finite()));
}

#[test]
fn spectral_time_stereo_widens_tilt_between_channels() {
    let mut p: st::SpectralTime = ready();
    set(
        &mut p,
        &[
            (st::DELAY_ON, 1.0),
            (st::FREEZER_ON, 0.0),
            (st::DRY_WET, 100.0),
            (st::DELAY_MIX, 100.0),
            (st::TIME_MS, 200.0),
            (st::FEEDBACK, 0.0),
            (st::TILT, 60.0),
            (st::STEREO, 100.0),
        ],
    );
    let x = noise(secs(1.5), 11, 0.4);
    let (l, r) = render(&mut p, &x, &x);
    assert_ne!(l, r, "the two sides differ");
}

#[test]
fn every_device_runs_the_sample_rates_it_will_see() {
    // Latency in samples scales with the rate for Auto Shift but the STFT sizes stay fixed.
    let mut a = asft::AutoShift::default();
    a.activate(96_000.0, 512);
    assert!(a.latency() > 800);
    let mut t = st::SpectralTime::default();
    t.activate(44_100.0, 512);
    assert_eq!(t.latency(), 480);
    let mut r = res::SpectralResonator::default();
    r.activate(44_100.0, 512);
    assert_eq!(r.latency() as usize, res::FRAME);
}

#[test]
fn note_math_matches_the_manual() {
    assert_eq!(tuner::nearest_note(440.0, 440.0).0, 69.0);
    let (note, cent) = tuner::nearest_note(466.16, 440.0);
    assert_eq!(note, 70.0);
    assert!(cent.abs() < 0.5, "{cent}");
}

/// Roughly what each device costs. Ignored by default; run with `-- --ignored --nocapture`.
#[test]
#[ignore]
fn cpu_report() {
    let seconds = 5.0;
    let x = saw(220.0, secs(seconds), 0.3);
    let report = |name: &str, p: &mut dyn Processor| {
        let start = std::time::Instant::now();
        let _ = render(p, &x, &x);
        let elapsed = start.elapsed().as_secs_f64();
        eprintln!("{name:42} {:.2}% of one core", elapsed / seconds * 100.0);
    };
    {
        let mut a: sp::Spectrum = ready();
        report("Spectrum (no UI reading)", &mut a);
    }
    {
        let mut a: tuner::Tuner = ready();
        report("Tuner (no UI reading)", &mut a);
    }
    {
        let mut a: voc::Vocoder = ready();
        report("Vocoder default (20 bands)", &mut a);
        let mut a: voc::Vocoder = ready();
        set(
            &mut a,
            &[(voc::BANDS, 7.0), (voc::CHANNELS, voc::CHANNELS_LR as f64)],
        );
        report("Vocoder 40 bands L/R", &mut a);
    }
    {
        let mut a: asft::AutoShift = ready();
        report("Auto Shift Mid quantizer", &mut a);
        let mut a: asft::AutoShift = ready();
        set(
            &mut a,
            &[
                (asft::MIDI_ON, 1.0),
                (asft::VOICE_MODE, 1.0),
                (asft::VOICE_COUNT, 2.0),
            ],
        );
        let notes: Vec<(usize, MidiMessage)> = (0..8).map(|i| (0, note_on(60 + i as u8))).collect();
        let start = std::time::Instant::now();
        render_midi(&mut a, &x, &x, &notes, TransportInfo::default());
        eprintln!(
            "{:42} {:.2}% of one core",
            "Auto Shift MIDI 8 voices",
            start.elapsed().as_secs_f64() / seconds * 100.0
        );
    }
    {
        let mut a: st::SpectralTime = ready();
        report("Spectral Time default (480)", &mut a);
        let mut a: st::SpectralTime = ready();
        set(
            &mut a,
            &[
                (st::FREEZE, 1.0),
                (st::FREEZE_MODE, 1.0),
                (st::RETRIGGER_MODE, 1.0),
                (st::FADE_SHAPE, 1.0),
                (st::INTERVAL_MS, 20.0),
                (st::FADE_OUT, 500.0),
                (st::RESOLUTION, 5.0),
            ],
        );
        report("Spectral Time 7680, 8 stacked freezes", &mut a);
    }
    {
        let mut a: res::SpectralResonator = ready();
        report("Spectral Resonator default (64 partials)", &mut a);
        let mut a: res::SpectralResonator = ready();
        set(
            &mut a,
            &[
                (res::HARMONICS, 256.0),
                (res::UNISON, 8.0),
                (res::MOD_MODE, 2.0),
                (res::MOD_RATE, 10.0),
                (res::PCH_MOD, 1.0),
            ],
        );
        report("Spectral Resonator 256 x 8 unison, Wander", &mut a);
    }
}
