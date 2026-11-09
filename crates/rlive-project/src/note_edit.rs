//! Edits of a clip's notes as pure functions (manual ch. 10): what overlaps, what snaps, and what
//! Split, Chop, Join, Note Stretch, Velocity and Chance do to a list of notes.
//!
//! Indices name notes in the list given. A function that adds or removes notes returns where the
//! ones it was asked about ended up, so the caller's selection can follow.

use crate::clip_edit::chosen;
use crate::{Breakpoint, ClipEnvelope, NoteData, NoteExpression, NoteGroup};

/// The softest a note can be: velocity 1 of 127.
pub const MIN_VELOCITY: f32 = 1.0 / 127.0;

/// One step of the velocity scale.
pub const VELOCITY_STEP: f32 = 1.0 / 127.0;

// ------------------------------------------------------------- placing notes

/// Settles notes placed on top of others: a note whose start is covered goes, and one whose end
/// is covered is shortened (manual p. 262). Returns where the `winners` are now.
pub fn resolve_overlaps(notes: &mut Vec<NoteData>, winners: &[usize]) -> Vec<usize> {
    let mut gone = vec![false; notes.len()];
    for &w in winners {
        let Some(new) = notes.get(w).cloned() else {
            continue;
        };
        let end = new.start + new.length;
        for (i, other) in notes.iter_mut().enumerate() {
            if winners.contains(&i) || other.key != new.key {
                continue;
            }
            let other_end = other.start + other.length;
            if other.start >= new.start && other.start < end {
                gone[i] = true;
            } else if other.start < new.start && other_end > new.start {
                other.length = new.start - other.start;
            }
        }
    }
    let mut moved = vec![None; notes.len()];
    let mut kept = 0;
    for (i, g) in gone.iter().enumerate() {
        if !g {
            moved[i] = Some(kept);
            kept += 1;
        }
    }
    let mut i = 0;
    notes.retain(|_| {
        i += 1;
        !gone[i - 1]
    });
    winners
        .iter()
        .filter_map(|w| moved.get(*w).copied().flatten())
        .collect()
}

/// The grid position a drag has reached, in ticks moved from `origin`.
///
/// The grid is magnetic: a note away from a grid line moves freely up to the first one it meets,
/// and from there it goes from line to line, or to a line plus the note's own offset from the
/// grid, which keeps a loose feel loose (manual p. 256). A note on a line has met one already.
pub fn magnetic_delta(origin: u64, raw: f64, grid: u64) -> i64 {
    if grid == 0 {
        return raw.round() as i64;
    }
    let g = grid as i64;
    let start = origin as i64;
    let offset = start.rem_euclid(g);
    let target = origin as f64 + raw;
    if offset != 0 {
        let line = if raw >= 0.0 {
            start - offset + g
        } else {
            start - offset
        };
        let reached = if raw >= 0.0 {
            target >= line as f64
        } else {
            target <= line as f64
        };
        if !reached {
            return raw.round() as i64;
        }
    }
    let k = (target / grid as f64).floor() as i64;
    let mut best = k * g;
    for candidate in [k * g, k * g + offset, (k + 1) * g, (k + 1) * g + offset] {
        if (candidate as f64 - target).abs() < (best as f64 - target).abs() {
            best = candidate;
        }
    }
    best - start
}

// ------------------------------------------------------------- split, chop, join

fn cut_points(points: &[Breakpoint], at: f64) -> (Vec<Breakpoint>, Vec<Breakpoint>) {
    let left = points.iter().filter(|p| p.time < at).cloned().collect();
    let right = points
        .iter()
        .filter(|p| p.time >= at)
        .cloned()
        .map(|mut p| {
            p.time -= at;
            p
        })
        .collect();
    (left, right)
}

/// Divides a note in two at `at`, the expression going with whichever half it's under.
fn split_note(n: &NoteData, at: u64) -> Option<(NoteData, NoteData)> {
    if at <= n.start || at >= n.start + n.length {
        return None;
    }
    let mut left = n.clone();
    let mut right = n.clone();
    left.length = at - n.start;
    right.start = at;
    right.length = n.start + n.length - at;
    if let Some(expr) = &n.expr {
        let cut = left.length as f64;
        let (pitch_l, pitch_r) = cut_points(&expr.pitch, cut);
        let (slide_l, slide_r) = cut_points(&expr.slide, cut);
        let (pressure_l, pressure_r) = cut_points(&expr.pressure, cut);
        left.expr = Some(Box::new(NoteExpression {
            pitch: pitch_l,
            slide: slide_l,
            pressure: pressure_l,
        }));
        right.expr = Some(Box::new(NoteExpression {
            pitch: pitch_r,
            slide: slide_r,
            pressure: pressure_r,
        }));
    }
    Some((left, right))
}

/// Notes sounding at `at`, not counting one that starts there: what Split acts on with nothing
/// selected (manual p. 265).
pub fn notes_crossing(notes: &[NoteData], at: u64) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.start < at && n.start + n.length > at)
        .map(|(i, _)| i)
        .collect()
}

/// Splits each of `indices` at `at`. Returns where the two halves of each one are.
pub fn split_at(notes: &mut Vec<NoteData>, indices: &[usize], at: u64) -> Vec<usize> {
    let mut made = Vec::new();
    for &i in indices {
        if let Some((left, right)) = notes.get(i).and_then(|n| split_note(n, at)) {
            notes[i] = left;
            notes.push(right);
            made.push(i);
            made.push(notes.len() - 1);
        }
    }
    made
}

