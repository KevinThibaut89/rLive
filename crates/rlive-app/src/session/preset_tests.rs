//! Presets, defaults, Hot-Swap, A/B and the Undo History through a session.

use super::*;
use crate::session::{PadDrop, SwapItem, SwapTarget};
use rlive_browser::defaults::{DropTarget, UserLibrary};
use rlive_browser::item::Role;
use rlive_browser::preset::{Payload, Preset, PRESET_FORMAT};
use rlive_core::param::{EnvTarget, ParamOwner, ParamRef};
use rlive_core::{ChainId, ParamId};
use rlive_engine::{create_engine, Engine};
use rlive_project::{
    Breakpoint, CompareSlot, DeviceKind, DrumPad, Route, SidechainSource, Tap, Zones,
};
use std::path::{Path, PathBuf};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "rlive-presets-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Rig {
    _engine: Engine,
    s: Session,
    a: TrackId,
    b: TrackId,
}

fn rig() -> Rig {
    let mut p = Project::new_default();
    p.tracks[0].devices.clear();
    let a = p.tracks[0].id;
    let mut second = rlive_project::Track::new("B", TrackKind::Audio, [1, 2, 3], p.scenes.len());
    second
        .devices
        .push(Device::builtin("rlive.compressor", "Compressor"));
    let b = second.id;
    p.tracks.push(second);
    let (engine, handle) = create_engine(SR, BLOCK);
    let s = Session::new(handle, p, PluginCache::default());
    Rig {
        _engine: engine,
        s,
        a,
        b,
    }
}

fn library(temp: &Temp) -> UserLibrary {
    let lib = UserLibrary::new(temp.0.join("library"));
    lib.ensure_folders().unwrap();
    lib
}

fn first_device(s: &Session, chain: ChainRef) -> Device {
    s.devices(chain)[0].clone()
}

fn param_id(type_id: &str, name: &str) -> u32 {
    let info = rlive_devices::find(type_id).unwrap();
    (info.desc)()
        .params
        .iter()
        .find(|p| p.name == name)
        .unwrap()
        .id
}

fn reverb_preset(size: f64) -> Preset {
    let mut d = Device::builtin("rlive.reverb", "Reverb");
    d.params.insert(param_id("rlive.reverb", "Size"), size);
    d.info = "Big room".to_string();
    Preset::of_device(&d, "Bigger")
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "{a} vs {b}");
}

// ------------------------------------------------------------- presets

#[test]
fn a_saved_preset_loads_back_with_its_settings_and_info() {
    let temp = Temp::new("save");
    let mut r = rig();
    r.s.library = Some(library(&temp));
    let chain = ChainRef::Track(r.b);
    let id = first_device(&r.s, chain).id;
    let threshold = param_id("rlive.compressor", "Threshold");
    r.s.set_param(chain, id, threshold, -22.0);
    r.s.commit_edits(false);
    let path = r.s.save_device_preset(chain, id, Some("Tight")).unwrap();
    assert!(path.starts_with(temp.0.join("library/Presets/Audio Effects/Compressor")));

    let preset = Preset::load(&path).unwrap();
    assert_eq!(preset.format, PRESET_FORMAT);
    let loaded = r.s.load_preset(chain, None, &preset).unwrap();
    let d =
        r.s.devices(chain)
            .iter()
            .find(|d| d.id == loaded)
            .unwrap()
            .clone();
    near(d.params[&threshold], -22.0);
    assert_eq!(d.name, "Tight");
    assert_ne!(loaded, id, "it's a device of its own");
    assert!(r.s.runtimes.contains_key(&loaded));
}

#[test]
fn saving_a_preset_collects_its_samples_into_the_library() {
    let temp = Temp::new("collect");
    let mut r = rig();
    let lib = library(&temp);
    r.s.library = Some(lib.clone());
    let sample = temp.0.join("source.wav");
    std::fs::write(&sample, b"riff").unwrap();
    let chain = ChainRef::Track(r.a);
    r.s.add_builtin(chain, "rlive.simpler");
    let id = first_device(&r.s, chain).id;
    if let DeviceKind::Builtin { samples, .. } = &mut r.s.chain_mut(chain).unwrap()[0].kind {
        samples.push(Some(rlive_project::FileRef::new(&sample)));
    }
    let path = r.s.save_device_preset(chain, id, None).unwrap();
    let preset = Preset::load(&path).unwrap();
    let Payload::Device(d) = &preset.payload else {
        panic!()
    };
    let DeviceKind::Builtin { samples, .. } = &d.kind else {
        panic!()
    };
    let saved = samples[0].as_ref().unwrap();
    assert!(saved.path.starts_with(lib.imported_samples_dir()));
    assert!(saved.path.exists());
}

