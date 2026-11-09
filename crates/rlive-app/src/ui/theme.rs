//! Visual design system: palettes, fonts and egui style.
//!
//! Every colour the UI draws is read from the current [`Theme`], rebuilt whenever
//! [`ThemeSettings`] or the OS appearance change. The default settings reproduce the original
//! dark look exactly: see the tests at the bottom of this file.

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::sync::Arc;

pub const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Declares each named colour once: its field on [`Theme`], its value in the default look, and a
/// deprecated alias for code still written against the old constants.
macro_rules! palette {
    ($($name:ident $field:ident $value:literal),* $(,)?) => {
        /// The colours of the current look, read fresh every frame through [`pal`].
        #[derive(Clone, Copy, PartialEq, Debug)]
        pub struct Theme {
            $(pub $field: Color32,)*
            map: ColorMap,
            grid_opacity: f32,
        }

        impl Theme {
            fn build(map: ColorMap, grid_opacity: f32) -> Self {
                Self { $($field: map.apply(hex($value)),)* map, grid_opacity }
            }
        }

        $(
            #[deprecated(note = "read the field of `pal()` with this name in lower case, so it follows the theme")]
            pub const $name: Color32 = hex($value);
        )*
    };
}

palette! {
    // Surfaces
    APP_BG app_bg 0x1c1c1c,
    BAR_BG bar_bg 0x2a2a2a,
    PANEL panel 0x303030,
    PANEL_DARK panel_dark 0x262626,
    RAISED raised 0x3b3b3b,
    RAISED_HOVER raised_hover 0x474747,
    GRID_BG grid_bg 0x1f1f1f,
    SLOT_EMPTY slot_empty 0x2c2c2c,
    SLOT_EMPTY_HOVER slot_empty_hover 0x353535,
    SEPARATOR separator 0x141414,
    FIELD_BG field_bg 0x1a1a1a,
    // Text
    TEXT text 0xd6d6d6,
    TEXT_DIM text_dim 0x8f8f8f,
    TEXT_FAINT text_faint 0x5e5e5e,
    TEXT_DARK text_dark 0x111111,
    // States
    ACCENT accent 0xffa733,
    PLAY_GREEN play_green 0x3ddc68,
    ARM_RED arm_red 0xff5050,
    SOLO_BLUE solo_blue 0x5ab4ff,
    ACTIVATOR activator 0xffc83d,
    SELECT select 0xb5e0ff,
    KNOB_ARC knob_arc 0xffa733,
    KNOB_TRACK knob_track 0x161616,
}

impl Default for Theme {
    fn default() -> Self {
        Self::build(ColorMap::IDENTITY, 1.0)
    }
}

impl Theme {
    /// A colour from the default look, as it appears in this theme.
    pub fn map(&self, c: Color32) -> Color32 {
        self.map.apply(c)
    }

    /// A grid line of `alpha` strength, scaled by the user's Grid Line Opacity. It lightens dark
    /// themes and darkens light ones, so it always stands out from what is behind it.
    pub fn grid_line(&self, alpha: u8) -> Color32 {
        self.overlay(alpha).gamma_multiply(self.grid_opacity)
    }

    /// A translucent wash that lightens in dark themes and darkens in light ones.
    pub fn overlay(&self, alpha: u8) -> Color32 {
        if self.map.light {
            Color32::from_black_alpha(alpha)
        } else {
            Color32::from_white_alpha(alpha)
        }
    }
}

thread_local! {
    static CURRENT: Cell<Theme> = Cell::new(Theme::default());
}

/// The current theme; cheap enough to call at every use.
pub fn pal() -> Theme {
    CURRENT.with(Cell::get)
}

/// A colour written as `0xRRGGBB` in the default look, translated into the current theme.
pub fn shade(v: u32) -> Color32 {
    pal().map(hex(v))
}

/// A named colour scheme. Each one works in both light and dark.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ThemeName {
    #[default]
    Default,
    /// Flatter, with the lights and darks pulled toward the middle.
    Mid,
    /// Deeper blacks and brighter text.
    Deep,
    /// A blue-grey cast with a blue accent.
    Slate,
}

impl ThemeName {
    pub const ALL: [ThemeName; 4] = [Self::Default, Self::Mid, Self::Deep, Self::Slate];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Mid => "Mid",
            Self::Deep => "Deep",
            Self::Slate => "Slate",
        }
    }
}

/// Whether the UI is light, dark, or follows the operating system.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Appearance {
    Light,
    #[default]
    Dark,
    FollowOs,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Self::Light, Self::Dark, Self::FollowOs];

    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::FollowOs => "Follow OS",
        }
    }
}

