//! Sets as files, and the Projects they live in: Save As, templates, and the File Manager's work
//! of finding, replacing and collecting what a Set refers to.
//!
//! Nothing here ever overwrites or deletes a file it wasn't told to. A copy is made in full before
//! a reference moves to it, and a reference that can't be moved stays where it was.

use super::{ClipRef, Session};
use rlive_audio_io::analysis;
use rlive_browser::backup;
use rlive_browser::files::{self, Document, FileIndex, UnusedGroup};
use rlive_browser::preset::{save_clip_file, ClipFile};
use rlive_project::project_folder::{self, imported_folder};
use rlive_project::{ClipContent, Device, DeviceKind, FileRef, Project, FORMAT_VERSION};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

/// Where a referenced file lives, relative to the open Set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileLocation {
    Project,
    UserLibrary,
    OtherProject,
    Elsewhere,
}

impl FileLocation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::UserLibrary => "User Library",
            Self::OtherProject => "Other Projects",
            Self::Elsewhere => "Elsewhere",
        }
    }
}

/// What the File Manager shows about one file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub location: FileLocation,
    pub size: Option<u64>,
    pub missing: bool,
    /// Where in the Set it's used.
    pub uses: Vec<String>,
}

/// The files of a Set in one location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationSummary {
    pub location: FileLocation,
    pub files: usize,
    pub bytes: u64,
}

/// Which locations Collect All copies files from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectOptions {
    pub user_library: bool,
    pub other_projects: bool,
    pub elsewhere: bool,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            user_library: true,
            other_projects: true,
            elsewhere: true,
        }
    }
}

impl CollectOptions {
    fn wants(&self, location: FileLocation) -> bool {
        match location {
            FileLocation::Project => false,
            FileLocation::UserLibrary => self.user_library,
            FileLocation::OtherProject => self.other_projects,
            FileLocation::Elsewhere => self.elsewhere,
        }
    }
}

/// What Collect All did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectReport {
    pub copied: usize,
    pub bytes: u64,
    /// Files that were left where they were because they don't exist.
    pub missing: Vec<PathBuf>,
}

/// Where Automatic Search looks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub folder: Option<PathBuf>,
    pub project: bool,
    pub library: bool,
    pub rescan: bool,
}

/// What Automatic Search found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchReport {
    /// Files with exactly one candidate: these are relinked.
    pub relinked: Vec<(PathBuf, PathBuf)>,
    /// Files with several candidates, for the user to choose between.
    pub choices: Vec<(PathBuf, Vec<PathBuf>)>,
    pub not_found: Vec<PathBuf>,
}

/// What the File Manager is looking after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedScope {
    Set,
    Project(PathBuf),
    UserLibrary,
}

/// What's known about the files of a Project or the User Library on disk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScopeReport {
    pub documents: usize,
    pub missing: Vec<PathBuf>,
    pub external: Vec<LocationSummary>,
    pub unused: Vec<UnusedGroup>,
}

/// Work running on another thread, because a Library or a Project can take a while.
pub(super) enum FileTask {
    Search {
        result: Receiver<(FileIndex, SearchReport)>,
        references: Vec<FileRef>,
    },
    Pack {
        result: Receiver<Result<String, String>>,
    },
}

fn with_extension(path: &Path) -> PathBuf {
    if path.extension().is_none() {
        path.with_extension(rlive_project::FILE_EXTENSION)
    } else {
        path.to_path_buf()
    }
}

