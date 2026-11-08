//! Racks through the session: macros and their mappings, variations, Rand, grouping, chains and
//! the Drum Rack, undo for all of it, and saved Sets.

use super::*;
use rlive_core::param::rack as rack_params;
use rlive_core::param::{chain as controls, EnvTarget, ParamOwner, ParamRef};
use rlive_core::{ChainId, MidiMessage, ParamId, PPQ};
use rlive_engine::{create_engine, CellTarget, Engine, Tap};
use rlive_project::{Breakpoint, Chain, Clip, DrumPad, Macro, NoteData, RackKind, Zone, Zones};
use rlive_testkit::assert_no_alloc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.8,
        note_id: 0,
        tuning: 0.0,
    }
}

fn render(engine: &mut Engine, s: &mut Session, frames: usize) -> Vec<f32> {
    let frames = frames.next_multiple_of(BLOCK);
    let (left, _) = rlive_testkit::render_chunks(frames, BLOCK, |l, r| {
        engine.process_stereo(l, r);
        s.idle();
    });
    left
}

fn commit(s: &mut Session) {
    s.commit_edits(false);
}

/// Parameter values come back from the engine through single precision.
fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-3, "{a} vs {b}");
}

/// A MIDI track with a rack on it, the rack's chain holding a Utility, ready to play.
struct Rig {
    engine: Engine,
    s: Session,
    track: TrackId,
    root: ChainRef,
    rack: DeviceId,
    chain: ChainId,
    device: DeviceId,
}

fn rig(kind: RackKind) -> Rig {
    let mut p = Project::new_default();
    let track = p.tracks[0].id;
    let mut rack = Device::rack(kind, "Rack");
    let rack_id = rack.id;
    let mut chain = Chain::new("A");
    let chain_id = chain.id;
    let inner = Device::builtin("rlive.utility", "Utility");
    let device = inner.id;
    chain.devices.push(inner);
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(chain);
    }
    p.tracks[0].devices.push(rack);
    let (engine, handle) = create_engine(SR, BLOCK);
    let s = Session::new(handle, p, PluginCache::default());
    Rig {
        engine,
        s,
        track,
        root: ChainRef::Track(track),
        rack: rack_id,
        chain: chain_id,
        device,
    }
}

fn param(device: DeviceId, id: u32) -> ParamRef {
    ParamRef {
        owner: ParamOwner::Device(device),
        id: ParamId(id),
    }
}

impl Rig {
    fn nested(&self) -> ChainRef {
        self.root.child(self.rack, self.chain).unwrap()
    }

    /// The value the parameter is sounding at, in plain units.
    fn gain_now(&mut self) -> f64 {
        render(&mut self.engine, &mut self.s, 4096);
        let norm = self
            .s
            .effective_norm(self.nested(), self.device, 0)
            .unwrap() as f64;
        self.s.runtimes[&self.device].params[0].from_normalized(norm)
    }

    fn map(&mut self, n: usize) -> bool {
        self.s
            .map_macro(self.root, self.rack, n, param(self.device, 0))
    }

    fn set_macro(&mut self, n: usize, value: f64) {
        self.s.set_macro(self.root, self.rack, n, value);
    }

    fn rack_dev(&self) -> &Device {
        self.s
            .devices(self.root)
            .iter()
            .find(|d| d.id == self.rack)
            .unwrap()
    }

    fn macros(&self) -> Vec<Macro> {
        match &self.rack_dev().kind {
            DeviceKind::Rack { macros, .. } => macros.clone(),
            _ => panic!(),
        }
    }

    fn variations(&self) -> Vec<rlive_project::Variation> {
        match &self.rack_dev().kind {
            DeviceKind::Rack { variations, .. } => variations.clone(),
            _ => panic!(),
        }
    }

    fn chain_ids(&self) -> Vec<ChainId> {
        self.rack_dev().chains().map(|c| c.id).collect()
    }
}

/// Runs an edit, then proves undo and redo bring the whole project back exactly.
fn round_trip(rig: &mut Rig, edit: impl FnOnce(&mut Rig)) {
    let before = rig.s.project.clone();
    edit(rig);
    commit(&mut rig.s);
    let after = rig.s.project.clone();
    assert_ne!(before, after, "the edit changed something");
    render(&mut rig.engine, &mut rig.s, BLOCK);
    assert!(rig.s.undo(), "undo");
    assert_eq!(rig.s.project, before, "undo brings it all back");
    render(&mut rig.engine, &mut rig.s, BLOCK);
    assert!(rig.s.redo(), "redo");
    assert_eq!(rig.s.project, after, "redo goes back to the edit");
    render(&mut rig.engine, &mut rig.s, BLOCK);
}

// ------------------------------------------------------------------ macros

#[test]
fn a_new_rack_has_eight_visible_macros_and_keeps_the_count() {
    let mut r = rig(RackKind::Instrument);
    assert_eq!(r.s.visible_macros(r.root, r.rack), 8);
    r.s.set_visible_macros(r.root, r.rack, 3);
    assert_eq!(r.s.visible_macros(r.root, r.rack), 3);
    r.s.set_visible_macros(r.root, r.rack, 99);
    assert_eq!(r.s.visible_macros(r.root, r.rack), 16, "at most sixteen");
    // A hidden macro keeps working.
    r.s.set_visible_macros(r.root, r.rack, 1);
    assert!(r.map(5));
    r.set_macro(5, 127.0);
    let top = r.s.runtimes[&r.device].params[0].max;
    near(r.gain_now(), top);
}

