//! What attachment cards, Print, Show Original and Save as .eml share (ML-061, ML-063): the
//! bytes of an attachment or of a whole message, from the store or, for a message stored as
//! its text parts (`BodyState::Text`), from the engine (`FetchPart`, `FetchOriginal`, which
//! store nothing); the private folder attachments open from; the file chooser (GTK's
//! `FileDialog`, the portal's where there is one); writing off the GTK thread; toasts; and the
//! hooks headless runs use instead of the desktop's applications and dialogs.
//!
//! An attachment opens from its own folder (0700) under `$XDG_RUNTIME_DIR/<app-id>/attachments`
//! (the cache folder without a runtime folder), as a 0600 file with a safe name and never the
//! executable bit. The folder is emptied when the next reader starts; the runtime folder is
//! also emptied at logout.

use super::{Inner, Reader};
use crate::engine_link::{EngineLink, Subscription};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use sealwax_domain::message;
use sealwax_domain::model::{Attachment, MessageId};
use sealwax_engine::api::{BodyError, Command, Event};
use sealwax_infrastructure::store::BodyState;
use sealwax_infrastructure::xdg::ensure_private_dir;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

type Waiter = async_channel::Sender<Result<Vec<u8>, BodyError>>;
type Opener = Rc<dyn Fn(&Path)>;
type Chooser = Rc<dyn Fn(&FileChoice) -> Option<PathBuf>>;

/// What a file chooser asks; a headless run's chooser answers it with a scratch path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChoice {
    /// Where to save one file, suggesting this name.
    Save { suggested: String },
    /// Which folder Download All saves into.
    Folder,
}

/// Why the bytes of an attachment or a message aren't there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The engine couldn't download them.
    Download(BodyError),
    /// The message's body isn't stored yet.
    NotDownloaded,
    /// The stored message doesn't hold the part.
    Unreadable,
}

impl Failure {
    /// Why, in plain words, after "Couldn't open it: " and the like.
    pub fn reason(self) -> &'static str {
        match self {
            Failure::Download(BodyError::Offline) => "the mail server can't be reached",
            Failure::Download(BodyError::NeedsSignIn) => "the account needs you to sign in again",
            Failure::Download(BodyError::Gone) => "the message is no longer on the server",
            Failure::Download(BodyError::Store) => "it couldn't be read on this computer",
            Failure::Download(BodyError::Other) => "it couldn't be downloaded",
            Failure::NotDownloaded => "the message hasn't been downloaded yet",
            Failure::Unreadable => "it couldn't be read from the message",
        }
    }
}

/// The reader's share of files: the engine link for downloads, the requests waiting for their
/// answers, the private folder and the hooks.
pub(super) struct Files {
    engine: RefCell<Option<EngineLink>>,
    subscription: RefCell<Option<Subscription>>,
    parts: RefCell<HashMap<(MessageId, u32), Vec<Waiter>>>,
    originals: RefCell<HashMap<MessageId, Vec<Waiter>>>,
    root: RefCell<PathBuf>,
    opener: RefCell<Option<Opener>>,
    chooser: RefCell<Option<Chooser>>,
    print_target: RefCell<Option<PathBuf>>,
    pub printing: Cell<bool>,
}

impl Files {
    /// Empties what an earlier run left in the private folder, off the GTK thread.
    pub(super) fn new(cache: &Path) -> Files {
        let root = default_root(cache);
        let stale = root.clone();
        gio::spawn_blocking(move || {
            if let Err(error) = std::fs::remove_dir_all(&stale)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "reader: opened attachments of an earlier run stayed");
            }
        });
        Files {
            engine: RefCell::new(None),
            subscription: RefCell::new(None),
            parts: RefCell::new(HashMap::new()),
            originals: RefCell::new(HashMap::new()),
            root: RefCell::new(root),
            opener: RefCell::new(None),
            chooser: RefCell::new(None),
            print_target: RefCell::new(None),
            printing: Cell::new(false),
        }
    }

    fn answer_part(&self, message: MessageId, part: u32, result: Result<Vec<u8>, BodyError>) {
        for waiter in self
            .parts
            .borrow_mut()
            .remove(&(message, part))
            .unwrap_or_default()
        {
            let _ = waiter.try_send(result.clone());
        }
    }

    fn answer_original(&self, message: MessageId, result: Result<Vec<u8>, BodyError>) {
        for waiter in self
            .originals
            .borrow_mut()
            .remove(&message)
            .unwrap_or_default()
        {
            let _ = waiter.try_send(result.clone());
        }
    }

    fn send(&self, command: Command) -> bool {
        self.engine
            .borrow()
            .as_ref()
            .is_some_and(|link| link.send(command).is_ok())
    }

    pub(super) fn print_target(&self) -> Option<PathBuf> {
        self.print_target.borrow().clone()
    }
}

