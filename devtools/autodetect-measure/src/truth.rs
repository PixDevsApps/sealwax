use crate::model::Security;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum User {
    Address,
    LocalPart,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub struct Accept {
    pub host: &'static str,
    pub port: u16,
    pub security: Security,
    pub users: &'static [User],
}

impl Accept {
    pub const fn tls(host: &'static str, port: u16, users: &'static [User]) -> Self {
        Self {
            host,
            port,
            security: Security::Tls,
            users,
        }
    }

    pub const fn starttls(host: &'static str, port: u16, users: &'static [User]) -> Self {
        Self {
            host,
            port,
            security: Security::StartTls,
            users,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Expect {
    Settings {
        imap: &'static [Accept],
        smtp: &'static [Accept],
    },
    Unsupported,
    NoMail,
}

#[derive(Clone, Copy, Debug)]
pub struct Truth {
    pub domain: &'static str,
    pub group: &'static str,
    pub expect: Expect,
    pub evidence: &'static str,
}

pub fn find(domain: &str) -> Option<&'static Truth> {
    TRUTH.iter().find(|t| t.domain == domain)
}

const A: &[User] = &[User::Address];
const L: &[User] = &[User::LocalPart];
const LA: &[User] = &[User::LocalPart, User::Address];
const O: &[User] = &[User::Other];

const GOOGLE: Expect = Expect::Settings {
    imap: &[Accept::tls("imap.gmail.com", 993, A)],
    smtp: &[
        Accept::tls("smtp.gmail.com", 465, A),
        Accept::starttls("smtp.gmail.com", 587, A),
    ],
};
const GOOGLE_DOCS: &str = "developers.google.com/workspace/gmail/imap/imap-smtp (updated 2026-09-15); knowledge.workspace.google.com set-up-gmail-with-a-third-party-email-client";

const OUTLOOK: Expect = Expect::Settings {
    imap: &[
        Accept::tls("outlook.office365.com", 993, A),
        Accept::tls("imap-mail.outlook.com", 993, A),
    ],
    smtp: &[Accept::starttls("smtp-mail.outlook.com", 587, A)],
};
const OUTLOOK_DOCS: &str = "support.microsoft.com pop-imap-and-smtp-settings-for-outlook-com (d088b986) and settings-for-imap4-access (fbb4210e); OAuth2 only since 2024-09-16";

const M365: Expect = Expect::Settings {
    imap: &[Accept::tls("outlook.office365.com", 993, A)],
    smtp: &[Accept::starttls("smtp.office365.com", 587, A)],
};

const ICLOUD: Expect = Expect::Settings {
    imap: &[Accept::tls("imap.mail.me.com", 993, LA)],
    smtp: &[Accept::starttls("smtp.mail.me.com", 587, A)],
};
const ICLOUD_DOCS: &str = "support.apple.com/en-us/102525 (2026-02-03): IMAP user usually the name part, full address as fallback; SMTP user the full address";

const FASTMAIL: Expect = Expect::Settings {
    imap: &[Accept::tls("imap.fastmail.com", 993, A)],
    smtp: &[
        Accept::tls("smtp.fastmail.com", 465, A),
        Accept::starttls("smtp.fastmail.com", 587, A),
    ],
};
const FASTMAIL_DOCS: &str =
    "fastmail.help 1500000278342 Server names and ports; app password mandatory";

const ZOHO_ORG: Expect = Expect::Settings {
    imap: &[
        Accept::tls("imappro.zoho.com", 993, A),
        Accept::tls("imap.zoho.com", 993, A),
    ],
    smtp: &[
        Accept::tls("smtppro.zoho.com", 465, A),
        Accept::starttls("smtppro.zoho.com", 587, A),
        Accept::tls("smtp.zoho.com", 465, A),
        Accept::starttls("smtp.zoho.com", 587, A),
    ],
};
const ZOHO_DOCS: &str = "zoho.com/mail/help/imap-access.html and zoho-smtp.html: imappro/smtppro for paid organizations, imap/smtp for free ones";

const ZOHO_ORG_EU: Expect = Expect::Settings {
    imap: &[
        Accept::tls("imappro.zoho.eu", 993, A),
        Accept::tls("imap.zoho.eu", 993, A),
    ],
    smtp: &[
        Accept::tls("smtppro.zoho.eu", 465, A),
        Accept::starttls("smtppro.zoho.eu", 587, A),
        Accept::tls("smtp.zoho.eu", 465, A),
        Accept::starttls("smtp.zoho.eu", 587, A),
    ],
};

const ALTIBOX: Expect = Expect::Settings {
    imap: &[Accept::tls("imap.altibox.no", 993, A)],
    smtp: &[Accept::tls("smtp.altibox.no", 465, A)],
};

const DOMENESHOP: Expect = Expect::Settings {
    imap: &[Accept::tls("imap.domeneshop.no", 993, O)],
    smtp: &[
        Accept::starttls("smtp.domeneshop.no", 587, O),
        Accept::tls("smtp.domeneshop.no", 465, O),
    ],
};
const DOMENESHOP_DOCS: &str = "domene.shop/faq?id=144,143,37,409,237: imap/smtp.domeneshop.no; the username is the generated account name (e.g. mittnavn1), not the address";

pub static TRUTH: &[Truth] = &[
    Truth {
        domain: "gmail.com",
        group: "big",
        expect: GOOGLE,
        evidence: GOOGLE_DOCS,
    },
    Truth {
        domain: "googlemail.com",
        group: "big",
        expect: GOOGLE,
        evidence: "same Google accounts as gmail.com (MX gmail-smtp-in.l.google.com); not separately documented",
    },
    Truth {
        domain: "outlook.com",
        group: "big",
        expect: OUTLOOK,
        evidence: OUTLOOK_DOCS,
    },
    Truth {
        domain: "hotmail.com",
        group: "big",
        expect: OUTLOOK,
        evidence: OUTLOOK_DOCS,
    },
    Truth {
        domain: "live.com",
        group: "big",
        expect: OUTLOOK,
        evidence: OUTLOOK_DOCS,
    },
    Truth {
        domain: "msn.com",
        group: "big",
        expect: OUTLOOK,
        evidence: OUTLOOK_DOCS,
    },
    Truth {
        domain: "icloud.com",
        group: "big",
        expect: ICLOUD,
        evidence: ICLOUD_DOCS,
    },
    Truth {
        domain: "me.com",
        group: "big",
        expect: ICLOUD,
        evidence: "support.apple.com/en-us/102525 and 102654 (me.com addresses are iCloud accounts)",
    },
    Truth {
        domain: "yahoo.com",
        group: "big",
        expect: Expect::Settings {
            imap: &[Accept::tls("imap.mail.yahoo.com", 993, A)],
            smtp: &[
                Accept::tls("smtp.mail.yahoo.com", 465, A),
                Accept::starttls("smtp.mail.yahoo.com", 587, A),
            ],
        },
        evidence: "help.yahoo.com SLN4075: 465 or 587, SSL; app password",
    },
    Truth {
        domain: "aol.com",
        group: "big",
        expect: Expect::Settings {
            imap: &[Accept::tls("imap.aol.com", 993, A)],
            smtp: &[Accept::tls("smtp.aol.com", 465, A)],
        },
        evidence: "help.aol.com other-email-applications (via search index only; pages did not render); app password",
    },
    Truth {
        domain: "fastmail.com",
        group: "big",
        expect: FASTMAIL,
        evidence: FASTMAIL_DOCS,
    },
    Truth {
        domain: "proton.me",
        group: "big",
        expect: Expect::Unsupported,
        evidence: "proton.me/support/imap-smtp-and-pop3-setup: IMAP/SMTP only through Proton Mail Bridge (paid)",
    },
    Truth {
        domain: "gmx.com",
        group: "big",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("imap.gmx.com", 993, A),
                Accept::starttls("imap.gmx.com", 143, A),
            ],
            smtp: &[
                Accept::tls("mail.gmx.com", 465, A),
                Accept::starttls("mail.gmx.com", 587, A),
            ],
        },
        evidence: "support.gmx.com/pop-imap/imap/server.html; IMAP must be enabled in settings",
    },
    Truth {
        domain: "gmx.net",
        group: "big",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("imap.gmx.net", 993, A),
                Accept::starttls("imap.gmx.net", 143, A),
            ],
            smtp: &[
                Accept::tls("mail.gmx.net", 465, A),
                Accept::starttls("mail.gmx.net", 587, A),
            ],
        },
        evidence: "hilfe.gmx.net/pop-imap/imap/imap-serverdaten.html; IMAP must be enabled; app password outside EU/US/CA",
    },
    Truth {
        domain: "web.de",
        group: "big",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("imap.web.de", 993, A),
                Accept::starttls("imap.web.de", 143, A),
            ],
            smtp: &[
                Accept::tls("smtp.web.de", 465, A),
                Accept::starttls("smtp.web.de", 587, A),
            ],
        },
        evidence: "hilfe.web.de/pop-imap/imap/imap-serverdaten.html; IMAP must be enabled",
    },
    Truth {
        domain: "mail.com",
        group: "big",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("imap.mail.com", 993, A),
                Accept::starttls("imap.mail.com", 143, A),
            ],
            smtp: &[
                Accept::tls("smtp.mail.com", 465, A),
                Accept::starttls("smtp.mail.com", 587, A),
            ],
        },
        evidence: "support.mail.com/pop-imap/imap/server.html; IMAP is a Premium feature and must be enabled",
    },
    Truth {
        domain: "zoho.com",
        group: "big",
        expect: Expect::Settings {
            imap: &[Accept::tls("imap.zoho.com", 993, A)],
            smtp: &[
                Accept::tls("smtp.zoho.com", 465, A),
                Accept::starttls("smtp.zoho.com", 587, A),
            ],
        },
        evidence: "zoho.com/mail/help/imap-access.html: personal users imap.zoho.com; IMAP must be enabled",
    },
    Truth {
        domain: "yandex.com",
        group: "big",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("imap.yandex.com", 993, L),
                Accept::tls("imap.ya.ru", 993, L),
            ],
            smtp: &[
                Accept::tls("smtp.yandex.com", 465, L),
                Accept::starttls("smtp.yandex.com", 587, L),
            ],
        },
        evidence: "yandex.com/support/mail/mail-clients/others.html: user is the part before @; imap.ya.ru outside Russia; app password",
    },
    Truth {
        domain: "mail.ru",
        group: "big",
        expect: Expect::Settings {
            imap: &[Accept::tls("imap.mail.ru", 993, A)],
            smtp: &[Accept::tls("smtp.mail.ru", 465, A)],
        },
        evidence: "help.mail.ru/mail/login/mailer (updated 2026-02-25); full address only partly confirmed; app password",
    },
    Truth {
        domain: "mailbox.org",
        group: "big",
        expect: Expect::Settings {
            imap: &[Accept::tls("imap.mailbox.org", 993, A)],
            smtp: &[
                Accept::tls("smtp.mailbox.org", 465, A),
                Accept::starttls("smtp.mailbox.org", 587, A),
            ],
        },
        evidence: "kb.mailbox.org/en/private/e-mail/e-mail-configuration",
    },
    Truth {
        domain: "posteo.de",
        group: "big",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("posteo.de", 993, A),
                Accept::starttls("posteo.de", 143, A),
            ],
            smtp: &[
                Accept::tls("posteo.de", 465, A),
                Accept::starttls("posteo.de", 587, A),
            ],
        },
        evidence: "posteo.de/en/help/how-do-i-set-up-posteo-in-an-email-client-pop3-imap-and-smtp",
    },
    Truth {
        domain: "tutanota.com",
        group: "big",
        expect: Expect::Unsupported,
        evidence: "tuta.com/security and tuta.com/blog/tuta-add-on-in-thunderbird (2026-02-18): no IMAP",
    },
    Truth {
        domain: "online.no",
        group: "norway",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("mail.online.no", 993, L),
                Accept::tls("imap.online.no", 993, L),
            ],
            smtp: &[
                Accept::tls("mail.online.no", 465, L),
                Accept::tls("smtp.online.no", 465, L),
            ],
        },
        evidence: "telenor.no/kundeservice/e-post/oppsett: mail.online.no 993/465; user is the part before @online.no; imap/smtp.online.no 'will also work'; 587 left out: its STARTTLS certificate has an unknown issuer (verify 2026-10-04)",
    },
    Truth {
        domain: "altibox.no",
        group: "norway",
        expect: ALTIBOX,
        evidence: "kundeservice.altibox.no/hc/no/articles/12424140074513 (edited 2026-03-31): imap.altibox.no 993, smtp.altibox.no 465; user is the address",
    },
    Truth {
        domain: "lyse.net",
        group: "norway",
        expect: ALTIBOX,
        evidence: "Altibox FAQ 32957746904209: lyse.net mailboxes are on Altibox's server (settings by implication, user not stated)",
    },
    Truth {
        domain: "broadpark.no",
        group: "norway",
        expect: Expect::Settings {
            imap: &[
                Accept::tls("imap.broadpark.no", 993, LA),
                Accept::starttls("imap.broadpark.no", 143, LA),
            ],
            smtp: &[Accept::starttls("smtp.broadpark.no", 587, LA)],
        },
        evidence: "hjelp.nextgentel.no/no_NO/e-post/oppsett-av-e-post; username form not documented (either form accepted)",
    },
    Truth {
        domain: "tussa.com",
        group: "norway",
        expect: Expect::Settings {
            imap: &[Accept::tls("imap.tussa.com", 993, A)],
            smtp: &[Accept::starttls("smtp.tussa.com", 587, A)],
        },
        evidence: "tussa.no/kundeservice/brukarrettleiingar (e-post, outlook-pc): imap 993 SSL/TLS, smtp 587 STARTTLS, user is the address",
    },
    Truth {
        domain: "domeneshop.no",
        group: "norway",
        expect: DOMENESHOP,
        evidence: DOMENESHOP_DOCS,
    },
    Truth {
        domain: "birdlife.no",
        group: "norway",
        expect: DOMENESHOP,
        evidence: "MX mx.domeneshop.no (DoH 2026-10-04); domene.shop FAQ",
    },
    Truth {
        domain: "piratpartiet.no",
        group: "norway",
        expect: DOMENESHOP,
        evidence: "MX mx.domeneshop.no (DoH 2026-10-04); domene.shop FAQ",
    },
    Truth {
        domain: "one.com",
        group: "norway",
        expect: Expect::Settings {
            imap: &[Accept::tls("imap.one.com", 993, A)],
            smtp: &[
                Accept::starttls("send.one.com", 587, A),
                Accept::tls("send.one.com", 465, A),
            ],
        },
        evidence: "help.one.com/hc/en-us/articles/115005594285 (edited 2026-01-08): user is the full address",
    },
    Truth {
        domain: "hotmail.no",
        group: "norway",
        expect: OUTLOOK,
        evidence: "MX eur.olc.protection.outlook.com (Outlook.com consumer, DoH 2026-10-04); Outlook.com settings page (hotmail.no not listed by name)",
    },
    Truth {
        domain: "c2i.net",
        group: "norway",
        expect: Expect::NoMail,
        evidence: "Tele2 ended the service 2013-05-01 (NRK 2013-05-06); domain taken over by an unknown party (digi.no 2016-07-15, NorCERT warning); MX blackhole.aftermarket.pl",
    },
    Truth {
        domain: "chello.no",
        group: "norway",
        expect: Expect::NoMail,
        evidence: "null MX (DoH 2026-10-04); chello addresses ended 2008-03-01 (Aftenposten 2007-11-23)",
    },
    Truth {
        domain: "tele2.no",
        group: "norway",
        expect: Expect::NoMail,
        evidence: "Tele2 ended the service 2013-05-01 (NRK 2013-05-06); no MX record (DoH 2026-10-04)",
    },
    Truth {
        domain: "mozilla.org",
        group: "workspace",
        expect: GOOGLE,
        evidence: "MX aspmx.l.google.com (DoH 2026-10-04); Google Workspace docs",
    },
    Truth {
        domain: "finn.no",
        group: "workspace",
        expect: GOOGLE,
        evidence: "MX aspmx.l.google.com (DoH 2026-10-04); Google Workspace docs",
    },
    Truth {
        domain: "nrk.no",
        group: "workspace",
        expect: GOOGLE,
        evidence: "MX smtp.google.com (DoH 2026-10-04); Google Workspace docs",
    },
    Truth {
        domain: "letsencrypt.org",
        group: "workspace",
        expect: GOOGLE,
        evidence: "MX aspmx.l.google.com (DoH 2026-10-04); Google Workspace docs",
    },
    Truth {
        domain: "vy.no",
        group: "m365",
        expect: M365,
        evidence: "MX vy-no.mail.protection.outlook.com (DoH 2026-10-04); learn.microsoft.com pop3-and-imap4 and authenticated-client-smtp-submission (465 unsupported)",
    },
    Truth {
        domain: "uib.no",
        group: "m365",
        expect: M365,
        evidence: "MX uib-no.mail.protection.outlook.com (DoH 2026-10-04); learn.microsoft.com pop3-and-imap4 and authenticated-client-smtp-submission (465 unsupported)",
    },
    Truth {
        domain: "oslomet.no",
        group: "m365",
        expect: M365,
        evidence: "MX oslomet-no.m-v1.mx.microsoft (DoH 2026-10-04); learn.microsoft.com pop3-and-imap4 and authenticated-client-smtp-submission (465 unsupported)",
    },
    Truth {
        domain: "stortinget.no",
        group: "m365",
        expect: M365,
        evidence: "MX stortinget-no.mail.protection.outlook.com (DoH 2026-10-04); learn.microsoft.com pop3-and-imap4 and authenticated-client-smtp-submission (465 unsupported)",
    },
    Truth {
        domain: "eff.org",
        group: "m365",
        expect: M365,
        evidence: "MX eff-org.mail.protection.outlook.com (DoH 2026-10-04); learn.microsoft.com pop3-and-imap4 and authenticated-client-smtp-submission (465 unsupported)",
    },
    Truth {
        domain: "linux.org.au",
        group: "fastmail",
        expect: FASTMAIL,
        evidence: "MX in1-smtp.messagingengine.com (DoH 2026-10-04); fastmail.help server names",
    },
    Truth {
        domain: "digitalrightswatch.org.au",
        group: "fastmail",
        expect: FASTMAIL,
        evidence: "MX in1-smtp.messagingengine.com (DoH 2026-10-04); fastmail.help server names",
    },
    Truth {
        domain: "cyrusimap.org",
        group: "fastmail",
        expect: FASTMAIL,
        evidence: "MX us1-smtp.messagingengine.com (DoH 2026-10-04); fastmail.help server names",
    },
    Truth {
        domain: "gitea.io",
        group: "zoho",
        expect: ZOHO_ORG,
        evidence: ZOHO_DOCS,
    },
    Truth {
        domain: "pine64.org",
        group: "zoho",
        expect: ZOHO_ORG,
        evidence: ZOHO_DOCS,
    },
    Truth {
        domain: "fosstodon.org",
        group: "zoho",
        expect: ZOHO_ORG_EU,
        evidence: "MX mx.zoho.eu (DoH 2026-10-04); EU hostnames only in Zoho community posts plus a TLS greeting check, not in Zoho's help pages",
    },
];