#[test]
fn a_first_mapping_keeps_the_parameter_where_it_is_and_sweeps_its_range() {
    let mut r = rig(RackKind::Instrument);
    let nested = r.nested();
    r.s.set_param(nested, r.device, 0, 12.0);
    assert!(r.map(0));
    let info = r.s.runtimes[&r.device].params[0].clone();
    let expect = info.to_normalized(12.0) * 127.0;
    assert_eq!(
        r.s.macro_value(r.root, r.rack, 0),
        expect,
        "the macro took its place"
    );
    near(r.gain_now(), 12.0);
    r.set_macro(0, 0.0);
    near(r.gain_now(), info.min);
    r.set_macro(0, 127.0);
    near(r.gain_now(), info.max);
}

#[test]
fn min_above_max_inverts_a_mapping_and_invert_range_swaps_them() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    r.s.set_macro_range(r.root, r.rack, 0, 0, -6.0, 6.0);
    r.set_macro(0, 127.0);
    near(r.gain_now(), 6.0);
    r.s.invert_macro_range(r.root, r.rack, 0, 0);
    assert_eq!(r.macros()[0].mappings[0].min, 6.0);
    r.set_macro(0, 127.0);
    near(r.gain_now(), -6.0);
    r.set_macro(0, 0.0);
    near(r.gain_now(), 6.0);
}

#[test]
fn a_mapped_parameter_is_locked_against_the_knob() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    r.set_macro(0, 127.0);
    let nested = r.nested();
    let before = r.s.devices(nested)[0].params.clone();
    r.s.set_param(nested, r.device, 0, -40.0);
    assert_eq!(
        r.s.devices(nested)[0].params,
        before,
        "the project didn't change"
    );
    near(r.gain_now(), r.s.runtimes[&r.device].params[0].max);
}

#[test]
fn a_mapped_device_switch_is_locked_too() {
    let mut r = rig(RackKind::Instrument);
    let target = param(r.device, rlive_core::ACTIVATOR.0);
    assert!(r.s.map_macro(r.root, r.rack, 0, target));
    let nested = r.nested();
    r.s.set_bypass(nested, r.device, true);
    assert!(!r.s.devices(nested)[0].bypass);
}

#[test]
fn a_parameter_already_mapped_refuses_a_second_macro() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    assert!(!r.map(1), "one owner at a time");
    assert!(r.macros()[1..].iter().all(|m| m.mappings.is_empty()));
}

#[test]
fn a_macro_can_only_map_what_is_inside_its_rack() {
    let mut r = rig(RackKind::Instrument);
    let poly = r.s.devices(r.root)[0].id;
    let mapped = r.s.map_macro(r.root, r.rack, 0, param(poly, 0));
    assert!(!mapped, "the Poly is beside the rack, not in it");
}

#[test]
fn a_macro_composes_with_automation_of_its_own_knob() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    let macro_ref = param(r.rack, 0);
    let chain = r.root;
    r.s.set_envelope(
        chain,
        EnvTarget::Param(macro_ref),
        vec![Breakpoint::new(0.0, 63.5)],
    );
    r.s.set_macro(chain, r.rack, 0, 0.0);
    let got = r.gain_now();
    let info = r.s.runtimes[&r.device].params[0].clone();
    let want = info.from_normalized(63.5 / 127.0);
    near(got, want);
}

#[test]
fn a_chain_volume_can_be_mapped_and_is_locked() {
    let mut r = rig(RackKind::Instrument);
    let target = ParamRef {
        owner: ParamOwner::Chain(r.chain),
        id: controls::VOLUME,
    };
    assert!(r.s.map_macro(r.root, r.rack, 0, target));
    r.set_macro(0, 0.0);
    render(&mut r.engine, &mut r.s, 1024);
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    r.s.set_chain_param(root, rack, chain, controls::VOLUME, -6.0);
    assert_eq!(
        r.s.chain_param(root, rack, chain, controls::VOLUME),
        Some(0.0)
    );
}

#[test]
fn deleting_a_mapping_leaves_the_parameter_where_the_macro_had_it() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    r.s.set_macro_range(r.root, r.rack, 0, 0, -12.0, 12.0);
    r.set_macro(0, 127.0);
    near(r.gain_now(), 12.0);
    r.s.unmap_macro(r.root, r.rack, 0, 0);
    assert!(r.macros()[0].mappings.is_empty());
    near(r.s.devices(r.nested())[0].params[&0], 12.0);
    near(r.gain_now(), 12.0);
    r.s.set_param(r.nested(), r.device, 0, 3.0);
    near(r.gain_now(), 3.0);
}

