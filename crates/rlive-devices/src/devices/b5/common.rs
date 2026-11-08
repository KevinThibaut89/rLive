//! What the distortion devices share: a DC blocker, an exact delay for dry paths, and the
//! Hi-Quality oversampling factor.

use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::oversample::OversampleFactor;
use std::f32::consts::TAU;

pub use crate::devices::b4::common::{Ramp, CONTROL_FRAMES};

/// How far Hi-Quality raises the sample rate. Ours; the manual only says it reduces aliasing.
pub const HI_QUALITY_FACTOR: OversampleFactor = OversampleFactor::X4;

/// A one-pole DC blocker.
#[derive(Clone, Copy, Debug)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
    r: f32,
}

impl Default for DcBlock {
    fn default() -> Self {
        Self {
            x1: 0.0,
            y1: 0.0,
            r: 0.9995,
        }
    }
}

impl DcBlock {
    pub fn set(&mut self, hz: f32, sample_rate: f32) {
        self.r = (-TAU * hz / sample_rate.max(1.0)).exp();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = crate::dsp::util::sanitize(y);
        self.y1
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

/// An exact delay of a stereo pair, for keeping the dry signal in step with an oversampled wet.
#[derive(Clone, Debug)]
pub struct DryLine {
    left: InterpDelay,
    right: InterpDelay,
    len: usize,
}

impl DryLine {
    pub fn new(len: usize) -> Self {
        Self {
            left: InterpDelay::new(len + 1),
            right: InterpDelay::new(len + 1),
            len,
        }
    }

    #[inline]
    pub fn left(&mut self, x: f32) -> f32 {
        self.left.write(x);
        self.left.read(self.len as f32, Interpolation::Linear)
    }

    #[inline]
    pub fn right(&mut self, x: f32) -> f32 {
        self.right.write(x);
        self.right.read(self.len as f32, Interpolation::Linear)
    }

    pub fn clear(&mut self) {
        self.left.clear();
        self.right.clear();
    }
}

/// Mid and side of a stereo pair, and back.
#[inline]
pub fn encode_ms(l: f32, r: f32) -> (f32, f32) {
    (0.5 * (l + r), 0.5 * (l - r))
}

#[inline]
pub fn decode_ms(m: f32, s: f32) -> (f32, f32) {
    (m + s, m - s)
}
