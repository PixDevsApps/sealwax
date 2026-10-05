//! Measures the production autodetect pipeline (`sealwax_engine::autoconfig`) on S4's 50 domains
//! or on a frozen held-out list (ML-097), and scores it with the rubric in docs/m2/autoconfig.md.
//! Every lookup uses the dummy address `test@<domain>`; no login is ever attempted. Results go to
//! `results/` next to this crate's manifest.

mod lists;
mod score;
#[path = "../../../spikes/s4-autodetect/src/truth.rs"]
mod truth;

/// S4's truth table names its `Security` through `crate::model`.
mod model {
    pub use sealwax_domain::account::Security;
}

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sealwax_domain::account::{AuthMethod, Detection, Protocol, UsernameForm};
use sealwax_domain::autoconfig::probing::Target;
use sealwax_domain::autoconfig::rank::{self, Wait};
use sealwax_domain::autoconfig::{Finding, Report, Rules, Source, detection};
use sealwax_engine::autoconfig::{
    DetectOptions, NetEvent, Network, Observer, RealNetwork, Trace, detect_with,
};

use lists::{Entry, List};
use score::{AliasPair, Scores, Verdict, alias_candidates, judge, percentile};
use truth::{Accept, Expect, Truth, User};

const USAGE: &str = "usage: sealwax-autodetect-measure run [--list s4|heldout] [--dns IP] [--label NAME] [rule switches] [domain...]
       sealwax-autodetect-measure verify [--list s4|heldout] [--dns IP] [--label NAME]
       sealwax-autodetect-measure truth [--list s4|heldout]
       sealwax-autodetect-measure table --label NAME
rule switches (each turns one rule off): --no-own-host --no-srv-conventional --no-mx-hosts
       --srv-before-ispdb --no-srv-verify --no-autodiscover";

