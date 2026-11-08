//! Lookahead buffer: delays audio by a fixed amount while reporting what's coming.

use super::util::sanitize;

/// A delay of `N`-channel frames that knows the loudest sample among those about to be output.
///
/// Each [`process`](Self::process) takes the incoming frame and returns the one from `len`
/// frames ago; [`window_peak`](Self::window_peak) covers that outgoing frame and everything
/// after it, so a limiter can start reacting before a peak arrives.
#[derive(Clone, Debug)]
pub struct LookaheadBuffer<const N: usize> {
    frames: Vec<[f32; N]>,
    capacity: usize,
    len: usize,
    write: usize,
    // A monotonic queue of (age counter, peak) over the window, as ring indices.
    queue: Vec<(u64, f32)>,
    head: usize,
    count: usize,
    written: u64,
}

impl<const N: usize> LookaheadBuffer<N> {
    /// Room for up to `max_len` frames of delay.
    pub fn new(max_len: usize) -> Self {
        let capacity = max_len + 1;
        Self {
            frames: vec![[0.0; N]; capacity],
            capacity,
            len: 0,
            write: 0,
            queue: vec![(0, 0.0); capacity],
            head: 0,
            count: 0,
            written: 0,
        }
    }

    /// The delay, in frames; a device reports it as latency.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn latency(&self) -> u32 {
        self.len as u32
    }

    /// Set the delay, clamped to the capacity. Rebuilds the window, at a cost bounded by the
    /// capacity; only the delay of what is already buffered changes.
    pub fn set_len(&mut self, len: usize) {
        let len = len.min(self.capacity - 1);
        if len == self.len {
            return;
        }
        self.len = len;
        self.head = 0;
        self.count = 0;
        // Replay what the new window covers, oldest first, as `process` would have.
        let start = self.written.saturating_sub(len as u64).max(1);
        for index in start..=self.written {
            let ago = (self.written - index + 1) as usize;
            let frame = self.frames[(self.write + self.capacity - ago) % self.capacity];
            self.enqueue(index, Self::frame_peak(&frame));
        }
    }

    pub fn reset(&mut self) {
        self.frames.fill([0.0; N]);
        self.write = 0;
        self.head = 0;
        self.count = 0;
        self.written = 0;
    }

    /// The largest absolute sample across the outgoing frame and everything newer.
    #[inline]
    pub fn window_peak(&self) -> f32 {
        if self.count == 0 {
            0.0
        } else {
            self.queue[self.head].1
        }
    }

    #[inline]
    fn frame_peak(frame: &[f32; N]) -> f32 {
        frame.iter().fold(0.0, |m: f32, s| m.max(s.abs()))
    }

    fn enqueue(&mut self, index: u64, peak: f32) {
        let cap = self.queue.len();
        // Drop quieter entries from the back; each is pushed and popped at most once.
        while self.count > 0 {
            let back = (self.head + self.count - 1) % cap;
            if self.queue[back].1 > peak {
                break;
            }
            self.count -= 1;
        }
        self.queue[(self.head + self.count) % cap] = (index, peak);
        self.count += 1;
    }

    /// Takes the newest frame and returns the one due out.
    #[inline]
    pub fn process(&mut self, frame: [f32; N]) -> [f32; N] {
        let frame = frame.map(sanitize);
        self.frames[self.write] = frame;
        let out = self.frames[(self.write + self.capacity - self.len) % self.capacity];
        self.write = (self.write + 1) % self.capacity;
        self.written += 1;
        // Expire what has fallen out of the window: the outgoing frame is the oldest kept.
        self.enqueue(self.written, Self::frame_peak(&frame));
        let oldest = self.written.saturating_sub(self.len as u64);
        while self.count > 0 && self.queue[self.head].0 < oldest {
            self.head = (self.head + 1) % self.queue.len();
            self.count -= 1;
        }
        out
    }
}
