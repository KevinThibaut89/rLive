//! Arpeggiator: turns held notes into rhythmic patterns.

use super::common::*;
use crate::dsp::tempo::SyncRate;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, ScaleInfo, Section, Status,
    TransportInfo, ViewId, ViewWriter,
};

pub const STYLE: u32 = 0;
pub const SYNC: u32 = 1;
pub const RATE_MS: u32 = 2;
pub const RATE_SYNC: u32 = 3;
pub const GATE: u32 = 4;
pub const HOLD: u32 = 5;
pub const OFFSET: u32 = 6;
pub const DISTANCE: u32 = 7;
pub const STEPS: u32 = 8;
pub const ROOT: u32 = 9;
pub const SCALE: u32 = 10;
pub const USE_SCALE: u32 = 11;
pub const GROOVE: u32 = 12;
pub const RETRIGGER: u32 = 13;
pub const INTERVAL: u32 = 14;
pub const REPEATS: u32 = 15;
pub const VELOCITY_ON: u32 = 16;
pub const DECAY: u32 = 17;
pub const TARGET: u32 = 18;
pub const VELOCITY_RETRIGGER: u32 = 19;

pub const LED: ViewId = ViewId(0);
pub const LED_COUNT: usize = 1;

const MAX_HELD: usize = 32;
const MAX_PATTERN: usize = 128;
/// Repeats at this menu position plays forever.
const INFINITE_REPEATS: usize = 36;
const INTERVAL_LABELS: [&str; 8] = [
    "1/16", "1/8", "1/4", "1/2", "1 Bar", "2 Bars", "4 Bars", "8 Bars",
];
const INTERVAL_BEATS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
/// How far groove delays off-grid steps, as a fraction of the grid.
const SWING: f64 = 1.0 / 3.0;
/// Notes this soon after the first one count as the same chord.
const CHORD_GRACE_MS: f64 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Up,
    Down,
    UpDown,
    DownUp,
    UpAndDown,
    DownAndUp,
    Converge,
    Diverge,
    ConAndDiverge,
    PinkyUp,
    PinkyUpDown,
    ThumbUp,
    ThumbUpDown,
    PlayOrder,
    ChordTrigger,
    Random,
    RandomOther,
    RandomOnce,
}

impl Style {
    const ALL: [Style; 18] = [
        Style::Up,
        Style::Down,
        Style::UpDown,
        Style::DownUp,
        Style::UpAndDown,
        Style::DownAndUp,
        Style::Converge,
        Style::Diverge,
        Style::ConAndDiverge,
        Style::PinkyUp,
        Style::PinkyUpDown,
        Style::ThumbUp,
        Style::ThumbUpDown,
        Style::PlayOrder,
        Style::ChordTrigger,
        Style::Random,
        Style::RandomOther,
        Style::RandomOnce,
    ];

    pub const LABELS: [&'static str; 18] = [
        "Up",
        "Down",
        "UpDown",
        "DownUp",
        "Up & Down",
        "Down & Up",
        "Converge",
        "Diverge",
        "Con & Diverge",
        "Pinky Up",
        "Pinky UpDown",
        "Thumb Up",
        "Thumb UpDown",
        "Play Order",
        "Chord Trigger",
        "Random",
        "Random Other",
        "Random Once",
    ];

