//! Print, Show Original and Save as .eml (ML-063) for the message the reader shows.
//!
//! - **Print** renders a print document in a view from the reader's own pool (the same
//!   settings, CSP and content filter; remote images only where the user allowed them for that
//!   message or its sender), held by a window that is never shown, and runs WebKit's print
//!   dialog on it. The document is the message's own HTML, or its plain text escaped and
//!   wrapped, with a header (subject, From, To, Cc, Date) that Sealwax's isolated world puts at
//!   its top and a user stylesheet keeps as written. Page-initiated printing stays refused.
//! - **Show Original** shows the raw source in a read-only, monospace, selectable view in a
//!   dialog over the window: at most [`ORIGINAL_SHOWN`] bytes of it, with Save as .eml for the
//!   rest; bytes that aren't UTF-8 are shown as U+FFFD.
//! - **Save as .eml** saves the bytes the server has through the file chooser.
//!
//! A message stored as its text parts (`BodyState::Text`) is fetched whole first
//! (`FetchOriginal`); its stored copy, rebuilt from its text parts, is never shown or saved.

use super::engine::InlineParts;
use super::files::{self, Failure};
use super::prepare::{self, Body};
use super::view::ViewEvent;
use super::{Inner, Reader, format, rgba};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::attachment::eml_file_name;
use sealwax_domain::model::{MessageDetail, MessageId};
use sealwax_domain::theme::{BodyStyle, Rgb};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// The most of a message's source Show Original shows; Save as .eml has the rest.
pub const ORIGINAL_SHOWN: usize = 512 * 1024;

/// How long the print document may take to load, its images included.
const PRINT_LOAD_WAIT: Duration = Duration::from_secs(10);

/// The print header's look and the page, as a user stylesheet: the message's own CSS, even
/// `!important`, can't hide or restyle the header.
const PRINT_CSS: &str = "@page{margin:16mm 14mm}\n\
html,body{height:auto!important;min-height:0!important;max-height:none!important;\
overflow:visible!important}\n\
#sealwax-print{display:block!important;visibility:visible!important;position:static!important;\
opacity:1!important;transform:none!important;margin:0 0 14px!important;padding:0!important;\
background:#ffffff!important;color:#1f1f1f!important;font:13px/1.45 sans-serif!important;\
text-align:left!important}\n\
#sealwax-print-subject{display:block!important;visibility:visible!important;\
margin:0 0 8px!important;color:#1f1f1f!important;font:bold 18px/1.3 sans-serif!important}\n\
#sealwax-print table{display:table!important;visibility:visible!important;\
border-collapse:collapse!important;margin:0!important}\n\
#sealwax-print tr{display:table-row!important;visibility:visible!important}\n\
#sealwax-print th,#sealwax-print td{display:table-cell!important;visibility:visible!important;\
padding:0 12px 2px 0!important;vertical-align:top!important;font:13px/1.45 sans-serif!important;\
text-align:left!important;border:0!important;background:none!important}\n\
#sealwax-print th{color:#5e5e5e!important;font-weight:bold!important;white-space:nowrap!important}\n\
#sealwax-print td{color:#1f1f1f!important}\n\
#sealwax-print hr{display:block!important;visibility:visible!important;border:0!important;\
border-top:1px solid #bbbbbb!important;margin:10px 0 0!important;height:0!important}\n";

/// A plain-text body's print document: the text escaped and wrapped, in the reading font.
fn plain_document(text: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         #sealwax-print-text{{white-space:pre-wrap;overflow-wrap:anywhere;margin:0;\
         font:inherit}}</style></head><body><pre id=\"sealwax-print-text\">{}</pre></body></html>",
        escape_html(text)
    )
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\0' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// `text` as a JavaScript string literal: every character but letters, digits and spaces
/// escaped, so message content can't end the literal.
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == ' ' {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out.push('"');
    out
}