/// `$XDG_RUNTIME_DIR/<app-id>/attachments`, or the cache folder's `attachments`.
fn default_root(cache: &Path) -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    let application = gio::Application::default().and_then(|app| app.application_id());
    match (runtime, application) {
        (Some(runtime), Some(id)) => runtime.join(id.as_str()).join("attachments"),
        _ => cache.join("attachments"),
    }
}

/// Takes the engine's answers to `FetchPart` and `FetchOriginal` (`Reader::connect_engine`).
pub(super) fn connect_engine(reader: &Reader, engine: &EngineLink) {
    let files = &reader.inner.files;
    files.engine.replace(Some(engine.clone()));
    let weak = Rc::downgrade(&reader.inner);
    let subscription = engine.subscribe(move |event| {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        match event {
            Event::PartFetched {
                message,
                part,
                result,
            } => inner.files.answer_part(
                *message,
                *part,
                result
                    .as_ref()
                    .map(|part| part.data.clone())
                    .map_err(|e| *e),
            ),
            Event::OriginalFetched { message, result } => inner.files.answer_original(
                *message,
                result
                    .as_ref()
                    .map(|original| original.0.clone())
                    .map_err(|e| *e),
            ),
            _ => {}
        }
    });
    files.subscription.replace(Some(subscription));
}

impl Reader {
    /// Opens attachments from folders under `root` and hands each file to `opener` instead of
    /// the desktop's application: for tests and headless runs.
    pub fn set_file_opener(&self, root: &Path, opener: impl Fn(&Path) + 'static) {
        self.inner.files.root.replace(root.to_owned());
        self.inner.files.opener.replace(Some(Rc::new(opener)));
    }

    /// Answers the Save and folder choosers instead of GTK's file dialog (`None` cancels): for
    /// tests and headless runs.
    pub fn set_file_chooser(&self, chooser: impl Fn(&FileChoice) -> Option<PathBuf> + 'static) {
        self.inner.files.chooser.replace(Some(Rc::new(chooser)));
    }

    /// Print writes a PDF to `path` instead of opening the print dialog: for tests and
    /// headless runs.
    pub fn set_print_target(&self, path: Option<PathBuf>) {
        self.inner.files.print_target.replace(path);
    }

    /// Whether a print is under way.
    pub fn printing(&self) -> bool {
        self.inner.files.printing.get()
    }
}

/// A headless (Broadway) run never reaches the desktop's applications, portals or printers.
pub(super) fn headless() -> bool {
    gdk::Display::default().is_some_and(|display| display.type_().name() == "GdkBroadwayDisplay")
}

/// What the store holds of `message`'s body; `None` when it doesn't hold the message.
async fn body_state(inner: &Inner, message: MessageId) -> Option<(BodyState, bool)> {
    let reply = inner.store.body_target(message)?;
    match reply.recv().await {
        Ok(mut targets) => targets
            .pop()
            .map(|target| (target.body_state, target.blob.is_some())),
        Err(error) => {
            tracing::warn!(%error, "reader: a message's body state couldn't be read");
            None
        }
    }
}

/// Whether `message` is stored as its text parts, its attachments left on the server.
pub(super) async fn stored_as_text(inner: &Inner, message: MessageId) -> bool {
    matches!(body_state(inner, message).await, Some((BodyState::Text, _)))
}

async fn raw(inner: &Inner, message: MessageId) -> Result<Vec<u8>, Failure> {
    let reply = inner
        .store
        .raw_message(message)
        .ok_or(Failure::NotDownloaded)?;
    match reply.recv().await {
        Ok(Some(raw)) => Ok(raw),
        Ok(None) => Err(Failure::NotDownloaded),
        Err(error) => {
            tracing::warn!(%error, "reader: a stored message couldn't be read");
            Err(Failure::Download(BodyError::Store))
        }
    }
}

