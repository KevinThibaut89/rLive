//! End-to-end CLAP hosting test against the `rlive-test-clap` fixture plugins.

use rlive_core::{
    AudioBuffer, AudioInput, AudioOutput, EventQueue, ExprKind, MidiEvent, MidiMessage, ParamEvent,
    ParamId, ParamOutQueue, PluginFormat, ProcessContext, Tail, TransportInfo, ViewWriter,
};
use rlive_plugin_host::{instantiate, scanner, PluginDescriptor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Build the fixture cdylib (in its own target dir to avoid fighting over cargo's lock)
/// and copy it to a `.clap` file.
fn fixture() -> PathBuf {
    fixture_named("rlive-clap")
}

/// The same, in a directory of its own so tests can clean up after themselves. The fixture is
/// built once per test run, and each caller gets its own copy, so parallel tests can't race on it.
fn fixture_named(dir_prefix: &str) -> PathBuf {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let lib = LIB.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // Inside the build's own target directory, if it has been given one.
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(|| root.join("target"), PathBuf::from)
            .join("test-plugins");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-q", "-p", "rlive-test-clap", "--target-dir"])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building rlive-test-clap failed");
        target.join("debug").join(format!(
            "{}rlive_test_clap.{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_EXTENSION
        ))
    });
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{dir_prefix}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("rlive-test.clap");
    std::fs::copy(lib, &dest).unwrap();
    dest
}

fn process(
    p: &mut dyn rlive_core::Processor,
    input: f32,
    events: &[MidiEvent],
    q: &mut ParamOutQueue,
) -> Vec<f32> {
    let transport = TransportInfo {
        playing: true,
        ..Default::default()
    };
    let mut l = vec![input; 512];
    let mut r = vec![input; 512];
    let mut events_out = EventQueue::with_capacity(0);
    let view = ViewWriter::none();
    p.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: None,
        aux_out: &mut [],
        events,
        events_out: &mut events_out,
        params: &[],
        transport: &transport,
        param_out: q,
        mod_out: &mut [],
        view: &view,
    });
    l
}

fn find<'a>(found: &'a [PluginDescriptor], id: &str) -> &'a PluginDescriptor {
    found.iter().find(|d| d.id == id).expect(id)
}

#[test]
fn clap_scan_instantiate_process_state() {
    let path = fixture();

    // Out-of-process scan through the scanner binary.
    let exe = Path::new(env!("CARGO_BIN_EXE_rlive-plugin-scanner"));
    let found =
        scanner::scan_out_of_process(exe, PluginFormat::Clap, &path, Duration::from_secs(30))
            .unwrap();
    assert_eq!(found.len(), 3);
    let gain = find(&found, "rs.rlive.test-gain");
    let sine = find(&found, "rs.rlive.test-sine");
    assert!(!gain.is_instrument);
    assert!(sine.is_instrument);
    assert_eq!(gain.name, "rLive Test Gain");

    // Effect: gain parameter scales the signal.
    let mut inst = instantiate(gain, 48_000.0, 512).unwrap();
    let params = inst.handle.params();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "Gain");
    assert_eq!(inst.handle.format_value(0, 0.5).as_deref(), Some("0.50x"));
    let mut q = ParamOutQueue::with_capacity(16);
    let out = process(&mut *inst.processor, 0.5, &[], &mut q);
    assert!((out[100] - 0.5).abs() < 1e-6);
    inst.processor.set_param(0, 0.5);
    let out = process(&mut *inst.processor, 0.5, &[], &mut q);
    assert!((out[100] - 0.25).abs() < 1e-6);
    assert_eq!(inst.handle.param_value(0), Some(0.5));

    // Plugin-originated change comes back through the output event queue.
    inst.processor.set_param(0, 2.0);
    let out = process(&mut *inst.processor, 0.5, &[], &mut q);
    assert!((out[100] - 0.75).abs() < 1e-6);
    assert_eq!(q.drain().collect::<Vec<_>>(), vec![(0, 1.5)]);

    // State round trip.
    let state = inst.handle.save_state().unwrap();
    inst.processor.set_param(0, 0.1);
    process(&mut *inst.processor, 0.5, &[], &mut q);
    assert!(inst.handle.load_state(&state));
    assert_eq!(inst.handle.param_value(0), Some(1.5));
    inst.processor.stop_processing();
    drop(inst);

    // Instrument: notes produce a 440 Hz sine.
    let mut inst = instantiate(sine, 48_000.0, 512).unwrap();
    let on = MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key: 69,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        },
    );
    let mut all = process(&mut *inst.processor, 0.0, &[on], &mut q);
    for _ in 0..93 {
        all.extend(process(&mut *inst.processor, 0.0, &[], &mut q));
    }
    let crossings = all.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let hz = crossings as f64 * 48_000.0 / all.len() as f64;
    assert!((hz - 440.0).abs() < 3.0, "{hz}");
    inst.processor.stop_processing();
    drop(inst);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// Runs one note through the fixture, which reports each event it sees as a parameter.
