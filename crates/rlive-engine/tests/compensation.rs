//! Delay compensation in the engine: impulses through planned delays, at the sample.

use rlive_core::{ChainId, DeviceId, ProcessContext, Processor, TrackId};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const AT: u64 = 2048;

/// Adds one sample to its input at a chosen frame.
struct Click {
    at: u64,
    pos: u64,
}

impl Click {
    fn at(at: u64) -> Box<Self> {
        Box::new(Self { at, pos: 0 })
    }
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

/// Holds its input back by the latency it reports.
struct Latent {
    ring: [Vec<f32>; 2],
    head: usize,
}

impl Latent {
    fn new(frames: usize) -> Box<Self> {
        Box::new(Self {
            ring: [vec![0.0; frames], vec![0.0; frames]],
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

fn slot(p: Box<dyn Processor>) -> Box<DeviceSlot> {
    Box::new(DeviceSlot::new(DeviceId::new(), p))
}

/// A clip track: a click into a chain of latent devices.
fn source(id: TrackId, latencies: &[usize]) -> Box<TrackNode> {
    let mut t = Box::new(TrackNode::new(id, BLOCK));
    t.devices.push(slot(Click::at(AT)));
    for &n in latencies {
        t.devices.push(slot(Latent::new(n)));
    }
    t
}

fn latent(id: TrackId, latencies: &[usize]) -> Box<TrackNode> {
    let mut t = Box::new(TrackNode::new(id, BLOCK));
    for &n in latencies {
        t.devices.push(slot(Latent::new(n)));
    }
    t
}

fn step(track: u32, inputs: Vec<BusIn>) -> Step {
    Step { track, inputs }
}

fn chain_plan(steps: Vec<Step>) -> RenderPlan {
    let levels = (0..steps.len() as u32).map(|i| i..i + 1).collect();
    RenderPlan {
        steps,
        levels,
        ..Default::default()
    }
}

fn delayed(mut input: BusIn, frames: u32) -> BusIn {
    input.delay = frames;
    input
}

fn line(frames: u32) -> Option<Box<StereoLine>> {
    Some(StereoLine::new(frames, BLOCK))
}

fn lines(track: TrackId, main: u32, pre: u32, post: u32) -> GraphOp {
    GraphOp::SetTapLines {
        track,
        lines: TapLines {
            main: (main > 0).then(|| StereoLine::new(main, BLOCK)),
            pre: (pre > 0).then(|| StereoLine::new(pre, BLOCK)),
            post: (post > 0).then(|| StereoLine::new(post, BLOCK)),
        },
    }
}

fn install(
    h: &mut EngineHandle,
    tracks: Vec<Box<TrackNode>>,
    extra: Vec<GraphOp>,
    plan: RenderPlan,
) {
    let mut ops = Vec::new();
    for (index, track) in tracks.into_iter().enumerate() {
        ops.push(GraphOp::AddTrack { index, track });
    }
    ops.extend(extra);
    assert!(h
        .send(EngineCommand::Graph(GraphTxn::new(ops, Some(plan))))
        .is_ok());
}

fn render(engine: &mut Engine, frames: usize) -> Vec<f32> {
    let (l, _) = rlive_testkit::render_chunks(frames, BLOCK, |l, r| engine.process_stereo(l, r));
    l
}

/// Where the output isn't silent, as (frame, level).
fn peaks(out: &[f32]) -> Vec<(usize, f32)> {
    out.iter()
        .enumerate()
        .filter(|(_, s)| s.abs() > 1e-5)
        .map(|(i, s)| (i, (s * 1000.0).round() / 1000.0))
        .collect()
}

#[test]
fn a_delayed_input_meets_the_slower_one_exactly() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, b) = (TrackId::new(), TrackId::new());
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![]),
        step(
            2,
            vec![
                delayed(BusIn::post_mixer(0), 0),
                delayed(BusIn::post_mixer(1), 100),
            ],
        ),
    ]);
    install(
        &mut h,
        vec![source(a, &[100]), source(b, &[])],
        vec![lines(b, 100, 0, 0)],
        plan,
    );
    let out = render(&mut engine, 4096);
    assert!(engine.has_compiled_plan());
    assert_eq!(peaks(&out), [(AT as usize + 100, 2.0)]);
}

