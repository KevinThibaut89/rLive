//! Tones, Texture, Complex and Complex Pro against the rules every warp mode has to keep.

use realfft::RealFftPlanner;
use rlive_core::warp::{StretchSpan, Stretcher, WarpMode};
use rlive_core::SampleBuffer;
use rlive_stretch::{stretcher, PhaseVocoder, Texture, Tones};
use rlive_testkit::{assert_no_alloc, max_step, rms, tone_amplitude};
use std::f64::consts::TAU;

const SR: f64 = 48_000.0;

fn stereo(left: Vec<f32>) -> SampleBuffer {
    SampleBuffer {
        name: "t".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    }
}

fn sine(hz: f64, frames: usize, amp: f32) -> SampleBuffer {
    stereo(
        (0..frames)
            .map(|i| (i as f64 / SR * hz * TAU).sin() as f32 * amp)
            .collect(),
    )
}

fn tones_mode() -> WarpMode {
    WarpMode::Tones { grain_size: 30.0 }
}

fn texture_mode() -> WarpMode {
    WarpMode::Texture {
        grain_size: 30.0,
        flux: 0.0,
    }
}

fn pro_mode() -> WarpMode {
    WarpMode::ComplexPro {
        formants: 100.0,
        envelope: 128.0,
    }
}

/// Name, stretcher and mode of every kind built in A10, prepared for the test rate.
fn all() -> Vec<(&'static str, Box<dyn Stretcher>, WarpMode)> {
    [
        ("Tones", tones_mode()),
        ("Texture", texture_mode()),
        ("Complex", WarpMode::Complex),
        ("Complex Pro", pro_mode()),
    ]
    .into_iter()
    .map(|(name, mode)| {
        let mut st = stretcher(mode.kind());
        st.prepare(SR);
        (name, st, mode)
    })
    .collect()
}

/// Renders `frames` output frames at `rate` source frames per output frame, in blocks of `block`.
#[allow(clippy::too_many_arguments)]
fn render(
    st: &mut dyn Stretcher,
    s: &SampleBuffer,
    mode: WarpMode,
    rate: f64,
    transpose: f32,
    frames: usize,
    block: usize,
    slices: &[f64],
) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    st.reset();
    st.seek(0.0);
    let mut pos = 0.0f64;
    let mut at = 0;
    while at < frames {
        let n = block.min(frames - at);
        st.render(
            s,
            StretchSpan {
                src_start: pos,
                src_end: pos + rate * n as f64,
                transpose,
                mode,
                slices,
            },
            &mut l[at..at + n],
            &mut r[at..at + n],
        );
        pos += rate * n as f64;
        at += n;
    }
    (l, r)
}

/// The frequency near `around` whose energy in `buf` is greatest, by a fine search.
fn peak_hz(buf: &[f32], around: f64) -> f64 {
    let mut best = (around, 0.0f64);
    for cents in -220..=220 {
        let hz = around * 2f64.powf(f64::from(cents) / 1200.0);
        let v = tone_amplitude(buf, hz, SR);
        if v > best.1 {
            best = (hz, v);
        }
    }
    best.0
}

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// A pure tone is the worst case for Texture, which scatters grains without aligning them: what
/// survives is where the energy is, not an exact pitch (manual p. 250).
fn tolerance(name: &str, other_modes: f64) -> f64 {
    if name == "Texture" {
        160.0
    } else {
        other_modes
    }
}

/// The energy of `buf` in 1/6-octave bands from 300 Hz to 6 kHz, by band index.
fn band_energies(buf: &[f32], bands: usize) -> Vec<f64> {
    let n = buf.len().next_power_of_two();
    let mut input = vec![0.0f32; n];
    for (i, v) in buf.iter().enumerate() {
        let w = 0.5 - 0.5 * (TAU * i as f64 / buf.len() as f64).cos();
        input[i] = *v * w as f32;
    }
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut out = fft.make_output_vec();
    fft.process(&mut input, &mut out).unwrap();
    (0..bands)
        .map(|b| {
            let lo = 300.0 * 2f64.powf(b as f64 / 6.0);
            let hi = lo * 2f64.powf(1.0 / 6.0);
            let (a, z) = ((lo / SR * n as f64) as usize, (hi / SR * n as f64) as usize);
            out[a..=z]
                .iter()
                .map(|c| f64::from(c.norm_sqr()))
                .sum::<f64>()
                .sqrt()
        })
        .collect()
}

#[test]
fn every_mode_has_a_real_stretcher() {
    for (name, st, _) in all() {
        assert!(st.declicks(), "{name}");
        assert_eq!(st.latency(), 0, "{name}");
    }
}