/// The isolated-world script that puts the header at the top of the print document.
fn header_script(detail: &MessageDetail) -> String {
    let headers = &detail.headers;
    let zone = glib::TimeZone::local();
    let date = headers.date.unwrap_or(detail.summary.date);
    let subject = headers.subject.trim();
    let subject = if subject.is_empty() {
        "(No subject)"
    } else {
        subject
    };
    let rows: Vec<String> = [
        ("From", format::address_list(&headers.from)),
        ("To", format::address_list(&headers.to)),
        ("Cc", format::address_list(&headers.cc)),
        ("Date", format::full_date(date, &zone)),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(name, value)| format!("[{},{}]", js_string(name), js_string(&value)))
    .collect();
    format!(
        "const rows = [{rows}];\n\
         const box = document.createElement('div');\n\
         box.id = 'sealwax-print';\n\
         const title = document.createElement('div');\n\
         title.id = 'sealwax-print-subject';\n\
         title.textContent = {subject};\n\
         box.appendChild(title);\n\
         const table = document.createElement('table');\n\
         for (const [name, value] of rows) {{\n\
           const row = table.insertRow();\n\
           const head = document.createElement('th');\n\
           head.textContent = name;\n\
           row.appendChild(head);\n\
           row.insertCell().textContent = value;\n\
         }}\n\
         box.appendChild(table);\n\
         box.appendChild(document.createElement('hr'));\n\
         const body = document.body || document.documentElement;\n\
         body.insertBefore(box, body.firstChild);\n\
         return true;",
        rows = rows.join(","),
        subject = js_string(subject)
    )
}

/// Prints the shown message: WebKit's print dialog, or a PDF when a print target is set.
pub(super) fn print(reader: &Reader) {
    let Some(id) = reader.message() else {
        return;
    };
    let inner = reader.inner.clone();
    if inner.files.printing.replace(true) {
        return;
    }
    glib::spawn_future_local(async move {
        let target = inner.files.print_target();
        if target.is_none() && files::headless() {
            tracing::warn!("reader: a headless (Broadway) run opens no print dialog");
        } else if let Err(error) = print_message(&inner, id, target).await {
            tracing::warn!(error, "reader: the message wasn't printed");
            files::toast(&inner, &format!("Couldn't print the message: {error}."));
        }
        inner.files.printing.set(false);
    });
}

/// The stored message and what the reader would show of it.
async fn read(inner: &Inner, id: MessageId) -> Result<(MessageDetail, Body), &'static str> {
    let reads = inner.store.read(id).ok_or("the mail store isn't open")?;
    let detail = reads
        .detail
        .recv()
        .await
        .ok()
        .and_then(|versioned| versioned.value)
        .ok_or("it is no longer here")?;
    let raw = reads.raw.recv().await.ok().flatten();
    let for_worker = detail.clone();
    let prepared = gio::spawn_blocking(move || prepare::prepare(&for_worker, raw.as_deref()))
        .await
        .map_err(|_| "it couldn't be read")?;
    Ok((detail, prepared.body))
}

async fn print_message(
    inner: &Rc<Inner>,
    id: MessageId,
    target: Option<PathBuf>,
) -> Result<(), &'static str> {
    let (detail, body) = read(inner, id).await?;
    let (html, parts) = match body {
        Body::Html { html, parts, .. } => (html, parts),
        Body::Plain(text) => (plain_document(&text), InlineParts::default()),
        Body::Empty => (plain_document(""), InlineParts::default()),
        Body::NotDownloaded => (
            plain_document("The rest of this message hasn't been downloaded yet."),
            InlineParts::default(),
        ),
    };
    let filter = inner
        .engine
        .filter()
        .await
        .ok_or("its formatted view isn't available")?;
    let theme = inner.theme.theme();
    let view = inner
        .engine
        .acquire(&theme.webview_fonts())
        .ok_or("every message view is in use")?;
    // A WebView loads and prints only with a window above it; this one is never shown.
    let holder = gtk::Window::new();
    holder.set_child(Some(view.web_view()));
    let (events, received) = async_channel::unbounded::<ViewEvent>();
    view.set_handler(move |event| {
        let _ = events.try_send(event.clone());
    });
    inner.engine.set_parts(view.page_id(), Rc::new(parts));
    let sender = detail
        .headers
        .from
        .first()
        .map(|from| from.email.to_lowercase())
        .unwrap_or_default();
    let allowed = inner.remote.allows(id, &sender);
    let css = format!("{}{PRINT_CSS}", theme.webview_css(BodyStyle::Paper));
    view.load(
        &html,
        &css,
        &rgba(Rgb::from_hex(0xffffff)),
        (!allowed).then_some(&filter),
    );
    let printed = async {
        loop {
            match glib::future_with_timeout(PRINT_LOAD_WAIT, received.recv()).await {
                Ok(Ok(ViewEvent::Loaded)) => break,
                Ok(Ok(ViewEvent::Failed | ViewEvent::Crashed)) | Ok(Err(_)) => {
                    return Err("it couldn't be laid out");
                }
                Ok(Ok(_)) => {}
                Err(_) => {
                    tracing::warn!("reader: the print document took too long to load");
                    break;
                }
            }
        }
        view.eval(&header_script(&detail))
            .await
            .map_err(|_| "its header couldn't be added")?;
        run_print(inner, view.web_view(), target, &detail).await
    }
    .await;
    inner.engine.release(view);
    holder.destroy();
    printed
}