#[test]
fn a_send_taps_before_and_after_the_mixer_through_their_own_lines() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (s, r, r2) = (TrackId::new(), TrackId::new(), TrackId::new());
    let mut src = source(s, &[]);
    src.seed_send(0, 0.0);
    src.seed_send(1, 0.0);
    let ret = |id, n: usize| {
        let mut t = TrackNode::new(id, BLOCK).with_feedback(BLOCK);
        t.devices.push(slot(Latent::new(n)));
        Box::new(t)
    };
    // The returns are 50 and 400 late, and the source is held back to meet the slowest.
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(
            1,
            vec![delayed(BusIn::send(0, mix_cell::send(0), true), 350)],
        ),
        step(
            2,
            vec![delayed(BusIn::send(0, mix_cell::send(1), false), 0)],
        ),
        step(
            3,
            vec![
                delayed(BusIn::post_mixer(0), 400),
                delayed(BusIn::post_mixer(1), 0),
                delayed(BusIn::post_mixer(2), 0),
            ],
        ),
    ]);
    install(
        &mut h,
        vec![src, ret(r, 50), ret(r2, 400)],
        vec![lines(s, 400, 350, 0)],
        plan,
    );
    let out = render(&mut engine, 4096);
    assert_eq!(peaks(&out), [(AT as usize + 400, 3.0)]);
}

#[test]
fn a_group_lines_up_its_children() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, b, g) = (TrackId::new(), TrackId::new(), TrackId::new());
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![]),
        step(
            2,
            vec![
                delayed(BusIn::post_mixer(0), 0),
                delayed(BusIn::post_mixer(1), 30),
            ],
        ),
        step(3, vec![delayed(BusIn::post_mixer(2), 0)]),
    ]);
    install(
        &mut h,
        vec![source(a, &[30]), source(b, &[]), latent(g, &[70])],
        vec![lines(b, 30, 0, 0)],
        plan,
    );
    let out = render(&mut engine, 4096);
    assert_eq!(peaks(&out), [(AT as usize + 100, 2.0)]);
}

#[test]
fn a_rack_holds_its_shorter_chains_back_inside() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let t = TrackId::new();
    let mut track = Box::new(TrackNode::new(t, BLOCK));
    track.devices.push(slot(Click::at(AT)));
    let (slow, fast) = (ChainId::new(), ChainId::new());
    let mut rack = Box::new(RackNode::new(RackKind::Audio));
    let mut chain = Box::new(ChainNode::new(slow, BLOCK));
    chain.devices.push(slot(Latent::new(64)));
    rack.chains.push(chain);
    rack.chains.push(Box::new(ChainNode::new(fast, BLOCK)));
    let rack_id = DeviceId::new();
    track
        .devices
        .push(Box::new(DeviceSlot::rack(rack_id, rack)));
    let plan = chain_plan(vec![step(0, vec![]), step(1, vec![BusIn::post_mixer(0)])]);
    let ops = vec![GraphOp::SetChainDelay {
        target: ChainAddr::track(t),
        device: rack_id,
        chain: fast,
        delay: 64,
        line: line(64),
    }];
    install(&mut h, vec![track], ops, plan);
    let out = render(&mut engine, 4096);
    assert_eq!(peaks(&out), [(AT as usize + 64, 2.0)]);
}

#[test]
fn a_bypassed_device_keeps_its_timing() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let t = TrackId::new();
    let mut track = source(t, &[]);
    let id = DeviceId::new();
    let mut device = DeviceSlot::new(id, Latent::new(128));
    device.bypass = true;
    track.devices.push(Box::new(device));
    let plan = chain_plan(vec![step(0, vec![]), step(1, vec![BusIn::post_mixer(0)])]);
    let ops = vec![GraphOp::SetBypassDelay {
        target: ChainAddr::track(t),
        device: id,
        delay: 128,
        line: line(128),
    }];
    install(&mut h, vec![track], ops, plan);
    let out = render(&mut engine, 4096);
    assert_eq!(peaks(&out), [(AT as usize + 128, 1.0)]);
}

#[test]
fn a_plan_with_a_delay_no_line_can_provide_is_refused() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![delayed(BusIn::post_mixer(0), 9000)]),
    ]);
    let t = TrackId::new();
    install(&mut h, vec![source(t, &[])], vec![lines(t, 16, 0, 0)], plan);
    render(&mut engine, BLOCK);
    assert!(!engine.has_compiled_plan(), "the flat plan runs instead");
}

#[test]
fn the_metronome_waits_for_the_tracks() {
    let onset = |click_delay: u32| {
        let (mut engine, mut h) = create_engine(SR, BLOCK);
        let plan = RenderPlan {
            click_delay,
            ..chain_plan(vec![step(0, vec![])])
        };
        let mut ops = Vec::new();
        if click_delay > 0 {
            ops.push(GraphOp::SetClickLine(StereoLine::new(click_delay, BLOCK)));
        }
        let txn = GraphTxn::new(ops, Some(plan));
        assert!(h.send(EngineCommand::Graph(txn)).is_ok());
        // Let the delay fade in from the plan change before anything sounds.
        render(&mut engine, BLOCK * 4);
        let cmds = [
            EngineCommand::Transport(TransportCmd::SetMetronome(true)),
            EngineCommand::Transport(TransportCmd::Play),
        ];
        for c in cmds {
            assert!(h.send(c).is_ok());
        }
        render(&mut engine, BLOCK * 16)
            .iter()
            .position(|s| s.abs() > 1e-6)
            .expect("a click")
    };
    let straight = onset(0);
    assert_eq!(onset(777), straight + 777);
}

