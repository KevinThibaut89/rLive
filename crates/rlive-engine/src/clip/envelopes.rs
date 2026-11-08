//! Envelopes in the clips the engine plays, read at a clip position.
//!
//! The curves here are compiled from a project clip's envelopes and are read-only: a control step
//! or a frame of audio finds its place by binary search, with nothing to fall behind at a loop or
//! a seek, and nothing allocated.

use super::data::{ClipData, ProgramChange};
use super::midi::push_event;
use super::BlockTime;
use rlive_core::envelope::{value_at, EnvPoint, Interp};
use rlive_core::{ClipRegion, MidiEvent, MidiMessage, ParamRange};

/// A point of a compiled envelope, in knob position, which is the space segments run straight in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CurvePoint {
    time: f64,
    knob: f64,
    curve: f32,
}

impl EnvPoint for CurvePoint {
    fn time(&self) -> f64 {
        self.time
    }
    fn value(&self) -> f64 {
        self.knob
    }
    fn curve(&self) -> f32 {
        self.curve
    }
}

/// An envelope's points, read at any position.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvCurve {
    range: ParamRange,
    interp: Interp,
    points: Box<[CurvePoint]>,
}

impl EnvCurve {
    /// A curve from points of `(position, plain value, curve)` in any order. A curve with no
    /// point says nothing, so it comes back as `None`.
    pub fn new(
        range: ParamRange,
        points: impl IntoIterator<Item = (f64, f64, f32)>,
    ) -> Option<Self> {
        let mut points: Vec<CurvePoint> = points
            .into_iter()
            .filter(|(time, value, _)| time.is_finite() && value.is_finite())
            .map(|(time, value, curve)| CurvePoint {
                time,
                knob: range.to_normalized(value),
                curve,
            })
            .collect();
        if points.is_empty() {
            return None;
        }
        // Stable, so two points at one position stay a step in the right direction.
        points.sort_by(|a, b| a.time.total_cmp(&b.time));
        Some(Self {
            range,
            interp: Interp::of(range.quantized),
            points: points.into_boxed_slice(),
        })
    }

    /// The curve's value at `position`, in plain units.
    pub fn plain_at(&self, position: f64) -> f64 {
        let knob = value_at(&self.points, position, self.interp).unwrap_or(0.0);
        self.range.from_normalized(knob)
    }
}

/// Whose time an envelope runs on (manual p. 542).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EnvTiming {
    /// Linked to the clip: it reads the clip's position, looping with the clip.
    Linked { length: f64, looped: bool },
    /// Linked to a clip whose markers have been moved: it reads what the clip is playing.
    Region(ClipRegion),
    /// Unlinked: it has a loop or region and a start marker of its own.
    Unlinked {
        start: f64,
        end: f64,
        marker: f64,
        looped: bool,
    },
}

impl EnvTiming {
    /// Where on the envelope a clip `local` ticks past its start is.
    fn position(&self, local: f64) -> f64 {
        match *self {
            EnvTiming::Linked { length, looped } => {
                if looped && length > 0.0 {
                    local.rem_euclid(length)
                } else {
                    local.max(0.0)
                }
            }
            EnvTiming::Region(region) => region.content_at(local),
            EnvTiming::Unlinked {
                start,
                end,
                marker,
                looped,
            } => {
                let at = marker + local.max(0.0);
                if !looped {
                    at.min(end)
                } else if at < end || end <= start {
                    at
                } else {
                    start + (at - start).rem_euclid(end - start)
                }
            }
        }
    }
}

/// An envelope with its timing: the value at a clip position.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipCurve {
    curve: EnvCurve,
    timing: EnvTiming,
}

impl ClipCurve {
    pub fn new(curve: EnvCurve, timing: EnvTiming) -> Self {
        Self { curve, timing }
    }

    /// The value at `local` ticks past the clip's start.
    pub fn at(&self, local: f64) -> f64 {
        self.curve.plain_at(self.timing.position(local))
    }
}

