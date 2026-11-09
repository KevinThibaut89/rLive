//! The browser's database, on a worker thread of its own.

use crate::catalog;
use crate::item::{Item, ItemKey, ItemKind, Label};
use crate::library::{LibraryData, Op, LIBRARY_VERSION};
use crate::preset::write_atomic;
use crate::query::{compare, Matcher, Query, Results, Row, Scope};
use crate::scan::{self, Found, ScanConfig};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often a running scan refreshes the results on show.
const RESULTS_REFRESH: Duration = Duration::from_millis(150);
/// How long after an edit it reaches the disk.
const SAVE_DELAY: Duration = Duration::from_millis(400);

#[derive(Debug, Clone)]
pub struct Config {
    /// Where the database lives.
    pub data_dir: PathBuf,
    pub user_library: PathBuf,
    pub audio_extensions: Vec<String>,
}

impl Config {
    pub fn library_file(&self) -> PathBuf {
        self.data_dir.join("browser-library.json")
    }

    pub fn index_file(&self) -> PathBuf {
        self.data_dir.join("browser-index.json")
    }
}

/// How the scan of a folder is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanState {
    pub folder: PathBuf,
    pub files: usize,
    pub running: bool,
    /// The folder wasn't where it was expected.
    pub missing: bool,
}

/// What the worker tells the UI.
enum Event {
    Results(Arc<Results>),
    Library(Arc<LibraryData>),
    Scan(ScanState),
    Error(String),
}

enum Cmd {
    Op(Op),
    Query(Query),
    /// Replaces the items of a named source: built-ins, plug-ins, the Current Project.
    Catalog(String, Vec<Item>),
    ScanBatch(PathBuf, u64, Vec<Item>),
    ScanDone(PathBuf, u64, bool),
    Rescan,
    SetUserLibrary(PathBuf),
    SetProjectFolder(Option<PathBuf>),
    Flush(mpsc::Sender<()>),
    Shutdown,
}

/// The UI's handle on the database. No method waits for the disk or the worker, except
/// [`flush_blocking`](Self::flush_blocking), which is for tests and shutdown.
pub struct BrowserDb {
    tx: mpsc::Sender<Cmd>,
    rx: mpsc::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
    library: Arc<LibraryData>,
    results: Arc<Results>,
    scans: BTreeMap<PathBuf, ScanState>,
    next_query: u64,
    errors: Vec<String>,
}

impl BrowserDb {
    /// Opens the database. It loads, and starts its first scans, on the worker.
    pub fn open(config: Config) -> Self {
        Self::open_with_waker(config, Arc::new(|| {}))
    }

    /// Opens the database; `wake` is called from the worker whenever there's news.
    pub fn open_with_waker(config: Config, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, cmd_rx) = mpsc::channel();
        let (event_tx, rx) = mpsc::channel();
        let worker_tx = tx.clone();
        let thread = std::thread::Builder::new()
            .name("rlive-browser".into())
            .spawn(move || Worker::new(config, cmd_rx, worker_tx, event_tx, wake).run())
            .expect("spawn browser thread");
        Self {
            tx,
            rx,
            thread: Some(thread),
            library: Arc::new(LibraryData::default()),
            results: Arc::new(Results::default()),
            scans: BTreeMap::new(),
            next_query: 0,
            errors: Vec::new(),
        }
    }

    pub fn op(&self, op: Op) {
        let _ = self.tx.send(Cmd::Op(op));
    }

    pub fn set_catalog(&self, source: &str, items: Vec<Item>) {
        let _ = self.tx.send(Cmd::Catalog(source.to_string(), items));
    }

    pub fn rescan(&self) {
        let _ = self.tx.send(Cmd::Rescan);
    }

    pub fn set_user_library(&self, path: PathBuf) {
        let _ = self.tx.send(Cmd::SetUserLibrary(path));
    }

    /// Tells the worker which Project is open, whose folder is scanned for the Current Project.
    pub fn set_project_folder(&self, path: Option<PathBuf>) {
        let _ = self.tx.send(Cmd::SetProjectFolder(path));
    }

    /// Sends a query; its id comes back with its results.
    pub fn query(&mut self, mut q: Query) -> u64 {
        self.next_query += 1;
        q.id = self.next_query;
        let id = q.id;
        let _ = self.tx.send(Cmd::Query(q));
        id
    }

    /// Picks up whatever the worker has said since last time. Returns whether anything did.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok(event) = self.rx.try_recv() {
            any = true;
            match event {
                Event::Results(r) => {
                    // Rows from an older query are never shown.
                    if r.id >= self.results.id {
                        self.results = r;
                    }
                }
                Event::Library(l) => self.library = l,
                Event::Scan(s) => {
                    self.scans.insert(s.folder.clone(), s);
                }
                Event::Error(e) => self.errors.push(e),
            }
        }
        any
    }

    pub fn library(&self) -> &Arc<LibraryData> {
        &self.library
    }

    pub fn results(&self) -> &Arc<Results> {
        &self.results
    }

    pub fn scans(&self) -> impl Iterator<Item = &ScanState> {
        self.scans.values()
    }

    /// Whether anything is being scanned.
    pub fn scanning(&self) -> bool {
        self.scans.values().any(|s| s.running)
    }

    pub fn scan_of(&self, folder: &Path) -> Option<&ScanState> {
        self.scans.get(folder)
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    /// Waits for the worker to finish what it's been told and write it out.
    pub fn flush_blocking(&mut self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Cmd::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
        self.poll();
    }
}

