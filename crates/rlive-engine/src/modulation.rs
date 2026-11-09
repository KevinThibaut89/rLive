//! Modulator runtime: what LFOs, followers and the like write, and how it reaches the cells they
//! are mapped to.
//!
//! A device publishes its `mod_out` values as it renders. They land in [`ModBus::back`], and the
//! end of every block rotates the buffers, so a route always reads what was published in the
//! block before. That makes one block of latency the whole story, wherever the modulator sits:
//! nothing depends on the order tracks render in, so a mapping to another track adds no edge to
//! the render plan and can't make a loop.
//!
//! Within a block a route ramps from the value before to the value published last block, one
//! control step at a time, so a block-rate source moves a parameter smoothly.
//!
//! During a block several threads may be publishing at once, each for a device of its own track,
//! so the buffers are atomics. Nothing there is ordered: reading and writing never meet in one
//! block, since readers read what the last block finished.
//!
//! A modulator with `n` mappable outputs declares `2 * n` values: output `i` at `i`, and a flag
//! at `n + i` that is above 0.5 for a Remote Control. A Remote Control's output is a position in
//! the target's normalized range; a Modulation's is added to it.

use crate::params::CellIx;
use std::sync::atomic::{AtomicU32, Ordering};

/// Values one engine's modulators can publish, all told.
pub const MAX_MOD_VALUES: usize = 4096;

/// Marks a device with nothing on the [`ModBus`].
pub const NO_MOD_BASE: u32 = u32::MAX;

/// One parameter a modulator output drives. Lives in the routes of the track that owns the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModRoute {
    value: u32,
    mode: u32,
    target: CellIx,
}

impl ModRoute {
    /// A route from the value at `value` on the bus, with its Remote Control flag at `mode`.
    pub fn new(value: u32, mode: u32, target: CellIx) -> Self {
        Self {
            value,
            mode,
            target,
        }
    }

    pub fn value(&self) -> u32 {
        self.value
    }

    pub fn mode(&self) -> u32 {
        self.mode
    }

    pub fn target(&self) -> CellIx {
        self.target
    }
}

/// The double-buffered values of every modulator in the engine. Not finite means the source is
/// switched off or hasn't run yet.
pub struct ModBus {
    prev: Box<[AtomicU32]>,
    front: Box<[AtomicU32]>,
    back: Box<[AtomicU32]>,
}

fn nan() -> AtomicU32 {
    AtomicU32::new(f32::NAN.to_bits())
}

impl Default for ModBus {
    fn default() -> Self {
        Self::new()
    }
}

impl ModBus {
    /// Call on the UI thread.
    pub fn new() -> Self {
        let silent = || {
            (0..MAX_MOD_VALUES)
                .map(|_| nan())
                .collect::<Box<[AtomicU32]>>()
        };
        Self {
            prev: silent(),
            front: silent(),
            back: silent(),
        }
    }

    /// A device's values for this block. A range past the end of the bus is ignored. Safe from
    /// any thread while the block renders.
    pub(crate) fn publish(&self, base: u32, values: &[f32]) {
        let start = base as usize;
        if let Some(to) = self.back.get(start..start + values.len()) {
            for (slot, v) in to.iter().zip(values) {
                slot.store(v.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// A source stops counting: the parameters it was moving go back to their knobs.
    pub(crate) fn switch_off(&self, base: u32, len: usize) {
        let start = base as usize;
        if let Some(to) = self.back.get(start..start + len) {
            for slot in to {
                slot.store(f32::NAN.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// Forget a range entirely, as a device is about to start publishing into it.
    pub(crate) fn clear(&mut self, base: u32, len: usize) {
        let start = base as usize;
        if start + len <= MAX_MOD_VALUES {
            for buffer in [&mut self.prev, &mut self.front, &mut self.back] {
                for slot in &mut buffer[start..start + len] {
                    *slot.get_mut() = f32::NAN.to_bits();
                }
            }
        }
    }

    pub(crate) fn clear_all(&mut self) {
        self.clear(0, MAX_MOD_VALUES);
    }

    /// The block is over: what was published in it becomes readable. Takes the bus exclusively,
    /// so it can only run with no worker in the middle of a block.
    pub(crate) fn end_block(&mut self) {
        std::mem::swap(&mut self.prev, &mut self.front);
        std::mem::swap(&mut self.front, &mut self.back);
        for (to, from) in self.back.iter_mut().zip(self.front.iter_mut()) {
            *to.get_mut() = *from.get_mut();
        }
    }

    /// The value at `at` of the way through the block, if the source is running.
    pub(crate) fn value(&self, slot: u32, at: f32) -> Option<f32> {
        let slot = slot as usize;
        let last = f32::from_bits(self.front.get(slot)?.load(Ordering::Relaxed));
        if !last.is_finite() {
            return None;
        }
        let before = f32::from_bits(self.prev[slot].load(Ordering::Relaxed));
        Some(if before.is_finite() {
            before + (last - before) * at
        } else {
            last
        })
    }

    pub(crate) fn flag(&self, slot: u32) -> bool {
        self.front
            .get(slot as usize)
            .is_some_and(|v| f32::from_bits(v.load(Ordering::Relaxed)) > 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_readable_only_from_the_next_block() {
        let mut bus = ModBus::new();
        bus.publish(0, &[0.75]);
        assert_eq!(bus.value(0, 1.0), None, "not this block");
        bus.end_block();
        assert_eq!(bus.value(0, 1.0), Some(0.75));
    }

    #[test]
    fn it_ramps_towards_what_was_published() {
        let mut bus = ModBus::new();
        bus.publish(0, &[0.0]);
        bus.end_block();
        bus.publish(0, &[1.0]);
        bus.end_block();
        assert_eq!(bus.value(0, 0.5), Some(0.5));
        assert_eq!(bus.value(0, 1.0), Some(1.0));
    }

    #[test]
    fn what_was_published_holds_until_replaced_or_switched_off() {
        let mut bus = ModBus::new();
        bus.publish(3, &[0.5]);
        bus.end_block();
        bus.end_block();
        assert_eq!(bus.value(3, 1.0), Some(0.5));
        bus.switch_off(3, 1);
        bus.end_block();
        assert_eq!(bus.value(3, 1.0), None);
    }

    #[test]
    fn rotating_does_not_allocate() {
        let mut bus = ModBus::new();
        rlive_testkit::assert_no_alloc(|| {
            bus.publish(10, &[0.1, 0.2]);
            bus.end_block();
        });
    }

    #[test]
    fn out_of_range_is_ignored() {
        let bus = ModBus::new();
        bus.publish(MAX_MOD_VALUES as u32 - 1, &[0.1, 0.2]);
        bus.switch_off(u32::MAX - 1, 4);
        assert_eq!(bus.value(u32::MAX, 0.0), None);
        assert!(!bus.flag(u32::MAX));
    }
}
