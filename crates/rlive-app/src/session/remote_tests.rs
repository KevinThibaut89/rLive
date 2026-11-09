//! MIDI and key mapping through a session: bytes in at the routing layer, values out of the
//! engine. The MIDI ports here are the routing layer's own [`RemotePort`]s, fed by hand.

use super::*;
use crate::midi_input::{MidiRouting, PortSwitches, RemotePort};
use rlive_core::param::{mixer, rack as rack_params, ParamId, ParamOwner, ParamRef};
use rlive_core::{MidiMessage, Quantize, PPQ};
use rlive_engine::{create_engine, mix_cell, shared::NO_SLOT, Engine};
use rlive_project::{
    CcEncoding, Chain, Clip, Crossfade, CrossfaderPoint, MapTarget, MidiSource, RackKind,
    RemoteControl, TrackKind,
};
use rlive_remote::{
    surface::GENERIC_FIRST_KNOB, FeedbackSink, GenericKnobs, Gesture, TakeoverMode,
};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
/// 120 BPM at 48 kHz.
const BAR_FRAMES: usize = 96_000;

struct Rig {
    engine: Engine,
    s: Session,
    port: RemotePort,
    routing: MidiRouting,
}

fn rig(mut p: Project) -> Rig {
    p.quantize = Quantize::Bar;
    let (engine, mut handle) = create_engine(SR, BLOCK);
    let sender = handle.take_midi_sender();
    let mut s = Session::new(handle, p, PluginCache::default());
    for t in s.project.tracks.iter_mut() {
        t.clips.iter_mut().for_each(|c| *c = None);
    }
    s.rebuild();
    let routing = MidiRouting {
        sender: Arc::new(Mutex::new(sender)),
        targets: s.midi_targets.clone(),
        selected: Arc::new(Mutex::new(None)),
        activity: Default::default(),
        system: Default::default(),
        mpe: Default::default(),
    };
    let switches = Arc::new(PortSwitches::new(true, false, true));
    let port = s.remote.hub.open_port(0, routing.clone(), switches);
    Rig {
        engine,
        s,
        port,
        routing,
    }
}

impl Rig {
    /// Lets the audio thread and the UI catch up.
    fn run(&mut self, frames: usize) {
        let frames = frames.next_multiple_of(BLOCK);
        let (engine, s) = (&mut self.engine, &mut self.s);
        rlive_testkit::render_chunks(frames, BLOCK, |l, r| {
            engine.process_stereo(l, r);
            s.idle();
        });
    }

    /// Publishes the mappings.
    fn settle(&mut self) {
        self.run(BLOCK);
    }

    /// Sends a stream of raw messages and lets them take effect.
    fn play(&mut self, stream: &[&[u8]]) {
        for m in stream {
            self.port.handle(m);
        }
        self.settle();
    }

    fn map(&mut self, channel: u8, source: MidiSource, target: MapTarget) {
        self.s.set_midi_mapping(target, channel, source);
        self.s.commit_now();
        self.settle();
    }

    fn volume_of(&self, t: TrackId) -> f64 {
        self.s.layouts[&ChainRef::Track(t)]
            .base(mix_cell::VOLUME)
            .unwrap()
    }

    /// One block of audio with no UI in between, and the notes that reached a track in it.
    fn played_notes(&mut self) -> Vec<PlayedKey> {
        let mut left = [0.0f32; BLOCK];
        let mut right = [0.0f32; BLOCK];
        self.engine.process_stereo(&mut left, &mut right);
        let mut out = Vec::new();
        while let Some(n) = self.s.engine.poll_played() {
            if let MidiMessage::NoteOn { key, .. } = n.message {
                out.push(PlayedKey(key));
            }
        }
        out
    }
}

#[derive(Debug, PartialEq)]
struct PlayedKey(u8);

/// What's been sent to each output port.
type Sent = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

fn volume(track: TrackId) -> MapTarget {
    MapTarget::Param(ParamRef {
        owner: ParamOwner::Track(track),
        id: mixer::VOLUME,
    })
}

fn cc(number: u8) -> MidiSource {
    MidiSource::Cc {
        number,
        encoding: CcEncoding::Absolute,
    }
}

