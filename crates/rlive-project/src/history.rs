//! Undo history: snapshots of everything undo covers.
//!
//! A snapshot leaves out plugin state blobs and view state ([`Track::arm`], [`TrackUi`], a
//! device's `expanded`), which are kept where they are and never compared, so a plugin reporting
//! a slightly different blob never creates a step. Everything else in a project is covered,
//! including settings no code knows how to restore yet: each level of the tree is captured as a
//! "shell", a copy of the type with its children emptied.
//!
//! Unchanged data is shared between snapshots at every level down to a single device or clip, so
//! a step costs roughly what it changed.
//!
//! The shells are written out field by field so that taking one never copies a track's notes.
//! Adding a field is a compile error here, which is the prompt to decide if undo should cover it.

use crate::{
    Arrangement, ArrangementClip, Chain, Clip, ClipContent, Device, DeviceKind, Envelope, Project,
    Scene, TakeLane, Track, TrackUi,
};
use rlive_core::{DeviceId, TrackId};
use std::collections::{HashMap, HashSet};
use std::mem::{size_of, size_of_val};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_STEPS: usize = 200;
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// One chain of a rack in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainSnap {
    /// The chain with no devices in it.
    pub shell: Chain,
    pub devices: Vec<Arc<DeviceSnap>>,
}

/// A device and everything nested in it, in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSnap {
    /// The device with no plugin state, no chains and `expanded` set.
    pub shell: Device,
    /// A rack's chains; empty for anything else.
    pub chains: Vec<ChainSnap>,
    /// A rack's return chains; empty for anything else.
    pub returns: Vec<ChainSnap>,
}

/// A track in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSnap {
    /// The track with no devices or clips, and with its view state and arm reset.
    pub shell: Track,
    pub devices: Vec<Arc<DeviceSnap>>,
    pub clips: Vec<Option<Arc<Clip>>>,
    pub lane: Vec<Arc<ArrangementClip>>,
    pub automation: Arc<Vec<Envelope>>,
    pub take_lanes: Arc<Vec<TakeLane>>,
}

/// The state of a project.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The project with no tracks or scenes: tempo, signature, grooves, mappings, and so on.
    pub shell: Arc<Project>,
    pub scenes: Arc<Vec<Scene>>,
    pub tracks: Vec<Arc<TrackSnap>>,
    pub returns: Vec<Arc<TrackSnap>>,
    pub master: Arc<TrackSnap>,
    pub arrangement: Arc<Arrangement>,
}

/// What undo must keep of a track it doesn't cover, for when it comes back.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedTrack {
    pub arm: bool,
    pub ui: TrackUi,
}

impl ParkedTrack {
    pub fn of(t: &Track) -> Self {
        Self {
            arm: t.arm,
            ui: t.ui.clone(),
        }
    }
}

/// What undo must keep of a device it doesn't cover, for when it comes back.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedDevice {
    pub plugin_state: Option<Vec<u8>>,
    pub expanded: bool,
}

impl ParkedDevice {
    pub fn of(d: &Device) -> Self {
        let plugin_state = match &d.kind {
            DeviceKind::Plugin { state, .. } => state.clone(),
            _ => None,
        };
        Self {
            plugin_state,
            expanded: d.expanded,
        }
    }
}

fn clip_bytes(c: &Clip) -> usize {
    size_of::<Clip>() + size_of_val(c.notes()) + c.name.len()
}

/// Rough size of a device in a snapshot, not counting what it shares with its children.
fn device_bytes(s: &DeviceSnap) -> usize {
    size_of::<DeviceSnap>()
        + s.shell.params.len() * (size_of::<u32>() + size_of::<f64>())
        + s.shell.name.len()
        + (s.chains.len() + s.returns.len()) * size_of::<ChainSnap>()
}

