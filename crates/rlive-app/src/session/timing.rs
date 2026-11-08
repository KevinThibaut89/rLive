//! Automatic delay compensation: how late each signal is, and how much to hold back what reaches
//! a mix point sooner than the rest.
//!
//! A device's reported latency delays everything after it. A rack is as late as its slowest chain,
//! with the others held back inside the rack to meet it. Track Delay is a deliberate offset on top:
//! a signal is compared as if it weren't there, so a negative one holds back everything else.
//! Feedback sends are not compensated; they already hear the past.
//!
//! As the manual warns (p. 478), tempo-synced effects and other devices reading the song position
//! may sound late when placed after a latent device: the transport a device sees is not shifted
//! by what is ahead of it in its chain. Devices on a track that is held back at a mix point are
//! unaffected, since the track itself is on time.
//!
//! A bypassed device stays exactly as late as it would be switched on (p. 992), so switching one
//! never moves anything. Latency only counts when compensation is on.
//!
//! Compensation costs a line per delayed signal, and every delay it adds is real time between
//! playing a note and hearing it, which is why the Options menu can turn it off.

use super::build::DeviceRuntime;
use rlive_core::{ChainId, DeviceId};
use rlive_devices::DeviceRole;
use rlive_engine::taps::TapBus;
use rlive_engine::{
    ChainAddr, GraphOp, HardwareOut, RenderPlan, SidechainIn, Step, StereoLine, Tap, TapLines,
    TrackIo, MAX_COMPENSATION_FRAMES,
};
use rlive_project::{Device, DeviceKind, Monitor, Project, Track, TrackDelay, TrackKind};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

const MAX: i64 = MAX_COMPENSATION_FRAMES as i64;

/// Where a device's latency comes from.
pub(super) trait LatencySource {
    fn device_latency(&self, device: &Device) -> u32;
    /// Whether a device turns MIDI into sound, if known. Plug-ins are only known when built.
    fn is_instrument(&self, _device: &Device) -> Option<bool> {
        None
    }
    /// A rack's latency, now that it is known, for whatever displays it.
    fn report_rack(&self, _rack: DeviceId, _latency: u32) {}
}

/// Latencies the engine publishes through the devices' shared atomics.
pub(super) struct RuntimeLatency<'a>(pub &'a HashMap<DeviceId, DeviceRuntime>);

impl LatencySource for RuntimeLatency<'_> {
    fn is_instrument(&self, device: &Device) -> Option<bool> {
        self.0
            .get(&device.id)
            .map(|rt| rt.role == DeviceRole::Instrument)
    }

    fn device_latency(&self, device: &Device) -> u32 {
        self.0.get(&device.id).map_or(0, |rt| {
            if rt.role == DeviceRole::MidiEffect {
                0
            } else {
                rt.latency.load(Ordering::Relaxed)
            }
        })
    }

    fn report_rack(&self, rack: DeviceId, latency: u32) {
        if let Some(rt) = self.0.get(&rack) {
            rt.latency.store(latency, Ordering::Relaxed);
        }
    }
}

/// How late one chain of a rack has to be held back.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ChainTiming {
    pub target: ChainAddr,
    pub device: DeviceId,
    pub chain: ChainId,
    pub delay: u32,
}

/// How far a rack holds its summed chains back, to meet its slowest return chain.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RackTiming {
    pub target: ChainAddr,
    pub device: DeviceId,
    pub delay: u32,
}

/// How late a device is while bypassed.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BypassTiming {
    pub target: ChainAddr,
    pub device: DeviceId,
    pub delay: u32,
}

