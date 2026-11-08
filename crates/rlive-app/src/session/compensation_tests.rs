//! Automatic delay compensation and Track Delay: what the plan works out, and impulses through
//! generated topologies at the sample.

use super::graph::compile_with;
use super::timing::{compensation_ops, LatencySource, Timing};
use super::*;
use rlive_core::{ChainId, ProcessContext, Processor};
use rlive_engine::{
    create_engine, ChainNode, DeviceSlot, Engine, GraphTxn, RackKind as EngineRack, RackNode,
    RenderPlan, TrackNode,
};
use rlive_project::{Chain, Monitor, RackKind, Track, TrackDelay, TrackSend};
use rlive_testkit::assert_no_alloc;
use std::collections::HashMap;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const AT: u64 = 8192;

/// Latencies a test decides.
#[derive(Default)]
struct Fake(HashMap<DeviceId, u32>);

impl LatencySource for Fake {
    fn device_latency(&self, device: &Device) -> u32 {
        self.0.get(&device.id).copied().unwrap_or(0)
    }
}

fn midi(p: &Project, name: &str) -> Track {
    Track::new(name, TrackKind::Midi, [1, 2, 3], p.scenes.len())
}

/// A project of `n` MIDI tracks.
fn project(n: usize) -> Project {
    let mut p = Project::empty();
    p.tracks = (0..n).map(|i| midi(&p, &format!("{i}"))).collect();
    p
}

fn add_return(p: &mut Project) -> TrackId {
    let r = Track::new("Return", TrackKind::Return, [1, 1, 1], p.scenes.len());
    let id = r.id;
    p.returns.push(r);
    id
}

fn send_from(p: &mut Project, track: usize, to: TrackId) {
    p.tracks[track].sends.push(TrackSend {
        to,
        level_db: 0.0,
        enabled: true,
    });
}

fn latent_device(fake: &mut Fake, latency: u32) -> Device {
    let d = Device::builtin("rlive.reverb", "Latent");
    fake.0.insert(d.id, latency);
    d
}

fn delays(step: &rlive_engine::Step) -> Vec<(u32, u32)> {
    step.inputs.iter().map(|i| (i.from, i.delay)).collect()
}

#[test]
fn the_slower_track_is_what_the_others_are_held_back_to() {
    let mut p = project(2);
    let mut fake = Fake::default();
    p.tracks[0].devices.push(latent_device(&mut fake, 100));
    let t = Timing::measure(&p, SR, &fake);
    let plan = compile_with(&p, &t);
    let master = plan.steps.last().unwrap();
    assert_eq!(delays(master), [(0, 0), (1, 100)]);
    assert_eq!(plan.output_latency, 100);
    assert_eq!(plan.click_delay, 100);
}

#[test]
fn a_track_of_two_devices_counts_them_both() {
    let mut p = project(2);
    let mut fake = Fake::default();
    p.tracks[0].devices.push(latent_device(&mut fake, 100));
    p.tracks[0].devices.push(latent_device(&mut fake, 28));
    let plan = compile_with(&p, &Timing::measure(&p, SR, &fake));
    assert_eq!(delays(plan.steps.last().unwrap()), [(0, 0), (1, 128)]);
}

#[test]
fn a_rack_takes_its_slowest_chain_and_its_others_catch_up() {
    let mut p = project(2);
    let mut fake = Fake::default();
    let mut rack = Device::rack(RackKind::Audio, "Rack");
    let mut slow = Chain::new("slow");
    slow.devices.push(latent_device(&mut fake, 96));
    let mut quick = Chain::new("quick");
    quick.devices.push(latent_device(&mut fake, 16));
    let (slow_id, quick_id) = (slow.id, quick.id);
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(slow);
        chains.push(quick);
    }
    p.tracks[0].devices.push(rack);
    let t = Timing::measure(&p, SR, &fake);
    let of = |id: ChainId| t.chains.iter().find(|c| c.chain == id).unwrap().delay;
    assert_eq!(of(slow_id), 0);
    assert_eq!(of(quick_id), 80);
    let plan = compile_with(&p, &t);
    assert_eq!(delays(plan.steps.last().unwrap()), [(0, 0), (1, 96)]);
}

