//! Loading samples in the background and sharing them.
//!
//! [`SampleCache::request`] never blocks: it returns what is known about a file and, the first
//! time, queues it for a loading thread. Whoever owns the cache calls [`SampleCache::poll`] to
//! hear about files that have finished, and hands the audio to the engine through its lock-free
//! commands. A sample is either entirely there or not there at all; nothing ever sees it half
//! decoded.
//!
//! Decoded audio currently stays in memory whatever its length. A reader that streams long files
//! from disk would sit behind [`crate::decode::Decoder`], where the block-by-block API already is.

use crate::analysis::{self, Analysis, AnalysisExtras};
use crate::decode::{DecodeError, DecodedAudio, Decoder};
use crate::decoding_cache::{CacheConfig, Cleanup, DecodingCache, Stored};
use crate::fingerprint::{fingerprint, Fingerprint};
use crate::peaks::Peaks;
use rlive_core::SampleBuffer;
use rlive_stretch::detect_transients;
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};

/// More than this many threads only makes the disk the bottleneck.
const MAX_WORKERS: usize = 4;

/// How samples are loaded. Takes effect from the next file loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderConfig {
    /// Whether to write an analysis file next to a sample. Existing ones are read either way.
    pub create_analysis_files: bool,
    pub cache: CacheConfig,
}

impl LoaderConfig {
    /// The system's place for files that can be recreated.
    pub fn default_cache_folder() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let base = if cfg!(target_os = "macos") {
            home.map(|h| h.join("Library/Caches"))
        } else {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| home.map(|h| h.join(".cache")))
        };
        base.unwrap_or_else(std::env::temp_dir)
            .join("rLive/Decoding")
    }
}

impl Default for LoaderConfig {
    fn default() -> Self {
        const GIB: u64 = 1024 * 1024 * 1024;
        Self {
            create_analysis_files: true,
            cache: CacheConfig {
                folder: Self::default_cache_folder(),
                minimum_free_space: 2 * GIB,
                maximum_size: Some(20 * GIB),
            },
        }
    }
}

/// Where a sample has got to.
#[derive(Debug, Clone)]
pub enum SampleStatus {
    /// Queued or being decoded. It can't be played yet.
    Loading,
    Ready(Arc<SampleBuffer>),
    /// Missing, unreadable or undecodable. The reason is ready to show.
    Failed(String),
}

/// A file that has finished loading, one way or the other.
pub struct SampleEvent {
    pub path: PathBuf,
    pub result: Result<Arc<SampleBuffer>, String>,
    /// Things worth telling the user that didn't stop the load.
    pub notices: Vec<String>,
}

enum State {
    Loading,
    Ready(Arc<SampleBuffer>),
    Failed(String),
}

struct Entry {
    state: State,
    /// Known ahead of the audio when an analysis file exists.
    peaks: Option<(Arc<Peaks>, f64)>,
    fingerprint: Option<Fingerprint>,
    /// In seconds, ascending. `None` until the analysis has finished.
    transients: Option<Arc<Vec<f64>>>,
}

struct Loaded {
    buffer: Arc<SampleBuffer>,
    peaks: Arc<Peaks>,
    fingerprint: Fingerprint,
    notices: Vec<String>,
    extras: AnalysisExtras,
}

enum Report {
    /// The analysis file was valid, so the waveform can be drawn before the audio is ready.
    Analysed {
        path: PathBuf,
        peaks: Arc<Peaks>,
        sample_rate: f64,
        fingerprint: Fingerprint,
        extras: AnalysisExtras,
    },
    Finished {
        path: PathBuf,
        result: Result<Loaded, String>,
    },
    /// The transients of a sample that had none stored.
    Transients { path: PathBuf, seconds: Vec<f64> },
}

struct Workers {
    jobs: Sender<PathBuf>,
    reports: Receiver<Report>,
}