#[test]
fn pitch_survives_stretching() {
    let s = sine(440.0, 240_000, 0.5);
    for rate in [0.5, 0.8, 1.25, 2.0] {
        for (name, mut st, mode) in all() {
            let (l, _) = render(&mut *st, &s, mode, rate, 0.0, 96_000, 512, &[]);
            let hz = peak_hz(&l[24_000..72_000], 440.0);
            assert!(
                cents(hz, 440.0).abs() < tolerance(name, 4.0),
                "{name} at {rate}: {hz}"
            );
        }
    }
}

#[test]
fn transposing_moves_pitch_to_the_semitone() {
    let s = sine(330.0, 240_000, 0.5);
    for semis in [7.0f32, -12.0, 12.0, 3.0, -5.0, 24.0] {
        let want = 330.0 * 2f64.powf(f64::from(semis) / 12.0);
        for (name, mut st, mode) in all() {
            if name == "Texture" {
                continue;
            }
            for rate in [1.0, 0.75] {
                let (l, _) = render(&mut *st, &s, mode, rate, semis, 96_000, 256, &[]);
                let hz = peak_hz(&l[24_000..72_000], want);
                assert!(
                    cents(hz, want).abs() < tolerance(name, 6.0),
                    "{name} {semis} at {rate}: {hz} wanted {want}"
                );
            }
        }
    }
}

#[test]
fn texture_moves_the_energy_by_the_transposition() {
    // Texture has no exact pitch to measure on a tone; a band of noise has a place to be.
    let (partials, frames) = (60, 192_000);
    let mut left = vec![0.0f32; frames];
    let mut phase_seed = 1u32;
    for p in 0..partials {
        phase_seed = phase_seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let phase = phase_seed as f64 / u32::MAX as f64 * TAU;
        let hz = 800.0 + p as f64 * 400.0 / partials as f64;
        for (i, v) in left.iter_mut().enumerate() {
            *v += ((i as f64 / SR * hz * TAU + phase).sin() * 0.04) as f32;
        }
    }
    let s = stereo(left);
    let centroid = |buf: &[f32]| {
        let bands = band_energies(buf, 30);
        let (sum, weight) = bands.iter().enumerate().fold((0.0, 0.0), |(s, w), (b, e)| {
            (s + (b as f64) * e * e, w + e * e)
        });
        sum / weight
    };
    let mut st = stretcher(texture_mode().kind());
    st.prepare(SR);
    let (flat, _) = render(&mut *st, &s, texture_mode(), 1.0, 0.0, 96_000, 512, &[]);
    let (up, _) = render(&mut *st, &s, texture_mode(), 1.0, 12.0, 96_000, 512, &[]);
    let moved = centroid(&up[24_000..]) - centroid(&flat[24_000..]);
    // An octave is six of the one-sixth-octave bands.
    assert!((moved - 6.0).abs() < 1.0, "moved {moved} bands");
}

#[test]
fn a_source_at_another_rate_keeps_its_pitch() {
    let hz = 440.0;
    let source_rate = 44_100.0;
    let left: Vec<f32> = (0..240_000)
        .map(|i| (i as f64 / source_rate * hz * TAU).sin() as f32 * 0.5)
        .collect();
    let s = SampleBuffer {
        name: "t".into(),
        right: left.clone(),
        left,
        sample_rate: source_rate,
    };
    for rate in [source_rate / SR, 0.8 * source_rate / SR] {
        for (name, mut st, mode) in all() {
            let (l, _) = render(&mut *st, &s, mode, rate, 0.0, 96_000, 512, &[]);
            let found = peak_hz(&l[24_000..72_000], hz);
            assert!(
                cents(found, hz).abs() < tolerance(name, 4.0),
                "{name} at {rate}: {found}"
            );
        }
    }
}

#[test]
fn level_stays_put_under_stretching() {
    let s = sine(523.0, 288_000, 0.5);
    for rate in [0.5, 0.75, 1.0, 1.5, 2.0] {
        for (name, mut st, mode) in all() {
            let (l, r) = render(&mut *st, &s, mode, rate, 0.0, 96_000, 512, &[]);
            assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{name} {rate}");
            assert!(l.iter().all(|v| v.abs() < 1.5), "{name} {rate}");
            let target = 0.5 / 2f32.sqrt();
            for chunk in l[12_000..].chunks(4800) {
                let v = rms(chunk);
                let db = 20.0 * (v / target).log10();
                assert!(db.abs() < 3.0, "{name} at {rate}: {db} dB");
            }
        }
    }
}