/// The envelopes of a clip that belong to its own content. Those that drive the track's
/// parameters are compiled with the track's routes, as they address its cells.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClipEnvelopes {
    /// A share of the Clip Gain, 0 to 1.
    pub gain: Option<ClipCurve>,
    /// Semitones added to the transposition.
    pub transpose: Option<ClipCurve>,
    /// Sixteenths the read position moves by. Only Beats mode has one.
    pub sample_offset: Option<ClipCurve>,
    /// MIDI controllers, by number.
    pub midi: Box<[(u8, ClipCurve)]>,
}

impl ClipEnvelopes {
    /// Whether any of these envelopes does anything.
    pub fn is_empty(&self) -> bool {
        self.gain.is_none()
            && self.transpose.is_none()
            && self.sample_offset.is_none()
            && self.midi.is_empty()
    }

    /// Whether a clip's read position has to be worked out in short pieces to follow these.
    pub(crate) fn moves_the_read(&self) -> bool {
        self.transpose.is_some() || self.sample_offset.is_some()
    }
}

/// A clip with envelopes on its own content.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvelopedClip {
    /// Never itself enveloped.
    pub clip: ClipData,
    pub envelopes: ClipEnvelopes,
    /// Where its markers are, when any isn't at its default.
    pub region: Option<ClipRegion>,
    /// Short fades on the edges that cut through its audio (manual p. 218).
    pub edge_fade: bool,
    /// Resample with the better interpolation (manual p. 220).
    pub hi_q: bool,
}

/// Spacing of controller values in the stream, in ticks: about 10 ms at 120 BPM.
const CC_STEP: f64 = 20.0;

/// An envelope's value as a 7-bit controller value.
fn cc_value(curve: &ClipCurve, local: f64) -> u8 {
    curve.at(local).round().clamp(0.0, 127.0) as u8
}

/// A controller leaves room in the block's event list, so a burst of it never crowds out
/// note-ons and note-offs: a lost value is corrected by the next, a lost note-off sticks.
fn push_cc(ev: &mut Vec<MidiEvent>, offset: u32, cc: u8, value: u8) {
    let limit = ev.capacity() - ev.capacity() / 4;
    if ev.len() < limit {
        push_event(
            ev,
            MidiEvent::new(
                offset,
                MidiMessage::ControlChange {
                    channel: 0,
                    cc,
                    value,
                },
            ),
        );
    }
}

/// The changes in a clip's controllers over `[a, b)`, on a fixed tick grid from the clip's start,
/// so a clip sends the same stream whatever the block size. The start itself is covered by
/// [`enter_clip`].
pub(super) fn render_controllers(
    envelopes: &ClipEnvelopes,
    origin: f64,
    a: f64,
    b: f64,
    time: BlockTime,
    ev: &mut Vec<MidiEvent>,
) {
    for (cc, curve) in envelopes.midi.iter() {
        let mut step = ((a - origin) / CC_STEP).ceil().max(1.0);
        let mut previous = cc_value(curve, (step - 1.0) * CC_STEP);
        loop {
            let tick = origin + step * CC_STEP;
            if tick >= b {
                break;
            }
            let value = cc_value(curve, step * CC_STEP);
            if value != previous {
                push_cc(ev, time.event_offset(tick), *cc, value);
            }
            previous = value;
            step += 1.0;
        }
    }
}

/// A clip starts at `local` ticks into itself: every controller it has envelopes for goes to its
/// value there, and with `auto_reset`, MIDI Envelope Auto-Reset's controllers go to their rest
/// positions first (p. 534).
pub(crate) fn enter_clip(
    clip: &ClipData,
    local: f64,
    offset: u32,
    auto_reset: bool,
    ev: &mut Vec<MidiEvent>,
) {
    if auto_reset && matches!(clip.content(), ClipData::Midi(_)) {
        reset_controllers(offset, ev);
    }
    if let ClipData::Midi(midi) = clip.content() {
        if let Some(program) = midi.program {
            send_program(program, offset, ev);
        }
    }
    if let Some(envelopes) = clip.envelopes() {
        for (cc, curve) in envelopes.midi.iter() {
            push_cc(ev, offset, *cc, cc_value(curve, local));
        }
    }
}

