//! Real-time-safe DSP shared by the built-in devices.
//!
//! [`basics`] holds the original small blocks the first devices were built on; everything else
//! is grouped by what it does. See `dsp/README.md` for a tour with examples. The rules every
//! block follows: nothing on a `process` path allocates, locks or does unbounded work, and
//! buffers are made at construction or in `prepare`.

mod basics;

pub mod convolution;
pub mod delay;
pub mod envelope;
pub mod fft;
pub mod filter;
pub mod follower;
pub mod granular;
pub mod ir;
pub mod lfo;
pub mod lookahead;
pub mod modmatrix;
pub mod oversample;
pub mod pitch;
pub mod sidechain;
pub mod spectrum;
pub mod stft;
pub mod tempo;
pub mod util;
pub mod voice;
pub mod wavetable;

#[cfg(test)]
mod tests;

pub use basics::*;
