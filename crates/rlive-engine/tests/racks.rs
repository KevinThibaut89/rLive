//! The rack runtime end to end: zones, fades, the chain mixer, Drum Rack pads, choke groups and
//! returns, and Macros, on sample-accurate synthetic instruments.
#![allow(clippy::vec_box)]

use rlive_core::param::{chain as controls, rack as rack_params};
use rlive_core::{
    BlockStep, ChainId, DeviceId, MidiEvent, MidiMessage, ParamInfo, ProcessContext, Processor,
    TrackId,
};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn note_on(key: u8, velocity: f32) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity,
        note_id: 0,
        tuning: 0.0,
    }
}

fn note_off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: 0,
    }
}

/// Emits MIDI on a schedule, in frames since it started.
struct Seq {
    pos: u64,
    notes: Vec<(u64, MidiMessage)>,
}

impl Processor for Seq {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = ctx.audio.left.len() as u64;
        for (at, message) in &self.notes {
            if *at >= self.pos && *at < self.pos + n {
                ctx.events_out
                    .push(MidiEvent::new((*at - self.pos) as u32, *message));
            }
        }
        self.pos += n;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn seq(notes: Vec<(u64, MidiMessage)>) -> Box<DeviceSlot> {
    let mut slot = DeviceSlot::new(DeviceId::new(), Box::new(Seq { pos: 0, notes }));
    slot.midi_effect = true;
    Box::new(slot)
}

/// Logged by [`Tone`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct Heard {
    frame: u64,
    key: u8,
    velocity: f32,
    on: bool,
}

/// A held note sounds at its velocity times `level`, from the sample it starts until its note-off
/// or a reset.
struct Tone {
    level: f32,
    frame: u64,
    voices: [f32; 128],
    log: Arc<Mutex<Vec<Heard>>>,
}

fn tone(level: f32) -> (Box<DeviceSlot>, Arc<Mutex<Vec<Heard>>>) {
    let log = Arc::new(Mutex::new(Vec::with_capacity(1024)));
    // The first lock of a mutex can allocate on some platforms, so it happens here.
    drop(log.lock().unwrap());
    let t = Tone {
        level,
        frame: 0,
        voices: [0.0; 128],
        log: log.clone(),
    };
    (Box::new(DeviceSlot::new(DeviceId::new(), Box::new(t))), log)
}

impl Tone {
    fn note(&mut self, heard: Heard) {
        let mut log = self.log.lock().unwrap();
        if log.len() < log.capacity() {
            log.push(heard);
        }
    }
}

impl Processor for Tone {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = ctx.audio.left.len();
        let mut next = 0;
        for i in 0..n {
            while let Some(e) = ctx.events.get(next).filter(|e| e.offset as usize == i) {
                next += 1;
                match e.message {
                    MidiMessage::NoteOn { key, velocity, .. } => {
                        self.voices[key as usize] = velocity;
                        self.note(Heard {
                            frame: self.frame + i as u64,
                            key,
                            velocity,
                            on: true,
                        });
                    }
                    MidiMessage::NoteOff { key, .. } => {
                        self.voices[key as usize] = 0.0;
                        self.note(Heard {
                            frame: self.frame + i as u64,
                            key,
                            velocity: 0.0,
                            on: false,
                        });
                    }
                    _ => {}
                }
            }
            let level: f32 = self.voices.iter().sum::<f32>() * self.level;
            ctx.audio.left[i] += level;
            ctx.audio.right[i] += level;
        }
        self.frame += n as u64;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
    fn reset(&mut self) {
        self.voices = [0.0; 128];
    }
}

/// `out = in * gain + add`, the arithmetic an audio-rack test can predict.
struct Fx {
    gain: f32,
    add: f32,
}

impl Processor for Fx {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for s in ctx.audio.left.iter_mut().chain(ctx.audio.right.iter_mut()) {
            *s = *s * self.gain + self.add;
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn fx(gain: f32, add: f32) -> Box<DeviceSlot> {
    Box::new(DeviceSlot::new(DeviceId::new(), Box::new(Fx { gain, add })))
}

/// A knob whose value the buffer holds, so what reaches a device's parameter can be heard.
struct Knob {
    value: f32,
}

impl Processor for Knob {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id: 0, value } => self.value = value as f32,
            BlockStep::Audio(audio) => {
                audio.left.fill(self.value);
                audio.right.fill(self.value);
            }
            _ => {}
        });
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn chain_of(devices: Vec<Box<DeviceSlot>>) -> Box<ChainNode> {
    let mut chain = ChainNode::new(ChainId::new(), BLOCK);
    chain.devices.extend(devices);
    Box::new(chain)
}

fn rack_of(kind: RackKind, chains: Vec<Box<ChainNode>>) -> Box<RackNode> {
    let mut rack = Box::new(RackNode::new(kind));
    rack.chains.extend(chains);
    rack
}

fn add_track(h: &mut EngineHandle, track: Box<TrackNode>) {
    assert!(h
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
}

fn track_of(id: TrackId, devices: Vec<Box<DeviceSlot>>) -> Box<TrackNode> {
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.extend(devices);
    track
}

fn zones() -> Zones {
    Zones::default()
}

fn render(engine: &mut Engine, frames: usize) -> (Vec<f32>, Vec<f32>) {
    rlive_testkit::render_chunks(frames, BLOCK, |l, r| engine.process_stereo(l, r))
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

fn fade(fade_low: u8, low: u8, high: u8, fade_high: u8) -> Zone {
    Zone {
        low,
        high,
        fade_low,
        fade_high,
    }
}

/// A rack with some tones and the key zones given, played by a sequence on the same track.
fn zone_rig(
    kind: RackKind,
    chains: Vec<(Box<DeviceSlot>, Zones)>,
    notes: Vec<(u64, MidiMessage)>,
) -> (Engine, EngineHandle, TrackId) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let nodes = chains
        .into_iter()
        .map(|(d, zones)| {
            let mut c = chain_of(vec![d]);
            c.zones = zones;
            c
        })
        .collect();
    let rack = rack_of(kind, nodes);
    let t = TrackId::new();
    add_track(
        &mut h,
        track_of(
            t,
            vec![
                seq(notes),
                Box::new(DeviceSlot::rack(DeviceId::new(), rack)),
            ],
        ),
    );
    (engine, h, t)
}

fn heard(log: &Arc<Mutex<Vec<Heard>>>) -> Vec<Heard> {
    log.lock().unwrap().clone()
}

// ------------------------------------------------------------------ zones

#[test]
fn key_zones_split_the_keyboard_between_chains() {
    let (low, low_log) = tone(1.0);
    let (high, high_log) = tone(1.0);
    let mut a = zones();
    a.key = Zone::span(0, 59);
    let mut b = zones();
    b.key = Zone::span(60, 127);
    let notes = vec![(0, note_on(40, 0.5)), (0, note_on(72, 0.5))];
    let (mut engine, _h, _) = zone_rig(RackKind::Instrument, vec![(low, a), (high, b)], notes);
    render(&mut engine, BLOCK);
    assert_eq!(
        heard(&low_log).iter().map(|h| h.key).collect::<Vec<_>>(),
        [40]
    );
    assert_eq!(
        heard(&high_log).iter().map(|h| h.key).collect::<Vec<_>>(),
        [72]
    );
}

#[test]
fn a_notes_velocity_scales_through_a_key_fade() {
    let (t, log) = tone(1.0);
    let mut z = zones();
    z.key = fade(48, 60, 127, 127);
    let notes = vec![
        (0, note_on(54, 0.8)),
        (0, note_on(60, 0.8)),
        (0, note_on(48, 0.8)),
    ];
    let (mut engine, _h, _) = zone_rig(RackKind::Instrument, vec![(t, z)], notes);
    render(&mut engine, BLOCK);
    let mut got = heard(&log);
    got.sort_by_key(|h| h.key);
    // The far edge of the fade is silent: that key never gets in.
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].key, 54);
    assert!(close(got[0].velocity, 0.8 * 0.5), "{}", got[0].velocity);
    assert_eq!(got[1].velocity, 0.8);
}

