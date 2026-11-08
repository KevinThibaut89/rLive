//! Tap points, track-to-track routing, sidechains, multi-output instruments, the crossfader, Cue
//! and multichannel output: what the plan works out, and what comes out of the engine.

use super::graph::compile_with;
use super::timing::{bus_shape, buses_op, compensation_ops, LatencySource, Timing};
use super::*;
use rlive_core::{
    AudioInput, ChainId, CrossfadeCurve, MidiEvent, MidiMessage, ProcessContext, Processor,
    SampleBuffer, PPQ,
};
use rlive_engine::{
    create_engine, mix_cell, ArrangementLane, AudioClip, AudioIo, ChainNode, ClipData, DeviceSlot,
    Engine, EngineCommand, EngineHandle, GraphTxn, LaneClip, ParamCmd, PreviewCmd,
    RackKind as EngineRack, RackNode, RenderPlan, Step, TrackNode, TransportCmd,
};
use rlive_project::{
    Chain, ChainTap, Crossfade, ExternalRoute, Monitor, RackKind, Route, SidechainSource, Tap,
    Track, TrackKind,
};
use rlive_testkit::assert_no_alloc;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

#[derive(Default)]
struct Fake(HashMap<DeviceId, u32>);

impl LatencySource for Fake {
    fn device_latency(&self, device: &Device) -> u32 {
        self.0.get(&device.id).copied().unwrap_or(0)
    }
}

