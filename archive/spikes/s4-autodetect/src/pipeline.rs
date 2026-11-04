use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hickory_resolver::TokioResolver;
use rustls::ClientConfig;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout, timeout_at};

use crate::dnsdata::{MxAnswer, Service, mx_lookup_domains, srv_servers};
use crate::fetch::{self, Fetched, Ledger, Request};
use crate::model::{
    Auth, Finding, Hints, Outcome, Protocol, Report, Security, Server, Source, Username,
};
use crate::presets;
use crate::probe::{self, Probe, Stage, Target};
use crate::rank::{Wait, decide, plaintext_only};

#[derive(Clone, Debug)]
pub struct Options {
    pub presets: bool,
    pub exhaustive: bool,
    pub probe_delay: Duration,
    pub cap: Duration,
    pub http_timeout: Duration,
    pub dns_timeout: Duration,
    pub source_timeout: Duration,
    pub probe_connect_timeout: Duration,
    pub probe_budget: Duration,
    pub dns_server: Option<IpAddr>,
    pub order: Vec<Source>,
    pub well_known_timeout: Duration,
    pub local_presets: bool,
    pub probe_wait_all: bool,
}

pub const TUNED_ORDER: [Source; 8] = [
    Source::Preset,
    Source::Autoconfig,
    Source::Srv,
    Source::Ispdb,
    Source::MxPreset,
    Source::WellKnown,
    Source::MxIspdb,
    Source::Probe,
];

impl Default for Options {
    fn default() -> Self {
        Self {
            presets: true,
            exhaustive: false,
            probe_delay: Duration::from_millis(2000),
            cap: Duration::from_secs(10),
            http_timeout: Duration::from_secs(4),
            dns_timeout: Duration::from_millis(2000),
            source_timeout: Duration::from_millis(4500),
            probe_connect_timeout: Duration::from_secs(3),
            probe_budget: Duration::from_secs(5),
            dns_server: None,
            order: Source::ALL.to_vec(),
            well_known_timeout: Duration::from_millis(4500),
            local_presets: false,
            probe_wait_all: false,
        }
    }
}

pub struct Discovery {
    pub domain: String,
    pub reports: Vec<Report>,
    pub outcome: Outcome,
    pub first_usable_ms: Option<f64>,
    pub final_ms: f64,
    pub settled: bool,
    pub setup_ms: f64,
    pub probe_started_ms: Option<f64>,
    pub probes: Vec<Probe>,
    pub requests: Vec<Request>,
}

impl Discovery {
    pub fn report(&self, source: Source) -> Option<&Report> {
        self.reports.iter().find(|r| r.source == source)
    }
}

