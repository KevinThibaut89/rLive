//! Filter groups and tags.
//!
//! The factory groups are constants, and so immutable: they aren't stored. Custom groups and tags
//! are, along with which items the user tagged, since an item's factory tags are part of the item.

use crate::item::ItemKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GroupId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TagId(pub u32);

/// The first id of custom tags and groups; everything below is factory.
const FIRST_CUSTOM: u32 = 1_000_000;
/// How far apart the factory groups' tag ids are.
const GROUP_STRIDE: u32 = 1000;

/// The name of the group whose tags can't have subtags (manual p. 78).
pub const CREATOR_GROUP: &str = "Creator";

/// rLive's own filter groups and tags, in the order they are shown. Only ever append to these:
/// their position is their id.
const FACTORY: &[(&str, &[&str])] = &[
    (
        "Sounds",
        &[
            "Bass", "Brass", "Keys", "Lead", "Mallets", "Pad", "Plucked", "Strings", "Synth",
            "Texture", "Voices", "Winds", "Sound FX",
        ],
    ),
    (
        "Drums",
        &[
            "Kick",
            "Snare",
            "Clap",
            "Hi-Hat",
            "Cymbal",
            "Tom",
            "Percussion",
            "Drum Loop",
            "Drum Kit",
        ],
    ),
    (
        "Devices",
        &[
            "Delay & Modulation",
            "Distortion",
            "Drum Machine",
            "Dynamics",
            "EQ & Filters",
            "Modulator",
            "Physical Model",
            "Reverb & Resonance",
            "Sampler",
            "Spectral",
            "Synthesizer",
            "Utility",
            "MIDI Tool",
            "Stacks",
        ],
    ),
    (
        "Character",
        &[
            "Bright", "Clean", "Dark", "Dirty", "Evolving", "Hard", "Soft", "Warm", "Wide",
        ],
    ),
    (CREATOR_GROUP, &["rLive", "Third Party", "User"]),
];

/// The tags of the factory group at `group`, in order.
fn factory_group(group: usize) -> Option<TagGroup> {
    let (name, tags) = FACTORY.get(group)?;
    let id = GroupId(group as u32 + 1);
    Some(TagGroup {
        id,
        name: (*name).to_string(),
        factory: true,
        tags: tags
            .iter()
            .enumerate()
            .map(|(i, tag)| Tag {
                id: TagId(id.0 * GROUP_STRIDE + i as u32),
                name: (*tag).to_string(),
                parent: None,
                group: id,
                factory: true,
            })
            .collect(),
    })
}

/// The id of a factory tag by its name. Panics on an unknown name: factory tags are constants.
pub fn factory_tag(name: &str) -> TagId {
    (0..FACTORY.len())
        .filter_map(factory_group)
        .flat_map(|g| g.tags)
        .find(|t| t.name == name)
        .map(|t| t.id)
        .unwrap_or_else(|| panic!("no factory tag named {name}"))
}

/// A factory filter group by name.
pub fn factory_group_named(name: &str) -> Option<GroupId> {
    FACTORY
        .iter()
        .position(|(n, _)| *n == name)
        .map(|i| GroupId(i as u32 + 1))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    /// The tag it is nested under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<TagId>,
    pub group: GroupId,
    #[serde(default)]
    pub factory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagGroup {
    pub id: GroupId,
    pub name: String,
    #[serde(default)]
    pub factory: bool,
    /// Factory tags only; custom tags live in [`TagDb::tags`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
}

/// A tag on the selection, as the Quick Tags section shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickTag {
    pub tag: TagId,
    pub name: String,
    /// Not every selected item has it: shown with an asterisk.
    pub partial: bool,
    /// Whether it can be taken off, which a factory tag on an item can't.
    pub removable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagError {
    #[error("there is already a tag or group called {0}")]
    Duplicate(String),
    #[error("a name can't be empty")]
    EmptyName,
    #[error("that can't be changed")]
    Factory,
    #[error("it doesn't exist")]
    Missing,
    #[error("tags in the Creator group can't have subtags")]
    NoSubtags,
}

/// Custom tags and groups, and what the user tagged with what.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TagDb {
    pub groups: Vec<TagGroup>,
    pub tags: Vec<Tag>,
    pub next_id: u32,
    /// Only what the user assigned.
    pub assigned: BTreeMap<ItemKey, BTreeSet<TagId>>,
}

impl TagDb {
    fn fresh_id(&mut self) -> u32 {
        self.next_id = self.next_id.max(FIRST_CUSTOM) + 1;
        self.next_id
    }

