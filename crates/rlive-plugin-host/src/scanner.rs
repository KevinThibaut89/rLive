//! Out-of-process plugin scanning.
//!
//! Loading a plugin binary runs arbitrary code that may crash or hang, so the app re-launches
//! itself (or the `rlive-plugin-scanner` binary) with `--scan <format> <path>` for each file.
//! The child prints its findings as JSON; crashes and timeouts just mark the file as failed.

use crate::{
    default_search_paths, find_plugin_files, scan_file, HostError, PluginDescriptor, Result,
};
use rlive_core::PluginFormat;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const RESULT_MARKER: &str = "RLIVE_SCAN_RESULT:";
pub const SCAN_FLAG: &str = "--scan";

/// If `args` (excluding argv[0]) is a scan request, run it and return the exit code.
///
/// Call this at the very top of `main` so the binary can act as its own scanner.
pub fn run_scanner_main(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some(SCAN_FLAG) {
        return None;
    }
    let (Some(format), Some(path)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: {SCAN_FLAG} <clap|vst3|au> <path>");
        return Some(2);
    };
    let Some(format) = PluginFormat::from_key(format) else {
        eprintln!("unknown plugin format {format}");
        return Some(2);
    };
    match scan_file(format, Path::new(path)) {
        Ok(found) => {
            let json = serde_json::to_string(&found).unwrap_or_else(|_| "[]".into());
            println!("{RESULT_MARKER}{json}");
            Some(0)
        }
        Err(e) => {
            eprintln!("{e}");
            Some(1)
        }
    }
}

/// How long one file may take to scan before it counts as hung.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(60);

/// A file that crashed or hung the scanner twice is kept out until it is reinstalled.
pub const CRASHES_TO_DISABLE: u32 = 2;

/// Exit codes the scanner uses for an ordinary failure, as opposed to dying.
const ORDINARY_EXIT: [i32; 2] = [1, 2];

/// Read a child's output on another thread, so a chatty plugin can't fill the pipe and wedge.
fn collect(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        // A plugin's output needn't be valid UTF-8, and the result line is found regardless.
        let mut bytes = Vec::new();
        if pipe.read_to_end(&mut bytes).is_ok() {
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        let _ = tx.send(text);
    });
    rx
}

/// What a pipe's reader got. The wait is short because a child's grandchild can hold it open.
fn collected(rx: Option<mpsc::Receiver<String>>) -> String {
    rx.and_then(|rx| rx.recv_timeout(Duration::from_secs(2)).ok())
        .unwrap_or_default()
}

/// Scan one plugin file in a child process.
///
/// A child that dies or hangs is reported as [`HostError::ScanCrashed`], anything else that went
/// wrong as [`HostError::Scan`].
pub fn scan_out_of_process(
    exe: &Path,
    format: PluginFormat,
    path: &Path,
    timeout: Duration,
) -> Result<Vec<PluginDescriptor>> {
    let mut child = Command::new(exe)
        .arg(SCAN_FLAG)
        .arg(format.key())
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| HostError::Scan(format!("could not start scanner {}: {e}", exe.display())))?;
    let stdout = child.stdout.take().map(collect);
    let stderr = child.stderr.take().map(collect);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostError::ScanCrashed(format!(
                    "{} timed out",
                    path.display()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(HostError::Scan(e.to_string())),
        }
    };
    let stdout = collected(stdout);
    if !status.success() {
        let stderr = collected(stderr);
        let message = format!("{} failed ({status}): {}", path.display(), stderr.trim());
        let ordinary = status.code().is_some_and(|c| ORDINARY_EXIT.contains(&c));
        return Err(if ordinary {
            HostError::Scan(message)
        } else {
            HostError::ScanCrashed(message)
        });
    }
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix(RESULT_MARKER))
        .ok_or_else(|| HostError::Scan("scanner produced no result".into()))?;
    serde_json::from_str(line).map_err(|e| HostError::Scan(e.to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub format: PluginFormat,
    /// Modification time (seconds since the epoch) when scanned.
    pub modified: u64,
    pub ok: bool,
    pub error: Option<String>,
    /// Scans of this exact file that ended with the scanner dying or hanging.
    #[serde(default)]
    pub crashes: u32,
    /// Kept out of the browser and of scans until it is reinstalled.
    #[serde(default)]
    pub disabled: bool,
}

/// Which plugin sources a scan looks at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSources {
    /// Folders to search, per format.
    pub clap_dirs: Vec<PathBuf>,
    pub vst3_dirs: Vec<PathBuf>,
    pub audio_units: bool,
}

impl ScanSources {
    /// The system's folders and Audio Units, plus `custom` folders searched for every format.
    pub fn standard(custom: &[PathBuf]) -> Self {
        let dirs = |format| {
            let mut v = default_search_paths(format);
            v.extend_from_slice(custom);
            v
        };
        Self {
            clap_dirs: dirs(PluginFormat::Clap),
            vst3_dirs: dirs(PluginFormat::Vst3),
            audio_units: true,
        }
    }
}

/// What went wrong in a scan that needs the user to know about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Files that crashed the scanner this time, and can be rescanned or disabled.
    pub crashed: Vec<PathBuf>,
    /// Files that crashed it often enough to have been disabled by this scan.
    pub disabled: Vec<PathBuf>,
}

impl ScanReport {
    pub fn is_clean(&self) -> bool {
        self.crashed.is_empty() && self.disabled.is_empty()
    }
}

/// Persistent scan results, so only new or changed plugins are re-scanned.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginCache {
    pub plugins: Vec<PluginDescriptor>,
    pub files: Vec<ScannedFile>,
}

fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

