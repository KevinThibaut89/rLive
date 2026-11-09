//! rLive's own factory presets, written here rather than shipped as files. Every one of them was
//! made for rLive: names and settings come from the table below and nowhere else.

use crate::preset::{Payload, Preset, PRESET_FORMAT};
use rlive_project::Device;

/// One factory preset: device, name, tags, then `(parameter name, value)`.
type Row = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [(&'static str, f64)],
);

include!("factory_table.rs");

/// The key a factory preset is found by.
pub fn key_of(preset: &Preset) -> String {
    format!(
        "{}/{}",
        preset.device_key().unwrap_or_default(),
        preset.name
    )
}

fn build(row: &Row) -> Option<Preset> {
    let (type_id, name, tags, values) = *row;
    let info = rlive_devices::find(type_id)?;
    let desc = (info.desc)();
    let mut device = Device::builtin(type_id, name);
    for (param, value) in values {
        let id = desc.params.iter().find(|p| p.name == *param)?.id;
        device.params.insert(id, *value);
    }
    Some(Preset {
        format: PRESET_FORMAT,
        name: name.to_string(),
        info: String::new(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        payload: Payload::Device(Box::new(device)),
    })
}

/// Every factory preset.
pub fn presets() -> Vec<Preset> {
    TABLE.iter().filter_map(build).collect()
}

/// A factory preset by its key.
pub fn get(key: &str) -> Option<Preset> {
    TABLE
        .iter()
        .filter(|r| key.starts_with(r.0) && key[r.0.len()..].strip_prefix('/') == Some(r.1))
        .find_map(build)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_resolves_to_real_parameters_in_range() {
        assert!(!TABLE.is_empty());
        for row in TABLE {
            let info = rlive_devices::find(row.0).unwrap_or_else(|| panic!("no device {}", row.0));
            let desc = (info.desc)();
            for (name, value) in row.3 {
                let p = desc
                    .params
                    .iter()
                    .find(|p| p.name == *name)
                    .unwrap_or_else(|| panic!("{}: no parameter {name}", row.0));
                assert!(
                    *value >= p.min && *value <= p.max,
                    "{} {name}: {value} out of {}..{}",
                    row.1,
                    p.min,
                    p.max
                );
            }
        }
        assert_eq!(presets().len(), TABLE.len());
    }

    #[test]
    fn presets_are_found_by_their_own_key() {
        for preset in presets() {
            let found = get(&key_of(&preset)).expect("found");
            assert_eq!(found.name, preset.name);
            assert_eq!(found.device_key(), preset.device_key());
        }
    }
}
