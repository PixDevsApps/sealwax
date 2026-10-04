use crate::model::{Auth, Finding, Hints, Protocol, Security, Server, Unsupported, Username};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum User {
    Address,
    LocalPart,
    NotDerivable(&'static str),
}

impl User {
    fn username(self) -> Username {
        match self {
            User::Address => Username::Address,
            User::LocalPart => Username::LocalPart,
            User::NotDerivable(what) => Username::Template(what.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub host: &'static str,
    pub port: u16,
    pub security: Security,
    pub user: User,
}

const fn tls(host: &'static str, port: u16, user: User) -> Endpoint {
    Endpoint {
        host,
        port,
        security: Security::Tls,
        user,
    }
}

const fn starttls(host: &'static str, port: u16, user: User) -> Endpoint {
    Endpoint {
        host,
        port,
        security: Security::StartTls,
        user,
    }
}

#[derive(Debug)]
pub enum Kind {
    Servers {
        imap: &'static [Endpoint],
        smtp: &'static [Endpoint],
        oauth: Option<&'static str>,
        app_password: bool,
        enable_imap: bool,
    },
    Unsupported(Unsupported),
}

#[derive(Debug)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub domains: &'static [&'static str],
    pub mx_suffixes: &'static [&'static str],
    pub kind: Kind,
}

impl Preset {
    pub fn finding(&self) -> Finding {
        match &self.kind {
            Kind::Unsupported(why) => Finding::Unsupported {
                provider: self.name,
                why: *why,
            },
            Kind::Servers {
                imap,
                smtp,
                oauth,
                app_password,
                enable_imap,
            } => {
                let auth: &[Auth] = if oauth.is_some() && !app_password {
                    &[Auth::OAuth2]
                } else if oauth.is_some() {
                    &[Auth::OAuth2, Auth::Password]
                } else {
                    &[Auth::Password]
                };
                let servers = imap
                    .iter()
                    .map(|e| (Protocol::Imap, e))
                    .chain(smtp.iter().map(|e| (Protocol::Smtp, e)))
                    .map(|(protocol, e)| {
                        Server::new(protocol, e.host, e.port, e.security)
                            .with_username(e.user.username())
                            .with_auth(auth)
                    })
                    .collect();
                Finding::Servers {
                    servers,
                    hints: Hints {
                        provider: Some(self.name.to_string()),
                        oauth: oauth.map(str::to_string),
                        app_password: *app_password,
                        enable_imap: *enable_imap,
                    },
                }
            }
        }
    }
}

fn tables(local: bool) -> impl Iterator<Item = &'static Preset> {
    let local: &'static [Preset] = if local { LOCAL } else { &[] };
    PRESETS.iter().chain(local)
}

pub fn by_domain(domain: &str, local: bool) -> Option<&'static Preset> {
    tables(local).find(|p| p.domains.contains(&domain))
}

pub fn by_mx(host: &str, local: bool) -> Option<&'static Preset> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    tables(local).find(|p| {
        p.mx_suffixes
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
    })
}

const GOOGLE_OAUTH: Option<&str> = Some("google");
const MICROSOFT_OAUTH: Option<&str> = Some("microsoft");

