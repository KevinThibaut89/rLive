//! Every shortcut in the manual's keyboard chapter, one row each.
//!
//! Rows follow the manual's section order. A row whose feature doesn't exist yet is
//! `Pending("<milestone>")` and does nothing; flip it to `L(...)` when the milestone builds it.

use super::super::commands::app::App;
use super::super::commands::arrange::Arrange as Ar;
use super::super::commands::bounce::Bounce;
use super::super::commands::clip::Clip as Cv;
use super::super::commands::create::Create;
use super::super::commands::edit::Edit;
use super::super::commands::file::File;
use super::super::commands::launch::Launch as La;
use super::super::commands::notes::Notes as Nt;
use super::super::commands::options::Options;
use super::super::commands::rack::Rack;
use super::super::commands::remote::Remote;
use super::super::commands::track::Track;
use super::super::commands::transport::Transport as Tr;
use super::super::commands::view::View;
use super::super::commands::MenuCommand as M;
use super::{Area, Binding, Chord, Exec, Mods, Status, Trigger, When};
use egui::Key;

const NO: Mods = Mods {
    cmd: false,
    ctrl: false,
    alt: false,
    shift: false,
};
const CMD: Mods = Mods { cmd: true, ..NO };
const ALT: Mods = Mods { alt: true, ..NO };
const SHIFT: Mods = Mods { shift: true, ..NO };
const CMD_SHIFT: Mods = Mods {
    cmd: true,
    shift: true,
    ..NO
};
const CMD_ALT: Mods = Mods {
    cmd: true,
    alt: true,
    ..NO
};
const ALT_SHIFT: Mods = Mods {
    alt: true,
    shift: true,
    ..NO
};
const CMD_ALT_SHIFT: Mods = Mods {
    cmd: true,
    alt: true,
    shift: true,
    ctrl: false,
};
const CTRL_CMD: Mods = Mods {
    ctrl: true,
    cmd: true,
    ..NO
};

const fn ch(mods: Mods, key: Key) -> Chord {
    Chord { mods, key }
}

const fn k(mods: Mods, key: Key) -> Trigger {
    Trigger::Keys(ch(mods, key))
}

/// A chord that differs by platform, macOS first.
const fn pl(mac: Chord, other: Chord) -> Trigger {
    Trigger::Platform { mac, other }
}

/// A modifier held during a click or drag.
const fn g(mac: &'static str, other: &'static str) -> Trigger {
    Trigger::Gesture { mac, other }
}

/// One gesture, written the same on every platform.
const fn g1(both: &'static str) -> Trigger {
    g(both, both)
}

/// Live: runs a command.
#[allow(non_snake_case)]
const fn L(c: M) -> Status {
    Status::Live(Exec::Command(c))
}

/// Live: handled by a widget or gesture.
const W: Status = Status::Live(Exec::Widget);

const fn b(
    section: &'static str,
    area: Area,
    when: When,
    label: &'static str,
    trigger: Trigger,
    status: Status,
) -> Binding {
    Binding {
        section,
        area,
        when,
        label,
        trigger,
        status,
    }
}

const fn a(
    section: &'static str,
    area: Area,
    label: &'static str,
    trigger: Trigger,
    status: Status,
) -> Binding {
    b(section, area, When::Always, label, trigger, status)
}

use Area::{
    Arrangement as Arr, Browser as Brw, Clip, ClipNotes as Notes, ClipSample as Sample,
    Device as Dev, Global as All, Main, Session as Ses, Timeline as Time,
};
use When as Wh;

macro_rules! p {
    ($m:literal) => {
        Status::Pending($m)
    };
}

