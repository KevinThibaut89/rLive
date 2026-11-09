//! Previewing a file from the browser, ahead of the mixer (manual pp. 118-120).
//!
//! Raw plays the file once at its own speed, straight away. Otherwise it waits for the next bar
//! while the transport runs, keeps to the Set's tempo and loops.

use crate::clip::BlockTime;
use crate::{retire, Engine, Garbage};
use rlive_core::time::Quantize;
use rlive_core::warp::{StretchBank, StretchKind, StretchSpan, WarpMode};
use rlive_core::SampleBuffer;
use std::sync::Arc;

/// How a preview that isn't Raw lines up with the Set.
pub struct PreviewSync {
    pub sample: Arc<SampleBuffer>,
    /// The loop's length in ticks.
    pub loop_ticks: f64,
    /// Whether the file fills the loop at the Set's tempo. Otherwise it plays at its own speed
    /// from the top of each loop, as an unwarped clip would.
    pub warped: bool,
    pub mode: WarpMode,
    /// Where Beats slices the source, in frames.
    pub slices: Arc<Vec<f64>>,
}

pub enum PreviewCmd {
    /// Start hearing a sample at once, replacing anything already previewing: Raw.
    Play(Arc<SampleBuffer>),
    /// Start on the next bar, in time.
    Start(PreviewSync),
    /// Jump to this fraction of the file. Raw previews can't be scrubbed.
    Scrub(f64),
    Stop,
}

/// The sample being previewed, and how far it has got.
pub(crate) struct Preview {
    sample: Option<Arc<SampleBuffer>>,
    position: f64,
    sync: Option<Sync>,
    stretch: StretchBank,
}

struct Sync {
    loop_ticks: f64,
    warped: bool,
    mode: WarpMode,
    slices: Arc<Vec<f64>>,
    /// The song tick it starts on, while it's waiting for it.
    wait_until: Option<f64>,
    /// Ticks into the loop.
    phase: f64,
    /// The stretcher has to be told where to start next.
    jump: bool,
    /// How far through the source a tick moves.
    frames_per_tick: f64,
}

impl Preview {
    pub(crate) fn new(max_block: usize, sample_rate: f64) -> Self {
        let mut stretch = rlive_stretch::bank(max_block);
        stretch.prepare(sample_rate);
        Self {
            sample: None,
            position: 0.0,
            sync: None,
            stretch,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.sample.is_some()
    }

    /// How far through the file it is, 0..1, or `None` when nothing is sounding yet.
    fn progress(&self) -> Option<f64> {
        let sample = self.sample.as_ref()?;
        match &self.sync {
            Some(s) if s.wait_until.is_some() => None,
            Some(s) => {
                Some((s.phase * s.frames_per_tick / sample.len().max(1) as f64).clamp(0.0, 1.0))
            }
            None => Some((self.position / sample.len().max(1) as f64).clamp(0.0, 1.0)),
        }
    }

    /// Mix the next frames into `left` and `right`. Returns the sample once it has played out,
    /// to be retired.
    pub(crate) fn render(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        time: &BlockTime,
        playing: bool,
    ) -> Option<Arc<SampleBuffer>> {
        let sample = self.sample.clone()?;
        if self.sync.is_some() {
            self.render_synced(&sample, left, right, time, playing);
            return None;
        }
        self.render_raw(&sample, left, right, time.sample_rate)
    }

    fn render_raw(
        &mut self,
        sample: &Arc<SampleBuffer>,
        left: &mut [f32],
        right: &mut [f32],
        sample_rate: f64,
    ) -> Option<Arc<SampleBuffer>> {
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

    fn render_synced(
        &mut self,
        sample: &Arc<SampleBuffer>,
        left: &mut [f32],
        right: &mut [f32],
        time: &BlockTime,
        playing: bool,
    ) {
        let n = left.len().min(right.len());
        let Some(sync) = self.sync.as_mut() else {
            return;
        };
        let mut from = 0;
        if let Some(at) = sync.wait_until {
            if playing && at >= time.t1() {
                return;
            }
            if playing {
                from = time.offset(at).min(n);
            }
            sync.wait_until = None;
            sync.phase = 0.0;
            sync.jump = true;
        }
        let tps = time.ticks_per_sample.max(f64::MIN_POSITIVE);
        let len = sample.len() as f64;
        let kind = if sync.warped {
            sync.mode.kind()
        } else {
            StretchKind::Off
        };
        let frames_per_tick = if sync.warped {
            len / sync.loop_ticks
        } else {
            sample.sample_rate / time.sample_rate / tps
        };
        sync.frames_per_tick = frames_per_tick;
        while from < n {
            let left_in_loop = sync.loop_ticks - sync.phase;
            let chunk = ((left_in_loop / tps).ceil() as usize).clamp(1, n - from);
            let end_phase = sync.phase + chunk as f64 * tps;
            let span = StretchSpan {
                src_start: sync.phase * frames_per_tick,
                src_end: end_phase * frames_per_tick,
                transpose: 0.0,
                mode: sync.mode,
                slices: sync.slices.as_slice(),
            };
            let (voice, vl, vr) = self.stretch.voice(kind, 0);
            if sync.jump {
                voice.reset();
                voice.seek(span.src_start);
                sync.jump = false;
            }
            voice.render(sample, span, &mut vl[..chunk], &mut vr[..chunk]);
            for (out, v) in left[from..from + chunk].iter_mut().zip(vl.iter()) {
                *out += *v;
            }
            for (out, v) in right[from..from + chunk].iter_mut().zip(vr.iter()) {
                *out += *v;
            }
            from += chunk;
            sync.phase = end_phase;
            if sync.phase >= sync.loop_ticks - 1e-9 {
                sync.phase = 0.0;
                sync.jump = true;
            }
        }
    }
}

impl Engine {
    pub(crate) fn handle_preview(&mut self, cmd: PreviewCmd) {
        let (next, sync) = match cmd {
            PreviewCmd::Play(sample) => (Some(sample), None),
            PreviewCmd::Start(s) => {
                let wait_until = self
                    .playing
                    .then(|| self.maps.bars.next_boundary(self.tick, Quantize::Bar));
                let sync = Sync {
                    loop_ticks: s.loop_ticks.max(1.0),
                    warped: s.warped,
                    mode: s.mode,
                    slices: s.slices,
                    wait_until,
                    phase: 0.0,
                    jump: true,
                    frames_per_tick: 0.0,
                };
                (Some(s.sample), Some(sync))
            }
            PreviewCmd::Scrub(fraction) => {
                if let Some(sync) = &mut self.preview.sync {
                    if sync.warped {
                        sync.phase = fraction.clamp(0.0, 0.999_999) * sync.loop_ticks;
                        sync.wait_until = None;
                        sync.jump = true;
                    }
                }
                return;
            }
            PreviewCmd::Stop => (None, None),
        };
        if let Some(old) = std::mem::replace(&mut self.preview.sample, next) {
            retire(&mut self.garbage, Garbage::Shared(old));
        }
        if let Some(old) = std::mem::replace(&mut self.preview.sync, sync) {
            retire(&mut self.garbage, Garbage::Shared(old.slices));
        }
        self.preview.position = 0.0;
    }

    /// Publishes how far the preview has got, for the waveform in the Preview Tab.
    pub(crate) fn publish_preview(&self) {
        let at = self.preview.progress().map_or(-1.0, |p| p as f32);
        self.shared.preview_position.store(at);
    }
}
