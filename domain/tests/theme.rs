//! The theme over Omarchy's 22 stock palettes, resolved by `omarchy-theme-color --all` and
//! stored in `tests/theme/` (regenerate with `bash tests/theme/regenerate.sh`), so nothing
//! here reads the system.

use proptest::prelude::*;
use sealwax_domain::account::AccountColor;
use sealwax_domain::theme::{
    AvatarTint, BORDER_MIX, BodyStyle, FontFamilies, MEASURE_CSS, Mode, Palette, Rgb,
    SECONDARY_MIX, ShellValues, Theme, ThemeInputs, ThemeOptions, ThemeSource, avatar_index,
    avatar_tints, hue_distance,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

/// WCAG AA for normal text: the avatar initials must reach it on every tint, secondary text
/// on the window and the list, and the selected chip's text on its background.
const INITIALS_CONTRAST: f64 = 4.5;
const TEXT_CONTRAST: f64 = 4.5;
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
    let rows = &theme.rows;
    let _ = writeln!(
        text,
        "hovered row: secondary_text {} on {}",
        rows.hover_secondary_text, rows.hover_background
    );
    let _ = writeln!(
        text,
        "selected row: secondary_text {} on {}",
        rows.selected_secondary_text, rows.selected_background
    );
    let _ = writeln!(text, "star {}", m.star);
    let _ = writeln!(text, "unread {}", m.unread);
    let _ = writeln!(
        text,
        "selected_chip {} on {}",
        m.selected_chip, m.selected_chip_background
    );
    let _ = writeln!(text, "accent_text {}", ui.accent_text);
    let _ = writeln!(text, "border {}", ui.border);
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
    for (color, value) in AccountColor::ALL.iter().zip(m.accounts) {
        let _ = writeln!(text, "account {} {value}", color.name());
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

/// Every stock theme, then the built-in palettes.
fn all_themes() -> Vec<(String, Theme)> {
    let mut themes: Vec<(String, Theme)> = theme_names()
        .into_iter()
        .map(|name| {
            let theme = theme(&name);
            (name, theme)
        })
        .collect();
    for mode in [Mode::Dark, Mode::Light] {
        let builtin = Theme::new(&ThemeInputs::builtin(mode), &ThemeOptions::default());
        themes.push((builtin.name().to_owned(), builtin));
    }
    themes
}

fn reads_on_window_and_list(theme: &Theme, color: Rgb) -> bool {
    color.contrast(theme.palette.background) >= TEXT_CONTRAST
        && color.contrast(theme.palette.dark_background) >= TEXT_CONTRAST
}

#[test]
fn secondary_text_reaches_aa_on_the_window_and_the_list() {
    for (name, theme) in all_themes() {
        let secondary = theme.mail.secondary_text;
        for (surface, background) in [
            ("window", theme.palette.background),
            ("list", theme.palette.dark_background),
        ] {
            let contrast = secondary.contrast(background);
            assert!(
                contrast >= TEXT_CONTRAST,
                "{name}: {secondary} on the {surface} {background} is {contrast:.2}"
            );
        }
    }
}

/// The account colours are sidebar text: AA on the window and the sidebar in every theme, each
/// still near its own hue, and no two alike.
#[test]
fn account_colors_read_as_text_and_stay_apart() {
    for (name, theme) in all_themes() {
        let colors = theme.mail.accounts;
        for (color, value) in AccountColor::ALL.iter().zip(colors) {
            assert!(
                reads_on_window_and_list(&theme, value),
                "{name}: {} {value} doesn't read on {} and {}",
                color.name(),
                theme.palette.background,
                theme.palette.dark_background
            );
        }
        for (index, a) in colors.iter().enumerate() {
            for b in &colors[index + 1..] {
                let distance = rgb_distance(*a, *b);
                assert!(
                    distance >= 20.0,
                    "{name}: {a} and {b} are {distance:.1} apart"
                );
            }
        }
    }
}

/// A hovered or selected row's fill must not take secondary text below AA: its own colour reads
/// on the fill and on both backgrounds, mixed as far toward the background as it can be.
#[test]
fn secondary_text_reaches_aa_on_hovered_and_selected_rows() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        let rows = &theme.rows;
        for (state, background, secondary) in [
            ("hovered", rows.hover_background, rows.hover_secondary_text),
            (
                "selected",
                rows.selected_background,
                rows.selected_secondary_text,
            ),
        ] {
            let contrast = secondary.contrast(background);
            assert!(
                contrast >= TEXT_CONTRAST,
                "{name}: {secondary} on the {state} row {background} is {contrast:.2}"
            );
            assert!(
                reads_on_window_and_list(&theme, secondary),
                "{name} {state}"
            );
            let percent = (0..=SECONDARY_MIX)
                .rev()
                .find(|percent| p.foreground.mix(p.background, *percent) == secondary)
                .unwrap_or_else(|| panic!("{name} {state}: {secondary} is not a mix"));
            if percent < SECONDARY_MIX {
                let more = p.foreground.mix(p.background, percent + 1);
                assert!(
                    !reads_on_window_and_list(&theme, more)
                        || more.contrast(background) < TEXT_CONTRAST,
                    "{name} {state}: {percent} % is not the largest mix that reads"
                );
            }
        }
        let list = theme.ui.sidebar;
        assert_eq!(
            rows.selected_background,
            theme
                .controls
                .selected
                .color
                .over(list, theme.controls.selected.fill)
        );
    }
}

