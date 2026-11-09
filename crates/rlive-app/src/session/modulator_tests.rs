//! Modulator mappings through the session: reaching a parameter, composing with automation,
//! Remote Control, one block of latency across tracks, and persistence, reordering, deletion
//! and undo.

use super::*;
use rlive_core::param::{mixer, EnvTarget, ParamOwner, ParamRef};
use rlive_core::{ParamId, ParamKind};
use rlive_devices::devices::b2::common::{map_param, MAP_MAX, MAP_MIN, MAP_MODE, MAP_POLARITY};
use rlive_devices::devices::b2::lfo;
use rlive_engine::{create_engine, Engine};
use rlive_project::{Breakpoint, Envelope, Track};
use rlive_testkit::assert_no_alloc;

const BLOCK: usize = 512;

struct Bed {
    engine: Engine,
    s: Session,
    source: TrackId,
    target: TrackId,
    lfo: DeviceId,
    util: DeviceId,
    param: u32,
}

fn track(p: &Project, name: &str) -> Track {
    Track::new(name, TrackKind::Midi, [1, 2, 3], p.scenes.len())
}

fn bed() -> Bed {
    let mut p = Project::empty();
    let (a, b) = (track(&p, "a"), track(&p, "b"));
    p.tracks = vec![a, b];
    let (source, target) = (p.tracks[0].id, p.tracks[1].id);
    let (engine, handle) = create_engine(48_000.0, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    s.add_builtin(ChainRef::Track(source), "rlive.lfo");
    s.add_builtin(ChainRef::Track(target), "rlive.utility");
    let lfo = s.devices(ChainRef::Track(source))[0].id;
    let util = s.devices(ChainRef::Track(target))[0].id;
    let param = s.runtimes[&util]
        .params
        .iter()
        .find(|p| matches!(p.kind, ParamKind::Continuous { skew } if skew == 1.0))
        .unwrap()
        .id;
    let mut b = Bed {
        engine,
        s,
        source,
        target,
        lfo,
        util,
        param,
    };
    // A still LFO: Depth 0, so the output only follows Offset.
    let chain = ChainRef::Track(source);
    b.s.set_param(chain, lfo, lfo::DEPTH, 0.0);
    b.s.set_param(chain, lfo, lfo::OFFSET, 0.0);
    b.block();
    b.s.commit_edits(false);
    b
}

impl Bed {
    fn block(&mut self) {
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        self.engine.process_stereo(&mut l, &mut r);
        self.s.idle();
        self.s.engine.collect_garbage();
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.block();
        }
    }

    fn target_ref(&self) -> ParamRef {
        ParamRef {
            owner: ParamOwner::Device(self.util),
            id: ParamId(self.param),
        }
    }

    fn chain_of_lfo(&self) -> ChainRef {
        ChainRef::Track(self.source)
    }

    fn chain_of_target(&self) -> ChainRef {
        ChainRef::Track(self.target)
    }

    fn norm(&self) -> f32 {
        self.s
            .effective_norm(self.chain_of_target(), self.util, self.param)
            .unwrap()
    }

    fn base_norm(&self) -> f32 {
        let chain = self.chain_of_target();
        let info = self.s.runtimes[&self.util]
            .params
            .iter()
            .find(|p| p.id == self.param)
            .unwrap();
        info.to_normalized(self.s.param_value(chain, self.util, info)) as f32
    }

    fn set_offset(&mut self, percent: f64) {
        let chain = self.chain_of_lfo();
        self.s.set_param(chain, self.lfo, lfo::OFFSET, percent);
    }

    fn map(&mut self) {
        let target = self.target_ref();
        assert!(self.s.map_modulator(self.lfo, 0, target), "maps");
        self.settle();
    }
}

fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 2e-3, "{a} vs {b}");
}

#[test]
fn a_modulation_moves_its_target_around_the_knob() {
    let mut b = bed();
    b.map();
    let base = b.base_norm();
    close(b.norm(), base);
    b.set_offset(20.0);
    b.settle();
    close(b.norm(), (base + 0.2).clamp(0.0, 1.0));
}

#[test]
fn it_reaches_the_mixer_as_well() {
    let mut b = bed();
    let volume = ParamRef {
        owner: ParamOwner::Track(b.target),
        id: mixer::VOLUME,
    };
    let chain = b.chain_of_target();
    let cell = b.s.mixer_cell(chain, mixer::VOLUME).unwrap();
    assert!(b.s.map_modulator(b.lfo, 0, volume));
    b.settle();
    b.set_offset(-20.0);
    b.settle();
    let base = rlive_core::ParamInfo::continuous(
        0,
        "",
        mixer::VOLUME_MIN_DB,
        mixer::VOLUME_MAX_DB,
        0.0,
        "",
    )
    .to_normalized(0.0) as f32;
    let layout = b.s.layouts.get(&chain).unwrap();
    close(layout.effective(cell).unwrap(), base - 0.2);
}

