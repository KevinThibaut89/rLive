//! End-to-end VST3 hosting test against the `rlive-test-vst3` fixture plugin.

use rlive_core::{
    AudioBuffer, AudioInput, AudioOutput, EventQueue, ExprKind, MidiEvent, MidiMessage,
    ParamOutQueue, PluginFormat, ProcessContext, TransportInfo, ViewWriter,
};
use rlive_plugin_host::{instantiate, instantiate_with, scanner, PluginBuses};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Build the fixture and wrap it in a `.vst3` bundle directory. The fixture is built once per
/// test run and each caller gets its own bundle, so parallel tests can't race on it.
fn fixture() -> PathBuf {
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
            .args(["build", "-q", "-p", "rlive-test-vst3", "--target-dir"])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building rlive-test-vst3 failed");
        target.join("debug").join(format!(
            "{}rlive_test_vst3.{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_EXTENSION
        ))
    });
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rlive-vst3-{}-{n}", std::process::id()));
    let bundle = dir.join("rlive-test.vst3");
    let (sub, name) = if cfg!(target_os = "macos") {
        ("MacOS", "rlive-test".to_string())
    } else if cfg!(target_os = "windows") {
        ("x86_64-win", "rlive-test.vst3".to_string())
    } else if cfg!(target_arch = "aarch64") {
        ("aarch64-linux", "rlive-test.so".to_string())
    } else {
        ("x86_64-linux", "rlive-test.so".to_string())
    };
    let bin_dir = bundle.join("Contents").join(sub);
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::copy(lib, bin_dir.join(name)).unwrap();
    bundle
}

fn process(p: &mut dyn rlive_core::Processor, input: f32) -> Vec<f32> {
    let transport = TransportInfo::default();
    let mut q = ParamOutQueue::with_capacity(16);
    let mut events_out = EventQueue::with_capacity(0);
    let view = ViewWriter::none();
    let mut l = vec![input; 256];
    let mut r = vec![input; 256];
    p.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: None,
        aux_out: &mut [],
        events: &[],
        events_out: &mut events_out,
        params: &[],
        transport: &transport,
        param_out: &mut q,
        mod_out: &mut [],
        view: &view,
    });
    l
}

fn process_keyed(p: &mut dyn rlive_core::Processor, key: f32) -> f32 {
    let frames = 64;
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let key_buf = vec![key; frames];
    let mut events_out = EventQueue::with_capacity(0);
    let mut q = ParamOutQueue::with_capacity(0);
    let transport = TransportInfo::default();
    let view = ViewWriter::none();
    p.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: Some(AudioInput {
            left: &key_buf,
            right: &key_buf,
        }),
        aux_out: &mut [],
        events: &[],
        events_out: &mut events_out,
        params: &[],
        transport: &transport,
        param_out: &mut q,
        mod_out: &mut [],
        view: &view,
    });
    l[10]
}

#[test]
fn a_vst3_sidechain_bus_only_comes_on_when_something_is_routed_to_it() {
    let bundle = fixture();
    let exe = Path::new(env!("CARGO_BIN_EXE_rlive-plugin-scanner"));
    let found =
        scanner::scan_out_of_process(exe, PluginFormat::Vst3, &bundle, Duration::from_secs(30))
            .unwrap();
    let desc = &found[0];

    let mut plain = instantiate(desc, 48_000.0, 256).unwrap();
    assert!(plain.handle.sidechain_input(), "the plugin offers one");
    assert_eq!(process_keyed(&mut *plain.processor, 0.5), 0.0, "bus off");
    plain.processor.stop_processing();
    drop(plain);

    let buses = PluginBuses {
        sidechain: true,
        aux: false,
    };
    let mut keyed = instantiate_with(desc, 48_000.0, 256, buses).unwrap();
    assert!(
        (process_keyed(&mut *keyed.processor, 0.5) - 0.5).abs() < 1e-6,
        "bus on: the key reaches the plugin"
    );
    keyed.processor.stop_processing();
    drop(keyed);
    std::fs::remove_dir_all(bundle.parent().unwrap()).ok();
}

