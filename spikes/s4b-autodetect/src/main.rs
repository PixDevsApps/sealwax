//! Spike S4b: account autodetection with general rules only (M0). Measurement code only; production crates never depend on it.

mod clientconfig;
mod dnsdata;
mod fetch;
mod heldout;
mod model;
mod pipeline;
mod presets;
mod probe;
mod rank;
mod score;
#[path = "../../s4-autodetect/src/truth.rs"]
mod truth;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use model::{Choice, Finding, Outcome, Protocol, Source, Username};
use pipeline::{Discovery, Options, Rules};
use probe::{ProbeCache, Target};
use rank::{Wait, decide};
use score::{AliasPair, Verdict, alias_candidates, judge, judge_extended, percentile, tls_only};
use truth::{Expect, TRUTH, Truth};

const USAGE: &str = "usage: email-spike-s4b run [--list s4|heldout] [--dns IP] [--label NAME] [--no-presets] [--exhaustive] [--no-own-host] [--no-srv-conventional] [--no-mx-hosts] [domain...]
       email-spike-s4b show [run options] <domain>
       email-spike-s4b verify [--list s4|heldout] [--dns IP] [--label NAME]
       email-spike-s4b truth [--list s4|heldout]
       email-spike-s4b table --label NAME [--list s4|heldout]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum List {
    S4,
    Heldout,
}

struct Args {
    command: String,
    options: Options,
    list: List,
    label: Option<String>,
    domains: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut raw = std::env::args().skip(1);
    let command = raw.next().ok_or(USAGE)?;
    let mut options = Options::default();
    let mut list = List::S4;
    let mut label = None;
    let mut domains = Vec::new();
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--no-presets" => options.presets = false,
            "--exhaustive" => options.exhaustive = true,
            "--no-own-host" => options.rules.own_host = false,
            "--no-srv-conventional" => options.rules.srv_conventional = false,
            "--no-mx-hosts" => options.rules.mx_hosts = false,
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
    let _ = rustls::crypto::ring::default_provider().install_default();
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
        "run" => runtime.block_on(run(args)),
        "verify" => runtime.block_on(verify(args)),
        "show" => runtime.block_on(show(args)),
        "truth" => entries(args.list).map(|entries| print!("{}", truth_table(&entries))),
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

struct Entry {
    domain: &'static str,
    group: &'static str,
    truth: Option<&'static Truth>,
    evidence: &'static str,
}

fn heldout_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/heldout.tsv")
}

fn entries(list: List) -> Result<Vec<Entry>, String> {
    match list {
        List::S4 => Ok(TRUTH
            .iter()
            .map(|t| Entry {
                domain: t.domain,
                group: t.group,
                truth: Some(t),
                evidence: t.evidence,
            })
            .collect()),
        List::Heldout => {
            let path = heldout_path();
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(heldout::parse(&text)?
                .into_iter()
                .map(|row| Entry {
                    domain: row.domain,
                    group: row.group,
                    truth: row.truth.map(|t| &*Box::leak(Box::new(t))),
                    evidence: row.evidence,
                })
                .collect())
        }
    }
}

fn results_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("results")
}

