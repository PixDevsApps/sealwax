//! The pipeline against a scripted network, on paused tokio time so the timings are exact. Every
//! name is under `.test` (RFC 2606) or a provider's real host that a preset names.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use sealwax_domain::account::{
    AuthMethod, DetectSource, Detection, OAuthProvider, Protocol, Security, UnsupportedReason,
    UsernameForm,
};
use sealwax_domain::autoconfig::dns::{Mx, MxAnswer, SrvRecord, SrvService};
use sealwax_domain::autoconfig::probing::Target;
use sealwax_domain::autoconfig::{Rules, Source};

use super::{DetectOptions, Fetched, NetError, Network, ProbeError, Stage, Trace, detect_with};

const ADDRESS: &str = "test@example.test";
const STEP: Duration = Duration::from_millis(20);

/// Answers from a script; anything not scripted fails after `STEP`. Records every call.
#[derive(Default)]
struct Script {
    mx: Option<(Duration, MxAnswer)>,
    srv: HashMap<&'static str, Vec<SrvRecord>>,
    cname: HashMap<&'static str, &'static str>,
    addresses: HashMap<&'static str, Vec<IpAddr>>,
    /// Pages by URL without the query.
    pages: HashMap<&'static str, (Duration, &'static str)>,
    /// Targets that pass the TLS check, after the given time.
    open: HashMap<Target, Duration>,
    calls: Mutex<Vec<String>>,
}

impl Script {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn mx(mut self, exchange: &str) -> Self {
        self.mx = Some((
            STEP,
            MxAnswer::Records(vec![Mx {
                preference: 10,
                exchange: exchange.into(),
            }]),
        ));
        self
    }

    fn page(mut self, url: &'static str, body: &'static str) -> Self {
        self.pages.insert(url, (STEP, body));
        self
    }

    fn open(mut self, protocol: Protocol, host: &str, port: u16, security: Security) -> Self {
        self.open
            .insert(Target::new(protocol, host, port, security), STEP);
        self
    }

    fn srv(mut self, name: &'static str, port: u16, target: &str) -> Self {
        let service = SrvService::ALL
            .into_iter()
            .find(|s| name.starts_with(s.label()))
            .unwrap();
        self.srv.entry(name).or_default().push(SrvRecord {
            service,
            priority: 0,
            weight: 0,
            port,
            target: target.into(),
        });
        self
    }
}

impl Network for Script {
    async fn mx(&self, domain: &str) -> Result<MxAnswer, NetError> {
        self.record(format!("mx {domain}"));
        match &self.mx {
            Some((delay, answer)) => {
                tokio::time::sleep(*delay).await;
                Ok(answer.clone())
            }
            None => {
                tokio::time::sleep(STEP).await;
                Ok(MxAnswer::NoRecords)
            }
        }
    }

    async fn srv(&self, service: SrvService, domain: &str) -> Result<Vec<SrvRecord>, NetError> {
        let name = format!("{}.{domain}", service.label());
        self.record(format!("srv {name}"));
        tokio::time::sleep(STEP).await;
        Ok(self.srv.get(name.as_str()).cloned().unwrap_or_default())
    }

    async fn cname(&self, name: &str) -> Result<Option<String>, NetError> {
        self.record(format!("cname {name}"));
        tokio::time::sleep(STEP).await;
        Ok(self.cname.get(name).map(|t| t.to_string()))
    }

    async fn addresses(&self, host: &str) -> Result<Vec<IpAddr>, NetError> {
        self.record(format!("addresses {host}"));
        tokio::time::sleep(STEP).await;
        self.addresses
            .get(host)
            .cloned()
            .ok_or_else(|| NetError("no addresses".into()))
    }

    async fn fetch(&self, url: &str) -> Result<Fetched, NetError> {
        self.record(format!("fetch {url}"));
        let page = url.split_once('?').map_or(url, |(page, _)| page);
        match self.pages.get(page) {
            Some((delay, body)) => {
                tokio::time::sleep(*delay).await;
                Ok(Fetched::Body(body.to_string()))
            }
            None => {
                tokio::time::sleep(STEP).await;
                Ok(Fetched::Status(404))
            }
        }
    }

