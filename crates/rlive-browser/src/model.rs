//! The browser's navigation state, apart from how it is drawn: where it is, what's open and
//! selected, and how the keyboard moves through it.

use crate::item::{ItemKey, Label};
use crate::query::{Query, Results, Row, Scope, Sort};
use crate::tags::TagId;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Where the browser is: one state of its history.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NavState {
    pub scope: Scope,
    pub text: String,
    pub tags: BTreeSet<TagId>,
}

/// The keyboard moving between the two halves of the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    Sidebar,
    #[default]
    Content,
}

/// What the Left and Right arrows did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// A folder opened or closed, or the keyboard moved: a new query is due.
    Changed,
    /// Right on an item with nothing to open: it should be previewed.
    Preview,
    /// The cursor went to the sidebar.
    ToSidebar,
    Nothing,
}

const MAX_HISTORY: usize = 100;

#[derive(Debug, Default)]
pub struct Model {
    pub nav: NavState,
    past: Vec<NavState>,
    future: Vec<NavState>,
    /// The last state the history knows about, which what's being typed may have moved on from.
    committed: NavState,
    pub sort: Sort,
    pub expanded: BTreeSet<ItemKey>,
    pub rows: Arc<Results>,
    pub selected: BTreeSet<ItemKey>,
    pub cursor: Option<ItemKey>,
    anchor: Option<ItemKey>,
    pub pane: Pane,
    /// The tags on in each scope's Filter View (manual p. 73).
    pub filters: BTreeMap<String, BTreeSet<TagId>>,
    /// Something changed that the content pane should be asked for again.
    pub query_due: bool,
    /// An item to move the cursor to as soon as the rows show it.
    pending_reveal: Option<ItemKey>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            query_due: true,
            ..Self::default()
        }
    }

    pub fn query(&self) -> Query {
        Query {
            id: 0,
            scope: self.nav.scope.clone(),
            text: self.nav.text.clone(),
            tags: self.nav.tags.clone(),
            sort: self.sort,
            expanded: self.expanded.clone(),
        }
    }

    fn remember(&mut self) {
        if self.past.last() != Some(&self.nav) {
            self.past.push(self.nav.clone());
            if self.past.len() > MAX_HISTORY {
                self.past.remove(0);
            }
        }
        self.future.clear();
    }

    /// Moves to a label, bringing back the filters it had (manual p. 73). A search is dropped.
    pub fn navigate(&mut self, scope: Scope) {
        if self.nav.scope == scope {
            return;
        }
        self.remember();
        self.nav.scope = scope;
        self.nav.text.clear();
        self.nav.tags = self
            .filters
            .get(&self.nav.scope.storage_key())
            .cloned()
            .unwrap_or_default();
        self.settled();
        self.query_due = true;
    }

    pub fn set_text(&mut self, text: String) {
        if self.nav.text != text {
            self.nav.text = text;
            self.query_due = true;
        }
    }

    /// Records the current search in the history; call when a search is finished being typed.
    pub fn commit_search(&mut self) {
        if self.committed != self.nav {
            self.past.push(self.committed.clone());
            self.future.clear();
            self.settled();
        }
    }

    fn settled(&mut self) {
        self.committed = self.nav.clone();
    }

    /// Clicking a tag in the Filter View. `multi` keeps the group's other tags on (p. 73);
    /// `same_group` lists the tags of the clicked one's group.
    pub fn click_tag(&mut self, tag: TagId, multi: bool, same_group: &[TagId]) {
        self.remember();
        if multi {
            if !self.nav.tags.remove(&tag) {
                self.nav.tags.insert(tag);
            }
        } else {
            let was_on = self.nav.tags.contains(&tag);
            self.nav.tags.retain(|t| !same_group.contains(t));
            if !was_on {
                self.nav.tags.insert(tag);
            }
        }
        self.filters
            .insert(self.nav.scope.storage_key(), self.nav.tags.clone());
        self.settled();
        self.query_due = true;
    }

    /// The x in the search field: the words go, the tags stay.
    pub fn clear_text(&mut self) {
        self.nav.text.clear();
        self.query_due = true;
    }

    /// The Clear button: words and tags. Esc does this too.
    pub fn clear_all(&mut self) {
        if self.nav.text.is_empty() && self.nav.tags.is_empty() {
            return;
        }
        self.remember();
        self.nav.text.clear();
        self.nav.tags.clear();
        self.filters
            .insert(self.nav.scope.storage_key(), BTreeSet::new());
        self.settled();
        self.query_due = true;
    }

    /// Ctrl/Cmd+F: search everything.
    pub fn focus_search(&mut self) {
        self.navigate(Scope::Label(Label::All));
    }

    pub fn can_go_back(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn back(&mut self) {
        if let Some(state) = self.past.pop() {
            self.future.push(std::mem::replace(&mut self.nav, state));
            self.filters
                .insert(self.nav.scope.storage_key(), self.nav.tags.clone());
            self.settled();
            self.query_due = true;
        }
    }

    pub fn forward(&mut self) {
        if let Some(state) = self.future.pop() {
            self.past.push(std::mem::replace(&mut self.nav, state));
            self.filters
                .insert(self.nav.scope.storage_key(), self.nav.tags.clone());
            self.settled();
            self.query_due = true;
        }
    }

    pub fn set_sort(&mut self, sort: Sort) {
        if self.sort != sort {
            self.sort = sort;
            self.query_due = true;
        }
    }

    /// Takes in a query's answer, keeping the selection on what is still there.
    pub fn take_results(&mut self, results: Arc<Results>) {
        self.rows = results;
        if let Some(target) = self.pending_reveal.clone() {
            if self.rows.rows.iter().any(|r| r.item.key == target) {
                self.pending_reveal = None;
                self.set_single(target);
            }
        }
        if let Some(cursor) = &self.cursor {
            if !self.rows.rows.iter().any(|r| r.item.key == *cursor) {
                self.cursor = None;
            }
        }
    }

    fn index_of(&self, key: &ItemKey) -> Option<usize> {
        self.rows.rows.iter().position(|r| r.item.key == *key)
    }

    pub fn row(&self, key: &ItemKey) -> Option<&Row> {
        self.rows.rows.iter().find(|r| r.item.key == *key)
    }

    pub fn cursor_row(&self) -> Option<&Row> {
        self.cursor.as_ref().and_then(|k| self.row(k))
    }

    fn set_single(&mut self, key: ItemKey) {
        self.selected.clear();
        self.selected.insert(key.clone());
        self.anchor = Some(key.clone());
        self.cursor = Some(key);
    }

    /// Clicks a row: alone, adding with `add`, or as a range with `range`.
    pub fn click(&mut self, key: ItemKey, add: bool, range: bool) {
        if range {
            self.extend_to(&key);
        } else if add {
            if !self.selected.remove(&key) {
                self.selected.insert(key.clone());
            }
            self.anchor = Some(key.clone());
            self.cursor = Some(key);
        } else {
            self.set_single(key);
        }
    }

    fn extend_to(&mut self, key: &ItemKey) {
        let (Some(a), Some(b)) = (
            self.anchor.as_ref().and_then(|k| self.index_of(k)),
            self.index_of(key),
        ) else {
            self.set_single(key.clone());
            return;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        self.selected = self.rows.rows[lo..=hi]
            .iter()
            .map(|r| r.item.key.clone())
            .collect();
        self.cursor = Some(key.clone());
    }

    /// The selection in row order.
    pub fn selected_rows(&self) -> Vec<&Row> {
        self.rows
            .rows
            .iter()
            .filter(|r| self.selected.contains(&r.item.key))
            .collect()
    }

    /// Up and Down. Returns the row moved to.
    pub fn move_cursor(&mut self, delta: i32, extend: bool) -> Option<&Row> {
        let n = self.rows.rows.len();
        if n == 0 {
            return None;
        }
        let at = self.cursor.as_ref().and_then(|k| self.index_of(k));
        let next = match at {
            None => 0,
            Some(i) => (i as i32 + delta).clamp(0, n as i32 - 1) as usize,
        };
        let key = self.rows.rows[next].item.key.clone();
        if extend {
            self.extend_to(&key);
        } else {
            self.set_single(key);
        }
        self.rows.rows.get(next)
    }

    /// Opens or closes a folder. Unless `keep_others` is held, opening one closes the others
    /// at the same level or deeper (manual p. 117).
    pub fn toggle_folder(&mut self, key: &ItemKey, keep_others: bool) {
        if self.expanded.remove(key) {
            self.query_due = true;
            return;
        }
        if !keep_others {
            if let Some(depth) = self.row(key).map(|r| r.depth) {
                let peers: Vec<ItemKey> = self
                    .rows
                    .rows
                    .iter()
                    .filter(|r| r.depth >= depth && r.expanded)
                    .map(|r| r.item.key.clone())
                    .collect();
                for p in peers {
                    self.expanded.remove(&p);
                }
            }
        }
        self.expanded.insert(key.clone());
        self.query_due = true;
    }

    /// Right: opens a folder, or says to preview what's selected.
    pub fn right(&mut self, keep_others: bool) -> Move {
        let Some(row) = self.cursor_row() else {
            return Move::Nothing;
        };
        let key = row.item.key.clone();
        if row.has_children && !row.expanded {
            self.toggle_folder(&key, keep_others);
            return Move::Changed;
        }
        Move::Preview
    }

    /// Left: closes a folder, or goes to the folder this item is in, or at the top level to the
    /// sidebar.
    pub fn left(&mut self) -> Move {
        let Some(row) = self.cursor_row() else {
            self.pane = Pane::Sidebar;
            return Move::ToSidebar;
        };
        let key = row.item.key.clone();
        if row.has_children && row.expanded {
            self.expanded.remove(&key);
            self.query_due = true;
            return Move::Changed;
        }
        if row.depth == 0 {
            self.pane = Pane::Sidebar;
            return Move::ToSidebar;
        }
        let depth = row.depth;
        let at = self.index_of(&key).unwrap_or(0);
        if let Some(parent) = self.rows.rows[..at].iter().rev().find(|r| r.depth < depth) {
            let parent = parent.item.key.clone();
            self.set_single(parent);
            return Move::Changed;
        }
        Move::Nothing
    }

    /// Goes to a scope with some folders open and moves the cursor to `key` once it appears.
    pub fn reveal(&mut self, scope: Scope, open: impl IntoIterator<Item = ItemKey>, key: ItemKey) {
        self.remember();
        self.nav = NavState {
            scope,
            text: String::new(),
            tags: BTreeSet::new(),
        };
        self.settled();
        self.expanded.extend(open);
        self.pending_reveal = Some(key);
        self.query_due = true;
    }
}
