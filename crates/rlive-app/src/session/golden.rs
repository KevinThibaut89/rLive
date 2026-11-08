//! Golden renders: a flat project must keep sounding exactly as it did before the device tree.
//!
//! [`GOLDEN`] holds hashes captured from the engine as it was before milestone K4 (the commit
//! adding Processor v2). They compare every sample bit for bit, on the machine they were captured
//! on: other platforms round `sin` and `powf` differently, so CI on Linux checks only the
//! portable reference mix further down.

use super::*;
use rlive_core::param::mixer;
use rlive_core::{TimeSignature, PPQ};
use rlive_engine::{create_engine, Engine};
use rlive_project::{ArrangementClip, Clip, ClipContent, NoteData, Track};

const SR: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

/// One hash per phase of [`scenario`], over the left and right channels.
///
/// Captured on macOS aarch64, from the code before K4, with
/// `GOLDEN_PRINT=1 cargo test -p rlive-app golden -- --nocapture`.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GOLDEN: &[(&str, u64)] = &[
    ("arrangement", 0xeb53ecd23d749f13),
    ("uneven blocks", 0x77a495aff8171022),
    ("session clips", 0x0d916b327789d78b),
    ("param moved", 0x2b0e146ccd11900d),
    ("bypassed", 0x72d2d598fa20d5d2),
    ("bypass cleared", 0x2ac1193df2e07020),
    ("mixer", 0xd567a19220b452b2),
    ("solo", 0x0aee1ddb40080121),
    ("master volume", 0x1e8db035723793f4),
    ("device added", 0x7854007d1c241e68),
    ("device moved", 0xb4025be5e5f6be08),
    ("device removed", 0x2f5a65390e224238),
    ("track added", 0xd9f937987893ee54),
    ("track moved", 0xd6c57a077c56a9c2),
    ("track deleted", 0x121010414e5c564a),
    ("looping", 0x8f9342b9f52e14d6),
    ("back to arrangement", 0x8a4c49a8d18c3765),
    ("tails", 0x9ebf9a6ec921bb25),
];