/// Adds a constant to the audio passing through.
struct Dc(f32);
impl Processor for Dc {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.audio.left.iter_mut().for_each(|s| *s += self.0);
        ctx.audio.right.iter_mut().for_each(|s| *s += self.0);
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Scales the audio passing through.
struct Scale(f32);
impl Processor for Scale {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.audio.left.iter_mut().for_each(|s| *s *= self.0);
        ctx.audio.right.iter_mut().for_each(|s| *s *= self.0);
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Replaces the audio with its sidechain, or silence.
struct Keyed;
impl Processor for Keyed {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = ctx.audio.left.len();
        match ctx.sidechain {
            Some(AudioInput { left, right }) => {
                ctx.audio.left[..n].copy_from_slice(&left[..n]);
                ctx.audio.right[..n].copy_from_slice(&right[..n]);
            }
            None => ctx.audio.clear(),
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// An impulse at frame `at`.
struct Click {
    at: u64,
    pos: u64,
    level: f32,
}
impl Processor for Click {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = ctx.audio.left.len() as u64;
        if self.at >= self.pos && self.at < self.pos + n {
            let i = (self.at - self.pos) as usize;
            ctx.audio.left[i] += self.level;
            ctx.audio.right[i] += self.level;
        }
        self.pos += n;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Holds audio back by a fixed number of frames and reports that as its latency.
struct Late {
    latency: u32,
    line: [Vec<f32>; 2],
    head: usize,
}
impl Processor for Late {
    fn activate(&mut self, _: f64, _: usize) {}
    fn latency(&self) -> u32 {
        self.latency
    }
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = self.line[0].len();
        for (i, (l, r)) in ctx
            .audio
            .left
            .iter_mut()
            .zip(ctx.audio.right.iter_mut())
            .enumerate()
        {
            let at = (self.head + i) % n;
            std::mem::swap(l, &mut self.line[0][at]);
            std::mem::swap(r, &mut self.line[1][at]);
        }
        self.head = (self.head + ctx.audio.left.len()) % n;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Writes a constant to its first aux output, and more to its main out.
struct MultiOut {
    main: f32,
    aux: f32,
    routed: Arc<std::sync::atomic::AtomicU32>,
}
impl Processor for MultiOut {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.audio.left.iter_mut().for_each(|s| *s += self.main);
        ctx.audio.right.iter_mut().for_each(|s| *s += self.main);
        if let Some(aux) = ctx.aux_out.first_mut() {
            aux.left.fill(self.aux);
            aux.right.fill(self.aux);
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
    fn set_aux_routed(&mut self, mask: u32) {
        self.routed
            .store(mask, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Plays one note, once.
struct Note(u8);
impl Processor for Note {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        if self.0 != 0 {
            ctx.events_out.push(MidiEvent {
                offset: 3,
                message: MidiMessage::NoteOn {
                    channel: 0,
                    key: self.0,
                    velocity: 1.0,
                    note_id: 0,
                    tuning: 0.0,
                },
            });
            self.0 = 0;
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Transposes MIDI.
struct Transpose(i16);
impl Processor for Transpose {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for e in ctx.events {
            let mut e = *e;
            if let MidiMessage::NoteOn { key, .. } = &mut e.message {
                *key = (*key as i16 + self.0) as u8;
            }
            ctx.events_out.push(e);
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Writes down every note it hears: `(block position, key, channel)`.
#[derive(Clone, Default)]
struct Heard(Arc<Mutex<Vec<(u32, u8, u8)>>>);

struct Recorder {
    heard: Heard,
    pos: u32,
}
impl Processor for Recorder {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let mut heard = self.heard.0.lock().unwrap();
        for e in ctx.events {
            if let MidiMessage::NoteOn { key, channel, .. } = e.message {
                heard.push((self.pos + e.offset, key, channel));
            }
        }
        self.pos += ctx.audio.left.len() as u32;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn audio(p: &Project, name: &str) -> Track {
    Track::new(name, TrackKind::Audio, [1, 2, 3], p.scenes.len())
}

fn midi(p: &Project, name: &str) -> Track {
    Track::new(name, TrackKind::Midi, [1, 2, 3], p.scenes.len())
}

fn device(name: &str, kind: &mut Rigged, processor: Box<dyn Processor>) -> Device {
    let d = Device::builtin("rlive.reverb", name);
    kind.procs.insert(d.id, processor);
    d
}

fn slot(d: &Device, rig: &mut Rigged) -> Box<DeviceSlot> {
    if let DeviceKind::Rack {
        kind,
        chains,
        returns,
        ..
    } = &d.kind
    {
        let kind = match kind {
            RackKind::Drum => EngineRack::Drum,
            _ => EngineRack::Audio,
        };
        let mut rack = Box::new(RackNode::new(kind));
        let build = |c: &Chain, rig: &mut Rigged| {
            let mut node = Box::new(ChainNode::new(c.id, BLOCK));
            let mut mix = rlive_engine::ChainMix {
                volume_db: c.mix.volume_db,
                pan: c.mix.pan,
                active: c.mix.active,
                solo: c.mix.solo,
                output: c.mix.output,
                ..Default::default()
            };
            for send in &c.mix.sends {
                if let Some(n) = returns.iter().position(|r| r.id == send.to) {
                    mix.sends[n] = send.level_db;
                }
            }
            node.seed_mix(mix);
            node.zones.drum = c.zones.drum.map(|p| rlive_engine::DrumPad {
                receive: p.receive,
                play: p.play,
                choke: p.choke,
            });
            for inner in &c.devices {
                node.devices.push(slot(inner, rig));
            }
            node
        };
        for c in chains {
            let node = build(c, rig);
            rack.chains.push(node);
        }
        for c in returns {
            let node = build(c, rig);
            rack.returns.push(node);
        }
        rack.seed_gates();
        let mut s = DeviceSlot::rack(d.id, rack);
        s.bypass = d.bypass;
        return Box::new(s);
    }
    let processor = rig
        .procs
        .remove(&d.id)
        .unwrap_or_else(|| Box::new(Scale(1.0)));
    let mut s = DeviceSlot::new(d.id, processor);
    if rig.aux.contains(&d.id) {
        s = s.with_aux_outputs(2, BLOCK);
    }
    s.midi_effect = rig.midi_effects.contains(&d.id);
    Box::new(s)
}

/// The processors and latencies a project's devices get when built by hand.
#[derive(Default)]
struct Rigged {
    fake: Fake,
    procs: HashMap<DeviceId, Box<dyn Processor>>,
    midi_effects: std::collections::HashSet<DeviceId>,
    aux: std::collections::HashSet<DeviceId>,
    /// Track id → a clip of this level, playing from the start.
    clips: HashMap<TrackId, f32>,
}

impl Rigged {
    fn latent(&mut self, name: &str, latency: u32) -> Device {
        let d = device(
            name,
            self,
            Box::new(Late {
                latency,
                line: [vec![0.0; latency as usize], vec![0.0; latency as usize]],
                head: 0,
            }),
        );
        self.fake.0.insert(d.id, latency);
        d
    }

    fn midi_effect(&mut self, processor: Box<dyn Processor>) -> Device {
        let d = device("fx", self, processor);
        self.midi_effects.insert(d.id);
        d
    }
}

fn clip_lane(level: f32) -> Arc<ArrangementLane> {
    let len = 48_000 * 8;
    let sample = SampleBuffer {
        name: "dc".into(),
        left: vec![level; len],
        right: vec![level; len],
        sample_rate: SR,
    };
    let clip = ClipData::Audio(AudioClip {
        sample: Arc::new(sample),
        length: PPQ * 16,
        looped: false,
        gain: 1.0,
        scale: None,
    });
    Arc::new(ArrangementLane {
        clips: vec![LaneClip {
            start: 0,
            end: PPQ * 16,
            offset: 0,
            data: Arc::new(clip),
            fades: Default::default(),
        }],
    })
}

fn node(t: &Track, returns: &[TrackId], rig: &mut Rigged) -> Box<TrackNode> {
    let mut node = TrackNode::new(t.id, BLOCK);
    if t.kind == TrackKind::Return {
        node = node.with_feedback(BLOCK);
    }
    for (k, ret) in returns.iter().enumerate() {
        node.seed_send(k, t.send_level(*ret));
    }
    let mut node = Box::new(node);
    node.mix = mix_of(t);
    for d in &t.devices {
        node.devices.push(slot(d, rig));
    }
    if let Some(level) = rig.clips.get(&t.id) {
        node.set_lane(clip_lane(*level));
    }
    node
}

fn install(
    handle: &mut EngineHandle,
    p: &Project,
    rig: &mut Rigged,
    plan_only: bool,
) -> RenderPlan {
    let timing = Timing::measure(p, SR, &rig.fake);
    let plan = compile_with(p, &timing);
    let returns: Vec<TrackId> = p.returns.iter().map(|t| t.id).collect();
    let mut ops = Vec::new();
    if !plan_only {
        let tracks: Vec<_> = p
            .tracks
            .iter()
            .chain(&p.returns)
            .map(|t| node(t, &returns, rig))
            .collect();
        let mut master = Box::new(TrackNode::master(BLOCK));
        for d in &p.master.devices {
            master.devices.push(slot(d, rig));
        }
        ops.push(GraphOp::ReplaceAll {
            tracks: Box::new(tracks),
            master,
        });
    }
    ops.push(buses_op(&bus_shape(&plan), BLOCK));
    ops.extend(compensation_ops(&plan, &timing, p, BLOCK));
    ops.extend(super::graph::rack_out_ops(p, BLOCK));
    let txn = GraphTxn::new(ops, Some(plan.clone()));
    assert!(handle.send(EngineCommand::Graph(txn)).is_ok());
    plan
}

struct Rig {
    engine: Engine,
    handle: EngineHandle,
    plan: RenderPlan,
}

fn rig(p: &Project, rigged: &mut Rigged) -> Rig {
    let (engine, mut handle) = create_engine(SR, BLOCK);
    let plan = install(&mut handle, p, rigged, false);
    Rig {
        engine,
        handle,
        plan,
    }
}

impl Rig {
    fn play(&mut self) {
        assert!(self
            .handle
            .send(EngineCommand::Transport(TransportCmd::Play))
            .is_ok());
    }

    /// `frames` frames of the Main output, left channel. The block that applies a transaction
    /// is the only one allowed to allocate.
    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        let mut first = true;
        while out.len() < frames {
            let n = BLOCK.min(frames - out.len());
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            if first {
                self.engine.process_stereo(&mut l, &mut r);
                first = false;
            } else {
                assert_no_alloc(|| self.engine.process_stereo(&mut l, &mut r));
            }
            out.extend_from_slice(&l);
            self.handle.collect_garbage();
        }
        out
    }

    /// What the signal settles to on the Main output.
    fn settled(&mut self) -> f32 {
        let out = self.render(BLOCK * 16);
        *out.last().unwrap()
    }

    /// One buffer of every output channel at once.
    fn render_outputs(&mut self, channels: usize) -> Vec<Vec<f32>> {
        let mut last = vec![vec![0.0; BLOCK]; channels];
        for _ in 0..20 {
            let mut bufs = vec![vec![0.0; BLOCK]; channels];
            let mut slices: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
            self.engine.process(AudioIo {
                inputs: &[],
                outputs: &mut slices,
            });
            self.handle.collect_garbage();
            last = bufs;
        }
        last
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

fn plan_of(p: &Project) -> RenderPlan {
    compile_with(p, &Timing::default())
}

/// Tapped track A plays 1.0 through a 0.5 effect at -6 dB, and B taps it at `tap`.
fn tap_scene(tap: Tap) -> (Project, Rigged, TrackId, TrackId) {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut source = audio(&p, "source");
    let mut a = audio(&p, "a");
    let mut b = audio(&p, "b");
    source
        .devices
        .push(device("src", &mut rigged, Box::new(Dc(1.0))));
    source.routing.output = Route::Track {
        track: a.id,
        device: None,
    };
    a.devices
        .push(device("half", &mut rigged, Box::new(Scale(0.5))));
    a.volume_db = -6.0206;
    b.routing.input = Route::Track {
        track: a.id,
        device: None,
    };
    b.routing.tap = tap;
    b.routing.monitor = Monitor::In;
    let (ia, ib) = (a.id, b.id);
    p.tracks = vec![source, a, b];
    (p, rigged, ia, ib)
}

#[test]
fn a_tapped_track_is_heard_at_the_point_it_is_tapped() {
    for (tap, want) in [
        (Tap::PreFx, 1.0),
        (Tap::PostFx, 0.5),
        (Tap::PostMixer, 0.25),
    ] {
        let (mut p, mut rigged, a, _) = tap_scene(tap);
        p.track_mut(a).unwrap().routing.output = Route::SendsOnly;
        let mut r = rig(&p, &mut rigged);
        assert!(close(r.settled(), want), "{tap:?}");
    }
}

#[test]
fn soloing_a_track_that_taps_another_hears_it_except_post_mixer() {
    for (tap, want) in [(Tap::PreFx, 1.0), (Tap::PostFx, 0.5), (Tap::PostMixer, 0.0)] {
        let (mut p, mut rigged, a, b) = tap_scene(tap);
        p.track_mut(a).unwrap().routing.output = Route::SendsOnly;
        p.track_mut(b).unwrap().solo = true;
        let mut r = rig(&p, &mut rigged);
        assert!(close(r.settled(), want), "{tap:?}");
    }
}

#[test]
fn a_track_routed_to_another_is_heard_through_it() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    let mut b = audio(&p, "b");
    a.devices
        .push(device("one", &mut rigged, Box::new(Dc(0.5))));
    b.devices
        .push(device("half", &mut rigged, Box::new(Scale(0.5))));
    a.routing.output = Route::Track {
        track: b.id,
        device: None,
    };
    p.tracks = vec![a, b];
    let plan = plan_of(&p);
    let main = plan.steps.last().unwrap();
    assert_eq!(main.inputs.iter().map(|i| i.from).collect::<Vec<_>>(), [1]);
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.25), "through B's chain, once");
}

/// Solo over a track feeding another: A and C feed B, B plays a clip of its own.
fn feeding_scene() -> (Project, Rigged, [TrackId; 3]) {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    let mut c = audio(&p, "c");
    let b = audio(&p, "b");
    let bid = b.id;
    let route = Route::Track {
        track: bid,
        device: None,
    };
    a.routing.output = route.clone();
    c.routing.output = route;
    a.devices.push(device("a", &mut rigged, Box::new(Dc(0.25))));
    c.devices
        .push(device("c", &mut rigged, Box::new(Dc(0.125))));
    let (aid, cid) = (a.id, c.id);
    rigged.clips.insert(bid, 0.5);
    p.tracks = vec![a, c, b];
    (p, rigged, [aid, cid, bid])
}

#[test]
fn soloing_the_track_fed_keeps_what_feeds_it() {
    let (mut p, mut rigged, [_, _, b]) = feeding_scene();
    p.track_mut(b).unwrap().solo = true;
    let mut r = rig(&p, &mut rigged);
    r.play();
    assert!(
        close(r.settled(), 0.875),
        "its clip and everything feeding it"
    );
}

#[test]
fn soloing_a_feeder_silences_everything_else_at_the_destination() {
    let (mut p, mut rigged, [a, ..]) = feeding_scene();
    p.track_mut(a).unwrap().solo = true;
    let mut r = rig(&p, &mut rigged);
    r.play();
    assert!(close(r.settled(), 0.25), "only A is left in B");
}

#[test]
fn an_audio_track_input_is_only_heard_while_monitoring() {
    let cases = [
        (Monitor::Off, false, false),
        (Monitor::Auto, false, false),
        (Monitor::Auto, true, true),
        (Monitor::In, false, true),
    ];
    for (monitor, armed, heard) in cases {
        let (mut p, mut rigged, a, b) = tap_scene(Tap::PostFx);
        p.track_mut(a).unwrap().routing.output = Route::SendsOnly;
        let t = p.track_mut(b).unwrap();
        t.routing.monitor = monitor;
        t.arm = armed;
        let mut r = rig(&p, &mut rigged);
        let v = r.settled();
        assert_eq!(close(v, 0.5), heard, "{monitor:?} armed {armed}: {v}");
    }
}

#[test]
fn monitoring_in_suppresses_the_clips_on_the_track() {
    let (mut p, mut rigged, a, b) = tap_scene(Tap::PostFx);
    p.track_mut(a).unwrap().routing.output = Route::SendsOnly;
    p.track_mut(b).unwrap().routing.monitor = Monitor::In;
    rigged.clips.insert(b, 0.5);
    let mut r = rig(&p, &mut rigged);
    r.play();
    assert!(close(r.settled(), 0.5), "the tap, without the clip");
}

#[test]
fn auto_monitoring_gives_way_to_a_playing_clip() {
    let (mut p, mut rigged, a, b) = tap_scene(Tap::PostFx);
    p.track_mut(a).unwrap().routing.output = Route::SendsOnly;
    let t = p.track_mut(b).unwrap();
    t.routing.monitor = Monitor::Auto;
    t.arm = true;
    rigged.clips.insert(b, 0.25);
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.5), "stopped: the input");
    r.play();
    assert!(close(r.settled(), 0.25), "playing: only the clip");
}

fn midi_scene(
    target: impl FnOnce(TrackId, DeviceId) -> Route,
) -> (Project, Rigged, Heard, TrackId) {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = midi(&p, "a");
    let mut b = midi(&p, "b");
    let note = rigged.midi_effect(Box::new(Note(60)));
    a.devices.push(note);
    let heard = Heard::default();
    let inst = device(
        "inst",
        &mut rigged,
        Box::new(Recorder {
            heard: heard.clone(),
            pos: 0,
        }),
    );
    let inst_id = inst.id;
    b.devices.push(inst);
    a.routing.output = target(b.id, inst_id);
    let bid = b.id;
    p.tracks = vec![a, b];
    (p, rigged, heard, bid)
}

#[test]
fn midi_to_track_in_goes_through_monitoring() {
    let track_in = |t, _| Route::Track {
        track: t,
        device: None,
    };
    let (p, mut rigged, heard, _) = midi_scene(track_in);
    let mut r = rig(&p, &mut rigged);
    r.render(BLOCK);
    assert!(heard.0.lock().unwrap().is_empty(), "not monitoring");

    let (mut p, mut rigged, heard, b) = midi_scene(track_in);
    p.track_mut(b).unwrap().routing.monitor = Monitor::In;
    let mut r = rig(&p, &mut rigged);
    r.render(BLOCK);
    assert_eq!(*heard.0.lock().unwrap(), [(3, 60, 0)], "monitoring");
}

#[test]
fn midi_to_a_device_skips_monitoring() {
    let (p, mut rigged, heard, _) = midi_scene(|t, d| Route::Track {
        track: t,
        device: Some(d),
    });
    assert!(plan_of(&p).midi[0].device.is_some());
    let mut r = rig(&p, &mut rigged);
    r.render(BLOCK);
    assert_eq!(*heard.0.lock().unwrap(), [(3, 60, 0)]);
}

#[test]
fn midi_to_a_channel_of_a_track_goes_out_on_that_channel() {
    let (mut p, mut rigged, heard, b) = midi_scene(|t, _| Route::TrackChannel {
        track: t,
        channel: 5,
    });
    p.track_mut(b).unwrap().routing.monitor = Monitor::In;
    let mut r = rig(&p, &mut rigged);
    r.render(BLOCK);
    assert_eq!(*heard.0.lock().unwrap(), [(3, 60, 5)]);
}

#[test]
fn midi_tapped_from_another_track_comes_after_its_midi_effects() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = midi(&p, "a");
    let mut b = midi(&p, "b");
    a.devices.push(rigged.midi_effect(Box::new(Note(60))));
    a.devices.push(rigged.midi_effect(Box::new(Transpose(7))));
    let heard_on_a = Heard::default();
    a.devices.push(device(
        "inst a",
        &mut rigged,
        Box::new(Recorder {
            heard: heard_on_a.clone(),
            pos: 0,
        }),
    ));
    let heard = Heard::default();
    b.devices.push(device(
        "inst b",
        &mut rigged,
        Box::new(Recorder {
            heard: heard.clone(),
            pos: 0,
        }),
    ));
    b.routing.input = Route::Track {
        track: a.id,
        device: None,
    };
    b.routing.monitor = Monitor::In;
    p.tracks = vec![a, b];
    let mut r = rig(&p, &mut rigged);
    r.render(BLOCK);
    assert_eq!(
        *heard.0.lock().unwrap(),
        [(3, 67, 0)],
        "the transposed note"
    );
    assert_eq!(*heard_on_a.0.lock().unwrap(), [(3, 67, 0)]);
}

#[test]
fn soloing_a_track_that_only_sends_midi_keeps_the_track_it_plays() {
    let (mut p, mut rigged, _, b) = midi_scene(|t, d| Route::Track {
        track: t,
        device: Some(d),
    });
    let a = p.tracks[0].id;
    p.track_mut(b)
        .unwrap()
        .devices
        .push(device("sound", &mut rigged, Box::new(Dc(0.5))));
    p.track_mut(a).unwrap().solo = true;
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.5), "its notes are played over there");
}

#[test]
fn a_route_that_would_close_a_loop_is_refused() {
    let (mut p, _rigged, a, b) = tap_scene(Tap::PostFx);
    p.tracks[0].routing.output = Route::Default;
    let (engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    assert!(s.set_output(
        &[a],
        Route::Track {
            track: b,
            device: None
        }
    ));
    // a → b and b taps a's input: a loop back to a is refused from either end.
    assert!(!s.set_output(
        &[b],
        Route::Track {
            track: a,
            device: None
        }
    ));
    assert_eq!(s.project.track(b).unwrap().routing.output, Route::Default);
    assert!(!s.set_input(
        &[a],
        Route::Track {
            track: b,
            device: None,
        },
        Tap::PostMixer,
    ));
    assert_eq!(s.project.track(a).unwrap().routing.input, Route::Default);
    drop(engine);
}

#[test]
fn a_loop_in_a_file_compiles_to_something_runnable() {
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    let mut b = audio(&p, "b");
    let (ia, ib) = (a.id, b.id);
    a.routing.output = Route::Track {
        track: ib,
        device: None,
    };
    b.routing.output = Route::Track {
        track: ia,
        device: None,
    };
    a.routing.input = Route::Track {
        track: ib,
        device: None,
    };
    a.routing.monitor = Monitor::In;
    p.tracks = vec![a, b];
    let plan = plan_of(&p);
    assert!(plan
        .steps
        .iter()
        .all(|s| s.inputs.iter().all(|i| i.from != s.track)));
    let (mut engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    s.idle();
    assert!(engine.has_compiled_plan());
}

#[test]
fn a_chooser_never_offers_a_loop_or_the_track_itself() {
    let (mut p, _rigged, a, b) = tap_scene(Tap::PostFx);
    p.tracks[0].routing.output = Route::Default;
    let (_engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    s.set_output(
        &[a],
        Route::Track {
            track: b,
            device: None,
        },
    );
    let hw = io::AudioHardware::default();
    let labels = |list: Vec<RouteChoice>| list.into_iter().map(|c| c.label).collect::<Vec<_>>();
    let outputs = labels(s.output_types(b, &hw));
    assert!(!outputs.contains(&"a".to_string()), "{outputs:?}");
    assert!(!outputs.contains(&"b".to_string()), "itself");
    let inputs = labels(s.input_types(a, &hw));
    assert!(!inputs.contains(&"b".to_string()), "{inputs:?}");
    assert!(inputs.contains(&"source".to_string()));
}

/// A key on track B taps source S's Post FX, S 50 frames late, B 200 late before the key.
#[test]
fn a_sidechain_is_compensated_to_the_sample() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut s = audio(&p, "s");
    let mut b = audio(&p, "b");
    let at = 8192;
    s.devices.push(device(
        "src",
        &mut rigged,
        Box::new(Click {
            at,
            pos: 0,
            level: 1.0,
        }),
    ));
    s.devices.push(rigged.latent("s late", 50));
    s.routing.output = Route::SendsOnly;
    b.devices.push(rigged.latent("b late", 200));
    let mut key = device("key", &mut rigged, Box::new(Keyed));
    key.sidechain = Some(SidechainSource {
        track: s.id,
        tap: Tap::PostFx,
    });
    b.devices.push(key);
    p.tracks = vec![s, b];
    let mut r = rig(&p, &mut rigged);
    assert_eq!(r.plan.sidechains[0].delay, 150, "50 late meets 200 late");
    let out = r.render(16_384);
    let hit = out.iter().position(|s| s.abs() > 1e-6);
    assert_eq!(hit, Some(at as usize + 200));
    assert!(close(out[at as usize + 200], 1.0));
}

#[test]
fn a_later_sidechain_makes_the_keyed_track_wait() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut s = audio(&p, "s");
    let mut b = audio(&p, "b");
    s.devices.push(rigged.latent("s late", 300));
    b.devices.push(rigged.latent("b late", 50));
    let mut key = device("key", &mut rigged, Box::new(Keyed));
    key.sidechain = Some(SidechainSource {
        track: s.id,
        tap: Tap::PostFx,
    });
    b.devices.push(key);
    p.tracks = vec![s, b];
    let timing = Timing::measure(&p, SR, &rigged.fake);
    let plan = compile_with(&p, &timing);
    assert_eq!(plan.sidechains[0].delay, 0);
    assert_eq!(plan.io[1].own_delay, 250, "B's clips wait for the key");
}

#[test]
fn an_output_routed_to_a_sidechain_input_reaches_the_device() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    let mut b = audio(&p, "b");
    a.devices
        .push(device("one", &mut rigged, Box::new(Dc(0.5))));
    let key = device("key", &mut rigged, Box::new(Keyed));
    let key_id = key.id;
    b.devices.push(key);
    a.routing.output = Route::Track {
        track: b.id,
        device: Some(key_id),
    };
    p.tracks = vec![a, b];
    let plan = plan_of(&p);
    assert_eq!(plan.sidechains.len(), 1);
    assert_eq!(
        plan.steps
            .last()
            .unwrap()
            .inputs
            .iter()
            .map(|i| i.from)
            .collect::<Vec<_>>(),
        [1],
        "A's output goes nowhere else"
    );
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.5));
}

#[test]
fn a_rack_chain_is_tapped_where_asked() {
    for (point, want) in [
        (ChainTap::PreFx, 1.0),
        (ChainTap::PostFx, 0.5),
        (ChainTap::PostMixer, 0.25),
    ] {
        let mut rigged = Rigged::default();
        let mut p = Project::empty();
        let mut source = audio(&p, "source");
        let mut a = audio(&p, "a");
        let mut b = audio(&p, "b");
        source
            .devices
            .push(device("src", &mut rigged, Box::new(Dc(1.0))));
        source.routing.output = Route::Track {
            track: a.id,
            device: None,
        };
        let mut rack = Device::rack(RackKind::Audio, "rack");
        let mut chain = Chain::new("c");
        chain
            .devices
            .push(device("half", &mut rigged, Box::new(Scale(0.5))));
        chain.mix.volume_db = -6.0206;
        let chain_id: ChainId = chain.id;
        if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
            chains.push(chain);
        }
        a.devices.push(rack);
        a.routing.output = Route::SendsOnly;
        b.routing.input = Route::Track {
            track: a.id,
            device: None,
        };
        b.routing.tap = Tap::Chain {
            chain: chain_id,
            point,
        };
        b.routing.monitor = Monitor::In;
        b.solo = true;
        p.tracks = vec![source, a, b];
        let mut r = rig(&p, &mut rigged);
        assert!(close(r.settled(), want), "{point:?}");
    }
}

/// An instrument's second output, tapped by another track.
fn multi_out_scene() -> (Project, Rigged, Arc<std::sync::atomic::AtomicU32>) {
    let routed = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = midi(&p, "a");
    let mut b = audio(&p, "b");
    let inst = Device::builtin("rlive.poly", "multi");
    rigged.procs.insert(
        inst.id,
        Box::new(MultiOut {
            main: 0.5,
            aux: 0.25,
            routed: routed.clone(),
        }),
    );
    let id = inst.id;
    rigged.aux.insert(id);
    a.devices.push(inst);
    a.routing.output = Route::SendsOnly;
    b.routing.input = Route::Track {
        track: a.id,
        device: None,
    };
    b.routing.tap = Tap::Aux {
        device: id,
        output: 0,
    };
    b.routing.monitor = Monitor::In;
    p.tracks = vec![a, b];
    (p, rigged, routed)
}

#[test]
fn a_multi_output_instrument_is_a_source() {
    let (p, mut rigged, routed) = multi_out_scene();
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.25));
    assert_eq!(
        routed.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the instrument is told which output is tapped"
    );
}

#[test]
fn soloing_a_track_that_taps_an_aux_out_hears_it() {
    let (mut p, mut rigged, _) = multi_out_scene();
    let b = p.tracks[1].id;
    p.track_mut(b).unwrap().solo = true;
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.25));
}

#[test]
fn a_track_can_take_a_multi_timbral_part_by_channel_and_its_output() {
    let (mut p, mut rigged, _) = multi_out_scene();
    let mut c = midi(&p, "c");
    c.routing.output = Route::TrackChannel {
        track: p.tracks[0].id,
        channel: 9,
    };
    p.tracks.push(c);
    let plan = plan_of(&p);
    assert_eq!(plan.midi[0].channel, Some(9));
    let mut r = rig(&p, &mut rigged);
    assert!(
        close(r.settled(), 0.25),
        "the aux output still comes through"
    );
}

fn crossfade_scene(position: f32, curve: CrossfadeCurve, side: Crossfade) -> Project {
    let mut p = Project::empty();
    p.crossfader.position = position;
    p.crossfader.curve = curve.index();
    let mut t = audio(&p, "a");
    t.crossfade = side;
    p.tracks = vec![t];
    p
}

/// A Dc track on the given side at a given crossfader, with its level compared to the curve.
fn crossfaded(position: f32, curve: CrossfadeCurve, side: Crossfade) -> f32 {
    let mut rigged = Rigged::default();
    let mut p = crossfade_scene(position, curve, side);
    p.tracks[0]
        .devices
        .push(device("one", &mut rigged, Box::new(Dc(1.0))));
    let mut r = rig(&p, &mut rigged);
    assert!(r
        .handle
        .send(EngineCommand::Param(ParamCmd::Set {
            track: rlive_engine::TrackRef::Master,
            cell: mix_cell::CROSSFADER,
            value: position as f64,
            gesture: false,
        }))
        .is_ok());
    r.settled()
}

#[test]
fn the_crossfader_follows_the_chosen_curve_for_its_assigned_tracks() {
    for curve in CrossfadeCurve::ALL {
        for position in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
            let (a, b) = curve.gains(position);
            assert!(
                close(crossfaded(position, curve, Crossfade::A), a),
                "{}, {position}",
                curve.name()
            );
            assert!(
                close(crossfaded(position, curve, Crossfade::B), b),
                "{}, {position}",
                curve.name()
            );
        }
    }
}

