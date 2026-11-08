//! Undo history: snapshots of the parts of a project that undo covers.
//!
//! Snapshots leave out device contents, mix settings and tempo, so moving a
//! knob never creates a step and undo never reverts one. Unchanged clips are
//! shared between snapshots, so a step costs roughly what it changed.

use crate::{Arrangement, ArrangementClip, Clip, Device, Project, Scene, Track, TrackKind};
use rlive_core::{DeviceId, TrackId};
use std::collections::{HashMap, HashSet};
use std::mem::{size_of, size_of_val};
use std::sync::Arc;

const MAX_STEPS: usize = 200;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackSnap {
    pub id: TrackId,
    pub name: String,
    pub color: [u8; 3],
    pub kind: TrackKind,
    pub device_ids: Vec<DeviceId>,
    pub clips: Vec<Option<Arc<Clip>>>,
    pub lane: Vec<Arc<ArrangementClip>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub scenes: Arc<Vec<Scene>>,
    pub tracks: Vec<Arc<TrackSnap>>,
    pub master_devices: Vec<DeviceId>,
    pub arrangement: Arrangement,
}

/// Track settings that undo doesn't cover but a re-created track must keep.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedMix {
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
}

impl ParkedMix {
    pub fn of(t: &Track) -> Self {
        Self {
            volume_db: t.volume_db,
            pan: t.pan,
            mute: t.mute,
            solo: t.solo,
            arm: t.arm,
        }
    }
}

fn clip_bytes(c: &Clip) -> usize {
    size_of::<Clip>() + size_of_val(c.notes()) + c.name.len()
}

/// Returns `prev` if it already holds `value`, otherwise a new `Arc`, adding its size to `fresh`.
fn share<T: PartialEq + Clone>(
    prev: Option<&Arc<T>>,
    value: &T,
    bytes: impl FnOnce(&T) -> usize,
    fresh: &mut usize,
) -> Arc<T> {
    match prev {
        Some(a) if **a == *value => a.clone(),
        _ => {
            *fresh += bytes(value);
            Arc::new(value.clone())
        }
    }
}

fn capture_track(t: &Track, prev: Option<&Arc<TrackSnap>>, fresh: &mut usize) -> Arc<TrackSnap> {
    let clips = t
        .clips
        .iter()
        .enumerate()
        .map(|(i, c)| {
            c.as_ref().map(|c| {
                let p = prev.and_then(|p| p.clips.get(i)).and_then(Option::as_ref);
                share(p, c, clip_bytes, fresh)
            })
        })
        .collect();
    let lane = t
        .arrangement
        .iter()
        .map(|c| {
            let p = prev.and_then(|p| p.lane.iter().find(|o| o.id == c.id));
            share(p, c, |c| clip_bytes(&c.clip), fresh)
        })
        .collect();
    let snap = TrackSnap {
        id: t.id,
        name: t.name.clone(),
        color: t.color,
        kind: t.kind,
        device_ids: t.devices.iter().map(|d| d.id).collect(),
        clips,
        lane,
    };
    match prev {
        Some(p) if **p == snap => p.clone(),
        _ => Arc::new(snap),
    }
}

/// Records `project`, sharing whatever is unchanged since `prev`. Returns the
/// snapshot and roughly how many bytes it newly allocated.
pub fn capture(project: &Project, prev: Option<&Snapshot>) -> (Snapshot, usize) {
    let mut fresh = 0;
    let tracks = project
        .tracks
        .iter()
        .map(|t| {
            let p = prev.and_then(|p| p.tracks.iter().find(|o| o.id == t.id));
            capture_track(t, p, &mut fresh)
        })
        .collect();
    let scenes = share(
        prev.map(|p| &p.scenes),
        &project.scenes,
        |s| s.len() * size_of::<Scene>(),
        &mut fresh,
    );
    let snapshot = Snapshot {
        scenes,
        tracks,
        master_devices: project.master.devices.iter().map(|d| d.id).collect(),
        arrangement: project.arrangement.clone(),
    };
    (snapshot, fresh)
}

/// A snapshot, the bytes it added, and the label of the edit that produced it.
struct Step {
    snapshot: Arc<Snapshot>,
    bytes: usize,
    label: &'static str,
}

pub struct History {
    steps: Vec<Step>,
    cursor: usize,
    saved: Option<Arc<Snapshot>>,
    /// Devices and mixes of things no longer in the project that undo may need back.
    pub parked_devices: HashMap<DeviceId, Device>,
    pub parked_mix: HashMap<TrackId, ParkedMix>,
    /// An edit was made since the last commit; holds its description.
    pub pending: Option<&'static str>,
    /// The user was mid-gesture, so direct edits may be waiting to commit.
    pub was_busy: bool,
    /// Something outside undo's coverage changed since the last save.
    pub untracked_dirty: bool,
}

impl History {
    /// A history whose starting point counts as saved.
    pub fn new(project: &Project) -> Self {
        let (snapshot, bytes) = capture(project, None);
        let snapshot = Arc::new(snapshot);
        Self {
            steps: vec![Step {
                snapshot: snapshot.clone(),
                bytes,
                label: "",
            }],
            cursor: 0,
            saved: Some(snapshot),
            parked_devices: HashMap::new(),
            parked_mix: HashMap::new(),
            pending: None,
            was_busy: false,
            untracked_dirty: false,
        }
    }

