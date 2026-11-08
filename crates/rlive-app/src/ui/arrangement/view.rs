//! How the Arrangement is framed: Follow, and zooming with a way back.

/// How far past the left edge a zoom to a selection leaves.
const ZOOM_MARGIN: f32 = 0.05;

/// The Follow switch, and whether it has paused itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Follow {
    pub on: bool,
    pub paused: bool,
    was_playing: bool,
}

impl Default for Follow {
    fn default() -> Self {
        Self {
            on: true,
            paused: false,
            was_playing: false,
        }
    }
}

impl Follow {
    /// Follow while it's on and hasn't paused, and starts or stops resume it.
    pub fn update(&mut self, playing: bool) -> bool {
        if playing != self.was_playing {
            self.paused = false;
            self.was_playing = playing;
        }
        self.on && !self.paused && playing
    }

    /// An edit, a scroll or a click on the beat-time ruler.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// A click in the Arrangement's or a clip's scrub area.
    pub fn resume(&mut self) {
        self.paused = false;
    }
}

/// The view that shows `[start, end)` filling `width`, with a little room either side.
pub fn zoom_for(start: u64, end: u64, width: f32, min: f32, max: f32) -> (f32, f32) {
    let beats = (end.saturating_sub(start)).max(1) as f32 / rlive_core::PPQ as f32;
    let px = (width * (1.0 - 2.0 * ZOOM_MARGIN) / beats).clamp(min, max);
    let scroll = start as f32 / rlive_core::PPQ as f32 * px - width * ZOOM_MARGIN;
    (px, scroll.max(0.0))
}

/// The zooms Z has done, for X to take back one at a time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ZoomHistory {
    steps: Vec<(f32, f32)>,
}

impl ZoomHistory {
    pub fn push(&mut self, px_per_beat: f32, scroll: f32) {
        self.steps.push((px_per_beat, scroll));
        if self.steps.len() > 64 {
            self.steps.remove(0);
        }
    }

    pub fn pop(&mut self) -> Option<(f32, f32)> {
        self.steps.pop()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// A height for each of `n` tracks so they just fit `available`, within the limits.
pub fn optimize_height(available: f32, n: usize, min: f32, max: f32) -> f32 {
    if n == 0 {
        return min;
    }
    (available / n as f32).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::PPQ;

    #[test]
    fn follow_pauses_and_resumes_on_restart_or_scrub() {
        let mut f = Follow::default();
        assert!(!f.update(false));
        assert!(f.update(true));
        f.pause();
        assert!(!f.update(true), "paused while playing on");
        f.resume();
        assert!(f.update(true));
        f.pause();
        f.update(false);
        assert!(f.update(true), "stopping or restarting resumes");
    }

    #[test]
    fn follow_off_never_follows() {
        let mut f = Follow {
            on: false,
            ..Follow::default()
        };
        assert!(!f.update(true));
    }

    #[test]
    fn zoom_frames_the_selection_with_a_margin() {
        let (px, scroll) = zoom_for(PPQ * 4, PPQ * 8, 1000.0, 1.0, 1000.0);
        assert!((px - 225.0).abs() < 0.01);
        assert!((scroll - (4.0 * px - 50.0)).abs() < 0.01);
    }

    #[test]
    fn zoom_history_steps_back_one_at_a_time() {
        let mut h = ZoomHistory::default();
        assert!(h.is_empty());
        h.push(1.0, 10.0);
        h.push(2.0, 20.0);
        assert_eq!(h.pop(), Some((2.0, 20.0)));
        assert_eq!(h.pop(), Some((1.0, 10.0)));
        assert_eq!(h.pop(), None);
    }

    #[test]
    fn optimize_height_fits_the_tracks_within_limits() {
        assert_eq!(optimize_height(400.0, 4, 20.0, 200.0), 100.0);
        assert_eq!(optimize_height(40.0, 4, 20.0, 200.0), 20.0);
        assert_eq!(optimize_height(4000.0, 4, 20.0, 200.0), 200.0);
    }
}
