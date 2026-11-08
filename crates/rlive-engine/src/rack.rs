//! Racks: devices that hold parallel chains of devices, nested to any depth (manual ch. 25).
//!
//! A rack is a node of the device tree, not a [`Processor`](rlive_core::Processor), so a device
//! inside one is addressed and timed exactly like a device on a track. Its parameter cells live
//! in the owning track's flat bus: the rack owns its macros and Chain Selector, each chain owns
//! the cells of its mixer strip (volume, pan, activator, solo and sends to the return chains).
//!
//! Zones filter MIDI per chain. A note has to pass the chain's key, velocity and Chain Select
//! zones to reach it; fade ranges attenuate its velocity (p. 498). In an Instrument or Audio
//! Effect Rack the Chain Select fades attenuate the chain's output instead, and with no fade range
//! nothing is attenuated, so tails ring out on their own (p. 499). Drum Racks have no zones: a
//! chain answers its Receive note, playing its Play note (p. 501).

use crate::delay::{FadingDelay, StereoLine};
use crate::taps::TapSlots;
use crate::track::{run_chain, ChainRun, DeviceSlot, GainStage, Stretch, MAX_DEVICES, MAX_EVENTS};
use rlive_core::param::{chain as controls, rack as rack_params};
use rlive_core::{
    db_to_gain, ChainId, EventQueue, MidiEvent, MidiMessage, NoteId, ParamId, TrackId,
};
use std::ops::Range;

/// How many chains a rack can hold.
pub const MAX_CHAINS: usize = 128;
/// How many Chain Selector moves can land inside one stretch of a block.
const MAX_SELECT_POINTS: usize = 256;
/// How many times one chain can be choked inside one stretch of a block.
const MAX_CHOKES: usize = 32;

/// Which of Live's rack types this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RackKind {
    Instrument,
    Audio,
    Midi,
    Drum,
}

/// A range of MIDI values and the fade range around it, all 0..127.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    pub low: u8,
    pub high: u8,
    /// Where the fade below `low` starts, at or under `low`.
    pub fade_low: u8,
    /// Where the fade above `high` ends, at or over `high`.
    pub fade_high: u8,
}

impl Zone {
    pub const fn span(low: u8, high: u8) -> Self {
        Self {
            low,
            high,
            fade_low: low,
            fade_high: high,
        }
    }

    /// How much of something at `x` passes: 1 inside the zone, 0 outside it, and in the fade
    /// range a straight line that reaches 0 at the far edge.
    pub fn gain(&self, x: f32) -> f32 {
        let (low, high) = (self.low as f32, self.high as f32);
        let (fade_low, fade_high) = (self.fade_low as f32, self.fade_high as f32);
        if x >= low && x <= high {
            1.0
        } else if x < low {
            if x > fade_low && low > fade_low {
                (x - fade_low) / (low - fade_low)
            } else {
                0.0
            }
        } else if x < fade_high && fade_high > high {
            (fade_high - x) / (fade_high - high)
        } else {
            0.0
        }
    }

    /// The Chain Select gain for a rack that outputs audio: a side with no fade range is not
    /// attenuated at all, whatever the selector does (manual p. 499).
    fn output_gain(&self, x: f32) -> f32 {
        let beyond_fadeless = (x < self.low as f32 && self.fade_low >= self.low)
            || (x > self.high as f32 && self.fade_high <= self.high);
        if beyond_fadeless {
            1.0
        } else {
            self.gain(x)
        }
    }
}

/// The note a Drum Rack chain answers to and what it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrumPad {
    /// The note that triggers the chain; `None` is All Notes.
    pub receive: Option<u8>,
    /// The note sent to the devices in the chain.
    pub play: u8,
    /// Choke group, 1..16.
    pub choke: Option<u8>,
}

/// Which notes and Chain Selector values reach a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zones {
    pub key: Zone,
    pub velocity: Zone,
    pub select: Zone,
    /// Set on the chains of a Drum Rack.
    pub drum: Option<DrumPad>,
}

impl Default for Zones {
    fn default() -> Self {
        Self {
            key: Zone::span(0, 127),
            velocity: Zone::span(1, 127),
            select: Zone::span(0, 0),
            drum: None,
        }
    }
}

/// A chain's mixer strip, as it stands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChainMix {
    pub volume_db: f32,
    pub pan: f32,
    pub active: bool,
    pub solo: bool,
    /// Levels into the return chains, by position, in dB.
    pub sends: [f32; controls::MAX_RETURNS],
    /// On a return chain: the Set return track it feeds, instead of the rack's own output.
    pub output: Option<TrackId>,
}

