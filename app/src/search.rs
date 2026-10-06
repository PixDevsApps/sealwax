//! Search beyond the list's field (ML-081, ML-082): Rebuild Search Index. The search field
//! itself lives in the list (`list::MessageList`).
//!
//! Rebuild Search Index asks the store to index every stored message again, off the GTK
//! thread; search keeps answering from the old index meanwhile. A toast says it runs, and
//! another what it did; the command is unavailable until it is done. Search results on screen
//! are read again afterwards. The log holds counts and times only.

use crate::application::AppState;
use gtk4::glib;
use libadwaita as adw;
use sealwax_domain::actions::ActionId;
use sealwax_infrastructure::store::IndexRebuilt;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// The toast while the index is rebuilt.
pub const REBUILDING_TEXT: &str = "Rebuilding the search index…";
/// The toast when it failed.
pub const FAILED_TEXT: &str = "The search index couldn't be rebuilt";
/// How long the result's toast shows.
pub const RESULT_SHOWN: Duration = Duration::from_secs(5);

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    /// The self-test's hold: a rebuild waits until the sender is dropped or sends.
    static HOLD: RefCell<Option<async_channel::Receiver<()>>> = const { RefCell::new(None) };
}

/// Installs the search commands, once at startup.
pub fn install(state: &Rc<AppState>) {
    let Some(actions) = state.app_actions() else {
        return;
    };
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::RebuildSearchIndex, move || {
        if let Some(state) = weak.upgrade() {
            rebuild(&state);
        }
    });
}

/// Whether a rebuild runs now.
pub fn is_running() -> bool {
    RUNNING.with(Cell::get)
}

/// The self-test's hold: the next rebuilds wait (with their toast shown) until the returned
/// sender sends or is dropped.
pub fn hold() -> async_channel::Sender<()> {
    let (sender, receiver) = async_channel::bounded(1);
    HOLD.with(|hold| hold.replace(Some(receiver)));
    sender
}

/// The result's toast: "Search index rebuilt: 1,234 messages in 2.5 s".
pub fn result_text(rebuilt: &IndexRebuilt) -> String {
    let messages = match rebuilt.messages {
        1 => "1 message".to_owned(),
        count => format!("{} messages", thousands(count)),
    };
    format!(
        "Search index rebuilt: {messages} in {:.1} s",
        rebuilt.elapsed.as_secs_f64()
    )
}

fn thousands(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn set_available(state: &AppState, available: bool) {
    if let Some(actions) = state.app_actions() {
        actions.set_available(ActionId::RebuildSearchIndex, available);
    }
}

fn rebuild(state: &Rc<AppState>) {
    if RUNNING.with(|running| running.replace(true)) {
        return;
    }
    set_available(state, false);
    let toast = adw::Toast::builder()
        .title(REBUILDING_TEXT)
        .use_markup(false)
        .timeout(0)
        .build();
    state.show_toast(&toast, false);
    tracing::info!("rebuilding the search index");
    let weak = Rc::downgrade(state);
    let store = state.mailbox().shared_store();
    let hold = HOLD.with(|hold| hold.borrow().clone());
    glib::spawn_future_local(async move {
        if let Some(hold) = hold {
            let _ = hold.recv().await;
        }
        let result = match store {
            Some(store) => store.rebuild_index().recv().await.ok(),
            None => None,
        };
        RUNNING.with(|running| running.set(false));
        toast.dismiss();
        let Some(state) = weak.upgrade() else {
            return;
        };
        set_available(&state, true);
        let text = match &result {
            Some(rebuilt) => {
                tracing::info!(
                    messages = rebuilt.messages,
                    elapsed_ms = rebuilt.elapsed.as_millis() as u64,
                    "search index rebuilt"
                );
                if let Some(list) = state.list() {
                    list.search_again();
                }
                result_text(rebuilt)
            }
            None => {
                tracing::warn!("the search index could not be rebuilt");
                FAILED_TEXT.to_owned()
            }
        };
        let toast = adw::Toast::builder()
            .title(text)
            .use_markup(false)
            .timeout(RESULT_SHOWN.as_secs() as u32)
            .build();
        state.show_toast(&toast, false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_counts_messages_and_seconds() {
        let rebuilt = |messages, millis| IndexRebuilt {
            messages,
            elapsed: Duration::from_millis(millis),
        };
        assert_eq!(
            result_text(&rebuilt(100_000, 41_260)),
            "Search index rebuilt: 100,000 messages in 41.3 s"
        );
        assert_eq!(
            result_text(&rebuilt(1, 40)),
            "Search index rebuilt: 1 message in 0.0 s"
        );
        assert_eq!(
            result_text(&rebuilt(1_234, 2_500)),
            "Search index rebuilt: 1,234 messages in 2.5 s"
        );
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }
}
