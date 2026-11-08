//! The Edit menu inside a time selection of the automation lanes.

use super::super::arrangement::TimeSel;
use super::super::commands::edit::Edit;
use super::super::commands::ClipBoard;
use super::super::UiState;
use super::{lane_target, LaneKey};
use crate::session::Session;
use rlive_core::param::EnvTarget;
use rlive_project::envelope_edit::{self, RangeClip};

/// Envelope segments on the clipboard, one per lane, in the order the lanes were selected.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeClipboard {
    pub length: f64,
    pub lanes: Vec<RangeClip>,
}

/// The lanes a command acts on and where its time span starts.
fn targets(session: &Session, state: &UiState) -> Vec<(LaneKey, EnvTarget)> {
    let keys: Vec<LaneKey> = match state.arr.time.as_ref().filter(|s| !s.is_point()) {
        Some(sel) => sel.lanes.clone(),
        None => state.arr.auto.focus.into_iter().collect(),
    };
    keys.into_iter()
        .filter_map(|k| Some((k, lane_target(session, k)?)))
        .collect()
}

/// Copies what the selection covers in every lane it spans.
fn copy(session: &Session, state: &UiState, sel: &TimeSel) -> Option<EnvelopeClipboard> {
    let mut lanes = Vec::new();
    for (key, target) in targets(session, state) {
        let param = session.env_param(key.chain, target)?;
        let points = session.env_points(key.chain, target);
        lanes.push(envelope_edit::copy_range(
            points,
            sel.start,
            sel.end,
            param.range,
        ));
    }
    Some(EnvelopeClipboard {
        length: sel.end - sel.start,
        lanes,
    })
}

fn delete(session: &mut Session, state: &UiState, sel: &TimeSel) {
    for (key, target) in targets(session, state) {
        let Some(range) = session.env_param(key.chain, target).map(|p| p.range) else {
            continue;
        };
        let points = envelope_edit::delete_range(
            session.env_points(key.chain, target),
            sel.start,
            sel.end,
            range,
        );
        session.set_envelope(key.chain, target, points);
    }
}

fn paste(session: &mut Session, state: &UiState, clip: &EnvelopeClipboard, at: f64) {
    for (n, (key, target)) in targets(session, state).into_iter().enumerate() {
        let Some(range_clip) = clip.lanes.get(n % clip.lanes.len().max(1)) else {
            continue;
        };
        let Some(param) = session.env_param(key.chain, target) else {
            continue;
        };
        let before = session
            .env_base_value(key.chain, target)
            .unwrap_or(param.info.default);
        let points = envelope_edit::paste_range(
            session.env_points(key.chain, target),
            range_clip,
            at,
            before,
            param.range,
        );
        session.set_envelope(key.chain, target, points);
    }
}

/// Runs an Edit command in the automation lanes.
pub fn edit_selection(session: &mut Session, state: &mut UiState, c: Edit) {
    let sel = state.arr.time.clone().filter(|s| !s.is_point());
    match c {
        Edit::Copy | Edit::Cut => {
            let Some(sel) = sel else { return };
            if let Some(clip) = copy(session, state, &sel) {
                state.clipboard = Some(ClipBoard::Envelopes(clip));
            }
            if c == Edit::Cut {
                delete(session, state, &sel);
            }
        }
        Edit::Paste => {
            let Some(ClipBoard::Envelopes(clip)) = state.clipboard.clone() else {
                return;
            };
            let at = sel
                .as_ref()
                .map_or_else(|| session.insert_marker(), |s| s.start);
            paste(session, state, &clip, at);
        }
        Edit::Duplicate => {
            let Some(sel) = sel else { return };
            if let Some(clip) = copy(session, state, &sel) {
                paste(session, state, &clip, sel.end);
                let len = sel.end - sel.start;
                if let Some(s) = state.arr.time.as_mut() {
                    s.start += len;
                    s.end += len;
                }
            }
        }
        Edit::Delete => {
            if let Some(sel) = sel {
                delete(session, state, &sel);
                state.arr.time = None;
            }
        }
        _ => {}
    }
}

/// Delete Automation on the focused lane, from the keyboard.
pub fn delete_focused_envelope(session: &mut Session, state: &UiState) {
    if let Some(key) = state.arr.auto.focus {
        if let Some(target) = lane_target(session, key) {
            session.delete_automation(key.chain, target);
        }
    }
}
