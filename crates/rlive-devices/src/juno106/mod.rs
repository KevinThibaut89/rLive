//! Circuit-level emulation of the Roland Juno-106, built from its 1984 service notes.
//!
//! Signal path, as on the module and jack boards:
//!
//! ```text
//! 8 MHz clock -> 8253 counters -> MC5534A saw/pulse/sub ---+
//!                                       shared noise ------+-> 80017A VCF -> 80017A VCA (x6)
//!   -> summing amp -> HPF -> global VCA -> MN3009 chorus -> output
//! ```
//!
//! All of it runs at an oversampled rate and is decimated once at the output. The panel exposes
//! the real machine's controls, plus four that describe the physical unit instead:
//! `Instability`, `Unit` (a serial number selecting one set of component tolerances),
//! `Cold Start` and `Hiss/Hum`.

mod analog;
mod chorus;
mod control;
mod dco;
mod unit;
mod vcf;

use analog::{Biquad, Decimator, OnePole, Rng};
use chorus::{ChannelTrim, Chorus};
use control::{Firmware, Panel, ScanOut};
use dco::{Dco, DcoInputs};
use rlive_core::{db_to_gain, BlockStep, MidiMessage, ParamInfo, ProcessContext, Processor, Tail};
use unit::{Unit, VoiceDraw, VoiceDrift, VOICES};
use vcf::Ota4;

pub use control::A4_HZ;

pub mod p {
    pub const LFO_RATE: u32 = 0;
    pub const LFO_DELAY: u32 = 1;
    pub const RANGE: u32 = 2;
    pub const DCO_LFO: u32 = 3;
    pub const PWM: u32 = 4;
    pub const PWM_MODE: u32 = 5;
    pub const PULSE: u32 = 6;
    pub const SAW: u32 = 7;
    pub const SUB: u32 = 8;
    pub const NOISE: u32 = 9;
    pub const HPF: u32 = 10;
    pub const VCF_FREQ: u32 = 11;
    pub const VCF_RES: u32 = 12;
    pub const VCF_POLARITY: u32 = 13;
    pub const VCF_ENV: u32 = 14;
    pub const VCF_LFO: u32 = 15;
    pub const VCF_KYBD: u32 = 16;
    pub const VCA_MODE: u32 = 17;
    pub const VCA_LEVEL: u32 = 18;
    pub const ENV_A: u32 = 19;
    pub const ENV_D: u32 = 20;
    pub const ENV_S: u32 = 21;
    pub const ENV_R: u32 = 22;
    pub const CHORUS: u32 = 23;
    pub const BEND_DCO: u32 = 24;
    pub const BEND_VCF: u32 = 25;
    pub const BEND_LFO: u32 = 26;
    pub const PORTAMENTO: u32 = 27;
    pub const PORTAMENTO_ON: u32 = 28;
    pub const ASSIGN: u32 = 29;
    pub const TUNE: u32 = 30;
    pub const VOLUME: u32 = 31;
    pub const INSTABILITY: u32 = 32;
    pub const UNIT: u32 = 33;
    pub const COLD_START: u32 = 34;
    pub const HISS: u32 = 35;
    pub const MAINS: u32 = 36;
}

