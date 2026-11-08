//! Looper: records the incoming audio, then plays and overdubs it in step with the Set.
//!
//! From the manual: the transport buttons and Multi-Purpose button, launch quantization while
//! the song runs and immediate action while it doesn't, Clear, Undo and Redo, the Tempo Control,
//! Record Length, Song Control, the x2 and /2 buttons, Speed with its octave buttons, Reverse,
//! Feedback and the four monitoring options. The manual gives no buffer length: ours holds 60
//! seconds of stereo. Chooser labels are the manual's. The tempo that "Set & Follow" computes
//! and the song start and stop requests are published on the view, because a device here cannot
//! reach the transport; see the report.

use super::common::{knob, menu, toggle, values};
use crate::dsp::util::sanitize;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor, Quantize,
    Resource, SampleBuffer, Section, Status, TransportInfo, ViewId,
};
use std::sync::Arc;

pub const TEMPO_CONTROL: u32 = 0;
pub const RECORD_LENGTH: u32 = 1;
pub const RECORD_FLOW: u32 = 2;
pub const SONG_CONTROL: u32 = 3;
pub const QUANTIZATION: u32 = 4;
pub const SPEED: u32 = 5;
pub const REVERSE: u32 = 6;
pub const FEEDBACK: u32 = 7;
pub const MONITOR: u32 = 8;
// Commands: a value rising to 1 is a press. They come after the settings, so that anything
// that sets the first few parameters can't start a recording.
pub const CMD_RECORD: u32 = 9;
pub const CMD_OVERDUB: u32 = 10;
pub const CMD_PLAY: u32 = 11;
pub const CMD_STOP: u32 = 12;
pub const CMD_CLEAR: u32 = 13;
pub const CMD_UNDO: u32 = 14;
pub const CMD_MULTI: u32 = 15;
pub const CMD_DOUBLE: u32 = 16;
pub const CMD_HALVE: u32 = 17;
pub const CMD_OCTAVE_UP: u32 = 18;
pub const CMD_OCTAVE_DOWN: u32 = 19;
const PARAMS: usize = 20;

pub const TEMPO_NONE: usize = 0;
pub const TEMPO_FOLLOW: usize = 1;
pub const TEMPO_SET_FOLLOW: usize = 2;
pub const LENGTH_LABELS: [&str; 7] = [
    "x bars", "1 Bar", "2 Bars", "4 Bars", "8 Bars", "16 Bars", "32 Bars",
];
const LENGTH_BARS: [f64; 7] = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
pub const QUANT_LABELS: [&str; 9] = [
    "Global", "None", "4 Bars", "2 Bars", "1 Bar", "1/2", "1/4", "1/8", "1/16",
];
pub const MONITOR_ALWAYS: usize = 0;
pub const MONITOR_NEVER: usize = 1;
pub const MONITOR_REC_OVR: usize = 2;
pub const MONITOR_REC_OVR_STOP: usize = 3;

const MAX_SECONDS: f64 = 60.0;
const SEAM_FRAMES: usize = 256;
const RAMP_FRAMES: f32 = 96.0;
const DOUBLE_PRESS_SECONDS: f64 = 0.3;
const HOLD_SECONDS: f64 = 2.0;
const UNDO_SWEEP_FRAMES: usize = 16_384;

/// What the display reads: state, position 0..1, length in beats, tempo, quantize pending, undo
/// available, Undo button reads Redo, seconds of buffer used. Then the two requests the host
/// would act on: tempo to set, and 1 to start or 2 to stop the song.
pub const VIEW: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 10;

/// What the device is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    #[default]
    Stopped,
    Recording,
    Overdub,
    Playing,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    Record,
    Overdub,
    Play,
    Stop,
    Reverse,
    OctaveUp,
    OctaveDown,
}