/// WebKit's print dialog over the window, or straight into a PDF at `target`.
async fn run_print(
    inner: &Inner,
    web_view: &webkit6::WebView,
    target: Option<PathBuf>,
    detail: &MessageDetail,
) -> Result<(), &'static str> {
    let operation = webkit6::PrintOperation::new(web_view);
    let (done, finished) = async_channel::bounded::<Result<(), ()>>(2);
    let failed = done.clone();
    operation.connect_failed(move |_, error| {
        tracing::warn!(%error, "reader: printing failed");
        let _ = failed.try_send(Err(()));
    });
    operation.connect_finished(move |_| {
        let _ = done.try_send(Ok(()));
    });
    // "Print to File" names the file after the subject, made safe as Save as .eml's name is.
    let basename = eml_file_name(&detail.headers.subject);
    let basename = basename.trim_end_matches(".eml");
    match target {
        Some(path) => {
            // Only the file printer: a test never asks CUPS or the network for printers.
            if let Some(settings) = gtk::Settings::default() {
                settings.set_property("gtk-print-backends", "file");
            }
            let settings = gtk::PrintSettings::new();
            settings.set_printer("Print to File");
            settings.set(gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT, Some("pdf"));
            settings.set(
                gtk::PRINT_SETTINGS_OUTPUT_URI,
                Some(gio::File::for_path(&path).uri().as_str()),
            );
            operation.set_print_settings(&settings);
            operation.print();
        }
        None => {
            let settings = gtk::PrintSettings::new();
            settings.set(gtk::PRINT_SETTINGS_OUTPUT_BASENAME, Some(basename));
            operation.set_print_settings(&settings);
            let parent = files::window(inner);
            if operation.run_dialog(parent.as_ref()) == webkit6::PrintOperationResponse::Cancel {
                return Ok(());
            }
        }
    }
    match finished.recv().await {
        Ok(Ok(())) => Ok(()),
        _ => Err("the printer refused it"),
    }
}

/// The Show Original dialog open now, for checks.
pub struct OriginalDialog {
    pub dialog: adw::Dialog,
    pub stack: gtk::Stack,
    pub text: gtk::TextView,
    pub status: gtk::Label,
    pub notice: gtk::Label,
    pub retry: gtk::Button,
    pub save: gtk::Button,
    /// Save as .eml beside the notice that the source is cut.
    pub save_rest: gtk::Button,
}

thread_local! {
    static ORIGINAL: RefCell<Option<Rc<OriginalDialog>>> = const { RefCell::new(None) };
}

/// The Show Original dialog open now, if any.
pub fn original_dialog() -> Option<Rc<OriginalDialog>> {
    ORIGINAL
        .with(|open| open.borrow().clone())
        .filter(|open| open.dialog.is_mapped())
}