fn track_bytes(s: &TrackSnap) -> usize {
    size_of::<TrackSnap>()
        + (s.devices.len() + s.clips.len() + s.lane.len()) * size_of::<usize>()
        + s.shell.name.len()
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

/// `d` without its children, its plugin blob or its view state.
fn device_shell(d: &Device) -> Device {
    let kind = match &d.kind {
        DeviceKind::Builtin {
            type_id,
            samples,
            state,
        } => DeviceKind::Builtin {
            type_id: type_id.clone(),
            samples: samples.clone(),
            state: state.clone(),
        },
        DeviceKind::Plugin {
            format,
            plugin_id,
            path,
            ..
        } => DeviceKind::Plugin {
            format: *format,
            plugin_id: plugin_id.clone(),
            path: path.clone(),
            state: None,
        },
        DeviceKind::Rack {
            kind,
            macros,
            visible_macros,
            select_filters_ctrl,
            variations,
            ..
        } => DeviceKind::Rack {
            kind: *kind,
            chains: Vec::new(),
            returns: Vec::new(),
            macros: macros.clone(),
            visible_macros: *visible_macros,
            select_filters_ctrl: *select_filters_ctrl,
            variations: variations.clone(),
        },
    };
    Device {
        id: d.id,
        name: d.name.clone(),
        kind,
        bypass: d.bypass,
        params: d.params.clone(),
        expanded: true,
        info: d.info.clone(),
        sidechain: d.sidechain,
    }
}

/// `c` without its devices.
fn chain_shell(c: &Chain) -> Chain {
    Chain {
        id: c.id,
        name: c.name.clone(),
        devices: Vec::new(),
        zones: c.zones,
        mix: c.mix.clone(),
        color: c.color,
        info: c.info.clone(),
    }
}

fn capture_chains(
    chains: &[Chain],
    prev: Option<&[ChainSnap]>,
    fresh: &mut usize,
) -> Vec<ChainSnap> {
    chains
        .iter()
        .map(|c| {
            let p = prev.and_then(|p| p.iter().find(|o| o.shell.id == c.id));
            ChainSnap {
                shell: chain_shell(c),
                devices: capture_devices(&c.devices, p.map(|p| p.devices.as_slice()), fresh),
            }
        })
        .collect()
}

fn capture_devices(
    devices: &[Device],
    prev: Option<&[Arc<DeviceSnap>]>,
    fresh: &mut usize,
) -> Vec<Arc<DeviceSnap>> {
    devices
        .iter()
        .map(|d| {
            let p = prev.and_then(|p| p.iter().find(|o| o.shell.id == d.id));
            capture_device(d, p, fresh)
        })
        .collect()
}

fn capture_device(
    d: &Device,
    prev: Option<&Arc<DeviceSnap>>,
    fresh: &mut usize,
) -> Arc<DeviceSnap> {
    let (chains, returns) = match &d.kind {
        DeviceKind::Rack {
            chains, returns, ..
        } => (
            capture_chains(chains, prev.map(|p| p.chains.as_slice()), fresh),
            capture_chains(returns, prev.map(|p| p.returns.as_slice()), fresh),
        ),
        _ => Default::default(),
    };
    let snap = DeviceSnap {
        shell: device_shell(d),
        chains,
        returns,
    };
    share(prev, &snap, device_bytes, fresh)
}

/// `t` without what is shared separately, and with its arm and view state reset.
fn track_shell(t: &Track) -> Track {
    Track {
        id: t.id,
        name: t.name.clone(),
        color: t.color,
        kind: t.kind,
        volume_db: t.volume_db,
        pan: t.pan,
        mute: t.mute,
        solo: t.solo,
        arm: false,
        devices: Vec::new(),
        clips: Vec::new(),
        no_stop: t.no_stop.clone(),
        arrangement: Vec::new(),
        group: t.group,
        sends: t.sends.clone(),
        routing: t.routing.clone(),
        crossfade: t.crossfade,
        pre_fader_sends: t.pre_fader_sends,
        pan_mode: t.pan_mode,
        delay: t.delay,
        bypass_tuning: t.bypass_tuning,
        automation: Vec::new(),
        take_lanes: Vec::new(),
        keep_monitor_latency: t.keep_monitor_latency,
        freeze: t.freeze,
        info: t.info.clone(),
        ui: TrackUi::default(),
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
        shell: track_shell(t),
        devices: capture_devices(&t.devices, prev.map(|p| p.devices.as_slice()), fresh),
        clips,
        lane,
        automation: share(
            prev.map(|p| &p.automation),
            &t.automation,
            |a| a.iter().map(|e| size_of_val(e.points.as_slice())).sum(),
            fresh,
        ),
        take_lanes: share(
            prev.map(|p| &p.take_lanes),
            &t.take_lanes,
            |l| {
                l.iter()
                    .map(|l| l.clips.len() * size_of::<ArrangementClip>())
                    .sum()
            },
            fresh,
        ),
    };
    share(prev, &snap, track_bytes, fresh)
}

fn capture_tracks(
    tracks: &[Track],
    prev: Option<&[Arc<TrackSnap>]>,
    fresh: &mut usize,
) -> Vec<Arc<TrackSnap>> {
    tracks
        .iter()
        .map(|t| {
            let p = prev.and_then(|p| p.iter().find(|o| o.shell.id == t.id));
            capture_track(t, p, fresh)
        })
        .collect()
}

/// Records `project`, sharing whatever is unchanged since `prev`. Returns the
/// snapshot and roughly how many bytes it newly allocated.
pub fn capture(project: &Project, prev: Option<&Snapshot>) -> (Snapshot, usize) {
    let mut fresh = 0;
    let shell = Project {
        version: project.version,
        tempo: project.tempo,
        time_signature: project.time_signature,
        quantize: project.quantize,
        metronome: project.metronome,
        scenes: Vec::new(),
        tracks: Vec::new(),
        master: track_shell(&project.master),
        arrangement: Arrangement::default(),
        returns: Vec::new(),
        crossfader: project.crossfader,
        cue: project.cue.clone(),
        groove_pool: project.groove_pool.clone(),
        tuning: project.tuning.clone(),
        midi_mappings: project.midi_mappings.clone(),
        key_mappings: project.key_mappings.clone(),
        surface_locks: project.surface_locks.clone(),
        solo_in_place: project.solo_in_place,
        delay_compensation: project.delay_compensation,
        follow_actions_on: project.follow_actions_on,
        reduced_latency_when_monitoring: project.reduced_latency_when_monitoring,
        metronome_options: project.metronome_options,
        record_quantize: project.record_quantize,
        midi_arrangement_overdub: project.midi_arrangement_overdub,
    };
    let snapshot = Snapshot {
        shell: share(
            prev.map(|p| &p.shell),
            &shell,
            |_| size_of::<Project>(),
            &mut fresh,
        ),
        scenes: share(
            prev.map(|p| &p.scenes),
            &project.scenes,
            |s| s.len() * size_of::<Scene>(),
            &mut fresh,
        ),
        tracks: capture_tracks(
            &project.tracks,
            prev.map(|p| p.tracks.as_slice()),
            &mut fresh,
        ),
        returns: capture_tracks(
            &project.returns,
            prev.map(|p| p.returns.as_slice()),
            &mut fresh,
        ),
        master: capture_track(&project.master, prev.map(|p| &p.master), &mut fresh),
        arrangement: share(
            prev.map(|p| &p.arrangement),
            &project.arrangement,
            |_| size_of::<Arrangement>(),
            &mut fresh,
        ),
    };
    (snapshot, fresh)
}

impl DeviceSnap {
    /// Rebuilds the device, taking what undo doesn't cover from `kept`.
    pub fn to_device(&self, kept: &impl Fn(DeviceId) -> Option<ParkedDevice>) -> Device {
        let mut d = self.shell.clone();
        let parked = kept(d.id);
        if let Some(p) = &parked {
            d.expanded = p.expanded;
        }
        match &mut d.kind {
            DeviceKind::Plugin { state, .. } => *state = parked.and_then(|p| p.plugin_state),
            DeviceKind::Rack {
                chains, returns, ..
            } => {
                *chains = self.chains.iter().map(|c| c.to_chain(kept)).collect();
                *returns = self.returns.iter().map(|c| c.to_chain(kept)).collect();
            }
            DeviceKind::Builtin { .. } => {}
        }
        d
    }

    /// Calls `f` with this device's id and those of everything nested in it.
    fn for_each_id(&self, f: &mut impl FnMut(DeviceId)) {
        f(self.shell.id);
        for c in self.chains.iter().chain(&self.returns) {
            for d in &c.devices {
                d.for_each_id(f);
            }
        }
    }
}

impl ChainSnap {
    pub fn to_chain(&self, kept: &impl Fn(DeviceId) -> Option<ParkedDevice>) -> Chain {
        Chain {
            devices: self.devices.iter().map(|d| d.to_device(kept)).collect(),
            ..self.shell.clone()
        }
    }
}

impl TrackSnap {
    /// Rebuilds the track, taking what undo doesn't cover from `kept` and `parked`.
    pub fn to_track(
        &self,
        kept: &impl Fn(DeviceId) -> Option<ParkedDevice>,
        parked: Option<&ParkedTrack>,
    ) -> Track {
        let mut t = self.shell.clone();
        if let Some(p) = parked {
            t.arm = p.arm;
            t.ui = p.ui.clone();
        }
        t.devices = self.devices.iter().map(|d| d.to_device(kept)).collect();
        t.clips = self.clips.iter().map(|c| c.as_deref().cloned()).collect();
        t.arrangement = self.lane.iter().map(|c| (**c).clone()).collect();
        t.automation = (*self.automation).clone();
        t.take_lanes = (*self.take_lanes).clone();
        t
    }
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
    /// What undo must restore about devices and tracks no longer in the project.
    pub parked_devices: HashMap<DeviceId, ParkedDevice>,
    pub parked_tracks: HashMap<TrackId, ParkedTrack>,
    /// An edit was made since the last commit; holds its description.
    pub pending: Option<&'static str>,
    /// The user was mid-gesture, so direct edits may be waiting to commit.
    pub was_busy: bool,
    /// Frames left to wait for a device to stop reporting parameter changes before they become a
    /// step. A plugin's editor has no gesture this app can see, only a stream of changes.
    pub settling: u32,
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
            parked_tracks: HashMap::new(),
            pending: None,
            was_busy: false,
            settling: 0,
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
            let all = s
                .snapshot
                .tracks
                .iter()
                .chain(&s.snapshot.returns)
                .chain([&s.snapshot.master]);
            for t in all {
                tracks.insert(t.shell.id);
                for d in &t.devices {
                    d.for_each_id(&mut |id| {
                        devices.insert(id);
                    });
                }
            }
        }
        self.parked_devices.retain(|id, _| devices.contains(id));
        self.parked_tracks.retain(|id, _| tracks.contains(id));
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

    /// Points every step at where its files are now, after some of them moved on disk, so an undo
    /// across the move still finds them.
    pub fn move_files(&mut self, moved: &HashMap<PathBuf, PathBuf>) {
        if moved.is_empty() {
            return;
        }
        for step in &mut self.steps {
            let mut snapshot = (*step.snapshot).clone();
            let mut changed = false;
            for t in snapshot
                .tracks
                .iter_mut()
                .chain(snapshot.returns.iter_mut())
            {
                if let Some(next) = moved_track(t, moved) {
                    *t = Arc::new(next);
                    changed = true;
                }
            }
            if changed {
                step.snapshot = Arc::new(snapshot);
            }
        }
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.current().clone());
        self.untracked_dirty = false;
    }

    pub fn at_saved(&self) -> bool {
        self.saved.as_ref().is_some_and(|s| **s == **self.current())
    }
}

