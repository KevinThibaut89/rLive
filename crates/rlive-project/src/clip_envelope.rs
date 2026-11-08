//! What clip envelopes are about, without reference to the engine or the screen: what their
//! values mean, finding and replacing them, and keeping Linked ones with the audio (manual
//! ch. 27).
//!
//! Automation mode envelopes hold plain parameter values, as track automation does. Modulation
//! mode envelopes only influence those, so theirs are a different kind of number:
//!
//! - A control that modulates by [`ModMode::Scale`] (Volume, Sends) holds a share, 0 to 1, of its
//!   current value: it can pull the control down but never open it further.
//! - One that modulates by [`ModMode::Additive`] or [`ModMode::Intensity`] (device controls, Pan)
//!   holds an offset, -1 to 1, from its current value.
//!
//! Clip envelopes for the clip's own sample are in the units the manual gives them.

use crate::clip::Clip;
use crate::{Breakpoint, ClipContent, ClipEnvelope, EnvMode, Envelope};
use rlive_core::param::{ClipParam, EnvTarget, ModMode};
use rlive_core::warp::{WarpMap, WarpMarker as CoreMarker};
use rlive_core::ParamRange;
use serde::{Deserialize, Serialize};

const fn range(min: f64, max: f64) -> ParamRange {
    ParamRange {
        min,
        max,
        skew: 1.0,
        quantized: false,
    }
}

/// The highest controller a MIDI clip envelope can address (manual p. 542).
pub const MAX_MIDI_CTRL: u8 = 119;
/// How many sixteenths a Sample Offset envelope reaches either way (p. 536).
pub const SAMPLE_OFFSET_REACH: f64 = 8.0;
/// How many semitones the Transposition envelope, and the transposition, reach either way (p. 536).
pub const TRANSPOSE_REACH: f64 = 48.0;

/// The range of a MIDI controller's clip envelope.
pub const MIDI_CTRL_RANGE: ParamRange = range(0.0, 127.0);

/// What a clip envelope for one of the clip's own settings runs over.
pub fn clip_param_range(param: ClipParam) -> ParamRange {
    match param {
        ClipParam::Gain => range(0.0, 1.0),
        ClipParam::Transpose => range(-TRANSPOSE_REACH, TRANSPOSE_REACH),
        ClipParam::SampleOffset => range(-SAMPLE_OFFSET_REACH, SAMPLE_OFFSET_REACH),
    }
}

/// What a modulation envelope for a parameter runs over, and where it does nothing.
pub fn modulation_range(mode: ModMode) -> (ParamRange, f64) {
    match mode {
        ModMode::Scale => (range(0.0, 1.0), 1.0),
        ModMode::Additive | ModMode::Intensity => (range(-1.0, 1.0), 0.0),
    }
}

/// What a clip envelope leaves its target at, with nothing drawn: the value that does nothing.
pub fn clip_param_rest(param: ClipParam) -> f64 {
    match param {
        ClipParam::Gain => 1.0,
        ClipParam::Transpose | ClipParam::SampleOffset => 0.0,
    }
}

/// Whether this envelope belongs to the clip's content, not to a parameter of its track.
pub fn targets_clip_content(target: EnvTarget) -> bool {
    matches!(target, EnvTarget::Clip(_) | EnvTarget::MidiCtrl(_))
}

/// Where "Record Session automation in" records (manual p. 512).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SessionAutomationIn {
    #[default]
    ArmedTracks,
    AllTracks,
}

impl Clip {
    /// The envelope for a target in a mode, if the clip has one.
    pub fn envelope(&self, target: EnvTarget, mode: EnvMode) -> Option<&ClipEnvelope> {
        self.envelopes
            .iter()
            .find(|e| e.envelope.target == target && e.mode == mode)
    }

    pub fn envelope_mut(&mut self, target: EnvTarget, mode: EnvMode) -> Option<&mut ClipEnvelope> {
        self.envelopes
            .iter_mut()
            .find(|e| e.envelope.target == target && e.mode == mode)
    }

    /// Replaces the points of an envelope, making it if there is none. No points deletes it, and
    /// with it any loop it had, which is what Clear Envelope means.
    pub fn set_envelope_points(
        &mut self,
        target: EnvTarget,
        mode: EnvMode,
        points: Vec<Breakpoint>,
    ) {
        let at = self
            .envelopes
            .iter()
            .position(|e| e.envelope.target == target && e.mode == mode);
        match at {
            Some(at) if points.is_empty() => {
                self.envelopes.remove(at);
            }
            Some(at) => self.envelopes[at].envelope.points = points,
            None if points.is_empty() => {}
            None => self.envelopes.push(ClipEnvelope {
                envelope: Envelope { target, points },
                mode,
                loop_region: None,
            }),
        }
    }

    /// The Clip Activator aside, whether this clip plays through a warp stretcher that Sample
    /// Offset can reach: only Beats can, by the manual's account.
    pub fn sample_offset_available(&self) -> bool {
        matches!(
            &self.content,
            ClipContent::Audio { warp, .. } if warp.enabled && warp.mode == crate::WarpMode::Beats
        )
    }
}

