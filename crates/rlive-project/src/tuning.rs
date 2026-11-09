//! Tuning systems: what a Set holds, Scala and ASCL files, controller layouts, and retuning.
//!
//! A scale has `n` degrees, repeating every pseudo-octave. A note of the scale is an octave and a
//! degree; counting them along gives its *position*. MIDI key `k` plays the position `lowest + k`.
//! The default puts the scale's root in octave 3 on key 60.

use crate::clip::NoteData;
use crate::defaults::is_default;
use rlive_core::{KeyLayout, TuningTable};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// The 12-TET frequency of a key.
pub fn equal_hz(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// A note of a scale: which pseudo-octave, and which degree in it. Octave 3 holds middle C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScaleNote {
    pub octave: i32,
    pub degree: u32,
}

/// A tuning system loaded into the project, kept whole so the project doesn't need the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tuning {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub description: String,
    /// Each scale degree's pitch in cents above the root; the last one is the pseudo-octave.
    pub cents: Vec<f64>,
    /// The note that sounds at `reference_hz`.
    pub reference: ScaleNote,
    pub reference_hz: f64,
    /// What MIDI key 0 plays. Every other key follows, so the highest key plays 127 steps above.
    pub lowest: ScaleNote,
    #[serde(default, skip_serializing_if = "is_default")]
    pub note_names: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub source: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub link: String,
}

impl Tuning {
    /// A tuning of these scale degrees, its root in octave 3 on middle C. As in Live, a
    /// twelve-note scale defaults to A = 440 Hz and any other to its root at 440 Hz.
    pub fn new(name: impl Into<String>, description: impl Into<String>, cents: Vec<f64>) -> Self {
        let n = cents.len().max(1) as i32;
        let degree = if n == 12 { 9 } else { 0 };
        let lowest_position = 3 * n - 60;
        Self {
            name: name.into(),
            description: description.into(),
            cents,
            reference: ScaleNote { octave: 3, degree },
            reference_hz: 440.0,
            lowest: ScaleNote {
                octave: lowest_position.div_euclid(n),
                degree: lowest_position.rem_euclid(n) as u32,
            },
            note_names: Vec::new(),
            source: String::new(),
            link: String::new(),
        }
    }

    /// Notes per pseudo-octave.
    pub fn steps(&self) -> i32 {
        self.cents.len().max(1) as i32
    }

    /// The pseudo-octave in cents.
    pub fn period(&self) -> f64 {
        self.cents.last().copied().unwrap_or(1200.0)
    }

    pub fn position_of(&self, note: ScaleNote) -> i32 {
        note.octave * self.steps() + note.degree.min(self.steps() as u32 - 1) as i32
    }

    pub fn note_at(&self, position: i32) -> ScaleNote {
        ScaleNote {
            octave: position.div_euclid(self.steps()),
            degree: position.rem_euclid(self.steps()) as u32,
        }
    }

    /// The scale position a key plays.
    pub fn key_position(&self, key: u8) -> i32 {
        self.position_of(self.lowest) + i32::from(key)
    }

    pub fn key_note(&self, key: u8) -> ScaleNote {
        self.note_at(self.key_position(key))
    }

    /// The highest key's note: always 127 steps above the lowest.
    pub fn highest(&self) -> ScaleNote {
        self.key_note(127)
    }

    /// Sets what the highest key plays. The lowest follows, keeping the notes between.
    pub fn set_highest(&mut self, note: ScaleNote) {
        self.lowest = self.note_at(self.position_of(note) - 127);
    }

    fn cents_at(&self, position: i32) -> f64 {
        let octave = f64::from(position.div_euclid(self.steps()));
        let degree = position.rem_euclid(self.steps()) as usize;
        let within = if degree == 0 {
            0.0
        } else {
            self.cents.get(degree - 1).copied().unwrap_or(0.0)
        };
        octave * self.period() + within
    }

    /// The frequency a scale position sounds at.
    pub fn position_hz(&self, position: i32) -> f64 {
        let from_reference =
            self.cents_at(position) - self.cents_at(self.position_of(self.reference));
        self.reference_hz * 2f64.powf(from_reference / 1200.0)
    }

