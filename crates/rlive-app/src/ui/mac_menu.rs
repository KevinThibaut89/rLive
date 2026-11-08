//! The native macOS menu bar.
//!
//! Menu callbacks only queue a [`MenuCommand`]; the app runs it on its next
//! frame, because commands can open blocking dialogs and need the app's state.

use super::commands::{self, shortcut, Entry, MenuCommand, MenuState};
use egui::{Event, Key, Modifiers, RawInput};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSMenu,
    NSMenuItem, NSMenuItemValidation, NSPasteboard, NSPasteboardTypeString, NSView, NSWindow,
};
use objc2_foundation::NSString;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::RefCell;
use std::rc::Rc;

use MenuCommand as C;

struct MenuShared {
    queue: RefCell<Vec<MenuCommand>>,
    state: RefCell<MenuState>,
    ctx: egui::Context,
    main_window: Option<Retained<NSWindow>>,
}

impl MenuShared {
    /// The key window, when it's something other than the main rLive window
    /// (in practice, a plugin editor).
    fn foreign_key_window(&self, mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
        let key = NSApplication::sharedApplication(mtm).keyWindow()?;
        let main = self.main_window.as_ref()?;
        (Retained::as_ptr(&key) != Retained::as_ptr(main)).then_some(key)
    }
}

/// The standard responder-chain action for an Edit command.
fn responder_action(c: MenuCommand) -> Option<Sel> {
    Some(match c {
        C::Undo => sel!(undo:),
        C::Redo => sel!(redo:),
        C::Cut => sel!(cut:),
        C::Copy => sel!(copy:),
        C::Paste => sel!(paste:),
        C::SelectAll => sel!(selectAll:),
        _ => return None,
    })
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RliveMenuTarget"]
    #[ivars = Rc<MenuShared>]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(perform:))]
        fn perform(&self, item: &NSMenuItem) {
            let Some(c) = MenuCommand::ALL.get(item.tag() as usize).copied() else {
                return;
            };
            let shared = self.ivars();
            let mtm = self.mtm();
            // Edit commands belong to a focused plugin editor's own text views.
            if shared.foreign_key_window(mtm).is_some() {
                if let Some(action) = responder_action(c) {
                    let app = NSApplication::sharedApplication(mtm);
                    // SAFETY: a nil target sends a standard edit action up the
                    // responder chain; the sender is a live object.
                    unsafe { app.sendAction_to_from(action, None, Some(self)) };
                    return;
                }
            }
            shared.queue.borrow_mut().push(c);
            shared.ctx.request_repaint();
        }

        #[unsafe(method(closeKeyWindow:))]
        fn close_key_window(&self, _sender: Option<&AnyObject>) {
            if let Some(w) = self.ivars().foreign_key_window(self.mtm()) {
                w.performClose(None);
            }
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}

    unsafe impl NSMenuItemValidation for MenuTarget {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            self.validate(item)
        }
    }
);

