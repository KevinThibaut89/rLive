//! Turning a browser row into something on a track.

use super::super::{BrowserItem, UiState};
use crate::session::{ChainRef, ClipRef, Session};
use rlive_browser::catalog::{FACTORY_GROOVE, FACTORY_TUNING};
use rlive_browser::item::{Item, ItemKey, ItemKind};
use rlive_browser::{factory, Preset, Row};
use rlive_project::TrackKind;
use rlive_project::{factory_grooves, factory_tunings, Groove, Tuning};
use std::path::{Path, PathBuf};

/// Where a preset comes from.
#[derive(Clone, Debug)]
pub enum PresetSource {
    Factory(String),
    File(PathBuf),
}

impl PresetSource {
    pub fn load(&self) -> Result<Preset, String> {
        match self {
            Self::Factory(key) => {
                factory::get(key).ok_or_else(|| format!("No factory preset {key}"))
            }
            Self::File(path) => Preset::load(path).map_err(|e| e.to_string()),
        }
    }
}

/// Where a groove or tuning comes from.
#[derive(Clone, Debug)]
pub enum AssetSource {
    /// One of the factory set, by name.
    Factory(String),
    File(PathBuf),
}

impl AssetSource {
    fn of(item: &Item, prefix: &str) -> Option<Self> {
        match item.key.as_factory() {
            Some(key) => Some(Self::Factory(key.strip_prefix(prefix)?.to_string())),
            None => Some(Self::File(item.path.clone()?)),
        }
    }

    fn stem(path: &Path) -> &str {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
    }

    fn groove(&self) -> Result<Groove, String> {
        match self {
            Self::Factory(name) => factory_grooves()
                .into_iter()
                .find(|g| g.name == *name)
                .ok_or_else(|| format!("No factory groove {name}")),
            Self::File(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                let mut groove = Groove::from_file(&text)?;
                groove.name = Self::stem(path).to_string();
                groove.file = Some(rlive_project::FileRef::new(path));
                Ok(groove)
            }
        }
    }

    fn tuning(&self) -> Result<Tuning, String> {
        match self {
            Self::Factory(name) => factory_tunings()
                .into_iter()
                .find(|t| t.name == *name)
                .ok_or_else(|| format!("No factory tuning {name}")),
            Self::File(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                Tuning::parse(&text, Self::stem(path))
            }
        }
    }
}

/// What the selection is, as something that can be dragged or loaded.
pub fn item_of(session: &Session, row: &Row) -> Option<BrowserItem> {
    let item = &row.item;
    match item.kind {
        ItemKind::Device => {
            let id = item.device.as_deref()?;
            rlive_devices::find(id).map(|i| BrowserItem::Builtin(i.id))
        }
        ItemKind::Plugin => {
            let rest = item.key.0.strip_prefix("plugin:")?;
            let (format, id) = rest.split_once(':')?;
            session
                .plugins
                .plugins
                .iter()
                .find(|p| p.format.label() == format && p.id == id)
                .cloned()
                .map(BrowserItem::Plugin)
        }
        ItemKind::Preset => Some(BrowserItem::Preset(match item.key.as_factory() {
            Some(key) => PresetSource::Factory(key.to_string()),
            None => PresetSource::File(item.path.clone()?),
        })),
        ItemKind::Sample => Some(BrowserItem::Sample(item.path.clone()?)),
        ItemKind::Clip => Some(BrowserItem::Clip(item.path.clone()?)),
        ItemKind::Set | ItemKind::Template => Some(BrowserItem::Set(item.path.clone()?)),
        ItemKind::ProjectPart => project_item(item),
        ItemKind::Groove => Some(BrowserItem::Groove(AssetSource::of(item, FACTORY_GROOVE)?)),
        ItemKind::Tuning => Some(BrowserItem::Tuning(AssetSource::of(item, FACTORY_TUNING)?)),
        ItemKind::ChordBank => Some(BrowserItem::ChordBank(item.path.clone()?)),
        ItemKind::Folder | ItemKind::Other => None,
    }
}

fn project_item(item: &Item) -> Option<BrowserItem> {
    let part = item.key.as_project()?;
    if let Some(device) = part.strip_prefix("device:") {
        return Some(BrowserItem::ProjectDevice(rlive_core::DeviceId(
            device.parse().ok()?,
        )));
    }
    if let Some(rest) = part.strip_prefix("clip:") {
        let (track, slot) = rest.split_once(':')?;
        return Some(BrowserItem::ProjectClip(ClipRef::slot(
            rlive_core::TrackId(track.parse().ok()?),
            slot.parse().ok()?,
        )));
    }
    None
}

