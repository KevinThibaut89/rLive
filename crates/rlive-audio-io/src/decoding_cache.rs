//! The decoding cache: decoded copies of compressed files, kept on disk so they need decoding
//! only once.
//!
//! Each entry is a 32-bit float WAV named after the content hash of its source, so moving or
//! renaming a source doesn't lose its entry and two copies of a file share one.

use crate::decode::{decode, DecodedAudio};
use crate::Fingerprint;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const ENTRY_EXTENSION: &str = "wav";
const BYTES_PER_SAMPLE: u64 = 4;
const HEADER_BYTES: u64 = 64;

/// Where the cache is and how much disk it may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheConfig {
    pub folder: PathBuf,
    /// Bytes that must stay free on the disk.
    pub minimum_free_space: u64,
    /// The cache's size limit in bytes; `None` leaves it unlimited.
    pub maximum_size: Option<u64>,
}

/// What [`DecodingCache::store`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Stored,
    /// The file didn't fit under the limits, even after evicting older entries.
    TooLarge,
}

/// What [`DecodingCache::cleanup`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cleanup {
    pub files: usize,
    pub bytes: u64,
}

pub struct DecodingCache {
    config: CacheConfig,
}

/// Bytes `audio` takes as a cache entry.
fn entry_size(audio: &DecodedAudio) -> u64 {
    HEADER_BYTES + audio.frames() as u64 * audio.channels as u64 * BYTES_PER_SAMPLE
}

struct Entry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

impl DecodingCache {
    pub fn new(config: CacheConfig) -> Self {
        Self { config }
    }

    pub fn folder(&self) -> &Path {
        &self.config.folder
    }

    fn entry_path(&self, source: &Fingerprint) -> PathBuf {
        self.config
            .folder
            .join(source.digest())
            .with_extension(ENTRY_EXTENSION)
    }

