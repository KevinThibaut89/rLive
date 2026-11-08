use super::defaults;
use crate::dsp::Smoother;
use rlive_core::{db_to_gain, ModMode, ParamInfo, ProcessContext, Processor};

// ---------------------------------------------------------------- Utility

pub const GAIN: u32 = 0;
pub const PAN: u32 = 1;
pub const WIDTH: u32 = 2;
pub const MONO: u32 = 3;
pub const INVERT_L: u32 = 4;
pub const INVERT_R: u32 = 5;

pub fn utility_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(GAIN, "Gain", -48.0, 24.0, 0.0, "dB").with_mod_mode(ModMode::Scale),
        ParamInfo::continuous(PAN, "Pan", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 2.0, 1.0, ""),
        ParamInfo::toggle(MONO, "Mono", false),
        ParamInfo::toggle(INVERT_L, "Phase L", false),
        ParamInfo::toggle(INVERT_R, "Phase R", false),
    ]
}

pub struct Utility {
    v: Vec<f64>,
    gain_l: Smoother,
    gain_r: Smoother,
}

impl Default for Utility {
    fn default() -> Self {
        Self {
            v: defaults(&utility_params()),
            gain_l: Smoother::new(1.0),
            gain_r: Smoother::new(1.0),
        }
    }
}

impl Processor for Utility {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.gain_l.set_time(0.01, sample_rate as f32);
        self.gain_r.set_time(0.01, sample_rate as f32);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let g = db_to_gain(self.v[GAIN as usize] as f32);
        let pan = self.v[PAN as usize] as f32;
        let width = if self.v[MONO as usize] >= 0.5 {
            0.0
        } else {
            self.v[WIDTH as usize] as f32
        };
        let inv_l = if self.v[INVERT_L as usize] >= 0.5 {
            -1.0
        } else {
            1.0
        };
        let inv_r = if self.v[INVERT_R as usize] >= 0.5 {
            -1.0
        } else {
            1.0
        };
        self.gain_l.target = g * (1.0 - pan).min(1.0) * inv_l;
        self.gain_r.target = g * (1.0 + pan).min(1.0) * inv_r;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
        for i in 0..left.len() {
            let mid = (left[i] + right[i]) * 0.5;
            let side = (left[i] - right[i]) * 0.5 * width;
            left[i] = (mid + side) * self.gain_l.next();
            right[i] = (mid - side) * self.gain_r.next();
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }
}
