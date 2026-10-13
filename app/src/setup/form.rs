//! The assistant's fields as values (PRD A4): the address, how mail comes in (IMAP or POP3,
//! M8), server names, ports and the usernames to try, checked before anything goes to the
//! engine.

use sealwax_domain::account::{
    AccountColor, AccountKey, AccountSettings, AuthMethod, DetectedSettings, Endpoint, Protocol,
    Receive, Security,
};
use std::net::{IpAddr, Ipv6Addr};

/// An address as typed, trimmed, with its domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub text: String,
    pub local: String,
    pub domain: String,
}

/// `name@domain`: one `@`, no spaces, a domain of two or more labels.
pub fn parse_address(text: &str) -> Option<Address> {
    let text = text.trim();
    let (local, domain) = text.rsplit_once('@')?;
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    let plain = |part: &str| !part.is_empty() && !part.contains(char::is_whitespace);
    let label = |label: &str| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_alphanumeric() || c == '-')
    };
    let labels: Vec<&str> = domain.split('.').collect();
    (plain(local) && !local.contains('@') && labels.len() >= 2 && labels.into_iter().all(label))
        .then(|| Address {
            text: text.to_owned(),
            local: local.to_owned(),
            domain: domain.to_owned(),
        })
}

/// A server name: a DNS name of letters, digits and hyphens, or an IP address.
pub fn parse_host(text: &str) -> Option<String> {
    let host = text.trim();
    if let Some(inner) = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return inner.parse::<Ipv6Addr>().is_ok().then(|| inner.to_owned());
    }
    if host.parse::<IpAddr>().is_ok() {
        return Some(host.to_owned());
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    let label = |label: &str| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    (!name.is_empty() && name.len() <= 253 && name.split('.').all(label)).then(|| name.to_owned())
}

/// A port from 1 to 65535, in digits only.
pub fn parse_port(text: &str) -> Option<u16> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|port| *port != 0)
}

/// The standard port for a protocol over a kind of TLS (RFC 8314, RFC 6409).
pub fn default_port(protocol: Protocol, security: Security) -> u16 {
    match (protocol, security) {
        (Protocol::Imap, Security::Tls) => 993,
        (Protocol::Imap, Security::StartTls) => 143,
        (Protocol::Smtp, Security::Tls) => 465,
        (Protocol::Smtp, Security::StartTls) => 587,
        (Protocol::Pop3, Security::Tls) => 995,
        (Protocol::Pop3, Security::StartTls) => 110,
    }
}

pub fn security_name(security: Security) -> &'static str {
    match security {
        Security::Tls => "TLS",
        Security::StartTls => "STARTTLS",
    }
}

/// The username to log in with and the one more to try (S4b: at most two attempts). A
/// username the user typed is tried alone, unless it is the one detection proposed.
pub fn usernames(
    address: &Address,
    detected: Option<&DetectedSettings>,
    typed: Option<&str>,
) -> Option<(String, Option<String>)> {
    let first = detected.and_then(|settings| settings.username.apply(&address.text));
    let second = detected
        .and_then(|settings| settings.second_username)
        .and_then(|form| form.apply(&address.text));
    let username = match typed.map(str::trim) {
        Some("") => return None,
        Some(typed) => typed.to_owned(),
        None => first.clone()?,
    };
    let second = (first.as_deref() == Some(username.as_str()))
        .then_some(second)
        .flatten()
        .filter(|second| *second != username);
    Some((username, second))
}

/// What the first step names: the sender name, the account's name in Sealwax and its colour.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Naming {
    pub name: String,
    pub label: String,
    pub color: Option<AccountColor>,
}

/// The settings to check and save. Without a name the address is the sender name; without a
/// label, or with the address typed as one, the address names the account.
pub fn settings(
    address: &Address,
    naming: &Naming,
    incoming: Endpoint,
    receive: Receive,
    smtp: Endpoint,
    username: String,
) -> AccountSettings {
    let name = naming.name.trim();
    let label = naming.label.trim();
    AccountSettings {
        // The engine gives a new account its random key (ML-034).
        key: AccountKey(String::new()),
        name: if name.is_empty() {
            address.text.clone()
        } else {
            name.to_owned()
        },
        label: (!label.is_empty() && !label.eq_ignore_ascii_case(&address.text))
            .then(|| label.to_owned()),
        color: naming.color,
        address: address.text.clone(),
        incoming,
        receive,
        smtp,
        username,
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    }
}

/// A manual field the assistant checks. The incoming server is IMAP's or POP3's, as chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Address,
    IncomingHost,
    IncomingPort,
    SmtpHost,
    SmtpPort,
    Username,
    Password,
}

/// Why a field isn't usable yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Nothing typed: the button waits without a message.
    Empty(Field),
    Invalid(Field),
}

