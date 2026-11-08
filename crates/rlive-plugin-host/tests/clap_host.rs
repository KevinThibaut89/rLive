//! End-to-end CLAP hosting test against the `rlive-test-clap` fixture plugins.

use rlive_core::{
    AudioBuffer, AudioInput, AudioOutput, EventQueue, MidiEvent, MidiMessage, ParamEvent, ParamId,
    ParamOutQueue, PluginFormat, ProcessContext, Tail, TransportInfo, ViewWriter,
};
use rlive_plugin_host::{instantiate, scanner, PluginDescriptor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Build the fixture cdylib (in its own target dir to avoid fighting over cargo's lock)
/// and copy it to a `.clap` file.
fn fixture() -> PathBuf {
    fixture_named("rlive-clap")
}

/// The same, in a directory of its own so tests can clean up after themselves.
fn fixture_named(dir_prefix: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target/test-plugins");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-q", "-p", "rlive-test-clap", "--target-dir"])
        .arg(&target)
        .current_dir(&root)
        .status()
        .expect("run cargo");
    assert!(status.success(), "building rlive-test-clap failed");
    let lib = target.join("debug").join(format!(
        "{}rlive_test_clap.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    ));
    let dir = std::env::temp_dir().join(format!("{dir_prefix}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("rlive-test.clap");
    std::fs::copy(&lib, &dest).unwrap();
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
    assert_eq!(found.len(), 2);
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
