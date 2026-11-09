//! What Collision, Electric, Tension and Operator share: note-to-frequency, stretch tuning, the
//! modal bank, and the short fade a stolen voice gets.

use crate::dsp::util::{midi_to_hz, sanitize};
use crate::dsp::voice::NoteInfo;
use rlive_core::{ExprKind, Item, MidiMessage, ParamId, MPE_BEND_RANGE};

/// How many samples a block of control-rate work covers.
pub const CONTROL: usize = 32;

/// How long a stolen voice fades before the new note starts, in samples.
pub const STEAL_FADE: u32 = 96;

/// Hertz of a MIDI-numbered pitch, the only place these instruments turn pitch into frequency:
/// tuning systems plug in here.
#[inline]
pub fn note_hz(pitch: f32) -> f32 {
    midi_to_hz(pitch)
}

/// Stretch tuning: at 100% a note is sharpened by half a cent per semitone above C3 and flattened
/// by the same below it. The manual gives no figure, so this one is ours.
#[inline]
pub fn stretch_semitones(pitch: f32, stretch_percent: f32) -> f32 {
    (pitch - 60.0) * 0.005 * stretch_percent / 100.0
}

/// A stateless ceiling on output samples: transparent up to 0.8, never past 1.0.
#[inline]
pub fn limit(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = sanitize(x).abs();
    if a <= KNEE {
        return sanitize(x);
    }
    let over = (a - KNEE) / (1.0 - KNEE);
    (KNEE + (1.0 - KNEE) * (over / (1.0 + over))).copysign(x)
}

/// Rescales a per-note pitch message to a device's Note PB range. Live's per-note range is 48
/// semitones, and a Note PB of 48 passes it through unchanged.
pub fn scale_note_bend(message: MidiMessage, note_pb: f32) -> MidiMessage {
    match message {
        MidiMessage::NoteExpr {
            channel,
            key,
            note_id,
            kind: ExprKind::Pitch,
            value,
        } => MidiMessage::NoteExpr {
            channel,
            key,
            note_id,
            kind: ExprKind::Pitch,
            value: value * note_pb / MPE_BEND_RANGE,
        },
        other => other,
    }
}

pub fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

pub fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

pub fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

/// A voice fading out because it was stolen, and the note waiting for it to finish.
#[derive(Clone, Copy, Debug, Default)]
pub struct StealFade {
    pending: Option<NoteInfo>,
    left: u32,
    armed: bool,
}

impl StealFade {
    /// Starts the fade, or just changes the note that will follow one already running.
    pub fn start(&mut self, pending: Option<NoteInfo>) {
        self.pending = pending;
        if !self.armed {
            self.left = STEAL_FADE;
            self.armed = true;
        }
    }

    pub fn clear(&mut self) {
        self.pending = None;
        self.left = 0;
        self.armed = false;
    }

    pub fn is_fading(&self) -> bool {
        self.armed
    }

    /// The gain for the next sample, 1 when not fading.
    #[inline]
    pub fn gain(&mut self) -> f32 {
        if !self.armed {
            return 1.0;
        }
        self.left = self.left.saturating_sub(1);
        self.left as f32 / STEAL_FADE as f32
    }

    /// When a fade has just finished: `Some(Some(note))` to start, `Some(None)` to go silent.
    pub fn poll(&mut self) -> Option<Option<NoteInfo>> {
        if self.armed && self.left == 0 {
            self.armed = false;
            Some(self.pending.take())
        } else {
            None
        }
    }
}

/// The most modes a [`ModalBank`] holds.
pub const MAX_MODES: usize = 32;

/// Decaying resonators, one per mode, with two output weights each. Every mode is a one-pole
/// complex rotation, so a mode's frequency can move while it rings without changing its amplitude
/// and it stays stable however the coefficients change.
#[derive(Clone, Debug)]
pub struct ModalBank {
    pub count: usize,
    re: [f32; MAX_MODES],
    im: [f32; MAX_MODES],
    cr: [f32; MAX_MODES],
    ci: [f32; MAX_MODES],
    input: [f32; MAX_MODES],
    out_a: [f32; MAX_MODES],
    out_b: [f32; MAX_MODES],
}

