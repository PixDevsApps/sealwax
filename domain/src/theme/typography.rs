//! Fonts and the type scale (DESIGN.md "Typography"): the chrome uses the Omarchy font
//! (fontconfig's `monospace`), message text uses `sans-serif`, and sizes follow
//! omarchy-shell's scale from `[font] base-size`.

use super::shell::{ShellValues, get, parse_int};
use std::str::FromStr;

/// omarchy-shell's default `[font] base-size`, in px.
pub const DEFAULT_BASE_SIZE: u32 = 12;
/// A cap on `base-size`, which omarchy-shell only floors at 1 px.
pub const MAX_BASE_SIZE: u32 = 64;
/// Message text at the default base size (DESIGN.md: ≈ 15 px) and its line height.
pub const READING_SIZE: u32 = 15;
pub const READING_LINE_HEIGHT: f64 = 1.55;

/// Text sizes in px. At base 12: caption 10, small 11, body 12, subtitle 13, title 14,
/// heading 16, display 24; message text 15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeScale {
    pub base: u32,
    pub caption: u32,
    pub small: u32,
    pub body: u32,
    pub subtitle: u32,
    pub title: u32,
    pub heading: u32,
    pub display: u32,
    pub reading: u32,
}

impl TypeScale {
    pub fn new(base: u32) -> Self {
        Self::from_shell(&ShellValues::from([(
            "font.base-size".to_owned(),
            base.to_string(),
        )]))
    }

    /// `[font] base-size` with omarchy-shell's multipliers, and its per-token overrides
    /// (`caption`, `body-small`, `body`, `subtitle`, `title`, `heading`, `display`) in px.
    pub fn from_shell(values: &ShellValues) -> Self {
        let base = get(values, "font", "base-size")
            .and_then(parse_int)
            .map_or(DEFAULT_BASE_SIZE, |base| {
                base.clamp(1, i64::from(MAX_BASE_SIZE)) as u32
            });
        let scaled = |multiplier: f64| ((f64::from(base) * multiplier).round() as u32).max(1);
        let token = |key: &str, multiplier: f64| {
            get(values, "font", key)
                .and_then(parse_int)
                .filter(|size| *size > 0)
                .map_or_else(
                    || scaled(multiplier),
                    |size| size.min(i64::from(MAX_BASE_SIZE) * 4) as u32,
                )
        };
        Self {
            base,
            caption: token("caption", 0.833),
            small: token("body-small", 0.917),
            body: token("body", 1.0),
            subtitle: token("subtitle", 1.083),
            title: token("title", 1.167),
            heading: token("heading", 1.333),
            display: token("display", 2.0),
            reading: scaled(f64::from(READING_SIZE) / f64::from(DEFAULT_BASE_SIZE)),
        }
    }
}

impl Default for TypeScale {
    fn default() -> Self {
        Self::new(DEFAULT_BASE_SIZE)
    }
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

/// The families fontconfig resolves: `fc-match monospace` (the Omarchy font, set by `omarchy
/// font set`) and `fc-match sans-serif`. The generic names stand in until they are known;
/// Pango and WebKit resolve those themselves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontFamilies {
    pub monospace: String,
    pub sans: String,
}

impl Default for FontFamilies {
    fn default() -> Self {
        Self {
            monospace: "monospace".to_owned(),
            sans: "sans-serif".to_owned(),
        }
    }
}

/// The families the app uses, after `config.toml`'s choices.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fonts {
    /// Sidebar, list, toolbars, chips.
    pub chrome: String,
    pub chrome_is_mono: bool,
    /// Message text, in the reader and the composer.
    pub reading: String,
    pub reading_is_mono: bool,
    pub monospace: String,
    pub sans: String,
}