/// The engine's download of one part of `message`; a request already out is joined.
async fn fetch_part(inner: &Inner, message: MessageId, part: u32) -> Result<Vec<u8>, BodyError> {
    let (waiter, answer) = async_channel::bounded(1);
    let first = {
        let mut parts = inner.files.parts.borrow_mut();
        let waiters = parts.entry((message, part)).or_default();
        waiters.push(waiter);
        waiters.len() == 1
    };
    if first && !inner.files.send(Command::FetchPart { message, part }) {
        inner
            .files
            .answer_part(message, part, Err(BodyError::Other));
    }
    answer.recv().await.unwrap_or(Err(BodyError::Other))
}

/// The engine's download of `message` whole; a request already out is joined.
async fn fetch_original(inner: &Inner, message: MessageId) -> Result<Vec<u8>, BodyError> {
    let (waiter, answer) = async_channel::bounded(1);
    let first = {
        let mut originals = inner.files.originals.borrow_mut();
        let waiters = originals.entry(message).or_default();
        waiters.push(waiter);
        waiters.len() == 1
    };
    if first && !inner.files.send(Command::FetchOriginal { message }) {
        inner.files.answer_original(message, Err(BodyError::Other));
    }
    answer.recv().await.unwrap_or(Err(BodyError::Other))
}

/// The decoded bytes of `attachment` of `message`: from the stored message, or downloaded by
/// its section when the message is stored as its text parts (`downloading` runs first).
pub(super) async fn attachment(
    inner: &Inner,
    message: MessageId,
    attachment: &Attachment,
    downloading: impl FnOnce(),
) -> Result<Vec<u8>, Failure> {
    if stored_as_text(inner, message).await {
        downloading();
        return fetch_part(inner, message, attachment.part)
            .await
            .map_err(Failure::Download);
    }
    let raw = raw(inner, message).await?;
    let index = attachment.part;
    gio::spawn_blocking(move || message::part(&raw, index).map(|part| part.data))
        .await
        .ok()
        .flatten()
        .ok_or(Failure::Unreadable)
}

/// `message` as the server has it: the stored bytes of a message stored whole, else
/// downloaded (`downloading` runs first), never the copy rebuilt from its text parts.
pub(super) async fn original(
    inner: &Inner,
    message: MessageId,
    downloading: impl FnOnce(),
) -> Result<Vec<u8>, Failure> {
    match body_state(inner, message).await {
        None => Err(Failure::Download(BodyError::Gone)),
        Some((BodyState::Full, true)) => raw(inner, message).await,
        Some(_) => {
            downloading();
            fetch_original(inner, message)
                .await
                .map_err(Failure::Download)
        }
    }
}

/// The window the reader is in.
pub(super) fn window(inner: &Inner) -> Option<gtk::Window> {
    inner.stage.root().and_downcast::<gtk::Window>()
}

/// A toast in the reader's window.
pub(super) fn toast(inner: &Inner, text: &str) {
    let overlay = inner
        .stage
        .ancestor(adw::ToastOverlay::static_type())
        .and_downcast::<adw::ToastOverlay>();
    match overlay {
        Some(overlay) => {
            overlay.add_toast(
                adw::Toast::builder()
                    .title(text)
                    .use_markup(false)
                    .timeout(4)
                    .build(),
            );
        }
        None => tracing::debug!("reader: no toast overlay for a file's outcome"),
    }
}

/// Where to save one file, suggesting `suggested`; `None` when the user cancelled.
pub(super) async fn choose_save(inner: &Inner, title: &str, suggested: &str) -> Option<gio::File> {
    let chooser = inner.files.chooser.borrow().clone();
    if let Some(chooser) = chooser {
        return chooser(&FileChoice::Save {
            suggested: suggested.to_owned(),
        })
        .map(|path| gio::File::for_path(&path));
    }
    if headless() {
        tracing::warn!("reader: a headless (Broadway) run opens no file chooser");
        return None;
    }
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .initial_name(suggested)
        .modal(true)
        .build();
    dialog.save_future(window(inner).as_ref()).await.ok()
}

