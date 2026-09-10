//! Theming.
//!
//! The default palette is built from ANSI slots rather than fixed RGB, so rigor
//! renders in whatever colors its host terminal is themed with — that is the
//! inheritance path from herdr / tmux / the system. Any slot can be pinned to a
//! concrete color in config, or by pointing `RIGOR_THEME` (or `HERDR_THEME_FILE`)
//! at a TOML file with the same `[theme]` keys.

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub accent: Option<String>,
    pub success: Option<String>,
    pub failure: Option<String>,
    pub pending: Option<String>,
    pub muted: Option<String>,
    pub warn: Option<String>,
    pub border: Option<String>,
    pub sel_bg: Option<String>,
    pub sel_fg: Option<String>,
}

impl ThemeConfig {
    pub fn merge(&mut self, other: ThemeConfig) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(
            fg, bg, accent, success, failure, pending, muted, warn, border, sel_bg, sel_fg
        );
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub fg: Color,
    pub bg: Color,
    pub accent: Color,
    pub success: Color,
    pub failure: Color,
    pub pending: Color,
    pub muted: Color,
    pub warn: Color,
    pub border: Color,
    pub sel_bg: Color,
    pub sel_fg: Color,
}

impl Default for Theme {
    /// Every slot here resolves through the terminal's own palette: ANSI 0–15
    /// are theme-defined, so they track whatever herdr / the terminal is themed
    /// with. Nothing is pinned to the fixed 256-colour cube, which would render
    /// as a flat off-hue patch against a tinted background.
    fn default() -> Self {
        Self {
            fg: Color::Reset,
            bg: Color::Reset,
            accent: Color::Cyan,
            success: Color::Green,
            failure: Color::Red,
            pending: Color::Yellow,
            muted: Color::DarkGray,
            warn: Color::Magenta,
            border: Color::DarkGray,
            sel_bg: Color::Reset,
            sel_fg: Color::Reset,
        }
    }
}

impl Theme {
    /// Config first, then `RIGOR_THEME` / `HERDR_THEME_FILE`, which win because
    /// they are how a parent shell hands its palette down at launch time.
    pub fn resolve(cfg: &ThemeConfig) -> Result<Self> {
        let mut merged = cfg.clone();

        for var in ["HERDR_THEME_FILE", "RIGOR_THEME"] {
            let Ok(val) = std::env::var(var) else {
                continue;
            };
            if val.trim().is_empty() {
                continue;
            }
            merged.merge(load_theme_file(Path::new(&val))?);
        }

        let mut t = Theme::default();
        if no_color() {
            t = Theme::monochrome();
        }

        macro_rules! set {
            ($($f:ident),*) => { $(
                if let Some(s) = merged.$f.as_deref() {
                    t.$f = parse_color(s).with_context(|| format!("theme.{}: `{s}` is not a color", stringify!($f)))?;
                }
            )* };
        }
        set!(
            fg, bg, accent, success, failure, pending, muted, warn, border, sel_bg, sel_fg
        );
        Ok(t)
    }

    /// NO_COLOR: keep the layout, drop the hue. Selection still needs contrast,
    /// so it inverts rather than tints.
    fn monochrome() -> Self {
        Self {
            fg: Color::Reset,
            bg: Color::Reset,
            accent: Color::Reset,
            success: Color::Reset,
            failure: Color::Reset,
            pending: Color::Reset,
            muted: Color::DarkGray,
            warn: Color::Reset,
            border: Color::DarkGray,
            sel_bg: Color::Reset,
            sel_fg: Color::Reset,
        }
    }
}

pub fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

fn load_theme_file(path: &Path) -> Result<ThemeConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading theme file {}", path.display()))?;

    // Accept either a bare table of slots or a file with a [theme] section.
    #[derive(Deserialize)]
    struct Wrapper {
        theme: Option<ThemeConfig>,
    }
    if let Ok(w) = toml::from_str::<Wrapper>(&text)
        && let Some(t) = w.theme
    {
        return Ok(t);
    }
    toml::from_str::<ThemeConfig>(&text)
        .with_context(|| format!("parsing theme file {}", path.display()))
}

/// `#7dd3fc`, a 0–255 palette index, an ANSI name, or `inherit`.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return match hex.len() {
            6 => Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
            3 => {
                let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
                Some(Color::Rgb((r * 17) as u8, (g * 17) as u8, (b * 17) as u8))
            }
            _ => None,
        };
    }
    if let Ok(n) = s.parse::<u8>() {
        return Some(Color::Indexed(n));
    }
    match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "inherit" | "default" | "reset" | "none" => Some(Color::Reset),
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" | "purple" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" | "white" => Some(Color::Gray),
        "darkgray" | "darkgrey" | "brightblack" => Some(Color::DarkGray),
        "lightred" | "brightred" => Some(Color::LightRed),
        "lightgreen" | "brightgreen" => Some(Color::LightGreen),
        "lightyellow" | "brightyellow" => Some(Color::LightYellow),
        "lightblue" | "brightblue" => Some(Color::LightBlue),
        "lightmagenta" | "brightmagenta" => Some(Color::LightMagenta),
        "lightcyan" | "brightcyan" => Some(Color::LightCyan),
        "brightwhite" => Some(Color::White),
        _ => None,
    }
}