fn params() -> Vec<ParamInfo> {
    let press = |id, name| ParamInfo::toggle(id, name, false);
    vec![
        ParamInfo::stepped(
            TEMPO_CONTROL,
            "Tempo Control",
            &["None", "Follow song tempo", "Set & Follow song tempo"],
            1,
        ),
        ParamInfo::stepped(RECORD_LENGTH, "Record Length", &LENGTH_LABELS, 0),
        ParamInfo::stepped(RECORD_FLOW, "Then", &["Play", "Overdub"], 0),
        ParamInfo::stepped(
            SONG_CONTROL,
            "Song Control",
            &["None", "Start Song", "Start & Stop Song"],
            0,
        ),
        ParamInfo::stepped(QUANTIZATION, "Quantization", &QUANT_LABELS, 0),
        ParamInfo::continuous(SPEED, "Speed", -36.0, 36.0, 0.0, "st"),
        ParamInfo::toggle(REVERSE, "Reverse", false),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 100.0, 100.0, "%"),
        ParamInfo::stepped(
            MONITOR,
            "Input -> Output",
            &["Always", "Never", "Rec/OVR", "Rec/OVR/Stop"],
            MONITOR_ALWAYS,
        ),
        press(CMD_RECORD, "Record"),
        press(CMD_OVERDUB, "Overdub"),
        press(CMD_PLAY, "Play"),
        press(CMD_STOP, "Stop"),
        press(CMD_CLEAR, "Clear"),
        press(CMD_UNDO, "Undo"),
        press(CMD_MULTI, "Multi-Purpose"),
        press(CMD_DOUBLE, "x2"),
        press(CMD_HALVE, "/2"),
        press(CMD_OCTAVE_UP, "Octave Up"),
        press(CMD_OCTAVE_DOWN, "Octave Down"),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Transport",
            items: vec![Item::Panel("looper_transport")],
        },
        Section {
            title: "Setup",
            items: vec![
                menu(TEMPO_CONTROL),
                menu(RECORD_LENGTH),
                menu(RECORD_FLOW),
                menu(SONG_CONTROL),
                menu(QUANTIZATION),
            ],
        },
        Section {
            title: "Sound",
            items: vec![knob(SPEED), toggle(REVERSE), knob(FEEDBACK), menu(MONITOR)],
        },
    ]
}

pub struct Looper {
    v: [f64; PARAMS],
    sr: f32,
    buf: [Vec<f32>; 2],
    undo: [Vec<f32>; 2],
    stamp: Vec<u32>,
    epoch: u32,
    undo_valid: bool,
    /// When on, the next press of Undo restores what the last one removed.
    undo_pending_redo: bool,
    sweep: Option<usize>,
    state: State,
    len: usize,
    pos: f64,
    /// The tempo the loop was captured at, 0 when none is known.
    loop_tempo: f64,
    loop_beats: f64,
    rec_target: usize,
    octave: i32,
    reversed: bool,
    feedback_now: f32,
    pending: Option<(Command, f64)>,
    transport: TransportInfo,
    /// Sample within the block.
    n: usize,
    gain: f32,
    tempo_request: f32,
    song_request: f32,
    /// Frames, not seconds: when the button was last let go.
    last_multi: f64,
    multi_held: f64,
    /// The press is waiting to see whether it's a click or a hold.
    multi_pending: bool,
    frames_seen: f64,
}

impl Default for Looper {
    fn default() -> Self {
        Self {
            v: values(&params()),
            sr: 48_000.0,
            buf: [Vec::new(), Vec::new()],
            undo: [Vec::new(), Vec::new()],
            stamp: Vec::new(),
            epoch: 1,
            undo_valid: false,
            undo_pending_redo: false,
            sweep: None,
            state: State::Stopped,
            len: 0,
            pos: 0.0,
            loop_tempo: 0.0,
            loop_beats: 0.0,
            rec_target: 0,
            octave: 0,
            reversed: false,
            feedback_now: 1.0,
            pending: None,
            transport: TransportInfo {
                sample_rate: 48_000.0,
                ..Default::default()
            },
            n: 0,
            gain: 0.0,
            tempo_request: 0.0,
            song_request: 0.0,
            last_multi: f64::NEG_INFINITY,
            multi_held: 0.0,
            multi_pending: false,
            frames_seen: 0.0,
        }
    }
}

impl Looper {
    fn value(&self, id: u32) -> f64 {
        self.v[id as usize]
    }

    fn capacity(&self) -> usize {
        self.buf[0].len()
    }

