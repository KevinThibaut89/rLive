//! Built-in rLive devices and their registry.

pub mod devices;
pub mod dsp;
pub mod effects;
pub mod juno106;
pub mod sampler;
pub mod synth;

use rlive_core::{DeviceDesc, Processor, Status};
use std::path::Path;
use std::sync::Arc;

/// Which of Live's three device classes a device belongs to. In a chain they run in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRole {
    MidiEffect,
    Instrument,
    AudioEffect,
}

pub use rlive_core::SampleBuffer;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("could not read WAV file: {0}")]
    Wav(#[from] hound::Error),
    #[error("WAV file has no channels")]
    NoChannels,
}

/// Load a WAV file (8/16/24/32-bit int or 32-bit float), mono or stereo.
pub fn load_wav(path: &Path) -> Result<SampleBuffer, LoadError> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err(LoadError::NoChannels);
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    let frames = samples.len() / channels;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for f in samples.chunks_exact(channels) {
        left.push(f[0]);
        right.push(if channels > 1 { f[1] } else { f[0] });
    }
    Ok(SampleBuffer {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        left,
        right,
        sample_rate: spec.sample_rate as f64,
    })
}

/// Write a stereo 32-bit float WAV file.
pub fn write_wav(
    path: &Path,
    left: &[f32],
    right: &[f32],
    sample_rate: u32,
) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for (l, r) in left.iter().zip(right) {
        w.write_sample(*l)?;
        w.write_sample(*r)?;
    }
    w.finalize()
}

/// Extra data some devices need at construction time.
#[derive(Default, Clone)]
pub struct DeviceInit {
    /// Loaded samples, one per [`DeviceDesc::samples`] slot.
    pub samples: Vec<Option<Arc<SampleBuffer>>>,
    /// The device's own saved state, if it has any beyond its parameters.
    pub state: Option<String>,
}

/// A row of the device registry.
pub struct BuiltinInfo {
    /// The persisted `type_id`. Never changes once released.
    pub id: &'static str,
    pub name: &'static str,
    pub role: DeviceRole,
    /// Where the browser files it.
    pub category: &'static str,
    pub status: Status,
    pub desc: fn() -> DeviceDesc,
    pub create: fn(&DeviceInit) -> Box<dyn Processor>,
}

impl BuiltinInfo {
    /// Whether the browser offers this device. Stubs stay hidden until promoted.
    pub fn visible(&self) -> bool {
        self.status != Status::Stub
    }
}

/// Every built-in device, shown or hidden.
pub fn builtins() -> &'static [&'static BuiltinInfo] {
    devices::ALL
}

pub fn find(id: &str) -> Option<&'static BuiltinInfo> {
    builtins().iter().find(|b| b.id == id).copied()
}
