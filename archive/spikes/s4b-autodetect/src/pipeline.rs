use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hickory_resolver::TokioResolver;
use rustls::ClientConfig;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout, timeout_at};

use crate::dnsdata::{MxAnswer, Service, conventional_sibling, mx_lookup_domains, srv_servers};
use crate::fetch::{self, Fetched, Ledger, Request};
use crate::model::{
    Auth, Finding, Hints, Outcome, Protocol, Report, Security, Server, Source, Username,
};
use crate::presets;
use crate::probe::{Probe, ProbeCache, Stage, Target};
use crate::rank::{Wait, best_index, decide, plaintext_only};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    pub own_host: bool,
    pub srv_conventional: bool,
    pub mx_hosts: bool,
}

impl Rules {
    pub const ALL: Rules = Rules {
        own_host: true,
        srv_conventional: true,
        mx_hosts: true,
    };
}

#[derive(Clone, Debug)]
pub struct Options {
    pub presets: bool,
    pub exhaustive: bool,
    pub rules: Rules,
    pub probe_delay: Duration,
    pub cap: Duration,
    pub http_timeout: Duration,
    pub dns_timeout: Duration,
    pub source_timeout: Duration,
    pub well_known_timeout: Duration,
    pub mx_autoconfig_timeout: Duration,
    pub own_host_timeout: Duration,
    pub probe_connect_timeout: Duration,
    pub probe_budget: Duration,
    pub dns_server: Option<IpAddr>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            presets: true,
            exhaustive: false,
            rules: Rules::ALL,
            probe_delay: Duration::from_millis(2000),
            cap: Duration::from_secs(10),
            http_timeout: Duration::from_secs(4),
            dns_timeout: Duration::from_millis(2000),
            source_timeout: Duration::from_millis(4500),
            well_known_timeout: Duration::from_millis(1500),
            mx_autoconfig_timeout: Duration::from_millis(1500),
            own_host_timeout: Duration::from_secs(3),
            probe_connect_timeout: Duration::from_secs(3),
            probe_budget: Duration::from_secs(5),
            dns_server: None,
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
    probes: ProbeCache,
    mx_hosts: watch::Sender<Option<Vec<String>>>,
    probe_started_ms: Mutex<Option<f64>>,
    start: Instant,
}

impl Context {
    async fn probe(&self, target: Target, limit: Duration) -> Probe {
        self.probes
            .probe(
                target,
                self.tls.clone(),
                limit,
                &self.resolver,
                Some(&self.ledger),
            )
            .await
    }
}

fn report(
    source: Source,
    start: Instant,
    result: Result<Finding, String>,
    detail: String,
) -> Report {
    Report {
        source,
        elapsed_ms: ms(start),
        result,
        detail,
        rewrites: Vec::new(),
    }
}

pub async fn discover(
    domain: &str,
    options: &Options,
    tls: Arc<ClientConfig>,
) -> Result<Discovery, String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let start = Instant::now();
    let deadline = tokio::time::Instant::from_std(start + options.cap);
    let sources: Vec<Source> = Source::ALL.to_vec();
    let mut reports = vec![preset_report(&domain, options.presets)];
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
        probes: ProbeCache::default(),
        mx_hosts: watch::Sender::new(None),
        probe_started_ms: Mutex::new(None),
        start,
    });
    let setup_ms = ms(start);

    let (tx, mut rx) = mpsc::unbounded_channel::<Report>();
    let (go_tx, go_rx) = watch::channel(false);
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
    spawn_source(
        &mut tasks,
        &tx,
        &context,
        Source::WellKnown,
        options.well_known_timeout,
        well_known,
    );
    spawn_source(&mut tasks, &tx, &context, Source::Srv, limit, srv);
    spawn_source(&mut tasks, &tx, &context, Source::Ispdb, limit, ispdb);
    spawn_source(
        &mut tasks,
        &tx,
        &context,
        Source::OwnHost,
        options.own_host_timeout * 2 + Duration::from_millis(500),
        own_host,
    );
    tasks.spawn(mx(context.clone(), tx.clone()));
    tasks.spawn(probe_source(context.clone(), tx.clone(), go_rx));
    drop(tx);

    let mut first_usable_ms = None;
    let mut settled_at: Option<(f64, Outcome)> = None;
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
            .filter(|s| !matches!(s, Source::Probe | Source::MxProbe))
            .all(|s| reports.iter().any(|r| r.source == *s));
        if others_done && !*go_tx.borrow() {
            let _ = go_tx.send(true);
        }
        if reports.len() == sources.len() {
            break;
        }
        match timeout_at(deadline, rx.recv()).await {
            Ok(Some(report)) => reports.push(report),
            Ok(None) => break,
            Err(_) => {
                timed_out = true;
                break;
            }
        }
    }
    tasks.abort_all();

    let (final_ms, outcome, settled) = match settled_at {
        Some((at, outcome)) => (at, outcome, true),
        None => (
            ms(start),
            decide(&sources, &reports, Wait::No).outcome,
            !timed_out && reports.len() == sources.len(),
        ),
    };
    let probe_started_ms = context.probe_started_ms.lock().ok().and_then(|v| *v);
    let mut probes = context.probes.done();
    probes.sort_by(|a, b| a.elapsed_ms.total_cmp(&b.elapsed_ms));
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

