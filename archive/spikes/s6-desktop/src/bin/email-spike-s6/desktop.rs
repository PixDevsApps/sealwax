//! What a background mail client listens to besides the server: logind's `PrepareForSleep` on
//! the system bus (read-only subscription) and GIO's network monitor.

use email_spike_s6::clock::{boot_ns, mono_ns, real_ns};
use gtk4::gio;
use gtk4::prelude::*;

#[derive(Clone, Debug)]
pub enum DesktopEvent {
    PrepareForSleep {
        sleeping: bool,
        mono: i64,
        boot: i64,
        real: i64,
    },
    Network {
        mono: i64,
        available: bool,
        connectivity: String,
        metered: bool,
    },
}

pub struct Watch {
    _sleep: Option<gio::SignalSubscription>,
    _monitor: gio::NetworkMonitor,
    pub monitor_type: String,
    pub system_bus: Result<(), String>,
}

pub fn watch(sender: async_channel::Sender<DesktopEvent>) -> Watch {
    let system = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE);
    let (sleep, system_bus) = match system {
        Ok(connection) => {
            let sender = sender.clone();
            let subscription = connection.subscribe_to_signal(
                Some("org.freedesktop.login1"),
                Some("org.freedesktop.login1.Manager"),
                Some("PrepareForSleep"),
                Some("/org/freedesktop/login1"),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    if let Some((sleeping,)) = signal.parameters.get::<(bool,)>() {
                        let _ = sender.try_send(DesktopEvent::PrepareForSleep {
                            sleeping,
                            mono: mono_ns(),
                            boot: boot_ns(),
                            real: real_ns(),
                        });
                    }
                },
            );
            (Some(subscription), Ok(()))
        }
        Err(error) => (None, Err(error.to_string())),
    };
    let monitor = gio::NetworkMonitor::default();
    let monitor_type = monitor.type_().name().to_owned();
    monitor.connect_network_changed(move |monitor, available| {
        let _ = sender.try_send(DesktopEvent::Network {
            mono: mono_ns(),
            available,
            connectivity: format!("{:?}", monitor.connectivity()),
            metered: monitor.is_network_metered(),
        });
    });
    Watch {
        _sleep: sleep,
        _monitor: monitor,
        monitor_type,
        system_bus,
    }
}
