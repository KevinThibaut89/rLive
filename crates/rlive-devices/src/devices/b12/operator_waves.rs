//! Operator's waveforms: the built-in set, the user harmonics and the `.ams` file format.
//!
//! The built-in saw, square and triangle are additive resynthesis with the named number of
//! harmonics, as the manual says; a square's harmonics are its odd ones. Which names exist beyond
//! the examples it gives ("Square 6", "Saw 32", "Saw D", "Square D", "Sine 4 Bit") is ours. A user
//! wave stores up to 64 partial amplitudes. An `.ams` file is a one-cycle RIFF WAVE with a private
//! chunk holding the partials exactly: the manual doesn't describe the format and says only that
//! Simpler and Sampler load the same files, which a WAVE satisfies.

use crate::dsp::fft::{Complex32, Fft};
use serde::{Deserialize, Serialize};
use std::f32::consts::TAU;
use std::sync::OnceLock;

pub const PARTIALS: usize = 64;
pub const FRAME_LEN: usize = 2048;

pub const WAVE_NAMES: [&str; 23] = [
    "Sine",
    "Sine 4 Bit",
    "Sine 8 Bit",
    "Saw 3",
    "Saw 4",
    "Saw 6",
    "Saw 8",
    "Saw 16",
    "Saw 32",
    "Saw 64",
    "Saw D",
    "Square 3",
    "Square 4",
    "Square 6",
    "Square 8",
    "Square 16",
    "Square 32",
    "Square 64",
    "Square D",
    "Triangle",
    "Noise Looped",
    "Noise White",
    "User",
];
pub const W_SINE: usize = 0;
pub const W_SAW_8: usize = 6;
pub const W_SAW_64: usize = 9;
pub const W_SQUARE_6: usize = 13;
pub const W_SQUARE_64: usize = 17;
pub const W_TRIANGLE: usize = 19;
pub const W_NOISE_LOOP: usize = 20;
pub const W_NOISE_WHITE: usize = 21;
pub const W_USER: usize = 22;

const HARMONICS: usize = 512;

/// The partial amplitudes of a wave named in [`WAVE_NAMES`], as far as they go.
pub fn builtin_partials(wave: usize) -> [f32; PARTIALS] {
    let mut p = [0.0; PARTIALS];
    let saw = |p: &mut [f32; PARTIALS], n: usize| {
        for (k, a) in p.iter_mut().enumerate().take(n) {
            *a = 1.0 / (k + 1) as f32;
        }
    };
    let square = |p: &mut [f32; PARTIALS], n: usize| {
        for i in 0..n.min(PARTIALS / 2) {
            p[2 * i] = 1.0 / (2 * i + 1) as f32;
        }
    };
    match wave {
        3 => saw(&mut p, 3),
        4 => saw(&mut p, 4),
        5 => saw(&mut p, 6),
        6 => saw(&mut p, 8),
        7 => saw(&mut p, 16),
        8 => saw(&mut p, 32),
        9 => saw(&mut p, 64),
        11 => square(&mut p, 3),
        12 => square(&mut p, 4),
        13 => square(&mut p, 6),
        14 => square(&mut p, 8),
        15 => square(&mut p, 16),
        16 => square(&mut p, 32),
        17 => square(&mut p, 64),
        W_TRIANGLE => {
            for i in 0..16 {
                p[2 * i] = 1.0 / ((2 * i + 1) * (2 * i + 1)) as f32;
            }
        }
        _ => {
            if wave == W_SINE {
                p[0] = 1.0;
            } else {
                let frame = builtin_frame(wave);
                return partials_of_frame(&frame);
            }
        }
    }
    p
}

fn additive(a: &[f32]) -> Vec<f32> {
    let mut frame = vec![0.0f32; FRAME_LEN];
    for (k, amplitude) in a.iter().enumerate() {
        if *amplitude == 0.0 {
            continue;
        }
        let w = TAU * (k + 1) as f32 / FRAME_LEN as f32;
        for (i, s) in frame.iter_mut().enumerate() {
            *s += amplitude * (w * i as f32).sin();
        }
    }
    frame
}

