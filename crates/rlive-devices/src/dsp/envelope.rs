//! Loopable envelope with per-stage slopes and tempo-related repeat modes.

use super::tempo::BeatClock;
use super::util::{lerp, sanitize};

/// What happens when an envelope reaches the end of its decay.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LoopMode {
    /// Holds at sustain until the gate closes.
    #[default]
    Off,
    /// Restarts at once, or after [`loop_time`](EnvelopeSettings::loop_time), while the gate is
    /// open. [`loop_span`](EnvelopeSettings::loop_span) says which stages repeat.
    Loop,
    /// Restarts every [`repeat_beats`](EnvelopeSettings::repeat_beats) beats counted from the
    /// note, while the gate is open.
    Beat,
    /// Restarts on the song's `repeat_beats` grid while the gate is open. While the transport
    /// is stopped it behaves like [`Beat`](LoopMode::Beat).
    Sync,
    /// Runs through to the end of its release, ignoring the gate closing.
    Trigger,
}

/// The stages a [`LoopMode::Loop`] envelope repeats.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LoopSpan {
    /// Attack and decay.
    #[default]
    AttackDecay,
    /// Attack, decay and release.
    AttackDecayRelease,
    /// Attack, decay and a hold at sustain for `loop_time`.
    AttackDecaySustain,
}

/// Where an [`Envelope`] is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EnvStage {
    #[default]
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EnvelopeSettings {
    /// Level a note starts from when no release is ringing out.
    pub initial: f32,
    pub peak: f32,
    pub sustain: f32,
    /// Level the release ends on.
    pub end: f32,
    /// Stage times in seconds. Zero takes one sample.
    pub attack: f32,
    pub decay: f32,
    pub release: f32,
    /// Stage curves in `-1.0..=1.0`. Zero is a straight line, positive moves early and settles
    /// slowly, negative moves late.
    pub attack_slope: f32,
    pub decay_slope: f32,
    pub release_slope: f32,
    pub loop_mode: LoopMode,
    pub loop_span: LoopSpan,
    /// Seconds to hold before a [`LoopMode::Loop`] restart. The `AttackDecayRelease` span
    /// doesn't use it.
    pub loop_time: f32,
    /// Interval of the [`LoopMode::Beat`] and [`LoopMode::Sync`] restarts.
    pub repeat_beats: f64,
}

impl Default for EnvelopeSettings {
    fn default() -> Self {
        Self {
            initial: 0.0,
            peak: 1.0,
            sustain: 0.7,
            end: 0.0,
            attack: 0.005,
            decay: 0.2,
            release: 0.2,
            attack_slope: 0.0,
            decay_slope: 0.0,
            release_slope: 0.0,
            loop_mode: LoopMode::Off,
            loop_span: LoopSpan::AttackDecay,
            loop_time: 0.0,
            repeat_beats: 1.0,
        }
    }
}

/// The shape of a stage: 0 at `t` = 0, 1 at `t` = 1, a straight line at `slope` = 0.
#[inline]
pub fn slope_curve(t: f32, slope: f32) -> f32 {
    let k = -6.0 * slope.clamp(-1.0, 1.0);
    if k.abs() < 1e-3 {
        t
    } else {
        (k * t).exp_m1() / k.exp_m1()
    }
}

/// A one-shot or looping envelope, at sample accuracy. Stage times can change while it runs.
///
/// Call [`set_clock`](Envelope::set_clock) at the start of each block if a beat-based mode is
/// in use.
#[derive(Clone, Copy, Debug)]
pub struct Envelope {
    sample_rate: f64,
    settings: EnvelopeSettings,
    attack_step: f64,
    decay_step: f64,
    release_step: f64,
    hold_step: f64,
    clock: BeatClock,
    stage: EnvStage,
    phase: f64,
    from: f32,
    value: f32,
    gate: bool,
    beats_since_on: f64,
    grid_index: i64,
}

impl Default for Envelope {
    fn default() -> Self {
        Self::new()
    }
}

impl Envelope {
    pub fn new() -> Self {
        let mut e = Self {
            sample_rate: 48_000.0,
            settings: EnvelopeSettings::default(),
            attack_step: 1.0,
            decay_step: 1.0,
            release_step: 1.0,
            hold_step: 0.0,
            clock: BeatClock::default(),
            stage: EnvStage::Idle,
            phase: 0.0,
            from: 0.0,
            value: 0.0,
            gate: false,
            beats_since_on: 0.0,
            grid_index: 0,
        };
        e.update_steps();
        e
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(1.0);
        self.update_steps();
    }

    pub fn set(&mut self, settings: &EnvelopeSettings) {
        let mut s = *settings;
        for level in [
            &mut s.initial,
            &mut s.peak,
            &mut s.sustain,
            &mut s.end,
            &mut s.attack_slope,
            &mut s.decay_slope,
            &mut s.release_slope,
        ] {
            *level = sanitize(*level);
        }
        self.settings = s;
        self.update_steps();
    }

    pub fn settings(&self) -> &EnvelopeSettings {
        &self.settings
    }

    fn update_steps(&mut self) {
        let sr = self.sample_rate;
        // `min` also turns the step for NaN seconds into a one-sample stage.
        let step = |seconds: f32| (1.0 / (seconds.max(0.0) as f64 * sr)).min(1.0);
        self.attack_step = step(self.settings.attack);
        self.decay_step = step(self.settings.decay);
        self.release_step = step(self.settings.release);
        self.hold_step = if self.settings.loop_time > 0.0 {
            step(self.settings.loop_time)
        } else {
            0.0
        };
    }

    /// Follow the transport for the block that is starting.
    pub fn set_clock(&mut self, clock: &BeatClock) {
        self.clock = *clock;
    }

