//! The colour palette every surface is drawn from.
//!
//! A *palette* is nine named colours — nine "roles" rather than nine
//! arbitrary swatches. Drawing code asks for the role it means
//! (`theme.accent` for something that should stand out, `theme.muted` for a
//! hint nobody needs to read) instead of naming a colour directly. That is
//! what makes a theme switch a one-line change: swap the palette and every
//! surface follows, because nothing anywhere says "cyan".
//!
//! The nine roles:
//!
//! | role         | what it is for                                       |
//! |--------------|------------------------------------------------------|
//! | `background` | the page behind everything                           |
//! | `surface`    | a pane raised above that page                        |
//! | `foreground` | ordinary readable text                               |
//! | `muted`      | text you can ignore — hints, timestamps, placeholders |
//! | `border`     | the lines around panes                               |
//! | `accent`     | the one colour that draws the eye                    |
//! | `warning`    | something needs attention but nothing is broken      |
//! | `error`      | something is broken                                  |
//! | `success`    | something worked                                     |
//!
//! There are 57 built-in presets. Most reproduce a scheme people already know
//! (Nord, Dracula, Gruvbox, the four Catppuccin flavours, Solarized, Tokyo
//! Night, …); a handful are authored here. Upstream schemes rarely publish all
//! nine roles — almost none name a border colour — so where a role has no
//! published value the nearest tone from that scheme's own ramp is reused
//! rather than a new hue being invented. Light schemes map `background` to the
//! lightest base and `surface` one step up, because the interface derives its
//! canvas by *darkening* the background, which keeps panes reading as raised
//! in light and dark schemes alike.

use ratatui::style::Color;
use std::collections::BTreeMap;

/// The nine colour roles that every surface is drawn from.
///
/// Colours are stored as `#rrggbb` strings rather than as parsed values
/// because that is the form the config file, the preset table and any custom
/// palette a user writes all use. [`Palette::color`] converts on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub muted: String,
    pub border: String,
    pub surface: String,
    pub warning: String,
    pub error: String,
    pub success: String,
}

impl Default for Palette {
    fn default() -> Self {
        default_palette()
    }
}

/// The nine roles in the order the theme picker draws its swatches.
pub const ROLES: [&str; 9] = [
    "background",
    "foreground",
    "accent",
    "muted",
    "border",
    "surface",
    "warning",
    "error",
    "success",
];

impl Palette {
    /// Read a role by name, for the swatch strip in the theme picker.
    pub fn role(&self, name: &str) -> &str {
        match name {
            "background" => &self.background,
            "foreground" => &self.foreground,
            "accent" => &self.accent,
            "muted" => &self.muted,
            "border" => &self.border,
            "surface" => &self.surface,
            "warning" => &self.warning,
            "error" => &self.error,
            "success" => &self.success,
            _ => &self.foreground,
        }
    }

    /// The canvas behind every pane: the background, darkened a little.
    ///
    /// Panes are painted in `surface`, so darkening the page they sit on is
    /// what makes them read as raised rather than as a flat wash of one
    /// colour. The amount is small enough that a near-black background stays
    /// near-black instead of clipping to pure black.
    pub fn canvas(&self) -> String {
        darken(&self.background, CANVAS_DARKEN_AMOUNT)
    }
}

/// How much [`Palette::canvas`] darkens the background.
pub const CANVAS_DARKEN_AMOUNT: f64 = 0.35;

/// Convert an `#rrggbb` string into a colour ratatui can draw with.
///
/// Anything unparseable becomes [`Color::Reset`], which tells the terminal to
/// use its own default. A typo in a hand-written custom palette therefore
/// costs one colour, not a crash and not an unreadable screen.
pub fn color(hex: &str) -> Color {
    match parse_hex(hex) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => Color::Reset,
    }
}

/// The nine roles as colours ratatui can draw with, plus the few blends the
/// interface needs often enough to be worth working out once.
///
/// This is what drawing code actually reads. It is `Copy` and holds no
/// strings, so reading it costs nothing per span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skin {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
    pub muted: Color,
    pub border: Color,
    pub surface: Color,
    pub warning: Color,
    pub error: Color,
    pub success: Color,
    /// The page behind every pane — the background, darkened.
    pub canvas: Color,
    /// A readable text colour to sit on top of `accent`, for the selected tab
    /// and other reversed labels. Worked out from the accent's own brightness
    /// rather than assumed, so a pale accent gets dark text on it and a dark
    /// accent gets light text.
    pub on_accent: Color,
    /// A faint tint of the accent, for the highlighted row in a list.
    pub selection: Color,
}

