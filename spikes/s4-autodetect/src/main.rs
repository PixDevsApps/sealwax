//! Spike S4 account autodetection accuracy (M0). Measurement code only; production crates never depend on it.

mod clientconfig;
mod dnsdata;
mod fetch;
mod model;
mod pipeline;
mod presets;
mod probe;
mod rank;
mod replay;
mod score;
mod truth;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use model::{Choice, Finding, Outcome, Protocol, Source};
use pipeline::{Discovery, Options};
use rank::{Wait, decide};
use score::{Verdict, judge, percentile, tls_only};
use truth::{Expect, TRUTH};

const USAGE: &str = "usage: email-spike-s4 run [--no-presets] [--exhaustive] [--probe-delay-ms N] [--dns IP] [--order default|tuned] [--well-known-ms N] [--local-presets] [--probe-wait-all] [--label NAME] [domain...]
       email-spike-s4 verify [--label NAME] [imap|smtp:host:port:tls|starttls ...]
       email-spike-s4 show [--no-presets] [--exhaustive] <domain>
       email-spike-s4 truth";

struct Args {
    command: String,
    options: Options,
    label: Option<String>,
    domains: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut raw = std::env::args().skip(1);
    let command = raw.next().ok_or(USAGE)?;
    let mut options = Options::default();
    let mut label = None;
    let mut domains = Vec::new();
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--no-presets" => options.presets = false,
            "--local-presets" => options.local_presets = true,
            "--probe-wait-all" => options.probe_wait_all = true,
            "--exhaustive" => options.exhaustive = true,
            "--probe-delay-ms" => {
                let value = raw.next().ok_or("--probe-delay-ms needs a value")?;
                options.probe_delay =
                    Duration::from_millis(value.parse().map_err(|_| "bad --probe-delay-ms")?);
            }
            "--label" => label = Some(raw.next().ok_or("--label needs a value")?),
            "--order" => match raw.next().as_deref() {
                Some("tuned") => options.order = pipeline::TUNED_ORDER.to_vec(),
                Some("default") => options.order = Source::ALL.to_vec(),
                _ => return Err("--order needs tuned or default".into()),
            },
            "--well-known-ms" => {
                let value = raw.next().ok_or("--well-known-ms needs a value")?;
                options.well_known_timeout =
                    Duration::from_millis(value.parse().map_err(|_| "bad --well-known-ms")?);
            }
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
        "truth" => {
            print!("{}", truth_table());
            Ok(())
        }
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

fn expect_text(expect: &Expect) -> &'static str {
    match expect {
        Expect::Settings { .. } => "settings",
        Expect::Unsupported => "unsupported",
        Expect::NoMail => "no-mail",
    }
}