#[test]
fn at_the_clips_own_speed_a_mode_plays_the_source_to_the_sample() {
    // Nothing to compensate: frame N of the output is frame N of the source.
    let s = sine(440.0, 96_000, 0.5);
    for (name, mut st, mode) in all() {
        let (l, _) = render(&mut *st, &s, mode, 1.0, 0.0, 48_000, 512, &[]);
        assert_eq!(st.latency(), 0, "{name}");
        let worst = l[1_000..48_000]
            .iter()
            .zip(&s.left[1_000..48_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 2e-3, "{name}: off by {worst}");
    }
}

#[test]
fn hits_land_where_the_timeline_says() {
    // Bursts at known source frames, each marked as a transient for the vocoder.
    let mut left = vec![0.0f32; 300_000];
    let hits: Vec<usize> = (1..=11).map(|k| k * 24_000).collect();
    for h in &hits {
        for i in 0..2400 {
            left[h + i] = ((i as f64 / SR * 1500.0 * TAU).sin() * 0.7) as f32;
        }
    }
    let s = stereo(left);
    let slices: Vec<f64> = hits.iter().map(|h| *h as f64).collect();
    for rate in [0.5, 0.75, 1.0, 1.5, 2.0] {
        for (name, mut st, mode) in all() {
            let frames = (260_000.0 / rate) as usize;
            let (l, _) = render(&mut *st, &s, mode, rate, 0.0, frames, 480, &slices);
            for h in &hits {
                let at = (*h as f64 / rate) as usize;
                if at + 12_000 >= frames {
                    continue;
                }
                let threshold = 0.1;
                let window = &l[at.saturating_sub(9_600)..at + 2400];
                let found = window
                    .windows(32)
                    .position(|w| rms(w) > threshold)
                    .map(|p| p as i64 + at.saturating_sub(9_600) as i64);
                let found = found.unwrap_or_else(|| panic!("{name} at {rate}: no hit near {at}"));
                let error_ms = (found - at as i64) as f64 / SR * 1000.0;
                let tolerance = if name.starts_with("Complex") {
                    6.0
                } else {
                    32.0
                };
                assert!(
                    error_ms.abs() <= tolerance,
                    "{name} at {rate}: hit {h} off by {error_ms} ms"
                );
            }
        }
    }
}

#[test]
fn a_seek_does_not_click() {
    // A jump to an unrelated place in a loud tone: nothing louder than the tones' own slope.
    let a = sine(220.0, 96_000, 0.6);
    let b = sine(330.0, 96_000, 0.6);
    let mut left = a.left.clone();
    left.extend_from_slice(&b.left);
    let s = stereo(left);
    let steady = max_step(&s.left[..96_000]);
    for (name, mut st, mode) in all() {
        st.reset();
        st.seek(0.0);
        let mut output = vec![0.0f32; 48_000];
        let mut right = vec![0.0f32; 48_000];
        let mut seek_frame = 0;
        let mut pos = 0.0f64;
        let mut at = 0;
        let span = |start: f64, n: usize| StretchSpan {
            src_start: start,
            src_end: start + 1.0 * n as f64,
            transpose: 0.0,
            mode,
            slices: &[],
        };
        while at < 48_000 {
            let n = 384.min(48_000 - at);
            if at >= 24_000 && seek_frame == 0 {
                seek_frame = at;
                pos = 96_000.0;
                st.seek(pos);
            }
            let window = span(pos, n);
            st.render(&s, window, &mut output[at..at + n], &mut right[at..at + n]);
            pos += n as f64;
            at += n;
        }
        // The steepest slope across the seek is no worse than a little over the tones'.
        let around = &output[seek_frame.saturating_sub(64)..seek_frame + 2048];
        let jump = max_step(around);
        assert!(
            jump < steady * 2.0 + 0.04,
            "{name}: {jump} against {steady}"
        );
        assert!(
            output[seek_frame + 128..seek_frame + 2048]
                .chunks(192)
                .all(|c| rms(c) > 0.1),
            "{name}: silence after the seek"
        );
    }
}

#[test]
fn block_size_changes_nothing_much() {
    let mut left = vec![0.0f32; 192_000];
    for (i, v) in left.iter_mut().enumerate() {
        let t = i as f64 / SR;
        *v = ((t * 330.0 * TAU).sin() * 0.3 + (t * 730.0 * TAU).sin() * 0.2) as f32;
    }
    let s = stereo(left);
    for (name, mut st, mode) in all() {
        let (reference, _) = render(&mut *st, &s, mode, 0.75, 2.0, 48_000, 64, &[]);
        for block in [128, 250, 512, 1024, 4096] {
            let (other, _) = render(&mut *st, &s, mode, 0.75, 2.0, 48_000, block, &[]);
            let worst = reference
                .iter()
                .zip(&other)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(worst < 1e-2, "{name}, block {block}: off by {worst}");
        }
    }
}

#[test]
fn rendering_and_seeking_never_allocate() {
    let s = sine(440.0, 96_000, 0.5);
    for (name, mut st, mode) in all() {
        st.reset();
        st.seek(10.0);
        let mut l = vec![0.0f32; 4096];
        let mut r = vec![0.0f32; 4096];
        assert_no_alloc(|| {
            let mut pos = 10.0;
            let mut at = 0;
            for (i, n) in [64, 256, 512, 1024, 2240].into_iter().enumerate() {
                if i == 2 {
                    st.seek(5_000.0);
                    pos = 5_000.0;
                }
                st.render(
                    &s,
                    StretchSpan {
                        src_start: pos,
                        src_end: pos + n as f64 * 0.9,
                        transpose: 2.0,
                        mode,
                        slices: &[100.0, 8_000.0],
                    },
                    &mut l[at..at + n],
                    &mut r[at..at + n],
                );
                pos += n as f64 * 0.9;
                at += n;
            }
        });
        assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{name}");
    }
}

/// A 200 Hz pulse train through a resonance around 1 kHz: a formant under a pitch.
fn voice_like(frames: usize) -> SampleBuffer {
    let mut out = vec![0.0f32; frames];
    let period = (SR / 200.0) as usize;
    let (mut y1, mut y2) = (0.0f64, 0.0f64);
    let f = TAU * 1000.0 / SR;
    let radius = 0.97f64;
    let (a1, a2) = (2.0 * radius * f.cos(), -radius * radius);
    for (i, v) in out.iter_mut().enumerate() {
        let x = if i % period == 0 { 1.0 } else { 0.0 };
        let y = x + a1 * y1 + a2 * y2;
        y2 = y1;
        y1 = y;
        *v = (y * 0.2) as f32;
    }
    stereo(out)
}

fn peak_band(buf: &[f32]) -> f64 {
    let bands = 30;
    let energy = band_energies(buf, bands);
    let (i, _) = energy
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    300.0 * 2f64.powf(i as f64 / 6.0 + 1.0 / 12.0)
}

#[test]
fn formants_stay_put_when_asked_and_follow_the_pitch_when_not() {
    let s = voice_like(240_000);
    let mut st = PhaseVocoder::complex_pro();
    st.prepare(SR);
    let run = |st: &mut PhaseVocoder, formants: f32| {
        let (l, _) = render(
            st,
            &s,
            WarpMode::ComplexPro {
                formants,
                envelope: 64.0,
            },
            1.0,
            7.0,
            72_000,
            512,
            &[],
        );
        peak_band(&l[24_000..72_000])
    };
    let (kept, followed) = (run(&mut st, 100.0), run(&mut st, 0.0));
    let source = peak_band(&s.left[24_000..72_000]);
    // Up a fifth is half an octave; a formant that followed moves by about that much.
    assert!(
        (kept / source).log2().abs() < 0.2,
        "kept at {kept} against {source}"
    );
    assert!(
        (followed / source).log2() > 0.4,
        "followed to {followed} against {source}"
    );
}

#[test]
fn formants_has_no_effect_without_transposition() {
    let s = voice_like(96_000);
    let mut st = PhaseVocoder::complex_pro();
    st.prepare(SR);
    let run = |st: &mut PhaseVocoder, formants: f32| {
        render(
            st,
            &s,
            WarpMode::ComplexPro {
                formants,
                envelope: 128.0,
            },
            0.8,
            0.0,
            48_000,
            512,
            &[],
        )
        .0
    };
    let (a, b) = (run(&mut st, 100.0), run(&mut st, 0.0));
    assert_eq!(a, b);
}

#[test]
fn the_envelope_control_reaches_the_formant_correction() {
    let s = voice_like(96_000);
    let mut st = PhaseVocoder::complex_pro();
    st.prepare(SR);
    let run = |st: &mut PhaseVocoder, envelope: f32| {
        render(
            st,
            &s,
            WarpMode::ComplexPro {
                formants: 100.0,
                envelope,
            },
            1.0,
            5.0,
            48_000,
            512,
            &[],
        )
        .0
    };
    let (coarse, fine) = (run(&mut st, 8.0), run(&mut st, 256.0));
    let difference = coarse
        .iter()
        .zip(&fine)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(difference > 1e-2, "Envelope did nothing: {difference}");
}

#[test]
fn tones_cuts_its_grains_on_the_period_it_finds() {
    let s = sine(220.0, 96_000, 0.5);
    let mut st = Tones::new();
    st.prepare(SR);
    let _ = render(&mut st, &s, tones_mode(), 0.8, 0.0, 12_000, 256, &[]);
    let period = SR / 220.0;
    assert!(
        (st.found_period() - period).abs() < 2.0,
        "found {} wanted {period}",
        st.found_period()
    );
    let frames = f64::from(st.grain_frames());
    let periods = frames / period;
    assert!(
        (periods - periods.round()).abs() < 0.02,
        "{frames} frames is {periods} periods"
    );
    // Asking for a longer grain gets a longer grain, still on a whole number of periods.
    let _ = render(
        &mut st,
        &s,
        WarpMode::Tones { grain_size: 80.0 },
        0.8,
        0.0,
        12_000,
        256,
        &[],
    );
    assert!(f64::from(st.grain_frames()) > frames * 1.8);
    let long = f64::from(st.grain_frames()) / period;
    assert!((long - long.round()).abs() < 0.1, "{long} periods");
}

#[test]
fn tones_keeps_the_asked_for_grain_size_when_there_is_no_pitch() {
    let mut x = 0x1234_5678u32;
    let noise = (0..96_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as f32 / u32::MAX as f32 - 0.5
        })
        .collect();
    let s = stereo(noise);
    let mut st = Tones::new();
    st.prepare(SR);
    let _ = render(&mut st, &s, tones_mode(), 0.8, 0.0, 12_000, 256, &[]);
    let frames = st.grain_frames();
    assert!(
        (f64::from(frames) - 0.03 * SR).abs() < 2.0,
        "{frames} frames"
    );
}

