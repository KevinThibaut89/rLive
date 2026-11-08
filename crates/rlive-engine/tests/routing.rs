//! Groups, return tracks and sends: what the plan routes, at the sample.

use rlive_core::{DeviceId, ProcessContext, Processor, TrackId};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Adds a constant to its input, so a graph's output is known exactly.
struct Dc(f32);

impl Processor for Dc {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for s in ctx.audio.left.iter_mut().chain(ctx.audio.right.iter_mut()) {
            *s += self.0;
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn track_of(id: TrackId, level: f32) -> Box<TrackNode> {
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.push(Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(Dc(level)),
    )));
    track
}

fn return_of(id: TrackId, level: f32) -> Box<TrackNode> {
    let mut t = track_of(id, level);
    *t = TrackNode::new(id, BLOCK).with_feedback(BLOCK);
    if level != 0.0 {
        t.devices.push(Box::new(DeviceSlot::new(
            DeviceId::new(),
            Box::new(Dc(level)),
        )));
    }
    t
}

fn send(h: &mut EngineHandle, cmd: EngineCommand) {
    assert!(h.send(cmd).is_ok(), "command queue full");
}

fn step(track: u32, inputs: Vec<BusIn>) -> Step {
    Step { track, inputs }
}

/// A plan with one step per level, in the order given: the Master must be last.
fn chain_plan(steps: Vec<Step>) -> RenderPlan {
    let levels = (0..steps.len() as u32).map(|i| i..i + 1).collect();
    RenderPlan {
        steps,
        levels,
        ..Default::default()
    }
}

fn set_cell(h: &mut EngineHandle, track: TrackId, cell: CellIx, value: f64) {
    send(
        h,
        EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(track),
            cell,
            value,
            gesture: true,
        }),
    );
}

fn install(h: &mut EngineHandle, tracks: Vec<Box<TrackNode>>, plan: RenderPlan) {
    let mut ops = Vec::new();
    for (index, track) in tracks.into_iter().enumerate() {
        ops.push(GraphOp::AddTrack { index, track });
    }
    send(h, EngineCommand::Graph(GraphTxn::new(ops, Some(plan))));
}

/// The left channel once every gain ramp has settled.
fn settled(engine: &mut Engine) -> f32 {
    let (l, _) =
        rlive_testkit::render_chunks(BLOCK * 64, BLOCK, |l, r| engine.process_stereo(l, r));
    *l.last().unwrap()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-3 * (1.0 + b.abs())
}

#[test]
fn a_group_sums_its_children_and_applies_its_own_mixer() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, b, g) = (TrackId::new(), TrackId::new(), TrackId::new());
    let mut group = TrackNode::new(g, BLOCK);
    group.mix.volume_db = -6.0;
    let mut layout = BusLayout::for_track(&group.mix);
    group.bus = layout.build();
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![]),
        step(2, vec![BusIn::post_mixer(0), BusIn::post_mixer(1)]),
        step(3, vec![BusIn::post_mixer(2)]),
    ]);
    let tracks = vec![track_of(a, 0.1), track_of(b, 0.2), Box::new(group)];
    install(&mut h, tracks, plan);
    let got = settled(&mut engine);
    let expect = (0.1 + 0.2) * 0.501_187_2;
    assert!(close(got, expect), "{got} vs {expect}");
}

#[test]
fn groups_nest_level_by_level() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, b, inner, outer) = (
        TrackId::new(),
        TrackId::new(),
        TrackId::new(),
        TrackId::new(),
    );
    let tracks = vec![
        track_of(a, 0.1),
        track_of(b, 0.2),
        track_of(inner, 0.4),
        track_of(outer, 0.8),
    ];
    // a → inner → outer → Main, b → outer. Each adds its own DC after summing its inputs.
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![]),
        step(2, vec![BusIn::post_mixer(0)]),
        step(3, vec![BusIn::post_mixer(1), BusIn::post_mixer(2)]),
        step(4, vec![BusIn::post_mixer(3)]),
    ]);
    install(&mut h, tracks, plan);
    let got = settled(&mut engine);
    assert!(close(got, 0.1 + 0.2 + 0.4 + 0.8), "{got}");
}