    pub fn key_hz(&self, key: u8) -> f64 {
        self.position_hz(self.key_position(key))
    }

    pub fn note_hz(&self, note: ScaleNote) -> f64 {
        self.position_hz(self.position_of(note))
    }

    /// Picks another note as the reference. The displayed frequency moves to that note's, so
    /// nothing is heard to change until the frequency itself is adjusted.
    pub fn set_reference_note(&mut self, note: ScaleNote) {
        let hz = self.note_hz(note);
        self.reference = note;
        self.reference_hz = hz;
    }

    /// Each key's offset from 12-TET, for the engine.
    pub fn table(&self) -> TuningTable {
        TuningTable::from_hz(|k| self.key_hz(k))
    }

    /// The name of a degree: its file name, or its number counted from 1.
    pub fn degree_name(&self, degree: u32) -> String {
        self.note_names
            .get(degree as usize)
            .cloned()
            .unwrap_or_else(|| (degree + 1).to_string())
    }

    /// A key on the note ruler.
    pub fn key_label(&self, key: u8) -> String {
        let note = self.key_note(key);
        if self.note_names.get(note.degree as usize).is_some() {
            format!("{}{}", self.degree_name(note.degree), note.octave)
        } else {
            format!("{}.{}", note.degree + 1, note.octave)
        }
    }

    /// What the Info View says about this tuning.
    pub fn summary(&self) -> String {
        let per = format!(
            "{} notes per pseudo-octave of {:.1} cents",
            self.steps(),
            self.period()
        );
        if self.description.is_empty() {
            format!("{}. {per}", self.name)
        } else {
            format!("{}. {}. {per}", self.name, self.description)
        }
    }

    /// Reads a Scala `.scl` file or its ASCL extension. `file_stem` names it if the file doesn't.
    pub fn parse(text: &str, file_stem: &str) -> Result<Self, String> {
        let mut name = None;
        let mut description = None;
        let mut count = None;
        let mut cents = Vec::new();
        let mut directives = Vec::new();
        for line in text.lines() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if let Some(comment) = line.strip_prefix('!') {
                let comment = comment.trim();
                match comment.strip_prefix("@ABL") {
                    Some(d) => directives.push(d.trim().to_string()),
                    None if name.is_none() && description.is_none() => {
                        name = comment
                            .strip_suffix(".ascl")
                            .or_else(|| comment.strip_suffix(".scl"))
                            .filter(|n| !n.is_empty())
                            .map(str::to_string);
                    }
                    None => {}
                }
                continue;
            }
            if description.is_none() {
                description = Some(line.trim().to_string());
            } else if count.is_none() {
                let first = line.split_whitespace().next().unwrap_or("");
                count = Some(
                    first
                        .parse::<usize>()
                        .map_err(|_| format!("bad note count: {first:?}"))?,
                );
            } else if cents.len() < count.unwrap_or(0) && !line.trim().is_empty() {
                cents.push(parse_pitch(line)?);
            }
        }
        let count = count.ok_or("the file ends before its note count")?;
        if cents.len() != count {
            return Err(format!("expected {count} pitches, found {}", cents.len()));
        }
        let mut t = Tuning::new(
            name.unwrap_or_else(|| file_stem.to_string()),
            description.unwrap_or_default(),
            cents,
        );
        for d in directives {
            t.apply_directive(&d)?;
        }
        Ok(t)
    }

    fn apply_directive(&mut self, directive: &str) -> Result<(), String> {
        let (word, rest) = directive
            .split_once(char::is_whitespace)
            .unwrap_or((directive, ""));
        let rest = rest.trim();
        let number = |s: &str| -> Result<f64, String> {
            s.parse::<f64>()
                .map_err(|_| format!("{word}: bad number {s:?}"))
        };
        match word {
            "REFERENCE_PITCH" => {
                let f: Vec<&str> = rest.split_whitespace().collect();
                let [octave, degree, hz] = f[..] else {
                    return Err("REFERENCE_PITCH needs an octave, a note and a frequency".into());
                };
                self.reference = ScaleNote {
                    octave: number(octave)? as i32,
                    degree: number(degree)? as u32,
                };
                self.reference_hz = number(hz)?;
            }
            "NOTE_NAMES" => self.note_names = quoted(rest),
            "NOTE_RANGE_BY_INDEX" => {
                let f: Vec<&str> = rest.split_whitespace().collect();
                if f.len() >= 2 {
                    self.lowest = ScaleNote {
                        octave: number(f[0])? as i32,
                        degree: number(f[1])? as u32,
                    };
                }
            }
            "NOTE_RANGE_BY_FREQUENCY" => {
                if let Some(low) = rest.split_whitespace().next() {
                    let low = number(low)?;
                    let from = self.position_of(self.reference) - self.steps() * 16;
                    let at = (from..from + self.steps() * 32)
                        .find(|p| self.position_hz(*p) >= low)
                        .unwrap_or(from);
                    self.lowest = self.note_at(at);
                }
            }
            "SOURCE" => self.source = unquote(rest),
            "LINK" => self.link = unquote(rest),
            // A later version of the format: what we do know still applies.
            _ => {}
        }
        Ok(())
    }

    /// The tuning as an `.ascl` file.
    pub fn to_ascl(&self) -> String {
        let mut out = format!("! {}.ascl\n!\n{}\n", self.name, self.description);
        out.push_str(&format!(" {}\n!\n", self.cents.len()));
        for c in &self.cents {
            out.push_str(&format!(" {c:.6}\n"));
        }
        out.push_str("!\n");
        if !self.note_names.is_empty() {
            let names: Vec<String> = self.note_names.iter().map(|n| format!("\"{n}\"")).collect();
            out.push_str(&format!("! @ABL NOTE_NAMES {}\n", names.join(" ")));
        }
        out.push_str(&format!(
            "! @ABL REFERENCE_PITCH {} {} {}\n",
            self.reference.octave, self.reference.degree, self.reference_hz
        ));
        let high = self.highest();
        out.push_str(&format!(
            "! @ABL NOTE_RANGE_BY_INDEX {} {} {} {}\n",
            self.lowest.octave, self.lowest.degree, high.octave, high.degree
        ));
        if !self.source.is_empty() {
            out.push_str(&format!("! @ABL SOURCE {}\n", self.source));
        }
        if !self.link.is_empty() {
            out.push_str(&format!("! @ABL LINK {}\n", self.link));
        }
        out
    }
}