    fn current(&self) -> &Arc<Snapshot> {
        &self.steps[self.cursor].snapshot
    }

    /// Adds a step if `project` differs from the current one. Returns whether it did.
    pub fn commit(&mut self, project: &Project, label: &'static str) -> bool {
        let (snapshot, bytes) = capture(project, Some(self.current()));
        if snapshot == **self.current() {
            return false;
        }
        self.steps.truncate(self.cursor + 1);
        self.steps.push(Step {
            snapshot: Arc::new(snapshot),
            bytes,
            label,
        });
        self.cursor += 1;
        self.evict();
        true
    }

    fn evict(&mut self) {
        let mut total: usize = self.steps.iter().map(|s| s.bytes).sum();
        let mut removed = false;
        while self.cursor > 0 && (self.steps.len() > MAX_STEPS || total > MAX_BYTES) {
            total -= self.steps.remove(0).bytes;
            self.cursor -= 1;
            removed = true;
        }
        if removed {
            self.collect_parked();
        }
    }

    /// Forgets parked items that no remaining step refers to.
    fn collect_parked(&mut self) {
        let mut devices = HashSet::new();
        let mut tracks = HashSet::new();
        for s in &self.steps {
            devices.extend(s.snapshot.master_devices.iter().copied());
            for t in &s.snapshot.tracks {
                tracks.insert(t.id);
                devices.extend(t.device_ids.iter().copied());
            }
        }
        self.parked_devices.retain(|id, _| devices.contains(id));
        self.parked_mix.retain(|id, _| tracks.contains(id));
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        (self.cursor > 0).then(|| self.steps[self.cursor].label)
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.steps.get(self.cursor + 1).map(|s| s.label)
    }

    /// Steps back and returns the state to restore.
    pub fn undo_target(&mut self) -> Option<Arc<Snapshot>> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(self.current().clone())
    }

    /// Steps forward and returns the state to restore.
    pub fn redo_target(&mut self) -> Option<Arc<Snapshot>> {
        if self.cursor + 1 >= self.steps.len() {
            return None;
        }
        self.cursor += 1;
        Some(self.current().clone())
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.current().clone());
        self.untracked_dirty = false;
    }

    pub fn at_saved(&self) -> bool {
        self.saved.as_ref().is_some_and(|s| **s == **self.current())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PALETTE;
    use rlive_core::TimeSignature;

    fn project() -> Project {
        let mut p = Project::new_default();
        p.tracks[1].clips[0] = Some(Clip::new_midi("B", PALETTE[1], 1, TimeSignature::default()));
        p
    }

    #[test]
    fn untouched_clips_are_shared_between_steps() {
        let mut p = project();
        let mut h = History::new(&p);
        p.tracks[0].clips[0].as_mut().unwrap().name = "edited".into();
        assert!(h.commit(&p, "Edit"));
        let (a, b) = (&h.steps[0].snapshot, &h.steps[1].snapshot);
        let (x, y) = (
            a.tracks[1].clips[0].as_ref().unwrap(),
            b.tracks[1].clips[0].as_ref().unwrap(),
        );
        assert!(Arc::ptr_eq(x, y));
        assert!(Arc::ptr_eq(&a.tracks[2], &b.tracks[2]));
    }

    #[test]
    fn identical_state_makes_no_step() {
        let p = project();
        let mut h = History::new(&p);
        assert!(!h.commit(&p, "Nothing"));
        assert!(h.undo_label().is_none());
    }

    #[test]
    fn mix_and_tempo_changes_make_no_step() {
        let mut p = project();
        let mut h = History::new(&p);
        p.tempo = 90.0;
        p.tracks[0].volume_db = -6.0;
        assert!(!h.commit(&p, "Mix"));
    }

    #[test]
    fn a_new_edit_discards_redo() {
        let mut p = project();
        let mut h = History::new(&p);
        p.tracks[0].name = "a".into();
        h.commit(&p, "A");
        h.undo_target();
        assert_eq!(h.redo_label(), Some("A"));
        p.tracks[0].name = "b".into();
        h.commit(&p, "B");
        assert_eq!(h.redo_label(), None);
        assert_eq!(h.undo_label(), Some("B"));
    }

    #[test]
    fn saved_state_is_recognised_after_undo() {
        let mut p = project();
        let mut h = History::new(&p);
        assert!(h.at_saved());
        p.tracks[0].name = "a".into();
        h.commit(&p, "A");
        assert!(!h.at_saved());
        h.undo_target();
        assert!(h.at_saved());
    }

    #[test]
    fn step_count_is_capped() {
        let mut p = project();
        let mut h = History::new(&p);
        for i in 0..(MAX_STEPS + 20) {
            p.tracks[0].name = i.to_string();
            h.commit(&p, "A");
        }
        assert_eq!(h.steps.len(), MAX_STEPS);
    }
}
