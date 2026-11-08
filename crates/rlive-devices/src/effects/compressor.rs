use super::defaults;
use rlive_core::{db_to_gain, gain_to_db, ParamInfo, ProcessContext, Processor};

// ---------------------------------------------------------------- Compressor

pub const THRESHOLD: u32 = 0;
pub const RATIO: u32 = 1;
pub const ATTACK: u32 = 2;
pub const RELEASE: u32 = 3;
pub const KNEE: u32 = 4;
pub const MAKEUP: u32 = 5;
pub const MIX: u32 = 6;

pub fn compressor_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(THRESHOLD, "Threshold", -60.0, 0.0, -18.0, "dB"),
        ParamInfo::skewed(RATIO, "Ratio", 1.0, 20.0, 4.0, ":1", 2.0),
        ParamInfo::skewed(ATTACK, "Attack", 0.1, 200.0, 10.0, "ms", 3.0),
        ParamInfo::skewed(RELEASE, "Release", 5.0, 2_000.0, 120.0, "ms", 3.0),
        ParamInfo::continuous(KNEE, "Knee", 0.0, 18.0, 6.0, "dB"),
        ParamInfo::continuous(MAKEUP, "Makeup", 0.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
    ]
}

pub struct Compressor {
    v: Vec<f64>,
    sr: f32,
    env_db: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Self {
            v: defaults(&compressor_params()),
            sr: 48_000.0,
            env_db: 0.0,
        }
    }
}

impl Compressor {
    fn gain_reduction(&self, level_db: f32) -> f32 {
        let t = self.v[THRESHOLD as usize] as f32;
        let r = self.v[RATIO as usize] as f32;
        let k = self.v[KNEE as usize] as f32;
        let over = level_db - t;
        let slope = 1.0 - 1.0 / r.max(1.0);
        if k > 0.0 && over.abs() <= k / 2.0 {
            slope * (over + k / 2.0).powi(2) / (2.0 * k)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }
}

impl Processor for Compressor {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let att = (-1.0 / (self.v[ATTACK as usize] as f32 * 0.001 * self.sr)).exp();
        let rel = (-1.0 / (self.v[RELEASE as usize] as f32 * 0.001 * self.sr)).exp();
        let makeup = self.v[MAKEUP as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
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

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        self.env_db = 0.0;
    }
}
