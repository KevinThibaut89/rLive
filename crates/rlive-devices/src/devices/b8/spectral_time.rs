//! Spectral Time: a spectral freezer feeding a spectral delay, either way round (manual pages
//! 673-676).
//!
//! The manual describes Manual and Retrigger freezing (Onsets with Sensitivity, Sync with an
//! Interval in ms or notes), the Crossfade and Envelope fade shapes with up to eight stacked
//! freezes, the delay's Time, Feedback, Shift, Stereo, Dry/Wet, Tilt, Spray and Mask, Resolution
//! and Zero Dry Signal Latency, and Frz > Dly / Dly > Frz. It gives no ranges beyond 100%-style
//! ones, so times, the Resolution values and the Mask curve are ours. Resolution is the STFT size
//! and the latency; the default is the largest value that stays inside a 512-sample block at
//! 48 kHz, which registry conformance requires of a default.

use super::common::{knob, menu, toggle, Spectrogram, SPECTROGRAM_VIEW_LEN};
use crate::devices::b1::common::int_param;
use crate::dsp::fft::Complex32;
use crate::dsp::follower::{DetectMode, EnvelopeFollower};
use crate::dsp::stft::Stft;
use crate::dsp::tempo::SyncRate;
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor, Section,
    Status, Tail, TransportInfo, ViewId,
};
use std::f32::consts::TAU;

pub const FREEZER_ON: u32 = 0;
pub const FREEZE: u32 = 1;
pub const FREEZE_MODE: u32 = 2;
pub const RETRIGGER_MODE: u32 = 3;
pub const SENSITIVITY: u32 = 4;
pub const INTERVAL_UNIT: u32 = 5;
pub const INTERVAL_MS: u32 = 6;
pub const INTERVAL_DIV: u32 = 7;
pub const FADE_SHAPE: u32 = 8;
pub const XFADE: u32 = 9;
pub const FADE_IN: u32 = 10;
pub const FADE_OUT: u32 = 11;
pub const DELAY_ON: u32 = 12;
pub const TIME_MODE: u32 = 13;
pub const TIME_MS: u32 = 14;
pub const TIME_NOTES: u32 = 15;
pub const TIME_16THS: u32 = 16;
pub const FEEDBACK: u32 = 17;
pub const SHIFT: u32 = 18;
pub const STEREO: u32 = 19;
pub const DELAY_MIX: u32 = 20;
pub const TILT: u32 = 21;
pub const SPRAY: u32 = 22;
pub const MASK: u32 = 23;
pub const RESOLUTION: u32 = 24;
pub const INPUT_SEND: u32 = 25;
pub const ORDER: u32 = 26;
pub const DRY_WET: u32 = 27;
pub const ZERO_DRY: u32 = 28;
pub const SPECTROGRAM: u32 = 29;
const PARAMS: usize = 30;

pub const MODE_MANUAL: usize = 0;
pub const MODE_RETRIGGER: usize = 1;
pub const RETRIGGER_ONSETS: usize = 0;
pub const RETRIGGER_SYNC: usize = 1;
pub const UNIT_MS: usize = 0;
pub const UNIT_NOTES: usize = 1;
pub const SHAPE_CROSSFADE: usize = 0;
pub const SHAPE_ENVELOPE: usize = 1;
pub const TIME_MODE_MS: usize = 0;
pub const TIME_MODE_NOTES: usize = 1;
pub const TIME_MODE_16TH: usize = 2;
pub const TIME_MODE_16TH_TRIPLET: usize = 3;
pub const TIME_MODE_16TH_DOTTED: usize = 4;
pub const ORDER_FREEZE_DELAY: usize = 0;
pub const ORDER_DELAY_FREEZE: usize = 1;

/// The Resolution choices, in samples. Every one is also the STFT latency.
pub const RESOLUTIONS: [usize; 6] = [240, 480, 960, 1920, 3840, 7680];
const RESOLUTION_LABELS: [&str; 6] = ["240", "480", "960", "1920", "3840", "7680"];
pub const DEFAULT_RESOLUTION: usize = 1;
/// How many freezes can overlap, in Envelope mode (manual p. 675).
pub const MAX_FREEZES: usize = 8;
pub const MAX_DELAY_SECONDS: f32 = 5.0;

