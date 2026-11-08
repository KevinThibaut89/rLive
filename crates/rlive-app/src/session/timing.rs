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
use rlive_engine::{
    ChainAddr, GraphOp, RenderPlan, Step, StereoLine, Tap, TapLines, MAX_COMPENSATION_FRAMES,
};
use rlive_project::{Device, DeviceKind, Monitor, Project, Track, TrackDelay, TrackKind};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

const MAX: i64 = MAX_COMPENSATION_FRAMES as i64;

/// Where a device's latency comes from.
pub(super) trait LatencySource {
    fn device_latency(&self, device: &Device) -> u32;
    /// A rack's latency, now that it is known, for whatever displays it.
    fn report_rack(&self, _rack: DeviceId, _latency: u32) {}
}

/// Latencies the engine publishes through the devices' shared atomics.
pub(super) struct RuntimeLatency<'a>(pub &'a HashMap<DeviceId, DeviceRuntime>);

impl LatencySource for RuntimeLatency<'_> {
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
    pub(super) chains: Vec<ChainTiming>,
    pub(super) racks: Vec<RackTiming>,
    pub(super) bypass: Vec<BypassTiming>,
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
            let own = walk(&t.devices, chain, source, &mut timing);
            timing.own.push(if on { own } else { 0 });
            timing.shift.push(if on {
                frames_of(t.delay, sample_rate)
            } else {
                0
            });
            timing.exempt.push(reduced && monitors(t));
        }
        let own = walk(
            &project.master.devices,
            ChainAddr::MASTER,
            source,
            &mut timing,
        );
        timing.own.push(if on { own } else { 0 });
        if !on {
            timing.chains.iter_mut().for_each(|c| c.delay = 0);
            timing.racks.iter_mut().for_each(|r| r.delay = 0);
            timing.bypass.iter_mut().for_each(|d| d.delay = 0);
        }
        timing
    }

    /// Fill in each input's delay, so everything meeting at a track arrives together. Steps come
    /// in rendering order: everything feeding a track, other than feedback, is before it.
    pub(super) fn align(&self, steps: &mut [Step], master: usize) -> Aligned {
        let own = |i: usize| self.own.get(i).copied().unwrap_or(0) as i64;
        let shift = |i: usize| self.shift.get(i).copied().unwrap_or(0);
        let exempt = |i: usize| self.exempt.get(i).copied().unwrap_or(false);
        // How late each track's output is, compared with the playhead.
        let mut out = vec![0i64; master + 1];
        let mut click = 0;
        for step in steps.iter_mut() {
            let track = step.track as usize;
            let arrival = step
                .inputs
                .iter()
                .filter(|i| !i.feedback && !exempt(i.from as usize) && !is_rack_out(i))
                .map(|i| out[i.from as usize] - shift(i.from as usize))
                .max()
                .unwrap_or(0)
                .max(0);
            for input in &mut step.inputs {
                let from = input.from as usize;
                input.delay = if input.feedback || exempt(from) || is_rack_out(input) {
                    0
                } else {
                    (arrival + shift(from) - out[from]).clamp(0, MAX) as u32
                };
            }
            out[track] = (arrival + own(track)).min(MAX);
            if track == master {
                click = arrival.min(MAX);
            }
        }
        Aligned {
            click_delay: click as u32,
            output_latency: out[master] as u32,
        }
    }
}

/// A rack return chain feeding a return track directly. It leaves the source track partway through
/// its devices, so it isn't compared with the track's main output and takes no delay.
fn is_rack_out(input: &rlive_engine::BusIn) -> bool {
    matches!(input.tap, Tap::Aux(..))
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

/// The latency of a chain, noting what it holds back along the way.
fn walk(devices: &[Device], addr: ChainAddr, source: &impl LatencySource, out: &mut Timing) -> u32 {
    let mut total = 0u32;
    for d in devices {
        let latency = match &d.kind {
            DeviceKind::Rack {
                chains, returns, ..
            } => {
                let lens: Vec<_> = chains
                    .iter()
                    .map(|c| Some((addr.child(d.id, c.id)?, c)))
                    .map(|c| c.map(|(a, c)| (c.id, walk(&c.devices, a, source, out))))
                    .collect();
                let longest = lens.iter().flatten().map(|l| l.1).max().unwrap_or(0);
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
                        Some((r, walk(&r.devices, addr.child(d.id, r.id)?, source, out)))
                    })
                    .collect();
                let slowest_return = return_lens
                    .iter()
                    .filter(|(r, _)| r.mix.output.is_none())
                    .map(|(_, len)| *len)
                    .max()
                    .unwrap_or(0);
                for (r, len) in &return_lens {
                    if r.mix.output.is_none() {
                        out.chains.push(ChainTiming {
                            target: addr,
                            device: d.id,
                            chain: r.id,
                            delay: slowest_return - len,
                        });
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
    for input in plan.steps.iter().flat_map(|s| &s.inputs) {
        if input.delay > 0 {
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
    if plan.click_delay > 0 {
        ops.push(GraphOp::SetClickLine(StereoLine::new(
            plan.click_delay,
            max_block,
        )));
    }
    ops
}
