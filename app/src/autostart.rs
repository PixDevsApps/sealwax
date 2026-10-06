//! Start Sealwax at Login (ML-084, PRD D6's opt-in autostart): an XDG autostart entry,
//! `$XDG_CONFIG_HOME/autostart/<app-id>.desktop`, that runs `sealwax --background`. On Omarchy
//! uwsm activates `xdg-desktop-autostart.target` once Hyprland has started, and systemd's
//! `systemd-xdg-autostart-generator` turns every entry there into an `app-<id>@autostart.service`
//! unit (INTEGRATIONS.md "Autostart"). The toggle is on while that entry exists and isn't
//! hidden. Turning it on says what it writes and asks first; turning it off deletes that one
//! file and nothing else. Development builds and builds not on `PATH` write their own
//! executable's path, with `TryExec`, so the entry does nothing once that file is gone. The
//! file I/O runs off the GTK thread.

use crate::application::{AppState, ICON_NAME};
use crate::window::Window;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::ActionId;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The command an installed Sealwax runs as.
pub const PROGRAM: &str = "sealwax";

/// The option the entry starts Sealwax with.
pub const BACKGROUND_OPTION: &str = "--background";

/// The entry for `app_id` under the configuration home `config_home`.
pub fn entry_path(config_home: &Path, app_id: &str) -> PathBuf {
    config_home
        .join("autostart")
        .join(format!("{app_id}.desktop"))
}

/// What the entry starts: `sealwax` when the running executable is the one on `PATH`, else its
/// own path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    Installed,
    Path(PathBuf),
}

impl Program {
    /// From the running executable and the `sealwax` found on `PATH`, both canonical.
    pub fn choose(current: &Path, on_path: Option<&Path>) -> Self {
        if on_path == Some(current) {
            Self::Installed
        } else {
            Self::Path(current.to_path_buf())
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Installed => PROGRAM.to_owned(),
            Self::Path(path) => path.to_string_lossy().into_owned(),
        }
    }
}

