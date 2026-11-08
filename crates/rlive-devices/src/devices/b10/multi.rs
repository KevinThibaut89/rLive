//! The Sampler's extended engine (manual pp. 773-791): multisample zones, loops, modulation.
//!
//! [`crate::sampler::Sampler`] keeps its original playback for its default state: that path is
//! fixed by the project golden. The moment anything here is set away from its default, or a zone
//! map is loaded, new notes go here instead.
//!
//! The manual names only some of the destinations (Loop Length, Volume, Filter, Pan, Pitch), so
//! the 29-entry list and the units of every amount are ours, as are the 21 oscillator shapes. It
//! does give: 21 waves, 29 destinations, LFO 0.01-30 Hz, 64th notes to 8 bars, Coarse 0.125-48,
//! Fine 0-1000, Filter depth 0-24, Pitch Bend Range 0-24, Voices up to 32, and Spin up to 50 %.

use super::common::*;
use super::multi_params::*;
use crate::dsp::envelope::{Envelope, EnvelopeSettings, LoopMode};
use crate::dsp::filter::{Filter, FilterSettings};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings, LfoShape};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::dsp::voice::{
    GlideMode, StealMode, VoiceAllocator, VoiceEvent, VoiceMode, VoiceSettings,
};
use crate::sampler::p as legacy;
use rlive_core::{MidiMessage, Resource, SampleBuffer, ViewId};
use serde::{Deserialize, Serialize};
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

/// How many layers a zone map can hold, and so how many sample slots the Sampler declares.
pub const MAX_LAYERS: usize = 256;

/// The version of [`ZoneMap`] this build writes.
pub const ZONES_VERSION: u32 = 1;

/// View: the overview of the selected layer, then a bit per layer that's sounding (16 per word).
pub const VIEW_WAVE: ViewId = ViewId(0);
pub const VIEW_ACTIVE: ViewId = ViewId(WAVE_VALUES as u32);
pub const ACTIVE_WORDS: usize = 16;
pub const VIEW_LEN: usize = WAVE_VALUES + ACTIVE_WORDS;

const MAX_VOICES: usize = 64;
const CONTROL_FRAMES: usize = 32;
const ROUTES: usize = 24;

/// Sources of the modulation matrix.
const SRC_AUX: usize = 0;
const SRC_LFO1: usize = 1;
const SRC_LFO2: usize = 2;
const SRC_LFO3: usize = 3;
const SRC_KEY: usize = 4;
const SRC_VEL: usize = 5;
const SRC_REL: usize = 6;
const SRC_AT: usize = 7;
const SRC_MW: usize = 8;
const SRC_FC: usize = 9;
const SRC_PB: usize = 10;
const SOURCES: usize = 11;

const DEST_COUNT: usize = 29;

/// One end of a zone: `lo..=hi`, fading in over the bottom `fade_lo` and out over the top
/// `fade_hi` steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub lo: u8,
    pub hi: u8,
    #[serde(default)]
    pub fade_lo: u8,
    #[serde(default)]
    pub fade_hi: u8,
}

impl Zone {
    pub const FULL_KEY: Zone = Zone {
        lo: 0,
        hi: 127,
        fade_lo: 0,
        fade_hi: 0,
    };
    pub const FULL_VEL: Zone = Zone {
        lo: 1,
        hi: 127,
        fade_lo: 0,
        fade_hi: 0,
    };
    pub const FULL_SEL: Zone = Self::FULL_KEY;

    /// The gain of this zone at `value`, or zero outside it.
    pub fn gain(&self, value: f32, power: bool) -> f32 {
        let (lo, hi) = (self.lo as f32, self.hi as f32);
        if value < lo || value > hi {
            return 0.0;
        }
        let mut t = 1.0f32;
        if self.fade_lo > 0 {
            t = t.min((value - lo) / self.fade_lo as f32);
        }
        if self.fade_hi > 0 {
            t = t.min((hi - value) / self.fade_hi as f32);
        }
        let t = t.clamp(0.0, 1.0);
        if power {
            (t * std::f32::consts::FRAC_PI_2).sin()
        } else {
            t
        }
    }
}

/// One sample of a multisample: a row of the layer list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    /// The device's sample slot this layer plays.
    pub slot: u32,
    #[serde(default = "root_c3")]
    pub root: f32,
    #[serde(default)]
    pub detune: f32,
    #[serde(default)]
    pub volume_db: f32,
    #[serde(default)]
    pub pan: f32,
    #[serde(default = "full_key")]
    pub key: Zone,
    #[serde(default = "full_vel")]
    pub vel: Zone,
    #[serde(default = "full_key")]
    pub sel: Zone,
    /// Frames; `None` for the whole sample.
    #[serde(default)]
    pub start: Option<f64>,
    #[serde(default)]
    pub end: Option<f64>,
    /// 0 off, 1 loop, 2 back and forth.
    #[serde(default)]
    pub sustain: u8,
    #[serde(default)]
    pub loop_start: f64,
    #[serde(default)]
    pub loop_end: f64,
    /// 0 Off, 1 Release, 2 Release Loop, 3 Back-and-Forth Release Loop.
    #[serde(default)]
    pub release: u8,
    #[serde(default)]
    pub release_loop: f64,
    /// Frames.
    #[serde(default)]
    pub crossfade: f64,
    #[serde(default)]
    pub loop_detune: f32,
    #[serde(default)]
    pub link: bool,
}

fn root_c3() -> f32 {
    60.0
}

fn full_key() -> Zone {
    Zone::FULL_KEY
}

fn full_vel() -> Zone {
    Zone::FULL_VEL
}

impl Layer {
    pub fn new(name: &str, slot: u32) -> Self {
        Self {
            name: name.to_string(),
            slot,
            root: 60.0,
            detune: 0.0,
            volume_db: 0.0,
            pan: 0.0,
            key: Zone::FULL_KEY,
            vel: Zone::FULL_VEL,
            sel: Zone::FULL_SEL,
            start: None,
            end: None,
            sustain: 0,
            loop_start: 0.0,
            loop_end: 0.0,
            release: 0,
            release_loop: 0.0,
            crossfade: 0.0,
            loop_detune: 0.0,
            link: false,
        }
    }
}

/// How Round Robin picks among samples sharing a key zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RrMode {
    #[default]
    Forward,
    Backward,
    /// Never the same sample twice in a row.
    Other,
    Random,
}

/// Everything about a Sampler that isn't a parameter. Saved as RON in the device's `state`,
/// beside the file references in its sample slots (a missing file leaves its layer silent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZoneMap {
    pub version: u32,
    #[serde(default)]
    pub layers: Vec<Layer>,
    #[serde(default)]
    pub round_robin: bool,
    #[serde(default)]
    pub rr_mode: RrMode,
    /// Beats between restarts of the cycle. 0 for never.
    #[serde(default)]
    pub rr_reset_beats: f64,
    /// Constant-power zone fades, instead of linear.
    #[serde(default)]
    pub power_fades: bool,
    #[serde(default)]
    pub auto_select: bool,
}

