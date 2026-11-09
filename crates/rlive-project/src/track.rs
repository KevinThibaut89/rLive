use crate::defaults::is_default;
use crate::{ArrangementClip, Clip, Device, Envelope, TakeLane};
use rlive_core::param::EnvTarget;
use rlive_core::{ChainId, DeviceId, TrackId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
    /// Holds tracks, whose output it mixes.
    Group,
    /// Fed by the sends of other tracks. Lives in [`Project::returns`](crate::Project).
    Return,
    /// The Main output. There is exactly one, [`Project::master`](crate::Project).
    Master,
}

/// A track's level into a return track.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackSend {
    /// The return track.
    pub to: TrackId,
    pub level_db: f32,
    /// Whether the send is switched on. A send that would feed back starts out off.
    #[serde(
        default = "crate::defaults::yes",
        skip_serializing_if = "crate::defaults::is_true"
    )]
    pub enabled: bool,
}

/// One end of a track's routing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Route {
    /// The natural choice for the track: all MIDI or no audio in, out to its group or Main.
    #[default]
    Default,
    /// `No Input`, or for an output no destination.
    None,
    /// A hardware input or output.
    External(ExternalRoute),
    /// Input only: what the Main output is playing.
    Resampling,
    /// Output only: the track reaches the mix through its sends alone.
    SendsOnly,
    /// Another track, or one of its devices wherever it is nested. As an output, `device` is
    /// what the Output Channel chooser addresses: `None` is Track In, and otherwise a device's
    /// sidechain input for audio, or the device itself for MIDI.
    Track {
        track: TrackId,
        #[serde(default, skip_serializing_if = "is_default")]
        device: Option<DeviceId>,
    },
    /// MIDI output only: another track, on one channel of a multi-timbral instrument there.
    TrackChannel { track: TrackId, channel: u8 },
}

/// Hardware audio channels or a MIDI port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalRoute {
    /// Audio channels of the interface, by the first channel's zero-based index.
    Audio { channel: u16, stereo: bool },
    /// `None` is All Ins or All Channels.
    Midi {
        port: Option<String>,
        channel: Option<u8>,
    },
}

/// A point along one chain of a rack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChainTap {
    /// Where the signal enters the rack, before the chain's devices.
    PreFx,
    /// The end of the chain, before its mixer.
    PostFx,
    /// The output of the chain's mixer, just before the rack sums its chains.
    #[default]
    PostMixer,
}

/// Where in another track's signal path a track listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Tap {
    PreFx,
    PostFx,
    #[default]
    PostMixer,
    /// A point in a chain or return chain of a rack in the tapped track.
    Chain {
        chain: ChainId,
        point: ChainTap,
    },
    /// An extra output of an instrument in the tapped track.
    Aux {
        device: DeviceId,
        output: u8,
    },
}

impl Tap {
    /// Soloing a track that taps this point still lets the tapped signal through. Only the
    /// post-mixer tap is silenced with the track it comes from (manual p. 382).
    pub fn hears_through_solo(&self) -> bool {
        !matches!(self, Tap::PostMixer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Monitor {
    In,
    #[default]
    Auto,
    Off,
}

impl Monitor {
    /// What Return to Default gives a track of this kind (manual p. 374).
    pub fn default_for(kind: TrackKind) -> Self {
        if kind == TrackKind::Audio {
            Monitor::Off
        } else {
            Monitor::Auto
        }
    }
}

/// A track's connections, as the In/Out section of the mixer shows them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Routing {
    pub input: Route,
    pub output: Route,
    pub monitor: Monitor,
    /// Where the signal is taken when `input` is another track.
    pub tap: Tap,
}

/// Which side of the crossfader a track is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Crossfade {
    #[default]
    None,
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PanMode {
    #[default]
    Stereo,
    /// Pans the left and right inputs independently, each -1..1.
    Split { left: f32, right: f32 },
}

/// Delay applied to the whole track, for compensating external gear.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum TrackDelay {
    #[default]
    None,
    Ms(f32),
    Samples(i32),
}

/// What a track's main automation lane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MainLane {
    /// Nothing has been chosen yet, so the mixer's volume.
    #[default]
    Volume,
    /// The Device chooser is set to None.
    None,
    Show(EnvTarget),
}

/// A frozen track's devices are out of the signal path (manual p. 985).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackFreeze {
    /// It was frozen in real time, for hardware; Unfreeze throws its files away (p. 987).
    pub realtime: bool,
}