fn track0(r: &Rig) -> TrackId {
    r.s.project.tracks[0].id
}

#[test]
fn a_controller_moves_a_parameter_on_the_engine_and_in_the_project() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.map(0, cc(7), volume(t));
    r.s.set_takeover(TakeoverMode::None);
    r.play(&[&[0xb0, 7, 127]]);
    assert_eq!(r.s.project.track(t).unwrap().volume_db, 6.0, "project");
    r.run(BLOCK * 2);
    assert_eq!(r.volume_of(t), 6.0, "layout");
    let bus = r.s.layouts[&ChainRef::Track(t)]
        .effective(mix_cell::VOLUME)
        .unwrap();
    assert!((bus - 1.0).abs() < 1e-4, "engine resolved it: {bus}");
    r.play(&[&[0xb0, 7, 0]]);
    assert_eq!(r.s.project.track(t).unwrap().volume_db, -96.0);
}

#[test]
fn a_mapped_message_stops_reaching_the_track_and_an_unmapped_one_still_plays() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    *r.routing.selected.lock().unwrap() = Some(t);
    r.map(0, MidiSource::Note(60), volume(t));
    r.port.handle(&[0x90, 60, 100]);
    assert!(
        r.played_notes().is_empty(),
        "the note belongs to the mapping"
    );
    r.settle();
    r.port.handle(&[0x90, 61, 100]);
    assert_eq!(r.played_notes(), vec![PlayedKey(61)]);
    assert!(r.routing.activity.load(Ordering::Relaxed));
}

#[test]
fn track_and_remote_switches_gate_each_port() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    *r.routing.selected.lock().unwrap() = Some(t);
    r.map(0, cc(7), volume(t));
    r.s.set_takeover(TakeoverMode::None);
    let off = Arc::new(PortSwitches::new(false, false, true));
    let mut remote_only = r.s.remote.hub.open_port(1, r.routing.clone(), off);
    r.settle();
    r.s.set_remote_ports(RemotePorts {
        in_names: vec!["a".into(), "b".into()],
        in_mask: 0b10,
        out_names: Vec::new(),
        out_mask: 0,
    });
    r.settle();
    // Port 1 has Remote on and Track off, port 0 is the other way round now.
    let before = r.s.project.track(t).unwrap().volume_db;
    r.play(&[&[0xb0, 7, 90]]);
    assert_eq!(
        r.s.project.track(t).unwrap().volume_db,
        before,
        "port 0 isn't Remote"
    );
    remote_only.handle(&[0xb0, 7, 90]);
    r.settle();
    assert_ne!(r.s.project.track(t).unwrap().volume_db, before);
    remote_only.handle(&[0x90, 40, 100]);
    assert!(r.played_notes().is_empty(), "Track is off on that port");
    r.settle();
    r.port.handle(&[0x90, 40, 100]);
    assert_eq!(
        r.played_notes(),
        vec![PlayedKey(40)],
        "unmapped on the Track port"
    );
}

#[test]
fn a_mapped_slot_launches_at_quantize_and_follows_the_slot_not_the_clip() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.s.set_clip(
        t,
        0,
        Some(Clip {
            name: "one".into(),
            color: [1, 2, 3],
            length: 4 * PPQ,
            ..Clip::default()
        }),
    );
    r.map(
        0,
        MidiSource::Note(36),
        MapTarget::ClipSlot { track: t, slot: 0 },
    );
    r.settle();
    r.s.play();
    r.run(BLOCK);
    r.port.handle(&[0x90, 36, 127]);
    r.run(BLOCK * 2);
    assert_eq!(
        r.s.track_shared[&t].queued_slot.load(Ordering::Relaxed),
        0,
        "launched, waiting for the bar"
    );
    assert_eq!(
        r.s.track_shared[&t].playing_slot.load(Ordering::Relaxed),
        NO_SLOT
    );
    r.run(BAR_FRAMES);
    assert_eq!(r.s.track_shared[&t].playing_slot.load(Ordering::Relaxed), 0);
    // The clip goes, the mapping stays: an empty slot's stop still arrives there.
    r.s.set_clip(t, 0, None);
    r.settle();
    assert_eq!(r.s.project.midi_mappings.len(), 1);
}

