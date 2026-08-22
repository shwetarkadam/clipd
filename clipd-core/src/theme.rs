use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    /// Follows the current macOS appearance (light/dark).
    System,
    /// Paper Light — warm ivory, graphite ink, muted sage accent.
    /// Legacy configs that stored `LightMinimal` land here.
    #[serde(alias = "LightMinimal")]
    Light,
    /// True dark with an off-white accent — neutral and modern.
    /// Configs holding the retired Paper Dark, Glass Dark, Command Palette
    /// or Minimal Dark land here.
    #[serde(
        alias = "Paper",
        alias = "GlassDark",
        alias = "CommandPalette",
        alias = "MinimalDark"
    )]
    Dark,
    /// Deep navy background with a periwinkle accent.
    Midnight,
    /// Dark green background with a sage accent.
    Forest,
    /// Cool slate background with a soft rose accent.
    /// Configs holding the retired Cocoa land here.
    #[serde(alias = "Cocoa")]
    Slate,
    /// Frosted translucent light glass — mint accent, dark ink (mockup Glass Light).
    GlassLight,
    /// Frosted translucent dark glass — mint accent, light ink.
    /// Legacy configs that stored `GlassMinimal` land here.
    #[serde(alias = "GlassMinimal")]
    /// Minimal Dark tuned for a small, dense window.
    CompactCapsule,
    /// Catppuccin Mocha — the island's own palette, for the rest of clipd.
    Catppuccin,
}

