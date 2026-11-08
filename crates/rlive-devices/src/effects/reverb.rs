use super::defaults;
use crate::devices::b7::reverb_studio::Studio;
use crate::dsp::DelayLine;
use rlive_core::{AudioBuffer, BlockStep, ParamInfo, ProcessContext, Processor, Tail};

// ---------------------------------------------------------------- Reverb (Freeverb)

pub const SIZE: u32 = 0;
pub const DAMPING: u32 = 1;
pub const WIDTH: u32 = 2;
pub const MIX: u32 = 3;
pub const PREDELAY: u32 = 4;
// Parameters from here on belong to the Studio engine and the choice of engine. Their defaults
// leave the Classic path untouched, so saved projects sound the same.
pub const ENGINE: u32 = 5;
pub const INPUT_LOW_ON: u32 = 6;
pub const INPUT_HIGH_ON: u32 = 7;
pub const INPUT_FREQ: u32 = 8;
pub const INPUT_WIDTH: u32 = 9;
pub const SPIN_ON: u32 = 10;
pub const SPIN_AMOUNT: u32 = 11;
pub const SPIN_RATE: u32 = 12;
pub const ER_SHAPE: u32 = 13;
pub const HIGH_ON: u32 = 14;
pub const HIGH_FREQ: u32 = 15;
pub const HIGH_GAIN: u32 = 16;
pub const LOW_ON: u32 = 17;
pub const LOW_FREQ: u32 = 18;
pub const LOW_GAIN: u32 = 19;
pub const DIFFUSION: u32 = 20;
pub const SCALE: u32 = 21;
pub const CHORUS_ON: u32 = 22;
pub const CHORUS_AMOUNT: u32 = 23;
pub const CHORUS_RATE: u32 = 24;
pub const SMOOTH: u32 = 25;
pub const DECAY: u32 = 26;
pub const FREEZE: u32 = 27;
pub const FLAT: u32 = 28;
pub const CUT: u32 = 29;
pub const STEREO: u32 = 30;
pub const DENSITY: u32 = 31;
pub const REFLECT: u32 = 32;
pub const DIFFUSE: u32 = 33;

/// Engine menu position of the original Freeverb-style reverb.
pub const ENGINE_CLASSIC: usize = 0;
/// Engine menu position of the reverb with every control of the manual.
pub const ENGINE_STUDIO: usize = 1;

pub const SMOOTH_NONE: usize = 0;
pub const SMOOTH_SLOW: usize = 1;
pub const SMOOTH_FAST: usize = 2;

pub fn reverb_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(SIZE, "Size", 0.0, 1.0, 0.7, ""),
        ParamInfo::continuous(DAMPING, "Damping", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 0.25, ""),
        ParamInfo::continuous(PREDELAY, "Pre-delay", 0.0, 200.0, 10.0, "ms"),
        ParamInfo::stepped(ENGINE, "Engine", &["Classic", "Studio"], ENGINE_CLASSIC),
        ParamInfo::toggle(INPUT_LOW_ON, "Input Low Cut", true),
        ParamInfo::toggle(INPUT_HIGH_ON, "Input High Cut", true),
        ParamInfo::skewed(
            INPUT_FREQ,
            "Input Filter Freq",
            50.0,
            18_000.0,
            830.0,
            "Hz",
            3.0,
        ),
        ParamInfo::continuous(INPUT_WIDTH, "Input Filter Width", 0.5, 9.0, 6.0, "oct"),
        ParamInfo::toggle(SPIN_ON, "Spin", true),
        ParamInfo::continuous(SPIN_AMOUNT, "Spin Amount", 0.0, 15.0, 1.5, "ms"),
        ParamInfo::skewed(SPIN_RATE, "Spin Rate", 0.07, 1_300.0, 0.3, "Hz", 4.0),
        ParamInfo::continuous(ER_SHAPE, "Shape", 0.0, 1.0, 0.25, ""),
        ParamInfo::toggle(HIGH_ON, "High Shelf", true),
        ParamInfo::skewed(
            HIGH_FREQ,
            "High Shelf Freq",
            20.0,
            16_000.0,
            4_500.0,
            "Hz",
            3.0,
        ),
        ParamInfo::continuous(HIGH_GAIN, "High Shelf Gain", 0.2, 1.0, 0.7, ""),
        ParamInfo::toggle(LOW_ON, "Low Shelf", true),
        ParamInfo::skewed(LOW_FREQ, "Low Shelf Freq", 20.0, 15_000.0, 90.0, "Hz", 3.0),
        ParamInfo::continuous(LOW_GAIN, "Low Shelf Gain", 0.2, 1.0, 0.75, ""),
        ParamInfo::continuous(DIFFUSION, "Diffusion", 0.0, 1.0, 0.6, ""),
        ParamInfo::continuous(SCALE, "Scale", 0.0, 1.0, 0.4, ""),
        ParamInfo::toggle(CHORUS_ON, "Chorus", true),
        ParamInfo::continuous(CHORUS_AMOUNT, "Chorus Amount", 0.0, 4.0, 0.3, "ms"),
        ParamInfo::skewed(CHORUS_RATE, "Chorus Rate", 0.01, 8.0, 0.8, "Hz", 3.0),
        ParamInfo::stepped(SMOOTH, "Smooth", &["None", "Slow", "Fast"], SMOOTH_FAST),
        ParamInfo::skewed(DECAY, "Decay", 0.2, 60.0, 2.5, "s", 3.0),
        ParamInfo::toggle(FREEZE, "Freeze", false),
        ParamInfo::toggle(FLAT, "Flat", true),
        ParamInfo::toggle(CUT, "Cut", true),
        ParamInfo::continuous(STEREO, "Stereo", 0.0, 120.0, 100.0, "deg"),
        ParamInfo::stepped(DENSITY, "Density", &["Sparse", "Low", "Mid", "High"], 2),
        ParamInfo::continuous(REFLECT, "Reflect", -30.0, 6.0, 0.0, "dB"),
        ParamInfo::continuous(DIFFUSE, "Diffuse", -30.0, 6.0, 0.0, "dB"),
    ]
}

