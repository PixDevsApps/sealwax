//! Autodetect's pure half (ML-030, ML-031): what each source found, the provider presets, the
//! rules that rank the sources, and the clientConfig parser. The engine makes the lookups and
//! hands their answers here; nothing in this module does I/O.
//!
//! Ported from spikes S4 and S4b. The source order and the rules R1–R3 are S4b's; the ISPDB above
//! SRV, verified SRV endpoints and Microsoft 365 found through `autodiscover` are M2's.

pub mod clientconfig;
pub mod dns;
pub mod presets;
pub mod probing;
pub mod rank;
pub mod redirect;

use crate::account::{
    AuthMethod, DetectSource, DetectedSettings, Detection, Endpoint, HelpLink, OAuthProvider,
    Protocol, Security, UnsupportedReason, UsernameForm,
};
use rank::{Choice, Outcome};

/// One server a source offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: UsernameForm,
}

impl Server {
    pub fn new(protocol: Protocol, host: &str, port: u16, security: Security) -> Self {
        Self {
            protocol,
            host: host.trim_end_matches('.').to_ascii_lowercase(),
            port,
            security,
            username: UsernameForm::Address,
        }
    }

    pub fn with_username(mut self, username: UsernameForm) -> Self {
        self.username = username;
        self
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint {
            host: self.host.clone(),
            port: self.port,
            security: self.security,
        }
    }
}

/// Where a finding came from. `Rules::order` ranks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// A built-in preset for the address's domain (offline).
    Preset,
    /// The domain's own HTTPS autoconfig: the only request that carries the address.
    Autoconfig,
    /// Mozilla's ISPDB, asked for the domain.
    Ispdb,
    /// DNS SRV records (RFC 6186, RFC 8314).
    Srv,
    /// R1: a verified `imap.<domain>` under the user's own domain.
    OwnHost,
    /// The MX records matched to a hosted platform's preset, or Microsoft 365 behind the
    /// domain's own MX gateway, found through its `autodiscover` record.
    MxPreset,
    /// `/.well-known/autoconfig` on the bare domain.
    WellKnown,
    /// R3: the MX operator's own autoconfig, asked without an address.
    MxAutoconfig,
    /// The ISPDB, asked for the MX operator's domain.
    MxIspdb,
    /// TLS checks of likely names under the user's domain.
    Probe,
    /// R3: TLS checks of the MX operator's `imap.`, `smtp.` and `mail.` names.
    MxProbe,
}

impl Source {
    pub const ALL: [Source; 11] = [
        Source::Preset,
        Source::Autoconfig,
        Source::Ispdb,
        Source::Srv,
        Source::OwnHost,
        Source::MxPreset,
        Source::WellKnown,
        Source::MxAutoconfig,
        Source::MxIspdb,
        Source::Probe,
        Source::MxProbe,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Source::Preset => "preset",
            Source::Autoconfig => "autoconfig",
            Source::Ispdb => "ispdb",
            Source::Srv => "srv",
            Source::OwnHost => "own-host",
            Source::MxPreset => "mx-preset",
            Source::WellKnown => "well-known",
            Source::MxAutoconfig => "mx-autoconfig",
            Source::MxIspdb => "mx-ispdb",
            Source::Probe => "probe",
            Source::MxProbe => "mx-probe",
        }
    }

    /// The name the assistant shows for this source.
    pub fn detect_source(self) -> DetectSource {
        match self {
            Source::Preset => DetectSource::Preset,
            Source::Autoconfig | Source::WellKnown => DetectSource::Autoconfig,
            Source::Ispdb => DetectSource::Ispdb,
            Source::Srv => DetectSource::Srv,
            Source::MxPreset | Source::MxAutoconfig | Source::MxIspdb => DetectSource::Mx,
            Source::OwnHost | Source::Probe | Source::MxProbe => DetectSource::Probe,
        }
    }

    /// Hosts found here sit at the MX operator and nowhere else (the M0 gate's confirmation rule).
    pub fn is_mx_operator(self) -> bool {
        matches!(self, Source::MxAutoconfig | Source::MxProbe)
    }
}