    /// Every group: the factory's, then the user's.
    pub fn all_groups(&self) -> Vec<TagGroup> {
        (0..FACTORY.len())
            .filter_map(factory_group)
            .chain(self.groups.iter().cloned())
            .collect()
    }

    /// Every tag in a group, with custom tags after the factory ones.
    pub fn tags_of(&self, group: GroupId) -> Vec<Tag> {
        let factory = self.all_groups().into_iter().find(|g| g.id == group);
        let mut out = factory.map(|g| g.tags).unwrap_or_default();
        out.extend(self.tags.iter().filter(|t| t.group == group).cloned());
        out
    }

    /// Every tag.
    pub fn all_tags(&self) -> Vec<Tag> {
        let mut out: Vec<Tag> = (0..FACTORY.len())
            .filter_map(factory_group)
            .flat_map(|g| g.tags)
            .collect();
        out.extend(self.tags.iter().cloned());
        out
    }

    pub fn tag(&self, id: TagId) -> Option<Tag> {
        self.all_tags().into_iter().find(|t| t.id == id)
    }

    pub fn group(&self, id: GroupId) -> Option<TagGroup> {
        self.all_groups().into_iter().find(|g| g.id == id)
    }

    /// The group a tag's name appears in, for the status bar.
    pub fn group_of(&self, id: TagId) -> Option<TagGroup> {
        self.tag(id).and_then(|t| self.group(t.group))
    }

    fn name_taken(&self, group: GroupId, parent: Option<TagId>, name: &str) -> bool {
        self.tags_of(group)
            .iter()
            .any(|t| t.parent == parent && t.name.eq_ignore_ascii_case(name))
    }

