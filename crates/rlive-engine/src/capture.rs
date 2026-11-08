//! Taking audio off the signal path for recording, without disturbing it.
//!
//! A track that is about to record gets a [`CaptureSink`], built on the UI thread with its other
//! half, a [`CaptureTail`], going to the disk writer. The audio thread keeps the sink's clock
//! running and hands it each block; the sink decides, to the frame, what belongs in the take.
//!
//! A sink places what it records on a *recording clock* that runs behind the playhead by its
//! latency offset. That one number is how both Overall Latency and Keep Monitoring Latency reach
//! the audio: a window of `[from, until)` on that clock is the audio that belongs on the timeline
//! there, wherever it physically arrived. Everything the sink reports is on that clock too.
//!
//! Frames are never allowed to go missing silently. If the ring is full the block is left out,
//! counted in `dropped`, and the sink remembers to tell the tail how much is missing, so the file
//! stays in step with the timeline.

use crate::EngineEvent;
use rlive_core::TrackId;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Where a captured track's signal comes from. The plan says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputSource {
    #[default]
    None,
    /// Hardware channels, by the first channel's index.
    Hardware { channel: u16, stereo: bool },
    /// The Main output: resampling.
    Main,
}

/// What a sink records for, which says when it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMode {
    /// Follows Arrangement Record.
    #[default]
    Arrangement,
    /// Follows the track's Session slot recording.
    Slot,
}

/// The most frames one gap marker can say are missing.
const MAX_GAP: u64 = 0x3f_ffff;
/// Jumps a sink can be waiting to act on. A jump is held for only as long as the latency offset,
/// and two within that would need a loop shorter than the latency.
const MAX_JUMPS: usize = 4;

/// A sample that says frames are missing from the stream. Real audio never has this pattern,
/// since the audio thread turns any NaN into silence.
fn gap_marker(frames: u64) -> f32 {
    f32::from_bits(0x7fc0_0000 | frames.min(MAX_GAP) as u32)
}

fn is_gap(sample: f32) -> Option<u64> {
    let bits = sample.to_bits();
    (bits & 0x7fc0_0000 == 0x7fc0_0000).then_some((bits & 0x003f_ffff) as u64)
}

/// A piece of what came out of the ring.
pub enum Chunk<'a> {
    /// Interleaved samples, not necessarily whole frames.
    Samples(&'a [f32]),
    /// This many frames are missing here.
    Gap(u64),
}

/// The disk writer's end of a capture ring.
pub struct CaptureTail {
    ring: rtrb::Consumer<f32>,
    channels: u8,
    dropped: Arc<AtomicU64>,
    expected: Arc<AtomicU64>,
}

impl CaptureTail {
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Frames the audio thread could not fit in the ring so far.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// How many frames the take should come to, gaps and all. When the audio thread could never
    /// fit a marker in the ring, what remains missing is at the end of what was written.
    pub fn expected_frames(&self) -> u64 {
        self.expected.load(Ordering::Relaxed)
    }

    /// A counter that goes on working after the tail has gone.
    pub fn dropped_counter(&self) -> Arc<AtomicU64> {
        self.dropped.clone()
    }

    /// Whether the sink has gone, so what is in the ring is all there will ever be.
    pub fn is_abandoned(&self) -> bool {
        self.ring.is_abandoned()
    }

