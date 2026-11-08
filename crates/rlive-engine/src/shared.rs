//! Lock-free state the audio thread publishes for the UI (meters, playhead, clip states).

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

/// `f32` stored in an `AtomicU32`.
#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
    /// Read and reset to zero (used for peak meters).
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
    /// Store `v` if it is larger than the current value.
    pub fn store_max(&self, v: f32) {
        if v > self.load() {
            self.store(v);
        }
    }
}

/// `f64` stored in an `AtomicU64`.
#[derive(Default)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub fn new(v: f64) -> Self {
        Self(AtomicU64::new(v.to_bits()))
    }
    pub fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// Stereo peak meter.
#[derive(Default)]
pub struct PeakMeter {
    pub left: AtomicF32,
    pub right: AtomicF32,
}

impl PeakMeter {
    pub fn update(&self, left: &[f32], right: &[f32]) {
        let pl = left.iter().fold(0f32, |m, s| m.max(s.abs()));
        let pr = right.iter().fold(0f32, |m, s| m.max(s.abs()));
        self.left.store_max(pl);
        self.right.store_max(pr);
    }

    /// Returns `(left, right)` peaks since the last call.
    pub fn take(&self) -> (f32, f32) {
        (self.left.take(), self.right.take())
    }
}

pub const NO_SLOT: i32 = -1;
/// `queued_slot` value meaning "a stop is queued".
pub const STOP_QUEUED: i32 = -2;

/// What a rack chain tells the UI: its output level, and whether notes are going in (for Auto
/// Select, manual p. 496).
#[derive(Default)]
pub struct ChainShared {
    pub meter: PeakMeter,
    /// The chain is holding at least one note.
    pub held: AtomicBool,
    /// Counts the notes let in, so a note shorter than a UI frame still shows.
    pub pulse: AtomicU32,
}

/// Per-track state visible to the UI.
pub struct TrackShared {
    pub meter: PeakMeter,
    /// Slot index of the playing clip, or [`NO_SLOT`].
    pub playing_slot: AtomicI32,
    /// Slot index waiting for its launch point, [`STOP_QUEUED`], or [`NO_SLOT`].
    pub queued_slot: AtomicI32,
    /// Playhead position within the playing clip, in ticks.
    pub clip_position: AtomicF64,
    /// How long the playing clip has run from its position 0, in ticks, loops included.
    pub clip_elapsed: AtomicF64,
    /// Whether the track plays its Arrangement lane rather than a launched Session clip.
    pub follow_arrangement: AtomicBool,
    /// The Session slot being recorded into, or [`NO_SLOT`].
    pub recording_slot: AtomicI32,
    /// Waiting to start recording: the slot, or [`NO_SLOT`].
    pub record_queued_slot: AtomicI32,
    /// Notes played on this track are going into its playing clip.
    pub overdubbing: AtomicBool,
}

impl Default for TrackShared {
    fn default() -> Self {
        Self {
            meter: PeakMeter::default(),
            playing_slot: AtomicI32::new(NO_SLOT),
            queued_slot: AtomicI32::new(NO_SLOT),
            clip_position: AtomicF64::new(0.0),
            clip_elapsed: AtomicF64::new(0.0),
            follow_arrangement: AtomicBool::new(true),
            recording_slot: AtomicI32::new(NO_SLOT),
            record_queued_slot: AtomicI32::new(NO_SLOT),
            overdubbing: AtomicBool::new(false),
        }
    }
}

/// Global engine state visible to the UI.
pub struct EngineShared {
    pub playing: AtomicBool,
    pub tick: AtomicF64,
    /// Where playback starts and returns to on stop, in ticks.
    pub insert_marker: AtomicF64,
    pub tempo: AtomicF64,
    /// DSP load of the last blocks, 0..1 (1 = using the whole buffer duration).
    pub cpu_load: AtomicF32,
    pub master_meter: PeakMeter,
    pub sample_rate: AtomicF64,
    /// Ticks left of the count-in; 0 when there is none.
    pub count_in_left: AtomicF64,
    /// Frames between the input converter and the block that receives them.
    pub input_latency: AtomicU32,
    /// Frames between a block and the output converter.
    pub output_latency: AtomicU32,
    /// Peak of each hardware input since the UI last looked. Index by channel, up to 64.
    pub input_meters: [AtomicF32; 64],
}

impl Default for EngineShared {
    fn default() -> Self {
        Self {
            playing: AtomicBool::new(false),
            tick: AtomicF64::new(0.0),
            insert_marker: AtomicF64::new(0.0),
            tempo: AtomicF64::new(120.0),
            cpu_load: AtomicF32::new(0.0),
            master_meter: PeakMeter::default(),
            sample_rate: AtomicF64::new(48_000.0),
            count_in_left: AtomicF64::new(0.0),
            input_latency: AtomicU32::new(0),
            output_latency: AtomicU32::new(0),
            input_meters: std::array::from_fn(|_| AtomicF32::new(0.0)),
        }
    }
}
