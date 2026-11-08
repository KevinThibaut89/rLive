//! Reading audio files: decoders, the background sample loader, analysis files, the decoding cache
//! and waveform peaks.
//!
//! Nothing in this crate runs on the audio thread, and decoding never runs on the UI thread: a
//! [`SampleCache`] hands the work to its own threads and reports back through
//! [`SampleCache::poll`].

pub mod analysis;
pub mod decode;
pub mod decoding_cache;
pub mod fingerprint;
pub mod peaks;
pub mod sample_cache;
pub mod writer;

pub use analysis::{Analysis, AnalysisExtras};
pub use decode::{probe, AudioInfo, DecodeError, DecodedAudio, Decoder};
pub use decoding_cache::{CacheConfig, Cleanup, DecodingCache, Stored};
pub use fingerprint::{fingerprint, Fingerprint};
pub use peaks::Peaks;
pub use sample_cache::{LoaderConfig, SampleCache, SampleEvent, SampleStatus};
pub use writer::{AudioWriter, BitDepth, FileType};

/// The file extensions the decoders are built for.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "aif", "aiff", "aifc", "flac", "ogg", "oga", "mp3", "m4a", "mp4", "aac", "caf",
];
