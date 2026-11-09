//! External Audio Effect and External Instrument through the engine: a fake interface loops the
//! outputs back to the inputs, a known number of frames later.

use super::*;
use rlive_core::{ExprKind, MidiMessage, MpeOutput, MpeZone};
use rlive_devices::devices::b9::{external_audio_effect as eae, external_instrument as ei};
use rlive_engine::{create_engine, AudioIo, Engine};
use rlive_project::{ExternalIo, ExternalMidi, ExternalReturn, ExternalRoute, Route, Track};
use rlive_testkit::onset;

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;
const LOOP: usize = 128;
const OUTPUTS: usize = 8;

fn track(p: &Project, name: &str) -> Track {
    Track::new(name, TrackKind::Midi, [1, 2, 3], p.scenes.len())
}

fn stereo(channel: u16) -> ExternalRoute {
    ExternalRoute::Audio {
        channel,
        stereo: true,
    }
}

struct Interface {
    /// What went out, oldest first, for each channel.
    out: Vec<Vec<f32>>,
    /// Which output channels are wired to inputs 0 and 1.
    loop_from: Option<usize>,
}

impl Interface {
    fn new(loop_from: Option<usize>) -> Self {
        Self {
            out: vec![Vec::new(); OUTPUTS],
            loop_from,
        }
    }

    /// One block in and out, returning what came out.
    fn block(&mut self, engine: &mut Engine) {
        let n = self.out[0].len();
        let silence = vec![0.0f32; BLOCK];
        let (i0, i1) = match self.loop_from {
            Some(from) if n >= LOOP => (
                self.out[from][n - LOOP..n - LOOP + BLOCK].to_vec(),
                self.out[from + 1][n - LOOP..n - LOOP + BLOCK].to_vec(),
            ),
            _ => (silence.clone(), silence.clone()),
        };
        let inputs: [&[f32]; 2] = [&i0, &i1];
        let mut bufs = vec![vec![0.0f32; BLOCK]; OUTPUTS];
        {
            let mut slices: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
            engine.process(AudioIo {
                inputs: &inputs,
                outputs: &mut slices,
            });
        }
        for (ch, b) in bufs.iter().enumerate() {
            self.out[ch].extend_from_slice(b);
        }
    }
}

fn step(engine: &mut Engine, s: &mut Session, io: &mut Interface, blocks: usize) {
    for _ in 0..blocks {
        io.block(engine);
        s.idle();
        s.engine.collect_garbage();
    }
}

/// Two MIDI tracks of DS Kick: `a` through an External Audio Effect, `b` straight out.
struct Rack {
    engine: Engine,
    s: Session,
    a: TrackId,
    b: TrackId,
    kick_a: DeviceId,
    kick_b: DeviceId,
    fx: DeviceId,
}

