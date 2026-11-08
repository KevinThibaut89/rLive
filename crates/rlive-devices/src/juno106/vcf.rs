//! IR3109 four-pole OTA cascade with BA662 resonance feedback (one half of the 80017A module).
//!
//! Each stage is `C·dy/dt = gm·S·tanh((x - y + offset) / S)`. Because the transconductance control
//! current sets both gain and output swing, the saturating current scales with the cutoff. The
//! feedback has no passband compensation, so raising resonance thins out the bass as on the real
//! chip. Solved implicitly (trapezoidal), with each `tanh` linearised at the last operating point.

use super::analog::tanh_ratio;

/// Stage saturation voltage referred to the VCF input node.
pub const STAGE_SAT_V: f32 = 8.0;
/// Saturation of the resonance OTA, referred to the VCF output.
pub const FEEDBACK_SAT_V: f32 = 12.0;

#[derive(Clone, Copy, Default)]
pub struct Ota4 {
    s: [f32; 4],
    v: [f32; 4],
    y4: f32,
}

impl Ota4 {
    /// Advance one sample. `g` are the per-stage integrator gains `tan(π·f/fs)`, `k` the resonance
    /// loop gain, `offset` the per-stage input offsets in volts.
    #[inline]
    pub fn process(&mut self, input: f32, g: &[f32; 4], k: f32, offset: &[f32; 4]) -> f32 {
        let t = [
            tanh_ratio(self.v[0] / STAGE_SAT_V),
            tanh_ratio(self.v[1] / STAGE_SAT_V),
            tanh_ratio(self.v[2] / STAGE_SAT_V),
            tanh_ratio(self.v[3] / STAGE_SAT_V),
        ];
        let tf = tanh_ratio(self.y4 / FEEDBACK_SAT_V);

        let mut a = [0.0f32; 4];
        let mut b = [0.0f32; 4];
        for i in 0..4 {
            let gt = g[i] * t[i];
            let d = 1.0 / (1.0 + gt);
            a[i] = gt * d;
            b[i] = (self.s[i] + gt * offset[i]) * d;
        }

        // y4 = A·u + B, u = input - K·y4; solve the feedback loop in closed form.
        let aa = a[0] * a[1] * a[2] * a[3];
        let bb = a[3] * (a[2] * (a[1] * b[0] + b[1]) + b[2]) + b[3];
        let kk = k * tf;
        let y4 = (aa * input + bb) / (1.0 + aa * kk);
        let u = input - kk * y4;

        let y1 = a[0] * u + b[0];
        let y2 = a[1] * y1 + b[1];
        let y3 = a[2] * y2 + b[2];

        self.v = [
            u - y1 + offset[0],
            y1 - y2 + offset[1],
            y2 - y3 + offset[2],
            y3 - y4 + offset[3],
        ];
        self.s = [
            2.0 * y1 - self.s[0],
            2.0 * y2 - self.s[1],
            2.0 * y3 - self.s[2],
            2.0 * y4 - self.s[3],
        ];
        self.y4 = y4;

        if !y4.is_finite() {
            *self = Self::default();
            return 0.0;
        }
        y4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 192_000.0;

    fn gain_at(hz: f32, cutoff: f32, k: f32) -> f32 {
        let g = (std::f32::consts::PI * cutoff / SR).tan();
        let mut f = Ota4::default();
        let n = SR as usize;
        let mut peak = 0.0f32;
        for i in 0..n {
            let x = 0.01 * (std::f64::consts::TAU * hz as f64 * i as f64 / SR as f64).sin() as f32;
            let y = f.process(x, &[g; 4], k, &[0.0; 4]);
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak / 0.01
    }

    #[test]
    fn rolls_off_at_24_db_per_octave() {
        let a = gain_at(4_000.0, 1_000.0, 0.0);
        let b = gain_at(8_000.0, 1_000.0, 0.0);
        let db = 20.0 * (b / a).log10();
        assert!((db + 24.0).abs() < 1.5, "{db}");
    }

    #[test]
    fn resonance_costs_bass() {
        // DC gain of the closed loop is 1 / (1 + k).
        let g = gain_at(30.0, 2_000.0, 3.0);
        assert!((g - 0.25).abs() < 0.02, "{g}");
    }
}
