//! One physical machine: component tolerances, thermal state and slow drift.
//!
//! Everything digital on a Juno-106 (DCO counters, envelopes, LFO) is identical from voice to voice,
//! so the differences live only where the real differences live: in the analog parts. Deviations
//! are stored as unit-variance draws and scaled by the Instability control when used, so that
//! turning it up exaggerates the same machine instead of making a different one.

use super::analog::Rng;

pub const VOICES: usize = 6;

/// Static per-voice draws for one 80017A, MC5534A channel and its trimmers.
#[derive(Clone, Copy, Default)]
pub struct VoiceDraw {
    /// Integrator R·C for 16', 8', 4' (1 % metal film, 2 % capacitor).
    pub integrator: [f32; 3],
    pub comparator: f32,
    pub cutoff_trim: f32,
    pub cutoff_scale: f32,
    pub cutoff_tempco: f32,
    pub stage_gm: [f32; 4],
    pub stage_offset: [f32; 4],
    pub resonance: f32,
    pub vca_gain: f32,
    pub vca_offset: f32,
    /// Where this free-running counter happened to be at power-on.
    pub phase: f64,
}

/// Slow time-varying state for one voice, updated at the CPU scan rate.
#[derive(Clone, Copy, Default)]
pub struct VoiceDrift {
    pub cutoff_slow: f32,
    pub cutoff_fast: f32,
    pub vca: f32,
}

pub struct Unit {
    pub voices: [VoiceDraw; VOICES],
    pub drift: [VoiceDrift; VOICES],
    /// 12-bit R-2R DAC transfer: code to fraction of full scale, with real bit-weight errors.
    pub dac: Vec<f32>,
    pub clock: f32,
    pub clock_tempco: f32,
    pub clock_wander: f32,
    pub chorus_rate: f32,
    pub chorus_clock: [f32; 2],
    pub chorus_filter: [f32; 2],
    pub chorus_bias: [f32; 2],
    /// Kelvin below thermal equilibrium; decays to 0 as the machine warms up.
    pub cold_kelvin: f32,
    rng: Rng,
}

/// Temperature rise of a Juno-106's interior from switch-on to equilibrium.
const WARMUP_KELVIN: f32 = 12.0;
/// The service notes demand a 10-minute warm-up before VCF adjustment: three time constants.
const WARMUP_TAU_S: f32 = 200.0;

impl Unit {
    pub fn new(serial: u32) -> Self {
        let mut unit = Self {
            voices: [VoiceDraw::default(); VOICES],
            drift: [VoiceDrift::default(); VOICES],
            dac: vec![0.0; 4096],
            clock: 0.0,
            clock_tempco: 0.0,
            clock_wander: 0.0,
            chorus_rate: 0.0,
            chorus_clock: [0.0; 2],
            chorus_filter: [0.0; 2],
            chorus_bias: [0.0; 2],
            cold_kelvin: 0.0,
            rng: Rng::new(serial),
        };
        unit.roll(serial);
        unit
    }

    /// Become the machine with this serial number. Does not allocate.
    pub fn roll(&mut self, serial: u32) {
        let mut r = Rng::new(serial);
        for v in self.voices.iter_mut() {
            *v = VoiceDraw {
                integrator: [r.gauss(), r.gauss(), r.gauss()],
                comparator: r.gauss(),
                cutoff_trim: r.gauss(),
                cutoff_scale: r.gauss(),
                cutoff_tempco: r.gauss(),
                stage_gm: [r.gauss(), r.gauss(), r.gauss(), r.gauss()],
                stage_offset: [r.gauss(), r.gauss(), r.gauss(), r.gauss()],
                resonance: r.gauss(),
                vca_gain: r.gauss(),
                vca_offset: r.gauss(),
                phase: (r.uniform() as f64 + 1.0) * 0.5,
            };
        }
        self.drift = [VoiceDrift::default(); VOICES];

        // Each R-2R bit carries its own weight error, worst at the most significant bits. This
        // gives the small non-monotonic jumps of a real ladder at major carries.
        let mut weights = [0.0f32; 12];
        for (bit, w) in weights.iter_mut().enumerate() {
            *w = (1u32 << bit) as f32 * (1.0 + r.gauss() * 0.0004);
        }
        let full: f32 = weights.iter().sum();
        for (code, out) in self.dac.iter_mut().enumerate() {
            let sum: f32 = (0..12)
                .filter(|b| code & (1 << b) != 0)
                .map(|b| weights[b])
                .sum();
            *out = sum / full;
        }

        self.clock = r.gauss();
        self.clock_tempco = r.gauss();
        self.clock_wander = 0.0;
        self.chorus_rate = r.gauss();
        self.chorus_clock = [r.gauss(), r.gauss()];
        self.chorus_filter = [r.gauss(), r.gauss()];
        self.chorus_bias = [r.gauss(), r.gauss()];
    }

    pub fn power_on(&mut self, cold: bool) {
        self.cold_kelvin = if cold { WARMUP_KELVIN } else { 0.0 };
    }

    /// Advance thermal state and random drift by one CPU scan.
    pub fn scan(&mut self, dt: f32) {
        self.cold_kelvin *= (-dt / WARMUP_TAU_S).exp();
        let r = &mut self.rng;
        for d in self.drift.iter_mut() {
            ou(&mut d.cutoff_slow, 40.0, dt, r);
            ou(&mut d.cutoff_fast, 0.4, dt, r);
            ou(&mut d.vca, 6.0, dt, r);
        }
        ou(&mut self.clock_wander, 25.0, dt, r);
    }
}

/// Unit-variance Ornstein-Uhlenbeck step.
fn ou(x: &mut f32, tau: f32, dt: f32, r: &mut Rng) {
    let a = dt / tau;
    *x += -a * *x + (2.0 * a).sqrt() * r.gauss();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_up_settles_inside_the_ten_minutes_the_notes_ask_for() {
        let mut u = Unit::new(1);
        u.power_on(true);
        for _ in 0..(600.0 / 0.1) as usize {
            u.scan(0.1);
        }
        assert!(u.cold_kelvin < 0.05 * WARMUP_KELVIN, "{}", u.cold_kelvin);
    }

    #[test]
    fn dac_ladder_is_near_ideal_and_each_serial_differs() {
        let (a, b) = (Unit::new(1), Unit::new(2));
        let lsb = 1.0 / 4095.0;
        assert!(a
            .dac
            .iter()
            .enumerate()
            .all(|(c, v)| (v - c as f32 * lsb).abs() < 4.0 * lsb));
        assert_ne!(a.dac, b.dac);
    }
}
