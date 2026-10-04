use crate::model::{Choice, Finding, Hints, Outcome, Protocol, Report, Security, Server, Source};

pub fn best_index(servers: &[Server], protocol: Protocol) -> Option<usize> {
    servers
        .iter()
        .enumerate()
        .filter(|(_, s)| s.protocol == protocol && s.security.is_tls())
        .min_by_key(|(_, s)| match s.security {
            Security::Tls => 0,
            Security::StartTls => 1,
            Security::Plain => 2,
        })
        .map(|(index, _)| index)
}

pub fn best(servers: &[Server], protocol: Protocol) -> Option<&Server> {
    best_index(servers, protocol).map(|index| &servers[index])
}

pub fn plaintext_only(servers: &[Server], protocol: Protocol) -> bool {
    let mut of_protocol = servers.iter().filter(|s| s.protocol == protocol).peekable();
    of_protocol.peek().is_some() && of_protocol.all(|s| !s.security.is_tls())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    ForHigherPriority,
    No,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub outcome: Outcome,
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MxSays {
    Pending,
    NoMailOrUnsupported,
    Other,
}

fn mx_says(reports: &[Report]) -> MxSays {
    match reports.iter().find(|r| r.source == Source::MxPreset) {
        None => MxSays::Pending,
        Some(report) => match report.finding() {
            Some(Finding::NoMail(_) | Finding::Unsupported { .. }) => MxSays::NoMailOrUnsupported,
            _ => MxSays::Other,
        },
    }
}

/// Picks, per protocol, the highest-priority source that offers a TLS server.
///
/// `OwnHost` (a protocol-named host under the user's own domain that passed the TLS
/// check) outranks everything inferred from the MX records, but an MX answer of "no
/// mail" or "not supported" still ends the search, so `OwnHost` waits for the MX answer.
pub fn decide(sources: &[Source], reports: &[Report], wait: Wait) -> Decision {
    let mut imap: Option<Box<Choice>> = None;
    let mut smtp: Option<Box<Choice>> = None;
    let mut hints: Option<Hints> = None;
    let mut settled = true;

    for &source in sources {
        if source == Source::OwnHost {
            let own = reports.iter().find(|r| r.source == Source::OwnHost);
            match mx_says(reports) {
                MxSays::NoMailOrUnsupported => continue,
                MxSays::Pending
                    if wait == Wait::ForHigherPriority
                        && own.is_some_and(|r| {
                            matches!(r.finding(), Some(Finding::Servers { .. }))
                        }) =>
                {
                    settled = false;
                    break;
                }
                _ => {}
            }
        }
        let Some(report) = reports.iter().find(|r| r.source == source) else {
            if wait == Wait::ForHigherPriority {
                settled = false;
                break;
            }
            continue;
        };
        match report.finding() {
            Some(Finding::Servers {
                servers,
                hints: found,
            }) => {
                let mut used = false;
                if imap.is_none()
                    && let Some(server) = best(servers, Protocol::Imap)
                {
                    imap = Some(Box::new(Choice {
                        server: server.clone(),
                        source,
                    }));
                    used = true;
                }
                if smtp.is_none()
                    && let Some(server) = best(servers, Protocol::Smtp)
                {
                    smtp = Some(Box::new(Choice {
                        server: server.clone(),
                        source,
                    }));
                    used = true;
                }
                if used && hints.is_none() {
                    hints = Some(found.clone());
                }
            }
            Some(Finding::Unsupported { provider, why }) if imap.is_none() && smtp.is_none() => {
                return Decision {
                    outcome: Outcome::Unsupported {
                        provider,
                        why: *why,
                        source,
                    },
                    settled: true,
                };
            }
            Some(Finding::NoMail(why)) if imap.is_none() && smtp.is_none() => {
                return Decision {
                    outcome: Outcome::NoMail { why, source },
                    settled: true,
                };
            }
            _ => {}
        }
        if imap.is_some() && smtp.is_some() {
            break;
        }
    }

    Decision {
        outcome: Outcome::Settings {
            imap,
            smtp,
            hints: hints.unwrap_or_default(),
        },
        settled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Unsupported, Username};

    fn servers(source: Source, list: &[(Protocol, &str, u16, Security)]) -> Report {
        Report {
            source,
            elapsed_ms: 1.0,
            result: Ok(Finding::Servers {
                servers: list
                    .iter()
                    .map(|&(p, h, port, sec)| Server::new(p, h, port, sec))
                    .collect(),
                hints: Hints::default(),
            }),
            detail: String::new(),
            rewrites: Vec::new(),
        }
    }

    fn pair(source: Source, imap: &str, smtp: &str) -> Report {
        servers(
            source,
            &[
                (Protocol::Imap, imap, 993, Security::Tls),
                (Protocol::Smtp, smtp, 465, Security::Tls),
            ],
        )
    }

    fn nothing(source: Source) -> Report {
        Report {
            source,
            elapsed_ms: 1.0,
            result: Ok(Finding::Nothing),
            detail: String::new(),
            rewrites: Vec::new(),
        }
    }

    fn finding(source: Source, finding: Finding) -> Report {
        Report {
            source,
            elapsed_ms: 1.0,
            result: Ok(finding),
            detail: String::new(),
            rewrites: Vec::new(),
        }
    }

    fn all_nothing_except(replacements: Vec<Report>) -> Vec<Report> {
        Source::ALL
            .iter()
            .map(|&source| {
                replacements
                    .iter()
                    .find(|r| r.source == source)
                    .cloned()
                    .unwrap_or_else(|| nothing(source))
            })
            .collect()
    }

    fn imap_host(decision: &Decision) -> Option<&str> {
        match &decision.outcome {
            Outcome::Settings { imap, .. } => imap.as_ref().map(|c| c.server.host.as_str()),
            _ => None,
        }
    }

    const ALL: &[Source] = &Source::ALL;

    #[test]
    fn implicit_tls_beats_starttls_and_plaintext_is_never_chosen() {
        let list = [
            Server::new(Protocol::Smtp, "a", 25, Security::Plain),
            Server::new(Protocol::Smtp, "b", 587, Security::StartTls),
            Server::new(Protocol::Smtp, "c", 465, Security::Tls),
        ];
        assert_eq!(best(&list, Protocol::Smtp).unwrap().host, "c");
        assert!(best(&list[..1], Protocol::Smtp).is_none());
        assert!(plaintext_only(&list[..1], Protocol::Smtp));
        assert!(!plaintext_only(&list, Protocol::Smtp));
    }

    #[test]
    fn document_order_breaks_ties() {
        let list = [
            Server::new(Protocol::Imap, "first", 993, Security::Tls),
            Server::new(Protocol::Imap, "second", 993, Security::Tls),
        ];
        assert_eq!(best(&list, Protocol::Imap).unwrap().host, "first");
    }

    #[test]
    fn lower_priority_answer_waits_for_higher_priority_sources() {
        let ispdb = pair(Source::Ispdb, "imap.isp", "smtp.isp");
        let reports = vec![nothing(Source::Preset), ispdb.clone()];
        let waiting = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(!waiting.settled);
        assert!(decide(ALL, &reports, Wait::No).outcome.is_usable());

        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            nothing(Source::Srv),
            ispdb,
        ];
        let done = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(done.settled);
        assert_eq!(done.outcome.winners(), "ispdb");
    }

    #[test]
    fn protocols_can_come_from_different_sources() {
        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            servers(
                Source::Srv,
                &[(Protocol::Imap, "imap.d", 993, Security::Tls)],
            ),
            servers(
                Source::Ispdb,
                &[
                    (Protocol::Imap, "other.d", 993, Security::Tls),
                    (Protocol::Smtp, "smtp.d", 587, Security::StartTls),
                ],
            ),
        ];
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(decision.settled);
        let Outcome::Settings { imap, smtp, .. } = &decision.outcome else {
            panic!()
        };
        assert_eq!(imap.as_ref().unwrap().server.host, "imap.d");
        assert_eq!(smtp.as_ref().unwrap().server.host, "smtp.d");
        assert_eq!(decision.outcome.winners(), "srv+ispdb");
    }

    #[test]
    fn unsupported_and_no_mail_are_terminal() {
        let reports = vec![finding(
            Source::Preset,
            Finding::Unsupported {
                provider: "Proton Mail",
                why: Unsupported::BridgeOnly,
            },
        )];
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(decision.settled);
        assert!(matches!(decision.outcome, Outcome::Unsupported { .. }));
    }

    #[test]
    fn everything_failing_settles_empty() {
        let reports: Vec<Report> = Source::ALL.iter().map(|&s| nothing(s)).collect();
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(decision.settled);
        assert!(!decision.outcome.is_usable());
    }

    #[test]
    fn errors_count_as_completed() {
        let mut reports = all_nothing_except(vec![servers(
            Source::Probe,
            &[
                (Protocol::Imap, "mail.d", 143, Security::StartTls),
                (Protocol::Smtp, "mail.d", 587, Security::StartTls),
            ],
        )]);
        reports[1].result = Err("timeout".into());
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(decision.settled && decision.outcome.is_usable());
        let Outcome::Settings { imap, .. } = decision.outcome else {
            panic!()
        };
        assert_eq!(imap.unwrap().server.username, Username::Address);
    }

    #[test]
    fn own_host_outranks_a_hosted_platform_found_through_mx() {
        let reports = all_nothing_except(vec![
            pair(Source::OwnHost, "imap.d", "smtp.d"),
            pair(
                Source::MxPreset,
                "outlook.office365.com",
                "smtp.office365.com",
            ),
        ]);
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(decision.settled);
        assert_eq!(imap_host(&decision), Some("imap.d"));
        assert_eq!(decision.outcome.winners(), "own-host");
    }

    #[test]
    fn own_host_ranks_below_the_domains_own_statements() {
        let reports = all_nothing_except(vec![
            pair(Source::Ispdb, "imap.isp", "smtp.isp"),
            pair(Source::OwnHost, "imap.d", "smtp.d"),
        ]);
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert_eq!(imap_host(&decision), Some("imap.isp"));
    }

    #[test]
    fn own_host_waits_for_the_mx_answer() {
        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            nothing(Source::Srv),
            nothing(Source::Ispdb),
            pair(Source::OwnHost, "imap.d", "smtp.d"),
        ];
        let waiting = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(!waiting.settled);
        assert_eq!(
            imap_host(&decide(ALL, &reports, Wait::No)),
            Some("imap.d"),
            "provisional answer"
        );
    }

    #[test]
    fn mx_no_mail_or_unsupported_overrides_own_host() {
        for mx in [
            Finding::NoMail("null MX"),
            Finding::Unsupported {
                provider: "Proton Mail",
                why: Unsupported::BridgeOnly,
            },
        ] {
            let reports = all_nothing_except(vec![
                pair(Source::OwnHost, "imap.d", "smtp.d"),
                finding(Source::MxPreset, mx),
            ]);
            let decision = decide(ALL, &reports, Wait::ForHigherPriority);
            assert!(decision.settled);
            assert!(!matches!(decision.outcome, Outcome::Settings { .. }));
        }
    }

    #[test]
    fn mx_probe_is_the_last_resort() {
        let reports = all_nothing_except(vec![
            pair(Source::Probe, "mail.d", "mail.d"),
            pair(Source::MxProbe, "imap.host", "smtp.host"),
        ]);
        assert_eq!(
            imap_host(&decide(ALL, &reports, Wait::ForHigherPriority)),
            Some("mail.d")
        );
        let reports = all_nothing_except(vec![pair(Source::MxProbe, "imap.host", "smtp.host")]);
        assert_eq!(
            imap_host(&decide(ALL, &reports, Wait::ForHigherPriority)),
            Some("imap.host")
        );
    }

    #[test]
    fn a_null_mx_does_not_wait_for_the_own_host_check() {
        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            nothing(Source::Srv),
            nothing(Source::Ispdb),
            finding(Source::MxPreset, Finding::NoMail("null MX")),
        ];
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(decision.settled);
        assert!(matches!(decision.outcome, Outcome::NoMail { .. }));
    }
}
