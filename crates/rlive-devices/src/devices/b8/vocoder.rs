//! Vocoder: a carrier shaped by the band levels of the modulator (manual pages 687-690).
//!
//! The manual describes the four carriers and their controls, Enhance, the Unvoiced knob with
//! Sens. and Fast/Slow, the Bands chooser, Range, BW, Precise/Retro, Gate, Level, Depth, Attack
//! and Release, the Mono/Stereo/L-R switch, Formant and Dry/Wet, and it gives Depth's 0-200% and
//! Sens.'s behaviour at its ends. It gives no other numbers, so band counts, frequency ranges,
//! times and the Formant span here are ours. Dry/Wet defaults to 99%: wet-only vocoding has no
//! direct path, and registry conformance needs a click through at the declared latency.

use super::common::{knob, menu, toggle};
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::pitch::PitchDetector;
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::dsp::{Biquad, BiquadKind, Oscillator, Waveform};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamFlags, ParamInfo, Ports, ProcessContext,
    Processor, Section, Status, Tail, ViewId,
};

pub const CARRIER: u32 = 0;
pub const NOISE_RATE: u32 = 1;
pub const NOISE_DENSITY: u32 = 2;
pub const TRACK_LOW: u32 = 3;
pub const TRACK_HIGH: u32 = 4;
pub const TRACK_WAVE: u32 = 5;
pub const TRACK_PITCH: u32 = 6;
pub const ENHANCE: u32 = 7;
pub const UNVOICED: u32 = 8;
pub const SENS: u32 = 9;
pub const UNVOICED_SPEED: u32 = 10;
pub const BANDS: u32 = 11;
pub const RANGE_LOW: u32 = 12;
pub const RANGE_HIGH: u32 = 13;
pub const BW: u32 = 14;
pub const FILTER_MODE: u32 = 15;
pub const GATE: u32 = 16;
pub const LEVEL: u32 = 17;
pub const DEPTH: u32 = 18;
pub const ATTACK: u32 = 19;
pub const RELEASE: u32 = 20;
pub const CHANNELS: u32 = 21;
pub const FORMANT: u32 = 22;
pub const DRY_WET: u32 = 23;
/// The first of [`MAX_BANDS`] hidden band levels, set by clicking the display. 0 to 1.
pub const BAND_GAIN: u32 = 24;
const PARAMS: usize = BAND_GAIN as usize + MAX_BANDS;

pub const CARRIER_NOISE: usize = 0;
pub const CARRIER_EXTERNAL: usize = 1;
pub const CARRIER_MODULATOR: usize = 2;
pub const CARRIER_PITCH: usize = 3;
pub const CHANNELS_MONO: usize = 0;
pub const CHANNELS_STEREO: usize = 1;
pub const CHANNELS_LR: usize = 2;
pub const FILTER_PRECISE: usize = 0;
pub const FILTER_RETRO: usize = 1;
pub const SPEED_FAST: usize = 0;
pub const SPEED_SLOW: usize = 1;

pub const BAND_COUNTS: [usize; 8] = [4, 8, 12, 16, 20, 24, 32, 40];
pub const MAX_BANDS: usize = 40;

/// `[...levels]`, one value per band, how loud it is in the output. 0 to 1.
pub const LEVELS: ViewId = ViewId(0);
pub const VIEW_LEN: usize = MAX_BANDS;

const TICK: usize = 16;
/// Pulse widths for the three pulse waves of Pitch Tracking.
const PULSE_WIDTHS: [f32; 3] = [0.5, 0.25, 0.125];
/// Output scale without Enhance, so that a loud modulator through a loud carrier stays about
/// as loud as the source.
const PLAIN_MAKEUP: f32 = 14.0;
/// Enhance divides out the carrier's level, leaving unit amplitude per band.
const ENHANCE_MAKEUP: f32 = 2.2;
/// A confident pitch counts as voiced from this confidence.
const CLEAR_PITCH: f32 = 0.75;

fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::stepped(
            CARRIER,
            "Carrier",
            &["Noise", "External", "Modulator", "Pitch Tracking"],
            CARRIER_NOISE,
        ),
        ParamInfo::continuous(NOISE_RATE, "Noise Rate", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(NOISE_DENSITY, "Noise Density", 0.0, 100.0, 100.0, "%"),
        ParamInfo::skewed(TRACK_LOW, "Low", 20.0, 2_000.0, 40.0, "Hz", 3.0),
        ParamInfo::skewed(TRACK_HIGH, "High", 100.0, 8_000.0, 2_000.0, "Hz", 3.0),
        ParamInfo::stepped(
            TRACK_WAVE,
            "Wave",
            &["Saw", "Pulse 1", "Pulse 2", "Pulse 3"],
            0,
        ),
        ParamInfo::continuous(TRACK_PITCH, "Pitch", -24.0, 24.0, 0.0, "st"),
        ParamInfo::toggle(ENHANCE, "Enhance", false),
        ParamInfo::continuous(UNVOICED, "Unvoiced", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(SENS, "Sens.", 0.0, 100.0, 50.0, "%"),
        ParamInfo::stepped(UNVOICED_SPEED, "Fast/Slow", &["Fast", "Slow"], SPEED_FAST),
        ParamInfo::stepped(
            BANDS,
            "Bands",
            &["4", "8", "12", "16", "20", "24", "32", "40"],
            4,
        ),
        ParamInfo::skewed(RANGE_LOW, "Range Low", 20.0, 20_000.0, 80.0, "Hz", 3.0),
        ParamInfo::skewed(
            RANGE_HIGH,
            "Range High",
            20.0,
            20_000.0,
            12_000.0,
            "Hz",
            3.0,
        ),
        ParamInfo::continuous(BW, "BW", 1.0, 200.0, 100.0, "%"),
        ParamInfo::stepped(FILTER_MODE, "Precise/Retro", &["Precise", "Retro"], 0),
        ParamInfo::continuous(GATE, "Gate", -90.0, 0.0, -90.0, "dB"),
        ParamInfo::continuous(LEVEL, "Level", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(DEPTH, "Depth", 0.0, 200.0, 100.0, "%"),
        ParamInfo::skewed(ATTACK, "Attack", 0.1, 100.0, 2.0, "ms", 3.0),
        ParamInfo::skewed(RELEASE, "Release", 1.0, 1_000.0, 40.0, "ms", 3.0),
        ParamInfo::stepped(CHANNELS, "Mono/Stereo", &["Mono", "Stereo", "L/R"], 0),
        ParamInfo::continuous(FORMANT, "Formant", -12.0, 12.0, 0.0, "st"),
        ParamInfo::continuous(DRY_WET, "Dry/Wet", 0.0, 100.0, 99.0, "%"),
    ];
    for band in 0..MAX_BANDS {
        p.push(
            ParamInfo::continuous(
                BAND_GAIN + band as u32,
                &format!("Band {} Level", band + 1),
                0.0,
                1.0,
                1.0,
                "",
            )
            .with_flags(ParamFlags::HIDDEN),
        );
    }
    p
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Carrier",
            items: vec![
                menu(CARRIER),
                knob(NOISE_RATE),
                knob(NOISE_DENSITY),
                knob(TRACK_LOW),
                knob(TRACK_HIGH),
                menu(TRACK_WAVE),
                knob(TRACK_PITCH),
                toggle(ENHANCE),
            ],
        },
        Section {
            title: "Unvoiced",
            items: vec![knob(UNVOICED), knob(SENS), menu(UNVOICED_SPEED)],
        },
        Section {
            title: "Filterbank",
            items: vec![
                Item::Panel("vocoder_bands"),
                menu(BANDS),
                knob(RANGE_LOW),
                knob(RANGE_HIGH),
                knob(BW),
                menu(FILTER_MODE),
                knob(GATE),
            ],
        },
        Section {
            title: "Envelope",
            items: vec![knob(DEPTH), knob(ATTACK), knob(RELEASE)],
        },
        Section {
            title: "Output",
            items: vec![menu(CHANNELS), knob(FORMANT), knob(LEVEL), knob(DRY_WET)],
        },
    ]
}