#[test]
fn a_rack_preset_loads_with_fresh_ids_and_working_macros() {
    let mut r = rig();
    let mut rack = Device::rack(rlive_project::RackKind::Audio, "Rack");
    let mut chain = rlive_project::Chain::new("Only");
    let inner = Device::builtin("rlive.utility", "Utility");
    let inner_id = inner.id;
    chain.devices.push(inner);
    if let DeviceKind::Rack { chains, macros, .. } = &mut rack.kind {
        chains.push(chain);
        macros.push(rlive_project::Macro {
            mappings: vec![rlive_project::MacroMapping {
                target: ParamRef {
                    owner: ParamOwner::Device(inner_id),
                    id: ParamId(0),
                },
                min: 0.0,
                max: 1.0,
            }],
            ..Default::default()
        });
    }
    let preset = Preset::of_device(&rack, "Rack Preset");
    let chain_ref = ChainRef::Track(r.b);
    let id = r.s.load_preset(chain_ref, None, &preset).unwrap();
    let loaded =
        r.s.devices(chain_ref)
            .iter()
            .find(|d| d.id == id)
            .unwrap()
            .clone();
    let DeviceKind::Rack { chains, macros, .. } = &loaded.kind else {
        panic!()
    };
    let new_inner = chains[0].devices[0].id;
    assert_ne!(new_inner, inner_id);
    assert_eq!(
        macros[0].mappings[0].target.owner,
        ParamOwner::Device(new_inner),
        "the macro follows the new id"
    );
    assert!(r.s.runtimes.contains_key(&new_inner));
}

