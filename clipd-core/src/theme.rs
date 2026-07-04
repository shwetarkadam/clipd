use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    System,
    Light,
    Catppuccin,
    Monokai,
    Dark,
    Nord,
    Dracula,
}

impl Theme {
    pub const ALL: [Theme; 7] = [
        Theme::System,
        Theme::Light,
        Theme::Dark,
        Theme::Catppuccin,
        Theme::Monokai,
        Theme::Nord,
        Theme::Dracula,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Catppuccin => "Catppuccin",
            Theme::Monokai => "Monokai",
            Theme::Dark => "Dark",
            Theme::Nord => "Nord",
            Theme::Dracula => "Dracula",
        }
    }

    pub fn next(&self) -> Theme {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Catppuccin,
            Theme::Catppuccin => Theme::Monokai,
            Theme::Monokai => Theme::Nord,
            Theme::Nord => Theme::Dracula,
            Theme::Dracula => Theme::System,
        }
    }

    pub fn colors(&self) -> ThemeColors {
        match self {
            Theme::System => DARK,
            Theme::Light => LIGHT,
            Theme::Catppuccin => CATPPUCCIN,
            Theme::Monokai => MONOKAI,
            Theme::Dark => DARK,
            Theme::Nord => NORD,
            Theme::Dracula => DRACULA,
        }
    }

    pub fn is_light(&self) -> bool {
        matches!(self, Theme::Light)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Monokai
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

    pub code: Rgb,
    pub url: Rgb,
    pub email: Rgb,
    pub path: Rgb,
}

// Clean Raycast/Spotlight-style light theme: near-white surface, crisp
// near-black text, hairline borders, subtle neutral selection.
const LIGHT: ThemeColors = ThemeColors {
    bg_base: Rgb(250, 250, 252),
    bg_surface: Rgb(255, 255, 255),
    bg_elevated: Rgb(245, 246, 248),
    bg_selected: Rgb(236, 237, 240),
    bg_hover: Rgb(243, 244, 246),
    accent: Rgb(10, 122, 255),
    accent2: Rgb(120, 90, 190),
    text: Rgb(26, 27, 30),
    subtext: Rgb(98, 102, 112),
    overlay: Rgb(150, 154, 163),
    green: Rgb(30, 150, 90),
    border: Rgb(228, 230, 235),
    code: Rgb(28, 140, 85),
    url: Rgb(18, 115, 205),
    email: Rgb(168, 110, 0),
    path: Rgb(120, 90, 190),
};

const CATPPUCCIN: ThemeColors = ThemeColors {
    bg_base: Rgb(30, 30, 46),
    bg_surface: Rgb(24, 24, 37),
    bg_elevated: Rgb(49, 50, 68),
    bg_selected: Rgb(69, 71, 90),
    bg_hover: Rgb(55, 56, 75),
    accent: Rgb(137, 180, 250),
    accent2: Rgb(203, 166, 247),
    text: Rgb(205, 214, 244),
    subtext: Rgb(186, 194, 222),
    overlay: Rgb(127, 132, 156),
    green: Rgb(166, 227, 161),
    border: Rgb(69, 71, 90),
    code: Rgb(166, 227, 161),
    url: Rgb(116, 199, 236),
    email: Rgb(249, 226, 175),
    path: Rgb(203, 166, 247),
};

// Authentic Sublime/Monokai palette: #272822 base, #F8F8F2 text, and the
// signature green (#A6E22E) as the accent/highlight — never orange.
const MONOKAI: ThemeColors = ThemeColors {
    bg_base: Rgb(39, 40, 34),
    bg_surface: Rgb(30, 31, 26),
    bg_elevated: Rgb(62, 61, 50),
    bg_selected: Rgb(73, 72, 62),
    bg_hover: Rgb(54, 55, 45),
    accent: Rgb(166, 226, 46),
    accent2: Rgb(174, 129, 255),
    text: Rgb(248, 248, 242),
    subtext: Rgb(190, 189, 175),
    overlay: Rgb(117, 113, 94),
    green: Rgb(166, 226, 46),
    border: Rgb(73, 72, 62),
    code: Rgb(166, 226, 46),
    url: Rgb(102, 217, 239),
    email: Rgb(230, 219, 116),
    path: Rgb(174, 129, 255),
};

const DARK: ThemeColors = ThemeColors {
    bg_base: Rgb(22, 22, 30),
    bg_surface: Rgb(16, 16, 23),
    bg_elevated: Rgb(36, 36, 48),
    bg_selected: Rgb(55, 55, 75),
    bg_hover: Rgb(44, 44, 58),
    accent: Rgb(100, 160, 255),
    accent2: Rgb(190, 150, 255),
    text: Rgb(230, 232, 240),
    subtext: Rgb(175, 178, 195),
    overlay: Rgb(120, 122, 140),
    green: Rgb(80, 210, 120),
    border: Rgb(50, 50, 65),
    code: Rgb(80, 210, 120),
    url: Rgb(100, 190, 255),
    email: Rgb(255, 210, 100),
    path: Rgb(190, 150, 255),
};

const NORD: ThemeColors = ThemeColors {
    bg_base: Rgb(46, 52, 64),
    bg_surface: Rgb(36, 41, 51),
    bg_elevated: Rgb(59, 66, 82),
    bg_selected: Rgb(76, 86, 106),
    bg_hover: Rgb(67, 76, 94),
    accent: Rgb(136, 192, 208),
    accent2: Rgb(180, 142, 173),
    text: Rgb(236, 239, 244),
    subtext: Rgb(200, 206, 216),
    overlay: Rgb(140, 148, 165),
    green: Rgb(163, 190, 140),
    border: Rgb(67, 76, 94),
    code: Rgb(163, 190, 140),
    url: Rgb(129, 161, 193),
    email: Rgb(235, 203, 139),
    path: Rgb(180, 142, 173),
};

const DRACULA: ThemeColors = ThemeColors {
    bg_base: Rgb(40, 42, 54),
    bg_surface: Rgb(30, 31, 42),
    bg_elevated: Rgb(55, 58, 74),
    bg_selected: Rgb(68, 71, 90),
    bg_hover: Rgb(60, 63, 80),
    accent: Rgb(139, 233, 253),
    accent2: Rgb(189, 147, 249),
    text: Rgb(248, 248, 242),
    subtext: Rgb(210, 210, 206),
    overlay: Rgb(150, 150, 148),
    green: Rgb(80, 250, 123),
    border: Rgb(68, 71, 90),
    code: Rgb(80, 250, 123),
    url: Rgb(139, 233, 253),
    email: Rgb(241, 250, 140),
    path: Rgb(189, 147, 249),
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

impl Default for CustomColors {
    fn default() -> Self {
        // Seed with the authentic Monokai palette so enabling or resetting
        // custom colors does not turn the Monokai theme orange.
        CustomColors {
            enabled: false,
            accent: Rgb(166, 226, 46),
            background: Rgb(39, 40, 34),
            text: Rgb(248, 248, 242),
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
    fn monokai_uses_authentic_green_accent() {
        let colors = Theme::Monokai.colors();
        assert_eq!(colors.bg_base, Rgb(39, 40, 34));
        assert_eq!(colors.text, Rgb(248, 248, 242));
        assert_eq!(colors.accent, Rgb(166, 226, 46));
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
        assert_eq!(migrated.accent, Rgb(166, 226, 46));
    }
}