impl Default for ChainMix {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            active: true,
            solo: false,
            sends: [crate::SEND_MIN_DB as f32; controls::MAX_RETURNS],
            output: None,
        }
    }
}

impl ChainMix {
    fn gains(&self) -> (f32, f32) {
        crate::MixSettings {
            volume_db: self.volume_db,
            pan: self.pan,
            ..Default::default()
        }
        .gains()
    }
}

/// One chain of a rack, or one return chain.
pub struct ChainNode {
    pub id: ChainId,
    pub devices: Vec<Box<DeviceSlot>>,
    pub zones: Zones,
    /// The cells of this chain's strip in its track's bus.
    pub(crate) cells: Range<u32>,
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    /// The rack's MIDI as this chain hears it.
    pub(crate) events: EventQueue,
    /// Holds what MIDI effects in this chain pass along.
    pub(crate) midi: [EventQueue; 2],
    /// Holds this chain back to line up with the slowest chain of its rack.
    pub(crate) line: Option<Box<StereoLine>>,
    pub(crate) delay: FadingDelay,
    mix: ChainMix,
    /// Volume and pan.
    fader: GainStage,
    /// The Chain Activator and solo.
    gate: GainStage,
    /// Open unless solo silences this chain's sends.
    send_gate: GainStage,
    /// Chain Select fades, for racks that output audio.
    select_gate: GainStage,
    /// Audio into the chain, shut when the selector leaves an Audio Effect Rack chain's zone.
    input_gate: GainStage,
    /// One level per return chain, by position.
    send_levels: Vec<GainStage>,
    /// The chain's post-fader signal, for its sends.
    tap: [Vec<f32>; 2],
    /// Notes this chain was let in on, by channel and key, so it gets their note-offs however
    /// its zones have changed since, and can release them when it is switched off.
    held: [u128; 16],
    /// Notes this chain turned away, by channel and key: their note-offs are the only ones it
    /// doesn't hear. A note it never saw, as when its rack was bypassed, still gets its note-off.
    refused: [u128; 16],
    /// The note each held key is playing in the chain, and its id.
    played: [(u8, NoteId); 128],
    /// The Chain Activator just went off here, so what the chain holds is to be released.
    release_at: Option<u32>,
    /// A note went in since the chain was last choked, so there can be sound to cut.
    sounding: bool,
    /// Frames in the coming stretch at which this chain is choked.
    chokes: Vec<u32>,
    /// Which tap buses this chain writes.
    pub(crate) taps: TapSlots,
}

impl ChainNode {
    /// Create a chain. Call on the UI thread: this allocates all buffers up front.
    pub fn new(id: ChainId, max_block: usize) -> Self {
        let mut send_levels = Vec::with_capacity(controls::MAX_RETURNS);
        for _ in 0..controls::MAX_RETURNS {
            let mut level = GainStage::new(max_block);
            level.seed((0.0, 0.0));
            send_levels.push(level);
        }
        Self {
            id,
            devices: Vec::with_capacity(MAX_DEVICES),
            zones: Zones::default(),
            cells: 0..0,
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            events: EventQueue::with_capacity(MAX_EVENTS),
            midi: [
                EventQueue::with_capacity(MAX_EVENTS),
                EventQueue::with_capacity(MAX_EVENTS),
            ],
            line: None,
            delay: FadingDelay::default(),
            mix: ChainMix::default(),
            fader: GainStage::unity(max_block),
            gate: GainStage::unity(max_block),
            send_gate: GainStage::unity(max_block),
            select_gate: GainStage::unity(max_block),
            input_gate: GainStage::unity(max_block),
            send_levels,
            tap: [vec![0.0; max_block], vec![0.0; max_block]],
            held: [0; 16],
            refused: [0; 16],
            played: [(0, 0); 128],
            release_at: None,
            sounding: false,
            chokes: Vec::with_capacity(MAX_CHOKES),
            taps: TapSlots::default(),
        }
    }

    /// Start with this strip, before the chain is sent to the engine.
    pub fn seed_mix(&mut self, mix: ChainMix) {
        self.mix = mix;
        if (mix.volume_db, mix.pan) != (0.0, 0.0) {
            self.fader.seed(mix.gains());
        }
        for (level, db) in self.send_levels.iter_mut().zip(mix.sends) {
            let gain = db_to_gain(db);
            level.seed((gain, gain));
        }
    }

    pub fn set_output(&mut self, output: Option<TrackId>) {
        self.mix.output = output;
    }

