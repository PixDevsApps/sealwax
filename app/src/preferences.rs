//! `config.toml` and `keys.toml` (ADR-009): read off the GTK thread when Sealwax starts and
//! again whenever either changes in `$XDG_CONFIG_HOME/sealwax/` (GFileMonitors on the folder
//! and its parent, debounced), as in Stet. A file with mistakes is not applied: the previous
//! settings or keys stay, and the listeners hear what is wrong, with the line and column. A
//! missing file is the defaults. Preferences and Customize Shortcuts open the files.

use crate::monitor::{DirWatch, TriggerRule};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use sealwax_domain::config::{CONFIG_FILE, Config, FileError, KEYS_FILE, default_config_file};
use sealwax_domain::keymap::{Keymap, default_keys_file};
use sealwax_infrastructure::settings_files::{create_if_missing, read_optional};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::{Cell, RefCell};
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The two settings files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFile {
    Config,
    Keys,
}

impl SettingsFile {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Config => CONFIG_FILE,
            Self::Keys => KEYS_FILE,
        }
    }

    /// The file a missing one is created as: every default, commented out.
    pub fn defaults(self) -> String {
        match self {
            Self::Config => default_config_file(),
            Self::Keys => default_keys_file(),
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Config => 0,
            Self::Keys => 1,
        }
    }
}

/// What a reload found.
#[derive(Debug, Clone, Default)]
pub struct Update {
    /// The settings in use before, when new ones were applied (on the first read too).
    pub config_before: Option<Config>,
    /// Whether the keys changed (always on the first read): reinstall the accelerators, update
    /// the scoped shortcuts and rebuild the menu.
    pub keymap_changed: bool,
    /// One message per file that could not be read or applied, for a toast.
    pub problems: Vec<String>,
}

impl Update {
    fn is_empty(&self) -> bool {
        self.config_before.is_none() && !self.keymap_changed && self.problems.is_empty()
    }
}

/// The settings and keys in use, and what each file said at the last read.
#[derive(Debug, Default)]
struct Loaded {
    config: Config,
    keymap: Keymap,
    /// Each file's text at its last read (`None` inside: no file), so an unchanged file is
    /// neither applied nor reported again.
    texts: [Option<Option<String>>; 2],
    errors: [Vec<FileError>; 2],
    loaded: bool,
}

impl Loaded {
    /// Applies freshly read texts of `config.toml` and `keys.toml`.
    fn apply(&mut self, texts: [io::Result<Option<String>>; 2]) -> Update {
        let mut update = Update::default();
        let first = !self.loaded;
        let [config, keys] = texts;
        if let Some(text) = self.changed(SettingsFile::Config, config, first, &mut update) {
            match Config::parse(text.as_deref().unwrap_or("")) {
                Ok(config) => {
                    self.errors[0].clear();
                    if first || self.config != config {
                        update.config_before = Some(std::mem::replace(&mut self.config, config));
                    }
                }
                Err(errors) => self.reject(SettingsFile::Config, errors, &mut update),
            }
        }
        if let Some(text) = self.changed(SettingsFile::Keys, keys, first, &mut update) {
            match Keymap::parse(text.as_deref().unwrap_or("")) {
                Ok(keymap) => {
                    self.errors[1].clear();
                    if first || self.keymap != keymap {
                        self.keymap = keymap;
                        update.keymap_changed = true;
                    }
                }
                Err(errors) => self.reject(SettingsFile::Keys, errors, &mut update),
            }
        }
        self.loaded = true;
        update
    }

    /// The new text of `file` when it differs from the last read, or on the first read.
    fn changed(
        &mut self,
        file: SettingsFile,
        read: io::Result<Option<String>>,
        first: bool,
        update: &mut Update,
    ) -> Option<Option<String>> {
        match read {
            Ok(text) if first || self.texts[file.index()].as_ref() != Some(&text) => {
                self.texts[file.index()] = Some(text.clone());
                Some(text)
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(file = file.name(), %error, "could not read a settings file");
                update
                    .problems
                    .push(format!("{} could not be read: {error}", file.name()));
                None
            }
        }
    }

    fn reject(&mut self, file: SettingsFile, errors: Vec<FileError>, update: &mut Update) {
        if let Some(first) = errors.first() {
            tracing::warn!(
                file = file.name(),
                line = first.line,
                column = first.column,
                mistakes = errors.len(),
                "settings file not applied"
            );
        }
        update.problems.push(problem(file.name(), &errors));
        self.errors[file.index()] = errors;
    }
}

