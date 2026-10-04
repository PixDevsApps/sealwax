use crate::model::{Choice, Finding, Hints, Outcome, Protocol, Report, Security, Server, Source};

pub fn best(servers: &[Server], protocol: Protocol) -> Option<&Server> {
    servers
        .iter()
        .filter(|s| s.protocol == protocol && s.security.is_tls())
        .min_by_key(|s| match s.security {
            Security::Tls => 0,
            Security::StartTls => 1,
            Security::Plain => 2,
        })
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

pub fn decide(sources: &[Source], reports: &[Report], wait: Wait) -> Decision {
    let mut imap: Option<Box<Choice>> = None;
    let mut smtp: Option<Box<Choice>> = None;
    let mut hints: Option<Hints> = None;
    let mut settled = true;

    for &source in sources {
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
    use crate::model::Username;

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
        }
    }

    fn nothing(source: Source) -> Report {
        Report {
            source,
            elapsed_ms: 1.0,
            result: Ok(Finding::Nothing),
            detail: String::new(),
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
        let ispdb = servers(
            Source::Ispdb,
            &[
                (Protocol::Imap, "imap.isp", 993, Security::Tls),
                (Protocol::Smtp, "smtp.isp", 465, Security::Tls),
            ],
        );
        let reports = vec![nothing(Source::Preset), ispdb.clone()];
        let waiting = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(!waiting.settled);
        let provisional = decide(ALL, &reports, Wait::No);
        assert!(provisional.outcome.is_usable());

        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            nothing(Source::WellKnown),
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
            nothing(Source::WellKnown),
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
        let reports = vec![Report {
            source: Source::Preset,
            elapsed_ms: 0.0,
            result: Ok(Finding::Unsupported {
                provider: "Proton Mail",
                why: crate::model::Unsupported::BridgeOnly,
            }),
            detail: String::new(),
        }];
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
        let mut reports: Vec<Report> = Source::ALL.iter().map(|&s| nothing(s)).collect();
        reports[1].result = Err("timeout".into());
        reports[7] = servers(
            Source::Probe,
            &[
                (Protocol::Imap, "mail.d", 143, Security::StartTls),
                (Protocol::Smtp, "mail.d", 587, Security::StartTls),
            ],
        );
        let decision = decide(ALL, &reports, Wait::ForHigherPriority);
        assert!(decision.settled && decision.outcome.is_usable());
        let Outcome::Settings { imap, .. } = decision.outcome else {
            panic!()
        };
        assert_eq!(imap.unwrap().server.username, Username::Address);
    }
}