    fn take_mix_events(&mut self, at: &mut Stretch<'_>, changed: &mut Option<u32>) {
        for e in at.bus.take_segment(self.cells.start, at.from, at.to) {
            let value = e.value as f32;
            match e.id {
                controls::VOLUME => {
                    self.mix.volume_db = value;
                    self.fader.set_target(e.offset as usize, self.mix.gains());
                }
                controls::PAN => {
                    self.mix.pan = value;
                    self.fader.set_target(e.offset as usize, self.mix.gains());
                }
                controls::ACTIVATOR => {
                    let active = value >= 0.5;
                    if self.mix.active && !active && self.held != [0; 16] {
                        self.release_at.get_or_insert(e.offset);
                    }
                    self.mix.active = active;
                    *changed = Some(changed.map_or(e.offset, |o| o.min(e.offset)));
                }
                controls::SOLO => {
                    self.mix.solo = value >= 0.5;
                    *changed = Some(changed.map_or(e.offset, |o| o.min(e.offset)));
                }
                ParamId(id) => {
                    let send = id
                        .checked_sub(controls::FIRST_SEND.0)
                        .map(|n| n as usize)
                        .filter(|n| *n < controls::MAX_RETURNS);
                    if let Some(n) = send {
                        self.mix.sends[n] = value;
                        let gain = db_to_gain(value);
                        self.send_levels[n].set_target(e.offset as usize, (gain, gain));
                    }
                }
            }
        }
    }

    /// Note on for `(channel, key)`, playing `play`, so its note-off will find its way back here.
    fn mark_held(&mut self, channel: u8, key: u8, play: u8, note_id: NoteId) {
        self.held[channel as usize & 15] |= 1u128 << (key & 127);
        self.played[(key & 127) as usize] = (play, note_id);
        self.sounding = true;
    }

    /// The note `(channel, key)` is playing here, if this chain holds it, which it no longer will.
    fn release_held(&mut self, channel: u8, key: u8) -> Option<u8> {
        let slot = &mut self.held[channel as usize & 15];
        let bit = 1u128 << (key & 127);
        let was = *slot & bit != 0;
        *slot &= !bit;
        was.then(|| self.played[(key & 127) as usize].0)
    }

    /// The note `(channel, key)` is playing here, if this chain holds it.
    fn played_note(&self, channel: u8, key: u8) -> Option<u8> {
        let held = self.held[channel as usize & 15] & (1u128 << (key & 127)) != 0;
        held.then(|| self.played[(key & 127) as usize].0)
    }

    fn mark_refused(&mut self, channel: u8, key: u8) {
        self.refused[channel as usize & 15] |= 1u128 << (key & 127);
    }

    fn is_refused(&self, channel: u8, key: u8) -> bool {
        self.refused[channel as usize & 15] & (1u128 << (key & 127)) != 0
    }

    /// Whether this chain turned this note away, which ends with its note-off.
    fn take_refused(&mut self, channel: u8, key: u8) -> bool {
        let was = self.is_refused(channel, key);
        self.refused[channel as usize & 15] &= !(1u128 << (key & 127));
        was
    }

    /// The Chain Activator went off: everything this chain holds ends, at the frame it did.
    fn release_all(&mut self) {
        let Some(offset) = self.release_at.take() else {
            return;
        };
        for channel in 0..16u8 {
            let mut bits = std::mem::take(&mut self.held[channel as usize]);
            while bits != 0 {
                let key = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let (play, note_id) = self.played[key];
                self.events.push(MidiEvent::new(
                    offset,
                    MidiMessage::NoteOff {
                        channel,
                        key: play,
                        velocity: 0.5,
                        note_id,
                    },
                ));
            }
        }
        self.events.sort();
    }

    /// Whether a note is let into this chain, and at what velocity. See the zones in the module
    /// documentation for why a Chain Select fade only touches velocity in a MIDI Effect Rack.
    fn admit(&self, key: u8, velocity: f32, selector: f32, kind: RackKind) -> Option<f32> {
        let z = &self.zones;
        let on_scale = (velocity * 127.0).round().clamp(1.0, 127.0);
        let key_gain = z.key.gain(key as f32);
        let velocity_gain = z.velocity.gain(on_scale);
        let select_gain = z.select.gain(selector);
        if key_gain <= 0.0 || velocity_gain <= 0.0 || select_gain <= 0.0 {
            return None;
        }
        let fades = if kind == RackKind::Midi {
            select_gain
        } else {
            1.0
        };
        Some(velocity * key_gain * velocity_gain * fades)
    }

