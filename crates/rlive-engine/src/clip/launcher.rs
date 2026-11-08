//! The per-track clip launcher.

use super::audio::{AudioCursor, SESSION_VOICE};
use super::data::ClipData;
use super::envelopes::enter_clip;
use super::midi::{chase, Voices};
use super::{render_range, BlockTime};
use rlive_core::time::BarMap;
use rlive_core::warp::StretchBank;
use rlive_core::{
    ClipRegion, FollowActions, LaunchMode, MidiEvent, MidiMessage, NoteSpace, Quantize,
};
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
    /// Song tick at which the clip's run began.
    pub start_tick: f64,
    /// How its run goes through its content: the clip's markers, or a scrub's.
    pub region: ClipRegion,
    pub audio: AudioCursor,
    /// From the velocity it was launched at. Exactly 1 at full velocity.
    pub gain: f32,
}

/// A newer version of the playing clip, waiting for its turn under the Clip Update Rate.
pub(crate) struct ClipUpdate {
    pub clip: Arc<ClipData>,
    pub at: f64,
}

impl Playing {
    pub fn new(slot: usize, clip: Arc<ClipData>, start_tick: f64, gain: f32) -> Self {
        Self {
            slot,
            region: clip.region(),
            clip,
            start_tick,
            audio: AudioCursor::new(SESSION_VOICE),
            gain,
        }
    }

    /// Swaps in a new version of the clip at song tick `tick`, and returns the old one. When its
    /// markers have moved, playback stays where it is in the content if the new markers ever
    /// reach there.
    pub fn replace_clip(&mut self, new: Arc<ClipData>, tick: f64) -> Arc<ClipData> {
        let region = new.region();
        if region != self.clip.region() {
            let now = self.region.content_at((tick - self.start_tick).max(0.0));
            if let Some(run) = region.run_of(now) {
                let moved = tick - run - self.start_tick;
                self.start_tick += moved;
                self.audio.next_local -= moved;
            }
            self.region = region;
        }
        std::mem::replace(&mut self.clip, new)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Pending {
    Launch {
        slot: usize,
        at: f64,
        velocity: f32,
    },
    Stop {
        at: f64,
    },
    /// Jump to a position in a clip, launching it if it isn't playing. With `repeat`, go back
    /// there every so many ticks until it's released (manual p. 228).
    Scrub {
        slot: usize,
        at: f64,
        position: f64,
        repeat: Option<f64>,
    },
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
            Pending::Launch { at, .. } | Pending::Stop { at } | Pending::Scrub { at, .. } => at,
        }
    }

    /// Whether this starts a clip playing, which takes the track off its lane.
    pub fn starts_clip(&self) -> bool {
        matches!(self, Pending::Launch { .. } | Pending::Scrub { .. })
    }

    pub fn set_at(&mut self, tick: f64) {
        match self {
            Pending::Launch { at, .. } | Pending::Stop { at } | Pending::Scrub { at, .. } => {
                *at = tick
            }
        }
    }
}

/// Plays one clip at a time and handles quantized launches.
pub(crate) struct ClipPlayer {
    pub playing: Option<Playing>,
    pub pending: Option<Pending>,
    /// An edit of the playing clip waiting for the Clip Update Rate.
    pub update: Option<ClipUpdate>,
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
            update: None,
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

        if self.update.as_ref().is_some_and(|u| u.at < t1) {
            let at = self
                .update
                .as_ref()
                .map_or(seg_start, |u| u.at.max(time.t0));
            self.render_segment(time, seg_start, at, events, left, right, retired, stretch);
            let off = time.event_offset(at);
            if let Some(update) = self.update.take() {
                if self.playing.is_none() {
                    retired(update.clip);
                } else if let Some(old) = self.replace_playing(update.clip, at) {
                    retired(old);
                    self.release_all(off, events);
                }
            }
            seg_start = at;
        }

        while let Some(p) = self.pending.filter(|p| p.at() < t1) {
            let at = p.at().max(seg_start);
            self.render_segment(time, seg_start, at, events, left, right, retired, stretch);
            let off = time.event_offset(at);
            self.release_all(off, events);
            if let Pending::Scrub {
                slot,
                position,
                repeat,
                ..
            } = p
            {
                let settings = launch.get(slot).copied().unwrap_or_default();
                self.scrub_to(slot, at, position, slots, &settings, off, events, retired);
                self.pending = repeat.filter(|r| *r > 0.0).map(|r| Pending::Scrub {
                    slot,
                    at: at + r,
                    position,
                    repeat,
                });
                self.late = None;
                seg_start = at;
                continue;
            }
            if let Some(u) = self.update.take() {
                retired(u.clip);
            }
            // A legato launch takes over from here, wherever the old clip had got to.
            let carried = self.playing.as_ref().map(|old| carried_position(old, at));
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
                    let region = clip.region();
                    let position = carried
                        .filter(|_| settings.legato)
                        .map_or(0.0, |c| c.run_in(region));
                    let start_tick = at - position;
                    enter_clip(clip, position, off, self.auto_reset, events);
                    let gain = velocity_gain(settings.velocity_amount, velocity);
                    if position > 0.0 && env.chase {
                        let first = events.len();
                        chase(clip, region, start_tick, at, off, &mut self.voices, events);
                        scale_notes(&mut events[first..], gain);
                    }
                    self.arm_follow(&settings, region, at, start_tick, slot);
                    self.playing = Some(Playing::new(slot, clip.clone(), start_tick, gain));
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
        region: ClipRegion,
        at: f64,
        start_tick: f64,
        slot: usize,
    ) {
        let rule = settings.follow;
        if !rule.enabled || region.loop_len <= 0.0 {
            return;
        }
        let due = if rule.linked {
            start_tick + linked_length(region, rule.multiplier)
        } else {
            at + rule.time.max(1) as f64
        };
        self.follow = Some(ArmedFollow {
            at: due,
            slot,
            rule,
        });
    }

