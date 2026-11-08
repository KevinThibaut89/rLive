//! The per-track clip launcher.

use super::audio::{AudioCursor, SESSION_VOICE};
use super::data::ClipData;
use super::envelopes::enter_clip;
use super::midi::{chase, Voices};
use super::{render_range, BlockTime};
use rlive_core::time::BarMap;
use rlive_core::warp::StretchBank;
use rlive_core::{FollowActions, LaunchMode, MidiEvent, MidiMessage, NoteSpace, Quantize};
use std::sync::Arc;

/// What a slot does when launched, kept next to its clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotLaunch {
    pub mode: LaunchMode,
    /// `None` follows the global quantization.
    pub quantize: Option<Quantize>,
    pub legato: bool,
    /// How much launch velocity lowers the clip's volume, 0..1.
    pub velocity_amount: f32,
    /// Off: launching the clip does nothing.
    pub active: bool,
    /// Off, on an empty slot: launching its scene leaves the track alone.
    pub stop_button: bool,
    pub follow: FollowActions,
}

impl Default for SlotLaunch {
    fn default() -> Self {
        Self {
            mode: LaunchMode::Trigger,
            quantize: None,
            legato: false,
            velocity_amount: 0.0,
            active: true,
            stop_button: true,
            follow: FollowActions::default(),
        }
    }
}

/// The gain a clip plays at, launched at `velocity` with this Velocity Amount.
pub fn velocity_gain(amount: f32, velocity: f32) -> f32 {
    1.0 - amount.clamp(0.0, 1.0) * (1.0 - velocity.clamp(0.0, 1.0))
}

pub(crate) struct Playing {
    pub slot: usize,
    pub clip: Arc<ClipData>,
    /// Song tick at which clip position 0 happened.
    pub start_tick: f64,
    pub audio: AudioCursor,
    /// From the velocity it was launched at. Exactly 1 at full velocity.
    pub gain: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Pending {
    Launch { slot: usize, at: f64, velocity: f32 },
    Stop { at: f64 },
}

/// A Follow Action waiting for its time, armed when its clip launches.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ArmedFollow {
    pub at: f64,
    pub slot: usize,
    pub rule: FollowActions,
}

/// A clip in Repeat mode whose button is held.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Repeat {
    pub slot: usize,
    pub quantize: Quantize,
    pub velocity: f32,
    pub next: f64,
}

/// What a launch needs to know about timing.
#[derive(Clone, Copy)]
pub(crate) struct LaunchEnv<'a> {
    pub bars: &'a BarMap,
    pub global: Quantize,
    pub chase: bool,
}

impl Pending {
    pub fn at(&self) -> f64 {
        match *self {
            Pending::Launch { at, .. } | Pending::Stop { at } => at,
        }
    }

    pub fn set_at(&mut self, tick: f64) {
        match self {
            Pending::Launch { at, .. } | Pending::Stop { at } => *at = tick,
        }
    }
}

/// Plays one clip at a time and handles quantized launches.
pub(crate) struct ClipPlayer {
    pub playing: Option<Playing>,
    pub pending: Option<Pending>,
    /// The next launch, if its clip isn't there, waits for it rather than doing nothing.
    pub hold_for_arrival: bool,
    /// A launch whose clip hadn't arrived when its time came, as `(slot, song tick of position 0)`.
    /// The clip starts from wherever that puts it the moment it does.
    pub late: Option<(usize, f64)>,
    /// A Follow Action waiting for its time.
    pub follow: Option<ArmedFollow>,
    pub repeat: Option<Repeat>,
    /// The button of a Gate or Repeat launch that hasn't started came up: it stops a quantum in.
    pub released_early: bool,
    /// The pending launch came from launching a scene, so Cancel Scene Launch takes it back.
    pub from_scene: bool,
    /// Where the playing clip last changed, for the Arrangement to log.
    pub changed_at: f64,
    /// What the Arrangement has been told is playing: slot and song tick of position 0.
    pub logged: Option<(usize, f64)>,
    voices: Voices,
    /// MIDI Envelope Auto-Reset: each new clip sets controllers back first.
    pub(crate) auto_reset: bool,
}

impl ClipPlayer {
    pub fn new() -> Self {
        Self {
            playing: None,
            pending: None,
            hold_for_arrival: false,
            late: None,
            follow: None,
            repeat: None,
            released_early: false,
            from_scene: false,
            changed_at: 0.0,
            logged: None,
            voices: Voices::new(NoteSpace::Session),
            auto_reset: false,
        }
    }

    /// Emit note-offs for every sounding note at `offset`.
    pub fn release_all(&mut self, offset: u32, events: &mut Vec<MidiEvent>) {
        self.voices.release_all(offset, events);
    }