/// Which folder to save into; `None` when the user cancelled or picked one that isn't on this
/// computer.
pub(super) async fn choose_folder(inner: &Inner, title: &str) -> Option<PathBuf> {
    let chooser = inner.files.chooser.borrow().clone();
    if let Some(chooser) = chooser {
        return chooser(&FileChoice::Folder);
    }
    if headless() {
        tracing::warn!("reader: a headless (Broadway) run opens no folder chooser");
        return None;
    }
    let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
    dialog
        .select_folder_future(window(inner).as_ref())
        .await
        .ok()
        .and_then(|folder| folder.path())
}

/// Writes `bytes` to `file`, replacing it (the chooser asked first), off the GTK thread.
pub(super) async fn write(file: &gio::File, bytes: Vec<u8>) -> Result<(), glib::Error> {
    file.replace_contents_future(
        bytes,
        None,
        false,
        gio::FileCreateFlags::REPLACE_DESTINATION,
    )
    .await
    .map(|_| ())
    .map_err(|(_, error)| error)
}

/// Writes `bytes` into a new private folder under the reader's root as `name` (0600), off the
/// GTK thread, and returns the file.
pub(super) async fn write_private(
    inner: &Inner,
    name: String,
    bytes: Vec<u8>,
) -> io::Result<PathBuf> {
    let root = inner.files.root.borrow().clone();
    gio::spawn_blocking(move || -> io::Result<PathBuf> {
        ensure_private_dir(&root)?;
        let folder = root.join(glib::uuid_string_random().as_str());
        DirBuilder::new().mode(0o700).create(&folder)?;
        let path = folder.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(path)
    })
    .await
    .unwrap_or_else(|_| Err(io::Error::other("the writer stopped")))
}

/// Hands `path` to the desktop's default application (`gtk::FileLauncher`, the portal's where
/// there is one), or to the test's opener; never from a headless run.
pub(super) fn launch(inner: &Inner, path: &Path) {
    let opener = inner.files.opener.borrow().clone();
    if let Some(opener) = opener {
        opener(path);
        return;
    }
    if headless() {
        tracing::warn!("reader: attachments don't open from a headless (Broadway) run");
        return;
    }
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    launcher.launch(window(inner).as_ref(), gio::Cancellable::NONE, |result| {
        if let Err(error) = result {
            tracing::warn!(%error, "reader: an attachment didn't open");
        }
    });
}

/// Saves `files` (names and bytes) into `folder` off the GTK thread, each under a name the
/// folder doesn't hold yet; returns how many were saved and the first error.
pub(super) async fn save_all(
    folder: PathBuf,
    files: Vec<(String, Vec<u8>)>,
) -> (usize, Option<io::Error>) {
    gio::spawn_blocking(move || {
        let mut saved = 0;
        let mut first = None;
        for (name, bytes) in files {
            match save_new(&folder, &name, &bytes) {
                Ok(_) => saved += 1,
                Err(error) => {
                    first.get_or_insert(error);
                }
            }
        }
        (saved, first)
    })
    .await
    .unwrap_or((0, Some(io::Error::other("the writer stopped"))))
}

/// `bytes` into `folder` as `name`, or `name (2)`… when it is taken; never over a file.
fn save_new(folder: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    loop {
        let unique =
            sealwax_domain::attachment::unique_name(name, |name| folder.join(name).exists());
        let path = folder.join(&unique);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn saving_into_a_folder_never_overwrites() {
        let dir = tempfile_dir();
        std::fs::write(dir.join("report.pdf"), b"old").unwrap();
        let first = save_new(&dir, "report.pdf", b"one").unwrap();
        let second = save_new(&dir, "report.pdf", b"two").unwrap();
        assert_eq!(first.file_name().unwrap(), "report (2).pdf");
        assert_eq!(second.file_name().unwrap(), "report (3).pdf");
        assert_eq!(std::fs::read(dir.join("report.pdf")).unwrap(), b"old");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        let mode = std::fs::metadata(&second).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0, "never executable");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failures_say_why_in_plain_words() {
        assert_eq!(
            Failure::Download(BodyError::Offline).reason(),
            "the mail server can't be reached"
        );
        assert_eq!(
            Failure::NotDownloaded.reason(),
            "the message hasn't been downloaded yet"
        );
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sealwax-files-test-{}-{}",
            std::process::id(),
            glib::random_int()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
