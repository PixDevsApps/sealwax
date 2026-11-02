//! The assistant's widgets (the mock's Setup artboard): the column, its sections and the
//! footer, built once and shown or hidden for the screen on view. Every class is a `sw-setup-*`
//! class of `style.css`; every part a self-test checks has a `setup-*` name.

use super::colors::ColorChoice;
use super::form::{Field, Naming, Problem};
use super::sources::{self, Sources, State as SourceState};
use super::text::{self, Tone};
use super::{CONTENT_HEIGHT, CONTENT_WIDTH, State, Step};
use crate::window::panes::{classed, icon, icon_button, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::account::{Endpoint, HelpLink, Protocol, Security};
use std::cell::RefCell;
use std::rc::Rc;

/// One server's manual fields.
pub(super) struct ServerFields {
    pub(super) host: gtk::Entry,
    pub(super) port: gtk::Entry,
    pub(super) security: gtk::DropDown,
    /// The host's caption, which names the protocol.
    host_caption: gtk::Label,
}

/// The host's caption and placeholder for a protocol.
fn server_texts(protocol: Protocol) -> (&'static str, &'static str) {
    match protocol {
        Protocol::Imap => ("IMAP server", "imap.example.test"),
        Protocol::Smtp => ("SMTP server", "smtp.example.test"),
        Protocol::Pop3 => ("POP3 server", "pop.example.test"),
    }
}

/// What a screen reader calls the port and the security fields, whose captions ("Port",
/// "Security") are the same for both servers.
fn field_names(protocol: Protocol) -> (&'static str, &'static str) {
    match protocol {
        Protocol::Imap => ("IMAP port", "IMAP security"),
        Protocol::Smtp => ("SMTP port", "SMTP security"),
        Protocol::Pop3 => ("POP3 port", "POP3 security"),
    }
}

impl ServerFields {
    /// Names the incoming server's protocol: the caption, the placeholder and the accessible
    /// label follow the "Incoming mail" choice.
    pub(super) fn name_protocol(&self, protocol: Protocol) {
        let (caption, placeholder) = server_texts(protocol);
        if self.host_caption.label() != caption {
            self.host_caption.set_label(caption);
            self.host.set_placeholder_text(Some(placeholder));
            self.host
                .update_property(&[gtk::accessible::Property::Label(caption)]);
            let (port, security) = field_names(protocol);
            self.port
                .update_property(&[gtk::accessible::Property::Label(port)]);
            self.security
                .update_property(&[gtk::accessible::Property::Label(security)]);
        }
    }

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
    caption: gtk::Label,
    pub(super) title: gtk::Label,
    pub(super) subtitle: gtk::Label,
    pub(super) alert: gtk::Box,
    pub(super) alert_icon: gtk::Image,
    pub(super) alert_title: gtk::Label,
    pub(super) alert_body: gtk::Label,
    pub(super) alert_link: gtk::LinkButton,
    pub(super) address_field: gtk::Box,
    pub(super) address: gtk::Entry,
    pub(super) name_field: gtk::Box,
    pub(super) name: gtk::Entry,
    /// What Sealwax calls the account; the address when left empty.
    label_field: gtk::Box,
    pub(super) label: gtk::Entry,
    color_field: gtk::Box,
    pub(super) colors: Rc<ColorChoice>,
    pub(super) sources: gtk::Box,
    pub(super) found: gtk::Box,
    pub(super) found_lines: [gtk::Label; 4],
    pub(super) found_note: gtk::Label,
    pub(super) microsoft: gtk::Box,
    pub(super) microsoft_button: gtk::Button,
    /// The Microsoft account signed in with isn't the address typed: which to send from.
    identity: gtk::Box,
    identity_text: gtk::Label,
    pub(super) send_from: gtk::Button,
    pub(super) keep_address: gtk::Button,
    /// A server on this computer showed a certificate the system doesn't trust (issue #9).
    certificate: gtk::Box,
    certificate_text: gtk::Label,
    fingerprint: gtk::Label,
    pub(super) trust: gtk::Button,
    pub(super) dont_trust: gtk::Button,
    microsoft_unavailable: gtk::Box,
    waiting: gtk::Box,
    /// After a minute: why Microsoft may never send the browser back.
    pub(super) still_waiting: gtk::Label,
    pub(super) open_again: gtk::Button,
    pub(super) cancel: gtk::Button,
    pub(super) username_field: gtk::Box,
    pub(super) username: gtk::Entry,
    pub(super) manual: gtk::Box,
    /// "Incoming mail": IMAP or POP3 (M8).
    pub(super) receive: gtk::DropDown,
    pop3_note: gtk::Label,
    /// The incoming server: IMAP's, or POP3's when chosen.
    pub(super) incoming: ServerFields,
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
pub(super) fn field(text: &str, control: &impl IsA<gtk::Widget>, name: &str) -> gtk::Box {
    captioned(text, control, name).0
}

/// [`field`] and its caption.
fn captioned(text: &str, control: &impl IsA<gtk::Widget>, name: &str) -> (gtk::Box, gtk::Label) {
    let caption = label(text, &["sw-setup-label"]);
    caption.set_mnemonic_widget(Some(control));
    control
        .upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(text)]);
    let field = named(column(&["sw-setup-field"]), name);
    field.append(&caption);
    field.append(control);
    (field, caption)
}

