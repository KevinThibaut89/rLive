//! Audio output: runs the engine inside the cpal (CoreAudio on macOS) callback.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rlive_engine::{create_engine, AudioIo, Engine, EngineHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Largest block the engine processes at once (cpal buffers are split into these).
pub const MAX_BLOCK: usize = 1024;
/// Most hardware output channels driven at once. More than any interface rLive expects to meet.
const MAX_OUTPUT_CHANNELS: usize = 64;
/// Frames handed to the engine in one call.
const SCRATCH_FRAMES: usize = 16_384;

pub struct AudioOutput {
    _stream: Option<cpal::Stream>,
    fallback: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    pub device_name: String,
    pub sample_rate: f64,
    /// How many hardware channels this stream writes.
    pub output_channels: usize,
    /// How many channels the system's input device has. Nothing is captured from them yet.
    pub input_channels: usize,
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
            output_channels: 2,
            input_channels: 0,
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
                    output_channels: 2,
                    input_channels: 0,
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
    let config = widest_config(&device, &supported);
    let channels = (config.channels as usize).min(MAX_OUTPUT_CHANNELS);
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
            output_channels: channels,
            input_channels: input_channel_count(&host),
            error: None,
        },
        handle,
    ))
}

/// The device's default rate, with as many channels as it offers there.
fn widest_config(
    device: &cpal::Device,
    default: &cpal::SupportedStreamConfig,
) -> cpal::StreamConfig {
    let rate = default.sample_rate();
    let widest = device.supported_output_configs().ok().and_then(|configs| {
        configs
            .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
            .filter(|c| c.min_sample_rate() <= rate && rate <= c.max_sample_rate())
            .max_by_key(|c| c.channels())
    });
    match widest {
        Some(range) if range.channels() > default.channels() => {
            range.with_sample_rate(rate).config()
        }
        _ => default.config(),
    }
}

/// The channels the system's input device offers, for the choosers. No stream is opened.
fn input_channel_count(host: &cpal::Host) -> usize {
    host.default_input_device()
        .and_then(|d| d.default_input_config().ok())
        .map_or(0, |c| c.channels() as usize)
}

/// Renders one callback's worth of interleaved audio. `scratch` holds a block per channel,
/// allocated up front; nothing here allocates. A mono device gets the Main output summed down.
fn render_interleaved(
    engine: &mut Engine,
    scratch: &mut [Vec<f32>],
    data: &mut [f32],
    channels: usize,
) {
    let wired = channels.max(2).min(scratch.len());
    let frames_total = data.len() / channels.max(1);
    let mut done = 0;
    while done < frames_total {
        let n = (frames_total - done).min(SCRATCH_FRAMES);
        {
            let mut buffers = scratch.iter_mut();
            let mut slices: [&mut [f32]; MAX_OUTPUT_CHANNELS] =
                std::array::from_fn(|_| match buffers.next() {
                    Some(b) => &mut b[..n],
                    None => &mut [],
                });
            engine.process(AudioIo {
                inputs: &[],
                outputs: &mut slices[..wired],
            });
        }
        let out = &mut data[done * channels..(done + n) * channels];
        for (i, frame) in out.chunks_mut(channels).enumerate() {
            match frame.len() {
                0 => {}
                1 => frame[0] = (scratch[0][i] + scratch[1][i]) * 0.5,
                _ => {
                    for (c, s) in frame.iter_mut().enumerate() {
                        *s = scratch.get(c).map_or(0.0, |b| b[i]);
                    }
                }
            }
        }
        done += n;
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut engine: Engine,
) -> Result<cpal::Stream, String> {
    // Scratch buffers allocated up front; the callback never allocates.
    let mut scratch = vec![vec![0.0f32; SCRATCH_FRAMES]; channels.max(2)];
    device
        .build_output_stream(
            *config,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                render_interleaved(&mut engine, &mut scratch, data, channels);
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
                engine.process_stereo(&mut l, &mut r);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> (Engine, EngineHandle) {
        create_engine(48_000.0, 256)
    }

    #[test]
    fn interleaved_channels_past_the_stereo_pair_are_silent_until_routed() {
        let (mut e, _h) = engine();
        let mut scratch = vec![vec![1.0f32; SCRATCH_FRAMES]; 6];
        let mut data = vec![0.5f32; 6 * 100];
        rlive_testkit::assert_no_alloc(|| render_interleaved(&mut e, &mut scratch, &mut data, 6));
        assert!(data.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_mono_device_gets_the_sum_of_the_main_pair() {
        let (mut e, _h) = engine();
        let mut scratch = vec![vec![0.0f32; SCRATCH_FRAMES]; 2];
        let mut data = vec![0.5f32; 100];
        render_interleaved(&mut e, &mut scratch, &mut data, 1);
        assert!(data.iter().all(|s| *s == 0.0));
    }
}