#[test]
fn an_unassigned_track_ignores_the_crossfader() {
    assert!(close(
        crossfaded(1.0, CrossfadeCurve::FastCut, Crossfade::None),
        1.0
    ));
}

#[test]
fn the_crossfader_applies_to_return_tracks_too() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    p.crossfader.position = 1.0;
    let mut a = audio(&p, "a");
    let mut r = Track::new("r", TrackKind::Return, [1, 1, 1], 0);
    r.crossfade = Crossfade::A;
    a.devices
        .push(device("one", &mut rigged, Box::new(Dc(1.0))));
    a.routing.output = Route::SendsOnly;
    a.sends.push(rlive_project::TrackSend {
        to: r.id,
        level_db: 0.0,
        enabled: true,
    });
    p.tracks = vec![a];
    p.returns = vec![r];
    let mut x = rig(&p, &mut rigged);
    assert!(x
        .handle
        .send(EngineCommand::Param(ParamCmd::Set {
            track: rlive_engine::TrackRef::Master,
            cell: mix_cell::CROSSFADER,
            value: 1.0,
            gesture: false,
        }))
        .is_ok());
    assert!(close(x.settled(), 0.0), "the return is on A, faded out");
}

#[test]
fn the_crossfader_can_be_automated() {
    use rlive_core::ParamRange;
    use rlive_engine::{AutomationLane, TrackRoutes};
    let mut rigged = Rigged::default();
    let mut p = crossfade_scene(0.0, CrossfadeCurve::Dipped, Crossfade::B);
    p.tracks[0]
        .devices
        .push(device("one", &mut rigged, Box::new(Dc(1.0))));
    let mut r = rig(&p, &mut rigged);
    let range = ParamRange {
        min: -1.0,
        max: 1.0,
        skew: 1.0,
        quantized: false,
    };
    let lane = AutomationLane::new(mix_cell::CROSSFADER, range, [(0.0, 1.0, 0.0)]).unwrap();
    let routes = Arc::new(TrackRoutes::new(vec![lane]));
    assert!(r
        .handle
        .send(EngineCommand::Param(ParamCmd::SetRoutes {
            track: rlive_engine::TrackRef::Master,
            routes,
        }))
        .is_ok());
    // At the centre, Dipped gives B half. Automation puts the fader at full B.
    assert!(close(r.settled(), 1.0));
}