#[test]
fn vst3_scan_instantiate_process_state() {
    let bundle = fixture();
    let exe = Path::new(env!("CARGO_BIN_EXE_rlive-plugin-scanner"));
    let found =
        scanner::scan_out_of_process(exe, PluginFormat::Vst3, &bundle, Duration::from_secs(30))
            .unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let desc = &found[0];
    assert_eq!(desc.format, PluginFormat::Vst3);
    assert!(desc.name.starts_with("Gain"));
    assert_eq!(desc.id.len(), 32);

    let mut inst = instantiate(desc, 48_000.0, 256).unwrap();
    let params = inst.handle.params();
    assert_eq!(params.len(), 1, "{params:?}");
    assert_eq!(params[0].id, 0);

    let out = process(&mut *inst.processor, 0.5);
    assert!((out[10] - 0.5).abs() < 1e-6, "{}", out[10]);
    inst.processor.set_param(0, 0.5);
    inst.handle.on_param_set(0, 0.5);
    let out = process(&mut *inst.processor, 0.5);
    assert!((out[10] - 0.25).abs() < 1e-6, "{}", out[10]);
    assert_eq!(inst.handle.param_value(0), Some(0.5));
    assert!(inst.handle.format_value(0, 0.5).is_some());

    let state = inst.handle.save_state().expect("state");
    assert!(inst.handle.load_state(&state));
    assert!(inst.handle.idle().is_empty());

    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(bundle.parent().unwrap()).ok();
}

fn gain_desc(path: &Path) -> rlive_plugin_host::PluginDescriptor {
    let found = scanner::scan_out_of_process(
        Path::new(env!("CARGO_BIN_EXE_rlive-plugin-scanner")),
        PluginFormat::Vst3,
        path,
        Duration::from_secs(30),
    )
    .unwrap();
    found.into_iter().next().unwrap()
}

fn key_and_aux(p: &mut dyn rlive_core::Processor, key: f32) -> (f32, f32) {
    let transport = TransportInfo::default();
    let mut q = ParamOutQueue::with_capacity(16);
    let mut events_out = EventQueue::with_capacity(0);
    let view = ViewWriter::none();
    let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    let (key_l, key_r) = (vec![key; 64], vec![key; 64]);
    let (mut aux_l, mut aux_r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    let mut aux = [AudioOutput {
        left: &mut aux_l,
        right: &mut aux_r,
    }];
    p.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: Some(AudioInput {
            left: &key_l,
            right: &key_r,
        }),
        aux_out: &mut aux,
        events: &[],
        events_out: &mut events_out,
        params: &[],
        transport: &transport,
        param_out: &mut q,
        mod_out: &mut [],
        view: &view,
    });
    (l[5], aux_l[5])
}

#[test]
fn the_aux_output_only_runs_once_it_is_switched_on() {
    let path = fixture();
    let desc = gain_desc(&path);
    let mut plain = instantiate(&desc, 48_000.0, 256).unwrap();
    assert_eq!(plain.handle.aux_outputs(), vec!["Aux".to_string()]);
    assert_eq!(key_and_aux(&mut *plain.processor, 0.4), (0.0, 0.0));
    plain.processor.stop_processing();
    drop(plain);

    let buses = PluginBuses {
        sidechain: true,
        aux: true,
    };
    let mut wired = instantiate_with(&desc, 48_000.0, 256, buses).unwrap();
    let (main, aux) = key_and_aux(&mut *wired.processor, 0.4);
    assert!(
        (main - 0.4).abs() < 1e-6,
        "key reaches the main output: {main}"
    );
    assert!((aux - 0.2).abs() < 1e-6, "and the aux: {aux}");
    wired.processor.stop_processing();
    drop(wired);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

fn run_block(
    p: &mut dyn rlive_core::Processor,
    events: &[MidiEvent],
) -> (Vec<MidiEvent>, Vec<(u32, f64)>) {
    let transport = TransportInfo::default();
    let mut q = ParamOutQueue::with_capacity(64);
    let mut events_out = EventQueue::with_capacity(64);
    let view = ViewWriter::none();
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
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
        param_out: &mut q,
        mod_out: &mut [],
        view: &view,
    });
    (events_out.as_slice().to_vec(), q.drain().collect())
}

