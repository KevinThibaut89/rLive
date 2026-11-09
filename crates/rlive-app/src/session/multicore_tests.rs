//! The multi-core proofs (milestone A13): a Set with groups, sends, sidechains, delay
//! compensation, modulators and MIDI output renders bit-for-bit the same on any number of threads,
//! and a storm of random edits during playback changes nothing about that and allocates nothing.
//!
//! Offline rendering (Freeze, Bounce, Export) runs on a private engine with no pool, so it never
//! multi-threads and needs no separate proof; see `session/render.rs`.

use super::*;
use rlive_core::param::{ParamOwner, ParamRef};
use rlive_core::{MidiMessage, ParamId, ParamKind, TimeSignature, PPQ};
use rlive_devices::devices::b2::lfo;
use rlive_devices::effects::compressor;
use rlive_engine::{create_engine, Engine, HwMidi, WorkerHooks, WorkerPool};
use rlive_project::{
    Clip, ClipContent, ExternalRoute, NoteData, Route, SidechainSource, Tap, Track, TrackKind,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn pool(threads: usize) -> Option<Box<WorkerPool>> {
    // The pool's worker threads count their allocations; the audio thread uses the usual guard.
    (threads > 1).then(|| {
        WorkerPool::spawn(
            threads - 1,
            Some(WorkerHooks {
                start: Arc::new(|_| rlive_testkit::watch_helper_thread()),
                stop: Some(Arc::new(|_| rlive_testkit::unwatch_helper_thread())),
            }),
        )
        .expect("worker threads")
    })
}

fn notes(clip: &mut Clip, notes: &[(u64, u64, u8)]) {
    clip.notes_mut()
        .expect("a MIDI clip")
        .extend(notes.iter().map(|&(s, l, k)| NoteData::new(s, l, k, 0.8)));
}

fn midi_track(name: &str, scenes: usize) -> Track {
    Track::new(name, TrackKind::Midi, [1, 2, 3], scenes)
}

fn write_sample(path: &Path) {
    let n = SR as usize / 2;
    let left: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SR as f32;
            (-t * 6.0).exp() * (t * 220.0 * std::f32::consts::TAU).sin() * 0.6
        })
        .collect();
    let right: Vec<f32> = left.iter().rev().copied().collect();
    rlive_devices::write_wav(path, &left, &right, SR as u32).unwrap();
}

struct SamplePath(PathBuf);

impl SamplePath {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("rlive-multicore-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loop.wav");
        write_sample(&path);
        Self(path)
    }
}

impl Drop for SamplePath {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

/// Plenty of tracks, playing together through groups, sends and sidechains.
fn project(sample: &Path) -> Project {
    let mut p = Project::empty();
    p.tempo = 125.0;
    p.time_signature = TimeSignature::default();
    p.quantize = rlive_core::Quantize::None;
    let scenes = p.scenes.len();
    let sig = p.time_signature;

    let mut tracks = Vec::new();
    for t in 0..6 {
        let mut keys = midi_track(&format!("Keys {t}"), scenes);
        keys.devices.push(Device::builtin("rlive.poly", "Poly"));
        keys.devices.push(Device::builtin("rlive.delay", "Delay"));
        keys.volume_db = -6.0 - t as f32;
        let mut clip = Clip::new_midi("Chords", [1, 1, 1], 2, sig);
        notes(
            &mut clip,
            &[
                (t as u64 * 37, 3 * PPQ, 57 + t as u8 * 2),
                (2 * PPQ, PPQ, 60 + t as u8),
                (4 * PPQ, 3 * PPQ, 53 + t as u8),
            ],
        );
        keys.clips[0] = Some(clip);
        tracks.push(keys);
    }
    // An audio track playing a loop.
    let mut loops = Track::new("Loops", TrackKind::Audio, [7, 8, 9], scenes);
    loops.devices.push(Device::builtin("rlive.eq3", "EQ"));
    loops.clips[0] = Some(Clip {
        name: "Loop".into(),
        color: [4, 5, 6],
        length: 2 * PPQ,
        looped: true,
        content: ClipContent::audio(sample, -3.0),
        ..Clip::default()
    });
    tracks.push(loops);
    p.tracks = tracks;
    p.master
        .devices
        .push(Device::builtin("rlive.compressor", "Glue"));
    p
}

/// The whole rig on one engine: `threads` of them rendering it.
struct Rig {
    engine: Engine,
    session: Session,
    hw: rtrb::Consumer<HwMidi>,
    hashes: Vec<(&'static str, u64)>,
    midi: Vec<(u16, u64, u64)>,
}

impl Rig {
    fn new(threads: usize, sample: &Path) -> Self {
        let (mut engine, mut handle) = create_engine(SR, BLOCK);
        engine.set_workers(pool(threads));
        assert_eq!(engine.render_threads(), threads.max(1));
        let hw = handle.take_hw_midi().expect("the hardware MIDI ring");
        let mut session = Session::new(handle, project(sample), PluginCache::default());
        wire_up(&mut session);
        Self {
            engine,
            session,
            hw,
            hashes: Vec::new(),
            midi: Vec::new(),
        }
    }

