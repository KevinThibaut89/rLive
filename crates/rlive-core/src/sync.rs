//! MIDI sync as data: parsing the messages, following a clock, and timecode arithmetic.
//!
//! Nothing here touches a port or a thread, so all of it can be tested with injected messages and
//! run on the audio thread. The engine decides what the results do to the transport.

use crate::PPQ;

/// MIDI Clock ticks per quarter note.
pub const CLOCKS_PER_QUARTER: u64 = 24;
/// Song ticks between two MIDI Clock messages.
pub const TICKS_PER_CLOCK: f64 = PPQ as f64 / CLOCKS_PER_QUARTER as f64;
/// Song ticks per MIDI beat, the unit of Song Position Pointer (a sixteenth note).
pub const TICKS_PER_MIDI_BEAT: f64 = PPQ as f64 / 4.0;

pub const CLOCK: u8 = 0xf8;
pub const START: u8 = 0xfa;
pub const CONTINUE: u8 = 0xfb;
pub const STOP: u8 = 0xfc;
pub const SONG_POSITION: u8 = 0xf2;
pub const QUARTER_FRAME: u8 = 0xf1;

/// A system message that matters for sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMessage {
    Clock,
    Start,
    Continue,
    Stop,
    /// Song Position Pointer, in MIDI beats (sixteenths).
    SongPosition(u16),
    /// One MTC quarter frame: its data byte.
    QuarterFrame(u8),
    /// A full-frame locate: the transport moves but doesn't run.
    FullFrame(Timecode),
}

/// Reads a sync message from raw MIDI, or `None` for anything else.
pub fn parse_sync(bytes: &[u8]) -> Option<SyncMessage> {
    match *bytes {
        [CLOCK, ..] => Some(SyncMessage::Clock),
        [START, ..] => Some(SyncMessage::Start),
        [CONTINUE, ..] => Some(SyncMessage::Continue),
        [STOP, ..] => Some(SyncMessage::Stop),
        [SONG_POSITION, lsb, msb, ..] => Some(SyncMessage::SongPosition(
            u16::from(lsb & 0x7f) | u16::from(msb & 0x7f) << 7,
        )),
        [QUARTER_FRAME, data, ..] => Some(SyncMessage::QuarterFrame(data)),
        [0xf0, 0x7f, _, 0x01, 0x01, hh, mm, ss, ff, 0xf7] => Some(SyncMessage::FullFrame(
            Timecode::from_rate_byte(hh, mm, ss, ff),
        )),
        _ => None,
    }
}

/// How often the tempo is worked out again, in clocks. Often enough to track a change quickly.
const CLOCK_HISTORY: usize = 96;
/// A pause longer than this is a new clock, not a slow one.
const CLOCK_GAP_SECONDS: f64 = 1.0;
/// The tempo the follower reports moves at most this much of the way toward a fresh measure per
/// clock: a clock is jittery, a tempo isn't.
const TEMPO_SMOOTHING: f64 = 0.15;

/// Tracks an incoming MIDI Clock: the tempo from when clocks arrive, the song position from how
/// many have arrived since the host said where it was.
#[derive(Debug, Clone)]
pub struct ClockFollower {
    times: [f64; CLOCK_HISTORY],
    /// How many entries of `times` count.
    filled: usize,
    write: usize,
    last_clock: Option<f64>,
    tempo: Option<f64>,
    /// Song position of the last clock, in ticks.
    position: f64,
    /// Clocks are counted from a Start or Continue.
    running: bool,
    /// A Start came and the next clock is beat zero.
    start_pending: bool,
}

impl Default for ClockFollower {
    fn default() -> Self {
        Self {
            times: [0.0; CLOCK_HISTORY],
            filled: 0,
            write: 0,
            last_clock: None,
            tempo: None,
            position: 0.0,
            running: false,
            start_pending: false,
        }
    }
}

/// What happened to the transport as a result of a message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClockEvent {
    /// The clock started running: from this song position, at the host's tempo.
    Started {
        position: f64,
    },
    Stopped,
    /// The host moved the song position.
    Located {
        position: f64,
    },
}

