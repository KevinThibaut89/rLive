//! External Audio Effect and External Instrument: where they connect, and what they can reach.

use super::graph::{audio_outputs, deps_without, Side};
use super::{AudioHardware, ChainRef, Session};
use rlive_core::{DeviceId, MpeOutput, TrackId};
use rlive_devices::devices::b9::external_instrument::INTERNAL_LATENCY;
use rlive_engine::{EngineCommand, PortRoute, TuningCmd};
use rlive_project::{
    Device, DeviceKind, ExternalIo, ExternalMidi, ExternalReturn, ExternalRoute, Project, Track,
};
use std::collections::HashMap;

const INSTRUMENT: &str = "rlive.external_instrument";

/// Where an External Instrument's MIDI goes, as the engine has been told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeviceOutput {
    track: TrackId,
    route: PortRoute,
    mpe: Option<MpeOutput>,
}

fn is_internal(io: &ExternalIo) -> bool {
    matches!(io.audio_from, Some(ExternalReturn::Aux { .. }))
}

/// How long the path through a plug-in on another track takes: a whole block.
pub(super) fn set_internal_latency(project: &mut Project, block: usize) {
    for t in project.tracks.iter_mut() {
        for d in &mut t.devices {
            d.for_each_mut(&mut |d| {
                let is_instrument = matches!(
                    &d.kind,
                    DeviceKind::Builtin { type_id, .. } if type_id == INSTRUMENT
                );
                let internal = d.external.as_ref().is_some_and(is_internal);
                if is_instrument {
                    let value = if internal { block as f64 } else { 0.0 };
                    if d.params.get(&INTERNAL_LATENCY).copied().unwrap_or(0.0) != value {
                        d.params.insert(INTERNAL_LATENCY, value);
                    }
                }
            });
        }
    }
}

/// One thing an External device's chooser can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalChoice {
    pub label: String,
    pub route: ExternalRoute,
}

/// A multitimbral target another track offers: its instrument and the tracks it can be reached
/// through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTarget {
    pub label: String,
    pub track: TrackId,
    pub device: DeviceId,
    /// Names of the plug-in's auxiliary outputs.
    pub aux: Vec<String>,
}

impl Session {
    /// What the Audio To and Audio From choosers offer.
    pub fn external_outputs(&self, hw: &AudioHardware) -> Vec<ExternalChoice> {
        hw.outputs
            .iter()
            .map(|(label, route)| ExternalChoice {
                label: label.clone(),
                route: route.clone(),
            })
            .collect()
    }

    pub fn external_inputs(&self, hw: &AudioHardware) -> Vec<ExternalChoice> {
        hw.inputs
            .iter()
            .map(|(label, route)| ExternalChoice {
                label: label.clone(),
                route: route.clone(),
            })
            .collect()
    }

    /// The plug-ins on other tracks that External Instrument can play (manual p. 741).
    pub fn plugin_targets(&self, device: DeviceId) -> Vec<PluginTarget> {
        let Some(ChainRef::Track(own)) = self.chain_of_device(device).map(ChainRef::owner) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for t in self.project.tracks.iter().chain(&self.project.returns) {
            if t.id == own {
                continue;
            }
            for d in &t.devices {
                self.collect_plugin(t, d, &mut out);
            }
        }
        out
    }

    fn collect_plugin(&self, t: &Track, d: &Device, out: &mut Vec<PluginTarget>) {
        if matches!(d.kind, DeviceKind::Plugin { .. }) {
            let aux = self
                .runtimes
                .get(&d.id)
                .map_or_else(Vec::new, |rt| rt.aux_names.clone());
            out.push(PluginTarget {
                label: format!("{} | {}", t.name, d.name),
                track: t.id,
                device: d.id,
                aux,
            });
        }
        for c in d.chains() {
            for child in &c.devices {
                self.collect_plugin(t, child, out);
            }
        }
    }

