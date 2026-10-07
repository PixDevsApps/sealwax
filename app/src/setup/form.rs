//! The assistant's fields as values (PRD A4): the address, server names, ports and the
//! usernames to try, checked before anything goes to the engine.

use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, DetectedSettings, Endpoint, Protocol, Security,
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

/// The settings to check and save. Without a name the address labels the account.
pub fn settings(
    address: &Address,
    name: &str,
    imap: Endpoint,
    smtp: Endpoint,
    username: String,
) -> AccountSettings {
    let name = name.trim();
    AccountSettings {
        // The engine gives a new account its random key (ML-034).
        key: AccountKey(String::new()),
        name: if name.is_empty() {
            address.text.clone()
        } else {
            name.to_owned()
        },
        address: address.text.clone(),
        incoming: imap,
        receive: sealwax_domain::account::Receive::Imap,
        smtp,
        username,
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    }
}

/// A manual field the assistant checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Address,
    ImapHost,
    ImapPort,
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

    /// What to fix, for a field that holds something unusable.
    pub fn message(self) -> Option<&'static str> {
        let Self::Invalid(field) = self else {
            return None;
        };
        Some(match field {
            Field::Address => "An email address looks like name@example.test.",
            Field::ImapHost => {
                "The IMAP server is a name such as imap.example.test, in letters, digits, dots \
                 and hyphens, or an IP address."
            }
            Field::SmtpHost => {
                "The SMTP server is a name such as smtp.example.test, in letters, digits, dots \
                 and hyphens, or an IP address."
            }
            Field::ImapPort => "The IMAP port is a number from 1 to 65535.",
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
    pub imap_host: &'a str,
    pub imap_port: &'a str,
    pub imap_security: Option<Security>,
    pub smtp_host: &'a str,
    pub smtp_port: &'a str,
    pub smtp_security: Option<Security>,
    pub username: &'a str,
}

/// The checked manual settings: the address, both servers and the username.
pub type Checked = (Address, Endpoint, Endpoint, String);

/// Checks the fields in the order they are shown; the first problem wins.
pub fn check_manual(fields: &Manual) -> Result<Checked, Problem> {
    let address = required(fields.address, Field::Address, parse_address)?;
    let imap_host = required(fields.imap_host, Field::ImapHost, parse_host)?;
    let imap_port = required(fields.imap_port, Field::ImapPort, parse_port)?;
    let smtp_host = required(fields.smtp_host, Field::SmtpHost, parse_host)?;
    let smtp_port = required(fields.smtp_port, Field::SmtpPort, parse_port)?;
    let username = fields.username.trim();
    if username.is_empty() {
        return Err(Problem::Empty(Field::Username));
    }
    Ok((
        address,
        Endpoint {
            host: imap_host,
            port: imap_port,
            security: fields.imap_security.unwrap_or(Security::Tls),
        },
        Endpoint {
            host: smtp_host,
            port: smtp_port,
            security: fields.smtp_security.unwrap_or(Security::Tls),
        },
        username.to_owned(),
    ))
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
    use sealwax_domain::account::{DetectSource, UsernameForm};

    fn address(text: &str) -> Address {
        parse_address(text).unwrap()
    }

    fn detected(username: UsernameForm, second: Option<UsernameForm>) -> DetectedSettings {
        let endpoint = |host: &str, port| Endpoint {
            host: host.to_owned(),
            port,
            security: Security::Tls,
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
    fn settings_fall_back_to_the_address_for_a_name() {
        let kari = address("Kari@Example.test");
        let endpoint = |host: &str, port| Endpoint {
            host: host.to_owned(),
            port,
            security: Security::StartTls,
        };
        let settings = settings(
            &kari,
            "  ",
            endpoint("imap.example.test", 143),
            endpoint("smtp.example.test", 587),
            "kari".to_owned(),
        );
        assert_eq!(settings.name, "Kari@Example.test");
        assert_eq!(settings.key, AccountKey(String::new()));
        assert_eq!(settings.auth, AuthMethod::Password);
        assert_eq!(
            super::settings(
                &kari,
                " Kari Nordmann ",
                settings.incoming.clone(),
                settings.smtp.clone(),
                "kari".to_owned()
            )
            .name,
            "Kari Nordmann"
        );
    }

    #[test]
    fn manual_fields_are_checked_in_order() {
        let mut fields = Manual {
            address: "kari@example.test",
            imap_host: "imap.example.test",
            imap_port: "993",
            imap_security: Some(Security::Tls),
            smtp_host: "smtp.example.test",
            smtp_port: "587",
            smtp_security: Some(Security::StartTls),
            username: " kari ",
        };
        let (address, imap, smtp, username) = check_manual(&fields).unwrap();
        assert_eq!(address.text, "kari@example.test");
        assert_eq!((imap.port, imap.security), (993, Security::Tls));
        assert_eq!(
            (smtp.host.as_str(), smtp.port, smtp.security),
            ("smtp.example.test", 587, Security::StartTls)
        );
        assert_eq!(username, "kari");

        fields.smtp_port = "99999";
        fields.imap_host = "";
        let problem = check_manual(&fields).unwrap_err();
        assert_eq!(problem, Problem::Empty(Field::ImapHost));
        assert_eq!(problem.message(), None);
        fields.imap_host = "imap example";
        let problem = check_manual(&fields).unwrap_err();
        assert_eq!(problem, Problem::Invalid(Field::ImapHost));
        assert!(problem.message().unwrap().contains("IMAP server"));
        fields.imap_host = "imap.example.test";
        let problem = check_manual(&fields).unwrap_err();
        assert_eq!(problem, Problem::Invalid(Field::SmtpPort));
        assert_eq!(
            problem.message(),
            Some("The SMTP port is a number from 1 to 65535.")
        );
        fields.smtp_port = "465";
        fields.username = "";
        assert_eq!(
            check_manual(&fields).unwrap_err(),
            Problem::Empty(Field::Username)
        );
    }
}