#[test]
fn a_cross_track_modulator_is_one_block_late_and_makes_no_graph_edge() {
    let mut b = bed();
    b.map();
    let base = b.base_norm();
    close(b.norm(), base);
    let plan = compile_plan(&b.s.project);
    assert!(plan.midi.is_empty() && plan.sidechains.is_empty());
    b.set_offset(50.0);
    // The block that hears the change: the LFO works it out, the target hasn't heard yet.
    b.block();
    close(b.norm(), base);
    // The next block ramps to it.
    b.block();
    close(b.norm(), (base + 0.5).clamp(0.0, 1.0));
}

#[test]
fn modulation_adds_to_what_automation_is_doing() {
    let mut b = bed();
    b.map();
    let info = b.s.runtimes[&b.util]
        .params
        .iter()
        .find(|p| p.id == b.param)
        .unwrap()
        .clone();
    let automated = info.from_normalized(0.25);
    let id = b.target;
    let target = b.target_ref();
    b.s.project
        .track_mut(id)
        .unwrap()
        .automation
        .push(Envelope {
            target: EnvTarget::Param(target),
            points: vec![Breakpoint {
                time: 0.0,
                value: automated,
                curve: 0.0,
            }],
        });
    b.s.send_routes(ChainRef::Track(id).into());
    b.set_offset(30.0);
    b.settle();
    close(b.norm(), 0.25 + 0.3);
}

#[test]
fn the_lfo_output_is_ramped_across_the_block() {
    let mut b = bed();
    b.map();
    let base = b.base_norm();
    b.set_offset(100.0);
    b.block();
    b.block();
    // The published value lands on the last control step, never a whole block early.
    close(b.norm(), 1.0f32.min(base + 1.0));
}

#[test]
fn remote_control_owns_the_parameter_between_min_and_max() {
    let mut b = bed();
    b.map();
    let chain = b.chain_of_lfo();
    b.s.set_param(chain, b.lfo, map_param(0, MAP_MODE), 1.0);
    b.s.set_param(chain, b.lfo, map_param(0, MAP_MIN), 20.0);
    b.s.set_param(chain, b.lfo, map_param(0, MAP_MAX), 40.0);
    b.set_offset(0.0);
    b.settle();
    close(b.norm(), 0.3);
    assert!(b.s.is_remote_controlled(b.target_ref()), "the UI is locked");

    // The knob won't move while a Remote Control has it.
    let before = b.s.param_value(b.chain_of_target(), b.util, &param(&b));
    b.s.set_param(b.chain_of_target(), b.util, b.param, before + 1.0);
    b.settle();
    assert_eq!(
        b.s.param_value(b.chain_of_target(), b.util, &param(&b)),
        before
    );

    // Back to Modulation, it is the knob's again.
    b.s.set_param(chain, b.lfo, map_param(0, MAP_MODE), 0.0);
    b.settle();
    assert!(!b.s.is_remote_controlled(b.target_ref()));
    b.s.set_param(chain, b.lfo, map_param(0, MAP_POLARITY), 1.0);
    b.settle();
    let chain_t = b.chain_of_target();
    let cell = b.s.device_cell(chain_t, b.util, b.param).unwrap();
    assert!(b.s.cell_state(chain_t, cell).modulated(), "modulated again");
}

fn param(b: &Bed) -> rlive_core::ParamInfo {
    b.s.runtimes[&b.util]
        .params
        .iter()
        .find(|p| p.id == b.param)
        .unwrap()
        .clone()
}

#[test]
fn a_parameter_is_held_by_one_mapping_and_a_macro_keeps_its_own() {
    let mut b = bed();
    b.map();
    let chain = b.chain_of_lfo();
    b.s.add_builtin(chain, "rlive.lfo");
    let second = b.s.devices(chain)[1].id;
    b.settle();
    let target = b.target_ref();
    assert!(b.s.map_modulator(second, 0, target), "taken over");
    assert_eq!(b.s.modulator_target(b.lfo, 0), None);
    assert_eq!(b.s.modulator_target(second, 0), Some(target));
    assert!(!b.s.can_modulate(ParamRef {
        owner: ParamOwner::Device(b.util),
        id: ParamId(99_999),
    }));
}

#[test]
fn eight_targets_is_as_many_as_there_are() {
    let mut b = bed();
    let chain = b.chain_of_lfo();
    assert_eq!(b.s.modulator_outputs(b.lfo), 8);
    let target = b.target_ref();
    assert!(!b.s.map_modulator(b.lfo, 8, target), "only slots 0 to 7");
    b.s.unmap_modulator(b.lfo, 3);
    assert_eq!(b.s.devices(chain)[0].modulations.len(), 0);
}

#[test]
fn a_mapping_survives_save_and_load() {
    let mut b = bed();
    b.map();
    let text = b.s.project.to_ron().unwrap();
    let loaded = Project::from_ron(&text).unwrap();
    assert_eq!(loaded.tracks[0].devices[0].modulations.len(), 1);
    let (engine, handle) = create_engine(48_000.0, BLOCK);
    let mut s = Session::new(handle, loaded, PluginCache::default());
    let mut b2 = Bed {
        engine,
        source: b.source,
        target: b.target,
        lfo: b.lfo,
        util: b.util,
        param: b.param,
        s: std::mem::replace(&mut s, s2()),
    };
    b2.settle();
    b2.set_offset(25.0);
    b2.settle();
    close(b2.norm(), (b2.base_norm() + 0.25).clamp(0.0, 1.0));
}

