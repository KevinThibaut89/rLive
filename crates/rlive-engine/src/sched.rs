//! The multi-core scheduler.
//!
//! A block is rendered in a few *phases*: rendering every track's clips, then the steps of each
//! plan level. Tracks in one phase are independent of each other, so one phase is a *job*: a
//! number of tasks that any thread may run in any order. [`WorkerPool::scatter`] hands a job to the
//! pool's threads and takes part itself.
//!
//! # How a job gets out
//!
//! The job is published as one 64-bit word: a generation, how many tasks have been claimed, and
//! how many there are. Claiming a task is a compare-and-swap on that word, so it is wait-free per
//! attempt and there is no queue to build or lock. Whoever claims task `i` runs it, then adds one
//! to `done`. Tasks are identified by a plain index into data the job already holds.
//!
//! # The thread that calls `scatter` is a worker
//!
//! That thread is the audio callback: it publishes the job and then claims tasks itself until
//! there are none left to claim. A worker that is asleep has claimed nothing, so it can never hold
//! up the block: the callback simply does the task the sleeper would have done. The only wait in
//! this design is for a task some worker has already claimed and is running, and that wait is
//! bounded by the time the task takes.
//!
//! # Waking
//!
//! A worker spins for [`SPIN`] after its last task, to catch the next phase of the same block
//! without paying for a wake, and then sleeps on a semaphore. The callback posts the semaphore
//! when it publishes a job, so waking costs one syscall on the audio thread and nothing else.
//! Both sides use sequentially consistent operations on `word` and `sleeping`, so a worker
//! going to sleep and a job being published can't miss each other.
//!
//! # What runs where
//!
//! The audio callback and the workers use the pool in the same way, but a task that holds a
//! plug-in which needs a consistent thread only ever runs on the callback: the caller partitions
//! those out and runs them itself ([`crate::Engine`]). See [`rlive_core::ThreadPolicy`].

use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a worker keeps looking for the next job before going to sleep.
pub const SPIN: Duration = Duration::from_micros(50);
/// Most pool threads, not counting the audio callback.
pub const MAX_WORKERS: usize = 31;
/// How long a waiting thread spins hard before it starts handing the core back now and then.
const YIELD_AFTER: Duration = Duration::from_micros(200);
/// A sleeping worker gets up this often regardless, in case a wake was lost.
const SLEEP_TIMEOUT: Duration = Duration::from_millis(50);

/// Keeps something on a cache line of its own (Apple silicon's are 128 bytes).
#[repr(align(128))]
#[derive(Default)]
struct Padded<T>(T);

/// A task's code and what it runs on: a monomorphised [`thunk`] and the closure it calls.
#[derive(Clone, Copy)]
struct Job {
    data: *const (),
    run: unsafe fn(*const (), usize),
}

impl Default for Job {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            run: nothing,
        }
    }
}

// SAFETY: a `Job` only moves between threads by way of `Shared`, and `scatter` keeps what it points
// at alive and `Sync` until every claimed task of the job has reported done.
unsafe impl Send for Job {}

unsafe fn nothing(_: *const (), _: usize) {}

unsafe fn thunk<F: Fn(usize) + Sync>(data: *const (), i: usize) {
    // SAFETY: `data` is the closure `scatter` is holding a borrow of until the job is done.
    unsafe { (*(data as *const F))(i) }
}

/// One published job: `gen << 32 | next << 16 | count`.
const fn pack(gen: u32, next: u32, count: u32) -> u64 {
    (gen as u64) << 32 | (next as u64) << 16 | count as u64
}

const fn generation(word: u64) -> u32 {
    (word >> 32) as u32
}

const fn next(word: u64) -> u32 {
    (word >> 16) as u32 & 0xffff
}

const fn count(word: u64) -> u32 {
    word as u32 & 0xffff
}

struct Shared {
    /// The published job, see [`pack`]. A new job only goes up once the old one is fully done.
    word: Padded<AtomicU64>,
    /// How many tasks of the current job have finished.
    done: Padded<AtomicU32>,
    /// Written by the publisher while no job is outstanding; only read after a successful claim.
    job: UnsafeCell<Job>,
    stop: AtomicBool,
    /// Workers that have promised to check `word` once more before sleeping.
    sleeping: Padded<AtomicU32>,
    waker: Waker,
    /// How many tasks each thread ran, for diagnostics. The caller is the last entry.
    ran: Box<[Padded<AtomicU64>]>,
    /// When false, publishing a job wakes nobody. Tests use it to play a worker that overslept.
    wake: AtomicBool,
}

