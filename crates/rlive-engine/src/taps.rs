//! Signals picked up along a track for other tracks and devices: tap points, a device's extra
//! outputs and its sidechain feed (manual pp. 381-383, 389-392).
//!
//! A plan names each signal it reads once and gives it a [`TapBus`]. The track that makes the
//! signal writes it here as it renders, and anything later in the plan reads it, each through a
//! delay of its own. Buses are built on the UI thread and only ever swapped in whole.

use crate::delay::{FadingDelay, StereoLine};
use rlive_core::AudioInput;

/// One tapped signal: the block being rendered, and however much history its readers need.
pub struct TapBus {
    left: Box<[f32]>,
    right: Box<[f32]>,
    line: Option<Box<StereoLine>>,
    readers: Box<[FadingDelay]>,
}

impl TapBus {
    /// Call on the UI thread. `longest_delay` is what the slowest reader is held back by.
    pub fn new(max_block: usize, readers: usize, longest_delay: u32) -> Self {
        Self {
            left: vec![0.0; max_block].into_boxed_slice(),
            right: vec![0.0; max_block].into_boxed_slice(),
            line: (longest_delay > 0).then(|| StereoLine::new(longest_delay, max_block)),
            readers: vec![FadingDelay::default(); readers].into_boxed_slice(),
        }
    }

    /// The longest delay this bus can be read at.
    pub(crate) fn max_delay(&self) -> u32 {
        self.line.as_ref().map_or(0, |l| l.max_delay())
    }

    /// Store part of a block, `from` frames into it.
    pub(crate) fn write(&mut self, from: usize, left: &[f32], right: &[f32]) {
        let end = from + left.len();
        self.left[from..end].copy_from_slice(left);
        self.right[from..end].copy_from_slice(right);
    }

    /// Silence part of a block.
    pub(crate) fn silence(&mut self, from: usize, to: usize) {
        self.left[from..to].fill(0.0);
        self.right[from..to].fill(0.0);
    }

    /// The block is written: add it to the history its readers are held back through.
    pub(crate) fn finish(&mut self, frames: usize) {
        if let Some(line) = &mut self.line {
            line.push(&self.left[..frames], &self.right[..frames]);
        }
    }

    pub(crate) fn set_reader(&mut self, reader: u16, delay: u32) {
        if let Some(r) = self.readers.get_mut(reader as usize) {
            r.set_target(delay);
        }
    }

    /// Part of this block as a read-only input, at no delay.
    pub(crate) fn input(&self, from: usize, to: usize) -> AudioInput<'_> {
        AudioInput {
            left: &self.left[from..to],
            right: &self.right[from..to],
        }
    }

    /// Mix this block into `left` and `right` through one reader's delay.
    pub(crate) fn mix_into(
        &mut self,
        reader: u16,
        left: &mut [f32],
        right: &mut [f32],
        scratch: (&mut [f32], &mut [f32]),
        frames: usize,
    ) {
        let (sl, sr) = scratch;
        let delay = self.readers.get_mut(reader as usize);
        match (&self.line, delay) {
            (Some(line), Some(delay)) if !delay.is_straight() => {
                delay.read(line, &mut sl[..frames], &mut sr[..frames]);
                for i in 0..frames {
                    left[i] += sl[i];
                    right[i] += sr[i];
                }
            }
            _ => {
                for i in 0..frames {
                    left[i] += self.left[i];
                    right[i] += self.right[i];
                }
            }
        }
    }

    /// Fill `self` with what `src` holds, through one of `src`'s readers.
    pub(crate) fn fetch_from(&mut self, src: &mut TapBus, reader: u16, frames: usize) {
        match (&src.line, src.readers.get_mut(reader as usize)) {
            (Some(line), Some(delay)) if !delay.is_straight() => {
                delay.read(line, &mut self.left[..frames], &mut self.right[..frames]);
            }
            _ => {
                self.left[..frames].copy_from_slice(&src.left[..frames]);
                self.right[..frames].copy_from_slice(&src.right[..frames]);
            }
        }
    }

    /// Keep what a bus at the same place in the previous plan remembers.
    pub(crate) fn adopt(&mut self, old: &TapBus) {
        if self.left.len() == old.left.len() {
            self.left.copy_from_slice(&old.left);
            self.right.copy_from_slice(&old.right);
        }
        if let (Some(new), Some(old)) = (&mut self.line, &old.line) {
            new.inherit(old);
        }
        for (new, old) in self.readers.iter_mut().zip(old.readers.iter()) {
            *new = *old;
        }
    }
}

/// How many extra outputs of one device can be tapped.
pub const MAX_AUX_OUTS: usize = 16;

/// One extra output of a device. Silent unless the device writes it.
pub(crate) struct AuxBus {
    pub left: Box<[f32]>,
    pub right: Box<[f32]>,
}

impl AuxBus {
    fn new(max_block: usize) -> Self {
        Self {
            left: vec![0.0; max_block].into_boxed_slice(),
            right: vec![0.0; max_block].into_boxed_slice(),
        }
    }

    pub(crate) fn bank(count: usize, max_block: usize) -> Vec<AuxBus> {
        (0..count.min(MAX_AUX_OUTS))
            .map(|_| Self::new(max_block))
            .collect()
    }
}

/// The buses a track or a rack chain writes as it goes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TapSlots {
    /// Before its devices.
    pub pre: Option<u32>,
    /// After its devices.
    pub post_fx: Option<u32>,
    /// After its mixer.
    pub post_mix: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bus_reads_back_its_block() {
        let mut bus = TapBus::new(4, 1, 0);
        bus.write(0, &[1.0, 2.0], &[3.0, 4.0]);
        bus.write(2, &[5.0, 6.0], &[7.0, 8.0]);
        bus.finish(4);
        let (mut l, mut r) = ([1.0; 4], [1.0; 4]);
        let (mut a, mut b) = ([0.0; 4], [0.0; 4]);
        bus.mix_into(0, &mut l, &mut r, (&mut a, &mut b), 4);
        assert_eq!(l, [2.0, 3.0, 6.0, 7.0]);
        assert_eq!(r, [4.0, 5.0, 8.0, 9.0]);
    }

    #[test]
    fn a_reader_with_a_delay_hears_the_past() {
        let mut bus = TapBus::new(4, 1, 8);
        bus.set_reader(0, 8);
        // The first block fades to the delay; the blocks after it read straight through.
        let mut sample = 0.0;
        let mut out = Vec::new();
        for _ in 0..200 {
            let block: Vec<f32> = (0..4)
                .map(|_| {
                    sample += 1.0;
                    sample
                })
                .collect();
            bus.write(0, &block, &block);
            bus.finish(4);
            let (mut l, mut r) = ([0.0; 4], [0.0; 4]);
            let (mut a, mut b) = ([0.0; 4], [0.0; 4]);
            bus.mix_into(0, &mut l, &mut r, (&mut a, &mut b), 4);
            out.extend(l);
        }
        // Past the crossfade each frame is just what was written eight frames before.
        let n = out.len();
        for (i, s) in out[n - 16..].iter().enumerate() {
            assert_eq!(*s, (n - 16 + i) as f32 - 7.0);
        }
    }

    #[test]
    fn fetching_a_sidechain_through_no_delay_copies_the_block() {
        let (mut src, mut dst) = (TapBus::new(2, 1, 0), TapBus::new(2, 0, 0));
        src.write(0, &[0.5, 0.25], &[0.5, 0.25]);
        dst.fetch_from(&mut src, 0, 2);
        assert_eq!(dst.input(0, 2).left, [0.5, 0.25]);
    }
}