fn io_error(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

fn location_of(path: &Path, root: Option<&Path>, library: Option<&Path>) -> FileLocation {
    if root.is_some_and(|r| path.starts_with(r)) {
        FileLocation::Project
    } else if library.is_some_and(|l| path.starts_with(l)) {
        FileLocation::UserLibrary
    } else if project_folder::find_root(path).is_some() {
        FileLocation::OtherProject
    } else {
        FileLocation::Elsewhere
    }
}

fn file_size(file: &FileRef) -> Option<u64> {
    file.size
        .or_else(|| std::fs::metadata(&file.path).ok().map(|m| m.len()))
}

/// Records where every file is relative to the Project, for when the folder moves.
fn note_relative(project: &mut Project, root: &Path) {
    project.for_each_file_ref_mut(&mut |file: &mut FileRef| {
        file.relative = file.path.strip_prefix(root).ok().map(Path::to_path_buf);
    });
}

/// Every reference in a Set, with a description of where it's used.
fn described_refs(p: &Project) -> Vec<(FileRef, String)> {
    fn devices(owner: &str, list: &[Device], out: &mut Vec<(FileRef, String)>) {
        for d in list {
            d.for_each(&mut |d| {
                if let DeviceKind::Builtin { samples, .. } = &d.kind {
                    for f in samples.iter().flatten() {
                        out.push((f.clone(), format!("{owner} / {}", d.name)));
                    }
                }
            });
        }
    }
    let mut out = Vec::new();
    for t in p.tracks.iter().chain(&p.returns) {
        let mut clip = |c: &rlive_project::Clip, place: String| {
            if let ClipContent::Audio { file, .. } = &c.content {
                out.push((file.clone(), format!("{} / {place}{}", t.name, c.name)));
            }
            if let Some(frozen) = &c.frozen {
                out.push((
                    frozen.file.clone(),
                    format!("{} / Frozen {}", t.name, c.name),
                ));
            }
        };
        for (slot, c) in t.clips.iter().enumerate() {
            if let Some(c) = c {
                clip(c, format!("Slot {}: ", slot + 1));
            }
        }
        for c in &t.arrangement {
            clip(&c.clip, String::new());
        }
        for lane in &t.take_lanes {
            for c in &lane.clips {
                clip(&c.clip, format!("{}: ", lane.name));
            }
        }
        devices(&t.name, &t.devices, &mut out);
    }
    devices("Main", &p.master.devices, &mut out);
    out
}

/// Copies a file's analysis along with it, when it has one. A copy that fails only means the new
/// file gets analysed when it's next loaded.
fn copy_sidecar(from: &Path, to: &Path) {
    let (a, b) = (analysis::sidecar_path(from), analysis::sidecar_path(to));
    if a.exists() && !b.exists() {
        let _ = files::copy_new(&a, &b);
    }
}

impl Session {
    // ------------------------------------------------------------- Projects

    /// The folder this Set's files belong in.
    pub fn project_root(&self) -> Option<PathBuf> {
        self.path
            .as_deref()
            .and_then(project_folder::root_or_parent)
    }

    /// The name the title bar shows for the Set's Project, if it's in one.
    pub fn project_name(&self) -> Option<String> {
        let root = project_folder::find_root(self.path.as_deref()?)?;
        Some(project_folder::display_name(&root))
    }

    /// Whether the Set came from an older format, so it can only be saved under a new name.
    pub fn needs_save_as(&self) -> bool {
        self.save_as_required
    }

    /// Saves under a new name: into the Project it's asked to go in, or a new one made for it.
    /// Files elsewhere stay where they are and go on being referred to there.
    pub fn save_as(&mut self, chosen: &Path) -> Result<PathBuf, String> {
        let chosen = with_extension(chosen);
        let (target, root) = project_folder::save_target(&chosen);
        if let Some(root) = &root {
            project_folder::mark(root).map_err(io_error(root))?;
        }
        self.save(&target)?;
        self.save_as_required = false;
        Ok(target)
    }

    /// Saves a copy under a new name, going on working on this Set. Recordings not yet saved
    /// anywhere are copied, not moved, since this Set still needs them.
    pub fn save_copy(&mut self, chosen: &Path) -> Result<PathBuf, String> {
        self.capture_plugin_states();
        self.record_fingerprints();
        let chosen = with_extension(chosen);
        let (target, root) = project_folder::save_target(&chosen);
        if let Some(root) = &root {
            project_folder::mark(root).map_err(io_error(root))?;
        }
        let mut copy = self.project.clone();
        for (from, processed) in &self.temp_recordings {
            let dir = match processed {
                Some(kind) => super::render::processed_folder(Some(&target), None, *kind),
                None => super::takes::recording_folder(Some(&target), None),
            };
            let to = files::collect_into(&dir, from).map_err(io_error(from))?;
            copy.for_each_file_ref_mut(&mut |file: &mut FileRef| {
                if file.path == *from {
                    file.path = to.clone();
                }
            });
        }
        if let Some(root) = project_folder::root_or_parent(&target) {
            note_relative(&mut copy, &root);
        }
        copy.save(&target).map_err(|e| e.to_string())?;
        Ok(target)
    }

    /// Finds out which of the Set's files aren't where they should be.
    pub(super) fn refresh_missing(&mut self) {
        let mut missing = BTreeSet::new();
        let samples = &self.samples;
        self.project.for_each_file_ref(&mut |file: &FileRef| {
            let failed = matches!(
                samples.status(&file.path),
                Some(rlive_audio_io::SampleStatus::Failed(_))
            );
            if failed || !file.path.exists() {
                missing.insert(file.path.clone());
            }
        });
        self.missing = missing;
    }

    /// The files the Set refers to that can't be found.
    pub fn missing_files(&self) -> Vec<PathBuf> {
        self.missing.iter().cloned().collect()
    }

    /// Whether a file is missing, so what uses it is Offline and plays silence.
    pub fn is_offline(&self, path: &Path) -> bool {
        self.missing.contains(path)
    }

    /// Whether this clip's audio can't be found.
    pub fn clip_is_offline(&self, r: ClipRef) -> bool {
        self.clip_at(r).is_some_and(|c| match &c.content {
            ClipContent::Audio { file, .. } => self.is_offline(&file.path),
            ClipContent::Midi { .. } => false,
        })
    }

    // ------------------------------------------------------------- templates

    /// A Set's file, loaded as a new Set with no name.
    pub fn load_as_untitled(&mut self, path: &Path) -> Result<(), String> {
        self.load(path)?;
        self.path = None;
        self.dirty = false;
        self.save_as_required = false;
        Ok(())
    }

    /// Starts a new Set from the default Set if there is one, and from nothing otherwise.
    pub fn new_from_default(&mut self) {
        let default = self.library.as_ref().map(|l| l.default_set_path());
        match default.filter(|d| d.exists()) {
            Some(path) => {
                if let Err(e) = self.load_as_untitled(&path) {
                    self.messages
                        .push(format!("The default Set could not be read: {e}"));
                    self.new_project();
                }
            }
            None => self.new_project(),
        }
    }

    fn write_template_to(&mut self, path: &Path) -> Result<(), String> {
        self.capture_plugin_states();
        self.record_fingerprints();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io_error(dir))?;
        }
        self.project.save(path).map_err(|e| e.to_string())
    }

    /// Saves the Set as a named template in the User Library's Templates folder. A template of
    /// that name already there is kept, and this one is numbered.
    pub fn save_as_template(&mut self, name: &str) -> Result<PathBuf, String> {
        let library = self.library()?;
        let dir = library.templates_dir();
        let safe = rlive_browser::preset::file_name(name);
        let stem = Path::new(&safe)
            .file_stem()
            .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
        let ext = rlive_project::FILE_EXTENSION;
        let path = (1u32..)
            .map(|n| {
                if n == 1 {
                    dir.join(format!("{stem}.{ext}"))
                } else {
                    dir.join(format!("{stem} {n}.{ext}"))
                }
            })
            .find(|p| !p.exists())
            .expect("a free name");
        self.write_template_to(&path)?;
        Ok(path)
    }

    fn back_up_default(path: &Path) -> Result<(), String> {
        if path.exists() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            backup::back_up(path, now, backup::trash_dir().as_deref()).map_err(io_error(path))?;
        }
        Ok(())
    }

    /// Makes the Set what every new Set starts as. The one it replaces goes to the Backup folder.
    pub fn save_as_default_set(&mut self) -> Result<PathBuf, String> {
        let path = self.library()?.default_set_path();
        Self::back_up_default(&path)?;
        self.write_template_to(&path)?;
        Ok(path)
    }

    /// Makes a Set on disk the default, as a template's context menu does.
    pub fn set_default_set_from(&mut self, source: &Path) -> Result<PathBuf, String> {
        let text = std::fs::read_to_string(source).map_err(io_error(source))?;
        Project::from_ron(&text).map_err(|e| e.to_string())?;
        let path = self.library()?.default_set_path();
        Self::back_up_default(&path)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io_error(dir))?;
        }
        rlive_browser::preset::write_atomic(&path, text.as_bytes()).map_err(io_error(&path))?;
        Ok(path)
    }

    // ------------------------------------------------------------- the file list

    /// Where a file is, seen from this Set.
    pub fn file_location(&self, path: &Path) -> FileLocation {
        let root = self.project_root();
        let library = self.library.as_ref().map(|l| l.root.clone());
        location_of(path, root.as_deref(), library.as_deref())
    }

    /// One entry per file the Set refers to, by name.
    pub fn file_list(&self) -> Vec<FileEntry> {
        let root = self.project_root();
        let library = self.library.as_ref().map(|l| l.root.clone());
        let mut by_path: BTreeMap<PathBuf, FileEntry> = BTreeMap::new();
        for (file, place) in described_refs(&self.project) {
            let entry = by_path
                .entry(file.path.clone())
                .or_insert_with(|| FileEntry {
                    name: file
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    location: location_of(&file.path, root.as_deref(), library.as_deref()),
                    size: file_size(&file),
                    missing: !file.path.exists(),
                    path: file.path.clone(),
                    uses: Vec::new(),
                });
            entry.uses.push(place);
        }
        by_path.into_values().collect()
    }

    /// The files outside the Project, as Collect All sees them.
    pub fn location_summaries(&self) -> Vec<LocationSummary> {
        let mut out: BTreeMap<FileLocation, LocationSummary> = BTreeMap::new();
        for entry in self.file_list().into_iter().filter(|e| !e.missing) {
            let s = out.entry(entry.location).or_insert(LocationSummary {
                location: entry.location,
                files: 0,
                bytes: 0,
            });
            s.files += 1;
            s.bytes += entry.size.unwrap_or(0);
        }
        out.into_values().collect()
    }

    // ------------------------------------------------------------- Collect All

    /// Copies the Set's external files into its Project and saves it. If any copy fails, no
    /// reference moves and the Set isn't saved, and the originals are untouched either way.
    pub fn collect_all_and_save(
        &mut self,
        options: CollectOptions,
    ) -> Result<CollectReport, String> {
        let set = self
            .path
            .clone()
            .ok_or("Save the Set before collecting its files")?;
        let root = project_folder::root_or_parent(&set).ok_or("The Set has no folder")?;
        let into = imported_folder(&root);
        let mut report = CollectReport::default();
        let mut moved: HashMap<PathBuf, PathBuf> = HashMap::new();
        for entry in self.file_list() {
            if entry.missing {
                report.missing.push(entry.path);
                continue;
            }
            if !options.wants(entry.location) {
                continue;
            }
            let to = files::collect_into(&into, &entry.path).map_err(io_error(&entry.path))?;
            copy_sidecar(&entry.path, &to);
            report.copied += 1;
            report.bytes += entry.size.unwrap_or(0);
            moved.insert(entry.path, to);
        }
        self.apply_moves(&moved);
        self.save(&set)?;
        Ok(report)
    }

    /// Points every reference at where its file has gone.
    fn apply_moves(&mut self, moved: &HashMap<PathBuf, PathBuf>) {
        if moved.is_empty() {
            return;
        }
        self.project
            .for_each_file_ref_mut(&mut |file: &mut FileRef| {
                if let Some(to) = moved.get(&file.path) {
                    file.path = to.clone();
                    file.relative = None;
                }
            });
        for (from, to) in moved {
            if self.samples.is_ready(from) {
                self.samples.rename(from, to);
            } else {
                // A file still loading finishes under its old name, so ask for the new one.
                self.samples.forget(from);
                self.samples.request(to);
            }
            self.missing.remove(from);
        }
        self.mark_edit("Collect Files");
    }

    // ------------------------------------------------------------- repairing references

    /// Makes every reference to `from` point at `to`, whatever `to` is: the user said so, and
    /// nothing checks it's the same audio (manual p. 148).
    pub fn relink_file(&mut self, from: &Path, to: &Path) -> usize {
        let mut count = 0;
        self.project
            .for_each_file_ref_mut(&mut |file: &mut FileRef| {
                if file.path == from {
                    *file = FileRef::new(to);
                    count += 1;
                }
            });
        if count > 0 {
            self.samples.forget(from);
            self.samples.forget(to);
            self.samples.request(to);
            self.missing.remove(from);
            self.file_candidates.remove(from);
            self.clear_device_errors_for(to);
            self.mark_edit("Relink File");
        }
        count
    }

    fn clear_device_errors_for(&mut self, path: &Path) {
        let mut ids = Vec::new();
        for (file, id) in device_refs(&self.project) {
            if file == path {
                ids.push(id);
            }
        }
        for id in ids {
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.error = None;
            }
        }
    }

    /// Replaces a file everywhere it's used. A clip keeps its settings; its Warp Markers stay
    /// only if the new file is at least as long as the old one (manual p. 141).
    pub fn replace_file(&mut self, from: &Path, to: &Path) -> usize {
        let old = self.probe_seconds(from);
        let new = self.probe_seconds(to);
        let shrinks = matches!((old, new), (Some(o), Some(n)) if n < o);
        if shrinks {
            let ticks = self.project.time_signature.ticks_per_bar();
            let tempo = self.project.tempo;
            self.project.for_each_clip_mut(&mut |clip| {
                if let ClipContent::Audio { file, warp, .. } = &mut clip.content {
                    if file.path == from {
                        warp.markers = match new.filter(|_| warp.enabled) {
                            Some(s) if s > 0.0 => {
                                let bars = rlive_project::warp_edit::suggest_bars(s, ticks, tempo);
                                rlive_project::warp_edit::loop_markers(s, bars, ticks)
                            }
                            _ => Vec::new(),
                        };
                        warp.transients = Default::default();
                    }
                }
            });
        }
        self.relink_file(from, to)
    }

    /// Looks for the missing files and relinks those with one candidate. Waits for the answer.
    pub fn search_missing(&mut self, options: &SearchOptions) -> SearchReport {
        let refs = self.missing_refs();
        let mut index = std::mem::take(&mut self.file_index);
        if options.rescan {
            index.clear();
        }
        for folder in self.search_folders(options) {
            index.add_folder(&folder);
        }
        let report = classify_candidates(&refs, &index);
        self.file_index = index;
        self.apply_search(report)
    }

    /// Starts the same search off the UI thread. Its result turns up in [`poll_file_task`].
    ///
    /// [`poll_file_task`]: Session::poll_file_task
    pub fn start_search(&mut self, options: &SearchOptions) {
        if self.file_task.is_some() {
            return;
        }
        let references = self.missing_refs();
        let mut index = std::mem::take(&mut self.file_index);
        let folders = self.search_folders(options);
        let rescan = options.rescan;
        let refs = references.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            if rescan {
                index.clear();
            }
            for f in &folders {
                index.add_folder(f);
            }
            let report = classify_candidates(&refs, &index);
            let _ = tx.send((index, report));
        });
        self.file_task = Some(FileTask::Search {
            result: rx,
            references,
        });
    }

    /// Whether a search or a pack is running.
    pub fn file_task_running(&self) -> bool {
        self.file_task.is_some()
    }

    /// Picks up work finished on another thread. Called every UI frame.
    pub(super) fn poll_file_task(&mut self) {
        match self.file_task.take() {
            Some(FileTask::Search { result, references }) => match result.try_recv() {
                Ok((index, report)) => {
                    self.file_index = index;
                    let summary = self.apply_search(report);
                    self.messages.push(format!(
                        "Search finished: {} relinked, {} with several candidates, {} not found",
                        summary.relinked.len(),
                        summary.choices.len(),
                        summary.not_found.len()
                    ));
                    self.search_report = Some(summary);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.file_task = Some(FileTask::Search { result, references });
                }
                Err(_) => {}
            },
            Some(FileTask::Pack { result }) => match result.try_recv() {
                Ok(Ok(message)) => self.messages.push(message),
                Ok(Err(e)) => self.messages.push(format!("Could not make the Pack: {e}")),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.file_task = Some(FileTask::Pack { result });
                }
                Err(_) => {}
            },
            None => {}
        }
    }

    fn missing_refs(&self) -> Vec<FileRef> {
        let mut out: BTreeMap<PathBuf, FileRef> = BTreeMap::new();
        self.project.for_each_file_ref(&mut |file: &FileRef| {
            if self.missing.contains(&file.path) {
                out.entry(file.path.clone()).or_insert_with(|| file.clone());
            }
        });
        out.into_values().collect()
    }

    fn search_folders(&self, options: &SearchOptions) -> Vec<PathBuf> {
        let mut folders = Vec::new();
        folders.extend(options.folder.clone());
        if options.project {
            folders.extend(self.project_root());
        }
        if options.library {
            folders.extend(self.library.as_ref().map(|l| l.root.clone()));
        }
        folders
    }

    fn apply_search(&mut self, report: SearchReport) -> SearchReport {
        for (from, to) in &report.relinked {
            self.relink_file(from, to);
        }
        for (from, candidates) in &report.choices {
            self.file_candidates
                .insert(from.clone(), candidates.clone());
        }
        report
    }

    /// What a search left for the user to choose, by file.
    pub fn candidates_for(&self, file: &Path) -> &[PathBuf] {
        self.file_candidates.get(file).map_or(&[], Vec::as_slice)
    }

    // ------------------------------------------------------------- Projects and the Library

    fn documents_of(&self, scope: &ManagedScope) -> Option<(PathBuf, Vec<Document>)> {
        let root = match scope {
            ManagedScope::Set => return None,
            ManagedScope::Project(root) => root.clone(),
            ManagedScope::UserLibrary => self.library.as_ref()?.root.clone(),
        };
        let docs = files::scan_documents(&root);
        Some((root, docs))
    }

    /// What's missing from, outside and unused in a Project or the User Library, on disk.
    pub fn scope_report(&self, scope: &ManagedScope) -> ScopeReport {
        let Some((root, docs)) = self.documents_of(scope) else {
            return ScopeReport {
                documents: 1,
                missing: self.missing_files(),
                external: self.location_summaries(),
                unused: Vec::new(),
            };
        };
        let library = self.library.as_ref().map(|l| l.root.clone());
        let mut seen = BTreeSet::new();
        let mut external: BTreeMap<FileLocation, LocationSummary> = BTreeMap::new();
        let mut missing = Vec::new();
        for doc in &docs {
            for file in &doc.refs {
                if !seen.insert(file.path.clone()) {
                    continue;
                }
                if !file.path.exists() {
                    missing.push(file.path.clone());
                    continue;
                }
                let location = if file.path.starts_with(&root) {
                    FileLocation::Project
                } else {
                    location_of(&file.path, Some(&root), library.as_deref())
                };
                let s = external.entry(location).or_insert(LocationSummary {
                    location,
                    files: 0,
                    bytes: 0,
                });
                s.files += 1;
                s.bytes += file_size(file).unwrap_or(0);
            }
        }
        // A file the open Set uses is in use, whether it has been saved yet or not.
        let mut open = docs;
        open.push(Document {
            path: PathBuf::new(),
            kind: files::DocumentKind::Set,
            refs: described_refs(&self.project)
                .into_iter()
                .map(|(f, _)| f)
                .collect(),
        });
        ScopeReport {
            documents: open.len() - 1,
            missing,
            external: external.into_values().collect(),
            unused: files::unused_files(&root, &open),
        }
    }

    /// Collects the external files of everything in a Project or the User Library into it, and
    /// rewrites what refers to them. Each file rewritten is kept in its Backup folder first.
    pub fn collect_scope(
        &mut self,
        scope: &ManagedScope,
        options: CollectOptions,
    ) -> Result<CollectReport, String> {
        let Some((root, docs)) = self.documents_of(scope) else {
            return self.collect_all_and_save(options);
        };
        let into = match scope {
            ManagedScope::UserLibrary => self.library()?.imported_samples_dir(),
            _ => imported_folder(&root),
        };
        let library = self.library.as_ref().map(|l| l.root.clone());
        let open_set = self.path.clone();
        let mut report = CollectReport::default();
        let mut moved: HashMap<PathBuf, PathBuf> = HashMap::new();
        for doc in &docs {
            for file in &doc.refs {
                if moved.contains_key(&file.path) {
                    continue;
                }
                if !file.path.exists() {
                    if !report.missing.contains(&file.path) {
                        report.missing.push(file.path.clone());
                    }
                    continue;
                }
                let location = location_of(&file.path, Some(&root), library.as_deref());
                let inside_library =
                    matches!(scope, ManagedScope::UserLibrary) && file.path.starts_with(&root);
                if inside_library || !options.wants(location) {
                    continue;
                }
                let to = files::collect_into(&into, &file.path).map_err(io_error(&file.path))?;
                copy_sidecar(&file.path, &to);
                report.copied += 1;
                report.bytes += file_size(file).unwrap_or(0);
                moved.insert(file.path.clone(), to);
            }
        }
        let change = |file: &mut FileRef| {
            if let Some(to) = moved.get(&file.path) {
                file.path = to.clone();
                file.relative = None;
            }
        };
        for doc in &docs {
            // The open Set is saved below, from what's in memory, not over it.
            if Some(&doc.path) == open_set.as_ref() || doc.refs.is_empty() {
                continue;
            }
            let mut change = change;
            files::rewrite_document(doc, &mut change).map_err(io_error(&doc.path))?;
        }
        if open_set.as_deref().is_some_and(|p| p.starts_with(&root)) {
            self.apply_moves(&moved);
            let set = open_set.expect("checked above");
            self.save(&set)?;
        }
        Ok(report)
    }

    /// Repairs references in a Project or the User Library by searching, as for the Set.
    pub fn repair_scope(
        &mut self,
        scope: &ManagedScope,
        options: &SearchOptions,
    ) -> Result<SearchReport, String> {
        let Some((_, docs)) = self.documents_of(scope) else {
            return Ok(self.search_missing(options));
        };
        let mut refs: BTreeMap<PathBuf, FileRef> = BTreeMap::new();
        for file in docs.iter().flat_map(|d| &d.refs) {
            if !file.path.exists() {
                refs.entry(file.path.clone())
                    .or_insert_with(|| file.clone());
            }
        }
        let refs: Vec<FileRef> = refs.into_values().collect();
        let mut index = std::mem::take(&mut self.file_index);
        if options.rescan {
            index.clear();
        }
        for folder in self.search_folders(options) {
            index.add_folder(&folder);
        }
        let report = classify_candidates(&refs, &index);
        self.file_index = index;
        let found: HashMap<&Path, &Path> = report
            .relinked
            .iter()
            .map(|(a, b)| (a.as_path(), b.as_path()))
            .collect();
        for doc in &docs {
            if Some(&doc.path) == self.path.as_ref() || doc.refs.iter().all(|r| r.path.exists()) {
                continue;
            }
            let mut change = |file: &mut FileRef| {
                if let Some(to) = found.get(file.path.as_path()) {
                    *file = FileRef::new(*to);
                }
            };
            files::rewrite_document(doc, &mut change).map_err(io_error(&doc.path))?;
        }
        Ok(self.apply_search(report))
    }

    /// Moves files the File Manager listed as unused to the Trash.
    pub fn trash_unused(&mut self, files: &[PathBuf]) -> Result<usize, String> {
        let mut done = 0;
        for path in files {
            backup::to_trash(path).map_err(io_error(path))?;
            done += 1;
        }
        Ok(done)
    }

    /// Packs a Project into one file, off the UI thread.
    pub fn start_pack(&mut self, root: PathBuf, pack: PathBuf) {
        if self.file_task.is_some() {
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = files::write_pack(&root, &pack)
                .map(|n| format!("Pack created: {} ({n} files)", pack.display()))
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.file_task = Some(FileTask::Pack { result: rx });
    }

    /// Unpacks a Pack under `parent` and returns the new folder.
    pub fn install_pack(&mut self, pack: &Path, parent: &Path) -> Result<PathBuf, String> {
        files::install_pack(pack, parent).map_err(io_error(pack))
    }

    // ------------------------------------------------------------- saving clips

    /// Saves a clip, with its track's devices, for the browser. With `copy` its samples go along:
    /// into the Library's imported samples if `dir` is in the Library, and otherwise into a
    /// Project folder made for the clip. Returns the clip file.
    pub fn save_clip_to(
        &mut self,
        r: ClipRef,
        dir: &Path,
        name: &str,
        copy: bool,
    ) -> Result<PathBuf, String> {
        let clip = self.clip_at(r).ok_or("That clip is gone")?.clone();
        self.capture_plugin_states();
        let track = self.project.track(r.track()).ok_or("That track is gone")?;
        let mut file = ClipFile::new(clip);
        file.track = Some(track.kind);
        file.devices = track.devices.clone();
        let stem = rlive_browser::preset::file_name(name);
        let stem = Path::new(&stem)
            .file_stem()
            .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
        let library = self.library.as_ref().filter(|l| dir.starts_with(&l.root));
        let (folder, samples) = match (copy, library) {
            (false, _) => (dir.to_path_buf(), None),
            (true, Some(l)) => (dir.to_path_buf(), Some(l.imported_samples_dir())),
            (true, None) if self.needs_no_project(dir) => {
                (dir.to_path_buf(), Some(imported_folder(dir)))
            }
            (true, None) => {
                let root = project_folder::free_folder(dir, &format!("{stem} Project"));
                project_folder::mark(&root).map_err(io_error(&root))?;
                (root.clone(), Some(imported_folder(&root)))
            }
        };
        if let Some(samples) = samples {
            let mut failed = None;
            file.for_each_file_ref_mut(&mut |f: &mut FileRef| {
                if failed.is_some() || !f.path.exists() {
                    return;
                }
                match files::collect_into(&samples, &f.path) {
                    Ok(to) => *f = FileRef::new(to),
                    Err(e) => failed = Some(format!("{}: {e}", f.path.display())),
                }
            });
            if let Some(e) = failed {
                return Err(e);
            }
        }
        let ext = rlive_browser::scan::CLIP_EXT;
        let path = (1u32..)
            .map(|n| {
                if n == 1 {
                    folder.join(format!("{stem}.{ext}"))
                } else {
                    folder.join(format!("{stem} {n}.{ext}"))
                }
            })
            .find(|p| !p.exists())
            .expect("a free name");
        save_clip_file(&path, &file).map_err(io_error(&path))?;
        Ok(path)
    }

    /// Whether a folder already is, or is inside, a Project.
    fn needs_no_project(&self, dir: &Path) -> bool {
        dir.join(project_folder::MARKER).exists() || project_folder::find_root(dir).is_some()
    }

    /// The first clip anywhere in the Set that plays this file, for opening it in an editor.
    pub fn clip_using_file(&self, path: &Path) -> Option<ClipRef> {
        for t in self.project.tracks.iter().chain(&self.project.returns) {
            let plays = |c: &rlive_project::Clip| match &c.content {
                ClipContent::Audio { file, .. } => file.path == path,
                ClipContent::Midi { .. } => false,
            };
            if let Some(slot) = t.clips.iter().position(|c| c.as_ref().is_some_and(plays)) {
                return Some(ClipRef::slot(t.id, slot));
            }
            if let Some(c) = t.arrangement.iter().find(|c| plays(&c.clip)) {
                return Some(ClipRef::Arr {
                    track: t.id,
                    id: c.id,
                });
            }
        }
        None
    }

    /// Puts a saved clip in a slot. Its devices come with it only onto a track with no devices
    /// or clips of its own (manual p. 134).
    pub fn place_clip_file(&mut self, track: rlive_core::TrackId, slot: usize, file: ClipFile) {
        let empty = self.project.track(track).is_some_and(|t| {
            t.devices.is_empty() && t.clips.iter().all(Option::is_none) && t.arrangement.is_empty()
        });
        if empty {
            for d in &file.devices {
                self.add_device(
                    super::ChainRef::Track(track),
                    None,
                    super::rack_edit::fresh_device(d),
                );
            }
        }
        self.set_clip(track, slot, Some(file.clip));
    }

    /// Exports Session clips as a new Set: the tracks they are on, with only those clips.
    pub fn export_clips_as_set(
        &mut self,
        clips: &[(rlive_core::TrackId, usize)],
        chosen: &Path,
    ) -> Result<PathBuf, String> {
        self.capture_plugin_states();
        let mut p = self.project.clone();
        p.tracks.retain(|t| clips.iter().any(|(id, _)| *id == t.id));
        for t in &mut p.tracks {
            t.arrangement.clear();
            t.take_lanes.clear();
            for (slot, clip) in t.clips.iter_mut().enumerate() {
                if !clips.contains(&(t.id, slot)) {
                    *clip = None;
                }
            }
        }
        let chosen = with_extension(chosen);
        let (target, root) = project_folder::save_target(&chosen);
        if let Some(root) = &root {
            project_folder::mark(root).map_err(io_error(root))?;
        }
        p.save(&target).map_err(|e| e.to_string())?;
        Ok(target)
    }

    /// Adds another Set's tracks to this one, with their clips, devices and automation. Its
    /// return tracks stay behind, so its sends are off (manual p. 136).
    pub fn merge_set(&mut self, path: &Path) -> Result<Vec<rlive_core::TrackId>, String> {
        let text = std::fs::read_to_string(path).map_err(io_error(path))?;
        let source = Project::from_ron(&text).map_err(|e| e.to_string())?;
        let mut track_ids = HashMap::new();
        let mut device_ids = HashMap::new();
        let mut tracks = Vec::with_capacity(source.tracks.len());
        for t in &source.tracks {
            let mut new = t.clone();
            new.id = rlive_core::TrackId::new();
            track_ids.insert(t.id, new.id);
            new.devices = t
                .devices
                .iter()
                .map(|d| {
                    let copy = super::rack_edit::fresh_device(d);
                    collect_device_ids(d, &copy, &mut device_ids);
                    copy
                })
                .collect();
            for clip in new.arrangement.iter_mut() {
                clip.id = rlive_core::ArrClipId::new();
            }
            for lane in &mut new.take_lanes {
                for clip in &mut lane.clips {
                    clip.id = rlive_core::ArrClipId::new();
                }
            }
            tracks.push(new);
        }
        while self.project.scenes.len() < source.tracks.first().map_or(0, |t| t.clips.len()) {
            self.project.add_scene();
        }
        let scenes = self.project.scenes.len();
        let mut added = Vec::with_capacity(tracks.len());
        for mut t in tracks {
            t.group = t.group.and_then(|g| track_ids.get(&g).copied());
            t.sends.clear();
            t.clips.resize(scenes, None);
            for d in &mut t.devices {
                d.for_each_mut(&mut |d| {
                    if let Some(sc) = &mut d.sidechain {
                        match track_ids.get(&sc.track) {
                            Some(new) => sc.track = *new,
                            None => d.sidechain = None,
                        }
                    }
                });
            }
            remap_route(&mut t.routing.output, &track_ids);
            remap_route(&mut t.routing.input, &track_ids);
            for e in &mut t.automation {
                remap_target(&mut e.target, &track_ids, &device_ids);
            }
            let mut clip_envelopes = |c: &mut rlive_project::Clip| {
                c.groove = None;
                for e in &mut c.envelopes {
                    remap_target(&mut e.envelope.target, &track_ids, &device_ids);
                }
            };
            t.clips.iter_mut().flatten().for_each(&mut clip_envelopes);
            t.arrangement
                .iter_mut()
                .for_each(|c| clip_envelopes(&mut c.clip));
            t.take_lanes
                .iter_mut()
                .flat_map(|l| &mut l.clips)
                .for_each(|c| clip_envelopes(&mut c.clip));
            let id = t.id;
            let index = self.project.tracks.len();
            self.install_track(t, index);
            added.push(id);
        }
        for path in self.project_audio_paths() {
            self.samples.request(&path);
        }
        self.sync_scenes();
        self.refresh_missing();
        self.mark_edit("Merge Set");
        Ok(added)
    }

    /// The folder this Set's presets go in: its Project's Presets folder when it has a Project.
    pub fn project_presets_dir(&self) -> Option<PathBuf> {
        let root = project_folder::find_root(self.path.as_deref()?)?;
        Some(root.join(project_folder::PRESETS))
    }
}

