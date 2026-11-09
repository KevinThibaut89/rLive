//! The groove pool: what a groove holds, its file format, the grooves that come with rLive,
//! and the operations that read or write notes and Warp Markers through one.

use crate::clip::{NoteData, WarpMarker};
use crate::defaults::is_default;
use crate::FileRef;
use rlive_core::{ClipRegion, GrooveAmounts, GrooveHit, GrooveId, GrooveMap, PPQ};
use serde::{Deserialize, Serialize};

/// What a groove file is called on disk.
pub const GROOVE_EXTENSION: &str = "rgroove";
const GROOVE_FORMAT: &str = "rlive.groove";
const GROOVE_VERSION: u32 = 1;
/// The shortest gap between two Warp Markers a groove may leave, in ticks.
const MIN_MARKER_GAP: f64 = 1.0;

/// The notes a groove is made of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroovePattern {
    /// Ticks before it repeats.
    pub length: u64,
    pub hits: Vec<GrooveHit>,
}

impl Default for GroovePattern {
    fn default() -> Self {
        Self {
            length: PPQ * 4,
            hits: Vec::new(),
        }
    }
}

/// A groove loaded into the project. Clips refer to it by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Groove {
    pub id: GrooveId,
    pub name: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub file: Option<FileRef>,
    /// The grid the groove's timing is measured against, in ticks.
    #[serde(default = "sixteenth", skip_serializing_if = "is_sixteenth")]
    pub base: u32,
    /// Each 0..100.
    #[serde(default, skip_serializing_if = "is_default")]
    pub quantize: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub timing: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub random: f32,
    /// -100..100.
    #[serde(default, skip_serializing_if = "is_default")]
    pub velocity: f32,
    /// Kept in the project, so a Set never needs the file back.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pattern: GroovePattern,
}

fn sixteenth() -> u32 {
    rlive_core::PPQ as u32 / 4
}

fn is_sixteenth(v: &u32) -> bool {
    *v == sixteenth()
}

