use super::color::Rgb;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub const fn from_dark(dark: bool) -> Self {
        if dark { Self::Dark } else { Self::Light }
    }
}

const SHORT_NAMES: [(&str, &str); 8] = [
    ("background", "bg"),
    ("dark_background", "dark_bg"),
    ("darker_background", "darker_bg"),
    ("lighter_background", "lighter_bg"),
    ("foreground", "fg"),
    ("dark_foreground", "dark_fg"),
    ("light_foreground", "light_fg"),
    ("bright_foreground", "bright_fg"),
];

const ANSI_NAMES: [(&str, &str); 16] = [
    ("color0", "background"),
    ("color1", "red"),
    ("color2", "green"),
    ("color3", "yellow"),
    ("color4", "blue"),
    ("color5", "magenta"),
    ("color6", "cyan"),
    ("color7", "foreground"),
    ("color8", "muted"),
    ("color9", "bright_red"),
    ("color10", "bright_green"),
    ("color11", "bright_yellow"),
    ("color12", "bright_blue"),
    ("color13", "bright_magenta"),
    ("color14", "bright_cyan"),
    ("color15", "bright_foreground"),
];

const ACCENTS: [&str; 6] = ["red", "yellow", "green", "cyan", "blue", "magenta"];

/// Applies `omarchy-theme-color --all`'s alias and fallback cascade to a raw `colors.toml` map,
/// so the result matches the script's output minus its empty values (Stet's port).
///
/// Two deliberate differences: shades are derived only from `#rrggbb` values (the script
/// mixes garbage into `#333333`), and a `light.mode` file is the caller's job (insert
/// `mode = "light"` when neither `mode` nor `theme_type` is set).
pub fn resolve_colors(raw: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut colors = Cascade(
        raw.iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );

    for (name, short) in SHORT_NAMES {
        colors.fill(name, colors.first(&[short]));
    }
    colors.fill("background", colors.first(&["color0"]));
    colors.fill("foreground", colors.first(&["color7"]));
    for (ansi, name) in [("color0", "background"), ("color7", "foreground")] {
        if let Some(value) = colors.first(&[name]) {
            colors.set(ansi, Some(value));
        }
    }
    for (ansi, name) in ANSI_NAMES {
        if !matches!(ansi, "color0" | "color7" | "color8" | "color15") {
            colors.fill(name, colors.first(&[ansi]));
        }
    }
    colors.fill("magenta", colors.first(&["purple"]));
    colors.fill("bright_magenta", colors.first(&["bright_purple"]));

    colors.fill("light_foreground", colors.first(&["color7", "foreground"]));
    colors.fill(
        "bright_foreground",
        colors.first(&["color15", "foreground"]),
    );
    colors.set("cursor", colors.first(&["bright_foreground"]));
    colors.fill(
        "lighter_background",
        colors.first(&["color0", "background"]),
    );
    colors.fill("dark_foreground", colors.first(&["color8", "foreground"]));
    colors.fill("muted", colors.first(&["color8", "dark_foreground"]));
    colors.fill(
        "selection",
        colors.first(&["selection_background", "color8", "color0", "background"]),
    );
    colors.fill("selection_background", colors.first(&["selection"]));
    colors.fill("selection_foreground", colors.first(&["bright_foreground"]));
    colors.fill("orange", colors.first(&["yellow"]));
    colors.fill("brown", colors.mixed("orange", Rgb::BLACK, 50));

    colors.fill(
        "dark_background",
        colors.mixed("background", Rgb::BLACK, 25),
    );
    colors.fill(
        "darker_background",
        colors.mixed("background", Rgb::BLACK, 50),
    );
    for name in ACCENTS {
        let bright = format!("bright_{name}");
        colors.fill(&bright, colors.mixed(name, Rgb::WHITE, 20));
    }
    colors.fill("purple", colors.first(&["magenta"]));
    colors.fill("bright_purple", colors.first(&["bright_magenta"]));

    for (ansi, name) in ANSI_NAMES {
        colors.fill(ansi, colors.first(&[name]));
    }
    for (name, short) in SHORT_NAMES {
        if let Some(value) = colors.first(&[name]) {
            colors.set(short, Some(value));
        }
    }

    colors.fill("mode", colors.first(&["theme_type"]));
    if colors.first(&["mode"]).is_none() {
        let light = colors
            .first(&["background"])
            .filter(|value| value.len() == 7)
            .and_then(|value| Rgb::parse(&value))
            .is_some_and(|bg| u16::from(bg.r) + u16::from(bg.g) + u16::from(bg.b) > 382);
        let mode = if light { Mode::Light } else { Mode::Dark };
        colors.set("mode", Some(mode.as_str().to_owned()));
    }
    colors.set("theme_type", colors.first(&["mode"]));
    colors.0
}

