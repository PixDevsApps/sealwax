use crate::model::{Choice, Outcome, Protocol, Security, Username};
use crate::truth::{Accept, Expect, Truth, User};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Correct,
    Wrong(String),
}

impl Verdict {
    pub fn is_correct(&self) -> bool {
        *self == Verdict::Correct
    }
}

fn user_matches(username: &Username, users: &[User]) -> bool {
    users.iter().any(|user| {
        matches!(
            (user, username),
            (User::Address, Username::Address)
                | (User::LocalPart, Username::LocalPart)
                | (User::Other, Username::Template(_))
        )
    })
}

fn check(label: &str, choice: Option<&Choice>, accepted: &[Accept]) -> Result<(), String> {
    let Some(choice) = choice else {
        return Err(format!("no {label} server"));
    };
    let server = &choice.server;
    let endpoint = accepted
        .iter()
        .find(|a| a.host == server.host && a.port == server.port && a.security == server.security);
    match endpoint {
        None => Err(format!(
            "{label} {}:{} {} not accepted",
            server.host, server.port, server.security
        )),
        Some(accept) if !user_matches(&server.username, accept.users) => {
            Err(format!("{label} username {} wrong", server.username))
        }
        Some(_) => Ok(()),
    }
}

/// S4's strict rubric, unchanged.
pub fn judge(truth: &Truth, outcome: &Outcome) -> Verdict {
    let result = match (&truth.expect, outcome) {
        (
            Expect::Settings { imap, smtp },
            Outcome::Settings {
                imap: i, smtp: s, ..
            },
        ) => check("IMAP", i.as_deref(), imap).and_then(|()| check("SMTP", s.as_deref(), smtp)),
        (Expect::Settings { .. }, Outcome::Unsupported { .. }) => Err("said unsupported".into()),
        (Expect::Settings { .. }, Outcome::NoMail { .. }) => Err("said no mail".into()),
        (Expect::Unsupported, Outcome::Unsupported { .. }) => Ok(()),
        (Expect::Unsupported, _) => Err("missed unsupported provider".into()),
        (Expect::NoMail, Outcome::NoMail { .. }) => Ok(()),
        (
            Expect::NoMail,
            Outcome::Settings {
                imap: None,
                smtp: None,
                ..
            },
        ) => Ok(()),
        (Expect::NoMail, _) => Err("offered settings for a domain without mail".into()),
    };
    match result {
        Ok(()) => Verdict::Correct,
        Err(why) => Verdict::Wrong(why),
    }
}

