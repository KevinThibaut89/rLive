//! Spectrum: real-time frequency analysis that leaves the audio untouched (manual pages 677-678).
//!
//! The manual describes Block, Channel, Refresh, Avg, Graph, Max, Scale X and the Range/Auto
//! display range. It gives no numbers, so the block sizes, refresh range and range limits are
//! ours. Levels are in dB with a full-scale sine at 0 dB. The analysis runs only while a UI is
//! reading; with nothing reading it costs a parameter check.

use super::common::{knob, menu, toggle};
use crate::devices::b1::common::int_param;
use crate::dsp::fft::{Complex32, Fft};
use crate::dsp::util::sanitize;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamFlags, ParamInfo, ProcessContext, Processor,
    Section, Status, TransportInfo, ViewId,
};

pub const BLOCK: u32 = 0;
pub const CHANNEL: u32 = 1;
pub const REFRESH: u32 = 2;
pub const AVG: u32 = 3;
pub const GRAPH: u32 = 4;
pub const MAX: u32 = 5;
pub const SCALE_X: u32 = 6;
pub const AUTO: u32 = 7;
pub const RANGE_MIN: u32 = 8;
pub const RANGE_MAX: u32 = 9;
/// Hidden. Any change clears the accumulated maximum (clicking the display does this).
pub const RESET_MAX: u32 = 10;
const PARAMS: usize = 11;

pub const BLOCKS: [usize; 8] = [256, 512, 1024, 2048, 4096, 8192, 16_384, 32_768];
const BLOCK_LABELS: [&str; 8] = [
    "256", "512", "1024", "2048", "4096", "8192", "16384", "32768",
];
const RING: usize = 32_768;
pub const CHANNEL_L: usize = 0;
pub const CHANNEL_R: usize = 1;
pub const CHANNEL_BOTH: usize = 2;
pub const X_LINEAR: usize = 0;
pub const X_LOG: usize = 1;
pub const X_SEMITONE: usize = 2;
pub const GRAPH_LINE: usize = 0;
pub const GRAPH_BINS: usize = 1;
pub const MAX_AVG: usize = 128;

/// The display's horizontal resolution.
pub const POINTS: usize = 512;
/// The lowest frequency a log axis shows.
pub const LOG_MIN_HZ: f32 = 20.0;
/// The level a point shows when there's nothing there, in dB.
pub const FLOOR_DB: f32 = -120.0;

/// `[sample rate, block, auto min dB, auto max dB]`, each block while the display is open.
pub const META: ViewId = ViewId(0);
pub const META_LEN: usize = 4;
/// [`POINTS`] values in dB, one per display column.
pub const CURVE: ViewId = ViewId(META_LEN as u32);
/// The accumulated maximum, also [`POINTS`] dB values.
pub const PEAKS: ViewId = ViewId((META_LEN + POINTS) as u32);
pub const VIEW_LEN: usize = META_LEN + POINTS + POINTS;

/// The centre frequency of display column `i`, for the given scale.
pub fn point_hz(i: usize, scale_x: usize, sample_rate: f32) -> f32 {
    let t = i as f32 / (POINTS - 1) as f32;
    let nyquist = sample_rate * 0.5;
    if scale_x == X_LINEAR {
        t * nyquist
    } else {
        LOG_MIN_HZ * (nyquist / LOG_MIN_HZ).powf(t)
    }
}

/// The display column nearest a frequency.
pub fn hz_point(hz: f32, scale_x: usize, sample_rate: f32) -> usize {
    let nyquist = sample_rate * 0.5;
    let t = if scale_x == X_LINEAR {
        hz / nyquist
    } else {
        (hz.max(LOG_MIN_HZ) / LOG_MIN_HZ).ln() / (nyquist / LOG_MIN_HZ).ln()
    };
    ((t * (POINTS - 1) as f32).round() as isize).clamp(0, POINTS as isize - 1) as usize
}

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(BLOCK, "Block", &BLOCK_LABELS, 4),
        ParamInfo::stepped(CHANNEL, "Channel", &["L", "R", "L+R"], CHANNEL_BOTH),
        ParamInfo::skewed(REFRESH, "Refresh", 10.0, 1_000.0, 60.0, "ms", 3.0),
        int_param(AVG, "Avg", 1.0, MAX_AVG as f64, 1.0),
        ParamInfo::stepped(GRAPH, "Graph", &["Line", "Bins"], GRAPH_LINE),
        ParamInfo::toggle(MAX, "Max", true),
        ParamInfo::stepped(SCALE_X, "Scale X", &["Lin", "Log", "ST"], X_LOG),
        ParamInfo::toggle(AUTO, "Auto", false),
        ParamInfo::continuous(RANGE_MIN, "Range Min", -120.0, 0.0, -96.0, "dB"),
        ParamInfo::continuous(RANGE_MAX, "Range Max", -96.0, 24.0, 6.0, "dB"),
        ParamInfo::continuous(RESET_MAX, "Reset Max", 0.0, 1e9, 0.0, "")
            .with_flags(ParamFlags::HIDDEN),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Spectrum",
            items: vec![Item::Panel("spectrum_display")],
        },
        Section {
            title: "Analysis",
            items: vec![
                menu(BLOCK),
                menu(CHANNEL),
                knob(REFRESH),
                knob(AVG),
                menu(GRAPH),
                toggle(MAX),
                menu(SCALE_X),
                toggle(AUTO),
                knob(RANGE_MIN),
                knob(RANGE_MAX),
            ],
        },
    ]
}

