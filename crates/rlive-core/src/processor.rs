use crate::view::ViewWriter;
use crate::{MidiEvent, MidiMessage, ParamEvent, TransportInfo};
use std::any::Any;
use std::sync::Arc;

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

/// Stereo audio a processor may only read, such as a sidechain.
#[derive(Clone, Copy)]
pub struct AudioInput<'a> {
    pub left: &'a [f32],
    pub right: &'a [f32],
}

impl AudioInput<'_> {
    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }
}

/// Stereo audio a processor writes, such as an extra output bus. Starts out silent.
pub struct AudioOutput<'a> {
    pub left: &'a mut [f32],
    pub right: &'a mut [f32],
}

impl AudioOutput<'_> {
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

/// MIDI a device hands to the next device in its chain. Never allocates after construction.
pub struct EventQueue {
    items: Vec<MidiEvent>,
}

impl EventQueue {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            items: Vec::with_capacity(cap),
        }
    }

    /// Push an event; silently dropped if the queue is full.
    pub fn push(&mut self, event: MidiEvent) {
        if self.items.len() < self.items.capacity() {
            self.items.push(event);
        }
    }

    /// Push as many of `events` as there is room for.
    pub fn extend(&mut self, events: &[MidiEvent]) {
        let room = self.items.capacity() - self.items.len();
        self.items
            .extend_from_slice(&events[..events.len().min(room)]);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Put the events in playback order. Does not allocate.
    pub fn sort(&mut self) {
        self.items.sort_unstable_by_key(|e| e.sort_key());
    }

    pub fn as_slice(&self) -> &[MidiEvent] {
        &self.items
    }

    pub fn as_mut_slice(&mut self) -> &mut [MidiEvent] {
        &mut self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// One step of a block being walked with [`ProcessContext::for_each_step`].
pub enum BlockStep<'s> {
    /// Render this stretch of the block. Already trimmed to the frames due.
    Audio(AudioBuffer<'s>),
    /// A parameter changes here, in plain units. Applies to the frames after it.
    Param { id: u32, value: f64 },
    /// A MIDI message lands here. Applies to the frames after it.
    Midi(MidiMessage),
}

/// Everything a processor needs for one block.
pub struct ProcessContext<'a> {
    /// Audio is processed in place. Instruments *add* their output to it; effects transform it.
    pub audio: AudioBuffer<'a>,
    /// The sidechain bus, when one is routed to this device.
    pub sidechain: Option<AudioInput<'a>>,
    /// Extra output buses, as many as the device declared and are routed.
    pub aux_out: &'a mut [AudioOutput<'a>],
    /// MIDI events for this block, sorted by offset.
    pub events: &'a [MidiEvent],
    /// Where a [``MidiEffect` role device writes its output. It replaces `events`
    /// for the next device. Other devices must leave it alone.
    pub events_out: &'a mut EventQueue,
    /// Parameter changes for this block, sorted by offset. The only real-time parameter path.
    pub params: &'a [ParamEvent],
    pub transport: &'a TransportInfo,
    pub param_out: &'a mut ParamOutQueue,
    /// The device's modulation sources, one value each, updated once per process call.
    pub mod_out: &'a mut [f32],
    /// Publishes data to the device's UI. Does nothing while no UI is reading.
    pub view: &'a ViewWriter<'a>,
}

impl ProcessContext<'_> {
    /// Walks the block in order, splitting the audio at each parameter change and MIDI event so
    /// `f` hears them at the right frame. A parameter change comes before MIDI at the same frame.
    /// Events past the end of the block are delivered after the last audio.
    pub fn for_each_step(&mut self, mut f: impl FnMut(BlockStep<'_>)) {
        let frames = self.audio.frames();
        let (mut next_param, mut next_midi, mut pos) = (0, 0, 0);
        loop {
            let param_at = self
                .params
                .get(next_param)
                .map(|e| (e.offset as usize).min(frames));
            let midi_at = self
                .events
                .get(next_midi)
                .map(|e| (e.offset as usize).min(frames));
            let (at, is_param) = match (param_at, midi_at) {
                (None, None) => break,
                (Some(p), Some(m)) if p <= m => (p, true),
                (Some(p), None) => (p, true),
                (_, Some(m)) => (m, false),
            };
            if at > pos {
                f(BlockStep::Audio(AudioBuffer {
                    left: &mut self.audio.left[pos..at],
                    right: &mut self.audio.right[pos..at],
                }));
                pos = at;
            }
            if is_param {
                let e = self.params[next_param];
                f(BlockStep::Param {
                    id: e.id.0,
                    value: e.value,
                });
                next_param += 1;
            } else {
                f(BlockStep::Midi(self.events[next_midi].message));
                next_midi += 1;
            }
        }
        if pos < frames {
            f(BlockStep::Audio(AudioBuffer {
                left: &mut self.audio.left[pos..frames],
                right: &mut self.audio.right[pos..frames],
            }));
        }
    }
}

/// How long a processor keeps sounding after its input falls silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tail {
    #[default]
    None,
    Frames(u32),
    /// It may never fall silent on its own, or the length isn't known.
    Infinite,
}

impl Tail {
    /// The convention CLAP and VST3 share: 0 is no tail and `u32::MAX` is infinite.
    pub fn from_samples(samples: u32) -> Self {
        match samples {
            0 => Self::None,
            u32::MAX => Self::Infinite,
            n => Self::Frames(n),
        }
    }
}

/// Something shared with a device that the UI loads, such as a sample. See
/// [`Processor::set_resource`].
pub type Resource = Arc<dyn Any + Send + Sync>;