    fn from_index(index: f64) -> Self {
        Self::ALL[index.round().clamp(0.0, (Self::ALL.len() - 1) as f64) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Retrigger {
    #[default]
    Off,
    Note,
    Beat,
}

pub fn params() -> Vec<ParamInfo> {
    let scale_names: Vec<&str> = std::iter::once("Chromatic")
        .chain(SCALES.iter().map(|(n, _)| *n))
        .collect();
    let groove = ["None", "Swing 8", "Swing 16"];
    let repeats: Vec<String> = (1..=INFINITE_REPEATS)
        .map(|n| n.to_string())
        .chain(std::iter::once("\u{221e}".to_string()))
        .collect();
    let repeats: Vec<&str> = repeats.iter().map(String::as_str).collect();
    vec![
        ParamInfo::stepped(STYLE, "Style", &Style::LABELS, 0),
        ParamInfo::toggle(SYNC, "Sync", true),
        ParamInfo::skewed(RATE_MS, "Rate", 1.0, 10_000.0, 125.0, "ms", 3.0),
        ParamInfo::stepped(RATE_SYNC, "Sync Rate", &SyncRate::LABELS, 6),
        percent_param(GATE, "Gate", 1.0, 200.0, 50.0),
        ParamInfo::toggle(HOLD, "Hold", false),
        int_param(OFFSET, "Offset", 0.0, 16.0, 0.0),
        int_param(DISTANCE, "Distance", -36.0, 36.0, 12.0),
        int_param(STEPS, "Steps", 0.0, 8.0, 0.0),
        ParamInfo::stepped(
            ROOT,
            "Root",
            &[
                "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
            ],
            0,
        ),
        ParamInfo::stepped(SCALE, "Scale", &scale_names, 0),
        ParamInfo::toggle(USE_SCALE, "Use Current Scale", false),
        ParamInfo::stepped(GROOVE, "Groove", &groove, 0),
        ParamInfo::stepped(RETRIGGER, "Retrigger", &["Off", "Note", "Beat"], 0),
        ParamInfo::stepped(INTERVAL, "Interval", &INTERVAL_LABELS, 4),
        ParamInfo::stepped(REPEATS, "Repeats", &repeats, INFINITE_REPEATS),
        ParamInfo::toggle(VELOCITY_ON, "Velocity", false),
        ParamInfo::skewed(DECAY, "Decay", 1.0, 60_000.0, 2_000.0, "ms", 3.0),
        int_param(TARGET, "Target", 0.0, 127.0, 0.0),
        ParamInfo::toggle(VELOCITY_RETRIGGER, "Vel. Retrigger", false),
    ]
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "Pattern",
            items: vec![
                Item::Menu(id(STYLE)),
                Item::Toggle(id(HOLD)),
                Item::Knob(id(OFFSET)),
                Item::Menu(id(GROOVE)),
            ],
        },
        Section {
            title: "Rate",
            items: vec![
                Item::Toggle(id(SYNC)),
                Item::Knob(id(RATE_MS)),
                Item::Menu(id(RATE_SYNC)),
                Item::Knob(id(GATE)),
            ],
        },
        Section {
            title: "Transposition",
            items: vec![
                Item::Knob(id(DISTANCE)),
                Item::Knob(id(STEPS)),
                Item::Menu(id(ROOT)),
                Item::Menu(id(SCALE)),
                Item::Toggle(id(USE_SCALE)),
            ],
        },
        Section {
            title: "Retrigger",
            items: vec![
                Item::Menu(id(RETRIGGER)),
                Item::Menu(id(INTERVAL)),
                Item::Menu(id(REPEATS)),
                Item::Panel("arpeggiator_led"),
            ],
        },
        Section {
            title: "Velocity",
            items: vec![
                Item::Toggle(id(VELOCITY_ON)),
                Item::Knob(id(DECAY)),
                Item::Knob(id(TARGET)),
                Item::Toggle(id(VELOCITY_RETRIGGER)),
            ],
        },
    ]
}

#[derive(Clone, Copy)]
struct Held {
    note: NoteRef,
    velocity: f32,
    tuning: f32,
    /// In the order they were played.
    order: u32,
    /// The key is physically down, rather than held by Hold.
    down: bool,
}

pub struct Arpeggiator {
    core: Core,
    style: Style,
    synced: bool,
    rate_ms: f64,
    rate_sync: SyncRate,
    gate: f64,
    hold: bool,
    offset: usize,
    distance: i32,
    steps: usize,
    root: u8,
    scale: usize,
    use_scale: bool,
    groove: usize,
    retrigger: Retrigger,
    interval: usize,
    repeats: usize,
    velocity_on: bool,
    decay_ms: f64,
    target: f32,
    velocity_retrigger: bool,

    transposer: Transposer,
    beats_per_bar: f64,
    rng: Rng,

