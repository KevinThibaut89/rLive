//! Polyphonic voice allocation: stealing, retrigger, mono and legato play, unison, glide.
//!
//! The allocator decides which voices play and tells the device through [`VoiceEvent`]s; the
//! device owns the oscillators and envelopes. Glide and per-note expression are tracked here
//! and read back with [`VoiceAllocator::pitch`].

use rlive_core::{ExprKind, MidiMessage, NoteId};

const HELD_CAPACITY: usize = 32;
const CHANNELS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VoiceMode {
    #[default]
    Poly,
    /// One note at a time. Every new note restarts the voices.
    Mono,
    /// One note at a time. A note played while another is held only changes the pitch.
    Legato,
}

/// Which note gives way when there are no voices left.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StealMode {
    #[default]
    Oldest,
    /// The quietest, going by [`VoiceAllocator::set_level`].
    Quietest,
    Lowest,
    Highest,
    /// Nothing gives way: the new note is dropped.
    None,
}

/// Which held note a mono voice plays.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NotePriority {
    #[default]
    Last,
    Low,
    High,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GlideMode {
    #[default]
    Off,
    Always,
    /// Only into a note played while another is held.
    LegatoOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GlideCurve {
    /// Every glide takes `glide_time` seconds.
    #[default]
    ConstantTime,
    /// A glide takes `glide_time` seconds per octave.
    ConstantRate,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VoiceSettings {
    pub mode: VoiceMode,
    /// Notes that can sound at once in poly mode.
    pub polyphony: usize,
    /// Voices each note takes.
    pub unison: usize,
    pub steal: StealMode,
    /// A repeated key restarts the voices already playing it, instead of taking new ones.
    pub retrigger: bool,
    pub priority: NotePriority,
    pub glide: GlideMode,
    pub glide_curve: GlideCurve,
    pub glide_time: f32,
    /// Semitones at a full channel pitch bend.
    pub bend_range: f32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            mode: VoiceMode::Poly,
            polyphony: 16,
            unison: 1,
            steal: StealMode::Oldest,
            retrigger: false,
            priority: NotePriority::Last,
            glide: GlideMode::Off,
            glide_curve: GlideCurve::ConstantTime,
            glide_time: 0.0,
            bend_range: 2.0,
        }
    }
}

/// What a voice has been asked to play.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct NoteInfo {
    pub key: u8,
    pub channel: u8,
    pub note_id: NoteId,
    pub velocity: f32,
    /// Which of the note's unison voices this is.
    pub unison_index: u8,
    pub unison_count: u8,
    /// Where this voice sits in the unison spread, `-1.0..=1.0`. Scale it for detune and pan.
    pub spread: f32,
}

/// What a device should do with a voice.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VoiceEvent {
    /// Restart this voice with `note`. When `stolen` it was still sounding, so fade rather than
    /// cut.
    Start {
        voice: usize,
        note: NoteInfo,
        stolen: bool,
    },
    /// Change the pitch to `note` without restarting anything.
    Legato { voice: usize, note: NoteInfo },
    /// Start this voice's release.
    Release { voice: usize },
    /// Silence this voice now; a stolen voice that got no new note.
    Kill { voice: usize },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VoiceState {
    #[default]
    Free,
    Held,
    Released,
}

/// What the allocator knows about one voice.
#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub state: VoiceState,
    pub note: NoteInfo,
    /// Per-note pressure, 0..1, starting at zero.
    pub pressure: f32,
    /// Per-note slide (MPE Y), 0..1, starting at zero.
    pub slide: f32,
    group: u32,
    age: u64,
    /// The gliding pitch, in semitones.
    pitch: f32,
    target: f32,
    step: f32,
    /// Semitones on top of the key: note-on tuning, later replaced by per-note pitch. A tuning
    /// system's offset is in both, as every source of tuning has to add it to Pitch values too.
    note_pitch: f32,
    /// How much of `note_pitch` is the tuning system's, as the note started.
    tuning: f32,
    level: f32,
    sustained: bool,
    stolen: bool,
}

