//! Compiles the project's routing into the plan the engine renders from, and sends structural
//! edits to the engine together with that plan.
//!
//! Routing, delay compensation and multi-core ordering all come down to producing a different
//! [`RenderPlan`] here. Delays are worked out in [`timing`](super::timing).

use super::timing::{compensation_ops, RuntimeLatency, Timing};
use super::Session;
use rlive_core::TrackId;
use rlive_engine::{
    mix_cell, BusIn, EngineCommand, GraphOp, GraphTxn, RackOuts, RenderPlan, Step, Tap, MAX_SENDS,
};
use rlive_project::{Device, DeviceKind, Project, Route, Track, TrackKind};

/// Where a track's main output goes, as an engine index. `None` for nowhere (Sends Only, or a
/// destination it has no route to). Tracks not yet routable by A5 stay on Main.
fn main_destination(project: &Project, t: &Track, master: usize) -> Option<usize> {
    match &t.routing.output {
        Route::None | Route::SendsOnly => None,
        Route::Track { track, .. } if t.kind != TrackKind::Return => {
            let target = project.track(*track).filter(|d| d.is_group());
            match target {
                Some(_) => project.track_index(*track),
                None => Some(master),
            }
        }
        Route::Default if t.kind != TrackKind::Return => match project.parent_group(t.id) {
            Some(g) => project.track_index(g),
            None => Some(master),
        },
        _ => Some(master),
    }
}

/// The render plan for a project: every track feeds its group or the Master, every clip and group
/// track feeds the returns through its sends, and tracks render after whatever feeds them. Group
/// nesting is what makes levels of a plan; the Master is always the last step.
pub fn compile_plan(project: &Project) -> RenderPlan {
    compile_with(project, &Timing::default())
}

/// [`compile_plan`] over the compensation `timing` asks for. Each input is delayed to reach its
/// destination in step with its siblings.
pub(super) fn compile_with(project: &Project, timing: &Timing) -> RenderPlan {
    let tracks = project.tracks.len();
    let returns = project.returns.len();
    let all: Vec<&Track> = project.tracks.iter().chain(&project.returns).collect();
    let master = tracks + returns;

    // Main outputs. A group can't feed back into itself, however it is routed.
    let mut outputs: Vec<Option<usize>> = vec![None; master];
    for (i, t) in all.iter().enumerate() {
        let dest = main_destination(project, t, master);
        let mut seen = dest;
        let mut hops = 0;
        let mut loops = false;
        while let Some(d) = seen.filter(|&d| d != master) {
            loops |= d == i;
            seen = outputs[d];
            hops += 1;
            if loops || hops > master {
                break;
            }
        }
        outputs[i] = if loops { None } else { dest };
    }

    let mut inputs: Vec<Vec<BusIn>> = vec![Vec::new(); master + 1];
    for (i, dest) in outputs.iter().enumerate() {
        if let Some(d) = dest {
            inputs[*d].push(BusIn::post_mixer(i as u32));
        }
    }
    // Sends, in track order, into the first MAX_SENDS returns.
    for (i, t) in all.iter().enumerate() {
        for (k, ret) in project.returns.iter().take(MAX_SENDS).enumerate() {
            if !t.send_enabled(ret.id) {
                continue;
            }
            let mut input = BusIn::send(i as u32, mix_cell::send(k), ret.pre_fader_sends);
            // A return feeding itself or one before it hears the past, so the graph stays acyclic.
            input.feedback = i >= tracks && k <= i - tracks;
            inputs[tracks + k].push(input);
        }
    }

    // Rack return chains that feed return tracks directly. A return track may only feed one
    // after it, so a chain of these can never loop.
    for (i, t) in all.iter().enumerate() {
        for (slot, target) in rack_out_targets(t).into_iter().enumerate() {
            let Some(k) = project.return_index(target).filter(|k| *k < MAX_SENDS) else {
                continue;
            };
            if i >= tracks && k <= i - tracks {
                continue;
            }
            inputs[tracks + k].push(BusIn {
                from: i as u32,
                tap: Tap::Aux(rlive_core::DeviceId(0), slot as u8),
                gain: None,
                delay: 0,
                feedback: false,
            });
        }
    }

    // Each track's depth: 0 for a track fed by nothing, and one past whatever feeds it.
    let mut depth = vec![0u32; master + 1];
    let mut known = vec![false; master + 1];
    fn depth_of(i: usize, inputs: &[Vec<BusIn>], depth: &mut [u32], known: &mut [bool]) -> u32 {
        if known[i] {
            return depth[i];
        }
        known[i] = true;
        let d = inputs[i]
            .iter()
            .filter(|input| !input.feedback)
            .map(|input| depth_of(input.from as usize, inputs, depth, known) + 1)
            .max()
            .unwrap_or(0);
        depth[i] = d;
        d
    }
    let last = (0..master)
        .map(|i| depth_of(i, &inputs, &mut depth, &mut known))
        .max()
        .unwrap_or(0);
    let master_depth = last + 1;
    depth[master] = master_depth;

    let mut steps = Vec::with_capacity(master + 1);
    let mut levels = Vec::with_capacity(master_depth as usize + 1);
    for level in 0..=master_depth {
        let start = steps.len() as u32;
        for i in 0..=master {
            if depth[i] == level {
                steps.push(Step {
                    track: i as u32,
                    inputs: std::mem::take(&mut inputs[i]),
                });
            }
        }
        levels.push(start..steps.len() as u32);
    }
    let aligned = timing.align(&mut steps, master);
    let groups = all
        .iter()
        .map(|t| {
            let parent = project.parent_group(t.id)?;
            let acyclic = !project.ancestors(parent).contains(&t.id);
            acyclic.then(|| project.track_index(parent).map(|g| g as u32))?
        })
        .collect();

    RenderPlan {
        steps,
        levels,
        buses: 0,
        sidechains: Vec::new(),
        groups,
        returns: tracks as u32..master as u32,
        solo_in_place: project.solo_in_place,
        click_delay: aligned.click_delay,
        output_latency: aligned.output_latency,
    }
}

