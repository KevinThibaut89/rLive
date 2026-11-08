//! Where a clip's markers put playback (manual pp. 208, 229).
//!
//! A clip plays from its start marker. Looped, it runs on to the end of the loop and then round
//! the loop for ever, which lets it run in from a start marker before the loop. Unlooped, it stops
//! at its end marker. A clip's "run" is how far it has played since its start marker, in ticks;
//! [`ClipRegion::content_at`] says what part of the clip's content that is.
//!
//! The engine, the UI's playheads and the edit commands all read this, so they can't disagree.

/// The markers of a clip, in ticks of its content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRegion {
    pub start: f64,
    pub loop_start: f64,
    pub loop_len: f64,
    /// Where an unlooped clip stops. Ignored while looped.
    pub end: f64,
    pub looped: bool,
}

impl ClipRegion {
    /// A clip whose markers are all at their defaults: the whole of `length`, from its start.
    pub fn whole(length: f64, looped: bool) -> Self {
        Self {
            start: 0.0,
            loop_start: 0.0,
            loop_len: length,
            end: length,
            looped,
        }
    }

    /// Markers as the project has them, made safe to play: a looped clip's start marker can't be
    /// past the end of its loop, and an unlooped clip always has something to play.
    pub fn new(start: f64, end: Option<f64>, loop_start: f64, loop_len: f64, looped: bool) -> Self {
        let loop_end = loop_start + loop_len;
        let start = if looped && start >= loop_end {
            loop_start
        } else {
            start
        };
        let end = end.unwrap_or(loop_end);
        let end = if end > start { end } else { start + 1.0 };
        Self {
            start,
            loop_start,
            loop_len,
            end,
            looped,
        }
    }

    /// Whether every marker is where a new clip has it.
    pub fn is_whole(&self) -> bool {
        self.start == 0.0 && self.loop_start == 0.0 && self.end == self.loop_len
    }

    pub fn loop_end(&self) -> f64 {
        self.loop_start + self.loop_len
    }

    /// The content position where this clip stops being what it plays: the end of the loop, or
    /// its end marker when unlooped.
    pub fn content_end(&self) -> f64 {
        if self.looped {
            self.loop_end()
        } else {
            self.end
        }
    }

    /// How far the clip plays before coming to the end of its region. Infinite while looped.
    pub fn play_length(&self) -> f64 {
        if self.looped {
            f64::INFINITY
        } else {
            self.end - self.start
        }
    }

    /// How long before the first full pass of the loop the start marker is. Negative for a start
    /// marker inside the loop.
    pub fn lead(&self) -> f64 {
        self.loop_start - self.start
    }

    /// The content position `run` ticks after the start marker.
    pub fn content_at(&self, run: f64) -> f64 {
        if !self.looped {
            return self.start + run.max(0.0);
        }
        let lead = self.lead();
        if lead > 0.0 && run < lead {
            self.start + run
        } else {
            self.loop_start + (run - lead).rem_euclid(self.loop_len)
        }
    }

    /// The run that reaches content position `content`, if playback ever gets there.
    pub fn run_of(&self, content: f64) -> Option<f64> {
        if !self.looped {
            return (self.start..self.end)
                .contains(&content)
                .then_some(content - self.start);
        }
        if content >= self.start && content < self.loop_end() {
            return Some(content - self.start);
        }
        (content >= self.loop_start && content < self.loop_end())
            .then_some(self.lead() + self.loop_len + (content - self.loop_start))
    }

    /// This region with playback departing from `start` instead. A start marker past the loop
    /// lands where that position falls in the loop.
    pub fn from_position(self, start: f64) -> Self {
        let start = if self.looped && start >= self.loop_end() && self.loop_len > 0.0 {
            self.loop_start + (start - self.loop_start).rem_euclid(self.loop_len)
        } else {
            start
        };
        Self { start, ..self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn the_default_region_reads_exactly_as_the_old_formulas_did() {
        for looped in [true, false] {
            let r = ClipRegion::whole(960.0, looped);
            assert!(r.is_whole());
            for x in [0.0, 1.0, 0.1 + 0.2, 959.999, 960.0, 1234.567, 8000.25] {
                let old = if looped {
                    f64::rem_euclid(x, 960.0)
                } else {
                    f64::max(x, 0.0)
                };
                assert_eq!(r.content_at(x).to_bits(), old.to_bits(), "{x}");
            }
        }
    }

    #[test]
    fn a_clip_runs_in_from_before_its_loop_and_then_stays_in_it() {
        let r = ClipRegion::new(0.0, None, 960.0, 480.0, true);
        close(r.content_at(100.0), 100.0);
        close(r.content_at(960.0), 960.0);
        close(r.content_at(1439.0), 1439.0);
        close(r.content_at(1440.0), 960.0);
        close(r.content_at(1440.0 + 480.0 * 3.0 + 7.0), 967.0);
    }

    #[test]
    fn a_start_marker_inside_the_loop_plays_a_short_first_pass() {
        let r = ClipRegion::new(1200.0, None, 960.0, 480.0, true);
        close(r.content_at(0.0), 1200.0);
        close(r.content_at(239.0), 1439.0);
        close(r.content_at(240.0), 960.0);
    }

    #[test]
    fn a_start_marker_past_the_loop_starts_at_the_loop() {
        let r = ClipRegion::new(5000.0, None, 0.0, 480.0, true);
        close(r.start, 0.0);
    }

    #[test]
    fn an_unlooped_clip_plays_between_its_markers() {
        let r = ClipRegion::new(480.0, Some(960.0), 0.0, 4000.0, false);
        close(r.play_length(), 480.0);
        close(r.content_at(100.0), 580.0);
        assert!(!r.is_whole());
    }

    #[test]
    fn run_of_inverts_content_at_wherever_playback_gets_to() {
        let r = ClipRegion::new(100.0, None, 960.0, 480.0, true);
        for content in [100.0, 500.0, 960.0, 1200.0, 1439.0] {
            let run = r.run_of(content).unwrap();
            close(r.content_at(run), content);
        }
        assert_eq!(r.run_of(50.0), None, "before the start marker");
        assert_eq!(r.run_of(2000.0), None, "past the loop");
    }

    #[test]
    fn starting_somewhere_else_keeps_the_loop() {
        let r = ClipRegion::new(0.0, None, 960.0, 480.0, true).from_position(1100.0);
        close(r.content_at(0.0), 1100.0);
        close(r.content_at(340.0), 960.0);
        let wrapped = ClipRegion::new(0.0, None, 960.0, 480.0, true).from_position(1500.0);
        close(wrapped.start, 1020.0);
    }
}