    fn silence(&mut self, frames: usize) {
        self.left[..frames].fill(0.0);
        self.right[..frames].fill(0.0);
    }

    /// The chain's devices, split wherever it gets choked. Returns which MIDI queue holds the
    /// chain's final MIDI, if there is a single one and a MIDI effect replaced it.
    fn run_devices(&mut self, frames: usize, at: &mut Stretch<'_>) -> Option<usize> {
        if self.chokes.is_empty() {
            return run_chain(
                &mut self.devices,
                ChainRun {
                    left: &mut self.left[..frames],
                    right: &mut self.right[..frames],
                    events: self.events.as_slice(),
                    midi: &mut self.midi,
                    at,
                },
            );
        }
        let (mut from, mut next_event) = (0usize, 0usize);
        for i in 0..=self.chokes.len() {
            let to = self
                .chokes
                .get(i)
                .map_or(frames, |&c| (c as usize).min(frames));
            if to > from {
                let events = self.events.as_mut_slice();
                let end =
                    next_event + events[next_event..].partition_point(|e| (e.offset as usize) < to);
                for e in &mut events[next_event..end] {
                    e.offset -= from as u32;
                }
                let mut transport = at.block.at(at.from as usize + from);
                transport.scale = at.transport.scale;
                let mut stretch = Stretch {
                    transport: &transport,
                    param_out: &mut *at.param_out,
                    bus: &mut *at.bus,
                    on_param: &mut *at.on_param,
                    block: at.block,
                    coeff: at.coeff,
                    rack_outs: at.rack_outs.as_deref_mut(),
                    from: at.from + from as u32,
                    to: at.from + to as u32,
                    buses: &mut *at.buses,
                    direct: at.direct,
                    merge: &mut *at.merge,
                };
                run_chain(
                    &mut self.devices,
                    ChainRun {
                        left: &mut self.left[from..to],
                        right: &mut self.right[from..to],
                        events: &self.events.as_slice()[next_event..end],
                        midi: &mut self.midi,
                        at: &mut stretch,
                    },
                );
                next_event = end;
                from = to;
            }
            if i < self.chokes.len() {
                // What is ringing stops here; whatever is triggered next starts clean.
                for d in self.devices.iter_mut() {
                    d.reset();
                }
                self.held = [0; 16];
                self.refused = [0; 16];
                self.sounding = false;
            }
        }
        self.chokes.clear();
        None
    }

    /// Run the latency line over this chain's output.
    fn hold_back(&mut self, frames: usize) {
        if let Some(line) = &mut self.line {
            line.push(&self.left[..frames], &self.right[..frames]);
            if !self.delay.is_straight() {
                self.delay
                    .read(line, &mut self.left[..frames], &mut self.right[..frames]);
            }
        }
    }

    pub(crate) fn clear_notes(&mut self) {
        self.held = [0; 16];
        self.refused = [0; 16];
        self.release_at = None;
        self.sounding = false;
        self.chokes.clear();
    }

    /// Publish one of this chain's points, as it stands now.
    fn publish(&self, bus: Option<u32>, at: &mut Stretch<'_>, frames: usize) {
        if let Some(bus) = bus {
            at.buses[bus as usize].write(
                at.from as usize,
                &self.left[..frames],
                &self.right[..frames],
            );
        }
    }

    /// Note a choke at `offset`, in order. Anything past what fits is not audible as such.
    fn choke(&mut self, offset: u32) {
        if self.chokes.len() < self.chokes.capacity() {
            self.chokes.push(offset);
        }
    }
}

/// The runtime of a rack device.
pub struct RackNode {
    pub kind: RackKind,
    pub chains: Vec<Box<ChainNode>>,
    pub returns: Vec<Box<ChainNode>>,
    /// The Chain Selector, as of the end of the last stretch.
    selector: f32,
    /// Chain Selector moves in the stretch being rendered: `(frame, value)`.
    select_points: Vec<(u32, f32)>,
    /// With this on the Chain Selector filters MIDI other than notes as well (p. 499).
    pub select_filters_ctrl: bool,
    /// Solo, an activator or the set of chains changed, so chains' gates need another look.
    gates_dirty: bool,
    /// Holds the chains back to line up with the slowest return chain.
    dry_line: Option<Box<StereoLine>>,
    dry_delay: FadingDelay,
}

