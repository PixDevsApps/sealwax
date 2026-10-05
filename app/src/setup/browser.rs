//! Opens Microsoft's sign-in page (ML-032) in the default browser with `gtk::UriLauncher`, as
//! the reader opens links (SECURITY.md "Message rendering"). A headless (Broadway) run never
//! opens it, and the self-tests put a recorder in its place. The page's address is never
//! logged: it carries the client ID and the sign-in's one-time state.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio};
use std::cell::RefCell;
use std::rc::Rc;

type Opener = Rc<dyn Fn(&gtk::Widget, &str)>;

thread_local! {
    static OPENER: RefCell<Option<Opener>> = const { RefCell::new(None) };
}

/// Replaces how the sign-in page opens (`gtk::UriLauncher` by default), for tests.
pub fn set_opener(opener: impl Fn(&gtk::Widget, &str) + 'static) {
    OPENER.with(|current| current.replace(Some(Rc::new(opener))));
}

/// Opens `uri` from `widget`'s window.
pub(super) fn open(widget: &gtk::Widget, uri: &str) {
    match OPENER.with(|current| current.borrow().clone()) {
        Some(opener) => opener(widget, uri),
        None => launch(widget, uri),
    }
}

fn launch(widget: &gtk::Widget, uri: &str) {
    let broadway = gdk::Display::default()
        .is_some_and(|display| display.type_().name() == "GdkBroadwayDisplay");
    if broadway {
        tracing::warn!("setup: the sign-in page doesn't open from a headless (Broadway) run");
        return;
    }
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |result| {
        if result.is_err() {
            tracing::warn!("setup: the browser didn't open the sign-in page");
        }
    });
}
