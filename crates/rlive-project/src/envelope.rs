//! Breakpoint envelopes: track automation, clip envelopes and per-note expression.

use crate::defaults::is_default;
use rlive_core::param::EnvTarget;
use serde::{Deserialize, Serialize};

/// A point on an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Breakpoint {
    /// Position in ticks. Two points at the same position make a step.
    pub time: f64,
    pub value: f64,
    /// Shape of the segment to the next point: 0 is straight, -1 to 1 bends it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub curve: f32,
}

impl Breakpoint {
    pub fn new(time: f64, value: f64) -> Self {
        Self {
            time,
            value,
            curve: 0.0,
        }
    }
}

/// An automation envelope: `points` sorted by time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub target: EnvTarget,
    #[serde(default, skip_serializing_if = "is_default")]
    pub points: Vec<Breakpoint>,
}

/// How a clip envelope applies to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EnvMode {
    /// Offsets the target, on top of what automation sets. The only mode in the Arrangement.
    #[default]
    Modulation,
    /// Sets the target's absolute value while the clip plays.
    Automation,
}

/// A clip envelope. With `loop_region` it is unlinked: it loops by itself instead of following
/// the clip's loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipEnvelope {
    pub envelope: Envelope,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: EnvMode,
    #[serde(default, skip_serializing_if = "is_default")]
    pub loop_region: Option<EnvLoop>,
}

/// The loop of an unlinked clip envelope, in ticks of the clip's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvLoop {
    pub start: u64,
    pub length: u64,
}