#[test]
fn cue_sends_a_track_to_its_own_output_without_touching_the_main_output() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    let mut b = audio(&p, "b");
    a.devices.push(device("a", &mut rigged, Box::new(Dc(0.5))));
    b.devices.push(device("b", &mut rigged, Box::new(Dc(0.25))));
    a.solo = true;
    p.tracks = vec![a, b];
    p.cue.cue_mode = true;
    p.cue.out = Some(ExternalRoute::Audio {
        channel: 2,
        stereo: true,
    });
    let mut r = rig(&p, &mut rigged);
    let out = r.render_outputs(4);
    assert!(close(out[0][BLOCK - 1], 0.75), "Main hears everything");
    assert!(
        close(out[2][BLOCK - 1], 0.5),
        "Cue hears only the cued track"
    );
}

#[test]
fn a_track_with_its_activator_off_can_still_be_cued() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    a.devices.push(device("a", &mut rigged, Box::new(Dc(0.5))));
    a.solo = true;
    a.mute = true;
    p.tracks = vec![a];
    p.cue.cue_mode = true;
    p.cue.out = Some(ExternalRoute::Audio {
        channel: 2,
        stereo: true,
    });
    let mut r = rig(&p, &mut rigged);
    let out = r.render_outputs(4);
    assert!(close(out[0][BLOCK - 1], 0.0), "out of the mix");
    assert!(close(out[2][BLOCK - 1], 0.5), "but in the headphones");
}