impl Default for ZoneMap {
    fn default() -> Self {
        Self {
            version: ZONES_VERSION,
            layers: Vec::new(),
            round_robin: false,
            rr_mode: RrMode::Forward,
            rr_reset_beats: 0.0,
            power_fades: false,
            auto_select: false,
        }
    }
}

impl ZoneMap {
    /// A newer version than this build knows is treated as no map rather than guessed at.
    pub fn parse(text: Option<&str>) -> Self {
        text.and_then(|t| ron::from_str::<ZoneMap>(t).ok())
            .filter(|m| m.version <= ZONES_VERSION)
            .unwrap_or_default()
    }

    pub fn serialize(&self) -> String {
        ron::to_string(&ZoneMap {
            version: ZONES_VERSION,
            ..self.clone()
        })
        .unwrap_or_default()
    }

    /// Distributes the key zones evenly over the keyboard, C-2 to G8.
    pub fn distribute_equally(&mut self) {
        let n = self.layers.len() as f32;
        for (i, layer) in self.layers.iter_mut().enumerate() {
            let lo = (i as f32 * 128.0 / n) as u8;
            let hi = (((i + 1) as f32 * 128.0 / n) as i32 - 1).clamp(lo as i32, 127) as u8;
            layer.key = Zone {
                lo,
                hi,
                fade_lo: 0,
                fade_hi: 0,
            };
        }
    }

    /// Spreads the key zones around the root keys without overlaps (p. 778). Layers on the same
    /// root key share that range evenly.
    pub fn distribute_around_root(&mut self) {
        let mut order: Vec<usize> = (0..self.layers.len()).collect();
        order.sort_by(|a, b| self.layers[*a].root.total_cmp(&self.layers[*b].root));
        let mut i = 0;
        let mut low = 0i32;
        while i < order.len() {
            let root = self.layers[order[i]].root;
            let mut j = i + 1;
            while j < order.len() && self.layers[order[j]].root == root {
                j += 1;
            }
            let next = order.get(j).map(|o| self.layers[*o].root);
            let high = match next {
                Some(n) => ((root + n) * 0.5) as i32,
                None => 127,
            };
            for (k, &layer) in order[i..j].iter().enumerate() {
                let span = (high - low + 1).max(1) as usize / (j - i);
                let lo = low + (span * k) as i32;
                let hi = if k + 1 == j - i {
                    high
                } else {
                    lo + span as i32 - 1
                };
                self.layers[layer].key = Zone {
                    lo: lo.clamp(0, 127) as u8,
                    hi: hi.clamp(0, 127) as u8,
                    fade_lo: 0,
                    fade_hi: 0,
                };
            }
            low = high + 1;
            i = j;
        }
    }
}

/// A parsed [`ZoneMap`], built off the audio thread and swapped in whole.
#[derive(Clone, Debug, Default)]
pub struct ParsedZones {
    pub map: ZoneMap,
}

/// One note-on's choice of a layer.
#[derive(Clone, Copy)]
struct Triggered {
    layer: u16,
    gain: f32,
}

#[derive(Clone, Copy, Default)]
struct OscState {
    phase: f32,
    held: f32,
    last_phase: f32,
}

struct MultiVoice {
    active: bool,
    released: bool,
    key: f32,
    velocity: f32,
    rel_vel: f32,
    root: f32,
    layer_detune: f32,
    layer_gain: f32,
    layer_pan: f32,
    random_pan: f32,
    slot: usize,
    interp: Interp,
    player: Player,
    /// Loop points before modulation.
    loop_points: (f64, f64),
    loop_kind: LoopKind,
    crossfade: f64,
    power: bool,
    loop_detune: f64,
    side: f32,
    amp: Envelope,
    pitch_env: Envelope,
    filter_env: Envelope,
    aux_env: Envelope,
    osc_env: Envelope,
    pitch_env_level: f32,
    filter_env_level: f32,
    aux_level: f32,
    osc_env_level: f32,
    lfo: [Lfo; 3],
    lfo_values: [[f32; 2]; 3],
    lfo_ramp: [f32; 3],
    filter: [Filter; 2],
    shape_filter_first: bool,
    bus_l: ModBus,
    bus_r: ModBus,
    osc: OscState,
    rng: Rng,
    ctl: usize,
    step: f64,
    gain_l: f32,
    gain_r: f32,
    osc_step: f32,
    osc_depth_fm: f32,
    osc_depth_am: f32,
    shaper_amount: f32,
}

/// The heart of the extended Sampler.
pub struct MultiEngine {
    /// `slots[0]` stays empty: the Sampler keeps its first sample where it always has.
    slots: Vec<Option<Arc<SampleBuffer>>>,
    analyses: Vec<Option<Arc<Analysis>>>,
    zones: Arc<ParsedZones>,
    alloc: VoiceAllocator,
    voices: Box<[MultiVoice]>,
    matrix: ModMatrix,
    rng: Rng,
    sr: f64,
    clock: BeatClock,
    rr_position: [u16; MAX_LAYERS],
    rr_first: [i16; MAX_LAYERS],
    rr_last: [i16; MAX_LAYERS],
    rr_interval: i64,
    routes_dirty: bool,
    wheel: f32,
    foot: f32,
    pressure: f32,
    bend: f32,
    selected_view: usize,
}

const NO_FIRST: i16 = -1;

fn map_loop(i: f64) -> LoopMode {
    match i.round() as i32 {
        1 => LoopMode::Loop,
        2 => LoopMode::Beat,
        3 => LoopMode::Sync,
        4 => LoopMode::Trigger,
        _ => LoopMode::Off,
    }
}

fn map_lfo(i: f64) -> LfoShape {
    match i.round() as i32 {
        1 => LfoShape::Square,
        2 => LfoShape::Triangle,
        3 => LfoShape::SawDown,
        4 => LfoShape::SawUp,
        5 => LfoShape::SampleHold,
        _ => LfoShape::Sine,
    }
}