/// Themes that read at the design's 35 % keep it; the others take the largest smaller mix
/// that reads.
#[test]
fn secondary_text_mixes_as_far_as_it_reads() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        let secondary = theme.mail.secondary_text;
        let design = p.foreground.mix(p.background, SECONDARY_MIX);
        if reads_on_window_and_list(&theme, design) {
            assert_eq!(
                secondary, design,
                "{name} reads at 35 % and must not change"
            );
            continue;
        }
        let percent = (0..SECONDARY_MIX)
            .rev()
            .find(|percent| p.foreground.mix(p.background, *percent) == secondary)
            .unwrap_or_else(|| panic!("{name}: {secondary} is not a smaller mix"));
        let more = p.foreground.mix(p.background, percent + 1);
        assert!(
            !reads_on_window_and_list(&theme, more),
            "{name}: {percent} % is not the largest mix that reads"
        );
    }
}

#[test]
fn the_selected_chip_reaches_aa() {
    for (name, theme) in all_themes() {
        let m = &theme.mail;
        let (accent, foreground) = (theme.palette.accent, theme.palette.foreground);
        let background = m.selected_chip_background;
        let contrast = m.selected_chip.contrast(background);
        assert!(contrast >= TEXT_CONTRAST, "{name}: {contrast:.2}");
        if accent.contrast(background) >= TEXT_CONTRAST {
            assert_eq!(m.selected_chip, accent, "{name} keeps the accent");
        } else {
            assert!(
                m.selected_chip == foreground
                    && foreground.contrast(background) > accent.contrast(background),
                "{name}: the more readable of accent and foreground"
            );
        }
    }
}

/// The mock's arithmetic: `Math.round(x + (y - x) * t)` per channel.
fn mock_mix(from: Rgb, toward: Rgb, amount: f64) -> Rgb {
    let channel =
        |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * amount).round() as u8;
    Rgb {
        r: channel(from.r, toward.r),
        g: channel(from.g, toward.g),
        b: channel(from.b, toward.b),
    }
}

#[test]
fn borders_follow_the_mock() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        let percent = match p.mode {
            Mode::Dark => BORDER_MIX[0],
            Mode::Light => BORDER_MIX[1],
        };
        let expected = mock_mix(
            p.lighter_background,
            p.foreground,
            f64::from(percent) / 100.0,
        );
        assert_eq!(theme.ui.border, expected, "{name}");
        assert!(
            theme
                .css()
                .contains(&format!("--border-color: {expected};")),
            "{name}"
        );
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
        let themed = theme.webview_css(BodyStyle::Themed);
        let quote = format!(
            "border-left:3px solid {};color:{}}}",
            theme.ui.border, theme.mail.secondary_text
        );
        assert!(
            themed.contains(&quote),
            "{name}: quotes take the muted text"
        );
        let rule = format!("border-top:1px solid {}", theme.ui.border);
        assert!(themed.contains(&rule), "{name}: separators take the border");
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