#[test]
fn cue_volume_scales_only_the_cue_output() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    a.devices.push(device("a", &mut rigged, Box::new(Dc(0.5))));
    a.solo = true;
    p.tracks = vec![a];
    p.cue.cue_mode = true;
    p.cue.volume_db = -6.0206;
    p.cue.out = Some(ExternalRoute::Audio {
        channel: 2,
        stereo: true,
    });
    let mut r = rig(&p, &mut rigged);
    assert!(r
        .handle
        .send(EngineCommand::Param(ParamCmd::Set {
            track: rlive_engine::TrackRef::Master,
            cell: mix_cell::CUE_VOLUME,
            value: -6.0206,
            gesture: false,
        }))
        .is_ok());
    let out = r.render_outputs(4);
    assert!(close(out[0][BLOCK - 1], 0.5));
    assert!(close(out[2][BLOCK - 1], 0.25));
}

fn preview_sample() -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer {
        name: "p".into(),
        left: vec![0.5; 48_000],
        right: vec![0.5; 48_000],
        sample_rate: SR,
    })
}

#[test]
fn a_browser_preview_goes_to_the_cue_output_when_cue_is_on() {
    let mut p = Project::empty();
    p.cue.cue_mode = true;
    p.cue.out = Some(ExternalRoute::Audio {
        channel: 2,
        stereo: true,
    });
    let mut rigged = Rigged::default();
    let mut r = rig(&p, &mut rigged);
    assert!(r
        .handle
        .send(EngineCommand::Preview(PreviewCmd::Play(preview_sample())))
        .is_ok());
    let out = r.render_outputs(4);
    assert!(close(out[2][BLOCK - 1], 0.5), "on the Cue output");
    assert!(close(out[0][BLOCK - 1], 0.0), "not the Main output");
}

