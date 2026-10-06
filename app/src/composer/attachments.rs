//! The footer's attachments (ML-072, C4): a card for each file with its type badge, the name it
//! is sent under (made safe by `domain::attachment`), its size and × to remove it. A file is
//! read, typed and written into the blob store on a thread of its own, so a large one never
//! freezes the window; its card says "Adding…" until then. The total encoded size is checked
//! against the account's submission limit.

use super::backend::Local;
use crate::window::panes::{classed, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_domain::attachment;
use sealwax_domain::compose::DraftPart;
use sealwax_infrastructure::blobs::BlobStore;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// Bytes a message needs for `size` bytes of attachment: base64 in 76-character lines.
pub fn encoded(size: u64) -> u64 {
    let base64 = size.div_ceil(3) * 4;
    base64 + base64.div_ceil(76) * 2
}

/// Headers, MIME boundaries and part headers, roughly, on top of the parts and the body.
const OVERHEAD: u64 = 4 * 1024;

/// The message's size as sent: the parts base64-encoded, the HTML body and its plain-text
/// alternative (quoted-printable, at most about the HTML's length each), and the headers.
pub fn message_size(parts: impl IntoIterator<Item = u64>, html_len: usize) -> u64 {
    parts.into_iter().map(encoded).sum::<u64>() + 2 * html_len as u64 + OVERHEAD
}

/// What a file became: a part of the draft, or why it couldn't be added.
pub type Added = Result<DraftPart, String>;

/// Reads `path`, finds its type and stores it; runs off the GTK thread.
fn store_file(blobs: &BlobStore, path: &std::path::Path) -> Added {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|error| error.kind().to_string())?;
    store_bytes(blobs, &name, None, bytes)
}

/// Stores `bytes` as a part named `name`; the type is `mime_type` or guessed.
fn store_bytes(blobs: &BlobStore, name: &str, mime_type: Option<&str>, bytes: Vec<u8>) -> Added {
    let mime_type = match mime_type.filter(|mime| mime.contains('/')) {
        Some(mime) => mime.to_ascii_lowercase(),
        None => {
            let sample = &bytes[..bytes.len().min(4096)];
            let (content_type, _) = gio::content_type_guess(Some(name), Some(sample));
            gio::content_type_get_mime_type(&content_type)
                .map(|mime| mime.to_string())
                .unwrap_or_else(|| "application/octet-stream".to_owned())
        }
    };
    let blob = blobs
        .put(&bytes)
        .map_err(|error| error.kind().to_string())?;
    Ok(DraftPart {
        name: attachment::safe_file_name(Some(name), &mime_type),
        mime_type,
        size: bytes.len() as u64,
        blob,
        content_id: None,
    })
}

/// Stores `path` off the GTK thread.
pub fn add_path(blobs: BlobStore, path: PathBuf) -> Local<Added> {
    Box::pin(async move {
        gio::spawn_blocking(move || store_file(&blobs, &path))
            .await
            .unwrap_or_else(|_| Err("the worker failed".to_owned()))
    })
}

/// Stores base64 `data` (a file dropped on the body) off the GTK thread.
pub fn add_base64(blobs: BlobStore, name: String, mime_type: String, data: String) -> Local<Added> {
    Box::pin(async move {
        gio::spawn_blocking(move || {
            let bytes = glib::base64_decode(&data);
            store_bytes(&blobs, &name, Some(&mime_type), bytes)
        })
        .await
        .unwrap_or_else(|_| Err("the worker failed".to_owned()))
    })
}

/// The badge's text: the extension, or the type's subtype, in capitals.
pub fn badge(name: &str, mime_type: &str) -> String {
    let from_name = attachment::extension(name)
        .filter(|extension| extension.len() <= 4 && extension.chars().all(char::is_alphanumeric));
    let text = from_name.unwrap_or_else(|| {
        mime_type
            .rsplit('/')
            .next()
            .filter(|subtype| subtype.len() <= 4)
            .unwrap_or("file")
    });
    text.to_uppercase()
}

struct Item {
    serial: u64,
    part: Option<DraftPart>,
    card: gtk::Box,
}

type Listener = Rc<dyn Fn()>;

/// The attachments of one draft, as cards in the footer.
pub struct Tray {
    root: adw::WrapBox,
    items: RefCell<Vec<Item>>,
    serial: Cell<u64>,
    listener: RefCell<Option<Listener>>,
}

impl Tray {
    pub fn new() -> Rc<Self> {
        let root = named(
            adw::WrapBox::builder()
                .child_spacing(8)
                .line_spacing(6)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build(),
            "composer-attachments",
        );
        Rc::new(Self {
            root,
            items: RefCell::new(Vec::new()),
            serial: Cell::new(0),
            listener: RefCell::new(None),
        })
    }

    pub fn widget(&self) -> &adw::WrapBox {
        &self.root
    }

    pub fn connect_changed(&self, listener: impl Fn() + 'static) {
        self.listener.replace(Some(Rc::new(listener)));
    }

