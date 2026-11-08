//! The In/Out section: what a track listens to and where it plays, what the choosers offer, the
//! crossfader and cueing.

use super::graph::{audio_outputs, deps_without, Side};
use super::{ChainRef, PluginWiring, Session};
use rlive_core::{CrossfadeCurve, DeviceId, TrackId};
use rlive_engine::{mix_cell, EngineCommand, PreviewCmd};
use rlive_plugin_host::PluginFormat;
use rlive_project::{
    ChainTap, Crossfade, Device, DeviceKind, ExternalRoute, Monitor, Project, Route,
    SidechainSource, Tap, Track, TrackKind,
};
use std::path::Path;

/// One line of a chooser in the In/Out section.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteChoice {
    pub label: String,
    pub route: Route,
    pub tap: Tap,
}

impl RouteChoice {
    fn of(label: impl Into<String>, route: Route) -> Self {
        Self {
            label: label.into(),
            route,
            tap: Tap::default(),
        }
    }

    fn tapping(label: impl Into<String>, route: Route, tap: Tap) -> Self {
        Self {
            label: label.into(),
            route,
            tap,
        }
    }
}

fn track_route(t: &Track) -> Route {
    Route::Track {
        track: t.id,
        device: None,
    }
}

/// The audio hardware the choosers can address, as a list of channels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioHardware {
    pub outputs: Vec<(String, ExternalRoute)>,
    pub inputs: Vec<(String, ExternalRoute)>,
}

/// A position the crossfader can be sent to by a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossfaderField {
    Left,
    Center,
    Right,
}

impl CrossfaderField {
    fn position(self) -> f32 {
        match self {
            Self::Left => -1.0,
            Self::Center => 0.0,
            Self::Right => 1.0,
        }
    }
}

/// How keys work the crossfader (manual p. 405). One key toggles between the two ends; holding
/// one key and pressing another jumps there and snaps back on release.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CrossfaderKeys {
    held: Vec<CrossfaderField>,
}

impl CrossfaderKeys {
    /// A key went down. Returns where the crossfader goes.
    pub fn pressed(&mut self, field: CrossfaderField, position: f32) -> f32 {
        let target = if self.held.iter().any(|f| *f != field) {
            field.position()
        } else {
            match field {
                CrossfaderField::Left if position > -1.0 => -1.0,
                CrossfaderField::Left => 1.0,
                CrossfaderField::Right if position < 1.0 => 1.0,
                CrossfaderField::Right => -1.0,
                CrossfaderField::Center if position <= 0.0 => 1.0,
                CrossfaderField::Center => -1.0,
            }
        };
        if !self.held.contains(&field) {
            self.held.push(field);
        }
        target
    }

    /// A key came up. Returns where the crossfader snaps back to, when another key is still held.
    pub fn released(&mut self, field: CrossfaderField) -> Option<f32> {
        self.held.retain(|f| *f != field);
        self.held.last().map(|f| f.position())
    }
}

impl Session {
    fn all_tracks(&self) -> impl Iterator<Item = &Track> {
        self.project.tracks.iter().chain(&self.project.returns)
    }

    fn engine_index(&self, id: TrackId) -> Option<usize> {
        self.all_tracks().position(|t| t.id == id)
    }

    fn apply_routing(&mut self, label: &'static str) {
        self.send_track_edits(Vec::new());
        self.mark_edit(label);
        self.refresh_wiring();
    }

    /// Whether a track's input or output can be set to a route without closing a loop.
    fn route_allowed(&self, id: TrackId, side: Side, route: &Route) -> bool {
        let Some(i) = self.engine_index(id) else {
            return false;
        };
        let audio = audio_outputs(&self.project, &self.timing);
        let target = match route {
            Route::Track { track, .. } | Route::TrackChannel { track, .. } => {
                self.engine_index(*track)
            }
            _ => None,
        };
        let Some(target) = target else {
            return true;
        };
        let deps = deps_without(&self.project, &audio, i, side);
        let (from, to) = match side {
            Side::Input => (target, i),
            Side::Output => (i, target),
        };
        !deps.closes_loop(from, to)
    }