#[test]
fn macros_name_and_scale_themselves_from_their_mappings() {
    let mut r = rig(RackKind::Instrument);
    let plain = r.s.macro_display(r.root, r.rack, 2);
    assert_eq!(plain.name, "Macro 3");
    assert_eq!((plain.min, plain.max), (0.0, 127.0));
    assert_eq!(plain.unit, "");

    assert!(r.map(2));
    r.s.set_macro_range(r.root, r.rack, 2, 0, -24.0, 12.0);
    let one = r.s.macro_display(r.root, r.rack, 2);
    assert_eq!(one.name, "Gain", "named after its one mapping");
    assert_eq!(one.unit, "dB");
    assert_eq!((one.min, one.max), (-24.0, 12.0));

    r.set_macro(2, 127.0);
    near(r.s.macro_display(r.root, r.rack, 2).value, 12.0);

    // A second mapping with the same unit and range keeps the unit; the name goes generic.
    let target_two = param(r.device, 1);
    assert!(r.s.map_macro(r.root, r.rack, 2, target_two));
    let two = r.s.macro_display(r.root, r.rack, 2);
    assert_eq!(two.name, "Macro 3", "several mappings: the generic name");
    assert_eq!(
        (two.unit.as_str(), two.min, two.max),
        ("", 0.0, 127.0),
        "mixed units: 0 to 127"
    );

    // A custom name always wins.
    r.s.set_macro_name(r.root, r.rack, 2, "Crush");
    assert_eq!(r.s.macro_display(r.root, r.rack, 2).name, "Crush");
    r.s.set_macro_name(r.root, r.rack, 2, "");
    assert_eq!(r.s.macro_display(r.root, r.rack, 2).name, "Macro 3");
}

// ------------------------------------------------------------------ variations and Rand

#[test]
fn variations_store_launch_overwrite_rename_duplicate_and_delete() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    r.set_macro(0, 10.0);
    r.set_macro(1, 20.0);
    let first = r.s.store_variation(r.root, r.rack).unwrap();
    assert_eq!(first, 0);
    assert_eq!(r.variations()[0].name, "Variation 1");
    assert_eq!(r.variations()[0].values, [10.0, 20.0]);

    r.set_macro(0, 90.0);
    r.set_macro(1, 80.0);
    let second = r.s.store_variation(r.root, r.rack).unwrap();
    assert_eq!(r.variations()[second].name, "Variation 2");

    r.s.launch_variation(r.root, r.rack, 0);
    assert_eq!(r.s.macro_value(r.root, r.rack, 0), 10.0);
    assert_eq!(r.s.macro_value(r.root, r.rack, 1), 20.0);
    let info = r.s.runtimes[&r.device].params[0].clone();
    let want = info.from_normalized(10.0 / 127.0);
    near(r.gain_now(), want);

    r.set_macro(1, 30.0);
    r.s.overwrite_variation(r.root, r.rack, 0);
    assert_eq!(r.variations()[0].values[1], 30.0);

    r.s.rename_variation(r.root, r.rack, 1, "Drop");
    r.s.duplicate_variation(r.root, r.rack, 1);
    assert_eq!(r.variations().len(), 3);
    assert_eq!(r.variations()[2].name, "Drop");
    r.s.delete_variation(r.root, r.rack, 0);
    assert_eq!(r.variations()[0].name, "Drop");
    assert_eq!(r.variations().len(), 2);
}

#[test]
fn launching_skips_macros_excluded_from_variations() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    r.set_macro(0, 10.0);
    r.set_macro(1, 20.0);
    r.s.store_variation(r.root, r.rack).unwrap();
    r.s.set_macro_exclude_variations(r.root, r.rack, 1, true);
    r.set_macro(0, 90.0);
    r.set_macro(1, 80.0);
    r.s.launch_variation(r.root, r.rack, 0);
    assert_eq!(r.s.macro_value(r.root, r.rack, 0), 10.0);
    assert_eq!(
        r.s.macro_value(r.root, r.rack, 1),
        80.0,
        "excluded: left alone"
    );
    r.s.set_macro_exclude_variations(r.root, r.rack, 1, false);
    r.s.launch_variation(r.root, r.rack, 0);
    assert_eq!(
        r.s.macro_value(r.root, r.rack, 1),
        20.0,
        "back in once it's on again"
    );
}

#[test]
fn rand_moves_mapped_macros_but_not_excluded_or_unmapped_ones() {
    let mut r = rig(RackKind::Audio);
    let target = param(r.device, 0);
    assert!(r.s.map_macro(r.root, r.rack, 0, target));
    let other = param(r.device, 1);
    assert!(r.s.map_macro(r.root, r.rack, 2, other));
    r.set_macro(0, 10.0);
    r.set_macro(1, 11.0);
    r.set_macro(2, 12.0);
    r.s.set_macro_exclude_random(r.root, r.rack, 2, true);
    r.s.randomize_macros_with(r.root, r.rack, &mut || 0.5);
    assert_eq!(r.s.macro_value(r.root, r.rack, 0), 63.5);
    assert_eq!(
        r.s.macro_value(r.root, r.rack, 1),
        11.0,
        "nothing mapped to it"
    );
    assert_eq!(r.s.macro_value(r.root, r.rack, 2), 12.0, "excluded");
}

#[test]
fn volume_macros_are_excluded_from_rand_in_instrument_racks_only() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0), "the Utility's Gain counts as a volume in dB");
    assert!(r.macros()[0].exclude_random);
    r.s.randomize_macros_with(r.root, r.rack, &mut || 0.25);
    let audio = {
        let mut r = rig(RackKind::Audio);
        assert!(r.map(0));
        assert!(!r.macros()[0].exclude_random, "only Instrument Racks");
        r.s.randomize_macros_with(r.root, r.rack, &mut || 0.25);
        r.s.macro_value(r.root, r.rack, 0)
    };
    assert_eq!(audio, 0.25 * 127.0);
    let m0 = r.s.macro_value(r.root, r.rack, 0);
    assert_ne!(m0, 0.25 * 127.0, "the mapped volume stayed out of it");
}