impl Skin {
    /// Work out a skin from a palette.
    pub fn from_palette(palette: &Palette) -> Self {
        let canvas = palette.canvas();
        let on_accent =
            contrast_corrected(&palette.background, &palette.accent, &palette.foreground);
        Self {
            background: color(&palette.background),
            foreground: color(&palette.foreground),
            accent: color(&palette.accent),
            muted: color(&palette.muted),
            border: color(&palette.border),
            surface: color(&palette.surface),
            warning: color(&palette.warning),
            error: color(&palette.error),
            success: color(&palette.success),
            canvas: color(&canvas),
            on_accent: color(&on_accent),
            selection: color(&mix(&palette.surface, &palette.accent, 0.25)),
        }
    }
}

impl Default for Skin {
    fn default() -> Self {
        Self::from_palette(&default_palette())
    }
}

/// The skin every surface is currently drawn from.
///
/// Deliberately one shared value rather than a parameter threaded through
/// every drawing function. Three separate modules draw parts of the screen,
/// and a colour is read in a few hundred places across them; passing a
/// palette into every one of those would be a great deal of plumbing for a
/// value that is the same everywhere by definition. The trade is real but
/// narrow: this is written twice in a run (at start-up, and again when the
/// theme changes) and read only while drawing a frame, so no reader ever sees
/// a half-changed theme.
static ACTIVE_SKIN: std::sync::RwLock<Option<Skin>> = std::sync::RwLock::new(None);

/// The skin to draw with.
pub fn skin() -> Skin {
    match ACTIVE_SKIN.read() {
        Ok(guard) => guard.unwrap_or_default(),
        // A poisoned lock means a thread panicked mid-write. The colour to
        // draw with is not worth propagating a panic over: fall back to the
        // default palette and let the real failure surface where it happened.
        Err(_) => Skin::default(),
    }
}

/// Change the skin every subsequent frame is drawn from.
pub fn set_active(palette: &Palette) {
    if let Ok(mut guard) = ACTIVE_SKIN.write() {
        *guard = Some(Skin::from_palette(palette));
    }
}

/// The escape sequence that tells the terminal emulator to use `hex` as its
/// own background colour.
///
/// This is OSC 11 ("operating system command 11"), a message to the terminal
/// program itself rather than something drawn into a cell. The difference
/// matters: this program paints the cells it draws, but a terminal window is
/// usually taller and wider than the content in it, and every cell it has not
/// drawn keeps the terminal's own colour. Without this, a light theme leaves
/// a dark frame around itself wherever the layout does not reach.
///
/// It has to be undone on exit — see [`RESET_BACKGROUND_SEQUENCE`] — or the
/// colour would be left behind in the shell the program was started from.
pub fn background_sequence(hex: &str) -> String {
    format!("\x1b]11;{hex}\x07")
}

/// The sequence that gives the terminal its own background colour back.
pub const RESET_BACKGROUND_SEQUENCE: &str = "\x1b]111\x07";

/// The name of the palette used when the config names none, and the fallback
/// when it names one that does not exist.
pub const DEFAULT_PRESET: &str = "claude";