/// What compensation was worked out from. Two equal timings compile to the same plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Timing {
    /// Latency of each track's devices, in frames, then the Master's.
    own: Vec<u32>,
    /// Each track's Track Delay, in frames.
    shift: Vec<i64>,
    /// Tracks excused from compensation: monitored inputs under Reduced Latency.
    exempt: Vec<bool>,
    /// Tracks holding clips of their own to keep in step with their inputs: audio tracks.
    is_clip_track: Vec<bool>,
    pub(super) chains: Vec<ChainTiming>,
    pub(super) racks: Vec<RackTiming>,
    pub(super) bypass: Vec<BypassTiming>,
    /// Which plug-ins are instruments, once built. Empty until measured.
    instruments: HashMap<DeviceId, bool>,
    /// How late each chain of a rack is from its track's start: at its top, and at its end.
    chain_points: HashMap<ChainId, (u32, u32)>,
    /// How late each device's input and output are from its track's start.
    device_points: HashMap<DeviceId, (u32, u32)>,
    /// For each track, how late each of its rack outputs is from its track's start, by slot.
    rack_outs: Vec<Vec<u32>>,
    /// Return chains routed to a Set return in the track being walked.
    leaving: Vec<LeavingChain>,
}

/// A return chain on its way out of its rack to a Set return track.
#[derive(Debug, Clone, PartialEq)]
struct LeavingChain {
    to: rlive_core::TrackId,
    rack: ChainAddr,
    device: DeviceId,
    chain: ChainId,
    /// How late it comes out, before anything lines it up with others going the same way.
    end: u32,
}

/// One bus of a plan: how many readers it has and the longest any waits. Two equal shapes need
/// no new buses.
pub(super) type BusShape = Vec<(u16, u32)>;

/// The buses for a plan.
pub(super) fn buses_op(shape: &BusShape, max_block: usize) -> GraphOp {
    let buses = shape
        .iter()
        .map(|&(readers, delay)| TapBus::new(max_block, readers as usize, delay))
        .collect();
    GraphOp::SetBuses(Box::new(buses))
}

/// What a plan needs of its buses.
pub(super) fn bus_shape(plan: &RenderPlan) -> BusShape {
    let mut shape = vec![(0u16, 0u32); plan.buses as usize];
    let mut note = |read: rlive_engine::BusRead, delay: u32| {
        let at = &mut shape[read.bus as usize];
        at.0 = at.0.max(read.reader + 1);
        at.1 = at.1.max(delay);
    };
    for input in plan.steps.iter().flat_map(|s| &s.inputs) {
        if let Some(read) = input.bus {
            note(read, input.delay);
        }
    }
    for sc in &plan.sidechains {
        note(sc.read, sc.delay);
    }
    shape
}

/// What [`Timing::align`] found beyond the delays it wrote.
pub(super) struct Aligned {
    pub click_delay: u32,
    pub output_latency: u32,
}

impl Timing {
    /// Read a project's latencies. With compensation off nothing is delayed.
    pub(super) fn measure(
        project: &Project,
        sample_rate: f64,
        source: &impl LatencySource,
    ) -> Self {
        let all: Vec<&Track> = project.tracks.iter().chain(&project.returns).collect();
        let on = project.delay_compensation;
        let reduced = on && project.reduced_latency_when_monitoring;
        let mut timing = Timing::default();
        for t in &all {
            let chain = ChainAddr::track(t.id);
            let own = walk(&t.devices, chain, 0, source, &mut timing);
            let outs = timing.settle_leaving(t);
            timing.rack_outs.push(if on { outs } else { Vec::new() });
            let instruments = &mut timing.instruments;
            for d in &t.devices {
                if let Some(known) = source.is_instrument(d) {
                    instruments.insert(d.id, known);
                }
            }
            timing.own.push(if on { own } else { 0 });
            timing.shift.push(if on {
                frames_of(t.delay, sample_rate)
            } else {
                0
            });
            timing.exempt.push(reduced && monitors(t));
            timing.is_clip_track.push(t.kind == TrackKind::Audio);
        }
        let own = walk(
            &project.master.devices,
            ChainAddr::MASTER,
            0,
            source,
            &mut timing,
        );
        timing.leaving.clear();
        timing.own.push(if on { own } else { 0 });
        if !on {
            timing.chains.iter_mut().for_each(|c| c.delay = 0);
            timing.racks.iter_mut().for_each(|r| r.delay = 0);
            timing.bypass.iter_mut().for_each(|d| d.delay = 0);
            timing.chain_points.values_mut().for_each(|p| *p = (0, 0));
            timing.device_points.values_mut().for_each(|p| *p = (0, 0));
        }
        timing
    }

