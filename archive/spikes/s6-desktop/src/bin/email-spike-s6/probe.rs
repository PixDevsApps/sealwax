//! Frame observation. Every painted frame records when GTK started it (`before-paint`), when its
//! layout ended and when painting ended (`after-paint`); callers wait for "the first painted
//! frame in which a condition holds" and, later, for that frame's presentation time.

use email_spike_s6::clock::mono_ns;
use gtk4::gdk;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default)]
pub struct Painted {
    pub counter: i64,
    pub start_ns: Option<i64>,
    pub layout_end_ns: Option<i64>,
    pub end_ns: i64,
    pub bound: i64,
    pub placeholders: i64,
    pub binds: u32,
    /// How many frames this window painted before and including this one.
    pub index: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Presented {
    /// `CLOCK_MONOTONIC` ns, from GDK's frame timings (the compositor's presentation feedback).
    pub at_ns: i64,
    pub refresh_ns: i64,
}

type Condition = Box<dyn Fn(&Probe) -> bool>;

struct Waiter {
    condition: Condition,
    done: async_channel::Sender<Painted>,
}

#[derive(Default)]
pub struct Probe {
    bound: Cell<i64>,
    placeholders: Cell<i64>,
    binds: Cell<u32>,
    frames: Cell<u32>,
    frame_start: Cell<Option<i64>>,
    layout_end: Cell<Option<i64>>,
    waiters: RefCell<Vec<Waiter>>,
    presentations: RefCell<Vec<(i64, async_channel::Sender<Presented>)>>,
    history: RefCell<Vec<Painted>>,
}

impl Probe {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn bind(&self, placeholder: bool) {
        self.bound.set(self.bound.get() + 1);
        if placeholder {
            self.placeholders.set(self.placeholders.get() + 1);
        }
        self.binds.set(self.binds.get() + 1);
    }

    pub fn unbind(&self, placeholder: bool) {
        self.bound.set(self.bound.get() - 1);
        if placeholder {
            self.placeholders.set(self.placeholders.get() - 1);
        }
    }

    pub fn resolved(&self) {
        self.placeholders.set(self.placeholders.get() - 1);
    }

    pub fn bound(&self) -> i64 {
        self.bound.get()
    }

    pub fn placeholders(&self) -> i64 {
        self.placeholders.get()
    }

    /// The first painted frames of this window, in order.
    pub fn history(&self) -> Vec<Painted> {
        self.history.borrow().clone()
    }

    /// The end of the first painted frame in which `condition` holds, or `None` after
    /// `timeout`. Registers now, so a frame painted before the caller awaits still counts.
    pub fn until<F: Fn(&Probe) -> bool + 'static>(
        self: &Rc<Self>,
        timeout: Duration,
        condition: F,
    ) -> impl std::future::Future<Output = Option<Painted>> + use<F> {
        let (done, wait) = async_channel::bounded(1);
        self.waiters.borrow_mut().push(Waiter {
            condition: Box::new(condition),
            done,
        });
        async move { first(wait.recv(), glib::timeout_future(timeout)).await }
    }

    /// The presentation time of frame `counter`, once GDK has it.
    pub fn presented(
        self: &Rc<Self>,
        counter: i64,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Option<Presented>> + use<> {
        let (done, wait) = async_channel::bounded(1);
        self.presentations.borrow_mut().push((counter, done));
        async move { first(wait.recv(), glib::timeout_future(timeout)).await }
    }

    pub fn attach(self: &Rc<Self>, clock: &gdk::FrameClock) {
        let probe = self.clone();
        clock.connect_before_paint(move |_| probe.frame_start.set(Some(mono_ns())));
        let probe = self.clone();
        clock.connect_layout(move |_| probe.layout_end.set(Some(mono_ns())));
        let probe = self.clone();
        clock.connect_after_paint(move |clock| probe.after_paint(clock));
    }

    fn after_paint(&self, clock: &gdk::FrameClock) {
        let end_ns = mono_ns();
        self.frames.set(self.frames.get() + 1);
        let painted = Painted {
            counter: clock.frame_counter(),
            start_ns: self.frame_start.take(),
            layout_end_ns: self.layout_end.take(),
            end_ns,
            bound: self.bound.get(),
            placeholders: self.placeholders.get(),
            binds: self.binds.replace(0),
            index: self.frames.get(),
        };
        {
            let mut history = self.history.borrow_mut();
            if history.len() < 64 {
                history.push(painted);
            }
        }
        let ready: Vec<Waiter> = {
            let mut waiters = self.waiters.borrow_mut();
            let (ready, waiting) = waiters
                .drain(..)
                .partition(|waiter| (waiter.condition)(self));
            *waiters = waiting;
            ready
        };
        for waiter in ready {
            let _ = waiter.done.try_send(painted);
        }
        let history_start = clock.history_start();
        self.presentations.borrow_mut().retain(|(counter, done)| {
            if *counter < history_start {
                return false;
            }
            match clock.timings(*counter) {
                Some(timings) if timings.is_complete() => {
                    let presented = timings.presentation_time();
                    if presented != 0 {
                        let _ = done.try_send(Presented {
                            at_ns: presented * 1000,
                            refresh_ns: timings.refresh_interval() * 1000,
                        });
                    }
                    false
                }
                _ => true,
            }
        });
    }
}

/// Whichever of a channel receive and a timer finishes first.
async fn first<T>(
    recv: async_channel::Recv<'_, T>,
    timer: impl std::future::Future<Output = ()>,
) -> Option<T> {
    use std::pin::pin;
    use std::task::Poll;
    let mut recv = pin!(recv);
    let mut timer = pin!(timer);
    std::future::poll_fn(move |context| {
        if let Poll::Ready(value) = recv.as_mut().poll(context) {
            return Poll::Ready(value.ok());
        }
        if timer.as_mut().poll(context).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

/// Waits for an async value, with a timeout.
pub async fn with_timeout<T>(
    future: impl std::future::Future<Output = T>,
    timeout: Duration,
) -> Option<T> {
    use std::pin::pin;
    use std::task::Poll;
    let mut future = pin!(future);
    let mut timer = pin!(glib::timeout_future(timeout));
    std::future::poll_fn(move |context| {
        if let Poll::Ready(value) = future.as_mut().poll(context) {
            return Poll::Ready(Some(value));
        }
        if timer.as_mut().poll(context).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}
