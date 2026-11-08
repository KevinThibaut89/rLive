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
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAGIC: &[u8; 8] = b"RLIVEANA";
const VERSION: u32 = 1;
const BYTES_PER_BLOCK: usize = 8;
const MAX_HEADER: usize = 16 * 1024 * 1024;

/// Extra analysis data kept with a sample. Not yet written by anything; here so A9 and C2 have
/// somewhere to put it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalysisExtras {
    #[serde(default)]
    pub warp: Option<serde_json::Value>,
    #[serde(default)]
    pub default_clip: Option<serde_json::Value>,
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

#[derive(Serialize, Deserialize)]
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
    fn the_sidecar_sits_next_to_the_sample_with_its_full_name() {
        assert_eq!(
            sidecar_path(Path::new("/s/kick.wav")),
            Path::new("/s/kick.wav.rlad")
        );
    }
}