// SAFETY: `job` is the only field that isn't `Sync` by itself. It is written only by the single
// thread calling `scatter` while no task is claimed, and read only after a claim that synchronizes
// with its publication, and before that task's `done` is reported.
unsafe impl Sync for Shared {}
// SAFETY: as above; nothing in it is tied to a thread.
unsafe impl Send for Shared {}

/// A job's tasks that some thread has started but not finished, and what to do when dropped
/// (a task that panics must still count as done, so the caller doesn't wait forever).
struct Finish<'a> {
    shared: &'a Shared,
}

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        self.shared.done.0.fetch_add(1, Ordering::AcqRel);
    }
}

/// Take the next task of job `gen`, if it is still current and has any left.
fn claim(shared: &Shared, gen: u32) -> Option<(usize, Job)> {
    let mut word = shared.word.0.load(Ordering::Acquire);
    loop {
        if generation(word) != gen || next(word) >= count(word) {
            return None;
        }
        match shared.word.0.compare_exchange_weak(
            word,
            word + (1 << 16),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // SAFETY: we hold a claim on this generation, so the publisher won't touch `job`
                // again until we report done. The claim above synchronizes with its publication.
                let job = unsafe { *shared.job.get() };
                return Some((next(word) as usize, job));
            }
            Err(now) => word = now,
        }
    }
}

