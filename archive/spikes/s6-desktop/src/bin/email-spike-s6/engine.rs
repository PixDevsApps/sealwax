//! The IMAP side of background mode, as the engine crate will run it: a tokio runtime on its own
//! thread (tokio never reaches the GTK thread), S1's IDLE loop on INBOX against the testbed, and
//! plain messages to and from the GTK thread over async-channel. After the first pass every new
//! UID becomes a `NewMail` event.

use crate::config::Imap;
use anyhow::{Context, Result};
use email_spike_s1::idle::{self, Event};
use email_spike_s1::net::Security;
use email_spike_s1::store::Store;
use email_spike_s1::sync::Shared;
use email_spike_s1::testbed;
use email_spike_s6::clock::{InstantMap, mono_ns};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Connected {
        t: i64,
    },
    Idling {
        t: i64,
    },
    Notice {
        t: i64,
    },
    Synced {
        started: i64,
        t: i64,
    },
    Disconnected {
        t: i64,
        reason: String,
    },
    Reconnecting {
        t: i64,
    },
    NewMail {
        t: i64,
        uid: u32,
        message_id: Option<String>,
    },
    Stopped {
        t: i64,
    },
    Failed {
        t: i64,
        reason: String,
    },
}

pub enum Command {
    Reconnect,
    Stop,
}

pub struct Engine {
    pub commands: async_channel::Sender<Command>,
}

pub fn start(
    imap: &Imap,
    run_dir: PathBuf,
    events: async_channel::Sender<EngineEvent>,
) -> Result<Engine> {
    let server = testbed::server(&imap.server)
        .with_context(|| format!("unknown testbed server {}", imap.server))?;
    let mut account = testbed::account(server, &imap.user)?;
    account.endpoint.port = imap.port;
    account.endpoint.security = Security::Tls;
    std::fs::create_dir_all(&run_dir)?;
    let store_path = run_dir.join("imap-cache.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", store_path.display()));
    }
    let (commands, queue) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("s6-engine".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = events.send_blocking(EngineEvent::Failed {
                        t: mono_ns(),
                        reason: error.to_string(),
                    });
                    return;
                }
            };
            let result = runtime.block_on(run(account, store_path, events.clone(), queue));
            if let Err(error) = result {
                let _ = events.send_blocking(EngineEvent::Failed {
                    t: mono_ns(),
                    reason: format!("{error:#}"),
                });
            }
            runtime.shutdown_timeout(Duration::from_secs(1));
            let _ = events.send_blocking(EngineEvent::Stopped { t: mono_ns() });
        })?;
    Ok(Engine { commands })
}

async fn run(
    account: idle::Account,
    store_path: PathBuf,
    events: async_channel::Sender<EngineEvent>,
    commands: async_channel::Receiver<Command>,
) -> Result<()> {
    let shared = Shared::new(Store::open(&store_path)?);
    let clock = InstantMap::now();
    let mut seen: Option<BTreeSet<u32>> = None;
    loop {
        let (stop, stop_rx) = watch::channel(false);
        let (s1_events, mut s1_rx) = mpsc::unbounded_channel();
        let mut task = tokio::spawn(idle::run_inbox(
            account.clone(),
            shared.clone(),
            Duration::from_secs(300),
            Duration::from_secs(5),
            s1_events,
            stop_rx,
        ));
        loop {
            tokio::select! {
                event = s1_rx.recv() => {
                    let Some(event) = event else { break };
                    forward(event, &clock, &shared, &mut seen, &events).await;
                }
                command = commands.recv() => match command {
                    Ok(Command::Reconnect) => {
                        task.abort();
                        let _ = events.send(EngineEvent::Reconnecting { t: mono_ns() }).await;
                        break;
                    }
                    Ok(Command::Stop) | Err(_) => {
                        let _ = stop.send(true);
                        if tokio::time::timeout(Duration::from_secs(3), &mut task).await.is_err() {
                            task.abort();
                        }
                        return Ok(());
                    }
                },
            }
        }
    }
}

async fn forward(
    event: Event,
    clock: &InstantMap,
    shared: &Arc<Shared>,
    seen: &mut Option<BTreeSet<u32>>,
    events: &async_channel::Sender<EngineEvent>,
) {
    let now = mono_ns();
    let out = match event {
        Event::Connected => EngineEvent::Connected { t: now },
        Event::Idling => EngineEvent::Idling { t: now },
        Event::Notice(at) => EngineEvent::Notice { t: clock.mono(at) },
        Event::Disconnected(reason) => EngineEvent::Disconnected { t: now, reason },
        Event::Synced { started, finished } => {
            let inbox = shared
                .store()
                .snapshot()
                .ok()
                .and_then(|mut folders| folders.remove("INBOX"))
                .map(|folder| folder.messages)
                .unwrap_or_default();
            match seen {
                None => *seen = Some(inbox.keys().copied().collect()),
                Some(known) => {
                    for (uid, (_, _, message_id)) in &inbox {
                        if known.insert(*uid) {
                            let _ = events
                                .send(EngineEvent::NewMail {
                                    t: mono_ns(),
                                    uid: *uid,
                                    message_id: message_id.clone(),
                                })
                                .await;
                        }
                    }
                }
            }
            EngineEvent::Synced {
                started: clock.mono(started),
                t: clock.mono(finished),
            }
        }
    };
    let _ = events.send(out).await;
}