fn run_events(dir: &str, events: &[MidiEvent]) -> Vec<(u32, f64)> {
    let path = fixture_named(dir);
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let mut inst = instantiate(find(&found, "rs.rlive.test-sine"), 48_000.0, 512).unwrap();
    let mut q = ParamOutQueue::with_capacity(64);
    process(&mut *inst.processor, 0.0, events, &mut q);
    let seen = q.drain().collect();
    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
    seen
}

#[test]
fn a_clap_plugin_sees_note_ids_and_native_expression() {
    let note = 42u32;
    let expr = |kind, value| {
        MidiEvent::new(
            0,
            MidiMessage::NoteExpr {
                channel: 0,
                key: 60,
                note_id: note,
                kind,
                value,
            },
        )
    };
    let on = MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
            note_id: note,
            tuning: 0.0,
        },
    );
    let off = MidiEvent::new(
        100,
        MidiMessage::NoteOff {
            channel: 0,
            key: 60,
            velocity: 0.2,
            note_id: note,
        },
    );
    let seen = run_events(
        "rlive-clap-expr",
        &[
            on,
            expr(ExprKind::Pitch, 2.5),
            expr(ExprKind::Slide, 0.75),
            expr(ExprKind::Pressure, 0.5),
            off,
        ],
    );
    let values_of = |id: u32| {
        seen.iter()
            .filter(|(i, _)| *i == id)
            .map(|(_, v)| *v)
            .collect::<Vec<_>>()
    };
    assert_eq!(values_of(1000), vec![note as f64], "{seen:?}");
    assert_eq!(values_of(1001), vec![note as f64], "{seen:?}");
    assert_eq!(values_of(1002), vec![note as f64; 3], "{seen:?}");
    // CLAP's tuning is in semitones, brightness and pressure in 0..1.
    let expr_value = |id: i32| values_of(1100 + id as u32);
    assert_eq!(
        expr_value(clap_sys::events::CLAP_NOTE_EXPRESSION_TUNING),
        vec![2.5]
    );
    assert_eq!(
        expr_value(clap_sys::events::CLAP_NOTE_EXPRESSION_BRIGHTNESS),
        vec![0.75]
    );
    assert_eq!(
        expr_value(clap_sys::events::CLAP_NOTE_EXPRESSION_PRESSURE),
        vec![0.5]
    );
}

#[test]
fn an_unassigned_note_id_reaches_a_clap_plugin_as_minus_one() {
    let seen = run_events(
        "rlive-clap-no-id",
        &[MidiEvent::new(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: 0,
                tuning: 0.0,
            },
        )],
    );
    assert_eq!(seen, vec![(1000, -1.0)]);
}

