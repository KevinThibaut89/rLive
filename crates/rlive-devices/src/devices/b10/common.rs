//! What the sample players share: reading a buffer, looping through it, and the analysis a
//! sample gets on the UI thread.

use crate::dsp::filter::{Circuit, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::util::cubic;
use rlive_core::SampleBuffer;
use std::f32::consts::FRAC_PI_2;
use std::sync::OnceLock;

/// How many (min, max) pairs of a sample's overview a waveform display reads.
pub const WAVE_POINTS: usize = 128;

/// How many values a waveform overview publishes.
pub const WAVE_VALUES: usize = WAVE_POINTS * 2;

/// The slot a device keeps its [`Analysis`] resources from, one per sample slot. Sample slots
/// count up from zero, so the analysis of slot `n` is resource slot `ANALYSIS_SLOTS + n`.
pub const ANALYSIS_SLOTS: u32 = 1_000_000;

/// The resource slot a device's parsed saved state arrives at.
pub const STATE_SLOT: u32 = 2_000_000;

/// How far a loop or region marker may move to find a zero crossing, in frames.
pub const SNAP_RANGE: i64 = 2_048;

/// How accurately a transposed sample is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Interp {
    /// Linear.
    #[default]
    Normal,
    /// Cubic through four points.
    Good,
    /// Windowed sinc through eight points.
    Best,
}

impl Interp {
    pub fn from_index(i: f64) -> Self {
        match i.round() as i32 {
            1 => Self::Good,
            2 => Self::Best,
            _ => Self::Normal,
        }
    }
}

const SINC_TAPS: usize = 8;
const SINC_PHASES: usize = 64;

static SINC_TABLE: OnceLock<Vec<f32>> = OnceLock::new();

/// Fills the sinc table. Call it off the audio thread before a device can read at Best.
pub fn prepare_interpolation() {
    SINC_TABLE.get_or_init(|| {
        let mut table = vec![0.0; (SINC_PHASES + 1) * SINC_TAPS];
        for phase in 0..=SINC_PHASES {
            let frac = phase as f64 / SINC_PHASES as f64;
            for tap in 0..SINC_TAPS {
                let x = tap as f64 - (SINC_TAPS / 2 - 1) as f64 - frac;
                let px = std::f64::consts::PI * x;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { px.sin() / px };
                let t = x / (SINC_TAPS / 2) as f64;
                let window = if t.abs() >= 1.0 {
                    0.0
                } else {
                    let c = (std::f64::consts::FRAC_PI_2 * t).cos();
                    c * c
                };
                table[phase * SINC_TAPS + tap] = (sinc * window) as f32;
            }
        }
        table
    });
}

#[inline]
fn at(channel: &[f32], i: i64) -> f32 {
    if i >= 0 {
        channel.get(i as usize).copied().unwrap_or(0.0)
    } else {
        0.0
    }
}

/// One channel of the sample at a fractional position. Outside the sample it reads silence.
#[inline]
pub fn read_channel(channel: &[f32], pos: f64, interp: Interp) -> f32 {
    let idx = pos.floor() as i64;
    let frac = (pos - idx as f64) as f32;
    match interp {
        Interp::Normal => {
            let a = at(channel, idx);
            a + (at(channel, idx + 1) - a) * frac
        }
        Interp::Good => cubic(
            at(channel, idx - 1),
            at(channel, idx),
            at(channel, idx + 1),
            at(channel, idx + 2),
            frac,
        ),
        Interp::Best => {
            let Some(table) = SINC_TABLE.get() else {
                return read_channel(channel, pos, Interp::Good);
            };
            let phase = frac * SINC_PHASES as f32;
            let row = phase.floor() as usize;
            let blend = phase - row as f32;
            let first = idx - (SINC_TAPS / 2 - 1) as i64;
            let mut out = 0.0;
            for tap in 0..SINC_TAPS {
                let low = table[row * SINC_TAPS + tap];
                let high = table[(row + 1).min(SINC_PHASES) * SINC_TAPS + tap];
                out += at(channel, first + tap as i64) * (low + (high - low) * blend);
            }
            out
        }
    }
}

/// Both channels of the sample at a fractional position.
#[inline]
pub fn read(sample: &SampleBuffer, pos: f64, interp: Interp) -> [f32; 2] {
    [
        read_channel(&sample.left, pos, interp),
        read_channel(&sample.right, pos, interp),
    ]
}

