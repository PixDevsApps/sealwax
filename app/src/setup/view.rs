//! The assistant's widgets (the mock's Setup artboard): the column, its sections and the
//! footer, built once and shown or hidden for the screen on view. Every class is a `sw-setup-*`
//! class of `style.css`; every part a self-test checks has a `setup-*` name.

use super::form::{Field, Problem};
use super::sources::{self, Sources, State as SourceState};
use super::text::{self, Tone};
use super::{CONTENT_HEIGHT, CONTENT_WIDTH, State, Step};
use crate::window::panes::{classed, icon, icon_button, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::account::{Endpoint, HelpLink, Protocol, Security};
use std::cell::RefCell;

/// One server's manual fields.
pub(super) struct ServerFields {
    pub(super) host: gtk::Entry,
    pub(super) port: gtk::Entry,
    pub(super) security: gtk::DropDown,
}

impl ServerFields {
    pub(super) fn security(&self) -> Security {
        match self.security.selected() {
            1 => Security::StartTls,
            _ => Security::Tls,
        }
    }

    /// The security goes first: it moves a standard port along, and the port set after it
    /// stays as found.
    pub(super) fn fill(&self, endpoint: &Endpoint) {
        self.security.set_selected(match endpoint.security {
            Security::Tls => 0,
            Security::StartTls => 1,
        });
        self.host.set_text(&endpoint.host);
        self.port.set_text(&endpoint.port.to_string());
    }
}

/// A row in the sources list, or the server check under it.
struct StatusRow {
    root: gtk::Box,
    mark: gtk::Stack,
    glyph: gtk::Label,
    status: gtk::Label,
}

impl StatusRow {
    fn new(name: &str) -> Self {
        let glyph = classed(
            gtk::Label::builder()
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
            &["sw-setup-glyph"],
        );
        let spinner = adw::Spinner::new();
        spinner
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Checking")]);
        let mark = classed(
            gtk::Stack::builder().valign(gtk::Align::Center).build(),
            &["sw-setup-mark"],
        );
        mark.add_named(&glyph, Some("glyph"));
        mark.add_named(&spinner, Some("spinner"));
        let name = classed(
            gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
            &["sw-setup-source"],
        );
        let status = classed(gtk::Label::new(None), &["sw-setup-status"]);
        let root = classed(
            gtk::Box::new(gtk::Orientation::Horizontal, 0),
            &["sw-setup-row"],
        );
        root.append(&mark);
        root.append(&name);
        root.append(&status);
        Self {
            root,
            mark,
            glyph,
            status,
        }
    }

    fn set(&self, state: SourceState) {
        let (glyph, ok) = match state {
            SourceState::Found => ("✓", true),
            _ => ("–", false),
        };
        self.mark
            .set_visible_child_name(if state == SourceState::Checking {
                "spinner"
            } else {
                "glyph"
            });
        self.glyph.set_label(glyph);
        set_class(&self.glyph, "sw-setup-ok", ok);
        self.status.set_label(state.word());
        set_class(&self.status, "sw-setup-found", ok);
    }
}

/// The parts of the column the assistant shows, hides and reads.
pub(super) struct Widgets {
    pub(super) title: gtk::Label,
    pub(super) subtitle: gtk::Label,
    pub(super) column: gtk::Box,
    pub(super) alert: gtk::Box,
    pub(super) alert_icon: gtk::Image,
    pub(super) alert_title: gtk::Label,
    pub(super) alert_body: gtk::Label,
    pub(super) alert_link: gtk::LinkButton,
    pub(super) address_field: gtk::Box,
    pub(super) address: gtk::Entry,
    pub(super) name_field: gtk::Box,
    pub(super) name: gtk::Entry,
    pub(super) sources: gtk::Box,
    pub(super) found: gtk::Box,
    pub(super) found_lines: [gtk::Label; 4],
    pub(super) found_note: gtk::Label,
    pub(super) microsoft: gtk::Box,
    pub(super) username_field: gtk::Box,
    pub(super) username: gtk::Entry,
    pub(super) manual: gtk::Box,
    pub(super) imap: ServerFields,
    pub(super) smtp: ServerFields,
    pub(super) manual_username: gtk::Entry,
    pub(super) password_field: gtk::Box,
    pub(super) password_label: gtk::Label,
    pub(super) password: gtk::PasswordEntry,
    pub(super) password_hint: gtk::Label,
    pub(super) app_password_link: gtk::LinkButton,
    pub(super) enable_imap_link: gtk::LinkButton,
    check: StatusRow,
    pub(super) validation: gtk::Box,
    pub(super) validation_text: gtk::Label,
    pub(super) manual_link: gtk::Button,
    pub(super) back: gtk::Button,
    pub(super) primary: gtk::Button,
    pub(super) close: gtk::Button,
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    classed(
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build(),
        classes,
    )
}

fn column(classes: &[&str]) -> gtk::Box {
    classed(gtk::Box::new(gtk::Orientation::Vertical, 0), classes)
}

/// A labelled field: the label above, tied to the control for screen readers.
fn field(text: &str, control: &impl IsA<gtk::Widget>, name: &str) -> gtk::Box {
    let caption = label(text, &["sw-setup-label"]);
    caption.set_mnemonic_widget(Some(control));
    control
        .upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(text)]);
    let field = named(column(&["sw-setup-field"]), name);
    field.append(&caption);
    field.append(control);
    field
}