#[test]
fn velocity_zones_attenuate_correctly() {
    let (t, log) = tone(1.0);
    let mut z = zones();
    z.velocity = fade(40, 80, 127, 127);
    // Velocity 60 sits halfway up the fade, 20 is outside the zone.
    let v60 = 60.0 / 127.0;
    let notes = vec![
        (0, note_on(60, v60)),
        (0, note_on(61, 100.0 / 127.0)),
        (0, note_on(62, 20.0 / 127.0)),
    ];
    let (mut engine, _h, _) = zone_rig(RackKind::Instrument, vec![(t, z)], notes);
    render(&mut engine, BLOCK);
    let got = heard(&log);
    assert_eq!(got.len(), 2, "{got:?}");
    let a = got.iter().find(|h| h.key == 60).unwrap();
    assert!(close(a.velocity, v60 * 0.5), "{}", a.velocity);
    let b = got.iter().find(|h| h.key == 61).unwrap();
    assert!(
        close(b.velocity, 100.0 / 127.0),
        "untouched inside the zone"
    );
}

/// A rack whose Chain Selector this test can move, set up for `params` chains.
struct SelectRig {
    engine: Engine,
    h: EngineHandle,
    track: TrackId,
    selector: CellIx,
}

fn select_rig(
    kind: RackKind,
    chains: Vec<(Vec<Box<DeviceSlot>>, Zones)>,
    source: Box<DeviceSlot>,
) -> SelectRig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack_id = DeviceId::new();
    let mut nodes = Vec::new();
    for (devices, zones) in chains {
        let mut c = chain_of(devices);
        c.zones = zones;
        nodes.push(c);
    }
    let rack = rack_of(kind, nodes);
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &rack_params::infos(), |p| p.default);
    let selector = layout
        .device_cell(rack_id, rack_params::CHAIN_SELECT.0)
        .unwrap();
    let mut node = track_of(
        track,
        vec![source, Box::new(DeviceSlot::rack(rack_id, rack))],
    );
    node.bus = layout.build();
    add_track(&mut h, node);
    SelectRig {
        engine,
        h,
        track,
        selector,
    }
}

impl SelectRig {
    fn select(&mut self, value: f64) {
        assert!(self
            .h
            .send(EngineCommand::Param(ParamCmd::Set {
                track: TrackRef::Track(self.track),
                cell: self.selector,
                value,
                gesture: false,
            }))
            .is_ok());
    }
}

/// Steady audio into the rack, from before it.
fn dc() -> Box<DeviceSlot> {
    fx(0.0, 1.0)
}

/// Half a second, long enough for any gain smoothing to finish.
const SETTLE: usize = 24_000;

#[test]
fn chain_select_decides_which_chains_are_heard() {
    let a = fx(1.0, 0.0);
    let b = fx(0.5, 0.0);
    let mut za = zones();
    za.select = Zone::span(0, 0);
    let mut zb = zones();
    zb.select = Zone::span(1, 1);
    // Chains with no signal of their own: only the one the selector picks passes the input.
    let mut r = select_rig(RackKind::Audio, vec![(vec![a], za), (vec![b], zb)], dc());
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 1.0),
        "only chain a: {}",
        l.last().unwrap()
    );
    r.select(1.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 0.5),
        "only chain b: {}",
        l.last().unwrap()
    );
}

#[test]
fn without_a_fade_an_audio_chain_keeps_its_tail_while_with_one_it_goes_silent() {
    // Each chain makes 0.2 of its own, standing for what's ringing in it, and doubles the input.
    let tail = || fx(2.0, 0.2);
    // Selected at 0.
    let mut no_fade = zones();
    no_fade.select = Zone::span(0, 0);
    let mut faded = zones();
    faded.select = fade(0, 0, 0, 8);
    let mut r = select_rig(
        RackKind::Audio,
        vec![(vec![tail()], no_fade), (vec![tail()], faded)],
        dc(),
    );
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(close(*l.last().unwrap(), 2.2 + 2.2), "both in the zone");
    r.select(40.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 0.2),
        "input cut off, tail through, faded chain silent: {}",
        l.last().unwrap()
    );
}

#[test]
fn chain_select_fades_crossfade_output_in_an_audio_rack() {
    let a = fx(1.0, 0.0);
    let b = fx(1.0, 0.0);
    let mut za = zones();
    za.select = fade(0, 0, 0, 8);
    let mut zb = zones();
    zb.select = fade(0, 8, 8, 16);
    let mut r = select_rig(RackKind::Audio, vec![(vec![a], za), (vec![b], zb)], dc());
    r.select(4.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 1.0),
        "halfway: {}",
        l.last().unwrap()
    );
    r.select(0.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(close(*l.last().unwrap(), 1.0), "all a");
}

#[test]
fn instrument_rack_chain_select_filters_notes_and_attenuates_output_only() {
    let (t, log) = tone(1.0);
    let mut z = zones();
    z.select = fade(10, 20, 20, 20);
    // No selector yet: the note comes in at the default 0, outside the zone.
    let mut r = select_rig(
        RackKind::Instrument,
        vec![(vec![t], z)],
        seq(vec![
            (0, note_on(60, 0.8)),
            ((SETTLE * 2) as u64, note_on(61, 0.8)),
        ]),
    );
    render(&mut r.engine, SETTLE);
    assert!(heard(&log).is_empty(), "outside the zone: filtered");
    r.select(15.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        heard(&log).is_empty(),
        "the selector moved after the note started"
    );
    assert!(close(*l.last().unwrap(), 0.0), "nothing playing");
    render(&mut r.engine, SETTLE);
    let (l, _) = render(&mut r.engine, SETTLE);
    let got = heard(&log);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].velocity, 0.8, "not touched by a Chain Select fade");
    assert!(
        close(*l.last().unwrap(), 0.8 * 0.5),
        "halfway up the fade: {}",
        l.last().unwrap()
    );
}

