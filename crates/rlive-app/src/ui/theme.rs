//! Visual design system: an Ableton-like dark palette, fonts and egui style.

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};
use std::sync::Arc;

pub const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

// Surfaces
pub const APP_BG: Color32 = hex(0x1c1c1c);
pub const BAR_BG: Color32 = hex(0x2a2a2a);
pub const PANEL: Color32 = hex(0x303030);
pub const PANEL_DARK: Color32 = hex(0x262626);
pub const RAISED: Color32 = hex(0x3b3b3b);
pub const RAISED_HOVER: Color32 = hex(0x474747);
pub const GRID_BG: Color32 = hex(0x1f1f1f);
pub const SLOT_EMPTY: Color32 = hex(0x2c2c2c);
pub const SLOT_EMPTY_HOVER: Color32 = hex(0x353535);
pub const SEPARATOR: Color32 = hex(0x141414);
pub const FIELD_BG: Color32 = hex(0x1a1a1a);

// Text
pub const TEXT: Color32 = hex(0xd6d6d6);
pub const TEXT_DIM: Color32 = hex(0x8f8f8f);
pub const TEXT_FAINT: Color32 = hex(0x5e5e5e);
pub const TEXT_DARK: Color32 = hex(0x111111);

// States
pub const ACCENT: Color32 = hex(0xffa733);
pub const PLAY_GREEN: Color32 = hex(0x3ddc68);
pub const ARM_RED: Color32 = hex(0xff5050);
pub const SOLO_BLUE: Color32 = hex(0x5ab4ff);
pub const ACTIVATOR: Color32 = hex(0xffc83d);
pub const SELECT: Color32 = hex(0xb5e0ff);
pub const KNOB_ARC: Color32 = hex(0xffa733);
pub const KNOB_TRACK: Color32 = hex(0x161616);

pub const BOLD: &str = "bold";

pub fn bold() -> FontFamily {
    FontFamily::Name(BOLD.into())
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn font_bold(size: f32) -> FontId {
    FontId::new(size, bold())
}

/// Text colour that stays readable on a given (clip/track) fill colour.
pub fn text_on(fill: Color32) -> Color32 {
    let l = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    if l > 140.0 {
        TEXT_DARK
    } else {
        hex(0xf2f2f2)
    }
}

/// Darken a colour by `amount` (0..1).
pub fn darken(c: Color32, amount: f32) -> Color32 {
    let k = 1.0 - amount;
    Color32::from_rgb(
        (c.r() as f32 * k) as u8,
        (c.g() as f32 * k) as u8,
        (c.b() as f32 * k) as u8,
    )
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-SemiBold.ttf"
        ))),
    );
    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "inter".into());
    let mut bold = vec!["inter-semibold".to_string()];
    bold.extend(defaults);
    fonts.families.insert(bold_family(), bold);
    ctx.set_fonts(fonts);
}

fn bold_family() -> FontFamily {
    bold()
}

/// Install fonts and the global style. Call once at startup.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = hex(0x343434);
    v.window_stroke = Stroke::new(1.0, SEPARATOR);
    v.window_corner_radius = CornerRadius::same(3);
    v.menu_corner_radius = CornerRadius::same(2);
    v.window_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.popup_shadow = egui::Shadow {
        offset: [0, 2],
        blur: 6,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    v.extreme_bg_color = FIELD_BG;
    v.faint_bg_color = PANEL_DARK;
    v.code_bg_color = FIELD_BG;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = SOLO_BLUE;
    v.override_text_color = None;
    v.striped = false;

    let r = CornerRadius::same(2);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0, SEPARATOR);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = RAISED;
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, SEPARATOR);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = RAISED_HOVER;
    w.hovered.weak_bg_fill = RAISED_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, hex(0x5a5a5a));
    w.hovered.fg_stroke = Stroke::new(1.0, hex(0xf0f0f0));
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = ACCENT.gamma_multiply(0.85);
    w.active.weak_bg_fill = ACCENT.gamma_multiply(0.85);
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, TEXT_DARK);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = RAISED_HOVER;
    w.open.weak_bg_fill = RAISED_HOVER;
    w.open.corner_radius = r;
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(5.0, 3.0);
        s.spacing.button_padding = egui::vec2(6.0, 2.0);
        s.spacing.interact_size = egui::vec2(36.0, 18.0);
        s.spacing.slider_width = 90.0;
        s.spacing.combo_width = 70.0;
        s.spacing.menu_margin = egui::Margin::same(4);
        s.spacing.window_margin = egui::Margin::same(10);
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.interaction.tooltip_delay = 0.6;
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(10.0)),
            (TextStyle::Body, FontId::proportional(12.0)),
            (TextStyle::Button, FontId::proportional(12.0)),
            (TextStyle::Heading, FontId::new(15.0, bold())),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
    });
}