fn preset_report(domain: &str, enabled: bool) -> Report {
    let start = Instant::now();
    let (result, detail) = if !enabled {
        (Ok(Finding::Nothing), "presets disabled".to_string())
    } else if let Some(preset) = presets::by_domain(domain) {
        (Ok(preset.finding()), format!("preset {}", preset.id))
    } else {
        (Ok(Finding::Nothing), "no preset".to_string())
    };
    report(Source::Preset, start, result, detail)
}

type SourceFuture = std::pin::Pin<Box<dyn Future<Output = Report> + Send>>;

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
        let mut out = match timeout(limit, run(context.clone())).await {
            Ok(out) => out,
            Err(_) => report(
                source,
                context.start,
                Err("source timeout".into()),
                format!("no answer within {} ms", limit.as_millis()),
            ),
        };
        out.source = source;
        out.elapsed_ms = ms(context.start);
        let _ = tx.send(out);
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
        let (result, detail) =
            config_finding(fetch::fetch_config(&context.http, &url, &dummy_address(domain)).await);
        report(Source::Autoconfig, context.start, result, detail)
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
        let (result, detail) =
            config_finding(fetch::fetch_config(&context.http, &url, &dummy_address(domain)).await);
        report(Source::WellKnown, context.start, result, detail)
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
        let (result, detail) =
            config_finding(fetch::fetch_config(&context.http, &url, &dummy_address(domain)).await);
        report(Source::Ispdb, context.start, result, detail)
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
        let mut servers = srv_servers(&records);
        let mut detail = if found.is_empty() {
            "no SRV records".to_string()
        } else {
            found.join(", ")
        };
        if servers.is_empty() {
            let result = if errors.len() == Service::ALL.len() {
                Err(errors.join("; "))
            } else {
                Ok(Finding::Nothing)
            };
            return report(Source::Srv, context.start, result, detail);
        }
        let mut rewrites = Vec::new();
        if context.options.rules.srv_conventional {
            for protocol in [Protocol::Imap, Protocol::Smtp] {
                let Some(index) = best_index(&servers, protocol) else {
                    continue;
                };
                let server = &servers[index];
                let Some(sibling) = conventional_sibling(&server.host, protocol) else {
                    continue;
                };
                let (same, note) =
                    same_server(&context, protocol, &server.host, &sibling, server).await;
                detail.push_str(&format!(
                    "; {protocol:?} {} → {sibling}: {note}",
                    server.host
                ));
                if same {
                    rewrites.push((servers[index].host.clone(), sibling.clone()));
                    servers[index].host = sibling;
                }
            }
        }
        let mut out = report(
            Source::Srv,
            context.start,
            Ok(Finding::Servers {
                servers,
                hints: Hints::default(),
            }),
            detail,
        );
        out.rewrites = rewrites;
        out
    })
}