// ------------------------------------------------------------------ chains, zones, the mixer

#[test]
fn chain_controls_reach_the_engine_as_parameters() {
    let mut r = rig(RackKind::Instrument);
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    let cell = r.s.chain_cell(root, chain, controls::VOLUME).unwrap();
    r.s.set_chain_param(root, rack, chain, controls::VOLUME, -6.0);
    assert_eq!(
        r.s.chain_param(root, rack, chain, controls::VOLUME),
        Some(-6.0)
    );
    assert_eq!(r.s.layouts[&root].base(cell), Some(-6.0));
    render(&mut r.engine, &mut r.s, 1024);
    assert_eq!(
        r.s.layouts[&root].spec(cell).unwrap().target,
        CellTarget::Chain {
            chain,
            control: controls::VOLUME
        }
    );
}

#[test]
fn zones_are_validated_and_sent() {
    let mut r = rig(RackKind::Instrument);
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    let z = Zone {
        low: 200,
        high: 60,
        fade_low: 90,
        fade_high: 1,
    };
    r.s.set_zone(root, rack, chain, racks::ZoneKind::Key, z);
    let c = r.rack_dev().chains().next().unwrap().clone();
    assert_eq!(c.zones.key.low, 60);
    assert_eq!(c.zones.key.high, 127);
    assert!(c.zones.key.fade_low <= c.zones.key.low);
    assert!(c.zones.key.fade_high >= c.zones.key.high);
    render(&mut r.engine, &mut r.s, 1024);
}

#[test]
fn a_chain_select_zone_decides_what_a_rack_of_chains_plays() {
    let mut r = rig(RackKind::Instrument);
    let (root, rack) = (r.root, r.rack);
    let mut b = Chain::new("B");
    b.devices.push(Device::builtin("rlive.poly", "Poly"));
    b.zones.select = Zone::span(5, 5);
    r.s.insert_rack_chain(root, rack, 1, b);
    r.s.play_midi(r.track, on(60));
    render(&mut r.engine, &mut r.s, 4096);
    let ids = r.chain_ids();
    r.s.set_chain_selector(root, rack, 5.0);
    assert_eq!(
        r.s.devices(root)[1].params[&rack_params::CHAIN_SELECT.0],
        5.0
    );
    assert_eq!(ids.len(), 2);
    render(&mut r.engine, &mut r.s, 4096);
}

#[test]
fn one_macro_can_drive_several_parameters() {
    let mut r = rig(RackKind::Audio);
    let (root, rack) = (r.root, r.rack);
    assert!(r.map(0));
    assert!(r.s.map_macro(root, rack, 0, param(r.device, 1)));
    r.s.set_macro_range(root, rack, 0, 0, -6.0, 6.0);
    r.s.set_macro_range(root, rack, 0, 1, -1.0, 0.5);
    r.set_macro(0, 127.0);
    near(r.gain_now(), 6.0);
    let nested = r.nested();
    let second = r.s.effective_norm(nested, r.device, 1).unwrap() as f64;
    let info = r.s.runtimes[&r.device].params[1].clone();
    near(info.from_normalized(second), 0.5);
}

// ------------------------------------------------------------------ rules and grouping

#[test]
fn rack_rules_keep_each_type_to_its_own_devices() {
    let mut r = rig(RackKind::Midi);
    let midi = r.nested();
    r.s.add_builtin(midi, "rlive.utility");
    assert_eq!(
        r.s.devices(midi).len(),
        1,
        "the Utility came from the rig and an audio effect is refused"
    );
    assert!(r.s.messages.iter().any(|m| m.contains("MIDI Effect Rack")));
    r.s.add_builtin(midi, "rlive.arpeggiator");
    assert_eq!(r.s.devices(midi).len(), 2, "a MIDI effect goes in");
}

#[test]
fn an_audio_rack_refuses_instruments_and_instrument_racks_stay_on_midi_tracks() {
    let mut r = rig(RackKind::Audio);
    let audio = r.nested();
    r.s.add_builtin(audio, "rlive.poly");
    assert_eq!(r.s.devices(audio).len(), 1);

    let audio_track = r.s.add_track(TrackKind::Audio, None);
    let at = ChainRef::Track(audio_track);
    assert!(r.s.add_rack(at, RackKind::Instrument, "I").is_none());
    assert!(r.s.add_rack(at, RackKind::Midi, "M").is_none());
    assert!(r.s.add_rack(at, RackKind::Audio, "A").is_some());
}

#[test]
fn grouping_devices_keeps_them_alive_and_ungrouping_puts_them_back() {
    let mut r = rig(RackKind::Instrument);
    let root = r.root;
    let poly = r.s.devices(root)[0].id;
    render(&mut r.engine, &mut r.s, 1024);
    let new_rack =
        r.s.group_devices(root, &[poly], false)
            .expect("a rack holding the poly");
    let in_rack = r.s.devices(root);
    assert_eq!(in_rack.len(), 2, "the old rack is untouched beside it");
    assert_eq!(in_rack[0].id, new_rack);
    let DeviceKind::Rack { kind, chains, .. } = &in_rack[0].kind else {
        panic!();
    };
    assert_eq!(*kind, RackKind::Instrument);
    assert_eq!(chains[0].devices[0].id, poly);
    assert!(
        r.s.runtimes.contains_key(&poly),
        "the device kept its runtime"
    );
    render(&mut r.engine, &mut r.s, 1024);
    r.s.play_midi(r.track, on(60));
    let audio = render(&mut r.engine, &mut r.s, 4096);
    assert!(
        audio.iter().any(|s| s.abs() > 1e-4),
        "the grouped poly still plays"
    );

    assert!(r.s.ungroup_rack(root, new_rack));
    assert_eq!(r.s.devices(root)[0].id, poly);
    assert!(r.s.runtimes.contains_key(&poly));
    render(&mut r.engine, &mut r.s, 1024);
}

