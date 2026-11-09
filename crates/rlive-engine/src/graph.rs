//! The shape of a session: how to name a chain in the device tree, and how audio moves between
//! tracks each block.
//!
//! A [`RenderPlan`] is compiled on the UI thread (`rlive-app/src/session/graph.rs`) and handed to
//! the engine inside a [`GraphTxn`](crate::GraphTxn), together with the edits it was compiled for.

use crate::capture::InputSource;
use crate::params::CellIx;
use rlive_core::{ChainId, CrossfadeCurve, DeviceId, TrackId};
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

/// The order tracks render in, and where each takes its input from. Compiled on the UI thread.
///
/// A step's `track` indexes the engine's tracks, in the order `project.tracks`, then
/// `project.returns`; one past the last track is the Master, which is always the last step.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RenderPlan {
    pub steps: Vec<Step>,
    /// Ranges of `steps` that depend on each other only through earlier levels.
    pub levels: Vec<Range<u32>>,
    /// Shared buffers the plan needs beyond the tracks' own: one per entry of `tap_outs`, plus a
    /// feed for each sidechain.
    pub buses: u32,
    /// The signals tracks publish for others to read, sorted by track.
    pub tap_outs: Vec<TapOut>,
    /// Audio routed to devices' sidechain inputs.
    pub sidechains: Vec<SidechainIn>,
    /// MIDI routed between tracks, sorted by the receiving track.
    pub midi: Vec<MidiRoute>,
    /// What each track's own input stage does. Empty is the default for every track.
    pub io: Vec<TrackIo>,
    /// Tracks heard through hardware outputs of their own.
    pub hardware: Vec<HardwareOut>,
    /// Devices' extra outputs that go straight to hardware outputs.
    pub hardware_sends: Vec<HardwareSend>,
    /// Hardware inputs that feed devices' sidechain inputs.
    pub hardware_returns: Vec<HardwareReturn>,
    /// The hardware the Main output goes to.
    pub main_out: OutputPair,
    /// The output that Cue switches go to, when Solo/Cue is set to Cue.
    pub cue: Option<OutputPair>,
    /// The crossfader's curve.
    pub crossfade_curve: CrossfadeCurve,
    /// `(tapper, tapped)`: soloing the tapper keeps what feeds the tapped track, since a tap ahead
    /// of the tapped track's mixer has to go on hearing it (manual p. 382).
    pub solo_taps: Vec<(u32, u32)>,
    /// For each track, what solo should treat as containing it: the track its output feeds, or
    /// else its group. A soloed track keeps what contains it open, and a soloed container keeps
    /// what feeds it audible (manual p. 381). Empty is no groups at all.
    pub groups: Vec<Option<u32>>,
    /// Which tracks are return tracks: they sit at the end of the tracks, before the Master.
    pub returns: Range<u32>,
    /// Soloing a clip or group track leaves the return tracks audible.
    pub solo_in_place: bool,
    /// Frames to hold the metronome back so it stays with the tracks at the Master.
    pub click_delay: u32,
    /// Frames from the playhead to the Master's output, however the Master got its input. Not
    /// used to render: offline rendering and input monitoring need to know it.
    pub output_latency: u32,
    /// Where each track takes its audio input from, by track. Empty is no input at all.
    pub inputs: Vec<InputSource>,
}

/// One track's turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// An index into the engine's tracks; one past the last of them is the Master.
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
    /// The sender's send level for this input, a cell in its own bus. A send takes the sender's
    /// signal before solo silences its main output. `None` is its main output, at unity.
    pub gain: Option<CellIx>,
    /// Frames to hold the signal back, to line up paths of different latency.
    pub delay: u32,
    /// Takes what `from` produced a fixed time ago rather than this block, which is the only way
    /// a track can hear itself or a track rendering after it (a feedback send).
    pub feedback: bool,
    /// Reads a tap bus. Such an input is a track tapping another as its input, so it only comes
    /// through while the track is monitoring.
    pub bus: Option<BusRead>,
}

/// A place on a [`TapBus`](crate::taps::TapBus): which bus, and which of its readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusRead {
    pub bus: u32,
    pub reader: u16,
    /// A track's own input, which only comes through while it is monitoring.
    pub monitored: bool,
}

impl BusIn {
    /// The finished output of a track, at unity.
    pub fn post_mixer(from: u32) -> Self {
        Self {
            from,
            tap: Tap::PostMixer,
            gain: None,
            delay: 0,
            feedback: false,
            bus: None,
        }
    }

    /// Another track's signal as this track's input, read from a tap bus.
    pub fn tapped(from: u32, tap: Tap, read: BusRead) -> Self {
        Self {
            from,
            tap,
            gain: None,
            delay: 0,
            feedback: false,
            bus: Some(read),
        }
    }

