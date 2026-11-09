//! Export: rendering the Set offline to audio files, and clips to MIDI files.

use super::render::{
    export_track_variant, from_tick, keep_material, launch_clip, Keep, Render, Rendered,
};
use super::{ClipRef, Session};
use rlive_audio_io::{analysis, BitDepth, Dither, ExportType, PcmFormat};
use rlive_browser::item::Role;
use rlive_core::TrackId;
use rlive_engine::shared::NO_SLOT;
use rlive_engine::{EngineCommand, LaunchCmd};
use rlive_project::midi_file;
use rlive_project::{ArrangementClip, Project, TrackKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// What part of the Set an export plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRange {
    /// A stretch of the timeline, plus any Session clips playing right now.
    Timeline {
        start: u64,
        length: u64,
    },
    Scene {
        scene: usize,
        bars: u32,
    },
}

/// Which signals an export writes out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RenderedTrack {
    #[default]
    Main,
    AllIndividual,
    Selected(Vec<TrackId>),
    One(TrackId),
}

/// The Export Audio/Video options.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportOptions {
    pub range: ExportRange,
    pub rendered: RenderedTrack,
    pub include_returns_and_main: bool,
    pub render_as_loop: bool,
    pub mono: bool,
    pub normalize: bool,
    pub create_analysis: bool,
    /// Hz.
    pub sample_rate: u32,
    pub pcm: Option<PcmFormat>,
    pub mp3: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            range: ExportRange::Timeline {
                start: 0,
                length: 0,
            },
            rendered: RenderedTrack::Main,
            include_returns_and_main: false,
            render_as_loop: false,
            mono: false,
            normalize: false,
            create_analysis: false,
            sample_rate: 44_100,
            pcm: Some(PcmFormat {
                kind: ExportType::Wav,
                depth: BitDepth::Bits24,
                dither: Dither::Triangular,
            }),
            mp3: false,
        }
    }
}

/// One signal being written: which track, and the files it makes.
struct Target {
    track: Option<TrackId>,
    pcm: Option<PathBuf>,
    mp3: Option<PathBuf>,
}

/// Whether a track makes sound at all: a MIDI track needs an instrument or effects to.
fn outputs_audio(t: &rlive_project::Track) -> bool {
    match t.kind {
        TrackKind::Midi => t
            .devices
            .iter()
            .any(|d| rlive_browser::preset::device_role(d) != Some(Role::MidiEffect)),
        _ => true,
    }
}

fn safe_name(name: &str) -> String {
    let file = rlive_browser::preset::file_name(name);
    Path::new(&file)
        .file_stem()
        .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned())
}

/// Mixes the tail of a render into its start, so the file loops without a jump.
fn wrap_tail(body: &mut Rendered, with_tail: &Rendered) {
    let n = body.frames();
    if n == 0 {
        return;
    }
    let tail_l = with_tail.left.iter().skip(n);
    let tail_r = with_tail.right.iter().skip(n);
    for (i, (l, r)) in tail_l.zip(tail_r).enumerate() {
        body.left[i % n] += *l;
        body.right[i % n] += *r;
    }
}