#[test]
fn a_send_taps_a_track_after_its_mixer() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (t, r) = (TrackId::new(), TrackId::new());
    let mut source = track_of(t, 0.5);
    source.seed_send(0, -6.0);
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![BusIn::send(0, mix_cell::send(0), false)]),
        step(2, vec![BusIn::post_mixer(0), BusIn::post_mixer(1)]),
    ]);
    install(&mut h, vec![source, return_of(r, 0.0)], plan);
    let got = settled(&mut engine);
    let expect = 0.5 + 0.5 * 0.501_187_2;
    assert!(close(got, expect), "{got} vs {expect}");
}

#[test]
fn a_pre_fader_send_ignores_the_tracks_volume_and_activator() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (t, r) = (TrackId::new(), TrackId::new());
    let mut source = track_of(t, 0.5);
    source.seed_send(0, 0.0);
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![BusIn::send(0, mix_cell::send(0), true)]),
        step(2, vec![BusIn::post_mixer(0), BusIn::post_mixer(1)]),
    ]);
    install(&mut h, vec![source, return_of(r, 0.0)], plan);
    settled(&mut engine);

    // The track is pulled down: only the send is left, and it hasn't moved.
    set_cell(&mut h, t, mix_cell::VOLUME, -96.0);
    let got = settled(&mut engine);
    assert!(close(got, 0.5), "volume at -inf: {got}");
    // Off entirely, same result: the tap is before the activator.
    set_cell(&mut h, t, mix_cell::VOLUME, 0.0);
    set_cell(&mut h, t, mix_cell::MUTE, 1.0);
    let got = settled(&mut engine);
    assert!(close(got, 0.5), "muted: {got}");
}

#[test]
fn a_post_fader_send_follows_the_tracks_volume_and_activator() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (t, r) = (TrackId::new(), TrackId::new());
    let mut source = track_of(t, 0.5);
    source.seed_send(0, 0.0);
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![BusIn::send(0, mix_cell::send(0), false)]),
        step(2, vec![BusIn::post_mixer(0), BusIn::post_mixer(1)]),
    ]);
    install(&mut h, vec![source, return_of(r, 0.0)], plan);
    assert!(close(settled(&mut engine), 1.0));
    set_cell(&mut h, t, mix_cell::MUTE, 1.0);
    assert!(close(settled(&mut engine), 0.0), "muted");
}

#[test]
fn moving_a_send_is_smoothed_not_stepped() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (t, r) = (TrackId::new(), TrackId::new());
    let source = track_of(t, 1.0);
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![BusIn::send(0, mix_cell::send(0), false)]),
        step(2, vec![BusIn::post_mixer(0), BusIn::post_mixer(1)]),
    ]);
    install(&mut h, vec![source, return_of(r, 0.0)], plan);
    settled(&mut engine);
    set_cell(&mut h, t, mix_cell::send(0), 0.0);
    let (l, _) = rlive_testkit::render_chunks(BLOCK, BLOCK, |l, r| engine.process_stereo(l, r));
    assert!(l[0] < 1.1, "the first sample barely moves: {}", l[0]);
    assert!(l[BLOCK - 1] > 1.01, "and it rises from there");
    let steps = l.windows(2).map(|w| (w[1] - w[0]).abs());
    assert!(steps.fold(0f32, f32::max) < 0.02, "no step");
    assert!(close(settled(&mut engine), 2.0));
}

#[test]
fn a_feedback_send_hears_a_fixed_time_ago_and_never_blows_up() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let r = TrackId::new();
    let mut ret = *return_of(r, 0.25);
    ret.seed_send(0, -6.0);
    let mut fb = BusIn::send(0, mix_cell::send(0), false);
    fb.feedback = true;
    let plan = chain_plan(vec![step(0, vec![fb]), step(1, vec![BusIn::post_mixer(0)])]);
    install(&mut h, vec![Box::new(ret)], plan);
    let (l, _) =
        rlive_testkit::render_chunks(BLOCK * 400, BLOCK, |l, r| engine.process_stereo(l, r));
    let last = *l.last().unwrap();
    // x = 0.25 + x·g settles at 0.25 / (1 - g).
    let g = 0.501_187_2;
    assert!(close(last, 0.25 / (1.0 - g)), "{last}");
    assert!(l.iter().all(|s| s.is_finite()));
    assert!(
        (l[BLOCK - 1] - 0.25).abs() < 1e-4,
        "the first block has nothing to feed back: {}",
        l[BLOCK - 1]
    );
}