fn entry(name: &str, placeholder: &str, classes: &[&str]) -> gtk::Entry {
    let entry = named(
        classed(
            gtk::Entry::builder()
                .placeholder_text(placeholder)
                .hexpand(true)
                .build(),
            &["sw-setup-entry"],
        ),
        name,
    );
    classed(entry, classes)
}

fn server_fields(protocol: Protocol) -> (ServerFields, gtk::Box) {
    let (key, server, placeholder) = match protocol {
        Protocol::Imap => ("imap", "IMAP server", "imap.example.test"),
        Protocol::Smtp => ("smtp", "SMTP server", "smtp.example.test"),
    };
    let host = entry(
        &format!("setup-{key}-host"),
        placeholder,
        &["sw-setup-small"],
    );
    let port = entry(&format!("setup-{key}-port"), "", &["sw-setup-small"]);
    port.set_input_purpose(gtk::InputPurpose::Digits);
    port.set_width_chars(5);
    let security = named(
        classed(
            gtk::DropDown::from_strings(&["TLS", "STARTTLS"]),
            &["sw-setup-security"],
        ),
        &format!("setup-{key}-security"),
    );
    // The mock's four columns: the server takes two, the port and the security one each.
    let pair = classed(
        gtk::Box::builder().homogeneous(true).build(),
        &["sw-setup-grid-row"],
    );
    pair.append(&field("Port", &port, &format!("setup-{key}-port-field")));
    pair.append(&field(
        "Security",
        &security,
        &format!("setup-{key}-security-field"),
    ));
    let row = classed(
        gtk::Box::builder().homogeneous(true).build(),
        &["sw-setup-grid-row"],
    );
    row.append(&field(server, &host, &format!("setup-{key}-host-field")));
    row.append(&pair);
    (
        ServerFields {
            host,
            port,
            security,
        },
        row,
    )
}

fn link(name: &str) -> gtk::LinkButton {
    let link = named(gtk::LinkButton::new("about:blank"), name);
    link.set_halign(gtk::Align::Start);
    classed(link, &["sw-setup-link"])
}

