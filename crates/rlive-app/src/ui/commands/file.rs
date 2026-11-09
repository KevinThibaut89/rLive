//! Commands that act on the Live Set as a file.

use super::{Entry, Entry::Item, Entry::Separator, MenuCommand as M, MenuState};
use crate::session::{CollectOptions, ManagedScope};
use crate::ui::{FileAction, RliveApp};
use rlive_browser::scan::is_set_file;

super::domain! {
    /// The File menu.
    File {
        NewSet, Open, OpenRecent, Save, SaveAs, SaveCopy, SaveAsTemplate, SaveAsDefault,
        CollectAllAndSave, ManageFiles, InstallPack, ExportAudio, ExportMidi,
    }
}

pub const MENU: &[Entry] = &[
    Item("New Live Set", M::File(File::NewSet)),
    Item("Open…", M::File(File::Open)),
    Item("Open Recent…", M::File(File::OpenRecent)),
    Separator,
    Item("Save", M::File(File::Save)),
    Item("Save As…", M::File(File::SaveAs)),
    Item("Save a Copy…", M::File(File::SaveCopy)),
    Item("Collect All and Save", M::File(File::CollectAllAndSave)),
    Item("Manage Files…", M::File(File::ManageFiles)),
    Separator,
    Item("Save Live Set As Template…", M::File(File::SaveAsTemplate)),
    Item(
        "Save Live Set As Default Set…",
        M::File(File::SaveAsDefault),
    ),
    Separator,
    Item("Install Pack…", M::File(File::InstallPack)),
    Separator,
    Item("Export Audio/Video…", M::File(File::ExportAudio)),
    Item("Export MIDI Clip…", M::File(File::ExportMidi)),
];

impl RliveApp {
    pub(in crate::ui) fn file_state(&self, s: &mut MenuState) {
        let midi_clip = self
            .ui
            .selected_clip
            .and_then(|c| self.session.clip_at(c))
            .is_some_and(|c| c.is_midi());
        s.enable(M::File(File::ExportMidi), midi_clip);
    }

    pub(super) fn file_command(&mut self, c: File) {
        match c {
            File::NewSet => self.request_file_action(FileAction::NewSet),
            File::Open => self.open_dialog(),
            File::OpenRecent => self.ui.files.show_recent = true,
            File::Save => self.save(false),
            File::SaveAs => self.save(true),
            File::SaveCopy => self.save_copy(),
            File::SaveAsTemplate => {
                self.ui.files.template_name = Some(String::from("Template"));
            }
            File::SaveAsDefault => self.ui.files.confirm_default = true,
            File::CollectAllAndSave => {
                self.ui.files.collect_prompt = Some(CollectOptions::default())
            }
            File::ManageFiles => self.ui.files.manager.show_scope(ManagedScope::Set),
            File::InstallPack => self.install_pack_dialog(),
            File::ExportAudio => self.open_export_dialog(),
            File::ExportMidi => self.export_midi_dialog(),
        }
    }

    fn export_midi_dialog(&mut self) {
        let Some(r) = self.ui.selected_clip else {
            self.ui.status = "Select a MIDI clip first".into();
            return;
        };
        let name = self
            .session
            .clip_at(r)
            .map_or_else(String::new, |c| c.name.clone());
        let Some(path) = rfd::FileDialog::new()
            .add_filter("MIDI File", &["mid"])
            .set_file_name(format!("{name}.mid"))
            .save_file()
        else {
            return;
        };
        self.ui.status = match self.session.export_midi_clip(r, &path) {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => e,
        };
    }

    fn install_pack_dialog(&mut self) {
        let Some(pack) = rfd::FileDialog::new()
            .add_filter("rLive Pack", &[rlive_browser::files::PACK_EXTENSION])
            .pick_file()
        else {
            return;
        };
        self.install_pack_from(&pack);
    }

    /// Asks where a Pack goes, unpacks it, and opens its Set if it has one.
    pub(in crate::ui) fn install_pack_from(&mut self, pack: &std::path::Path) {
        let Some(parent) = rfd::FileDialog::new()
            .set_title("Install Pack into")
            .pick_folder()
        else {
            return;
        };
        match self.session.install_pack(pack, &parent) {
            Ok(folder) => {
                let set = walk_for_set(&folder);
                self.ui.status = format!("Installed {}", folder.display());
                if let Some(set) = set {
                    self.request_file_action(FileAction::Open(set));
                }
            }
            Err(e) => self.ui.status = format!("Could not install the Pack: {e}"),
        }
    }
}

fn walk_for_set(folder: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(folder).ok()?;
    let mut sets: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_set_file(p))
        .collect();
    sets.sort();
    sets.into_iter().next()
}
