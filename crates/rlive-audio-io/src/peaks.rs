//! Min/max summaries of audio, so waveforms draw without rescanning samples.

/// Frames per entry at the finest level.
pub const BASE_BLOCK: usize = 256;

/// One channel's summaries at doubling block sizes.
#[derive(Debug, Clone, PartialEq)]
struct Channel {
    levels: Vec<Vec<(f32, f32)>>,
}

impl Channel {
    fn build(samples: &[f32]) -> Self {
        Self::from_base(
            samples
                .chunks(BASE_BLOCK)
                .map(|c| {
                    c.iter()
                        .fold((f32::MAX, f32::MIN), |(lo, hi), s| (lo.min(*s), hi.max(*s)))
                })
                .collect(),
        )
    }

    fn from_base(mut level: Vec<(f32, f32)>) -> Self {
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
        Self { levels }
    }

    fn base(&self) -> &[(f32, f32)] {
        &self.levels[0]
    }

    fn min_max(&self, from: usize, to: usize) -> (f32, f32) {
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

/// Waveform summaries of a mono or stereo file.
#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    left: Channel,
    /// `None` for a mono file.
    right: Option<Channel>,
    frames: usize,
}

impl Peaks {
    pub fn mono(samples: &[f32]) -> Self {
        Self {
            left: Channel::build(samples),
            right: None,
            frames: samples.len(),
        }
    }

    pub fn stereo(left: &[f32], right: &[f32]) -> Self {
        Self {
            left: Channel::build(left),
            right: Some(Channel::build(right)),
            frames: left.len().min(right.len()),
        }
    }

    /// Rebuilds the coarser levels from the finest ones, as stored in an analysis file.
    pub fn from_base(left: Vec<(f32, f32)>, right: Option<Vec<(f32, f32)>>, frames: usize) -> Self {
        Self {
            left: Channel::from_base(left),
            right: right.map(Channel::from_base),
            frames,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn is_stereo(&self) -> bool {
        self.right.is_some()
    }

    /// The finest level of the left channel.
    pub fn left_base(&self) -> &[(f32, f32)] {
        self.left.base()
    }

    /// The finest level of the right channel, in a stereo file.
    pub fn right_base(&self) -> Option<&[(f32, f32)]> {
        self.right.as_ref().map(Channel::base)
    }

    fn range(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let to = to.min(self.frames);
        (from < to).then_some((from, to))
    }

    /// The lowest and highest sample of channel 0 (left) or 1 (right) in `from..to`, accurate to
    /// one summary block. A mono file answers the same for both.
    pub fn min_max(&self, channel: usize, from: usize, to: usize) -> (f32, f32) {
        let Some((from, to)) = self.range(from, to) else {
            return (0.0, 0.0);
        };
        match (channel, &self.right) {
            (1, Some(right)) => right.min_max(from, to),
            _ => self.left.min_max(from, to),
        }
    }

    /// The extent over both channels, for where only one lane of waveform fits.
    pub fn min_max_mixed(&self, from: usize, to: usize) -> (f32, f32) {
        let (a_lo, a_hi) = self.min_max(0, from, to);
        let (b_lo, b_hi) = self.min_max(1, from, to);
        (a_lo.min(b_lo), a_hi.max(b_hi))
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
        let p = Peaks::mono(&saw(100_000));
        let (lo, hi) = p.min_max(0, 0, 100_000);
        assert!(lo <= -0.99 && hi >= 0.99);
    }

    #[test]
    fn a_spike_is_found_at_every_zoom() {
        let mut s = vec![0.0; 50_000];
        s[31_234] = 0.9;
        let p = Peaks::mono(&s);
        for width in [300, 3_000, 30_000] {
            assert_eq!(p.min_max(0, 31_234 - width / 2, 31_234 + width / 2).1, 0.9);
        }
    }

    #[test]
    fn out_of_range_is_silence() {
        let p = Peaks::mono(&saw(1_000));
        assert_eq!(p.min_max(0, 2_000, 3_000), (0.0, 0.0));
        assert_eq!(p.min_max(0, 10, 10), (0.0, 0.0));
    }

    #[test]
    fn empty_input_is_safe() {
        let p = Peaks::stereo(&[], &[]);
        assert_eq!(p.min_max(0, 0, 100), (0.0, 0.0));
        assert_eq!(p.min_max(1, 0, 100), (0.0, 0.0));
    }

    #[test]
    fn stereo_channels_are_summarised_separately() {
        let left = vec![0.25f32; 4_000];
        let mut right = vec![-0.1f32; 4_000];
        right[2_000] = 0.75;
        let p = Peaks::stereo(&left, &right);
        assert!(p.is_stereo());
        assert_eq!(p.min_max(0, 0, 4_000), (0.25, 0.25));
        assert_eq!(p.min_max(1, 0, 4_000), (-0.1, 0.75));
        assert_eq!(p.min_max_mixed(0, 4_000), (-0.1, 0.75));
    }

    #[test]
    fn a_mono_file_answers_the_same_for_both_channels() {
        let p = Peaks::mono(&[0.5; 600]);
        assert!(!p.is_stereo());
        assert_eq!(p.min_max(1, 0, 600), (0.5, 0.5));
    }

    #[test]
    fn rebuilding_from_the_finest_level_gives_the_same_answers() {
        let p = Peaks::stereo(&saw(50_000), &saw(50_000));
        let q = Peaks::from_base(
            p.left_base().to_vec(),
            p.right_base().map(<[_]>::to_vec),
            p.frames(),
        );
        assert_eq!(p, q);
    }
}
