//! Built-in settings for global providers (ML-031), matched by the address's domain or by the
//! MX hosts of a hosted platform. Hosts and ports were checked against the providers' help pages
//! and with TLS handshakes in S4 (2026-10-04). There are no presets for Norwegian providers: the
//! user declined them on 2026-10-04, and S4b's general rules stand in for them.

use super::{Finding, Hints, Server};
use crate::account::{
    HelpLink, OAuthProvider, Protocol, Security, UnsupportedReason, UsernameForm,
};

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub host: &'static str,
    pub port: u16,
    pub security: Security,
}

const fn tls(host: &'static str, port: u16) -> Endpoint {
    Endpoint {
        host,
        port,
        security: Security::Tls,
    }
}

const fn starttls(host: &'static str, port: u16) -> Endpoint {
    Endpoint {
        host,
        port,
        security: Security::StartTls,
    }
}

/// A page on the provider's own site.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub label: &'static str,
    pub url: &'static str,
}

impl Link {
    fn help(self) -> HelpLink {
        HelpLink {
            label: self.label.into(),
            url: self.url.into(),
        }
    }
}

#[derive(Debug)]
pub enum Kind {
    Servers {
        imap: &'static [Endpoint],
        smtp: &'static [Endpoint],
        /// The login name for both protocols.
        username: UsernameForm,
        oauth: Option<OAuthProvider>,
        /// Where to make the app password the provider requires.
        app_password: Option<Link>,
        /// Where to turn IMAP on, when the user must do it first.
        enable_imap: Option<Link>,
    },
    Unsupported(UnsupportedReason),
}

#[derive(Debug)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub domains: &'static [&'static str],
    /// MX host suffixes of the provider's hosted platform, matched on label boundaries.
    pub mx_suffixes: &'static [&'static str],
    pub kind: Kind,
}

impl Preset {
    pub fn finding(&self) -> Finding {
        match &self.kind {
            Kind::Unsupported(reason) => Finding::Unsupported {
                provider: self.name.into(),
                reason: *reason,
            },
            Kind::Servers {
                imap,
                smtp,
                username,
                oauth,
                app_password,
                enable_imap,
            } => {
                let servers = imap
                    .iter()
                    .map(|e| (Protocol::Imap, e))
                    .chain(smtp.iter().map(|e| (Protocol::Smtp, e)))
                    .map(|(protocol, e)| {
                        Server::new(protocol, e.host, e.port, e.security).with_username(*username)
                    })
                    .collect();
                Finding::Servers {
                    servers,
                    hints: Hints {
                        provider: Some(self.name.into()),
                        oauth: *oauth,
                        app_password: app_password.map(Link::help),
                        enable_imap: enable_imap.map(Link::help),
                    },
                }
            }
        }
    }

    fn endpoints(&self) -> impl Iterator<Item = &Endpoint> {
        let (imap, smtp): (&[Endpoint], &[Endpoint]) = match &self.kind {
            Kind::Servers { imap, smtp, .. } => (imap, smtp),
            Kind::Unsupported(_) => (&[], &[]),
        };
        imap.iter().chain(smtp)
    }
}

pub fn by_domain(domain: &str) -> Option<&'static Preset> {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    PRESETS
        .iter()
        .find(|p| p.domains.contains(&domain.as_str()))
}

/// The preset of the hosted platform an MX host belongs to.
pub fn by_mx(host: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| {
        p.mx_suffixes
            .iter()
            .any(|suffix| super::dns::within(host, suffix))
    })
}

/// A server name of a provider with a preset.
pub fn known_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    PRESETS
        .iter()
        .flat_map(Preset::endpoints)
        .any(|e| e.host.eq_ignore_ascii_case(host))
}

/// Microsoft's IMAP and SMTP hosts accept only OAuth (S4: Outlook.com and Microsoft 365 advertise
/// only XOAUTH2 with LOGINDISABLED).
pub fn microsoft_host(host: &str) -> bool {
    super::dns::within(host, "outlook.com") || super::dns::within(host, "office365.com")
}

/// Gmail's IMAP is always on (Google's help, read on 2026-10-05; INTEGRATIONS.md), so an "enable
/// IMAP" link for its hosts, such as the ISPDB's for gmail.com, is out of date.
pub fn imap_always_on(host: &str) -> bool {
    super::dns::within(host, "gmail.com") || super::dns::within(host, "googlemail.com")
}

