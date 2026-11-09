//! What the content pane asks for, and how an item is matched against it.

use crate::item::{Item, ItemKey, ItemKind, Label};
use crate::library::LibraryData;
use crate::tags::{factory_group_named, factory_tag, GroupId, TagId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Where in the sidebar the browser is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    Label(Label),
    /// A Collections label, 0 to 6.
    Collection(u8),
    /// A saved search.
    Custom(u32),
    /// A folder under Places: the User Library, a user folder, or the Project.
    Place(PathBuf),
    /// The Set that's open, expanded into its tracks, clips and devices.
    Project,
    /// A similarity search (A19). Finds nothing until then.
    Similar(ItemKey),
}

impl Default for Scope {
    fn default() -> Self {
        Self::Label(Label::All)
    }
}

impl Scope {
    /// How a scope's preferences are stored.
    pub fn storage_key(&self) -> String {
        match self {
            Self::Label(l) => format!("label:{}", l.name()),
            Self::Collection(n) => format!("collection:{n}"),
            Self::Custom(id) => format!("custom:{id}"),
            Self::Place(p) => format!("place:{}", p.display()),
            Self::Project => "project".to_string(),
            Self::Similar(k) => format!("similar:{}", k.0),
        }
    }
}

/// A column the content pane can show after Name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Column {
    Name,
    Type,
    Size,
    Modified,
    Tags,
}

impl Column {
    /// Every column that can be added.
    pub const OPTIONAL: [Column; 4] = [Column::Type, Column::Size, Column::Modified, Column::Tags];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Type => "Type",
            Self::Size => "Size",
            Self::Modified => "Modified",
            Self::Tags => "Tags",
        }
    }
}

/// How rows are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            column: Column::Name,
            descending: false,
        }
    }
}

/// A request for the content pane's rows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Query {
    /// Results carry this back, so a late answer can be told from the current one.
    pub id: u64,
    pub scope: Scope,
    pub text: String,
    /// Tags on in the Filter View.
    pub tags: BTreeSet<TagId>,
    pub sort: Sort,
    /// Open folders, when browsing rather than searching.
    pub expanded: BTreeSet<ItemKey>,
}

impl Query {
    /// Searches and filters show a flat list; otherwise the pane is a tree.
    pub fn is_search(&self) -> bool {
        !self.text.trim().is_empty() || !self.tags.is_empty()
    }
}

/// One line of the content pane.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub item: Arc<Item>,
    pub depth: u8,
    pub has_children: bool,
    pub expanded: bool,
    /// Which collections it is in, one bit each.
    pub colors: u8,
}

/// A query's answer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Results {
    pub id: u64,
    pub rows: Vec<Row>,
    /// Whether this was a search or filter, so the Results bar shows.
    pub search: bool,
}

/// The search field, split: words every result has to contain, and `#tag`s it has to carry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Terms {
    pub words: Vec<String>,
    pub tags: Vec<String>,
}

impl Terms {
    pub fn parse(text: &str) -> Self {
        let mut out = Terms::default();
        for token in text.split_whitespace() {
            match token.strip_prefix('#') {
                Some("") => {}
                Some(tag) => out.tags.push(tag.to_string()),
                None => out.words.push(token.to_lowercase()),
            }
        }
        out
    }
}

/// Which `#fragment` the search field ends in, for autocomplete.
pub fn trailing_hashtag(text: &str) -> Option<&str> {
    if text.ends_with(char::is_whitespace) {
        return None;
    }
    text.split_whitespace().last()?.strip_prefix('#')
}

/// Replaces the `#fragment` the field ends in with a finished tag.
pub fn complete_hashtag(text: &str, tag_name: &str) -> String {
    let word = text.split_whitespace().last().unwrap_or("");
    let head = &text[..text.len() - word.len()];
    let tag: String = tag_name.split_whitespace().collect::<Vec<_>>().join("_");
    format!("{head}#{tag} ")
}