#[test]
fn a_macro_is_driven_like_any_other_parameter() {
    let mut p = Project::new_default();
    let t = p.tracks[0].id;
    let mut rack = Device::rack(RackKind::Instrument, "Rack");
    let rack_id = rack.id;
    let mut chain = Chain::new("A");
    let inner = Device::builtin("rlive.utility", "Utility");
    let inner_id = inner.id;
    chain.devices.push(inner);
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(chain);
    }
    p.tracks[0].devices.push(rack);
    let mut r = rig(p);
    let root = ChainRef::Track(t);
    let gain = ParamRef {
        owner: ParamOwner::Device(inner_id),
        id: ParamId(0),
    };
    assert!(r.s.map_macro(root, rack_id, 0, gain));
    r.settle();
    let macro1 = ParamRef {
        owner: ParamOwner::Device(rack_id),
        id: rack_params::macro_id(0),
    };
    r.map(0, cc(20), MapTarget::Param(macro1));
    r.s.set_takeover(TakeoverMode::None);
    r.play(&[&[0xb0, 20, 127]]);
    r.run(BLOCK * 4);
    let m = r.s.macro_value(root, rack_id, 0);
    assert!((m - 127.0).abs() < 1e-6, "macro moved: {m}");
    // A parameter a Macro owns refuses a controller of its own: taken, but doing nothing.
    r.map(0, cc(21), MapTarget::Param(gain));
    let (rack_chain, before) = {
        let nested = r.s.chain_of_device(inner_id).unwrap();
        let v = r.s.effective_norm(nested, inner_id, 0).unwrap();
        (nested, v)
    };
    r.play(&[&[0xb0, 21, 0]]);
    r.run(BLOCK * 4);
    assert_eq!(r.s.effective_norm(rack_chain, inner_id, 0).unwrap(), before);
}

#[test]
fn a_hardware_gesture_latches_while_automation_records() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.map(0, cc(7), volume(t));
    r.s.set_takeover(TakeoverMode::None);
    r.s.set_automation_arm(true);
    r.settle();
    assert!(r.s.start_recording());
    r.run(BLOCK * 8);
    r.play(&[&[0xb0, 7, 64]]);
    r.run(BLOCK * 8);
    r.s.pointer_frame(true);
    r.s.pointer_frame(false);
    r.play(&[&[0xb0, 7, 90]]);
    r.run(BLOCK * 8);
    r.s.stop_recording();
    r.run(BLOCK * 8);
    let track = r.s.project.track(t).unwrap();
    let lane = track
        .automation
        .iter()
        .find(|e| {
            e.target
                == rlive_core::param::EnvTarget::Param(ParamRef {
                    owner: ParamOwner::Track(t),
                    id: mixer::VOLUME,
                })
        })
        .expect("the move was recorded");
    assert!(lane.points.len() >= 2);
}

#[test]
fn feedback_goes_to_outputs_with_remote_on() {
    #[derive(Clone, Default)]
    struct Spy(Sent);
    impl FeedbackSink for Spy {
        fn send(&mut self, port: u32, bytes: &[u8]) {
            self.0.lock().unwrap().push((port, bytes.to_vec()));
        }
    }
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    let spy = Spy::default();
    r.s.set_feedback_output(Some(Box::new(spy.clone())));
    r.s.set_remote_ports(RemotePorts {
        in_names: vec!["in".into()],
        in_mask: u64::MAX,
        out_names: vec!["a".into(), "b".into()],
        out_mask: 0b10,
    });
    r.map(0, cc(7), volume(t));
    r.settle();
    spy.0.lock().unwrap().clear();
    // Moved from the mouse side: the fader should follow, on the Remote port only.
    r.s.project.track_mut(t).unwrap().volume_db = 6.0;
    r.s.sync_mix(t);
    r.settle();
    {
        let sent = spy.0.lock().unwrap();
        assert_eq!(sent.as_slice(), &[(1, vec![0xb0, 7, 127])]);
    }
    spy.0.lock().unwrap().clear();
    // Moving the fader by hand doesn't bounce back at it.
    r.s.set_takeover(TakeoverMode::None);
    r.play(&[&[0xb0, 7, 100]]);
    r.settle();
    assert!(spy.0.lock().unwrap().is_empty());
}