const EMPTY_NOTE: NoteInfo = NoteInfo {
    key: 0,
    channel: 0,
    note_id: 0,
    velocity: 0.0,
    unison_index: 0,
    unison_count: 1,
    spread: 0.0,
};

const EMPTY_VOICE: Voice = Voice {
    state: VoiceState::Free,
    note: EMPTY_NOTE,
    pressure: 0.0,
    slide: 0.0,
    group: 0,
    age: 0,
    pitch: 60.0,
    target: 60.0,
    step: 0.0,
    note_pitch: 0.0,
    tuning: 0.0,
    level: 0.0,
    sustained: false,
    stolen: false,
};

#[derive(Clone, Copy, Debug)]
struct HeldNote {
    key: u8,
    channel: u8,
    note_id: NoteId,
    velocity: f32,
    tuning: f32,
}

/// See the [module docs](self). Everything is sized at construction.
#[derive(Clone, Debug)]
pub struct VoiceAllocator {
    voices: Box<[Voice]>,
    settings: VoiceSettings,
    sample_rate: f32,
    held: [HeldNote; HELD_CAPACITY],
    held_count: usize,
    next_group: u32,
    next_age: u64,
    last_pitch: Option<f32>,
    channel_bend: [f32; CHANNELS],
    channel_pressure: [f32; CHANNELS],
    sustain_pedal: bool,
    mono_sustained: bool,
}

impl VoiceAllocator {
    /// An allocator with room for `max_voices`, counting every unison voice.
    pub fn new(max_voices: usize) -> Self {
        Self {
            voices: vec![EMPTY_VOICE; max_voices.max(1)].into_boxed_slice(),
            settings: VoiceSettings::default(),
            sample_rate: 48_000.0,
            held: [HeldNote {
                key: 0,
                channel: 0,
                note_id: 0,
                velocity: 0.0,
                tuning: 0.0,
            }; HELD_CAPACITY],
            held_count: 0,
            next_group: 1,
            next_age: 1,
            last_pitch: None,
            channel_bend: [0.0; CHANNELS],
            channel_pressure: [0.0; CHANNELS],
            sustain_pedal: false,
            mono_sustained: false,
        }
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(1.0) as f32;
    }

    pub fn set(&mut self, settings: &VoiceSettings) {
        self.settings = *settings;
    }

    pub fn settings(&self) -> &VoiceSettings {
        &self.settings
    }

    pub fn capacity(&self) -> usize {
        self.voices.len()
    }

    pub fn voice(&self, index: usize) -> &Voice {
        &self.voices[index]
    }

    pub fn voices(&self) -> &[Voice] {
        &self.voices
    }

    /// The device reports each voice's level so [`StealMode::Quietest`] has something to go on.
    pub fn set_level(&mut self, voice: usize, level: f32) {
        if let Some(v) = self.voices.get_mut(voice) {
            v.level = level;
        }
    }

    /// The device calls this when a voice has fallen silent, usually when its amp envelope
    /// finishes, to make it available again.
    pub fn finished(&mut self, voice: usize) {
        if let Some(v) = self.voices.get_mut(voice) {
            if v.state == VoiceState::Released {
                v.state = VoiceState::Free;
            }
        }
    }

    /// Free every voice and forget every held note, without events.
    pub fn reset(&mut self) {
        self.voices.fill(EMPTY_VOICE);
        self.held_count = 0;
        self.sustain_pedal = false;
        self.mono_sustained = false;
        self.last_pitch = None;
        self.channel_bend = [0.0; CHANNELS];
        self.channel_pressure = [0.0; CHANNELS];
    }

    /// A voice's pitch now, in semitones (MIDI note numbers), with glide, per-note pitch and
    /// channel bend. Add the unison detune on top, from [`NoteInfo::spread`].
    #[inline]
    pub fn pitch(&self, voice: usize) -> f32 {
        let v = &self.voices[voice];
        let bend = self.channel_bend[v.note.channel as usize % CHANNELS];
        v.pitch + v.note_pitch + bend * self.settings.bend_range
    }

