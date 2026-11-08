use serde::{Deserialize, Serialize};

/// Ticks per quarter note. All musical positions are expressed in ticks.
pub const PPQ: u64 = 960;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

impl TimeSignature {
    /// Length of one beat (the denominator note value) in ticks.
    pub fn ticks_per_beat(&self) -> u64 {
        PPQ * 4 / self.denominator.max(1) as u64
    }

    pub fn ticks_per_bar(&self) -> u64 {
        self.ticks_per_beat() * self.numerator.max(1) as u64
    }

    /// Tick of the start of the bar `tick` falls in.
    pub fn bar_start(&self, tick: f64) -> f64 {
        let bar = self.ticks_per_bar() as f64;
        (tick.max(0.0) / bar).floor() * bar
    }

    /// Format a tick position as `bar.beat.sixteenth` (1-based, like Ableton).
    pub fn format_position(&self, ticks: f64) -> String {
        let ticks = ticks.max(0.0) as u64;
        let bar = ticks / self.ticks_per_bar() + 1;
        let in_bar = ticks % self.ticks_per_bar();
        let beat = in_bar / self.ticks_per_beat() + 1;
        let sixteenth = (in_bar % self.ticks_per_beat()) / (PPQ / 4) + 1;
        format!("{bar}.{beat}.{sixteenth}")
    }
}

/// Global launch quantization for clips and scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Quantize {
    None,
    Sixteenth,
    Eighth,
    Beat,
    HalfBar,
    #[default]
    Bar,
    TwoBars,
    FourBars,
}

impl Quantize {
    pub const ALL: [Quantize; 8] = [
        Quantize::None,
        Quantize::Sixteenth,
        Quantize::Eighth,
        Quantize::Beat,
        Quantize::HalfBar,
        Quantize::Bar,
        Quantize::TwoBars,
        Quantize::FourBars,
    ];

    /// Quantum in ticks (0 = launch immediately).
    pub fn ticks(&self, sig: TimeSignature) -> u64 {
        match self {
            Quantize::None => 0,
            Quantize::Sixteenth => PPQ / 4,
            Quantize::Eighth => PPQ / 2,
            Quantize::Beat => sig.ticks_per_beat(),
            Quantize::HalfBar => sig.ticks_per_bar() / 2,
            Quantize::Bar => sig.ticks_per_bar(),
            Quantize::TwoBars => sig.ticks_per_bar() * 2,
            Quantize::FourBars => sig.ticks_per_bar() * 4,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Quantize::None => "None",
            Quantize::Sixteenth => "1/16",
            Quantize::Eighth => "1/8",
            Quantize::Beat => "Beat",
            Quantize::HalfBar => "1/2 Bar",
            Quantize::Bar => "1 Bar",
            Quantize::TwoBars => "2 Bars",
            Quantize::FourBars => "4 Bars",
        }
    }

    /// Next launch point at or after `tick`, for one signature with bars from tick 0. A song with
    /// signature changes needs [`BarMap::next_boundary`].
    pub fn next_boundary(&self, tick: f64, sig: TimeSignature) -> f64 {
        let q = self.ticks(sig) as f64;
        if q <= 0.0 {
            return tick;
        }
        // Tolerate tiny float error so a launch exactly on a boundary is not delayed a whole bar.
        let n = ((tick - 1e-6) / q).ceil().max(0.0);
        n * q
    }
}

/// Transport snapshot handed to every processor for the current block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportInfo {
    pub playing: bool,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    /// Song position at the start of the block, in ticks.
    pub tick: f64,
    /// Song position at the start of the block, in samples since play was pressed.
    pub sample_pos: u64,
    pub sample_rate: f64,
    /// Tick of the start of the bar the block starts in.
    pub bar_start: f64,
    /// The Arrangement loop as (start, end) ticks, when it is on.
    pub loop_range: Option<(f64, f64)>,
    /// The current clip's scale, for devices with Use Current Scale. `None` when there is none
    /// or nothing is delivering one; those devices then work in semitones.
    pub scale: Option<ScaleInfo>,
}

