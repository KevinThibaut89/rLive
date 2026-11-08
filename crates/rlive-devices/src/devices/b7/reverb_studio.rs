//! Reverb's full engine: Input Filter, Early Reflections, Diffusion Network, Chorus and the global
//! controls of manual pages 648-651. The Classic engine in [`crate::effects::reverb`] stays as it
//! was, so projects saved with it sound the same; this runs when Engine is set to Studio.
//!
//! The manual gives the Stereo range (0 to 120 degrees) and names the choosers' end points. It
//! gives no time or frequency ranges, tap times or smoothing times; the constants below are ours.

use super::tank::{Tank, TankSettings, TankShelves};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::{Biquad, BiquadKind};
use crate::effects::reverb as p;
use rlive_core::{db_to_gain, AudioBuffer};
use std::f32::consts::TAU;

/// The most Size can scale the delay lines by.
pub const MAX_ROOM_SCALE: f32 = 3.0;
/// Size 0..1 to the factor on the line delay times. Size 0 is a few samples: very coloured.
pub fn room_scale(size: f32) -> f32 {
    0.02 * 150f32.powf(size.clamp(0.0, 1.0))
}

/// Lines per Density setting.
pub const DENSITY_LINES: [usize; 4] = [4, 6, 8, 12];
/// How long Slow and Fast take to bring the delay times to a new Size, in seconds.
pub const SMOOTH_SECONDS: [f32; 3] = [0.0, 1.0, 0.15];
/// The early reflections' spacing at a room scale of 1, in ms. The first is at the Predelay.
pub const ER_TAP_MS: [f32; 8] = [0.0, 5.3, 11.1, 17.9, 23.7, 31.3, 41.0, 53.0];
/// Reflection decay constants at Shape 0 and Shape 1, in ms.
pub const ER_DECAY_MS: [f32; 2] = [40.0, 6.0];
/// How much later the diffused sound starts at Shape 1, in ms.
pub const MAX_ONSET_MS: f32 = 40.0;
/// Level of the first reflection, at a Reflect of 0 dB.
const ER_LEVEL: f32 = 0.5;
const MAX_SPIN_MS: f32 = 15.0;
const MAX_PREDELAY_S: f64 = 0.25;

/// See the module documentation.
#[derive(Default)]
pub struct Studio {
    sr: f32,
    tank: Tank,
    low_cut: [Biquad; 2],
    high_cut: [Biquad; 2],
    low_cut_on: bool,
    high_cut_on: bool,
    predelay: [InterpDelay2; 2],
    pre_current: f32,
    pre_target: f32,
    pre_placed: bool,
    er: [InterpDelay2; 2],
    onset: [InterpDelay2; 2],
    onset_samples: f32,
    er_gain: [f32; ER_TAP_MS.len()],
    er_offset: [f32; ER_TAP_MS.len()],
    spin_on: bool,
    spin_amount: f32,
    spin_step: f32,
    spin_phase: f32,
    freeze: bool,
    cut: bool,
    reflect: f32,
    diffuse: f32,
    width: f32,
    dirty: bool,
}

/// A delay line that starts empty and is sized in `prepare`.
#[derive(Clone)]
struct InterpDelay2(InterpDelay);

impl Default for InterpDelay2 {
    fn default() -> Self {
        Self(InterpDelay::new(16))
    }
}

