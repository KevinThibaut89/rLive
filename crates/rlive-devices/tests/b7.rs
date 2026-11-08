//! Batch B7: Reverb, Hybrid Reverb, Resonators and Corpus, against the manual and against
//! arithmetic.

use rlive_core::{MidiEvent, MidiMessage, ParamEvent, ParamId, Processor, Resource, ScaleInfo};
use rlive_devices::devices::b7::{corpus, hybrid_ir, hybrid_reverb as hr, resonators as res};
use rlive_devices::dsp::ir::IrKind;
use rlive_devices::dsp::util::Rng;
use rlive_devices::dsp::DelayLine;
use rlive_devices::effects::reverb as rv;
use rlive_devices::effects::Reverb;
use rlive_devices::{find, SampleBuffer};
use rlive_testkit::{assert_no_alloc, max_step, onset, peak, rms, sine, tone_amplitude, Harness};
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
    render_with(p, left, right, &mut h)
}

fn render_with(
    p: &mut dyn Processor,
    left: &[f32],
    right: &[f32],
    h: &mut Harness,
) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        assert_no_alloc(|| h.process(p, cl, cr, &[], &[]));
    }
    (l, r)
}

fn impulse(frames: usize) -> Vec<f32> {
    let mut v = vec![0.0; frames];
    v[0] = 1.0;
    v
}

fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames]
}

fn secs(s: f64) -> usize {
    (s * SR) as usize
}

fn mono(p: &mut dyn Processor, input: &[f32]) -> Vec<f32> {
    render(p, input, input).0
}

fn near(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} != {expected} (+-{tol})"
    );
}

fn noise(frames: usize, seed: u32, level: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..frames).map(|_| rng.bipolar() * level).collect()
}

/// RT60 of a decaying signal by Schroeder backward integration, over the -5 to -35 dB stretch.
fn rt60(signal: &[f32]) -> f64 {
    let mut energy: Vec<f64> = signal.iter().map(|s| (*s as f64) * (*s as f64)).collect();
    for i in (0..energy.len() - 1).rev() {
        energy[i] += energy[i + 1];
    }
    let total = energy[0];
    let db: Vec<f64> = energy
        .iter()
        .map(|e| 10.0 * (e / total).max(1e-12).log10())
        .collect();
    let t5 = db.iter().position(|d| *d < -5.0).unwrap();
    let t35 = db.iter().position(|d| *d < -35.0).unwrap();
    (t35 - t5) as f64 / SR * 2.0
}

/// Amplitude of a tone in `[from, from + len)`.
fn tone(signal: &[f32], hz: f64, from: usize, len: usize) -> f64 {
    let cycles = (len as f64 * hz / SR).round().max(1.0);
    let frames = ((cycles * SR / hz).round() as usize).min(len);
    tone_amplitude(&signal[from..from + frames], hz, SR)
}

/// The frequency near `hz` where the signal peaks, searched in `step_cents` over +-`span_cents`.
fn peak_near(signal: &[f32], hz: f64, span_cents: f64, step_cents: f64) -> f64 {
    let mut best = (0.0f64, hz);
    let mut c = -span_cents;
    while c <= span_cents {
        let f = hz * 2f64.powf(c / 1200.0);
        let a = tone_amplitude(signal, f, SR);
        if a > best.0 {
            best = (a, f);
        }
        c += step_cents;
    }
    best.1
}

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// The fraction of samples above -20 dB of the signal's RMS. A sparse echo train reads low, a
/// dense reverberation high.
fn fill(signal: &[f32]) -> f32 {
    let floor = rms(signal) * 0.1;
    signal.iter().filter(|s| s.abs() > floor).count() as f32 / signal.len() as f32
}

/// Summed power in a band, by a sweep of Goertzels. For signals whose energy spreads around a
/// frequency instead of sitting on it, as a pitch-shifted one does.
fn band_power(signal: &[f32], low: f64, high: f64) -> f64 {
    let mut power = 0.0;
    let mut hz = low;
    while hz <= high {
        let a = tone_amplitude(signal, hz, SR);
        power += a * a;
        hz += 4.0;
    }
    power
}

// ------------------------------------------------------------------------------ Reverb

/// The pre-B7 Reverb, copied so the comparison can't drift with the code.
fn original_reverb(left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
    const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
    const STEREO_SPREAD: usize = 23;
    let (size, damping, width, mix, predelay_ms) = (0.7f32, 0.5f32, 1.0f32, 0.25f32, 10.0f32);
    let scale = SR / 44_100.0;
    let sr = SR as f32;
    struct Comb {
        buf: Vec<f32>,
        pos: usize,
        store: f32,
    }
    struct Allpass {
        buf: Vec<f32>,
        pos: usize,
    }
    let mut combs: Vec<Vec<Comb>> = (0..2)
        .map(|ch| {
            COMB_TUNING
                .iter()
                .map(|&t| Comb {
                    buf: vec![0.0; (((t + ch * STEREO_SPREAD) as f64) * scale) as usize],
                    pos: 0,
                    store: 0.0,
                })
                .collect()
        })
        .collect();
    let mut allpasses: Vec<Vec<Allpass>> = (0..2)
        .map(|ch| {
            ALLPASS_TUNING
                .iter()
                .map(|&t| Allpass {
                    buf: vec![0.0; (((t + ch * STEREO_SPREAD) as f64) * scale) as usize],
                    pos: 0,
                })
                .collect()
        })
        .collect();
    let mut predelay = DelayLine::new((SR * 0.25) as usize);
    let feedback = 0.7 + size * 0.28;
    let damp = damping * 0.4;
    let pre = (predelay_ms * 0.001 * sr).max(1.0);
    let wet1 = width / 2.0 + 0.5;
    let wet2 = (1.0 - width) / 2.0;
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    for i in 0..l.len() {
        let (inl, inr) = (l[i], r[i]);
        let input = (inl + inr) * 0.015;
        predelay.write(input);
        let x = predelay.read(pre);
        let mut out = [0f32; 2];
        for (ch, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0;
            for c in combs[ch].iter_mut() {
                let y = c.buf[c.pos];
                c.store = y * (1.0 - damp) + c.store * damp;
                c.buf[c.pos] = x + c.store * feedback;
                c.pos = (c.pos + 1) % c.buf.len();
                acc += y;
            }
            for a in allpasses[ch].iter_mut() {
                let b = a.buf[a.pos];
                let y = b - acc;
                a.buf[a.pos] = acc + b * 0.5;
                a.pos = (a.pos + 1) % a.buf.len();
                acc = y;
            }
            *o = acc;
        }
        let wl = out[0] * wet1 + out[1] * wet2;
        let wr = out[1] * wet1 + out[0] * wet2;
        l[i] = inl * (1.0 - mix) + wl * mix;
        r[i] = inr * (1.0 - mix) + wr * mix;
    }
    (l, r)
}

#[test]
fn reverb_defaults_match_the_original_algorithm_sample_for_sample() {
    let left = noise(secs(1.2), 7, 0.5);
    let right = noise(secs(1.2), 11, 0.4);
    let (want_l, want_r) = original_reverb(&left, &right);
    for block in [1usize, 32, 128, 511] {
        let mut p = Reverb::default();
        p.activate(SR, block);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (left.clone(), right.clone());
        for (cl, cr) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
            h.process(&mut p, cl, cr, &[], &[]);
        }
        assert_eq!(l, want_l, "left, block size {block}");
        assert_eq!(r, want_r, "right, block size {block}");
    }
}

#[test]
fn reverb_default_params_are_unchanged_where_saved_projects_can_see_them() {
    let params = rv::reverb_params();
    let v = |id: u32| params.iter().find(|p| p.id == id).unwrap().default;
    assert_eq!(v(rv::SIZE), 0.7);
    assert_eq!(v(rv::DAMPING), 0.5);
    assert_eq!(v(rv::WIDTH), 1.0);
    assert_eq!(v(rv::MIX), 0.25);
    assert_eq!(v(rv::PREDELAY), 10.0);
    assert_eq!(v(rv::ENGINE), rv::ENGINE_CLASSIC as f64);
}

/// A Studio reverb with its filters off, wet, and showing only what `reflect` and `diffuse` let through.
fn studio(reflect: f64, diffuse: f64) -> Reverb {
    let mut p: Reverb = ready();
    set(
        &mut p,
        &[
            (rv::ENGINE, rv::ENGINE_STUDIO as f64),
            (rv::MIX, 1.0),
            (rv::PREDELAY, 0.0),
            (rv::INPUT_LOW_ON, 0.0),
            (rv::INPUT_HIGH_ON, 0.0),
            (rv::HIGH_ON, 0.0),
            (rv::LOW_ON, 0.0),
            (rv::CHORUS_ON, 0.0),
            (rv::SPIN_ON, 0.0),
            (rv::REFLECT, reflect),
            (rv::DIFFUSE, diffuse),
        ],
    );
    p
}

#[test]
fn reverb_decay_time_tracks_the_decay_control() {
    for decay in [0.8f64, 2.0, 5.0] {
        let mut p = studio(-30.0, 0.0);
        set(&mut p, &[(rv::DECAY, decay), (rv::SIZE, 0.6)]);
        let out = mono(&mut p, &impulse(secs(decay * 2.5)));
        let measured = rt60(&out);
        near(measured / decay, 1.0, 0.12, &format!("Decay {decay}"));
    }
}

#[test]
fn reverb_predelay_is_exact() {
    for ms in [0.0f64, 7.5, 50.0, 137.0] {
        let mut p = studio(0.0, -30.0);
        set(&mut p, &[(rv::PREDELAY, ms)]);
        let out = mono(&mut p, &impulse(secs(0.4)));
        let at = onset(&out, 0.3).unwrap();
        assert_eq!(at, (ms * 0.001 * SR).round() as usize, "{ms} ms");
    }
}

#[test]
fn reverb_freeze_holds_the_tail_and_cut_decides_whether_input_adds() {
    let mut p = studio(-30.0, 0.0);
    set(
        &mut p,
        &[(rv::DECAY, 1.0), (rv::SIZE, 0.5), (rv::CHORUS_ON, 0.0)],
    );
    let burst = noise(secs(0.5), 3, 0.3);
    let mut h = Harness::new(SR);
    render_with(&mut p, &burst, &burst, &mut h);
    set(&mut p, &[(rv::FREEZE, 1.0), (rv::CUT, 1.0)]);
    let quiet = silence(secs(0.3));
    let (settle, _) = render_with(&mut p, &quiet, &quiet, &mut h);
    let held_start = rms(&settle[secs(0.15)..]);
    let hold = silence(secs(12.0));
    let (held, _) = render_with(&mut p, &hold, &hold, &mut h);
    let held_end = rms(&held[held.len() - secs(1.0)..]);
    assert!(held_start > 1e-3, "{held_start}");
    near(
        (held_end / held_start) as f64,
        1.0,
        0.08,
        "Cut on: frozen tail neither grows nor fades",
    );

    // With Cut off, input keeps adding to the frozen sound.
    let more = noise(secs(1.0), 5, 0.3);
    let frozen_cut_level = held_end;
    set(&mut p, &[(rv::CUT, 0.0)]);
    let (added, _) = render_with(&mut p, &more, &more, &mut h);
    assert!(
        rms(&added[added.len() - secs(0.2)..]) > frozen_cut_level * 1.2,
        "input adds to the frozen reverb"
    );
    assert!(peak(&added) < 8.0, "stays bounded: {}", peak(&added));
}

