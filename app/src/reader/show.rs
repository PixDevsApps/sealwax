//! Showing a conversation: read it, build its page (rows and card shells), load the expanded
//! messages, lay the page out unseen until the bodies its first screen needs have their heights,
//! then put it on screen in place of the shown one, scrolled to the selected message. A store
//! change that touches the shown conversation reads it again and changes the page in place,
//! keeping its bodies and its scroll.

use super::conversation::{
    self, Entry, Fold, Item, Page, collapsed_row, fill_row, fold_row, folds, heading,
    initially_expanded, initially_shown,
};
use super::{Inner, Opened, READY_TIMEOUT, Shown, ShownBody, page, widgets};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::MessageId;
use sealwax_engine::api::BodyError;
use sealwax_infrastructure::store::{ChangeEvent, Conversation, ConversationMessage};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

impl Inner {
    pub(super) async fn load(self: Rc<Self>, id: MessageId, generation: u64) {
        let started = Instant::now();
        let conversation = match self.store.conversation(id) {
            Some(reply) => match reply.recv().await {
                Ok(versioned) => versioned.value,
                Err(error) => {
                    tracing::warn!(message = id.0, %error, "reader: the conversation couldn't be read");
                    None
                }
            },
            None => None,
        };
        if self.stale(generation) {
            return;
        }
        let Some(conversation) = conversation else {
            self.show_missing(id, generation, started);
            return;
        };
        let fetched = started.elapsed();
        if self.filter.borrow().is_none() {
            let filter = self.engine.filter().await;
            if self.stale(generation) {
                return;
            }
            self.filter.replace(filter);
        }
        self.discard_pending();
        let page = self.build_page(generation, &conversation);
        let expanded: Vec<MessageId> = page
            .items()
            .filter(|item| item.expanded)
            .map(|item| item.id())
            .collect();
        tracing::debug!(
            message = id.0,
            messages = conversation.messages.len(),
            expanded = expanded.len(),
            "reader: showing a conversation"
        );
        self.pending.replace(Some(page));
        let loads: Vec<_> = expanded
            .iter()
            .map(|message| {
                let token = self.next_token();
                self.with_page(generation, |page| {
                    if let Some(item) = page.item_mut(*message) {
                        item.load = token;
                    }
                });
                let inner = self.clone();
                let message = *message;
                glib::spawn_future_local(async move {
                    inner.load_card(generation, message, token).await;
                })
            })
            .collect();
        for load in loads {
            let _ = load.await;
        }
        if self.stale(generation) {
            return;
        }
        self.attach_quick_reply(generation);
        self.request_bodies(generation);
        let prepared = started.elapsed();
        let staged = self.stage_pending(generation);
        if staged {
            self.lay_out(generation).await;
            if self.stale(generation) {
                return;
            }
        }
        self.present(generation, staged, started, fetched, prepared);
    }

    /// The page of `conversation`: the heading, then a row or an expanded card's place for each
    /// message, the middle of a long conversation folded.
    fn build_page(self: &Rc<Self>, key: u64, conversation: &Conversation) -> Page {
        let selected = conversation
            .messages
            .iter()
            .find(|message| message.summary.id == conversation.selected)
            .or(conversation.messages.last());
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .css_classes(["sw-reader-page"])
            .build();
        root.set_widget_name("reader-page");
        root.append(&heading(
            selected.map_or("", |message| message.summary.subject.as_str()),
        ));
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(["sw-reader-conversation"])
            .build();
        list.set_widget_name("reader-conversation");
        list.update_property(&[gtk::accessible::Property::Label(&format!(
            "Conversation, {}",
            conversation::size_text(conversation.messages.len())
        ))]);
        root.append(&list);
        let expanded = initially_expanded(&conversation.messages, conversation.selected);
        let shown = initially_shown(&expanded);
        let folded = folds(&shown);
        let mut entries = Vec::new();
        let mut index = 0;
        while index < conversation.messages.len() {
            if let Some(fold) = folded.iter().find(|fold| fold.start == index) {
                let messages = conversation.messages[fold.clone()].to_vec();
                entries.push(Entry::Fold(self.fold(key, messages)));
                index = fold.end;
                continue;
            }
            let item = self.item(key, &conversation.messages[index], expanded[index]);
            entries.push(Entry::Message(Box::new(item)));
            index += 1;
        }
        let newest_first = self.newest_first.get();
        for entry in &entries {
            if newest_first {
                list.prepend(&entry_widget(entry));
            } else {
                list.append(&entry_widget(entry));
            }
        }
        Page {
            key,
            selected: conversation.selected,
            thread: Some(conversation.thread),
            root,
            list,
            entries,
            members: conversation.members().collect(),
            missing: false,
            current: None,
            quick: None,
            newest_first,
        }
    }