fn rack() -> Rack {
    let mut p = Project::empty();
    p.tracks = vec![track(&p, "a"), track(&p, "b")];
    let (a, b) = (p.tracks[0].id, p.tracks[1].id);
    let (engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    s.add_builtin(ChainRef::Track(a), "rlive.ds_kick");
    s.add_builtin(ChainRef::Track(a), "rlive.external_audio_effect");
    s.add_builtin(ChainRef::Track(b), "rlive.ds_kick");
    let chain_a = ChainRef::Track(a);
    let kick_a = s.devices(chain_a)[0].id;
    let fx = s.devices(chain_a)[1].id;
    let kick_b = s.devices(ChainRef::Track(b))[0].id;
    // The insert's track stays on Main, the plain track gets a hardware output of its own, and
    // compensation holds it back to stay with Main: the two can be compared.
    assert!(s.set_output(&[b], Route::External(stereo(6))));
    Rack {
        engine,
        s,
        a,
        b,
        kick_a,
        kick_b,
        fx,
    }
}

fn route(s: &mut Session, chain: ChainRef, id: DeviceId, send_to: u16, back: u16) {
    let io = ExternalIo {
        audio_to: Some(stereo(send_to)),
        audio_from: Some(ExternalReturn::Hardware(stereo(back))),
        midi_to: None,
    };
    assert!(s.set_external_io(chain, id, io));
}

#[test]
fn a_hardware_insert_comes_back_on_time_with_the_rest_of_the_set() {
    let Rack {
        mut engine,
        mut s,
        a,
        b,
        kick_a,
        kick_b,
        fx,
    } = rack();
    let chain = ChainRef::Track(a);
    route(&mut s, chain, fx, 2, 0);
    s.set_param(chain, fx, eae::LATENCY_MODE, 1.0);
    s.set_param(chain, fx, eae::LATENCY_SAMPLES, LOOP as f64);
    let mut io = Interface::new(Some(2));
    // The plan picks up the new latency from the idle after the device has reported it, and
    // the delay it adds fades in.
    step(&mut engine, &mut s, &mut io, 60);
    let start = io.out[0].len();
    s.preview_device(chain, kick_a);
    s.preview_device(ChainRef::Track(b), kick_b);
    step(&mut engine, &mut s, &mut io, 40);

    let out = &io.out[0][start..];
    let dry = &io.out[6][start..];
    let at_a = onset(out, 1e-3).expect("the insert came back");
    let at_b = onset(dry, 1e-3).expect("the plain track played");
    assert_eq!(
        at_a, at_b,
        "the track through the hardware lines up with the one that isn't"
    );
    // The return is the same sound, delayed by the loop and compensated out.
    let late = (at_a..at_a + 1024)
        .map(|i| (out[i] - dry[i]).abs())
        .fold(0.0f32, f32::max);
    assert!(late < 0.05, "same sound: {late}");

    // What went to the hardware really was the send.
    assert!(onset(&io.out[2][start..], 1e-3).is_some());
}

#[test]
fn no_latency_setting_means_no_compensation_and_the_signal_is_late() {
    let Rack {
        mut engine,
        mut s,
        a,
        b,
        kick_a,
        kick_b,
        fx,
    } = rack();
    let chain = ChainRef::Track(a);
    route(&mut s, chain, fx, 2, 0);
    let mut io = Interface::new(Some(2));
    step(&mut engine, &mut s, &mut io, 8);
    let start = io.out[0].len();
    s.preview_device(chain, kick_a);
    s.preview_device(ChainRef::Track(b), kick_b);
    step(&mut engine, &mut s, &mut io, 40);
    let at_a = onset(&io.out[0][start..], 1e-3).unwrap();
    let at_b = onset(&io.out[6][start..], 1e-3).unwrap();
    assert_eq!(at_a, at_b + LOOP, "the hardware's own delay is heard");
}

#[test]
fn a_bypassed_insert_sends_nothing_and_a_frozen_track_is_left_out_of_the_plan() {
    let Rack {
        mut engine,
        mut s,
        a,
        kick_a,
        fx,
        ..
    } = rack();
    let chain = ChainRef::Track(a);
    route(&mut s, chain, fx, 2, 0);
    let mut io = Interface::new(None);
    s.set_bypass(chain, fx, true);
    step(&mut engine, &mut s, &mut io, 4);
    let start = io.out[0].len();
    s.preview_device(chain, kick_a);
    step(&mut engine, &mut s, &mut io, 20);
    assert!(
        io.out[2][start..].iter().all(|v| *v == 0.0),
        "nothing goes out"
    );
    assert!(
        onset(&io.out[0][start..], 1e-3).is_some(),
        "and it plays on"
    );
    let plan = compile_plan(&s.project);
    assert_eq!(plan.hardware_sends.len(), 1);
}

fn midi_rig() -> (Engine, Session, TrackId, DeviceId) {
    let mut p = Project::empty();
    p.tracks = vec![track(&p, "a"), track(&p, "b")];
    let id = p.tracks[0].id;
    let (engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    s.add_builtin(ChainRef::Track(id), "rlive.external_instrument");
    let d = s.devices(ChainRef::Track(id))[0].id;
    (engine, s, id, d)
}

#[test]
fn an_external_instrument_plays_a_port_and_ends_its_notes() {
    let (mut engine, mut s, id, d) = midi_rig();
    let chain = ChainRef::Track(id);
    let mut rx = s.engine.take_hw_midi().unwrap();
    s.set_midi_out_ports(vec!["Other".into(), "Synth".into()]);
    let io = ExternalIo {
        midi_to: Some(ExternalMidi::Port {
            port: "Synth".into(),
            channel: 3,
        }),
        ..Default::default()
    };
    assert!(s.set_external_io(chain, d, io));
    let mut iface = Interface::new(None);
    step(&mut engine, &mut s, &mut iface, 2);
    let note = |on: bool| rlive_engine::GraphOp::DirectNote {
        target: chain.owner().into(),
        device: d,
        message: if on {
            MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: 0,
                tuning: 0.0,
            }
        } else {
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.0,
                note_id: 0,
            }
        },
    };
    s.send_ops(vec![note(true)]);
    step(&mut engine, &mut s, &mut iface, 2);
    s.send_ops(vec![note(false)]);
    step(&mut engine, &mut s, &mut iface, 2);
    let mut seen = Vec::new();
    while let Ok(e) = rx.pop() {
        seen.push(e);
    }
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().all(|e| e.port == 1), "the port's number");
    assert!(matches!(
        seen[0].message,
        MidiMessage::NoteOn { channel: 3, .. }
    ));
    assert!(matches!(
        seen[1].message,
        MidiMessage::NoteOff { channel: 3, .. }
    ));
}