    pub fn stage(&self) -> EnvStage {
        self.stage
    }

    pub fn level(&self) -> f32 {
        self.value
    }

    pub fn is_active(&self) -> bool {
        self.stage != EnvStage::Idle
    }

    /// Start the attack: from `initial` if idle, otherwise from wherever it is now, so
    /// retriggering doesn't click.
    pub fn gate_on(&mut self) {
        self.gate = true;
        self.from = if self.stage == EnvStage::Idle {
            self.settings.initial
        } else {
            self.value
        };
        self.stage = EnvStage::Attack;
        self.phase = 0.0;
        self.beats_since_on = 0.0;
        self.grid_index = self.grid();
    }

    /// Start the release, unless the mode is [`LoopMode::Trigger`].
    pub fn gate_off(&mut self) {
        self.gate = false;
        if self.settings.loop_mode == LoopMode::Trigger || self.stage == EnvStage::Idle {
            return;
        }
        self.enter_release(self.value, 0.0);
    }

    /// Go silent at once, for a voice that has been stolen.
    pub fn kill(&mut self) {
        self.gate = false;
        self.stage = EnvStage::Idle;
        self.value = 0.0;
        self.phase = 0.0;
    }

    pub fn reset(&mut self) {
        self.kill();
    }

    fn grid(&self) -> i64 {
        (self.clock.beat / self.settings.repeat_beats.max(1e-6)).floor() as i64
    }

    fn enter_release(&mut self, from: f32, carry: f64) {
        self.from = from;
        self.stage = EnvStage::Release;
        self.phase = carry * self.release_step;
    }

    fn enter_sustain(&mut self, carry: f64) {
        self.stage = EnvStage::Sustain;
        self.phase = carry * self.hold_step;
    }

    fn restart(&mut self, carry: f64) {
        self.from = self.value;
        self.stage = EnvStage::Attack;
        self.phase = carry * self.attack_step;
    }

    /// Moves on from a stage that ended `carry` samples ago, so stage lengths that aren't a
    /// whole number of samples still add up exactly.
    fn stage_ended(&mut self, carry: f64) {
        let s = self.settings;
        match self.stage {
            EnvStage::Attack => {
                self.from = s.peak;
                self.stage = EnvStage::Decay;
                self.phase = carry * self.decay_step;
            }
            EnvStage::Decay => match (s.loop_mode, s.loop_span) {
                (LoopMode::Trigger, _) => self.enter_release(s.sustain, carry),
                (LoopMode::Loop, LoopSpan::AttackDecayRelease) if self.gate => {
                    self.enter_release(s.sustain, carry)
                }
                (LoopMode::Loop, _) if self.gate && self.hold_step == 0.0 => {
                    self.value = s.sustain;
                    self.restart(carry);
                }
                _ => self.enter_sustain(carry),
            },
            EnvStage::Sustain => {
                if s.loop_mode == LoopMode::Loop && self.gate {
                    self.restart(carry);
                } else {
                    self.phase = 0.0;
                }
            }
            EnvStage::Release => {
                if s.loop_mode == LoopMode::Loop
                    && s.loop_span == LoopSpan::AttackDecayRelease
                    && self.gate
                {
                    self.value = s.end;
                    self.restart(carry);
                } else {
                    self.stage = EnvStage::Idle;
                    self.value = s.end;
                    self.phase = 0.0;
                }
            }
            EnvStage::Idle => {}
        }
    }

    #[inline]
    fn check_repeat(&mut self) {
        if !self.gate || self.stage == EnvStage::Idle {
            return;
        }
        let s = &self.settings;
        match s.loop_mode {
            LoopMode::Sync if self.clock.playing => {
                let index = self.grid();
                if index != self.grid_index {
                    self.grid_index = index;
                    self.restart(0.0);
                }
            }
            LoopMode::Beat | LoopMode::Sync => {
                let repeat = s.repeat_beats.max(1e-6);
                if self.beats_since_on + 1e-9 >= repeat {
                    self.beats_since_on -= repeat;
                    self.restart(0.0);
                }
            }
            _ => {}
        }
    }

    /// The next output sample.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f32 {
        self.check_repeat();
        let s = &self.settings;
        let t = (self.phase as f32).clamp(0.0, 1.0);
        let value = match self.stage {
            EnvStage::Idle => self.value,
            EnvStage::Attack => lerp(self.from, s.peak, slope_curve(t, s.attack_slope)),
            EnvStage::Decay => lerp(self.from, s.sustain, slope_curve(t, s.decay_slope)),
            EnvStage::Sustain => s.sustain,
            EnvStage::Release => lerp(self.from, s.end, slope_curve(t, s.release_slope)),
        };
        self.value = value;
        let step = match self.stage {
            EnvStage::Attack => self.attack_step,
            EnvStage::Decay => self.decay_step,
            EnvStage::Release => self.release_step,
            EnvStage::Sustain => self.hold_step,
            EnvStage::Idle => 0.0,
        };
        self.phase += step;
        // A zero-length stage may finish several in one go; the stage order bounds the steps.
        for _ in 0..4 {
            if self.phase < 1.0 || step == 0.0 {
                break;
            }
            let carry = (self.phase - 1.0) / step;
            let before = self.stage;
            self.stage_ended(carry);
            if self.stage == before {
                break;
            }
        }
        self.clock.tick();
        self.beats_since_on += self.clock.beats_per_sample;
        value
    }

    /// Runs `frames` samples and returns the last. For control-rate use.
    pub fn advance(&mut self, frames: usize) -> f32 {
        for _ in 0..frames {
            self.next();
        }
        self.value
    }
}