    /// Sets what tracks listen to. A route that would close a loop is refused.
    pub fn set_input(&mut self, ids: &[TrackId], route: Route, tap: Tap) -> bool {
        let allowed = ids
            .iter()
            .all(|id| self.route_allowed(*id, Side::Input, &route));
        if !allowed {
            return false;
        }
        let no_input = route == Route::None;
        for id in ids {
            if let Some(t) = self.project.track_mut(*id) {
                t.routing.input = route.clone();
                t.routing.tap = tap;
                if no_input {
                    t.arm = false;
                }
            }
        }
        self.update_midi_targets();
        self.apply_routing("Change Input");
        true
    }

    /// Sets where tracks play. A route that would close a loop is refused.
    pub fn set_output(&mut self, ids: &[TrackId], route: Route) -> bool {
        let allowed = ids
            .iter()
            .all(|id| self.route_allowed(*id, Side::Output, &route));
        if !allowed {
            return false;
        }
        for id in ids {
            if let Some(t) = self.project.track_mut(*id) {
                t.routing.output = route.clone();
            }
        }
        self.apply_routing("Change Output");
        true
    }

    /// Sets the Monitor radio button of tracks.
    pub fn set_monitor(&mut self, ids: &[TrackId], monitor: Monitor) {
        for id in ids {
            if let Some(t) = self.project.track_mut(*id) {
                t.routing.monitor = monitor;
            }
        }
        self.apply_routing("Change Monitor");
    }

    /// Delete on a Monitor button: Off on audio tracks, Auto on MIDI tracks.
    pub fn reset_monitor(&mut self, ids: &[TrackId]) {
        for id in ids {
            if let Some(t) = self.project.track_mut(*id) {
                t.routing.monitor = Monitor::default_for(t.kind);
            }
        }
        self.apply_routing("Return to Default");
    }

