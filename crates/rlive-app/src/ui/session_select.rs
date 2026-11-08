//! What's selected in the Session grid beyond the one clip the Clip View shows.

use super::UiState;
use crate::session::{ClipRef, Session};
use crate::settings::LaunchPrefs;
use egui::Pos2;
use rlive_core::TrackId;

/// A field of a scene's row that can be typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneField {
    Name,
    Tempo,
    Signature,
}

/// The text being typed into a scene's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneEdit {
    pub scene: usize,
    pub field: SceneField,
    pub text: String,
    /// The field has yet to take keyboard focus.
    pub focus: bool,
}

/// Selection and gestures in the Session grid.
#[derive(Default)]
pub struct SessionSelection {
    /// Every slot selected, the one in `UiState::selected_clip` among them.
    pub slots: Vec<(TrackId, usize)>,
    /// Where a Shift-click extends from.
    pub anchor: Option<(TrackId, usize)>,
    pub scenes: Vec<usize>,
    pub scene_anchor: Option<usize>,
    /// The launch button being held down.
    pub held: Option<(TrackId, usize)>,
    /// The clip launched by the Enter key, while it's still down.
    pub held_key: Option<(TrackId, usize)>,
    /// Where a rubber-band drag started.
    pub rubber: Option<Pos2>,
    pub scene_edit: Option<SceneEdit>,
    /// The Scene Tempo or Time Signature field that's selected.
    pub scene_field: Option<(usize, SceneField)>,
    /// How far the Main track has been widened for the Scene Tempo and Signature controls.
    pub scene_fields_w: f32,
    /// A copy of the Launch settings.
    pub prefs: LaunchPrefs,
}

impl SessionSelection {
    /// Clears everything selected in the grid, slots and scenes alike.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.scenes.clear();
        self.anchor = None;
        self.scene_anchor = None;
        self.scene_field = None;
    }
}

/// The clips the Clip View's launch controls act on: the selection if the shown clip is in it.
pub fn launch_targets(state: &UiState) -> Vec<ClipRef> {
    let Some(shown) = state.selected_clip else {
        return Vec::new();
    };
    let in_selection = shown
        .as_slot()
        .is_some_and(|s| state.sel.slots.contains(&s) && state.sel.slots.len() > 1);
    if in_selection {
        state
            .sel
            .slots
            .iter()
            .map(|(t, s)| ClipRef::slot(*t, *s))
            .collect()
    } else {
        vec![shown]
    }
}

/// The filled slots of the current selection.
pub fn selected_slots(session: &Session, state: &UiState) -> Vec<(TrackId, usize)> {
    let mut slots = state.sel.slots.clone();
    if let Some(one) = state.selected_clip.and_then(ClipRef::as_slot) {
        if !slots.contains(&one) {
            slots.push(one);
        }
    }
    slots.retain(|(t, s)| session.clip(*t, *s).is_some());
    slots
}

/// Selects one clip, as clicking it does.
pub fn select_one(state: &mut UiState, track: TrackId, slot: usize) {
    state.sel.clear();
    state.sel.slots = vec![(track, slot)];
    state.sel.anchor = Some((track, slot));
    state.selected_clip = Some(ClipRef::slot(track, slot));
}

/// Applies a click on a slot, with Shift extending and Cmd toggling.
pub fn click_slot(
    session: &Session,
    state: &mut UiState,
    track: TrackId,
    slot: usize,
    shift: bool,
    cmd: bool,
) {
    state.sel.scenes.clear();
    state.sel.scene_anchor = None;
    if cmd {
        if let Some(i) = state.sel.slots.iter().position(|s| *s == (track, slot)) {
            state.sel.slots.remove(i);
        } else {
            state.sel.slots.push((track, slot));
        }
        state.selected_clip = Some(ClipRef::slot(track, slot));
        return;
    }
    if shift {
        if let Some(anchor) = state.sel.anchor {
            state.sel.slots = rectangle(session, anchor, (track, slot));
            state.selected_clip = Some(ClipRef::slot(track, slot));
            return;
        }
    }
    select_one(state, track, slot);
}

/// Every slot in the rectangle between two.
fn rectangle(session: &Session, a: (TrackId, usize), b: (TrackId, usize)) -> Vec<(TrackId, usize)> {
    let index = |t: TrackId| session.project.track_index(t).unwrap_or(0);
    let (i, j) = (index(a.0), index(b.0));
    let (lo_t, hi_t) = (i.min(j), i.max(j));
    let (lo_s, hi_s) = (a.1.min(b.1), a.1.max(b.1));
    let mut out = Vec::new();
    for t in lo_t..=hi_t {
        let id = session.project.tracks[t].id;
        for s in lo_s..=hi_s {
            out.push((id, s));
        }
    }
    out
}

/// Applies a click on a scene, with Shift extending and Cmd toggling.
pub fn click_scene(state: &mut UiState, scene: usize, shift: bool, cmd: bool) {
    state.sel.slots.clear();
    state.sel.anchor = None;
    state.selected_clip = None;
    state.sel.scene_field = None;
    let sel = &mut state.sel;
    if cmd {
        match sel.scenes.iter().position(|s| *s == scene) {
            Some(i) => {
                sel.scenes.remove(i);
            }
            None => sel.scenes.push(scene),
        }
    } else if let Some(anchor) = sel.scene_anchor.filter(|_| shift) {
        sel.scenes = (anchor.min(scene)..=anchor.max(scene)).collect();
    } else {
        sel.scenes = vec![scene];
        sel.scene_anchor = Some(scene);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use rlive_engine::create_engine;
    use rlive_plugin_host::scanner::PluginCache;
    use rlive_project::Project;

    fn session() -> (rlive_engine::Engine, Session) {
        let (engine, handle) = create_engine(48_000.0, 512);
        (
            engine,
            Session::new(handle, Project::new_default(), PluginCache::default()),
        )
    }

    #[test]
    fn shift_clicking_selects_the_rectangle() {
        let (_e, s) = session();
        let (a, b) = (s.project.tracks[0].id, s.project.tracks[1].id);
        let r = rectangle(&s, (a, 2), (b, 3));
        assert_eq!(r, vec![(a, 2), (a, 3), (b, 2), (b, 3)]);
    }
}