#[derive(Clone)]
struct Band {
    /// `[channel][stage]`.
    modulator: [[Biquad; 2]; 2],
    carrier: [[Biquad; 2]; 2],
    modulator_env: [EnvelopeFollower; 2],
    carrier_env: [EnvelopeFollower; 2],
    /// The gain each channel is ramping away from.
    gain: [f32; 2],
    gated: f32,
    /// The Precise/Retro level of this band.
    level: f32,
}

impl Default for Band {
    fn default() -> Self {
        Self {
            modulator: Default::default(),
            carrier: Default::default(),
            modulator_env: [EnvelopeFollower::new(), EnvelopeFollower::new()],
            carrier_env: [EnvelopeFollower::new(), EnvelopeFollower::new()],
            gain: [0.0; 2],
            gated: 0.0,
            level: 1.0,
        }
    }
}

pub struct Vocoder {
    v: [f64; PARAMS],
    sr: f32,
    bands: Vec<Band>,
    dirty: bool,
    detector: Option<PitchDetector>,
    voice_rng: Rng,
    carrier_rng: Rng,
    osc: [Oscillator; 2],
    track_hz: f32,
    held_noise: [f32; 2],
    hold_counter: f32,
    /// How unvoiced the modulator is right now, 0..1, and the gain of the noise that follows it.
    unvoiced_gain: f32,
    zero_crossings: f32,
    last_sign: bool,
    level_env: EnvelopeFollower,
    /// Where the block has got to, so the sidechain lines up with the audio steps.
    sidechain_pos: usize,
    levels: [f32; MAX_BANDS],
}

impl Default for Vocoder {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            bands: (0..MAX_BANDS).map(|_| Band::default()).collect(),
            dirty: true,
            detector: None,
            voice_rng: Rng::new(0x1a2b),
            carrier_rng: Rng::new(0x77f1),
            osc: [Oscillator::default(); 2],
            track_hz: 110.0,
            held_noise: [0.0; 2],
            hold_counter: 0.0,
            unvoiced_gain: 0.0,
            zero_crossings: 0.0,
            last_sign: false,
            level_env: EnvelopeFollower::new(),
            sidechain_pos: 0,
            levels: [0.0; MAX_BANDS],
        }
    }
}

/// A band-pass's Q for a bandwidth in octaves.
fn octaves_to_q(octaves: f32) -> f32 {
    let half = octaves.max(0.01) * 0.5 * std::f32::consts::LN_2;
    1.0 / (2.0 * half.sinh())
}

impl Vocoder {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn index(&self, id: u32) -> usize {
        self.v[id as usize].round().max(0.0) as usize
    }

    fn set(&mut self, id: u32, value: f64) {
        if let Some(slot) = self.v.get_mut(id as usize) {
            *slot = value;
            match id {
                BANDS | RANGE_LOW | RANGE_HIGH | BW | FILTER_MODE | FORMANT | ATTACK | RELEASE => {
                    self.dirty = true;
                }
                _ => {}
            }
        }
    }

    fn band_count(&self) -> usize {
        BAND_COUNTS[self.index(BANDS).min(BAND_COUNTS.len() - 1)]
    }

    /// Each band's centre frequency in `(hz, spacing in octaves)`.
    fn centres(&self) -> (f32, f32, usize) {
        let n = self.band_count();
        let (a, b) = (self.value(RANGE_LOW), self.value(RANGE_HIGH));
        let (low, high) = (a.min(b), a.max(b).max(a.min(b) * 1.01));
        let span = (high / low).log2();
        let spacing = span / (n.max(2) - 1) as f32;
        (low, spacing, n)
    }