#[test]
fn reverb_flat_keeps_the_shelves_out_of_a_frozen_tail() {
    let run = |flat: bool| -> f64 {
        let mut p = studio(-30.0, 0.0);
        set(
            &mut p,
            &[
                (rv::DECAY, 1.0),
                (rv::SIZE, 0.5),
                (rv::HIGH_ON, 1.0),
                (rv::HIGH_FREQ, 2_000.0),
                (rv::HIGH_GAIN, 0.3),
            ],
        );
        let burst = noise(secs(0.3), 3, 0.3);
        let mut h = Harness::new(SR);
        render_with(&mut p, &burst, &burst, &mut h);
        set(
            &mut p,
            &[
                (rv::FREEZE, 1.0),
                (rv::CUT, 1.0),
                (rv::FLAT, flat as u32 as f64),
            ],
        );
        let quiet = silence(secs(4.0));
        let (out, _) = render_with(&mut p, &quiet, &quiet, &mut h);
        let tail = &out[out.len() - secs(1.0)..];
        // Highs that a shelf would have eaten by now.
        (tone(tail, 8_000.0, 0, tail.len()) + tone(tail, 12_000.0, 0, tail.len())) * 0.5
    };
    let flat = run(true);
    let shelved = run(false);
    assert!(
        flat > shelved * 20.0,
        "Flat keeps the highs: flat {flat} vs shelved {shelved}"
    );
    assert!(flat > 1e-4, "still sounding: {flat}");
}

#[test]
fn reverb_diffusion_makes_the_impulse_response_denser() {
    let density = |diffusion: f64| -> f32 {
        let mut p = studio(-30.0, 0.0);
        set(
            &mut p,
            &[
                (rv::DIFFUSION, diffusion),
                (rv::SCALE, 1.0),
                (rv::DECAY, 2.0),
                (rv::SIZE, 0.6),
            ],
        );
        let out = mono(&mut p, &impulse(secs(0.25)));
        fill(&out[secs(0.04)..])
    };
    let sparse = density(0.0);
    let dense = density(1.0);
    assert!(dense > sparse + 0.2, "sparse {sparse} vs dense {dense}");
}

#[test]
fn reverb_size_scales_the_echo_spacing_and_density_sets_how_many_lines_run() {
    use rlive_devices::devices::b7::reverb_studio as studio_ms;
    let first = |size: f64| -> f64 {
        let mut p = studio(-30.0, 0.0);
        set(
            &mut p,
            &[
                (rv::DIFFUSION, 0.0),
                (rv::SIZE, size),
                (rv::DECAY, 4.0),
                (rv::ER_SHAPE, 0.0),
            ],
        );
        let out = mono(&mut p, &impulse(secs(0.5)));
        onset(&out[2..], 0.05).unwrap() as f64 / SR
    };
    let (a, b) = (first(0.55), first(0.75));
    let want = (studio_ms::room_scale(0.75) / studio_ms::room_scale(0.55)) as f64;
    near(b / a, want, want * 0.1, "first echo time scales with Size");

    let lines = |density: f64| -> f32 {
        let mut p = studio(-30.0, 0.0);
        set(
            &mut p,
            &[
                (rv::DIFFUSION, 0.0),
                (rv::SCALE, 1.0),
                (rv::DENSITY, density),
                (rv::SIZE, 0.6),
            ],
        );
        let out = mono(&mut p, &impulse(secs(0.2)));
        fill(&out[secs(0.04)..])
    };
    assert!(lines(3.0) > lines(0.0) * 1.2, "High is denser than Sparse");
}

#[test]
fn reverb_smooth_decides_whether_a_size_change_clicks() {
    let run = |smooth: usize| -> f32 {
        let mut p = studio(-30.0, 0.0);
        set(
            &mut p,
            &[
                (rv::SIZE, 0.4),
                (rv::SMOOTH, smooth as f64),
                (rv::DECAY, 2.0),
            ],
        );
        let tone_in = sine(330.0, SR, secs(0.5));
        let mut h = Harness::new(SR);
        render_with(&mut p, &tone_in, &tone_in, &mut h);
        let more = sine(330.0, SR, secs(0.05));
        let (mut l, mut r) = (more.clone(), more.clone());
        let size = ParamEvent {
            offset: 0,
            id: ParamId(rv::SIZE),
            value: 0.9,
        };
        let mut step = 0.0f32;
        for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
            let params = if i == 0 { &[size][..] } else { &[] };
            assert_no_alloc(|| h.process(&mut p, cl, cr, &[], params));
            step = step.max(max_step(cl));
        }
        step
    };
    let (none, slow, fast) = (
        run(rv::SMOOTH_NONE),
        run(rv::SMOOTH_SLOW),
        run(rv::SMOOTH_FAST),
    );
    assert!(none > 1.5 * slow, "None {none} Slow {slow}");
    assert!(none > 1.2 * fast, "None {none} Fast {fast}");
}

#[test]
fn reverb_stereo_runs_from_mono_to_independent_channels() {
    let measure = |stereo: f64| -> f32 {
        let mut p = studio(0.0, 0.0);
        set(&mut p, &[(rv::STEREO, stereo), (rv::SIZE, 0.6)]);
        let input = noise(secs(1.0), 2, 0.4);
        let (l, r) = render(&mut p, &input, &input);
        let (l, r) = (&l[secs(0.2)..], &r[secs(0.2)..]);
        let dot: f32 = l.iter().zip(r).map(|(a, b)| a * b).sum();
        let (la, ra): (f32, f32) = (l.iter().map(|s| s * s).sum(), r.iter().map(|s| s * s).sum());
        dot / (la * ra).sqrt().max(1e-9)
    };
    assert!(measure(0.0) > 0.999, "{}", measure(0.0));
    assert!(measure(120.0).abs() < 0.35, "{}", measure(120.0));
}

#[test]
fn reverb_reflect_and_diffuse_are_separate_levels() {
    let mut full = studio(0.0, -30.0);
    let first = mono(&mut full, &impulse(4096));
    let mut quiet = studio(-30.0, -30.0);
    let second = mono(&mut quiet, &impulse(4096));
    let db = 20.0 * ((first[0].abs() / second[0].abs()) as f64).log10();
    near(db, 30.0, 0.2, "Reflect -30 dB");
}

#[test]
fn reverb_shape_moves_reflections_and_the_diffuse_onset_apart() {
    let onset_time = |shape: f64| -> f64 {
        let mut p = studio(-30.0, 0.0);
        set(
            &mut p,
            &[(rv::ER_SHAPE, shape), (rv::SIZE, 0.6), (rv::DIFFUSION, 0.0)],
        );
        let out = mono(&mut p, &impulse(secs(0.3)));
        onset(&out[2..], 0.05).unwrap() as f64 / SR
    };
    assert!(
        onset_time(1.0) > onset_time(0.0) + 0.03,
        "late diffuse onset"
    );
    let er_energy = |shape: f64| -> f32 {
        let mut p = studio(0.0, -30.0);
        set(&mut p, &[(rv::ER_SHAPE, shape), (rv::SIZE, 0.6)]);
        let out = mono(&mut p, &impulse(secs(0.2)));
        rms(&out[secs(0.01)..secs(0.07)])
    };
    assert!(
        er_energy(0.0) > er_energy(1.0) * 1.5,
        "reflections die away faster at high Shape"
    );
}

#[test]
fn reverb_input_filter_cuts_outside_the_band_and_switches_off() {
    let level = |low: bool, high: bool, hz: f32, band: f64, width: f64| -> f64 {
        let mut p: Reverb = ready();
        set(
            &mut p,
            &[
                (rv::ENGINE, rv::ENGINE_STUDIO as f64),
                (rv::MIX, 1.0),
                (rv::PREDELAY, 0.0),
                (rv::DIFFUSE, -30.0),
                (rv::INPUT_LOW_ON, low as u32 as f64),
                (rv::INPUT_HIGH_ON, high as u32 as f64),
                (rv::INPUT_FREQ, band),
                (rv::INPUT_WIDTH, width),
                (rv::SPIN_ON, 0.0),
                (rv::CHORUS_ON, 0.0),
            ],
        );
        let input = sine(hz, SR, secs(0.5));
        let out = mono(&mut p, &input);
        tone(&out, hz as f64, secs(0.25), secs(0.2))
    };
    let on = level(true, true, 80.0, 1_000.0, 2.0);
    let off = level(false, false, 80.0, 1_000.0, 2.0);
    assert!(off > on * 10.0, "bypassed {off} vs filtered {on}");
}

#[test]
fn reverb_stays_bounded_and_finite_at_extreme_settings() {
    let mut p: Reverb = ready();
    set(
        &mut p,
        &[
            (rv::ENGINE, rv::ENGINE_STUDIO as f64),
            (rv::SIZE, 1.0),
            (rv::DECAY, 60.0),
            (rv::DIFFUSION, 1.0),
            (rv::SCALE, 1.0),
            (rv::DENSITY, 3.0),
            (rv::CHORUS_AMOUNT, 4.0),
            (rv::SPIN_AMOUNT, 15.0),
            (rv::SPIN_RATE, 1_300.0),
            (rv::MIX, 1.0),
            (rv::FREEZE, 1.0),
            (rv::CUT, 0.0),
            (rv::FLAT, 1.0),
            (rv::REFLECT, 6.0),
            (rv::DIFFUSE, 6.0),
        ],
    );
    let loud = sine(440.0, SR, secs(20.0));
    let out = mono(&mut p, &loud);
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak(&out) < 40.0, "{}", peak(&out));
}

// ------------------------------------------------------------------------ Hybrid Reverb

fn user_ir(left: Vec<f32>, right: Vec<f32>) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer {
        name: "ir".into(),
        left,
        right,
        sample_rate: SR,
    })
}

fn hybrid_with(ir: Option<Arc<SampleBuffer>>) -> Box<dyn Processor> {
    let info = find("rlive.hybrid_reverb").unwrap();
    let mut p = (info.create)(&rlive_devices::DeviceInit {
        samples: vec![ir],
        state: None,
    });
    p.activate(SR, BLOCK);
    set(&mut *p, &[(hr::MIX, 100.0)]);
    p
}

fn hybrid() -> Box<dyn Processor> {
    hybrid_with(None)
}

fn convolve(input: &[f32], ir: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; input.len()];
    for (n, x) in input.iter().enumerate() {
        if *x == 0.0 {
            continue;
        }
        for (k, h) in ir.iter().enumerate() {
            if n + k >= out.len() {
                break;
            }
            out[n + k] += x * h;
        }
    }
    out
}

#[test]
fn hybrid_convolution_of_a_user_ir_matches_direct_convolution() {
    let ir_l = noise(3_000, 77, 0.8);
    let ir_r: Vec<f32> = noise(3_000, 78, 0.5);
    let sample = user_ir(ir_l.clone(), ir_r.clone());
    let gain = hybrid_ir::ir_gain(&sample.left, &sample.right);
    let mut p = hybrid_with(Some(sample));
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
        ],
    );
    let input = noise(secs(0.2), 5, 0.5);
    let (l, r) = render(&mut *p, &input, &input);
    let want_l: Vec<f32> = convolve(&input, &ir_l).iter().map(|s| s * gain).collect();
    let want_r: Vec<f32> = convolve(&input, &ir_r).iter().map(|s| s * gain).collect();
    let lat = p.latency() as usize;
    assert_eq!(lat, hr::PARTITION);
    let error_l: f32 = l[lat..]
        .iter()
        .zip(&want_l)
        .fold(0.0, |m, (a, b)| m.max((a - b).abs()));
    let error_r: f32 = r[lat..]
        .iter()
        .zip(&want_r)
        .fold(0.0, |m, (a, b)| m.max((a - b).abs()));
    assert!(error_l < 3e-3, "left {error_l}");
    assert!(error_r < 3e-3, "right {error_r}");
}

#[test]
fn hybrid_generated_ir_matches_direct_convolution_with_the_generated_response() {
    let library = hybrid_ir::library(SR);
    let source = library.get(4, 1); // Halls, the second of them
    let mut p = hybrid();
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, 4.0),
            (hr::IR_SELECT, 1.0),
        ],
    );
    let input = noise(secs(0.5), 9, 0.5);
    let out = mono(&mut *p, &input);
    let lat = p.latency() as usize;
    let n = secs(0.5) - lat;
    let truncated: Vec<f32> = source.left[..source.left.len().min(n)]
        .iter()
        .map(|s| s * source.gain)
        .collect();
    let want = convolve(&input, &truncated);
    let error: f32 = out[lat..n + lat]
        .iter()
        .zip(&want[..n])
        .fold(0.0, |m, (a, b)| m.max((a - b).abs()));
    let level = peak(&out[lat..]);
    assert!(error < 2e-3 * level.max(1.0), "error {error} level {level}");
}

