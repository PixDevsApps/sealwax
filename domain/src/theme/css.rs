//! The generated application stylesheet: libadwaita's colour variables mapped as in
//! DESIGN.md's token table, and Sealwax's own `--sw-*` variables that `app/src/style.css`
//! uses. Widgets never name a palette key; they use these variables through CSS classes.

use super::Theme;
use super::color::Rgb;
use super::roles::avatar_index;
use super::typography::css_family;
use super::webview::{PAPER, PAPER_TEXT};
use std::fmt::Write as _;

/// Component sizes from the approved mock at the default text size, in px; they scale with
/// the spacing scale and become `--sw-size-<name>`.
pub const COMPONENT_SIZES: [(&str, u32); 14] = [
    ("avatar", 32),
    ("avatar-small", 24),
    ("avatar-large", 38),
    ("unread-dot", 7),
    ("status-dot", 6),
    ("chip", 26),
    ("nav-row", 30),
    ("search", 32),
    ("icon-button", 32),
    ("compose", 34),
    ("toolbar", 48),
    ("attachment-type-width", 34),
    ("attachment-type-height", 40),
    ("empty-icon", 40),
];

/// The CSS class that gives an avatar its tint: `sw-avatar-tint-<0..6>`.
pub fn avatar_class(address: &str) -> String {
    format!("sw-avatar-tint-{}", avatar_index(address))
}

impl Theme {
    /// libadwaita's colour variables, in output order.
    pub fn adwaita_colors(&self) -> Vec<(&'static str, Rgb)> {
        let p = &self.palette;
        let ui = &self.ui;
        vec![
            ("--window-bg-color", p.background),
            ("--window-fg-color", p.foreground),
            ("--view-bg-color", p.background),
            ("--view-fg-color", p.foreground),
            ("--headerbar-bg-color", ui.sidebar),
            ("--headerbar-fg-color", p.foreground),
            ("--headerbar-backdrop-color", ui.sidebar),
            ("--headerbar-border-color", ui.border),
            ("--headerbar-shade-color", ui.border),
            ("--sidebar-bg-color", ui.sidebar),
            ("--sidebar-fg-color", p.foreground),
            ("--sidebar-backdrop-color", ui.sidebar),
            ("--sidebar-border-color", ui.border),
            ("--secondary-sidebar-bg-color", ui.sidebar),
            ("--secondary-sidebar-fg-color", p.foreground),
            ("--secondary-sidebar-backdrop-color", ui.sidebar),
            ("--secondary-sidebar-border-color", ui.border),
            ("--popover-bg-color", ui.popover),
            ("--popover-fg-color", p.foreground),
            ("--dialog-bg-color", ui.popover),
            ("--dialog-fg-color", p.foreground),
            ("--card-bg-color", ui.card),
            ("--card-fg-color", p.foreground),
            ("--thumbnail-bg-color", ui.card),
            ("--thumbnail-fg-color", p.foreground),
            ("--border-color", ui.border),
            ("--accent-bg-color", p.accent),
            ("--accent-fg-color", ui.accent_text),
            ("--accent-color", p.accent),
            ("--destructive-bg-color", ui.error),
            ("--destructive-fg-color", ui.error_text),
            ("--destructive-color", ui.error),
            ("--error-bg-color", ui.error),
            ("--error-fg-color", ui.error_text),
            ("--error-color", ui.error),
            ("--warning-bg-color", ui.warning),
            ("--warning-fg-color", ui.warning_text),
            ("--warning-color", ui.warning),
            ("--success-bg-color", ui.success),
            ("--success-fg-color", ui.success_text),
            ("--success-color", ui.success),
        ]
    }

