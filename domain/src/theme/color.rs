use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self::from_hex(0x000000);
    pub const WHITE: Self = Self::from_hex(0xffffff);

    pub const fn from_hex(hex: u32) -> Self {
        Self {
            r: (hex >> 16) as u8,
            g: (hex >> 8) as u8,
            b: hex as u8,
        }
    }

    /// Accepts `#rgb`, `#rrggbb`, `#rrggbbaa`, and Hyprland's `rgb(rrggbb)`, `rgba(rrggbbaa)`
    /// and `rgb(r, g, b)`. Alpha is dropped.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if let Some(hex) = value.strip_prefix('#') {
            return Self::parse_hex(hex);
        }
        let lower = value.to_ascii_lowercase();
        let inner = lower
            .strip_prefix("rgba(")
            .or_else(|| lower.strip_prefix("rgb("))?
            .strip_suffix(')')?;
        if !inner.contains(',') {
            return Self::parse_hex(inner.trim());
        }
        let channels: Vec<&str> = inner.split(',').map(str::trim).collect();
        match channels.as_slice() {
            [r, g, b] | [r, g, b, _] => Some(Self {
                r: r.parse().ok()?,
                g: g.parse().ok()?,
                b: b.parse().ok()?,
            }),
            _ => None,
        }
    }

    fn parse_hex(hex: &str) -> Option<Self> {
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        match hex.len() {
            3 => {
                let value = u32::from_str_radix(hex, 16).ok()?;
                let nibble = |shift: u32| ((value >> shift) & 0xf) as u8 * 17;
                Some(Self {
                    r: nibble(8),
                    g: nibble(4),
                    b: nibble(0),
                })
            }
            6 | 8 => Some(Self::from_hex(u32::from_str_radix(&hex[..6], 16).ok()?)),
            _ => None,
        }
    }

    /// Blends toward `other` by `percent`, rounding exactly like `omarchy-theme-color`'s
    /// `mix_color`.
    pub fn mix(self, other: Self, percent: u8) -> Self {
        let amount = f64::from(percent.min(100)) / 100.0;
        let channel = |start: u8, end: u8| {
            (f64::from(start) * (1.0 - amount) + f64::from(end) * amount + 0.5) as u8
        };
        Self {
            r: channel(self.r, other.r),
            g: channel(self.g, other.g),
            b: channel(self.b, other.b),
        }
    }

    /// WCAG 2 relative luminance.
    pub fn relative_luminance(self) -> f64 {
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// WCAG 2 contrast ratio, from 1 to 21.
    pub fn contrast(self, other: Self) -> f64 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Hue in degrees, then saturation and lightness from 0 to 1.
    pub fn to_hsl(self) -> (f64, f64, f64) {
        let [r, g, b] = [self.r, self.g, self.b].map(|channel| f64::from(channel) / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let lightness = (max + min) / 2.0;
        let delta = max - min;
        if delta == 0.0 {
            return (0.0, 0.0, lightness);
        }
        let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
        let hue = if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        (hue, saturation, lightness)
    }

    pub fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> Self {
        let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
        let sector = hue.rem_euclid(360.0) / 60.0;
        let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match sector as u8 {
            0 => (chroma, x, 0.0),
            1 => (x, chroma, 0.0),
            2 => (0.0, chroma, x),
            3 => (0.0, x, chroma),
            4 => (x, 0.0, chroma),
            _ => (chroma, 0.0, x),
        };
        let m = lightness - chroma / 2.0;
        let channel = |value: f64| ((value + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Self {
            r: channel(r),
            g: channel(g),
            b: channel(b),
        }
    }

    /// `rgba(r, g, b, alpha)` for CSS, alpha clamped to 0–1.
    pub fn with_alpha(self, alpha: f64) -> String {
        let alpha = (alpha.clamp(0.0, 1.0) * 1000.0).round() / 1000.0;
        format!("rgba({}, {}, {}, {alpha})", self.r, self.g, self.b)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// Whichever of `a` and `b` contrasts more with `background`.
pub fn readable_on(background: Rgb, a: Rgb, b: Rgb) -> Rgb {
    if background.contrast(a) >= background.contrast(b) {
        a
    } else {
        b
    }
}

/// `colors` as the theme has them when they can be told apart; otherwise each turned to its
/// entry in `hues` (Stet's rule). Some themes give every colour of their palette one hue or
/// none at all. Apart means the hues spread over at least a quarter of the colour wheel and
/// none is nearly grey.
pub fn distinct<const N: usize>(colors: [Rgb; N], hues: [f64; N]) -> [Rgb; N] {
    let hsl = colors.map(Rgb::to_hsl);
    let mut angles: Vec<f64> = hsl.iter().map(|&(hue, _, _)| hue).collect();
    angles.sort_by(f64::total_cmp);
    let largest_gap = angles
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .chain(
            angles
                .first()
                .zip(angles.last())
                .map(|(first, last)| first + 360.0 - last),
        )
        .fold(0.0, f64::max);
    if !any_grey(&hsl) && 360.0 - largest_gap >= 90.0 {
        return colors;
    }
    turned(&hsl, hues)
}

/// Like [`distinct`] for colours that sit close by nature, such as error, warning and
/// success: apart means every two hues are at least `min_gap` degrees apart and none is
/// nearly grey. Matte-black's red and yellow share a hue; flexoki-light's red, yellow and
/// olive green are 27° apart and stay.
pub fn distinct_pairs<const N: usize>(colors: [Rgb; N], hues: [f64; N], min_gap: f64) -> [Rgb; N] {
    let hsl = colors.map(Rgb::to_hsl);
    let apart = hsl.iter().enumerate().all(|(index, &(hue, _, _))| {
        hsl[index + 1..]
            .iter()
            .all(|&(other, _, _)| hue_distance(hue, other) >= min_gap)
    });
    if apart && !any_grey(&hsl) {
        return colors;
    }
    turned(&hsl, hues)
}

fn any_grey(hsl: &[(f64, f64, f64)]) -> bool {
    hsl.iter().any(|&(_, saturation, _)| saturation < 0.15)
}

/// Each colour at its role's hue, keeping roughly its lightness, at 45–75 % saturation (an
/// upper bound Stet doesn't have, so a vivid amber turned green doesn't go neon).
fn turned<const N: usize>(hsl: &[(f64, f64, f64); N], hues: [f64; N]) -> [Rgb; N] {
    std::array::from_fn(|index| {
        let (_, saturation, lightness) = hsl[index];
        Rgb::from_hsl(
            hues[index],
            saturation.clamp(0.45, 0.75),
            lightness.clamp(0.4, 0.65),
        )
    })
}

/// The shorter way round the colour wheel between two hues, in degrees.
pub fn hue_distance(a: f64, b: f64) -> f64 {
    let gap = (a - b).rem_euclid(360.0);
    gap.min(360.0 - gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_forms() {
        let expected = Rgb::from_hex(0x1e66f5);
        for value in [
            "#1e66f5",
            "#1E66F5",
            " #1e66f5 ",
            "#1e66f5ee",
            "rgb(1e66f5)",
            "rgba(1e66f5ee)",
            "rgb(30, 102, 245)",
            "rgba(30,102,245,0.5)",
        ] {
            assert_eq!(Rgb::parse(value), Some(expected), "{value}");
        }
        assert_eq!(Rgb::parse("#fa0"), Some(Rgb::from_hex(0xffaa00)));
    }

    #[test]
    fn rejects_garbage() {
        for value in [
            "",
            "#",
            "#12345",
            "#1234567",
            "#ggggggg",
            "blue",
            "rgb(1,2)",
            "rgb(256,0,0)",
            "rgb(",
        ] {
            assert_eq!(Rgb::parse(value), None, "{value}");
        }
    }

    #[test]
    fn displays_lowercase_hex_and_rgba() {
        assert_eq!(Rgb::from_hex(0x1E66F5).to_string(), "#1e66f5");
        assert_eq!(Rgb::BLACK.to_string(), "#000000");
        assert_eq!(
            Rgb::from_hex(0xbebebe).with_alpha(0.04),
            "rgba(190, 190, 190, 0.04)"
        );
        assert_eq!(Rgb::WHITE.with_alpha(1.5), "rgba(255, 255, 255, 1)");
        assert_eq!(
            Rgb::WHITE.with_alpha(1.0 / 3.0),
            "rgba(255, 255, 255, 0.333)"
        );
    }

    #[test]
    fn mix_rounds_like_omarchy_theme_color() {
        let dark = Rgb::from_hex(0x101010);
        assert_eq!(dark.mix(Rgb::BLACK, 25), Rgb::from_hex(0x0c0c0c));
        assert_eq!(dark.mix(Rgb::BLACK, 50), Rgb::from_hex(0x080808));
        assert_eq!(
            Rgb::from_hex(0xff0000).mix(Rgb::WHITE, 20),
            Rgb::from_hex(0xff3333)
        );
        assert_eq!(dark.mix(Rgb::WHITE, 0), dark);
        assert_eq!(dark.mix(Rgb::WHITE, 100), Rgb::WHITE);
    }

    #[test]
    fn contrast_spans_one_to_twenty_one() {
        assert!((Rgb::BLACK.contrast(Rgb::WHITE) - 21.0).abs() < 1e-9);
        assert!((Rgb::WHITE.contrast(Rgb::WHITE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn readable_on_picks_the_stronger_contrast() {
        let pale = Rgb::from_hex(0xffff80);
        assert_eq!(readable_on(pale, Rgb::WHITE, Rgb::BLACK), Rgb::BLACK);
        assert_eq!(
            readable_on(Rgb::from_hex(0x1e66f5), Rgb::BLACK, Rgb::WHITE),
            Rgb::WHITE
        );
    }

    #[test]
    fn hsl_round_trips() {
        for hex in [
            0x926d67, 0x7aa2f7, 0x9ece6a, 0xffffff, 0x000000, 0x808080, 0xde6f69,
        ] {
            let color = Rgb::from_hex(hex);
            let (h, s, l) = color.to_hsl();
            let back = Rgb::from_hsl(h, s, l);
            for (a, b) in [(color.r, back.r), (color.g, back.g), (color.b, back.b)] {
                assert!(a.abs_diff(b) <= 1, "{color} -> {back}");
            }
        }
    }

    #[test]
    fn single_hue_colours_are_turned_apart() {
        let matte = [0xd35f5f, 0xb91c1c, 0xffc107].map(Rgb::from_hex);
        let hues = [0.0, 40.0, 130.0];
        let turned = distinct(matte, hues);
        for (index, color) in turned.iter().enumerate() {
            assert!(hue_distance(color.to_hsl().0, hues[index]) < 2.0, "{index}");
        }
        let tokyo = [0xf7768e, 0xe0af68, 0x9ece6a].map(Rgb::from_hex);
        assert_eq!(distinct(tokyo, hues), tokyo);
        let greys = [0x2a2a2a, 0x4a4a4a, 0x3a3a3a].map(Rgb::from_hex);
        assert_ne!(distinct(greys, hues), greys);
    }

    #[test]
    fn close_pairs_are_turned_apart() {
        let hues = [0.0, 40.0, 130.0];
        let flexoki = [0xd14d41, 0xd0a215, 0x879a39].map(Rgb::from_hex);
        assert_eq!(distinct_pairs(flexoki, hues, 20.0), flexoki);
        assert_ne!(distinct(flexoki, hues), flexoki);
        let retro = [0xf85525, 0xe97b3c, 0x028391].map(Rgb::from_hex);
        let turned = distinct_pairs(retro, hues, 20.0);
        for (index, color) in turned.iter().enumerate() {
            assert!(hue_distance(color.to_hsl().0, hues[index]) < 2.0, "{index}");
        }
        let greys = [0x2a2a2a, 0x4a4a4a, 0x3a3a3a].map(Rgb::from_hex);
        assert_ne!(distinct_pairs(greys, hues, 20.0), greys);
        let neon = distinct_pairs(
            [0xd35f5f, 0xb91c1c, 0xffc107].map(Rgb::from_hex),
            hues,
            20.0,
        );
        assert!(neon[2].to_hsl().1 <= 0.76, "{}", neon[2]);
    }

    #[test]
    fn hue_distance_wraps() {
        assert!((hue_distance(350.0, 10.0) - 20.0).abs() < 1e-9);
        assert!((hue_distance(10.0, 350.0) - 20.0).abs() < 1e-9);
        assert!((hue_distance(0.0, 180.0) - 180.0).abs() < 1e-9);
    }
}
