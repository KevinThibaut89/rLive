//! Compiles the project's routing into the plan the engine renders from, and sends structural
//! edits to the engine together with that plan.
//!
//! Routing, delay compensation and multi-core ordering all come down to producing a different
//! [`RenderPlan`] here. Delays are worked out in [`timing`](super::timing).

use super::timing::{bus_shape, buses_op, compensation_ops, RuntimeLatency, Timing};
use super::Session;
use rlive_core::{CrossfadeCurve, DeviceId, TrackId};
use rlive_devices::DeviceRole;
use rlive_engine::{
    mix_cell, BusIn, BusRead, EngineCommand, GraphOp, GraphTxn, HardwareOut, InputGate, MidiRoute,
    OutputPair, RackOuts, RenderPlan, SidechainIn, Step, Tap as EngineTap, TapOut, TrackIo,
    MAX_SENDS,
};
use rlive_project::{
    ChainTap, Device, DeviceKind, ExternalRoute, Monitor, Project, Route, Tap, Track, TrackKind,
};

/// Whether a device makes sound from MIDI.
pub(super) fn is_instrument(device: &Device) -> Option<bool> {
    match &device.kind {
        DeviceKind::Builtin { type_id, .. } => {
            Some(rlive_devices::find(type_id).is_some_and(|d| d.role == DeviceRole::Instrument))
        }
        DeviceKind::Rack { kind, .. } => Some(matches!(
            kind,
            rlive_project::RackKind::Instrument | rlive_project::RackKind::Drum
        )),
        DeviceKind::Plugin { .. } => None,
    }
}

/// A chain of a rack held in `devices`, however deep, with its rack's id.
pub(super) fn find_chain_in(devices: &[Device], id: rlive_core::ChainId) -> bool {
    devices.iter().any(|d| {
        d.chains()
            .any(|c| c.id == id || find_chain_in(&c.devices, id))
    })
}

fn find_device(devices: &[Device], id: DeviceId) -> Option<&Device> {
    devices.iter().find_map(|d| {
        if d.id == id {
            return Some(d);
        }
        d.chains().find_map(|c| find_device(&c.devices, id))
    })
}

fn out_pair(route: &ExternalRoute) -> Option<OutputPair> {
    match route {
        ExternalRoute::Audio { channel, stereo } => Some(OutputPair {
            channel: *channel,
            stereo: *stereo,
        }),
        ExternalRoute::Midi { .. } => None,
    }
}

fn engine_tap(tap: Tap) -> EngineTap {
    match tap {
        Tap::PreFx => EngineTap::PreFx,
        Tap::PostFx => EngineTap::PostFx,
        Tap::PostMixer => EngineTap::PostMixer,
        Tap::Chain { chain, point } => EngineTap::Chain(
            chain,
            Box::new(match point {
                ChainTap::PreFx => EngineTap::PreFx,
                ChainTap::PostFx => EngineTap::PostFx,
                ChainTap::PostMixer => EngineTap::PostMixer,
            }),
        ),
        Tap::Aux { device, output } => EngineTap::Aux(device, output),
    }
}

/// Does `tap` name something that exists in `source`?
pub(super) fn tap_exists(source: &Track, tap: Tap) -> bool {
    match tap {
        Tap::PreFx | Tap::PostFx | Tap::PostMixer => true,
        Tap::Chain { chain, .. } => find_chain_in(&source.devices, chain),
        Tap::Aux { device, .. } => find_device(&source.devices, device).is_some(),
    }
}

/// Which tracks can only give MIDI, because they have no instrument to make audio from it.
fn midi_only(t: &Track, instrument: impl Fn(&Device) -> bool) -> bool {
    t.kind == TrackKind::Midi && !t.devices.iter().any(instrument)
}

/// Edges the plan has committed to. Any new edge that would close a loop is refused: the manual
/// simply doesn't offer such a target, and a loaded file is handled the same way.
pub(super) struct Deps {
    /// What feeds each track.
    sources: Vec<Vec<usize>>,
    /// What each track feeds.
    sinks: Vec<Vec<usize>>,
}

