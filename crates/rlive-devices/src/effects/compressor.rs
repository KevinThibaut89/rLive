use super::defaults;
use crate::devices::b3::common::{
    auto_release_ms, detector_mode, knee_db, ms_to_frames, sidechain_items, sidechain_params,
    sidechain_settings, sidechain_slice, time_coeff, MAX_UPWARD_DB, SC_EQ_ON, SC_LISTEN, SC_ON,
};
use crate::dsp::lookahead::LookaheadBuffer;
use crate::dsp::sidechain::SidechainDetector;
use rlive_core::{
    db_to_gain, gain_to_db, AudioBuffer, AudioInput, BlockStep, Item, ModMode, ParamId, ParamInfo,
    ProcessContext, Processor, Section, ViewId, METER_VALUES,
};

// ---------------------------------------------------------------- Compressor

pub const THRESHOLD: u32 = 0;
pub const RATIO: u32 = 1;
pub const ATTACK: u32 = 2;
pub const RELEASE: u32 = 3;
pub const KNEE: u32 = 4;
/// The continuous output gain, which Live calls Output. Its released name stays "Makeup".
pub const MAKEUP: u32 = 5;
pub const MIX: u32 = 6;
pub const MODE: u32 = 7;
pub const ENVELOPE: u32 = 8;
pub const LOOKAHEAD: u32 = 9;
pub const AUTO_RELEASE: u32 = 10;
pub const AUTO_MAKEUP: u32 = 11;
/// The first of the sidechain parameters; see [`crate::devices::b3::common`].
pub const SIDECHAIN: u32 = 12;

/// The top of the Ratio control, which means infinity: the output doesn't rise at all.
pub const RATIO_INF: f32 = 100.0;

/// [`MODE`] values.
pub const MODE_PEAK: f64 = 0.0;
pub const MODE_RMS: f64 = 1.0;
pub const MODE_EXPAND: f64 = 2.0;

/// [`ENVELOPE`] values.
pub const ENV_LIN: f64 = 0.0;
pub const ENV_LOG: f64 = 1.0;

/// [`LOOKAHEAD`]'s choices, in ms.
pub const LOOKAHEAD_MS: [f32; 3] = [0.0, 1.0, 10.0];

/// The current gain reduction, in dB (never negative), read by the `gain_reduction` panel.
pub const GAIN_REDUCTION: ViewId = ViewId(0);
/// The output level, left and right, as a [`METER_VALUES`] meter.
pub const OUTPUT: ViewId = ViewId(1);
/// The input level, left and right, read by the `compressor_display` panel.
pub const INPUT: ViewId = ViewId(3);
/// How many view values the Compressor publishes.
pub const VIEW_LEN: usize = 1 + METER_VALUES + METER_VALUES;

pub fn compressor_params() -> Vec<ParamInfo> {
    let mut params = vec![
        ParamInfo::continuous(THRESHOLD, "Threshold", -70.0, 6.0, -18.0, "dB"),
        ParamInfo::skewed(RATIO, "Ratio", 1.0, RATIO_INF as f64, 4.0, ":1", 2.0),
        ParamInfo::skewed(ATTACK, "Attack", 0.01, 1_000.0, 10.0, "ms", 3.0),
        ParamInfo::skewed(RELEASE, "Release", 1.0, 3_000.0, 120.0, "ms", 3.0),
        ParamInfo::continuous(KNEE, "Knee", 0.0, 18.0, 6.0, "dB"),
        ParamInfo::continuous(MAKEUP, "Makeup", -36.0, 36.0, 0.0, "dB")
            .with_mod_mode(ModMode::Scale),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
        ParamInfo::stepped(MODE, "Mode", &["Peak", "RMS", "Expand"], 0),
        ParamInfo::stepped(ENVELOPE, "Envelope", &["Lin", "Log"], 0),
        ParamInfo::stepped(LOOKAHEAD, "Lookahead", &["0 ms", "1 ms", "10 ms"], 0),
        ParamInfo::toggle(AUTO_RELEASE, "Auto Release", false),
        ParamInfo::toggle(AUTO_MAKEUP, "Auto Makeup", false),
    ];
    params.extend(sidechain_params(SIDECHAIN, true));
    params
}