#[test]
fn a_preset_of_the_wrong_kind_is_refused_there() {
    let mut p = Project::new_default();
    let group = rlive_project::Track::new("G", TrackKind::Group, [0; 3], p.scenes.len());
    let group_id = group.id;
    p.tracks.push(group);
    let (_engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    let mut d = Device::builtin("rlive.poly", "Poly Synth");
    d.name = "Inst".to_string();
    let instrument = Preset::of_device(&d, "Inst");
    assert!(s
        .load_preset(ChainRef::Track(group_id), None, &instrument)
        .is_err());
    assert!(s.devices(ChainRef::Track(group_id)).is_empty());
}

// ------------------------------------------------------------- Hot-Swap

/// The acceptance test: replace a device and nothing around it notices.
#[test]
fn hot_swap_replaces_a_device_without_losing_routing() {
    let mut r = rig();
    let chain = ChainRef::Track(r.b);
    // The device has a neighbour on either side, a sidechain, and a track routed into it.
    r.s.add_builtin(chain, "rlive.utility");
    r.s.add_builtin(chain, "rlive.limiter");
    let devices: Vec<_> = r.s.devices(chain).iter().map(|d| d.id).collect();
    let target = devices[1];
    r.s.chain_mut(chain).unwrap()[1].sidechain = Some(SidechainSource {
        track: r.a,
        tap: Tap::default(),
    });
    r.s.project.track_mut(r.a).unwrap().routing.output = Route::Track {
        track: r.b,
        device: Some(target),
    };
    let reverb = reverb_preset(0.9);
    r.s.hot_swap(
        &SwapTarget::Device { chain, id: target },
        SwapItem::Preset(&reverb),
    )
    .unwrap();
    r.s.commit_edits(false);

    let now: Vec<_> = r.s.devices(chain).iter().map(|d| d.id).collect();
    assert_eq!(now, devices, "same place, same id, same neighbours");
    let swapped = r.s.devices(chain)[1].clone();
    assert!(
        matches!(&swapped.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.reverb")
    );
    assert_eq!(swapped.name, "Bigger");
    near(swapped.params[&param_id("rlive.reverb", "Size")], 0.9);
    assert_eq!(
        swapped.sidechain,
        Some(SidechainSource {
            track: r.a,
            tap: Tap::default(),
        }),
        "its sidechain stays"
    );
    assert_eq!(
        r.s.project.track(r.a).unwrap().routing.output,
        Route::Track {
            track: r.b,
            device: Some(target),
        },
        "and what was routed to it still points at it"
    );
    assert_eq!(
        r.s.runtimes[&target].role,
        rlive_devices::DeviceRole::AudioEffect
    );
}

#[test]
fn hot_swap_refuses_another_device_type() {
    let mut r = rig();
    let chain = ChainRef::Track(r.b);
    let id = first_device(&r.s, chain).id;
    let err = r.s.hot_swap(
        &SwapTarget::Device { chain, id },
        SwapItem::Builtin("rlive.poly"),
    );
    assert!(err.is_err(), "an audio effect can't become an instrument");
    assert!(matches!(
        &first_device(&r.s, chain).kind,
        DeviceKind::Builtin { type_id, .. } if type_id == "rlive.compressor"
    ));
}

#[test]
fn hot_swapping_the_same_device_type_keeps_its_automation_and_undo_brings_it_back() {
    let mut r = rig();
    let chain = ChainRef::Track(r.b);
    let id = first_device(&r.s, chain).id;
    let threshold = param_id("rlive.compressor", "Threshold");
    let target = EnvTarget::Param(ParamRef {
        owner: ParamOwner::Device(id),
        id: ParamId(threshold),
    });
    r.s.set_param(chain, id, threshold, -30.0);
    r.s.commit_edits(false);
    r.s.project
        .track_mut(r.b)
        .unwrap()
        .automation
        .push(rlive_project::Envelope {
            target,
            points: vec![Breakpoint::new(0.0, 0.5)],
        });
    let mut other = Device::builtin("rlive.compressor", "Softer");
    other.params.insert(threshold, -12.0);
    let preset = Preset::of_device(&other, "Softer");
    r.s.hot_swap(&SwapTarget::Device { chain, id }, SwapItem::Preset(&preset))
        .unwrap();
    r.s.commit_edits(false);
    assert_eq!(
        r.s.project.track(r.b).unwrap().automation.len(),
        1,
        "same parameters, so it stays"
    );
    near(first_device(&r.s, chain).params[&threshold], -12.0);

    assert!(r.s.undo());
    let back = first_device(&r.s, chain);
    assert_eq!(back.id, id);
    near(back.params[&threshold], -30.0);
    assert!(r.s.runtimes.contains_key(&id));
}

#[test]
fn hot_swapping_a_different_device_clears_automation_that_cannot_apply() {
    let mut r = rig();
    let chain = ChainRef::Track(r.b);
    let id = first_device(&r.s, chain).id;
    r.s.project
        .track_mut(r.b)
        .unwrap()
        .automation
        .push(rlive_project::Envelope {
            target: EnvTarget::Param(ParamRef {
                owner: ParamOwner::Device(id),
                id: ParamId(0),
            }),
            points: vec![Breakpoint::new(0.0, 0.5)],
        });
    let reverb = reverb_preset(0.5);
    r.s.hot_swap(&SwapTarget::Device { chain, id }, SwapItem::Preset(&reverb))
        .unwrap();
    assert!(r.s.project.track(r.b).unwrap().automation.is_empty());
    assert!(
        r.s.layouts[&chain].device_cell(id, 0).is_some(),
        "the new device is wired up"
    );
}

#[test]
fn hot_swap_a_sample_replaces_just_the_sample() {
    let temp = Temp::new("swap");
    let mut r = rig();
    let chain = ChainRef::Track(r.a);
    let first = temp.0.join("a.wav");
    let second = temp.0.join("b.wav");
    for p in [&first, &second] {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(p, spec).unwrap();
        for _ in 0..512 {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }
    r.s.add_builtin(chain, "rlive.simpler");
    let id = first_device(&r.s, chain).id;
    r.s.set_device_sample_at(chain, id, 0, first.clone());
    wait_for_samples(&mut r.s);
    let target = SwapTarget::Sample { chain, id, slot: 0 };
    r.s.hot_swap(&target, SwapItem::Sample(&second)).unwrap();
    wait_for_samples(&mut r.s);
    assert_eq!(first_device(&r.s, chain).sample(0).unwrap().path, second);
    assert!(r
        .s
        .hot_swap(&target, SwapItem::Builtin("rlive.reverb"))
        .is_err());
}

fn wait_for_samples(s: &mut Session) {
    for _ in 0..500 {
        s.idle();
        if !s.samples.any_loading() && s.pending_samples.is_empty() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("samples never finished loading");
}

fn drum_rack(s: &mut Session, chain: ChainRef) -> (DeviceId, u8) {
    let mut rack = Device::rack(rlive_project::RackKind::Drum, "Kit");
    let mut c = rlive_project::Chain::new("Pad");
    c.zones = Zones {
        drum: Some(DrumPad {
            receive: Some(36),
            play: 60,
            choke: None,
        }),
        ..Zones::default()
    };
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(c);
    }
    let id = rack.id;
    s.add_device(chain, None, rack);
    (id, 36)
}

#[test]
fn a_preset_hot_swapped_onto_a_pad_fills_that_pad() {
    let mut r = rig();
    let chain = ChainRef::Track(r.a);
    let (rack, note) = drum_rack(&mut r.s, chain);
    let poly = Preset::of_device(&Device::builtin("rlive.poly", "Poly Synth"), "Stab");
    r.s.hot_swap(
        &SwapTarget::Pad { chain, rack, note },
        SwapItem::Preset(&poly),
    )
    .unwrap();
    let id = r.s.chains_on_pad(chain, rack, note)[0];
    let on_pad = chain.child(rack, id).unwrap();
    assert_eq!(r.s.devices(on_pad).len(), 1);
    assert_eq!(r.s.devices(on_pad)[0].name, "Stab");
    assert_eq!(
        r.s.rack(chain, rack).unwrap().chains().count(),
        1,
        "no new pad"
    );
}

#[test]
fn chain_presets_save_and_load_into_a_rack() {
    let temp = Temp::new("chain");
    let mut r = rig();
    r.s.library = Some(library(&temp));
    let chain = ChainRef::Track(r.a);
    let (rack, _) = drum_rack(&mut r.s, chain);
    let id = r.s.rack(chain, rack).unwrap().chains().next().unwrap().id;
    r.s.add_builtin(chain.child(rack, id).unwrap(), "rlive.utility");
    let path =
        r.s.save_chain_preset(chain, rack, id, Some("Saved"))
            .unwrap();
    assert!(path.starts_with(temp.0.join("library/Presets/Chains")));
    let preset = Preset::load(&path).unwrap();
    let new_id: ChainId = r.s.load_chain_preset(chain, rack, None, &preset).unwrap();
    assert_ne!(new_id, id);
    assert_eq!(r.s.rack(chain, rack).unwrap().chains().count(), 2);
    let child = chain.child(rack, new_id).unwrap();
    assert_eq!(r.s.devices(child).len(), 1);
}

// ------------------------------------------------------------- A/B

fn compare_rig() -> (Rig, ChainRef, DeviceId, u32) {
    let r = rig();
    let chain = ChainRef::Track(r.b);
    let id = first_device(&r.s, chain).id;
    let t = param_id("rlive.compressor", "Threshold");
    (r, chain, id, t)
}

#[test]
fn b_keeps_the_values_from_when_the_device_loaded() {
    let (mut r, chain, id, t) = compare_rig();
    assert_eq!(r.s.compare_slot(chain, id), Some(CompareSlot::A));
    let start = r.s.param_value(chain, id, &param_info(&r.s, id, t));
    r.s.set_param(chain, id, t, -40.0);
    r.s.commit_edits(false);
    r.s.compare_switch(chain, id);
    assert_eq!(r.s.compare_slot(chain, id), Some(CompareSlot::B));
    let d = first_device(&r.s, chain);
    assert_eq!(d.params.get(&t), None, "B holds nothing changed");
    assert!(
        (r.s.layouts[&chain]
            .base(r.s.device_cell(chain, id, t).unwrap())
            .unwrap()
            - start)
            .abs()
            < 1e-6
    );
    r.s.compare_switch(chain, id);
    assert_eq!(r.s.compare_slot(chain, id), Some(CompareSlot::A));
    near(first_device(&r.s, chain).params[&t], -40.0);
}

fn param_info(s: &Session, id: DeviceId, param: u32) -> rlive_core::ParamInfo {
    s.runtimes[&id]
        .params
        .iter()
        .find(|p| p.id == param)
        .unwrap()
        .clone()
}

#[test]
fn copy_makes_the_other_state_match() {
    let (mut r, chain, id, t) = compare_rig();
    r.s.set_param(chain, id, t, -40.0);
    r.s.compare_copy(chain, id);
    r.s.set_param(chain, id, t, -10.0);
    r.s.commit_edits(false);
    r.s.compare_switch(chain, id);
    near(first_device(&r.s, chain).params[&t], -40.0);
}

#[test]
fn a_preset_holds_the_selected_state_in_both() {
    let temp = Temp::new("flatten");
    let (mut r, chain, id, t) = compare_rig();
    r.s.library = Some(library(&temp));
    r.s.set_param(chain, id, t, -40.0);
    r.s.commit_edits(false);
    r.s.compare_switch(chain, id);
    r.s.set_param(chain, id, t, -5.0);
    let path = r.s.save_device_preset(chain, id, Some("From B")).unwrap();
    let loaded =
        r.s.load_preset(chain, None, &Preset::load(&path).unwrap())
            .unwrap();
    r.s.commit_edits(false);
    let d =
        r.s.devices(chain)
            .iter()
            .find(|d| d.id == loaded)
            .unwrap()
            .clone();
    near(d.params[&t], -5.0);
    r.s.compare_switch(chain, loaded);
    let d =
        r.s.devices(chain)
            .iter()
            .find(|d| d.id == loaded)
            .unwrap()
            .clone();
    near(d.params[&t], -5.0);
}

#[test]
fn switching_state_can_be_undone() {
    let (mut r, chain, id, t) = compare_rig();
    r.s.set_param(chain, id, t, -40.0);
    r.s.commit_edits(false);
    r.s.compare_switch(chain, id);
    r.s.commit_edits(false);
    assert!(r.s.undo());
    assert_eq!(r.s.compare_slot(chain, id), Some(CompareSlot::A));
    near(first_device(&r.s, chain).params[&t], -40.0);
}

#[test]
fn switching_state_leaves_automation_off_until_reenabled() {
    let (mut r, chain, id, t) = compare_rig();
    let target = EnvTarget::Param(ParamRef {
        owner: ParamOwner::Device(id),
        id: ParamId(t),
    });
    let cell = r.s.device_cell(chain, id, t).unwrap();
    r.s.project
        .track_mut(r.b)
        .unwrap()
        .automation
        .push(rlive_project::Envelope {
            target,
            points: vec![Breakpoint::new(0.0, 0.5)],
        });
    r.s.send_routes(TrackRef::Track(r.b));
    render(&mut r);
    assert!(r.s.cell_state(chain, cell).automated());
    assert!(!r.s.cell_state(chain, cell).overridden());
    r.s.compare_switch(chain, id);
    render(&mut r);
    assert!(
        r.s.cell_state(chain, cell).overridden(),
        "automation is off for it now"
    );
    r.s.re_enable_cell(chain, cell);
    render(&mut r);
    assert!(!r.s.cell_state(chain, cell).overridden(), "per parameter");
}

fn render(r: &mut Rig) {
    let (mut l, mut rr) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    for _ in 0..4 {
        r._engine.process_stereo(&mut l, &mut rr);
        r.s.idle();
    }
}

#[test]
fn a_rack_has_no_ab() {
    let mut r = rig();
    let chain = ChainRef::Track(r.a);
    let (rack, _) = drum_rack(&mut r.s, chain);
    assert_eq!(r.s.compare_slot(chain, rack), None);
    r.s.compare_switch(chain, rack);
    assert_eq!(r.s.rack(chain, rack).unwrap().compare, None);
}

// ------------------------------------------------------------- defaults

fn save_default(
    lib: &UserLibrary,
    role: Role,
    name: &str,
    type_id: &str,
    param: &str,
    value: f64,
) -> PathBuf {
    let mut d = Device::builtin(type_id, name);
    d.params.insert(param_id(type_id, param), value);
    let preset = Preset::of_device(&d, name);
    let path = lib.device_default_path(role, name);
    preset.save(&path).unwrap();
    path
}

#[test]
fn a_new_device_loads_its_default_project_before_library_before_factory() {
    let temp = Temp::new("precedence");
    let mut r = rig();
    let lib = library(&temp);
    r.s.library = Some(lib.clone());
    let chain = ChainRef::Track(r.a);
    let size = param_id("rlive.reverb", "Size");

    r.s.add_builtin(chain, "rlive.reverb");
    assert_eq!(
        r.s.devices(chain)[0].params.get(&size),
        None,
        "factory settings"
    );

    save_default(
        &lib,
        Role::AudioEffect,
        "Reverb",
        "rlive.reverb",
        "Size",
        0.31,
    );
    r.s.add_builtin(chain, "rlive.reverb");
    near(r.s.devices(chain)[1].params[&size], 0.31);
    assert_eq!(r.s.devices(chain)[1].name, "Reverb");

    let project = temp.0.join("Song");
    std::fs::create_dir_all(&project).unwrap();
    r.s.path = Some(project.join("Song.rlive"));
    let mut d = Device::builtin("rlive.reverb", "Reverb");
    d.params.insert(size, 0.77);
    Preset::of_device(&d, "Reverb")
        .save(&project.join("Defaults/Audio Effects/Reverb.rlpreset"))
        .unwrap();
    r.s.add_builtin(chain, "rlive.reverb");
    near(r.s.devices(chain)[2].params[&size], 0.77);
}

#[test]
fn save_as_default_writes_under_the_devices_own_name() {
    let temp = Temp::new("default");
    let mut r = rig();
    let lib = library(&temp);
    r.s.library = Some(lib.clone());
    let chain = ChainRef::Track(r.b);
    let id = first_device(&r.s, chain).id;
    r.s.rename_device(chain, id, "Whatever I Called It");
    let (path, exists) = r.s.default_preset_status(chain, id).unwrap();
    assert!(!exists);
    assert_eq!(
        path,
        lib.device_default_path(Role::AudioEffect, "Compressor")
    );
    r.s.save_default_preset(chain, id).unwrap();
    assert!(
        r.s.default_preset_status(chain, id).unwrap().1,
        "the overwrite question"
    );
    r.s.add_builtin(chain, "rlive.compressor");
    assert_eq!(r.s.devices(chain)[1].name, "Compressor");
}

#[test]
fn a_new_track_starts_from_its_default() {
    let temp = Temp::new("track");
    let mut r = rig();
    let lib = library(&temp);
    r.s.library = Some(lib);
    let id = r.b;
    r.s.project.track_mut(id).unwrap().volume_db = -9.0;
    r.s.add_builtin(ChainRef::Track(id), "rlive.utility");
    r.s.commit_edits(false);
    r.s.save_track_default(id).unwrap();
    let new = r.s.add_track(TrackKind::Audio, None);
    let t = r.s.project.track(new).unwrap().clone();
    assert!((t.volume_db + 9.0).abs() < 1e-3);
    assert_eq!(t.devices.len(), 2, "with the default's devices");
    assert_ne!(
        t.devices[0].id,
        r.s.project.track(id).unwrap().devices[0].id
    );
    assert!(r.s.runtimes.contains_key(&t.devices[1].id));
    let midi = r.s.add_track(TrackKind::Midi, None);
    assert_eq!(
        r.s.project.track(midi).unwrap().devices.len(),
        0,
        "MIDI has no default"
    );
}

#[test]
fn a_dropped_sample_loads_into_its_default_on_a_midi_track() {
    let temp = Temp::new("dropdefault");
    let mut r = rig();
    let lib = library(&temp);
    r.s.library = Some(lib.clone());
    let sample = temp.0.join("hit.wav");
    write_wav(&sample);
    let chain = ChainRef::Track(r.a);
    r.s.add_sampled_device(chain, sample.clone());
    assert!(
        matches!(&r.s.devices(chain)[0].kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.simpler")
    );

    let mut sampler = Device::builtin("rlive.sampler", "My Default");
    let root = param_id("rlive.sampler", "Root Key");
    sampler.params.insert(root, 72.0);
    Preset::of_device(&sampler, "My Default")
        .save(
            &lib.dropping_dir(DropTarget::OnDeviceView)
                .join("My Default.rlpreset"),
        )
        .unwrap();
    r.s.add_sampled_device(chain, sample);
    let d = r.s.devices(chain)[1].clone();
    assert!(matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.sampler"));
    near(d.params[&root], 72.0);
}

fn write_wav(p: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(p, spec).unwrap();
    for _ in 0..512 {
        w.write_sample(0i16).unwrap();
    }
    w.finalize().unwrap();
}

#[test]
fn a_new_pad_loads_into_the_default_pad() {
    let temp = Temp::new("pad");
    let mut r = rig();
    let lib = library(&temp);
    r.s.library = Some(lib.clone());
    let sample = temp.0.join("hit.wav");
    write_wav(&sample);
    let chain = ChainRef::Track(r.a);
    let (rack, note) = drum_rack(&mut r.s, chain);
    r.s.drop_on_pad(chain, rack, note, PadDrop::Sample(sample.clone()));
    let id = r.s.chains_on_pad(chain, rack, note)[0];
    let at = chain.child(rack, id).unwrap();
    let pad_id = r.s.devices(at)[0].id;
    let gain = param_id("rlive.simpler", "Gain");
    r.s.set_param(at, pad_id, gain, -6.0);
    r.s.commit_edits(false);
    r.s.save_default_pad(chain, rack, note).unwrap();

    r.s.drop_on_pad(chain, rack, 38, PadDrop::Sample(sample));
    let other = r.s.chains_on_pad(chain, rack, 38)[0];
    let at = chain.child(rack, other).unwrap();
    near(r.s.devices(at)[0].params[&gain], -6.0);
}

#[test]
fn a_plugin_default_configuration_keeps_only_the_list() {
    let temp = Temp::new("plugin");
    let lib = library(&temp);
    let config = rlive_browser::preset::PluginConfig {
        format: rlive_core::PluginFormat::Clap,
        plugin_id: "com.example.synth".into(),
        params: vec![3, 7],
        xy: [Some(7), None],
    };
    let path = lib.plugin_config_path(config.format, &config.plugin_id);
    Preset::of_plugin_config(config.clone(), "Default")
        .save(&path)
        .unwrap();
    let Payload::PluginConfig(back) = Preset::load(&path).unwrap().payload else {
        panic!()
    };
    assert_eq!(back, config);
}

// ------------------------------------------------------------- the Undo History

#[test]
fn the_undo_history_jumps_to_any_step_and_back() {
    let mut r = rig();
    let chain = ChainRef::Track(r.b);
    let id = first_device(&r.s, chain).id;
    let t = param_id("rlive.compressor", "Threshold");
    for v in [-10.0, -20.0, -30.0] {
        r.s.set_param(chain, id, t, v);
        r.s.commit_edits(false);
    }
    assert_eq!(r.s.undo_steps().len(), 4);
    assert_eq!(r.s.undo_steps()[0], "New Set", "it begins with the Set");
    assert_eq!(r.s.undo_position(), 3);

    assert!(r.s.jump_to_step(1));
    near(first_device(&r.s, chain).params[&t], -10.0);
    assert_eq!(r.s.undo_position(), 1);
    assert_eq!(
        r.s.undo_steps().len(),
        4,
        "what's above stays, to go back to"
    );

    assert!(r.s.jump_to_step(3));
    near(first_device(&r.s, chain).params[&t], -30.0);

    assert!(r.s.jump_to_step(0));
    assert_eq!(first_device(&r.s, chain).params.get(&t), None);
    assert!(!r.s.jump_to_step(0), "already there");

    assert!(r.s.jump_to_step(2));
    r.s.set_param(chain, id, t, -50.0);
    r.s.commit_edits(false);
    assert_eq!(
        r.s.undo_steps().len(),
        4,
        "a new edit drops what was undone"
    );
    assert_eq!(r.s.undo_position(), 3);
}

// ------------------------------------------------------------- backups

#[test]
fn saving_a_set_keeps_its_previous_version_in_backup() {
    let temp = Temp::new("backup");
    let mut r = rig();
    r.s.backups = true;
    let path = temp.0.join("Song.rlive");
    r.s.save(&path).unwrap();
    assert!(
        !temp.0.join("Backup").exists(),
        "nothing to back up the first time"
    );
    r.s.set_tempo(99.0);
    r.s.save(&path).unwrap();
    let kept = rlive_browser::backup::list(&temp.0.join("Backup"), "Song");
    assert_eq!(kept.len(), 1);
}
