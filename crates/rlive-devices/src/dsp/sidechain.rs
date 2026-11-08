//! Sidechain detector: routes, gains, mixes, equalises and follows a key signal.

use super::basics::{Biquad, BiquadKind};
use super::follower::{DetectMode, EnvelopeFollower};
use super::util::{db_to_gain, lerp, sanitize};
use rlive_core::{AudioBuffer, AudioInput};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SidechainEq {
    Off,
    #[default]
    LowPass,
    BandPass,
    HighPass,
    Notch,
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SidechainSettings {
    /// Gain on the external signal, in decibels.
    pub gain_db: f32,
    /// How much of the key comes from the external signal, 0..1. The rest comes from the main
    /// signal. Without an external signal the main signal is used in full.
    pub mix: f32,
    pub eq: SidechainEq,
    pub eq_freq: f32,
    pub eq_q: f32,
    pub eq_gain_db: f32,
    pub attack: f32,
    pub release: f32,
    pub detect: DetectMode,
    /// Whether the key should replace the device's output; see [`SidechainDetector::listen`].
    pub listen: bool,
}

impl Default for SidechainSettings {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            mix: 1.0,
            eq: SidechainEq::Off,
            eq_freq: 1_000.0,
            eq_q: 0.71,
            eq_gain_db: 0.0,
            attack: 0.005,
            release: 0.1,
            detect: DetectMode::Peak,
            listen: false,
        }
    }
}

/// A key signal and its level, valid until the next [`SidechainDetector::process`].
#[derive(Clone, Copy, Debug)]
pub struct SidechainKey<'a> {
    pub left: &'a [f32],
    pub right: &'a [f32],
    /// The follower's level for each frame.
    pub level: &'a [f32],
}

#[derive(Clone, Debug)]
pub struct SidechainDetector {
    settings: SidechainSettings,
    sample_rate: f32,
    left: Vec<f32>,
    right: Vec<f32>,
    level: Vec<f32>,
    eq: [Biquad; 2],
    follower: EnvelopeFollower,
    gain: f32,
    mix: f32,
    frames: usize,
}

impl Default for SidechainDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl SidechainDetector {
    pub fn new() -> Self {
        Self {
            settings: SidechainSettings::default(),
            sample_rate: 48_000.0,
            left: Vec::new(),
            right: Vec::new(),
            level: Vec::new(),
            eq: [Biquad::default(); 2],
            follower: EnvelopeFollower::new(),
            gain: 1.0,
            mix: 1.0,
            frames: 0,
        }
    }

    pub fn prepare(&mut self, sample_rate: f64, max_block: usize) {
        self.sample_rate = sample_rate.max(1.0) as f32;
        self.left = vec![0.0; max_block];
        self.right = vec![0.0; max_block];
        self.level = vec![0.0; max_block];
        self.follower.prepare(sample_rate);
        let s = self.settings;
        self.set(&s);
        self.gain = db_to_gain(s.gain_db);
        self.mix = s.mix.clamp(0.0, 1.0);
        self.reset();
    }

    pub fn set(&mut self, settings: &SidechainSettings) {
        self.settings = *settings;
        let s = settings;
        let kind = match s.eq {
            SidechainEq::Off => None,
            SidechainEq::LowPass => Some(BiquadKind::LowPass),
            SidechainEq::BandPass => Some(BiquadKind::BandPass),
            SidechainEq::HighPass => Some(BiquadKind::HighPass),
            SidechainEq::Notch => Some(BiquadKind::Notch),
            SidechainEq::Peak => Some(BiquadKind::Peak),
            SidechainEq::LowShelf => Some(BiquadKind::LowShelf),
            SidechainEq::HighShelf => Some(BiquadKind::HighShelf),
        };
        if let Some(kind) = kind {
            for eq in &mut self.eq {
                eq.set(kind, s.eq_freq, s.eq_q, s.eq_gain_db, self.sample_rate);
            }
        }
        self.follower.set_mode(s.detect);
        self.follower.set_times(s.attack, s.release);
    }

    pub fn settings(&self) -> &SidechainSettings {
        &self.settings
    }

    pub fn reset(&mut self) {
        for eq in &mut self.eq {
            eq.reset();
        }
        self.follower.reset();
        self.gain = db_to_gain(self.settings.gain_db);
        self.mix = self.settings.mix.clamp(0.0, 1.0);
    }

    /// Builds this block's key from the main signal and any external one, and follows its level.
    /// Gain and mix ramp across the block, so moving them doesn't step. Only as many frames as
    /// `prepare` allowed are looked at.
    pub fn process(
        &mut self,
        main_left: &[f32],
        main_right: &[f32],
        external: Option<AudioInput<'_>>,
    ) -> SidechainKey<'_> {
        let frames = main_left
            .len()
            .min(main_right.len())
            .min(self.left.len())
            .min(external.map_or(usize::MAX, |e| e.frames()));
        let target_gain = db_to_gain(self.settings.gain_db);
        let target_mix = self.settings.mix.clamp(0.0, 1.0);
        let dt = if frames == 0 {
            0.0
        } else {
            1.0 / frames as f32
        };
        for i in 0..frames {
            let t = (i + 1) as f32 * dt;
            let (mut l, mut r) = (sanitize(main_left[i]), sanitize(main_right[i]));
            if let Some(ext) = &external {
                let g = lerp(self.gain, target_gain, t);
                let m = lerp(self.mix, target_mix, t);
                l = lerp(l, sanitize(ext.left[i]) * g, m);
                r = lerp(r, sanitize(ext.right[i]) * g, m);
            }
            self.left[i] = l;
            self.right[i] = r;
        }
        self.gain = target_gain;
        self.mix = target_mix;
        if self.settings.eq != SidechainEq::Off {
            for s in &mut self.left[..frames] {
                *s = sanitize(self.eq[0].process(*s));
            }
            for s in &mut self.right[..frames] {
                *s = sanitize(self.eq[1].process(*s));
            }
        }
        for i in 0..frames {
            let peak = self.left[i].abs().max(self.right[i].abs());
            self.level[i] = self.follower.process(peak);
        }
        self.frames = frames;
        SidechainKey {
            left: &self.left[..frames],
            right: &self.right[..frames],
            level: &self.level[..frames],
        }
    }

    /// If Listen is on, replaces `audio` with the current key.
    pub fn listen(&self, audio: &mut AudioBuffer<'_>) {
        if self.settings.listen {
            let n = self.frames.min(audio.frames());
            audio.left[..n].copy_from_slice(&self.left[..n]);
            audio.right[..n].copy_from_slice(&self.right[..n]);
        }
    }
}