#[test]
fn hybrid_routing_modes_connect_the_engines_as_the_manual_says() {
    let input = noise(secs(0.5), 21, 0.5);
    let run = |routing: usize, blend: f64, ir: f64| -> Vec<f32> {
        let mut p = hybrid();
        set(
            &mut *p,
            &[
                (hr::ROUTING, routing as f64),
                (hr::BLEND, blend),
                (hr::IR_CATEGORY, ir),
                (hr::MODULATION, 0.0),
            ],
        );
        mono(&mut *p, &input)
    };
    let tolerance = |a: &[f32], b: &[f32], eps: f32, what: &str| {
        let max = a
            .iter()
            .zip(b)
            .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
        assert!(max <= eps, "{what}: {max}");
    };
    let convolution = run(hr::ROUTE_CONVOLUTION, 50.0, 1.0);
    let algorithm = run(hr::ROUTE_ALGORITHM, 50.0, 1.0);
    assert!(rms(&convolution) > 1e-3 && rms(&algorithm) > 1e-3);
    // Blend does nothing in Algorithm or Convolution.
    tolerance(
        &run(hr::ROUTE_ALGORITHM, 10.0, 1.0),
        &algorithm,
        1e-6,
        "Algorithm ignores Blend",
    );
    tolerance(
        &run(hr::ROUTE_CONVOLUTION, 90.0, 1.0),
        &convolution,
        1e-6,
        "Convolution ignores Blend",
    );
    // Algorithm routing is independent of the impulse response.
    tolerance(
        &run(hr::ROUTE_ALGORITHM, 50.0, 4.0),
        &algorithm,
        1e-6,
        "no IR in Algorithm",
    );
    // Parallel at the ends of Blend is each engine alone.
    tolerance(
        &run(hr::ROUTE_PARALLEL, 100.0, 1.0),
        &convolution,
        1e-6,
        "Parallel 100/0",
    );
    tolerance(
        &run(hr::ROUTE_PARALLEL, 0.0, 1.0),
        &algorithm,
        1e-6,
        "Parallel 0/100",
    );
    // Halfway is the average of the two.
    let middle = run(hr::ROUTE_PARALLEL, 50.0, 1.0);
    let average: Vec<f32> = convolution
        .iter()
        .zip(&algorithm)
        .map(|(a, b)| 0.5 * (a + b))
        .collect();
    tolerance(&middle, &average, 1e-5, "Parallel 50/50");
    // Serial feeds the convolution into the algorithm.
    tolerance(
        &run(hr::ROUTE_SERIAL, 100.0, 1.0),
        &convolution,
        1e-6,
        "Serial 100/0",
    );
    let serial_end = run(hr::ROUTE_SERIAL, 0.0, 1.0);
    let difference: f32 = serial_end
        .iter()
        .zip(&algorithm)
        .fold(0.0, |m, (a, b)| m.max((a - b).abs()));
    assert!(
        difference > 1e-3,
        "Serial 0/100 is fed by the convolution, so unlike Algorithm alone"
    );
}

#[test]
fn hybrid_attack_decay_and_size_shape_the_response() {
    let constant = vec![0.5f32; secs(0.8)];
    let ir_gain = hybrid_ir::ir_gain(&constant, &constant);
    let response = |attack: f64, decay: f64, size: f64| -> Vec<f32> {
        let sample = user_ir(constant.clone(), constant.clone());
        let mut p = hybrid_with(Some(sample));
        set(
            &mut *p,
            &[
                (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
                (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
                (hr::IR_ATTACK, attack),
                (hr::IR_DECAY, decay),
                (hr::IR_SIZE, size),
            ],
        );
        let out = mono(&mut *p, &impulse(secs(1.0)));
        out[p.latency() as usize..].to_vec()
    };
    let plain = response(0.0, hr::IR_DECAY_OFF_MS, 100.0);
    near(
        plain[secs(0.1)] as f64,
        (0.5 * ir_gain) as f64,
        1e-3,
        "unshaped",
    );

    // Attack is a linear ramp over its time.
    let attacked = response(200.0, hr::IR_DECAY_OFF_MS, 100.0);
    near(
        (attacked[secs(0.1)] / plain[secs(0.1)]) as f64,
        0.5,
        0.01,
        "halfway up the attack",
    );

    // Decay reaches -60 dB at its time.
    let decayed = response(0.0, 200.0, 100.0);
    let db = 20.0 * ((decayed[secs(0.2)] / plain[secs(0.1)]) as f64).log10();
    near(db, -60.0, 0.5, "IR Decay is a -60 dB time");

    // Size stretches the response in time.
    let shorter = response(0.0, hr::IR_DECAY_OFF_MS, 50.0);
    assert!(shorter[secs(0.3)].abs() > 1e-3);
    assert!(shorter[secs(0.5)].abs() < 1e-4, "ends halfway through");
}

#[test]
fn hybrid_switching_impulse_responses_does_not_click() {
    let mut p = hybrid();
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, 3.0),
        ],
    );
    let tone_in = sine(150.0, SR, secs(1.2));
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (tone_in.clone(), tone_in.clone());
    let change = secs(0.8);
    let change_block = change / BLOCK;
    let switch = [
        ParamEvent {
            offset: 0,
            id: ParamId(hr::IR_CATEGORY),
            value: 1.0,
        },
        ParamEvent {
            offset: 0,
            id: ParamId(hr::IR_SELECT),
            value: 2.0,
        },
    ];
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        let params = if i == change_block { &switch[..] } else { &[] };
        assert_no_alloc(|| h.process(&mut *p, cl, cr, &[], params));
    }
    let before = max_step(&l[secs(0.5)..change]);
    let across = max_step(&l[change..secs(1.15)]);
    assert!(
        across < before * 2.0,
        "across the swap {across} vs before {before}"
    );
    assert!(before > 1e-4);
    // The output after the swap is different: the new response really is playing.
    let settled = &l[change + secs(0.3)..];
    assert!(rms(settled) > 1e-4, "still sounding");
}

#[test]
fn hybrid_reset_keeps_the_ir_and_silences_it() {
    let mut p = hybrid();
    set(&mut *p, &[(hr::ROUTING, hr::ROUTE_CONVOLUTION as f64)]);
    let burst = noise(secs(0.2), 1, 0.5);
    let mut h = Harness::new(SR);
    render_with(&mut *p, &burst, &burst, &mut h);
    p.reset();
    let quiet = silence(secs(0.2));
    let (l, _) = render_with(&mut *p, &quiet, &quiet, &mut h);
    assert!(peak(&l) < 1e-9, "{}", peak(&l));
    let (again, _) = render_with(&mut *p, &burst, &burst, &mut h);
    assert!(peak(&again) > 1e-3, "the response is still there");
}

#[test]
fn hybrid_predelay_delays_the_engines_exactly_and_sync_follows_tempo() {
    let unit = user_ir(vec![1.0], vec![1.0]);
    let mut p = hybrid_with(Some(unit));
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
            (hr::PREDELAY, 25.0),
        ],
    );
    let out = mono(&mut *p, &impulse(secs(0.2)));
    let lat = p.latency() as usize;
    assert_eq!(onset(&out, 0.1), Some(lat + secs(0.025)));

    // Synced at 120 BPM: a 1/4 note is half a second.
    set(
        &mut *p,
        &[
            (hr::PREDELAY_SYNC, 1.0),
            (hr::PREDELAY_DIVISION, 12.0), // 1/4
        ],
    );
    p.reset();
    let out = mono(&mut *p, &impulse(secs(1.0)));
    assert_eq!(onset(&out, 0.1), Some(lat + secs(0.5)));
}

#[test]
fn hybrid_predelay_feedback_repeats_and_each_mode_has_its_own_amount() {
    let unit = user_ir(vec![1.0], vec![1.0]);
    let mut p = hybrid_with(Some(unit));
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
            (hr::PREDELAY, 20.0),
            (hr::FEEDBACK_TIME, 0.5),
            (hr::FEEDBACK_SYNC, 0.0),
        ],
    );
    let out = mono(&mut *p, &impulse(secs(0.2)));
    let lat = p.latency() as usize;
    near(
        out[lat + secs(0.04)] as f64,
        0.5 / 1.0,
        0.02,
        "first repeat",
    );
    near(out[lat + secs(0.06)] as f64, 0.25, 0.02, "second repeat");
    set(
        &mut *p,
        &[(hr::PREDELAY_SYNC, 1.0), (hr::PREDELAY_DIVISION, 12.0)],
    );
    p.reset();
    let out = mono(&mut *p, &impulse(secs(1.2)));
    assert!(
        out[lat + secs(1.0)].abs() < 1e-3,
        "sync mode reads its own Feedback, which is 0 here"
    );
}

#[test]
fn hybrid_send_scales_the_reverb_and_leaves_the_dry_signal_alone() {
    let mut p = hybrid();
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_ALGORITHM as f64),
            (hr::MIX, 50.0),
            (hr::SEND, -36.0),
        ],
    );
    let input = impulse(secs(0.5));
    let out = mono(&mut *p, &input);
    let lat = p.latency() as usize;
    near(out[lat] as f64, 0.5, 1e-4, "dry path unaffected by Send");
    let mut q = hybrid();
    set(
        &mut *q,
        &[
            (hr::ROUTING, hr::ROUTE_ALGORITHM as f64),
            (hr::MIX, 100.0),
            (hr::SEND, -36.0),
        ],
    );
    let wet_quiet = mono(&mut *q, &input);
    let mut r = hybrid();
    set(
        &mut *r,
        &[
            (hr::ROUTING, hr::ROUTE_ALGORITHM as f64),
            (hr::MIX, 100.0),
            (hr::SEND, 0.0),
        ],
    );
    let wet_loud = mono(&mut *r, &input);
    let db = 20.0 * (rms(&wet_loud[secs(0.05)..]) / rms(&wet_quiet[secs(0.05)..])).log10();
    near(db as f64, 36.0, 1.0, "Send gain");
}

/// A wet Algorithm-routing Hybrid Reverb with `algo` and otherwise-default algorithm controls.
fn algorithm(algo: usize, extra: &[(u32, f64)]) -> Box<dyn Processor> {
    let mut p = hybrid();
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_ALGORITHM as f64),
            (hr::ALGORITHM, algo as f64),
        ],
    );
    set(&mut *p, extra);
    p
}

#[test]
fn hybrid_decay_time_tracks_decay_on_every_algorithm() {
    for (algo, extra, name) in [
        (
            hr::DARK_HALL,
            vec![(hr::DAMPING, 0.0), (hr::MODULATION, 0.0)],
            "Dark Hall",
        ),
        (
            hr::QUARTZ,
            vec![
                (hr::DAMPING, 0.0),
                (hr::LO_DAMP, 0.0),
                (hr::MODULATION, 0.0),
            ],
            "Quartz",
        ),
        (
            hr::SHIMMER_ALGO,
            vec![
                (hr::DAMPING, 0.0),
                (hr::SHIMMER, 0.0),
                (hr::MODULATION, 0.0),
            ],
            "Shimmer",
        ),
        (
            hr::TIDES,
            vec![(hr::DAMPING, 0.0), (hr::TIDE, 0.0)],
            "Tides",
        ),
        (hr::PRISM, vec![], "Prism"),
    ] {
        for decay in [1.0f64, 3.0] {
            let mut p = algorithm(algo, &extra);
            set(&mut *p, &[(hr::DECAY, decay), (hr::SIZE, 50.0)]);
            let out = mono(&mut *p, &impulse(secs(decay * 2.5)));
            let measured = rt60(&out);
            near(
                measured / decay,
                1.0,
                0.2,
                &format!("{name} Decay {decay}: measured {measured}"),
            );
        }
    }
}

#[test]
fn hybrid_delay_is_an_extra_predelay_for_the_algorithm_section_alone() {
    let first = |delay: f64| -> usize {
        let mut p = algorithm(hr::DARK_HALL, &[(hr::DELAY, delay), (hr::MODULATION, 0.0)]);
        let out = mono(&mut *p, &impulse(secs(0.4)));
        onset(&out, 1e-3).unwrap()
    };
    assert_eq!(
        first(100.0),
        first(0.0) + secs(0.1),
        "Delay moves the algorithm by exactly that"
    );
}