impl Deps {
    pub(super) fn new(nodes: usize) -> Self {
        Self {
            sources: vec![Vec::new(); nodes],
            sinks: vec![Vec::new(); nodes],
        }
    }

    fn reaches(&self, from: usize, to: usize) -> bool {
        let mut seen = vec![false; self.sinks.len()];
        let mut stack = vec![from];
        while let Some(at) = stack.pop() {
            if at == to {
                return true;
            }
            if !std::mem::replace(&mut seen[at], true) {
                stack.extend(self.sinks[at].iter().copied());
            }
        }
        false
    }

    /// Whether `from` feeding `to` would close a loop.
    pub(super) fn closes_loop(&self, from: usize, to: usize) -> bool {
        from == to || self.reaches(to, from)
    }

    /// Commit an edge, unless it would close a loop.
    fn try_add(&mut self, from: usize, to: usize) -> bool {
        if self.closes_loop(from, to) {
            return false;
        }
        self.sources[to].push(from);
        self.sinks[from].push(to);
        true
    }
}

/// Every source a plan reads, handed a bus the first time it is needed.
#[derive(Default)]
struct TapTable {
    outs: Vec<(u32, EngineTap)>,
    readers: Vec<u16>,
}

impl TapTable {
    fn read(&mut self, from: usize, tap: EngineTap) -> BusRead {
        let at = self
            .outs
            .iter()
            .position(|(f, t)| *f == from as u32 && *t == tap);
        let bus = at.unwrap_or_else(|| {
            self.outs.push((from as u32, tap));
            self.readers.push(0);
            self.outs.len() - 1
        });
        let reader = self.readers[bus];
        self.readers[bus] += 1;
        BusRead {
            bus: bus as u32,
            reader,
            monitored: false,
        }
    }
}

/// One connection the routing asks for, before loops are looked at.
enum Conn {
    /// To the Master, which everything can reach.
    ToMaster { from: usize },
    /// A track's output into another track.
    ToTrack { from: usize, to: usize },
    /// A track's output straight to the hardware.
    ToHardware { from: usize, out: OutputPair },
    /// Audio into a device's sidechain input.
    Sidechain {
        from: usize,
        to: usize,
        device: DeviceId,
        tap: Tap,
        /// The source's Audio To points here, so it counts as feeding the device's track.
        via_output: bool,
    },
    /// Audio tapped by the track `to`, as its input.
    TapIn { from: usize, to: usize, tap: Tap },
    /// A rack's return chains, out to a Set return track.
    RackOut { from: usize, to: usize, slot: u8 },
    /// A send, in the track order the plan keeps them in.
    Send {
        from: usize,
        to: usize,
        k: usize,
        pre_fader: bool,
        feedback: bool,
    },
    /// MIDI from one track to another. `output` is set when the sender's own output points
    /// there, which makes it count as feeding that track.
    Midi { route: MidiRoute, output: bool },
}

impl Conn {
    /// The dependency this connection makes, if it makes one.
    fn edge(&self) -> Option<(usize, usize)> {
        match *self {
            Conn::ToMaster { .. } | Conn::ToHardware { .. } => None,
            Conn::ToTrack { from, to }
            | Conn::RackOut { from, to, .. }
            | Conn::Sidechain { from, to, .. }
            | Conn::TapIn { from, to, .. } => Some((from, to)),
            Conn::Send {
                from, to, feedback, ..
            } => (!feedback).then_some((from, to)),
            Conn::Midi { route, .. } => Some((route.from as usize, route.to as usize)),
        }
    }
}

/// Which side of a track a chooser would change, when working out what a new route may close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Input,
    Output,
}

