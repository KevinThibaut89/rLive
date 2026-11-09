//! The engine's multi-core behaviour that doesn't need a whole Set to show: idle devices going to
//! sleep without losing their tails, deadlines, the CPU meter and a stress test for data races.
//! (A whole Set rendering bit-identically is proved in `rlive-app`'s `session::multicore_tests`.)
#![allow(clippy::vec_box)]

use rlive_core::{DeviceId, MidiMessage, ProcessContext, Processor, Tail, ThreadPolicy, TrackId};
use rlive_engine::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn note(on: bool) -> MidiMessage {
    if on {
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
            note_id: 1,
            tuning: 0.0,
        }
    } else {
        MidiMessage::NoteOff {
            channel: 0,
            key: 60,
            velocity: 0.0,
            note_id: 1,
        }
    }
}

/// Rings for a set time after a note, counting the blocks it's called for.
struct Bell {
    ringing: u32,
    tail: Tail,
    level: f32,
    calls: Arc<AtomicU64>,
    sleeps: bool,
}

impl Bell {
    fn new(tail: Tail, calls: Arc<AtomicU64>) -> Self {
        Self {
            ringing: 0,
            tail,
            level: 0.25,
            calls,
            sleeps: true,
        }
    }
}

impl Processor for Bell {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        for e in ctx.events {
            if matches!(e.message, MidiMessage::NoteOn { .. }) {
                self.ringing = 2000;
            }
        }
        for (l, r) in ctx.audio.left.iter_mut().zip(ctx.audio.right.iter_mut()) {
            if self.ringing > 0 {
                let v = self.level * self.ringing as f32 / 2000.0;
                *l += v;
                *r += v;
                self.ringing -= 1;
            }
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
    fn tail(&self) -> Tail {
        self.tail
    }
    fn sleeps_when_silent(&self) -> bool {
        self.sleeps
    }
}

fn add_track(engine: &mut Engine, h: &mut EngineHandle, processor: Box<dyn Processor>) -> TrackId {
    let id = TrackId::new();
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track
        .devices
        .push(Box::new(DeviceSlot::new(DeviceId::new(), processor)));
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    let mut l = [0.0; BLOCK];
    let mut r = [0.0; BLOCK];
    engine.process_stereo(&mut l, &mut r);
    h.collect_garbage();
    id
}

/// A track holding a bell, and its one device's id.
fn bell_rig(workers: Option<usize>, bell: Bell) -> (Engine, EngineHandle, TrackId, DeviceId) {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    if let Some(n) = workers {
        engine.set_workers(WorkerPool::spawn(n, None));
    }
    h.shared.idle_sleep.store(true, Ordering::Relaxed);
    let id = TrackId::new();
    let dev = DeviceId::new();
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track
        .devices
        .push(Box::new(DeviceSlot::new(dev, Box::new(bell))));
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    (engine, h, id, dev)
}

fn direct(h: &mut EngineHandle, track: TrackId, device: DeviceId) {
    h.send(EngineCommand::graph(GraphOp::DirectNote {
        target: TrackRef::Track(track),
        device,
        message: note(true),
    }))
    .ok()
    .unwrap();
}

fn render(engine: &mut Engine, h: &mut EngineHandle, blocks: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    for _ in 0..blocks {
        engine.process_stereo(&mut l, &mut r);
        h.collect_garbage();
        out.extend_from_slice(&l);
    }
    out
}

/// About a second and a half, enough for any device's idle time to run out.
const LONG: usize = (1.5 * SR) as usize / BLOCK;

#[test]
fn a_device_with_nothing_to_do_goes_to_sleep_after_its_tail() {
    for workers in [None, Some(3)] {
        let calls = Arc::new(AtomicU64::new(0));
        let (mut engine, mut h, track, device) =
            bell_rig(workers, Bell::new(Tail::Frames(2000), calls.clone()));
        direct(&mut h, track, device);
        let out = render(&mut engine, &mut h, LONG);
        // Everything the tail played is there.
        assert!(out[1000] > 0.01, "the tail plays");
        assert!(out[1999] > 0.0, "the very end of the tail plays");
        let later = calls.load(Ordering::Relaxed);
        let more = render(&mut engine, &mut h, 20);
        assert!(more.iter().all(|s| *s == 0.0));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            later,
            "no more calls once it is asleep"
        );
        // And it wakes for the next note.
        direct(&mut h, track, device);
        let woken = render(&mut engine, &mut h, 4);
        assert!(woken.iter().any(|s| s.abs() > 0.01), "it plays again");
        assert!(calls.load(Ordering::Relaxed) > later);
    }
}

