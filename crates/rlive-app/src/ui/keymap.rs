//! The focus-scoped keymap.
//!
//! Live's shortcuts change meaning with the part of the window that has focus, so every shortcut
//! is declared in [`bindings::BINDINGS`] with the area and state it applies to. A shortcut is
//! `Live` when its feature exists and `Pending(milestone)` when it doesn't: pending ones are
//! declared so the table stays complete against the manual, and do nothing until they are built.
//!
//! Letters are matched by the character they type, so Cmd+Z is the key labelled Z on any layout.
//! Digits fall back to the key's position, because on layouts like AZERTY the digit row types
//! symbols unless Shift is held, and Live's digit shortcuts still mean that row.

use super::commands::MenuCommand;
use egui::{Event, Key, KeyboardShortcut, Modifiers, RawInput};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

pub mod bindings;

pub use bindings::BINDINGS;

/// The operating system whose shortcut variants apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Platform {
    Mac,
    Other,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Other
        }
    }
}

/// The part of the window with keyboard focus, as named by the manual.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum FocusArea {
    ControlBar,
    #[default]
    Session,
    Arrangement,
    Clip,
    Device,
    Browser,
}

impl FocusArea {
    pub const ALL: [FocusArea; 6] = [
        Self::ControlBar,
        Self::Session,
        Self::Arrangement,
        Self::Clip,
        Self::Device,
        Self::Browser,
    ];
}

/// Which focus areas a binding applies to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Area {
    Global,
    ControlBar,
    Session,
    Arrangement,
    /// Session or Arrangement.
    Main,
    /// The Clip View, whatever its contents.
    Clip,
    ClipSample,
    ClipNotes,
    /// Arrangement or Clip View.
    Timeline,
    Device,
    Browser,
}

impl Area {
    pub fn contains(self, focus: FocusArea, clip_is_midi: bool) -> bool {
        match self {
            Self::Global => true,
            Self::ControlBar => focus == FocusArea::ControlBar,
            Self::Session => focus == FocusArea::Session,
            Self::Arrangement => focus == FocusArea::Arrangement,
            Self::Main => matches!(focus, FocusArea::Session | FocusArea::Arrangement),
            Self::Clip => focus == FocusArea::Clip,
            Self::ClipSample => focus == FocusArea::Clip && !clip_is_midi,
            Self::ClipNotes => focus == FocusArea::Clip && clip_is_midi,
            Self::Timeline => matches!(focus, FocusArea::Arrangement | FocusArea::Clip),
            Self::Device => focus == FocusArea::Device,
            Self::Browser => focus == FocusArea::Browser,
        }
    }

    /// How narrowly the area applies; narrower bindings win.
    fn specificity(self) -> u8 {
        match self {
            Self::Global => 0,
            Self::Main | Self::Timeline => 1,
            Self::Clip => 2,
            _ => 3,
        }
    }
}

/// A state a binding needs on top of its area. Until a milestone builds the state it is never
/// true, so its bindings stay out of the way of the ones that work today.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum When {
    Always,
    /// The Computer MIDI Keyboard is on.
    ComputerKeys,
    /// A window such as Settings is open.
    DialogOpen,
    /// "Use Tab to Move Focus" is on.
    TabFocus,
    /// A name is being edited.
    Renaming,
    /// A value control has keyboard focus.
    ValueControl,
    /// The loop brace or start/end markers are selected.
    LoopBrace,
    /// A Warp Marker or transient is selected.
    WarpMarker,
    /// A Group Track is selected.
    GroupTrack,
    /// A track title bar is highlighted.
    TrackTitle,
    /// A track header in the Arrangement has the keyboard.
    TrackHeader,
    /// Enter is held in the Arrangement, for resizing clips from the keyboard.
    EnterHeld,
    /// The arrow keys aren't navigating the automation lanes. Ranks like [`When::Always`], so
    /// a selection's arrow bindings still win.
    ArrowsFree,
    /// A Session slot is selected.
    SlotSelected,
    /// A chain is selected in a Rack.
    ChainSelected,
    /// There is a time selection in the Arrangement.
    TimeSelection,
    /// A take lane is selected.
    TakeLane,
    /// A Drum Rack, pad or Simpler is selected.
    SimilarSwap,
    /// The note editor's insert marker is set.
    NoteInsertMarker,
}

/// Modifier keys. `cmd` is Cmd on macOS and Ctrl elsewhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct Mods {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A key and the modifiers that make up a shortcut.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