/// Shows the shown message's raw source in a dialog over the window.
pub(super) fn show_original(reader: &Reader) {
    let Some(id) = reader.message() else {
        return;
    };
    let inner = reader.inner.clone();
    let open = Rc::new(original_widgets());
    open.dialog.present(Some(&inner.stage));
    ORIGINAL.with(|current| current.replace(Some(open.clone())));
    let saved: Rc<RefCell<Option<Rc<Vec<u8>>>>> = Rc::default();
    let save = {
        let inner = Rc::downgrade(&inner);
        let saved = saved.clone();
        move || {
            if let Some(inner) = inner.upgrade() {
                let bytes = saved.borrow().clone();
                glib::spawn_future_local(save(inner, id, bytes));
            }
        }
    };
    let on_save = save.clone();
    open.save.connect_clicked(move |_| on_save());
    let on_save = save.clone();
    open.save_rest.connect_clicked(move |_| on_save());
    let weak = Rc::downgrade(&open);
    let retry_inner = Rc::downgrade(&inner);
    let retry_saved = saved.clone();
    open.retry.connect_clicked(move |_| {
        if let (Some(open), Some(inner)) = (weak.upgrade(), retry_inner.upgrade()) {
            glib::spawn_future_local(fill_original(inner, id, open, retry_saved.clone()));
        }
    });
    glib::spawn_future_local(fill_original(inner, id, open, saved));
}

fn original_widgets() -> OriginalDialog {
    let status = gtk::Label::builder()
        .label("Reading the message…")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["sw-secondary"])
        .build();
    status.set_widget_name("original-status");
    let retry = gtk::Button::builder()
        .label("Retry")
        .halign(gtk::Align::Center)
        .visible(false)
        .css_classes(["sw-control"])
        .build();
    retry.set_widget_name("original-retry");
    let waiting = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .valign(gtk::Align::Center)
        .margin_start(24)
        .margin_end(24)
        .build();
    waiting.append(&status);
    waiting.append(&retry);
    let text = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(12)
        .right_margin(12)
        .top_margin(10)
        .bottom_margin(10)
        .css_classes(["sw-original-text"])
        .build();
    text.set_widget_name("original-text");
    text.set_monospace(true);
    text.update_property(&[gtk::accessible::Property::Label("Original message")]);
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&text)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let notice = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["sw-secondary", "sw-small"])
        .build();
    notice.set_widget_name("original-notice");
    let save_rest = gtk::Button::builder()
        .label("Save as .eml…")
        .valign(gtk::Align::Center)
        .css_classes(["sw-control"])
        .build();
    save_rest.set_widget_name("original-save-rest");
    let notice_row = gtk::Box::builder()
        .spacing(12)
        .visible(false)
        .css_classes(["sw-original-notice"])
        .build();
    notice_row.append(&notice);
    notice_row.append(&save_rest);
    let source = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    source.append(&scrolled);
    source.append(&notice_row);
    let stack = gtk::Stack::new();
    stack.add_named(&waiting, Some("waiting"));
    stack.add_named(&source, Some("source"));
    stack.set_visible_child_name("waiting");
    let save = gtk::Button::builder()
        .label("Save as .eml…")
        .tooltip_text("Save the message as the server has it")
        .build();
    save.set_widget_name("original-save");
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.set_content(Some(&stack));
    let dialog = adw::Dialog::builder()
        .title("Original Message")
        .content_width(860)
        .content_height(640)
        .child(&content)
        .css_classes(["sw-original"])
        .build();
    dialog.set_widget_name("original-dialog");
    OriginalDialog {
        dialog,
        stack,
        text,
        status,
        notice,
        retry,
        save,
        save_rest,
    }
}