fn collect_device_ids(
    old: &Device,
    new: &Device,
    map: &mut HashMap<rlive_core::DeviceId, rlive_core::DeviceId>,
) {
    let mut from = Vec::new();
    let mut to = Vec::new();
    old.for_each(&mut |d| from.push(d.id));
    new.for_each(&mut |d| to.push(d.id));
    map.extend(from.into_iter().zip(to));
}

fn remap_route(
    route: &mut rlive_project::Route,
    tracks: &HashMap<rlive_core::TrackId, rlive_core::TrackId>,
) {
    if let rlive_project::Route::Track { track, .. } = route {
        match tracks.get(track) {
            Some(new) => *track = *new,
            None => *route = rlive_project::Route::Default,
        }
    }
}

fn remap_target(
    target: &mut rlive_core::param::EnvTarget,
    tracks: &HashMap<rlive_core::TrackId, rlive_core::TrackId>,
    devices: &HashMap<rlive_core::DeviceId, rlive_core::DeviceId>,
) {
    use rlive_core::param::{EnvTarget, ParamOwner};
    if let EnvTarget::Param(p) = target {
        p.owner = match p.owner {
            ParamOwner::Track(t) => ParamOwner::Track(*tracks.get(&t).unwrap_or(&t)),
            ParamOwner::Device(d) => ParamOwner::Device(*devices.get(&d).unwrap_or(&d)),
            other => other,
        };
    }
}