/// The Compressor's panel: how it responds on the left, what it does to the signal on the right.
pub fn compressor_layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let mut output = vec![
        knob(MAKEUP),
        Item::Toggle(ParamId(AUTO_MAKEUP)),
        knob(MIX),
        Item::Row(vec![Item::Panel("gain_reduction"), Item::Meter(OUTPUT)]),
    ];
    output.extend(sidechain_items(SIDECHAIN, true));
    vec![
        Section {
            title: "Dynamics",
            items: vec![
                Item::Panel("compressor_display"),
                knob(THRESHOLD),
                knob(RATIO),
                knob(ATTACK),
                knob(RELEASE),
                knob(KNEE),
                Item::Row(vec![
                    Item::Menu(ParamId(MODE)),
                    Item::Menu(ParamId(ENVELOPE)),
                ]),
                Item::Row(vec![
                    Item::Menu(ParamId(LOOKAHEAD)),
                    Item::Toggle(ParamId(AUTO_RELEASE)),
                ]),
            ],
        },
        Section {
            title: "Output",
            items: output,
        },
    ]
}

/// The compressor's static curve: the gain, in dB, it applies to a steady `level_db`. Negative is
/// gain reduction. Upward gain is capped at [`MAX_UPWARD_DB`] so output can't run away.
pub fn curve_gain_db(level_db: f32, threshold: f32, ratio: f32, knee: f32, expand: bool) -> f32 {
    let over = level_db - threshold;
    if expand {
        knee_db(over, knee, ratio.max(1.0) - 1.0).min(MAX_UPWARD_DB)
    } else {
        -knee_db(over, knee, compress_slope(ratio))
    }
}

/// How many dB of reduction each dB over the threshold earns.
#[inline]
fn compress_slope(ratio: f32) -> f32 {
    if ratio >= RATIO_INF {
        1.0
    } else {
        1.0 - 1.0 / ratio.max(1.0)
    }
}

pub struct Compressor {
    v: Vec<f64>,
    sr: f32,
    env_db: f32,
    detector: SidechainDetector,
    look: LookaheadBuffer<2>,
    /// A slow average of the gain reduction, for Auto Release.
    slow_db: f32,
    max_block: usize,
    sc_dirty: bool,
}

/// What [`Default`] and [`Processor::activate`] allocate for, until told otherwise.
const DEFAULT_MAX_BLOCK: usize = 4096;
/// The longest lookahead the device offers, in ms.
const MAX_LOOKAHEAD_MS: f32 = 10.0;

impl Default for Compressor {
    fn default() -> Self {
        let mut detector = SidechainDetector::new();
        detector.prepare(48_000.0, DEFAULT_MAX_BLOCK);
        Self {
            v: defaults(&compressor_params()),
            sr: 48_000.0,
            env_db: 0.0,
            detector,
            look: LookaheadBuffer::new(ms_to_frames(MAX_LOOKAHEAD_MS, 48_000.0)),
            slow_db: 0.0,
            max_block: DEFAULT_MAX_BLOCK,
            sc_dirty: true,
        }
    }
}