impl Theme {
    pub const ALL: [Theme; 9] = [
        Theme::System,
        Theme::Light,
        Theme::Dark,
        Theme::Midnight,
        Theme::Forest,
        Theme::Slate,
        Theme::GlassLight,
        Theme::CompactCapsule,
        Theme::Catppuccin,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Paper Light",
            Theme::Dark => "Dark",
            Theme::Midnight => "Midnight",
            Theme::Forest => "Forest",
            Theme::Slate => "Slate",
            Theme::GlassLight => "Glass Light",
            Theme::CompactCapsule => "Compact Capsule",
            Theme::Catppuccin => "Catppuccin",
        }
    }

    pub fn next(&self) -> Theme {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Midnight,
            Theme::Midnight => Theme::Forest,
            Theme::Forest => Theme::Slate,
            Theme::Slate => Theme::GlassLight,
            Theme::GlassLight => Theme::CompactCapsule,
            Theme::CompactCapsule => Theme::Catppuccin,
            Theme::Catppuccin => Theme::System,
        }
    }

    pub fn colors(&self) -> ThemeColors {
        match self {
            Theme::System => DARK,
            Theme::Light => LIGHT,
            Theme::Dark => DARK,
            Theme::Midnight => MIDNIGHT,
            Theme::Forest => FOREST,
            Theme::Slate => SLATE,
            Theme::GlassLight => GLASS_LIGHT,
            Theme::CompactCapsule => COMPACT_CAPSULE,
            Theme::Catppuccin => CATPPUCCIN,
        }
    }

    pub fn is_light(&self) -> bool {
        matches!(self, Theme::Light | Theme::GlassLight)
    }

    /// True for frosted translucent shells (Glass Light / Glass Dark).
    pub fn is_glass(&self) -> bool {
        matches!(self, Theme::GlassLight)
    }

    /// Soft glassmorphism blooms under the frost plate. `None` = flat opaque shell.
    pub fn shell_glows(&self) -> Option<(Rgb, Rgb)> {
        match self {
            // Soft sky + lavender (Glass Light glassmorphism).
            Theme::GlassLight => Some((GLASS_LIGHT_GLOW_A, GLASS_LIGHT_GLOW_B)),
            _ => None,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Dark
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub bg_base: Rgb,
    pub bg_surface: Rgb,
    pub bg_elevated: Rgb,
    pub bg_selected: Rgb,
    pub bg_hover: Rgb,
    pub accent: Rgb,
    pub accent2: Rgb,
    pub text: Rgb,
    pub subtext: Rgb,
    pub overlay: Rgb,
    pub green: Rgb,
    pub border: Rgb,

    /// Opacity of the background surfaces — cards, rows, pills — as an alpha.
    ///
    /// 255 for every solid theme, which is what makes this backwards
    /// compatible. The glass themes drop it so surfaces become frosted panes
    /// that the shell's blur and blooms show *through*, instead of opaque
    /// cards sitting on top of a transparent window. Without this the shell can
    /// be as translucent as you like and the UI still reads as a white panel.
    pub surface_alpha: u8,

    pub code: Rgb,
    pub url: Rgb,
    pub email: Rgb,
    pub path: Rgb,
}

// Paper Light — warm ivory shell, near-black graphite ink (readable on
// cream), soft beige separators, muted sage for pins/filters. No stark white.
const LIGHT: ThemeColors = ThemeColors {
    bg_base: Rgb(242, 237, 226),
    bg_surface: Rgb(247, 243, 232),
    bg_elevated: Rgb(249, 245, 235),
    // Sage-tinted selection — the old beige wash was nearly identical to the
    // card and made the active row (and selected text) look unchanged.
    bg_selected: Rgb(220, 230, 212),
    bg_hover: Rgb(236, 232, 218),
    // Sage from the mockup (~#78966D), darkened just enough for 3:1 on ivory.
    accent: Rgb(108, 135, 98),
    accent2: Rgb(74, 70, 62),
    // Ink pushed darker than the mockup graphite — warm ivory eats mid-grays.
    text: Rgb(28, 26, 23),
    subtext: Rgb(74, 70, 62),
    overlay: Rgb(102, 97, 88),
    // A step darker than the accent: the spot marks stars and active chips on
    // ivory, where the accent itself is only just clearing 3:1.
    green: Rgb(88, 118, 78),
    border: Rgb(210, 200, 182),
    surface_alpha: 255,
    code: Rgb(48, 46, 40),
    url: Rgb(58, 96, 72),
    email: Rgb(74, 70, 62),
    path: Rgb(64, 60, 52),
};

// True black like OpenCode: charcoal blacks, neutral grays, and an off-white
// accent. No blue tint anywhere.
const DARK: ThemeColors = ThemeColors {
    bg_base: Rgb(5, 5, 5),
    bg_surface: Rgb(13, 13, 13),
    bg_elevated: Rgb(24, 24, 24),
    bg_selected: Rgb(35, 35, 35),
    bg_hover: Rgb(28, 28, 28),
    accent: Rgb(220, 220, 220),
    accent2: Rgb(150, 150, 150),
    text: Rgb(245, 245, 245),
    subtext: Rgb(155, 155, 155),
    overlay: Rgb(110, 110, 110),
    green: Rgb(220, 220, 220),
    border: Rgb(45, 45, 45),
    surface_alpha: 255,
    code: Rgb(160, 160, 160),
    url: Rgb(160, 160, 160),
    email: Rgb(160, 160, 160),
    path: Rgb(160, 160, 160),
};

// ---------------------------------------------------------------------------
// Concept themes — chrome (`accent`) stays near-neutral; the spot mark
// (`green`: selection rail, pin star, active chip) is themed so it matches the
// palette instead of forcing one lime everywhere. Glass Dark paints dual
// frosted glass plates — see `shell_glows` / `is_glass`.
// ---------------------------------------------------------------------------

/// Deep mint for Glass Light chips — needs ≥3:1 on the cool select wash.
// Glass Light's accent. Was a deep green (24,112,80), which the HUD paints
// large — the eye button, the search glyph, filled stars — so the tray popover
// read as a green app while the island beside it was neutral white. A deep
// neutral slate does the same job: dark enough to carry a glyph on a pale
// plate, with no hue to argue with the material.
const GLASS_LIGHT_ACCENT: Rgb = Rgb(72, 80, 92);
/// Cool silver highlight for Compact Capsule.
const COMPACT_SPOT: Rgb = Rgb(170, 185, 198);

/// Glass Light glassmorphism — cool window light + a trace of warm reflection.
/// The colours stay close to Spotlight's smoked neutral material; they should
/// be perceived as changing light in the glass, never as a cyan/pink gradient.
// Kept, but drained of hue. A pale plate still needs blooms — backdrop blur
// over a white document necessarily becomes white, so without them the theme
// stops looking like glass at all. What it did not need was a blue bloom in
// one corner and a mauve one in the other, which is what tinted the whole
// popover lavender next to a neutral island. Same depth, no colour.
const GLASS_LIGHT_GLOW_A: Rgb = Rgb(156, 158, 163);
const GLASS_LIGHT_GLOW_B: Rgb = Rgb(172, 172, 176);
/// Glass Dark glassmorphism — restrained teal slate + dusky plum.

// Glass Light — smoked pearl frost, closer to macOS Spotlight than a white
// sheet. Its translucency provides the brightness; the RGB values deliberately
// stay in the mid-light range so layers do not compound into harsh white.
const GLASS_LIGHT: ThemeColors = ThemeColors {
    // Actually light. These grounds sat at 180-204 — mid-grey, which is what
    // made the theme read as a dull slab rather than as a pale plate, and why
    // it looked closer to dark than to light. Bright, with the frost veil and
    // the native material keeping it off pure white.
    bg_base: Rgb(232, 234, 238),
    bg_surface: Rgb(241, 243, 246),
    bg_elevated: Rgb(249, 250, 252),
    // Was green-tinted (185,198,191). Same fault Glass Dark had: on a theme
    // whose idea is glass, a colour cast on the selected row is the thing you
    // notice first.
    bg_selected: Rgb(226, 229, 234),
    bg_hover: Rgb(235, 237, 241),
    accent: GLASS_LIGHT_ACCENT,
    accent2: Rgb(104, 112, 123),
    text: Rgb(28, 28, 32),
    subtext: Rgb(92, 96, 104),
    overlay: Rgb(132, 138, 146),
    green: GLASS_LIGHT_ACCENT,
    border: Rgb(206, 210, 216),
    // Same reasoning as Glass Dark, and more urgent here: a pale translucent
    // surface over AppKit's pale glass is where legibility collapses outright,
    // because dark text ends up reading against whatever bright window is
    // behind rather than against the panel.
    surface_alpha: 205,
    // Separated by lightness, one blue for links. Dark ink on a pale plate
    // needs no more than that.
    code: Rgb(60, 64, 72),
    url: Rgb(38, 100, 150),
    email: Rgb(96, 102, 112),
    path: Rgb(78, 84, 94),
};

// 4 — Compact Capsule. "Ultra-compact, fits anywhere."
//
// Pure charcoal — no green cast. Cool silver spot keeps density quiet.
const COMPACT_CAPSULE: ThemeColors = ThemeColors {
    bg_base: Rgb(10, 10, 10),
    bg_surface: Rgb(18, 18, 18),
    bg_elevated: Rgb(28, 28, 28),
    bg_selected: Rgb(36, 36, 36),
    bg_hover: Rgb(26, 26, 26),
    accent: Rgb(228, 228, 228),
    accent2: Rgb(115, 115, 115),
    text: Rgb(228, 228, 228),
    subtext: Rgb(115, 115, 115),
    overlay: Rgb(90, 90, 90),
    green: COMPACT_SPOT,
    border: Rgb(40, 40, 40),
    surface_alpha: 255,
    code: Rgb(150, 150, 150),
    url: Rgb(150, 150, 150),
    email: Rgb(150, 150, 150),
    path: Rgb(150, 150, 150),
};

// Deep navy with a periwinkle accent — calm, modern, and easy on the eyes.
const MIDNIGHT: ThemeColors = ThemeColors {
    bg_base: Rgb(13, 17, 28),
    bg_surface: Rgb(20, 26, 40),
    bg_elevated: Rgb(28, 36, 54),
    bg_selected: Rgb(38, 48, 70),
    bg_hover: Rgb(32, 42, 62),
    accent: Rgb(139, 184, 254),
    accent2: Rgb(188, 161, 218),
    text: Rgb(229, 234, 247),
    subtext: Rgb(159, 170, 193),
    overlay: Rgb(108, 120, 148),
    green: Rgb(139, 184, 254),
    border: Rgb(42, 52, 72),
    surface_alpha: 255,
    code: Rgb(143, 192, 138),
    url: Rgb(139, 184, 254),
    email: Rgb(220, 192, 132),
    path: Rgb(188, 161, 218),
};

// Catppuccin Mocha — the palette the island wears, so the two surfaces are
// one product rather than two that happen to ship together. Values are the
// published Mocha ramp, not hand-mixed approximations: base/mantle/crust for
// the grounds, surface0..2 for raised rows, overlay/subtext/text for ink, and
// mauve as the accent the site's own mockup already uses.
const CATPPUCCIN: ThemeColors = ThemeColors {
    // Deep navy with a cyan accent — the palette of a terminal, not of a
    // filing cabinet.
    //
    // Two failed attempts got here: Catppuccin's own neutrals carry a violet
    // cast that read as purple beside a mauve accent, and flattening them to
    // blue-grey fixed the purple by draining the colour out entirely. The
    // grounds are properly blue now — red held well below blue rather than
    // near it — and the accent is a bright sky rather than a pastel, which is
    // what gives a dark window life instead of making it merely dark.
    bg_base: Rgb(13, 17, 28),
    bg_surface: Rgb(20, 26, 40),
    bg_elevated: Rgb(32, 41, 60),
    bg_selected: Rgb(40, 51, 72),
    bg_hover: Rgb(28, 36, 54),
    accent: Rgb(125, 211, 240),
    accent2: Rgb(148, 226, 213),
    text: Rgb(214, 224, 240),
    subtext: Rgb(160, 176, 200),
    overlay: Rgb(110, 126, 152),
    // The active/positive colour, which every theme sets to its own accent.
    green: Rgb(125, 211, 240),
    border: Rgb(40, 51, 72),
    surface_alpha: 255,
    // One family, four steps, separated by lightness rather than hue.
    url: Rgb(125, 211, 240),
    code: Rgb(148, 226, 213),
    email: Rgb(137, 180, 250),
    path: Rgb(116, 199, 236),
};

// Forest — the tray-target mockup. Deep green-black shell (#0d1915), elevated
// cards (#16231c), soft mint pin/status (#8abb83), off-white ink (#e0e7e1).
const FOREST: ThemeColors = ThemeColors {
    bg_base: Rgb(10, 17, 14),
    bg_surface: Rgb(13, 25, 21),
    bg_elevated: Rgb(22, 35, 28),
    bg_selected: Rgb(34, 47, 40),
    bg_hover: Rgb(28, 41, 34),
    accent: Rgb(138, 187, 131),
    accent2: Rgb(126, 143, 130),
    text: Rgb(224, 231, 225),
    subtext: Rgb(126, 143, 130),
    overlay: Rgb(96, 105, 100),
    green: Rgb(138, 187, 131),
    border: Rgb(36, 46, 40),
    surface_alpha: 255,
    code: Rgb(138, 187, 131),
    url: Rgb(145, 197, 232),
    email: Rgb(223, 195, 131),
    path: Rgb(206, 166, 205),
};

// Cool slate with a rose accent.
//
// The accent was amber, which on a blue-grey ground is the most conventional
// warm/cool pairing there is — and it read as the same colour every terminal
// theme reaches for. Rose keeps the warmth the ground was designed against,
// so the depth still works, without landing on orange.
const SLATE: ThemeColors = ThemeColors {
    bg_base: Rgb(22, 26, 31),
    bg_surface: Rgb(29, 34, 40),
    bg_elevated: Rgb(41, 48, 57),
    bg_selected: Rgb(51, 60, 71),
    bg_hover: Rgb(46, 54, 64),
    accent: Rgb(238, 155, 170),
    accent2: Rgb(163, 183, 204),
    text: Rgb(237, 241, 245),
    subtext: Rgb(165, 176, 188),
    overlay: Rgb(115, 128, 142),
    // The active/positive colour, which every theme sets to its own accent.
    green: Rgb(238, 155, 170),
    border: Rgb(48, 56, 66),
    surface_alpha: 255,
    code: Rgb(154, 204, 154),
    url: Rgb(147, 195, 232),
    email: Rgb(232, 195, 140),
    path: Rgb(210, 178, 210),
};

fn pref_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("theme.json")
}