#[test]
fn mappings_survive_saving_and_moving_and_undoing() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    let other = r.s.add_track(TrackKind::Audio, None);
    r.map(1, cc(10), volume(t));
    r.map(0, MidiSource::PitchBend, MapTarget::Scene(0));
    let ron = r.s.project.to_ron().unwrap();
    let back = Project::from_ron(&ron).unwrap();
    assert_eq!(back.midi_mappings, r.s.project.midi_mappings);
    // Move the track: the mapping goes with it, still driving the same track.
    r.s.move_track(t, 1);
    r.s.commit_edits(false);
    r.s.set_takeover(TakeoverMode::None);
    r.settle();
    r.play(&[&[0xb1, 10, 127]]);
    assert_eq!(r.s.project.track(t).unwrap().volume_db, 6.0);
    assert_ne!(r.s.project.track(other).unwrap().volume_db, 6.0);
    // Undo takes the mapping away and redo puts it back.
    let n = r.s.project.midi_mappings.len();
    r.s.delete_midi_mapping(0);
    r.s.commit_edits(false);
    assert_eq!(r.s.project.midi_mappings.len(), n - 1);
    assert!(r.s.undo());
    assert_eq!(r.s.project.midi_mappings.len(), n);
    r.settle();
    r.play(&[&[0xb1, 10, 0]]);
    assert_eq!(
        r.s.project.track(t).unwrap().volume_db,
        -96.0,
        "working again"
    );
}

#[test]
fn a_deleted_target_takes_its_mapping_with_it_undoably() {
    let mut r = rig(Project::new_default());
    let other = r.s.add_track(TrackKind::Midi, None);
    r.settle();
    r.map(0, cc(7), volume(other));
    r.map(0, cc(8), MapTarget::TrackArm(other));
    assert_eq!(r.s.project.midi_mappings.len(), 2);
    r.s.delete_track(other);
    r.s.commit_edits(false);
    assert!(r.s.project.midi_mappings.is_empty());
    assert!(r.s.undo());
    assert_eq!(r.s.project.midi_mappings.len(), 2, "undo brings them back");
}

#[test]
fn scenes_moving_takes_their_mappings_along() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.s.add_scene();
    r.s.add_scene();
    r.settle();
    let scenes = r.s.project.scenes.len();
    assert!(scenes >= 3);
    r.map(0, MidiSource::Note(1), MapTarget::Scene(0));
    r.map(
        0,
        MidiSource::Note(2),
        MapTarget::ClipSlot { track: t, slot: 1 },
    );
    let target = |r: &Rig, i: usize| r.s.project.midi_mappings[i].target;
    r.s.insert_scene(0);
    assert_eq!(target(&r, 0), MapTarget::Scene(0), "above the insert");
    assert_eq!(
        target(&r, 1),
        MapTarget::ClipSlot { track: t, slot: 2 },
        "below it"
    );
    r.s.delete_scene(0);
    r.s.commit_edits(false);
    assert_eq!(r.s.project.midi_mappings.len(), 1, "deleted with its scene");
    assert_eq!(target(&r, 0), MapTarget::ClipSlot { track: t, slot: 1 });
    assert!(r.s.undo());
    assert_eq!(r.s.project.midi_mappings.len(), 2);
}

#[test]
fn locator_mappings_follow_the_locator() {
    let mut r = rig(Project::new_default());
    let i = r.s.add_locator(1000);
    r.s.commit_edits(false);
    r.map(0, MidiSource::Note(1), MapTarget::Locator(1000));
    r.s.move_locator(i, 2000);
    assert_eq!(
        r.s.project.midi_mappings[0].target,
        MapTarget::Locator(2000)
    );
    r.s.delete_locator(0);
    r.s.commit_edits(false);
    assert!(r.s.project.midi_mappings.is_empty());
}