/// Divides each of `indices` at every point in `cuts(note)`. Returns all the pieces.
fn divide(
    notes: &mut Vec<NoteData>,
    indices: &[usize],
    cuts: impl Fn(&NoteData) -> Vec<u64>,
) -> Vec<usize> {
    let mut pieces = Vec::new();
    for &i in indices {
        let Some(note) = notes.get(i).cloned() else {
            continue;
        };
        let mut rest = i;
        pieces.push(i);
        let mut current = note.clone();
        for at in cuts(&note) {
            let Some((left, right)) = split_note(&current, at) else {
                continue;
            };
            notes[rest] = left;
            notes.push(right.clone());
            rest = notes.len() - 1;
            pieces.push(rest);
            current = right;
        }
    }
    pieces
}

/// Chops notes into parts on the grid lines they cross (manual p. 266).
pub fn chop_on_grid(notes: &mut Vec<NoteData>, indices: &[usize], grid: u64) -> Vec<usize> {
    let grid = grid.max(1);
    divide(notes, indices, |n| {
        let end = n.start + n.length;
        let mut at = (n.start / grid + 1) * grid;
        let mut cuts = Vec::new();
        while at < end {
            cuts.push(at);
            at += grid;
        }
        cuts
    })
}

/// How many parts a note is chopped into on the grid.
pub fn parts_on_grid(n: &NoteData, grid: u64) -> usize {
    let grid = grid.max(1);
    let end = n.start + n.length;
    (end.saturating_sub(1) / grid - n.start / grid + 1) as usize
}

/// Chops each note into `parts` equal parts.
pub fn chop_into(notes: &mut Vec<NoteData>, indices: &[usize], parts: usize) -> Vec<usize> {
    let parts = parts.max(1) as u64;
    divide(notes, indices, |n| {
        (1..parts).map(|p| n.start + n.length * p / parts).collect()
    })
}

/// A step through chop counts: by one, or by a power of two with Shift.
pub fn next_parts(parts: usize, up: bool, power: bool) -> usize {
    match (up, power) {
        (true, false) => parts + 1,
        (false, false) => parts.saturating_sub(1).max(1),
        (true, true) if parts.is_power_of_two() => parts * 2,
        (true, true) => parts.next_power_of_two(),
        (false, true) => (parts / 2).max(1),
    }
}

fn shifted(points: &[Breakpoint], by: f64) -> impl Iterator<Item = Breakpoint> + '_ {
    points.iter().cloned().map(move |mut p| {
        p.time += by;
        p
    })
}

/// Makes one note of the chosen notes of each pitch, from the earliest start to the latest end,
/// with their expression joined up (manual p. 267). Returns where the joined notes are.
pub fn join(notes: &mut Vec<NoteData>, indices: &[usize]) -> Vec<usize> {
    let mut keys: Vec<u8> = indices
        .iter()
        .filter_map(|i| notes.get(*i).map(|n| n.key))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let mut gone = vec![false; notes.len()];
    let mut joined = Vec::new();
    for key in keys {
        let mut members: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|i| notes.get(*i).is_some_and(|n| n.key == key))
            .collect();
        members.sort_by_key(|i| notes[*i].start);
        let Some((&head, tail)) = members.split_first() else {
            continue;
        };
        let start = notes[head].start;
        let end = members
            .iter()
            .map(|i| notes[*i].start + notes[*i].length)
            .max()
            .unwrap_or(start);
        let mut expr = notes[head].expr.clone();
        for &i in tail {
            gone[i] = true;
            let Some(more) = notes[i].expr.clone() else {
                continue;
            };
            let by = (notes[i].start - start) as f64;
            let e = expr.get_or_insert_with(Default::default);
            e.pitch.extend(shifted(&more.pitch, by));
            e.slide.extend(shifted(&more.slide, by));
            e.pressure.extend(shifted(&more.pressure, by));
        }
        notes[head].length = end - start;
        notes[head].expr = expr;
        joined.push(head);
    }
    let mut new_index = vec![0; notes.len()];
    let mut kept = 0;
    for (i, g) in gone.iter().enumerate() {
        new_index[i] = kept;
        kept += usize::from(!g);
    }
    let mut i = 0;
    notes.retain(|_| {
        i += 1;
        !gone[i - 1]
    });
    joined.iter().map(|i| new_index[*i]).collect()
}

/// Every chosen note starts and ends with the time range (manual p. 263).
pub fn fit_to_range(notes: &mut [NoteData], selected: &[usize], from: u64, to: u64) {
    for i in chosen(notes, selected) {
        notes[i].start = from;
        notes[i].length = to.saturating_sub(from).max(1);
    }
}

/// The range a set of notes covers.
pub fn span_of(notes: &[NoteData], indices: &[usize]) -> Option<(u64, u64)> {
    let chosen = || indices.iter().filter_map(|i| notes.get(*i));
    Some((
        chosen().map(|n| n.start).min()?,
        chosen().map(|n| n.start + n.length).max()?,
    ))
}

// ------------------------------------------------------------- note stretch

/// What dragging a Note Stretch marker does to time: `from..to` becomes `new_from..new_to`, which
/// is backwards if a marker has crossed the other. With `mid` the pseudo marker is what moved, and
/// the two sides of it stretch against the fixed ends (manual p. 264).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeMap {
    pub from: f64,
    pub to: f64,
    pub new_from: f64,
    pub new_to: f64,
    /// The pseudo marker: where it was and where it is.
    pub mid: Option<(f64, f64)>,
}

