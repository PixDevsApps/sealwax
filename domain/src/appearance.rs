//! The appearance choices `config.toml` makes (ADR-019, DESIGN.md "Typography"): the corner
//! radius and the two fonts. Plain values: the theme (`sealwax-ui-model`) turns them into
//! sizes, families and stylesheets.

use std::str::FromStr;

/// The largest corner radius applied, in px.
pub const MAX_RADIUS: u32 = 64;

/// `config.toml`'s `corner_radius` (ADR-019): Hyprland's `decoration:rounding`, or px.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CornerRadius {
    #[default]
    Auto,
    Px(u32),
}

impl CornerRadius {
    /// The radius in px: Hyprland's rounding for `Auto`, square (0) when that is unknown.
    pub fn resolve(self, hyprland: Option<u32>) -> u32 {
        match self {
            Self::Auto => hyprland.unwrap_or(0),
            Self::Px(px) => px,
        }
        .min(MAX_RADIUS)
    }
}

impl FromStr for CornerRadius {
    type Err = String;

    /// `auto`, or a whole number of px with an optional `px`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        text.strip_suffix("px")
            .unwrap_or(text)
            .trim()
            .parse()
            .map(Self::Px)
            .map_err(|_| format!("expected \"auto\" or a number of px, not {text:?}"))
    }
}

/// The appearance choices `config.toml` makes; the app fills them from its settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ThemeOptions {
    pub corner_radius: CornerRadius,
    pub chrome_font: ChromeFont,
    pub reading_font: ReadingFont,
    /// Styled HTML mail shown in dark colours under a dark theme (issue #27), instead of on
    /// the light paper card.
    pub dark_styled_mail: bool,
}

/// `config.toml`'s chrome font: the Omarchy font (default) or sans-serif.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChromeFont {
    #[default]
    Omarchy,
    Sans,
}

/// `config.toml`'s reading font for message text: sans-serif (default) or the Omarchy font.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReadingFont {
    #[default]
    Sans,
    Mono,
}

impl FromStr for ChromeFont {
    type Err = String;

    /// `omarchy` or `sans`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().as_str() {
            "omarchy" => Ok(Self::Omarchy),
            "sans" => Ok(Self::Sans),
            _ => Err(format!("expected \"omarchy\" or \"sans\", not {text:?}")),
        }
    }
}

impl FromStr for ReadingFont {
    type Err = String;

    /// `sans` or `mono`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().as_str() {
            "sans" => Ok(Self::Sans),
            "mono" => Ok(Self::Mono),
            _ => Err(format!("expected \"sans\" or \"mono\", not {text:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_radius_follows_hyprland_or_the_setting() {
        assert_eq!(CornerRadius::Auto.resolve(Some(8)), 8);
        assert_eq!(CornerRadius::Auto.resolve(None), 0);
        assert_eq!(CornerRadius::Px(4).resolve(Some(8)), 4);
        assert_eq!(CornerRadius::Px(500).resolve(None), MAX_RADIUS);
        assert_eq!("auto".parse(), Ok(CornerRadius::Auto));
        assert_eq!(" 12px".parse(), Ok(CornerRadius::Px(12)));
        assert_eq!("0".parse(), Ok(CornerRadius::Px(0)));
        assert!("round".parse::<CornerRadius>().is_err());
        assert!("-2".parse::<CornerRadius>().is_err());
    }

    #[test]
    fn fonts_parse() {
        assert_eq!("Omarchy".parse(), Ok(ChromeFont::Omarchy));
        assert_eq!(" mono ".parse(), Ok(ReadingFont::Mono));
        assert!("serif".parse::<ReadingFont>().is_err());
    }
}