#[test]
fn texture_grain_size_and_fluctuation_change_the_grains() {
    let s = sine(440.0, 96_000, 0.5);
    let mut st = Texture::new();
    st.prepare(SR);
    let flat = |st: &mut Texture, grain: f32, flux: f32| {
        render(
            st,
            &s,
            WarpMode::Texture {
                grain_size: grain,
                flux,
            },
            0.7,
            0.0,
            24_000,
            256,
            &[],
        )
        .0
    };
    let (a, a_len) = (flat(&mut st, 40.0, 0.0), st.grain_frames());
    assert!((f64::from(a_len) - 0.04 * SR).abs() < 2.0, "{a_len} frames");
    assert_eq!(a, flat(&mut st, 40.0, 0.0), "no fluctuation is repeatable");
    let moved = flat(&mut st, 40.0, 60.0);
    let moved_len = st.grain_frames();
    let difference = a
        .iter()
        .zip(&moved)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max);
    assert!(difference > 0.05, "Fluctuation did nothing");
    assert_ne!(a_len, moved_len, "Fluctuation left the length alone");
    assert_eq!(
        moved,
        flat(&mut st, 40.0, 60.0),
        "the randomness restarts with a clip"
    );
    let short = flat(&mut st, 10.0, 0.0);
    assert_ne!(short, a, "Grain Size did nothing");
}