/// Loads audio files once and shares them.
pub struct SampleCache {
    entries: HashMap<PathBuf, Entry>,
    config: Arc<RwLock<LoaderConfig>>,
    workers: Option<Workers>,
    events: Vec<SampleEvent>,
    /// Files whose transients have arrived since the last [`SampleCache::poll_transients`].
    new_transients: Vec<PathBuf>,
}

impl Default for SampleCache {
    fn default() -> Self {
        Self::new(LoaderConfig::default())
    }
}

impl SampleCache {
    pub fn new(config: LoaderConfig) -> Self {
        Self {
            entries: HashMap::new(),
            config: Arc::new(RwLock::new(config)),
            workers: None,
            events: Vec::new(),
            new_transients: Vec::new(),
        }
    }

    pub fn configure(&self, config: LoaderConfig) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// The current state of `path`, starting a load if this is the first anyone has asked for it.
    /// Never waits, whatever the state of the file.
    pub fn request(&mut self, path: &Path) -> SampleStatus {
        if !self.entries.contains_key(path) {
            self.entries.insert(
                path.to_path_buf(),
                Entry {
                    state: State::Loading,
                    peaks: None,
                    fingerprint: None,
                    transients: None,
                },
            );
            self.start_workers();
            if let Some(w) = &self.workers {
                let _ = w.jobs.send(path.to_path_buf());
            }
        }
        self.status(path).expect("just inserted")
    }

    /// The state of `path`, if anyone has asked for it.
    pub fn status(&self, path: &Path) -> Option<SampleStatus> {
        Some(match &self.entries.get(path)?.state {
            State::Loading => SampleStatus::Loading,
            State::Ready(b) => SampleStatus::Ready(b.clone()),
            State::Failed(e) => SampleStatus::Failed(e.clone()),
        })
    }

    /// The audio of a file that has finished loading.
    pub fn get(&self, path: &Path) -> Option<Arc<SampleBuffer>> {
        match &self.entries.get(path)?.state {
            State::Ready(b) => Some(b.clone()),
            _ => None,
        }
    }

    pub fn is_loading(&self, path: &Path) -> bool {
        self.entries
            .get(path)
            .is_some_and(|e| matches!(e.state, State::Loading))
    }

    pub fn is_ready(&self, path: &Path) -> bool {
        self.entries
            .get(path)
            .is_some_and(|e| matches!(e.state, State::Ready(_)))
    }

    /// Whether any file is still being loaded.
    pub fn any_loading(&self) -> bool {
        self.entries
            .values()
            .any(|e| matches!(e.state, State::Loading))
    }

    /// The waveform summary of a file, with its sample rate. Can arrive before the audio does.
    pub fn peaks(&self, path: &Path) -> Option<(Arc<Peaks>, f64)> {
        self.entries.get(path)?.peaks.clone()
    }

    /// Where the transients of a file are, in seconds, once its analysis has finished.
    pub fn transients(&self, path: &Path) -> Option<Arc<Vec<f64>>> {
        self.entries.get(path)?.transients.clone()
    }