    fn changed(&self) {
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener();
        }
    }

    /// The parts added so far, in order.
    pub fn parts(&self) -> Vec<DraftPart> {
        self.items
            .borrow()
            .iter()
            .filter_map(|item| item.part.clone())
            .collect()
    }

    /// Files still being added.
    pub fn busy(&self) -> usize {
        self.items
            .borrow()
            .iter()
            .filter(|item| item.part.is_none())
            .count()
    }

    pub fn cards(&self) -> Vec<gtk::Box> {
        self.items
            .borrow()
            .iter()
            .map(|item| item.card.clone())
            .collect()
    }

    /// Cards for parts a draft already has.
    pub fn set_parts(self: &Rc<Self>, parts: &[DraftPart]) {
        for item in self.items.take() {
            self.root.remove(&item.card);
        }
        for part in parts {
            let serial = self.next();
            let card = self.card(serial, &part.name, &part.mime_type, Some(part.size));
            self.root.append(&card);
            self.items.borrow_mut().push(Item {
                serial,
                part: Some(part.clone()),
                card,
            });
        }
    }

    fn next(&self) -> u64 {
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        serial
    }

    /// Shows a busy card for `name` while `adding` stores it; `failed` hears why it couldn't
    /// be added (the card goes).
    pub fn add(
        self: &Rc<Self>,
        name: &str,
        adding: Local<Added>,
        failed: impl Fn(&str, &str) + 'static,
    ) {
        let serial = self.next();
        let shown = attachment::safe_file_name(Some(name), "application/octet-stream");
        let card = self.card(serial, &shown, "", None);
        self.root.append(&card);
        self.items.borrow_mut().push(Item {
            serial,
            part: None,
            card,
        });
        self.changed();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let added = adding.await;
            let Some(tray) = weak.upgrade() else {
                return;
            };
            let at = tray
                .items
                .borrow()
                .iter()
                .position(|item| item.serial == serial);
            let Some(at) = at else {
                return;
            };
            match added {
                Ok(part) => {
                    let card = tray.card(serial, &part.name, &part.mime_type, Some(part.size));
                    let old =
                        std::mem::replace(&mut tray.items.borrow_mut()[at].card, card.clone());
                    tray.root.insert_child_after(&card, Some(&old));
                    tray.root.remove(&old);
                    tray.items.borrow_mut()[at].part = Some(part);
                }
                Err(reason) => {
                    let item = tray.items.borrow_mut().remove(at);
                    tray.root.remove(&item.card);
                    failed(&shown, &reason);
                }
            }
            tray.changed();
        });
    }

    /// Removes card `index` (a busy one too: its part, when it arrives, is dropped).
    pub fn remove(&self, index: usize) {
        let removed = {
            let mut items = self.items.borrow_mut();
            (index < items.len()).then(|| items.remove(index))
        };
        if let Some(item) = removed {
            self.root.remove(&item.card);
            self.changed();
        }
    }

    fn remove_serial(&self, serial: u64) {
        let index = self
            .items
            .borrow()
            .iter()
            .position(|item| item.serial == serial);
        if let Some(index) = index {
            self.remove(index);
        }
    }

    fn card(
        self: &Rc<Self>,
        serial: u64,
        name: &str,
        mime_type: &str,
        size: Option<u64>,
    ) -> gtk::Box {
        let badge = classed(
            gtk::Label::builder()
                .label(badge(name, mime_type))
                .valign(gtk::Align::Center)
                .build(),
            &["sw-attachment-type", "sw-composer-attachment-type"],
        );
        let title = classed(
            gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .max_width_chars(28)
                .build(),
            &["sw-composer-attachment-name"],
        );
        let meta = classed(
            gtk::Label::builder()
                .label(match size {
                    Some(size) => glib::format_size(size).to_string(),
                    None => "Adding…".to_owned(),
                })
                .xalign(0.0)
                .build(),
            &["sw-attachment-meta"],
        );
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        text.append(&title);
        text.append(&meta);
        let remove = classed(
            gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .tooltip_text("Remove attachment")
                .valign(gtk::Align::Center)
                .build(),
            &["sw-icon-button", "sw-composer-attachment-remove"],
        );
        remove.update_property(&[gtk::accessible::Property::Label(&format!("Remove {name}"))]);
        let weak = Rc::downgrade(self);
        remove.connect_clicked(move |_| {
            if let Some(tray) = weak.upgrade() {
                tray.remove_serial(serial);
            }
        });
        let card = classed(
            gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .build(),
            &["sw-composer-attachment"],
        );
        card.set_widget_name(&format!("composer-attachment-{serial}"));
        card.append(&badge);
        card.append(&text);
        card.append(&remove);
        card
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_count_the_encoding() {
        assert_eq!(encoded(0), 0);
        assert_eq!(encoded(3), 4 + 2);
        assert_eq!(encoded(57), 76 + 2);
        assert_eq!(encoded(58), 80 + 4);
        let mb = 1_000_000;
        let total = message_size([10 * mb, 5 * mb], 2_000);
        assert!(total > 20 * mb && total < 21 * mb, "{total}");
    }

    #[test]
    fn badges_name_the_type() {
        assert_eq!(badge("Report.pdf", "application/pdf"), "PDF");
        assert_eq!(badge("archive.tar.gz", "application/gzip"), "GZ");
        assert_eq!(badge("notes", "text/plain"), "FILE");
        assert_eq!(badge("image", "image/png"), "PNG");
    }
}
