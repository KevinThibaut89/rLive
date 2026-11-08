//! Delay lines for latency compensation: stereo audio held back by a number of frames, in rings
//! allocated on the UI thread, whose delay changes with a short crossfade rather than a click.

/// The longest delay a compensation line provides, in frames.
pub const MAX_COMPENSATION_FRAMES: u32 = 1 << 20;

/// How long a change of delay crossfades for, in frames.
pub const FADE_FRAMES: u32 = 512;

/// A ring of the most recent stereo audio. Allocated on the UI thread; written and read on the
/// audio thread without allocating.
pub struct StereoLine {
    left: Box<[f32]>,
    right: Box<[f32]>,
    mask: usize,
    /// One past the newest frame.
    head: usize,
    /// The longest delay it can be read at.
    max_delay: u32,
}

impl StereoLine {
    /// A line that can hold back `delay` frames behind blocks of up to `max_block` frames.
    pub fn new(delay: u32, max_block: usize) -> Box<Self> {
        let len = (delay as usize + max_block).next_power_of_two();
        Box::new(Self {
            left: vec![0.0; len].into_boxed_slice(),
            right: vec![0.0; len].into_boxed_slice(),
            mask: len - 1,
            head: 0,
            max_delay: (len - max_block) as u32,
        })
    }

    /// The longest delay this line can be read at.
    pub fn max_delay(&self) -> u32 {
        self.max_delay
    }

    /// Append a block. A value that blew up is not held on to.
    pub(crate) fn push(&mut self, left: &[f32], right: &[f32]) {
        let clean = |s: f32| if s.is_finite() { s } else { 0.0 };
        for (i, (&l, &r)) in left.iter().zip(right).enumerate() {
            let at = (self.head + i) & self.mask;
            self.left[at] = clean(l);
            self.right[at] = clean(r);
        }
        self.head = (self.head + left.len()) & self.mask;
    }

    /// The frame `i` of a block of `frames` read `delay` behind the newest audio.
    #[inline]
    fn frame(&self, delay: u32, frames: usize, i: usize) -> (f32, f32) {
        let at = self
            .head
            .wrapping_sub(frames + delay as usize)
            .wrapping_add(i)
            & self.mask;
        (self.left[at], self.right[at])
    }

    /// Take over what an older, smaller line remembers, so growing a line loses nothing.
    pub(crate) fn inherit(&mut self, old: &StereoLine) {
        let n = old.left.len().min(self.left.len());
        for i in 0..n {
            let (l, r) = old.frame(0, n, i);
            self.left[i] = l;
            self.right[i] = r;
        }
        self.head = n & self.mask;
    }
}

/// How far behind a [`StereoLine`] a reader is, moving to a new delay with a crossfade.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FadingDelay {
    /// The delay the fade is heading to.
    current: u32,
    /// The delay to move to next.
    target: u32,
    /// The delay being faded out, while `left` is above 0.
    from: u32,
    /// Frames left in the fade.
    left: u32,
}

impl FadingDelay {
    /// Move to a new delay. A fade in progress finishes first, so two changes close together
    /// never click.
    pub(crate) fn set_target(&mut self, delay: u32) {
        self.target = delay;
    }

    /// Be at a delay right away, with nothing to fade from.
    pub(crate) fn snap_to(&mut self, delay: u32) {
        *self = Self {
            current: delay,
            target: delay,
            from: delay,
            left: 0,
        };
    }

    /// Reading the line would give back exactly what was written.
    pub(crate) fn is_straight(&self) -> bool {
        self.current == 0 && self.target == 0 && self.left == 0
    }

    /// Fill `left` and `right` with what `line` held `frames` ago. Call once per block, after
    /// the block has been pushed.
    pub(crate) fn read(&mut self, line: &StereoLine, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len();
        if self.left == 0 && self.current != self.target {
            self.from = self.current;
            self.current = self.target;
            self.left = FADE_FRAMES;
        }
        for i in 0..frames {
            let (mut l, mut r) = line.frame(self.current, frames, i);
            if self.left > 0 {
                let (ol, or) = line.frame(self.from, frames, i);
                let old = self.left as f32 / FADE_FRAMES as f32;
                l = l * (1.0 - old) + ol * old;
                r = r * (1.0 - old) + or * old;
                self.left -= 1;
            }
            left[i] = l;
            right[i] = r;
        }
    }
}

/// Replace the line in `slot` with `new` if that gives more room, keeping what it remembers.
/// Returns the one that lost, to be retired.
pub(crate) fn offer_line(
    slot: &mut Option<Box<StereoLine>>,
    new: Option<Box<StereoLine>>,
) -> Option<Box<StereoLine>> {
    let mut new = new?;
    match slot {
        Some(old) if old.max_delay() >= new.max_delay() => Some(new),
        Some(old) => {
            new.inherit(old);
            std::mem::swap(old, &mut new);
            Some(new)
        }
        None => {
            *slot = Some(new);
            None
        }
    }
}

