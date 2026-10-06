//! Background mode (ML-084, PRD D6, ADR-013 and its M7 amendment): on by default, closing the
//! window keeps Sealwax running so sync and notifications go on, with a one-time notice the
//! first time; Quit stops it. Also Start Sealwax at Login (opt-in, asks first:
//! [`autostart`](crate::autostart)) and the engine started without a window
//! (`AppState::start_engine_without_window`).
//!
//! - **The hold** follows `[background] enabled` in `config.toml`, applied live: on, the
//!   application holds itself (`AppState::set_background`), so closing the last window leaves
//!   the process, the engine, IDLE and any open composers running; off, closing the last window
//!   quits, and turning it off while no window is open quits now, through Quit.
//! - **The notice:** the first time the last window closes into background mode, a desktop
//!   notification says that Sealwax keeps running and how to quit it. Once per user: a marker in
//!   Sealwax's state folder remembers it across restarts. Clicking it shows the main window; a
//!   window opening withdraws it. The texts are proposals for the M7 gate.

use crate::application::{AppState, ICON_NAME, SealwaxApplication};
use crate::autostart;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use sealwax_infrastructure::xdg::ensure_private_dir;
use std::cell::{Cell, RefCell};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// The notice's notification id; new-mail notifications use their own.
pub const NOTICE_ID: &str = "background-notice";

/// The marker in Sealwax's state folder that says the notice was shown.
pub const NOTICE_MARKER: &str = "background-notice-shown";

/// The application action a click on the notice runs: show the main window. Not a command of
/// the registry: nothing but a notification runs it.
pub const SHOW_WINDOW_ACTION: &str = "show-main-window";

/// Development builds only: closes the first main window this many milliseconds after its first
/// frame, as a user would, so a self-test can watch a Sealwax of its own close into background
/// mode.
pub const CLOSE_AFTER_ENV: &str = "SEALWAX_CLOSE_WINDOW_AFTER_MS";

/// A notice as sent: its id and texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub id: &'static str,
    pub title: String,
    pub body: String,
}

thread_local! {
    /// Every notice sent in this process, oldest first; the self-tests read it.
    static SENT: RefCell<Vec<Notice>> = const { RefCell::new(Vec::new()) };
}

/// The notices sent in this process, oldest first.
pub fn sent() -> Vec<Notice> {
    SENT.with(|sent| sent.borrow().clone())
}

/// The notice's texts, with the Quit key the user has (`keys.toml` can change it).
pub fn notice(keymap: &Keymap) -> Notice {
    let quit = match keymap.shown(ActionId::Quit).first() {
        Some(key) => format!("To quit, open Sealwax and press {}.", key.display()),
        None => "To quit, open Sealwax and choose Quit in its menu.".to_owned(),
    };
    Notice {
        id: NOTICE_ID,
        title: "Sealwax keeps running".to_owned(),
        body: format!(
            "Mail keeps syncing and new mail is still notified while the window is closed. \
             {quit} To quit whenever the window closes, set enabled = false under [background] \
             in config.toml."
        ),
    }
}