const MICROSOFT: Option<OAuthProvider> = Some(OAuthProvider::Microsoft);

// The providers' own help pages, opened and checked on 2026-10-05. An app-password link is set
// only where the provider always requires one for IMAP; GMX, WEB.DE, mail.com and Zoho require
// one only with two-factor login (INTEGRATIONS.md, provider quirks).

const fn app_password(url: &'static str) -> Option<Link> {
    Some(Link {
        label: "Create an app password",
        url,
    })
}

const fn enable_imap(url: &'static str) -> Option<Link> {
    Some(Link {
        label: "Turn on IMAP",
        url,
    })
}

const ICLOUD_APP_PASSWORD: Option<Link> = app_password("https://support.apple.com/en-us/102654");
const YAHOO_APP_PASSWORD: Option<Link> = app_password("https://help.yahoo.com/kb/SLN15241.html");
const AOL_APP_PASSWORD: Option<Link> =
    app_password("https://help.aol.com/articles/create-and-manage-app-password");
const FASTMAIL_APP_PASSWORD: Option<Link> =
    app_password("https://www.fastmail.help/hc/en-us/articles/360058752854-App-passwords");
const YANDEX_APP_PASSWORD: Option<Link> =
    app_password("https://yandex.com/support/id/en/authorization/app-passwords");
const MAILRU_APP_PASSWORD: Option<Link> = app_password("https://help.mail.ru/mail/login/mailer/");
const OUTLOOK_ENABLE_IMAP: Option<Link> = enable_imap(
    "https://support.microsoft.com/en-us/office/pop-imap-and-smtp-settings-for-outlook-com-d088b986-291d-42b8-9564-9c414e2aa040",
);
const GMX_NET_ENABLE_IMAP: Option<Link> =
    enable_imap("https://hilfe.gmx.net/pop-imap/einschalten.html");
const GMX_COM_ENABLE_IMAP: Option<Link> =
    enable_imap("https://support.gmx.com/pop-imap/toggle.html");
const WEBDE_ENABLE_IMAP: Option<Link> =
    enable_imap("https://hilfe.web.de/pop-imap/einschalten.html");
const MAILCOM_ENABLE_IMAP: Option<Link> =
    enable_imap("https://support.mail.com/pop-imap/toggle.html");
const ZOHO_ENABLE_IMAP: Option<Link> =
    enable_imap("https://www.zoho.com/mail/help/imap-access.html");
const YANDEX_ENABLE_IMAP: Option<Link> =
    enable_imap("https://yandex.com/support/yandex-360/customers/mail/en/mail-clients/others");

