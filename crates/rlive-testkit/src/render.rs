//! Offline rendering.

use rlive_core::{
    AudioBuffer, EventQueue, MidiEvent, ParamEvent, ParamOutQueue, ProcessContext, Processor,
    TransportInfo, ViewWriter,
};

/// Renders `frames` stereo frames through `process`, `chunk` frames at a time (the last chunk
/// may be shorter). Buffers start out silent.
pub fn render_chunks(
    frames: usize,
    chunk: usize,
    mut process: impl FnMut(&mut [f32], &mut [f32]),
) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    for (cl, cr) in l.chunks_mut(chunk).zip(r.chunks_mut(chunk)) {
        process(cl, cr);
    }
    (l, r)
}

/// The parts of a [`ProcessContext`] a test doesn't care about, ready for many blocks.
pub struct Harness {
    pub transport: TransportInfo,
    pub events_out: EventQueue,
    pub param_out: ParamOutQueue,
}

impl Harness {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            transport: TransportInfo {
                playing: true,
                sample_rate,
                ..Default::default()
            },
            events_out: EventQueue::with_capacity(1024),
            param_out: ParamOutQueue::with_capacity(256),
        }
    }

    /// Runs one block through `p` in place. `events_out` keeps what the processor wrote.
    pub fn process(
        &mut self,
        p: &mut dyn Processor,
        left: &mut [f32],
        right: &mut [f32],
        events: &[MidiEvent],
        params: &[ParamEvent],
    ) {
        self.events_out.clear();
        let view = ViewWriter::none();
        p.process(&mut ProcessContext {
            audio: AudioBuffer { left, right },
            sidechain: None,
            aux_out: &mut [],
            events,
            events_out: &mut self.events_out,
            params,
            transport: &self.transport,
            param_out: &mut self.param_out,
            mod_out: &mut [],
            view: &view,
        });
    }
}

/// Runs a processor over `blocks` blocks of constant `input`, delivering `events` in the first
/// block, and returns its left output.
pub fn render_processor(
    p: &mut dyn Processor,
    input: f32,
    events: &[MidiEvent],
    blocks: usize,
    block_size: usize,
    sample_rate: f64,
) -> Vec<f32> {
    let mut h = Harness::new(sample_rate);
    let mut out = Vec::new();
    for b in 0..blocks {
        let mut l = vec![input; block_size];
        let mut r = vec![input; block_size];
        h.process(p, &mut l, &mut r, if b == 0 { events } else { &[] }, &[]);
        out.extend_from_slice(&l);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_chunks_covers_every_frame_with_a_short_last_chunk() {
        let mut lens = Vec::new();
        let (l, r) = render_chunks(700, 300, |cl, cr| {
            lens.push(cl.len());
            cl.fill(1.0);
            cr.fill(-1.0);
        });
        assert_eq!(lens, [300, 300, 100]);
        assert!(l.iter().all(|s| *s == 1.0) && l.len() == 700);
        assert!(r.iter().all(|s| *s == -1.0) && r.len() == 700);
    }

    struct Gain(f32);

    impl Processor for Gain {
        fn activate(&mut self, _: f64, _: usize) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            for s in ctx.audio.left.iter_mut() {
                *s *= self.0;
            }
        }
        fn set_param(&mut self, _: u32, _: f64) {}
    }

    #[test]
    fn render_processor_feeds_the_input_through() {
        let out = render_processor(&mut Gain(0.5), 0.8, &[], 3, 64, 48_000.0);
        assert_eq!(out.len(), 192);
        assert!(out.iter().all(|s| (*s - 0.4).abs() < 1e-6));
    }
}
