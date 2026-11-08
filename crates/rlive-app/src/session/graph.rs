//! Compiles the project's routing into the plan the engine renders from, and sends structural
//! edits to the engine together with that plan.
//!
//! Routing, delay compensation and multi-core ordering all come down to producing a different
//! [`RenderPlan`] here. For now the plan is what the engine has always done: every track renders,
//! then the Master sums them all.

use super::Session;
use rlive_engine::{BusIn, EngineCommand, GraphOp, GraphTxn, RenderPlan, Step, MASTER_STEP};
use rlive_project::Project;

/// The render plan for a project: every track, summed to the Master in track order.
pub fn compile_plan(project: &Project) -> RenderPlan {
    let tracks = project.tracks.len() as u32;
    let mut steps: Vec<Step> = (0..tracks)
        .map(|track| Step {
            track,
            inputs: Vec::new(),
        })
        .collect();
    steps.push(Step {
        track: MASTER_STEP,
        inputs: (0..tracks).map(BusIn::post_mixer).collect(),
    });
    RenderPlan {
        steps,
        levels: vec![0..tracks, tracks..tracks + 1],
        buses: 0,
        sidechains: Vec::new(),
    }
}

impl Session {
    /// Apply edits to devices and chains, all at once. Only for edits within tracks: the list of
    /// tracks must not change, so the running plan still applies.
    pub(super) fn send_edits(&mut self, ops: Vec<GraphOp>) {
        self.send(EngineCommand::Graph(GraphTxn::new(ops, None)));
    }

    /// Apply edits that change the list of tracks, along with the plan for the result. Call after
    /// the project reflects them.
    pub(super) fn send_track_edits(&mut self, ops: Vec<GraphOp>) {
        let plan = compile_plan(&self.project);
        self.send(EngineCommand::Graph(GraphTxn::new(ops, Some(plan))));
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
        assert_eq!(steps, [0, 1, 2, MASTER_STEP]);
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
        assert_eq!(plan.steps[0].track, MASTER_STEP);
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