pub fn dummy_address(domain: &str) -> String {
    format!("test@{domain}")
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

struct Context {
    domain: String,
    options: Options,
    http: reqwest::Client,
    resolver: Arc<TokioResolver>,
    tls: Arc<ClientConfig>,
    ledger: Ledger,
    start: Instant,
}

pub async fn discover(
    domain: &str,
    options: &Options,
    tls: Arc<ClientConfig>,
) -> Result<Discovery, String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let start = Instant::now();
    let deadline = tokio::time::Instant::from_std(start + options.cap);
    let sources: Vec<Source> = options.order.clone();
    let mut reports = vec![preset_report(
        &domain,
        options.presets,
        options.local_presets,
    )];
    let preset = decide(&[Source::Preset], &reports, Wait::ForHigherPriority);
    if preset.outcome.is_usable() && !options.exhaustive {
        let elapsed = ms(start);
        return Ok(Discovery {
            domain,
            reports,
            outcome: preset.outcome,
            first_usable_ms: Some(elapsed),
            final_ms: elapsed,
            settled: true,
            setup_ms: 0.0,
            probe_started_ms: None,
            probes: Vec::new(),
            requests: Vec::new(),
        });
    }

    let ledger = Ledger::default();
    let resolver = Arc::new(fetch::resolver(options.dns_timeout, options.dns_server)?);
    let context = Arc::new(Context {
        domain: domain.clone(),
        options: options.clone(),
        http: fetch::http_client(
            &domain,
            options.http_timeout,
            ledger.clone(),
            resolver.clone(),
        )?,
        resolver,
        tls,
        ledger: ledger.clone(),
        start,
    });
    let setup_ms = ms(start);

    let (tx, mut rx) = mpsc::unbounded_channel::<Report>();
    let (go_tx, go_rx) = watch::channel(false);
    let (probes_tx, mut probes_rx) = mpsc::unbounded_channel::<(f64, Vec<Probe>)>();
    let mut tasks = JoinSet::new();
    let limit = options.source_timeout;
    spawn_source(
        &mut tasks,
        &tx,
        &context,
        Source::Autoconfig,
        limit,
        autoconfig,
    );
    let well_known_limit = options.well_known_timeout;
    spawn_source(
        &mut tasks,
        &tx,
        &context,
        Source::WellKnown,
        well_known_limit,
        well_known,
    );
    spawn_source(&mut tasks, &tx, &context, Source::Srv, limit, srv);
    spawn_source(&mut tasks, &tx, &context, Source::Ispdb, limit, ispdb);
    tasks.spawn(mx(context.clone(), tx.clone()));
    tasks.spawn(probe_source(context.clone(), tx.clone(), go_rx, probes_tx));
    drop(tx);

    let mut first_usable_ms = None;
    let mut settled_at: Option<(f64, Outcome)> = None;
    let mut probe_started_ms = None;
    let mut probes = Vec::new();
    let mut timed_out = false;

    loop {
        let now = ms(start);
        if first_usable_ms.is_none() && decide(&sources, &reports, Wait::No).outcome.is_usable() {
            first_usable_ms = Some(now);
        }
        let decision = decide(&sources, &reports, Wait::ForHigherPriority);
        if decision.settled && settled_at.is_none() {
            settled_at = Some((now, decision.outcome));
            if !options.exhaustive {
                break;
            }
        }
        let others_done = sources
            .iter()
            .filter(|s| **s != Source::Probe)
            .all(|s| reports.iter().any(|r| r.source == *s));
        if others_done && !*go_tx.borrow() {
            let _ = go_tx.send(true);
        }
        if reports.len() == sources.len() {
            break;
        }
        tokio::select! {
            received = timeout_at(deadline, rx.recv()) => match received {
                Ok(Some(report)) => reports.push(report),
                Ok(None) => break,
                Err(_) => {
                    timed_out = true;
                    break;
                }
            },
            Some((started, batch)) = probes_rx.recv() => {
                probe_started_ms = Some(started);
                probes.extend(batch);
            }
        }
    }
    tasks.abort_all();
    while let Ok((started, batch)) = probes_rx.try_recv() {
        probe_started_ms = Some(started);
        probes.extend(batch);
    }

    let (final_ms, outcome, settled) = match settled_at {
        Some((at, outcome)) => (at, outcome, true),
        None => (
            ms(start),
            decide(&sources, &reports, Wait::No).outcome,
            !timed_out && reports.len() == sources.len(),
        ),
    };
    Ok(Discovery {
        domain,
        reports,
        outcome,
        first_usable_ms,
        final_ms,
        settled,
        setup_ms,
        probe_started_ms,
        probes,
        requests: ledger.entries(),
    })
}

fn preset_report(domain: &str, enabled: bool, local: bool) -> Report {
    let start = Instant::now();
    let (result, detail) = if !enabled {
        (Ok(Finding::Nothing), "presets disabled".to_string())
    } else if let Some(preset) = presets::by_domain(domain, local) {
        (Ok(preset.finding()), format!("preset {}", preset.id))
    } else {
        (Ok(Finding::Nothing), "no preset".to_string())
    };
    Report {
        source: Source::Preset,
        elapsed_ms: ms(start),
        result,
        detail,
    }
}

