use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Catppuccin,
    Monokai,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Catppuccin, Theme::Monokai, Theme::Dark];

    pub fn label(&self) -> &'static str {
        match self {
            Theme::Catppuccin => "Catppuccin",
            Theme::Monokai => "Monokai",
            Theme::Dark => "Dark",
        }
    }

    pub fn next(&self) -> Theme {
        match self {
            Theme::Catppuccin => Theme::Monokai,
            Theme::Monokai => Theme::Dark,
            Theme::Dark => Theme::Catppuccin,
        }
    }

    pub fn colors(&self) -> ThemeColors {
        match self {
            Theme::Catppuccin => CATPPUCCIN,
            Theme::Monokai => MONOKAI,
            Theme::Dark => DARK,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Catppuccin
    }
}

#[derive(Debug, Clone, Copy)]
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

const CATPPUCCIN: ThemeColors = ThemeColors {
    bg_base:     Rgb(30, 30, 46),
    bg_surface:  Rgb(24, 24, 37),
    bg_elevated: Rgb(49, 50, 68),
    bg_selected: Rgb(69, 71, 90),
    bg_hover:    Rgb(49, 50, 68),
    accent:      Rgb(137, 180, 250),
    accent2:     Rgb(203, 166, 247),
    text:        Rgb(205, 214, 244),
    subtext:     Rgb(166, 173, 200),
    overlay:     Rgb(108, 112, 134),
    green:       Rgb(166, 227, 161),
    border:      Rgb(69, 71, 90),
    code:        Rgb(166, 227, 161),
    url:         Rgb(116, 199, 236),
    email:       Rgb(249, 226, 175),
    path:        Rgb(203, 166, 247),
};

const MONOKAI: ThemeColors = ThemeColors {
    bg_base:     Rgb(39, 40, 34),
    bg_surface:  Rgb(30, 31, 28),
    bg_elevated: Rgb(62, 61, 50),
    bg_selected: Rgb(78, 77, 66),
    bg_hover:    Rgb(55, 54, 44),
    accent:      Rgb(253, 151, 31),
    accent2:     Rgb(174, 129, 255),
    text:        Rgb(248, 248, 242),
    subtext:     Rgb(188, 182, 162),
    overlay:     Rgb(117, 113, 94),
    green:       Rgb(166, 226, 46),
    border:      Rgb(85, 84, 72),
    code:        Rgb(166, 226, 46),
    url:         Rgb(102, 217, 239),
    email:       Rgb(230, 219, 116),
    path:        Rgb(174, 129, 255),
};

const DARK: ThemeColors = ThemeColors {
    bg_base:     Rgb(18, 18, 18),
    bg_surface:  Rgb(24, 24, 24),
    bg_elevated: Rgb(38, 38, 38),
    bg_selected: Rgb(55, 55, 55),
    bg_hover:    Rgb(42, 42, 42),
    accent:      Rgb(130, 170, 255),
    accent2:     Rgb(180, 140, 255),
    text:        Rgb(212, 212, 212),
    subtext:     Rgb(156, 156, 156),
    overlay:     Rgb(100, 100, 100),
    green:       Rgb(100, 200, 110),
    border:      Rgb(50, 50, 50),
    code:        Rgb(100, 200, 110),
    url:         Rgb(100, 180, 240),
    email:       Rgb(240, 200, 100),
    path:        Rgb(180, 140, 255),
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