/// The built-in palettes, as `(name, [the nine roles in `ROLES` order])`.
///
/// A flat table rather than a map so the picker can list them in a stable,
/// deliberate order: the authored themes first, then the well-known dark
/// schemes, then light schemes, then the louder decorative ones.
const PRESETS: &[(&str, [&str; 9])] = &[
    (
        "claude",
        [
            "#1a1523", "#f2ede4", "#d97757", "#948f9c", "#4a4358", "#241d30", "#e0a72e", "#e0685a",
            "#7fbf8e",
        ],
    ),
    (
        "codex",
        [
            "#0d1117", "#e6edf3", "#3fb950", "#8b949e", "#30363d", "#161b22", "#d29922", "#f85149",
            "#3fb950",
        ],
    ),
    (
        "btop",
        [
            "#000000", "#d3d3d3", "#00ff00", "#5a5a5a", "#3a3a3a", "#101010", "#ffdd33", "#ff3333",
            "#00ff00",
        ],
    ),
    (
        "nord",
        [
            "#2e3440", "#eceff4", "#88c0d0", "#4c566a", "#3b4252", "#3b4252", "#ebcb8b", "#bf616a",
            "#a3be8c",
        ],
    ),
    (
        "dracula",
        [
            "#282a36", "#f8f8f2", "#bd93f9", "#6272a4", "#44475a", "#343746", "#f1fa8c", "#ff5555",
            "#50fa7b",
        ],
    ),
    (
        "gruvbox",
        [
            "#282828", "#ebdbb2", "#fe8019", "#928374", "#3c3836", "#32302f", "#fabd2f", "#fb4934",
            "#b8bb26",
        ],
    ),
    (
        "solarized-dark",
        [
            "#002b36", "#839496", "#268bd2", "#586e75", "#073642", "#073642", "#b58900", "#dc322f",
            "#859900",
        ],
    ),
    (
        "monokai",
        [
            "#272822", "#f8f8f2", "#f92672", "#75715e", "#3e3d32", "#3e3d32", "#e6db74", "#f92672",
            "#a6e22e",
        ],
    ),
    (
        "one-dark",
        [
            "#282c34", "#abb2bf", "#61afef", "#5c6370", "#3e4451", "#21252b", "#e5c07b", "#e06c75",
            "#98c379",
        ],
    ),
    (
        "tokyo-night",
        [
            "#1a1b26", "#c0caf5", "#7aa2f7", "#565f89", "#414868", "#24283b", "#e0af68", "#f7768e",
            "#9ece6a",
        ],
    ),
    (
        "catppuccin-mocha",
        [
            "#1e1e2e", "#cdd6f4", "#cba6f7", "#a6adc8", "#45475a", "#313244", "#f9e2af", "#f38ba8",
            "#a6e3a1",
        ],
    ),
    (
        "rose-pine",
        [
            "#191724", "#e0def4", "#c4a7e7", "#6e6a86", "#403d52", "#26233a", "#f6c177", "#eb6f92",
            "#31748f",
        ],
    ),
    (
        "mono",
        [
            "#000000", "#ffffff", "#ffffff", "#808080", "#808080", "#1a1a1a", "#c0c0c0", "#ffffff",
            "#ffffff",
        ],
    ),
    (
        "catppuccin-macchiato",
        [
            "#24273a", "#cad3f5", "#c6a0f6", "#a5adcb", "#494d64", "#363a4f", "#eed49f", "#ed8796",
            "#a6da95",
        ],
    ),
    (
        "catppuccin-frappe",
        [
            "#303446", "#c6d0f5", "#ca9ee6", "#a5adce", "#51576d", "#414559", "#e5c890", "#e78284",
            "#a6d189",
        ],
    ),
    (
        "catppuccin-latte",
        [
            "#eff1f5", "#4c4f69", "#8839ef", "#6c6f85", "#ccd0da", "#e6e9ef", "#df8e1d", "#d20f39",
            "#40a02b",
        ],
    ),
    (
        "rose-pine-moon",
        [
            "#232136", "#e0def4", "#c4a7e7", "#6e6a86", "#44415a", "#2a273f", "#f6c177", "#eb6f92",
            "#3e8fb0",
        ],
    ),
    (
        "rose-pine-dawn",
        [
            "#faf4ed", "#575279", "#907aa9", "#797593", "#dfdad9", "#fffaf3", "#ea9d34", "#b4637a",
            "#286983",
        ],
    ),
    (
        "gruvbox-light",
        [
            "#fbf1c7", "#3c3836", "#af3a03", "#7c6f64", "#d5c4a1", "#f2e5bc", "#b57614", "#9d0006",
            "#79740e",
        ],
    ),
    (
        "solarized-light",
        [
            "#fdf6e3", "#586e75", "#268bd2", "#93a1a1", "#ded8c4", "#f5efdd", "#b58900", "#dc322f",
            "#859900",
        ],
    ),
    (
        "github-light",
        [
            "#ffffff", "#24292f", "#0969da", "#57606a", "#d0d7de", "#f6f8fa", "#9a6700", "#cf222e",
            "#1a7f37",
        ],
    ),
    (
        "everforest",
        [
            "#2d353b", "#d3c6aa", "#a7c080", "#859289", "#3d484d", "#343f44", "#dbbc7f", "#e67e80",
            "#83c092",
        ],
    ),
    (
        "kanagawa",
        [
            "#1f1f28", "#dcd7ba", "#7e9cd8", "#727169", "#2d4f67", "#2a2a37", "#e6c384", "#e82424",
            "#98bb6c",
        ],
    ),
    (
        "ayu-dark",
        [
            "#0d1017", "#bfbdb6", "#e6b450", "#7b8391", "#1f242c", "#131721", "#ffb454", "#d95757",
            "#aad94c",
        ],
    ),
    (
        "ayu-mirage",
        [
            "#1f2430", "#cccac2", "#ffcc66", "#8a94a6", "#33415e", "#242936", "#ffd173", "#f28779",
            "#d5ff80",
        ],
    ),
    (
        "night-owl",
        [
            "#011627", "#d6deeb", "#82aaff", "#7c93a8", "#1d3b53", "#0b2942", "#ecc48d", "#ef5350",
            "#22da6e",
        ],
    ),
    (
        "palenight",
        [
            "#292d3e", "#a6accd", "#c792ea", "#676e95", "#3a3f58", "#32374d", "#ffcb6b", "#f07178",
            "#c3e88d",
        ],
    ),
    (
        "synthwave-84",
        [
            "#262335", "#f0eff1", "#ff7edb", "#848bbd", "#34294f", "#2a2139", "#fede5d", "#fe4450",
            "#72f1b8",
        ],
    ),
    (
        "oceanic-next",
        [
            "#1b2b34", "#c0c5ce", "#6699cc", "#8b98a6", "#4f5b66", "#343d46", "#fac863", "#ec5f67",
            "#99c794",
        ],
    ),
    (
        "nightfox",
        [
            "#192330", "#cdcecf", "#719cd6", "#71839b", "#29394f", "#212e3f", "#dbc074", "#c94f6d",
            "#81b29a",
        ],
    ),
    (
        "zenburn",
        [
            "#3f3f3f", "#dcdccc", "#f0dfaf", "#989890", "#5f5f5f", "#4f4f4f", "#dfaf8f", "#cc9393",
            "#7f9f7f",
        ],
    ),
    (
        "cobalt2",
        [
            "#193549", "#e1efff", "#ffc600", "#93b3cc", "#234e6d", "#1f4662", "#ff9d00", "#ff628c",
            "#3ad900",
        ],
    ),
    (
        "horizon",
        [
            "#1c1e26", "#d5d8da", "#e95678", "#8a8daf", "#2e303e", "#232530", "#fab795", "#f43e5c",
            "#29d398",
        ],
    ),
    (
        "neon-tokyo",
        [
            "#07060d", "#eae6ff", "#ff2bd6", "#7b7596", "#2b2447", "#100c1c", "#ffc531", "#ff4d6d",
            "#3df5c4",
        ],
    ),
    (
        "vaporwave",
        [
            "#0d0618", "#f2e9ff", "#ff6ad5", "#8d7fa8", "#3a2a5c", "#170c28", "#ffd166", "#ff5c8a",
            "#61e8e1",
        ],
    ),
    (
        "hotline",
        [
            "#0a0410", "#ffe9f4", "#ff2e88", "#96738c", "#3d1230", "#16081d", "#ffb627", "#ff4d4d",
            "#00e0c7",
        ],
    ),
    (
        "ultraviolet",
        [
            "#08040f", "#ece2ff", "#a855ff", "#7d6f9c", "#2d1a4d", "#110a1e", "#ffcc4d", "#ff4f81",
            "#4de8b0",
        ],
    ),
    (
        "cyberpunk",
        [
            "#050505", "#f4f4e8", "#fcee0a", "#8a8a7a", "#2e2e18", "#0f0f0a", "#ff9f1c", "#ff003c",
            "#00f0ff",
        ],
    ),
    (
        "matrix",
        [
            "#000000", "#c8ffc8", "#00ff41", "#4f8f4f", "#0f3d0f", "#050f05", "#b6ff00", "#ff5f56",
            "#00ff41",
        ],
    ),
    (
        "toxic",
        [
            "#040703", "#e8ffd9", "#aaff00", "#7f9a66", "#1f3312", "#0a1006", "#ffe600", "#ff4d3d",
            "#39ff88",
        ],
    ),
    (
        "amber-crt",
        [
            "#0a0600", "#ffd799", "#ffab00", "#9a7440", "#3d2a08", "#140d02", "#ffd500", "#ff6b4a",
            "#b8e04a",
        ],
    ),
    (
        "midnight-ember",
        [
            "#0a0705", "#ffe8d6", "#ff7a33", "#9c7f6b", "#3a2618", "#150e09", "#ffc233", "#ff5a4d",
            "#7fd98a",
        ],
    ),
    (
        "blood-moon",
        [
            "#0b0406", "#f6e3e6", "#ff3b52", "#96707a", "#3b1620", "#160a0e", "#ffab4a", "#ff5c6c",
            "#59d99d",
        ],
    ),
    (
        "deep-ocean",
        [
            "#020914", "#dcefff", "#00d4ff", "#5f7f99", "#123049", "#06141f", "#ffc857", "#ff5d73",
            "#2ee6a8",
        ],
    ),
    (
        "arctic-neon",
        [
            "#04080d", "#e6f4ff", "#4dd8ff", "#6f8799", "#183040", "#0a1219", "#ffd166", "#ff6b81",
            "#5df2b5",
        ],
    ),
    (
        "carbon",
        [
            "#000000", "#f0f0f0", "#ff5f1f", "#8c8c8c", "#262626", "#0d0d0d", "#ffbf00", "#ff3b30",
            "#32d74b",
        ],
    ),
    (
        "plasma",
        [
            "#06050f", "#e6e3ff", "#6c5cff", "#75729c", "#231f4a", "#0d0b1c", "#ffc93c", "#ff5470",
            "#3ce8b0",
        ],
    ),
    (
        "sapphire",
        [
            "#03070f", "#dfeaff", "#2979ff", "#65799c", "#122647", "#070e1c", "#ffc233", "#ff5c7a",
            "#31dba0",
        ],
    ),
    (
        "orchid",
        [
            "#08040a", "#f7e4ff", "#e56cf0", "#8f6b99", "#341542", "#120818", "#ffcc52", "#ff5c8a",
            "#52e0b8",
        ],
    ),
    (
        "ruby",
        [
            "#0a0308", "#ffe0ec", "#f50057", "#9c6b81", "#3d0f28", "#150610", "#ffbf47", "#ff5c7a",
            "#4de3a8",
        ],
    ),
    (
        "magma",
        [
            "#0b0503", "#ffe4d6", "#ff3d00", "#9c7263", "#3d1a0d", "#160a05", "#ffab2e", "#ff6347",
            "#8fd97f",
        ],
    ),
    (
        "bullion",
        [
            "#0a0803", "#fff3d4", "#ffcf33", "#9c8a5e", "#3a2f10", "#141005", "#ffe066", "#ff6b52",
            "#a8e05f",
        ],
    ),
    (
        "emerald-noir",
        [
            "#020a07", "#dcf6ea", "#00d68f", "#5f8c78", "#0f3327", "#06130e", "#ffc94d", "#ff5d6e",
            "#3ff2a8",
        ],
    ),
    (
        "mint-noir",
        [
            "#040b09", "#e0fff2", "#5effc4", "#6b9c8a", "#12352a", "#081511", "#ffd75e", "#ff6b7d",
            "#5effc4",
        ],
    ),
    (
        "abyss",
        [
            "#010a0c", "#d6f5f2", "#00e5cc", "#5c8a8a", "#0d3033", "#041416", "#ffcb47", "#ff5f70",
            "#4dffd2",
        ],
    ),
    (
        "spectre",
        [
            "#000000", "#e8f6fa", "#9fe8ff", "#6b8592", "#1c2b33", "#080d10", "#ffd98f", "#ff8fa3",
            "#8fffd6",
        ],
    ),
    (
        "obsidian",
        [
            "#000000", "#e8eaed", "#cfd8dc", "#7a8288", "#22262a", "#0b0d0f", "#ffca28", "#ff5252",
            "#69f0ae",
        ],
    ),
];

