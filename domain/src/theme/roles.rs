use super::color::{Rgb, distinct_pairs, readable_on};
use super::palette::{Mode, Palette};

/// The hues the status colours turn to when a theme can't tell its `red`, `yellow` and
/// `green` apart ([`distinct_pairs`]): error, warning, success. Matte-black's are two reds
/// and an amber.
pub const STATUS_HUES: [f64; 3] = [0.0, 40.0, 130.0];
/// How far apart, in degrees of hue, the theme's own status colours must be to be kept.
pub const STATUS_MIN_GAP: f64 = 20.0;

/// libadwaita's roles, mapped as in DESIGN.md's token table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiRoles {
    /// Sidebar, list and header bar: `dark_background`.
    pub sidebar: Rgb,
    /// Popovers, the palette and dialogs: `darker_background`.
    pub popover: Rgb,
    /// Cards, hovered rows and attachment cards: `lighter_background`.
    pub card: Rgb,
    /// Borders and separators: `lighter_background`.
    pub border: Rgb,
    /// Text on the accent: `readable_on(accent)`.
    pub accent_text: Rgb,
    pub error: Rgb,
    pub warning: Rgb,
    pub success: Rgb,
    pub error_text: Rgb,
    pub warning_text: Rgb,
    pub success_text: Rgb,
    /// Banner backgrounds: the window background tinted toward the status colour.
    pub error_soft: Rgb,
    pub warning_soft: Rgb,
}

impl UiRoles {
    pub fn new(palette: &Palette) -> Self {
        let p = palette;
        let on = |color: Rgb| readable_on(color, p.background, p.foreground);
        let [error, warning, success] =
            distinct_pairs([p.red, p.yellow, p.green], STATUS_HUES, STATUS_MIN_GAP);
        let soft = match p.mode {
            Mode::Dark => 16,
            Mode::Light => 10,
        };
        Self {
            sidebar: p.dark_background,
            popover: p.darker_background,
            card: p.lighter_background,
            border: p.lighter_background,
            accent_text: on(p.accent),
            error,
            warning,
            success,
            error_text: on(error),
            warning_text: on(warning),
            success_text: on(success),
            error_soft: p.background.mix(error, soft),
            warning_soft: p.background.mix(warning, soft),
        }
    }
}

/// How many avatar tints there are.
pub const AVATAR_TINTS: usize = 7;
/// The hue step between neighbouring tints, from the accent's hue (DESIGN.md).
pub const AVATAR_HUE_STEP: f64 = 51.0;
/// The tints' fixed saturation and their lightness per mode, as in the approved mock.
pub const AVATAR_SATURATION: f64 = 0.42;
pub const AVATAR_LIGHTNESS_DARK: f64 = 0.32;
pub const AVATAR_LIGHTNESS_LIGHT: f64 = 0.80;
/// The two colours the initials choose between with `readable_on`, as in the approved mock.
pub const INITIALS_LIGHT: Rgb = Rgb::WHITE;
pub const INITIALS_DARK: Rgb = Rgb::from_hex(0x1b1b1f);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvatarTint {
    pub background: Rgb,
    /// The initials: whichever of [`INITIALS_LIGHT`] and [`INITIALS_DARK`] reads better.
    pub foreground: Rgb,
}

/// The seven tints: hues rotated from `accent`'s in 51° steps at a fixed saturation and a
/// mode-dependent lightness. Palette hues are often too alike (matte-black's are reds and
/// ambers), so the tints never come from the palette's colours.
pub fn avatar_tints(accent: Rgb, mode: Mode) -> [AvatarTint; AVATAR_TINTS] {
    let (hue, _, _) = accent.to_hsl();
    let lightness = match mode {
        Mode::Dark => AVATAR_LIGHTNESS_DARK,
        Mode::Light => AVATAR_LIGHTNESS_LIGHT,
    };
    std::array::from_fn(|index| {
        let background = Rgb::from_hsl(
            hue + AVATAR_HUE_STEP * index as f64,
            AVATAR_SATURATION,
            lightness,
        );
        AvatarTint {
            background,
            foreground: readable_on(background, INITIALS_LIGHT, INITIALS_DARK),
        }
    })
}

/// Which tint an address gets: FNV-1a over the trimmed, lower-cased address, so the same
/// sender keeps its colour across runs, machines and themes.
pub fn avatar_index(address: &str) -> usize {
    let hash = address
        .trim()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    (hash % AVATAR_TINTS as u64) as usize
}

/// Up to two initials: the first letters of the display name's first and last words, or else
/// of the address's local part split at `.`, `_`, `-` and `+`; `?` when there is neither.
/// Words that don't start with a letter or digit, such as `(Work)`, are skipped.
pub fn initials(display_name: &str, address: &str) -> String {
    let name = display_name.trim().trim_matches(['"', '\'']);
    let from_name = first_letters(name.split_whitespace());
    if !from_name.is_empty() {
        return from_name;
    }
    let local = address.trim().split('@').next().unwrap_or_default();
    let from_address = first_letters(local.split(['.', '_', '-', '+']));
    if from_address.is_empty() {
        "?".to_owned()
    } else {
        from_address
    }
}

fn first_letters<'a>(words: impl Iterator<Item = &'a str>) -> String {
    let letters: Vec<char> = words
        .filter_map(|word| word.chars().next())
        .filter(|letter| letter.is_alphanumeric())
        .collect();
    let (first, last) = match letters.as_slice() {
        [] => return String::new(),
        [only] => (*only, None),
        [first, .., last] => (*first, Some(*last)),
    };
    std::iter::once(first)
        .chain(last)
        .flat_map(char::to_uppercase)
        .collect()
}