#[test]
fn midi_rack_chain_select_fades_attenuate_velocity() {
    let (t, log) = tone(1.0);
    let mut z = zones();
    z.select = fade(10, 20, 20, 20);
    // A MIDI Effect Rack passes notes on: the tone sits after it on the track.
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack_id = DeviceId::new();
    let mut c = chain_of(Vec::new());
    c.zones = z;
    let rack = rack_of(RackKind::Midi, vec![c]);
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &rack_params::infos(), |p| p.default);
    let selector = layout
        .device_cell(rack_id, rack_params::CHAIN_SELECT.0)
        .unwrap();
    let mut node = track_of(
        track,
        vec![
            seq(vec![(SETTLE as u64, note_on(60, 0.8))]),
            Box::new(DeviceSlot::rack(rack_id, rack)),
            t,
        ],
    );
    node.bus = layout.build();
    add_track(&mut h, node);
    let mut engine = engine;
    assert!(h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(track),
            cell: selector,
            value: 15.0,
            gesture: false,
        }))
        .is_ok());
    render(&mut engine, SETTLE + BLOCK);
    let got = heard(&log);
    assert_eq!(got.len(), 1);
    assert!(close(got[0].velocity, 0.4), "{}", got[0].velocity);
}

#[test]
fn notes_stopped_by_a_zone_edit_still_get_their_note_off() {
    let (t, log) = tone(1.0);
    let z = zones();
    let id = ChainId::new();
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack_id = DeviceId::new();
    let mut chain = ChainNode::new(id, BLOCK);
    chain.devices.push(t);
    chain.zones = z;
    let rack = rack_of(RackKind::Instrument, vec![Box::new(chain)]);
    let node = track_of(
        track,
        vec![
            seq(vec![(0, note_on(60, 0.8)), (2000, note_off(60))]),
            Box::new(DeviceSlot::rack(rack_id, rack)),
        ],
    );
    add_track(&mut h, node);
    let mut engine = engine;
    render(&mut engine, 1024);
    let mut narrow = zones();
    narrow.key = Zone::span(0, 40);
    assert!(h
        .send(EngineCommand::graph(GraphOp::SetChainZones {
            target: ChainAddr::track(track),
            device: rack_id,
            chain: id,
            zones: narrow,
        }))
        .is_ok());
    render(&mut engine, 2048);
    let got = heard(&log);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        !got[1].on,
        "the note ended even though it is now out of the zone"
    );
}

#[test]
fn chain_select_filters_cc_only_with_the_option_on() {
    let run = |filters: bool| {
        // A probe counting control messages.
        struct Count {
            count: Arc<Mutex<usize>>,
        }
        impl Processor for Count {
            fn activate(&mut self, _: f64, _: usize) {}
            fn process(&mut self, ctx: &mut ProcessContext) {
                *self.count.lock().unwrap() += ctx
                    .events
                    .iter()
                    .filter(|e| matches!(e.message, MidiMessage::ControlChange { .. }))
                    .count();
            }
            fn set_param(&mut self, _: u32, _: f64) {}
        }
        let count = Arc::new(Mutex::new(0));
        let mut z = zones();
        z.select = Zone::span(40, 40);
        let (engine, mut h) = create_engine(SR, BLOCK);
        let track = TrackId::new();
        let mut c = chain_of(vec![Box::new(DeviceSlot::new(
            DeviceId::new(),
            Box::new(Count {
                count: count.clone(),
            }),
        ))]);
        c.zones = z;
        let mut rack = RackNode::new(RackKind::Instrument);
        rack.chains.push(c);
        rack.select_filters_ctrl = filters;
        let cc = MidiMessage::ControlChange {
            channel: 0,
            cc: 1,
            value: 1,
        };
        add_track(
            &mut h,
            track_of(
                track,
                vec![
                    seq(vec![(0, cc)]),
                    Box::new(DeviceSlot::rack(DeviceId::new(), Box::new(rack))),
                ],
            ),
        );
        let mut engine = engine;
        render(&mut engine, BLOCK);
        let n = *count.lock().unwrap();
        n
    };
    assert_eq!(run(false), 1, "control messages pass by default");
    assert_eq!(run(true), 0);
}

// ------------------------------------------------------------------ notes across switches

/// A MIDI Effect Rack with one empty chain and a tone after it on the track, with the rack's
/// on/off switch and the chain's activator in reach.
struct SwitchRig {
    engine: Engine,
    h: EngineHandle,
    track: TrackId,
    rack_on: CellIx,
    chain_on: CellIx,
    log: Arc<Mutex<Vec<Heard>>>,
}

fn switch_rig(notes: Vec<(u64, MidiMessage)>, zones: Zones) -> SwitchRig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack_id = DeviceId::new();
    let chain = ChainId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &rack_params::infos(), |p| p.default);
    layout.place_chain(chain, &default_strip());
    let mut c = ChainNode::new(chain, BLOCK);
    c.zones = zones;
    let rack = rack_of(RackKind::Midi, vec![Box::new(c)]);
    let (t, log) = tone(1.0);
    let mut node = track_of(
        track,
        vec![seq(notes), Box::new(DeviceSlot::rack(rack_id, rack)), t],
    );
    node.bus = layout.build();
    add_track(&mut h, node);
    SwitchRig {
        engine,
        h,
        track,
        rack_on: layout.activator_cell(rack_id).unwrap(),
        chain_on: layout.chain_cell(chain, controls::ACTIVATOR).unwrap(),
        log,
    }
}

impl SwitchRig {
    fn set(&mut self, cell: CellIx, on: bool) {
        assert!(self
            .h
            .send(EngineCommand::Param(ParamCmd::Set {
                track: TrackRef::Track(self.track),
                cell,
                value: on as u8 as f64,
                gesture: true,
            }))
            .is_ok());
    }

    fn blocks(&mut self, n: usize) {
        for _ in 0..n {
            let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
            assert_no_alloc(|| self.engine.process_stereo(&mut l, &mut r));
        }
    }

    fn ons_and_offs(&self) -> (usize, usize) {
        let log = heard(&self.log);
        let ons = log.iter().filter(|h| h.on).count();
        (ons, log.len() - ons)
    }
}

const B: u64 = BLOCK as u64;