/// Gmail isn't listed (ADR-008, 2026-10-07): a Gmail account found elsewhere connects unsupported.
pub static PRESETS: &[Preset] = &[
    Preset {
        id: "outlook",
        name: "Outlook.com",
        domains: &[
            "outlook.com",
            "hotmail.com",
            "live.com",
            "msn.com",
            "windowslive.com",
        ],
        mx_suffixes: &["olc.protection.outlook.com"],
        kind: Kind::Servers {
            imap: &[tls("outlook.office365.com", 993)],
            smtp: &[starttls("smtp-mail.outlook.com", 587)],
            username: UsernameForm::Address,
            oauth: MICROSOFT,
            app_password: None,
            enable_imap: OUTLOOK_ENABLE_IMAP,
        },
    },
    Preset {
        id: "microsoft365",
        name: "Microsoft 365",
        domains: &[],
        mx_suffixes: &["mail.protection.outlook.com", "mx.microsoft"],
        kind: Kind::Servers {
            imap: &[tls("outlook.office365.com", 993)],
            smtp: &[starttls("smtp.office365.com", 587)],
            username: UsernameForm::Address,
            oauth: MICROSOFT,
            app_password: None,
            enable_imap: None,
        },
    },
    Preset {
        id: "icloud",
        name: "iCloud",
        domains: &["icloud.com", "me.com", "mac.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.me.com", 993)],
            smtp: &[starttls("smtp.mail.me.com", 587)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: ICLOUD_APP_PASSWORD,
            enable_imap: None,
        },
    },
    Preset {
        id: "icloud-custom-domain",
        name: "iCloud",
        domains: &[],
        mx_suffixes: &["mail.icloud.com"],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.me.com", 993)],
            smtp: &[starttls("smtp.mail.me.com", 587)],
            username: UsernameForm::Other,
            oauth: None,
            app_password: ICLOUD_APP_PASSWORD,
            enable_imap: None,
        },
    },
    Preset {
        id: "yahoo",
        name: "Yahoo",
        domains: &["yahoo.com", "ymail.com", "rocketmail.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.yahoo.com", 993)],
            smtp: &[
                tls("smtp.mail.yahoo.com", 465),
                starttls("smtp.mail.yahoo.com", 587),
            ],
            username: UsernameForm::Address,
            oauth: None,
            app_password: YAHOO_APP_PASSWORD,
            enable_imap: None,
        },
    },
    Preset {
        id: "aol",
        name: "AOL",
        domains: &["aol.com", "aim.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.aol.com", 993)],
            smtp: &[tls("smtp.aol.com", 465)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: AOL_APP_PASSWORD,
            enable_imap: None,
        },
    },
    Preset {
        id: "fastmail",
        name: "Fastmail",
        domains: &["fastmail.com", "fastmail.fm"],
        mx_suffixes: &["messagingengine.com"],
        kind: Kind::Servers {
            imap: &[tls("imap.fastmail.com", 993)],
            smtp: &[
                tls("smtp.fastmail.com", 465),
                starttls("smtp.fastmail.com", 587),
            ],
            username: UsernameForm::Address,
            oauth: None,
            app_password: FASTMAIL_APP_PASSWORD,
            enable_imap: None,
        },
    },
    Preset {
        id: "proton",
        name: "Proton Mail",
        domains: &["proton.me", "protonmail.com", "protonmail.ch", "pm.me"],
        mx_suffixes: &["protonmail.ch"],
        // Proton Mail Bridge on this computer, as Proton's own autoconfig names it (checked
        // 2026-10-13); its self-signed certificate is pinned when the user trusts it (issue #9).
        kind: Kind::Servers {
            imap: &[starttls("127.0.0.1", 1143)],
            smtp: &[starttls("127.0.0.1", 1025)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: Some(Link {
                label: "Get Proton Mail Bridge",
                url: "https://proton.me/mail/bridge",
            }),
            enable_imap: None,
        },
    },
    Preset {
        id: "tuta",
        name: "Tuta",
        domains: &[
            "tuta.com",
            "tutanota.com",
            "tutanota.de",
            "tutamail.com",
            "tuta.io",
            "keemail.me",
        ],
        mx_suffixes: &["tutanota.de"],
        kind: Kind::Unsupported(UnsupportedReason::NoImap),
    },
    Preset {
        id: "gmx-net",
        name: "GMX",
        domains: &["gmx.net", "gmx.de", "gmx.at", "gmx.ch"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.gmx.net", 993)],
            smtp: &[tls("mail.gmx.net", 465), starttls("mail.gmx.net", 587)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: GMX_NET_ENABLE_IMAP,
        },
    },
    Preset {
        id: "gmx-com",
        name: "GMX",
        domains: &["gmx.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.gmx.com", 993)],
            smtp: &[tls("mail.gmx.com", 465), starttls("mail.gmx.com", 587)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: GMX_COM_ENABLE_IMAP,
        },
    },
    Preset {
        id: "webde",
        name: "WEB.DE",
        domains: &["web.de"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.web.de", 993)],
            smtp: &[tls("smtp.web.de", 465), starttls("smtp.web.de", 587)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: WEBDE_ENABLE_IMAP,
        },
    },
    Preset {
        id: "mailcom",
        name: "mail.com",
        domains: &["mail.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.com", 993)],
            smtp: &[tls("smtp.mail.com", 465), starttls("smtp.mail.com", 587)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: MAILCOM_ENABLE_IMAP,
        },
    },
    Preset {
        id: "zoho",
        name: "Zoho Mail",
        domains: &["zoho.com", "zohomail.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.zoho.com", 993)],
            smtp: &[tls("smtp.zoho.com", 465), starttls("smtp.zoho.com", 587)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: ZOHO_ENABLE_IMAP,
        },
    },
    Preset {
        id: "zoho-org-eu",
        name: "Zoho Mail",
        domains: &[],
        mx_suffixes: &["zoho.eu"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.eu", 993)],
            smtp: &[
                tls("smtppro.zoho.eu", 465),
                starttls("smtppro.zoho.eu", 587),
            ],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: ZOHO_ENABLE_IMAP,
        },
    },
    Preset {
        id: "zoho-org-in",
        name: "Zoho Mail",
        domains: &[],
        mx_suffixes: &["zoho.in"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.in", 993)],
            smtp: &[
                tls("smtppro.zoho.in", 465),
                starttls("smtppro.zoho.in", 587),
            ],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: ZOHO_ENABLE_IMAP,
        },
    },
    Preset {
        id: "zoho-org-au",
        name: "Zoho Mail",
        domains: &[],
        mx_suffixes: &["zoho.com.au"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.com.au", 993)],
            smtp: &[
                tls("smtppro.zoho.com.au", 465),
                starttls("smtppro.zoho.com.au", 587),
            ],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: ZOHO_ENABLE_IMAP,
        },
    },
    Preset {
        id: "zoho-org",
        name: "Zoho Mail",
        domains: &[],
        mx_suffixes: &["zoho.com"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.com", 993)],
            smtp: &[
                tls("smtppro.zoho.com", 465),
                starttls("smtppro.zoho.com", 587),
            ],
            username: UsernameForm::Address,
            oauth: None,
            app_password: None,
            enable_imap: ZOHO_ENABLE_IMAP,
        },
    },
    Preset {
        id: "yandex",
        name: "Yandex Mail",
        domains: &["yandex.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.yandex.com", 993)],
            smtp: &[
                tls("smtp.yandex.com", 465),
                starttls("smtp.yandex.com", 587),
            ],
            username: UsernameForm::LocalPart,
            oauth: None,
            app_password: YANDEX_APP_PASSWORD,
            enable_imap: YANDEX_ENABLE_IMAP,
        },
    },
    Preset {
        id: "mailru",
        name: "Mail.ru",
        domains: &["mail.ru", "inbox.ru", "list.ru", "bk.ru"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.ru", 993)],
            smtp: &[tls("smtp.mail.ru", 465)],
            username: UsernameForm::Address,
            oauth: None,
            app_password: MAILRU_APP_PASSWORD,
            enable_imap: None,
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_domains_exactly() {
        assert_eq!(by_domain("Hotmail.com.").map(|p| p.id), Some("outlook"));
        assert!(by_domain("mail.hotmail.com").is_none());
        assert!(by_domain("example.com").is_none());
    }

    #[test]
    fn gmail_is_not_listed() {
        assert!(by_domain("gmail.com").is_none());
        assert!(by_domain("googlemail.com").is_none());
        assert!(by_mx("aspmx.l.google.com.").is_none());
    }

    #[test]
    fn matches_mx_on_label_boundaries() {
        for (mx, id) in [
            ("example-com.mail.protection.outlook.com", "microsoft365"),
            ("example-com.m-v1.mx.microsoft.", "microsoft365"),
            ("eur.olc.protection.outlook.com", "outlook"),
            ("in1-smtp.messagingengine.com", "fastmail"),
            ("mx01.mail.icloud.com", "icloud-custom-domain"),
            ("mx.zoho.eu", "zoho-org-eu"),
            ("mx.zoho.com", "zoho-org"),
            ("mx.zoho.com.au", "zoho-org-au"),
            ("mail.protonmail.ch", "proton"),
            ("mail.tutanota.de", "tuta"),
        ] {
            assert_eq!(by_mx(mx).map(|p| p.id), Some(id), "{mx}");
        }
        assert!(by_mx("notmessagingengine.com").is_none());
        assert!(by_mx("messagingengine.com.evil.example").is_none());
    }

    #[test]
    fn proton_is_its_bridge_on_this_computer() {
        let Finding::Servers { servers, hints } = by_mx("mail.protonmail.ch").unwrap().finding()
        else {
            panic!("Proton has servers");
        };
        let endpoints: Vec<_> = servers
            .iter()
            .map(|server| {
                (
                    server.protocol,
                    server.host.as_str(),
                    server.port,
                    server.security,
                )
            })
            .collect();
        assert_eq!(
            endpoints,
            [
                (Protocol::Imap, "127.0.0.1", 1143, Security::StartTls),
                (Protocol::Smtp, "127.0.0.1", 1025, Security::StartTls),
            ]
        );
        assert_eq!(hints.provider.as_deref(), Some("Proton Mail"));
        assert_eq!(
            hints.app_password.map(|link| link.url).as_deref(),
            Some("https://proton.me/mail/bridge")
        );
        assert_eq!(
            by_domain("proton.me").unwrap().finding(),
            by_domain("pm.me").unwrap().finding()
        );
    }

    #[test]
    fn unsupported_presets_say_why() {
        assert!(matches!(
            by_domain("tutanota.com").unwrap().finding(),
            Finding::Unsupported {
                reason: UnsupportedReason::NoImap,
                ..
            }
        ));
    }

    #[test]
    fn every_preset_offers_both_protocols_and_https_links() {
        for preset in PRESETS {
            if let Kind::Servers {
                imap,
                smtp,
                app_password,
                enable_imap,
                ..
            } = &preset.kind
            {
                assert!(!imap.is_empty() && !smtp.is_empty(), "{}", preset.id);
                for link in [app_password, enable_imap].into_iter().flatten() {
                    assert!(link.url.starts_with("https://"), "{}", preset.id);
                    assert!(!link.label.is_empty(), "{}", preset.id);
                }
            }
        }
    }

    #[test]
    fn microsoft_presets_sign_in_with_oauth() {
        for id in ["outlook", "microsoft365"] {
            let preset = PRESETS.iter().find(|p| p.id == id).unwrap();
            let Finding::Servers { hints, servers } = preset.finding() else {
                panic!("{id}");
            };
            assert_eq!(hints.oauth, Some(OAuthProvider::Microsoft));
            assert!(servers.iter().all(|s| microsoft_host(&s.host)));
        }
        assert!(!microsoft_host("imap.gmail.com"));
        assert!(!microsoft_host("outlook.com.evil.test"));
    }

    #[test]
    fn help_links_point_at_the_providers_own_pages() {
        let hints = |domain: &str| match by_domain(domain).unwrap().finding() {
            Finding::Servers { hints, .. } => hints,
            other => panic!("{domain}: {other:?}"),
        };
        for (domain, site) in [
            ("icloud.com", "support.apple.com"),
            ("yahoo.com", "help.yahoo.com"),
            ("fastmail.com", "www.fastmail.help"),
        ] {
            let link = hints(domain).app_password.expect(domain);
            assert!(
                link.url.starts_with(&format!("https://{site}/")),
                "{domain}"
            );
        }
        for (domain, site) in [
            ("gmx.net", "hilfe.gmx.net"),
            ("web.de", "hilfe.web.de"),
            ("outlook.com", "support.microsoft.com"),
        ] {
            let link = hints(domain).enable_imap.expect(domain);
            assert!(
                link.url.starts_with(&format!("https://{site}/")),
                "{domain}"
            );
            assert_eq!(hints(domain).app_password, None, "{domain}");
        }
    }

    #[test]
    fn known_hosts_are_preset_hosts() {
        assert!(known_host("imap.fastmail.com"));
        assert!(known_host("SMTP.office365.com."));
        assert!(!known_host("imap.example.com"));
    }

    #[test]
    fn no_preset_is_keyed_to_a_norwegian_provider() {
        const NAMES: &[&str] = &[
            "telenor",
            "online.no",
            "altibox",
            "lyse",
            "domeneshop",
            "nextgentel",
            "broadpark",
            "tussa",
            "telia",
            "get.no",
            "one.com",
            "proisp",
            "uniweb",
            "webhuset",
            "domene",
        ];
        for preset in PRESETS {
            let mut keys: Vec<&str> = preset.domains.to_vec();
            keys.extend(preset.mx_suffixes);
            keys.extend(preset.endpoints().map(|e| e.host));
            for key in keys {
                assert!(!key.ends_with(".no"), "{}: {key}", preset.id);
                assert!(
                    NAMES.iter().all(|name| !key.contains(name)),
                    "{}: {key}",
                    preset.id
                );
            }
        }
    }
}