    /// A send at the level in the sender's cell `level`, tapped before or after its mixer.
    pub fn send(from: u32, level: CellIx, pre_mixer: bool) -> Self {
        Self {
            from,
            tap: if pre_mixer {
                Tap::PostFx
            } else {
                Tap::PostMixer
            },
            gain: Some(level),
            delay: 0,
            feedback: false,
            bus: None,
        }
    }
}

/// Where along a track, or a chain in it, a signal is picked up.
#[derive(Debug, Clone, PartialEq)]
pub enum Tap {
    PreFx,
    PostFx,
    PostMixer,
    /// A tap inside a chain or return chain of a rack, by the chain's id.
    Chain(ChainId, Box<Tap>),
    /// A device's extra output bus. For a rack, the return chains routed to the Set's return
    /// tracks come out of slot `n` of the track's rack outputs.
    Aux(DeviceId, u8),
}

/// A signal one track publishes on a tap bus.
#[derive(Debug, Clone, PartialEq)]
pub struct TapOut {
    pub track: u32,
    pub tap: Tap,
    pub bus: u32,
}

/// A signal routed to a device's sidechain input.
#[derive(Debug, Clone, PartialEq)]
pub struct SidechainIn {
    pub device: DeviceId,
    /// The track holding the device. Rendered after `from`.
    pub to: u32,
    pub from: u32,
    pub tap: Tap,
    /// Frames to hold the signal back, so it reaches the device in step with what it keys.
    pub delay: u32,
    /// The tap bus it reads.
    pub read: BusRead,
    /// The bus the device reads, holding the signal after its delay.
    pub feed: u32,
    /// Reads a track that renders after the device's own, so it can only hear the past. Adds no
    /// dependency between the two.
    pub feedback: bool,
}

/// MIDI from one track to another, taken after MIDI effects and before any instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiRoute {
    pub from: u32,
    pub to: u32,
    /// `None` is Track In, which goes through monitoring. Otherwise a device in the receiving
    /// track, addressed directly.
    pub device: Option<DeviceId>,
    /// Sends every event on this channel, for a multi-timbral instrument.
    pub channel: Option<u8>,
}

/// When a track's input is heard through it (manual p. 374).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputGate {
    /// `Off`, or `Auto` on an unarmed track.
    #[default]
    Never,
    /// `Auto` on an armed track: while no clip is playing.
    WhenIdle,
    /// `In`: always, with the output of clips suppressed.
    Always,
}

/// What a track does at the top of its chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackIo {
    pub gate: InputGate,
    /// Frames to hold the clips back by, so they stay with its later inputs.
    pub own_delay: u32,
}

/// A hardware output: one channel, or the first of a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputPair {
    pub channel: u16,
    pub stereo: bool,
}

impl Default for OutputPair {
    fn default() -> Self {
        Self {
            channel: 0,
            stereo: true,
        }
    }
}

/// A tap bus that plays straight out of a hardware output, as External Audio Effect sends do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareSend {
    pub bus: u32,
    pub out: OutputPair,
}

/// Hardware inputs read into the sidechain input of a device, through an A11 input path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareReturn {
    pub device: DeviceId,
    /// The track holding the device.
    pub to: u32,
    pub channel: u16,
    pub stereo: bool,
    /// The bus the device reads.
    pub feed: u32,
}

/// A track whose output goes to its own hardware output instead of the Main output.
#[derive(Debug, Clone, PartialEq)]
pub struct HardwareOut {
    pub input: BusIn,
    pub out: OutputPair,
}

