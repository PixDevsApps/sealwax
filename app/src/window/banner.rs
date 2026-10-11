//! The window's banner (DESIGN.md "Feedback": banners for persistent states; the mock's
//! "Account needs attention" and offline strips): one strip across the top of the window, over
//! every layout, that shows the most important state and counts the others (ML-032, ML-048).
//! It fades in and out (`gtk-enable-animations` decides) and takes no room while hidden.
//!
//! The order of importance is the caller's ([`crate::status`]); this module says each state in
//! words, tints the strip and offers the state's buttons, which report a [`BannerAction`].

use super::panes::{classed, icon, named};
use gtk4 as gtk;
use gtk4::gio;
use gtk4::prelude::*;
use sealwax_engine::api::UncertainReason;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A state the banner can show (ML-048), from the most to the least important kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// No Secret Service answered: passwords and sign-ins stay in memory until Sealwax quits.
    NoKeyring,
    /// The account's server shows a certificate that no longer verifies, so Sealwax stopped
    /// connecting to it.
    CertificateChanged { name: String, host: String },
    /// The server refused the account's password (`browser` false) or its Microsoft sign-in.
    SignIn { name: String, browser: bool },
    /// The server says the account's mailbox is full.
    QuotaFull { name: String },
    /// A message from the account may have been sent (Sealwax stopped while sending it, or the
    /// server's reply didn't come or couldn't be read; `reason` `None` when the engine said why
    /// in an earlier session), and it is never sent again without the user's word (M6, ADR-015).
    UncertainSend {
        name: String,
        reason: Option<UncertainReason>,
    },
    /// The account's rules file can't run, so no rule runs until it is fixed (M9, ADR-020):
    /// `problem` says where and why ("Line 3, column 7: …").
    RulesFailed { name: String, problem: String },
    /// The desktop has no network (`network`), or no account's server answers; `waiting`
    /// changes wait for a server.
    Offline {
        network: bool,
        accounts: usize,
        waiting: u32,
    },
}

/// What a banner button asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    /// Microsoft sign-in in the browser, for the account shown.
    SignIn,
    /// The setup assistant's password step, for the account shown.
    TypePassword,
    /// Reconnect now.
    Retry,
    /// Why the certificate stopped Sealwax, and the account's servers.
    Details,
    /// Hide the no-keyring banner for this session.
    Dismiss,
    /// Ask what became of the uncertain send shown: send again, it was sent, or back to a draft.
    Decide,
    /// Open the rules file of the account shown in the desktop's editor.
    OpenRules,
}

/// The strip's tint: the error or warning colour's soft background, or the card's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Error,
    Warning,
    Neutral,
}

impl Attention {
    pub fn tone(&self) -> Tone {
        match self {
            Self::CertificateChanged { .. } | Self::SignIn { .. } => Tone::Error,
            Self::QuotaFull { .. }
            | Self::UncertainSend { .. }
            | Self::RulesFailed { .. }
            | Self::Offline { .. } => Tone::Warning,
            Self::NoKeyring => Tone::Neutral,
        }
    }

