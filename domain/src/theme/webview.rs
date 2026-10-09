//! HTML mail inside the theme (DESIGN.md; ADR-011): simple HTML takes the theme's
//! background, text, accent links and muted quotes; HTML with colours of its own keeps its
//! design on a light "paper" card. The reader adds the stylesheet as a user stylesheet,
//! sets the view background and WebKit's default fonts from here, and restyles every view
//! when the theme changes.

use super::Theme;
use super::color::Rgb;
use super::palette::Mode;
use super::typography::{READING_LINE_HEIGHT, css_family};
use std::fmt::Write as _;

/// The paper card's surface and the text it starts with; the message's own CSS wins.
pub const PAPER: Rgb = Rgb::WHITE;
pub const PAPER_TEXT: Rgb = Rgb::from_hex(0x1f1f1f);

/// Rules every reader view gets (ADR-011 amendment, S2): the root and body heights follow
/// their content, so the reported height never includes the viewport it was measured in.
/// `max-content` rather than `auto`: in quirks mode an `auto` root and body fill the
/// viewport.
pub const MEASURE_CSS: &str =
    "html,body{height:max-content!important;min-height:0!important;max-height:none!important}";

/// The custom property that carries the theme's background into a paper body, for the space
/// beside a design narrower than the view.
pub const MARGINS_COLOR: &str = "--sealwax-margins-color";

/// How a body is shown, decided by the reader's colour heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyStyle {
    /// No colours of its own: the theme's background, text and accent links.
    Themed,
    /// Brings its own colours: kept as designed on the paper card.
    Paper,
}

/// WebKit's default fonts for reader and composer views (`WebKitSettings`), set explicitly
/// because WebKit would otherwise pick its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WebViewFonts {
    /// `default-font-family`: `sans-serif`, or `monospace` when message text uses the Omarchy
    /// font.
    pub default_family: &'static str,
    /// `sans-serif-font-family`: fontconfig's `sans-serif`.
    pub sans_serif: String,
    /// `monospace-font-family`: the Omarchy font.
    pub monospace: String,
    /// `default-font-size` in px.
    pub default_size: u32,
    /// `default-monospace-font-size` in px.
    pub monospace_size: u32,
}

impl Theme {
    /// The user stylesheet for one HTML body. Author styles in the message win over it, except
    /// that themed links take the accent and paper stays in the light scheme: WebKit would
    /// paint a dark canvas behind a light design that declares `color-scheme: light dark`.
    /// Paper paints no background of its own, so a message's body background reaches the
    /// canvas as in a browser (a dark-mode design fills the card), and the view's white shows
    /// where the message paints none. Its `--sealwax-margins-color` is the theme's background,
    /// for the reader's script that puts a narrow design's column on it.
    pub fn webview_css(&self, style: BodyStyle) -> String {
        let mut css = String::with_capacity(1024);
        css.push_str(MEASURE_CSS);
        css.push('\n');
        match style {
            BodyStyle::Themed => self.themed_css(&mut css),
            BodyStyle::Paper => {
                let _ = write!(
                    css,
                    ":root{{color-scheme:light!important;\
                     {MARGINS_COLOR}:{margins}!important}}\n\
                     html{{color:{PAPER_TEXT}}}\n",
                    margins = self.palette.background,
                );
            }
        }
        css
    }

    fn themed_css(&self, css: &mut String) {
        let p = &self.palette;
        let fonts = self.webview_fonts();
        let generic = fonts.default_family;
        let scheme = match p.mode {
            Mode::Dark => "dark",
            Mode::Light => "light",
        };
        let border = self.ui.border;
        let selection = self
            .controls
            .selection_color
            .with_alpha(self.controls.selection_fill);
        let _ = write!(
            css,
            ":root{{color-scheme:{scheme}}}\n\
             html{{background-color:{background};color:{fg};font:{size}px/{line} {family}}}\n\
             body{{margin:0;padding:0;overflow-wrap:anywhere}}\n\
             a{{color:{accent}!important}}\n\
             blockquote{{margin:0 0 0 2px;padding-left:12px;border-left:3px solid {border};color:{secondary}}}\n\
             pre,code,kbd,samp,tt{{font-family:{mono};font-size:{mono_size}px}}\n\
             pre{{white-space:pre-wrap}}\n\
             hr{{border:0;border-top:1px solid {border}}}\n\
             img{{max-width:100%;height:auto}}\n\
             table{{border-collapse:collapse}}\n\
             td,th{{border-color:{border}}}\n\
             ::selection{{background-color:{selection}}}\n",
            background = p.background,
            fg = p.foreground,
            size = fonts.default_size,
            line = READING_LINE_HEIGHT,
            family = css_family(&self.fonts.reading, generic),
            accent = p.accent,
            secondary = self.mail.secondary_text,
            mono = css_family(&fonts.monospace, "monospace"),
            mono_size = fonts.monospace_size,
        );
    }