#[test]
fn going_to_sleep_loses_nothing_of_the_tail() {
    // The same note on an engine that may sleep and one that may not: not one sample differs.
    let run = |sleep: bool| {
        let calls = Arc::new(AtomicU64::new(0));
        let (mut engine, mut h, track, device) =
            bell_rig(None, Bell::new(Tail::Frames(2000), calls));
        h.shared.idle_sleep.store(sleep, Ordering::Relaxed);
        direct(&mut h, track, device);
        render(&mut engine, &mut h, LONG * 2)
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn an_unknown_tail_never_sleeps() {
    // A Tail::Infinite effect has told us it can't say; it is called for as long as we run.
    let calls = Arc::new(AtomicU64::new(0));
    let (mut engine, mut h, track, device) =
        bell_rig(None, Bell::new(Tail::Infinite, calls.clone()));
    direct(&mut h, track, device);
    render(&mut engine, &mut h, LONG);
    let at = calls.load(Ordering::Relaxed);
    render(&mut engine, &mut h, 20);
    assert_eq!(calls.load(Ordering::Relaxed), at + 20, "called every block");
}

#[test]
fn a_device_that_has_not_opted_in_stays_awake() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut bell = Bell::new(Tail::Frames(0), calls.clone());
    bell.sleeps = false;
    let (mut engine, mut h, _, _) = bell_rig(None, bell);
    render(&mut engine, &mut h, LONG);
    let at = calls.load(Ordering::Relaxed);
    render(&mut engine, &mut h, 20);
    assert_eq!(calls.load(Ordering::Relaxed), at + 20);
}

/// Burns `spin` per block, then leaves a value that depends only on how many blocks it's seen.
struct Slow {
    spin: std::time::Duration,
    index: u32,
    blocks: u64,
}

impl Processor for Slow {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let until = std::time::Instant::now() + self.spin;
        while std::time::Instant::now() < until {
            std::hint::spin_loop();
        }
        self.blocks += 1;
        let v = (self.index as f32 + 1.0) * 0.01 + (self.blocks % 7) as f32 * 0.001;
        ctx.audio.left.iter_mut().for_each(|s| *s += v);
        ctx.audio.right.iter_mut().for_each(|s| *s += v);
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn slow_rig(tracks: u32, spin_micros: u64, workers: Option<usize>) -> (Engine, EngineHandle) {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    if let Some(n) = workers {
        engine.set_workers(WorkerPool::spawn(n, None));
    }
    for index in 0..tracks {
        add_track(
            &mut engine,
            &mut h,
            Box::new(Slow {
                spin: std::time::Duration::from_micros(spin_micros),
                index,
                blocks: 0,
            }),
        );
    }
    (engine, h)
}

#[test]
fn a_slow_device_is_an_overload_but_never_changes_the_sound() {
    // One block is 5.3 ms long. A device taking 8 ms can't fit however many threads there are.
    let run = |workers: Option<usize>| {
        let (mut engine, mut h) = slow_rig(1, 8_000, workers);
        let out = render(&mut engine, &mut h, 4);
        (out, h.shared.cpu_overloads.load(Ordering::Relaxed))
    };
    let (serial, overloads) = run(None);
    assert_eq!(
        overloads,
        4 + 1,
        "every block overran (one more to add the track)"
    );
    let (threaded, overloads) = run(Some(3));
    assert!(overloads >= 4);
    assert_eq!(serial, threaded, "a late block is still the right block");
}

#[test]
fn work_spread_over_threads_meets_a_deadline_one_thread_would_miss() {
    // Eight tracks of 2 ms each: 16 ms on one thread, against a block of 5.3 ms.
    // Timing on a busy machine is rough, so this asks for much less than the real gain.
    let run = |workers: Option<usize>| {
        let (mut engine, mut h) = slow_rig(8, 2_000, workers);
        render(&mut engine, &mut h, 20);
        let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
        let before = std::time::Instant::now();
        for _ in 0..20 {
            engine.process_stereo(&mut l, &mut r);
        }
        before.elapsed().as_secs_f64() / 20.0
    };
    let serial = run(None);
    let spread = run(Some(3));
    assert!(serial > 0.014, "one thread does it all: {serial}");
    assert!(
        spread < serial * 0.75,
        "threads share it: {spread} against {serial}"
    );
}

#[test]
fn a_block_finishes_without_any_worker_at_all() {
    // Workers that never wake: the callback does all the work itself, as a deadline fallback.
    let (mut engine, mut h) = slow_rig(4, 0, Some(2));
    engine.workers().unwrap().set_wakes(false);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    let before = std::time::Instant::now();
    for _ in 0..100 {
        engine.process_stereo(&mut l, &mut r);
    }
    h.collect_garbage();
    assert!(
        before.elapsed().as_millis() < 500,
        "the callback isn't waiting for sleeping workers"
    );
}

#[test]
fn the_cpu_meter_reports_average_current_and_overload() {
    let (mut engine, mut h) = slow_rig(1, 2_500, None);
    render(&mut engine, &mut h, 30);
    // About half the deadline.
    let average = h.shared.cpu_load.load();
    assert!((0.3..0.9).contains(&average), "average {average}");
    let current = h.shared.cpu_current.take();
    assert!((0.3..1.2).contains(&current), "current {current}");
    assert_eq!(
        h.shared.cpu_current.take(),
        0.0,
        "it is since the last look"
    );
    assert_eq!(h.shared.cpu_overloads.load(Ordering::Relaxed), 0);
}

/// Makes a noise that depends on a seed, so a track racing another would corrupt the sum.
struct Hash {
    seed: u32,
    phase: u32,
}

impl Processor for Hash {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for (l, r) in ctx.audio.left.iter_mut().zip(ctx.audio.right.iter_mut()) {
            self.phase = self.phase.wrapping_mul(2654435761).wrapping_add(self.seed);
            let v = (self.phase >> 16) as f32 / 65536.0 * 0.02;
            *l += v;
            *r -= v;
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Thousands of blocks, with tracks coming and going under the workers, against a serial engine:
/// a race in the scheduler or between tracks shows up as a difference, or a crash under
/// `ThreadSanitizer`, which this test is written to run usefully under.
#[test]
fn stress_many_short_blocks_with_edits_match_the_serial_engine() {
    fn hash_track(seed: u32) -> Box<TrackNode> {
        let mut node = Box::new(TrackNode::new(TrackId::new(), BLOCK));
        node.devices.push(Box::new(DeviceSlot::new(
            DeviceId::new(),
            Box::new(Hash { seed, phase: 0 }),
        )));
        node
    }
    let (mut a, mut ha) = create_engine(SR, BLOCK);
    let (mut b, mut hb) = create_engine(SR, BLOCK);
    b.set_workers(WorkerPool::spawn(3, None));
    for seed in 1..=16 {
        ha.send(EngineCommand::graph(GraphOp::AddTrack {
            index: 0,
            track: hash_track(seed),
        }))
        .ok()
        .unwrap();
        hb.send(EngineCommand::graph(GraphOp::AddTrack {
            index: 0,
            track: hash_track(seed),
        }))
        .ok()
        .unwrap();
    }
    let mut seed = 16u32;
    let (mut la, mut ra) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    let (mut lb, mut rb) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    for i in 0..4000 {
        if i % 200 == 0 {
            seed += 1;
            ha.send(EngineCommand::graph(GraphOp::AddTrack {
                index: 3,
                track: hash_track(seed),
            }))
            .ok()
            .unwrap();
            hb.send(EngineCommand::graph(GraphOp::AddTrack {
                index: 3,
                track: hash_track(seed),
            }))
            .ok()
            .unwrap();
        }
        rlive_testkit::assert_no_alloc(|| a.process_stereo(&mut la, &mut ra));
        rlive_testkit::assert_no_alloc(|| b.process_stereo(&mut lb, &mut rb));
        assert_eq!(la, lb, "block {i}");
        assert_eq!(ra, rb, "block {i}");
        ha.collect_garbage();
        hb.collect_garbage();
    }
}

#[test]
fn a_plan_with_a_bus_two_tracks_claim_runs_serially() {
    fn tracks() -> Vec<Box<TrackNode>> {
        (0..2)
            .map(|_| Box::new(TrackNode::new(TrackId::new(), BLOCK)))
            .collect()
    }
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    engine.set_workers(WorkerPool::spawn(2, None));
    let mut t = tracks();
    let (a, b) = (t.remove(0), t.remove(0));
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: a,
    }))
    .ok()
    .unwrap();
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 1,
        track: b,
    }))
    .ok()
    .unwrap();
    let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    assert!(engine.renders_on_workers());
    // Two tap-outs on the same bus, from different tracks: the engine can't trust that.
    let buses = Box::new(vec![
        taps::TapBus::new(BLOCK, 1, 0),
        taps::TapBus::new(BLOCK, 1, 0),
    ]);
    let plan = RenderPlan {
        steps: vec![
            Step {
                track: 0,
                inputs: Vec::new(),
            },
            Step {
                track: 1,
                inputs: Vec::new(),
            },
            Step {
                track: 2,
                inputs: vec![BusIn::post_mixer(0), BusIn::post_mixer(1)],
            },
        ],
        levels: vec![0..2, 2..3],
        buses: 2,
        tap_outs: vec![
            TapOut {
                track: 0,
                tap: Tap::PostMixer,
                bus: 0,
            },
            TapOut {
                track: 1,
                tap: Tap::PostMixer,
                bus: 0,
            },
        ],
        ..RenderPlan::default()
    };
    h.send(EngineCommand::Graph(GraphTxn::new(
        vec![GraphOp::SetBuses(buses)],
        Some(plan),
    )))
    .ok()
    .unwrap();
    engine.process_stereo(&mut l, &mut r);
    assert!(engine.has_compiled_plan(), "the plan is running");
    assert!(
        !engine.renders_on_workers(),
        "a plan that could race stays on one thread"
    );
}