/// One Scala pitch line: a dot makes it cents, anything else is a ratio. What follows is ignored.
fn parse_pitch(line: &str) -> Result<f64, String> {
    let token = line.split_whitespace().next().unwrap_or("");
    if token.contains('.') {
        return token
            .parse::<f64>()
            .map_err(|_| format!("bad pitch: {token:?}"));
    }
    let (top, bottom) = token.split_once('/').unwrap_or((token, "1"));
    let top = top
        .parse::<u64>()
        .map_err(|_| format!("bad ratio: {token:?}"))?;
    let bottom = bottom
        .parse::<u64>()
        .map_err(|_| format!("bad ratio: {token:?}"))?;
    if top == 0 || bottom == 0 {
        return Err(format!("bad ratio: {token:?}"));
    }
    Ok(1200.0 * (top as f64 / bottom as f64).log2())
}

fn quoted(text: &str) -> Vec<String> {
    text.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn unquote(text: &str) -> String {
    text.trim().trim_matches('"').to_string()
}

/// Tunings that come with rLive, generated here: C12's browser lists these under Tunings.
pub fn factory_tunings() -> Vec<Tuning> {
    let edo = |n: usize, note: &str| {
        Tuning::new(
            format!("{n} EDO"),
            format!("{n} equal divisions of the octave. {note}"),
            (1..=n).map(|i| i as f64 * 1200.0 / n as f64).collect(),
        )
    };
    let just = |name: &str, about: &str, ratios: &[(u32, u32)]| {
        Tuning::new(
            name,
            about,
            ratios
                .iter()
                .map(|(a, b)| 1200.0 * (f64::from(*a) / f64::from(*b)).log2())
                .collect(),
        )
    };
    let mut out = vec![
        edo(5, "A pentatonic scale of equal steps"),
        edo(7, "A heptatonic scale of equal steps"),
        edo(19, "A meantone-like tuning"),
        edo(24, "Quarter tones"),
        edo(31, "A close approximation of quarter-comma meantone"),
        edo(
            53,
            "A close approximation of Pythagorean and just intervals",
        ),
        just(
            "5-Limit Just Intonation",
            "A twelve-note just scale",
            &[
                (16, 15),
                (9, 8),
                (6, 5),
                (5, 4),
                (4, 3),
                (45, 32),
                (3, 2),
                (8, 5),
                (5, 3),
                (9, 5),
                (15, 8),
                (2, 1),
            ],
        ),
        just(
            "Harmonic Series 8-16",
            "The eighth octave of the harmonic series",
            &[
                (9, 8),
                (10, 8),
                (11, 8),
                (12, 8),
                (13, 8),
                (14, 8),
                (15, 8),
                (2, 1),
            ],
        ),
    ];
    out.push(Tuning::new(
        "Bohlen-Pierce",
        "13 equal divisions of the 3:1 tritave, a scale with no octaves",
        (1..=13)
            .map(|i| f64::from(i) * 1200.0 * 3f64.log2() / 13.0)
            .collect(),
    ));
    out
}

/// How a controller's keys are laid out over a tuning system, per track.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TuningLayout {
    /// Every key plays the next note of the tuning.
    #[default]
    All,
    /// Black keys only, centred on C#3.
    Black,
    /// White keys only, centred on C3.
    White,
    /// Each key plays whichever note is nearest its usual pitch.
    Closest,
    Custom(CustomLayout),
}

