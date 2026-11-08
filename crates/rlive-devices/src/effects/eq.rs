use super::defaults;
use crate::dsp::{Biquad, BiquadKind};
use rlive_core::{db_to_gain, ModMode, ParamInfo, ProcessContext, Processor};

// ---------------------------------------------------------------- EQ Three

pub const LOW_GAIN: u32 = 0;
pub const LOW_FREQ: u32 = 1;
pub const MID_GAIN: u32 = 2;
pub const MID_FREQ: u32 = 3;
pub const MID_Q: u32 = 4;
pub const HIGH_GAIN: u32 = 5;
pub const HIGH_FREQ: u32 = 6;
pub const OUTPUT: u32 = 7;

pub fn eq_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(LOW_GAIN, "Low", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::skewed(LOW_FREQ, "Low Freq", 30.0, 1_000.0, 200.0, "Hz", 2.5),
        ParamInfo::continuous(MID_GAIN, "Mid", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::skewed(MID_FREQ, "Mid Freq", 100.0, 10_000.0, 1_000.0, "Hz", 2.5),
        ParamInfo::skewed(MID_Q, "Mid Q", 0.1, 10.0, 0.8, "", 2.0),
        ParamInfo::continuous(HIGH_GAIN, "High", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::skewed(
            HIGH_FREQ,
            "High Freq",
            1_000.0,
            18_000.0,
            4_000.0,
            "Hz",
            2.5,
        ),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 12.0, 0.0, "dB")
            .with_mod_mode(ModMode::Scale),
    ]
}

pub struct Eq3 {
    v: Vec<f64>,
    sr: f32,
    bands: [[Biquad; 3]; 2],
    dirty: bool,
}

impl Default for Eq3 {
    fn default() -> Self {
        Self {
            v: defaults(&eq_params()),
            sr: 48_000.0,
            bands: [[Biquad::default(); 3]; 2],
            dirty: true,
        }
    }
}

impl Processor for Eq3 {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        if self.dirty {
            let g = |id: u32| self.v[id as usize] as f32;
            for ch in 0..2 {
                self.bands[ch][0].set(
                    BiquadKind::LowShelf,
                    g(LOW_FREQ),
                    0.707,
                    g(LOW_GAIN),
                    self.sr,
                );
                self.bands[ch][1].set(
                    BiquadKind::Peak,
                    g(MID_FREQ),
                    g(MID_Q),
                    g(MID_GAIN),
                    self.sr,
                );
                self.bands[ch][2].set(
                    BiquadKind::HighShelf,
                    g(HIGH_FREQ),
                    0.707,
                    g(HIGH_GAIN),
                    self.sr,
                );
            }
            self.dirty = false;
        }
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        for (ch, buf) in [&mut *ctx.audio.left, &mut *ctx.audio.right]
            .into_iter()
            .enumerate()
        {
            for s in buf.iter_mut() {
                let mut x = *s;
                for b in self.bands[ch].iter_mut() {
                    x = b.process(x);
                }
                *s = x * out;
            }
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        for ch in self.bands.iter_mut() {
            for b in ch.iter_mut() {
                b.reset();
            }
        }
    }
}