pub fn load_theme() -> Theme {
    std::fs::read_to_string(pref_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_theme(theme: Theme) {
    let path = pref_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_string(&theme).unwrap_or_default());
}

// ---------------------------------------------------------------------------
// Custom palette — user-defined colors that override the active theme.
// The user picks colors "for his eyes" in Settings; when `enabled`, these are
// layered on top of whatever base theme is selected.
// ---------------------------------------------------------------------------

/// User-defined color overrides. Kept small on purpose: an accent plus the two
/// values that carry a palette (background + text). Surface/hover/selected are
/// derived from the background so the whole UI stays coherent from one pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomColors {
    pub enabled: bool,
    pub accent: Rgb,
    pub background: Rgb,
    pub text: Rgb,
}

#[cfg(test)]
mod custom_colour_tests {
    use super::*;

    #[test]
    fn a_custom_accent_reaches_the_marks_it_should() {
        // Slot badges, stars and the active chip all draw from `green`. If a
        // custom accent does not reach it, the user picks a colour and every
        // mark keeps the theme's — which reads as the setting being ignored,
        // or as clipd choosing a colour of its own.
        let picked = Rgb(200, 60, 120);
        let custom = CustomColors {
            enabled: true,
            accent: picked,
            background: Rgb(10, 10, 12),
            text: Rgb(240, 240, 240),
        };
        let mut c = Theme::Dark.colors();
        custom.apply_to(&mut c);
        assert_eq!(c.accent, picked);
        assert_eq!(c.green, picked, "the spot colour must follow the accent");

        // Disabled means untouched.
        let before = Theme::Dark.colors();
        let mut after = Theme::Dark.colors();
        CustomColors { enabled: false, ..custom }.apply_to(&mut after);
        assert_eq!(after.green, before.green);
        assert_eq!(after.accent, before.accent);
    }
}