impl ClockFollower {
    pub fn tempo(&self) -> Option<f64> {
        self.tempo
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether a clock has been seen within the last `CLOCK_GAP_SECONDS` before `now`.
    pub fn is_alive(&self, now: f64) -> bool {
        self.last_clock.is_some_and(|t| now - t < CLOCK_GAP_SECONDS)
    }

    /// Where the song is at `at` seconds, extrapolated from the last clock at the current tempo.
    pub fn position_at(&self, at: f64) -> Option<f64> {
        let (last, tempo) = (self.last_clock?, self.tempo?);
        let ticks_per_second = tempo / 60.0 * PPQ as f64;
        Some(self.position + (at - last).max(0.0) * ticks_per_second)
    }

    /// Feed one message, stamped in seconds on any steady clock. A Clock moves the count and
    /// returns no event unless it completes a Start.
    pub fn feed(&mut self, message: SyncMessage, at: f64) -> Option<ClockEvent> {
        match message {
            SyncMessage::Clock => self.clock(at),
            SyncMessage::Start => {
                self.running = false;
                self.start_pending = true;
                self.position = 0.0;
                None
            }
            SyncMessage::Continue => {
                self.running = true;
                self.start_pending = false;
                Some(ClockEvent::Started {
                    position: self.position,
                })
            }
            SyncMessage::Stop => {
                let was = self.running || self.start_pending;
                self.running = false;
                self.start_pending = false;
                was.then_some(ClockEvent::Stopped)
            }
            SyncMessage::SongPosition(beats) => {
                self.position = f64::from(beats) * TICKS_PER_MIDI_BEAT;
                Some(ClockEvent::Located {
                    position: self.position,
                })
            }
            _ => None,
        }
    }

    fn clock(&mut self, at: f64) -> Option<ClockEvent> {
        if self.last_clock.is_some_and(|t| at - t > CLOCK_GAP_SECONDS) {
            self.filled = 0;
        }
        self.times[self.write] = at;
        self.write = (self.write + 1) % CLOCK_HISTORY;
        self.filled = (self.filled + 1).min(CLOCK_HISTORY);
        self.measure();
        self.last_clock = Some(at);
        if self.start_pending {
            self.start_pending = false;
            self.running = true;
            self.position = 0.0;
            return Some(ClockEvent::Started { position: 0.0 });
        }
        if self.running {
            self.position += TICKS_PER_CLOCK;
        }
        None
    }

    /// The tempo the last clocks imply: from the first and last of them, which keeps the error
    /// of one late clock from counting more than once.
    fn measure(&mut self) {
        if self.filled < 3 {
            return;
        }
        let newest = self.times[(self.write + CLOCK_HISTORY - 1) % CLOCK_HISTORY];
        let oldest = self.times[(self.write + CLOCK_HISTORY - self.filled) % CLOCK_HISTORY];
        let span = newest - oldest;
        if span <= 0.0 {
            return;
        }
        let quarters = (self.filled - 1) as f64 / CLOCKS_PER_QUARTER as f64;
        let bpm = quarters * 60.0 / span;
        self.tempo = Some(match self.tempo {
            Some(t) if (bpm - t).abs() < t * 0.04 => t + (bpm - t) * TEMPO_SMOOTHING,
            _ => bpm,
        });
    }
}

/// How timecode counts frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FrameRate {
    Fps24,
    Fps25,
    Fps2997Drop,
    #[default]
    Fps30,
}

impl FrameRate {
    pub const ALL: [FrameRate; 4] = [
        FrameRate::Fps24,
        FrameRate::Fps25,
        FrameRate::Fps2997Drop,
        FrameRate::Fps30,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FrameRate::Fps24 => "24",
            FrameRate::Fps25 => "25",
            FrameRate::Fps2997Drop => "29.97 Drop",
            FrameRate::Fps30 => "30",
        }
    }

    /// The nominal count of frames in a second of timecode.
    pub fn nominal_fps(self) -> u32 {
        match self {
            FrameRate::Fps24 => 24,
            FrameRate::Fps25 => 25,
            FrameRate::Fps2997Drop | FrameRate::Fps30 => 30,
        }
    }

    /// Real frames per real second.
    pub fn fps(self) -> f64 {
        match self {
            FrameRate::Fps2997Drop => 30000.0 / 1001.0,
            other => other.nominal_fps() as f64,
        }
    }

    /// The two rate bits MTC carries in its hour.
    pub fn bits(self) -> u8 {
        match self {
            FrameRate::Fps24 => 0,
            FrameRate::Fps25 => 1,
            FrameRate::Fps2997Drop => 2,
            FrameRate::Fps30 => 3,
        }
    }

    pub fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => FrameRate::Fps24,
            1 => FrameRate::Fps25,
            2 => FrameRate::Fps2997Drop,
            _ => FrameRate::Fps30,
        }
    }
}