/// A real-time audio processor: built-in device or hosted plugin.
///
/// Lifecycle: created and [`activate`](Processor::activate)d on the UI thread, then moved to the
/// audio thread where `process` and `stop_processing` are called. It is moved back to the UI
/// thread before being dropped, so `Drop` may block or deallocate freely.
pub trait Processor: Send {
    /// Prepare for processing. Called on the UI/main thread before the processor is sent
    /// to the audio thread. Allocate all buffers here.
    fn activate(&mut self, sample_rate: f64, max_block: usize);

    /// Process one block in place. Must not allocate, lock or block.
    fn process(&mut self, ctx: &mut ProcessContext);

    /// Set a parameter (plain value) on the UI thread, before the processor goes to the engine.
    /// Real-time changes arrive only through [`ProcessContext::params`].
    fn set_param(&mut self, id: u32, value: f64);

    /// Called on the audio thread right before the processor leaves the engine.
    fn stop_processing(&mut self) {}

    /// Which of the device's aux outputs are being tapped by another track, one bit per output.
    /// Called on the audio thread whenever routing changes. An instrument that takes a tapped
    /// output out of its own mix, as Impulse does (manual p. 390), reads this; most plug-ins
    /// mix every part into their main output regardless.
    fn set_aux_routed(&mut self, _mask: u32) {}

    /// Silence tails / release voices (e.g. after transport stop).
    fn reset(&mut self) {}

    /// Processing latency in samples.
    fn latency(&self) -> u32 {
        0
    }

    /// How long this processor rings on after its input stops.
    fn tail(&self) -> Tail {
        Tail::None
    }

    /// Replace a shared resource in `slot`, without interrupting voices. Called on the audio
    /// thread: return what was replaced, or `resource` itself if it was refused, so the engine
    /// can retire it. Never free a resource here.
    fn set_resource(&mut self, _slot: u32, resource: Resource) -> Option<Resource> {
        Some(resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParamId, TransportInfo};

    fn run(frames: usize, params: &[ParamEvent], events: &[MidiEvent]) -> Vec<String> {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        let mut out = EventQueue::with_capacity(0);
        let mut param_out = ParamOutQueue::with_capacity(0);
        let transport = TransportInfo::default();
        let view = ViewWriter::none();
        let mut ctx = ProcessContext {
            audio: AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            sidechain: None,
            aux_out: &mut [],
            events,
            events_out: &mut out,
            params,
            transport: &transport,
            param_out: &mut param_out,
            mod_out: &mut [],
            view: &view,
        };
        let mut log = Vec::new();
        ctx.for_each_step(|step| {
            log.push(match step {
                BlockStep::Audio(a) => format!("audio {}", a.frames()),
                BlockStep::Param { id, value } => format!("param {id}={value}"),
                BlockStep::Midi(_) => "midi".to_string(),
            })
        });
        log
    }

    fn param(offset: u32, id: u32, value: f64) -> ParamEvent {
        ParamEvent {
            offset,
            id: ParamId(id),
            value,
        }
    }

    fn note(offset: u32) -> MidiEvent {
        MidiEvent::new(offset, MidiMessage::AllNotesOff)
    }

    #[test]
    fn a_block_with_nothing_in_it_is_one_step() {
        assert_eq!(run(64, &[], &[]), ["audio 64"]);
    }

    #[test]
    fn the_block_splits_at_each_change_in_order() {
        assert_eq!(
            run(100, &[param(10, 1, 0.5), param(40, 2, 0.25)], &[note(25)]),
            [
                "audio 10",
                "param 1=0.5",
                "audio 15",
                "midi",
                "audio 15",
                "param 2=0.25",
                "audio 60"
            ]
        );
    }

    #[test]
    fn a_change_at_the_start_needs_no_audio_before_it() {
        assert_eq!(run(50, &[param(0, 1, 1.0)], &[]), ["param 1=1", "audio 50"]);
    }

    #[test]
    fn parameters_come_before_midi_on_the_same_frame() {
        assert_eq!(
            run(32, &[param(8, 1, 1.0)], &[note(8)]),
            ["audio 8", "param 1=1", "midi", "audio 24"]
        );
    }

    #[test]
    fn events_past_the_end_still_arrive_after_all_the_audio() {
        assert_eq!(
            run(16, &[param(99, 1, 1.0)], &[note(16)]),
            ["audio 16", "param 1=1", "midi"]
        );
    }

    #[test]
    fn event_queue_drops_what_it_has_no_room_for() {
        let mut q = EventQueue::with_capacity(2);
        q.extend(&[note(0), note(1), note(2)]);
        q.push(note(3));
        assert_eq!(q.len(), 2);
        q.clear();
        assert!(q.is_empty());
    }

    #[test]
    fn event_queue_sorts_note_offs_before_note_ons_at_the_same_frame() {
        let mut q = EventQueue::with_capacity(4);
        q.push(MidiEvent::new(
            4,
            MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: 0,
                tuning: 0.0,
            },
        ));
        q.push(MidiEvent::new(
            4,
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.0,
                note_id: 0,
            },
        ));
        q.sort();
        assert!(matches!(
            q.as_slice()[0].message,
            MidiMessage::NoteOff { .. }
        ));
    }

    #[test]
    fn tail_samples_follow_the_plugin_convention() {
        assert_eq!(Tail::from_samples(0), Tail::None);
        assert_eq!(Tail::from_samples(100), Tail::Frames(100));
        assert_eq!(Tail::from_samples(u32::MAX), Tail::Infinite);
    }
}
