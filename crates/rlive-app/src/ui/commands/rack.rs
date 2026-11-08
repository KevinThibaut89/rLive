//! Commands that act on the racks in the Device View and the Session View's rack mixer.

use crate::session::Session;
use crate::ui::devices::rack::{KeyFocus, Rename, RenameTarget};
use crate::ui::devices::{group_selected, ungroup_selected};
use crate::ui::RliveApp;
use rlive_core::param::chain as controls;

super::domain! {
    /// Commands with no menu, reached only through the keymap.
    Rack {
        Group,
        Ungroup,
        SoloChain,
    }
}

impl RliveApp {
    pub(super) fn rack_command(&mut self, c: Rack) {
        match c {
            Rack::Group => group_selected(
                &mut self.session,
                &mut self.ui.devices,
                &mut self.ui.status,
                false,
            ),
            Rack::Ungroup => ungroup_selected(&mut self.session, &mut self.ui.devices),
            Rack::SoloChain => self.toggle_solo_chain(),
        }
    }

    fn toggle_solo_chain(&mut self) {
        let targets = self.ui.devices.mixer_selection.clone();
        let session: &mut Session = &mut self.session;
        for t in targets {
            let solo = session
                .chain_param(t.at, t.rack, t.id, controls::SOLO)
                .unwrap_or(0.0)
                >= 0.5;
            session.set_chain_param(t.at, t.rack, t.id, controls::SOLO, (!solo) as u8 as f64);
        }
    }

    /// Starts a rename in the Device View if something there is selected. Returns whether one
    /// started.
    pub(super) fn rename_in_rack(&mut self) -> bool {
        if self.ui.focus != super::FocusArea::Device {
            return false;
        }
        let devices = &mut self.ui.devices;
        if let Some(KeyFocus::ChainList(rack)) = devices.ui.key_focus {
            let selection = devices.rack_views.get(&rack).map(|v| v.selection.clone());
            if let Some([id]) = selection.as_deref() {
                let id = *id;
                let name = self
                    .session
                    .chain_of_device(rack)
                    .and_then(|at| {
                        self.session
                            .devices(at)
                            .iter()
                            .find(|d| d.id == rack)
                            .and_then(|d| d.chains().find(|c| c.id == id).map(|c| c.name.clone()))
                    })
                    .unwrap_or_default();
                devices.ui.rename = Some(Rename {
                    target: RenameTarget::Chain(rack, id),
                    text: name,
                });
                return true;
            }
        }
        if let Some(&(chain, id)) = devices.selected_devices.first() {
            if let Some(d) = self.session.devices(chain).iter().find(|d| d.id == id) {
                devices.ui.rename = Some(Rename {
                    target: RenameTarget::Device(id),
                    text: d.name.clone(),
                });
                return true;
            }
        }
        false
    }
}