impl MenuTarget {
    fn validate(&self, item: &NSMenuItem) -> bool {
        let shared = self.ivars();
        let foreign = shared.foreign_key_window(self.mtm()).is_some();
        if item.action() == Some(sel!(closeKeyWindow:)) {
            return foreign;
        }
        let Some(c) = MenuCommand::ALL.get(item.tag() as usize).copied() else {
            return true;
        };
        let state = shared.state.borrow();
        item.setState(if state.is_checked(c) {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        if foreign && responder_action(c).is_some() {
            // A plugin's editor decides what these do.
            if let Some(t) = commands::plain_title(c) {
                item.setTitle(&NSString::from_str(t));
            }
            return true;
        }
        if let Some(t) = state.title(c) {
            item.setTitle(&NSString::from_str(&t));
        }
        state.is_enabled(c)
    }

    fn new(mtm: MainThreadMarker, shared: Rc<MenuShared>) -> Retained<Self> {
        let this: Allocated<Self> = Self::alloc(mtm);
        let this = this.set_ivars(shared);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

pub struct MacMenu {
    shared: Rc<MenuShared>,
    _target: Retained<MenuTarget>,
    _menu: Retained<NSMenu>,
}

fn key_equivalent(key: Key) -> String {
    match key {
        Key::Comma => ",".into(),
        Key::Equals => "=".into(),
        Key::Minus => "-".into(),
        Key::Num0 => "0".into(),
        other => other.name().to_lowercase(),
    }
}

fn modifier_mask(m: Modifiers) -> NSEventModifierFlags {
    let mut mask = NSEventModifierFlags::empty();
    if m.command || m.mac_cmd {
        mask |= NSEventModifierFlags::Command;
    }
    if m.shift {
        mask |= NSEventModifierFlags::Shift;
    }
    if m.alt {
        mask |= NSEventModifierFlags::Option;
    }
    mask
}

struct Builder<'a> {
    mtm: MainThreadMarker,
    target: &'a MenuTarget,
}

impl Builder<'_> {
    fn raw_item(&self, title: &str, action: Sel, key: &str) -> Retained<NSMenuItem> {
        // SAFETY: the selector names a method implemented by the item's target
        // or found on the responder chain.
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        }
    }

    /// An item handled by rLive.
    fn command(&self, title: &str, c: MenuCommand) -> Retained<NSMenuItem> {
        let key = shortcut(c).map(|s| key_equivalent(s.logical_key));
        let item = self.raw_item(title, sel!(perform:), key.as_deref().unwrap_or(""));
        if let Some(s) = shortcut(c) {
            item.setKeyEquivalentModifierMask(modifier_mask(s.modifiers));
        }
        item.setTag(c as isize);
        // SAFETY: the target outlives the menu; MacMenu retains both.
        unsafe { item.setTarget(Some(self.target)) };
        item
    }

    /// An item sent to the first responder, such as the standard window actions.
    fn system(
        &self,
        menu: &NSMenu,
        title: &str,
        action: Sel,
        key: &str,
        mods: NSEventModifierFlags,
    ) {
        let item = self.raw_item(title, action, key);
        item.setKeyEquivalentModifierMask(mods);
        menu.addItem(&item);
    }

    fn separator(&self, menu: &NSMenu) {
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
    }

    fn entries(&self, menu: &NSMenu, list: &[Entry]) {
        for e in list {
            match *e {
                Entry::Separator => self.separator(menu),
                Entry::Item(title, c) => menu.addItem(&self.command(title, c)),
            }
        }
    }

    fn submenu(&self, bar: &NSMenu, title: &str) -> Retained<NSMenu> {
        let t = NSString::from_str(title);
        let menu = NSMenu::initWithTitle(NSMenu::alloc(self.mtm), &t);
        let holder = self.raw_item(title, sel!(submenuAction:), "");
        holder.setSubmenu(Some(&menu));
        bar.addItem(&holder);
        menu
    }
}

impl MacMenu {
    /// Replaces winit's default menu; call once the window exists.
    pub fn install(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let main_window = main_window(cc);
        let shared = Rc::new(MenuShared {
            queue: RefCell::new(Vec::new()),
            state: RefCell::new(MenuState::default()),
            ctx: cc.egui_ctx.clone(),
            main_window,
        });
        let target = MenuTarget::new(mtm, shared.clone());
        let b = Builder {
            mtm,
            target: &target,
        };
        let app = NSApplication::sharedApplication(mtm);
        let bar = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Main"));
        let cmd = NSEventModifierFlags::Command;
        let none = NSEventModifierFlags::empty();

        // The app menu must stay first: eframe retitles item 0.
        let rlive = b.submenu(&bar, "rLive");
        rlive.addItem(&b.command("About rLive", C::About));
        b.separator(&rlive);
        rlive.addItem(&b.command("Preferences…", C::Preferences));
        b.separator(&rlive);
        let services = b.submenu(&rlive, "Services");
        app.setServicesMenu(Some(&services));
        b.separator(&rlive);
        b.system(&rlive, "Hide rLive", sel!(hide:), "h", cmd);
        b.system(
            &rlive,
            "Hide Others",
            sel!(hideOtherApplications:),
            "h",
            cmd | NSEventModifierFlags::Option,
        );
        b.system(&rlive, "Show All", sel!(unhideAllApplications:), "", none);
        b.separator(&rlive);
        rlive.addItem(&b.command("Quit rLive", C::Quit));

        b.entries(&b.submenu(&bar, "File"), commands::FILE_MENU);
        b.entries(&b.submenu(&bar, "Edit"), commands::EDIT_MENU);
        b.entries(&b.submenu(&bar, "Create"), commands::CREATE_MENU);
        b.entries(&b.submenu(&bar, "View"), commands::VIEW_MENU);
        b.entries(&b.submenu(&bar, "Options"), commands::OPTIONS_MENU);

        let window = b.submenu(&bar, "Window");
        b.system(&window, "Minimize", sel!(performMiniaturize:), "m", cmd);
        b.system(&window, "Zoom", sel!(performZoom:), "", none);
        b.separator(&window);
        let close = b.raw_item("Close Plugin Window", sel!(closeKeyWindow:), "w");
        close.setKeyEquivalentModifierMask(cmd);
        // SAFETY: the target outlives the menu; MacMenu retains both.
        unsafe { close.setTarget(Some(&*target)) };
        window.addItem(&close);
        b.separator(&window);
        b.system(
            &window,
            "Bring All to Front",
            sel!(arrangeInFront:),
            "",
            none,
        );
        app.setWindowsMenu(Some(&window));

        let help = b.submenu(&bar, "Help");
        app.setHelpMenu(Some(&help));

        app.setMainMenu(Some(&bar));
        Some(Self {
            shared,
            _target: target,
            _menu: bar,
        })
    }

