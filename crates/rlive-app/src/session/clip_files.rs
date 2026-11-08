//! Clip operations that make new audio files or react to files changing: Reverse, Crop, an
//! external destructive edit, and replacing a clip's sample.
//!
//! New files are written on their own thread so a long sample doesn't hold up the UI, and go in
//! the Set's `Samples/Processed/<kind>` folder, or the temporary folder until it has been saved.

use super::takes::recording_folder;
use super::{ClipRef, Session};
use rlive_audio_io::{AudioWriter, FileType};
use rlive_core::{SampleBuffer, PPQ};
use rlive_project::clip_edit::{self as edit};
use rlive_project::warp_edit;
use rlive_project::{Clip, ClipContent, FileRef};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::SystemTime;

/// How many frames are written between progress reports.
const PROGRESS_CHUNK: usize = 16_384;
/// How often files open in an external editor are looked at, in UI frames.
const EXTERNAL_POLL_FRAMES: u32 = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum JobKind {
    Reverse,
    Crop,
}

impl JobKind {
    fn folder(self) -> &'static str {
        match self {
            Self::Reverse => "Reverse",
            Self::Crop => "Crop",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Reverse => "Reversing",
            Self::Crop => "Cropping",
        }
    }
}

/// What a job leaves to do on the clip once its file exists.
#[derive(Clone, Copy, Debug)]
enum Apply {
    Reverse,
    /// The cropped ticks of the clip, and where `from` is in the old file.
    Crop {
        from: u64,
        to: u64,
        seconds: f64,
    },
}

struct Finished {
    path: PathBuf,
    buffer: Arc<SampleBuffer>,
}

struct Job {
    kind: JobKind,
    apply: Apply,
    source: PathBuf,
    target: ClipRef,
    /// Thousandths done, written by the job's thread.
    progress: Arc<AtomicU32>,
    done: Receiver<Result<Finished, String>>,
}

/// A file that was opened in an external editor.
struct Watched {
    modified: Option<SystemTime>,
    /// Its length when last seen, to tell if the markers still fit.
    seconds: Option<f64>,
    /// The new version is being read.
    loading: bool,
}

#[derive(Default)]
pub(super) struct ClipJobs {
    running: Vec<Job>,
    /// A reversed file and its original, both ways round, so reversing again costs nothing.
    reversed: HashMap<PathBuf, PathBuf>,
    /// Files made before the Set had a folder, and the kind each is.
    temporary: Vec<(PathBuf, JobKind)>,
    watched: HashMap<PathBuf, Watched>,
    frames: u32,
    made: u64,
}

fn file_seconds_of_buffer(b: &SampleBuffer) -> f64 {
    b.len() as f64 / b.sample_rate.max(1.0)
}

/// Writes frames of `buffer`, reversed if asked, reporting how far it has got.
fn write_frames(
    path: &Path,
    file_type: FileType,
    depth: rlive_audio_io::BitDepth,
    buffer: &SampleBuffer,
    range: std::ops::Range<usize>,
    backwards: bool,
    progress: &AtomicU32,
) -> Result<Arc<SampleBuffer>, String> {
    let frames = range.len();
    let at = |i: usize| {
        if backwards {
            range.end - 1 - i
        } else {
            range.start + i
        }
    };
    let left: Vec<f32> = (0..frames).map(|i| buffer.left[at(i)]).collect();
    let right: Vec<f32> = (0..frames).map(|i| buffer.right[at(i)]).collect();
    let mut writer = AudioWriter::create(path, file_type, depth, 2, buffer.sample_rate as u32)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut interleaved = Vec::with_capacity(PROGRESS_CHUNK * 2);
    let mut done = 0;
    while done < frames {
        let end = (done + PROGRESS_CHUNK).min(frames);
        interleaved.clear();
        for i in done..end {
            interleaved.push(left[i]);
            interleaved.push(right[i]);
        }
        writer
            .write(&interleaved)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        done = end;
        progress.store((done * 1000 / frames.max(1)) as u32, Ordering::Relaxed);
    }
    writer
        .finish()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Arc::new(SampleBuffer {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        left,
        right,
        sample_rate: buffer.sample_rate,
    }))
}

impl Session {
    /// Where a clip's tick is in its file, in seconds.
    pub fn tick_seconds_of(&self, clip: &Clip, tick: f64) -> f64 {
        if let ClipContent::Audio { warp, .. } = &clip.content {
            if warp.enabled {
                if let Some(map) = warp_edit::seconds_map(warp) {
                    return map.tick_to_src(tick);
                }
            }
        }
        tick / (self.project.tempo / 60.0 * PPQ as f64)
    }

