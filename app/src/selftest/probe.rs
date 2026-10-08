//! What a widget looks like, for the self-test's look assertions: its text colour and font as
//! GTK computed them, and its own drawing (rendered alone, with GSK's Cairo renderer) for the
//! background, the text contrast and the corner radius. Colours are compared with the theme the
//! follower applied, by role, so a script names roles rather than hex values.

use super::{Harness, StepResult};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{graphene, gsk};
use sealwax_domain::account::AccountColor;
use sealwax_domain::theme::{Rgb, Theme};
use std::collections::HashMap;

/// How far a channel may be from the theme's value (rounding, colour conversions).
const CHANNEL_TOLERANCE: u8 = 2;
/// How far a measured radius may be from the wanted one, in px.
const RADIUS_TOLERANCE: f64 = 1.0;

/// A theme role a script can name.
fn role(theme: &Theme, name: &str) -> Result<Rgb, String> {
    Ok(match name {
        "accent" => theme.palette.accent,
        "on-accent" => theme.ui.accent_text,
        "foreground" => theme.palette.foreground,
        "secondary" => theme.mail.secondary_text,
        "window" => theme.palette.background,
        "sidebar" => theme.ui.sidebar,
        "border" => theme.ui.border,
        "chip" => theme.mail.selected_chip_background,
        "chip-text" => theme.mail.selected_chip,
        "unread" => theme.mail.unread,
        "warning" => theme.ui.warning,
        "error" => theme.ui.error,
        "error-soft" => theme.ui.error_soft,
        "warning-soft" => theme.ui.warning_soft,
        "card" => theme.ui.card,
        other => {
            let color = other
                .strip_prefix("account-")
                .and_then(AccountColor::from_name)
                .ok_or_else(|| format!("no colour role {other}"))?;
            let index = AccountColor::ALL
                .iter()
                .position(|known| *known == color)
                .unwrap_or_default();
            theme.mail.accounts[index]
        }
    })
}

fn close(a: Rgb, b: Rgb) -> bool {
    a.r.abs_diff(b.r) <= CHANNEL_TOLERANCE
        && a.g.abs_diff(b.g) <= CHANNEL_TOLERANCE
        && a.b.abs_diff(b.b) <= CHANNEL_TOLERANCE
}

/// The CSS `color` GTK computed for the widget.
fn text_color(widget: &gtk::Widget) -> Rgb {
    let color = widget.color();
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb {
        r: channel(color.red()),
        g: channel(color.green()),
        b: channel(color.blue()),
    }
}

/// The first family of the widget's font, as Pango gets it from CSS.
fn family(widget: &gtk::Widget) -> String {
    widget
        .pango_context()
        .font_description()
        .and_then(|description| description.family())
        .and_then(|families| families.split(',').next().map(str::to_owned))
        .unwrap_or_default()
}

/// The widget drawn on its own, as straight (not premultiplied) RGBA.
struct Drawing {
    width: usize,
    height: usize,
    pixels: Vec<(Rgb, u8)>,
}

