//! Clip data, stateless clip rendering and the per-track clip launcher.

use rlive_core::{MidiEvent, MidiMessage, SampleBuffer};
use std::sync::Arc;

/// Length of the anti-click ramps applied to audio at discontinuities, in frames.
pub(crate) const FADE_FRAMES: u32 = 64;

/// A note inside a MIDI clip. Positions are in ticks relative to the clip start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiClip {
    /// Loop length in ticks.
    pub length: u64,
    /// Notes sorted by start.
    pub notes: Vec<Note>,
    pub looped: bool,
    /// Longest note in `notes` (at least 1), the lookback needed to find held notes.
    max_len: u64,
}

impl MidiClip {
    /// Build a clip, sorting `notes` by start and key.
    pub fn new(length: u64, mut notes: Vec<Note>, looped: bool) -> Self {
        notes.sort_by_key(|n| (n.start, n.key));
        let max_len = notes.iter().map(|n| n.length.max(1)).max().unwrap_or(1);
        Self {
            length,
            notes,
            looped,
            max_len,
        }
    }
}

/// An audio clip played back without time-stretching ("warp off").
#[derive(Debug, Clone)]
pub struct AudioClip {
    pub sample: Arc<SampleBuffer>,
    /// Loop length in ticks.
    pub length: u64,
    pub looped: bool,
    pub gain: f32,
}

impl PartialEq for AudioClip {
    /// Samples compare by identity, so comparing clips never walks the audio data.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.sample, &other.sample)
            && self.length == other.length
            && self.looped == other.looped
            && self.gain == other.gain
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClipData {
    Midi(MidiClip),
    Audio(AudioClip),
}

impl ClipData {
    pub fn length(&self) -> u64 {
        match self {
            ClipData::Midi(c) => c.length,
            ClipData::Audio(c) => c.length,
        }
    }

    pub fn looped(&self) -> bool {
        match self {
            ClipData::Midi(c) => c.looped,
            ClipData::Audio(c) => c.looped,
        }
    }
}

pub(crate) struct Playing {
    pub slot: usize,
    pub clip: Arc<ClipData>,
    /// Song tick at which clip position 0 happened.
    pub start_tick: f64,
    pub audio: AudioCursor,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Pending {
    Launch { slot: usize, at: f64 },
    Stop { at: f64 },
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

/// Block geometry shared by everything rendered in one block.
#[derive(Clone, Copy)]
pub(crate) struct BlockTime {
    pub t0: f64,
    pub ticks_per_sample: f64,
    /// Tick at the end of the block. Exactly `loop_end` when the block runs up to a loop end,
    /// so float error can never pull a note on the boundary into the block.
    pub end: f64,
    pub frames: usize,
    pub sample_rate: f64,
    pub tempo: f64,
}

impl BlockTime {
    pub fn t1(&self) -> f64 {
        self.end
    }

    /// Frame offset of `tick` inside the block, clamped to `0..=frames`.
    pub fn offset(&self, tick: f64) -> usize {
        let off = ((tick - self.t0) / self.ticks_per_sample).round();
        off.clamp(0.0, self.frames as f64) as usize
    }

    pub fn event_offset(&self, tick: f64) -> u32 {
        self.offset(tick).min(self.frames.saturating_sub(1)) as u32
    }
}

/// Counts sounding instances of each key, so note-offs can be sent on stop.
pub(crate) struct Voices([u8; 128]);

impl Voices {
    pub fn new() -> Self {
        Self([0; 128])
    }

    fn on(&mut self, key: usize) {
        self.0[key] = self.0[key].saturating_add(1);
    }

    /// Returns whether the key was sounding (and a note-off should be sent).
    fn off(&mut self, key: usize) -> bool {
        if self.0[key] > 0 {
            self.0[key] -= 1;
            true
        } else {
            false
        }
    }

    /// Emit note-offs for every sounding note at `offset`.
    pub fn release_all(&mut self, offset: u32, events: &mut Vec<MidiEvent>) {
        for key in 0..128 {
            if self.0[key] > 0 {
                self.0[key] = 0;
                push_event(events, MidiEvent::new(offset, note_off(key as u8)));
            }
        }
    }
}

fn note_off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
    }
}

/// Read position of an audio clip.
pub(crate) struct AudioCursor {
    /// Next read position, in source frames.
    pub src: f64,
    /// Clip-local tick `src` corresponds to.
    pub next_local: f64,
    /// Frames of ramp-in still to apply.
    pub fade_in: u32,
    /// Clip-local tick the region ends at, which audio ramps out towards. Infinite (no ramp)
    /// for Session clips.
    pub region_end: f64,
}

impl AudioCursor {
    pub fn new() -> Self {
        Self {
            src: 0.0,
            next_local: 0.0,
            fade_in: 0,
            region_end: f64::INFINITY,
        }
    }