    /// Return chains going to the same Set return leave together, however different their racks
    /// are, since they share one output. Returns how late each output is, by slot.
    fn settle_leaving(&mut self, t: &Track) -> Vec<u32> {
        let leaving = std::mem::take(&mut self.leaving);
        let targets = super::graph::rack_out_targets(t);
        let mut slots = Vec::with_capacity(targets.len());
        for target in targets {
            let going = || leaving.iter().filter(|l| l.to == target);
            let end = going().map(|l| l.end).max().unwrap_or(0);
            for l in going() {
                self.chains.push(ChainTiming {
                    target: l.rack,
                    device: l.device,
                    chain: l.chain,
                    delay: end - l.end,
                });
                if let Some(point) = self.chain_points.get_mut(&l.chain) {
                    point.1 = end;
                }
            }
            slots.push(end);
        }
        slots
    }

    /// How long a track's own chain takes, in frames.
    pub(super) fn own_latency(&self, track: usize) -> u32 {
        self.own.get(track).copied().unwrap_or(0)
    }

    /// Whether a device is known to be an instrument, once it has been built.
    pub(super) fn instrument(&self, device: &Device) -> Option<bool> {
        self.instruments.get(&device.id).copied()
    }

    /// How late `tap` of a track is, from when its input arrived and its output left.
    fn tap_lateness(&self, from: usize, tap: &Tap, arrival: i64, out: i64) -> i64 {
        match tap {
            Tap::PreFx => arrival,
            Tap::PostFx | Tap::PostMixer => out,
            Tap::Chain(chain, point) => {
                let (top, end) = self.chain_points.get(chain).copied().unwrap_or((0, 0));
                arrival
                    + if matches!(**point, Tap::PreFx) {
                        top as i64
                    } else {
                        end as i64
                    }
            }
            Tap::Aux(DeviceId(0), slot) => {
                let late = self.rack_outs.get(from).and_then(|o| o.get(*slot as usize));
                arrival + late.copied().unwrap_or(0) as i64
            }
            Tap::Aux(device, _) => {
                arrival + self.device_points.get(device).map_or(0, |p| p.1 as i64)
            }
        }
    }

    /// Fill in each input's delay, so everything meeting at a track arrives together. Steps come
    /// in rendering order: everything feeding a track, other than feedback, is before it.
    pub(super) fn align(
        &self,
        steps: &mut [Step],
        sidechains: &mut [SidechainIn],
        hardware: &mut [HardwareOut],
        io: &mut [TrackIo],
        master: usize,
    ) -> Aligned {
        let own = |i: usize| self.own.get(i).copied().unwrap_or(0) as i64;
        let shift = |i: usize| self.shift.get(i).copied().unwrap_or(0);
        let exempt = |i: usize| self.exempt.get(i).copied().unwrap_or(false);
        // How late each track's input and output are, compared with the playhead.
        let mut late_in = vec![0i64; master + 1];
        let mut out = vec![0i64; master + 1];
        let mut click = 0;
        // Taps ahead of a track's mixer are unaffected by its Track Delay.
        let offset = |from: usize, tap: &Tap, is_send: bool| {
            if is_send || matches!(tap, Tap::PostMixer) {
                shift(from)
            } else {
                0
            }
        };
        for step in steps.iter_mut() {
            let track = step.track as usize;
            // A device's sidechain has to meet what the device hears, not the top of its chain.
            let keyed = sidechains
                .iter()
                .filter(|s| s.to as usize == track && !exempt(s.from as usize))
                .map(|s| {
                    let from = s.from as usize;
                    let at = self.tap_lateness(from, &s.tap, late_in[from], out[from])
                        - offset(from, &s.tap, false);
                    at - self.device_points.get(&s.device).map_or(0, |p| p.0 as i64)
                })
                .max()
                .unwrap_or(0);
            let arrival = step
                .inputs
                .iter()
                .filter(|i| !i.feedback && !exempt(i.from as usize))
                .map(|i| {
                    let from = i.from as usize;
                    self.tap_lateness(from, &i.tap, late_in[from], out[from])
                        - offset(from, &i.tap, i.gain.is_some())
                })
                .max()
                .unwrap_or(0)
                .max(keyed)
                .max(0);
            for input in &mut step.inputs {
                let from = input.from as usize;
                input.delay = if input.feedback || exempt(from) {
                    0
                } else {
                    let at = self.tap_lateness(from, &input.tap, late_in[from], out[from])
                        - offset(from, &input.tap, input.gain.is_some());
                    (arrival - at).clamp(0, MAX) as u32
                };
            }
            for sc in sidechains.iter_mut().filter(|s| s.to as usize == track) {
                let from = sc.from as usize;
                let at = self.tap_lateness(from, &sc.tap, late_in[from], out[from])
                    - offset(from, &sc.tap, false);
                let heard = arrival + self.device_points.get(&sc.device).map_or(0, |p| p.0 as i64);
                sc.delay = if exempt(from) {
                    0
                } else {
                    (heard - at).clamp(0, MAX) as u32
                };
            }
            if track < io.len() && self.is_clip_track.get(track).copied().unwrap_or(false) {
                io[track].own_delay = arrival.clamp(0, MAX) as u32;
            }
            late_in[track] = arrival;
            out[track] = (arrival + own(track)).min(MAX);
            if track == master {
                click = arrival.min(MAX);
            }
        }
        for hw in hardware.iter_mut() {
            let from = hw.input.from as usize;
            hw.input.delay = if exempt(from) {
                0
            } else {
                (out[master] + shift(from) - out[from]).clamp(0, MAX) as u32
            };
        }
        Aligned {
            click_delay: click as u32,
            output_latency: out[master] as u32,
        }
    }
}

