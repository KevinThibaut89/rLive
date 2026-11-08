//! Audio I/O: runs the engine inside the cpal (CoreAudio on macOS) output callback, feeding it
//! from an input stream through a ring that absorbs the two devices' clock drift.

use crate::settings::InputDevice;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rlive_engine::{create_engine, AudioIo, Engine, EngineHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Largest block the engine processes at once (cpal buffers are split into these).
pub const MAX_BLOCK: usize = 1024;
/// Most hardware output channels driven at once. More than any interface rLive expects to meet.
const MAX_OUTPUT_CHANNELS: usize = 64;
/// Frames handed to the engine in one call.
const SCRATCH_FRAMES: usize = 16_384;
/// Most hardware input channels read at once.
const MAX_INPUT_CHANNELS: usize = 32;

pub struct AudioOutput {
    _stream: Option<cpal::Stream>,
    _input: Option<cpal::Stream>,
    fallback: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    pub device_name: String,
    pub sample_rate: f64,
    /// How many hardware channels this stream writes.
    pub output_channels: usize,
    /// How many channels the input stream has. Zero when there isn't one.
    pub input_channels: usize,
    /// The input stream's device.
    pub input_name: Option<String>,
    /// Frames lost because the input and output drifted further apart than the ring could hide.
    pub input_glitches: Arc<AtomicU64>,
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
            _input: None,
            fallback: None,
            device_name: "Stopped".into(),
            sample_rate: 48_000.0,
            output_channels: 2,
            input_channels: 0,
            input_name: None,
            input_glitches: Arc::new(AtomicU64::new(0)),
            error: None,
        }
    }
}

pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devs| {
            devs.filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
                .collect()
        })
        .unwrap_or_default()
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
pub fn start(device_name: Option<&str>, input: &InputDevice) -> (AudioOutput, EngineHandle) {
    match try_start(device_name, input) {
        Ok(r) => r,
        Err(e) => {
            log::error!("audio output unavailable: {e}");
            let (engine, handle) = create_engine(48_000.0, MAX_BLOCK);
            let stop = Arc::new(AtomicBool::new(false));
            let thread = spawn_fallback(engine, stop.clone());
            (
                AudioOutput {
                    _stream: None,
                    _input: None,
                    fallback: Some((stop, thread)),
                    device_name: "No audio device".into(),
                    sample_rate: 48_000.0,
                    output_channels: 2,
                    input_channels: 0,
                    input_name: None,
                    input_glitches: Arc::new(AtomicU64::new(0)),
                    error: Some(e),
                },
                handle,
            )
        }
    }
}

fn try_start(
    device_name: Option<&str>,
    input: &InputDevice,
) -> Result<(AudioOutput, EngineHandle), String> {
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
    let glitches = Arc::new(AtomicU64::new(0));
    let opened = match open_input(&host, input, config.sample_rate, glitches.clone()) {
        Ok(opened) => opened,
        Err(e) => {
            log::warn!("audio input unavailable: {e}");
            None
        }
    };
    let (input_stream, tail, input_channels, input_name) = match opened {
        Some(o) => (Some(o.stream), Some(o.tail), o.channels, Some(o.name)),
        None => (None, None, 0, None),
    };
    let stream = build_stream(
        &device,
        &config,
        channels,
        engine,
        tail,
        handle.shared.clone(),
    )?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((
        AudioOutput {
            _stream: Some(stream),
            _input: input_stream,
            fallback: None,
            device_name: name,
            sample_rate,
            output_channels: channels,
            input_channels,
            input_name,
            input_glitches: glitches,
            error: None,
        },
        handle,
    ))
}

/// An open input stream and the output side's end of its ring.
struct OpenInput {
    stream: cpal::Stream,
    tail: DriftRing,
    channels: usize,
    name: String,
}

