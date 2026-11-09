//! Keeping time with the outside world: MIDI Clock in and out, MIDI Timecode in, Tempo Follower
//! and Ableton Link. All of it runs on the audio thread and allocates nothing.
//!
//! An external tempo takes over the tempo map with [`TempoMap::flatten`], which leaves the
//! engine's map in place but one tempo long until the session sends a fresh one. Position is held
//! with a small rate trim, so the tempo the song plays at stays what the source says it is.
//!
//! [`TempoMap::flatten`]: rlive_core::TempoMap::flatten

use crate::{BlockTime, Engine, JumpKind};
use rlive_core::sync::{
    self, ClockEvent, ClockFollower, FrameRate, MtcDecoder, SyncMessage, TICKS_PER_CLOCK,
    TICKS_PER_MIDI_BEAT,
};
use rlive_core::tempo_follower::TempoFollower;
use rlive_core::PPQ;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

/// How many ports can receive clock at once.
pub const MAX_CLOCK_PORTS: usize = 16;

/// Beyond this far from the source, in ticks, the transport jumps rather than catching up.
const RELOCATE_TICKS: f64 = PPQ as f64 / 2.0;
/// How much the rate may be trimmed to catch up.
const MAX_TRIM: f64 = 0.03;
/// How hard a phase error is corrected, in rate per second of error.
const TRIM_GAIN: f64 = 2.0;
/// Without a quarter frame for this long, timecode has stopped.
const MTC_TIMEOUT: f64 = 0.25;
/// How far a tempo has to move to count as a change for recording.
const TEMPO_EPSILON: f64 = 0.02;
/// A clock on a block boundary belongs to one block, whatever float error says.
const CLOCK_EPSILON: f64 = 1e-6;

/// What the transport is slaved to, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ExternalSync {
    #[default]
    Off,
    /// MIDI Clock. `delay` is the Sync Delay in seconds: positive holds the song behind it.
    Clock { delay: f64 },
    /// MIDI Timecode. `rate` is `None` for SMPTE All, which reads the rate from the stream.
    /// `offset` is the timecode at the Arrangement's start, in seconds.
    Timecode {
        rate: Option<FrameRate>,
        offset: f64,
        delay: f64,
    },
}

/// A port sending MIDI Clock. `delay` is its Sync Delay in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ClockPort {
    pub port: u16,
    pub delay: f64,
}

/// How the engine should keep time. Changing it replaces the whole lot.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SyncConfig {
    pub external: ExternalSync,
    /// The audio input channel Tempo Follower listens to, while it is on.
    pub follower: Option<u16>,
    /// Whether what it hears sets the tempo. Off while Tempo Leader clips hold it.
    pub follower_drives: bool,
    pub clock_ports: [ClockPort; MAX_CLOCK_PORTS],
    pub clock_port_count: u8,
}

impl SyncConfig {
    pub fn outputs(&self) -> &[ClockPort] {
        &self.clock_ports[..self.clock_port_count as usize]
    }

    pub fn add_output(&mut self, port: ClockPort) {
        if (self.clock_port_count as usize) < MAX_CLOCK_PORTS {
            self.clock_ports[self.clock_port_count as usize] = port;
            self.clock_port_count += 1;
        }
    }
}

/// When a sync message arrived.
#[derive(Debug, Clone, Copy)]
pub enum SyncStamp {
    /// As stamped by the MIDI thread.
    At(Instant),
    /// On the engine's own frame count, for tests and offline use.
    Frame(u64),
}

/// A clock message on its way to a hardware port.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HwSync {
    pub port: u16,
    /// When the block it belongs to began.
    pub block: Instant,
    /// How far into that block it falls, in seconds, including the port's delay.
    pub offset: f64,
    pub bytes: [u8; 3],
    pub len: u8,
    /// Where it falls on the engine's frame count, before the port's delay.
    pub frame: u64,
}

/// What a Link session is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LinkState {
    pub tempo: f64,
    /// Where the session is within the quantum, in beats: `0..quantum`.
    pub phase: f64,
    pub peers: u32,
    /// The session's transport, when Start Stop Sync is on and anyone has said so.
    pub playing: Option<bool>,
}

/// The audio-thread side of a Link session. Implementations must not allocate, block or lock in
/// [`capture`](Self::capture).
pub trait LinkClock: Send {
    /// The session's state for the current block, `latency` seconds ahead of now.
    fn capture(&mut self, quantum: f64, latency: f64) -> LinkState;
    /// Tell the session we changed the tempo. The last one to do that wins.
    fn set_tempo(&mut self, bpm: f64);
    /// Tell the session the transport started or stopped.
    fn set_playing(&mut self, playing: bool);
}