impl Compressor {
    fn gain_reduction(&self, level_db: f32) -> f32 {
        let t = self.v[THRESHOLD as usize] as f32;
        let r = self.v[RATIO as usize] as f32;
        let k = self.v[KNEE as usize] as f32;
        knee_db(level_db - t, k, compress_slope(r))
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn step(&self, id: u32) -> f64 {
        self.v[id as usize].round()
    }

    fn sc_active(&self) -> bool {
        self.flag(SIDECHAIN + SC_ON)
    }

    /// True while every new feature is off, so the sound is the original Compressor's.
    fn is_plain(&self) -> bool {
        self.step(MODE) == MODE_PEAK
            && self.step(ENVELOPE) == ENV_LIN
            && self.step(LOOKAHEAD) == 0.0
            && !self.flag(AUTO_RELEASE)
            && !self.flag(AUTO_MAKEUP)
            && !self.sc_active()
            && !self.flag(SIDECHAIN + SC_LISTEN)
            && !self.flag(SIDECHAIN + SC_EQ_ON)
    }

    /// Keeps today's arithmetic for the plain path, sample for sample, so projects made before
    /// Live's modes were added sound the same and K4's golden render hashes stay valid. Don't
    /// route it through the shared detector, whose follower runs on linear level.
    fn render(&mut self, audio: AudioBuffer) {
        let att = (-1.0 / (self.v[ATTACK as usize] as f32 * 0.001 * self.sr)).exp();
        let rel = (-1.0 / (self.v[RELEASE as usize] as f32 * 0.001 * self.sr)).exp();
        let makeup = self.v[MAKEUP as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for i in 0..left.len() {
            let (l, r) = (left[i], right[i]);
            let level = gain_to_db(l.abs().max(r.abs()));
            let gr = self.gain_reduction(level);
            let coeff = if gr > self.env_db { att } else { rel };
            self.env_db = gr + (self.env_db - gr) * coeff;
            let g = db_to_gain(makeup - self.env_db);
            left[i] = l * (1.0 - mix) + l * g * mix;
            right[i] = r * (1.0 - mix) + r * g * mix;
        }
    }

    fn sync_detector(&mut self) {
        if !self.sc_dirty {
            return;
        }
        self.sc_dirty = false;
        let (detect, attack, release) = detector_mode(self.step(MODE) == MODE_RMS);
        let settings = sidechain_settings(&self.v, SIDECHAIN, true, detect, attack, release);
        self.detector.set(&settings);
    }

    /// The modes the original Compressor didn't have, built from the shared DSP blocks.
    fn render_modes(&mut self, audio: AudioBuffer, external: Option<AudioInput>) {
        self.sync_detector();
        let external = external.filter(|_| self.sc_active());
        let expand = self.step(MODE) == MODE_EXPAND;
        let log = self.step(ENVELOPE) == ENV_LOG;
        let auto_release = self.flag(AUTO_RELEASE);
        let auto_makeup = self.flag(AUTO_MAKEUP) && external.is_none();
        let (t, r, k) = (
            self.v[THRESHOLD as usize] as f32,
            self.v[RATIO as usize] as f32,
            self.v[KNEE as usize] as f32,
        );
        let attack = time_coeff(self.v[ATTACK as usize] as f32, self.sr);
        let release_ms = self.v[RELEASE as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        // The gain a 0 dBFS signal would lose, restored whatever the threshold and ratio.
        let makeup = self.v[MAKEUP as usize] as f32
            - if auto_makeup {
                curve_gain_db(0.0, t, r, k, expand)
            } else {
                0.0
            };
        let slow_coeff = time_coeff(400.0, self.sr);
        let sr = self.sr;

        let frames = audio.frames();
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(self.max_block);
            let ext = sidechain_slice(external, pos, n);
            let (left, right) = (
                &mut audio.left[pos..pos + n],
                &mut audio.right[pos..pos + n],
            );
            let key = self.detector.process(left, right, ext);
            let level = key.level;
            for i in 0..n {
                let target = -curve_gain_db(gain_to_db(level[i]), t, r, k, expand);
                let coeff = if target.abs() > self.env_db.abs() {
                    attack
                } else {
                    let mut ms = if auto_release {
                        auto_release_ms(self.slow_db, self.env_db, 30.0, 500.0)
                    } else {
                        release_ms
                    };
                    if log {
                        // The deeper the reduction, the faster it recovers.
                        ms /= 1.0 + self.env_db.abs() / 6.0;
                    }
                    time_coeff(ms, sr)
                };
                self.env_db = target + (self.env_db - target) * coeff;
                self.slow_db = self.env_db + (self.slow_db - self.env_db) * slow_coeff;
                let [dl, dr] = self.look.process([left[i], right[i]]);
                let g = db_to_gain(makeup - self.env_db);
                left[i] = dl * (1.0 - mix) + dl * g * mix;
                right[i] = dr * (1.0 - mix) + dr * g * mix;
            }
            self.detector.listen(&mut AudioBuffer { left, right });
            pos += n;
        }
    }
}

impl Processor for Compressor {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.detector.prepare(sample_rate, self.max_block);
        self.look = LookaheadBuffer::new(ms_to_frames(MAX_LOOKAHEAD_MS, self.sr));
        self.sync_lookahead();
        self.sc_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let sidechain = ctx.sidechain;
        let mut pos = 0;
        // The input level, for the activity display: read before the audio is replaced.
        let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let input = if ctx.view.is_active() {
            (peak(ctx.audio.left), peak(ctx.audio.right))
        } else {
            (0.0, 0.0)
        };
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => {
                let frames = audio.frames();
                if self.is_plain() {
                    self.render(audio);
                } else {
                    self.render_modes(audio, sidechain_slice(sidechain, pos, frames));
                }
                pos += frames;
            }
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            let (left, right) = (peak(ctx.audio.left), peak(ctx.audio.right));
            ctx.view.publish(
                GAIN_REDUCTION,
                &[self.env_db.max(0.0), left, right, input.0, input.1],
            );
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == LOOKAHEAD {
            self.sync_lookahead();
        }
        // Mode picks the detector's RMS or Peak, so it counts as a sidechain setting.
        if id == MODE || id >= SIDECHAIN {
            self.sc_dirty = true;
        }
    }

    fn reset(&mut self) {
        self.env_db = 0.0;
        self.slow_db = 0.0;
        self.detector.reset();
        self.look.reset();
    }

    fn latency(&self) -> u32 {
        self.look.latency()
    }
}

impl Compressor {
    fn sync_lookahead(&mut self) {
        let i = (self.v[LOOKAHEAD as usize].round() as usize).min(LOOKAHEAD_MS.len() - 1);
        self.look.set_len(ms_to_frames(LOOKAHEAD_MS[i], self.sr));
    }
}
