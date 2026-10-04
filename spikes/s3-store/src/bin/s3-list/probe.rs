//! Frame and binding measurements. Every frame records the time GTK spent on it (from the frame
//! clock's `before-paint` to the end of `after-paint`) and, once GDK has them, its
//! `GdkFrameTimings` frame and presentation times. Script steps wait for "the first painted
//! frame in which a condition holds".

use email_spike_s3::stats::{Summary, ms, summarize};
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Frame {
    pub phase: &'static str,
    pub counter: i64,
    pub cpu_ms: f64,
    pub layout_ms: f64,
    pub paint_ms: f64,
    pub placeholders: i64,
    pub binds: u32,
    pub frame_time: Option<i64>,
    pub presentation_time: Option<i64>,
    pub refresh_interval: Option<i64>,
}

type Condition = Box<dyn Fn(&Probe) -> bool>;

struct Waiter {
    condition: Condition,
    done: async_channel::Sender<Instant>,
}

#[derive(Default)]
pub struct Probe {
    bound: RefCell<BTreeMap<u32, u32>>,
    bound_ids: RefCell<HashMap<i64, u32>>,
    placeholders: Cell<i64>,
    binds: Cell<u32>,
    phase: Cell<&'static str>,
    frames: RefCell<Vec<Frame>>,
    pending: RefCell<VecDeque<usize>>,
    frame_start: Cell<Option<Instant>>,
    layout_end: Cell<Option<Instant>>,
    paint_end: Cell<Option<Instant>>,
    waiters: RefCell<Vec<Waiter>>,
}

impl Probe {
    pub fn new() -> Rc<Self> {
        let probe = Rc::new(Self::default());
        probe.phase.set("startup");
        probe
    }

    pub fn bind(&self, position: u32, placeholder: bool, id: Option<i64>) {
        *self.bound.borrow_mut().entry(position).or_default() += 1;
        if let Some(id) = id {
            *self.bound_ids.borrow_mut().entry(id).or_default() += 1;
        }
        if placeholder {
            self.placeholders.set(self.placeholders.get() + 1);
        }
        self.binds.set(self.binds.get() + 1);
    }

    pub fn unbind(&self, position: u32, placeholder: bool, id: Option<i64>) {
        let mut bound = self.bound.borrow_mut();
        if let Some(count) = bound.get_mut(&position) {
            *count -= 1;
            if *count == 0 {
                bound.remove(&position);
            }
        }
        if let Some(id) = id {
            let mut ids = self.bound_ids.borrow_mut();
            if let Some(count) = ids.get_mut(&id) {
                *count -= 1;
                if *count == 0 {
                    ids.remove(&id);
                }
            }
        }
        if placeholder {
            self.placeholders.set(self.placeholders.get() - 1);
        }
    }

    /// A bound placeholder received its row.
    pub fn resolved(&self, id: i64) {
        self.placeholders.set(self.placeholders.get() - 1);
        *self.bound_ids.borrow_mut().entry(id).or_default() += 1;
    }

    pub fn placeholders(&self) -> i64 {
        self.placeholders.get()
    }

    pub fn has_position(&self, position: u32) -> bool {
        self.bound.borrow().contains_key(&position)
    }

    pub fn has_id(&self, id: i64) -> bool {
        self.bound_ids.borrow().contains_key(&id)
    }

    pub fn bound_rows(&self) -> usize {
        self.bound
            .borrow()
            .values()
            .map(|count| *count as usize)
            .sum()
    }

    pub fn set_phase(&self, phase: &'static str) {
        self.phase.set(phase);
    }

    pub fn frames(&self) -> Vec<Frame> {
        self.frames.borrow().clone()
    }