/// Every built-in palette, keyed by name.
pub fn presets() -> BTreeMap<String, Palette> {
    PRESETS
        .iter()
        .map(|(name, roles)| ((*name).to_string(), palette_from_roles(roles)))
        .collect()
}

/// The built-in palette names in the table's own order, which is the order the
/// picker lists them in. Deliberately not alphabetical: related schemes stay
/// together, so the four Catppuccin flavours are neighbours in the list.
pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|(name, _)| *name).collect()
}

fn palette_from_roles(roles: &[&str; 9]) -> Palette {
    Palette {
        background: roles[0].to_string(),
        foreground: roles[1].to_string(),
        accent: roles[2].to_string(),
        muted: roles[3].to_string(),
        border: roles[4].to_string(),
        surface: roles[5].to_string(),
        warning: roles[6].to_string(),
        error: roles[7].to_string(),
        success: roles[8].to_string(),
    }
}

/// The default palette.
pub fn default_palette() -> Palette {
    let roles = PRESETS
        .iter()
        .find(|(name, _)| *name == DEFAULT_PRESET)
        .map(|(_, roles)| roles)
        .expect("the default preset is present in the built-in table");
    palette_from_roles(roles)
}

/// Look up a palette by name.
///
/// `custom` returns the palette the user wrote in their config. An unknown
/// name returns the default palette and `false`, so a typo in `config.toml`
/// costs a warning rather than a refusal to start — the same lenient rule the
/// rest of the config parsing uses.
pub fn resolve(name: &str, custom: &Palette) -> (Palette, bool) {
    let name = name.trim().to_ascii_lowercase();
    if name == "custom" {
        return (custom.clone(), true);
    }
    match presets().get(&name) {
        Some(palette) => (palette.clone(), true),
        None => (default_palette(), false),
    }
}