/// Switches for the rules S4's pipeline didn't have, so a measurement can turn each one off.
/// Production uses `Rules::default()`, which turns every rule on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    /// R1: a verified `imap.<domain>` outranks everything inferred from the MX records.
    pub own_host: bool,
    /// R2: an SRV target takes the conventional `imap.`/`smtp.` name when it is the same server.
    pub srv_conventional: bool,
    /// R3: the MX operator's own autoconfig and hosts are asked when nothing else answers.
    pub mx_hosts: bool,
    /// The ISPDB outranks SRV records.
    pub ispdb_before_srv: bool,
    /// SRV endpoints pass a TLS check before they are offered, and `_submission` on 465 or
    /// `_imap` on 993 is read as implicit TLS.
    pub verify_srv: bool,
    /// An `autodiscover.<domain>` alias of Microsoft's means Microsoft 365 when the MX records
    /// match no preset.
    pub autodiscover: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            own_host: true,
            srv_conventional: true,
            mx_hosts: true,
            ispdb_before_srv: true,
            verify_srv: true,
            autodiscover: true,
        }
    }
}

impl Rules {
    /// The sources, highest rank first.
    pub fn order(&self) -> [Source; 11] {
        let mut order = Source::ALL;
        if !self.ispdb_before_srv {
            order.swap(2, 3);
        }
        order
    }
}

/// What the assistant needs to know besides the servers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    pub provider: Option<String>,
    pub oauth: Option<OAuthProvider>,
    pub app_password: Option<HelpLink>,
    pub enable_imap: Option<HelpLink>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    Servers {
        servers: Vec<Server>,
        hints: Hints,
    },
    Unsupported {
        provider: String,
        reason: UnsupportedReason,
    },
    /// A null MX (RFC 7505), or a domain that doesn't exist.
    NoMail,
    /// Nothing usable, or the lookup failed.
    Nothing,
}

/// One source's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub source: Source,
    pub finding: Finding,
}

/// The address's domain, lower-cased and without a trailing dot; `None` unless the address has a
/// local part and a domain.
pub fn address_domain(address: &str) -> Option<String> {
    let (local, domain) = address.trim().rsplit_once('@')?;
    let domain = domain.trim_end_matches('.').to_lowercase();
    (!local.is_empty() && !domain.is_empty()).then_some(domain)
}

/// What the assistant gets from a settled outcome; `domain` is the address's domain.
pub fn detection(outcome: &Outcome, domain: &str) -> Detection {
    match outcome {
        Outcome::NoMail { .. } => Detection::NoMail,
        Outcome::Unsupported {
            provider, reason, ..
        } => Detection::Unsupported {
            provider: provider.clone(),
            reason: *reason,
        },
        Outcome::Settings {
            imap: Some(imap),
            smtp: Some(smtp),
        } => Detection::Found(settings(imap, smtp, domain)),
        Outcome::Settings { .. } => Detection::NotFound,
    }
}

fn settings(imap: &Choice, smtp: &Choice, domain: &str) -> DetectedSettings {
    let oauth = imap
        .hints
        .oauth
        .or_else(|| presets::microsoft_host(&imap.server.host).then_some(OAuthProvider::Microsoft));
    let auth = oauth.map_or(AuthMethod::Password, AuthMethod::OAuth);
    let username = match (imap.server.username, smtp.server.username) {
        (UsernameForm::Other, _) | (_, UsernameForm::Other) => UsernameForm::Other,
        (form, _) => form,
    };
    let second_username = match (auth, username) {
        (AuthMethod::OAuth(_), _) | (_, UsernameForm::Other) => None,
        (_, UsernameForm::Address) => Some(UsernameForm::LocalPart),
        (_, UsernameForm::LocalPart) => Some(UsernameForm::Address),
    };
    DetectedSettings {
        imap: imap.server.endpoint(),
        smtp: smtp.server.endpoint(),
        username,
        second_username,
        auth,
        provider: imap.hints.provider.clone(),
        confirm: needs_confirmation(imap, domain) || needs_confirmation(smtp, domain),
        app_password: imap.hints.app_password.clone(),
        enable_imap: imap
            .hints
            .enable_imap
            .clone()
            .filter(|_| !presets::imap_always_on(&imap.server.host)),
        source: imap.source.detect_source(),
    }
}