/// A steady tone through a planned delay, and the delay changing while it plays.
#[test]
fn a_new_plan_while_playing_only_ever_crossfades() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let t = TrackId::new();
    let mut track = Box::new(TrackNode::new(t, BLOCK));
    struct Sine(u64);
    impl Processor for Sine {
        fn activate(&mut self, _: f64, _: usize) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            for i in 0..ctx.audio.left.len() {
                let s = (self.0 as f32 * 2.0 * std::f32::consts::PI * 440.0 / SR as f32).sin();
                ctx.audio.left[i] += 0.5 * s;
                ctx.audio.right[i] += 0.5 * s;
                self.0 += 1;
            }
        }
        fn set_param(&mut self, _: u32, _: f64) {}
    }
    track.devices.push(slot(Box::new(Sine(0))));
    let plan = |frames: u32| {
        chain_plan(vec![
            step(0, vec![]),
            step(1, vec![delayed(BusIn::post_mixer(0), frames)]),
        ])
    };
    install(&mut h, vec![track], vec![lines(t, 5000, 0, 0)], plan(100));
    render(&mut engine, BLOCK * 8);
    let txn = GraphTxn::new(vec![], Some(plan(2000)));
    assert!(h.send(EngineCommand::Graph(txn)).is_ok());
    let (out, _) = rlive_testkit::render_chunks(FADE_FRAMES as usize * 4, BLOCK, |l, r| {
        assert_no_alloc(|| engine.process_stereo(l, r))
    });
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak_abs(&out) <= 0.5 * 1.05);
    let max_step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0f32, f32::max);
    let tone_step = 0.5 * 2.0 * std::f32::consts::PI * 440.0 / SR as f32;
    assert!(
        max_step < tone_step * 2.5,
        "no click: step {max_step} against {tone_step} for the tone itself"
    );
}

fn peak_abs(s: &[f32]) -> f32 {
    s.iter().fold(0f32, |m, x| m.max(x.abs()))
}

#[test]
fn a_growing_line_loses_nothing_of_what_it_held() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, b) = (TrackId::new(), TrackId::new());
    let plan = |frames: u32| {
        chain_plan(vec![
            step(0, vec![]),
            step(1, vec![]),
            step(
                2,
                vec![
                    delayed(BusIn::post_mixer(0), 0),
                    delayed(BusIn::post_mixer(1), frames),
                ],
            ),
        ])
    };
    install(
        &mut h,
        vec![source(a, &[]), source(b, &[])],
        vec![lines(b, 64, 0, 0)],
        plan(64),
    );
    // The click from B is already 200 frames into a line that's about to be swapped.
    render(&mut engine, AT as usize + 200);
    let txn = GraphTxn::new(vec![lines(b, 4096, 0, 0)], Some(plan(64)));
    assert!(h.send(EngineCommand::Graph(txn)).is_ok());
    let out = render(&mut engine, 2048);
    assert!(engine.has_compiled_plan());
    assert!(peaks(&out).is_empty(), "nothing replays, nothing is lost");
}

#[test]
fn changing_plans_and_lines_never_allocate() {
    use rlive_testkit::assert_no_alloc;
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, b) = (TrackId::new(), TrackId::new());
    let plan = |x: u32, y: u32| {
        chain_plan(vec![
            step(0, vec![]),
            step(1, vec![]),
            step(
                2,
                vec![
                    delayed(BusIn::post_mixer(0), x),
                    delayed(BusIn::post_mixer(1), y),
                ],
            ),
        ])
    };
    install(
        &mut h,
        vec![source(a, &[]), source(b, &[])],
        vec![lines(a, 3000, 0, 0), lines(b, 3000, 0, 0)],
        plan(0, 0),
    );
    let mut rng = 0x1234_5678u32;
    let mut next = || {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        rng >> 16
    };
    for round in 0..200 {
        // Built here, the way the UI thread would, then applied without allocating.
        let (x, y) = (next() % 3000, next() % 3000);
        let mut ops = Vec::new();
        if round % 7 == 0 {
            ops.push(lines(a, 3000, 0, 0));
        }
        let txn = GraphTxn::new(ops, Some(plan(x, y)));
        assert!(h.send(EngineCommand::Graph(txn)).is_ok());
        let frames = (next() % BLOCK as u32) as usize + 1;
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
        h.collect_garbage();
    }
    assert!(engine.has_compiled_plan());
}