/// A [`LinkClock`] as one object, so that it can go back as ordinary garbage.
pub struct LinkHolder(pub Box<dyn LinkClock>);

/// The engine's sync state.
pub(crate) struct SyncState {
    pub(crate) config: SyncConfig,
    pub(crate) clock_in: ClockFollower,
    mtc: MtcDecoder,
    mtc_last: Option<f64>,
    /// Sync messages waiting to be heard, from the MIDI thread.
    pub(crate) inbox: rtrb::Consumer<(SyncMessage, SyncStamp)>,
    /// Sent by [`SyncSender`].
    pub(crate) outbox: rtrb::Producer<HwSync>,
    follower: Box<TempoFollower>,
    /// The tempo an override or external source has fixed the song at.
    pub(crate) tempo_override: Option<f64>,
    last_marked_tempo: f64,
    /// Rate trim, while catching up with an external position.
    trim: f64,
    /// Phase Nudge, as a rate ratio.
    pub(crate) nudge: f64,
    /// Sending clock: whether the transport was running after the last block.
    was_playing: bool,
    /// The playhead jumped since clock last went out.
    relocated: bool,
    pub(crate) link: Option<Box<LinkHolder>>,
    /// Play was pressed with a Link peer around: waiting for the phase to come round.
    pub(crate) link_waiting: bool,
    link_state: LinkState,
    /// Last tempo Link was told of, or told us. Zero until the session has said one.
    pub(crate) link_tempo: f64,
    was_link_playing: Option<bool>,
}

impl SyncState {
    pub(crate) fn new(
        sample_rate: f64,
        inbox: rtrb::Consumer<(SyncMessage, SyncStamp)>,
        outbox: rtrb::Producer<HwSync>,
    ) -> Self {
        Self {
            config: SyncConfig::default(),
            clock_in: ClockFollower::default(),
            mtc: MtcDecoder::default(),
            mtc_last: None,
            inbox,
            outbox,
            follower: Box::new(TempoFollower::new(sample_rate)),
            tempo_override: None,
            last_marked_tempo: 0.0,
            trim: 1.0,
            nudge: 1.0,
            was_playing: false,
            relocated: false,
            link: None,
            link_waiting: false,
            link_state: LinkState::default(),
            link_tempo: 0.0,
            was_link_playing: None,
        }
    }

    /// Whether anything outside is deciding where the song is.
    pub(crate) fn externally_timed(&self) -> bool {
        !matches!(self.config.external, ExternalSync::Off)
    }

    pub(crate) fn link_enabled(&self) -> bool {
        self.link.is_some()
    }

    pub(crate) fn note_jump(&mut self) {
        self.relocated = true;
    }

    /// The ratio time runs at for the next block: nudge and catching up.
    pub(crate) fn rate(&self) -> f64 {
        self.nudge * self.trim
    }

    pub(crate) fn wants_input(&self) -> Option<u16> {
        self.config.follower
    }
}

impl Engine {
    /// Fix the tempo at `bpm` for the whole song until it is released, in place.
    pub(crate) fn apply_tempo_override(&mut self, bpm: f64) {
        self.sync.tempo_override = Some(bpm);
        if let Some(maps) = Arc::get_mut(&mut self.maps) {
            maps.tempo.flatten(bpm);
        }
        if (bpm - self.sync.last_marked_tempo).abs() > TEMPO_EPSILON {
            self.sync.last_marked_tempo = bpm;
            self.mark_tempo_change();
        }
    }

    /// The Tempo Leader or Set tempo going back to being what the map says. The session follows
    /// this with the map itself.
    pub(crate) fn clear_tempo_override(&mut self) {
        self.sync.tempo_override = None;
        self.sync.trim = 1.0;
    }

    pub(crate) fn set_sync_config(&mut self, config: SyncConfig) {
        let was_external = self.sync.externally_timed();
        let follower_drove =
            self.sync.config.follower.is_some() && self.sync.config.follower_drives;
        let follower_drives = config.follower.is_some() && config.follower_drives;
        self.sync.config = config;
        self.sync.mtc.reset();
        self.sync.mtc_last = None;
        self.sync.trim = 1.0;
        self.sync.clock_in = ClockFollower::default();
        if config.follower.is_none() {
            self.sync.follower.reset();
        }
        self.shared.external_tempo.store(0.0);
        if (was_external && !self.sync.externally_timed()) || (follower_drove && !follower_drives) {
            self.clear_tempo_override();
        }
    }

