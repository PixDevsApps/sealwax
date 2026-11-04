//! S1 spike driver against the local testbed (tools/testbed). Prints counts and timings only.
//!
//! s1 probe <server>
//! s1 first-sync <server> [--user perf] [--batch 500] [--runs 3] [--rtt-ms 0]
//! s1 fetch-only <server> [--user perf] [--batch 500] [--runs 3]
//! s1 resync <server> [--user perf] [--runs 20] [--rtt-ms 0]
//! s1 idle-latency <server> [--user idle] [--samples 20]
//! s1 random <server> [--seeds 1-10] [--ops 1000] [--inbox-backstop yes|no] [--list-status yes|no]
//!     [--sabotage ignore-vanished|ignore-vanished-no-count-check]
//! s1 faults <server>
//!
//! <server> is dovecot, dovecot-rev1 or stalwart.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use email_spike_s1::testbed::{self, Server};
use email_spike_s1::{faults, perf, probe, random};

struct Args {
    command: String,
    server: Server,
    options: HashMap<String, String>,
}

fn parse() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context("missing command")?;
    let server_name = args.next().context("missing server")?;
    let server =
        testbed::server(&server_name).with_context(|| format!("unknown server {server_name}"))?;
    let mut options = HashMap::new();
    while let Some(flag) = args.next() {
        let name = flag
            .strip_prefix("--")
            .with_context(|| format!("unexpected argument {flag}"))?;
        options.insert(
            name.to_owned(),
            args.next()
                .with_context(|| format!("--{name} needs a value"))?,
        );
    }
    Ok(Args {
        command,
        server,
        options,
    })
}

impl Args {
    fn get<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T> {
        match self.options.get(name) {
            Some(value) => value.parse().map_err(|_| anyhow::anyhow!("bad --{name}")),
            None => Ok(default),
        }
    }

    fn text(&self, name: &str, default: &str) -> String {
        self.options
            .get(name)
            .cloned()
            .unwrap_or_else(|| default.to_owned())
    }
}

fn seeds(spec: &str) -> Result<Vec<u64>> {
    if let Some((a, b)) = spec.split_once('-') {
        Ok((a.parse()?..=b.parse()?).collect())
    } else {
        spec.split(',').map(|s| Ok(s.parse()?)).collect()
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = parse()?;
    let server = args.server;
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!("s1 {} on {} ({build} build)", args.command, server.name);
    match args.command.as_str() {
        "probe" => {
            for (check, result) in probe::probe(server).await? {
                println!("{check}: {result}");
            }
        }
        "first-sync" => {
            let rtt = std::time::Duration::from_millis(args.get("rtt-ms", 0u64)?);
            println!("added round-trip time: {} ms", rtt.as_millis());
            let user = args.text("user", "perf");
            let batch = args.get("batch", 500usize)?;
            for run in 1..=args.get("runs", 3usize)? {
                let r = perf::first_sync(server, &user, batch, rtt).await?;
                println!(
                    "run {run}: batch {batch}: {} messages in {} batches; connect {:.1} ms, LIST {:.1} ms, \
                     INBOX {:.1} ms, total {:.1} ms; store {} bytes; IMAP bytes received {:?}",
                    r.messages,
                    r.batches,
                    r.connect_ms,
                    r.list_ms,
                    r.inbox_ms,
                    r.total_ms,
                    r.db_bytes,
                    r.bytes_received
                );
            }
        }
        "fetch-only" => {
            let batch = args.get("batch", 500usize)?;
            for run in 1..=args.get("runs", 3usize)? {
                let (n, elapsed) =
                    perf::fetch_only(server, &args.text("user", "perf"), batch).await?;
                println!(
                    "run {run}: header fetch of {n} messages in batches of {batch}, decoded (no store): {elapsed:.1} ms"
                );
            }
        }
        "resync" => {
            let rtt = std::time::Duration::from_millis(args.get("rtt-ms", 0u64)?);
            println!("added round-trip time: {} ms", rtt.as_millis());
            let r = perf::resync(
                server,
                &args.text("user", "perf"),
                args.get("runs", 20usize)?,
                rtt,
            )
            .await?;
            println!("{} messages in {} mailboxes", r.messages, r.mailboxes);
            println!(
                "INBOX pass, open connection: {}",
                perf::summary(&r.inbox_ms)
            );
            println!(
                "account pass (LIST + all mailboxes), open connection: {}",
                perf::summary(&r.account_ms)
            );
            println!(
                "connect + login + account pass: {}",
                perf::summary(&r.reconnect_ms)
            );
            println!(
                "fast-path passes {}, mailboxes skipped by STATUS {}, full flag scans {}",
                r.fast_path, r.status_skips, r.full_flag_scans
            );
        }
        "idle-latency" => {
            let samples = perf::idle_latency(
                server,
                &args.text("user", "idle"),
                args.get("samples", 20usize)?,
            )
            .await?;
            let visible: Vec<f64> = samples.iter().map(|s| s.visible_ms).collect();
            let notice: Vec<f64> = samples.iter().filter_map(|s| s.notice_ms).collect();
            println!("APPEND OK -> IDLE notice: {}", perf::summary(&notice));
            println!("APPEND OK -> in the cache: {}", perf::summary(&visible));
        }
        "random" => {
            let mut failed = 0;
            for seed in seeds(&args.text("seeds", "1-10"))? {
                let mut config = random::Config::new(server, seed, args.get("ops", 1000usize)?);
                config.user = args.text("user", "rand");
                config.inbox_backstop = args.text("inbox-backstop", "yes") == "yes";
                config.list_status = args.text("list-status", "yes") == "yes";
                config.sabotage = match args.text("sabotage", "none").as_str() {
                    "none" => email_spike_s1::sync::Sabotage::None,
                    "ignore-vanished" => email_spike_s1::sync::Sabotage::IgnoreVanished,
                    "ignore-vanished-no-count-check" => {
                        email_spike_s1::sync::Sabotage::IgnoreVanishedNoCountCheck
                    }
                    other => bail!("unknown --sabotage {other}"),
                };
                let r = random::run(&config).await?;
                println!(
                    "seed {seed}: {} divergences; server {} / cache {} messages; ops {:?}; UIDVALIDITY reset {:?}; \
                     mutation {:.1} s, quiescence {:.1} s; IDLE notices {}, disconnects {}",
                    r.divergences.len(),
                    r.server_messages,
                    r.local_messages,
                    r.ops,
                    r.uidvalidity_reset,
                    r.mutation.as_secs_f64(),
                    r.quiescence.as_secs_f64(),
                    r.idle_notices,
                    r.disconnects
                );
                println!("  idle task: {:?}", r.idle_stats);
                println!("  poller: {:?}", r.poller_stats);
                for d in r.divergences.iter().take(10) {
                    println!("  DIVERGENCE {d}");
                }
                if !r.divergences.is_empty() {
                    failed += 1;
                }
            }
            if failed > 0 {
                bail!("{failed} seed(s) diverged");
            }
        }
        "faults" => {
            let mut failed = 0;
            for outcome in faults::all(server).await {
                match outcome {
                    Ok(o) => {
                        println!("{}: {}", o.name, if o.passed { "PASS" } else { "FAIL" });
                        for note in &o.notes {
                            println!("  {note}");
                        }
                        if !o.passed {
                            failed += 1;
                        }
                    }
                    Err(e) => {
                        println!("scenario error: {e:#}");
                        failed += 1;
                    }
                }
            }
            if failed > 0 {
                bail!("{failed} fault scenario(s) failed");
            }
        }
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