fn slider(id: u32, name: &str, default: f64) -> ParamInfo {
    ParamInfo::continuous(id, name, 0.0, 10.0, default, "")
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        slider(p::LFO_RATE, "LFO Rate", 3.0),
        slider(p::LFO_DELAY, "LFO Delay", 0.0),
        ParamInfo::stepped(p::RANGE, "Range", &["16'", "8'", "4'"], 1),
        slider(p::DCO_LFO, "DCO LFO", 0.0),
        slider(p::PWM, "PWM", 0.0),
        ParamInfo::stepped(p::PWM_MODE, "PWM Mode", &["LFO", "Man"], 1),
        ParamInfo::toggle(p::PULSE, "Pulse", false),
        ParamInfo::toggle(p::SAW, "Saw", true),
        slider(p::SUB, "Sub", 0.0),
        slider(p::NOISE, "Noise", 0.0),
        ParamInfo::stepped(p::HPF, "HPF", &["0", "1", "2", "3"], 1),
        slider(p::VCF_FREQ, "VCF Freq", 7.0),
        slider(p::VCF_RES, "VCF Res", 0.5),
        ParamInfo::stepped(p::VCF_POLARITY, "VCF Pol", &["Norm", "Inv"], 0),
        slider(p::VCF_ENV, "VCF Env", 3.0),
        slider(p::VCF_LFO, "VCF LFO", 0.0),
        slider(p::VCF_KYBD, "VCF Kybd", 5.0),
        ParamInfo::stepped(p::VCA_MODE, "VCA Mode", &["Env", "Gate"], 0),
        slider(p::VCA_LEVEL, "VCA Level", 5.0),
        slider(p::ENV_A, "Attack", 0.5),
        slider(p::ENV_D, "Decay", 4.0),
        slider(p::ENV_S, "Sustain", 6.0),
        slider(p::ENV_R, "Release", 3.0),
        ParamInfo::stepped(p::CHORUS, "Chorus", &["Off", "I", "II"], 1),
        slider(p::BEND_DCO, "Bend DCO", 1.67),
        slider(p::BEND_VCF, "Bend VCF", 0.0),
        slider(p::BEND_LFO, "LFO Sens", 5.0),
        slider(p::PORTAMENTO, "Porta", 0.0),
        ParamInfo::toggle(p::PORTAMENTO_ON, "Porta On", false),
        ParamInfo::stepped(p::ASSIGN, "Assign", &["Poly 1", "Poly 2", "Unison"], 0),
        ParamInfo::continuous(p::TUNE, "Tune", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(p::VOLUME, "Volume", -60.0, 6.0, 0.0, "dB"),
        ParamInfo::continuous(p::INSTABILITY, "Instability", 0.0, 2.0, 1.0, ""),
        ParamInfo::continuous(p::UNIT, "Unit", 0.0, 999.0, 106.0, ""),
        ParamInfo::toggle(p::COLD_START, "Cold Start", false),
        ParamInfo::continuous(p::HISS, "Hiss/Hum", 0.0, 2.0, 1.0, ""),
        ParamInfo::stepped(p::MAINS, "Mains", &["50 Hz", "60 Hz"], 0),
    ]
}

// ---- values read from the module board schematic -------------------------------------------

const SLOTS: usize = 23;
const INTEGRATOR_C: f64 = 1e-9;
/// R85, R87, R86: selected by the 4052 according to RANGE.
const INTEGRATOR_R: [f64; 3] = [399e3, 200e3, 100e3];
const CLOCK_DIVIDE: [f64; 3] = [8.0, 4.0, 2.0];
const MASTER_CLOCK_HZ: f64 = 8.0e6;

/// CV smoothing after each S/H. VCF: R113/R110 network into C61. VCA: R106 into C58.
/// PWM: C62 across R118 + VR31. Global level: R30 into C7.
const VCF_CV_TAU: f64 = 0.5e-3;
const VCA_CV_TAU: f64 = 1.0e-3;
const PWM_CV_TAU: f64 = 3.1e-3;
const LEVEL_CV_TAU: f64 = 22e-3;

// Levels at the VCF input node, in volts peak-to-peak. Noise follows the 4 Vp-p NOISE LEVEL
// adjustment at VCA output, and saw/pulse/sub are estimated from the mixing resistors.
const SAW_TO_NODE: f32 = 4.0 / 12.0;
const PULSE_NODE_V: f32 = 4.0;
const SUB_TO_NODE: f32 = 0.48;
const NOISE_NODE_RMS_FULL: f64 = 0.48;

/// VCA GAIN trimmer: 4.8 Vp-p at the VCF becomes 6 Vp-p at the VCA.
const VCA_GAIN: f32 = 1.25;
const VCA_SAT_V: f32 = 6.0;
/// Each voice sums through 33 k into a 3.3 k feedback resistor.
const VOICE_TO_SUM: f32 = 0.1;

/// Resonance loop gain at full RES CV. A four-pole loop needs 4 to oscillate. Together with the
/// saturation points in `vcf` this gives the 6 Vp-p sine the VCA GAIN adjustment asks for.
const RES_K_MAX: f32 = 4.1;