pub static PRESETS: &[Preset] = &[
    Preset {
        id: "google",
        name: "Google",
        domains: &["gmail.com", "googlemail.com"],
        mx_suffixes: &["google.com", "googlemail.com"],
        kind: Kind::Servers {
            imap: &[tls("imap.gmail.com", 993, User::Address)],
            smtp: &[
                tls("smtp.gmail.com", 465, User::Address),
                starttls("smtp.gmail.com", 587, User::Address),
            ],
            oauth: GOOGLE_OAUTH,
            app_password: true,
            enable_imap: false,
        },
    },
    Preset {
        id: "outlook",
        name: "Outlook.com",
        domains: &[
            "outlook.com",
            "hotmail.com",
            "live.com",
            "msn.com",
            "windowslive.com",
            "hotmail.no",
            "live.no",
            "hotmail.se",
            "live.se",
            "hotmail.dk",
            "live.dk",
            "outlook.dk",
            "hotmail.co.uk",
            "live.co.uk",
            "hotmail.de",
            "outlook.de",
            "hotmail.fr",
            "outlook.fr",
            "live.fr",
        ],
        mx_suffixes: &["olc.protection.outlook.com"],
        kind: Kind::Servers {
            imap: &[tls("outlook.office365.com", 993, User::Address)],
            smtp: &[starttls("smtp-mail.outlook.com", 587, User::Address)],
            oauth: MICROSOFT_OAUTH,
            app_password: false,
            enable_imap: false,
        },
    },
    Preset {
        id: "microsoft365",
        name: "Microsoft 365",
        domains: &[],
        mx_suffixes: &["mail.protection.outlook.com", "mx.microsoft"],
        kind: Kind::Servers {
            imap: &[tls("outlook.office365.com", 993, User::Address)],
            smtp: &[starttls("smtp.office365.com", 587, User::Address)],
            oauth: MICROSOFT_OAUTH,
            app_password: false,
            enable_imap: false,
        },
    },
    Preset {
        id: "icloud",
        name: "iCloud",
        domains: &["icloud.com", "me.com", "mac.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.me.com", 993, User::LocalPart)],
            smtp: &[starttls("smtp.mail.me.com", 587, User::Address)],
            oauth: None,
            app_password: true,
            enable_imap: false,
        },
    },
    Preset {
        id: "icloud-custom-domain",
        name: "iCloud (custom domain)",
        domains: &[],
        mx_suffixes: &["mail.icloud.com"],
        kind: Kind::Servers {
            imap: &[tls(
                "imap.mail.me.com",
                993,
                User::NotDerivable("primary iCloud address"),
            )],
            smtp: &[starttls(
                "smtp.mail.me.com",
                587,
                User::NotDerivable("primary iCloud address"),
            )],
            oauth: None,
            app_password: true,
            enable_imap: false,
        },
    },
    Preset {
        id: "yahoo",
        name: "Yahoo",
        domains: &[
            "yahoo.com",
            "ymail.com",
            "rocketmail.com",
            "yahoo.no",
            "yahoo.se",
            "yahoo.dk",
            "yahoo.co.uk",
            "yahoo.de",
            "yahoo.fr",
        ],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.yahoo.com", 993, User::Address)],
            smtp: &[
                tls("smtp.mail.yahoo.com", 465, User::Address),
                starttls("smtp.mail.yahoo.com", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: false,
        },
    },
    Preset {
        id: "aol",
        name: "AOL",
        domains: &["aol.com", "aim.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.aol.com", 993, User::Address)],
            smtp: &[
                tls("smtp.aol.com", 465, User::Address),
                starttls("smtp.aol.com", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: false,
        },
    },
    Preset {
        id: "fastmail",
        name: "Fastmail",
        domains: &["fastmail.com", "fastmail.fm"],
        mx_suffixes: &["messagingengine.com"],
        kind: Kind::Servers {
            imap: &[tls("imap.fastmail.com", 993, User::Address)],
            smtp: &[
                tls("smtp.fastmail.com", 465, User::Address),
                starttls("smtp.fastmail.com", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: false,
        },
    },
    Preset {
        id: "proton",
        name: "Proton Mail",
        domains: &["proton.me", "protonmail.com", "protonmail.ch", "pm.me"],
        mx_suffixes: &["protonmail.ch"],
        kind: Kind::Unsupported(Unsupported::BridgeOnly),
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
        mx_suffixes: &["tutanota.de", "tuta.com"],
        kind: Kind::Unsupported(Unsupported::NoImap),
    },
    Preset {
        id: "gmx-net",
        name: "GMX",
        domains: &["gmx.net", "gmx.de", "gmx.at", "gmx.ch"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.gmx.net", 993, User::Address)],
            smtp: &[
                tls("mail.gmx.net", 465, User::Address),
                starttls("mail.gmx.net", 587, User::Address),
            ],
            oauth: None,
            app_password: false,
            enable_imap: true,
        },
    },
    Preset {
        id: "gmx-com",
        name: "GMX",
        domains: &["gmx.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.gmx.com", 993, User::Address)],
            smtp: &[
                tls("mail.gmx.com", 465, User::Address),
                starttls("mail.gmx.com", 587, User::Address),
            ],
            oauth: None,
            app_password: false,
            enable_imap: true,
        },
    },
    Preset {
        id: "webde",
        name: "WEB.DE",
        domains: &["web.de"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.web.de", 993, User::Address)],
            smtp: &[
                tls("smtp.web.de", 465, User::Address),
                starttls("smtp.web.de", 587, User::Address),
            ],
            oauth: None,
            app_password: false,
            enable_imap: true,
        },
    },
    Preset {
        id: "mailcom",
        name: "mail.com",
        domains: &["mail.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.com", 993, User::Address)],
            smtp: &[
                tls("smtp.mail.com", 465, User::Address),
                starttls("smtp.mail.com", 587, User::Address),
            ],
            oauth: None,
            app_password: false,
            enable_imap: true,
        },
    },
    Preset {
        id: "yandex",
        name: "Yandex Mail",
        domains: &["yandex.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.yandex.com", 993, User::LocalPart)],
            smtp: &[
                tls("smtp.yandex.com", 465, User::LocalPart),
                starttls("smtp.yandex.com", 587, User::LocalPart),
            ],
            oauth: None,
            app_password: true,
            enable_imap: true,
        },
    },
    Preset {
        id: "mailru",
        name: "Mail.ru",
        domains: &["mail.ru", "inbox.ru", "list.ru", "bk.ru"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.mail.ru", 993, User::Address)],
            smtp: &[tls("smtp.mail.ru", 465, User::Address)],
            oauth: None,
            app_password: true,
            enable_imap: false,
        },
    },
    Preset {
        id: "zoho",
        name: "Zoho Mail",
        domains: &["zoho.com", "zohomail.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.zoho.com", 993, User::Address)],
            smtp: &[
                tls("smtp.zoho.com", 465, User::Address),
                starttls("smtp.zoho.com", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: true,
        },
    },
    Preset {
        id: "zoho-org-eu",
        name: "Zoho Mail (organization, EU)",
        domains: &[],
        mx_suffixes: &["zoho.eu"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.eu", 993, User::Address)],
            smtp: &[
                tls("smtppro.zoho.eu", 465, User::Address),
                starttls("smtppro.zoho.eu", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: true,
        },
    },
    Preset {
        id: "zoho-org-in",
        name: "Zoho Mail (organization, IN)",
        domains: &[],
        mx_suffixes: &["zoho.in"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.in", 993, User::Address)],
            smtp: &[
                tls("smtppro.zoho.in", 465, User::Address),
                starttls("smtppro.zoho.in", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: true,
        },
    },
    Preset {
        id: "zoho-org-au",
        name: "Zoho Mail (organization, AU)",
        domains: &[],
        mx_suffixes: &["zoho.com.au"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.com.au", 993, User::Address)],
            smtp: &[
                tls("smtppro.zoho.com.au", 465, User::Address),
                starttls("smtppro.zoho.com.au", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: true,
        },
    },
    Preset {
        id: "zoho-org",
        name: "Zoho Mail (organization)",
        domains: &[],
        mx_suffixes: &["zoho.com"],
        kind: Kind::Servers {
            imap: &[tls("imappro.zoho.com", 993, User::Address)],
            smtp: &[
                tls("smtppro.zoho.com", 465, User::Address),
                starttls("smtppro.zoho.com", 587, User::Address),
            ],
            oauth: None,
            app_password: true,
            enable_imap: true,
        },
    },
];

const DOMENESHOP_USER: User = User::NotDerivable("Domeneshop account name, e.g. mittnavn1");

pub static LOCAL: &[Preset] = &[
    Preset {
        id: "telenor",
        name: "Telenor",
        domains: &["online.no"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("mail.online.no", 993, User::LocalPart)],
            smtp: &[tls("mail.online.no", 465, User::LocalPart)],
            oauth: None,
            app_password: false,
            enable_imap: true,
        },
    },
    Preset {
        id: "altibox",
        name: "Altibox",
        domains: &["altibox.no", "lyse.net"],
        mx_suffixes: &["altibox.net"],
        kind: Kind::Servers {
            imap: &[tls("imap.altibox.no", 993, User::Address)],
            smtp: &[tls("smtp.altibox.no", 465, User::Address)],
            oauth: None,
            app_password: false,
            enable_imap: false,
        },
    },
    Preset {
        id: "domeneshop",
        name: "Domeneshop",
        domains: &["domeneshop.no"],
        mx_suffixes: &["domeneshop.no"],
        kind: Kind::Servers {
            imap: &[tls("imap.domeneshop.no", 993, DOMENESHOP_USER)],
            smtp: &[
                tls("smtp.domeneshop.no", 465, DOMENESHOP_USER),
                starttls("smtp.domeneshop.no", 587, DOMENESHOP_USER),
            ],
            oauth: None,
            app_password: false,
            enable_imap: false,
        },
    },
    Preset {
        id: "broadpark",
        name: "NextGenTel (Broadpark)",
        domains: &["broadpark.no"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.broadpark.no", 993, User::Address)],
            smtp: &[starttls("smtp.broadpark.no", 587, User::Address)],
            oauth: None,
            app_password: false,
            enable_imap: false,
        },
    },
    Preset {
        id: "tussa",
        name: "Tussa",
        domains: &["tussa.com"],
        mx_suffixes: &[],
        kind: Kind::Servers {
            imap: &[tls("imap.tussa.com", 993, User::Address)],
            smtp: &[starttls("smtp.tussa.com", 587, User::Address)],
            oauth: None,
            app_password: false,
            enable_imap: false,
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_domains_exactly() {
        assert_eq!(by_domain("gmail.com", false).map(|p| p.id), Some("google"));
        assert_eq!(
            by_domain("hotmail.no", false).map(|p| p.id),
            Some("outlook")
        );
        assert!(by_domain("mail.gmail.com", false).is_none());
        assert!(by_domain("example.com", true).is_none());
        assert!(by_domain("online.no", false).is_none());
        assert_eq!(by_domain("online.no", true).map(|p| p.id), Some("telenor"));
    }

    #[test]
    fn matches_mx_on_label_boundaries() {
        assert_eq!(
            by_mx("aspmx.l.google.com.", false).map(|p| p.id),
            Some("google")
        );
        assert_eq!(
            by_mx("smtp.google.com", false).map(|p| p.id),
            Some("google")
        );
        assert_eq!(
            by_mx("ALT1.ASPMX.L.GOOGLE.COM.", false).map(|p| p.id),
            Some("google")
        );
        assert_eq!(
            by_mx("example-com.mail.protection.outlook.com", false).map(|p| p.id),
            Some("microsoft365")
        );
        assert_eq!(
            by_mx("example-com.m-v1.mx.microsoft.", false).map(|p| p.id),
            Some("microsoft365")
        );
        assert_eq!(
            by_mx("hotmail-com.olc.protection.outlook.com", false).map(|p| p.id),
            Some("outlook")
        );
        assert_eq!(
            by_mx("in1-smtp.messagingengine.com", false).map(|p| p.id),
            Some("fastmail")
        );
        assert_eq!(
            by_mx("mx.zoho.eu", false).map(|p| p.id),
            Some("zoho-org-eu")
        );
        assert_eq!(by_mx("mx.zoho.com", false).map(|p| p.id), Some("zoho-org"));
        assert_eq!(
            by_mx("mx.zoho.com.au", false).map(|p| p.id),
            Some("zoho-org-au")
        );
        assert_eq!(
            by_mx("mail.protonmail.ch", false).map(|p| p.id),
            Some("proton")
        );
        assert!(by_mx("notgoogle.com", false).is_none());
        assert!(by_mx("google.com.evil.example", false).is_none());
    }

    #[test]
    fn unsupported_presets_say_why() {
        let finding = by_domain("proton.me", false).unwrap().finding();
        assert!(matches!(
            finding,
            Finding::Unsupported {
                why: Unsupported::BridgeOnly,
                ..
            }
        ));
    }

    #[test]
    fn every_preset_server_uses_tls() {
        for preset in PRESETS.iter().chain(LOCAL) {
            if let Kind::Servers { imap, smtp, .. } = &preset.kind {
                assert!(!imap.is_empty() && !smtp.is_empty(), "{}", preset.id);
                assert!(imap.iter().chain(smtp.iter()).all(|e| e.security.is_tls()));
            }
        }
    }
}