#[test]
fn group_picks_the_rack_type_and_group_to_drum_makes_a_pad() {
    let mut r = rig(RackKind::Instrument);
    let root = r.root;
    let poly = r.s.devices(root)[0].id;
    let drum = r.s.group_devices(root, &[poly], true).unwrap();
    let DeviceKind::Rack { kind, chains, .. } = &r.s.devices(root)[0].kind else {
        panic!();
    };
    assert_eq!(*kind, RackKind::Drum);
    assert_eq!(chains[0].zones.drum.unwrap().receive, Some(36));
    assert_eq!(chains[0].zones.drum.unwrap().play, 60);
    assert!(r.s.runtimes.contains_key(&drum));
}

#[test]
fn grouping_audio_devices_makes_an_audio_effect_rack() {
    let (mut engine, handle) = create_engine(SR, BLOCK);
    let _ = &mut engine;
    let mut s = Session::new(handle, Project::new_default(), PluginCache::default());
    let t = s.project.tracks[0].id;
    let root = ChainRef::Track(t);
    s.add_builtin(root, "rlive.utility");
    let id = s.devices(root).last().unwrap().id;
    let rack = s.group_devices(root, &[id], false).unwrap();
    let DeviceKind::Rack { kind, .. } = &s.devices(root).last().unwrap().kind else {
        panic!();
    };
    assert_eq!(*kind, RackKind::Audio);
    assert_eq!(s.devices(root).last().unwrap().id, rack);
}

#[test]
fn nesting_twice_makes_a_rack_inside_a_rack() {
    let mut r = rig(RackKind::Instrument);
    let root = r.root;
    let once = r.s.group_devices(root, &[r.rack], false).unwrap();
    let twice = r.s.group_devices(root, &[once], false).unwrap();
    let outer = r.s.devices(root)[1].clone();
    assert_eq!(outer.id, twice);
    let DeviceKind::Rack { chains, .. } = &outer.kind else {
        panic!();
    };
    let inner = &chains[0].devices[0];
    assert_eq!(inner.id, once);
    assert!(matches!(inner.kind, DeviceKind::Rack { .. }));
    render(&mut r.engine, &mut r.s, 2048);
}

#[test]
fn grouping_chains_nests_them_in_a_new_rack_of_the_same_kind() {
    let mut r = rig(RackKind::Instrument);
    let (root, rack) = (r.root, r.rack);
    let b = r.s.add_chain(root, rack, "B").unwrap();
    let (wrapper, inner) = r.s.group_chains(root, rack, &[r.chain, b]).unwrap();
    let parent = r.rack_dev().clone();
    let chains: Vec<ChainId> = parent.chains().map(|c| c.id).collect();
    assert_eq!(chains, [wrapper]);
    let nested = parent.chains().next().unwrap().devices[0].clone();
    assert_eq!(nested.id, inner);
    let ids: Vec<ChainId> = nested.chains().map(|c| c.id).collect();
    assert_eq!(ids, [r.chain, b]);
    render(&mut r.engine, &mut r.s, 2048);
}

// ------------------------------------------------------------------ the Drum Rack

fn drum_rig() -> Rig {
    let mut r = rig(RackKind::Drum);
    let c = r.chain;
    r.s.set_drum_pad(
        r.root,
        r.rack,
        c,
        DrumPad {
            receive: Some(38),
            play: 60,
            choke: Some(1),
        },
    );
    commit(&mut r.s);
    r
}

#[test]
fn drum_chains_get_the_next_free_pad_and_returns_stop_at_six() {
    let mut r = drum_rig();
    let (root, rack) = (r.root, r.rack);
    let a = r.s.add_chain(root, rack, "A").unwrap();
    let b = r.s.add_chain(root, rack, "B").unwrap();
    let pads: Vec<_> = r
        .rack_dev()
        .chains()
        .map(|c| c.zones.drum.unwrap().receive)
        .collect();
    assert_eq!(
        pads,
        [Some(38), Some(36), Some(37)],
        "the next free ones from C1"
    );
    let _ = (a, b);

    for n in 0..6 {
        assert!(r.s.add_return_chain(root, rack, &format!("R{n}")).is_some());
    }
    assert!(
        r.s.add_return_chain(root, rack, "Too many").is_none(),
        "up to six"
    );
    render(&mut r.engine, &mut r.s, 1024);
}

#[test]
fn a_return_chain_only_exists_on_a_drum_rack() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.s.add_return_chain(r.root, r.rack, "R").is_none());
}

