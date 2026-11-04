//! omarchy-shell's `shell.toml` tokens that Sealwax follows: the control states and the
//! spacing scale (DESIGN.md "Theme tokens"), read the way `Commons/Style.qml` reads them.

use super::color::Rgb;
use super::palette::Palette;
use std::collections::BTreeMap;

/// `shell.toml` as omarchy-shell reads it: `section.key` → the raw value, with the user's
/// `~/.config/omarchy/shell.toml` merged over the theme's `current/theme/shell.toml`.
pub type ShellValues = BTreeMap<String, String>;

pub(super) fn get<'a>(values: &'a ShellValues, section: &str, key: &str) -> Option<&'a str> {
    values.get(&format!("{section}.{key}")).map(String::as_str)
}

/// JavaScript's `parseInt`: leading spaces, a sign and digits; what follows is ignored.
pub(super) fn parse_int(text: &str) -> Option<i64> {
    let text = text.trim_start();
    let digits = text
        .strip_prefix(['-', '+'])
        .unwrap_or(text)
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    let sign = usize::from(text.starts_with(['-', '+']));
    text[..sign + digits].parse().ok()
}

/// JavaScript's `parseFloat` for plain decimals: the longest leading `[-+]?\d*(\.\d+)?`.
pub(super) fn parse_float(text: &str) -> Option<f64> {
    let text = text.trim_start();
    let mut end = usize::from(text.starts_with(['-', '+']));
    let integer = text[end..].bytes().take_while(u8::is_ascii_digit).count();
    end += integer;
    let mut fraction = 0;
    if text[end..].starts_with('.') {
        fraction = text[end + 1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if fraction > 0 {
            end += 1 + fraction;
        }
    }
    if integer + fraction == 0 {
        return None;
    }
    text[..end].parse().ok()
}

/// JavaScript's `Number` for the values `shell.toml` holds: the whole value or nothing.
fn number(text: &str) -> Option<f64> {
    text.trim()
        .parse()
        .ok()
        .filter(|value: &f64| value.is_finite())
}

fn bool_token(text: Option<&str>, fallback: bool) -> bool {
    match text.map(|text| text.trim().to_ascii_lowercase()).as_deref() {
        Some("true" | "1" | "yes" | "on") => true,
        Some("false" | "0" | "no" | "off") => false,
        _ => fallback,
    }
}

/// One interactive state: its colour, the fill and border alphas over it, and the border
/// width in px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlState {
    pub color: Rgb,
    pub fill: f64,
    pub border_alpha: f64,
    pub border_width: u32,
}

/// omarchy-shell's `[controls]`: normal = foreground at 4 % with a 1 px border at 40 %; hover
/// and focus 8 %; selected 18 %; pressed 22 %; text selection 35 %.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlStates {
    pub normal: ControlState,
    pub hover: ControlState,
    pub focus: ControlState,
    pub selected: ControlState,
    pub pressed_color: Rgb,
    pub pressed_fill: f64,
    pub selection_color: Rgb,
    pub selection_fill: f64,
}