/// The lines a track can be heard through later than it renders: its main output, its send before
/// the mixer, and its send after it. Only what the plan uses is allocated.
#[derive(Default)]
pub struct TapLines {
    pub main: Option<Box<StereoLine>>,
    pub pre: Option<Box<StereoLine>>,
    pub post: Option<Box<StereoLine>>,
}

impl TapLines {
    /// Any line to install at all.
    pub fn is_empty(&self) -> bool {
        self.main.is_none() && self.pre.is_none() && self.post.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 64;

    fn line(delay: u32) -> Box<StereoLine> {
        StereoLine::new(delay, BLOCK)
    }

    /// Push `blocks` blocks of a counter, read through `delay`, and give back what came out.
    fn run(delay: &mut FadingDelay, line: &mut StereoLine, blocks: usize, start: f32) -> Vec<f32> {
        let mut out = Vec::new();
        let mut count = start;
        for _ in 0..blocks {
            let block: Vec<f32> = (0..BLOCK)
                .map(|_| {
                    count += 1.0;
                    count
                })
                .collect();
            line.push(&block, &block);
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            delay.read(line, &mut l, &mut r);
            assert_eq!(l, r, "both channels read the same way");
            out.extend(l);
        }
        out
    }

    #[test]
    fn a_line_delays_by_exactly_what_it_is_asked_to() {
        let mut d = FadingDelay::default();
        d.snap_to(100);
        let mut l = line(100);
        let out = run(&mut d, &mut l, 8, 0.0);
        // A counter: frame n reads n + 1, so holding it back 100 frames starts it at -99.
        for (i, s) in out.iter().enumerate().skip(100) {
            assert_eq!(*s, (i + 1 - 100) as f32, "frame {i}");
        }
    }

    #[test]
    fn no_delay_gives_back_the_block_just_written() {
        let mut d = FadingDelay::default();
        d.snap_to(0);
        let mut l = line(0);
        assert_eq!(
            run(&mut d, &mut l, 3, 0.0),
            (1..=192).map(|n| n as f32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_change_of_delay_crossfades_rather_than_jumps() {
        let mut d = FadingDelay::default();
        d.snap_to(0);
        let mut l = line(64);
        // A steady signal: fading between two reads of it leaves it alone.
        for _ in 0..32 {
            let v = vec![0.5; BLOCK];
            l.push(&v, &v);
        }
        d.set_target(64);
        let (mut out_l, mut out_r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        l.push(&[0.5; BLOCK], &[0.5; BLOCK]);
        d.read(&l, &mut out_l, &mut out_r);
        assert!(out_l.iter().all(|s| (s - 0.5).abs() < 1e-6));
        assert!(!d.is_straight(), "still fading");
    }

    #[test]
    fn a_fade_finishes_before_the_next_begins() {
        let mut d = FadingDelay::default();
        d.snap_to(10);
        d.set_target(20);
        let mut l = line(64);
        run(&mut d, &mut l, 1, 0.0);
        d.set_target(30);
        assert_eq!(d.current, 20, "the first target stands while it fades in");
        assert_eq!(d.target, 30);
        run(&mut d, &mut l, 40, 0.0);
        assert_eq!(d.current, 30);
        assert!(d.left == 0);
    }

    #[test]
    fn a_silly_delay_never_reads_outside_the_line() {
        let mut d = FadingDelay::default();
        d.snap_to(u32::MAX - 7);
        let mut l = line(8);
        assert_eq!(run(&mut d, &mut l, 4, 0.0).len(), BLOCK * 4);
    }

    #[test]
    fn a_grown_line_remembers_what_the_old_one_held() {
        let mut old = line(0);
        let mut d = FadingDelay::default();
        d.snap_to(0);
        run(&mut d, &mut old, 2, 0.0);
        let mut grown = line(1000);
        grown.inherit(&old);
        d.snap_to(0);
        let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        d.read(&grown, &mut l, &mut r);
        let first = BLOCK as f32 + 1.0;
        assert_eq!(l[0], first, "the oldest frame the small line still had");
        assert_eq!(l[BLOCK - 1], first + BLOCK as f32 - 1.0);
    }

    #[test]
    fn a_bigger_line_replaces_a_smaller_one_and_a_smaller_one_is_refused() {
        let mut slot = Some(line(10));
        assert!(offer_line(&mut slot, None).is_none());
        let retired = offer_line(&mut slot, Some(line(4000))).expect("the old one retires");
        assert!(slot.as_ref().unwrap().max_delay() >= 4000);
        assert!(retired.max_delay() < 4000);
        assert!(
            offer_line(&mut slot, Some(line(0))).is_some(),
            "a smaller one is sent back"
        );
        assert!(slot.as_ref().unwrap().max_delay() >= 4000);
    }

    #[test]
    fn nothing_blown_up_is_remembered() {
        let mut l = line(0);
        l.push(&[f32::NAN; 4], &[f32::INFINITY; 4]);
        assert_eq!(l.frame(0, 4, 0), (0.0, 0.0));
    }
}
