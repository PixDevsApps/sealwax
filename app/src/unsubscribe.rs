//! Unsubscribe (issue #14, ADR-023): the selected messages' lists, one row each in a dialog that
//! says how Sealwax will leave each one, and nothing sent until the user says Unsubscribe. A
//! one-click list gets RFC 8058's POST from the engine; an email list a message from the account
//! the mail came to, through the outbox (Undo send applies); a list with only a web page gets a
//! button that opens it in the browser. Each one done is remembered, and the dialog says so the
//! next time. The lists' addresses never reach a log.

use crate::actions::ActionSet;
use crate::application::AppState;
use crate::composer::store_backend::{UNDO_SEND_MS, message_id, now_ms, senders};
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_domain::actions::ActionId;
use sealwax_domain::compose;
use sealwax_domain::links;
use sealwax_domain::model::MessageId;
use sealwax_domain::unsubscribe::{self, Method, Methods};
use sealwax_engine::api::{Command, Event, UnsubscribeError};
use sealwax_infrastructure::store::{DraftSaved, SaveDraft, SendQueued, UnsubscribeTarget};
use std::rc::Rc;

const DIALOG: &str = "unsubscribe-dialog";

pub fn install_window(state: &Rc<AppState>, actions: &ActionSet) {
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::Unsubscribe, move || {
        if let Some(state) = weak.upgrade() {
            start(&state);
        }
    });
}

/// Whether Unsubscribe applies to the selected messages, or why not.
pub fn applies(state: &AppState) -> Result<(), &'static str> {
    let list = state.list().ok_or("Select a message")?;
    let positions = list.selected_positions();
    if positions.is_empty() {
        return Err("Select a message");
    }
    let mut loaded = false;
    for position in positions {
        match list.model().row_at(position) {
            Some(row) if row.subscription => return Ok(()),
            Some(_) => loaded = true,
            None => {}
        }
    }
    Err(if loaded {
        "The selected messages have no unsubscribe link"
    } else {
        "Some selected messages haven't loaded"
    })
}

fn selected(state: &AppState) -> Vec<MessageId> {
    let Some(list) = state.list() else {
        return Vec::new();
    };
    list.selected_positions()
        .into_iter()
        .filter_map(|position| list.model().row_at(position))
        .filter(|row| row.subscription)
        .map(|row| row.id)
        .collect()
}

fn start(state: &Rc<AppState>) {
    let ids = selected(state);
    let Some(reply) = state
        .mailbox()
        .store()
        .map(|store| store.unsubscribe_targets(ids))
    else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let targets = reply.recv().await.unwrap_or_default();
        if let Some(state) = weak.upgrade()
            && !targets.is_empty()
        {
            open(&state, targets);
        }
    });
}

/// One list as the dialog shows it.
struct Row {
    target: UnsubscribeTarget,
    methods: Methods,
}

impl Row {
    fn name(&self) -> String {
        let name = self.target.sender_name.trim();
        if name.is_empty() {
            self.target.sender_email.clone()
        } else {
            name.to_owned()
        }
    }

    /// How Unsubscribe leaves this list, as the dialog says it.
    fn how(&self) -> String {
        let host = |uri: &str| {
            links::target(uri)
                .map(|target| links::display_host(&target.host))
                .unwrap_or_else(|| "the sender".to_owned())
        };
        match self.methods.best() {
            Some(Method::OneClick) => format!(
                "One click: Sealwax tells {}",
                host(self.methods.one_click.as_deref().unwrap_or_default())
            ),
            Some(Method::Mail) => format!(
                "By email to {}",
                self.methods
                    .mailto
                    .as_ref()
                    .and_then(|mailto| mailto.to.first())
                    .map_or("the list", String::as_str)
            ),
            Some(Method::Web) => format!(
                "On {}'s page, in your browser",
                host(self.methods.web.as_deref().unwrap_or_default())
            ),
            None => "No way to unsubscribe that Sealwax can use".to_owned(),
        }
    }

    /// Unsubscribe in the dialog does it; a web page waits for its own button.
    fn in_app(&self) -> bool {
        matches!(self.methods.best(), Some(Method::OneClick | Method::Mail))
    }
}

fn open(state: &Rc<AppState>, targets: Vec<UnsubscribeTarget>) {
    let Some(window) = state.window() else {
        return;
    };
    let rows: Vec<Row> = targets
        .into_iter()
        .map(|target| Row {
            methods: unsubscribe::methods(&target.list_unsubscribe, target.one_click),
            target,
        })
        .collect();
    let heading = match rows.as_slice() {
        [row] => format!("Unsubscribe from {}?", row.name()),
        rows => format!("Unsubscribe from {} senders?", rows.len()),
    };
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(
            "Sealwax contacts each sender only when you choose Unsubscribe. \
             Mail you already have stays where it is.",
        )
        .close_response("cancel")
        .default_response("unsubscribe")
        .build();
    dialog.set_widget_name(DIALOG);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("unsubscribe", "Unsubscribe");
    dialog.set_response_appearance("unsubscribe", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("unsubscribe", rows.iter().any(Row::in_app));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.set_widget_name("unsubscribe-lists");
    let rows = Rc::new(rows);
    for (index, row) in rows.iter().enumerate() {
        let mut subtitle = row.how();
        if let Some((_, at)) = row.target.unsubscribed
            && let Ok(when) = glib::DateTime::from_unix_local(at)
            && let Ok(day) = when.format("%e %B %Y")
        {
            subtitle = format!("Unsubscribed on {}. {subtitle}", day.trim());
        }
        let item = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&row.name()))
            .subtitle(glib::markup_escape_text(&subtitle))
            .build();
        if row.methods.best() == Some(Method::Web)
            && let Some(uri) = row.methods.web.clone()
        {
            let button = gtk::Button::builder()
                .label("Open Page")
                .valign(gtk::Align::Center)
                .build();
            button.set_widget_name(&format!("unsubscribe-open-{index}"));
            let (weak, rows) = (Rc::downgrade(state), rows.clone());
            button.connect_clicked(move |button| {
                if let Some(state) = weak.upgrade() {
                    open_page(&state, &uri);
                    record(&state, &rows[index].target, Method::Web);
                    button.set_sensitive(false);
                }
            });
            item.add_suffix(&button);
        }
        list.append(&item);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(320)
        .child(&list)
        .build();
    dialog.set_extra_child(Some(&scroller));
    let weak = Rc::downgrade(state);
    dialog.connect_response(Some("unsubscribe"), move |_, _| {
        if let Some(state) = weak.upgrade() {
            run(&state, rows.clone());
        }
    });
    dialog.present(Some(window.window()));
}

