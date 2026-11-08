//! End-to-end VST3 hosting test against the `rlive-test-vst3` fixture plugin.

use rlive_core::{
    AudioBuffer, AudioInput, EventQueue, ParamOutQueue, PluginFormat, ProcessContext,
    TransportInfo, ViewWriter,
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
        let target = root.join("target/test-plugins");
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