    /// Stores what the menus should show when AppKit next asks.
    pub fn publish(&self, state: MenuState) {
        *self.shared.state.borrow_mut() = state;
    }

    /// Takes the queued commands. Edit commands aimed at a focused text field
    /// are turned into egui events, since menu key equivalents never reach it.
    pub fn drain_into(
        &self,
        text_focused: bool,
        raw: &mut RawInput,
        commands: &mut Vec<MenuCommand>,
    ) {
        for c in self.shared.queue.borrow_mut().drain(..) {
            if !(text_focused && inject_text_edit(c, raw)) {
                commands.push(c);
            }
        }
    }
}

/// The window eframe created for the app.
fn main_window(cc: &eframe::CreationContext<'_>) -> Option<Retained<NSWindow>> {
    let RawWindowHandle::AppKit(handle) = cc.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: winit's handle points to the window's live content view.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    view.window()
}

fn pasteboard_text() -> Option<String> {
    let board = NSPasteboard::generalPasteboard();
    // SAFETY: NSPasteboardTypeString is a constant provided by AppKit.
    let text = board.stringForType(unsafe { NSPasteboardTypeString })?;
    Some(text.to_string())
}

fn inject_text_edit(c: MenuCommand, raw: &mut RawInput) -> bool {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::COMMAND
    };
    let (key, modifiers) = match c {
        C::Cut => {
            raw.events.push(Event::Cut);
            return true;
        }
        C::Copy => {
            raw.events.push(Event::Copy);
            return true;
        }
        C::Paste => {
            if let Some(text) = pasteboard_text() {
                raw.events.push(Event::Paste(text));
            }
            return true;
        }
        C::SelectAll => (Key::A, Modifiers::COMMAND),
        C::Undo => (Key::Z, Modifiers::COMMAND),
        C::Redo => (Key::Z, shift),
        _ => return false,
    };
    for pressed in [true, false] {
        raw.events.push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
    true
}