    /// Sets a device's sidechain source from its own chooser.
    pub fn set_sidechain(
        &mut self,
        chain: ChainRef,
        device: DeviceId,
        source: Option<SidechainSource>,
    ) -> bool {
        if let Some(src) = source {
            let owner = match chain.owner() {
                ChainRef::Track(t) => t,
                _ => return false,
            };
            let audio = audio_outputs(&self.project, &self.timing);
            let (Some(i), Some(from)) = (self.engine_index(owner), self.engine_index(src.track))
            else {
                return false;
            };
            let deps = deps_without(&self.project, &audio, i, Side::Input);
            if deps.closes_loop(from, i) {
                return false;
            }
        }
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == device))
        {
            d.sidechain = source;
        }
        self.apply_routing("Change Sidechain");
        true
    }

    /// Plug-ins only get their extra buses switched on while something uses them. VST3 can only
    /// change this when re-instantiated.
    pub(super) fn refresh_wiring(&mut self) {
        let wanted = PluginWiring::of(&super::graph::compile_plan(&self.project));
        let mut changed: Vec<DeviceId> = wanted
            .sidechain
            .symmetric_difference(&self.wiring.sidechain)
            .chain(wanted.aux.symmetric_difference(&self.wiring.aux))
            .copied()
            .collect();
        changed.sort();
        changed.dedup();
        self.wiring = wanted;
        for id in changed {
            let is_vst3 = self.find_device_any(id).is_some_and(|d| {
                matches!(
                    d.kind,
                    DeviceKind::Plugin {
                        format: PluginFormat::Vst3,
                        ..
                    }
                )
            });
            if !is_vst3 {
                continue;
            }
            if let Some(chain) = self.chain_of_device(id) {
                self.reload_device(chain, id);
            }
        }
    }

    /// A MIDI track with no instrument: its output is MIDI, not audio.
    pub fn is_midi_only(&self, id: TrackId) -> bool {
        let audio = audio_outputs(&self.project, &self.timing);
        self.engine_index(id).is_some_and(|i| !audio[i])
    }

    /// The chain a device is in, for what the UI shows about it.
    pub fn chain_of(&self, id: DeviceId) -> Option<ChainRef> {
        self.chain_of_device(id)
    }

    pub fn devices_of(&self, chain: ChainRef) -> &[Device] {
        self.devices(chain)
    }

    fn find_device_any(&self, id: DeviceId) -> Option<&Device> {
        let chain = self.chain_of_device(id)?;
        self.devices(chain).iter().find(|d| d.id == id)
    }

    // ------------------------------------------------------------- crossfader

    pub fn set_crossfade_assign(&mut self, ids: &[TrackId], side: Crossfade) {
        for id in ids {
            let n = match side {
                Crossfade::None => 0.0,
                Crossfade::A => 1.0,
                Crossfade::B => 2.0,
            };
            if let Some(t) = self.project.track_mut(*id) {
                t.crossfade = side;
                self.set_cell(ChainRef::Track(*id), mix_cell::CROSSFADE_ASSIGN, n, true);
            }
        }
        self.mark_edit("Crossfade Assign");
    }

    pub fn set_crossfader(&mut self, position: f32) {
        let position = position.clamp(-1.0, 1.0);
        self.project.crossfader.position = position;
        self.set_cell(
            ChainRef::Master,
            mix_cell::CROSSFADER,
            position as f64,
            true,
        );
        self.mark_edit("Crossfader");
    }

    pub fn set_crossfade_curve(&mut self, curve: CrossfadeCurve) {
        self.project.crossfader.curve = curve.index();
        self.send_track_edits(Vec::new());
        self.mark_edit("Crossfader Curve");
    }

    /// A key mapped to one of the crossfader's positions went down.
    pub fn crossfader_key_down(&mut self, field: CrossfaderField) {
        let position = self.project.crossfader.position;
        let target = self.crossfader_keys.pressed(field, position);
        self.set_crossfader(target);
    }

    pub fn crossfader_key_up(&mut self, field: CrossfaderField) {
        if let Some(back) = self.crossfader_keys.released(field) {
            self.set_crossfader(back);
        }
    }

    // ------------------------------------------------------------- cue and outputs

    pub fn set_cue_mode(&mut self, on: bool) {
        self.project.cue.cue_mode = on;
        self.send_track_edits(Vec::new());
        self.mark_edit("Solo/Cue");
    }

    pub fn set_cue_out(&mut self, out: ExternalRoute) {
        self.project.cue.out = Some(out);
        self.send_track_edits(Vec::new());
        self.mark_edit("Cue Out");
    }

    /// Whether a track's recordings keep the timing of its monitoring path (manual p. 374).
    pub fn set_keep_monitor_latency(&mut self, tracks: &[TrackId], keep: bool) {
        for id in tracks {
            if let Some(t) = self.project.track_mut(*id) {
                t.keep_monitor_latency = keep;
            }
        }
        self.mark_edit("Keep Monitoring Latency");
    }

    pub fn set_main_out(&mut self, out: ExternalRoute) {
        self.project.master.routing.output = Route::External(out);
        self.send_track_edits(Vec::new());
        self.mark_edit("Main Out");
    }

    pub fn set_cue_volume(&mut self, db: f32) {
        self.project.cue.volume_db = db;
        self.set_cell(ChainRef::Master, mix_cell::CUE_VOLUME, db as f64, true);
        self.mark_edit("Cue Volume");
    }

    // ------------------------------------------------------------- preview

    /// Heard from the browser, ahead of the mixer: through Cue when it is on, or Main.
    pub fn preview_file(&mut self, path: &Path) {
        self.pending_preview = Some(path.to_path_buf());
        self.poll_preview();
    }

    pub fn stop_preview(&mut self) {
        self.pending_preview = None;
        self.send(EngineCommand::Preview(PreviewCmd::Stop));
    }

    pub(super) fn poll_preview(&mut self) {
        let Some(path) = self.pending_preview.clone() else {
            return;
        };
        match self.samples.request(&path) {
            rlive_audio_io::SampleStatus::Loading => {}
            rlive_audio_io::SampleStatus::Ready(sample) => {
                self.pending_preview = None;
                self.send(EngineCommand::Preview(PreviewCmd::Play(sample)));
            }
            rlive_audio_io::SampleStatus::Failed(e) => {
                self.pending_preview = None;
                self.messages.push(e);
            }
        }
    }

    // ------------------------------------------------------------- the choosers

    /// What can be picked in a track's Audio/MIDI From type chooser.
    pub fn input_types(&self, id: TrackId, hw: &AudioHardware) -> Vec<RouteChoice> {
        let Some(t) = self.project.track(id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match t.kind {
            TrackKind::Audio => {
                if let Some((_, first)) = hw.inputs.first() {
                    out.push(RouteChoice::of("Ext. In", Route::External(first.clone())));
                }
                out.push(RouteChoice::of("Resampling", Route::Resampling));
            }
            TrackKind::Midi => {
                out.push(RouteChoice::of("All Ins", Route::Default));
                out.push(RouteChoice::of(
                    "Ext. In",
                    Route::External(ExternalRoute::Midi {
                        port: None,
                        channel: None,
                    }),
                ));
            }
            _ => return out,
        }
        out.extend(self.track_sources(id, t.kind));
        out.push(RouteChoice::of("No Input", Route::None));
        out
    }

    fn track_sources(&self, id: TrackId, kind: TrackKind) -> Vec<RouteChoice> {
        let audio = audio_outputs(&self.project, &self.timing);
        self.all_tracks()
            .enumerate()
            .filter(|(i, t)| {
                t.id != id
                    && t.kind != TrackKind::Master
                    && match kind {
                        TrackKind::Audio => audio[*i],
                        _ => t.kind == TrackKind::Midi,
                    }
                    && self.route_allowed(id, Side::Input, &track_route(t))
            })
            .map(|(_, t)| RouteChoice::of(&t.name, track_route(t)))
            .collect()
    }

    /// What can be picked in a track's Input Channel chooser.
    pub fn input_channels(&self, id: TrackId, hw: &AudioHardware) -> Vec<RouteChoice> {
        let Some(t) = self.project.track(id) else {
            return Vec::new();
        };
        match &t.routing.input {
            Route::Track { track, .. } => {
                let route = Route::Track {
                    track: *track,
                    device: None,
                };
                let Some(source) = self.project.track(*track) else {
                    return Vec::new();
                };
                let mut out = vec![
                    RouteChoice::tapping("Pre FX", route.clone(), Tap::PreFx),
                    RouteChoice::tapping("Post FX", route.clone(), Tap::PostFx),
                    RouteChoice::tapping("Post Mixer", route.clone(), Tap::PostMixer),
                ];
                for d in &source.devices {
                    collect_taps(d, &mut out, &route, &self.runtimes);
                }
                out
            }
            Route::External(ExternalRoute::Audio { .. }) => hw
                .inputs
                .iter()
                .map(|(name, r)| RouteChoice::of(name, Route::External(r.clone())))
                .collect(),
            Route::External(ExternalRoute::Midi { port, .. }) => {
                let port = port.clone();
                let mut out = vec![RouteChoice::of(
                    "All Channels",
                    Route::External(ExternalRoute::Midi {
                        port: port.clone(),
                        channel: None,
                    }),
                )];
                out.extend((0..16u8).map(|c| {
                    RouteChoice::of(
                        format!("Ch. {}", c + 1),
                        Route::External(ExternalRoute::Midi {
                            port: port.clone(),
                            channel: Some(c),
                        }),
                    )
                }));
                out
            }
            _ => Vec::new(),
        }
    }

    /// What can be picked in a track's Audio/MIDI To type chooser.
    pub fn output_types(&self, id: TrackId, hw: &AudioHardware) -> Vec<RouteChoice> {
        let Some(t) = self.project.track(id) else {
            return Vec::new();
        };
        let audio = audio_outputs(&self.project, &self.timing);
        let midi_out = self.engine_index(id).is_some_and(|i| !audio[i]);
        let mut out = vec![RouteChoice::of("Main", Route::Default)];
        if midi_out {
            out[0].label = "No Output".into();
            out[0].route = Route::None;
        } else if let Some((_, first)) = hw.outputs.first() {
            out.push(RouteChoice::of("Ext. Out", Route::External(first.clone())));
        }
        if !midi_out && t.kind != TrackKind::Return {
            out.push(RouteChoice::of("Sends Only", Route::SendsOnly));
        }
        out.extend(
            self.project
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, x)| {
                    x.id != id
                        && (!midi_out || x.kind == TrackKind::Midi)
                        && self.route_allowed(id, Side::Output, &track_route(x))
                })
                .map(|(_, x)| RouteChoice::of(&x.name, track_route(x))),
        );
        out
    }

    /// What can be picked in a track's Output Channel chooser.
    pub fn output_channels(&self, id: TrackId, hw: &AudioHardware) -> Vec<RouteChoice> {
        let Some(t) = self.project.track(id) else {
            return Vec::new();
        };
        match &t.routing.output {
            Route::Track { track, .. } | Route::TrackChannel { track, .. } => {
                let track_id = *track;
                let Some(target) = self.project.track(track_id) else {
                    return Vec::new();
                };
                let mut out = vec![RouteChoice::of(
                    "Track In",
                    Route::Track {
                        track: track_id,
                        device: None,
                    },
                )];
                let audio = audio_outputs(&self.project, &self.timing);
                let midi_out = self.engine_index(id).is_some_and(|i| !audio[i]);
                let mut devices = Vec::new();
                for d in &target.devices {
                    d.for_each(&mut |d| devices.push((d.id, d.name.clone())));
                }
                for (device, name) in devices {
                    let takes_key = self
                        .runtimes
                        .get(&device)
                        .is_some_and(|rt| rt.sidechain_input);
                    if midi_out || takes_key {
                        let suffix = if midi_out { "" } else { " | Sidechain" };
                        out.push(RouteChoice::of(
                            format!("{name}{suffix}"),
                            Route::Track {
                                track: track_id,
                                device: Some(device),
                            },
                        ));
                    }
                }
                if midi_out {
                    out.extend((0..16u8).map(|c| {
                        RouteChoice::of(
                            format!("Ch. {}", c + 1),
                            Route::TrackChannel {
                                track: track_id,
                                channel: c,
                            },
                        )
                    }));
                }
                out
            }
            Route::External(_) => hw
                .outputs
                .iter()
                .map(|(name, r)| RouteChoice::of(name, Route::External(r.clone())))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// What a built-in device's own sidechain chooser offers: every other track, at every point.
    pub fn sidechain_sources(&self, device: DeviceId) -> Vec<(String, SidechainSource)> {
        let Some(chain) = self.chain_of_device(device) else {
            return Vec::new();
        };
        let ChainRef::Track(owner) = chain.owner() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for t in self.all_tracks() {
            if t.id == owner {
                continue;
            }
            for (name, tap) in [
                ("Pre FX", Tap::PreFx),
                ("Post FX", Tap::PostFx),
                ("Post Mixer", Tap::PostMixer),
            ] {
                out.push((
                    format!("{} | {name}", t.name),
                    SidechainSource { track: t.id, tap },
                ));
            }
        }
        out
    }
}

/// Adds the tap points inside a device of a tapped track to a chooser, for racks and instruments
/// with extra outputs.
fn collect_taps(
    device: &Device,
    out: &mut Vec<RouteChoice>,
    route: &Route,
    runtimes: &std::collections::HashMap<DeviceId, super::DeviceRuntime>,
) {
    let chains: Vec<&rlive_project::Chain> = device.chains().collect();
    for chain in chains {
        for (label, point) in [
            ("Pre FX", ChainTap::PreFx),
            ("Post FX", ChainTap::PostFx),
            ("Post Mixer", ChainTap::PostMixer),
        ] {
            out.push(RouteChoice::tapping(
                format!("{} | {} | {label}", device.name, chain.name),
                route.clone(),
                Tap::Chain {
                    chain: chain.id,
                    point,
                },
            ));
        }
        for d in &chain.devices {
            collect_taps(d, out, route, runtimes);
        }
    }
    if let Some(rt) = runtimes.get(&device.id) {
        for (n, name) in rt.aux_names.iter().enumerate() {
            out.push(RouteChoice::tapping(
                format!("{} | {name}", device.name),
                route.clone(),
                Tap::Aux {
                    device: device.id,
                    output: n as u8,
                },
            ));
        }
    }
}

/// Whether a track's Arm button is shown: not on tracks with no input.
pub fn can_arm(project: &Project, id: TrackId) -> bool {
    project.track(id).is_some_and(|t| {
        matches!(t.kind, TrackKind::Audio | TrackKind::Midi) && t.routing.input != Route::None
    })
}

#[cfg(test)]
mod tests {
    use super::CrossfaderField as F;
    use super::*;

    #[test]
    fn a_key_alone_toggles_between_the_ends() {
        let mut keys = CrossfaderKeys::default();
        assert_eq!(keys.pressed(F::Left, 0.0), -1.0);
        assert_eq!(keys.released(F::Left), None);
        assert_eq!(keys.pressed(F::Left, -1.0), 1.0);
        keys.released(F::Left);
        assert_eq!(keys.pressed(F::Center, -1.0), 1.0);
        keys.released(F::Center);
        assert_eq!(keys.pressed(F::Center, 1.0), -1.0);
        keys.released(F::Center);
        assert_eq!(keys.pressed(F::Right, 0.3), 1.0);
    }

    #[test]
    fn holding_one_key_and_tapping_another_snaps_back() {
        let mut keys = CrossfaderKeys::default();
        keys.pressed(F::Left, 0.0);
        assert_eq!(keys.pressed(F::Right, -1.0), 1.0, "jumps to the other");
        assert_eq!(keys.released(F::Right), Some(-1.0), "and snaps back");
        assert_eq!(keys.released(F::Left), None);
    }
}
