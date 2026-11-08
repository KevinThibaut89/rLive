//! Transport, loop and global launch quantization.

use crate::ui::RliveApp;
use rlive_core::Quantize;

super::domain! {
    /// Commands with no menu, reached only through the keymap.
    Transport {
        PlayStop,
        Continue,
        Record,
        BackToArrangement,
        ToggleMetronome,
        InsertMarkerToStart,
        ToggleLoop,
        QuantizeSixteenth,
        QuantizeEighth,
        QuantizeQuarter,
        QuantizeBar,
        QuantizeOff,
    }
}

impl RliveApp {
    pub(super) fn transport_command(&mut self, c: Transport) {
        match c {
            Transport::PlayStop => {
                if self.session.is_playing() {
                    self.session.stop();
                } else {
                    self.session.play();
                }
            }
            Transport::Continue => self.session.play(),
            Transport::Record => self.toggle_recording(),
            Transport::BackToArrangement => self.session.back_to_arrangement(),
            Transport::ToggleMetronome => {
                self.session.set_metronome(!self.session.project.metronome)
            }
            Transport::InsertMarkerToStart => self.session.seek(0.0),
            Transport::ToggleLoop => {
                let a = self.session.project.arrangement.clone();
                self.session
                    .set_loop(!a.loop_enabled, a.loop_start, a.loop_end);
            }
            Transport::QuantizeSixteenth => self.session.set_quantize(Quantize::Sixteenth),
            Transport::QuantizeEighth => self.session.set_quantize(Quantize::Eighth),
            Transport::QuantizeQuarter => self.session.set_quantize(Quantize::Beat),
            Transport::QuantizeBar => self.session.set_quantize(Quantize::Bar),
            Transport::QuantizeOff => self.session.set_quantize(Quantize::None),
        }
    }
}