/// A scale as it reaches devices: small and `Copy`, so it can ride in [`TransportInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaleInfo {
    /// Root pitch class, 0 (C) to 11.
    pub root: u8,
    /// Bit `n` is set when the note `n` semitones above the root is in the scale.
    pub mask: u16,
}

impl TransportInfo {
    pub fn ticks_per_sample(&self) -> f64 {
        ticks_per_sample(self.tempo, self.sample_rate)
    }

    /// Song position in quarter notes (beats in x/4).
    pub fn beats(&self) -> f64 {
        self.tick / PPQ as f64
    }
}

impl Default for TransportInfo {
    fn default() -> Self {
        Self {
            playing: false,
            tempo: 120.0,
            time_signature: TimeSignature::default(),
            tick: 0.0,
            sample_pos: 0,
            sample_rate: 48_000.0,
            bar_start: 0.0,
            loop_range: None,
            scale: None,
        }
    }
}

#[inline]
pub fn ticks_per_sample(tempo: f64, sample_rate: f64) -> f64 {
    tempo / 60.0 * PPQ as f64 / sample_rate
}

/// The tempo range the engine accepts, in BPM.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 999.0;

#[inline]
fn ticks_per_second(bpm: f64) -> f64 {
    bpm / 60.0 * PPQ as f64
}

/// A tempo at a song position. Tempo ramps linearly in ticks to the next point; two points at
/// the same tick make a step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoPoint {
    pub tick: f64,
    pub bpm: f64,
}

impl TempoPoint {
    pub fn new(tick: f64, bpm: f64) -> Self {
        Self { tick, bpm }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct TempoSeg {
    tick: f64,
    bpm: f64,
    /// BPM per tick. Zero holds `bpm`.
    slope: f64,
    /// Seconds from tick 0 to `tick`.
    seconds: f64,
}

impl TempoSeg {
    /// Seconds to cover `dt` ticks from the start of the segment.
    fn seconds_over(&self, dt: f64) -> f64 {
        if self.slope == 0.0 {
            dt / ticks_per_second(self.bpm)
        } else {
            (self.slope * dt / self.bpm).ln_1p() * 60.0 / (PPQ as f64 * self.slope)
        }
    }

    /// Ticks covered in `ds` seconds from the start of the segment.
    fn ticks_over(&self, ds: f64) -> f64 {
        if self.slope == 0.0 {
            ds * ticks_per_second(self.bpm)
        } else {
            self.bpm * (ds * self.slope * PPQ as f64 / 60.0).exp_m1() / self.slope
        }
    }
}

/// How the timeline moves from one song position until the next tempo point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoRun {
    /// Ticks per sample at the position.
    pub ticks_per_sample: f64,
    /// How fast `ticks_per_sample` grows, per tick. Zero while the tempo holds.
    pub ramp: f64,
    /// Tick of the next tempo point, infinite after the last one.
    pub until: f64,
}

impl TempoRun {
    /// Ticks covered by `frames` samples from the position.
    #[inline]
    pub fn advance(&self, frames: f64) -> f64 {
        if self.ramp == 0.0 {
            frames * self.ticks_per_sample
        } else {
            self.ticks_per_sample * (self.ramp * frames).exp_m1() / self.ramp
        }
    }

    /// Samples it takes to cover `ticks` from the position. Exact only up to `until`.
    #[inline]
    pub fn frames_to(&self, ticks: f64) -> f64 {
        if self.ramp == 0.0 {
            ticks / self.ticks_per_sample
        } else {
            let growth = (self.ramp * ticks / self.ticks_per_sample).max(-0.999_999);
            growth.ln_1p() / self.ramp
        }
    }
}

/// Tempo over the song. Ticks stay the canonical position; this says how they relate to time.
///
/// Before the first point the tempo holds at its value, and after the last point at its value.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    segs: Vec<TempoSeg>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(120.0)
    }
}