/// A SMPTE time, hours to frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub rate: FrameRate,
}

impl Timecode {
    fn from_rate_byte(hh: u8, mm: u8, ss: u8, ff: u8) -> Self {
        Self {
            hours: hh & 0x1f,
            minutes: mm & 0x3f,
            seconds: ss & 0x3f,
            frames: ff & 0x1f,
            rate: FrameRate::from_bits(hh >> 5),
        }
    }

    /// The timecode as seconds of real time since `00:00:00:00`, read at `rate`.
    pub fn seconds_at(&self, rate: FrameRate) -> f64 {
        let (h, m, s) = (
            f64::from(self.hours),
            f64::from(self.minutes),
            f64::from(self.seconds),
        );
        let f = f64::from(self.frames);
        let mut total = (h * 3600.0 + m * 60.0 + s) * f64::from(rate.nominal_fps()) + f;
        if rate == FrameRate::Fps2997Drop {
            // Two frame numbers are skipped in every minute not divisible by ten.
            let minutes = h * 60.0 + m;
            total -= 2.0 * (minutes - (minutes / 10.0).floor());
        }
        total / rate.fps()
    }

    /// The nearest timecode to `seconds` at `rate`, the inverse of [`seconds_at`](Self::seconds_at).
    pub fn from_seconds(seconds: f64, rate: FrameRate) -> Self {
        let mut frames = (seconds.max(0.0) * rate.fps()).round() as u64;
        let nominal = u64::from(rate.nominal_fps());
        if rate == FrameRate::Fps2997Drop {
            // Frames in a ten-minute block and a normal minute, counted as frame numbers.
            const TEN_MIN: u64 = 17982;
            const MIN: u64 = 1798;
            let d = frames / TEN_MIN;
            let m = frames % TEN_MIN;
            frames += 18 * d;
            if m > 1 {
                frames += 2 * ((m - 2) / MIN);
            }
        }
        Self {
            frames: (frames % nominal) as u8,
            seconds: (frames / nominal % 60) as u8,
            minutes: (frames / nominal / 60 % 60) as u8,
            hours: (frames / nominal / 3600 % 24) as u8,
            rate,
        }
    }

    /// The eight MTC quarter-frame data bytes that carry this time, in the order they're sent.
    pub fn quarter_frames(&self) -> [u8; 8] {
        let f = self.frames;
        let s = self.seconds;
        let m = self.minutes;
        let h = self.hours;
        [
            f & 0x0f,
            0x10 | (f >> 4 & 1),
            0x20 | (s & 0x0f),
            0x30 | (s >> 4 & 3),
            0x40 | (m & 0x0f),
            0x50 | (m >> 4 & 3),
            0x60 | (h & 0x0f),
            0x70 | (h >> 4 & 1) | self.rate.bits() << 1,
        ]
    }

    /// The System Exclusive message that locates a device to this time without running it.
    pub fn full_frame(&self) -> [u8; 10] {
        [
            0xf0,
            0x7f,
            0x7f,
            0x01,
            0x01,
            self.rate.bits() << 5 | (self.hours & 0x1f),
            self.minutes & 0x3f,
            self.seconds & 0x3f,
            self.frames & 0x1f,
            0xf7,
        ]
    }
}

/// Builds timecode out of the quarter frames as they arrive. Needs a run of all eight, in order,
/// to say anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct MtcDecoder {
    nibbles: [u8; 8],
    /// How many quarter frames in a row have been seen, up to 8.
    run: u8,
    last_piece: Option<u8>,
    /// Going backward, for a reversing machine.
    reverse: bool,
}