#[test]
fn a_note_started_while_its_rack_is_bypassed_still_ends_after_the_rack_returns() {
    let mut r = switch_rig(
        vec![(2 * B, note_on(60, 0.8)), (6 * B, note_off(60))],
        zones(),
    );
    r.set(r.rack_on, false);
    r.blocks(4);
    assert_eq!(r.ons_and_offs(), (1, 0), "it played straight through");
    r.set(r.rack_on, true);
    r.blocks(4);
    assert_eq!(
        r.ons_and_offs(),
        (1, 1),
        "the rack let its note-off through"
    );
}

#[test]
fn a_note_held_across_a_rack_bypass_still_ends() {
    let mut r = switch_rig(vec![(0, note_on(60, 0.8)), (6 * B, note_off(60))], zones());
    r.blocks(2);
    r.set(r.rack_on, false);
    r.blocks(2);
    r.set(r.rack_on, true);
    r.blocks(4);
    assert_eq!(r.ons_and_offs(), (1, 1));
}

#[test]
fn switching_a_chain_off_releases_what_it_had_sounding_downstream() {
    let mut r = switch_rig(vec![(0, note_on(60, 0.8)), (6 * B, note_off(60))], zones());
    r.blocks(2);
    assert_eq!(r.ons_and_offs(), (1, 0));
    r.set(r.chain_on, false);
    r.blocks(1);
    assert_eq!(
        r.ons_and_offs(),
        (1, 1),
        "released at the switch, not at key up"
    );
    r.blocks(5);
    assert_eq!(
        r.ons_and_offs(),
        (1, 1),
        "and the key coming up adds nothing"
    );
}

#[test]
fn a_chain_that_is_off_starts_no_notes_and_a_refused_note_gets_no_note_off() {
    let mut narrow = zones();
    narrow.key = Zone::span(0, 40);
    let mut r = switch_rig(vec![(0, note_on(60, 0.8)), (B, note_off(60))], narrow);
    r.blocks(4);
    assert_eq!(
        r.ons_and_offs(),
        (0, 0),
        "outside the zone, both ends of it"
    );

    let mut r = switch_rig(
        vec![(2 * B, note_on(60, 0.8)), (3 * B, note_off(60))],
        zones(),
    );
    r.set(r.chain_on, false);
    r.blocks(6);
    assert_eq!(r.ons_and_offs(), (0, 0));
}

#[test]
fn switching_an_instrument_chain_off_releases_its_instrument() {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack_id = DeviceId::new();
    let chain = ChainId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &rack_params::infos(), |p| p.default);
    layout.place_chain(chain, &default_strip());
    let (t, log) = tone(1.0);
    let mut c = ChainNode::new(chain, BLOCK);
    c.devices.push(t);
    let rack = rack_of(RackKind::Instrument, vec![Box::new(c)]);
    let mut node = track_of(
        track,
        vec![
            seq(vec![(0, note_on(60, 0.8))]),
            Box::new(DeviceSlot::rack(rack_id, rack)),
        ],
    );
    node.bus = layout.build();
    add_track(&mut h, node);
    let mut engine = engine;
    render(&mut engine, BLOCK * 2);
    let cell = layout.chain_cell(chain, controls::ACTIVATOR).unwrap();
    assert!(h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(track),
            cell,
            value: 0.0,
            gesture: true,
        }))
        .is_ok());
    render(&mut engine, BLOCK);
    let got = heard(&log);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(!got[1].on, "the instrument was told to let go");
}

// ------------------------------------------------------------------ chain mixer

/// A rack of arithmetic chains with cells placed in the track's bus, for driving its strips.
struct MixRig {
    engine: Engine,
    h: EngineHandle,
    track: TrackId,
    layout: BusLayout,
    chains: Vec<ChainId>,
}

fn mix_rig(kind: RackKind, adds: &[f32]) -> MixRig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack_id = DeviceId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &rack_params::infos(), |p| p.default);
    let mut chains = Vec::new();
    let mut nodes = Vec::new();
    for add in adds {
        let id = ChainId::new();
        let mut node = ChainNode::new(id, BLOCK);
        node.devices.push(fx(0.0, *add));
        layout.place_chain(id, &default_strip());
        chains.push(id);
        nodes.push(Box::new(node));
    }
    let rack = rack_of(kind, nodes);
    let mut node = track_of(track, vec![Box::new(DeviceSlot::rack(rack_id, rack))]);
    node.bus = layout.build();
    add_track(&mut h, node);
    MixRig {
        engine,
        h,
        track,
        layout,
        chains,
    }
}

fn default_strip() -> [f64; controls::CONTROLS] {
    let mut v = [0.0; controls::CONTROLS];
    v[2] = 1.0;
    for s in v.iter_mut().skip(4) {
        *s = SEND_MIN_DB;
    }
    v
}

impl MixRig {
    fn set(&mut self, chain: usize, control: rlive_core::ParamId, value: f64) {
        let cell = self.layout.chain_cell(self.chains[chain], control).unwrap();
        assert!(self
            .h
            .send(EngineCommand::Param(ParamCmd::Set {
                track: TrackRef::Track(self.track),
                cell,
                value,
                gesture: true,
            }))
            .is_ok());
    }
}

#[test]
fn the_chain_mixer_applies_volume_pan_and_switches() {
    let mut r = mix_rig(RackKind::Audio, &[0.25, 0.5]);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(close(*l.last().unwrap(), 0.75), "{}", l.last().unwrap());

    r.set(0, controls::VOLUME, -6.0206);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 0.125 + 0.5),
        "{}",
        l.last().unwrap()
    );

    r.set(1, controls::PAN, -1.0);
    let (l, right) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 0.125 + 0.5),
        "hard left: left unity gain (compensated law)"
    );
    assert!(close(*right.last().unwrap(), 0.125));

    r.set(1, controls::PAN, 0.0);
    r.set(1, controls::ACTIVATOR, 0.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(close(*l.last().unwrap(), 0.125), "b off");

    r.set(1, controls::ACTIVATOR, 1.0);
    r.set(0, controls::SOLO, 1.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(close(*l.last().unwrap(), 0.125), "only the soloed chain");
    r.set(1, controls::SOLO, 1.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(close(*l.last().unwrap(), 0.625), "both soloed");
    r.set(0, controls::SOLO, 0.0);
    r.set(1, controls::SOLO, 0.0);
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(close(*l.last().unwrap(), 0.625));
}

// ------------------------------------------------------------------ drum racks

fn pad(receive: Option<u8>, play: u8, choke: Option<u8>) -> Zones {
    let mut z = zones();
    z.drum = Some(DrumPad {
        receive,
        play,
        choke,
    });
    z
}

#[test]
fn drum_chains_answer_their_receive_note_and_play_their_play_note() {
    let (a, a_log) = tone(1.0);
    let (b, b_log) = tone(1.0);
    let notes = vec![
        (0, note_on(36, 0.5)),
        (0, note_on(38, 0.5)),
        (0, note_on(40, 0.5)),
    ];
    let (mut engine, _h, _) = zone_rig(
        RackKind::Drum,
        vec![(a, pad(Some(36), 60, None)), (b, pad(Some(38), 72, None))],
        notes,
    );
    render(&mut engine, BLOCK);
    let got = heard(&a_log);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, 60, "receives 36, plays 60");
    let got = heard(&b_log);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, 72);
}

