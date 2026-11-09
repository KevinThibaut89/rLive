//! The Backup folder of a Project: the last ten saves of a Set (manual p. 115).

use std::path::{Path, PathBuf};

pub const BACKUP_FOLDER: &str = "Backup";
pub const MAX_BACKUPS: usize = 10;

/// The Backup folder of the Set at `set`.
pub fn folder_of(set: &Path) -> Option<PathBuf> {
    Some(set.parent()?.join(BACKUP_FOLDER))
}

/// Where the OS keeps deleted files for the current user, when it has one.
pub fn trash_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if cfg!(target_os = "macos") {
        Some(home.join(".Trash"))
    } else if cfg!(unix) {
        Some(home.join(".local/share/Trash/files"))
    } else {
        None
    }
}

/// The timestamp in a backup's name for a moment, in the local-time-free UTC form
/// `YYYY-MM-DD HHMMSS`, which sorts in time order.
pub fn stamp(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let rest = unix_seconds % 86_400;
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}{:02}{:02}",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// Copies the Set about to be overwritten into its Backup folder under a timestamp, then keeps
/// only the newest [`MAX_BACKUPS`], moving the rest to `trash` (or removing them, without one).
pub fn back_up(set: &Path, now: u64, trash: Option<&Path>) -> std::io::Result<PathBuf> {
    let dir = folder_of(set).ok_or_else(|| std::io::Error::other("no folder"))?;
    std::fs::create_dir_all(&dir)?;
    let stem = set
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let ext = set
        .extension()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let mut name = format!("{stem} [{}].{ext}", stamp(now));
    let mut n = 1;
    while dir.join(&name).exists() {
        n += 1;
        name = format!("{stem} [{}] {n}.{ext}", stamp(now));
    }
    let target = dir.join(name);
    std::fs::copy(set, &target)?;
    prune(&dir, &stem, trash)?;
    Ok(target)
}

/// The backups of one Set, oldest first.
pub fn list(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let prefix = format!("{stem} [");
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
        })
        .collect();
    found.sort();
    found
}

fn prune(dir: &Path, stem: &str, trash: Option<&Path>) -> std::io::Result<()> {
    let found = list(dir, stem);
    let excess = found.len().saturating_sub(MAX_BACKUPS);
    for old in &found[..excess] {
        discard(old, trash)?;
    }
    Ok(())
}

/// Moves a file to the OS's trash, or deletes it when there isn't one.
pub fn to_trash(path: &Path) -> std::io::Result<()> {
    discard(path, trash_dir().as_deref())
}

fn discard(path: &Path, trash: Option<&Path>) -> std::io::Result<()> {
    if let (Some(trash), Some(name)) = (trash, path.file_name()) {
        std::fs::create_dir_all(trash)?;
        let mut target = trash.join(name);
        let mut n = 1;
        while target.exists() {
            n += 1;
            target = trash.join(format!("{n} {}", name.to_string_lossy()));
        }
        if std::fs::rename(path, &target).is_ok() {
            return Ok(());
        }
    }
    std::fs::remove_file(path)
}