#[test]
fn sends_and_returns_line_up_at_the_master() {
    let mut p = project(2);
    let mut fake = Fake::default();
    let ret = add_return(&mut p);
    send_from(&mut p, 0, ret);
    p.returns[0].devices.push(latent_device(&mut fake, 200));
    p.tracks[1].devices.push(latent_device(&mut fake, 50));
    let plan = compile_with(&p, &Timing::measure(&p, SR, &fake));
    let master = plan.steps.iter().find(|s| s.track == 3).unwrap();
    // A send counts even at its lowest, so a level can change without a new plan: the return
    // waits for the slower sender, 50 late, and is 200 late itself on top.
    let at_return = plan.steps.iter().find(|s| s.track == 2).unwrap();
    assert_eq!(delays(at_return), [(0, 50), (1, 0)]);
    assert_eq!(delays(master), [(0, 250), (1, 200), (2, 0)]);
}

#[test]
fn a_feedback_send_is_left_alone() {
    let mut p = project(0);
    let mut fake = Fake::default();
    let ret = add_return(&mut p);
    p.returns[0].devices.push(latent_device(&mut fake, 64));
    p.returns[0].sends.push(TrackSend {
        to: ret,
        level_db: 0.0,
        enabled: true,
    });
    let plan = compile_with(&p, &Timing::measure(&p, SR, &fake));
    let feedback = plan.steps[0].inputs.iter().find(|i| i.feedback).unwrap();
    assert_eq!(feedback.delay, 0);
    assert_eq!(plan.output_latency, 64);
}

#[test]
fn track_delay_moves_a_track_relative_to_the_rest() {
    let mut p = project(2);
    p.tracks[0].delay = TrackDelay::Ms(10.0); // 480 frames at 48 kHz
    let plan = compile_with(&p, &Timing::measure(&p, SR, &Fake::default()));
    assert_eq!(delays(plan.steps.last().unwrap()), [(0, 480), (1, 0)]);

    // Pulled forward, it plays on time and everything else waits.
    p.tracks[0].delay = TrackDelay::Samples(-100);
    let plan = compile_with(&p, &Timing::measure(&p, SR, &Fake::default()));
    assert_eq!(delays(plan.steps.last().unwrap()), [(0, 0), (1, 100)]);
}

#[test]
fn track_delay_on_a_group_moves_its_children_together() {
    let mut p = project(2);
    let mut group = Track::new("G", TrackKind::Group, [0, 0, 0], p.scenes.len());
    group.delay = TrackDelay::Samples(32);
    p.tracks[1].group = Some(group.id);
    p.tracks.insert(0, group);
    let plan = compile_with(&p, &Timing::measure(&p, SR, &Fake::default()));
    let at_group = plan.steps.iter().find(|s| s.track == 0).unwrap();
    assert_eq!(delays(at_group), [(2, 0)]);
    let master = plan.steps.last().unwrap();
    assert_eq!(delays(master), [(0, 32), (1, 0)]);
}

#[test]
fn turning_compensation_off_restores_the_plan_without_it() {
    let mut p = project(3);
    let mut fake = Fake::default();
    p.tracks[0].devices.push(latent_device(&mut fake, 100));
    let mut rack = Device::rack(RackKind::Audio, "Rack");
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        let mut c = Chain::new("a");
        c.devices.push(latent_device(&mut fake, 50));
        chains.push(c);
        chains.push(Chain::new("b"));
    }
    p.tracks[1].devices.push(rack);
    p.tracks[2].delay = TrackDelay::Samples(7);
    p.delay_compensation = false;
    let t = Timing::measure(&p, SR, &fake);
    assert!(t.chains.iter().all(|c| c.delay == 0));
    assert!(t.bypass.iter().all(|d| d.delay == 0));
    assert_eq!(compile_with(&p, &t), compile_plan(&p));
    p.delay_compensation = true;
    assert_ne!(
        compile_with(&p, &Timing::measure(&p, SR, &fake)),
        compile_plan(&p)
    );
}