/// Reads or downloads the source and shows it, capped at [`ORIGINAL_SHOWN`].
async fn fill_original(
    inner: Rc<Inner>,
    id: MessageId,
    open: Rc<OriginalDialog>,
    saved: Rc<RefCell<Option<Rc<Vec<u8>>>>>,
) {
    open.stack.set_visible_child_name("waiting");
    open.retry.set_visible(false);
    open.status.set_text("Reading the message…");
    let bytes = files::original(&inner, id, || {
        open.status
            .set_text("Downloading the original message from the server…");
    })
    .await;
    let bytes = match bytes {
        Ok(bytes) => Rc::new(bytes),
        Err(failure) => {
            tracing::info!(id = id.0, ?failure, "reader: no original to show");
            open.status.set_text(&format!(
                "The original couldn't be shown: {}.",
                failure.reason()
            ));
            open.retry
                .set_visible(!matches!(failure, Failure::NotDownloaded));
            return;
        }
    };
    saved.replace(Some(bytes.clone()));
    let total = bytes.len();
    let head = bytes[..total.min(ORIGINAL_SHOWN)].to_vec();
    let shown = gio::spawn_blocking(move || source_text(&head))
        .await
        .unwrap_or_default();
    open.text.buffer().set_text(&shown);
    if total > ORIGINAL_SHOWN {
        open.notice.set_text(&format!(
            "Showing the first {} of {}. Save as .eml to read all of it.",
            format::size(ORIGINAL_SHOWN as u64),
            format::size(total as u64)
        ));
        if let Some(row) = open.notice.parent() {
            row.set_visible(true);
        }
    }
    open.stack.set_visible_child_name("source");
}

/// Source bytes as text: invalid UTF-8 and NUL become U+FFFD.
fn source_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\0', "\u{FFFD}")
}

/// Saves the shown message as `.eml` through the file chooser.
pub(super) fn save_message(reader: &Reader) {
    if let Some(id) = reader.message() {
        glib::spawn_future_local(save(reader.inner.clone(), id, None));
    }
}

/// Asks where, then saves the bytes the server has (`known` when Show Original already has
/// them).
async fn save(inner: Rc<Inner>, id: MessageId, known: Option<Rc<Vec<u8>>>) {
    let subject = match inner.store.read(id) {
        Some(reads) => reads
            .detail
            .recv()
            .await
            .ok()
            .and_then(|versioned| versioned.value)
            .map(|detail| detail.headers.subject)
            .unwrap_or_default(),
        None => String::new(),
    };
    let Some(file) = files::choose_save(&inner, "Save Message", &eml_file_name(&subject)).await
    else {
        return;
    };
    let bytes = match known {
        Some(bytes) => bytes.as_ref().clone(),
        None => {
            let downloading = || files::toast(&inner, "Downloading the message from the server…");
            match files::original(&inner, id, downloading).await {
                Ok(bytes) => bytes,
                Err(failure) => {
                    tracing::info!(id = id.0, ?failure, "reader: no message to save");
                    files::toast(
                        &inner,
                        &format!("Couldn't save the message: {}.", failure.reason()),
                    );
                    return;
                }
            }
        }
    };
    match files::write(&file, bytes).await {
        Ok(()) => {
            let name = file
                .basename()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            files::toast(&inner, &format!("Saved {name}"));
        }
        Err(error) => {
            tracing::warn!("reader: a message couldn't be saved");
            files::toast(
                &inner,
                &format!("Couldn't save the message: {}", error.message()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_prints_escaped() {
        let document = plain_document("<script>alert(1)</script> & \"Tom's\"\0");
        assert!(
            document.contains(
                "&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;Tom&#39;s&quot;\u{FFFD}"
            )
        );
        assert!(!document.contains("<script>"));
    }

    #[test]
    fn javascript_strings_escape_everything_but_letters_digits_and_spaces() {
        assert_eq!(js_string("Hi 2"), "\"Hi 2\"");
        assert_eq!(js_string("a\"b"), "\"a\\u0022b\"");
        assert_eq!(js_string("</script>"), "\"\\u003c\\u002fscript\\u003e\"");
        assert_eq!(js_string("\u{2028}"), "\"\\u2028\"");
        assert_eq!(js_string("æ😀"), "\"\\u00e6\\ud83d\\ude00\"");
    }

    #[test]
    fn source_text_replaces_invalid_bytes_and_nul() {
        assert_eq!(source_text(b"Subject: a\r\n"), "Subject: a\r\n");
        assert_eq!(source_text(b"a\xffb\0c"), "a\u{FFFD}b\u{FFFD}c");
    }

    #[test]
    fn the_print_header_holds_its_ground() {
        for rule in [
            "display:block!important",
            "visibility:visible!important",
            "@page",
        ] {
            assert!(PRINT_CSS.contains(rule), "{rule}");
        }
    }
}
