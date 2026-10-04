//! The reader stylesheets from the live Omarchy palette (ADR-004). The palette file is only
//! read. Plain and simple HTML takes the theme; HTML with its own colours sits on a light
//! "paper" card (DESIGN.md, "HTML mail inside the theme").

use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const WHITE: Self = Self(0xff, 0xff, 0xff);

    /// Accepts `#rgb` and `#rrggbb` (alpha in `#rrggbbaa` is dropped).
    pub fn parse(value: &str) -> Option<Self> {
        let hex = value.trim().strip_prefix('#')?;
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
        match hex.len() {
            3 => {
                let nibble = |index: usize| channel(index..index + 1).map(|value| value * 17);
                Some(Self(nibble(0)?, nibble(1)?, nibble(2)?))
            }
            6 | 8 => Some(Self(channel(0..2)?, channel(2..4)?, channel(4..6)?)),
            _ => None,
        }
    }

    /// Blends toward `other` by `percent`, as `omarchy-theme-color` mixes.
    pub fn mix(self, other: Self, percent: u8) -> Self {
        let amount = f64::from(percent.min(100)) / 100.0;
        let blend =
            |a: u8, b: u8| (f64::from(a) * (1.0 - amount) + f64::from(b) * amount + 0.5) as u8;
        Self(
            blend(self.0, other.0),
            blend(self.1, other.1),
            blend(self.2, other.2),
        )
    }

    pub fn relative_luminance(self) -> f64 {
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.0) + 0.7152 * linear(self.1) + 0.0722 * linear(self.2)
    }

    pub fn css(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    pub fn floats(self) -> [f32; 3] {
        [self.0, self.1, self.2].map(|channel| f32::from(channel) / 255.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub source: String,
    pub dark: bool,
    pub background: Rgb,
    pub foreground: Rgb,
    pub accent: Rgb,
    pub lighter_background: Rgb,
    pub dark_background: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyStyle {
    /// No colours of its own: the theme's background, text and accent links.
    Themed,
    /// Brings its own colours: kept as designed on a light card.
    Paper,
}

/// The paper card's surface.
pub const PAPER: Rgb = Rgb::WHITE;
const PAPER_TEXT: Rgb = Rgb(0x1f, 0x1f, 0x1f);

impl Palette {
    /// libadwaita's dark palette, for running outside Omarchy.
    pub fn builtin_dark() -> Self {
        Self {
            source: "builtin-dark".into(),
            dark: true,
            background: Rgb(0x1d, 0x1d, 0x20),
            foreground: Rgb(0xde, 0xdd, 0xda),
            accent: Rgb(0x35, 0x84, 0xe4),
            lighter_background: Rgb(0x2e, 0x2e, 0x32),
            dark_background: Rgb(0x16, 0x16, 0x18),
        }
    }

    /// Parses the flat `key = "value"` lines of Omarchy's `colors.toml`.
    pub fn from_colors_toml(text: &str, source: &str) -> Option<Self> {
        let get = |key: &str| -> Option<String> {
            text.lines().find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
            })
        };
        let background = Rgb::parse(&get("background")?)?;
        let foreground = Rgb::parse(&get("foreground")?)?;
        let dark = match get("mode") {
            Some(mode) => !mode.eq_ignore_ascii_case("light"),
            None => background.relative_luminance() < 0.5,
        };
        let accent = get("accent")
            .and_then(|value| Rgb::parse(&value))
            .or_else(|| get("blue").and_then(|value| Rgb::parse(&value)))?;
        let shade = |key: &str, fallback: Rgb| {
            get(key)
                .and_then(|value| Rgb::parse(&value))
                .unwrap_or(fallback)
        };
        let lighter_background = shade("lighter_background", background.mix(foreground, 10));
        let dark_background = shade("dark_background", background.mix(Rgb(0, 0, 0), 25));
        Some(Self {
            source: source.to_owned(),
            dark,
            background,
            foreground,
            accent,
            lighter_background,
            dark_background,
        })
    }

    /// The live palette from `<omarchy_state>/current/theme/colors.toml`, read only.
    pub fn load_omarchy(omarchy_state: &Path) -> Option<Self> {
        let current = omarchy_state.join("current");
        let text = std::fs::read_to_string(current.join("theme/colors.toml")).ok()?;
        let name = std::fs::read_to_string(current.join("theme.name"))
            .map(|name| name.trim().to_owned())
            .unwrap_or_else(|_| "unnamed".into());
        Self::from_colors_toml(&text, &format!("omarchy:{name}"))
    }

    /// Secondary text: foreground mixed 35 % toward background (DESIGN.md theme tokens).
    pub fn secondary(&self) -> Rgb {
        self.foreground.mix(self.background, 35)
    }
}

/// Rules every reader view gets: the root and body heights follow their content, so the
/// height the helper script reports never includes the viewport it was measured in (the
/// view is sized to its content, so "fill the viewport" rules have nothing to fill).
/// `max-content` rather than `auto`: in quirks mode (bodies without a doctype) an `auto`
/// root and body fill the viewport.
const MEASURE_CSS: &str =
    "html,body{height:max-content!important;min-height:0!important;max-height:none!important}";

/// The user-level stylesheet for one message body. Author styles in the message win over it.
pub fn reader_css(palette: &Palette, style: BodyStyle, sans: &str) -> String {
    let mut css = String::from(MEASURE_CSS);
    match style {
        BodyStyle::Themed => {
            let scheme = if palette.dark { "dark" } else { "light" };
            let _ = write!(
                css,
                ":root{{color-scheme:{scheme}}}\
                 html{{background-color:transparent;color:{fg};font:15px/1.55 \"{sans}\",sans-serif}}\
                 body{{margin:0;padding:14px 18px;overflow-wrap:anywhere}}\
                 a{{color:{accent}}}\
                 blockquote{{margin:0 0 0 2px;padding-left:12px;border-left:3px solid {border};color:{secondary}}}\
                 pre,code,tt{{font-family:monospace;font-size:13px}}\
                 hr{{border:0;border-top:1px solid {border}}}\
                 img{{max-width:100%;height:auto}}\
                 table{{border-collapse:collapse}}\
                 td,th{{border-color:{border}}}",
                fg = palette.foreground.css(),
                accent = palette.accent.css(),
                border = palette.lighter_background.css(),
                secondary = palette.secondary().css(),
            );
        }
        BodyStyle::Paper => {
            let _ = write!(
                css,
                ":root{{color-scheme:light}}html{{background-color:{paper};color:{text}}}",
                paper = PAPER.css(),
                text = PAPER_TEXT.css(),
            );
        }
    }
    css
}

/// The GTK chrome around the bodies: window, message headers and the paper card.
pub fn app_css(palette: &Palette, radius: u32) -> String {
    format!(
        "window.s2, .conversation {{ background-color: {bg}; color: {fg}; }}\
         .message {{ margin: 10px 0; }}\
         .message-header {{ padding: 6px 2px; }}\
         .avatar {{ background-color: {accent}; color: {bg}; border-radius: 999px; min-width: 32px; min-height: 32px; font-weight: bold; }}\
         .sender {{ font-weight: bold; }}\
         .dim {{ color: {secondary}; }}\
         .paper {{ background-color: {paper}; border: 1px solid {border}; border-radius: {radius}px; }}\
         .themed-body {{ background-color: {bg}; }}\
         textview.plain, textview.plain text {{ background-color: {bg}; color: {fg}; font-size: 15px; }}",
        bg = palette.background.css(),
        fg = palette.foreground.css(),
        accent = palette.accent.css(),
        secondary = palette.secondary().css(),
        border = palette.lighter_background.css(),
        paper = PAPER.css(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MATTE: &str = "mode = \"dark\"\naccent = \"#e68e0d\"\nbackground = \"#121212\"\n\
                         lighter_background = \"#1e1e1e\"\nforeground = \"#bebebe\"\nblue = \"#e68e0d\"\n";

    #[test]
    fn parses_palette_lines() {
        let palette = Palette::from_colors_toml(MATTE, "test").unwrap();
        assert!(palette.dark);
        assert_eq!(palette.background, Rgb(0x12, 0x12, 0x12));
        assert_eq!(palette.accent, Rgb(0xe6, 0x8e, 0x0d));
        assert_eq!(palette.lighter_background, Rgb(0x1e, 0x1e, 0x1e));
        assert!(Palette::from_colors_toml("mode = \"dark\"", "x").is_none());
    }

    #[test]
    fn parses_short_and_alpha_hex() {
        assert_eq!(Rgb::parse("#fff"), Some(Rgb::WHITE));
        assert_eq!(Rgb::parse("#11223344"), Some(Rgb(0x11, 0x22, 0x33)));
        assert_eq!(Rgb::parse("red"), None);
    }

    #[test]
    fn themed_and_paper_css_differ() {
        let palette = Palette::from_colors_toml(MATTE, "test").unwrap();
        let themed = reader_css(&palette, BodyStyle::Themed, "Liberation Sans");
        let paper = reader_css(&palette, BodyStyle::Paper, "Liberation Sans");
        assert!(themed.contains("#bebebe") && themed.contains("#e68e0d"));
        assert!(paper.contains("#ffffff") && !paper.contains("#bebebe"));
        assert!(themed.starts_with(MEASURE_CSS) && paper.starts_with(MEASURE_CSS));
    }
}