/// Run a claimed task and report it done, even if it panics.
fn run_claimed(shared: &Shared, thread: usize, task: usize, job: Job) {
    let _finish = Finish { shared };
    // SAFETY: `scatter` keeps the closure alive until the job is reported done, and the closure is
    // written to be run from any thread for any index below `count`.
    unsafe { (job.run)(job.data, task) };
    if let Some(ran) = shared.ran.get(thread) {
        ran.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn worker_loop(shared: &Shared, thread: usize) {
    let mut seen = 0;
    'next_job: loop {
        let word = wait_for_job(shared, seen, thread);
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        seen = generation(word);
        while let Some((task, job)) = claim(shared, seen) {
            run_claimed(shared, thread, task, job);
        }
        continue 'next_job;
    }
}

/// Block until a job newer than `seen` is up, or the pool is stopping. Spins first.
fn wait_for_job(shared: &Shared, seen: u32, thread: usize) -> u64 {
    let spin_until = Instant::now() + SPIN;
    loop {
        for _ in 0..64 {
            let word = shared.word.0.load(Ordering::Acquire);
            if generation(word) != seen || shared.stop.load(Ordering::Relaxed) {
                return word;
            }
            std::hint::spin_loop();
        }
        if Instant::now() >= spin_until {
            break;
        }
    }
    // Sleep until a job comes, checking once more after saying so.
    loop {
        shared.sleeping.0.fetch_add(1, Ordering::SeqCst);
        let word = shared.word.0.load(Ordering::SeqCst);
        if generation(word) != seen || shared.stop.load(Ordering::SeqCst) {
            shared.sleeping.0.fetch_sub(1, Ordering::SeqCst);
            return word;
        }
        shared.waker.sleep(thread, SLEEP_TIMEOUT);
        shared.sleeping.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What a worker does on its own thread, either side of the work: promote itself to real time and
/// so on before it can take a task, and undo that when it stops.
#[derive(Clone)]
pub struct WorkerHooks {
    pub start: Arc<dyn Fn(usize) + Send + Sync>,
    pub stop: Option<Arc<dyn Fn(usize) + Send + Sync>>,
}

/// A set of threads that help the audio callback render. Built and dropped on the UI thread.
pub struct WorkerPool {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// The generation of the job now up.
    gen: Cell<u32>,
}

// SAFETY: `gen` is touched only by the one thread that owns the pool and calls `scatter`; the
// engine only ever hands the pool between threads whole, while nothing else uses it.
unsafe impl Send for WorkerPool {}

impl WorkerPool {
    /// `threads` pool threads, or `None` if they can't be had.
    pub fn spawn(threads: usize, hooks: Option<WorkerHooks>) -> Option<Box<Self>> {
        let threads = threads.min(MAX_WORKERS);
        if threads == 0 {
            return None;
        }
        let shared = Arc::new(Shared {
            word: Padded(AtomicU64::new(0)),
            done: Padded(AtomicU32::new(0)),
            job: UnsafeCell::new(Job::default()),
            stop: AtomicBool::new(false),
            sleeping: Padded(AtomicU32::new(0)),
            waker: Waker::new(threads)?,
            ran: (0..=threads)
                .map(|_| Padded(AtomicU64::new(0)))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            wake: AtomicBool::new(true),
        });
        let mut handles = Vec::with_capacity(threads);
        for i in 0..threads {
            let thread_shared = shared.clone();
            let hooks = hooks.clone();
            let handle = std::thread::Builder::new()
                .name(format!("rlive-audio-worker-{i}"))
                .spawn(move || {
                    if let Some(hooks) = &hooks {
                        (hooks.start)(i);
                    }
                    worker_loop(&thread_shared, i);
                    if let Some(stop) = hooks.as_ref().and_then(|h| h.stop.as_ref()) {
                        stop(i);
                    }
                })
                .ok()?;
            shared.waker.register(i, handle.thread().clone());
            handles.push(handle);
        }
        Some(Box::new(Self {
            shared,
            threads: handles,
            gen: Cell::new(0),
        }))
    }

    /// How many threads the pool has, not counting the audio callback.
    pub fn threads(&self) -> usize {
        self.threads.len()
    }

    /// How many tasks each thread has run: the pool's threads, then the caller.
    pub fn tasks_run(&self) -> Vec<u64> {
        self.shared
            .ran
            .iter()
            .map(|r| r.0.load(Ordering::Relaxed))
            .collect()
    }

    /// Whether publishing a job wakes workers that are asleep. Off, they stay asleep until the
    /// timeout. Tests use this to check that a sleeper can't hold the block up.
    #[doc(hidden)]
    pub fn set_wakes(&self, wake: bool) {
        self.shared.wake.store(wake, Ordering::Relaxed);
    }

    /// Run `task(0)`, `task(1)`, ... up to `task(count - 1)`, in any order, on the pool's threads
    /// and on this one, and return when every one has finished. `task` must be safe to call on
    /// several threads at once with different indices.
    pub fn scatter<F: Fn(usize) + Sync>(&self, count: usize, task: &F) {
        self.scatter_after(count, task, || {});
    }

    /// [`scatter`](Self::scatter), but this thread runs `first` once the job is out, before it
    /// starts claiming tasks. For work that has to run on this thread and no other.
    pub fn scatter_after<F: Fn(usize) + Sync>(&self, count: usize, task: &F, first: impl FnOnce()) {
        if count == 0 {
            first();
            return;
        }
        if count == 1 || self.threads.is_empty() {
            first();
            (0..count).for_each(task);
            return;
        }
        debug_assert!(count <= 0xffff, "more tasks than a job word can count");
        let shared = &*self.shared;
        let me = self.threads.len();
        // SAFETY: no task of the previous job can still be claimed or running: the last call
        // returned only once all of them had reported done, and we hold `&self` alone.
        unsafe {
            *shared.job.get() = Job {
                data: task as *const F as *const (),
                run: thunk::<F>,
            };
        }
        shared.done.0.store(0, Ordering::Relaxed);
        let gen = self.gen.get().wrapping_add(1).max(1);
        self.gen.set(gen);
        shared
            .word
            .0
            .store(pack(gen, 0, count as u32), Ordering::SeqCst);
        // Waiting past here is only ever for a task someone has claimed; see the module docs.
        let wait = WaitDone {
            shared,
            count: count as u32,
        };
        let sleeping = shared.sleeping.0.load(Ordering::SeqCst) as usize;
        if sleeping > 0 && shared.wake.load(Ordering::Relaxed) {
            shared.waker.wake(sleeping.min(count - 1));
        }
        first();
        while let Some((i, job)) = claim(shared, gen) {
            run_claimed(shared, me, i, job);
        }
        drop(wait);
    }
}

/// Spins until a job is fully done, even while a panic is unwinding past `scatter`.
struct WaitDone<'a> {
    shared: &'a Shared,
    count: u32,
}

impl Drop for WaitDone<'_> {
    fn drop(&mut self) {
        let started = Instant::now();
        let mut spins = 0u32;
        while self.shared.done.0.load(Ordering::Acquire) < self.count {
            std::hint::spin_loop();
            spins = spins.wrapping_add(1);
            // A worker holding a task may be waiting for this very core. After a moment of
            // spinning, hand the core back now and then so it can finish.
            if spins.is_multiple_of(4096) && started.elapsed() > YIELD_AFTER {
                std::thread::yield_now();
            }
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.waker.wake(self.threads.len());
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

#[cfg(target_os = "macos")]
mod waker {
    //! Mach semaphores: the same primitive CoreAudio and JUCE use. Posting one is a single trap
    //! that does not allocate, take a lock or block, so it is safe from the audio thread.
    use std::time::Duration;

    #[allow(non_camel_case_types)]
    type mach_port_t = u32;
    const SYNC_POLICY_FIFO: i32 = 0;

    #[repr(C)]
    struct MachTimespec {
        tv_sec: u32,
        tv_nsec: i32,
    }

    extern "C" {
        static mach_task_self_: mach_port_t;
        fn semaphore_create(
            task: mach_port_t,
            semaphore: *mut mach_port_t,
            policy: i32,
            value: i32,
        ) -> i32;
        fn semaphore_destroy(task: mach_port_t, semaphore: mach_port_t) -> i32;
        fn semaphore_signal(semaphore: mach_port_t) -> i32;
        fn semaphore_timedwait(semaphore: mach_port_t, wait: MachTimespec) -> i32;
    }

    /// Workers sleep on one counting semaphore.
    pub(super) struct Waker(mach_port_t);

    impl Waker {
        pub(super) fn new(_workers: usize) -> Option<Self> {
            let mut sem = 0;
            // SAFETY: `sem` is valid to write to; the task port is always ours.
            let result =
                unsafe { semaphore_create(mach_task_self_, &mut sem, SYNC_POLICY_FIFO, 0) };
            (result == 0).then_some(Self(sem))
        }

        /// A worker thread joined the pool. Mach semaphores don't need to know.
        pub(super) fn register(&self, _worker: usize, _thread: std::thread::Thread) {}

        pub(super) fn wake(&self, n: usize) {
            for _ in 0..n {
                // SAFETY: the port lives until this struct is dropped.
                unsafe { semaphore_signal(self.0) };
            }
        }

        pub(super) fn sleep(&self, _worker: usize, timeout: Duration) {
            let wait = MachTimespec {
                tv_sec: timeout.as_secs() as u32,
                tv_nsec: timeout.subsec_nanos() as i32,
            };
            // SAFETY: as above. Timing out and being woken are the same to the caller.
            unsafe { semaphore_timedwait(self.0, wait) };
        }
    }

    impl Drop for Waker {
        fn drop(&mut self) {
            // SAFETY: nothing can be using it any more; workers have been joined.
            unsafe { semaphore_destroy(mach_task_self_, self.0) };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod waker {
    //! Elsewhere, parking: a futex on Linux and Windows, so waking takes no lock.
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use std::thread::Thread;
    use std::time::Duration;

    struct Slot {
        thread: OnceLock<Thread>,
        asleep: AtomicBool,
    }

    pub(super) struct Waker(Box<[Slot]>);

    impl Waker {
        pub(super) fn new(workers: usize) -> Option<Self> {
            Some(Self(
                (0..workers)
                    .map(|_| Slot {
                        thread: OnceLock::new(),
                        asleep: AtomicBool::new(false),
                    })
                    .collect(),
            ))
        }

        pub(super) fn register(&self, worker: usize, thread: Thread) {
            let _ = self.0[worker].thread.set(thread);
        }

        pub(super) fn wake(&self, mut n: usize) {
            for slot in self.0.iter() {
                if n == 0 {
                    return;
                }
                if slot.asleep.load(Ordering::SeqCst) {
                    if let Some(t) = slot.thread.get() {
                        t.unpark();
                        n -= 1;
                    }
                }
            }
        }

        pub(super) fn sleep(&self, worker: usize, timeout: Duration) {
            self.0[worker].asleep.store(true, Ordering::SeqCst);
            std::thread::park_timeout(timeout);
            self.0[worker].asleep.store(false, Ordering::SeqCst);
        }
    }
}

use waker::Waker;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn pool(threads: usize) -> Box<WorkerPool> {
        WorkerPool::spawn(threads, None).expect("threads")
    }

    #[test]
    fn every_task_runs_exactly_once() {
        let pool = pool(4);
        for count in [1usize, 2, 3, 17, 257] {
            for _ in 0..50 {
                let counts: Vec<AtomicUsize> = (0..count).map(|_| AtomicUsize::new(0)).collect();
                pool.scatter(count, &|i| {
                    counts[i].fetch_add(1, Ordering::Relaxed);
                });
                assert!(counts.iter().all(|c| c.load(Ordering::Relaxed) == 1));
            }
        }
    }

    #[test]
    fn a_job_waits_for_every_task_it_was_given() {
        let pool = pool(3);
        let done = AtomicUsize::new(0);
        pool.scatter(32, &|_| {
            std::thread::sleep(Duration::from_micros(200));
            done.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(done.load(Ordering::SeqCst), 32);
    }

    #[test]
    fn consecutive_jobs_are_separate() {
        // A worker that loses the race for a task must not run a stale job's closure later.
        let pool = pool(6);
        for round in 0..2000u32 {
            let seen = AtomicUsize::new(0);
            pool.scatter(4, &|_| {
                seen.fetch_add(round as usize + 1, Ordering::SeqCst);
            });
            assert_eq!(seen.load(Ordering::SeqCst), 4 * (round as usize + 1));
        }
    }

    #[test]
    fn workers_really_do_some_of_it() {
        let pool = pool(4);
        // Long enough tasks that the sleepers have time to wake and claim some.
        for _ in 0..64 {
            pool.scatter(64, &|_| {
                let until = Instant::now() + Duration::from_micros(50);
                while Instant::now() < until {
                    std::hint::spin_loop();
                }
            });
        }
        let ran = pool.tasks_run();
        assert_eq!(ran.iter().sum::<u64>(), 64 * 64);
        assert!(
            ran[..4].iter().any(|n| *n > 0),
            "workers should have taken tasks: {ran:?}"
        );
    }

    #[test]
    fn dropping_the_pool_stops_every_thread() {
        let threads = pool(8);
        drop(threads);
    }

    #[test]
    fn a_worker_that_never_wakes_cannot_hold_up_the_job() {
        // No wakes: workers asleep stay asleep, and the caller does the lot.
        let pool = pool(2);
        std::thread::sleep(Duration::from_millis(5));
        pool.set_wakes(false);
        let started = Instant::now();
        let done = AtomicUsize::new(0);
        for _ in 0..100 {
            pool.scatter(8, &|_| {
                done.fetch_add(1, Ordering::Relaxed);
            });
        }
        assert_eq!(done.load(Ordering::Relaxed), 800);
        // Nowhere near the sleep timeout, per block.
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn scatter_does_not_allocate_on_the_caller() {
        let pool = pool(2);
        let n = AtomicUsize::new(0);
        rlive_testkit::assert_no_alloc(|| {
            for _ in 0..100 {
                pool.scatter(8, &|_| {
                    n.fetch_add(1, Ordering::Relaxed);
                });
            }
        });
        assert_eq!(n.load(Ordering::Relaxed), 800);
    }

    #[test]
    fn a_panicking_task_does_not_hang_the_job() {
        let pool = pool(3);
        let done = AtomicUsize::new(0);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.scatter(16, &|i| {
                if i == 7 {
                    panic!("device blew up");
                }
                done.fetch_add(1, Ordering::Relaxed);
            });
        }));
        assert!(r.is_err() || done.load(Ordering::Relaxed) <= 15);
    }

    #[test]
    fn a_pool_of_none_is_no_pool() {
        assert!(WorkerPool::spawn(0, None).is_none());
    }
}