    /// Takes everything in the ring. Returns how many samples went by.
    pub fn drain(&mut self, mut each: impl FnMut(Chunk<'_>)) -> usize {
        let Ok(chunk) = self.ring.read_chunk(self.ring.slots()) else {
            return 0;
        };
        let (a, b) = chunk.as_slices();
        let total = a.len() + b.len();
        for part in [a, b] {
            let mut rest = part;
            while !rest.is_empty() {
                match rest.iter().position(|s| s.is_nan()) {
                    Some(at) => {
                        if at > 0 {
                            each(Chunk::Samples(&rest[..at]));
                        }
                        if let Some(frames) = is_gap(rest[at]) {
                            each(Chunk::Gap(frames));
                        }
                        rest = &rest[at + 1..];
                    }
                    None => {
                        each(Chunk::Samples(rest));
                        rest = &[];
                    }
                }
            }
        }
        chunk.commit_all();
        total
    }
}

/// The audio thread's end of a capture ring, and what it knows of the take.
pub struct CaptureSink {
    ring: rtrb::Producer<f32>,
    channels: u8,
    dropped: Arc<AtomicU64>,
    expected: Arc<AtomicU64>,
    pub(crate) mode: CaptureMode,
    /// How far this take's clock runs behind the playhead, in frames.
    offset: u32,
    /// The recording clock, a tick. Only meaningful once `running`.
    clock: f64,
    running: bool,
    /// The window was open after the last frame.
    inside: bool,
    /// Frames written or accounted for, which is where the next frame goes in the file.
    frames: u64,
    /// Frames lost that the tail hasn't been told about yet.
    gap: u64,
    /// Playhead jumps, as `(frame clock, destination)`, waiting for the recording clock to get
    /// there. `count` of them are live.
    jumps: [(u64, f64); MAX_JUMPS],
    count: usize,
}

/// The audio a block offers a sink.
#[derive(Clone, Copy)]
pub(crate) struct CaptureBlock<'a> {
    pub(crate) left: &'a [f32],
    pub(crate) right: &'a [f32],
    /// The engine's frame count at the start of the block.
    pub(crate) clock: u64,
    /// Song position at the start of the block.
    pub(crate) t0: f64,
    pub(crate) ticks_per_sample: f64,
    /// What to record, on the recording clock. `None` is nothing.
    pub(crate) window: Option<(f64, f64)>,
}

/// Build the two ends of a capture. Call on the UI thread. `capacity` is in frames, and how long
/// the disk can fall behind before audio has to be dropped.
pub fn capture_pair(
    channels: u8,
    capacity: usize,
    mode: CaptureMode,
    offset: u32,
) -> (CaptureSink, CaptureTail) {
    let channels = channels.clamp(1, 2);
    let (ring, tail) = rtrb::RingBuffer::new(capacity.max(1) * channels as usize + 4);
    let dropped = Arc::new(AtomicU64::new(0));
    let expected = Arc::new(AtomicU64::new(0));
    (
        CaptureSink {
            ring,
            channels,
            dropped: dropped.clone(),
            expected: expected.clone(),
            mode,
            offset,
            clock: 0.0,
            running: false,
            inside: false,
            frames: 0,
            gap: 0,
            jumps: [(0, 0.0); MAX_JUMPS],
            count: 0,
        },
        CaptureTail {
            ring: tail,
            channels,
            dropped,
            expected,
        },
    )
}

impl CaptureSink {
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Frames of the take so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Where the recording clock is.
    pub(crate) fn clock(&self) -> f64 {
        self.clock
    }

    /// Whether the clock has been started by the transport.
    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    /// The clock restarts from the playhead the next time the transport runs.
    pub(crate) fn restart(&mut self) {
        self.running = false;
        self.count = 0;
    }

    /// Note that the playhead moved at `clock`. The take jumps once its own clock gets there.
    pub(crate) fn jumped(&mut self, clock: u64, to: f64) {
        if self.count == MAX_JUMPS {
            self.jumps.copy_within(1.., 0);
            self.count -= 1;
        }
        self.jumps[self.count] = (clock + self.offset as u64, to);
        self.count += 1;
    }

    /// Record a tempo change, which the UI turns into a Warp Marker.
    pub(crate) fn mark_tempo(
        &self,
        track: TrackId,
        ticks_per_frame: f64,
        events: &mut rtrb::Producer<EngineEvent>,
    ) {
        if self.inside {
            let _ = events.push(EngineEvent::CaptureSegment {
                track,
                frame: self.frames,
                tick: self.clock,
                jump: false,
                ticks_per_frame,
            });
        }
    }