#[test]
fn hybrid_freeze_holds_and_freeze_in_adds() {
    let mut p = algorithm(
        hr::DARK_HALL,
        &[(hr::DECAY, 1.0), (hr::MODULATION, 0.0), (hr::DAMPING, 0.0)],
    );
    let burst = noise(secs(0.5), 3, 0.3);
    let mut h = Harness::new(SR);
    render_with(&mut *p, &burst, &burst, &mut h);
    set(&mut *p, &[(hr::FREEZE, 1.0)]);
    let quiet = silence(secs(1.0));
    let (settle, _) = render_with(&mut *p, &quiet, &quiet, &mut h);
    let start = rms(&settle[secs(0.5)..]);
    let hold = silence(secs(8.0));
    let (held, _) = render_with(&mut *p, &hold, &hold, &mut h);
    let end = rms(&held[held.len() - secs(1.0)..]);
    near((end / start) as f64, 1.0, 0.1, "frozen tail");

    let more = noise(secs(1.0), 5, 0.3);
    set(&mut *p, &[(hr::FREEZE_IN, 1.0)]);
    let (added, _) = render_with(&mut *p, &more, &more, &mut h);
    assert!(
        rms(&added[added.len() - secs(0.2)..]) > end * 1.2,
        "input is added"
    );
}

#[test]
fn hybrid_damping_size_shape_mod_and_quartz_distance_do_what_the_manual_says() {
    // Damping darkens.
    let dark = |damping: f64| -> f64 {
        let mut p = algorithm(
            hr::DARK_HALL,
            &[
                (hr::DAMPING, damping),
                (hr::MODULATION, 0.0),
                (hr::DECAY, 3.0),
            ],
        );
        let input = noise(secs(0.3), 3, 0.3);
        let mut h = Harness::new(SR);
        render_with(&mut *p, &input, &input, &mut h);
        let quiet = silence(secs(1.0));
        let (out, _) = render_with(&mut *p, &quiet, &quiet, &mut h);
        tone(&out, 8_000.0, 0, out.len())
    };
    assert!(
        dark(0.0) > dark(90.0) * 4.0,
        "{} vs {}",
        dark(0.0),
        dark(90.0)
    );

    // Size scales when the first echo arrives.
    let first = |size: f64| -> usize {
        let mut p = algorithm(
            hr::PRISM,
            &[(hr::SIZE, size), (hr::DECAY, 4.0), (hr::MODULATION, 0.0)],
        );
        let lat = p.latency() as usize;
        let out = mono(&mut *p, &impulse(secs(0.5)));
        onset(&out[lat + 8..], 0.01).unwrap()
    };
    assert!(
        first(80.0) > first(30.0) * 2,
        "{} vs {}",
        first(80.0),
        first(30.0)
    );

    // Quartz Distance spreads its early reflections out.
    let first_echo = |distance: f64| -> usize {
        let mut p = algorithm(
            hr::QUARTZ,
            &[
                (hr::DISTANCE, distance),
                (hr::SIZE, 20.0),
                (hr::MODULATION, 0.0),
            ],
        );
        let lat = p.latency() as usize;
        let out = mono(&mut *p, &impulse(secs(0.5)));
        onset(&out[lat + 8..], 0.05).unwrap()
    };
    assert!(first_echo(90.0) > first_echo(10.0) + secs(0.02), "Distance");

    // Modulation sets the diffused sound moving: reruns differ from a static one.
    let moving = |depth: f64| -> Vec<f32> {
        let mut p = algorithm(hr::DARK_HALL, &[(hr::MODULATION, depth), (hr::DECAY, 2.0)]);
        mono(&mut *p, &impulse(secs(0.6)))
    };
    let (still, modded) = (moving(0.0), moving(100.0));
    let diff: f32 = still[secs(0.3)..]
        .iter()
        .zip(&modded[secs(0.3)..])
        .fold(0.0, |m, (a, b)| m.max((a - b).abs()));
    assert!(diff > 1e-4, "{diff}");

    // Shape 0 is sparser than Shape 100.
    // Shape goes from small and resonant to large and diffused: resonant reads as a peaky
    // spectrum, diffused as a smoother one.
    let spectral_peakiness = |shape: f64| -> f32 {
        let mut p = algorithm(
            hr::DARK_HALL,
            &[
                (hr::SHAPE, shape),
                (hr::SIZE, 30.0),
                (hr::MODULATION, 0.0),
                (hr::DAMPING, 0.0),
                (hr::DECAY, 3.0),
            ],
        );
        let out = mono(&mut *p, &impulse(secs(1.5)));
        let mut levels: Vec<f32> = (0..96)
            .map(|k| tone_amplitude(&out, 200.0 * 1.04f64.powi(k), SR) as f32)
            .collect();
        levels.sort_by(f32::total_cmp);
        levels[95] / levels[48].max(1e-9)
    };
    assert!(
        spectral_peakiness(0.0) > spectral_peakiness(100.0) * 1.5,
        "{} vs {}",
        spectral_peakiness(0.0),
        spectral_peakiness(100.0)
    );
}

#[test]
fn hybrid_bass_and_prism_multipliers_change_decay_of_their_bands() {
    let bands = |algo: usize, extra: &[(u32, f64)]| -> (f64, f64) {
        let mut p = algorithm(algo, extra);
        set(
            &mut *p,
            &[(hr::DECAY, 2.0), (hr::MODULATION, 0.0), (hr::DAMPING, 0.0)],
        );
        let input = noise(secs(0.5), 8, 0.3);
        let mut h = Harness::new(SR);
        let (loud, _) = render_with(&mut *p, &input, &input, &mut h);
        let quiet = silence(secs(3.0));
        let (out, _) = render_with(&mut *p, &quiet, &quiet, &mut h);
        let _ = loud;
        (
            tone(&out, 60.0, secs(1.5), secs(1.0)),
            tone(&out, 6_000.0, secs(1.5), secs(1.0)),
        )
    };
    let dark_flat = bands(hr::DARK_HALL, &[(hr::BASS_MULT, 1.0), (hr::BASS_X, 300.0)]);
    let dark_long = bands(hr::DARK_HALL, &[(hr::BASS_MULT, 4.0), (hr::BASS_X, 300.0)]);
    assert!(dark_long.0 > dark_flat.0 * 2.0, "Bass Mult lengthens lows");

    let low_long = bands(
        hr::PRISM,
        &[
            (hr::LOW_MULT, 4.0),
            (hr::HIGH_MULT, 0.2),
            (hr::XOVER, 1_000.0),
        ],
    );
    let high_long = bands(
        hr::PRISM,
        &[
            (hr::LOW_MULT, 0.2),
            (hr::HIGH_MULT, 4.0),
            (hr::XOVER, 1_000.0),
        ],
    );
    assert!(low_long.0 > high_long.0 * 5.0, "Low Mult");
    assert!(high_long.1 > low_long.1 * 5.0, "High Mult");

    // Quartz Lo Damp shortens the lows.
    let lo = |damp: f64| -> f64 {
        let mut p = algorithm(
            hr::QUARTZ,
            &[
                (hr::LO_DAMP, damp),
                (hr::DAMPING, 0.0),
                (hr::MODULATION, 0.0),
                (hr::DECAY, 3.0),
            ],
        );
        let input = noise(secs(0.5), 8, 0.3);
        let mut h = Harness::new(SR);
        render_with(&mut *p, &input, &input, &mut h);
        let quiet = silence(secs(2.0));
        let (out, _) = render_with(&mut *p, &quiet, &quiet, &mut h);
        tone(&out, 60.0, secs(1.0), secs(1.0))
    };
    assert!(lo(0.0) > lo(100.0) * 3.0, "{} vs {}", lo(0.0), lo(100.0));
}

#[test]
fn hybrid_shimmer_climbs_in_pitch_and_tides_ripples() {
    // Shimmer pitches the tail: energy turns up an octave above what went in, only when it's on.
    let shifted = |shimmer: f64, pitch: f64, low: f64, high: f64| -> f64 {
        let mut p = algorithm(
            hr::SHIMMER_ALGO,
            &[
                (hr::SHIMMER, shimmer),
                (hr::PITCH, pitch),
                (hr::DIFFUSION, 40.0),
                (hr::DAMPING, 0.0),
                (hr::MODULATION, 0.0),
                (hr::DECAY, 4.0),
                (hr::SIZE, 35.0),
            ],
        );
        let input: Vec<f32> = sine(330.0, SR, secs(0.2)).iter().map(|s| s * 0.5).collect();
        let mut h = Harness::new(SR);
        render_with(&mut *p, &input, &input, &mut h);
        let quiet = silence(secs(0.9));
        let (out, _) = render_with(&mut *p, &quiet, &quiet, &mut h);
        band_power(&out[secs(0.05)..], low, high)
    };
    let octave_up = shifted(100.0, 12.0, 600.0, 720.0);
    let no_shimmer = shifted(0.0, 12.0, 600.0, 720.0);
    assert!(octave_up > no_shimmer * 20.0, "{octave_up} vs {no_shimmer}");
    let octave_down = shifted(100.0, -12.0, 140.0, 190.0);
    assert!(
        octave_down > shifted(0.0, -12.0, 140.0, 190.0) * 20.0,
        "Pitch goes down too"
    );

    // Tides: with Tide up, a held wet tone pumps at the rate; with it down, it doesn't.
    let ripple = |tide: f64| -> f32 {
        let mut p = algorithm(
            hr::TIDES,
            &[
                (hr::TIDE, tide),
                (hr::DECAY, 2.0),
                (hr::DAMPING, 0.0),
                (hr::RATE, 12.0), // 1/4
                (hr::WAVE, 50.0),
            ],
        );
        // The centre of a tide band, so the ripple on it isn't averaged away by its neighbours.
        let input = sine(699.84, SR, secs(5.0));
        let mut h = Harness::new(SR);
        // Stopped, so the rate-synced LFO free-runs instead of following a song position that
        // the harness doesn't advance.
        h.transport.playing = false;
        let (out, _) = render_with(&mut *p, &input, &input, &mut h);
        // Past the build-up of the tail, where Tide 0 settles to a steady level.
        let window = secs(0.05);
        let levels: Vec<f32> = out[secs(3.0)..].chunks(window).map(rms).collect();
        let (mn, mx) = levels
            .iter()
            .fold((f32::MAX, 0.0f32), |(a, b), v| (a.min(*v), b.max(*v)));
        mx / mn.max(1e-6)
    };
    let (moving, still) = (ripple(100.0), ripple(0.0));
    assert!(moving > still * 2.0, "Tide 100 {moving} vs Tide 0 {still}");
}

#[test]
fn hybrid_tides_phase_offsets_the_stereo_channels() {
    // The two channels' tank outputs are unrelated; the shared thing is the ripple on them.
    let envelope_match = |phase: f64| -> f32 {
        let mut p = algorithm(
            hr::TIDES,
            &[
                (hr::TIDE, 100.0),
                (hr::PHASE, phase),
                (hr::DECAY, 8.0),
                (hr::DAMPING, 0.0),
                (hr::RATE, 12.0),
            ],
        );
        // The centre of a tide band, as for the ripple test.
        let input = sine(699.84, SR, secs(4.0));
        let mut h = Harness::new(SR);
        h.transport.playing = false;
        let (l, r) = render_with(&mut *p, &input, &input, &mut h);
        let envelope = |s: &[f32]| -> Vec<f32> {
            let levels: Vec<f32> = s[secs(1.5)..].chunks(secs(0.02)).map(rms).collect();
            let mean = levels.iter().sum::<f32>() / levels.len() as f32;
            levels.iter().map(|v| v - mean).collect()
        };
        let (a, b) = (envelope(&l), envelope(&r));
        let dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        dot / (a.iter().map(|x| x * x).sum::<f32>() * b.iter().map(|x| x * x).sum::<f32>())
            .sqrt()
            .max(1e-9)
    };
    let (same, opposite) = (envelope_match(0.0), envelope_match(180.0));
    assert!(
        same > opposite + 0.2,
        "Phase 0: {same}, Phase 180: {opposite}"
    );
}