impl TempoMap {
    pub fn constant(bpm: f64) -> Self {
        Self {
            segs: vec![TempoSeg {
                tick: 0.0,
                bpm: bpm.clamp(MIN_BPM, MAX_BPM),
                slope: 0.0,
                seconds: 0.0,
            }],
        }
    }

    /// Build a map from points in any order. Points before tick 0 count as at tick 0, and with
    /// no usable point the tempo is 120 BPM.
    pub fn new(points: impl IntoIterator<Item = TempoPoint>) -> Self {
        let mut pts: Vec<TempoPoint> = points
            .into_iter()
            .filter(|p| p.tick.is_finite() && p.bpm.is_finite())
            .map(|p| TempoPoint {
                tick: p.tick.max(0.0),
                bpm: p.bpm.clamp(MIN_BPM, MAX_BPM),
            })
            .collect();
        if pts.is_empty() {
            return Self::default();
        }
        pts.sort_by(|a, b| a.tick.total_cmp(&b.tick));
        if pts[0].tick > 0.0 {
            pts.insert(0, TempoPoint::new(0.0, pts[0].bpm));
        }
        let mut segs = Vec::with_capacity(pts.len());
        let mut seconds = 0.0;
        for (i, p) in pts.iter().enumerate() {
            let Some(q) = pts.get(i + 1) else {
                segs.push(TempoSeg {
                    tick: p.tick,
                    bpm: p.bpm,
                    slope: 0.0,
                    seconds,
                });
                break;
            };
            let dt = q.tick - p.tick;
            // The earlier of two points at one tick only ends the ramp arriving at it.
            if dt <= 0.0 {
                continue;
            }
            let seg = TempoSeg {
                tick: p.tick,
                bpm: p.bpm,
                slope: (q.bpm - p.bpm) / dt,
                seconds,
            };
            seconds += seg.seconds_over(dt);
            segs.push(seg);
        }
        Self { segs }
    }

    /// One tempo for the whole song, with no ramps or steps.
    pub fn is_constant(&self) -> bool {
        self.segs.len() == 1 && self.segs[0].slope == 0.0
    }

    /// Replace the tempo of a constant map, in place. A map with points keeps them and returns
    /// `false`: the tempo envelope wins over the tempo field.
    pub fn set_constant(&mut self, bpm: f64) -> bool {
        let constant = self.is_constant();
        if constant {
            self.segs[0].bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        }
        constant
    }

    fn index_at(&self, tick: f64) -> usize {
        self.segs
            .partition_point(|s| s.tick <= tick)
            .saturating_sub(1)
    }

    pub fn bpm_at(&self, tick: f64) -> f64 {
        let seg = &self.segs[self.index_at(tick)];
        if tick < seg.tick {
            seg.bpm
        } else {
            seg.bpm + seg.slope * (tick - seg.tick)
        }
    }

    pub fn tick_to_seconds(&self, tick: f64) -> f64 {
        let seg = &self.segs[self.index_at(tick)];
        if tick < seg.tick {
            // Only before the first segment, where the tempo holds.
            return seg.seconds + (tick - seg.tick) / ticks_per_second(seg.bpm);
        }
        seg.seconds + seg.seconds_over(tick - seg.tick)
    }

    pub fn seconds_to_tick(&self, seconds: f64) -> f64 {
        let i = self
            .segs
            .partition_point(|s| s.seconds <= seconds)
            .saturating_sub(1);
        let seg = &self.segs[i];
        if seconds < seg.seconds {
            return seg.tick + (seconds - seg.seconds) * ticks_per_second(seg.bpm);
        }
        seg.tick + seg.ticks_over(seconds - seg.seconds)
    }

