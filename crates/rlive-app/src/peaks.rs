//! Min/max summaries of audio, so waveforms draw without rescanning samples.

const BASE_BLOCK: usize = 256;

/// Summaries of a channel at doubling block sizes.
pub struct Peaks {
    levels: Vec<Vec<(f32, f32)>>,
    frames: usize,
}

impl Peaks {
    pub fn build(samples: &[f32]) -> Self {
        let mut level: Vec<(f32, f32)> = samples
            .chunks(BASE_BLOCK)
            .map(|c| {
                c.iter()
                    .fold((f32::MAX, f32::MIN), |(lo, hi), s| (lo.min(*s), hi.max(*s)))
            })
            .collect();
        let mut levels = Vec::new();
        while level.len() > 1 {
            let next = level
                .chunks(2)
                .map(|c| {
                    c.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                        (lo.min(p.0), hi.max(p.1))
                    })
                })
                .collect();
            levels.push(std::mem::replace(&mut level, next));
        }
        levels.push(level);
        Self {
            levels,
            frames: samples.len(),
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The lowest and highest sample in `from..to`, accurate to one summary block.
    pub fn min_max(&self, from: usize, to: usize) -> (f32, f32) {
        let to = to.min(self.frames);
        if from >= to {
            return (0.0, 0.0);
        }
        let want = (to - from) / BASE_BLOCK;
        let index = (usize::BITS - want.leading_zeros()) as usize;
        let index = index.saturating_sub(1).min(self.levels.len() - 1);
        let block = BASE_BLOCK << index;
        let level = &self.levels[index];
        let (first, last) = (from / block, (to - 1) / block);
        level[first..=last.min(level.len() - 1)]
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                (lo.min(p.0), hi.max(p.1))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saw(n: usize) -> Vec<f32> {
        (0..n).map(|i| ((i % 1000) as f32 / 500.0) - 1.0).collect()
    }

    #[test]
    fn long_ranges_see_the_full_extent() {
        let p = Peaks::build(&saw(100_000));
        let (lo, hi) = p.min_max(0, 100_000);
        assert!(lo <= -0.99 && hi >= 0.99);
    }

    #[test]
    fn a_spike_is_found_at_every_zoom() {
        let mut s = vec![0.0; 50_000];
        s[31_234] = 0.9;
        let p = Peaks::build(&s);
        for width in [300, 3_000, 30_000] {
            assert_eq!(p.min_max(31_234 - width / 2, 31_234 + width / 2).1, 0.9);
        }
    }

    #[test]
    fn out_of_range_is_silence() {
        let p = Peaks::build(&saw(1_000));
        assert_eq!(p.min_max(2_000, 3_000), (0.0, 0.0));
        assert_eq!(p.min_max(10, 10), (0.0, 0.0));
    }

    #[test]
    fn empty_input_is_safe() {
        let p = Peaks::build(&[]);
        assert_eq!(p.min_max(0, 100), (0.0, 0.0));
    }
}
