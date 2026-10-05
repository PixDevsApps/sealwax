//! What the desktop says besides the engine (ADR-003 amendment, S6): logind's
//! `PrepareForSleep` on the system bus, a read-only subscription that calls no logind method,
//! and GIO's network monitor. The app reconnects after a resume and when the network comes
//! back, and says it is offline while the network is gone (ML-048).
//!
//! Nothing here blocks the GTK thread: the system bus is connected asynchronously, and the
//! network monitor, whose NetworkManager backend connects to the system bus synchronously, is
//! created on a thread of its own. GIO keeps it as the process's default and emits its signals
//! on the main context.

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// A change the app acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopEvent {
    /// The machine woke up: logind's `PrepareForSleep(false)`.
    Resumed,
    /// The desktop has a network (`true`) or lost it.
    Network(bool),
}

/// Keeps the subscriptions alive; dropping it ends them.
#[derive(Default)]
pub struct DesktopWatch {
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
pub fn watch(handler: impl Fn(DesktopEvent) + 'static) -> Rc<DesktopWatch> {
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
        handler(DesktopEvent::Network(available));
        let changed = handler.clone();
        let id = monitor.connect_network_changed(move |_, available| {
            changed(DesktopEvent::Network(available));
        });
        watch.network.replace(Some((monitor, id)));
    });
    watch
}
