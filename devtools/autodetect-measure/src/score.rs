//! The rubric (docs/m2/autoconfig.md, frozen before the held-out run): S4's strict rubric, then
//! the same with the product's second login attempt (E1, decided by the user on 2026-10-04), then
//! with verified aliases as well (E2, reported apart: not decided).

use sealwax_domain::account::{Detection, Endpoint, Protocol, Security, UsernameForm};

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

    pub fn why(&self) -> &str {
        match self {
            Verdict::Correct => "",
            Verdict::Wrong(why) => why,
        }
    }
}

/// A chosen host that isn't documented, paired with a documented host on the same port and TLS
/// mode: E2 needs evidence that both are the same server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasPair {
    pub protocol: Protocol,
    pub chosen: String,
    pub documented: String,
    pub port: u16,
    pub security: Security,
}

/// The rubric's three readings of one result.
#[derive(Clone, Debug)]
pub struct Scores {
    pub strict: Verdict,
    /// With the second login attempt (E1).
    pub second: Verdict,
    /// With the second attempt and verified aliases (E1 + E2).
    pub aliases: Verdict,
}

fn matches(form: UsernameForm, users: &[User]) -> bool {
    users.iter().any(|user| {
        matches!(
            (user, form),
            (User::Address, UsernameForm::Address)
                | (User::LocalPart, UsernameForm::LocalPart)
                | (User::Other, UsernameForm::Other)
        )
    })
}

fn same_endpoint(accept: &Accept, endpoint: &Endpoint) -> bool {
    accept.port == endpoint.port && accept.security == endpoint.security
}

/// The accepted entries that `endpoint` reaches: the documented ones, plus with `aliases` the
/// documented hosts `same` vouches for.
fn reached<'a>(
    protocol: Protocol,
    endpoint: &Endpoint,
    accepted: &'a [Accept],
    aliases: bool,
    same: &dyn Fn(&AliasPair) -> bool,
) -> Vec<&'a Accept> {
    accepted
        .iter()
        .filter(|a| same_endpoint(a, endpoint))
        .filter(|a| {
            a.host == endpoint.host
                || aliases
                    && same(&AliasPair {
                        protocol,
                        chosen: endpoint.host.clone(),
                        documented: a.host.to_string(),
                        port: endpoint.port,
                        security: endpoint.security,
                    })
        })
        .collect()
}

pub fn alias_candidates(truth: &Truth, detection: &Detection) -> Vec<AliasPair> {
    let (Expect::Settings { imap, smtp }, Detection::Found(found)) = (&truth.expect, detection)
    else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    for (protocol, endpoint, accepted) in [
        (Protocol::Imap, &found.imap, *imap),
        (Protocol::Smtp, &found.smtp, *smtp),
    ] {
        if accepted.iter().any(|a| a.host == endpoint.host) {
            continue;
        }
        for accept in accepted.iter().filter(|a| same_endpoint(a, endpoint)) {
            let pair = AliasPair {
                protocol,
                chosen: endpoint.host.clone(),
                documented: accept.host.to_string(),
                port: endpoint.port,
                security: endpoint.security,
            };
            if !pairs.contains(&pair) {
                pairs.push(pair);
            }
        }
    }
    pairs
}

fn judge_with(
    truth: &Truth,
    detection: &Detection,
    second: bool,
    aliases: bool,
    same: &dyn Fn(&AliasPair) -> bool,
) -> Verdict {
    let wrong = |why: &str| Verdict::Wrong(why.to_string());
    match (&truth.expect, detection) {
        (Expect::Settings { imap, smtp }, Detection::Found(found)) => {
            let imap_reached = reached(Protocol::Imap, &found.imap, imap, aliases, same);
            let smtp_reached = reached(Protocol::Smtp, &found.smtp, smtp, aliases, same);
            for (label, endpoint, reached) in [
                ("IMAP", &found.imap, &imap_reached),
                ("SMTP", &found.smtp, &smtp_reached),
            ] {
                if reached.is_empty() {
                    return Verdict::Wrong(format!(
                        "{label} {}:{} {:?} not accepted",
                        endpoint.host, endpoint.port, endpoint.security
                    ));
                }
            }
            // The product logs in with one form for both protocols: the first, then (E1) the
            // second for both.
            let mut forms = vec![found.username];
            if second {
                forms.extend(found.second_username);
            }
            let fits = |form: UsernameForm, reached: &[&Accept]| {
                reached.iter().any(|a| matches(form, a.users))
            };
            if forms
                .iter()
                .any(|&form| fits(form, &imap_reached) && fits(form, &smtp_reached))
            {
                Verdict::Correct
            } else {
                Verdict::Wrong(format!("username {:?} wrong", found.username))
            }
        }
        (Expect::Settings { .. }, Detection::Unsupported { .. }) => wrong("said unsupported"),
        (Expect::Settings { .. }, Detection::NoMail) => wrong("said no mail"),
        (Expect::Settings { .. }, Detection::NotFound) => wrong("nothing found"),
        (Expect::Unsupported, Detection::Unsupported { .. }) => Verdict::Correct,
        (Expect::Unsupported, _) => wrong("missed an unsupported provider"),
        (Expect::NoMail, Detection::NoMail | Detection::NotFound) => Verdict::Correct,
        (Expect::NoMail, _) => wrong("offered settings for a domain without mail"),
    }
}