pub const SPECTROGRAM_VIEW: ViewId = ViewId(0);
pub const VIEW_LEN: usize = SPECTROGRAM_VIEW_LEN;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::toggle(FREEZER_ON, "Freezer On", true),
        ParamInfo::toggle(FREEZE, "Freeze", false),
        ParamInfo::stepped(FREEZE_MODE, "Freezer Mode", &["Manual", "Retrigger"], 0),
        ParamInfo::stepped(RETRIGGER_MODE, "Retrigger Mode", &["Onsets", "Sync"], 0),
        ParamInfo::continuous(SENSITIVITY, "Sensitivity", 0.0, 100.0, 50.0, "%"),
        ParamInfo::stepped(INTERVAL_UNIT, "Freezer Time Unit", &["ms", "Notes"], 0),
        ParamInfo::skewed(INTERVAL_MS, "Interval", 10.0, 5_000.0, 500.0, "ms", 3.0),
        ParamInfo::stepped(INTERVAL_DIV, "Interval Notes", &SyncRate::LABELS, 12),
        ParamInfo::stepped(FADE_SHAPE, "Fade Shape", &["Crossfade", "Envelope"], 0),
        ParamInfo::continuous(XFADE, "X-Fade", 0.0, 100.0, 50.0, "%"),
        ParamInfo::skewed(FADE_IN, "Fade In", 0.0, 5_000.0, 20.0, "ms", 3.0),
        ParamInfo::skewed(FADE_OUT, "Fade Out", 0.0, 5_000.0, 200.0, "ms", 3.0),
        ParamInfo::toggle(DELAY_ON, "Delay On", true),
        ParamInfo::stepped(
            TIME_MODE,
            "Mode",
            &["Time", "Notes", "16th", "16th Triplet", "16th Dotted"],
            TIME_MODE_MS,
        ),
        ParamInfo::skewed(TIME_MS, "Time", 10.0, 5_000.0, 300.0, "ms", 3.0),
        ParamInfo::stepped(TIME_NOTES, "Time Notes", &SyncRate::LABELS, 12),
        int_param(TIME_16THS, "Time 16ths", 1.0, 32.0, 3.0),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 100.0, 40.0, "%"),
        ParamInfo::continuous(SHIFT, "Shift", -2_000.0, 2_000.0, 0.0, "Hz"),
        ParamInfo::continuous(STEREO, "Stereo", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(DELAY_MIX, "Delay Dry/Wet", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(TILT, "Tilt", -100.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(SPRAY, "Spray", 0.0, 1_000.0, 0.0, "ms", 2.0),
        ParamInfo::continuous(MASK, "Mask", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(
            RESOLUTION,
            "Resolution",
            &RESOLUTION_LABELS,
            DEFAULT_RESOLUTION,
        ),
        ParamInfo::continuous(INPUT_SEND, "Input Send", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::stepped(ORDER, "Order", &["Frz > Dly", "Dly > Frz"], 0),
        ParamInfo::continuous(DRY_WET, "Dry/Wet", 0.0, 100.0, 50.0, "%"),
        ParamInfo::toggle(ZERO_DRY, "Zero Dry Signal Latency", false),
        ParamInfo::toggle(SPECTROGRAM, "Show Spectrogram", true),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Spectrogram",
            items: vec![
                Item::Panel("spectrogram_time"),
                toggle(SPECTROGRAM),
                menu(RESOLUTION),
            ],
        },
        Section {
            title: "Freezer",
            items: vec![
                toggle(FREEZER_ON),
                toggle(FREEZE),
                menu(FREEZE_MODE),
                menu(RETRIGGER_MODE),
                knob(SENSITIVITY),
                menu(INTERVAL_UNIT),
                knob(INTERVAL_MS),
                menu(INTERVAL_DIV),
                menu(FADE_SHAPE),
                knob(XFADE),
                knob(FADE_IN),
                knob(FADE_OUT),
            ],
        },
        Section {
            title: "Delay",
            items: vec![
                toggle(DELAY_ON),
                menu(TIME_MODE),
                knob(TIME_MS),
                menu(TIME_NOTES),
                knob(TIME_16THS),
                knob(FEEDBACK),
                knob(SHIFT),
                knob(STEREO),
                knob(DELAY_MIX),
                knob(TILT),
                knob(SPRAY),
                knob(MASK),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                knob(INPUT_SEND),
                menu(ORDER),
                toggle(ZERO_DRY),
                knob(DRY_WET),
            ],
        },
    ]
}

/// One captured spectrum, per channel.
#[derive(Clone)]
struct Freeze {
    mag: Vec<f32>,
    phase: Vec<f32>,
    /// How far each bin's phase turns per hop.
    delta: Vec<f32>,
}

/// One channel of the spectral engine, sized for the largest Resolution.
#[derive(Clone)]
struct Channel {
    freezes: Vec<Freeze>,
    prev_phase: Vec<f32>,
    /// The delay line: `frames_per_bin` frames of `bins` values each, at the current Resolution.
    delay: Vec<Complex32>,
    /// How many frames back each bin reads.
    delay_frames: Vec<usize>,
    spray: Vec<f32>,
    delayed: Vec<Complex32>,
    write: usize,
    /// The freezer's output before a feedback delay or the wet mix.
    held: Vec<Complex32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Off,
    FadeIn,
    Hold,
    FadeOut,
}

/// A freeze's envelope, shared by both channels.
#[derive(Clone, Copy, Debug)]
struct Slot {
    stage: Stage,
    env: f32,
    fade_in: f32,
    fade_out: f32,
    /// The freeze falls straight into its fade-out from the peak, for the Envelope shape.
    no_hold: bool,
    age: u32,
}

impl Slot {
    const fn off() -> Self {
        Self {
            stage: Stage::Off,
            env: 0.0,
            fade_in: 1.0,
            fade_out: 1.0,
            no_hold: false,
            age: 0,
        }
    }

    fn advance(&mut self) {
        match self.stage {
            Stage::FadeIn => {
                self.env += self.fade_in;
                if self.env >= 1.0 {
                    self.env = 1.0;
                    self.stage = if self.no_hold {
                        Stage::FadeOut
                    } else {
                        Stage::Hold
                    };
                }
            }
            Stage::FadeOut => {
                self.env -= self.fade_out;
                if self.env <= 0.0 {
                    self.env = 0.0;
                    self.stage = Stage::Off;
                }
            }
            _ => {}
        }
        if self.stage != Stage::Off {
            self.age = self.age.saturating_add(1);
        }
    }
}

/// What one frame does, shared by the two channels.
#[derive(Clone, Copy, Debug, Default)]
struct FrameCommand {
    capture: Option<usize>,
}

pub struct SpectralTime {
    v: [f64; PARAMS],
    sr: f32,
    stft: Vec<[Stft; 2]>,
    channels: Vec<Channel>,
    slots: [Slot; MAX_FREEZES],
    /// The dry signal, delayed to line up with the wet path.
    dry_ring: [Vec<f32>; 2],
    dry_write: usize,
    /// Samples until the next Sync interval, in samples.
    sync_left: f32,
    /// The song beat the last Sync trigger landed on.
    last_interval: Option<i64>,
    freeze_armed: bool,
    since_hop: usize,
    res: usize,
    /// Freezer's time-domain onset detector.
    fast: EnvelopeFollower,
    slow: EnvelopeFollower,
    holdoff: u32,
    onset_pending: bool,
    spectrogram: Option<Spectrogram>,
    /// Where the frequency shift phase has got to, per channel.
    shift_phase: [f32; 2],
    delay_dirty: bool,
    last_tempo: f64,
}

impl Default for SpectralTime {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            stft: Vec::new(),
            channels: Vec::new(),
            slots: [Slot::off(); MAX_FREEZES],
            dry_ring: [Vec::new(), Vec::new()],
            dry_write: 0,
            sync_left: 0.0,
            last_interval: None,
            freeze_armed: false,
            since_hop: 0,
            res: DEFAULT_RESOLUTION,
            fast: EnvelopeFollower::new(),
            slow: EnvelopeFollower::new(),
            holdoff: 0,
            onset_pending: false,
            spectrogram: None,
            shift_phase: [0.0; 2],
            delay_dirty: true,
            last_tempo: 0.0,
        }
    }
}

