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
    scrolling(actions, reader);
    files(actions, reader);
    remote_content(actions, reader);
    quick_reply(actions, reader);
}

/// Expand All Messages, Collapse All Messages, and Next and Previous Message in Conversation
/// (ML-057), available while the shown conversation has more than one message.
fn conversation(actions: &ActionSet, reader: &Reader) {
    for (id, run) in [
        (ActionId::ExpandAll, Reader::expand_all as fn(&Reader)),
        (ActionId::CollapseAll, Reader::collapse_all),
        (ActionId::NextInConversation, Reader::next_in_conversation),
        (
            ActionId::PreviousInConversation,
            Reader::previous_in_conversation,
        ),
    ] {
        // Weak: the reader keeps the action (below), so a strong reader here would keep both
        // alive after the window closes into background mode (M7).
        let target = Rc::downgrade(&reader.inner);
        actions.connect(id, move || {
            if let Some(inner) = target.upgrade() {
                run(&Reader { inner });
            }
        });
        if let Some(action) = actions.action(id).cloned() {
            action.set_enabled(reader.state().messages > 1);
            reader.connect_size(move |size| action.set_enabled(size > 1));
        }
    }
}

/// Scroll Message Down and Up (`d`, Shift+D; issue #10), available while the reader shows a
/// conversation.
fn scrolling(actions: &ActionSet, reader: &Reader) {
    for (id, down) in [
        (ActionId::ScrollMessageDown, true),
        (ActionId::ScrollMessageUp, false),
    ] {
        let target = Rc::downgrade(&reader.inner);
        actions.connect(id, move || {
            if let Some(inner) = target.upgrade() {
                inner.scroll_half_page(down);
            }
        });
        if let Some(action) = actions.action(id).cloned() {
            action.set_enabled(reader.state().messages > 0);
            reader.connect_size(move |size| action.set_enabled(size > 0));
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

/// Quick Reply, Open Reply in Composer and Send (M6, ML-068), on the quick reply under the shown
/// conversation: the first two while one is on screen, Send while it holds text (the quick
/// reply's Send button is bound to it, and its Ctrl+Enter is the Composer scope's).
fn quick_reply(actions: &ActionSet, reader: &Reader) {
    for (id, run) in [
        (ActionId::QuickReply, Reader::focus_quick_reply as Command),
        (ActionId::ReplyInComposer, Reader::expand_quick_reply),
        (ActionId::Send, Reader::send_quick_reply),
    ] {
        let target = Rc::downgrade(&reader.inner);
        actions.connect(id, move || {
            if let Some(inner) = target.upgrade() {
                run(&Reader { inner });
            }
        });
    }
    let (Some(quick), Some(expand), Some(send)) = (
        actions.action(ActionId::QuickReply).cloned(),
        actions.action(ActionId::ReplyInComposer).cloned(),
        actions.action(ActionId::Send).cloned(),
    ) else {
        return;
    };
    let follow = move |state: super::QuickState| {
        quick.set_enabled(state.shown);
        expand.set_enabled(state.shown);
        send.set_enabled(state.has_text);
    };
    follow(reader.quick_state());
    reader.connect_quick_state(follow);
}