    /// The per-note pitch part of [`pitch`](Self::pitch) that comes from expression, in semitones.
    /// For a device with its own per-note bend range or switch: a tuning system's offset is left
    /// out of it, so such a switch never turns the tuning off.
    #[inline]
    pub fn note_bend(&self, voice: usize) -> f32 {
        let v = &self.voices[voice];
        v.note_pitch - v.tuning
    }

    /// The part of a voice's pitch set by the tuning system, in semitones.
    #[inline]
    pub fn tuning(&self, voice: usize) -> f32 {
        self.voices[voice].tuning
    }

    /// The channel pitch bend on a voice's channel, -1..1, before `bend_range`.
    #[inline]
    pub fn channel_bend(&self, voice: usize) -> f32 {
        self.channel_bend[self.voices[voice].note.channel as usize % CHANNELS]
    }

    /// Channel pressure on a voice's channel, 0..1.
    pub fn channel_pressure(&self, voice: usize) -> f32 {
        self.channel_pressure[self.voices[voice].note.channel as usize % CHANNELS]
    }

    /// Move glides on by `frames` samples.
    pub fn advance(&mut self, frames: usize) {
        let frames = frames as f32;
        for v in self.voices.iter_mut() {
            if v.state != VoiceState::Free && v.step > 0.0 {
                let distance = v.target - v.pitch;
                let travel = v.step * frames;
                if distance.abs() <= travel {
                    v.pitch = v.target;
                    v.step = 0.0;
                } else {
                    v.pitch += travel.copysign(distance);
                }
            }
        }
    }

