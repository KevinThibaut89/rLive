//! Tempo as the user and the outside world change it: overriding the tempo automation, recording
//! it, Tap and Nudge, and the choice of what the transport follows.

use super::Session;
use crate::settings::SyncSettings;
use rlive_core::param::{transport, EnvTarget};
use rlive_core::DeviceId;
use rlive_core::ParamRange;
use rlive_engine::sync::{ClockPort, ExternalSync, LinkClock, LinkHolder, SyncConfig};
use rlive_engine::{EngineCommand, TransportCmd};
use rlive_project::{Breakpoint, DeviceKind, Envelope};
use std::collections::HashMap;

/// How much faster or slower time runs while a Phase Nudge button is held.
pub const PHASE_NUDGE: f64 = 0.03;
/// A gap this long between taps starts a new count, in seconds.
const TAP_GAP_SECONDS: f64 = 2.0;
/// The taps averaged for the tempo.
const MAX_TAPS: usize = 8;

/// What the settings say about keeping time, in the form the session uses it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncPorts {
    /// The input port's sync, if any port has its Sync switch on.
    pub source: Option<ExternalSync>,
    /// The ports sending MIDI Clock.
    pub outputs: Vec<ClockPort>,
    /// The channel Tempo Follower listens to, when one is set.
    pub follower_channel: Option<u16>,
}

/// The session's view of who's keeping time.
#[derive(Default)]
pub(super) struct SyncRuntime {
    ports: SyncPorts,
    /// The EXT switch.
    external: bool,
    /// The Follow switch.
    follower: bool,
    /// A Link session is connected.
    link: bool,
    /// The tempo field has been moved over automation; it stays moved until Re-Enable.
    overridden: bool,
    last_sent: Option<SyncConfig>,
    stroke: Option<TempoStroke>,
    taps: Vec<f64>,
    /// The last tempo each Looper asked for, so one request is honoured once.
    looper_requests: HashMap<DeviceId, f32>,
    looper_readers: HashMap<DeviceId, rlive_core::ViewReader>,
}

/// Tempo moves made while recording, on their way into the Arrangement.
struct TempoStroke {
    before: f64,
    points: Vec<Breakpoint>,
}

impl Session {
    // ------------------------------------------------------------- the tempo field

    /// Moving the tempo: into the map when there are no points to override, otherwise over them
    /// until Re-Enable Automation, and into the Arrangement if the recording arm is on.
    pub fn set_tempo(&mut self, bpm: f64) {
        if self.tempo_field_locked() {
            return;
        }
        let bpm = bpm.clamp(rlive_core::time::MIN_BPM, rlive_core::time::MAX_BPM);
        if self.tempo_recording() {
            self.record_tempo(bpm);
        }
        self.project.tempo = bpm;
        if !self.project.arrangement.tempo.points.is_empty() && !self.sync.overridden {
            self.sync.overridden = true;
        }
        if self.sync.link {
            self.send(EngineCommand::Transport(TransportCmd::TellLinkTempo(bpm)));
        } else if self.sync.overridden || self.sync.stroke.is_some() {
            self.send(EngineCommand::Transport(TransportCmd::OverrideTempo(Some(
                bpm,
            ))));
        } else {
            self.send(EngineCommand::Transport(TransportCmd::SetTempo(bpm)));
        }
        self.mark_edit("Change Tempo");
    }

    /// Whether the tempo field can't be moved, because something else sets the tempo.
    pub fn tempo_field_locked(&self) -> bool {
        self.project.has_tempo_leader() || self.externally_timed() || self.follower_drives()
    }

    /// Whether the transport is slaved to MIDI Clock or timecode.
    pub fn externally_timed(&self) -> bool {
        self.sync.external && self.sync.ports.source.is_some() && !self.follower_active()
    }

    /// Whether Tempo Follower is listening.
    pub fn follower_active(&self) -> bool {
        self.sync.follower && self.sync.ports.follower_channel.is_some()
    }

    /// Whether what Tempo Follower hears is setting the tempo. Clip Tempo Leaders overrule it.
    pub fn follower_drives(&self) -> bool {
        self.follower_active() && !self.project.has_tempo_leader()
    }

    /// Whether the tempo automation is being held off by a manual change.
    pub fn tempo_overridden(&self) -> bool {
        self.sync.overridden
    }