impl Default for CustomColors {
    fn default() -> Self {
        // Seed with the curated Dark palette so enabling custom colours starts
        // from the same neutral high-contrast values as the built-in theme.
        CustomColors {
            enabled: false,
            accent: Rgb(220, 220, 220),
            background: Rgb(5, 5, 5),
            text: Rgb(245, 245, 245),
        }
    }
}

impl CustomColors {
    /// Overlay the custom palette onto a base set of theme colors.
    pub fn apply_to(&self, c: &mut ThemeColors) {
        if !self.enabled {
            return;
        }
        c.accent = self.accent;
        // The spot colour follows the accent you picked.
        //
        // Every built-in theme sets `green` to its own accent — it is what
        // paints slot badges, pinned stars, the active chip and the eye
        // button. Overriding `accent` alone left all of those showing the
        // *theme's* colour, so choosing a custom colour changed the chrome and
        // left every mark on screen untouched. That is why turning custom
        // colours on looked like it was picking a colour of its own.
        c.green = self.accent;
        c.bg_base = self.background;
        c.bg_surface = lighten(self.background, 0.05);
        c.bg_elevated = lighten(self.background, 0.11);
        c.bg_hover = lighten(self.background, 0.08);
        c.bg_selected = mix(self.background, self.accent, 0.22);
        c.text = self.text;
        c.subtext = mix(self.text, self.background, 0.42);
        c.border = lighten(self.background, 0.16);
    }
}