fn open_page(state: &AppState, uri: &str) {
    let window = state.window().map(|window| window.window().clone());
    gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |result| {
        if result.is_err() {
            tracing::info!("an unsubscribe page didn't open");
        }
    });
}

fn record(state: &AppState, target: &UnsubscribeTarget, method: Method) {
    if let Some(store) = state.mailbox().store() {
        let _ = store.record_unsubscribed(
            target.account,
            target.key.clone(),
            method,
            glib::real_time() / 1_000_000,
        );
    }
}

/// Unsubscribe from every list the app can leave itself, then a toast with how it went.
fn run(state: &Rc<AppState>, rows: Rc<Vec<Row>>) {
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let (mut done, mut failed) = (0, 0);
        for row in rows.iter().filter(|row| row.in_app()) {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let outcome = match row.methods.best() {
                Some(Method::OneClick) => {
                    let uri = row.methods.one_click.clone().unwrap_or_default();
                    one_click(&state, uri).await.map(|()| Method::OneClick)
                }
                Some(Method::Mail) => by_mail(&state, row).await.map(|()| Method::Mail),
                _ => continue,
            };
            match outcome {
                Ok(method) => {
                    record(&state, &row.target, method);
                    done += 1;
                }
                Err(why) => {
                    tracing::info!(why, "an unsubscribe didn't go through");
                    failed += 1;
                }
            }
        }
        if let Some(state) = weak.upgrade() {
            let text = match (done, failed) {
                (1, 0) => "Unsubscribed".to_owned(),
                (done, 0) => format!("Unsubscribed from {done} senders"),
                (0, _) => {
                    "Not unsubscribed: the sender didn't confirm it. Try again later.".to_owned()
                }
                (done, failed) => format!(
                    "Unsubscribed from {done}; {failed} didn't confirm it. Try those again later."
                ),
            };
            state.show_toast(&adw::Toast::new(&text), false);
        }
    });
}

async fn one_click(state: &AppState, uri: String) -> Result<(), &'static str> {
    let engine = state.engine();
    let request = engine.next_request();
    let (sender, answers) = async_channel::bounded(1);
    let _subscription = engine.subscribe(move |event| {
        if let Event::OneClickUnsubscribed {
            request: answered,
            result,
        } = event
            && *answered == request
        {
            let _ = sender.try_send(*result);
        }
    });
    engine
        .send(Command::OneClickUnsubscribe { request, uri })
        .map_err(|_| "no engine")?;
    match answers.recv().await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(UnsubscribeError::Refused)) => Err("refused"),
        Ok(Err(UnsubscribeError::NotConfirmed)) => Err("not confirmed"),
        Ok(Err(_)) => Err("not reached"),
        Err(_) => Err("no answer"),
    }
}

/// The `mailto:` method as a message from the list's account, queued as the composer queues a
/// send, so Undo send and the outbox's retries apply.
async fn by_mail(state: &AppState, row: &Row) -> Result<(), &'static str> {
    let mailto = row.methods.mailto.as_ref().ok_or("no address")?;
    let account = state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|account| account.id == row.target.account)
        .ok_or("no account")?;
    let settings = state
        .preferences()
        .map(|prefs| prefs.accounts())
        .unwrap_or_default();
    let sender = senders(&account, &settings)
        .into_iter()
        .next()
        .ok_or("no sender")?;
    let mut draft = compose::new_draft(account.id, &sender, message_id(&sender.address));
    let only = sealwax_domain::mailto::Mailto {
        to: mailto.to.clone(),
        subject: Some(
            mailto
                .subject
                .clone()
                .filter(|subject| !subject.trim().is_empty())
                .unwrap_or_else(|| "unsubscribe".to_owned()),
        ),
        body: mailto.body.clone(),
        ..Default::default()
    };
    only.fill(&mut draft);
    let store = state.mailbox().shared_store().ok_or("no store")?;
    let saved = store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: now_ms(),
        })
        .recv()
        .await
        .map_err(|_| "not saved")?;
    let DraftSaved::Saved { id, .. } = saved else {
        return Err("not saved");
    };
    match store
        .queue_send(id, now_ms() + UNDO_SEND_MS)
        .recv()
        .await
        .map_err(|_| "not queued")?
    {
        SendQueued::Queued { account, .. } => {
            let _ = state.engine().send(Command::OutboxQueued { account });
            crate::outbox::queued(state, id, account);
            Ok(())
        }
        _ => Err("not queued"),
    }
}