fn bin_count(size: usize) -> usize {
    size / 2 + 1
}

fn princ_arg(phase: f32) -> f32 {
    phase - TAU * (phase / TAU + 0.5).floor()
}

/// Where a bin sits between the lowest and the highest frequency, on a log scale, -1 to 1.
fn bin_position(bin: usize, bins: usize) -> f32 {
    let top = (bins - 1).max(2) as f32;
    let k = (bin.max(1) as f32).min(top);
    (k.ln() / top.ln()) * 2.0 - 1.0
}

/// How much of Tilt and Spray a bin gets, from the Mask setting (-1 to 1).
/// `mask > 0` keeps only the high frequencies above a split that rises with the amount.
fn mask_weight(position: f32, mask: f32) -> f32 {
    if mask.abs() < 1e-4 {
        return 1.0;
    }
    let split = 2.0 * mask.abs() - 1.0;
    let t = if mask > 0.0 {
        (position - split) / 0.5
    } else {
        (-position - split) / 0.5
    };
    t.clamp(0.0, 1.0)
}

impl SpectralTime {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn index(&self, id: u32) -> usize {
        self.v[id as usize].round().max(0.0) as usize
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn size(&self) -> usize {
        RESOLUTIONS[self.res]
    }

    fn hop(&self) -> usize {
        self.size() / 4
    }