/// An oscillator shape, in `-1..=1`.
#[inline]
fn osc_wave(kind: usize, phase: f32, state: &mut OscState, rng: &mut Rng) -> f32 {
    let p = phase.rem_euclid(1.0);
    let sine = (p * TAU).sin();
    match kind {
        1 => sine.max(0.0) * 2.0 - 1.0,
        2 => sine.abs() * 2.0 - 1.0,
        3 => 1.0 - 4.0 * (p - 0.5).abs(),
        4 => 2.0 * p - 1.0,
        5 => 1.0 - 2.0 * p,
        6 => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        7 => {
            if p < 0.25 {
                1.0
            } else {
                -1.0
            }
        }
        8 => {
            if p < 0.125 {
                1.0
            } else {
                -1.0
            }
        }
        9 => 0.66 * sine + 0.34 * (p * 2.0 * TAU).sin(),
        10 => 0.6 * sine + 0.4 * (p * 1.5 * TAU).sin(),
        11 => 0.7 * sine + 0.3 * (p * 3.0 * TAU).sin(),
        12 => (2.0 * p - 1.0).powi(3),
        13 => {
            let t = 1.0 - 4.0 * (p - 0.5).abs();
            t * t.abs()
        }
        14 => ((p * 4.0).floor() / 3.0) * 2.0 - 1.0,
        15 => ((p * 8.0).floor() / 7.0) * 2.0 - 1.0,
        16 => (p * 6.0).exp_m1() / 6f32.exp_m1() * 2.0 - 1.0,
        17 => {
            if p < state.last_phase {
                state.held = rng.bipolar();
            }
            state.last_phase = p;
            state.held
        }
        18 => rng.bipolar(),
        19 => (p * TAU + 2.0 * (p * 3.0 * TAU).sin()).sin(),
        20 => {
            if p < 0.1 {
                (p * 10.0 * PI).sin()
            } else {
                0.0
            }
        }
        _ => sine,
    }
}

#[inline]
fn shape(kind: i32, x: f32) -> f32 {
    match kind {
        1 => x.clamp(-1.0, 1.0),
        2 => (x.clamp(-1.0, 1.0) * std::f32::consts::FRAC_PI_2).sin(),
        3 => (x.clamp(-1.0, 1.0) * 8.0).round() / 8.0,
        _ => x.tanh(),
    }
}

/// Reads an envelope block out of `values`, with times scaled by `time`. `from` is the block's
/// first id: the first of the Initial parameter.
fn envelope_of(values: &[f64], from: u32, time: f32) -> EnvelopeSettings {
    let get = |offset: u32| values[(from + offset) as usize];
    EnvelopeSettings {
        initial: get(0) as f32,
        peak: get(1) as f32,
        sustain: get(2) as f32,
        end: get(3) as f32,
        attack: (get(4) * 0.001) as f32 * time,
        decay: (get(5) * 0.001) as f32 * time,
        release: (get(6) * 0.001) as f32 * time,
        attack_slope: get(7) as f32,
        decay_slope: get(8) as f32,
        release_slope: get(9) as f32,
        loop_mode: map_loop(get(10)),
        loop_time: (get(11) * 0.001) as f32 * time,
        repeat_beats: SyncRate::from_index(get(12)).beats(),
        ..Default::default()
    }
}

fn time_vel(values: &[f64], from: u32, velocity: f32) -> f32 {
    let amount = (values[(from + 13) as usize] * 0.01) as f32;
    2f32.powf(amount * (velocity - 0.5) * 2.0)
}

impl MultiVoice {
    fn new(matrix: &ModMatrix, seed: u32) -> Self {
        Self {
            active: false,
            released: false,
            key: 60.0,
            velocity: 0.0,
            rel_vel: 0.0,
            root: 60.0,
            layer_detune: 0.0,
            layer_gain: 1.0,
            layer_pan: 0.0,
            random_pan: 0.0,
            slot: 0,
            interp: Interp::Normal,
            player: Player::default(),
            loop_points: (0.0, 0.0),
            loop_kind: LoopKind::Off,
            crossfade: 0.0,
            power: true,
            loop_detune: 1.0,
            side: 0.0,
            amp: Envelope::new(),
            pitch_env: Envelope::new(),
            filter_env: Envelope::new(),
            aux_env: Envelope::new(),
            osc_env: Envelope::new(),
            pitch_env_level: 0.0,
            filter_env_level: 0.0,
            aux_level: 0.0,
            osc_env_level: 0.0,
            lfo: [Lfo::new(seed + 1), Lfo::new(seed + 2), Lfo::new(seed + 3)],
            lfo_values: [[0.0; 2]; 3],
            lfo_ramp: [0.0; 3],
            filter: [Filter::new(); 2],
            shape_filter_first: false,
            bus_l: matrix.bus(),
            bus_r: matrix.bus(),
            osc: OscState::default(),
            rng: Rng::new(seed + 7),
            ctl: 0,
            step: 1.0,
            gain_l: 1.0,
            gain_r: 1.0,
            osc_step: 0.0,
            osc_depth_fm: 0.0,
            osc_depth_am: 0.0,
            shaper_amount: 0.0,
        }
    }
}

fn bipolar_key(key: f32) -> f32 {
    ((key - 60.0) / 60.0).clamp(-1.0, 1.0)
}

fn sample_of<'a>(
    slots: &'a [Option<Arc<SampleBuffer>>],
    first: Option<&'a SampleBuffer>,
    slot: usize,
) -> Option<&'a SampleBuffer> {
    if slot == 0 {
        first
    } else {
        slots.get(slot)?.as_deref()
    }
}

impl MultiEngine {
    pub fn new(state: Option<&str>, samples: &[Option<Arc<SampleBuffer>>]) -> Self {
        prepare_interpolation();
        let matrix = ModMatrix::new(SOURCES, DEST_COUNT, ROUTES);
        let voices: Box<[MultiVoice]> = (0..MAX_VOICES)
            .map(|i| MultiVoice::new(&matrix, (i * 17 + 3) as u32))
            .collect();
        let mut slots: Vec<Option<Arc<SampleBuffer>>> = vec![None; MAX_LAYERS];
        let mut analyses: Vec<Option<Arc<Analysis>>> = vec![None; MAX_LAYERS];
        for (i, sample) in samples.iter().enumerate().take(MAX_LAYERS) {
            if let Some(sample) = sample {
                if i > 0 {
                    slots[i] = Some(sample.clone());
                }
                analyses[i] = Some(Arc::new(Analysis::new(sample, Some(&[]))));
            }
        }
        Self {
            slots,
            analyses,
            zones: Arc::new(ParsedZones {
                map: ZoneMap::parse(state),
            }),
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices,
            matrix,
            rng: Rng::new(0x51a4),
            sr: 48_000.0,
            clock: BeatClock::default(),
            rr_position: [0; MAX_LAYERS],
            rr_first: [NO_FIRST; MAX_LAYERS],
            rr_last: [NO_FIRST; MAX_LAYERS],
            rr_interval: i64::MIN,
            routes_dirty: true,
            wheel: 0.0,
            foot: 0.0,
            pressure: 0.0,
            bend: 0.0,
            selected_view: 0,
        }
    }

    pub fn has_zones(&self) -> bool {
        !self.zones.map.layers.is_empty()
    }

    pub fn zones(&self) -> &ZoneMap {
        &self.zones.map
    }