impl RackNode {
    /// Create a rack with no chains yet. Call on the UI thread.
    pub fn new(kind: RackKind) -> Self {
        Self {
            kind,
            chains: Vec::with_capacity(MAX_CHAINS),
            returns: Vec::with_capacity(controls::MAX_RETURNS),
            selector: 0.0,
            select_points: Vec::with_capacity(MAX_SELECT_POINTS),
            select_filters_ctrl: false,
            gates_dirty: true,
            dry_line: None,
            dry_delay: FadingDelay::default(),
        }
    }

    /// Start with the Chain Selector here, before the rack is sent to the engine.
    pub fn seed_selector(&mut self, value: f32) {
        self.selector = value;
    }

    /// Start with every gate where the strips say it should be, before the rack is sent to the
    /// engine, so nothing fades in that was never audible.
    pub fn seed_gates(&mut self) {
        let solo = self.solo_state();
        let (selector, kind) = (self.selector, self.kind);
        let chains = self.chains.len();
        for (i, c) in self
            .chains
            .iter_mut()
            .chain(self.returns.iter_mut())
            .enumerate()
        {
            let (main, send) = gate_targets(c, solo, i >= chains);
            c.gate.seed((main, main));
            c.send_gate.seed((send, send));
            let g = select_output(c, selector, kind);
            c.select_gate.seed((g, g));
            let g = select_input(c, selector, kind);
            c.input_gate.seed((g, g));
        }
        self.gates_dirty = false;
    }

    /// Chains came or went, so solo has to be worked out again.
    pub(crate) fn invalidate_gates(&mut self) {
        self.gates_dirty = true;
    }

    /// Take over a line to hold the chains back with. Longer lines replace shorter ones, as
    /// [`crate::delay::offer_line`] does for a chain.
    pub(crate) fn dry_line_mut(&mut self) -> (&mut Option<Box<StereoLine>>, &mut FadingDelay) {
        (&mut self.dry_line, &mut self.dry_delay)
    }

