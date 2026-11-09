//! Files on disk, for the File Manager: copying without loss, finding files, working out what a
//! folder's Sets, clips and presets use, and packing a Project.

use crate::backup;
use crate::preset::{self, Preset};
use crate::scan::CLIP_EXT;
use rlive_project::project_folder::{self, SAMPLES};
use rlive_project::{FileRef, Project, FILE_EXTENSION, FORMAT_VERSION};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

// ------------------------------------------------------------- copying

fn hash_of(path: &Path) -> io::Result<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.finalize());
        }
        hasher.update(&buf[..n]);
    }
}

/// Whether two files hold the same bytes.
pub fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    if std::fs::metadata(a)?.len() != std::fs::metadata(b)?.len() {
        return Ok(false);
    }
    Ok(hash_of(a)? == hash_of(b)?)
}

/// Copies `from` to `to`, which must not exist, so that `to` only ever appears whole.
pub fn copy_new(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if to.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    let mut name = to.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    let tmp = to.with_file_name(name);
    let result = std::fs::copy(from, &tmp).and_then(|_| std::fs::File::open(&tmp)?.sync_all());
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, to)
}

/// Where a copy of `from` belongs in `dir`: a new name when another file has its name, and the
/// file that's already there when it holds the same bytes.
pub fn place_in(dir: &Path, from: &Path) -> io::Result<(PathBuf, bool)> {
    let name = from
        .file_name()
        .ok_or_else(|| io::Error::other("a file with no name"))?;
    let first = dir.join(name);
    if !first.exists() {
        return Ok((first, false));
    }
    if same_contents(from, &first)? {
        return Ok((first, true));
    }
    let stem = from
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = from
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for n in 2u32.. {
        let candidate = dir.join(format!("{stem} {n}{ext}"));
        if !candidate.exists() {
            return Ok((candidate, false));
        }
        if same_contents(from, &candidate)? {
            return Ok((candidate, true));
        }
    }
    unreachable!("names run out long after disks do")
}

/// Copies a file into a folder, reusing one with the same bytes. Returns where it is now.
pub fn collect_into(dir: &Path, from: &Path) -> io::Result<PathBuf> {
    let (to, reused) = place_in(dir, from)?;
    if !reused {
        copy_new(from, &to)?;
    }
    Ok(to)
}

// ------------------------------------------------------------- finding files

/// One file an index knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub path: PathBuf,
    pub size: u64,
}

/// File names found under folders, remembered from one search to the next.
#[derive(Debug, Default)]
pub struct FileIndex {
    folders: HashMap<PathBuf, HashMap<String, Vec<IndexedFile>>>,
}

fn file_key(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().to_lowercase())
}

fn walk(dir: &Path, depth: usize, out: &mut HashMap<String, Vec<IndexedFile>>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if kind.is_dir() {
            walk(&path, depth - 1, out);
        } else if kind.is_file() {
            if let Some(key) = file_key(&path) {
                let size = entry.metadata().map_or(0, |m| m.len());
                out.entry(key).or_default().push(IndexedFile { path, size });
            }
        }
    }
}

const MAX_DEPTH: usize = 32;

impl FileIndex {
    /// Learns a folder's files, unless it has been looked through already.
    pub fn add_folder(&mut self, folder: &Path) {
        if !self.folders.contains_key(folder) {
            let mut found = HashMap::new();
            walk(folder, MAX_DEPTH, &mut found);
            self.folders.insert(folder.to_path_buf(), found);
        }
    }

    /// Forgets what it knows, for Fully Rescan Folders.
    pub fn clear(&mut self) {
        self.folders.clear();
    }

    /// Everything known with the name of `path`.
    fn named(&self, path: &Path) -> Vec<&IndexedFile> {
        let Some(key) = file_key(path) else {
            return Vec::new();
        };
        self.folders
            .values()
            .filter_map(|f| f.get(&key))
            .flatten()
            .collect()
    }