    pub(crate) fn tempo_from_link(&mut self, bpm: f64) {
        self.sync.link_tempo = bpm;
        self.apply_tempo_override(bpm);
    }

    /// What arrived from the sync ports since the last block.
    pub(crate) fn drain_sync(&mut self) {
        let now = self.clock as f64 / self.sample_rate;
        while let Ok((message, stamp)) = self.sync.inbox.pop() {
            let at = match stamp {
                SyncStamp::At(i) => now - i.elapsed().as_secs_f64(),
                SyncStamp::Frame(f) => f as f64 / self.sample_rate,
            };
            match self.sync.config.external {
                ExternalSync::Off => {}
                ExternalSync::Clock { .. } => self.heard_clock(message, at),
                ExternalSync::Timecode { .. } => self.heard_timecode(message, at),
            }
        }
    }

    /// Where the song should be for a clock position: held behind by Sync Delay, and around the
    /// loop if there is one, as the host doesn't know about it.
    fn clock_target(&self, position: f64) -> f64 {
        let delay = match self.sync.config.external {
            ExternalSync::Clock { delay } => delay,
            _ => 0.0,
        };
        let target = position - delay * self.maps.tempo.bpm_at(self.tick) / 60.0 * PPQ as f64;
        self.wrap_into_loop(target.max(0.0))
    }

    fn wrap_into_loop(&self, tick: f64) -> f64 {
        match self.loop_range {
            Some((start, end)) if tick >= end && end > start => {
                start + (tick - start).rem_euclid(end - start)
            }
            _ => tick,
        }
    }

    fn heard_clock(&mut self, message: SyncMessage, at: f64) {
        let event = self.sync.clock_in.feed(message, at);
        if let Some(bpm) = self.sync.clock_in.tempo() {
            self.shared.external_tempo.store(bpm);
            self.apply_tempo_override(bpm);
        }
        match event {
            Some(ClockEvent::Started { position }) => {
                let to = self.clock_target(position);
                self.jump(to, JumpKind::Seek);
                self.sync.link_waiting = false;
                if !self.playing {
                    self.count_in = None;
                    self.begin_playing();
                }
            }
            Some(ClockEvent::Stopped) => self.stop_where_it_is(),
            Some(ClockEvent::Located { position }) => {
                let to = self.clock_target(position);
                self.jump(to, JumpKind::Seek);
                self.insert_marker = to;
            }
            None => {}
        }
    }

    fn heard_timecode(&mut self, message: SyncMessage, at: f64) {
        let ExternalSync::Timecode {
            rate,
            offset,
            delay,
        } = self.sync.config.external
        else {
            return;
        };
        let timeline = |tc: sync::Timecode, late: f64| {
            let rate = rate.unwrap_or(tc.rate);
            tc.seconds_at(rate) + late / rate.fps() - offset - delay
        };
        match message {
            SyncMessage::QuarterFrame(data) => {
                self.sync.mtc_last = Some(at);
                if let Some(tc) = self.sync.mtc.feed(data) {
                    // The time is that of the first quarter frame, 7 of them ago.
                    self.follow_timecode(timeline(tc, 1.75), at);
                }
            }
            SyncMessage::FullFrame(tc) => {
                let to = self.maps.tempo.seconds_to_tick(timeline(tc, 0.0).max(0.0));
                self.jump(to, JumpKind::Seek);
                self.insert_marker = to;
                self.sync.mtc.reset();
            }
            _ => {}
        }
    }

    fn follow_timecode(&mut self, seconds: f64, at: f64) {
        if seconds < 0.0 {
            return;
        }
        let tempo = &self.maps.tempo;
        let now = self.clock as f64 / self.sample_rate;
        // Where the song was when this quarter frame arrived.
        let target = tempo.seconds_to_tick(seconds + (now - at).max(0.0));
        let err = target - self.tick;
        let per_second = tempo.bpm_at(self.tick) / 60.0 * PPQ as f64;
        if !self.playing {
            self.jump(target, JumpKind::Seek);
            self.count_in = None;
            self.begin_playing();
        } else if err.abs() > RELOCATE_TICKS {
            self.jump(target, JumpKind::Seek);
            self.sync.trim = 1.0;
        } else {
            self.sync.trim = 1.0 + (err / per_second * TRIM_GAIN).clamp(-MAX_TRIM, MAX_TRIM);
        }
    }

    /// Stop the transport, leaving the playhead where the source left it.
    fn stop_where_it_is(&mut self) {
        if self.playing {
            self.insert_marker = self.tick;
            self.stop_transport();
        }
    }