/// Is this track's input heard live right now?
fn monitors(t: &Track) -> bool {
    !matches!(t.kind, TrackKind::Group | TrackKind::Return)
        && match t.routing.monitor {
            Monitor::In => true,
            Monitor::Auto => t.arm,
            Monitor::Off => false,
        }
}

fn frames_of(delay: TrackDelay, sample_rate: f64) -> i64 {
    let frames = match delay {
        TrackDelay::None => 0,
        TrackDelay::Ms(ms) => (ms as f64 / 1000.0 * sample_rate).round() as i64,
        TrackDelay::Samples(n) => n as i64,
    };
    frames.clamp(-MAX, MAX)
}

/// The latency of a chain, noting what it holds back along the way. `before` is how late the
/// chain's top is from the start of its track.
fn walk(
    devices: &[Device],
    addr: ChainAddr,
    before: u32,
    source: &impl LatencySource,
    out: &mut Timing,
) -> u32 {
    let mut total = 0u32;
    for d in devices {
        let at = before.saturating_add(total).min(MAX_COMPENSATION_FRAMES);
        let latency = match &d.kind {
            DeviceKind::Rack {
                chains, returns, ..
            } => {
                let lens: Vec<_> = chains
                    .iter()
                    .map(|c| Some((addr.child(d.id, c.id)?, c)))
                    .map(|c| c.map(|(a, c)| (c.id, walk(&c.devices, a, at, source, out))))
                    .collect();
                let longest = lens.iter().flatten().map(|l| l.1).max().unwrap_or(0);
                for (chain, _) in lens.iter().flatten() {
                    out.chain_points
                        .insert(*chain, (at, at.saturating_add(longest)));
                }
                for (chain, len) in lens.into_iter().flatten() {
                    out.chains.push(ChainTiming {
                        target: addr,
                        device: d.id,
                        chain,
                        delay: longest - len,
                    });
                }
                // Return chains that come back into the rack hold up everything else in it. Their
                // own latency adds to the chains'; ones routed out take their own way to the Set.
                let return_lens: Vec<_> = returns
                    .iter()
                    .filter_map(|r| {
                        let inner = addr.child(d.id, r.id)?;
                        let top = at.saturating_add(longest);
                        Some((r, walk(&r.devices, inner, top, source, out)))
                    })
                    .collect();
                let slowest_return = return_lens
                    .iter()
                    .filter(|(r, _)| r.mix.output.is_none())
                    .map(|(_, len)| *len)
                    .max()
                    .unwrap_or(0);
                let top = at.saturating_add(longest);
                for (r, len) in &return_lens {
                    let back_in = top.saturating_add(slowest_return);
                    out.chain_points.insert(r.id, (top, back_in));
                    match r.mix.output {
                        None => out.chains.push(ChainTiming {
                            target: addr,
                            device: d.id,
                            chain: r.id,
                            delay: slowest_return - len,
                        }),
                        Some(to) => out.leaving.push(LeavingChain {
                            to,
                            rack: addr,
                            device: d.id,
                            chain: r.id,
                            end: top.saturating_add(*len),
                        }),
                    }
                }
                if !returns.is_empty() {
                    out.racks.push(RackTiming {
                        target: addr,
                        device: d.id,
                        delay: slowest_return,
                    });
                }
                let total = longest + slowest_return;
                source.report_rack(d.id, total);
                total
            }
            _ => source.device_latency(d).min(MAX_COMPENSATION_FRAMES),
        };
        out.bypass.push(BypassTiming {
            target: addr,
            device: d.id,
            delay: latency,
        });
        out.device_points
            .insert(d.id, (at, at.saturating_add(latency)));
        total = total.saturating_add(latency).min(MAX_COMPENSATION_FRAMES);
    }
    total
}