type SourceFuture =
    std::pin::Pin<Box<dyn Future<Output = (Result<Finding, String>, String)> + Send>>;

fn spawn_source(
    tasks: &mut JoinSet<()>,
    tx: &mpsc::UnboundedSender<Report>,
    context: &Arc<Context>,
    source: Source,
    limit: Duration,
    run: fn(Arc<Context>) -> SourceFuture,
) {
    let tx = tx.clone();
    let context = context.clone();
    tasks.spawn(async move {
        let (result, detail) = match timeout(limit, run(context.clone())).await {
            Ok(out) => out,
            Err(_) => (
                Err("source timeout".into()),
                format!("no answer within {} ms", limit.as_millis()),
            ),
        };
        let _ = tx.send(Report {
            source,
            elapsed_ms: ms(context.start),
            result,
            detail,
        });
    });
}

fn config_finding(fetched: Result<Fetched, String>) -> (Result<Finding, String>, String) {
    match fetched {
        Err(e) => (Err(e.clone()), e),
        Ok(Fetched::Missing(why)) => (Ok(Finding::Nothing), why),
        Ok(Fetched::Config(config)) => {
            let mut detail = format!(
                "{} ({} servers)",
                config.provider_id.as_deref().unwrap_or("?"),
                config.servers.len()
            );
            for protocol in [Protocol::Imap, Protocol::Smtp] {
                if plaintext_only(&config.servers, protocol) {
                    detail.push_str(&format!("; plaintext-only {protocol:?} rejected"));
                }
            }
            if !config.skipped.is_empty() {
                detail.push_str(&format!("; skipped {}", config.skipped.join(", ")));
            }
            if config.servers.is_empty() {
                return (Ok(Finding::Nothing), detail);
            }
            let oauth = config.oauth_issuer.clone().or_else(|| {
                config
                    .servers
                    .iter()
                    .any(|s| s.auth.contains(&Auth::OAuth2))
                    .then(|| "OAuth2".to_string())
            });
            let hints = Hints {
                provider: config.provider_id.clone(),
                oauth,
                app_password: false,
                enable_imap: config.enable_imap,
            };
            (
                Ok(Finding::Servers {
                    servers: config.servers,
                    hints,
                }),
                detail,
            )
        }
    }
}

fn autoconfig(context: Arc<Context>) -> SourceFuture {
    Box::pin(async move {
        let domain = &context.domain;
        let url = format!(
            "https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={}",
            dummy_address(domain)
        );
        context.ledger.record(
            "https",
            format!("autoconfig.{domain}/mail/config-v1.1.xml?emailaddress=…"),
            true,
        );
        config_finding(fetch::fetch_config(&context.http, &url, &dummy_address(domain)).await)
    })
}

