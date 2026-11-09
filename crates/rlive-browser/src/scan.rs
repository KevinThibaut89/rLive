//! Walks a folder off the UI thread, classifying what it finds.

use crate::catalog::{device_labels, preset_device_tags, DRUM_WORDS};
use crate::item::{Item, ItemKey, ItemKind, Label, LabelSet, Role};
use crate::preset::{Payload, Preset, PRESET_EXTENSION};
use crate::tags::factory_tag;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

/// How many items go to the worker at once.
pub const BATCH: usize = 256;
/// How deep a tree the walker follows.
const MAX_DEPTH: usize = 32;

/// What the classifier needs to know.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub audio_extensions: Vec<String>,
}

/// The extension of a Set, a tuning and a clip, as classified.
const SET_EXT: &str = rlive_project::FILE_EXTENSION;

/// Whether a path is a Set by its extension.
pub fn is_set_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(SET_EXT))
}
pub const CLIP_EXT: &str = "rlclip";

/// Makes an item for a file, or `None` for what the browser doesn't list.
pub fn classify(path: &Path, parent: Option<&Path>, config: &ScanConfig) -> Option<Item> {
    let ext = path.extension()?.to_string_lossy().to_string();
    let lower = ext.to_lowercase();
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let key = ItemKey::file(path);
    let mut item = if config.audio_extensions.contains(&lower) {
        sample_item(key, stem, path)
    } else if lower == PRESET_EXTENSION {
        preset_item(key, stem, path)?
    } else if lower == CLIP_EXT {
        labelled(key, stem, ItemKind::Clip, Label::Clips)
    } else if lower == rlive_project::GROOVE_EXTENSION {
        labelled(key, stem, ItemKind::Groove, Label::Grooves)
    } else if lower == "scl" || lower == "ascl" {
        tuning_item(key, stem, path)
    } else if lower == "stacks" {
        let mut item = Item::new(key, stem, ItemKind::ChordBank);
        item.factory_tags = vec![factory_tag("MIDI Tool"), factory_tag("Stacks")];
        item
    } else if lower == SET_EXT {
        let template = path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("Templates"));
        if template {
            labelled(key, stem, ItemKind::Template, Label::Templates)
        } else {
            Item::new(key, stem, ItemKind::Set)
        }
    } else {
        return None;
    };
    item.path = Some(path.to_path_buf());
    item.ext = ext;
    item.parent = parent.map(ItemKey::file);
    Some(item)
}

fn labelled(key: ItemKey, name: String, kind: ItemKind, label: Label) -> Item {
    let mut item = Item::new(key, name, kind);
    item.labels = LabelSet::of(&[label]);
    item
}

/// A tuning file the user added, which lists under the User tag (manual p. 352).
fn tuning_item(key: ItemKey, stem: String, path: &Path) -> Item {
    let mut item = labelled(key, stem.clone(), ItemKind::Tuning, Label::Tunings);
    item.factory_tags = vec![factory_tag("User")];
    let parsed = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| rlive_project::Tuning::parse(&text, &stem).ok());
    if let Some(tuning) = parsed {
        item.vendor = tuning.summary();
    }
    item
}

fn sample_item(key: ItemKey, stem: String, path: &Path) -> Item {
    let mut item = labelled(key, stem, ItemKind::Sample, Label::Samples);
    let lower = path.to_string_lossy().to_lowercase();
    if DRUM_WORDS.iter().any(|w| lower.contains(w)) {
        item.labels.insert(Label::Drums);
    }
    item
}

fn preset_item(key: ItemKey, stem: String, path: &Path) -> Option<Item> {
    let text = std::fs::read_to_string(path).ok()?;
    let preset = Preset::parse(&text).ok()?;
    let mut item = Item::new(key, stem, ItemKind::Preset);
    fill_preset(&mut item, &preset);
    Some(item)
}