impl Drop for BrowserDb {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Entry {
    item: Arc<Item>,
    /// The item's search text.
    hay: String,
}

struct Scan {
    generation: u64,
    cancel: Arc<AtomicBool>,
    seen: HashSet<ItemKey>,
    files: usize,
}

struct Worker {
    config: Config,
    cmd_rx: mpsc::Receiver<Cmd>,
    cmd_tx: mpsc::Sender<Cmd>,
    events: mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    lib: LibraryData,
    /// Items by key.
    items: HashMap<ItemKey, Entry>,
    /// Keys each catalog source put in, so they can be replaced whole.
    sources: HashMap<String, Vec<ItemKey>>,
    roots: BTreeSet<PathBuf>,
    project_folder: Option<PathBuf>,
    by_parent: HashMap<ItemKey, Vec<Arc<Item>>>,
    by_device: HashMap<String, Vec<Arc<Item>>>,
    index_stale: bool,
    scans: HashMap<PathBuf, Scan>,
    next_generation: u64,
    last_query: Option<Query>,
    results_stale: Option<Instant>,
    library_dirty: Option<Instant>,
    index_dirty: bool,
}

impl Worker {
    fn new(
        config: Config,
        cmd_rx: mpsc::Receiver<Cmd>,
        cmd_tx: mpsc::Sender<Cmd>,
        events: mpsc::Sender<Event>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            config,
            cmd_rx,
            cmd_tx,
            events,
            wake,
            lib: LibraryData::default(),
            items: HashMap::new(),
            sources: HashMap::new(),
            roots: BTreeSet::new(),
            project_folder: None,
            by_parent: HashMap::new(),
            by_device: HashMap::new(),
            index_stale: true,
            scans: HashMap::new(),
            next_generation: 0,
            last_query: None,
            results_stale: None,
            library_dirty: None,
            index_dirty: false,
        }
    }

    fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            (self.wake)();
        }
    }

    fn publish_library(&self) {
        self.send(Event::Library(Arc::new(self.lib.clone())));
    }

    fn run(mut self) {
        self.load();
        self.publish_library();
        let _ =
            crate::defaults::UserLibrary::new(self.config.user_library.clone()).ensure_folders();
        self.rescan_all();
        loop {
            let wait = Duration::from_millis(40);
            match self.cmd_rx.recv_timeout(wait) {
                Ok(Cmd::Shutdown) => {
                    self.cancel_scans();
                    self.save_all();
                    return;
                }
                Ok(cmd) => {
                    self.handle(cmd);
                    // Take everything that has queued up before doing the slow parts.
                    while let Ok(cmd) = self.cmd_rx.try_recv() {
                        if matches!(cmd, Cmd::Shutdown) {
                            self.cancel_scans();
                            self.save_all();
                            return;
                        }
                        self.handle(cmd);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.cancel_scans();
                    self.save_all();
                    return;
                }
            }
            self.tick();
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self
            .results_stale
            .is_some_and(|since| now.duration_since(since) >= RESULTS_REFRESH)
        {
            self.results_stale = None;
            self.run_last_query();
        }
        if self
            .library_dirty
            .is_some_and(|since| now.duration_since(since) >= SAVE_DELAY)
        {
            self.save_library();
        }
        if self.index_dirty && self.scans.is_empty() {
            self.save_index();
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Op(op) => self.apply(op),
            Cmd::Query(q) => {
                let r = self.run_query(&q);
                self.last_query = Some(q);
                self.results_stale = None;
                self.send(Event::Results(Arc::new(r)));
            }
            Cmd::Catalog(source, items) => self.set_catalog(source, items),
            Cmd::ScanBatch(root, gen, items) => self.take_batch(root, gen, items),
            Cmd::ScanDone(root, gen, exists) => self.finish_scan(root, gen, exists),
            Cmd::Rescan => self.rescan_all(),
            Cmd::SetUserLibrary(path) => {
                self.config.user_library = path;
                let _ = crate::defaults::UserLibrary::new(self.config.user_library.clone())
                    .ensure_folders();
                self.rescan_all();
            }
            Cmd::SetProjectFolder(path) => {
                if self.project_folder != path {
                    self.project_folder = path;
                    self.rescan_all();
                }
            }
            Cmd::Flush(ack) => {
                if let Some(q) = self.last_query.clone() {
                    let r = self.run_query(&q);
                    self.send(Event::Results(Arc::new(r)));
                }
                self.save_all();
                let _ = ack.send(());
            }
            Cmd::Shutdown => {}
        }
    }

    fn apply(&mut self, op: Op) {
        if let Err(e) = self.lib.apply(&op) {
            self.send(Event::Error(e.to_string()));
            return;
        }
        self.library_dirty = Some(Instant::now());
        match &op {
            Op::AddFolder(_) | Op::RemoveFolder(_) | Op::LocateFolder(..) => self.rescan_all(),
            _ => {}
        }
        self.publish_library();
        // Tags and colours change what's shown.
        self.results_stale
            .get_or_insert(Instant::now() - RESULTS_REFRESH);
    }

    fn load(&mut self) {
        let _ = std::fs::create_dir_all(&self.config.data_dir);
        if let Ok(text) = std::fs::read_to_string(self.config.library_file()) {
            match serde_json::from_str::<LibraryData>(&text) {
                Ok(lib) if lib.version <= LIBRARY_VERSION => self.lib = lib,
                Ok(lib) => log::warn!("browser library is from a newer version ({})", lib.version),
                Err(e) => log::warn!("could not read the browser library: {e}"),
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.config.index_file()) {
            if let Ok(items) = serde_json::from_str::<Vec<Item>>(&text) {
                for item in items {
                    self.insert(item);
                }
            }
        }
    }

    fn save_library(&mut self) {
        self.library_dirty = None;
        match serde_json::to_vec_pretty(&self.lib) {
            Ok(bytes) => {
                if let Err(e) = write_atomic(&self.config.library_file(), &bytes) {
                    log::warn!("could not save the browser library: {e}");
                }
            }
            Err(e) => log::warn!("could not encode the browser library: {e}"),
        }
    }

    fn save_index(&mut self) {
        self.index_dirty = false;
        let items: Vec<&Item> = self
            .items
            .values()
            .filter(|e| e.item.path.is_some())
            .map(|e| e.item.as_ref())
            .collect();
        match serde_json::to_vec(&items) {
            Ok(bytes) => {
                if let Err(e) = write_atomic(&self.config.index_file(), &bytes) {
                    log::warn!("could not save the browser index: {e}");
                }
            }
            Err(e) => log::warn!("could not encode the browser index: {e}"),
        }
    }

    fn save_all(&mut self) {
        self.save_library();
        if self.index_dirty {
            self.save_index();
        }
    }

    fn insert(&mut self, item: Item) {
        let hay = item.haystack();
        self.items.insert(
            item.key.clone(),
            Entry {
                item: Arc::new(item),
                hay,
            },
        );
        self.index_stale = true;
    }

    fn set_catalog(&mut self, source: String, items: Vec<Item>) {
        for key in self.sources.remove(&source).unwrap_or_default() {
            self.items.remove(&key);
        }
        let keys = items.iter().map(|i| i.key.clone()).collect();
        for item in items {
            self.insert(item);
        }
        self.sources.insert(source, keys);
        self.index_stale = true;
        self.results_stale
            .get_or_insert(Instant::now() - RESULTS_REFRESH);
    }

    // ------------------------------------------------------------- scanning

    fn cancel_scans(&mut self) {
        for s in self.scans.values() {
            s.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn wanted_roots(&self) -> BTreeSet<PathBuf> {
        let mut roots: BTreeSet<PathBuf> =
            self.lib.folders.iter().map(|f| f.path.clone()).collect();
        roots.insert(self.config.user_library.clone());
        roots.extend(self.project_folder.clone());
        roots
    }

    /// Starts a scan of every folder, dropping what a removed folder had put in.
    fn rescan_all(&mut self) {
        let wanted = self.wanted_roots();
        let dropped: Vec<PathBuf> = self.roots.difference(&wanted).cloned().collect();
        for root in dropped {
            if let Some(s) = self.scans.remove(&root) {
                s.cancel.store(true, Ordering::Relaxed);
            }
            let inside: Vec<ItemKey> = self
                .items
                .iter()
                .filter(|(_, e)| {
                    e.item.path.as_deref().is_some_and(|p| p.starts_with(&root))
                        && !wanted
                            .iter()
                            .any(|w| e.item.path.as_deref().is_some_and(|p| p.starts_with(w)))
                })
                .map(|(k, _)| k.clone())
                .collect();
            for k in inside {
                self.items.remove(&k);
            }
            self.index_stale = true;
            self.index_dirty = true;
        }
        self.roots = wanted.clone();
        for root in wanted {
            self.start_scan(root);
        }
        self.results_stale.get_or_insert(Instant::now());
    }

    fn start_scan(&mut self, root: PathBuf) {
        if let Some(old) = self.scans.remove(&root) {
            old.cancel.store(true, Ordering::Relaxed);
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.scans.insert(
            root.clone(),
            Scan {
                generation,
                cancel: cancel.clone(),
                seen: HashSet::new(),
                files: 0,
            },
        );
        self.send(Event::Scan(ScanState {
            folder: root.clone(),
            files: 0,
            running: true,
            missing: false,
        }));
        let tx = self.cmd_tx.clone();
        let config = ScanConfig {
            audio_extensions: self.config.audio_extensions.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("rlive-browser-scan".into())
            .spawn(move || {
                let at = root.clone();
                scan::walk(&at, &config, &cancel, |found| match found {
                    Found::Batch(items) => {
                        let _ = tx.send(Cmd::ScanBatch(root.clone(), generation, items));
                    }
                    Found::Done { exists } => {
                        let _ = tx.send(Cmd::ScanDone(root.clone(), generation, exists));
                    }
                });
            });
        if let Err(e) = spawned {
            log::warn!("could not start a browser scan: {e}");
        }
    }

    fn take_batch(&mut self, root: PathBuf, generation: u64, items: Vec<Item>) {
        let Some(scan) = self
            .scans
            .get_mut(&root)
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        scan.files += items.len();
        scan.seen.extend(items.iter().map(|i| i.key.clone()));
        let state = ScanState {
            folder: root.clone(),
            files: scan.files,
            running: true,
            missing: false,
        };
        for item in items {
            self.insert(item);
        }
        self.index_dirty = true;
        self.send(Event::Scan(state));
        self.results_stale.get_or_insert(Instant::now());
    }

    fn finish_scan(&mut self, root: PathBuf, generation: u64, exists: bool) {
        if !self
            .scans
            .get(&root)
            .is_some_and(|s| s.generation == generation)
        {
            return;
        }
        let scan = self.scans.remove(&root).expect("just checked");
        if exists {
            let stale: Vec<ItemKey> = self
                .items
                .iter()
                .filter(|(k, e)| {
                    e.item.path.as_deref().is_some_and(|p| p.starts_with(&root))
                        && !scan.seen.contains(*k)
                        && !self.scans.keys().any(|r| {
                            r != &root && e.item.path.as_deref().is_some_and(|p| p.starts_with(r))
                        })
                })
                .map(|(k, _)| k.clone())
                .collect();
            if !stale.is_empty() {
                for k in stale {
                    self.items.remove(&k);
                }
                self.index_stale = true;
            }
        }
        self.index_dirty = true;
        self.send(Event::Scan(ScanState {
            folder: root,
            files: scan.files,
            running: false,
            missing: !exists,
        }));
        self.results_stale
            .get_or_insert(Instant::now() - RESULTS_REFRESH);
    }

    // ------------------------------------------------------------- queries

    fn run_last_query(&mut self) {
        if let Some(q) = self.last_query.clone() {
            let r = self.run_query(&q);
            self.send(Event::Results(Arc::new(r)));
        }
    }

    /// Brings the parent and device lookups up to date.
    fn reindex(&mut self) {
        if !self.index_stale {
            return;
        }
        self.index_stale = false;
        self.by_parent.clear();
        self.by_device.clear();
        for e in self.items.values() {
            if let Some(p) = &e.item.parent {
                self.by_parent
                    .entry(p.clone())
                    .or_default()
                    .push(e.item.clone());
            }
            if e.item.kind == ItemKind::Preset {
                if let Some(d) = &e.item.device {
                    self.by_device
                        .entry(d.clone())
                        .or_default()
                        .push(e.item.clone());
                }
            }
        }
    }

    fn run_query(&mut self, q: &Query) -> Results {
        self.reindex();
        let matcher_lib = self.lib.clone();
        let (scope, text, tags) = self.resolve_scope(q);
        let matcher = Matcher::new(&text, &tags, &matcher_lib);
        let search = !text.trim().is_empty() || !tags.is_empty();
        let rows = if search || q.is_search() {
            self.flat(&scope, &matcher, q)
        } else {
            self.tree(&scope, &matcher, q)
        };
        Results {
            id: q.id,
            rows,
            search: q.is_search(),
        }
    }

    /// A saved search stands for its own query.
    fn resolve_scope(&self, q: &Query) -> (Scope, String, BTreeSet<crate::tags::TagId>) {
        if let Scope::Custom(id) = &q.scope {
            if let Some(l) = self.lib.custom_labels.iter().find(|l| l.id == *id) {
                let mut text = l.text.clone();
                if !q.text.trim().is_empty() {
                    text.push(' ');
                    text.push_str(&q.text);
                }
                let mut tags = l.tags.clone();
                tags.extend(&q.tags);
                return (l.scope.clone(), text, tags);
            }
        }
        (q.scope.clone(), q.text.clone(), q.tags.clone())
    }

    fn in_scope(&self, item: &Item, scope: &Scope, m: &Matcher<'_>) -> bool {
        match scope {
            Scope::Label(l) => item.kind != ItemKind::ProjectPart && m.in_label(item, *l),
            Scope::Collection(n) => self.lib.colors_of(&item.key) & (1 << n) != 0,
            Scope::Custom(_) => false,
            Scope::Place(root) => item.path.as_deref().is_some_and(|p| p.starts_with(root)),
            Scope::Project => {
                item.kind == ItemKind::ProjectPart
                    || self
                        .project_folder
                        .as_deref()
                        .is_some_and(|f| item.path.as_deref().is_some_and(|p| p.starts_with(f)))
            }
            Scope::Similar(_) => false,
        }
    }

    fn row(&self, item: &Arc<Item>, depth: u8, has_children: bool, expanded: bool) -> Row {
        Row {
            item: item.clone(),
            depth,
            has_children,
            expanded,
            colors: self.lib.colors_of(&item.key),
        }
    }

    fn flat(&self, scope: &Scope, m: &Matcher<'_>, q: &Query) -> Vec<Row> {
        let mut hits: Vec<&Arc<Item>> = self
            .items
            .values()
            .filter(|e| e.item.kind != ItemKind::Folder || matches!(scope, Scope::Place(_)))
            .filter(|e| self.in_scope(&e.item, scope, m))
            .filter(|e| m.matches(&e.item, &e.hay))
            .map(|e| &e.item)
            .collect();
        hits.sort_by(|a, b| compare(a, b, q.sort));
        hits.into_iter()
            .map(|item| self.row(item, 0, false, false))
            .collect()
    }

    /// Whether an item lists as a child of its device folder in this scope, rather than alone.
    fn under_device(&self, item: &Item) -> bool {
        item.kind == ItemKind::Preset
            && item
                .device
                .as_deref()
                .is_some_and(|d| self.items.contains_key(&ItemKey::builtin(d)))
    }

    /// The items directly under `parent` in the tree of `scope`.
    fn children(&self, parent: &Item, scope: &Scope, m: &Matcher<'_>) -> Vec<Arc<Item>> {
        match (scope, parent.kind) {
            (Scope::Label(l), ItemKind::Device) => self
                .by_device
                .get(parent.device.as_deref().unwrap_or_default())
                .into_iter()
                .flatten()
                .filter(|i| m.in_label(i, *l) || parent.labels.contains(*l))
                .cloned()
                .collect(),
            (Scope::Label(_) | Scope::Collection(_), _) | (Scope::Place(_) | Scope::Project, _) => {
                self.by_parent.get(&parent.key).cloned().unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// The top level of the tree of `scope`.
    fn roots_of(&self, scope: &Scope, m: &Matcher<'_>) -> Vec<Arc<Item>> {
        match scope {
            Scope::Place(root) => self
                .by_parent
                .get(&ItemKey::file(root))
                .cloned()
                .unwrap_or_default(),
            Scope::Project => {
                let mut roots: Vec<Arc<Item>> = self
                    .items
                    .values()
                    .filter(|e| e.item.kind == ItemKind::ProjectPart && e.item.parent.is_none())
                    .map(|e| e.item.clone())
                    .collect();
                if let Some(f) = &self.project_folder {
                    roots.extend(
                        self.by_parent
                            .get(&ItemKey::file(f))
                            .cloned()
                            .unwrap_or_default(),
                    );
                }
                roots
            }
            Scope::Label(label) => self
                .items
                .values()
                .filter(|e| e.item.kind != ItemKind::ProjectPart)
                .filter(|e| *label != Label::All || e.item.kind != ItemKind::Folder)
                .filter(|e| m.in_label(&e.item, *label))
                .filter(|e| !self.under_device(&e.item))
                .filter(|e| {
                    e.item
                        .parent
                        .as_ref()
                        .and_then(|p| self.items.get(p))
                        .is_none_or(|p| {
                            p.item.kind == ItemKind::Folder && !m.in_label(&p.item, *label)
                        })
                })
                .map(|e| e.item.clone())
                .collect(),
            _ => self
                .items
                .values()
                .filter(|e| self.in_scope(&e.item, scope, m))
                .map(|e| e.item.clone())
                .collect(),
        }
    }

    fn tree(&self, scope: &Scope, m: &Matcher<'_>, q: &Query) -> Vec<Row> {
        let mut out = Vec::new();
        let mut roots = self.roots_of(scope, m);
        roots.sort_by(|a, b| compare(a, b, q.sort));
        for item in roots {
            self.add_tree(&item, 0, scope, m, q, &mut out);
        }
        out
    }

    fn add_tree(
        &self,
        item: &Arc<Item>,
        depth: u8,
        scope: &Scope,
        m: &Matcher<'_>,
        q: &Query,
        out: &mut Vec<Row>,
    ) {
        let mut kids = if item.kind.is_container() {
            self.children(item, scope, m)
        } else {
            Vec::new()
        };
        let open = !kids.is_empty() && q.expanded.contains(&item.key);
        out.push(self.row(item, depth, !kids.is_empty(), open));
        if open {
            kids.sort_by(|a, b| compare(a, b, q.sort));
            for kid in &kids {
                self.add_tree(kid, depth.saturating_add(1), scope, m, q, out);
            }
        }
    }
}

/// The built-in items, ready to hand to [`BrowserDb::set_catalog`].
pub fn builtin_catalog() -> Vec<Item> {
    catalog::builtin_items()
}
