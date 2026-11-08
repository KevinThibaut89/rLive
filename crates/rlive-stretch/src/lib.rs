//! Stretching audio to the timeline, and finding the transients that guide it.
//!
//! The engine asks a [`rlive_core::warp::Stretcher`] to render a range of the source into output
//! frames. This crate holds the real implementations: [`RePitch`] changes the playback rate and
//! [`Beats`] plays slices of the source against the timeline. Nothing here allocates once a
//! stretcher is built, except [`detect_transients`], which runs off the audio thread.

mod beats;
mod interp;
mod repitch;
mod transients;

pub use beats::Beats;
pub use repitch::RePitch;
pub use transients::{detect_transients, DETECTOR_VERSION};

use rlive_core::warp::{Passthrough, StretchBank, StretchKind, Stretcher};

/// How long a discontinuity is crossfaded, in seconds.
const CROSSFADE_SECS: f64 = 0.001;

fn crossfade_frames(sample_rate: f64) -> u32 {
    ((CROSSFADE_SECS * sample_rate).round() as u32).max(8)
}

/// A bank with the stretchers this crate has built; the other modes are still passthroughs.
pub fn bank(max_block: usize) -> StretchBank {
    StretchBank::with_rows(max_block, |kind| match kind {
        StretchKind::Beats => Box::new(Beats::new()) as Box<dyn Stretcher>,
        StretchKind::RePitch => Box::new(RePitch::new()),
        _ => Box::new(Passthrough::default()),
    })
}
