//! Loading in the background: the state a sample is in, what lands on disk, and what's reported.

use rlive_audio_io::analysis::{self, sidecar_path};
use rlive_audio_io::decoding_cache::{CacheConfig, DecodingCache};
use rlive_audio_io::{fingerprint, LoaderConfig, SampleCache, SampleStatus};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rlive-samples-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn config(&self) -> LoaderConfig {
        LoaderConfig {
            create_analysis_files: true,
            cache: CacheConfig {
                folder: self.file("cache"),
                minimum_free_space: 0,
                maximum_size: None,
            },
        }
    }

    fn cache(&self) -> SampleCache {
        SampleCache::new(self.config())
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn write_wav(path: &Path, left: &[f32], right: &[f32], rate: u32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for (l, r) in left.iter().zip(right) {
        w.write_sample(*l).unwrap();
        w.write_sample(*r).unwrap();
    }
    w.finalize().unwrap();
}

fn ramp(frames: usize, amp: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| amp * (i as f32 / frames as f32 - 0.5))
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn a_request_returns_at_once_and_finishes_later() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &ramp(4_000, 1.0), &ramp(4_000, 0.5), 44_100);
    let mut cache = dir.cache();
    assert!(matches!(cache.request(&path), SampleStatus::Loading));
    assert!(cache.is_loading(&path));
    assert!(cache.get(&path).is_none(), "nothing half-loaded is visible");
    cache.wait_idle();
    let events = cache.poll();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].path, path);
    let buffer = events[0].result.as_ref().unwrap();
    assert_eq!(buffer.len(), 4_000);
    assert_eq!(buffer.sample_rate, 44_100.0);
    assert_eq!(cache.poll().len(), 0, "reported once");
    assert!(matches!(cache.request(&path), SampleStatus::Ready(_)));
}

#[test]
fn the_same_file_loads_once() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &ramp(4_000, 1.0), &ramp(4_000, 0.5), 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.request(&path);
    cache.wait_idle();
    assert_eq!(cache.poll().len(), 1);
    let a = cache.get(&path).unwrap();
    let b = cache.get(&path).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}

#[test]
fn a_loaded_file_has_stereo_peaks_and_a_fingerprint() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &[0.5; 8_000], &[-0.25; 8_000], 48_000);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let (peaks, rate) = cache.peaks(&path).unwrap();
    assert!(peaks.is_stereo());
    assert_eq!(rate, 48_000.0);
    assert_eq!(peaks.min_max(0, 0, 8_000), (0.5, 0.5));
    assert_eq!(peaks.min_max(1, 0, 8_000), (-0.25, -0.25));
    assert_eq!(
        cache.fingerprint(&path).unwrap(),
        &fingerprint(&path).unwrap()
    );
}

#[test]
fn a_missing_file_is_reported_and_stays_failed() {
    let dir = Dir::new();
    let path = dir.file("gone.wav");
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let events = cache.poll();
    let reason = events[0].result.as_ref().unwrap_err();
    assert!(reason.contains("gone.wav"), "{reason}");
    assert!(reason.contains("not found"), "{reason}");
    assert!(matches!(cache.request(&path), SampleStatus::Failed(_)));
    assert!(cache.poll().is_empty(), "no second report, no second load");
}

#[test]
fn an_undecodable_file_is_reported() {
    let dir = Dir::new();
    let path = dir.file("junk.wav");
    std::fs::write(&path, vec![1u8; 4096]).unwrap();
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    assert!(cache.poll()[0].result.is_err());
}

#[test]
fn forgetting_a_file_lets_it_be_read_again() {
    let dir = Dir::new();
    let path = dir.file("later.wav");
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    cache.poll();
    write_wav(&path, &[0.5; 512], &[0.5; 512], 44_100);
    cache.forget(&path);
    cache.request(&path);
    cache.wait_idle();
    assert!(cache.poll()[0].result.is_ok());
}

#[test]
fn wait_for_blocks_until_named_files_are_done() {
    let dir = Dir::new();
    let a = dir.file("a.wav");
    let b = dir.file("missing.wav");
    write_wav(&a, &ramp(2_000, 1.0), &ramp(2_000, 1.0), 44_100);
    let mut cache = dir.cache();
    cache.wait_for([a.as_path(), b.as_path()]);
    assert!(cache.is_ready(&a));
    assert!(matches!(cache.status(&b), Some(SampleStatus::Failed(_))));
    assert_eq!(cache.poll().len(), 2, "the events are not lost");
}

// -------------------------------------------------------------------- transients