    pub fn activate(&mut self, sample_rate: f64) {
        self.sr = sample_rate;
        self.alloc.prepare(sample_rate);
        for v in self.voices.iter_mut() {
            for e in [
                &mut v.amp,
                &mut v.pitch_env,
                &mut v.filter_env,
                &mut v.aux_env,
                &mut v.osc_env,
            ] {
                e.prepare(sample_rate);
            }
            for l in &mut v.lfo {
                l.prepare(sample_rate);
            }
            for f in &mut v.filter {
                f.prepare(sample_rate);
            }
        }
    }

    pub fn reset(&mut self) {
        self.alloc.reset();
        for v in self.voices.iter_mut() {
            v.active = false;
        }
    }

    /// Takes a resource. Slot 0 is the first sample, which the caller owns: don't come here for
    /// it. Voices keep reading by slot, so nothing a voice holds dangles.
    pub fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot == STATE_SLOT {
            return match resource.downcast::<ParsedZones>() {
                Ok(zones) => {
                    self.routes_dirty = true;
                    Some(std::mem::replace(&mut self.zones, zones) as Resource)
                }
                Err(refused) => Some(refused),
            };
        }
        if (ANALYSIS_SLOTS..ANALYSIS_SLOTS + MAX_LAYERS as u32).contains(&slot) {
            let index = (slot - ANALYSIS_SLOTS) as usize;
            return match resource.downcast::<Analysis>() {
                Ok(a) => self.analyses[index].replace(a).map(|o| o as Resource),
                Err(refused) => Some(refused),
            };
        }
        let index = slot as usize;
        if index == 0 || index >= MAX_LAYERS {
            return Some(resource);
        }
        match resource.downcast::<SampleBuffer>() {
            Ok(s) => self.slots[index].replace(s).map(|o| o as Resource),
            Err(refused) => Some(refused),
        }
    }

    fn voice_settings(&self, values: &[f64], unison: usize) -> VoiceSettings {
        let glide = values[GLIDE as usize].round() as i32;
        VoiceSettings {
            mode: if glide == 1 {
                VoiceMode::Mono
            } else {
                VoiceMode::Poly
            },
            polyphony: values[VOICES as usize].round().clamp(1.0, 32.0) as usize,
            unison,
            steal: StealMode::Oldest,
            retrigger: values[RETRIGGER as usize] >= 0.5,
            glide: if glide > 0 {
                GlideMode::Always
            } else {
                GlideMode::Off
            },
            glide_time: (values[GLIDE_TIME as usize] * 0.001) as f32,
            bend_range: values[PB_RANGE as usize] as f32,
            ..Default::default()
        }
    }

    /// Rebuilds the modulation routes from their parameters.
    fn rebuild_routes(&mut self, values: &[f64]) {
        self.routes_dirty = false;
        let get = |id: u32| values[id as usize];
        let route = |source: usize, dest: f64, amount: f64| -> Option<Route> {
            let dest = dest.round() as usize;
            (dest > 0 && amount != 0.0).then_some(Route {
                source,
                dest,
                amount: amount as f32,
                kind: RouteKind::Add,
                via: None,
            })
        };
        let fixed = |source: usize, dest: usize, amount: f64| route(source, dest as f64, amount);
        let table: [Option<Route>; ROUTES] = [
            route(SRC_AUX, get(AE_DEST_A), get(AE_AMT_A) * 0.01),
            route(SRC_AUX, get(AE_DEST_B), get(AE_AMT_B) * 0.01),
            fixed(SRC_LFO1, 1, get(L1_TO_VOL) * 0.01),
            fixed(SRC_LFO1, 2, get(L1_TO_PAN) * 0.01),
            fixed(SRC_LFO1, 4, get(L1_TO_FILTER) / 48.0),
            fixed(SRC_LFO1, 3, get(L1_TO_PITCH) * 0.01),
            route(SRC_LFO2, get(L2_DEST_A), get(L2_AMT_A) * 0.01),
            route(SRC_LFO2, get(L2_DEST_B), get(L2_AMT_B) * 0.01),
            route(SRC_LFO3, get(L3_DEST_A), get(L3_AMT_A) * 0.01),
            route(SRC_LFO3, get(L3_DEST_B), get(L3_AMT_B) * 0.01),
            route(SRC_KEY, get(M_KEY_DEST_A), get(M_KEY_AMT_A) * 0.01),
            route(SRC_KEY, get(M_KEY_DEST_B), get(M_KEY_AMT_B) * 0.01),
            route(SRC_VEL, get(M_VEL_DEST_A), get(M_VEL_AMT_A) * 0.01),
            route(SRC_VEL, get(M_VEL_DEST_B), get(M_VEL_AMT_B) * 0.01),
            route(SRC_REL, get(M_REL_DEST_A), get(M_REL_AMT_A) * 0.01),
            route(SRC_REL, get(M_REL_DEST_B), get(M_REL_AMT_B) * 0.01),
            route(SRC_AT, get(M_AT_DEST_A), get(M_AT_AMT_A) * 0.01),
            route(SRC_AT, get(M_AT_DEST_B), get(M_AT_AMT_B) * 0.01),
            route(SRC_MW, get(M_MW_DEST_A), get(M_MW_AMT_A) * 0.01),
            route(SRC_MW, get(M_MW_DEST_B), get(M_MW_AMT_B) * 0.01),
            route(SRC_FC, get(M_FC_DEST_A), get(M_FC_AMT_A) * 0.01),
            route(SRC_FC, get(M_FC_DEST_B), get(M_FC_AMT_B) * 0.01),
            route(SRC_PB, get(M_PB_DEST_A), get(M_PB_AMT_A) * 0.01),
            route(SRC_PB, get(M_PB_DEST_B), get(M_PB_AMT_B) * 0.01),
        ];
        for (slot, r) in table.into_iter().enumerate() {
            self.matrix.set_route(slot, r);
        }
    }

    pub fn param_changed(&mut self) {
        self.routes_dirty = true;
    }

    pub fn set_clock(&mut self, clock: BeatClock) {
        self.clock = clock;
    }

    /// Whether any voice is still playing, so the engine has to keep running.
    pub fn has_active_voices(&self) -> bool {
        self.voices.iter().any(|v| v.active)
    }

    /// Picks the layers a note triggers: every layer whose zones hold it, except that with
    /// Round Robin on only one layer of each key zone sounds. Fills `out`, returns the count.
    fn pick_layers(
        &mut self,
        key: f32,
        velocity: f32,
        selector: f32,
        out: &mut [Triggered; MAX_VOICES],
    ) -> usize {
        let map = &self.zones.map;
        let power = map.power_fades;
        let vel_value = (velocity * 127.0).round().clamp(1.0, 127.0);
        if map.round_robin && map.rr_reset_beats > 0.0 {
            let interval = (self.clock.beat / map.rr_reset_beats.max(1e-6)).floor() as i64;
            if interval != self.rr_interval {
                self.rr_interval = interval;
                self.rr_position = [0; MAX_LAYERS];
            }
        }
        let mut count = 0usize;
        let mut matching = [0u16; MAX_LAYERS];
        let mut gains = [0f32; MAX_LAYERS];
        let mut n = 0usize;
        for (i, layer) in map.layers.iter().enumerate() {
            let g = layer.key.gain(key, power)
                * layer.vel.gain(vel_value, power)
                * layer.sel.gain(selector, power);
            if g > 0.0 && n < MAX_LAYERS {
                matching[n] = i as u16;
                gains[n] = g;
                n += 1;
            }
        }
        if !map.round_robin {
            for i in 0..n.min(MAX_VOICES) {
                out[count] = Triggered {
                    layer: matching[i],
                    gain: gains[i],
                };
                count += 1;
            }
            return count;
        }
        // Group by key zone, using the first index of each group as its identity.
        let mut seen = [false; MAX_LAYERS];
        for i in 0..n {
            if seen[matching[i] as usize] {
                continue;
            }
            let zone = map.layers[matching[i] as usize].key;
            let mut group = [0u16; MAX_LAYERS];
            let mut group_gain = [0f32; MAX_LAYERS];
            let mut m = 0usize;
            for j in i..n {
                let l = matching[j] as usize;
                let same = map.layers[l].key.lo == zone.lo && map.layers[l].key.hi == zone.hi;
                if same && !seen[l] {
                    seen[l] = true;
                    group[m] = matching[j];
                    group_gain[m] = gains[j];
                    m += 1;
                }
            }
            let id = group[0] as usize;
            let position = self.rr_position[id] as usize;
            let first = self.rr_first[id];
            let pick = match map.rr_mode {
                RrMode::Forward => position % m,
                RrMode::Backward => m - 1 - position % m,
                RrMode::Random | RrMode::Other if position == 0 && first >= 0 => first as usize,
                RrMode::Random => (self.rng.next_u32() as usize) % m,
                RrMode::Other => {
                    let last = self.rr_last[id];
                    if m < 2 {
                        0
                    } else if last < 0 {
                        (self.rng.next_u32() as usize) % m
                    } else {
                        (last as usize + 1 + (self.rng.next_u32() as usize) % (m - 1)) % m
                    }
                }
            };
            if position == 0 && first < 0 {
                self.rr_first[id] = pick as i16;
            }
            self.rr_last[id] = pick as i16;
            self.rr_position[id] = self.rr_position[id].wrapping_add(1);
            if count < MAX_VOICES {
                out[count] = Triggered {
                    layer: group[pick],
                    gain: group_gain[pick],
                };
                count += 1;
            }
        }
        count
    }

    /// Handles one MIDI message.
    pub fn midi(
        &mut self,
        message: MidiMessage,
        values: &[f64],
        first: Option<&SampleBuffer>,
        accept_notes: bool,
    ) {
        match message {
            MidiMessage::ControlChange { cc: 1, value, .. } => self.wheel = value as f32 / 127.0,
            MidiMessage::ControlChange { cc: 4, value, .. } => self.foot = value as f32 / 127.0,
            MidiMessage::ChannelPressure { value, .. } => self.pressure = value,
            MidiMessage::PitchBend { value, .. } => self.bend = value,
            _ => {}
        }
        if self.routes_dirty {
            self.rebuild_routes(values);
        }
        let mut triggered = [Triggered {
            layer: 0,
            gain: 1.0,
        }; MAX_VOICES];
        let mut layers = 0usize;
        let implicit = self.zones.map.layers.is_empty();
        let mut rel_vel = 0.0;
        if let MidiMessage::NoteOn { key, velocity, .. } = message {
            if velocity > 0.0 {
                if !accept_notes {
                    return;
                }
                let selector = values[SELECTOR as usize] as f32;
                let zone_key = (key as f32 + values[ZONE_SHIFT as usize] as f32).clamp(0.0, 127.0);
                layers = if implicit {
                    1
                } else {
                    self.pick_layers(zone_key, velocity, selector, &mut triggered)
                };
                if layers == 0 {
                    return;
                }
                let spread = values[SPREAD as usize] > 0.0;
                let per_note = if spread { 2 } else { 1 };
                let settings = self.voice_settings(values, layers * per_note);
                self.alloc.set(&settings);
            }
        }
        if let MidiMessage::NoteOff { velocity, .. } = message {
            rel_vel = velocity;
        }
        if layers == 0 && !matches!(message, MidiMessage::NoteOn { .. }) {
            let unison = self.alloc.settings().unison;
            let settings = self.voice_settings(values, unison);
            self.alloc.set(&settings);
        }
        let per_note = if values[SPREAD as usize] > 0.0 { 2 } else { 1 };
        let mut events = [None; MAX_VOICES];
        let mut count = 0;
        self.alloc.handle(message, |event| {
            if count < events.len() {
                events[count] = Some(event);
                count += 1;
            }
        });
        for event in events.into_iter().flatten() {
            if let VoiceEvent::Release { voice } = event {
                if !self.voices[voice].active {
                    self.alloc.finished(voice);
                    continue;
                }
            }
            match event {
                VoiceEvent::Start { voice, note, .. } => {
                    if layers == 0 {
                        continue; // A pedal or retrigger restarted nothing here.
                    }
                    let index = note.unison_index as usize;
                    let which = (index / per_note) % layers;
                    let side = if per_note == 2 {
                        if index.is_multiple_of(2) {
                            -1.0
                        } else {
                            1.0
                        }
                    } else {
                        0.0
                    };
                    self.start_voice(
                        voice,
                        note.key as f32,
                        note.velocity,
                        triggered[which],
                        implicit,
                        side,
                        values,
                        first,
                    );
                }
                VoiceEvent::Legato { voice, note } => self.voices[voice].key = note.key as f32,
                VoiceEvent::Release { voice } => {
                    let v = &mut self.voices[voice];
                    if v.active {
                        v.released = true;
                        v.rel_vel = rel_vel;
                        for e in [
                            &mut v.amp,
                            &mut v.pitch_env,
                            &mut v.filter_env,
                            &mut v.aux_env,
                            &mut v.osc_env,
                        ] {
                            e.gate_off();
                        }
                    }
                }
                VoiceEvent::Kill { voice } => self.voices[voice].active = false,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_voice(
        &mut self,
        voice: usize,
        key: f32,
        velocity: f32,
        pick: Triggered,
        implicit: bool,
        side: f32,
        values: &[f64],
        first: Option<&SampleBuffer>,
    ) {
        let (slot, root, detune, layer_gain, layer_pan) = if implicit {
            (0, values[legacy::ROOT as usize] as f32, 0.0, 1.0, 0.0)
        } else {
            let l = &self.zones.map.layers[pick.layer as usize];
            (
                l.slot as usize,
                l.root,
                l.detune,
                db_to_gain(l.volume_db) * pick.gain,
                l.pan,
            )
        };
        let Some(sample) = sample_of(&self.slots, first, slot) else {
            return;
        };
        let n = sample.len() as f64;
        let layer = (!implicit).then(|| &self.zones.map.layers[pick.layer as usize]);
        let snap = values[SNAP as usize] >= 0.5;
        let snap_frame = |frame: f64| {
            if snap {
                snap_to_zero(&sample.left, frame)
            } else {
                frame
            }
        };
        let mut start = match layer.and_then(|l| l.start) {
            Some(s) => s,
            None => values[legacy::START as usize].clamp(0.0, 1.0) * n,
        };
        let mut end = layer.and_then(|l| l.end).unwrap_or(n).min(n);
        if let Some(l) = layer.filter(|l| l.link && l.sustain != 0) {
            start = l.loop_start;
        }
        let mut sustain = LoopKind::Off;
        let mut crossfade = 0.0;
        let (mut loop_start, mut loop_end) = (0.0, n);
        let mut loop_detune = 1.0;
        let mut release = ReleaseMode::Off;
        let mut release_at = 0.0;
        if let Some(l) = layer {
            sustain = match l.sustain {
                1 => LoopKind::Forward,
                2 => LoopKind::BackAndForth,
                _ => LoopKind::Off,
            };
            loop_start = l.loop_start;
            loop_end = l.loop_end.max(l.loop_start);
            crossfade = l.crossfade;
            loop_detune = 2f64.powf(l.loop_detune as f64 / 1200.0);
            release = match l.release {
                1 => ReleaseMode::Linear,
                2 => ReleaseMode::Loop,
                3 => ReleaseMode::BackAndForth,
                _ => ReleaseMode::Off,
            };
            release_at = l.release_loop;
        }
        start = snap_frame(start);
        end = snap_frame(end);
        let reverse = values[REVERSE as usize] >= 0.5;
        let interp = Interp::from_index(values[INTERPOLATION as usize]);
        let power = values[CONST_POWER as usize] >= 0.5;

        let rng_pan = self.rng.bipolar() * (values[PAN_RND as usize] * 0.01) as f32;
        let sources = self.note_sources(key, velocity, 0.0);
        let v = &mut self.voices[voice];
        v.active = true;
        v.released = false;
        v.key = key;
        v.velocity = velocity;
        v.rel_vel = 0.0;
        v.root = root;
        v.layer_detune = detune;
        v.layer_gain = layer_gain;
        v.layer_pan = layer_pan;
        v.random_pan = rng_pan;
        v.slot = slot;
        v.interp = interp;
        v.power = power;
        v.side = side;
        v.ctl = 0;
        self.matrix.evaluate(&sources, &mut v.bus_l);
        self.matrix.evaluate(&sources, &mut v.bus_r);
        // Start, end and Time are sampled once, when the note starts.
        let modded_start = (start + v.bus_l.offset(11) as f64 * (end - start)).clamp(0.0, n);
        let modded_end = (end + v.bus_l.offset(12) as f64 * (end - start)).clamp(0.0, n);
        let (s0, e0) = if modded_end > modded_start + 1.0 {
            (modded_start, modded_end)
        } else {
            (modded_start, (modded_start + 1.0).min(n))
        };
        v.player = Player::new(s0, e0, reverse);
        v.loop_points = (snap_frame(loop_start), snap_frame(loop_end));
        v.loop_kind = sustain;
        v.crossfade = crossfade;
        v.loop_detune = loop_detune;
        v.player
            .set_sustain(v.loop_points, sustain, v.crossfade, power, loop_detune);
        v.player.set_release(release, release_at);
        for f in &mut v.filter {
            f.reset();
        }
        v.shape_filter_first = values[SHAPER_ORDER as usize].round() as i32 == 1;
        v.lfo_values = [[0.0; 2]; 3];
        v.lfo_ramp = [1.0; 3];
        v.osc = OscState::default();

        let tk = (values[TIME_KEY as usize] * 0.01) as f32;
        let mut time = 2f32.powf((values[TIME as usize] as f32 * 0.01 + 0.0) * 2.0)
            * 2f32.powf(-tk * (key - 60.0) / 12.0);
        time *= 2f32.powf(v.bus_l.offset(28) * 4.0);
        let vol_time = time;
        let attack_mod = 2f32.powf(v.bus_l.offset(22) * 4.0);
        let decay_mod = 2f32.powf(v.bus_l.offset(23) * 4.0);
        let release_mod = 2f32.powf(v.bus_l.offset(24) * 4.0);
        let vel_time =
            2f32.powf((values[V_TIME_VEL as usize] * 0.01) as f32 * (velocity - 0.5) * 2.0);
        let amp = EnvelopeSettings {
            initial: values[V_INITIAL as usize] as f32,
            peak: values[V_PEAK as usize] as f32,
            sustain: values[V_SUSTAIN as usize] as f32,
            end: 0.0,
            attack: (values[legacy::ATTACK as usize] * 0.001) as f32
                * vol_time
                * attack_mod
                * vel_time,
            decay: (values[V_DECAY as usize] * 0.001) as f32 * vol_time * decay_mod * vel_time,
            release: (values[legacy::RELEASE as usize] * 0.001) as f32
                * vol_time
                * release_mod
                * vel_time,
            attack_slope: values[V_A_SLOPE as usize] as f32,
            decay_slope: values[V_D_SLOPE as usize] as f32,
            release_slope: values[V_R_SLOPE as usize] as f32,
            loop_mode: if values[legacy::ONE_SHOT as usize] >= 0.5 {
                LoopMode::Trigger
            } else {
                map_loop(values[V_LOOP as usize])
            },
            loop_time: (values[V_LOOP_TIME as usize] * 0.001) as f32 * vol_time,
            repeat_beats: SyncRate::from_index(values[V_REPEAT as usize]).beats(),
            ..Default::default()
        };
        v.amp.set(&amp);
        for (env, from) in [
            (&mut v.pitch_env, PE_BASE),
            (&mut v.filter_env, FE_BASE),
            (&mut v.aux_env, AE_BASE),
            (&mut v.osc_env, OE_BASE),
        ] {
            env.set(&envelope_of(
                values,
                from,
                vol_time * time_vel(values, from, velocity),
            ));
            env.reset();
        }
        v.amp.reset();
        for e in [
            &mut v.amp,
            &mut v.pitch_env,
            &mut v.filter_env,
            &mut v.aux_env,
            &mut v.osc_env,
        ] {
            e.gate_on();
        }
        for (i, base) in [L1_ON, L2_ON, L3_ON].iter().enumerate() {
            let get = |offset: u32| values[(base + offset) as usize];
            if get(6) >= 0.5 {
                v.lfo[i].retrigger();
                v.lfo_ramp[i] = 0.0;
            }
        }
    }

    fn note_sources(&self, key: f32, velocity: f32, rel: f32) -> [f32; SOURCES] {
        let mut s = [0.0; SOURCES];
        s[SRC_KEY] = bipolar_key(key);
        s[SRC_VEL] = velocity;
        s[SRC_REL] = rel;
        s[SRC_AT] = self.pressure;
        s[SRC_MW] = self.wheel;
        s[SRC_FC] = self.foot;
        s[SRC_PB] = self.bend;
        s
    }

    /// Renders this engine's voices into `left` and `right`.
    pub fn render(
        &mut self,
        values: &[f64],
        first: Option<&SampleBuffer>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        if self.routes_dirty {
            self.rebuild_routes(values);
        }
        let frames = left.len();
        self.alloc.advance(frames);
        let Self {
            voices,
            alloc,
            matrix,
            slots,
            wheel,
            foot,
            pressure,
            bend,
            sr,
            clock,
            ..
        } = self;
        let sources_base = [
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, *pressure, *wheel, *foot, *bend,
        ];
        let sr = *sr;
        let get = |id: u32| values[id as usize];
        let volume = db_to_gain(values[legacy::VOLUME as usize] as f32);
        let filter_on = get(F_ON) >= 0.5;
        let shaper_on = get(SHAPER_ON) >= 0.5;
        let shaper_type = get(SHAPER_TYPE).round() as i32;
        let osc_on = get(OSC_ON) >= 0.5;
        let osc_am = get(OSC_MODE).round() as i32 == 1;
        let osc_kind = get(OSC_TYPE).round() as usize;
        for (index, v) in voices.iter_mut().enumerate() {
            if !v.active {
                continue;
            }
            let Some(sample) = sample_of(slots, first, v.slot) else {
                v.active = false;
                alloc.finished(index);
                continue;
            };
            let rate = sample.sample_rate / sr;
            let mut frame = 0usize;
            while frame < frames {
                if v.ctl == 0 {
                    Self::control(
                        v,
                        index,
                        values,
                        alloc,
                        matrix,
                        &sources_base,
                        rate,
                        sr as f32,
                        clock,
                        filter_on,
                        osc_on,
                        osc_am,
                    );
                }
                let run = (CONTROL_FRAMES - v.ctl).min(frames - frame);
                v.ctl = (v.ctl + run) % CONTROL_FRAMES;
                let mut alive = true;
                for i in frame..frame + run {
                    let mut step = v.step;
                    let mut am = 1.0f32;
                    if osc_on {
                        let m = osc_wave(osc_kind, v.osc.phase, &mut v.osc, &mut v.rng);
                        v.osc.phase = (v.osc.phase + v.osc_step).fract();
                        if osc_am {
                            am = (1.0 + m * v.osc_depth_am).max(0.0);
                        } else {
                            step *= 2f64.powf((m * v.osc_depth_fm) as f64);
                        }
                    }
                    let [mut l, mut r] = v.player.sample(sample, v.interp, v.released);
                    if shaper_on && !v.shape_filter_first {
                        let g = 1.0 + 8.0 * v.shaper_amount;
                        l = (1.0 - v.shaper_amount) * l
                            + v.shaper_amount * shape(shaper_type, l * g);
                        r = (1.0 - v.shaper_amount) * r
                            + v.shaper_amount * shape(shaper_type, r * g);
                    }
                    if filter_on {
                        l = v.filter[0].process(l);
                        r = v.filter[1].process(r);
                    }
                    if shaper_on && v.shape_filter_first {
                        let g = 1.0 + 8.0 * v.shaper_amount;
                        l = (1.0 - v.shaper_amount) * l
                            + v.shaper_amount * shape(shaper_type, l * g);
                        r = (1.0 - v.shaper_amount) * r
                            + v.shaper_amount * shape(shaper_type, r * g);
                    }
                    let env = v.amp.next();
                    left[i] += sanitize(l * env * v.gain_l * am) * volume;
                    right[i] += sanitize(r * env * v.gain_r * am) * volume;
                    alive = v.player.advance(step, v.released);
                    if !v.amp.is_active() {
                        alive = false;
                    }
                    if !alive {
                        break;
                    }
                }
                frame += run;
                if !alive {
                    v.active = false;
                    alloc.finished(index);
                    break;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn control(
        v: &mut MultiVoice,
        index: usize,
        values: &[f64],
        alloc: &VoiceAllocator,
        matrix: &ModMatrix,
        base: &[f32; SOURCES],
        rate: f64,
        sr: f32,
        clock: &BeatClock,
        filter_on: bool,
        osc_on: bool,
        osc_am: bool,
    ) {
        let get = |id: u32| values[id as usize];
        // LFOs (stereo for 2 and 3) ...
        let mut lfo_out = [[0.0f32; 2]; 3];
        for (i, base_id) in [L1_ON, L2_ON, L3_ON].iter().enumerate() {
            let g = |offset: u32| values[(base_id + offset) as usize];
            if g(0) < 0.5 {
                continue;
            }
            let rate_mod = 2f32.powf(v.bus_l.offset(16 + 2 * i) * 4.0);
            let settings_rate = if g(2) >= 0.5 {
                LfoRate::Beats(SyncRate::from_index(g(4)).beats() / rate_mod as f64)
            } else {
                let keyed = g(8) as f32 * (v.key - 60.0) / 12.0;
                LfoRate::Hz(g(3) as f32 * 2f32.powf(keyed) * rate_mod)
            };
            let (phase, spin) = if i > 0 {
                let stereo = |offset: u32| values[(*base_id + offset) as usize];
                // Stereo mode, Phase, Spin follow the shared LFO fields at +9, +10, +11.
                if stereo(9).round() as i32 == 1 {
                    (0.0, (stereo(11) * 0.01) as f32)
                } else {
                    (stereo(10) as f32, 0.0)
                }
            } else {
                (0.0, 0.0)
            };
            let settings = LfoSettings {
                shape: map_lfo(g(1)),
                rate: settings_rate,
                offset: g(7) as f32,
                phase,
                spin,
            };
            v.lfo[i].set(&settings);
            v.lfo[i].set_clock(clock);
            let out = v.lfo[i].advance(CONTROL_FRAMES);
            let attack = g(5) as f32 * 0.001 * sr;
            if attack > 0.0 {
                v.lfo_ramp[i] = (v.lfo_ramp[i] + CONTROL_FRAMES as f32 / attack).min(1.0);
            } else {
                v.lfo_ramp[i] = 1.0;
            }
            let depth = (1.0 + v.bus_l.offset(17 + 2 * i)).max(0.0) * v.lfo_ramp[i];
            lfo_out[i] = [out[0] * depth, out[1] * depth];
        }
        v.pitch_env_level = v.pitch_env.advance(CONTROL_FRAMES);
        v.filter_env_level = v.filter_env.advance(CONTROL_FRAMES);
        v.aux_level = v.aux_env.advance(CONTROL_FRAMES);
        v.osc_env_level = v.osc_env.advance(CONTROL_FRAMES);

        // Modulation for each channel.
        let mut sources = *base;
        sources[SRC_AUX] = v.aux_level * (1.0 + v.bus_l.offset(27)).max(0.0);
        sources[SRC_LFO1] = lfo_out[0][0];
        sources[SRC_LFO2] = lfo_out[1][0];
        sources[SRC_LFO3] = lfo_out[2][0];
        sources[SRC_KEY] = bipolar_key(v.key);
        sources[SRC_VEL] = v.velocity;
        sources[SRC_REL] = v.rel_vel;
        sources[SRC_AT] = sources[SRC_AT].max(alloc.voice(index).pressure);
        matrix.evaluate(&sources, &mut v.bus_l);
        sources[SRC_LFO1] = lfo_out[0][1];
        sources[SRC_LFO2] = lfo_out[1][1];
        sources[SRC_LFO3] = lfo_out[2][1];
        matrix.evaluate(&sources, &mut v.bus_r);

        // Pitch.
        let pitch_on = get(PE_ON) >= 0.5;
        let pe_amount = get(PE_AMOUNT) as f32 + v.bus_l.offset(26) * 48.0;
        let pitch_env = if pitch_on {
            v.pitch_env_level * pe_amount
        } else {
            0.0
        };
        let spread = (get(SPREAD) * 0.01) as f32;
        let semis = alloc.pitch(index) - v.root
            + values[legacy::TUNE as usize] as f32
            + (get(DETUNE) as f32 + v.layer_detune) * 0.01
            + pitch_env
            + v.bus_l.offset(3) * 12.0
            + v.side * spread * 0.3;
        v.step = 2f64.powf(semis as f64 / 12.0) * rate;
        // Loop modulation.
        if v.loop_kind != LoopKind::Off {
            let (a, b) = v.loop_points;
            let len = (b - a).max(1.0);
            let start_off = v.bus_l.offset(13) as f64 * len;
            let new_len = (len * (1.0 + v.bus_l.offset(14) as f64)).max(1.0);
            let crossfade = (v.crossfade * (1.0 + v.bus_l.offset(15) as f64)).max(0.0);
            v.player.set_sustain(
                (a + start_off, a + start_off + new_len),
                v.loop_kind,
                crossfade,
                v.power,
                v.loop_detune,
            );
        }
        // Modulation oscillator.
        if osc_on {
            let pitch_mod = v.bus_l.offset(10) * 24.0;
            let hz = if get(OSC_FIXED) >= 0.5 {
                let multi = [0.01, 0.1, 1.0, 10.0][(get(OSC_MULTI).round() as usize).min(3)] as f32;
                get(OSC_FREQ) as f32 * multi
            } else {
                let ratio = get(OSC_COARSE) as f32 + get(OSC_FINE) as f32 * 0.001;
                440.0
                    * 2f32.powf((v.key - 69.0) / 12.0)
                    * ratio
                    * 2f32.powf((pitch_env + 0.0) / 12.0)
            };
            let hz = hz * 2f32.powf(pitch_mod / 12.0);
            v.osc_step = (hz / sr).clamp(0.0, 0.49);
            let depth = {
                let vol = (get(OSC_VOL) * 0.01) as f32 * (1.0 + v.bus_l.offset(9)).max(0.0);
                let vel = (get(OSC_VOL_VEL) * 0.01) as f32 * (v.velocity - 1.0);
                ((vol + vel) * v.osc_env_level).clamp(0.0, 1.0)
            };
            v.osc_depth_fm = depth * if osc_am { 0.0 } else { 2.0 };
            v.osc_depth_am = depth * if osc_am { 1.0 } else { 0.0 };
        }
        // Filter.
        if filter_on {
            for (ch, bus) in [&v.bus_l, &v.bus_r].into_iter().enumerate() {
                let fe = if get(FE_ON) >= 0.5 {
                    v.filter_env_level * (get(FE_AMOUNT) as f32 + bus.offset(25) * 72.0)
                } else {
                    0.0
                };
                let cutoff = get(F_FREQ) as f32 * 2f32.powf((fe + bus.offset(4) * 48.0) / 12.0);
                let settings: FilterSettings = filter_settings(
                    get(F_TYPE),
                    get(F_CIRCUIT),
                    get(F_SLOPE),
                    cutoff.clamp(20.0, 20_000.0),
                    (get(F_RES) as f32 + bus.offset(5)).clamp(0.0, 1.0),
                    (get(F_DRIVE) as f32 + bus.offset(7) * 24.0).max(0.0),
                    ((get(F_MORPH) * 0.01) as f32 + bus.offset(6)).rem_euclid(1.0),
                );
                v.filter[ch].set(&settings);
            }
        }
        v.shaper_amount = (get(SHAPER_AMOUNT) as f32 + v.bus_l.offset(8)).clamp(0.0, 1.0);
        // Gain and pan, per channel because stereo LFOs can move them apart.
        let velocity_sense = values[legacy::VELOCITY as usize] as f32;
        let vel_gain = 1.0 - velocity_sense + velocity_sense * v.velocity;
        let base_pan =
            values[legacy::PAN as usize] as f32 + v.layer_pan + v.random_pan + v.side * spread;
        for (ch, bus) in [&v.bus_l, &v.bus_r].into_iter().enumerate() {
            let pan = (base_pan + bus.offset(2)).clamp(-1.0, 1.0);
            let side = if ch == 0 {
                (1.0 - pan).min(1.0)
            } else {
                (1.0 + pan).min(1.0)
            };
            let gain = v.layer_gain * vel_gain * (1.0 + bus.offset(1)).max(0.0) * side;
            if ch == 0 {
                v.gain_l = gain;
            } else {
                v.gain_r = gain;
            }
        }
    }

    /// Publishes the selected layer's waveform and which layers are sounding.
    pub fn publish(&mut self, view: &rlive_core::ViewWriter, selected: usize) {
        let slot = self
            .zones
            .map
            .layers
            .get(selected)
            .map_or(0, |l| l.slot as usize);
        self.selected_view = slot;
        if let Some(Some(a)) = self.analyses.get(slot) {
            view.publish(VIEW_WAVE, &a.overview);
        }
        let mut words = [0f32; ACTIVE_WORDS];
        let mut bits = [0u32; ACTIVE_WORDS];
        for v in self.voices.iter().filter(|v| v.active) {
            for (i, l) in self.zones.map.layers.iter().enumerate() {
                if l.slot as usize == v.slot {
                    bits[(i / 16).min(ACTIVE_WORDS - 1)] |= 1 << (i % 16);
                }
            }
        }
        for (w, b) in words.iter_mut().zip(bits) {
            *w = b as f32;
        }
        view.publish(VIEW_ACTIVE, &words);
    }

    /// The analysis of a slot, if it has one, for the Sampler's tests and tools.
    pub fn analysis(&self, slot: usize) -> Option<&Arc<Analysis>> {
        self.analyses.get(slot)?.as_ref()
    }
}
