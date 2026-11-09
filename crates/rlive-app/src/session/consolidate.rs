//! Consolidate (manual p. 182) and Consolidate Time to New Scene (p. 199).
//!
//! Audio clips are rendered as the warp engine gives them, ahead of the track's devices and
//! mixer: Warp, the clip's own gain and its envelopes are all in the result. The file is
//! normalized and its clip turned down to match (p. 995). MIDI clips just join up.

use super::bounce::audio_clip;
use super::render::{
    from_tick, keep_material, post_fx_variant, pre_fx_variant, Keep, Processed, Render, Rendered,
};
use super::Session;
use rlive_core::TrackId;
use rlive_project::{ArrangementClip, Clip, ClipContent, FrozenAudio, NoteData, TrackKind};

/// What consolidating one track makes.
enum Piece {
    Midi(Box<Clip>),
    Audio { name: String, color: [u8; 3] },
}

struct Task {
    track: TrackId,
    /// The take lane this is on. The main lane if none.
    lane: Option<usize>,
    piece: Piece,
    /// A frozen track has its frozen sound for this stretch rendered too.
    frozen: bool,
}

impl Session {
    /// The clips of a track inside a span, without frozen tails.
    fn span_clips(
        &self,
        track: TrackId,
        lane: Option<usize>,
        start: u64,
        end: u64,
    ) -> Vec<ArrangementClip> {
        let Some(clips) = self.project.track(track).and_then(|t| t.lane(lane)) else {
            return Vec::new();
        };
        rlive_project::arrangement_edit::copy_lane_span(clips, start, end)
            .into_iter()
            .filter(|c| !c.clip.frozen_tail)
            .collect()
    }

    /// Whether there is anything to consolidate in a selection.
    pub fn can_consolidate(&self, tracks: &[TrackId], start: u64, end: u64) -> bool {
        let lanes: Vec<(TrackId, Option<usize>)> = tracks.iter().map(|t| (*t, None)).collect();
        self.can_consolidate_lanes(&lanes, start, end)
    }

    /// Whether there is anything to consolidate on these lanes.
    pub fn can_consolidate_lanes(
        &self,
        lanes: &[(TrackId, Option<usize>)],
        start: u64,
        end: u64,
    ) -> bool {
        end > start
            && lanes.iter().any(|(t, lane)| {
                self.project
                    .track(*t)
                    .is_some_and(|tr| matches!(tr.kind, TrackKind::Midi | TrackKind::Audio))
                    && !self.span_clips(*t, *lane, start, end).is_empty()
            })
    }

    /// Consolidates what's in `[start, end)` of each track into one clip. Starts a job.
    pub fn consolidate(&mut self, tracks: &[TrackId], start: u64, end: u64) -> Result<(), String> {
        let lanes: Vec<(TrackId, Option<usize>)> = tracks.iter().map(|t| (*t, None)).collect();
        self.consolidate_lanes(&lanes, start, end)
    }

    /// Consolidates what's in `[start, end)` of each lane, and its linked lanes, into one clip.
    /// Starts a job.
    pub fn consolidate_lanes(
        &mut self,
        lanes: &[(TrackId, Option<usize>)],
        start: u64,
        end: u64,
    ) -> Result<(), String> {
        let lanes = self.linked_lanes(lanes);
        let (renders, tasks) = self.plan_consolidate(&lanes, start, end)?;
        self.start_render_job(
            "Consolidate",
            renders,
            false,
            Box::new(move |s, audio| s.finish_consolidate(tasks, audio, start, end)),
        )
    }

    /// One new clip per track in a new scene, from `[start, end)` of the Arrangement. Starts a job.
    pub fn consolidate_time_to_new_scene(
        &mut self,
        below: usize,
        start: u64,
        end: u64,
    ) -> Result<(), String> {
        let lanes: Vec<(TrackId, Option<usize>)> =
            self.project.tracks.iter().map(|t| (t.id, None)).collect();
        let (renders, tasks) = self.plan_consolidate(&lanes, start, end)?;
        self.start_render_job(
            "Consolidate Time to New Scene",
            renders,
            false,
            Box::new(move |s, audio| s.finish_to_scene(tasks, audio, below, start, end)),
        )
    }

    fn plan_consolidate(
        &mut self,
        lanes: &[(TrackId, Option<usize>)],
        start: u64,
        end: u64,
    ) -> Result<(Vec<Render>, Vec<Task>), String> {
        if !self.can_consolidate_lanes(lanes, start, end) {
            return Err("There are no clips in the selection to consolidate".into());
        }
        self.prepare_render();
        let (mut renders, mut tasks) = (Vec::new(), Vec::new());
        let keep = Keep::Span { start, end };
        for (track, lane) in lanes {
            let Some(t) = self.project.track(*track) else {
                continue;
            };
            let clips = self.span_clips(*track, *lane, start, end);
            let Some(first) = clips.first() else {
                continue;
            };
            let audio = clips.iter().any(ArrangementClip::is_audio);
            let frozen = t.is_frozen();
            let piece = if audio {
                let mut v = pre_fx_variant(&self.project, &[*track]);
                // A take lane renders the way an auditioned one plays: in the main lane's place.
                if let (Some(i), Some(t)) = (lane, v.track_mut(*track)) {
                    if let Some(l) = t.take_lanes.get(*i) {
                        t.arrangement = l.clips.clone();
                    }
                }
                keep_material(&mut v, &[*track], &keep);
                renders.push(Render {
                    project: v,
                    setup: from_tick(start),
                    start,
                    ticks: end - start,
                    tail: false,
                    rate: None,
                });
                Piece::Audio {
                    name: first.clip.name.clone(),
                    color: first.clip.color,
                }
            } else {
                Piece::Midi(Box::new(merge_midi(&clips, end - start)))
            };
            if frozen {
                let mut v = post_fx_variant(&self.project, &[*track]);
                keep_material(&mut v, &[*track], &keep);
                renders.push(Render {
                    project: v,
                    setup: from_tick(start),
                    start,
                    ticks: end - start,
                    tail: false,
                    rate: None,
                });
            }
            tasks.push(Task {
                track: *track,
                lane: *lane,
                piece,
                frozen,
            });
        }
        Ok((renders, tasks))
    }