#[test]
fn a_monitored_input_skips_compensation_only_with_the_option_on() {
    let mut p = project(3);
    let mut fake = Fake::default();
    p.tracks[0].devices.push(latent_device(&mut fake, 40));
    p.tracks[1].devices.push(latent_device(&mut fake, 500));
    p.tracks[2].devices.push(latent_device(&mut fake, 1000));
    p.tracks[0].routing.monitor = Monitor::In;
    p.tracks[1].routing.monitor = Monitor::Auto;
    p.tracks[1].arm = true;
    p.tracks[2].routing.monitor = Monitor::Off;

    let plan = compile_with(&p, &Timing::measure(&p, SR, &fake));
    assert_eq!(
        delays(plan.steps.last().unwrap()),
        [(0, 960), (1, 500), (2, 0)],
        "all in step"
    );

    p.reduced_latency_when_monitoring = true;
    let plan = compile_with(&p, &Timing::measure(&p, SR, &fake));
    assert_eq!(
        delays(plan.steps.last().unwrap()),
        [(0, 0), (1, 0), (2, 0)],
        "the monitored tracks skip it, and what's left already lines up"
    );
    assert_eq!(plan.output_latency, 1000);
}

// ------------------------------------------------------------------ impulses

/// Adds one sample to its input at a chosen frame.
struct Click {
    at: u64,
    pos: u64,
}

impl Processor for Click {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = ctx.audio.left.len() as u64;
        if (self.pos..self.pos + n).contains(&self.at) {
            let i = (self.at - self.pos) as usize;
            ctx.audio.left[i] += 1.0;
            ctx.audio.right[i] += 1.0;
        }
        self.pos += n;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Holds its input back by what it reports.
struct Latent {
    ring: [Vec<f32>; 2],
    head: usize,
}

impl Latent {
    fn new(frames: u32) -> Box<Self> {
        Box::new(Self {
            ring: [vec![0.0; frames as usize], vec![0.0; frames as usize]],
            head: 0,
        })
    }
}

impl Processor for Latent {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = self.ring[0].len();
        if n == 0 {
            return;
        }
        for i in 0..ctx.audio.left.len() {
            let (l, r) = (self.ring[0][self.head], self.ring[1][self.head]);
            self.ring[0][self.head] = ctx.audio.left[i];
            self.ring[1][self.head] = ctx.audio.right[i];
            ctx.audio.left[i] = l;
            ctx.audio.right[i] = r;
            self.head = (self.head + 1) % n;
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
    fn latency(&self) -> u32 {
        self.ring[0].len() as u32
    }
}

fn slot_of(d: &Device, fake: &Fake) -> Box<DeviceSlot> {
    if let DeviceKind::Rack { chains, .. } = &d.kind {
        let mut rack = Box::new(RackNode::new(EngineRack::Audio));
        for c in chains {
            let mut node = Box::new(ChainNode::new(c.id, BLOCK));
            for inner in &c.devices {
                node.devices.push(slot_of(inner, fake));
            }
            rack.chains.push(node);
        }
        return Box::new(DeviceSlot::rack(d.id, rack));
    }
    let latency = fake.0.get(&d.id).copied().unwrap_or(0);
    Box::new(DeviceSlot::new(d.id, Latent::new(latency)))
}

fn nodes_of(p: &Project, fake: &Fake) -> (Vec<Box<TrackNode>>, Box<TrackNode>) {
    let returns: Vec<TrackId> = p.returns.iter().map(|r| r.id).collect();
    let track = |t: &Track| -> Box<TrackNode> {
        let mut node = TrackNode::new(t.id, BLOCK);
        if t.kind == TrackKind::Return {
            node = node.with_feedback(BLOCK);
        }
        for (k, ret) in returns.iter().enumerate() {
            let db = t.send_level(*ret);
            node.seed_send(k, db);
        }
        let mut node = Box::new(node);
        if t.kind == TrackKind::Midi {
            node.devices.push(Box::new(DeviceSlot::new(
                DeviceId::new(),
                Box::new(Click { at: AT, pos: 0 }),
            )));
        }
        for d in &t.devices {
            node.devices.push(slot_of(d, fake));
        }
        node
    };
    let tracks = p.tracks.iter().chain(&p.returns).map(track).collect();
    let mut master = Box::new(TrackNode::master(BLOCK));
    for d in &p.master.devices {
        master.devices.push(slot_of(d, fake));
    }
    (tracks, master)
}

fn install(handle: &mut rlive_engine::EngineHandle, p: &Project, fake: &Fake) -> RenderPlan {
    let timing = Timing::measure(p, SR, fake);
    let plan = compile_with(p, &timing);
    let (tracks, master) = nodes_of(p, fake);
    let mut ops = vec![GraphOp::ReplaceAll {
        tracks: Box::new(tracks),
        master,
    }];
    ops.extend(compensation_ops(&plan, &timing, p, BLOCK));
    let txn = GraphTxn::new(ops, Some(plan.clone()));
    assert!(handle.send(EngineCommand::Graph(txn)).is_ok());
    plan
}

fn render(engine: &mut Engine, handle: &mut rlive_engine::EngineHandle, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    while out.len() < frames {
        let n = BLOCK.min(frames - out.len());
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        // The first block applies the transaction, which builds the engine.
        if out.is_empty() {
            engine.process_stereo(&mut l, &mut r);
        } else {
            assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
        }
        out.extend_from_slice(&l);
        handle.collect_garbage();
    }
    out
}

/// Where the output isn't silent, as (frame, level rounded to a thousandth).
fn peaks(out: &[f32]) -> Vec<(usize, f32)> {
    out.iter()
        .enumerate()
        .filter(|(_, s)| s.abs() > 1e-6)
        .map(|(i, s)| (i, (s * 1000.0).round() / 1000.0))
        .collect()
}

/// How much a device chain multiplies an impulse: racks sum their chains.
fn multiplicity(devices: &[Device]) -> f32 {
    devices
        .iter()
        .map(|d| match &d.kind {
            DeviceKind::Rack { chains, .. } => {
                chains.iter().map(|c| multiplicity(&c.devices)).sum()
            }
            _ => 1.0,
        })
        .product()
}

/// A random project of tracks, groups, returns, sends and racks, with random latencies.
fn generate(seed: u32) -> (Project, Fake) {
    let mut rng = seed.wrapping_mul(2654435761) | 1;
    let mut next = |n: u32| {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng % n
    };
    let mut p = Project::empty();
    let mut fake = Fake::default();

    fn chain_of(devs: u32, next: &mut impl FnMut(u32) -> u32, fake: &mut Fake) -> Vec<Device> {
        (0..devs)
            .map(|_| {
                if next(3) == 0 {
                    let mut rack = Device::rack(RackKind::Audio, "Rack");
                    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
                        for _ in 0..1 + next(2) {
                            let mut c = Chain::new("c");
                            let n = next(3);
                            c.devices = chain_of(n, next, fake);
                            chains.push(c);
                        }
                    }
                    rack
                } else {
                    let d = Device::builtin("rlive.reverb", "L");
                    fake.0.insert(d.id, 1 + next(3000));
                    d
                }
            })
            .collect()
    }