struct Cascade(BTreeMap<String, String>);

impl Cascade {
    fn first(&self, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|key| self.0.get(*key).filter(|value| !value.is_empty()))
            .cloned()
    }

    fn fill(&mut self, key: &str, value: Option<String>) {
        if self.first(&[key]).is_none() {
            self.set(key, value);
        }
    }

    fn set(&mut self, key: &str, value: Option<String>) {
        match value {
            Some(value) => self.0.insert(key.to_owned(), value),
            None => self.0.remove(key),
        };
    }

    fn mixed(&self, key: &str, toward: Rgb, percent: u8) -> Option<String> {
        let value = self.first(&[key])?;
        let hex = value.strip_prefix('#')?;
        if !matches!(hex.len(), 6 | 8) {
            return None;
        }
        Some(Rgb::parse(&value)?.mix(toward, percent).to_string())
    }
}

/// The semantic Omarchy palette with every fallback applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub mode: Mode,
    pub accent: Rgb,
    pub background: Rgb,
    pub dark_background: Rgb,
    pub darker_background: Rgb,
    pub lighter_background: Rgb,
    pub foreground: Rgb,
    pub dark_foreground: Rgb,
    pub light_foreground: Rgb,
    pub bright_foreground: Rgb,
    pub muted: Rgb,
    pub selection_background: Rgb,
    pub selection_foreground: Rgb,
    pub red: Rgb,
    pub yellow: Rgb,
    pub orange: Rgb,
    pub green: Rgb,
    pub cyan: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
}

impl Palette {
    /// Built-in GNOME-palette colours for running outside Omarchy, and for keys a theme lacks.
    pub const DARK: Self = Self {
        mode: Mode::Dark,
        accent: Rgb::from_hex(0x3584e4),
        background: Rgb::from_hex(0x1d1d20),
        dark_background: Rgb::from_hex(0x161618),
        darker_background: Rgb::from_hex(0x0f0f10),
        lighter_background: Rgb::from_hex(0x2e2e32),
        foreground: Rgb::from_hex(0xdeddda),
        dark_foreground: Rgb::from_hex(0x9a9996),
        light_foreground: Rgb::from_hex(0xdeddda),
        bright_foreground: Rgb::from_hex(0xffffff),
        muted: Rgb::from_hex(0x77767b),
        selection_background: Rgb::from_hex(0x254165),
        selection_foreground: Rgb::from_hex(0xffffff),
        red: Rgb::from_hex(0xf66151),
        yellow: Rgb::from_hex(0xf8e45c),
        orange: Rgb::from_hex(0xffa348),
        green: Rgb::from_hex(0x57e389),
        cyan: Rgb::from_hex(0x5bc8af),
        blue: Rgb::from_hex(0x62a0ea),
        magenta: Rgb::from_hex(0xdc8add),
    };

    pub const LIGHT: Self = Self {
        mode: Mode::Light,
        accent: Rgb::from_hex(0x3584e4),
        background: Rgb::from_hex(0xffffff),
        dark_background: Rgb::from_hex(0xf5f5f5),
        darker_background: Rgb::from_hex(0xdedede),
        lighter_background: Rgb::from_hex(0xebebed),
        foreground: Rgb::from_hex(0x241f31),
        dark_foreground: Rgb::from_hex(0x77767b),
        light_foreground: Rgb::from_hex(0x3d3846),
        bright_foreground: Rgb::from_hex(0x000000),
        muted: Rgb::from_hex(0x9a9996),
        selection_background: Rgb::from_hex(0xcde0f8),
        selection_foreground: Rgb::from_hex(0x241f31),
        red: Rgb::from_hex(0xc01c28),
        yellow: Rgb::from_hex(0x9c6e03),
        orange: Rgb::from_hex(0xc64600),
        green: Rgb::from_hex(0x26a269),
        cyan: Rgb::from_hex(0x218787),
        blue: Rgb::from_hex(0x1a5fb4),
        magenta: Rgb::from_hex(0x813d9c),
    };

