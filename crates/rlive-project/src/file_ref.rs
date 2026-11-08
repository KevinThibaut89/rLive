//! References to audio files.
//!
//! Every path to audio in a project is a [`FileRef`], whichever device or clip holds it, so that
//! Collect All and missing-file search can find them without knowing any device's format.

use crate::defaults::is_default;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileRef {
    /// Where the file was last seen.
    pub path: PathBuf,
    /// The path relative to the project folder, when the file is inside it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub relative: Option<PathBuf>,
    /// File size in bytes, when known. Tells candidates apart when searching for a missing file.
    #[serde(default, skip_serializing_if = "is_default")]
    pub size: Option<u64>,
    /// Content hash, when known, including the name of its algorithm (`"blake3:ab12..."`).
    #[serde(default, skip_serializing_if = "is_default")]
    pub hash: Option<String>,
}

impl FileRef {
    /// A reference known only by its path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            relative: None,
            size: None,
            hash: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