pub(super) fn entry(name: &str, placeholder: &str, classes: &[&str]) -> gtk::Entry {
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

/// A server's row. The incoming server's fields keep IMAP's widget names (`setup-imap-host`, …)
/// when POP3 is chosen.
fn server_fields(protocol: Protocol) -> (ServerFields, gtk::Box) {
    let key = match protocol {
        Protocol::Imap | Protocol::Pop3 => "imap",
        Protocol::Smtp => "smtp",
    };
    let (server, placeholder) = server_texts(protocol);
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
    // Their captions say only "Port" and "Security" for both servers, so a screen reader
    // hears the label property instead of the caption GTK ties them to.
    let (port_name, security_name) = field_names(protocol);
    for (control, name) in [
        (port.upcast_ref::<gtk::Widget>(), port_name),
        (security.upcast_ref(), security_name),
    ] {
        control.reset_relation(gtk::AccessibleRelation::LabelledBy);
        control.update_property(&[gtk::accessible::Property::Label(name)]);
    }
    let row = classed(
        gtk::Box::builder().homogeneous(true).build(),
        &["sw-setup-grid-row"],
    );
    let (host_field, host_caption) = captioned(server, &host, &format!("setup-{key}-host-field"));
    host_caption.set_widget_name(&format!("setup-{key}-host-label"));
    row.append(&host_field);
    row.append(&pair);
    (
        ServerFields {
            host,
            port,
            security,
            host_caption,
        },
        row,
    )
}

/// The manual step's "Incoming mail" choice (M8): IMAP, or POP3 with a note on what POP3 does.
fn receive_choice() -> (gtk::DropDown, gtk::Box, gtk::Label) {
    let choice = named(
        classed(
            gtk::DropDown::from_strings(&["IMAP", "POP3"]),
            &["sw-setup-security"],
        ),
        "setup-incoming",
    );
    // As wide as a server's name, the first of the grid's two halves.
    let row = classed(
        gtk::Box::builder().homogeneous(true).build(),
        &["sw-setup-grid-row"],
    );
    row.append(&field("Incoming mail", &choice, "setup-incoming-field"));
    row.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
    let note = named(
        label(text::POP3_NOTE, &["sw-setup-hint"]),
        "setup-pop3-note",
    );
    (choice, row, note)
}

fn link(name: &str) -> gtk::LinkButton {
    let link = named(gtk::LinkButton::new("about:blank"), name);
    link.set_halign(gtk::Align::Start);
    classed(link, &["sw-setup-link"])
}

/// The account name's placeholder before an address is typed.
const LABEL_PLACEHOLDER: &str = "Shown in the sidebar";

impl Widgets {
    /// The sender name, the account's name and its colour, as typed and chosen.
    pub(super) fn naming(&self) -> Naming {
        Naming {
            name: self.name.text().to_string(),
            label: self.label.text().to_string(),
            color: self.colors.chosen(),
        }
    }

    /// The incoming server's protocol the manual step's "Incoming mail" names.
    pub(super) fn receive_protocol(&self) -> Protocol {
        match self.receive.selected() {
            1 => Protocol::Pop3,
            _ => Protocol::Imap,
        }
    }

    /// Chooses IMAP or POP3 in "Incoming mail".
    pub(super) fn set_receive_protocol(&self, protocol: Protocol) {
        self.receive.set_selected(match protocol {
            Protocol::Pop3 => 1,
            Protocol::Imap | Protocol::Smtp => 0,
        });
    }

    /// The assistant's column and the dialog that holds it.
    pub(super) fn build() -> (Self, adw::Dialog) {
        let caption = named(
            label("Add an account", &["sw-setup-caption"]),
            "setup-caption",
        );
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
        let label_entry = entry("setup-label", LABEL_PLACEHOLDER, &[]);
        let label_field = field("Account name", &label_entry, "setup-label-field");
        let colors = ColorChoice::new("setup");
        let color_field = field("Colour", &colors.root, "setup-color-field");

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
                    .build(),
                &["sw-primary", "sw-setup-primary"],
            ),
            "setup-microsoft-button",
        );
        let microsoft_card = named(
            column(&["sw-setup-card", "sw-setup-microsoft"]),
            "setup-microsoft",
        );
        microsoft_card.append(&microsoft_title);
        microsoft_card.append(&label(
            "Your browser opens Microsoft's sign-in page. Sealwax never sees your password; it \
             keeps a sign-in token in your keyring.",
            &[],
        ));
        microsoft_card.append(&microsoft_button);

        // The mock's "Waiting for the browser…" box: Cancel goes back to the button, and the
        // page opens again for a tab closed too soon.
        let spinner = classed(adw::Spinner::new(), &["sw-setup-spinner"]);
        spinner
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Waiting")]);
        let waiting_label = named(
            label("Waiting for the browser…", &["sw-setup-hint"]),
            "setup-waiting-label",
        );
        waiting_label.set_hexpand(true);
        let cancel = named(
            classed(
                gtk::Button::builder()
                    .label("Cancel")
                    .valign(gtk::Align::Center)
                    .build(),
                &["sw-setup-chip"],
            ),
            "setup-cancel",
        );
        let waiting_row = classed(
            gtk::Box::new(gtk::Orientation::Horizontal, 0),
            &["sw-setup-waiting-row"],
        );
        waiting_row.append(&spinner);
        waiting_row.append(&waiting_label);
        waiting_row.append(&cancel);
        let still_waiting = named(
            label(text::STILL_WAITING, &["sw-setup-hint"]),
            "setup-still-waiting",
        );
        let open_again = named(
            classed(
                gtk::Button::builder()
                    .label("Open the sign-in page again")
                    .halign(gtk::Align::Start)
                    .build(),
                &["sw-setup-link"],
            ),
            "setup-open-again",
        );
        let waiting = named(
            column(&["sw-setup-note", "sw-setup-waiting"]),
            "setup-waiting",
        );
        waiting.append(&waiting_row);
        waiting.append(&still_waiting);
        waiting.append(&open_again);

        let microsoft_unavailable = named(
            column(&["sw-setup-note", "sw-setup-unavailable"]),
            "setup-microsoft-unavailable",
        );
        microsoft_unavailable.append(&named(
            label(text::MICROSOFT_UNAVAILABLE, &[]),
            "setup-microsoft-note",
        ));
        microsoft_unavailable.append(&named(
            label(text::MICROSOFT_CLIENT_ID_HINT, &["sw-setup-hint"]),
            "setup-microsoft-client-id",
        ));
        let microsoft_footnote = named(
            label(
                "Outlook.com and Microsoft 365 accept only Microsoft sign-in for mail apps.",
                &["sw-setup-footnote"],
            ),
            "setup-microsoft-footnote",
        );
        let microsoft_box = column(&["sw-setup-section"]);
        microsoft_box.append(&microsoft_card);
        microsoft_box.append(&waiting);
        microsoft_box.append(&microsoft_unavailable);
        microsoft_box.append(&microsoft_footnote);

        let identity_text = named(label("", &[]), "setup-identity-text");
        let choice = |name: &str, classes: &[&str]| {
            let text = gtk::Label::builder()
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            let button = named(
                classed(
                    gtk::Button::builder()
                        .child(&text)
                        .halign(gtk::Align::Start)
                        .build(),
                    classes,
                ),
                name,
            );
            button.set_hexpand(false);
            button
        };
        let send_from = choice("setup-send-from", &["sw-primary", "sw-setup-primary"]);
        let keep_address = choice("setup-keep-address", &["sw-setup-secondary"]);
        let identity = named(
            column(&["sw-setup-card", "sw-setup-microsoft"]),
            "setup-identity",
        );
        identity.append(&label(text::IDENTITY_TITLE, &["sw-setup-provider"]));
        identity.append(&identity_text);
        identity.append(&send_from);
        identity.append(&keep_address);

        let certificate_text = named(label("", &[]), "setup-certificate-text");
        let fingerprint = named(label("", &["sw-setup-fingerprint"]), "setup-fingerprint");
        fingerprint.set_selectable(true);
        fingerprint.set_wrap(false);
        let fingerprint_field = field(
            "SHA-256 fingerprint",
            &fingerprint,
            "setup-fingerprint-field",
        );
        let button = |text: &str, name: &str, classes: &[&str]| {
            let button = named(
                classed(
                    gtk::Button::builder()
                        .label(text)
                        .halign(gtk::Align::Start)
                        .build(),
                    classes,
                ),
                name,
            );
            button.set_hexpand(false);
            button
        };
        let trust = button(
            text::TRUST,
            "setup-trust",
            &["sw-primary", "sw-setup-primary"],
        );
        let dont_trust = button(
            text::DONT_TRUST,
            "setup-dont-trust",
            &["sw-setup-secondary"],
        );
        let certificate = named(
            column(&["sw-setup-card", "sw-setup-microsoft"]),
            "setup-certificate",
        );
        certificate.append(&label(text::CERTIFICATE_TITLE, &["sw-setup-provider"]));
        certificate.append(&certificate_text);
        certificate.append(&fingerprint_field);
        certificate.append(&trust);
        certificate.append(&dont_trust);

        let username = entry("setup-username", "", &[]);
        let username_field = field("Username", &username, "setup-username-field");

        let (receive, receive_row, pop3_note) = receive_choice();
        let (incoming, incoming_row) = server_fields(Protocol::Imap);
        let (smtp, smtp_row) = server_fields(Protocol::Smtp);
        let manual_username = entry("setup-manual-username", "", &["sw-setup-small"]);
        let manual = named(column(&["sw-setup-section"]), "setup-manual");
        manual.append(&label("Set up manually", &["sw-setup-caption"]));
        manual.append(&receive_row);
        manual.append(&pop3_note);
        manual.append(&incoming_row);
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
            label_field.upcast_ref(),
            color_field.upcast_ref(),
            sources.upcast_ref(),
            found.upcast_ref(),
            microsoft_box.upcast_ref(),
            identity.upcast_ref(),
            certificate.upcast_ref(),
            username_field.upcast_ref(),
            manual.upcast_ref(),
            password_field.upcast_ref(),
            check.root.upcast_ref(),
            validation.upcast_ref(),
        ] {
            content.append(part);
        }
        // Not a Tab stop of its own while every field waits (checking the login): the focus
        // would land on an unnamed pane.
        let scroller = classed(
            gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&content)
                .build(),
            &["sw-setup-scroller"],
        );
        scroller.vscrollbar().set_widget_name("setup-scrollbar");

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
        sheet.append(&crate::accessibility::tab_through(&scroller));
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
            caption,
            title,
            subtitle,
            alert,
            alert_icon,
            alert_title,
            alert_body,
            alert_link,
            address_field,
            address,
            name_field,
            name,
            label_field,
            label: label_entry,
            color_field,
            colors,
            sources,
            found,
            found_lines,
            found_note,
            microsoft: microsoft_box,
            microsoft_button,
            identity,
            identity_text,
            certificate,
            certificate_text,
            fingerprint,
            trust,
            dont_trust,
            send_from,
            keep_address,
            microsoft_unavailable,
            waiting,
            still_waiting,
            open_again,
            cancel,
            username_field,
            username,
            manual,
            receive,
            pop3_note,
            incoming,
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
    /// can be pressed (`problem`). `microsoft`: a client ID is in use, so Microsoft sign-in can
    /// start.
    pub(super) fn draw(
        &self,
        state: &State,
        problem: Option<Problem>,
        drawn_sources: &RefCell<Option<Sources>>,
        microsoft: bool,
    ) {
        let w = self;
        let step = state.step;
        let shown = state.shown();
        let busy = step.is_busy();
        let found = state.found();
        let again = state.again.is_some();
        let domain = state
            .address
            .as_ref()
            .map(|address| address.domain.as_str())
            .unwrap_or_default();

        w.caption.set_label(if again {
            "Sign in again"
        } else {
            "Add an account"
        });
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
                | Step::PasswordAgain
        ));
        w.name_field
            .set_visible(matches!(shown, Step::Address | Step::Manual));
        // Every way to the manual settings passes the first step, which keeps what it chose;
        // shown there too, they would push the manual step's message out of view.
        w.label_field.set_visible(shown == Step::Address);
        w.color_field.set_visible(shown == Step::Address);
        // The placeholder shows what an empty name gives: the address.
        let typed = w.address.text();
        w.label.set_placeholder_text(Some(match typed.trim() {
            "" => LABEL_PLACEHOLDER,
            address => address,
        }));

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
        w.identity.set_visible(shown == Step::Identity);
        if let (Some(signed_in), Some(typed)) = (state.signed_in_as(), state.typed_address()) {
            w.identity_text
                .set_label(&text::identity_body(signed_in, typed));
            for (button, text) in [
                (&w.send_from, text::send_from(signed_in)),
                (&w.keep_address, text::keep_address(typed)),
            ] {
                if let Some(child) = button.child().and_downcast::<gtk::Label>() {
                    child.set_label(&text);
                }
                button.set_tooltip_text(Some(&text));
                button.update_property(&[gtk::accessible::Property::Label(&text)]);
            }
        }
        w.certificate.set_visible(shown == Step::Certificate);
        if let Some(untrusted) = &state.untrusted {
            w.certificate_text.set_label(&text::certificate_body(
                untrusted.protocol,
                &untrusted.endpoint,
            ));
            w.fingerprint
                .set_label(&text::fingerprint_lines(&untrusted.pin));
        }
        w.microsoft_button
            .set_tooltip_text((!microsoft).then_some("Not available in this build yet"));
        w.microsoft_unavailable.set_visible(!microsoft);
        w.waiting.set_visible(step == Step::SigningIn);
        // Cancel keeps the focus while the browser signs in, so it carries the note as its
        // description too.
        let still_waiting = step == Step::SigningIn && state.still_waiting;
        w.still_waiting.set_visible(still_waiting);
        if still_waiting {
            w.cancel
                .update_property(&[gtk::accessible::Property::Description(text::STILL_WAITING)]);
        } else {
            w.cancel
                .reset_property(gtk::AccessibleProperty::Description);
        }
        w.open_again
            .set_visible(step == Step::SigningIn && state.sign_in_url.is_some());

        w.manual
            .set_visible(matches!(shown, Step::Manual | Step::Failed));
        w.password_field.set_visible(matches!(
            shown,
            Step::Password | Step::Manual | Step::Failed | Step::PasswordAgain
        ));
        let app_password = state.app_password();
        let enable_imap = state.enable_imap();
        let provider = state.provider();
        let typed_here = matches!(shown, Step::Password | Step::PasswordAgain);
        let bridge = state.bridge();
        w.password_label.set_label(if bridge {
            text::BRIDGE_PASSWORD
        } else if app_password.is_some() {
            "App password"
        } else {
            "Password"
        });
        w.password
            .update_property(&[gtk::accessible::Property::Label(
                w.password_label.label().as_str(),
            )]);
        let hints: Vec<String> = [
            app_password.map(|_| {
                if bridge {
                    text::bridge_password_hint(provider)
                } else {
                    text::app_password_hint(provider)
                }
            }),
            enable_imap.map(|_| text::enable_imap_hint(provider)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let hinted = typed_here && !hints.is_empty();
        w.password_hint.set_visible(hinted);
        w.password_hint.set_label(&hints.join(" "));
        show_link(&w.app_password_link, app_password.filter(|_| typed_here));
        show_link(&w.enable_imap_link, enable_imap.filter(|_| typed_here));

        w.check
            .root
            .set_visible(typed_here || matches!(step, Step::Checking | Step::Adding));
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

        let incoming = w.receive_protocol();
        w.incoming.name_protocol(incoming);
        w.pop3_note.set_visible(incoming == Protocol::Pop3);
        let message = problem
            .filter(|problem| {
                problem.field() != Field::Address || state.address_tried || step != Step::Address
            })
            .and_then(|problem| problem.message(incoming))
            .filter(|_| !busy);
        w.validation.set_visible(message.is_some());
        w.validation_text.set_label(message.unwrap_or_default());
        let invalid = message.and(problem).map(Problem::field);
        for (field, entry) in [
            (Field::Address, &w.address),
            (Field::IncomingHost, &w.incoming.host),
            (Field::IncomingPort, &w.incoming.port),
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

        w.manual_link.set_visible(matches!(
            step,
            Step::Address | Step::Detecting | Step::Password
        ));
        w.back.set_visible(match step {
            Step::Microsoft | Step::PasswordAgain => !again,
            step => matches!(
                step,
                Step::Blocked
                    | Step::Manual
                    | Step::Failed
                    | Step::Checking
                    | Step::Identity
                    | Step::Certificate
            ),
        });
        // While it checks or signs in, the fields and the sign-in button wait; the status row
        // under them, the sign-in card's text and the waiting box with its Cancel stay
        // readable.
        let frozen = matches!(step, Step::Checking | Step::Adding | Step::SigningIn);
        let fields: [&gtk::Widget; 9] = [
            w.address_field.upcast_ref(),
            w.name_field.upcast_ref(),
            w.label_field.upcast_ref(),
            w.color_field.upcast_ref(),
            w.sources.upcast_ref(),
            w.found.upcast_ref(),
            w.username_field.upcast_ref(),
            w.manual.upcast_ref(),
            w.password_field.upcast_ref(),
        ];
        // A text field that turns insensitive with the focus misses its focus-out (GTK warns
        // when its cursor blinks next), so Back, the one way out while it checks, takes it
        // first; while the browser signs in, Cancel does.
        if step == Step::SigningIn {
            if w.microsoft_button.is_sensitive() {
                w.cancel.grab_focus();
            }
        } else if frozen && w.password_field.is_sensitive() {
            w.back.grab_focus();
        }
        for field in fields {
            field.set_sensitive(!frozen);
        }
        w.microsoft_button.set_sensitive(microsoft && !frozen);
        // An account signed in again keeps its address.
        w.address_field.set_sensitive(!frozen && !again);
        w.primary.set_visible(!matches!(
            shown,
            Step::Microsoft | Step::Blocked | Step::Identity | Step::Certificate
        ));
        w.primary.set_label(match shown {
            Step::Address | Step::Detecting => "Next",
            Step::Password | Step::PasswordAgain => "Sign in",
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
        Step::SigningIn => heading_for(state, Step::Microsoft),
        step => heading_for(state, step),
    }
}

fn heading_for(state: &State, step: Step) -> (String, &'static str) {
    match step {
        Step::Microsoft => match &state.again {
            Some(again) => (
                again.name.clone(),
                "Sealwax keeps your mail; nothing syncs until you sign in.",
            ),
            None => (
                format!(
                    "{} account",
                    state
                        .found()
                        .and_then(|settings| settings.provider.as_deref())
                        .unwrap_or("Microsoft")
                ),
                "",
            ),
        },
        Step::PasswordAgain => (
            state
                .again
                .as_ref()
                .map(|again| again.name.clone())
                .unwrap_or_default(),
            "The server stopped accepting the password Sealwax has. Type it again; nothing \
             syncs until you do.",
        ),
        Step::Blocked => ("Can't add this address".to_owned(), ""),
        Step::Identity => ("Check the sending address".to_owned(), ""),
        Step::Certificate => ("A server on this computer".to_owned(), ""),
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