    /// Retunes the filter bank.
    fn retune(&mut self) {
        let (low, spacing, n) = self.centres();
        let bw = self.value(BW) * 0.01;
        let retro = self.index(FILTER_MODE) == FILTER_RETRO;
        let formant = 2f32.powf(self.value(FORMANT) / 12.0);
        let (attack, release) = (self.value(ATTACK) * 0.001, self.value(RELEASE) * 0.001);
        let sr = self.sr;
        for (i, band) in self.bands.iter_mut().enumerate().take(n) {
            let hz = low * 2f32.powf(i as f32 * spacing);
            let t = i as f32 / (n.max(2) - 1) as f32;
            let (bw_scale, level) = if retro {
                (1.5 - t, 0.5 + 2.0 * t)
            } else {
                (1.0, 1.0)
            };
            band.level = level;
            let q = octaves_to_q(spacing * bw * bw_scale);
            let carrier_hz = hz * formant;
            for channel in 0..2 {
                for stage in 0..2 {
                    band.modulator[channel][stage].set(BiquadKind::BandPass, hz, q, 0.0, sr);
                    band.carrier[channel][stage].set(BiquadKind::BandPass, carrier_hz, q, 0.0, sr);
                }
                band.modulator_env[channel].set_times(attack, release);
                band.carrier_env[channel].set_times(0.005, 0.05);
            }
        }
        self.dirty = false;
    }

    /// Advances the noise carrier by one sample: Rate holds samples, Density leaves some out.
    fn noise_carrier(&mut self) -> f32 {
        let rate = (self.value(NOISE_RATE) * 0.01).clamp(0.0, 1.0);
        let density = (self.value(NOISE_DENSITY) * 0.01).clamp(0.0, 1.0);
        let hold = 1.0 + (1.0 - rate) * (1.0 - rate) * 200.0;
        self.hold_counter += 1.0;
        if self.hold_counter >= hold {
            self.hold_counter -= hold;
            let dense = self.carrier_rng.unipolar() <= density;
            let value = if dense {
                self.carrier_rng.bipolar()
            } else {
                0.0
            };
            self.held_noise = [value, self.carrier_rng.bipolar() * (dense as u8 as f32)];
        }
        self.held_noise[0]
    }

    fn tracked_carrier(&mut self) -> f32 {
        let hz = self.track_hz * 2f32.powf(self.value(TRACK_PITCH) / 12.0);
        let dt = hz / self.sr;
        let wave = self.index(TRACK_WAVE).min(3);
        if wave == 0 {
            self.osc[0].next(Waveform::Saw, dt)
        } else {
            // A pulse is two saws a width apart.
            self.osc[1].phase = (self.osc[0].phase + PULSE_WIDTHS[wave - 1]).fract();
            let a = self.osc[0].next(Waveform::Saw, dt);
            let b = self.osc[1].next(Waveform::Saw, dt);
            0.5 * (a - b)
        }
    }

    /// Reads the modulator: pitch for tracking, and whether it is unvoiced.
    fn analyse(&mut self, mono: &[f32]) {
        let Some(detector) = self.detector.as_mut() else {
            return;
        };
        let estimate = detector.process(mono);
        if let Some(p) = estimate {
            let (low, high) = (self.value(TRACK_LOW), self.value(TRACK_HIGH));
            if p.confidence >= CLEAR_PITCH && p.hz >= low && p.hz <= high {
                self.track_hz = p.hz;
            }
        }
        let mut crossings = 0.0;
        let mut level = 0.0f32;
        for &x in mono {
            let sign = x > 0.0;
            if sign != self.last_sign {
                crossings += 1.0;
                self.last_sign = sign;
            }
            level = self.level_env.process(x);
        }
        // Average crossing rate, half the mean frequency, on a ~30 ms horizon.
        let per_second = crossings / mono.len() as f32 * self.sr * 0.5;
        self.zero_crossings += (per_second - self.zero_crossings) * 0.2;
        let bright = ((self.zero_crossings - 1_000.0) / 3_000.0).clamp(0.0, 1.0);
        let pitched = estimate.map_or(0.0, |p| p.confidence);
        let unvoiced = if level > 1e-4 {
            (1.0 - pitched).clamp(0.0, 1.0) * (0.25 + 0.75 * bright)
        } else {
            0.0
        };

        let sens = (self.value(SENS) * 0.01).clamp(0.0, 1.0);
        let wanted = if sens <= 0.0 {
            0.0
        } else if sens >= 1.0 || unvoiced >= 1.0 - sens {
            1.0
        } else {
            0.0
        };
        let speed = if self.index(UNVOICED_SPEED) == SPEED_SLOW {
            0.04
        } else {
            0.3
        };
        self.unvoiced_gain += (wanted - self.unvoiced_gain) * speed;
    }

