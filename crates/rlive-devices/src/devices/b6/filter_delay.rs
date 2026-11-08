//! Filter Delay: three delay lines, each behind a linked low-pass and high-pass.
//!
//! From the manual: line 1 hears the left channel, line 2 both and line 3 the right; each has
//! its own On and filter switches, a filter with centre and width, Sync with Offset, Feedback
//! that returns through the filter, Pan, and a Volume up to +6 dB; plus Dry. The manual gives no
//! ranges: time 1 ms to 5 s, Offset +-33%, filter 50 Hz to 18 kHz, width 0.5 to 9 octaves and
//! Feedback up to 110%, with the path saturating so it stays finite.

use super::common::{
    guard, knob, menu, numbered_beats, toggle, values, DelayHead, LinkedBand, NUMBERED_LABELS,
};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor,
    Section, Status, Tail,
};

pub const DRY: u32 = 0;
const TAP_BASE: u32 = 1;
const TAP_STRIDE: u32 = 11;
// Offsets within one line's block of parameters.
pub const ON: u32 = 0;
pub const FILTER_ON: u32 = 1;
pub const FREQ: u32 = 2;
pub const WIDTH: u32 = 3;
pub const SYNC: u32 = 4;
pub const TIME: u32 = 5;
pub const DIVISION: u32 = 6;
pub const OFFSET: u32 = 7;
pub const FEEDBACK: u32 = 8;
pub const PAN: u32 = 9;
pub const VOLUME: u32 = 10;
const PARAMS: usize = 34;

/// The id of a parameter of line `tap`, counting from 0.
pub const fn tap_id(tap: usize, what: u32) -> u32 {
    TAP_BASE + tap as u32 * TAP_STRIDE + what
}

const MAX_SECONDS: f64 = 13.0;
const SILENCE_DB: f64 = -60.0;
const NAMES: [&str; 3] = ["1 L", "2 L+R", "3 R"];

fn params() -> Vec<ParamInfo> {
    let mut p = vec![ParamInfo::continuous(DRY, "Dry", -60.0, 6.0, 0.0, "dB")];
    let divisions = [2.0, 3.0, 4.0]; // "3", "4", "5" sixteenths
    let pans = [-1.0, 0.0, 1.0];
    for tap in 0..3 {
        let n = |what: &str| format!("{} {}", NAMES[tap], what);
        p.extend([
            ParamInfo::toggle(tap_id(tap, ON), &n("On"), true),
            ParamInfo::toggle(tap_id(tap, FILTER_ON), &n("Filter On"), true),
            ParamInfo::skewed(
                tap_id(tap, FREQ),
                &n("Filter Freq"),
                50.0,
                18_000.0,
                1_000.0,
                "Hz",
                3.0,
            ),
            ParamInfo::continuous(tap_id(tap, WIDTH), &n("Filter Width"), 0.5, 9.0, 4.0, "oct"),
            ParamInfo::toggle(tap_id(tap, SYNC), &n("Sync"), true),
            ParamInfo::skewed(
                tap_id(tap, TIME),
                &n("Time"),
                1.0,
                5_000.0,
                250.0,
                "ms",
                2.0,
            ),
            ParamInfo::stepped(
                tap_id(tap, DIVISION),
                &n("Delay Time"),
                &NUMBERED_LABELS,
                divisions[tap] as usize,
            ),
            ParamInfo::continuous(tap_id(tap, OFFSET), &n("Offset"), -33.0, 33.0, 0.0, "%"),
            ParamInfo::continuous(tap_id(tap, FEEDBACK), &n("Feedback"), 0.0, 1.1, 0.3, ""),
            ParamInfo::continuous(tap_id(tap, PAN), &n("Pan"), -1.0, 1.0, pans[tap], ""),
            ParamInfo::continuous(
                tap_id(tap, VOLUME),
                &n("Volume"),
                SILENCE_DB,
                6.0,
                0.0,
                "dB",
            ),
        ]);
    }
    p
}

