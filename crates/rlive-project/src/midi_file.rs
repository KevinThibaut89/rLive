//! Standard MIDI Files: what Export MIDI Clip writes and Import MIDI File reads.
//!
//! A file holds notes on one timeline. Everything else in a file (controllers, pitch bends, later
//! tempo changes) is skipped on the way in, since a clip keeps nothing but its notes.

use crate::clip::{Clip, NoteData};
use rlive_core::{TimeSignature, PPQ};

/// What a file holds, in the file's own ticks.
#[derive(Debug, Clone, PartialEq)]
pub struct MidiFile {
    pub division: u16,
    pub name: String,
    /// Beats per minute, from the first tempo event.
    pub tempo: Option<f64>,
    pub signature: Option<TimeSignature>,
    /// Ascending by start.
    pub notes: Vec<MidiNote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiNote {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    /// 1..127.
    pub velocity: u8,
    pub channel: u8,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MidiFileError {
    #[error("not a MIDI file")]
    NotMidi,
    #[error("the MIDI file ends too soon")]
    Truncated,
    #[error("MIDI files timed in SMPTE frames can't be imported")]
    Smpte,
}

/// The file a clip exports as. A deactivated note isn't heard, so it isn't written.
pub fn from_clip(clip: &Clip, tempo: f64, signature: TimeSignature) -> MidiFile {
    let mut notes: Vec<MidiNote> = clip
        .notes()
        .iter()
        .filter(|n| !n.muted && n.key < 128)
        .map(|n| MidiNote {
            start: n.start,
            length: n.length.max(1),
            key: n.key,
            velocity: (n.velocity * 127.0).round().clamp(1.0, 127.0) as u8,
            channel: 0,
        })
        .collect();
    notes.sort_by_key(|n| (n.start, n.key));
    MidiFile {
        division: PPQ as u16,
        name: clip.name.clone(),
        tempo: Some(tempo),
        signature: Some(signature),
        notes,
    }
}

/// A clip of a file's notes, a whole number of bars long.
pub fn to_clip(file: &MidiFile, name: &str, color: [u8; 3], signature: TimeSignature) -> Clip {
    let scale = |t: u64| (t as u128 * PPQ as u128 / file.division.max(1) as u128) as u64;
    let notes: Vec<NoteData> = file
        .notes
        .iter()
        .map(|n| {
            NoteData::new(
                scale(n.start),
                scale(n.length).max(1),
                n.key,
                n.velocity as f32 / 127.0,
            )
        })
        .collect();
    let bar = signature.ticks_per_bar();
    let end = notes.iter().map(|n| n.start + n.length).max().unwrap_or(0);
    let mut clip = Clip::new_midi(name, color, end.div_ceil(bar).max(1), signature);
    if let Some(store) = clip.notes_mut() {
        *store = notes;
    }
    clip
}

fn push_var(out: &mut Vec<u8>, mut value: u64) {
    let mut bytes = [0u8; 10];
    let mut n = 0;
    loop {
        bytes[n] = (value & 0x7f) as u8;
        n += 1;
        value >>= 7;
        if value == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        out.push(bytes[i] | if i == 0 { 0 } else { 0x80 });
    }
}

/// The bytes of a format 0 file.
pub fn write(file: &MidiFile) -> Vec<u8> {
    let mut track = Vec::new();
    if !file.name.is_empty() {
        let name = file.name.as_bytes();
        track.extend_from_slice(&[0, 0xff, 0x03]);
        push_var(&mut track, name.len() as u64);
        track.extend_from_slice(name);
    }
    if let Some(bpm) = file.tempo {
        let micros = (60_000_000.0 / bpm.max(1.0)).round().min(0xff_ffff as f64) as u32;
        track.extend_from_slice(&[0, 0xff, 0x51, 3]);
        track.extend_from_slice(&micros.to_be_bytes()[1..]);
    }
    if let Some(sig) = file.signature {
        let power = sig.denominator.max(1).trailing_zeros() as u8;
        track.extend_from_slice(&[0, 0xff, 0x58, 4, sig.numerator, power, 24, 8]);
    }
    // At one tick, a note's end comes before the next one's start.
    let mut events: Vec<(u64, u8, u8, u8)> = Vec::with_capacity(file.notes.len() * 2);
    for n in &file.notes {
        events.push((n.start, 1, n.key, n.velocity));
        events.push((n.start + n.length.max(1), 0, n.key, 0));
    }
    let channel_of = |key: u8, start: u64| {
        file.notes
            .iter()
            .find(|n| n.key == key && n.start <= start)
            .map_or(0, |n| n.channel)
    };
    events.sort_unstable();
    let mut at = 0;
    for (time, on, key, velocity) in events {
        push_var(&mut track, time - at);
        at = time;
        let channel = channel_of(key, time) & 0x0f;
        if on == 1 {
            track.extend_from_slice(&[0x90 | channel, key, velocity]);
        } else {
            track.extend_from_slice(&[0x80 | channel, key, 0x40]);
        }
    }
    track.extend_from_slice(&[0, 0xff, 0x2f, 0]);

    let mut out = Vec::with_capacity(track.len() + 22);
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&file.division.to_be_bytes());
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&(track.len() as u32).to_be_bytes());
    out.extend_from_slice(&track);
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, MidiFileError> {
        let b = *self.bytes.get(self.at).ok_or(MidiFileError::Truncated)?;
        self.at += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&[u8], MidiFileError> {
        let end = self.at.checked_add(n).ok_or(MidiFileError::Truncated)?;
        let s = self
            .bytes
            .get(self.at..end)
            .ok_or(MidiFileError::Truncated)?;
        self.at = end;
        Ok(s)
    }

    fn var(&mut self) -> Result<u64, MidiFileError> {
        let mut value = 0u64;
        for _ in 0..8 {
            let b = self.byte()?;
            value = (value << 7) | (b & 0x7f) as u64;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(MidiFileError::Truncated)
    }

    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }
}

fn u32_be(b: &[u8]) -> usize {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
}

/// Reads a file, merging all its tracks onto one timeline.
pub fn parse(bytes: &[u8]) -> Result<MidiFile, MidiFileError> {
    let mut r = Reader { bytes, at: 0 };
    if bytes.len() < 14 || r.take(4)? != b"MThd" {
        return Err(MidiFileError::NotMidi);
    }
    let header_len = u32_be(r.take(4)?);
    if header_len < 6 {
        return Err(MidiFileError::NotMidi);
    }
    let header = r.take(header_len)?.to_vec();
    let division = u16::from_be_bytes([header[4], header[5]]);
    if division & 0x8000 != 0 {
        return Err(MidiFileError::Smpte);
    }
    let mut file = MidiFile {
        division,
        name: String::new(),
        tempo: None,
        signature: None,
        notes: Vec::new(),
    };
    while !r.done() {
        let Ok(id) = r.take(4).map(<[u8]>::to_vec) else {
            break;
        };
        let len = u32_be(r.take(4)?);
        let body = r.take(len)?;
        if id == b"MTrk" {
            read_track(body, &mut file)?;
        }
    }
    file.notes.sort_by_key(|n| (n.start, n.key));
    Ok(file)
}

fn read_track(body: &[u8], file: &mut MidiFile) -> Result<(), MidiFileError> {
    let mut r = Reader { bytes: body, at: 0 };
    let mut tick = 0u64;
    let mut status = 0u8;
    // Notes sounding, by channel and key, oldest first.
    let mut open: Vec<(u8, u8, u64, u8)> = Vec::new();
    while !r.done() {
        tick += r.var()?;
        let mut first = r.byte()?;
        if first < 0x80 {
            // Running status: this byte was the first data byte.
            r.at -= 1;
            first = status;
        } else if first < 0xf0 {
            status = first;
        }
        match first {
            0xff => {
                let kind = r.byte()?;
                let len = r.var()? as usize;
                let data = r.take(len)?;
                match kind {
                    0x03 if file.name.is_empty() => {
                        file.name = String::from_utf8_lossy(data).into_owned();
                    }
                    0x51 if file.tempo.is_none() && data.len() == 3 => {
                        let micros = u32::from_be_bytes([0, data[0], data[1], data[2]]);
                        if micros > 0 {
                            file.tempo = Some(60_000_000.0 / micros as f64);
                        }
                    }
                    0x58 if file.signature.is_none() && data.len() >= 2 => {
                        file.signature = Some(TimeSignature {
                            numerator: data[0],
                            denominator: 1u8.checked_shl(data[1] as u32).unwrap_or(4),
                        });
                    }
                    0x2f => break,
                    _ => {}
                }
            }
            0xf0 | 0xf7 => {
                let len = r.var()? as usize;
                r.take(len)?;
            }
            s if (0x80..0xf0).contains(&s) => {
                let channel = s & 0x0f;
                match s & 0xf0 {
                    0xc0 | 0xd0 => {
                        r.byte()?;
                    }
                    kind => {
                        let (key, velocity) = (r.byte()? & 0x7f, r.byte()? & 0x7f);
                        match kind {
                            0x90 if velocity > 0 => open.push((channel, key, tick, velocity)),
                            0x80 | 0x90 => {
                                if let Some(i) =
                                    open.iter().position(|o| o.0 == channel && o.1 == key)
                                {
                                    let (_, _, start, velocity) = open.remove(i);
                                    file.notes.push(MidiNote {
                                        start,
                                        length: (tick - start).max(1),
                                        key,
                                        velocity,
                                        channel,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => return Err(MidiFileError::NotMidi),
        }
    }
    // A note nobody ended stops with its track.
    for (channel, key, start, velocity) in open {
        file.notes.push(MidiNote {
            start,
            length: (tick - start).max(1),
            key,
            velocity,
            channel,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig() -> TimeSignature {
        TimeSignature {
            numerator: 3,
            denominator: 4,
        }
    }

    fn clip() -> Clip {
        let mut c = Clip::new_midi("Riff", [1, 2, 3], 2, sig());
        let notes = c.notes_mut().unwrap();
        notes.push(NoteData::new(0, 480, 60, 1.0));
        notes.push(NoteData::new(480, 480, 60, 0.5));
        notes.push(NoteData::new(240, 960, 67, 0.25));
        let mut quiet = NoteData::new(0, 480, 72, 1.0);
        quiet.muted = true;
        notes.push(quiet);
        c
    }

    #[test]
    fn a_clip_survives_being_written_and_read_back() {
        let bytes = write(&from_clip(&clip(), 128.0, sig()));
        let file = parse(&bytes).unwrap();
        assert_eq!(file.division, 960);
        assert_eq!(file.name, "Riff");
        assert_eq!(file.signature, Some(sig()));
        assert!((file.tempo.unwrap() - 128.0).abs() < 0.01);
        let got: Vec<_> = file
            .notes
            .iter()
            .map(|n| (n.start, n.length, n.key, n.velocity))
            .collect();
        assert_eq!(
            got,
            [(0, 480, 60, 127), (240, 960, 67, 32), (480, 480, 60, 64)],
            "the deactivated note stays out"
        );
    }

    #[test]
    fn a_repeated_note_ends_before_the_next_starts() {
        // The end of the first C and the start of the second are on the same tick.
        let file = parse(&write(&from_clip(&clip(), 120.0, sig()))).unwrap();
        let c: Vec<_> = file.notes.iter().filter(|n| n.key == 60).collect();
        assert_eq!(c[0].length, 480);
        assert_eq!(c[1].start, 480);
    }

    #[test]
    fn importing_rescales_to_our_ticks_and_rounds_up_to_bars() {
        let file = MidiFile {
            division: 96,
            name: String::new(),
            tempo: None,
            signature: None,
            notes: vec![MidiNote {
                start: 96,
                length: 480,
                key: 40,
                velocity: 127,
                channel: 3,
            }],
        };
        let c = to_clip(&file, "x", [0; 3], sig());
        assert_eq!(c.notes()[0].start, 960);
        assert_eq!(c.notes()[0].length, 4800);
        assert_eq!(c.length, 2 * sig().ticks_per_bar());
    }

    #[test]
    fn running_status_and_zero_velocity_note_offs_are_understood() {
        let track = [
            0, 0x90, 60, 100, // on
            96, 60, 0, // running status, note off by velocity 0
            0, 0xff, 0x2f, 0,
        ];
        let mut bytes = b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk".to_vec();
        bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&track);
        let f = parse(&bytes).unwrap();
        assert_eq!(f.notes.len(), 1);
        assert_eq!(f.notes[0].length, 96);
    }

    #[test]
    fn rubbish_is_refused() {
        assert_eq!(parse(b"hello there world"), Err(MidiFileError::NotMidi));
        assert_eq!(parse(b"MThd"), Err(MidiFileError::NotMidi));
        let smpte = b"MThd\0\0\0\x06\0\0\0\x01\xe7\x28";
        assert_eq!(parse(smpte), Err(MidiFileError::Smpte));
    }
}