#[test]
fn hybrid_eq_pass_slopes_and_shelves_measure_correctly() {
    let level = |mode: f64, slope: f64, hz: f32, band_id: u32, cut: f64| -> f64 {
        let unit = user_ir(vec![1.0], vec![1.0]);
        let mut p = hybrid_with(Some(unit));
        let (mode_id, slope_id, freq_id) = if band_id == 0 {
            (hr::EQ_LOW_MODE, hr::EQ_LOW_SLOPE, hr::EQ_LOW_FREQ)
        } else {
            (hr::EQ_HIGH_MODE, hr::EQ_HIGH_SLOPE, hr::EQ_HIGH_FREQ)
        };
        set(
            &mut *p,
            &[
                (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
                (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
                (hr::EQ_ON, 1.0),
                (mode_id, mode),
                (slope_id, slope),
                (freq_id, cut),
            ],
        );
        let input = sine(hz, SR, secs(0.6));
        let out = mono(&mut *p, &input);
        tone(&out, hz as f64, secs(0.4), secs(0.15))
    };
    let reference = level(0.0, 1.0, 1_000.0, 0, 20.0);
    // High-pass at 1 kHz: an octave below, 6 dB/oct and 96 dB/oct.
    let hp6 = level(1.0, 0.0, 500.0, 0, 1_000.0);
    near(
        20.0 * (hp6 / reference).log10(),
        -6.0,
        2.0,
        "HP 6 dB/oct, one octave down",
    );
    let hp96 = level(1.0, 6.0, 500.0, 0, 1_000.0);
    near(
        20.0 * (hp96 / reference).log10(),
        -96.0,
        6.0,
        "HP 96 dB/oct, one octave down",
    );
    // Low-pass at 1 kHz: an octave above, 96 dB/oct.
    let lp96 = level(1.0, 6.0, 2_000.0, 1, 1_000.0);
    near(
        20.0 * (lp96 / reference).log10(),
        -96.0,
        6.0,
        "LP 96 dB/oct, one octave up",
    );
}

#[test]
fn hybrid_eq_pre_algo_puts_it_in_front_of_the_algorithm() {
    let tail = |pre: f64| -> f64 {
        let mut p = algorithm(
            hr::DARK_HALL,
            &[
                (hr::EQ_ON, 1.0),
                (hr::EQ_PRE_ALGO, pre),
                (hr::EQ_LOW_MODE, 1.0),
                (hr::EQ_LOW_SLOPE, 3.0),
                (hr::EQ_LOW_FREQ, 2_000.0),
                (hr::DECAY, 3.0),
                (hr::MODULATION, 0.0),
                (hr::DAMPING, 0.0),
            ],
        );
        let input = sine(300.0, SR, secs(1.0));
        let out = mono(&mut *p, &input);
        tone(&out, 300.0, secs(0.5), secs(0.4))
    };
    // A low cut before the algorithm starves it of low notes; after, it filters the tail by the
    // same amount, but both must attenuate a held low tone against no EQ at all.
    let (pre, post) = (tail(1.0), tail(0.0));
    let mut none = algorithm(
        hr::DARK_HALL,
        &[(hr::DECAY, 3.0), (hr::MODULATION, 0.0), (hr::DAMPING, 0.0)],
    );
    let baseline = tone(
        &mono(&mut *none, &sine(300.0, SR, secs(1.0))),
        300.0,
        secs(0.5),
        secs(0.4),
    );
    assert!(pre < baseline * 0.3, "{pre} vs {baseline}");
    assert!(post < baseline * 0.3, "{post} vs {baseline}");
}

#[test]
fn hybrid_output_section_widens_degrades_and_monos_the_bass() {
    let unit = user_ir(vec![1.0], vec![1.0]);
    let mut p = hybrid_with(Some(unit.clone()));
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
            (hr::STEREO, 0.0),
        ],
    );
    let (left, right) = (noise(secs(0.2), 1, 0.3), noise(secs(0.2), 2, 0.3));
    let (l, r) = render(&mut *p, &left, &right);
    let lat = p.latency() as usize;
    assert!(
        l[lat + 100..]
            .iter()
            .zip(&r[lat + 100..])
            .all(|(a, b)| (a - b).abs() < 1e-6),
        "0% is mono"
    );

    let width_of = |stereo: f64| -> f32 {
        let mut q = hybrid_with(Some(unit.clone()));
        set(
            &mut *q,
            &[
                (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
                (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
                (hr::STEREO, stereo),
            ],
        );
        let (l, r) = render(&mut *q, &left, &right);
        let side: Vec<f32> = l[lat..].iter().zip(&r[lat..]).map(|(a, b)| a - b).collect();
        rms(&side)
    };
    near(
        (width_of(200.0) / width_of(100.0)) as f64,
        2.0,
        0.05,
        "Stereo 200% doubles the sides",
    );

    // Bass Mono: a low tone in one side reaches both.
    let hz = 60.0f32;
    let mono_input = sine(hz, SR, secs(0.6));
    let quiet = silence(secs(0.6));
    let mut b = hybrid_with(Some(unit.clone()));
    set(
        &mut *b,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
            (hr::BASS_MONO, 1.0),
        ],
    );
    let (bl, br) = render(&mut *b, &mono_input, &quiet);
    let (a_l, a_r) = (
        tone(&bl, 60.0, secs(0.3), secs(0.25)),
        tone(&br, 60.0, secs(0.3), secs(0.25)),
    );
    near(a_l / a_r, 1.0, 0.05, "60 Hz is mono");

    // Well above 180 Hz stays stereo.
    let hi_input = sine(3_000.0, SR, secs(0.6));
    let mut c = hybrid_with(Some(unit.clone()));
    set(
        &mut *c,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
            (hr::BASS_MONO, 1.0),
        ],
    );
    let (cl, cr) = render(&mut *c, &hi_input, &quiet);
    assert!(
        tone(&cl, 3_000.0, secs(0.3), secs(0.25)) > tone(&cr, 3_000.0, secs(0.3), secs(0.25)) * 8.0,
        "3 kHz stays where it was"
    );

    // Vintage: a preset quantises and holds the wet signal, and Off doesn't.
    let unique_values = |preset: f64| -> usize {
        let mut q = hybrid_with(Some(unit.clone()));
        set(
            &mut *q,
            &[
                (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
                (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
                (hr::VINTAGE_PRESET, preset),
                (hr::VINTAGE, 100.0),
            ],
        );
        let input = sine(220.0, SR, secs(0.2));
        let (out, _) = render(&mut *q, &input, &input);
        let mut values: Vec<u32> = out[lat + 100..].iter().map(|s| s.to_bits()).collect();
        values.sort_unstable();
        values.dedup();
        values.len()
    };
    let (off, subtle, extreme) = (unique_values(0.0), unique_values(1.0), unique_values(4.0));
    assert!(extreme < subtle && subtle < off, "{off} {subtle} {extreme}");
    assert_eq!(hr::BASS_MONO_HZ, 180.0);
}

#[test]
fn hybrid_user_ir_set_at_runtime_replaces_the_slot_without_allocating() {
    let mut p = hybrid();
    set(
        &mut *p,
        &[
            (hr::ROUTING, hr::ROUTE_CONVOLUTION as f64),
            (hr::IR_CATEGORY, hybrid_ir::USER_CATEGORY as f64),
        ],
    );
    let quiet_at_first = mono(&mut *p, &impulse(secs(0.05)));
    assert!(peak(&quiet_at_first) < 1e-9, "no file yet");

    let first: Arc<SampleBuffer> = user_ir(vec![1.0], vec![1.0]);
    let second: Arc<SampleBuffer> = user_ir(vec![0.0, 0.0, 0.0, 1.0], vec![0.0, 0.0, 0.0, 1.0]);
    let handed: Resource = first.clone();
    let retired = assert_no_alloc(|| p.set_resource(0, handed));
    assert!(retired.is_none(), "the slot started out empty");

    // Rebuilt over the next blocks and heard after the crossfade.
    let wait = silence(secs(0.35));
    let mut h = Harness::new(SR);
    render_with(&mut *p, &wait, &wait, &mut h);
    let (out, _) = render_with(&mut *p, &impulse(4_096), &impulse(4_096), &mut h);
    let lat = p.latency() as usize;
    near(out[lat] as f64, 1.0, 1e-3, "the file is playing");

    // A replacement comes back to the engine for retirement.
    let again: Resource = second.clone();
    let retired = assert_no_alloc(|| p.set_resource(0, again));
    let back: Arc<SampleBuffer> = retired.expect("the old one comes back").downcast().unwrap();
    assert!(Arc::ptr_eq(&back, &first));

    // A resource of the wrong type or slot is refused.
    let wrong: Resource = Arc::new(7u32);
    assert!(p.set_resource(0, wrong).is_some());
    let wrong_slot: Resource = second.clone();
    assert!(p.set_resource(5, wrong_slot).is_some());
}

#[test]
fn hybrid_stays_finite_and_bounded_at_extreme_settings() {
    for algo in [
        hr::DARK_HALL,
        hr::QUARTZ,
        hr::SHIMMER_ALGO,
        hr::TIDES,
        hr::PRISM,
    ] {
        let mut p = algorithm(
            algo,
            &[
                (hr::DECAY, 60.0),
                (hr::SIZE, 100.0),
                (hr::FREEZE, 1.0),
                (hr::FREEZE_IN, 1.0),
                (hr::DAMPING, 0.0),
                (hr::MODULATION, 100.0),
                (hr::SHIMMER, 100.0),
                (hr::PITCH, 24.0),
                (hr::BASS_MULT, 4.0),
                (hr::LOW_MULT, 4.0),
                (hr::HIGH_MULT, 4.0),
                (hr::TIDE, 100.0),
            ],
        );
        let loud: Vec<f32> = sine(440.0, SR, secs(6.0)).iter().map(|s| s * 1.0).collect();
        let out = mono(&mut *p, &loud);
        assert!(out.iter().all(|s| s.is_finite()), "algorithm {algo}");
        assert!(peak(&out) < 30.0, "algorithm {algo}: {}", peak(&out));
    }
}

#[test]
fn hybrid_generated_ir_kinds_cover_every_category() {
    assert_eq!(hybrid_ir::CATEGORIES.len(), 10);
    assert_eq!(hybrid_ir::CATEGORIES[hybrid_ir::USER_CATEGORY], "User");
    let library = hybrid_ir::library(SR);
    for category in 0..hybrid_ir::USER_CATEGORY {
        for i in 0..hybrid_ir::PER_CATEGORY {
            let ir = library.get(category, i);
            assert!(!ir.left.is_empty(), "{category}/{i}");
            assert!(ir.gain.is_finite() && ir.gain > 0.0);
        }
    }
    let _ = IrKind::Plate;
}

// -------------------------------------------------------------------------- Resonators

fn resonators(extra: &[(u32, f64)]) -> res::Resonators {
    let mut p: res::Resonators = ready();
    set(
        &mut p,
        &[
            (res::ON2, 0.0),
            (res::ON3, 0.0),
            (res::ON4, 0.0),
            (res::ON5, 0.0),
        ],
    );
    set(&mut p, extra);
    p
}

fn midi_hz(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

#[test]
fn resonators_ring_on_the_right_notes_in_both_modes() {
    for mode in [res::MODE_A, res::MODE_B] {
        for note in [36.0f64, 48.0, 60.0, 72.0] {
            let mut p = resonators(&[
                (res::NOTE, note),
                (res::MODE, mode as f64),
                (res::COLOR, 100.0),
                (res::DECAY, 20_000.0),
                (res::CONST, 1.0),
            ]);
            let out = mono(&mut p, &impulse(secs(1.0)));
            let want = midi_hz(note);
            let found = peak_near(&out[secs(0.05)..], want, 80.0, 1.0);
            let off = cents(found, want);
            assert!(off.abs() < 3.0, "mode {mode} note {note}: {off} cents");
        }
    }
}

#[test]
fn resonators_mode_b_has_odd_harmonics_and_mode_a_all_of_them() {
    let harmonics = |mode: usize| -> (f64, f64) {
        let mut p = resonators(&[
            (res::NOTE, 48.0),
            (res::MODE, mode as f64),
            (res::COLOR, 100.0),
            (res::DECAY, 5_000.0),
            (res::CONST, 1.0),
        ]);
        let out = mono(&mut p, &impulse(secs(1.0)));
        let f = midi_hz(48.0);
        (
            tone(&out, f, 2_000, secs(0.4)),
            tone(&out, f * 2.0, 2_000, secs(0.4)),
        )
    };
    let (a1, a2) = harmonics(res::MODE_A);
    let (b1, b2) = harmonics(res::MODE_B);
    assert!(a2 > a1 * 0.4, "Mode A keeps the even harmonics: {a1} {a2}");
    assert!(b2 < b1 * 0.03, "Mode B has no even harmonics: {b1} {b2}");
}

#[test]
fn resonators_decay_time_is_the_control_when_const_and_shortens_with_pitch_otherwise() {
    let ring = |note: f64, constant: bool, decay: f64| -> f64 {
        let mut p = resonators(&[
            (res::NOTE, note),
            (res::COLOR, 100.0),
            (res::DECAY, decay),
            (res::CONST, constant as u32 as f64),
        ]);
        let out = mono(&mut p, &impulse(secs(decay * 0.001 * 2.0)));
        let f = midi_hz(note);
        let (a1, a2) = (
            tone(&out, f, secs(0.02), secs(0.1)),
            tone(&out, f, secs(0.32), secs(0.1)),
        );
        60.0 * 0.3 / (20.0 * (a1 / a2).log10())
    };
    for note in [48.0f64, 72.0] {
        let t = ring(note, true, 1_000.0);
        near(t, 1.0, 0.1, &format!("Const at {note}"));
    }
    let (low, high) = (ring(48.0, false, 1_000.0), ring(72.0, false, 1_000.0));
    let expected = 2f64.powi(2);
    near(
        low / high,
        expected,
        0.15 * expected,
        "low notes last longer",
    );
    near(
        low,
        1_000.0 * res::REFERENCE_HZ as f64 / midi_hz(48.0) / 1000.0,
        0.15,
        "Decay at the reference",
    );
}

#[test]
fn resonators_are_scale_aware_and_fall_back_to_semitones_without_a_scale() {
    // C major, Use Current Scale, Note on the root.
    let c_major = ScaleInfo {
        root: 0,
        mask: 0b1010_1011_0101,
    };
    let pitches = res::voice_pitches(60.0, [0.0; 5], [2.0, 0.0, 0.0, 0.0], Some(c_major));
    assert_eq!(pitches[0], 48.0, "the root in octave 2");
    assert_eq!(pitches[1], 52.0, "two degrees up from C is E");
    let chromatic = res::voice_pitches(60.0, [0.0; 5], [2.0, 0.0, 0.0, 0.0], None);
    assert_eq!(chromatic[0], 60.0);
    assert_eq!(chromatic[1], 62.0);
    // Degrees, not semitones, are what Note steps by.
    let up = res::voice_pitches(61.0, [0.0; 5], [0.0; 4], Some(c_major));
    assert_eq!(up[0], 50.0, "one degree above C is D");

    let run = |scale: Option<ScaleInfo>| -> f64 {
        let mut p = resonators(&[
            (res::USE_SCALE, 1.0),
            (res::COLOR, 100.0),
            (res::DECAY, 20_000.0),
            (res::CONST, 1.0),
        ]);
        let mut h = Harness::new(SR);
        h.transport.scale = scale;
        let (out, _) = render_with(&mut p, &impulse(secs(1.0)), &impulse(secs(1.0)), &mut h);
        peak_near(&out[secs(0.05)..], midi_hz(48.0), 2_500.0, 3.0)
    };
    let aware = run(Some(c_major));
    assert!(cents(aware, midi_hz(48.0)).abs() < 5.0, "{aware}");
    let plain = run(None);
    assert!(
        cents(plain, midi_hz(60.0)).abs() < 5.0,
        "without a scale it's semitones: {plain}"
    );
}

#[test]
fn resonators_ii_to_v_follow_note_and_i_can_be_off_without_stopping_them() {
    let mut p = resonators(&[
        (res::ON1, 0.0),
        (res::ON2, 1.0),
        (res::PITCH2, 7.0),
        (res::NOTE, 48.0),
        (res::COLOR, 100.0),
        (res::DECAY, 10_000.0),
        (res::CONST, 1.0),
    ]);
    let out = mono(&mut p, &impulse(secs(1.0)));
    let want = midi_hz(55.0);
    let found = peak_near(&out[secs(0.05)..], want, 400.0, 2.0);
    assert!(cents(found, want).abs() < 4.0, "{found}");
}

#[test]
fn resonators_i_hears_both_channels_and_ii_iv_left_and_iii_v_right() {
    let ring_for = |left: bool, right: bool, on: u32| -> f32 {
        let mut p: res::Resonators = ready();
        set(
            &mut p,
            &[
                (res::ON1, 0.0),
                (res::ON2, 0.0),
                (res::ON3, 0.0),
                (res::ON4, 0.0),
                (res::ON5, 0.0),
                (on, 1.0),
                (res::COLOR, 100.0),
                (res::DECAY, 2_000.0),
                (res::CONST, 1.0),
            ],
        );
        let imp = impulse(secs(0.2));
        let s = silence(secs(0.2));
        let (l_in, r_in) = (
            if left { imp.as_slice() } else { s.as_slice() },
            if right { imp.as_slice() } else { s.as_slice() },
        );
        let (l, r) = render(&mut p, l_in, r_in);
        rms(&l).max(rms(&r))
    };
    assert!(ring_for(true, false, res::ON1) > 1e-3, "I hears the left");
    assert!(ring_for(false, true, res::ON1) > 1e-3, "and the right");
    assert!(ring_for(true, false, res::ON2) > 1e-3, "II hears the left");
    assert!(
        ring_for(false, true, res::ON2) < 1e-6,
        "II doesn't hear the right"
    );
    assert!(
        ring_for(false, true, res::ON3) > 1e-3,
        "III hears the right"
    );
    assert!(
        ring_for(true, false, res::ON3) < 1e-6,
        "III doesn't hear the left"
    );
    assert!(ring_for(true, false, res::ON4) > 1e-3, "IV hears the left");
    assert!(ring_for(false, true, res::ON5) > 1e-3, "V hears the right");
}

#[test]
fn resonators_width_blends_the_side_voices_into_mono() {
    let run = |width: f64| -> (Vec<f32>, Vec<f32>) {
        let mut p: res::Resonators = ready();
        set(
            &mut p,
            &[
                (res::ON1, 0.0),
                (res::ON2, 1.0),
                (res::ON3, 0.0),
                (res::ON4, 0.0),
                (res::ON5, 0.0),
                (res::WIDTH, width),
                (res::COLOR, 100.0),
            ],
        );
        let (l, r) = render(&mut p, &impulse(secs(0.2)), &impulse(secs(0.2)));
        (l, r)
    };
    let (l, r) = run(100.0);
    assert!(
        rms(&l) > 1e-3 && rms(&r) < 1e-6,
        "full width: II only on the left"
    );
    let (l, r) = run(0.0);
    assert!(
        l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6),
        "zero width: mono"
    );
    assert!(rms(&l) > 1e-3);
}

#[test]
fn resonators_input_filter_gain_and_dry_wet_do_their_jobs() {
    let rung = |filter: f64, freq: f64, gain: f64| -> f32 {
        let mut p = resonators(&[
            (res::FILTER_TYPE, filter),
            (res::FILTER_FREQ, freq),
            (res::NOTE, 60.0),
            (res::OUTPUT_GAIN, gain),
            (res::COLOR, 100.0),
        ]);
        // Quiet enough not to touch the loops' saturation.
        let input: Vec<f32> = sine(261.63, SR, secs(0.5))
            .iter()
            .map(|s| s * 0.02)
            .collect();
        let out = mono(&mut p, &input);
        tone(&out, 261.63, secs(0.25), secs(0.2)) as f32
    };
    let open = rung(0.0, 20_000.0, 0.0);
    assert!(
        rung(0.0, 40.0, 0.0) < open * 0.05,
        "a low-pass below the note starves it"
    );
    assert!(
        rung(2.0, 8_000.0, 0.0) < open * 0.05,
        "a high-pass above the note starves it"
    );
    near(
        (rung(0.0, 20_000.0, 12.0) / open) as f64,
        3.98,
        0.2,
        "Gain +12 dB",
    );

    let mut p = resonators(&[(res::MIX, 0.0), (res::NOTE, 60.0)]);
    let dry = noise(secs(0.1), 4, 0.5);
    let out = mono(&mut p, &dry);
    assert_eq!(out, dry, "fully dry passes the input untouched");
}

#[test]
fn resonators_stay_bounded_with_long_decay_and_resonant_input() {
    let mut p: res::Resonators = ready();
    set(
        &mut p,
        &[
            (res::NOTE, 60.0),
            (res::DECAY, 30_000.0),
            (res::CONST, 1.0),
            (res::COLOR, 100.0),
            (res::OUTPUT_GAIN, 24.0),
        ],
    );
    let drive = sine(261.63, SR, secs(30.0));
    let out = mono(&mut p, &drive);
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak(&out) < 500.0, "{}", peak(&out));
}

// ------------------------------------------------------------------------------ Corpus

fn corpus_for(extra: &[(u32, f64)]) -> corpus::Corpus {
    let mut p: corpus::Corpus = ready();
    set(&mut p, extra);
    p
}

fn ratios_at(signal: &[f32], f0: f64, ratios: &[f64], tol_percent: f64) {
    for &r in ratios {
        let want = f0 * r;
        let found = peak_near(signal, want, 60.0, 1.0);
        let off = (found / want - 1.0).abs() * 100.0;
        assert!(off < tol_percent, "ratio {r}: found {found}, wanted {want}");
    }
}

fn corpus_impulse(p: &mut corpus::Corpus, secs_len: f64) -> Vec<f32> {
    mono(p, &impulse(secs(secs_len)))
}

const BODY_F0: f64 = 200.0;

fn body(body: usize, extra: &[(u32, f64)]) -> corpus::Corpus {
    let mut p = corpus_for(&[
        (corpus::BODY, body as f64),
        (corpus::TUNE, BODY_F0),
        (corpus::QUALITY, 3.0),
        (corpus::DECAY, 8_000.0),
        (corpus::HIT, 42.0),
        (corpus::POS_L, 55.0),
        (corpus::POS_R, 55.0),
        (corpus::WIDTH, 0.0),
    ]);
    set(&mut p, extra);
    p
}

#[test]
fn corpus_resonance_frequencies_match_each_body() {
    // Beam: free-free Euler-Bernoulli modes.
    let mut p = body(corpus::BEAM, &[]);
    ratios_at(
        &corpus_impulse(&mut p, 1.0),
        BODY_F0,
        &[1.0, 2.7565, 5.4039, 8.9329],
        1.0,
    );
    // Marimba: 1:4:10.
    let mut p = body(corpus::MARIMBA, &[]);
    ratios_at(
        &corpus_impulse(&mut p, 1.0),
        BODY_F0,
        &[1.0, 4.0, 10.0],
        1.0,
    );
    // String: integer harmonics with a little stiffness.
    let mut p = body(corpus::STRING, &[]);
    ratios_at(
        &corpus_impulse(&mut p, 1.0),
        BODY_F0,
        &[1.0, 2.0, 3.0, 4.0, 5.0],
        1.0,
    );
    // Membrane, square: sqrt((m^2 + n^2) / 2).
    let mut p = body(corpus::MEMBRANE, &[(corpus::RATIO, 1.0)]);
    let root = |m: f64, n: f64| ((m * m + n * n) / 2.0f64).sqrt();
    ratios_at(
        &corpus_impulse(&mut p, 1.0),
        BODY_F0,
        &[
            root(1.0, 1.0),
            root(1.0, 2.0),
            root(2.0, 2.0),
            root(1.0, 3.0),
        ],
        1.0,
    );
    // Plate, square: (m^2 + n^2) / 2.
    let mut p = body(corpus::PLATE, &[(corpus::RATIO, 1.0)]);
    let plate = |m: f64, n: f64| (m * m + n * n) / 2.0f64;
    ratios_at(
        &corpus_impulse(&mut p, 1.0),
        BODY_F0,
        &[plate(1.0, 1.0), plate(1.0, 2.0), plate(2.0, 2.0)],
        1.0,
    );
    // Tube: open at both ends, every harmonic.
    let mut p = body(corpus::TUBE, &[(corpus::RADIUS, 55.0)]);
    ratios_at(&corpus_impulse(&mut p, 1.0), BODY_F0, &[1.0, 2.0, 3.0], 1.0);
    // Pipe closed at one end: odd harmonics of the same fundamental; fully open: all of them.
    let measure = |opening: f64| -> Vec<f32> {
        let mut p = body(
            corpus::PIPE,
            &[(corpus::RADIUS, 55.0), (corpus::OPENING, opening)],
        );
        corpus_impulse(&mut p, 1.0)
    };
    let (closed, open) = (measure(0.0), measure(100.0));
    ratios_at(&closed, BODY_F0, &[1.0, 3.0], 1.5);
    assert!(
        tone(&closed, BODY_F0 * 2.0, 2_000, secs(0.4))
            < tone(&closed, BODY_F0, 2_000, secs(0.4)) * 0.1,
        "closed pipe has no second harmonic"
    );
    ratios_at(&open, BODY_F0, &[1.0, 2.0, 3.0], 1.5);
}

#[test]
fn corpus_ratio_moves_the_modes_of_membrane_and_plate() {
    for r in [0.5f64, 1.0, 2.0] {
        let mut p = body(corpus::MEMBRANE, &[(corpus::RATIO, r)]);
        let out = corpus_impulse(&mut p, 1.0);
        // (m, n) = (2, 1) relative to (1, 1).
        let want = ((4.0 + r * r) / (1.0 + r * r)).sqrt();
        ratios_at(&out, BODY_F0, &[want], 1.5);
        let mut p = body(corpus::PLATE, &[(corpus::RATIO, r)]);
        let out = corpus_impulse(&mut p, 1.0);
        let want = (4.0 + r * r) / (1.0 + r * r);
        ratios_at(&out, BODY_F0, &[want], 1.5);
    }
}

#[test]
fn corpus_decay_sets_the_fundamental_ring_time() {
    for decay in [500.0f64, 2_000.0] {
        let mut p = body(corpus::STRING, &[(corpus::DECAY, decay)]);
        let out = corpus_impulse(&mut p, decay * 0.001 * 1.5);
        let window = secs(0.05);
        let (a1, a2) = (
            tone(&out, BODY_F0, secs(0.05), window),
            tone(&out, BODY_F0, secs(0.05 + decay * 0.001 * 0.5), window),
        );
        let t60 = 60.0 * (decay * 0.001 * 0.5) / (20.0 * (a1 / a2).log10());
        near(t60 / (decay * 0.001), 1.0, 0.08, "ring time");
    }
}

#[test]
fn corpus_material_sets_how_partials_decay_against_each_other() {
    let ring_of = |partial: f64, material: f64, gap: f64| -> f64 {
        let mut p = body(
            corpus::STRING,
            &[(corpus::DECAY, 1_500.0), (corpus::MATERIAL, material)],
        );
        let out = corpus_impulse(&mut p, gap + 0.4);
        let f = BODY_F0 * partial * (1.0 + 0.000_5 * partial * partial).sqrt();
        // Long windows, so a loud neighbouring partial doesn't leak into a quiet one.
        let (a1, a2) = (
            tone(&out, f, secs(0.05), secs(0.2)),
            tone(&out, f, secs(0.05 + gap), secs(0.2)),
        );
        60.0 * gap / (20.0 * (a1 / a2).log10())
    };
    let flat_ratio = ring_of(3.0, 0.0, 0.5) / ring_of(1.0, 0.0, 0.5);
    near(flat_ratio, 1.0, 0.2, "Material 0: same ring time");

    // Windows for a drop of at least 20 dB on the faster partial, so the readings are good.
    let wood = ring_of(3.0, -100.0, 0.25) / ring_of(1.0, -100.0, 0.5);
    let metal = ring_of(3.0, 100.0, 1.5) / ring_of(1.0, 100.0, 1.0);
    near(
        wood,
        1.0 / 3.0,
        0.1,
        "Material -100: highs ring shorter by the ratio",
    );
    near(
        metal,
        3.0,
        0.6,
        "Material +100: highs ring longer by the ratio",
    );
}

#[test]
fn corpus_inharm_stretches_and_compresses_the_partials_as_the_law_says() {
    for inharm in [-100.0f64, 0.0, 100.0] {
        let mut p = body(corpus::STRING, &[(corpus::INHARM, inharm)]);
        let out = corpus_impulse(&mut p, 1.0);
        let ratio = 3.0f64 * (1.0 + 0.0005 * 9.0f64).sqrt();
        let warped = ratio.powf(1.0 + inharm * 0.01 * corpus::INHARM_SLOPE as f64);
        ratios_at(&out, BODY_F0, &[warped], 1.5);
    }
}

#[test]
fn corpus_bright_tilts_the_partial_levels() {
    let level = |bright: f64, partial: f64| -> f64 {
        let mut p = body(
            corpus::STRING,
            &[(corpus::BRIGHT, bright), (corpus::DECAY, 4_000.0)],
        );
        let out = corpus_impulse(&mut p, 1.0);
        let f = BODY_F0 * partial * (1.0 + 0.000_5 * partial * partial).sqrt();
        tone(&out, f, 256, secs(0.3))
    };
    let ratio = |bright: f64| level(bright, 5.0) / level(bright, 1.0);
    let (dark, flat, bright) = (ratio(-100.0), ratio(0.0), ratio(100.0));
    near(
        bright / flat,
        5f64.powf(corpus::BRIGHT_SLOPE as f64),
        0.4,
        "Bright +100 boosts by ratio^slope",
    );
    assert!(dark < flat && flat < bright);
}

#[test]
fn corpus_hit_at_the_centre_leaves_out_even_modes() {
    let level = |partial: f64, hit: f64| -> f64 {
        let mut p = body(
            corpus::STRING,
            &[
                (corpus::HIT, hit),
                (corpus::POS_L, 25.0),
                (corpus::POS_R, 25.0),
                (corpus::DECAY, 4_000.0),
            ],
        );
        let out = corpus_impulse(&mut p, 1.0);
        let f = BODY_F0 * partial * (1.0 + 0.000_5 * partial * partial).sqrt();
        tone(&out, f, 256, secs(0.4))
    };
    // The Goertzel readings leak by about 1% of the fundamental, so this is close to the floor.
    assert!(
        level(2.0, 0.0) < level(1.0, 0.0) * 0.02,
        "no 2nd partial from the centre"
    );
    assert!(
        level(2.0, 70.0) > level(1.0, 70.0) * 0.05,
        "it appears off centre"
    );
}

#[test]
fn corpus_pos_listens_at_a_point_and_quality_sets_the_partial_count() {
    // A node at the centre for the second partial: listening at 0 can't hear it.
    let at = |partial: f64, pos: f64| -> f64 {
        let mut p = body(
            corpus::STRING,
            &[
                (corpus::POS_L, pos),
                (corpus::POS_R, pos),
                (corpus::DECAY, 4_000.0),
            ],
        );
        let out = corpus_impulse(&mut p, 1.0);
        let f = BODY_F0 * partial * (1.0 + 0.000_5 * partial * partial).sqrt();
        tone(&out, f, 256, secs(0.4))
    };
    assert!(
        at(2.0, 0.0) < at(1.0, 0.0) * 0.02,
        "{} vs {}",
        at(2.0, 0.0),
        at(1.0, 0.0)
    );

    // Eco stops at QUALITY_MODES[0] partials: a beam's 9th partial is silent there.
    let beam_ninth = |quality: f64| -> f64 {
        let mut p = body(
            corpus::BEAM,
            &[(corpus::QUALITY, quality), (corpus::DECAY, 2_000.0)],
        );
        let out = corpus_impulse(&mut p, 0.5);
        let n = 9.0f64;
        let f = BODY_F0 * (2.0 * n + 1.0) * (2.0 * n + 1.0) / 9.0;
        tone(&out, f, 64, secs(0.2))
    };
    assert_eq!(corpus::QUALITY_MODES[0], 8);
    assert!(
        beam_ninth(3.0) > beam_ninth(0.0) * 20.0,
        "{} vs {}",
        beam_ninth(3.0),
        beam_ninth(0.0)
    );
}

#[test]
fn corpus_width_and_spread_treat_the_two_channels_as_the_manual_says() {
    // Zero width: mono. Full: each resonator on its own side, so a sound on the left stays left.
    let (l, r) = {
        let mut p = corpus_for(&[(corpus::WIDTH, 0.0), (corpus::DECAY, 2_000.0)]);
        render(&mut p, &impulse(secs(0.2)), &silence(secs(0.2)))
    };
    assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-7));
    assert!(rms(&l) > 1e-4);
    let (l, r) = {
        let mut p = corpus_for(&[(corpus::WIDTH, 100.0), (corpus::DECAY, 2_000.0)]);
        render(&mut p, &impulse(secs(0.2)), &silence(secs(0.2)))
    };
    assert!(rms(&l) > 1e-4 && rms(&r) < 1e-7, "{} {}", rms(&l), rms(&r));

    // Spread +100: left up a semitone, right down.
    let mut p = body(
        corpus::STRING,
        &[(corpus::SPREAD, 100.0), (corpus::WIDTH, 100.0)],
    );
    let (l, r) = render(&mut p, &impulse(secs(1.0)), &impulse(secs(1.0)));
    let up = BODY_F0 * 2f64.powf(corpus::SPREAD_SEMITONES as f64 / 12.0);
    let down = BODY_F0 * 2f64.powf(-corpus::SPREAD_SEMITONES as f64 / 12.0);
    assert!(
        cents(peak_near(&l[1_000..], up, 80.0, 1.0), up).abs() < 3.0,
        "left up"
    );
    assert!(
        cents(peak_near(&r[1_000..], down, 80.0, 1.0), down).abs() < 3.0,
        "right down"
    );
}