impl TimeMap {
    pub fn at(&self, t: f64) -> f64 {
        let lerp = |a: f64, b: f64, c: f64, d: f64| {
            if (b - a).abs() < f64::EPSILON {
                c
            } else {
                c + (t - a) / (b - a) * (d - c)
            }
        };
        match self.mid {
            Some((was, is)) if t <= was => lerp(self.from, was, self.new_from, is),
            Some((was, is)) => lerp(was, self.to, is, self.new_to),
            None => lerp(self.from, self.to, self.new_from, self.new_to),
        }
    }

    /// Whether time runs backwards: a marker has been dragged past the other.
    pub fn mirrored(&self) -> bool {
        self.new_to < self.new_from
    }
}

/// Scales a note's expression in time, as Note Stretch and the x2 and /2 buttons do to the note
/// (manual p. 335).
pub fn stretch_expression(n: &mut NoteData, factor: f64) {
    let Some(e) = n.expr.as_deref_mut() else {
        return;
    };
    for points in [&mut e.pitch, &mut e.slide, &mut e.pressure] {
        for p in points.iter_mut() {
            p.time *= factor;
        }
    }
}

/// Turns a note's expression back to front, for a note whose end has become its start. Each
/// segment keeps its shape, mirrored.
fn mirror_expression(n: &mut NoteData) {
    let length = n.length as f64;
    let Some(e) = n.expr.as_deref_mut() else {
        return;
    };
    for points in [&mut e.pitch, &mut e.slide, &mut e.pressure] {
        let curves: Vec<f32> = points.iter().map(|p| -p.curve).collect();
        points.reverse();
        let count = points.len();
        for (i, p) in points.iter_mut().enumerate() {
            p.time = length - p.time;
            p.curve = if i + 1 < count {
                curves[count - 2 - i]
            } else {
                0.0
            };
        }
    }
}

/// Moves the `originals` as `map` says, by their starts and ends, so each keeps its proportion. A
/// mirrored note's end becomes its start. Expression goes with the note.
pub fn apply_stretch(notes: &mut [NoteData], originals: &[(usize, NoteData)], map: TimeMap) {
    for (i, orig) in originals {
        let Some(n) = notes.get_mut(*i) else {
            continue;
        };
        let a = map.at(orig.start as f64);
        let b = map.at((orig.start + orig.length) as f64);
        let mirrored = b < a;
        let (a, b) = if mirrored { (b, a) } else { (a, b) };
        n.start = a.round().max(0.0) as u64;
        n.length = ((b - a).round() as u64).max(1);
        n.expr.clone_from(&orig.expr);
        if mirrored {
            let keep = n.length;
            n.length = orig.length;
            mirror_expression(n);
            n.length = keep;
        }
        stretch_expression(n, n.length as f64 / orig.length as f64);
    }
}

/// Linked clip envelopes follow a Note Stretch; unlinked ones keep their own time (manual p. 264).
pub fn stretch_envelopes(envelopes: &mut [ClipEnvelope], before: &[ClipEnvelope], map: TimeMap) {
    for (e, was) in envelopes.iter_mut().zip(before) {
        if e.loop_region.is_some() {
            continue;
        }
        e.envelope.points = was
            .envelope
            .points
            .iter()
            .cloned()
            .map(|mut p| {
                if p.time >= map.from && p.time <= map.to {
                    p.time = map.at(p.time).max(0.0);
                }
                p
            })
            .collect();
        e.envelope.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    }
}

// ------------------------------------------------------------- deactivation, quantize

/// Deactivates the notes, or reactivates them if they're all deactivated (manual p. 264).
pub fn toggle_active(notes: &mut [NoteData], selected: &[usize]) {
    let set = chosen(notes, selected);
    let mute = set.iter().any(|i| !notes[*i].muted);
    for i in set {
        notes[i].muted = mute;
    }
}

/// The Quantize settings, which the command uses too (manual p. 277).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantize {
    pub grid: u64,
    pub amount: f64,
    pub starts: bool,
    pub ends: bool,
}

/// Moves notes `amount` of the way to the grid.
pub fn quantize(notes: &mut [NoteData], selected: &[usize], q: Quantize) {
    let grid = q.grid.max(1) as f64;
    let amount = q.amount.clamp(0.0, 1.0);
    let toward = |t: u64| {
        let t = t as f64;
        (t + ((t / grid).round() * grid - t) * amount)
            .round()
            .max(0.0) as u64
    };
    for i in chosen(notes, selected) {
        let n = &mut notes[i];
        let end = n.start + n.length;
        if q.starts {
            n.start = toward(n.start);
        }
        let end = if q.ends { toward(end) } else { end };
        n.length = end.saturating_sub(n.start).max(1);
    }
}

// ------------------------------------------------------------- velocity

/// Which value a lane edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneValue {
    Velocity,
    Release,
    Chance,
}

impl LaneValue {
    /// The value's place on the lane, 0..1.
    pub fn get(self, n: &NoteData) -> f32 {
        match self {
            Self::Velocity => n.velocity,
            Self::Release => n.release,
            Self::Chance => n.probability,
        }
    }

    fn put(self, n: &mut NoteData, v: f32) {
        match self {
            Self::Velocity => n.velocity = v.clamp(MIN_VELOCITY, 1.0),
            Self::Release => n.release = v.clamp(0.0, 1.0),
            Self::Chance => n.probability = v.clamp(0.0, 1.0),
        }
    }
}

/// Sets a lane's value on the chosen notes. A group's notes share their chance.
pub fn set_value(notes: &mut [NoteData], selected: &[usize], what: LaneValue, v: f32) {
    let set = chosen(notes, selected);
    for &i in &set {
        what.put(&mut notes[i], v);
    }
    if what == LaneValue::Chance {
        spread_group_chance(notes, &set);
    }
}