fn well_known(context: Arc<Context>) -> SourceFuture {
    Box::pin(async move {
        let domain = &context.domain;
        let url = format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml");
        context.ledger.record(
            "https",
            format!("{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
            false,
        );
        config_finding(fetch::fetch_config(&context.http, &url, &dummy_address(domain)).await)
    })
}

fn ispdb(context: Arc<Context>) -> SourceFuture {
    Box::pin(async move {
        let domain = &context.domain;
        context.ledger.record(
            "https",
            format!("autoconfig.thunderbird.net/v1.1/{domain}"),
            false,
        );
        let url = format!("https://autoconfig.thunderbird.net/v1.1/{domain}");
        config_finding(fetch::fetch_config(&context.http, &url, &dummy_address(domain)).await)
    })
}

fn srv(context: Arc<Context>) -> SourceFuture {
    Box::pin(async move {
        let mut lookups = JoinSet::new();
        for service in Service::ALL {
            let context = context.clone();
            context.ledger.record(
                "dns",
                format!("{}.{} SRV", service.label(), context.domain),
                false,
            );
            lookups.spawn(async move {
                (
                    service,
                    fetch::srv(&context.resolver, service, &context.domain).await,
                )
            });
        }
        let mut records = Vec::new();
        let mut errors = Vec::new();
        let mut found = Vec::new();
        while let Some(joined) = lookups.join_next().await {
            let Ok((service, result)) = joined else {
                continue;
            };
            match result {
                Ok(list) if !list.is_empty() => {
                    found.push(format!("{} {}", service.label(), list.len()));
                    records.extend(list);
                }
                Ok(_) => {}
                Err(e) => errors.push(format!("{}: {e}", service.label())),
            }
        }
        let servers = srv_servers(&records);
        let detail = if found.is_empty() {
            "no SRV records".to_string()
        } else {
            found.join(", ")
        };
        if servers.is_empty() {
            if errors.len() == Service::ALL.len() {
                return (Err(errors.join("; ")), detail);
            }
            return (Ok(Finding::Nothing), detail);
        }
        (
            Ok(Finding::Servers {
                servers,
                hints: Hints::default(),
            }),
            detail,
        )
    })
}

async fn mx(context: Arc<Context>, tx: mpsc::UnboundedSender<Report>) {
    let send = |source: Source, result: Result<Finding, String>, detail: String| {
        let _ = tx.send(Report {
            source,
            elapsed_ms: ms(context.start),
            result,
            detail,
        });
    };
    let domain = context.domain.clone();
    context.ledger.record("dns", format!("{domain} MX"), false);
    let answer = match timeout(
        context.options.source_timeout,
        fetch::mx(&context.resolver, &domain),
    )
    .await
    {
        Ok(Ok(answer)) => answer,
        Ok(Err(e)) => {
            send(Source::MxPreset, Err(e.clone()), e.clone());
            send(Source::MxIspdb, Err(e.clone()), e);
            return;
        }
        Err(_) => {
            let e = "source timeout".to_string();
            send(Source::MxPreset, Err(e.clone()), e.clone());
            send(Source::MxIspdb, Err(e.clone()), e);
            return;
        }
    };
    let records = match answer {
        MxAnswer::NullMx => {
            send(
                Source::MxPreset,
                Ok(Finding::NoMail("null MX")),
                "null MX (RFC 7505)".into(),
            );
            send(Source::MxIspdb, Ok(Finding::Nothing), "null MX".into());
            return;
        }
        MxAnswer::NxDomain => {
            send(
                Source::MxPreset,
                Ok(Finding::NoMail("domain does not exist")),
                "NXDOMAIN".into(),
            );
            send(Source::MxIspdb, Ok(Finding::Nothing), "NXDOMAIN".into());
            return;
        }
        MxAnswer::NoRecords => {
            send(
                Source::MxPreset,
                Ok(Finding::Nothing),
                "no MX records".into(),
            );
            send(
                Source::MxIspdb,
                Ok(Finding::Nothing),
                "no MX records".into(),
            );
            return;
        }
        MxAnswer::Records(records) => records,
    };
    let first = &records[0].exchange;
    let summary = format!(
        "mx {first}{}",
        if records.len() > 1 {
            format!(" (+{})", records.len() - 1)
        } else {
            String::new()
        }
    );
    let preset = context
        .options
        .presets
        .then(|| {
            records
                .iter()
                .find_map(|mx| presets::by_mx(&mx.exchange, context.options.local_presets))
        })
        .flatten();
    if let Some(preset) = preset {
        send(
            Source::MxPreset,
            Ok(preset.finding()),
            format!("{summary} → preset {}", preset.id),
        );
        if !context.options.exhaustive {
            send(Source::MxIspdb, Ok(Finding::Nothing), "not needed".into());
            return;
        }
    } else {
        send(
            Source::MxPreset,
            Ok(Finding::Nothing),
            format!("{summary} → no preset"),
        );
    }

    let lookup = mx_lookup_domains(&records, &domain, 2);
    if lookup.is_empty() {
        send(
            Source::MxIspdb,
            Ok(Finding::Nothing),
            format!("{summary} → self-hosted"),
        );
        return;
    }
    let mut details = Vec::new();
    for base in lookup {
        context.ledger.record(
            "https",
            format!("autoconfig.thunderbird.net/v1.1/{base}"),
            false,
        );
        let url = format!("https://autoconfig.thunderbird.net/v1.1/{base}");
        let fetched = timeout(
            context.options.source_timeout,
            fetch::fetch_config(&context.http, &url, &dummy_address(&domain)),
        )
        .await
        .unwrap_or_else(|_| Err("source timeout".into()));
        let (result, detail) = config_finding(fetched);
        if let Ok(Finding::Servers { servers, hints }) = result {
            send(
                Source::MxIspdb,
                Ok(Finding::Servers { servers, hints }),
                format!("{summary} → ispdb {base}: {detail}"),
            );
            return;
        }
        details.push(format!("{base}: {detail}"));
    }
    send(
        Source::MxIspdb,
        Ok(Finding::Nothing),
        format!("{summary} → {}", details.join("; ")),
    );
}

const PROBE_HOSTS: usize = 3;

fn decided(results: &[(Protocol, usize, Vec<Probe>)], protocol: Protocol) -> bool {
    for order in 0..PROBE_HOSTS {
        match results
            .iter()
            .find(|(p, o, _)| *p == protocol && *o == order)
        {
            None => return false,
            Some((_, _, chain)) if chain.iter().any(|probe| probe.result.is_ok()) => return true,
            Some(_) => {}
        }
    }
    true
}

fn probe_targets(domain: &str, protocol: Protocol) -> Vec<Vec<Target>> {
    let (prefixes, ports) = match protocol {
        Protocol::Smtp => (
            ["smtp.", "mail.", ""],
            [(465, Security::Tls), (587, Security::StartTls)],
        ),
        _ => (
            ["imap.", "mail.", ""],
            [(993, Security::Tls), (143, Security::StartTls)],
        ),
    };
    prefixes
        .iter()
        .map(|prefix| {
            ports
                .iter()
                .map(|&(port, security)| Target {
                    protocol,
                    host: format!("{prefix}{domain}"),
                    port,
                    security,
                })
                .collect()
        })
        .collect()
}

async fn probe_chain(context: Arc<Context>, chain: Vec<Target>, deadline: Instant) -> Vec<Probe> {
    let mut done = Vec::new();
    for target in chain {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_millis(300) {
            break;
        }
        context
            .ledger
            .record("dns", format!("{} AAAA/A", target.host), false);
        let limit = remaining.min(context.options.probe_connect_timeout);
        let result = probe::probe(
            target,
            context.tls.clone(),
            limit,
            &context.resolver,
            Some(&context.ledger),
        )
        .await;
        let stop = match &result.result {
            Ok(_) => true,
            Err((Stage::Dns, _)) => true,
            Err(_) => false,
        };
        done.push(result);
        if stop {
            break;
        }
    }
    done
}

async fn probe_source(
    context: Arc<Context>,
    tx: mpsc::UnboundedSender<Report>,
    mut go: watch::Receiver<bool>,
    probes_tx: mpsc::UnboundedSender<(f64, Vec<Probe>)>,
) {
    let delay = if context.options.exhaustive {
        Duration::ZERO
    } else {
        context.options.probe_delay
    };
    tokio::select! {
        _ = sleep(delay) => {}
        _ = go.wait_for(|go| *go) => {}
    }
    let started = ms(context.start);
    let _ = probes_tx.send((started, Vec::new()));
    let deadline = Instant::now() + context.options.probe_budget;
    let mut chains = JoinSet::new();
    for protocol in [Protocol::Imap, Protocol::Smtp] {
        for (order, chain) in probe_targets(&context.domain, protocol)
            .into_iter()
            .enumerate()
        {
            let context = context.clone();
            chains.spawn(
                async move { (protocol, order, probe_chain(context, chain, deadline).await) },
            );
        }
    }
    let mut results: Vec<(Protocol, usize, Vec<Probe>)> = Vec::new();
    while let Some(joined) = chains.join_next().await {
        if let Ok(result) = joined {
            results.push(result);
        }
        if !context.options.probe_wait_all
            && decided(&results, Protocol::Imap)
            && decided(&results, Protocol::Smtp)
        {
            break;
        }
    }
    chains.abort_all();
    results.sort_by_key(|(protocol, order, _)| (*protocol == Protocol::Smtp, *order));
    let mut servers = Vec::new();
    let mut notes = Vec::new();
    for protocol in [Protocol::Imap, Protocol::Smtp] {
        let winner = results
            .iter()
            .filter(|(p, _, _)| *p == protocol)
            .find_map(|(_, _, chain)| chain.iter().find(|p| p.result.is_ok()));
        if let Some(found) = winner {
            servers.push(
                Server::new(
                    protocol,
                    &found.target.host,
                    found.target.port,
                    found.target.security,
                )
                .with_username(Username::Address),
            );
            notes.push(format!(
                "{protocol:?} {}:{}",
                found.target.host, found.target.port
            ));
        } else {
            notes.push(format!("{protocol:?} none"));
        }
    }
    let attempts: usize = results.iter().map(|(_, _, chain)| chain.len()).sum();
    let all: Vec<Probe> = results
        .into_iter()
        .flat_map(|(_, _, chain)| chain)
        .collect();
    let _ = probes_tx.send((started, all));
    let finding = if servers.is_empty() {
        Finding::Nothing
    } else {
        Finding::Servers {
            servers,
            hints: Hints::default(),
        }
    };
    let _ = tx.send(Report {
        source: Source::Probe,
        elapsed_ms: ms(context.start),
        result: Ok(finding),
        detail: format!(
            "started {started:.0} ms, {attempts} connections; {}",
            notes.join(", ")
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Greeting;

    fn chain(protocol: Protocol, order: usize, ok: bool) -> (Protocol, usize, Vec<Probe>) {
        let target = Target {
            protocol,
            host: format!("h{order}"),
            port: 993,
            security: Security::Tls,
        };
        let result = if ok {
            Ok(Greeting {
                banner: String::new(),
                capabilities: Vec::new(),
                tls_version: String::new(),
            })
        } else {
            Err((Stage::Connect, String::new()))
        };
        (
            protocol,
            order,
            vec![Probe {
                target,
                elapsed_ms: 1.0,
                result,
            }],
        )
    }

    #[test]
    fn a_preferred_host_decides_without_waiting_for_the_rest() {
        let results = vec![chain(Protocol::Imap, 0, true)];
        assert!(decided(&results, Protocol::Imap));
        assert!(!decided(&results, Protocol::Smtp));
    }

    #[test]
    fn a_less_preferred_success_waits_for_the_preferred_hosts() {
        let mut results = vec![chain(Protocol::Imap, 1, true)];
        assert!(!decided(&results, Protocol::Imap));
        results.push(chain(Protocol::Imap, 0, false));
        assert!(decided(&results, Protocol::Imap));
    }

    #[test]
    fn all_failed_is_decided() {
        let results: Vec<_> = (0..PROBE_HOSTS)
            .map(|o| chain(Protocol::Smtp, o, false))
            .collect();
        assert!(decided(&results, Protocol::Smtp));
    }

    #[test]
    fn probe_hosts_follow_the_documented_order() {
        let imap = probe_targets("example.com", Protocol::Imap);
        assert_eq!(imap.len(), PROBE_HOSTS);
        assert_eq!(imap[0][0].host, "imap.example.com");
        assert_eq!((imap[0][0].port, imap[0][1].port), (993, 143));
        let smtp = probe_targets("example.com", Protocol::Smtp);
        assert_eq!(smtp[2][0].host, "example.com");
        assert_eq!((smtp[2][0].port, smtp[2][1].port), (465, 587));
    }
}