/// The roles Sealwax adds to the palette (DESIGN.md "Theme tokens").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailRoles {
    /// Snippets, times and addresses: `foreground` mixed 35 % toward `background`.
    pub secondary_text: Rgb,
    pub star: Rgb,
    pub unread: Rgb,
    /// The selected filter chip: accent text on the background tinted toward the accent.
    pub selected_chip: Rgb,
    pub selected_chip_background: Rgb,
    pub avatars: [AvatarTint; AVATAR_TINTS],
}

impl MailRoles {
    pub fn new(palette: &Palette) -> Self {
        let p = palette;
        let chip_tint = match p.mode {
            Mode::Dark => 18,
            Mode::Light => 14,
        };
        Self {
            secondary_text: p.foreground.mix(p.background, 35),
            star: p.accent,
            unread: p.accent,
            selected_chip: p.accent,
            selected_chip_background: p.background.mix(p.accent, chip_tint),
            avatars: avatar_tints(p.accent, p.mode),
        }
    }

    /// The tint for a sender's address.
    pub fn avatar(&self, address: &str) -> AvatarTint {
        self.avatars[avatar_index(address)]
    }
}

#[cfg(test)]
mod tests {
    use super::super::color::hue_distance;
    use super::*;

    #[test]
    fn avatar_index_is_stable_and_ignores_case_and_spaces() {
        let index = avatar_index("ingrid@fjordpost.example");
        assert_eq!(index, avatar_index(" Ingrid@FjordPost.example "));
        assert!(index < AVATAR_TINTS);
        assert_eq!(
            avatar_index(""),
            0xcbf2_9ce4_8422_2325_u64 as usize % AVATAR_TINTS
        );
        let spread: std::collections::BTreeSet<usize> = (0..200)
            .map(|n| avatar_index(&format!("user{n}@example.test")))
            .collect();
        assert_eq!(spread.len(), AVATAR_TINTS);
    }

    #[test]
    fn avatar_index_values_never_change() {
        // FNV-1a mod 7 for fixed synthetic addresses; a different value means every sender
        // would change colour after an update.
        let indexes: Vec<usize> = [
            "ingrid@fjordpost.example",
            "news@northwind.example",
            "marius@stavtech.example",
            "a@b.test",
        ]
        .iter()
        .map(|address| avatar_index(address))
        .collect();
        assert_eq!(indexes, [2, 4, 5, 0]);
    }

    #[test]
    fn tints_rotate_from_the_accent_hue() {
        let accent = Rgb::from_hex(0xe68e0d);
        let (accent_hue, _, _) = accent.to_hsl();
        let tints = avatar_tints(accent, Mode::Dark);
        for (index, tint) in tints.iter().enumerate() {
            let (hue, saturation, lightness) = tint.background.to_hsl();
            let expected = accent_hue + AVATAR_HUE_STEP * index as f64;
            assert!(hue_distance(hue, expected) < 2.0, "{index}: {hue}");
            assert!((saturation - AVATAR_SATURATION).abs() < 0.03, "{index}");
            assert!((lightness - AVATAR_LIGHTNESS_DARK).abs() < 0.01, "{index}");
        }
        let light = avatar_tints(accent, Mode::Light);
        assert!(light.iter().all(|tint| tint.foreground == INITIALS_DARK));
        assert!(tints.iter().all(|tint| tint.foreground == INITIALS_LIGHT));
    }

    #[test]
    fn initials_come_from_the_name_then_the_address() {
        assert_eq!(initials("Ingrid Solberg", "ingrid@fjordpost.example"), "IS");
        assert_eq!(
            initials("\"Northwind Weekly\"", "news@northwind.example"),
            "NW"
        );
        assert_eq!(initials("Marius Dahl (Work)", ""), "MD");
        assert_eq!(initials("Cher", ""), "C");
        assert_eq!(initials("  ", "tomas.berg@example.net"), "TB");
        assert_eq!(initials("", "news@northwind.example"), "N");
        assert_eq!(initials("øyvind ås", ""), "ØÅ");
        assert_eq!(initials("", ""), "?");
        assert_eq!(initials("(none)", "@example.test"), "?");
    }

    #[test]
    fn mail_roles_follow_the_design_table() {
        let palette = Palette::DARK;
        let roles = MailRoles::new(&palette);
        assert_eq!(
            roles.secondary_text,
            palette.foreground.mix(palette.background, 35)
        );
        assert_eq!(roles.star, palette.accent);
        assert_eq!(roles.unread, palette.accent);
        assert_eq!(roles.selected_chip, palette.accent);
        assert_eq!(
            roles.avatar("ingrid@fjordpost.example"),
            roles.avatars[avatar_index("ingrid@fjordpost.example")]
        );
    }

    #[test]
    fn status_colours_are_turned_apart_on_matte_black() {
        let palette = Palette {
            red: Rgb::from_hex(0xd35f5f),
            yellow: Rgb::from_hex(0xb91c1c),
            green: Rgb::from_hex(0xffc107),
            ..Palette::DARK
        };
        let roles = UiRoles::new(&palette);
        let hues = [roles.error, roles.warning, roles.success].map(|color| color.to_hsl().0);
        for (index, hue) in hues.iter().enumerate() {
            assert!(
                hue_distance(*hue, STATUS_HUES[index]) < 2.0,
                "{index}: {hue}"
            );
        }
    }
}
