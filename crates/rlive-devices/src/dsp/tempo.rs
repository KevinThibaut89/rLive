//! Tempo-synced rates and a clock that follows the transport.

use rlive_core::TransportInfo;

/// Musical time for one block, stepped along sample by sample. Beats are quarter notes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatClock {
    /// Song position in beats.
    pub beat: f64,
    pub beats_per_sample: f64,
    pub playing: bool,
}

impl Default for BeatClock {
    fn default() -> Self {
        Self::new(120.0, 48_000.0)
    }
}

impl BeatClock {
    /// A stopped clock at the song start.
    pub fn new(tempo: f64, sample_rate: f64) -> Self {
        Self {
            beat: 0.0,
            beats_per_sample: tempo / 60.0 / sample_rate.max(1.0),
            playing: false,
        }
    }

    /// The clock at the start of the block `transport` describes.
    pub fn from_transport(transport: &TransportInfo) -> Self {
        Self {
            beat: transport.beats(),
            beats_per_sample: (transport.tempo / 60.0 / transport.sample_rate.max(1.0)).max(0.0),
            playing: transport.playing,
        }
    }

    #[inline]
    pub fn tick(&mut self) {
        self.beat += self.beats_per_sample;
    }

    pub fn advance(&mut self, frames: usize) {
        self.beat += self.beats_per_sample * frames as f64;
    }
}

/// Note-length feel of a [`SyncRate`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncFeel {
    Straight,
    Triplet,
    Dotted,
}

/// A note length such as 1/16T. `numerator / denominator` is in whole notes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SyncRate {
    pub numerator: u16,
    pub denominator: u16,
    pub feel: SyncFeel,
}

const fn rate(numerator: u16, denominator: u16, feel: SyncFeel) -> SyncRate {
    SyncRate {
        numerator,
        denominator,
        feel,
    }
}

impl SyncRate {
    /// The rates a device menu offers, from short to long.
    pub const ALL: [SyncRate; 21] = [
        rate(1, 64, SyncFeel::Triplet),
        rate(1, 64, SyncFeel::Straight),
        rate(1, 32, SyncFeel::Triplet),
        rate(1, 32, SyncFeel::Straight),
        rate(1, 32, SyncFeel::Dotted),
        rate(1, 16, SyncFeel::Triplet),
        rate(1, 16, SyncFeel::Straight),
        rate(1, 16, SyncFeel::Dotted),
        rate(1, 8, SyncFeel::Triplet),
        rate(1, 8, SyncFeel::Straight),
        rate(1, 8, SyncFeel::Dotted),
        rate(1, 4, SyncFeel::Triplet),
        rate(1, 4, SyncFeel::Straight),
        rate(1, 4, SyncFeel::Dotted),
        rate(1, 2, SyncFeel::Straight),
        rate(1, 2, SyncFeel::Dotted),
        rate(1, 1, SyncFeel::Straight),
        rate(2, 1, SyncFeel::Straight),
        rate(4, 1, SyncFeel::Straight),
        rate(8, 1, SyncFeel::Straight),
        rate(16, 1, SyncFeel::Straight),
    ];

    /// Menu labels, matching [`SyncRate::ALL`].
    pub const LABELS: [&'static str; 21] = [
        "1/64T", "1/64", "1/32T", "1/32", "1/32D", "1/16T", "1/16", "1/16D", "1/8T", "1/8", "1/8D",
        "1/4T", "1/4", "1/4D", "1/2", "1/2D", "1 Bar", "2 Bars", "4 Bars", "8 Bars", "16 Bars",
    ];

    /// The rate at menu position `index`, clamped to the list.
    pub fn from_index(index: f64) -> Self {
        Self::ALL[index.round().clamp(0.0, (Self::ALL.len() - 1) as f64) as usize]
    }

    /// Length in beats (quarter notes).
    pub fn beats(self) -> f64 {
        let straight = 4.0 * self.numerator as f64 / self.denominator.max(1) as f64;
        match self.feel {
            SyncFeel::Straight => straight,
            SyncFeel::Triplet => straight * 2.0 / 3.0,
            SyncFeel::Dotted => straight * 1.5,
        }
    }
}
