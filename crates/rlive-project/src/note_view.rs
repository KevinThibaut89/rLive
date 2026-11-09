//! How notes are shown: which key tracks are visible, how keys are spelled, and how several clips
//! line up in one editor (manual pp. 284-292).

use crate::clip_edit::{in_scale, PitchScale};
use rlive_core::{note_name, ClipRegion};

// ------------------------------------------------------------- folding

/// The key tracks to show, highest first.
///
/// Fold to Notes shows only keys with notes; Fold to Scale shows the scale and any key with a note
/// outside it; a Drum Rack shows its pads and any key with a note (manual pp. 285, 287).
pub fn visible_keys(
    used: &[u8],
    fold_notes: bool,
    fold_scale: Option<PitchScale>,
    pads: &[u8],
) -> Vec<u8> {
    let has_note = |k: u8| used.contains(&k);
    let keep = |k: u8| {
        if fold_notes {
            has_note(k)
        } else if let Some(s) = fold_scale {
            in_scale(k as i32, s) || has_note(k)
        } else if !pads.is_empty() {
            pads.contains(&k) || has_note(k)
        } else {
            true
        }
    };
    let keys: Vec<u8> = (0..=127u8).rev().filter(|k| keep(*k)).collect();
    if keys.is_empty() {
        (0..=127u8).rev().collect()
    } else {
        keys
    }
}

/// Whether a key is in a scale.
pub fn spell_in_scale(key: u8, scale: PitchScale) -> bool {
    in_scale(key as i32, scale)
}

// ------------------------------------------------------------- spelling

/// How keys are named on the note ruler (manual p. 286).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Spelling {
    #[default]
    Sharps,
    Flats,
    Both,
    /// By the root's place in the circle of fifths.
    Auto,
    /// MIDI note numbers.
    Numbers,
}

impl Spelling {
    pub const ALL: [Self; 5] = [
        Self::Sharps,
        Self::Flats,
        Self::Both,
        Self::Auto,
        Self::Numbers,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sharps => "Sharps",
            Self::Flats => "Flats",
            Self::Both => "Both",
            Self::Auto => "Auto",
            Self::Numbers => "MIDI Note Numbers",
        }
    }
}

const FLAT_NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

/// Whether a root's key signature is written in flats.
fn root_uses_flats(root: u8) -> bool {
    matches!(root % 12, 1 | 3 | 5 | 8 | 10)
}

fn flat_name(key: u8) -> String {
    format!("{}{}", FLAT_NAMES[key as usize % 12], key as i32 / 12 - 2)
}

/// A key's name. Inside an active scale it has the scale's own accidentals, and the preference
/// only decides notes outside it.
pub fn spell(key: u8, spelling: Spelling, scale: Option<PitchScale>) -> String {
    if spelling == Spelling::Numbers {
        return key.to_string();
    }
    let black = matches!(key % 12, 1 | 3 | 6 | 8 | 10);
    if !black {
        return note_name(key);
    }
    let by_root = |root: u8| {
        if root_uses_flats(root) {
            flat_name(key)
        } else {
            note_name(key)
        }
    };
    if let Some((root, _)) = scale.filter(|s| in_scale(key as i32, *s)) {
        return by_root(root);
    }
    match spelling {
        Spelling::Flats => flat_name(key),
        Spelling::Both => format!("{}/{}", note_name(key), flat_name(key)),
        Spelling::Auto => by_root(scale.map_or(0, |s| s.0)),
        _ => note_name(key),
    }
}

// ------------------------------------------------------------- several clips at once

/// How many clips the editor shows at once (manual p. 292).
pub const MAX_SHOWN: usize = 8;

/// How a shown clip's content lies on the editor's timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipSpan {
    /// The timeline is the clip's own content, for a single clip.
    Content,
    /// The clip plays from `origin` on the timeline, and is seen from `from` to `to`.
    Placed {
        region: ClipRegion,
        origin: f64,
        from: f64,
        to: f64,
    },
}