    /// Sealwax's own variables, in output order: mail roles, control states, paper, avatar
    /// tints, fonts, text sizes, spacing, component sizes and corners.
    pub fn sealwax_variables(&self) -> Vec<(String, String)> {
        let mail = &self.mail;
        let ui = &self.ui;
        let c = &self.controls;
        let mut variables: Vec<(String, String)> = Vec::with_capacity(96);
        let mut push = |name: &str, value: String| variables.push((name.to_owned(), value));

        push("--sw-secondary-fg-color", mail.secondary_text.to_string());
        push(
            "--sw-hover-secondary-fg-color",
            self.rows.hover_secondary_text.to_string(),
        );
        push(
            "--sw-selected-secondary-fg-color",
            self.rows.selected_secondary_text.to_string(),
        );
        push("--sw-star-color", mail.star.to_string());
        push("--sw-unread-color", mail.unread.to_string());
        push(
            "--sw-chip-selected-fg-color",
            mail.selected_chip.to_string(),
        );
        push(
            "--sw-chip-selected-bg-color",
            mail.selected_chip_background.to_string(),
        );
        push("--sw-error-soft-bg-color", ui.error_soft.to_string());
        push("--sw-warning-soft-bg-color", ui.warning_soft.to_string());
        push("--sw-paper-bg-color", PAPER.to_string());
        push("--sw-paper-fg-color", PAPER_TEXT.to_string());

        push(
            "--sw-control-bg-color",
            c.normal.color.with_alpha(c.normal.fill),
        );
        push(
            "--sw-control-border-color",
            c.normal.color.with_alpha(c.normal.border_alpha),
        );
        push("--sw-control-border-width", px(c.normal.border_width));
        push(
            "--sw-hover-bg-color",
            c.hover.color.with_alpha(c.hover.fill),
        );
        push(
            "--sw-hover-border-color",
            c.hover.color.with_alpha(c.hover.border_alpha),
        );
        push(
            "--sw-focus-bg-color",
            c.focus.color.with_alpha(c.focus.fill),
        );
        push(
            "--sw-focus-border-color",
            c.focus.color.with_alpha(c.focus.border_alpha),
        );
        push(
            "--sw-selected-bg-color",
            c.selected.color.with_alpha(c.selected.fill),
        );
        push(
            "--sw-selected-border-color",
            c.selected.color.with_alpha(c.selected.border_alpha),
        );
        push("--sw-selected-border-width", px(c.selected.border_width));
        push(
            "--sw-pressed-bg-color",
            c.pressed_color.with_alpha(c.pressed_fill),
        );
        push(
            "--sw-selection-bg-color",
            c.selection_color.with_alpha(c.selection_fill),
        );

        for (index, tint) in mail.avatars.iter().enumerate() {
            push(
                &format!("--sw-avatar-{index}-bg-color"),
                tint.background.to_string(),
            );
            push(
                &format!("--sw-avatar-{index}-fg-color"),
                tint.foreground.to_string(),
            );
        }

        let fonts = &self.fonts;
        let generic = |mono: bool| if mono { "monospace" } else { "sans-serif" };
        push(
            "--sw-font-chrome",
            css_family(&fonts.chrome, generic(fonts.chrome_is_mono)),
        );
        push(
            "--sw-font-reading",
            css_family(&fonts.reading, generic(fonts.reading_is_mono)),
        );
        push("--sw-font-mono", css_family(&fonts.monospace, "monospace"));
        push("--sw-font-sans", css_family(&fonts.sans, "sans-serif"));

        let s = &self.scale;
        for (name, size) in [
            ("caption", s.caption),
            ("small", s.small),
            ("body", s.body),
            ("subtitle", s.subtitle),
            ("title", s.title),
            ("heading", s.heading),
            ("display", s.display),
            ("reading", s.reading),
        ] {
            push(&format!("--sw-text-{name}"), px(size));
        }
        for (name, size) in self.spacing.tokens() {
            push(&format!("--sw-space-{name}"), px(*size));
        }
        for (name, size) in COMPONENT_SIZES {
            push(&format!("--sw-size-{name}"), px(self.spacing.space(size)));
        }

        push("--window-radius", px(self.radius));
        push("--sw-radius", px(self.radius));
        push("--sw-radius-small", px(self.radius_small));
        let avatar_radius = if self.radius == 0 { 0 } else { 9999 };
        push("--sw-avatar-radius", px(avatar_radius));
        variables
    }

    /// The application-priority stylesheet: one `:root` block with every variable.
    pub fn css(&self) -> String {
        let mut css = String::with_capacity(8 * 1024);
        css.push_str(":root {\n");
        for (name, color) in self.adwaita_colors() {
            let _ = writeln!(css, "  {name}: {color};");
        }
        for (name, value) in self.sealwax_variables() {
            let _ = writeln!(css, "  {name}: {value};");
        }
        css.push_str("}\n");
        css
    }
}

fn px(size: u32) -> String {
    format!("{size}px")
}

#[cfg(test)]
mod tests {
    use super::super::{AVATAR_TINTS, Mode, ThemeInputs, ThemeOptions};
    use super::*;

    fn theme(mode: Mode) -> Theme {
        Theme::new(&ThemeInputs::builtin(mode), &ThemeOptions::default())
    }

    #[test]
    fn css_sets_the_design_table() {
        let theme = theme(Mode::Light);
        let p = &theme.palette;
        let css = theme.css();
        for expected in [
            format!("--window-bg-color: {};", p.background),
            format!("--view-bg-color: {};", p.background),
            format!("--window-fg-color: {};", p.foreground),
            format!("--sidebar-bg-color: {};", p.dark_background),
            format!("--headerbar-bg-color: {};", p.dark_background),
            format!("--popover-bg-color: {};", p.darker_background),
            format!("--dialog-bg-color: {};", p.darker_background),
            format!("--card-bg-color: {};", p.lighter_background),
            format!(
                "--border-color: {};",
                p.lighter_background.mix(p.foreground, 14)
            ),
            format!("--sidebar-border-color: {};", theme.ui.border),
            format!("--accent-bg-color: {};", p.accent),
            format!("--accent-color: {};", p.accent),
            format!(
                "--sw-secondary-fg-color: {};",
                p.foreground.mix(p.background, 35)
            ),
            format!("--sw-chip-selected-fg-color: {};", theme.mail.selected_chip),
            format!("--sw-star-color: {};", p.accent),
            format!("--sw-unread-color: {};", p.accent),
            "--window-radius: 0px;".to_owned(),
            "--sw-avatar-radius: 0px;".to_owned(),
            "--sw-text-body: 12px;".to_owned(),
            "--sw-space-control-height: 28px;".to_owned(),
            "--sw-size-avatar: 32px;".to_owned(),
            "--sw-font-chrome: monospace;".to_owned(),
            "--sw-font-reading: sans-serif;".to_owned(),
        ] {
            assert!(css.contains(&expected), "{expected}\n{css}");
        }
        assert!(css.starts_with(":root {\n") && css.ends_with("}\n"));
        assert_eq!(css.matches(":root").count(), 1);
    }

    #[test]
    fn every_variable_is_set_once() {
        let css = theme(Mode::Dark).css();
        let names: Vec<&str> = css
            .lines()
            .filter_map(|line| line.trim().split_once(':').map(|(name, _)| name))
            .filter(|name| name.starts_with("--"))
            .collect();
        let unique: std::collections::BTreeSet<&&str> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
        for index in 0..AVATAR_TINTS {
            assert!(names.contains(&format!("--sw-avatar-{index}-bg-color").as_str()));
        }
    }

    #[test]
    fn avatar_classes_name_the_tint() {
        let class = avatar_class("ingrid@fjordpost.example");
        assert_eq!(
            class,
            format!(
                "sw-avatar-tint-{}",
                avatar_index("ingrid@fjordpost.example")
            )
        );
    }
}