#[test]
fn a_return_feeding_a_later_return_is_an_ordinary_edge() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r1, r2) = (TrackId::new(), TrackId::new());
    let mut a = *return_of(r1, 0.5);
    a.seed_send(1, 0.0);
    let plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![BusIn::send(0, mix_cell::send(1), false)]),
        step(2, vec![BusIn::post_mixer(0), BusIn::post_mixer(1)]),
    ]);
    install(&mut h, vec![Box::new(a), return_of(r2, 0.0)], plan);
    let got = settled(&mut engine);
    assert!(close(got, 1.0), "{got}");
}

struct Rig {
    engine: Engine,
    h: EngineHandle,
    clip: TrackId,
    other: TrackId,
    group: TrackId,
    child: TrackId,
    ret: TrackId,
}

/// `clip` and `other` straight to Main, `group → [child]`, and a return `other` and `clip` send
/// to at unity. Levels are distinct powers of two, so the output says who is audible.
fn rig(solo_in_place: bool) -> Rig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let (clip, other, group, child, ret) = (
        TrackId::new(),
        TrackId::new(),
        TrackId::new(),
        TrackId::new(),
        TrackId::new(),
    );
    let mut t0 = track_of(clip, 0.1);
    t0.seed_send(0, 0.0);
    let mut t1 = track_of(other, 0.2);
    t1.seed_send(0, 0.0);
    let tracks = vec![
        t0,
        t1,
        track_of(group, 0.0),
        track_of(child, 0.4),
        return_of(ret, 0.0),
    ];
    let mut plan = chain_plan(vec![
        step(0, vec![]),
        step(1, vec![]),
        step(3, vec![]),
        step(2, vec![BusIn::post_mixer(3)]),
        step(
            4,
            vec![
                BusIn::send(0, mix_cell::send(0), false),
                BusIn::send(1, mix_cell::send(0), false),
            ],
        ),
        step(
            5,
            vec![
                BusIn::post_mixer(0),
                BusIn::post_mixer(1),
                BusIn::post_mixer(2),
                BusIn::post_mixer(4),
            ],
        ),
    ]);
    plan.groups = vec![None, None, None, Some(2), None];
    plan.returns = 4..5;
    plan.solo_in_place = solo_in_place;
    install(&mut h, tracks, plan);
    Rig {
        engine,
        h,
        clip,
        other,
        group,
        child,
        ret,
    }
}

impl Rig {
    fn solo(&mut self, track: TrackId, on: bool) {
        set_cell(&mut self.h, track, mix_cell::SOLO, f64::from(on as u8));
    }

    fn output(&mut self) -> f32 {
        settled(&mut self.engine)
    }
}

#[test]
fn with_no_solo_everything_is_heard() {
    let mut r = rig(true);
    let all = 0.1 + 0.2 + 0.4 + 0.1 + 0.2;
    assert!(close(r.output(), all));
}

#[test]
fn soloing_a_track_mutes_the_others_and_its_return_with_solo_in_place_off() {
    let mut r = rig(false);
    r.solo(r.clip, true);
    let got = r.output();
    assert!(close(got, 0.1), "only the track itself: {got}");
}

#[test]
fn solo_in_place_leaves_the_returns_and_the_soloed_tracks_send() {
    let mut r = rig(true);
    r.solo(r.clip, true);
    let got = r.output();
    assert!(close(got, 0.1 + 0.1), "its send through the return: {got}");
    let _ = r.other;
}

#[test]
fn soloing_a_return_mutes_mains_but_keeps_what_arrives_at_it() {
    let mut r = rig(true);
    r.solo(r.ret, true);
    let got = r.output();
    assert!(close(got, 0.1 + 0.2), "both sends, no main outputs: {got}");
}

#[test]
fn soloing_a_child_keeps_its_group_open_and_mutes_the_rest() {
    let mut r = rig(false);
    r.solo(r.child, true);
    let got = r.output();
    assert!(close(got, 0.4), "{got}");
}

#[test]
fn soloing_a_group_keeps_everything_in_it() {
    let mut r = rig(false);
    r.solo(r.group, true);
    let got = r.output();
    assert!(close(got, 0.4), "{got}");
}