    for _ in 0..1 + next(3) {
        add_return(&mut p);
        let n = p.returns.len() - 1;
        p.returns[n].pre_fader_sends = next(2) == 0;
        let devs = next(3);
        p.returns[n].devices = chain_of(devs, &mut next, &mut fake);
    }
    let returns: Vec<TrackId> = p.returns.iter().map(|r| r.id).collect();

    let mut groups: Vec<TrackId> = Vec::new();
    for _ in 0..next(3) {
        let mut g = Track::new("G", TrackKind::Group, [0, 0, 0], p.scenes.len());
        let devs = next(3);
        g.devices = chain_of(devs, &mut next, &mut fake);
        if !groups.is_empty() && next(2) == 0 {
            g.group = Some(groups[next(groups.len() as u32) as usize]);
        }
        groups.push(g.id);
        p.tracks.push(g);
    }
    for i in 0..1 + next(4) {
        let mut t = Track::new(&format!("{i}"), TrackKind::Midi, [1, 2, 3], p.scenes.len());
        let devs = next(4);
        t.devices = chain_of(devs, &mut next, &mut fake);
        if !groups.is_empty() && next(2) == 0 {
            t.group = Some(groups[next(groups.len() as u32) as usize]);
        }
        for r in &returns {
            if next(2) == 0 {
                t.sends.push(TrackSend {
                    to: *r,
                    level_db: 0.0,
                    enabled: true,
                });
            }
        }
        p.tracks.push(t);
    }
    (p, fake)
}

/// Every route from a click reaches the Master on the same frame, at the sum of their gains.
fn check_alignment(p: &Project, fake: &Fake) {
    let (mut engine, mut handle) = create_engine(SR, BLOCK);
    let plan = install(&mut handle, p, fake);
    let master = p.tracks.len() + p.returns.len();
    let mut chains = vec![0.0; master];
    for (i, t) in p.tracks.iter().chain(&p.returns).enumerate() {
        chains[i] = multiplicity(&t.devices);
    }
    chains.push(1.0);
    let frames = AT as usize + plan.output_latency as usize + BLOCK * 4;
    let out = render(&mut engine, &mut handle, frames);
    let want: f32 = p
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TrackKind::Midi)
        .map(|(i, t)| {
            // A send that's off is silent, not a route.
            let sent: f32 = p
                .returns
                .iter()
                .map(|r| r.id)
                .zip(&chains[p.tracks.len()..])
                .filter(|(r, _)| t.send_level(*r) >= 0.0)
                .map(|(_, c)| c)
                .sum();
            let main = gain_through_main(p, &chains, i);
            chains[i] * (sent + main)
        })
        .sum();
    let peaks = peaks(&out);
    if want == 0.0 {
        assert!(peaks.is_empty());
        return;
    }
    assert_eq!(peaks.len(), 1, "one impulse at the Master: {peaks:?}");
    let (frame, level) = peaks[0];
    assert_eq!(frame, (AT + plan.output_latency as u64) as usize);
    assert!((level - want).abs() < 0.01, "{level} against {want}");
}