/// How a binding is triggered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    /// The same chord everywhere.
    Keys(Chord),
    /// Different chords on macOS and elsewhere.
    Platform {
        mac: Chord,
        other: Chord,
    },
    MacOnly(Chord),
    /// A modifier held during a click or drag, described as the manual does. Handled by the
    /// widget, so it never reaches the resolver.
    Gesture {
        mac: &'static str,
        other: &'static str,
    },
}

impl Trigger {
    /// The chord on `platform`, or `None` for gestures and chords that platform doesn't have.
    pub fn chord(self, platform: Platform) -> Option<Chord> {
        match (self, platform) {
            (Self::Keys(c), _) => Some(c),
            (Self::Platform { mac, .. }, Platform::Mac) => Some(mac),
            (Self::Platform { other, .. }, Platform::Other) => Some(other),
            (Self::MacOnly(c), Platform::Mac) => Some(c),
            _ => None,
        }
    }
}

/// What a live binding runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exec {
    Command(MenuCommand),
    /// The widget under the pointer or in focus does it.
    Widget,
    /// The operating system's menu does it.
    Native,
}

/// Whether a binding works yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Live(Exec),
    /// Declared, and does nothing until this milestone. "out" means out of scope.
    Pending(&'static str),
}

/// One row of the keymap.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// The manual section the shortcut comes from, or "rlive" for one the manual doesn't list.
    pub section: &'static str,
    pub area: Area,
    pub when: When,
    pub label: &'static str,
    pub trigger: Trigger,
    pub status: Status,
}

/// What the keymap needs to know about the app to resolve a shortcut.
#[derive(Clone, Copy, Debug)]
pub struct KeyContext {
    pub platform: Platform,
    pub focus: FocusArea,
    pub clip_is_midi: bool,
    pub computer_keys: bool,
    /// A text field has keyboard focus.
    pub text_focused: bool,
    /// Any egui widget has keyboard focus.
    pub keyboard_taken: bool,
    /// A window such as Settings is open.
    pub dialog_open: bool,
    /// A Group Track is selected.
    pub group_selected: bool,
    /// A Warp Marker, transient or time selection is selected in the Sample tab.
    pub warp_selected: bool,
    /// A chain is selected in the Session View's rack mixer.
    pub chain_selected: bool,
    /// Clips or a span of time are selected in the Arrangement.
    pub time_selection: bool,
    /// The Arrangement's loop brace has the arrow keys.
    pub loop_brace: bool,
    /// A track header in the Arrangement has the keyboard.
    pub track_header: bool,
    /// Enter is held down.
    pub enter_held: bool,
    /// A Session slot is selected.
    pub slot_selected: bool,
    /// An automation lane has the arrow keys.
    pub lane_focused: bool,
}

impl KeyContext {
    fn holds(&self, when: When) -> bool {
        match when {
            When::Always => true,
            When::ComputerKeys => self.computer_keys,
            When::DialogOpen => self.dialog_open,
            When::GroupTrack => self.group_selected,
            When::WarpMarker => self.warp_selected,
            When::ChainSelected => self.chain_selected,
            When::TimeSelection => self.time_selection,
            When::LoopBrace => self.loop_brace,
            When::TrackHeader => self.track_header,
            When::EnterHeld => self.enter_held,
            When::ArrowsFree => !self.lane_focused,
            When::SlotSelected => self.slot_selected,
            // The rest are built by the milestones that need them.
            _ => false,
        }
    }
}

/// What a key press turned out to mean.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Run(MenuCommand),
    /// A note on the Computer MIDI Keyboard, played by the app's keyboard handler.
    MidiKeyboard,
    /// A declared shortcut whose feature doesn't exist yet.
    Pending,
    /// The native menu owns this chord, or nothing is bound to it.
    Unbound,
}

/// The note keys of the Computer MIDI Keyboard, lowest first.
pub const MIDI_NOTE_KEYS: [Key; 13] = [
    Key::A,
    Key::W,
    Key::S,
    Key::E,
    Key::D,
    Key::F,
    Key::T,
    Key::G,
    Key::Y,
    Key::H,
    Key::U,
    Key::J,
    Key::K,
];
const MIDI_CONTROL_KEYS: [Key; 4] = [Key::Z, Key::X, Key::C, Key::V];

fn is_midi_keyboard_key(key: Key) -> bool {
    MIDI_NOTE_KEYS.contains(&key) || MIDI_CONTROL_KEYS.contains(&key)
}

fn is_digit(key: Key) -> bool {
    matches!(
        key,
        Key::Num0
            | Key::Num1
            | Key::Num2
            | Key::Num3
            | Key::Num4
            | Key::Num5
            | Key::Num6
            | Key::Num7
            | Key::Num8
            | Key::Num9
    )
}

