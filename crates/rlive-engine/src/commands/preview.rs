//! Previewing a file from the browser, ahead of the mixer.

use crate::{retire, Engine, Garbage};
use rlive_core::SampleBuffer;
use std::sync::Arc;

pub enum PreviewCmd {
    /// Start hearing a sample, replacing anything already previewing.
    Play(Arc<SampleBuffer>),
    Stop,
}

/// The sample being previewed, and how far it has got.
#[derive(Default)]
pub(crate) struct Preview {
    sample: Option<Arc<SampleBuffer>>,
    position: f64,
}

impl Preview {
    pub(crate) fn is_active(&self) -> bool {
        self.sample.is_some()
    }

    /// Mix the next `frames` into `left` and `right` at the sample's own pitch. Returns the
    /// sample once it has played out, to be retired.
    pub(crate) fn render(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        sample_rate: f64,
    ) -> Option<Arc<SampleBuffer>> {
        let sample = self.sample.as_ref()?;
        let rate = sample.sample_rate / sample_rate;
        let len = sample.left.len();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let at = self.position as usize;
            if at + 1 >= len {
                self.position = len as f64;
                break;
            }
            let t = (self.position - at as f64) as f32;
            *l += sample.left[at] + (sample.left[at + 1] - sample.left[at]) * t;
            *r += sample.right[at] + (sample.right[at + 1] - sample.right[at]) * t;
            self.position += rate;
        }
        if self.position >= (len.saturating_sub(1)) as f64 {
            self.position = 0.0;
            return self.sample.take();
        }
        None
    }
}

impl Engine {
    pub(crate) fn handle_preview(&mut self, cmd: PreviewCmd) {
        let next = match cmd {
            PreviewCmd::Play(sample) => Some(sample),
            PreviewCmd::Stop => None,
        };
        if let Some(old) = std::mem::replace(&mut self.preview.sample, next) {
            retire(&mut self.garbage, Garbage::Shared(old));
        }
        self.preview.position = 0.0;
    }
}
