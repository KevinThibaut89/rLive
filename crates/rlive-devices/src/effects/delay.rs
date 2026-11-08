use super::defaults;
use crate::dsp::{Biquad, BiquadKind, DelayLine, Smoother};
use rlive_core::{ParamInfo, ProcessContext, Processor};

// ---------------------------------------------------------------- Delay

pub const SYNC: u32 = 0;
pub const TIME: u32 = 1;
pub const DIVISION: u32 = 2;
pub const FEEDBACK: u32 = 3;
pub const MIX: u32 = 4;
pub const PING_PONG: u32 = 5;
pub const TONE: u32 = 6;
pub const DIVISIONS: [&str; 7] = ["1/16", "1/8", "1/8.", "1/4", "1/4.", "1/2", "1 Bar"];
/// Division lengths in quarter notes.
pub const DIVISION_BEATS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 4.0];

pub fn delay_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::toggle(SYNC, "Sync", true),
        ParamInfo::skewed(TIME, "Time", 1.0, 2_000.0, 375.0, "ms", 2.0),
        ParamInfo::stepped(DIVISION, "Division", &DIVISIONS, 2),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 0.95, 0.4, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 0.3, ""),
        ParamInfo::toggle(PING_PONG, "Ping Pong", true),
        ParamInfo::skewed(TONE, "Tone", 200.0, 20_000.0, 6_000.0, "Hz", 3.0),
    ]
}

pub struct Delay {
    v: Vec<f64>,
    sr: f32,
    lines: [DelayLine; 2],
    time: Smoother,
    tone: [Biquad; 2],
    tone_dirty: bool,
}

impl Default for Delay {
    fn default() -> Self {
        Self {
            v: defaults(&delay_params()),
            sr: 48_000.0,
            lines: [DelayLine::default(), DelayLine::default()],
            time: Smoother::new(0.0),
            tone: [Biquad::default(); 2],
            tone_dirty: true,
        }
    }
}

impl Processor for Delay {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let max = (sample_rate * 4.5) as usize;
        self.lines = [DelayLine::new(max), DelayLine::new(max)];
        self.time.set_time(0.08, self.sr);
        self.tone_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        if self.tone_dirty {
            for f in self.tone.iter_mut() {
                f.set(
                    BiquadKind::LowPass,
                    self.v[TONE as usize] as f32,
                    0.707,
                    0.0,
                    self.sr,
                );
            }
            self.tone_dirty = false;
        }
        let seconds = if self.v[SYNC as usize] >= 0.5 {
            let div = (self.v[DIVISION as usize].round() as usize).min(DIVISION_BEATS.len() - 1);
            DIVISION_BEATS[div] * 60.0 / ctx.transport.tempo
        } else {
            self.v[TIME as usize] / 1000.0
        };
        self.time.target = (seconds as f32 * self.sr).min(self.sr * 4.4);
        if self.time.value == 0.0 {
            self.time.reset();
        }
        let fb = self.v[FEEDBACK as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let ping = self.v[PING_PONG as usize] >= 0.5;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
        for i in 0..left.len() {
            let d = self.time.next();
            let dl = self.tone[0].process(self.lines[0].read(d));
            let dr = self.tone[1].process(self.lines[1].read(d));
            let (inl, inr) = (left[i], right[i]);
            if ping {
                let mono = (inl + inr) * 0.5;
                self.lines[0].write(mono + dr * fb);
                self.lines[1].write(dl * fb);
            } else {
                self.lines[0].write(inl + dl * fb);
                self.lines[1].write(inr + dr * fb);
            }
            left[i] = inl * (1.0 - mix) + dl * mix;
            right[i] = inr * (1.0 - mix) + dr * mix;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == TONE {
            self.tone_dirty = true;
        }
    }

    fn reset(&mut self) {
        for l in self.lines.iter_mut() {
            l.clear();
        }
    }
}