/// Sets what an item for a preset takes from the preset.
pub fn fill_preset(item: &mut Item, preset: &Preset) {
    item.device = preset.device_key();
    item.role = preset.role();
    item.labels = preset_labels(preset);
    item.factory_tags = preset_device_tags(preset)
        .into_iter()
        .chain(preset.tags.iter().filter_map(|t| known_factory_tag(t)))
        .collect();
    item.factory_tags.sort_unstable();
    item.factory_tags.dedup();
}

fn known_factory_tag(name: &str) -> Option<crate::tags::TagId> {
    let db = crate::tags::TagDb::default();
    db.all_tags()
        .into_iter()
        .find(|t| t.factory && t.name.eq_ignore_ascii_case(name))
        .map(|_| factory_tag(name))
}

/// The Library labels a preset goes under (manual pp. 83-84).
pub fn preset_labels(preset: &Preset) -> LabelSet {
    let mut set = LabelSet::default();
    match &preset.payload {
        Payload::Device(d) => match &d.kind {
            rlive_project::DeviceKind::Rack { kind, .. } => match kind {
                rlive_project::RackKind::Instrument => {
                    set.insert(Label::Sounds);
                    set.insert(Label::Instruments);
                }
                rlive_project::RackKind::Drum => {
                    set.insert(Label::Drums);
                    set.insert(Label::Instruments);
                }
                rlive_project::RackKind::Audio => set.insert(Label::AudioEffects),
                rlive_project::RackKind::Midi => set.insert(Label::MidiEffects),
            },
            rlive_project::DeviceKind::Builtin { type_id, .. } => {
                set = device_labels(type_id);
                if preset.role() == Some(Role::Instrument) && !set.contains(Label::Drums) {
                    set.insert(Label::Sounds);
                }
            }
            rlive_project::DeviceKind::Plugin { .. } => set.insert(Label::Plugins),
        },
        Payload::Chain(_) => {}
        Payload::Track(_) | Payload::PluginConfig(_) => {}
    }
    set
}

/// What a scan handed over.
pub enum Found {
    Batch(Vec<Item>),
    /// The walk ended. `exists` says if the folder was there at all.
    Done {
        exists: bool,
    },
}

/// Walks `root` and everything in it, handing the items to `sink` in batches, until done or
/// `cancel` is set. Never follows symlinks, so a loop can't trap it.
pub fn walk(
    root: &Path,
    config: &ScanConfig,
    cancel: &AtomicBool,
    mut sink: impl FnMut(Found),
) -> usize {
    let mut total = 0usize;
    let Ok(meta) = std::fs::metadata(root) else {
        sink(Found::Done { exists: false });
        return 0;
    };
    if !meta.is_dir() {
        sink(Found::Done { exists: false });
        return 0;
    }
    let mut batch = Vec::with_capacity(BATCH);
    let mut stack: Vec<(PathBuf, Option<PathBuf>, usize)> = vec![(root.to_path_buf(), None, 0)];
    while let Some((dir, parent, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            sink(Found::Done { exists: true });
            return total;
        }
        batch.push(folder_item(&dir, parent.as_deref()));
        total += 1;
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if cancel.load(Ordering::Relaxed) {
                sink(Found::Done { exists: true });
                return total;
            }
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, Some(dir.clone()), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if let Some(mut item) = classify(&path, Some(&dir), config) {
                if let Ok(meta) = entry.metadata() {
                    item.size = meta.len();
                    item.modified = meta
                        .modified()
                        .ok()
                        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs());
                }
                batch.push(item);
                total += 1;
                if batch.len() >= BATCH {
                    sink(Found::Batch(std::mem::replace(
                        &mut batch,
                        Vec::with_capacity(BATCH),
                    )));
                }
            }
        }
    }
    if !batch.is_empty() {
        sink(Found::Batch(batch));
    }
    sink(Found::Done { exists: true });
    total
}

fn folder_item(path: &Path, parent: Option<&Path>) -> Item {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    );
    let mut item = Item::new(ItemKey::file(path), name, ItemKind::Folder);
    item.path = Some(path.to_path_buf());
    item.parent = parent.map(ItemKey::file);
    item
}