#[test]
fn clearing_a_solo_brings_everything_back() {
    let mut r = rig(false);
    r.solo(r.child, true);
    r.output();
    r.solo(r.child, false);
    let all = 0.1 + 0.2 + 0.4 + 0.1 + 0.2;
    assert!(close(r.output(), all));
}

#[test]
fn soloing_two_tracks_hears_both() {
    let mut r = rig(false);
    r.solo(r.clip, true);
    r.solo(r.other, true);
    let got = r.output();
    assert!(close(got, 0.1 + 0.2), "{got}");
}

#[test]
fn a_plan_with_a_loop_or_an_unready_input_is_refused() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, b) = (TrackId::new(), TrackId::new());
    let mut good = chain_plan(vec![step(0, vec![]), step(1, vec![]), step(2, vec![])]);
    good.steps[2] = step(2, vec![BusIn::post_mixer(0), BusIn::post_mixer(1)]);
    install(&mut h, vec![track_of(a, 0.1), track_of(b, 0.2)], good);
    assert!(close(settled(&mut engine), 0.3));
    assert!(engine.has_compiled_plan());

    // b reads a, in the same level: a isn't rendered yet.
    let bad = RenderPlan {
        steps: vec![
            step(0, vec![]),
            step(1, vec![BusIn::post_mixer(0)]),
            step(2, vec![BusIn::post_mixer(1)]),
        ],
        levels: vec![0..2, 2..3],
        ..Default::default()
    };
    send(
        &mut h,
        EngineCommand::Graph(GraphTxn::new(vec![], Some(bad))),
    );
    settled(&mut engine);
    assert!(!engine.has_compiled_plan(), "falls back to the flat plan");
    assert!(close(settled(&mut engine), 0.3), "every track to Main");
}

/// Everything a plan can ask for, repeatedly replaced under the allocation guard.
#[test]
fn structural_edits_while_routing_never_allocate_and_stay_finite() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound.max(1) as u64) as usize
    };
    let mut ids: Vec<TrackId> = Vec::new();
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    for round in 0..300 {
        // A random edit, built off the audio thread.
        let mut ops = Vec::new();
        match next(5) {
            0 if ids.len() < 12 => {
                let id = TrackId::new();
                let index = next(ids.len() + 1);
                ids.insert(index, id);
                ops.push(GraphOp::AddTrack {
                    index,
                    track: Box::new((*return_of(id, 0.01)).with_shared(Default::default())),
                });
            }
            1 if ids.len() > 1 => {
                let gone = ids.remove(next(ids.len()));
                ops.push(GraphOp::RemoveTrack(gone));
            }
            2 if ids.len() > 1 => {
                let at = next(ids.len());
                let to = next(ids.len());
                let id = ids.remove(at);
                ids.insert(to, id);
                ops.push(GraphOp::MoveTrack { track: id, to });
            }
            _ => {}
        }
        // A random valid plan over what is there: a chain of random inputs, with feedback sends.
        let n = ids.len();
        let mut steps: Vec<Step> = (0..n as u32).map(|i| step(i, vec![])).collect();
        for (i, step) in steps.iter_mut().enumerate() {
            for _ in 0..next(3) {
                let from = next(n);
                let cell = mix_cell::send(next(MAX_SENDS));
                let pre = next(2) == 0;
                let mut input = if from < i && next(2) == 0 {
                    BusIn::post_mixer(from as u32)
                } else {
                    BusIn::send(from as u32, cell, pre)
                };
                input.feedback = from >= i;
                step.inputs.push(input);
            }
        }
        // Inputs only run forward by construction: every step is its own level.
        steps.push(step(
            n as u32,
            (0..n as u32).map(BusIn::post_mixer).collect(),
        ));
        let levels = (0..steps.len() as u32).map(|i| i..i + 1).collect();
        let plan = RenderPlan {
            steps,
            levels,
            returns: 0..n as u32,
            solo_in_place: round % 2 == 0,
            ..Default::default()
        };
        send(&mut h, EngineCommand::Graph(GraphTxn::new(ops, Some(plan))));
        if n > 0 {
            set_cell(
                &mut h,
                ids[next(n)],
                if next(2) == 0 {
                    mix_cell::send(next(MAX_SENDS))
                } else {
                    mix_cell::SOLO
                },
                next(2) as f64 * -6.0,
            );
        }
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
        assert!(l.iter().chain(&r).all(|s| s.is_finite()), "round {round}");
        h.collect_garbage();
    }
}
