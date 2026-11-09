//! Wavetables: band-limited single-cycle frames that can be swept through, and generators for
//! original tables made from code.

use super::fft::{Complex32, Fft};
use super::util::{lerp, Rng};
use std::f32::consts::{PI, TAU};

/// A sweepable set of single-cycle frames, each kept at several bandwidths so it won't alias
/// however high it's played.
#[derive(Clone, Debug)]
pub struct Wavetable {
    frame_len: usize,
    frames: usize,
    capacity: usize,
    /// Per mip: start offset and length, including one wrap-around sample.
    mips: Vec<(usize, usize)>,
    data: Vec<f32>,
}

/// The mip levels of a `frame_len` table: each has half the harmonics of the one above it, down
/// to the fundamental alone, but no less than a table's worth of samples to read smoothly.
/// `(samples per frame, harmonics kept)`.
fn mip_levels(frame_len: usize) -> Vec<(usize, usize)> {
    const MIN_MIP_LEN: usize = 32;
    (0..)
        .map(|m| frame_len >> (m + 1))
        .take_while(|h| *h >= 1)
        .map(|harmonics| {
            let len = (harmonics * 2).max(MIN_MIP_LEN);
            let keep = if harmonics * 2 == len {
                (harmonics - 1).max(1)
            } else {
                harmonics
            };
            (len, keep)
        })
        .collect()
}

impl Wavetable {
    /// An empty table with room for `capacity` frames, reading as silence until frames are
    /// written with a [`WavetableBuilder`]. Allocates.
    pub fn with_capacity(capacity: usize, frame_len: usize) -> Self {
        let capacity = capacity.max(1);
        let frame_len = frame_len.next_power_of_two().max(32);
        let levels = mip_levels(frame_len);
        let total: usize = levels.iter().map(|(l, _)| (l + 1) * capacity).sum();
        let mut mips = Vec::with_capacity(levels.len());
        let mut offset = 0;
        for &(len, _) in &levels {
            mips.push((offset, len));
            offset += (len + 1) * capacity;
        }
        Self {
            frame_len,
            frames: 1,
            capacity,
            mips,
            data: vec![0.0; total],
        }
    }

    /// Builds the bandwidths for `frames`, each a single cycle of `frame_len` samples (a power
    /// of two). Allocates.
    pub fn from_frames(frames: &[Vec<f32>], frame_len: usize) -> Self {
        let frame_count = frames.len().max(1);
        let mut table = Self::with_capacity(frame_count, frame_len);
        let mut builder = WavetableBuilder::new(table.frame_len);
        let silent = vec![0.0f32; table.frame_len];
        for f in 0..frame_count {
            let frame = frames.get(f).map_or(silent.as_slice(), |v| v.as_slice());
            builder.write_frame(&mut table, f, frame);
        }
        builder.finish(&mut table, frame_count);
        table
    }

    /// How many frames this table has room for.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    #[inline]
    fn sample(&self, mip: usize, frame: usize, phase: f32) -> f32 {
        let (offset, len) = self.mips[mip];
        let start = offset + frame * (len + 1);
        let at = phase * len as f32;
        let i = (at as usize).min(len - 1);
        let t = at - i as f32;
        let a = self.data[start + i];
        a + (self.data[start + i + 1] - a) * t
    }

    #[inline]
    fn sweep(&self, mip: usize, position: f32, phase: f32) -> f32 {
        if self.frames < 2 {
            return self.sample(mip, 0, phase);
        }
        let at = position.clamp(0.0, 1.0) * (self.frames - 1) as f32;
        let i = (at as usize).min(self.frames - 2);
        lerp(
            self.sample(mip, i, phase),
            self.sample(mip, i + 1, phase),
            at - i as f32,
        )
    }