    /// The contents of the loop, one slice per channel. For rendering and tests; the host has
    /// no way to reach this on the audio thread.
    pub fn contents(&self) -> [&[f32]; 2] {
        [&self.buf[0][..self.len], &self.buf[1][..self.len]]
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The loop's length in frames.
    pub fn loop_frames(&self) -> usize {
        self.len
    }

    /// The tempo the loop runs at.
    pub fn loop_tempo(&self) -> f64 {
        self.loop_tempo
    }

    /// The song tempo the device wants, or 0 for none, for a host that can set it.
    pub fn tempo_request(&self) -> f32 {
        self.tempo_request
    }

    /// The transport command the device wants: 0 none, 1 start, 2 stop.
    pub fn song_request(&self) -> f32 {
        self.song_request
    }

    fn running(&self) -> bool {
        self.transport.playing
    }

    fn beat_now(&self) -> f64 {
        self.transport.beats() + self.n as f64 * self.transport.tempo / 60.0 / self.sr as f64
    }

    fn beats_per_bar(&self) -> f64 {
        let sig = self.transport.time_signature;
        sig.numerator as f64 * 4.0 / sig.denominator.max(1) as f64
    }

    fn quantize(&self) -> Quantize {
        match self.value(QUANTIZATION).round() as i32 {
            1 => Quantize::None,
            2 => Quantize::FourBars,
            3 => Quantize::TwoBars,
            5 => Quantize::HalfBar,
            6 => Quantize::Beat,
            7 => Quantize::Eighth,
            8 => Quantize::Sixteenth,
            _ => Quantize::Bar,
        }
    }

    /// Runs a transport button now, or at the next launch boundary while the song runs.
    fn request(&mut self, command: Command) {
        if !self.running() {
            self.apply(command);
            return;
        }
        let q = self.quantize();
        let tick = self.transport.tick + self.n as f64 * self.transport.ticks_per_sample();
        let at = q.next_boundary(tick, self.transport.time_signature) / rlive_core::PPQ as f64;
        if q == Quantize::None || (at - self.beat_now()).abs() < 1e-7 {
            self.apply(command);
        } else {
            self.pending = Some((command, at));
        }
    }

    fn apply(&mut self, command: Command) {
        self.pending = None;
        match command {
            Command::Record => self.start_recording(),
            Command::Overdub => self.start_playback(true),
            Command::Play => self.start_playback(false),
            Command::Stop => self.stop(),
            Command::Reverse => self.reversed = self.value(REVERSE) >= 0.5,
            Command::OctaveUp => self.octave = (self.octave + 1).min(4),
            Command::OctaveDown => self.octave = (self.octave - 1).max(-4),
        }
    }

    fn start_recording(&mut self) {
        self.state = State::Recording;
        self.len = 0;
        self.pos = 0.0;
        self.undo_valid = false;
        self.undo_pending_redo = false;
        let bars = LENGTH_BARS[(self.value(RECORD_LENGTH).round() as usize).min(6)];
        self.rec_target = if bars > 0.0 && self.running() {
            ((bars * self.beats_per_bar() * 60.0 / self.transport.tempo.max(1.0)) * self.sr as f64)
                as usize
        } else {
            0
        };
        self.loop_tempo = if self.running() {
            self.transport.tempo
        } else {
            0.0
        };
    }

    /// Closes a recording and works out its tempo and length.
    fn finish_recording(&mut self) {
        self.len = self.pos as usize;
        if self.len == 0 {
            self.state = State::Stopped;
            return;
        }
        self.apply_seam();
        let seconds = self.len as f64 / self.sr as f64;
        let bars = LENGTH_BARS[(self.value(RECORD_LENGTH).round() as usize).min(6)];
        if self.running() {
            self.loop_tempo = self.transport.tempo;
            self.loop_beats = seconds * self.loop_tempo / 60.0;
        } else {
            // A guess in the stopped case: pick the bar count that lands in a sensible tempo.
            let chosen = if bars > 0.0 {
                bars
            } else {
                self.guess_bars(seconds)
            };
            self.loop_beats = chosen * self.beats_per_bar();
            self.loop_tempo = self.loop_beats * 60.0 / seconds.max(1e-6);
            if self.value(TEMPO_CONTROL).round() as usize == TEMPO_SET_FOLLOW {
                self.tempo_request = self.loop_tempo as f32;
            }
        }
        self.pos = 0.0;
        self.feedback_now = self.value(FEEDBACK) as f32 * 0.01;
    }

    fn guess_bars(&self, seconds: f64) -> f64 {
        let per_bar = self.beats_per_bar();
        let mut bars = 1.0;
        for _ in 0..12 {
            let tempo = bars * per_bar * 60.0 / seconds.max(1e-6);
            if tempo >= 80.0 {
                break;
            }
            bars *= 2.0;
        }
        bars
    }

    /// A short fade at the loop's ends, so the seam doesn't click.
    fn apply_seam(&mut self) {
        let frames = SEAM_FRAMES.min(self.len / 2);
        for ch in self.buf.iter_mut() {
            for i in 0..frames {
                let g = i as f32 / frames.max(1) as f32;
                ch[i] *= g;
                ch[self.len - 1 - i] *= g;
            }
        }
    }

    fn start_playback(&mut self, overdub: bool) {
        match self.state {
            State::Recording => {
                self.finish_recording();
            }
            State::Stopped if self.len > 0 => {
                // From stopped the loop starts at its beginning.
                self.pos = 0.0;
            }
            _ => {}
        }
        if self.len == 0 {
            self.state = State::Stopped;
            return;
        }
        self.song_control_start();
        if overdub {
            if self.state != State::Overdub {
                self.begin_overdub();
            }
            self.state = State::Overdub;
        } else {
            self.state = State::Playing;
        }
    }

    fn begin_overdub(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.undo_valid = true;
        self.undo_pending_redo = false;
    }

    fn song_control_start(&mut self) {
        if self.value(SONG_CONTROL).round() as i32 >= 1 {
            self.song_request = 1.0;
        }
    }

    fn stop(&mut self) {
        if self.state == State::Recording {
            self.finish_recording();
        }
        self.state = State::Stopped;
        if self.value(SONG_CONTROL).round() as i32 == 2 {
            self.song_request = 2.0;
        }
    }

    fn clear(&mut self) {
        if self.state == State::Overdub && self.running() {
            // Keep the tempo and the length, lose the audio.
            for ch in self.buf.iter_mut() {
                ch[..self.len].fill(0.0);
            }
            self.undo_valid = false;
        } else {
            self.len = 0;
            self.pos = 0.0;
            self.loop_tempo = 0.0;
            self.loop_beats = 0.0;
            self.state = State::Stopped;
            self.undo_valid = false;
        }
        self.undo_pending_redo = false;
        self.pending = None;
    }

    fn undo_press(&mut self) {
        if !self.undo_valid || self.sweep.is_some() {
            return;
        }
        self.sweep = Some(0);
        self.undo_pending_redo = !self.undo_pending_redo;
    }

    fn run_undo_sweep(&mut self, frames: usize) {
        let Some(mut at) = self.sweep else {
            return;
        };
        let end = (at + frames).min(self.len);
        let [buf_l, buf_r] = &mut self.buf;
        let [undo_l, undo_r] = &mut self.undo;
        for i in at..end {
            if self.stamp[i] == self.epoch {
                std::mem::swap(&mut buf_l[i], &mut undo_l[i]);
                std::mem::swap(&mut buf_r[i], &mut undo_r[i]);
            }
        }
        at = end;
        self.sweep = (at < self.len).then_some(at);
    }

    fn double(&mut self) {
        if self.len == 0 || self.len * 2 > self.capacity() {
            return;
        }
        for ch in self.buf.iter_mut() {
            ch.copy_within(0..self.len, self.len);
        }
        self.len *= 2;
        self.loop_beats *= 2.0;
        self.undo_valid = false;
    }

    fn halve(&mut self) {
        if self.len < 4 {
            return;
        }
        let half = self.len / 2;
        if self.pos as usize >= half {
            for ch in self.buf.iter_mut() {
                ch.copy_within(half..self.len, 0);
            }
            self.pos -= half as f64;
        }
        self.len = half;
        self.loop_beats *= 0.5;
        self.undo_valid = false;
    }

    fn multi_click(&mut self) {
        match self.state {
            State::Recording | State::Overdub => self.request(Command::Play),
            State::Playing => self.request(Command::Overdub),
            State::Stopped => {
                if self.len == 0 {
                    self.request(Command::Record);
                } else {
                    self.request(Command::Play);
                }
            }
        }
    }

    /// A press: straight to work from where a click means something else held. A double press
    /// stops, but the manual also gives Play and Stop a held meaning, so those wait for the
    /// release, or two seconds, to find out which this is.
    fn multi_press(&mut self) {
        let now = self.frames_seen;
        let quick = (now - self.last_multi) / self.sr as f64 <= DOUBLE_PRESS_SECONDS;
        if quick && matches!(self.state, State::Playing | State::Overdub) {
            self.request(Command::Stop);
            return;
        }
        if matches!(self.state, State::Playing) || (self.state == State::Stopped && self.len > 0) {
            self.multi_pending = true;
            self.multi_held = 0.0;
        } else {
            self.multi_click();
        }
    }

    fn multi_release(&mut self) {
        self.last_multi = self.frames_seen;
        if self.multi_pending {
            self.multi_pending = false;
            self.multi_click();
        }
    }

    fn multi_hold_check(&mut self, frames: usize) {
        if !self.multi_pending {
            return;
        }
        self.multi_held += frames as f64;
        if self.multi_held / self.sr as f64 >= HOLD_SECONDS {
            self.multi_pending = false;
            match self.state {
                State::Playing => self.undo_press(),
                State::Stopped => self.clear(),
                _ => {}
            }
        }
    }

    fn command_param(&mut self, id: u32, value: f64) {
        let rising = value >= 0.5 && self.v[id as usize] < 0.5;
        let falling = value < 0.5 && self.v[id as usize] >= 0.5;
        self.v[id as usize] = value;
        if id == CMD_MULTI {
            if rising {
                self.multi_press();
            } else if falling {
                self.multi_release();
            }
            return;
        }
        if !rising {
            return;
        }
        match id {
            CMD_RECORD => self.request(Command::Record),
            CMD_OVERDUB => {
                if self.len == 0 && self.state != State::Recording {
                    self.request(Command::Record);
                } else {
                    self.request(Command::Overdub);
                }
            }
            CMD_PLAY => self.request(Command::Play),
            CMD_STOP => self.request(Command::Stop),
            CMD_CLEAR => self.clear(),
            CMD_UNDO => self.undo_press(),
            CMD_DOUBLE => self.double(),
            CMD_HALVE => self.halve(),
            CMD_OCTAVE_UP => self.request(Command::OctaveUp),
            CMD_OCTAVE_DOWN => self.request(Command::OctaveDown),
            _ => {}
        }
    }

    fn playback_rate(&self) -> f64 {
        let mut rate = 2f64.powf((self.value(SPEED) + 12.0 * self.octave as f64) / 12.0);
        let tempo = self.value(TEMPO_CONTROL).round() as usize;
        if tempo != TEMPO_NONE && self.loop_tempo > 0.0 {
            rate *= self.transport.tempo / self.loop_tempo;
        }
        rate
    }

    #[inline]
    fn read_at(&self, channel: usize, pos: f64) -> f32 {
        let n = self.len;
        let i = (pos as usize).min(n - 1);
        let t = (pos - i as f64) as f32;
        let a = self.buf[channel][i];
        let b = self.buf[channel][(i + 1) % n];
        a + (b - a) * t
    }

    fn overdub_write(&mut self, l: f32, r: f32, fb: f32) {
        let i = (self.pos as usize).min(self.len - 1);
        if self.stamp[i] != self.epoch {
            self.stamp[i] = self.epoch;
            self.undo[0][i] = self.buf[0][i];
            self.undo[1][i] = self.buf[1][i];
        }
        self.buf[0][i] = sanitize(self.buf[0][i] * fb + l);
        self.buf[1][i] = sanitize(self.buf[1][i] * fb + r);
    }

    fn advance(&mut self, rate: f64) {
        let len = self.len as f64;
        if self.reversed {
            self.pos -= rate;
            if self.pos < 0.0 {
                self.pos += len;
                self.feedback_now = self.value(FEEDBACK) as f32 * 0.01;
            }
        } else {
            self.pos += rate;
            if self.pos >= len {
                self.pos -= len;
                self.feedback_now = self.value(FEEDBACK) as f32 * 0.01;
            }
        }
        self.pos = self.pos.clamp(0.0, len - 1e-6);
    }

    fn render(&mut self, audio: AudioBuffer) {
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        let rate = self.playback_rate();
        let monitor = self.value(MONITOR).round() as usize;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if let Some((command, at)) = self.pending {
                if !self.running() || self.beat_now() >= at - 1e-9 {
                    self.apply(command);
                }
            }
            let (in_l, in_r) = (*l, *r);
            let mut out = [0.0f32; 2];
            match self.state {
                State::Recording => {
                    let at = self.pos as usize;
                    if at < self.capacity() {
                        self.buf[0][at] = sanitize(in_l);
                        self.buf[1][at] = sanitize(in_r);
                        self.pos += 1.0;
                    }
                    let finished = (self.rec_target > 0 && self.pos as usize >= self.rec_target)
                        || self.pos as usize >= self.capacity();
                    if finished {
                        let overdub = self.value(RECORD_FLOW).round() as i32 == 1;
                        self.start_playback(overdub);
                    }
                }
                State::Playing | State::Overdub if self.len > 0 => {
                    out[0] = self.read_at(0, self.pos);
                    out[1] = self.read_at(1, self.pos);
                    if self.state == State::Overdub {
                        self.overdub_write(in_l, in_r, self.feedback_now);
                    }
                    self.advance(rate);
                }
                _ => {}
            }
            // A short ramp when playback starts or stops.
            let audible = matches!(self.state, State::Playing | State::Overdub);
            let target = if audible { 1.0 } else { 0.0 };
            self.gain = (self.gain + (target - self.gain) / RAMP_FRAMES).clamp(0.0, 1.0);
            let hear_input = match monitor {
                MONITOR_NEVER => false,
                MONITOR_REC_OVR => matches!(self.state, State::Recording | State::Overdub),
                MONITOR_REC_OVR_STOP => self.state != State::Playing,
                _ => true,
            };
            let input_gain = if hear_input { 1.0 } else { 0.0 };
            *l = out[0] * self.gain + in_l * input_gain;
            *r = out[1] * self.gain + in_r * input_gain;
            self.n += 1;
            self.frames_seen += 1.0;
        }
    }