fn source_status(discovery: &Discovery, source: Source) -> String {
    match discovery.report(source) {
        None if source == Source::Probe && discovery.probe_started_ms.is_none() => {
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

fn privacy_violations(discovery: &Discovery) -> Vec<String> {
    let allowed = format!("autoconfig.{}/", discovery.domain);
    let inside = |target: &str| {
        target
            .strip_prefix("https://")
            .is_some_and(|host| fetch::within(host, &discovery.domain))
    };
    discovery
        .requests
        .iter()
        .filter(|r| r.carries_address)
        .filter(|r| match r.kind {
            "https" => !r.target.starts_with(&allowed),
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

async fn run(args: Args) -> Result<(), String> {
    let tls = probe::tls_config()?;
    let domains: Vec<String> = if args.domains.is_empty() {
        TRUTH.iter().map(|t| t.domain.to_string()).collect()
    } else {
        args.domains.clone()
    };
    let label = args.label.clone().unwrap_or_else(|| "run".into());
    let started = std::time::SystemTime::now();
    let mut tsv = String::from(
        "domain\tgroup\texpect\toutcome\timap\tsmtp\twinner\tcorrect\twhy\tfirst_usable_ms\tfinal_ms\tsettled\tsetup_ms\tprobe_started_ms\t",
    );
    for source in Source::ALL {
        let _ = write!(tsv, "{source}\t");
    }
    tsv.push_str("requests\ttcp_connections\tevidence\tdetails\n");

    let mut rows: Vec<(Discovery, Option<Verdict>, &'static str)> = Vec::new();
    for domain in &domains {
        let discovery = pipeline::discover(domain, &args.options, tls.clone()).await?;
        let truth = truth::find(domain);
        let verdict = truth.map(|t| judge(t, &discovery.outcome));
        let group = truth.map_or("-", |t| t.group);
        let (kind, imap, smtp) = outcome_text(&discovery.outcome);
        let correct = match &verdict {
            Some(Verdict::Correct) => "yes".to_string(),
            Some(Verdict::Wrong(_)) => "NO".to_string(),
            None => "?".to_string(),
        };
        let why = match &verdict {
            Some(Verdict::Wrong(why)) => why.clone(),
            _ => String::new(),
        };
        println!(
            "{domain:28} {correct:3} first={:>6} final={:>6} winner={:18} {kind} | {imap} | {smtp} {why}",
            fmt_ms(discovery.first_usable_ms),
            format!("{:.0}", discovery.final_ms),
            discovery.outcome.winners(),
        );
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
        let _ = write!(
            tsv,
            "{domain}\t{group}\t{}\t{kind}\t{imap}\t{smtp}\t{}\t{correct}\t{why}\t{}\t{:.1}\t{}\t{:.1}\t{}\t",
            truth.map_or("?", |t| expect_text(&t.expect)),
            discovery.outcome.winners(),
            fmt_ms(discovery.first_usable_ms),
            discovery.final_ms,
            discovery.settled,
            discovery.setup_ms,
            fmt_ms(discovery.probe_started_ms),
        );
        for source in Source::ALL {
            let mut status = source_status(&discovery, source);
            if let (Some(truth), Some(outcome)) = (truth, source_alone(&discovery, source))
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
        let _ = writeln!(
            tsv,
            "{}\t{tcp}\t{}\t{}",
            discovery.requests.len(),
            truth.map_or("-", |t| t.evidence),
            details.join(" || ").replace(['\t', '\n'], " ")
        );
        rows.push((discovery, verdict, group));
    }

    let summary = summarize(&rows, &args.options, &label, started);
    print!("{summary}");
    let tsv_path = write_result(&format!("{label}.tsv"), &tsv)?;
    let summary_path = write_result(&format!("{label}.summary.txt"), &summary)?;
    println!(
        "wrote {} and {}",
        tsv_path.display(),
        summary_path.display()
    );
    Ok(())
}

fn summarize(
    rows: &[(Discovery, Option<Verdict>, &'static str)],
    options: &Options,
    label: &str,
    started: std::time::SystemTime,
) -> String {
    let mut out = String::new();
    let since_epoch = started
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(
        out,
        "== {label}: {} domains, started at unix {since_epoch}",
        rows.len()
    );
    let _ = writeln!(
        out,
        "options: presets={} local_presets={} exhaustive={} resolver={} order={} probe_delay={}ms probe_wait_all={} cap={}ms http={}ms dns={}ms source={}ms well_known={}ms probe_connect={}ms probe_budget={}ms",
        options.presets,
        options.local_presets,
        options.exhaustive,
        options
            .dns_server
            .map_or_else(|| "system".to_string(), |ip| ip.to_string()),
        options
            .order
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(">"),
        options.probe_delay.as_millis(),
        options.probe_wait_all,
        options.cap.as_millis(),
        options.http_timeout.as_millis(),
        options.dns_timeout.as_millis(),
        options.source_timeout.as_millis(),
        options.well_known_timeout.as_millis(),
        options.probe_connect_timeout.as_millis(),
        options.probe_budget.as_millis(),
    );
    let judged: Vec<_> = rows.iter().filter(|(_, v, _)| v.is_some()).collect();
    let correct = judged
        .iter()
        .filter(|(_, v, _)| v.as_ref().is_some_and(Verdict::is_correct))
        .count();
    let tls = rows.iter().filter(|(d, _, _)| tls_only(&d.outcome)).count();
    let _ = writeln!(out, "correct: {correct}/{}", judged.len());
    let _ = writeln!(out, "TLS-only final results: {tls}/{}", rows.len());

    let mut groups: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (_, verdict, group) in rows {
        let entry = groups.entry(group).or_default();
        entry.1 += 1;
        if verdict.as_ref().is_some_and(Verdict::is_correct) {
            entry.0 += 1;
        }
    }
    for (group, (ok, total)) in &groups {
        let _ = writeln!(out, "  group {group}: {ok}/{total}");
    }

    let finals: Vec<f64> = rows.iter().map(|(d, _, _)| d.final_ms).collect();
    let firsts: Vec<f64> = rows
        .iter()
        .filter_map(|(d, _, _)| d.first_usable_ms)
        .collect();
    let line = |name: &str, values: &[f64]| {
        format!(
            "{name}: n={} p50={} p95={} max={} ms\n",
            values.len(),
            fmt_ms(percentile(values, 50.0)),
            fmt_ms(percentile(values, 95.0)),
            fmt_ms(percentile(values, 100.0)),
        )
    };
    out.push_str(&line("final ranked result", &finals));
    out.push_str(&line("first usable result", &firsts));
    for group in groups.keys() {
        let values: Vec<f64> = rows
            .iter()
            .filter(|(_, _, g)| g == group)
            .map(|(d, _, _)| d.final_ms)
            .collect();
        out.push_str(&line(&format!("  final, group {group}"), &values));
    }
    let unsettled = rows.iter().filter(|(d, _, _)| !d.settled).count();
    let _ = writeln!(
        out,
        "hit the {} ms cap or ended unsettled: {unsettled}",
        options.cap.as_millis()
    );

    let mut winners: BTreeMap<String, usize> = BTreeMap::new();
    for (discovery, _, _) in rows {
        *winners.entry(discovery.outcome.winners()).or_default() += 1;
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

    let probed = rows
        .iter()
        .filter(|(d, _, _)| d.probe_started_ms.is_some())
        .count();
    let tcp_total: usize = rows
        .iter()
        .map(|(d, _, _)| d.requests.iter().filter(|r| r.kind == "tcp").count())
        .sum();
    let mut busiest = 0;
    for (discovery, _, _) in rows {
        let mut per_host: BTreeMap<String, usize> = BTreeMap::new();
        for request in discovery.requests.iter().filter(|r| r.kind == "tcp") {
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
        "probing ran for {probed} domains: {tcp_total} TCP connections in total, at most {busiest} to one host in one discovery; no AUTH/LOGIN/USER sent (the probe code has no such command)"
    );

    let address_requests: Vec<String> = rows
        .iter()
        .flat_map(|(d, _, _)| {
            d.requests
                .iter()
                .filter(|r| r.carries_address)
                .map(|r| r.target.clone())
        })
        .collect();
    let violations: Vec<String> = rows
        .iter()
        .flat_map(|(d, _, _)| privacy_violations(d))
        .collect();
    let redirects: Vec<String> = rows
        .iter()
        .flat_map(|(d, _, _)| {
            d.requests
                .iter()
                .filter(|r| r.kind.starts_with("redirect"))
                .map(move |r| format!("{} {} {}", d.domain, r.kind, r.target))
        })
        .collect();
    let requests_total: usize = rows.iter().map(|(d, _, _)| d.requests.len()).sum();
    let guarded = rows
        .iter()
        .flat_map(|(d, _, _)| d.requests.iter())
        .filter(|r| r.kind == "redirect-refused" && r.carries_address)
        .count();
    let _ = writeln!(
        out,
        "redirects refused that would have carried the dummy address elsewhere: {guarded}"
    );
    let _ = writeln!(
        out,
        "privacy: {requests_total} requests logged; {} carried the dummy address, all to autoconfig.<domain>: {}; violations: {}",
        address_requests.len(),
        violations.is_empty(),
        if violations.is_empty() {
            "none".to_string()
        } else {
            violations.join(", ")
        }
    );
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
            "per-source (each source alone; latency = completion time since start):"
        );
        let alone = |discovery: &Discovery, source: Source| -> Option<bool> {
            let truth = truth::find(&discovery.domain)?;
            let outcome = source_alone(discovery, source)?;
            if matches!(
                outcome,
                Outcome::Settings {
                    imap: None,
                    smtp: None,
                    ..
                }
            ) {
                return None;
            }
            Some(judge(truth, &outcome).is_correct())
        };
        for source in Source::ALL {
            let mut alone_ok = 0;
            let mut unique = Vec::new();
            let mut wrong = Vec::new();
            let mut answered = 0;
            let mut latencies = Vec::new();
            let mut errors = 0;
            for (discovery, _, _) in rows {
                let Some(report) = discovery.report(source) else {
                    continue;
                };
                latencies.push(report.elapsed_ms);
                match &report.result {
                    Err(_) => errors += 1,
                    Ok(Finding::Nothing) => {}
                    Ok(_) => answered += 1,
                }
                match alone(discovery, source) {
                    Some(true) => {
                        alone_ok += 1;
                        let others = Source::ALL
                            .iter()
                            .filter(|&&s| s != source)
                            .any(|&s| alone(discovery, s) == Some(true));
                        if !others {
                            unique.push(discovery.domain.clone());
                        }
                    }
                    Some(false) => wrong.push(discovery.domain.clone()),
                    None => {}
                }
            }
            let _ = writeln!(
                out,
                "  {source:11} answered={answered:2} correct-alone={alone_ok:2} errors={errors:2} p50={} p95={} max={} ms; only source right for: [{}]; answered wrongly for: [{}]",
                fmt_ms(percentile(&latencies, 50.0)),
                fmt_ms(percentile(&latencies, 95.0)),
                fmt_ms(percentile(&latencies, 100.0)),
                unique.join(" "),
                wrong.join(" "),
            );
        }
    }

    if options.exhaustive {
        let _ = writeln!(
            out,
            "replayed policies (from this run's per-source completion times; probe start = min({} ms, rest done) + measured probe duration):",
            options.probe_delay.as_millis()
        );
        for policy in replay::POLICIES {
            let mut correct = 0;
            let mut finals = Vec::new();
            let mut firsts = Vec::new();
            let mut tls = 0;
            let mut wrong = Vec::new();
            for (discovery, _, _) in rows {
                let replayed = replay::replay(
                    &discovery.reports,
                    discovery.probe_started_ms,
                    policy,
                    options.probe_delay.as_secs_f64() * 1000.0,
                    options.cap.as_secs_f64() * 1000.0,
                );
                finals.push(replayed.final_ms);
                firsts.extend(replayed.first_usable_ms);
                if tls_only(&replayed.outcome) {
                    tls += 1;
                }
                match truth::find(&discovery.domain).map(|t| judge(t, &replayed.outcome)) {
                    Some(Verdict::Correct) => correct += 1,
                    Some(Verdict::Wrong(_)) => wrong.push(discovery.domain.clone()),
                    None => {}
                }
            }
            let _ = writeln!(
                out,
                "  {:40} correct={correct}/{} tls-only={tls} final p50={} p95={} max={} first p50={} p95={} ms; wrong: {}",
                policy.name,
                rows.len(),
                fmt_ms(percentile(&finals, 50.0)),
                fmt_ms(percentile(&finals, 95.0)),
                fmt_ms(percentile(&finals, 100.0)),
                fmt_ms(percentile(&firsts, 50.0)),
                fmt_ms(percentile(&firsts, 95.0)),
                wrong.join(" "),
            );
        }
    }

    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(d, v, _)| match v {
            Some(Verdict::Wrong(why)) => Some(format!("{}: {why}", d.domain)),
            _ => None,
        })
        .collect();
    let _ = writeln!(
        out,
        "wrong: {}",
        if wrong.is_empty() {
            "none".into()
        } else {
            wrong.join("; ")
        }
    );
    out
}

fn parse_target(text: &str) -> Result<probe::Target, String> {
    let bad = || format!("bad target {text:?}, expected imap|smtp:host:port:tls|starttls");
    let parts: Vec<&str> = text.split(':').collect();
    let [protocol, host, port, security] = parts[..] else {
        return Err(bad());
    };
    Ok(probe::Target {
        protocol: match protocol {
            "imap" => Protocol::Imap,
            "smtp" => Protocol::Smtp,
            _ => return Err(bad()),
        },
        host: host.to_ascii_lowercase(),
        port: port.parse().map_err(|_| bad())?,
        security: match security {
            "tls" => model::Security::Tls,
            "starttls" => model::Security::StartTls,
            _ => return Err(bad()),
        },
    })
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

fn truth_table() -> String {
    let mut out = String::from(
        "| # | Domain | Group | IMAP accepted | SMTP accepted | Evidence |\n|---|---|---|---|---|---|\n",
    );
    for (n, truth) in TRUTH.iter().enumerate() {
        let (imap, smtp) = match truth.expect {
            Expect::Settings { imap, smtp } => (accept_text(imap), accept_text(smtp)),
            Expect::Unsupported => ("not supported".into(), "-".into()),
            Expect::NoMail => ("no mail service".into(), "-".into()),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {imap} | {smtp} | {} |",
            n + 1,
            truth.domain,
            truth.group,
            truth.evidence
        );
    }
    out
}

async fn verify(args: Args) -> Result<(), String> {
    let tls = probe::tls_config()?;
    let resolver = fetch::resolver(Duration::from_secs(2), args.options.dns_server)?;
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for truth in TRUTH {
        if let Expect::Settings { imap, smtp } = truth.expect {
            for (protocol, list) in [(Protocol::Imap, imap), (Protocol::Smtp, smtp)] {
                for accept in list {
                    let target = probe::Target {
                        protocol,
                        host: accept.host.to_string(),
                        port: accept.port,
                        security: accept.security,
                    };
                    if seen.insert(target.clone()) {
                        targets.push(target);
                    }
                }
            }
        }
    }
    for extra in &args.domains {
        let target = parse_target(extra)?;
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
    let mut tsv = String::from(
        "protocol\thost\tport\tsecurity\tok\tstage\telapsed_ms\ttls\tauth\tbanner_or_error\n",
    );
    let mut ok = 0;
    for target in &targets {
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
            "{:?}\t{}\t{}\t{}\t{state}\t{stage}\t{:.0}\t{tls_version}\t{auth}\t{}",
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
    let tls: Arc<rustls::ClientConfig> = probe::tls_config()?;
    let domain = args.domains.first().ok_or(USAGE)?;
    let discovery = pipeline::discover(domain, &args.options, tls).await?;
    for report in &discovery.reports {
        println!(
            "{:11} {:6.0} ms  {}",
            report.source.name(),
            report.elapsed_ms,
            report.detail
        );
        if let Ok(Finding::Servers { servers, hints }) = &report.result {
            for server in servers {
                println!("              {:?} {server}", server.protocol);
            }
            println!("              hints {hints:?}");
        }
    }
    for probe in &discovery.probes {
        let state = match &probe.result {
            Ok(g) => format!(
                "ok {} auth={}",
                g.tls_version,
                probe::auth_summary(probe.target.protocol, &g.capabilities)
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
        "outcome: {kind} | {imap} | {smtp} | winner={} first={} final={:.0} settled={}",
        discovery.outcome.winners(),
        fmt_ms(discovery.first_usable_ms),
        discovery.final_ms,
        discovery.settled
    );
    if let Some(truth) = truth::find(domain) {
        println!(
            "verdict: {:?} (truth from {})",
            judge(truth, &discovery.outcome),
            truth.evidence
        );
    }
    Ok(())
}