/// Every note in a group the chosen notes are in takes on its chance.
fn spread_group_chance(notes: &mut [NoteData], set: &[usize]) {
    for &i in set {
        let Some(group) = notes[i].group else {
            continue;
        };
        let p = notes[i].probability;
        for n in notes.iter_mut().filter(|n| n.group == Some(group)) {
            n.probability = p;
        }
    }
}

/// Adds `by` to a lane's value on the chosen notes.
pub fn change_value(notes: &mut [NoteData], selected: &[usize], what: LaneValue, by: f32) {
    let set = chosen(notes, selected);
    for &i in &set {
        let v = what.get(&notes[i]) + by;
        what.put(&mut notes[i], v);
    }
    if what == LaneValue::Chance {
        spread_group_chance(notes, &set);
    }
}

/// Velocities go up or down by up to `amount`, chosen at random. `random` gives -1..1.
pub fn randomize_velocity(
    notes: &mut [NoteData],
    selected: &[usize],
    amount: f32,
    mut random: impl FnMut() -> f64,
) {
    for i in chosen(notes, selected) {
        let v = notes[i].velocity + amount * random().clamp(-1.0, 1.0) as f32;
        LaneValue::Velocity.put(&mut notes[i], v);
    }
}

/// Chances move to either side of where they are, within `amount` (manual p. 283).
pub fn randomize_chance(
    notes: &mut [NoteData],
    selected: &[usize],
    amount: f32,
    mut random: impl FnMut() -> f64,
) {
    let set = chosen(notes, selected);
    for &i in &set {
        let p = notes[i].probability + amount * random().clamp(-1.0, 1.0) as f32;
        LaneValue::Chance.put(&mut notes[i], p);
    }
    spread_group_chance(notes, &set);
}

/// Spreads velocities evenly through time from `start` to `end` (manual p. 280).
pub fn ramp_velocity(notes: &mut [NoteData], selected: &[usize], start: f32, end: f32) {
    let mut set = chosen(notes, selected);
    set.sort_by_key(|i| notes[*i].start);
    let Some((first, last)) =
        span_of(notes, &set).map(|_| (notes[set[0]].start, notes[set[set.len() - 1]].start))
    else {
        return;
    };
    let width = (last - first).max(1) as f32;
    for i in set {
        let at = (notes[i].start - first) as f32 / width;
        LaneValue::Velocity.put(&mut notes[i], start + (end - start) * at);
    }
}

/// Sets the Velocity Deviation, kept within what each note's velocity leaves room for (manual
/// p. 280).
pub fn set_deviation(notes: &mut [NoteData], selected: &[usize], range: f32) {
    for i in chosen(notes, selected) {
        let v = notes[i].velocity;
        notes[i].vel_range = range.clamp(-v, 1.0 - v);
    }
}

/// Moves the Velocity Deviation of the chosen notes.
pub fn change_deviation(notes: &mut [NoteData], selected: &[usize], by: f32) {
    for i in chosen(notes, selected) {
        let v = notes[i].velocity;
        notes[i].vel_range = (notes[i].vel_range + by).clamp(-v, 1.0 - v);
    }
}

/// The lowest and highest velocity a note can come out at.
pub fn deviation_bounds(n: &NoteData) -> (f32, f32) {
    let a = n.velocity + n.vel_range;
    (a.min(n.velocity).max(0.0), a.max(n.velocity).min(1.0))
}

/// The notes Draw Mode reaches at `tick` in a lane: those starting in its grid division, or the
/// one note starting nearest with no grid. Selected notes only, if there are any (manual p. 281).
pub fn drawn_notes(
    notes: &[NoteData],
    selected: &[usize],
    tick: f64,
    grid: Option<u64>,
) -> Vec<usize> {
    let pool = chosen(notes, selected);
    match grid {
        Some(grid) => {
            let grid = grid.max(1);
            let cell = (tick.max(0.0) as u64) / grid;
            pool.into_iter()
                .filter(|i| notes[*i].start / grid == cell)
                .collect()
        }
        None => pool
            .into_iter()
            .min_by(|a, b| {
                let distance = |i: &usize| (notes[*i].start as f64 - tick).abs();
                distance(a).total_cmp(&distance(b))
            })
            .into_iter()
            .collect(),
    }
}

/// Draws along a line from `(t0, v0)` to `(t1, v1)`: each chosen note it passes over takes the
/// line's value there.
pub fn line_values(
    notes: &mut [NoteData],
    selected: &[usize],
    what: LaneValue,
    (t0, v0): (f64, f32),
    (t1, v1): (f64, f32),
) {
    let (lo, hi) = (t0.min(t1), t0.max(t1));
    for i in chosen(notes, selected) {
        let t = notes[i].start as f64;
        if t < lo || t > hi {
            continue;
        }
        let at = if hi > lo { (t - t0) / (t1 - t0) } else { 0.0 };
        what.put(&mut notes[i], v0 + (v1 - v0) * at as f32);
    }
}

// ------------------------------------------------------------- probability groups

/// Makes a probability group of the notes. They leave whatever groups they were in, and all
/// share the chance of the first. A group left with one note is no longer a group.
pub fn group_notes(notes: &mut [NoteData], selected: &[usize], one: bool) {
    if selected.len() < 2 {
        return;
    }
    let id = notes
        .iter()
        .filter_map(|n| n.group.map(|g| g.id))
        .max()
        .unwrap_or(0)
        + 1;
    let p = selected
        .iter()
        .find_map(|i| notes.get(*i).map(|n| n.probability))
        .unwrap_or(1.0);
    for &i in selected {
        if let Some(n) = notes.get_mut(i) {
            n.group = Some(NoteGroup { id, one });
            n.probability = p;
        }
    }
    dissolve_single_groups(notes);
}