/// The gain from a track's main output to the Master through its groups.
fn gain_through_main(p: &Project, chains: &[f32], i: usize) -> f32 {
    match p.tracks[i].group.and_then(|g| p.track_index(g)) {
        Some(g) => chains[g] * gain_through_main(p, chains, g),
        None => 1.0,
    }
}

#[test]
fn every_route_of_a_generated_session_arrives_together() {
    for seed in 1..=60 {
        let (p, fake) = generate(seed);
        check_alignment(&p, &fake);
    }
}

#[test]
fn compensation_off_plays_each_route_at_its_own_latency() {
    for seed in 1..=20 {
        let (mut p, fake) = generate(seed);
        p.delay_compensation = false;
        let (mut engine, mut handle) = create_engine(SR, BLOCK);
        let plan = install(&mut handle, &p, &fake);
        assert_eq!(plan, compile_plan(&p));
        assert_eq!(plan.output_latency, 0);
        // Each route is as late as it happens to be: compare to rendering with the plan alone.
        let frames = AT as usize + 20_000;
        let out = render(&mut engine, &mut handle, frames);
        let (mut bare, mut h2) = create_engine(SR, BLOCK);
        let (tracks, master) = nodes_of(&p, &fake);
        let txn = GraphTxn::new(
            vec![GraphOp::ReplaceAll {
                tracks: Box::new(tracks),
                master,
            }],
            Some(compile_plan(&p)),
        );
        assert!(h2.send(EngineCommand::Graph(txn)).is_ok());
        let reference = render(&mut bare, &mut h2, frames);
        assert_eq!(out, reference, "seed {seed}");
    }
}

#[test]
fn track_delay_shifts_an_impulse_by_exactly_its_amount() {
    let mut p = project(2);
    p.tracks[0].delay = TrackDelay::Samples(137);
    let (mut engine, mut handle) = create_engine(SR, BLOCK);
    let plan = install(&mut handle, &p, &Fake::default());
    assert_eq!(plan.output_latency, 0);
    let out = render(&mut engine, &mut handle, AT as usize + BLOCK * 8);
    assert_eq!(
        peaks(&out),
        [(AT as usize, 1.0), (AT as usize + 137, 1.0)],
        "track 1 on time, track 0 late"
    );

    let mut p = project(2);
    p.tracks[1].delay = TrackDelay::Samples(-64);
    let (mut engine, mut handle) = create_engine(SR, BLOCK);
    let plan = install(&mut handle, &p, &Fake::default());
    assert_eq!(plan.output_latency, 64, "everything else waits for it");
    let out = render(&mut engine, &mut handle, AT as usize + BLOCK * 8);
    assert_eq!(
        peaks(&out),
        [(AT as usize, 1.0), (AT as usize + 64, 1.0)],
        "track 1 early, track 0 at the compensated position"
    );
}

// ------------------------------------------------------------------ through the session

fn session(project: Project) -> (Engine, Session) {
    let (engine, handle) = create_engine(SR, BLOCK);
    (
        engine,
        Session::new(handle, project, PluginCache::default()),
    )
}