/// The nearest frame to `frame` where the left channel crosses zero, within [`SNAP_RANGE`].
/// Stays where it is if there isn't one. A bounded amount of work, so a note-on can do it.
pub fn snap_to_zero(left: &[f32], frame: f64) -> f64 {
    let n = left.len() as i64;
    let at = (frame.round() as i64).clamp(0, (n - 1).max(0));
    let crossing =
        |i: i64| i > 0 && i < n && (left[i as usize - 1] <= 0.0) != (left[i as usize] <= 0.0);
    if n > 0 && left[at as usize] == 0.0 {
        return at as f64;
    }
    for distance in 0..SNAP_RANGE {
        if crossing(at + distance) {
            return (at + distance) as f64;
        }
        if crossing(at - distance) {
            return (at - distance) as f64;
        }
    }
    frame
}

/// A transient, in source frames, and how strong it is against its neighbours (0..1).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transient {
    pub frame: f64,
    pub strength: f32,
}

/// What the audio thread can't work out for itself about a sample. Built on the UI thread and
/// handed to a device at `ANALYSIS_SLOTS + slot`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Analysis {
    pub frames: usize,
    /// The largest absolute sample.
    pub peak: f32,
    /// `[min, max]` pairs, [`WAVE_POINTS`] of them, for a display.
    pub overview: Vec<f32>,
    /// Ascending. At most [`MAX_TRANSIENTS`].
    pub transients: Vec<Transient>,
    /// The frames of `transients`, as Beats mode wants them.
    pub transient_frames: Vec<f64>,
}

/// The most transients kept per sample, which keeps every later use bounded.
pub const MAX_TRANSIENTS: usize = 1024;

const STRENGTH_WINDOW_SECS: f64 = 0.005;

fn window_rms(channel: &[f32], from: i64, len: i64) -> f32 {
    let from = from.max(0) as usize;
    let to = (from + len as usize).min(channel.len());
    if to <= from {
        return 0.0;
    }
    let sum: f32 = channel[from..to].iter().map(|s| s * s).sum();
    (sum / (to - from) as f32).sqrt()
}

impl Analysis {
    /// Analyses `sample`. Runs [`rlive_stretch::detect_transients`] unless the sample cache has
    /// already found the transients, in seconds. Not for the audio thread.
    pub fn new(sample: &SampleBuffer, transients: Option<&[f64]>) -> Self {
        let frames = sample.len();
        let found;
        let seconds = match transients {
            Some(t) => t,
            None => {
                found = rlive_stretch::detect_transients(
                    &sample.left,
                    Some(&sample.right),
                    sample.sample_rate,
                );
                &found
            }
        };
        let win = ((STRENGTH_WINDOW_SECS * sample.sample_rate) as i64).max(8);
        let mono: Vec<f32> = (0..frames)
            .map(|i| 0.5 * (sample.left[i] + sample.right[i]))
            .collect();
        let mut transients: Vec<Transient> = seconds
            .iter()
            .take(MAX_TRANSIENTS)
            .map(|s| {
                let frame = s * sample.sample_rate;
                let at = frame as i64;
                let before = window_rms(&mono, at - win * 4, win * 4);
                let after = window_rms(&mono, at, win);
                Transient {
                    frame,
                    strength: ((after + 1e-4) / (before + 1e-4)).max(1.0).ln(),
                }
            })
            .collect();
        let strongest = transients.iter().fold(1e-6f32, |m, t| m.max(t.strength));
        for t in &mut transients {
            t.strength = (t.strength / strongest).clamp(0.0, 1.0);
        }
        let transient_frames = transients.iter().map(|t| t.frame).collect();

        let mut overview = vec![0.0; WAVE_VALUES];
        let mut peak = 0.0f32;
        if frames > 0 {
            for (i, cell) in overview.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let from = i * frames / WAVE_POINTS;
                let to = ((i + 1) * frames / WAVE_POINTS).max(from + 1).min(frames);
                let (mut low, mut high) = (0.0f32, 0.0f32);
                for s in &mono[from..to] {
                    low = low.min(*s);
                    high = high.max(*s);
                }
                cell[0] = low;
                cell[1] = high;
            }
            peak = sample
                .left
                .iter()
                .chain(&sample.right)
                .fold(0.0f32, |m, s| m.max(s.abs()));
        }
        Self {
            frames,
            peak,
            overview,
            transients,
            transient_frames,
        }
    }
}