    /// Hands back the files whose transients arrived since the last call. Never waits.
    pub fn poll_transients(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.new_transients)
    }

    /// Whether the audio of `path` is here but its transients aren't known yet.
    fn analysing(&self, path: &Path) -> bool {
        self.entries
            .get(path)
            .is_some_and(|e| matches!(e.state, State::Ready(_)) && e.transients.is_none())
    }

    /// Stores `settings` as the default for new clips of `path`, in its analysis file.
    pub fn save_default_clip(
        &mut self,
        path: &Path,
        settings: serde_json::Value,
    ) -> Result<(), String> {
        let entry = self
            .entries
            .get(path)
            .ok_or_else(|| "the sample isn't loaded".to_string())?;
        let fingerprint = entry
            .fingerprint
            .clone()
            .ok_or_else(|| "the sample hasn't finished loading".to_string())?;
        let change = |extras: &mut AnalysisExtras| extras.default_clip = Some(settings.clone());
        if analysis::update_extras(path, &fingerprint, change).is_ok() {
            return Ok(());
        }
        // Analysis files are turned off, or this one went away: this one is asked for.
        let (peaks, sample_rate) = entry
            .peaks
            .clone()
            .ok_or_else(|| "the sample hasn't finished loading".to_string())?;
        let mut extras = AnalysisExtras::default();
        if let Some(seconds) = &entry.transients {
            extras.set_transients(seconds);
        }
        extras.default_clip = Some(settings);
        analysis::write(
            path,
            &Analysis {
                fingerprint,
                frames: peaks.frames() as u64,
                sample_rate,
                peaks,
                extras,
            },
        )
        .map_err(|e| format!("couldn't write the analysis file: {e}"))
    }

    /// What the file was, content-wise, when it was loaded.
    pub fn fingerprint(&self, path: &Path) -> Option<&Fingerprint> {
        self.entries.get(path)?.fingerprint.as_ref()
    }

    /// Forgets a file, so the next request reads it again. For after a file has been relocated or
    /// repaired.
    pub fn forget(&mut self, path: &Path) {
        self.entries.remove(path);
    }

    /// Forgets every file that failed, so that they're tried again the next time they're asked
    /// for. For when a new project starts, whose files may have come back.
    pub fn forget_failed(&mut self) {
        self.entries
            .retain(|_, e| !matches!(e.state, State::Failed(_)));
    }

    /// Picks up finished work and returns the files that finished since the last call. Never
    /// waits.
    pub fn poll(&mut self) -> Vec<SampleEvent> {
        while let Some(report) = self
            .workers
            .as_ref()
            .and_then(|w| w.reports.try_recv().ok())
        {
            self.absorb(report);
        }
        std::mem::take(&mut self.events)
    }

    /// Requests `paths` and blocks until each has finished loading or failed, transients included,
    /// so a render doesn't depend on how far the analysis has got. Only for callers that must have
    /// the audio and aren't on a thread anything is waiting for, such as an offline render.
    pub fn wait_for<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) {
        let paths: Vec<&Path> = paths.into_iter().collect();
        for path in &paths {
            self.request(path);
        }
        while paths
            .iter()
            .any(|p| self.is_loading(p) || self.analysing(p))
        {
            let Some(report) = self.workers.as_ref().and_then(|w| w.reports.recv().ok()) else {
                self.fail_everything_loading();
                return;
            };
            self.absorb(report);
        }
    }

    /// Blocks until nothing is loading or being analysed.
    pub fn wait_idle(&mut self) {
        let waiting = |c: &Self| {
            c.any_loading()
                || c.entries
                    .values()
                    .any(|e| matches!(e.state, State::Ready(_)) && e.transients.is_none())
        };
        while waiting(self) {
            let Some(report) = self.workers.as_ref().and_then(|w| w.reports.recv().ok()) else {
                self.fail_everything_loading();
                return;
            };
            self.absorb(report);
        }
    }

    fn fail_everything_loading(&mut self) {
        let reason = "the loading threads stopped".to_string();
        let paths: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(e.state, State::Loading))
            .map(|(p, _)| p.clone())
            .collect();
        for path in paths {
            self.absorb(Report::Finished {
                path,
                result: Err(reason.clone()),
            });
        }
    }

    fn absorb(&mut self, report: Report) {
        match report {
            Report::Analysed {
                path,
                peaks,
                sample_rate,
                fingerprint,
                extras,
            } => {
                if let Some(e) = self.entries.get_mut(&path) {
                    e.peaks = Some((peaks, sample_rate));
                    e.fingerprint = Some(fingerprint);
                    if let Some(seconds) = extras.transients() {
                        e.transients = Some(Arc::new(seconds));
                        self.new_transients.push(path);
                    }
                }
            }
            Report::Transients { path, seconds } => {
                if let Some(e) = self.entries.get_mut(&path) {
                    e.transients = Some(Arc::new(seconds));
                    self.new_transients.push(path);
                }
            }
            Report::Finished { path, result } => {
                let Some(entry) = self.entries.get_mut(&path) else {
                    return;
                };
                let event = match result {
                    Ok(loaded) => {
                        entry.state = State::Ready(loaded.buffer.clone());
                        entry.peaks = Some((loaded.peaks, loaded.buffer.sample_rate));
                        entry.fingerprint = Some(loaded.fingerprint);
                        SampleEvent {
                            path,
                            result: Ok(loaded.buffer),
                            notices: loaded.notices,
                        }
                    }
                    Err(reason) => {
                        entry.state = State::Failed(reason.clone());
                        SampleEvent {
                            path,
                            result: Err(reason),
                            notices: Vec::new(),
                        }
                    }
                };
                self.events.push(event);
            }
        }
    }

    fn start_workers(&mut self) {
        if self.workers.is_some() {
            return;
        }
        let (jobs, job_queue) = channel::<PathBuf>();
        let (report, reports) = channel();
        let job_queue = Arc::new(Mutex::new(job_queue));
        let count = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .clamp(2, MAX_WORKERS);
        for n in 0..count {
            let job_queue = job_queue.clone();
            let report = report.clone();
            let config = self.config.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("rlive-sample-loader-{n}"))
                .spawn(move || worker(job_queue, report, config));
            if let Err(e) = spawned {
                log::error!("could not start a sample loading thread: {e}");
            }
        }
        self.workers = Some(Workers { jobs, reports });
    }

    /// Deletes everything in the decoding cache that no file in `in_use` needs.
    pub fn cleanup_decoding_cache(&self, in_use: &HashSet<String>) -> Cleanup {
        DecodingCache::new(self.cache_config()).cleanup(in_use)
    }

    fn cache_config(&self) -> CacheConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .cache
            .clone()
    }
}

