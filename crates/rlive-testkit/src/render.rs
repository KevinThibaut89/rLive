//! Offline rendering.

use rlive_core::{AudioBuffer, MidiEvent, ParamOutQueue, ProcessContext, Processor, TransportInfo};

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
    let transport = TransportInfo {
        playing: true,
        sample_rate,
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut q = ParamOutQueue::with_capacity(16);
    for b in 0..blocks {
        let mut l = vec![input; block_size];
        let mut r = vec![input; block_size];
        let ev: &[MidiEvent] = if b == 0 { events } else { &[] };
        p.process(&mut ProcessContext {
            audio: AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            events: ev,
            transport: &transport,
            param_out: &mut q,
        });
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