#[test]
fn scanner_reports_broken_files() {
    let dir = std::env::temp_dir().join(format!("rlive-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("broken.clap");
    std::fs::write(&bad, b"not a library").unwrap();
    let exe = Path::new(env!("CARGO_BIN_EXE_rlive-plugin-scanner"));
    let r = scanner::scan_out_of_process(exe, PluginFormat::Clap, &bad, Duration::from_secs(10));
    assert!(r.is_err());
    std::fs::remove_dir_all(&dir).ok();
}

/// The fixture gain is sample-accurate, adds its key input and copies half of it to its aux bus.
fn with_routing(
    p: &mut dyn rlive_core::Processor,
    params: &[ParamEvent],
    sidechain: Option<(&[f32], &[f32])>,
    aux: Option<(&mut [f32], &mut [f32])>,
) -> Vec<f32> {
    let transport = TransportInfo {
        playing: true,
        ..Default::default()
    };
    let (mut l, mut r) = (vec![0.5f32; 512], vec![0.5f32; 512]);
    let mut q = ParamOutQueue::with_capacity(16);
    let mut events_out = EventQueue::with_capacity(0);
    let view = ViewWriter::none();
    let mut aux_buses: Vec<AudioOutput<'_>> = aux
        .into_iter()
        .map(|(left, right)| AudioOutput { left, right })
        .collect();
    p.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: sidechain.map(|(left, right)| AudioInput { left, right }),
        aux_out: &mut aux_buses,
        events: &[],
        events_out: &mut events_out,
        params,
        transport: &transport,
        param_out: &mut q,
        mod_out: &mut [],
        view: &view,
    });
    l
}