impl Problem {
    pub fn field(self) -> Field {
        match self {
            Self::Empty(field) | Self::Invalid(field) => field,
        }
    }

    /// What to fix, for a field that holds something unusable; `incoming` is the incoming
    /// server's protocol, IMAP or POP3.
    pub fn message(self, incoming: Protocol) -> Option<&'static str> {
        let Self::Invalid(field) = self else {
            return None;
        };
        let pop3 = incoming == Protocol::Pop3;
        Some(match field {
            Field::Address => "An email address looks like name@example.test.",
            Field::IncomingHost if pop3 => {
                "The POP3 server is a name such as pop.example.test, in letters, digits, dots \
                 and hyphens, or an IP address."
            }
            Field::IncomingHost => {
                "The IMAP server is a name such as imap.example.test, in letters, digits, dots \
                 and hyphens, or an IP address."
            }
            Field::SmtpHost => {
                "The SMTP server is a name such as smtp.example.test, in letters, digits, dots \
                 and hyphens, or an IP address."
            }
            Field::IncomingPort if pop3 => "The POP3 port is a number from 1 to 65535.",
            Field::IncomingPort => "The IMAP port is a number from 1 to 65535.",
            Field::SmtpPort => "The SMTP port is a number from 1 to 65535.",
            Field::Username => "Type the username.",
            Field::Password => "Type the password.",
        })
    }
}

/// The manual settings as typed.
#[derive(Debug, Clone, Default)]
pub struct Manual<'a> {
    pub address: &'a str,
    /// IMAP, or POP3 with what happens to downloaded mail on its server (M8).
    pub receive: Receive,
    pub incoming_host: &'a str,
    pub incoming_port: &'a str,
    pub incoming_security: Option<Security>,
    pub smtp_host: &'a str,
    pub smtp_port: &'a str,
    pub smtp_security: Option<Security>,
    pub username: &'a str,
}

/// The checked manual settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub address: Address,
    pub incoming: Endpoint,
    pub receive: Receive,
    pub smtp: Endpoint,
    pub username: String,
}

/// Checks the fields in the order they are shown; the first problem wins.
pub fn check_manual(fields: &Manual) -> Result<Checked, Problem> {
    let address = required(fields.address, Field::Address, parse_address)?;
    let incoming_host = required(fields.incoming_host, Field::IncomingHost, parse_host)?;
    let incoming_port = required(fields.incoming_port, Field::IncomingPort, parse_port)?;
    let smtp_host = required(fields.smtp_host, Field::SmtpHost, parse_host)?;
    let smtp_port = required(fields.smtp_port, Field::SmtpPort, parse_port)?;
    let username = fields.username.trim();
    if username.is_empty() {
        return Err(Problem::Empty(Field::Username));
    }
    Ok(Checked {
        address,
        incoming: Endpoint {
            host: incoming_host,
            port: incoming_port,
            security: fields.incoming_security.unwrap_or(Security::Tls),
            certificate: None,
        },
        receive: fields.receive,
        smtp: Endpoint {
            host: smtp_host,
            port: smtp_port,
            security: fields.smtp_security.unwrap_or(Security::Tls),
            certificate: None,
        },
        username: username.to_owned(),
    })
}

/// The port to show after the incoming protocol changed from `from` to `to`: a standard
/// port of `from` (or none) moves to `to`'s for `security`; a port the user typed stays.
pub fn moved_port(port: &str, from: Protocol, to: Protocol, security: Security) -> Option<u16> {
    let standard = [Security::Tls, Security::StartTls].map(|other| default_port(from, other));
    let typed = parse_port(port);
    (port.trim().is_empty() || typed.is_some_and(|typed| standard.contains(&typed)))
        .then(|| default_port(to, security))
}