    /// Connects an External device. MIDI to a track that feeds this one is refused.
    pub fn set_external_io(&mut self, chain: ChainRef, device: DeviceId, io: ExternalIo) -> bool {
        if self.devices_locked(chain) {
            return false;
        }
        if let (Some(ExternalMidi::Track { track, .. }), ChainRef::Track(own)) =
            (&io.midi_to, chain.owner())
        {
            let index = |id| {
                self.project
                    .tracks
                    .iter()
                    .chain(&self.project.returns)
                    .position(|t| t.id == id)
            };
            let audio = audio_outputs(&self.project, &self.timing);
            if let (Some(i), Some(to)) = (index(own), index(*track)) {
                if deps_without(&self.project, &audio, i, Side::Output).closes_loop(i, to) {
                    return false;
                }
            }
        }
        let internal = is_internal(&io);
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == device))
        else {
            return false;
        };
        d.external = Some(io);
        let instrument = matches!(
            &d.kind,
            DeviceKind::Builtin { type_id, .. } if type_id == INSTRUMENT
        );
        if instrument {
            let value = if internal {
                self.engine.max_block as f64
            } else {
                0.0
            };
            self.write_param(chain, device, INTERNAL_LATENCY, value, false);
        }
        self.send_track_edits(Vec::new());
        self.mark_edit("Change External Routing");
        true
    }

    /// The output ports changed, so what points at them has to be told their new numbers.
    pub fn set_midi_out_ports(&mut self, ports: Vec<String>) {
        if self.midi_out_ports == ports {
            return;
        }
        self.midi_out_ports = ports;
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for track in tracks {
            self.send_midi_output(track);
        }
        self.sent_device_outputs.clear();
        self.mods_dirty = true;
    }

    /// Tells the engine about every External Instrument whose MIDI port has changed.
    pub(super) fn sync_device_outputs(&mut self) {
        let mut wanted = HashMap::new();
        let ports = &self.midi_out_ports;
        for t in self.project.tracks.iter().chain(&self.project.returns) {
            for d in &t.devices {
                d.for_each(&mut |d| {
                    let Some(ExternalMidi::Port { port, channel }) =
                        d.external.as_ref().and_then(|e| e.midi_to.as_ref())
                    else {
                        return;
                    };
                    if let Some(i) = ports.iter().position(|n| n == port) {
                        wanted.insert(
                            d.id,
                            DeviceOutput {
                                track: t.id,
                                route: PortRoute {
                                    port: i as u16,
                                    // MPE Mode picks the channels itself.
                                    channel: d.mpe.is_none().then_some(*channel),
                                },
                                mpe: d.mpe,
                            },
                        );
                    }
                });
            }
        }
        let gone: Vec<(DeviceId, TrackId)> = self
            .sent_device_outputs
            .iter()
            .filter(|(id, _)| !wanted.contains_key(*id))
            .map(|(id, sent)| (*id, sent.track))
            .collect();
        for (device, track) in gone {
            self.sent_device_outputs.remove(&device);
            self.send(EngineCommand::Tuning(TuningCmd::SetDeviceOutput {
                track,
                device,
                route: None,
                encoder: None,
            }));
        }
        for (device, output) in wanted {
            if self.sent_device_outputs.get(&device) == Some(&output) {
                continue;
            }
            self.sent_device_outputs.insert(device, output);
            self.send(EngineCommand::Tuning(TuningCmd::SetDeviceOutput {
                track: output.track,
                device,
                route: Some(output.route),
                encoder: output
                    .mpe
                    .map(|o| Box::new(rlive_core::MpeEncoder::configured(o))),
            }));
        }
    }

    /// Fires a Drum Synth for its display's preview. The sound stops by itself, so there's no
    /// note-off to send.
    pub fn preview_device(&mut self, chain: ChainRef, device: DeviceId) {
        self.send_ops(vec![rlive_engine::GraphOp::DirectNote {
            target: chain.owner().into(),
            device,
            message: rlive_core::MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: 0,
                tuning: 0.0,
            },
        }]);
    }

    /// Whether what this device connects to is in the Set, so Hardware Latency has no use.
    pub fn external_is_internal(&self, device: DeviceId) -> bool {
        self.chain_of_device(device)
            .and_then(|c| self.devices(c).iter().find(|d| d.id == device))
            .and_then(|d| d.external.as_ref())
            .is_some_and(is_internal)
    }
}