fn moved_clip(c: &Clip, moved: &HashMap<PathBuf, PathBuf>) -> Option<Clip> {
    let content = match &c.content {
        ClipContent::Audio { file, .. } => moved.get(&file.path),
        ClipContent::Midi { .. } => None,
    };
    let frozen = c.frozen.as_ref().and_then(|f| moved.get(&f.file.path));
    if content.is_none() && frozen.is_none() {
        return None;
    }
    let mut next = c.clone();
    if let (ClipContent::Audio { file, .. }, Some(to)) = (&mut next.content, content) {
        file.path = to.clone();
    }
    if let (Some(f), Some(to)) = (&mut next.frozen, frozen) {
        f.file.path = to.clone();
    }
    Some(next)
}

fn moved_lane(
    lane: &[Arc<ArrangementClip>],
    moved: &HashMap<PathBuf, PathBuf>,
) -> Option<Vec<Arc<ArrangementClip>>> {
    let mut changed = false;
    let next = lane
        .iter()
        .map(|c| match moved_clip(&c.clip, moved) {
            Some(clip) => {
                changed = true;
                Arc::new(ArrangementClip {
                    clip,
                    ..(**c).clone()
                })
            }
            None => c.clone(),
        })
        .collect();
    changed.then_some(next)
}

fn moved_track(t: &TrackSnap, moved: &HashMap<PathBuf, PathBuf>) -> Option<TrackSnap> {
    let mut next = t.clone();
    let mut changed = false;
    for c in next.clips.iter_mut().flatten() {
        if let Some(clip) = moved_clip(c, moved) {
            *c = Arc::new(clip);
            changed = true;
        }
    }
    if let Some(lane) = moved_lane(&next.lane, moved) {
        next.lane = lane;
        changed = true;
    }
    let mut lanes = (*next.take_lanes).clone();
    let mut lanes_changed = false;
    for l in &mut lanes {
        for c in &mut l.clips {
            if let Some(clip) = moved_clip(&c.clip, moved) {
                c.clip = clip;
                lanes_changed = true;
            }
        }
    }
    if lanes_changed {
        next.take_lanes = Arc::new(lanes);
        changed = true;
    }
    changed.then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceKind, RackKind, PALETTE};
    use rlive_core::{PluginFormat, TimeSignature};

    fn project() -> Project {
        let mut p = Project::new_default();
        p.tracks[1].clips[0] = Some(Clip::new_midi("B", PALETTE[1], 1, TimeSignature::default()));
        p
    }

    #[test]
    fn moving_a_file_reaches_every_step() {
        let mut p = project();
        let old = PathBuf::from("/tmp/a.wav");
        let clip = Clip {
            length: 960,
            content: ClipContent::audio(&old, 0.0),
            ..Clip::default()
        };
        p.tracks[2].clips[0] = Some(clip);
        let mut h = History::new(&p);
        p.tracks[0].clips[0].as_mut().unwrap().name = "edited".into();
        assert!(h.commit(&p, "Edit"));
        let new = PathBuf::from("/tmp/b.wav");
        h.move_files(&HashMap::from([(old.clone(), new.clone())]));
        for step in &h.steps {
            let c = step.snapshot.tracks[2].clips[0].as_ref().unwrap();
            assert!(matches!(&c.content, ClipContent::Audio { file, .. } if file.path == new));
        }
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
    fn mix_and_tempo_changes_make_a_step() {
        let mut p = project();
        let mut h = History::new(&p);
        p.tempo = 90.0;
        p.tracks[0].volume_db = -6.0;
        assert!(h.commit(&p, "Mix"));
    }

    #[test]
    fn a_parameter_change_shares_the_other_devices() {
        let mut p = project();
        let mut h = History::new(&p);
        p.tracks[1].devices[0].params.insert(3, 0.5);
        assert!(h.commit(&p, "Knob"));
        let (a, b) = (&h.steps[0].snapshot, &h.steps[1].snapshot);
        assert!(!Arc::ptr_eq(
            &a.tracks[1].devices[0],
            &b.tracks[1].devices[0]
        ));
        assert!(Arc::ptr_eq(
            &a.tracks[1].devices[1],
            &b.tracks[1].devices[1]
        ));
        assert!(Arc::ptr_eq(
            &a.tracks[0].devices[0],
            &b.tracks[0].devices[0]
        ));
        assert_eq!(b.tracks[1].devices[0].shell.params[&3], 0.5);
    }

    fn rack_project() -> Project {
        let mut p = project();
        let mut rack = Device::rack(RackKind::Instrument, "Rack");
        let mut chain = Chain::new("A");
        chain.devices.push(Device::builtin("rlive.poly", "Inner"));
        chain
            .devices
            .push(Device::builtin("rlive.delay", "Inner FX"));
        if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
            chains.push(chain);
        }
        p.tracks[0].devices.push(rack);
        p
    }

    fn inner_devices(p: &mut Project) -> &mut Vec<Device> {
        let DeviceKind::Rack { chains, .. } = &mut p.tracks[0].devices[1].kind else {
            panic!("not a rack");
        };
        &mut chains[0].devices
    }

    #[test]
    fn nested_devices_are_shared_individually() {
        let mut p = rack_project();
        let mut h = History::new(&p);
        inner_devices(&mut p)[1].params.insert(0, 0.25);
        assert!(h.commit(&p, "Nested knob"));
        let (a, b) = (&h.steps[0].snapshot, &h.steps[1].snapshot);
        let (ca, cb) = (
            &a.tracks[0].devices[1].chains[0],
            &b.tracks[0].devices[1].chains[0],
        );
        assert!(Arc::ptr_eq(&ca.devices[0], &cb.devices[0]));
        assert!(!Arc::ptr_eq(&ca.devices[1], &cb.devices[1]));
        assert!(!Arc::ptr_eq(
            &a.tracks[0].devices[1],
            &b.tracks[0].devices[1]
        ));
        assert!(Arc::ptr_eq(
            &a.tracks[0].devices[0],
            &b.tracks[0].devices[0]
        ));
    }

    #[test]
    fn a_routing_change_makes_a_step_and_survives_the_round_trip() {
        let mut p = project();
        let mut h = History::new(&p);
        p.tracks[0].routing.monitor = crate::Monitor::In;
        assert!(h.commit(&p, "Monitor"));
        h.undo_target();
        let snap = h.current().tracks[0].to_track(&|_| None, None);
        assert_eq!(snap.routing.monitor, crate::Monitor::Auto);
    }

    #[test]
    fn plugin_state_and_view_state_are_not_compared() {
        let mut p = project();
        let mut plug = Device::plugin(PluginFormat::Clap, "x", "/x".into(), "Plug");
        if let DeviceKind::Plugin { state, .. } = &mut plug.kind {
            *state = Some(vec![1, 2, 3]);
        }
        p.tracks[0].devices.push(plug);
        let mut h = History::new(&p);
        if let DeviceKind::Plugin { state, .. } = &mut p.tracks[0].devices[1].kind {
            *state = Some(vec![9; 100]);
        }
        p.tracks[0].devices[1].expanded = false;
        p.tracks[0].arm = true;
        p.tracks[0].ui.folded = true;
        assert!(!h.commit(&p, "Nothing undoable"));
    }

    #[test]
    fn rebuilding_a_device_takes_blobs_from_what_was_parked() {
        let mut p = rack_project();
        let mut plug = Device::plugin(PluginFormat::Clap, "x", "/x".into(), "Plug");
        let plug_id = plug.id;
        if let DeviceKind::Plugin { state, .. } = &mut plug.kind {
            *state = Some(vec![4, 5, 6]);
        }
        plug.expanded = false;
        inner_devices(&mut p).push(plug.clone());
        let snap = capture(&p, None).0;
        let kept = |id| (id == plug_id).then(|| ParkedDevice::of(&plug));
        let rebuilt = snap.tracks[0].devices[1].to_device(&kept);
        let DeviceKind::Rack { chains, .. } = &rebuilt.kind else {
            panic!("not a rack");
        };
        let DeviceKind::Plugin { state, .. } = &chains[0].devices[2].kind else {
            panic!("not a plugin");
        };
        assert_eq!(state.as_deref(), Some(&[4u8, 5, 6][..]));
        assert!(!chains[0].devices[2].expanded);
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