impl Default for ModalBank {
    fn default() -> Self {
        Self {
            count: 0,
            re: [0.0; MAX_MODES],
            im: [0.0; MAX_MODES],
            cr: [0.0; MAX_MODES],
            ci: [0.0; MAX_MODES],
            input: [0.0; MAX_MODES],
            out_a: [0.0; MAX_MODES],
            out_b: [0.0; MAX_MODES],
        }
    }
}

impl ModalBank {
    pub fn with_count(count: usize) -> Self {
        Self {
            count: count.min(MAX_MODES),
            ..Default::default()
        }
    }

    pub fn clear(&mut self) {
        self.re = [0.0; MAX_MODES];
        self.im = [0.0; MAX_MODES];
    }

    /// Sets a mode's rotation for `hz` and a sixty-decibel decay of `t60` seconds.
    #[inline]
    pub fn tune(&mut self, i: usize, hz: f32, t60: f32, sample_rate: f32) {
        let r = (-6.907_755 / (t60.max(1e-4) * sample_rate)).exp();
        let w = std::f32::consts::TAU * hz / sample_rate;
        self.cr[i] = r * w.cos();
        self.ci[i] = r * w.sin();
    }

    /// Sets how much of the input a mode takes and how much it adds to each output.
    #[inline]
    pub fn weights(&mut self, i: usize, input: f32, out_a: f32, out_b: f32) {
        self.input[i] = input;
        self.out_a[i] = out_a;
        self.out_b[i] = out_b;
    }

    /// Drives every mode with `x`, returning the two outputs.
    #[inline]
    pub fn tick(&mut self, x: f32) -> [f32; 2] {
        let (mut a, mut b) = (0.0f32, 0.0f32);
        let n = self.count;
        for i in 0..n {
            let (re, im) = (self.re[i], self.im[i]);
            let new_im = re * self.ci[i] + im * self.cr[i];
            self.re[i] = re * self.cr[i] - im * self.ci[i] + self.input[i] * x;
            self.im[i] = new_im;
            a += new_im * self.out_a[i];
            b += new_im * self.out_b[i];
        }
        [a, b]
    }

    /// Clears denormals and anything non-finite. Call once a control block.
    pub fn sanitize(&mut self) {
        for i in 0..self.count {
            self.re[i] = sanitize(self.re[i]);
            self.im[i] = sanitize(self.im[i]);
        }
    }

    /// The largest mode amplitude.
    pub fn level(&self) -> f32 {
        let mut m = 0.0f32;
        for i in 0..self.count {
            m = m.max((self.re[i] * self.re[i] + self.im[i] * self.im[i]).sqrt());
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_is_zero_at_c3_and_opposite_either_side() {
        assert_eq!(stretch_semitones(60.0, 100.0), 0.0);
        assert!(stretch_semitones(84.0, 100.0) > 0.0);
        assert!(stretch_semitones(36.0, 100.0) < 0.0);
        assert!(stretch_semitones(84.0, -100.0) < 0.0);
    }

    #[test]
    fn the_limit_never_passes_one_and_leaves_small_signals_alone() {
        assert_eq!(limit(0.5), 0.5);
        assert!(limit(100.0) <= 1.0);
        assert!(limit(-1e9) >= -1.0);
        assert_eq!(limit(f32::NAN), 0.0);
    }

    #[test]
    fn a_mode_rings_at_its_frequency_and_decays_to_sixty_db_in_its_t60() {
        let sr = 48_000.0;
        let mut b = ModalBank::with_count(1);
        b.tune(0, 1_000.0, 0.5, sr);
        b.weights(0, 1.0, 1.0, 0.0);
        b.tick(1.0);
        for _ in 0..24_000 {
            b.tick(0.0);
        }
        let mut late = 0.0f32;
        for _ in 0..100 {
            late = late.max(b.tick(0.0)[0].abs());
        }
        assert!((late - 0.001).abs() < 0.0003, "{late}");
    }
}