#[test]
fn corpus_lfo_modulates_frequency_and_phase_offsets_the_channels() {
    let track = |phase: f64, hz: f64| -> (Vec<f32>, Vec<f32>) {
        let mut p = body(
            corpus::STRING,
            &[
                (corpus::LFO_ON, 1.0),
                (corpus::LFO_AMOUNT, 25.0),
                (corpus::LFO_RATE, hz),
                (corpus::LFO_PHASE, phase),
                (corpus::WIDTH, 100.0),
                (corpus::DECAY, 12_000.0),
            ],
        );
        render(&mut p, &impulse(secs(2.0)), &impulse(secs(2.0)))
    };
    // The LFO's peak is +3 semitones on Amount 25%: a quarter cycle in, left has climbed.
    let (l, r) = track(180.0, 1.0);
    let window = secs(0.1);
    let up = BODY_F0 * 2f64.powf(3.0 / 12.0);
    assert!(
        tone(&l, up, secs(0.2), window) > tone(&l, BODY_F0, secs(0.2), window),
        "left has moved up by a quarter cycle in"
    );
    // At 180 degrees the right is at the other end at that moment.
    let down = BODY_F0 * 2f64.powf(-3.0 / 12.0);
    assert!(
        tone(&r, down, secs(0.2), window) > tone(&r, up, secs(0.2), window),
        "right has moved down at that point"
    );
}