impl ControlStates {
    pub fn from_shell(values: &ShellValues, palette: &Palette) -> Self {
        let raw = |key: &str| {
            get(values, "controls", key)
                .or_else(|| get(values, "style", key))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let num = |key: &str, fallback: f64| raw(key).and_then(number).unwrap_or(fallback);
        let alpha = |key: &str, fallback: f64| num(key, fallback).clamp(0.0, 1.0);
        let width =
            |key: &str, fallback: u32| num(key, f64::from(fallback)).round().max(0.0) as u32;
        let color = |key: &str, fallback: Rgb| match raw(key) {
            Some(token) => state_color(token, palette).unwrap_or(fallback),
            None => fallback,
        };

        let normal_width = width("normal-border-width", 1);
        let normal = ControlState {
            color: color("normal-color", palette.foreground),
            fill: alpha("normal-fill-alpha", 0.04),
            border_alpha: alpha("normal-border-alpha", 0.4),
            border_width: normal_width,
        };
        let hover = ControlState {
            color: color("hover-cursor-color", palette.foreground),
            fill: alpha("hover-cursor-fill-alpha", 0.08),
            border_alpha: alpha("hover-cursor-border-alpha", 0.25),
            border_width: width("hover-cursor-border-width", normal_width),
        };
        let focus_color = match raw("focus-color").map(str::to_ascii_lowercase).as_deref() {
            Some("hover" | "hover-cursor" | "inherit") | None => hover.color,
            Some(_) => color("focus-color", hover.color),
        };
        let focus = ControlState {
            color: focus_color,
            fill: alpha("focus-fill-alpha", hover.fill),
            border_alpha: alpha("focus-border-alpha", hover.border_alpha),
            border_width: width("focus-border-width", hover.border_width),
        };
        let selected = ControlState {
            color: color("selected-color", palette.foreground),
            fill: alpha("selected-fill-alpha", 0.18),
            border_alpha: alpha("selected-border-alpha", 1.0),
            border_width: width("selected-border-width", 0),
        };
        Self {
            normal,
            hover,
            focus,
            selected,
            pressed_color: color("pressed-color", hover.color),
            pressed_fill: alpha("pressed-fill-alpha", 0.22),
            selection_color: color("selection-color", palette.foreground),
            selection_fill: alpha("selection-fill-alpha", 0.35),
        }
    }
}

/// A state colour token: a palette role, `transparent` (as the background), or a colour,
/// of which a Hyprland-style gradient contributes its first stop.
fn state_color(token: &str, palette: &Palette) -> Option<Rgb> {
    match token.to_ascii_lowercase().as_str() {
        "foreground" | "text" => Some(palette.foreground),
        "accent" => Some(palette.accent),
        "urgent" => Some(palette.red),
        "background" | "transparent" => Some(palette.background),
        _ => Rgb::parse(token.split_whitespace().next()?),
    }
}

/// omarchy-shell's spacing tokens and their sizes at the default text size, in px.
pub const SPACING_TOKENS: [(&str, u32); 20] = [
    ("xxs", 2),
    ("xs", 3),
    ("sm", 4),
    ("md", 6),
    ("lg", 8),
    ("xl", 10),
    ("xxl", 12),
    ("xxxl", 14),
    ("huge", 18),
    ("control-gap", 8),
    ("control-padding-x", 10),
    ("control-padding-y", 6),
    ("input-padding-y", 7),
    ("control-height", 28),
    ("row-gap", 8),
    ("row-padding-x", 12),
    ("label-gap", 4),
    ("panel-gap", 14),
    ("panel-padding", 18),
    ("popup-padding", 14),
];

/// The spacing scale: `[spacing] scale`, times the text size over 12 px while
/// `scale-with-font` is on (the default); per-token overrides are px and not scaled.
#[derive(Debug, Clone, PartialEq)]
pub struct Spacing {
    pub scale: f64,
    tokens: Vec<(&'static str, u32)>,
}

impl Spacing {
    pub fn from_shell(values: &ShellValues, base_size: u32) -> Self {
        let scale = get(values, "spacing", "scale")
            .and_then(parse_float)
            .filter(|scale| *scale >= 0.0)
            .unwrap_or(1.0);
        let with_font = bool_token(get(values, "spacing", "scale-with-font"), true);
        let scale = if with_font {
            scale * f64::from(base_size) / f64::from(super::typography::DEFAULT_BASE_SIZE)
        } else {
            scale
        };
        let mut spacing = Self {
            scale,
            tokens: Vec::with_capacity(SPACING_TOKENS.len()),
        };
        for (name, px) in SPACING_TOKENS {
            let size = get(values, "spacing", name)
                .and_then(parse_float)
                .filter(|size| *size >= 0.0)
                .map_or_else(|| spacing.space(px), |size| size.round() as u32);
            spacing.tokens.push((name, size));
        }
        spacing
    }

