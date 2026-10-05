//! One detection: every source runs at once inside a single future, each with its own time
//! limit, and the settle rule (`rank::decide`) ends the search as soon as the answer can't
//! change. Dropping the future cancels every lookup.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt, join, join_all};
use futures_util::stream::{FuturesUnordered, StreamExt};
use reqwest::Url;
use sealwax_domain::account::{DetectSource, Detection, Protocol};
use sealwax_domain::autoconfig::dns::{self, MxAnswer, SrvService};
use sealwax_domain::autoconfig::probing::{self, Chain, ChainResult, Scope, Target};
use sealwax_domain::autoconfig::rank::{self, Outcome, Wait};
use sealwax_domain::autoconfig::{
    Finding, Report, Server, Source, address_domain, clientconfig, detection, presets,
};
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep_until, timeout};

use super::DetectOptions;
use super::network::{Fetched, Network};
use super::probe::{ProbeError, Stage};

/// What one source said, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTrace {
    pub source: Source,
    /// Since the detection started.
    pub at: Duration,
    pub finding: Finding,
    /// What happened, for diagnostics. It names hosts and may name the domain, never the
    /// address; don't log it.
    pub detail: String,
}

/// How a detection went, for the measurement harness and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    /// The address's domain in ASCII (IDNA), when the address had one.
    pub domain: Option<String>,
    pub sources: Vec<SourceTrace>,
    pub outcome: Outcome,
    /// When the sources so far first gave a usable answer.
    pub first_usable: Option<Duration>,
    /// When the answer settled, or the cap struck.
    pub elapsed: Duration,
    pub settled: bool,
    pub probe_started: Option<Duration>,
}

/// The address's domain in ASCII, or `None` for anything that isn't a mail domain.
pub fn ascii_domain(address: &str) -> Option<String> {
    let domain = address_domain(address)?;
    if domain
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || ":/?#@[]\\%".contains(c))
    {
        return None;
    }
    let url = Url::parse(&format!("https://{domain}/")).ok()?;
    let host = url.host_str()?;
    dns::acceptable_host(host).then(|| host.to_string())
}

/// The answer when no lookup is needed: a preset for the domain, or no usable domain at all.
pub fn preset(address: &str) -> Option<(Detection, Trace)> {
    match ascii_domain(address) {
        None => Some(not_a_domain()),
        Some(domain) => preset_answer(domain),
    }
}

fn not_a_domain() -> (Detection, Trace) {
    let outcome = Outcome::Settings {
        imap: None,
        smtp: None,
    };
    (
        Detection::NotFound,
        Trace {
            domain: None,
            sources: Vec::new(),
            outcome,
            first_usable: None,
            elapsed: Duration::ZERO,
            settled: true,
            probe_started: None,
        },
    )
}

fn preset_answer(domain: String) -> Option<(Detection, Trace)> {
    let preset = presets::by_domain(&domain)?;
    let finding = preset.finding();
    let report = Report {
        source: Source::Preset,
        finding: finding.clone(),
    };
    let outcome = rank::decide(&[Source::Preset], &[report], Wait::No).outcome;
    Some((
        detection(&outcome, &domain),
        Trace {
            sources: vec![SourceTrace {
                source: Source::Preset,
                at: Duration::ZERO,
                finding,
                detail: format!("preset {}", preset.id),
            }],
            domain: Some(domain),
            outcome,
            first_usable: Some(Duration::ZERO),
            elapsed: Duration::ZERO,
            settled: true,
            probe_started: None,
        },
    ))
}