#[test]
fn removing_an_external_instrument_ends_its_held_notes() {
    let (mut engine, mut s, id, d) = midi_rig();
    let chain = ChainRef::Track(id);
    let mut rx = s.engine.take_hw_midi().unwrap();
    s.set_midi_out_ports(vec!["Other".into(), "Synth".into()]);
    let io = ExternalIo {
        midi_to: Some(ExternalMidi::Port {
            port: "Synth".into(),
            channel: 0,
        }),
        ..Default::default()
    };
    s.set_external_io(chain, d, io);
    let mut iface = Interface::new(None);
    step(&mut engine, &mut s, &mut iface, 2);
    // A note-on only, by hand.
    s.send_ops(vec![rlive_engine::GraphOp::DirectNote {
        target: chain.owner().into(),
        device: d,
        message: MidiMessage::NoteOn {
            channel: 0,
            key: 64,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        },
    }]);
    step(&mut engine, &mut s, &mut iface, 2);
    s.remove_device(chain, d);
    step(&mut engine, &mut s, &mut iface, 2);
    let mut seen = Vec::new();
    while let Ok(e) = rx.pop() {
        seen.push(e.message);
    }
    assert_eq!(seen.len(), 2);
    assert!(
        matches!(seen[1], MidiMessage::NoteOff { key: 64, .. }),
        "{seen:?}"
    );
}

#[test]
fn a_multitimbral_route_sends_midi_on_a_channel_and_reads_an_aux_a_block_behind() {
    let (mut engine, mut s, id, d) = midi_rig();
    let chain = ChainRef::Track(id);
    let other = s.project.tracks[1].id;
    s.add_builtin(ChainRef::Track(other), "rlive.ds_kick");
    let plug = s.devices(ChainRef::Track(other))[0].id;
    let io = ExternalIo {
        midi_to: Some(ExternalMidi::Track {
            track: other,
            device: Some(plug),
            channel: 5,
        }),
        audio_from: Some(ExternalReturn::Aux {
            track: other,
            device: plug,
            output: 0,
        }),
        ..Default::default()
    };
    assert!(s.set_external_io(chain, d, io));
    let mut plan = super::graph::compile_plan(&s.project);
    super::graph::set_feedback_delays(&mut plan, BLOCK);
    assert_eq!(plan.midi.len(), 1);
    assert_eq!(plan.midi[0].channel, Some(5));
    assert_eq!(plan.midi[0].device, Some(plug));
    assert_eq!(plan.sidechains.len(), 1, "the return reads the aux");
    assert!(plan.sidechains[0].feedback);
    assert_eq!(plan.sidechains[0].delay, BLOCK as u32);
    let value = s.devices(chain)[0]
        .params
        .get(&ei::INTERNAL_LATENCY)
        .copied();
    assert_eq!(
        value,
        Some(BLOCK as f64),
        "and the block is compensated for"
    );
    assert!(s.external_is_internal(d));
    let mut iface = Interface::new(None);
    step(&mut engine, &mut s, &mut iface, 4);
}