    fn hop_seconds(&self) -> f32 {
        self.hop() as f32 / self.sr
    }

    fn set(&mut self, id: u32, value: f64) {
        if let Some(slot) = self.v.get_mut(id as usize) {
            *slot = value;
            match id {
                RESOLUTION => self.switch_resolution(),
                TIME_MODE | TIME_MS | TIME_NOTES | TIME_16THS | TILT | SPRAY | MASK | STEREO => {
                    self.delay_dirty = true;
                }
                _ => {}
            }
        }
    }

    fn switch_resolution(&mut self) {
        let res = self.index(RESOLUTION).min(RESOLUTIONS.len() - 1);
        if res == self.res || self.stft.is_empty() {
            self.res = res;
            return;
        }
        self.res = res;
        self.clear_state();
    }

    /// Everything that depends on the frame size starts again from silence.
    fn clear_state(&mut self) {
        for pair in self.stft.iter_mut() {
            for s in pair.iter_mut() {
                s.reset();
            }
        }
        for ch in self.channels.iter_mut() {
            ch.prev_phase.fill(0.0);
            ch.delay.fill(Complex32::new(0.0, 0.0));
            ch.write = 0;
            for f in ch.freezes.iter_mut() {
                f.mag.fill(0.0);
                f.phase.fill(0.0);
                f.delta.fill(0.0);
            }
            ch.held.fill(Complex32::new(0.0, 0.0));
        }
        self.slots = [Slot::off(); MAX_FREEZES];
        self.since_hop = 0;
        self.sync_left = 0.0;
        self.last_interval = None;
        self.delay_dirty = true;
        self.shift_phase = [0.0; 2];
    }

    /// Delay time in seconds, from the Mode chooser and the tempo.
    fn delay_seconds(&self, transport: &TransportInfo) -> f32 {
        let beat = 60.0 / transport.tempo.max(1.0) as f32;
        let sixteenth = beat * 0.25;
        match self.index(TIME_MODE) {
            TIME_MODE_NOTES => {
                SyncRate::from_index(self.v[TIME_NOTES as usize]).beats() as f32 * beat
            }
            TIME_MODE_16TH => self.value(TIME_16THS) * sixteenth,
            TIME_MODE_16TH_TRIPLET => self.value(TIME_16THS) * sixteenth * 2.0 / 3.0,
            TIME_MODE_16TH_DOTTED => self.value(TIME_16THS) * sixteenth * 1.5,
            _ => self.value(TIME_MS) * 0.001,
        }
    }

    /// Works out each bin's delay. Tilt skews it, Spray adds a random time, Stereo widens the two
    /// across the channels and Mask limits both.
    fn update_delays(&mut self, transport: &TransportInfo) {
        let hop_seconds = self.hop_seconds();
        let bins = bin_count(self.size());
        let frames = (self.delay_seconds(transport) / hop_seconds).max(1.0);
        let max_frames = self.max_frames() - 1;
        let tilt = (self.value(TILT) * 0.01).clamp(-1.0, 1.0);
        let spray = (self.value(SPRAY) * 0.001) / hop_seconds;
        let mask = (self.value(MASK) * 0.01).clamp(-1.0, 1.0);
        let stereo = (self.value(STEREO) * 0.01).clamp(0.0, 1.0);
        for c in 0..2 {
            let widen = if c == 0 { 1.0 } else { -1.0 };
            for k in 0..bins {
                let position = bin_position(k, bins);
                let weight = mask_weight(position, mask);
                let tilt_here = tilt * (1.0 + widen * stereo);
                let skewed = frames * 2f32.powf(2.0 * tilt_here * position * weight);
                // Stereo draws the right channel's spray from its own table.
                let (own, other) = (self.channels[c].spray[k], self.channels[0].spray[k]);
                let random = if c == 0 {
                    own
                } else {
                    other + (own - other) * stereo
                };
                let jitter = spray * random * weight;
                self.channels[c].delay_frames[k] =
                    ((skewed + jitter).round() as usize).clamp(1, max_frames);
            }
        }
        self.delay_dirty = false;
    }

