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

use crate::probe::{Probed, Rgb};

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
    ///
    /// `probed` carries the terminal's own foreground and background when it
    /// answered OSC 10/11. The selection band and the hairlines are mixed from
    /// them, so they sit inside the user's theme rather than on top of it.
    pub fn resolve(cfg: &ThemeConfig, probed: Probed) -> Result<Self> {
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

        // Explicit colours always win; derivation only fills what was left to
        // the terminal. A configured hex background is a better base than the
        // probed one, because it is the colour rigor actually paints.
        if !no_color() {
            let base_bg = rgb_of(t.bg).or(probed.bg);
            let base_fg = rgb_of(t.fg).or(probed.fg);
            if let Some(bg) = base_bg {
                let dark = luminance(bg) < 0.5;
                let toward = base_fg.unwrap_or(if dark { (255, 255, 255) } else { (0, 0, 0) });
                if merged.sel_bg.is_none() {
                    t.sel_bg = to_color(mix(bg, toward, if dark { 0.11 } else { 0.075 }));
                }
                if merged.border.is_none() {
                    t.border = to_color(mix(bg, toward, if dark { 0.22 } else { 0.18 }));
                }
            }
        }
        Ok(t)
    }

    /// NO_COLOR: keep the layout, drop the hue. With no band to lean on, the
    /// selection is carried by the bar glyph and weight alone.
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

fn rgb_of(c: Color) -> Option<Rgb> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

fn to_color((r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// `a` moved toward `b` by `t` (0 = a, 1 = b), per channel.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let ch = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Relative luminance (sRGB, WCAG), 0 for black to 1 for white.
fn luminance((r, g, b): Rgb) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
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

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Probed = Probed {
        fg: Some((0xd4, 0xd8, 0xde)),
        bg: Some((0x16, 0x18, 0x1c)),
    };
    const LIGHT: Probed = Probed {
        fg: Some((0x2b, 0x2f, 0x33)),
        bg: Some((0xfb, 0xfb, 0xfa)),
    };

    fn rgb(c: Color) -> Rgb {
        rgb_of(c).expect("expected a derived rgb colour")
    }

    /// The band must be visible but quiet: a small step from the background
    /// toward the foreground, in the right direction for the theme.
    #[test]
    fn the_band_is_a_small_step_toward_the_foreground() {
        let dark = Theme::resolve(&ThemeConfig::default(), DARK).unwrap();
        let band = rgb(dark.sel_bg);
        assert!(
            band.0 > 0x16 && band.0 < 0x40,
            "dark band {band:?} should lift slightly"
        );

        let light = Theme::resolve(&ThemeConfig::default(), LIGHT).unwrap();
        let band = rgb(light.sel_bg);
        assert!(
            band.0 < 0xfb && band.0 > 0xdc,
            "light band {band:?} should darken slightly"
        );
    }

    /// Hairlines sit further from the background than the band, so the rail
    /// and divider still read on top of a selected row.
    #[test]
    fn hairlines_are_stronger_than_the_band() {
        let t = Theme::resolve(&ThemeConfig::default(), DARK).unwrap();
        assert!(rgb(t.border).0 > rgb(t.sel_bg).0);
    }

    /// No answer from the terminal means nothing to mix from: fall back to the
    /// palette and the bar-and-weight selection rather than guessing a colour.
    #[test]
    fn an_unanswered_probe_leaves_the_palette_alone() {
        let t = Theme::resolve(&ThemeConfig::default(), Probed::default()).unwrap();
        assert_eq!(t.sel_bg, Color::Reset);
        assert_eq!(t.border, Color::DarkGray);
    }

    /// Anything the user set by hand beats derivation.
    #[test]
    fn explicit_colours_win_over_derived_ones() {
        let cfg = ThemeConfig {
            sel_bg: Some("#123456".into()),
            border: Some("8".into()),
            ..ThemeConfig::default()
        };
        let t = Theme::resolve(&cfg, DARK).unwrap();
        assert_eq!(t.sel_bg, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(t.border, Color::Indexed(8));
    }

    #[test]
    fn mix_is_linear_per_channel() {
        assert_eq!(mix((0, 0, 0), (200, 100, 50), 0.5), (100, 50, 25));
        assert_eq!(mix((10, 20, 30), (99, 99, 99), 0.0), (10, 20, 30));
    }
}