/// Opens the chosen input device at the output's sample rate. `None` is no input at all: nothing
/// is opened, so a machine that has never used one is never asked for the microphone.
fn open_input(
    host: &cpal::Host,
    choice: &InputDevice,
    sample_rate: cpal::SampleRate,
    glitches: Arc<AtomicU64>,
) -> Result<Option<OpenInput>, String> {
    let device = match choice {
        InputDevice::None => return Ok(None),
        InputDevice::SystemDefault => host.default_input_device(),
        InputDevice::Named(name) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.description().map(|x| x.name() == name).unwrap_or(false)),
    }
    .ok_or("input device not found")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Input".into());
    let config = device
        .supported_input_configs()
        .map_err(|e| e.to_string())?
        .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
        .filter(|c| c.min_sample_rate() <= sample_rate && sample_rate <= c.max_sample_rate())
        .max_by_key(|c| c.channels())
        .map(|c| c.with_sample_rate(sample_rate).config())
        .ok_or("the input device does not run at the output's sample rate")?;
    let channels = (config.channels as usize).clamp(1, MAX_INPUT_CHANNELS);
    let (mut head, tail) = drift_ring(channels, config.sample_rate as usize, glitches);
    let latency = tail.latency.clone();
    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], info: &cpal::InputCallbackInfo| {
                let timestamp = info.timestamp();
                let lag = timestamp
                    .callback
                    .duration_since(timestamp.capture)
                    .as_nanos() as u64;
                latency.store(lag, Ordering::Relaxed);
                head.push(data);
            },
            |err| log::error!("audio input error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Some(OpenInput {
        stream,
        tail,
        channels,
        name,
    }))
}

/// The writing end of the input ring.
struct DriftHead {
    ring: rtrb::Producer<f32>,
}

impl DriftHead {
    fn push(&mut self, samples: &[f32]) {
        for s in samples {
            if self.ring.push(*s).is_err() {
                return;
            }
        }
    }
}

/// The reading end of the input ring. It holds a few blocks in reserve and gently skips or repeats
/// a frame now and again to stay there, so two devices on separate clocks can run forever.
struct DriftRing {
    ring: rtrb::Consumer<f32>,
    channels: usize,
    /// Frames held back to ride out jitter between the two callbacks.
    target: usize,
    /// How far between corrections, so a correction isn't audible.
    since_correction: usize,
    glitches: Arc<AtomicU64>,
    /// How old the latest input frame was on arrival, in nanoseconds.
    latency: Arc<AtomicU64>,
    /// Nothing read yet: wait for the reserve to build up.
    primed: bool,
}

/// Build the two ends of an input ring. `capacity` is in frames.
fn drift_ring(
    channels: usize,
    capacity: usize,
    glitches: Arc<AtomicU64>,
) -> (DriftHead, DriftRing) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity.max(1) * channels.max(1));
    (
        DriftHead { ring: producer },
        DriftRing {
            ring: consumer,
            channels: channels.max(1),
            target: DRIFT_TARGET_FRAMES,
            since_correction: 0,
            glitches,
            latency: Arc::new(AtomicU64::new(0)),
            primed: false,
        },
    )
}

/// Frames of reserve the ring keeps. A few callbacks' worth at the sizes hardware runs at.
const DRIFT_TARGET_FRAMES: usize = 1024;
/// A correction every this many frames at most; a tenth of a percent of drift, far below hearing.
const DRIFT_CORRECT_EVERY: usize = 2048;

impl DriftRing {
    fn frames_held(&self) -> usize {
        self.ring.slots() / self.channels
    }