    /// Swaps in a new version of the playing clip at song tick `tick`, returning the old one. A
    /// linked Follow Action stays on the end of the clip if that has moved.
    pub fn replace_playing(&mut self, new: Arc<ClipData>, tick: f64) -> Option<Arc<ClipData>> {
        let playing = self.playing.as_mut()?;
        let was = playing.clip.region();
        let old = playing.replace_clip(new, tick);
        if playing.clip.region() != was {
            if let Some(f) = self.follow.as_mut().filter(|f| f.rule.linked) {
                let region = playing.region;
                let mut due = playing.start_tick + linked_length(region, f.rule.multiplier);
                if region.looped && region.loop_len > 0.0 {
                    while due <= tick {
                        due += region.loop_len;
                    }
                }
                f.at = due.max(tick);
            }
        }
        Some(old)
    }

    /// Playback jumps to `position` in the clip in `slot`, which starts playing at full velocity
    /// if it wasn't. A clip already playing keeps its Follow Action and its level.
    #[allow(clippy::too_many_arguments)]
    fn scrub_to(
        &mut self,
        slot: usize,
        at: f64,
        position: f64,
        slots: &[Option<Arc<ClipData>>],
        settings: &SlotLaunch,
        off: u32,
        events: &mut Vec<MidiEvent>,
        retired: &mut dyn FnMut(Arc<ClipData>),
    ) {
        let starting = self.playing.as_ref().is_none_or(|p| p.slot != slot);
        if starting {
            if let Some(old) = self.playing.take() {
                retired(old.clip);
            }
            self.follow = None;
            self.repeat = None;
            if let Some(Some(clip)) = slots.get(slot) {
                enter_clip(clip, 0.0, off, self.auto_reset, events);
                let gain = velocity_gain(settings.velocity_amount, 1.0);
                self.playing = Some(Playing::new(slot, clip.clone(), at, gain));
            }
        }
        let Some(p) = self.playing.as_mut() else {
            return;
        };
        let region = p.clip.region();
        match region.run_of(position) {
            Some(run) => {
                p.region = region;
                p.start_tick = at - run;
            }
            // Where the clip's own run never goes: it departs from there instead.
            None => {
                p.region = region.from_position(position);
                p.start_tick = at;
            }
        }
        p.audio.jump();
        self.changed_at = at;
        let start_tick = p.start_tick;
        if starting {
            self.arm_follow(settings, region, at, start_tick, slot);
        }
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
            playing.region,
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

        let end = playing.start_tick + playing.region.play_length();
        if !playing.region.looped && b >= end {
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
            Some(Pending::Launch { slot: s, .. } | Pending::Scrub { slot: s, .. }) => s == slot,
            Some(Pending::Stop { .. }) => false,
            None => self.playing.as_ref().is_some_and(|p| p.slot == slot),
        }
    }

    /// Position of the playhead within the playing clip, in ticks.
    pub fn position(&self, tick: f64) -> f64 {
        match &self.playing {
            Some(p) if p.clip.length() > 0 => {
                let run = (tick - p.start_tick).max(0.0);
                if p.region.is_whole() {
                    run % p.region.loop_len
                } else {
                    p.region.content_at(run)
                }
            }
            _ => 0.0,
        }
    }
}

/// How long a clip plays before its linked Follow Action: its first pass, then round the loop.
fn linked_length(region: ClipRegion, multiplier: u32) -> f64 {
    let times = multiplier.max(1) as f64;
    if region.looped {
        region.lead() + region.loop_len * times
    } else {
        region.play_length() * times
    }
}

/// Where a clip had got to when a Legato launch takes over from it.
#[derive(Clone, Copy)]
struct Carried {
    /// How far through a pass it was, wrapped to its length.
    run: f64,
    /// The position in its content.
    content: f64,
    plain: bool,
}

impl Carried {
    /// The run of a clip with `region` that puts it at the same place.
    fn run_in(self, region: ClipRegion) -> f64 {
        if self.plain && region.is_whole() {
            return self.run;
        }
        if let Some(run) = region.run_of(self.content) {
            return run;
        }
        if !region.looped || region.loop_len <= 0.0 {
            return 0.0;
        }
        // Outside the new clip's loop: the same distance into it.
        let wrapped =
            region.loop_start + (self.content - region.loop_start).rem_euclid(region.loop_len);
        region.run_of(wrapped).unwrap_or(0.0)
    }
}

/// Where `playing` is at song tick `at`.
fn carried_position(playing: &Playing, at: f64) -> Carried {
    let len = playing.clip.length() as f64;
    let elapsed = (at - playing.start_tick).max(0.0);
    Carried {
        run: if len > 0.0 { elapsed % len } else { 0.0 },
        content: playing.region.content_at(elapsed),
        plain: playing.region.is_whole(),
    }
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
