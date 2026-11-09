//! Analysis files: a sidecar next to a sample with what was worked out from it.
//!
//! The file is `<sample name>.<extension>.rlad`. It starts with [`MAGIC`] and the length of a JSON
//! header, followed by the finest level of the waveform peaks as little-endian `f32`s. It is not
//! compatible with Live's `.asd`.
//!
//! An analysis file belongs to one exact version of its sample: a different size or content hash
//! makes it stale, and it is read as if it weren't there. Unknown header fields are ignored and
//! new ones must have defaults, so that later milestones (warp data, default clips) can extend
//! the format without invalidating what is already on disk.

use crate::peaks::{Peaks, BASE_BLOCK};
use crate::Fingerprint;
use rlive_core::warp::BeatGrid;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAGIC: &[u8; 8] = b"RLIVEANA";
const VERSION: u32 = 1;
const BYTES_PER_BLOCK: usize = 8;
const MAX_HEADER: usize = 16 * 1024 * 1024;

/// One writer at a time, so read-modify-write never loses what another thread just stored.
static WRITING: Mutex<()> = Mutex::new(());

/// What the sample analysis found in the audio, as the `warp` extras store it.
#[derive(Serialize, Deserialize)]
struct StoredWarp {
    detector: u32,
    /// Transients in seconds, ascending.
    seconds: Vec<f64>,
    /// The beat grid, if the sample has been tracked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    beats: Option<StoredBeats>,
}

#[derive(Serialize, Deserialize)]
struct StoredBeats {
    tracker: u32,
    bpm: f64,
    beats: Vec<f64>,
    downbeat: usize,
    confidence: f32,
}

/// Extra analysis data kept with a sample. `warp` holds what the analysis found (see
/// [`AnalysisExtras::transients`]); `default_clip` holds clip settings saved by the user.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalysisExtras {
    #[serde(default)]
    pub warp: Option<serde_json::Value>,
    #[serde(default)]
    pub default_clip: Option<serde_json::Value>,
}

impl AnalysisExtras {
    /// The transients found in the sample, unless a newer detector should look again.
    pub fn transients(&self) -> Option<Vec<f64>> {
        let warp = self.warp.as_ref()?;
        let stored: StoredWarp = serde_json::from_value(warp.clone()).ok()?;
        (stored.detector == rlive_stretch::DETECTOR_VERSION).then_some(stored.seconds)
    }

    pub fn set_transients(&mut self, seconds: &[f64]) {
        let beats = self.stored_warp().and_then(|w| w.beats);
        let stored = StoredWarp {
            detector: rlive_stretch::DETECTOR_VERSION,
            seconds: rounded(seconds),
            beats,
        };
        self.warp = serde_json::to_value(stored).ok();
    }

    /// The beat grid of the sample, unless a newer tracker should look again.
    pub fn beats(&self) -> Option<BeatGrid> {
        let stored = self.stored_warp()?.beats?;
        (stored.tracker == rlive_stretch::TRACKER_VERSION).then_some(BeatGrid {
            bpm: stored.bpm,
            beats: stored.beats,
            downbeat: stored.downbeat,
            confidence: stored.confidence,
        })
    }

    pub fn set_beats(&mut self, grid: &BeatGrid) {
        let (detector, seconds) = match self.stored_warp() {
            Some(w) => (w.detector, w.seconds),
            // No transients yet: a detector of 0 is never current, so they are still looked for.
            None => (0, Vec::new()),
        };
        let stored = StoredWarp {
            detector,
            seconds,
            beats: Some(StoredBeats {
                tracker: rlive_stretch::TRACKER_VERSION,
                bpm: grid.bpm,
                beats: rounded(&grid.beats),
                downbeat: grid.downbeat,
                confidence: grid.confidence,
            }),
        };
        self.warp = serde_json::to_value(stored).ok();
    }

    fn stored_warp(&self) -> Option<StoredWarp> {
        serde_json::from_value(self.warp.as_ref()?.clone()).ok()
    }
}

/// A microsecond is far finer than a sample, and keeps the file short.
fn rounded(seconds: &[f64]) -> Vec<f64> {
    seconds.iter().map(|s| (s * 1e6).round() / 1e6).collect()
}

/// What an analysis file holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub fingerprint: Fingerprint,
    pub frames: u64,
    pub sample_rate: f64,
    pub peaks: Arc<Peaks>,
    pub extras: AnalysisExtras,
}

