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

/// Scan one plugin file in a child process.
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
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostError::Scan(format!("{} timed out", path.display())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(HostError::Scan(e.to_string())),
        }
    };
    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut stderr);
        }
        return Err(HostError::Scan(format!(
            "{} failed ({status}): {}",
            path.display(),
            stderr.trim()
        )));
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
        mut progress: impl FnMut(&Path),
    ) {
        let mut plugins = Vec::new();
        let mut files = Vec::new();
        for format in [PluginFormat::Clap, PluginFormat::Vst3] {
            let mut dirs = default_search_paths(format);
            dirs.extend_from_slice(extra_dirs);
            for path in find_plugin_files(format, &dirs) {
                let m = modified(&path);
                if let Some(prev) = self
                    .files
                    .iter()
                    .find(|f| f.path == path && f.format == format && f.modified == m)
                {
                    files.push(prev.clone());
                    plugins.extend(
                        self.plugins
                            .iter()
                            .filter(|p| p.path == path && p.format == format)
                            .cloned(),
                    );
                    continue;
                }
                progress(&path);
                let result =
                    scan_out_of_process(scanner_exe, format, &path, Duration::from_secs(30));
                let (ok, error) = match result {
                    Ok(found) => {
                        plugins.extend(found);
                        (true, None)
                    }
                    Err(e) => {
                        log::warn!("plugin scan failed: {e}");
                        (false, Some(e.to_string()))
                    }
                };
                files.push(ScannedFile {
                    path,
                    format,
                    modified: m,
                    ok,
                    error,
                });
            }
        }
        // Audio Units are enumerated through the system registry without loading them.
        if let Ok(aus) = crate::scan_audio_units() {
            plugins.extend(aus);
        }
        plugins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.plugins = plugins;
        self.files = files;
    }
}
