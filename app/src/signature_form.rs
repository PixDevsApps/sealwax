//! Signature… (1.5.0, issue #6): a form for an account's designed HTML signature, from the
//! account heading's menu in the sidebar or the palette. It asks for a name, a title, a company,
//! an address, a phone number, an email address, a website, social links and a picture (a
//! photo, shown round, or a logo), and shows the signature as it will be sent
//! (`infrastructure::signatures::designed_html`) in a web view that loads nothing. On Save it
//! writes the signature into the signatures folder (`save_template`) and sets `signature_html`
//! for the address in `accounts.toml`, which Sealwax reads again by itself. An account with
//! identities offers each address. A signature written by hand is never overwritten: the form
//! writes its own file and says so first. Remove Signature takes the `signature_html` line out
//! and leaves the files. Every file is read and written off the GTK thread.

use crate::application::AppState;
use gtk4 as gtk;
use gtk4::gdk_pixbuf::{InterpType, Pixbuf};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::{AccountKey, AccountSettings};
use sealwax_infrastructure::signatures::{self, LogoChange, SOCIAL, Template};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use webkit6::prelude::*;

/// The preview's policy: its own images as `data:` and inline styles, nothing else.
const PREVIEW_POLICY: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'";

/// The picture's size when it is saved: a photo is cropped square to this, a logo fits in it
/// twice as wide; twice what the signature shows, for sharp screens.
const PHOTO_SIZE: i32 = 192;

thread_local! {
    /// The form open now: its window's handlers hold it weakly, so this keeps it until the
    /// window closes.
    static OPEN: RefCell<Option<Rc<Form>>> = const { RefCell::new(None) };
}

/// One address the form can make a signature for.
#[derive(Clone, Debug)]
struct Address {
    email: String,
    /// The sender name it goes out with.
    name: String,
    /// Its `signature_html` now.
    current: Option<String>,
}

/// The picture as the form holds it.
#[derive(Clone, Default)]
enum Picture {
    /// The saved one, if any.
    #[default]
    Saved,
    New(Vec<u8>),
    Removed,
}

struct Rows {
    name: adw::EntryRow,
    title: adw::EntryRow,
    company: adw::EntryRow,
    address: adw::EntryRow,
    phone: adw::EntryRow,
    email: adw::EntryRow,
    website: adw::EntryRow,
    social: Vec<(&'static str, adw::EntryRow)>,
}

struct Form {
    state: std::rc::Weak<AppState>,
    key: AccountKey,
    addresses: Vec<Address>,
    chosen: Cell<usize>,
    dialog: adw::Dialog,
    notice: gtk::Label,
    rows: Rows,
    round: adw::SwitchRow,
    picture_row: adw::ActionRow,
    picture_remove: gtk::Button,
    remove: gtk::Button,
    preview: webkit6::WebView,
    picture: RefCell<Picture>,
    /// The saved picture's bytes, while it is kept.
    saved_picture: RefCell<Option<Vec<u8>>>,
    /// Bumped when another address is chosen, so a slow read for the last one is dropped.
    generation: Cell<u64>,
    /// While the form fills itself in, changes don't redraw the preview.
    filling: Cell<bool>,
}

/// The accounts the form can open for: those `accounts.toml` lists.
pub(crate) fn applies(state: &AppState) -> bool {
    state
        .preferences()
        .is_some_and(|prefs| !prefs.accounts().is_empty())
}

/// Opens the form for the account `key`, or the first in `accounts.toml` when it isn't there.
pub(crate) fn present(state: &Rc<AppState>, key: Option<&str>) {
    let Some(window) = state.window() else {
        return;
    };
    let accounts = state
        .preferences()
        .map(|prefs| prefs.accounts())
        .unwrap_or_default();
    let Some(account) = key
        .and_then(|key| accounts.iter().find(|account| account.key.as_str() == key))
        .or_else(|| accounts.first())
        .cloned()
    else {
        return;
    };
    let form = Form::new(state, &account);
    form.dialog.connect_closed(|_| {
        OPEN.with(|open| open.replace(None));
    });
    OPEN.with(|open| open.replace(Some(form.clone())));
    form.dialog.present(Some(window.window()));
    form.load();
}

fn addresses(account: &AccountSettings) -> Vec<Address> {
    sealwax_domain::compose::identity::senders(account)
        .into_iter()
        .map(|identity| Address {
            email: identity.address,
            name: identity.name,
            current: identity.signature_html,
        })
        .collect()
}

/// `data` as the signature saves it: a photo cropped square around its middle and scaled to
/// [`PHOTO_SIZE`], a logo scaled to fit [`PHOTO_SIZE`] high and three times as wide, both PNG;
/// `None` for an image GdkPixbuf can't read.
fn prepared(data: &[u8], round: bool) -> Option<Vec<u8>> {
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(data));
    let pixbuf = Pixbuf::from_stream(&stream, gio::Cancellable::NONE).ok()?;
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let scaled = if round {
        let side = width.min(height);
        let square = pixbuf.new_subpixbuf((width - side) / 2, (height - side) / 2, side, side);
        square.scale_simple(PHOTO_SIZE, PHOTO_SIZE, InterpType::Bilinear)?
    } else {
        let scale = (f64::from(PHOTO_SIZE) / f64::from(height))
            .min(f64::from(PHOTO_SIZE * 3) / f64::from(width))
            .min(1.0);
        let (w, h) = (
            ((f64::from(width) * scale).round() as i32).max(1),
            ((f64::from(height) * scale).round() as i32).max(1),
        );
        pixbuf.scale_simple(w, h, InterpType::Bilinear)?
    };
    scaled.save_to_bufferv("png", &[]).ok()
}

