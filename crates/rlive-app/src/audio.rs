//! Audio output: runs the engine inside the cpal (CoreAudio on macOS) callback.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rlive_engine::{create_engine, Engine, EngineHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Largest block the engine processes at once (cpal buffers are split into these).
pub const MAX_BLOCK: usize = 1024;

pub struct AudioOutput {
    _stream: Option<cpal::Stream>,
    fallback: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    pub device_name: String,
    pub sample_rate: f64,
    pub error: Option<String>,
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        if let Some((stop, thread)) = self.fallback.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
        }
    }
}

impl AudioOutput {
    /// A placeholder with no stream (used while switching devices).
    pub fn stopped() -> Self {
        Self {
            _stream: None,
            fallback: None,
            device_name: "Stopped".into(),
            sample_rate: 48_000.0,
            error: None,
        }
    }
}

pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|devs| {
            devs.filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Open an output device (by name, or the default one) and start an engine on it.
/// Falls back to a silent timer-driven engine when no device is available so the app still runs.
pub fn start(device_name: Option<&str>) -> (AudioOutput, EngineHandle) {
    match try_start(device_name) {
        Ok(r) => r,
        Err(e) => {
            log::error!("audio output unavailable: {e}");
            let (engine, handle) = create_engine(48_000.0, MAX_BLOCK);
            let stop = Arc::new(AtomicBool::new(false));
            let thread = spawn_fallback(engine, stop.clone());
            (
                AudioOutput {
                    _stream: None,
                    fallback: Some((stop, thread)),
                    device_name: "No audio device".into(),
                    sample_rate: 48_000.0,
                    error: Some(e),
                },
                handle,
            )
        }
    }
}

fn try_start(device_name: Option<&str>) -> Result<(AudioOutput, EngineHandle), String> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.description().map(|x| x.name() == name).unwrap_or(false))
            .ok_or_else(|| format!("output device {name} not found"))?,
        None => host
            .default_output_device()
            .ok_or("no default output device")?,
    };
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Output".into());
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        log::warn!(
            "device prefers {:?}; requesting f32",
            supported.sample_format()
        );
    }
    let config = supported.config();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate as f64;
    let (engine, handle) = create_engine(sample_rate, MAX_BLOCK);
    let stream = build_stream(&device, &config, channels, engine)?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((
        AudioOutput {
            _stream: Some(stream),
            fallback: None,
            device_name: name,
            sample_rate,
            error: None,
        },
        handle,
    ))
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut engine: Engine,
) -> Result<cpal::Stream, String> {
    // Scratch buffers allocated up front; the callback never allocates.
    let mut left = vec![0.0f32; 16_384];
    let mut right = vec![0.0f32; 16_384];
    device
        .build_output_stream(
            *config,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                let frames_total = data.len() / channels.max(1);
                let mut done = 0;
                while done < frames_total {
                    let n = (frames_total - done).min(left.len());
                    engine.process(&mut left[..n], &mut right[..n]);
                    let out = &mut data[done * channels..(done + n) * channels];
                    for (i, frame) in out.chunks_mut(channels).enumerate() {
                        match frame.len() {
                            0 => {}
                            1 => frame[0] = (left[i] + right[i]) * 0.5,
                            _ => {
                                frame[0] = left[i];
                                frame[1] = right[i];
                                for s in frame.iter_mut().skip(2) {
                                    *s = 0.0;
                                }
                            }
                        }
                    }
                    done += n;
                }
            },
            |err| log::error!("audio stream error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// Drive the engine in real time without an audio device (output is discarded).
fn spawn_fallback(mut engine: Engine, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("rlive-null-audio".into())
        .spawn(move || {
            let block = 512;
            let mut l = vec![0.0; block];
            let mut r = vec![0.0; block];
            let period = std::time::Duration::from_secs_f64(block as f64 / engine.sample_rate());
            let mut next = std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) {
                engine.process(&mut l, &mut r);
                next += period;
                let now = std::time::Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        })
        .expect("spawn audio thread")
}
