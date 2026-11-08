use super::defaults;
use crate::dsp::DelayLine;
use rlive_core::{ParamInfo, ProcessContext, Processor};

// ---------------------------------------------------------------- Reverb (Freeverb)

pub const SIZE: u32 = 0;
pub const DAMPING: u32 = 1;
pub const WIDTH: u32 = 2;
pub const MIX: u32 = 3;
pub const PREDELAY: u32 = 4;

pub fn reverb_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(SIZE, "Size", 0.0, 1.0, 0.7, ""),
        ParamInfo::continuous(DAMPING, "Damping", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 0.25, ""),
        ParamInfo::continuous(PREDELAY, "Pre-delay", 0.0, 200.0, 10.0, "ms"),
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
}

impl Default for Reverb {
    fn default() -> Self {
        Self {
            v: defaults(&reverb_params()),
            sr: 48_000.0,
            combs: [Vec::new(), Vec::new()],
            allpasses: [Vec::new(), Vec::new()],
            predelay: [DelayLine::default(), DelayLine::default()],
        }
    }
}

impl Processor for Reverb {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
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
        if self.combs[0].is_empty() {
            return;
        }
        let feedback = 0.7 + self.v[SIZE as usize] as f32 * 0.28;
        let damp = self.v[DAMPING as usize] as f32 * 0.4;
        let width = self.v[WIDTH as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let pre = (self.v[PREDELAY as usize] as f32 * 0.001 * self.sr).max(1.0);
        let wet1 = width / 2.0 + 0.5;
        let wet2 = (1.0 - width) / 2.0;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
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

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
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
}
