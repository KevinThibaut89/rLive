//! Tuning systems and MPE output, per track.

use crate::track::{DeviceBody, DeviceSlot};
use crate::{retire, Engine, Garbage};
use rlive_core::{KeyLayout, MpeEncoder, TrackId, TuningTable, TUNING_SLOT};
use std::sync::Arc;

pub enum TuningCmd {
    /// What a track plays: its tuning, if it isn't bypassed, and its controller layout.
    /// `table` is always given, so devices can be told even when there is no tuning.
    SetTrack {
        track: TrackId,
        table: Arc<TuningTable>,
        active: bool,
        layout: Option<Arc<KeyLayout>>,
    },
    /// Where a track's MIDI goes and how it is spread over channels.
    SetOutput {
        track: TrackId,
        port: Option<u16>,
        encoder: Option<Box<MpeEncoder>>,
    },
}

fn deliver(
    devices: &mut [Box<DeviceSlot>],
    table: &Arc<TuningTable>,
    garbage: &mut rtrb::Producer<Garbage>,
) {
    for d in devices.iter_mut() {
        if let DeviceBody::Rack(r) = &mut d.body {
            for chain in r.chains.iter_mut().chain(r.returns.iter_mut()) {
                deliver(&mut chain.devices, table, garbage);
            }
        }
        if let Some(back) = d.set_resource(TUNING_SLOT, table.clone()) {
            retire(garbage, Garbage::Resource(back));
        }
    }
}

impl Engine {
    pub(crate) fn handle_tuning(&mut self, cmd: TuningCmd) {
        match cmd {
            TuningCmd::SetTrack {
                track,
                table,
                active,
                layout,
            } => {
                let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) else {
                    retire(&mut self.garbage, Garbage::Shared(table));
                    if let Some(l) = layout {
                        retire(&mut self.garbage, Garbage::Shared(l));
                    }
                    return;
                };
                deliver(&mut t.devices, &table, &mut self.garbage);
                let old = if active {
                    t.tuning.replace(table)
                } else {
                    retire(&mut self.garbage, Garbage::Shared(table));
                    t.tuning.take()
                };
                if let Some(old) = old {
                    retire(&mut self.garbage, Garbage::Shared(old));
                }
                let old = std::mem::replace(&mut t.key_layout, layout);
                if let Some(old) = old {
                    retire(&mut self.garbage, Garbage::Shared(old));
                }
            }
            TuningCmd::SetOutput {
                track,
                port,
                encoder,
            } => {
                let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) else {
                    if let Some(e) = encoder {
                        retire(&mut self.garbage, Garbage::Boxed(e));
                    }
                    return;
                };
                t.hw_port = port;
                if t.hw_port.is_some() || encoder.is_some() {
                    t.midi_tap = true;
                }
                if let Some(old) = std::mem::replace(&mut t.mpe_out, encoder) {
                    retire(&mut self.garbage, Garbage::Boxed(old));
                }
            }
        }
    }
}