    /// Enter a new timeline region: the next render reseeds from the clip position.
    pub fn enter(&mut self, region_end: f64) {
        self.next_local = f64::NEG_INFINITY;
        self.region_end = region_end;
    }
}

/// Render clip content for the song-tick range `[a, b)`. MIDI clips push events, audio clips
/// write into `l`/`r` (block-sized buffers). `origin` is the song tick of clip-local 0.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_range(
    clip: &ClipData,
    origin: f64,
    a: f64,
    b: f64,
    time: BlockTime,
    voices: &mut Voices,
    audio: &mut AudioCursor,
    ev: &mut Vec<MidiEvent>,
    l: &mut [f32],
    r: &mut [f32],
) {
    let len = clip.length() as f64;
    if b <= a || len <= 0.0 {
        return;
    }
    let seg_end = if clip.looped() {
        b
    } else {
        b.min(origin + len)
    };
    if seg_end <= a {
        return;
    }
    match clip {
        ClipData::Midi(clip) => render_midi(clip, origin, a, seg_end, time, voices, ev),
        ClipData::Audio(clip) => render_audio(clip, origin, a, seg_end, time, audio, l, r),
    }
}

fn render_midi(
    clip: &MidiClip,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    voices: &mut Voices,
    ev: &mut Vec<MidiEvent>,
) {
    let len = clip.length as f64;
    let k_first = (((a - origin) / len).floor() as i64 - 1).max(0);
    let mut k_last = ((seg_end - origin) / len).floor() as i64;
    if !clip.looped {
        k_last = k_last.min(0);
    }
    let lookback = clip.max_len as f64;
    for k in k_first..=k_last {
        let base = origin + k as f64 * len;
        // One tick of slack keeps float error in `base` from dropping a boundary note; the
        // exact comparisons below decide what actually fires.
        let scan_from = a - base - lookback - 1.0;
        let scan_to = seg_end - base + 1.0;
        let first = clip.notes.partition_point(|n| (n.start as f64) < scan_from);
        for note in &clip.notes[first..] {
            let start = note.start as f64;
            if start > scan_to || start >= len {
                break;
            }
            let key = note.key as usize & 127;
            let on = base + start;
            let off = base + ((note.start + note.length.max(1)) as f64).min(len);
            // Offs first: a note ending exactly where the next begins must not cut the new one.
            if off >= a && off < seg_end && voices.off(key) {
                push_event(
                    ev,
                    MidiEvent::new(time.event_offset(off), note_off(key as u8)),
                );
            }
            if on >= a && on < seg_end {
                voices.on(key);
                push_event(
                    ev,
                    MidiEvent::new(
                        time.event_offset(on),
                        MidiMessage::NoteOn {
                            channel: 0,
                            key: key as u8,
                            velocity: note.velocity,
                        },
                    ),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_audio(
    clip: &AudioClip,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    audio: &mut AudioCursor,
    l: &mut [f32],
    r: &mut [f32],
) {
    let sample = &*clip.sample;
    let len = clip.length as f64;
    let ticks_per_src_frame = rlive_core::time::ticks_per_sample(time.tempo, sample.sample_rate);
    let local_a = a - origin;
    // Continue from where the last render stopped, so a tempo change never moves the read
    // position; only a real discontinuity reseeds it from the clip position.
    if (local_a - audio.next_local).abs() > time.ticks_per_sample * 0.5 {
        let local = if clip.looped {
            local_a.rem_euclid(len)
        } else {
            local_a.max(0.0)
        };
        audio.src = local / ticks_per_src_frame;
        audio.fade_in = FADE_FRAMES;
    }
    audio.next_local = seg_end - origin;

    let from = time.offset(a);
    let to = time.offset(seg_end).min(l.len()).min(r.len());
    let ratio = sample.sample_rate / time.sample_rate;
    let loop_frames = len / ticks_per_src_frame;
    let n = sample.len();
    // A one-shot's content can run out before its region does.
    let fade_end = if clip.looped || audio.region_end.is_infinite() {
        audio.region_end
    } else {
        audio.region_end.min(len)
    };
    for i in from..to {
        if audio.src >= loop_frames {
            if clip.looped {
                audio.src -= loop_frames;
            } else {
                break;
            }
        }
        let pos = audio.src;
        let idx = pos as usize;
        let mut gain = clip.gain;
        if audio.fade_in > 0 {
            gain *= 1.0 - audio.fade_in as f32 / FADE_FRAMES as f32;
            audio.fade_in -= 1;
        }
        let local = local_a + (i - from) as f64 * time.ticks_per_sample;
        let frames_left = (fade_end - local) / time.ticks_per_sample;
        if frames_left < FADE_FRAMES as f64 {
            gain *= (frames_left / FADE_FRAMES as f64).max(0.0) as f32;
        }
        if idx + 1 < n {
            let frac = (pos - idx as f64) as f32;
            l[i] += (sample.left[idx] + (sample.left[idx + 1] - sample.left[idx]) * frac) * gain;
            r[i] += (sample.right[idx] + (sample.right[idx + 1] - sample.right[idx]) * frac) * gain;
        }
        audio.src += ratio;
    }
}

/// Start the MIDI notes that are already sounding at song tick `at`, at frame `offset`.
/// Notes starting exactly at `at` are left to [`render_range`].
pub(crate) fn chase(
    clip: &ClipData,
    origin: f64,
    at: f64,
    offset: u32,
    voices: &mut Voices,
    ev: &mut Vec<MidiEvent>,
) {
    let ClipData::Midi(clip) = clip else {
        return;
    };
    let len = clip.length as f64;
    let local = at - origin;
    if len <= 0.0 || local < 0.0 || (!clip.looped && local >= len) {
        return;
    }
    let base = origin + (local / len).floor() * len;
    let scan_from = at - base - clip.max_len as f64 - 1.0;
    let first = clip.notes.partition_point(|n| (n.start as f64) < scan_from);
    for note in &clip.notes[first..] {
        let start = note.start as f64;
        if base + start >= at || start >= len {
            break;
        }
        let off = base + ((note.start + note.length.max(1)) as f64).min(len);
        if off > at {
            let key = note.key as usize & 127;
            voices.on(key);
            push_event(
                ev,
                MidiEvent::new(
                    offset,
                    MidiMessage::NoteOn {
                        channel: 0,
                        key: key as u8,
                        velocity: note.velocity,
                    },
                ),
            );
        }
    }
}

/// Plays one clip at a time and handles quantized launches.
pub(crate) struct ClipPlayer {
    pub playing: Option<Playing>,
    pub pending: Option<Pending>,
    voices: Voices,
}

impl ClipPlayer {
    pub fn new() -> Self {
        Self {
            playing: None,
            pending: None,
            voices: Voices::new(),
        }
    }

    /// Emit note-offs for every sounding note at `offset`.
    pub fn release_all(&mut self, offset: u32, events: &mut Vec<MidiEvent>) {
        self.voices.release_all(offset, events);
    }

    /// Render one block: MIDI clips push events, audio clips write into `left`/`right`.
    /// Clips that stop playing are pushed to `retired` so they are freed off the audio thread.
    pub fn render(
        &mut self,
        time: BlockTime,
        events: &mut Vec<MidiEvent>,
        left: &mut [f32],
        right: &mut [f32],
        retired: &mut dyn FnMut(Arc<ClipData>),
        slots: &[Option<Arc<ClipData>>],
    ) {
        let t1 = time.t1();
        let mut seg_start = time.t0;

        if let Some(p) = self.pending {
            if p.at() < t1 {
                let at = p.at().max(time.t0);
                self.render_segment(time, seg_start, at, events, left, right, retired);
                let off = time.event_offset(at);
                self.release_all(off, events);
                if let Some(old) = self.playing.take() {
                    retired(old.clip);
                }
                if let Pending::Launch { slot, .. } = p {
                    if let Some(Some(clip)) = slots.get(slot) {
                        self.playing = Some(Playing {
                            slot,
                            clip: clip.clone(),
                            start_tick: at,
                            audio: AudioCursor::new(),
                        });
                    }
                }
                self.pending = None;
                seg_start = at;
            }
        }
        self.render_segment(time, seg_start, t1, events, left, right, retired);
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
        );

        let end = playing.start_tick + len;
        if !playing.clip.looped() && b >= end {
            // A one-shot clip reached its end inside this segment.
            let off = time.event_offset(end);
            let old = self.playing.take();
            self.release_all(off, events);
            if let Some(old) = old {
                retired(old.clip);
            }
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

/// Push without ever growing the vector (the audio thread must not allocate).
#[inline]
pub(crate) fn push_event(events: &mut Vec<MidiEvent>, e: MidiEvent) {
    if events.len() < events.capacity() {
        events.push(e);
    }
}