fn is_letter(key: Key) -> bool {
    let name = key.name();
    name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase()
}

/// Keys that are a typed symbol. Whether Shift is needed to type one depends on the layout, so a
/// binding on one of these never cares about Shift.
fn is_symbol(key: Key) -> bool {
    matches!(
        key,
        Key::Plus
            | Key::Equals
            | Key::Minus
            | Key::Comma
            | Key::Period
            | Key::Questionmark
            | Key::OpenBracket
            | Key::CloseBracket
    )
}

/// The chord a key event means, on this platform and keyboard layout.
pub fn chord_of(
    key: Key,
    physical: Option<Key>,
    modifiers: Modifiers,
    platform: Platform,
) -> Chord {
    let mut mods = Mods {
        cmd: match platform {
            Platform::Mac => modifiers.mac_cmd,
            Platform::Other => modifiers.ctrl,
        },
        ctrl: platform == Platform::Mac && modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
    };
    let key = match key {
        Key::Backspace => Key::Delete,
        k if is_digit(k) => {
            // The key typed its digit; on some layouts that takes Shift.
            if mods.shift && !mods.cmd && !mods.alt && physical == Some(k) {
                mods.shift = false;
            }
            k
        }
        // Layouts whose digit row types symbols unshifted still mean that row.
        k => match physical {
            Some(p) if is_digit(p) && !is_letter(k) => p,
            _ => k,
        },
    };
    if is_symbol(key) {
        mods.shift = false;
    }
    Chord { mods, key }
}

/// The form a declared chord is looked up in, matching what [`chord_of`] produces.
fn normalized(mut c: Chord) -> Chord {
    if is_symbol(c.key) {
        c.mods.shift = false;
    }
    c
}

/// Which bindings each chord can mean on each platform.
static BY_CHORD: LazyLock<HashMap<(Platform, Chord), Vec<&'static Binding>>> =
    LazyLock::new(|| {
        let mut map: HashMap<(Platform, Chord), Vec<&'static Binding>> = HashMap::new();
        for platform in [Platform::Mac, Platform::Other] {
            for b in BINDINGS {
                if let Some(c) = b.trigger.chord(platform) {
                    map.entry((platform, normalized(c))).or_default().push(b);
                }
            }
        }
        map
    });

/// Whether a chord still runs while a widget has the keyboard.
fn survives_typing(chord: Chord, text_focused: bool) -> bool {
    if !chord.mods.cmd && !chord.mods.ctrl {
        return false;
    }
    let text_edit = !chord.mods.alt
        && matches!(
            chord.key,
            Key::A | Key::C | Key::V | Key::X | Key::Z | Key::Y
        );
    !(text_focused && text_edit)
}

/// The binding a chord means right now: the narrowest one that applies.
fn lookup(chord: Chord, cx: &KeyContext) -> Option<&'static Binding> {
    let candidates = BY_CHORD.get(&(cx.platform, chord))?;
    candidates
        .iter()
        .filter(|b| b.area.contains(cx.focus, cx.clip_is_midi) && cx.holds(b.when))
        .max_by_key(|b| {
            let ranks_as_always = matches!(b.when, When::Always | When::ArrowsFree);
            (!ranks_as_always, b.area.specificity())
        })
        .copied()
}

fn outcome(b: &Binding) -> Resolution {
    match b.status {
        Status::Live(Exec::Command(c)) => Resolution::Run(c),
        Status::Live(_) => Resolution::Unbound,
        Status::Pending(_) => Resolution::Pending,
    }
}

/// Resolves a key press: what the Computer MIDI Keyboard takes first, then the focused area,
/// then what applies everywhere.
pub fn resolve(chord: Chord, cx: &KeyContext) -> Resolution {
    if cx.keyboard_taken && !survives_typing(chord, cx.text_focused) {
        return Resolution::Unbound;
    }
    let m = chord.mods;
    if cx.computer_keys && is_midi_keyboard_key(chord.key) && !m.cmd && !m.alt && !m.ctrl {
        if m.shift {
            // Shift reaches the single-letter shortcut the keyboard has taken over.
            let mut bare = chord;
            bare.mods.shift = false;
            let without_keyboard = KeyContext {
                computer_keys: false,
                ..*cx
            };
            return lookup(bare, &without_keyboard).map_or(Resolution::Unbound, outcome);
        }
        if MIDI_NOTE_KEYS.contains(&chord.key) {
            return Resolution::MidiKeyboard;
        }
    }
    if cx.platform == Platform::Mac && NATIVE_CHORDS.contains(&chord) {
        return Resolution::Unbound;
    }
    lookup(chord, cx).map_or(Resolution::Unbound, outcome)
}