#[test]
fn all_notes_passes_every_note_through_unchanged() {
    let (a, log) = tone(1.0);
    let notes = vec![(0, note_on(50, 0.5)), (0, note_on(100, 0.5))];
    let (mut engine, _h, _) = zone_rig(RackKind::Drum, vec![(a, pad(None, 0, Some(1)))], notes);
    render(&mut engine, BLOCK);
    let mut keys: Vec<u8> = heard(&log).iter().map(|h| h.key).collect();
    keys.sort_unstable();
    assert_eq!(keys, [50, 100], "Play is ignored for All Notes");
}

#[test]
fn a_shared_receive_note_triggers_every_chain_on_it_like_a_multi_pad() {
    let (a, a_log) = tone(1.0);
    let (b, b_log) = tone(1.0);
    let (c, c_log) = tone(1.0);
    let notes = vec![(0, note_on(38, 0.5))];
    let (mut engine, _h, _) = zone_rig(
        RackKind::Drum,
        vec![
            (a, pad(Some(38), 60, Some(2))),
            (b, pad(Some(38), 61, Some(2))),
            (c, pad(Some(39), 62, Some(2))),
        ],
        notes,
    );
    let (l, _) = render(&mut engine, BLOCK);
    assert_eq!(heard(&a_log).len(), 1);
    assert_eq!(
        heard(&b_log).len(),
        1,
        "both chains on the note fire together"
    );
    assert!(heard(&c_log).is_empty());
    assert!(close(*l.last().unwrap(), 1.0), "and don't choke each other");
}

#[test]
fn choke_groups_cut_at_the_right_sample() {
    let (open, open_log) = tone(1.0);
    let (closed, _) = tone(0.25);
    let (other, _) = tone(0.125);
    let cut_at: u64 = (BLOCK + 100) as u64;
    let notes = vec![
        (10, note_on(46, 1.0)),
        (10, note_on(50, 1.0)),
        (cut_at, note_on(42, 1.0)),
    ];
    let (mut engine, _h, _) = zone_rig(
        RackKind::Drum,
        vec![
            (open, pad(Some(46), 46, Some(1))),
            (closed, pad(Some(42), 42, Some(1))),
            (other, pad(Some(50), 50, None)),
        ],
        notes,
    );
    let (l, _) = render(&mut engine, BLOCK * 3);
    assert_eq!(l[9], 0.0);
    assert!(
        close(l[10], 1.0 + 0.125),
        "open hat and the other drum start: {}",
        l[10]
    );
    assert!(close(l[cut_at as usize - 1], 1.125), "still ringing");
    assert!(
        close(l[cut_at as usize], 0.25 + 0.125),
        "cut on the sample the closed hat starts: {}",
        l[cut_at as usize]
    );
    let got = heard(&open_log);
    assert_eq!(
        got.len(),
        1,
        "the choked voice doesn't get a stale note-off"
    );
}

#[test]
fn a_chain_retriggered_in_the_same_block_as_its_choke_rings_again() {
    let (open, _) = tone(1.0);
    let (closed, _) = tone(0.25);
    let notes = vec![
        (0, note_on(46, 1.0)),
        (100, note_on(42, 1.0)),
        (200, note_on(46, 1.0)),
    ];
    let (mut engine, _h, _) = zone_rig(
        RackKind::Drum,
        vec![
            (open, pad(Some(46), 46, Some(1))),
            (closed, pad(Some(42), 42, Some(1))),
        ],
        notes,
    );
    let (l, _) = render(&mut engine, BLOCK);
    assert!(close(l[50], 1.0), "open hat");
    assert!(close(l[150], 0.25), "cut, closed hat alone");
    assert!(close(l[250], 1.0), "the open hat cuts the closed one back");
}

#[test]
fn all_notes_ignores_choke() {
    let (all, all_log) = tone(1.0);
    let (hit, _) = tone(0.25);
    let notes = vec![(0, note_on(46, 1.0)), (50, note_on(42, 1.0))];
    let (mut engine, _h, _) = zone_rig(
        RackKind::Drum,
        vec![
            (all, pad(None, 0, Some(1))),
            (hit, pad(Some(42), 42, Some(1))),
        ],
        notes,
    );
    let (l, _) = render(&mut engine, BLOCK);
    assert!(close(l[60], 1.0 + 0.25 + 1.0), "nothing choked: {}", l[60]);
    assert_eq!(heard(&all_log).len(), 2, "it answers every note");
}

/// A drum chain with a tone on it, and a return chain, cells placed.
struct DrumRig {
    engine: Engine,
    h: EngineHandle,
    track: TrackId,
    layout: BusLayout,
    chain: ChainId,
}

fn drum_rig(return_gain: f32, seed_send_db: Option<f64>) -> DrumRig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack_id = DeviceId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &rack_params::infos(), |p| p.default);
    let (t, _) = tone(1.0);
    let chain = ChainId::new();
    let mut node = ChainNode::new(chain, BLOCK);
    node.devices.push(t);
    node.zones = pad(Some(36), 60, None);
    let mut strip = default_strip();
    if let Some(db) = seed_send_db {
        strip[4] = db;
    }
    layout.place_chain(chain, &strip);
    let mut mix = ChainMix::default();
    if let Some(db) = seed_send_db {
        mix.sends[0] = db as f32;
    }
    node.seed_mix(mix);
    let ret_id = ChainId::new();
    let mut ret = ChainNode::new(ret_id, BLOCK);
    ret.devices.push(fx(return_gain, 0.0));
    layout.place_chain(ret_id, &default_strip());
    let mut rack = RackNode::new(RackKind::Drum);
    rack.chains.push(Box::new(node));
    rack.returns.push(Box::new(ret));
    let mut node = track_of(
        track,
        vec![
            seq(vec![(0, note_on(36, 1.0))]),
            Box::new(DeviceSlot::rack(rack_id, Box::new(rack))),
        ],
    );
    node.bus = layout.build();
    add_track(&mut h, node);
    DrumRig {
        engine,
        h,
        track,
        layout,
        chain,
    }
}