/// The technician's FREQ and WIDTH trimmers: an oscillating filter runs flat of its small-signal
/// cutoff, so these are set for 248 Hz at C4 and 992 Hz at C6 at full resonance, as in the notes.
const VCF_FREQ_TRIM: f32 = 1.0896;
const VCF_WIDTH_TRIM: f32 = 0.9896;

/// Voltage that maps to digital full scale at the output.
const FULL_SCALE_V: f32 = 8.0;
/// IC6 runs from ±15 V and clips a little short of the rails.
const OUTPUT_RAIL_V: f32 = 13.5;

/// Base-emitter turn-on of the transistors that convert CVs into OTA bias currents.
fn conduction(v: f32, span: f32) -> f32 {
    const VBE: f32 = 0.55;
    const KNEE: f32 = 0.04;
    let x = v - VBE;
    0.5 * (x + (x * x + KNEE * KNEE).sqrt()) / span
}

#[inline]
fn rail(x: f32) -> f32 {
    OUTPUT_RAIL_V * (x / OUTPUT_RAIL_V).tanh()
}

#[derive(Clone, Copy)]
struct Voice {
    dco: Dco,
    vcf: Ota4,
    input_ac: OnePole,
    output_ac: OnePole,
    dco_v: f32,
    divisor: u32,
    vcf_sh: f32,
    vca_sh: f32,
    vcf_cv: f32,
    vca_cv: f32,
    g: [f32; 4],
    offset: [f32; 4],
    amp: f32,
    vca_offset: f32,
}

/// The HPF board: 4052 selects one of four paths around IC4.
struct HighPass {
    input_ac: OnePole,
    hp226: OnePole,
    hp720: OnePole,
    boost: Biquad,
}

impl HighPass {
    fn new(sr: f64) -> Self {
        // Position 0: flat path plus IC4b's shelf through R24. From the schematic,
        // H(s) = 1 + (47k/220k)·(1 + s·T1)(11 + s·T2) / ((1 + s·T3)(1 + s·T2)).
        let t1 = 47e3 * 0.047e-6;
        let t2 = 100e3 * 0.022e-6;
        let t3 = 47e3 * (0.047e-6 + 0.01e-6);
        let g = 47.0 / 220.0;
        let den = [1.0, t2 + t3, t2 * t3];
        let num = [
            den[0] + g * 11.0,
            den[1] + g * (t2 + 11.0 * t1),
            den[2] + g * t1 * t2,
        ];
        Self {
            input_ac: OnePole::new(0.48, sr),
            hp226: OnePole::new(1.0 / (std::f64::consts::TAU * 47e3 * 0.015e-6), sr),
            hp720: OnePole::new(1.0 / (std::f64::consts::TAU * 47e3 * 0.0047e-6), sr),
            boost: Biquad::from_analog(num, den, sr),
        }
    }

    #[inline]
    fn process(&mut self, x: f32, mode: usize) -> f32 {
        let x = self.input_ac.highpass(x);
        // Every path is kept running so a switch change only clicks as much as the real one.
        let hp226 = self.hp226.highpass(x);
        let hp720 = self.hp720.highpass(x);
        let boost = self.boost.process(x);
        match mode {
            0 => boost,
            1 => x,
            2 => hp226,
            _ => hp720,
        }
    }
}

pub struct Juno106 {
    /// Silent since `reset`, until the next note. The machine keeps running underneath.
    muted: bool,
    values: Vec<f64>,
    sr: f64,
    os: usize,
    os_sr: f64,
    dt: f64,

    unit: Unit,
    serial: u32,
    k: f32,
    fw: Firmware,
    next: ScanOut,
    slot: usize,
    slot_timer: f64,
    slot_len: f64,

    voices: [Voice; VOICES],
    noise_rng: Rng,
    noise_shape: [OnePole; 2],
    noise_gain: f32,

    noise_sh: f32,
    res_sh: f32,
    level_sh: f32,
    sub_sh: f32,
    pwm_sh: f32,
    pwm_cv: f32,
    level_cv: f32,
    saw: bool,
    range: usize,
    hpf_mode: usize,
    chorus_mode: usize,
    clock_factor: f64,
    res_k: [f32; VOICES],

    hpf: Option<HighPass>,
    chorus: Option<Chorus>,
    output_ac: [OnePole; 2],
    decim: Option<[Decimator; 2]>,