/// The Groove Pool's entry for a groove row, adding it if it isn't there, for dragging it onto a
/// clip. `None` for anything else, or a multi-selection.
pub(super) fn pool_groove(
    session: &mut Session,
    state: &mut UiState,
    row: &Row,
) -> Option<rlive_core::GrooveId> {
    if row.item.kind != ItemKind::Groove || state.browser.model.selected.len() > 1 {
        return None;
    }
    let BrowserItem::Groove(source) = item_of(session, row)? else {
        return None;
    };
    let groove = match source.groove() {
        Ok(g) => g,
        Err(e) => {
            state.status = e;
            return None;
        }
    };
    let known = session
        .project
        .groove_pool
        .grooves
        .iter()
        .find(|g| g.name == groove.name && g.file == groove.file)
        .map(|g| g.id);
    Some(known.unwrap_or_else(|| session.add_groove(groove)))
}

/// Double-click or Enter on a groove: into the Groove Pool, or with the clip's Hot-Swap on in
/// place of the groove that clip uses (manual pp. 345-347).
fn load_groove(session: &mut Session, state: &mut UiState, source: &AssetSource) {
    let hot_swap = state
        .clip_view
        .tools
        .hot_swap
        .then_some(state.selected_clip)
        .flatten();
    let result = match (hot_swap, source) {
        (Some(clip), _) => source.groove().map(|g| {
            session.hot_swap_groove(clip, g);
        }),
        (None, AssetSource::File(path)) => session.load_groove_file(path).map(|_| ()),
        (None, AssetSource::Factory(_)) => source.groove().map(|g| {
            session.add_groove(g);
        }),
    };
    match result {
        Ok(()) => state.show_groove_pool = true,
        Err(e) => state.status = e,
    }
}

/// Loads a tuning system, through the Retune question when that option is on, and opens the
/// Tuning section (manual p. 353).
pub fn load_tuning(session: &mut Session, state: &mut UiState, source: &AssetSource) {
    match source.tuning() {
        Ok(tuning) => {
            let retune = state.retune_on_load;
            crate::ui::tuning_section::ask_to_load(session, state, retune, tuning);
            state.show_tuning = true;
        }
        Err(e) => state.status = e,
    }
}

fn load_chord_bank(state: &mut UiState, path: &Path) {
    let tools = &mut state.clip_view.midi_tools;
    if let Err(e) = crate::ui::clip_view::tools::load_chord_bank(tools, path) {
        state.status = e;
    }
}

/// What dragging this row carries: the selection when it's part of one.
pub(super) fn drag_item(session: &Session, state: &UiState, row: &Row) -> Option<BrowserItem> {
    if state.browser.model.selected.contains(&row.item.key)
        && state.browser.model.selected.len() > 1
    {
        let items: Vec<BrowserItem> = state
            .browser
            .model
            .selected_rows()
            .iter()
            .filter_map(|r| item_of(session, r))
            .collect();
        return (items.len() > 1)
            .then_some(BrowserItem::Many(items))
            .or_else(|| item_of(session, row));
    }
    item_of(session, row)
}