    fn icons(&self) -> &'static [&'static str] {
        match self {
            Self::NoKeyring => &["changes-prevent-symbolic", "system-lock-screen-symbolic"],
            Self::CertificateChanged { .. } => &[
                "security-low-symbolic",
                "channel-insecure-symbolic",
                "dialog-warning-symbolic",
            ],
            Self::SignIn { .. } => &["dialog-password-symbolic", "dialog-warning-symbolic"],
            Self::QuotaFull { .. } => &["drive-harddisk-symbolic", "dialog-warning-symbolic"],
            Self::UncertainSend { .. } => &["mail-send-symbolic", "dialog-warning-symbolic"],
            Self::RulesFailed { .. } => &["dialog-warning-symbolic"],
            Self::Offline { .. } => &[
                "network-offline-symbolic",
                "network-wireless-offline-symbolic",
            ],
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::NoKeyring => "No keyring found".to_owned(),
            Self::CertificateChanged { name, .. } => {
                format!("Sealwax stopped connecting to {name}")
            }
            Self::SignIn { name, .. } => format!("{name} needs you to sign in again"),
            Self::QuotaFull { name } => format!("{name}'s mailbox is full"),
            Self::UncertainSend { .. } => "A message may have been sent".to_owned(),
            Self::RulesFailed { name, .. } => crate::rules::text::failed_title(name),
            Self::Offline { network: true, .. } => "You're offline".to_owned(),
            Self::Offline { accounts: 1, .. } => "Can't reach your mail server".to_owned(),
            Self::Offline { .. } => "Can't reach your mail servers".to_owned(),
        }
    }

    /// What happened and what to do next.
    pub fn body(&self) -> String {
        match self {
            Self::NoKeyring => "Passwords and sign-ins stay in memory until Sealwax quits, so \
                                you'll type them again next time. Start a Secret Service such \
                                as gnome-keyring to keep them."
                .to_owned(),
            Self::CertificateChanged { host, .. } => format!(
                "{host} shows a certificate that no longer verifies, and Sealwax never signs in \
                 to a server it can't verify. A network that inspects secure connections can \
                 cause this: try another network, then Retry."
            ),
            Self::SignIn { browser: true, .. } => "Microsoft ended the session. Sealwax keeps \
                                                   your mail; nothing syncs until you sign in."
                .to_owned(),
            Self::SignIn { browser: false, .. } => "The server stopped accepting the password \
                                                    Sealwax has. Sealwax keeps your mail; \
                                                    nothing syncs until you type it again."
                .to_owned(),
            Self::QuotaFull { .. } => "The server has no room for more mail: new messages may \
                                       bounce, and changes can't be saved. Delete mail you \
                                       don't need, or ask your provider for more space."
                .to_owned(),
            Self::UncertainSend { name, reason } => format!(
                "{} Look in your Sent folder, or ask a recipient, before you send it again.",
                crate::outbox::uncertain_cause(name, *reason)
            ),
            Self::RulesFailed { problem, .. } => crate::rules::text::failed_body(problem),
            Self::Offline {
                network: true,
                waiting: 0,
                ..
            } => {
                "Sealwax shows the mail it has and syncs again when the network is back.".to_owned()
            }
            Self::Offline { network: true, .. } => "Sealwax shows the mail it has, keeps your \
                                                    changes and sends them when the network is \
                                                    back."
                .to_owned(),
            Self::Offline { waiting: 0, .. } => "Sealwax shows the mail it has and tries again \
                                                 by itself. Check your connection, or try \
                                                 another network."
                .to_owned(),
            Self::Offline { .. } => "Sealwax shows the mail it has, keeps your changes and tries \
                                     again by itself. Check your connection, or try another \
                                     network."
                .to_owned(),
        }
    }

    /// The changes waiting for a server, as the offline strip counts them ("3 waiting").
    pub fn waiting(&self) -> Option<String> {
        match self {
            Self::Offline { waiting, .. } if *waiting > 0 => Some(format!("{waiting} waiting")),
            _ => None,
        }
    }

    /// The plain button and the main one, as (label, action); either may be missing.
    pub fn buttons(&self) -> [Option<(&'static str, BannerAction)>; 2] {
        match self {
            Self::NoKeyring => [Some(("Dismiss", BannerAction::Dismiss)), None],
            Self::CertificateChanged { .. } => [
                Some(("Details", BannerAction::Details)),
                Some(("Retry", BannerAction::Retry)),
            ],
            Self::SignIn { browser: true, .. } => [None, Some(("Sign in", BannerAction::SignIn))],
            Self::SignIn { browser: false, .. } => [
                None,
                Some(("Type the password again", BannerAction::TypePassword)),
            ],
            Self::UncertainSend { .. } => [None, Some(("Choose…", BannerAction::Decide))],
            Self::RulesFailed { .. } => [None, Some(("Open File", BannerAction::OpenRules))],
            Self::QuotaFull { .. } | Self::Offline { .. } => [None, None],
        }
    }

    /// One account's state, which others can share.
    fn per_account(&self) -> bool {
        matches!(
            self,
            Self::CertificateChanged { .. }
                | Self::SignIn { .. }
                | Self::QuotaFull { .. }
                | Self::RulesFailed { .. }
        )
    }

    fn same_kind(&self, other: &Attention) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// The banner's text for `shown`, followed by the count of the states waiting behind it.
pub fn body(shown: &Attention, rest: &[Attention]) -> String {
    let body = shown.body();
    let count = rest.len();
    if count == 0 {
        return body;
    }
    let alike = shown.per_account() && rest.iter().all(|other| shown.same_kind(other));
    let more = match (alike, shown, count) {
        (true, Attention::SignIn { .. }, 1) => "1 more account needs it too.".to_owned(),
        (true, Attention::SignIn { .. }, count) => format!("{count} more accounts need it too."),
        (true, _, 1) => "1 more account has the same problem.".to_owned(),
        (true, _, count) => format!("{count} more accounts have the same problem."),
        (false, _, 1) => "1 more problem waits after this one.".to_owned(),
        (false, _, count) => format!("{count} more problems wait after this one."),
    };
    format!("{body} {more}")
}

pub(super) struct Banner {
    pub(super) root: gtk::Revealer,
    strip: gtk::Box,
    icon: gtk::Image,
    title: gtk::Label,
    body: gtk::Label,
    /// The offline strip's "3 waiting".
    waiting: gtk::Label,
    secondary: gtk::Button,
    primary: gtk::Button,
    /// What each button does for the state shown.
    actions: Rc<[Cell<Option<BannerAction>>; 2]>,
    /// What it shows now: the account states change often during a sync, the banner rarely.
    shown: RefCell<Vec<Attention>>,
}

impl Banner {
    pub(super) fn new() -> Self {
        let icon = named(
            classed(icon(&["dialog-warning-symbolic"]), &["sw-banner-icon"]),
            "banner-icon",
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
        let title = text_label("banner-title", &["sw-banner-title"]);
        let body = text_label("banner-body", &[]);
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
        let waiting = named(
            classed(
                gtk::Label::builder()
                    .valign(gtk::Align::Center)
                    .visible(false)
                    .build(),
                &["sw-banner-waiting"],
            ),
            "banner-waiting",
        );
        let button = |name: &str, classes: &[&str]| {
            named(
                classed(
                    gtk::Button::builder().valign(gtk::Align::Center).build(),
                    classes,
                ),
                name,
            )
        };
        let secondary = button("banner-secondary", &["sw-banner-secondary"]);
        let primary = button("banner-primary", &["sw-primary", "sw-banner-button"]);
        let strip = named(
            classed(
                gtk::Box::builder()
                    .accessible_role(gtk::AccessibleRole::Alert)
                    .build(),
                &["sw-banner", "sw-window-banner"],
            ),
            "banner-strip",
        );
        strip.append(&icon);
        strip.append(&text);
        strip.append(&waiting);
        strip.append(&secondary);
        strip.append(&primary);
        let root = named(
            gtk::Revealer::builder()
                .transition_type(gtk::RevealerTransitionType::Crossfade)
                .transition_duration(150)
                .reveal_child(false)
                .visible(false)
                .child(&strip)
                .build(),
            "banner",
        );
        // Hidden once faded out, so it takes no room.
        root.connect_child_revealed_notify(|root| {
            if !root.reveals_child() && !root.is_child_revealed() {
                root.set_visible(false);
            }
        });
        Self {
            root,
            strip,
            icon,
            title,
            body,
            waiting,
            secondary,
            primary,
            actions: Rc::new([Cell::new(None), Cell::new(None)]),
            shown: RefCell::default(),
        }
    }

    /// Shows the first of `queue` and counts the others; an empty queue hides the banner.
    pub(super) fn show(&self, queue: &[Attention]) {
        if *self.shown.borrow() == queue {
            return;
        }
        self.shown.replace(queue.to_vec());
        let Some((shown, rest)) = queue.split_first() else {
            self.root.set_reveal_child(false);
            if !self.root.is_child_revealed() {
                self.root.set_visible(false);
            }
            return;
        };
        let title = shown.title();
        self.title.set_label(&title);
        self.body.set_label(&body(shown, rest));
        let waiting = shown.waiting();
        self.waiting
            .set_label(waiting.as_deref().unwrap_or_default());
        self.waiting.set_visible(waiting.is_some());
        self.icon
            .set_from_gicon(&gio::ThemedIcon::from_names(shown.icons()));
        for (tone, class) in [(Tone::Error, "sw-error"), (Tone::Warning, "sw-warning")] {
            if shown.tone() == tone {
                self.strip.add_css_class(class);
            } else {
                self.strip.remove_css_class(class);
            }
        }
        for (index, (button, choice)) in [&self.secondary, &self.primary]
            .into_iter()
            .zip(shown.buttons())
            .enumerate()
        {
            button.set_visible(choice.is_some());
            if let Some((label, _)) = choice {
                button.set_label(label);
            }
            self.actions[index].set(choice.map(|(_, action)| action));
        }
        self.strip
            .update_property(&[gtk::accessible::Property::Label(&title)]);
        self.root.set_visible(true);
        self.root.set_reveal_child(true);
    }

    pub(super) fn is_shown(&self) -> bool {
        self.root.reveals_child()
    }

    /// Runs `handler` with the action of every button clicked.
    pub(super) fn connect(&self, handler: impl Fn(BannerAction) + 'static) {
        let handler = Rc::new(handler);
        for (index, button) in [&self.secondary, &self.primary].into_iter().enumerate() {
            let actions = self.actions.clone();
            let handler = handler.clone();
            button.connect_clicked(move |_| {
                if let Some(action) = actions[index].get() {
                    handler(action);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign_in(name: &str, browser: bool) -> Attention {
        Attention::SignIn {
            name: name.to_owned(),
            browser,
        }
    }

    #[test]
    fn the_banner_names_the_account_and_counts_the_rest() {
        let kari = sign_in("Kari", true);
        assert_eq!(kari.title(), "Kari needs you to sign in again");
        assert!(body(&kari, &[]).ends_with("nothing syncs until you sign in."));
        assert!(body(&kari, &[sign_in("Ola", false)]).ends_with(" 1 more account needs it too."));
        let rest = [
            sign_in("Ola", false),
            sign_in("Anne", true),
            sign_in("Per", true),
        ];
        assert!(body(&kari, &rest).ends_with(" 3 more accounts need it too."));
        let certificate = Attention::CertificateChanged {
            name: "Work".to_owned(),
            host: "imap.example.test".to_owned(),
        };
        let other = Attention::CertificateChanged {
            name: "Home".to_owned(),
            host: "mail.example.test".to_owned(),
        };
        assert!(
            body(&certificate, std::slice::from_ref(&other))
                .ends_with(" 1 more account has the same problem.")
        );
        assert!(
            body(&certificate, &[other, kari.clone()])
                .ends_with(" 2 more problems wait after this one.")
        );
        let offline = Attention::Offline {
            network: true,
            accounts: 2,
            waiting: 0,
        };
        assert!(
            body(&Attention::NoKeyring, std::slice::from_ref(&offline))
                .ends_with(" 1 more problem waits after this one.")
        );
    }

    #[test]
    fn every_state_says_what_happened_and_what_to_do() {
        let states = [
            Attention::NoKeyring,
            Attention::CertificateChanged {
                name: "Work".to_owned(),
                host: "imap.example.test".to_owned(),
            },
            sign_in("Work", true),
            sign_in("Work", false),
            Attention::QuotaFull {
                name: "Work".to_owned(),
            },
            Attention::Offline {
                network: true,
                accounts: 1,
                waiting: 0,
            },
            Attention::Offline {
                network: false,
                accounts: 1,
                waiting: 0,
            },
            Attention::Offline {
                network: false,
                accounts: 2,
                waiting: 0,
            },
        ];
        let titles: Vec<String> = states.iter().map(Attention::title).collect();
        assert_eq!(
            titles,
            [
                "No keyring found",
                "Sealwax stopped connecting to Work",
                "Work needs you to sign in again",
                "Work needs you to sign in again",
                "Work's mailbox is full",
                "You're offline",
                "Can't reach your mail server",
                "Can't reach your mail servers",
            ]
        );
        assert!(
            states[1]
                .body()
                .starts_with("imap.example.test shows a certificate")
        );
        assert!(states[1].body().ends_with("then Retry."));
        assert!(states[3].body().contains("until you type it again"));
        let buttons: Vec<Vec<BannerAction>> = states
            .iter()
            .map(|state| {
                state
                    .buttons()
                    .into_iter()
                    .flatten()
                    .map(|(_, action)| action)
                    .collect()
            })
            .collect();
        assert_eq!(buttons[0], [BannerAction::Dismiss]);
        assert_eq!(buttons[1], [BannerAction::Details, BannerAction::Retry]);
        assert_eq!(buttons[2], [BannerAction::SignIn]);
        assert_eq!(buttons[3], [BannerAction::TypePassword]);
        assert!(buttons[4..].iter().all(Vec::is_empty));
        let tones: Vec<Tone> = states.iter().map(Attention::tone).collect();
        assert_eq!(
            tones[..5],
            [
                Tone::Neutral,
                Tone::Error,
                Tone::Error,
                Tone::Error,
                Tone::Warning
            ]
        );
        assert!(tones[5..].iter().all(|tone| *tone == Tone::Warning));
    }

    #[test]
    fn the_offline_strip_counts_the_changes_waiting() {
        let offline = |network, waiting| Attention::Offline {
            network,
            accounts: 1,
            waiting,
        };
        assert_eq!(offline(true, 0).waiting(), None);
        assert_eq!(offline(true, 3).waiting().as_deref(), Some("3 waiting"));
        assert_eq!(offline(false, 1).waiting().as_deref(), Some("1 waiting"));
        assert!(offline(true, 3).body().contains("keeps your changes"));
        assert!(offline(false, 1).body().contains("keeps your changes"));
        assert!(!offline(true, 0).body().contains("changes"));
        assert_eq!(Attention::NoKeyring.waiting(), None);
    }
}