fn layout() -> Vec<Section> {
    let mut sections: Vec<Section> = (0..3)
        .map(|tap| Section {
            title: ["1 L", "2 L+R", "3 R"][tap],
            items: vec![
                Item::Row(vec![
                    toggle(tap_id(tap, ON)),
                    toggle(tap_id(tap, FILTER_ON)),
                    knob(tap_id(tap, FREQ)),
                    knob(tap_id(tap, WIDTH)),
                ]),
                Item::Row(vec![
                    toggle(tap_id(tap, SYNC)),
                    menu(tap_id(tap, DIVISION)),
                    knob(tap_id(tap, TIME)),
                    knob(tap_id(tap, OFFSET)),
                ]),
                Item::Row(vec![
                    knob(tap_id(tap, FEEDBACK)),
                    knob(tap_id(tap, PAN)),
                    knob(tap_id(tap, VOLUME)),
                ]),
            ],
        })
        .collect();
    sections.push(Section {
        title: "Output",
        items: vec![knob(DRY)],
    });
    sections
}

pub struct FilterDelay {
    v: [f64; PARAMS],
    sr: f32,
    lines: [InterpDelay; 3],
    heads: [DelayHead; 3],
    filters: [LinkedBand; 3],
    filter_dirty: bool,
}

impl Default for FilterDelay {
    fn default() -> Self {
        Self {
            v: values(&params()),
            sr: 48_000.0,
            lines: [
                InterpDelay::new(16),
                InterpDelay::new(16),
                InterpDelay::new(16),
            ],
            heads: [DelayHead::default(); 3],
            filters: [LinkedBand::default(); 3],
            filter_dirty: true,
        }
    }
}

impl FilterDelay {
    fn tap_value(&self, tap: usize, what: u32) -> f64 {
        self.v[tap_id(tap, what) as usize]
    }

    fn render(&mut self, audio: AudioBuffer, tempo: f64) {
        if self.filter_dirty {
            for tap in 0..3 {
                let freq = self.tap_value(tap, FREQ) as f32;
                let width = self.tap_value(tap, WIDTH) as f32;
                self.filters[tap].set(freq, width, self.sr);
            }
            self.filter_dirty = false;
        }
        let mut gains = [(0.0f32, 0.0f32); 3];
        let mut on = [false; 3];
        let mut filter_on = [false; 3];
        let mut feedback = [0.0f32; 3];
        for tap in 0..3 {
            on[tap] = self.tap_value(tap, ON) >= 0.5;
            filter_on[tap] = self.tap_value(tap, FILTER_ON) >= 0.5;
            feedback[tap] = self.tap_value(tap, FEEDBACK) as f32;
            let db = self.tap_value(tap, VOLUME);
            let volume = if db <= SILENCE_DB {
                0.0
            } else {
                db_to_gain(db as f32)
            };
            let pan = self.tap_value(tap, PAN) as f32;
            gains[tap] = (volume * (1.0 - pan).min(1.0), volume * (1.0 + pan).min(1.0));
            let seconds = if self.tap_value(tap, SYNC) >= 0.5 {
                let beats =
                    numbered_beats(self.tap_value(tap, DIVISION), self.tap_value(tap, OFFSET));
                beats * 60.0 / tempo.max(1.0)
            } else {
                self.tap_value(tap, TIME) * 0.001
            };
            self.heads[tap].set_target((seconds as f32 * self.sr).max(1.0));
        }
        let dry = if self.v[DRY as usize] <= SILENCE_DB {
            0.0
        } else {
            db_to_gain(self.v[DRY as usize] as f32)
        };
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let inputs = [*l, (*l + *r) * 0.5, *r];
            let (mut wet_l, mut wet_r) = (0.0, 0.0);
            for tap in 0..3 {
                if !on[tap] {
                    continue;
                }
                let line = &self.lines[tap];
                let y = self.heads[tap].read(|d| line.read(d - 1.0, Interpolation::Cubic));
                let mut x = inputs[tap] + y * feedback[tap];
                if filter_on[tap] {
                    x = self.filters[tap].process(x);
                }
                self.lines[tap].write(guard(x));
                wet_l += y * gains[tap].0;
                wet_r += y * gains[tap].1;
            }
            *l = *l * dry + wet_l;
            *r = *r * dry + wet_r;
        }
    }
}

impl Processor for FilterDelay {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let capacity = (sample_rate * MAX_SECONDS) as usize;
        self.lines = std::array::from_fn(|_| InterpDelay::new(capacity));
        self.heads.iter_mut().for_each(|h| h.prepare(self.sr));
        self.filter_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let tempo = ctx.transport.tempo;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio, tempo),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        self.filter_dirty = true;
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(InterpDelay::clear);
        self.filters.iter_mut().for_each(LinkedBand::reset);
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.filter_delay",
    name: "Filter Delay",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<FilterDelay>::default(),
};