    /// One message's place in the page: its row, which a click expands.
    pub(super) fn item(
        self: &Rc<Self>,
        key: u64,
        message: &ConversationMessage,
        expanded: bool,
    ) -> Item {
        let row = collapsed_row(&message.summary, super::now());
        let id = message.summary.id;
        let weak = Rc::downgrade(self);
        row.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && inner.is_shown(key)
            {
                inner.expand(id);
            }
        });
        let slot = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        slot.set_widget_name("reader-item");
        if !expanded {
            slot.append(&row);
        }
        Item {
            message: message.clone(),
            slot,
            row,
            expanded,
            card: None,
            load: 0,
            opened: false,
        }
    }

    /// Folded messages' row, which reveals them a step at a time.
    pub(super) fn fold(self: &Rc<Self>, key: u64, messages: Vec<ConversationMessage>) -> Fold {
        let row = fold_row(messages.len());
        let first = messages.first().map(|message| message.summary.id);
        let weak = Rc::downgrade(self);
        row.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && inner.is_shown(key)
                && let Some(first) = first
            {
                inner.unfold(first);
            }
        });
        Fold { row, messages }
    }

    /// Asks once for the bodies the store doesn't hold of the page's expanded messages.
    pub(super) fn request_bodies(self: &Rc<Self>, key: u64) {
        let wanted: Vec<MessageId> = self
            .with_page(key, |page| {
                page.items()
                    .filter(|item| {
                        item.expanded
                            && item
                                .card
                                .as_ref()
                                .is_some_and(|card| card.body == ShownBody::Downloading)
                    })
                    .map(|item| item.id())
                    .collect()
            })
            .unwrap_or_default();
        let wanted: Vec<MessageId> = wanted
            .into_iter()
            .filter(|id| !self.fetching.borrow().contains(id))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let Some(fetch) = self.fetcher.borrow().clone() else {
            return;
        };
        if fetch(&wanted) {
            self.fetching.borrow_mut().extend(wanted);
        } else {
            for id in wanted {
                self.answers.borrow_mut().insert(id, Err(BodyError::Other));
                self.reload_card(key, id);
            }
        }
    }

    /// Puts the page being read into the stage, to lay it out unseen; `false` when it has no
    /// HTML body to wait for, or the reader isn't on screen to lay anything out.
    fn stage_pending(&self, key: u64) -> bool {
        let pending = self.pending.borrow();
        let Some(page) = pending.as_ref().filter(|page| page.key == key) else {
            return false;
        };
        let html = page
            .items()
            .any(|item| item.card.as_ref().is_some_and(|card| card.html.is_some()));
        if !html || !self.stage.is_mapped() {
            return false;
        }
        self.stage.stage(&page.root);
        true
    }

    /// Waits until the bodies the page's first screen needs have their heights at the column's
    /// width and their images in: every expanded HTML body up to the selected message, and
    /// those below it that start within a screen of its top. At most [`READY_TIMEOUT`], and no
    /// longer than a conversation whose message left the list may stay ([`super::LEFT_SHOWN`]).
    async fn lay_out(self: &Rc<Self>, key: u64) {
        let deadline = Instant::now() + READY_TIMEOUT;
        while let Some(waiting) = self.first_screen_waiting(key) {
            if !waiting {
                return;
            }
            if !self.stage.is_mapped() {
                return;
            }
            let due = self.left_due_at();
            let now = Instant::now();
            if due.is_some_and(|due| due <= now) {
                tracing::debug!(
                    "reader: shown before it was laid out, in place of a message that left the list"
                );
                return;
            }
            let left = deadline.saturating_duration_since(now);
            if left.is_zero() {
                tracing::warn!(
                    "reader: the conversation took over {} s to lay out; shown as it is",
                    READY_TIMEOUT.as_secs()
                );
                return;
            }
            self.schedule_now();
            let wait = due.map_or(left, |due| left.min(due.saturating_duration_since(now)));
            let _ = glib::future_with_timeout(
                wait.min(Duration::from_millis(50)),
                self.progress.1.recv(),
            )
            .await;
            while self.progress.1.try_recv().is_ok() {}
            if self.stale(key) {
                return;
            }
        }
    }

    /// Puts the page being laid out on screen; the page it replaces gives its views back.
    fn present(
        self: &Rc<Self>,
        key: u64,
        staged: bool,
        started: Instant,
        fetched: Duration,
        prepared: Duration,
    ) {
        let Some(page) = self.pending.take().filter(|page| page.key == key) else {
            return;
        };
        let focus = page.selected_card().and_then(|card| card.focus_target());
        if staged
            && self
                .stage
                .staged()
                .is_some_and(|staged| staged == *page.root.upcast_ref::<gtk::Widget>())
        {
            self.stage.promote(focus.as_ref());
        } else {
            self.stage.unstage();
            self.stage.show(Some(&page.root), focus.as_ref());
        }
        let selected = page.selected;
        let shown = Shown {
            id: selected,
            body: if page.missing {
                ShownBody::Missing
            } else {
                page.selected_card()
                    .map_or(ShownBody::Empty, |card| card.body.clone())
            },
            fetched,
            prepared,
            ready: started.elapsed(),
            messages: page.message_count(),
            expanded: page.expanded().len(),
        };
        let count = page.message_count();
        if let Some(old) = self.shown.replace(Some(page)) {
            self.release_page(old);
        }
        self.left.set(None);
        self.hidden.set(false);
        self.scroll_to_selected();
        tracing::debug!(
            message = selected.0,
            body = ?shown.body,
            messages = shown.messages,
            read_ms = shown.fetched.as_secs_f64() * 1000.0,
            prepared_ms = shown.prepared.as_secs_f64() * 1000.0,
            ms = shown.ready.as_secs_f64() * 1000.0,
            "reader: shown"
        );
        let listeners = self.on_shown.borrow().clone();
        for listener in listeners {
            listener(&shown);
        }
        self.report_opened(key);
        self.size_changed();
        self.quick_changed();
        if count > 1 {
            self.stage.announce(
                &format!(
                    "Conversation, {}, {} expanded",
                    conversation::size_text(count),
                    shown.expanded
                ),
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        }
        self.schedule();
    }

    /// When the shown conversation, whose message left the list, must be off screen.
    fn left_due_at(&self) -> Option<Instant> {
        self.left
            .get()
            .filter(|(key, _)| self.is_shown(*key))
            .map(|(_, due)| due)
    }

    /// The time a conversation whose message left the list may stay is up: a next one being
    /// laid out shows as it is; one not read yet hides the left one until it is.
    pub(super) fn left_due(self: &Rc<Self>, key: u64) {
        if self.left.get().is_none_or(|(left, _)| left != key) || !self.is_shown(key) {
            return;
        }
        let staged = self.pending.borrow().as_ref().is_some_and(|page| {
            self.stage
                .staged()
                .is_some_and(|staged| staged == *page.root.upcast_ref::<gtk::Widget>())
        });
        if staged {
            let _ = self.progress.0.try_send(());
            return;
        }
        if let Some(page) = self.shown.borrow().as_ref() {
            // Kept in place, unseen: a focus in it moves on to the next page as usual.
            page.root.set_opacity(0.0);
            page.root.set_can_target(false);
            page.root
                .update_state(&[gtk::accessible::State::Hidden(true)]);
        }
        self.hidden.set(true);
        tracing::debug!("reader: a message that left the list hidden; the next isn't read yet");
    }

    /// The bodies of page `key` that came on screen and weren't reported yet.
    pub(super) fn report_opened(&self, key: u64) {
        if !self.is_shown(key) {
            return;
        }
        let opened = self.with_page(key, |page| {
            let messages = page
                .items_mut()
                .filter(|item| {
                    !item.opened
                        && item.expanded
                        && item.card.as_ref().is_some_and(|card| card.body.is_shown())
                })
                .map(|item| {
                    item.opened = true;
                    item.message.summary.clone()
                })
                .collect();
            Opened {
                selected: page.selected,
                messages,
            }
        });
        if let Some(opened) = opened {
            for message in &opened.messages {
                self.store.opened(message.id, super::now());
            }
            self.opened(opened);
        }
    }

    fn show_missing(self: &Rc<Self>, id: MessageId, generation: u64, started: Instant) {
        self.discard_pending();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        root.append(&page::notice("This message is no longer here."));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        self.pending.replace(Some(Page {
            key: generation,
            selected: id,
            thread: None,
            root,
            list,
            entries: Vec::new(),
            members: HashSet::from([id]),
            missing: true,
            current: None,
            quick: None,
            newest_first: false,
        }));
        let elapsed = started.elapsed();
        self.present(generation, false, started, elapsed, elapsed);
    }

    /// A store commit: cards waiting for their body load again; a commit that touched the
    /// shown conversation reads it again; one that touched the conversation being laid out
    /// starts its `show` again.
    pub(super) fn store_changed(self: &Rc<Self>, event: &ChangeEvent) {
        let touches = |page: &Page| {
            event
                .inserted
                .iter()
                .chain(&event.removed)
                .chain(&event.changed)
                .any(|id| page.members.contains(id))
                || page.thread.is_some_and(|thread| {
                    event.threads.iter().any(|count| count.thread == thread)
                        || event
                            .merged_threads
                            .iter()
                            .any(|merge| merge.from == thread || merge.into == thread)
                })
        };
        let pending = self
            .pending
            .borrow()
            .as_ref()
            .filter(|page| touches(page))
            .map(|page| page.selected);
        if let Some(selected) = pending
            && self.target.get() == Some(selected)
        {
            let generation = self.next_generation();
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.load(selected, generation).await });
            return;
        }
        let shown = self.shown.borrow().as_ref().map(|page| {
            let waiting: Vec<(MessageId, Option<BodyError>)> = page
                .items()
                .filter(|item| event.changed.contains(&item.id()))
                .filter_map(|item| match item.card.as_ref().map(|card| &card.body) {
                    Some(ShownBody::Downloading) => Some((item.id(), None)),
                    Some(ShownBody::DownloadFailed(error)) => Some((item.id(), Some(*error))),
                    _ => None,
                })
                .collect();
            (page.key, touches(page), waiting)
        });
        let Some((key, touched, waiting)) = shown else {
            return;
        };
        for (id, failure) in waiting {
            if let Some(error) = failure {
                self.answers.borrow_mut().insert(id, Err(error));
            }
            self.reload_card(key, id);
        }
        if touched && !self.refresh_queued.replace(true) {
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.refresh(key).await });
        }
    }

    /// The engine answered a download: the card waiting for it loads again with the answer.
    pub(super) fn body_answered(self: &Rc<Self>, id: MessageId, result: Result<(), BodyError>) {
        let key = self.shown.borrow().as_ref().and_then(|page| {
            page.item(id)
                .filter(|item| {
                    item.card
                        .as_ref()
                        .is_some_and(|card| card.body == ShownBody::Downloading)
                })
                .map(|_| page.key)
        });
        if let Some(key) = key {
            self.answers.borrow_mut().insert(id, result);
            self.reload_card(key, id);
        }
    }

    /// Reads the shown conversation again and changes its page in place: messages gone are
    /// taken out, new ones put in their place as rows (unread until the user opens them),
    /// changed flags shown. The bodies already shown stay, and so does what the column shows.
    async fn refresh(self: Rc<Self>, key: u64) {
        let selected = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| page.key == key)
            .map(|page| page.selected);
        let reply = selected.and_then(|selected| self.store.conversation(selected));
        let conversation = match reply {
            Some(reply) => reply
                .recv()
                .await
                .ok()
                .and_then(|versioned| versioned.value),
            None => None,
        };
        self.refresh_queued.set(false);
        let Some(conversation) = conversation else {
            return;
        };
        if !self.is_shown(key) {
            return;
        }
        self.hold_scroll();
        let mut added = Vec::new();
        let mut removed = Vec::new();
        {
            let mut shown = self.shown.borrow_mut();
            let Some(page) = shown.as_mut() else {
                return;
            };
            let wanted: HashSet<MessageId> = conversation
                .messages
                .iter()
                .map(|message| message.summary.id)
                .collect();
            let mut kept: HashSet<MessageId> = HashSet::new();
            let mut index = 0;
            while index < page.entries.len() {
                let gone = match &mut page.entries[index] {
                    Entry::Message(item) => !wanted.contains(&item.id()),
                    Entry::Fold(fold) => {
                        fold.messages
                            .retain(|message| wanted.contains(&message.summary.id));
                        fold.messages.is_empty()
                    }
                };
                if gone {
                    match page.entries.remove(index) {
                        Entry::Message(item) => removed.push(*item),
                        Entry::Fold(fold) => page.list.remove(&fold.row),
                    }
                    continue;
                }
                match &page.entries[index] {
                    Entry::Message(item) => {
                        kept.insert(item.id());
                    }
                    Entry::Fold(fold) => {
                        kept.extend(fold.messages.iter().map(|message| message.summary.id));
                    }
                }
                index += 1;
            }
            for message in &conversation.messages {
                let id = message.summary.id;
                if !kept.contains(&id) {
                    added.push(message.clone());
                    continue;
                }
                for entry in &mut page.entries {
                    match entry {
                        Entry::Message(item) if item.id() == id => {
                            update_item(item, message);
                        }
                        Entry::Fold(fold) => {
                            for folded in &mut fold.messages {
                                if folded.summary.id == id {
                                    *folded = message.clone();
                                }
                            }
                        }
                        Entry::Message(_) => {}
                    }
                }
            }
            for entry in &page.entries {
                if let Entry::Fold(fold) = entry {
                    update_fold(fold);
                }
            }
            page.members = conversation.members().collect();
            page.thread = Some(conversation.thread);
        }
        for item in removed {
            self.remove_item(key, item);
        }
        for message in added {
            let item = self.item(key, &message, false);
            self.with_page(key, |page| {
                let order =
                    |message: &ConversationMessage| (message.summary.date, message.summary.id);
                let at = page
                    .entries
                    .iter()
                    .position(|entry| entry_order(entry) > order(&message))
                    .unwrap_or(page.entries.len());
                let before = at.checked_sub(1).map(|at| entry_widget(&page.entries[at]));
                page.place(&item.slot, before.as_ref());
                page.entries.insert(at, Entry::Message(Box::new(item)));
            });
        }
        tracing::debug!("reader: the shown conversation changed");
        self.size_changed();
        self.retarget_quick_reply(key);
        self.schedule();
    }

    /// Takes `item` off page `key`: a focus in it goes to the selected message, its view back
    /// to the pool.
    fn remove_item(&self, key: u64, mut item: Item) {
        let target = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| page.key == key)
            .and_then(|page| page.selected_card())
            .and_then(|card| card.focus_target());
        widgets::hand_over_focus(&item.slot, target.as_ref());
        if let Some(parent) = item.slot.parent().and_downcast::<gtk::Box>() {
            parent.remove(&item.slot);
        }
        if let Some(view) = item
            .card
            .as_mut()
            .and_then(|card| card.html.as_mut())
            .and_then(|html| html.view.take())
        {
            self.engine.release(view);
        }
    }
}