/// What the Current Project label lists.
pub(super) fn project_items(session: &Session) -> Vec<Item> {
    let mut out = Vec::new();
    let part = |key: String, name: &str, parent: Option<ItemKey>| {
        let mut item = Item::new(ItemKey::project(&key), name, ItemKind::ProjectPart);
        item.parent = parent;
        item
    };
    let set_key = ItemKey::project("set");
    let set_name = session
        .path
        .as_deref()
        .and_then(|p| p.file_stem())
        .map_or_else(
            || "Untitled".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
    out.push(part("set".to_string(), &set_name, None));
    let add_track = |t: &rlive_project::Track, tag: &str, out: &mut Vec<Item>| {
        let track_key = format!("{tag}:{}", t.id.0);
        out.push(part(track_key.clone(), &t.name, Some(set_key.clone())));
        let parent = ItemKey::project(&track_key);
        for (slot, clip) in t.clips.iter().enumerate() {
            if let Some(c) = clip {
                let name = if c.name.is_empty() {
                    format!("Clip {}", slot + 1)
                } else {
                    c.name.clone()
                };
                out.push(part(
                    format!("clip:{}:{}", t.id.0, slot),
                    &name,
                    Some(parent.clone()),
                ));
            }
        }
        for d in &t.devices {
            out.push(part(
                format!("device:{}", d.id.0),
                &d.name,
                Some(parent.clone()),
            ));
        }
    };
    for t in &session.project.tracks {
        add_track(t, "track", &mut out);
    }
    for t in &session.project.returns {
        add_track(t, "return", &mut out);
    }
    add_track(&session.project.master, "main", &mut out);
    for g in &session.project.groove_pool.grooves {
        out.push(part(
            format!("groove:{}", g.id.0),
            &g.name,
            Some(set_key.clone()),
        ));
    }
    out
}

/// A dropped sample on a MIDI track: into a Simpler, or the track's Simpler-to-be.
fn sample_on_midi_track(
    session: &mut Session,
    state: &mut UiState,
    id: rlive_core::TrackId,
    path: &std::path::Path,
) {
    crate::ui::devices::add_item(
        session,
        ChainRef::Track(id),
        BrowserItem::Sample(path.to_path_buf()),
    );
    state.selected = Some(ChainRef::Track(id));
}

fn sample_on_audio_track(
    session: &mut Session,
    state: &mut UiState,
    id: rlive_core::TrackId,
    path: &std::path::Path,
    slot: Option<usize>,
) {
    match session.audio_clip_from_file(path, id) {
        Ok(c) => {
            let slot = slot
                .or_else(|| {
                    session
                        .project
                        .track(id)
                        .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                })
                .unwrap_or(0);
            session.set_clip(id, slot, Some(c));
            state.selected_clip = Some(ClipRef::slot(id, slot));
        }
        Err(e) => state.status = e,
    }
}

fn track_kind(session: &Session, id: rlive_core::TrackId) -> Option<TrackKind> {
    session.project.track(id).map(|t| t.kind)
}

/// Loads an item onto a track at a slot, as a drop there or a double-click does.
pub fn drop_item(
    session: &mut Session,
    state: &mut UiState,
    id: rlive_core::TrackId,
    item: &BrowserItem,
    slot: Option<usize>,
    spread: bool,
) {
    let chain = ChainRef::Track(id);
    match item {
        BrowserItem::Builtin(t) => session.add_builtin(chain, t),
        BrowserItem::Plugin(p) => session.add_plugin(chain, p),
        BrowserItem::Sample(path) => match track_kind(session, id) {
            Some(TrackKind::Audio) => sample_on_audio_track(session, state, id, path, slot),
            Some(TrackKind::Midi) => sample_on_midi_track(session, state, id, path),
            _ => {
                state.status = "Samples go on audio or MIDI tracks".into();
                return;
            }
        },
        BrowserItem::Preset(source) => match source.load() {
            Ok(preset) => {
                if let Err(e) = session.load_preset(chain, None, &preset) {
                    state.status = e;
                }
            }
            Err(e) => state.status = e,
        },
        BrowserItem::Groove(source) => {
            load_groove(session, state, source);
            return;
        }
        BrowserItem::Tuning(source) => {
            load_tuning(session, state, source);
            return;
        }
        BrowserItem::ChordBank(path) => {
            load_chord_bank(state, path);
            return;
        }
        BrowserItem::Clip(path) => match rlive_browser::preset::load_clip(path) {
            Ok(clip) => {
                let at = slot
                    .or_else(|| {
                        session
                            .project
                            .track(id)
                            .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                    })
                    .unwrap_or(0);
                session.set_clip(id, at, Some(clip));
                state.selected_clip = Some(ClipRef::slot(id, at));
            }
            Err(e) => state.status = e,
        },
        BrowserItem::Set(path) => state.browser.open_set = Some(path.clone()),
        BrowserItem::ProjectDevice(device) => copy_device_to(session, state, *device, chain),
        BrowserItem::ProjectClip(cref) => {
            if let Some(clip) = session.clip_at(*cref).cloned() {
                let at = slot
                    .or_else(|| {
                        session
                            .project
                            .track(id)
                            .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                    })
                    .unwrap_or(0);
                session.set_clip(id, at, Some(clip));
                state.selected_clip = Some(ClipRef::slot(id, at));
            }
        }
        BrowserItem::Many(items) => {
            drop_many(session, state, id, items, slot, spread);
            return;
        }
    }
    state.selected = Some(ChainRef::Track(id));
}

/// A multi-selection dropped on a track: samples go in one track's slots, or with Cmd one per
/// track as a scene, on audio tracks (manual p. 195); anything else loads in turn.
fn drop_many(
    session: &mut Session,
    state: &mut UiState,
    id: rlive_core::TrackId,
    items: &[BrowserItem],
    slot: Option<usize>,
    spread: bool,
) {
    let samples: Vec<PathBuf> = items
        .iter()
        .filter_map(|i| match i {
            BrowserItem::Sample(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    if samples.len() == items.len() && track_kind(session, id) == Some(TrackKind::Audio) {
        let first = slot.unwrap_or_else(|| {
            session
                .project
                .track(id)
                .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                .unwrap_or(0)
        });
        let placed = session.drop_files(&samples, id, first, spread);
        if let Some((track, at)) = placed.last() {
            state.selected_clip = Some(ClipRef::slot(*track, *at));
        }
        return;
    }
    for item in items {
        drop_item(session, state, id, item, None, false);
    }
}

fn copy_device_to(
    session: &mut Session,
    state: &mut UiState,
    device: rlive_core::DeviceId,
    target: ChainRef,
) {
    let found = session
        .chain_of_device(device)
        .and_then(|at| session.devices(at).iter().find(|d| d.id == device).cloned());
    let Some(d) = found else {
        state.status = "That device is gone".into();
        return;
    };
    let preset = Preset::of_device(&d, &d.name);
    if let Err(e) = session.load_preset(target, None, &preset) {
        state.status = e;
    }
}

/// Double-click or Enter on the selection (manual pp. 120-121).
pub fn apply_selection(session: &mut Session, state: &mut UiState) {
    if state.browser.hot_swap.is_some() {
        super::hot_swap::swap_selected(session, state);
        return;
    }
    let rows: Vec<Row> = state
        .browser
        .model
        .selected_rows()
        .into_iter()
        .cloned()
        .collect();
    let items: Vec<BrowserItem> = rows.iter().filter_map(|r| item_of(session, r)).collect();
    let Some(first) = items.first().cloned() else {
        state.status = rows
            .first()
            .filter(|r| matches!(r.item.kind, ItemKind::Groove | ItemKind::Tuning))
            .map_or(String::new(), |r| {
                format!("{} can't be loaded from here yet", r.item.name)
            });
        return;
    };
    let item = if items.len() > 1 {
        BrowserItem::Many(items)
    } else {
        first
    };
    apply(session, state, &item);
}

/// Loads an item on the selected track, making one if there is none.
pub(crate) fn apply(session: &mut Session, state: &mut UiState, item: &BrowserItem) {
    if let (BrowserItem::Sample(path), Some(cref)) = (item, sample_target(session, state)) {
        session.replace_clip_sample(cref, path);
        return;
    }
    if let BrowserItem::Set(p) = item {
        state.browser.open_set = Some(p.clone());
        return;
    }
    // These load into the Set, whatever's selected.
    match item {
        BrowserItem::Groove(source) => return load_groove(session, state, source),
        BrowserItem::Tuning(source) => return load_tuning(session, state, source),
        BrowserItem::ChordBank(path) => return load_chord_bank(state, path),
        _ => {}
    }
    let id = match state.selected {
        Some(ChainRef::Track(t)) => t,
        Some(chain @ (ChainRef::Master | ChainRef::Nested(_))) => {
            add_to_chain(session, state, chain, item);
            return;
        }
        None => {
            let kind = match item {
                BrowserItem::Sample(_) => TrackKind::Audio,
                _ => TrackKind::Midi,
            };
            session.add_track(kind, None)
        }
    };
    drop_item(session, state, id, item, None, false);
}

fn add_to_chain(session: &mut Session, state: &mut UiState, chain: ChainRef, item: &BrowserItem) {
    match item {
        BrowserItem::Builtin(t) => session.add_builtin(chain, t),
        BrowserItem::Plugin(p) => session.add_plugin(chain, p),
        BrowserItem::Preset(source) => match source
            .load()
            .and_then(|p| session.load_preset(chain, None, &p))
        {
            Ok(_) => {}
            Err(e) => state.status = e,
        },
        BrowserItem::Sample(_) => state.status = "Samples can't go on the Master".into(),
        _ => {}
    }
}

/// The audio clip the Clip View is showing, which a sample from the browser goes into.
fn sample_target(session: &Session, state: &UiState) -> Option<ClipRef> {
    let cref = state.selected_clip?;
    let showing = state.show_detail && state.detail == crate::ui::Detail::Clip;
    (showing && session.clip_at(cref).is_some_and(|c| !c.is_midi())).then_some(cref)
}