    /// Everything that keeps the song in step, at the start of a block.
    pub(crate) fn run_sync(&mut self) {
        let now = self.clock as f64 / self.sample_rate;
        match self.sync.config.external {
            ExternalSync::Clock { .. } => self.chase_clock(now),
            ExternalSync::Timecode { .. } => {
                if self.playing && self.sync.mtc_last.is_some_and(|t| now - t > MTC_TIMEOUT) {
                    self.sync.mtc_last = None;
                    self.stop_where_it_is();
                    self.sync.trim = 1.0;
                }
            }
            ExternalSync::Off => {}
        }
        self.run_link();
    }

    fn chase_clock(&mut self, now: f64) {
        self.sync.trim = 1.0;
        if !self.playing || !self.sync.clock_in.is_running() || !self.sync.clock_in.is_alive(now) {
            return;
        }
        let Some(position) = self.sync.clock_in.position_at(now) else {
            return;
        };
        let target = self.clock_target(position);
        let per_second = self.maps.tempo.bpm_at(self.tick) / 60.0 * PPQ as f64;
        let mut err = target - self.tick;
        if let Some((start, end)) = self.loop_range {
            // Around a loop the short way round is the true distance.
            let len = end - start;
            if len > 0.0 && err.abs() > len / 2.0 {
                err -= len * err.signum();
            }
        }
        if err.abs() > RELOCATE_TICKS {
            self.jump(target, JumpKind::Seek);
        } else {
            self.sync.trim = 1.0 + (err / per_second * TRIM_GAIN).clamp(-MAX_TRIM, MAX_TRIM);
        }
    }

    /// The Follower, hearing its input. Excludes External Sync, by the session's say-so.
    pub(crate) fn run_follower(&mut self, inputs: &[&[f32]], total: usize) {
        let Some(channel) = self.sync.wants_input() else {
            self.shared
                .follower_connected
                .store(false, Ordering::Relaxed);
            return;
        };
        let Some(input) = inputs.get(channel as usize) else {
            self.shared
                .follower_connected
                .store(false, Ordering::Relaxed);
            return;
        };
        self.shared
            .follower_connected
            .store(true, Ordering::Relaxed);
        let input = &input[..input.len().min(total)];
        if let Some(bpm) = self.sync.follower.feed(input) {
            self.shared.follower_tempo.store(bpm);
            if self.sync.config.follower_drives && !self.sync.externally_timed() {
                self.apply_tempo_override(bpm);
                self.tell_link_tempo(bpm);
            }
        }
    }

    /// Tell a Link session about a tempo that came from somewhere else than Link.
    fn tell_link_tempo(&mut self, bpm: f64) {
        if let Some(link) = self.sync.link.as_mut() {
            if (bpm - self.sync.link_tempo).abs() > TEMPO_EPSILON {
                self.sync.link_tempo = bpm;
                link.0.set_tempo(bpm);
            }
        }
    }

    fn run_link(&mut self) {
        if self.sync.link.is_none() {
            self.shared.link_peers.store(0, Ordering::Relaxed);
            return;
        }
        let quantum = f64::from(self.maps.bars.signature_at(self.tick).numerator.max(1))
            * self.maps.bars.signature_at(self.tick).ticks_per_beat() as f64
            / PPQ as f64;
        let Some(link) = self.sync.link.as_mut() else {
            return;
        };
        let state = link.0.capture(quantum, 0.0);
        let before = self.sync.link_state;
        self.sync.link_state = state;
        self.shared.link_peers.store(state.peers, Ordering::Relaxed);
        self.shared
            .link_phase
            .store((state.phase / quantum.max(1e-6)).rem_euclid(1.0));

        // A session we started alone begins at our tempo and leaves the map be. After that, the
        // session's tempo is the tempo: when anyone has changed it, it overrides the map.
        if self.sync.link_tempo == 0.0 && state.peers == 0 {
            self.sync.link_tempo = state.tempo;
        }
        if (state.tempo - self.sync.link_tempo).abs() > TEMPO_EPSILON && state.tempo > 0.0 {
            self.tempo_from_link(state.tempo);
        }
        if let Some(playing) = state.playing {
            if self.sync.was_link_playing != Some(playing) {
                self.sync.was_link_playing = Some(playing);
                match playing {
                    true if !self.playing => self.sync.link_waiting = true,
                    false if self.playing => self.stop_where_it_is(),
                    _ => {}
                }
            }
        }
        if self.sync.link_waiting {
            // Playback waits for the bar line, so it starts in phase with everyone else.
            if state.peers == 0 || state.phase < before.phase {
                self.sync.link_waiting = false;
                self.count_in = None;
                self.begin_playing();
            }
            return;
        }
        if self.sync.externally_timed() {
            return;
        }
        self.sync.trim = 1.0;
        if self.playing && state.peers > 0 {
            let beat = PPQ as f64;
            let quantum_ticks = quantum * beat;
            let per_second = self.maps.tempo.bpm_at(self.tick) / 60.0 * beat;
            let ours = (self.tick / beat).rem_euclid(quantum);
            let mut err = (state.phase - ours) * beat;
            if err.abs() > quantum_ticks / 2.0 {
                err -= quantum_ticks * err.signum();
            }
            if err.abs() > RELOCATE_TICKS {
                let to = self.tick + err;
                self.jump(to.max(0.0), JumpKind::Seek);
            } else {
                self.sync.trim = 1.0 + (err / per_second * TRIM_GAIN).clamp(-MAX_TRIM, MAX_TRIM);
            }
        }
    }