#[test]
fn corpus_filter_bleed_gain_and_limiter_shape_the_output() {
    // The band-pass at 1 kHz takes down a 200 Hz resonance.
    let at_200 = |filter: f64| -> f64 {
        let mut p = body(
            corpus::STRING,
            &[
                (corpus::FILTER_ON, filter),
                (corpus::FILTER_FREQ, 2_000.0),
                (corpus::FILTER_BW, 1.0),
                (corpus::DECAY, 4_000.0),
            ],
        );
        tone(&corpus_impulse(&mut p, 1.0), 200.0, 1_024, secs(0.4))
    };
    assert!(at_200(0.0) > at_200(1.0) * 4.0);

    // Bleed lets the input straight through, except on Pipe and Tube.
    let dry_at_start = |body_type: usize, bleed: f64| -> f32 {
        let mut p = body(body_type, &[(corpus::BLEED, bleed), (corpus::DECAY, 800.0)]);
        let input: Vec<f32> = impulse(secs(0.1)).iter().map(|s| s * 0.1).collect();
        mono(&mut p, &input)[0]
    };
    let without = dry_at_start(corpus::STRING, 0.0);
    let with = dry_at_start(corpus::STRING, 50.0);
    near(
        (with - without) as f64,
        0.05,
        0.002,
        "Bleed 50% adds half the input",
    );
    assert_eq!(
        dry_at_start(corpus::TUBE, 0.0),
        dry_at_start(corpus::TUBE, 50.0),
        "Bleed off on Tube"
    );

    // The limiter holds the ceiling when the level is too high.
    let mut p = corpus_for(&[
        (corpus::GAIN, 24.0),
        (corpus::DECAY, 20_000.0),
        (corpus::TUNE, 220.0),
    ]);
    let loud = sine(220.0, SR, secs(4.0));
    let out = mono(&mut p, &loud);
    assert!(
        out.iter().all(|s| s.abs() <= corpus::LIMIT_CEILING + 1e-5),
        "{}",
        peak(&out)
    );
    assert!(peak(&out) > 0.5);
}

