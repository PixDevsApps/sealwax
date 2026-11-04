//! The palette and stylesheet from S3's `s3-list`, derived as in the DESIGN mock from the live
//! Omarchy `colors.toml` (read only), with matte-black as the fallback. Omarchy keeps the theme
//! under `$HOME/.local/state` whatever `XDG_STATE_HOME` says, so scratch XDG folders don't hide it.

use gtk4::gdk;
use std::cell::OnceCell;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: String,
    pub side: String,
    pub fg: String,
    pub fg2: String,
    pub fg3: String,
    pub accent: String,
    pub on_accent: String,
    pub border: String,
    pub hover: String,
    pub selection: String,
    pub chip: String,
    pub placeholder: String,
    pub tints: [String; 7],
    pub tint_fg: [String; 7],
}

thread_local! {
    static PALETTE: OnceCell<Palette> = const { OnceCell::new() };
    static COLORS: OnceCell<Colors> = const { OnceCell::new() };
}

/// The palette as RGBA, for widgets that draw themselves.
pub struct Colors {
    pub accent: gdk::RGBA,
    pub placeholder: gdk::RGBA,
    pub tints: [gdk::RGBA; 7],
    pub tint_fg: [gdk::RGBA; 7],
}

pub fn palette() -> Palette {
    PALETTE.with(|cell| cell.get_or_init(load).clone())
}

pub fn with_colors<T>(f: impl FnOnce(&Colors) -> T) -> T {
    COLORS.with(|cell| {
        f(cell.get_or_init(|| {
            let palette = palette();
            let rgba = |hex: &str| gdk::RGBA::parse(hex).unwrap_or(gdk::RGBA::BLACK);
            Colors {
                accent: rgba(&palette.accent),
                placeholder: rgba(&palette.placeholder),
                tints: palette.tints.each_ref().map(|hex| rgba(hex)),
                tint_fg: palette.tint_fg.each_ref().map(|hex| rgba(hex)),
            }
        }))
    })
}

fn colors_toml() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    std::fs::read_to_string(home.join(".local/state/omarchy/current/theme/colors.toml")).ok()
}

fn value(toml: &str, key: &str) -> Option<String> {
    toml.lines().find_map(|line| {
        let (name, rest) = line.split_once('=')?;
        (name.trim() == key).then(|| rest.trim().trim_matches('"').to_owned())
    })
}

fn load() -> Palette {
    let toml = colors_toml().unwrap_or_default();
    let pick = |key: &str, fallback: &str| {
        value(&toml, key)
            .filter(|hex| hex.len() == 7 && hex.starts_with('#'))
            .unwrap_or_else(|| fallback.to_owned())
    };
    let dark = value(&toml, "mode").is_none_or(|mode| mode != "light");
    let accent = pick("accent", "#e68e0d");
    let bg = pick("background", "#121212");
    let side = pick("dark_background", "#0d0d0d");
    let lighter = pick("lighter_background", "#1e1e1e");
    let fg = pick("foreground", "#bebebe");
    let selection = pick("selection", "#2a2a2a");
    let hue = hue_of(&accent);
    let tints: [String; 7] = std::array::from_fn(|i| {
        hsl(
            (hue + i as f64 * 51.0) % 360.0,
            0.42,
            if dark { 0.32 } else { 0.8 },
        )
    });
    let tint_fg = tints
        .each_ref()
        .map(|tint| readable(tint, "#ffffff", "#1b1b1f").to_owned());
    Palette {
        dark,
        fg2: mix(&fg, &bg, 0.28),
        fg3: mix(&fg, &bg, 0.48),
        on_accent: readable(&accent, &bg, "#ffffff").to_owned(),
        border: mix(&lighter, &fg, if dark { 0.08 } else { 0.14 }),
        hover: mix(&side, &fg, if dark { 0.06 } else { 0.07 }),
        chip: mix(&bg, &accent, if dark { 0.18 } else { 0.14 }),
        placeholder: mix(&side, &fg, 0.1),
        bg,
        side,
        fg,
        accent,
        selection,
        tints,
        tint_fg,
    }
}

fn rgb(hex: &str) -> [f64; 3] {
    let n = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    [
        f64::from((n >> 16) & 255),
        f64::from((n >> 8) & 255),
        f64::from(n & 255),
    ]
}

fn hex(c: [f64; 3]) -> String {
    let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", byte(c[0]), byte(c[1]), byte(c[2]))
}

fn mix(a: &str, b: &str, t: f64) -> String {
    let (x, y) = (rgb(a), rgb(b));
    hex(std::array::from_fn(|i| x[i] + (y[i] - x[i]) * t))
}