    fn block(&mut self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        rlive_testkit::assert_no_alloc(|| self.engine.process_stereo(&mut l, &mut r));
        self.session.idle();
        self.session.engine.collect_garbage();
        while let Ok(m) = self.hw.pop() {
            self.midi.push(midi_key(m));
        }
        (l, r)
    }

    /// Renders a phase and records its hash.
    fn phase(&mut self, name: &'static str, frames: usize, chunk: usize) {
        let mut done = 0;
        let mut hash = 0xcbf2_9ce4_8422_2325;
        let mut peak = 0f32;
        while done < frames {
            let n = chunk.min(frames - done);
            let (l, r) = self.block(n);
            peak = l.iter().fold(peak, |p, s| p.max(s.abs()));
            fnv(&mut hash, &l);
            fnv(&mut hash, &r);
            done += n;
        }
        assert!(name != "playing" || peak > 0.01, "the Set makes sound");
        self.hashes.push((name, hash));
    }
}

fn midi_key(m: HwMidi) -> (u16, u64, u64) {
    let word = match m.message {
        MidiMessage::NoteOn {
            channel,
            key,
            velocity,
            ..
        } => 0x10 << 56 | (channel as u64) << 48 | (key as u64) << 40 | velocity.to_bits() as u64,
        MidiMessage::NoteOff { channel, key, .. } => {
            0x20 << 56 | (channel as u64) << 48 | (key as u64) << 40
        }
        other => 0xff << 56 | format!("{other:?}").len() as u64,
    };
    (m.port, m.offset.to_bits(), word)
}

fn fnv(hash: &mut u64, samples: &[f32]) {
    for s in samples {
        for b in s.to_bits().to_le_bytes() {
            *hash ^= b as u64;
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Group, sends, a sidechain, a compensated latency, a modulator and MIDI output.
fn wire_up(s: &mut Session) {
    let t: Vec<TrackId> = s.project.tracks.iter().map(|t| t.id).collect();
    // Keys 0 and 1 go through a group with an effect on it.
    let group = s.group_tracks(&t[0..2]).expect("a group");
    s.add_builtin(ChainRef::Track(group), "rlive.compressor");
    // A return with a send from the group and from Keys 2.
    let ret = s.add_return().expect("a return track");
    s.add_builtin(ChainRef::Track(ret), "rlive.delay");
    s.set_send(group, ret, -6.0);
    s.set_send(t[2], ret, -3.0);
    // A compressor keyed by Keys 3.
    s.add_builtin(ChainRef::Track(t[4]), "rlive.compressor");
    let comp = s.devices(ChainRef::Track(t[4]))[2].id;
    let source = SidechainSource {
        track: t[3],
        tap: Tap::PostFx,
    };
    assert!(s.set_sidechain(ChainRef::Track(t[4]), comp, Some(source)));
    // Lookahead on the keyed compressor: a latency for delay compensation to meet.
    s.set_param(ChainRef::Track(t[4]), comp, compressor::LOOKAHEAD, 2.0);
    // An LFO on Keys 3 modulating Keys 2's first continuous parameter: no graph edge.
    s.add_builtin(ChainRef::Track(t[3]), "rlive.lfo");
    let lfo_id = s.devices(ChainRef::Track(t[3])).last().unwrap().id;
    let device = s.devices(ChainRef::Track(t[2]))[0].id;
    let param = s.runtimes[&device]
        .params
        .iter()
        .find(|p| matches!(p.kind, ParamKind::Continuous { .. }))
        .expect("something to modulate")
        .id;
    let target = ParamRef {
        owner: ParamOwner::Device(device),
        id: ParamId(param),
    };
    assert!(s.map_modulator(lfo_id, 0, target), "maps");
    s.set_param(ChainRef::Track(t[3]), lfo_id, lfo::OFFSET, 25.0);
    // Keys 5 plays out of a hardware MIDI port.
    s.set_midi_out_ports(vec!["Out".into()]);
    s.set_output(
        &[t[5]],
        Route::External(ExternalRoute::Midi {
            port: Some("Out".into()),
            channel: None,
        }),
    );
    s.send_midi_output(t[5]);
    s.commit_edits(false);
    let plan = graph::compile_plan(&s.project);
    assert!(!plan.sidechains.is_empty(), "sidechain in the plan");
    assert!(!plan.tap_outs.is_empty(), "something is tapped");
    assert!(plan.levels.len() > 2, "groups give the plan depth");
}

/// Everything a real Set might do while it plays.
fn scenario(threads: usize, sample: &Path) -> Rig {
    let mut r = Rig::new(threads, sample);
    let t: Vec<TrackId> = r.session.project.tracks.iter().map(|t| t.id).collect();
    r.session.play();
    for track in &t {
        r.session.launch_clip(*track, 0);
    }
    r.session.set_metronome(true);
    r.phase("playing", SR as usize, BLOCK);
    r.phase("uneven blocks", SR as usize / 2, 311);
    r.session.set_metronome(false);
    let (chain, device, param) = {
        let d = r.session.devices(ChainRef::Track(t[3]))[0].id;
        (
            ChainRef::Track(t[3]),
            d,
            r.session.runtimes[&d].params[0].id,
        )
    };
    r.session.set_param(chain, device, param, 0.25);
    r.phase("param moved", SR as usize / 4, 128);
    r.session.project.track_mut(t[1]).unwrap().solo = true;
    r.session.sync_mix(t[1]);
    r.phase("soloed in a group", SR as usize / 2, BLOCK);
    r.session.project.track_mut(t[1]).unwrap().solo = false;
    r.session.sync_mix(t[1]);
    r.session
        .add_builtin(ChainRef::Track(t[6]), "rlive.compressor");
    r.phase("device added", SR as usize / 4, BLOCK);
    let added = r.session.add_track(TrackKind::Midi, Some(2));
    r.session.add_builtin(ChainRef::Track(added), "rlive.poly");
    r.phase("track added", SR as usize / 4, BLOCK);
    r.session.delete_track(added);
    r.phase("track deleted", SR as usize / 4, BLOCK);
    r.session.set_loop(true, 0, 4 * PPQ);
    r.session.seek(3.0 * PPQ as f64);
    r.phase("looping", SR as usize, BLOCK);
    r.session.stop();
    r.phase("tails", SR as usize / 2, BLOCK);
    r
}

#[test]
fn a_full_set_renders_bit_identically_on_one_two_and_many_threads() {
    let sample = SamplePath::new("identity");
    let one = scenario(1, &sample.0);
    assert!(
        one.hashes.iter().any(|(_, h)| *h != 0),
        "the scenario makes sound"
    );
    assert!(!one.midi.is_empty(), "the Set really plays MIDI out");
    for threads in [2, 4, 8] {
        let many = scenario(threads, &sample.0);
        assert_eq!(
            many.hashes, one.hashes,
            "audio differs on {threads} threads"
        );
        assert_eq!(many.midi, one.midi, "MIDI differs on {threads} threads");
        let ran = many.engine.workers().unwrap().tasks_run();
        assert!(
            ran[..threads - 1].iter().any(|n| *n > 0),
            "workers ran: {ran:?}"
        );
        assert_eq!(
            rlive_testkit::helper_heap_touches(),
            0,
            "a worker thread touched the heap"
        );
    }
}

/// A seeded random number generator, so a failure can be replayed.
struct Xor(u64);

impl Xor {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// One random edit, the same on both sides.
fn random_edit(rng: &mut Xor, sessions: [&mut Session; 2]) {
    let [a, b] = sessions;
    let tracks = a.project.tracks.len().min(b.project.tracks.len());
    let choice = rng.pick(14);
    let i = rng.pick(tracks);
    let k = rng.pick(4);
    let chain = |s: &Session, i: usize| ChainRef::Track(s.project.tracks[i].id);
    match choice {
        0 if tracks > 2 => {
            let ta = a.project.tracks[i].id;
            let tb = b.project.tracks[i].id;
            if !a.project.track(ta).unwrap().is_group() {
                a.delete_track(ta);
                b.delete_track(tb);
            }
        }
        1 if tracks < 24 => {
            let kinds = [TrackKind::Midi, TrackKind::Audio];
            a.add_track(kinds[k % 2], Some(i));
            b.add_track(kinds[k % 2], Some(i));
        }
        2 => {
            let id = ["rlive.compressor", "rlive.delay", "rlive.eq3", "rlive.poly"][k];
            a.add_builtin(chain(a, i), id);
            b.add_builtin(chain(b, i), id);
        }
        3 => {
            let (ca, cb) = (chain(a, i), chain(b, i));
            let count = a.devices(ca).len().min(b.devices(cb).len());
            if count > 0 {
                let at = rng.pick(count);
                let da = a.devices(ca)[at].id;
                let db = b.devices(cb)[at].id;
                a.remove_device(ca, da);
                b.remove_device(cb, db);
            }
        }
        4 => {
            let (ca, cb) = (chain(a, i), chain(b, i));
            let count = a.devices(ca).len().min(b.devices(cb).len());
            if count > 1 {
                let (from, to) = (rng.pick(count), rng.pick(count));
                let da = a.devices(ca)[from].id;
                let db = b.devices(cb)[from].id;
                a.move_device(ca, da, to);
                b.move_device(cb, db, to);
            }
        }
        5 => {
            let (ca, cb) = (chain(a, i), chain(b, i));
            if let (Some(da), Some(db)) = (a.devices(ca).first(), b.devices(cb).first()) {
                let (da, db) = (da.id, db.id);
                let params = a.runtimes[&da].params.len();
                if params > 0 {
                    let p = rng.pick(params);
                    let pa = a.runtimes[&da].params[p].id;
                    let pb = b.runtimes[&db].params[p].id;
                    let value = rng.pick(1000) as f64 / 1000.0;
                    a.set_param(ca, da, pa, value);
                    b.set_param(cb, db, pb, value);
                }
            }
        }
        6 => {
            let (ta, tb) = (a.project.tracks[i].id, b.project.tracks[i].id);
            a.launch_clip(ta, 0);
            b.launch_clip(tb, 0);
        }
        7 => {
            let tick = (rng.pick(16) as u64 * PPQ) as f64;
            a.seek(tick);
            b.seek(tick);
        }
        8 => {
            for s in [&mut *a, &mut *b] {
                let t = s.project.tracks[i].id;
                let solo = !s.project.track(t).unwrap().solo;
                s.project.track_mut(t).unwrap().solo = solo;
                s.sync_mix(t);
            }
        }
        9 if a.project.returns.len() < 3 => {
            a.add_return();
            b.add_return();
        }
        10 if !a.project.returns.is_empty() => {
            let ra = a.project.returns[0].id;
            let rb = b.project.returns[0].id;
            let level = -(rng.pick(40) as f32);
            a.set_send(a.project.tracks[i].id, ra, level);
            b.set_send(b.project.tracks[i].id, rb, level);
        }
        11 if tracks >= 3 => {
            let (ta, tb) = (a.project.tracks[i].id, b.project.tracks[i].id);
            let (ga, gb) = (
                a.project.tracks[(i + 1) % tracks].id,
                b.project.tracks[(i + 1) % tracks].id,
            );
            a.group_tracks(&[ta, ga]);
            b.group_tracks(&[tb, gb]);
        }
        12 => {
            let vol = -(rng.pick(30) as f32);
            for s in [&mut *a, &mut *b] {
                let t = s.project.tracks[i].id;
                s.project.track_mut(t).unwrap().volume_db = vol;
                s.sync_mix(t);
            }
        }
        _ => {
            a.play();
            b.play();
        }
    }
}

/// `seed` drives everything, so a failure can be replayed.
fn random_edits_stay_identical(seed: u64, threads: usize, steps: usize) {
    let sample = SamplePath::new(&format!("fuzz-{seed}"));
    let mut serial = Rig::new(1, &sample.0);
    let mut parallel = Rig::new(threads, &sample.0);
    serial.session.play();
    parallel.session.play();
    let mut rng = Xor(seed | 1);
    for step in 0..steps {
        random_edit(&mut rng, [&mut serial.session, &mut parallel.session]);
        serial.session.commit_edits(false);
        parallel.session.commit_edits(false);
        for block in 0..3 {
            let (la, ra) = serial.block(BLOCK);
            let (lb, rb) = parallel.block(BLOCK);
            assert_eq!(
                la, lb,
                "seed {seed}: left differs at step {step}, block {block}, {threads} threads"
            );
            assert_eq!(
                ra, rb,
                "seed {seed}: right differs at step {step}, block {block}"
            );
        }
    }
    assert_eq!(serial.midi, parallel.midi, "seed {seed}: MIDI differs");
    assert_eq!(
        rlive_testkit::helper_heap_touches(),
        0,
        "a worker allocated"
    );
}

#[test]
fn random_edits_during_playback_are_bit_identical_and_allocation_free() {
    for seed in [1u64, 2, 3] {
        random_edits_stay_identical(seed, 4, 60);
    }
}

#[test]
fn deliberately_mismatched_threads_fuzz() {
    // A second thread count, a second seed set: over the number of threads that matters most.
    for (seed, threads) in [(0x1234u64, 2), (0x9876, 8), (0xdead, 3)] {
        random_edits_stay_identical(seed, threads, 40);
    }
}

/// Speed-up on a heavy synthetic project. Ignored: run by hand and read the numbers.
///
/// `cargo test -p rlive-app multicore_speedup -- --ignored --nocapture`
#[test]
#[ignore = "a measurement, not a check"]
fn multicore_speedup() {
    // 48 tracks of real DSP: a Poly with a held chord into Reverb, Compressor and EQ, and a send.
    fn heavy_project() -> Project {
        let mut p = Project::empty();
        p.quantize = rlive_core::Quantize::None;
        let scenes = p.scenes.len();
        let sig = p.time_signature;
        p.tracks = (0..48)
            .map(|t| {
                let mut track = midi_track(&format!("T{t}"), scenes);
                track.devices.push(Device::builtin("rlive.poly", "Poly"));
                track.devices.push(Device::builtin("rlive.compressor", "C"));
                track.devices.push(Device::builtin("rlive.eq3", "EQ"));
                track.devices.push(Device::builtin("rlive.delay", "Delay"));
                track
                    .devices
                    .push(Device::builtin("rlive.reverb", "Reverb"));
                let mut clip = Clip::new_midi("Pad", [1, 1, 1], 4, sig);
                notes(
                    &mut clip,
                    &[
                        (0, 16 * PPQ, 48 + (t % 12) as u8),
                        (0, 16 * PPQ, 55 + (t % 12) as u8),
                        (0, 16 * PPQ, 60 + (t % 12) as u8),
                        (0, 16 * PPQ, 67 + (t % 12) as u8),
                    ],
                );
                track.clips[0] = Some(clip);
                track
            })
            .collect();
        p
    }

    fn time(threads: usize) -> f64 {
        const FRAMES: usize = 256;
        const BLOCKS: usize = 3000;
        let (mut engine, handle) = create_engine(SR, FRAMES);
        engine.set_workers(pool(threads));
        let mut session = Session::new(handle, heavy_project(), PluginCache::default());
        session.play();
        for t in session.project.tracks.clone() {
            session.launch_clip(t.id, 0);
        }
        let (mut l, mut r) = (vec![0.0; FRAMES], vec![0.0; FRAMES]);
        for _ in 0..500 {
            engine.process_stereo(&mut l, &mut r);
            session.idle();
        }
        let started = std::time::Instant::now();
        for _ in 0..BLOCKS {
            engine.process_stereo(&mut l, &mut r);
        }
        let elapsed = started.elapsed().as_secs_f64();
        session.idle();
        session.engine.collect_garbage();
        elapsed / BLOCKS as f64 / (FRAMES as f64 / SR)
    }

    let serial = time(1);
    println!("  1 thread : {:.1}% of the deadline", serial * 100.0);
    for threads in [2, 4, 8] {
        let load = time(threads);
        println!(
            "{threads:>3} threads: {:.1}% of the deadline, speed-up {:.2}x",
            load * 100.0,
            serial / load
        );
    }
    let hw = std::thread::available_parallelism().map_or(0, |n| n.get());
    println!("available parallelism: {hw}");
}
