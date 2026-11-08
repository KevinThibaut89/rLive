//! DCO and wave shaper: one half of an MC5534A driven by one 8253 counter.
//!
//! Service notes p.9. The counter output resets an integrator (saw), clocks a flip-flop (sub) and
//! the comparator slices the saw against the PWM CV (pulse). Everything derives from the actual
//! integrator voltage, so saw amplitude errors and pulse-width errors come out of the same state,
//! as they do on the board. Edges are band-limited with a four-point B-spline residual.

/// Everything the wave shaper needs for one sample.
#[derive(Clone, Copy)]
pub struct DcoInputs {
    /// Integrator slope in V/s: `DCO CV / (R·C)`.
    pub slope: f64,
    /// Comparator threshold in volts. Below 0 V the pulse stays high (pulse switched off).
    pub pwm_v: f64,
    /// Saw gain from integrator volts to the VCF input node. 0 when the saw is switched off.
    pub saw: f32,
    /// Pulse height in volts at the VCF input node.
    pub pulse_v: f32,
    /// Sub height in volts at the VCF input node.
    pub sub_v: f32,
    /// Period the counter will reload at its next terminal count, in seconds.
    pub next_period: f64,
}

#[derive(Clone, Copy)]
pub struct Dco {
    /// Seconds until the next positive-going counter edge.
    t_edge: f64,
    /// Integrator voltage.
    pub ramp: f64,
    sub: bool,
    pulse: bool,
    /// Samples n-1, n, n+1, n+2 around the current one: band-limiting needs two of look-ahead.
    line: [f32; 4],
}

/// Integrated cubic B-spline minus the unit step, for `t` in `-2..2` samples from the edge.
#[inline]
fn residual(t: f32) -> f32 {
    if t < -1.0 {
        let u = t + 2.0;
        u * u * u * u / 24.0
    } else if t < 0.0 {
        let u = t + 1.0;
        1.0 / 24.0 + (u + 1.5 * u * u + u * u * u - 0.75 * u * u * u * u) / 6.0
    } else if t < 1.0 {
        let u = t;
        (4.0 * u - 2.0 * u * u * u + 0.75 * u * u * u * u) / 6.0 - 0.5
    } else {
        let u = 2.0 - t;
        -u * u * u * u / 24.0
    }
}

impl Dco {
    /// `phase` in `0..1` sets where in its cycle this free-running counter starts.
    pub fn new(phase: f64, period: f64) -> Self {
        Self {
            t_edge: period * (1.0 - phase),
            ramp: 0.0,
            sub: false,
            pulse: false,
            line: [0.0; 4],
        }
    }

    #[inline]
    pub fn tick(&mut self, dt: f64, i: &DcoInputs) -> f32 {
        let inv = 1.0 / dt;
        let mut line = self.line;
        let mut blep = |step: f32, at: f64| {
            // `d` is the distance from the edge to the end of this sample, in samples.
            let d = ((dt - at) * inv).clamp(0.0, 1.0) as f32;
            line[0] += step * residual(d - 2.0);
            line[1] += step * residual(d - 1.0);
            line[2] += step * residual(d);
            line[3] += step * residual(d + 1.0);
        };

        let mut t = 0.0f64;
        loop {
            let seg_end = (t + self.t_edge).min(dt);
            let seg = seg_end - t;
            let r1 = self.ramp + i.slope * seg;
            if !self.pulse && r1 > i.pwm_v {
                let cross = if i.slope > 0.0 {
                    t + ((i.pwm_v - self.ramp) / i.slope).clamp(0.0, seg)
                } else {
                    t
                };
                blep(i.pulse_v, cross);
                self.pulse = true;
            } else if self.pulse && r1 < i.pwm_v {
                // The PWM CV moved above the ramp while the output was high.
                blep(-i.pulse_v, seg_end);
                self.pulse = false;
            }
            self.ramp = r1;
            self.t_edge -= seg;
            t = seg_end;
            if self.t_edge > 0.0 {
                break;
            }

            // Positive counter edge: integrator discharges, flip-flop toggles, comparator resets.
            let mut step = -i.saw * self.ramp as f32;
            self.ramp = 0.0;
            if self.pulse && i.pwm_v >= 0.0 {
                step -= i.pulse_v;
                self.pulse = false;
            }
            self.sub = !self.sub;
            step += if self.sub { i.sub_v } else { -i.sub_v };
            blep(step, t);
            self.t_edge += i.next_period;
            if t >= dt {
                break;
            }
        }

        let now = i.saw * self.ramp as f32
            + if self.pulse { i.pulse_v } else { 0.0 }
            + if self.sub { i.sub_v } else { 0.0 };
        line[2] += now;
        let out = line[0];
        self.line = [line[1], line[2], line[3], 0.0];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 192_000.0;

    fn inputs(hz: f64, pwm_v: f64) -> DcoInputs {
        DcoInputs {
            // 12 Vp-p at this frequency.
            slope: 12.0 * hz,
            pwm_v,
            saw: 0.0,
            pulse_v: 1.0,
            sub_v: 0.0,
            next_period: 1.0 / hz,
        }
    }

    fn duty(pwm_v: f64) -> f64 {
        let i = inputs(250.0, pwm_v);
        let mut d = Dco::new(0.0, i.next_period);
        let n = (SR * 2.0) as usize;
        let high: f64 = (0..n).map(|_| d.tick(1.0 / SR, &i) as f64).sum();
        high / n as f64
    }

    #[test]
    fn duty_follows_the_service_note_cv_points() {
        // Service notes p.9: 50 % at +6 V, 95 % at +0.6 V, disabled (stuck high) at -0.8 V.
        assert!((duty(6.0) - 0.50).abs() < 0.002, "{}", duty(6.0));
        assert!((duty(0.6) - 0.95).abs() < 0.002, "{}", duty(0.6));
        assert!(duty(-0.8) > 0.999, "{}", duty(-0.8));
    }

    #[test]
    fn sub_runs_at_half_the_counter_rate() {
        let mut i = inputs(1000.0, -0.8);
        i.pulse_v = 0.0;
        i.sub_v = 1.0;
        let mut d = Dco::new(0.0, i.next_period);
        let n = SR as usize;
        let s: Vec<f32> = (0..n).map(|_| d.tick(1.0 / SR, &i)).collect();
        let rising = s.windows(2).filter(|w| w[0] < 0.5 && w[1] >= 0.5).count();
        assert!((rising as i64 - 500).abs() <= 1, "{rising}");
    }
}