#[test]
fn deleting_a_return_shifts_the_sends_after_it_down() {
    let mut r = drum_rig();
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    let a = r.s.add_return_chain(root, rack, "A").unwrap();
    let b = r.s.add_return_chain(root, rack, "B").unwrap();
    r.s.set_chain_param(root, rack, chain, controls::send(1), -3.0);
    assert_eq!(
        r.s.chain_param(root, rack, chain, controls::send(1)),
        Some(-3.0)
    );
    r.s.delete_chain(root, rack, a);
    assert_eq!(
        r.s.chain_param(root, rack, chain, controls::send(0)),
        Some(-3.0),
        "B moved up a slot and its send went with it"
    );
    let cell = r.s.chain_cell(root, chain, controls::send(0)).unwrap();
    assert_eq!(r.s.layouts[&root].base(cell), Some(-3.0));
    let _ = b;
    render(&mut r.engine, &mut r.s, 1024);
}

#[test]
fn routing_a_return_chain_to_a_return_track_compiles_into_the_plan() {
    let mut r = drum_rig();
    let (root, rack) = (r.root, r.rack);
    let ret = r.s.add_return_chain(root, rack, "R").unwrap();
    let target = r.s.add_return().unwrap();
    r.s.set_return_output(root, rack, ret, Some(target));
    let plan = compile_plan(&r.s.project);
    let dest = r.s.project.tracks.len() as u32;
    let step = plan.steps.iter().find(|s| s.track == dest).unwrap();
    assert!(
        step.inputs.iter().any(|i| matches!(i.tap, Tap::Aux(..))),
        "{:?}",
        step.inputs
    );
    render(&mut r.engine, &mut r.s, 1024);
    assert!(r.engine.has_compiled_plan(), "the plan fits the engine");
}

#[test]
fn a_drum_return_routed_to_a_set_return_arrives_there() {
    let mut r = drum_rig();
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    // A DC source on the chain, sent fully into a return chain with a Utility boost.
    let ret = r.s.add_return_chain(root, rack, "R").unwrap();
    let target = r.s.add_return().unwrap();
    r.s.set_chain_param(root, rack, chain, controls::send(0), 0.0);
    r.s.set_return_output(root, rack, ret, Some(target));
    let nested = root.child(rack, ret).unwrap();
    r.s.add_builtin(nested, "rlive.utility");
    r.s.play_midi(r.track, on(38));
    let audio = render(&mut r.engine, &mut r.s, 8192);
    assert!(r.engine.has_compiled_plan());
    assert!(audio.iter().all(|s| s.is_finite()));
}

// ------------------------------------------------------------------ extracting

fn drum_clip(p: &Project) -> Clip {
    let mut clip = Clip::new_midi("Beat", [1, 2, 3], 1, p.time_signature);
    let notes = clip.notes_mut().unwrap();
    notes.push(NoteData::new(0, PPQ, 38, 0.9));
    notes.push(NoteData::new(PPQ, PPQ, 42, 0.5));
    notes.push(NoteData::new(2 * PPQ, PPQ, 38, 0.7));
    clip
}

#[test]
fn extracting_a_drum_chain_with_notes_moves_them_to_a_new_track() {
    let mut r = drum_rig();
    let (root, rack, chain, track) = (r.root, r.rack, r.chain, r.track);
    let clip = drum_clip(&r.s.project);
    r.s.set_clip(track, 0, Some(clip));
    commit(&mut r.s);
    let new_id =
        r.s.extract_chain_to_track(root, rack, chain, ExtractNotes::With)
            .expect("extracted");
    let new_track = r.s.project.track(new_id).unwrap().clone();
    assert_eq!(new_track.devices.len(), 1, "the chain's devices moved here");
    let moved = new_track.clips[0].as_ref().unwrap().notes();
    assert_eq!(moved.len(), 2);
    assert!(
        moved.iter().all(|n| n.key == 60),
        "at the chain's Play note"
    );
    let left = r.s.project.track(track).unwrap().clips[0].as_ref().unwrap();
    assert!(
        left.notes().iter().all(|n| n.key != 38),
        "the source clip gave them up"
    );
    assert!(
        r.rack_dev().chains().next().is_none(),
        "the chain left the rack"
    );
    render(&mut r.engine, &mut r.s, 1024);
}

#[test]
fn extracting_without_notes_leaves_the_clips_alone() {
    let mut r = drum_rig();
    let (root, rack, chain, track) = (r.root, r.rack, r.chain, r.track);
    let clip = drum_clip(&r.s.project);
    r.s.set_clip(track, 0, Some(clip.clone()));
    commit(&mut r.s);
    let new_id =
        r.s.extract_chain_to_track(root, rack, chain, ExtractNotes::Without)
            .unwrap();
    assert!(r.s.project.track(new_id).unwrap().clips[0].is_none());
    assert_eq!(
        r.s.project.track(track).unwrap().clips[0]
            .as_ref()
            .unwrap()
            .notes(),
        clip.notes()
    );
}

#[test]
fn extracting_a_return_chain_makes_a_return_track() {
    let mut r = drum_rig();
    let (root, rack) = (r.root, r.rack);
    let ret = r.s.add_return_chain(root, rack, "Verb").unwrap();
    let nested = root.child(rack, ret).unwrap();
    r.s.add_builtin(nested, "rlive.reverb");
    let id = r.s.extract_return_to_track(root, rack, ret).unwrap();
    let t = r.s.project.track(id).unwrap();
    assert_eq!(t.kind, TrackKind::Return);
    assert_eq!(t.devices.len(), 1);
    assert_eq!(t.name, "Verb");
    let DeviceKind::Rack { returns, .. } = &r.rack_dev().kind else {
        panic!();
    };
    assert!(returns.is_empty());
    render(&mut r.engine, &mut r.s, 1024);
}