/// A query made ready to test many items against.
pub struct Matcher<'a> {
    lib: &'a LibraryData,
    words: Vec<String>,
    /// For each `#tag` term the tags that satisfy it, or an empty set for no such tag.
    hashtags: Vec<BTreeSet<TagId>>,
    /// Filter View selections grouped by filter group: within one any may match, across groups
    /// all must (manual p. 73).
    selected: BTreeMap<GroupId, BTreeSet<TagId>>,
    sounds: GroupId,
    drums: GroupId,
    modulator: TagId,
}

impl<'a> Matcher<'a> {
    pub fn new(query_text: &str, tags: &BTreeSet<TagId>, lib: &'a LibraryData) -> Self {
        let terms = Terms::parse(query_text);
        let hashtags = terms.tags.iter().map(|t| lib.tags.resolve(t)).collect();
        let mut selected: BTreeMap<GroupId, BTreeSet<TagId>> = BTreeMap::new();
        for tag in tags {
            if let Some(info) = lib.tags.tag(*tag) {
                selected
                    .entry(info.group)
                    .or_default()
                    .extend(lib.tags.descendants(*tag));
            }
        }
        Self {
            lib,
            words: terms.words,
            hashtags,
            selected,
            sounds: factory_group_named("Sounds").expect("factory group"),
            drums: factory_group_named("Drums").expect("factory group"),
            modulator: factory_tag("Modulator"),
        }
    }

    /// Whether `item` is in a Library label, which being tagged can also put it in.
    pub fn in_label(&self, item: &Item, label: Label) -> bool {
        if item.labels.contains(label) {
            return true;
        }
        let group = match label {
            Label::Sounds => self.sounds,
            Label::Drums => self.drums,
            Label::Modulators => {
                return self
                    .lib
                    .tags_of(&item.key, &item.factory_tags)
                    .contains(&self.modulator);
            }
            _ => return false,
        };
        self.lib
            .tags_of(&item.key, &item.factory_tags)
            .iter()
            .any(|t| self.lib.tags.tag(*t).is_some_and(|t| t.group == group))
    }

    /// Whether `item` is a hit, given what's in its search text already lowered.
    pub fn matches(&self, item: &Item, haystack: &str) -> bool {
        if !self.words.iter().all(|w| haystack.contains(w)) {
            return false;
        }
        if self.hashtags.is_empty() && self.selected.is_empty() {
            return true;
        }
        let tags = self.lib.tags_of(&item.key, &item.factory_tags);
        self.hashtags
            .iter()
            .all(|set| set.iter().any(|t| tags.contains(t)))
            && self
                .selected
                .values()
                .all(|set| set.iter().any(|t| tags.contains(t)))
    }
}

/// Where a kind sorts: devices first, then things to play, then files.
pub fn kind_rank(kind: ItemKind) -> u8 {
    match kind {
        ItemKind::Folder | ItemKind::ProjectPart => 0,
        ItemKind::Device => 1,
        ItemKind::Plugin => 2,
        ItemKind::Preset => 3,
        ItemKind::Clip => 4,
        ItemKind::Sample => 5,
        ItemKind::Groove => 6,
        ItemKind::Template | ItemKind::Set => 7,
        ItemKind::Tuning | ItemKind::ChordBank => 8,
        ItemKind::Other => 9,
    }
}

/// Orders rows of a tree level or result list: by kind rank, then by the chosen column.
pub fn compare(a: &Item, b: &Item, sort: Sort) -> std::cmp::Ordering {
    let by_rank = kind_rank(a.kind).cmp(&kind_rank(b.kind));
    if by_rank.is_ne() {
        return by_rank;
    }
    let by_col = match sort.column {
        Column::Name | Column::Tags => natural(&a.name, &b.name),
        Column::Type => a.kind.label().cmp(b.kind.label()),
        Column::Size => a.size.cmp(&b.size),
        Column::Modified => a.modified.cmp(&b.modified),
    };
    let by_col = if sort.descending {
        by_col.reverse()
    } else {
        by_col
    };
    by_col.then_with(|| a.key.cmp(&b.key))
}

fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (la, lb) = (a.to_lowercase(), b.to_lowercase());
    la.cmp(&lb)
}