    pub(super) fn re_enable_tempo(&mut self) {
        if std::mem::take(&mut self.sync.overridden) {
            self.send(EngineCommand::Transport(TransportCmd::OverrideTempo(None)));
            self.sync_time_maps();
        }
    }

    /// Whether moves of the tempo field are being recorded into the Arrangement.
    fn tempo_recording(&self) -> bool {
        self.automation_arm() && self.is_recording() && self.is_playing()
    }

    fn record_tempo(&mut self, bpm: f64) {
        let tick = self.engine.shared.tick.load();
        let before = self.project.tempo;
        let stroke = self.sync.stroke.get_or_insert_with(|| TempoStroke {
            before,
            points: vec![Breakpoint::new(tick, bpm)],
        });
        match stroke.points.last_mut() {
            Some(last) if tick <= last.time => last.value = bpm,
            // The field jumps from one value to the next: what played held until this moment.
            Some(last) => {
                let held = last.value;
                stroke.points.push(Breakpoint::new(tick, held));
                stroke.points.push(Breakpoint::new(tick, bpm));
            }
            None => stroke.points.push(Breakpoint::new(tick, bpm)),
        }
    }

    /// The pass ended or the playhead jumped: what was recorded goes into the Arrangement, and
    /// the tempo goes back to following it.
    pub(super) fn finish_tempo_stroke(&mut self, tick: f64) {
        let Some(mut stroke) = self.sync.stroke.take() else {
            return;
        };
        if let Some(last) = stroke.points.last().copied() {
            if tick > last.time {
                stroke.points.push(Breakpoint::new(tick, last.value));
            }
        }
        let tempo = &self.project.arrangement.tempo;
        let range = ParamRange {
            min: tempo.min,
            max: tempo.max.max(tempo.min + 1.0),
            skew: 1.0,
            quantized: false,
        };
        let mut envelope = Envelope {
            target: EnvTarget::Param(transport_param()),
            points: std::mem::take(&mut self.project.arrangement.tempo.points),
        };
        envelope.overwrite(&stroke.points, stroke.before, range);
        self.project.arrangement.tempo.points = envelope.points;
        self.sync.overridden = false;
        self.send(EngineCommand::Transport(TransportCmd::OverrideTempo(None)));
        self.sync_time_maps();
        self.mark_edit("Record Automation");
    }

    /// One press of Tap Tempo at `now` seconds. With `start`, tapping out a bar starts the
    /// transport at the tapped tempo.
    pub fn tap_tempo(&mut self, now: f64, start: bool) {
        let taps = &mut self.sync.taps;
        if taps.last().is_some_and(|t| now - t > TAP_GAP_SECONDS) {
            taps.clear();
        }
        taps.push(now);
        if taps.len() > MAX_TAPS {
            taps.remove(0);
        }
        let count = taps.len();
        if count >= 2 {
            let avg = (taps[count - 1] - taps[0]) / (count - 1) as f64;
            if avg > 0.0 {
                self.set_tempo(60.0 / avg);
            }
        }
        if start && !self.is_playing() {
            let beats = self.project.bar_map().signature_at(0.0).numerator as usize;
            if count >= beats.max(2) {
                self.sync.taps.clear();
                self.play();
            }
        }
    }

    /// A Phase Nudge button went down (`direction` of 1 or -1) or came up (0).
    pub fn set_phase_nudge(&mut self, direction: i8) {
        let ratio = 1.0 + f64::from(direction.signum()) * PHASE_NUDGE;
        self.send(EngineCommand::Transport(TransportCmd::SetNudge(ratio)));
    }

    // ------------------------------------------------------------- what keeps time

    /// The settings changed: which ports do what. Sent along if it changes anything.
    pub fn configure_sync(&mut self, ports: SyncPorts, settings: &SyncSettings) {
        self.sync.ports = ports;
        self.sync.external = settings.external;
        self.sync.follower = settings.follower;
        self.send_sync_config();
    }

    pub fn external_sync(&self) -> bool {
        self.sync.external
    }

    /// Whether the EXT switch can be thrown: there's a source, and Tempo Follower isn't active.
    pub fn external_sync_available(&self) -> bool {
        self.sync.ports.source.is_some() && !self.follower_active()
    }

    pub fn set_external_sync(&mut self, on: bool) {
        if on && !self.external_sync_available() {
            return;
        }
        self.sync.external = on;
        self.after_sync_change();
    }

    pub fn tempo_follower(&self) -> bool {
        self.sync.follower
    }