    pub(crate) fn clear_notes(&mut self) {
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            c.clear_notes();
        }
    }

    fn solo_state(&self) -> SoloState {
        SoloState {
            any_chain: self.chains.iter().any(|c| c.mix.solo),
            any_return: self.returns.iter().any(|c| c.mix.solo),
        }
    }

    /// Read this rack's own cells. Chain Selector moves apply as they fall due; `selector` stays
    /// at the value in force at the start of the stretch until [`commit_selector`](Self::commit_selector).
    fn take_selector(&mut self, cell: u32, at: &mut Stretch<'_>) {
        self.select_points.clear();
        for e in at.bus.take_segment(cell, at.from, at.to) {
            if e.id == rack_params::CHAIN_SELECT && self.select_points.len() < MAX_SELECT_POINTS {
                self.select_points.push((e.offset, e.value as f32));
            }
        }
    }

    fn commit_selector(&mut self) {
        if let Some(&(_, value)) = self.select_points.last() {
            self.selector = value;
        }
    }

    /// Set the output and input gates for the selector's moves this stretch.
    fn follow_selector(&mut self, zones_changed: bool) {
        let kind = self.kind;
        if zones_changed {
            let selector = self.selector;
            for c in self.chains.iter_mut() {
                let g = select_output(c, selector, kind);
                c.select_gate.set_target(0, (g, g));
                let g = select_input(c, selector, kind);
                c.input_gate.set_target(0, (g, g));
            }
        }
        for &(offset, value) in &self.select_points {
            for c in self.chains.iter_mut() {
                let g = select_output(c, value, kind);
                c.select_gate.set_target(offset as usize, (g, g));
                let g = select_input(c, value, kind);
                c.input_gate.set_target(offset as usize, (g, g));
            }
        }
    }

    /// Point every chain's gates at what its switches and solo say.
    fn refresh_gates(&mut self, at_frame: usize) {
        let solo = self.solo_state();
        let chains = self.chains.len();
        for (i, c) in self
            .chains
            .iter_mut()
            .chain(self.returns.iter_mut())
            .enumerate()
        {
            let (main, send) = gate_targets(c, solo, i >= chains);
            c.gate.set_target(at_frame, (main, main));
            c.send_gate.set_target(at_frame, (send, send));
        }
    }

    /// Work out what MIDI each chain hears this stretch.
    fn route_midi(&mut self, events: &[MidiEvent]) {
        for c in self.chains.iter_mut() {
            c.events.clear();
        }
        match self.kind {
            RackKind::Audio => {
                for c in self.chains.iter_mut() {
                    c.events.extend(events);
                }
            }
            RackKind::Instrument | RackKind::Midi => self.route_through_zones(events),
            RackKind::Drum => self.route_to_pads(events),
        }
        for c in self.chains.iter_mut() {
            c.release_all();
        }
    }

    fn route_through_zones(&mut self, events: &[MidiEvent]) {
        let (kind, filters_ctrl) = (self.kind, self.select_filters_ctrl);
        let mut selector = self.selector;
        let mut point = 0;
        for e in events {
            while point < self.select_points.len() && self.select_points[point].0 <= e.offset {
                selector = self.select_points[point].1;
                point += 1;
            }
            let value = selector;
            match e.message {
                MidiMessage::NoteOn {
                    channel,
                    key,
                    velocity,
                    note_id,
                    tuning,
                } => {
                    for c in self.chains.iter_mut() {
                        let admitted = c
                            .mix
                            .active
                            .then(|| c.admit(key, velocity, value, kind))
                            .flatten();
                        let Some(v) = admitted else {
                            c.mark_refused(channel, key);
                            continue;
                        };
                        c.mark_held(channel, key, key, note_id);
                        c.events.push(MidiEvent::new(
                            e.offset,
                            MidiMessage::NoteOn {
                                channel,
                                key,
                                velocity: v,
                                note_id,
                                tuning,
                            },
                        ));
                    }
                }
                MidiMessage::NoteOff { channel, key, .. } => {
                    for c in self.chains.iter_mut() {
                        // A chain switched off has already let go of everything it held.
                        let held = c.release_held(channel, key).is_some();
                        if !c.take_refused(channel, key) && (held || c.mix.active) {
                            c.events.push(*e);
                        }
                    }
                }
                MidiMessage::NoteExpr { channel, key, .. } => {
                    for c in self.chains.iter_mut() {
                        if !c.is_refused(channel, key) {
                            c.events.push(*e);
                        }
                    }
                }
                _ => {
                    for c in self.chains.iter_mut() {
                        if !filters_ctrl || c.zones.select.gain(value) > 0.0 {
                            c.events.push(*e);
                        }
                    }
                }
            }
        }
    }

    fn route_to_pads(&mut self, events: &[MidiEvent]) {
        for e in events {
            match e.message {
                MidiMessage::NoteOn {
                    channel,
                    key,
                    note_id,
                    ..
                } => {
                    self.trigger_pads(e, channel, key, note_id);
                }
                MidiMessage::NoteOff { channel, key, .. } => {
                    for c in self.chains.iter_mut() {
                        if let Some(play) = c.release_held(channel, key) {
                            c.events.push(with_key(*e, play));
                        }
                    }
                }
                MidiMessage::NoteExpr { channel, key, .. } => {
                    for c in self.chains.iter_mut() {
                        if let Some(play) = c.played_note(channel, key) {
                            c.events.push(with_key(*e, play));
                        }
                    }
                }
                _ => {
                    for c in self.chains.iter_mut() {
                        c.events.push(*e);
                    }
                }
            }
        }
    }

    /// Fire the chains that answer this note. Chains that fired together never choke each other,
    /// so a Multi pad holds together; the rest of each choke group is silenced.
    fn trigger_pads(&mut self, event: &MidiEvent, channel: u8, key: u8, note_id: NoteId) {
        let mut groups = 0u32;
        for c in self.chains.iter_mut() {
            if !c.mix.active || !answers(c.zones.drum, key) {
                continue;
            }
            let play = pad_note(c.zones.drum, key);
            c.mark_held(channel, key, play, note_id);
            c.events.push(with_key(*event, play));
            if let Some(g) = c.zones.drum.and_then(choke_group) {
                groups |= 1 << g;
            }
        }
        if groups == 0 {
            return;
        }
        for c in self.chains.iter_mut() {
            let Some(g) = c.zones.drum.and_then(choke_group) else {
                continue;
            };
            if groups & (1 << g) != 0 && !answers(c.zones.drum, key) && c.sounding {
                c.choke(event.offset);
            }
        }
    }

    /// Run the rack over a stretch of a block.
    pub(crate) fn run(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        events: &[MidiEvent],
        events_out: &mut EventQueue,
        cell: u32,
        at: &mut Stretch<'_>,
    ) {
        self.take_selector(cell, at);
        let forward_midi = self.kind == RackKind::Midi;
        if self.chains.is_empty() && self.returns.is_empty() {
            if forward_midi {
                events_out.extend(events);
            }
            self.commit_selector();
            return;
        }
        let frames = left.len();
        let mut gate_change = None;
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            c.take_mix_events(at, &mut gate_change);
        }
        if std::mem::take(&mut self.gates_dirty) {
            self.follow_selector(true);
            self.refresh_gates(0);
        } else {
            self.follow_selector(false);
        }
        if let Some(offset) = gate_change {
            self.refresh_gates(offset as usize);
        }
        self.route_midi(events);
        self.commit_selector();

        let audio_filters = self.kind == RackKind::Audio;
        for ret in self.returns.iter_mut() {
            ret.silence(frames);
            ret.events.clear();
        }
        for chain in self.chains.iter_mut() {
            let chain = &mut **chain;
            chain.left[..frames].copy_from_slice(left);
            chain.right[..frames].copy_from_slice(right);
            if audio_filters {
                chain.input_gate.apply_unless_unity(
                    &mut chain.left[..frames],
                    &mut chain.right[..frames],
                    frames,
                    at.coeff,
                );
            }
            chain.publish(chain.taps.pre, at, frames);
            let held = chain.run_devices(frames, at);
            if forward_midi {
                let output = held.map_or(chain.events.as_slice(), |q| chain.midi[q].as_slice());
                if chain.mix.active {
                    events_out.extend(output);
                } else {
                    // Off, it starts nothing, but what it started before still has to end.
                    for e in output {
                        if matches!(e.message, MidiMessage::NoteOff { .. }) {
                            events_out.push(*e);
                        }
                    }
                }
            }
            chain.hold_back(frames);
            chain.publish(chain.taps.post_fx, at, frames);
            chain.fader.apply_unless_unity(
                &mut chain.left[..frames],
                &mut chain.right[..frames],
                frames,
                at.coeff,
            );
            if !self.returns.is_empty() {
                send_to_returns(chain, &mut self.returns, frames, at.coeff);
            }
            chain.gate.apply_unless_unity(
                &mut chain.left[..frames],
                &mut chain.right[..frames],
                frames,
                at.coeff,
            );
            chain.select_gate.apply_unless_unity(
                &mut chain.left[..frames],
                &mut chain.right[..frames],
                frames,
                at.coeff,
            );
            chain.publish(chain.taps.post_mix, at, frames);
        }

        left.fill(0.0);
        right.fill(0.0);
        for chain in self.chains.iter() {
            add(left, right, &chain.left[..frames], &chain.right[..frames]);
        }

        if let Some(line) = &mut self.dry_line {
            line.push(left, right);
            if !self.dry_delay.is_straight() {
                self.dry_delay.read(line, left, right);
            }
        }

        for ret in self.returns.iter_mut() {
            let ret = &mut **ret;
            ret.publish(ret.taps.pre, at, frames);
            ret.run_devices(frames, at);
            ret.hold_back(frames);
            ret.publish(ret.taps.post_fx, at, frames);
            ret.fader.apply_unless_unity(
                &mut ret.left[..frames],
                &mut ret.right[..frames],
                frames,
                at.coeff,
            );
            ret.gate.apply_unless_unity(
                &mut ret.left[..frames],
                &mut ret.right[..frames],
                frames,
                at.coeff,
            );
            ret.publish(ret.taps.post_mix, at, frames);
            let (l, r) = (&ret.left[..frames], &ret.right[..frames]);
            let outs = at.rack_outs.as_deref_mut();
            match (ret.mix.output, outs) {
                (Some(track), Some(outs)) if outs.has(track) => outs.add(track, l, r),
                _ => add(left, right, l, r),
            }
        }
    }
}