/// Notes in `state_dir` that the notice is shown; `Ok(true)` the first time ever, `Ok(false)`
/// once it was. Blocking.
pub fn remember_notice(state_dir: &Path) -> io::Result<bool> {
    ensure_private_dir(state_dir)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(state_dir.join(NOTICE_MARKER))
    {
        Ok(mut file) => {
            file.write_all(
                b"Sealwax showed its one-time notice about background mode. Delete this file to \
                  see it again.\n",
            )?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Follows `[background]`, installs the notice and Start Sealwax at Login, once at startup.
pub fn install(app: &SealwaxApplication, state: &Rc<AppState>) {
    follow_settings(app, state);
    install_notice(app, state);
    autostart::install(state);
    close_after_first_frame(app, state);
}

/// `[background] enabled` from `config.toml`; on until the file says otherwise.
fn enabled(state: &AppState) -> bool {
    state
        .preferences()
        .is_none_or(|prefs| prefs.config().background.enabled)
}

/// The hold follows `[background]`, now and whenever `config.toml` changes.
fn follow_settings(app: &SealwaxApplication, state: &Rc<AppState>) {
    apply(app, state);
    let Some(prefs) = state.preferences() else {
        return;
    };
    let (app, weak) = (app.downgrade(), Rc::downgrade(state));
    prefs.connect_changed(move |update| {
        if update.config_before.is_some()
            && let (Some(app), Some(state)) = (app.upgrade(), weak.upgrade())
        {
            apply(&app, &state);
        }
    });
}

fn apply(app: &SealwaxApplication, state: &Rc<AppState>) {
    let on = enabled(state);
    if on == state.is_background() || state.is_quitting() {
        return;
    }
    tracing::info!(on, "background mode");
    state.set_background(app, on);
    // Off with no window open: nothing keeps Sealwax but the hold just released, so it quits
    // the usual way (a `--background` start waits for its first window instead).
    if !on && app.windows().is_empty() && !state.is_hidden_start() {
        tracing::info!("background mode turned off with no window open; quitting");
        state.quit(app);
    }
}

fn install_notice(app: &SealwaxApplication, state: &Rc<AppState>) {
    let shown = Rc::new(Cell::new(false));
    let action = gio::SimpleAction::new(SHOW_WINDOW_ACTION, None);
    let (weak_app, weak) = (app.downgrade(), Rc::downgrade(state));
    action.connect_activate(move |_, _| {
        if let (Some(app), Some(state)) = (weak_app.upgrade(), weak.upgrade()) {
            state.show_main_window(&app);
        }
    });
    app.add_action(&action);
    // The last window closed: checked once the main loop turns, so a window that replaces it
    // at once (an activation) counts, and the windows Quit and shutdown destroy don't.
    let weak = Rc::downgrade(state);
    let sent = shown.clone();
    app.connect_window_removed(move |app, _| {
        let (app, weak, sent) = (app.downgrade(), weak.clone(), sent.clone());
        glib::idle_add_local_once(move || {
            if let (Some(app), Some(state)) = (app.upgrade(), weak.upgrade()) {
                closed_into_background(&app, &state, &sent);
            }
        });
    });
    app.connect_window_added(move |app, _| {
        if shown.get() {
            app.withdraw_notification(NOTICE_ID);
        }
    });
}

/// The last window closed: tell the user once, ever, that Sealwax runs on.
fn closed_into_background(app: &SealwaxApplication, state: &Rc<AppState>, sent: &Rc<Cell<bool>>) {
    if state.is_quitting() || !app.windows().is_empty() || !state.is_background() || !enabled(state)
    {
        return;
    }
    tracing::info!("the last window closed; running in the background");
    if sent.get() {
        return;
    }
    let dir = state.config().dirs.state.clone();
    let (app, state, sent) = (app.clone(), state.clone(), sent.clone());
    glib::spawn_future_local(async move {
        let first = match gio::spawn_blocking(move || remember_notice(&dir)).await {
            Ok(Ok(first)) => first,
            Ok(Err(error)) => {
                // Shown anyway, once in this process, rather than never.
                tracing::warn!(%error, "the background notice can't be remembered");
                true
            }
            Err(_) => true,
        };
        if !first || sent.replace(true) {
            tracing::debug!("the background notice was shown before");
            return;
        }
        let keymap = state
            .preferences()
            .map(|prefs| prefs.keymap())
            .unwrap_or_default();
        send(&app, notice(&keymap));
    });
}

fn send(app: &SealwaxApplication, notice: Notice) {
    let notification = gio::Notification::new(&notice.title);
    notification.set_body(Some(&notice.body));
    notification.set_icon(&gio::ThemedIcon::new(ICON_NAME));
    notification.set_default_action(&format!("app.{SHOW_WINDOW_ACTION}"));
    app.send_notification(Some(notice.id), &notification);
    tracing::info!("background notice sent");
    SENT.with(|sent| sent.borrow_mut().push(notice));
}

/// [`CLOSE_AFTER_ENV`], in development builds.
fn close_after_first_frame(app: &SealwaxApplication, state: &Rc<AppState>) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(delay) = std::env::var(CLOSE_AFTER_ENV)
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map(Duration::from_millis)
    else {
        return;
    };
    let (app, weak) = (app.downgrade(), Rc::downgrade(state));
    glib::timeout_add_local(Duration::from_millis(20), move || {
        let Some(state) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if state.first_frame().is_none() {
            return glib::ControlFlow::Continue;
        }
        let weak = Rc::downgrade(&state);
        let app = app.clone();
        glib::timeout_add_local_once(delay, move || {
            if let (Some(state), Some(_)) = (weak.upgrade(), app.upgrade())
                && let Some(window) = state.window()
            {
                tracing::info!("{CLOSE_AFTER_ENV} elapsed; closing the window");
                window.window().close();
            }
        });
        glib::ControlFlow::Break
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::keymap::Binding;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sealwax-background-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_notice_says_how_to_quit() {
        let text = notice(&Keymap::default());
        assert_eq!(text.id, NOTICE_ID);
        assert_eq!(text.title, "Sealwax keeps running");
        assert!(
            text.body
                .contains("To quit, open Sealwax and press Ctrl+Q."),
            "{}",
            text.body
        );
        assert!(text.body.contains("[background]"), "{}", text.body);
        let rebound = Keymap::with_overrides([(
            ActionId::Quit,
            vec![Binding::parse("<Control><Shift>w").unwrap()],
        )]);
        assert!(notice(&rebound).body.contains("press Ctrl+Shift+W."));
        let none = Keymap::with_overrides([(ActionId::Quit, Vec::new())]);
        assert!(
            notice(&none).body.contains("choose Quit in its menu."),
            "{}",
            notice(&none).body
        );
    }

    #[test]
    fn the_notice_is_remembered_once_across_restarts() {
        let dir = scratch("notice").join("state").join("sealwax");
        assert!(remember_notice(&dir).unwrap());
        assert!(!remember_notice(&dir).unwrap());
        // A restart reads the same folder: still shown.
        assert!(!remember_notice(&dir).unwrap());
        let marker = std::fs::read_to_string(dir.join(NOTICE_MARKER)).unwrap();
        assert!(marker.contains("Delete this file"), "{marker}");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap()).unwrap();
    }
}
