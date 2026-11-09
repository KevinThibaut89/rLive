//! Catches allocation on the audio thread. A counting wrapper around the system allocator,
//! because `assert_no_alloc` is not available offline.
//!
//! Only the calling thread is watched, and only inside [`count_allocs`] or
//! [`assert_no_alloc`], so unrelated test threads can't cause false failures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    // Const-initialised and without destructors, so touching these never allocates.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static FREES: Cell<u64> = const { Cell::new(0) };
}

thread_local! {
    /// This thread is a helper the code under test started, counted in [`helper_heap_touches`].
    static HELPER: Cell<bool> = const { Cell::new(false) };
}

static HELPER_TOUCHES: AtomicU64 = AtomicU64::new(0);

fn note(counter: &'static std::thread::LocalKey<Cell<u64>>) {
    if WATCHING.try_with(Cell::get).unwrap_or(false) {
        let _ = counter.try_with(|c| c.set(c.get() + 1));
    }
    if HELPER.try_with(Cell::get).unwrap_or(false) {
        HELPER_TOUCHES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Mark this thread as a helper of the code under test, such as a real-time worker: from now on
/// any allocation or free it makes is counted in [`helper_heap_touches`].
pub fn watch_helper_thread() {
    HELPER.set(true);
}

/// Stop counting this thread, for what it does as it winds down.
pub fn unwatch_helper_thread() {
    HELPER.set(false);
}

/// How many times any thread marked with [`watch_helper_thread`] has touched the heap.
pub fn helper_heap_touches() -> u64 {
    HELPER_TOUCHES.load(Ordering::Relaxed)
}

/// The system allocator, counting what the watched thread does.
pub struct CountingAllocator;

// SAFETY: every call is forwarded to `System` with the same arguments; the counters neither
// allocate nor touch the returned memory.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(&ALLOCS);
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(&ALLOCS);
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(&ALLOCS);
        System.realloc(ptr, layout, new_size)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note(&FREES);
        System.dealloc(ptr, layout)
    }
}

/// How much a piece of code touched the heap on its own thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocCounts {
    /// Allocations and reallocations.
    pub allocs: u64,
    pub frees: u64,
}

/// Restores the previous watch state, even if the watched code panics.
struct Watch {
    was_watching: bool,
    allocs: u64,
    frees: u64,
}

impl Watch {
    fn start() -> Self {
        let watch = Watch {
            was_watching: WATCHING.get(),
            allocs: ALLOCS.get(),
            frees: FREES.get(),
        };
        ALLOCS.set(0);
        FREES.set(0);
        WATCHING.set(true);
        watch
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // What this watch saw also counts towards any watch around it.
        WATCHING.set(self.was_watching);
        ALLOCS.set(self.allocs + ALLOCS.get());
        FREES.set(self.frees + FREES.get());
    }
}

/// Runs `f` and reports what it allocated and freed on this thread.
pub fn count_allocs<R>(f: impl FnOnce() -> R) -> (R, AllocCounts) {
    let watch = Watch::start();
    let result = f();
    let counts = AllocCounts {
        allocs: ALLOCS.get(),
        frees: FREES.get(),
    };
    drop(watch);
    (result, counts)
}

/// Runs `f`, panicking if it allocated or freed memory on this thread.
#[track_caller]
pub fn assert_no_alloc<R>(f: impl FnOnce() -> R) -> R {
    let (result, counts) = count_allocs(f);
    assert_eq!(
        counts,
        AllocCounts::default(),
        "the code under test touched the heap"
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    #[test]
    fn counts_allocations_and_frees() {
        let (_, counts) = count_allocs(|| {
            let v = black_box(vec![1u8; 64]);
            drop(v);
        });
        assert_eq!(
            counts,
            AllocCounts {
                allocs: 1,
                frees: 1
            }
        );
    }

    #[test]
    fn passes_code_that_leaves_the_heap_alone() {
        let mut buf = [0.0f32; 16];
        let sum = assert_no_alloc(|| {
            for (i, s) in buf.iter_mut().enumerate() {
                *s = i as f32;
            }
            black_box(&buf).iter().sum::<f32>()
        });
        assert_eq!(sum, 120.0);
    }

    #[test]
    #[should_panic(expected = "touched the heap")]
    fn fails_code_that_allocates() {
        assert_no_alloc(|| black_box(Vec::<u8>::with_capacity(16)));
    }

    #[test]
    #[should_panic(expected = "touched the heap")]
    fn fails_code_that_frees() {
        let v = black_box(vec![1u8; 16]);
        assert_no_alloc(move || drop(v));
    }

    #[test]
    fn ignores_other_threads() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let (go, done) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (worker_go, worker_done) = (go.clone(), done.clone());
        let worker = std::thread::spawn(move || {
            while !worker_go.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            black_box(vec![1u8; 64]);
            worker_done.store(true, Ordering::Release);
        });
        let (_, counts) = count_allocs(|| {
            go.store(true, Ordering::Release);
            while !done.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
        });
        worker.join().unwrap();
        assert_eq!(counts, AllocCounts::default());
    }

    #[test]
    fn a_nested_watch_also_counts_towards_the_outer_one() {
        let (_, outer) = count_allocs(|| {
            let a = black_box(vec![1u8; 8]);
            let (_, inner) = count_allocs(|| drop(black_box(vec![2u8; 8])));
            assert_eq!(
                inner,
                AllocCounts {
                    allocs: 1,
                    frees: 1
                }
            );
            drop(a);
        });
        assert_eq!(
            outer,
            AllocCounts {
                allocs: 2,
                frees: 2
            }
        );
    }
}