fn data_uri(mime_type: &str, data: &[u8]) -> String {
    format!(
        "data:{mime_type};base64,{}",
        glib::base64_encode(data).as_str()
    )
}

impl Form {
    fn new(state: &Rc<AppState>, account: &AccountSettings) -> Rc<Self> {
        let addresses = addresses(account);
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::builder()
            .label("Save")
            .css_classes(["suggested-action"])
            .build();
        save.set_widget_name("signature-save");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();

        let preview = Self::preview_view(state);
        let frame = gtk::Frame::builder()
            .child(&preview)
            .css_classes(["sw-signature-preview"])
            .build();
        let preview_group = adw::PreferencesGroup::builder()
            .title("Preview")
            .description("As it is sent, in light and in dark mail, under the “-- ” line")
            .build();
        preview_group.add(&frame);
        content.append(&preview_group);

        let from = adw::ComboRow::builder().title("From").build();
        let labels: Vec<String> = addresses
            .iter()
            .map(|address| format!("{} · {}", address.name, address.email))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        from.set_model(Some(&gtk::StringList::new(&labels)));
        if addresses.len() > 1 {
            let group = adw::PreferencesGroup::new();
            group.add(&from);
            content.append(&group);
        }
        let notice = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .css_classes(["sw-secondary"])
            .build();
        notice.set_widget_name("signature-notice");
        content.append(&notice);

        let entry = |title: &str, name: &str| {
            let row = adw::EntryRow::builder().title(title).build();
            row.set_widget_name(name);
            row.set_max_length(signatures::FIELD_MAX as i32);
            row
        };
        let rows = Rows {
            name: entry("Name", "signature-name"),
            title: entry("Title", "signature-title"),
            company: entry("Company", "signature-company"),
            address: entry("Address", "signature-address"),
            phone: entry("Phone", "signature-phone"),
            email: entry("Email", "signature-email"),
            website: entry("Website", "signature-website"),
            social: SOCIAL
                .iter()
                .map(|(key, label)| {
                    let row = entry(label, &format!("signature-{key}"));
                    row.set_input_purpose(gtk::InputPurpose::Url);
                    (*key, row)
                })
                .collect(),
        };
        rows.email.set_input_purpose(gtk::InputPurpose::Email);
        rows.website.set_input_purpose(gtk::InputPurpose::Url);
        rows.phone.set_input_purpose(gtk::InputPurpose::Phone);
        let you = adw::PreferencesGroup::builder().title("You").build();
        for row in [&rows.name, &rows.title, &rows.company] {
            you.add(row);
        }
        content.append(&you);

        let picture_row = adw::ActionRow::builder()
            .title("Photo or logo")
            .subtitle("A photo or a logo: PNG, JPEG or GIF, up to 1 MB")
            .build();
        let choose = gtk::Button::builder()
            .label("Choose…")
            .valign(gtk::Align::Center)
            .build();
        choose.set_widget_name("signature-picture-choose");
        let picture_remove = gtk::Button::builder()
            .label("Remove")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .visible(false)
            .build();
        picture_row.add_suffix(&picture_remove);
        picture_row.add_suffix(&choose);
        let round = adw::SwitchRow::builder()
            .title("Round")
            .subtitle("For a photo of you; a logo stays as it is")
            .active(true)
            .build();
        let look = adw::PreferencesGroup::builder().title("Picture").build();
        look.add(&picture_row);
        look.add(&round);
        content.append(&look);

        let contact = adw::PreferencesGroup::builder().title("Contact").build();
        for row in [&rows.address, &rows.phone, &rows.email, &rows.website] {
            contact.add(row);
        }
        content.append(&contact);
        let social = adw::PreferencesGroup::builder()
            .title("Social")
            .description("Web addresses; each shows as its icon, a link to it")
            .build();
        for (_, row) in &rows.social {
            social.add(row);
        }
        content.append(&social);

        let remove = gtk::Button::builder()
            .label("Remove Signature")
            .halign(gtk::Align::Start)
            .css_classes(["destructive-action"])
            .visible(false)
            .build();
        remove.set_widget_name("signature-remove");
        content.append(&remove);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&content)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&scrolled));
        let dialog = adw::Dialog::builder()
            .title(format!("Signature for {}", account.title()))
            .content_width(720)
            .content_height(760)
            .child(&view)
            .focus_widget(&rows.name)
            .build();
        dialog.set_widget_name("signature-form");

        let form = Rc::new(Self {
            state: Rc::downgrade(state),
            key: account.key.clone(),
            addresses,
            chosen: Cell::new(0),
            dialog,
            notice,
            rows,
            round,
            picture_row,
            picture_remove: picture_remove.clone(),
            remove: remove.clone(),
            preview,
            picture: RefCell::new(Picture::Saved),
            saved_picture: RefCell::new(None),
            generation: Cell::new(0),
            filling: Cell::new(false),
        });
        cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            form.dialog,
            move |_| {
                dialog.close();
            }
        ));
        let weak = Rc::downgrade(&form);
        save.connect_clicked(move |_| {
            if let Some(form) = weak.upgrade() {
                form.save();
            }
        });
        let weak = Rc::downgrade(&form);
        remove.connect_clicked(move |_| {
            if let Some(form) = weak.upgrade() {
                form.remove_signature();
            }
        });
        let weak = Rc::downgrade(&form);
        choose.connect_clicked(move |_| {
            if let Some(form) = weak.upgrade() {
                form.choose_picture();
            }
        });
        let weak = Rc::downgrade(&form);
        picture_remove.connect_clicked(move |_| {
            if let Some(form) = weak.upgrade() {
                form.picture.replace(Picture::Removed);
                form.show_preview();
            }
        });
        let weak = Rc::downgrade(&form);
        from.connect_selected_notify(move |row| {
            if let Some(form) = weak.upgrade() {
                form.chosen.set(row.selected() as usize);
                form.load();
            }
        });
        let weak = Rc::downgrade(&form);
        form.round.connect_active_notify(move |_| {
            if let Some(form) = weak.upgrade() {
                form.show_preview();
            }
        });
        for row in form.entries() {
            let weak = Rc::downgrade(&form);
            row.connect_changed(move |_| {
                if let Some(form) = weak.upgrade() {
                    form.show_preview();
                }
            });
        }
        form
    }

    /// The preview: a web view with the reader's hardening, no script, nothing loaded but its
    /// own `data:` images, every navigation refused.
    fn preview_view(state: &AppState) -> webkit6::WebView {
        let fonts = state.theme().map_or_else(
            || {
                sealwax_ui_model::theme::Theme::new(
                    &sealwax_ui_model::theme::ThemeInputs::builtin(
                        sealwax_ui_model::theme::Mode::Light,
                    ),
                    &sealwax_ui_model::theme::ThemeOptions::default(),
                )
                .webview_fonts()
            },
            |theme| theme.theme().webview_fonts(),
        );
        let settings = crate::webview::hardened_settings(&fonts);
        settings.set_enable_javascript(false);
        let (context, session) = crate::webview::ephemeral_context(|| {});
        let view = webkit6::WebView::builder()
            .settings(&settings)
            .web_context(&context)
            .network_session(&session)
            .default_content_security_policy(PREVIEW_POLICY)
            .height_request(300)
            .hexpand(true)
            .build();
        view.connect_decide_policy(|_, decision, kind| {
            let ours = kind == webkit6::PolicyDecisionType::NavigationAction
                && decision
                    .downcast_ref::<webkit6::NavigationPolicyDecision>()
                    .and_then(|decision| decision.navigation_action())
                    .and_then(|action| action.request())
                    .and_then(|request| request.uri())
                    .is_some_and(|uri| uri == "about:blank");
            if ours || kind == webkit6::PolicyDecisionType::Response {
                decision.use_();
            } else {
                decision.ignore();
            }
            true
        });
        view.connect_create(|_, _| None);
        view.set_widget_name("signature-preview");
        view.update_property(&[gtk::accessible::Property::Label("Signature preview")]);
        view
    }

    fn entries(&self) -> Vec<adw::EntryRow> {
        let rows = &self.rows;
        let mut entries = vec![
            rows.name.clone(),
            rows.title.clone(),
            rows.company.clone(),
            rows.address.clone(),
            rows.phone.clone(),
            rows.email.clone(),
            rows.website.clone(),
        ];
        entries.extend(rows.social.iter().map(|(_, row)| row.clone()));
        entries
    }

    fn address(&self) -> Option<Address> {
        self.addresses.get(self.chosen.get()).cloned()
    }

    fn dir(&self) -> Option<PathBuf> {
        let state = self.state.upgrade()?;
        Some(signatures::dir(&state.config().dirs.config))
    }

    /// Fills the form with the chosen address's signature, read off the GTK thread.
    fn load(self: &Rc<Self>) {
        let (Some(address), Some(dir)) = (self.address(), self.dir()) else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let current = address.current.clone();
        let key = self.key.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking(move || {
                let template = current
                    .as_deref()
                    .and_then(|path| signatures::read_template(&dir, path));
                let picture = template
                    .as_ref()
                    .and_then(|template| template.logo.as_ref())
                    .and_then(|logo| std::fs::read(dir.join(key.as_str()).join(logo)).ok());
                (template, picture)
            })
            .await
            .unwrap_or_default();
            let Some(form) = weak.upgrade() else {
                return;
            };
            if form.generation.get() != generation {
                return;
            }
            let (template, picture) = read;
            let hand_made = address.current.is_some() && template.is_none();
            let template = template.unwrap_or_else(|| Template {
                name: address.name.clone(),
                email: address.email.clone(),
                round: true,
                ..Template::default()
            });
            form.fill(&template);
            form.saved_picture.replace(picture);
            form.picture.replace(Picture::Saved);
            form.notice.set_visible(hand_made);
            if let Some(path) = address.current.as_deref().filter(|_| hand_made) {
                form.notice.set_label(&format!(
                    "{} uses your own file, “{path}”. Save makes a new signature from this form \
                     and uses it instead; your file stays as it is.",
                    address.email
                ));
            }
            form.remove.set_visible(address.current.is_some());
            form.show_preview();
        });
    }

    fn fill(&self, template: &Template) {
        self.filling.set(true);
        let rows = &self.rows;
        rows.name.set_text(&template.name);
        rows.title.set_text(&template.title);
        rows.company.set_text(&template.company);
        rows.address.set_text(&template.address);
        rows.phone.set_text(&template.phone);
        rows.email.set_text(&template.email);
        rows.website.set_text(&template.website);
        for (key, row) in &rows.social {
            let url = template
                .social
                .iter()
                .find(|(known, _)| known == key)
                .map_or("", |(_, url)| url.as_str());
            row.set_text(url);
        }
        self.round.set_active(template.round);
        self.filling.set(false);
    }

    /// The picture the form would save, as chosen.
    fn picture_bytes(&self) -> Option<Vec<u8>> {
        match &*self.picture.borrow() {
            Picture::Saved => self.saved_picture.borrow().clone(),
            Picture::New(data) => Some(data.clone()),
            Picture::Removed => None,
        }
    }

    fn template(&self) -> Template {
        let rows = &self.rows;
        Template {
            name: rows.name.text().to_string(),
            title: rows.title.text().to_string(),
            company: rows.company.text().to_string(),
            address: rows.address.text().to_string(),
            phone: rows.phone.text().to_string(),
            email: rows.email.text().to_string(),
            website: rows.website.text().to_string(),
            social: rows
                .social
                .iter()
                .map(|(key, row)| ((*key).to_owned(), row.text().to_string()))
                .filter(|(_, url)| !url.trim().is_empty())
                .collect(),
            round: self.round.is_active(),
            logo: None,
        }
    }

    /// Draws the signature as it will be sent.
    fn show_preview(&self) {
        if self.filling.get() {
            return;
        }
        let picture = self
            .picture_bytes()
            .and_then(|data| prepared(&data, self.round.is_active()));
        self.picture_remove.set_visible(picture.is_some());
        let mut template = self.template();
        template.logo = picture.as_ref().map(|_| "picture".to_owned());
        let html = signatures::designed_html(&template, &|name| {
            if name == "picture" {
                picture
                    .as_deref()
                    .map(|data| data_uri("image/png", data))
                    .unwrap_or_default()
            } else {
                signatures::ICONS
                    .iter()
                    .find(|(icon, _)| signatures::icon_file(icon) == name)
                    .map(|(_, data)| data_uri("image/png", data))
                    .unwrap_or_default()
            }
        });
        // Twice: as a light mail client shows it and as a dark one, each with its own text and
        // link colours, since the signature sets no colour but its grey.
        let shown = |class: &str, background: &str, text: &str| {
            format!(
                "<div class=\"{class}\" style=\"background:{background};color:{text};\
                 padding:16px 18px\"><div>--&nbsp;</div><div style=\"margin-top:6px\">{html}\
                 </div></div>"
            )
        };
        let document = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>body{{margin:0}} \
             .light a{{color:#1a5fb4}} .dark a{{color:#99c1f1}}</style></head><body>{}{}</body>\
             </html>",
            shown("light", "#ffffff", "#222222"),
            shown("dark", "#1e1e1e", "#e6e6e6")
        );
        self.preview.load_html(&document, None);
    }

    fn choose_picture(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("PNG, JPEG or GIF"));
        for mime in ["image/png", "image/jpeg", "image/gif"] {
            filter.add_mime_type(mime);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title("Choose a Picture")
            .filters(&filters)
            .default_filter(&filter)
            .modal(true)
            .build();
        let parent = self
            .dialog
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(file) = chooser.open_future(parent.as_ref()).await else {
                return;
            };
            let loaded = file.load_contents_future().await;
            let Some(form) = weak.upgrade() else {
                return;
            };
            let subtitle = match loaded {
                Ok((data, _)) => match signatures::check_logo(&data) {
                    Ok(_) if prepared(&data, false).is_some() => {
                        form.picture.replace(Picture::New(data.to_vec()));
                        form.show_preview();
                        "A photo or a logo: PNG, JPEG or GIF, up to 1 MB"
                    }
                    Err(signatures::ImageFault::TooLarge) => {
                        "That image is larger than 1 MB; choose a smaller one"
                    }
                    _ => "That file isn't a PNG, JPEG or GIF; choose another",
                },
                Err(_) => "That file can't be read",
            };
            form.picture_row.set_subtitle(subtitle);
        });
    }

    fn save(self: &Rc<Self>) {
        let (Some(state), Some(address), Some(dir)) =
            (self.state.upgrade(), self.address(), self.dir())
        else {
            return;
        };
        let template = self.template();
        let picture = self
            .picture_bytes()
            .and_then(|data| prepared(&data, template.round));
        let empty = [
            &template.name,
            &template.title,
            &template.company,
            &template.address,
            &template.phone,
            &template.email,
            &template.website,
        ]
        .iter()
        .all(|text| text.trim().is_empty())
            && template.social.is_empty()
            && picture.is_none();
        if empty {
            self.notice
                .set_label("Fill in at least one detail, or add a picture, to save a signature.");
            self.notice.set_visible(true);
            return;
        }
        let picture = match picture {
            Some(data) => LogoChange::Replace(data),
            None => LogoChange::Remove,
        };
        let accounts_file = state.config().dirs.accounts_file();
        let key = self.key.clone();
        let email = address.email.clone();
        self.dialog.close();
        let weak = Rc::downgrade(&state);
        glib::spawn_future_local(async move {
            let saved = gio::spawn_blocking(move || {
                let path = signatures::save_template(&dir, &key, &email, &template, picture)
                    .map_err(|error| error.to_string())?;
                sealwax_infrastructure::accounts::set_signature_html(
                    &accounts_file,
                    &key,
                    &email,
                    Some(&path),
                )
                .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or_else(|_| Err("the signature couldn't be saved".to_owned()));
            let Some(state) = weak.upgrade() else {
                return;
            };
            let text = match saved {
                Ok(_) => format!(
                    "Signature saved: new messages from {} have it",
                    address.email
                ),
                Err(why) => format!("The signature couldn't be saved: {why}"),
            };
            state.show_toast(&adw::Toast::new(&text), false);
        });
    }

    fn remove_signature(self: &Rc<Self>) {
        let (Some(state), Some(address)) = (self.state.upgrade(), self.address()) else {
            return;
        };
        let accounts_file = state.config().dirs.accounts_file();
        let key = self.key.clone();
        let email = address.email.clone();
        self.dialog.close();
        let weak = Rc::downgrade(&state);
        glib::spawn_future_local(async move {
            let removed = gio::spawn_blocking(move || {
                sealwax_infrastructure::accounts::set_signature_html(
                    &accounts_file,
                    &key,
                    &email,
                    None,
                )
                .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or_else(|_| Err("the signature couldn't be removed".to_owned()));
            let Some(state) = weak.upgrade() else {
                return;
            };
            let text = match removed {
                Ok(_) => format!(
                    "Signature removed from {}; its files stay in the signatures folder",
                    address.email
                ),
                Err(why) => format!("The signature couldn't be removed: {why}"),
            };
            state.show_toast(&adw::Toast::new(&text), false);
        });
    }
}