#[test]
fn drum_returns_are_fed_post_fader_and_mix_into_the_rack() {
    // The chain carries 1.0, its send sends half of that, the return halves it again.
    let mut r = drum_rig(0.5, Some(-6.0206));
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 1.0 + 0.5 * 0.5),
        "dry plus return: {}",
        l.last().unwrap()
    );
    // Post-fader: halving the chain halves what the return gets too.
    let cell = r.layout.chain_cell(r.chain, controls::VOLUME).unwrap();
    assert!(r
        .h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(r.track),
            cell,
            value: -6.0206,
            gesture: true,
        }))
        .is_ok());
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 0.5 + 0.25 * 0.5),
        "{}",
        l.last().unwrap()
    );
}

#[test]
fn soloing_a_drum_chain_leaves_returns_on_but_mutes_others_sends() {
    let mut r = drum_rig(0.5, Some(0.0));
    // A silent chain soloed: the drum chain goes quiet, sends included, but the return itself
    // stays open.
    let solo = r.layout.chain_cell(r.chain, controls::SOLO).unwrap();
    assert!(r
        .h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(r.track),
            cell: solo,
            value: 1.0,
            gesture: true,
        }))
        .is_ok());
    let (l, _) = render(&mut r.engine, SETTLE);
    assert!(
        close(*l.last().unwrap(), 1.0 + 0.5),
        "the only chain, soloed, is heard along with its return: {}",
        l.last().unwrap()
    );
}

#[test]
fn a_return_chain_can_feed_a_return_track_instead_of_the_rack() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let target = TrackId::new();
    let rack_id = DeviceId::new();
    let ret_id = ChainId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &rack_params::infos(), |p| p.default);
    let mut drum = ChainNode::new(ChainId::new(), BLOCK);
    let (t, _) = tone(1.0);
    drum.devices.push(t);
    drum.zones = pad(Some(36), 60, None);
    layout.place_chain(drum.id, &default_strip());
    let mut mix = ChainMix::default();
    mix.sends[0] = 0.0;
    drum.seed_mix(mix);
    let mut ret = ChainNode::new(ret_id, BLOCK);
    ret.devices.push(fx(1.0, 0.0));
    layout.place_chain(ret_id, &default_strip());
    ret.set_output(Some(target));
    let mut rack = RackNode::new(RackKind::Drum);
    rack.chains.push(Box::new(drum));
    rack.returns.push(Box::new(ret));
    let mut a = Box::new(TrackNode::new(track, BLOCK));
    a.devices.push(seq(vec![(0, note_on(36, 1.0))]));
    a.devices
        .push(Box::new(DeviceSlot::rack(rack_id, Box::new(rack))));
    a.bus = layout.build();
    let plain = Box::new(TrackNode::new(target, BLOCK));

    let plan = RenderPlan {
        steps: vec![
            Step {
                track: 0,
                inputs: vec![],
            },
            Step {
                track: 1,
                inputs: vec![BusIn {
                    from: 0,
                    tap: Tap::Aux(DeviceId(0), 0),
                    gain: None,
                    delay: 0,
                    feedback: false,
                }],
            },
            Step {
                track: 2,
                inputs: vec![BusIn::post_mixer(0), BusIn::post_mixer(1)],
            },
        ],
        levels: vec![0..1, 1..2, 2..3],
        ..Default::default()
    };
    let txn = GraphTxn::new(
        vec![
            GraphOp::AddTrack { index: 0, track: a },
            GraphOp::AddTrack {
                index: 1,
                track: plain,
            },
            GraphOp::SetRackOuts {
                track,
                outs: Box::new(RackOuts::new(vec![target], BLOCK)),
            },
        ],
        Some(plan),
    );
    assert!(h.send(EngineCommand::Graph(txn)).is_ok());
    let (l, _) = render(&mut engine, SETTLE);
    assert!(engine.has_compiled_plan());
    assert!(
        close(*l.last().unwrap(), 1.0 + 1.0),
        "chain direct plus the return track's copy: {}",
        l.last().unwrap()
    );
}

// ------------------------------------------------------------------ macros

/// A rack with one Knob device, a macro driving it, and automation available on the macro.
struct MacroRig {
    engine: Engine,
    h: EngineHandle,
    track: TrackId,
    layout: BusLayout,
    rack: DeviceId,
    knob: CellIx,
}

const KNOB_MIN: f64 = 0.0;
const KNOB_MAX: f64 = 2.0;

fn macro_rig() -> MacroRig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack = DeviceId::new();
    let device = DeviceId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack, true, &rack_params::infos(), |p| p.default);
    let params = [ParamInfo::continuous(
        0, "Level", KNOB_MIN, KNOB_MAX, 0.5, "",
    )];
    layout.place_device(device, true, &params, |p| p.default);
    let knob = layout.device_cell(device, 0).unwrap();
    let chain = ChainNode::new(ChainId::new(), BLOCK);
    let mut chain = chain;
    chain.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Knob { value: 0.5 }),
    )));
    let node = rack_of(RackKind::Instrument, vec![Box::new(chain)]);
    let mut t = track_of(track, vec![Box::new(DeviceSlot::rack(rack, node))]);
    t.bus = layout.build();
    add_track(&mut h, t);
    MacroRig {
        engine,
        h,
        track,
        layout,
        rack,
        knob,
    }
}

impl MacroRig {
    fn routes(&mut self, macros: Vec<MacroRoute>, lanes: Vec<AutomationLane>) {
        let routes = Arc::new(TrackRoutes::new(lanes).with_macros(macros));
        assert!(self
            .h
            .send(EngineCommand::Param(ParamCmd::SetRoutes {
                track: TrackRef::Track(self.track),
                routes,
            }))
            .is_ok());
    }

    fn map(&self, min: f64, max: f64) -> MacroRoute {
        let source = self.layout.device_cell(self.rack, 0).unwrap();
        let range = self.layout.spec(self.knob).unwrap().range;
        MacroRoute::new(source, self.knob, range, min, max)
    }

    fn set_macro(&mut self, value: f64, gesture: bool) {
        let cell = self.layout.device_cell(self.rack, 0).unwrap();
        assert!(self
            .h
            .send(EngineCommand::Param(ParamCmd::Set {
                track: TrackRef::Track(self.track),
                cell,
                value,
                gesture,
            }))
            .is_ok());
    }

    fn level(&mut self) -> f32 {
        let (l, _) = render(&mut self.engine, BLOCK * 2);
        *l.last().unwrap()
    }
}

#[test]
fn a_macro_sweeps_its_mapped_range() {
    let mut r = macro_rig();
    let map = r.map(0.5, 1.5);
    r.routes(vec![map], Vec::new());
    r.set_macro(0.0, true);
    assert!(close(r.level(), 0.5), "min");
    r.set_macro(127.0, true);
    assert!(close(r.level(), 1.5), "max");
    r.set_macro(127.0 / 2.0, true);
    assert!(close(r.level(), 1.0), "halfway");
}