/// How a loop repeats.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LoopKind {
    #[default]
    Off,
    Forward,
    BackAndForth,
}

/// What a voice does after its note is released, relative to a sustain loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ReleaseMode {
    /// Stays inside the sustain loop until the envelope finishes.
    #[default]
    Off,
    /// Runs on to the sample end.
    Linear,
    Loop,
    BackAndForth,
}

#[derive(Clone, Copy, Debug, Default)]
struct Region {
    start: f64,
    end: f64,
    kind: LoopKind,
}

/// Crossfade gains at `t` through a crossfade: the gain of what's arriving, then of what's
/// leaving.
#[inline]
pub fn crossfade_gains(t: f32, constant_power: bool) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    if constant_power {
        (FRAC_PI_2 * t).sin_cos()
    } else {
        (t, 1.0 - t)
    }
}

/// A read head moving through a sample's region, its loops and their crossfades.
///
/// Positions are kept in forward time. A reversed head is the same head reading the region from
/// the other end, so loops, crossfades and reflections need no second set of rules.
#[derive(Clone, Copy, Debug, Default)]
pub struct Player {
    start: f64,
    end: f64,
    reverse: bool,
    sustain: Region,
    release: Region,
    release_mode: ReleaseMode,
    crossfade: f64,
    power: bool,
    detune: f64,
    pos: f64,
    backwards: bool,
    in_loop: bool,
}

impl Player {
    /// A head at the start of the region `[start, end)`.
    pub fn new(start: f64, end: f64, reverse: bool) -> Self {
        Self {
            start,
            end: end.max(start),
            reverse,
            pos: start,
            detune: 1.0,
            ..Self::default()
        }
    }

    #[inline]
    fn mirror(&self, pos: f64) -> f64 {
        if self.reverse {
            self.start + self.end - pos
        } else {
            pos
        }
    }

    /// Adds a sustain loop, in sample frames. `crossfade` is in frames and `detune` is the
    /// pitch ratio while looping.
    pub fn set_sustain(
        &mut self,
        (start, end): (f64, f64),
        kind: LoopKind,
        crossfade: f64,
        power: bool,
        detune: f64,
    ) {
        let (a, b) = (
            self.mirror(start.clamp(self.start, self.end)),
            self.mirror(end.clamp(self.start, self.end)),
        );
        let (a, b) = (a.min(b), a.max(b));
        self.sustain = Region {
            start: a,
            end: b,
            kind: if b - a < 1.0 { LoopKind::Off } else { kind },
        };
        self.crossfade = crossfade.max(0.0);
        self.power = power;
        self.detune = detune;
    }

    /// Says what a release does. `loop_start` is where a release loop returns to.
    pub fn set_release(&mut self, mode: ReleaseMode, loop_start: f64) {
        self.release_mode = mode;
        let a = self.mirror(loop_start.clamp(self.start, self.end));
        let (a, b) = if self.reverse {
            (self.start, a)
        } else {
            (a, self.end)
        };
        self.release = Region {
            start: a,
            end: b,
            kind: match mode {
                ReleaseMode::Loop => LoopKind::Forward,
                ReleaseMode::BackAndForth => LoopKind::BackAndForth,
                _ => LoopKind::Off,
            },
        };
    }

    /// Where the head is, in sample frames.
    pub fn position(&self) -> f64 {
        self.mirror(self.pos)
    }

    /// Moves the head to `pos`, in sample frames.
    pub fn seek(&mut self, pos: f64) {
        self.pos = self.mirror(pos.clamp(self.start, self.end));
    }

    fn crossfade_now(&self, r: &Region) -> f64 {
        // A crossfade reads from before the loop, so it can't be longer than what's there.
        self.crossfade
            .min(r.end - r.start)
            .min(r.start - self.start)
            .max(0.0)
    }

