//! Testbed suites. They need the Docker testbed running and seeded, and must run one at a time
//! (they share test users):
//!
//! tools/testbed/up.sh && tools/testbed/seed.sh
//! CARGO_TARGET_DIR=target/s1 cargo test -p email-spike-s1 --release -- --ignored --test-threads=1

use std::time::Duration;

use email_spike_s1::faults::{self, Strategy};
use email_spike_s1::net::{self, ConnectOptions, Security};
use email_spike_s1::testbed::{self, DOVECOT, DOVECOT_REV1, STALWART, Server};
use email_spike_s1::{perf, random, sync};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn randomized(server: Server) {
    runtime().block_on(async {
        let mut failures = Vec::new();
        for seed in 1..=10 {
            let report = random::run(&random::Config::new(server, seed, 1000))
                .await
                .unwrap();
            assert!(
                report
                    .uidvalidity_reset
                    .is_some_and(|(before, after)| before != after),
                "seed {seed}: no UIDVALIDITY change"
            );
            if !report.divergences.is_empty() {
                failures.push((seed, report.divergences));
            }
        }
        assert!(
            failures.is_empty(),
            "{} diverged: {failures:?}",
            server.name
        );
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_two_client_dovecot() {
    randomized(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_two_client_dovecot_rev1() {
    randomized(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_two_client_stalwart() {
    randomized(STALWART);
}

fn fault_suite(server: Server) {
    runtime().block_on(async {
        let mut outcomes = vec![
            faults::mid_fetch_initial(server).await.unwrap(),
            faults::mid_fetch_incremental(server).await.unwrap(),
            faults::mid_append_literal(server, Strategy::Window)
                .await
                .unwrap(),
            faults::mid_append_reply(server, Strategy::Window)
                .await
                .unwrap(),
            faults::mid_append_reply(server, Strategy::Blind)
                .await
                .unwrap(),
            faults::mid_move_reply(server, Strategy::Window)
                .await
                .unwrap(),
        ];
        if server != DOVECOT_REV1 {
            outcomes.push(faults::mid_idle(server).await.unwrap());
        }
        let failed: Vec<_> = outcomes.iter().filter(|o| !o.passed).collect();
        assert!(failed.is_empty(), "{}: {failed:?}", server.name);
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot() {
    fault_suite(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot_rev1() {
    fault_suite(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_stalwart() {
    fault_suite(STALWART);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn non_ascii_mailbox_names_round_trip_on_every_server() {
    runtime().block_on(async {
        for server in testbed::SERVERS {
            for security in [Security::Tls, Security::StartTls] {
                let mut conn = net::connect(
                    &testbed::endpoint(server, security),
                    testbed::tls().unwrap(),
                    &testbed::login("alice"),
                    ConnectOptions::default(),
                )
                .await
                .unwrap();
                let names: Vec<String> =
                    sync::listed_mailboxes(&sync::list(&mut conn).await.unwrap())
                        .into_iter()
                        .map(|m| m.name)
                        .collect();
                for wanted in [
                    "Søppelpost",
                    "Papirkurv",
                    "Sendt",
                    "Økonomi & regnskap/Kvitteringer",
                    "Prosjekter/Høst 📬 2026",
                ] {
                    assert!(
                        names.iter().any(|n| n == wanted),
                        "{} ({security:?}): {wanted} not listed",
                        server.name
                    );
                }
                conn.logout().await;
            }
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn pass_bars_for_sync_speed_and_idle() {
    runtime().block_on(async {
        for server in [DOVECOT, STALWART, DOVECOT_REV1] {
            let first = perf::first_sync(server, "perf", 500, Duration::ZERO)
                .await
                .unwrap();
            assert_eq!(
                first.messages, 10_000,
                "{}: seed.sh seeds 10,000 messages",
                server.name
            );
            assert!(
                first.total_ms <= 20_000.0,
                "{}: first sync {:.0} ms",
                server.name,
                first.total_ms
            );
            let resync = perf::resync(server, "perf", 5, Duration::ZERO)
                .await
                .unwrap();
            let worst = resync.account_ms.iter().copied().fold(0.0, f64::max);
            let bar = if server == DOVECOT_REV1 {
                2_000.0
            } else {
                200.0
            };
            assert!(
                worst <= bar,
                "{}: no-change re-sync {worst:.1} ms",
                server.name
            );
        }
        for server in [DOVECOT, STALWART] {
            let samples = perf::idle_latency(server, "idle", 5).await.unwrap();
            let worst = samples.iter().map(|s| s.visible_ms).fold(0.0, f64::max);
            assert!(
                worst <= 3_000.0,
                "{}: IDLE new mail {worst:.0} ms",
                server.name
            );
        }
    });
}