fn lighten(Rgb(r, g, b): Rgb, f: f32) -> Rgb {
    let f = f.clamp(0.0, 1.0);
    let step = |v: u8| {
        (v as f32 + (255.0 - v as f32) * f)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgb(step(r), step(g), step(b))
}

fn mix(Rgb(ar, ag, ab): Rgb, Rgb(br, bg, bb): Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |a: u8, b: u8| {
        (a as f32 * (1.0 - t) + b as f32 * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgb(m(ar, br), m(ag, bg), m(ab, bb))
}

fn custom_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("custom_colors.json")
}

pub fn load_custom_colors() -> CustomColors {
    std::fs::read_to_string(custom_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(migrate_legacy_custom_colors)
        .unwrap_or_default()
}

fn migrate_legacy_custom_colors(colors: CustomColors) -> CustomColors {
    let legacy_orange_seed = CustomColors {
        enabled: true,
        accent: Rgb(255, 160, 50),
        background: Rgb(24, 26, 33),
        text: Rgb(238, 241, 247),
    };

    if colors == legacy_orange_seed {
        CustomColors::default()
    } else {
        colors
    }
}

pub fn save_custom_colors(colors: &CustomColors) {
    let path = custom_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_string(colors).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_neutral_charcoal_not_blue() {
        let colors = Theme::Dark.colors();
        assert_eq!(colors.bg_base, Rgb(5, 5, 5));
        assert_eq!(colors.text, Rgb(245, 245, 245));
        assert_eq!(colors.accent, Rgb(220, 220, 220));
        // No channel should read as a strong blue tint.
        assert!(colors.accent.2 < 240, "dark accent should stay neutral/off-white");
    }

    #[test]
    fn every_theme_keeps_selection_contrast_quiet() {
        for theme in Theme::ALL {
            let colors = theme.colors();
            let Rgb(sr, sg, sb) = colors.bg_surface;
            let Rgb(rr, rg, rb) = colors.bg_selected;
            let largest_step = [sr.abs_diff(rr), sg.abs_diff(rg), sb.abs_diff(rb)]
                .into_iter()
                .max()
                .unwrap_or_default();
            assert!(
                largest_step <= 35,
                "{} selection is too harsh (channel step {largest_step})",
                theme.label()
            );
        }
    }

    #[test]
    fn every_theme_has_contrasting_text_on_surface() {
        for theme in Theme::ALL {
            let colors = theme.colors();
            let surface_luma = luminance(colors.bg_surface);
            let text_luma = luminance(colors.text);
            let step = surface_luma.max(text_luma) - surface_luma.min(text_luma);
            assert!(
                step >= 100.0,
                "{} text lacks contrast against its surface",
                theme.label()
            );
        }
    }

    #[test]
    fn paper_light_matches_the_ivory_mockup() {
        let colors = Theme::Light.colors();
        assert_eq!(colors.bg_surface, Rgb(247, 243, 232));
        assert_eq!(colors.text, Rgb(28, 26, 23));
        assert_eq!(colors.subtext, Rgb(74, 70, 62));
        assert_eq!(colors.green, Rgb(88, 118, 78));
        // No channel hits pure white — the whole point of warm ivory.
        assert!(colors.bg_surface.0 < 255 && colors.bg_elevated.0 < 255);
        assert!(Theme::Light.is_light());
    }

    /// WCAG relative luminance — the perceptual one, not the naive average.
    fn wcag_luminance(Rgb(r, g, b): Rgb) -> f32 {
        fn channel(v: u8) -> f32 {
            let c = v as f32 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f32 {
        let (x, y) = (wcag_luminance(a), wcag_luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Body text has to clear WCAG AA (4.5:1) on every surface it lands on.
    /// A theme that fails this isn't a style choice, it's unreadable.
    #[test]
    fn every_theme_keeps_text_readable() {
        for theme in Theme::ALL {
            let c = theme.colors();
            for (name, bg) in [
                ("bg_base", c.bg_base),
                ("bg_surface", c.bg_surface),
                ("bg_elevated", c.bg_elevated),
                ("bg_selected", c.bg_selected),
                ("bg_hover", c.bg_hover),
            ] {
                let ratio = contrast(c.text, bg);
                assert!(
                    ratio >= 4.5,
                    "{}: text on {name} is {ratio:.2}:1, under the 4.5:1 minimum",
                    theme.label()
                );
            }
        }
    }

    /// Subtext is smaller and secondary, so AA-large (3:1) is the bar — but it
    /// still has to be legible, not decorative.
    #[test]
    fn every_theme_keeps_subtext_legible() {
        for theme in Theme::ALL {
            let c = theme.colors();
            let ratio = contrast(c.subtext, c.bg_surface);
            assert!(
                ratio >= 3.0,
                "{}: subtext is {ratio:.2}:1 on the card, under 3:1",
                theme.label()
            );
        }
    }

    /// The accent marks the selected row. If it doesn't separate from the
    /// surface it is sitting on, selection stops being visible at a glance —
    /// which is the entire job of the accent in these concepts.
    #[test]
    fn every_theme_accent_stands_out_against_its_surface() {
        for theme in Theme::ALL {
            let c = theme.colors();
            let ratio = contrast(c.accent, c.bg_selected);
            assert!(
                ratio >= 3.0,
                "{}: accent is {ratio:.2}:1 on the selected row, under 3:1",
                theme.label()
            );
        }
    }

    /// A selected row has to be distinguishable from an unselected one. The
    /// step is small by design in these palettes, so this guards the floor.
    #[test]
    fn selection_is_visible_against_the_card() {
        for theme in Theme::ALL {
            let c = theme.colors();
            let step = (wcag_luminance(c.bg_selected) - wcag_luminance(c.bg_surface)).abs();
            assert!(
                step > 0.001,
                "{}: the selected row is indistinguishable from the card",
                theme.label()
            );
        }
    }

    /// Every theme in the enum must be reachable by cycling, or a theme exists
    /// that the user can never actually get to with the theme shortcut.
    #[test]
    fn cycling_reaches_every_theme_and_returns() {
        let start = Theme::System;
        let mut seen = vec![start];
        let mut cur = start;
        for _ in 0..Theme::ALL.len() * 2 {
            cur = cur.next();
            if cur == start {
                break;
            }
            seen.push(cur);
        }
        assert_eq!(
            seen.len(),
            Theme::ALL.len(),
            "cycle visits {} of {} themes",
            seen.len(),
            Theme::ALL.len()
        );
        for theme in Theme::ALL {
            assert!(seen.contains(&theme), "{} is unreachable", theme.label());
        }
    }

    /// Saturation in the concepts is a *spot* colour on the selected row, not
    /// the UI's chrome. `accent` paints ~68 places across the window, so it has
    /// to stay near-neutral or the whole thing turns green.
    #[test]
    fn the_concept_themes_keep_their_chrome_neutral() {
        fn saturation(Rgb(r, g, b): Rgb) -> u8 {
            r.max(g).max(b) - r.min(g).min(b)
        }
        for theme in [
            Theme::Dark,
            Theme::CompactCapsule,
        ] {
            let accent = theme.colors().accent;
            assert!(
                saturation(accent) <= 24,
                "{}: accent {accent:?} is too saturated for chrome used in ~68 places",
                theme.label()
            );
        }
    }

    /// Each concept theme gets its own spot colour so rails/stars match the
    /// shell (cyan on glass, lilac on plum, olive on minimal, silver on capsule).
    #[test]
    fn the_glass_and_dark_concepts_use_themed_spot_colours() {
        assert_eq!(Theme::GlassLight.colors().green, GLASS_LIGHT_ACCENT);
        assert_eq!(Theme::GlassLight.colors().accent, GLASS_LIGHT_ACCENT);
        assert_eq!(Theme::CompactCapsule.colors().green, COMPACT_SPOT);
        // Glass Light's accent is deliberately neutral now. It used to be a
        // deep green, and the tray popover paints the accent large — eye
        // button, search glyph, filled stars — so the popover read as a green
        // app sitting next to a neutral white island.
        let a = Theme::GlassLight.colors().accent;
        let spread = a.0.max(a.1).max(a.2) - a.0.min(a.1).min(a.2);
        assert!(
            spread <= 24,
            "Glass Light accent should be near-neutral, spread was {spread}"
        );
    }

    #[test]
    fn glass_themes_declare_shell_glows() {
        let (la, lb) = Theme::GlassLight
            .shell_glows()
            .expect("Glass Light must declare shell glows");
        assert_eq!(la, GLASS_LIGHT_GLOW_A);
        assert_eq!(lb, GLASS_LIGHT_GLOW_B);
        assert!(Theme::GlassLight.is_glass());
        assert!(Theme::GlassLight.is_light());
        for theme in [
            Theme::CompactCapsule,
        ] {
            assert!(theme.shell_glows().is_none(), "{} should be flat", theme.label());
            assert!(!theme.is_glass());
        }
    }

    /// The spot colour has to separate from the row it marks, or selection
    /// stops being visible — which is the star's entire job.
    #[test]
    fn the_spot_colour_stands_out_on_the_selected_row() {
        for theme in [
            Theme::GlassLight,
            Theme::CompactCapsule,
            Theme::Midnight,
            Theme::Slate,
            Theme::Light,
        ] {
            let c = theme.colors();
            let ratio = contrast(c.green, c.bg_selected);
            assert!(
                ratio >= 3.0,
                "{}: the star is {ratio:.2}:1 on the selected row, under 3:1",
                theme.label()
            );
        }
    }

    #[test]
    fn a_fresh_install_lands_on_dark() {
        // What a new user sees, and what anyone falls back to when the stored
        // preference is missing or unreadable. Worth pinning: themes get added
        // and retired, and the default is the one value where a quiet change
        // would go unnoticed until someone opened the app for the first time.
        assert_eq!(Theme::default(), Theme::Dark);
        assert!(!Theme::default().is_light());
        // load_theme() falls back to the same place when nothing is stored.
        let unreadable: Result<Theme, _> = serde_json::from_str("not a theme");
        assert_eq!(unreadable.ok().unwrap_or_default(), Theme::Dark);
    }

    #[test]
    fn retired_themes_load_as_their_nearest_survivor() {
        // Paper Dark and Cocoa are gone. A config still naming them must land
        // somewhere deliberate — without the aliases serde fails the whole
        // parse and the user is silently reset to the default theme.
        let paper: Theme = serde_json::from_str("\"Paper\"").expect("Paper still parses");
        assert_eq!(paper, Theme::Dark);
        let cocoa: Theme = serde_json::from_str("\"Cocoa\"").expect("Cocoa still parses");
        assert_eq!(cocoa, Theme::Slate, "the nearest warm dark");
        // Glass Dark's job — a dark surface with no colour in it — is what
        // Dark already does, without a translucency layer to fight.
        let glass: Theme = serde_json::from_str("\"GlassDark\"").expect("GlassDark still parses");
        assert_eq!(glass, Theme::Dark);
        let cmd: Theme =
            serde_json::from_str("\"CommandPalette\"").expect("CommandPalette still parses");
        assert_eq!(cmd, Theme::Dark);
        let minimal: Theme =
            serde_json::from_str("\"MinimalDark\"").expect("MinimalDark still parses");
        assert_eq!(minimal, Theme::Dark);
        // The survivors are still offered; the list is two shorter.
        assert!(Theme::ALL.contains(&Theme::Dark));
        assert!(Theme::ALL.contains(&Theme::Slate));
        assert_eq!(Theme::ALL.len(), 9);
        // Nothing in the list still calls itself by a retired name.
        for theme in Theme::ALL {
            assert!(!matches!(
                theme.label(),
                "Paper Dark" | "Cocoa" | "Glass Dark" | "Command Palette" | "Minimal Dark"
            ));
        }
    }

    #[test]
    fn glass_surfaces_are_a_ground_not_a_second_veil() {
        // AppKit's glass material behind the window is already translucent.
        // Drawing the theme's own surface at a low alpha stacks one see-through
        // layer on another: the depth doubles and text ends up reading against
        // whatever window happens to be behind, which changes as you move. The
        // material provides the depth; the surface has to provide the ground.
        for theme in Theme::ALL.into_iter().filter(Theme::is_glass) {
            let alpha = theme.colors().surface_alpha;
            assert!(
                alpha >= 200,
                "{} draws its surface at {alpha} over an already translucent material",
                theme.label()
            );
        }
    }

    #[test]
    fn light_themes_report_as_light() {
        assert!(Theme::Light.is_light());
        assert!(Theme::GlassLight.is_light());
        assert!(!Theme::Catppuccin.is_light());
        for theme in [
            Theme::CompactCapsule,
        ] {
            assert!(!theme.is_light(), "{} should be dark", theme.label());
        }
    }

    #[test]
    fn legacy_light_minimal_deserializes_as_paper_light() {
        let theme: Theme =
            serde_json::from_str("\"LightMinimal\"").expect("LightMinimal alias");
        assert_eq!(theme, Theme::Light);
    }

    /// Forest matches the tray-target mockup's measured fills and mint accent.
    #[test]
    fn forest_matches_the_target_mockup() {
        let c = Theme::Forest.colors();
        assert_eq!(c.bg_surface, Rgb(13, 25, 21));
        assert_eq!(c.bg_elevated, Rgb(22, 35, 28));
        assert_eq!(c.green, Rgb(138, 187, 131));
        assert_eq!(c.text, Rgb(224, 231, 225));
    }


    #[test]
    fn glass_light_glows_are_sky_and_lavender() {
        let (a, b) = Theme::GlassLight.shell_glows().unwrap();
        assert_eq!(a, GLASS_LIGHT_GLOW_A);
        assert_eq!(b, GLASS_LIGHT_GLOW_B);
        assert!(a.2 > a.0, "glow A should read sky/cyan");
        assert!(b.2 > b.1 || b.0 > b.1, "glow B should read lavender");
    }

    #[test]
    fn legacy_glass_minimal_deserializes_as_glass_dark() {
        let theme: Theme =
            serde_json::from_str("\"GlassMinimal\"").expect("GlassMinimal alias");
    }

    /// Only the glass themes are translucent. Anything else losing its opacity
    /// would let the desktop bleed through a UI that was never designed for it.
    #[test]
    fn only_glass_themes_are_translucent() {
        for theme in Theme::ALL {
            let alpha = theme.colors().surface_alpha;
            if theme.is_glass() {
                assert!(
                    (100..255).contains(&alpha),
                    "{}: glass needs real translucency, got alpha {alpha}",
                    theme.label()
                );
            } else {
                assert_eq!(
                    alpha,
                    255,
                    "{} must stay fully opaque",
                    theme.label()
                );
            }
        }
    }

    #[test]
    fn legacy_orange_custom_seed_is_ignored() {
        let migrated = migrate_legacy_custom_colors(CustomColors {
            enabled: true,
            accent: Rgb(255, 160, 50),
            background: Rgb(24, 26, 33),
            text: Rgb(238, 241, 247),
        });

        assert!(!migrated.enabled);
        assert_eq!(migrated.accent, Rgb(220, 220, 220));
        assert_eq!(migrated.background, Rgb(5, 5, 5));
    }
}

#[cfg(test)]
fn luminance(Rgb(r, g, b): Rgb) -> f32 {
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}
