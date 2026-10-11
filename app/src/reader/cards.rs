//! An expanded message's card: the message read off the GTK thread, its body chosen and
//! prepared (`prepare`), then the card built with its header, banner, body and attachment cards
//! and swapped in for the message's row. A click on a row expands it, a click on a card's header
//! collapses it (the only expanded message stays), "N earlier messages" reveals a step, and
//! Expand All and Collapse All act on the whole conversation. Each HTML body gets a view from
//! [`super::live`]; its events arrive in [`Inner::body_event`].

use super::conversation::{Card, Entry, HtmlBody, Item, reveal};
use super::page::{self, Header};
use super::prepare::{self, Body, Prepared};
use super::remote::Notice;
use super::{
    Inner, Reader, ShownBody, ViewEvent, download_failure, now, plain, protection, rgba,
    stylesheet, widgets,
};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use sealwax_domain::model::{FolderRole, MessageDetail, MessageId};
use sealwax_domain::theme::BodyStyle;
use sealwax_engine::api::BodyError;
use std::rc::Rc;

impl Inner {
    /// Whether load `token` of message `id` is still wanted on page `key`.
    fn wanted_load(&self, key: u64, id: MessageId, token: u64) -> bool {
        self.with_page(key, |page| {
            page.item(id)
                .is_some_and(|item| item.expanded && item.load == token)
        })
        .unwrap_or(false)
    }