    fn write(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len();
        let wanted = n * self.channels as usize + (self.gap / MAX_GAP + 1) as usize;
        if self.ring.slots() < wanted {
            self.gap += n as u64;
            self.dropped.fetch_add(n as u64, Ordering::Relaxed);
            self.frames += n as u64;
            self.expected.store(self.frames, Ordering::Relaxed);
            return;
        }
        while self.gap > 0 {
            let piece = self.gap.min(MAX_GAP);
            let _ = self.ring.push(gap_marker(piece));
            self.gap -= piece;
        }
        let clean = |s: f32| if s.is_nan() { 0.0 } else { s };
        if self.channels == 1 {
            for &s in left {
                let _ = self.ring.push(clean(s));
            }
        } else {
            for (&l, &r) in left.iter().zip(right) {
                let _ = self.ring.push(clean(l));
                let _ = self.ring.push(clean(r));
            }
        }
        self.frames += n as u64;
        self.expected.store(self.frames, Ordering::Relaxed);
    }

    /// Run the take across one block, writing what the window lets through.
    pub(crate) fn run(
        &mut self,
        block: CaptureBlock<'_>,
        track: TrackId,
        events: &mut rtrb::Producer<EngineEvent>,
    ) {
        let n = block.left.len();
        let tps = block.ticks_per_sample;
        if !self.running {
            self.clock = block.t0 - self.offset as f64 * tps;
            self.running = true;
        }
        let mut f = 0;
        while f < n {
            let now = block.clock + f as u64;
            // The soonest jump still ahead.
            let mut next = n;
            if let Some(at) = self.jumps[..self.count].iter().position(|j| j.0 <= now) {
                let (_, to) = self.jumps[at];
                self.jumps.copy_within(at + 1..self.count, at);
                self.count -= 1;
                self.clock = to;
                if self.inside && block.window.is_some() {
                    let _ = events.push(EngineEvent::CaptureSegment {
                        track,
                        frame: self.frames,
                        tick: self.clock,
                        jump: true,
                        ticks_per_frame: tps,
                    });
                }
                continue;
            }
            for &(at, _) in &self.jumps[..self.count] {
                next = next.min((at - block.clock) as usize);
            }
            let left = next - f;
            let Some((from, until)) = block.window else {
                self.inside = false;
                self.clock += left as f64 * tps;
                f = next;
                continue;
            };
            let near = 1e-3 * tps;
            if self.clock < from - near {
                let wait = ((from - self.clock) / tps - 1e-3).ceil().max(1.0) as usize;
                let skip = wait.min(left);
                self.inside = false;
                self.clock += skip as f64 * tps;
                f += skip;
                continue;
            }
            if self.clock >= until - near {
                self.inside = false;
                self.clock += left as f64 * tps;
                f = next;
                continue;
            }
            let room = ((until - self.clock) / tps - 1e-3).ceil().max(1.0) as usize;
            let m = room.min(left);
            if !self.inside {
                self.inside = true;
                let _ = events.push(EngineEvent::CaptureSegment {
                    track,
                    frame: self.frames,
                    tick: self.clock,
                    jump: self.frames > 0,
                    ticks_per_frame: tps,
                });
            }
            self.write(&block.left[f..f + m], &block.right[f..f + m]);
            self.clock += m as f64 * tps;
            f += m;
        }
    }
}

