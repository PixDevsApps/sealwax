//! The reader's registry commands (M5): the window's actions that act on the message or the
//! conversation the reader shows. Each owner connects its own commands below; a command stays
//! unavailable, and so insensitive in the toolbar, the menu and the palette, until it does.

use super::{Reader, remote};
use crate::actions::ActionSet;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use std::rc::Rc;

/// What one of the reader's commands does.
type Command = fn(&Reader);

/// Connects the reader's commands to the window's actions, once the reader exists.
pub fn connect(actions: &ActionSet, reader: &Reader) {
    conversation(actions, reader);
    files(actions, reader);
    remote_content(actions, reader);
}

/// Expand All Messages and Collapse All Messages (ML-057), available while the shown
/// conversation has more than one message.
fn conversation(actions: &ActionSet, reader: &Reader) {
    for (id, run) in [
        (ActionId::ExpandAll, Reader::expand_all as fn(&Reader)),
        (ActionId::CollapseAll, Reader::collapse_all),
    ] {
        let target = reader.clone();
        actions.connect(id, move || run(&target));
        if let Some(action) = actions.action(id).cloned() {
            action.set_enabled(reader.state().messages > 1);
            reader.connect_size(move |size| action.set_enabled(size > 1));
        }
    }
}

/// Print, Show Original and Save as .eml (ML-063), on the message the reader shows. They need a
/// selection (the registry's), and apply only while the reader shows one message (the app's
/// `refresh_commands`, as Open Message).
fn files(actions: &ActionSet, reader: &Reader) {
    for (id, run) in [
        (ActionId::Print, Reader::print as Command),
        (ActionId::ShowOriginal, Reader::show_original),
        (ActionId::SaveMessage, Reader::save_message),
    ] {
        let target = Rc::downgrade(&reader.inner);
        actions.connect(id, move || {
            if let Some(inner) = target.upgrade() {
                run(&Reader { inner });
            }
        });
    }
}

/// Show Images and Always Show Images from This Sender (ML-058), on the message the reader
/// shows: Show Images while its banner says its images are hidden; Always Show Images while it
/// has a sender address, its state whether that sender is on the list. Both follow the shown
/// message, the list and the banner; nothing else in the app enables or disables them.
fn remote_content(actions: &ActionSet, reader: &Reader) {
    let target = Rc::downgrade(&reader.inner);
    actions.connect(ActionId::ShowImages, move || {
        if let Some(inner) = target.upgrade() {
            Reader {
                inner: inner.clone(),
            }
            .show_remote_images();
            inner.remote.changed();
        }
    });
    let target = Rc::downgrade(&reader.inner);
    actions.connect_toggle(ActionId::AlwaysShowImages, false, move |on| {
        if let Some(inner) = target.upgrade() {
            Reader { inner }.set_always_show_images(on);
        }
    });
    let (Some(show), Some(always)) = (
        actions.action(ActionId::ShowImages).cloned(),
        actions.action(ActionId::AlwaysShowImages).cloned(),
    ) else {
        return;
    };
    let weak = Rc::downgrade(&reader.inner);
    let follow = Rc::new(move || {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let reader = Reader { inner };
        let shown = remote::shown(&reader).filter(|_| reader.widget().parent().is_some());
        show.set_enabled(shown.as_ref().is_some_and(|(_, _, banner)| *banner));
        let sender = shown
            .map(|(_, sender, _)| sender)
            .filter(|sender| !sender.is_empty());
        always.set_enabled(sender.is_some());
        let on = sender.is_some_and(|sender| reader.inner.remote.sender_allowed(&sender));
        always.set_state(&on.to_variant());
    });
    follow();
    let on_shown = follow.clone();
    reader.connect_shown(move |_| on_shown());
    let on_change = follow.clone();
    remote::connect_changed(reader, move || on_change());
    reader
        .widget()
        .connect_notify_local(Some("parent"), move |_, _| {
            let follow = follow.clone();
            glib::idle_add_local_once(move || follow());
        });
}