    /// The decoded audio of `source`, if it's in the cache.
    pub fn load(&self, source: &Fingerprint) -> Option<DecodedAudio> {
        let path = self.entry_path(source);
        if !path.is_file() {
            return None;
        }
        match decode(&path) {
            Ok(audio) if !audio.truncated => {
                // Using an entry keeps it from being the next one evicted.
                if let Ok(f) = fs::File::options().write(true).open(&path) {
                    let _ = f.set_modified(SystemTime::now());
                }
                Some(audio)
            }
            _ => {
                log::warn!("removing unreadable cache entry {}", path.display());
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Adds a decoded file, evicting the least recently used entries if that makes it fit.
    pub fn store(&self, source: &Fingerprint, audio: &DecodedAudio) -> io::Result<Stored> {
        fs::create_dir_all(&self.config.folder)?;
        let path = self.entry_path(source);
        if path.is_file() {
            return Ok(Stored::Stored);
        }
        let needed = entry_size(audio);
        let mut free = free_space(&self.config.folder);
        if free.is_some_and(|f| f < needed.saturating_add(self.config.minimum_free_space)) {
            let room = self.make_room(needed);
            free = free.map(|f| f + room);
            if free.is_some_and(|f| f < needed.saturating_add(self.config.minimum_free_space)) {
                return Ok(Stored::TooLarge);
            }
        }
        if let Some(limit) = self.config.maximum_size {
            if needed > limit {
                return Ok(Stored::TooLarge);
            }
            self.evict_to(limit - needed)?;
        }
        write_entry(&path, audio)?;
        Ok(Stored::Stored)
    }

    fn entries(&self) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.config.folder)? {
            let item = item?;
            let path = item.path();
            if path.extension().is_none_or(|e| e != ENTRY_EXTENSION) {
                continue;
            }
            let meta = item.metadata()?;
            entries.push(Entry {
                path,
                size: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
        entries.sort_by_key(|e| e.modified);
        Ok(entries)
    }

    /// Total bytes in the cache.
    pub fn size(&self) -> u64 {
        self.entries()
            .map_or(0, |list| list.iter().map(|e| e.size).sum())
    }

    /// Evicts the oldest entries until no more than `limit` bytes remain.
    fn evict_to(&self, limit: u64) -> io::Result<()> {
        let entries = self.entries()?;
        let mut total: u64 = entries.iter().map(|e| e.size).sum();
        for e in entries {
            if total <= limit {
                break;
            }
            fs::remove_file(&e.path)?;
            total -= e.size;
        }
        Ok(())
    }

    /// Frees up to `needed` bytes by evicting the oldest entries. Returns what it freed.
    fn make_room(&self, needed: u64) -> u64 {
        let mut freed = 0;
        for e in self.entries().unwrap_or_default() {
            if freed >= needed {
                break;
            }
            if fs::remove_file(&e.path).is_ok() {
                freed += e.size;
            }
        }
        freed
    }

    /// Deletes every entry except those of `keep`, content hashes (`"blake3:..."`) in use.
    pub fn cleanup(&self, keep: &HashSet<String>) -> Cleanup {
        let keep: HashSet<&str> = keep
            .iter()
            .map(|h| h.strip_prefix("blake3:").unwrap_or(h))
            .collect();
        let mut done = Cleanup::default();
        let Ok(entries) = self.entries() else {
            return done;
        };
        for e in entries {
            let name = e.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !keep.contains(name) && fs::remove_file(&e.path).is_ok() {
                done.files += 1;
                done.bytes += e.size;
            }
        }
        done
    }
}

fn write_entry(path: &Path, audio: &DecodedAudio) -> io::Result<()> {
    let temporary = path.with_extension("part");
    let spec = hound::WavSpec {
        channels: audio.channels.max(1) as u16,
        sample_rate: audio.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let result = (|| -> Result<(), hound::Error> {
        let mut w = hound::WavWriter::create(&temporary, spec)?;
        for (l, r) in audio.left.iter().zip(&audio.right) {
            w.write_sample(*l)?;
            if audio.channels > 1 {
                w.write_sample(*r)?;
            }
        }
        w.finalize()
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&temporary);
        return Err(io::Error::other(e));
    }
    fs::rename(&temporary, path)
}

/// Free bytes on the disk holding `folder`, when the system can say.
#[cfg(unix)]
fn free_space(folder: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(folder.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stat` is writable; the struct is read only on success.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    // These fields have different types on different systems.
    #[allow(clippy::unnecessary_cast)]
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(not(unix))]
fn free_space(_folder: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(frames: usize) -> DecodedAudio {
        DecodedAudio {
            left: (0..frames).map(|i| i as f32 / frames as f32).collect(),
            right: vec![0.25; frames],
            sample_rate: 44_100,
            channels: 2,
            truncated: false,
        }
    }

    fn fp(name: &str) -> Fingerprint {
        Fingerprint {
            size: 1,
            hash: format!("blake3:{name}"),
        }
    }

    fn cache(name: &str, minimum_free_space: u64, maximum_size: Option<u64>) -> DecodingCache {
        let folder =
            std::env::temp_dir().join(format!("rlive-cache-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        DecodingCache::new(CacheConfig {
            folder,
            minimum_free_space,
            maximum_size,
        })
    }

    fn set_age(c: &DecodingCache, source: &Fingerprint, seconds_ago: u64) {
        let path = c.entry_path(source);
        let f = fs::File::options().write(true).open(path).unwrap();
        let when = SystemTime::now() - std::time::Duration::from_secs(seconds_ago);
        f.set_modified(when).unwrap();
    }

    #[test]
    fn a_stored_file_comes_back_exactly() {
        let c = cache("round-trip", 0, None);
        let a = audio(1_000);
        assert_eq!(c.store(&fp("a"), &a).unwrap(), Stored::Stored);
        assert_eq!(c.load(&fp("a")).unwrap(), a);
        assert!(c.load(&fp("b")).is_none());
        fs::remove_dir_all(c.folder()).ok();
    }

    #[test]
    fn the_size_limit_evicts_the_oldest_entries_first() {
        let one = entry_size(&audio(1_000));
        let c = cache("evict", 0, Some(one * 2 + one / 2));
        c.store(&fp("a"), &audio(1_000)).unwrap();
        set_age(&c, &fp("a"), 3_000);
        c.store(&fp("b"), &audio(1_000)).unwrap();
        set_age(&c, &fp("b"), 2_000);
        c.store(&fp("c"), &audio(1_000)).unwrap();
        assert!(!c.entry_path(&fp("a")).exists(), "the oldest went first");
        assert!(c.entry_path(&fp("b")).exists());
        assert!(c.entry_path(&fp("c")).exists());
        assert!(c.size() <= one * 2 + one / 2);
        fs::remove_dir_all(c.folder()).ok();
    }

    #[test]
    fn using_an_entry_protects_it_from_eviction() {
        let one = entry_size(&audio(1_000));
        let c = cache("touch", 0, Some(one * 2 + one / 2));
        c.store(&fp("a"), &audio(1_000)).unwrap();
        set_age(&c, &fp("a"), 3_000);
        c.store(&fp("b"), &audio(1_000)).unwrap();
        set_age(&c, &fp("b"), 2_000);
        c.load(&fp("a")).unwrap();
        c.store(&fp("c"), &audio(1_000)).unwrap();
        assert!(c.entry_path(&fp("a")).exists(), "recently used");
        assert!(!c.entry_path(&fp("b")).exists(), "now the oldest");
        fs::remove_dir_all(c.folder()).ok();
    }

    #[test]
    fn a_file_larger_than_the_cache_is_not_stored() {
        let c = cache("oversize", 0, Some(100));
        assert_eq!(c.store(&fp("a"), &audio(1_000)).unwrap(), Stored::TooLarge);
        assert!(!c.entry_path(&fp("a")).exists());
        fs::remove_dir_all(c.folder()).ok();
    }

    #[test]
    fn minimum_free_space_is_respected() {
        // No disk has this much free, so nothing may be written.
        let c = cache("free", u64::MAX / 2, None);
        assert_eq!(c.store(&fp("a"), &audio(1_000)).unwrap(), Stored::TooLarge);
        assert!(!c.entry_path(&fp("a")).exists());
        fs::remove_dir_all(c.folder()).ok();
    }

    #[test]
    fn cleanup_deletes_what_is_not_in_use() {
        let c = cache("cleanup", 0, None);
        c.store(&fp("keep"), &audio(1_000)).unwrap();
        c.store(&fp("drop1"), &audio(1_000)).unwrap();
        c.store(&fp("drop2"), &audio(1_000)).unwrap();
        let done = c.cleanup(&HashSet::from(["blake3:keep".to_string()]));
        assert_eq!(done.files, 2);
        assert!(c.entry_path(&fp("keep")).exists());
        assert!(!c.entry_path(&fp("drop1")).exists());
        fs::remove_dir_all(c.folder()).ok();
    }

    #[test]
    fn a_damaged_entry_is_dropped_and_read_as_a_miss() {
        let c = cache("damaged", 0, None);
        c.store(&fp("a"), &audio(1_000)).unwrap();
        fs::write(c.entry_path(&fp("a")), b"RIFF not really").unwrap();
        assert!(c.load(&fp("a")).is_none());
        assert!(!c.entry_path(&fp("a")).exists());
        fs::remove_dir_all(c.folder()).ok();
    }

    #[test]
    fn free_space_can_be_measured() {
        assert!(free_space(&std::env::temp_dir()).is_some_and(|f| f > 0));
    }
}