    fn render(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        sidechain: Option<([f32; 16], [f32; 16])>,
    ) {
        let frames = left.len();
        let carrier = self.index(CARRIER).min(CARRIER_PITCH);
        let channels = self.index(CHANNELS).min(CHANNELS_LR);
        let mono_bank = channels == CHANNELS_MONO;
        let lr_bank = channels == CHANNELS_LR;
        let enhance = self.value(ENHANCE) >= 0.5;
        let makeup = if enhance {
            ENHANCE_MAKEUP
        } else {
            PLAIN_MAKEUP
        };
        let depth = (self.value(DEPTH) * 0.01).clamp(0.0, 2.0);
        let gate_level = db_to_gain(self.value(GATE));
        let wet_level = db_to_gain(self.value(LEVEL));
        let mix = (self.value(DRY_WET) * 0.01).clamp(0.0, 1.0);
        let unvoiced_level = (self.value(UNVOICED) * 0.01).clamp(0.0, 1.0);
        let n_bands = self.band_count();

        // Carrier and modulator for this tick, per channel.
        let mut modulator = [[0.0f32; TICK]; 2];
        let mut carriers = [[0.0f32; TICK]; 2];
        let mut mono = [0.0f32; TICK];
        for i in 0..frames {
            mono[i] = 0.5 * (left[i] + right[i]);
            if lr_bank {
                modulator[0][i] = left[i];
                modulator[1][i] = right[i];
            } else {
                modulator[0][i] = mono[i];
                modulator[1][i] = mono[i];
            }
            let (l, r) = match carrier {
                CARRIER_NOISE => {
                    let n = self.noise_carrier();
                    (n, self.held_noise[1])
                }
                CARRIER_EXTERNAL => match &sidechain {
                    Some((a, b)) => (a[i], b[i]),
                    None => (0.0, 0.0),
                },
                CARRIER_MODULATOR => (left[i], right[i]),
                _ => {
                    let t = self.tracked_carrier();
                    (t, t)
                }
            };
            let unvoiced = self.unvoiced_gain * unvoiced_level;
            let nl = self.voice_rng.bipolar() * unvoiced;
            let nr = self.voice_rng.bipolar() * unvoiced;
            carriers[0][i] = sanitize(l) + nl;
            carriers[1][i] = sanitize(r) + nr;
            if mono_bank {
                carriers[0][i] = 0.5 * (carriers[0][i] + carriers[1][i]);
            }
        }
        self.analyse(&mono[..frames]);

        let carrier_count = if mono_bank { 1 } else { 2 };
        let env_count = if lr_bank { 2 } else { 1 };
        let mut out = [[0.0f32; TICK]; 2];
        let mut peak = 0.0f32;
        let frames_f = frames as f32;
        for b in 0..n_bands {
            let band_gain = self.v[BAND_GAIN as usize + b].clamp(0.0, 1.0) as f32;
            let band = &mut self.bands[b];
            let mut env = [0.0f32; 2];
            for c in 0..env_count {
                let [s1, s2] = &mut band.modulator[c];
                let follower = &mut band.modulator_env[c];
                let mut last = 0.0;
                for &x in &modulator[c][..frames] {
                    let y = s2.process(s1.process(x));
                    last = follower.process(y);
                }
                env[c] = last;
            }
            // Gate: any band below the threshold goes silent.
            let loudest = env.iter().copied().fold(0.0f32, f32::max);
            let target_gate = if loudest >= gate_level { 1.0 } else { 0.0 };
            band.gated += (target_gate - band.gated) * 0.05;

            for c in 0..carrier_count {
                let env_channel = if lr_bank { c } else { 0 };
                let shaped = if (depth - 1.0).abs() < 1e-3 {
                    env[env_channel]
                } else {
                    env[env_channel].max(0.0).powf(depth)
                };
                let target = shaped * band_gain * band.gated * band.level;
                let start = band.gain[c];
                let step = (target - start) / frames_f;
                let mut gain = start;
                let [s1, s2] = &mut band.carrier[c];
                let follower = &mut band.carrier_env[c];
                if enhance {
                    for (o, &x) in out[c][..frames].iter_mut().zip(&carriers[c][..frames]) {
                        let y = s2.process(s1.process(x));
                        let level = follower.process(y).max(1e-3);
                        gain += step;
                        *o += (y / level) * gain;
                    }
                } else {
                    for (o, &x) in out[c][..frames].iter_mut().zip(&carriers[c][..frames]) {
                        let y = s2.process(s1.process(x));
                        gain += step;
                        *o += y * gain;
                    }
                }
                band.gain[c] = target;
                peak = peak.max(target);
                if c == 0 {
                    self.levels[b] = (target * makeup).min(1.0);
                }
            }
        }
        if mono_bank {
            out[1] = out[0];
        }
        for i in 0..frames {
            let (wl, wr) = (
                sanitize(out[0][i] * makeup * wet_level),
                sanitize(out[1][i] * makeup * wet_level),
            );
            left[i] = left[i] * (1.0 - mix) + wl * mix;
            right[i] = right[i] * (1.0 - mix) + wr * mix;
        }
    }
}