/// The return tracks a track's racks feed directly, in the order of the track's rack outputs.
fn rack_out_targets(t: &Track) -> Vec<TrackId> {
    fn walk(devices: &[Device], out: &mut Vec<TrackId>) {
        for d in devices {
            if let DeviceKind::Rack { returns, .. } = &d.kind {
                for r in returns {
                    if let Some(target) = r.mix.output {
                        if !out.contains(&target) {
                            out.push(target);
                        }
                    }
                }
            }
            for c in d.chains() {
                walk(&c.devices, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&t.devices, &mut out);
    out
}

/// The room each track needs for its rack outputs, sent along with every plan that may read it.
fn rack_out_ops(project: &Project, max_block: usize) -> Vec<GraphOp> {
    project
        .tracks
        .iter()
        .chain(&project.returns)
        .filter_map(|t| {
            let targets = rack_out_targets(t);
            (!targets.is_empty()).then(|| GraphOp::SetRackOuts {
                track: t.id,
                outs: Box::new(RackOuts::new(targets, max_block)),
            })
        })
        .collect()
}

impl Session {
    /// Apply edits to devices and chains, all at once. Only for edits within tracks: the list of
    /// tracks must not change, so the running plan still applies.
    pub(super) fn send_edits(&mut self, mut ops: Vec<GraphOp>) {
        let routes = self.routes_for_bus_swaps(&ops);
        ops.extend(routes);
        let timing = self.measure_timing();
        if timing == self.timing {
            self.send(EngineCommand::Graph(GraphTxn::new(ops, None)));
        } else {
            self.send_recompiled(ops, timing);
        }
    }

    /// Apply edits that change the list of tracks, along with the plan for the result. Call after
    /// the project reflects them.
    pub(super) fn send_track_edits(&mut self, ops: Vec<GraphOp>) {
        let timing = self.measure_timing();
        self.send_recompiled(ops, timing);
    }

    fn send_recompiled(&mut self, mut ops: Vec<GraphOp>, timing: Timing) {
        let plan = compile_with(&self.project, &timing);
        ops.extend(compensation_ops(
            &plan,
            &timing,
            &self.project,
            self.engine.max_block,
        ));
        ops.extend(rack_out_ops(&self.project, self.engine.max_block));
        self.timing = timing;
        self.send(EngineCommand::Graph(GraphTxn::new(ops, Some(plan))));
    }

    /// Recompile if what compensation depends on has moved: a plug-in's latency, or anything else
    /// that doesn't pass through an edit. Call at idle.
    pub(super) fn follow_latency(&mut self) {
        let timing = self.measure_timing();
        if timing != self.timing {
            self.send_recompiled(Vec::new(), timing);
        }
    }

    pub(super) fn measure_timing(&self) -> Timing {
        Timing::measure(
            &self.project,
            self.engine.sample_rate,
            &RuntimeLatency(&self.runtimes),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_engine::{create_engine, Tap};
    use rlive_plugin_host::scanner::PluginCache;
    use rlive_project::{Track, TrackKind};

    fn project(tracks: usize) -> Project {
        let mut p = Project::empty();
        p.tracks = (0..tracks)
            .map(|i| Track::new(&i.to_string(), TrackKind::Midi, [0, 0, 0], p.scenes.len()))
            .collect();
        p
    }

    #[test]
    fn every_track_renders_once_before_the_master() {
        let plan = compile_plan(&project(3));
        let steps: Vec<u32> = plan.steps.iter().map(|s| s.track).collect();
        assert_eq!(steps, [0, 1, 2, 3]);
        assert!(plan.steps[..3].iter().all(|s| s.inputs.is_empty()));
    }

    #[test]
    fn the_master_sums_every_track_in_order_after_its_mixer() {
        let plan = compile_plan(&project(4));
        let master = plan.steps.last().unwrap();
        let from: Vec<u32> = master.inputs.iter().map(|i| i.from).collect();
        assert_eq!(from, [0, 1, 2, 3]);
        assert!(master
            .inputs
            .iter()
            .all(|i| i.tap == Tap::PostMixer && i.delay == 0 && i.gain.is_none()));
    }

    #[test]
    fn the_tracks_share_a_level_and_the_master_has_its_own() {
        let plan = compile_plan(&project(5));
        assert_eq!(plan.levels, [0..5, 5..6]);
    }

    #[test]
    fn an_empty_project_is_just_the_master() {
        let plan = compile_plan(&project(0));
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].track, 0);
        assert!(plan.steps[0].inputs.is_empty());
        assert_eq!(plan.levels, [0..0, 0..1]);
    }

    #[test]
    fn the_engine_runs_the_plan_after_every_kind_of_track_edit() {
        let (mut engine, handle) = create_engine(48_000.0, 512);
        let mut s = Session::new(handle, Project::new_default(), PluginCache::default());
        let mut block = |s: &mut Session| {
            let (mut l, mut r) = ([0.0; 512], [0.0; 512]);
            engine.process_stereo(&mut l, &mut r);
            s.idle();
            engine.has_compiled_plan()
        };
        assert!(block(&mut s), "after the first build");
        let added = s.add_track(TrackKind::Midi, Some(0));
        assert!(block(&mut s), "after adding a track");
        s.move_track(added, 2);
        assert!(block(&mut s), "after moving a track");
        s.delete_track(added);
        assert!(block(&mut s), "after deleting a track");
        s.rebuild();
        assert!(block(&mut s), "after a rebuild");
    }

    #[test]
    fn nothing_needs_a_shared_bus_or_a_sidechain_yet() {
        let plan = compile_plan(&project(2));
        assert_eq!(plan.buses, 0);
        assert!(plan.sidechains.is_empty());
    }
}