impl Studio {
    pub fn prepare(&mut self, sample_rate: f64) {
        self.sr = sample_rate as f32;
        self.tank.prepare(sample_rate, MAX_ROOM_SCALE);
        let pre = (sample_rate * MAX_PREDELAY_S) as usize + 16;
        let er = ((ER_TAP_MS[7] * 1.5 * 1.13 + MAX_SPIN_MS + 2.0) * self.sr * 0.001) as usize + 16;
        let onset = (MAX_ONSET_MS * self.sr * 0.001) as usize + 16;
        for ch in 0..2 {
            self.predelay[ch] = InterpDelay2(InterpDelay::new(pre));
            self.er[ch] = InterpDelay2(InterpDelay::new(er));
            self.onset[ch] = InterpDelay2(InterpDelay::new(onset));
        }
        self.pre_placed = false;
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.tank.clear();
        for ch in 0..2 {
            self.low_cut[ch].reset();
            self.high_cut[ch].reset();
            self.predelay[ch].0.clear();
            self.er[ch].0.clear();
            self.onset[ch].0.clear();
        }
        self.spin_phase = 0.0;
        self.pre_placed = false;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn configure(&mut self, v: &[f64]) {
        let value = |id: u32| v[id as usize] as f32;
        let flag = |id: u32| v[id as usize] >= 0.5;

        let band = value(p::INPUT_FREQ);
        let width = 2f32.powf(value(p::INPUT_WIDTH) * 0.5);
        self.low_cut_on = flag(p::INPUT_LOW_ON);
        self.high_cut_on = flag(p::INPUT_HIGH_ON);
        for ch in 0..2 {
            self.low_cut[ch].set(BiquadKind::HighPass, band / width, 0.707, 0.0, self.sr);
            self.high_cut[ch].set(BiquadKind::LowPass, band * width, 0.707, 0.0, self.sr);
        }

        let size = value(p::SIZE);
        let scale = room_scale(size);
        let glide = SMOOTH_SECONDS[(v[p::SMOOTH as usize].round() as usize).min(2)];
        let freeze = flag(p::FREEZE);
        let density = (v[p::DENSITY as usize].round() as usize).min(3);
        let settings = TankSettings {
            lines: DENSITY_LINES[density],
            scale,
            allpass_scale: 0.05 + 0.95 * value(p::SCALE).clamp(0.0, 1.0),
            diffusion: 0.75 * value(p::DIFFUSION).clamp(0.0, 1.0),
            t60_low: value(p::DECAY),
            t60_high: value(p::DECAY),
            xover_hz: 200.0,
            damp: 0.0,
            mod_ms: if flag(p::CHORUS_ON) {
                value(p::CHORUS_AMOUNT)
            } else {
                0.0
            },
            mod_hz: value(p::CHORUS_RATE),
            freeze,
        };
        self.tank.set(&settings, glide);
        self.tank.set_shelves(&TankShelves {
            high_on: flag(p::HIGH_ON),
            high_hz: value(p::HIGH_FREQ),
            high_gain: value(p::HIGH_GAIN),
            low_on: flag(p::LOW_ON),
            low_hz: value(p::LOW_FREQ),
            low_gain: value(p::LOW_GAIN),
        });
        self.tank.bypass_shelves(freeze && flag(p::FLAT));

        let shape = value(p::ER_SHAPE).clamp(0.0, 1.0);
        let spacing = room_scale(size).clamp(0.25, 1.5);
        let tau = ER_DECAY_MS[0] + (ER_DECAY_MS[1] - ER_DECAY_MS[0]) * shape;
        for (k, (gain, offset)) in self
            .er_gain
            .iter_mut()
            .zip(self.er_offset.iter_mut())
            .enumerate()
        {
            let t = ER_TAP_MS[k] * spacing;
            *offset = t * self.sr * 0.001;
            *gain = (-t / tau).exp() * ER_LEVEL * if k % 2 == 0 { 1.0 } else { -1.0 };
        }
        self.onset_samples = (MAX_ONSET_MS * shape * self.sr * 0.001).round();
        self.spin_on = flag(p::SPIN_ON);
        self.spin_amount = value(p::SPIN_AMOUNT).clamp(0.0, MAX_SPIN_MS) * self.sr * 0.001;
        self.spin_step = value(p::SPIN_RATE) / self.sr;
        self.freeze = freeze;
        self.cut = flag(p::CUT);
        self.reflect = db_to_gain(value(p::REFLECT));
        self.diffuse = db_to_gain(value(p::DIFFUSE));
        self.width = (value(p::STEREO) / 120.0).clamp(0.0, 1.0);
        self.pre_target = (value(p::PREDELAY) * self.sr * 0.001).round().max(0.0);
        if !self.pre_placed {
            self.pre_current = self.pre_target;
            self.pre_placed = true;
        }
        self.dirty = false;
    }

    /// Renders in place, from the device's current parameter values.
    pub fn render(&mut self, audio: AudioBuffer, v: &[f64]) {
        if self.dirty {
            self.configure(v);
        }
        let mix = (v[p::MIX as usize] as f32).clamp(0.0, 1.0);
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        let pre_coeff = 1.0 - (-1.0f32 / (0.05 * self.sr)).exp();
        for i in 0..left.len() {
            let (dry_l, dry_r) = (left[i], right[i]);
            // Input filter.
            let mut x = [dry_l, dry_r];
            if self.low_cut_on {
                x[0] = self.low_cut[0].process(x[0]);
                x[1] = self.low_cut[1].process(x[1]);
            }
            if self.high_cut_on {
                x[0] = self.high_cut[0].process(x[0]);
                x[1] = self.high_cut[1].process(x[1]);
            }
            // Predelay, which sits ahead of everything that follows.
            if self.pre_current != self.pre_target {
                self.pre_current += (self.pre_target - self.pre_current) * pre_coeff;
                if (self.pre_current - self.pre_target).abs() < 0.01 {
                    self.pre_current = self.pre_target;
                }
            }
            for (ch, value) in x.iter_mut().enumerate() {
                self.predelay[ch].0.write(*value);
                *value = self.predelay[ch]
                    .0
                    .read(self.pre_current, Interpolation::Linear);
            }
            // Early reflections.
            self.spin_phase += self.spin_step;
            if self.spin_phase >= 1.0 {
                self.spin_phase -= 1.0;
            }
            let mut er = [0.0f32; 2];
            for ch in 0..2 {
                self.er[ch].0.write(x[ch]);
                let direction = if ch == 0 { 1.0 } else { -1.0 };
                let mut sum = 0.0;
                for k in 0..ER_TAP_MS.len() {
                    // The right ear's taps sit a little later than the left's, so the two hear
                    // different reflections even from a mono source.
                    let spread = if ch == 0 { 1.0 } else { 1.13 };
                    let spin = if self.spin_on && k > 0 {
                        let angle = TAU * (self.spin_phase + k as f32 / 8.0);
                        self.spin_amount * (1.0 + direction * angle.sin()) * 0.5
                    } else {
                        0.0
                    };
                    sum += self.er_gain[k]
                        * self.er[ch]
                            .0
                            .read(self.er_offset[k] * spread + spin, Interpolation::Linear);
                }
                er[ch] = sum;
            }
            // The diffuse field, starting later as Shape goes up.
            let mut late = [0.0f32; 2];
            for ch in 0..2 {
                self.onset[ch].0.write(x[ch]);
                late[ch] = self.onset[ch]
                    .0
                    .read(self.onset_samples, Interpolation::Linear);
            }
            if self.freeze && self.cut {
                late = [0.0, 0.0];
            }
            let diffuse = self.tank.process(late[0], late[1]);

            let out_a = er[0] * self.reflect + diffuse[0] * self.diffuse;
            let out_b = er[1] * self.reflect + diffuse[1] * self.diffuse;
            let (mid, side) = (0.5 * (out_a + out_b), 0.5 * (out_a - out_b));
            let wet_l = mid + side * self.width;
            let wet_r = mid - side * self.width;
            left[i] = dry_l * (1.0 - mix) + wet_l * mix;
            right[i] = dry_r * (1.0 - mix) + wet_r * mix;
        }
    }
}