    fn max_frames(&self) -> usize {
        // Everything fits for the largest bin count at the shortest hop, by construction.
        let size = self.size();
        let bins = bin_count(size);
        (self.channels[0].delay.len() / bins).max(2)
    }

    /// The Sync interval in seconds.
    fn interval_seconds(&self, transport: &TransportInfo) -> f32 {
        if self.index(INTERVAL_UNIT) == UNIT_NOTES {
            let beat = 60.0 / transport.tempo.max(1.0) as f32;
            SyncRate::from_index(self.v[INTERVAL_DIV as usize]).beats() as f32 * beat
        } else {
            self.value(INTERVAL_MS) * 0.001
        }
    }

    fn release_all(&mut self, seconds: f32) {
        let hops = (seconds / self.hop_seconds()).max(1.0);
        let step = 1.0 / hops;
        for s in self.slots.iter_mut() {
            if s.stage != Stage::Off {
                s.fade_out = step;
                s.no_hold = false;
                s.stage = Stage::FadeOut;
            }
        }
    }

    fn free_slot(&self) -> usize {
        // Reuse the slot that has the least to lose: first an empty one, otherwise the quietest.
        if let Some(i) = self.slots.iter().position(|s| s.stage == Stage::Off) {
            return i;
        }
        let mut best = 0;
        for i in 1..MAX_FREEZES {
            if self.slots[i].env < self.slots[best].env
                || (self.slots[i].env == self.slots[best].env
                    && self.slots[i].age > self.slots[best].age)
            {
                best = i;
            }
        }
        best
    }

    /// A new freeze: picks its slot and shape, and fades out what it replaces.
    fn trigger(&mut self, transport: &TransportInfo) -> usize {
        let hop_seconds = self.hop_seconds();
        let manual = self.index(FREEZE_MODE) == MODE_MANUAL;
        let shape = self.index(FADE_SHAPE);
        let slot = self.free_slot();
        let fade_in;
        let fade_out;
        let mut no_hold = false;
        if manual {
            fade_in = self.value(FADE_IN) * 0.001;
            fade_out = self.value(FADE_OUT) * 0.001;
            self.release_all(fade_out);
        } else if shape == SHAPE_ENVELOPE {
            fade_in = self.value(FADE_IN) * 0.001;
            fade_out = self.value(FADE_OUT) * 0.001;
            no_hold = true;
        } else {
            let span = self.value(XFADE) * 0.01 * self.interval_seconds(transport);
            fade_in = span;
            fade_out = span;
            self.release_all(span);
        }
        self.slots[slot] = Slot {
            stage: Stage::FadeIn,
            env: if fade_in < hop_seconds { 1.0 } else { 0.0 },
            fade_in: (hop_seconds / fade_in.max(hop_seconds)).min(1.0),
            fade_out: (hop_seconds / fade_out.max(hop_seconds)).min(1.0),
            no_hold,
            age: 0,
        };
        slot
    }

    /// The control side of a frame: Freeze and Retrigger logic, once per hop for both channels.
    fn frame_command(&mut self, transport: &TransportInfo, frames: usize) -> FrameCommand {
        let mut cmd = FrameCommand::default();
        let armed = self.flag(FREEZER_ON) && self.flag(FREEZE);
        let manual = self.index(FREEZE_MODE) == MODE_MANUAL;
        let mode = self.index(RETRIGGER_MODE);
        let mut should_capture = false;
        if armed && !self.freeze_armed {
            if manual || mode == RETRIGGER_SYNC {
                should_capture = true;
            }
            self.sync_left = self.interval_seconds(transport) * self.sr;
            self.last_interval = None;
        }
        if !armed && self.freeze_armed {
            let fade_out = self.value(FADE_OUT) * 0.001;
            self.release_all(fade_out);
        }
        self.freeze_armed = armed;

        if armed && !manual {
            match mode {
                RETRIGGER_SYNC => {
                    let beats = self.index(INTERVAL_UNIT) == UNIT_NOTES && transport.playing;
                    if beats {
                        let interval = SyncRate::from_index(self.v[INTERVAL_DIV as usize])
                            .beats()
                            .max(1e-3);
                        let at = ((transport.beats() / interval).floor()) as i64;
                        if self.last_interval != Some(at) {
                            if self.last_interval.is_some() {
                                should_capture = true;
                            }
                            self.last_interval = Some(at);
                        }
                    } else {
                        self.sync_left -= frames as f32;
                        if self.sync_left <= 0.0 {
                            should_capture = true;
                            self.sync_left += self.interval_seconds(transport) * self.sr;
                            if self.sync_left <= 0.0 {
                                self.sync_left = self.interval_seconds(transport) * self.sr;
                            }
                        }
                    }
                }
                _ => {
                    if std::mem::take(&mut self.onset_pending) {
                        should_capture = true;
                    }
                }
            }
        } else {
            self.onset_pending = false;
        }

        if should_capture {
            cmd.capture = Some(self.trigger(transport));
        }
        // Slots move on after the capture so a new freeze hasn't faded in before it starts.
        for s in self.slots.iter_mut() {
            s.advance();
        }
        if self.delay_dirty
            || (self.index(TIME_MODE) != TIME_MODE_MS && transport.tempo != self.last_tempo)
        {
            self.update_delays(transport);
            self.last_tempo = transport.tempo;
        }
        cmd
    }

