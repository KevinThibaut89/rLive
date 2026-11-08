//! Application commands, shared by the native macOS menu bar, the egui menu used on other
//! platforms, and keyboard shortcuts.
//!
//! Each domain declares its commands in its own file with [`domain!`], and handles them there
//! too. To add a domain, add it to the `registry!` list below and to [`RliveApp::dispatch`].

use super::{keymap, RliveApp};
use std::collections::HashMap;
use std::sync::LazyLock;

pub mod app;
pub mod create;
pub mod edit;
pub mod file;
pub mod options;
pub mod track;
pub mod transport;
pub mod view;

pub use super::keymap::FocusArea;
pub use edit::{ClipBoard, EditFocus};

/// Declares one domain's commands and the list of them the registry reads.
macro_rules! domain {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub enum $name { $($variant),* }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];
        }
    };
}
use domain;

/// Collects the domains into [`MenuCommand`] and the list of every command.
macro_rules! registry {
    ($($variant:ident($ty:path)),* $(,)?) => {
        /// A command of any domain.
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub enum MenuCommand { $($variant($ty)),* }

        $(impl From<$ty> for MenuCommand {
            fn from(c: $ty) -> Self { Self::$variant(c) }
        })*

        fn build_all() -> Vec<MenuCommand> {
            let mut all = Vec::new();
            $(all.extend(<$ty>::ALL.iter().map(|c| MenuCommand::$variant(*c)));)*
            all
        }
    };
}

registry! {
    App(app::App),
    File(file::File),
    Edit(edit::Edit),
    Create(create::Create),
    View(view::View),
    Options(options::Options),
    Transport(transport::Transport),
    Track(track::Track),
}

static ALL: LazyLock<Vec<MenuCommand>> = LazyLock::new(build_all);
static INDEX: LazyLock<HashMap<MenuCommand, usize>> =
    LazyLock::new(|| ALL.iter().enumerate().map(|(i, c)| (*c, i)).collect());

impl MenuCommand {
    /// Every command, in a stable order: `all()[c.index()] == c`.
    pub fn all() -> &'static [MenuCommand] {
        &ALL
    }

    pub fn index(self) -> usize {
        INDEX[&self]
    }
}

/// One row of a menu.
#[derive(Clone, Copy)]
pub enum Entry {
    Item(&'static str, MenuCommand),
    Separator,
}

use Entry::{Item, Separator};

/// What the menus should show right now.
#[derive(Clone, PartialEq)]
pub struct MenuState {
    enabled: Vec<bool>,
    checked: Vec<bool>,
    pub undo_label: Option<&'static str>,
    pub redo_label: Option<&'static str>,
}

impl Default for MenuState {
    fn default() -> Self {
        let n = MenuCommand::all().len();
        Self {
            enabled: vec![true; n],
            checked: vec![false; n],
            undo_label: None,
            redo_label: None,
        }
    }
}

impl MenuState {
    pub fn is_enabled(&self, c: MenuCommand) -> bool {
        self.enabled[c.index()]
    }

    pub fn is_checked(&self, c: MenuCommand) -> bool {
        self.checked[c.index()]
    }

    pub(super) fn enable(&mut self, c: MenuCommand, on: bool) {
        self.enabled[c.index()] = on;
    }

    pub(super) fn check(&mut self, c: MenuCommand, on: bool) {
        self.checked[c.index()] = on;
    }

    /// The title an item should show, for items whose title changes.
    pub fn title(&self, c: MenuCommand) -> Option<String> {
        match c {
            MenuCommand::Edit(edit::Edit::Undo) => Some(match self.undo_label {
                Some(l) => format!("Undo {l}"),
                None => "Undo".to_string(),
            }),
            MenuCommand::Edit(edit::Edit::Redo) => Some(match self.redo_label {
                Some(l) => format!("Redo {l}"),
                None => "Redo".to_string(),
            }),
            _ => None,
        }
    }
}

impl RliveApp {
    pub(super) fn menu_state(&self, ctx: &egui::Context) -> MenuState {
        let mut s = MenuState::default();
        self.edit_state(ctx, &mut s);
        self.create_state(&mut s);
        self.view_state(&mut s);
        self.options_state(&mut s);
        s
    }

    pub(super) fn dispatch(&mut self, ctx: &egui::Context, c: MenuCommand) {
        match c {
            MenuCommand::App(c) => self.app_command(ctx, c),
            MenuCommand::File(c) => self.file_command(c),
            MenuCommand::Edit(c) => self.edit_dispatch(ctx, c),
            MenuCommand::Create(c) => self.create_command(c),
            MenuCommand::View(c) => self.view_command(ctx, c),
            MenuCommand::Options(c) => self.options_command(c),
            MenuCommand::Transport(c) => self.transport_command(c),
            MenuCommand::Track(c) => self.track_command(c),
        }
    }

    /// The egui menu bar, for platforms without a native one.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        fn entries(
            ui: &mut egui::Ui,
            list: &[Entry],
            st: &MenuState,
            fired: &mut Option<MenuCommand>,
        ) {
            for e in list {
                match *e {
                    Separator => {
                        ui.separator();
                    }
                    Item(label, c) => {
                        let title = st.title(c).unwrap_or_else(|| label.to_string());
                        let mut b = egui::Button::new(title).selected(st.is_checked(c));
                        if let Some(s) = keymap::menu_shortcut(c) {
                            b = b.shortcut_text(ui.ctx().format_shortcut(&s));
                        }
                        if ui.add_enabled(st.is_enabled(c), b).clicked() {
                            *fired = Some(c);
                            ui.close();
                        }
                    }
                }
            }
        }
        let st = self.menu_state(ui.ctx());
        let mut fired = None;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                entries(ui, file::MENU, &st, &mut fired);
                ui.separator();
                entries(ui, &[app::QUIT], &st, &mut fired);
            });
            ui.menu_button("Edit", |ui| entries(ui, edit::MENU, &st, &mut fired));
            ui.menu_button("Create", |ui| entries(ui, create::MENU, &st, &mut fired));
            ui.menu_button("View", |ui| entries(ui, view::MENU, &st, &mut fired));
            ui.menu_button("Options", |ui| {
                entries(ui, &[app::PREFERENCES], &st, &mut fired);
                entries(ui, options::MENU, &st, &mut fired);
            });
            ui.menu_button("Help", |ui| {
                entries(ui, &[app::ABOUT], &st, &mut fired);
            });
        });
        if let Some(c) = fired {
            self.dispatch(&ui.ctx().clone(), c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_indexed_in_order() {
        for (i, c) in MenuCommand::all().iter().enumerate() {
            assert_eq!(c.index(), i, "{c:?} is out of order in all()");
        }
    }

    #[test]
    fn every_domain_reaches_the_registry() {
        let expected = app::App::ALL.len()
            + file::File::ALL.len()
            + edit::Edit::ALL.len()
            + create::Create::ALL.len()
            + view::View::ALL.len()
            + options::Options::ALL.len()
            + transport::Transport::ALL.len()
            + track::Track::ALL.len();
        assert_eq!(MenuCommand::all().len(), expected);
        let unique: std::collections::HashSet<_> = MenuCommand::all().iter().collect();
        assert_eq!(unique.len(), expected, "a command appears twice");
    }
}
