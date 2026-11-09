//! Everything the browser remembers: tags, collections, custom labels, folders and view options.

use crate::item::{ItemKey, Label};
use crate::query::{Column, Scope, Sort};
use crate::tags::{factory_group_named, GroupId, TagDb, TagError, TagId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const LIBRARY_VERSION: u32 = 1;
pub const COLLECTION_COUNT: usize = 7;

/// The icons a label can wear, by name.
pub const LABEL_ICONS: &[&str] = &[
    "Folder", "Star", "Heart", "Note", "Wave", "Drum", "Knob", "Tag",
];

/// A Collections label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub name: String,
    pub color: [u8; 3],
    pub visible: bool,
}

fn default_collections() -> Vec<Collection> {
    [
        ("Favorites", [0xe6, 0x4b, 0x4b], true),
        ("Orange", [0xef, 0x8a, 0x3c], true),
        ("Yellow", [0xe8, 0xcf, 0x3f], false),
        ("Green", [0x5b, 0xc2, 0x5b], false),
        ("Blue", [0x4b, 0x9b, 0xe6], false),
        ("Purple", [0xa2, 0x6b, 0xe0], false),
        ("Grey", [0x9a, 0x9a, 0x9a], false),
    ]
    .into_iter()
    .map(|(name, color, visible)| Collection {
        name: name.to_string(),
        color,
        visible,
    })
    .collect()
}

/// A saved search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomLabel {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub icon: String,
    pub scope: Scope,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tags: BTreeSet<TagId>,
}

/// What the user changed about a Library or Places label.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LabelPrefs {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub hidden: bool,
}

/// A label's Filter View: which groups are showing and which tags are on.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterPrefs {
    /// `None` is the label's default set.
    pub groups: Option<BTreeSet<GroupId>>,
    pub selected: BTreeSet<TagId>,
}

/// A folder added to Places.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserFolder {
    pub path: PathBuf,
}

/// The content pane's columns and sort.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContentPrefs {
    /// After Name, which is always shown, in order.
    pub columns: Vec<Column>,
    pub show_extensions: bool,
    pub sort: Sort,
}

/// The browser's stored state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryData {
    pub version: u32,
    pub tags: TagDb,
    pub collections: Vec<Collection>,
    /// Which collections each item is in, one bit each.
    pub colors: BTreeMap<ItemKey, u8>,
    pub custom_labels: Vec<CustomLabel>,
    pub next_label: u32,
    /// By [`Label::name`] or a Place's path.
    pub labels: BTreeMap<String, LabelPrefs>,
    /// By [`Scope::storage_key`].
    pub filters: BTreeMap<String, FilterPrefs>,
    pub folders: Vec<UserFolder>,
    pub content: ContentPrefs,
    pub show_filters: bool,
    pub show_tag_editor: bool,
    pub show_quick_tags: bool,
    pub auto_tags_enabled: bool,
    /// Tags from sound analysis (A19), kept apart from the user's.
    pub auto_tags: BTreeMap<ItemKey, BTreeSet<TagId>>,
}

impl Default for LibraryData {
    fn default() -> Self {
        Self {
            version: LIBRARY_VERSION,
            tags: TagDb::default(),
            collections: default_collections(),
            colors: BTreeMap::new(),
            custom_labels: Vec::new(),
            next_label: 1,
            labels: BTreeMap::new(),
            filters: BTreeMap::new(),
            folders: Vec::new(),
            content: ContentPrefs::default(),
            show_filters: true,
            show_tag_editor: false,
            show_quick_tags: true,
            auto_tags_enabled: true,
            auto_tags: BTreeMap::new(),
        }
    }
}

/// A change to the stored state. Every edit the UI makes is one of these, sent to the worker.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // Tags.
    AddTag {
        group: GroupId,
        parent: Option<TagId>,
        name: String,
        /// Assign the new tag to these items as well.
        assign: Vec<ItemKey>,
    },
    AddGroup(String),
    RenameTag(TagId, String),
    RenameGroup(GroupId, String),
    DeleteTag(TagId),
    DeleteGroup(GroupId),
    Assign {
        items: Vec<ItemKey>,
        tag: TagId,
        on: bool,
    },
    /// Sound analysis's tags for an item (A19).
    SetAutoTags(ItemKey, BTreeSet<TagId>),
    // Collections.
    /// Puts the items in collection `n`, 0 to 6, or takes them out.
    SetColor {
        items: Vec<ItemKey>,
        collection: u8,
        on: bool,
    },
    /// Takes the items out of every collection.
    ClearColors(Vec<ItemKey>),
    RenameCollection(u8, String),
    SetCollectionVisible(u8, bool),
    // Labels.
    AddCustomLabel {
        name: String,
        scope: Scope,
        text: String,
        tags: BTreeSet<TagId>,
    },
    RenameCustomLabel(u32, String),
    SetCustomLabelIcon(u32, String),
    DeleteCustomLabel(u32),
    /// Renames a Library or Places label; an empty name goes back to its own.
    RenameLabel(String, String),
    SetLabelIcon(String, String),
    SetLabelHidden(String, bool),
    // Filter View.
    SetFilterSelection(String, BTreeSet<TagId>),
    SetFilterGroups(String, Option<BTreeSet<GroupId>>),
    SetShowFilters(bool),
    SetShowTagEditor(bool),
    SetShowQuickTags(bool),
    SetAutoTagsEnabled(bool),
    // Content pane.
    SetColumns(Vec<Column>),
    SetShowExtensions(bool),
    SetSort(Sort),
    // Places.
    AddFolder(PathBuf),
    RemoveFolder(PathBuf),
    /// Points a missing folder at where it is now, keeping what was tagged inside it.
    LocateFolder(PathBuf, PathBuf),
}