    volume: f32,
    hum_phase: f64,
    floor_rng: Rng,
    a_vcf: f32,
    a_vca: f32,
    a_pwm: f32,
    a_level: f32,
    a_volume: f32,
}

impl Default for Juno106 {
    fn default() -> Self {
        Self::new()
    }
}

fn smoothing(tau: f64, rate: f64) -> f32 {
    (1.0 - (-1.0 / (tau * rate)).exp()) as f32
}

impl Juno106 {
    pub fn new() -> Self {
        let values: Vec<f64> = params().iter().map(|p| p.default).collect();
        let serial = values[p::UNIT as usize] as u32;
        let unit = Unit::new(serial);
        let mut s = Self {
            muted: false,
            values,
            sr: 48_000.0,
            os: 4,
            os_sr: 192_000.0,
            dt: 1.0 / 192_000.0,
            unit,
            serial,
            k: 1.0,
            fw: Firmware::new(),
            next: ScanOut::default(),
            slot: 0,
            slot_timer: 0.0,
            slot_len: 1.0,
            voices: [Voice {
                dco: Dco::new(0.0, 1e-3),
                vcf: Ota4::default(),
                input_ac: OnePole::default(),
                output_ac: OnePole::default(),
                dco_v: 0.0,
                divisor: 4545,
                vcf_sh: 0.0,
                vca_sh: 0.0,
                vcf_cv: 0.0,
                vca_cv: 0.0,
                g: [0.01; 4],
                offset: [0.0; 4],
                amp: 0.0,
                vca_offset: 0.0,
            }; VOICES],
            noise_rng: Rng::new(0x5EED),
            noise_shape: [OnePole::default(); 2],
            noise_gain: 1.0,
            noise_sh: 0.0,
            res_sh: 0.0,
            level_sh: 0.5,
            sub_sh: 0.0,
            pwm_sh: -0.8,
            pwm_cv: -0.8,
            level_cv: 0.5,
            saw: true,
            range: 1,
            hpf_mode: 1,
            chorus_mode: 0,
            clock_factor: 1.0,
            res_k: [0.0; VOICES],
            hpf: None,
            chorus: None,
            output_ac: [OnePole::default(); 2],
            decim: None,
            volume: 1.0,
            hum_phase: 0.0,
            floor_rng: Rng::new(0xF100),
            a_vcf: 1.0,
            a_vca: 1.0,
            a_pwm: 1.0,
            a_level: 1.0,
            a_volume: 1.0,
        };
        s.configure(48_000.0);
        s
    }

    fn value(&self, id: u32) -> f64 {
        self.values[id as usize]
    }

    fn chorus_trims(&self) -> [ChannelTrim; 2] {
        let k = self.k;
        [0, 1].map(|c| ChannelTrim {
            clock: (k * 0.02 * self.unit.chorus_clock[c]) as f64,
            filter: (k * 0.015 * self.unit.chorus_filter[c]) as f64,
            bias: k * 0.12 * self.unit.chorus_bias[c],
        })
    }