fn builtin_frame(wave: usize) -> Vec<f32> {
    let sine = |i: usize| (TAU * i as f32 / FRAME_LEN as f32).sin();
    let quantised = |levels: f32| -> Vec<f32> {
        (0..FRAME_LEN)
            .map(|i| ((sine(i) * 0.5 + 0.5) * (levels - 1.0)).round() / (levels - 1.0) * 2.0 - 1.0)
            .collect()
    };
    match wave {
        1 => quantised(16.0),
        2 => quantised(256.0),
        10 => (0..FRAME_LEN)
            .map(|i| 1.0 - 2.0 * i as f32 / FRAME_LEN as f32)
            .collect(),
        18 => (0..FRAME_LEN)
            .map(|i| if i < FRAME_LEN / 2 { 1.0 } else { -1.0 })
            .collect(),
        _ => additive(&builtin_partials(wave)),
    }
}

/// Amplitudes of the first [`PARTIALS`] harmonics of one cycle, by direct Fourier sum.
pub fn partials_of_frame(frame: &[f32]) -> [f32; PARTIALS] {
    let mut p = [0.0f32; PARTIALS];
    let n = frame.len().max(1) as f32;
    for (k, a) in p.iter_mut().enumerate() {
        let w = TAU * (k + 1) as f32 / n;
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (i, s) in frame.iter().enumerate() {
            let t = w * i as f32;
            re += s * t.cos();
            im += s * t.sin();
        }
        *a = (re * re + im * im).sqrt() * 2.0 / n;
    }
    p
}

/// A wave stored at decreasing bandwidths, each at full table length.
///
/// The shared `dsp::wavetable` shrinks its high-frequency levels to a few dozen samples, which at
/// the top of the keyboard limits the noise floor of an FM-modulated read to about -36 dB. Here
/// each level has [`TABLE_LEN`] samples, and the level for a note is chosen once per control block.
#[derive(Clone, Debug)]
pub struct MipTable {
    data: Vec<f32>,
    levels: usize,
    /// How much a partial needs to be scaled up to undo the frame's own peak normalisation.
    pub peak: f32,
}

/// Samples in a level of a [`MipTable`].
pub const TABLE_LEN: usize = 2048;
/// Levels in a [`MipTable`]: 1023 harmonics, 511, 255, ... down to one.
const TABLE_LEVELS: usize = 11;

impl MipTable {
    /// From one cycle of [`TABLE_LEN`] samples. Allocates.
    pub fn new(frame: &[f32]) -> Self {
        let mut fft = Fft::new(TABLE_LEN);
        let mut time = vec![0.0f32; TABLE_LEN];
        let n = frame.len().min(TABLE_LEN);
        time[..n].copy_from_slice(&frame[..n]);
        let mut spectrum = vec![Complex32::new(0.0, 0.0); fft.bins()];
        fft.forward(&mut time, &mut spectrum);
        let mut data = vec![0.0f32; (TABLE_LEN + 1) * TABLE_LEVELS];
        let mut output = vec![Complex32::new(0.0, 0.0); fft.bins()];
        let mut peak = 0.0f32;
        for level in 0..TABLE_LEVELS {
            let harmonics = ((TABLE_LEN / 2) >> level).saturating_sub(1).max(1);
            output.copy_from_slice(&spectrum);
            for (bin, value) in output.iter_mut().enumerate() {
                if bin > harmonics {
                    *value = Complex32::new(0.0, 0.0);
                }
            }
            fft.inverse(&mut output, &mut time);
            let at = level * (TABLE_LEN + 1);
            data[at..at + TABLE_LEN].copy_from_slice(&time);
            data[at + TABLE_LEN] = time[0];
            if level == 0 {
                peak = time.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            }
        }
        if peak > 0.0 {
            data.iter_mut().for_each(|s| *s /= peak);
        }
        Self {
            data,
            levels: TABLE_LEVELS,
            peak,
        }
    }

    /// Which level holds only harmonics a note of `dt` cycles per sample can carry.
    pub fn level_for(&self, dt: f32) -> usize {
        let highest = (0.5 / dt.abs().max(1e-6)).max(1.0);
        let top = (TABLE_LEN / 2 - 1) as f32;
        let octaves = (top / highest).max(1.0).log2();
        (octaves.ceil() as usize).min(self.levels - 1)
    }