#[test]
fn corpus_dry_wet_stops_new_input_without_cutting_the_tail() {
    let mut p = corpus_for(&[(corpus::DECAY, 4_000.0)]);
    let mut h = Harness::new(SR);
    render_with(&mut p, &impulse(secs(0.1)), &impulse(secs(0.1)), &mut h);
    set(&mut p, &[(corpus::MIX, 0.0)]);
    let quiet = silence(secs(0.1));
    let (tail, _) = render_with(&mut p, &quiet, &quiet, &mut h);
    assert!(rms(&tail) > 1e-3, "resonance carries on");
    let input = noise(secs(0.1), 3, 0.5);
    let mut q = corpus_for(&[(corpus::MIX, 0.0)]);
    let out = mono(&mut q, &input);
    let diff: f32 = out
        .iter()
        .zip(&input)
        .fold(0.0, |m, (a, b)| m.max((a - b).abs()));
    assert!(
        diff < 1e-9,
        "fully dry with no resonance rings nothing: {diff}"
    );
}

fn note_on(key: u8, offset: u32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        },
    )
}

fn note_off(key: u8, offset: u32) -> MidiEvent {
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

/// Plays `events` then an impulse, returning the left output.
fn midi_and_ping(p: &mut corpus::Corpus, events: &[MidiEvent], frames: usize) -> Vec<f32> {
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (silence(frames), silence(frames));
    l[0] = 1.0;
    r[0] = 1.0;
    for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
        let e = if i == 0 { events } else { &[] };
        assert_no_alloc(|| h.process(p, cl, cr, e, &[]));
    }
    l
}

#[test]
fn corpus_midi_frequency_tunes_the_resonance_and_transpose_and_fine_offset_it() {
    let mut p = body(corpus::STRING, &[(corpus::MIDI_FREQ, 1.0)]);
    let out = midi_and_ping(&mut p, &[note_on(69, 0)], secs(1.0));
    assert!(
        cents(peak_near(&out[1_000..], 440.0, 80.0, 1.0), 440.0).abs() < 3.0,
        "A4"
    );

    let mut p = body(
        corpus::STRING,
        &[
            (corpus::MIDI_FREQ, 1.0),
            (corpus::TRANSPOSE, -12.0),
            (corpus::FINE, 50.0),
        ],
    );
    let out = midi_and_ping(&mut p, &[note_on(69, 0)], secs(1.0));
    let want = 220.0 * 2f64.powf(50.0 / 1200.0);
    assert!(
        cents(peak_near(&out[1_000..], want, 80.0, 1.0), want).abs() < 3.0,
        "transposed"
    );

    // Without Frequency, MIDI doesn't change Tune.
    let mut p = body(corpus::STRING, &[]);
    let out = midi_and_ping(&mut p, &[note_on(69, 0)], secs(1.0));
    assert!(cents(peak_near(&out[1_000..], BODY_F0, 80.0, 1.0), BODY_F0).abs() < 3.0);
}

#[test]
fn corpus_midi_note_priority_and_pitch_bend() {
    let ping_level = |p: &mut corpus::Corpus, hz: f64| -> f64 {
        let out = midi_and_ping(p, &[], secs(1.0));
        tone(&out, hz, 1_000, secs(0.5))
    };
    // Last priority: the last note held wins.
    let mut p = body(
        corpus::STRING,
        &[(corpus::MIDI_FREQ, 1.0), (corpus::PRIORITY, 0.0)],
    );
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (silence(BLOCK), silence(BLOCK));
    h.process(
        &mut p,
        &mut l,
        &mut r,
        &[note_on(60, 0), note_on(69, 8)],
        &[],
    );
    let (last, first) = (
        ping_level(&mut p, midi_hz(69.0)),
        ping_level(&mut p, midi_hz(60.0)),
    );
    assert!(last > first * 10.0, "last note wins: {last} vs {first}");
    // Low priority: the lowest note wins.
    let mut p = body(
        corpus::STRING,
        &[(corpus::MIDI_FREQ, 1.0), (corpus::PRIORITY, 1.0)],
    );
    let (mut l, mut r) = (silence(BLOCK), silence(BLOCK));
    h.process(
        &mut p,
        &mut l,
        &mut r,
        &[note_on(60, 0), note_on(69, 8)],
        &[],
    );
    let (high, low) = (
        ping_level(&mut p, midi_hz(69.0)),
        ping_level(&mut p, midi_hz(60.0)),
    );
    assert!(low > high * 10.0, "low note wins: {low} vs {high}");

    // Pitch bend with PB Range 12: full up is an octave.
    let mut p = body(
        corpus::STRING,
        &[(corpus::MIDI_FREQ, 1.0), (corpus::PB_RANGE, 12.0)],
    );
    let (mut l, mut r) = (silence(BLOCK), silence(BLOCK));
    let bend = MidiEvent::new(
        0,
        MidiMessage::PitchBend {
            channel: 0,
            value: 1.0,
        },
    );
    h.process(&mut p, &mut l, &mut r, &[note_on(69, 0), bend], &[]);
    let (at_880, at_440) = (ping_level(&mut p, 880.0), ping_level(&mut p, 440.0));
    assert!(
        at_880 > at_440 * 3.0,
        "bent an octave up: {at_880} vs {at_440}"
    );
}

#[test]
fn corpus_off_decay_mutes_on_note_off_by_its_amount() {
    let tail_after_release = |amount: f64, enable: bool| -> f32 {
        let mut p = corpus_for(&[
            (corpus::DECAY, 4_000.0),
            (corpus::OFF_DECAY, enable as u32 as f64),
            (corpus::OFF_AMOUNT, amount),
        ]);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (impulse(secs(0.2)), impulse(secs(0.2)));
        let events = [note_on(60, 0)];
        for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
            let e = if i == 0 { &events[..] } else { &[] };
            assert_no_alloc(|| h.process(&mut p, cl, cr, e, &[]));
        }
        let release = [note_off(60, 0)];
        let (mut l, mut r) = (silence(secs(0.3)), silence(secs(0.3)));
        for (i, (cl, cr)) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).enumerate() {
            let e = if i == 0 { &release[..] } else { &[] };
            assert_no_alloc(|| h.process(&mut p, cl, cr, e, &[]));
        }
        rms(&l[secs(0.1)..])
    };
    let held = tail_after_release(0.0, false);
    let at_zero = tail_after_release(0.0, true);
    let at_full = tail_after_release(100.0, true);
    assert!(held > 1e-3);
    near(
        (at_zero / held) as f64,
        1.0,
        0.02,
        "0%: Note Offs are ignored",
    );
    assert!(
        at_full < held * 1e-3,
        "100%: muted at once: {at_full} vs {held}"
    );
}

#[test]
fn corpus_stays_bounded_and_finite_at_extreme_settings() {
    for body_type in [corpus::BEAM, corpus::MEMBRANE, corpus::PIPE, corpus::TUBE] {
        let mut p = corpus_for(&[
            (corpus::BODY, body_type as f64),
            (corpus::DECAY, 20_000.0),
            (corpus::QUALITY, 3.0),
            (corpus::RADIUS, 100.0),
            (corpus::TUNE, 8_000.0),
            (corpus::BRIGHT, 100.0),
            (corpus::INHARM, 100.0),
            (corpus::LFO_ON, 1.0),
            (corpus::LFO_AMOUNT, 100.0),
            (corpus::LFO_RATE, 30.0),
            (corpus::GAIN, 24.0),
            (corpus::SPREAD, 100.0),
        ]);
        let loud = sine(1_000.0, SR, secs(8.0));
        let out = mono(&mut p, &loud);
        assert!(out.iter().all(|s| s.is_finite()), "body {body_type}");
        assert!(peak(&out) <= corpus::LIMIT_CEILING + 1e-5, "{}", peak(&out));
    }
}

#[test]
fn every_b7_device_survives_notes_and_parameter_automation_without_touching_the_heap() {
    for id in [
        "rlive.reverb",
        "rlive.hybrid_reverb",
        "rlive.resonators",
        "rlive.corpus",
    ] {
        let info = find(id).unwrap();
        let mut p = (info.create)(&rlive_devices::DeviceInit {
            samples: vec![
                Some(user_ir(vec![0.5; 2000], vec![0.5; 2000]));
                (info.desc)().samples.len()
            ],
            state: None,
        });
        p.activate(SR, BLOCK);
        let desc = (info.desc)();
        let mut h = Harness::new(SR);
        for k in 0..600usize {
            let (mut l, mut r) = (noise(BLOCK, k as u32, 0.3), noise(BLOCK, k as u32 + 1, 0.3));
            let param = &desc.params[k % desc.params.len()];
            let events = [ParamEvent {
                offset: 16,
                id: param.param_id(),
                value: param.from_normalized(((k * 37) % 100) as f64 * 0.01),
            }];
            let notes = [note_on(60 + (k % 12) as u8, 4)];
            assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &notes, &events));
            assert!(l.iter().chain(&r).all(|s| s.is_finite()), "{id} block {k}");
        }
    }
}

#[test]
fn hybrid_tuning_helpers_expose_the_known_categories_in_order() {
    assert_eq!(hybrid_ir::CATEGORIES[0], "Early Reflections");
    assert_eq!(hybrid_ir::CATEGORIES[8], "Textures");
    assert_eq!(hr::ir_names(0).len(), hybrid_ir::PER_CATEGORY);
}

#[test]
fn corpus_radius_lengthens_and_brightens_a_tube_and_only_moves_the_pitch_at_the_top() {
    let measure = |radius: f64| -> (f64, f64, f64) {
        let mut p = body(
            corpus::TUBE,
            &[(corpus::RADIUS, radius), (corpus::DECAY, 1_500.0)],
        );
        let out = corpus_impulse(&mut p, 1.5);
        let early = tone(&out, BODY_F0, secs(0.02), secs(0.1));
        let late = tone(&out, BODY_F0, secs(0.5), secs(0.1));
        let high = tone(&out, BODY_F0 * 8.0, secs(0.05), secs(0.1));
        (
            late / early,
            high / early,
            peak_near(&out, BODY_F0, 500.0, 2.0),
        )
    };
    let (narrow, wide) = (measure(10.0), measure(70.0));
    assert!(
        wide.0 > narrow.0 * 2.0,
        "wider rings longer: {} vs {}",
        wide.0,
        narrow.0
    );
    assert!(
        wide.1 > narrow.1 * 2.0,
        "and keeps more highs: {} vs {}",
        wide.1,
        narrow.1
    );
    assert!(
        cents(wide.2, BODY_F0).abs() < 8.0,
        "no pitch change below the top: {}",
        wide.2
    );
    let top = measure(100.0);
    assert!(
        cents(top.2, BODY_F0 / (1.0 + corpus::RADIUS_PITCH_PULL as f64)).abs() < 8.0,
        "the very top pulls the pitch: {}",
        top.2
    );
}