/// The lines a plan needs and the delays of everything inside tracks. Everything here is built on
/// this thread and handed to the engine with the plan it goes with; a line the engine doesn't
/// need comes back as garbage.
pub(super) fn compensation_ops(
    plan: &RenderPlan,
    timing: &Timing,
    project: &Project,
    max_block: usize,
) -> Vec<GraphOp> {
    let ids: Vec<_> = project
        .tracks
        .iter()
        .chain(&project.returns)
        .map(|t| t.id)
        .collect();
    let mut wanted: HashMap<usize, [u32; 3]> = HashMap::new();
    let hardware = plan.hardware.iter().map(|h| &h.input);
    for input in plan.steps.iter().flat_map(|s| &s.inputs).chain(hardware) {
        if input.delay > 0 && input.bus.is_none() {
            let kind = match (input.gain, &input.tap) {
                (None, _) => 0,
                (Some(_), rlive_engine::Tap::PostFx) => 1,
                (Some(_), _) => 2,
            };
            let delays = wanted.entry(input.from as usize).or_insert([0; 3]);
            delays[kind] = delays[kind].max(input.delay);
        }
    }
    let line = |delay: u32| (delay > 0).then(|| StereoLine::new(delay, max_block));
    let mut ops = Vec::new();
    for (from, [main, pre, post]) in wanted {
        let Some(&track) = ids.get(from) else {
            continue;
        };
        ops.push(GraphOp::SetTapLines {
            track,
            lines: TapLines {
                main: line(main),
                pre: line(pre),
                post: line(post),
            },
        });
    }
    for c in &timing.chains {
        ops.push(GraphOp::SetChainDelay {
            target: c.target,
            device: c.device,
            chain: c.chain,
            delay: c.delay,
            line: line(c.delay),
        });
    }
    for r in &timing.racks {
        ops.push(GraphOp::SetRackDelay {
            target: r.target,
            device: r.device,
            delay: r.delay,
            line: line(r.delay),
        });
    }
    for d in &timing.bypass {
        ops.push(GraphOp::SetBypassDelay {
            target: d.target,
            device: d.device,
            delay: d.delay,
            line: line(d.delay),
        });
    }
    for (i, track_io) in plan.io.iter().enumerate() {
        if track_io.own_delay == 0 {
            continue;
        }
        if let Some(line) = line(track_io.own_delay) {
            ops.push(GraphOp::SetOwnLine {
                track: ids[i],
                line,
            });
        }
    }
    if plan.click_delay > 0 {
        ops.push(GraphOp::SetClickLine(StereoLine::new(
            plan.click_delay,
            max_block,
        )));
    }
    ops
}