impl Fonts {
    pub fn new(families: &FontFamilies, chrome: ChromeFont, reading: ReadingFont) -> Self {
        let pick = |mono: bool| {
            if mono {
                families.monospace.clone()
            } else {
                families.sans.clone()
            }
        };
        let chrome_is_mono = chrome == ChromeFont::Omarchy;
        let reading_is_mono = reading == ReadingFont::Mono;
        Self {
            chrome: pick(chrome_is_mono),
            chrome_is_mono,
            reading: pick(reading_is_mono),
            reading_is_mono,
            monospace: families.monospace.clone(),
            sans: families.sans.clone(),
        }
    }
}

/// A CSS font-family list: the family quoted and escaped, then its generic fallback; the
/// generic name alone when that is the family.
pub fn css_family(family: &str, generic: &str) -> String {
    if family.trim().eq_ignore_ascii_case(generic) {
        return generic.to_owned();
    }
    let mut quoted = String::with_capacity(family.len() + generic.len() + 4);
    quoted.push('"');
    for character in family.chars() {
        match character {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(character);
            }
            character if character.is_control() => quoted.push(' '),
            _ => quoted.push(character),
        }
    }
    quoted.push_str("\", ");
    quoted.push_str(generic);
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scale_matches_omarchy_shell() {
        let scale = TypeScale::default();
        assert_eq!(
            [
                scale.caption,
                scale.small,
                scale.body,
                scale.subtitle,
                scale.title,
                scale.heading,
                scale.display,
                scale.reading,
            ],
            [10, 11, 12, 13, 14, 16, 24, 15]
        );
    }

    #[test]
    fn the_scale_follows_the_base_size() {
        let scale = TypeScale::new(16);
        assert_eq!(scale.base, 16);
        assert_eq!(scale.caption, 13);
        assert_eq!(scale.body, 16);
        assert_eq!(scale.heading, 21);
        assert_eq!(scale.display, 32);
        assert_eq!(scale.reading, 20);
        assert_eq!(TypeScale::new(0).base, 1);
        assert_eq!(TypeScale::new(1000).base, MAX_BASE_SIZE);
    }

    #[test]
    fn per_token_overrides_win() {
        let values = ShellValues::from([
            ("font.base-size".to_owned(), "14".to_owned()),
            ("font.heading".to_owned(), "20".to_owned()),
            ("font.caption".to_owned(), "0".to_owned()),
        ]);
        let scale = TypeScale::from_shell(&values);
        assert_eq!(scale.heading, 20);
        assert_eq!(scale.caption, 12);
        assert_eq!(scale.body, 14);
        let garbage = ShellValues::from([("font.base-size".to_owned(), "big".to_owned())]);
        assert_eq!(TypeScale::from_shell(&garbage), TypeScale::default());
    }

    #[test]
    fn fonts_follow_the_options() {
        let families = FontFamilies {
            monospace: "Test Mono".into(),
            sans: "Test Sans".into(),
        };
        let fonts = Fonts::new(&families, ChromeFont::Omarchy, ReadingFont::Sans);
        assert_eq!(
            (fonts.chrome.as_str(), fonts.reading.as_str()),
            ("Test Mono", "Test Sans")
        );
        let fonts = Fonts::new(&families, ChromeFont::Sans, ReadingFont::Mono);
        assert_eq!(
            (fonts.chrome.as_str(), fonts.reading.as_str()),
            ("Test Sans", "Test Mono")
        );
        assert!(fonts.reading_is_mono);
        assert_eq!("Omarchy".parse(), Ok(ChromeFont::Omarchy));
        assert_eq!(" mono ".parse(), Ok(ReadingFont::Mono));
        assert!("serif".parse::<ReadingFont>().is_err());
    }

    #[test]
    fn css_family_quotes_and_escapes() {
        assert_eq!(
            css_family("JetBrainsMono Nerd Font", "monospace"),
            "\"JetBrainsMono Nerd Font\", monospace"
        );
        assert_eq!(
            css_family("A\"; } * { color: red; \\\n", "sans-serif"),
            "\"A\\\"; } * { color: red; \\\\ \", sans-serif"
        );
        assert_eq!(css_family("monospace", "monospace"), "monospace");
    }
}
