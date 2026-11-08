//! Interpolated and smeared delay lines.

use super::util::{cubic, sanitize};

/// How a read between samples is resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Interpolation {
    /// Fast, darkens the signal slightly at fractional delays.
    Linear,
    /// Four-point Catmull-Rom. Use this for modulated delays and repitching.
    #[default]
    Cubic,
}

/// A delay line with fractional reads. Capacity is fixed at construction.
#[derive(Clone, Debug)]
pub struct InterpDelay {
    buf: Vec<f32>,
    mask: usize,
    write: usize,
}

impl InterpDelay {
    /// Room for delays up to `max_delay` samples.
    pub fn new(max_delay: usize) -> Self {
        let size = (max_delay + 8).next_power_of_two();
        Self {
            buf: vec![0.0; size],
            mask: size - 1,
            write: 0,
        }
    }

    /// The longest delay that can be read.
    pub fn capacity(&self) -> usize {
        self.buf.len() - 8
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        self.write = 0;
    }

    /// Writes the next sample.
    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buf[self.write & self.mask] = sanitize(x);
        self.write = self.write.wrapping_add(1) & self.mask;
    }

    #[inline]
    fn at(&self, delay: usize) -> f32 {
        // One behind the write position is the sample just written.
        self.buf[self.write.wrapping_sub(delay + 1) & self.mask]
    }

    /// Reads `delay` samples behind the sample most recently written.
    #[inline]
    pub fn read(&self, delay: f32, interpolation: Interpolation) -> f32 {
        let delay = delay.max(0.0).min(self.capacity() as f32);
        let whole = delay as usize;
        let t = delay - whole as f32;
        match interpolation {
            Interpolation::Linear => {
                let a = self.at(whole);
                a + (self.at(whole + 1) - a) * t
            }
            Interpolation::Cubic => {
                // A read within a sample of the write head has no newer neighbour to ask.
                let newer = if whole == 0 {
                    self.at(0)
                } else {
                    self.at(whole - 1)
                };
                cubic(
                    newer,
                    self.at(whole),
                    self.at(whole + 1),
                    self.at(whole + 2),
                    t,
                )
            }
        }
    }
}

const SMEAR_TAPS: usize = 9;

/// A delay whose output is spread out in time, for feedback paths that dissolve rather than
/// repeat. With `smear` at zero it's an exact delay.
///
/// A fixed bell of taps around the delay time, with weights that sum to one, so the level of a
/// steady signal doesn't change with the smear.
#[derive(Clone, Debug)]
pub struct SmearDelay {
    line: InterpDelay,
    weights: [f32; SMEAR_TAPS],
    offsets: [f32; SMEAR_TAPS],
    smear: f32,
}

impl SmearDelay {
    /// Room for a delay of `max_delay` samples plus a smear of `max_smear` samples.
    pub fn new(max_delay: usize, max_smear: usize) -> Self {
        let mut weights = [0.0; SMEAR_TAPS];
        let mut offsets = [0.0; SMEAR_TAPS];
        let center = (SMEAR_TAPS - 1) as f32 / 2.0;
        for i in 0..SMEAR_TAPS {
            let x = (i as f32 - center) / center;
            offsets[i] = x;
            weights[i] = (-3.0 * x * x).exp();
        }
        let sum: f32 = weights.iter().sum();
        for w in &mut weights {
            *w /= sum;
        }
        Self {
            line: InterpDelay::new(max_delay + max_smear + 8),
            weights,
            offsets,
            smear: 0.0,
        }
    }

    /// The width of the spread, in samples.
    pub fn set_smear(&mut self, samples: f32) {
        self.smear = samples.max(0.0);
    }

    pub fn clear(&mut self) {
        self.line.clear();
    }

    /// Writes the next sample.
    #[inline]
    pub fn write(&mut self, x: f32) {
        self.line.write(x);
    }

    /// Reads the smeared delay centred `delay` samples behind the sample most recently written.
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        if self.smear < 1e-3 {
            return self.line.read(delay, Interpolation::Cubic);
        }
        let half = self.smear * 0.5;
        let mut out = 0.0;
        for (w, o) in self.weights.iter().zip(&self.offsets) {
            out += w * self
                .line
                .read((delay + o * half).max(0.0), Interpolation::Linear);
        }
        out
    }
}
