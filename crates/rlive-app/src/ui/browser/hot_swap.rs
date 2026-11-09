//! Hot-Swap mode (manual pp. 459-462), linking something on a track to the browser.

use super::super::UiState;
use crate::session::{ChainRef, Session, SwapItem, SwapTarget};
use rlive_browser::item::{ItemKey, Label, Role};
use rlive_browser::preset::{device_key, device_role};
use rlive_browser::Scope;
use rlive_core::DeviceId;
use rlive_project::{DeviceKind, TrackKind};

/// Hot-Swap is on, for this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotSwap {
    pub target: SwapTarget,
    /// The Drum Rack this is on, for D to flip between the rack and its last pad (manual p. 503).
    pub rack: Option<(ChainRef, DeviceId, Option<u8>)>,
}

/// What a device or pad asked Hot-Swap for, from drawing.
#[derive(Debug, Clone, Copy)]
pub enum SwapRequest {
    Start(SwapTarget),
}

fn device_at(session: &Session, chain: ChainRef, id: DeviceId) -> Option<rlive_project::Device> {
    session.devices(chain).iter().find(|d| d.id == id).cloned()
}

/// What Q applies to: the selected device, or the first audio effect on an audio track and the
/// instrument on a MIDI track (manual p. 459).
fn default_target(session: &Session, state: &UiState) -> Option<SwapTarget> {
    if let Some(&(chain, id)) = state.devices.selected_devices.last() {
        return Some(SwapTarget::Device { chain, id });
    }
    let chain = state.selected?;
    let is_midi = matches!(chain, ChainRef::Track(t) if session.project.track(t).is_some_and(|t| t.kind == TrackKind::Midi));
    let wanted = if is_midi {
        Role::Instrument
    } else {
        Role::AudioEffect
    };
    let found = session
        .devices(chain)
        .iter()
        .find(|d| device_role(d) == Some(wanted))?;
    Some(SwapTarget::Device {
        chain,
        id: found.id,
    })
}

/// Opens the browser on where the target's replacements are.
fn reveal(session: &Session, state: &mut UiState, target: &SwapTarget) {
    match *target {
        SwapTarget::Device { chain, id } => {
            let Some(device) = device_at(session, chain, id) else {
                return;
            };
            let key = device_key(&device);
            let scope = match device_role(&device) {
                Some(Role::Instrument) => Scope::Label(Label::Instruments),
                Some(Role::AudioEffect) => Scope::Label(Label::AudioEffects),
                Some(Role::MidiEffect) => Scope::Label(Label::MidiEffects),
                None => Scope::Label(Label::Plugins),
            };
            let item = ItemKey::builtin(&key);
            state.browser.model.reveal(scope, [item.clone()], item);
        }
        SwapTarget::Sample { chain, id, slot } => {
            let file = device_at(session, chain, id).and_then(|d| d.sample(slot).cloned());
            if let Some(file) = file {
                let parent = file
                    .path
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default();
                let scope = Scope::Place(parent);
                state
                    .browser
                    .model
                    .reveal(scope, [], ItemKey::file(&file.path));
            } else {
                state.browser.model.navigate(Scope::Label(Label::Samples));
            }
        }
        SwapTarget::Pad { .. } | SwapTarget::RackChain { .. } => {
            state.browser.model.navigate(Scope::Label(Label::Drums));
        }
    }
    state.focus = crate::ui::keymap::FocusArea::Browser;
    state.show_browser = true;
}

/// Q: on, for the device in focus; or off.
pub fn toggle_hot_swap(session: &Session, state: &mut UiState) {
    if state.browser.hot_swap.is_some() {
        exit_hot_swap(state);
        return;
    }
    let Some(target) = default_target(session, state) else {
        state.status = "Select a device to Hot-Swap".into();
        return;
    };
    start(session, state, target);
}

pub(crate) fn start(session: &Session, state: &mut UiState, target: SwapTarget) {
    let rack = drum_rack_of(session, state, &target);
    state.browser.hot_swap = Some(HotSwap { target, rack });
    reveal(session, state, &target);
}

pub fn exit_hot_swap(state: &mut UiState) {
    state.browser.hot_swap = None;
}