/// The colour cast of the neutral surfaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Tone {
    #[default]
    Neutral,
    Warm,
    Cool,
}

impl Tone {
    pub const ALL: [Tone; 3] = [Self::Neutral, Self::Warm, Self::Cool];

    pub fn label(self) -> &'static str {
        match self {
            Self::Neutral => "Neutral",
            Self::Warm => "Warm",
            Self::Cool => "Cool",
        }
    }
}

/// What the user chose on the Theme & Colors tab.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    pub name: ThemeName,
    pub appearance: Appearance,
    pub tone: Tone,
    pub high_contrast: bool,
    /// How strongly grid lines are drawn, 0 to 1.
    pub grid_opacity: f32,
    /// Added to every colour's lightness, -1 to 1.
    pub brightness: f32,
    /// Scales colour saturation; 1 is unchanged, 0 is grey.
    pub color_intensity: f32,
    /// Turns the hue of the UI's colours, in degrees.
    pub hue: f32,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            name: ThemeName::Default,
            appearance: Appearance::Dark,
            tone: Tone::Neutral,
            high_contrast: false,
            grid_opacity: 1.0,
            brightness: 0.0,
            color_intensity: 1.0,
            hue: 0.0,
        }
    }
}

impl ThemeSettings {
    /// The theme these settings describe. `os_dark` is the operating system's appearance.
    pub fn resolve(&self, os_dark: bool) -> Theme {
        let light = match self.appearance {
            Appearance::Light => true,
            Appearance::Dark => false,
            Appearance::FollowOs => !os_dark,
        };
        let (mut lift, mut contrast, mut tint, hue_shift) = match self.name {
            ThemeName::Default => (0.0, 1.0, 0.0, 0.0),
            ThemeName::Mid => (0.07, 0.8, 0.0, 0.0),
            ThemeName::Deep => (-0.05, 1.1, 0.0, 0.0),
            ThemeName::Slate => (0.0, 1.0, -0.6, 170.0),
        };
        if self.high_contrast {
            contrast *= 1.35;
            lift *= 0.5;
        }
        tint += match self.tone {
            Tone::Neutral => 0.0,
            Tone::Warm => 1.0,
            Tone::Cool => -1.0,
        };
        let map = ColorMap {
            light,
            lift,
            contrast,
            tint,
            brightness: self.brightness.clamp(-1.0, 1.0),
            intensity: self.color_intensity.clamp(0.0, 2.0),
            hue: self.hue + hue_shift,
        };
        let mut theme = Theme::build(map, self.grid_opacity.clamp(0.0, 1.0));
        // These two sit on colour rather than on the page, so they don't invert.
        theme.text_dark = hex(0x111111);
        if light {
            theme.separator = map.apply(hex(0x707070));
        }
        theme
    }
}

/// How a colour from the default look is turned into a themed one.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ColorMap {
    light: bool,
    lift: f32,
    contrast: f32,
    tint: f32,
    brightness: f32,
    intensity: f32,
    hue: f32,
}

impl ColorMap {
    const IDENTITY: Self = Self {
        light: false,
        lift: 0.0,
        contrast: 1.0,
        tint: 0.0,
        brightness: 0.0,
        intensity: 1.0,
        hue: 0.0,
    };

    fn apply(&self, c: Color32) -> Color32 {
        if *self == Self::IDENTITY {
            return c;
        }
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
        let (r, g, b) = if saturation < 0.12 {
            self.neutral((r + g + b) / 3.0)
        } else {
            self.chromatic(r, g, b)
        };
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Color32::from_rgba_unmultiplied(byte(r), byte(g), byte(b), a)
    }

    /// Greys carry the appearance, the tone and the contrast.
    fn neutral(&self, level: f32) -> (f32, f32, f32) {
        let level = if self.light { 1.0 - level } else { level };
        let level = 0.5 + (level - 0.5) * self.contrast + self.lift + self.brightness * 0.35;
        let cast = self.tint * 0.018;
        (level + cast, level, level - cast)
    }