/// Not a check: run with `--release -- --ignored --nocapture` for the cost of a voice.
#[test]
#[ignore]
fn cost_per_voice_in_microseconds_per_second_of_audio() {
    let mut left = vec![0.0f32; 480_000];
    let mut seed = 1u32;
    for (i, v) in left.iter_mut().enumerate() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let t = i as f64 / SR;
        let noise = (seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
        *v = ((t * 220.0 * TAU).sin() * 0.3 + (t * 1337.0 * TAU).sin() * 0.2 + noise * 0.2) as f32;
    }
    let s = SampleBuffer {
        name: "t".into(),
        right: left.iter().rev().copied().collect(),
        left,
        sample_rate: SR,
    };
    let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    for (name, mut st, mode) in all() {
        for (label, rate, transpose) in [("stretch", 0.75, 0.0f32), ("+5 st", 1.0, 5.0)] {
            st.reset();
            st.seek(0.0);
            let blocks = 400;
            let start = std::time::Instant::now();
            for b in 0..blocks {
                let pos = b as f64 * 512.0 * rate;
                st.render(
                    &s,
                    StretchSpan {
                        src_start: pos,
                        src_end: pos + 512.0 * rate,
                        transpose,
                        mode,
                        slices: &[],
                    },
                    &mut l,
                    &mut r,
                );
            }
            let seconds = start.elapsed().as_secs_f64();
            let audio = blocks as f64 * 512.0 / SR;
            println!(
                "{name:12} {label:8} {:6.2}% of a core at 48 kHz, 512 frames",
                100.0 * seconds / audio
            );
        }
    }
}