/// The toast for a file with mistakes: the first one, and how many more there are.
fn problem(file: &str, errors: &[FileError]) -> String {
    let more = match errors.len() {
        0 | 1 => String::new(),
        2 => " (and 1 more mistake)".to_owned(),
        count => format!(" (and {} more mistakes)", count - 1),
    };
    let first = errors.first().map_or_else(String::new, ToString::to_string);
    format!("{file} not applied: {first}{more}")
}

type Listener = Rc<dyn Fn(&Update)>;

/// Sealwax's settings and keys, kept in step with the files.
pub struct Preferences {
    config_file: PathBuf,
    keys_file: PathBuf,
    loaded: RefCell<Loaded>,
    generation: Cell<u64>,
    listeners: RefCell<Vec<Listener>>,
    watch: RefCell<Option<Rc<DirWatch>>>,
}

impl Preferences {
    /// Starts reading both files and watching their folder.
    pub fn start(dirs: &SealwaxDirs) -> Rc<Self> {
        let this = Rc::new(Self {
            config_file: dirs.config_file(),
            keys_file: dirs.keys_file(),
            loaded: RefCell::new(Loaded::default()),
            generation: Cell::new(0),
            listeners: RefCell::new(Vec::new()),
            watch: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        let rule: TriggerRule = Box::new(|_, file, other| {
            let settings = |name: &str| name == CONFIG_FILE || name == KEYS_FILE;
            settings(file) || other.is_some_and(settings)
        });
        let watch = DirWatch::start(dirs.config.clone(), rule, move || {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
        this.watch.replace(Some(watch));
        this.reload();
        this
    }

    pub fn config(&self) -> Config {
        self.loaded.borrow().config.clone()
    }

    pub fn keymap(&self) -> Keymap {
        self.loaded.borrow().keymap.clone()
    }

    pub fn path(&self, file: SettingsFile) -> PathBuf {
        match file {
            SettingsFile::Config => self.config_file.clone(),
            SettingsFile::Keys => self.keys_file.clone(),
        }
    }

    /// The mistakes `file` had at the last read; empty when it was applied.
    pub fn errors(&self, file: SettingsFile) -> Vec<FileError> {
        self.loaded.borrow().errors[file.index()].clone()
    }

    /// Whether both files have been read once.
    pub fn is_loaded(&self) -> bool {
        self.loaded.borrow().loaded
    }

    /// Calls `listener` after every read that changed something or found a problem.
    pub fn connect_changed(&self, listener: impl Fn(&Update) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(listener));
    }

    /// Resolves once the files have been read the first time, or after `timeout`.
    pub async fn ready(&self, timeout: Duration) {
        let started = Instant::now();
        while !self.is_loaded() && started.elapsed() < timeout {
            glib::timeout_future(Duration::from_millis(2)).await;
        }
    }

    /// Reads both files again; the newest read wins.
    pub fn reload(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let paths = [self.config_file.clone(), self.keys_file.clone()];
        let this = self.clone();
        glib::spawn_future_local(async move {
            let texts = gio::spawn_blocking(move || paths.map(|path| read_optional(&path))).await;
            if this.generation.get() != generation {
                return;
            }
            let Ok(texts) = texts else {
                tracing::error!("the settings reader panicked");
                return;
            };
            let update = this.loaded.borrow_mut().apply(texts);
            if update.is_empty() {
                return;
            }
            let listeners = this.listeners.borrow().clone();
            for listener in listeners {
                listener(&update);
            }
        });
    }
}

/// Creates `file` at `path` with its commented defaults unless it exists, off the GTK thread.
/// Resolves to whether it created the file.
pub async fn ensure_file(path: PathBuf, file: SettingsFile) -> io::Result<bool> {
    gio::spawn_blocking(move || create_if_missing(&path, &file.defaults()))
        .await
        .unwrap_or_else(|_| Err(io::Error::other("the file writer panicked")))
}

/// Preferences and Customize Shortcuts: `path`, created with its defaults when missing, opened
/// in the default text editor. Resolves to a message for a toast.
pub async fn open_in_editor(
    window: &(impl IsA<gtk::Window> + Clone + 'static),
    path: PathBuf,
    file: SettingsFile,
) -> String {
    let created = match ensure_file(path.clone(), file).await {
        Ok(created) => created,
        Err(error) => return format!("Could not create {}: {error}", file.name()),
    };
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
    match launcher.launch_future(Some(window)).await {
        Ok(()) if created => format!(
            "Created {} with every default commented out; Sealwax applies it when you save",
            file.name()
        ),
        Ok(()) => format!("Opened {}; Sealwax applies it when you save", file.name()),
        Err(error) => format!("Could not open {}: {}", path.display(), error.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::actions::ActionId;
    use sealwax_domain::config::CornerRadius;

    fn read(text: &str) -> io::Result<Option<String>> {
        Ok(Some(text.to_owned()))
    }

    #[test]
    fn the_first_read_applies_both_files() {
        let mut loaded = Loaded::default();
        let update = loaded.apply([Ok(None), Ok(None)]);
        assert_eq!(update.config_before, Some(Config::default()));
        assert!(update.keymap_changed);
        assert!(update.problems.is_empty());
        assert!(loaded.loaded);
        // The same files again change nothing.
        assert!(loaded.apply([Ok(None), Ok(None)]).is_empty());
    }

    #[test]
    fn changes_are_applied_and_reported_once() {
        let mut loaded = Loaded::default();
        loaded.apply([Ok(None), Ok(None)]);
        let update = loaded.apply([read("[appearance]\ncorner_radius = 12\n"), Ok(None)]);
        assert_eq!(update.config_before, Some(Config::default()));
        assert!(!update.keymap_changed);
        assert_eq!(loaded.config.appearance.corner_radius, CornerRadius::Px(12));
        let update = loaded.apply([
            read("[appearance]\ncorner_radius = 12\n"),
            read("archive = [\"e\"]\n"),
        ]);
        assert!(update.config_before.is_none());
        assert!(update.keymap_changed);
        assert_eq!(loaded.keymap.keys(ActionId::Archive).len(), 1);
        // A comment changes the text but not the keys.
        let update = loaded.apply([
            read("[appearance]\ncorner_radius = 12\n"),
            read("# mine\narchive = [\"e\"]\n"),
        ]);
        assert!(update.is_empty());
    }

    #[test]
    fn a_faulty_file_is_never_applied() {
        let mut loaded = Loaded::default();
        loaded.apply([Ok(None), read("archive = [\"e\"]\n")]);
        let update = loaded.apply([
            read("[appearance]\ncorner_radius = 900\n"),
            read("archive = [\"e\"]\nstar = [\"e\"]\n"),
        ]);
        assert!(update.config_before.is_none());
        assert!(!update.keymap_changed);
        assert_eq!(update.problems.len(), 2);
        assert!(
            update.problems[0].starts_with("config.toml not applied: line 2, column 17: "),
            "{:?}",
            update.problems
        );
        assert!(update.problems[1].starts_with("keys.toml not applied: line 2, column 9: "));
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.keymap.keys(ActionId::Archive).len(), 1);
        assert_eq!(loaded.errors[0].len(), 1);
        // The same faulty text isn't reported again; fixing it applies it.
        let update = loaded.apply([
            read("[appearance]\ncorner_radius = 900\n"),
            read("archive = [\"e\"]\nstar = [\"e\"]\n"),
        ]);
        assert!(update.is_empty());
        let update = loaded.apply([
            read("[appearance]\ncorner_radius = 9\n"),
            read("archive = [\"e\"]\nstar = [\"e\"]\n"),
        ]);
        assert!(update.config_before.is_some());
        assert!(loaded.errors[0].is_empty());
    }

    #[test]
    fn a_deleted_file_means_defaults() {
        let mut loaded = Loaded::default();
        loaded.apply([Ok(None), read("quit = []\n")]);
        assert!(loaded.keymap.keys(ActionId::Quit).is_empty());
        let update = loaded.apply([Ok(None), Ok(None)]);
        assert!(update.keymap_changed);
        assert_eq!(loaded.keymap, Keymap::default());
    }

    #[test]
    fn read_errors_are_reported_and_keep_the_settings() {
        let mut loaded = Loaded::default();
        loaded.apply([read("[appearance]\nreading_font = \"mono\"\n"), Ok(None)]);
        let update = loaded.apply([
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "it is not UTF-8 text",
            )),
            Ok(None),
        ]);
        assert_eq!(
            update.problems,
            ["config.toml could not be read: it is not UTF-8 text"]
        );
        assert_ne!(loaded.config, Config::default());
    }

    #[test]
    fn problems_count_the_other_mistakes() {
        let error = |line| FileError {
            line,
            column: 1,
            message: "x".to_owned(),
        };
        assert_eq!(
            problem("keys.toml", &[error(1)]),
            "keys.toml not applied: line 1, column 1: x"
        );
        assert_eq!(
            problem("keys.toml", &[error(1), error(2)]),
            "keys.toml not applied: line 1, column 1: x (and 1 more mistake)"
        );
        assert_eq!(
            problem("keys.toml", &[error(1), error(2), error(3)]),
            "keys.toml not applied: line 1, column 1: x (and 2 more mistakes)"
        );
    }
}