#[test]
fn a_browser_preview_goes_to_the_main_output_otherwise() {
    let p = Project::empty();
    let mut rigged = Rigged::default();
    let mut r = rig(&p, &mut rigged);
    assert!(r
        .handle
        .send(EngineCommand::Preview(PreviewCmd::Play(preview_sample())))
        .is_ok());
    let out = r.render_outputs(2);
    assert!(close(out[0][BLOCK - 1], 0.5));
    assert!(r
        .handle
        .send(EngineCommand::Preview(PreviewCmd::Stop))
        .is_ok());
    let out = r.render_outputs(2);
    assert_eq!(out[0][BLOCK - 1], 0.0, "stopped");
}

#[test]
fn a_track_can_go_to_its_own_hardware_output() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    a.devices.push(device("a", &mut rigged, Box::new(Dc(0.5))));
    a.routing.output = Route::External(ExternalRoute::Audio {
        channel: 4,
        stereo: true,
    });
    let mut b = audio(&p, "b");
    b.devices.push(device("b", &mut rigged, Box::new(Dc(0.25))));
    b.routing.output = Route::External(ExternalRoute::Audio {
        channel: 2,
        stereo: false,
    });
    p.tracks = vec![a, b];
    let mut r = rig(&p, &mut rigged);
    let out = r.render_outputs(6);
    assert!(close(out[4][BLOCK - 1], 0.5), "stereo pair, left");
    assert!(close(out[5][BLOCK - 1], 0.5), "stereo pair, right");
    assert!(
        close(out[2][BLOCK - 1], 0.25),
        "mono: both sides summed at -6 dB"
    );
    assert_eq!(out[0][BLOCK - 1], 0.0, "not on Main");
    assert_eq!(out[3][BLOCK - 1], 0.0);
}

#[test]
fn the_main_output_can_be_any_channel_pair() {
    let mut p = Project::empty();
    let mut rigged = Rigged::default();
    let mut a = audio(&p, "a");
    a.devices.push(device("a", &mut rigged, Box::new(Dc(0.5))));
    p.tracks = vec![a];
    p.master.routing.output = Route::External(ExternalRoute::Audio {
        channel: 2,
        stereo: true,
    });
    let mut r = rig(&p, &mut rigged);
    let out = r.render_outputs(4);
    assert!(close(out[2][BLOCK - 1], 0.5));
    assert!(close(out[3][BLOCK - 1], 0.5));
    assert_eq!(out[0][BLOCK - 1], 0.0);
}

#[test]
fn a_stereo_main_on_a_mono_device_sums_at_minus_six() {
    let mut p = Project::empty();
    let mut rigged = Rigged::default();
    let mut a = audio(&p, "a");
    a.devices.push(device("a", &mut rigged, Box::new(Dc(0.5))));
    a.pan = -1.0;
    p.tracks = vec![a];
    p.master.routing.output = Route::External(ExternalRoute::Audio {
        channel: 0,
        stereo: false,
    });
    let mut r = rig(&p, &mut rigged);
    let out = r.render_outputs(1);
    // Hard left: 0.5 on one side, summed with silence and attenuated by 6 dB.
    assert!(close(out[0][BLOCK - 1], 0.25));
}

#[test]
fn sends_only_and_no_input_are_honoured() {
    let (mut p, mut rigged, a, b) = tap_scene(Tap::PostFx);
    p.track_mut(a).unwrap().routing.output = Route::SendsOnly;
    p.track_mut(b).unwrap().routing.input = Route::None;
    let plan = plan_of(&p);
    assert!(plan
        .steps
        .iter()
        .find(|s| s.track == 2)
        .unwrap()
        .inputs
        .is_empty());
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.0));
}

