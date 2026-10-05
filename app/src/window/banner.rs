//! The window's banner for an account that must sign in again (ML-032; DESIGN.md "Feedback":
//! banners for persistent states; the mock's "Account needs attention"): a strip across the
//! top of the window, over every layout, that names the account and opens its sign-in. It
//! fades in and out (`gtk-enable-animations` decides) and takes no room while hidden.

use super::panes::{classed, icon, named};
use gtk4 as gtk;
use gtk4::prelude::*;

/// Who must sign in again: the account the banner names, and how many more wait after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignInPrompt<'a> {
    pub name: &'a str,
    pub more: usize,
}

/// "Work needs you to sign in again".
pub fn title(name: &str) -> String {
    format!("{name} needs you to sign in again")
}

/// What happened and what waits; the others are signed in one after another.
pub fn body(more: usize) -> String {
    let what = "Microsoft ended the session. Sealwax keeps your mail; nothing syncs until you \
                sign in.";
    match more {
        0 => what.to_owned(),
        1 => format!("{what} 1 more account needs it too."),
        more => format!("{what} {more} more accounts need it too."),
    }
}

pub(super) struct SignInBanner {
    pub(super) root: gtk::Revealer,
    title: gtk::Label,
    body: gtk::Label,
    pub(super) button: gtk::Button,
}

impl SignInBanner {
    pub(super) fn new() -> Self {
        let icon = classed(
            icon(&["dialog-password-symbolic", "dialog-warning-symbolic"]),
            &["sw-banner-icon"],
        );
        icon.set_valign(gtk::Align::Center);
        let text_label = |name: &str, classes: &[&str]| {
            named(
                classed(
                    gtk::Label::builder()
                        .xalign(0.0)
                        .wrap(true)
                        .wrap_mode(gtk::pango::WrapMode::WordChar)
                        .build(),
                    classes,
                ),
                name,
            )
        };
        let title = text_label("sign-in-banner-title", &["sw-banner-title"]);
        let body = text_label("sign-in-banner-body", &[]);
        let text = classed(
            gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build(),
            &["sw-banner-text"],
        );
        text.append(&title);
        text.append(&body);
        let button = named(
            classed(
                gtk::Button::builder()
                    .label("Sign in")
                    .valign(gtk::Align::Center)
                    .build(),
                &["sw-primary", "sw-banner-button"],
            ),
            "sign-in-banner-button",
        );
        let strip = named(
            classed(
                gtk::Box::builder()
                    .accessible_role(gtk::AccessibleRole::Alert)
                    .build(),
                &["sw-banner", "sw-error", "sw-window-banner"],
            ),
            "sign-in-banner-strip",
        );
        strip.append(&icon);
        strip.append(&text);
        strip.append(&button);
        let root = named(
            gtk::Revealer::builder()
                .transition_type(gtk::RevealerTransitionType::Crossfade)
                .transition_duration(150)
                .reveal_child(false)
                .visible(false)
                .child(&strip)
                .build(),
            "sign-in-banner",
        );
        // Hidden once faded out, so it takes no room.
        root.connect_child_revealed_notify(|root| {
            if !root.reveals_child() && !root.is_child_revealed() {
                root.set_visible(false);
            }
        });
        Self {
            root,
            title,
            body,
            button,
        }
    }

    pub(super) fn show(&self, prompt: Option<SignInPrompt<'_>>) {
        match prompt {
            Some(prompt) => {
                let title = title(prompt.name);
                self.title.set_label(&title);
                self.body.set_label(&body(prompt.more));
                if let Some(strip) = self.root.child() {
                    strip.update_property(&[gtk::accessible::Property::Label(&title)]);
                }
                self.root.set_visible(true);
                self.root.set_reveal_child(true);
            }
            None => {
                self.root.set_reveal_child(false);
                if !self.root.is_child_revealed() {
                    self.root.set_visible(false);
                }
            }
        }
    }

    pub(super) fn is_shown(&self) -> bool {
        self.root.reveals_child()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_names_the_account_and_counts_the_rest() {
        assert_eq!(title("Work"), "Work needs you to sign in again");
        assert!(body(0).ends_with("nothing syncs until you sign in."));
        assert!(body(1).ends_with(" 1 more account needs it too."));
        assert!(body(3).ends_with(" 3 more accounts need it too."));
    }
}