/// What a clip's bank and program choosers ask for: Bank Select, its LSB, then Program Change.
fn send_program(program: ProgramChange, offset: u32, ev: &mut Vec<MidiEvent>) {
    for (cc, value) in [(0, program.bank), (32, program.sub_bank)] {
        if let Some(value) = value {
            push_event(
                ev,
                MidiEvent::new(
                    offset,
                    MidiMessage::ControlChange {
                        channel: 0,
                        cc,
                        value: value & 127,
                    },
                ),
            );
        }
    }
    if let Some(program) = program.program {
        push_event(
            ev,
            MidiEvent::new(
                offset,
                MidiMessage::ProgramChange {
                    channel: 0,
                    program: program & 127,
                },
            ),
        );
    }
}

/// MIDI Envelope Auto-Reset's controllers, which the manual calls "certain MIDI control messages":
/// the ones that stay stuck audibly when a clip that moved them is left behind.
fn reset_controllers(offset: u32, ev: &mut Vec<MidiEvent>) {
    /// Modulation Wheel, Expression and Sustain.
    const RESETS: [(u8, u8); 3] = [(1, 0), (11, 127), (64, 0)];
    push_event(
        ev,
        MidiEvent::new(
            offset,
            MidiMessage::PitchBend {
                channel: 0,
                value: 0.0,
            },
        ),
    );
    for (cc, value) in RESETS {
        push_cc(ev, offset, cc, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT: ParamRange = ParamRange {
        min: 0.0,
        max: 127.0,
        skew: 1.0,
        quantized: false,
    };

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    fn curve(points: &[(f64, f64)], timing: EnvTiming) -> ClipCurve {
        ClipCurve::new(
            EnvCurve::new(UNIT, points.iter().map(|&(t, v)| (t, v, 0.0))).unwrap(),
            timing,
        )
    }

    fn linked(length: f64, looped: bool) -> EnvTiming {
        EnvTiming::Linked { length, looped }
    }

    fn unlinked(start: f64, end: f64, marker: f64, looped: bool) -> EnvTiming {
        EnvTiming::Unlinked {
            start,
            end,
            marker,
            looped,
        }
    }

    #[test]
    fn a_linked_envelope_loops_with_the_clip_and_a_one_shot_holds() {
        let c = curve(&[(0.0, 0.0), (960.0, 96.0)], linked(960.0, true));
        close(c.at(480.0), 48.0);
        close(c.at(960.0 + 480.0), 48.0);
        let c = curve(&[(0.0, 0.0), (960.0, 96.0)], linked(960.0, false));
        close(c.at(2000.0), 96.0);
    }

    #[test]
    fn an_unlinked_loop_of_an_odd_length_comes_round_on_its_own() {
        let c = curve(
            &[(0.0, 0.0), (700.0, 70.0)],
            unlinked(0.0, 700.0, 0.0, true),
        );
        close(c.at(350.0), 35.0);
        close(c.at(700.0), 0.0);
        close(c.at(700.0 * 3.0 + 70.0), 7.0);
    }

    #[test]
    fn a_start_marker_is_where_it_departs_from() {
        let c = curve(
            &[(0.0, 0.0), (100.0, 10.0), (400.0, 40.0)],
            unlinked(100.0, 400.0, 200.0, true),
        );
        close(c.at(0.0), 20.0);
        close(c.at(100.0), 30.0);
        // Round the end, back to the loop's start, not the marker.
        close(c.at(200.0), 10.0);
    }

    #[test]
    fn an_unlinked_envelope_with_its_loop_off_plays_once_and_holds_its_end() {
        let c = curve(
            &[(0.0, 0.0), (480.0, 48.0)],
            unlinked(0.0, 480.0, 0.0, false),
        );
        close(c.at(240.0), 24.0);
        close(c.at(100_000.0), 48.0);
    }
}