impl RenderPlan {
    /// Room for today's topology over any number of tracks, written without allocating.
    pub(crate) fn with_capacity(tracks: usize) -> Self {
        let mut plan = Self {
            steps: Vec::with_capacity(tracks + 1),
            levels: Vec::with_capacity(2),
            buses: 0,
            tap_outs: Vec::new(),
            sidechains: Vec::new(),
            midi: Vec::new(),
            io: Vec::new(),
            hardware: Vec::new(),
            hardware_sends: Vec::new(),
            hardware_returns: Vec::new(),
            main_out: OutputPair::default(),
            cue: None,
            crossfade_curve: CrossfadeCurve::default(),
            solo_taps: Vec::new(),
            groups: Vec::new(),
            returns: 0..0,
            solo_in_place: true,
            click_delay: 0,
            output_latency: 0,
            inputs: Vec::new(),
        };
        plan.steps.push(Step {
            track: 0,
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
        self.steps[0].track = tracks as u32;
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
        let bus_ok = |b: u32| b < self.buses;
        let wired = self.tap_outs.len() + self.sidechains.len() + self.hardware_returns.len()
            == self.buses as usize
            && self.tap_outs.iter().all(|t| bus_ok(t.bus))
            && self.hardware_sends.iter().all(|h| bus_ok(h.bus))
            && self
                .hardware_returns
                .iter()
                .all(|h| bus_ok(h.feed) && (h.to as usize) < tracks)
            && self
                .sidechains
                .iter()
                .all(|s| bus_ok(s.read.bus) && bus_ok(s.feed));
        if tiled != Some(self.steps.len() as u32)
            || !wired
            || tracks > crate::MAX_TRACKS
            || self.steps.len() != tracks + 1
            || self.returns.start > self.returns.end
            || self.returns.end > tracks as u32
            || !(self.io.is_empty() || self.io.len() == tracks)
            || !(self.inputs.is_empty() || self.inputs.len() == tracks)
        {
            return false;
        }
        let master = tracks as u32;
        if self.steps.last().map(|s| s.track) != Some(master) {
            return false;
        }
        let parents_ok = (self.groups.is_empty() || self.groups.len() == tracks)
            && self
                .groups
                .iter()
                .all(|g| g.is_none_or(|p| (p as usize) < tracks));
        if !parents_ok {
            return false;
        }
        // Every track exactly once, and the level it renders in, so an input can be checked
        // against what its source has rendered by then.
        let mut level_of = [u16::MAX; crate::MAX_TRACKS + 1];
        for (level, range) in self.levels.iter().enumerate() {
            for step in &self.steps[range.start as usize..range.end as usize] {
                match level_of.get_mut(step.track as usize) {
                    Some(l) if *l == u16::MAX => *l = level as u16,
                    _ => return false,
                }
            }
        }
        let inputs_fit = self.levels.iter().enumerate().all(|(level, range)| {
            self.steps[range.start as usize..range.end as usize]
                .iter()
                .all(|s| {
                    s.inputs.iter().all(|i| {
                        let from = i.from as usize;
                        let readable = match i.bus {
                            Some(read) => bus_ok(read.bus),
                            // Read straight off the source, a rack output can't be held back.
                            None => {
                                matches!(i.tap, Tap::PostMixer | Tap::PostFx | Tap::Aux(..))
                                    && !(matches!(i.tap, Tap::Aux(..)) && i.delay != 0)
                            }
                        };
                        from < tracks
                            && readable
                            // A feedback tap is already as late as it can be.
                            && (i.delay == 0 || !i.feedback)
                            // Rendered already, unless it is explicitly a feedback tap.
                            && (i.feedback || (level_of[from] as usize) < level)
                    })
                })
        });
        let before = |from: u32, to: u32| {
            (from as usize) < tracks
                && (to as usize) < tracks
                && level_of[from as usize] < level_of[to as usize]
        };
        inputs_fit
            && self.midi.iter().all(|m| before(m.from, m.to))
            && self.sidechains.iter().all(|s| {
                ((s.from as usize) < tracks && (s.to as usize) < tracks)
                    && (s.feedback || before(s.from, s.to))
            })
            && self.tap_outs.iter().all(|t| (t.track as usize) < tracks)
            && self
                .hardware
                .iter()
                .all(|h| (h.input.from as usize) < tracks)
    }

    /// The tap buses a track publishes, in `tap_outs`.
    pub(crate) fn taps_of(&self, track: u32) -> &[TapOut] {
        let start = self.tap_outs.partition_point(|t| t.track < track);
        let end = start + self.tap_outs[start..].partition_point(|t| t.track == track);
        &self.tap_outs[start..end]
    }

    /// The MIDI a track receives, in `midi`.
    pub(crate) fn midi_into(&self, track: u32) -> &[MidiRoute] {
        let start = self.midi.partition_point(|m| m.to < track);
        let end = start + self.midi[start..].partition_point(|m| m.to == track);
        &self.midi[start..end]
    }

    pub(crate) fn sidechains_into(&self, track: u32) -> impl Iterator<Item = &SidechainIn> {
        self.sidechains.iter().filter(move |s| s.to == track)
    }

    pub(crate) fn returns_into(&self, track: u32) -> impl Iterator<Item = &HardwareReturn> {
        self.hardware_returns.iter().filter(move |s| s.to == track)
    }

    pub(crate) fn input_of(&self, track: usize) -> InputSource {
        self.inputs.get(track).copied().unwrap_or_default()
    }

    pub(crate) fn io_of(&self, track: u32) -> TrackIo {
        self.io.get(track as usize).copied().unwrap_or_default()
    }
}

/// The engine's audio channels for one call. Which channels carry what is up to the plan: the
/// Main output is on its `main_out`, 0 and 1 unless told otherwise. An input channel may be
/// shorter than the block (or missing), which reads as silence.
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
                step(2, vec![BusIn::post_mixer(0)]),
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
        assert_eq!(p.steps[3].track, 3);
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
        p.steps[2].inputs[0].feedback = true;
        p.steps[2].inputs[0].delay = 1;
        assert!(!p.fits(2), "a feedback tap takes no delay");

        let mut p = good();
        p.steps[0].inputs.push(BusIn::post_mixer(1));
        assert!(!p.fits(2), "an input from a track in the same level");

        let mut p = good();
        p.buses = 1;
        assert!(!p.fits(2));
    }
}