/// The password goes to a host that only unauthenticated DNS pointed to: the MX operator's hosts
/// (M0 gate decision), or an SRV or MX-derived host outside the user's domain that no preset
/// knows (S4's recommendation, INTEGRATIONS.md).
fn needs_confirmation(choice: &Choice, domain: &str) -> bool {
    match choice.source {
        source if source.is_mx_operator() => true,
        Source::Srv | Source::MxIspdb => {
            !dns::within(&choice.server.host, domain) && !presets::known_host(&choice.server.host)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(source: Source, protocol: Protocol, host: &str, username: UsernameForm) -> Choice {
        let (port, security) = match protocol {
            Protocol::Imap => (993, Security::Tls),
            Protocol::Smtp => (465, Security::Tls),
            Protocol::Pop3 => (995, Security::Tls),
        };
        Choice {
            server: Server::new(protocol, host, port, security).with_username(username),
            source,
            hints: Hints::default(),
        }
    }

    fn found(imap: Choice, smtp: Choice, domain: &str) -> DetectedSettings {
        let outcome = Outcome::Settings {
            imap: Some(Box::new(imap)),
            smtp: Some(Box::new(smtp)),
        };
        match detection(&outcome, domain) {
            Detection::Found(settings) => settings,
            other => panic!("expected settings, got {other:?}"),
        }
    }

    #[test]
    fn the_second_username_is_the_other_form() {
        let settings = found(
            choice(
                Source::Ispdb,
                Protocol::Imap,
                "imap.example.test",
                UsernameForm::Address,
            ),
            choice(
                Source::Ispdb,
                Protocol::Smtp,
                "smtp.example.test",
                UsernameForm::Address,
            ),
            "example.test",
        );
        assert_eq!(settings.username, UsernameForm::Address);
        assert_eq!(settings.second_username, Some(UsernameForm::LocalPart));
        assert_eq!(settings.auth, AuthMethod::Password);
        assert_eq!(settings.source, DetectSource::Ispdb);
        assert!(!settings.confirm);

        let local = found(
            choice(
                Source::Preset,
                Protocol::Imap,
                "imap.example.test",
                UsernameForm::LocalPart,
            ),
            choice(
                Source::Preset,
                Protocol::Smtp,
                "smtp.example.test",
                UsernameForm::LocalPart,
            ),
            "example.test",
        );
        assert_eq!(local.second_username, Some(UsernameForm::Address));
    }

    #[test]
    fn an_underivable_username_has_no_second_attempt() {
        let settings = found(
            choice(
                Source::MxPreset,
                Protocol::Imap,
                "imap.mail.me.com",
                UsernameForm::Other,
            ),
            choice(
                Source::MxPreset,
                Protocol::Smtp,
                "smtp.mail.me.com",
                UsernameForm::Address,
            ),
            "example.test",
        );
        assert_eq!(settings.username, UsernameForm::Other);
        assert_eq!(settings.second_username, None);
    }

    #[test]
    fn microsoft_hosts_sign_in_with_oauth() {
        let settings = found(
            choice(
                Source::Ispdb,
                Protocol::Imap,
                "outlook.office365.com",
                UsernameForm::Address,
            ),
            choice(
                Source::Ispdb,
                Protocol::Smtp,
                "smtp.office365.com",
                UsernameForm::Address,
            ),
            "example.test",
        );
        assert_eq!(settings.auth, AuthMethod::OAuth(OAuthProvider::Microsoft));
        assert_eq!(settings.second_username, None);
    }

    #[test]
    fn gmail_gets_no_turn_on_imap_link() {
        let link = HelpLink {
            label: "Turn on IMAP".into(),
            url: "https://example.test/imap".into(),
        };
        let found_with_link = |imap: &str, smtp: &str, domain: &str| {
            let imap = Choice {
                hints: Hints {
                    enable_imap: Some(link.clone()),
                    ..Hints::default()
                },
                ..choice(Source::Ispdb, Protocol::Imap, imap, UsernameForm::Address)
            };
            let smtp = choice(Source::Ispdb, Protocol::Smtp, smtp, UsernameForm::Address);
            found(imap, smtp, domain).enable_imap
        };
        assert_eq!(
            found_with_link("imap.gmail.com", "smtp.gmail.com", "gmail.com"),
            None
        );
        assert_eq!(
            found_with_link(
                "IMAP.GoogleMail.com.",
                "smtp.googlemail.com",
                "googlemail.com"
            ),
            None
        );
        assert_eq!(
            found_with_link("imap.example.test", "smtp.example.test", "example.test"),
            Some(link.clone())
        );
        assert_eq!(
            found_with_link("imap.notgmail.com", "smtp.notgmail.com", "notgmail.com"),
            Some(link)
        );
    }

    #[test]
    fn the_mx_operators_hosts_are_confirmed() {
        let settings = found(
            choice(
                Source::MxProbe,
                Protocol::Imap,
                "imap.host.test",
                UsernameForm::Address,
            ),
            choice(
                Source::MxProbe,
                Protocol::Smtp,
                "smtp.host.test",
                UsernameForm::Address,
            ),
            "example.test",
        );
        assert!(settings.confirm);
        assert_eq!(settings.source, DetectSource::Probe);
        assert_eq!(settings.username, UsernameForm::Address);
    }

    #[test]
    fn dns_derived_hosts_outside_the_domain_are_confirmed_unless_known() {
        let outside = found(
            choice(
                Source::Srv,
                Protocol::Imap,
                "mail.host.test",
                UsernameForm::Address,
            ),
            choice(
                Source::Srv,
                Protocol::Smtp,
                "mail.host.test",
                UsernameForm::Address,
            ),
            "example.test",
        );
        assert!(outside.confirm);
        let inside = found(
            choice(
                Source::Srv,
                Protocol::Imap,
                "imap.example.test",
                UsernameForm::Address,
            ),
            choice(
                Source::Srv,
                Protocol::Smtp,
                "smtp.example.test",
                UsernameForm::Address,
            ),
            "example.test",
        );
        assert!(!inside.confirm);
        let known = found(
            choice(
                Source::Srv,
                Protocol::Imap,
                "imap.fastmail.com",
                UsernameForm::Address,
            ),
            choice(
                Source::Srv,
                Protocol::Smtp,
                "smtp.fastmail.com",
                UsernameForm::Address,
            ),
            "example.test",
        );
        assert!(!known.confirm);
    }

    #[test]
    fn partial_settings_are_not_found() {
        let outcome = Outcome::Settings {
            imap: Some(Box::new(choice(
                Source::Probe,
                Protocol::Imap,
                "imap.example.test",
                UsernameForm::Address,
            ))),
            smtp: None,
        };
        assert_eq!(detection(&outcome, "example.test"), Detection::NotFound);
    }

    #[test]
    fn address_domains() {
        assert_eq!(
            address_domain("test@Example.TEST.").as_deref(),
            Some("example.test")
        );
        assert_eq!(address_domain("@example.test"), None);
        assert_eq!(address_domain("test@"), None);
        assert_eq!(address_domain("no-at-sign"), None);
    }

    #[test]
    fn the_ispdb_outranks_srv_unless_switched_off() {
        let order = Rules::default().order();
        assert_eq!(order[2..4], [Source::Ispdb, Source::Srv]);
        let off = Rules {
            ispdb_before_srv: false,
            ..Rules::default()
        };
        assert_eq!(off.order()[2..4], [Source::Srv, Source::Ispdb]);
    }
}