#[derive(Clone, Copy)]
struct SoloState {
    any_chain: bool,
    any_return: bool,
}

/// Whether a chain is heard, and whether its sends are. A soloed chain leaves the return chains
/// on, the way a soloed track leaves return tracks on; a soloed return chain keeps hearing every
/// send, as a soloed return track does.
fn gate_targets(c: &ChainNode, solo: SoloState, is_return: bool) -> (f32, f32) {
    let main = if is_return {
        c.mix.active && (c.mix.solo || !solo.any_return)
    } else {
        c.mix.active && (c.mix.solo || !(solo.any_chain || solo.any_return))
    };
    let send = c.mix.active && (c.mix.solo || !solo.any_chain || solo.any_return);
    (main as u8 as f32, send as u8 as f32)
}

/// The Chain Select gain at a chain's output in a rack that outputs audio.
fn select_output(c: &ChainNode, selector: f32, kind: RackKind) -> f32 {
    match kind {
        RackKind::Instrument | RackKind::Audio => c.zones.select.output_gain(selector),
        RackKind::Midi | RackKind::Drum => 1.0,
    }
}

/// How open an Audio Effect Rack chain's input is at this selector value.
fn select_input(c: &ChainNode, selector: f32, kind: RackKind) -> f32 {
    if kind == RackKind::Audio && c.zones.select.gain(selector) <= 0.0 {
        0.0
    } else {
        1.0
    }
}