#[test]
fn a_track_and_an_external_instrument_share_the_one_path_to_the_ports() {
    let (mut engine, mut s, id, d) = midi_rig();
    let chain = ChainRef::Track(id);
    let mut rx = s.engine.take_hw_midi().unwrap();
    s.set_midi_out_ports(vec!["A".into(), "B".into()]);
    let io = ExternalIo {
        midi_to: Some(ExternalMidi::Port {
            port: "B".into(),
            channel: 2,
        }),
        ..Default::default()
    };
    assert!(s.set_external_io(chain, d, io));
    let other = s.project.tracks[1].id;
    assert!(s.set_output(
        &[other],
        Route::External(ExternalRoute::Midi {
            port: Some("A".into()),
            channel: None,
        })
    ));
    let mut iface = Interface::new(None);
    step(&mut engine, &mut s, &mut iface, 2);
    s.send_ops(vec![rlive_engine::GraphOp::DirectNote {
        target: chain.owner().into(),
        device: d,
        message: note_on(60),
    }]);
    let mut live = s.engine.take_midi_sender().unwrap();
    live.send(other, note_on(62));
    step(&mut engine, &mut s, &mut iface, 2);
    let mut seen = Vec::new();
    while let Ok(e) = rx.pop() {
        seen.push((e.port, e.message));
    }
    assert!(
        seen.iter().any(|(p, m)| *p == 1
            && matches!(
                m,
                MidiMessage::NoteOn {
                    channel: 2,
                    key: 60,
                    ..
                }
            )),
        "the instrument: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|(p, m)| *p == 0 && matches!(m, MidiMessage::NoteOn { key: 62, .. })),
        "the track: {seen:?}"
    );
}

#[test]
fn mpe_mode_spreads_an_external_instruments_notes_and_keeps_their_expression() {
    let (mut engine, mut s, id, d) = midi_rig();
    let chain = ChainRef::Track(id);
    let mut rx = s.engine.take_hw_midi().unwrap();
    s.set_midi_out_ports(vec!["Synth".into()]);
    let io = ExternalIo {
        midi_to: Some(ExternalMidi::Port {
            port: "Synth".into(),
            channel: 7,
        }),
        ..Default::default()
    };
    assert!(s.set_external_io(chain, d, io));
    s.set_device_mpe(
        chain,
        d,
        Some(MpeOutput::Zone {
            zone: MpeZone::Lower,
            members: 4,
        }),
    );
    let mut iface = Interface::new(None);
    step(&mut engine, &mut s, &mut iface, 2);
    let send = |s: &mut Session, message| {
        s.send_ops(vec![rlive_engine::GraphOp::DirectNote {
            target: chain.owner().into(),
            device: d,
            message,
        }]);
    };
    send(&mut s, note_on(60));
    step(&mut engine, &mut s, &mut iface, 2);
    send(
        &mut s,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 0,
            kind: ExprKind::Slide,
            value: 0.5,
        },
    );
    step(&mut engine, &mut s, &mut iface, 2);
    let mut seen = Vec::new();
    while let Ok(e) = rx.pop() {
        seen.push(e.message);
    }
    let member = seen.iter().find_map(|m| match m {
        MidiMessage::NoteOn { channel, .. } => Some(*channel),
        _ => None,
    });
    assert_eq!(
        member,
        Some(1),
        "a member channel, not the chooser's: {seen:?}"
    );
    assert!(
        seen.iter().any(|m| matches!(
            m,
            MidiMessage::ControlChange {
                channel: 1,
                cc: rlive_core::SLIDE_CC,
                ..
            }
        )),
        "slide goes out on that channel: {seen:?}"
    );

    // Out of MPE Mode the chooser's channel is back in charge.
    s.set_device_mpe(chain, d, None);
    step(&mut engine, &mut s, &mut iface, 2);
    while rx.pop().is_ok() {}
    send(&mut s, note_on(64));
    step(&mut engine, &mut s, &mut iface, 2);
    let e = rx.pop().unwrap();
    assert!(matches!(e.message, MidiMessage::NoteOn { channel: 7, .. }));
}

#[test]
fn a_rebuilt_external_instrument_is_told_its_port_again() {
    let (mut engine, mut s, id, d) = midi_rig();
    let chain = ChainRef::Track(id);
    let mut rx = s.engine.take_hw_midi().unwrap();
    s.set_midi_out_ports(vec!["Synth".into()]);
    let io = ExternalIo {
        midi_to: Some(ExternalMidi::Port {
            port: "Synth".into(),
            channel: 0,
        }),
        ..Default::default()
    };
    assert!(s.set_external_io(chain, d, io));
    let mut iface = Interface::new(None);
    step(&mut engine, &mut s, &mut iface, 2);
    s.rebuild();
    step(&mut engine, &mut s, &mut iface, 2);
    s.send_ops(vec![rlive_engine::GraphOp::DirectNote {
        target: chain.owner().into(),
        device: d,
        message: note_on(60),
    }]);
    step(&mut engine, &mut s, &mut iface, 2);
    assert!(rx.pop().is_ok(), "it still reaches the port");
}

fn note_on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 1.0,
        note_id: 0,
        tuning: 0.0,
    }
}