#[test]
fn learning_a_note_and_a_controller() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.s.set_map_mode(Some(MapMode::Midi));
    let slot = MapTarget::ClipSlot { track: t, slot: 0 };
    r.s.select_map_target(Some(slot));
    r.settle();
    r.play(&[&[0x90, 60, 100]]);
    assert_eq!(r.s.project.midi_mappings[0].source, MidiSource::Note(60));
    r.play(&[&[0x90, 55, 100], &[0x90, 67, 100]]);
    assert_eq!(
        r.s.project.midi_mappings[0].source,
        MidiSource::ChromaticRange {
            root: 60,
            low: 55,
            high: 67
        }
    );
    // While learning, nothing reaches a track or the old meaning of a message.
    r.s.select_map_target(Some(volume(t)));
    r.settle();
    r.play(&[&[0xb0, 9, 65], &[0xb0, 9, 65]]);
    let m =
        r.s.project
            .midi_mappings
            .iter()
            .find(|m| m.target == volume(t))
            .unwrap();
    assert_eq!(
        m.source,
        MidiSource::Cc {
            number: 9,
            encoding: CcEncoding::RelativeSignedBit
        }
    );
    r.s.set_map_mode(None);
    r.s.commit_edits(false);
    r.settle();
    // Undo goes back through what was learned.
    assert!(r.s.undo());
    assert!(r.s.project.midi_mappings.is_empty());
}

#[test]
fn keys_work_slots_switches_and_the_crossfader() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.s.set_key_mapping(MapTarget::Crossfader(CrossfaderPoint::Left), "A");
    r.s.set_key_mapping(MapTarget::Crossfader(CrossfaderPoint::Right), "B");
    r.s.set_key_mapping(
        MapTarget::Param(ParamRef {
            owner: ParamOwner::Track(t),
            id: mixer::MUTE,
        }),
        "M",
    );
    r.s.commit_edits(false);
    let find = |r: &Rig, k: &str| {
        r.s.project
            .key_mappings
            .iter()
            .find(|m| m.key == k)
            .unwrap()
            .clone()
    };
    let left = find(&r, "A");
    let right = find(&r, "B");
    r.s.key_mapping_event(&left, true);
    assert_eq!(
        r.s.project.crossfader.position, -1.0,
        "goes to the left end"
    );
    // Hold it and press the other: jumps there, and snaps back on release.
    r.s.key_mapping_event(&right, true);
    assert_eq!(r.s.project.crossfader.position, 1.0);
    r.s.key_mapping_event(&right, false);
    assert_eq!(r.s.project.crossfader.position, -1.0, "snaps back");
    r.s.key_mapping_event(&left, false);
    let mute = find(&r, "M");
    r.s.key_mapping_event(&mute, true);
    r.settle();
    assert!(r.s.project.track(t).unwrap().mute);
    r.s.key_mapping_event(&mute, true);
    assert!(
        !r.s.project.track(t).unwrap().mute,
        "a key toggles a switch"
    );
    r.s.delete_key_mapping(0);
    r.s.commit_edits(false);
    assert_eq!(r.s.project.key_mappings.len(), 2);
    assert!(r.s.undo());
    assert_eq!(r.s.project.key_mappings.len(), 3);
}

#[test]
fn a_track_activator_goes_the_right_way_round_and_crossfade_assign_is_a_radio() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    let muted = MapTarget::Param(ParamRef {
        owner: ParamOwner::Track(t),
        id: mixer::MUTE,
    });
    r.map(0, cc(1), muted);
    let assign = MapTarget::Param(ParamRef {
        owner: ParamOwner::Track(t),
        id: mixer::CROSSFADE_ASSIGN,
    });
    r.map(0, MidiSource::Note(1), assign);
    r.play(&[&[0xb0, 1, 127]]);
    assert!(!r.s.project.track(t).unwrap().mute, "the Activator is on");
    r.play(&[&[0xb0, 1, 0]]);
    assert!(r.s.project.track(t).unwrap().mute);
    r.play(&[&[0x90, 1, 1]]);
    assert_eq!(r.s.project.track(t).unwrap().crossfade, Crossfade::A);
    r.play(&[&[0x90, 1, 1]]);
    assert_eq!(r.s.project.track(t).unwrap().crossfade, Crossfade::B);
    r.play(&[&[0x90, 1, 1]]);
    assert_eq!(r.s.project.track(t).unwrap().crossfade, Crossfade::None);
}

