//! Offline rendering to WAV files.

use super::render::{from_tick, Render};
use super::Session;
use rlive_engine::{EngineCommand, LaunchCmd};
use std::path::Path;

impl Session {
    // ------------------------------------------------------------- export

    /// Render `bars` bars of `scene` offline into a WAV file at the engine's sample rate.
    /// Plugins are instantiated afresh (with their current state) for the render.
    pub fn export_scene(&mut self, scene: usize, bars: u32, path: &Path) -> Result<(), String> {
        let ticks = bars as u64 * self.project.time_signature.ticks_per_bar();
        self.render_offline(
            vec![EngineCommand::Launch(LaunchCmd::LaunchScene(scene))],
            0,
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
        self.render_offline(from_tick(start), start, end - start, path)
    }

    /// Renders `ticks` of the project on a private engine, after `setup` commands.
    fn render_offline(
        &mut self,
        setup: Vec<EngineCommand>,
        start: u64,
        ticks: u64,
        path: &Path,
    ) -> Result<(), String> {
        self.prepare_render();
        let rendered = self.render_now(Render {
            project: self.project.clone(),
            setup,
            start,
            ticks,
            tail: false,
        })?;
        rlive_devices::write_wav(
            path,
            &rendered.left,
            &rendered.right,
            self.engine.sample_rate as u32,
        )
        .map_err(|e| e.to_string())
    }
}