    /// Render one block: MIDI clips push events, audio clips write into `left`/`right`.
    /// Clips that stop playing are pushed to `retired` so they are freed off the audio thread.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        time: BlockTime,
        events: &mut Vec<MidiEvent>,
        left: &mut [f32],
        right: &mut [f32],
        retired: &mut dyn FnMut(Arc<ClipData>),
        slots: &[Option<Arc<ClipData>>],
        launch: &[SlotLaunch],
        env: LaunchEnv,
        stretch: &mut StretchBank,
    ) {
        let t1 = time.t1();
        let mut seg_start = time.t0;

        while let Some(p) = self.pending.filter(|p| p.at() < t1) {
            let at = p.at().max(seg_start);
            self.render_segment(time, seg_start, at, events, left, right, retired, stretch);
            let off = time.event_offset(at);
            self.release_all(off, events);
            // A legato launch takes over from here, wherever the old clip had got to.
            let carried = self.playing.as_ref().map(|old| position_in(old, at));
            if let Some(old) = self.playing.take() {
                retired(old.clip);
            }
            self.late = None;
            self.follow = None;
            self.changed_at = at;
            self.pending = None;
            self.from_scene = false;
            if let Pending::Launch { slot, velocity, .. } = p {
                if let Some(Some(clip)) = slots.get(slot) {
                    let settings = launch.get(slot).copied().unwrap_or_default();
                    let position = carried.filter(|_| settings.legato).unwrap_or(0.0);
                    let start_tick = at - position;
                    enter_clip(clip, position, off, self.auto_reset, events);
                    let gain = velocity_gain(settings.velocity_amount, velocity);
                    if position > 0.0 && env.chase {
                        let first = events.len();
                        chase(clip, start_tick, at, off, &mut self.voices, events);
                        scale_notes(&mut events[first..], gain);
                    }
                    self.arm_follow(&settings, clip.length() as f64, at, start_tick, slot);
                    self.playing = Some(Playing {
                        slot,
                        clip: clip.clone(),
                        start_tick,
                        audio: AudioCursor::new(SESSION_VOICE),
                        gain,
                    });
                    if self.released_early {
                        // The button came up before the clip began: it plays one quantum.
                        let q = settings.quantize.unwrap_or(env.global);
                        self.pending = Some(Pending::Stop {
                            at: env.bars.next_boundary(at + 1e-3, q),
                        });
                    }
                } else if self.hold_for_arrival {
                    self.late = Some((slot, at));
                }
            } else {
                self.repeat = None;
            }
            self.released_early = false;
            self.hold_for_arrival = false;
            seg_start = at;
        }
        self.render_segment(time, seg_start, t1, events, left, right, retired, stretch);
    }

    /// Arm the clip's Follow Action, counted from where play starts.
    fn arm_follow(
        &mut self,
        settings: &SlotLaunch,
        length: f64,
        at: f64,
        start_tick: f64,
        slot: usize,
    ) {
        let rule = settings.follow;
        if !rule.enabled || length <= 0.0 {
            return;
        }
        let due = if rule.linked {
            start_tick + length * rule.multiplier.max(1) as f64
        } else {
            at + rule.time.max(1) as f64
        };
        self.follow = Some(ArmedFollow {
            at: due,
            slot,
            rule,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_segment(
        &mut self,
        time: BlockTime,
        a: f64,
        b: f64,
        events: &mut Vec<MidiEvent>,
        left: &mut [f32],
        right: &mut [f32],
        retired: &mut dyn FnMut(Arc<ClipData>),
        stretch: &mut StretchBank,
    ) {
        if b <= a {
            return;
        }
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        let len = playing.clip.length() as f64;
        if len <= 0.0 {
            return;
        }
        let first_event = events.len();
        render_range(
            &playing.clip,
            playing.start_tick,
            a,
            b,
            time,
            &mut self.voices,
            &mut playing.audio,
            events,
            left,
            right,
            stretch,
        );
        if playing.gain != 1.0 {
            scale_notes(&mut events[first_event..], playing.gain);
            let (from, to) = (time.offset(a), time.offset(b));
            for s in left[from..to].iter_mut().chain(right[from..to].iter_mut()) {
                *s *= playing.gain;
            }
        }

        let end = playing.start_tick + len;
        if !playing.clip.looped() && b >= end {
            // A one-shot clip reached its end inside this segment.
            let off = time.event_offset(end);
            let old = self.playing.take();
            self.release_all(off, events);
            self.changed_at = end;
            self.repeat = None;
            // Only an action due by the clip's end still happens.
            self.follow = self.follow.filter(|f| f.at <= end + 1e-3);
            if let Some(old) = old {
                retired(old.clip);
            }
        }
    }

    /// How long the playing clip has run, position 0 being zero, in ticks.
    pub fn elapsed(&self, tick: f64) -> f64 {
        self.playing
            .as_ref()
            .map_or(0.0, |p| (tick - p.start_tick).max(0.0))
    }

    /// Whether `slot` is playing, or is on its way, with nothing else on its way instead.
    pub fn is_on(&self, slot: usize) -> bool {
        match self.pending {
            Some(Pending::Launch { slot: s, .. }) => s == slot,
            Some(Pending::Stop { .. }) => false,
            None => self.playing.as_ref().is_some_and(|p| p.slot == slot),
        }
    }

    /// Position of the playhead within the playing clip, in ticks.
    pub fn position(&self, tick: f64) -> f64 {
        match &self.playing {
            Some(p) => {
                let len = p.clip.length() as f64;
                if len <= 0.0 {
                    0.0
                } else {
                    (tick - p.start_tick).max(0.0) % len
                }
            }
            None => 0.0,
        }
    }
}

/// Where `playing` is at song tick `at`, wrapped to its length.
fn position_in(playing: &Playing, at: f64) -> f64 {
    let len = playing.clip.length() as f64;
    if len <= 0.0 {
        return 0.0;
    }
    (at - playing.start_tick).max(0.0) % len
}

fn scale_notes(events: &mut [MidiEvent], gain: f32) {
    if gain == 1.0 {
        return;
    }
    for e in events {
        if let MidiMessage::NoteOn { velocity, .. } = &mut e.message {
            *velocity *= gain;
        }
    }
}