    /// Reads at `phase` (0..1) and `position` (0..1 through the frames), for a tone moving by
    /// `dt` cycles per sample.
    #[inline]
    pub fn read(&self, phase: f32, position: f32, dt: f32) -> f32 {
        let phase = phase.rem_euclid(1.0);
        // The highest harmonic that fits is 0.5 / dt; mip m has `frame_len / 2^m / 2` of them.
        let harmonics = (0.5 / dt.abs().max(1e-6)).max(1.0);
        let top = self.frame_len as f32 * 0.5;
        let octaves = (top / harmonics).max(1.0).log2();
        let last = self.mips.len() - 1;
        let mip = (octaves.ceil() as usize).min(last);
        // Near the top of each octave, lean towards the next duller mip so the switch isn't heard.
        let lean = ((octaves - (mip as f32 - 1.0) - 0.7) / 0.3).clamp(0.0, 1.0);
        if mip >= last || mip == 0 || lean <= 0.0 {
            return self.sweep(mip, position, phase);
        }
        lerp(
            self.sweep(mip, position, phase),
            self.sweep(mip + 1, position, phase),
            lean,
        )
    }
}

/// Writes frames into a table without allocating, so a table can be rebuilt a frame at a time on
/// the audio thread. Its plans and buffers are made once, by [`new`](Self::new).
#[derive(Clone, Debug)]
pub struct WavetableBuilder {
    levels: Vec<(usize, usize)>,
    forward: Fft,
    inverse: Vec<Fft>,
    spectrum: Vec<Complex32>,
    small: Vec<Vec<Complex32>>,
    output: Vec<Vec<f32>>,
    scratch: Vec<f32>,
}

impl WavetableBuilder {
    pub fn new(frame_len: usize) -> Self {
        let frame_len = frame_len.next_power_of_two().max(32);
        let levels = mip_levels(frame_len);
        let forward = Fft::new(frame_len);
        let bins = forward.bins();
        let inverse: Vec<Fft> = levels.iter().map(|(len, _)| Fft::new(*len)).collect();
        Self {
            spectrum: vec![Complex32::new(0.0, 0.0); bins],
            small: inverse
                .iter()
                .map(|f| vec![Complex32::new(0.0, 0.0); f.bins()])
                .collect(),
            output: levels.iter().map(|(len, _)| vec![0.0; *len]).collect(),
            scratch: vec![0.0; frame_len],
            forward,
            inverse,
            levels,
        }
    }

    /// Writes one frame at `index`, at every bandwidth. `frame` is one cycle of up to `frame_len`
    /// samples.
    pub fn write_frame(&mut self, table: &mut Wavetable, index: usize, frame: &[f32]) {
        let frame_len = table.frame_len;
        self.scratch.fill(0.0);
        let n = frame.len().min(frame_len);
        self.scratch[..n].copy_from_slice(&frame[..n]);
        self.forward.forward(&mut self.scratch, &mut self.spectrum);
        for (i, &(len, keep)) in self.levels.iter().enumerate() {
            let small = &mut self.small[i];
            small.fill(Complex32::new(0.0, 0.0));
            let scale = len as f32 / frame_len as f32;
            for (out, bin) in small[1..=keep].iter_mut().zip(&self.spectrum[1..]) {
                *out = bin * scale;
            }
            self.inverse[i].inverse(small, &mut self.output[i]);
            let (offset, _) = table.mips[i];
            let start = offset + index * (len + 1);
            table.data[start..start + len].copy_from_slice(&self.output[i]);
            table.data[start + len] = self.output[i][0];
        }
    }

    /// Makes `frames` the readable part of `table`, scaled to a peak of one. The table's level
    /// across its positions stays what the frames made it.
    pub fn finish(&self, table: &mut Wavetable, frames: usize) {
        let frames = frames.clamp(1, table.capacity());
        table.frames = frames;
        let mut peak = 0.0f32;
        for &(offset, len) in &table.mips {
            for s in &table.data[offset..offset + (len + 1) * frames] {
                peak = peak.max(s.abs());
            }
        }
        if peak > 0.0 {
            for i in 0..table.mips.len() {
                let (offset, len) = table.mips[i];
                for s in &mut table.data[offset..offset + (len + 1) * frames] {
                    *s /= peak;
                }
            }
        }
    }
}

/// A phase accumulator for reading a [`Wavetable`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WavetableOsc {
    pub phase: f32,
}