/// The Configure MIDI Layout dialog: which keys of each octave play, and where they start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomLayout {
    /// The controller key that plays `note`.
    pub anchor: u8,
    /// The note of the tuning it plays, as a piano-roll key.
    pub note: u8,
    /// Which keys of the octave play, starting from C. Each plays one step up from the last.
    pub keys: [bool; 12],
}

impl Default for CustomLayout {
    fn default() -> Self {
        Self {
            anchor: 60,
            note: 60,
            keys: [true; 12],
        }
    }
}

const WHITE: [bool; 12] = [
    true, false, true, false, true, true, false, true, false, true, false, true,
];
const BLACK: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

impl TuningLayout {
    pub const NAMES: [&'static str; 5] = [
        "All Keys",
        "Black Keys Only",
        "White Keys Only",
        "Closest in Pitch to Keyboard",
        "Custom Controller Layout",
    ];

    pub fn name(&self) -> &'static str {
        Self::NAMES[match self {
            Self::All => 0,
            Self::Black => 1,
            Self::White => 2,
            Self::Closest => 3,
            Self::Custom(_) => 4,
        }]
    }

    /// What each controller key plays on `tuning`, or `None` if every key plays itself.
    pub fn keys(&self, tuning: &Tuning) -> Option<KeyLayout> {
        match self {
            Self::All => None,
            Self::Black => Some(sequential(61, 60, BLACK)),
            Self::White => Some(sequential(60, 60, WHITE)),
            Self::Custom(c) => Some(sequential(c.anchor, c.note, c.keys)),
            Self::Closest => Some(KeyLayout::new(|k| {
                Some(nearest_key(equal_hz(k), |candidate| {
                    tuning.key_hz(candidate)
                }))
            })),
        }
    }
}

/// Walks the playing keys outward from `anchor`, one note of the tuning per key.
fn sequential(anchor: u8, note: u8, keys: [bool; 12]) -> KeyLayout {
    let plays = |k: i32| keys[k.rem_euclid(12) as usize];
    let anchor = i32::from(anchor);
    KeyLayout::new(|key| {
        let key = i32::from(key);
        if !plays(key) {
            return None;
        }
        let steps = match key.cmp(&anchor) {
            Ordering::Equal => 0,
            Ordering::Greater => (anchor + 1..=key).filter(|k| plays(*k)).count() as i32,
            Ordering::Less => -((key..anchor).filter(|k| plays(*k)).count() as i32),
        };
        u8::try_from(i32::from(note) + steps)
            .ok()
            .filter(|k| *k < 128)
    })
}

/// The key whose frequency is closest to `hz`, by pitch.
pub fn nearest_key(hz: f64, of: impl Fn(u8) -> f64) -> u8 {
    let distance = |k: u8| (of(k) / hz).log2().abs();
    (0..128u8)
        .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
        .unwrap_or(60)
}