fn send_to_returns(
    chain: &mut ChainNode,
    returns: &mut [Box<ChainNode>],
    frames: usize,
    coeff: f32,
) {
    let ChainNode {
        left,
        right,
        tap,
        send_gate,
        send_levels,
        ..
    } = chain;
    let [tap_l, tap_r] = tap;
    tap_l[..frames].copy_from_slice(&left[..frames]);
    tap_r[..frames].copy_from_slice(&right[..frames]);
    send_gate.apply_unless_unity(&mut tap_l[..frames], &mut tap_r[..frames], frames, coeff);
    for (n, ret) in returns.iter_mut().enumerate() {
        if let Some(level) = send_levels.get_mut(n) {
            level.add(
                (&tap_l[..frames], &tap_r[..frames]),
                (&mut ret.left[..frames], &mut ret.right[..frames]),
                frames,
                coeff,
            );
        }
    }
}

fn add(left: &mut [f32], right: &mut [f32], from_left: &[f32], from_right: &[f32]) {
    for (out, s) in left.iter_mut().zip(from_left) {
        *out += s;
    }
    for (out, s) in right.iter_mut().zip(from_right) {
        *out += s;
    }
}

/// Whether a chain with this pad answers `key`. A chain with no pad answers every note.
fn answers(pad: Option<DrumPad>, key: u8) -> bool {
    match pad.and_then(|p| p.receive) {
        Some(receive) => receive == key,
        None => true,
    }
}

/// The note a chain plays for a received `key`. All Notes passes it through as it came.
fn pad_note(pad: Option<DrumPad>, key: u8) -> u8 {
    match pad {
        Some(p) if p.receive.is_some() => p.play,
        _ => key,
    }
}

/// The chain's choke group as a bit position. All Notes disables choking (p. 501).
fn choke_group(pad: DrumPad) -> Option<u32> {
    pad.receive?;
    pad.choke.filter(|g| (1..=16).contains(g)).map(u32::from)
}

/// `event` with its note replaced by `key`.
fn with_key(mut event: MidiEvent, new_key: u8) -> MidiEvent {
    match &mut event.message {
        MidiMessage::NoteOn { key, .. }
        | MidiMessage::NoteOff { key, .. }
        | MidiMessage::NoteExpr { key, .. } => *key = new_key,
        _ => {}
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fade(fade_low: u8, low: u8, high: u8, fade_high: u8) -> Zone {
        Zone {
            low,
            high,
            fade_low,
            fade_high,
        }
    }

    #[test]
    fn a_zone_passes_its_range_and_nothing_else() {
        let z = Zone::span(10, 20);
        assert_eq!(z.gain(9.0), 0.0);
        assert_eq!(z.gain(10.0), 1.0);
        assert_eq!(z.gain(20.0), 1.0);
        assert_eq!(z.gain(20.5), 0.0);
    }

    #[test]
    fn a_fade_range_ramps_to_nothing_at_its_far_edge() {
        let z = fade(0, 8, 8, 16);
        assert_eq!(z.gain(0.0), 0.0, "a far edge is silent");
        assert_eq!(z.gain(4.0), 0.5);
        assert_eq!(z.gain(8.0), 1.0);
        assert_eq!(z.gain(12.0), 0.5);
        assert_eq!(z.gain(16.0), 0.0);
        assert_eq!(z.gain(17.0), 0.0);
    }

    #[test]
    fn crossfading_zones_add_up_where_they_cross() {
        // The manual's preset bank: core values 8 apart, sharing the fade range between them.
        let a = fade(0, 0, 0, 8);
        let b = fade(0, 8, 8, 16);
        assert_eq!(a.gain(8.0) + b.gain(8.0), 1.0);
        for x in 0..=8 {
            let x = x as f32;
            assert!((a.gain(x) + b.gain(x) - 1.0).abs() < 1e-6, "at {x}");
        }
    }

    #[test]
    fn output_gain_leaves_a_side_with_no_fade_alone() {
        let z = fade(4, 8, 8, 8);
        assert_eq!(z.output_gain(0.0), 0.0, "past the fade below");
        assert_eq!(z.output_gain(6.0), 0.5, "in the fade below");
        assert_eq!(z.output_gain(100.0), 1.0, "no fade above, no attenuation");
    }
}
