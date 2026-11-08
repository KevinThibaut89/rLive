//! Clip data and the per-track clip launcher.

use rlive_core::{MidiEvent, MidiMessage};
use std::sync::Arc;

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
}

/// An audio clip played back without time-stretching ("warp off").
#[derive(Debug, Clone, PartialEq)]
pub struct AudioClip {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: f64,
    /// Loop length in ticks.
    pub length: u64,
    pub looped: bool,
    pub gain: f32,
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
    /// Read position for audio clips, in source frames.
    pub audio_pos: f64,
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
}

/// Block geometry shared by everything rendered in one block.
#[derive(Clone, Copy)]
pub(crate) struct BlockTime {
    pub t0: f64,
    pub ticks_per_sample: f64,
    pub frames: usize,
    pub sample_rate: f64,
    pub tempo: f64,
}

impl BlockTime {
    pub fn t1(&self) -> f64 {
        self.t0 + self.frames as f64 * self.ticks_per_sample
    }

    /// Frame offset of `tick` inside the block, clamped to `0..=frames`.
    pub fn offset(&self, tick: f64) -> usize {
        let off = ((tick - self.t0) / self.ticks_per_sample).round();
        off.clamp(0.0, self.frames as f64) as usize
    }

    fn event_offset(&self, tick: f64) -> u32 {
        self.offset(tick).min(self.frames.saturating_sub(1)) as u32
    }
}

/// Plays one clip at a time and handles quantized launches.
pub(crate) struct ClipPlayer {
    pub playing: Option<Playing>,
    pub pending: Option<Pending>,
    /// Number of sounding instances of each key, so we can send note-offs on stop.
    active: [u8; 128],
}

impl ClipPlayer {
    pub fn new() -> Self {
        Self {
            playing: None,
            pending: None,
            active: [0; 128],
        }
    }

    /// Emit note-offs for every sounding note at `offset`.
    pub fn release_all(&mut self, offset: u32, events: &mut Vec<MidiEvent>) {
        for key in 0..128 {
            if self.active[key] > 0 {
                self.active[key] = 0;
                push_event(
                    events,
                    MidiEvent::new(
                        offset,
                        MidiMessage::NoteOff {
                            channel: 0,
                            key: key as u8,
                            velocity: 0.5,
                        },
                    ),
                );
            }
        }
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
                            audio_pos: 0.0,
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
        let looped = playing.clip.looped();
        let s = playing.start_tick;
        let end = if looped { f64::INFINITY } else { s + len };
        let seg_end = b.min(end);

        match &*playing.clip {
            ClipData::Midi(clip) => {
                let k_first = (((a - s) / len).floor() as i64 - 1).max(0);
                let mut k_last = ((seg_end - s) / len).floor() as i64;
                if !looped {
                    k_last = k_last.min(0);
                }
                for k in k_first..=k_last {
                    let base = s + k as f64 * len;
                    for note in &clip.notes {
                        if note.start as f64 >= len {
                            continue;
                        }
                        let key = note.key as usize & 127;
                        let on = base + note.start as f64;
                        let off = base + ((note.start + note.length.max(1)) as f64).min(len);
                        // Offs first: a note ending exactly where the next begins must not
                        // cut the new one.
                        if off >= a && off < seg_end.max(a) && self.active[key] > 0 {
                            self.active[key] -= 1;
                            push_event(
                                events,
                                MidiEvent::new(
                                    time.event_offset(off),
                                    MidiMessage::NoteOff {
                                        channel: 0,
                                        key: key as u8,
                                        velocity: 0.5,
                                    },
                                ),
                            );
                        }
                        if on >= a && on < seg_end {
                            self.active[key] = self.active[key].saturating_add(1);
                            push_event(
                                events,
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
            ClipData::Audio(clip) => {
                let from = time.offset(a);
                let to = time.offset(seg_end).min(left.len()).min(right.len());
                let ratio = clip.sample_rate / time.sample_rate;
                let loop_frames =
                    len / rlive_core::time::ticks_per_sample(time.tempo, clip.sample_rate);
                let n = clip.left.len().min(clip.right.len());
                for i in from..to {
                    if playing.audio_pos >= loop_frames {
                        if looped {
                            playing.audio_pos -= loop_frames;
                        } else {
                            break;
                        }
                    }
                    let pos = playing.audio_pos;
                    let idx = pos as usize;
                    if idx + 1 < n {
                        let frac = (pos - idx as f64) as f32;
                        let l = clip.left[idx] + (clip.left[idx + 1] - clip.left[idx]) * frac;
                        let r = clip.right[idx] + (clip.right[idx + 1] - clip.right[idx]) * frac;
                        left[i] += l * clip.gain;
                        right[i] += r * clip.gain;
                    }
                    playing.audio_pos += ratio;
                }
            }
        }

        if !looped && b >= end {
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