    /// Resolves at the end of the first painted frame in which `condition` holds, or `None`
    /// after `timeout`. Registers immediately, so a frame painted before the caller awaits counts.
    pub fn until<F: Fn(&Probe) -> bool + 'static>(
        self: &Rc<Self>,
        timeout: Duration,
        condition: F,
    ) -> impl std::future::Future<Output = Option<Instant>> + use<F> {
        let (done, wait) = async_channel::bounded(1);
        self.waiters.borrow_mut().push(Waiter {
            condition: Box::new(condition),
            done,
        });
        async move {
            let timer = glib::timeout_future(timeout);
            futures_select(wait.recv(), timer).await
        }
    }

    pub fn attach(self: &Rc<Self>, clock: &gdk::FrameClock) {
        let probe = self.clone();
        clock.connect_before_paint(move |_| probe.frame_start.set(Some(Instant::now())));
        let probe = self.clone();
        clock.connect_local("layout", true, move |_| {
            probe.layout_end.set(Some(Instant::now()));
            None
        });
        let probe = self.clone();
        clock.connect_local("paint", true, move |_| {
            probe.paint_end.set(Some(Instant::now()));
            None
        });
        let probe = self.clone();
        clock.connect_local("after-paint", true, move |values| {
            let clock = values[0].get::<gdk::FrameClock>().ok()?;
            probe.after_paint(&clock);
            None
        });
    }

    fn after_paint(&self, clock: &gdk::FrameClock) {
        let end = Instant::now();
        let start = self.frame_start.take();
        let cpu_ms = start.map_or(0.0, |start| ms(end - start));
        let layout_end = self.layout_end.take();
        let paint_end = self.paint_end.take();
        let layout_ms = match (start, layout_end) {
            (Some(start), Some(layout)) if layout >= start => ms(layout - start),
            _ => 0.0,
        };
        let paint_ms = match (layout_end.or(start), paint_end) {
            (Some(from), Some(paint)) if paint >= from => ms(paint - from),
            _ => 0.0,
        };
        {
            let mut frames = self.frames.borrow_mut();
            frames.push(Frame {
                phase: self.phase.get(),
                counter: clock.frame_counter(),
                cpu_ms,
                layout_ms,
                paint_ms,
                placeholders: self.placeholders.get(),
                binds: self.binds.replace(0),
                frame_time: None,
                presentation_time: None,
                refresh_interval: None,
            });
            self.pending.borrow_mut().push_back(frames.len() - 1);
            let history_start = clock.history_start();
            self.pending.borrow_mut().retain(|index| {
                let frame = &mut frames[*index];
                if frame.counter < history_start {
                    return false;
                }
                match clock.timings(frame.counter) {
                    Some(timings) if timings.is_complete() => {
                        frame.frame_time = Some(timings.frame_time());
                        let presented = timings.presentation_time();
                        frame.presentation_time = (presented != 0).then_some(presented);
                        let refresh = timings.refresh_interval();
                        frame.refresh_interval = (refresh != 0).then_some(refresh);
                        false
                    }
                    _ => true,
                }
            });
        }
        let ready: Vec<Waiter> = {
            let mut waiters = self.waiters.borrow_mut();
            let (ready, waiting): (Vec<Waiter>, Vec<Waiter>) = waiters
                .drain(..)
                .partition(|waiter| (waiter.condition)(self));
            *waiters = waiting;
            ready
        };
        for waiter in ready {
            let _ = waiter.done.try_send(end);
        }
    }
}

/// The first of a channel receive and a timer.
async fn futures_select(
    recv: async_channel::Recv<'_, Instant>,
    timer: impl std::future::Future<Output = ()>,
) -> Option<Instant> {
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

pub struct PhaseStats {
    pub frames: usize,
    pub interval: Summary,
    pub presented: Summary,
    pub cpu: Summary,
    pub layout: Summary,
    pub paint: Summary,
    pub refresh_ms: f64,
    pub over_budget: usize,
    pub missed_vblank: usize,
    pub with_placeholders: usize,
    pub binds_per_frame: f64,
}

pub fn phase_stats(frames: &[Frame], phase: &str) -> Option<PhaseStats> {
    let selected: Vec<&Frame> = frames.iter().filter(|frame| frame.phase == phase).collect();
    if selected.is_empty() {
        return None;
    }
    let pairs = |pick: fn(&Frame) -> Option<i64>| -> Vec<f64> {
        selected
            .windows(2)
            .filter(|pair| pair[1].counter == pair[0].counter + 1)
            .filter_map(|pair| Some((pick(pair[1])? - pick(pair[0])?) as f64 / 1000.0))
            .collect()
    };
    let intervals = pairs(|frame| frame.frame_time);
    let presented = pairs(|frame| frame.presentation_time);
    let mut refresh: Vec<f64> = selected
        .iter()
        .filter_map(|frame| frame.refresh_interval)
        .map(|value| value as f64 / 1000.0)
        .collect();
    refresh.sort_by(f64::total_cmp);
    let refresh_ms = refresh.get(refresh.len() / 2).copied().unwrap_or(0.0);
    let cpu: Vec<f64> = selected.iter().map(|frame| frame.cpu_ms).collect();
    let basis = if presented.is_empty() {
        &intervals
    } else {
        &presented
    };
    Some(PhaseStats {
        frames: selected.len(),
        interval: summarize(&intervals),
        presented: summarize(&presented),
        cpu: summarize(&cpu),
        layout: summarize(
            &selected
                .iter()
                .map(|frame| frame.layout_ms)
                .collect::<Vec<_>>(),
        ),
        paint: summarize(
            &selected
                .iter()
                .map(|frame| frame.paint_ms)
                .collect::<Vec<_>>(),
        ),
        refresh_ms,
        over_budget: basis.iter().filter(|value| **value > 1000.0 / 60.0).count(),
        missed_vblank: if refresh_ms > 0.0 {
            basis
                .iter()
                .filter(|value| **value > 1.5 * refresh_ms)
                .count()
        } else {
            0
        },
        with_placeholders: selected
            .iter()
            .filter(|frame| frame.placeholders > 0)
            .count(),
        binds_per_frame: selected
            .iter()
            .map(|frame| f64::from(frame.binds))
            .sum::<f64>()
            / selected.len() as f64,
    })
}

fn field(text: &str, key: &str) -> f64 {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0)
        / 1024.0
}

/// MiB from `/proc/self/status` (`VmRSS`, `VmHWM`, `RssAnon`, `RssFile`, `RssShmem`) and
/// `/proc/self/smaps_rollup` (`Pss`, `Pss_Anon`, `Pss_File`).
pub fn memory() -> [(String, f64); 8] {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    [
        ("VmRSS", &status),
        ("VmHWM", &status),
        ("RssAnon", &status),
        ("RssFile", &status),
        ("RssShmem", &status),
        ("Pss", &rollup),
        ("Pss_Anon", &rollup),
        ("Pss_File", &rollup),
    ]
    .map(|(key, text)| (key.to_owned(), field(text, key)))
}
