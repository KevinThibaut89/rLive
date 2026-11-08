//! Offline rendering to WAV files.

use super::graph::compile_with;
use super::timing::{compensation_ops, RuntimeLatency, Timing};
use super::{Builder, Session};
use rlive_core::Quantize;
use rlive_engine::shared::TrackShared;
use rlive_engine::{EngineCommand, GraphOp, GraphTxn, LaunchCmd, TransportCmd, MAX_TRACKS};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

impl Session {
    // ------------------------------------------------------------- export

    /// Render `bars` bars of `scene` offline into a WAV file at the engine's sample rate.
    /// Plugins are instantiated afresh (with their current state) for the render.
    pub fn export_scene(&mut self, scene: usize, bars: u32, path: &Path) -> Result<(), String> {
        let ticks = bars as u64 * self.project.time_signature.ticks_per_bar();
        self.render_offline(
            vec![EngineCommand::Launch(LaunchCmd::LaunchScene(scene))],
            ticks,
            path,
        )
    }

    /// Render the Arrangement to a WAV file: the loop range if looping, otherwise
    /// from the start to the end of the last clip.
    pub fn export_arrangement(&mut self, path: &Path) -> Result<(), String> {
        let a = &self.project.arrangement;
        let (start, end) = if a.loop_enabled {
            (a.loop_start, a.loop_end)
        } else {
            (0, self.project.arrangement_end())
        };
        if end <= start {
            return Err("the Arrangement is empty".into());
        }
        let setup = vec![
            EngineCommand::Transport(TransportCmd::SetLoop(None)),
            EngineCommand::Transport(TransportCmd::Seek(start as f64)),
            EngineCommand::Transport(TransportCmd::Play),
        ];
        self.render_offline(setup, end - start, path)
    }

    /// Renders `ticks` of the project on a private engine, after `setup` commands.
    fn render_offline(
        &mut self,
        setup: Vec<EngineCommand>,
        ticks: u64,
        path: &Path,
    ) -> Result<(), String> {
        self.capture_plugin_states();
        let sample_rate = self.engine.sample_rate;
        let max_block = 512;
        let (mut engine, mut handle) = rlive_engine::create_engine(sample_rate, max_block);
        let mut runtimes = HashMap::new();
        let mut layouts = HashMap::new();
        let mut errors = Vec::new();
        let mut b = Builder {
            sample_rate,
            max_block,
            plugins: &self.plugins,
            samples: &mut self.samples,
        };
        let p = &self.project;
        let mut tracks = Vec::with_capacity(MAX_TRACKS);
        let returns: Vec<_> = p.returns.iter().map(|t| t.id).collect();
        for t in p.tracks.iter().chain(&p.returns) {
            tracks.push(b.track(
                t,
                &returns,
                Arc::new(TrackShared::default()),
                &mut runtimes,
                &mut layouts,
                &mut errors,
            ));
        }
        let master = b.master(p, &mut runtimes, &mut layouts, &mut errors);
        let maps = super::build::time_maps(p);
        let timing = Timing::measure(p, sample_rate, &RuntimeLatency(&runtimes));
        let plan = compile_with(p, &timing);
        // The render starts this much late, and is one this much longer, for the audio that
        // leaves the Master in its first frames.
        let lead_in = plan.output_latency as usize;
        let mut ops = vec![GraphOp::ReplaceAll {
            tracks: Box::new(tracks),
            master,
        }];
        ops.extend(compensation_ops(&plan, &timing, p, max_block));
        let mut cmds = vec![
            EngineCommand::Graph(GraphTxn::new(ops, Some(plan))),
            EngineCommand::Transport(TransportCmd::SetTimeMaps(Arc::new(maps.clone()))),
            EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::None)),
        ];
        cmds.extend(setup);
        for c in cmds {
            if handle.send(c).is_err() {
                return Err("engine queue full".into());
            }
        }
        let seconds = if p.arrangement.tempo.points.is_empty() {
            ticks as f64 / rlive_core::PPQ as f64 * 60.0 / p.tempo
        } else {
            maps.tempo.tick_to_seconds(ticks as f64)
        };
        let frames = (seconds * sample_rate) as usize + lead_in;
        let mut left = vec![0.0; frames];
        let mut right = vec![0.0; frames];
        for (l, r) in left.chunks_mut(max_block).zip(right.chunks_mut(max_block)) {
            engine.process_stereo(l, r);
            handle.collect_garbage();
        }
        left.drain(..lead_in);
        right.drain(..lead_in);
        rlive_devices::write_wav(path, &left, &right, sample_rate as u32)
            .map_err(|e| e.to_string())?;
        drop(engine);
        drop(runtimes);
        Ok(())
    }
}
