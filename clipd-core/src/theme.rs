use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Catppuccin,
    Monokai,
    Dark,
    Nord,
    Dracula,
}

impl Theme {
    pub const ALL: [Theme; 5] = [
        Theme::Catppuccin,
        Theme::Monokai,
        Theme::Dark,
        Theme::Nord,
        Theme::Dracula,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Theme::Catppuccin => "Catppuccin",
            Theme::Monokai => "Monokai",
            Theme::Dark => "Dark",
            Theme::Nord => "Nord",
            Theme::Dracula => "Dracula",
        }
    }

    pub fn next(&self) -> Theme {
        match self {
            Theme::Catppuccin => Theme::Monokai,
            Theme::Monokai => Theme::Dark,
            Theme::Dark => Theme::Nord,
            Theme::Nord => Theme::Dracula,
            Theme::Dracula => Theme::Catppuccin,
        }
    }

    pub fn colors(&self) -> ThemeColors {
        match self {
            Theme::Catppuccin => CATPPUCCIN,
            Theme::Monokai => MONOKAI,
            Theme::Dark => DARK,
            Theme::Nord => NORD,
            Theme::Dracula => DRACULA,
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

const MONOKAI: ThemeColors = ThemeColors {
    bg_base: Rgb(45, 42, 46),
    bg_surface: Rgb(34, 32, 36),
    bg_elevated: Rgb(62, 58, 65),
    bg_selected: Rgb(82, 76, 88),
    bg_hover: Rgb(68, 64, 72),
    accent: Rgb(255, 216, 102),
    accent2: Rgb(171, 157, 242),
    text: Rgb(252, 252, 248),
    subtext: Rgb(200, 196, 192),
    overlay: Rgb(145, 140, 135),
    green: Rgb(169, 220, 118),
    border: Rgb(72, 68, 76),
    code: Rgb(169, 220, 118),
    url: Rgb(120, 220, 232),
    email: Rgb(252, 152, 103),
    path: Rgb(171, 157, 242),
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