    /// How time runs from `tick`, at `sample_rate`.
    pub fn run_at(&self, tick: f64, sample_rate: f64) -> TempoRun {
        let i = self.index_at(tick);
        let seg = &self.segs[i];
        if tick < seg.tick {
            // Before the first segment, where the tempo holds until the segment starts.
            return TempoRun {
                ticks_per_sample: ticks_per_sample(seg.bpm, sample_rate),
                ramp: 0.0,
                until: seg.tick,
            };
        }
        TempoRun {
            ticks_per_sample: ticks_per_sample(self.bpm_at(tick), sample_rate),
            ramp: ticks_per_sample(seg.slope, sample_rate),
            until: self.segs.get(i + 1).map_or(f64::INFINITY, |s| s.tick),
        }
    }
}

/// One bar on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    /// Zero-based bar number.
    pub index: u64,
    pub start: f64,
    /// Where the bar ends: its full length, or earlier for a fragmentary bar.
    pub end: f64,
    pub signature: TimeSignature,
}

impl Bar {
    /// Whether a signature change cut this bar short.
    pub fn is_fragment(&self) -> bool {
        self.end - self.start < self.signature.ticks_per_bar() as f64
    }
}

/// A beat on the timeline, for walking the metronome grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    pub tick: f64,
    pub downbeat: bool,
    seg: u32,
    n: u64,
}