#[derive(Clone, Serialize, Deserialize)]
struct Header {
    version: u32,
    source_size: u64,
    source_hash: String,
    frames: u64,
    sample_rate: f64,
    stereo: bool,
    base_block: u32,
    blocks: u64,
    #[serde(default)]
    extras: AnalysisExtras,
}

/// Where the analysis file of `sample` lives.
pub fn sidecar_path(sample: &Path) -> PathBuf {
    let mut name = sample.as_os_str().to_os_string();
    name.push(".rlad");
    PathBuf::from(name)
}

/// The analysis of `sample`, if it has one that matches `fingerprint` exactly.
pub fn read(sample: &Path, fingerprint: &Fingerprint) -> Option<Analysis> {
    let bytes = fs::read(sidecar_path(sample)).ok()?;
    parse(&bytes, fingerprint)
}

/// The header of a sidecar, whichever version of the sample it was written for. Reads only the
/// start of the file, so it is cheap however long the sample is.
fn read_header(sample: &Path) -> Option<Header> {
    let mut file = fs::File::open(sidecar_path(sample)).ok()?;
    let mut start = [0; 12];
    file.read_exact(&mut start).ok()?;
    let (magic, len) = start.split_at(8);
    if magic != MAGIC.as_slice() {
        return None;
    }
    let len = u32::from_le_bytes(len.try_into().ok()?) as usize;
    if len > MAX_HEADER {
        return None;
    }
    let mut json = vec![0; len];
    file.read_exact(&mut json).ok()?;
    serde_json::from_slice(&json).ok()
}

/// The saved settings next to `sample`, if its size still matches. Cheap, for when a clip is made
/// from a file whose audio isn't loaded yet.
pub fn default_clip_of(sample: &Path) -> Option<serde_json::Value> {
    let size = fs::metadata(sample).ok()?.len();
    let header = read_header(sample)?;
    (header.source_size == size)
        .then_some(header.extras.default_clip)
        .flatten()
}

/// How long the sample was when its sidecar was written, and the extras stored there, even if the
/// sample has changed since.
pub(crate) fn read_stale(sample: &Path) -> Option<(u64, AnalysisExtras)> {
    let header = read_header(sample)?;
    Some((header.frames, header.extras))
}

fn parse(bytes: &[u8], fingerprint: &Fingerprint) -> Option<Analysis> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let (len, rest) = rest.split_first_chunk::<4>()?;
    let len = u32::from_le_bytes(*len) as usize;
    if len > MAX_HEADER || len > rest.len() {
        return None;
    }
    let (json, data) = rest.split_at(len);
    let header: Header = serde_json::from_slice(json).ok()?;
    if header.version != VERSION
        || header.base_block as usize != BASE_BLOCK
        || header.source_size != fingerprint.size
        || header.source_hash != fingerprint.hash
    {
        return None;
    }
    let channels = if header.stereo { 2 } else { 1 };
    let blocks = header.blocks as usize;
    if data.len() != blocks.checked_mul(channels * BYTES_PER_BLOCK)? {
        return None;
    }
    let mut values = data.chunks(4).map(|b| {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(b);
        f32::from_le_bytes(bytes)
    });
    let mut channel = |_| {
        (0..blocks)
            .map(|_| Some((values.next()?, values.next()?)))
            .collect::<Option<Vec<_>>>()
    };
    let left = channel(0)?;
    let right = if header.stereo {
        Some(channel(1)?)
    } else {
        None
    };
    Some(Analysis {
        fingerprint: fingerprint.clone(),
        frames: header.frames,
        sample_rate: header.sample_rate,
        peaks: Arc::new(Peaks::from_base(left, right, header.frames as usize)),
        extras: header.extras,
    })
}

/// Writes the analysis file of `sample`. Failures are the caller's to ignore: a read-only sample
/// folder only means the analysis happens again next time.
pub fn write(sample: &Path, analysis: &Analysis) -> io::Result<()> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    write_locked(sample, analysis)
}

/// Changes the extras of an existing analysis file, keeping everything else in it.
pub fn update_extras(
    sample: &Path,
    fingerprint: &Fingerprint,
    change: impl FnOnce(&mut AnalysisExtras),
) -> io::Result<()> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut analysis = read(sample, fingerprint)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no matching analysis file"))?;
    change(&mut analysis.extras);
    write_locked(sample, &analysis)
}