    /// The clip tick a moment of its file plays at.
    pub fn tick_of_seconds(&self, clip: &Clip, seconds: f64) -> f64 {
        if let ClipContent::Audio { warp, .. } = &clip.content {
            if warp.enabled {
                if let Some(map) = warp_edit::seconds_map(warp) {
                    return map.src_to_tick(seconds);
                }
            }
        }
        seconds * (self.project.tempo / 60.0 * PPQ as f64)
    }

    /// The file a new processed sample of this kind goes in.
    fn processed_path(&mut self, source: &Path, kind: JobKind) -> PathBuf {
        let base = recording_folder(self.path.as_deref(), self.record.temp_folder.as_deref());
        let folder = match self.path.is_some() {
            // `recording_folder` is `Samples/Recorded` once there is a Set.
            true => base
                .parent()
                .map_or(base.clone(), Path::to_path_buf)
                .join("Processed"),
            false => base.join("Processed"),
        }
        .join(kind.folder());
        self.clip_jobs.made += 1;
        let stem = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Sample".into());
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        folder.join(format!(
            "{stem} {} {stamp}-{}.{}",
            kind.folder(),
            self.clip_jobs.made,
            self.record.file_type.extension()
        ))
    }

    fn start_job(
        &mut self,
        kind: JobKind,
        apply: Apply,
        target: ClipRef,
        source: PathBuf,
        range: std::ops::Range<usize>,
    ) -> bool {
        let Some(buffer) = self.samples.get(&source) else {
            self.messages
                .push(format!("{}: not loaded yet", source.display()));
            return false;
        };
        let dest = self.processed_path(&source, kind);
        let backwards = kind == JobKind::Reverse;
        let (file_type, depth) = (self.record.file_type, self.record.bit_depth);
        let progress = Arc::new(AtomicU32::new(0));
        let thread_progress = progress.clone();
        let (tx, done) = mpsc::channel();
        std::thread::spawn(move || {
            let result = write_frames(
                &dest,
                file_type,
                depth,
                &buffer,
                range,
                backwards,
                &thread_progress,
            )
            .map(|buffer| Finished { path: dest, buffer });
            let _ = tx.send(result);
        });
        self.clip_jobs.running.push(Job {
            kind,
            apply,
            source,
            target,
            progress,
            done,
        });
        true
    }

    /// What is being written and how far along it is, 0 to 1, for the Status Bar.
    pub fn clip_job_progress(&self) -> Option<(&'static str, f32)> {
        let job = self.clip_jobs.running.first()?;
        Some((
            job.kind.label(),
            job.progress.load(Ordering::Relaxed) as f32 / 1000.0,
        ))
    }

    pub fn clip_job_running(&self) -> bool {
        !self.clip_jobs.running.is_empty()
    }

    /// The Reverse button. A file reversed before needs no new work (manual p. 217).
    pub fn reverse_clip(&mut self, r: ClipRef) -> bool {
        let Some(path) = self.audio_path(r) else {
            return false;
        };
        if let Some(cached) = self.clip_jobs.reversed.get(&path).cloned() {
            if cached.exists() || self.samples.is_ready(&cached) {
                self.samples.request(&cached);
                self.apply_reverse(r, &cached);
                return true;
            }
        }
        let Some(frames) = self.samples.get(&path).map(|b| b.len()) else {
            self.messages
                .push(format!("{}: not loaded yet", path.display()));
            return false;
        };
        self.start_job(JobKind::Reverse, Apply::Reverse, r, path, 0..frames)
    }

    fn audio_path(&self, r: ClipRef) -> Option<PathBuf> {
        match &self.clip_at(r)?.content {
            ClipContent::Audio { file, .. } => Some(file.path.clone()),
            ClipContent::Midi { .. } => None,
        }
    }

    /// Puts the reversed file in the clip, flipping its markers and regions. Clip envelopes stay
    /// where they are in time.
    fn apply_reverse(&mut self, r: ClipRef, reversed: &Path) {
        let Some(clip) = self.clip_at(r) else {
            return;
        };
        let seconds = self.file_seconds_of(clip).unwrap_or(0.0);
        let map = match &clip.content {
            ClipContent::Audio { warp, .. } if warp.enabled => warp_edit::seconds_map(warp),
            _ => None,
        };
        let ticks_per_second = self.project.tempo / 60.0 * PPQ as f64;
        // The clip positions at the two ends of the file.
        let (first, last) = match &map {
            Some(m) => (m.src_to_tick(0.0), m.src_to_tick(seconds)),
            None => (0.0, seconds * ticks_per_second),
        };
        let axis = first + last;
        let path = reversed.to_path_buf();
        self.edit_clips(&[r], "Reverse Clip", |c| {
            edit::flip_region(c, axis);
            if let ClipContent::Audio {
                file,
                reverse,
                warp,
                ..
            } = &mut c.content
            {
                edit::flip_warp(warp, seconds, axis);
                *file = FileRef::new(&path);
                *reverse = !*reverse;
            }
        });
        self.commit_now();
    }