    /// Handle one MIDI message, reporting what voices should do to `on`.
    pub fn handle(&mut self, message: MidiMessage, mut on: impl FnMut(VoiceEvent)) {
        match message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                note_id,
                tuning,
            } => {
                if velocity <= 0.0 {
                    self.note_off(channel, key, note_id, &mut on);
                } else {
                    let note = HeldNote {
                        key,
                        channel,
                        note_id,
                        velocity,
                        tuning,
                    };
                    match self.settings.mode {
                        VoiceMode::Poly => self.note_on_poly(note, &mut on),
                        _ => self.note_on_mono(note, &mut on),
                    }
                }
            }
            MidiMessage::NoteOff {
                channel,
                key,
                note_id,
                ..
            } => self.note_off(channel, key, note_id, &mut on),
            MidiMessage::NoteExpr {
                channel,
                key,
                note_id,
                kind,
                value,
            } => self.expression(channel, key, note_id, kind, value),
            MidiMessage::ControlChange { cc: 64, value, .. } => {
                self.set_pedal(value >= 64, &mut on)
            }
            MidiMessage::ControlChange { cc: 120 | 123, .. } | MidiMessage::AllNotesOff => {
                self.all_off(&mut on)
            }
            MidiMessage::PitchBend { channel, value } => {
                self.channel_bend[channel as usize % CHANNELS] = value;
            }
            MidiMessage::ChannelPressure { channel, value } => {
                self.channel_pressure[channel as usize % CHANNELS] = value;
            }
            _ => {}
        }
    }

    fn unison_count(&self) -> usize {
        self.settings.unison.clamp(1, self.voices.len())
    }

    fn glide_for(&self, target: f32, from: f32, overlapped: bool) -> (f32, f32) {
        let glide = match self.settings.glide {
            GlideMode::Off => false,
            GlideMode::Always => true,
            GlideMode::LegatoOnly => overlapped,
        };
        let time = self.settings.glide_time;
        if !glide || time <= 0.0 || from == target {
            return (target, 0.0);
        }
        let distance = (target - from).abs();
        let step = match self.settings.glide_curve {
            GlideCurve::ConstantTime => distance / (time * self.sample_rate),
            GlideCurve::ConstantRate => 12.0 / (time * self.sample_rate),
        };
        (from, step.max(1e-9))
    }

    fn matches(voice: &Voice, channel: u8, key: u8, note_id: NoteId) -> bool {
        if note_id != 0 && voice.note.note_id != 0 {
            voice.note.note_id == note_id
        } else {
            voice.note.key == key && voice.note.channel == channel
        }
    }

    fn fill(
        voice: &mut Voice,
        note: HeldNote,
        (index, count): (usize, usize),
        (group, age): (u32, u64),
        (pitch, step): (f32, f32),
    ) -> NoteInfo {
        let spread = if count > 1 {
            2.0 * index as f32 / (count - 1) as f32 - 1.0
        } else {
            0.0
        };
        voice.note = NoteInfo {
            key: note.key,
            channel: note.channel,
            note_id: note.note_id,
            velocity: note.velocity,
            unison_index: index as u8,
            unison_count: count as u8,
            spread,
        };
        voice.state = VoiceState::Held;
        voice.pressure = 0.0;
        voice.slide = 0.0;
        voice.group = group;
        voice.age = age;
        voice.pitch = pitch;
        voice.target = note.key as f32;
        voice.step = step;
        voice.note_pitch = note.tuning;
        voice.tuning = note.tuning;
        voice.sustained = false;
        voice.stolen = false;
        voice.note
    }

    fn take_tags(&mut self) -> (u32, u64) {
        let tags = (self.next_group, self.next_age);
        self.next_group = self.next_group.wrapping_add(1);
        self.next_age += 1;
        tags
    }

    // ------------------------------------------------------------------------------- poly

    fn active_groups(&self) -> usize {
        let mut count = 0;
        for (i, v) in self.voices.iter().enumerate() {
            let first = v.state != VoiceState::Free
                && !self.voices[..i]
                    .iter()
                    .any(|o| o.state != VoiceState::Free && o.group == v.group);
            count += first as usize;
        }
        count
    }

    fn free_count(&self) -> usize {
        self.voices
            .iter()
            .filter(|v| v.state == VoiceState::Free)
            .count()
    }

    fn steal_candidate(&self) -> Option<u32> {
        // Notes already releasing go first, oldest first.
        let released = self
            .voices
            .iter()
            .filter(|v| v.state == VoiceState::Released)
            .min_by_key(|v| v.age);
        if let Some(v) = released {
            return Some(v.group);
        }
        let held = || self.voices.iter().filter(|v| v.state == VoiceState::Held);
        let voice = match self.settings.steal {
            StealMode::Oldest => held().min_by_key(|v| v.age),
            StealMode::Lowest => held().min_by_key(|v| (v.note.key, v.age)),
            StealMode::Highest => held().max_by_key(|v| (v.note.key, u64::MAX - v.age)),
            StealMode::Quietest => {
                held().min_by(|a, b| a.level.total_cmp(&b.level).then(a.age.cmp(&b.age)))
            }
            StealMode::None => None,
        };
        voice.map(|v| v.group)
    }

    fn steal_group(&mut self, group: u32) {
        for v in self.voices.iter_mut() {
            if v.state != VoiceState::Free && v.group == group {
                v.state = VoiceState::Free;
                v.stolen = true;
            }
        }
    }

    fn note_on_poly(&mut self, note: HeldNote, on: &mut impl FnMut(VoiceEvent)) {
        let unison = self.unison_count();
        let overlapped = self.voices.iter().any(|v| v.state == VoiceState::Held);
        let target = note.key as f32;
        let from = self.last_pitch.unwrap_or(target);
        let (pitch, step) = self.glide_for(target, from, overlapped);

        if self.settings.retrigger {
            let same = self
                .voices
                .iter()
                .find(|v| {
                    v.state != VoiceState::Free
                        && v.note.key == note.key
                        && v.note.channel == note.channel
                })
                .map(|v| v.group);
            if let Some(group) = same {
                let (_, age) = self.take_tags();
                let mut index = 0;
                for (i, voice) in self.voices.iter_mut().enumerate() {
                    if voice.state != VoiceState::Free && voice.group == group {
                        let count = voice.note.unison_count as usize;
                        let info =
                            Self::fill(voice, note, (index, count), (group, age), (pitch, step));
                        index += 1;
                        on(VoiceEvent::Start {
                            voice: i,
                            note: info,
                            stolen: true,
                        });
                    }
                }
                self.last_pitch = Some(target);
                return;
            }
        }

        let groups_allowed = self
            .settings
            .polyphony
            .clamp(1, self.voices.len() / unison.max(1))
            .max(1);
        // Each pass frees one group, so this can't run longer than there are voices.
        for _ in 0..=self.voices.len() {
            if self.active_groups() < groups_allowed && self.free_count() >= unison {
                break;
            }
            match self.steal_candidate() {
                Some(group) => self.steal_group(group),
                None => return,
            }
        }
        if self.free_count() < unison {
            return;
        }

        let (group, age) = self.take_tags();
        let mut placed = 0;
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if placed < unison && voice.state == VoiceState::Free {
                let stolen = voice.stolen;
                let info = Self::fill(voice, note, (placed, unison), (group, age), (pitch, step));
                placed += 1;
                on(VoiceEvent::Start {
                    voice: i,
                    note: info,
                    stolen,
                });
            }
        }
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if voice.stolen {
                voice.stolen = false;
                on(VoiceEvent::Kill { voice: i });
            }
        }
        self.last_pitch = Some(target);
    }

    fn release_voice(voice: &mut Voice, index: usize, on: &mut impl FnMut(VoiceEvent)) {
        voice.state = VoiceState::Released;
        voice.sustained = false;
        on(VoiceEvent::Release { voice: index });
    }

    fn note_off_poly(
        &mut self,
        channel: u8,
        key: u8,
        note_id: NoteId,
        on: &mut impl FnMut(VoiceEvent),
    ) {
        // The oldest matching note goes first, so stacked repeats of a key end in order.
        let Some(group) = self
            .voices
            .iter()
            .filter(|v| {
                v.state == VoiceState::Held
                    && !v.sustained
                    && Self::matches(v, channel, key, note_id)
            })
            .min_by_key(|v| v.age)
            .map(|v| v.group)
        else {
            return;
        };
        let pedal = self.sustain_pedal;
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if voice.state == VoiceState::Held && voice.group == group {
                if pedal {
                    voice.sustained = true;
                } else {
                    Self::release_voice(voice, i, on);
                }
            }
        }
    }

    // ------------------------------------------------------------------------------- mono

    fn pick_held(&self) -> Option<HeldNote> {
        let held = &self.held[..self.held_count];
        match self.settings.priority {
            NotePriority::Last => held.last().copied(),
            NotePriority::Low => held.iter().min_by_key(|n| n.key).copied(),
            NotePriority::High => held.iter().max_by_key(|n| n.key).copied(),
        }
    }

    fn switch_mono(&mut self, note: HeldNote, overlapped: bool, on: &mut impl FnMut(VoiceEvent)) {
        let unison = self.unison_count();
        let target = note.key as f32;
        let sounding = self.voices[..unison]
            .iter()
            .any(|v| v.state != VoiceState::Free);
        let legato = self.settings.mode == VoiceMode::Legato && overlapped && sounding;
        if legato {
            for voice in self.voices[..unison].iter_mut() {
                voice.note.key = note.key;
                voice.note.channel = note.channel;
                voice.note.note_id = note.note_id;
                voice.note.velocity = note.velocity;
                voice.note_pitch = note.tuning;
                voice.tuning = note.tuning;
                voice.state = VoiceState::Held;
                voice.target = target;
            }
            let from = self.voices[0].pitch;
            let (pitch, step) = self.glide_for(target, from, true);
            for (i, voice) in self.voices[..unison].iter_mut().enumerate() {
                voice.pitch = pitch;
                voice.step = step;
                on(VoiceEvent::Legato {
                    voice: i,
                    note: voice.note,
                });
            }
        } else {
            let from = if sounding {
                self.voices[0].pitch
            } else {
                self.last_pitch.unwrap_or(target)
            };
            let (pitch, step) = self.glide_for(target, from, overlapped);
            let (group, age) = self.take_tags();
            for (i, voice) in self.voices[..unison].iter_mut().enumerate() {
                let stolen = voice.state != VoiceState::Free;
                let info = Self::fill(voice, note, (i, unison), (group, age), (pitch, step));
                on(VoiceEvent::Start {
                    voice: i,
                    note: info,
                    stolen,
                });
            }
            for i in unison..self.voices.len() {
                if self.voices[i].state != VoiceState::Free {
                    self.voices[i].state = VoiceState::Free;
                    on(VoiceEvent::Kill { voice: i });
                }
            }
        }
        self.last_pitch = Some(target);
    }

    fn note_on_mono(&mut self, note: HeldNote, on: &mut impl FnMut(VoiceEvent)) {
        let overlapped = self.held_count > 0;
        if self.held_count == HELD_CAPACITY {
            self.held.copy_within(1.., 0);
            self.held_count -= 1;
        }
        self.held[self.held_count] = note;
        self.held_count += 1;
        self.mono_sustained = false;
        if let Some(chosen) = self.pick_held() {
            let already_playing = overlapped
                && self.voices[0].state == VoiceState::Held
                && self.voices[0].note.key == chosen.key
                && self.voices[0].note.channel == chosen.channel;
            if !already_playing {
                self.switch_mono(chosen, overlapped, on);
            }
        }
    }

    fn note_off_mono(
        &mut self,
        channel: u8,
        key: u8,
        note_id: NoteId,
        on: &mut impl FnMut(VoiceEvent),
    ) {
        let held = &self.held[..self.held_count];
        let found = held.iter().position(|n| {
            if note_id != 0 && n.note_id != 0 {
                n.note_id == note_id
            } else {
                n.key == key && n.channel == channel
            }
        });
        let Some(index) = found else {
            return;
        };
        self.held.copy_within(index + 1..self.held_count, index);
        self.held_count -= 1;
        if self.held_count == 0 {
            if self.sustain_pedal {
                self.mono_sustained = true;
            } else {
                self.release_mono(on);
            }
            return;
        }
        if let Some(chosen) = self.pick_held() {
            let v = &self.voices[0];
            if v.note.key != chosen.key || v.note.channel != chosen.channel {
                self.switch_mono(chosen, true, on);
            }
        }
    }

    fn release_mono(&mut self, on: &mut impl FnMut(VoiceEvent)) {
        self.mono_sustained = false;
        let unison = self.unison_count();
        for (i, voice) in self.voices[..unison].iter_mut().enumerate() {
            if voice.state == VoiceState::Held {
                Self::release_voice(voice, i, on);
            }
        }
    }

    // ----------------------------------------------------------------------------- shared

    fn note_off(&mut self, channel: u8, key: u8, note_id: NoteId, on: &mut impl FnMut(VoiceEvent)) {
        match self.settings.mode {
            VoiceMode::Poly => self.note_off_poly(channel, key, note_id, on),
            _ => self.note_off_mono(channel, key, note_id, on),
        }
    }

    fn expression(&mut self, channel: u8, key: u8, note_id: NoteId, kind: ExprKind, value: f32) {
        for voice in self.voices.iter_mut() {
            if voice.state != VoiceState::Free && Self::matches(voice, channel, key, note_id) {
                match kind {
                    ExprKind::Pitch => voice.note_pitch = value,
                    ExprKind::Slide => voice.slide = value,
                    ExprKind::Pressure => voice.pressure = value,
                }
            }
        }
    }

    fn set_pedal(&mut self, down: bool, on: &mut impl FnMut(VoiceEvent)) {
        self.sustain_pedal = down;
        if down {
            return;
        }
        if self.mono_sustained {
            self.release_mono(on);
        }
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if voice.state == VoiceState::Held && voice.sustained {
                Self::release_voice(voice, i, on);
            }
        }
    }

    fn all_off(&mut self, on: &mut impl FnMut(VoiceEvent)) {
        self.held_count = 0;
        self.sustain_pedal = false;
        self.mono_sustained = false;
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if voice.state == VoiceState::Held {
                Self::release_voice(voice, i, on);
            }
        }
    }
}