    /// The sample under the head, with the crossfade of the loop it is in applied.
    #[inline]
    pub fn sample(&self, sample: &SampleBuffer, interp: Interp, released: bool) -> [f32; 2] {
        let main = read(sample, self.mirror(self.pos), interp);
        let looping = self.active_loop(released);
        let Some(region) = looping else {
            return main;
        };
        if region.kind != LoopKind::Forward || self.backwards {
            return main;
        }
        let fade = self.crossfade_now(&region);
        if fade <= 0.0 || self.pos < region.end - fade {
            return main;
        }
        let t = ((self.pos - (region.end - fade)) / fade) as f32;
        let (arriving, leaving) = crossfade_gains(t, self.power);
        let early = read(
            sample,
            self.mirror(self.pos - (region.end - region.start)),
            interp,
        );
        [
            main[0] * leaving + early[0] * arriving,
            main[1] * leaving + early[1] * arriving,
        ]
    }

    #[inline]
    fn active_loop(&self, released: bool) -> Option<Region> {
        if self.sustain.kind != LoopKind::Off
            && (!released || self.release_mode == ReleaseMode::Off)
        {
            return Some(self.sustain);
        }
        if released && self.release.kind != LoopKind::Off {
            return Some(self.release);
        }
        None
    }

    /// Moves the head on by `step` frames. Returns whether there is anything left to play.
    #[inline]
    pub fn advance(&mut self, step: f64, released: bool) -> bool {
        let looping = self.active_loop(released);
        if released
            && self.release_mode != ReleaseMode::Off
            && self.release_mode != ReleaseMode::Loop
            && looping.is_none_or(|r| r.kind != LoopKind::BackAndForth)
        {
            self.backwards = false;
        }
        let distance = if self.in_loop {
            step * self.detune
        } else {
            step
        };
        self.pos += if self.backwards { -distance } else { distance };
        match looping {
            Some(region) => {
                if self.pos >= region.start {
                    self.in_loop = true;
                }
                self.wrap(&region)
            }
            None => self.pos < self.end,
        }
    }

    #[inline]
    fn wrap(&mut self, r: &Region) -> bool {
        let len = r.end - r.start;
        if len < 1.0 {
            return self.pos < self.end;
        }
        match r.kind {
            LoopKind::Off => self.pos < self.end,
            LoopKind::Forward => {
                if self.pos >= r.end {
                    self.pos = r.start + (self.pos - r.end) % len;
                }
                true
            }
            LoopKind::BackAndForth => {
                if self.pos >= r.end {
                    self.pos = r.end - (self.pos - r.end).min(len);
                    self.backwards = true;
                } else if self.pos < r.start && self.backwards {
                    self.pos = r.start + (r.start - self.pos).min(len);
                    self.backwards = false;
                }
                true
            }
        }
    }
}

/// Sets every parameter the sample filters share. Circuits the manual doesn't offer for a type
/// fall back to Clean (pp. 787, 797): only Clean and OSR have every type, MS2, SMP and PRD are
/// low-pass and high-pass only.
pub fn filter_settings(
    kind_index: f64,
    circuit_index: f64,
    slope_index: f64,
    cutoff: f32,
    resonance: f32,
    drive: f32,
    morph: f32,
) -> FilterSettings {
    let kind = match kind_index.round() as i32 {
        1 => FilterKind::HighPass,
        2 => FilterKind::BandPass,
        3 => FilterKind::Notch,
        4 => FilterKind::Morph,
        _ => FilterKind::LowPass,
    };
    let picked = match circuit_index.round() as i32 {
        1 => Circuit::Osr,
        2 => Circuit::Ms2,
        3 => Circuit::Smp,
        4 => Circuit::Prd,
        _ => Circuit::Svf,
    };
    let any_type = matches!(picked, Circuit::Svf | Circuit::Osr);
    let simple = matches!(kind, FilterKind::LowPass | FilterKind::HighPass);
    FilterSettings {
        circuit: if any_type || simple {
            picked
        } else {
            Circuit::Svf
        },
        kind,
        slope: if slope_index.round() as i32 == 1 {
            FilterSlope::Db24
        } else {
            FilterSlope::Db12
        },
        cutoff,
        resonance,
        drive: if picked == Circuit::Svf { 0.0 } else { drive },
        morph,
    }
}

/// Menu labels of a filter's type, circuit and slope.
pub const FILTER_TYPES: [&str; 5] = ["Low-pass", "High-pass", "Band-pass", "Notch", "Morph"];
pub const FILTER_CIRCUITS: [&str; 5] = ["Clean", "OSR", "MS2", "SMP", "PRD"];
pub const FILTER_SLOPES: [&str; 2] = ["12 dB", "24 dB"];