/// Remembers every thread it was run on.
struct ThreadProbe {
    seen: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>,
    policy: ThreadPolicy,
}

impl Processor for ThreadProbe {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, _: &mut ProcessContext) {
        self.seen.lock().unwrap().push(std::thread::current().id());
    }
    fn set_param(&mut self, _: u32, _: f64) {}
    fn thread_policy(&self) -> ThreadPolicy {
        self.policy
    }
}

#[test]
fn a_track_holding_a_plugin_that_needs_it_stays_on_the_audio_thread() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    engine.set_workers(WorkerPool::spawn(3, None));
    let pinned = Arc::new(std::sync::Mutex::new(Vec::new()));
    let free = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Enough other work that a free device can't just be taken by the caller every time.
    for index in 0..8 {
        add_track(
            &mut engine,
            &mut h,
            Box::new(Slow {
                spin: std::time::Duration::from_micros(300),
                index,
                blocks: 0,
            }),
        );
    }
    add_track(
        &mut engine,
        &mut h,
        Box::new(ThreadProbe {
            seen: pinned.clone(),
            policy: ThreadPolicy::AudioThread,
        }),
    );
    add_track(
        &mut engine,
        &mut h,
        Box::new(ThreadProbe {
            seen: free.clone(),
            policy: ThreadPolicy::AnyWorker,
        }),
    );
    let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    for _ in 0..400 {
        engine.process_stereo(&mut l, &mut r);
    }
    let me = std::thread::current().id();
    let pinned = pinned.lock().unwrap();
    assert_eq!(pinned.len(), 400 + 2, "it ran every block");
    assert!(
        pinned.iter().all(|t| *t == me),
        "a pinned device never moves"
    );
    let free = free.lock().unwrap();
    assert!(
        free.iter().any(|t| *t != me),
        "an unpinned device can run on a worker"
    );
}

/// A data race on purpose, kept out of normal runs. Under ThreadSanitizer this must report a
/// warning: it is how to tell the sanitizer works on this toolchain.
#[test]
#[ignore = "only meaningful under ThreadSanitizer"]
fn tsan_canary_reports_a_deliberate_race() {
    struct Racy(std::cell::UnsafeCell<u64>);
    // SAFETY: not safe at all; that is the point of this test.
    unsafe impl Sync for Racy {}
    let racy = Arc::new(Racy(std::cell::UnsafeCell::new(0)));
    let writers: Vec<_> = (0..4)
        .map(|_| {
            let r = racy.clone();
            std::thread::spawn(move || {
                for _ in 0..100_000 {
                    // SAFETY: this is the bug.
                    unsafe { *r.0.get() += 1 };
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
}