    /// Loads message `id` of page `key` again (its body arrived, or Retry).
    pub(super) fn reload_card(self: &Rc<Self>, key: u64, id: MessageId) {
        let token = self.next_token();
        let wanted = self
            .with_page(key, |page| {
                page.item_mut(id).is_some_and(|item| {
                    item.expanded = true;
                    item.load = token;
                    true
                })
            })
            .unwrap_or(false);
        if wanted {
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.load_card(key, id, token).await });
        }
    }

    /// Reads message `id` and builds its card on page `key`, in place of its row or of the card
    /// it had. A newer load of the message, or a collapse, drops this one.
    pub(super) async fn load_card(self: Rc<Self>, key: u64, id: MessageId, token: u64) {
        let Some(reads) = self.store.read(id) else {
            self.install_missing(key, id, token);
            return;
        };
        let detail = reads.detail.recv().await;
        let accounts = reads.accounts.recv().await;
        let raw = reads.raw.recv().await.ok().flatten();
        if !self.wanted_load(key, id, token) {
            return;
        }
        let detail = match detail {
            Ok(versioned) => versioned.value,
            Err(error) => {
                tracing::warn!(message = id.0, %error, "reader: the message couldn't be read");
                None
            }
        };
        let Some(detail) = detail else {
            self.install_missing(key, id, token);
            return;
        };
        let mine: Vec<String> = accounts
            .map(|accounts| {
                accounts
                    .value
                    .into_iter()
                    .map(|account| account.address)
                    .collect()
            })
            .unwrap_or_default();
        if !mine.is_empty() {
            self.mine
                .replace(mine.iter().map(|address| address.to_lowercase()).collect());
        }
        let answer = self.answers.borrow_mut().remove(&id);
        let download = match &raw {
            None => self.download_state(answer),
            Some(_) => None,
        };
        let for_worker = detail.clone();
        let prepared = gio::spawn_blocking(move || prepare::prepare(&for_worker, raw.as_deref()))
            .await
            .unwrap_or_else(|_| {
                tracing::error!(message = id.0, "reader: preparing the message failed");
                Prepared {
                    body: Body::Empty,
                    attachments: Vec::new(),
                    signed: None,
                }
            });
        if !self.wanted_load(key, id, token) {
            return;
        }
        let header = Header {
            detail: &detail,
            mine: &mine,
            now: now(),
        };
        let sender = detail
            .headers
            .from
            .first()
            .map(|from| from.email.to_lowercase())
            .unwrap_or_default();
        let remote_images = matches!(
            &prepared.body,
            Body::Html {
                remote_images: true,
                ..
            }
        );
        let reader = Reader {
            inner: self.clone(),
        };
        let notice = self.remote.notice(id, &sender, remote_images);
        let widgets = page::card(&reader, &header, &prepared.attachments, notice);
        let draft = self
            .with_page(key, |page| {
                page.item(id)
                    .is_some_and(|item| item.message.role == FolderRole::Drafts)
            })
            .unwrap_or(false);
        let replies = self.reply_buttons(&widgets.header, id, draft);
        let weak = Rc::downgrade(&self);
        widgets.header.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && inner.is_shown(key)
            {
                inner.collapse(id);
            }
        });
        if let Some(banner) = &widgets.banner {
            let weak = Rc::downgrade(&self);
            banner.show_images.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.allow_remote(id);
                }
            });
        }
        let mut card = Card {
            widgets,
            body: ShownBody::Empty,
            html: None,
            attachments: prepared.attachments.len(),
            sender,
            remote_images,
            replies,
        };
        let collapse_quotes = self
            .with_page(key, |page| page.has_earlier(id))
            .unwrap_or(false);
        match download {
            Some(state) => self.download_body(&mut card, &detail, state, key, id),
            None => {
                self.fill_body(&mut card, id, prepared, collapse_quotes)
                    .await;
            }
        }
        if !self.wanted_load(key, id, token) {
            return;
        }
        self.install(key, id, card);
    }

    /// What a message whose body isn't stored shows (ML-044): `None` without a fetcher (M1's
    /// "not downloaded yet"), else the engine's failure, or "Downloading…" (the reader asks for
    /// it once the card is on its page).
    fn download_state(&self, answer: Option<Result<(), BodyError>>) -> Option<ShownBody> {
        self.fetcher.borrow().as_ref()?;
        Some(match answer {
            Some(Err(error)) => ShownBody::DownloadFailed(error),
            // Answered, and still no body: asking again would only loop.
            Some(Ok(())) => ShownBody::DownloadFailed(BodyError::Other),
            None => ShownBody::Downloading,
        })
    }

    /// The snippet under a quiet "Downloading…", or under why the download failed with Retry.
    fn download_body(
        self: &Rc<Self>,
        card: &mut Card,
        detail: &MessageDetail,
        state: ShownBody,
        key: u64,
        id: MessageId,
    ) {
        if let ShownBody::DownloadFailed(error) = &state {
            let row = gtk::Box::builder().spacing(12).build();
            let notice = page::notice(download_failure(*error));
            notice.set_hexpand(true);
            row.append(&notice);
            let retry = gtk::Button::builder()
                .label("Retry")
                .valign(gtk::Align::Center)
                .css_classes(["sw-control"])
                .build();
            retry.set_widget_name("reader-retry");
            let weak = Rc::downgrade(self);
            retry.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.answers.borrow_mut().remove(&id);
                    inner.reload_card(key, id);
                }
            });
            row.append(&retry);
            card.widgets.body.append(&row);
        } else {
            card.widgets.body.append(&page::notice("Downloading…"));
        }
        let snippet = &detail.summary.snippet;
        if !snippet.is_empty() {
            let snippet = plain::plain_text(snippet);
            card.widgets.body.append(&snippet);
            card.widgets.focus = Some(snippet.upcast());
        }
        card.body = state;
    }

    /// The card's body, where the reader builds every body: plain text in a text view, HTML in
    /// a frame whose view the live views give it, or a notice (an encrypted message's among
    /// them), under the quiet line of a signed message. With `collapse_quotes` (an earlier
    /// message is on the page) the quoted history collapses behind "…".
    async fn fill_body(
        self: &Rc<Self>,
        card: &mut Card,
        id: MessageId,
        prepared: Prepared,
        collapse_quotes: bool,
    ) {
        if let Some(signature) = prepared.signed {
            card.widgets
                .body
                .append(&protection::signed_note(signature));
        }
        match prepared.body {
            Body::Html {
                html,
                style,
                remote_images,
                parts,
            } => {
                let crashes = self.crashed.borrow().get(&id).copied().unwrap_or(0);
                if crashes >= 2 || self.filter.borrow().is_none() {
                    self.text_fallback(card, &html).await;
                    return;
                }
                let frame = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .overflow(gtk::Overflow::Hidden)
                    .css_classes([match style {
                        BodyStyle::Paper => "sw-paper",
                        BodyStyle::Themed => "sw-themed-body",
                    }])
                    .build();
                frame.set_widget_name("reader-frame");
                let holder = widgets::BodyBox::empty();
                holder.set_placeholder(1);
                frame.append(&holder);
                card.widgets.body.append(&frame);
                let remote_allowed = self.remote.allows(id, &card.sender);
                card.body = ShownBody::Html {
                    style,
                    remote_images,
                    remote_allowed,
                    inline_parts: parts.len(),
                };
                card.html = Some(HtmlBody {
                    frame,
                    holder,
                    view: None,
                    style,
                    html: Rc::from(html),
                    parts: Rc::new(parts),
                    remote_images,
                    remote_allowed,
                    collapse_quotes,
                    measured: None,
                    loaded: false,
                    painted: false,
                    released: false,
                    token: 0,
                });
            }
            Body::Plain(text) => {
                let text = plain::message_text(&text, &self.links, &self.theme, collapse_quotes);
                card.widgets.body.append(&text);
                card.widgets.focus = Some(text.upcast());
                card.body = ShownBody::Plain;
            }
            Body::Unreadable(unreadable) => {
                card.widgets.body.append(&protection::notice(unreadable));
                card.body = ShownBody::Unreadable(unreadable);
            }
            Body::Empty => {
                card.widgets
                    .body
                    .append(&page::notice("This message has no text."));
                card.body = ShownBody::Empty;
            }
            Body::NotDownloaded => {
                card.widgets.body.append(&page::notice(
                    "The rest of this message hasn't been downloaded yet.",
                ));
                card.body = ShownBody::NotDownloaded;
            }
        }
    }

    /// The HTML as text, when no WebView may show it.
    async fn text_fallback(&self, card: &mut Card, html: &str) {
        let html = html.to_owned();
        let text = gio::spawn_blocking(move || sealwax_domain::message::html_to_text(&html))
            .await
            .unwrap_or_default();
        card.widgets.body.append(&page::notice(
            "This message is shown as text: its formatted view isn't available.",
        ));
        let text = plain::plain_text(&text);
        card.widgets.body.append(&text);
        card.widgets.focus = Some(text.upcast());
        card.body = ShownBody::TextFallback;
    }

    /// A message the store no longer holds keeps its row, with a notice under it.
    fn install_missing(self: &Rc<Self>, key: u64, id: MessageId, token: u64) {
        if !self.wanted_load(key, id, token) {
            return;
        }
        tracing::debug!(
            message = id.0,
            "reader: a message of the conversation is gone"
        );
        let notice = page::notice("This message is no longer here.");
        self.with_page(key, |page| {
            if let Some(item) = page.item_mut(id) {
                item.expanded = false;
                if item.row.parent().is_none() {
                    item.slot.append(&item.row);
                }
                item.slot.append(&notice);
            }
        });
    }

    /// Puts `card` on page `key` in place of the message's row or earlier card; a focus held
    /// there moves into the new card.
    fn install(self: &Rc<Self>, key: u64, id: MessageId, card: Card) {
        let shown = self.is_shown(key);
        if shown {
            self.hold_scroll();
        }
        let target: gtk::Widget = card.widgets.header.clone().upcast();
        let old = self.with_page(key, |page| {
            let item = page.item_mut(id)?;
            let mut old_widgets = Vec::new();
            let mut child = item.slot.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                old_widgets.push(current);
            }
            item.slot.append(&card.widgets.root);
            for old in old_widgets {
                widgets::hand_over_focus(&old, Some(&target));
                item.slot.remove(&old);
            }
            item.card
                .replace(card)
                .and_then(|mut old| old.html.as_mut()?.view.take())
        });
        if let Some(Some(view)) = old {
            self.engine.release(view);
        }
        if shown {
            self.request_bodies(key);
            self.report_opened(key);
        }
        self.schedule();
        if shown && self.with_page(key, |page| page.selected == id) == Some(true) {
            self.remote.changed();
        }
    }

    /// Expands message `id` of the shown conversation: its card replaces its row once read.
    pub(super) fn expand(self: &Rc<Self>, id: MessageId) {
        let token = self.next_token();
        let key = self.shown.borrow_mut().as_mut().and_then(|page| {
            let item = page.item_mut(id).filter(|item| !item.expanded)?;
            item.expanded = true;
            item.load = token;
            Some(page.key)
        });
        if let Some(key) = key {
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.load_card(key, id, token).await });
        }
    }

    /// Collapses message `id` of the shown conversation to its row, unless it is the only
    /// message expanded.
    pub(super) fn collapse(self: &Rc<Self>, id: MessageId) {
        let only = self
            .shown
            .borrow()
            .as_ref()
            .is_none_or(|page| page.expanded().len() <= 1);
        if only {
            return;
        }
        self.hold_scroll();
        let view = self.shown.borrow_mut().as_mut().and_then(|page| {
            let item = page.item_mut(id).filter(|item| item.expanded)?;
            collapse_item(item)
        });
        if let Some(view) = view {
            self.engine.release(view);
        }
        self.schedule();
    }

    /// Expand All: every message shown as a row expands; folded ones stay folded.
    pub(super) fn expand_all(self: &Rc<Self>) {
        let ids: Vec<MessageId> = self
            .shown
            .borrow()
            .as_ref()
            .map(|page| {
                page.items()
                    .filter(|item| !item.expanded)
                    .map(Item::id)
                    .collect()
            })
            .unwrap_or_default();
        self.hold_scroll();
        for id in ids {
            self.expand(id);
        }
    }

    /// Next (`forward`) or Previous Message in Conversation: the message after or before the one
    /// these last moved to (the selected one at first) expands, and the column scrolls to it.
    /// Folded messages are passed over.
    pub(super) fn step(self: &Rc<Self>, forward: bool) {
        let target = self.shown.borrow_mut().as_mut().and_then(|page| {
            let ids: Vec<MessageId> = page.items().map(Item::id).collect();
            let current = page
                .current
                .filter(|id| ids.contains(id))
                .unwrap_or(page.selected);
            let at = ids.iter().position(|id| *id == current)?;
            // Next goes down the screen: to an older message when the newest is on top.
            let next = if forward != page.newest_first {
                ids.get(at + 1).copied()
            } else {
                at.checked_sub(1).map(|at| ids[at])
            }?;
            page.current = Some(next);
            Some(next)
        });
        if let Some(id) = target {
            self.expand(id);
            self.scroll_to_message(id);
        }
    }

    /// Collapse All: every message but the selected one becomes a row.
    pub(super) fn collapse_all(self: &Rc<Self>) {
        let Some(selected) = self.shown.borrow().as_ref().map(|page| page.selected) else {
            return;
        };
        self.expand(selected);
        self.hold_scroll();
        let views: Vec<_> = self
            .shown
            .borrow_mut()
            .as_mut()
            .map(|page| {
                page.items_mut()
                    .filter(|item| item.expanded && item.id() != selected)
                    .filter_map(collapse_item)
                    .collect()
            })
            .unwrap_or_default();
        for view in views {
            self.engine.release(view);
        }
        self.schedule();
    }

    /// Reveals one step of the fold that starts with message `first`: its newest messages, as
    /// rows under it; the fold goes once it is empty.
    pub(super) fn unfold(self: &Rc<Self>, first: MessageId) {
        let Some(key) = self.shown.borrow().as_ref().map(|page| page.key) else {
            return;
        };
        let taken = self.with_page(key, |page| {
            let index = page.entries.iter().position(|entry| {
                matches!(entry, Entry::Fold(fold)
                    if fold.messages.first().is_some_and(|message| message.summary.id == first))
            })?;
            let Entry::Fold(fold) = &mut page.entries[index] else {
                return None;
            };
            let range = reveal(0..fold.messages.len());
            let revealed: Vec<_> = fold.messages.drain(range).collect();
            Some((index, revealed))
        });
        let Some(Some((index, revealed))) = taken else {
            return;
        };
        let items: Vec<Item> = revealed
            .iter()
            .map(|message| self.item(key, message, false))
            .collect();
        self.with_page(key, |page| {
            let Entry::Fold(fold) = &page.entries[index] else {
                return;
            };
            let mut after: gtk::Widget = fold.row.clone().upcast();
            let empty = fold.messages.is_empty();
            for item in &items {
                page.place(&item.slot, Some(&after));
                after = item.slot.clone().upcast();
            }
            let rest = page.entries.split_off(index + 1);
            page.entries
                .extend(items.into_iter().map(|item| Entry::Message(Box::new(item))));
            page.entries.extend(rest);
            if empty {
                if let Entry::Fold(fold) = page.entries.remove(index) {
                    let first_row = page.entries.get(index).map(super::show::entry_widget);
                    widgets::hand_over_focus(&fold.row, first_row.as_ref());
                    page.list.remove(&fold.row);
                }
            } else if let Entry::Fold(fold) = &page.entries[index] {
                super::show::update_fold(fold);
            }
        });
        self.size_changed();
        self.schedule();
    }

    /// Reveals a step of the shown conversation's first fold.
    pub(super) fn unfold_first(self: &Rc<Self>) {
        let first = self.shown.borrow().as_ref().and_then(|page| {
            page.entries.iter().find_map(|entry| match entry {
                Entry::Fold(fold) => fold.messages.first().map(|message| message.summary.id),
                Entry::Message(_) => None,
            })
        });
        if let Some(first) = first {
            self.unfold(first);
        }
    }

    /// Show images, for message `id`: allowed until Sealwax quits, in every page it is on.
    pub(super) fn allow_remote(self: &Rc<Self>, id: MessageId) {
        self.remote.allow_message(id);
        self.refresh_remote();
        tracing::debug!(
            message = id.0,
            "reader: remote images allowed for a message"
        );
    }

    /// Every expanded card follows the remote-content policy (ML-058) after Show images, a
    /// sender allowed or forgotten, or the list read: a card whose images are now allowed for
    /// it alone loses its banner, and its view, if it holds one, loads again without the
    /// content filter; a card whose sender joined or left the list is built again, with the
    /// sender's line or the banner. Collapsed messages take the policy when expanded.
    pub(super) fn refresh_remote(self: &Rc<Self>) {
        let theme = self.theme.theme();
        let mut rebuild = Vec::new();
        for pages in [&self.shown, &self.pending] {
            let mut pages = pages.borrow_mut();
            let Some(page) = pages.as_mut() else {
                continue;
            };
            let key = page.key;
            for item in page.items_mut() {
                let id = item.id();
                let Some(card) = item.card.as_mut() else {
                    continue;
                };
                let wanted = self.remote.notice(id, &card.sender, card.remote_images);
                if wanted == card.widgets.notice {
                    continue;
                }
                if !matches!(
                    (&card.widgets.notice, &wanted),
                    (Notice::Hidden(_), Notice::None)
                ) {
                    rebuild.push((key, id));
                    continue;
                }
                card.widgets.notice = wanted;
                if let Some(banner) = &card.widgets.banner
                    && banner.root.parent().is_some()
                {
                    widgets::hand_over_focus(&banner.root, card.focus_target().as_ref());
                    card.widgets.root.remove(&banner.root);
                }
                let Some(html) = card.html.as_mut().filter(|html| !html.remote_allowed) else {
                    continue;
                };
                html.remote_allowed = true;
                card.body = ShownBody::Html {
                    style: html.style,
                    remote_images: html.remote_images,
                    remote_allowed: true,
                    inline_parts: html.parts.len(),
                };
                if let Some(view) = &html.view {
                    html.loaded = false;
                    html.painted = false;
                    self.engine.set_parts(view.page_id(), html.parts.clone());
                    view.load(
                        &html.html,
                        &stylesheet(&theme, html.style),
                        &rgba(theme.webview_background(html.style)),
                        None,
                    );
                }
            }
        }
        for (key, id) in rebuild {
            self.reload_card(key, id);
        }
    }

    /// An event of the view that holds message `id`'s body on page `key`, from its load
    /// `token`: heights keep the body's place (and the column's scroll), loads and frames tell
    /// the layout wait, links open after vetting, a crash renders the body again once, then as
    /// text.
    pub(super) fn body_event(
        self: &Rc<Self>,
        key: u64,
        id: MessageId,
        token: u64,
        event: &ViewEvent,
    ) {
        let ours = |html: &HtmlBody| html.token == token && html.view.is_some();
        match event {
            ViewEvent::Height { height, width } => {
                let changed = self.with_page(key, |page| {
                    let html = page
                        .item_mut(id)?
                        .card
                        .as_mut()?
                        .html
                        .as_mut()
                        .filter(|html| ours(html))?;
                    let before = html.measured.map(|(height, _)| height);
                    html.measured = Some((*height, *width));
                    html.holder.set_placeholder(*height);
                    html.released = false;
                    Some(before != Some(*height))
                });
                if changed == Some(Some(true)) && self.is_shown(key) {
                    self.hold_scroll();
                }
                self.schedule();
            }
            ViewEvent::Loaded | ViewEvent::Frame => {
                let loaded = matches!(event, ViewEvent::Loaded);
                self.with_page(key, |page| {
                    if let Some(html) = page
                        .item_mut(id)
                        .and_then(|item| item.card.as_mut())
                        .and_then(|card| card.html.as_mut())
                        .filter(|html| ours(html))
                    {
                        if loaded {
                            html.loaded = true;
                        } else {
                            html.painted = true;
                        }
                    }
                });
            }
            ViewEvent::Link(link) => self.links.open(link),
            ViewEvent::Hover(hover) => {
                let view = self
                    .with_page(key, |page| {
                        page.item(id)?
                            .card
                            .as_ref()?
                            .html
                            .as_ref()
                            .filter(|html| ours(html))?
                            .view
                            .as_ref()
                            .map(|view| view.web_view().clone().upcast::<gtk::Widget>())
                    })
                    .flatten();
                if view.is_some() || hover.is_none() {
                    self.links.hover(view.as_ref(), hover.as_ref());
                }
            }
            ViewEvent::Margins(fitted) => {
                self.with_page(key, |page| {
                    if let Some(html) = page
                        .item(id)
                        .and_then(|item| item.card.as_ref())
                        .and_then(|card| card.html.as_ref())
                        .filter(|html| ours(html))
                    {
                        if *fitted {
                            html.frame.add_css_class("sw-paper-fitted");
                        } else {
                            html.frame.remove_css_class("sw-paper-fitted");
                        }
                    }
                });
            }
            ViewEvent::Quotes { .. } => {}
            ViewEvent::Refused(refused) => {
                tracing::debug!(?refused, "reader: refused what a message asked for");
            }
            ViewEvent::Crashed | ViewEvent::Failed => self.body_failed(key, id, token, event),
            ViewEvent::Finished | ViewEvent::Resource(_) => {}
        }
        let _ = self.progress.0.try_send(());
        self.view_event(id, event);
    }

    /// The view of message `id` crashed or its load failed: it goes back to the pool and the
    /// body loads again in another; after a second crash, or a failed load, the card is built
    /// again with the body as text.
    fn body_failed(self: &Rc<Self>, key: u64, id: MessageId, token: u64, event: &ViewEvent) {
        let view = self.with_page(key, |page| {
            let html = page
                .item_mut(id)?
                .card
                .as_mut()?
                .html
                .as_mut()
                .filter(|html| html.token == token)?;
            html.view.take()
        });
        let Some(Some(view)) = view else {
            return;
        };
        self.engine.release(view);
        let crashes = {
            let mut crashed = self.crashed.borrow_mut();
            let count = crashed.entry(id).or_default();
            *count += if matches!(event, ViewEvent::Crashed) {
                1
            } else {
                2
            };
            *count
        };
        tracing::warn!(message = id.0, crashes, "reader: a body's view failed");
        if crashes >= 2 {
            self.reload_card(key, id);
        } else {
            self.schedule();
        }
    }
}

/// Puts `item`'s row back in place of its card; a focus in the card goes to the row. Returns
/// the view the card's body held.
fn collapse_item(item: &mut Item) -> Option<super::view::MessageView> {
    item.expanded = false;
    let mut card = item.card.take()?;
    item.slot.append(&item.row);
    widgets::hand_over_focus(&card.widgets.root, Some(item.row.upcast_ref()));
    item.slot.remove(&card.widgets.root);
    card.html.as_mut().and_then(|html| html.view.take())
}
