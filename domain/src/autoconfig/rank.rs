//! S4's settle rule with S4b's own-host exception: per protocol, the highest-ranked source that
//! offers a TLS server wins once every higher-ranked source has answered, failed or timed out.
//! "Not supported" and "no mail" end the search. Implicit TLS beats STARTTLS (RFC 8314), then
//! the source's own order.

use super::{Finding, Hints, Report, Server, Source};
use crate::account::{Protocol, Security, UnsupportedReason};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub server: Server,
    pub source: Source,
    /// The hints of the source that offered the server.
    pub hints: Hints,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Settings {
        imap: Option<Box<Choice>>,
        smtp: Option<Box<Choice>>,
    },
    Unsupported {
        provider: String,
        reason: UnsupportedReason,
        source: Source,
    },
    NoMail {
        source: Source,
    },
}

impl Outcome {
    /// Settings for both protocols, or a definite statement.
    pub fn is_usable(&self) -> bool {
        match self {
            Outcome::Settings { imap, smtp } => imap.is_some() && smtp.is_some(),
            Outcome::Unsupported { .. } | Outcome::NoMail { .. } => true,
        }
    }

    pub fn winners(&self) -> Vec<Source> {
        match self {
            Outcome::Settings { imap, smtp } => [imap, smtp]
                .iter()
                .filter_map(|c| c.as_ref())
                .map(|c| c.source)
                .collect(),
            Outcome::Unsupported { source, .. } | Outcome::NoMail { source } => vec![*source],
        }
    }
}

pub fn best_index(servers: &[Server], protocol: Protocol) -> Option<usize> {
    servers
        .iter()
        .enumerate()
        .filter(|(_, s)| s.protocol == protocol)
        .min_by_key(|(_, s)| match s.security {
            Security::Tls => 0,
            Security::StartTls => 1,
        })
        .map(|(index, _)| index)
}