    /// The files that could be a missing file. Everything with the right name, narrowed to those
    /// of the right size when the reference knows its size and any of them has it.
    pub fn candidates(&self, file: &FileRef) -> Vec<PathBuf> {
        let mut named = self.named(&file.path);
        named.sort_by(|a, b| a.path.cmp(&b.path));
        named.dedup_by(|a, b| a.path == b.path);
        if let Some(size) = file.size {
            if named.iter().any(|f| f.size == size) {
                named.retain(|f| f.size == size);
            }
        }
        named.into_iter().map(|f| f.path.clone()).collect()
    }
}

// ------------------------------------------------------------- what a folder uses

/// A file that refers to other files.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub path: PathBuf,
    pub kind: DocumentKind,
    pub refs: Vec<FileRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Set,
    Clip,
    Preset,
}

/// The audio files a Set depends on.
fn project_refs(p: &Project) -> Vec<FileRef> {
    let mut out = Vec::new();
    p.for_each_file_ref(&mut |f| out.push(f.clone()));
    out
}

/// What one file refers to, if it's a kind of file that refers to anything.
pub fn read_document(path: &Path) -> Option<Document> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if ext == FILE_EXTENSION {
        let text = std::fs::read_to_string(path).ok()?;
        let p = Project::from_ron(&text).ok()?;
        return Some(Document {
            path: path.to_path_buf(),
            kind: DocumentKind::Set,
            refs: project_refs(&p),
        });
    }
    if ext == CLIP_EXT {
        let mut file = preset::load_clip_file(path).ok()?;
        let mut refs = Vec::new();
        file.for_each_file_ref_mut(&mut |f| refs.push(f.clone()));
        return Some(Document {
            path: path.to_path_buf(),
            kind: DocumentKind::Clip,
            refs,
        });
    }
    if ext == preset::PRESET_EXTENSION {
        let mut preset = Preset::load(path).ok()?;
        let refs = preset
            .file_refs_mut()
            .iter()
            .map(|f| (*f).clone())
            .collect();
        return Some(Document {
            path: path.to_path_buf(),
            kind: DocumentKind::Preset,
            refs,
        });
    }
    None
}

/// Every Set, clip and preset under a folder, leaving out the Backup folders.
pub fn scan_documents(root: &Path) -> Vec<Document> {
    fn visit(dir: &Path, out: &mut Vec<Document>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if name != backup::BACKUP_FOLDER {
                    visit(&path, out);
                }
            } else if let Some(doc) = read_document(&path) {
                out.push(doc);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Rewrites a document's references, keeping a copy of it in its Backup folder first. A Set in an
/// older format is left alone: it has to be saved under a new name.
pub fn rewrite_document(doc: &Document, change: &mut dyn FnMut(&mut FileRef)) -> io::Result<bool> {
    let text = std::fs::read_to_string(&doc.path)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match doc.kind {
        DocumentKind::Set => {
            if Project::format_of(&text).ok() != Some(FORMAT_VERSION) {
                return Ok(false);
            }
            let mut p = Project::from_ron(&text).map_err(io::Error::other)?;
            backup::back_up(&doc.path, now, backup::trash_dir().as_deref())?;
            p.for_each_file_ref_mut(&mut |f| change(f));
            p.save(&doc.path).map_err(io::Error::other)?;
        }
        DocumentKind::Clip => {
            let mut file = preset::load_clip_file(&doc.path).map_err(io::Error::other)?;
            backup::back_up(&doc.path, now, backup::trash_dir().as_deref())?;
            file.for_each_file_ref_mut(&mut |f| change(f));
            preset::save_clip_file(&doc.path, &file)?;
        }
        DocumentKind::Preset => {
            let mut preset = Preset::load(&doc.path).map_err(io::Error::other)?;
            backup::back_up(&doc.path, now, backup::trash_dir().as_deref())?;
            for f in preset.file_refs_mut() {
                change(f);
            }
            preset.save(&doc.path).map_err(io::Error::other)?;
        }
    }
    Ok(true)
}

/// The files a Project's Sets, clips and presets don't use, by the folder they're in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedGroup {
    /// "Recorded", "Imported", "Bounce", "Other" and so on.
    pub category: String,
    pub files: Vec<PathBuf>,
}

/// What's under a Project's Samples folder that nothing refers to.
pub fn unused_files(root: &Path, docs: &[Document]) -> Vec<UnusedGroup> {
    let used: HashSet<&Path> = docs
        .iter()
        .flat_map(|d| d.refs.iter().map(|f| f.path.as_path()))
        .collect();
    let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
    fn visit(
        root: &Path,
        dir: &Path,
        used: &HashSet<&Path>,
        groups: &mut HashMap<String, Vec<PathBuf>>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.is_dir() {
                visit(root, &path, used, groups);
            } else if !is_sidecar(&path) && !used.contains(path.as_path()) {
                groups
                    .entry(category_of(root, &path))
                    .or_default()
                    .push(path);
            }
        }
    }
    let samples = root.join(SAMPLES);
    visit(&samples, &samples, &used, &mut groups);
    let mut out: Vec<UnusedGroup> = groups
        .into_iter()
        .map(|(category, mut files)| {
            files.sort();
            UnusedGroup { category, files }
        })
        .collect();
    out.sort_by(|a, b| a.category.cmp(&b.category));
    out
}

