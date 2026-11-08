//! Test helpers for rLive: offline rendering, signal generators, measurements and an
//! audio-thread allocation guard.
//!
//! Use this crate only as a dev-dependency: linking it installs a counting global allocator
//! (see [`alloc_guard`]).

pub mod alloc_guard;
pub mod measure;
pub mod render;
pub mod signal;

pub use alloc_guard::{assert_no_alloc, count_allocs, AllocCounts};
pub use measure::{max_step, onset, peak, rms, tone_amplitude};
pub use render::{render_chunks, render_processor, Harness};
pub use signal::{dc, impulse, silence, sine};

#[global_allocator]
static ALLOCATOR: alloc_guard::CountingAllocator = alloc_guard::CountingAllocator;