pub fn best(servers: &[Server], protocol: Protocol) -> Option<&Server> {
    best_index(servers, protocol).map(|index| &servers[index])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Settle only once every higher-ranked source has answered.
    ForHigherRanked,
    /// Take what has answered so far (the first usable answer, or the cap).
    No,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub outcome: Outcome,
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MxSays {
    Pending,
    EndsTheSearch,
    Other,
}

fn mx_says(reports: &[Report]) -> MxSays {
    match reports.iter().find(|r| r.source == Source::MxPreset) {
        None => MxSays::Pending,
        Some(report) => match report.finding {
            Finding::NoMail | Finding::Unsupported { .. } => MxSays::EndsTheSearch,
            _ => MxSays::Other,
        },
    }
}

/// Picks, per protocol, the highest-ranked source in `order` that offers a server.
///
/// `OwnHost` (R1) outranks everything inferred from the MX records, but an MX answer of "no mail"
/// or "not supported" still ends the search, so a found own host waits for the MX answer.
pub fn decide(order: &[Source], reports: &[Report], wait: Wait) -> Decision {
    let mut imap: Option<Box<Choice>> = None;
    let mut smtp: Option<Box<Choice>> = None;
    let mut settled = true;

    for &source in order {
        let report = reports.iter().find(|r| r.source == source);
        if source == Source::OwnHost {
            match mx_says(reports) {
                MxSays::EndsTheSearch => continue,
                MxSays::Pending
                    if wait == Wait::ForHigherRanked
                        && report.is_some_and(|r| matches!(r.finding, Finding::Servers { .. })) =>
                {
                    settled = false;
                    break;
                }
                _ => {}
            }
        }
        let Some(report) = report else {
            if wait == Wait::ForHigherRanked {
                settled = false;
                break;
            }
            continue;
        };
        match &report.finding {
            Finding::Servers { servers, hints } => {
                for (slot, protocol) in [(&mut imap, Protocol::Imap), (&mut smtp, Protocol::Smtp)] {
                    if slot.is_none()
                        && let Some(server) = best(servers, protocol)
                    {
                        *slot = Some(Box::new(Choice {
                            server: server.clone(),
                            source,
                            hints: hints.clone(),
                        }));
                    }
                }
            }
            Finding::Unsupported { provider, reason } if imap.is_none() && smtp.is_none() => {
                return Decision {
                    outcome: Outcome::Unsupported {
                        provider: provider.clone(),
                        reason: *reason,
                        source,
                    },
                    settled: true,
                };
            }
            Finding::NoMail if imap.is_none() && smtp.is_none() => {
                return Decision {
                    outcome: Outcome::NoMail { source },
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
        outcome: Outcome::Settings { imap, smtp },
        settled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoconfig::Rules;

    fn servers(source: Source, list: &[(Protocol, &str, u16, Security)]) -> Report {
        Report {
            source,
            finding: Finding::Servers {
                servers: list
                    .iter()
                    .map(|&(p, h, port, sec)| Server::new(p, h, port, sec))
                    .collect(),
                hints: Hints::default(),
            },
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

    fn report(source: Source, finding: Finding) -> Report {
        Report { source, finding }
    }

    fn nothing(source: Source) -> Report {
        report(source, Finding::Nothing)
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

    fn order() -> [Source; 11] {
        Rules::default().order()
    }

    #[test]
    fn implicit_tls_beats_starttls_then_document_order() {
        let list = [
            Server::new(Protocol::Smtp, "b", 587, Security::StartTls),
            Server::new(Protocol::Smtp, "c", 465, Security::Tls),
            Server::new(Protocol::Smtp, "d", 465, Security::Tls),
        ];
        assert_eq!(best(&list, Protocol::Smtp).unwrap().host, "c");
        assert!(best(&list, Protocol::Imap).is_none());
    }

    #[test]
    fn a_lower_ranked_answer_waits_for_higher_ranked_sources() {
        let srv = pair(Source::Srv, "imap.srv", "smtp.srv");
        let reports = vec![nothing(Source::Preset), srv.clone()];
        assert!(!decide(&order(), &reports, Wait::ForHigherRanked).settled);
        assert!(decide(&order(), &reports, Wait::No).outcome.is_usable());

        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            nothing(Source::Ispdb),
            srv,
        ];
        let done = decide(&order(), &reports, Wait::ForHigherRanked);
        assert!(done.settled);
        assert_eq!(done.outcome.winners(), [Source::Srv, Source::Srv]);
    }

    #[test]
    fn the_ispdb_outranks_srv() {
        let reports = all_nothing_except(vec![
            pair(Source::Srv, "imap.srv", "smtp.srv"),
            pair(Source::Ispdb, "imap.isp", "smtp.isp"),
        ]);
        let decision = decide(&order(), &reports, Wait::ForHigherRanked);
        assert_eq!(imap_host(&decision), Some("imap.isp"));
        let s4b = Rules {
            ispdb_before_srv: false,
            ..Rules::default()
        }
        .order();
        assert_eq!(
            imap_host(&decide(&s4b, &reports, Wait::ForHigherRanked)),
            Some("imap.srv")
        );
    }

    #[test]
    fn protocols_can_come_from_different_sources() {
        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            servers(
                Source::Ispdb,
                &[(Protocol::Imap, "imap.d", 993, Security::Tls)],
            ),
            servers(
                Source::Srv,
                &[
                    (Protocol::Imap, "other.d", 993, Security::Tls),
                    (Protocol::Smtp, "smtp.d", 587, Security::StartTls),
                ],
            ),
        ];
        let decision = decide(&order(), &reports, Wait::ForHigherRanked);
        assert!(decision.settled);
        let Outcome::Settings { imap, smtp } = &decision.outcome else {
            panic!("expected settings")
        };
        assert_eq!(imap.as_ref().unwrap().server.host, "imap.d");
        assert_eq!(smtp.as_ref().unwrap().server.host, "smtp.d");
        assert_eq!(decision.outcome.winners(), [Source::Ispdb, Source::Srv]);
    }

    #[test]
    fn unsupported_and_no_mail_are_terminal() {
        let reports = vec![report(
            Source::Preset,
            Finding::Unsupported {
                provider: "Proton Mail".into(),
                reason: UnsupportedReason::BridgeOnly,
            },
        )];
        let decision = decide(&order(), &reports, Wait::ForHigherRanked);
        assert!(decision.settled);
        assert!(matches!(decision.outcome, Outcome::Unsupported { .. }));
    }

    #[test]
    fn everything_failing_settles_empty() {
        let reports: Vec<Report> = Source::ALL.iter().map(|&s| nothing(s)).collect();
        let decision = decide(&order(), &reports, Wait::ForHigherRanked);
        assert!(decision.settled);
        assert!(!decision.outcome.is_usable());
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
        let decision = decide(&order(), &reports, Wait::ForHigherRanked);
        assert!(decision.settled);
        assert_eq!(imap_host(&decision), Some("imap.d"));
    }

    #[test]
    fn own_host_ranks_below_the_domains_own_statements() {
        let reports = all_nothing_except(vec![
            pair(Source::Srv, "imap.srv", "smtp.srv"),
            pair(Source::OwnHost, "imap.d", "smtp.d"),
        ]);
        let decision = decide(&order(), &reports, Wait::ForHigherRanked);
        assert_eq!(imap_host(&decision), Some("imap.srv"));
    }

    #[test]
    fn own_host_waits_for_the_mx_answer() {
        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            nothing(Source::Ispdb),
            nothing(Source::Srv),
            pair(Source::OwnHost, "imap.d", "smtp.d"),
        ];
        assert!(!decide(&order(), &reports, Wait::ForHigherRanked).settled);
        assert_eq!(
            imap_host(&decide(&order(), &reports, Wait::No)),
            Some("imap.d"),
            "provisional answer"
        );
    }

    #[test]
    fn mx_no_mail_or_unsupported_overrides_own_host() {
        for mx in [
            Finding::NoMail,
            Finding::Unsupported {
                provider: "Proton Mail".into(),
                reason: UnsupportedReason::BridgeOnly,
            },
        ] {
            let reports = all_nothing_except(vec![
                pair(Source::OwnHost, "imap.d", "smtp.d"),
                report(Source::MxPreset, mx),
            ]);
            let decision = decide(&order(), &reports, Wait::ForHigherRanked);
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
            imap_host(&decide(&order(), &reports, Wait::ForHigherRanked)),
            Some("mail.d")
        );
        let reports = all_nothing_except(vec![pair(Source::MxProbe, "imap.host", "smtp.host")]);
        assert_eq!(
            imap_host(&decide(&order(), &reports, Wait::ForHigherRanked)),
            Some("imap.host")
        );
    }

    #[test]
    fn a_null_mx_does_not_wait_for_the_own_host_check() {
        let reports = vec![
            nothing(Source::Preset),
            nothing(Source::Autoconfig),
            nothing(Source::Ispdb),
            nothing(Source::Srv),
            report(Source::MxPreset, Finding::NoMail),
        ];
        let decision = decide(&order(), &reports, Wait::ForHigherRanked);
        assert!(decision.settled);
        assert!(matches!(decision.outcome, Outcome::NoMail { .. }));
    }

    #[test]
    fn the_winning_sources_hints_travel_with_the_choice() {
        let mut ispdb = pair(Source::Ispdb, "imap.isp", "smtp.isp");
        if let Finding::Servers { hints, .. } = &mut ispdb.finding {
            hints.provider = Some("ISP".into());
        }
        let reports = all_nothing_except(vec![ispdb]);
        let Outcome::Settings {
            imap: Some(imap), ..
        } = decide(&order(), &reports, Wait::ForHigherRanked).outcome
        else {
            panic!("expected settings")
        };
        assert_eq!(imap.hints.provider.as_deref(), Some("ISP"));
    }
}