fn is_sidecar(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rlad"))
}

fn category_of(samples: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(samples).unwrap_or(path);
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.pop();
    match parts.as_slice() {
        [] => "Other".into(),
        [one] => one.clone(),
        [first, second, ..] if first == "Processed" => second.clone(),
        [first, ..] => first.clone(),
    }
}

// ------------------------------------------------------------- packs

pub const PACK_EXTENSION: &str = "rlpack";
const PACK_MAGIC: &[u8; 8] = b"RLPACK01";

fn pack_files(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if entry.file_name() != backup::BACKUP_FOLDER {
                pack_files(&path, out)?;
            }
        } else if entry.file_type()?.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Stores a whole folder losslessly in one file. A partial pack never appears under `pack`.
pub fn write_pack(root: &Path, pack: &Path) -> io::Result<u64> {
    let mut files = Vec::new();
    pack_files(root, &mut files)?;
    files.sort();
    let mut name = pack.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    let tmp = pack.with_file_name(name);
    let result = pack_into(root, &files, &tmp);
    match result {
        Ok(()) => {
            std::fs::rename(&tmp, pack)?;
            Ok(files.len() as u64)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn pack_into(root: &Path, files: &[PathBuf], tmp: &Path) -> io::Result<()> {
    let mut out = std::fs::File::create(tmp)?;
    out.write_all(PACK_MAGIC)?;
    out.write_all(&(files.len() as u64).to_le_bytes())?;
    for path in files {
        let rel = path.strip_prefix(root).map_err(io::Error::other)?;
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        out.write_all(&(name.len() as u32).to_le_bytes())?;
        out.write_all(name.as_bytes())?;
        let size = std::fs::metadata(path)?.len();
        out.write_all(&size.to_le_bytes())?;
        out.write_all(hash_of(path)?.as_bytes())?;
        let at = out.stream_position()?;
        out.write_all(&0u64.to_le_bytes())?;
        let start = out.stream_position()?;
        let mut encoder =
            flate2::write::DeflateEncoder::new(&mut out, flate2::Compression::default());
        io::copy(&mut std::fs::File::open(path)?, &mut encoder)?;
        encoder.finish()?;
        let end = out.stream_position()?;
        out.seek(SeekFrom::Start(at))?;
        out.write_all(&(end - start).to_le_bytes())?;
        out.seek(SeekFrom::Start(end))?;
    }
    out.sync_all()
}

fn read_exact_vec(r: &mut impl Read, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// A path inside a pack, refused if it would land outside the folder it's installed into.
fn safe_relative(name: &str) -> io::Result<PathBuf> {
    let path = Path::new(name);
    let fine = !name.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)));
    if !fine {
        return Err(io::Error::other(format!("a pack entry names {name}")));
    }
    Ok(path.to_path_buf())
}

/// Unpacks a Project into a new folder under `parent` and returns it. Nothing appears there
/// unless every file in the pack came out intact.
pub fn install_pack(pack: &Path, parent: &Path) -> io::Result<PathBuf> {
    let stem = pack
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Pack".into());
    let target = project_folder::free_folder(parent, &stem);
    let tmp = project_folder::free_folder(parent, &format!(".{stem}.installing"));
    std::fs::create_dir_all(&tmp)?;
    let result = unpack(pack, &tmp);
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

fn unpack(pack: &Path, into: &Path) -> io::Result<()> {
    let mut input = io::BufReader::new(std::fs::File::open(pack)?);
    if read_exact_vec(&mut input, 8)? != PACK_MAGIC {
        return Err(io::Error::other("not an rLive Pack"));
    }
    let count = read_u64(&mut input)?;
    for _ in 0..count {
        let mut len = [0u8; 4];
        input.read_exact(&mut len)?;
        let name = String::from_utf8(read_exact_vec(
            &mut input,
            u32::from_le_bytes(len) as usize,
        )?)
        .map_err(io::Error::other)?;
        let rel = safe_relative(&name)?;
        let size = read_u64(&mut input)?;
        let hash: [u8; 32] = read_exact_vec(&mut input, 32)?
            .try_into()
            .map_err(|_| io::Error::other("a short hash"))?;
        let compressed = read_u64(&mut input)?;
        let to = into.join(rel);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = std::fs::File::create(&to)?;
        let mut decoder = flate2::read::DeflateDecoder::new((&mut input).take(compressed));
        let written = io::copy(&mut decoder, &mut out)?;
        // The decoder can stop short of its input.
        io::copy(&mut decoder.into_inner(), &mut io::sink())?;
        if written != size || hash_of(&to)?.as_bytes() != &hash {
            return Err(io::Error::other(format!("{name} is damaged in the pack")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rlive-files-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_copy_never_replaces_what_is_there() {
        let d = temp("copy");
        std::fs::write(d.join("a"), b"one").unwrap();
        std::fs::write(d.join("b"), b"two").unwrap();
        assert!(copy_new(&d.join("a"), &d.join("b")).is_err());
        assert_eq!(std::fs::read(d.join("b")).unwrap(), b"two");
        copy_new(&d.join("a"), &d.join("c/d")).unwrap();
        assert_eq!(std::fs::read(d.join("c/d")).unwrap(), b"one");
    }

    #[test]
    fn same_bytes_are_reused_and_a_clash_gets_a_new_name() {
        let d = temp("place");
        let dir = d.join("Imported");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(d.join("x.wav"), b"audio").unwrap();
        assert_eq!(
            collect_into(&dir, &d.join("x.wav")).unwrap(),
            dir.join("x.wav")
        );
        assert_eq!(
            collect_into(&dir, &d.join("x.wav")).unwrap(),
            dir.join("x.wav"),
            "the same file, not a second copy"
        );
        std::fs::write(d.join("x2.wav"), b"other").unwrap();
        std::fs::rename(d.join("x2.wav"), d.join("sub.wav")).unwrap();
        std::fs::create_dir_all(d.join("o")).unwrap();
        std::fs::rename(d.join("sub.wav"), d.join("o/x.wav")).unwrap();
        assert_eq!(
            collect_into(&dir, &d.join("o/x.wav")).unwrap(),
            dir.join("x 2.wav")
        );
        assert_eq!(std::fs::read(dir.join("x.wav")).unwrap(), b"audio");
    }

    #[test]
    fn candidates_are_found_by_name_and_narrowed_by_size() {
        let d = temp("search");
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::create_dir_all(d.join("b/c")).unwrap();
        std::fs::write(d.join("a/kick.wav"), b"12345").unwrap();
        std::fs::write(d.join("b/c/Kick.WAV"), b"123").unwrap();
        let mut index = FileIndex::default();
        index.add_folder(&d);
        let mut file = FileRef::new("/gone/kick.wav");
        assert_eq!(index.candidates(&file).len(), 2, "by name alone");
        file.size = Some(5);
        assert_eq!(index.candidates(&file), [d.join("a/kick.wav")]);
        file.size = Some(9);
        assert_eq!(
            index.candidates(&file).len(),
            2,
            "no size match: keep them all"
        );
    }

    #[test]
    fn an_index_is_only_rebuilt_when_asked() {
        let d = temp("rescan");
        let mut index = FileIndex::default();
        index.add_folder(&d);
        std::fs::write(d.join("late.wav"), b"x").unwrap();
        let file = FileRef::new("/gone/late.wav");
        index.add_folder(&d);
        assert!(index.candidates(&file).is_empty(), "from memory");
        index.clear();
        index.add_folder(&d);
        assert_eq!(index.candidates(&file).len(), 1);
    }

    #[test]
    fn unused_files_are_grouped_by_folder_and_used_ones_left_out() {
        let d = temp("unused");
        for f in [
            "Samples/Recorded/a.wav",
            "Samples/Recorded/used.wav",
            "Samples/Processed/Freeze/b.wav",
            "Samples/loose.wav",
            "Samples/Recorded/a.wav.rlad",
        ] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let docs = [Document {
            path: d.join("Set.rlive"),
            kind: DocumentKind::Set,
            refs: vec![FileRef::new(d.join("Samples/Recorded/used.wav"))],
        }];
        let groups = unused_files(&d, &docs);
        let names: Vec<_> = groups
            .iter()
            .map(|g| (g.category.as_str(), g.files.len()))
            .collect();
        assert_eq!(names, [("Freeze", 1), ("Other", 1), ("Recorded", 1)]);
    }

    #[test]
    fn a_pack_restores_a_project_exactly() {
        let d = temp("pack");
        let root = d.join("Song Project");
        for (f, body) in [
            ("Song.rlive", b"(version: 2)".to_vec()),
            ("Samples/Recorded/a.wav", vec![7u8; 100_000]),
            (".rlive-project", b"(format: 1)".to_vec()),
            ("Backup/old.rlive", b"skip me".to_vec()),
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let pack = d.join("Song.rlpack");
        write_pack(&root, &pack).unwrap();
        assert!(
            std::fs::metadata(&pack).unwrap().len() < 10_000,
            "compressed"
        );
        let installed = install_pack(&pack, &d.join("out")).unwrap();
        assert_eq!(installed, d.join("out/Song"));
        assert_eq!(
            std::fs::read(installed.join("Samples/Recorded/a.wav")).unwrap(),
            vec![7u8; 100_000]
        );
        assert!(installed.join(".rlive-project").exists());
        assert!(!installed.join("Backup").exists());
        let again = install_pack(&pack, &d.join("out")).unwrap();
        assert_eq!(again, d.join("out/Song 2"), "never over what's there");
    }

    #[test]
    fn a_pack_with_a_path_out_of_its_folder_is_refused() {
        assert!(safe_relative("../evil").is_err());
        assert!(safe_relative("/etc/passwd").is_err());
        assert!(safe_relative("a/../b").is_err());
        assert!(safe_relative("a/b").is_ok());
    }

    #[test]
    fn a_damaged_pack_installs_nothing() {
        let d = temp("damaged");
        let root = d.join("P");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.wav"), b"abcdefgh").unwrap();
        let pack = d.join("P.rlpack");
        write_pack(&root, &pack).unwrap();
        let mut bytes = std::fs::read(&pack).unwrap();
        let n = bytes.len();
        bytes[n - 2] ^= 0xff;
        std::fs::write(&pack, bytes).unwrap();
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(install_pack(&pack, &out).is_err());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    }
}