#[test]
fn a_control_surface_follows_the_selected_device_and_can_be_locked() {
    let mut p = Project::new_default();
    let t = p.tracks[0].id;
    let a = Device::builtin("rlive.utility", "A");
    let b = Device::builtin("rlive.utility", "B");
    let (a_id, b_id) = (a.id, b.id);
    p.tracks[0].devices.push(a);
    p.tracks[0].devices.push(b);
    let mut r = rig(p);
    r.s.remote.surfaces.insert(0, Box::new(GenericKnobs));
    r.s.remote.surfaces.set_ports(0, Some("Keys".into()), None);
    r.s.set_remote_ports(RemotePorts {
        in_names: vec!["Keys".into()],
        in_mask: 0,
        out_names: Vec::new(),
        out_mask: 0,
    });
    r.s.set_takeover(TakeoverMode::None);
    r.s.remote.selected_device = Some(a_id);
    r.settle();
    let knob: &[u8] = &[0xb0, GENERIC_FIRST_KNOB, 127];
    let chain = ChainRef::Track(t);
    r.play(&[knob]);
    let first = r.s.runtimes[&a_id].params[0].id;
    let a_value = |r: &Rig| {
        r.s.devices(chain)
            .iter()
            .find(|d| d.id == a_id)
            .unwrap()
            .stored_param(first)
    };
    assert_eq!(
        a_value(&r),
        Some(r.s.runtimes[&a_id].params[0].from_normalized(1.0))
    );
    // Selecting another device retargets the knob.
    r.s.remote.selected_device = Some(b_id);
    r.settle();
    r.play(&[&[0xb0, GENERIC_FIRST_KNOB, 0]]);
    let b_param = r.s.runtimes[&b_id].params[0].id;
    let b_value = |r: &Rig| {
        r.s.devices(chain)
            .iter()
            .find(|d| d.id == b_id)
            .unwrap()
            .stored_param(b_param)
    };
    assert!(b_value(&r).is_some());
    assert_eq!(
        a_value(&r),
        Some(r.s.runtimes[&a_id].params[0].from_normalized(1.0)),
        "A left alone"
    );
    // Locked, it stays on A wherever the selection goes.
    r.s.lock_surface(0, Some(a_id));
    r.s.remote.selected_device = Some(b_id);
    r.settle();
    r.play(&[&[0xb0, GENERIC_FIRST_KNOB, 0]]);
    assert_ne!(
        a_value(&r),
        Some(r.s.runtimes[&a_id].params[0].from_normalized(1.0))
    );
    assert!(r.s.remote.surfaces.is_locked(a_id));
    // A manual mapping of the same control takes over.
    let vol = volume(t);
    r.map(0, cc(GENERIC_FIRST_KNOB), vol);
    assert!(r.s.mapped_label(vol, MapMode::Midi).is_some());
    let _ = NO_SLOT;
}

#[test]
fn controls_reach_the_ui_through_actions() {
    let mut r = rig(Project::new_default());
    r.map(
        0,
        MidiSource::Note(1),
        MapTarget::Control(RemoteControl::Metronome),
    );
    r.map(
        0,
        MidiSource::Cc {
            number: 3,
            encoding: CcEncoding::RelativeTwosComplement,
        },
        MapTarget::Control(RemoteControl::SceneScroll),
    );
    r.play(&[&[0x90, 1, 100], &[0xb0, 3, 2]]);
    let actions = r.s.take_remote_actions();
    assert_eq!(
        actions,
        vec![
            (
                MapTarget::Control(RemoteControl::Metronome),
                Gesture::Toggle
            ),
            (
                MapTarget::Control(RemoteControl::SceneScroll),
                Gesture::Step(2)
            )
        ]
    );
}

#[test]
fn a_remote_port_with_a_table_mapped_never_allocates_on_a_mapped_message() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.map(0, cc(7), volume(t));
    r.s.set_takeover(TakeoverMode::None);
    r.settle();
    r.port.handle(&[0xb0, 7, 10]);
    rlive_testkit::assert_no_alloc(|| {
        for v in 0..100u8 {
            r.port.handle(&[0xb0, 7, v % 127]);
        }
    });
}

