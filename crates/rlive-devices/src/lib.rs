//! Built-in rLive devices and their registry.

pub mod dsp;
pub mod effects;
pub mod sampler;
pub mod synth;

use rlive_core::{ParamInfo, Processor};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRole {
    Instrument,
    AudioEffect,
}

/// Decoded audio file, used by the sampler and audio clips.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleBuffer {
    pub name: String,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: f64,
}

impl SampleBuffer {
    pub fn len(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn duration_secs(&self) -> f64 {
        self.len() as f64 / self.sample_rate
    }
}

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
    pub sample: Option<Arc<SampleBuffer>>,
}

/// Static description of a built-in device.
pub struct BuiltinInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub role: DeviceRole,
    pub params: fn() -> Vec<ParamInfo>,
    pub create: fn(&DeviceInit) -> Box<dyn Processor>,
    /// Whether the device accepts a sample (shows a "load sample" button).
    pub uses_sample: bool,
}

static BUILTINS: &[BuiltinInfo] = &[
    BuiltinInfo {
        id: "rlive.poly",
        name: "Poly Synth",
        role: DeviceRole::Instrument,
        params: synth::params,
        create: |_| Box::new(synth::PolySynth::new()),
        uses_sample: false,
    },
    BuiltinInfo {
        id: "rlive.sampler",
        name: "Sampler",
        role: DeviceRole::Instrument,
        params: sampler::params,
        create: |init| Box::new(sampler::Sampler::new(init.sample.clone())),
        uses_sample: true,
    },
    BuiltinInfo {
        id: "rlive.eq3",
        name: "EQ Three",
        role: DeviceRole::AudioEffect,
        params: effects::eq_params,
        create: |_| Box::<effects::Eq3>::default(),
        uses_sample: false,
    },
    BuiltinInfo {
        id: "rlive.compressor",
        name: "Compressor",
        role: DeviceRole::AudioEffect,
        params: effects::compressor_params,
        create: |_| Box::<effects::Compressor>::default(),
        uses_sample: false,
    },
    BuiltinInfo {
        id: "rlive.saturator",
        name: "Saturator",
        role: DeviceRole::AudioEffect,
        params: effects::saturator_params,
        create: |_| Box::<effects::Saturator>::default(),
        uses_sample: false,
    },
    BuiltinInfo {
        id: "rlive.delay",
        name: "Delay",
        role: DeviceRole::AudioEffect,
        params: effects::delay_params,
        create: |_| Box::<effects::Delay>::default(),
        uses_sample: false,
    },
    BuiltinInfo {
        id: "rlive.reverb",
        name: "Reverb",
        role: DeviceRole::AudioEffect,
        params: effects::reverb_params,
        create: |_| Box::<effects::Reverb>::default(),
        uses_sample: false,
    },
    BuiltinInfo {
        id: "rlive.utility",
        name: "Utility",
        role: DeviceRole::AudioEffect,
        params: effects::utility_params,
        create: |_| Box::<effects::Utility>::default(),
        uses_sample: false,
    },
];

pub fn builtins() -> &'static [BuiltinInfo] {
    BUILTINS
}

pub fn find(id: &str) -> Option<&'static BuiltinInfo> {
    BUILTINS.iter().find(|b| b.id == id)
}
