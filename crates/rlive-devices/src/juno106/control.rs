//! The slave CPU: key assignment, software envelopes and LFO, and the CV multiplex.
//!
//! The real firmware recomputes every control voltage once per 4.2 ms scan (service notes p.8) and
//! writes it through one 12-bit DAC into sample-and-hold capacitors. That scan rate, the 7-bit
//! panel, and the integer counter divisors are part of the sound, so they are kept here as they
//! are on the machine. Time and level curves between the published end points are estimates.

use super::p;
use super::unit::{Unit, VOICES};

/// One full pass of the D/A and S/H timing chart.
pub const SCAN_S: f32 = 4.2e-3;

/// Concert pitch: the service notes tune A4 to 442 Hz.
pub const A4_HZ: f32 = 442.0;

/// The VCF CV spans +4 V to -6 V, which is the specified 5 Hz to 50 kHz.
pub const VCF_V_MIN: f32 = -6.0;
pub const VCF_V_MAX: f32 = 4.0;
pub const VCF_OCT_PER_V: f32 = 1.329;

/// Calibration point: test bank 3 (FREQ 6.3, full keyboard follow, C4) self-oscillates at 248 Hz.
pub const VCF_REF_HZ: f32 = 248.0;
const VCF_REF_SLIDER: f32 = 80.0 / 127.0;

pub fn vcf_ref_v() -> f32 {
    VCF_V_MIN + (VCF_REF_HZ / 5.0).log2() / VCF_OCT_PER_V
}

const MASTER_CLOCK_8FT_HZ: f32 = 2.0e6;

/// 12-bit DAC: quantises `frac` in `0..=1` and applies this unit's ladder errors.
fn dac(unit: &Unit, k: f32, frac: f32) -> f32 {
    let code = (frac.clamp(0.0, 1.0) * 4095.0).round() as usize;
    let ideal = code as f32 / 4095.0;
    ideal + (unit.dac[code] - ideal) * k.min(1.0)
}

/// Panel sliders are read through a 7-bit conversion, so sweeps move in audible steps.
#[inline]
fn seven_bit(v: f64) -> f32 {
    ((v / 10.0).clamp(0.0, 1.0) * 127.0).round() as f32 / 127.0
}

pub struct Panel {
    lfo_rate: f32,
    lfo_delay: f32,
    range: usize,
    dco_lfo: f32,
    pwm: f32,
    pwm_man: bool,
    pulse: bool,
    pub saw: bool,
    sub: f32,
    noise: f32,
    pub hpf: usize,
    freq: f32,
    res: f32,
    invert: bool,
    env: f32,
    vcf_lfo: f32,
    kybd: f32,
    gate_mode: bool,
    level: f32,
    a: f32,
    d: f32,
    s: f32,
    r: f32,
    pub chorus: usize,
    bend_dco: f32,
    bend_vcf: f32,
    bend_lfo: f32,
    porta: f32,
    porta_on: bool,
    assign: usize,
    tune: f32,
}