/// Changes the type of every group the chosen notes are in.
pub fn set_group_kind(notes: &mut [NoteData], selected: &[usize], one: bool) {
    let ids: Vec<u32> = selected
        .iter()
        .filter_map(|i| notes.get(*i).and_then(|n| n.group.map(|g| g.id)))
        .collect();
    for n in notes.iter_mut() {
        if let Some(g) = &mut n.group {
            if ids.contains(&g.id) {
                g.one = one;
            }
        }
    }
}

/// Takes the notes out of their groups (manual p. 284).
pub fn ungroup(notes: &mut [NoteData], selected: &[usize]) {
    for &i in selected {
        if let Some(n) = notes.get_mut(i) {
            n.group = None;
        }
    }
    dissolve_single_groups(notes);
}

fn dissolve_single_groups(notes: &mut [NoteData]) {
    let ids: Vec<u32> = notes.iter().filter_map(|n| n.group.map(|g| g.id)).collect();
    for n in notes.iter_mut() {
        if n.group
            .is_some_and(|g| ids.iter().filter(|i| **i == g.id).count() < 2)
        {
            n.group = None;
        }
    }
}

/// What the Status Bar says of the groups of the selected notes: the type if they are all in one
/// group, an asterisk if not (manual p. 284).
pub fn group_status(notes: &[NoteData], selected: &[usize]) -> Option<String> {
    let groups: Vec<Option<NoteGroup>> = selected
        .iter()
        .filter_map(|i| notes.get(*i).map(|n| n.group))
        .collect();
    if groups.iter().all(Option::is_none) {
        return None;
    }
    let one_group = groups.windows(2).all(|w| w[0] == w[1]) && groups[0].is_some();
    Some(match (one_group, groups[0]) {
        (true, Some(g)) if g.one => "Play One".to_string(),
        (true, Some(_)) => "Play All".to_string(),
        _ => "*".to_string(),
    })
}

// ------------------------------------------------------------- time

/// Duplicate Time: a copy of `[from, to)` follows it, and what came after moves along. The clip's
/// markers don't change (manual p. 288).
pub fn duplicate_time(notes: &mut Vec<NoteData>, from: u64, to: u64) {
    let len = to.saturating_sub(from);
    let mut copies = Vec::new();
    for n in notes.iter_mut() {
        if n.start >= to {
            n.start += len;
        } else if n.start >= from {
            copies.push(NoteData {
                start: n.start + len,
                ..n.clone()
            });
        }
    }
    notes.extend(copies);
}

/// Delete Time: `[from, to)` goes, and what came after closes up. A note reaching into it is cut
/// at its edge.
pub fn delete_time(notes: &mut Vec<NoteData>, from: u64, to: u64) {
    let len = to.saturating_sub(from);
    notes.retain(|n| n.start < from || n.start >= to);
    for n in notes.iter_mut() {
        if n.start >= to {
            n.start -= len;
        } else if n.start + n.length > from {
            n.length = from - n.start;
        }
    }
    notes.retain(|n| n.length > 0);
}

/// Insert Time: empty time of length `len` at `at`, pushing later notes along.
pub fn insert_time(notes: &mut [NoteData], at: u64, len: u64) {
    for n in notes.iter_mut().filter(|n| n.start >= at) {
        n.start += len;
    }
}

// ------------------------------------------------------------- drawing

/// The notes a melodic Draw Mode stroke makes: one in each grid cell the pointer went through, on
/// the key it was on. Drawing over a note erases it instead (manual p. 258).
pub fn draw_melodic(
    base: &[NoteData],
    path: &[(u64, u8)],
    grid: u64,
    velocity: f32,
) -> Vec<NoteData> {
    let grid = grid.max(1);
    let mut out = base.to_vec();
    for &(cell, key) in path {
        let at = cell * grid;
        match out
            .iter()
            .position(|n| n.key == key && n.start < at + grid && n.start + n.length > at)
        {
            Some(i) => {
                out.remove(i);
            }
            None => out.push(NoteData::new(at, grid, key, velocity)),
        }
    }
    out
}