fn luminance(h: &str) -> f64 {
    let c = rgb(h).map(|v| {
        let v = v / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn readable<'a>(on: &str, a: &'a str, b: &'a str) -> &'a str {
    if contrast(on, a) >= contrast(on, b) {
        a
    } else {
        b
    }
}

fn hue_of(h: &str) -> f64 {
    let [r, g, b] = rgb(h).map(|v| v / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    if d == 0.0 {
        return 0.0;
    }
    let x = if max == r {
        ((g - b) / d) % 6.0
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (x * 60.0 + 360.0) % 360.0
}

fn hsl(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let p = match h {
        h if h < 60.0 => [c, x, 0.0],
        h if h < 120.0 => [x, c, 0.0],
        h if h < 180.0 => [0.0, c, x],
        h if h < 240.0 => [0.0, x, c],
        h if h < 300.0 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    hex(p.map(|v| (v + m) * 255.0))
}

pub fn css(p: &Palette) -> String {
    format!(
        "window {{ background-color: {bg}; color: {fg}; }}
.chrome, .chrome * {{ font-family: monospace; }}
.sidebar {{ background-color: {side}; font-size: 12px; padding: 14px 10px 12px 10px; border-right: 1px solid {border}; }}
.sidebar .compose {{ background-color: {accent}; color: {on_accent}; font-weight: 700; min-height: 34px; margin: 0 2px 14px 2px; border-radius: 8px; }}
.sidebar .folder {{ min-height: 30px; padding: 0 10px; border-radius: 6px; }}
.sidebar .folder.current {{ background-color: {selection}; font-weight: 700; }}
.sidebar .account {{ color: {fg3}; font-size: 10px; letter-spacing: 0.8px; padding: 16px 10px 6px 10px; }}
.sidebar .count {{ color: {fg3}; font-size: 11px; }}
.list-pane {{ background-color: {side}; border-right: 1px solid {border}; font-size: 12px; }}
.list-head {{ padding: 14px 16px 10px 16px; }}
.list-title {{ font-size: 16px; font-weight: 700; }}
.list-head entry {{ min-height: 32px; border-radius: 8px; background-color: {bg}; border: 1px solid {border}; font-size: 12px; }}
.chip {{ min-height: 26px; padding: 0 10px; border-radius: 6px; border: 1px solid {border}; font-size: 12px; background: none; }}
.chip:checked {{ background-color: {chip}; color: {accent}; font-weight: 700; border-color: transparent; }}
listview.message-list {{ background-color: {side}; padding: 0 8px 12px 8px; }}
listview.message-list > row {{ padding: 0; margin: 0 0 2px 0; border-radius: 8px; background: none; }}
listview.message-list > row:hover {{ background-color: {hover}; }}
listview.message-list > row:selected {{ background-color: {selection}; }}
listview.message-list > header {{ padding: 12px 10px 6px 10px; background: none; min-height: 0; }}
.section-header {{ color: {fg3}; font-size: 10px; letter-spacing: 0.8px; }}
.message-row {{ padding: 10px 10px 11px 6px; }}
.message-row label {{ line-height: 1.1; }}
.message-row .sender {{ font-size: 13px; }}
.message-row .time {{ font-size: 11px; color: {fg3}; }}
.message-row .count {{ font-size: 10px; color: {fg3}; border: 1px solid {border}; border-radius: 6px; padding: 0 5px; }}
.message-row .subject {{ font-size: 12px; }}
.message-row .snippet {{ font-size: 11.5px; color: {fg2}; min-height: 33px; }}
.message-row .snippet.oneline {{ min-height: 0; }}
.noliga label {{ font-feature-settings: \"liga\" 0, \"calt\" 0; }}
.instructions {{ font-family: sans-serif; font-size: 17px; line-height: 1.4; }}
.instructions-title {{ font-size: 22px; font-weight: 700; color: {accent}; }}
.message-row .star {{ color: {accent}; }}
.message-row .clip {{ color: {fg3}; }}
.reader {{ background-color: {bg}; }}
.reader-bar {{ min-height: 48px; padding: 0 14px; border-bottom: 1px solid {border}; }}
.reader-subject {{ font-size: 22px; font-weight: 700; }}
.reader-meta {{ color: {fg3}; font-size: 11px; }}
.reader-body {{ font-family: sans-serif; font-size: 15px; line-height: 1.55; }}
.reader-card {{ border: 1px solid {border}; border-radius: 8px; padding: 18px 20px 20px 20px; }}
",
        bg = p.bg,
        fg = p.fg,
        side = p.side,
        border = p.border,
        accent = p.accent,
        on_accent = p.on_accent,
        selection = p.selection,
        fg2 = p.fg2,
        fg3 = p.fg3,
        chip = p.chip,
        hover = p.hover,
    )
}