    fn configure(&mut self, sample_rate: f64) {
        self.sr = sample_rate;
        // Keep the analog core near 192 kHz regardless of the host rate.
        self.os = ((192_000.0 / sample_rate).round() as usize).clamp(1, 8);
        self.os_sr = sample_rate * self.os as f64;
        self.dt = 1.0 / self.os_sr;
        self.slot_len = control::SCAN_S as f64 / SLOTS as f64 * self.os_sr;
        self.slot = 0;
        self.slot_timer = 0.0;

        // The control-rate smoothers run once per host sample.
        self.a_vcf = smoothing(VCF_CV_TAU, sample_rate);
        self.a_vca = smoothing(VCA_CV_TAU, sample_rate);
        self.a_pwm = smoothing(PWM_CV_TAU, sample_rate);
        self.a_level = smoothing(LEVEL_CV_TAU, sample_rate);
        self.a_volume = smoothing(5e-3, sample_rate);

        // A powered-on machine's coupling capacitors have long since charged to the average of
        // the waveform; starting them empty would thump on the first note after loading.
        let saw_dc = if self.value(p::SAW) >= 0.5 {
            6.0 * SAW_TO_NODE
        } else {
            0.0
        };
        let pulse_dc = if self.value(p::PULSE) >= 0.5 {
            0.5
        } else {
            1.0
        } * PULSE_NODE_V;
        let sub_dc = self.value(p::SUB) as f32 * SUB_TO_NODE * 0.5;
        let node_dc = saw_dc + pulse_dc + sub_dc;

        let period = 1.0 / control::A4_HZ as f64;
        for (i, v) in self.voices.iter_mut().enumerate() {
            v.dco = Dco::new(self.unit.voices[i].phase, period);
            v.input_ac = OnePole::new(0.48, self.os_sr);
            v.input_ac.precharge(node_dc);
            v.output_ac = OnePole::new(1.5, self.os_sr);
        }

        // IC14 output: R79 330 k with C41 100 pF; input coupling C42 1 µF into 4.7 k.
        self.noise_shape = [
            OnePole::new(4_823.0, self.os_sr),
            OnePole::new(34.0, self.os_sr),
        ];
        // White noise is scaled so the band-limited result has the specified level.
        let band = std::f64::consts::FRAC_PI_2 * 4_823.0;
        self.noise_gain = (NOISE_NODE_RMS_FULL * (self.os_sr * 0.5 / band).sqrt() / 0.577) as f32;

        self.hpf = Some(HighPass::new(self.os_sr));
        self.chorus = Some(Chorus::new(self.chorus_trims(), self.serial, self.os_sr));
        self.output_ac = [OnePole::new(2.0, self.os_sr); 2];
        self.decim = Some([Decimator::new(self.os), Decimator::new(self.os)]);
    }

    fn apply_unit_changes(&mut self) {
        let serial = self.value(p::UNIT).round().clamp(0.0, 999.0) as u32;
        if serial != self.serial {
            self.serial = serial;
            self.unit.roll(serial);
        }
        self.k = self.value(p::INSTABILITY).clamp(0.0, 2.0) as f32;
    }

    fn run_slot(&mut self) {
        match self.slot {
            0 => {
                self.apply_unit_changes();
                self.unit.scan(control::SCAN_S);
                let panel = Panel::read(&self.values);
                self.next = self.fw.scan(&panel, &self.unit, self.k);
                self.saw = self.next.saw;
                self.range = self.next.range;
                self.hpf_mode = self.next.hpf;
                self.chorus_mode = self.next.chorus;

                let k = self.k as f64;
                let cold = self.unit.cold_kelvin as f64;
                self.clock_factor = 1.0
                    + k * (0.0017 * self.unit.clock as f64
                        - 40e-6 * self.unit.clock_tempco as f64 * cold
                        + 3e-6 * self.unit.clock_wander as f64);

                self.noise_sh = self.next.noise_v;
            }
            1 => self.res_sh = self.next.res_v,
            2 => self.level_sh = self.next.level_v,
            3 => self.sub_sh = self.next.sub_v,
            s @ 4..=9 => {
                let i = s - 4;
                self.voices[i].dco_v = self.next.dco_v[i];
                self.voices[i].divisor = self.next.divisor[i];
            }
            10 => self.pwm_sh = self.next.pwm_v,
            s => {
                let i = (s - 11) / 2;
                if (s - 11) % 2 == 0 {
                    self.voices[i].vcf_sh = self.next.vcf_v[i];
                } else {
                    self.voices[i].vca_sh = self.next.vca_v[i];
                }
            }
        }
        self.slot = (self.slot + 1) % SLOTS;
    }

