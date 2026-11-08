//! End-to-end VST3 hosting test against the `rlive-test-vst3` fixture plugin.

use rlive_core::{AudioBuffer, ParamOutQueue, PluginFormat, ProcessContext, TransportInfo};
use rlive_plugin_host::{instantiate, scanner};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Build the fixture and wrap it in a `.vst3` bundle directory.
fn fixture() -> PathBuf {
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
    let lib = target.join("debug").join(format!(
        "{}rlive_test_vst3.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    ));
    let dir = std::env::temp_dir().join(format!("rlive-vst3-{}", std::process::id()));
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
    std::fs::copy(&lib, bin_dir.join(name)).unwrap();
    bundle
}

fn process(p: &mut dyn rlive_core::Processor, input: f32) -> Vec<f32> {
    let transport = TransportInfo::default();
    let mut q = ParamOutQueue::with_capacity(16);
    let mut l = vec![input; 256];
    let mut r = vec![input; 256];
    p.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        events: &[],
        transport: &transport,
        param_out: &mut q,
    });
    l
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