fn run(engine: &mut Engine, s: &mut Session) {
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    s.idle();
}

#[test]
fn a_new_device_latency_recompiles_the_plan_at_idle() {
    let (mut engine, mut s) = session(project(2));
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    let dev = s.devices(chain)[0].id;
    run(&mut engine, &mut s);
    assert_eq!(
        engine.active_plan().steps.last().unwrap().inputs[1].delay,
        0
    );
    s.runtimes[&dev]
        .latency
        .store(300, std::sync::atomic::Ordering::Relaxed);
    s.idle();
    run(&mut engine, &mut s);
    let plan = engine.active_plan();
    assert_eq!(delays(plan.steps.last().unwrap()), [(0, 0), (1, 300)]);
    assert_eq!(plan.output_latency, 300);
}

#[test]
fn a_rack_reports_the_latency_of_its_slowest_chain() {
    let (_engine, mut s) = session(project(1));
    let t = s.project.tracks[0].id;
    let mut rack = Device::rack(RackKind::Audio, "Rack");
    let rack_id = rack.id;
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(Chain::new("a"));
    }
    s.add_device(ChainRef::Track(t), None, rack);
    let chain = s.devices(ChainRef::Track(t))[0].chains().next().unwrap().id;
    let nested = ChainRef::Track(t).child(rack_id, chain).unwrap();
    s.add_builtin(nested, "rlive.reverb");
    let dev = s.devices(nested)[0].id;
    s.runtimes[&dev]
        .latency
        .store(64, std::sync::atomic::Ordering::Relaxed);
    s.idle();
    assert_eq!(
        s.runtimes[&rack_id]
            .latency
            .load(std::sync::atomic::Ordering::Relaxed),
        64
    );
}

#[test]
fn turning_compensation_off_takes_the_delays_away_and_the_toggle_is_undoable() {
    let (mut engine, mut s) = session(project(2));
    let t = s.project.tracks[0].id;
    s.add_builtin(ChainRef::Track(t), "rlive.reverb");
    let dev = s.devices(ChainRef::Track(t))[0].id;
    s.runtimes[&dev]
        .latency
        .store(300, std::sync::atomic::Ordering::Relaxed);
    s.idle();
    run(&mut engine, &mut s);
    assert_eq!(engine.active_plan().output_latency, 300);

    s.commit_edits(false);
    s.set_delay_compensation(false);
    s.commit_edits(false);
    run(&mut engine, &mut s);
    assert_eq!(engine.active_plan().output_latency, 0);
    assert!(engine
        .active_plan()
        .steps
        .iter()
        .flat_map(|step| &step.inputs)
        .all(|i| i.delay == 0));

    assert!(s.undo());
    assert!(s.project.delay_compensation);
}

#[test]
fn track_delay_is_undoable_and_saved() {
    let (mut engine, mut s) = session(project(2));
    let t = s.project.tracks[0].id;
    s.commit_edits(false);
    s.set_track_delay(t, TrackDelay::Samples(90));
    s.commit_edits(false);
    run(&mut engine, &mut s);
    assert_eq!(
        engine.active_plan().steps.last().unwrap().inputs[0].delay,
        90
    );
    assert_eq!(s.project.track(t).unwrap().delay, TrackDelay::Samples(90));

    let loaded = Project::from_ron(&s.project.to_ron().unwrap()).unwrap();
    assert_eq!(loaded.track(t).unwrap().delay, TrackDelay::Samples(90));

    assert!(s.undo());
    run(&mut engine, &mut s);
    assert_eq!(s.project.track(t).unwrap().delay, TrackDelay::None);
    assert_eq!(
        engine.active_plan().steps.last().unwrap().inputs[0].delay,
        0
    );
}

#[test]
fn the_options_default_on_and_off_and_are_only_saved_when_changed() {
    let p = project(1);
    assert!(p.delay_compensation);
    assert!(!p.reduced_latency_when_monitoring);
    let text = p.to_ron().unwrap();
    assert!(!text.contains("delay_compensation"));
    assert!(!text.contains("reduced_latency"));
    let mut changed = p;
    changed.delay_compensation = false;
    let loaded = Project::from_ron(&changed.to_ron().unwrap()).unwrap();
    assert!(!loaded.delay_compensation);
}
