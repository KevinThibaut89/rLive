//! Upgrading files saved by older versions of rLive.
//!
//! `ron::Value` loses struct and enum tags, so an upgrade can't work on the parsed text. Instead:
//!
//! 1. [`probe_version`] reads only `version`, ignoring the rest of the file.
//! 2. [`to_current`] parses the file with that version's frozen types and runs its upgrade.
//!
//! `vN.rs` holds frozen copies of only the types that changed in version `N`, plus
//! `upgrade(old) -> next`. Frozen types must never be edited: they are the file format.
//!
//! When the format changes again: freeze the affected live types in a new `v2.rs` with
//! `upgrade(v2::Project) -> Project`; keep `v1::upgrade`, retargeted at `v2::Project`; add a
//! match arm; and bump [`FORMAT_VERSION`](crate::FORMAT_VERSION). An added field needs none of
//! this, only `serde(default)`; a new version is needed only for a rename or a change of meaning.
//!
//! Every file in `tests/fixtures/` is loaded and round-tripped at every version. Add fixtures for
//! a new version from the code of the version it was saved by, never by hand.

pub mod v1;

use crate::{Project, ProjectError, FORMAT_VERSION};
use serde::Deserialize;

/// The oldest version [`to_current`] can read.
pub const OLDEST_VERSION: u32 = 1;

/// Only the field every project has had from the start. Ignores everything else in the file.
#[derive(Deserialize)]
#[serde(rename = "Project")]
struct Probe {
    version: u32,
}

/// The format version of a project file.
pub fn probe_version(s: &str) -> Result<u32, ProjectError> {
    Ok(ron::from_str::<Probe>(s)?.version)
}

/// Parses a project in format `version` and upgrades it to the current model.
pub fn to_current(s: &str, version: u32) -> Result<Project, ProjectError> {
    match version {
        1 => Ok(v1::upgrade(ron::from_str(s)?)),
        FORMAT_VERSION => Ok(ron::from_str(s)?),
        v if v > FORMAT_VERSION => Err(ProjectError::TooNew(v)),
        v => Err(ProjectError::UnknownVersion(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probing_ignores_everything_but_the_version() {
        let file = r#"(
            tempo: 90.5,
            version: 7,
            nested: (a: Some([1, 2]), b: Builtin(type_id: "x", sample: None)),
            ids: (12),
        )"#;
        assert_eq!(probe_version(file).unwrap(), 7);
    }

    #[test]
    fn a_future_version_is_refused() {
        let file = format!("(version: {}, x: 1)", FORMAT_VERSION + 1);
        assert!(matches!(
            Project::from_ron(&file),
            Err(ProjectError::TooNew(v)) if v == FORMAT_VERSION + 1
        ));
    }

    #[test]
    fn version_zero_is_refused() {
        assert!(matches!(
            Project::from_ron("(version: 0)"),
            Err(ProjectError::UnknownVersion(0))
        ));
    }

    #[test]
    fn a_missing_version_is_a_parse_error() {
        assert!(matches!(
            Project::from_ron("(tempo: 1.0)"),
            Err(ProjectError::Parse(_))
        ));
    }
}