/// One argument of an `Exec` line (the Desktop Entry Specification): quoted when it holds a
/// reserved character, `%` doubled, then escaped as a string value. `None` for a line break,
/// which no entry can hold.
pub fn exec_argument(argument: &str) -> Option<String> {
    if argument.contains(['\n', '\r']) {
        return None;
    }
    let argument = argument.replace('%', "%%");
    let reserved = argument.contains([
        ' ', '\t', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`',
    ]);
    if !reserved {
        return Some(argument);
    }
    let mut quoted = String::from("\"");
    for character in argument.chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    Some(quoted.replace('\\', "\\\\"))
}

/// The entry's text, or `None` when the program's path can't be written into one.
pub fn entry_text(program: &Program, development: bool) -> Option<String> {
    let text = program.text();
    let exec = exec_argument(&text)?;
    let try_exec = text.replace('\\', "\\\\");
    let name = if development {
        "Sealwax (development build)"
    } else {
        "Sealwax"
    };
    Some(format!(
        "[Desktop Entry]\n\
         # Written by Sealwax's Start Sealwax at Login. Turning it off deletes this file.\n\
         Type=Application\n\
         Name={name}\n\
         Comment=Starts Sealwax without a window, so mail keeps syncing\n\
         Exec={exec} {BACKGROUND_OPTION}\n\
         TryExec={try_exec}\n\
         Icon={ICON_NAME}\n\
         Terminal=false\n\
         NoDisplay=true\n"
    ))
}

/// Whether an entry's text starts Sealwax: not `Hidden=true`, not
/// `X-GNOME-Autostart-enabled=false`, in its `[Desktop Entry]` group.
pub fn starts(text: &str) -> bool {
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => return false,
            _ => {}
        }
    }
    true
}

/// What the desktop has now, and what turning the toggle on would write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub path: PathBuf,
    /// The entry exists and starts Sealwax.
    pub on: bool,
    pub program: Program,
    /// A debug build (the `.Devel` app-id), which packaging never installs.
    pub development: bool,
}

/// Looks at the entry and the executable; blocking.
pub fn probe(config_home: &Path, app_id: &str) -> Probe {
    let path = entry_path(config_home, app_id);
    let on = match fs::read_to_string(&path) {
        Ok(text) => starts(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(%error, "the autostart entry can't be read");
            path.exists()
        }
    };
    let current = std::env::current_exe().and_then(fs::canonicalize).ok();
    let on_path = glib::find_program_in_path(PROGRAM).and_then(|path| fs::canonicalize(path).ok());
    let program = match current {
        Some(current) => Program::choose(&current, on_path.as_deref()),
        None => Program::Installed,
    };
    Probe {
        path,
        on,
        program,
        development: cfg!(debug_assertions),
    }
}

/// Writes the entry (its folder made when missing; the file replaced whole); blocking.
pub fn enable(path: &Path, text: &str) -> io::Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| io::Error::other("the entry has no folder"))?;
    fs::create_dir_all(folder)?;
    let temporary = folder.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let mut file = fs::File::create(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

/// Deletes the entry; `Ok(false)` when there was none. Blocking.
pub fn disable(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// `path` with the home folder as `~`.
fn shown(path: &Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The question before the entry is written: what changes, where, and how to undo it.
pub fn question(probe: &Probe) -> (String, String) {
    let command = format!("{} {BACKGROUND_OPTION}", probe.program.text());
    let mut body = format!(
        "Sealwax will start without a window when you log in, so mail syncs and new mail is \
         notified from the start. Open Sealwax as usual to see its window.\n\nSealwax writes \
         {}, which runs “{command}”. Nothing else changes.\n\nTo undo: turn Start Sealwax at \
         Login off, or delete that file.",
        shown(&probe.path),
    );
    if let Program::Path(path) = &probe.program {
        let build = if probe.development {
            "This is a development build"
        } else {
            "This Sealwax isn't the one installed"
        };
        body.push_str(&format!(
            "\n\n{build}: the entry starts {} and does nothing once that file is gone.",
            path.display()
        ));
    }
    ("Start Sealwax at login?".to_owned(), body)
}

/// Installs the toggle, with its state read from the entry.
pub fn install(state: &Rc<AppState>) {
    let Some(actions) = state.app_actions() else {
        return;
    };
    let weak = Rc::downgrade(state);
    actions.connect_toggle(ActionId::StartAtLogin, false, move |on| {
        if let Some(state) = weak.upgrade() {
            toggled(&state, on);
        }
    });
    let weak = Rc::downgrade(state);
    let app_id = state.config().app_id.clone();
    glib::spawn_future_local(async move {
        let config = glib::user_config_dir();
        if let Ok(probe) = gio::spawn_blocking(move || probe(&config, &app_id)).await
            && let Some(state) = weak.upgrade()
        {
            show_state(&state, probe.on);
        }
    });
}

fn show_state(state: &AppState, on: bool) {
    if let Some(actions) = state.app_actions() {
        actions.set_toggle_state(ActionId::StartAtLogin, on);
    }
}

thread_local! {
    /// The command's last toast, which its next one replaces.
    static TOAST: std::cell::RefCell<Option<adw::Toast>> = const { std::cell::RefCell::new(None) };
}

fn toast(window: Option<&Window>, text: &str) {
    let Some(window) = window else {
        tracing::info!("{text}");
        return;
    };
    let toast = adw::Toast::new(text);
    if let Some(previous) = TOAST.with(|last| last.replace(Some(toast.clone()))) {
        previous.dismiss();
    }
    window.add_toast(toast);
}

/// The user turned the toggle `on` or off. The check mark shows the entry as it is until the
/// change is done; on asks first.
fn toggled(state: &Rc<AppState>, on: bool) {
    show_state(state, !on);
    let weak = Rc::downgrade(state);
    let app_id = state.config().app_id.clone();
    glib::spawn_future_local(async move {
        let config = glib::user_config_dir();
        let Ok(probe) = gio::spawn_blocking(move || probe(&config, &app_id)).await else {
            return;
        };
        let Some(state) = weak.upgrade() else {
            return;
        };
        show_state(&state, probe.on);
        let window = state.window();
        if !on {
            let path = probe.path.clone();
            let outcome = gio::spawn_blocking(move || disable(&path)).await;
            match outcome {
                Ok(Ok(_)) => {
                    tracing::info!("start at login turned off");
                    show_state(&state, false);
                    toast(window.as_ref(), "Sealwax won't start at login");
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "the autostart entry can't be deleted");
                    toast(
                        window.as_ref(),
                        &format!("Couldn't delete {}: {error}", shown(&probe.path)),
                    );
                }
                Err(_) => toast(window.as_ref(), "Turning off Start at Login failed"),
            }
            return;
        }
        if probe.on {
            toast(window.as_ref(), "Sealwax already starts at login");
            return;
        }
        let Some(window) = window else {
            tracing::warn!("Start at Login needs the window to ask");
            return;
        };
        let Some(text) = entry_text(&probe.program, probe.development) else {
            toast(
                Some(&window),
                "Sealwax's path can't be written into an autostart entry",
            );
            return;
        };
        if !ask(&window, &probe).await {
            return;
        }
        let path = probe.path.clone();
        let outcome = gio::spawn_blocking(move || enable(&path, &text)).await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        match outcome {
            Ok(Ok(())) => {
                tracing::info!("start at login turned on");
                show_state(&state, true);
                toast(Some(&window), "Sealwax will start at login");
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "the autostart entry can't be written");
                toast(
                    Some(&window),
                    &format!("Couldn't write {}: {error}", shown(&probe.path)),
                );
            }
            Err(_) => toast(Some(&window), "Turning on Start at Login failed"),
        }
    });
}

/// The question, over the main window; true on yes.
async fn ask(window: &Window, probe: &Probe) -> bool {
    let (heading, body) = question(probe);
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.set_widget_name("start-at-login");
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("start-at-login", "Start at Login");
    dialog.set_response_appearance("start-at-login", adw::ResponseAppearance::Suggested);
    dialog.choose_future(Some(window.window())).await == "start-at-login"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sealwax-autostart-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_entry_runs_sealwax_in_the_background() {
        assert_eq!(
            entry_path(
                Path::new("/home/kari/.config"),
                "io.github.pixdevsapps.Sealwax"
            ),
            Path::new("/home/kari/.config/autostart/io.github.pixdevsapps.Sealwax.desktop")
        );
        let text = entry_text(&Program::Installed, false).unwrap();
        assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
        assert!(text.contains("\nExec=sealwax --background\n"), "{text}");
        assert!(text.contains("\nTryExec=sealwax\n"), "{text}");
        assert!(text.contains("\nName=Sealwax\n"), "{text}");
        assert!(text.contains("\nType=Application\n"), "{text}");
        assert!(starts(&text));
        let development = entry_text(
            &Program::Path(PathBuf::from("/home/kari/src/sealwax/target/debug/sealwax")),
            true,
        )
        .unwrap();
        assert!(
            development.contains(
                "\nExec=/home/kari/src/sealwax/target/debug/sealwax --background\n\
                 TryExec=/home/kari/src/sealwax/target/debug/sealwax\n"
            ),
            "{development}"
        );
        assert!(development.contains("\nName=Sealwax (development build)\n"));
    }

    #[test]
    fn exec_arguments_are_quoted_as_the_specification_says() {
        assert_eq!(
            exec_argument("/usr/bin/sealwax").unwrap(),
            "/usr/bin/sealwax"
        );
        assert_eq!(
            exec_argument("/opt/a b/sealwax").unwrap(),
            "\"/opt/a b/sealwax\""
        );
        assert_eq!(
            exec_argument("/opt/100%/sealwax").unwrap(),
            "/opt/100%%/sealwax"
        );
        // A quoted `$` and `"` are escaped with a backslash, and every backslash is then
        // escaped once more as a string value.
        assert_eq!(
            exec_argument("/opt/$x\"y/sealwax").unwrap(),
            "\"/opt/\\\\$x\\\\\"y/sealwax\""
        );
        assert_eq!(exec_argument("/opt/a\nb"), None);
        assert_eq!(
            entry_text(&Program::Path(PathBuf::from("/a\nb")), false),
            None
        );
    }

    #[test]
    fn hidden_or_disabled_entries_start_nothing() {
        assert!(starts("[Desktop Entry]\nExec=sealwax --background\n"));
        assert!(!starts("[Desktop Entry]\nHidden=true\n"));
        assert!(!starts(
            "[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"
        ));
        assert!(starts(
            "[Desktop Entry]\nExec=sealwax\n[Desktop Action other]\nHidden=true\n"
        ));
    }

    #[test]
    fn the_program_is_sealwax_only_when_it_is_the_one_on_the_path() {
        let installed = Path::new("/usr/bin/sealwax");
        assert_eq!(
            Program::choose(installed, Some(installed)),
            Program::Installed
        );
        let built = Path::new("/home/kari/src/sealwax/target/release/sealwax");
        assert_eq!(
            Program::choose(built, Some(installed)),
            Program::Path(built.to_path_buf())
        );
        assert_eq!(
            Program::choose(built, None),
            Program::Path(built.to_path_buf())
        );
    }

    #[test]
    fn the_question_says_what_changes_and_how_to_undo_it() {
        let mut probe = Probe {
            path: PathBuf::from("/nowhere/.config/autostart/io.github.pixdevsapps.Sealwax.desktop"),
            on: false,
            program: Program::Installed,
            development: false,
        };
        let (heading, body) = question(&probe);
        assert_eq!(heading, "Start Sealwax at login?");
        assert!(
            body.contains("io.github.pixdevsapps.Sealwax.desktop"),
            "{body}"
        );
        assert!(body.contains("runs “sealwax --background”"), "{body}");
        assert!(
            body.contains("To undo: turn Start Sealwax at Login off"),
            "{body}"
        );
        assert!(!body.contains("development"), "{body}");
        probe.program = Program::Path(PathBuf::from("/src/target/debug/sealwax"));
        probe.development = true;
        let (_, body) = question(&probe);
        assert!(
            body.contains(
                "This is a development build: the entry starts /src/target/debug/sealwax"
            ),
            "{body}"
        );
    }

    #[test]
    fn on_writes_the_entry_and_off_deletes_only_it() {
        let config = scratch("toggle").join("config");
        let path = entry_path(&config, "io.github.pixdevsapps.Sealwax.Test");
        let neighbour = config.join("autostart").join("other.desktop");
        let probe_now = || probe(&config, "io.github.pixdevsapps.Sealwax.Test");
        assert!(!probe_now().on);
        let text = entry_text(&Program::Installed, false).unwrap();
        enable(&path, &text).unwrap();
        assert!(probe_now().on);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        fs::write(&neighbour, "[Desktop Entry]\nExec=other\n").unwrap();
        // Written again: replaced whole, no temporary file left.
        enable(&path, &text).unwrap();
        let mut names: Vec<String> = fs::read_dir(config.join("autostart"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "io.github.pixdevsapps.Sealwax.Test.desktop",
                "other.desktop"
            ]
        );
        // An entry the user hid counts as off.
        fs::write(&path, "[Desktop Entry]\nHidden=true\n").unwrap();
        assert!(!probe_now().on);
        assert!(disable(&path).unwrap());
        assert!(!disable(&path).unwrap());
        assert!(!probe_now().on);
        assert!(neighbour.exists());
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }
}