impl LibraryData {
    /// Applies an edit. Editing something that can't be edited changes nothing.
    pub fn apply(&mut self, op: &Op) -> Result<(), TagError> {
        match op {
            Op::AddTag {
                group,
                parent,
                name,
                assign,
            } => {
                let id = self.tags.add_tag(*group, *parent, name)?;
                self.tags.assign(assign, id, true);
            }
            Op::AddGroup(name) => {
                self.tags.add_group(name)?;
            }
            Op::RenameTag(id, name) => self.tags.rename_tag(*id, name)?,
            Op::RenameGroup(id, name) => self.tags.rename_group(*id, name)?,
            Op::DeleteTag(id) => self.tags.delete_tag(*id)?,
            Op::DeleteGroup(id) => self.tags.delete_group(*id)?,
            Op::Assign { items, tag, on } => self.tags.assign(items, *tag, *on),
            Op::SetAutoTags(key, tags) => {
                if tags.is_empty() {
                    self.auto_tags.remove(key);
                } else {
                    self.auto_tags.insert(key.clone(), tags.clone());
                }
            }
            Op::SetColor {
                items,
                collection,
                on,
            } => self.set_color(items, *collection, *on),
            Op::ClearColors(items) => {
                for i in items {
                    self.colors.remove(i);
                }
            }
            Op::RenameCollection(n, name) => {
                if let Some(c) = self.collections.get_mut(*n as usize) {
                    c.name = name.trim().to_string();
                }
            }
            Op::SetCollectionVisible(n, on) => {
                if let Some(c) = self.collections.get_mut(*n as usize) {
                    c.visible = *on;
                }
            }
            Op::AddCustomLabel {
                name,
                scope,
                text,
                tags,
            } => {
                let id = self.next_label;
                self.next_label += 1;
                self.custom_labels.push(CustomLabel {
                    id,
                    name: name.clone(),
                    icon: "Tag".to_string(),
                    scope: scope.clone(),
                    text: text.clone(),
                    tags: tags.clone(),
                });
            }
            Op::RenameCustomLabel(id, name) => {
                if let Some(l) = self.custom_labels.iter_mut().find(|l| l.id == *id) {
                    l.name = name.trim().to_string();
                }
            }
            Op::SetCustomLabelIcon(id, icon) => {
                if let Some(l) = self.custom_labels.iter_mut().find(|l| l.id == *id) {
                    l.icon = icon.clone();
                }
            }
            Op::DeleteCustomLabel(id) => self.custom_labels.retain(|l| l.id != *id),
            Op::RenameLabel(key, name) => {
                let trimmed = name.trim();
                let prefs = self.labels.entry(key.clone()).or_default();
                prefs.name = (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
            Op::SetLabelIcon(key, icon) => {
                self.labels.entry(key.clone()).or_default().icon = Some(icon.clone());
            }
            Op::SetLabelHidden(key, hidden) => {
                self.labels.entry(key.clone()).or_default().hidden = *hidden;
            }
            Op::SetFilterSelection(key, tags) => {
                self.filters.entry(key.clone()).or_default().selected = tags.clone();
            }
            Op::SetFilterGroups(key, groups) => {
                self.filters.entry(key.clone()).or_default().groups = groups.clone();
            }
            Op::SetShowFilters(on) => self.show_filters = *on,
            Op::SetShowTagEditor(on) => self.show_tag_editor = *on,
            Op::SetShowQuickTags(on) => self.show_quick_tags = *on,
            Op::SetAutoTagsEnabled(on) => self.auto_tags_enabled = *on,
            Op::SetColumns(columns) => self.content.columns = columns.clone(),
            Op::SetShowExtensions(on) => self.content.show_extensions = *on,
            Op::SetSort(sort) => self.content.sort = *sort,
            Op::AddFolder(path) => {
                if !self.folders.iter().any(|f| f.path == *path) {
                    self.folders.push(UserFolder { path: path.clone() });
                }
            }
            Op::RemoveFolder(path) => self.folders.retain(|f| f.path != *path),
            Op::LocateFolder(from, to) => self.locate_folder(from, to),
        }
        Ok(())
    }

    fn set_color(&mut self, items: &[ItemKey], collection: u8, on: bool) {
        if collection as usize >= COLLECTION_COUNT {
            return;
        }
        let bit = 1 << collection;
        for i in items {
            let mask = self.colors.entry(i.clone()).or_default();
            if on {
                *mask |= bit;
            } else {
                *mask &= !bit;
            }
            if *mask == 0 {
                self.colors.remove(i);
            }
        }
        // A hidden colour that gets something assigned shows up by itself (manual p. 83).
        if on {
            self.collections[collection as usize].visible = true;
        }
    }

    fn locate_folder(&mut self, from: &std::path::Path, to: &std::path::Path) {
        for f in &mut self.folders {
            if f.path == from {
                f.path = to.to_path_buf();
            }
        }
        let prefix = format!("file:{}", from.display());
        let moved = |k: &ItemKey| -> Option<ItemKey> {
            let rest = k.0.strip_prefix(&prefix)?;
            let boundary = rest.is_empty() || rest.starts_with(std::path::MAIN_SEPARATOR);
            boundary.then(|| ItemKey(format!("file:{}{rest}", to.display())))
        };
        let tagged: Vec<(ItemKey, ItemKey)> = self
            .tags
            .assigned
            .keys()
            .filter_map(|k| moved(k).map(|n| (k.clone(), n)))
            .collect();
        for (old, new) in tagged {
            self.tags.rekey(&old, new);
        }
        let coloured: Vec<(ItemKey, ItemKey)> = self
            .colors
            .keys()
            .filter_map(|k| moved(k).map(|n| (k.clone(), n)))
            .collect();
        for (old, new) in coloured {
            if let Some(mask) = self.colors.remove(&old) {
                *self.colors.entry(new).or_default() |= mask;
            }
        }
    }

    /// The name a Library or Places label goes by.
    pub fn label_name(&self, key: &str, own: &str) -> String {
        self.labels
            .get(key)
            .and_then(|p| p.name.clone())
            .unwrap_or_else(|| own.to_string())
    }

    pub fn label_hidden(&self, key: &str) -> bool {
        self.labels.get(key).is_some_and(|p| p.hidden)
    }

    /// Which collections an item is in, one bit each.
    pub fn colors_of(&self, key: &ItemKey) -> u8 {
        self.colors.get(key).copied().unwrap_or(0)
    }

    /// The tags on an item: what it came with, what the user gave it, and sound analysis's.
    pub fn tags_of(&self, key: &ItemKey, factory: &[TagId]) -> BTreeSet<TagId> {
        let mut out: BTreeSet<TagId> = factory.iter().copied().collect();
        if let Some(mine) = self.tags.assigned.get(key) {
            out.extend(mine);
        }
        if self.auto_tags_enabled {
            if let Some(auto) = self.auto_tags.get(key) {
                out.extend(auto);
            }
        }
        out
    }

    /// The filter groups a scope shows, in order: its choice, or its defaults, plus every custom
    /// group something has been tagged from (manual p. 78).
    pub fn visible_groups(&self, scope: &Scope) -> Vec<GroupId> {
        let chosen = self
            .filters
            .get(&scope.storage_key())
            .and_then(|f| f.groups.clone())
            .unwrap_or_else(|| default_groups(scope));
        let used: BTreeSet<GroupId> = self
            .tags
            .assigned
            .values()
            .flatten()
            .filter_map(|t| self.tags.tag(*t).map(|t| t.group))
            .collect();
        self.tags
            .all_groups()
            .into_iter()
            .filter(|g| chosen.contains(&g.id) || (!g.factory && used.contains(&g.id)))
            .map(|g| g.id)
            .collect()
    }
}

/// The filter groups a label starts with.
pub fn default_groups(scope: &Scope) -> BTreeSet<GroupId> {
    let names: &[&str] = match scope {
        Scope::Label(Label::Sounds) => &["Sounds", "Character", "Creator"],
        Scope::Label(Label::Drums) => &["Drums", "Character", "Creator"],
        Scope::Label(Label::Instruments)
        | Scope::Label(Label::AudioEffects)
        | Scope::Label(Label::MidiEffects)
        | Scope::Label(Label::Modulators)
        | Scope::Label(Label::MaxForLive) => &["Devices", "Character", "Creator"],
        Scope::Label(Label::Plugins) => &["Devices", "Creator"],
        Scope::Label(Label::Samples) | Scope::Label(Label::Clips) => {
            &["Drums", "Sounds", "Character"]
        }
        _ => &["Sounds", "Drums", "Devices", "Character", "Creator"],
    };
    names
        .iter()
        .filter_map(|n| factory_group_named(n))
        .collect()
}
