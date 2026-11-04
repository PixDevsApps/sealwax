use crate::model::{Choice, Outcome, Security, Username};
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
    use crate::model::{Hints, Protocol, Server, Source};

    const IMAP: &[Accept] = &[Accept::tls("imap.example.com", 993, &[User::Address])];
    const SMTP: &[Accept] = &[
        Accept::tls("smtp.example.com", 465, &[User::Address]),
        Accept::starttls("smtp.example.com", 587, &[User::Address]),
    ];

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

    fn outcome(smtp_port: u16, security: Security, username: Username) -> Outcome {
        let choice = |protocol, host: &str, port, security| Choice {
            server: Server::new(protocol, host, port, security).with_username(username.clone()),
            source: Source::Ispdb,
        };
        Outcome::Settings {
            imap: Some(Box::new(choice(
                Protocol::Imap,
                "imap.example.com",
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
}
