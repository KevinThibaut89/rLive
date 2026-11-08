//! Follow Actions, Repeat and what a launched scene sets: everything due at a tick of its own.
//!
//! All of it runs between blocks. The block before one of these ends exactly on its frame, as it
//! does at a loop end, so each lands as sample-accurately as a launch does.

use crate::clip::{ArmedFollow, Pending, SlotLaunch};
use crate::track::MAX_SCENES;
use crate::{ClipData, Engine, EngineEvent};
use rlive_core::time::TempoRun;
use rlive_core::{FollowAction, FollowActions};
use std::sync::Arc;

/// The Follow Actions' random numbers: xorshift64*, seedable so tests can repeat them.
#[derive(Clone, Copy)]
pub(crate) struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Self {
        // Zero is the one state xorshift can't leave.
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in `0..1`.
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A whole number below `n`, which must not be zero.
    fn below(&mut self, n: usize) -> usize {
        (self.unit() * n as f64) as usize % n
    }
}

/// A scene launch waiting for its launch point, to set the tempo and arm its Follow Action.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct PendingScene {
    pub scene: usize,
    pub at: f64,
}

/// The slots a Follow Action can reach from `slot`: the unbroken run of clips around it.
fn group_of(slots: &[Option<Arc<ClipData>>], slot: usize) -> (usize, usize) {
    let filled = |i: usize| matches!(slots.get(i), Some(Some(_)));
    let mut first = slot;
    while first > 0 && filled(first - 1) {
        first -= 1;
    }
    let mut last = slot;
    while filled(last + 1) {
        last += 1;
    }
    (first, last)
}

/// Which of `first..=last` a Follow Action leaves for. `usable` says whether a place can play.
fn resolve(
    action: FollowAction,
    current: usize,
    (first, last): (usize, usize),
    usable: &dyn Fn(usize) -> bool,
    random: &mut Random,
) -> Option<usize> {
    let pick = |skip: Option<usize>, random: &mut Random| {
        let count = (first..=last)
            .filter(|i| usable(*i) && Some(*i) != skip)
            .count();
        if count == 0 {
            return None;
        }
        let n = random.below(count);
        (first..=last)
            .filter(|i| usable(*i) && Some(*i) != skip)
            .nth(n)
    };
    match action {
        FollowAction::None | FollowAction::Stop => None,
        FollowAction::PlayAgain => usable(current).then_some(current),
        FollowAction::Previous => (first..current).rev().find(|i| usable(*i)),
        // Wraps from the last place of the group to the first.
        FollowAction::Next => (current + 1..=last)
            .chain(first..=current)
            .find(|i| usable(*i)),
        FollowAction::First => (first..=last).find(|i| usable(*i)),
        FollowAction::Last => (first..=last).rev().find(|i| usable(*i)),
        FollowAction::Any => pick(None, random),
        // Only a lone clip plays twice in a row.
        FollowAction::Other => pick(Some(current), random).or(usable(current).then_some(current)),
        FollowAction::Jump(to) => {
            let to = to as usize;
            usable(to).then_some(to)
        }
    }
}

impl Engine {
    /// The next tick a Follow Action, Repeat or scene is due at.
    pub(crate) fn next_launch_event(&self) -> Option<f64> {
        let tracks = self.tracks.iter().flat_map(|t| {
            let follow = t.player.follow.map(|f| f.at);
            let repeat = t.player.repeat.map(|r| r.next);
            [follow, repeat]
        });
        let scene = [
            self.pending_scene.map(|s| s.at),
            self.scene_follow.map(|f| f.at),
        ];
        tracks.chain(scene).flatten().min_by(|a, b| a.total_cmp(b))
    }

    /// Carry out what has come due. Scenes go first: their actions take precedence.
    pub(crate) fn run_launch_events(&mut self, run: &mut TempoRun) {
        if let Some(s) = self
            .pending_scene
            .filter(|s| self.frames_until(s.at, run) == 0)
        {
            self.pending_scene = None;
            self.enter_scene(s.scene, s.at.max(self.tick));
            *run = self.tempo_run();
        }
        if let Some(f) = self
            .scene_follow
            .filter(|f| self.frames_until(f.at, run) == 0)
        {
            self.scene_follow = None;
            self.fire_scene_follow(f);
            *run = self.tempo_run();
        }
        for i in 0..self.tracks.len() {
            if let Some(r) = self.tracks[i]
                .player
                .repeat
                .filter(|r| self.frames_until(r.next, run) == 0)
            {
                let at = r.next.max(self.tick);
                let next = self.maps.bars.next_boundary(at + 1e-3, r.quantize);
                let player = &mut self.tracks[i].player;
                player.repeat = Some(crate::clip::Repeat { next, ..r });
                player.pending = Some(Pending::Launch {
                    slot: r.slot,
                    at,
                    velocity: r.velocity,
                });
            }
            let Some(f) = self.tracks[i]
                .player
                .follow
                .filter(|f| self.frames_until(f.at, run) == 0)
            else {
                continue;
            };
            self.tracks[i].player.follow = None;
            self.fire_clip_follow(i, f);
        }
    }