/// The widget an entry puts in the page's list.
pub(super) fn entry_widget(entry: &Entry) -> gtk::Widget {
    match entry {
        Entry::Message(item) => item.slot.clone().upcast(),
        Entry::Fold(fold) => fold.row.clone().upcast(),
    }
}

/// Where an entry sorts: its (first) message's date and id.
fn entry_order(entry: &Entry) -> (i64, MessageId) {
    match entry {
        Entry::Message(item) => (item.message.summary.date, item.id()),
        Entry::Fold(fold) => fold
            .messages
            .first()
            .map_or((i64::MAX, MessageId(i64::MAX)), |message| {
                (message.summary.date, message.summary.id)
            }),
    }
}

/// A message read again: its row shows its new flags; its card keeps its body.
fn update_item(item: &mut Item, message: &ConversationMessage) {
    let unread_before = !item.message.summary.flags.is_seen();
    let unread_now = !message.summary.flags.is_seen();
    item.message = message.clone();
    if unread_before != unread_now {
        fill_row(&item.row, &message.summary, super::now());
    }
}

/// A fold's count after a re-read.
pub(super) fn update_fold(fold: &Fold) {
    let count = fold.messages.len();
    if let Some(label) = find_label(fold.row.upcast_ref()) {
        label.set_label(&conversation::fold_text(count));
    }
    fold.row
        .update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, folded; show them",
            conversation::fold_text(count)
        ))]);
}

pub(super) fn find_label(widget: &gtk::Widget) -> Option<gtk::Label> {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        return Some(label.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_label(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}
