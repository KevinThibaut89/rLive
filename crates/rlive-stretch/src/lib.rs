//! Stretching audio to the timeline, and finding the transients that guide it.
//!
//! The engine asks a [`rlive_core::warp::Stretcher`] to render a range of the source into output
//! frames. This crate holds the real implementations: [`RePitch`] changes the playback rate,
//! [`Beats`] plays slices of the source against the timeline, [`Tones`] and [`Texture`] are
//! granular, and [`PhaseVocoder`] is Complex and Complex Pro. Nothing here allocates once a
//! stretcher is built, except [`detect_transients`] and [`track_beats`], which run off the audio
//! thread.

mod beat_track;
mod beats;
mod grain;
mod hiq;
mod interp;
mod pv;
mod repitch;
mod transients;

pub use beat_track::{track_beats, TRACKER_VERSION};
pub use beats::Beats;
pub use grain::{Granular, Texture, Tones};
pub use hiq::HiQ;
pub use pv::{Flavor, PhaseVocoder};
pub use repitch::RePitch;
pub use transients::{detect_transients, DETECTOR_VERSION};

pub use rlive_core::warp::BeatGrid;
use rlive_core::warp::{Passthrough, StretchBank, StretchKind, Stretcher};

/// How long a discontinuity is crossfaded, in seconds.
const CROSSFADE_SECS: f64 = 0.001;

fn crossfade_frames(sample_rate: f64) -> u32 {
    ((CROSSFADE_SECS * sample_rate).round() as u32).max(8)
}

/// How far from one source frame per output frame still counts as native speed.
const NATIVE_RATE: f64 = 1e-6;

/// Whether reading at `rate` is native speed: a clip at the Set's tempo plays back unstretched.
fn is_native_rate(rate: f64) -> bool {
    (rate - 1.0).abs() < NATIVE_RATE
}

/// A stereo frame of the source exactly as the file has it.
#[inline]
fn source_frame(s: &rlive_core::SampleBuffer, frame: f64) -> (f32, f32) {
    if frame < 0.0 {
        return (0.0, 0.0);
    }
    let at = frame as usize;
    match (s.left.get(at), s.right.get(at)) {
        (Some(l), Some(r)) => (*l, *r),
        _ => (0.0, 0.0),
    }
}

/// A stretcher for `kind`. Warp off is a passthrough.
pub fn stretcher(kind: StretchKind) -> Box<dyn Stretcher> {
    match kind {
        StretchKind::Beats => Box::new(Beats::new()),
        StretchKind::RePitch => Box::new(RePitch::new()),
        StretchKind::OffHiQ => Box::new(HiQ::new()),
        StretchKind::Tones => Box::new(Tones::new()),
        StretchKind::Texture => Box::new(Texture::new()),
        StretchKind::Complex => Box::new(PhaseVocoder::complex()),
        StretchKind::ComplexPro => Box::new(PhaseVocoder::complex_pro()),
        StretchKind::Off => Box::new(Passthrough::default()),
    }
}

/// A bank with a stretcher of every kind.
pub fn bank(max_block: usize) -> StretchBank {
    StretchBank::with_rows(max_block, stretcher)
}
