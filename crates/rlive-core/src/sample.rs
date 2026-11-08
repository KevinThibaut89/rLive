//! Decoded audio shared by samplers and audio clips.

/// Decoded audio file, used by the sampler and audio clips.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleBuffer {
    pub name: String,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: f64,
}

impl SampleBuffer {
    pub fn len(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn duration_secs(&self) -> f64 {
        self.len() as f64 / self.sample_rate
    }
}