pub fn judge(truth: &Truth, detection: &Detection, same: &dyn Fn(&AliasPair) -> bool) -> Scores {
    Scores {
        strict: judge_with(truth, detection, false, false, same),
        second: judge_with(truth, detection, true, false, same),
        aliases: judge_with(truth, detection, true, true, same),
    }
}

/// Nearest-rank percentile, as S4 and S4b reported them.
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
    use sealwax_domain::account::{AuthMethod, DetectSource, DetectedSettings};

    const A: &[User] = &[User::Address];
    const L: &[User] = &[User::LocalPart];
    const IMAP: &[Accept] = &[Accept::tls("imap.example.test", 993, A)];
    const IMAP_LOCAL: &[Accept] = &[Accept::tls("imap.example.test", 993, L)];
    const SMTP: &[Accept] = &[
        Accept::tls("smtp.example.test", 465, A),
        Accept::starttls("smtp.example.test", 587, A),
    ];
    const SMTP_LOCAL: &[Accept] = &[Accept::tls("smtp.example.test", 465, L)];

    fn truth(imap: &'static [Accept], smtp: &'static [Accept]) -> Truth {
        Truth {
            domain: "example.test",
            group: "test",
            expect: Expect::Settings { imap, smtp },
            evidence: "",
        }
    }

    fn found(imap_host: &str, smtp: (u16, Security), username: UsernameForm) -> Detection {
        let endpoint = |host: &str, port, security| Endpoint {
            host: host.into(),
            port,
            security,
            certificate: None,
        };
        Detection::Found(Box::new(DetectedSettings {
            imap: endpoint(imap_host, 993, Security::Tls),
            smtp: endpoint("smtp.example.test", smtp.0, smtp.1),
            username,
            second_username: match username {
                UsernameForm::Address => Some(UsernameForm::LocalPart),
                UsernameForm::LocalPart => Some(UsernameForm::Address),
                UsernameForm::Other => None,
            },
            auth: AuthMethod::Password,
            provider: None,
            confirm: false,
            app_password: None,
            enable_imap: None,
            source: DetectSource::Ispdb,
        }))
    }

    fn never(_: &AliasPair) -> bool {
        false
    }

    #[test]
    fn any_documented_endpoint_counts_strictly() {
        let t = truth(IMAP, SMTP);
        for smtp in [(465, Security::Tls), (587, Security::StartTls)] {
            let scores = judge(
                &t,
                &found("imap.example.test", smtp, UsernameForm::Address),
                &never,
            );
            assert!(scores.strict.is_correct());
        }
        let wrong_mode = judge(
            &t,
            &found(
                "imap.example.test",
                (587, Security::Tls),
                UsernameForm::Address,
            ),
            &never,
        );
        assert!(!wrong_mode.aliases.is_correct());
    }

    #[test]
    fn the_second_attempt_reaches_the_other_form_for_both_protocols() {
        let t = truth(IMAP_LOCAL, SMTP_LOCAL);
        let scores = judge(
            &t,
            &found(
                "imap.example.test",
                (465, Security::Tls),
                UsernameForm::Address,
            ),
            &never,
        );
        assert!(!scores.strict.is_correct());
        assert!(scores.second.is_correct());

        let mixed = truth(IMAP_LOCAL, SMTP);
        let scores = judge(
            &mixed,
            &found(
                "imap.example.test",
                (465, Security::Tls),
                UsernameForm::Address,
            ),
            &never,
        );
        assert!(
            !scores.second.is_correct(),
            "one form must fit both protocols"
        );
    }

    #[test]
    fn an_underivable_name_matches_only_other() {
        const OTHER: &[Accept] = &[Accept::tls("imap.example.test", 993, &[User::Other])];
        const SMTP_OTHER: &[Accept] = &[Accept::tls("smtp.example.test", 465, &[User::Other])];
        let t = truth(OTHER, SMTP_OTHER);
        let ask = found(
            "imap.example.test",
            (465, Security::Tls),
            UsernameForm::Other,
        );
        assert!(judge(&t, &ask, &never).strict.is_correct());
        let address = found(
            "imap.example.test",
            (465, Security::Tls),
            UsernameForm::Address,
        );
        assert!(!judge(&t, &address, &never).aliases.is_correct());
        assert!(!judge(&truth(IMAP, SMTP), &ask, &never).aliases.is_correct());
    }

    #[test]
    fn verified_aliases_count_only_under_e2() {
        let t = truth(IMAP, SMTP);
        let aliased = found(
            "mail.example.test",
            (465, Security::Tls),
            UsernameForm::Address,
        );
        let pairs = alias_candidates(&t, &aliased);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].documented, "imap.example.test");
        let same = |pair: &AliasPair| pair.chosen == "mail.example.test";
        let scores = judge(&t, &aliased, &same);
        assert!(!scores.strict.is_correct());
        assert!(!scores.second.is_correct());
        assert!(scores.aliases.is_correct());
        assert!(!judge(&t, &aliased, &never).aliases.is_correct());
    }

    #[test]
    fn no_mail_accepts_nothing_found() {
        let t = Truth {
            expect: Expect::NoMail,
            ..truth(IMAP, SMTP)
        };
        assert!(judge(&t, &Detection::NotFound, &never).strict.is_correct());
        assert!(judge(&t, &Detection::NoMail, &never).strict.is_correct());
        let offered = found(
            "imap.example.test",
            (465, Security::Tls),
            UsernameForm::Address,
        );
        assert!(!judge(&t, &offered, &never).aliases.is_correct());
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