    fn publish(&self, ctx: &ProcessContext) {
        if !ctx.view.is_active() {
            return;
        }
        let state = match self.state {
            State::Stopped => 0.0,
            State::Recording => 1.0,
            State::Overdub => 2.0,
            State::Playing => 3.0,
        };
        let progress = if self.len > 0 {
            (self.pos / self.len as f64) as f32
        } else {
            0.0
        };
        ctx.view.publish(
            VIEW,
            &[
                state,
                progress,
                self.loop_beats as f32,
                self.loop_tempo as f32,
                f32::from(self.pending.is_some()),
                f32::from(self.undo_valid),
                f32::from(self.undo_pending_redo),
                (self.len as f64 / self.sr as f64) as f32,
                self.tempo_request,
                self.song_request,
            ],
        );
    }
}

impl Processor for Looper {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let n = (sample_rate * MAX_SECONDS) as usize;
        self.buf = [vec![0.0; n], vec![0.0; n]];
        self.undo = [vec![0.0; n], vec![0.0; n]];
        self.stamp = vec![0; n];
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.transport = *ctx.transport;
        self.n = 0;
        let frames = ctx.audio.frames();
        self.run_undo_sweep(UNDO_SWEEP_FRAMES);
        self.multi_hold_check(frames);
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                if (CMD_RECORD..=CMD_OCTAVE_DOWN).contains(&id) {
                    self.command_param(id, value);
                } else {
                    if let Some(v) = self.v.get_mut(id as usize) {
                        *v = value;
                    }
                    if id == REVERSE {
                        // Engaging Reverse waits for the launch boundary while the song runs.
                        self.request(Command::Reverse);
                    }
                }
            }
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
        self.publish(ctx);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == REVERSE {
            self.reversed = value >= 0.5;
        }
    }

    fn reset(&mut self) {
        // Stopping the Set stops the Looper but leaves the recording alone.
        self.state = State::Stopped;
        self.pending = None;
        self.gain = 0.0;
        self.pos = 0.0;
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot == 0 {
            if let Ok(sample) = Arc::clone(&resource).downcast::<SampleBuffer>() {
                let n = sample.left.len().min(self.capacity());
                self.buf[0][..n].copy_from_slice(&sample.left[..n]);
                self.buf[1][..n].copy_from_slice(&sample.right[..n]);
                self.len = n;
                self.pos = 0.0;
                self.loop_tempo = 0.0;
                self.loop_beats = 0.0;
                self.undo_valid = false;
                self.state = State::Stopped;
            }
        }
        Some(resource)
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.looper",
    name: "Looper",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Looper>::default(),
};