impl Session {
    // ------------------------------------------------------------- audio

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
            rate: None,
        })?;
        rlive_devices::write_wav(
            path,
            &rendered.left,
            &rendered.right,
            self.engine.sample_rate as u32,
        )
        .map_err(|e| e.to_string())
    }

    /// The range a new Export dialog starts with: the whole of the Arrangement.
    pub fn whole_set_range(&self) -> ExportRange {
        ExportRange::Timeline {
            start: 0,
            length: self
                .project
                .arrangement_end()
                .max(self.project.time_signature.ticks_per_bar()),
        }
    }

    /// The tracks an export of `rendered` writes, in order. None is the Main output.
    fn rendered_targets(&self, rendered: &RenderedTrack) -> Vec<Option<TrackId>> {
        match rendered {
            RenderedTrack::Main => vec![None],
            RenderedTrack::One(id) => vec![Some(*id)],
            RenderedTrack::Selected(ids) => self
                .project
                .tracks
                .iter()
                .chain(&self.project.returns)
                .filter(|t| ids.contains(&t.id) && outputs_audio(t))
                .map(|t| Some(t.id))
                .collect(),
            RenderedTrack::AllIndividual => self
                .project
                .tracks
                .iter()
                .chain(&self.project.returns)
                .filter(|t| outputs_audio(t))
                .map(|t| Some(t.id))
                .chain([None])
                .collect(),
        }
    }

    fn export_paths(
        &self,
        tracks: Vec<Option<TrackId>>,
        options: &ExportOptions,
        output: &Path,
    ) -> Vec<Target> {
        let stem = output
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Export".into());
        let dir = output.parent().unwrap_or(Path::new(""));
        let individual = options.rendered != RenderedTrack::Main;
        let many = individual && tracks.len() > 1;
        let mut used: Vec<String> = Vec::new();
        tracks
            .into_iter()
            .map(|track| {
                let mut name = match (many, track) {
                    (false, Some(id)) => self
                        .project
                        .track(id)
                        .map_or(stem.clone(), |t| format!("{stem} {}", safe_name(&t.name))),
                    (true, Some(id)) => self
                        .project
                        .track(id)
                        .map_or(stem.clone(), |t| format!("{stem} {}", safe_name(&t.name))),
                    (_, None) if individual => format!("{stem} Main"),
                    (_, None) => stem.clone(),
                };
                let base = name.clone();
                let mut n = 1;
                while used.contains(&name) {
                    n += 1;
                    name = format!("{base} {n}");
                }
                used.push(name.clone());
                Target {
                    track,
                    pcm: options
                        .pcm
                        .map(|f| dir.join(format!("{name}.{}", f.kind.extension()))),
                    mp3: options.mp3.then(|| dir.join(format!("{name}.mp3"))),
                }
            })
            .collect()
    }

    /// The files an export would write, so that the user can be asked before any are replaced.
    pub fn export_outputs(&self, options: &ExportOptions, output: &Path) -> Vec<PathBuf> {
        let tracks = self.rendered_targets(&options.rendered);
        self.export_paths(tracks, options, output)
            .into_iter()
            .flat_map(|t| t.pcm.into_iter().chain(t.mp3))
            .collect()
    }

    /// Commands that start every Session clip that's playing now, so the render has what's being
    /// heard, on whichever tracks it's playing on (manual p. 128).
    fn active_session_clips(&self) -> Vec<(TrackId, usize)> {
        let mut out = Vec::new();
        for t in &self.project.tracks {
            let Some(shared) = self.track_shared.get(&t.id) else {
                continue;
            };
            let slot = shared.playing_slot.load(Ordering::Relaxed);
            if slot != NO_SLOT && t.clips.get(slot as usize).is_some_and(Option::is_some) {
                out.push((t.id, slot as usize));
            }
        }
        out
    }

    /// The project as this export hears it.
    fn export_project(&self, track: Option<TrackId>, options: &ExportOptions) -> Project {
        match track {
            None => self.project.clone(),
            Some(id) => export_track_variant(&self.project, id, options.include_returns_and_main),
        }
    }

    /// Starts rendering the files of an export. They are written once the render has finished.
    pub fn start_export(
        &mut self,
        options: &ExportOptions,
        output: &Path,
    ) -> Result<Vec<PathBuf>, String> {
        if options.pcm.is_none() && !options.mp3 {
            return Err("Choose at least one of PCM and MP3".into());
        }
        if options.mp3 && !rlive_audio_io::MP3_AVAILABLE {
            return Err("This build has no MP3 encoder".into());
        }
        if options.render_as_loop && matches!(options.range, ExportRange::Scene { .. }) {
            return Err("Render as Loop needs a stretch of the timeline".into());
        }
        if let ExportRange::Timeline { length: 0, .. } = options.range {
            return Err("Nothing to render: the length is zero".into());
        }
        self.prepare_render();
        let tracks = self.rendered_targets(&options.rendered);
        if tracks.is_empty() {
            return Err("None of those tracks make any sound".into());
        }
        let targets = self.export_paths(tracks, options, output);
        let engine_rate = self.engine.sample_rate;
        let rate = options.sample_rate as f64;
        // Rendering below the Set's rate would change what effects do: it goes in a second step.
        let render_rate = (rate > engine_rate).then_some(rate);
        let produced: Vec<PathBuf> = targets
            .iter()
            .flat_map(|t| t.pcm.iter().chain(t.mp3.iter()).cloned())
            .collect();
        let mut renders = Vec::new();
        let mut realtime = false;
        let active = self.active_session_clips();
        for target in &targets {
            let project = self.export_project(target.track, options);
            realtime |= project
                .tracks
                .iter()
                .chain(&project.returns)
                .chain(std::iter::once(&project.master))
                .any(|t| super::freeze::needs_realtime(&t.devices));
            let (setup, start, ticks) = match options.range {
                ExportRange::Timeline { start, length } => {
                    let mut setup = from_tick(start);
                    for (track, slot) in &active {
                        setup.extend(launch_clip(*track, *slot));
                    }
                    (setup, start, length)
                }
                ExportRange::Scene { scene, bars } => (
                    super::render::launch_scene(scene),
                    0,
                    bars as u64 * self.project.time_signature.ticks_per_bar(),
                ),
            };
            if options.render_as_loop {
                // First the tail on its own: without what follows the range, or it never ends.
                let mut with_tail = project.clone();
                let everything: Vec<TrackId> = with_tail.tracks.iter().map(|t| t.id).collect();
                keep_material(
                    &mut with_tail,
                    &everything,
                    &Keep::Span {
                        start,
                        end: start + ticks,
                    },
                );
                renders.push(Render {
                    project: with_tail,
                    setup: from_tick(start),
                    start,
                    ticks,
                    tail: true,
                    rate: render_rate,
                });
            }
            renders.push(Render {
                project,
                setup,
                start,
                ticks,
                tail: false,
                rate: render_rate,
            });
        }
        let options = options.clone();
        self.start_render_job(
            "Export",
            renders,
            realtime,
            Box::new(move |s, audio| s.finish_export(targets, &options, audio)),
        )?;
        Ok(produced)
    }

    /// Runs an export to the end, for callers that have nothing to show in the meantime.
    pub fn export_now(
        &mut self,
        options: &ExportOptions,
        output: &Path,
    ) -> Result<Vec<PathBuf>, String> {
        let files = self.start_export(options, output)?;
        self.finish_render()?;
        Ok(files)
    }

    fn finish_export(
        &mut self,
        targets: Vec<Target>,
        options: &ExportOptions,
        audio: Vec<Rendered>,
    ) -> Result<(), String> {
        let mut audio = audio.into_iter();
        for target in targets {
            let first = audio.next().ok_or("A render went missing")?;
            let body = if options.render_as_loop {
                let mut body = audio.next().ok_or("A render went missing")?;
                wrap_tail(&mut body, &first);
                body
            } else {
                first
            };
            self.write_export(&target, options, body)?;
        }
        Ok(())
    }

    fn write_export(
        &mut self,
        target: &Target,
        options: &ExportOptions,
        mut audio: Rendered,
    ) -> Result<(), String> {
        let engine_rate = self.engine.sample_rate as u32;
        let (mut rate, wants) = (engine_rate, options.sample_rate);
        if wants > engine_rate {
            rate = wants;
        }
        if options.normalize {
            rlive_audio_io::normalize(&mut [&mut audio.left, &mut audio.right]);
        }
        if options.mono {
            for (l, r) in audio.left.iter_mut().zip(&mut audio.right) {
                let m = (*l + *r) * 0.5;
                *l = m;
                *r = m;
            }
        }
        if wants < engine_rate {
            audio.left = rlive_audio_io::resample(&audio.left, engine_rate, wants);
            audio.right = rlive_audio_io::resample(&audio.right, engine_rate, wants);
            rate = wants;
        }
        let fail = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
        if let (Some(path), Some(format)) = (&target.pcm, options.pcm) {
            let channels: Vec<&[f32]> = if options.mono {
                vec![&audio.left]
            } else {
                vec![&audio.left, &audio.right]
            };
            rlive_audio_io::write_pcm(path, &channels, rate, format).map_err(|e| fail(path, e))?;
            if options.create_analysis {
                if let Err(e) = analysis::create_for(path) {
                    self.messages
                        .push(format!("No analysis file for {}: {e}", path.display()));
                }
            }
        }
        if let Some(path) = &target.mp3 {
            rlive_audio_io::write_mp3(path, &audio.left, &audio.right, rate)
                .map_err(|e| fail(path, e))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------- MIDI

    /// Writes a MIDI clip as a Standard MIDI File, replacing nothing unless told to overwrite.
    pub fn export_midi_clip(&self, r: ClipRef, path: &Path) -> Result<(), String> {
        let clip = self.clip_at(r).ok_or("That clip is gone")?;
        if !clip.is_midi() {
            return Err("Only MIDI clips can be exported as a MIDI file".into());
        }
        let file = midi_file::from_clip(clip, self.project.tempo, self.project.time_signature);
        rlive_browser::preset::write_atomic(path, &midi_file::write(&file))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Makes a clip from a MIDI file, in a Session slot or on the timeline at `tick`.
    pub fn import_midi_file(
        &mut self,
        path: &Path,
        track: TrackId,
        into: MidiImportTarget,
    ) -> Result<ClipRef, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = midi_file::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let is_midi = self
            .project
            .track(track)
            .is_some_and(|t| t.kind == TrackKind::Midi);
        if !is_midi {
            return Err("A MIDI file can only go on a MIDI track".into());
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let color = self.new_clip_color(track);
        let clip = midi_file::to_clip(&file, &name, color, self.project.time_signature);
        match into {
            MidiImportTarget::Slot(slot) => {
                self.set_clip(track, slot, Some(clip));
                self.mark_edit("Import MIDI File");
                Ok(ClipRef::slot(track, slot))
            }
            MidiImportTarget::Timeline(tick) => {
                let placed = ArrangementClip::new(clip, tick);
                let id = placed.id;
                let t = self.project.track_mut(track).ok_or("That track is gone")?;
                t.place_clip(placed);
                self.sync_lane(track);
                self.mark_edit("Import MIDI File");
                Ok(ClipRef::Arr { track, id })
            }
        }
    }
}

/// Where an imported MIDI file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiImportTarget {
    Slot(usize),
    Timeline(u64),
}