    /// Play was pressed. With Link peers around, it waits for the launch point; otherwise now.
    pub(crate) fn link_holds_start(&mut self) -> bool {
        if self.sync.link.is_none() || self.sync.link_state.peers == 0 {
            return false;
        }
        self.sync.link_waiting = true;
        true
    }

    pub(crate) fn tell_link_playing(&mut self, playing: bool) {
        if let Some(link) = self.sync.link.as_mut() {
            link.0.set_playing(playing);
            self.sync.was_link_playing = Some(playing);
        }
    }

    /// Send what the transport did in this block to the clock ports.
    pub(crate) fn emit_clock(&mut self, time: BlockTime) {
        if self.sync.config.clock_port_count == 0 {
            self.sync.was_playing = self.playing;
            self.sync.relocated = false;
            return;
        }
        let started = self.playing && !self.sync.was_playing;
        let stopped = !self.playing && self.sync.was_playing;
        let relocated = self.sync.relocated && self.playing && !started;
        self.sync.relocated = false;
        self.sync.was_playing = self.playing;
        let block = Instant::now();
        let clock = self.clock;
        let sr = self.sample_rate;
        let ports = self.sync.config;
        let per_second = time.tempo / 60.0 * PPQ as f64;
        for port in ports.outputs() {
            let delay = port.delay.max(0.0);
            let outbox = &mut self.sync.outbox;
            let mut send = |bytes: [u8; 3], len: u8, offset: usize| {
                let _ = outbox.push(HwSync {
                    port: port.port,
                    block,
                    offset: offset as f64 / sr + delay,
                    bytes,
                    len,
                    frame: clock + offset as u64,
                });
            };
            if stopped {
                send([sync::STOP, 0, 0], 1, 0);
                continue;
            }
            if !self.playing {
                continue;
            }
            // A start or a jump says where the song is first; clocks follow from there.
            if started || relocated {
                let beats = (time.t0.max(0.0) / TICKS_PER_MIDI_BEAT).floor() as u16 & 0x3fff;
                if relocated {
                    send([sync::STOP, 0, 0], 1, 0);
                }
                if started && time.t0 < 1.0 {
                    send([sync::START, 0, 0], 1, 0);
                } else {
                    send(
                        [
                            sync::SONG_POSITION,
                            (beats & 0x7f) as u8,
                            (beats >> 7) as u8,
                        ],
                        3,
                        0,
                    );
                    send([sync::CONTINUE, 0, 0], 1, 0);
                }
            }
            // A negative delay plays the clock early: it reads ahead by that much of the song.
            let ahead = (-port.delay).max(0.0) * per_second;
            let first = (time.t0 + ahead - CLOCK_EPSILON) / TICKS_PER_CLOCK;
            // The clock on the start itself isn't skipped because it's a moment ago.
            let mut n = if started || relocated {
                first.floor().max(0.0)
            } else {
                first.ceil()
            };
            loop {
                let tick = n * TICKS_PER_CLOCK - ahead;
                if tick >= time.t1() - CLOCK_EPSILON {
                    break;
                }
                send([sync::CLOCK, 0, 0], 1, time.offset(tick));
                n += 1.0;
            }
        }
    }
}

/// Everything sync adds to the MIDI sender.
impl crate::MidiSender {
    /// A sync message from a port, stamped now.
    pub fn send_sync(&mut self, message: SyncMessage) -> bool {
        self.sync
            .push((message, SyncStamp::At(Instant::now())))
            .is_ok()
    }

    /// A sync message from a port, stamped on the engine's clock: for tests.
    pub fn send_sync_at(&mut self, message: SyncMessage, frame: u64) -> bool {
        self.sync.push((message, SyncStamp::Frame(frame))).is_ok()
    }
}