impl Widgets {
    /// The assistant's column and the dialog that holds it.
    pub(super) fn build() -> (Self, adw::Dialog) {
        let caption = label("Add an account", &["sw-setup-caption"]);
        let title = named(label("", &["sw-setup-title"]), "setup-title");
        let subtitle = named(label("", &["sw-setup-subtitle"]), "setup-subtitle");

        let alert_icon = classed(icon(&["dialog-error-symbolic"]), &["sw-setup-alert-icon"]);
        alert_icon.set_valign(gtk::Align::Start);
        let alert_title = named(label("", &["sw-setup-alert-title"]), "setup-alert-title");
        let alert_body = named(label("", &["sw-setup-alert-body"]), "setup-alert-body");
        let alert_link = link("setup-alert-link");
        let alert_text = column(&["sw-setup-alert-text"]);
        alert_text.set_hexpand(true);
        alert_text.append(&alert_title);
        alert_text.append(&alert_body);
        alert_text.append(&alert_link);
        let alert = named(
            classed(
                gtk::Box::builder()
                    .accessible_role(gtk::AccessibleRole::Alert)
                    .build(),
                &["sw-setup-alert"],
            ),
            "setup-alert",
        );
        alert.append(&alert_icon);
        alert.append(&alert_text);

        let address = entry("setup-address", "name@example.test", &[]);
        address.set_input_purpose(gtk::InputPurpose::Email);
        let address_field = field("Email address", &address, "setup-address-field");
        let name = entry("setup-name", "Shown to the people you write to", &[]);
        name.set_input_purpose(gtk::InputPurpose::Name);
        let name_field = field("Your name", &name, "setup-name-field");

        let sources = named(column(&["sw-setup-sources"]), "setup-sources");

        let found_lines = [
            named(label("", &["sw-setup-provider"]), "setup-found-provider"),
            named(label("", &[]), "setup-found-imap"),
            named(label("", &[]), "setup-found-smtp"),
            named(label("", &[]), "setup-found-username"),
        ];
        let found_note = named(label("", &["sw-setup-card-note"]), "setup-found-note");
        let found = named(column(&["sw-setup-card"]), "setup-found");
        for line in &found_lines {
            found.append(line);
        }
        found.append(&found_note);

        let microsoft_title = label("Sign in with Microsoft", &["sw-setup-provider"]);
        let microsoft_button = named(
            classed(
                gtk::Button::builder()
                    .label("Sign in with Microsoft")
                    .halign(gtk::Align::Start)
                    .sensitive(false)
                    .tooltip_text("Not available in this build yet")
                    .build(),
                &["sw-primary", "sw-setup-primary"],
            ),
            "setup-microsoft-button",
        );
        let microsoft_note = named(
            label(
                "Microsoft sign-in isn't available in this build of Sealwax yet.",
                &["sw-setup-note"],
            ),
            "setup-microsoft-note",
        );
        let microsoft = named(
            column(&["sw-setup-card", "sw-setup-microsoft"]),
            "setup-microsoft",
        );
        microsoft.append(&microsoft_title);
        microsoft.append(&label(
            "Your browser opens Microsoft's sign-in page. Sealwax never sees your password; it \
             keeps a sign-in token in your keyring.",
            &[],
        ));
        microsoft.append(&microsoft_button);
        let microsoft_footnote = named(
            label(
                "Outlook.com and Microsoft 365 accept only Microsoft sign-in for mail apps.",
                &["sw-setup-footnote"],
            ),
            "setup-microsoft-footnote",
        );
        let microsoft_box = column(&["sw-setup-section"]);
        microsoft_box.append(&microsoft);
        microsoft_box.append(&microsoft_note);
        microsoft_box.append(&microsoft_footnote);

        let username = entry("setup-username", "", &[]);
        let username_field = field("Username", &username, "setup-username-field");

        let (imap, imap_row) = server_fields(Protocol::Imap);
        let (smtp, smtp_row) = server_fields(Protocol::Smtp);
        let manual_username = entry("setup-manual-username", "", &["sw-setup-small"]);
        let manual = named(column(&["sw-setup-section"]), "setup-manual");
        manual.append(&label("Set up manually", &["sw-setup-caption"]));
        manual.append(&imap_row);
        manual.append(&smtp_row);
        manual.append(&field(
            "Username",
            &manual_username,
            "setup-manual-username-field",
        ));

        let password = named(
            classed(
                gtk::PasswordEntry::builder()
                    .show_peek_icon(true)
                    .hexpand(true)
                    .build(),
                &["sw-setup-entry"],
            ),
            "setup-password",
        );
        let password_label = named(
            label("Password", &["sw-setup-label"]),
            "setup-password-label",
        );
        password_label.set_mnemonic_widget(Some(&password));
        let password_hint = named(label("", &["sw-setup-hint"]), "setup-password-hint");
        let app_password_link = link("setup-app-password-link");
        let enable_imap_link = link("setup-enable-imap-link");
        let password_field = named(column(&["sw-setup-field"]), "setup-password-field");
        password_field.append(&password_label);
        password_field.append(&password);
        password_field.append(&password_hint);
        password_field.append(&app_password_link);
        password_field.append(&enable_imap_link);

        let check = StatusRow::new("Server check");
        check.root.set_widget_name("setup-check");

        let validation_text = named(label("", &["sw-setup-validation-text"]), "setup-validation");
        validation_text.set_hexpand(true);
        let validation = classed(
            gtk::Box::new(gtk::Orientation::Horizontal, 0),
            &["sw-setup-validation"],
        );
        validation.append(&classed(
            icon(&["dialog-warning-symbolic"]),
            &["sw-setup-alert-icon"],
        ));
        validation.append(&validation_text);

        let content = named(column(&["sw-setup-column"]), "setup-column");
        for part in [
            alert.upcast_ref::<gtk::Widget>(),
            address_field.upcast_ref(),
            name_field.upcast_ref(),
            sources.upcast_ref(),
            found.upcast_ref(),
            microsoft_box.upcast_ref(),
            username_field.upcast_ref(),
            manual.upcast_ref(),
            password_field.upcast_ref(),
            check.root.upcast_ref(),
            validation.upcast_ref(),
        ] {
            content.append(part);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();

        let manual_link = named(
            classed(
                gtk::Button::builder()
                    .label("Set up manually")
                    .halign(gtk::Align::Start)
                    .valign(gtk::Align::Center)
                    .build(),
                &["sw-setup-link"],
            ),
            "setup-manual-link",
        );
        let back = named(
            classed(
                gtk::Button::builder().label("Back").build(),
                &["sw-setup-secondary"],
            ),
            "setup-back",
        );
        let primary = named(
            classed(
                gtk::Button::builder().label("Next").build(),
                &["sw-primary", "sw-setup-primary"],
            ),
            "setup-primary",
        );
        let footer = classed(
            gtk::Box::new(gtk::Orientation::Horizontal, 0),
            &["sw-setup-footer"],
        );
        footer.append(&manual_link);
        footer.append(&gtk::Box::builder().hexpand(true).build());
        footer.append(&back);
        footer.append(&primary);

        let sheet = column(&["sw-setup-sheet"]);
        sheet.append(&caption);
        sheet.append(&title);
        sheet.append(&subtitle);
        sheet.append(&scroller);
        sheet.append(&footer);
        let close = named(
            icon_button(&["window-close-symbolic"], "Cancel (Escape)"),
            "setup-close",
        );
        close.set_halign(gtk::Align::End);
        close.set_valign(gtk::Align::Start);
        close.add_css_class("sw-setup-close");
        let overlay = gtk::Overlay::builder().child(&sheet).build();
        overlay.add_overlay(&close);

        let dialog = named(
            classed(
                adw::Dialog::builder()
                    .title("Add an account")
                    .content_width(CONTENT_WIDTH)
                    .content_height(CONTENT_HEIGHT)
                    .can_close(false)
                    .child(&overlay)
                    .focus_widget(&address)
                    .build(),
                &["sw-setup"],
            ),
            "setup",
        );

        let widgets = Widgets {
            title,
            subtitle,
            column: content,
            alert,
            alert_icon,
            alert_title,
            alert_body,
            alert_link,
            address_field,
            address,
            name_field,
            name,
            sources,
            found,
            found_lines,
            found_note,
            microsoft: microsoft_box,
            username_field,
            username,
            manual,
            imap,
            smtp,
            manual_username,
            password_field,
            password_label,
            password,
            password_hint,
            app_password_link,
            enable_imap_link,
            check,
            validation,
            validation_text,
            manual_link,
            back,
            primary,
            close,
        };
        (widgets, dialog)
    }

    /// Shows `state`: the sections of the screen on view, its texts, and whether its button
    /// can be pressed (`problem`).
    pub(super) fn draw(
        &self,
        state: &State,
        problem: Option<Problem>,
        drawn_sources: &RefCell<Option<Sources>>,
    ) {
        let w = self;
        let step = state.step;
        let shown = state.shown();
        let busy = step.is_busy();
        let found = state.found();
        let domain = state
            .address
            .as_ref()
            .map(|address| address.domain.as_str())
            .unwrap_or_default();

        let (title, subtitle) = heading(state);
        w.title.set_label(&title);
        w.subtitle.set_label(subtitle);
        w.subtitle.set_visible(!subtitle.is_empty());

        let notice = state.notice.as_ref().filter(|_| !busy);
        w.alert.set_visible(notice.is_some());
        if let Some(notice) = notice {
            w.alert_title.set_label(&notice.title);
            w.alert_body.set_label(&notice.body);
            set_class(&w.alert, "sw-setup-info", notice.tone == Tone::Info);
            w.alert_icon
                .set_from_gicon(&gtk4::gio::ThemedIcon::from_names(match notice.tone {
                    Tone::Problem => &["dialog-error-symbolic"],
                    Tone::Info => &["dialog-information-symbolic"],
                }));
            show_link(&w.alert_link, notice.link.as_ref());
        }

        w.address_field.set_visible(matches!(
            shown,
            Step::Address
                | Step::Detecting
                | Step::Password
                | Step::Microsoft
                | Step::Blocked
                | Step::Manual
        ));
        w.name_field
            .set_visible(matches!(shown, Step::Address | Step::Manual));

        let listed = matches!(
            shown,
            Step::Detecting | Step::Password | Step::Microsoft | Step::Blocked
        ) && !state.sources.rows().is_empty();
        w.sources.set_visible(listed);
        if listed && drawn_sources.borrow().as_ref() != Some(&state.sources) {
            while let Some(child) = w.sources.first_child() {
                w.sources.remove(&child);
            }
            for (source, source_state) in state.sources.rows() {
                let row = StatusRow::new(&text::source(*source, domain));
                row.root
                    .set_widget_name(&format!("setup-source:{}", sources::name(*source)));
                row.set(*source_state);
                w.sources.append(&row.root);
            }
            drawn_sources.replace(Some(state.sources.clone()));
        }

        let card = found.filter(|_| shown == Step::Password);
        w.found.set_visible(card.is_some());
        if let Some(settings) = card {
            for (line, value) in w.found_lines.iter().zip(text::found(settings, domain)) {
                line.set_label(&value);
            }
            w.found_note.set_label(&text::confirm_note(settings));
            w.found_note.set_visible(settings.confirm);
        }
        w.username_field
            .set_visible(shown == Step::Password && state.asks_username());

        w.microsoft.set_visible(shown == Step::Microsoft);

        w.manual
            .set_visible(matches!(shown, Step::Manual | Step::Failed));
        w.password_field.set_visible(matches!(
            shown,
            Step::Password | Step::Manual | Step::Failed
        ));
        let app_password = found.and_then(|settings| settings.app_password.as_ref());
        let enable_imap = found.and_then(|settings| settings.enable_imap.as_ref());
        let provider = found.and_then(|settings| settings.provider.as_deref());
        w.password_label.set_label(if app_password.is_some() {
            "App password"
        } else {
            "Password"
        });
        w.password
            .update_property(&[gtk::accessible::Property::Label(
                w.password_label.label().as_str(),
            )]);
        let hints: Vec<String> = [
            app_password.map(|_| text::app_password_hint(provider)),
            enable_imap.map(|_| text::enable_imap_hint(provider)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let hinted = shown == Step::Password && !hints.is_empty();
        w.password_hint.set_visible(hinted);
        w.password_hint.set_label(&hints.join(" "));
        show_link(
            &w.app_password_link,
            app_password.filter(|_| shown == Step::Password),
        );
        show_link(
            &w.enable_imap_link,
            enable_imap.filter(|_| shown == Step::Password),
        );

        w.check
            .root
            .set_visible(shown == Step::Password || matches!(step, Step::Checking | Step::Adding));
        match step {
            Step::Checking => {
                w.check.mark.set_visible_child_name("spinner");
                w.check.glyph.set_label("–");
                w.check.status.set_label("checking…");
            }
            Step::Adding => {
                w.check.mark.set_visible_child_name("glyph");
                w.check.glyph.set_label("✓");
                w.check.status.set_label("passed");
            }
            _ => {
                w.check.mark.set_visible_child_name("glyph");
                w.check.glyph.set_label("–");
                w.check.status.set_label("next");
            }
        }
        set_class(&w.check.glyph, "sw-setup-ok", step == Step::Adding);

        let message = problem
            .filter(|problem| {
                problem.field() != Field::Address || state.address_tried || step != Step::Address
            })
            .and_then(Problem::message)
            .filter(|_| !busy);
        w.validation.set_visible(message.is_some());
        w.validation_text.set_label(message.unwrap_or_default());
        let invalid = message.and(problem).map(Problem::field);
        for (field, entry) in [
            (Field::Address, &w.address),
            (Field::ImapHost, &w.imap.host),
            (Field::ImapPort, &w.imap.port),
            (Field::SmtpHost, &w.smtp.host),
            (Field::SmtpPort, &w.smtp.port),
        ] {
            let on = invalid == Some(field);
            set_class(entry, "sw-setup-invalid", on);
            entry.update_state(&[gtk::accessible::State::Invalid(if on {
                gtk::AccessibleInvalidState::True
            } else {
                gtk::AccessibleInvalidState::False
            })]);
        }

        w.column
            .set_sensitive(!matches!(step, Step::Checking | Step::Adding));
        w.manual_link.set_visible(matches!(
            step,
            Step::Address | Step::Detecting | Step::Password
        ));
        w.back.set_visible(matches!(
            step,
            Step::Microsoft | Step::Blocked | Step::Manual | Step::Failed | Step::Checking
        ));
        w.primary
            .set_visible(!matches!(step, Step::Microsoft | Step::Blocked));
        w.primary.set_label(match shown {
            Step::Address | Step::Detecting => "Next",
            Step::Password => "Sign in",
            _ => "Check settings",
        });
        w.primary.set_sensitive(!busy && problem.is_none());
        w.close.set_sensitive(step != Step::Adding);
    }
}

/// The title and the line under it for the screen on view.
fn heading(state: &State) -> (String, &'static str) {
    match state.step {
        Step::Detecting => ("Type your address".to_owned(), "Looking up your provider…"),
        Step::Checking => (
            heading_for(state, state.shown()).0,
            "Checking your login with the server…",
        ),
        Step::Adding => (heading_for(state, state.shown()).0, "Saving the account…"),
        step => heading_for(state, step),
    }
}

fn heading_for(state: &State, step: Step) -> (String, &'static str) {
    match step {
        Step::Microsoft => (
            format!(
                "{} account",
                state
                    .found()
                    .and_then(|settings| settings.provider.as_deref())
                    .unwrap_or("Microsoft")
            ),
            "",
        ),
        Step::Blocked => ("Can't add this address".to_owned(), ""),
        Step::Manual => (
            "Set up manually".to_owned(),
            if state.notice.is_some() {
                ""
            } else {
                "Copy the settings from your provider's help pages."
            },
        ),
        Step::Failed => ("Couldn't sign in".to_owned(), ""),
        _ => (
            "Type your address".to_owned(),
            "Sealwax finds the server settings for you.",
        ),
    }
}

fn show_link(link: &gtk::LinkButton, target: Option<&HelpLink>) {
    link.set_visible(target.is_some());
    if let Some(target) = target {
        link.set_label(&target.label);
        link.set_uri(&target.url);
    }
}

pub(super) fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_named(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}