    /// Reads `n` frames into `out`, one buffer per channel, resampling by a whole frame only when
    /// the reserve has drifted. Reads silence until the reserve exists.
    fn pull(&mut self, n: usize, out: &mut [Vec<f32>]) {
        let held = self.frames_held();
        if !self.primed {
            if held < self.target {
                out.iter_mut().for_each(|b| b[..n].fill(0.0));
                return;
            }
            self.primed = true;
        }
        if held < n {
            self.glitches
                .fetch_add((n - held) as u64, Ordering::Relaxed);
            self.primed = false;
            out.iter_mut().for_each(|b| b[..n].fill(0.0));
            return;
        }
        let mut read = n;
        self.since_correction += n;
        if self.since_correction >= DRIFT_CORRECT_EVERY {
            if held > self.target + n * 2 {
                // Ahead of plan: drop one frame.
                read = n + 1;
                self.since_correction = 0;
            } else if held + n < self.target && held > n {
                // Falling behind: repeat one.
                read = n - 1;
                self.since_correction = 0;
            }
        }
        let wanted = read.min(held) * self.channels;
        let Ok(chunk) = self.ring.read_chunk(wanted) else {
            out.iter_mut().for_each(|b| b[..n].fill(0.0));
            return;
        };
        let (a, b) = chunk.as_slices();
        let mut frame_at = 0;
        let mut channel = 0;
        for sample in a.iter().chain(b) {
            if let Some(out) = out.get_mut(channel) {
                if frame_at < n {
                    out[frame_at] = *sample;
                }
            }
            channel += 1;
            if channel == self.channels {
                channel = 0;
                frame_at += 1;
            }
        }
        chunk.commit_all();
        // A repeated frame fills the one short; a dropped one was simply never written.
        for buffer in out.iter_mut() {
            for i in read..n {
                buffer[i] = buffer[read.saturating_sub(1)];
            }
        }
    }
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

/// Renders one callback's worth of interleaved audio. `scratch` holds a block per channel,
/// allocated up front; nothing here allocates. A mono device gets the Main output summed down.
fn render_interleaved(
    engine: &mut Engine,
    scratch: &mut [Vec<f32>],
    input: Option<&mut InputSide>,
    data: &mut [f32],
    channels: usize,
) {
    let wired = channels.max(2).min(scratch.len());
    let frames_total = data.len() / channels.max(1);
    let mut input = input;
    let mut done = 0;
    while done < frames_total {
        let n = (frames_total - done).min(SCRATCH_FRAMES);
        let empty: &[f32] = &[];
        let mut in_slices: [&[f32]; MAX_INPUT_CHANNELS] = [empty; MAX_INPUT_CHANNELS];
        let mut in_channels = 0;
        if let Some(side) = input.as_mut() {
            side.ring.pull(n, &mut side.scratch);
            in_channels = side.scratch.len().min(MAX_INPUT_CHANNELS);
            for (slice, buffer) in in_slices.iter_mut().zip(side.scratch.iter()) {
                *slice = &buffer[..n];
            }
        }
        {
            let mut buffers = scratch.iter_mut();
            let mut slices: [&mut [f32]; MAX_OUTPUT_CHANNELS] =
                std::array::from_fn(|_| match buffers.next() {
                    Some(b) => &mut b[..n],
                    None => &mut [],
                });
            engine.process(AudioIo {
                inputs: &in_slices[..in_channels],
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

/// The output callback's view of the input stream.
struct InputSide {
    ring: DriftRing,
    scratch: Vec<Vec<f32>>,
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut engine: Engine,
    input: Option<DriftRing>,
    shared: Arc<rlive_engine::shared::EngineShared>,
) -> Result<cpal::Stream, String> {
    // Scratch buffers allocated up front; the callback never allocates.
    let mut scratch = vec![vec![0.0f32; SCRATCH_FRAMES]; channels.max(2)];
    let mut input = input.map(|ring| InputSide {
        scratch: vec![vec![0.0f32; SCRATCH_FRAMES]; ring.channels],
        ring,
    });
    let sample_rate = config.sample_rate as f64;
    device
        .build_output_stream(
            *config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                let timestamp = info.timestamp();
                let ahead = timestamp.playback.duration_since(timestamp.callback);
                let frames = (ahead.as_secs_f64() * sample_rate) as u32;
                shared.output_latency.store(frames, Ordering::Relaxed);
                if let Some(side) = &input {
                    let lag = side.ring.latency.load(Ordering::Relaxed);
                    let frames = (lag as f64 * 1e-9 * sample_rate) as u32;
                    shared.input_latency.store(frames, Ordering::Relaxed);
                }
                render_interleaved(&mut engine, &mut scratch, input.as_mut(), data, channels);
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
        rlive_testkit::assert_no_alloc(|| {
            render_interleaved(&mut e, &mut scratch, None, &mut data, 6)
        });
        assert!(data.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_mono_device_gets_the_sum_of_the_main_pair() {
        let (mut e, _h) = engine();
        let mut scratch = vec![vec![0.0f32; SCRATCH_FRAMES]; 2];
        let mut data = vec![0.5f32; 100];
        render_interleaved(&mut e, &mut scratch, None, &mut data, 1);
        assert!(data.iter().all(|s| *s == 0.0));
    }

    /// `(head, tail)` of a mono ring holding 512 frames of a known ramp.
    fn ring_with(frames: usize) -> (DriftHead, DriftRing) {
        let (mut head, mut tail) = drift_ring(1, 16_384, Arc::new(AtomicU64::new(0)));
        tail.target = 256;
        head.push(&(0..frames).map(|i| i as f32).collect::<Vec<_>>());
        (head, tail)
    }

    #[test]
    fn the_input_ring_waits_for_a_reserve_then_hands_frames_over_in_order() {
        let (_head, mut tail) = ring_with(100);
        let mut out = vec![vec![0.0f32; 64]];
        tail.pull(32, &mut out);
        assert_eq!(out[0][0], 0.0);
        let (mut head, mut tail) = ring_with(1024);
        tail.pull(32, &mut out);
        assert_eq!(out[0][..3], [0.0, 1.0, 2.0]);
        head.push(&[5000.0]);
        tail.pull(32, &mut out);
        assert_eq!(out[0][..3], [32.0, 33.0, 34.0]);
    }

    #[test]
    fn a_ring_that_runs_ahead_or_behind_corrects_a_frame_at_a_time() {
        // Clocks that agree, with a backlog above the reserve: the ring works it off.
        let (mut head, mut tail) = ring_with(4096);
        let mut out = vec![vec![0.0f32; 256]];
        for _ in 0..200 {
            head.push(&vec![0.0; 256]);
            tail.pull(256, &mut out);
        }
        let held = tail.frames_held();
        assert!(held < 4096 - 10, "the ring reins in its backlog: {held}");
        assert_eq!(tail.glitches.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn running_dry_counts_as_a_glitch_and_reads_silence() {
        let (_head, mut tail) = ring_with(1024);
        let mut out = vec![vec![1.0f32; 2048]];
        tail.pull(1024, &mut out);
        tail.pull(2048, &mut out);
        assert!(out[0].iter().all(|s| *s == 0.0));
        assert_eq!(tail.glitches.load(Ordering::Relaxed), 2048);
    }

    #[test]
    fn pulling_does_not_allocate() {
        let (_head, mut tail) = ring_with(4096);
        let mut out = vec![vec![0.0f32; 512]];
        rlive_testkit::assert_no_alloc(|| tail.pull(256, &mut out));
    }

    #[test]
    fn the_callback_with_an_input_does_not_allocate() {
        let (mut e, _h) = engine();
        let mut scratch = vec![vec![0.0f32; SCRATCH_FRAMES]; 2];
        let (_head, ring) = ring_with(8000);
        let mut side = InputSide {
            scratch: vec![vec![0.0f32; SCRATCH_FRAMES]],
            ring,
        };
        let mut data = vec![0.0f32; 2 * 100];
        rlive_testkit::assert_no_alloc(|| {
            render_interleaved(&mut e, &mut scratch, Some(&mut side), &mut data, 2)
        });
    }
}