#[test]
fn moving_a_chain_between_racks_keeps_it_playing() {
    let mut r = rig(RackKind::Instrument);
    let (root, rack) = (r.root, r.rack);
    let b = r.s.add_chain(root, rack, "B").unwrap();
    let dest = r.s.add_rack(root, RackKind::Instrument, "Other").unwrap();
    assert!(r.s.move_chain(root, rack, b, root, dest, 0));
    let DeviceKind::Rack { chains, .. } =
        &r.s.devices(root)
            .iter()
            .find(|d| d.id == dest)
            .unwrap()
            .kind
    else {
        panic!();
    };
    assert_eq!(chains[0].id, b);
    assert_eq!(r.chain_ids(), [r.chain]);
    render(&mut r.engine, &mut r.s, 1024);
}

// ------------------------------------------------------------------ undo

#[test]
fn undo_round_trips_macro_edits() {
    let mut r = rig(RackKind::Instrument);
    round_trip(&mut r, |r| {
        r.map(0);
    });
    round_trip(&mut r, |r| {
        r.s.set_macro_range(r.root, r.rack, 0, 0, -3.0, 3.0);
    });
    round_trip(&mut r, |r| {
        r.s.invert_macro_range(r.root, r.rack, 0, 0);
    });
    round_trip(&mut r, |r| {
        r.set_macro(0, 42.0);
    });
    round_trip(&mut r, |r| {
        r.s.set_macro_name(r.root, r.rack, 0, "Name");
    });
    round_trip(&mut r, |r| {
        r.s.set_macro_exclude_random(r.root, r.rack, 0, true);
        r.s.set_macro_exclude_variations(r.root, r.rack, 0, true);
    });
    round_trip(&mut r, |r| {
        r.s.set_visible_macros(r.root, r.rack, 3);
    });
    round_trip(&mut r, |r| {
        r.s.unmap_macro(r.root, r.rack, 0, 0);
    });
}

#[test]
fn undoing_a_macro_edit_also_restores_what_the_engine_hears() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    r.set_macro(0, 0.0);
    commit(&mut r.s);
    r.set_macro(0, 127.0);
    commit(&mut r.s);
    near(r.gain_now(), r.s.runtimes[&r.device].params[0].max);
    assert!(r.s.undo());
    near(r.gain_now(), r.s.runtimes[&r.device].params[0].min);
}

#[test]
fn undo_round_trips_variations_and_rand() {
    let mut r = rig(RackKind::Instrument);
    assert!(r.map(0));
    r.set_macro(0, 10.0);
    commit(&mut r.s);
    round_trip(&mut r, |r| {
        r.s.store_variation(r.root, r.rack);
    });
    r.set_macro(0, 90.0);
    commit(&mut r.s);
    round_trip(&mut r, |r| {
        r.s.launch_variation(r.root, r.rack, 0);
    });
    round_trip(&mut r, |r| {
        r.s.rename_variation(r.root, r.rack, 0, "A");
    });
    round_trip(&mut r, |r| {
        r.s.duplicate_variation(r.root, r.rack, 0);
    });
    round_trip(&mut r, |r| {
        r.s.delete_variation(r.root, r.rack, 1);
    });
    round_trip(&mut r, |r| {
        r.s.set_macro_exclude_random(r.root, r.rack, 0, false);
        r.s.randomize_macros_with(r.root, r.rack, &mut || 0.75);
    });
}

#[test]
fn undo_round_trips_chains_zones_and_the_chain_mixer() {
    let mut r = rig(RackKind::Instrument);
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    round_trip(&mut r, |r| {
        r.s.set_chain_param(root, rack, chain, controls::VOLUME, -9.0);
    });
    round_trip(&mut r, |r| {
        r.s.set_chain_param(root, rack, chain, controls::PAN, 0.5);
        r.s.set_chain_param(root, rack, chain, controls::ACTIVATOR, 0.0);
        r.s.set_chain_param(root, rack, chain, controls::SOLO, 1.0);
    });
    round_trip(&mut r, |r| {
        let z = Zones {
            key: Zone {
                low: 40,
                high: 50,
                fade_low: 30,
                fade_high: 60,
            },
            velocity: Zone::span(20, 100),
            select: Zone::span(3, 9),
            ..Zones::default()
        };
        r.s.set_chain_zones(root, rack, chain, z);
    });
    round_trip(&mut r, |r| {
        r.s.set_select_filters_ctrl(root, rack, true);
    });
    round_trip(&mut r, |r| {
        r.s.set_chain_selector(root, rack, 77.0);
    });
    round_trip(&mut r, |r| {
        r.s.add_chain(root, rack, "B");
    });
    round_trip(&mut r, |r| {
        r.s.delete_chain(root, rack, chain);
    });
}

