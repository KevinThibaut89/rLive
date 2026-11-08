//! The shape of a session: how to name a chain in the device tree, and how audio moves between
//! tracks each block.
//!
//! A [`RenderPlan`] is compiled on the UI thread (`rlive-app/src/session/graph.rs`) and handed to
//! the engine inside a [`GraphTxn`](crate::GraphTxn), together with the edits it was compiled for.

use crate::params::CellIx;
use rlive_core::{ChainId, DeviceId, TrackId};
use std::ops::Range;

/// How deep racks can nest. Bounds the audio thread's recursion and a [`ChainPath`]'s size.
pub const MAX_RACK_DEPTH: usize = 8;

/// A track or the Master: what owns a parameter bus and a root device chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackRef {
    Track(TrackId),
    Master,
}

/// One step down the device tree: a rack, and one of its chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChainStep {
    pub rack: DeviceId,
    pub chain: ChainId,
}

const NO_STEP: ChainStep = ChainStep {
    rack: DeviceId(0),
    chain: ChainId(0),
};

/// The way from a root chain down to a chain inside nested racks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChainPath {
    steps: [ChainStep; MAX_RACK_DEPTH],
    len: u8,
}

impl ChainPath {
    /// The root chain itself.
    pub const ROOT: ChainPath = ChainPath {
        steps: [NO_STEP; MAX_RACK_DEPTH],
        len: 0,
    };

    pub fn steps(&self) -> &[ChainStep] {
        &self.steps[..self.len as usize]
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// One step further down, or `None` if that would nest deeper than [`MAX_RACK_DEPTH`].
    pub fn child(&self, step: ChainStep) -> Option<ChainPath> {
        if self.len() == MAX_RACK_DEPTH {
            return None;
        }
        let mut path = *self;
        path.steps[path.len()] = step;
        path.len += 1;
        Some(path)
    }
}

/// A device chain anywhere in the session: an owner's root chain, or one nested below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChainAddr {
    pub owner: TrackRef,
    pub path: ChainPath,
}

impl ChainAddr {
    pub const MASTER: ChainAddr = ChainAddr {
        owner: TrackRef::Master,
        path: ChainPath::ROOT,
    };

    pub fn track(id: TrackId) -> Self {
        Self {
            owner: TrackRef::Track(id),
            path: ChainPath::ROOT,
        }
    }

    /// A chain of the rack `rack`, found in this chain.
    pub fn child(self, rack: DeviceId, chain: ChainId) -> Option<Self> {
        Some(Self {
            owner: self.owner,
            path: self.path.child(ChainStep { rack, chain })?,
        })
    }
}

impl From<TrackRef> for ChainAddr {
    fn from(owner: TrackRef) -> Self {
        Self {
            owner,
            path: ChainPath::ROOT,
        }
    }
}

/// The value of [`Step::track`] for the Master, which isn't one of the engine's tracks yet.
pub const MASTER_STEP: u32 = u32::MAX;

/// The order tracks render in, and where each takes its input from. Compiled on the UI thread.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RenderPlan {
    pub steps: Vec<Step>,
    /// Ranges of `steps` that depend on each other only through earlier levels.
    pub levels: Vec<Range<u32>>,
    /// Shared buffers the plan needs beyond the tracks' own. None yet.
    pub buses: u32,
    /// Audio routed to devices' sidechain inputs. None yet.
    pub sidechains: Vec<SidechainIn>,
}

/// One track's turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// An index into the engine's tracks, or [`MASTER_STEP`].
    pub track: u32,
    /// What it sums before its devices run, in order.
    pub inputs: Vec<BusIn>,
}

/// One input of a track.
#[derive(Debug, Clone, PartialEq)]
pub struct BusIn {
    /// An index into the engine's tracks.
    pub from: u32,
    pub tap: Tap,
    /// The cell in the receiving track's bus scaling this input. `None` is unity.
    pub gain: Option<CellIx>,
    /// Frames to hold the signal back, to line up paths of different latency.
    pub delay: u32,
}

impl BusIn {
    /// The finished output of a track, at unity.
    pub fn post_mixer(from: u32) -> Self {
        Self {
            from,
            tap: Tap::PostMixer,
            gain: None,
            delay: 0,
        }
    }
}

/// Where along a track, or a chain in it, a signal is picked up.
#[derive(Debug, Clone, PartialEq)]
pub enum Tap {
    PreFx,
    PostFx,
    PostMixer,
    /// A tap inside a chain of a rack, by the chain's id.
    Chain(ChainId, Box<Tap>),
    /// A device's extra output bus.
    Aux(DeviceId, u8),
}

/// A signal routed to a device's sidechain input.
#[derive(Debug, Clone, PartialEq)]
pub struct SidechainIn {
    pub device: DeviceId,
    pub from: u32,
    pub tap: Tap,
    pub delay: u32,
}

impl RenderPlan {
    /// Room for today's topology over any number of tracks, written without allocating.
    pub(crate) fn with_capacity(tracks: usize) -> Self {
        let mut plan = Self {
            steps: Vec::with_capacity(tracks + 1),
            levels: Vec::with_capacity(2),
            buses: 0,
            sidechains: Vec::new(),
        };
        plan.steps.push(Step {
            track: MASTER_STEP,
            inputs: Vec::with_capacity(tracks),
        });
        plan.levels.push(0..0);
        plan.levels.push(0..1);
        plan
    }