    /// WebKit's default fonts and sizes for message text.
    pub fn webview_fonts(&self) -> WebViewFonts {
        let reading = self.scale.reading;
        WebViewFonts {
            default_family: if self.fonts.reading_is_mono {
                "monospace"
            } else {
                "sans-serif"
            },
            sans_serif: self.fonts.sans.clone(),
            monospace: self.fonts.monospace.clone(),
            default_size: reading,
            monospace_size: ((f64::from(reading) * 13.0 / 15.0).round() as u32).max(1),
        }
    }

    /// The view's background: the theme's for themed bodies, white paper otherwise.
    pub fn webview_background(&self, style: BodyStyle) -> Rgb {
        match style {
            BodyStyle::Themed => self.palette.background,
            BodyStyle::Paper => PAPER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FontFamilies, ReadingFont, ThemeInputs, ThemeOptions};
    use super::*;

    fn theme(mode: Mode, reading: ReadingFont) -> Theme {
        let inputs = ThemeInputs {
            fonts: FontFamilies {
                monospace: "Test Mono".into(),
                sans: "Test Sans".into(),
            },
            ..ThemeInputs::builtin(mode)
        };
        let options = ThemeOptions {
            reading_font: reading,
            ..ThemeOptions::default()
        };
        Theme::new(&inputs, &options)
    }

    #[test]
    fn themed_bodies_take_the_theme() {
        let theme = theme(Mode::Dark, ReadingFont::Sans);
        let p = &theme.palette;
        let css = theme.webview_css(BodyStyle::Themed);
        assert!(css.starts_with(MEASURE_CSS));
        for expected in [
            ":root{color-scheme:dark}".to_owned(),
            format!("color:{}", p.foreground),
            format!("a{{color:{}!important}}", p.accent),
            format!("color:{}}}", theme.mail.secondary_text),
            "font:15px/1.55 \"Test Sans\", sans-serif".to_owned(),
            "font-family:\"Test Mono\", monospace;font-size:13px".to_owned(),
        ] {
            assert!(css.contains(&expected), "{expected}\n{css}");
        }
        assert_eq!(theme.webview_background(BodyStyle::Themed), p.background);
    }

    #[test]
    fn paper_bodies_keep_their_design() {
        let theme = theme(Mode::Dark, ReadingFont::Sans);
        let css = theme.webview_css(BodyStyle::Paper);
        assert!(css.starts_with(MEASURE_CSS));
        assert!(css.contains(":root{color-scheme:light!important;"));
        assert!(css.contains(&format!(
            "{MARGINS_COLOR}:{}!important}}",
            theme.palette.background
        )));
        assert!(css.contains("html{color:#1f1f1f}"));
        assert!(
            !css.contains("background"),
            "the message's own body background reaches the canvas"
        );
        assert!(!css.contains(&theme.palette.foreground.to_string()));
        assert_eq!(theme.webview_background(BodyStyle::Paper), PAPER);
    }

    #[test]
    fn webkit_defaults_follow_the_reading_font() {
        let sans = theme(Mode::Light, ReadingFont::Sans).webview_fonts();
        assert_eq!(
            sans,
            WebViewFonts {
                default_family: "sans-serif",
                sans_serif: "Test Sans".into(),
                monospace: "Test Mono".into(),
                default_size: 15,
                monospace_size: 13,
            }
        );
        let mono = theme(Mode::Light, ReadingFont::Mono);
        assert_eq!(mono.webview_fonts().default_family, "monospace");
        assert!(
            mono.webview_css(BodyStyle::Themed)
                .contains("font:15px/1.55 \"Test Mono\", monospace")
        );
    }
}