    held: Vec<Held>,
    next_order: u32,
    pattern: [u8; MAX_PATTERN],
    pattern_len: usize,
    dirty: bool,
    running: bool,
    next_step: u64,
    /// Which step of the pattern comes next.
    index: usize,
    /// Which transposition the pattern is on.
    pass: usize,
    repeats_done: usize,
    finished: bool,
    velocity_origin: u64,
    note_retrigger: bool,
    last_interval: i64,
    /// When the current run started, so late notes of the opening chord can join it.
    started_at: u64,
    led: f32,
}

impl Arpeggiator {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            core: Core::default(),
            style: Style::Up,
            synced: true,
            rate_ms: 125.0,
            rate_sync: SyncRate::from_index(6.0),
            gate: 0.5,
            hold: false,
            offset: 0,
            distance: 12,
            steps: 0,
            root: 0,
            scale: 0,
            use_scale: false,
            groove: 0,
            retrigger: Retrigger::Off,
            interval: 4,
            repeats: INFINITE_REPEATS,
            velocity_on: false,
            decay_ms: 2_000.0,
            target: 0.0,
            velocity_retrigger: false,
            transposer: Transposer::Chromatic,
            beats_per_bar: 4.0,
            rng: Rng::new(seed),
            held: Vec::with_capacity(MAX_HELD),
            next_order: 0,
            pattern: [0; MAX_PATTERN],
            pattern_len: 0,
            dirty: true,
            running: false,
            next_step: 0,
            index: 0,
            pass: 0,
            repeats_done: 0,
            finished: false,
            velocity_origin: 0,
            note_retrigger: false,
            last_interval: 0,
            started_at: 0,
            led: 0.0,
        }
    }

    fn physically_down(&self) -> usize {
        self.held.iter().filter(|h| h.down).count()
    }

    fn step_frames(&self) -> f64 {
        if self.synced {
            self.core.beats_to_frames(self.rate_sync.beats())
        } else {
            self.core.ms_to_frames(self.rate_ms)
        }
        .max(1.0)
    }

    fn interval_beats(&self) -> f64 {
        match self.interval {
            0..=3 => INTERVAL_BEATS[self.interval],
            i => INTERVAL_BEATS[i.min(7)] / 4.0 * self.beats_per_bar,
        }
    }

    fn interval_index(&self) -> i64 {
        (self.core.beat() / self.interval_beats() + 1e-6).floor() as i64
    }

    /// Lays the held notes out for a style, in terms of positions in `held`.
    fn rebuild_pattern(&mut self) {
        self.dirty = false;
        let n = self.held.len();
        self.pattern_len = 0;
        if n == 0 {
            return;
        }
        // A pattern's longest form is two passes of Pinky UpDown.
        let mut order = [0u8; MAX_HELD];
        for (i, slot) in order.iter_mut().enumerate().take(n) {
            *slot = i as u8;
        }
        let top = n - 1;
        let style = self.style;
        match style {
            Style::Up => self.extend(0..n),
            Style::Down => self.extend((0..n).rev()),
            Style::UpDown => {
                self.extend(0..n);
                if n > 2 {
                    self.extend((1..top).rev());
                }
            }
            Style::DownUp => {
                self.extend((0..n).rev());
                if n > 2 {
                    self.extend(1..top);
                }
            }
            Style::UpAndDown => {
                self.extend(0..n);
                self.extend((0..n).rev());
            }
            Style::DownAndUp => {
                self.extend((0..n).rev());
                self.extend(0..n);
            }
            Style::Converge | Style::Diverge | Style::ConAndDiverge => {
                self.push_converge();
                if style == Style::Diverge {
                    self.pattern[..self.pattern_len].reverse();
                } else if style == Style::ConAndDiverge && n > 1 {
                    let len = self.pattern_len;
                    for i in (0..len - 1).rev() {
                        self.push(self.pattern[i] as usize);
                    }
                }
            }
            Style::PinkyUp => {
                if n == 1 {
                    self.push(0);
                } else {
                    for i in 0..top {
                        self.push(i);
                        self.push(top);
                    }
                }
            }
            Style::PinkyUpDown => {
                if n == 1 {
                    self.push(0);
                } else {
                    for i in 0..top {
                        self.push(i);
                        self.push(top);
                    }
                    for i in (1..top).rev() {
                        self.push(i);
                        self.push(top);
                    }
                }
            }
            Style::ThumbUp => {
                if n == 1 {
                    self.push(0);
                } else {
                    for i in 1..n {
                        self.push(0);
                        self.push(i);
                    }
                }
            }
            Style::ThumbUpDown => {
                if n == 1 {
                    self.push(0);
                } else {
                    for i in 1..n {
                        self.push(0);
                        self.push(i);
                    }
                    for i in (1..top).rev() {
                        self.push(0);
                        self.push(i);
                    }
                }
            }
            Style::PlayOrder => {
                let held = &self.held;
                order[..n].sort_unstable_by_key(|&i| held[i as usize].order);
                self.extend_from(&order[..n]);
            }
            Style::ChordTrigger => self.push(0),
            Style::Random => self.extend(0..n),
            Style::RandomOther | Style::RandomOnce => {
                self.extend(0..n);
                self.shuffle_pattern();
            }
        }
    }

    fn extend(&mut self, iter: impl Iterator<Item = usize>) {
        iter.for_each(|i| self.push(i));
    }

    fn extend_from(&mut self, items: &[u8]) {
        items.iter().for_each(|i| self.push(*i as usize));
    }

    fn push(&mut self, position: usize) {
        if self.pattern_len < MAX_PATTERN {
            self.pattern[self.pattern_len] = position as u8;
            self.pattern_len += 1;
        }
    }

    fn push_converge(&mut self) {
        let (mut low, mut high) = (0usize, self.held.len() - 1);
        loop {
            self.push(low);
            if low == high {
                break;
            }
            self.push(high);
            low += 1;
            if low > high || high == 0 {
                break;
            }
            high -= 1;
            if low > high {
                break;
            }
        }
    }

    fn shuffle_pattern(&mut self) {
        for i in (1..self.pattern_len).rev() {
            let j = self.rng.below(i + 1);
            self.pattern.swap(i, j);
        }
    }

    /// The velocity a held note plays at right now.
    fn velocity_for(&self, velocity: f32) -> f32 {
        if !self.velocity_on {
            return velocity;
        }
        let elapsed =
            (self.core.now() - self.velocity_origin) as f64 * 1000.0 / self.core.sample_rate();
        let amount = (elapsed / self.decay_ms.max(0.001)).min(1.0) as f32;
        velocity + (self.target / 127.0 - velocity) * amount
    }

    /// Frames the groove pushes the step about to play.
    fn groove_delay(&self) -> u64 {
        let unit = match self.groove {
            1 => 0.5,
            2 => 0.25,
            _ => return 0,
        };
        if !self.synced || !self.core.playing() {
            return 0;
        }
        let units = (self.core.beat() / unit + 1e-6).floor() as i64;
        if units.rem_euclid(2) == 1 {
            self.core.beats_to_frames(unit * SWING).round() as u64
        } else {
            0
        }
    }

    fn play(&mut self, position: usize, swing: u64, length: u64) {
        let h = self.held[position];
        let amount = self.pass as i32 * self.distance;
        let key = self.transposer.apply(h.note.key as i32, amount);
        if !(0..=127).contains(&key) {
            return;
        }
        let out = NoteRef {
            channel: h.note.channel,
            key: key as u8,
            id: device_note_id(),
        };
        let velocity = self.velocity_for(h.velocity);
        self.core
            .note_on_after(swing, h.note, out, velocity, h.tuning);
        self.core.note_off_after(swing + length, out, 0.0);
    }

    /// Plays whatever this step of the pattern calls for.
    fn play_step(&mut self) {
        let n = self.held.len();
        if n == 0 || self.pattern_len == 0 {
            return;
        }
        let swing = self.groove_delay();
        let length = ((self.step_frames() * self.gate).round() as u64).max(1);
        match self.style {
            Style::ChordTrigger => {
                for position in 0..n {
                    self.play(position, swing, length);
                }
            }
            Style::Random => {
                let position = self.rng.below(n);
                self.play(position, swing, length);
            }
            _ => {
                let at = (self.index + self.offset) % self.pattern_len;
                let position = (self.pattern[at] as usize).min(n - 1);
                self.play(position, swing, length);
            }
        }
    }

    fn move_on(&mut self) {
        self.index += 1;
        if self.index < self.pattern_len.max(1) {
            return;
        }
        self.index = 0;
        if self.style == Style::RandomOther {
            self.shuffle_pattern();
        }
        self.pass += 1;
        if self.pass > self.steps {
            self.pass = 0;
            self.repeats_done += 1;
            if self.repeats < INFINITE_REPEATS && self.repeats_done > self.repeats {
                self.finished = true;
            }
        }
    }

    fn schedule_next(&mut self) {
        let now = self.core.now();
        let frames = self.step_frames();
        let wait = if self.synced && self.core.playing() {
            // Line up with the song grid. A step just played off the grid doesn't get a
            // second step straight after it: the next grid line must be at least half a
            // step away.
            let grid = self.rate_sync.beats();
            let beat = self.core.beat();
            let mut line = ((beat / grid + 1e-9).floor() + 1.0) * grid;
            if (line - beat) / grid < 0.5 {
                line += grid;
            }
            ((line - beat) / self.core.beats_per_frame()).round()
        } else {
            frames.round()
        };
        self.next_step = now + (wait as u64).max(1);
    }

    fn step(&mut self) {
        if self.dirty {
            self.rebuild_pattern();
            if self.pattern_len > 0 {
                self.index %= self.pattern_len;
            } else {
                self.index = 0;
            }
        }
        let mut retrigger = false;
        match self.retrigger {
            Retrigger::Off => self.note_retrigger = false,
            Retrigger::Note => {
                retrigger = self.note_retrigger;
                self.note_retrigger = false;
            }
            Retrigger::Beat => {
                self.note_retrigger = false;
                let interval = self.interval_index();
                if interval != self.last_interval {
                    self.last_interval = interval;
                    retrigger = true;
                }
            }
        }
        if retrigger {
            self.index = 0;
            self.pass = 0;
            self.repeats_done = 0;
            self.finished = false;
            self.led = 1.0;
            if self.velocity_retrigger {
                self.velocity_origin = self.core.now();
            }
        }
        if !self.finished {
            self.play_step();
            self.move_on();
        }
        self.schedule_next();
    }

    fn start(&mut self) {
        self.running = true;
        self.index = 0;
        self.pass = 0;
        self.repeats_done = 0;
        self.finished = false;
        self.next_step = self.core.now();
        self.velocity_origin = self.core.now();
        self.started_at = self.core.now();
        self.last_interval = self.interval_index();
        self.note_retrigger = false;
    }

    fn note_on(&mut self, src: NoteRef, velocity: f32, tuning: f32) {
        if self.hold {
            if self.physically_down() == 0 {
                // A fresh gesture replaces whatever Hold was keeping.
                self.held.clear();
            } else if let Some(i) = self
                .held
                .iter()
                .position(|h| !h.down && h.note.key == src.key)
            {
                // Playing a held note again takes it out.
                self.held.remove(i);
                self.dirty = true;
                if self.held.is_empty() {
                    self.running = false;
                }
                return;
            }
        }
        if self.held.len() >= MAX_HELD {
            return;
        }
        let at = self
            .held
            .iter()
            .position(|h| h.note.key > src.key)
            .unwrap_or(self.held.len());
        self.held.insert(
            at,
            Held {
                note: src,
                velocity,
                tuning,
                order: self.next_order,
                down: true,
            },
        );
        self.next_order = self.next_order.wrapping_add(1);
        self.dirty = true;
        if self.running {
            self.note_retrigger = true;
            let elapsed =
                (self.core.now() - self.started_at) as f64 * 1000.0 / self.core.sample_rate();
            // The rest of a chord played by hand, a moment after the first key. The note it
            // started with has already played: the next step is the one after it.
            if self.index == 0 && self.next_step > self.core.now() && elapsed < CHORD_GRACE_MS {
                self.index = 1;
            }
        } else {
            self.start();
        }
    }

    fn note_off(&mut self, src: NoteRef, message: MidiMessage) {
        let Some(i) = self.held.iter().position(|h| h.down && h.note.is(src)) else {
            // Hold keeps a released note around under its old ref.
            if !self.held.iter().any(|h| h.note.is(src)) {
                self.core.forward(message);
            }
            return;
        };
        if self.hold {
            self.held[i].down = false;
        } else {
            self.held.remove(i);
            self.dirty = true;
            if self.held.is_empty() {
                self.running = false;
            }
        }
    }

    fn set_hold(&mut self, hold: bool) {
        if self.hold && !hold {
            self.held.retain(|h| h.down);
            self.dirty = true;
            if self.held.is_empty() {
                self.running = false;
            }
        }
        self.hold = hold;
    }

    fn resolve_transposer(&self, transport: &TransportInfo) -> Transposer {
        match Transposer::resolve(self.use_scale, transport) {
            Transposer::Scale(s) => Transposer::Scale(s),
            Transposer::Chromatic if self.scale > 0 => Transposer::Scale(ScaleInfo {
                root: self.root,
                mask: SCALES[(self.scale - 1).min(SCALES.len() - 1)].1,
            }),
            Transposer::Chromatic => Transposer::Chromatic,
        }
    }
}