    async fn probe(&self, target: &Target, limit: Duration) -> Result<(), ProbeError> {
        self.record(format!(
            "probe {:?} {}:{}",
            target.protocol, target.host, target.port
        ));
        match self.open.get(target) {
            Some(delay) if *delay <= limit => {
                tokio::time::sleep(*delay).await;
                Ok(())
            }
            Some(_) => {
                tokio::time::sleep(limit).await;
                Err(ProbeError::new(Stage::Timeout, "slow"))
            }
            None => {
                tokio::time::sleep(STEP).await;
                Err(ProbeError::new(Stage::Dns, "no addresses"))
            }
        }
    }
}

async fn detect(script: &Script) -> (Detection, Trace) {
    detect_with(script, ADDRESS, &DetectOptions::default(), |_| {}).await
}

fn found(detection: &Detection) -> &sealwax_domain::account::DetectedSettings {
    match detection {
        Detection::Found(settings) => settings,
        other => panic!("expected settings, got {other:?}"),
    }
}

const ISPDB_DOC: &str = r#"<clientConfig version="1.1"><emailProvider id="example.test">
    <displayName>Example Internet</displayName><displayShortName>Example</displayShortName>
    <incomingServer type="imap"><hostname>imap.isp.test</hostname><port>993</port>
      <socketType>SSL</socketType><username>%EMAILADDRESS%</username></incomingServer>
    <outgoingServer type="smtp"><hostname>smtp.isp.test</hostname><port>465</port>
      <socketType>SSL</socketType><username>%EMAILADDRESS%</username></outgoingServer>
    <enable visiturl="https://isp.test/imap"><instruction>Turn on IMAP</instruction></enable>
  </emailProvider></clientConfig>"#;

const PLAINTEXT_DOC: &str = r#"<clientConfig version="1.1"><emailProvider id="example.test">
    <incomingServer type="imap"><hostname>mail.example.test</hostname><port>143</port>
      <socketType>plain</socketType></incomingServer>
    <outgoingServer type="smtp"><hostname>mail.example.test</hostname><port>25</port>
      <socketType>plain</socketType></outgoingServer>
  </emailProvider></clientConfig>"#;

#[tokio::test(start_paused = true)]
async fn a_preset_answers_without_any_request() {
    let script = Script::default();
    let heard = Mutex::new(Vec::new());
    let (detection, trace) = detect_with(
        &script,
        "test@gmail.com",
        &DetectOptions::default(),
        |source| heard.lock().unwrap().push(source),
    )
    .await;
    let settings = found(&detection);
    assert_eq!(settings.imap.host, "imap.gmail.com");
    assert_eq!(settings.smtp.port, 465);
    assert_eq!(settings.provider.as_deref(), Some("Google"));
    assert_eq!(settings.source, DetectSource::Preset);
    assert!(script.calls().is_empty());
    assert_eq!(trace.elapsed, Duration::ZERO);
    assert_eq!(*heard.lock().unwrap(), [DetectSource::Preset]);
}