impl Panel {
    pub fn read(v: &[f64]) -> Self {
        let on = |id: u32| v[id as usize] >= 0.5;
        let idx = |id: u32| v[id as usize].round().max(0.0) as usize;
        let s = |id: u32| seven_bit(v[id as usize]);
        Self {
            lfo_rate: s(p::LFO_RATE),
            lfo_delay: s(p::LFO_DELAY),
            range: idx(p::RANGE).min(2),
            dco_lfo: s(p::DCO_LFO),
            pwm: s(p::PWM),
            pwm_man: on(p::PWM_MODE),
            pulse: on(p::PULSE),
            saw: on(p::SAW),
            sub: s(p::SUB),
            noise: s(p::NOISE),
            hpf: idx(p::HPF).min(3),
            freq: s(p::VCF_FREQ),
            res: s(p::VCF_RES),
            invert: on(p::VCF_POLARITY),
            env: s(p::VCF_ENV),
            vcf_lfo: s(p::VCF_LFO),
            kybd: s(p::VCF_KYBD),
            gate_mode: on(p::VCA_MODE),
            level: s(p::VCA_LEVEL),
            a: s(p::ENV_A),
            d: s(p::ENV_D),
            s: s(p::ENV_S),
            r: s(p::ENV_R),
            chorus: idx(p::CHORUS).min(2),
            bend_dco: s(p::BEND_DCO),
            bend_vcf: s(p::BEND_VCF),
            bend_lfo: s(p::BEND_LFO),
            porta: s(p::PORTAMENTO),
            porta_on: on(p::PORTAMENTO_ON),
            assign: idx(p::ASSIGN).min(2),
            tune: v[p::TUNE as usize] as f32,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Attack,
    Decay,
    Release,
}

/// Software envelope: a 16-bit integer stepped once per scan. Linear attack, exponential decay
/// and release. Retriggering continues from the present level instead of jumping to zero.
#[derive(Clone, Copy)]
struct Envelope {
    level: i32,
    stage: Stage,
}

const ENV_MAX: i32 = 65_535;

impl Envelope {
    fn step(&mut self, attack: i32, decay: i32, sustain: i32, release: i32) {
        match self.stage {
            Stage::Attack => {
                self.level += attack;
                if self.level >= ENV_MAX {
                    self.level = ENV_MAX;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => self.level = approach(self.level, sustain, decay),
            Stage::Release => self.level = approach(self.level, 0, release),
        }
    }
}

/// Exponential approach in Q16. A step that truncates to nothing still moves by one, so tails end
/// in the short linear run an integer envelope has.
fn approach(level: i32, target: i32, coef: i32) -> i32 {
    let diff = target - level;
    if diff == 0 {
        return level;
    }
    let delta = ((diff as i64 * coef as i64) >> 16) as i32;
    if delta == 0 {
        level + diff.signum()
    } else {
        level + delta
    }
}

/// Time from slider position, exponential between the end points in the specifications.
fn slider_time(s: f32, min: f32, max: f32) -> f32 {
    min * (max / min).powf(s)
}

/// Per-scan Q16 coefficient reaching the point where the VCA closes (about 3 % of full CV, set by
/// the transistor driving it) in `seconds`, which is how the specified times read in practice.
fn decay_coef(seconds: f32) -> i32 {
    let c = 1.0 - (-SCAN_S * 3.5 / seconds).exp();
    (c * 65_536.0).round() as i32
}

#[derive(Clone, Copy)]
struct Channel {
    key: u8,
    gate: bool,
    /// Set by a note-on; consumed by the next scan so that a very short note still sounds.
    trigger: bool,
    /// Note is released but held by the pedal.
    sustained: bool,
    age: u64,
    pitch: f32,
    env: Envelope,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            key: 60,
            gate: false,
            trigger: false,
            sustained: false,
            age: 0,
            pitch: 60.0,
            env: Envelope {
                level: 0,
                stage: Stage::Release,
            },
        }
    }
}

/// CV values produced by one scan, waiting for their sample-and-hold slot.
#[derive(Clone, Copy, Default)]
pub struct ScanOut {
    pub noise_v: f32,
    pub res_v: f32,
    pub level_v: f32,
    pub sub_v: f32,
    /// Volts at the comparator: +6 V is 50 %, +0.6 V is 95 %, -0.8 V disables the pulse.
    pub pwm_v: f32,
    pub saw: bool,
    pub range: usize,
    pub hpf: usize,
    pub chorus: usize,
    pub dco_v: [f32; VOICES],
    pub divisor: [u32; VOICES],
    pub vcf_v: [f32; VOICES],
    pub vca_v: [f32; VOICES],
}

const MAX_HELD: usize = 16;

pub struct Firmware {
    /// Each key's offset from 12-TET, as last heard from a tuning system.
    pub key_tuning: [f32; 128],
    channels: [Channel; VOICES],
    rotary_next: usize,
    clock: u64,
    held: [u8; MAX_HELD],
    held_count: usize,
    assign: usize,
    pub bend: f32,
    pub mod_wheel: f32,
    pedal: bool,
    lfo_phase: f32,
    lfo_held_s: f32,
}

impl Firmware {
    pub fn new() -> Self {
        Self {
            channels: [Channel::default(); VOICES],
            rotary_next: 0,
            clock: 0,
            held: [0; MAX_HELD],
            held_count: 0,
            assign: 0,
            bend: 0.0,
            key_tuning: [0.0; 128],
            mod_wheel: 0.0,
            pedal: false,
            lfo_phase: 0.0,
            lfo_held_s: 0.0,
        }
    }

    fn start(&mut self, ch: usize, key: u8) {
        self.clock += 1;
        let c = &mut self.channels[ch];
        c.key = key;
        c.gate = true;
        c.sustained = false;
        c.trigger = true;
        c.age = self.clock;
    }

    pub fn note_on(&mut self, key: u8) {
        if self.assign == 2 {
            self.hold_key(key);
            let first = !self.channels.iter().any(|c| c.gate);
            for ch in 0..VOICES {
                if first {
                    self.start(ch, key);
                } else {
                    self.channels[ch].key = key;
                }
            }
            return;
        }
        // Re-striking a held key reuses its channel.
        if let Some(ch) = self.channels.iter().position(|c| c.gate && c.key == key) {
            self.start(ch, key);
            return;
        }
        let free = |c: &Channel| !c.gate;
        let ch = if self.assign == 0 {
            // Poly 1, rotary: keep walking round the channels, releases ring on.
            (0..VOICES)
                .map(|i| (self.rotary_next + i) % VOICES)
                .find(|&i| free(&self.channels[i]))
        } else {
            // Poly 2, non-rotary: always the lowest free channel.
            (0..VOICES).find(|&i| free(&self.channels[i]))
        };
        let ch = ch.unwrap_or_else(|| {
            // Seventh key steals from the first.
            (0..VOICES)
                .min_by_key(|&i| self.channels[i].age)
                .unwrap_or(0)
        });
        self.rotary_next = (ch + 1) % VOICES;
        self.start(ch, key);
    }

    fn hold_key(&mut self, key: u8) {
        self.unhold_key(key);
        if self.held_count == MAX_HELD {
            self.held.copy_within(1.., 0);
            self.held_count -= 1;
        }
        self.held[self.held_count] = key;
        self.held_count += 1;
    }

    fn unhold_key(&mut self, key: u8) {
        if let Some(i) = self.held[..self.held_count].iter().position(|k| *k == key) {
            self.held.copy_within(i + 1..self.held_count, i);
            self.held_count -= 1;
        }
    }

    pub fn note_off(&mut self, key: u8) {
        if self.assign == 2 {
            self.unhold_key(key);
            if self.held_count > 0 {
                let top = self.held[self.held_count - 1];
                self.channels.iter_mut().for_each(|c| c.key = top);
            } else {
                self.release_all_gates();
            }
            return;
        }
        for c in self.channels.iter_mut().filter(|c| c.gate && c.key == key) {
            if self.pedal {
                c.sustained = true;
            } else {
                c.gate = false;
            }
        }
    }

    fn release_all_gates(&mut self) {
        let pedal = self.pedal;
        for c in self.channels.iter_mut() {
            if pedal {
                c.sustained = c.gate;
            } else {
                c.gate = false;
            }
        }
    }

    pub fn set_pedal(&mut self, down: bool) {
        self.pedal = down;
        if !down {
            for c in self.channels.iter_mut().filter(|c| c.sustained) {
                c.gate = false;
                c.sustained = false;
            }
        }
    }

    pub fn all_off(&mut self) {
        self.held_count = 0;
        self.pedal = false;
        for c in self.channels.iter_mut() {
            c.gate = false;
            c.sustained = false;
            c.trigger = false;
        }
    }

    /// One pass of the firmware's main loop.
    pub fn scan(&mut self, panel: &Panel, unit: &Unit, k: f32) -> ScanOut {
        if panel.assign != self.assign {
            self.all_off();
            self.assign = panel.assign;
        }

        // LFO: one software triangle shared by all voices, 0.1 Hz to 30 Hz.
        let rate = 0.1 * 300f32.powf(panel.lfo_rate);
        self.lfo_phase = (self.lfo_phase + rate * SCAN_S).fract();
        let tri = 1.0 - 4.0 * (self.lfo_phase - 0.5).abs();

        // The delay restarts only once every key has been let go.
        if self.channels.iter().any(|c| c.gate) {
            self.lfo_held_s += SCAN_S;
        } else {
            self.lfo_held_s = 0.0;
        }
        let delay = panel.lfo_delay * 3.0;
        let fade = if delay > 0.0 {
            (self.lfo_held_s / delay).min(1.0)
        } else {
            1.0
        };
        let lfo = tri * fade.max(self.mod_wheel * panel.bend_lfo);

        let attack = (ENV_MAX as f32 * SCAN_S / slider_time(panel.a, 1.5e-3, 3.0)) as i32;
        let decay = decay_coef(slider_time(panel.d, 1.5e-3, 12.0));
        let release = decay_coef(slider_time(panel.r, 1.5e-3, 12.0));
        let sustain = (panel.s * ENV_MAX as f32) as i32;
        let porta_st = if panel.porta_on && panel.porta > 0.0 {
            // Linear in pitch, up to 3 s per octave.
            12.0 * SCAN_S / (0.003 * 1000f32.powf(panel.porta))
        } else {
            f32::INFINITY
        };

        let bend = self.bend;
        let mut out = ScanOut {
            noise_v: 10.0 * dac(unit, k, panel.noise),
            res_v: 10.0 * dac(unit, k, panel.res),
            level_v: dac(unit, k, panel.level),
            sub_v: 10.0 * dac(unit, k, panel.sub),
            pwm_v: self.pwm_volts(panel, tri, unit, k),
            saw: panel.saw,
            range: panel.range,
            hpf: panel.hpf,
            chorus: panel.chorus,
            ..Default::default()
        };

        for (i, c) in self.channels.iter_mut().enumerate() {
            if c.trigger {
                c.trigger = false;
                c.env.stage = Stage::Attack;
            } else if !c.gate {
                c.env.stage = Stage::Release;
            }
            c.env.step(attack, decay, sustain, release);

            let target = c.key as f32 + self.key_tuning[(c.key & 127) as usize];
            let step = (target - c.pitch).clamp(-porta_st, porta_st);
            c.pitch += step;

            let cents = panel.tune + lfo * panel.dco_lfo * 400.0 + bend * panel.bend_dco * 1200.0;
            let semis = c.pitch + cents / 100.0;
            let f8 = A4_HZ * 2f32.powf((semis - 69.0) / 12.0);

            // Integer counter divisor: the source of the DCO's pitch quantisation.
            out.divisor[i] = (MASTER_CLOCK_8FT_HZ / f8).round().clamp(2.0, 65_535.0) as u32;

            // The firmware raises the integrator charge voltage with pitch so the saw stays 12 Vp-p.
            out.dco_v[i] = 10.0 * dac(unit, k, f8 * 2.4e-3 / 10.0);

            let env = c.env.level as f32 / ENV_MAX as f32;
            let sign = if panel.invert { -1.0 } else { 1.0 };
            let oct = (panel.freq - VCF_REF_SLIDER) * 14.0
                + panel.kybd * (c.pitch - 60.0) / 12.0
                + sign * env * panel.env * 14.0
                + lfo * panel.vcf_lfo * 3.5
                + bend * panel.bend_vcf * 3.5;
            let v = vcf_ref_v() + oct / VCF_OCT_PER_V;
            let span = VCF_V_MAX - VCF_V_MIN;
            out.vcf_v[i] = VCF_V_MIN + span * dac(unit, k, (v - VCF_V_MIN) / span);

            let level = if panel.gate_mode {
                if c.gate {
                    1.0
                } else {
                    0.0
                }
            } else {
                env
            };
            out.vca_v[i] = 10.0 * dac(unit, k, level);
        }
        out
    }

    fn pwm_volts(&self, panel: &Panel, tri: f32, unit: &Unit, k: f32) -> f32 {
        if !panel.pulse {
            return -0.8;
        }
        let amount = if panel.pwm_man {
            panel.pwm
        } else {
            panel.pwm * 0.5 * (tri + 1.0)
        };
        // +6 V at the centre of the slider's travel down to +0.6 V at full, through the 12-bit DAC.
        let v = 6.0 - 5.4 * amount;
        const SPAN: f32 = 7.5;
        -1.0 + SPAN * dac(unit, k, (v + 1.0) / SPAN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gated(f: &Firmware) -> Vec<usize> {
        (0..VOICES).filter(|&i| f.channels[i].gate).collect()
    }

    #[test]
    fn poly2_staccato_always_uses_channel_one() {
        let mut f = Firmware::new();
        f.assign = 1;
        for key in [60, 64, 67] {
            f.note_on(key);
            assert_eq!(gated(&f), vec![0]);
            f.note_off(key);
        }
    }

    #[test]
    fn poly1_rotates_and_the_seventh_key_steals_the_first() {
        let mut f = Firmware::new();
        f.note_on(60);
        f.note_off(60);
        f.note_on(62);
        assert_eq!(gated(&f), vec![1]);
        f.note_off(62);
        for (i, key) in (70..76).enumerate() {
            f.note_on(key);
            assert_eq!(f.channels[(i + 2) % VOICES].key, key);
        }
        f.note_on(80);
        assert_eq!(f.channels[2].key, 80);
    }

    #[test]
    fn unison_uses_every_channel_with_last_note_priority() {
        let mut f = Firmware::new();
        f.assign = 2;
        f.note_on(60);
        assert_eq!(gated(&f).len(), VOICES);
        f.note_on(67);
        assert!(f.channels.iter().all(|c| c.key == 67));
        f.note_off(67);
        assert!(f.channels.iter().all(|c| c.key == 60 && c.gate));
        f.note_off(60);
        assert!(gated(&f).is_empty());
    }

    #[test]
    fn pedal_holds_released_keys() {
        let mut f = Firmware::new();
        f.set_pedal(true);
        f.note_on(60);
        f.note_off(60);
        assert_eq!(gated(&f).len(), 1);
        f.set_pedal(false);
        assert!(gated(&f).is_empty());
    }

    #[test]
    fn decay_tail_terminates_exactly() {
        let mut level = 3000;
        let coef = decay_coef(0.05);
        for _ in 0..4000 {
            level = approach(level, 0, coef);
        }
        assert_eq!(level, 0);
    }
}