#[test]
fn clap_params_land_on_their_frame_and_buses_are_forwarded() {
    let path = fixture_named("rlive-clap-routing");
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let mut inst = instantiate(find(&found, "rs.rlive.test-gain"), 48_000.0, 512).unwrap();

    // Reported by the host side and readable from the processor half.
    assert_eq!(inst.handle.latency(), 64);
    assert_eq!(inst.processor.latency(), 64);
    assert_eq!(inst.processor.tail(), Tail::Frames(480));

    let change = ParamEvent {
        offset: 200,
        id: ParamId(0),
        value: 0.5,
    };
    let out = with_routing(&mut *inst.processor, &[change], None, None);
    assert!((out[199] - 0.5).abs() < 1e-6, "{}", out[199]);
    assert!((out[200] - 0.25).abs() < 1e-6, "{}", out[200]);

    let (key_l, key_r) = (vec![0.2f32; 512], vec![0.2f32; 512]);
    let (mut aux_l, mut aux_r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let out = with_routing(
        &mut *inst.processor,
        &[],
        Some((&key_l, &key_r)),
        Some((&mut aux_l, &mut aux_r)),
    );
    assert!((out[0] - 0.45).abs() < 1e-6, "gain then key: {}", out[0]);
    assert!((aux_l[0] - 0.1).abs() < 1e-6, "{}", aux_l[0]);
    assert!((aux_r[100] - 0.1).abs() < 1e-6, "{}", aux_r[100]);

    let out = with_routing(&mut *inst.processor, &[], None, None);
    assert!((out[0] - 0.25).abs() < 1e-6, "no key, silence: {}", out[0]);

    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// A copy of the fixture named `name` in a folder of its own.
fn fixture_as(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let source = fixture();
    let dest = dir.join(name);
    std::fs::copy(&source, &dest).unwrap();
    std::fs::remove_dir_all(source.parent().unwrap()).ok();
    dest
}

fn scan_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn clap_sources(dir: &Path) -> scanner::ScanSources {
    scanner::ScanSources {
        clap_dirs: vec![dir.to_path_buf()],
        ..Default::default()
    }
}

fn rescan(
    cache: &mut scanner::PluginCache,
    sources: &scanner::ScanSources,
    timeout: Duration,
) -> (scanner::ScanReport, Vec<PathBuf>) {
    let exe = Path::new(env!("CARGO_BIN_EXE_rlive-plugin-scanner"));
    let mut seen = Vec::new();
    let report = cache.rescan_sources(exe, sources, timeout, |p| seen.push(p.to_path_buf()));
    (report, seen)
}

#[test]
fn a_crashing_plugin_is_recorded_and_the_scan_carries_on() {
    let dir = scan_dir("rlive-scan-crash");
    let bad = fixture_as(&dir, "a-crash-on-scan.clap");
    let good = fixture_as(&dir, "b-good.clap");
    let sources = clap_sources(&dir);
    let mut cache = scanner::PluginCache::default();

    // First crash: the other plug-in is still found, and the culprit is named.
    let (report, seen) = rescan(&mut cache, &sources, Duration::from_secs(30));
    assert_eq!(seen, vec![bad.clone(), good.clone()]);
    assert_eq!(report.crashed, vec![bad.clone()]);
    assert!(report.disabled.is_empty());
    assert_eq!(cache.plugins.len(), 3, "{:?}", cache.plugins);
    assert!(cache.plugins.iter().all(|p| p.path == good));
    let culprit = cache.files.iter().find(|f| f.path == bad).unwrap();
    assert!(!culprit.ok);
    assert_eq!(culprit.crashes, 1);
    assert!(!culprit.disabled);
    assert!(culprit.error.as_deref().unwrap().contains("crash"));

    // It is tried again, and a second crash takes it out of circulation.
    let (report, seen) = rescan(&mut cache, &sources, Duration::from_secs(30));
    assert_eq!(seen, vec![bad.clone()], "the good one came from the cache");
    assert!(report.crashed.is_empty());
    assert_eq!(report.disabled, vec![bad.clone()]);
    assert!(cache.files.iter().find(|f| f.path == bad).unwrap().disabled);
    assert_eq!(cache.plugins.len(), 3);

    // Once disabled, it isn't even loaded.
    let (report, seen) = rescan(&mut cache, &sources, Duration::from_secs(30));
    assert!(seen.is_empty(), "{seen:?}");
    assert!(report.is_clean());

    // A reinstall (a newer file) gets another chance.
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    let file = std::fs::File::options().write(true).open(&bad).unwrap();
    file.set_modified(later).unwrap();
    let (_, seen) = rescan(&mut cache, &sources, Duration::from_secs(30));
    assert_eq!(seen, vec![bad.clone()]);
    assert_eq!(
        cache.files.iter().find(|f| f.path == bad).unwrap().crashes,
        1
    );

    // Disabling by hand does what the second crash does.
    cache.set_disabled(&bad, true);
    let (report, seen) = rescan(&mut cache, &sources, Duration::from_secs(30));
    assert!(seen.is_empty() && report.is_clean());

    // A cache that survives a save and a load behaves the same.
    let saved = dir.join("plugins.json");
    cache.save(&saved).unwrap();
    let mut loaded = scanner::PluginCache::load(&saved);
    assert_eq!(loaded, cache);
    assert!(rescan(&mut loaded, &sources, Duration::from_secs(30))
        .1
        .is_empty());

    // And a clean rebuild forgets all of it.
    loaded.clear();
    let (report, seen) = rescan(&mut loaded, &sources, Duration::from_secs(30));
    assert_eq!(seen.len(), 2);
    assert_eq!(report.crashed, vec![bad]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_hanging_plugin_is_killed_and_treated_as_a_crash() {
    let dir = scan_dir("rlive-scan-hang");
    let bad = fixture_as(&dir, "hang-on-scan.clap");
    let mut cache = scanner::PluginCache::default();
    let (report, _) = rescan(&mut cache, &clap_sources(&dir), Duration::from_millis(300));
    assert_eq!(report.crashed, vec![bad]);
    assert!(cache.files[0]
        .error
        .as_deref()
        .unwrap()
        .contains("timed out"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_broken_file_is_a_failure_not_a_crash() {
    let dir = scan_dir("rlive-scan-broken");
    std::fs::write(dir.join("broken.clap"), b"not a library").unwrap();
    let mut cache = scanner::PluginCache::default();
    let (report, seen) = rescan(&mut cache, &clap_sources(&dir), Duration::from_secs(30));
    assert_eq!(seen.len(), 1);
    assert!(report.is_clean());
    assert_eq!(cache.files[0].crashes, 0);
    assert!(!cache.files[0].ok);
    // It isn't looked at again until it changes.
    assert!(
        rescan(&mut cache, &clap_sources(&dir), Duration::from_secs(30))
            .1
            .is_empty()
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Runs a block with room for MIDI out, giving back what came out.
fn process_midi(p: &mut dyn rlive_core::Processor, events: &[MidiEvent]) -> Vec<MidiEvent> {
    let transport = TransportInfo {
        playing: true,
        ..Default::default()
    };
    let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut q = ParamOutQueue::with_capacity(64);
    let mut out = EventQueue::with_capacity(64);
    let view = ViewWriter::none();
    p.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: None,
        aux_out: &mut [],
        events,
        events_out: &mut out,
        params: &[],
        transport: &transport,
        param_out: &mut q,
        mod_out: &mut [],
        view: &view,
    });
    out.as_slice().to_vec()
}

fn note(offset: u32, key: u8, on: bool) -> MidiEvent {
    MidiEvent::new(
        offset,
        if on {
            MidiMessage::NoteOn {
                channel: 0,
                key,
                velocity: 1.0,
                note_id: 0,
                tuning: 0.0,
            }
        } else {
            MidiMessage::NoteOff {
                channel: 0,
                key,
                velocity: 0.0,
                note_id: 0,
            }
        },
    )
}

#[test]
fn clap_midi_output_reaches_the_next_device() {
    let path = fixture_named("rlive-clap-midi-out");
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let desc = find(&found, "rs.rlive.test-midi");
    assert!(desc.is_midi_effect);
    assert!(!desc.is_instrument);
    let mut inst = instantiate(desc, 48_000.0, 512).unwrap();
    assert!(inst.handle.midi_output());

    let out = process_midi(&mut *inst.processor, &[note(10, 60, true)]);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0].offset, 10);
    assert!(matches!(
        out[0].message,
        MidiMessage::NoteOn { key: 72, .. }
    ));
    assert!(matches!(
        out[1].message,
        MidiMessage::ControlChange {
            cc: 11,
            value: 60,
            ..
        }
    ));
    let out = process_midi(&mut *inst.processor, &[note(3, 60, false)]);
    assert!(matches!(
        out[0].message,
        MidiMessage::NoteOff { key: 72, .. }
    ));

    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn a_plugin_without_a_note_output_reports_none() {
    let path = fixture_named("rlive-clap-no-midi-out");
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let inst = instantiate(find(&found, "rs.rlive.test-gain"), 48_000.0, 512).unwrap();
    assert!(!inst.handle.midi_output());
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn clap_extra_buses_use_their_own_names() {
    let path = fixture_named("rlive-clap-names");
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let inst = instantiate(find(&found, "rs.rlive.test-gain"), 48_000.0, 512).unwrap();
    assert!(inst.handle.sidechain_input());
    assert_eq!(inst.handle.aux_outputs(), vec!["Aux".to_string()]);
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

fn mpe_mode(p: &mut dyn rlive_core::Processor, output: Option<rlive_core::MpeOutput>) {
    let resource: rlive_core::Resource = std::sync::Arc::new(output);
    assert!(p.set_resource(rlive_core::MPE_SLOT, resource).is_some());
}

#[test]
fn mpe_mode_plays_a_plugin_that_understands_it_on_member_channels() {
    let path = fixture_named("rlive-clap-mpe-in");
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let mut inst = instantiate(find(&found, "rs.rlive.test-midi"), 48_000.0, 512).unwrap();

    // Without MPE Mode it takes CLAP's own note events.
    let mut q = ParamOutQueue::with_capacity(64);
    process(&mut *inst.processor, 0.0, &[note(0, 60, true)], &mut q);
    assert!(q.drain().all(|(id, _)| !(1200..1300).contains(&id)));
    process(&mut *inst.processor, 0.0, &[note(0, 60, false)], &mut q);
    q.drain().count();

    mpe_mode(
        &mut *inst.processor,
        Some(rlive_core::MpeOutput::Zone {
            zone: rlive_core::MpeZone::Lower,
            members: 15,
        }),
    );
    let slide = MidiEvent::new(
        0,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 64,
            note_id: 7,
            kind: ExprKind::Slide,
            value: 0.5,
        },
    );
    let on = MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key: 64,
            velocity: 1.0,
            note_id: 7,
            tuning: 0.0,
        },
    );
    process(&mut *inst.processor, 0.0, &[on, slide], &mut q);
    let seen: Vec<_> = q.drain().collect();
    // Channel 1 is the zone's manager, so the note goes out on a member channel.
    let member_note = seen
        .iter()
        .find(|(id, _)| (1201..1216).contains(id))
        .unwrap_or_else(|| panic!("{seen:?}"));
    assert_eq!(member_note.1, 64.0);
    assert!(
        seen.iter().any(|(id, _)| (1301..1316).contains(id)),
        "{seen:?}"
    );

    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mpe_mode_decodes_what_the_plugin_sends_back() {
    let path = fixture_named("rlive-clap-mpe-out");
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let mut inst = instantiate(find(&found, "rs.rlive.test-midi"), 48_000.0, 512).unwrap();
    // Off: the note comes back as a plain event. On: its member-channel note gets an id.
    let off = process_midi(&mut *inst.processor, &[note(0, 60, true)]);
    assert!(matches!(
        off[0].message,
        MidiMessage::NoteOn { note_id: 0, .. }
    ));
    process_midi(&mut *inst.processor, &[note(0, 60, false)]);

    mpe_mode(
        &mut *inst.processor,
        Some(rlive_core::MpeOutput::Zone {
            zone: rlive_core::MpeZone::Lower,
            members: 15,
        }),
    );
    // In MPE Mode it hears MIDI on a member channel and answers there, which decodes.
    let out = process_midi(&mut *inst.processor, &[note(0, 60, true)]);
    assert!(
        out.iter().any(|e| matches!(
            e.message,
            MidiMessage::NoteOn { note_id, .. } if note_id != 0
        )),
        "{out:?}"
    );

    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// The fixture adds its key to the output, so what it does shows up in the output sample.
fn keyed_output(p: &mut dyn rlive_core::Processor, params: &[ParamEvent], key: Option<f32>) -> f32 {
    let (key_l, key_r) = (vec![key.unwrap_or(0.0); 512], vec![key.unwrap_or(0.0); 512]);
    with_routing(p, params, key.map(|_| (&key_l[..], &key_r[..])), None)[0]
}

#[test]
fn sidechain_gain_mix_and_mute_shape_the_key() {
    use rlive_core::param::plugin_sidechain as sc;
    use rlive_core::Processor;
    let path = fixture_named("rlive-clap-keyed");
    let found = rlive_plugin_host::clap::scan(&path).unwrap();
    let inst = instantiate(find(&found, "rs.rlive.test-gain"), 48_000.0, 512).unwrap();
    let mut keyed = rlive_plugin_host::keyed::KeyedProcessor::new(inst.processor, 512);
    let set = |id: ParamId, value: f64| ParamEvent {
        offset: 0,
        id,
        value,
    };
    let dry = 0.5;
    assert!(
        (keyed_output(&mut keyed, &[], Some(0.2)) - (dry + 0.2)).abs() < 1e-5,
        "untouched by default"
    );
    let out = keyed_output(&mut keyed, &[set(sc::GAIN, 6.0206)], Some(0.2));
    assert!((out - (dry + 0.4)).abs() < 1e-3, "6 dB: {out}");
    let out = keyed_output(
        &mut keyed,
        &[set(sc::GAIN, 0.0), set(sc::MIX, 50.0)],
        Some(0.2),
    );
    assert!(
        (out - (dry + 0.1 + dry * 0.5)).abs() < 1e-5,
        "half key, half its own signal: {out}"
    );
    let out = keyed_output(&mut keyed, &[set(sc::MIX, 0.0)], Some(0.2));
    assert!(
        (out - (dry + dry)).abs() < 1e-5,
        "key out of the mix: {out}"
    );
    let out = keyed_output(
        &mut keyed,
        &[set(sc::MIX, 100.0), set(sc::MUTE, 1.0)],
        Some(0.2),
    );
    assert!((out - dry).abs() < 1e-5, "muted: {out}");
    let out = keyed_output(&mut keyed, &[set(sc::MUTE, 0.0)], None);
    assert!((out - dry).abs() < 1e-5, "nothing routed: {out}");

    // Everything else still reaches the plug-in, and from a queued setting too.
    keyed.set_param(0, 0.5);
    keyed.set_param(sc::GAIN.0, 0.0);
    assert!((keyed_output(&mut keyed, &[], None) - 0.25).abs() < 1e-5);
    keyed.stop_processing();
    drop(keyed);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
