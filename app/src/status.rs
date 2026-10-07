//! What the window's banner shows (ML-048; DESIGN.md "Feedback"): the states the engine reports
//! for the accounts, the keyring, and the desktop's network, most important first. The banner
//! shows the first and counts the rest.
//!
//! The order, a proposal for the M3 gate: no keyring, then each account whose certificate
//! changed, each that needs a sign-in, each send that may have gone out (M6, oldest first, a
//! proposal for the M6 gate), each account whose mailbox is full (in the sidebar's order), then
//! offline. Offline is one entry: when the desktop has no network, or when every account's
//! server is out of reach; a single unreachable account shows only in the sidebar.

use crate::setup::Again;
use crate::setup::form::security_name;
use crate::window::{AccountStatus, Attention, Window};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Receive};

/// An account as the banner needs it: the store's name and the engine's state for it, and its
/// `accounts.toml` entry.
#[derive(Debug, Clone, Copy)]
pub struct Account<'a> {
    pub name: &'a str,
    pub status: AccountStatus,
    pub settings: &'a AccountSettings,
}

/// A banner entry, and the account its buttons act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub attention: Attention,
    pub account: Option<AccountKey>,
}

/// Everything waiting, most important first. `keyring_missing`: the engine fell back to memory
/// and the banner wasn't dismissed; `network`: the desktop reports a network; `pending`: the
/// changes and sends waiting for a server, which the offline entry counts; `uncertain`: the
/// account name of each send that may have gone out, oldest first.
pub fn queue(
    accounts: &[Account<'_>],
    keyring_missing: bool,
    network: bool,
    pending: u32,
    uncertain: &[String],
) -> Vec<Waiting> {
    let mut waiting = Vec::new();
    if keyring_missing {
        waiting.push(Waiting {
            attention: Attention::NoKeyring,
            account: None,
        });
    }
    let kinds = [
        AccountStatus::CertificateChanged,
        AccountStatus::NeedsSignIn,
        AccountStatus::QuotaFull,
    ];
    for kind in kinds {
        if kind == AccountStatus::QuotaFull {
            waiting.extend(uncertain.iter().map(|name| Waiting {
                attention: Attention::UncertainSend { name: name.clone() },
                account: None,
            }));
        }
        for account in accounts.iter().filter(|account| account.status == kind) {
            let name = account.name.to_owned();
            let attention = match kind {
                AccountStatus::CertificateChanged => Attention::CertificateChanged {
                    name,
                    host: account.settings.incoming.host.clone(),
                },
                AccountStatus::NeedsSignIn => Attention::SignIn {
                    name,
                    browser: matches!(account.settings.auth, AuthMethod::OAuth(_)),
                },
                _ => Attention::QuotaFull { name },
            };
            waiting.push(Waiting {
                attention,
                account: Some(account.settings.key.clone()),
            });
        }
    }
    let unreachable = !accounts.is_empty()
        && accounts
            .iter()
            .all(|account| account.status == AccountStatus::Offline);
    if !network || unreachable {
        waiting.push(Waiting {
            attention: Attention::Offline {
                network: !network,
                accounts: accounts.len(),
                waiting: pending,
            },
            account: None,
        });
    }
    waiting
}

/// The certificate banner's Details: why Sealwax stopped, what to do, and the account's
/// servers. There is no way to trust the certificate (pinning is 1.x); Retry reconnects.
pub fn certificate_details(window: &Window, again: &Again, retry: impl Fn() + 'static) {
    let (heading, body) = details_text(again);
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("close")
        .default_response("retry")
        .build();
    dialog.set_widget_name("certificate-details");
    dialog.add_response("close", "Close");
    dialog.add_response("retry", "Retry");
    dialog.set_response_appearance("retry", adw::ResponseAppearance::Suggested);
    dialog.connect_response(None, move |_, response| {
        if response == "retry" {
            retry();
        }
    });
    dialog.present(Some(window.window()));
}

fn details_text(again: &Again) -> (String, String) {
    let server = |what: &str, endpoint: &Endpoint| {
        format!(
            "{what}: {}, port {}, {}",
            endpoint.host,
            endpoint.port,
            security_name(endpoint.security)
        )
    };
    let heading = format!("Why Sealwax stopped connecting to {}", again.name);
    let body = format!(
        "Sealwax checks a server's certificate before it signs in, and {host}'s no longer \
         verifies. It may have changed or expired, or a network that inspects secure \
         connections, as some workplace, school and hotel networks do, may stand in between. \
         Sealwax never signs in to a server it can't verify, and this version can't be told to \
         trust one.\n\nTry another network, then Retry. If it keeps happening, ask your provider \
         about its certificate.\n\n{imap}\n{smtp}",
        host = again.incoming.host,
        imap = server(
            match again.receive {
                Receive::Imap => "Incoming mail (IMAP)",
                Receive::Pop3(_) => "Incoming mail (POP3)",
            },
            &again.incoming
        ),
        smtp = server("Outgoing mail (SMTP)", &again.smtp),
    );
    (heading, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::{Endpoint, OAuthProvider, Security};

    fn settings(key: &str, auth: AuthMethod) -> AccountSettings {
        let endpoint = |host: &str| Endpoint {
            host: host.to_owned(),
            port: 993,
            security: Security::Tls,
        };
        AccountSettings {
            key: AccountKey(key.to_owned()),
            name: key.to_owned(),
            address: format!("{key}@example.test"),
            incoming: endpoint(&format!("imap.{key}.example.test")),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: endpoint(&format!("smtp.{key}.example.test")),
            username: key.to_owned(),
            auth,
            signature: None,
            identities: Vec::new(),
        }
    }

    fn shown(waiting: &[Waiting]) -> Vec<String> {
        waiting
            .iter()
            .map(|entry| {
                let kind = match &entry.attention {
                    Attention::NoKeyring => "keyring",
                    Attention::CertificateChanged { .. } => "certificate",
                    Attention::SignIn { browser: true, .. } => "sign-in",
                    Attention::SignIn { browser: false, .. } => "password",
                    Attention::QuotaFull { .. } => "quota",
                    Attention::UncertainSend { .. } => "uncertain",
                    Attention::Offline { network: true, .. } => "offline",
                    Attention::Offline { .. } => "unreachable",
                };
                match &entry.account {
                    Some(key) => format!("{kind}:{key}"),
                    None => kind.to_owned(),
                }
            })
            .collect()
    }

    #[test]
    fn the_most_important_comes_first_and_accounts_keep_the_sidebars_order() {
        let microsoft = AuthMethod::OAuth(OAuthProvider::Microsoft);
        let listed = [
            settings("anne", AuthMethod::Password),
            settings("kari", microsoft),
            settings("ola", AuthMethod::Password),
            settings("per", AuthMethod::Password),
            settings("siri", AuthMethod::Password),
        ];
        let states = [
            AccountStatus::QuotaFull,
            AccountStatus::NeedsSignIn,
            AccountStatus::NeedsSignIn,
            AccountStatus::CertificateChanged,
            AccountStatus::Offline,
        ];
        let accounts: Vec<Account<'_>> = listed
            .iter()
            .zip(states)
            .map(|(settings, status)| Account {
                name: &settings.name,
                status,
                settings,
            })
            .collect();
        assert_eq!(
            shown(&queue(&accounts, true, false, 0, &[])),
            [
                "keyring",
                "certificate:per",
                "sign-in:kari",
                "password:ola",
                "quota:anne",
                "offline"
            ]
        );
        // Sends that may have gone out: after the sign-ins, before the full mailboxes.
        let uncertain = ["anne".to_owned(), "ola".to_owned()];
        assert_eq!(
            shown(&queue(&accounts, false, false, 2, &uncertain)),
            [
                "certificate:per",
                "sign-in:kari",
                "password:ola",
                "uncertain",
                "uncertain",
                "quota:anne",
                "offline"
            ]
        );
        assert_eq!(
            shown(&queue(&accounts, false, true, 0, &[])),
            [
                "certificate:per",
                "sign-in:kari",
                "password:ola",
                "quota:anne"
            ],
            "one unreachable account shows only in the sidebar"
        );
        assert_eq!(
            queue(&accounts, false, true, 0, &[])[0].attention,
            Attention::CertificateChanged {
                name: "per".to_owned(),
                host: "imap.per.example.test".to_owned(),
            }
        );
    }

    #[test]
    fn offline_when_the_network_is_gone_or_no_server_answers() {
        let listed = [
            settings("anne", AuthMethod::Password),
            settings("ola", AuthMethod::Password),
        ];
        let accounts = |states: [AccountStatus; 2]| -> Vec<Account<'_>> {
            listed
                .iter()
                .zip(states)
                .map(|(settings, status)| Account {
                    name: &settings.name,
                    status,
                    settings,
                })
                .collect()
        };
        let offline = accounts([AccountStatus::Offline, AccountStatus::Offline]);
        assert_eq!(
            shown(&queue(&offline, false, true, 0, &[])),
            ["unreachable"]
        );
        assert_eq!(
            queue(&offline, false, true, 0, &[])[0].attention,
            Attention::Offline {
                network: false,
                accounts: 2,
                waiting: 0,
            }
        );
        assert_eq!(shown(&queue(&offline, false, false, 0, &[])), ["offline"]);
        assert_eq!(
            queue(&offline, false, false, 3, &[])[0].attention,
            Attention::Offline {
                network: true,
                accounts: 2,
                waiting: 3,
            }
        );
        let synced = accounts([AccountStatus::Synced, AccountStatus::Syncing]);
        assert!(queue(&synced, false, true, 0, &[]).is_empty());
        assert_eq!(shown(&queue(&synced, false, false, 0, &[])), ["offline"]);
        assert_eq!(shown(&queue(&[], false, false, 0, &[])), ["offline"]);
        assert!(
            queue(&[], false, true, 0, &[]).is_empty(),
            "no account, no server"
        );
        assert_eq!(shown(&queue(&[], true, true, 0, &[])), ["keyring"]);
    }

    #[test]
    fn the_details_name_the_servers_and_offer_no_way_to_trust() {
        let account = settings("work", AuthMethod::Password);
        let again = Again {
            key: account.key.clone(),
            name: "Work".to_owned(),
            address: account.address.clone(),
            incoming: account.incoming.clone(),
            receive: account.receive,
            smtp: Endpoint {
                port: 587,
                security: Security::StartTls,
                ..account.smtp.clone()
            },
        };
        let (heading, body) = details_text(&again);
        assert_eq!(heading, "Why Sealwax stopped connecting to Work");
        assert!(body.contains("imap.work.example.test's no longer verifies"));
        assert!(body.ends_with(
            "Incoming mail (IMAP): imap.work.example.test, port 993, TLS\n\
             Outgoing mail (SMTP): smtp.work.example.test, port 587, STARTTLS"
        ));
        assert!(!body.to_lowercase().contains("trust it"));
        // A POP3 account's incoming server is named as POP3's (M8).
        let pop3 = Again {
            incoming: Endpoint {
                host: "pop.work.example.test".to_owned(),
                port: 995,
                security: Security::Tls,
            },
            receive: Receive::Pop3(sealwax_domain::account::Pop3Policy::default()),
            ..again
        };
        let (_, body) = details_text(&pop3);
        assert!(body.contains("pop.work.example.test's no longer verifies"));
        assert!(body.ends_with(
            "Incoming mail (POP3): pop.work.example.test, port 995, TLS\n\
             Outgoing mail (SMTP): smtp.work.example.test, port 587, STARTTLS"
        ));
    }
}
