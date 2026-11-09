//! Third-party plugin hosting for rLive: CLAP, VST3 and (on macOS) Audio Units.
//!
//! Every hosted plugin is split in two halves:
//!
//! * a [`PluginHandle`] that stays on the UI/main thread (parameters, state, editor window,
//!   servicing plugin callbacks), and
//! * a [`Processor`] that is moved into the audio engine.
//!
//! Both share the underlying plugin instance, which is destroyed when both halves are dropped
//! (the engine always hands processors back to the UI thread to be dropped).
//!
//! Scanning untrusted plugin binaries can crash, so the app scans out of process:
//! see [`scanner`].

#[cfg(target_os = "macos")]
pub mod au;
pub mod clap;
pub mod keyed;
#[cfg(target_os = "macos")]
mod mac;
mod midi_out;
pub mod scanner;
pub mod vst3;

pub use rlive_core::PluginFormat;
use rlive_core::{ParamInfo, Processor};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A plugin found by the scanner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub format: PluginFormat,
    /// CLAP plugin id, VST3 class id (hex), or AU `type:subtype:manufacturer` four-char codes.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// Bundle or library path (empty for Audio Units, which are found by id).
    pub path: PathBuf,
    pub is_instrument: bool,
    /// VST3's `|`-separated subcategories, as declared; empty for other formats.
    #[serde(default)]
    pub subcategories: String,
    /// It works on MIDI rather than making sound: it goes before the instrument in a chain.
    #[serde(default)]
    pub is_midi_effect: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("could not load plugin library {0}: {1}")]
    Load(PathBuf, String),
    #[error("plugin entry point missing: {0}")]
    NoEntry(String),
    #[error("plugin {0} not found in {1}")]
    NotFound(String, PathBuf),
    #[error("plugin failed to initialize: {0}")]
    Init(String),
    #[error("plugin failed to activate: {0}")]
    Activate(String),
    #[error("plugin has no editor")]
    NoEditor,
    #[error("editor error: {0}")]
    Editor(String),
    #[error("{0} plugins are not supported on this platform")]
    Unsupported(&'static str),
    #[error("scan failed: {0}")]
    Scan(String),
    /// The scanner process died or was killed. Whatever it was loading is the culprit.
    #[error("scan crashed: {0}")]
    ScanCrashed(String),
}

pub type Result<T> = std::result::Result<T, HostError>;

/// Something a plugin asked the host to do, reported from [`PluginHandle::idle`].
#[derive(Debug, Clone, PartialEq)]
pub enum HostNotification {
    /// The plugin's own UI changed a parameter (plain value).
    ParamChanged { id: u32, value: f64 },
    /// The parameter list changed; call [`PluginHandle::params`] again.
    ParamsRescanned,
    /// The plugin needs to be re-instantiated (e.g. its I/O layout changed).
    RestartRequested,
    /// The plugin closed its own editor window.
    EditorClosed,
    /// The plugin's latency changed.
    LatencyChanged,
}

/// Main-thread half of a hosted plugin.
pub trait PluginHandle {
    fn descriptor(&self) -> &PluginDescriptor;
    /// Parameter list (plain values for CLAP/AU, normalized 0..1 for VST3).
    fn params(&self) -> Vec<ParamInfo>;
    fn param_value(&self, id: u32) -> Option<f64>;
    /// The plugin's own text for a value, e.g. "-6.0 dB".
    fn format_value(&self, id: u32, value: f64) -> Option<String>;
    /// Tell the main-thread side about a value the host set (keeps VST3 controllers in sync).
    fn on_param_set(&mut self, _id: u32, _value: f64) {}
    fn save_state(&mut self) -> Option<Vec<u8>>;
    fn load_state(&mut self, data: &[u8]) -> bool;
    fn has_editor(&self) -> bool;
    fn is_editor_open(&self) -> bool;
    fn open_editor(&mut self) -> Result<()>;
    fn close_editor(&mut self);
    /// Service plugin callbacks. Call regularly (every UI frame) on the main thread.
    fn idle(&mut self) -> Vec<HostNotification>;
    fn latency(&self) -> u32 {
        0
    }
    /// Hide the open editor window, or show it again, without closing it.
    fn set_editor_visible(&mut self, _visible: bool) {}
    /// The title of the editor window, when it has one of the host's.
    fn set_editor_title(&mut self, _title: &str) {}
    /// Whether the plugin can send MIDI to the devices after it.
    fn midi_output(&self) -> bool {
        false
    }
    /// The presets it can load, for formats that have their own (Audio Units).
    fn presets(&self) -> Vec<PluginPreset> {
        Vec::new()
    }
    /// Switch to one of [`presets`](Self::presets).
    fn load_preset(&mut self, _preset: &PluginPreset) -> bool {
        false
    }
    /// Whether the plugin has an input for a sidechain signal.
    fn sidechain_input(&self) -> bool {
        false
    }
    /// The names of its extra audio outputs, beyond the main pair.
    fn aux_outputs(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A preset of a plugin that keeps its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPreset {
    pub name: String,
    pub source: PresetSource,
}

/// Where a [`PluginPreset`] comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetSource {
    /// Built into the plugin, and read-only.
    Factory(i32),
    /// A file in the system's preset folders.
    File { path: PathBuf, read_only: bool },
}

impl PluginPreset {
    pub fn read_only(&self) -> bool {
        match &self.source {
            PresetSource::Factory(_) => true,
            PresetSource::File { read_only, .. } => *read_only,
        }
    }
}

/// A freshly created plugin instance.
pub struct PluginInstance {
    pub handle: Box<dyn PluginHandle>,
    pub processor: Box<dyn Processor>,
}

/// Default locations searched for plugins of a format.
pub fn default_search_paths(format: PluginFormat) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut v = Vec::new();
    let env_var = match format {
        PluginFormat::Clap => Some("CLAP_PATH"),
        PluginFormat::Vst3 => Some("VST3_PATH"),
        PluginFormat::AudioUnit => None,
    };
    if let Some(var) = env_var.and_then(std::env::var_os) {
        v.extend(std::env::split_paths(&var));
    }
    let dir = match format {
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
        PluginFormat::AudioUnit => return v,
    };
    if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            v.push(h.join("Library/Audio/Plug-Ins").join(dir));
        }
        v.push(PathBuf::from("/Library/Audio/Plug-Ins").join(dir));
    } else if cfg!(target_os = "windows") {
        v.push(PathBuf::from(r"C:\Program Files\Common Files").join(dir));
    } else {
        let lower = dir.to_lowercase();
        if let Some(h) = &home {
            v.push(h.join(format!(".{lower}")));
        }
        v.push(PathBuf::from("/usr/lib").join(&lower));
        v.push(PathBuf::from("/usr/local/lib").join(&lower));
    }
    v
}