#[test]
fn no_input_hides_the_arm_button() {
    let mut p = Project::empty();
    let a = midi(&p, "a");
    let id = a.id;
    p.tracks = vec![a];
    let (_e, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    assert!(io::can_arm(&s.project, id));
    s.set_arm(id, true, true);
    assert!(s.project.track(id).unwrap().arm);
    s.set_input(&[id], Route::None, Tap::default());
    assert!(!io::can_arm(&s.project, id));
    assert!(!s.project.track(id).unwrap().arm, "and disarms it");
    s.set_arm(id, true, true);
    assert!(!s.project.track(id).unwrap().arm);
}

#[test]
fn routing_edits_apply_to_every_selected_track_and_undo() {
    let mut p = Project::empty();
    p.tracks = vec![audio(&p, "a"), audio(&p, "b"), audio(&p, "c")];
    let ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    let (_e, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    let before = s.project.clone();
    let to = Route::Track {
        track: ids[2],
        device: None,
    };
    assert!(s.set_output(&ids[..2], to.clone()));
    s.set_monitor(&ids, Monitor::In);
    s.set_crossfade_assign(&ids[..1], Crossfade::B);
    s.set_crossfader(0.5);
    s.set_crossfade_curve(CrossfadeCurve::SlowCut);
    s.set_cue_mode(true);
    s.commit_edits(false);
    for id in &ids[..2] {
        assert_eq!(s.project.track(*id).unwrap().routing.output, to);
    }
    assert!(s.undo());
    assert_eq!(s.project.tracks, before.tracks);
    assert_eq!(s.project.crossfader, before.crossfader);
    assert_eq!(s.project.cue, before.cue);
    assert!(s.redo());
    assert!(s.project.cue.cue_mode);
    assert_eq!(s.project.track(ids[0]).unwrap().routing.output, to);
    assert_eq!(s.project.crossfader.curve, CrossfadeCurve::SlowCut.index());
}

#[test]
fn delete_returns_monitor_to_each_kinds_default() {
    let mut p = Project::empty();
    p.tracks = vec![audio(&p, "a"), midi(&p, "b")];
    let ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    let (_e, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    s.set_monitor(&ids, Monitor::In);
    s.reset_monitor(&ids);
    assert_eq!(
        s.project.track(ids[0]).unwrap().routing.monitor,
        Monitor::Off
    );
    assert_eq!(
        s.project.track(ids[1]).unwrap().routing.monitor,
        Monitor::Auto
    );
}

/// Fuzz: random routings, swapped in by plan under the allocation guard.
#[test]
fn any_routing_renders_under_the_allocation_guard() {
    let mut seed = 0x9e37u32;
    let mut next = |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed % n
    };
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut tracks: Vec<Track> = (0..6).map(|i| audio(&p, &format!("{i}"))).collect();
    for t in &mut tracks {
        t.devices.push(device("dc", &mut rigged, Box::new(Dc(0.1))));
    }
    p.tracks = tracks;
    p.returns
        .push(Track::new("r", TrackKind::Return, [1, 1, 1], 0));
    let (mut engine, mut handle) = create_engine(SR, BLOCK);
    install(&mut handle, &p, &mut rigged, false);
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    for _ in 0..200 {
        let t = next(p.tracks.len() as u32) as usize;
        let other = p.tracks[next(p.tracks.len() as u32) as usize].id;
        let route = match next(5) {
            0 => Route::Default,
            1 => Route::SendsOnly,
            2 => Route::Track {
                track: other,
                device: None,
            },
            3 => Route::None,
            _ => Route::External(ExternalRoute::Audio {
                channel: 2,
                stereo: next(2) == 0,
            }),
        };
        p.tracks[t].routing.output = route;
        p.tracks[t].routing.input = if next(3) == 0 {
            Route::Track {
                track: other,
                device: None,
            }
        } else {
            Route::Default
        };
        p.tracks[t].routing.tap = match next(4) {
            0 => Tap::PreFx,
            1 => Tap::PostFx,
            _ => Tap::PostMixer,
        };
        p.tracks[t].routing.monitor = if next(2) == 0 {
            Monitor::In
        } else {
            Monitor::Auto
        };
        p.tracks[t].crossfade = match next(3) {
            0 => Crossfade::A,
            1 => Crossfade::B,
            _ => Crossfade::None,
        };
        p.tracks[t].solo = next(8) == 0;
        p.cue.cue_mode = next(5) == 0;
        let plan = install(&mut handle, &p, &mut rigged, true);
        assert!(plan.steps.len() == 8);
        for _ in 0..4 {
            let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
            assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
            assert!(l.iter().all(|s| s.is_finite()));
            handle.collect_garbage();
        }
    }
    assert!(engine.has_compiled_plan(), "every plan was accepted");
}

#[test]
fn a_plan_with_no_routing_needs_nothing_new_of_the_engine() {
    let p = Project::new_default();
    let plan = plan_of(&p);
    assert_eq!(plan.buses, 0);
    assert!(plan.midi.is_empty() && plan.hardware.is_empty() && plan.sidechains.is_empty());
    assert_eq!(plan.main_out, rlive_engine::OutputPair::default());
    assert_eq!(plan.cue, None);
    let _: Vec<&Step> = plan.steps.iter().collect();
}

fn with_compressor(p: &mut Project) -> (TrackId, TrackId, DeviceId) {
    let mut a = audio(p, "a");
    let mut b = audio(p, "b");
    a.routing.output = Route::SendsOnly;
    let compressor = Device::builtin("rlive.compressor", "Compressor");
    let id = compressor.id;
    b.devices.push(compressor);
    let (ia, ib) = (a.id, b.id);
    p.tracks = vec![a, b];
    (ia, ib, id)
}

#[test]
fn a_built_in_devices_chooser_routes_its_sidechain_and_undoes() {
    let mut p = Project::empty();
    let (a, b, device) = with_compressor(&mut p);
    let (mut engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    assert!(s.runtimes.get(&device).unwrap().sidechain_input);
    let sources = s.sidechain_sources(device);
    assert_eq!(sources.len(), 3, "the other track at each of its points");
    let source = SidechainSource {
        track: a,
        tap: Tap::PostFx,
    };
    assert!(s.set_sidechain(ChainRef::Track(b), device, Some(source)));
    s.commit_edits(false);
    let plan = compile_plan(&s.project);
    assert_eq!(plan.sidechains.len(), 1);
    assert_eq!(plan.sidechains[0].device, device);
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    s.idle();
    assert!(engine.has_compiled_plan(), "the engine took the keyed plan");
    assert!(s.undo());
    assert!(compile_plan(&s.project).sidechains.is_empty());
    assert_eq!(s.chain_of(device), Some(ChainRef::Track(b)));
    assert!(s.redo());
    assert_eq!(compile_plan(&s.project).sidechains.len(), 1);
}

#[test]
fn a_sidechain_that_would_close_a_loop_is_refused() {
    let mut p = Project::empty();
    let (a, b, device) = with_compressor(&mut p);
    p.tracks[0].routing.input = Route::Track {
        track: b,
        device: None,
    };
    let (_e, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    let source = SidechainSource {
        track: a,
        tap: Tap::PostMixer,
    };
    assert!(!s.set_sidechain(ChainRef::Track(b), device, Some(source)));
    assert_eq!(s.project.track(b).unwrap().devices[0].sidechain, None);
}

#[test]
fn a_plugins_extra_buses_are_wired_from_the_plan() {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let (mut a, mut b, mut c) = (audio(&p, "a"), audio(&p, "b"), audio(&p, "c"));
    let key = device("key", &mut rigged, Box::new(Scale(1.0)));
    let key_id = key.id;
    c.devices.push(key);
    let inst = device("inst", &mut rigged, Box::new(Scale(1.0)));
    let inst_id = inst.id;
    a.devices.push(inst);
    a.routing.output = Route::Track {
        track: c.id,
        device: Some(key_id),
    };
    b.routing.input = Route::Track {
        track: a.id,
        device: None,
    };
    b.routing.tap = Tap::Aux {
        device: inst_id,
        output: 0,
    };
    p.tracks = vec![a, b, c];
    let wiring = PluginWiring::of(&plan_of(&p));
    assert!(wiring.sidechain.contains(&key_id));
    assert!(wiring.aux.contains(&inst_id));
}

/// A Drum Rack whose pad plays 1.0 into a return chain: a 0.5 effect and a -6 dB fader.
fn drum_return_scene(point: ChainTap) -> (Project, Rigged) {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = midi(&p, "a");
    let mut b = audio(&p, "b");
    let mut rack = Device::rack(RackKind::Drum, "kit");
    let mut pad = Chain::new("pad");
    pad.devices
        .push(device("one", &mut rigged, Box::new(Dc(1.0))));
    let mut ret = Chain::new("return");
    ret.devices
        .push(device("half", &mut rigged, Box::new(Scale(0.5))));
    ret.mix.volume_db = -6.0206;
    pad.mix.sends.push(rlive_project::ChainSend {
        to: ret.id,
        level_db: 0.0,
    });
    let ret_id = ret.id;
    if let DeviceKind::Rack {
        chains, returns, ..
    } = &mut rack.kind
    {
        chains.push(pad);
        returns.push(ret);
    }
    a.devices.push(rack);
    a.routing.output = Route::SendsOnly;
    b.routing.input = Route::Track {
        track: a.id,
        device: None,
    };
    b.routing.tap = Tap::Chain {
        chain: ret_id,
        point,
    };
    b.routing.monitor = Monitor::In;
    p.tracks = vec![a, b];
    (p, rigged)
}

#[test]
fn a_drum_rack_return_chain_is_tapped_where_asked() {
    for (point, want) in [
        (ChainTap::PreFx, 1.0),
        (ChainTap::PostFx, 0.5),
        (ChainTap::PostMixer, 0.25),
    ] {
        let (p, mut rigged) = drum_return_scene(point);
        let mut r = rig(&p, &mut rigged);
        assert!(close(r.settled(), want), "{point:?}");
    }
}

#[test]
fn the_input_channel_chooser_offers_every_chain_and_return_chain() {
    let (p, _) = drum_return_scene(ChainTap::PreFx);
    let b = p.tracks[1].id;
    let (_e, handle) = create_engine(SR, BLOCK);
    let s = Session::new(handle, p, PluginCache::default());
    let labels: Vec<String> = s
        .input_channels(b, &io::AudioHardware::default())
        .into_iter()
        .map(|c| c.label)
        .collect();
    for point in ["Pre FX", "Post FX", "Post Mixer"] {
        assert!(
            labels.contains(&format!("kit | pad | {point}")),
            "{labels:?}"
        );
        assert!(
            labels.contains(&format!("kit | return | {point}")),
            "{labels:?}"
        );
    }
}

#[test]
fn soloing_a_track_that_taps_a_return_chain_still_hears_it() {
    let (mut p, mut rigged) = drum_return_scene(ChainTap::PostMixer);
    p.tracks[1].solo = true;
    let mut r = rig(&p, &mut rigged);
    assert!(close(r.settled(), 0.25));
}

#[test]
fn a_bypassed_rack_has_nothing_to_tap() {
    let (mut p, mut rigged) = drum_return_scene(ChainTap::PostFx);
    p.tracks[0].devices[0].bypass = true;
    let mut r = rig(&p, &mut rigged);
    assert_eq!(r.settled(), 0.0);
}

/// Track `a` clicks through a rack whose return chains go to Set return `r`, one per latency.
fn leaving_scene(latencies: &[u32], at: u64) -> (Project, Rigged, Vec<ChainId>) {
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    let ret = Track::new("r", TrackKind::Return, [1, 1, 1], 0);
    let mut rack = Device::rack(RackKind::Audio, "rack");
    let mut chain = Chain::new("c");
    chain.devices.push(device(
        "click",
        &mut rigged,
        Box::new(Click {
            at,
            pos: 0,
            level: 1.0,
        }),
    ));
    let mut ids = Vec::new();
    let mut leaving = Vec::new();
    for latency in latencies {
        let mut c = Chain::new("return");
        c.devices.push(rigged.latent("late", *latency));
        c.mix.output = Some(ret.id);
        chain.mix.sends.push(rlive_project::ChainSend {
            to: c.id,
            level_db: 0.0,
        });
        ids.push(c.id);
        leaving.push(c);
    }
    if let DeviceKind::Rack {
        chains, returns, ..
    } = &mut rack.kind
    {
        chains.push(chain);
        *returns = leaving;
    }
    a.devices.push(rack);
    a.routing.output = Route::SendsOnly;
    a.sends.push(rlive_project::TrackSend {
        to: ret.id,
        level_db: rlive_project::SEND_OFF_DB,
        enabled: false,
    });
    p.tracks = vec![a];
    p.returns = vec![ret];
    (p, rigged, ids)
}

#[test]
fn a_return_chain_going_to_a_set_return_is_read_through_a_tap_bus() {
    let (p, _, _) = leaving_scene(&[200], 8192);
    let plan = plan_of(&p);
    let at_return = plan.steps.iter().find(|s| s.track == 1).unwrap();
    assert_eq!(at_return.inputs.len(), 1);
    assert!(matches!(
        at_return.inputs[0].tap,
        rlive_engine::Tap::Aux(DeviceId(0), 0)
    ));
    let read = at_return.inputs[0].bus.expect("carried on a tap bus");
    assert!(!read.monitored, "a return track has nothing to monitor");
    assert!(plan
        .tap_outs
        .iter()
        .any(|t| t.bus == read.bus && t.track == 0));
}

#[test]
fn a_return_chain_going_to_a_set_return_meets_the_sends_at_the_sample() {
    let at = 8192;
    let (mut p, mut rigged, _) = leaving_scene(&[200], at);
    let ret = p.returns[0].id;
    let mut b = audio(&p, "b");
    b.devices.push(device(
        "click",
        &mut rigged,
        Box::new(Click {
            at,
            pos: 0,
            level: 1.0,
        }),
    ));
    b.routing.output = Route::SendsOnly;
    b.sends.push(rlive_project::TrackSend {
        to: ret,
        level_db: 0.0,
        enabled: true,
    });
    p.tracks.push(b);
    let mut r = rig(&p, &mut rigged);
    let at_return = r.plan.steps.iter().find(|s| s.track == 2).unwrap();
    let delay = |from: u32| {
        at_return
            .inputs
            .iter()
            .find(|i| i.from == from && i.tap != rlive_engine::Tap::PostFx)
            .map(|i| i.delay)
    };
    assert_eq!(delay(1), Some(200), "the plain send waits for the rack");
    assert_eq!(delay(0), Some(0), "the rack's output is the late one");
    let out = r.render(16_384);
    let hits: Vec<usize> = (0..out.len()).filter(|i| out[*i].abs() > 1e-6).collect();
    assert_eq!(hits, [at as usize + 200], "one click, both paths together");
    assert!(close(out[at as usize + 200], 2.0));
}

#[test]
fn return_chains_leaving_for_one_set_return_line_up_with_each_other() {
    let (p, mut rigged, ids) = leaving_scene(&[200, 50], 8192);
    let timing = Timing::measure(&p, SR, &rigged.fake);
    let delay = |id: ChainId| timing.chains.iter().find(|c| c.chain == id).unwrap().delay;
    assert_eq!(delay(ids[0]), 0);
    assert_eq!(delay(ids[1]), 150);
    let at = 8192;
    let mut r = rig(&p, &mut rigged);
    let out = r.render(16_384);
    let hits: Vec<usize> = (0..out.len()).filter(|i| out[*i].abs() > 1e-6).collect();
    assert_eq!(hits, [at as usize + 200], "both as late as the slowest");
    assert!(close(out[at as usize + 200], 2.0));
}

#[test]
fn a_chain_tap_is_compensated_at_its_own_point_in_the_rack() {
    let at = 8192;
    let mut rigged = Rigged::default();
    let mut p = Project::empty();
    let mut a = audio(&p, "a");
    let mut b = audio(&p, "b");
    let mut c = audio(&p, "c");
    a.devices.push(device(
        "click",
        &mut rigged,
        Box::new(Click {
            at,
            pos: 0,
            level: 1.0,
        }),
    ));
    a.devices.push(rigged.latent("before", 100));
    let mut rack = Device::rack(RackKind::Audio, "rack");
    let mut slow = Chain::new("slow");
    slow.devices.push(rigged.latent("slow", 300));
    let mut quick = Chain::new("quick");
    quick.devices.push(rigged.latent("quick", 40));
    let quick_id = quick.id;
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(slow);
        chains.push(quick);
    }
    a.devices.push(rack);
    a.routing.output = Route::SendsOnly;
    b.routing.input = Route::Track {
        track: a.id,
        device: None,
    };
    b.routing.tap = Tap::Chain {
        chain: quick_id,
        point: ChainTap::PostFx,
    };
    b.routing.monitor = Monitor::In;
    c.devices.push(device(
        "click",
        &mut rigged,
        Box::new(Click {
            at,
            pos: 0,
            level: 1.0,
        }),
    ));
    p.tracks = vec![a, b, c];
    let mut r = rig(&p, &mut rigged);
    let out = r.render(16_384);
    // Post FX of the quick chain has been held back to meet the slow one: 100 + 300 late.
    // A later rendering track's click and the tap come out together, so nothing leads.
    let hits: Vec<usize> = (0..out.len()).filter(|i| out[*i].abs() > 1e-6).collect();
    assert_eq!(
        hits,
        [at as usize + 400],
        "the tap and the plain track, as one"
    );
    assert!(close(out[at as usize + 400], 2.0));
}