/// The Base chooser's values, in ticks and as they read.
pub const BASE_CHOICES: [(u32, &str); 6] = [
    (PPQ as u32, "1/4"),
    (PPQ as u32 / 2, "1/8"),
    (PPQ as u32 / 3, "1/8T"),
    (PPQ as u32 / 4, "1/16"),
    (PPQ as u32 / 6, "1/16T"),
    (PPQ as u32 / 8, "1/32"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroovePool {
    pub grooves: Vec<Groove>,
    /// Global Amount, 0..1.3.
    pub amount: f32,
}

impl Default for GroovePool {
    fn default() -> Self {
        Self {
            grooves: Vec::new(),
            amount: 1.0,
        }
    }
}

impl GroovePool {
    pub fn get(&self, id: GrooveId) -> Option<&Groove> {
        self.grooves.iter().find(|g| g.id == id)
    }

    pub fn get_mut(&mut self, id: GrooveId) -> Option<&mut Groove> {
        self.grooves.iter_mut().find(|g| g.id == id)
    }

    /// The groove's position in the pool, which is its slot in the engine's bank.
    pub fn index_of(&self, id: GrooveId) -> Option<u32> {
        self.grooves
            .iter()
            .position(|g| g.id == id)
            .map(|i| i as u32)
    }

    /// The map a groove places notes with, after Global Amount.
    pub fn map_of(&self, groove: &Groove) -> GrooveMap {
        groove.map(self.amount)
    }
}

impl Groove {
    /// A new groove playing `pattern` at full Timing, as a file drops into the pool.
    pub fn new(name: impl Into<String>, pattern: GroovePattern) -> Self {
        Self {
            id: GrooveId::new(),
            name: name.into(),
            file: None,
            base: sixteenth(),
            quantize: 0.0,
            timing: 100.0,
            random: 0.0,
            velocity: 0.0,
            pattern,
        }
    }

    /// What the groove does, with Global Amount scaling Timing, Random and Velocity.
    pub fn map(&self, amount: f32) -> GrooveMap {
        let amount = amount.clamp(0.0, 1.3);
        GrooveMap::new(
            &self.pattern.hits,
            self.pattern.length as f64,
            f64::from(self.base.max(1)),
            GrooveAmounts {
                quantize: (self.quantize / 100.0).clamp(0.0, 1.0),
                timing: self.timing / 100.0 * amount,
                random: self.random / 100.0 * amount,
                velocity: self.velocity / 100.0 * amount,
            },
        )
    }

    /// The groove as a file.
    pub fn to_file(&self) -> Result<String, String> {
        ron::ser::to_string_pretty(
            &GrooveFile {
                format: GROOVE_FORMAT.into(),
                version: GROOVE_VERSION,
                name: self.name.clone(),
                base: self.base,
                quantize: self.quantize,
                timing: self.timing,
                random: self.random,
                velocity: self.velocity,
                pattern: self.pattern.clone(),
            },
            ron::ser::PrettyConfig::new(),
        )
        .map_err(|e| e.to_string())
    }

    /// Reads a groove file. The new groove has an id of its own.
    pub fn from_file(text: &str) -> Result<Self, String> {
        let f: GrooveFile = ron::from_str(text).map_err(|e| e.to_string())?;
        if f.format != GROOVE_FORMAT {
            return Err("this is not an rLive groove file".into());
        }
        if f.version > GROOVE_VERSION {
            return Err(format!("groove file version {} is too new", f.version));
        }
        if f.pattern.length == 0 || f.base == 0 {
            return Err("groove file has no length".into());
        }
        Ok(Self {
            id: GrooveId::new(),
            name: f.name,
            file: None,
            base: f.base,
            quantize: f.quantize.clamp(0.0, 100.0),
            timing: f.timing.clamp(0.0, 100.0),
            random: f.random.clamp(0.0, 100.0),
            velocity: f.velocity.clamp(-100.0, 100.0),
            pattern: f.pattern,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct GrooveFile {
    format: String,
    version: u32,
    name: String,
    base: u32,
    quantize: f32,
    timing: f32,
    random: f32,
    velocity: f32,
    pattern: GroovePattern,
}

/// The part of a clip that plays: its loop if it loops, otherwise from start to end marker.
fn playing_range(region: &ClipRegion) -> (f64, f64) {
    if region.looped {
        (region.loop_start, region.loop_end())
    } else {
        (region.start, region.end)
    }
}

/// A groove with the timing and volume of the notes in the part of a MIDI clip that plays.
pub fn extract_from_notes(notes: &[NoteData], region: &ClipRegion) -> GroovePattern {
    let (from, to) = playing_range(region);
    let mut hits: Vec<GrooveHit> = notes
        .iter()
        .filter(|n| (from..to).contains(&(n.start as f64)))
        .map(|n| GrooveHit {
            tick: n.start as f64 - from,
            velocity: n.velocity.clamp(0.0, 1.0),
        })
        .collect();
    hits.sort_by(|a, b| a.tick.total_cmp(&b.tick));
    GroovePattern {
        length: (to - from).max(1.0) as u64,
        hits,
    }
}

/// A groove from events found in audio, as `(content tick, volume)`, within what plays.
pub fn extract_from_events(events: &[(f64, f32)], region: &ClipRegion) -> GroovePattern {
    let (from, to) = playing_range(region);
    let loudest = events
        .iter()
        .map(|(_, v)| *v)
        .fold(0.0f32, f32::max)
        .max(1e-6);
    let mut hits: Vec<GrooveHit> = events
        .iter()
        .filter(|(t, _)| (from..to).contains(t))
        .map(|(t, v)| GrooveHit {
            tick: t - from,
            velocity: (*v / loudest).clamp(0.0, 1.0),
        })
        .collect();
    hits.sort_by(|a, b| a.tick.total_cmp(&b.tick));
    GroovePattern {
        length: (to - from).max(1.0) as u64,
        hits,
    }
}

/// A repeatable number in -1..1 for one note of a Commit.
fn roll(seed: u64, index: u64) -> f32 {
    let mut x = seed.wrapping_add(index).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

/// Writes a groove into a MIDI clip's notes: where they are heard now is where they will be.
pub fn commit_notes(map: &GrooveMap, notes: &mut [NoteData], seed: u64) {
    for (i, n) in notes.iter_mut().enumerate() {
        let (at, velocity) = map.place(n.start as f64, n.velocity, roll(seed, i as u64));
        n.start = at.round().max(0.0) as u64;
        n.velocity = velocity;
    }
    notes.sort_by_key(|n| (n.start, n.key));
}

/// The Warp Markers that make `anchors`, as `(content tick, file seconds)`, land where the groove
/// puts them. Everything between two anchors follows along. Sorted, and never folded back on
/// itself.
pub fn groove_markers(map: &GrooveMap, anchors: &[(f64, f64)], seed: u64) -> Vec<WarpMarker> {
    let mut sorted: Vec<(f64, f64)> = anchors.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    sorted.dedup_by(|a, b| (a.0 - b.0).abs() < MIN_MARKER_GAP);
    let mut out: Vec<WarpMarker> = Vec::with_capacity(sorted.len());
    for (i, (tick, seconds)) in sorted.into_iter().enumerate() {
        let (mut at, _) = map.place(tick, 0.5, roll(seed, i as u64));
        if let Some(last) = out.last() {
            at = at.max(last.tick + MIN_MARKER_GAP);
            if seconds <= last.seconds {
                continue;
            }
        }
        out.push(WarpMarker {
            seconds,
            tick: at.max(0.0),
        });
    }
    out
}

fn swing(name: &str, base: u64, percent: f64, bars: u64) -> Groove {
    let length = PPQ * 4 * bars;
    let delay = base as f64 * (percent - 50.0) / 50.0 * 2.0;
    let mut hits = Vec::new();
    for i in 0..length / base {
        let offbeat = i % 2 == 1;
        let velocity = if offbeat {
            0.62
        } else if i % 4 == 0 {
            1.0
        } else {
            0.8
        };
        hits.push(GrooveHit {
            tick: (i * base) as f64 + if offbeat { delay } else { 0.0 },
            velocity,
        });
    }
    Groove::new(name, GroovePattern { length, hits })
}

fn pushed(name: &str, offset: f64, spread: f64) -> Groove {
    let step = PPQ / 4;
    let length = PPQ * 4;
    let hits = (0..length / step)
        .map(|i| {
            let wobble = spread * (i as f64 * 2.399).sin();
            GrooveHit {
                tick: (i * step) as f64 + offset + wobble,
                velocity: 0.7 + 0.25 * ((i * 3 % 8) as f32 / 7.0),
            }
        })
        .collect();
    Groove::new(name, GroovePattern { length, hits })
}

/// The grooves that come with rLive, all of them generated here. C12's browser lists these; the
/// Groove Pool's Load menu reads them directly until it lands.
pub fn factory_grooves() -> Vec<Groove> {
    let mut out = vec![Groove {
        quantize: 100.0,
        timing: 0.0,
        ..Groove::new(
            "Quantize Only",
            GroovePattern {
                length: PPQ * 4,
                hits: Vec::new(),
            },
        )
    }];
    for percent in [54.0, 58.0, 62.0, 66.0, 70.0] {
        out.push(swing(&format!("Swing 8 {percent:.0}"), PPQ / 2, percent, 1));
        out.push(swing(
            &format!("Swing 16 {percent:.0}"),
            PPQ / 4,
            percent,
            1,
        ));
    }
    out.push(pushed("Laid Back 16", 18.0, 4.0));
    out.push(pushed("Pushed 16", -14.0, 3.0));
    out.push(pushed("Loose 16", 0.0, 22.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::GrooveAmounts;

    fn note(start: u64, velocity: f32) -> NoteData {
        NoteData::new(start, 120, 60, velocity)
    }

    fn straight() -> GroovePattern {
        GroovePattern {
            length: PPQ,
            hits: vec![
                GrooveHit {
                    tick: 0.0,
                    velocity: 1.0,
                },
                GrooveHit {
                    tick: 240.0 + 40.0,
                    velocity: 0.5,
                },
            ],
        }
    }

    #[test]
    fn a_groove_file_comes_back_whole() {
        let mut g = Groove::new("Mine", straight());
        g.quantize = 25.0;
        g.velocity = -40.0;
        let back = Groove::from_file(&g.to_file().unwrap()).unwrap();
        assert_ne!(back.id, g.id, "a loaded groove is a new one");
        assert_eq!(back.pattern, g.pattern);
        assert_eq!((back.quantize, back.velocity), (25.0, -40.0));
    }

    #[test]
    fn other_files_are_refused() {
        assert!(Groove::from_file("()").is_err());
        let mut text = Groove::new("x", straight()).to_file().unwrap();
        text = text.replace(GROOVE_FORMAT, "something.else");
        assert!(Groove::from_file(&text).is_err());
    }

    #[test]
    fn global_amount_scales_timing_random_and_velocity_but_not_quantize() {
        let mut g = Groove::new("x", straight());
        g.quantize = 50.0;
        g.velocity = 100.0;
        let a = g.map(1.3).amounts();
        assert!((a.timing - 1.3).abs() < 1e-6);
        assert!((a.velocity - 1.3).abs() < 1e-6);
        assert_eq!(a.quantize, 0.5);
        let at_zero = g.map(0.0);
        assert_eq!(at_zero.place(240.0, 0.5, 0.0).0, 240.0);
    }

    #[test]
    fn commit_moves_notes_to_where_the_groove_put_them() {
        let map = Groove::new("x", straight()).map(1.0);
        let mut notes = vec![note(0, 0.5), note(240, 0.5)];
        commit_notes(&map, &mut notes, 7);
        assert_eq!(notes[0].start, 0);
        assert_eq!(notes[1].start, 280);
    }

    #[test]
    fn commit_of_random_is_repeatable_and_differs_per_voice() {
        let mut g = Groove::new("x", GroovePattern::default());
        g.timing = 0.0;
        g.random = 100.0;
        let map = g.map(1.0);
        let notes: Vec<NoteData> = (0..16).map(|_| note(PPQ * 2, 0.5)).collect();
        let (mut a, mut b) = (notes.clone(), notes.clone());
        commit_notes(&map, &mut a, 11);
        commit_notes(&map, &mut b, 11);
        assert_eq!(a, b, "the same seed gives the same notes");
        assert!(
            a.windows(2).any(|w| w[0].start != w[1].start),
            "notes played together are pulled apart"
        );
    }

    #[test]
    fn extraction_round_trips_through_a_clip() {
        let g = Groove::new("x", straight());
        let map = g.map(1.0);
        let mut notes = vec![note(0, 1.0), note(240, 0.5)];
        commit_notes(&map, &mut notes, 1);
        let region = ClipRegion::whole(PPQ as f64, true);
        let back = extract_from_notes(&notes, &region);
        assert_eq!(back.length, PPQ);
        assert_eq!(back.hits, straight().hits);
    }

    #[test]
    fn extraction_only_hears_what_plays() {
        let notes = vec![note(0, 1.0), note(1000, 0.5), note(3000, 0.7)];
        let region = ClipRegion::new(0.0, None, 960.0, 960.0, true);
        let p = extract_from_notes(&notes, &region);
        assert_eq!(p.length, 960);
        assert_eq!(p.hits.len(), 1);
        assert_eq!(p.hits[0].tick, 40.0);
    }

    #[test]
    fn audio_events_become_relative_volume() {
        let region = ClipRegion::whole(960.0, true);
        let p = extract_from_events(&[(0.0, 0.8), (480.0, 0.4), (2000.0, 0.2)], &region);
        assert_eq!(p.hits.len(), 2);
        assert_eq!(p.hits[0].velocity, 1.0);
        assert_eq!(p.hits[1].velocity, 0.5);
    }

    #[test]
    fn markers_follow_the_groove_and_stay_in_order() {
        let map = Groove::new("x", straight()).map(1.0);
        let m = groove_markers(&map, &[(0.0, 0.0), (240.0, 0.5), (480.0, 1.0)], 0);
        assert_eq!(m[0].tick, 0.0);
        assert_eq!(m[1].tick, 280.0);
        assert_eq!(m[1].seconds, 0.5);
        assert!(m.windows(2).all(|w| w[0].tick < w[1].tick));
    }

    #[test]
    fn markers_never_cross_even_when_random_is_large() {
        let mut g = Groove::new("x", GroovePattern::default());
        g.timing = 0.0;
        g.random = 100.0;
        let map = g.map(1.0);
        let anchors: Vec<(f64, f64)> = (0..50).map(|i| (i as f64 * 30.0, i as f64 * 0.1)).collect();
        let m = groove_markers(&map, &anchors, 3);
        assert!(m.windows(2).all(|w| w[0].tick < w[1].tick));
    }

    #[test]
    fn factory_grooves_are_valid_and_named_apart() {
        let all = factory_grooves();
        assert!(all.len() > 10);
        let mut names: Vec<&str> = all.iter().map(|g| g.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len());
        for g in &all {
            assert!(g.pattern.length > 0);
            Groove::from_file(&g.to_file().unwrap()).unwrap();
        }
        let _ = GrooveAmounts::default();
    }
}