/// Find plugin files/bundles (`.clap`, `.vst3`) below the given directories.
pub fn find_plugin_files(format: PluginFormat, dirs: &[PathBuf]) -> Vec<PathBuf> {
    let ext = match format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
        PluginFormat::AudioUnit => return Vec::new(),
    };
    let mut out = Vec::new();
    /// `seen` holds every folder walked, so a link back up the tree can't send this in circles.
    fn walk(
        dir: &Path,
        ext: &str,
        depth: usize,
        seen: &mut HashSet<PathBuf>,
        out: &mut Vec<PathBuf>,
    ) {
        if depth > 6 || !seen.insert(dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            // Symbolic links resolve by themselves, Finder aliases only when asked.
            let p = resolve_alias(&e.path()).unwrap_or_else(|| e.path());
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext)) {
                out.push(p);
            } else if p.is_dir() {
                walk(&p, ext, depth + 1, seen, out);
            }
        }
    }
    let mut seen = HashSet::new();
    for d in dirs {
        walk(d, ext, 0, &mut seen, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Where a Finder alias points, if `path` is one.
fn resolve_alias(path: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        mac::resolve_alias(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// Whether Option is held down at this moment, which skips the scan at launch and makes a
/// rescan start clean.
pub fn alt_key_held() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::option_key_down()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Scan a single plugin file in this process. Prefer [`scanner::scan_out_of_process`].
pub fn scan_file(format: PluginFormat, path: &Path) -> Result<Vec<PluginDescriptor>> {
    match format {
        PluginFormat::Clap => clap::scan(path),
        PluginFormat::Vst3 => vst3::scan(path),
        PluginFormat::AudioUnit => scan_audio_units(),
    }
}

/// Enumerate installed Audio Units (no instantiation needed, so this is safe in-process).
pub fn scan_audio_units() -> Result<Vec<PluginDescriptor>> {
    #[cfg(target_os = "macos")]
    {
        Ok(au::scan())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(HostError::Unsupported("Audio Unit"))
    }
}

/// Which extra buses of a plugin to switch on when it is created. Only VST3 needs telling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PluginBuses {
    pub sidechain: bool,
    pub aux: bool,
}

/// Create a plugin instance. Must be called on the main/UI thread.
pub fn instantiate(
    desc: &PluginDescriptor,
    sample_rate: f64,
    max_block: usize,
) -> Result<PluginInstance> {
    instantiate_with(desc, sample_rate, max_block, PluginBuses::default())
}

/// [`instantiate`], with its extra buses switched on as asked.
pub fn instantiate_with(
    desc: &PluginDescriptor,
    sample_rate: f64,
    max_block: usize,
    buses: PluginBuses,
) -> Result<PluginInstance> {
    match desc.format {
        PluginFormat::Clap => clap::instantiate(desc, sample_rate, max_block),
        PluginFormat::Vst3 => vst3::instantiate(desc, sample_rate, max_block, buses),
        PluginFormat::AudioUnit => {
            #[cfg(target_os = "macos")]
            {
                au::instantiate(desc, sample_rate, max_block)
            }
            #[cfg(not(target_os = "macos"))]
            {
                Err(HostError::Unsupported("Audio Unit"))
            }
        }
    }
}

/// Resolve the shared library inside a plugin bundle (or the file itself).
pub(crate) fn bundle_binary(path: &Path, linux_arch_dir: &str) -> PathBuf {
    if !path.is_dir() {
        return path.to_path_buf();
    }
    let contents = path.join("Contents");
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![contents.join("MacOS")]
    } else if cfg!(target_os = "windows") {
        vec![contents.join("x86_64-win")]
    } else {
        vec![contents.join(linux_arch_dir)]
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    for dir in candidates {
        // Prefer a binary named like the bundle, else the first file in the directory.
        for ext in ["", "so", "dylib", "vst3", "clap"] {
            let mut p = dir.join(&stem);
            if !ext.is_empty() {
                p.set_extension(ext);
            }
            if p.is_file() {
                return p;
            }
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            if let Some(first) = rd.flatten().map(|e| e.path()).find(|p| p.is_file()) {
                return first;
            }
        }
    }
    path.to_path_buf()
}

/// Read a NUL-terminated C string from a fixed buffer.
pub(crate) fn cbuf_to_string(buf: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Read a C string pointer (null → empty).
pub(crate) unsafe fn cptr_to_string(ptr: *const std::ffi::c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

/// Channel-count adapter shared by the plugin processors: stereo engine buffers ↔ plugin ports.
pub(crate) struct PortBuffers {
    pub channels: Vec<Vec<f32>>,
    pub ptrs: Vec<*mut f32>,
}

// The raw pointers only ever point into `channels`, which moves with the struct.
unsafe impl Send for PortBuffers {}

impl PortBuffers {
    pub fn new(channels: usize, max_block: usize) -> Self {
        let mut channels: Vec<Vec<f32>> =
            (0..channels).map(|_| vec![0.0; max_block.max(1)]).collect();
        let ptrs = channels.iter_mut().map(|c| c.as_mut_ptr()).collect();
        Self { channels, ptrs }
    }

    pub fn count(&self) -> usize {
        self.channels.len()
    }

    /// Fill from stereo input (mono ports get the mix).
    pub fn load_stereo(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len();
        match self.channels.len() {
            0 => {}
            1 => {
                for i in 0..n {
                    self.channels[0][i] = (left[i] + right[i]) * 0.5;
                }
            }
            _ => {
                self.channels[0][..n].copy_from_slice(left);
                self.channels[1][..n].copy_from_slice(right);
                for c in self.channels.iter_mut().skip(2) {
                    c[..n].fill(0.0);
                }
            }
        }
    }

    pub fn clear(&mut self, n: usize) {
        for c in self.channels.iter_mut() {
            c[..n].fill(0.0);
        }
    }

    /// Write to stereo output, either replacing (effects) or adding (instruments).
    pub fn store_stereo(&self, left: &mut [f32], right: &mut [f32], add: bool) {
        let n = left.len();
        let (l, r) = match self.channels.len() {
            0 => return,
            1 => (&self.channels[0], &self.channels[0]),
            _ => (&self.channels[0], &self.channels[1]),
        };
        if add {
            for i in 0..n {
                left[i] += l[i];
                right[i] += r[i];
            }
        } else {
            left.copy_from_slice(&l[..n]);
            right.copy_from_slice(&r[..n]);
        }
    }
}

/// A note id as CLAP and VST3 want it: signed, and -1 where it is unassigned. Ids from
/// [`rlive_core::NoteIds`] stay below 2^31, so nothing is lost.
pub(crate) fn wire_note_id(id: rlive_core::NoteId) -> i32 {
    if id == 0 {
        -1
    } else {
        id as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_link_back_up_the_tree_does_not_loop() {
        let root = folder("rlive-walk-loop");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/x.clap"), b"").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("a/back")).unwrap();
        assert_eq!(
            find_plugin_files(PluginFormat::Clap, std::slice::from_ref(&root)),
            vec![root.join("a/x.clap")]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_is_followed() {
        let root = folder("rlive-walk-link");
        let elsewhere = folder("rlive-walk-elsewhere");
        std::fs::write(elsewhere.join("y.vst3"), b"").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("alias")).unwrap();
        assert_eq!(
            find_plugin_files(PluginFormat::Vst3, std::slice::from_ref(&root)),
            vec![root.join("alias/y.vst3")]
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_finder_alias_to_a_folder_is_followed() {
        let root = folder("rlive-walk-alias");
        let elsewhere = folder("rlive-walk-alias-target");
        std::fs::write(elsewhere.join("z.clap"), b"").unwrap();
        assert!(mac::write_alias(&elsewhere, &root.join("Alias")));
        let found = find_plugin_files(PluginFormat::Clap, std::slice::from_ref(&root));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].file_name().unwrap(), "z.clap");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }
}