    /// omarchy-shell's `space(px)`: a size given at the default text size, scaled, never
    /// below 1 px unless it is 0.
    pub fn space(&self, px: u32) -> u32 {
        let scaled = f64::from(px) * self.scale;
        if scaled <= 0.0 {
            0
        } else {
            (scaled.round() as u32).max(1)
        }
    }

    /// A named token, such as `lg` or `control-height`.
    pub fn token(&self, name: &str) -> Option<u32> {
        self.tokens
            .iter()
            .find(|(token, _)| *token == name)
            .map(|(_, size)| *size)
    }

    pub fn tokens(&self) -> &[(&'static str, u32)] {
        &self.tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> ShellValues {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn numbers_parse_like_javascript() {
        assert_eq!(parse_int("12"), Some(12));
        assert_eq!(parse_int(" 12.9px"), Some(12));
        assert_eq!(parse_int("-3"), Some(-3));
        assert_eq!(parse_int("px"), None);
        assert_eq!(parse_float("1.25 2"), Some(1.25));
        assert_eq!(parse_float("3."), Some(3.0));
        assert_eq!(parse_float(".5x"), Some(0.5));
        assert_eq!(parse_float("-."), None);
        assert_eq!(number("0.04"), Some(0.04));
        assert_eq!(number("1 2"), None);
    }

    #[test]
    fn control_states_default_to_omarchy_shell() {
        let palette = Palette::DARK;
        let states = ControlStates::from_shell(&ShellValues::new(), &palette);
        assert_eq!(states.normal.color, palette.foreground);
        assert_eq!(states.normal.fill, 0.04);
        assert_eq!(states.normal.border_alpha, 0.4);
        assert_eq!(states.normal.border_width, 1);
        assert_eq!(states.hover.fill, 0.08);
        assert_eq!(states.focus.fill, 0.08);
        assert_eq!(states.selected.fill, 0.18);
        assert_eq!(states.selected.border_width, 0);
        assert_eq!(states.pressed_fill, 0.22);
        assert_eq!(states.selection_fill, 0.35);
    }

    #[test]
    fn control_states_follow_the_file() {
        let palette = Palette::DARK;
        let states = ControlStates::from_shell(
            &values(&[
                ("controls.normal-fill-alpha", "0.1"),
                ("controls.selected-color", "accent"),
                ("controls.hover-cursor-color", "#ff0000"),
                ("controls.focus-color", "inherit"),
                ("controls.pressed-fill-alpha", "7"),
                ("style.selection-fill-alpha", "0.5"),
                ("controls.normal-border-width", "1 2"),
            ]),
            &palette,
        );
        assert_eq!(states.normal.fill, 0.1);
        assert_eq!(states.selected.color, palette.accent);
        assert_eq!(states.hover.color, Rgb::from_hex(0xff0000));
        assert_eq!(states.focus.color, Rgb::from_hex(0xff0000));
        assert_eq!(states.pressed_fill, 1.0);
        assert_eq!(states.selection_fill, 0.5);
        assert_eq!(states.normal.border_width, 1);
    }

    #[test]
    fn spacing_scales_with_the_text_size() {
        let default = Spacing::from_shell(&ShellValues::new(), 12);
        let tokens: Vec<u32> = SPACING_TOKENS.iter().map(|(_, px)| *px).collect();
        let sizes: Vec<u32> = default.tokens().iter().map(|(_, px)| *px).collect();
        assert_eq!(sizes, tokens);
        assert_eq!(default.token("control-height"), Some(28));

        let large = Spacing::from_shell(&ShellValues::new(), 16);
        assert_eq!(large.token("lg"), Some(11));
        assert_eq!(large.token("control-height"), Some(37));
        assert_eq!(large.space(32), 43);

        let pinned = Spacing::from_shell(
            &values(&[
                ("spacing.scale", "2"),
                ("spacing.scale-with-font", "false"),
                ("spacing.lg", "5"),
            ]),
            16,
        );
        assert_eq!(pinned.token("lg"), Some(5));
        assert_eq!(pinned.token("md"), Some(12));
        assert_eq!(pinned.space(0), 0);
        assert_eq!(pinned.token("nope"), None);
    }
}