/// The bins each block size uses.
struct Plan {
    fft: Fft,
    window: Vec<f32>,
    /// Turns a magnitude into the amplitude of a sine.
    scale: f32,
}

impl Plan {
    fn new(block: usize) -> Self {
        let window: Vec<f32> = (0..block)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / block as f32).cos())
            .collect();
        let sum: f32 = window.iter().sum();
        Self {
            fft: Fft::new(block),
            window,
            scale: 2.0 / sum,
        }
    }
}

pub struct Spectrum {
    v: [f64; PARAMS],
    sr: f32,
    plans: Vec<Plan>,
    ring: Vec<f32>,
    write: usize,
    since: usize,
    frame: Vec<f32>,
    spectrum: Vec<Complex32>,
    /// The last [`MAX_AVG`] measurements, in power, [`POINTS`] values each.
    history: Vec<f32>,
    /// Which measurement in `history` comes next.
    slot: usize,
    filled: usize,
    power: Vec<f32>,
    curve: Vec<f32>,
    peaks: Vec<f32>,
    /// Fractional FFT bin at the centre of each column's edges, for the current settings.
    edges: Vec<f32>,
    dirty: bool,
    was_playing: bool,
}

impl Default for Spectrum {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            plans: Vec::new(),
            ring: vec![0.0; RING],
            write: 0,
            since: 0,
            frame: vec![0.0; RING],
            spectrum: vec![Complex32::new(0.0, 0.0); RING / 2 + 1],
            history: vec![0.0; MAX_AVG * POINTS],
            slot: 0,
            filled: 0,
            power: vec![0.0; POINTS],
            curve: vec![FLOOR_DB; POINTS],
            peaks: vec![FLOOR_DB; POINTS],
            edges: vec![0.0; POINTS + 1],
            dirty: true,
            was_playing: false,
        }
    }
}

fn to_db(power: f32) -> f32 {
    (10.0 * power.max(1e-14).log10()).max(FLOOR_DB)
}

impl Spectrum {
    fn index(&self, id: u32) -> usize {
        self.v[id as usize].round().max(0.0) as usize
    }

    fn block_index(&self) -> usize {
        self.index(BLOCK).min(BLOCKS.len() - 1)
    }

    fn refresh_frames(&self) -> usize {
        ((self.v[REFRESH as usize] * 0.001 * self.sr as f64) as usize).max(16)
    }

    fn clear_measurements(&mut self) {
        self.history.fill(0.0);
        self.slot = 0;
        self.filled = 0;
        self.power.fill(0.0);
        self.curve.fill(FLOOR_DB);
        self.peaks.fill(FLOOR_DB);
    }

    fn rebuild_edges(&mut self) {
        let block = BLOCKS[self.block_index()] as f32;
        let bin_hz = self.sr / block;
        let scale_x = self.index(SCALE_X);
        let at = |t: f32| point_hz_t(t, scale_x, self.sr) / bin_hz;
        for (i, edge) in self.edges.iter_mut().enumerate() {
            *edge = at((i as f32 - 0.5) / (POINTS - 1) as f32);
        }
        self.dirty = false;
    }

    fn set(&mut self, id: u32, value: f64) {
        if let Some(slot) = self.v.get_mut(id as usize) {
            let changed = *slot != value;
            *slot = value;
            match id {
                RESET_MAX if changed => self.peaks.fill(FLOOR_DB),
                BLOCK | SCALE_X if changed => {
                    self.dirty = true;
                    self.clear_measurements();
                }
                AVG if changed => {
                    self.history.fill(0.0);
                    self.slot = 0;
                    self.filled = 0;
                }
                _ => {}
            }
        }
    }

    fn song_restart(&mut self, transport: &TransportInfo) {
        if transport.playing && !self.was_playing {
            self.peaks.fill(FLOOR_DB);
        }
        self.was_playing = transport.playing;
    }