const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;

#[derive(Default)]
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.pos];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

#[derive(Default)]
struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        let y = b - x;
        self.buf[self.pos] = x + b * 0.5;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

pub struct Reverb {
    v: Vec<f64>,
    sr: f32,
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    predelay: [DelayLine; 2],
    /// The full engine, used when Engine isn't Classic. Its buffers exist whichever is running,
    /// so the engine can change on the audio thread.
    studio: Studio,
}

impl Default for Reverb {
    fn default() -> Self {
        Self {
            v: defaults(&reverb_params()),
            sr: 48_000.0,
            combs: [Vec::new(), Vec::new()],
            allpasses: [Vec::new(), Vec::new()],
            predelay: [DelayLine::default(), DelayLine::default()],
            studio: Studio::default(),
        }
    }
}

impl Reverb {
    fn render(&mut self, audio: AudioBuffer) {
        if self.combs[0].is_empty() {
            return;
        }
        if self.v[ENGINE as usize].round() as usize != ENGINE_CLASSIC {
            self.studio.render(audio, &self.v);
            return;
        }
        let feedback = 0.7 + self.v[SIZE as usize] as f32 * 0.28;
        let damp = self.v[DAMPING as usize] as f32 * 0.4;
        let width = self.v[WIDTH as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let pre = (self.v[PREDELAY as usize] as f32 * 0.001 * self.sr).max(1.0);
        let wet1 = width / 2.0 + 0.5;
        let wet2 = (1.0 - width) / 2.0;
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for i in 0..left.len() {
            let (inl, inr) = (left[i], right[i]);
            let input = (inl + inr) * 0.015;
            self.predelay[0].write(input);
            let x = self.predelay[0].read(pre);
            let mut out = [0f32; 2];
            for (ch, o) in out.iter_mut().enumerate() {
                let mut acc = 0.0;
                for c in self.combs[ch].iter_mut() {
                    acc += c.process(x, feedback, damp);
                }
                for a in self.allpasses[ch].iter_mut() {
                    acc = a.process(acc);
                }
                *o = acc;
            }
            let wl = out[0] * wet1 + out[1] * wet2;
            let wr = out[1] * wet1 + out[0] * wet2;
            left[i] = inl * (1.0 - mix) + wl * mix;
            right[i] = inr * (1.0 - mix) + wr * mix;
        }
    }
}

impl Processor for Reverb {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.studio.prepare(sample_rate);
        let scale = sample_rate / 44_100.0;
        for ch in 0..2 {
            let spread = ch * STEREO_SPREAD;
            self.combs[ch] = COMB_TUNING
                .iter()
                .map(|&t| Comb {
                    buf: vec![0.0; (((t + spread) as f64) * scale) as usize],
                    pos: 0,
                    store: 0.0,
                })
                .collect();
            self.allpasses[ch] = ALLPASS_TUNING
                .iter()
                .map(|&t| Allpass {
                    buf: vec![0.0; (((t + spread) as f64) * scale) as usize],
                    pos: 0,
                })
                .collect();
            self.predelay[ch] = DelayLine::new((sample_rate * 0.25) as usize);
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
            self.studio.mark_dirty();
        }
    }

    fn reset(&mut self) {
        self.studio.clear();
        for ch in 0..2 {
            for c in self.combs[ch].iter_mut() {
                c.buf.fill(0.0);
                c.store = 0.0;
            }
            for a in self.allpasses[ch].iter_mut() {
                a.buf.fill(0.0);
            }
            self.predelay[ch].clear();
        }
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}