fn fnv(hash: &mut u64, samples: &[f32]) {
    for s in samples {
        for b in s.to_bits().to_le_bytes() {
            *hash ^= b as u64;
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn notes(clip: &mut Clip, notes: &[(u64, u64, u8)]) {
    let list = clip.notes_mut().expect("a MIDI clip");
    list.extend(notes.iter().map(|&(start, length, key)| NoteData {
        start,
        length,
        key,
        velocity: 0.8,
    }));
}

fn midi_track(name: &str, device: &str, scenes: usize) -> Track {
    let mut t = Track::new(name, TrackKind::Midi, [1, 2, 3], scenes);
    t.devices.push(Device::builtin(device, name));
    t
}

/// A short, deterministic sample with an attack and some harmonics.
fn write_sample(path: &Path) {
    let n = SR as usize / 2;
    let left: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SR as f32;
            let env = (-t * 6.0).exp();
            env * (0.6 * (t * 220.0 * std::f32::consts::TAU).sin()
                + 0.3 * (t * 663.0 * std::f32::consts::TAU).sin())
        })
        .collect();
    let right: Vec<f32> = left.iter().rev().copied().collect();
    rlive_devices::write_wav(path, &left, &right, SR as u32).unwrap();
}

fn audio_clip(path: &Path, length: u64) -> Clip {
    Clip {
        name: "Loop".into(),
        color: [4, 5, 6],
        length,
        looped: true,
        content: ClipContent::Audio {
            path: path.to_path_buf(),
            gain_db: -3.0,
        },
    }
}

/// Clips, built-in devices at every position, a mixer without sends, and Arrangement lanes.
fn project(sample: &Path) -> Project {
    let mut p = Project::empty();
    p.tempo = 123.0;
    p.time_signature = TimeSignature::default();
    p.quantize = rlive_core::Quantize::None;
    let scenes = p.scenes.len();
    let sig = p.time_signature;
    let beat = PPQ;

    let mut keys = midi_track("Keys", "rlive.poly", scenes);
    keys.devices.push(Device::builtin("rlive.delay", "Delay"));
    keys.volume_db = -4.0;
    let mut chords = Clip::new_midi("Chords", [1, 1, 1], 2, sig);
    notes(
        &mut chords,
        &[
            (0, 3 * beat, 57),
            (0, 3 * beat, 60),
            (4 * beat, 3 * beat, 53),
        ],
    );
    keys.clips[0] = Some(chords);
    let mut lane = Clip::new_midi("Lane", [1, 1, 1], 2, sig);
    notes(&mut lane, &[(beat / 2, beat, 69), (2 * beat, beat, 72)]);
    keys.arrangement.push(ArrangementClip::new(lane, 0));

    let mut pad = midi_track("Pad", "rlive.poly", scenes);
    pad.devices.push(Device::builtin("rlive.reverb", "Reverb"));
    pad.devices
        .push(Device::builtin("rlive.saturator", "Saturator"));
    pad.pan = -0.6;
    pad.volume_db = -9.0;
    let mut long = Clip::new_midi("Long", [1, 1, 1], 4, sig);
    notes(&mut long, &[(0, 8 * beat, 48), (8 * beat, 6 * beat, 43)]);
    pad.arrangement.push(ArrangementClip::new(long, beat));

    let mut loops = Track::new("Loops", TrackKind::Audio, [7, 8, 9], scenes);
    loops.devices.push(Device::builtin("rlive.eq3", "EQ"));
    loops
        .devices
        .push(Device::builtin("rlive.compressor", "Compressor"));
    loops.pan = 0.35;
    loops.clips[0] = Some(audio_clip(sample, 2 * beat));
    loops
        .arrangement
        .push(ArrangementClip::new(audio_clip(sample, 4 * beat), 2 * beat));

    let mut sampler = Track::new("Sampler", TrackKind::Midi, [2, 2, 2], scenes);
    let mut dev = Device::builtin("rlive.sampler", "Sampler");
    if let DeviceKind::Builtin { sample: s, .. } = &mut dev.kind {
        *s = Some(sample.to_path_buf());
    }
    sampler
        .devices
        .push(Device::builtin("rlive.pitch", "Pitch"));
    sampler.devices.push(dev);
    let mut hits = Clip::new_midi("Hits", [1, 1, 1], 1, sig);
    notes(
        &mut hits,
        &[(0, beat, 60), (beat, beat, 64), (3 * beat, beat, 67)],
    );
    sampler.clips[0] = Some(hits.clone());
    sampler
        .arrangement
        .push(ArrangementClip::new(hits, 4 * beat));

    let mut muted = midi_track("Muted", "rlive.poly", scenes);
    muted.mute = true;
    muted.clips[0] = keys.clips[0].clone();

    p.tracks = vec![keys, pad, loops, sampler, muted];
    p.master.volume_db = -2.0;
    p.master
        .devices
        .push(Device::builtin("rlive.compressor", "Glue"));
    p.master
        .devices
        .push(Device::builtin("rlive.utility", "Utility"));
    p
}

struct Render {
    engine: Engine,
    session: Session,
    hashes: Vec<(&'static str, u64)>,
}

impl Render {
    fn new(project: Project) -> Self {
        let (engine, handle) = create_engine(SR, MAX_BLOCK);
        let session = Session::new(handle, project, PluginCache::default());
        Self {
            engine,
            session,
            hashes: Vec::new(),
        }
    }

    /// Renders one phase in chunks of the given sizes, cycling through them.
    fn phase(&mut self, name: &'static str, frames: usize, chunks: &[usize]) {
        let mut l = vec![0.0; 2048];
        let mut r = vec![0.0; 2048];
        let (mut left, mut right) = (Vec::new(), Vec::new());
        let mut done = 0;
        for &chunk in chunks.iter().cycle() {
            if done >= frames {
                break;
            }
            let n = chunk.min(frames - done);
            render_block(&mut self.engine, &mut l[..n], &mut r[..n]);
            self.session.idle();
            left.extend_from_slice(&l[..n]);
            right.extend_from_slice(&r[..n]);
            done += n;
        }
        assert!(left.iter().all(|s| s.is_finite()), "{name}: finite");
        // Stopping the transport resets every device, so only that last phase may be silent.
        assert!(
            name == "tails" || left.iter().any(|s| *s != 0.0),
            "{name}: makes sound"
        );
        let mut hash = 0xcbf2_9ce4_8422_2325;
        fnv(&mut hash, &left);
        fnv(&mut hash, &right);
        self.hashes.push((name, hash));
    }
}

fn render_block(engine: &mut Engine, left: &mut [f32], right: &mut [f32]) {
    engine.process_stereo(left, right);
}

/// Everything a flat project can do while playing.
fn scenario(sample: &Path) -> Vec<(&'static str, u64)> {
    let mut r = Render::new(project(sample));
    let (keys, pad, loops, sampler) = {
        let t = &r.session.project.tracks;
        (t[0].id, t[1].id, t[2].id, t[3].id)
    };
    let keys_chain = ChainRef::Track(keys);
    let pad_chain = ChainRef::Track(pad);

    // The Arrangement alone, in the block sizes a sound card might use.
    r.session.play();
    r.phase("arrangement", SR as usize * 2, &[512]);
    r.phase("uneven blocks", SR as usize, &[64, 777, 128, 1500, 33]);

    // Session clips and a metronome over the Arrangement.
    r.session.launch_clip(keys, 0);
    r.session.launch_clip(loops, 0);
    r.session.launch_clip(sampler, 0);
    r.session.set_metronome(true);
    r.phase("session clips", SR as usize * 2, &[256]);
    r.session.set_metronome(false);

    // Device parameters and bypass, between and inside chains.
    let (dev, param) = {
        let d = r.session.devices(keys_chain)[0].id;
        (d, r.session.runtimes[&d].params[0].id)
    };
    let delay = r.session.devices(keys_chain)[1].id;
    r.session.set_param(keys_chain, dev, param, 0.2);
    r.phase("param moved", SR as usize / 4, &[128]);
    r.session.set_bypass(keys_chain, delay, true);
    r.phase("bypassed", SR as usize / 4, &[300]);
    r.session.set_bypass(keys_chain, delay, false);
    r.phase("bypass cleared", SR as usize / 4, &[300]);

    // The mixer.
    r.session.project.track_mut(pad).unwrap().volume_db = -1.5;
    r.session.project.track_mut(pad).unwrap().pan = 0.8;
    r.session.sync_mix(pad);
    r.phase("mixer", SR as usize / 2, &[512]);
    r.session.project.track_mut(sampler).unwrap().solo = true;
    r.session.sync_mix(sampler);
    r.phase("solo", SR as usize / 4, &[512]);
    r.session.project.track_mut(sampler).unwrap().solo = false;
    r.session.sync_mix(sampler);
    let master = r.session.mixer_cell(ChainRef::Master, mixer::VOLUME);
    assert!(master.is_some());
    r.session.set_master_volume(-5.5);
    r.phase("master volume", SR as usize / 4, &[512]);

    // Structure while playing.
    r.session.add_builtin(pad_chain, "rlive.utility");
    r.phase("device added", SR as usize / 4, &[512]);
    let reverb = r.session.devices(pad_chain)[1].id;
    r.session.move_device(pad_chain, reverb, 0);
    r.phase("device moved", SR as usize / 4, &[512]);
    r.session.remove_device(pad_chain, reverb);
    r.phase("device removed", SR as usize / 4, &[512]);
    let added = r.session.add_track(TrackKind::Midi, Some(1));
    r.phase("track added", SR as usize / 4, &[512]);
    r.session.move_track(added, 3);
    r.phase("track moved", SR as usize / 4, &[512]);
    r.session.delete_track(added);
    r.phase("track deleted", SR as usize / 4, &[512]);

    // The loop brace and the playhead.
    r.session.set_loop(true, 0, 8 * PPQ);
    r.session.seek(7.0 * PPQ as f64);
    r.phase("looping", SR as usize * 3, &[512]);
    r.session.set_loop(false, 0, 8 * PPQ);
    r.session.back_to_arrangement();
    r.session.seek(0.0);
    r.phase("back to arrangement", SR as usize, &[1024]);
    r.session.stop();
    r.phase("tails", SR as usize / 2, &[512]);
    r.hashes
}

fn sample_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rlive-golden-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("loop.wav");
    write_sample(&path);
    path
}

#[test]
fn a_flat_project_renders_identically_to_before_the_device_tree() {
    let path = sample_path("flat");
    let hashes = scenario(&path);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
    if std::env::var_os("GOLDEN_PRINT").is_some() {
        for (name, hash) in &hashes {
            println!("    (\"{name}\", 0x{hash:016x}),");
        }
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    assert_eq!(hashes, GOLDEN, "the flat render changed");
}

/// Plays `project` for a while and returns the left channel, then the right.
fn solo_render(project: Project, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut r = Render::new(project);
    r.session.play();
    let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
    let chunks = left.chunks_mut(256).zip(right.chunks_mut(256));
    for (cl, cr) in chunks {
        render_block(&mut r.engine, cl, cr);
    }
    (left, right)
}

/// The portable half of the proof: without master devices or volume, the output is exactly the
/// sum of what each track makes on its own, added in track order.
#[test]
fn every_track_reaches_the_master_exactly_once_in_track_order() {
    let path = sample_path("sum");
    let mut full = project(&path);
    full.master.devices.clear();
    full.master.volume_db = 0.0;
    full.tracks.retain(|t| !t.mute);
    let frames = SR as usize;
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    for t in &full.tracks {
        let mut one = full.clone();
        one.tracks = vec![t.clone()];
        let (l, r) = solo_render(one, frames);
        for i in 0..frames {
            left[i] += l[i];
            right[i] += r[i];
        }
    }
    let (l, r) = solo_render(full, frames);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
    assert!(l.iter().any(|s| *s != 0.0), "the project makes sound");
    assert_eq!(l, left);
    assert_eq!(r, right);
}
