//! The Hardware Latency control both devices share (manual pp. 611, 742).

use rlive_core::{Item, ParamId, ParamInfo};

/// Most milliseconds of Hardware Latency. The manual gives no figure.
pub const MAX_LATENCY_MS: f64 = 2000.0;
/// Most samples of Hardware Latency, likewise.
pub const MAX_LATENCY_SAMPLES: f64 = 96_000.0;

/// Hardware Latency in the current unit, and which unit that is.
pub fn latency_params(mode: u32, ms: u32, samples: u32) -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(mode, "Latency Mode", &["ms", "Samples"], 0),
        ParamInfo::skewed(ms, "Hardware Latency", 0.0, MAX_LATENCY_MS, 0.0, "ms", 3.0),
        ParamInfo::skewed(
            samples,
            "Hardware Latency Samples",
            0.0,
            MAX_LATENCY_SAMPLES,
            0.0,
            "",
            3.0,
        ),
    ]
}

pub fn latency_items(mode: u32, ms: u32, samples: u32) -> Vec<Item> {
    vec![
        Item::Menu(ParamId(mode)),
        Item::Knob(ParamId(ms)),
        Item::Knob(ParamId(samples)),
    ]
}

/// Hardware Latency in samples at a sample rate.
#[derive(Clone, Copy, Debug, Default)]
pub struct Latency {
    pub in_samples: bool,
    pub ms: f64,
    pub samples: f64,
    /// Added on top by the router, for a path that isn't hardware at all.
    pub internal: f64,
}

impl Latency {
    pub fn frames(&self, sample_rate: f64) -> u32 {
        let own = if self.in_samples {
            self.samples
        } else {
            self.ms * 0.001 * sample_rate
        };
        (own + self.internal).round().max(0.0) as u32
    }
}