#[test]
fn an_inverted_range_sweeps_backwards() {
    let mut r = macro_rig();
    let map = r.map(1.5, 0.5);
    r.routes(vec![map], Vec::new());
    r.set_macro(0.0, true);
    assert!(close(r.level(), 1.5));
    r.set_macro(127.0, true);
    assert!(close(r.level(), 0.5));
}

#[test]
fn a_macro_owns_its_parameter_over_the_knob_and_automation() {
    let mut r = macro_rig();
    let map = r.map(0.0, 2.0);
    let lane = AutomationLane::new(
        r.knob,
        r.layout.spec(r.knob).unwrap().range,
        [(0.0, 0.25, 0.0)],
    )
    .unwrap();
    r.routes(vec![map], vec![lane]);
    r.set_macro(63.5, true);
    assert!(close(r.level(), 1.0), "the macro, not the envelope");

    assert!(r
        .h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(r.track),
            cell: r.knob,
            value: 1.75,
            gesture: true,
        }))
        .is_ok());
    assert!(close(r.level(), 1.0), "and locked against the knob");
    assert!(!r.layout.state(r.knob).overridden());
    assert!(r.layout.state(r.knob).macro_owned());
}

#[test]
fn clip_envelope_modulation_still_moves_a_mapped_parameter() {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let rack = DeviceId::new();
    let device = DeviceId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack, true, &rack_params::infos(), |p| p.default);
    let params = [ParamInfo::continuous(0, "Level", 0.0, 1.0, 0.5, "")];
    layout.place_device(device, true, &params, |p| p.default);
    let knob = layout.device_cell(device, 0).unwrap();
    let mut chain = ChainNode::new(ChainId::new(), BLOCK);
    chain.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Knob { value: 0.5 }),
    )));
    let node = rack_of(RackKind::Instrument, vec![Box::new(chain)]);
    let mut t = track_of(track, vec![Box::new(DeviceSlot::rack(rack, node))]);
    t.bus = layout.build();
    struct ClipEnv(CellIx);
    impl ControlHooks for ClipEnv {
        fn modulation(&mut self, _: ControlStep, bus: &mut ParamBus) {
            bus.modulate(self.0, 0.0, 0.5);
        }
    }
    t.hooks = Some(Box::new(ClipEnv(knob)));
    let source = layout.device_cell(rack, 0).unwrap();
    let routes = Arc::new(
        TrackRoutes::new(Vec::new()).with_macros(vec![MacroRoute::new(
            source,
            knob,
            layout.spec(knob).unwrap().range,
            0.0,
            1.0,
        )]),
    );
    t.routes = routes;
    add_track(&mut h, t);
    let cell = layout.device_cell(rack, 0).unwrap();
    assert!(h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(track),
            cell,
            value: 127.0,
            gesture: true,
        }))
        .is_ok());
    let mut engine = engine;
    let (l, _) = render(&mut engine, BLOCK * 2);
    assert!(
        close(*l.last().unwrap(), 0.5),
        "a macro at max scaled by the envelope: {}",
        l.last().unwrap()
    );
}

#[test]
fn automation_of_a_macro_knob_drives_what_it_maps() {
    let mut r = macro_rig();
    let map = r.map(0.0, 2.0);
    let source = r.layout.device_cell(r.rack, 0).unwrap();
    let range = r.layout.spec(source).unwrap().range;
    let lane = AutomationLane::new(source, range, [(0.0, 63.5, 0.0)]).unwrap();
    r.routes(vec![map], vec![lane]);
    r.set_macro(0.0, false);
    assert!(close(r.level(), 1.0));
}

#[test]
fn one_macro_can_drive_many_parameters_and_nests() {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let track = TrackId::new();
    let outer = DeviceId::new();
    let inner = DeviceId::new();
    let device = DeviceId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(outer, true, &rack_params::infos(), |p| p.default);
    layout.place_device(inner, true, &rack_params::infos(), |p| p.default);
    let params = [ParamInfo::continuous(0, "Level", 0.0, 1.0, 0.0, "")];
    layout.place_device(device, true, &params, |p| p.default);
    let knob = layout.device_cell(device, 0).unwrap();
    let mut inner_chain = ChainNode::new(ChainId::new(), BLOCK);
    inner_chain.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Knob { value: 0.0 }),
    )));
    let inner_rack = rack_of(RackKind::Instrument, vec![Box::new(inner_chain)]);
    let mut outer_chain = ChainNode::new(ChainId::new(), BLOCK);
    outer_chain
        .devices
        .push(Box::new(DeviceSlot::rack(inner, inner_rack)));
    let outer_rack = rack_of(RackKind::Instrument, vec![Box::new(outer_chain)]);
    let mut t = track_of(track, vec![Box::new(DeviceSlot::rack(outer, outer_rack))]);
    t.bus = layout.build();
    // The outer macro drives the inner macro, which drives the device.
    let spec = |c: CellIx| layout.spec(c).unwrap().range;
    let inner_macro = layout.device_cell(inner, 0).unwrap();
    let outer_macro = layout.device_cell(outer, 0).unwrap();
    t.routes = Arc::new(TrackRoutes::new(Vec::new()).with_macros(vec![
        MacroRoute::new(outer_macro, inner_macro, spec(inner_macro), 0.0, 127.0),
        MacroRoute::new(inner_macro, knob, spec(knob), 0.0, 1.0),
    ]));
    add_track(&mut h, t);
    let cell = layout.device_cell(outer, 0).unwrap();
    assert!(h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(track),
            cell,
            value: 127.0,
            gesture: true,
        }))
        .is_ok());
    let mut engine = engine;
    let (l, _) = render(&mut engine, BLOCK * 2);
    assert!(
        close(*l.last().unwrap(), 1.0),
        "through two macros in the same step: {}",
        l.last().unwrap()
    );
}

// ------------------------------------------------------------------ depth and edits

#[test]
fn racks_nest_to_the_depth_limit_and_keep_their_gains() {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let mut inner: Box<DeviceSlot> = fx(0.5, 0.25);
    for _ in 0..MAX_RACK_DEPTH {
        let mut chain = ChainNode::new(ChainId::new(), BLOCK);
        chain.devices.push(inner);
        let rack = rack_of(RackKind::Audio, vec![Box::new(chain)]);
        inner = Box::new(DeviceSlot::rack(DeviceId::new(), rack));
    }
    add_track(&mut h, track_of(TrackId::new(), vec![dc(), inner]));
    let mut engine = engine;
    let (l, _) = render(&mut engine, BLOCK);
    assert!(close(l[BLOCK - 1], 0.75), "{}", l[BLOCK - 1]);
}