    /// Colours keep their identity; only intensity, hue and brightness move them.
    fn chromatic(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        let (h, s, v) = rgb_to_hsv(r, g, b);
        let v = v + self.brightness * 0.25 - if self.light { 0.08 } else { 0.0 };
        hsv_to_rgb(
            h + self.hue,
            (s * self.intensity).min(1.0),
            v.clamp(0.0, 1.0),
        )
    }
}

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    if d <= 0.0 {
        return (0.0, 0.0, max);
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, d / max, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (r + m, g + m, b + m)
}

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
        pal().text_dark
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
    ctx.all_styles_mut(|s| {
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
    set_current(ctx, Theme::default());
}

/// Makes `theme` the one the UI reads, and gives egui's own widgets matching colours.
fn set_current(ctx: &egui::Context, theme: Theme) {
    CURRENT.with(|c| c.set(theme));
    let v = visuals(&theme);
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
}

/// Call at the start of every frame. Picks up settings changes and OS appearance changes.
pub fn begin_frame(ctx: &egui::Context, settings: &ThemeSettings) {
    let os_dark = ctx.system_theme() != Some(egui::Theme::Light);
    let theme = settings.resolve(os_dark);
    if theme != pal() {
        set_current(ctx, theme);
    }
}

/// egui's own widget colours for a theme.
fn visuals(t: &Theme) -> egui::Visuals {
    let mut v = if t.map.light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    v.panel_fill = t.panel;
    v.window_fill = t.map(hex(0x343434));
    v.window_stroke = Stroke::new(1.0, t.separator);
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
    v.extreme_bg_color = t.field_bg;
    v.faint_bg_color = t.panel_dark;
    v.code_bg_color = t.field_bg;
    v.selection.bg_fill = t.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.hyperlink_color = t.solo_blue;
    v.override_text_color = None;
    v.striped = false;

    let r = CornerRadius::same(2);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.panel;
    w.noninteractive.weak_bg_fill = t.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = t.raised;
    w.inactive.weak_bg_fill = t.raised;
    w.inactive.bg_stroke = Stroke::new(1.0, t.separator);
    w.inactive.fg_stroke = Stroke::new(1.0, t.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = t.raised_hover;
    w.hovered.weak_bg_fill = t.raised_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, t.map(hex(0x5a5a5a)));
    w.hovered.fg_stroke = Stroke::new(1.0, t.map(hex(0xf0f0f0)));
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = t.accent.gamma_multiply(0.85);
    w.active.weak_bg_fill = t.accent.gamma_multiply(0.85);
    w.active.bg_stroke = Stroke::new(1.0, t.accent);
    w.active.fg_stroke = Stroke::new(1.0, t.text_dark);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = t.raised_hover;
    w.open.weak_bg_fill = t.raised_hover;
    w.open.corner_radius = r;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The original palette, written out so a change to the table above can't slip through.
    type Field = fn(&Theme) -> Color32;

    #[rustfmt::skip]
    const ORIGINAL: [(Field, u32); 23] = [
        (|t| t.app_bg, 0x1c1c1c), (|t| t.bar_bg, 0x2a2a2a), (|t| t.panel, 0x303030),
        (|t| t.panel_dark, 0x262626), (|t| t.raised, 0x3b3b3b), (|t| t.raised_hover, 0x474747),
        (|t| t.grid_bg, 0x1f1f1f), (|t| t.slot_empty, 0x2c2c2c), (|t| t.slot_empty_hover, 0x353535),
        (|t| t.separator, 0x141414), (|t| t.field_bg, 0x1a1a1a), (|t| t.text, 0xd6d6d6),
        (|t| t.text_dim, 0x8f8f8f), (|t| t.text_faint, 0x5e5e5e), (|t| t.text_dark, 0x111111),
        (|t| t.accent, 0xffa733), (|t| t.play_green, 0x3ddc68), (|t| t.arm_red, 0xff5050),
        (|t| t.solo_blue, 0x5ab4ff), (|t| t.activator, 0xffc83d), (|t| t.select, 0xb5e0ff),
        (|t| t.knob_arc, 0xffa733), (|t| t.knob_track, 0x161616),
    ];

    #[test]
    fn the_default_theme_is_the_original_palette() {
        let theme = Theme::default();
        for (read, value) in ORIGINAL {
            assert_eq!(read(&theme), hex(value));
        }
    }

    #[test]
    fn default_settings_resolve_to_the_default_theme_in_either_os_appearance() {
        let settings = ThemeSettings::default();
        assert_eq!(settings.resolve(true), Theme::default());
        assert_eq!(settings.resolve(false), Theme::default());
    }

    #[test]
    fn the_default_theme_leaves_every_literal_colour_alone() {
        let theme = Theme::default();
        for v in [0x000000, 0x2a2a2a, 0x3d4a57, 0xb5e0ff, 0xffffff] {
            assert_eq!(theme.map(hex(v)), hex(v));
        }
        assert_eq!(theme.grid_line(45), Color32::from_white_alpha(45));
        assert_eq!(theme.overlay(45), Color32::from_white_alpha(45));
    }

    #[test]
    fn the_default_theme_gives_egui_the_original_widget_colours() {
        let v = visuals(&Theme::default());
        assert_eq!(v.panel_fill, hex(0x303030));
        assert_eq!(v.window_fill, hex(0x343434));
        assert_eq!(v.extreme_bg_color, hex(0x1a1a1a));
        assert_eq!(v.faint_bg_color, hex(0x262626));
        assert_eq!(v.widgets.inactive.bg_fill, hex(0x3b3b3b));
        assert_eq!(v.widgets.hovered.bg_fill, hex(0x474747));
        assert_eq!(v.widgets.hovered.bg_stroke.color, hex(0x5a5a5a));
        assert_eq!(v.widgets.hovered.fg_stroke.color, hex(0xf0f0f0));
        assert_eq!(v.widgets.active.fg_stroke.color, hex(0x111111));
        assert_eq!(v.selection.stroke.color, hex(0xffa733));
        assert_eq!(v.hyperlink_color, hex(0x5ab4ff));
        assert!(v.dark_mode);
    }

    fn with(f: impl FnOnce(&mut ThemeSettings)) -> Theme {
        let mut s = ThemeSettings::default();
        f(&mut s);
        s.resolve(true)
    }

    fn level(c: Color32) -> i32 {
        c.r() as i32 + c.g() as i32 + c.b() as i32
    }

    #[test]
    fn light_turns_the_page_light_and_the_text_dark() {
        let t = with(|s| s.appearance = Appearance::Light);
        assert!(level(t.panel) > level(t.text));
        assert!(level(t.app_bg) > 600, "{:?}", t.app_bg);
        assert_eq!(t.overlay(45), Color32::from_black_alpha(45));
    }

    #[test]
    fn follow_os_uses_the_os_appearance() {
        let s = ThemeSettings {
            appearance: Appearance::FollowOs,
            ..ThemeSettings::default()
        };
        assert_eq!(s.resolve(true), Theme::default());
        let light = ThemeSettings {
            appearance: Appearance::Light,
            ..ThemeSettings::default()
        };
        assert_eq!(s.resolve(false), light.resolve(true));
    }

    #[test]
    fn tone_casts_the_neutrals_warm_or_cool_and_leaves_them_grey_when_neutral() {
        let warm = with(|s| s.tone = Tone::Warm).panel;
        assert!(warm.r() > warm.b());
        let cool = with(|s| s.tone = Tone::Cool).panel;
        assert!(cool.b() > cool.r());
        let neutral = Theme::default().panel;
        assert_eq!(neutral.r(), neutral.b());
    }

    #[test]
    fn high_contrast_pushes_text_and_page_apart() {
        let plain = Theme::default();
        let high = with(|s| s.high_contrast = true);
        assert!(level(high.text) - level(high.panel) > level(plain.text) - level(plain.panel));
    }

    #[test]
    fn named_themes_differ_from_each_other() {
        let themes: Vec<Theme> = ThemeName::ALL
            .iter()
            .map(|n| with(|s| s.name = *n))
            .collect();
        for (i, a) in themes.iter().enumerate() {
            for b in &themes[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn brightness_and_intensity_and_hue_move_the_colours() {
        let brighter = with(|s| s.brightness = 0.5);
        assert!(level(brighter.panel) > level(Theme::default().panel));
        let grey = with(|s| s.color_intensity = 0.0).accent;
        assert_eq!(grey.r(), grey.g());
        assert_eq!(grey.g(), grey.b());
        let turned = with(|s| s.hue = 120.0).accent;
        assert_ne!(turned, Theme::default().accent);
    }

    #[test]
    fn grid_opacity_fades_grid_lines() {
        let faded = with(|s| s.grid_opacity = 0.5).grid_line(100).a();
        let full = with(|s| s.grid_opacity = 1.0).grid_line(100).a();
        assert!(faded < full, "{faded} {full}");
        assert_eq!(with(|s| s.grid_opacity = 0.0).grid_line(100).a(), 0);
    }

    #[test]
    fn user_clip_colours_are_not_part_of_the_theme() {
        // text_on reads text_dark, which has to stay dark in a light theme to sit on bright fills.
        let t = with(|s| s.appearance = Appearance::Light);
        assert_eq!(t.text_dark, hex(0x111111));
    }
}
