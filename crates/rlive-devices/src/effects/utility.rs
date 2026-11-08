use super::defaults;
use crate::devices::b3::common::Lr4;
use crate::devices::b4::common::{gain_or_silence, SILENCE_DB};
use crate::dsp::util::sanitize;
use crate::dsp::{BiquadKind, Smoother};
use rlive_core::{AudioBuffer, BlockStep, ModMode, ParamInfo, ProcessContext, Processor};

// ---------------------------------------------------------------- Utility

pub const GAIN: u32 = 0;
pub const PAN: u32 = 1;
pub const WIDTH: u32 = 2;
pub const MONO: u32 = 3;
pub const INVERT_L: u32 = 4;
pub const INVERT_R: u32 = 5;
// Added after the first release. Every default leaves the sound as it was.
pub const CHANNEL_MODE: u32 = 6;
pub const MID_SIDE_MODE: u32 = 7;
pub const MID_SIDE: u32 = 8;
pub const MUTE: u32 = 9;
pub const DC: u32 = 10;
pub const BASS_MONO: u32 = 11;
pub const BASS_FREQ: u32 = 12;
pub const BASS_AUDITION: u32 = 13;

/// The DC filter's cutoff, in Hz: far below hearing.
pub const DC_CUTOFF: f32 = 5.0;

pub fn utility_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(GAIN, "Gain", SILENCE_DB, 35.0, 0.0, "dB")
            .with_mod_mode(ModMode::Scale),
        ParamInfo::continuous(PAN, "Pan", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 4.0, 1.0, ""),
        ParamInfo::toggle(MONO, "Mono", false),
        ParamInfo::toggle(INVERT_L, "Phase L", false),
        ParamInfo::toggle(INVERT_R, "Phase R", false),
        ParamInfo::stepped(
            CHANNEL_MODE,
            "Channel Mode",
            &["Stereo", "Left", "Right", "Swap"],
            0,
        ),
        ParamInfo::toggle(MID_SIDE_MODE, "Mid/Side Mode", false),
        ParamInfo::continuous(MID_SIDE, "Mid/Side", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(MUTE, "Mute", false),
        ParamInfo::toggle(DC, "DC", false),
        ParamInfo::toggle(BASS_MONO, "Bass Mono", false),
        ParamInfo::skewed(BASS_FREQ, "Bass Mono Freq", 50.0, 500.0, 120.0, "Hz", 2.0),
        ParamInfo::toggle(BASS_AUDITION, "Bass Mono Audition", false),
    ]
}

/// A one-pole DC blocker for one channel.
#[derive(Clone, Copy, Default)]
struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline]
    fn process(&mut self, x: f32, r: f32) -> f32 {
        let y = x - self.x1 + r * self.y1;
        self.x1 = x;
        self.y1 = sanitize(y);
        self.y1
    }
}

pub struct Utility {
    v: Vec<f64>,
    gain_l: Smoother,
    gain_r: Smoother,
    sr: f32,
    dc: [DcBlock; 2],
    bass_lp: [Lr4; 2],
    bass_hp: [Lr4; 2],
    bass_dirty: bool,
}

impl Default for Utility {
    fn default() -> Self {
        Self {
            v: defaults(&utility_params()),
            gain_l: Smoother::new(1.0),
            gain_r: Smoother::new(1.0),
            sr: 48_000.0,
            dc: [DcBlock::default(); 2],
            bass_lp: [Lr4::default(); 2],
            bass_hp: [Lr4::default(); 2],
            bass_dirty: true,
        }
    }
}

impl Utility {
    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn render(&mut self, audio: AudioBuffer) {
        let g = if self.on(MUTE) {
            0.0
        } else {
            gain_or_silence(self.v[GAIN as usize])
        };
        let pan = self.v[PAN as usize] as f32;
        let width = if self.on(MONO) {
            0.0
        } else {
            self.v[WIDTH as usize] as f32
        };
        // How much mid and side there are. In Width mode that's what it always was.
        let (mid_gain, side_gain) = if self.on(MID_SIDE_MODE) && !self.on(MONO) {
            let amount = self.v[MID_SIDE as usize] as f32 * 0.01;
            if amount < 0.0 {
                (1.0, 1.0 + amount)
            } else {
                (1.0 - amount, 1.0)
            }
        } else {
            (1.0, width)
        };
        let inv_l = if self.on(INVERT_L) { -1.0 } else { 1.0 };
        let inv_r = if self.on(INVERT_R) { -1.0 } else { 1.0 };
        self.gain_l.target = g * (1.0 - pan).min(1.0) * inv_l;
        self.gain_r.target = g * (1.0 + pan).min(1.0) * inv_r;

        let channel_mode = self.v[CHANNEL_MODE as usize].round() as u32;
        let dc_on = self.on(DC);
        let dc_r = 1.0 - std::f32::consts::TAU * DC_CUTOFF / self.sr;
        let bass_on = self.on(BASS_MONO);
        let audition = bass_on && self.on(BASS_AUDITION);
        if bass_on && self.bass_dirty {
            let freq = self.v[BASS_FREQ as usize] as f32;
            for ch in 0..2 {
                self.bass_lp[ch].set(BiquadKind::LowPass, freq, self.sr);
                self.bass_hp[ch].set(BiquadKind::HighPass, freq, self.sr);
            }
            self.bass_dirty = false;
        }

        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for i in 0..left.len() {
            let (mut l, mut r) = (left[i], right[i]);
            if dc_on {
                l = self.dc[0].process(l, dc_r);
                r = self.dc[1].process(r, dc_r);
            }
            match channel_mode {
                1 => r = l,
                2 => l = r,
                3 => std::mem::swap(&mut l, &mut r),
                _ => {}
            }
            let mid = (l + r) * 0.5 * mid_gain;
            let side = (l - r) * 0.5 * side_gain;
            let (mut out_l, mut out_r) = (mid + side, mid - side);
            if bass_on {
                let low = (self.bass_lp[0].process(out_l) + self.bass_lp[1].process(out_r)) * 0.5;
                let (high_l, high_r) = (
                    self.bass_hp[0].process(out_l),
                    self.bass_hp[1].process(out_r),
                );
                if audition {
                    out_l = low;
                    out_r = low;
                } else {
                    out_l = low + high_l;
                    out_r = low + high_r;
                }
            }
            left[i] = out_l * self.gain_l.next();
            right[i] = out_r * self.gain_r.next();
        }
    }
}

impl Processor for Utility {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.gain_l.set_time(0.01, sample_rate as f32);
        self.gain_r.set_time(0.01, sample_rate as f32);
        self.sr = sample_rate as f32;
        self.bass_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == BASS_FREQ {
            self.bass_dirty = true;
        }
    }

    fn reset(&mut self) {
        self.dc = [DcBlock::default(); 2];
        for ch in 0..2 {
            self.bass_lp[ch].reset();
            self.bass_hp[ch].reset();
        }
    }
}