#[rustfmt::skip]
pub static BINDINGS: &[Binding] = &[
    // 42.1 Showing and hiding views
    a("42.1", All, "Toggle full screen", pl(ch(CTRL_CMD, Key::F), ch(NO, Key::F11)), L(M::View(View::ToggleFullScreen))),
    a("42.1", All, "Toggle second window", k(CMD_SHIFT, Key::W), p!("C17")),
    a("42.1", All, "Toggle Session/Arrangement", k(NO, Key::Tab), L(M::View(View::ToggleMainView))),
    a("42.1", All, "Toggle Device/Clip View", k(SHIFT, Key::Tab), L(M::View(View::SwitchDetail))),
    a("42.1", All, "Toggle Device/Clip View", k(NO, Key::F12), L(M::View(View::SwitchDetail))),
    a("42.1", All, "Show Device and Clip View together", g1("Alt click view toggle"), W),
    a("42.1", All, "Toggle Hot-Swap Mode", k(NO, Key::Q), p!("C12")),
    a("42.1", Dev, "Toggle Drum Rack/last selected pad", k(NO, Key::D), p!("C12")),
    a("42.1", All, "Hide/show Info View", k(NO, Key::Questionmark), L(M::View(View::ToggleInfoView))),
    a("42.1", All, "Hide/show video window", k(CMD_ALT, Key::V), p!("C18")),
    a("42.1", All, "Hide/show Browser", k(CMD_ALT, Key::B), L(M::View(View::ToggleBrowser))),
    a("42.1", All, "Hide/show Browser", k(CMD_ALT, Key::Num5), L(M::View(View::ToggleBrowser))),
    a("42.1", All, "Hide/show Overview", k(CMD_ALT, Key::O), L(M::Arrange(Ar::ToggleOverview))),
    a("42.1", All, "Hide/show In/Out", k(CMD_ALT, Key::I), L(M::View(View::ToggleInOut))),
    a("42.1", All, "Hide/show Sends", k(CMD_ALT, Key::S), p!("C17")),
    a("42.1", All, "Hide/show Mixer", k(CMD_ALT, Key::M), L(M::View(View::ToggleMixer))),
    a("42.1", All, "Hide/show Clip View", k(CMD_ALT, Key::Num3), L(M::View(View::ToggleClipView))),
    a("42.1", All, "Hide/show Device View", k(CMD_ALT, Key::Num4), L(M::View(View::ToggleDeviceView))),
    a("42.1", All, "Hide/show Groove Pool", k(CMD_ALT, Key::Num6), L(M::View(View::ToggleGroovePool))),
    a("42.1", All, "Hide/show Learn View", k(CMD_ALT, Key::Num7), p!("out")),
    a("42.1", All, "Open Settings", k(CMD, Key::Comma), L(M::App(App::Preferences))),
    b("42.1", All, Wh::DialogOpen, "Close window or dialog", k(NO, Key::Escape), L(M::View(View::CloseDialog))),

    // 42.2 Keyboard focus and navigation
    a("42.2", All, "Focus Control Bar", k(ALT, Key::Num0), L(M::View(View::FocusControlBar))),
    a("42.2", All, "Focus Session View", k(ALT, Key::Num1), L(M::View(View::FocusSession))),
    a("42.2", All, "Focus Arrangement View", k(ALT, Key::Num2), L(M::View(View::FocusArrangement))),
    a("42.2", All, "Focus Clip View", k(ALT, Key::Num3), L(M::View(View::FocusClip))),
    a("42.2", All, "Focus Device View", k(ALT, Key::Num4), L(M::View(View::FocusDevice))),
    a("42.2", All, "Focus Browser", k(ALT, Key::Num5), L(M::View(View::FocusBrowser))),
    a("42.2", All, "Focus Groove Pool", k(ALT, Key::Num6), L(M::View(View::FocusGroovePool))),
    a("42.2", All, "Focus Learn View", k(ALT, Key::Num7), p!("out")),
    a("42.2", All, "Focus selected clip panel", k(ALT, Key::Num8), L(M::Clip(Cv::FocusPanels))),
    a("42.2", All, "Focus clip panels", k(ALT_SHIFT, Key::P), L(M::Clip(Cv::FocusPanels))),
    b("42.2", All, Wh::TabFocus, "Next control", k(NO, Key::Tab), p!("C19")),
    b("42.2", All, Wh::TabFocus, "Previous control", k(SHIFT, Key::Tab), p!("C19")),
    b("42.2", All, Wh::TabFocus, "Next neighbouring control", pl(ch(ALT, Key::Tab), ch(CMD, Key::Tab)), p!("C19")),
    b("42.2", All, Wh::TabFocus, "Previous neighbouring control", pl(ch(ALT_SHIFT, Key::Tab), ch(CMD_SHIFT, Key::Tab)), p!("C19")),

    // 42.3 Sets and the program
    a("42.3", All, "New Live Set", k(CMD, Key::N), L(M::File(File::NewSet))),
    a("42.3", All, "Open Live Set", k(CMD, Key::O), L(M::File(File::Open))),
    a("42.3", All, "Save Live Set", k(CMD, Key::S), L(M::File(File::Save))),
    a("42.3", All, "Save Live Set As", k(CMD_SHIFT, Key::S), L(M::File(File::SaveAs))),
    a("42.3", All, "Quit", k(CMD, Key::Q), L(M::App(App::Quit))),
    a("42.3", All, "Hide", Trigger::MacOnly(ch(CMD, Key::H)), Status::Live(Exec::Native)),
    a("42.3", All, "Export Audio/Video", k(CMD_SHIFT, Key::R), L(M::File(File::ExportAudio))),
    a("42.3", All, "Export MIDI File", k(CMD_SHIFT, Key::E), p!("C14")),

    // 42.4 Devices and plug-ins
    a("42.4", Dev, "Group devices", k(CMD, Key::G), L(M::Rack(Rack::Group))),
    a("42.4", Dev, "Ungroup devices", k(CMD_SHIFT, Key::G), L(M::Rack(Rack::Ungroup))),
    a("42.4", Dev, "All devices in a group on/off", g1("Hold Alt and click on device activator"), W),
    a("42.4", Dev, "Add device to selection", g1("Shift click"), W),
    a("42.4", Dev, "List a track's devices", g1("right-click on Device View selector"), W),
    a("42.4", Dev, "Reset parameter", g1("Delete or double-click"), W),
    a("42.4", Brw, "Load selected device", k(NO, Key::Enter), p!("C12")),
    a("42.4", Dev, "Hot-swap selected device", k(NO, Key::Q), p!("C12")),
    a("42.4", All, "Hide/show plug-in windows", k(CMD_ALT, Key::P), p!("A17")),
    a("42.4", Dev, "Keep several plug-in windows open", g("Cmd", "Ctrl"), p!("A17")),
    a("42.4", Dev, "Compare A/B", k(NO, Key::P), p!("C12")),

    // 42.5 Editing
    a("42.5", All, "Cut", k(CMD, Key::X), L(M::Edit(Edit::Cut))),
    a("42.5", All, "Copy", k(CMD, Key::C), L(M::Edit(Edit::Copy))),
    a("42.5", All, "Paste", k(CMD, Key::V), L(M::Edit(Edit::Paste))),
    a("42.5", All, "Duplicate", k(CMD, Key::D), L(M::Edit(Edit::Duplicate))),
    a("42.5", All, "Delete", k(NO, Key::Delete), L(M::Edit(Edit::Delete))),
    a("42.5", All, "Undo", k(CMD, Key::Z), L(M::Edit(Edit::Undo))),
    a("42.5", All, "Redo", pl(ch(CMD_SHIFT, Key::Z), ch(CMD, Key::Y)), L(M::Edit(Edit::Redo))),
    a("42.5", All, "Rename", k(CMD, Key::R), L(M::Edit(Edit::Rename))),
    a("42.5", All, "Select all", k(CMD, Key::A), L(M::Edit(Edit::SelectAll))),
    a("42.5", All, "Select several items", g("Cmd click", "Ctrl click"), W),
    a("42.5", All, "Select a range of items", g1("Shift click"), W),
    b("42.5", All, Wh::Renaming, "Next name while renaming", k(NO, Key::Tab), p!("C19")),
    a("42.5", All, "Ignore grid while dragging", g("Cmd", "Alt"), W),
    a("42.5", All, "Edit clips and slots across all tracks", g1("Shift"), W),
    a("42.5", All, "Edit time across all tracks", g1("Shift"), W),
    a("42.5", All, "Edit the selected part of an envelope", g1("Alt"), W),

    // 42.6 Adjusting values
    b("42.6", All, Wh::ValueControl, "Decrement value", k(NO, Key::ArrowDown), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Increment value", k(NO, Key::ArrowUp), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Value down by an octave or fine step", k(SHIFT, Key::ArrowDown), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Value up by an octave or fine step", k(SHIFT, Key::ArrowUp), p!("C19")),
    a("42.6", All, "Finer resolution while dragging", g1("Shift"), W),
    b("42.6", All, Wh::ValueControl, "Return to default", k(NO, Key::Delete), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num0), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num1), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num2), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num3), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num4), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num5), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num6), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num7), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num8), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Type a value", k(NO, Key::Num9), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Next field", k(NO, Key::Period), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Next field", k(NO, Key::Comma), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Cancel value entry", k(NO, Key::Escape), p!("C19")),
    b("42.6", All, Wh::ValueControl, "Confirm value entry", k(NO, Key::Enter), p!("C19")),

    // 42.7 Breakpoint envelopes
    a("42.7", Arr, "Toggle Automation Mode", k(NO, Key::A), L(M::View(View::ToggleAutomationMode))),
    a("42.7", Time, "Finer envelope resolution", g1("Shift"), W),
    a("42.7", Time, "Curve an automation segment", g1("Alt"), W),
    a("42.7", Time, "Toggle fade controls for a moment", k(NO, Key::F), W),
    a("42.7", Time, "Delete the selected envelope", k(CMD, Key::Delete), L(M::Edit(Edit::DeleteEnvelope))),
    a("42.7", Time, "Ignore grid while dragging breakpoints", g("Cmd", "Alt"), W),

    // 42.8 Loop brace and start/end markers
    a("42.8", Time, "Set start marker", k(CMD, Key::F9), L(M::Clip(Cv::SetStart))),
    a("42.8", Time, "Set loop start", k(CMD, Key::F10), L(M::Clip(Cv::SetLoopStart))),
    a("42.8", Time, "Set loop end", k(CMD, Key::F11), L(M::Clip(Cv::SetLoopEnd))),
    a("42.8", Time, "Set end marker", k(CMD, Key::F12), L(M::Clip(Cv::SetEnd))),
    b("42.8", Time, Wh::LoopBrace, "Move start marker to position", g("Cmd click", "Ctrl click"), W),
    b("42.8", Time, Wh::LoopBrace, "Move end marker to position", g("Cmd Shift click", "Ctrl Shift click"), W),
    b("42.8", Time, Wh::LoopBrace, "Nudge loop brace left", k(NO, Key::ArrowLeft), L(M::Arrange(Ar::LoopNudgeLeft))),
    b("42.8", Time, Wh::LoopBrace, "Nudge loop brace right", k(NO, Key::ArrowRight), L(M::Arrange(Ar::LoopNudgeRight))),
    b("42.8", Time, Wh::LoopBrace, "Move loop by its length, up", k(NO, Key::ArrowUp), L(M::Arrange(Ar::LoopShiftUp))),
    b("42.8", Time, Wh::LoopBrace, "Move loop by its length, down", k(NO, Key::ArrowDown), L(M::Arrange(Ar::LoopShiftDown))),
    b("42.8", Time, Wh::LoopBrace, "Double loop length", k(CMD, Key::ArrowUp), L(M::Arrange(Ar::LoopDouble))),
    b("42.8", Time, Wh::LoopBrace, "Halve loop length", k(CMD, Key::ArrowDown), L(M::Arrange(Ar::LoopHalve))),
    b("42.8", Time, Wh::LoopBrace, "Shorten loop", k(CMD, Key::ArrowLeft), L(M::Arrange(Ar::LoopShorten))),
    b("42.8", Time, Wh::LoopBrace, "Lengthen loop", k(CMD, Key::ArrowRight), L(M::Arrange(Ar::LoopLengthen))),
    a("42.8", Time, "Select material in loop", k(CMD_SHIFT, Key::L), L(M::Arrange(Ar::SelectLoop))),

    // 42.9 Zooming, display and selections
    a("42.9", All, "Zoom window in", k(CMD, Key::Plus), L(M::View(View::ZoomIn))),
    a("42.9", All, "Zoom window out", k(CMD, Key::Minus), L(M::View(View::ZoomOut))),
    a("42.9", Arr, "Zoom time ruler in", k(NO, Key::Plus), L(M::View(View::RulerZoomIn))),
    a("42.9", Arr, "Zoom time ruler out", k(NO, Key::Minus), L(M::View(View::RulerZoomOut))),
    a("42.9", All, "Follow playback", k(ALT_SHIFT, Key::F), L(M::Clip(Cv::ToggleFollow))),
    a("42.9", All, "Scroll left/right", g1("Shift scroll"), W),
    a("42.9", All, "Pan beside a selection", g("Cmd Alt", "Ctrl Alt"), W),
    a("42.9", All, "Add items to a selection", g1("Shift click or drag"), W),
    a("42.9", All, "Add adjacent items to a selection", g1("Shift click"), W),
    a("42.9", All, "Add separate items to a selection", g("Cmd click", "Ctrl click"), W),

    // 42.10 Clip View tabs
    a("42.10", Sample, "Switch Sample/Envelopes tabs", pl(ch(ALT, Key::Tab), ch(CMD, Key::Tab)), L(M::Clip(Cv::CycleEditorMode))),
    a("42.10", Notes, "Switch Notes/Envelopes/MPE tabs", pl(ch(ALT, Key::Tab), ch(CMD, Key::Tab)), L(M::Clip(Cv::CycleEditorMode))),
    a("42.10", Clip, "Sample or Notes tab", k(ALT_SHIFT, Key::Num1), L(M::Clip(Cv::ContentTab))),
    a("42.10", Clip, "Envelopes tab", k(ALT_SHIFT, Key::Num2), L(M::Clip(Cv::EnvelopeTab))),
    a("42.10", Clip, "MPE tab", k(ALT_SHIFT, Key::Num3), p!("C10")),

    // 42.11 Sample Editor
    a("42.11", Sample, "Quantize", k(CMD, Key::U), W),
    a("42.11", Sample, "Quantize settings", k(CMD_SHIFT, Key::U), W),
    b("42.11", Sample, Wh::WarpMarker, "Move Warp Marker left", k(NO, Key::ArrowLeft), W),
    b("42.11", Sample, Wh::WarpMarker, "Move Warp Marker right", k(NO, Key::ArrowRight), W),
    a("42.11", Sample, "Select previous Warp Marker", k(CMD, Key::ArrowLeft), W),
    a("42.11", Sample, "Select next Warp Marker", k(CMD, Key::ArrowRight), W),
    a("42.11", Sample, "Follow playback", pl(ch(ALT_SHIFT, Key::F), ch(CMD_SHIFT, Key::F)), L(M::Clip(Cv::ToggleFollow))),
    a("42.11", Sample, "Move clip region left with start marker", k(SHIFT, Key::ArrowLeft), L(M::Clip(Cv::ShiftRegionLeft))),
    a("42.11", Sample, "Move clip region right with start marker", k(SHIFT, Key::ArrowRight), L(M::Clip(Cv::ShiftRegionRight))),
    a("42.11", Sample, "Zoom to clip selection", k(NO, Key::Z), L(M::Clip(Cv::ZoomToSelection))),
    a("42.11", Sample, "Zoom back from clip selection", k(NO, Key::X), L(M::Clip(Cv::ZoomBack))),
    a("42.11", Sample, "Insert Warp Marker", k(CMD, Key::I), W),
    b("42.11", Sample, Wh::WarpMarker, "Delete Warp Marker", k(NO, Key::Delete), W),
    a("42.11", Sample, "Insert transient", k(CMD_SHIFT, Key::I), W),
    a("42.11", Sample, "Delete transient", k(CMD_SHIFT, Key::Delete), W),
    a("42.11", Sample, "Fit content to view width", k(NO, Key::W), L(M::Clip(Cv::FitWidth))),
    a("42.11", Sample, "Fit content to view height", k(NO, Key::H), L(M::Clip(Cv::FitHeight))),

    // 42.12 MIDI Note Editor
    a("42.12", Notes, "Select all notes", k(CMD, Key::A), L(M::Edit(Edit::SelectAll))),
    a("42.12", Notes, "Copy notes", g("Alt drag", "Ctrl drag"), W),
    a("42.12", Notes, "Chop notes or split at time selection", k(CMD, Key::E), L(M::Notes(Nt::ChopOrSplit))),
    a("42.12", Notes, "Chop notes, one at a time", g("Cmd E drag up/down", "Ctrl E drag up/down"), W),
    a("42.12", Notes, "Chop notes, two at a time", g("Cmd Shift E drag up/down", "Ctrl Shift E drag up/ down"), W),
    a("42.12", Notes, "Split notes at an exact place", g1("Hold E then click to split, drag to move"), W),
    a("42.12", Notes, "Join notes", k(CMD, Key::J), L(M::Notes(Nt::Join))),
    a("42.12", Notes, "Fit notes to time range", k(CMD_ALT, Key::J), L(M::Notes(Nt::FitToRange))),
    a("42.12", Notes, "Fit notes to view width", k(NO, Key::W), L(M::Notes(Nt::FitWidth))),
    a("42.12", Notes, "Fit notes to view height", k(NO, Key::H), L(M::Notes(Nt::FitHeight))),
    a("42.12", Notes, "Quantize", k(CMD, Key::U), L(M::Notes(Nt::Quantize))),
    a("42.12", Notes, "Quantize settings", k(CMD_SHIFT, Key::U), L(M::Notes(Nt::QuantizeSettings))),
    a("42.12", Notes, "Scroll editor up", k(NO, Key::PageUp), L(M::Notes(Nt::ScrollUp))),
    a("42.12", Notes, "Scroll editor down", k(NO, Key::PageDown), L(M::Notes(Nt::ScrollDown))),
    a("42.12", Notes, "Scroll editor up, fine", k(SHIFT, Key::PageUp), L(M::Notes(Nt::ScrollUpFine))),
    a("42.12", Notes, "Scroll editor down, fine", k(SHIFT, Key::PageDown), L(M::Notes(Nt::ScrollDownFine))),
    a("42.12", Notes, "Scroll editor left", k(CMD, Key::PageUp), L(M::Notes(Nt::ScrollLeft))),
    a("42.12", Notes, "Scroll editor right", k(CMD, Key::PageDown), L(M::Notes(Nt::ScrollRight))),
    a("42.12", Notes, "Change velocity from the note editor", g("Cmd drag", "Alt drag"), W),
    b("42.12", Notes, Wh::NoteInsertMarker, "Insert marker to beginning", k(NO, Key::Home), L(M::Notes(Nt::InsertHome))),
    b("42.12", Notes, Wh::NoteInsertMarker, "Insert marker to end", k(NO, Key::End), L(M::Notes(Nt::InsertEnd))),
    a("42.12", Notes, "Select next note", k(ALT, Key::ArrowDown), L(M::Notes(Nt::SelectNext))),
    a("42.12", Notes, "Select previous note", k(ALT, Key::ArrowUp), L(M::Notes(Nt::SelectPrevious))),
    a("42.12", Notes, "Select previous note in key track", k(ALT, Key::ArrowLeft), L(M::Notes(Nt::KeyTrackLeft))),
    a("42.12", Notes, "Select next note in key track", k(ALT, Key::ArrowRight), L(M::Notes(Nt::KeyTrackRight))),
    // Ctrl+Shift+F is both of these on Windows, and the last listed wins: filters, which the
    // manual gives that chord in the Note Editor, must stay after Follow.
    a("42.12", Notes, "Follow playback", pl(ch(ALT_SHIFT, Key::F), ch(CMD_SHIFT, Key::F)), L(M::Clip(Cv::ToggleFollow))),
    a("42.12", Notes, "Show/hide note filters", k(CMD_SHIFT, Key::F), L(M::Notes(Nt::ToggleFilters))),
    a("42.12", Notes, "Move clip region left with start marker", k(SHIFT, Key::ArrowLeft), L(M::Clip(Cv::ShiftRegionLeft))),
    a("42.12", Notes, "Move clip region right with start marker", k(SHIFT, Key::ArrowRight), L(M::Clip(Cv::ShiftRegionRight))),
    a("42.12", Notes, "Zoom to clip selection", k(NO, Key::Z), L(M::Clip(Cv::ZoomToSelection))),
    a("42.12", Notes, "Zoom back from clip selection", k(NO, Key::X), L(M::Notes(Nt::ZoomBack))),
    a("42.12", Notes, "Zoom in horizontally", k(NO, Key::Plus), L(M::Notes(Nt::ZoomIn))),
    a("42.12", Notes, "Zoom out horizontally", k(NO, Key::Minus), L(M::Notes(Nt::ZoomOut))),
    a("42.12", Notes, "Set note velocity by typing", g1("Type a value between 0-127 and press Enter"), W),
    a("42.12", Notes, "Raise note velocity", k(CMD, Key::ArrowUp), L(M::Notes(Nt::VelocityUp))),
    a("42.12", Notes, "Lower note velocity", k(CMD, Key::ArrowDown), L(M::Notes(Nt::VelocityDown))),
    a("42.12", Notes, "Raise velocity deviation", k(CMD_SHIFT, Key::ArrowUp), L(M::Notes(Nt::DeviationUp))),
    a("42.12", Notes, "Lower velocity deviation", k(CMD_SHIFT, Key::ArrowDown), L(M::Notes(Nt::DeviationDown))),
    a("42.12", Notes, "Raise note chance", k(CMD_ALT, Key::ArrowUp), L(M::Notes(Nt::ChanceUp))),
    a("42.12", Notes, "Lower note chance", k(CMD_ALT, Key::ArrowDown), L(M::Notes(Nt::ChanceDown))),
    a("42.12", Clip, "Toggle full-size Clip View", k(CMD_ALT, Key::E), L(M::Clip(Cv::ToggleFullSize))),
    a("42.12", Notes, "Group notes (play all)", k(CMD, Key::G), L(M::Notes(Nt::Group))),
    a("42.12", Notes, "Ungroup notes", k(CMD_SHIFT, Key::G), L(M::Notes(Nt::Ungroup))),
    a("42.12", Notes, "Apply MIDI Tool settings", k(CMD, Key::Enter), p!("C11")),
    a("42.12", Notes, "Invert note selection", k(CMD_SHIFT, Key::A), L(M::Notes(Nt::InvertSelection))),
    a("42.12", Notes, "Highlight scale", k(NO, Key::K), L(M::Notes(Nt::Highlight))),
    a("42.12", Arr, "Split Arrangement clip at time selection", k(CMD_SHIFT, Key::E), L(M::Arrange(Ar::Split))),

    // 42.13 Grid snapping and drawing
    a("42.13", Time, "Toggle Draw Mode", k(NO, Key::B), W),
    a("42.13", Time, "Narrow grid", k(CMD, Key::Num1), L(M::Arrange(Ar::GridNarrow))),
    a("42.13", Time, "Widen grid", k(CMD, Key::Num2), L(M::Arrange(Ar::GridWiden))),
    a("42.13", Time, "Triplet grid", k(CMD, Key::Num3), L(M::Arrange(Ar::GridTriplet))),
    a("42.13", Time, "Snap to grid", k(CMD, Key::Num4), L(M::Arrange(Ar::GridSnap))),
    a("42.13", Time, "Fixed or zoom-adaptive grid", k(CMD, Key::Num5), L(M::Arrange(Ar::GridFixed))),

    b("rlive", Clip, Wh::LoopBrace, "Nudge the Clip View's loop or marker left", k(NO, Key::ArrowLeft), L(M::Clip(Cv::NudgeLeft))),
    b("rlive", Clip, Wh::LoopBrace, "Nudge the Clip View's loop or marker right", k(NO, Key::ArrowRight), L(M::Clip(Cv::NudgeRight))),
    b("rlive", Clip, Wh::LoopBrace, "Move the Clip View's loop by its length, up", k(NO, Key::ArrowUp), L(M::Clip(Cv::MoveLoopUp))),
    b("rlive", Clip, Wh::LoopBrace, "Move the Clip View's loop by its length, down", k(NO, Key::ArrowDown), L(M::Clip(Cv::MoveLoopDown))),
    b("rlive", Clip, Wh::LoopBrace, "Double the Clip View's loop", k(CMD, Key::ArrowUp), L(M::Clip(Cv::DoubleLoop))),
    b("rlive", Clip, Wh::LoopBrace, "Halve the Clip View's loop", k(CMD, Key::ArrowDown), L(M::Clip(Cv::HalveLoop))),
    b("rlive", Clip, Wh::LoopBrace, "Shorten the Clip View's loop", k(CMD, Key::ArrowLeft), L(M::Clip(Cv::ShortenLoop))),
    b("rlive", Clip, Wh::LoopBrace, "Lengthen the Clip View's loop", k(CMD, Key::ArrowRight), L(M::Clip(Cv::LengthenLoop))),
    b("rlive", Clip, Wh::LoopBrace, "Move clip region left, as the manual's Clip View chapter has it", k(ALT, Key::ArrowLeft), L(M::Clip(Cv::ShiftRegionLeft))),
    b("rlive", Clip, Wh::LoopBrace, "Move clip region right, as the manual's Clip View chapter has it", k(ALT, Key::ArrowRight), L(M::Clip(Cv::ShiftRegionRight))),
    a("rlive", Clip, "Select what's in the Clip View's loop", k(CMD_SHIFT, Key::L), L(M::Clip(Cv::SelectLoopContents))),
    a("rlive", Clip, "Narrow the Clip View's grid", k(CMD, Key::Num1), L(M::Clip(Cv::NarrowGrid))),
    a("rlive", Clip, "Widen the Clip View's grid", k(CMD, Key::Num2), L(M::Clip(Cv::WidenGrid))),
    a("rlive", Clip, "Triplet grid in the Clip View", k(CMD, Key::Num3), L(M::Clip(Cv::TripletGrid))),
    a("rlive", Clip, "Snap to grid in the Clip View", k(CMD, Key::Num4), L(M::Clip(Cv::SnapGrid))),
    a("rlive", Clip, "Fixed or zoom-adaptive grid in the Clip View", k(CMD, Key::Num5), L(M::Clip(Cv::AdaptiveGrid))),
    a("rlive", Clip, "Hide/show the Clip View Selector", k(CMD_ALT, Key::O), L(M::Clip(Cv::ToggleOverview))),
    a("42.13", Time, "Bypass snapping while dragging", g("Cmd", "Alt"), W),

    // 42.14 Global quantization
    a("42.14", All, "Sixteenth-note quantization", k(CMD, Key::Num6), L(M::Transport(Tr::QuantizeSixteenth))),
    a("42.14", All, "Eighth-note quantization", k(CMD, Key::Num7), L(M::Transport(Tr::QuantizeEighth))),
    a("42.14", All, "Quarter-note quantization", k(CMD, Key::Num8), L(M::Transport(Tr::QuantizeQuarter))),
    a("42.14", All, "One-bar quantization", k(CMD, Key::Num9), L(M::Transport(Tr::QuantizeBar))),
    a("42.14", All, "Quantization off", k(CMD, Key::Num0), L(M::Transport(Tr::QuantizeOff))),

    // 42.15 Session View
    a("42.15", Ses, "Launch selected clip or slot", k(NO, Key::Enter), L(M::Track(Track::LaunchSelectedClip))),
    b("42.15", Ses, Wh::SlotSelected, "Select clip above", k(NO, Key::ArrowUp), p!("C19")),
    b("42.15", Ses, Wh::SlotSelected, "Select clip below", k(NO, Key::ArrowDown), p!("C19")),
    b("42.15", Ses, Wh::SlotSelected, "Select clip left", k(NO, Key::ArrowLeft), p!("C19")),
    b("42.15", Ses, Wh::SlotSelected, "Select clip right", k(NO, Key::ArrowRight), p!("C19")),
    b("42.15", Ses, Wh::SlotSelected, "Select all clips and slots", k(CMD, Key::A), p!("C19")),
    a("42.15", Ses, "Copy clips", g("Alt drag", "Ctrl drag"), W),
    a("42.15", Ses, "Add or remove stop button", k(CMD, Key::E), L(M::Launch(La::ToggleStopButton))),
    a("42.15", Ses, "Stop clips in the selected slot's track", k(CMD, Key::Enter), L(M::Track(Track::StopSelectedTrack))),
    a("42.15", Ses, "Insert MIDI clip", k(CMD_SHIFT, Key::M), L(M::Track(Track::InsertMidiClip))),
    a("42.15", Ses, "Insert scene", k(CMD, Key::I), L(M::Create(Create::NewScene))),
    a("42.15", Ses, "Insert captured scene", k(CMD_SHIFT, Key::I), L(M::Create(Create::CaptureAndInsertScene))),
    b("42.15", Ses, Wh::TrackTitle, "Scene up", k(NO, Key::ArrowUp), p!("C19")),
    b("42.15", Ses, Wh::TrackTitle, "Scene down", k(NO, Key::ArrowDown), p!("C19")),
    a("42.15", Ses, "Eight scenes up", k(NO, Key::PageUp), p!("C19")),
    a("42.15", Ses, "Eight scenes down", k(NO, Key::PageDown), p!("C19")),
    a("42.15", All, "Record to Session View", k(CMD_SHIFT, Key::F9), L(M::Transport(Tr::SessionRecord))),
    a("42.15", Ses, "Toggle Follow Actions", k(SHIFT, Key::Enter), L(M::Launch(La::ToggleFollow))),
    a("42.15", Ses, "Create Follow Action chain", k(CMD_SHIFT, Key::Enter), L(M::Launch(La::CreateFollowChain))),
    a("42.15", Ses, "Move selected track left", k(CMD, Key::ArrowLeft), L(M::Track(Track::MoveLeft))),
    a("42.15", Ses, "Move selected track right", k(CMD, Key::ArrowRight), L(M::Track(Track::MoveRight))),
    a("42.15", Ses, "Move nonadjacent scenes up", k(CMD, Key::ArrowUp), L(M::Launch(La::MoveScenesUp))),
    a("42.15", Ses, "Move nonadjacent scenes down", k(CMD, Key::ArrowDown), L(M::Launch(La::MoveScenesDown))),
    a("42.15", Ses, "Drop browser clips as a scene", g("Cmd", "Ctrl"), p!("C12")),
    b("42.15", Ses, Wh::SlotSelected, "Deactivate selected clip", k(NO, Key::Num0), L(M::Launch(La::DeactivateClip))),
    b("42.15", Ses, Wh::TrackTitle, "Jump to highlighted track title", k(NO, Key::Escape), p!("C19")),
    b("42.15", Ses, Wh::SlotSelected, "First track of the scene", k(NO, Key::Home), p!("C19")),
    b("42.15", Ses, Wh::SlotSelected, "Last track of the scene", k(NO, Key::End), p!("C19")),
    b("42.15", Ses, Wh::TrackTitle, "Current scene from the last track", k(NO, Key::End), p!("C19")),
    b("42.15", Ses, Wh::ChainSelected, "Solo selected chain", k(NO, Key::S), L(M::Rack(Rack::SoloChain))),

    // 42.16 Arrangement View
    a("42.16", Arr, "Split clip at selection", k(CMD, Key::E), L(M::Arrange(Ar::Split))),
    a("42.16", Arr, "Consolidate selection", k(CMD, Key::J), L(M::Bounce(Bounce::Consolidate))),
    a("42.16", Arr, "Crop selected clips", k(CMD_SHIFT, Key::J), L(M::Arrange(Ar::Crop))),
    a("42.16", Arr, "Resize clip at the insert marker", g1("Enter left and right arrow"), W),
    a("42.16", Arr, "Slide waveform", g1("Shift Alt drag"), W),
    a("42.16", Arr, "Stretch warped clip", g1("Shift drag in clip title bar"), p!("A9")),
    a("42.16", Arr, "Select time within a clip", g1("Shift Alt drag in clip title bar"), W),
    a("42.16", Arr, "Create fade or crossfade", k(CMD_ALT, Key::F), L(M::Arrange(Ar::CreateFade))),
    a("42.16", Arr, "Delete fades in selected clips", k(CMD_ALT, Key::Delete), L(M::Arrange(Ar::DeleteFades))),
    a("42.16", Arr, "Toggle fade handles for a moment", k(NO, Key::F), W),
    a("42.16", Arr, "Toggle loop brace", k(CMD, Key::L), L(M::Arrange(Ar::LoopSelection))),
    b("42.16", Arr, Wh::LoopBrace, "Shorten loop brace", k(CMD, Key::ArrowLeft), L(M::Arrange(Ar::LoopShorten))),
    b("42.16", Arr, Wh::LoopBrace, "Lengthen loop brace", k(CMD, Key::ArrowRight), L(M::Arrange(Ar::LoopLengthen))),
    a("42.16", Arr, "Select loop brace contents", k(CMD_SHIFT, Key::L), L(M::Arrange(Ar::SelectLoop))),
    a("42.16", Arr, "Insert silence", k(CMD, Key::I), L(M::Arrange(Ar::InsertSilence))),
    a("42.16", Arr, "Cut time", k(CMD_SHIFT, Key::X), L(M::Arrange(Ar::CutTime))),
    a("42.16", Arr, "Copy time", k(CMD_SHIFT, Key::C), L(M::Arrange(Ar::CopyTime))),
    a("42.16", Arr, "Paste time", k(CMD_SHIFT, Key::V), L(M::Arrange(Ar::PasteTime))),
    a("42.16", Arr, "Duplicate time", k(CMD_SHIFT, Key::D), L(M::Arrange(Ar::DuplicateTime))),
    a("42.16", Arr, "Delete time", k(CMD_SHIFT, Key::Delete), L(M::Arrange(Ar::DeleteTime))),
    b("42.16", Arr, Wh::TrackHeader, "Fold or unfold selected tracks", k(NO, Key::U), L(M::Arrange(Ar::ToggleFold))),
    b("42.16", Arr, Wh::TrackHeader, "Fold selected tracks", k(NO, Key::ArrowLeft), L(M::Arrange(Ar::Fold))),
    b("42.16", Arr, Wh::TrackHeader, "Unfold selected tracks", k(NO, Key::ArrowRight), L(M::Arrange(Ar::Unfold))),
    a("42.16", Arr, "Unfold all tracks", k(ALT, Key::U), L(M::Arrange(Ar::UnfoldAll))),
    a("42.16", Arr, "Taller tracks or clips", k(ALT, Key::Plus), L(M::Arrange(Ar::Taller))),
    a("42.16", Arr, "Shorter tracks or clips", k(ALT, Key::Minus), L(M::Arrange(Ar::Shorter))),
    a("42.16", Arr, "Follow playback", pl(ch(ALT_SHIFT, Key::F), ch(CMD_SHIFT, Key::F)), L(M::Arrange(Ar::ToggleFollow))),
    a("42.16", Arr, "Pan beside a selection", g("Cmd Alt drag", "Ctrl Alt drag"), W),
    a("42.16", Arr, "Optimize height", k(NO, Key::H), L(M::Arrange(Ar::OptimizeHeight))),
    a("42.16", Arr, "Optimize width", k(NO, Key::W), L(M::Arrange(Ar::OptimizeWidth))),
    b("42.16", Arr, Wh::TimeSelection, "Deactivate selection", k(NO, Key::Num0), L(M::Arrange(Ar::Deactivate))),
    b("42.16", Arr, Wh::TimeSelection, "Nudge selection left", k(NO, Key::ArrowLeft), L(M::Arrange(Ar::NudgeLeft))),
    b("42.16", Arr, Wh::TimeSelection, "Nudge selection right", k(NO, Key::ArrowRight), L(M::Arrange(Ar::NudgeRight))),
    a("42.16", Arr, "Reverse audio selection", k(NO, Key::R), p!("A9")),
    a("42.16", Arr, "Zoom to time selection", k(NO, Key::Z), L(M::Arrange(Ar::ZoomToSelection))),
    a("42.16", Arr, "Zoom back from time selection", k(NO, Key::X), L(M::Arrange(Ar::ZoomBack))),
    a("42.16", Arr, "Play from insert marker in clip", pl(ch(ALT, Key::Space), ch(CMD, Key::Space)), L(M::Arrange(Ar::PlayFromInsert))),
    a("42.16", Arr, "Insert marker to playhead", k(CMD_SHIFT, Key::Space), L(M::Arrange(Ar::InsertToPlayhead))),
    b("42.16", Arr, Wh::TrackTitle, "Jump to highlighted track title", k(NO, Key::Escape), p!("C19")),
    a("42.16", Arr, "Focus Mixer", k(ALT_SHIFT, Key::M), p!("C19")),

    // 42.17 Comping
    a("42.17", Arr, "Show take lanes", k(CMD_ALT, Key::U), p!("C6")),
    b("42.17", Arr, Wh::TakeLane, "Add take area to the main lane", k(NO, Key::Enter), p!("C6")),
    b("42.17", Arr, Wh::TakeLane, "Audition take lane", k(NO, Key::T), p!("C6")),
    a("42.17", Arr, "Add take lane", k(ALT_SHIFT, Key::T), p!("C6")),
    b("42.17", Arr, Wh::TakeLane, "Duplicate take lane", k(CMD, Key::D), p!("C6")),
    b("42.17", Arr, Wh::TakeLane, "Next take into the main clip", k(CMD, Key::ArrowUp), p!("C6")),
    b("42.17", Arr, Wh::TakeLane, "Previous take into the main clip", k(CMD, Key::ArrowDown), p!("C6")),

    // 42.18 Bounce to audio
    a("42.18", All, "Bounce to new track", k(CMD, Key::B), L(M::Bounce(Bounce::ToNewTrack))),
    a("42.18", Main, "Paste bounced audio", k(CMD_ALT, Key::V), L(M::Bounce(Bounce::PasteBounced))),

    // 42.19 Tracks
    a("42.19", All, "Insert audio track", k(CMD, Key::T), L(M::Create(Create::NewAudioTrack))),
    a("42.19", All, "Insert MIDI track", k(CMD_SHIFT, Key::T), L(M::Create(Create::NewMidiTrack))),
    a("42.19", All, "Insert return track", k(CMD_ALT, Key::T), L(M::Create(Create::NewReturnTrack))),
    a("42.19", All, "Rename selected track", k(CMD, Key::R), L(M::Edit(Edit::Rename))),
    b("42.19", All, Wh::Renaming, "Next track while renaming", k(NO, Key::Tab), p!("C19")),
    a("42.19", Main, "Group selected tracks", k(CMD, Key::G), L(M::Create(Create::GroupTracks))),
    a("42.19", Main, "Ungroup tracks", k(CMD_SHIFT, Key::G), L(M::Create(Create::UngroupTracks))),
    b("42.19", Main, Wh::GroupTrack, "Show grouped tracks", k(NO, Key::Plus), L(M::Track(Track::ShowGroupedTracks))),
    b("42.19", Main, Wh::GroupTrack, "Hide grouped tracks", k(NO, Key::Minus), L(M::Track(Track::HideGroupedTracks))),
    b("42.19", Main, Wh::GroupTrack, "Collapse or expand grouped tracks", k(NO, Key::U), L(M::Track(Track::ToggleGroupFold))),
    a("42.19", All, "Hide/show return tracks", k(CMD_ALT, Key::R), p!("C17")),
    b("42.19", Main, Wh::TrackTitle, "Move nonadjacent tracks up", k(CMD, Key::ArrowUp), p!("C19")),
    b("42.19", Main, Wh::TrackTitle, "Move nonadjacent tracks down", k(CMD, Key::ArrowDown), p!("C19")),
    b("42.19", Main, Wh::TrackTitle, "Move nonadjacent tracks left", k(CMD, Key::ArrowLeft), p!("C19")),
    b("42.19", Main, Wh::TrackTitle, "Move nonadjacent tracks right", k(CMD, Key::ArrowRight), p!("C19")),
    a("42.19", Main, "Arm selected tracks", k(NO, Key::C), L(M::Track(Track::Arm))),
    a("42.19", Main, "Solo selected tracks", k(NO, Key::S), L(M::Track(Track::Solo))),
    a("42.19", Brw, "Add device from browser", k(NO, Key::Enter), p!("C12")),
    a("42.19", Main, "Deactivate selected track", k(NO, Key::Num0), L(M::Track(Track::ToggleActivator))),
    a("42.19", All, "Freeze or unfreeze tracks", k(CMD_ALT_SHIFT, Key::F), L(M::Bounce(Bounce::Freeze))),
    b("42.19", Main, Wh::TrackTitle, "Delete track from title bar", k(NO, Key::Delete), p!("C19")),

    // 42.20 Transport
    a("42.20", All, "Play from start marker, or stop", k(NO, Key::Space), L(M::Transport(Tr::PlayStop))),
    a("42.20", All, "Continue from the stop point", k(SHIFT, Key::Space), L(M::Transport(Tr::Continue))),
    b("42.20", Arr, Wh::TimeSelection, "Stop at the end of the selection", pl(ch(ALT, Key::Space), ch(CMD, Key::Space)), L(M::Arrange(Ar::PlaySelectionOnce))),
    b("42.20", Arr, Wh::TimeSelection, "Play the selection", k(NO, Key::Space), L(M::Arrange(Ar::PlaySelection))),
    a("42.20", All, "Insert marker to beginning", k(NO, Key::Home), L(M::Transport(Tr::InsertMarkerToStart))),
    a("42.20", All, "Record", k(NO, Key::F9), L(M::Transport(Tr::Record))),
    a("42.20", All, "Arm recording in Arrangement", k(SHIFT, Key::F9), L(M::Transport(Tr::RecordInverted))),
    a("42.20", All, "Record to Session View", k(CMD_SHIFT, Key::F9), L(M::Transport(Tr::SessionRecord))),
    a("42.20", All, "Back to Arrangement", k(NO, Key::F10), L(M::Transport(Tr::BackToArrangement))),
    a("42.20", All, "Activator, track 1", k(NO, Key::F1), L(M::Track(Track::Activator1))),
    a("42.20", All, "Activator, track 2", k(NO, Key::F2), L(M::Track(Track::Activator2))),
    a("42.20", All, "Activator, track 3", k(NO, Key::F3), L(M::Track(Track::Activator3))),
    a("42.20", All, "Activator, track 4", k(NO, Key::F4), L(M::Track(Track::Activator4))),
    a("42.20", All, "Activator, track 5", k(NO, Key::F5), L(M::Track(Track::Activator5))),
    a("42.20", All, "Activator, track 6", k(NO, Key::F6), L(M::Track(Track::Activator6))),
    a("42.20", All, "Activator, track 7", k(NO, Key::F7), L(M::Track(Track::Activator7))),
    a("42.20", All, "Activator, track 8", k(NO, Key::F8), L(M::Track(Track::Activator8))),
    a("42.20", All, "Toggle metronome", k(NO, Key::O), L(M::Transport(Tr::ToggleMetronome))),

    // 42.21 Audio engine
    a("42.21", All, "Audio engine on/off", k(CMD_ALT_SHIFT, Key::E), p!("A5")),

    // 42.22 Browser
    a("42.22", Brw, "Scroll down", k(NO, Key::ArrowDown), p!("C12")),
    a("42.22", Brw, "Scroll up", k(NO, Key::ArrowUp), p!("C12")),
    a("42.22", All, "Hide/show Browser", k(CMD_ALT, Key::B), L(M::View(View::ToggleBrowser))),
    a("42.22", Brw, "Close folder", k(NO, Key::ArrowLeft), p!("C12")),
    a("42.22", Brw, "Open folder", k(NO, Key::ArrowRight), p!("C12")),
    a("42.22", Brw, "Load selected item", k(NO, Key::Enter), p!("C12")),
    a("42.22", Brw, "Preview selected file", k(SHIFT, Key::Enter), p!("C12")),
    a("42.22", Brw, "Preview selected file", k(NO, Key::ArrowRight), p!("C12")),
    a("42.22", Brw, "Search in Browser", k(CMD, Key::F), p!("C12")),
    a("42.22", Brw, "Jump to search results", k(NO, Key::ArrowDown), p!("C12")),
    a("42.22", Brw, "Jump to search results", k(NO, Key::Enter), p!("C12")),
    a("42.22", Brw, "Assign colour 1", k(NO, Key::Num1), p!("C12")),
    a("42.22", Brw, "Assign colour 2", k(NO, Key::Num2), p!("C12")),
    a("42.22", Brw, "Assign colour 3", k(NO, Key::Num3), p!("C12")),
    a("42.22", Brw, "Assign colour 4", k(NO, Key::Num4), p!("C12")),
    a("42.22", Brw, "Assign colour 5", k(NO, Key::Num5), p!("C12")),
    a("42.22", Brw, "Assign colour 6", k(NO, Key::Num6), p!("C12")),
    a("42.22", Brw, "Assign colour 7", k(NO, Key::Num7), p!("C12")),
    a("42.22", Brw, "Reset assigned colour", k(NO, Key::Num0), p!("C12")),
    a("42.22", Brw, "Similarity search", k(CMD_SHIFT, Key::F), p!("A19")),
    a("42.22", Brw, "Browser history back", k(CMD, Key::OpenBracket), p!("C12")),
    a("42.22", Brw, "Browser history forward", k(CMD, Key::CloseBracket), p!("C12")),
    a("42.22", Brw, "Hide/show Filter View", k(CMD_ALT, Key::G), p!("C12")),
    a("42.22", Brw, "Hide/show Tag Editor", k(CMD_SHIFT, Key::E), p!("C12")),

    // 42.23 Similar sample swapping
    b("42.23", Dev, Wh::SimilarSwap, "Next similar sample", k(CMD, Key::ArrowRight), p!("A19")),
    b("42.23", Dev, Wh::SimilarSwap, "Previous similar sample", k(CMD, Key::ArrowLeft), p!("A19")),
    b("42.23", Dev, Wh::SimilarSwap, "Save as similarity reference", k(CMD, Key::ArrowUp), p!("A19")),
    b("42.23", Dev, Wh::SimilarSwap, "Return to reference", k(CMD, Key::ArrowDown), p!("A19")),
    a("42.23", Dev, "Show similar-sample swap controls", g1("Alt"), p!("A19")),

    // 42.24 Key/MIDI Map Mode and the Computer MIDI Keyboard
    a("42.24", All, "Toggle MIDI Map Mode", k(CMD, Key::M), L(M::Remote(Remote::ToggleMidiMap))),
    a("42.24", All, "Toggle Key Map Mode", k(CMD, Key::K), L(M::Remote(Remote::ToggleKeyMap))),
    a("42.24", All, "Computer MIDI Keyboard", k(NO, Key::M), L(M::Options(Options::ToggleComputerKeys))),
    b("42.24", All, Wh::ComputerKeys, "Computer keyboard octave up", k(NO, Key::X), L(M::Options(Options::OctaveUp))),
    b("42.24", All, Wh::ComputerKeys, "Computer keyboard octave down", k(NO, Key::Z), L(M::Options(Options::OctaveDown))),
    b("42.24", All, Wh::ComputerKeys, "Computer keyboard velocity down", k(NO, Key::C), L(M::Options(Options::VelocityDown))),
    b("42.24", All, Wh::ComputerKeys, "Computer keyboard velocity up", k(NO, Key::V), L(M::Options(Options::VelocityUp))),

    // The Arrangement's keyboard commands that the manual describes in prose rather than listing.
    b("rlive", Arr, Wh::ArrowsFree, "Move insert marker left", k(NO, Key::ArrowLeft), L(M::Arrange(Ar::MoveLeft))),
    b("rlive", Arr, Wh::ArrowsFree, "Move insert marker right", k(NO, Key::ArrowRight), L(M::Arrange(Ar::MoveRight))),
    b("rlive", Arr, Wh::ArrowsFree, "Move insert marker up a track", k(NO, Key::ArrowUp), L(M::Arrange(Ar::MoveUp))),
    b("rlive", Arr, Wh::ArrowsFree, "Move insert marker down a track", k(NO, Key::ArrowDown), L(M::Arrange(Ar::MoveDown))),
    b("rlive", Arr, Wh::EnterHeld, "Shorten clip at the insert marker", k(NO, Key::ArrowLeft), L(M::Arrange(Ar::ResizeLeft))),
    b("rlive", Arr, Wh::EnterHeld, "Lengthen clip at the insert marker", k(NO, Key::ArrowRight), L(M::Arrange(Ar::ResizeRight))),
    a("rlive", Arr, "Extend selection left", k(SHIFT, Key::ArrowLeft), L(M::Arrange(Ar::ExtendLeft))),
    a("rlive", Arr, "Extend selection right", k(SHIFT, Key::ArrowRight), L(M::Arrange(Ar::ExtendRight))),
    a("rlive", Arr, "Insert marker to clip edge or locator, left", pl(ch(ALT, Key::ArrowLeft), ch(CMD, Key::ArrowLeft)), L(M::Arrange(Ar::SnapLeft))),
    a("rlive", Arr, "Insert marker to clip edge or locator, right", pl(ch(ALT, Key::ArrowRight), ch(CMD, Key::ArrowRight)), L(M::Arrange(Ar::SnapRight))),
    a("rlive", Arr, "Insert marker to end", k(NO, Key::End), L(M::Arrange(Ar::InsertToEnd))),

    // The note editor's keys that the manual describes in prose rather than listing.
    b("rlive", Notes, Wh::NotesFree, "Move notes or insert marker left", k(NO, Key::ArrowLeft), L(M::Notes(Nt::Left))),
    b("rlive", Notes, Wh::NotesFree, "Move notes or insert marker right", k(NO, Key::ArrowRight), L(M::Notes(Nt::Right))),
    b("rlive", Notes, Wh::NotesFree, "Transpose notes up", k(NO, Key::ArrowUp), L(M::Notes(Nt::Up))),
    b("rlive", Notes, Wh::NotesFree, "Transpose notes down", k(NO, Key::ArrowDown), L(M::Notes(Nt::Down))),
    b("rlive", Notes, Wh::NotesFree, "Transpose notes up an octave", k(SHIFT, Key::ArrowUp), L(M::Notes(Nt::OctaveUp))),
    b("rlive", Notes, Wh::NotesFree, "Transpose notes down an octave", k(SHIFT, Key::ArrowDown), L(M::Notes(Nt::OctaveDown))),
    b("rlive", Notes, Wh::NotesFree, "Nudge notes left, off the grid", k(CMD, Key::ArrowLeft), L(M::Notes(Nt::NudgeLeft))),
    b("rlive", Notes, Wh::NotesFree, "Nudge notes right, off the grid", k(CMD, Key::ArrowRight), L(M::Notes(Nt::NudgeRight))),
    b("rlive", Notes, Wh::NotesFree, "Shorten notes or narrow the time selection", k(SHIFT, Key::ArrowLeft), L(M::Notes(Nt::ExtendLeft))),
    b("rlive", Notes, Wh::NotesFree, "Lengthen notes or extend the time selection", k(SHIFT, Key::ArrowRight), L(M::Notes(Nt::ExtendRight))),
    b("rlive", Notes, Wh::NotesFree, "Shorten notes or narrow the time selection, off the grid", k(CMD_SHIFT, Key::ArrowLeft), L(M::Notes(Nt::ExtendFineLeft))),
    b("rlive", Notes, Wh::NotesFree, "Lengthen notes or extend the time selection, off the grid", k(CMD_SHIFT, Key::ArrowRight), L(M::Notes(Nt::ExtendFineRight))),
    b("rlive", Notes, Wh::NotesFree, "Narrow the time selection to a note boundary", k(ALT_SHIFT, Key::ArrowLeft), L(M::Notes(Nt::ExtendBoundaryLeft))),
    b("rlive", Notes, Wh::NotesFree, "Extend the time selection to a note boundary", k(ALT_SHIFT, Key::ArrowRight), L(M::Notes(Nt::ExtendBoundaryRight))),
    b("rlive", Notes, Wh::NotesFree, "Add the previous note to the selection", k(ALT_SHIFT, Key::ArrowUp), L(M::Notes(Nt::AddPrevious))),
    b("rlive", Notes, Wh::NotesFree, "Add the next note to the selection", k(ALT_SHIFT, Key::ArrowDown), L(M::Notes(Nt::AddNext))),
    b("rlive", Notes, Wh::NotesFree, "Switch between a time span and the notes in it", k(NO, Key::Enter), L(M::Notes(Nt::EnterKey))),
    b("rlive", Notes, Wh::NotesFree, "Deselect", k(NO, Key::Escape), L(M::Notes(Nt::Escape))),
    b("rlive", Notes, Wh::NotesFree, "Deactivate or reactivate notes", k(NO, Key::Num0), L(M::Notes(Nt::Deactivate))),
    b("rlive", Notes, Wh::NotesFree, "Fold to Notes", k(NO, Key::F), L(M::Notes(Nt::FoldNotes))),
    b("rlive", Notes, Wh::NotesFree, "Fold to Scale", k(NO, Key::G), L(M::Notes(Nt::FoldScale))),
    b("rlive", Notes, Wh::NotesFree, "Focus Mode, or for a moment while held", k(NO, Key::N), W),
    b("rlive", Notes, Wh::NotesFree, "Duplicate Time", k(CMD_SHIFT, Key::D), L(M::Notes(Nt::DuplicateTime))),
    b("rlive", Notes, Wh::NotesFree, "Delete Time", k(CMD_SHIFT, Key::Delete), L(M::Notes(Nt::DeleteTime))),
    b("rlive", Notes, Wh::NotesFree, "Insert Time", k(CMD, Key::I), L(M::Notes(Nt::InsertTime))),
    b("rlive", Notes, Wh::NotesFree, "Crop Clip to Time Selection", k(CMD_SHIFT, Key::J), L(M::Notes(Nt::CropToSelection))),

    // Kept from before the manual pass: not in the manual, but harmless and already in use.
    a("rlive", All, "Hide/show Detail View", k(CMD_ALT, Key::L), L(M::View(View::ToggleDetail))),
    a("rlive", All, "Redo", pl(ch(CMD_SHIFT, Key::Z), ch(CMD_SHIFT, Key::Z)), L(M::Edit(Edit::Redo))),
    // With Cmd held the OS reports the key's unshifted character, so Cmd+"+" often arrives as "=".
    a("rlive", All, "Zoom window in", k(CMD, Key::Equals), L(M::View(View::ZoomIn))),
];