fn write_locked(sample: &Path, analysis: &Analysis) -> io::Result<()> {
    let peaks = &analysis.peaks;
    let header = Header {
        version: VERSION,
        source_size: analysis.fingerprint.size,
        source_hash: analysis.fingerprint.hash.clone(),
        frames: analysis.frames,
        sample_rate: analysis.sample_rate,
        stereo: peaks.is_stereo(),
        base_block: BASE_BLOCK as u32,
        blocks: peaks.left_base().len() as u64,
        extras: analysis.extras.clone(),
    };
    let json = serde_json::to_vec(&header).map_err(io::Error::other)?;
    let mut out = Vec::with_capacity(
        MAGIC.len() + 4 + json.len() + peaks.left_base().len() * 2 * BYTES_PER_BLOCK,
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    for base in std::iter::once(peaks.left_base()).chain(peaks.right_base()) {
        for (lo, hi) in base {
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&hi.to_le_bytes());
        }
    }
    let path = sidecar_path(sample);
    let temporary = path.with_extension("rlad.part");
    fs::write(&temporary, out)?;
    fs::rename(&temporary, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(size: u64, hash: &str) -> Fingerprint {
        Fingerprint {
            size,
            hash: format!("blake3:{hash}"),
        }
    }

    fn analysis(peaks: Peaks) -> Analysis {
        Analysis {
            fingerprint: fp(9, "aa"),
            frames: peaks.frames() as u64,
            sample_rate: 44_100.0,
            peaks: Arc::new(peaks),
            extras: AnalysisExtras::default(),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rlive-analysis-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("a.wav")
    }

    #[test]
    fn a_written_analysis_reads_back_the_same() {
        let path = temp("round-trip");
        let left: Vec<f32> = (0..5_000).map(|i| (i as f32 / 77.0).sin()).collect();
        let right: Vec<f32> = left.iter().map(|x| x * 0.5).collect();
        let a = analysis(Peaks::stereo(&left, &right));
        write(&path, &a).unwrap();
        assert_eq!(read(&path, &a.fingerprint).unwrap(), a);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn mono_stays_mono() {
        let path = temp("mono");
        let a = analysis(Peaks::mono(&[0.5; 1_000]));
        write(&path, &a).unwrap();
        assert!(!read(&path, &a.fingerprint).unwrap().peaks.is_stereo());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_different_size_or_hash_invalidates_it() {
        let path = temp("invalidate");
        let a = analysis(Peaks::mono(&[0.5; 1_000]));
        write(&path, &a).unwrap();
        assert!(read(&path, &fp(10, "aa")).is_none(), "size changed");
        assert!(read(&path, &fp(9, "bb")).is_none(), "contents changed");
        assert!(read(&path, &a.fingerprint).is_some());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_damaged_file_reads_as_none() {
        let path = temp("damaged");
        let a = analysis(Peaks::mono(&[0.5; 1_000]));
        write(&path, &a).unwrap();
        let sidecar = sidecar_path(&path);
        let bytes = std::fs::read(&sidecar).unwrap();
        for keep in [0, 4, 11, 20, bytes.len() - 1] {
            std::fs::write(&sidecar, &bytes[..keep]).unwrap();
            assert!(read(&path, &a.fingerprint).is_none(), "cut at {keep}");
        }
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn unknown_header_fields_are_ignored() {
        let path = temp("forward");
        let a = analysis(Peaks::mono(&[0.5; 1_000]));
        let header = Header {
            version: VERSION,
            source_size: a.fingerprint.size,
            source_hash: a.fingerprint.hash.clone(),
            frames: a.frames,
            sample_rate: a.sample_rate,
            stereo: false,
            base_block: BASE_BLOCK as u32,
            blocks: 4,
            extras: AnalysisExtras::default(),
        };
        let mut value = serde_json::to_value(&header).unwrap();
        value["from_the_future"] = serde_json::json!(true);
        let json = serde_json::to_vec(&value).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(&[0u8; 4 * BYTES_PER_BLOCK]);
        std::fs::write(sidecar_path(&path), &bytes).unwrap();
        assert!(read(&path, &a.fingerprint).is_some());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn transients_and_a_default_clip_round_trip_in_the_extras() {
        let path = temp("extras");
        let mut a = analysis(Peaks::mono(&[0.5; 1_000]));
        a.extras.set_transients(&[0.1, 0.25, 1.5]);
        a.extras.default_clip = Some(serde_json::json!({ "bpm": 128 }));
        write(&path, &a).unwrap();
        let back = read(&path, &a.fingerprint).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.extras.transients().unwrap(), vec![0.1, 0.25, 1.5]);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn transients_from_an_old_detector_are_not_reused() {
        let mut a = AnalysisExtras::default();
        a.set_transients(&[0.1]);
        let mut stored = a.warp.take().unwrap();
        stored["detector"] = serde_json::json!(rlive_stretch::DETECTOR_VERSION + 1);
        a.warp = Some(stored);
        assert_eq!(a.transients(), None);
    }

    #[test]
    fn updating_the_extras_keeps_the_peaks_and_the_other_extras() {
        let path = temp("update");
        let left: Vec<f32> = (0..4_000).map(|i| (i as f32 / 9.0).sin()).collect();
        let mut a = analysis(Peaks::mono(&left));
        a.extras.default_clip = Some(serde_json::json!(1));
        write(&path, &a).unwrap();
        update_extras(&path, &a.fingerprint, |e| e.set_transients(&[0.5])).unwrap();
        let back = read(&path, &a.fingerprint).unwrap();
        assert_eq!(back.peaks, a.peaks);
        assert_eq!(back.extras.default_clip, a.extras.default_clip);
        assert_eq!(back.extras.transients().unwrap(), vec![0.5]);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn beats_and_transients_share_a_sidecar_without_losing_each_other() {
        let path = temp("beats");
        let a = analysis(Peaks::mono(&[0.5; 1_000]));
        write(&path, &a).unwrap();
        let grid = BeatGrid {
            bpm: 124.5,
            beats: vec![0.5, 0.98, 1.46],
            downbeat: 1,
            confidence: 0.7,
        };
        update_extras(&path, &a.fingerprint, |e| e.set_transients(&[0.5])).unwrap();
        update_extras(&path, &a.fingerprint, |e| e.set_beats(&grid)).unwrap();
        let back = read(&path, &a.fingerprint).unwrap();
        assert_eq!(back.extras.transients().unwrap(), vec![0.5]);
        assert_eq!(back.extras.beats().unwrap(), grid);
        update_extras(&path, &a.fingerprint, |e| e.set_transients(&[0.25])).unwrap();
        let back = read(&path, &a.fingerprint).unwrap();
        assert_eq!(
            back.extras.beats().unwrap(),
            grid,
            "transients keep the beats"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_newer_tracker_looks_again() {
        let mut extras = AnalysisExtras::default();
        extras.set_beats(&BeatGrid {
            bpm: 120.0,
            beats: vec![0.0],
            downbeat: 0,
            confidence: 1.0,
        });
        let mut value = extras.warp.clone().unwrap();
        value["beats"]["tracker"] = serde_json::json!(rlive_stretch::TRACKER_VERSION + 1);
        extras.warp = Some(value);
        assert!(extras.beats().is_none());
    }

    #[test]
    fn updating_a_sidecar_for_another_version_of_the_sample_does_nothing() {
        let path = temp("update-stale");
        let a = analysis(Peaks::mono(&[0.5; 1_000]));
        write(&path, &a).unwrap();
        assert!(update_extras(&path, &fp(1, "x"), |e| e.set_transients(&[1.0])).is_err());
        assert_eq!(read(&path, &a.fingerprint).unwrap().extras.warp, None);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn the_default_clip_is_read_from_the_header_alone_while_the_size_matches() {
        let path = temp("peek");
        std::fs::write(&path, [0u8; 9]).unwrap();
        let mut a = analysis(Peaks::mono(&[0.5; 1_000]));
        a.extras.default_clip = Some(serde_json::json!("settings"));
        write(&path, &a).unwrap();
        assert_eq!(default_clip_of(&path), Some(serde_json::json!("settings")));
        std::fs::write(&path, [0u8; 10]).unwrap();
        assert_eq!(default_clip_of(&path), None, "the file changed size");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn the_sidecar_sits_next_to_the_sample_with_its_full_name() {
        assert_eq!(
            sidecar_path(Path::new("/s/kick.wav")),
            Path::new("/s/kick.wav.rlad")
        );
    }
}