struct Args {
    command: String,
    options: DetectOptions,
    list: List,
    label: Option<String>,
    domains: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut raw = std::env::args().skip(1);
    let command = raw.next().ok_or(USAGE)?;
    let mut options = DetectOptions::default();
    let mut list = List::S4;
    let mut label = None;
    let mut domains = Vec::new();
    while let Some(arg) = raw.next() {
        let rules = &mut options.rules;
        match arg.as_str() {
            "--no-own-host" => rules.own_host = false,
            "--no-srv-conventional" => rules.srv_conventional = false,
            "--no-mx-hosts" => rules.mx_hosts = false,
            "--srv-before-ispdb" => rules.ispdb_before_srv = false,
            "--no-srv-verify" => rules.verify_srv = false,
            "--no-autodiscover" => rules.autodiscover = false,
            "--label" => label = Some(raw.next().ok_or("--label needs a value")?),
            "--list" => match raw.next().as_deref() {
                Some("s4") => list = List::S4,
                Some("heldout") => list = List::Heldout,
                _ => return Err("--list needs s4 or heldout".into()),
            },
            "--dns" => {
                let value = raw.next().ok_or("--dns needs an IP address")?;
                options.dns_server = Some(value.parse().map_err(|_| "bad --dns address")?);
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}\n{USAGE}"));
            }
            domain => domains.push(domain.to_ascii_lowercase()),
        }
    }
    Ok(Args {
        command,
        options,
        list,
        label,
        domains,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.command.as_str() {
        "run" => runtime.block_on(run(&args)),
        "verify" => runtime.block_on(verify(&args)),
        "truth" => lists::entries(args.list).map(|e| print!("{}", truth_table(&e))),
        "table" => table(&args),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("results")
}

fn write_result(name: &str, contents: &str) -> Result<PathBuf, String> {
    let dir = results_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(name);
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
fn utc(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn fmt_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{v:.0}"))
}

fn selected(args: &Args) -> Result<Vec<Entry>, String> {
    let all = lists::entries(args.list)?;
    if args.domains.is_empty() {
        return Ok(all);
    }
    Ok(args
        .domains
        .iter()
        .map(|domain| {
            all.iter()
                .find(|e| e.domain == domain)
                .map(|e| Entry {
                    domain: e.domain,
                    group: e.group,
                    truth: e.truth,
                    evidence: e.evidence,
                })
                .unwrap_or_else(|| Entry {
                    domain: Box::leak(domain.clone().into_boxed_str()),
                    group: "-",
                    truth: None,
                    evidence: "-",
                })
        })
        .collect())
}

type Ledger = Arc<Mutex<Vec<NetEvent>>>;

fn observer(ledger: &Ledger) -> Observer {
    let ledger = ledger.clone();
    Arc::new(move |event| {
        if let Ok(mut events) = ledger.lock() {
            events.push(event);
        }
    })
}

fn events(ledger: &Ledger) -> Vec<NetEvent> {
    ledger.lock().map(|e| e.clone()).unwrap_or_default()
}

/// The dummy address in any spelling a URL may give it.
fn carries_dummy(text: &str, domain: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains(&format!("test@{domain}")) || text.contains(&format!("test%40{domain}"))
}

/// Requests that broke the privacy rule: the dummy address anywhere but the domain's own HTTPS
/// autoconfig, a followed redirect carrying it, plain HTTP, or an `@` in a DNS name.
fn violations(domain: &str, events: &[NetEvent]) -> Vec<String> {
    let own = format!("https://autoconfig.{domain}/");
    events
        .iter()
        .filter_map(|event| match event {
            NetEvent::Https { url } if url.starts_with("http://") => Some(elide(url, domain)),
            NetEvent::Https { url } if carries_dummy(url, domain) && !url.starts_with(&own) => {
                Some(elide(url, domain))
            }
            NetEvent::Redirect {
                url,
                followed: true,
            } if carries_dummy(url, domain) && !url.starts_with(&own) => {
                Some(format!("redirect {}", elide(url, domain)))
            }
            NetEvent::Dns { name, .. } if name.contains('@') => Some(format!("dns {name}")),
            _ => None,
        })
        .collect()
}

fn elide(url: &str, domain: &str) -> String {
    url.replace(&format!("test%40{domain}"), "…")
        .replace(&format!("test@{domain}"), "…")
}

fn event_row(event: &NetEvent, domain: &str) -> (&'static str, String, bool) {
    match event {
        NetEvent::Dns { name, record } => ("dns", format!("{name} {record}"), false),
        NetEvent::Https { url } => ("https", elide(url, domain), carries_dummy(url, domain)),
        NetEvent::Redirect { url, followed } => (
            if *followed {
                "redirect-followed"
            } else {
                "redirect-refused"
            },
            elide(url, domain),
            carries_dummy(url, domain),
        ),
        NetEvent::Connect { host, port } => ("tcp", format!("{host}:{port}"), false),
    }
}

struct Checker {
    network: RealNetwork,
}

impl Checker {
    /// E2's evidence: the same non-empty address set, and the chosen name passes the TLS check.
    async fn alias(&self, pair: &AliasPair) -> (bool, String) {
        let (a, b) = tokio::join!(
            self.network.addresses(&pair.chosen),
            self.network.addresses(&pair.documented)
        );
        match (a, b) {
            (Ok(a), Ok(b)) if a == b => {
                let target = Target::new(pair.protocol, &pair.chosen, pair.port, pair.security);
                match self.network.probe(&target, Duration::from_secs(8)).await {
                    Ok(()) => (
                        true,
                        format!(
                            "{} ≡ {} ({} addresses, TLS ok)",
                            pair.chosen,
                            pair.documented,
                            a.len()
                        ),
                    ),
                    Err(e) => (
                        false,
                        format!(
                            "{} ≡ {} addresses, TLS failed at {:?}",
                            pair.chosen, pair.documented, e.stage
                        ),
                    ),
                }
            }
            (Ok(a), Ok(b)) => (
                false,
                format!(
                    "{} ≢ {}: {} vs {} addresses, not the same set",
                    pair.chosen,
                    pair.documented,
                    a.len(),
                    b.len()
                ),
            ),
            (Err(e), _) | (_, Err(e)) => {
                (false, format!("{} ≢ {}: {e}", pair.chosen, pair.documented))
            }
        }
    }
}

struct Row {
    entry: Entry,
    detection: Detection,
    trace: Trace,
    scores: Option<Scores>,
    alias_notes: Vec<String>,
    setup: Duration,
    final_ms: f64,
    first_usable_ms: Option<f64>,
    events: Vec<NetEvent>,
    violations: Vec<String>,
}

fn verdict_mark(verdict: Option<&Verdict>) -> &'static str {
    match verdict {
        Some(Verdict::Correct) => "yes",
        Some(Verdict::Wrong(_)) => "NO",
        None => "?",
    }
}

fn username_text(form: UsernameForm) -> &'static str {
    match form {
        UsernameForm::Address => "addr",
        UsernameForm::LocalPart => "local",
        UsernameForm::Other => "ask",
    }
}

fn endpoint_text(endpoint: &sealwax_domain::account::Endpoint) -> String {
    let mode = match endpoint.security {
        sealwax_domain::account::Security::Tls => "TLS",
        sealwax_domain::account::Security::StartTls => "STARTTLS",
    };
    format!("{}:{} {mode}", endpoint.host, endpoint.port)
}

fn result_text(detection: &Detection) -> String {
    match detection {
        Detection::Found(s) => {
            let plan = match s.second_username {
                Some(second) => format!("{}→{}", username_text(s.username), username_text(second)),
                None => username_text(s.username).to_string(),
            };
            format!(
                "{} / {} ({plan}{}{})",
                endpoint_text(&s.imap),
                endpoint_text(&s.smtp),
                if s.confirm { ", confirm" } else { "" },
                if matches!(s.auth, AuthMethod::OAuth(_)) {
                    ", OAuth"
                } else {
                    ""
                }
            )
        }
        Detection::Unsupported { provider, reason } => {
            format!("unsupported: {provider} ({reason:?})")
        }
        Detection::NoMail => "no mail".into(),
        Detection::NotFound => "nothing found".into(),
    }
}

fn expect_text(truth: Option<&Truth>) -> &'static str {
    match truth.map(|t| t.expect) {
        Some(Expect::Settings { .. }) => "settings",
        Some(Expect::Unsupported) => "unsupported",
        Some(Expect::NoMail) => "no-mail",
        None => "unknown",
    }
}