/// Bursts of a clicking tone at each of `at`, in seconds.
fn clicks(at: &[f64], rate: u32, seconds: f64) -> Vec<f32> {
    let mut out = vec![0.0; (seconds * rate as f64) as usize];
    for t in at {
        let start = (t * rate as f64) as usize;
        for (i, s) in out[start..].iter_mut().take(2_000).enumerate() {
            *s += (-(i as f32) / 300.0).exp() * if i % 3 == 0 { 0.7 } else { -0.35 };
        }
    }
    out
}

#[test]
fn transients_are_found_in_the_background_and_stored() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    let hits = [0.5, 1.0, 1.5, 2.0];
    let audio = clicks(&hits, 44_100, 2.5);
    write_wav(&path, &audio, &audio, 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let found = cache.transients(&path).expect("the analysis finished");
    assert_eq!(found.len(), hits.len(), "{found:?}");
    for (f, t) in found.iter().zip(hits) {
        assert!((f - t).abs() < 0.01, "{f} {t}");
    }
    assert_eq!(cache.poll_transients(), vec![path.clone()]);
    let a = analysis::read(&path, &fingerprint(&path).unwrap()).unwrap();
    let stored = a.extras.transients().unwrap();
    assert_eq!(stored.len(), found.len());
    for (s, f) in stored.iter().zip(found.iter()) {
        assert!((s - f).abs() < 2e-6, "{s} {f}");
    }
}

#[test]
fn stored_transients_are_reused_without_analysing_again() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    let audio = clicks(&[0.5, 1.0], 44_100, 1.5);
    write_wav(&path, &audio, &audio, 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    // Doctor the sidecar: if the transients come back changed, nothing recomputed them.
    let fp = fingerprint(&path).unwrap();
    analysis::update_extras(&path, &fp, |e| e.set_transients(&[0.123])).unwrap();
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    assert_eq!(cache.transients(&path).unwrap().as_slice(), &[0.123]);
}

#[test]
fn a_changed_file_is_analysed_again_and_keeps_saved_settings_if_it_is_as_long() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    let audio = clicks(&[0.5, 1.0], 44_100, 1.5);
    write_wav(&path, &audio, &audio, 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let saved = serde_json::json!({ "markers": 2 });
    cache.save_default_clip(&path, saved.clone()).unwrap();

    // Same length, different audio: the markers stay, as the old transients must not.
    let audio = clicks(&[0.25, 0.75, 1.25], 44_100, 1.5);
    write_wav(&path, &audio, &audio, 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    assert_eq!(cache.transients(&path).unwrap().len(), 3, "analysed anew");
    let fp = fingerprint(&path).unwrap();
    let a = analysis::read(&path, &fp).unwrap();
    assert_eq!(a.extras.default_clip, Some(saved));
    assert_eq!(a.extras.transients().unwrap().len(), 3);

    // A different length: nothing saved against the old audio survives.
    let audio = clicks(&[0.25], 44_100, 2.5);
    write_wav(&path, &audio, &audio, 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let a = analysis::read(&path, &fingerprint(&path).unwrap()).unwrap();
    assert_eq!(a.extras.default_clip, None);
}

#[test]
fn saved_settings_survive_the_transients_being_stored() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    let audio = clicks(&[0.5], 44_100, 1.0);
    write_wav(&path, &audio, &audio, 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let saved = serde_json::json!(true);
    cache.save_default_clip(&path, saved.clone()).unwrap();
    assert_eq!(analysis::default_clip_of(&path), Some(saved));
    let a = analysis::read(&path, &fingerprint(&path).unwrap()).unwrap();
    assert!(a.extras.transients().is_some(), "still there");
}

// -------------------------------------------------------------------- analysis files

#[test]
fn an_analysis_file_is_written_next_to_the_sample() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &ramp(4_000, 1.0), &ramp(4_000, 0.5), 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let fp = fingerprint(&path).unwrap();
    let a = analysis::read(&path, &fp).unwrap();
    assert_eq!(a.frames, 4_000);
    assert_eq!(a.sample_rate, 44_100.0);
    assert!(a.peaks.is_stereo());
}

#[test]
fn changing_the_file_invalidates_its_analysis() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &ramp(4_000, 1.0), &ramp(4_000, 0.5), 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let first = analysis::read(&path, &fingerprint(&path).unwrap()).unwrap();

    // The same length with different contents: only the hash can tell.
    write_wav(&path, &[0.9; 4_000], &[-0.9; 4_000], 44_100);
    let new_fp = fingerprint(&path).unwrap();
    assert_eq!(new_fp.size, first.fingerprint.size);
    assert!(analysis::read(&path, &new_fp).is_none(), "stale");

    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    let (peaks, _) = cache.peaks(&path).unwrap();
    assert_eq!(peaks.min_max(0, 0, 4_000), (0.9, 0.9), "analysed anew");
    assert!(analysis::read(&path, &new_fp).is_some(), "and saved");
}