fn note_on(offset: u32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0.75,
            note_id: 9,
            tuning: 0.0,
        },
    )
}

#[test]
fn vst3_events_come_out_as_midi() {
    let path = fixture();
    let desc = gain_desc(&path);
    let mut inst = instantiate(&desc, 48_000.0, 256).unwrap();
    assert!(inst.handle.midi_output());
    let (out, _) = run_block(&mut *inst.processor, &[note_on(12)]);
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(out[0].offset, 12);
    assert!(matches!(
        out[0].message,
        MidiMessage::NoteOn {
            key: 72,
            note_id: 9,
            ..
        }
    ));
    assert!(matches!(
        out[1].message,
        MidiMessage::ControlChange {
            channel: 2,
            cc: 11,
            value: 99
        }
    ));
    match out[2].message {
        MidiMessage::PitchBend { channel: 2, value } => {
            assert!((value - 0.5).abs() < 1e-3, "{value}")
        }
        ref m => panic!("{m:?}"),
    }
    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mapped_controllers_reach_their_parameters() {
    let path = fixture();
    let desc = gain_desc(&path);
    let mut inst = instantiate(&desc, 48_000.0, 256).unwrap();
    let cc = MidiEvent::new(
        3,
        MidiMessage::ControlChange {
            channel: 0,
            cc: 1,
            value: 127,
        },
    );
    let bend = MidiEvent::new(
        7,
        MidiMessage::PitchBend {
            channel: 0,
            value: 0.0,
        },
    );
    let unmapped = MidiEvent::new(
        8,
        MidiMessage::ControlChange {
            channel: 3,
            cc: 1,
            value: 10,
        },
    );
    let (_, params) = run_block(&mut *inst.processor, &[cc, bend, unmapped]);
    assert_eq!(params.len(), 2, "{params:?}");
    assert!(params.contains(&(1, 1.0)), "{params:?}");
    assert!(params.contains(&(2, 0.5)), "{params:?}");
    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mpe_mode_sends_a_vst3_each_notes_bend_on_its_own_channel() {
    let path = fixture();
    let desc = gain_desc(&path);
    let mut inst = instantiate(&desc, 48_000.0, 256).unwrap();
    let output = rlive_core::MpeOutput::Zone {
        zone: rlive_core::MpeZone::Lower,
        members: 15,
    };
    let resource: rlive_core::Resource = std::sync::Arc::new(Some(output));
    assert!(inst
        .processor
        .set_resource(rlive_core::MPE_SLOT, resource)
        .is_some());
    let pitch = MidiEvent::new(
        5,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 9,
            kind: ExprKind::Pitch,
            value: 48.0,
        },
    );
    // Member channel 2 is the first the encoder hands out, and the fixture maps its bend.
    let (_, params) = run_block(&mut *inst.processor, &[note_on(0), pitch]);
    assert!(
        params
            .iter()
            .any(|&(id, v)| id == 3 && (v - 1.0).abs() < 1e-3),
        "{params:?}"
    );
    inst.processor.stop_processing();
    drop(inst);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn a_crashing_vst3_only_loses_its_own_scan() {
    let dir = std::env::temp_dir().join(format!("rlive-vst3-crash-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let copy = fixture();
    let bad = dir.join("crash-on-scan.vst3");
    let good = dir.join("good.vst3");
    std::fs::create_dir_all(&dir).unwrap();
    let cp = |to: &Path| {
        assert!(Command::new("cp")
            .args(["-R"])
            .arg(&copy)
            .arg(to)
            .status()
            .unwrap()
            .success())
    };
    cp(&bad);
    cp(&good);
    let exe = Path::new(env!("CARGO_BIN_EXE_rlive-plugin-scanner"));
    let mut cache = scanner::PluginCache::default();
    let sources = scanner::ScanSources {
        vst3_dirs: vec![dir.clone()],
        ..Default::default()
    };
    let report = cache.rescan_sources(exe, &sources, Duration::from_secs(30), |_| {});
    assert_eq!(report.crashed, vec![bad]);
    assert_eq!(cache.plugins.len(), 1);
    assert_eq!(cache.plugins[0].path, good);
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(copy.parent().unwrap()).ok();
}