impl Drawing {
    fn of(widget: &gtk::Widget) -> Result<Self, String> {
        let paintable = gtk::WidgetPaintable::new(Some(widget));
        let (width, height) = (paintable.intrinsic_width(), paintable.intrinsic_height());
        if width <= 0 || height <= 0 {
            return Err("it has no size".to_owned());
        }
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
        let node = snapshot.to_node().ok_or("it drew nothing")?;
        let renderer = gsk::CairoRenderer::new();
        renderer
            .realize_for_display(&WidgetExt::display(widget))
            .map_err(|error| error.to_string())?;
        let viewport = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
        let texture = renderer.render_texture(&node, Some(&viewport));
        renderer.unrealize();
        let (width, height) = (texture.width() as usize, texture.height() as usize);
        let stride = width * 4;
        let mut data = vec![0; stride * height];
        texture.download(&mut data, stride);
        let pixels = data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bgra| {
                let alpha = bgra[3];
                let straight = |value: u8| match alpha {
                    0 => 0,
                    255 => value,
                    _ => ((u32::from(value) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                        .min(255) as u8,
                };
                (
                    Rgb {
                        r: straight(bgra[2]),
                        g: straight(bgra[1]),
                        b: straight(bgra[0]),
                    },
                    alpha,
                )
            })
            .collect();
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    fn alpha(&self, x: usize, y: usize) -> u8 {
        self.pixels[y * self.width + x].1
    }

    fn opaque(&self) -> impl Iterator<Item = Rgb> + '_ {
        self.pixels
            .iter()
            .filter(|(_, alpha)| *alpha == 255)
            .map(|(color, _)| *color)
    }

    /// The colour most opaque pixels have: the widget's own background.
    fn background(&self) -> Option<Rgb> {
        let mut counts: HashMap<Rgb, usize> = HashMap::new();
        for color in self.opaque() {
            *counts.entry(color).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(color, _)| color)
    }

    /// The radius of the background's corners, from how much of each corner is left out: a
    /// quarter circle of radius r leaves r² (1 − π/4) px of the corner's square uncovered
    /// (antialiasing included). The background's own alpha is read on the left edge, between
    /// the corners, so translucent fills (a selected row) measure as well as opaque ones.
    fn corner_radius(&self) -> Result<f64, String> {
        let block = 12.min(self.width / 2).min(self.height / 2);
        if block < 2 {
            return Err("it is too small to measure".to_owned());
        }
        let mut edge: Vec<u8> = (block..self.height - block)
            .map(|y| self.alpha(0, y))
            .collect();
        edge.sort_unstable();
        let reference = f64::from(edge.get(edge.len() / 2).copied().unwrap_or(0));
        if reference == 0.0 {
            return Err("it has no background at its left edge".to_owned());
        }
        let mut uncovered = 0.0;
        for y in 0..block {
            for x in 0..block {
                for (px, py) in [
                    (x, y),
                    (self.width - 1 - x, y),
                    (x, self.height - 1 - y),
                    (self.width - 1 - x, self.height - 1 - y),
                ] {
                    uncovered += (1.0 - f64::from(self.alpha(px, py)) / reference).max(0.0);
                }
            }
        }
        let per_corner = uncovered / 4.0;
        Ok((per_corner / (1.0 - std::f64::consts::FRAC_PI_4)).sqrt())
    }
}

impl Harness {
    fn theme(&self) -> Result<std::rc::Rc<Theme>, String> {
        Ok(self
            .state
            .theme()
            .ok_or("the theme follower isn't running")?
            .theme())
    }

    /// `assert-color NAME text|background ROLE`, `assert-contrast NAME MIN`,
    /// `assert-radius NAME PX` and `assert-font NAME chrome|reading`.
    pub(super) fn check_look(&self, assertion: &str, args: &[String]) -> StepResult {
        let name = args[0].as_str();
        let widget = self.widget(name)?;
        let theme = self.theme()?;
        let drawing = || Drawing::of(&widget).map_err(|error| format!("{name}: {error}"));
        match assertion {
            "assert-color" => {
                let wanted = role(&theme, &args[2])?;
                let shown = match args[1].as_str() {
                    "text" => text_color(&widget),
                    "background" => drawing()?
                        .background()
                        .ok_or_else(|| format!("{name} draws no opaque background"))?,
                    other => return Err(format!("use text or background, not {other}")),
                };
                println!("  # {name}: {} {shown}", args[1]);
                if close(shown, wanted) {
                    Ok(())
                } else {
                    Err(format!(
                        "{name}'s {} is {shown}, not {} {wanted}",
                        args[1], args[2]
                    ))
                }
            }
            "assert-contrast" => {
                let minimum: f64 = args[1]
                    .parse()
                    .map_err(|_| format!("expected a contrast ratio, got {}", args[1]))?;
                let drawing = drawing()?;
                let background = drawing
                    .background()
                    .ok_or_else(|| format!("{name} draws no opaque background"))?;
                let (text, ratio) = drawing
                    .opaque()
                    .map(|color| (color, color.contrast(background)))
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .unwrap_or((background, 1.0));
                println!("  # {name}: {text} on {background}, {ratio:.2}:1");
                if ratio >= minimum {
                    Ok(())
                } else {
                    Err(format!(
                        "{name}: {text} on {background} is {ratio:.2}:1, under {minimum}:1"
                    ))
                }
            }
            "assert-radius" => {
                let wanted: f64 = args[1]
                    .parse()
                    .map_err(|_| format!("expected px, got {}", args[1]))?;
                let radius = drawing()?
                    .corner_radius()
                    .map_err(|error| format!("{name}: {error}"))?;
                println!("  # {name}: corners of about {radius:.1} px");
                if (radius - wanted).abs() <= RADIUS_TOLERANCE {
                    Ok(())
                } else {
                    Err(format!(
                        "{name}'s corners are about {radius:.1} px, not {wanted}"
                    ))
                }
            }
            "assert-font" => {
                let wanted = match args[1].as_str() {
                    "chrome" => &theme.fonts.chrome,
                    "reading" => &theme.fonts.reading,
                    other => return Err(format!("use chrome or reading, not {other}")),
                };
                let shown = family(&widget);
                println!("  # {name}: {shown}");
                if &shown == wanted {
                    Ok(())
                } else {
                    Err(format!("{name} is in {shown:?}, not {wanted:?}"))
                }
            }
            other => Err(format!("unknown command {other}")),
        }
    }
}
