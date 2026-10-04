//! Prints what the theme follower reads on this machine and what it makes of it, without
//! opening a window: `cargo run -p sealwax --example theme_dump [-- --css] [-- --webview]`.
//! Only reads (Omarchy's state, fontconfig, `shell.toml`, `hyprctl getoption`). Without an
//! Omarchy theme it shows the built-in dark palette (the app follows the system preference).

use sealwax_domain::theme::{
    BodyStyle, Mode, Palette, Theme, ThemeInputs, ThemeOptions, ThemeSource,
};
use sealwax_infrastructure::omarchy::{self, OmarchyPaths, Request, colors, font, hyprland, shell};
use std::time::{Duration, Instant};

fn timed<T>(read: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = read();
    (value, started.elapsed())
}

fn ms(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = OmarchyPaths::from_env();

    let (readout, all) = timed(|| omarchy::read(&paths, Request::ALL));
    let (_, palette) = timed(|| colors::read_theme(&paths));
    let (_, mono) = timed(|| font::family_with("fc-match".as_ref(), font::MONOSPACE));
    let (_, sans) = timed(|| font::family_with("fc-match".as_ref(), font::SANS_SERIF));
    let (_, rounding) = timed(hyprland::rounding);
    let (_, shell_toml) = timed(|| shell::read_shell(&paths));
    println!(
        "read everything in parallel: {}; alone: palette {}, fc-match monospace {}, \
         fc-match sans-serif {}, hyprctl {}, shell.toml {}",
        ms(all),
        ms(palette),
        ms(mono),
        ms(sans),
        ms(rounding),
        ms(shell_toml)
    );

    let omarchy = readout.palette.flatten();
    let (palette, source) = match &omarchy {
        Some(theme) => (
            Palette::from_resolved(&theme.resolved),
            ThemeSource::Omarchy(theme.name.clone()),
        ),
        None => (Palette::builtin(Mode::Dark), ThemeSource::Builtin),
    };
    let inputs = ThemeInputs {
        source,
        palette,
        shell: readout.shell.unwrap_or_default(),
        fonts: readout.fonts.unwrap_or_default(),
        hyprland_rounding: readout.rounding.flatten(),
    };
    let (theme, built) = timed(|| Theme::new(&inputs, &ThemeOptions::default()));
    let (css, generated) = timed(|| theme.css());
    println!(
        "theme {} ({}), corners {} px ({} small), chrome font {:?}, reading font {:?}, \
         text {} px, built in {}, stylesheet {} bytes in {}",
        theme.name(),
        theme.mode().as_str(),
        theme.radius,
        theme.radius_small,
        theme.fonts.chrome,
        theme.fonts.reading,
        theme.scale.body,
        ms(built),
        css.len(),
        ms(generated)
    );
    let mail = &theme.mail;
    println!(
        "secondary text {} ({:.2}:1 on the list), unread and star {}, selected chip {} on {}",
        mail.secondary_text,
        mail.secondary_text.contrast(theme.palette.dark_background),
        mail.unread,
        mail.selected_chip,
        mail.selected_chip_background
    );
    for (index, tint) in mail.avatars.iter().enumerate() {
        println!(
            "avatar tint {index}: {} on {} ({:.2}:1)",
            tint.foreground,
            tint.background,
            tint.background.contrast(tint.foreground)
        );
    }
    if args.iter().any(|arg| arg == "--css") {
        print!("{css}");
    }
    if args.iter().any(|arg| arg == "--webview") {
        for style in [BodyStyle::Themed, BodyStyle::Paper] {
            println!("/* {style:?} */");
            print!("{}", theme.webview_css(style));
        }
        println!("{:?}", theme.webview_fonts());
    }
}
