//! The CPU meter's arithmetic (manual pp. 982-984).
//!
//! The figure is the time a buffer took to process over the time it plays for, so 50% is half the
//! deadline used and anything over 100% is an overload. Live's meter is defined this way, and with
//! worker threads it is measured the same: wall-clock time on the audio callback, which includes
//! any time spent waiting for workers.

/// How fast the Average reading follows the load, as an e-folding time in seconds.
const AVERAGE_SECONDS: f64 = 0.5;

/// Average load over time, and what the last buffer took.
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuMeter {
    average: f32,
    started: bool,
}

impl CpuMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the time one buffer of `budget` seconds took to process, and give back the Average
    /// and Current loads, as fractions of the budget.
    pub fn update(&mut self, elapsed: f64, budget: f64) -> (f32, f32) {
        if budget <= 0.0 {
            return (self.average, 0.0);
        }
        let current = (elapsed / budget) as f32;
        let k = 1.0 - (-budget / AVERAGE_SECONDS).exp();
        self.average = if self.started {
            self.average + (current - self.average) * k as f32
        } else {
            self.started = true;
            current
        };
        (self.average, current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_the_deadline_reads_fifty_percent() {
        let mut meter = CpuMeter::new();
        let (average, current) = meter.update(0.005, 0.01);
        assert_eq!(current, 0.5);
        assert_eq!(average, 0.5, "the first buffer sets the average");
    }

    #[test]
    fn a_buffer_that_takes_longer_than_it_lasts_reads_over_a_hundred_percent() {
        let mut meter = CpuMeter::new();
        let (_, current) = meter.update(0.03, 0.01);
        assert!(current > 1.0);
        assert_eq!(current, 3.0);
    }

    #[test]
    fn the_average_moves_towards_the_load_over_time_constant() {
        let mut meter = CpuMeter::new();
        meter.update(0.0, 0.01);
        let budget = 0.01;
        let mut average = 0.0;
        // Hold a steady 100% load for one e-folding time.
        for _ in 0..(AVERAGE_SECONDS / budget) as usize {
            average = meter.update(budget, budget).0;
        }
        let target = 1.0 - (-1.0f32).exp();
        assert!(
            (average - target).abs() < 0.01,
            "{average} should be near {target}"
        );
    }

    #[test]
    fn a_short_spike_barely_moves_the_average_but_shows_in_current() {
        let mut meter = CpuMeter::new();
        for _ in 0..1000 {
            meter.update(0.001, 0.01);
        }
        let (average, current) = meter.update(0.05, 0.01);
        assert_eq!(current, 5.0);
        assert!(average < 0.2, "average {average}");
    }

    #[test]
    fn the_same_load_reads_the_same_at_any_buffer_size() {
        let mut small = CpuMeter::new();
        let mut large = CpuMeter::new();
        small.update(0.0, 0.001);
        large.update(0.0, 0.01);
        // A second of 70% load, one way or the other.
        let mut a = 0.0;
        for _ in 0..1000 {
            a = small.update(0.0007, 0.001).0;
        }
        let mut b = 0.0;
        for _ in 0..100 {
            b = large.update(0.007, 0.01).0;
        }
        assert!((a - b).abs() < 0.01, "{a} and {b} differ");
    }

    #[test]
    fn no_budget_is_no_reading() {
        let mut meter = CpuMeter::new();
        assert_eq!(meter.update(1.0, 0.0), (0.0, 0.0));
    }
}