    /// Updates everything that moves slower than audio; called once per host sample.
    fn control_rate(&mut self) {
        let k = self.k;
        self.pwm_cv += (self.pwm_sh - self.pwm_cv) * self.a_pwm;
        self.level_cv += (self.level_sh - self.level_cv) * self.a_level;
        let res_i = conduction(self.res_sh, 9.45);
        let ref_v = control::vcf_ref_v();
        let nyquist_guard = (0.45 * self.os_sr) as f32;
        let cold = self.unit.cold_kelvin;

        for (i, v) in self.voices.iter_mut().enumerate() {
            let d: &VoiceDraw = &self.unit.voices[i];
            let w: &VoiceDrift = &self.unit.drift[i];
            v.vcf_cv += (v.vcf_sh - v.vcf_cv) * self.a_vcf;
            v.vca_cv += (v.vca_sh - v.vca_cv) * self.a_vca;

            // Service notes tolerance is ±10 cents after trimming; temperature moves it further.
            let cents = k
                * (5.0 * d.cutoff_trim
                    + 4.0 * w.cutoff_slow
                    + 0.8 * w.cutoff_fast
                    + 2.5 * d.cutoff_tempco * cold);
            let scale =
                control::VCF_OCT_PER_V * VCF_WIDTH_TRIM * (1.0 + k * 0.003 * d.cutoff_scale);
            let oct = (v.vcf_cv - ref_v) * scale + cents / 1200.0;
            let hz = (control::VCF_REF_HZ * VCF_FREQ_TRIM * 2f32.powf(oct))
                .clamp(5.0, nyquist_guard.min(50_000.0));
            let g = (std::f32::consts::PI * hz / self.os_sr as f32).tan();
            for s in 0..4 {
                v.g[s] = g * (1.0 + k * 0.01 * d.stage_gm[s]);
                v.offset[s] = k * 0.015 * d.stage_offset[s];
            }

            // VCA BIAS trimmer holds the DAC floor at +0.26 V; below turn-on the voice closes.
            let i_vca = conduction(v.vca_cv + 0.26, 9.71);
            v.amp = VCA_GAIN * i_vca * (1.0 + k * (0.03 * d.vca_gain + 0.005 * w.vca));
            v.vca_offset = k * 0.02 * d.vca_offset;
            self.res_k[i] = RES_K_MAX * res_i * (1.0 + k * 0.015 * d.resonance);
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let (Some(mut hpf), Some(mut chorus), Some(mut decim)) =
            (self.hpf.take(), self.chorus.take(), self.decim.take())
        else {
            return;
        };

        let rate = if self.chorus_mode == 2 {
            chorus::RATE_II
        } else {
            chorus::RATE_I
        } * (1.0 + (self.k * 0.04 * self.unit.chorus_rate) as f64);
        let hiss = self.value(p::HISS) as f32;
        let hum_hz = if self.value(p::MAINS) >= 0.5 {
            60.0
        } else {
            50.0
        };
        let volume = if self.value(p::VOLUME) <= -60.0 {
            0.0
        } else {
            db_to_gain(self.value(p::VOLUME) as f32)
        };

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.control_rate();

            for _ in 0..self.os {
                self.slot_timer -= 1.0;
                while self.slot_timer <= 0.0 {
                    self.run_slot();
                    self.slot_timer += self.slot_len;
                }

                // One noise source feeds all six voices.
                let white = self.noise_rng.uniform() * self.noise_gain;
                let lp = self.noise_shape[0].lowpass(white);
                let noise = self.noise_shape[1].highpass(lp) * conduction(self.noise_sh, 9.45);

                let k = self.k as f64;
                let clk = MASTER_CLOCK_HZ * self.clock_factor / CLOCK_DIVIDE[self.range];
                let r_int = INTEGRATOR_R[self.range];
                let sub_v = self.sub_sh * SUB_TO_NODE;
                let saw_gain = if self.saw { SAW_TO_NODE } else { 0.0 };

                let mut sum = 0.0f32;
                for (i, v) in self.voices.iter_mut().enumerate() {
                    let d = &self.unit.voices[i];
                    let rc =
                        r_int * INTEGRATOR_C * (1.0 + k * 0.012 * d.integrator[self.range] as f64);
                    let inputs = DcoInputs {
                        slope: v.dco_v as f64 / rc,
                        pwm_v: self.pwm_cv as f64 + k * 0.09 * d.comparator as f64,
                        saw: saw_gain,
                        pulse_v: PULSE_NODE_V,
                        sub_v,
                        next_period: v.divisor as f64 / clk,
                    };
                    let node = v.dco.tick(self.dt, &inputs) + noise;
                    let vcf_in = v.input_ac.highpass(node);
                    let y = v.vcf.process(vcf_in, &v.g, self.res_k[i], &v.offset);
                    let vca_in = v.output_ac.highpass(y) + v.vca_offset;
                    let out = v.amp * VCA_SAT_V * (vca_in / VCA_SAT_V).tanh();
                    sum += out;
                }

                let mono = hpf.process(sum * VOICE_TO_SUM, self.hpf_mode);
                // IC5: the VCA Level slider covers ±10 dB around its centre.
                let level = 10f32.powf((self.level_cv - 0.5) * 20.0 / 20.0);
                let post_vca = mono * level;

                let (wet_l, wet_r) = chorus.process(post_vca, self.chorus_mode != 0, rate, hiss);
                let dry = post_vca * chorus::DRY_GAIN;
                let out_l = self.output_ac[0].highpass(rail(dry + wet_l * chorus::WET_GAIN));
                let out_r = self.output_ac[1].highpass(rail(dry + wet_r * chorus::WET_GAIN));
                decim[0].push(out_l);
                decim[1].push(out_r);
            }

            self.volume += (volume - self.volume) * self.a_volume;

            // Mains hum and amplifier noise exist whether or not anything is playing.
            self.hum_phase = (self.hum_phase + hum_hz / self.sr).fract();
            let t = std::f64::consts::TAU * self.hum_phase;
            let hum = (t.sin() + 0.5 * (2.0 * t + 0.7).sin() + 0.3 * (3.0 * t + 1.9).sin()) as f32;
            let floor = hiss * (6e-5 * hum + 1.5e-5 * self.floor_rng.uniform());

            let g = self.volume / FULL_SCALE_V;
            *l += decim[0].read() * g + floor * self.volume;
            *r += decim[1].read() * g + floor * self.volume;
        }

        self.hpf = Some(hpf);
        self.chorus = Some(chorus);
        self.decim = Some(decim);
    }

