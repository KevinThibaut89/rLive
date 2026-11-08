//! Commands that act on the Live Set as a file.

use super::{Entry, Entry::Item, Entry::Separator, MenuCommand as M};
use crate::ui::RliveApp;

super::domain! {
    /// The File menu.
    File { NewSet, Open, Save, SaveAs, ExportAudio }
}

pub const MENU: &[Entry] = &[
    Item("New Live Set", M::File(File::NewSet)),
    Item("Open…", M::File(File::Open)),
    Separator,
    Item("Save", M::File(File::Save)),
    Item("Save As…", M::File(File::SaveAs)),
    Separator,
    Item("Export Audio…", M::File(File::ExportAudio)),
];

impl RliveApp {
    pub(super) fn file_command(&mut self, c: File) {
        match c {
            File::NewSet => {
                self.session.new_project();
                self.ui.selected_clip = None;
            }
            File::Open => self.open_dialog(),
            File::Save => self.save(false),
            File::SaveAs => self.save(true),
            File::ExportAudio => self.ui.show_export = true,
        }
    }
}