fn s2() -> Session {
    let (_engine, handle) = create_engine(48_000.0, BLOCK);
    Session::new(handle, Project::empty(), PluginCache::default())
}

#[test]
fn a_mapping_survives_the_target_moving_along_its_chain() {
    let mut b = bed();
    let chain = b.chain_of_target();
    b.s.add_builtin(chain, "rlive.delay");
    b.s.move_device(chain, b.util, 1);
    b.settle();
    b.map();
    b.s.move_device(chain, b.util, 0);
    b.settle();
    b.set_offset(25.0);
    b.settle();
    close(b.norm(), (b.base_norm() + 0.25).clamp(0.0, 1.0));
}

#[test]
fn deleting_the_target_drops_the_mapping_cleanly() {
    let mut b = bed();
    b.map();
    let chain = b.chain_of_target();
    b.s.remove_device(chain, b.util);
    b.settle();
    assert!(b.s.devices(b.chain_of_lfo())[0].modulations.is_empty());
    let routes = b.s.compiled_routes(chain.into()).unwrap();
    assert!(routes.mods().is_empty());
}

#[test]
fn deleting_the_source_hands_the_target_back_to_its_knob() {
    let mut b = bed();
    b.map();
    b.set_offset(30.0);
    b.settle();
    let chain = b.chain_of_lfo();
    b.s.remove_device(chain, b.lfo);
    b.settle();
    close(b.norm(), b.base_norm());
}

#[test]
fn switching_the_source_off_hands_the_target_back() {
    let mut b = bed();
    b.map();
    b.set_offset(30.0);
    b.settle();
    let chain = b.chain_of_lfo();
    b.s.set_bypass(chain, b.lfo, true);
    b.settle();
    close(b.norm(), b.base_norm());
    b.s.set_bypass(chain, b.lfo, false);
    b.settle();
    close(b.norm(), (b.base_norm() + 0.3).clamp(0.0, 1.0));
}

#[test]
fn undo_takes_a_mapping_back_and_redo_restores_it() {
    let mut b = bed();
    b.set_offset(40.0);
    b.settle();
    b.s.commit_edits(false);
    b.map();
    b.s.commit_edits(false);
    close(b.norm(), (b.base_norm() + 0.4).clamp(0.0, 1.0));
    assert!(b.s.undo());
    b.settle();
    let lfo = b.lfo;
    assert_eq!(b.s.modulator_target(lfo, 0), None);
    close(b.norm(), b.base_norm());
    assert!(b.s.redo());
    b.settle();
    assert_eq!(b.s.modulator_target(lfo, 0), Some(b.target_ref()));
}

#[test]
fn undoing_a_deletion_brings_the_mapping_back() {
    let mut b = bed();
    b.map();
    b.s.commit_edits(false);
    let chain = b.chain_of_target();
    b.s.remove_device(chain, b.util);
    b.settle();
    b.s.commit_edits(false);
    assert!(b.s.undo());
    b.settle();
    assert_eq!(b.s.modulator_target(b.lfo, 0), Some(b.target_ref()));
    b.set_offset(25.0);
    b.settle();
    close(b.norm(), (b.base_norm() + 0.25).clamp(0.0, 1.0));
}

#[test]
fn modulating_allocates_nothing_on_the_audio_thread() {
    let mut b = bed();
    b.map();
    b.set_offset(10.0);
    b.settle();
    assert_no_alloc(|| {
        for _ in 0..4 {
            let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
            b.engine.process_stereo(&mut l, &mut r);
        }
    });
}

#[test]
fn expression_control_maps_through_the_same_path() {
    let mut p = Project::empty();
    p.tracks = vec![track(&p, "a")];
    let id = p.tracks[0].id;
    let (engine, handle) = create_engine(48_000.0, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    let chain = ChainRef::Track(id);
    s.add_builtin(chain, "rlive.expression_control");
    s.add_builtin(chain, "rlive.utility");
    let ec = s.devices(chain)[0].id;
    let util = s.devices(chain)[1].id;
    let param = s.runtimes[&util]
        .params
        .iter()
        .find(|p| matches!(p.kind, ParamKind::Continuous { skew } if skew == 1.0))
        .unwrap()
        .id;
    assert_eq!(s.modulator_outputs(ec), 5);
    let target = ParamRef {
        owner: ParamOwner::Device(util),
        id: ParamId(param),
    };
    assert!(s.map_modulator(ec, 0, target));
    let mut bed = Bed {
        engine,
        s,
        source: id,
        target: id,
        lfo: ec,
        util,
        param,
    };
    bed.settle();
    assert!(bed
        .s
        .compiled_routes(ChainRef::Track(id).into())
        .is_some_and(|r| r.mods().len() == 1));
}