/// A tiny xorshift, seeded per fuzz run.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Many zone, strip, selector and chain edits sent mid-playback, one transaction at a time. Every
/// block must run off the heap and never produce a bad sample, whatever the edits add up to.
fn fuzz(seed: u64, blocks: usize) {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let mut rng = Rng(seed);
    let track = TrackId::new();
    let rack = DeviceId::new();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack, true, &rack_params::infos(), |p| p.default);
    let mut chains: Vec<ChainId> = Vec::new();
    let mut returns: Vec<ChainId> = Vec::new();

    let new_chain = |rng: &mut Rng, layout: &mut BusLayout, chains: &mut Vec<ChainId>| {
        let (t, _) = tone(rng.unit() as f32);
        let mut c = ChainNode::new(ChainId::new(), BLOCK);
        c.devices.push(t);
        c.zones = random_zones(rng);
        layout.place_chain(c.id, &default_strip());
        chains.push(c.id);
        Box::new(c)
    };

    let mut rack_node = RackNode::new(RackKind::Drum);
    for _ in 0..3 {
        rack_node
            .chains
            .push(new_chain(&mut rng, &mut layout, &mut chains));
    }
    let mut notes = Vec::new();
    for i in 0..(blocks as u64 * BLOCK as u64 / 64) {
        let at = i * 64 + rng.below(32);
        notes.push((at, note_on(rng.below(127) as u8, rng.unit() as f32)));
        if rng.below(2) == 0 {
            notes.push((at + 32, note_off(rng.below(127) as u8)));
        }
    }
    let mut t = track_of(
        track,
        vec![
            seq(notes),
            Box::new(DeviceSlot::rack(rack, Box::new(rack_node))),
        ],
    );
    t.bus = layout.build();
    add_track(&mut h, t);
    assert!(h.send(EngineCommand::Transport(TransportCmd::Play)).is_ok());

    let mut buffers = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    let selector = layout
        .device_cell(rack, rack_params::CHAIN_SELECT.0)
        .unwrap();
    for block in 0..blocks {
        match rng.below(8) {
            0 if chains.len() < 16 => {
                let node = new_chain(&mut rng, &mut layout, &mut chains);
                let bus = layout.build();
                let index = rng.below(chains.len() as u64) as usize;
                send_txn(
                    &mut h,
                    track,
                    vec![
                        GraphOp::SwapBus {
                            target: TrackRef::Track(track),
                            bus,
                        },
                        GraphOp::InsertChain {
                            target: ChainAddr::track(track),
                            device: rack,
                            index,
                            chain: node,
                        },
                    ],
                );
            }
            1 if chains.len() > 1 => {
                let i = rng.below(chains.len() as u64) as usize;
                let id = chains.remove(i);
                layout.remove_chain(id);
                send_txn(
                    &mut h,
                    track,
                    vec![GraphOp::RemoveChain {
                        target: ChainAddr::track(track),
                        device: rack,
                        chain: id,
                    }],
                );
            }
            2 if !chains.is_empty() => {
                let id = chains[rng.below(chains.len() as u64) as usize];
                let zones = random_zones(&mut rng);
                send_txn(
                    &mut h,
                    track,
                    vec![GraphOp::SetChainZones {
                        target: ChainAddr::track(track),
                        device: rack,
                        chain: id,
                        zones,
                    }],
                );
            }
            3 if !chains.is_empty() => {
                let id = chains[rng.below(chains.len() as u64) as usize];
                let control = match rng.below(4) {
                    0 => controls::VOLUME,
                    1 => controls::PAN,
                    2 => controls::ACTIVATOR,
                    _ => controls::SOLO,
                };
                let value = rng.unit() * 2.0 - 1.0;
                let cell = layout.chain_cell(id, control).unwrap();
                assert!(h
                    .send(EngineCommand::Param(ParamCmd::Set {
                        track: TrackRef::Track(track),
                        cell,
                        value,
                        gesture: false,
                    }))
                    .is_ok());
            }
            4 => {
                let value = rng.unit() * 127.0;
                assert!(h
                    .send(EngineCommand::Param(ParamCmd::Set {
                        track: TrackRef::Track(track),
                        cell: selector,
                        value,
                        gesture: false,
                    }))
                    .is_ok());
            }
            5 if returns.len() < 6 => {
                let mut r = ChainNode::new(ChainId::new(), BLOCK);
                layout.place_chain(r.id, &default_strip());
                returns.push(r.id);
                r.devices.push(fx(1.0, 0.0));
                let bus = layout.build();
                send_txn(
                    &mut h,
                    track,
                    vec![
                        GraphOp::SwapBus {
                            target: TrackRef::Track(track),
                            bus,
                        },
                        GraphOp::InsertReturn {
                            target: ChainAddr::track(track),
                            device: rack,
                            index: 0,
                            chain: Box::new(r),
                        },
                    ],
                );
            }
            6 if !returns.is_empty() => {
                let i = rng.below(returns.len() as u64) as usize;
                let id = returns.remove(i);
                layout.remove_chain(id);
                send_txn(
                    &mut h,
                    track,
                    vec![GraphOp::RemoveChain {
                        target: ChainAddr::track(track),
                        device: rack,
                        chain: id,
                    }],
                );
            }
            _ => {}
        }
        let (l, r) = (&mut buffers.0, &mut buffers.1);
        assert_no_alloc(|| engine.process_stereo(l, r));
        for s in l.iter().chain(r.iter()) {
            assert!(s.is_finite(), "seed {seed}, block {block}");
        }
        h.collect_garbage();
    }
}

fn send_txn(h: &mut EngineHandle, _track: TrackId, ops: Vec<GraphOp>) {
    assert!(h
        .send(EngineCommand::Graph(GraphTxn::new(ops, None)))
        .is_ok());
}

fn random_zones(rng: &mut Rng) -> Zones {
    let (low, high) = (rng.below(127) as u8, rng.below(127) as u8);
    let (low, high) = (low.min(high), low.max(high));
    let zone = Zone {
        low,
        high,
        fade_low: low.saturating_sub(rng.below(8) as u8),
        fade_high: high.saturating_add(rng.below(8) as u8).min(127),
    };
    let mut z = Zones {
        key: zone,
        velocity: Zone::span(1, 127),
        select: zone,
        drum: None,
    };
    z.drum = Some(DrumPad {
        receive: if rng.below(4) == 0 {
            None
        } else {
            Some(rng.below(127) as u8)
        },
        play: rng.below(127) as u8,
        choke: if rng.below(2) == 0 {
            None
        } else {
            Some(1 + rng.below(16) as u8)
        },
    });
    z
}

#[test]
fn random_rack_edits_while_playing_stay_finite_and_off_the_heap() {
    for seed in [0x9E37_79B9_7F4A_7C15, 1, 7, 0xBEEF, 2024] {
        fuzz(seed, 400);
    }
}