/// Moves notes so they sound as close as they can to what they did before. `before` and `after`
/// give each key's frequency in the old and new tuning. Notes that land on the same key and
/// overlap can't both stay: the later is kept and the earlier is shortened, or dropped if the two
/// began together.
pub fn retune_notes(
    notes: &mut Vec<NoteData>,
    before: impl Fn(u8) -> f64,
    after: impl Fn(u8) -> f64,
) {
    let mut map = [0u8; 128];
    for (k, slot) in map.iter_mut().enumerate() {
        *slot = nearest_key(before(k as u8), &after);
    }
    for n in notes.iter_mut() {
        n.key = map[(n.key & 127) as usize];
    }
    notes.sort_by_key(|n| (n.key, n.start));
    let mut kept: Vec<NoteData> = Vec::with_capacity(notes.len());
    for n in notes.drain(..) {
        if let Some(last) = kept.last_mut() {
            if last.key == n.key && n.start < last.start + last.length {
                if n.start == last.start {
                    *last = n;
                    continue;
                }
                last.length = n.start - last.start;
            }
        }
        kept.push(n);
    }
    kept.sort_by_key(|n| (n.start, n.key));
    *notes = kept;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-3, "{a} vs {b}");
    }

    /// Scala's own documentation shows this scale.
    const MEANTONE: &str = "! meanquar.scl\n!\n1/4-comma meantone scale. Philipp Stoelzel\n 12\n!\n 76.04900\n 193.15686\n 310.26471\n 5/4\n 503.42157\n 579.47057\n 696.57843\n 25/16\n 889.73529\n 1006.84314\n 1082.89214\n 2/1\n";

    /// The worked example on Ableton's "make your own tuning" page.
    const ASCL: &str = "! Make your own tuning file.ascl\nThis is an example of a description.\n12\n100. ! C#\n200. ! D\n300.\n400.\n500.\n600.\n700.\n800.\n900. ! A\n1000.\n1100.\n2/1 ! C\n! @ABL NOTE_NAMES \"C\" \"C#\" \"D\" \"D#\" \"E\" \"F\" \"F#\" \"G\" \"G#\" \"A\" \"A#\" \"B\"\n! @ABL REFERENCE_PITCH 3 9 440.0\n";

    fn edo(n: usize) -> Tuning {
        Tuning::new(
            format!("{n} EDO"),
            "",
            (1..=n).map(|i| i as f64 * 1200.0 / n as f64).collect(),
        )
    }

    #[test]
    fn scala_files_read_cents_and_ratios() {
        let t = Tuning::parse(MEANTONE, "x").unwrap();
        assert_eq!(t.name, "meanquar");
        assert_eq!(t.description, "1/4-comma meantone scale. Philipp Stoelzel");
        assert_eq!(t.steps(), 12);
        close(t.cents[0], 76.049);
        close(t.cents[2], 310.26471);
        close(t.cents[3], 386.31371);
        close(t.period(), 1200.0);
    }

    #[test]
    fn scala_edge_cases_from_the_format_page() {
        // Pitches with a trailing word, a bare integer, a dot with no decimals.
        let t = Tuning::parse("test\n 4\n 100.0 cents\n 408.\n 3\n 10/20\n", "x").unwrap();
        close(t.cents[0], 100.0);
        close(t.cents[1], 408.0);
        close(t.cents[2], 1901.955);
        close(t.cents[3], -1200.0);
        assert_eq!(t.name, "x", "no filename comment");

        // An empty description is still a line.
        let t = Tuning::parse("!\n\n 1\n 2/1\n", "y").unwrap();
        assert_eq!(t.description, "");

        // Zero notes is allowed: the root alone.
        assert!(Tuning::parse("root only\n 0\n", "z")
            .unwrap()
            .cents
            .is_empty());
    }

    #[test]
    fn broken_scala_files_are_refused() {
        assert!(Tuning::parse("name\n 2\n 100.0\n", "x").is_err(), "too few");
        assert!(Tuning::parse("name\n x\n", "x").is_err(), "bad count");
        assert!(Tuning::parse("name\n", "x").is_err(), "no count");
        assert!(
            Tuning::parse("name\n 1\n -3/2\n", "x").is_err(),
            "negative ratio"
        );
    }

    #[test]
    fn ascl_directives_apply() {
        let t = Tuning::parse(ASCL, "x").unwrap();
        assert_eq!(t.name, "Make your own tuning file");
        assert_eq!(t.note_names.len(), 12);
        assert_eq!(t.note_names[9], "A");
        assert_eq!(
            t.reference,
            ScaleNote {
                octave: 3,
                degree: 9
            }
        );
        assert_eq!(t.reference_hz, 440.0);
    }

    #[test]
    fn a_twelve_tone_file_plays_like_nothing_was_loaded() {
        let t = Tuning::parse(ASCL, "x").unwrap();
        for k in 0..128u8 {
            close(t.key_hz(k), equal_hz(k));
        }
        assert!((0..128).all(|k| t.table().offset(k).abs() < 1e-3));
    }

    #[test]
    fn ascl_defaults_for_other_sizes_put_the_root_on_the_reference() {
        let t = edo(19);
        assert_eq!(
            t.reference,
            ScaleNote {
                octave: 3,
                degree: 0
            }
        );
        close(t.key_hz(60), 440.0);
        close(t.key_hz(60 + 19), 880.0);
        close(t.key_hz(60 - 19), 220.0);
        close(t.key_hz(61), 440.0 * 2f64.powf(1.0 / 19.0));
    }

    #[test]
    fn pseudo_octaves_repeat_at_their_own_interval() {
        // Bohlen-Pierce: 13 steps of the tritave.
        let t = Tuning::new(
            "BP",
            "",
            (1..=13).map(|i| i as f64 * 1901.955 / 13.0).collect(),
        );
        close(t.key_hz(60), 440.0);
        close(t.key_hz(73), 1320.0);
    }

    #[test]
    fn saved_ascl_reads_back_identically() {
        let mut t = Tuning::parse(ASCL, "x").unwrap();
        t.reference_hz = 432.0;
        t.source = "A book".into();
        t.link = "https://example.test/t".into();
        let back = Tuning::parse(&t.to_ascl(), "x").unwrap();
        for (a, b) in t.cents.iter().zip(&back.cents) {
            close(*a, *b);
        }
        assert_eq!(back.name, t.name);
        assert_eq!(back.note_names, t.note_names);
        assert_eq!(back.reference, t.reference);
        assert_eq!(back.reference_hz, 432.0);
        assert_eq!(back.lowest, t.lowest);
        assert_eq!(back.source, t.source);
        assert_eq!(back.link, t.link);
    }

    #[test]
    fn changing_the_reference_note_changes_nothing_you_can_hear() {
        let mut t = edo(19);
        let before: Vec<f64> = (0..128).map(|k| t.key_hz(k)).collect();
        t.set_reference_note(ScaleNote {
            octave: 4,
            degree: 2,
        });
        for k in 0..128u8 {
            close(t.key_hz(k), before[k as usize]);
        }
        let now = t.reference_hz;
        t.reference_hz = now * 2.0;
        close(t.key_hz(60), before[60] * 2.0);
    }

    #[test]
    fn lowest_and_highest_move_together() {
        let mut t = edo(19);
        let span = t.position_of(t.highest()) - t.position_of(t.lowest);
        assert_eq!(span, 127);
        let hz_at_key_0 = t.key_hz(0);
        t.lowest = ScaleNote {
            octave: t.lowest.octave + 1,
            degree: t.lowest.degree,
        };
        assert_eq!(t.position_of(t.highest()) - t.position_of(t.lowest), 127);
        close(t.key_hz(0), hz_at_key_0 * 2.0);

        let target = ScaleNote {
            octave: 8,
            degree: 3,
        };
        t.set_highest(target);
        assert_eq!(t.highest(), target);
        assert_eq!(t.position_of(t.highest()) - t.position_of(t.lowest), 127);
    }

    #[test]
    fn the_ruler_names_notes_from_the_file_or_by_number() {
        let t = Tuning::parse(ASCL, "x").unwrap();
        assert_eq!(t.key_label(69), "A3");
        assert_eq!(t.key_label(60), "C3");
        let e = edo(19);
        assert_eq!(e.key_label(60), "1.3");
        assert_eq!(e.key_label(62), "3.3");
    }

    #[test]
    fn factory_tunings_are_sound_and_save_as_ascl() {
        let all = factory_tunings();
        assert!(all.len() >= 8);
        for t in &all {
            let mut c = t.cents.clone();
            c.dedup();
            assert_eq!(c.len(), t.cents.len(), "{}", t.name);
            assert!(c.windows(2).all(|w| w[1] > w[0]), "{} rises", t.name);
            let back = Tuning::parse(&t.to_ascl(), "x").unwrap();
            assert_eq!(back.steps(), t.steps());
            assert!((back.period() - t.period()).abs() < 1e-3, "{}", t.name);
        }
    }

    #[test]
    fn the_summary_names_the_size_of_the_scale() {
        assert!(edo(31).summary().contains("31 notes"));
    }

    #[test]
    fn layouts_map_keys_as_the_manual_says() {
        let t = edo(19);
        assert!(TuningLayout::All.keys(&t).is_none());

        let white = TuningLayout::White.keys(&t).unwrap();
        assert_eq!(white.map(60), Some(60), "centred on C3");
        assert_eq!(white.map(62), Some(61));
        assert_eq!(white.map(64), Some(62));
        assert_eq!(white.map(59), Some(59));
        assert_eq!(white.map(61), None, "black keys are off");

        let black = TuningLayout::Black.keys(&t).unwrap();
        assert_eq!(black.map(61), Some(60), "centred on C#3");
        assert_eq!(black.map(63), Some(61));
        assert_eq!(black.map(66), Some(62));
        assert_eq!(black.map(58), Some(59));
        assert_eq!(black.map(60), None, "white keys are off");
    }

    #[test]
    fn closest_in_pitch_finds_the_nearest_note() {
        let t = edo(24);
        let map = TuningLayout::Closest.keys(&t).unwrap();
        for k in 40..80u8 {
            let to = map.map(k).unwrap();
            let want = nearest_key(equal_hz(k), |c| t.key_hz(c));
            assert_eq!(to, want);
            let error = 1200.0 * (t.key_hz(to) / equal_hz(k)).log2();
            assert!(error.abs() <= 25.1, "key {k} is {error} cents off");
        }
    }

    #[test]
    fn a_custom_layout_is_saved_and_applied() {
        let layout = TuningLayout::Custom(CustomLayout {
            anchor: 48,
            note: 72,
            keys: WHITE,
        });
        let text = ron::to_string(&layout).unwrap();
        assert_eq!(ron::from_str::<TuningLayout>(&text).unwrap(), layout);
        let keys = layout.keys(&edo(19)).unwrap();
        assert_eq!(keys.map(48), Some(72));
        assert_eq!(keys.map(50), Some(73));
    }

    fn note(start: u64, length: u64, key: u8) -> NoteData {
        NoteData::new(start, length, key, 0.5)
    }

    #[test]
    fn retuning_moves_notes_to_the_nearest_pitch() {
        let t = edo(24);
        let mut notes = vec![note(0, 100, 60), note(100, 100, 61)];
        retune_notes(&mut notes, equal_hz, |k| t.key_hz(k));
        for n in &notes {
            let was = if n.start == 0 { 60 } else { 61 };
            let cents = 1200.0 * (t.key_hz(n.key) / equal_hz(was)).log2();
            assert!(cents.abs() < 25.1, "{cents}");
        }
    }

    #[test]
    fn retuning_shortens_or_drops_notes_that_land_together() {
        // Everything lands on the one key this "tuning" has that isn't far away.
        let only = |k: u8| if k == 70 { 500.0 } else { 1.0 };
        let mut notes = vec![note(0, 200, 60), note(100, 100, 61)];
        retune_notes(&mut notes, equal_hz, only);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].length, 100, "the earlier note is cut short");
        assert_eq!((notes[1].start, notes[1].length), (100, 100));

        let mut notes = vec![note(0, 100, 60), note(0, 100, 61)];
        retune_notes(&mut notes, equal_hz, only);
        assert_eq!(
            notes.len(),
            1,
            "two notes on one key at one moment can't both stay"
        );
    }
}