/// View state stored with the track. Undo leaves it alone.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackUi {
    /// A group track's children are hidden, or a track's Arrangement lane is folded.
    pub folded: bool,
    /// The Arrangement lane's height in points, when set by hand.
    pub height: Option<f32>,
    pub show_take_lanes: bool,
    /// The take lane being auditioned, by index.
    pub audition_lane: Option<usize>,
    /// Automation lanes on show under the main one, in display order.
    pub automation_lanes: Vec<EnvTarget>,
    /// The main automation lane.
    pub automation_main: MainLane,
    /// The additional automation lanes are folded away.
    pub automation_lanes_hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub color: [u8; 3],
    pub kind: TrackKind,
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
    pub devices: Vec<Device>,
    /// One entry per scene.
    pub clips: Vec<Option<Clip>>,
    /// Scenes whose empty slot on this track has no Clip Stop button, sorted.
    #[serde(default, skip_serializing_if = "is_default")]
    pub no_stop: Vec<u32>,
    /// Clips on the timeline, sorted by start and not overlapping.
    #[serde(default)]
    pub arrangement: Vec<ArrangementClip>,
    /// The group track this track belongs to.
    #[serde(default, skip_serializing_if = "is_default")]
    pub group: Option<TrackId>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub sends: Vec<TrackSend>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub routing: Routing,
    #[serde(default, skip_serializing_if = "is_default")]
    pub crossfade: Crossfade,
    /// On a return track: whether the sends feeding it tap before the fader.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pre_fader_sends: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pan_mode: PanMode,
    #[serde(default, skip_serializing_if = "is_default")]
    pub delay: TrackDelay,
    /// Plays 12-TET even when the project has a tuning loaded.
    #[serde(default, skip_serializing_if = "is_default")]
    pub bypass_tuning: bool,
    /// What this track's controller keys play while a tuning is loaded.
    #[serde(default, skip_serializing_if = "is_default")]
    pub tuning_layout: crate::TuningLayout,
    /// The MPE/Multi-channel Settings of the track's MIDI output. `None` plays on one channel.
    #[serde(default, skip_serializing_if = "is_default")]
    pub mpe_out: Option<rlive_core::MpeOutput>,
    /// Automation of this track's parameters. Sorted by target only for stable files.
    #[serde(default, skip_serializing_if = "is_default")]
    pub automation: Vec<Envelope>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub take_lanes: Vec<TakeLane>,
    /// Keep the monitoring path's timing in what is recorded, so it lands where it was heard.
    #[serde(
        default = "crate::defaults::yes",
        skip_serializing_if = "crate::defaults::is_true"
    )]
    pub keep_monitor_latency: bool,
    /// Set while the track is frozen.
    #[serde(default, skip_serializing_if = "is_default")]
    pub freeze: Option<TrackFreeze>,
    /// The user's info text.
    #[serde(default, skip_serializing_if = "is_default")]
    pub info: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub ui: TrackUi,
}

impl Track {
    pub fn new(name: &str, kind: TrackKind, color: [u8; 3], scenes: usize) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            color,
            kind,
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            arm: false,
            devices: Vec::new(),
            clips: vec![None; scenes],
            no_stop: Vec::new(),
            arrangement: Vec::new(),
            group: None,
            sends: Vec::new(),
            routing: Routing::default(),
            crossfade: Crossfade::default(),
            pre_fader_sends: false,
            pan_mode: PanMode::default(),
            delay: TrackDelay::default(),
            bypass_tuning: false,
            tuning_layout: crate::TuningLayout::default(),
            mpe_out: None,
            automation: Vec::new(),
            take_lanes: Vec::new(),
            keep_monitor_latency: true,
            freeze: None,
            info: String::new(),
            ui: TrackUi::default(),
        }
    }

    /// Whether this track plays 12-TET under a loaded tuning: asked to, or holding a Drum Rack,
    /// whose pads aren't pitches.
    pub fn tuning_bypassed(&self) -> bool {
        self.bypass_tuning
            || self.devices.iter().any(|d| {
                matches!(
                    d.kind,
                    crate::DeviceKind::Rack {
                        kind: crate::RackKind::Drum,
                        ..
                    }
                )
            })
    }

    pub fn is_frozen(&self) -> bool {
        self.freeze.is_some()
    }

    /// Whether a slot's Clip Stop button is there.
    pub fn has_stop_button(&self, slot: usize) -> bool {
        self.no_stop.binary_search(&(slot as u32)).is_err()
    }

    pub fn set_stop_button(&mut self, slot: usize, on: bool) {
        match (self.no_stop.binary_search(&(slot as u32)), on) {
            (Ok(at), true) => {
                self.no_stop.remove(at);
            }
            (Err(at), false) => self.no_stop.insert(at, slot as u32),
            _ => {}
        }
    }

    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }
}
