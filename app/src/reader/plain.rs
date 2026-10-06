//! A plain-text body (M1; M5 adds format=flowed, collapsed quotes and links in plain text):
//! the reading font in a GTK text view, no WebView.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

/// A plain-text body in the reading font, selectable, wrapped, with no WebView.
pub fn plain_text(text: &str) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .accepts_tab(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .css_classes(["sw-reading", "sw-reader-plain"])
        .build();
    view.set_widget_name("reader-plain");
    view.update_property(&[gtk::accessible::Property::Label("Message text")]);
    view.buffer().set_text(text.trim_end());
    // GTK 4.22 leaves a text view that holds a single line at no height until something
    // resizes it, so a one-paragraph message showed no text (M1 gate previews). A resize once
    // it is on screen, after its first validation, gives it its height.
    view.connect_map(|view| {
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            view,
            move || view.queue_resize()
        ));
    });
    view
}