    fn handle(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { key, tuning, .. } => {
                self.fw.key_tuning[(key & 127) as usize] = tuning;
                self.fw.note_on(key);
            }
            MidiMessage::NoteOff { key, .. } => self.fw.note_off(key),
            MidiMessage::AllNotesOff => self.fw.all_off(),
            MidiMessage::PitchBend { value, .. } => self.fw.bend = value,
            MidiMessage::ControlChange { cc: 1, value, .. } => {
                self.fw.mod_wheel = value as f32 / 127.0;
            }
            MidiMessage::ControlChange { cc: 64, value, .. } => self.fw.set_pedal(value >= 64),
            MidiMessage::ControlChange { cc: 120, .. } => self.fw.all_off(),
            _ => {}
        }
    }
}

impl Processor for Juno106 {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.apply_unit_changes();
        self.unit.power_on(self.value(p::COLD_START) >= 0.5);
        self.configure(sample_rate);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => {
                self.render(audio.left, audio.right);
                if self.muted {
                    audio.left.fill(0.0);
                    audio.right.fill(0.0);
                }
            }
            BlockStep::Midi(message) => {
                if matches!(message, MidiMessage::NoteOn { .. }) {
                    self.muted = false;
                }
                self.handle(message);
            }
        });
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        // Bypass and choke need silence now, but the analog side of a powered-on machine carries
        // on running, so drift and thermal state are kept and only the output is held off.
        self.fw.all_off();
        self.muted = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain(mode: usize, hz: f64) -> f32 {
        let sr = 192_000.0;
        let mut h = HighPass::new(sr);
        let n = 4 * sr as usize;
        let mut peak = 0.0f32;
        for i in 0..n {
            let x = (std::f64::consts::TAU * hz * i as f64 / sr).sin() as f32;
            let y = h.process(x, mode);
            if i > 3 * n / 4 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    fn db(x: f32) -> f32 {
        20.0 * x.log10()
    }

    #[test]
    fn hpf_corners_match_the_schematic_rc_values() {
        assert!((db(gain(2, 225.8)) + 3.0).abs() < 0.3);
        assert!((db(gain(3, 720.5)) + 3.0).abs() < 0.3);
        assert!(db(gain(1, 1000.0)).abs() < 0.05);
    }

    #[test]
    fn hpf_position_zero_is_a_bass_shelf() {
        let low = db(gain(0, 20.0));
        let high = db(gain(0, 5000.0));
        assert!((low - 10.0).abs() < 0.5, "{low}"); // 10.5 dB at DC, still settling at 20 Hz.
        assert!((high - 1.4).abs() < 0.3, "{high}");
    }
}