/// E1: the two-attempt plan reaches either of full address and local part.
fn plan_matches(username: &Username, users: &[User]) -> bool {
    match username {
        Username::Template(_) => users.contains(&User::Other),
        Username::Address | Username::LocalPart => users
            .iter()
            .any(|u| matches!(u, User::Address | User::LocalPart)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasPair {
    pub protocol: Protocol,
    pub chosen: String,
    pub documented: String,
    pub port: u16,
    pub security: Security,
}

/// The documented endpoints an undocumented chosen host would need E2 evidence for.
pub fn alias_candidates(truth: &Truth, outcome: &Outcome) -> Vec<AliasPair> {
    let (
        Expect::Settings { imap, smtp },
        Outcome::Settings {
            imap: chosen_imap,
            smtp: chosen_smtp,
            ..
        },
    ) = (&truth.expect, outcome)
    else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    for (protocol, choice, accepted) in [
        (Protocol::Imap, chosen_imap, imap),
        (Protocol::Smtp, chosen_smtp, smtp),
    ] {
        let Some(choice) = choice else { continue };
        let server = &choice.server;
        if accepted.iter().any(|a| a.host == server.host) {
            continue;
        }
        for accept in accepted.iter() {
            let pair = AliasPair {
                protocol,
                chosen: server.host.clone(),
                documented: accept.host.to_string(),
                port: server.port,
                security: server.security,
            };
            if accept.port == server.port
                && accept.security == server.security
                && !pairs.contains(&pair)
            {
                pairs.push(pair);
            }
        }
    }
    pairs
}

type UserCheck = fn(&Username, &[User]) -> bool;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Via {
    pub username_plan: bool,
    pub alias: bool,
}

impl Via {
    pub fn text(self) -> &'static str {
        match (self.username_plan, self.alias) {
            (false, false) => "strict",
            (true, false) => "E1",
            (false, true) => "E2",
            (true, true) => "E1+E2",
        }
    }
}

fn check_extended(
    label: &str,
    choice: Option<&Choice>,
    accepted: &[Accept],
    same: &dyn Fn(&AliasPair) -> bool,
    protocol: Protocol,
) -> Result<Via, String> {
    let Some(choice) = choice else {
        return Err(format!("no {label} server"));
    };
    let server = &choice.server;
    let exact: Vec<&Accept> = accepted
        .iter()
        .filter(|a| a.host == server.host && a.port == server.port && a.security == server.security)
        .collect();
    let aliased: Vec<&Accept> = accepted
        .iter()
        .filter(|a| {
            a.host != server.host
                && a.port == server.port
                && a.security == server.security
                && same(&AliasPair {
                    protocol,
                    chosen: server.host.clone(),
                    documented: a.host.to_string(),
                    port: server.port,
                    security: server.security,
                })
        })
        .collect();
    for (alias, endpoints) in [(false, &exact), (true, &aliased)] {
        for username_plan in [false, true] {
            let matches: UserCheck = if username_plan {
                plan_matches
            } else {
                user_matches
            };
            if endpoints.iter().any(|a| matches(&server.username, a.users)) {
                return Ok(Via {
                    username_plan,
                    alias,
                });
            }
        }
    }
    if exact.is_empty() && aliased.is_empty() {
        Err(format!(
            "{label} {}:{} {} not accepted",
            server.host, server.port, server.security
        ))
    } else {
        Err(format!("{label} username {} wrong", server.username))
    }
}

/// The extended rubric: strict plus E1 (username plan) and E2 (alias equivalence).
pub fn judge_extended(
    truth: &Truth,
    outcome: &Outcome,
    same: &dyn Fn(&AliasPair) -> bool,
) -> (Verdict, Via) {
    match (&truth.expect, outcome) {
        (
            Expect::Settings { imap, smtp },
            Outcome::Settings {
                imap: i, smtp: s, ..
            },
        ) => {
            let result =
                check_extended("IMAP", i.as_deref(), imap, same, Protocol::Imap).and_then(|a| {
                    check_extended("SMTP", s.as_deref(), smtp, same, Protocol::Smtp).map(|b| Via {
                        username_plan: a.username_plan || b.username_plan,
                        alias: a.alias || b.alias,
                    })
                });
            match result {
                Ok(via) => (Verdict::Correct, via),
                Err(why) => (Verdict::Wrong(why), Via::default()),
            }
        }
        _ => (judge(truth, outcome), Via::default()),
    }
}

pub fn tls_only(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Settings { imap, smtp, .. } => [imap, smtp]
            .iter()
            .filter_map(|c| c.as_ref())
            .all(|c| c.server.security != Security::Plain),
        _ => true,
    }
}

pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Hints, Server, Source};

    const IMAP: &[Accept] = &[Accept::tls("imap.example.com", 993, &[User::Address])];
    const SMTP: &[Accept] = &[
        Accept::tls("smtp.example.com", 465, &[User::Address]),
        Accept::starttls("smtp.example.com", 587, &[User::Address]),
    ];
    const LOCAL_ONLY: &[Accept] = &[Accept::tls("imap.example.com", 993, &[User::LocalPart])];
    const OTHER: &[Accept] = &[Accept::tls("imap.example.com", 993, &[User::Other])];

    fn truth() -> Truth {
        Truth {
            domain: "example.com",
            group: "test",
            expect: Expect::Settings {
                imap: IMAP,
                smtp: SMTP,
            },
            evidence: "",
        }
    }

    fn with_imap(imap: &'static [Accept]) -> Truth {
        Truth {
            expect: Expect::Settings { imap, smtp: SMTP },
            ..truth()
        }
    }

    fn outcome_with(
        imap_host: &str,
        smtp_port: u16,
        security: Security,
        username: Username,
    ) -> Outcome {
        let choice = |protocol, host: &str, port, security| Choice {
            server: Server::new(protocol, host, port, security).with_username(username.clone()),
            source: Source::Ispdb,
        };
        Outcome::Settings {
            imap: Some(Box::new(choice(
                Protocol::Imap,
                imap_host,
                993,
                Security::Tls,
            ))),
            smtp: Some(Box::new(choice(
                Protocol::Smtp,
                "smtp.example.com",
                smtp_port,
                security,
            ))),
            hints: Hints::default(),
        }
    }

    fn outcome(smtp_port: u16, security: Security, username: Username) -> Outcome {
        outcome_with("imap.example.com", smtp_port, security, username)
    }

    fn no_alias(_: &AliasPair) -> bool {
        false
    }

    #[test]
    fn any_documented_endpoint_counts() {
        assert!(judge(&truth(), &outcome(465, Security::Tls, Username::Address)).is_correct());
        assert!(
            judge(
                &truth(),
                &outcome(587, Security::StartTls, Username::Address)
            )
            .is_correct()
        );
        assert!(!judge(&truth(), &outcome(587, Security::Tls, Username::Address)).is_correct());
        assert!(!judge(&truth(), &outcome(465, Security::Tls, Username::LocalPart)).is_correct());
    }

    #[test]
    fn no_mail_accepts_nothing_found() {
        let truth = Truth {
            expect: Expect::NoMail,
            ..truth()
        };
        let empty = Outcome::Settings {
            imap: None,
            smtp: None,
            hints: Hints::default(),
        };
        assert!(judge(&truth, &empty).is_correct());
        assert!(!judge(&truth, &outcome(465, Security::Tls, Username::Address)).is_correct());
    }

    #[test]
    fn nearest_rank_percentiles() {
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&values, 50.0), Some(10.0));
        assert_eq!(percentile(&values, 95.0), Some(19.0));
        assert_eq!(percentile(&values, 100.0), Some(20.0));
        assert_eq!(percentile(&[3.0], 95.0), Some(3.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn e1_makes_address_and_local_part_interchangeable() {
        let truth = with_imap(LOCAL_ONLY);
        let address = outcome(465, Security::Tls, Username::Address);
        assert!(!judge(&truth, &address).is_correct());
        let mixed = Outcome::Settings {
            imap: Some(Box::new(Choice {
                server: Server::new(Protocol::Imap, "imap.example.com", 993, Security::Tls),
                source: Source::Probe,
            })),
            smtp: Some(Box::new(Choice {
                server: Server::new(Protocol::Smtp, "smtp.example.com", 465, Security::Tls),
                source: Source::Probe,
            })),
            hints: Hints::default(),
        };
        let (verdict, via) = judge_extended(&truth, &mixed, &no_alias);
        assert!(verdict.is_correct());
        assert_eq!(via.text(), "E1");
    }

    #[test]
    fn ask_matches_only_a_provider_assigned_name() {
        let ask = Username::Template("ask".into());
        let other = with_imap(OTHER);
        let imap_only = |username: Username| Outcome::Settings {
            imap: Some(Box::new(Choice {
                server: Server::new(Protocol::Imap, "imap.example.com", 993, Security::Tls)
                    .with_username(username),
                source: Source::MxProbe,
            })),
            smtp: Some(Box::new(Choice {
                server: Server::new(Protocol::Smtp, "smtp.example.com", 465, Security::Tls),
                source: Source::MxProbe,
            })),
            hints: Hints::default(),
        };
        assert!(judge(&other, &imap_only(ask.clone())).is_correct());
        assert!(!judge(&other, &imap_only(Username::Address)).is_correct());
        assert!(
            !judge_extended(&other, &imap_only(Username::Address), &no_alias)
                .0
                .is_correct()
        );
        assert!(!judge(&truth(), &imap_only(ask.clone())).is_correct());
        assert!(
            !judge_extended(&truth(), &imap_only(ask), &no_alias)
                .0
                .is_correct()
        );
    }

    #[test]
    fn e2_accepts_a_verified_alias_on_the_same_port_and_mode() {
        let aliased = outcome_with("mail.example.com", 465, Security::Tls, Username::Address);
        assert!(!judge(&truth(), &aliased).is_correct());
        let pairs = alias_candidates(&truth(), &aliased);
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            (pairs[0].chosen.as_str(), pairs[0].documented.as_str()),
            ("mail.example.com", "imap.example.com")
        );
        let same = |pair: &AliasPair| pair.chosen == "mail.example.com";
        let (verdict, via) = judge_extended(&truth(), &aliased, &same);
        assert!(verdict.is_correct());
        assert_eq!(via.text(), "E2");
        assert!(!judge_extended(&truth(), &aliased, &no_alias).0.is_correct());
    }

    #[test]
    fn e2_never_accepts_another_port() {
        let other_port = outcome(587, Security::Tls, Username::Address);
        assert!(alias_candidates(&truth(), &other_port).is_empty());
        let same = |_: &AliasPair| true;
        assert!(!judge_extended(&truth(), &other_port, &same).0.is_correct());
    }
}