    /// The time-domain onset detector.
    fn detect_onsets(&mut self, left: &[f32], right: &[f32]) {
        let sensitivity = (self.value(SENSITIVITY) * 0.01).clamp(0.0, 1.0);
        // Above this many times the slow level counts as an onset; the highest sensitivity
        // listens for the smallest jumps.
        let ratio_needed = 1.0 + 7.0 * (1.0 - sensitivity).powi(2);
        let floor = 0.002 * (1.0 + 20.0 * (1.0 - sensitivity));
        for (l, r) in left.iter().zip(right) {
            let x = 0.5 * (l + r);
            let fast = self.fast.process(x);
            let slow = self.slow.process(x);
            self.holdoff = self.holdoff.saturating_sub(1);
            if self.holdoff == 0 && fast > floor && fast > slow * ratio_needed {
                self.onset_pending = true;
                self.holdoff = (self.sr * 0.06) as u32;
            }
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32], transport: &TransportInfo) {
        let n = left.len();
        let size = self.size();
        let hop = self.hop();
        let bins = bin_count(size);
        let gain = db_to_gain(self.value(INPUT_SEND));
        let zero_dry = self.flag(ZERO_DRY);
        let mix = (self.value(DRY_WET) * 0.01).clamp(0.0, 1.0);
        let freezer_on = self.flag(FREEZER_ON);
        let delay_on = self.flag(DELAY_ON);
        let delay_mix = (self.value(DELAY_MIX) * 0.01).clamp(0.0, 1.0);
        let feedback = (self.value(FEEDBACK) * 0.01).clamp(0.0, 0.995);
        let shift_bins = self.value(SHIFT) / (self.sr / size as f32);
        let order = self.index(ORDER);
        let mut pos = 0;
        while pos < n {
            let chunk = (hop - self.since_hop).min(n - pos).min(512);
            self.detect_onsets(&left[pos..pos + chunk], &right[pos..pos + chunk]);
            let frame_due = self.since_hop + chunk == hop;
            let command = if frame_due {
                self.frame_command(transport, hop)
            } else {
                FrameCommand::default()
            };
            let slots = self.slots;
            let res = self.res;
            let (stft_pair, channels) = (&mut self.stft[res], &mut self.channels);
            let shift_phase = &mut self.shift_phase;
            let mut input = [[0.0f32; 512]; 2];
            let mut wet = [[0.0f32; 512]; 2];
            // A chunk stops at the next frame and fits these arrays, so a frame always falls at
            // its end and both channels see the same command.
            for (i, (l, r)) in left[pos..pos + chunk]
                .iter()
                .zip(&right[pos..pos + chunk])
                .enumerate()
            {
                input[0][i] = l * gain;
                input[1][i] = r * gain;
            }
            for (c, (s, ch)) in stft_pair.iter_mut().zip(channels.iter_mut()).enumerate() {
                let shift = &mut shift_phase[c];
                s.process(&input[c][..chunk], &mut wet[c][..chunk], |spec| {
                    process_frame(
                        ch, spec, bins, &slots, command, freezer_on, delay_on, delay_mix, feedback,
                        shift_bins, hop, size, order, shift,
                    );
                });
            }
            // Dry path, in line with the wet one unless Zero Dry Signal Latency is on.
            let dry_len = self.dry_ring[0].len();
            for i in 0..chunk {
                let (l, r) = (left[pos + i], right[pos + i]);
                let w = self.dry_write;
                let read = (w + dry_len - size) % dry_len;
                self.dry_ring[0][w] = l;
                self.dry_ring[1][w] = r;
                let (dry_l, dry_r) = if zero_dry {
                    (l, r)
                } else {
                    (self.dry_ring[0][read], self.dry_ring[1][read])
                };
                self.dry_write = (w + 1) % dry_len;
                left[pos + i] = dry_l * (1.0 - mix) + sanitize(wet[0][i]) * mix;
                right[pos + i] = dry_r * (1.0 - mix) + sanitize(wet[1][i]) * mix;
            }
            self.since_hop = (self.since_hop + chunk) % hop;
            pos += chunk;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_frame(
    ch: &mut Channel,
    spec: &mut [Complex32],
    bins: usize,
    slots: &[Slot; MAX_FREEZES],
    command: FrameCommand,
    freezer_on: bool,
    delay_on: bool,
    delay_mix: f32,
    feedback: f32,
    shift_bins: f32,
    hop: usize,
    size: usize,
    order: usize,
    shift_phase: &mut f32,
) {
    let mut delay = |ch: &mut Channel, spec: &mut [Complex32]| {
        if delay_on {
            run_delay(
                ch,
                spec,
                bins,
                delay_mix,
                feedback,
                shift_bins,
                hop,
                size,
                shift_phase,
            );
        }
    };
    if order == ORDER_DELAY_FREEZE {
        delay(ch, spec);
    }
    if freezer_on {
        run_freezer(ch, spec, bins, slots, command);
    }
    if order == ORDER_FREEZE_DELAY {
        delay(ch, spec);
    }
}

#[allow(clippy::needless_range_loop)] // Several parallel per-bin arrays.
fn run_freezer(
    ch: &mut Channel,
    spec: &mut [Complex32],
    bins: usize,
    slots: &[Slot; MAX_FREEZES],
    command: FrameCommand,
) {
    if let Some(index) = command.capture {
        let freeze = &mut ch.freezes[index];
        for k in 0..bins {
            let (mag, phase) = spec[k].to_polar();
            freeze.mag[k] = mag;
            freeze.phase[k] = phase;
            freeze.delta[k] = princ_arg(phase - ch.prev_phase[k]);
        }
    }
    let total: f32 = slots.iter().map(|s| s.env).sum();
    let dry = (1.0 - total).clamp(0.0, 1.0);
    if total > 0.0 {
        ch.held[..bins].fill(Complex32::new(0.0, 0.0));
        for (slot, freeze) in slots.iter().zip(ch.freezes.iter_mut()) {
            if slot.stage == Stage::Off && slot.env <= 0.0 {
                continue;
            }
            let env = slot.env;
            for k in 0..bins {
                let phase = freeze.phase[k] + freeze.delta[k];
                freeze.phase[k] = princ_arg(phase);
                let (s, c) = phase.sin_cos();
                let m = freeze.mag[k] * env;
                ch.held[k].re += m * c;
                ch.held[k].im += m * s;
            }
        }
        for k in 0..bins {
            let phase = spec[k].arg();
            ch.prev_phase[k] = phase;
            spec[k] = spec[k] * dry + ch.held[k];
        }
    } else {
        for k in 0..bins {
            ch.prev_phase[k] = spec[k].arg();
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
fn run_delay(
    ch: &mut Channel,
    spec: &mut [Complex32],
    bins: usize,
    delay_mix: f32,
    feedback: f32,
    shift_bins: f32,
    hop: usize,
    size: usize,
    shift_phase: &mut f32,
) {
    let frames = ch.delay.len() / bins;
    for k in 0..bins {
        let at = (ch.write + frames - ch.delay_frames[k]) % frames;
        ch.delayed[k] = ch.delay[at * bins + k];
    }
    if shift_bins.abs() > 1e-6 {
        // Move by whole bins, and advance the phase by the full shift from frame to frame, which
        // is what turns the move into a true frequency shift of the overlap-added output.
        let whole = shift_bins.round() as isize;
        *shift_phase = princ_arg(*shift_phase + TAU * shift_bins * hop as f32 / size as f32);
        let (s, c) = (*shift_phase).sin_cos();
        let turn = Complex32::new(c, s);
        let read = |delayed: &[Complex32], source: isize| {
            if source >= 0 && (source as usize) < bins {
                delayed[source as usize]
            } else {
                Complex32::new(0.0, 0.0)
            }
        };
        if whole >= 0 {
            for k in (0..bins).rev() {
                ch.delayed[k] = read(&ch.delayed, k as isize - whole);
            }
        } else {
            for k in 0..bins {
                ch.delayed[k] = read(&ch.delayed, k as isize - whole);
            }
        }
        for value in ch.delayed[..bins].iter_mut() {
            *value *= turn;
        }
    }
    let write = ch.write * bins;
    for k in 0..bins {
        let delayed = ch.delayed[k];
        ch.delay[write + k] = spec[k] + delayed * feedback;
        spec[k] = spec[k] * (1.0 - delay_mix) + delayed * delay_mix;
    }
    ch.write = (ch.write + 1) % frames;
}

impl Processor for SpectralTime {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate.max(1.0) as f32;
        self.stft = RESOLUTIONS
            .iter()
            .map(|&size| [Stft::new(size, size / 4), Stft::new(size, size / 4)])
            .collect();
        let largest = RESOLUTIONS[RESOLUTIONS.len() - 1];
        let bins = bin_count(largest);
        // bins x frames is the same for every Resolution, so size the line for the shortest hop.
        let hop = RESOLUTIONS[0] / 4;
        let frames = ((MAX_DELAY_SECONDS * self.sr / hop as f32).ceil() as usize + 2).max(4);
        let line = bin_count(RESOLUTIONS[0]) * frames;
        let mut rng = Rng::new(0x7e57);
        self.channels = (0..2)
            .map(|_| Channel {
                freezes: (0..MAX_FREEZES)
                    .map(|_| Freeze {
                        mag: vec![0.0; bins],
                        phase: vec![0.0; bins],
                        delta: vec![0.0; bins],
                    })
                    .collect(),
                prev_phase: vec![0.0; bins],
                delay: vec![Complex32::new(0.0, 0.0); line],
                delay_frames: vec![1; bins],
                spray: (0..bins).map(|_| rng.unipolar()).collect(),
                delayed: vec![Complex32::new(0.0, 0.0); bins],
                write: 0,
                held: vec![Complex32::new(0.0, 0.0); bins],
            })
            .collect();
        self.dry_ring = [vec![0.0; largest + 16], vec![0.0; largest + 16]];
        self.fast.set_mode(DetectMode::Peak);
        self.slow.set_mode(DetectMode::Peak);
        self.fast.prepare(sample_rate);
        self.fast.set_times(0.0005, 0.02);
        self.slow.prepare(sample_rate);
        self.slow.set_times(0.08, 0.3);
        self.spectrogram = Some(Spectrogram::new(sample_rate, SPECTROGRAM_VIEW));
        self.res = self.index(RESOLUTION).min(RESOLUTIONS.len() - 1);
        self.clear_state();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let view = ctx.view;
        let transport = ctx.transport;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set(id, value),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(AudioBuffer { left, right }) => {
                if self.channels.is_empty() {
                    return;
                }
                if let Some(s) = self.spectrogram.as_mut() {
                    if view.is_active() {
                        s.dry(left, right, view);
                    }
                }
                self.render(left, right, transport);
                if let Some(s) = self.spectrogram.as_mut() {
                    if view.is_active() {
                        s.wet(left, right, view);
                    }
                }
            }
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.set(id, value);
    }

    fn reset(&mut self) {
        self.clear_state();
        for line in self.dry_ring.iter_mut() {
            line.fill(0.0);
        }
        self.dry_write = 0;
        self.freeze_armed = false;
        self.fast.reset();
        self.slow.reset();
        self.onset_pending = false;
        self.holdoff = 0;
        if let Some(s) = self.spectrogram.as_mut() {
            s.reset();
        }
    }

    fn latency(&self) -> u32 {
        if self.flag(ZERO_DRY) {
            0
        } else {
            self.size() as u32
        }
    }

    fn tail(&self) -> Tail {
        if self.flag(FREEZER_ON) && self.flag(FREEZE) {
            return Tail::Infinite;
        }
        if self.flag(DELAY_ON) && self.value(FEEDBACK) > 0.0 {
            return Tail::Frames((self.sr * MAX_DELAY_SECONDS * 4.0) as u32);
        }
        Tail::Frames(self.size() as u32 * 2)
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.spectral_time",
    name: "Spectral Time",
    role: DeviceRole::AudioEffect,
    category: "Spectral",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(SpectralTime::default()),
};