#[tokio::test(start_paused = true)]
async fn progress_lists_every_kind_of_source_once_in_order() {
    let script = Script::default();
    let heard = Mutex::new(Vec::new());
    detect_with(&script, ADDRESS, &DetectOptions::default(), |source| {
        heard.lock().unwrap().push(source);
    })
    .await;
    assert_eq!(
        *heard.lock().unwrap(),
        [
            DetectSource::Preset,
            DetectSource::Autoconfig,
            DetectSource::Ispdb,
            DetectSource::Mx,
            DetectSource::Srv,
            DetectSource::Probe,
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn the_address_goes_only_to_the_domains_own_autoconfig() {
    let script = Script::default().mx("mx.host.test");
    let (detection, trace) = detect(&script).await;
    assert_eq!(detection, Detection::NotFound);
    assert!(trace.settled);
    let calls = script.calls();
    let carrying: Vec<&str> = calls
        .iter()
        .map(String::as_str)
        .filter(|c| c.contains('@') || c.contains("%40") || c.contains("test%40"))
        .collect();
    assert_eq!(
        carrying,
        [
            "fetch https://autoconfig.example.test/mail/config-v1.1.xml?emailaddress=test%40example.test"
        ]
    );
    assert!(calls.contains(&"fetch https://autoconfig.thunderbird.net/v1.1/example.test".into()));
    assert!(calls.contains(&"fetch https://autoconfig.host.test/mail/config-v1.1.xml".into()));
    assert!(calls.iter().all(|c| !c.starts_with("fetch http://")));
}

#[tokio::test(start_paused = true)]
async fn the_ispdb_outranks_srv_and_its_hints_reach_the_assistant() {
    let script = Script::default()
        .page(
            "https://autoconfig.thunderbird.net/v1.1/example.test",
            ISPDB_DOC,
        )
        .srv("_imaps._tcp.example.test", 993, "imap.example.test.")
        .srv("_submissions._tcp.example.test", 465, "smtp.example.test.")
        .open(Protocol::Imap, "imap.example.test", 993, Security::Tls)
        .open(Protocol::Smtp, "smtp.example.test", 465, Security::Tls);
    let (detection, _) = detect(&script).await;
    let settings = found(&detection);
    assert_eq!(settings.imap.host, "imap.isp.test");
    assert_eq!(settings.source, DetectSource::Ispdb);
    assert_eq!(settings.provider.as_deref(), Some("Example"));
    assert_eq!(
        settings.enable_imap.as_ref().map(|l| l.url.as_str()),
        Some("https://isp.test/imap")
    );
    assert_eq!(settings.second_username, Some(UsernameForm::LocalPart));
    assert!(!settings.confirm);

    let options = DetectOptions {
        rules: Rules {
            ispdb_before_srv: false,
            ..Rules::default()
        },
        ..DetectOptions::default()
    };
    let (detection, _) = detect_with(&script, ADDRESS, &options, |_| {}).await;
    assert_eq!(found(&detection).imap.host, "imap.example.test");
}

#[tokio::test(start_paused = true)]
async fn srv_endpoints_must_pass_the_tls_check() {
    let script = Script::default()
        .srv("_imaps._tcp.example.test", 993, "broken.example.test.")
        .srv("_imap._tcp.example.test", 143, "imap.example.test.")
        .srv("_submission._tcp.example.test", 465, "ssl0.host.test.")
        .open(Protocol::Imap, "imap.example.test", 143, Security::StartTls)
        .open(Protocol::Smtp, "ssl0.host.test", 465, Security::Tls);
    let (detection, trace) = detect(&script).await;
    let settings = found(&detection);
    assert_eq!(
        (settings.imap.host.as_str(), settings.imap.port),
        ("imap.example.test", 143)
    );
    assert_eq!(settings.imap.security, Security::StartTls);
    assert_eq!(
        (settings.smtp.host.as_str(), settings.smtp.security),
        ("ssl0.host.test", Security::Tls),
        "_submission on 465 is implicit TLS"
    );
    assert!(
        settings.confirm,
        "an SRV host outside the domain is confirmed"
    );
    let srv = trace
        .sources
        .iter()
        .find(|t| t.source == Source::Srv)
        .unwrap();
    assert!(
        srv.detail.contains("broken.example.test:993 failed"),
        "{}",
        srv.detail
    );
}

#[tokio::test(start_paused = true)]
async fn srv_targets_take_the_conventional_name_of_the_same_server() {
    let ip: IpAddr = "192.0.2.10".parse().unwrap();
    let mut script = Script::default()
        .srv("_imaps._tcp.example.test", 993, "mail.example.test.")
        .srv("_submission._tcp.example.test", 587, "mail.example.test.")
        .open(Protocol::Imap, "imap.example.test", 993, Security::Tls)
        .open(Protocol::Smtp, "mail.example.test", 587, Security::StartTls);
    for host in ["mail.example.test", "imap.example.test"] {
        script.addresses.insert(host, vec![ip]);
    }
    script
        .addresses
        .insert("smtp.example.test", vec!["192.0.2.11".parse().unwrap()]);
    let (detection, _) = detect(&script).await;
    let settings = found(&detection);
    assert_eq!(settings.imap.host, "imap.example.test");
    assert_eq!(
        settings.smtp.host, "mail.example.test",
        "different addresses"
    );
    assert!(!settings.confirm);
}

#[tokio::test(start_paused = true)]
async fn the_users_own_imap_host_outranks_a_microsoft_mx() {
    let script = Script::default()
        .mx("example-test.mail.protection.outlook.com")
        .open(Protocol::Imap, "imap.example.test", 993, Security::Tls)
        .open(Protocol::Smtp, "smtp.example.test", 465, Security::Tls);
    let (detection, trace) = detect(&script).await;
    let settings = found(&detection);
    assert_eq!(settings.imap.host, "imap.example.test");
    assert_eq!(settings.smtp.host, "smtp.example.test");
    assert_eq!(settings.auth, AuthMethod::Password);
    assert_eq!(trace.outcome.winners(), [Source::OwnHost, Source::OwnHost]);
}

#[tokio::test(start_paused = true)]
async fn microsoft_365_signs_in_with_oauth() {
    let script = Script::default().mx("example-test.mail.protection.outlook.com");
    let (detection, _) = detect(&script).await;
    let settings = found(&detection);
    assert_eq!(settings.imap.host, "outlook.office365.com");
    assert_eq!(settings.smtp.host, "smtp.office365.com");
    assert_eq!(settings.auth, AuthMethod::OAuth(OAuthProvider::Microsoft));
    assert_eq!(settings.source, DetectSource::Mx);
    assert_eq!(settings.provider.as_deref(), Some("Microsoft 365"));
}

#[tokio::test(start_paused = true)]
async fn autodiscover_finds_microsoft_365_behind_an_mx_gateway() {
    let mut script = Script::default().mx("mx.example.test");
    script
        .cname
        .insert("autodiscover.example.test", "autodiscover.outlook.com.");
    let (detection, _) = detect(&script).await;
    let settings = found(&detection);
    assert_eq!(settings.imap.host, "outlook.office365.com");
    assert_eq!(settings.auth, AuthMethod::OAuth(OAuthProvider::Microsoft));

    let off = DetectOptions {
        rules: Rules {
            autodiscover: false,
            ..Rules::default()
        },
        ..DetectOptions::default()
    };
    let (detection, _) = detect_with(&script, ADDRESS, &off, |_| {}).await;
    assert_eq!(detection, Detection::NotFound);
}

#[tokio::test(start_paused = true)]
async fn a_null_mx_means_no_mail_even_with_an_own_host() {
    let mut script = Script::default()
        .open(Protocol::Imap, "imap.example.test", 993, Security::Tls)
        .open(Protocol::Smtp, "smtp.example.test", 465, Security::Tls);
    script.mx = Some((STEP, MxAnswer::NullMx));
    let (detection, _) = detect(&script).await;
    assert_eq!(detection, Detection::NoMail);
}

#[tokio::test(start_paused = true)]
async fn an_unsupported_provider_behind_the_mx_ends_the_search() {
    let script = Script::default().mx("mail.protonmail.ch");
    let (detection, _) = detect(&script).await;
    assert_eq!(
        detection,
        Detection::Unsupported {
            provider: "Proton Mail".into(),
            reason: UnsupportedReason::BridgeOnly,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn the_mx_operators_hosts_are_shown_for_confirmation() {
    let script = Script::default()
        .mx("mx.host.test")
        .open(Protocol::Imap, "imap.host.test", 993, Security::Tls)
        .open(Protocol::Smtp, "smtp.host.test", 465, Security::Tls);
    let (detection, trace) = detect(&script).await;
    let settings = found(&detection);
    assert_eq!(settings.imap.host, "imap.host.test");
    assert!(settings.confirm);
    assert_eq!(settings.username, UsernameForm::Address);
    assert_eq!(settings.second_username, Some(UsernameForm::LocalPart));
    assert_eq!(settings.source, DetectSource::Probe);
    assert!(trace.settled);
    assert!(trace.probe_started.is_some());
}

#[tokio::test(start_paused = true)]
async fn an_mx_operator_with_only_imap_is_not_offered() {
    let script = Script::default().mx("mx.host.test").open(
        Protocol::Imap,
        "imap.host.test",
        993,
        Security::Tls,
    );
    let (detection, _) = detect(&script).await;
    assert_eq!(detection, Detection::NotFound);
}

#[tokio::test(start_paused = true)]
async fn plaintext_only_settings_are_never_offered() {
    let script = Script::default().page(
        "https://autoconfig.example.test/mail/config-v1.1.xml",
        PLAINTEXT_DOC,
    );
    let (detection, _) = detect(&script).await;
    assert_eq!(detection, Detection::NotFound);
}

#[tokio::test(start_paused = true)]
async fn probing_waits_for_the_other_sources_or_two_seconds() {
    let mut script = Script::default()
        .open(Protocol::Imap, "mail.example.test", 993, Security::Tls)
        .open(Protocol::Smtp, "mail.example.test", 465, Security::Tls);
    let (detection, trace) = detect(&script).await;
    assert_eq!(found(&detection).imap.host, "mail.example.test");
    let started = trace.probe_started.unwrap();
    assert!(started < Duration::from_millis(200), "{started:?}");

    script.pages.insert(
        "https://autoconfig.thunderbird.net/v1.1/example.test",
        (Duration::from_secs(60), ""),
    );
    let (detection, trace) = detect(&script).await;
    assert_eq!(found(&detection).imap.host, "mail.example.test");
    assert_eq!(trace.probe_started, Some(Duration::from_secs(2)));
    assert_eq!(
        trace.elapsed,
        Duration::from_millis(4500),
        "the probe's answer waits for the ISPDB's time limit"
    );
}

#[tokio::test(start_paused = true)]
async fn the_cap_takes_the_best_answer_so_far() {
    let mut script = Script::default()
        .open(Protocol::Imap, "imap.example.test", 993, Security::Tls)
        .open(Protocol::Smtp, "smtp.example.test", 465, Security::Tls);
    script.mx = Some((Duration::from_secs(60), MxAnswer::NoRecords));
    let options = DetectOptions {
        source_timeout: Duration::from_secs(30),
        ..DetectOptions::default()
    };
    let (detection, trace) = detect_with(&script, ADDRESS, &options, |_| {}).await;
    assert!(!trace.settled);
    assert_eq!(trace.elapsed, Duration::from_secs(10));
    assert_eq!(found(&detection).imap.host, "imap.example.test");
    assert!(trace.first_usable.unwrap() < Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn addresses_without_a_mail_domain_make_no_request() {
    let script = Script::default();
    for address in [
        "no-at-sign",
        "test@",
        "test@[192.0.2.1]",
        "test@localhost",
        "test@a/b.test",
    ] {
        let (detection, trace) =
            detect_with(&script, address, &DetectOptions::default(), |_| {}).await;
        assert_eq!(detection, Detection::NotFound, "{address}");
        assert_eq!(trace.domain, None, "{address}");
    }
    assert!(script.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn international_domains_are_looked_up_in_ascii() {
    let script = Script::default();
    let (_, trace) = detect_with(
        &script,
        "test@blåbær.test",
        &DetectOptions::default(),
        |_| {},
    )
    .await;
    let domain = trace.domain.unwrap();
    assert!(domain.starts_with("xn--"), "{domain}");
    assert!(
        script
            .calls()
            .iter()
            .all(|c| c.is_ascii() || c.starts_with("fetch https://autoconfig.xn--")),
        "only the address in the autoconfig query may stay non-ASCII"
    );
}

#[tokio::test(start_paused = true)]
async fn each_endpoint_is_probed_at_most_once() {
    let script = Script::default().mx("mx.host.test");
    detect(&script).await;
    let probes: Vec<String> = script
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("probe "))
        .collect();
    let mut unique = probes.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(probes.len(), unique.len(), "{probes:?}");
}

/// The real network, with a dummy address at a domain without a preset: run with
/// `cargo test -p sealwax-engine -- --ignored live_`. It sends the dummy address to that domain's
/// autoconfig and the domain to the ISPDB and DNS.
#[test]
#[ignore = "network"]
fn live_detects_a_domain_with_its_own_autoconfig() {
    let runtime = crate::runtime().unwrap();
    let detection = runtime.block_on(super::detect(
        "test@mailbox.org",
        &DetectOptions::default(),
        |_| {},
    ));
    let settings = found(&detection);
    assert!(settings.imap.host.ends_with("mailbox.org"), "{settings:?}");
}
