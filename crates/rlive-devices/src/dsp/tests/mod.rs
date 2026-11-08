//! Analytic checks of the DSP blocks. They read signals and compare them to numbers, rather
//! than to anyone's ears.

mod convolution;
mod delay;
mod envelope;
mod filter;
mod follower;
mod generators;
mod granular;
mod lfo;
mod lookahead;
mod modmatrix;
mod oversample;
mod pitch;
mod realtime;
mod sidechain;
mod spectrum;
mod stft;
mod voice;

const SR: f64 = 48_000.0;