fn worker(
    jobs: Arc<Mutex<Receiver<PathBuf>>>,
    report: Sender<Report>,
    config: Arc<RwLock<LoaderConfig>>,
) {
    loop {
        let job = jobs.lock().unwrap_or_else(|e| e.into_inner()).recv();
        let Ok(path) = job else {
            return;
        };
        let config = config.read().unwrap_or_else(|e| e.into_inner()).clone();
        // A decoder bug on a strange file must not take the whole application down.
        let result = catch_unwind(AssertUnwindSafe(|| load(&path, &config, &report)))
            .unwrap_or_else(|_| Err("the decoder crashed on this file".to_string()));
        let result = result.map_err(|reason| format!("{}: {reason}", path.display()));
        // The audio can be played from here on; finding transients doesn't hold that up.
        let to_analyse = result
            .as_ref()
            .ok()
            .filter(|l| l.extras.transients().is_none())
            .map(|l| (l.buffer.clone(), l.fingerprint.clone()));
        let finished = Report::Finished {
            path: path.clone(),
            result,
        };
        if report.send(finished).is_err() {
            return;
        }
        if let Some((buffer, fingerprint)) = to_analyse {
            let seconds = catch_unwind(AssertUnwindSafe(|| {
                detect_transients(&buffer.left, Some(&buffer.right), buffer.sample_rate)
            }))
            .unwrap_or_default();
            if config.create_analysis_files {
                let stored = analysis::update_extras(&path, &fingerprint, |extras| {
                    extras.set_transients(&seconds);
                });
                if let Err(e) = stored {
                    log::debug!("{}: transients not stored: {e}", path.display());
                }
            }
            if report.send(Report::Transients { path, seconds }).is_err() {
                return;
            }
        }
    }
}

fn describe(error: DecodeError) -> String {
    error.to_string()
}

