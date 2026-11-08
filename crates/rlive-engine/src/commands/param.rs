//! Mixer settings and device parameters.

use crate::{ChainTarget, Engine, MixSettings};
use rlive_core::{DeviceId, TrackId};

pub enum ParamCmd {
    SetMix {
        track: TrackId,
        mix: MixSettings,
    },
    SetMasterVolume(f32),
    SetBypass {
        target: ChainTarget,
        device: DeviceId,
        bypass: bool,
    },
    SetParam {
        target: ChainTarget,
        device: DeviceId,
        param: u32,
        value: f64,
    },
}

impl Engine {
    pub(crate) fn handle_param(&mut self, cmd: ParamCmd) {
        use ParamCmd as C;
        match cmd {
            C::SetMix { track, mix } => {
                if let Some(t) = self.track_mut(track) {
                    t.mix = mix;
                }
            }
            C::SetMasterVolume(db) => self.master.volume_db = db,
            C::SetBypass {
                target,
                device,
                bypass,
            } => {
                if let Some(chain) = self.chain_mut(target) {
                    if let Some(d) = chain.iter_mut().find(|d| d.id == device) {
                        d.bypass = bypass;
                        if bypass {
                            d.processor.reset();
                        }
                    }
                }
            }
            C::SetParam {
                target,
                device,
                param,
                value,
            } => {
                if let Some(chain) = self.chain_mut(target) {
                    if let Some(d) = chain.iter_mut().find(|d| d.id == device) {
                        d.processor.set_param(param, value);
                    }
                }
            }
        }
    }
}