    /// The clips a consolidation makes, in the order of `tasks`.
    fn consolidated_clips(
        &mut self,
        tasks: Vec<Task>,
        audio: Vec<Rendered>,
        start: u64,
        end: u64,
    ) -> Result<Vec<(TrackId, Option<usize>, Clip)>, String> {
        let mut audio = audio.into_iter();
        let mut out = Vec::with_capacity(tasks.len());
        let format = (self.record.file_type, self.record.bit_depth);
        for task in tasks {
            let mut clip = match task.piece {
                Piece::Midi(c) => *c,
                Piece::Audio { name, color } => {
                    let mut rendered = audio.next().ok_or("A render went missing")?;
                    let gain_db = normalize(&mut rendered);
                    let path =
                        self.write_processed(Processed::Consolidate, &name, &rendered, format)?;
                    let at = start as f64;
                    let markers = self.markers_for(at, 0.0, (end - start) as f64, at);
                    let mode = self.warp_defaults.mode;
                    let mut clip = audio_clip(&path, markers, name, color, end - start, true, mode);
                    if let ClipContent::Audio { gain_db: g, .. } = &mut clip.content {
                        *g = gain_db;
                    }
                    clip
                }
            };
            if task.frozen {
                let sound = audio.next().ok_or("A render went missing")?;
                let path = self.write_processed(
                    Processed::Freeze,
                    &clip.name,
                    &sound,
                    super::freeze::FORMAT,
                )?;
                let at = start as f64;
                clip.frozen = Some(Box::new(FrozenAudio {
                    file: rlive_project::FileRef::new(&path),
                    markers: self.markers_for(at, 0.0, (end - start) as f64, at),
                    length: end - start,
                    looped: false,
                    loop_start: 0,
                }));
            }
            out.push((task.track, task.lane, clip));
        }
        Ok(out)
    }

    fn finish_consolidate(
        &mut self,
        tasks: Vec<Task>,
        audio: Vec<Rendered>,
        start: u64,
        end: u64,
    ) -> Result<(), String> {
        let clips = self.consolidated_clips(tasks, audio, start, end)?;
        self.mark_edit("Consolidate");
        for (track, lane, clip) in clips {
            let Some(t) = self.project.track_mut(track) else {
                continue;
            };
            t.delete_lane_span(lane, start, end);
            t.place_on_lane(
                lane,
                ArrangementClip {
                    length: end - start,
                    ..ArrangementClip::new(clip, start)
                },
            );
            t.settle_fades();
            self.sync_lane(track);
        }
        Ok(())
    }

    fn finish_to_scene(
        &mut self,
        tasks: Vec<Task>,
        audio: Vec<Rendered>,
        below: usize,
        start: u64,
        end: u64,
    ) -> Result<(), String> {
        let clips = self.consolidated_clips(tasks, audio, start, end)?;
        let Some(scene) = self.insert_scene(below) else {
            return Err("There is no room for another scene".into());
        };
        for (track, _, mut clip) in clips {
            clip.launch = self.launch_defaults.settings();
            if let Some(entry) = self
                .project
                .track_mut(track)
                .and_then(|t| t.clips.get_mut(scene))
            {
                *entry = Some(clip);
            }
            self.send_clip(track, scene);
        }
        Ok(())
    }
}

/// Brings the loudest sample to full scale, returning the gain that puts it back (manual p. 995).
fn normalize(audio: &mut Rendered) -> f32 {
    let peak = audio
        .left
        .iter()
        .chain(&audio.right)
        .fold(0.0f32, |p, s| p.max(s.abs()));
    if peak <= 0.0 {
        return 0.0;
    }
    let scale = 1.0 / peak;
    audio
        .left
        .iter_mut()
        .chain(audio.right.iter_mut())
        .for_each(|s| *s = (*s * scale).clamp(-1.0, 1.0));
    20.0 * peak.log10()
}

/// The notes of MIDI clips placed in order in one clip of `length`. Clips placed relative to 0.
/// Each note lands wherever the clip's markers play it, every time round its loop.
fn merge_midi(clips: &[ArrangementClip], length: u64) -> Clip {
    let mut notes = Vec::new();
    for c in clips.iter().filter(|c| c.clip.active) {
        let region = c.clip.region();
        let (from, to) = (c.run_offset() as f64, (c.run_offset() + c.length) as f64);
        for n in c.clip.notes().iter().filter(|n| !n.muted) {
            let Some(first) = region.run_of(n.start as f64) else {
                continue;
            };
            let in_loop = region.looped
                && n.start as f64 >= region.loop_start
                && (n.start as f64) < region.loop_end();
            let period = region.loop_len.max(1.0);
            let mut run = first;
            while run < to {
                if run >= from {
                    let at = c.start + (run - from).round() as u64;
                    let room = (c.start + c.length).saturating_sub(at);
                    notes.push(NoteData {
                        start: at,
                        length: n.length.min(room),
                        ..n.clone()
                    });
                }
                if !in_loop {
                    break;
                }
                run += period;
            }
        }
    }
    notes.sort_by_key(|n: &NoteData| n.start);
    let first = &clips[0].clip;
    Clip {
        name: first.name.clone(),
        color: first.color,
        length: length.max(1),
        looped: true,
        content: ClipContent::midi(notes),
        ..Clip::default()
    }
}
