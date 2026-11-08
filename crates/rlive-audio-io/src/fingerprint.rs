//! Identifying a file by its contents.

use std::fs::File;
use std::io;
use std::path::Path;

/// What a file is, independently of where it is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    pub size: u64,
    /// The content hash with the name of its algorithm, as in `FileRef`: `"blake3:ab12..."`.
    pub hash: String,
}

const ALGORITHM: &str = "blake3:";

impl Fingerprint {
    /// The hash on its own, usable in a file name.
    pub fn digest(&self) -> &str {
        self.hash.strip_prefix(ALGORITHM).unwrap_or(&self.hash)
    }
}

/// Reads the whole file, so this belongs on a loading thread.
pub fn fingerprint(path: &Path) -> io::Result<Fingerprint> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(File::open(path)?)?;
    Ok(Fingerprint {
        size: std::fs::metadata(path)?.len(),
        hash: format!("{ALGORITHM}{}", hasher.finalize().to_hex()),
    })
}
