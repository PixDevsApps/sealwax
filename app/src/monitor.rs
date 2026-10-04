//! Watches a directory that may not exist yet (Omarchy's `current/`, `~/.config/fontconfig/`,
//! `~/.config/omarchy/`), ported from Stet. The parent is always watched too, so the directory
//! appearing, being renamed into place or disappearing is seen at once instead of after
//! GLib's poll for a missing path. Triggers are debounced; if no monitor can be created, the
//! owner is polled. Only reads: nothing here writes to the watched directories.

use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const DEBOUNCE: Duration = Duration::from_millis(150);
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Decides whether an event inside the watched directory is a trigger: the event, the file's
/// name and, for renames, the new name.
pub type TriggerRule = Box<dyn Fn(gio::FileMonitorEvent, &str, Option<&str>) -> bool>;

pub struct DirWatch {
    dir: PathBuf,
    name: String,
    rule: TriggerRule,
    on_trigger: Box<dyn Fn()>,
    parent_monitor: RefCell<Option<gio::FileMonitor>>,
    dir_monitor: RefCell<Option<gio::FileMonitor>>,
    debounce: RefCell<Option<glib::SourceId>>,
    poll: RefCell<Option<glib::SourceId>>,
}

impl DirWatch {
    pub fn start(dir: PathBuf, rule: TriggerRule, on_trigger: impl Fn() + 'static) -> Rc<Self> {
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let watch = Rc::new(Self {
            dir,
            name,
            rule,
            on_trigger: Box::new(on_trigger),
            parent_monitor: RefCell::new(None),
            dir_monitor: RefCell::new(None),
            debounce: RefCell::new(None),
            poll: RefCell::new(None),
        });
        let parent = watch.dir.parent().map(Path::to_path_buf);
        let parent_monitor =
            parent.and_then(|parent| monitor(&watch, &parent, Self::on_parent_event));
        let watching_parent = parent_monitor.is_some();
        watch.parent_monitor.replace(parent_monitor);
        watch.sync_dir_monitor();
        if !watching_parent {
            watch.start_polling();
        }
        watch
    }

    /// Watches `dir` itself while it exists. Returns whether that changed.
    fn sync_dir_monitor(self: &Rc<Self>) -> bool {
        let exists = self.dir.is_dir();
        let watching = self.dir_monitor.borrow().is_some();
        if exists == watching {
            return false;
        }
        let monitor = if exists {
            let monitor = monitor(self, &self.dir, Self::on_dir_event);
            if monitor.is_none() {
                self.start_polling();
            }
            monitor
        } else {
            None
        };
        if let Some(old) = self.dir_monitor.replace(monitor) {
            old.cancel();
        }
        true
    }

    fn on_parent_event(
        self: &Rc<Self>,
        event: gio::FileMonitorEvent,
        file: &str,
        other: Option<&str>,
    ) {
        use gio::FileMonitorEvent as E;
        let about_dir = file == self.name || other == Some(self.name.as_str());
        let appeared_or_left = matches!(
            event,
            E::Created | E::MovedIn | E::Renamed | E::Deleted | E::MovedOut
        );
        let changed = self.sync_dir_monitor();
        if changed || (about_dir && appeared_or_left) {
            self.trigger();
        }
    }

    fn on_dir_event(
        self: &Rc<Self>,
        event: gio::FileMonitorEvent,
        file: &str,
        other: Option<&str>,
    ) {
        if (self.rule)(event, file, other) {
            self.trigger();
        }
    }

    /// Restarts the debounce; the owner's callback runs 150 ms after the last trigger.
    pub fn trigger(self: &Rc<Self>) {
        if let Some(pending) = self.debounce.take() {
            pending.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(DEBOUNCE, move || {
            if let Some(watch) = weak.upgrade() {
                watch.debounce.replace(None);
                (watch.on_trigger)();
            }
        });
        self.debounce.replace(Some(source));
    }

    fn start_polling(self: &Rc<Self>) {
        if self.poll.borrow().is_some() {
            return;
        }
        tracing::warn!("cannot watch a theme or font directory; polling every 2 s");
        let weak: Weak<Self> = Rc::downgrade(self);
        let source = glib::timeout_add_local(POLL_INTERVAL, move || {
            let Some(watch) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            (watch.on_trigger)();
            glib::ControlFlow::Continue
        });
        self.poll.replace(Some(source));
    }
}

impl Drop for DirWatch {
    fn drop(&mut self) {
        for monitor in [self.parent_monitor.take(), self.dir_monitor.take()]
            .into_iter()
            .flatten()
        {
            monitor.cancel();
        }
        for source in [self.debounce.take(), self.poll.take()]
            .into_iter()
            .flatten()
        {
            source.remove();
        }
    }
}

fn base_name(file: &gio::File) -> String {
    file.basename()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn monitor(
    watch: &Rc<DirWatch>,
    dir: &Path,
    handler: fn(&Rc<DirWatch>, gio::FileMonitorEvent, &str, Option<&str>),
) -> Option<gio::FileMonitor> {
    let monitor = match gio::File::for_path(dir).monitor_directory(
        gio::FileMonitorFlags::WATCH_MOVES,
        None::<&gio::Cancellable>,
    ) {
        Ok(monitor) => monitor,
        Err(error) => {
            tracing::warn!(%error, "could not create a file monitor");
            return None;
        }
    };
    let weak = Rc::downgrade(watch);
    monitor.connect_changed(move |_, file, other, event| {
        if let Some(watch) = weak.upgrade() {
            let other = other.map(base_name);
            handler(&watch, event, &base_name(file), other.as_deref());
        }
    });
    Some(monitor)
}