    fn measure(&mut self) {
        if self.plans.is_empty() {
            return;
        }
        if self.dirty {
            self.rebuild_edges();
        }
        let index = self.block_index();
        let block = BLOCKS[index];
        let bins = block / 2 + 1;
        let start = (self.write + RING - block) % RING;
        let plan = &mut self.plans[index];
        for (i, s) in self.frame[..block].iter_mut().enumerate() {
            *s = self.ring[(start + i) % RING] * plan.window[i];
        }
        plan.fft
            .forward(&mut self.frame[..block], &mut self.spectrum[..bins]);
        let scale = plan.scale * plan.scale;
        let last = bins - 1;

        let avg = (self.v[AVG as usize].round() as usize).clamp(1, MAX_AVG);
        let slot = &mut self.history[self.slot * POINTS..(self.slot + 1) * POINTS];
        for (i, value) in slot.iter_mut().enumerate() {
            let (lo, hi) = (self.edges[i].max(0.0), self.edges[i + 1]);
            let (first, end) = (lo.ceil() as usize, hi.floor() as usize);
            let power = if end >= first {
                let (first, end) = (first.min(last), end.min(last));
                self.spectrum[first..=end]
                    .iter()
                    .fold(0.0f32, |m, c| m.max(c.norm_sqr()))
            } else {
                let at = (0.5 * (lo + hi)).min(last as f32 - 1.0).max(0.0);
                let bin = at.floor() as usize;
                let t = at - bin as f32;
                let a = self.spectrum[bin].norm();
                let b = self.spectrum[(bin + 1).min(last)].norm();
                let m = a + (b - a) * t;
                m * m
            };
            *value = power * scale;
        }
        self.slot = (self.slot + 1) % avg;
        self.filled = (self.filled + 1).min(avg);

        self.power.fill(0.0);
        for measurement in 0..self.filled {
            let row = &self.history[measurement * POINTS..(measurement + 1) * POINTS];
            for (sum, value) in self.power.iter_mut().zip(row) {
                *sum += value;
            }
        }
        let keep_max = self.v[MAX as usize] >= 0.5;
        let filled = self.filled.max(1) as f32;
        for i in 0..POINTS {
            let power = self.power[i] / filled;
            let db = to_db(power);
            self.curve[i] = db;
            if keep_max && db > self.peaks[i] {
                self.peaks[i] = db;
            }
        }
    }
}

fn point_hz_t(t: f32, scale_x: usize, sample_rate: f32) -> f32 {
    let nyquist = sample_rate * 0.5;
    if scale_x == X_LINEAR {
        t * nyquist
    } else {
        LOG_MIN_HZ * (nyquist / LOG_MIN_HZ).powf(t)
    }
}

impl Processor for Spectrum {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate.max(1.0) as f32;
        self.plans = BLOCKS.iter().map(|&b| Plan::new(b)).collect();
        self.dirty = true;
        self.write = 0;
        self.since = 0;
        self.ring.fill(0.0);
        self.clear_measurements();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let view = ctx.view;
        let active = view.is_active();
        let transport = ctx.transport;
        let channel = self.index(CHANNEL).min(CHANNEL_BOTH);
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set(id, value),
            BlockStep::Audio(AudioBuffer { left, right }) => {
                if !active {
                    return;
                }
                self.song_restart(transport);
                let refresh = self.refresh_frames();
                for (l, r) in left.iter().zip(right.iter()) {
                    let x = match channel {
                        CHANNEL_L => *l,
                        CHANNEL_R => *r,
                        _ => 0.5 * (l + r),
                    };
                    self.ring[self.write] = sanitize(x);
                    self.write = (self.write + 1) % RING;
                    self.since += 1;
                    if self.since >= refresh {
                        self.since = 0;
                        self.measure();
                    }
                }
            }
            BlockStep::Midi(_) => {}
        });
        if active {
            let top = self.curve.iter().copied().fold(FLOOR_DB, f32::max);
            let auto_max = (top / 6.0).ceil() * 6.0 + 6.0;
            let meta = [
                self.sr,
                BLOCKS[self.block_index()] as f32,
                (auto_max - 72.0).max(FLOOR_DB),
                auto_max,
            ];
            view.publish(META, &meta);
            view.publish(CURVE, &self.curve);
            view.publish(PEAKS, &self.peaks);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.set(id, value);
    }

    fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.since = 0;
        self.clear_measurements();
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.spectrum",
    name: "Spectrum",
    role: DeviceRole::AudioEffect,
    category: "Spectral",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Spectrum::default()),
};