    /// Reads level `level` at `phase`, `0.0..1.0`, with linear interpolation or the nearest sample.
    #[inline]
    pub fn read(&self, level: usize, phase: f32, interpolate: bool) -> f32 {
        let at = level * (TABLE_LEN + 1);
        let x = phase * TABLE_LEN as f32;
        let i = (x as usize).min(TABLE_LEN - 1);
        let a = self.data[at + i];
        if interpolate {
            a + (self.data[at + i + 1] - a) * (x - i as f32)
        } else {
            a
        }
    }
}

struct Builtins {
    tables: Vec<MipTable>,
}

fn builtins() -> &'static Builtins {
    static TABLES: OnceLock<Builtins> = OnceLock::new();
    TABLES.get_or_init(|| Builtins {
        tables: (0..WAVE_NAMES.len())
            .map(|w| {
                let frame = if matches!(w, W_NOISE_LOOP | W_NOISE_WHITE | W_USER) {
                    vec![0.0; FRAME_LEN]
                } else {
                    builtin_frame(w)
                };
                MipTable::new(&frame)
            })
            .collect(),
    })
}

/// The table of a built-in wave.
pub fn builtin_table(wave: usize) -> &'static MipTable {
    &builtins().tables[wave.min(WAVE_NAMES.len() - 1)]
}

/// A second of looping noise, shared by every voice.
pub fn noise_loop() -> &'static [f32] {
    static NOISE: OnceLock<Vec<f32>> = OnceLock::new();
    NOISE.get_or_init(|| {
        let mut rng = crate::dsp::util::Rng::new(0x51ab);
        (0..65_536).map(|_| rng.bipolar()).collect()
    })
}

pub fn sine_table() -> &'static [f32] {
    static SINE: OnceLock<Vec<f32>> = OnceLock::new();
    SINE.get_or_init(|| {
        (0..=SINE_LEN)
            .map(|i| (TAU * i as f32 / SINE_LEN as f32).sin())
            .collect()
    })
}

pub const SINE_LEN: usize = 4096;

/// One oscillator's harmonics editor state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserWave {
    /// 16, 32 or 64.
    pub count: usize,
    /// 0 is off.
    pub repeat: usize,
    pub normalize: bool,
    /// 0 All, 1 Even only, 2 Odd only.
    pub restrict: usize,
    pub partials: Vec<f32>,
}

impl Default for UserWave {
    fn default() -> Self {
        let mut partials = vec![0.0; PARTIALS];
        partials[0] = 1.0;
        Self {
            count: 16,
            repeat: 0,
            normalize: true,
            restrict: 0,
            partials,
        }
    }
}

/// The highest Repeat setting.
pub const REPEAT_MAX: usize = 8;

impl UserWave {
    /// Amplitudes up to harmonic [`HARMONICS`] after Repeat. Harmonics beyond the drawn ones copy
    /// the drawn ones, each copy quieter than the last: `1 / (1 + repeat / 2)` per copy. The
    /// manual gives no figures for Repeat.
    pub fn spectrum(&self) -> Vec<f32> {
        let count = self.count.clamp(1, PARTIALS);
        let mut out = vec![0.0f32; HARMONICS];
        let drawn = &self.partials;
        for (k, a) in out.iter_mut().enumerate() {
            let source = drawn.get(k % count).copied().unwrap_or(0.0);
            if k < count {
                *a = source;
            } else if self.repeat > 0 {
                let fade = 1.0 / (1.0 + self.repeat as f32 * 0.5);
                *a = source * fade.powi((k / count) as i32);
            }
        }
        out
    }

    /// One cycle of the wave, not normalised.
    pub fn frame(&self) -> Vec<f32> {
        additive(&self.spectrum())
    }

