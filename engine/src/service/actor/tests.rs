//! The supervisor of an account's actor (M9, F15) with injected panics: started again after the
//! backoff, never in a tight loop, the count starting over after a long run, at once on
//! `Reconnect`, and never again once stopped; the log line names the account and nothing else.

use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};

fn quick() -> SyncConfig {
    SyncConfig {
        backoff_min: Duration::from_millis(40),
        backoff_max: Duration::from_millis(320),
        ..SyncConfig::default()
    }
}

/// When each start happened, and the counts `crashed` heard.
#[derive(Clone, Default)]
struct Record {
    starts: Arc<Mutex<Vec<Instant>>>,
    crashes: Arc<Mutex<Vec<u32>>>,
}

impl Record {
    fn started(&self) -> usize {
        let mut starts = self.starts.lock().unwrap();
        starts.push(Instant::now());
        starts.len()
    }

    fn starts(&self) -> Vec<Instant> {
        self.starts.lock().unwrap().clone()
    }

    fn crashes(&self) -> Vec<u32> {
        self.crashes.lock().unwrap().clone()
    }

    fn crashed(&self) -> impl FnMut(u32) + use<> {
        let crashes = self.crashes.clone();
        move |count| crashes.lock().unwrap().push(count)
    }
}

#[tokio::test]
async fn a_panicking_actor_starts_again_after_a_growing_backoff() {
    let config = quick();
    let (signal, signals) = watch::channel(Signal::default());
    let record = Record::default();
    let starts = record.clone();
    supervise(
        &config,
        signals,
        move || {
            let n = starts.started();
            async move {
                if n <= 4 {
                    panic!("injected panic {n}");
                }
            }
        },
        record.crashed(),
    )
    .await;
    drop(signal);
    assert_eq!(record.crashes(), [1, 2, 3, 4]);
    let starts = record.starts();
    assert_eq!(starts.len(), 5, "ended once the actor ended by itself");
    for (gap, ceiling) in starts
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .zip([40, 80, 160, 320])
    {
        let least = Duration::from_millis(ceiling / 2);
        assert!(
            gap >= least,
            "restarted after {gap:?}, before the backoff's {least:?}"
        );
    }
}

#[tokio::test]
async fn the_count_starts_over_after_a_long_run() {
    let config = quick();
    let (signal, signals) = watch::channel(Signal::default());
    let record = Record::default();
    let starts = record.clone();
    let backoff_max = config.backoff_max;
    supervise(
        &config,
        signals,
        move || {
            let n = starts.started();
            async move {
                if n <= 3 {
                    tokio::time::sleep(backoff_max).await;
                    panic!("injected panic {n} after a long run");
                }
            }
        },
        record.crashed(),
    )
    .await;
    drop(signal);
    assert_eq!(record.crashes(), [1, 1, 1]);
}

#[tokio::test]
async fn an_actor_that_ends_by_itself_stays_ended() {
    let config = quick();
    let (_signal, signals) = watch::channel(Signal::default());
    let record = Record::default();
    let starts = record.clone();
    supervise(
        &config,
        signals,
        move || {
            starts.started();
            async {}
        },
        record.crashed(),
    )
    .await;
    assert_eq!(record.starts().len(), 1);
    assert!(record.crashes().is_empty());
}

#[tokio::test]
async fn a_stop_ends_it_during_the_backoff_and_before_a_restart() {
    let config = SyncConfig {
        backoff_min: Duration::from_secs(30),
        backoff_max: Duration::from_secs(60),
        ..SyncConfig::default()
    };
    let (signal, signals) = watch::channel(Signal::default());
    let record = Record::default();
    let starts = record.clone();
    let supervised = tokio::spawn({
        let crashed = record.crashed();
        async move {
            supervise(
                &config,
                signals,
                move || {
                    starts.started();
                    async { panic!("injected panic") }
                },
                crashed,
            )
            .await;
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    signal.send_modify(|signal| signal.stop = Some(Stop::Drop));
    tokio::time::timeout(Duration::from_secs(5), supervised)
        .await
        .expect("a stop during the backoff ends the supervisor")
        .unwrap();
    assert_eq!(record.starts().len(), 1);

    // A panic while the stop is under way (the actor saw it too late) isn't restarted.
    let (signal, signals) = watch::channel(Signal::default());
    let record = Record::default();
    let starts = record.clone();
    supervise(
        &quick(),
        signals,
        move || {
            starts.started();
            signal.send_modify(|signal| signal.stop = Some(Stop::Drop));
            async { panic!("injected panic after the stop") }
        },
        record.crashed(),
    )
    .await;
    assert_eq!(record.starts().len(), 1);
    assert!(record.crashes().is_empty());
}

#[tokio::test]
async fn a_reconnect_starts_it_at_once() {
    let config = SyncConfig {
        backoff_min: Duration::from_secs(30),
        backoff_max: Duration::from_secs(60),
        ..SyncConfig::default()
    };
    let (signal, signals) = watch::channel(Signal::default());
    let record = Record::default();
    let starts = record.clone();
    let supervised = tokio::spawn({
        let crashed = record.crashed();
        async move {
            supervise(
                &config,
                signals,
                move || {
                    let n = starts.started();
                    async move {
                        if n == 1 {
                            panic!("injected panic");
                        }
                    }
                },
                crashed,
            )
            .await;
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let asked = Instant::now();
    signal.send_modify(|signal| signal.reconnects += 1);
    tokio::time::timeout(Duration::from_secs(5), supervised)
        .await
        .expect("the actor started again and ended")
        .unwrap();
    let starts = record.starts();
    assert_eq!(starts.len(), 2);
    assert!(starts[1] - asked < Duration::from_secs(1));
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_crash_is_logged_with_the_account_and_shown_offline() {
    let capture = Capture::default();
    let sink = capture.clone();
    let subscriber = sealwax_infrastructure::logging::subscriber(
        tracing_subscriber::EnvFilter::new("trace"),
        move || sink.clone(),
        false,
    );
    let (known, unknown) = tracing::subscriber::with_default(subscriber, || {
        (crash_report(Some(AccountId(7)), 2), crash_report(None, 1))
    });
    assert!(matches!(
        known,
        Some(Event::AccountStatus {
            account: AccountId(7),
            status: SyncStatus::Offline(LoginFailure::Other),
        })
    ));
    assert!(unknown.is_none());
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("account=7 crashes=2") && text.contains("panicked"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 2, "{text}");
}

/// A panic in one half of an actor that joins two (as a POP3 account's passes and outbox are)
/// ends the whole actor once.
#[tokio::test]
async fn a_panic_in_one_half_of_a_joined_actor_is_one_crash() {
    let config = quick();
    let (_signal, signals) = watch::channel(Signal::default());
    let record = Record::default();
    let starts = record.clone();
    let halves = Arc::new(AtomicU32::new(0));
    let seen = halves.clone();
    supervise(
        &config,
        signals,
        move || {
            let n = starts.started();
            let halves = halves.clone();
            async move {
                let passes = async {
                    halves.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                };
                let outbox = async {
                    if n == 1 {
                        panic!("injected panic in the outbox half");
                    }
                };
                tokio::join!(passes, outbox);
            }
        },
        record.crashed(),
    )
    .await;
    assert_eq!(record.crashes(), [1]);
    assert_eq!(record.starts().len(), 2);
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}
