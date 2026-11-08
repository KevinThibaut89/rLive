//! Built-in audio effects.

pub mod compressor;
pub mod delay;
pub mod eq;
pub mod reverb;
pub mod saturator;
pub mod utility;

pub use compressor as comp;
pub use compressor::{compressor_params, Compressor};
pub use delay::{delay_params, Delay};
pub use eq::{eq_params, Eq3};
pub use reverb::{reverb_params, Reverb};
pub use saturator as sat;
pub use saturator::{saturator_params, Saturator};
pub use utility as util;
pub use utility::{utility_params, Utility};

use rlive_core::ParamInfo;

fn defaults(params: &[ParamInfo]) -> Vec<f64> {
    let max = params.iter().map(|p| p.id as usize).max().unwrap_or(0);
    let mut v = vec![0.0; max + 1];
    for p in params {
        v[p.id as usize] = p.default;
    }
    v
}