fn winners_text(trace: &Trace) -> String {
    let names: Vec<&str> = trace.outcome.winners().iter().map(|s| s.name()).collect();
    match names.as_slice() {
        [] => "-".into(),
        [a, b] if a == b => (*a).to_string(),
        _ => names.join("+"),
    }
}

/// Each source's own answer, judged alone under the strict rubric.
fn source_state(row: &Row, source: Source) -> String {
    let Some(found) = row.trace.sources.iter().find(|t| t.source == source) else {
        return if row.trace.sources.is_empty() || source == Source::Preset {
            "-".into()
        } else {
            "not-done".into()
        };
    };
    let state = match found.finding {
        Finding::Servers { .. } => "found",
        Finding::Nothing => "none",
        Finding::NoMail => "no-mail",
        Finding::Unsupported { .. } => "unsupported",
    };
    let mut text = format!("{state}@{:.0}", ms(found.at));
    if let (Some(truth), Finding::Servers { .. }) = (row.entry.truth, &found.finding) {
        let report = Report {
            source,
            finding: found.finding.clone(),
        };
        let alone = rank::decide(&[source], &[report], Wait::No).outcome;
        let domain = row.trace.domain.as_deref().unwrap_or_default();
        let verdict = judge(truth, &detection(&alone, domain), &|_| false).strict;
        text.push_str(if verdict.is_correct() {
            "/ok"
        } else {
            "/wrong"
        });
    }
    text
}