impl WavetableOsc {
    /// The next sample, for a tone moving by `dt` cycles per sample.
    #[inline]
    pub fn next(&mut self, table: &Wavetable, position: f32, dt: f32) -> f32 {
        let out = table.read(self.phase, position, dt);
        self.phase = (self.phase + dt).rem_euclid(1.0);
        out
    }
}

/// Families of generated tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TableKind {
    /// Sine to triangle to saw to square.
    #[default]
    BasicShapes,
    /// Saw-rich additive frames, with the rolloff opening across the table.
    Harmonics,
    /// Square with the pulse width sweeping.
    PulseWidth,
    /// Two-operator FM with the index rising across the table.
    Fm,
    /// Vocal-like fixed resonances moving across the table.
    Formants,
    /// Random spectra, smooth from frame to frame. Different seeds sound different.
    Spectral,
}

const FRAME_LEN: usize = 2048;

fn additive(harmonics: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut frame = vec![0.0f32; FRAME_LEN];
    for h in 1..FRAME_LEN / 2 {
        let a = harmonics(h);
        if a.abs() < 1e-5 {
            continue;
        }
        for (i, s) in frame.iter_mut().enumerate() {
            *s += a * (TAU * h as f32 * i as f32 / FRAME_LEN as f32).sin();
        }
    }
    frame
}

/// Makes a table of `frames` frames. Allocates; do this away from the audio thread.
pub fn generate_wavetable(kind: TableKind, frames: usize, seed: u32) -> Wavetable {
    let count = frames.max(2);
    let n = FRAME_LEN as f32;
    let list: Vec<Vec<f32>> = (0..count)
        .map(|f| {
            let t = f as f32 / (count - 1) as f32;
            match kind {
                TableKind::BasicShapes => (0..FRAME_LEN)
                    .map(|i| {
                        let p = i as f32 / n;
                        let sine = (TAU * p).sin();
                        let tri = if p < 0.25 {
                            4.0 * p
                        } else if p < 0.75 {
                            2.0 - 4.0 * p
                        } else {
                            4.0 * p - 4.0
                        };
                        let saw = 2.0 * (p + 0.5).fract() - 1.0;
                        let square = if p < 0.5 { 1.0 } else { -1.0 };
                        let x = t * 3.0;
                        if x < 1.0 {
                            lerp(sine, tri, x)
                        } else if x < 2.0 {
                            lerp(tri, saw, x - 1.0)
                        } else {
                            lerp(saw, square, x - 2.0)
                        }
                    })
                    .collect(),
                TableKind::Harmonics => {
                    let rolloff = 3.0 - 2.0 * t;
                    additive(|h| (h as f32).powf(-rolloff))
                }
                TableKind::PulseWidth => {
                    let width = 0.5 - 0.46 * t;
                    (0..FRAME_LEN)
                        .map(|i| if (i as f32 / n) < width { 1.0 } else { -1.0 })
                        .collect()
                }
                TableKind::Fm => {
                    let index = 8.0 * t * t;
                    (0..FRAME_LEN)
                        .map(|i| {
                            let p = TAU * i as f32 / n;
                            (p + index * (2.0 * p).sin()).sin()
                        })
                        .collect()
                }
                TableKind::Formants => {
                    let f1 = 3.0 + 12.0 * t;
                    let f2 = 18.0 + 30.0 * (t * PI).sin();
                    additive(|h| {
                        let h = h as f32;
                        let a = 1.0 / (1.0 + (h - f1).powi(2) * 0.08);
                        let b = 0.7 / (1.0 + (h - f2).powi(2) * 0.02);
                        (a + b) / h.sqrt()
                    })
                }
                TableKind::Spectral => {
                    let mut rng = Rng::new(seed);
                    let a: Vec<f32> = (0..96).map(|_| rng.unipolar()).collect();
                    let b: Vec<f32> = (0..96).map(|_| rng.unipolar()).collect();
                    additive(|h| {
                        if h > 96 {
                            return 0.0;
                        }
                        lerp(a[h - 1], b[h - 1], t) / (h as f32)
                    })
                }
            }
        })
        .collect();
    Wavetable::from_frames(&list, FRAME_LEN)
}