    /// The moment a launched scene lands: it sets its tempo and meter and starts counting.
    fn enter_scene(&mut self, scene: usize, at: f64) {
        let Some(info) = self.scenes.get(scene).copied() else {
            return;
        };
        if info.tempo.is_some() || info.signature.is_some() {
            if let Some(maps) = Arc::get_mut(&mut self.maps) {
                if let Some(bpm) = info.tempo {
                    maps.tempo.hold_from(at, bpm);
                }
                if let Some(signature) = info.signature {
                    maps.bars.restart_at(at, signature);
                }
            }
            self.mark_tempo_change();
            let _ = self.events.push(EngineEvent::SceneTiming {
                scene,
                tick: at,
                tempo: info.tempo,
                signature: info.signature,
            });
        }
        self.scene_follow = info.follow.enabled.then(|| ArmedFollow {
            at: at + info.follow.time.max(1) as f64,
            slot: scene,
            rule: info.follow,
        });
    }

    fn choose(&mut self, rule: &FollowActions) -> FollowAction {
        let unit = self.follow_random.unit();
        rule.choose(unit)
    }

    /// When a clip launched by a Follow Action starts: exactly on time, unless the clip has a
    /// quantization of its own. Global quantization doesn't hold it back (manual p. 367).
    fn follow_point(&self, settings: &SlotLaunch, due: f64) -> f64 {
        match settings.quantize {
            Some(q) => self.maps.bars.next_boundary(due, q),
            None => due,
        }
    }

    fn queue_follow_launch(&mut self, track: usize, slot: usize, due: f64) {
        let settings = self.tracks[track].launch_of(slot);
        let at = self.follow_point(&settings, due);
        let player = &mut self.tracks[track].player;
        player.released_early = false;
        player.repeat = None;
        player.from_scene = false;
        player.pending = Some(Pending::Launch {
            slot,
            at,
            velocity: 1.0,
        });
    }

    fn fire_clip_follow(&mut self, track: usize, f: ArmedFollow) {
        // A launch already on its way, a scene's above all, overrules the action.
        if !self.follow_enabled || self.tracks[track].player.pending.is_some() {
            return;
        }
        let action = self.choose(&f.rule);
        if action == FollowAction::Stop {
            self.tracks[track].player.pending = Some(Pending::Stop { at: f.at });
            return;
        }
        let t = &self.tracks[track];
        let group = group_of(&t.slots, f.slot);
        let usable = |i: usize| matches!(t.slots.get(i), Some(Some(_))) && t.launch_of(i).active;
        let target = resolve(action, f.slot, group, &usable, &mut self.follow_random);
        if let Some(slot) = target {
            self.queue_follow_launch(track, slot, f.at);
        }
    }

    fn fire_scene_follow(&mut self, f: ArmedFollow) {
        if !self.follow_enabled {
            return;
        }
        let action = self.choose(&f.rule);
        let n = self.scene_count.max(f.slot + 1).min(MAX_SCENES);
        if action == FollowAction::Stop {
            for t in self.tracks.iter_mut() {
                if matches!(t.slots.get(f.slot), Some(Some(_))) {
                    t.player.pending = Some(Pending::Stop { at: f.at });
                    t.player.repeat = None;
                    t.player.from_scene = false;
                }
            }
            return;
        }
        let usable = |i: usize| i < n;
        let Some(scene) = resolve(action, f.slot, (0, n - 1), &usable, &mut self.follow_random)
        else {
            return;
        };
        self.launch_scene_from_follow(scene, f.at);
    }

    fn launch_scene_from_follow(&mut self, scene: usize, due: f64) {
        for i in 0..self.tracks.len() {
            let has_clip = matches!(self.tracks[i].slots.get(scene), Some(Some(_)));
            let settings = self.tracks[i].launch_of(scene);
            if has_clip && settings.active {
                self.queue_follow_launch(i, scene, due);
            } else if !has_clip && settings.stop_button {
                let player = &mut self.tracks[i].player;
                player.repeat = None;
                player.pending = Some(Pending::Stop { at: due });
            }
        }
        self.enter_scene(scene, due.max(self.tick));
    }
}