pub async fn run<N: Network>(
    net: &N,
    address: &str,
    options: &DetectOptions,
    progress: impl Fn(DetectSource),
) -> (Detection, Trace) {
    let start = Instant::now();
    progress(DetectSource::Preset);
    let Some(domain) = ascii_domain(address) else {
        return not_a_domain();
    };
    if let Some(answer) = preset_answer(domain.clone()) {
        return answer;
    }
    for source in [
        DetectSource::Autoconfig,
        DetectSource::Ispdb,
        DetectSource::Mx,
        DetectSource::Srv,
        DetectSource::Probe,
    ] {
        progress(source);
    }
    let search = Search {
        net,
        address,
        domain,
        options,
        start,
        probes: ProbeCache::default(),
    };
    let trace = search.run().await;
    let domain = trace.domain.as_deref().unwrap_or_default();
    (detection(&trace.outcome, domain), trace)
}

/// What a source future hands back to the loop.
enum Step {
    Done(SourceTrace),
    /// The MX answer; `operators` are the MX operators' domains for R3 and the operator's ISPDB
    /// entry, or `None` when the MX answer already settles their sources.
    Mx {
        trace: SourceTrace,
        operators: Option<Vec<String>>,
    },
    Probes(Vec<SourceTrace>),
}

struct Search<'a, N> {
    net: &'a N,
    address: &'a str,
    domain: String,
    options: &'a DetectOptions,
    start: Instant,
    probes: ProbeCache,
}

/// One probe per endpoint per detection: later askers share the first answer.
#[derive(Default)]
struct ProbeCache(Mutex<HashMap<Target, ProbeCell>>);

type ProbeCell = Arc<OnceCell<Result<(), ProbeError>>>;