/// Every command that has a menu item.
fn menu_commands() -> Vec<MenuCommand> {
    use super::commands::{app, create, edit, file, options, view, Entry};
    let menus: [&[Entry]; 6] = [
        file::MENU,
        edit::MENU,
        create::MENU,
        view::MENU,
        options::MENU,
        &[app::ABOUT, app::PREFERENCES, app::QUIT],
    ];
    menus
        .iter()
        .flat_map(|m| m.iter())
        .filter_map(|e| match *e {
            Entry::Item(_, c) => Some(c),
            Entry::Separator => None,
        })
        .collect()
}

/// A menu item's shortcut: its first global chord that uses Cmd. Bare keys like Tab and Delete
/// never become key equivalents, as they must keep working in text fields and plugin windows.
fn menu_chord(c: MenuCommand, platform: Platform) -> Option<Chord> {
    BINDINGS
        .iter()
        .filter(|b| b.area == Area::Global && b.status == Status::Live(Exec::Command(c)))
        .filter_map(|b| b.trigger.chord(platform))
        .find(|chord| chord.mods.cmd)
}

/// Chords a native menu item handles. The keymap leaves them alone, so each fires only once.
static NATIVE_CHORDS: LazyLock<HashSet<Chord>> = LazyLock::new(|| {
    menu_commands()
        .into_iter()
        .filter_map(|c| menu_chord(c, Platform::Mac))
        .map(normalized)
        .collect()
});

/// The shortcut shown on a menu item and bound to it natively.
pub fn menu_shortcut(c: MenuCommand) -> Option<KeyboardShortcut> {
    let chord = menu_chord(c, Platform::current())?;
    let mut m = Modifiers::COMMAND;
    m.alt = chord.mods.alt;
    m.shift = chord.mods.shift;
    Some(KeyboardShortcut::new(m, chord.key))
}

/// Holding this ignores the grid while dragging.
pub fn ignores_grid(m: Modifiers, platform: Platform) -> bool {
    match platform {
        Platform::Mac => m.mac_cmd,
        Platform::Other => m.alt,
    }
}

/// Holding this copies what is being dragged instead of moving it.
pub fn copies_on_drag(m: Modifiers, platform: Platform) -> bool {
    match platform {
        Platform::Mac => m.alt,
        Platform::Other => m.ctrl,
    }
}

/// Keys whose presses the keymap took, so their repeats and releases are taken too.
#[derive(Default)]
pub struct KeyState {
    down: HashSet<Key>,
}

impl KeyState {
    /// Takes every working shortcut out of the frame's input before egui sees it, queueing its
    /// command. Everything else, including Computer MIDI Keyboard notes, is left in place.
    pub fn process(
        &mut self,
        raw: &mut RawInput,
        cx: &KeyContext,
        commands: &mut Vec<MenuCommand>,
    ) {
        let down = &mut self.down;
        raw.events.retain(|e| {
            if matches!(e, Event::WindowFocused(false)) {
                down.clear();
            }
            let Event::Key {
                key,
                physical_key,
                pressed,
                modifiers,
                ..
            } = e
            else {
                return true;
            };
            if !*pressed {
                return !down.remove(key);
            }
            if down.contains(key) {
                return false;
            }
            let chord = chord_of(*key, *physical_key, *modifiers, cx.platform);
            if let Resolution::Run(c) = resolve(chord, cx) {
                commands.push(c);
                down.insert(*key);
                return false;
            }
            true
        });
    }
}

/// A chord as the manual writes it.
pub fn chord_text(chord: Chord, platform: Platform) -> String {
    let m = chord.mods;
    let mut parts: Vec<&str> = Vec::new();
    if m.ctrl {
        parts.push("Ctrl");
    }
    if m.cmd {
        parts.push(match platform {
            Platform::Mac => "Cmd",
            Platform::Other => "Ctrl",
        });
    }
    if m.alt {
        parts.push("Alt");
    }
    if m.shift {
        parts.push("Shift");
    }
    let key = match chord.key {
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        Key::ArrowLeft => "Left",
        Key::ArrowRight => "Right",
        Key::Escape => "Esc",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::Plus => "+",
        Key::Minus => "-",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Questionmark => "?",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Equals => "=",
        k => k.name(),
    };
    parts.push(key);
    parts.join("+")
}

#[cfg(test)]
mod tests;