impl ClipSpan {
    /// Where on the timeline a content position sounds, each time it does.
    pub fn occurrences(&self, content: u64, out: &mut Vec<f64>) {
        out.clear();
        let c = content as f64;
        let (region, origin, from, to) = match *self {
            Self::Content => {
                out.push(c);
                return;
            }
            Self::Placed {
                region,
                origin,
                from,
                to,
            } => (region, origin, from, to),
        };
        let mut push = |t: f64| {
            if t >= from && t < to {
                out.push(t);
            }
        };
        if region.looped && region.loop_len > 0.0 {
            let zero = origin + region.lead();
            // The first pass reaches from the start marker.
            if c >= region.start && c < region.loop_end() {
                push(origin + (c - region.start));
            }
            if c >= region.loop_start && c < region.loop_end() {
                let base = zero - region.loop_start;
                let mut k = (((from - base - c) / region.loop_len).floor() as i64).max(1);
                loop {
                    let t = base + k as f64 * region.loop_len + c;
                    if t >= to {
                        break;
                    }
                    push(t);
                    k += 1;
                }
            }
        } else if c >= region.start && c < region.end {
            push(origin + (c - region.start));
        }
    }

    /// The content position at a timeline position.
    pub fn content_at(&self, t: f64) -> f64 {
        match *self {
            Self::Content => t.max(0.0),
            Self::Placed { region, origin, .. } => region.content_at((t - origin).max(0.0)),
        }
    }

