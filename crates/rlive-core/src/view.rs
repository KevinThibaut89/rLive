//! Device data for the UI: one seqlock per device, written by the audio thread and read by the UI.
//!
//! A device writes through a [`ViewWriter`]. When no [`ViewReader`] is attached, writing does
//! nothing, so meters and scopes cost nothing on a device nobody is looking at.

use std::sync::atomic::{fence, AtomicU32, Ordering};
use std::sync::Arc;

/// A device's own name for a piece of view data. It is also where the data starts in the
/// device's channel, so views laid out next to each other must not overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewId(pub u32);

/// How many values [`Item::Meter`](crate::Item::Meter) reads at its view: linear left and right.
pub const METER_VALUES: usize = 2;

/// How many values [`Item::Scope`](crate::Item::Scope) reads at its view: `[-1, 1]` samples for
/// a waveform, `[0, 1]` magnitudes for a spectrum, `[l, r]` pairs for a vectorscope.
pub const SCOPE_VALUES: usize = 128;

/// The storage shared between one device and its UI.
pub struct ViewChannel {
    seq: AtomicU32,
    readers: AtomicU32,
    values: Box<[AtomicU32]>,
}

impl ViewChannel {
    /// A channel holding `len` values, all zero.
    pub fn new(len: usize) -> Self {
        Self {
            seq: AtomicU32::new(0),
            readers: AtomicU32::new(0),
            values: (0..len).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Start reading. The device writes until the returned reader is dropped.
    pub fn attach(self: &Arc<Self>) -> ViewReader {
        self.readers.fetch_add(1, Ordering::Relaxed);
        ViewReader {
            channel: self.clone(),
        }
    }
}

/// The audio-thread end of a [`ViewChannel`].
#[derive(Clone, Copy)]
pub struct ViewWriter<'a> {
    channel: Option<&'a ViewChannel>,
}

impl<'a> ViewWriter<'a> {
    pub fn new(channel: &'a ViewChannel) -> Self {
        Self {
            channel: Some(channel),
        }
    }

    /// A writer that never writes, for a device with no channel.
    pub const fn none() -> ViewWriter<'static> {
        ViewWriter { channel: None }
    }

    /// Whether a UI is reading. Skip expensive measurements when it isn't.
    pub fn is_active(&self) -> bool {
        self.channel
            .is_some_and(|c| c.readers.load(Ordering::Relaxed) > 0)
    }

    /// Publish `values` at `view`, cut off at the end of the channel. There is only ever one
    /// writer, the audio thread.
    pub fn publish(&self, view: ViewId, values: &[f32]) {
        let Some(c) = self.channel.filter(|_| self.is_active()) else {
            return;
        };
        let Some(slots) = c.values.get(view.0 as usize..) else {
            return;
        };
        let seq = c.seq.load(Ordering::Relaxed);
        c.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (slot, value) in slots.iter().zip(values) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
        c.seq.store(seq.wrapping_add(2), Ordering::Release);
    }
}

/// The UI end of a [`ViewChannel`]. The device stops writing when the last one is dropped.
pub struct ViewReader {
    channel: Arc<ViewChannel>,
}

impl ViewReader {
    /// Copy the data at `view` into `out`. `false` if the writer kept interrupting, in which
    /// case `out` holds no consistent data and the UI should keep what it had.
    pub fn read(&self, view: ViewId, out: &mut [f32]) -> bool {
        const ATTEMPTS: usize = 16;
        let c = &self.channel;
        let Some(slots) = c.values.get(view.0 as usize..) else {
            return false;
        };
        for _ in 0..ATTEMPTS {
            let before = c.seq.load(Ordering::Acquire);
            if before & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            for (o, slot) in out.iter_mut().zip(slots) {
                *o = f32::from_bits(slot.load(Ordering::Relaxed));
            }
            fence(Ordering::Acquire);
            if c.seq.load(Ordering::Relaxed) == before {
                return true;
            }
        }
        false
    }
}

impl Drop for ViewReader {
    fn drop(&mut self) {
        self.channel.readers.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn nothing_is_written_while_no_one_reads() {
        let channel = Arc::new(ViewChannel::new(4));
        let writer = ViewWriter::new(&channel);
        assert!(!writer.is_active());
        writer.publish(ViewId(0), &[1.0, 2.0]);
        assert_eq!(channel.seq.load(Ordering::Relaxed), 0, "seqlock untouched");

        let reader = channel.attach();
        let mut out = [9.0; 2];
        assert!(reader.read(ViewId(0), &mut out));
        assert_eq!(out, [0.0, 0.0]);
    }

    #[test]
    fn a_reader_sees_what_was_published_where_it_was_published() {
        let channel = Arc::new(ViewChannel::new(4));
        let reader = channel.attach();
        ViewWriter::new(&channel).publish(ViewId(1), &[0.5, 0.25]);
        let mut out = [0.0; 2];
        assert!(reader.read(ViewId(1), &mut out));
        assert_eq!(out, [0.5, 0.25]);
        drop(reader);
        assert!(!ViewWriter::new(&channel).is_active());
    }

    #[test]
    fn publishing_past_the_end_is_cut_off() {
        let channel = Arc::new(ViewChannel::new(2));
        let reader = channel.attach();
        let writer = ViewWriter::new(&channel);
        writer.publish(ViewId(1), &[1.0, 2.0, 3.0]);
        writer.publish(ViewId(9), &[4.0]);
        let mut out = [0.0; 2];
        assert!(reader.read(ViewId(0), &mut out));
        assert_eq!(out, [0.0, 1.0]);
    }

    #[test]
    fn a_writer_with_no_channel_does_nothing() {
        let writer = ViewWriter::none();
        assert!(!writer.is_active());
        writer.publish(ViewId(0), &[1.0]);
    }

    #[test]
    fn a_reader_never_sees_a_torn_write() {
        let channel = Arc::new(ViewChannel::new(8));
        let reader = channel.attach();
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s) = (channel.clone(), stop.clone());
        let writer = std::thread::spawn(move || {
            let w = ViewWriter::new(&c);
            let mut v = 0.0f32;
            while !s.load(Ordering::Relaxed) {
                v += 1.0;
                w.publish(ViewId(0), &[v; 8]);
            }
        });
        let mut consistent = 0;
        for _ in 0..200_000 {
            let mut out = [0.0; 8];
            if reader.read(ViewId(0), &mut out) {
                assert!(out.iter().all(|x| *x == out[0]), "torn: {out:?}");
                consistent += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(consistent > 0);
    }
}