    /// Whether Tempo Follower has its input: it can only be switched on with one.
    pub fn follower_available(&self) -> bool {
        self.sync.ports.follower_channel.is_some()
    }

    /// The Follow switch. It excludes External Sync.
    pub fn set_tempo_follower(&mut self, on: bool) {
        if on && !self.follower_available() {
            return;
        }
        self.sync.follower = on;
        if on {
            self.sync.external = false;
        }
        self.after_sync_change();
    }

    /// Connect a Link session or disconnect one.
    pub fn set_link(&mut self, link: Option<Box<dyn LinkClock>>) {
        self.sync.link = link.is_some();
        let holder = link.map(|l| Box::new(LinkHolder(l)));
        self.send(EngineCommand::Transport(TransportCmd::SetLink(holder)));
        if !self.sync.link {
            self.release_tempo();
        }
    }

    pub fn link_enabled(&self) -> bool {
        self.sync.link
    }

    pub fn link_peers(&self) -> u32 {
        self.engine
            .shared
            .link_peers
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn after_sync_change(&mut self) {
        self.send_sync_config();
        if !self.externally_timed() && !self.follower_active() && !self.sync.link {
            self.release_tempo();
        }
    }

    /// Hands the tempo back to the map: whatever overrode it has gone.
    fn release_tempo(&mut self) {
        if self.sync.overridden {
            let bpm = self.project.tempo;
            self.send(EngineCommand::Transport(TransportCmd::OverrideTempo(Some(
                bpm,
            ))));
        } else {
            self.send(EngineCommand::Transport(TransportCmd::OverrideTempo(None)));
            self.sync_time_maps();
        }
    }

    pub(super) fn send_sync_config(&mut self) {
        let config = self.sync_config();
        if self.sync.last_sent != Some(config) {
            self.sync.last_sent = Some(config);
            self.send(EngineCommand::Transport(TransportCmd::SetSync(Box::new(
                config,
            ))));
        }
    }

    fn sync_config(&self) -> SyncConfig {
        let mut config = SyncConfig::default();
        for port in &self.sync.ports.outputs {
            config.add_output(*port);
        }
        if self.externally_timed() {
            config.external = self.sync.ports.source.unwrap_or_default();
        }
        if self.follower_active() {
            config.follower = self.sync.ports.follower_channel;
            config.follower_drives = !self.project.has_tempo_leader();
        }
        config
    }

    /// A fresh engine knows nothing of what the last one was told.
    pub(super) fn resend_sync(&mut self) {
        self.sync.last_sent = None;
        self.send_sync_config();
        let cmd = if self.sync.overridden {
            TransportCmd::OverrideTempo(Some(self.project.tempo))
        } else {
            TransportCmd::OverrideTempo(None)
        };
        if !(self.externally_timed() || self.follower_drives() || self.sync.link) {
            self.send(EngineCommand::Transport(cmd));
        }
    }

    /// A new Set: nothing has been overridden in it yet.
    pub(super) fn forget_tempo_override(&mut self) {
        self.sync.overridden = false;
        self.sync.stroke = None;
    }

    /// Looper's Set & Follow finds a tempo while the Set is stopped: honour each request once.
    pub fn poll_looper_tempo(&mut self) {
        use rlive_devices::devices::b6::looper;
        let mut requests = Vec::new();
        let readers = &mut self.sync.looper_readers;
        let looper = |d: &&rlive_project::Device| matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.looper");
        let tracks = self.project.tracks.iter().chain(&self.project.returns);
        for device in tracks.flat_map(|t| t.devices.iter()).filter(looper) {
            let Some(view) = self.runtimes.get(&device.id).and_then(|r| r.view.as_ref()) else {
                continue;
            };
            // A device only publishes while someone is reading, so the reader stays.
            let reader = readers.entry(device.id).or_insert_with(|| view.attach());
            let mut values = [0.0f32; looper::VIEW_LEN];
            if reader.read(looper::VIEW, &mut values) {
                requests.push((device.id, values[looper::TEMPO_REQUEST_SLOT]));
            }
        }
        for (id, request) in requests {
            let last = self.sync.looper_requests.insert(id, request);
            if request > 0.0 && last != Some(request) {
                self.set_tempo(f64::from(request));
            }
        }
    }
}

fn transport_param() -> rlive_core::param::ParamRef {
    let EnvTarget::Param(p) = transport::tempo_target() else {
        unreachable!("the tempo is a parameter");
    };
    p
}