    /// Become today's topology, every track summed to the Master, over this many tracks. What the
    /// engine runs when no compiled plan applies. Allocates nothing.
    pub(crate) fn make_flat(&mut self, tracks: usize) {
        // Drop the track steps, which own nothing; the Master's keeps its reserved inputs.
        let last = self.steps.len() - 1;
        self.steps.drain(..last);
        let room = self.steps[0].inputs.capacity();
        let tracks = tracks.min(room).min(self.steps.capacity() - 1);
        self.steps[0].inputs.clear();
        self.steps[0]
            .inputs
            .extend((0..tracks as u32).map(BusIn::post_mixer));
        for track in 0..tracks as u32 {
            self.steps.insert(
                track as usize,
                Step {
                    track,
                    inputs: Vec::new(),
                },
            );
        }
        self.levels[0] = 0..tracks as u32;
        self.levels[1] = tracks as u32..tracks as u32 + 1;
    }

    /// Whether the engine can run this plan as it stands. It is written by the UI side, but the
    /// audio thread must not trust its indices, and refuses shapes it does not render yet.
    pub(crate) fn fits(&self, tracks: usize) -> bool {
        let tiled = self.levels.iter().try_fold(0, |end, level| {
            (level.start == end && level.end >= level.start).then_some(level.end)
        });
        let renderable = self.buses == 0 && self.sidechains.is_empty();
        if tiled != Some(self.steps.len() as u32) || !renderable || self.steps.len() != tracks + 1 {
            return false;
        }
        let once = |t: u32| self.steps.iter().filter(|s| s.track == t).count() == 1;
        self.steps.last().is_some_and(|s| s.track == MASTER_STEP)
            && (0..tracks as u32).all(once)
            && self.steps.iter().all(|s| {
                (s.track == MASTER_STEP || s.inputs.is_empty())
                    && s.inputs.iter().all(|i| {
                        (i.from as usize) < tracks
                            && i.tap == Tap::PostMixer
                            && i.gain.is_none()
                            && i.delay == 0
                    })
            })
    }
}

/// The engine's audio channels for one call. Channel 0 is the left output, 1 the right; any more
/// are silent for now.
pub struct AudioIo<'a> {
    pub inputs: &'a [&'a [f32]],
    pub outputs: &'a mut [&'a mut [f32]],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(track: u32, inputs: Vec<BusIn>) -> Step {
        Step { track, inputs }
    }

    fn plan(steps: Vec<Step>, levels: Vec<Range<u32>>) -> RenderPlan {
        RenderPlan {
            steps,
            levels,
            ..Default::default()
        }
    }

    fn good() -> RenderPlan {
        plan(
            vec![
                step(0, vec![]),
                step(1, vec![]),
                step(MASTER_STEP, vec![BusIn::post_mixer(0)]),
            ],
            vec![0..2, 2..3],
        )
    }

    #[test]
    fn a_path_is_as_long_as_its_steps() {
        let step = ChainStep {
            rack: DeviceId(1),
            chain: ChainId(2),
        };
        let mut path = ChainPath::ROOT;
        assert!(path.is_empty());
        for n in 1..=MAX_RACK_DEPTH {
            path = path.child(step).unwrap();
            assert_eq!(path.len(), n);
            assert_eq!(path.steps().len(), n);
        }
        assert_eq!(path.child(step), None, "no deeper than the limit");
    }

    #[test]
    fn the_flat_plan_keeps_every_track_in_order() {
        let mut p = RenderPlan::with_capacity(4);
        p.make_flat(3);
        assert_eq!(p.steps.len(), 4);
        assert_eq!(p.levels, [0..3, 3..4]);
        assert_eq!(p.steps[3].track, MASTER_STEP);
        assert_eq!(
            p.steps[3].inputs.iter().map(|i| i.from).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(p.fits(3));
        p.make_flat(0);
        assert_eq!(p.levels, [0..0, 0..1]);
        assert!(p.fits(0));
    }

    #[test]
    fn rewriting_the_flat_plan_does_not_allocate() {
        let mut p = RenderPlan::with_capacity(16);
        rlive_testkit::assert_no_alloc(|| {
            for n in [16, 3, 16, 0, 7] {
                p.make_flat(n);
            }
        });
        assert_eq!(p.steps.len(), 8);
    }

    #[test]
    fn a_plan_fits_only_the_tracks_it_was_compiled_for() {
        assert!(good().fits(2));
        assert!(!good().fits(1));
        assert!(!good().fits(3));
    }

    #[test]
    fn a_plan_must_tile_its_levels_and_end_at_the_master() {
        let mut p = good();
        p.levels = vec![0..1, 2..3];
        assert!(!p.fits(2), "a gap");
        p.levels = vec![0..1, 1..2];
        assert!(!p.fits(2), "a step missing");

        let mut p = good();
        p.steps.swap(0, 2);
        assert!(!p.fits(2), "the Master comes last");

        let mut p = good();
        p.steps[1].track = 0;
        assert!(!p.fits(2), "every track once");
    }

    #[test]
    fn a_plan_with_what_the_engine_cannot_render_yet_is_refused() {
        let mut p = good();
        p.steps[2].inputs[0].from = 9;
        assert!(!p.fits(2), "a track that isn't there");

        let mut p = good();
        p.steps[2].inputs[0].tap = Tap::PreFx;
        assert!(!p.fits(2));

        let mut p = good();
        p.steps[2].inputs[0].delay = 1;
        assert!(!p.fits(2));

        let mut p = good();
        p.steps[0].inputs.push(BusIn::post_mixer(1));
        assert!(!p.fits(2), "only the Master has inputs");

        let mut p = good();
        p.buses = 1;
        assert!(!p.fits(2));
    }
}