/// Keeps a clip's Linked envelopes with the audio after its Warp Markers moved (p. 544): a point
/// stays over the audio it was over. Unlinked envelopes have their own time and don't move. Does
/// nothing without a usable map either side, as there's no audio to stay with.
pub fn retime_linked(
    envelopes: &mut [ClipEnvelope],
    before: &[(f64, f64)],
    after: &[(f64, f64)],
) -> bool {
    let map = |markers: &[(f64, f64)]| {
        WarpMap::new(
            markers
                .iter()
                .map(|&(seconds, tick)| CoreMarker::new(seconds, tick)),
        )
    };
    let (Some(old), Some(new)) = (map(before), map(after)) else {
        return false;
    };
    if old == new {
        return false;
    }
    let mut moved = false;
    for e in envelopes.iter_mut().filter(|e| e.loop_region.is_none()) {
        for p in e.envelope.points.iter_mut() {
            let t = new.src_to_tick(old.tick_to_src(p.time)).max(0.0);
            moved |= t != p.time;
            p.time = t;
        }
    }
    moved
}

/// Turns MIDI controller data, as `(tick, controller, value)` in time order, into clip envelopes,
/// one per controller up to 119 (p. 542). Controller messages hold their value until the next one
/// comes along, which a breakpoint envelope says with a step.
pub fn controller_envelopes(events: &[(f64, u8, u8)]) -> Vec<ClipEnvelope> {
    let mut controllers: Vec<u8> = events
        .iter()
        .map(|e| e.1)
        .filter(|c| *c <= MAX_MIDI_CTRL)
        .collect();
    controllers.sort_unstable();
    controllers.dedup();
    controllers
        .into_iter()
        .map(|cc| {
            let mut points: Vec<Breakpoint> = Vec::new();
            for &(tick, _, value) in events.iter().filter(|e| e.1 == cc) {
                let value = f64::from(value.min(127));
                match points.last().copied() {
                    Some(last) if last.value == value => continue,
                    Some(last) => {
                        if last.time != tick {
                            points.push(Breakpoint::new(tick, last.value));
                        }
                        points.push(Breakpoint::new(tick, value));
                    }
                    None => points.push(Breakpoint::new(tick, value)),
                }
            }
            ClipEnvelope {
                envelope: Envelope {
                    target: EnvTarget::MidiCtrl(cc),
                    points,
                },
                mode: EnvMode::Modulation,
                loop_region: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvLoop;

    fn env(points: &[(f64, f64)], looped: bool) -> ClipEnvelope {
        ClipEnvelope {
            envelope: Envelope {
                target: EnvTarget::MidiCtrl(1),
                points: points.iter().map(|&(t, v)| Breakpoint::new(t, v)).collect(),
            },
            mode: EnvMode::Modulation,
            loop_region: looped.then_some(EnvLoop::new(0, 960)),
        }
    }

    #[test]
    fn linked_envelopes_follow_a_marker_and_unlinked_ones_do_not() {
        let before = [(0.0, 0.0), (1.0, 960.0), (2.0, 1920.0)];
        // Audio that was at beat one is now at tick 480.
        let after = [(0.0, 0.0), (1.0, 480.0), (2.0, 1920.0)];
        let mut envelopes = [
            env(&[(0.0, 0.0), (960.0, 1.0)], false),
            env(&[(960.0, 1.0)], true),
        ];
        assert!(retime_linked(&mut envelopes, &before, &after));
        assert_eq!(envelopes[0].envelope.points[1].time, 480.0);
        assert_eq!(envelopes[1].envelope.points[0].time, 960.0, "unlinked");
    }

    #[test]
    fn nothing_moves_without_warp_markers() {
        let mut envelopes = [env(&[(0.0, 0.0)], false)];
        assert!(!retime_linked(
            &mut envelopes,
            &[],
            &[(0.0, 0.0), (1.0, 480.0)]
        ));
    }

    #[test]
    fn controller_data_becomes_held_steps() {
        let envelopes = controller_envelopes(&[
            (0.0, 1, 10),
            (480.0, 1, 100),
            (480.0, 74, 64),
            (960.0, 1, 100),
            (0.0, 120, 9),
        ]);
        assert_eq!(envelopes.len(), 2, "controller 120 is out of range");
        let points: Vec<(f64, f64)> = envelopes[0]
            .envelope
            .points
            .iter()
            .map(|p| (p.time, p.value))
            .collect();
        assert_eq!(points, [(0.0, 10.0), (480.0, 10.0), (480.0, 100.0)]);
        assert_eq!(envelopes[1].envelope.target, EnvTarget::MidiCtrl(74));
    }

    #[test]
    fn setting_points_makes_and_clearing_deletes() {
        let mut clip = Clip::default();
        let t = EnvTarget::Clip(ClipParam::Gain);
        clip.set_envelope_points(t, EnvMode::Modulation, vec![Breakpoint::new(0.0, 0.5)]);
        assert_eq!(clip.envelopes.len(), 1);
        clip.set_envelope_points(t, EnvMode::Modulation, Vec::new());
        assert!(clip.envelopes.is_empty());
    }

    #[test]
    fn an_old_unlinked_envelope_loads_with_a_loop_switch_on() {
        let e: EnvLoop = ron::from_str("(start:0,length:960)").unwrap();
        assert!(e.looped);
        assert_eq!(e.marker, 0);
    }
}