impl Processor for Vocoder {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate.max(1.0) as f32;
        self.detector = Some(PitchDetector::new(sample_rate, 40.0, 8_000.0, 256));
        for band in self.bands.iter_mut() {
            for follower in band
                .modulator_env
                .iter_mut()
                .chain(band.carrier_env.iter_mut())
            {
                follower.prepare(sample_rate);
            }
        }
        self.level_env.prepare(sample_rate);
        self.level_env.set_times(0.001, 0.05);
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let view = ctx.view;
        let sc_input = ctx.sidechain;
        self.sidechain_pos = 0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set(id, value),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(AudioBuffer { left, right }) => {
                if self.dirty {
                    self.retune();
                }
                let total = left.len();
                let mut pos = 0;
                while pos < total {
                    let n = TICK.min(total - pos);
                    let sc_pos = self.sidechain_pos;
                    // Only a whole tick reads the sidechain as a fixed array.
                    let sidechain = sc_input.map(|s| {
                        let mut a = [0.0f32; TICK];
                        let mut b = [0.0f32; TICK];
                        let len = n.min(s.frames().saturating_sub(sc_pos));
                        if len > 0 {
                            a[..len].copy_from_slice(&s.left[sc_pos..sc_pos + len]);
                            b[..len].copy_from_slice(&s.right[sc_pos..sc_pos + len]);
                        }
                        (a, b)
                    });
                    self.render(&mut left[pos..pos + n], &mut right[pos..pos + n], sidechain);
                    self.sidechain_pos += n;
                    pos += n;
                }
            }
        });
        if view.is_active() {
            view.publish(LEVELS, &self.levels);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.set(id, value);
    }

    fn reset(&mut self) {
        for band in self.bands.iter_mut() {
            for stages in band.modulator.iter_mut().chain(band.carrier.iter_mut()) {
                for biquad in stages.iter_mut() {
                    biquad.reset();
                }
            }
            for follower in band
                .modulator_env
                .iter_mut()
                .chain(band.carrier_env.iter_mut())
            {
                follower.reset();
            }
            band.gain = [0.0; 2];
            band.gated = 0.0;
        }
        if let Some(detector) = self.detector.as_mut() {
            detector.reset();
        }
        self.unvoiced_gain = 0.0;
        self.zero_crossings = 0.0;
        self.level_env.reset();
        self.levels = [0.0; MAX_BANDS];
    }

    fn tail(&self) -> Tail {
        Tail::Frames((self.value(RELEASE) * 0.001 * self.sr * 4.0) as u32 + 2048)
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.vocoder",
    name: "Vocoder",
    role: DeviceRole::AudioEffect,
    category: "Spectral",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ports: Ports {
            sidechain: true,
            ..Default::default()
        },
        ..Default::default()
    },
    create: |_| Box::new(Vocoder::default()),
};