#[test]
fn the_mapping_browser_edits_range_inverts_and_deletes() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.map(0, cc(7), volume(t));
    r.s.set_takeover(TakeoverMode::None);
    let row = r.s.mapping_rows(MapMode::Midi)[0].clone();
    assert_eq!(row.control, "1 CC 7");
    assert_eq!((row.min, row.max), (-96.0, 6.0));
    assert!(row.path.contains("Poly"), "{}", row.path);
    assert_eq!(row.name, "Volume");
    r.s.set_mapping_range(MapMode::Midi, 0, -20.0, 0.0);
    r.settle();
    r.play(&[&[0xb0, 7, 127]]);
    assert_eq!(
        r.s.project.track(t).unwrap().volume_db,
        0.0,
        "new top of the range"
    );
    r.s.invert_mapping(MapMode::Midi, 0);
    r.s.commit_now();
    r.settle();
    assert_eq!(r.s.project.midi_mappings[0].min, 0.0);
    r.play(&[&[0xb0, 7, 0], &[0xb0, 7, 127]]);
    assert_eq!(r.s.project.track(t).unwrap().volume_db, -20.0);
    r.s.set_mapping_encoding(0, CcEncoding::RelativeTwosComplement);
    r.settle();
    assert_eq!(
        r.s.mapping_rows(MapMode::Midi)[0].encoding,
        Some(CcEncoding::RelativeTwosComplement)
    );
    assert!(r.s.undo());
    r.s.delete_midi_mapping(0);
    r.s.commit_now();
    assert!(r.s.mapping_rows(MapMode::Midi).is_empty());
}

#[test]
fn a_mapped_scene_control_launches_through_the_engine_and_a_locator_seeks() {
    let mut r = rig(Project::new_default());
    let t = track0(&r);
    r.s.set_clip(
        t,
        0,
        Some(Clip {
            name: "one".into(),
            color: [1, 2, 3],
            length: 4 * PPQ,
            ..Clip::default()
        }),
    );
    r.map(0, MidiSource::Note(1), MapTarget::Scene(0));
    r.settle();
    r.s.play();
    r.run(BLOCK);
    r.play(&[&[0x90, 1, 127]]);
    r.run(BAR_FRAMES);
    assert_eq!(r.s.track_shared[&t].playing_slot.load(Ordering::Relaxed), 0);
    r.s.stop();
    r.run(BLOCK * 2);
    r.s.add_locator(4 * PPQ);
    r.s.commit_edits(false);
    r.map(0, MidiSource::Note(2), MapTarget::Locator(4 * PPQ));
    r.play(&[&[0x90, 2, 127]]);
    r.run(BLOCK);
    assert!(
        (r.s.engine.shared.tick.load() - (4 * PPQ) as f64).abs() < 1.0,
        "the locator was jumped to"
    );
}

#[test]
fn sysex_goes_to_the_surfaces_and_clock_goes_to_the_engine() {
    let mut r = rig(Project::new_default());
    let on = Arc::new(PortSwitches::new(true, true, true));
    let mut p = r.s.remote.hub.open_port(1, r.routing.clone(), on);
    p.handle(&[0xf8]);
    p.handle(&[0xf0, 1, 2, 0xf7]);
    assert_eq!(r.routing.system.take_sysex(), vec![vec![0xf0, 1, 2, 0xf7]]);
    assert!(
        r.routing.system.drain().is_empty(),
        "a Sync port's clock goes to the engine, not the queue"
    );
    assert!(r
        .s
        .remote
        .hub
        .shared
        .lights
        .sync_in
        .swap(false, std::sync::atomic::Ordering::Relaxed));
    // Off, neither is kept.
    let off = Arc::new(PortSwitches::new(true, false, false));
    let mut q = r.s.remote.hub.open_port(2, r.routing.clone(), off);
    q.handle(&[0xf8]);
    q.handle(&[0xf0, 1, 2, 0xf7]);
    assert!(r.routing.system.drain().is_empty());
}