async fn run(args: &Args) -> Result<(), String> {
    let entries = selected(args)?;
    let label = args.label.clone().unwrap_or_else(|| "run".into());
    let started = SystemTime::now();
    let scoring_ledger: Ledger = Arc::default();
    let checker = Checker {
        network: RealNetwork::new(&args.options, Some(observer(&scoring_ledger)))
            .map_err(|e| e.0)?,
    };

    let mut rows = Vec::new();
    for entry in entries {
        let address = format!("test@{}", entry.domain);
        let ledger: Ledger = Arc::default();
        let began = Instant::now();
        let network = RealNetwork::new(&args.options, Some(observer(&ledger))).map_err(|e| e.0)?;
        let setup = began.elapsed();
        let detect_began = Instant::now();
        let (detection, trace) = detect_with(&network, &address, &args.options, |_| {}).await;
        let detect_time = detect_began.elapsed();
        let preset = trace.sources.len() == 1 && trace.sources[0].source == Source::Preset;
        // Production skips the network setup when a preset answers.
        let offset = if preset { Duration::ZERO } else { setup };
        let final_ms = ms(offset + detect_time);
        let first_usable_ms = trace.first_usable.map(|t| ms(offset + t));

        let mut alias_notes = Vec::new();
        let mut evidence: Vec<(AliasPair, bool)> = Vec::new();
        if let Some(truth) = entry.truth {
            for pair in alias_candidates(truth, &detection) {
                let (same, note) = checker.alias(&pair).await;
                alias_notes.push(note);
                evidence.push((pair, same));
            }
        }
        let same = |pair: &AliasPair| evidence.iter().any(|(p, ok)| p == pair && *ok);
        let scores = entry.truth.map(|t| judge(t, &detection, &same));
        let events = events(&ledger);
        let violations = violations(entry.domain, &events);
        println!(
            "{:28} strict={:3} e1={:3} e2={:3} final={:>6} {:16} {}",
            entry.domain,
            verdict_mark(scores.as_ref().map(|s| &s.strict)),
            verdict_mark(scores.as_ref().map(|s| &s.second)),
            verdict_mark(scores.as_ref().map(|s| &s.aliases)),
            format!("{final_ms:.0}"),
            winners_text(&trace),
            result_text(&detection),
        );
        rows.push(Row {
            entry,
            detection,
            trace,
            scores,
            alias_notes,
            setup,
            final_ms,
            first_usable_ms,
            events,
            violations,
        });
    }

    let scoring_events = events(&scoring_ledger);
    let summary = summarize(&rows, args, &label, started, &scoring_events);
    print!("{summary}");
    for (name, contents) in [
        (format!("{label}.tsv"), run_tsv(&rows)),
        (format!("{label}.summary.txt"), summary),
        (
            format!("{label}.requests.tsv"),
            requests_tsv(&rows, &scoring_events),
        ),
    ] {
        let path = write_result(&name, &contents)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn clean(text: &str) -> String {
    text.replace(['\t', '\n'], " ")
}

fn run_tsv(rows: &[Row]) -> String {
    let mut tsv = String::from(
        "domain\tgroup\texpect\tresult\tsource\twinners\tprovider\tstrict\tstrict_why\te1\te1_why\te1_e2\te1_e2_why\talias_evidence\tfirst_usable_ms\tfinal_ms\tsettled\tsetup_ms\tprobe_started_ms\t",
    );
    for source in Source::ALL {
        let _ = write!(tsv, "{}\t", source.name());
    }
    tsv.push_str(
        "dns\thttps\tredirects_followed\tredirects_refused\ttcp\tviolations\tevidence\tdetails\n",
    );
    for row in rows {
        let scores = row.scores.as_ref();
        let (source, provider) = match &row.detection {
            Detection::Found(s) => (
                format!("{:?}", s.source),
                s.provider.clone().unwrap_or_else(|| "-".into()),
            ),
            _ => ("-".into(), "-".into()),
        };
        let _ = write!(
            tsv,
            "{}\t{}\t{}\t{}\t{source}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{}\t{:.1}\t{}\t",
            row.entry.domain,
            row.entry.group,
            expect_text(row.entry.truth),
            clean(&result_text(&row.detection)),
            winners_text(&row.trace),
            clean(&provider),
            verdict_mark(scores.map(|s| &s.strict)),
            clean(scores.map_or("", |s| s.strict.why())),
            verdict_mark(scores.map(|s| &s.second)),
            clean(scores.map_or("", |s| s.second.why())),
            verdict_mark(scores.map(|s| &s.aliases)),
            clean(scores.map_or("", |s| s.aliases.why())),
            if row.alias_notes.is_empty() {
                "-".into()
            } else {
                clean(&row.alias_notes.join("; "))
            },
            fmt_ms(row.first_usable_ms),
            row.final_ms,
            row.trace.settled,
            ms(row.setup),
            fmt_ms(row.trace.probe_started.map(ms)),
        );
        for source in Source::ALL {
            let _ = write!(tsv, "{}\t", source_state(row, source));
        }
        let count = |kind: &str| {
            row.events
                .iter()
                .filter(|e| event_row(e, row.entry.domain).0 == kind)
                .count()
        };
        let details: Vec<String> = row
            .trace
            .sources
            .iter()
            .map(|t| format!("{}: {}", t.source.name(), t.detail))
            .collect();
        let _ = writeln!(
            tsv,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            count("dns"),
            count("https"),
            count("redirect-followed"),
            count("redirect-refused"),
            count("tcp"),
            if row.violations.is_empty() {
                "-".into()
            } else {
                clean(&row.violations.join("; "))
            },
            clean(row.entry.evidence),
            clean(&elide(&details.join(" || "), row.entry.domain)),
        );
    }
    tsv
}

fn requests_tsv(rows: &[Row], scoring: &[NetEvent]) -> String {
    let mut out = String::from("domain\tphase\tkind\ttarget\tcarries_dummy_address\n");
    for row in rows {
        for event in &row.events {
            let (kind, target, carries) = event_row(event, row.entry.domain);
            let _ = writeln!(
                out,
                "{}\tdetect\t{kind}\t{target}\t{carries}",
                row.entry.domain
            );
        }
    }
    for event in scoring {
        let (kind, target, carries) = event_row(event, "-");
        let _ = writeln!(out, "-\tscoring\t{kind}\t{target}\t{carries}");
    }
    out
}

fn timing_line(name: &str, values: &[f64]) -> String {
    format!(
        "{name}: n={} p50={} p95={} max={} ms\n",
        values.len(),
        fmt_ms(percentile(values, 50.0)),
        fmt_ms(percentile(values, 95.0)),
        fmt_ms(percentile(values, 100.0)),
    )
}

fn strict(scores: &Scores) -> &Verdict {
    &scores.strict
}

fn second(scores: &Scores) -> &Verdict {
    &scores.second
}

fn aliases(scores: &Scores) -> &Verdict {
    &scores.aliases
}

fn summarize(
    rows: &[Row],
    args: &Args,
    label: &str,
    started: SystemTime,
    scoring: &[NetEvent],
) -> String {
    let options = &args.options;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "== {label}: {} domains ({:?} list), started {}",
        rows.len(),
        args.list,
        utc(started)
    );
    let Rules {
        own_host,
        srv_conventional,
        mx_hosts,
        ispdb_before_srv,
        verify_srv,
        autodiscover,
    } = options.rules;
    let _ = writeln!(
        out,
        "rules: own_host={own_host} srv_conventional={srv_conventional} mx_hosts={mx_hosts} ispdb_before_srv={ispdb_before_srv} verify_srv={verify_srv} autodiscover={autodiscover}"
    );
    let _ = writeln!(
        out,
        "options: resolver={} order={} cap={}ms http={}ms dns={}ms source={}ms well_known={}ms mx_autoconfig={}ms connect={}ms probe_delay={}ms probe_budget={}ms user_agent={:?}",
        options
            .dns_server
            .map_or_else(|| "system".to_string(), |ip| ip.to_string()),
        options
            .rules
            .order()
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(">"),
        options.cap.as_millis(),
        options.http_timeout.as_millis(),
        options.dns_timeout.as_millis(),
        options.source_timeout.as_millis(),
        options.well_known_timeout.as_millis(),
        options.mx_autoconfig_timeout.as_millis(),
        options.connect_timeout.as_millis(),
        options.probe_delay.as_millis(),
        options.probe_budget.as_millis(),
        options.user_agent,
    );

    let judged: Vec<&Row> = rows.iter().filter(|r| r.scores.is_some()).collect();
    let count = |pick: fn(&Scores) -> &Verdict, rows: &[&Row]| {
        rows.iter()
            .filter(|r| r.scores.as_ref().is_some_and(|s| pick(s).is_correct()))
            .count()
    };
    let n = judged.len();
    let _ = writeln!(out, "correct, strict: {}/{n}", count(strict, &judged));
    let _ = writeln!(
        out,
        "correct, with the second login attempt (E1): {}/{n}",
        count(second, &judged)
    );
    let _ = writeln!(
        out,
        "correct, E1 plus verified aliases (E2): {}/{n}",
        count(aliases, &judged)
    );
    let added = |from: fn(&Scores) -> &Verdict, to: fn(&Scores) -> &Verdict| -> Vec<&str> {
        judged
            .iter()
            .filter(|r| {
                r.scores
                    .as_ref()
                    .is_some_and(|s| !from(s).is_correct() && to(s).is_correct())
            })
            .map(|r| r.entry.domain)
            .collect()
    };
    let _ = writeln!(out, "  E1 adds: {:?}", added(strict, second));
    let _ = writeln!(out, "  E2 adds: {:?}", added(second, aliases));
    let unknown: Vec<&str> = rows
        .iter()
        .filter(|r| r.scores.is_none())
        .map(|r| r.entry.domain)
        .collect();
    let _ = writeln!(out, "truth unknown (not scored): {unknown:?}");
    let mut groups: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in &judged {
        groups.entry(row.entry.group).or_default().push(row);
    }
    for (group, members) in &groups {
        let _ = writeln!(
            out,
            "  {group}: strict {}/{m}, E1 {}/{m}, E1+E2 {}/{m}",
            count(strict, members),
            count(second, members),
            count(aliases, members),
            m = members.len()
        );
    }
    let wrong: Vec<String> = judged
        .iter()
        .filter_map(|r| {
            let s = r.scores.as_ref()?;
            (!s.strict.is_correct()).then(|| format!("{}: {}", r.entry.domain, s.strict.why()))
        })
        .collect();
    let _ = writeln!(out, "strict misses: {wrong:#?}");

    let found = rows
        .iter()
        .filter(|r| matches!(r.detection, Detection::Found(_)))
        .count();
    let _ = writeln!(
        out,
        "TLS-only: {found}/{found} offered settings use TLS or STARTTLS (the contract has no plaintext mode)"
    );
    let confirm: Vec<&str> = rows
        .iter()
        .filter(|r| matches!(&r.detection, Detection::Found(s) if s.confirm))
        .map(|r| r.entry.domain)
        .collect();
    let _ = writeln!(out, "shown for confirmation: {confirm:?}");
    let finals: Vec<f64> = rows.iter().map(|r| r.final_ms).collect();
    let firsts: Vec<f64> = rows.iter().filter_map(|r| r.first_usable_ms).collect();
    out.push_str(&timing_line("final", &finals));
    out.push_str(&timing_line("first usable", &firsts));
    let unsettled: Vec<&str> = rows
        .iter()
        .filter(|r| !r.trace.settled)
        .map(|r| r.entry.domain)
        .collect();
    let _ = writeln!(out, "hit the cap unsettled: {unsettled:?}");
    let mut winners: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *winners.entry(winners_text(&row.trace)).or_default() += 1;
    }
    let _ = writeln!(out, "won by: {winners:?}");

    let all: Vec<(&str, &NetEvent)> = rows
        .iter()
        .flat_map(|r| r.events.iter().map(move |e| (r.entry.domain, e)))
        .collect();
    let kinds = |kind: &str| {
        all.iter()
            .filter(|(domain, e)| event_row(e, domain).0 == kind)
            .count()
    };
    let carrying: Vec<&(&str, &NetEvent)> = all
        .iter()
        .filter(|(domain, e)| event_row(e, domain).2)
        .collect();
    let _ = writeln!(
        out,
        "requests: {} (dns {}, https {}, redirects followed {}, refused {}, tcp {}); scoring checks after detection: {}",
        all.len(),
        kinds("dns"),
        kinds("https"),
        kinds("redirect-followed"),
        kinds("redirect-refused"),
        kinds("tcp"),
        scoring.len()
    );
    let own = carrying
        .iter()
        .filter(|(domain, e)| {
            matches!(e, NetEvent::Https { url } if url.starts_with(&format!("https://autoconfig.{domain}/")))
        })
        .count();
    let _ = writeln!(
        out,
        "carrying the dummy address: {} ({own} to the row's own https://autoconfig.<domain>/)",
        carrying.len()
    );
    let violations: Vec<&String> = rows.iter().flat_map(|r| &r.violations).collect();
    let _ = writeln!(
        out,
        "privacy violations: {} {violations:?}",
        violations.len()
    );
    let scoring_carrying = scoring
        .iter()
        .filter(|e| event_row(e, "-").2 || matches!(e, NetEvent::Https { .. }))
        .count();
    let _ = writeln!(
        out,
        "scoring checks carrying an address or using HTTPS: {scoring_carrying}"
    );
    out
}

