//! Oversampling path: run a non-linearity at 2, 4 or 8 times the sample rate.
//!
//! Linear-phase half-band FIR stages, so the path delays everything equally and by a whole
//! number of samples; see [`Oversampler::latency`]. A device should report that latency.

use super::util::sanitize;
use std::f64::consts::PI;

/// How much an [`Oversampler`] raises the sample rate.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OversampleFactor {
    /// Off: audio goes through unchanged and without latency.
    #[default]
    X1,
    X2,
    X4,
    X8,
}

impl OversampleFactor {
    pub fn multiplier(self) -> usize {
        match self {
            Self::X1 => 1,
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
        }
    }

    fn stages(self) -> usize {
        self.multiplier().trailing_zeros() as usize
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

/// Taps of a half-band low-pass of odd length `4m + 3`, Kaiser-windowed.
fn half_band_taps(len: usize) -> Vec<f32> {
    const BETA: f64 = 10.0;
    let c = (len - 1) / 2;
    let denom = bessel_i0(BETA);
    (0..len)
        .map(|i| {
            let n = i as f64 - c as f64;
            let sinc = if i == c {
                1.0
            } else {
                (PI * n / 2.0).sin() / (PI * n / 2.0)
            };
            let r = n / c as f64;
            let window = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / denom;
            (0.5 * sinc * window) as f32
        })
        .collect()
}

// Taps per stage, the first (lowest-rate) stage having the tightest transition.
const STAGE_TAPS: [usize; 3] = [67, 35, 19];

#[derive(Clone, Debug)]
struct History {
    data: Vec<f32>,
    pos: usize,
}

impl History {
    fn new(len: usize) -> Self {
        Self {
            data: vec![0.0; len * 2],
            pos: 0,
        }
    }

    /// Writes `x` and returns the most recent `len` values, newest first.
    #[inline]
    fn push(&mut self, x: f32) -> &[f32] {
        let len = self.data.len() / 2;
        self.pos = if self.pos == 0 { len - 1 } else { self.pos - 1 };
        self.data[self.pos] = x;
        self.data[self.pos + len] = x;
        &self.data[self.pos..self.pos + len]
    }

    #[inline]
    fn peek(&self) -> &[f32] {
        let len = self.data.len() / 2;
        &self.data[self.pos..self.pos + len]
    }

    fn clear(&mut self) {
        self.data.fill(0.0);
        self.pos = 0;
    }
}

#[derive(Clone, Debug)]
struct Stage {
    /// Every other tap, the ones that aren't zero.
    even_taps: Vec<f32>,
    /// `h[c]`, 0.5 in theory, as designed.
    center: f32,
    up: History,
    /// The even samples of the signal coming down, which meet the FIR taps.
    down_even: History,
    /// The odd samples, which only meet the centre tap.
    down_odd: History,
    /// A one-sample delay on the way down, to keep the total latency a whole number.
    pad: Option<f32>,
}

impl Stage {
    fn new(len: usize, pad: bool) -> Self {
        let taps = half_band_taps(len);
        let c = (len - 1) / 2;
        // With c odd, the even-indexed taps are the non-zero ones.
        let even_taps: Vec<f32> = taps.iter().step_by(2).copied().collect();
        // (c - 1) / 2 + 1 values of the other branch reach the center tap.
        let center_delay = c.div_ceil(2);
        Self {
            up: History::new(even_taps.len()),
            down_even: History::new(even_taps.len()),
            down_odd: History::new(center_delay),
            even_taps,
            center: taps[c],
            pad: pad.then_some(0.0),
        }
    }

    #[inline]
    fn upsample(&mut self, x: f32, a: &mut f32, b: &mut f32) {
        let centre_at = self.down_odd.data.len() / 2 - 1;
        let hist = self.up.push(sanitize(x));
        let mut y = 0.0;
        for (t, s) in self.even_taps.iter().zip(hist) {
            y += t * s;
        }
        *a = 2.0 * y;
        *b = 2.0 * self.center * hist[centre_at];
    }

    #[inline]
    fn downsample(&mut self, a: f32, b: f32) -> f32 {
        let mut y = 0.0;
        for (t, s) in self.even_taps.iter().zip(self.down_even.push(sanitize(a))) {
            y += t * s;
        }
        // The centre tap reaches the odd samples, which are half a low-rate sample out of step
        // with the even ones, so `a` is taken before `b` joins the history.
        let odd = self.down_odd.peek();
        let out = y + self.center * odd[odd.len() - 1];
        self.down_odd.push(sanitize(b));
        if let Some(pad) = &mut self.pad {
            std::mem::replace(pad, out)
        } else {
            out
        }
    }

    fn reset(&mut self) {
        self.up.clear();
        self.down_even.clear();
        self.down_odd.clear();
        if let Some(pad) = &mut self.pad {
            *pad = 0.0;
        }
    }
}

/// See the [module docs](self). One channel; use two for stereo.
#[derive(Clone, Debug)]
pub struct Oversampler {
    factor: OversampleFactor,
    stages: Vec<Stage>,
    latency: u32,
    max_block: usize,
    buffers: Vec<Vec<f32>>,
}

impl Oversampler {
    /// Ready for blocks of up to `max_block` samples. Longer blocks are handled in pieces.
    pub fn new(factor: OversampleFactor, max_block: usize) -> Self {
        let max_block = max_block.max(1);
        let count = factor.stages();
        // Work outwards from the fastest stage, padding so each hands a whole number of
        // samples of the slower rate to the next.
        let mut total = 0usize;
        let mut pads = vec![false; count];
        for i in (0..count).rev() {
            total = (STAGE_TAPS[i] - 1) / 2 + total / 2;
            if i > 0 && total % 2 == 1 {
                pads[i] = true;
                total += 1;
            }
        }
        let stages = (0..count)
            .map(|i| Stage::new(STAGE_TAPS[i], pads[i]))
            .collect();
        Self {
            factor,
            stages,
            latency: total as u32,
            max_block,
            buffers: (0..count)
                .map(|i| vec![0.0; max_block << (i + 1)])
                .collect(),
        }
    }

    pub fn factor(&self) -> OversampleFactor {
        self.factor
    }

    /// The path's delay at the base sample rate.
    pub fn latency(&self) -> u32 {
        self.latency
    }

    pub fn reset(&mut self) {
        for s in &mut self.stages {
            s.reset();
        }
    }

    fn upsample(&mut self, input: &[f32]) {
        for s in 0..self.stages.len() {
            let (done, rest) = self.buffers.split_at_mut(s);
            let (src, dst): (&[f32], &mut Vec<f32>) = if s == 0 {
                (input, &mut rest[0])
            } else {
                (&done[s - 1][..input.len() << s], &mut rest[0])
            };
            let stage = &mut self.stages[s];
            for (x, [a, b]) in src.iter().zip(dst.as_chunks_mut::<2>().0.iter_mut()) {
                stage.upsample(*x, a, b);
            }
        }
    }

    fn downsample(&mut self, output: &mut [f32]) {
        for s in (0..self.stages.len()).rev() {
            let (done, rest) = self.buffers.split_at_mut(s);
            let frames = output.len() << s;
            let stage = &mut self.stages[s];
            let dst: &mut [f32] = if s == 0 {
                &mut output[..]
            } else {
                &mut done[s - 1][..frames]
            };
            for ([a, b], y) in rest[0].as_chunks::<2>().0.iter().zip(dst.iter_mut()) {
                *y = stage.downsample(*a, *b);
            }
        }
    }

    /// Runs `input` through `f` at the higher rate and writes the result to `output`. The two
    /// are as long as the shorter of them.
    pub fn process(&mut self, input: &[f32], output: &mut [f32], mut f: impl FnMut(&mut [f32])) {
        let frames = input.len().min(output.len());
        for (inp, out) in input[..frames]
            .chunks(self.max_block)
            .zip(output[..frames].chunks_mut(self.max_block))
        {
            if self.stages.is_empty() {
                out.copy_from_slice(inp);
                f(out);
                continue;
            }
            self.upsample(inp);
            let top = self.stages.len() - 1;
            let hi_frames = inp.len() << (top + 1);
            f(&mut self.buffers[top][..hi_frames]);
            self.downsample(out);
        }
    }

    /// Runs a block in place through `f`, as [`process`](Self::process) does.
    pub fn process_in_place(&mut self, block: &mut [f32], mut f: impl FnMut(&mut [f32])) {
        let frames = block.len();
        for chunk in block[..frames].chunks_mut(self.max_block) {
            if self.stages.is_empty() {
                f(chunk);
                continue;
            }
            self.upsample(chunk);
            let top = self.stages.len() - 1;
            f(&mut self.buffers[top][..chunk.len() << (top + 1)]);
            self.downsample(chunk);
        }
    }
}