impl Beat {
    /// A beat placed by hand, for clicks that don't follow the denominator's grid.
    pub fn at(tick: f64, downbeat: bool) -> Self {
        Self {
            tick,
            downbeat,
            seg: 0,
            n: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BarSeg {
    tick: f64,
    signature: TimeSignature,
    first_bar: u64,
}

/// Meter over the song. A signature change restarts the bar grid at its tick, so a change placed
/// mid-bar leaves a fragmentary bar before it.
#[derive(Debug, Clone, PartialEq)]
pub struct BarMap {
    segs: Vec<BarSeg>,
    /// A marker sits at tick 0, so the base signature is never heard.
    zero_marker: bool,
}

impl Default for BarMap {
    fn default() -> Self {
        Self::constant(TimeSignature::default())
    }
}

impl BarMap {
    pub fn constant(signature: TimeSignature) -> Self {
        Self::new(signature, [])
    }

    /// Build a map from `base`, in force until the first marker, and signature changes in any
    /// order. Of two changes at one tick the later in the list wins.
    pub fn new(
        base: TimeSignature,
        markers: impl IntoIterator<Item = (u64, TimeSignature)>,
    ) -> Self {
        let mut marks: Vec<(u64, TimeSignature)> = markers.into_iter().collect();
        marks.sort_by_key(|m| m.0);
        let mut segs = vec![BarSeg {
            tick: 0.0,
            signature: base,
            first_bar: 0,
        }];
        let mut zero_marker = false;
        for (tick, signature) in marks {
            let tick = tick as f64;
            let last = segs.len() - 1;
            if tick == segs[last].tick {
                segs[last].signature = signature;
                zero_marker |= tick == 0.0;
            } else {
                segs.push(BarSeg {
                    tick,
                    signature,
                    first_bar: 0,
                });
            }
        }
        let mut map = Self { segs, zero_marker };
        map.number_bars();
        map
    }

    fn number_bars(&mut self) {
        for i in 1..self.segs.len() {
            let prev = self.segs[i - 1];
            let bars = (self.segs[i].tick - prev.tick) / prev.signature.ticks_per_bar() as f64;
            self.segs[i].first_bar = prev.first_bar + bars.ceil() as u64;
        }
    }

    /// Replace the signature in force before the first marker, in place. No effect when a marker
    /// sits at tick 0.
    pub fn set_base(&mut self, signature: TimeSignature) {
        if !self.zero_marker {
            self.segs[0].signature = signature;
            self.number_bars();
        }
    }

    fn index_at(&self, tick: f64) -> usize {
        self.segs
            .partition_point(|s| s.tick <= tick)
            .saturating_sub(1)
    }

    fn next_marker(&self, index: usize) -> Option<f64> {
        self.segs.get(index + 1).map(|s| s.tick)
    }

    pub fn signature_at(&self, tick: f64) -> TimeSignature {
        self.segs[self.index_at(tick)].signature
    }

    /// The nominal bar length at `tick`. [`bar_at`](Self::bar_at) has the real one, which may be
    /// cut short.
    pub fn ticks_per_bar_at(&self, tick: f64) -> u64 {
        self.signature_at(tick).ticks_per_bar()
    }

    /// The bar `tick` falls in. Before the first bar that is the first bar.
    pub fn bar_at(&self, tick: f64) -> Bar {
        let i = self.index_at(tick);
        let seg = &self.segs[i];
        let len = seg.signature.ticks_per_bar() as f64;
        let n = ((tick.max(0.0) - seg.tick) / len).floor();
        let start = seg.tick + n * len;
        let end = start + len;
        Bar {
            index: seg.first_bar + n as u64,
            start,
            end: self.next_marker(i).map_or(end, |m| end.min(m)),
            signature: seg.signature,
        }
    }

    /// Next launch point at or after `tick`. The grid restarts at each signature change, which
    /// is itself a boundary.
    pub fn next_boundary(&self, tick: f64, q: Quantize) -> f64 {
        let i = self.index_at(tick);
        let seg = &self.segs[i];
        let quantum = q.ticks(seg.signature) as f64;
        if quantum <= 0.0 {
            return tick;
        }
        // Tolerate tiny float error so a launch exactly on a boundary is not delayed a whole bar.
        let n = ((tick - seg.tick - 1e-6) / quantum).ceil().max(0.0);
        let boundary = seg.tick + n * quantum;
        self.next_marker(i).map_or(boundary, |m| boundary.min(m))
    }

    /// The first beat at or after `tick`.
    pub fn first_beat(&self, tick: f64) -> Beat {
        let i = self.index_at(tick);
        let seg = &self.segs[i];
        let beat = seg.signature.ticks_per_beat() as f64;
        let n = ((tick - seg.tick) / beat).ceil().max(0.0) as u64;
        self.beat(i, n)
    }

    /// The beat after `beat`.
    pub fn next_beat(&self, beat: Beat) -> Beat {
        self.beat(beat.seg as usize, beat.n + 1)
    }

    fn beat(&self, index: usize, n: u64) -> Beat {
        let seg = &self.segs[index];
        let tick = seg.tick + n as f64 * seg.signature.ticks_per_beat() as f64;
        match self.next_marker(index) {
            Some(next) if tick >= next => self.beat(index + 1, 0),
            _ => Beat {
                tick,
                downbeat: n.is_multiple_of(seg.signature.numerator.max(1) as u64),
                seg: index as u32,
                n,
            },
        }
    }
}

/// The song's tempo and meter, swapped together on the audio thread.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TimeMaps {
    pub tempo: TempoMap,
    pub bars: BarMap,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_boundaries() {
        let sig = TimeSignature::default();
        assert_eq!(Quantize::Bar.next_boundary(0.0, sig), 0.0);
        assert_eq!(Quantize::Bar.next_boundary(1.0, sig), 3840.0);
        assert_eq!(Quantize::Bar.next_boundary(3840.0, sig), 3840.0);
        assert_eq!(Quantize::Beat.next_boundary(961.0, sig), 1920.0);
        assert_eq!(Quantize::None.next_boundary(123.4, sig), 123.4);
    }

    #[test]
    fn position_format() {
        let sig = TimeSignature::default();
        assert_eq!(sig.format_position(0.0), "1.1.1");
        assert_eq!(sig.format_position(3840.0 + 960.0 + 240.0), "2.2.2");
    }

    #[test]
    fn three_four() {
        let sig = TimeSignature {
            numerator: 3,
            denominator: 4,
        };
        assert_eq!(sig.ticks_per_bar(), 2880);
        let six_eight = TimeSignature {
            numerator: 6,
            denominator: 8,
        };
        assert_eq!(six_eight.ticks_per_bar(), 2880);
    }

    #[test]
    fn bar_start_finds_the_bar_a_tick_is_in() {
        let sig = TimeSignature::default();
        assert_eq!(sig.bar_start(0.0), 0.0);
        assert_eq!(sig.bar_start(3839.9), 0.0);
        assert_eq!(sig.bar_start(3840.0), 3840.0);
        assert_eq!(sig.bar_start(5000.0), 3840.0);
        assert_eq!(sig.bar_start(-10.0), 0.0);
    }

    fn close(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() <= eps, "{a} is not within {eps} of {b}");
    }

    fn sig(numerator: u8, denominator: u8) -> TimeSignature {
        TimeSignature {
            numerator,
            denominator,
        }
    }

    fn bpm(tick: u64, bpm: f64) -> TempoPoint {
        TempoPoint::new(tick as f64, bpm)
    }

    fn ramped() -> TempoMap {
        TempoMap::new([
            bpm(0, 100.0),
            bpm(1920, 100.0),
            bpm(5760, 200.0),
            bpm(5760, 90.0),
            bpm(9600, 90.0),
        ])
    }

    #[test]
    fn a_constant_map_matches_the_single_tempo_formulas() {
        let map = TempoMap::constant(123.0);
        assert!(map.is_constant());
        let run = map.run_at(4321.5, 48_000.0);
        assert_eq!(run.ticks_per_sample, ticks_per_sample(123.0, 48_000.0));
        assert_eq!(run.ramp, 0.0);
        assert_eq!(run.until, f64::INFINITY);
        assert_eq!(map.bpm_at(9.0), 123.0);
        close(map.tick_to_seconds(960.0), 60.0 / 123.0, 1e-12);
        close(map.seconds_to_tick(1.0), 123.0 / 60.0 * 960.0, 1e-9);
    }

    #[test]
    fn tempo_is_clamped_and_set_in_place_only_when_constant() {
        let mut map = TempoMap::constant(5000.0);
        assert_eq!(map.bpm_at(0.0), MAX_BPM);
        assert!(map.set_constant(90.0));
        assert_eq!(map.bpm_at(100.0), 90.0);
        let mut ramp = ramped();
        assert!(!ramp.set_constant(77.0));
        assert_eq!(ramp.bpm_at(0.0), 100.0);
    }

    #[test]
    fn a_step_takes_the_new_tempo_at_its_tick() {
        let map = ramped();
        close(map.bpm_at(5759.999), 200.0, 1e-2);
        assert_eq!(map.bpm_at(5760.0), 90.0);
        assert_eq!(map.bpm_at(6000.0), 90.0);
        assert_eq!(map.bpm_at(100_000.0), 90.0);
    }

    #[test]
    fn the_first_point_holds_before_it_and_order_does_not_matter() {
        let map = TempoMap::new([bpm(1920, 140.0), bpm(3840, 70.0)]);
        assert_eq!(map.bpm_at(0.0), 140.0);
        assert_eq!(map.bpm_at(1920.0), 140.0);
        close(map.bpm_at(2880.0), 105.0, 1e-9);
        assert_eq!(
            map,
            TempoMap::new([bpm(3840, 70.0), bpm(1920, 140.0)]),
            "sorted"
        );
    }

    #[test]
    fn a_ramp_takes_the_integral_of_its_tempo() {
        let map = TempoMap::new([bpm(0, 100.0), bpm(3840, 200.0)]);
        // Over 4 beats, tempo linear in ticks: t = 60 / (PPQ * k) * ln(b1 / b0) * PPQ, k in BPM
        // per beat is 25.
        let expected = 60.0 / 25.0 * 2f64.ln();
        close(map.tick_to_seconds(3840.0), expected, 1e-12);
    }

    #[test]
    fn seconds_round_trip_through_ramps_and_steps() {
        let map = ramped();
        for i in 0..4000 {
            let tick = i as f64 * 3.1;
            close(map.seconds_to_tick(map.tick_to_seconds(tick)), tick, 1e-6);
        }
        for i in 0..2000 {
            let s = i as f64 * 0.003;
            close(map.tick_to_seconds(map.seconds_to_tick(s)), s, 1e-9);
        }
    }

    #[test]
    fn time_is_strictly_increasing_and_continuous() {
        let map = ramped();
        let mut last = -1.0;
        for i in 0..10_000 {
            let s = map.tick_to_seconds(i as f64);
            assert!(s > last, "tick {i} went backwards");
            last = s;
        }
        // Seconds stay continuous across the step at 5760, where the tempo itself jumps.
        close(
            map.tick_to_seconds(5760.0 + 1e-3) - map.tick_to_seconds(5760.0 - 1e-3),
            0.0,
            1e-5,
        );
    }

    #[test]
    fn times_before_the_song_extrapolate_from_the_first_tempo() {
        let map = TempoMap::new([bpm(0, 120.0), bpm(960, 60.0)]);
        close(map.tick_to_seconds(-960.0), -0.5, 1e-12);
        close(map.seconds_to_tick(-0.5), -960.0, 1e-9);
    }

    #[test]
    fn a_run_integrates_a_ramp_exactly() {
        let map = TempoMap::new([bpm(0, 100.0), bpm(9600, 200.0)]);
        let sr = 48_000.0;
        let start = 1234.5;
        let run = map.run_at(start, sr);
        assert!(run.ramp > 0.0);
        assert_eq!(run.until, 9600.0);
        let t = start + run.advance(8000.0);
        close(
            map.tick_to_seconds(t) - map.tick_to_seconds(start),
            8000.0 / sr,
            1e-11,
        );
        close(run.frames_to(t - start), 8000.0, 1e-7);
    }

    #[test]
    fn bars_restart_at_a_signature_change() {
        // Two bars of 4/4, then 3/4.
        let map = BarMap::new(sig(4, 4), [(7680, sig(3, 4))]);
        assert_eq!(map.bar_at(0.0).index, 0);
        assert_eq!(map.bar_at(3839.0).start, 0.0);
        assert_eq!(map.bar_at(3840.0).index, 1);
        let b = map.bar_at(7680.0);
        assert_eq!((b.index, b.start, b.end), (2, 7680.0, 10560.0));
        assert_eq!(b.signature, sig(3, 4));
        assert_eq!(map.ticks_per_bar_at(7680.0), 2880);
        assert_eq!(map.bar_at(10560.0).index, 3);
        assert!(!map.bar_at(5000.0).is_fragment());
        assert_eq!(map.signature_at(7000.0), sig(4, 4));
    }

    #[test]
    fn a_change_mid_bar_leaves_a_fragmentary_bar() {
        // 4/4 from 0, a change at beat 6 cuts bar 2 of 4/4 in half.
        let map = BarMap::new(sig(4, 4), [(5760, sig(6, 8))]);
        let fragment = map.bar_at(4000.0);
        assert_eq!(
            (fragment.index, fragment.start, fragment.end),
            (1, 3840.0, 5760.0)
        );
        assert!(fragment.is_fragment());
        // The fragment still counts as a bar.
        let next = map.bar_at(5760.0);
        assert_eq!((next.index, next.start), (2, 5760.0));
        assert!(!next.is_fragment());
        assert_eq!(map.bar_at(5760.0 + 2880.0).index, 3);
    }

    #[test]
    fn markers_at_the_same_tick_and_zero_replace_the_earlier_one() {
        let map = BarMap::new(
            sig(4, 4),
            [(0, sig(5, 4)), (0, sig(7, 8)), (960, sig(3, 4))],
        );
        assert_eq!(map.signature_at(0.0), sig(7, 8));
        let mut set = map.clone();
        set.set_base(sig(2, 4));
        assert_eq!(set, map, "a marker at zero wins over the base");
    }

    #[test]
    fn set_base_renumbers_later_bars() {
        let mut map = BarMap::new(sig(4, 4), [(7680, sig(3, 4))]);
        assert_eq!(map.bar_at(7680.0).index, 2);
        map.set_base(sig(2, 4));
        assert_eq!(map.bar_at(7680.0).index, 4);
    }

    #[test]
    fn a_single_signature_matches_the_plain_quantize_formula() {
        let s = sig(7, 8);
        let map = BarMap::constant(s);
        for q in Quantize::ALL {
            for i in 0..2000 {
                let tick = i as f64 * 17.3;
                assert_eq!(
                    map.next_boundary(tick, q),
                    q.next_boundary(tick, s),
                    "{q:?}"
                );
            }
        }
        for i in 0..2000 {
            let tick = i as f64 * 13.7;
            assert_eq!(map.bar_at(tick).start, s.bar_start(tick));
        }
    }

    #[test]
    fn quantize_boundaries_cross_a_signature_change() {
        // 4/4 then 3/4 at 7680, then 6/8 at 10560.
        let map = BarMap::new(sig(4, 4), [(7680, sig(3, 4)), (10560, sig(6, 8))]);
        assert_eq!(map.next_boundary(1.0, Quantize::Bar), 3840.0);
        // The change is a bar line whatever the grid before it says.
        assert_eq!(map.next_boundary(7000.0, Quantize::Bar), 7680.0);
        assert_eq!(map.next_boundary(7680.0, Quantize::Bar), 7680.0);
        // After the change bars come from the change, not from zero.
        assert_eq!(map.next_boundary(7681.0, Quantize::Bar), 10560.0);
        assert_eq!(map.next_boundary(7681.0, Quantize::Beat), 8640.0);
        assert_eq!(map.next_boundary(10561.0, Quantize::Beat), 10560.0 + 480.0);
        // A grid longer than the segment stops at the next change.
        assert_eq!(map.next_boundary(7681.0, Quantize::FourBars), 10560.0);
        assert_eq!(map.next_boundary(5.0, Quantize::None), 5.0);
    }

    #[test]
    fn quantize_cuts_short_at_a_fragmentary_bar() {
        let map = BarMap::new(sig(4, 4), [(5760, sig(4, 4))]);
        assert_eq!(map.next_boundary(4000.0, Quantize::Bar), 5760.0);
        assert_eq!(map.next_boundary(5761.0, Quantize::Bar), 5760.0 + 3840.0);
    }

    #[test]
    fn beats_walk_across_signature_changes() {
        // Two beats of 4/4, then 3/8: a fragmentary bar, then eighths with a new downbeat.
        let map = BarMap::new(sig(4, 4), [(1920, sig(3, 8))]);
        let mut beat = map.first_beat(0.0);
        let mut got = Vec::new();
        for _ in 0..6 {
            got.push((beat.tick, beat.downbeat));
            beat = map.next_beat(beat);
        }
        assert_eq!(
            got,
            [
                (0.0, true),
                (960.0, false),
                (1920.0, true),
                (2400.0, false),
                (2880.0, false),
                (3360.0, true)
            ]
        );
        let from_mid = map.first_beat(1000.0);
        assert_eq!((from_mid.tick, from_mid.downbeat), (1920.0, true));
    }

    #[test]
    fn beats_match_the_plain_grid_on_one_signature() {
        let s = sig(7, 8);
        let map = BarMap::constant(s);
        let beat = s.ticks_per_beat() as f64;
        let mut b = map.first_beat(1234.5);
        assert_eq!(b.tick, (1234.5 / beat).ceil() * beat);
        for _ in 0..50 {
            let n = (b.tick / beat).round() as u64;
            assert_eq!(b.downbeat, n.is_multiple_of(7));
            let next = map.next_beat(b);
            assert_eq!(next.tick, b.tick + beat);
            b = next;
        }
    }
}