/// A Session slot being recorded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SlotRec {
    pub(crate) slot: usize,
    /// Where recording starts, on the song's timeline.
    pub(crate) from: f64,
    pub(crate) started: bool,
    /// Where recording ends, and whether the clip then plays from there.
    pub(crate) close: Option<(f64, bool)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events() -> (rtrb::Producer<EngineEvent>, rtrb::Consumer<EngineEvent>) {
        rtrb::RingBuffer::new(64)
    }

    fn block<'a>(left: &'a [f32], right: &'a [f32], clock: u64, t0: f64) -> CaptureBlock<'a> {
        CaptureBlock {
            left,
            right,
            clock,
            t0,
            ticks_per_sample: 1.0,
            window: Some((f64::NEG_INFINITY, f64::INFINITY)),
        }
    }

    #[test]
    fn a_full_ring_drops_counts_and_leaves_a_gap_of_the_right_length() {
        let (mut sink, mut tail) = capture_pair(1, 100, CaptureMode::Arrangement, 0);
        let (mut tx, _rx) = events();
        let audio = vec![0.25f32; 80];
        let t = TrackId::new();
        for n in 0..4u64 {
            sink.run(block(&audio, &audio, n * 80, n as f64 * 80.0), t, &mut tx);
        }
        assert_eq!(tail.dropped(), 240, "one block fits, three do not");
        let (mut samples, mut gap) = (0, 0);
        let drain = |tail: &mut CaptureTail, samples: &mut usize, gap: &mut u64| {
            tail.drain(|c| match c {
                Chunk::Samples(s) => *samples += s.len(),
                Chunk::Gap(g) => *gap += g,
            });
        };
        drain(&mut tail, &mut samples, &mut gap);
        assert_eq!((samples, gap), (80, 0));
        sink.run(block(&audio, &audio, 320, 320.0), t, &mut tx);
        drain(&mut tail, &mut samples, &mut gap);
        assert_eq!((samples, gap), (160, 240), "the file stays in step");
        assert_eq!(sink.frames(), 400);
    }

    #[test]
    fn the_window_is_cut_to_the_frame_on_the_recording_clock() {
        let (mut sink, mut tail) = capture_pair(1, 4096, CaptureMode::Arrangement, 10);
        let (mut tx, mut rx) = events();
        let audio: Vec<f32> = (0..256).map(|i| i as f32).collect();
        let mut b = block(&audio, &audio, 0, 1000.0);
        b.window = Some((1100.0, 1200.0));
        sink.run(b, TrackId::new(), &mut tx);
        let mut got = Vec::new();
        tail.drain(|c| {
            if let Chunk::Samples(s) = c {
                got.extend_from_slice(s);
            }
        });
        // Frame 110 is the first whose clock, 10 behind the playhead, has reached 1100.
        assert_eq!(got.len(), 100);
        assert_eq!(got[0], 110.0);
        match rx.pop().unwrap() {
            EngineEvent::CaptureSegment {
                frame, tick, jump, ..
            } => {
                assert_eq!((frame, tick), (0, 1100.0));
                assert!(!jump);
            }
            _ => panic!("expected the take to open"),
        }
    }

    #[test]
    fn a_jump_reaches_the_take_only_when_its_clock_does() {
        let (mut sink, mut tail) = capture_pair(1, 4096, CaptureMode::Arrangement, 5);
        let (mut tx, mut rx) = events();
        let audio = vec![0.5f32; 100];
        let t = TrackId::new();
        sink.jumped(40, 0.0);
        sink.run(block(&audio, &audio, 0, 960.0), t, &mut tx);
        let mut marks = Vec::new();
        while let Ok(EngineEvent::CaptureSegment { frame, tick, .. }) = rx.pop() {
            marks.push((frame, tick));
        }
        assert_eq!(marks[0], (0, 955.0));
        assert_eq!(marks[1], (45, 0.0), "held back by the offset");
        let mut total = 0;
        tail.drain(|c| {
            if let Chunk::Samples(s) = c {
                total += s.len();
            }
        });
        assert_eq!(total, 100, "a take is contiguous across a jump");
    }

    #[test]
    fn running_does_not_allocate() {
        let (mut sink, _tail) = capture_pair(2, 4096, CaptureMode::Arrangement, 0);
        let (mut tx, _rx) = events();
        let audio = vec![0.1f32; 256];
        let t = TrackId::new();
        rlive_testkit::assert_no_alloc(|| {
            sink.run(block(&audio, &audio, 0, 0.0), t, &mut tx);
            sink.jumped(256, 0.0);
            sink.run(block(&audio, &audio, 256, 256.0), t, &mut tx);
        });
    }
}
