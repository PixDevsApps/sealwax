//! What the desktop says besides the engine (ADR-003 amendment, S6): logind's
//! `PrepareForSleep` on the system bus, a read-only subscription that calls no logind method,
//! and GIO's network monitor. The app reconnects after a resume and when the network comes
//! back, and says it is offline while the network is gone (ML-048).
//!
//! Nothing here blocks the GTK thread: the system bus is connected asynchronously, and the
//! network monitor, whose NetworkManager backend connects to the system bus synchronously, is
//! created on a thread of its own. GIO keeps it as the process's default and emits its signals
//! on the main context.
//!
//! The monitor's first reading of "no network" is held back for [`FIRST_READING_GRACE`]: the
//! portal's monitor (in a Flatpak) says so until the portal first answers, a moment later, and
//! the "network back" that followed made every account reconnect while the engine was making
//! its first connections. A change within that time replaces the reading; a real "no network"
//! still shows once the time is up.

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// How long the monitor's first "no network" waits before it counts.
pub(crate) const FIRST_READING_GRACE: Duration = Duration::from_secs(1);

/// A change the app acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopEvent {
    /// The machine woke up: logind's `PrepareForSleep(false)`.
    Resumed,
    /// The desktop has a network (`true`) or lost it.
    Network(bool),
}

/// The monitor's first reading, held back while it says "no network".
#[derive(Debug, Default)]
pub(crate) struct FirstReading {
    held: Cell<bool>,
}

impl FirstReading {
    /// The reading when watching starts: what to report now.
    pub(crate) fn first(&self, available: bool) -> Option<bool> {
        if available {
            Some(true)
        } else {
            self.held.set(true);
            None
        }
    }

    /// A change from the monitor: always reported, and the held reading is dropped.
    pub(crate) fn changed(&self, available: bool) -> bool {
        self.held.set(false);
        available
    }

    /// [`FIRST_READING_GRACE`] is up: the monitor's state now, when the first reading is
    /// still held.
    pub(crate) fn grace_over(&self, available: bool) -> Option<bool> {
        self.held.replace(false).then_some(available)
    }
}

/// Keeps the subscriptions alive; dropping it ends them.
#[derive(Default)]
pub(crate) struct DesktopWatch {
    sleep: RefCell<Option<gio::SignalSubscription>>,
    network: RefCell<Option<(gio::NetworkMonitor, glib::SignalHandlerId)>>,
}

impl Drop for DesktopWatch {
    fn drop(&mut self) {
        if let Some((monitor, handler)) = self.network.take() {
            monitor.disconnect(handler);
        }
    }
}

/// Starts watching; `handler` runs on the GTK thread with every change, the network's state
/// first of all.
pub(crate) fn watch(handler: impl Fn(DesktopEvent) + 'static) -> Rc<DesktopWatch> {
    let handler: Rc<dyn Fn(DesktopEvent)> = Rc::new(handler);
    let watch = Rc::new(DesktopWatch::default());
    let weak = Rc::downgrade(&watch);
    let resumed = handler.clone();
    glib::spawn_future_local(async move {
        let connection = match gio::bus_get_future(gio::BusType::System).await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!("no system bus, so no resume signal: {error}");
                return;
            }
        };
        let Some(watch) = weak.upgrade() else {
            return;
        };
        let subscription = connection.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some("/org/freedesktop/login1"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if let Some((false,)) = signal.parameters.get::<(bool,)>() {
                    resumed(DesktopEvent::Resumed);
                }
            },
        );
        watch.sleep.replace(Some(subscription));
        tracing::info!("watching logind's resume signal");
    });
    let weak = Rc::downgrade(&watch);
    glib::spawn_future_local(async move {
        let (ready, created) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("sealwax-network-monitor".into())
            .spawn(move || {
                let _ = gio::NetworkMonitor::default();
                let _ = ready.send_blocking(());
            });
        if spawned.is_err() || created.recv().await.is_err() {
            tracing::warn!("the network monitor didn't start");
            return;
        }
        let Some(watch) = weak.upgrade() else {
            return;
        };
        let monitor = gio::NetworkMonitor::default();
        let available = monitor.is_network_available();
        tracing::info!(
            available,
            monitor = monitor.type_().name(),
            "watching the network"
        );
        let reading = Rc::new(FirstReading::default());
        if let Some(available) = reading.first(available) {
            handler(DesktopEvent::Network(available));
        } else {
            let (reading, handler, monitor) = (reading.clone(), handler.clone(), monitor.clone());
            glib::timeout_add_local_once(FIRST_READING_GRACE, move || {
                if let Some(available) = reading.grace_over(monitor.is_network_available()) {
                    handler(DesktopEvent::Network(available));
                }
            });
        }
        let changed = handler.clone();
        let id = monitor.connect_network_changed(move |_, available| {
            changed(DesktopEvent::Network(reading.changed(available)));
        });
        watch.network.replace(Some((monitor, id)));
    });
    watch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_no_network_waits_and_a_change_replaces_it() {
        let reading = FirstReading::default();
        assert_eq!(reading.first(true), Some(true));
        assert_eq!(reading.grace_over(true), None);

        // The portal's monitor: unavailable at first, available a moment later.
        let reading = FirstReading::default();
        assert_eq!(reading.first(false), None);
        assert!(reading.changed(true));
        assert_eq!(reading.grace_over(true), None, "nothing more to report");

        // Really offline: reported when the time is up.
        let reading = FirstReading::default();
        assert_eq!(reading.first(false), None);
        assert_eq!(reading.grace_over(false), Some(false));
        assert!(!reading.changed(false));
    }
}
