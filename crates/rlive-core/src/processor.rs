use crate::{MidiEvent, TransportInfo};

/// Stereo audio processed in place.
pub struct AudioBuffer<'a> {
    pub left: &'a mut [f32],
    pub right: &'a mut [f32],
}

impl AudioBuffer<'_> {
    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn clear(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
    }
}

/// Fixed-capacity queue a processor uses to report parameter changes it made itself
/// (e.g. a plugin whose own editor moved a knob). Never allocates after construction.
pub struct ParamOutQueue {
    items: Vec<(u32, f64)>,
}

impl ParamOutQueue {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            items: Vec::with_capacity(cap),
        }
    }

    /// Push a change; silently dropped if the queue is full.
    pub fn push(&mut self, id: u32, value: f64) {
        if self.items.len() < self.items.capacity() {
            self.items.push((id, value));
        }
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (u32, f64)> + '_ {
        self.items.drain(..)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Everything a processor needs for one block.
pub struct ProcessContext<'a> {
    /// Audio is processed in place. Instruments *add* their output to it; effects transform it.
    pub audio: AudioBuffer<'a>,
    /// MIDI events for this block, sorted by offset.
    pub events: &'a [MidiEvent],
    pub transport: &'a TransportInfo,
    pub param_out: &'a mut ParamOutQueue,
}

/// A real-time audio processor: built-in device or hosted plugin.
///
/// Lifecycle: created and [`activate`](Processor::activate)d on the UI thread, then moved to the
/// audio thread where `process`, `set_param` and `stop_processing` are called. It is moved back to
/// the UI thread before being dropped, so `Drop` may block or deallocate freely.
pub trait Processor: Send {
    /// Prepare for processing. Called on the UI/main thread before the processor is sent
    /// to the audio thread. Allocate all buffers here.
    fn activate(&mut self, sample_rate: f64, max_block: usize);

    /// Process one block in place. Must not allocate, lock or block.
    fn process(&mut self, ctx: &mut ProcessContext);

    /// Set a parameter (plain value). Called on the audio thread.
    fn set_param(&mut self, id: u32, value: f64);

    /// Called on the audio thread right before the processor leaves the engine.
    fn stop_processing(&mut self) {}

    /// Silence tails / release voices (e.g. after transport stop).
    fn reset(&mut self) {}

    /// Processing latency in samples.
    fn latency(&self) -> u32 {
        0
    }
}