/// The notes a pitch-locked stroke makes: one in each cell from where it began to where the
/// pointer is, on one key, so moving back towards the first note takes the later ones away
/// (manual p. 258).
pub fn draw_locked(
    base: &[NoteData],
    key: u8,
    first: u64,
    current: u64,
    grid: u64,
    velocity: f32,
) -> Vec<NoteData> {
    let grid = grid.max(1);
    let (lo, hi) = (first.min(current), first.max(current));
    let mut out: Vec<NoteData> = base
        .iter()
        .filter(|n| n.key != key || n.start + n.length <= lo * grid || n.start >= (hi + 1) * grid)
        .cloned()
        .collect();
    out.extend((lo..=hi).map(|cell| NoteData::new(cell * grid, grid, key, velocity)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::PPQ;

    const GRID: u64 = PPQ / 4;

    fn note(start: u64, key: u8) -> NoteData {
        NoteData::new(start, PPQ, key, 0.5)
    }

    #[test]
    fn a_new_note_overwrites_what_it_starts_over_and_shortens_what_it_ends() {
        let mut n = vec![
            note(0, 60),
            note(PPQ / 2, 60),
            NoteData::new(0, 4 * PPQ, 61, 0.5),
            note(PPQ / 4, 60),
        ];
        // The last note is new: it covers the start of the second and the end of the first.
        let w = resolve_overlaps(&mut n, &[3]);
        assert_eq!(n.len(), 3, "the second note is gone");
        assert_eq!(n[0].length, PPQ / 4, "the first is shortened");
        assert_eq!(n[1].key, 61, "another key track is left alone");
        assert_eq!(w, vec![2], "the new note is still in the list");
    }

    #[test]
    fn the_grid_is_magnetic_after_the_first_line() {
        // Off the grid by 10: free until it gets to the next line.
        let origin = GRID + 10;
        assert_eq!(magnetic_delta(origin, 20.0, GRID), 20);
        // Past the line it goes from line to line.
        assert_eq!(magnetic_delta(origin, (GRID + 2) as f64, GRID), GRID as i64);
        // A note on a line has met one, and goes straight to the next.
        assert_eq!(magnetic_delta(GRID, 3.0, GRID), 0);
        assert_eq!(magnetic_delta(GRID, (GRID - 3) as f64, GRID), GRID as i64);
    }

    #[test]
    fn a_loose_note_can_keep_its_offset_from_the_grid() {
        let origin = GRID + 10;
        // Dragged about a beat on: the offset's nearer than the line.
        let d = magnetic_delta(origin, (2 * GRID) as f64 + 2.0, GRID);
        assert_eq!(d, 2 * GRID as i64, "lands with its offset");
        assert_eq!((origin as i64 + d) % GRID as i64, 10);
        // Backwards likewise.
        let d = magnetic_delta(origin, -(GRID as f64) - 2.0, GRID);
        assert_eq!(origin as i64 + d, 10, "the same offset from the first line");
    }

    fn with_slide(length: u64) -> NoteData {
        NoteData {
            expr: Some(Box::new(NoteExpression {
                slide: vec![
                    Breakpoint::new(0.0, 0.1),
                    Breakpoint::new(length as f64, 0.9),
                ],
                ..Default::default()
            })),
            ..NoteData::new(0, length, 60, 0.8)
        }
    }

    #[test]
    fn stretching_a_note_stretches_its_expression() {
        let mut notes = vec![with_slide(480)];
        let originals = vec![(0, notes[0].clone())];
        apply_stretch(
            &mut notes,
            &originals,
            TimeMap {
                from: 0.0,
                to: 480.0,
                new_from: 0.0,
                new_to: 960.0,
                mid: None,
            },
        );
        assert_eq!(notes[0].length, 960);
        let slide = &notes[0].expr.as_ref().unwrap().slide;
        assert_eq!(slide[1].time, 960.0);
        assert_eq!(slide[1].value, 0.9);
    }

    #[test]
    fn a_mirrored_stretch_turns_the_expression_round() {
        let mut notes = vec![with_slide(480)];
        notes[0].expr.as_mut().unwrap().slide[0].curve = 0.5;
        let originals = vec![(0, notes[0].clone())];
        apply_stretch(
            &mut notes,
            &originals,
            TimeMap {
                from: 0.0,
                to: 480.0,
                new_from: 480.0,
                new_to: 0.0,
                mid: None,
            },
        );
        assert_eq!(notes[0].start, 0);
        let slide = &notes[0].expr.as_ref().unwrap().slide;
        assert_eq!(slide[0].value, 0.9, "what was last comes first");
        assert_eq!(slide[1].value, 0.1);
        assert_eq!(slide[0].time, 0.0);
        assert_eq!(slide[1].time, 480.0);
        assert_eq!(slide[0].curve, -0.5, "and the curve is mirrored");
    }

    #[test]
    fn split_keeps_everything_but_time_and_divides_expression() {
        let mut n = vec![NoteData {
            expr: Some(Box::new(NoteExpression {
                slide: vec![
                    Breakpoint::new(0.0, 0.1),
                    Breakpoint::new(PPQ as f64 * 0.75, 0.9),
                ],
                ..NoteExpression::default()
            })),
            ..note(0, 60)
        }];
        let made = split_at(&mut n, &[0], PPQ / 2);
        assert_eq!(made, vec![0, 1]);
        assert_eq!(
            (n[0].length, n[1].start, n[1].length),
            (PPQ / 2, PPQ / 2, PPQ / 2)
        );
        let right = n[1].expr.as_ref().unwrap();
        assert_eq!(
            right.slide[0].time,
            PPQ as f64 / 4.0,
            "time restarts at the cut"
        );
        assert_eq!(n[0].expr.as_ref().unwrap().slide.len(), 1);
    }

    #[test]
    fn notes_crossing_a_point_are_those_it_can_split() {
        let n = vec![note(0, 60), note(PPQ, 62)];
        assert_eq!(notes_crossing(&n, PPQ / 2), vec![0]);
        assert_eq!(
            notes_crossing(&n, PPQ),
            Vec::<usize>::new(),
            "not at their edges"
        );
    }

    #[test]
    fn chop_cuts_on_grid_lines_and_in_equal_parts() {
        let mut n = vec![note(0, 60)];
        let pieces = chop_on_grid(&mut n, &[0], GRID);
        assert_eq!(pieces.len(), 4);
        assert!(n.iter().all(|x| x.length == GRID));
        let mut n = vec![NoteData::new(GRID / 2, GRID, 60, 0.5)];
        assert_eq!(parts_on_grid(&n[0], GRID), 2);
        chop_on_grid(&mut n, &[0], GRID);
        assert_eq!(n[0].length, GRID / 2, "up to the first line");
        let mut n = vec![note(0, 60)];
        chop_into(&mut n, &[0], 3);
        assert_eq!(n.len(), 3);
        assert_eq!(n.iter().map(|x| x.length).sum::<u64>(), PPQ);
    }

    #[test]
    fn chop_counts_step_by_one_or_by_powers_of_two() {
        assert_eq!(next_parts(3, true, false), 4);
        assert_eq!(next_parts(1, false, false), 1);
        assert_eq!(next_parts(4, true, true), 8);
        assert_eq!(next_parts(3, true, true), 4);
        assert_eq!(next_parts(8, false, true), 4);
    }

    #[test]
    fn join_makes_one_note_of_each_pitch() {
        let mut n = vec![
            NoteData {
                expr: Some(Box::new(NoteExpression {
                    pitch: vec![Breakpoint::new(0.0, 1.0)],
                    ..Default::default()
                })),
                ..note(0, 60)
            },
            note(2 * PPQ, 62),
            NoteData {
                expr: Some(Box::new(NoteExpression {
                    pitch: vec![Breakpoint::new(0.0, 2.0)],
                    ..Default::default()
                })),
                ..note(PPQ, 60)
            },
        ];
        let joined = join(&mut n, &[0, 2]);
        assert_eq!(n.len(), 2);
        assert_eq!(joined, vec![0]);
        assert_eq!(n[0].length, 2 * PPQ);
        let pitch = &n[0].expr.as_ref().unwrap().pitch;
        assert_eq!(pitch[1].time, PPQ as f64, "the later note's MPE follows on");
    }

    #[test]
    fn fit_to_range_gives_every_note_the_span() {
        let mut n = vec![note(PPQ, 60), note(3 * PPQ, 62)];
        fit_to_range(&mut n, &[0, 1], 0, 4 * PPQ);
        assert!(n.iter().all(|x| x.start == 0 && x.length == 4 * PPQ));
    }

    #[test]
    fn note_stretch_scales_proportionally_and_mirrors_when_crossed() {
        let originals = vec![(0, note(0, 60)), (1, note(PPQ, 62))];
        let mut n = vec![note(0, 60), note(PPQ, 62)];
        let doubled = TimeMap {
            from: 0.0,
            to: 2.0 * PPQ as f64,
            new_from: 0.0,
            new_to: 4.0 * PPQ as f64,
            mid: None,
        };
        apply_stretch(&mut n, &originals, doubled);
        assert_eq!((n[1].start, n[1].length), (2 * PPQ, 2 * PPQ));
        let crossed = TimeMap {
            new_from: 2.0 * PPQ as f64,
            new_to: 0.0,
            ..doubled
        };
        assert!(crossed.mirrored());
        apply_stretch(&mut n, &originals, crossed);
        assert_eq!(n[0].start, PPQ, "the first note ends up last");
        assert_eq!(n[1].start, 0);
    }

    #[test]
    fn the_pseudo_marker_stretches_against_the_fixed_ones() {
        let map = TimeMap {
            from: 0.0,
            to: 4.0,
            new_from: 0.0,
            new_to: 4.0,
            mid: Some((2.0, 3.0)),
        };
        assert_eq!(map.at(1.0), 1.5);
        assert_eq!(map.at(3.0), 3.5);
        assert_eq!(map.at(4.0), 4.0, "the end marker doesn't move");
    }

    #[test]
    fn a_stretch_leaves_unlinked_envelopes_alone() {
        use crate::{EnvLoop, Envelope};
        let env = |looped: bool| ClipEnvelope {
            envelope: Envelope {
                points: vec![Breakpoint::new(PPQ as f64, 1.0)],
                target: rlive_core::EnvTarget::MidiCtrl(1),
            },
            mode: Default::default(),
            loop_region: looped.then_some(EnvLoop::new(0, PPQ)),
        };
        let before = vec![env(false), env(true)];
        let mut after = before.clone();
        stretch_envelopes(
            &mut after,
            &before,
            TimeMap {
                from: 0.0,
                to: 2.0 * PPQ as f64,
                new_from: 0.0,
                new_to: 4.0 * PPQ as f64,
                mid: None,
            },
        );
        assert_eq!(after[0].envelope.points[0].time, 2.0 * PPQ as f64);
        assert_eq!(after[1].envelope.points[0].time, PPQ as f64);
    }

    #[test]
    fn deactivate_toggles_the_lot() {
        let mut n = vec![note(0, 60), note(PPQ, 62)];
        n[0].muted = true;
        toggle_active(&mut n, &[]);
        assert!(n.iter().all(|x| x.muted), "any still on: all go off");
        toggle_active(&mut n, &[]);
        assert!(n.iter().all(|x| !x.muted));
    }

    #[test]
    fn quantize_moves_by_the_amount_and_can_do_ends() {
        let mut n = vec![NoteData::new(GRID + 20, GRID, 60, 0.5)];
        let q = Quantize {
            grid: GRID,
            amount: 0.5,
            starts: true,
            ends: false,
        };
        quantize(&mut n, &[], q);
        assert_eq!(n[0].start, GRID + 10);
        quantize(
            &mut n,
            &[],
            Quantize {
                amount: 1.0,
                ends: true,
                ..q
            },
        );
        assert_eq!(n[0].start, GRID);
        assert_eq!(n[0].start + n[0].length, 2 * GRID);
    }

    #[test]
    fn a_deviation_stays_within_a_notes_velocity_room() {
        let mut n = vec![
            NoteData::new(0, GRID, 60, 50.0 / 127.0),
            NoteData::new(0, GRID, 62, 25.0 / 127.0),
        ];
        set_deviation(&mut n, &[], 1.0);
        assert!((n[0].vel_range - 77.0 / 127.0).abs() < 1e-5);
        assert!((n[1].vel_range - 102.0 / 127.0).abs() < 1e-5);
        assert_eq!(deviation_bounds(&n[0]), (n[0].velocity, 1.0));
    }

    #[test]
    fn velocity_is_ramped_and_randomized_within_its_amount() {
        let mut n = vec![note(0, 60), note(PPQ, 60), note(2 * PPQ, 60)];
        ramp_velocity(&mut n, &[], 0.25, 0.75);
        assert_eq!(
            (n[0].velocity, n[1].velocity, n[2].velocity),
            (0.25, 0.5, 0.75)
        );
        randomize_velocity(&mut n, &[1], 0.1, || 1.0);
        assert!((n[1].velocity - 0.6).abs() < 1e-5);
        assert_eq!(n[0].velocity, 0.25, "only the selected note");
    }

    #[test]
    fn drawing_reaches_the_grid_cell_or_the_nearest_note() {
        let n = vec![note(0, 60), note(GRID / 2, 62), note(GRID, 64)];
        assert_eq!(drawn_notes(&n, &[], 10.0, Some(GRID)), vec![0, 1]);
        assert_eq!(
            drawn_notes(&n, &[1], 10.0, Some(GRID)),
            vec![1],
            "selected only"
        );
        assert_eq!(drawn_notes(&n, &[], (GRID - 2) as f64, None), vec![2]);
    }

    #[test]
    fn a_line_draws_a_crescendo() {
        let mut n = vec![note(0, 60), note(PPQ, 60), note(2 * PPQ, 60)];
        line_values(
            &mut n,
            &[],
            LaneValue::Velocity,
            (0.0, 0.0),
            (2.0 * PPQ as f64, 1.0),
        );
        assert_eq!(n[1].velocity, 0.5);
    }

    #[test]
    fn a_group_shares_a_chance_and_a_type() {
        let mut n = vec![note(0, 60), note(PPQ, 62), note(2 * PPQ, 64)];
        n[1].probability = 0.3;
        group_notes(&mut n, &[1, 2], true);
        assert_eq!(n[2].probability, 0.3, "takes the first note's chance");
        assert_eq!(group_status(&n, &[1, 2]).as_deref(), Some("Play One"));
        assert_eq!(group_status(&n, &[0, 1]).as_deref(), Some("*"));
        set_value(&mut n, &[1], LaneValue::Chance, 0.8);
        assert_eq!(n[2].probability, 0.8, "one marker for the whole group");
        set_group_kind(&mut n, &[2], false);
        assert_eq!(group_status(&n, &[1]).as_deref(), Some("Play All"));
    }

    #[test]
    fn regrouping_a_note_takes_it_out_of_the_old_group() {
        let mut n = vec![note(0, 60), note(PPQ, 62), note(2 * PPQ, 64)];
        group_notes(&mut n, &[0, 1], false);
        let old = n[0].group.unwrap();
        group_notes(&mut n, &[1, 2], true);
        assert_ne!(n[1].group.unwrap(), old);
        assert_eq!(n[0].group, None, "a group of one is no group");
        ungroup(&mut n, &[1]);
        assert!(n.iter().all(|x| x.group.is_none()));
    }

    #[test]
    fn chance_randomizes_to_either_side() {
        let mut n = vec![note(0, 60)];
        n[0].probability = 0.5;
        randomize_chance(&mut n, &[], 0.25, || -1.0);
        assert_eq!(n[0].probability, 0.25);
    }

    #[test]
    fn the_time_commands_leave_markers_alone_and_move_notes() {
        let mut n = vec![note(0, 60), note(PPQ, 62), note(2 * PPQ, 64)];
        duplicate_time(&mut n, PPQ, 2 * PPQ);
        let at = |n: &[NoteData], k: u8| {
            let mut v: Vec<u64> = n.iter().filter(|x| x.key == k).map(|x| x.start).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(at(&n, 62), vec![PPQ, 2 * PPQ]);
        assert_eq!(at(&n, 64), vec![3 * PPQ], "what came after moved along");
        delete_time(&mut n, PPQ, 2 * PPQ);
        assert_eq!(at(&n, 62), vec![PPQ]);
        assert_eq!(at(&n, 64), vec![2 * PPQ]);
        insert_time(&mut n, PPQ, PPQ);
        assert_eq!(at(&n, 60), vec![0], "before the insert stays put");
        assert_eq!(at(&n, 62), vec![2 * PPQ]);
    }

    #[test]
    fn delete_time_cuts_a_note_reaching_into_it() {
        let mut n = vec![NoteData::new(0, 2 * PPQ, 60, 0.5)];
        delete_time(&mut n, PPQ, 3 * PPQ);
        assert_eq!(n[0].length, PPQ);
    }

    #[test]
    fn melodic_drawing_adds_and_erases() {
        let base = vec![NoteData::new(GRID, GRID, 62, 0.5)];
        let out = draw_melodic(&base, &[(0, 60), (1, 62), (2, 64)], GRID, 0.7);
        let keys: Vec<u8> = out.iter().map(|n| n.key).collect();
        assert_eq!(keys, vec![60, 64], "the note drawn over is erased");
    }

    #[test]
    fn pitch_locked_drawing_shrinks_back_towards_its_first_note() {
        let out = draw_locked(&[], 60, 0, 3, GRID, 0.7);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|n| n.key == 60));
        let out = draw_locked(&[], 60, 0, 1, GRID, 0.7);
        assert_eq!(out.len(), 2, "moving back takes notes away");
        let existing = vec![NoteData::new(GRID, GRID, 60, 0.2)];
        let out = draw_locked(&existing, 60, 0, 2, GRID, 0.7);
        assert_eq!(out.len(), 3, "what was under the stroke is replaced");
    }
}