    /// The linear gain to apply on top of the table's unit peak: 1 when normalised, the frame's
    /// real peak otherwise, so that drawing more harmonics makes it louder.
    pub fn gain(&self, frame: &[f32]) -> f32 {
        if self.normalize {
            1.0
        } else {
            frame.iter().fold(0.0f32, |m, s| m.max(s.abs()))
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WavesState {
    pub ops: [UserWave; 4],
}

impl WavesState {
    pub fn parse(text: Option<&str>) -> Self {
        text.and_then(|t| ron::from_str(t).ok()).unwrap_or_default()
    }

    pub fn to_text(&self) -> String {
        ron::to_string(self).unwrap_or_default()
    }
}

/// What the audio thread reads for the four user waves. Built on the UI thread.
pub struct ParsedWaves {
    pub tables: [MipTable; 4],
    pub gains: [f32; 4],
}

impl ParsedWaves {
    pub fn parse(text: Option<&str>) -> Self {
        Self::from_state(&WavesState::parse(text))
    }

    pub fn from_state(state: &WavesState) -> Self {
        let mut gains = [1.0; 4];
        let tables: Vec<MipTable> = state
            .ops
            .iter()
            .enumerate()
            .map(|(op, wave)| {
                let frame = wave.frame();
                gains[op] = wave.gain(&frame);
                MipTable::new(&frame)
            })
            .collect();
        let tables: [MipTable; 4] = tables.try_into().unwrap_or_else(|_| unreachable!());
        Self { tables, gains }
    }
}

const AMS_CHUNK: &[u8; 4] = b"rLWv";

/// A `.ams` file for a wave: one cycle of float samples, and the partials in a private chunk.
pub fn to_ams(wave: &UserWave) -> Vec<u8> {
    let frame = wave.frame();
    let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-9);
    let data_len = frame.len() * 4;
    let mut extra = Vec::with_capacity(16 + PARTIALS * 4);
    extra.extend_from_slice(&(wave.count as u32).to_le_bytes());
    extra.extend_from_slice(&(wave.repeat as u32).to_le_bytes());
    extra.extend_from_slice(&(wave.normalize as u32).to_le_bytes());
    extra.extend_from_slice(&(wave.restrict as u32).to_le_bytes());
    for i in 0..PARTIALS {
        extra.extend_from_slice(&wave.partials.get(i).copied().unwrap_or(0.0).to_le_bytes());
    }
    let riff_len = 4 + (8 + 16) + (8 + data_len) + (8 + extra.len());
    let mut out = Vec::with_capacity(riff_len + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in &frame {
        out.extend_from_slice(&(s / peak).to_le_bytes());
    }
    out.extend_from_slice(AMS_CHUNK);
    out.extend_from_slice(&(extra.len() as u32).to_le_bytes());
    out.extend_from_slice(&extra);
    out
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// Reads a `.ams` file back: its partials if it's one of ours, otherwise the harmonics of the
/// single cycle in any plain RIFF WAVE of 16-bit or float samples.
pub fn from_ams(bytes: &[u8]) -> Option<UserWave> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut format, mut bits, mut data) = (1u16, 16u16, None);
    let mut at = 12;
    while let (Some(id), Some(len)) = (bytes.get(at..at + 4), u32_at(bytes, at + 4)) {
        let body = at + 8;
        let len = len as usize;
        match id {
            b"fmt " => {
                format = u16_at(bytes, body)?;
                bits = u16_at(bytes, body + 14)?;
            }
            b"data" => data = bytes.get(body..body + len),
            id if id == AMS_CHUNK => {
                let count = u32_at(bytes, body)? as usize;
                let repeat = u32_at(bytes, body + 4)? as usize;
                let normalize = u32_at(bytes, body + 8)? != 0;
                let restrict = u32_at(bytes, body + 12)? as usize;
                let partials = (0..PARTIALS)
                    .map(|i| u32_at(bytes, body + 16 + i * 4).map(f32::from_bits))
                    .collect::<Option<Vec<f32>>>()?;
                return Some(UserWave {
                    count,
                    repeat,
                    normalize,
                    restrict,
                    partials,
                });
            }
            _ => {}
        }
        at = body + len + (len & 1);
    }
    let data = data?;
    let frame: Vec<f32> = match (format, bits) {
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c) as f32 / 32_768.0)
            .collect(),
        _ => return None,
    };
    if frame.is_empty() {
        return None;
    }
    let mut partials = partials_of_frame(&frame);
    let peak = partials.iter().fold(0.0f32, |m, a| m.max(*a)).max(1e-9);
    partials.iter_mut().for_each(|a| *a /= peak);
    Some(UserWave {
        count: PARTIALS,
        repeat: 0,
        normalize: true,
        restrict: 0,
        partials: partials.to_vec(),
    })
}
