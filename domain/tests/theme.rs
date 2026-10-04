//! The theme over Omarchy's 22 stock palettes, resolved by `omarchy-theme-color --all` and
//! stored in `tests/theme/` (regenerate with `bash tests/theme/regenerate.sh`), so nothing
//! here reads the system.

use proptest::prelude::*;
use sealwax_domain::theme::{
    AvatarTint, BodyStyle, FontFamilies, MEASURE_CSS, Mode, Palette, Rgb, ShellValues, Theme,
    ThemeInputs, ThemeOptions, ThemeSource, avatar_index, avatar_tints, hue_distance,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

/// WCAG AA for normal text: the avatar initials must reach it on every tint.
const INITIALS_CONTRAST: f64 = 4.5;
/// WCAG's floor for large text and controls: text on the accent fill.
const ACCENT_TEXT_CONTRAST: f64 = 3.0;
/// Tints are 51° apart by design; quantizing to 8-bit channels moves a hue by about 1°.
const TINT_HUE_GAP: f64 = 49.0;
/// The smallest RGB distance between two tints over every accent hue and both modes was
/// 24.8 when this was written (the test below samples every degree).
const TINT_RGB_GAP: f64 = 20.0;

const SNAPSHOTTED: [&str; 5] = [
    "matte-black",
    "tokyo-night",
    "catppuccin-latte",
    "flexoki-light",
    "gruvbox",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/theme")
}

fn theme_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn resolved(name: &str) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(fixtures_dir().join(name).join("resolved.tsv")).unwrap();
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn theme(name: &str) -> Theme {
    let inputs = ThemeInputs {
        source: ThemeSource::Omarchy(name.to_owned()),
        palette: Palette::from_resolved(&resolved(name)),
        shell: ShellValues::new(),
        fonts: FontFamilies {
            monospace: "JetBrainsMono Nerd Font".to_owned(),
            sans: "Liberation Sans".to_owned(),
        },
        hyprland_rounding: Some(8),
    };
    Theme::new(&inputs, &ThemeOptions::default())
}

fn rgb_distance(a: Rgb, b: Rgb) -> f64 {
    [(a.r, b.r), (a.g, b.g), (a.b, b.b)]
        .iter()
        .map(|&(x, y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn assert_tints_apart(tints: &[AvatarTint], context: &str) {
    for (index, a) in tints.iter().enumerate() {
        for (offset, b) in tints[index + 1..].iter().enumerate() {
            let (x, y) = (a.background, b.background);
            let pair = format!("{context}: tints {index} and {}", index + offset + 1);
            assert!(
                hue_distance(x.to_hsl().0, y.to_hsl().0) >= TINT_HUE_GAP,
                "{pair}: {x} {y}"
            );
            assert!(rgb_distance(x, y) >= TINT_RGB_GAP, "{pair}: {x} {y}");
        }
    }
}

/// The mail roles and the status colours, one per line, for snapshots.
fn roles(theme: &Theme) -> String {
    let mut text = String::new();
    let m = &theme.mail;
    let ui = &theme.ui;
    let _ = writeln!(text, "mode {}", theme.mode().as_str());
    let _ = writeln!(text, "secondary_text {}", m.secondary_text);
    let _ = writeln!(text, "star {}", m.star);
    let _ = writeln!(text, "unread {}", m.unread);
    let _ = writeln!(
        text,
        "selected_chip {} on {}",
        m.selected_chip, m.selected_chip_background
    );
    let _ = writeln!(text, "accent_text {}", ui.accent_text);
    let _ = writeln!(
        text,
        "error {} warning {} success {}",
        ui.error, ui.warning, ui.success
    );
    for (index, tint) in m.avatars.iter().enumerate() {
        let _ = writeln!(
            text,
            "avatar {index} {} on {}",
            tint.foreground, tint.background
        );
    }
    text
}

#[test]
fn all_stock_themes_are_present() {
    let names = theme_names();
    assert_eq!(names.len(), 22, "{names:?}");
    for name in SNAPSHOTTED {
        assert!(names.iter().any(|known| known == name), "{name}");
    }
}

#[test]
fn modes_match_the_files() {
    let light: Vec<String> = theme_names()
        .into_iter()
        .filter(|name| theme(name).mode() == Mode::Light)
        .collect();
    assert_eq!(
        light,
        [
            "catppuccin-latte",
            "flexoki-light",
            "lupine",
            "rose-pine",
            "white"
        ]
    );
    for name in theme_names() {
        assert_eq!(
            theme(&name).mode().as_str(),
            resolved(&name)["mode"],
            "{name}"
        );
    }
}

#[test]
fn snapshots() {
    for name in SNAPSHOTTED {
        let theme = theme(name);
        insta::assert_snapshot!(format!("{name}_css"), theme.css());
        insta::assert_snapshot!(format!("{name}_roles"), roles(&theme));
        insta::assert_snapshot!(
            format!("{name}_webview_themed"),
            theme.webview_css(BodyStyle::Themed)
        );
    }
    insta::assert_snapshot!(
        "webview_paper",
        theme("matte-black").webview_css(BodyStyle::Paper)
    );
}

#[test]
fn tints_stay_distinct_on_single_hue_themes() {
    let matte = theme("matte-black");
    let p = &matte.palette;
    let own = [p.red, p.yellow, p.green, p.blue, p.magenta].map(|color| color.to_hsl().0);
    let spread_from_accent = own
        .iter()
        .map(|hue| hue_distance(*hue, p.accent.to_hsl().0))
        .fold(0.0, f64::max);
    assert!(
        spread_from_accent < 45.0,
        "matte-black is no longer single-hue: {own:?}"
    );
    assert_tints_apart(&matte.mail.avatars, "matte-black");
    for name in theme_names() {
        assert_tints_apart(&theme(&name).mail.avatars, &name);
    }
}

#[test]
fn tints_stay_distinct_for_every_accent_hue() {
    for mode in [Mode::Dark, Mode::Light] {
        for hue in 0..360 {
            let accent = Rgb::from_hsl(f64::from(hue), 0.7, 0.5);
            let context = format!("{} accent hue {hue}", mode.as_str());
            assert_tints_apart(&avatar_tints(accent, mode), &context);
        }
    }
    assert_tints_apart(
        &avatar_tints(Rgb::from_hex(0x6e6e6e), Mode::Light),
        "grey accent",
    );
}

#[test]
fn initials_clear_aa_contrast_on_every_tint() {
    for name in theme_names() {
        for tint in theme(&name).mail.avatars {
            let contrast = tint.background.contrast(tint.foreground);
            assert!(
                contrast >= INITIALS_CONTRAST,
                "{name}: {} on {} is {contrast:.2}",
                tint.foreground,
                tint.background
            );
        }
    }
}

#[test]
fn text_on_the_accent_stays_readable() {
    for name in theme_names() {
        let theme = theme(&name);
        let contrast = theme.ui.accent_text.contrast(theme.palette.accent);
        assert!(contrast >= ACCENT_TEXT_CONTRAST, "{name}: {contrast:.2}");
    }
}

#[test]
fn the_same_sender_keeps_its_tint_across_themes() {
    let address = "ingrid@fjordpost.example";
    let index = avatar_index(address);
    for name in theme_names() {
        let theme = theme(&name);
        assert_eq!(theme.avatar(address), theme.mail.avatars[index], "{name}");
    }
}

#[test]
fn every_theme_yields_a_complete_stylesheet() {
    for name in theme_names() {
        let theme = theme(&name);
        let css = theme.css();
        assert!(css.contains("--accent-bg-color: #"), "{name}");
        assert!(css.contains("--sw-avatar-6-fg-color: #"), "{name}");
        assert!(css.contains("--window-radius: 8px;"), "{name}");
        for style in [BodyStyle::Themed, BodyStyle::Paper] {
            assert!(theme.webview_css(style).starts_with(MEASURE_CSS), "{name}");
        }
    }
}

proptest! {
    #[test]
    fn initials_clear_aa_contrast_for_any_accent(r: u8, g: u8, b: u8, dark: bool) {
        let mode = Mode::from_dark(dark);
        for tint in avatar_tints(Rgb { r, g, b }, mode) {
            prop_assert!(tint.background.contrast(tint.foreground) >= INITIALS_CONTRAST);
        }
    }

    #[test]
    fn avatar_index_is_in_range(address in "\\PC{0,40}") {
        prop_assert!(avatar_index(&address) < 7);
        prop_assert_eq!(avatar_index(&address), avatar_index(&address.to_lowercase()));
    }

    #[test]
    fn any_palette_yields_a_complete_theme(
        raw in proptest::collection::btree_map(
            proptest::sample::select(vec![
                "mode", "accent", "background", "foreground", "red", "yellow", "green",
                "blue", "lighter_background", "dark_background", "color0", "color7",
            ]).prop_map(str::to_owned),
            prop_oneof!["#[0-9a-fA-F]{6}", "(light|dark)", "[ -~]{0,8}"],
            0..12,
        )
    ) {
        let inputs = ThemeInputs {
            palette: Palette::from_colors(&raw),
            ..ThemeInputs::builtin(Mode::Dark)
        };
        let theme = Theme::new(&inputs, &ThemeOptions::default());
        let css = theme.css();
        prop_assert!(css.contains("--window-bg-color: #"));
        prop_assert!(css.contains("--sw-secondary-fg-color: #"));
        prop_assert_eq!(css.matches('{').count(), 1);
        prop_assert!(theme.webview_css(BodyStyle::Themed).starts_with(MEASURE_CSS));
    }
}