impl MtcDecoder {
    /// Feed a quarter frame's data. Returns the time carried once the last piece of a run lands.
    /// The time is the frame the first piece went with: 7 quarter frames ago in forward play.
    pub fn feed(&mut self, data: u8) -> Option<Timecode> {
        let piece = data >> 4 & 7;
        let forward = self.last_piece == Some(piece.wrapping_sub(1) & 7);
        let backward = self.last_piece == Some((piece + 1) & 7);
        if forward {
            self.reverse = false;
            self.run = (self.run + 1).min(8);
        } else if backward {
            self.reverse = true;
            self.run = (self.run + 1).min(8);
        } else {
            self.run = 1;
        }
        self.last_piece = Some(piece);
        self.nibbles[usize::from(piece)] = data & 0x0f;
        let ending = if self.reverse { 0 } else { 7 };
        (self.run >= 8 && piece == ending).then(|| self.assemble())
    }

    fn assemble(&self) -> Timecode {
        let n = &self.nibbles;
        Timecode {
            frames: n[0] | (n[1] & 1) << 4,
            seconds: n[2] | (n[3] & 3) << 4,
            minutes: n[4] | (n[5] & 3) << 4,
            hours: n[6] | (n[7] & 1) << 4,
            rate: FrameRate::from_bits(n[7] >> 1),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_messages_are_recognised_and_nothing_else_is() {
        assert_eq!(parse_sync(&[0xf8]), Some(SyncMessage::Clock));
        assert_eq!(parse_sync(&[0xfa]), Some(SyncMessage::Start));
        assert_eq!(parse_sync(&[0xfb]), Some(SyncMessage::Continue));
        assert_eq!(parse_sync(&[0xfc]), Some(SyncMessage::Stop));
        assert_eq!(
            parse_sync(&[0xf2, 0x05, 0x01]),
            Some(SyncMessage::SongPosition(5 + 128))
        );
        assert_eq!(
            parse_sync(&[0xf1, 0x25]),
            Some(SyncMessage::QuarterFrame(0x25))
        );
        assert_eq!(parse_sync(&[0x90, 60, 100]), None);
        assert_eq!(parse_sync(&[0xf0, 1, 2, 0xf7]), None);
    }

    #[test]
    fn a_steady_clock_gives_its_tempo_quickly() {
        let mut f = ClockFollower::default();
        let interval = 60.0 / 128.0 / 24.0;
        for i in 0..96 {
            f.feed(SyncMessage::Clock, i as f64 * interval);
        }
        assert!((f.tempo().unwrap() - 128.0).abs() < 0.01);
    }

    #[test]
    fn a_jittery_clock_still_reads_the_right_tempo() {
        let mut f = ClockFollower::default();
        let interval = 60.0 / 100.0 / 24.0;
        for i in 0..240 {
            // A millisecond either way, as a real interface might.
            let jitter = if i % 2 == 0 { 0.001 } else { -0.001 };
            f.feed(SyncMessage::Clock, i as f64 * interval + jitter);
        }
        assert!((f.tempo().unwrap() - 100.0).abs() < 0.5);
    }

    #[test]
    fn start_makes_the_next_clock_beat_zero_and_clocks_then_count() {
        let mut f = ClockFollower::default();
        let t = 60.0 / 120.0 / 24.0;
        assert_eq!(f.feed(SyncMessage::Start, 0.0), None);
        assert!(!f.is_running());
        assert_eq!(
            f.feed(SyncMessage::Clock, 0.0),
            Some(ClockEvent::Started { position: 0.0 })
        );
        assert!(f.is_running());
        for i in 1..=24 {
            f.feed(SyncMessage::Clock, i as f64 * t);
        }
        // Clock 24 is one quarter on.
        assert_eq!(f.position_at(24.0 * t), Some(960.0));
    }

    #[test]
    fn a_song_position_locates_and_continue_runs_from_it() {
        let mut f = ClockFollower::default();
        assert_eq!(
            f.feed(SyncMessage::SongPosition(16), 0.0),
            Some(ClockEvent::Located { position: 3840.0 })
        );
        assert_eq!(
            f.feed(SyncMessage::Continue, 0.1),
            Some(ClockEvent::Started { position: 3840.0 })
        );
        f.feed(SyncMessage::Clock, 0.2);
        assert_eq!(f.position_at(0.2), None, "no tempo yet");
    }

    #[test]
    fn stop_stops_only_what_was_running() {
        let mut f = ClockFollower::default();
        assert_eq!(f.feed(SyncMessage::Stop, 0.0), None);
        f.feed(SyncMessage::Continue, 0.0);
        assert_eq!(f.feed(SyncMessage::Stop, 0.1), Some(ClockEvent::Stopped));
        assert!(!f.is_running());
    }

    fn tc(h: u8, m: u8, s: u8, f: u8, rate: FrameRate) -> Timecode {
        Timecode {
            hours: h,
            minutes: m,
            seconds: s,
            frames: f,
            rate,
        }
    }

    #[test]
    fn timecode_is_read_as_real_time() {
        let t = tc(1, 2, 3, 12, FrameRate::Fps24);
        assert_eq!(t.seconds_at(FrameRate::Fps24), 3723.0 + 0.5);
        // Non-drop 30 runs at the 30 it says.
        assert_eq!(
            tc(0, 0, 1, 15, FrameRate::Fps30).seconds_at(FrameRate::Fps30),
            1.5
        );
    }

    #[test]
    fn drop_frame_keeps_up_with_real_time() {
        // An hour of 29.97 Drop is an hour of real time, not 3603.6 s.
        let hour = tc(1, 0, 0, 0, FrameRate::Fps2997Drop);
        assert!((hour.seconds_at(FrameRate::Fps2997Drop) - 3600.0).abs() < 0.01);
        // 00:01:00;02 is the first frame after the two dropped at the first minute.
        let after_skip = tc(0, 1, 0, 2, FrameRate::Fps2997Drop);
        let frames = after_skip.seconds_at(FrameRate::Fps2997Drop) * FrameRate::Fps2997Drop.fps();
        assert!((frames - 1800.0).abs() < 1e-6);
    }

    #[test]
    fn timecode_round_trips_through_seconds_at_every_rate() {
        for rate in FrameRate::ALL {
            for frame in (0..200_000).step_by(997) {
                let seconds = frame as f64 / rate.fps();
                let t = Timecode::from_seconds(seconds, rate);
                assert!(
                    (t.seconds_at(rate) - seconds).abs() < 1e-6,
                    "{rate:?} frame {frame}: {t:?}"
                );
            }
        }
    }

    #[test]
    fn quarter_frames_carry_the_time_and_the_rate() {
        for rate in FrameRate::ALL {
            let want = tc(13, 37, 42, 11, rate);
            let mut d = MtcDecoder::default();
            let mut got = None;
            for piece in want.quarter_frames() {
                got = d.feed(piece);
            }
            assert_eq!(got, Some(want), "{rate:?}");
        }
    }

    #[test]
    fn nothing_is_decoded_until_a_full_run_of_eight_lands() {
        let want = tc(0, 0, 10, 3, FrameRate::Fps25);
        let pieces = want.quarter_frames();
        let mut d = MtcDecoder::default();
        // Picking up mid-stream, then a whole run.
        for p in &pieces[3..] {
            assert_eq!(d.feed(*p), None);
        }
        let mut got = None;
        for p in pieces {
            got = d.feed(p);
        }
        assert_eq!(got, Some(want));
    }

    #[test]
    fn a_break_in_the_order_starts_over() {
        let t = tc(0, 0, 10, 3, FrameRate::Fps25);
        let mut d = MtcDecoder::default();
        for p in t.quarter_frames().iter().take(7) {
            d.feed(*p);
        }
        // Piece 7 of a run that lost piece 6: no time yet.
        d.feed(t.quarter_frames()[5]);
        assert_eq!(d.feed(t.quarter_frames()[7]), None);
    }

    #[test]
    fn a_full_frame_message_parses_back() {
        let t = tc(2, 5, 9, 20, FrameRate::Fps2997Drop);
        assert_eq!(parse_sync(&t.full_frame()), Some(SyncMessage::FullFrame(t)));
    }
}