fn required<T>(text: &str, field: Field, parse: impl Fn(&str) -> Option<T>) -> Result<T, Problem> {
    if text.trim().is_empty() {
        return Err(Problem::Empty(field));
    }
    parse(text).ok_or(Problem::Invalid(field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::{DetectSource, Pop3Policy, UsernameForm};

    fn address(text: &str) -> Address {
        parse_address(text).unwrap()
    }

    fn detected(username: UsernameForm, second: Option<UsernameForm>) -> DetectedSettings {
        let endpoint = |host: &str, port| Endpoint {
            host: host.to_owned(),
            port,
            security: Security::Tls,
            certificate: None,
        };
        DetectedSettings {
            imap: endpoint("imap.example.test", 993),
            smtp: endpoint("smtp.example.test", 465),
            username,
            second_username: second,
            auth: AuthMethod::Password,
            provider: None,
            confirm: false,
            app_password: None,
            enable_imap: None,
            source: DetectSource::Autoconfig,
        }
    }

    #[test]
    fn addresses() {
        let parsed = address("  kari.nordmann@post.example.test ");
        assert_eq!(parsed.text, "kari.nordmann@post.example.test");
        assert_eq!(parsed.local, "kari.nordmann");
        assert_eq!(parsed.domain, "post.example.test");
        assert_eq!(address("ola@bølgen.example").domain, "bølgen.example");
        for bad in [
            "",
            "kari",
            "kari@",
            "@example.test",
            "kari@example",
            "kari nordmann@example.test",
            "kari@exa mple.test",
            "kari@-example.test",
            "kari@example..test",
            "kari@@example.test",
        ] {
            assert_eq!(parse_address(bad), None, "{bad}");
        }
    }

    #[test]
    fn hosts() {
        assert_eq!(
            parse_host(" imap.example.test ").as_deref(),
            Some("imap.example.test")
        );
        assert_eq!(parse_host("mail.").as_deref(), Some("mail"));
        assert_eq!(parse_host("127.0.0.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(parse_host("[::1]").as_deref(), Some("::1"));
        assert_eq!(parse_host("::1").as_deref(), Some("::1"));
        let long = format!("{}.test", "a".repeat(64));
        for bad in [
            "",
            " ",
            "imap example.test",
            "imap_example.test",
            "-imap.example.test",
            "imap-.example.test",
            "imap..example.test",
            "https://imap.example.test",
            "imap.example.test:993",
            "[imap.example.test]",
            long.as_str(),
        ] {
            assert_eq!(parse_host(bad), None, "{bad}");
        }
    }

    #[test]
    fn ports() {
        assert_eq!(parse_port("993"), Some(993));
        assert_eq!(parse_port(" 65535 "), Some(65535));
        for bad in ["", "0", "65536", "+993", "-1", "99 3", "port", "9.9"] {
            assert_eq!(parse_port(bad), None, "{bad}");
        }
        assert_eq!(default_port(Protocol::Imap, Security::Tls), 993);
        assert_eq!(default_port(Protocol::Imap, Security::StartTls), 143);
        assert_eq!(default_port(Protocol::Smtp, Security::Tls), 465);
        assert_eq!(default_port(Protocol::Smtp, Security::StartTls), 587);
    }

    #[test]
    fn detected_usernames_get_one_more_try_typed_ones_none() {
        let kari = address("kari@example.test");
        let found = detected(UsernameForm::Address, Some(UsernameForm::LocalPart));
        assert_eq!(
            usernames(&kari, Some(&found), None),
            Some(("kari@example.test".to_owned(), Some("kari".to_owned())))
        );
        // The detected username, confirmed unchanged, keeps its second try.
        assert_eq!(
            usernames(&kari, Some(&found), Some(" kari@example.test ")),
            Some(("kari@example.test".to_owned(), Some("kari".to_owned())))
        );
        assert_eq!(
            usernames(&kari, Some(&found), Some("k.nordmann")),
            Some(("k.nordmann".to_owned(), None))
        );
        assert_eq!(usernames(&kari, Some(&found), Some("  ")), None);
        let other = detected(UsernameForm::Other, None);
        assert_eq!(usernames(&kari, Some(&other), None), None);
        assert_eq!(
            usernames(&kari, Some(&other), Some("u12345")),
            Some(("u12345".to_owned(), None))
        );
        assert_eq!(
            usernames(&kari, None, Some("kari")),
            Some(("kari".to_owned(), None))
        );
        // A second form that gives the same name is no second try.
        let same = detected(UsernameForm::Address, Some(UsernameForm::Address));
        assert_eq!(
            usernames(&kari, Some(&same), None),
            Some(("kari@example.test".to_owned(), None))
        );
    }

    #[test]
    fn settings_fall_back_to_the_address_for_a_name_and_a_label() {
        let kari = address("Kari@Example.test");
        let endpoint = |host: &str, port| Endpoint {
            host: host.to_owned(),
            port,
            security: Security::StartTls,
            certificate: None,
        };
        let settings = settings(
            &kari,
            &Naming {
                name: "  ".to_owned(),
                label: " kari@example.test ".to_owned(),
                color: None,
            },
            endpoint("imap.example.test", 143),
            Receive::Imap,
            endpoint("smtp.example.test", 587),
            "kari".to_owned(),
        );
        assert_eq!(settings.name, "Kari@Example.test");
        assert_eq!(settings.label, None, "the address typed as the label");
        assert_eq!(settings.title(), "Kari@Example.test");
        assert_eq!(settings.color, None);
        assert_eq!(settings.key, AccountKey(String::new()));
        assert_eq!(settings.auth, AuthMethod::Password);
        assert_eq!(settings.receive, Receive::Imap);
        let pop3 = super::settings(
            &kari,
            &Naming {
                name: " Kari Nordmann ".to_owned(),
                label: " Work ".to_owned(),
                color: Some(AccountColor::Sage),
            },
            endpoint("pop.example.test", 110),
            Receive::Pop3(Pop3Policy::default()),
            settings.smtp.clone(),
            "kari".to_owned(),
        );
        assert_eq!(pop3.name, "Kari Nordmann");
        assert_eq!(pop3.title(), "Work");
        assert_eq!(pop3.color, Some(AccountColor::Sage));
        assert_eq!(pop3.incoming.host, "pop.example.test");
        assert_eq!(pop3.receive, Receive::Pop3(Pop3Policy::default()));
    }

    #[test]
    fn manual_fields_are_checked_in_order() {
        let mut fields = Manual {
            address: "kari@example.test",
            receive: Receive::Imap,
            incoming_host: "imap.example.test",
            incoming_port: "993",
            incoming_security: Some(Security::Tls),
            smtp_host: "smtp.example.test",
            smtp_port: "587",
            smtp_security: Some(Security::StartTls),
            username: " kari ",
        };
        let checked = check_manual(&fields).unwrap();
        assert_eq!(checked.address.text, "kari@example.test");
        assert_eq!(
            (checked.incoming.port, checked.incoming.security),
            (993, Security::Tls)
        );
        assert_eq!(checked.receive, Receive::Imap);
        assert_eq!(
            (
                checked.smtp.host.as_str(),
                checked.smtp.port,
                checked.smtp.security
            ),
            ("smtp.example.test", 587, Security::StartTls)
        );
        assert_eq!(checked.username, "kari");

        fields.smtp_port = "99999";
        fields.incoming_host = "";
        let problem = check_manual(&fields).unwrap_err();
        assert_eq!(problem, Problem::Empty(Field::IncomingHost));
        assert_eq!(problem.message(Protocol::Imap), None);
        fields.incoming_host = "imap example";
        let problem = check_manual(&fields).unwrap_err();
        assert_eq!(problem, Problem::Invalid(Field::IncomingHost));
        assert!(
            problem
                .message(Protocol::Imap)
                .unwrap()
                .starts_with("The IMAP server is a name such as imap.example.test")
        );
        assert!(
            problem
                .message(Protocol::Pop3)
                .unwrap()
                .starts_with("The POP3 server is a name such as pop.example.test")
        );
        fields.incoming_host = "imap.example.test";
        let problem = check_manual(&fields).unwrap_err();
        assert_eq!(problem, Problem::Invalid(Field::SmtpPort));
        assert_eq!(
            problem.message(Protocol::Pop3),
            Some("The SMTP port is a number from 1 to 65535.")
        );
        fields.smtp_port = "465";
        fields.incoming_port = "0";
        assert_eq!(
            check_manual(&fields).unwrap_err().message(Protocol::Pop3),
            Some("The POP3 port is a number from 1 to 65535.")
        );
        fields.incoming_port = "993";
        fields.username = "";
        assert_eq!(
            check_manual(&fields).unwrap_err(),
            Problem::Empty(Field::Username)
        );
    }

    #[test]
    fn a_pop3_account_is_checked_like_an_imap_one() {
        let policy = Pop3Policy::default();
        let fields = Manual {
            address: "kari@example.test",
            receive: Receive::Pop3(policy),
            incoming_host: " pop.example.test ",
            incoming_port: "995",
            incoming_security: Some(Security::Tls),
            smtp_host: "smtp.example.test",
            smtp_port: "465",
            smtp_security: Some(Security::Tls),
            username: "kari",
        };
        let checked = check_manual(&fields).unwrap();
        assert_eq!(checked.receive, Receive::Pop3(policy));
        assert_eq!(
            checked.incoming,
            Endpoint {
                host: "pop.example.test".to_owned(),
                port: 995,
                security: Security::Tls,
                certificate: None,
            }
        );
    }

    #[test]
    fn a_standard_port_moves_with_the_protocol_a_typed_one_stays() {
        let (imap, pop3) = (Protocol::Imap, Protocol::Pop3);
        assert_eq!(moved_port("993", imap, pop3, Security::Tls), Some(995));
        assert_eq!(moved_port("143", imap, pop3, Security::StartTls), Some(110));
        assert_eq!(moved_port(" ", imap, pop3, Security::StartTls), Some(110));
        assert_eq!(moved_port("995", pop3, imap, Security::Tls), Some(993));
        assert_eq!(moved_port("110", pop3, imap, Security::StartTls), Some(143));
        // A port of the other security moves too: the security chosen decides.
        assert_eq!(moved_port("143", imap, pop3, Security::Tls), Some(995));
        for typed in ["1995", "10993", "port"] {
            assert_eq!(
                moved_port(typed, imap, pop3, Security::Tls),
                None,
                "{typed}"
            );
        }
        assert_eq!(default_port(Protocol::Pop3, Security::Tls), 995);
        assert_eq!(default_port(Protocol::Pop3, Security::StartTls), 110);
    }
}