async fn same_server(
    context: &Context,
    protocol: Protocol,
    host: &str,
    sibling: &str,
    server: &Server,
) -> (bool, String) {
    context
        .ledger
        .record("dns", format!("{host} AAAA/A"), false);
    context
        .ledger
        .record("dns", format!("{sibling} AAAA/A"), false);
    let (a, b) = tokio::join!(
        fetch::addresses(&context.resolver, host),
        fetch::addresses(&context.resolver, sibling)
    );
    let count = match (a, b) {
        (Ok(a), Ok(b)) if !a.is_empty() && a == b => a.len(),
        (Ok(_), Ok(_)) => return (false, "different addresses".into()),
        (Err(e), _) | (_, Err(e)) => return (false, format!("lookup: {e}")),
    };
    let probe = context
        .probe(
            Target {
                protocol,
                host: sibling.to_string(),
                port: server.port,
                security: server.security,
            },
            context.options.probe_connect_timeout,
        )
        .await;
    match probe.result {
        Ok(_) => (true, format!("same {count} addresses, TLS ok")),
        Err((stage, _)) => (
            false,
            format!("same addresses, sibling failed at {stage:?}"),
        ),
    }
}

fn own_host(context: Arc<Context>) -> SourceFuture {
    Box::pin(async move {
        let domain = &context.domain;
        if !context.options.rules.own_host {
            return report(
                Source::OwnHost,
                context.start,
                Ok(Finding::Nothing),
                "rule off".into(),
            );
        }
        let limit = context.options.own_host_timeout;
        let imap = context
            .probe(
                Target {
                    protocol: Protocol::Imap,
                    host: format!("imap.{domain}"),
                    port: 993,
                    security: Security::Tls,
                },
                limit,
            )
            .await;
        if let Err((stage, _)) = &imap.result {
            return report(
                Source::OwnHost,
                context.start,
                Ok(Finding::Nothing),
                format!("imap.{domain}:993 failed at {stage:?}"),
            );
        }
        let mut servers = vec![
            Server::new(Protocol::Imap, &imap.target.host, 993, Security::Tls)
                .with_username(Username::Address),
        ];
        let mut detail = format!("imap.{domain}:993 ok");
        let deadline = Instant::now() + limit;
        for (port, security) in [(465, Security::Tls), (587, Security::StartTls)] {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < Duration::from_millis(300) {
                break;
            }
            let smtp = context
                .probe(
                    Target {
                        protocol: Protocol::Smtp,
                        host: format!("smtp.{domain}"),
                        port,
                        security,
                    },
                    remaining,
                )
                .await;
            if smtp.result.is_ok() {
                servers.push(
                    Server::new(Protocol::Smtp, &smtp.target.host, port, security)
                        .with_username(Username::Address),
                );
                detail.push_str(&format!(", smtp.{domain}:{port} ok"));
                break;
            }
        }
        report(
            Source::OwnHost,
            context.start,
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
        let _ = tx.send(report(source, context.start, result, detail));
    };
    let set_hosts = |hosts: Vec<String>| {
        context.mx_hosts.send_replace(Some(hosts));
    };
    let rest = |detail: &str| {
        send(Source::MxAutoconfig, Ok(Finding::Nothing), detail.into());
        send(Source::MxIspdb, Ok(Finding::Nothing), detail.into());
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
            set_hosts(Vec::new());
            for source in [Source::MxPreset, Source::MxAutoconfig, Source::MxIspdb] {
                send(source, Err(e.clone()), e.clone());
            }
            return;
        }
        Err(_) => {
            set_hosts(Vec::new());
            let e = "source timeout".to_string();
            for source in [Source::MxPreset, Source::MxAutoconfig, Source::MxIspdb] {
                send(source, Err(e.clone()), e.clone());
            }
            return;
        }
    };
    let records = match answer {
        MxAnswer::NullMx => {
            set_hosts(Vec::new());
            send(
                Source::MxPreset,
                Ok(Finding::NoMail("null MX")),
                "null MX (RFC 7505)".into(),
            );
            rest("null MX");
            return;
        }
        MxAnswer::NxDomain => {
            set_hosts(Vec::new());
            send(
                Source::MxPreset,
                Ok(Finding::NoMail("domain does not exist")),
                "NXDOMAIN".into(),
            );
            rest("NXDOMAIN");
            return;
        }
        MxAnswer::NoRecords => {
            set_hosts(Vec::new());
            send(
                Source::MxPreset,
                Ok(Finding::Nothing),
                "no MX records".into(),
            );
            rest("no MX records");
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
        .then(|| records.iter().find_map(|mx| presets::by_mx(&mx.exchange)))
        .flatten();
    let bases = mx_lookup_domains(&records, &domain, 2);
    if let Some(preset) = preset {
        set_hosts(Vec::new());
        send(
            Source::MxPreset,
            Ok(preset.finding()),
            format!("{summary} → preset {}", preset.id),
        );
        if !context.options.exhaustive {
            rest("not needed");
            return;
        }
    } else {
        set_hosts(if context.options.rules.mx_hosts {
            bases.clone()
        } else {
            Vec::new()
        });
        send(
            Source::MxPreset,
            Ok(Finding::Nothing),
            format!("{summary} → no preset"),
        );
    }
    if bases.is_empty() {
        rest(&format!("{summary} → self-hosted"));
        return;
    }
    tokio::join!(
        mx_autoconfig(&context, &bases, &summary, &send),
        mx_ispdb(&context, &bases, &summary, &send)
    );
}

async fn mx_autoconfig(
    context: &Context,
    bases: &[String],
    summary: &str,
    send: &impl Fn(Source, Result<Finding, String>, String),
) {
    if !context.options.rules.mx_hosts {
        send(
            Source::MxAutoconfig,
            Ok(Finding::Nothing),
            "rule off".into(),
        );
        return;
    }
    let deadline = Instant::now() + context.options.mx_autoconfig_timeout;
    let mut details = Vec::new();
    for base in bases {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            details.push(format!("{base}: no time left"));
            break;
        }
        context.ledger.record(
            "https",
            format!("autoconfig.{base}/mail/config-v1.1.xml"),
            false,
        );
        let url = format!("https://autoconfig.{base}/mail/config-v1.1.xml");
        let fetched = timeout(
            remaining,
            fetch::fetch_config(&context.http, &url, &dummy_address(&context.domain)),
        )
        .await
        .unwrap_or_else(|_| Err("timeout".into()));
        let (result, detail) = config_finding(fetched);
        if let Ok(Finding::Servers { servers, hints }) = result {
            send(
                Source::MxAutoconfig,
                Ok(Finding::Servers { servers, hints }),
                format!("{summary} → autoconfig.{base}: {detail}"),
            );
            return;
        }
        details.push(format!("{base}: {detail}"));
    }
    send(
        Source::MxAutoconfig,
        Ok(Finding::Nothing),
        format!("{summary} → {}", details.join("; ")),
    );
}

async fn mx_ispdb(
    context: &Context,
    bases: &[String],
    summary: &str,
    send: &impl Fn(Source, Result<Finding, String>, String),
) {
    let mut details = Vec::new();
    for base in bases {
        context.ledger.record(
            "https",
            format!("autoconfig.thunderbird.net/v1.1/{base}"),
            false,
        );
        let url = format!("https://autoconfig.thunderbird.net/v1.1/{base}");
        let fetched = timeout(
            context.options.source_timeout,
            fetch::fetch_config(&context.http, &url, &dummy_address(&context.domain)),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Own,
    MxHost,
}

#[derive(Clone, Debug)]
pub struct Chain {
    pub protocol: Protocol,
    pub scope: Scope,
    pub order: usize,
    pub targets: Vec<Target>,
}

fn chain_targets(host: &str, protocol: Protocol) -> Vec<Target> {
    let ports = match protocol {
        Protocol::Smtp => [(465, Security::Tls), (587, Security::StartTls)],
        _ => [(993, Security::Tls), (143, Security::StartTls)],
    };
    ports
        .iter()
        .map(|&(port, security)| Target {
            protocol,
            host: host.to_string(),
            port,
            security,
        })
        .collect()
}

pub fn probe_chains(domain: &str, mx_hosts: &[String]) -> Vec<Chain> {
    let mut chains = Vec::new();
    for protocol in [Protocol::Imap, Protocol::Smtp] {
        let label = if protocol == Protocol::Smtp {
            "smtp."
        } else {
            "imap."
        };
        let mut hosts: Vec<(Scope, String)> = [label, "mail.", ""]
            .iter()
            .map(|prefix| (Scope::Own, format!("{prefix}{domain}")))
            .collect();
        for base in mx_hosts {
            for prefix in [label, "mail."] {
                hosts.push((Scope::MxHost, format!("{prefix}{base}")));
            }
        }
        for (order, (scope, host)) in hosts.into_iter().enumerate() {
            chains.push(Chain {
                protocol,
                scope,
                order,
                targets: chain_targets(&host, protocol),
            });
        }
    }
    chains
}

/// A host found only at the MX operator counts when the operator offers both IMAP and
/// submission, as a mailbox host does; an IMAP server alone may be the operator's own.
pub fn probe_finding(scope: Scope, servers: Vec<Server>) -> (Finding, Option<&'static str>) {
    let protocols = |p| servers.iter().any(|s: &Server| s.protocol == p);
    if servers.is_empty() {
        (Finding::Nothing, None)
    } else if scope == Scope::MxHost && !(protocols(Protocol::Imap) && protocols(Protocol::Smtp)) {
        (
            Finding::Nothing,
            Some("not accepted: an MX host must offer both IMAP and submission"),
        )
    } else {
        (
            Finding::Servers {
                servers,
                hints: Hints::default(),
            },
            None,
        )
    }
}

pub fn decided(chains: &[Chain], results: &[(usize, Vec<Probe>)], protocol: Protocol) -> bool {
    let mut ordered: Vec<(usize, &Chain)> = chains
        .iter()
        .enumerate()
        .filter(|(_, c)| c.protocol == protocol)
        .collect();
    ordered.sort_by_key(|(_, c)| c.order);
    for (index, _) in ordered {
        match results.iter().find(|(i, _)| *i == index) {
            None => return false,
            Some((_, probes)) if probes.iter().any(|p| p.result.is_ok()) => return true,
            Some(_) => {}
        }
    }
    true
}

async fn probe_chain(context: Arc<Context>, targets: Vec<Target>, deadline: Instant) -> Vec<Probe> {
    let mut done = Vec::new();
    for target in targets {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_millis(300) {
            break;
        }
        let limit = remaining.min(context.options.probe_connect_timeout);
        let result = context.probe(target, limit).await;
        let stop = matches!(&result.result, Ok(_) | Err((Stage::Dns, _)));
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
    if let Ok(mut slot) = context.probe_started_ms.lock() {
        *slot = Some(started);
    }
    let mut known = context.mx_hosts.subscribe();
    let mx_answer = context.start + context.options.source_timeout;
    let mx_hosts = timeout(
        mx_answer.saturating_duration_since(Instant::now()),
        known.wait_for(Option::is_some),
    )
    .await
    .ok()
    .and_then(|hosts| hosts.ok().and_then(|hosts| hosts.clone()))
    .unwrap_or_default();
    let chains = probe_chains(&context.domain, &mx_hosts);
    let deadline = Instant::now() + context.options.probe_budget;
    let mut running = JoinSet::new();
    for (index, chain) in chains.iter().enumerate() {
        let context = context.clone();
        let targets = chain.targets.clone();
        running.spawn(async move { (index, probe_chain(context, targets, deadline).await) });
    }
    let mut results: Vec<(usize, Vec<Probe>)> = Vec::new();
    while let Some(joined) = running.join_next().await {
        if let Ok(result) = joined {
            results.push(result);
        }
        if !context.options.exhaustive
            && decided(&chains, &results, Protocol::Imap)
            && decided(&chains, &results, Protocol::Smtp)
        {
            break;
        }
    }
    running.abort_all();
    let attempts: usize = results.iter().map(|(_, probes)| probes.len()).sum();
    for (source, scope) in [
        (Source::Probe, Scope::Own),
        (Source::MxProbe, Scope::MxHost),
    ] {
        let mut servers = Vec::new();
        let mut notes = Vec::new();
        for protocol in [Protocol::Imap, Protocol::Smtp] {
            let mut candidates: Vec<(usize, &Vec<Probe>)> = results
                .iter()
                .filter(|(i, _)| chains[*i].protocol == protocol && chains[*i].scope == scope)
                .map(|(i, probes)| (chains[*i].order, probes))
                .collect();
            candidates.sort_by_key(|(order, _)| *order);
            let winner = candidates
                .iter()
                .find_map(|(_, probes)| probes.iter().find(|p| p.result.is_ok()));
            match winner {
                Some(found) => {
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
                }
                None => notes.push(format!("{protocol:?} none")),
            }
        }
        let (finding, refused) = probe_finding(scope, servers);
        if let Some(refused) = refused {
            notes.push(refused.into());
        }
        let scope_note = match scope {
            Scope::Own => format!("started {started:.0} ms, {attempts} connections; own domain"),
            Scope::MxHost if mx_hosts.is_empty() => "no MX hosts to probe".to_string(),
            Scope::MxHost => format!("MX hosts {}", mx_hosts.join(", ")),
        };
        let _ = tx.send(report(
            source,
            context.start,
            Ok(finding),
            format!("{scope_note}: {}", notes.join(", ")),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Greeting;

    fn probes(chain: &Chain, ok: bool) -> Vec<Probe> {
        let result = if ok {
            Ok(Greeting {
                banner: String::new(),
                capabilities: Vec::new(),
                tls_version: String::new(),
            })
        } else {
            Err((Stage::Connect, String::new()))
        };
        vec![Probe {
            target: chain.targets[0].clone(),
            elapsed_ms: 1.0,
            result,
        }]
    }

    fn index_of(chains: &[Chain], protocol: Protocol, host: &str) -> usize {
        chains
            .iter()
            .position(|c| c.protocol == protocol && c.targets[0].host == host)
            .unwrap()
    }

    #[test]
    fn own_domain_hosts_come_before_mx_hosts() {
        let chains = probe_chains("example.no", &["host.example".to_string()]);
        let imap: Vec<&str> = chains
            .iter()
            .filter(|c| c.protocol == Protocol::Imap)
            .map(|c| c.targets[0].host.as_str())
            .collect();
        assert_eq!(
            imap,
            [
                "imap.example.no",
                "mail.example.no",
                "example.no",
                "imap.host.example",
                "mail.host.example"
            ]
        );
        let smtp = &chains[index_of(&chains, Protocol::Smtp, "smtp.host.example")];
        assert_eq!(smtp.scope, Scope::MxHost);
        assert_eq!(
            smtp.targets
                .iter()
                .map(|t| (t.port, t.security))
                .collect::<Vec<_>>(),
            [(465, Security::Tls), (587, Security::StartTls)]
        );
        assert_eq!(chains.len(), 10);
        assert_eq!(probe_chains("example.no", &[]).len(), 6);
    }

    #[test]
    fn a_preferred_host_decides_without_waiting_for_the_rest() {
        let chains = probe_chains("d.example", &[]);
        let first = index_of(&chains, Protocol::Imap, "imap.d.example");
        let results = vec![(first, probes(&chains[first], true))];
        assert!(decided(&chains, &results, Protocol::Imap));
        assert!(!decided(&chains, &results, Protocol::Smtp));
    }

    #[test]
    fn an_mx_host_waits_for_every_own_domain_host() {
        let chains = probe_chains("d.example", &["host.example".to_string()]);
        let mx = index_of(&chains, Protocol::Imap, "imap.host.example");
        let mut results = vec![(mx, probes(&chains[mx], true))];
        assert!(!decided(&chains, &results, Protocol::Imap));
        for host in ["imap.d.example", "mail.d.example", "d.example"] {
            let index = index_of(&chains, Protocol::Imap, host);
            results.push((index, probes(&chains[index], false)));
        }
        assert!(decided(&chains, &results, Protocol::Imap));
    }

    #[test]
    fn all_failed_is_decided() {
        let chains = probe_chains("d.example", &[]);
        let results: Vec<_> = chains
            .iter()
            .enumerate()
            .filter(|(_, c)| c.protocol == Protocol::Smtp)
            .map(|(i, c)| (i, probes(c, false)))
            .collect();
        assert!(decided(&chains, &results, Protocol::Smtp));
    }

    #[test]
    fn an_mx_host_needs_both_imap_and_submission() {
        let imap = Server::new(Protocol::Imap, "imap.host.example", 993, Security::Tls);
        let smtp = Server::new(Protocol::Smtp, "smtp.host.example", 465, Security::Tls);
        let (finding, refused) = probe_finding(Scope::MxHost, vec![imap.clone()]);
        assert_eq!(finding, Finding::Nothing);
        assert!(refused.is_some());
        let (finding, refused) = probe_finding(Scope::MxHost, vec![imap.clone(), smtp]);
        assert!(matches!(finding, Finding::Servers { .. }));
        assert!(refused.is_none());
        let (finding, _) = probe_finding(Scope::Own, vec![imap]);
        assert!(matches!(finding, Finding::Servers { .. }));
        assert_eq!(probe_finding(Scope::MxHost, Vec::new()).0, Finding::Nothing);
    }
}