    /// Adds a tag to a group, or if `parent` is given a subtag of that tag.
    pub fn add_tag(
        &mut self,
        group: GroupId,
        parent: Option<TagId>,
        name: &str,
    ) -> Result<TagId, TagError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TagError::EmptyName);
        }
        let g = self.group(group).ok_or(TagError::Missing)?;
        if parent.is_some() && g.name == CREATOR_GROUP {
            return Err(TagError::NoSubtags);
        }
        if parent.is_some_and(|p| self.tag(p).is_none_or(|t| t.group != group)) {
            return Err(TagError::Missing);
        }
        if self.name_taken(group, parent, name) {
            return Err(TagError::Duplicate(name.to_string()));
        }
        let id = TagId(self.fresh_id());
        self.tags.push(Tag {
            id,
            name: name.to_string(),
            parent,
            group,
            factory: false,
        });
        Ok(id)
    }

    pub fn add_group(&mut self, name: &str) -> Result<GroupId, TagError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TagError::EmptyName);
        }
        if self
            .all_groups()
            .iter()
            .any(|g| g.name.eq_ignore_ascii_case(name))
        {
            return Err(TagError::Duplicate(name.to_string()));
        }
        let id = GroupId(self.fresh_id());
        self.groups.push(TagGroup {
            id,
            name: name.to_string(),
            factory: false,
            tags: Vec::new(),
        });
        Ok(id)
    }

    pub fn rename_tag(&mut self, id: TagId, name: &str) -> Result<(), TagError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TagError::EmptyName);
        }
        let tag = self.tag(id).ok_or(TagError::Missing)?;
        if tag.factory {
            return Err(TagError::Factory);
        }
        if self.name_taken(tag.group, tag.parent, name) && !tag.name.eq_ignore_ascii_case(name) {
            return Err(TagError::Duplicate(name.to_string()));
        }
        if let Some(t) = self.tags.iter_mut().find(|t| t.id == id) {
            t.name = name.to_string();
        }
        Ok(())
    }

    pub fn rename_group(&mut self, id: GroupId, name: &str) -> Result<(), TagError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TagError::EmptyName);
        }
        let group = self.group(id).ok_or(TagError::Missing)?;
        if group.factory {
            return Err(TagError::Factory);
        }
        if self
            .all_groups()
            .iter()
            .any(|g| g.id != id && g.name.eq_ignore_ascii_case(name))
        {
            return Err(TagError::Duplicate(name.to_string()));
        }
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == id) {
            g.name = name.to_string();
        }
        Ok(())
    }

    /// A tag and the subtags under it, as ids.
    pub fn descendants(&self, id: TagId) -> BTreeSet<TagId> {
        let all = self.all_tags();
        let mut found = BTreeSet::from([id]);
        loop {
            let before = found.len();
            for t in &all {
                if t.parent.is_some_and(|p| found.contains(&p)) {
                    found.insert(t.id);
                }
            }
            if found.len() == before {
                return found;
            }
        }
    }

    /// Takes a custom tag, its subtags, and every assignment of them away.
    pub fn delete_tag(&mut self, id: TagId) -> Result<(), TagError> {
        let tag = self.tag(id).ok_or(TagError::Missing)?;
        if tag.factory {
            return Err(TagError::Factory);
        }
        let gone = self.descendants(id);
        self.tags.retain(|t| !gone.contains(&t.id));
        self.forget(&gone);
        Ok(())
    }

    /// Takes a custom group and every tag in it away.
    pub fn delete_group(&mut self, id: GroupId) -> Result<(), TagError> {
        let group = self.group(id).ok_or(TagError::Missing)?;
        if group.factory {
            return Err(TagError::Factory);
        }
        let gone: BTreeSet<TagId> = self
            .tags
            .iter()
            .filter(|t| t.group == id)
            .map(|t| t.id)
            .collect();
        self.tags.retain(|t| t.group != id);
        self.groups.retain(|g| g.id != id);
        self.forget(&gone);
        Ok(())
    }

    fn forget(&mut self, gone: &BTreeSet<TagId>) {
        for tags in self.assigned.values_mut() {
            tags.retain(|t| !gone.contains(t));
        }
        self.assigned.retain(|_, t| !t.is_empty());
    }

    /// Assigns a tag to each item, or takes it off.
    pub fn assign(&mut self, items: &[ItemKey], tag: TagId, on: bool) {
        for item in items {
            if on {
                self.assigned.entry(item.clone()).or_default().insert(tag);
            } else if let Some(tags) = self.assigned.get_mut(item) {
                tags.remove(&tag);
                if tags.is_empty() {
                    self.assigned.remove(item);
                }
            }
        }
    }

    /// Moves what was assigned to one key onto another, as when a folder was located again.
    pub fn rekey(&mut self, from: &ItemKey, to: ItemKey) {
        if let Some(tags) = self.assigned.remove(from) {
            self.assigned.entry(to).or_default().extend(tags);
        }
    }

    /// The Quick Tags section for a selection of `(key, factory tags)`: every tag any item has,
    /// alphabetically, each marked if some of them lack it.
    pub fn quick_tags(&self, items: &[(ItemKey, Vec<TagId>)]) -> Vec<QuickTag> {
        let mut all: BTreeMap<TagId, (usize, bool)> = BTreeMap::new();
        for (key, factory) in items {
            let mine = self.assigned.get(key);
            let tags: BTreeSet<TagId> = factory
                .iter()
                .chain(mine.into_iter().flatten())
                .copied()
                .collect();
            for t in tags {
                let on_factory = factory.contains(&t);
                let e = all.entry(t).or_insert((0, false));
                e.0 += 1;
                e.1 |= on_factory;
            }
        }
        let tags = self.all_tags();
        let mut out: Vec<QuickTag> = all
            .into_iter()
            .filter_map(|(id, (count, factory))| {
                let tag = tags.iter().find(|t| t.id == id)?;
                Some(QuickTag {
                    tag: id,
                    name: tag.name.clone(),
                    partial: count < items.len(),
                    removable: !factory,
                })
            })
            .collect();
        out.sort_by_key(|q| q.name.to_lowercase());
        out
    }

    /// The tags a `#fragment` could be finishing, best first: names starting with it, then
    /// names containing it.
    pub fn completions(&self, fragment: &str) -> Vec<Tag> {
        let f = normalize(fragment);
        let all = self.all_tags();
        let mut starts: Vec<Tag> = Vec::new();
        let mut contains: Vec<Tag> = Vec::new();
        for t in all {
            let n = normalize(&t.name);
            if n.starts_with(&f) {
                starts.push(t);
            } else if n.contains(&f) {
                contains.push(t);
            }
        }
        starts.sort_by_key(|t| t.name.to_lowercase());
        contains.sort_by_key(|t| t.name.to_lowercase());
        starts.extend(contains);
        starts
    }

    /// The tags a `#name` search term means, with their subtags: the name exactly, or while it's
    /// still being typed anything it could be finishing.
    pub fn resolve(&self, term: &str) -> BTreeSet<TagId> {
        let n = normalize(term);
        let tags = self.all_tags();
        let exact: Vec<TagId> = tags
            .iter()
            .filter(|t| normalize(&t.name) == n)
            .map(|t| t.id)
            .collect();
        let chosen = if exact.is_empty() {
            self.completions(term).into_iter().map(|t| t.id).collect()
        } else {
            exact
        };
        chosen
            .into_iter()
            .flat_map(|id| self.descendants(id))
            .collect()
    }
}

/// How tag names are compared: without case, and with spaces and hyphens interchangeable since a
/// `#hashtag` is one word.
fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .collect()
}