/// The smallest contrast ratio (from the Web Content Accessibility
/// Guidelines) at which normal-size text stays comfortably readable.
const MINIMUM_TEXT_CONTRAST: f64 = 4.5;

/// Return a foreground colour that is actually readable on `background`.
///
/// Chat colours the author's name from a hash of their name, so a user can
/// end up with a colour that all but disappears against the current theme.
/// This keeps the chosen colour when it is readable enough, falls back to
/// `fallback` when that is readable, and otherwise picks plain black or white
/// — whichever stands out more against the background.
pub fn contrast_corrected(foreground: &str, background: &str, fallback: &str) -> String {
    let Some(fg) = parse_hex(foreground) else {
        return fallback.to_string();
    };
    let Some(bg) = parse_hex(background) else {
        return canonical(fg);
    };
    if contrast_ratio(fg, bg) >= MINIMUM_TEXT_CONTRAST {
        return canonical(fg);
    }
    if let Some(candidate) = parse_hex(fallback) {
        if contrast_ratio(candidate, bg) >= MINIMUM_TEXT_CONTRAST {
            return canonical(candidate);
        }
    }
    let white = (255, 255, 255);
    let black = (0, 0, 0);
    if contrast_ratio(black, bg) > contrast_ratio(white, bg) {
        canonical(black)
    } else {
        canonical(white)
    }
}

