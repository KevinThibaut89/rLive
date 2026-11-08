//! The device registry: one module per device, grouped by batch.
//!
//! Each module exports its own `INFO` row, so building a device means editing only its file. The
//! devices that existed before the registry keep their code under `effects`, `sampler` and
//! `synth`, and list here first so the browser order doesn't change.

use crate::BuiltinInfo;

/// Declares a stub: a device in the registry that passes everything through and is hidden from
/// the browser. MIDI effects forward their events; audio effects and instruments leave the audio
/// alone, so an instrument adds silence and an effect is transparent.
macro_rules! stub_device {
    ($id:literal, $name:literal, $category:literal, $role:ident $(, mod_out: [$($m:literal),*])?) => {
        /// The passthrough for this stub.
        #[derive(Default)]
        pub struct Stub;

        impl rlive_core::Processor for Stub {
            fn activate(&mut self, _: f64, _: usize) {}

            fn process(&mut self, ctx: &mut rlive_core::ProcessContext) {
                if $crate::DeviceRole::$role == $crate::DeviceRole::MidiEffect {
                    ctx.events_out.extend(ctx.events);
                }
            }

            fn set_param(&mut self, _: u32, _: f64) {}
        }

        pub static INFO: $crate::BuiltinInfo = $crate::BuiltinInfo {
            id: $id,
            name: $name,
            role: $crate::DeviceRole::$role,
            category: $category,
            status: rlive_core::Status::Stub,
            desc: || rlive_core::DeviceDesc {
                ports: rlive_core::Ports {
                    midi_out: $crate::DeviceRole::$role == $crate::DeviceRole::MidiEffect,
                    mod_out: vec![$($($m),*)?],
                    ..Default::default()
                },
                ..Default::default()
            },
            create: |_| Box::<Stub>::default(),
        };
    };
}

pub(crate) use stub_device;

pub mod b1;
pub mod b10;
pub mod b11;
pub mod b12;
pub mod b13;
pub mod b2;
pub mod b3;
pub mod b4;
pub mod b5;
pub mod b6;
pub mod b7;
pub mod b8;
pub mod b9;
pub mod extra;

/// Every device, in the order the browser lists them.
pub static ALL: &[&BuiltinInfo] = &[
    &extra::poly_synth::INFO,
    &extra::juno106::INFO,
    &b10::sampler::INFO,
    &b4::eq_three::INFO,
    &b3::compressor::INFO,
    &b5::saturator::INFO,
    &b6::delay::INFO,
    &b7::reverb::INFO,
    &b4::utility::INFO,
    &b1::pitch::INFO,
    &b1::velocity::INFO,
    &b1::scale::INFO,
    &b1::note_length::INFO,
    &b1::chord::INFO,
    &b1::random::INFO,
    &b1::cc_control::INFO,
    &b1::arpeggiator::INFO,
    &b1::midi_monitor::INFO,
    &b1::note_echo::INFO,
    &b1::expression_control::INFO,
    &b1::mpe_control::INFO,
    &b2::lfo::INFO,
    &b2::envelope_follower::INFO,
    &b2::shaper::INFO,
    &b2::envelope_midi::INFO,
    &b2::shaper_midi::INFO,
    &b3::gate::INFO,
    &b3::glue_compressor::INFO,
    &b3::limiter::INFO,
    &b3::multiband_dynamics::INFO,
    &b3::drum_buss::INFO,
    &b4::eq_eight::INFO,
    &b4::channel_eq::INFO,
    &b4::auto_filter::INFO,
    &b4::auto_pan_tremolo::INFO,
    &b5::overdrive::INFO,
    &b5::dynamic_tube::INFO,
    &b5::pedal::INFO,
    &b5::amp::INFO,
    &b5::cabinet::INFO,
    &b5::erosion::INFO,
    &b5::redux::INFO,
    &b5::vinyl_distortion::INFO,
    &b5::roar::INFO,
    &b6::echo::INFO,
    &b6::filter_delay::INFO,
    &b6::align_delay::INFO,
    &b6::chorus_ensemble::INFO,
    &b6::phaser_flanger::INFO,
    &b6::beat_repeat::INFO,
    &b6::grain_delay::INFO,
    &b6::shifter::INFO,
    &b6::looper::INFO,
    &b7::hybrid_reverb::INFO,
    &b7::resonators::INFO,
    &b7::corpus::INFO,
    &b8::spectrum::INFO,
    &b8::tuner::INFO,
    &b8::spectral_resonator::INFO,
    &b8::spectral_time::INFO,
    &b8::vocoder::INFO,
    &b8::auto_shift::INFO,
    &b9::external_audio_effect::INFO,
    &b9::external_instrument::INFO,
    &b10::simpler::INFO,
    &b10::drum_sampler::INFO,
    &b10::impulse::INFO,
    &b11::drift::INFO,
    &b11::analog::INFO,
    &b11::wavetable::INFO,
    &b11::meld::INFO,
    &b12::operator::INFO,
    &b12::electric::INFO,
    &b12::tension::INFO,
    &b12::collision::INFO,
    &b13::ds_kick::INFO,
    &b13::ds_snare::INFO,
    &b13::ds_clap::INFO,
    &b13::ds_tom::INFO,
    &b13::ds_hh::INFO,
    &b13::ds_cymbal::INFO,
    &b13::ds_fm::INFO,
    &b13::ds_clang::INFO,
];
