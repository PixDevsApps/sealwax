//! The rules files from the app (ADR-020): read, written and changed off the GTK thread
//! (AGENTS.md: nothing blocks it), one job at a time, so two quick changes never cross and the
//! last one wins on disk. Writes go through `infrastructure::rules::write` (atomic, 0600). A
//! write checks first that the file is still the one read, so an edit made in a text editor
//! meanwhile is never overwritten. The engine reads the file again when its modification time
//! changes, so what is saved takes effect at the next new mail.

use gtk4::{gio, glib};
use sealwax_domain::account::AccountKey;
use sealwax_domain::sieve::{self, Folders, Invalid, RuleSet};
use sealwax_infrastructure::rules;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// An account's rules file: where it is and the folders its rules name.
#[derive(Clone, Debug)]
pub struct Place {
    pub dir: PathBuf,
    pub key: AccountKey,
    pub folders: Folders,
}

impl Place {
    /// The file itself; `None` for a key that can't name one.
    pub fn path(&self) -> Option<PathBuf> {
        rules::path(&self.dir, &self.key)
    }
}

/// Why the rules can't be shown or changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// The script doesn't parse, or uses Sieve Sealwax doesn't run.
    Script(sieve::Error),
    /// Over 1 MiB, not UTF-8, or not readable.
    File,
}

/// The rules as read.
#[derive(Clone)]
pub struct Loaded {
    pub set: Result<RuleSet, Unreadable>,
    /// The file's modification time before it was read; `None` without a file.
    pub modified: Option<SystemTime>,
}

/// Why a save didn't happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The file changed since it was read (a text editor saved it): nothing was written.
    Changed,
    /// The rules can't be written as they are.
    Invalid(Invalid),
    /// The file couldn't be read or written.
    Io,
}

/// Reads the account's rules. The time is taken before the text, so a change between them
/// shows as a change at the next save, never as none.
pub fn load(place: &Place) -> Loaded {
    let modified = match rules::modified(&place.dir, &place.key) {
        Ok(modified) => modified,
        Err(_) => {
            return Loaded {
                set: Err(Unreadable::File),
                modified: None,
            };
        }
    };
    let set = match rules::read(&place.dir, &place.key) {
        Ok(None) => Ok(RuleSet::default()),
        Ok(Some(text)) => RuleSet::read(&text, &place.folders).map_err(Unreadable::Script),
        Err(_) => Err(Unreadable::File),
    };
    Loaded { set, modified }
}

/// Writes `set` when the file is still as it was at `expected`; its new modification time.
pub fn save(
    place: &Place,
    set: &RuleSet,
    expected: Option<SystemTime>,
) -> Result<Option<SystemTime>, SaveError> {
    let now = rules::modified(&place.dir, &place.key).map_err(|_| SaveError::Io)?;
    if now != expected {
        return Err(SaveError::Changed);
    }
    write(place, set)
}

fn write(place: &Place, set: &RuleSet) -> Result<Option<SystemTime>, SaveError> {
    let text = set.render(&place.folders).map_err(SaveError::Invalid)?;
    rules::write(&place.dir, &place.key, &text).map_err(|_| SaveError::Io)?;
    rules::modified(&place.dir, &place.key).map_err(|_| SaveError::Io)
}

/// Reads the account's rules, lets `edit` change them and writes them when it says it did:
/// Always Junk and Never Junk from a message, and their Undo. Whether `edit` changed them.
pub fn change(
    place: &Place,
    edit: impl FnOnce(&mut RuleSet) -> bool,
) -> Result<bool, ChangeError> {
    let loaded = load(place);
    let mut set = loaded.set.map_err(ChangeError::Unreadable)?;
    if !edit(&mut set) {
        return Ok(false);
    }
    save(place, &set, loaded.modified).map_err(ChangeError::Save)?;
    Ok(true)
}

/// Why [`change`] didn't change the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeError {
    Unreadable(Unreadable),
    Save(SaveError),
}

/// Whether the script at `path` can run, for the banner once the file changed on disk:
/// `Ok(())` also without a file.
pub fn check(dir: &Path, key: &AccountKey) -> Result<(), Unreadable> {
    match rules::read(dir, key) {
        Ok(None) => Ok(()),
        Ok(Some(text)) => sieve::compile(&text).map(drop).map_err(Unreadable::Script),
        Err(_) => Err(Unreadable::File),
    }
}

type Job = Box<dyn FnOnce() + Send>;

thread_local! {
    /// The jobs waiting their turn; the runner starts with the first.
    static QUEUE: RefCell<Option<async_channel::Sender<Job>>> = const { RefCell::new(None) };
}

/// Runs `job` on a worker thread after every job handed in before it; resolves on the GTK
/// thread with its result, `None` when the worker failed.
pub async fn run<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (answer, result) = async_channel::bounded(1);
    let job: Job = Box::new(move || {
        let _ = answer.send_blocking(job());
    });
    let queue = QUEUE.with(|queue| {
        queue
            .borrow_mut()
            .get_or_insert_with(|| {
                let (sender, jobs) = async_channel::unbounded::<Job>();
                glib::spawn_future_local(async move {
                    while let Ok(job) = jobs.recv().await {
                        if gio::spawn_blocking(job).await.is_err() {
                            tracing::warn!("a rules file job failed");
                        }
                    }
                });
                sender
            })
            .clone()
    });
    queue.send(job).await.ok()?;
    result.recv().await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_infrastructure::xdg::SealwaxDirs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sealwax-rules-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn place(root: &Path) -> Place {
        Place {
            dir: SealwaxDirs::under(root).rules_dir(),
            key: AccountKey("work".to_owned()),
            folders: Folders::default(),
        }
    }

    #[test]
    fn rules_are_read_changed_and_never_written_over_an_edit() {
        let root = scratch("change");
        let place = place(&root);
        let empty = load(&place);
        assert!(empty.set.as_ref().unwrap().items.is_empty());
        assert_eq!(empty.modified, None, "no file");
        assert_eq!(check(&place.dir, &place.key), Ok(()));

        assert_eq!(
            change(&place, |set| set.always_junk("offers@example.test")),
            Ok(true)
        );
        assert_eq!(
            change(&place, |set| set.always_junk("offers@example.test")),
            Ok(false),
            "already listed: nothing written"
        );
        let loaded = load(&place);
        let set = loaded.set.clone().unwrap();
        assert_eq!(set.blocked.addresses, ["offers@example.test"]);
        assert!(loaded.modified.is_some());

        // A text editor saves the file meanwhile: the dialog's save is refused.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let edited = "require [\"fileinto\"];\nif header :contains \"subject\" \"x\" { \
                      fileinto \"Arkiv\"; }\n";
        rules::write(&place.dir, &place.key, edited).unwrap();
        assert_eq!(
            save(&place, &set, loaded.modified),
            Err(SaveError::Changed)
        );
        assert_eq!(
            rules::read(&place.dir, &place.key).unwrap().as_deref(),
            Some(edited)
        );
        let again = load(&place);
        assert!(again.set.unwrap().is_hand_written());

        rules::write(&place.dir, &place.key, "if foo { keep; }\n").unwrap();
        assert!(matches!(load(&place).set, Err(Unreadable::Script(_))));
        assert!(matches!(
            change(&place, |set| set.never_junk("kari@example.test")),
            Err(ChangeError::Unreadable(Unreadable::Script(_)))
        ));
        assert!(matches!(
            check(&place.dir, &place.key),
            Err(Unreadable::Script(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