/// Whether a track's output carries audio or only MIDI.
pub(super) fn audio_outputs(project: &Project, timing: &Timing) -> Vec<bool> {
    // A plug-in not measured yet might be an instrument. Taken as one, its track keeps its audio.
    let instrument = |d: &Device| timing.instrument(d).or(is_instrument(d)).unwrap_or(true);
    project
        .tracks
        .iter()
        .chain(&project.returns)
        .map(|t| !midi_only(t, instrument))
        .collect()
}

/// Everything the project's routing asks for, in the order a loop is settled: the earliest
/// connection wins. `skip` leaves out one side of one track, for a chooser checking a new route.
fn connections(project: &Project, audio: &[bool], skip: Option<(usize, Side)>) -> Vec<Conn> {
    let tracks = project.tracks.len();
    let all: Vec<&Track> = project.tracks.iter().chain(&project.returns).collect();
    let index_of = |id: rlive_core::TrackId| all.iter().position(|t| t.id == id);
    let skipped = |i: usize, side: Side| skip == Some((i, side));
    let device_in =
        |track: usize, device: DeviceId| find_device(&all[track].devices, device).is_some();
    let mut conns = Vec::new();
    // Main outputs.
    for (i, t) in all.iter().enumerate() {
        if skipped(i, Side::Output) {
            continue;
        }
        // A MIDI track with no instrument sends MIDI there, which is handled below.
        if !audio[i]
            && matches!(
                t.routing.output,
                Route::Track { .. } | Route::TrackChannel { .. }
            )
        {
            continue;
        }
        match &t.routing.output {
            Route::None | Route::SendsOnly | Route::TrackChannel { .. } => {}
            Route::External(ext) => {
                if let Some(out) = out_pair(ext).filter(|_| audio[i]) {
                    conns.push(Conn::ToHardware { from: i, out });
                }
            }
            Route::Track { track, device } if t.kind != TrackKind::Return => {
                match index_of(*track) {
                    Some(dest) => match device.filter(|d| device_in(dest, *d)) {
                        Some(device) => conns.push(Conn::Sidechain {
                            from: i,
                            to: dest,
                            device,
                            tap: Tap::PostMixer,
                            via_output: true,
                        }),
                        None => conns.push(Conn::ToTrack { from: i, to: dest }),
                    },
                    None => conns.push(Conn::ToMaster { from: i }),
                }
            }
            Route::Default if t.kind != TrackKind::Return => {
                match project.parent_group(t.id).and_then(index_of) {
                    Some(g) => conns.push(Conn::ToTrack { from: i, to: g }),
                    None => conns.push(Conn::ToMaster { from: i }),
                }
            }
            _ => conns.push(Conn::ToMaster { from: i }),
        }
    }
    // Sends, in track order, into the first MAX_SENDS returns.
    for (i, t) in all.iter().enumerate() {
        for (k, ret) in project.returns.iter().take(MAX_SENDS).enumerate() {
            if t.send_enabled(ret.id) {
                conns.push(Conn::Send {
                    from: i,
                    to: tracks + k,
                    k,
                    pre_fader: ret.pre_fader_sends,
                    // A return feeding itself or one before it hears the past, so the graph stays
                    // acyclic.
                    feedback: i >= tracks && k <= i - tracks,
                });
            }
        }
    }
    // Rack return chains that feed return tracks directly. A return track may only feed one
    // after it, so a chain of these can never loop.
    for (i, t) in all.iter().enumerate() {
        for (slot, target) in rack_out_targets(t).into_iter().enumerate() {
            let Some(k) = project.return_index(target).filter(|k| *k < MAX_SENDS) else {
                continue;
            };
            if i >= tracks && k <= i - tracks {
                continue;
            }
            conns.push(Conn::RackOut {
                from: i,
                to: tracks + k,
                slot: slot as u8,
            });
        }
    }
    // Inputs taken from another track, then MIDI outputs.
    for (i, t) in all.iter().enumerate() {
        match t.kind {
            TrackKind::Audio if !skipped(i, Side::Input) => {
                if let Route::Track { track, .. } = t.routing.input {
                    let Some(from) = index_of(track).filter(|f| audio[*f]) else {
                        continue;
                    };
                    let tap = t.routing.tap;
                    if tap_exists(all[from], tap) {
                        conns.push(Conn::TapIn { from, to: i, tap });
                    }
                }
            }
            TrackKind::Midi => {
                if !skipped(i, Side::Input) {
                    if let Route::Track { track, .. } = t.routing.input {
                        if let Some(from) =
                            index_of(track).filter(|f| all[*f].kind == TrackKind::Midi)
                        {
                            conns.push(Conn::Midi {
                                route: MidiRoute {
                                    from: from as u32,
                                    to: i as u32,
                                    device: None,
                                    channel: None,
                                },
                                output: false,
                            });
                        }
                    }
                }
                if !audio[i] && !skipped(i, Side::Output) {
                    let dest = match &t.routing.output {
                        Route::Track { track, device } => {
                            index_of(*track).map(|d| (d, device.filter(|x| device_in(d, *x)), None))
                        }
                        Route::TrackChannel { track, channel } => {
                            index_of(*track).map(|d| (d, None, Some(*channel)))
                        }
                        _ => None,
                    };
                    if let Some((to, device, channel)) = dest {
                        conns.push(Conn::Midi {
                            route: MidiRoute {
                                from: i as u32,
                                to: to as u32,
                                device,
                                channel,
                            },
                            output: true,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    // Sidechains chosen from the device itself.
    for (i, t) in all.iter().enumerate() {
        for d in &t.devices {
            d.for_each(&mut |d| {
                let Some(src) = d.sidechain else {
                    return;
                };
                if let Some(from) = index_of(src.track) {
                    if tap_exists(all[from], src.tap) {
                        conns.push(Conn::Sidechain {
                            from,
                            to: i,
                            device: d.id,
                            tap: src.tap,
                            via_output: false,
                        });
                    }
                }
            });
        }
    }
    conns
}

/// What the routing would be without one side of one track: what a chooser can still add to it
/// without closing a loop.
pub(super) fn deps_without(project: &Project, audio: &[bool], track: usize, side: Side) -> Deps {
    let mut deps = Deps::new(audio.len() + 1);
    for conn in connections(project, audio, Some((track, side))) {
        if let Some((from, to)) = conn.edge() {
            deps.try_add(from, to);
        }
    }
    deps
}

/// The render plan for a project: every track feeds its group or the Master, every clip and group
/// track feeds the returns through its sends, and tracks render after whatever feeds them. Group
/// nesting is what makes levels of a plan; the Master is always the last step.
pub fn compile_plan(project: &Project) -> RenderPlan {
    compile_with(project, &Timing::default())
}

/// [`compile_plan`] over the compensation `timing` asks for. Each input is delayed to reach its
/// destination in step with its siblings.
pub(super) fn compile_with(project: &Project, timing: &Timing) -> RenderPlan {
    let tracks = project.tracks.len();
    let returns = project.returns.len();
    let all: Vec<&Track> = project.tracks.iter().chain(&project.returns).collect();
    let master = tracks + returns;
    let index_of = |id: rlive_core::TrackId| all.iter().position(|t| t.id == id);
    let audio_out = audio_outputs(project, timing);

    let mut deps = Deps::new(master + 1);
    let mut inputs: Vec<Vec<BusIn>> = vec![Vec::new(); master + 1];
    let mut hardware = Vec::new();
    let mut taps = TapTable::default();
    let mut sidechains: Vec<(usize, usize, DeviceId, Tap)> = Vec::new();
    let mut midi: Vec<MidiRoute> = Vec::new();
    let mut output_parents: Vec<Option<u32>> = vec![None; master];
    let mut solo_taps: Vec<(u32, u32)> = Vec::new();
    for conn in connections(project, &audio_out, None) {
        if let Some((from, to)) = conn.edge() {
            if !deps.try_add(from, to) {
                // A loop in a file: the connection that would close it is dropped.
                continue;
            }
        }
        match conn {
            Conn::ToMaster { from } => inputs[master].push(BusIn::post_mixer(from as u32)),
            Conn::ToTrack { from, to } => {
                inputs[to].push(BusIn::post_mixer(from as u32));
                output_parents[from] = Some(to as u32);
            }
            Conn::ToHardware { from, out } => hardware.push(HardwareOut {
                input: BusIn::post_mixer(from as u32),
                out,
            }),
            Conn::Sidechain {
                from,
                to,
                device,
                tap,
                via_output,
            } => {
                if via_output {
                    output_parents[from] = Some(to as u32);
                }
                sidechains.push((from, to, device, tap));
            }
            Conn::RackOut { from, to, slot } => {
                let tap = EngineTap::Aux(DeviceId(0), slot);
                let read = taps.read(from, tap.clone());
                inputs[to].push(BusIn::tapped(from as u32, tap, read));
            }
            Conn::TapIn { from, to, tap } => {
                if tap.hears_through_solo() {
                    solo_taps.push((to as u32, from as u32));
                }
                let tap = engine_tap(tap);
                let mut read = taps.read(from, tap.clone());
                read.monitored = true;
                inputs[to].push(BusIn::tapped(from as u32, tap, read));
            }
            Conn::Send {
                from,
                to,
                k,
                pre_fader,
                feedback,
            } => {
                let mut input = BusIn::send(from as u32, mix_cell::send(k), pre_fader);
                input.feedback = feedback;
                inputs[to].push(input);
            }
            Conn::Midi { route, output } => {
                if output {
                    output_parents[route.from as usize] = Some(route.to);
                }
                midi.push(route);
            }
        }
    }
    midi.sort_by_key(|m| m.to);

    // Each track's depth: 0 for a track fed by nothing, and one past whatever feeds it.
    let mut depth = vec![0u32; master + 1];
    let mut known = vec![false; master + 1];
    fn depth_of(i: usize, sources: &[Vec<usize>], depth: &mut [u32], known: &mut [bool]) -> u32 {
        if known[i] {
            return depth[i];
        }
        known[i] = true;
        let d = sources[i]
            .iter()
            .map(|&s| depth_of(s, sources, depth, known) + 1)
            .max()
            .unwrap_or(0);
        depth[i] = d;
        d
    }
    let last = (0..master)
        .map(|i| depth_of(i, &deps.sources, &mut depth, &mut known))
        .max()
        .unwrap_or(0);
    let master_depth = last + 1;
    depth[master] = master_depth;

    let mut steps = Vec::with_capacity(master + 1);
    let mut levels = Vec::with_capacity(master_depth as usize + 1);
    for level in 0..=master_depth {
        let start = steps.len() as u32;
        for i in 0..=master {
            if depth[i] == level {
                steps.push(Step {
                    track: i as u32,
                    inputs: std::mem::take(&mut inputs[i]),
                });
            }
        }
        levels.push(start..steps.len() as u32);
    }

    // Now every signal that gets read is known, and so is each feed.
    let mut sc_inputs: Vec<SidechainIn> = Vec::with_capacity(sidechains.len());
    for (from, to, device, tap) in sidechains {
        let tap = engine_tap(tap);
        let read = taps.read(from, tap.clone());
        sc_inputs.push(SidechainIn {
            device,
            to: to as u32,
            from: from as u32,
            tap,
            delay: 0,
            read,
            feed: 0,
        });
    }
    let tap_buses = taps.outs.len() as u32;
    for (n, sc) in sc_inputs.iter_mut().enumerate() {
        sc.feed = tap_buses + n as u32;
    }

    let mut tap_outs: Vec<TapOut> = taps
        .outs
        .into_iter()
        .enumerate()
        .map(|(bus, (track, tap))| TapOut {
            track,
            tap,
            bus: bus as u32,
        })
        .collect();
    tap_outs.sort_by_key(|t| t.track);

    let mut io: Vec<TrackIo> = all
        .iter()
        .map(|t| TrackIo {
            gate: input_gate(t),
            own_delay: 0,
        })
        .collect();
    let aligned = timing.align(&mut steps, &mut sc_inputs, &mut hardware, &mut io, master);
    let groups = all
        .iter()
        .enumerate()
        .map(|(i, t)| {
            // The track its output feeds, or else its group, but never a loop of groups.
            output_parents[i].or_else(|| {
                let parent = project.parent_group(t.id)?;
                let acyclic = !project.ancestors(parent).contains(&t.id);
                acyclic.then(|| index_of(parent).map(|g| g as u32))?
            })
        })
        .collect();

    RenderPlan {
        steps,
        levels,
        buses: tap_buses + sc_inputs.len() as u32,
        tap_outs,
        sidechains: sc_inputs,
        midi,
        io,
        hardware,
        main_out: match &project.master.routing.output {
            Route::External(ext) => out_pair(ext).unwrap_or_default(),
            _ => OutputPair::default(),
        },
        cue: project.cue.cue_mode.then(|| {
            project
                .cue
                .out
                .as_ref()
                .and_then(out_pair)
                .unwrap_or(OutputPair {
                    channel: 2,
                    stereo: true,
                })
        }),
        crossfade_curve: CrossfadeCurve::from_index(project.crossfader.curve),
        solo_taps,
        groups,
        returns: tracks as u32..master as u32,
        solo_in_place: project.solo_in_place,
        click_delay: aligned.click_delay,
        output_latency: aligned.output_latency,
    }
}

/// The return tracks a track's racks feed directly, in the order of the track's rack outputs.
pub(super) fn rack_out_targets(t: &Track) -> Vec<TrackId> {
    fn walk(devices: &[Device], out: &mut Vec<TrackId>) {
        for d in devices {
            if let DeviceKind::Rack { returns, .. } = &d.kind {
                for r in returns {
                    if let Some(target) = r.mix.output {
                        if !out.contains(&target) {
                            out.push(target);
                        }
                    }
                }
            }
            for c in d.chains() {
                walk(&c.devices, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&t.devices, &mut out);
    out
}

/// The room each track needs for its rack outputs, sent along with every plan that may read it.
pub(super) fn rack_out_ops(project: &Project, max_block: usize) -> Vec<GraphOp> {
    project
        .tracks
        .iter()
        .chain(&project.returns)
        .filter_map(|t| {
            let targets = rack_out_targets(t);
            (!targets.is_empty()).then(|| GraphOp::SetRackOuts {
                track: t.id,
                outs: Box::new(RackOuts::new(targets, max_block)),
            })
        })
        .collect()
}

/// When this track's input comes through.
fn input_gate(t: &Track) -> InputGate {
    if !matches!(t.kind, TrackKind::Audio | TrackKind::Midi) {
        return InputGate::Never;
    }
    match t.routing.monitor {
        Monitor::In => InputGate::Always,
        Monitor::Auto if t.arm => InputGate::WhenIdle,
        Monitor::Auto | Monitor::Off => InputGate::Never,
    }
}

impl Session {
    /// Apply edits to devices and chains, all at once. Only for edits within tracks: the list of
    /// tracks must not change, so the running plan still applies.
    pub(super) fn send_edits(&mut self, mut ops: Vec<GraphOp>) {
        let routes = self.routes_for_bus_swaps(&ops);
        ops.extend(routes);
        let timing = self.measure_timing();
        if timing == self.timing {
            self.send(EngineCommand::Graph(GraphTxn::new(ops, None)));
        } else {
            self.send_recompiled(ops, timing);
        }
    }

    /// Apply edits that change the list of tracks, along with the plan for the result. Call after
    /// the project reflects them.
    pub(super) fn send_track_edits(&mut self, ops: Vec<GraphOp>) {
        let timing = self.measure_timing();
        self.send_recompiled(ops, timing);
    }

    fn send_recompiled(&mut self, mut ops: Vec<GraphOp>, timing: Timing) {
        let plan = compile_with(&self.project, &timing);
        let shape = bus_shape(&plan);
        if shape != self.bus_shape {
            ops.push(buses_op(&shape, self.engine.max_block));
            self.bus_shape = shape;
        }
        ops.extend(compensation_ops(
            &plan,
            &timing,
            &self.project,
            self.engine.max_block,
        ));
        ops.extend(rack_out_ops(&self.project, self.engine.max_block));
        self.timing = timing;
        self.send(EngineCommand::Graph(GraphTxn::new(ops, Some(plan))));
    }

    /// Recompile if what compensation depends on has moved: a plug-in's latency, or anything else
    /// that doesn't pass through an edit. Call at idle.
    pub(super) fn follow_latency(&mut self) {
        let timing = self.measure_timing();
        if timing != self.timing {
            self.send_recompiled(Vec::new(), timing);
        }
    }

    pub(super) fn measure_timing(&self) -> Timing {
        Timing::measure(
            &self.project,
            self.engine.sample_rate,
            &RuntimeLatency(&self.runtimes),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_engine::{create_engine, Tap};
    use rlive_plugin_host::scanner::PluginCache;
    use rlive_project::{Track, TrackKind};

    fn project(tracks: usize) -> Project {
        let mut p = Project::empty();
        p.tracks = (0..tracks)
            .map(|i| Track::new(&i.to_string(), TrackKind::Midi, [0, 0, 0], p.scenes.len()))
            .collect();
        p
    }

    #[test]
    fn every_track_renders_once_before_the_master() {
        let plan = compile_plan(&project(3));
        let steps: Vec<u32> = plan.steps.iter().map(|s| s.track).collect();
        assert_eq!(steps, [0, 1, 2, 3]);
        assert!(plan.steps[..3].iter().all(|s| s.inputs.is_empty()));
    }

    #[test]
    fn the_master_sums_every_track_in_order_after_its_mixer() {
        let plan = compile_plan(&project(4));
        let master = plan.steps.last().unwrap();
        let from: Vec<u32> = master.inputs.iter().map(|i| i.from).collect();
        assert_eq!(from, [0, 1, 2, 3]);
        assert!(master
            .inputs
            .iter()
            .all(|i| i.tap == Tap::PostMixer && i.delay == 0 && i.gain.is_none()));
    }

    #[test]
    fn the_tracks_share_a_level_and_the_master_has_its_own() {
        let plan = compile_plan(&project(5));
        assert_eq!(plan.levels, [0..5, 5..6]);
    }

    #[test]
    fn an_empty_project_is_just_the_master() {
        let plan = compile_plan(&project(0));
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].track, 0);
        assert!(plan.steps[0].inputs.is_empty());
        assert_eq!(plan.levels, [0..0, 0..1]);
    }

    #[test]
    fn the_engine_runs_the_plan_after_every_kind_of_track_edit() {
        let (mut engine, handle) = create_engine(48_000.0, 512);
        let mut s = Session::new(handle, Project::new_default(), PluginCache::default());
        let mut block = |s: &mut Session| {
            let (mut l, mut r) = ([0.0; 512], [0.0; 512]);
            engine.process_stereo(&mut l, &mut r);
            s.idle();
            engine.has_compiled_plan()
        };
        assert!(block(&mut s), "after the first build");
        let added = s.add_track(TrackKind::Midi, Some(0));
        assert!(block(&mut s), "after adding a track");
        s.move_track(added, 2);
        assert!(block(&mut s), "after moving a track");
        s.delete_track(added);
        assert!(block(&mut s), "after deleting a track");
        s.rebuild();
        assert!(block(&mut s), "after a rebuild");
    }

    #[test]
    fn nothing_needs_a_shared_bus_or_a_sidechain_yet() {
        let plan = compile_plan(&project(2));
        assert_eq!(plan.buses, 0);
        assert!(plan.sidechains.is_empty());
    }
}