fn drum_rack_of(
    session: &Session,
    state: &UiState,
    target: &SwapTarget,
) -> Option<(ChainRef, DeviceId, Option<u8>)> {
    let (chain, rack) = match *target {
        SwapTarget::Device { chain, id } => {
            let d = device_at(session, chain, id)?;
            matches!(
                d.kind,
                DeviceKind::Rack {
                    kind: rlive_project::RackKind::Drum,
                    ..
                }
            )
            .then_some((chain, id))?
        }
        SwapTarget::Pad { chain, rack, .. } => (chain, rack),
        _ => return None,
    };
    let last = state.devices.rack_views.get(&rack).and_then(|v| v.last_pad);
    Some((chain, rack, last))
}

/// D: between the Drum Rack and its last selected pad.
pub fn toggle_drum_pad_target(state: &mut UiState) {
    let Some(swap) = state.browser.hot_swap.as_mut() else {
        return;
    };
    let Some((chain, rack, last)) = swap.rack else {
        return;
    };
    swap.target = match swap.target {
        SwapTarget::Device { .. } => match last {
            Some(note) => SwapTarget::Pad { chain, rack, note },
            None => return,
        },
        SwapTarget::Pad { .. } => SwapTarget::Device { chain, id: rack },
        other => other,
    };
}

/// Carries out what the Device View asked for while it was drawn.
pub fn poll_requests(session: &Session, state: &mut UiState) {
    for request in std::mem::take(&mut state.devices.swap_requests) {
        let SwapRequest::Start(target) = request;
        // The same button again turns it off (manual pp. 460, 462).
        if state.browser.hot_swap.is_some_and(|s| s.target == target) {
            exit_hot_swap(state);
        } else {
            start(session, state, target);
        }
    }
    if let Some(path) = state.devices.reveal_saved.take() {
        let folder = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let key = ItemKey::file(&path);
        let text = path
            .file_stem()
            .map_or(String::new(), |s| s.to_string_lossy().into_owned());
        state
            .browser
            .model
            .reveal(Scope::Place(folder.clone()), [], key);
        state.browser.pending_save = Some(super::SaveName { text, path });
        state.show_browser = true;
        state.focus = crate::ui::keymap::FocusArea::Browser;
    }
}

/// Replaces the target with what's selected. Returns whether anything happened.
pub(super) fn swap_selected(session: &mut Session, state: &mut UiState) -> bool {
    let Some(swap) = state.browser.hot_swap else {
        return false;
    };
    let Some(row) = state.browser.model.cursor_row().cloned() else {
        return false;
    };
    let Some(item) = super::item_of(session, &row) else {
        return false;
    };
    let result = match &item {
        crate::ui::BrowserItem::Builtin(t) => session.hot_swap(&swap.target, SwapItem::Builtin(t)),
        crate::ui::BrowserItem::Plugin(p) => session.hot_swap(&swap.target, SwapItem::Plugin(p)),
        crate::ui::BrowserItem::Sample(p) => session.hot_swap(&swap.target, SwapItem::Sample(p)),
        crate::ui::BrowserItem::Preset(source) => source
            .load()
            .and_then(|p| session.hot_swap(&swap.target, SwapItem::Preset(&p))),
        _ => Err("That can't be Hot-Swapped in".to_string()),
    };
    if let Err(e) = result {
        state.status = e;
    }
    true
}

/// The name the Hot-Swap bar shows.
pub(super) fn target_name(session: &Session, swap: Option<&HotSwap>) -> String {
    match swap.map(|s| s.target) {
        Some(SwapTarget::Device { chain, id }) => {
            device_at(session, chain, id).map_or_else(|| "?".to_string(), |d| d.name)
        }
        Some(SwapTarget::Sample { chain, id, .. }) => device_at(session, chain, id)
            .map_or_else(|| "?".to_string(), |d| format!("{} sample", d.name)),
        Some(SwapTarget::Pad { note, .. }) => format!("Pad {note}"),
        Some(SwapTarget::RackChain { .. }) => "Chain".to_string(),
        None => String::new(),
    }
}