fn load(path: &Path, config: &LoaderConfig, report: &Sender<Report>) -> Result<Loaded, String> {
    if !path.exists() {
        return Err(describe(DecodeError::NotFound));
    }
    let fingerprint = fingerprint(path).map_err(|e| describe(DecodeError::Io(e)))?;
    let analysis = analysis::read(path, &fingerprint);
    if let Some(a) = &analysis {
        let _ = report.send(Report::Analysed {
            path: path.to_path_buf(),
            peaks: a.peaks.clone(),
            sample_rate: a.sample_rate,
            fingerprint: fingerprint.clone(),
            extras: a.extras.clone(),
        });
    }
    let mut notices = Vec::new();
    let audio = read_audio(path, &fingerprint, config, &mut notices).map_err(describe)?;
    let (peaks, extras) = match analysis {
        Some(a) if a.peaks.frames() == audio.frames() => (a.peaks, a.extras),
        _ => {
            let peaks = Arc::new(if audio.is_stereo() {
                Peaks::stereo(&audio.left, &audio.right)
            } else {
                Peaks::mono(&audio.left)
            });
            // What was found in the old audio is gone, but saved markers stay if they still fit.
            let mut extras = AnalysisExtras::default();
            if let Some((frames, old)) = analysis::read_stale(path) {
                if frames == audio.frames() as u64 {
                    extras.default_clip = old.default_clip;
                }
            }
            if config.create_analysis_files {
                let written = analysis::write(
                    path,
                    &Analysis {
                        fingerprint: fingerprint.clone(),
                        frames: audio.frames() as u64,
                        sample_rate: audio.sample_rate as f64,
                        peaks: peaks.clone(),
                        extras: extras.clone(),
                    },
                );
                if let Err(e) = written {
                    log::debug!("{}: no analysis file written: {e}", path.display());
                }
            }
            (peaks, extras)
        }
    };
    Ok(Loaded {
        buffer: Arc::new(SampleBuffer {
            name: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            left: audio.left,
            right: audio.right,
            sample_rate: audio.sample_rate as f64,
        }),
        peaks,
        fingerprint,
        notices,
        extras,
    })
}

/// Reads the audio from the decoding cache if the file is compressed and has been decoded before,
/// and from the file otherwise.
fn read_audio(
    path: &Path,
    source: &Fingerprint,
    config: &LoaderConfig,
    notices: &mut Vec<String>,
) -> Result<DecodedAudio, DecodeError> {
    let decoder = Decoder::open(path)?;
    if !decoder.info().is_compressed() {
        return decoder.decode_all();
    }
    let cache = DecodingCache::new(config.cache.clone());
    if let Some(audio) = cache.load(source) {
        return Ok(audio);
    }
    let audio = decoder.decode_all()?;
    match cache.store(source, &audio) {
        Ok(Stored::Stored) => {}
        Ok(Stored::TooLarge) => notices.push(format!(
            "{}: too large to be decoded into the cache, which is full. It is kept in memory \
             for this session.",
            path.display()
        )),
        Err(e) => notices.push(format!(
            "{}: could not write to the decoding cache: {e}",
            path.display()
        )),
    }
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn a_valid_analysis_is_reported_before_the_audio_is_ready() {
        let dir = std::env::temp_dir().join(format!("rlive-early-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..4_000 {
            w.write_sample(0.5f32).unwrap();
            w.write_sample(0.5f32).unwrap();
        }
        w.finalize().unwrap();
        let config = LoaderConfig {
            create_analysis_files: true,
            cache: CacheConfig {
                folder: dir.join("cache"),
                minimum_free_space: 0,
                maximum_size: None,
            },
        };
        let (tx, rx) = channel();
        load(&path, &config, &tx).unwrap();
        assert!(rx.try_recv().is_err(), "nothing to reuse the first time");

        let (tx, rx) = channel();
        load(&path, &config, &tx).unwrap();
        let Ok(Report::Analysed { peaks, .. }) = rx.try_recv() else {
            panic!("the stored analysis is reported first");
        };
        assert_eq!(peaks.min_max(0, 0, 4_000), (0.5, 0.5));
        std::fs::remove_dir_all(dir).ok();
    }
}