/// `steps` colours evenly spaced from `start` to `end`.
///
/// An unparseable endpoint yields `steps` copies of `start`, so a broken
/// custom palette loses a decoration rather than the whole screen.
pub fn gradient(start: &str, end: &str, steps: usize) -> Vec<String> {
    if steps == 0 {
        return Vec::new();
    }
    let (Some(from), Some(to)) = (parse_hex(start), parse_hex(end)) else {
        return vec![start.to_string(); steps];
    };
    if steps == 1 {
        return vec![canonical(from)];
    }
    (0..steps)
        .map(|index| {
            let fraction = index as f64 / (steps - 1) as f64;
            canonical((
                interpolate(from.0, to.0, fraction),
                interpolate(from.1, to.1, fraction),
                interpolate(from.2, to.2, fraction),
            ))
        })
        .collect()
}

/// A gradient that runs `start` → `end` → `start`.
///
/// Animations rotate through a gradient to make colour travel along a line of
/// text. With a plain gradient the wrap from the last colour back to the first
/// is a visible seam that scrolls past once per cycle; mirroring the ramp so
/// both ends match removes it.
pub fn seamless_gradient(start: &str, end: &str, steps: usize) -> Vec<String> {
    if steps == 0 {
        return Vec::new();
    }
    let forward_length = steps / 2 + steps % 2;
    let forward = gradient(start, end, forward_length);
    let mut colors = Vec::with_capacity(steps);
    colors.extend_from_slice(&forward);

    let mut reverse_start = forward.len() as isize - 1;
    if steps % 2 == 1 {
        reverse_start -= 1;
    }
    let mut index = reverse_start;
    while index >= 0 {
        colors.push(forward[index as usize].clone());
        index -= 1;
    }
    colors
}

