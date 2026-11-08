use super::defaults;
use crate::dsp::soft_clip;
use rlive_core::{db_to_gain, ModMode, ParamInfo, ProcessContext, Processor};

// ---------------------------------------------------------------- Saturator

pub const DRIVE: u32 = 0;
pub const OUTPUT: u32 = 1;
pub const MIX: u32 = 2;

pub fn saturator_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(DRIVE, "Drive", 0.0, 36.0, 6.0, "dB"),
        ParamInfo::continuous(OUTPUT, "Output", -36.0, 6.0, -3.0, "dB")
            .with_mod_mode(ModMode::Scale),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
    ]
}

pub struct Saturator {
    v: Vec<f64>,
}

impl Default for Saturator {
    fn default() -> Self {
        Self {
            v: defaults(&saturator_params()),
        }
    }
}

impl Processor for Saturator {
    fn activate(&mut self, _: f64, _: usize) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        let drive = db_to_gain(self.v[DRIVE as usize] as f32);
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        let mix = self.v[MIX as usize] as f32;
        for s in ctx.audio.left.iter_mut().chain(ctx.audio.right.iter_mut()) {
            let wet = soft_clip(*s * drive) * out;
            *s = *s * (1.0 - mix) + wet * mix;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }
}
