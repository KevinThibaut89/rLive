use serde::{Deserialize, Serialize};

/// Ticks per quarter note. All musical positions are expressed in ticks.
pub const PPQ: u64 = 960;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

impl TimeSignature {
    /// Length of one beat (the denominator note value) in ticks.
    pub fn ticks_per_beat(&self) -> u64 {
        PPQ * 4 / self.denominator.max(1) as u64
    }

    pub fn ticks_per_bar(&self) -> u64 {
        self.ticks_per_beat() * self.numerator.max(1) as u64
    }

    /// Format a tick position as `bar.beat.sixteenth` (1-based, like Ableton).
    pub fn format_position(&self, ticks: f64) -> String {
        let ticks = ticks.max(0.0) as u64;
        let bar = ticks / self.ticks_per_bar() + 1;
        let in_bar = ticks % self.ticks_per_bar();
        let beat = in_bar / self.ticks_per_beat() + 1;
        let sixteenth = (in_bar % self.ticks_per_beat()) / (PPQ / 4) + 1;
        format!("{bar}.{beat}.{sixteenth}")
    }
}

/// Global launch quantization for clips and scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Quantize {
    None,
    Sixteenth,
    Eighth,
    Beat,
    HalfBar,
    #[default]
    Bar,
    TwoBars,
    FourBars,
}

impl Quantize {
    pub const ALL: [Quantize; 8] = [
        Quantize::None,
        Quantize::Sixteenth,
        Quantize::Eighth,
        Quantize::Beat,
        Quantize::HalfBar,
        Quantize::Bar,
        Quantize::TwoBars,
        Quantize::FourBars,
    ];

    /// Quantum in ticks (0 = launch immediately).
    pub fn ticks(&self, sig: TimeSignature) -> u64 {
        match self {
            Quantize::None => 0,
            Quantize::Sixteenth => PPQ / 4,
            Quantize::Eighth => PPQ / 2,
            Quantize::Beat => sig.ticks_per_beat(),
            Quantize::HalfBar => sig.ticks_per_bar() / 2,
            Quantize::Bar => sig.ticks_per_bar(),
            Quantize::TwoBars => sig.ticks_per_bar() * 2,
            Quantize::FourBars => sig.ticks_per_bar() * 4,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Quantize::None => "None",
            Quantize::Sixteenth => "1/16",
            Quantize::Eighth => "1/8",
            Quantize::Beat => "Beat",
            Quantize::HalfBar => "1/2 Bar",
            Quantize::Bar => "1 Bar",
            Quantize::TwoBars => "2 Bars",
            Quantize::FourBars => "4 Bars",
        }
    }

    /// Next launch point at or after `tick`.
    pub fn next_boundary(&self, tick: f64, sig: TimeSignature) -> f64 {
        let q = self.ticks(sig) as f64;
        if q <= 0.0 {
            return tick;
        }
        // Tolerate tiny float error so a launch exactly on a boundary is not delayed a whole bar.
        let n = ((tick - 1e-6) / q).ceil().max(0.0);
        n * q
    }
}

/// Transport snapshot handed to every processor for the current block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportInfo {
    pub playing: bool,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    /// Song position at the start of the block, in ticks.
    pub tick: f64,
    /// Song position at the start of the block, in samples since play was pressed.
    pub sample_pos: u64,
    pub sample_rate: f64,
}

impl TransportInfo {
    pub fn ticks_per_sample(&self) -> f64 {
        ticks_per_sample(self.tempo, self.sample_rate)
    }

    /// Song position in quarter notes (beats in x/4).
    pub fn beats(&self) -> f64 {
        self.tick / PPQ as f64
    }
}

impl Default for TransportInfo {
    fn default() -> Self {
        Self {
            playing: false,
            tempo: 120.0,
            time_signature: TimeSignature::default(),
            tick: 0.0,
            sample_pos: 0,
            sample_rate: 48_000.0,
        }
    }
}

#[inline]
pub fn ticks_per_sample(tempo: f64, sample_rate: f64) -> f64 {
    tempo / 60.0 * PPQ as f64 / sample_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_boundaries() {
        let sig = TimeSignature::default();
        assert_eq!(Quantize::Bar.next_boundary(0.0, sig), 0.0);
        assert_eq!(Quantize::Bar.next_boundary(1.0, sig), 3840.0);
        assert_eq!(Quantize::Bar.next_boundary(3840.0, sig), 3840.0);
        assert_eq!(Quantize::Beat.next_boundary(961.0, sig), 1920.0);
        assert_eq!(Quantize::None.next_boundary(123.4, sig), 123.4);
    }

    #[test]
    fn position_format() {
        let sig = TimeSignature::default();
        assert_eq!(sig.format_position(0.0), "1.1.1");
        assert_eq!(sig.format_position(3840.0 + 960.0 + 240.0), "2.2.2");
    }

    #[test]
    fn three_four() {
        let sig = TimeSignature {
            numerator: 3,
            denominator: 4,
        };
        assert_eq!(sig.ticks_per_bar(), 2880);
        let six_eight = TimeSignature {
            numerator: 6,
            denominator: 8,
        };
        assert_eq!(six_eight.ticks_per_bar(), 2880);
    }
}