    pub const fn builtin(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self::DARK,
            Mode::Light => Self::LIGHT,
        }
    }

    /// Builds the palette from a raw `colors.toml` map by applying [`resolve_colors`] first.
    pub fn from_colors(raw: &BTreeMap<String, String>) -> Self {
        Self::from_resolved(&resolve_colors(raw))
    }

    /// Builds the palette from `omarchy-theme-color --all` output or [`resolve_colors`] output.
    /// The cascade is not re-run: like the script, it is not idempotent. Keys still missing or
    /// unparsable come from the built-in palette for the mode; `accent` tries `blue` first.
    pub fn from_resolved(resolved: &BTreeMap<String, String>) -> Self {
        let mode = if resolved
            .get("mode")
            .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("light"))
        {
            Mode::Light
        } else {
            Mode::Dark
        };
        let base = Self::builtin(mode);
        let parsed = |key: &str| resolved.get(key).and_then(|value| Rgb::parse(value));
        let color = |key: &str, fallback: Rgb| parsed(key).unwrap_or(fallback);
        Self {
            mode,
            accent: parsed("accent")
                .or_else(|| parsed("blue"))
                .unwrap_or(base.accent),
            background: color("background", base.background),
            dark_background: color("dark_background", base.dark_background),
            darker_background: color("darker_background", base.darker_background),
            lighter_background: color("lighter_background", base.lighter_background),
            foreground: color("foreground", base.foreground),
            dark_foreground: color("dark_foreground", base.dark_foreground),
            light_foreground: color("light_foreground", base.light_foreground),
            bright_foreground: color("bright_foreground", base.bright_foreground),
            muted: color("muted", base.muted),
            selection_background: color("selection_background", base.selection_background),
            selection_foreground: color("selection_foreground", base.selection_foreground),
            red: color("red", base.red),
            yellow: color("yellow", base.yellow),
            orange: color("orange", base.orange),
            green: color("green", base.green),
            cyan: color("cyan", base.cyan),
            blue: color("blue", base.blue),
            magenta: color("magenta", base.magenta),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// A pre-semantic theme: ANSI names only, no accent, no mode.
    const LEGACY: &[(&str, &str)] = &[
        ("color0", "#282828"),
        ("color1", "#cc241d"),
        ("color2", "#98971a"),
        ("color3", "#d79921"),
        ("color4", "#458588"),
        ("color5", "#b16286"),
        ("color6", "#689d6a"),
        ("color7", "#ebdbb2"),
        ("color8", "#928374"),
        ("color15", "#fbf1c7"),
    ];

    #[test]
    fn resolves_an_ansi_only_theme_like_the_script() {
        let resolved = resolve_colors(&colors(&[("color0", "#101010"), ("color1", "#ff0000")]));
        let expected = colors(&[
            ("background", "#101010"),
            ("bg", "#101010"),
            ("bright_red", "#ff3333"),
            ("color0", "#101010"),
            ("color1", "#ff0000"),
            ("color9", "#ff3333"),
            ("dark_background", "#0c0c0c"),
            ("dark_bg", "#0c0c0c"),
            ("darker_background", "#080808"),
            ("darker_bg", "#080808"),
            ("lighter_background", "#101010"),
            ("lighter_bg", "#101010"),
            ("mode", "dark"),
            ("red", "#ff0000"),
            ("selection", "#101010"),
            ("selection_background", "#101010"),
            ("theme_type", "dark"),
        ]);
        assert_eq!(resolved, expected);
    }

    #[test]
    fn canonical_names_win_over_short_names() {
        let resolved = resolve_colors(&colors(&[("bg", "#111111"), ("background", "#222222")]));
        assert_eq!(resolved["background"], "#222222");
        assert_eq!(resolved["bg"], "#222222");
        assert_eq!(resolved["color0"], "#222222");

        let resolved = resolve_colors(&colors(&[("fg", "#dddddd")]));
        assert_eq!(resolved["foreground"], "#dddddd");
        assert_eq!(resolved["color7"], "#dddddd");
    }

    #[test]
    fn empty_values_count_as_missing() {
        let resolved = resolve_colors(&colors(&[("background", ""), ("color0", "#123456")]));
        assert_eq!(resolved["background"], "#123456");
        assert!(!resolve_colors(&colors(&[("accent", "")])).contains_key("accent"));
    }

    #[test]
    fn cursor_always_follows_bright_foreground() {
        let resolved = resolve_colors(&colors(&[
            ("cursor", "#123456"),
            ("bright_foreground", "#abcdef"),
        ]));
        assert_eq!(resolved["cursor"], "#abcdef");
        assert!(!resolve_colors(&colors(&[("cursor", "#123456")])).contains_key("cursor"));
    }

    #[test]
    fn purple_aliases_magenta_both_ways() {
        let resolved = resolve_colors(&colors(&[("purple", "#aa00aa")]));
        assert_eq!(resolved["magenta"], "#aa00aa");
        assert_eq!(resolved["color5"], "#aa00aa");
        assert_eq!(resolved["bright_magenta"], "#bb33bb");
        assert_eq!(resolved["bright_purple"], "#bb33bb");

        let resolved = resolve_colors(&colors(&[("magenta", "#aa00aa")]));
        assert_eq!(resolved["purple"], "#aa00aa");
    }

    #[test]
    fn mode_comes_from_mode_then_theme_type_then_luminance() {
        let mode = |pairs: &[(&str, &str)]| resolve_colors(&colors(pairs))["mode"].clone();
        assert_eq!(
            mode(&[("mode", "light"), ("background", "#000000")]),
            "light"
        );
        assert_eq!(mode(&[("theme_type", "light")]), "light");
        assert_eq!(mode(&[("background", "#808080")]), "light");
        assert_eq!(mode(&[("background", "#7f7f7f")]), "dark");
        assert_eq!(mode(&[("background", "#ffffffff")]), "dark");
        assert_eq!(mode(&[]), "dark");
        let resolved = resolve_colors(&colors(&[("theme_type", "light")]));
        assert_eq!(resolved["theme_type"], "light");
    }

    #[test]
    fn shades_are_derived_only_from_hex_values() {
        let resolved = resolve_colors(&colors(&[("background", "rgb(10, 10, 10)")]));
        assert!(!resolved.contains_key("dark_background"));
        let resolved = resolve_colors(&colors(&[("orange", "#ffa348")]));
        assert_eq!(resolved["brown"], "#805224");
    }

    #[test]
    fn empty_input_gives_the_builtin_dark_palette() {
        assert_eq!(Palette::from_colors(&BTreeMap::new()), Palette::DARK);
        assert_eq!(
            Palette::from_colors(&colors(&[("mode", "light")])),
            Palette::LIGHT
        );
    }

    #[test]
    fn accent_falls_back_to_blue_then_the_builtin_accent() {
        let palette = Palette::from_colors(&colors(&[("blue", "#0000ff")]));
        assert_eq!(palette.accent, Rgb::from_hex(0x0000ff));
        let palette = Palette::from_colors(&colors(&[("color4", "#0000aa")]));
        assert_eq!(palette.accent, Rgb::from_hex(0x0000aa));
        let palette = Palette::from_colors(&colors(&[("background", "#101010")]));
        assert_eq!(palette.accent, Palette::DARK.accent);
    }

    #[test]
    fn legacy_theme_fills_every_key() {
        let palette = Palette::from_colors(&colors(LEGACY));
        assert_eq!(palette.mode, Mode::Dark);
        assert_eq!(palette.background, Rgb::from_hex(0x282828));
        assert_eq!(palette.magenta, Rgb::from_hex(0xb16286));
        assert_eq!(palette.orange, palette.yellow);
        assert_eq!(palette.accent, Rgb::from_hex(0x458588));
        assert_eq!(palette.muted, Rgb::from_hex(0x928374));
    }

    #[test]
    fn unparsable_values_fall_back_per_key() {
        let palette = Palette::from_colors(&colors(&[("red", "tomato"), ("mode", "Light")]));
        assert_eq!(palette.mode, Mode::Light);
        assert_eq!(palette.red, Palette::LIGHT.red);
    }
}
