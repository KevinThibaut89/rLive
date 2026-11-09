//! Project folders: a folder holding Sets and everything that belongs to them.
//!
//! A folder is a Project when it holds a [`MARKER`] file. A Set can be any depth inside one. A Set
//! in no Project at all, as every Set saved before Projects existed is, treats its own folder as
//! its Project, and nothing is moved or created until it is saved somewhere new.

use std::path::{Path, PathBuf};

/// The file that makes a folder a Project.
pub const MARKER: &str = ".rlive-project";
const MARKER_TEXT: &str = "(format: 1)\n";
/// Where a Project keeps its audio.
pub const SAMPLES: &str = "Samples";
/// Where Collect All puts files from outside the Project.
pub const IMPORTED: &str = "Imported";
/// Where presets saved into a Project go.
pub const PRESETS: &str = "Presets";

/// The Project a path is inside, if any.
pub fn find_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|dir| dir.join(MARKER).is_file())
        .map(Path::to_path_buf)
}

/// The Project of a Set, or its own folder when it has none.
pub fn root_or_parent(set: &Path) -> Option<PathBuf> {
    find_root(set).or_else(|| set.parent().map(Path::to_path_buf))
}

/// Makes a folder a Project, creating it if need be. Whatever is in it stays.
pub fn mark(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let marker = root.join(MARKER);
    if !marker.exists() {
        std::fs::write(marker, MARKER_TEXT)?;
    }
    Ok(())
}

/// Where the imported samples of a Project go.
pub fn imported_folder(root: &Path) -> PathBuf {
    root.join(SAMPLES).join(IMPORTED)
}

/// Where a Set saved under a name the user picked ends up, and the new Project it needs if any:
/// inside an existing Project it stays where it was asked to go, and anywhere else it gets a
/// `<name> Project` folder of its own. A folder already there is never reused unless it's a
/// Project the path is in, so saving can't mix a Set in with someone's unrelated files.
pub fn save_target(chosen: &Path) -> (PathBuf, Option<PathBuf>) {
    if find_root(chosen).is_some() {
        return (chosen.to_path_buf(), None);
    }
    let stem = chosen
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into());
    let parent = chosen.parent().unwrap_or(Path::new(""));
    let file = chosen
        .file_name()
        .map_or_else(|| PathBuf::from(&stem), PathBuf::from);
    let root = free_folder(parent, &format!("{stem} Project"));
    (root.join(file), Some(root))
}

/// A folder under `parent` with this name, or a numbered one if that's taken.
pub fn free_folder(parent: &Path, name: &str) -> PathBuf {
    let first = parent.join(name);
    if !first.exists() {
        return first;
    }
    (2u32..)
        .map(|n| parent.join(format!("{name} {n}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// The Project's name as the title bar shows it.
pub fn display_name(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rlive-pf-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_set_saved_outside_a_project_gets_a_project_of_its_own() {
        let dir = temp("new");
        let (set, root) = save_target(&dir.join("Tango.rlive"));
        assert_eq!(root.as_deref(), Some(dir.join("Tango Project").as_path()));
        assert_eq!(set, dir.join("Tango Project/Tango.rlive"));
    }

    #[test]
    fn a_set_saved_inside_a_project_stays_there() {
        let dir = temp("inside");
        let root = dir.join("Tango Project");
        mark(&root).unwrap();
        let chosen = root.join("Tango with Piano.rlive");
        assert_eq!(save_target(&chosen), (chosen.clone(), None));
        let deep = root.join("Versions/Old.rlive");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        assert_eq!(find_root(&deep), Some(root));
    }

    #[test]
    fn a_folder_that_is_already_there_is_not_taken_over() {
        let dir = temp("taken");
        std::fs::create_dir_all(dir.join("Tango Project")).unwrap();
        let (set, root) = save_target(&dir.join("Tango.rlive"));
        assert_eq!(root, Some(dir.join("Tango Project 2")));
        assert_eq!(set, dir.join("Tango Project 2/Tango.rlive"));
    }

    #[test]
    fn a_set_with_no_project_uses_its_own_folder() {
        let dir = temp("orphan");
        let set = dir.join("Old.rlive");
        assert_eq!(find_root(&set), None);
        assert_eq!(root_or_parent(&set), Some(dir));
    }
}