/// Darken a colour by `amount`, where 0 changes nothing and 1 gives black.
pub fn darken(color: &str, amount: f64) -> String {
    let Some((r, g, b)) = parse_hex(color) else {
        return color.to_string();
    };
    let factor = 1.0 - amount.clamp(0.0, 1.0);
    canonical((
        (r as f64 * factor).round() as u8,
        (g as f64 * factor).round() as u8,
        (b as f64 * factor).round() as u8,
    ))
}

/// Blend `overlay` into `base`, where 0 returns `base` and 1 returns `overlay`.
///
/// Used for tinting: at a low amount the result is still a background, but it
/// carries enough of the overlay's hue to be recognisable.
pub fn mix(base: &str, overlay: &str, amount: f64) -> String {
    let (Some(from), Some(to)) = (parse_hex(base), parse_hex(overlay)) else {
        return base.to_string();
    };
    let amount = amount.clamp(0.0, 1.0);
    canonical((
        interpolate(from.0, to.0, amount),
        interpolate(from.1, to.1, amount),
        interpolate(from.2, to.2, amount),
    ))
}

fn interpolate(from: u8, to: u8, fraction: f64) -> u8 {
    let value = from as f64 + (to as f64 - from as f64) * fraction;
    value.round().clamp(0.0, 255.0) as u8
}

/// Parse `#rgb`, `#rrggbb` or the same without the `#`.
fn parse_hex(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.trim().trim_start_matches('#');
    let expanded;
    let value = if value.len() == 3 {
        // The three-digit form doubles each digit: `#abc` means `#aabbcc`.
        expanded = value.chars().flat_map(|c| [c, c]).collect::<String>();
        expanded.as_str()
    } else {
        value
    };
    if value.len() != 6 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let parsed = u32::from_str_radix(value, 16).ok()?;
    Some((
        (parsed >> 16) as u8,
        ((parsed >> 8) & 0xff) as u8,
        (parsed & 0xff) as u8,
    ))
}