impl<N: Network> Search<'_, N> {
    async fn run(&self) -> Trace {
        let options = self.options;
        let order = options.rules.order();
        let cap = self.start + options.cap;
        let probe_at = self.start + options.probe_delay;
        let mut traces = vec![self.trace(Source::Preset, Finding::Nothing, "no preset".into())];
        let mut operators: Option<Vec<String>> = None;
        let mut probe_started: Option<Duration> = None;
        let mut first_usable = None;

        let mut pending: FuturesUnordered<BoxFuture<'_, Step>> = FuturesUnordered::new();
        pending.push(self.source(
            Source::Autoconfig,
            options.source_timeout,
            self.autoconfig(),
        ));
        pending.push(self.source(Source::Ispdb, options.source_timeout, self.ispdb()));
        pending.push(self.source(Source::Srv, options.source_timeout, self.srv()));
        pending.push(self.source(
            Source::OwnHost,
            options.connect_timeout * 2 + Duration::from_millis(500),
            self.own_host(),
        ));
        pending.push(self.source(
            Source::WellKnown,
            options.well_known_timeout,
            self.well_known(),
        ));
        pending.push(self.mx().boxed());

        let (outcome, settled) = loop {
            let reports: Vec<Report> = traces
                .iter()
                .map(|t| Report {
                    source: t.source,
                    finding: t.finding.clone(),
                })
                .collect();
            if first_usable.is_none()
                && rank::decide(&order, &reports, Wait::No).outcome.is_usable()
            {
                first_usable = Some(self.start.elapsed());
            }
            let decision = rank::decide(&order, &reports, Wait::ForHigherRanked);
            if decision.settled || traces.len() == Source::ALL.len() {
                break (decision.outcome, true);
            }
            let now = Instant::now();
            if now >= cap {
                break (rank::decide(&order, &reports, Wait::No).outcome, false);
            }
            let others_done = Source::ALL
                .iter()
                .filter(|s| !matches!(s, Source::Preset | Source::Probe | Source::MxProbe))
                .all(|s| traces.iter().any(|t| t.source == *s));
            if probe_started.is_none()
                && let Some(known) = &operators
                && (others_done || now >= probe_at)
            {
                probe_started = Some(self.start.elapsed());
                pending.push(self.probe(known.clone()).boxed());
            }
            let wake = if probe_started.is_none() && operators.is_some() {
                probe_at.min(cap)
            } else {
                cap
            };
            let step = tokio::select! {
                step = pending.next(), if !pending.is_empty() => step,
                () = sleep_until(wake) => None,
            };
            match step {
                Some(Step::Done(trace)) => traces.push(trace),
                Some(Step::Mx {
                    trace,
                    operators: Some(found),
                }) => {
                    traces.push(trace);
                    pending.push(self.source(
                        Source::MxAutoconfig,
                        options.mx_autoconfig_timeout,
                        self.mx_autoconfig(found.clone()),
                    ));
                    pending.push(self.source(
                        Source::MxIspdb,
                        options.source_timeout,
                        self.mx_ispdb(found.clone()),
                    ));
                    operators = Some(if options.rules.mx_hosts {
                        found
                    } else {
                        Vec::new()
                    });
                }
                Some(Step::Mx {
                    trace,
                    operators: None,
                }) => {
                    traces.push(trace);
                    for source in [Source::MxAutoconfig, Source::MxIspdb] {
                        traces.push(self.trace(source, Finding::Nothing, "not needed".into()));
                    }
                    operators = Some(Vec::new());
                }
                Some(Step::Probes(found)) => traces.extend(found),
                None => {}
            }
        };

        let elapsed = self.start.elapsed();
        if let Some(winner) = outcome.winners().first() {
            tracing::debug!(
                source = winner.name(),
                elapsed_ms = elapsed.as_millis() as u64,
                settled,
                "autodetect answered"
            );
        }
        Trace {
            domain: Some(self.domain.clone()),
            sources: traces,
            outcome,
            first_usable,
            elapsed,
            settled,
            probe_started,
        }
    }

    fn trace(&self, source: Source, finding: Finding, detail: String) -> SourceTrace {
        SourceTrace {
            source,
            at: self.start.elapsed(),
            finding,
            detail,
        }
    }

    /// A source with its time limit; a source that runs out of time found nothing.
    fn source<'s>(
        &'s self,
        source: Source,
        limit: Duration,
        work: impl Future<Output = (Finding, String)> + Send + 's,
    ) -> BoxFuture<'s, Step> {
        async move {
            let (finding, detail) = timeout(limit, work).await.unwrap_or_else(|_| {
                (
                    Finding::Nothing,
                    format!("no answer within {} ms", limit.as_millis()),
                )
            });
            Step::Done(self.trace(source, finding, detail))
        }
        .boxed()
    }

    async fn config(&self, url: &str) -> (Finding, String) {
        match self.net.fetch(url).await {
            Err(e) => (Finding::Nothing, e.0),
            Ok(Fetched::Status(code)) => (Finding::Nothing, format!("HTTP {code}")),
            Ok(Fetched::TooLarge) => (Finding::Nothing, "body over 256 KiB".into()),
            Ok(Fetched::Body(text)) => match clientconfig::parse(&text, self.address) {
                Err(e) => (Finding::Nothing, format!("not a clientConfig ({e})")),
                Ok(config) => {
                    let mut detail = format!(
                        "{} ({} servers)",
                        config.provider_id.as_deref().unwrap_or("?"),
                        config.servers.len()
                    );
                    if !config.skipped.is_empty() {
                        detail.push_str(&format!("; skipped {}", config.skipped.join(", ")));
                    }
                    (config.finding(), detail)
                }
            },
        }
    }

    /// The domain's own autoconfig: the one request that carries the address.
    async fn autoconfig(&self) -> (Finding, String) {
        let base = format!("https://autoconfig.{}/mail/config-v1.1.xml", self.domain);
        match Url::parse_with_params(&base, [("emailaddress", self.address)]) {
            Ok(url) => self.config(url.as_str()).await,
            Err(e) => (Finding::Nothing, e.to_string()),
        }
    }

    async fn ispdb(&self) -> (Finding, String) {
        self.config(&format!(
            "https://autoconfig.thunderbird.net/v1.1/{}",
            self.domain
        ))
        .await
    }

    async fn well_known(&self) -> (Finding, String) {
        self.config(&format!(
            "https://{}/.well-known/autoconfig/mail/config-v1.1.xml",
            self.domain
        ))
        .await
    }

    async fn probe_once(&self, target: Target, limit: Duration) -> Result<(), ProbeError> {
        let cell = match self.probes.0.lock() {
            Ok(mut cells) => cells.entry(target.clone()).or_default().clone(),
            Err(_) => Arc::new(OnceCell::new()),
        };
        cell.get_or_init(|| self.net.probe(&target, limit))
            .await
            .clone()
    }

    /// SRV records; with `verify_srv`, the best record per protocol must pass the TLS check, and
    /// R2 swaps a target for its conventional sibling when that is the same server.
    async fn srv(&self) -> (Finding, String) {
        let rules = self.options.rules;
        let lookups = join_all(
            SrvService::ALL
                .iter()
                .map(|&service| self.net.srv(service, &self.domain)),
        )
        .await;
        let mut records = Vec::new();
        let mut errors = 0;
        for lookup in lookups {
            match lookup {
                Ok(found) => records.extend(found),
                Err(_) => errors += 1,
            }
        }
        let servers = dns::srv_servers(&records, rules.verify_srv);
        if servers.is_empty() {
            let detail = if errors == SrvService::ALL.len() {
                "every SRV lookup failed"
            } else {
                "no SRV records"
            };
            return (Finding::Nothing, detail.into());
        }
        let ((imap, imap_notes), (smtp, smtp_notes)) = join(
            self.srv_choice(&servers, Protocol::Imap),
            self.srv_choice(&servers, Protocol::Smtp),
        )
        .await;
        let detail = format!(
            "{} records; {}",
            records.len(),
            [imap_notes, smtp_notes].concat().join("; ")
        );
        let chosen: Vec<Server> = imap.into_iter().chain(smtp).collect();
        if chosen.is_empty() {
            return (Finding::Nothing, detail);
        }
        (
            Finding::Servers {
                servers: chosen,
                hints: Default::default(),
            },
            detail,
        )
    }

    /// The SRV server offered for `protocol`: the best record, renamed by R2, or with
    /// `verify_srv` the first of the best two that passes the TLS check.
    async fn srv_choice(
        &self,
        servers: &[Server],
        protocol: Protocol,
    ) -> (Option<Server>, Vec<String>) {
        let rules = self.options.rules;
        let mut candidates: Vec<&Server> =
            servers.iter().filter(|s| s.protocol == protocol).collect();
        candidates.sort_by_key(|s| s.security != sealwax_domain::account::Security::Tls);
        let tries = if rules.verify_srv { 2 } else { 1 };
        let mut notes = Vec::new();
        for server in candidates.into_iter().take(tries) {
            if rules.srv_conventional
                && let Some(sibling) = dns::conventional_sibling(&server.host, protocol)
                && self.same_server(server, &sibling).await
            {
                notes.push(format!("{} → {sibling}", server.host));
                let mut renamed = server.clone();
                renamed.host = sibling;
                return (Some(renamed), notes);
            }
            if !rules.verify_srv {
                return (Some(server.clone()), notes);
            }
            match self
                .probe_once(Target::of(server), self.options.connect_timeout)
                .await
            {
                Ok(()) => return (Some(server.clone()), notes),
                Err(e) => notes.push(format!(
                    "{}:{} failed at {:?}",
                    server.host, server.port, e.stage
                )),
            }
        }
        (None, notes)
    }

    /// R2: the sibling has the same addresses and passes the same TLS check.
    async fn same_server(&self, server: &Server, sibling: &str) -> bool {
        let (a, b) = join(
            self.net.addresses(&server.host),
            self.net.addresses(sibling),
        )
        .await;
        match (a, b) {
            (Ok(a), Ok(b)) if a == b => {
                let mut target = Target::of(server);
                target.host = sibling.to_string();
                self.probe_once(target, self.options.connect_timeout)
                    .await
                    .is_ok()
            }
            _ => false,
        }
    }

    /// R1: `imap.<domain>` on 993 with a certificate for that name, and `smtp.<domain>` on 465 or
    /// 587 when it answers.
    async fn own_host(&self) -> (Finding, String) {
        if !self.options.rules.own_host {
            return (Finding::Nothing, "rule off".into());
        }
        let limit = self.options.connect_timeout;
        let imap = Target::new(
            Protocol::Imap,
            &format!("imap.{}", self.domain),
            993,
            sealwax_domain::account::Security::Tls,
        );
        if let Err(e) = self.probe_once(imap.clone(), limit).await {
            return (
                Finding::Nothing,
                format!("{}:993 failed at {:?}", imap.host, e.stage),
            );
        }
        let mut servers = vec![imap.server()];
        let mut detail = format!("{}:993 ok", imap.host);
        let deadline = Instant::now() + limit;
        for (port, security) in [
            (465, sealwax_domain::account::Security::Tls),
            (587, sealwax_domain::account::Security::StartTls),
        ] {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < Duration::from_millis(300) {
                break;
            }
            let smtp = Target::new(
                Protocol::Smtp,
                &format!("smtp.{}", self.domain),
                port,
                security,
            );
            if self.probe_once(smtp.clone(), remaining).await.is_ok() {
                detail.push_str(&format!(", {}:{port} ok", smtp.host));
                servers.push(smtp.server());
                break;
            }
        }
        (
            Finding::Servers {
                servers,
                hints: Default::default(),
            },
            detail,
        )
    }

    /// MX records → a hosted platform's preset, "no mail", or the MX operators for R3. With
    /// `autodiscover`, a domain whose MX matches no preset but whose `autodiscover` name is
    /// Microsoft's is Microsoft 365.
    async fn mx(&self) -> Step {
        let rules = self.options.rules;
        let autodiscover_name = format!("autodiscover.{}", self.domain);
        let lookups = async {
            if rules.autodiscover {
                let (mx, cname) = join(
                    self.net.mx(&self.domain),
                    self.net.cname(&autodiscover_name),
                )
                .await;
                (mx, cname.ok().flatten())
            } else {
                (self.net.mx(&self.domain).await, None)
            }
        };
        let (answer, autodiscover) = match timeout(self.options.source_timeout, lookups).await {
            Ok((Ok(answer), cname)) => (answer, cname),
            Ok((Err(e), _)) => {
                return Step::Mx {
                    trace: self.trace(Source::MxPreset, Finding::Nothing, e.0),
                    operators: Some(Vec::new()),
                };
            }
            Err(_) => {
                return Step::Mx {
                    trace: self.trace(Source::MxPreset, Finding::Nothing, "timeout".into()),
                    operators: Some(Vec::new()),
                };
            }
        };
        let records = match answer {
            MxAnswer::NullMx => {
                return Step::Mx {
                    trace: self.trace(Source::MxPreset, Finding::NoMail, "null MX".into()),
                    operators: None,
                };
            }
            MxAnswer::NxDomain => {
                return Step::Mx {
                    trace: self.trace(Source::MxPreset, Finding::NoMail, "NXDOMAIN".into()),
                    operators: None,
                };
            }
            MxAnswer::NoRecords => {
                return Step::Mx {
                    trace: self.trace(Source::MxPreset, Finding::Nothing, "no MX records".into()),
                    operators: None,
                };
            }
            MxAnswer::Records(records) => records,
        };
        let summary = format!(
            "mx {}{}",
            records[0].exchange,
            if records.len() > 1 {
                format!(" (+{})", records.len() - 1)
            } else {
                String::new()
            }
        );
        if let Some(preset) = records.iter().find_map(|mx| presets::by_mx(&mx.exchange)) {
            return Step::Mx {
                trace: self.trace(
                    Source::MxPreset,
                    preset.finding(),
                    format!("{summary} → preset {}", preset.id),
                ),
                operators: None,
            };
        }
        if let Some(target) = autodiscover.filter(|t| dns::autodiscover_is_microsoft(t))
            && let Some(preset) = presets::PRESETS.iter().find(|p| p.id == "microsoft365")
        {
            return Step::Mx {
                trace: self.trace(
                    Source::MxPreset,
                    preset.finding(),
                    format!("{summary}; {autodiscover_name} → {target}"),
                ),
                operators: None,
            };
        }
        let operators = dns::mx_operators(&records, &self.domain, 2);
        Step::Mx {
            trace: self.trace(
                Source::MxPreset,
                Finding::Nothing,
                format!("{summary} → no preset; operators {}", operators.join(", ")),
            ),
            operators: Some(operators),
        }
    }

    /// R3: the MX operators' own autoconfig, asked without an address.
    async fn mx_autoconfig(&self, operators: Vec<String>) -> (Finding, String) {
        if !self.options.rules.mx_hosts {
            return (Finding::Nothing, "rule off".into());
        }
        let mut details = Vec::new();
        for operator in &operators {
            let (finding, detail) = self
                .config(&format!(
                    "https://autoconfig.{operator}/mail/config-v1.1.xml"
                ))
                .await;
            if matches!(finding, Finding::Servers { .. }) {
                return (finding, format!("{operator}: {detail}"));
            }
            details.push(format!("{operator}: {detail}"));
        }
        (Finding::Nothing, details.join("; "))
    }

    /// The ISPDB's entries for the MX operators' domains.
    async fn mx_ispdb(&self, operators: Vec<String>) -> (Finding, String) {
        let mut details = Vec::new();
        for operator in &operators {
            let (finding, detail) = self
                .config(&format!(
                    "https://autoconfig.thunderbird.net/v1.1/{operator}"
                ))
                .await;
            if matches!(finding, Finding::Servers { .. }) {
                return (finding, format!("{operator}: {detail}"));
            }
            details.push(format!("{operator}: {detail}"));
        }
        if details.is_empty() {
            details.push("no MX operators".into());
        }
        (Finding::Nothing, details.join("; "))
    }

    /// Probes the user's own names, then the MX operators' (R3), stopping once each protocol's
    /// best answer is known.
    async fn probe(&self, operators: Vec<String>) -> Step {
        let chains = probing::chains(&self.domain, &operators);
        let deadline = Instant::now() + self.options.probe_budget;
        let mut running: FuturesUnordered<_> = chains
            .iter()
            .enumerate()
            .map(|(index, chain)| async move { (index, self.probe_chain(chain, deadline).await) })
            .collect();
        let mut results: Vec<(usize, ChainResult)> = Vec::new();
        while let Some(result) = running.next().await {
            results.push(result);
            if probing::decided(&chains, &results, Protocol::Imap)
                && probing::decided(&chains, &results, Protocol::Smtp)
            {
                break;
            }
        }
        let tried = results.len();
        let note = |scope| -> String {
            let found: Vec<String> = results
                .iter()
                .filter(|(i, _)| chains[*i].scope == scope)
                .filter_map(|(_, r)| r.as_ref().map(|t| format!("{}:{}", t.host, t.port)))
                .collect();
            format!("{tried} chains done; answered: {}", found.join(", "))
        };
        Step::Probes(vec![
            self.trace(
                Source::Probe,
                probing::finding(&chains, &results, Scope::Own),
                note(Scope::Own),
            ),
            self.trace(
                Source::MxProbe,
                probing::finding(&chains, &results, Scope::MxOperator),
                if operators.is_empty() {
                    "no MX operators".into()
                } else {
                    note(Scope::MxOperator)
                },
            ),
        ])
    }

    async fn probe_chain(&self, chain: &Chain, deadline: Instant) -> ChainResult {
        for target in &chain.targets {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < Duration::from_millis(300) {
                break;
            }
            let limit = remaining.min(self.options.connect_timeout);
            match self.probe_once(target.clone(), limit).await {
                Ok(()) => return Some(target.clone()),
                Err(e) if e.stage == Stage::Dns => break,
                Err(_) => {}
            }
        }
        None
    }
}