async fn verify(args: &Args) -> Result<(), String> {
    let entries = selected(args)?;
    let label = args.label.clone().unwrap_or_else(|| "verify".into());
    let ledger: Ledger = Arc::default();
    let network = RealNetwork::new(&args.options, Some(observer(&ledger))).map_err(|e| e.0)?;
    let mut out = String::from("domain\tprotocol\tendpoint\tresult\n");
    let (mut ok, mut total) = (0, 0);
    for entry in &entries {
        let Some(Truth {
            expect: Expect::Settings { imap, smtp },
            ..
        }) = entry.truth
        else {
            continue;
        };
        for (protocol, accepted) in [(Protocol::Imap, *imap), (Protocol::Smtp, *smtp)] {
            for accept in accepted {
                let target = Target::new(protocol, accept.host, accept.port, accept.security);
                let result = network.probe(&target, Duration::from_secs(8)).await;
                total += 1;
                let text = match result {
                    Ok(()) => {
                        ok += 1;
                        "ok".to_string()
                    }
                    Err(e) => format!("{:?}: {}", e.stage, clean(&e.detail)),
                };
                println!(
                    "{:24} {protocol:?} {}:{} {:?} {text}",
                    entry.domain, accept.host, accept.port, accept.security
                );
                let _ = writeln!(
                    out,
                    "{}\t{protocol:?}\t{}:{} {:?}\t{text}",
                    entry.domain, accept.host, accept.port, accept.security
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "# {ok} of {total} documented endpoints passed; {} requests; {}",
        events(&ledger).len(),
        utc(SystemTime::now())
    );
    let path = write_result(&format!("{label}.tsv"), &out)?;
    println!("{ok} of {total} passed; wrote {}", path.display());
    Ok(())
}

fn users_text(users: &[User]) -> String {
    users
        .iter()
        .map(|u| match u {
            User::Address => "addr",
            User::LocalPart => "local",
            User::Other => "other",
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn accepts_text(list: &[Accept]) -> String {
    list.iter()
        .map(|a| {
            format!(
                "{}:{} {:?} ({})",
                a.host,
                a.port,
                a.security,
                users_text(a.users)
            )
        })
        .collect::<Vec<_>>()
        .join("<br>")
}

fn truth_table(entries: &[Entry]) -> String {
    let mut out = String::from(
        "| # | Domain | Group | IMAP accepted | SMTP accepted | Evidence |\n|---|---|---|---|---|---|\n",
    );
    for (index, entry) in entries.iter().enumerate() {
        let (imap, smtp) = match entry.truth.map(|t| t.expect) {
            Some(Expect::Settings { imap, smtp }) => (accepts_text(imap), accepts_text(smtp)),
            Some(Expect::Unsupported) => ("not supported".into(), "-".into()),
            Some(Expect::NoMail) => ("no mail service".into(), "-".into()),
            None => ("truth unknown".into(), "-".into()),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {imap} | {smtp} | {} |",
            index + 1,
            entry.domain,
            entry.group,
            entry.evidence.replace('|', "\\|")
        );
    }
    out
}

fn table(args: &Args) -> Result<(), String> {
    let label = args.label.as_deref().ok_or("table needs --label")?;
    let path = results_dir().join(format!("{label}.tsv"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().ok_or("empty file")?.split('\t').collect();
    let column = |name: &str| header.iter().position(|h| *h == name);
    let mut out = String::from(
        "| # | Domain | Group | Result (IMAP / SMTP, username plan) | Won by | Strict | E1 | E1+E2 | Final ms |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    for (index, line) in lines.enumerate() {
        let cells: Vec<&str> = line.split('\t').collect();
        let cell = |name: &str| {
            column(name)
                .and_then(|i| cells.get(i))
                .copied()
                .unwrap_or("")
        };
        let mark = |verdict: &str, why: &str| {
            if verdict == "yes" {
                "yes".to_string()
            } else {
                format!("**no**: {why}")
            }
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {:.0} |",
            index + 1,
            cell("domain"),
            cell("group"),
            cell("result").replace(" / ", "<br>"),
            cell("winners"),
            mark(cell("strict"), cell("strict_why")),
            mark(cell("e1"), cell("e1_why")),
            mark(cell("e1_e2"), cell("e1_e2_why")),
            cell("final_ms").parse::<f64>().unwrap_or(0.0),
        );
    }
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats_unix_time() {
        let time = UNIX_EPOCH + Duration::from_secs(1_791_189_743);
        assert_eq!(utc(time), "2026-10-05T08:42:23Z");
        assert_eq!(utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_audit_flags_the_address_outside_the_domains_autoconfig() {
        let domain = "example.test";
        let events = vec![
            NetEvent::Https {
                url: "https://autoconfig.example.test/mail/config-v1.1.xml?emailaddress=test%40example.test".into(),
            },
            NetEvent::Https {
                url: "https://autoconfig.thunderbird.net/v1.1/example.test".into(),
            },
            NetEvent::Redirect {
                url: "https://www.example.test/?emailaddress=test%40example.test".into(),
                followed: false,
            },
        ];
        assert!(violations(domain, &events).is_empty());
        let leak = vec![NetEvent::Redirect {
            url: "https://www.example.test/?emailaddress=test%40example.test".into(),
            followed: true,
        }];
        assert_eq!(violations(domain, &leak).len(), 1);
        assert_eq!(
            elide(
                "https://autoconfig.example.test/x?emailaddress=test%40example.test",
                domain
            ),
            "https://autoconfig.example.test/x?emailaddress=…"
        );
    }
}