fn canonical((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The WCAG contrast ratio between two colours: 1.0 when identical, 21.0 for
/// black against white.
fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let first = relative_luminance(a);
    let second = relative_luminance(b);
    let (lighter, darker) = if first > second {
        (first, second)
    } else {
        (second, first)
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// How bright a colour looks to a human eye, on a 0.0–1.0 scale, using the
/// WCAG formula: each channel is un-gamma-corrected, then the three are
/// weighted (green counts for most of perceived brightness, blue for least).
fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    fn channel(value: u8) -> f64 {
        let value = value as f64 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_fills_all_nine_roles_with_a_parseable_colour() {
        for (name, palette) in presets() {
            for role in ROLES {
                let value = palette.role(role);
                assert!(
                    parse_hex(value).is_some(),
                    "preset {name} has an unparseable {role}: {value:?}"
                );
            }
        }
    }

    #[test]
    fn the_table_holds_the_full_set_of_presets_with_no_duplicates() {
        let names = preset_names();
        assert_eq!(names.len(), 57);
        assert_eq!(presets().len(), names.len(), "a preset name is duplicated");
    }

    /// Body text has to be readable in every built-in theme without the
    /// contrast correction stepping in, which is a property of the palettes
    /// themselves rather than of the drawing code.
    #[test]
    fn every_preset_keeps_its_foreground_readable_on_its_own_background() {
        for (name, palette) in presets() {
            let fg = parse_hex(&palette.foreground).expect("a parseable foreground");
            let bg = parse_hex(&palette.background).expect("a parseable background");
            let ratio = contrast_ratio(fg, bg);
            assert!(
                ratio >= MINIMUM_TEXT_CONTRAST,
                "preset {name} only reaches {ratio:.2}:1, below the {MINIMUM_TEXT_CONTRAST}:1 floor"
            );
        }
    }

    #[test]
    fn an_unknown_name_falls_back_to_the_default_instead_of_failing() {
        let (palette, known) = resolve("no-such-theme", &default_palette());
        assert!(!known);
        assert_eq!(palette, default_palette());
    }

    #[test]
    fn resolve_ignores_case_and_surrounding_space() {
        let (palette, known) = resolve("  NORD  ", &default_palette());
        assert!(known);
        assert_eq!(palette, presets()["nord"]);
    }

    #[test]
    fn custom_returns_the_users_own_palette() {
        let mut custom = default_palette();
        custom.accent = "#123456".to_string();
        let (palette, known) = resolve("custom", &custom);
        assert!(known);
        assert_eq!(palette.accent, "#123456");
    }

    #[test]
    fn hex_parsing_accepts_both_forms_and_rejects_nonsense() {
        assert_eq!(parse_hex("#1a2b3c"), Some((0x1a, 0x2b, 0x3c)));
        assert_eq!(parse_hex("1a2b3c"), Some((0x1a, 0x2b, 0x3c)));
        assert_eq!(parse_hex("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn an_unparseable_colour_draws_as_the_terminal_default() {
        assert_eq!(color("#ff0000"), Color::Rgb(255, 0, 0));
        assert_eq!(color("not a colour"), Color::Reset);
    }

    #[test]
    fn a_gradient_runs_from_one_endpoint_to_the_other() {
        let ramp = gradient("#000000", "#ffffff", 3);
        assert_eq!(ramp, vec!["#000000", "#808080", "#ffffff"]);
        assert_eq!(gradient("#000000", "#ffffff", 0), Vec::<String>::new());
        assert_eq!(gradient("#000000", "#ffffff", 1), vec!["#000000"]);
    }

    #[test]
    fn a_broken_endpoint_degrades_the_gradient_rather_than_the_screen() {
        assert_eq!(gradient("nope", "#ffffff", 2), vec!["nope", "nope"]);
    }

    /// The point of the mirrored ramp is that rotating through it has no
    /// visible jump, which means the first and last entries must match.
    #[test]
    fn a_seamless_gradient_starts_and_ends_on_the_same_colour() {
        for steps in 2..12 {
            let ramp = seamless_gradient("#000000", "#ffffff", steps);
            assert_eq!(ramp.len(), steps, "wrong length for {steps} steps");
            assert_eq!(ramp[0], ramp[steps - 1], "seam visible at {steps} steps");
        }
    }

    #[test]
    fn darken_and_mix_stay_inside_their_endpoints() {
        assert_eq!(darken("#ffffff", 0.0), "#ffffff");
        assert_eq!(darken("#ffffff", 1.0), "#000000");
        assert_eq!(darken("#ffffff", 0.5), "#808080");
        // Out-of-range amounts clamp rather than wrapping around.
        assert_eq!(darken("#ffffff", 5.0), "#000000");
        assert_eq!(darken("broken", 0.5), "broken");

        assert_eq!(mix("#000000", "#ffffff", 0.0), "#000000");
        assert_eq!(mix("#000000", "#ffffff", 1.0), "#ffffff");
        assert_eq!(mix("#000000", "#ffffff", 0.5), "#808080");
    }

    #[test]
    fn contrast_correction_leaves_a_readable_colour_alone() {
        assert_eq!(
            contrast_corrected("#ffffff", "#000000", "#cccccc"),
            "#ffffff"
        );
    }

    #[test]
    fn contrast_correction_replaces_a_colour_that_would_vanish() {
        // Near-black on black: unreadable, and the fallback is no better, so
        // it must end up as plain white.
        assert_eq!(
            contrast_corrected("#010101", "#000000", "#020202"),
            "#ffffff"
        );
        // The same on a white background must end up black instead.
        assert_eq!(
            contrast_corrected("#fefefe", "#ffffff", "#fdfdfd"),
            "#000000"
        );
    }

    /// The escape sequences have to be exactly right: a malformed one is not
    /// ignored by a terminal, it is *displayed*, which would leave stray
    /// characters across the top of the screen.
    #[test]
    fn the_terminal_background_sequences_are_well_formed() {
        assert_eq!(background_sequence("#1a1523"), "\u{1b}]11;#1a1523\u{7}");
        assert_eq!(RESET_BACKGROUND_SEQUENCE, "\u{1b}]111\u{7}");
    }

    #[test]
    fn the_canvas_is_darker_than_the_background_it_comes_from() {
        let palette = presets()["nord"].clone();
        assert_ne!(palette.canvas(), palette.background);
        assert_eq!(
            palette.canvas(),
            darken(&palette.background, CANVAS_DARKEN_AMOUNT)
        );
    }
}
