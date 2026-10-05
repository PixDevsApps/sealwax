//! The app's end of the engine's channels (ADR-003): `engine::api` commands go out on an
//! async-channel, and the engine's events come back to the subscribers on the GTK main loop.
//! Nothing here is tokio's. Until an engine is attached every command fails with
//! [`NotConnected`], so the setup assistant can say so instead of waiting.

use gtk4::glib;
use sealwax_engine::api::{Command, Event, RequestId};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::{Rc, Weak};

/// No engine is attached, or it stopped listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotConnected;

impl fmt::Display for NotConnected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not connected to the mail engine")
    }
}

impl std::error::Error for NotConnected {}

type Handler = Rc<dyn Fn(&Event)>;

#[derive(Default)]
struct Inner {
    commands: RefCell<Option<async_channel::Sender<Command>>>,
    listener: RefCell<Option<glib::JoinHandle<()>>>,
    subscribers: RefCell<Vec<(u64, Handler)>>,
    next_subscriber: Cell<u64>,
    last_request: Cell<RequestId>,
}

/// Shared by everything in the app that talks to the engine; clones are the same link.
#[derive(Clone, Default)]
pub struct EngineLink {
    inner: Rc<Inner>,
}

impl EngineLink {
    /// A link with no engine behind it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Connects the link to an engine: commands go to `commands`, and every event from
    /// `events` reaches the subscribers on the GTK main loop, in order. A link attached
    /// before is replaced.
    pub fn attach(
        &self,
        commands: async_channel::Sender<Command>,
        events: async_channel::Receiver<Event>,
    ) {
        self.detach();
        self.inner.commands.replace(Some(commands));
        let weak = Rc::downgrade(&self.inner);
        let listener = glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                dispatch(&inner, &event);
            }
        });
        self.inner.listener.replace(Some(listener));
    }

    /// Forgets the engine; later commands fail with [`NotConnected`].
    pub fn detach(&self) {
        self.inner.commands.take();
        if let Some(listener) = self.inner.listener.take() {
            listener.abort();
        }
    }

    pub fn is_connected(&self) -> bool {
        self.inner
            .commands
            .borrow()
            .as_ref()
            .is_some_and(|commands| !commands.is_closed())
    }

    /// A request id no earlier command of this link used.
    pub fn next_request(&self) -> RequestId {
        let request = self.inner.last_request.get() + 1;
        self.inner.last_request.set(request);
        request
    }

    /// Hands `command` to the engine without waiting. A full channel takes it a moment
    /// later, on the main loop.
    pub fn send(&self, command: Command) -> Result<(), NotConnected> {
        let commands = self.inner.commands.borrow().clone().ok_or(NotConnected)?;
        match commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(async_channel::TrySendError::Closed(_)) => Err(NotConnected),
            Err(async_channel::TrySendError::Full(command)) => {
                glib::spawn_future_local(async move {
                    if commands.send(command).await.is_err() {
                        tracing::warn!("the engine stopped before it took a command");
                    }
                });
                Ok(())
            }
        }
    }

    /// Calls `handler` with every event until the returned subscription is dropped.
    pub fn subscribe(&self, handler: impl Fn(&Event) + 'static) -> Subscription {
        let id = self.inner.next_subscriber.get();
        self.inner.next_subscriber.set(id + 1);
        self.inner
            .subscribers
            .borrow_mut()
            .push((id, Rc::new(handler)));
        Subscription {
            link: Rc::downgrade(&self.inner),
            id,
        }
    }
}

/// Every subscriber sees the event; one that unsubscribes meanwhile still sees this one.
fn dispatch(inner: &Inner, event: &Event) {
    let handlers: Vec<Handler> = inner
        .subscribers
        .borrow()
        .iter()
        .map(|(_, handler)| handler.clone())
        .collect();
    for handler in handlers {
        handler(event);
    }
}

/// Ends a [`EngineLink::subscribe`] when dropped.
pub struct Subscription {
    link: Weak<Inner>,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.link.upgrade() {
            inner
                .subscribers
                .borrow_mut()
                .retain(|(id, _)| *id != self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::DetectSource;

    fn drain(context: &glib::MainContext) {
        while context.iteration(false) {}
    }

    #[test]
    fn commands_fail_until_an_engine_is_attached() {
        let link = EngineLink::new();
        assert!(!link.is_connected());
        let detect = Command::Detect {
            request: link.next_request(),
            address: "test@example.test".to_owned(),
        };
        assert_eq!(link.send(detect), Err(NotConnected));
        assert_eq!(link.next_request(), 2);
    }

    #[test]
    fn events_reach_subscribers_in_order_until_they_unsubscribe() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let link = EngineLink::new();
                let (commands, engine) = async_channel::unbounded();
                let (events, receiver) = async_channel::unbounded();
                link.attach(commands, receiver);
                assert!(link.is_connected());
                link.send(Command::Shutdown).unwrap();
                assert!(matches!(engine.try_recv(), Ok(Command::Shutdown)));

                let seen = Rc::new(RefCell::new(Vec::new()));
                let record = seen.clone();
                let subscription = link.subscribe(move |event| {
                    if let Event::DetectProgress { request, .. } = event {
                        record.borrow_mut().push(*request);
                    }
                });
                for request in [1, 2] {
                    events
                        .try_send(Event::DetectProgress {
                            request,
                            source: DetectSource::Preset,
                        })
                        .unwrap();
                }
                drain(&context);
                assert_eq!(*seen.borrow(), [1, 2]);

                drop(subscription);
                events
                    .try_send(Event::DetectProgress {
                        request: 3,
                        source: DetectSource::Mx,
                    })
                    .unwrap();
                drain(&context);
                assert_eq!(*seen.borrow(), [1, 2]);

                drop(engine);
                assert!(!link.is_connected());
                assert_eq!(link.send(Command::Shutdown), Err(NotConnected));
                link.detach();
            })
            .unwrap();
    }
}