fn write_result(name: &str, contents: &str) -> Result<PathBuf, String> {
    let dir = results_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(name);
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn choice_text(choice: Option<&Choice>) -> String {
    choice.map_or_else(|| "-".to_string(), |c| c.server.to_string())
}

fn outcome_text(outcome: &Outcome) -> (String, String, String) {
    match outcome {
        Outcome::Settings { imap, smtp, hints } => {
            let mut notes = Vec::new();
            if let Some(provider) = &hints.provider {
                notes.push(format!("provider={provider}"));
            }
            if let Some(oauth) = &hints.oauth {
                notes.push(format!("oauth={oauth}"));
            }
            if hints.app_password {
                notes.push("app-password".into());
            }
            if hints.enable_imap {
                notes.push("enable-imap".into());
            }
            let kind = if imap.is_none() && smtp.is_none() {
                "nothing-found".to_string()
            } else {
                format!("settings {}", notes.join(" "))
            };
            (
                kind,
                choice_text(imap.as_deref()),
                choice_text(smtp.as_deref()),
            )
        }
        Outcome::Unsupported { provider, why, .. } => (
            format!("unsupported {provider} {why:?}"),
            "-".into(),
            "-".into(),
        ),
        Outcome::NoMail { why, .. } => (format!("no-mail {why}"), "-".into(), "-".into()),
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

fn source_status(discovery: &Discovery, source: Source) -> String {
    match discovery.report(source) {
        None if matches!(source, Source::Probe | Source::MxProbe)
            && discovery.probe_started_ms.is_none() =>
        {
            "not-started".into()
        }
        None if discovery.setup_ms > 0.0 => "cancelled".into(),
        None => "-".into(),
        Some(report) => {
            let state = match &report.result {
                Err(_) => "error",
                Ok(Finding::Nothing) => "none",
                Ok(Finding::Servers { .. }) => "found",
                Ok(Finding::Unsupported { .. }) => "unsupported",
                Ok(Finding::NoMail(_)) => "no-mail",
            };
            format!("{state}@{:.0}", report.elapsed_ms)
        }
    }
}

fn privacy_violations(domain: &str, requests: &[fetch::Request]) -> Vec<String> {
    let allowed = format!("autoconfig.{domain}/");
    let inside = |target: &str| {
        target
            .strip_prefix("https://")
            .is_some_and(|host| fetch::within(host, domain))
    };
    requests
        .iter()
        .filter(|r| r.carries_address || r.target.contains('@') || r.target.contains("%40"))
        .filter(|r| match r.kind {
            "https" => !r.target.starts_with(&allowed) || r.target.contains('@'),
            "redirect-followed" => !inside(&r.target),
            "redirect-refused" => false,
            _ => true,
        })
        .map(|r| format!("{} {}", r.kind, r.target))
        .collect()
}

fn source_alone(discovery: &Discovery, source: Source) -> Option<Outcome> {
    let report = discovery.report(source)?;
    Some(decide(&[source], std::slice::from_ref(report), Wait::No).outcome)
}

fn fmt_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{v:.0}"))
}

fn winners(outcome: &Outcome) -> Vec<Source> {
    match outcome {
        Outcome::Settings { imap, smtp, .. } => [imap, smtp]
            .iter()
            .filter_map(|c| c.as_ref().map(|c| c.source))
            .collect(),
        _ => Vec::new(),
    }
}

fn rules_fired(discovery: &Discovery) -> Vec<&'static str> {
    let mut fired = Vec::new();
    let won = winners(&discovery.outcome);
    if won.contains(&Source::OwnHost) {
        let mx_had_servers = discovery.reports.iter().any(|r| {
            r.source.is_mx_derived() && matches!(r.finding(), Some(Finding::Servers { .. }))
        });
        fired.push(if mx_had_servers {
            "own-host-over-mx"
        } else {
            "own-host"
        });
    }
    if let Outcome::Settings { imap, smtp, .. } = &discovery.outcome
        && let Some(srv) = discovery.report(Source::Srv)
        && [imap, smtp].iter().flat_map(|c| c.iter()).any(|c| {
            c.source == Source::Srv && srv.rewrites.iter().any(|(_, to)| *to == c.server.host)
        })
    {
        fired.push("srv-conventional");
    }
    if won.contains(&Source::MxAutoconfig) {
        fired.push("mx-autoconfig");
    }
    if won.contains(&Source::MxProbe) {
        fired.push("mx-probe");
    }
    fired
}

fn ask_when_only_mx_hosts_probed(outcome: &Outcome) -> Outcome {
    let mut outcome = outcome.clone();
    if let Outcome::Settings { imap, smtp, .. } = &mut outcome {
        for choice in [imap, smtp].into_iter().flatten() {
            if choice.source == Source::MxProbe {
                choice.server.username = Username::Template("ask".into());
            }
        }
    }
    outcome
}

struct Checker {
    resolver: TokioResolver,
    tls: Arc<rustls::ClientConfig>,
    ledger: fetch::Ledger,
    probes: ProbeCache,
}

impl Checker {
    async fn alias(&self, pair: &AliasPair) -> (bool, String) {
        for host in [&pair.chosen, &pair.documented] {
            self.ledger
                .record("dns", format!("{host} AAAA/A (scoring)"), false);
        }
        let (a, b) = tokio::join!(
            fetch::addresses(&self.resolver, &pair.chosen),
            fetch::addresses(&self.resolver, &pair.documented)
        );
        let count = match (a, b) {
            (Ok(a), Ok(b)) if !a.is_empty() && a == b => a.len(),
            (Ok(a), Ok(b)) => {
                return (
                    false,
                    format!(
                        "{} ≢ {}: {} vs {} addresses, not the same set",
                        pair.chosen,
                        pair.documented,
                        a.len(),
                        b.len()
                    ),
                );
            }
            (Err(e), _) | (_, Err(e)) => {
                return (
                    false,
                    format!("{} ≢ {}: lookup {e}", pair.chosen, pair.documented),
                );
            }
        };
        let probe = self
            .probes
            .probe(
                Target {
                    protocol: pair.protocol,
                    host: pair.chosen.clone(),
                    port: pair.port,
                    security: pair.security,
                },
                self.tls.clone(),
                Duration::from_secs(8),
                &self.resolver,
                Some(&self.ledger),
            )
            .await;
        match probe.result {
            Ok(_) => (
                true,
                format!(
                    "{} ≡ {} ({count} addresses, TLS ok)",
                    pair.chosen, pair.documented
                ),
            ),
            Err((stage, _)) => (
                false,
                format!(
                    "{} ≡ {} addresses but TLS failed at {stage:?}",
                    pair.chosen, pair.documented
                ),
            ),
        }
    }
}

struct Row {
    entry: Entry,
    discovery: Discovery,
    strict: Option<Verdict>,
    extended: Option<(Verdict, score::Via)>,
    alt_strict: Option<Verdict>,
    alias_notes: Vec<String>,
    fired: Vec<&'static str>,
}

fn verdict_text(verdict: Option<&Verdict>) -> (&'static str, String) {
    match verdict {
        Some(Verdict::Correct) => ("yes", String::new()),
        Some(Verdict::Wrong(why)) => ("NO", why.clone()),
        None => ("?", String::new()),
    }
}

async fn run(args: Args) -> Result<(), String> {
    let tls = probe::tls_config()?;
    let all = entries(args.list)?;
    let selected: Vec<Entry> = if args.domains.is_empty() {
        all
    } else {
        let mut picked = Vec::new();
        for domain in &args.domains {
            match all.iter().position(|e| e.domain == domain) {
                Some(index) => picked.push(Entry {
                    domain: all[index].domain,
                    group: all[index].group,
                    truth: all[index].truth,
                    evidence: all[index].evidence,
                }),
                None => picked.push(Entry {
                    domain: Box::leak(domain.clone().into_boxed_str()),
                    group: "-",
                    truth: None,
                    evidence: "-",
                }),
            }
        }
        picked
    };
    let label = args.label.clone().unwrap_or_else(|| "run".into());
    let started = std::time::SystemTime::now();
    let checker = Checker {
        resolver: fetch::resolver(Duration::from_secs(2), args.options.dns_server)?,
        tls: tls.clone(),
        ledger: fetch::Ledger::default(),
        probes: ProbeCache::default(),
    };

    let mut rows: Vec<Row> = Vec::new();
    for entry in selected {
        let discovery = pipeline::discover(entry.domain, &args.options, tls.clone()).await?;
        let truth = entry.truth;
        let strict = truth.map(|t| judge(t, &discovery.outcome));
        let mut evidence: Vec<(AliasPair, bool)> = Vec::new();
        let mut alias_notes = Vec::new();
        if let Some(truth) = truth {
            for pair in alias_candidates(truth, &discovery.outcome) {
                let (same, note) = checker.alias(&pair).await;
                alias_notes.push(note);
                evidence.push((pair, same));
            }
        }
        let same = |pair: &AliasPair| evidence.iter().any(|(p, ok)| p == pair && *ok);
        let extended = truth.map(|t| judge_extended(t, &discovery.outcome, &same));
        let alt_strict =
            truth.map(|t| judge(t, &ask_when_only_mx_hosts_probed(&discovery.outcome)));
        let fired = rules_fired(&discovery);
        let (strict_mark, strict_why) = verdict_text(strict.as_ref());
        let (ext_mark, _) = verdict_text(extended.as_ref().map(|(v, _)| v));
        let (kind, imap, smtp) = outcome_text(&discovery.outcome);
        println!(
            "{:28} strict={strict_mark:3} ext={ext_mark:3} final={:>6} winner={:18} {kind} | {imap} | {smtp} {strict_why} {}",
            entry.domain,
            format!("{:.0}", discovery.final_ms),
            discovery.outcome.winners(),
            if fired.is_empty() {
                String::new()
            } else {
                format!("[{}]", fired.join(","))
            },
        );
        rows.push(Row {
            entry,
            discovery,
            strict,
            extended,
            alt_strict,
            alias_notes,
            fired,
        });
    }

    let tsv = run_tsv(&rows);
    let requests = requests_tsv(&rows, &checker.ledger);
    let summary = summarize(&rows, &args, &label, started, &checker.ledger);
    print!("{summary}");
    for (name, contents) in [
        (format!("{label}.tsv"), &tsv),
        (format!("{label}.summary.txt"), &summary),
        (format!("{label}.requests.tsv"), &requests),
    ] {
        let path = write_result(&name, contents)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn run_tsv(rows: &[Row]) -> String {
    let mut tsv = String::from(
        "domain\tgroup\texpect\toutcome\timap\tsmtp\twinner\tstrict\tstrict_why\textended\textended_via\textended_why\talias_evidence\trules_fired\tstrict_if_ask_for_mx_probe\tfirst_usable_ms\tfinal_ms\tsettled\tsetup_ms\tprobe_started_ms\t",
    );
    for source in Source::ALL {
        let _ = write!(tsv, "{source}\t");
    }
    tsv.push_str("requests\ttcp_connections\tevidence\tdetails\n");
    for row in rows {
        let discovery = &row.discovery;
        let (kind, imap, smtp) = outcome_text(&discovery.outcome);
        let (strict, strict_why) = verdict_text(row.strict.as_ref());
        let (extended, extended_why) = verdict_text(row.extended.as_ref().map(|(v, _)| v));
        let via = match &row.extended {
            Some((Verdict::Correct, via)) => via.text(),
            _ => "-",
        };
        let (alt, _) = verdict_text(row.alt_strict.as_ref());
        let _ = write!(
            tsv,
            "{}\t{}\t{}\t{kind}\t{imap}\t{smtp}\t{}\t{strict}\t{strict_why}\t{extended}\t{via}\t{extended_why}\t{}\t{}\t{alt}\t{}\t{:.1}\t{}\t{:.1}\t{}\t",
            row.entry.domain,
            row.entry.group,
            expect_text(row.entry.truth),
            discovery.outcome.winners(),
            if row.alias_notes.is_empty() {
                "-".to_string()
            } else {
                row.alias_notes.join("; ")
            },
            if row.fired.is_empty() {
                "-".to_string()
            } else {
                row.fired.join(",")
            },
            fmt_ms(discovery.first_usable_ms),
            discovery.final_ms,
            discovery.settled,
            discovery.setup_ms,
            fmt_ms(discovery.probe_started_ms),
        );
        for source in Source::ALL {
            let mut status = source_status(discovery, source);
            if let (Some(truth), Some(outcome)) = (row.entry.truth, source_alone(discovery, source))
                && !matches!(
                    outcome,
                    Outcome::Settings {
                        imap: None,
                        smtp: None,
                        ..
                    }
                )
            {
                status.push_str(match judge(truth, &outcome) {
                    Verdict::Correct => "/ok",
                    Verdict::Wrong(_) => "/wrong",
                });
            }
            let _ = write!(tsv, "{status}\t");
        }
        let tcp = discovery
            .requests
            .iter()
            .filter(|r| r.kind == "tcp")
            .count();
        let details: Vec<String> = discovery
            .reports
            .iter()
            .map(|r| format!("{}: {}", r.source, r.detail))
            .collect();
        let _ = writeln!(
            tsv,
            "{}\t{tcp}\t{}\t{}",
            discovery.requests.len(),
            row.entry.evidence.replace(['\t', '\n'], " "),
            details.join(" || ").replace(['\t', '\n'], " ")
        );
    }
    tsv
}

fn requests_tsv(rows: &[Row], scoring: &fetch::Ledger) -> String {
    let mut out = String::from("domain\tphase\tkind\ttarget\tcarries_dummy_address\n");
    for row in rows {
        for request in &row.discovery.requests {
            let _ = writeln!(
                out,
                "{}\tdiscovery\t{}\t{}\t{}",
                row.discovery.domain, request.kind, request.target, request.carries_address
            );
        }
    }
    for request in scoring.entries() {
        let _ = writeln!(
            out,
            "-\tscoring\t{}\t{}\t{}",
            request.kind, request.target, request.carries_address
        );
    }
    out
}

fn line(name: &str, values: &[f64]) -> String {
    format!(
        "{name}: n={} p50={} p95={} max={} ms\n",
        values.len(),
        fmt_ms(percentile(values, 50.0)),
        fmt_ms(percentile(values, 95.0)),
        fmt_ms(percentile(values, 100.0)),
    )
}

fn summarize(
    rows: &[Row],
    args: &Args,
    label: &str,
    started: std::time::SystemTime,
    scoring: &fetch::Ledger,
) -> String {
    let options = &args.options;
    let mut out = String::new();
    let since_epoch = started
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(
        out,
        "== {label}: {} domains ({:?} list), started at unix {since_epoch}",
        rows.len(),
        args.list
    );
    let Rules {
        own_host,
        srv_conventional,
        mx_hosts,
    } = options.rules;
    let _ = writeln!(
        out,
        "options: presets={} exhaustive={} rules: own_host={own_host} srv_conventional={srv_conventional} mx_hosts={mx_hosts}; resolver={} order={} probe_delay={}ms cap={}ms http={}ms dns={}ms source={}ms well_known={}ms mx_autoconfig={}ms own_host={}ms probe_connect={}ms probe_budget={}ms",
        options.presets,
        options.exhaustive,
        options
            .dns_server
            .map_or_else(|| "system".to_string(), |ip| ip.to_string()),
        Source::ALL
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(">"),
        options.probe_delay.as_millis(),
        options.cap.as_millis(),
        options.http_timeout.as_millis(),
        options.dns_timeout.as_millis(),
        options.source_timeout.as_millis(),
        options.well_known_timeout.as_millis(),
        options.mx_autoconfig_timeout.as_millis(),
        options.own_host_timeout.as_millis(),
        options.probe_connect_timeout.as_millis(),
        options.probe_budget.as_millis(),
    );
    let judged: Vec<&Row> = rows.iter().filter(|r| r.strict.is_some()).collect();
    let unknown: Vec<&str> = rows
        .iter()
        .filter(|r| r.strict.is_none())
        .map(|r| r.entry.domain)
        .collect();
    let strict = judged
        .iter()
        .filter(|r| r.strict.as_ref().is_some_and(Verdict::is_correct))
        .count();
    let extended = judged
        .iter()
        .filter(|r| r.extended.as_ref().is_some_and(|(v, _)| v.is_correct()))
        .count();
    let _ = writeln!(out, "correct (strict rubric): {strict}/{}", judged.len());
    let _ = writeln!(
        out,
        "correct (extended rubric): {extended}/{}",
        judged.len()
    );
    let added = |via: &str| -> Vec<&str> {
        judged
            .iter()
            .filter(|r| r.strict.as_ref().is_some_and(|v| !v.is_correct()))
            .filter(|r| {
                r.extended
                    .as_ref()
                    .is_some_and(|(v, x)| v.is_correct() && x.text() == via)
            })
            .map(|r| r.entry.domain)
            .collect()
    };
    for via in ["E1", "E2", "E1+E2"] {
        let list = added(via);
        if !list.is_empty() {
            let _ = writeln!(out, "  extended adds via {via}: {}", list.join(" "));
        }
    }
    let alt = judged
        .iter()
        .filter(|r| r.alt_strict.as_ref().is_some_and(Verdict::is_correct))
        .count();
    let _ = writeln!(
        out,
        "replay, not a rule: strict score if MX-host-probe results asked for the username: {alt}/{}",
        judged.len()
    );
    if !unknown.is_empty() {
        let _ = writeln!(
            out,
            "truth unknown, not scored: {} ({})",
            unknown.len(),
            unknown.join(" ")
        );
    }
    let tls = rows
        .iter()
        .filter(|r| tls_only(&r.discovery.outcome))
        .count();
    let _ = writeln!(out, "TLS-only final results: {tls}/{}", rows.len());

    let mut groups: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for row in &judged {
        let entry = groups.entry(row.entry.group).or_default();
        entry.2 += 1;
        if row.strict.as_ref().is_some_and(Verdict::is_correct) {
            entry.0 += 1;
        }
        if row.extended.as_ref().is_some_and(|(v, _)| v.is_correct()) {
            entry.1 += 1;
        }
    }
    for (group, (s, e, total)) in &groups {
        let _ = writeln!(
            out,
            "  group {group}: strict {s}/{total}, extended {e}/{total}"
        );
    }

    let finals: Vec<f64> = rows.iter().map(|r| r.discovery.final_ms).collect();
    let firsts: Vec<f64> = rows
        .iter()
        .filter_map(|r| r.discovery.first_usable_ms)
        .collect();
    out.push_str(&line("final ranked result", &finals));
    out.push_str(&line("first usable result", &firsts));
    let all_groups: Vec<&str> = {
        let mut g: Vec<&str> = rows.iter().map(|r| r.entry.group).collect();
        g.sort();
        g.dedup();
        g
    };
    for group in all_groups {
        let values: Vec<f64> = rows
            .iter()
            .filter(|r| r.entry.group == group)
            .map(|r| r.discovery.final_ms)
            .collect();
        out.push_str(&line(&format!("  final, group {group}"), &values));
    }
    let unsettled = rows.iter().filter(|r| !r.discovery.settled).count();
    let _ = writeln!(
        out,
        "hit the {} ms cap or ended unsettled: {unsettled}",
        options.cap.as_millis()
    );

    let mut winners: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *winners.entry(row.discovery.outcome.winners()).or_default() += 1;
    }
    let _ = writeln!(
        out,
        "winning source: {}",
        winners
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut fired: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for row in rows {
        for rule in &row.fired {
            fired.entry(rule).or_default().push(row.entry.domain);
        }
    }
    for (rule, domains) in &fired {
        let _ = writeln!(out, "rule {rule} decided: {}", domains.join(" "));
    }

    let probed = rows
        .iter()
        .filter(|r| r.discovery.probe_started_ms.is_some())
        .count();
    let tcp_total: usize = rows
        .iter()
        .map(|r| {
            r.discovery
                .requests
                .iter()
                .filter(|q| q.kind == "tcp")
                .count()
        })
        .sum();
    let mut busiest = 0;
    for row in rows {
        let mut per_host: BTreeMap<String, usize> = BTreeMap::new();
        for request in row.discovery.requests.iter().filter(|r| r.kind == "tcp") {
            let host = request
                .target
                .rsplit_once(':')
                .map_or(request.target.as_str(), |(h, _)| h);
            *per_host.entry(host.to_string()).or_default() += 1;
        }
        busiest = busiest.max(per_host.values().copied().max().unwrap_or(0));
    }
    let _ = writeln!(
        out,
        "TCP connections in discovery: {tcp_total} (general probe started for {probed} domains), at most {busiest} to one host in one discovery; no AUTH/LOGIN/USER sent (the probe code has no such command)"
    );

    let scoring_entries = scoring.entries();
    let discovery_total: usize = rows.iter().map(|r| r.discovery.requests.len()).sum();
    let address_requests = rows
        .iter()
        .flat_map(|r| r.discovery.requests.iter())
        .filter(|r| r.carries_address)
        .count();
    let mut violations: Vec<String> = rows
        .iter()
        .flat_map(|r| privacy_violations(&r.discovery.domain, &r.discovery.requests))
        .collect();
    violations.extend(privacy_violations("-", &scoring_entries));
    let guarded = rows
        .iter()
        .flat_map(|r| r.discovery.requests.iter())
        .filter(|r| r.kind == "redirect-refused" && r.carries_address)
        .count();
    let _ = writeln!(
        out,
        "privacy: {} requests logged ({discovery_total} in discovery, {} in scoring checks); {address_requests} carried the dummy address, all to autoconfig.<domain>: {}; violations: {}; refused redirects that would have carried it elsewhere: {guarded}",
        discovery_total + scoring_entries.len(),
        scoring_entries.len(),
        violations.is_empty(),
        if violations.is_empty() {
            "none".to_string()
        } else {
            violations.join(", ")
        }
    );
    let redirects: Vec<String> = rows
        .iter()
        .flat_map(|r| {
            r.discovery
                .requests
                .iter()
                .filter(|q| q.kind.starts_with("redirect"))
                .map(move |q| format!("{} {} {}", r.discovery.domain, q.kind, q.target))
        })
        .collect();
    let _ = writeln!(
        out,
        "redirects: {}",
        if redirects.is_empty() {
            "none".to_string()
        } else {
            redirects.join("; ")
        }
    );

    if options.exhaustive {
        let _ = writeln!(
            out,
            "per-source (each source alone, strict rubric; latency = completion time since start):"
        );
        for source in Source::ALL {
            let mut answered = 0;
            let mut ok = Vec::new();
            let mut wrong = Vec::new();
            let mut latencies = Vec::new();
            for row in rows {
                let Some(report) = row.discovery.report(source) else {
                    continue;
                };
                latencies.push(report.elapsed_ms);
                if matches!(
                    report.result,
                    Ok(Finding::Servers { .. } | Finding::Unsupported { .. } | Finding::NoMail(_))
                ) {
                    answered += 1;
                }
                if let (Some(truth), Some(outcome)) =
                    (row.entry.truth, source_alone(&row.discovery, source))
                    && !matches!(
                        outcome,
                        Outcome::Settings {
                            imap: None,
                            smtp: None,
                            ..
                        }
                    )
                {
                    if judge(truth, &outcome).is_correct() {
                        ok.push(row.entry.domain);
                    } else {
                        wrong.push(row.entry.domain);
                    }
                }
            }
            let _ = writeln!(
                out,
                "  {:13} answered={answered:2} correct-alone={:2} p50={} p95={} max={} ms; wrong for: [{}]",
                source.name(),
                ok.len(),
                fmt_ms(percentile(&latencies, 50.0)),
                fmt_ms(percentile(&latencies, 95.0)),
                fmt_ms(percentile(&latencies, 100.0)),
                wrong.join(" "),
            );
        }
    }

    for (name, pick) in [("wrong (strict)", 0usize), ("wrong (extended)", 1usize)] {
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|r| {
                let verdict = if pick == 0 {
                    r.strict.as_ref()
                } else {
                    r.extended.as_ref().map(|(v, _)| v)
                };
                match verdict {
                    Some(Verdict::Wrong(why)) => Some(format!("{}: {why}", r.entry.domain)),
                    _ => None,
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "{name}: {}",
            if wrong.is_empty() {
                "none".into()
            } else {
                wrong.join("; ")
            }
        );
    }
    out
}

fn accept_text(list: &[truth::Accept]) -> String {
    list.iter()
        .map(|a| {
            let users: Vec<&str> = a
                .users
                .iter()
                .map(|u| match u {
                    truth::User::Address => "addr",
                    truth::User::LocalPart => "local",
                    truth::User::Other => "other",
                })
                .collect();
            format!("{}:{} {} ({})", a.host, a.port, a.security, users.join("/"))
        })
        .collect::<Vec<_>>()
        .join("<br>")
}

fn truth_table(entries: &[Entry]) -> String {
    let mut out = String::from(
        "| # | Domain | Group | IMAP accepted | SMTP accepted | Evidence |\n|---|---|---|---|---|---|\n",
    );
    for (n, entry) in entries.iter().enumerate() {
        let (imap, smtp) = match entry.truth.map(|t| t.expect) {
            Some(Expect::Settings { imap, smtp }) => (accept_text(imap), accept_text(smtp)),
            Some(Expect::Unsupported) => ("not supported".into(), "-".into()),
            Some(Expect::NoMail) => ("no mail service".into(), "-".into()),
            None => ("truth unknown (not scored)".into(), "-".into()),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {imap} | {smtp} | {} |",
            n + 1,
            entry.domain,
            entry.group,
            entry.evidence.replace('|', "\\|")
        );
    }
    out
}

fn tsv_rows(path: &PathBuf) -> Result<Vec<BTreeMap<String, String>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().ok_or("empty file")?.split('\t').collect();
    Ok(lines
        .map(|line| {
            header
                .iter()
                .zip(line.split('\t'))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect())
}

fn cell(row: &BTreeMap<String, String>, key: &str) -> String {
    row.get(key).cloned().unwrap_or_default()
}

fn short_user(text: &str) -> String {
    text.replace(" user=address then local-part", " (addr→local)")
        .replace(" user=local-part then address", " (local→addr)")
        .replace(" user=address", " (addr)")
        .replace(" user=local-part", " (local)")
        .replace(" user=other(", " (other: ")
}

fn table(args: &Args) -> Result<(), String> {
    let label = args.label.clone().ok_or("table needs --label")?;
    let rows = tsv_rows(&results_dir().join(format!("{label}.tsv")))?;
    let s4 = if args.list == List::S4 {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../s4-autodetect/results/run-a-presets.tsv");
        tsv_rows(&path)?
    } else {
        Vec::new()
    };
    let mut out = String::new();
    if args.list == List::S4 {
        out.push_str("| # | Domain | Group | S4 run A (won by) | S4b result (IMAP / SMTP, username plan) | Won by | Strict | Extended | Rule | Final ms |\n|---|---|---|---|---|---|---|---|---|---|\n");
    } else {
        out.push_str("| # | Domain | Group | S4b result (IMAP / SMTP, username plan) | Won by | Strict | Extended | Rule | Final ms |\n|---|---|---|---|---|---|---|---|---|\n");
    }
    for (n, row) in rows.iter().enumerate() {
        let domain = cell(row, "domain");
        let outcome = cell(row, "outcome");
        let result = if outcome.starts_with("settings") {
            format!(
                "{}<br>{}",
                short_user(&cell(row, "imap")),
                short_user(&cell(row, "smtp"))
            )
        } else {
            outcome.split(" provider=").next().unwrap_or("").to_string()
        };
        let mark = |verdict: String, why: String| match verdict.as_str() {
            "yes" => "yes".to_string(),
            "?" => "not scored".to_string(),
            _ => format!("**no**: {why}"),
        };
        let strict = mark(cell(row, "strict"), cell(row, "strict_why"));
        let extended = match cell(row, "extended").as_str() {
            "yes" => format!("yes ({})", cell(row, "extended_via")),
            _ => mark(cell(row, "extended"), cell(row, "extended_why")),
        };
        let rule = cell(row, "rules_fired");
        let final_ms = cell(row, "final_ms")
            .parse::<f64>()
            .map_or_else(|_| "-".into(), |v| format!("{v:.0}"));
        if args.list == List::S4 {
            let before = s4.iter().find(|r| cell(r, "domain") == domain).map_or_else(
                || "-".to_string(),
                |r| {
                    let correct = if cell(r, "correct") == "yes" {
                        "yes"
                    } else {
                        "**no**"
                    };
                    format!("{correct} ({})", cell(r, "winner"))
                },
            );
            let _ = writeln!(
                out,
                "| {} | {domain} | {} | {before} | {result} | {} | {strict} | {extended} | {rule} | {final_ms} |",
                n + 1,
                cell(row, "group"),
                cell(row, "winner"),
            );
        } else {
            let _ = writeln!(
                out,
                "| {} | {domain} | {} | {result} | {} | {strict} | {extended} | {rule} | {final_ms} |",
                n + 1,
                cell(row, "group"),
                cell(row, "winner"),
            );
        }
    }
    print!("{out}");
    Ok(())
}

async fn verify(args: Args) -> Result<(), String> {
    let tls = probe::tls_config()?;
    let resolver = fetch::resolver(Duration::from_secs(2), args.options.dns_server)?;
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for entry in entries(args.list)? {
        if let Some(Expect::Settings { imap, smtp }) = entry.truth.map(|t| t.expect) {
            for (protocol, list) in [(Protocol::Imap, imap), (Protocol::Smtp, smtp)] {
                for accept in list {
                    let target = Target {
                        protocol,
                        host: accept.host.to_string(),
                        port: accept.port,
                        security: accept.security,
                    };
                    if seen.insert(target.clone()) {
                        targets.push((entry.domain, target));
                    }
                }
            }
        }
    }
    let mut tsv = String::from(
        "domain\tprotocol\thost\tport\tsecurity\tok\tstage\telapsed_ms\ttls\tauth\tbanner_or_error\n",
    );
    let mut ok = 0;
    for (domain, target) in &targets {
        let result = probe::probe(
            target.clone(),
            tls.clone(),
            Duration::from_secs(8),
            &resolver,
            None,
        )
        .await;
        let (state, stage, tls_version, auth, text) = match &result.result {
            Ok(greeting) => {
                ok += 1;
                (
                    "ok",
                    "-".to_string(),
                    greeting.tls_version.clone(),
                    probe::auth_summary(target.protocol, &greeting.capabilities),
                    greeting.banner.clone(),
                )
            }
            Err((stage, error)) => (
                "FAIL",
                format!("{stage:?}"),
                "-".into(),
                "-".into(),
                error.clone(),
            ),
        };
        println!(
            "{state:4} {:5.0} ms {target} tls={tls_version} auth={auth} {text}",
            result.elapsed_ms
        );
        let _ = writeln!(
            tsv,
            "{domain}\t{:?}\t{}\t{}\t{}\t{state}\t{stage}\t{:.0}\t{tls_version}\t{auth}\t{}",
            target.protocol,
            target.host,
            target.port,
            target.security,
            result.elapsed_ms,
            text.replace('\t', " ")
        );
    }
    println!("verified {ok}/{} endpoints", targets.len());
    let label = args.label.unwrap_or_else(|| "verify".into());
    let path = write_result(&format!("{label}.tsv"), &tsv)?;
    println!("wrote {}", path.display());
    Ok(())
}

async fn show(args: Args) -> Result<(), String> {
    let tls = probe::tls_config()?;
    let domain = args.domains.first().ok_or(USAGE)?;
    let discovery = pipeline::discover(domain, &args.options, tls).await?;
    for report in &discovery.reports {
        println!(
            "{:13} {:6.0} ms  {}",
            report.source.name(),
            report.elapsed_ms,
            report.detail
        );
        if let Ok(Finding::Servers { servers, hints }) = &report.result {
            for server in servers {
                println!("                {:?} {server}", server.protocol);
            }
            println!("                hints {hints:?}");
        }
    }
    for probe in &discovery.probes {
        let state = match &probe.result {
            Ok(g) => format!(
                "ok {} auth={} {}",
                g.tls_version,
                probe::auth_summary(probe.target.protocol, &g.capabilities),
                g.banner
            ),
            Err((stage, e)) => format!("{stage:?}: {e}"),
        };
        println!("probe {:5.0} ms {} {state}", probe.elapsed_ms, probe.target);
    }
    for request in &discovery.requests {
        println!(
            "request {} {} address={}",
            request.kind, request.target, request.carries_address
        );
    }
    let (kind, imap, smtp) = outcome_text(&discovery.outcome);
    println!(
        "outcome: {kind} | {imap} | {smtp} | winner={} first={} final={:.0} settled={} rules={:?}",
        discovery.outcome.winners(),
        fmt_ms(discovery.first_usable_ms),
        discovery.final_ms,
        discovery.settled,
        rules_fired(&discovery)
    );
    let truth = match args.list {
        List::S4 => truth::find(domain),
        List::Heldout => entries(args.list)?
            .into_iter()
            .find(|e| e.domain == domain.as_str())
            .and_then(|e| e.truth),
    };
    if let Some(truth) = truth {
        println!(
            "strict verdict: {:?} (truth from {})",
            judge(truth, &discovery.outcome),
            truth.evidence
        );
    }
    Ok(())
}