    /// Crop Clip: a new, shorter file of what plays, or of `range` in ticks if given. A MIDI clip
    /// loses its notes outside instead.
    pub fn crop_clip(&mut self, r: ClipRef, range: Option<(u64, u64)>) -> bool {
        let Some(clip) = self.clip_at(r).cloned() else {
            return false;
        };
        let (from, to) = range.unwrap_or_else(|| edit::crop_range(&clip));
        if to <= from {
            return false;
        }
        if clip.is_midi() {
            let changed = self.edit_clips(&[r], "Crop Clip", |c| edit::crop_midi(c, from, to));
            if let ClipRef::Arr { track, id } = r {
                if let Some(a) = self
                    .project
                    .track_mut(track)
                    .and_then(|t| t.arr_clip_mut(id))
                {
                    a.offset = a.offset.saturating_sub(from);
                }
                self.settle_after_crop(track);
            }
            return changed;
        }
        let Some(path) = self.audio_path(r) else {
            return false;
        };
        let Some(buffer) = self.samples.get(&path) else {
            self.messages
                .push(format!("{}: not loaded yet", path.display()));
            return false;
        };
        let seconds = self.tick_seconds_of(&clip, from as f64);
        let end = self.tick_seconds_of(&clip, to as f64);
        let rate = buffer.sample_rate;
        let a = ((seconds * rate).max(0.0) as usize).min(buffer.len());
        let b = ((end * rate).max(0.0) as usize).min(buffer.len());
        if b <= a {
            return false;
        }
        self.start_job(
            JobKind::Crop,
            Apply::Crop { from, to, seconds },
            r,
            path,
            a..b,
        )
    }

    fn apply_crop(&mut self, r: ClipRef, path: &Path, from: u64, to: u64, seconds: f64, len: f64) {
        let new = path.to_path_buf();
        self.edit_clips(&[r], "Crop Clip Sample", |c| {
            edit::crop_audio_markers(c, from, to, seconds, len);
            if let ClipContent::Audio { file, .. } = &mut c.content {
                *file = FileRef::new(&new);
            }
        });
        if let ClipRef::Arr { track, id } = r {
            if let Some(a) = self
                .project
                .track_mut(track)
                .and_then(|t| t.arr_clip_mut(id))
            {
                a.offset = a.offset.saturating_sub(from);
                self.settle_after_crop(track);
            }
        }
        self.commit_now();
    }

    /// A crop moves a timeline clip's offset, which is what its fades are limited by.
    fn settle_after_crop(&mut self, track: rlive_core::TrackId) {
        if let Some(t) = self.project.track_mut(track) {
            t.settle_fades();
        }
        self.sync_lane(track);
    }