#[test]
fn undo_round_trips_the_drum_rack() {
    let mut r = drum_rig();
    let (root, rack) = (r.root, r.rack);
    let chain = r.chain;
    round_trip(&mut r, |r| {
        r.s.set_drum_pad(
            root,
            rack,
            chain,
            DrumPad {
                receive: Some(40),
                play: 48,
                choke: Some(3),
            },
        );
    });
    round_trip(&mut r, |r| {
        r.s.add_return_chain(root, rack, "R");
    });
    let ret = {
        let DeviceKind::Rack { returns, .. } = &r.rack_dev().kind else {
            panic!()
        };
        returns[0].id
    };
    round_trip(&mut r, |r| {
        r.s.set_chain_param(root, rack, chain, controls::send(0), -2.0);
    });
    let target = r.s.add_return().unwrap();
    commit(&mut r.s);
    round_trip(&mut r, |r| {
        r.s.set_return_output(root, rack, ret, Some(target));
    });
    round_trip(&mut r, |r| {
        r.s.delete_chain(root, rack, ret);
    });
}

#[test]
fn undo_round_trips_grouping() {
    let mut r = rig(RackKind::Instrument);
    let root = r.root;
    let poly = r.s.devices(root)[0].id;
    round_trip(&mut r, |r| {
        r.s.group_devices(root, &[poly], false);
    });
    let new_rack = r.s.devices(root)[0].id;
    round_trip(&mut r, |r| {
        r.s.ungroup_rack(root, new_rack);
    });
    let rack = r.rack;
    let b = r.s.add_chain(root, rack, "B").unwrap();
    commit(&mut r.s);
    let chain = r.chain;
    round_trip(&mut r, |r| {
        r.s.group_chains(root, rack, &[chain, b]);
    });
}

#[test]
fn undo_round_trips_extracting_chains() {
    let mut r = drum_rig();
    let (root, rack, chain, track) = (r.root, r.rack, r.chain, r.track);
    let clip = drum_clip(&r.s.project);
    r.s.set_clip(track, 0, Some(clip));
    commit(&mut r.s);
    round_trip(&mut r, |r| {
        r.s.extract_chain_to_track(root, rack, chain, ExtractNotes::With);
    });
    assert!(r.s.undo());
    let ret = r.s.add_return_chain(root, rack, "R").unwrap();
    commit(&mut r.s);
    round_trip(&mut r, |r| {
        r.s.extract_return_to_track(root, rack, ret);
    });
}

// ------------------------------------------------------------------ edits while playing

#[test]
fn rack_edits_mid_playback_never_touch_the_heap_on_the_audio_thread() {
    let mut r = drum_rig();
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    r.s.play_midi(r.track, on(38));
    let block = |r: &mut Rig| {
        let (mut l, mut rb) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
        assert_no_alloc(|| r.engine.process_stereo(&mut l, &mut rb));
        r.s.idle();
    };
    block(&mut r);
    let a = r.s.add_chain(root, rack, "A").unwrap();
    block(&mut r);
    let ret = r.s.add_return_chain(root, rack, "R").unwrap();
    block(&mut r);
    r.s.set_chain_param(root, rack, chain, controls::send(0), 0.0);
    block(&mut r);
    r.s.set_drum_pad(
        root,
        rack,
        a,
        DrumPad {
            receive: Some(38),
            play: 61,
            choke: Some(1),
        },
    );
    block(&mut r);
    r.s.set_chain_param(root, rack, a, controls::SOLO, 1.0);
    block(&mut r);
    r.s.delete_chain(root, rack, ret);
    block(&mut r);
    r.s.delete_chain(root, rack, a);
    block(&mut r);
    let poly = r.s.devices(root)[0].id;
    r.s.group_devices(root, &[poly], false);
    block(&mut r);
    r.s.play_midi(r.track, on(60));
    for _ in 0..8 {
        block(&mut r);
    }
}

// ------------------------------------------------------------------ saved Sets

#[test]
fn a_rack_survives_a_save_and_reload() {
    let mut r = drum_rig();
    let (root, rack, chain) = (r.root, r.rack, r.chain);
    assert!(r.map(0));
    r.s.set_macro_range(root, rack, 0, 0, 9.0, -9.0);
    r.set_macro(0, 90.0);
    r.s.store_variation(root, rack);
    r.s.set_visible_macros(root, rack, 12);
    let ret = r.s.add_return_chain(root, rack, "R").unwrap();
    r.s.set_chain_param(root, rack, chain, controls::send(0), -7.0);
    let target = r.s.add_return().unwrap();
    r.s.set_return_output(root, rack, ret, Some(target));
    let z = Zones {
        drum: Some(DrumPad {
            receive: Some(50),
            play: 60,
            choke: Some(4),
        }),
        ..Zones::default()
    };
    r.s.set_chain_zones(root, rack, chain, z);
    r.s.set_chain_selector(root, rack, 33.0);
    r.s.set_select_filters_ctrl(root, rack, true);
    render(&mut r.engine, &mut r.s, 1024);

    let dir = std::env::temp_dir().join(format!("rlive-racks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rack.rlive");
    r.s.project.save(&path).unwrap();
    let loaded = Project::load(&path).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(loaded, r.s.project);

    let (mut engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, loaded, PluginCache::default());
    assert_eq!(s.macro_value(root, rack, 0), 90.0);
    let nested = root.child(rack, chain).unwrap();
    assert!(s.is_macro_mapped(nested, param(r.device, 0)));
    let (l, _) = rlive_testkit::render_chunks(1024, 256, |l, rb| engine.process_stereo(l, rb));
    assert!(l.iter().all(|s| s.is_finite()));
    s.idle();
}
