//! Small helpers the DSP blocks share.

/// Zero for NaN, infinities and denormals, otherwise `x`. Put this on feedback paths.
#[inline]
pub fn sanitize(x: f32) -> f32 {
    if x.is_finite() && x.abs() > 1e-30 {
        x
    } else {
        0.0
    }
}

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Decibels of a linear gain, floored at -240 dB so silence stays finite.
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.abs().max(1e-12).log10()
}

/// Frequency in Hz of a (possibly fractional) MIDI note number, with A4 at 440 Hz.
#[inline]
pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Four-point Catmull-Rom interpolation; `t` is the position between `y1` and `y2`.
#[inline]
pub fn cubic(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

/// A small deterministic random generator (xorshift32): same seed, same sequence.
#[derive(Clone, Copy, Debug)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        // Xorshift has a fixed point at zero, and small seeds start out poorly mixed.
        let mixed = seed.wrapping_mul(0x9E37_79B9) ^ 0x6D2B_79F5;
        Self(if mixed == 0 { 1 } else { mixed })
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in `0.0..1.0`.
    #[inline]
    pub fn unipolar(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Uniform in `-1.0..1.0`.
    #[inline]
    pub fn bipolar(&mut self) -> f32 {
        self.unipolar() * 2.0 - 1.0
    }
}