impl MidiFx for Arpeggiator {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            STYLE => {
                self.style = Style::from_index(value);
                self.dirty = true;
            }
            SYNC => self.synced = value >= 0.5,
            RATE_MS => self.rate_ms = value,
            RATE_SYNC => self.rate_sync = SyncRate::from_index(value),
            GATE => self.gate = value * 0.01,
            HOLD => self.set_hold(value >= 0.5),
            OFFSET => self.offset = value.round() as usize,
            DISTANCE => self.distance = value.round() as i32,
            STEPS => self.steps = value.round() as usize,
            ROOT => self.root = value as u8 % 12,
            SCALE => self.scale = value.round() as usize,
            USE_SCALE => self.use_scale = value >= 0.5,
            GROOVE => self.groove = value.round() as usize,
            RETRIGGER => {
                self.retrigger = match value.round() as i32 {
                    1 => Retrigger::Note,
                    2 => Retrigger::Beat,
                    _ => Retrigger::Off,
                }
            }
            INTERVAL => self.interval = value.round() as usize,
            // The menu's first entry is one repeat.
            REPEATS => self.repeats = value.round() as usize,
            VELOCITY_ON => self.velocity_on = value >= 0.5,
            DECAY => self.decay_ms = value,
            TARGET => self.target = value as f32,
            VELOCITY_RETRIGGER => self.velocity_retrigger = value >= 0.5,
            _ => {}
        }
    }

    fn begin(&mut self, transport: &TransportInfo) {
        self.transposer = self.resolve_transposer(transport);
        self.beats_per_bar = transport.time_signature.numerator as f64 * 4.0
            / transport.time_signature.denominator as f64;
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                note_id,
                tuning,
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                self.note_on(src, velocity, tuning);
            }
            MidiMessage::NoteOff {
                channel,
                key,
                note_id,
                ..
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                self.note_off(src, message);
            }
            MidiMessage::NoteExpr { .. } | MidiMessage::AllNotesOff => {
                if message == MidiMessage::AllNotesOff {
                    self.held.clear();
                    self.running = false;
                }
                self.core.pass_other(message);
            }
            other => self.core.forward(other),
        }
    }

    fn advance(&mut self, frames: usize) {
        let end = self.core.now() + frames as u64;
        while self.running && self.next_step < end {
            let due = self.next_step.saturating_sub(self.core.now());
            if due > 0 {
                self.core.advance(due);
            }
            self.step();
            if !self.running {
                break;
            }
        }
        let left = end - self.core.now();
        if left > 0 {
            self.core.advance(left);
        }
    }

    fn clear(&mut self) {
        self.held.clear();
        self.running = false;
        self.dirty = true;
        self.index = 0;
        self.pass = 0;
    }

    fn end(&mut self, _mod_out: &mut [f32], view: &ViewWriter) {
        if view.is_active() {
            view.publish(LED, &[self.led]);
        }
        self.led *= 0.85;
    }
}

midi_fx_processor!(Arpeggiator);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.arpeggiator",
    name: "Arpeggiator",
    role: DeviceRole::MidiEffect,
    category: "MIDI Effects",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            midi_out: true,
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Arpeggiator::with_seed(entropy_seed())),
};