/// Sorts missing files by how many candidates a search turned up.
fn classify_candidates(refs: &[FileRef], index: &FileIndex) -> SearchReport {
    let mut report = SearchReport::default();
    for file in refs {
        let candidates = index.candidates(file);
        match candidates.as_slice() {
            [] => report.not_found.push(file.path.clone()),
            [one] => report.relinked.push((file.path.clone(), one.clone())),
            _ => report.choices.push((file.path.clone(), candidates)),
        }
    }
    report
}

/// Every sample path a device holds, with the device it's in.
fn device_refs(p: &Project) -> Vec<(PathBuf, rlive_core::DeviceId)> {
    let mut out = Vec::new();
    let mut scan = |d: &Device| {
        if let DeviceKind::Builtin { samples, .. } = &d.kind {
            for f in samples.iter().flatten() {
                out.push((f.path.clone(), d.id));
            }
        }
    };
    for t in p.tracks.iter().chain(&p.returns) {
        t.devices.iter().for_each(|d| d.for_each(&mut scan));
    }
    p.master.devices.iter().for_each(|d| d.for_each(&mut scan));
    out
}

/// Whether a Set of this format can be saved over.
pub(super) fn format_is_current(text: &str) -> bool {
    Project::format_of(text).is_ok_and(|v| v == FORMAT_VERSION)
}