impl PluginCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// Default cache location (`~/Library/Application Support/rLive/plugins.json` on macOS).
    pub fn default_path() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/rLive/plugins.json")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("rlive/plugins.json")
        }
    }

    /// Scan all default locations (plus `extra_dirs`), re-using cached results for unchanged files.
    /// `progress` is called with each file before it is scanned.
    pub fn rescan(
        &mut self,
        scanner_exe: &Path,
        extra_dirs: &[PathBuf],
        progress: impl FnMut(&Path),
    ) -> ScanReport {
        self.rescan_sources(
            scanner_exe,
            &ScanSources::standard(extra_dirs),
            SCAN_TIMEOUT,
            progress,
        )
    }

    /// Scan `sources`, leaving out the files already known to be broken.
    ///
    /// Each file gets its own scanner process, so one that crashes only loses its own results:
    /// the scan carries on, and the file is scanned afresh next time. After
    /// [`CRASHES_TO_DISABLE`] such scans it is disabled, until it is replaced by a new version.
    pub fn rescan_sources(
        &mut self,
        scanner_exe: &Path,
        sources: &ScanSources,
        timeout: Duration,
        mut progress: impl FnMut(&Path),
    ) -> ScanReport {
        let mut report = ScanReport::default();
        let mut plugins = Vec::new();
        let mut files = Vec::new();
        let targets = [
            (PluginFormat::Clap, &sources.clap_dirs),
            (PluginFormat::Vst3, &sources.vst3_dirs),
        ];
        for (format, dirs) in targets {
            for path in find_plugin_files(format, dirs) {
                let m = modified(&path);
                let known = self
                    .files
                    .iter()
                    .find(|f| f.path == path && f.format == format && f.modified == m);
                if let Some(prev) = known {
                    if prev.disabled {
                        files.push(prev.clone());
                        continue;
                    }
                    if prev.crashes == 0 {
                        files.push(prev.clone());
                        plugins.extend(
                            self.plugins
                                .iter()
                                .filter(|p| p.path == path && p.format == format)
                                .cloned(),
                        );
                        continue;
                    }
                }
                let crashes = known.map_or(0, |f| f.crashes);
                progress(&path);
                let result = scan_out_of_process(scanner_exe, format, &path, timeout);
                let (ok, error, crashes) = match result {
                    Ok(found) => {
                        plugins.extend(found);
                        (true, None, 0)
                    }
                    Err(e @ HostError::ScanCrashed(_)) => {
                        log::warn!("plugin scan crashed: {e}");
                        (false, Some(e.to_string()), crashes + 1)
                    }
                    Err(e) => {
                        log::warn!("plugin scan failed: {e}");
                        (false, Some(e.to_string()), 0)
                    }
                };
                let disabled = crashes >= CRASHES_TO_DISABLE;
                if disabled {
                    report.disabled.push(path.clone());
                } else if crashes > 0 {
                    report.crashed.push(path.clone());
                }
                files.push(ScannedFile {
                    path,
                    format,
                    modified: m,
                    ok,
                    error,
                    crashes,
                    disabled,
                });
            }
        }
        // Audio Units are enumerated through the system registry without loading them.
        if sources.audio_units {
            if let Ok(aus) = crate::scan_audio_units() {
                plugins.extend(aus);
            }
        }
        plugins.sort_by_key(|a| a.name.to_lowercase());
        self.plugins = plugins;
        self.files = files;
        report
    }

    /// Forget everything found so far, so that the next scan starts clean.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Keep a file out of the browser and of scans, or let it back in to be scanned again.
    pub fn set_disabled(&mut self, path: &Path, disabled: bool) {
        for f in self.files.iter_mut().filter(|f| f.path == path) {
            f.disabled = disabled;
            f.crashes = 0;
            f.error = None;
            f.ok = !disabled;
        }
        if disabled {
            self.plugins.retain(|p| p.path != path);
        } else {
            // It's scanned again next time because nothing recorded says it was seen.
            self.files.retain(|f| f.path != path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with(file: ScannedFile) -> PluginCache {
        PluginCache {
            plugins: vec![PluginDescriptor {
                format: file.format,
                id: "x".into(),
                name: "X".into(),
                vendor: String::new(),
                version: String::new(),
                path: file.path.clone(),
                is_instrument: false,
                subcategories: String::new(),
                is_midi_effect: false,
            }],
            files: vec![file],
        }
    }

    fn file(path: &str) -> ScannedFile {
        ScannedFile {
            path: PathBuf::from(path),
            format: PluginFormat::Clap,
            modified: 1,
            ok: true,
            error: None,
            crashes: 0,
            disabled: false,
        }
    }

    #[test]
    fn disabling_a_file_removes_its_plugins_until_it_is_let_back() {
        let mut cache = cache_with(file("/a.clap"));
        cache.set_disabled(Path::new("/a.clap"), true);
        assert!(cache.plugins.is_empty());
        assert!(cache.files[0].disabled);
        cache.set_disabled(Path::new("/a.clap"), false);
        assert!(cache.files.is_empty(), "so the next scan looks at it");
    }

    #[test]
    fn a_cache_written_before_crash_tracking_still_loads() {
        let old = r#"{"plugins":[],"files":[{"path":"/a.clap","format":"Clap","modified":1,"ok":false,"error":"x"}]}"#;
        let cache: PluginCache = serde_json::from_str(old).unwrap();
        assert_eq!(cache.files[0].crashes, 0);
        assert!(!cache.files[0].disabled);
    }

    #[test]
    fn a_clear_forgets_disabled_files_too() {
        let mut cache = cache_with(ScannedFile {
            disabled: true,
            ..file("/a.clap")
        });
        cache.clear();
        assert_eq!(cache, PluginCache::default());
    }
}