    /// Picks up finished jobs. Called every UI frame.
    pub(super) fn poll_clip_jobs(&mut self) {
        let mut finished = Vec::new();
        let jobs = &mut self.clip_jobs.running;
        let mut i = 0;
        while i < jobs.len() {
            match jobs[i].done.try_recv() {
                Ok(result) => finished.push((jobs.remove(i), result)),
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    jobs.remove(i);
                }
            }
        }
        for (job, result) in finished {
            match result {
                Ok(done) => self.job_finished(job, done),
                Err(reason) => self.messages.push(reason),
            }
        }
    }

    fn job_finished(&mut self, job: Job, done: Finished) {
        let seconds = file_seconds_of_buffer(&done.buffer);
        self.samples.install(&done.path, done.buffer);
        if self.path.is_none() {
            self.clip_jobs.temporary.push((done.path.clone(), job.kind));
        }
        match job.apply {
            Apply::Reverse => {
                self.clip_jobs
                    .reversed
                    .insert(job.source.clone(), done.path.clone());
                self.clip_jobs
                    .reversed
                    .insert(done.path.clone(), job.source.clone());
                self.apply_reverse(job.target, &done.path);
            }
            Apply::Crop {
                from,
                to,
                seconds: at,
            } => self.apply_crop(job.target, &done.path, from, to, at, seconds),
        }
    }

    /// Waits for running jobs, for tests that want what they make.
    #[cfg(test)]
    pub(super) fn finish_clip_jobs(&mut self) {
        while self.clip_job_running() {
            self.poll_clip_jobs();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Moves processed files out of the temporary folder into the Set's own, now that it has one.
    pub(super) fn adopt_temp_processed(&mut self, project_file: &Path) {
        let moved = std::mem::take(&mut self.clip_jobs.temporary);
        let Some(dir) = project_file.parent() else {
            return;
        };
        for (from, kind) in moved {
            let Some(name) = from.file_name() else {
                continue;
            };
            let folder = dir.join("Samples").join("Processed").join(kind.folder());
            if let Err(e) = std::fs::create_dir_all(&folder) {
                self.messages.push(format!("{}: {e}", folder.display()));
                continue;
            }
            let to = folder.join(name);
            if let Err(e) =
                std::fs::rename(&from, &to).or_else(|_| std::fs::copy(&from, &to).map(|_| ()))
            {
                self.messages.push(format!("{}: {e}", from.display()));
                continue;
            }
            let _ = std::fs::remove_file(&from);
            self.samples.rename(&from, &to);
            self.project.for_each_file_ref_mut(&mut |file| {
                if file.path == from {
                    file.path = to.clone();
                }
            });
            let cache = &mut self.clip_jobs.reversed;
            if let Some(other) = cache.remove(&from) {
                cache.insert(to.clone(), other.clone());
                cache.insert(other, to.clone());
            }
        }
    }

    /// The Edit button: opens the clip's file in the external sample editor. Playback has to be
    /// stopped first (manual p. 218).
    pub fn edit_sample_externally(&mut self, r: ClipRef) -> Result<(), String> {
        if self.is_playing() {
            return Err("Stop playback to edit a sample in another program".into());
        }
        let path = self.audio_path(r).ok_or("not an audio clip")?;
        let editor = self
            .sample_editor
            .clone()
            .ok_or("Choose a Sample Editor in the File & Folder settings")?;
        self.watch_external(&path);
        let mut command = if editor.extension().is_some_and(|e| e == "app") {
            let mut open = std::process::Command::new("open");
            open.arg("-a").arg(&editor);
            open
        } else {
            std::process::Command::new(&editor)
        };
        command
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", editor.display()))
    }

    /// Starts noticing when `path` changes on disk.
    pub(super) fn watch_external(&mut self, path: &Path) {
        let seconds = self.probe_seconds(path);
        self.clip_jobs.watched.insert(
            path.to_path_buf(),
            Watched {
                modified: modified_time(path),
                seconds,
                loading: false,
            },
        );
    }

    /// Looks at files out in an external editor, every so many frames.
    pub(super) fn poll_external_edits(&mut self) {
        if self.clip_jobs.watched.is_empty() {
            return;
        }
        self.clip_jobs.frames += 1;
        if self.clip_jobs.frames.is_multiple_of(EXTERNAL_POLL_FRAMES) {
            self.reload_changed_files();
        }
    }

    pub(super) fn reload_changed_files(&mut self) {
        let mut changed = Vec::new();
        for (path, watched) in &mut self.clip_jobs.watched {
            let now = modified_time(path);
            if !watched.loading && now != watched.modified {
                watched.modified = now;
                watched.loading = true;
                changed.push(path.clone());
            }
        }
        for path in changed {
            self.samples.forget(&path);
            self.samples.request(&path);
        }
    }

    /// A new version of a file an editor had has been read. Every clip playing it changes, and
    /// keeps its Warp Markers only if the length is what it was (manual p. 218).
    pub(super) fn external_edit_arrived(&mut self, path: &Path, sample: &SampleBuffer) {
        let Some(watched) = self.clip_jobs.watched.get_mut(path) else {
            return;
        };
        if !watched.loading {
            return;
        }
        watched.loading = false;
        let now = file_seconds_of_buffer(sample);
        let same = watched
            .seconds
            .is_some_and(|before| (before - now).abs() < 1e-6);
        watched.seconds = Some(now);
        if same {
            return;
        }
        self.project.for_each_clip_mut(&mut |c| {
            if let ClipContent::Audio { file, warp, .. } = &mut c.content {
                if file.path == path {
                    warp.markers.clear();
                    warp.transients = Default::default();
                }
            }
        });
        self.mark_edit("Edit Sample");
    }

    /// Manage Sample File: the File Manager arrives with C13, and shows this file when it opens.
    pub fn manage_sample_file(&mut self, r: ClipRef) {
        if let Some(path) = self.audio_path(r) {
            self.file_manager_target = Some(path);
        }
    }
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}