    /// How far a timeline position is along the clip's loop bar, if it's seen at all.
    pub fn contains(&self, t: f64) -> bool {
        match *self {
            Self::Content => true,
            Self::Placed { from, to, .. } => t >= from && t < to,
        }
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// How long until loops of these lengths come back into line. Very awkward lengths stop at
/// sixty-four of the longest, so the view stays usable.
pub fn realign_span(lengths: &[u64]) -> u64 {
    let longest = lengths.iter().copied().max().unwrap_or(1).max(1);
    let lcm = lengths
        .iter()
        .filter(|l| **l > 0)
        .fold(1u64, |a, b| a / gcd(a, *b) * *b);
    lcm.min(longest.saturating_mul(64))
}

/// The clips to show, from a selection: up to [`MAX_SHOWN`], in the order given, always keeping
/// the one in the foreground. Session clips must all be looped (manual p. 292).
pub fn pick_shown<T: Copy + PartialEq>(ordered: &[T], foreground: T) -> Vec<T> {
    let mut shown: Vec<T> = ordered.iter().copied().take(MAX_SHOWN).collect();
    if !shown.contains(&foreground) {
        shown.pop();
        shown.push(foreground);
    }
    shown
}

/// Where notes copied from several clips go: back to the same clips while the selection and
/// foreground clip haven't changed, or all to the foreground clip (manual p. 292).
pub fn paste_keeps_clips<T: PartialEq>(
    copied: &[T],
    copied_foreground: &T,
    shown: &[T],
    foreground: &T,
) -> bool {
    copied_foreground == foreground
        && copied.len() == shown.len()
        && copied.iter().all(|c| shown.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::PPQ;

    const BAR: f64 = 4.0 * PPQ as f64;
    const MAJOR: PitchScale = (0, 0b1010_1011_0101);

    #[test]
    fn folding_shows_only_what_it_should() {
        let used = [60, 61];
        assert_eq!(visible_keys(&used, true, None, &[]), vec![61, 60]);
        let scale = visible_keys(&used, false, Some(MAJOR), &[]);
        assert!(scale.contains(&62));
        assert!(!scale.contains(&63));
        assert!(
            scale.contains(&61),
            "a note outside the scale stays in view"
        );
        assert_eq!(visible_keys(&[], false, None, &[36, 38]), vec![38, 36]);
        assert_eq!(visible_keys(&used, true, None, &[36]), vec![61, 60]);
        assert_eq!(
            visible_keys(&[], true, None, &[]).len(),
            128,
            "no empty view"
        );
    }

    #[test]
    fn spelling_follows_preference_and_scale() {
        assert_eq!(spell(61, Spelling::Sharps, None), "C#3");
        assert_eq!(spell(61, Spelling::Flats, None), "Db3");
        assert_eq!(spell(61, Spelling::Both, None), "C#3/Db3");
        assert_eq!(spell(61, Spelling::Numbers, None), "61");
        // F major has a Bb, whatever the preference says of notes outside it.
        let f_major = (5, 0b1010_1011_0101);
        assert_eq!(spell(70, Spelling::Sharps, Some(f_major)), "Bb3");
        assert_eq!(spell(61, Spelling::Sharps, Some(f_major)), "C#3");
        // Auto reads the root.
        assert_eq!(spell(61, Spelling::Auto, Some((5, 0))), "Db3");
        assert_eq!(spell(61, Spelling::Auto, Some((7, 0))), "C#3");
    }

    #[test]
    fn a_shorter_loop_repeats_until_the_loops_realign() {
        assert_eq!(realign_span(&[BAR as u64, 2 * BAR as u64]), 2 * BAR as u64);
        assert_eq!(realign_span(&[3 * PPQ, 4 * PPQ]), 12 * PPQ);
        assert_eq!(realign_span(&[7, 1000]), 7000);
        assert_eq!(realign_span(&[997, 1009]), 1009 * 64, "stops somewhere");
    }

    #[test]
    fn a_looped_clip_shows_each_time_round() {
        let span = ClipSpan::Placed {
            region: ClipRegion::whole(BAR, true),
            origin: 0.0,
            from: 0.0,
            to: 3.0 * BAR,
        };
        let mut at = Vec::new();
        span.occurrences(PPQ, &mut at);
        assert_eq!(
            at,
            vec![PPQ as f64, PPQ as f64 + BAR, PPQ as f64 + 2.0 * BAR]
        );
        assert_eq!(span.content_at(BAR + PPQ as f64), PPQ as f64);
    }

    #[test]
    fn a_start_marker_before_the_loop_shifts_it() {
        let region = ClipRegion::new(0.0, None, BAR, BAR, true);
        let span = ClipSpan::Placed {
            region,
            origin: 0.0,
            from: 0.0,
            to: 3.0 * BAR,
        };
        let mut at = Vec::new();
        span.occurrences(0, &mut at);
        assert_eq!(at, vec![0.0], "the run-in plays once");
        span.occurrences(BAR as u64, &mut at);
        assert_eq!(at, vec![BAR, 2.0 * BAR]);
    }

    #[test]
    fn a_placed_arrangement_clip_is_seen_only_where_it_sits() {
        let span = ClipSpan::Placed {
            region: ClipRegion::whole(BAR, false),
            origin: 2.0 * BAR,
            from: 2.0 * BAR,
            to: 3.0 * BAR,
        };
        let mut at = Vec::new();
        span.occurrences(PPQ, &mut at);
        assert_eq!(at, vec![2.0 * BAR + PPQ as f64]);
        span.occurrences(BAR as u64, &mut at);
        assert!(at.is_empty());
        assert_eq!(span.content_at(2.0 * BAR + 5.0), 5.0);
    }

    #[test]
    fn eight_clips_at_most_and_the_foreground_stays() {
        let all: Vec<u32> = (0..12).collect();
        let shown = pick_shown(&all, 11);
        assert_eq!(shown.len(), MAX_SHOWN);
        assert!(shown.contains(&11));
        assert_eq!(pick_shown(&[1, 2], 2), vec![1, 2]);
    }

    #[test]
    fn pasting_goes_back_to_the_same_clips_only_if_nothing_changed() {
        assert!(paste_keeps_clips(&[1, 2], &1, &[2, 1], &1));
        assert!(!paste_keeps_clips(&[1, 2], &1, &[2, 1], &2));
        assert!(!paste_keeps_clips(&[1, 2], &1, &[3], &1));
    }
}
