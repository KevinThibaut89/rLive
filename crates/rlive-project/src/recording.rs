//! Project settings for recording and the metronome.

use rlive_core::TimeSignature;
use rlive_core::PPQ;
use serde::{Deserialize, Serialize};

/// The metronome's tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MetronomeSound {
    #[default]
    Classic,
    Click,
    Wood,
    Beep,
}

impl MetronomeSound {
    pub const ALL: [MetronomeSound; 4] = [
        MetronomeSound::Classic,
        MetronomeSound::Click,
        MetronomeSound::Wood,
        MetronomeSound::Beep,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MetronomeSound::Classic => "Classic",
            MetronomeSound::Click => "Click",
            MetronomeSound::Wood => "Wood",
            MetronomeSound::Beep => "Beep",
        }
    }

    /// Which of the engine's sounds this is.
    pub fn index(self) -> u8 {
        self as u8
    }
}

/// How often the metronome ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BeatDivision {
    /// Follows the time signature's denominator.
    #[default]
    Auto,
    Half,
    Quarter,
    Eighth,
    Sixteenth,
}

impl BeatDivision {
    pub const ALL: [BeatDivision; 5] = [
        BeatDivision::Auto,
        BeatDivision::Half,
        BeatDivision::Quarter,
        BeatDivision::Eighth,
        BeatDivision::Sixteenth,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BeatDivision::Auto => "Auto",
            BeatDivision::Half => "1/2",
            BeatDivision::Quarter => "1/4",
            BeatDivision::Eighth => "1/8",
            BeatDivision::Sixteenth => "1/16",
        }
    }

    /// Ticks between clicks, or `None` for the denominator.
    pub fn ticks(self) -> Option<u32> {
        let quarter = PPQ as u32;
        match self {
            BeatDivision::Auto => None,
            BeatDivision::Half => Some(quarter * 2),
            BeatDivision::Quarter => Some(quarter),
            BeatDivision::Eighth => Some(quarter / 2),
            BeatDivision::Sixteenth => Some(quarter / 4),
        }
    }

    /// Whether this division divides a bar of `sig` evenly. One that doesn't is greyed out, and
    /// the metronome ticks the denominator until the meter fits again.
    pub fn fits(self, sig: TimeSignature) -> bool {
        self.ticks()
            .is_none_or(|t| sig.ticks_per_bar().is_multiple_of(t as u64))
    }
}

/// The metronome's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MetronomeOptions {
    /// Bars of count-in before recording. Zero is None.
    pub count_in: u8,
    pub sound: MetronomeSound,
    pub division: BeatDivision,
    pub only_while_recording: bool,
}

/// The grid new MIDI notes snap to while recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RecordQuantize {
    #[default]
    Off,
    Quarter,
    Eighth,
    EighthTriplet,
    EighthPlusTriplet,
    Sixteenth,
    SixteenthTriplet,
    SixteenthPlusTriplet,
    ThirtySecond,
}

impl RecordQuantize {
    pub const ALL: [RecordQuantize; 9] = [
        RecordQuantize::Off,
        RecordQuantize::Quarter,
        RecordQuantize::Eighth,
        RecordQuantize::EighthTriplet,
        RecordQuantize::EighthPlusTriplet,
        RecordQuantize::Sixteenth,
        RecordQuantize::SixteenthTriplet,
        RecordQuantize::SixteenthPlusTriplet,
        RecordQuantize::ThirtySecond,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RecordQuantize::Off => "No Quantization",
            RecordQuantize::Quarter => "1/4",
            RecordQuantize::Eighth => "1/8",
            RecordQuantize::EighthTriplet => "1/8T",
            RecordQuantize::EighthPlusTriplet => "1/8 + 1/8T",
            RecordQuantize::Sixteenth => "1/16",
            RecordQuantize::SixteenthTriplet => "1/16T",
            RecordQuantize::SixteenthPlusTriplet => "1/16 + 1/16T",
            RecordQuantize::ThirtySecond => "1/32",
        }
    }

    /// The tick a note at `tick` lands on.
    pub fn apply(self, tick: u64) -> u64 {
        let nearest = |grid: f64| (tick as f64 / grid).round() * grid;
        let closest_of = |a: f64, b: f64| {
            if (a - tick as f64).abs() <= (b - tick as f64).abs() {
                a
            } else {
                b
            }
        };
        let quarter = PPQ as f64;
        (match self {
            RecordQuantize::Off => tick as f64,
            RecordQuantize::Quarter => nearest(quarter),
            RecordQuantize::Eighth => nearest(quarter / 2.0),
            RecordQuantize::EighthTriplet => nearest(quarter / 3.0),
            RecordQuantize::EighthPlusTriplet => {
                closest_of(nearest(quarter / 2.0), nearest(quarter / 3.0))
            }
            RecordQuantize::Sixteenth => nearest(quarter / 4.0),
            RecordQuantize::SixteenthTriplet => nearest(quarter / 6.0),
            RecordQuantize::SixteenthPlusTriplet => {
                closest_of(nearest(quarter / 4.0), nearest(quarter / 6.0))
            }
            RecordQuantize::ThirtySecond => nearest(quarter / 8.0),
        })
        .round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_division_that_does_not_divide_the_bar_does_not_fit() {
        let three_four = TimeSignature {
            numerator: 3,
            denominator: 4,
        };
        assert!(!BeatDivision::Half.fits(three_four));
        assert!(BeatDivision::Quarter.fits(three_four));
        assert!(BeatDivision::Auto.fits(three_four));
    }

    #[test]
    fn record_quantize_snaps_to_the_grid() {
        assert_eq!(RecordQuantize::Off.apply(123), 123);
        assert_eq!(RecordQuantize::Sixteenth.apply(250), 240);
        assert_eq!(RecordQuantize::EighthTriplet.apply(330), 320);
        let triplet_or_eighth = RecordQuantize::EighthPlusTriplet.apply(470);
        assert_eq!(triplet_or_eighth, 480);
    }
}