#[test]
fn the_analysis_toggle_stops_new_files_but_leaves_old_ones() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &ramp(4_000, 1.0), &ramp(4_000, 0.5), 44_100);
    let mut off = dir.config();
    off.create_analysis_files = false;
    let mut cache = SampleCache::new(off.clone());
    cache.request(&path);
    cache.wait_idle();
    assert!(!sidecar_path(&path).exists(), "turned off");
    assert!(cache.peaks(&path).is_some(), "still analysed for this run");

    let mut on = dir.cache();
    on.request(&path);
    on.wait_idle();
    let sidecar = sidecar_path(&path);
    assert!(sidecar.exists(), "turned on");

    let mut off_again = SampleCache::new(off);
    off_again.request(&path);
    off_again.wait_idle();
    assert!(sidecar.exists(), "turning it off doesn't delete anything");
}

// -------------------------------------------------------------------- decoding cache

fn cache_size(dir: &Dir) -> u64 {
    DecodingCache::new(dir.config().cache).size()
}

#[test]
fn compressed_files_land_in_the_decoding_cache() {
    let dir = Dir::new();
    let source = dir.file("tone.ogg");
    std::fs::copy(fixture("stereo44.ogg"), &source).unwrap();
    let mut cache = dir.cache();
    cache.request(&source);
    cache.wait_idle();
    let first = cache.get(&source).unwrap();
    assert!(cache_size(&dir) > 0);

    // A second load reads the decoded copy, and it is the same audio.
    let mut cache = dir.cache();
    cache.request(&source);
    cache.wait_idle();
    let second = cache.get(&source).unwrap();
    assert_eq!(first.left, second.left);
    assert_eq!(first.sample_rate, second.sample_rate);
}

#[test]
fn plain_pcm_never_touches_the_decoding_cache() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &ramp(4_000, 1.0), &ramp(4_000, 0.5), 44_100);
    let mut cache = dir.cache();
    cache.request(&path);
    cache.wait_idle();
    assert_eq!(cache_size(&dir), 0);
}

#[test]
fn a_full_cache_warns_and_the_file_still_plays() {
    let dir = Dir::new();
    let source = dir.file("tone.mp3");
    std::fs::copy(fixture("stereo44.mp3"), &source).unwrap();
    let mut config = dir.config();
    config.cache.minimum_free_space = u64::MAX / 2;
    let mut cache = SampleCache::new(config);
    cache.request(&source);
    cache.wait_idle();
    let events = cache.poll();
    assert!(events[0].result.is_ok(), "kept in memory");
    assert!(
        events[0].notices[0].contains("too large"),
        "{:?}",
        events[0].notices
    );
    assert_eq!(cache_size(&dir), 0);
}

#[test]
fn cleanup_keeps_only_what_the_project_uses() {
    let dir = Dir::new();
    let mut cache = dir.cache();
    let (a, b) = (dir.file("a.ogg"), dir.file("b.mp3"));
    std::fs::copy(fixture("stereo44.ogg"), &a).unwrap();
    std::fs::copy(fixture("stereo44.mp3"), &b).unwrap();
    cache.request(&a);
    cache.request(&b);
    cache.wait_idle();
    let keep = std::collections::HashSet::from([cache.fingerprint(&a).unwrap().hash.clone()]);
    let done = cache.cleanup_decoding_cache(&keep);
    assert_eq!(done.files, 1);
    assert!(cache_size(&dir) > 0, "a's entry is still there");
}

#[test]
fn the_cache_location_setting_is_used() {
    let dir = Dir::new();
    let mut config = dir.config();
    config.cache.folder = dir.file("elsewhere");
    let source = dir.file("tone.m4a");
    std::fs::copy(fixture("stereo44.m4a"), &source).unwrap();
    let mut cache = SampleCache::new(config.clone());
    cache.request(&source);
    cache.wait_idle();
    assert!(DecodingCache::new(config.cache).size() > 0);
    assert_eq!(cache_size(&dir), 0);
}

#[test]
fn a_new_configuration_takes_effect_from_the_next_file() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_wav(&path, &[0.5; 1_024], &[0.5; 1_024], 44_100);
    let mut off = dir.config();
    off.create_analysis_files = false;
    let mut cache = SampleCache::new(dir.config());
    cache.configure(off);
    cache.request(&path);
    cache.wait_idle();
    assert!(!sidecar_path(&path).exists());
}
