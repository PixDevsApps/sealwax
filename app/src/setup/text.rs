//! What the assistant says (PRD A3, DESIGN.md "Setup assistant"): a plain message for every
//! outcome that says what to do next, and the lines that describe detected settings. Only
//! the screen shows these; nothing here is logged.

use super::form::security_name;
use sealwax_domain::account::{
    CertificatePin, DetectSource, DetectedSettings, Detection, Endpoint, HelpLink, LoginError,
    LoginFailure, Protocol, UnsupportedReason, UsernameForm, is_loopback,
};
use sealwax_engine::api::AccountError;

/// How a notice looks: a problem to fix, or information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Problem,
    Info,
}

/// The assistant's alert: a heading, what to do next, and the provider's page when it helps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub tone: Tone,
    pub title: String,
    pub body: String,
    pub link: Option<HelpLink>,
}

impl Notice {
    fn problem(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            tone: Tone::Problem,
            title: title.into(),
            body: body.into(),
            link: None,
        }
    }

    fn with_link(mut self, link: Option<&HelpLink>) -> Self {
        self.link = link.cloned();
        self
    }
}

/// What the failure message can name: the provider, the servers tried and its help pages.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub provider: Option<&'a str>,
    /// The incoming server tried: IMAP's, or POP3's (M8).
    pub incoming: &'a Endpoint,
    pub smtp: &'a Endpoint,
    pub app_password: Option<&'a HelpLink>,
    pub enable_imap: Option<&'a HelpLink>,
}

fn server(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Imap => "The incoming mail server (IMAP)",
        Protocol::Smtp => "The outgoing mail server (SMTP)",
        Protocol::Pop3 => "The incoming mail server (POP3)",
    }
}

/// Why a login failed and what to do about it.
pub fn login_failure(error: &LoginError, context: &Context) -> Notice {
    let endpoint = match error.protocol {
        Protocol::Imap | Protocol::Pop3 => context.incoming,
        Protocol::Smtp => context.smtp,
    };
    let (host, port) = (&endpoint.host, endpoint.port);
    let server = server(error.protocol);
    let provider = context.provider.unwrap_or("This provider");
    match error.failure {
        LoginFailure::WrongPassword => match context.app_password {
            Some(link) => Notice::problem(
                "Wrong username or password",
                format!(
                    "{server} didn't accept them. {provider} needs an app password here, not \
                     your account password."
                ),
            )
            .with_link(Some(link)),
            None => Notice::problem(
                "Wrong username or password",
                format!("{server} didn't accept them. Check both, then try again."),
            ),
        },
        LoginFailure::AppPasswordRequired => Notice::problem(
            "The server asked for an app password",
            format!(
                "{provider} lets mail apps in with an app password: turn on two-step \
                 verification in your account if it isn't on, then create an app password for \
                 Sealwax and type it here."
            ),
        )
        .with_link(context.app_password),
        LoginFailure::ImapDisabled if error.protocol == Protocol::Pop3 => Notice::problem(
            "POP3 is turned off for this account",
            format!(
                "Turn on POP3 in {}'s web settings, then try again.",
                context.provider.unwrap_or("your provider")
            ),
        ),
        LoginFailure::ImapDisabled => Notice::problem(
            "IMAP is turned off for this account",
            format!(
                "Turn on IMAP in {}'s web settings, then try again.",
                context.provider.unwrap_or("your provider")
            ),
        )
        .with_link(context.enable_imap),
        LoginFailure::OAuthRequired => Notice::problem(
            "Only a browser sign-in works here",
            format!(
                "{server} doesn't take passwords from mail apps. Sealwax offers browser sign-in \
                 for Microsoft accounts only."
            ),
        ),
        LoginFailure::SignInDeclined => Notice::problem(
            "The sign-in was declined",
            "Microsoft didn't let Sealwax in. Sign in again and approve Sealwax; for a work \
             account, your organization's admin may have to approve it first."
                .to_owned(),
        ),
        LoginFailure::NoTls => Notice::problem(
            "No secure connection",
            format!(
                "{host} offers neither TLS nor STARTTLS on port {port}, and Sealwax never sends a \
                 password unencrypted. Check the server, port and security below."
            ),
        ),
        LoginFailure::Certificate if is_loopback(host) => Notice::problem(
            "The server's certificate isn't trusted",
            format!(
                "{host} is on this computer, and Sealwax signs in to it only with a certificate \
                 you trusted. Check settings again to see its certificate."
            ),
        ),
        LoginFailure::Certificate => Notice::problem(
            "The server's certificate isn't trusted",
            format!(
                "{host} showed a certificate Sealwax can't verify. Check the server name; if it \
                 is right, ask your provider about its certificate."
            ),
        ),
        LoginFailure::Unreachable => Notice::problem(
            "Couldn't reach the server",
            format!(
                "Nothing answers at {host} on port {port}. Check the server name and port, and \
                 that you're online."
            ),
        ),
        LoginFailure::Timeout => Notice::problem(
            "The server didn't answer in time",
            format!("{host} took too long to answer. Check that you're online, then try again."),
        ),
        LoginFailure::NoUidl => Notice::problem(
            "This POP3 server can't keep track of messages",
            format!(
                "{server} doesn't number its messages for good (no UIDL), so Sealwax would \
                 download everything again on every check and couldn't leave mail on the \
                 server. Use IMAP if your provider offers it."
            ),
        ),
        LoginFailure::Busy => Notice::problem(
            "The mailbox is busy",
            format!(
                "{server} says the mailbox is in use or wants a pause between sign-ins. Another \
                 mail app may be downloading from it: close that app, or try again in a few \
                 minutes."
            ),
        ),
        // RFC 2449's IN-USE: a POP3 mailbox takes one session at a time.
        LoginFailure::Other if error.protocol == Protocol::Pop3 => Notice::problem(
            "The server refused the sign-in",
            format!(
                "{server} reported a problem Sealwax doesn't recognise. Another mail app \
                 downloading from this mailbox at the same time can cause it: close that app, \
                 check the settings below, then try again."
            ),
        ),
        LoginFailure::Other => Notice::problem(
            "The server refused the sign-in",
            format!(
                "{server} reported a problem Sealwax doesn't recognise. Check the settings \
                 below, or try again later."
            ),
        ),
    }
}

/// Why a browser sign-in (ML-032) failed and what to do about it: each ends in signing in
/// again, which the step's button does. `provider` names the account's provider
/// ("Outlook.com") when detection knew it.
pub fn sign_in_failure(error: &LoginError, provider: Option<&str>) -> Notice {
    let server = server(error.protocol);
    match error.failure {
        LoginFailure::SignInDeclined => Notice::problem(
            "The sign-in was declined",
            "Microsoft didn't let Sealwax in. Sign in again and approve Sealwax; for a work \
             account, your organization's admin may have to approve it first.",
        ),
        LoginFailure::Timeout => Notice::problem(
            "The sign-in didn't finish in time",
            "Sealwax stopped waiting for the browser or for Microsoft's servers. Check that \
             you're online, then sign in again.",
        ),
        LoginFailure::WrongPassword => Notice::problem(
            "The sign-in wasn't accepted for mail",
            format!(
                "{server} refused the sign-in. Sign in again with the Microsoft account this \
                 address belongs to."
            ),
        ),
        LoginFailure::OAuthRequired | LoginFailure::AppPasswordRequired => Notice::problem(
            "The server refused this sign-in",
            format!(
                "{server} didn't take the sign-in from Microsoft. Sign in again; if it keeps \
                 happening, the account may not allow mail apps, which a work account's admin \
                 can change."
            ),
        ),
        LoginFailure::ImapDisabled => Notice::problem(
            "IMAP is turned off for this account",
            format!(
                "Turn on IMAP in {}'s settings, or ask your organization's admin, then sign in \
                 again.",
                provider.unwrap_or("Outlook.com")
            ),
        ),
        LoginFailure::Unreachable => Notice::problem(
            "Couldn't reach Microsoft",
            "Microsoft's servers didn't answer. Check that you're online, then sign in again.",
        ),
        LoginFailure::NoTls => Notice::problem(
            "No secure connection",
            format!(
                "{server} offered no secure connection, and Sealwax never signs in without one. \
                 Check your network, then sign in again."
            ),
        ),
        LoginFailure::Certificate => Notice::problem(
            "The server's certificate isn't trusted",
            format!(
                "{server} showed a certificate Sealwax can't verify. A network that inspects \
                 secure connections can cause this: try another network, then sign in again."
            ),
        ),
        LoginFailure::Other | LoginFailure::NoUidl | LoginFailure::Busy => Notice::problem(
            "The sign-in didn't work",
            format!(
                "{server} reported a problem Sealwax doesn't recognise. Sign in again, or try \
                 later."
            ),
        ),
    }
}

/// The Microsoft account signed in with isn't the address typed (issue #2): which to send from.
pub const IDENTITY_TITLE: &str = "Not the address you typed";

pub fn identity_body(signed_in: &str, typed: &str) -> String {
    format!(
        "You signed in to Microsoft as {signed_in}, but your mail would be sent from {typed}. \
         Microsoft refuses mail from an address your account isn't allowed to send from. Keep \
         {typed} only if it's an alias or a shared mailbox you may send from."
    )
}

pub fn send_from(address: &str) -> String {
    format!("Send from {address}")
}

pub fn keep_address(address: &str) -> String {
    format!("Keep {address}")
}

/// The note under "Waiting for the browser…" once a Microsoft sign-in has waited a minute: an
/// organization that doesn't let its users approve an unverified app leaves its own page open,
/// and the browser never comes back.
pub const STILL_WAITING: &str = "Still waiting for Microsoft. If the browser shows “Need admin \
     approval”, your organization must approve Sealwax first: see the user guide.";

/// The note in the Microsoft step while no client ID is in use (ADR-008).
pub const MICROSOFT_UNAVAILABLE: &str =
    "Microsoft sign-in isn't available in this build of Sealwax yet.";

/// How to make it available.
pub const MICROSOFT_CLIENT_ID_HINT: &str = "It needs a Microsoft client ID: set client_id under \
     [oauth.microsoft] in config.toml (Preferences opens it). docs/OAUTH-SETUP.md shows how to \
     register one.";

/// The note under the manual step's "Incoming mail" while POP3 is chosen (M8): where its mail
/// goes, and that the server keeps it unless `accounts.toml` says otherwise.
pub const POP3_NOTE: &str = "Sealwax downloads new mail to folders on this computer and \
     leaves it on the server. To delete it there after some days, set delete_after_days in \
     accounts.toml.";

/// The toast after an account signed in again.
pub fn signed_in_again(name: &str) -> String {
    format!("{name} is signed in again")
}

/// Why a password typed again (ML-048) was refused and what to do next. Only the password can
/// be changed on this step, so nothing points at the server settings.
pub fn password_again_failure(error: &LoginError, context: &Context) -> Notice {
    let endpoint = match error.protocol {
        Protocol::Imap | Protocol::Pop3 => context.incoming,
        Protocol::Smtp => context.smtp,
    };
    let (host, port) = (&endpoint.host, endpoint.port);
    let server = server(error.protocol);
    let provider = context.provider.unwrap_or("This provider");
    match error.failure {
        LoginFailure::WrongPassword => match context.app_password {
            Some(link) => Notice::problem(
                "Wrong password",
                format!(
                    "{server} didn't accept it. {provider} needs an app password here, not your \
                     account password."
                ),
            )
            .with_link(Some(link)),
            None => Notice::problem(
                "Wrong password",
                format!(
                    "{server} didn't accept it. Check it and try again; if you changed it at \
                     your provider, type the new one."
                ),
            ),
        },
        LoginFailure::OAuthRequired => Notice::problem(
            "Only a browser sign-in works here",
            format!(
                "{server} no longer takes passwords from mail apps. Sealwax offers browser \
                 sign-in for Microsoft accounts only."
            ),
        ),
        LoginFailure::NoTls => Notice::problem(
            "No secure connection",
            format!(
                "{host} offered no secure connection on port {port}, and Sealwax never sends a \
                 password without one. Check your network, then try again."
            ),
        ),
        LoginFailure::Certificate => Notice::problem(
            "The server's certificate isn't trusted",
            format!(
                "{host} showed a certificate Sealwax can't verify. A network that inspects \
                 secure connections can cause this: try another network, then try again."
            ),
        ),
        LoginFailure::Unreachable => Notice::problem(
            "Couldn't reach the server",
            format!(
                "Nothing answers at {host} on port {port}. Check that you're online, then try \
                 again."
            ),
        ),
        LoginFailure::Other => Notice::problem(
            "The password wasn't saved",
            format!(
                "{server} or the keyring reported a problem Sealwax doesn't recognise. Try \
                 again, or try later."
            ),
        ),
        LoginFailure::AppPasswordRequired
        | LoginFailure::ImapDisabled
        | LoginFailure::SignInDeclined
        | LoginFailure::Timeout
        | LoginFailure::NoUidl
        | LoginFailure::Busy => login_failure(error, context),
    }
}

/// The toast after a password typed again worked.
pub fn password_accepted(name: &str) -> String {
    format!("Password accepted for {name}")
}

/// What detection's dead ends say; `None` for settings that were found.
pub fn detection(detection: &Detection, domain: &str) -> Option<Notice> {
    Some(match detection {
        Detection::Found(_) => return None,
        Detection::Unsupported {
            provider,
            reason: UnsupportedReason::NoImap,
        } => Notice::problem(
            format!("{provider} doesn't work with mail apps"),
            format!(
                "{provider} offers no IMAP, so no mail app can connect to it. Use {provider}'s \
                 own app for this address."
            ),
        ),
        Detection::NoMail => Notice::problem(
            format!("{domain} receives no mail"),
            "The domain says it doesn't receive mail. Check the address for typos.",
        ),
        Detection::NotFound => Notice {
            tone: Tone::Info,
            title: format!("No settings found for {domain}"),
            body: "Copy them from your provider's help pages.".to_owned(),
            link: None,
        },
    })
}

/// The engine isn't attached (or stopped).
pub fn not_connected() -> Notice {
    Notice::problem(
        "Not connected to the mail engine",
        "This build of Sealwax has no mail engine attached yet, so it can't look up or check an \
         account.",
    )
}

/// Why saving a checked account failed.
pub fn account_error(error: &AccountError) -> Notice {
    match error {
        AccountError::Keyring => Notice::problem(
            "Couldn't save the password",
            "The keyring refused it. Make sure a Secret Service such as gnome-keyring is \
             running, then try again.",
        ),
        AccountError::SettingsFile => Notice::problem(
            "Couldn't save the account",
            "Sealwax couldn't write accounts.toml. Check that its settings folder is writable, \
             then try again.",
        ),
        AccountError::Store => Notice::problem(
            "Couldn't save the account",
            "The local mail store failed. Try again; if it keeps failing, the log has the \
             details.",
        ),
        AccountError::Exists => Notice::problem(
            "This account is already set up",
            "An account with this address is already in Sealwax.",
        ),
        AccountError::Missing => Notice::problem(
            "Couldn't save the account",
            "Something went wrong on the way. Try again.",
        ),
        AccountError::SignInExpired => Notice::problem(
            "The sign-in expired",
            "Sign in with Microsoft again, then add the account.",
        ),
    }
}

/// A source in the list the assistant shows while it looks.
pub fn source(source: DetectSource, domain: &str) -> String {
    match source {
        DetectSource::Preset => "Built-in providers".to_owned(),
        DetectSource::Autoconfig => format!("{domain} autoconfig"),
        DetectSource::Ispdb => "Mozilla's provider list".to_owned(),
        DetectSource::Mx => "DNS (MX)".to_owned(),
        DetectSource::Srv => "DNS (SRV)".to_owned(),
        DetectSource::Probe => "Common server names".to_owned(),
    }
}

/// `IMAP mail.example.test · 993 · TLS`, as the mock writes found settings.
pub fn endpoint(protocol: Protocol, endpoint: &Endpoint) -> String {
    let name = match protocol {
        Protocol::Imap => "IMAP",
        Protocol::Smtp => "SMTP",
        Protocol::Pop3 => "POP3",
    };
    format!(
        "{name} {} · {} · {}",
        endpoint.host,
        endpoint.port,
        security_name(endpoint.security)
    )
}

/// The found settings card: the provider, both servers and the username's form.
pub fn found(settings: &DetectedSettings, domain: &str) -> [String; 4] {
    let username = match settings.username {
        UsernameForm::Address => "Username: your full address",
        UsernameForm::LocalPart => "Username: the part before the @",
        UsernameForm::Other => "Username: the one your provider gave you",
    };
    [
        settings
            .provider
            .clone()
            .unwrap_or_else(|| domain.to_owned()),
        endpoint(Protocol::Imap, &settings.imap),
        endpoint(Protocol::Smtp, &settings.smtp),
        username.to_owned(),
    ]
}

/// The note under found settings that only the domain's mail host gave (S4: show and
/// confirm before sending the password).
pub fn confirm_note(settings: &DetectedSettings) -> String {
    match &settings.provider {
        Some(provider) => format!(
            "Found through {provider}, which handles this domain's mail. Check the username \
             before you sign in."
        ),
        None => "Found through the domain's mail host. Check the username before you sign in."
            .to_owned(),
    }
}

/// The password hint for a provider that wants an app password.
pub fn app_password_hint(provider: Option<&str>) -> String {
    format!(
        "{} needs an app password, not your account password.",
        provider.unwrap_or("This provider")
    )
}

/// The password label and hint for a provider reached through its Bridge on this computer
/// (Proton Mail, issue #9).
pub const BRIDGE_PASSWORD: &str = "Bridge password";

pub fn bridge_password_hint(provider: Option<&str>) -> String {
    let provider = provider.unwrap_or("The provider");
    format!(
        "Start {provider} Bridge on this computer, then type the password it shows for this \
         address, not your {provider} password."
    )
}

/// The trust step (issue #9): a server on this computer whose certificate the system doesn't
/// trust, such as Proton Mail Bridge's.
pub const CERTIFICATE_TITLE: &str = "Trust this server's certificate?";
pub const TRUST: &str = "Trust and Sign In";
pub const DONT_TRUST: &str = "Don't Trust";

pub fn certificate_body(protocol: Protocol, endpoint: &Endpoint) -> String {
    let server = match protocol {
        Protocol::Imap => "incoming mail server (IMAP)",
        Protocol::Pop3 => "incoming mail server (POP3)",
        Protocol::Smtp => "outgoing mail server (SMTP)",
    };
    format!(
        "The {server} at {host}, port {port}, runs on this computer, and your system doesn't \
         trust its certificate. Programs such as Proton Mail Bridge make their own. Trust it if \
         you run one here: Sealwax will accept this certificate from this server and no other.",
        host = endpoint.host,
        port = endpoint.port,
    )
}

/// The fingerprint in two lines of 16 bytes, as `openssl x509 -fingerprint -sha256` prints it
/// in one.
pub fn fingerprint_lines(pin: &CertificatePin) -> String {
    let text = pin.to_string();
    format!("{}\n{}", &text[..47], &text[48..])
}

/// The hint for a provider whose IMAP must be turned on first.
pub fn enable_imap_hint(provider: Option<&str>) -> String {
    format!(
        "Turn on IMAP in {}'s settings first.",
        provider.unwrap_or("your provider")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::{AuthMethod, Security};

    fn endpoint(host: &str, port: u16) -> Endpoint {
        Endpoint {
            host: host.to_owned(),
            port,
            security: Security::Tls,
            certificate: None,
        }
    }

    const FAILURES: [LoginFailure; 11] = [
        LoginFailure::WrongPassword,
        LoginFailure::AppPasswordRequired,
        LoginFailure::ImapDisabled,
        LoginFailure::OAuthRequired,
        LoginFailure::NoTls,
        LoginFailure::Certificate,
        LoginFailure::Unreachable,
        LoginFailure::Timeout,
        LoginFailure::NoUidl,
        LoginFailure::Busy,
        LoginFailure::Other,
    ];

    #[test]
    fn every_failure_says_what_to_do_next() {
        let (imap, smtp) = (
            endpoint("imap.example.test", 993),
            endpoint("smtp.example.test", 465),
        );
        let link = HelpLink {
            label: "Create an app password".to_owned(),
            url: "https://example.test/app-passwords".to_owned(),
        };
        let context = Context {
            provider: Some("iCloud"),
            incoming: &imap,
            smtp: &smtp,
            app_password: Some(&link),
            enable_imap: None,
        };
        let mut titles = Vec::new();
        for failure in FAILURES {
            for protocol in [Protocol::Imap, Protocol::Smtp] {
                let notice = login_failure(&LoginError { protocol, failure }, &context);
                assert_eq!(notice.tone, Tone::Problem);
                assert!(notice.body.ends_with('.'), "{failure:?}: {}", notice.body);
                assert!(!notice.title.ends_with('.'), "{failure:?}");
                titles.push(notice.title);
            }
        }
        titles.dedup();
        assert_eq!(titles.len(), FAILURES.len(), "one heading per failure");

        let app_password = login_failure(
            &LoginError {
                protocol: Protocol::Imap,
                failure: LoginFailure::AppPasswordRequired,
            },
            &context,
        );
        assert_eq!(app_password.title, "The server asked for an app password");
        assert!(app_password.body.starts_with("iCloud lets mail apps in"));
        assert_eq!(app_password.link.as_ref(), Some(&link));

        let unreachable = login_failure(
            &LoginError {
                protocol: Protocol::Smtp,
                failure: LoginFailure::Unreachable,
            },
            &context,
        );
        assert!(unreachable.body.contains("smtp.example.test on port 465"));
        let wrong = login_failure(
            &LoginError {
                protocol: Protocol::Smtp,
                failure: LoginFailure::WrongPassword,
            },
            &Context {
                app_password: None,
                provider: None,
                ..context
            },
        );
        assert_eq!(
            wrong.body,
            "The outgoing mail server (SMTP) didn't accept them. Check both, then try again."
        );
        assert_eq!(wrong.link, None);
    }

    #[test]
    fn a_pop3_failure_names_the_pop3_server() {
        let (pop3, smtp) = (
            endpoint("pop.example.test", 995),
            endpoint("smtp.example.test", 465),
        );
        let context = Context {
            provider: None,
            incoming: &pop3,
            smtp: &smtp,
            app_password: None,
            enable_imap: None,
        };
        let mut titles = Vec::new();
        for failure in FAILURES {
            let error = LoginError {
                protocol: Protocol::Pop3,
                failure,
            };
            let notice = login_failure(&error, &context);
            assert_eq!(notice.tone, Tone::Problem);
            assert!(notice.body.ends_with('.'), "{failure:?}: {}", notice.body);
            // Only a server Sealwax can't use as POP3 points to IMAP.
            assert!(
                failure == LoginFailure::NoUidl || !notice.body.contains("IMAP"),
                "{failure:?}: {}",
                notice.body
            );
            // An app password is the account's, not the server's.
            assert!(
                failure == LoginFailure::AppPasswordRequired
                    || notice.body.contains("POP3")
                    || notice.body.contains("pop.example.test"),
                "{failure:?}: {}",
                notice.body
            );
            titles.push(notice.title);
            let again = password_again_failure(&error, &context);
            assert!(
                failure == LoginFailure::NoUidl || !again.body.contains("IMAP"),
                "{failure:?}: {}",
                again.body
            );
        }
        titles.dedup();
        assert_eq!(titles.len(), FAILURES.len(), "one heading per failure");
        let failure = |failure| {
            login_failure(
                &LoginError {
                    protocol: Protocol::Pop3,
                    failure,
                },
                &context,
            )
        };
        assert_eq!(
            failure(LoginFailure::WrongPassword).body,
            "The incoming mail server (POP3) didn't accept them. Check both, then try again."
        );
        let off = failure(LoginFailure::ImapDisabled);
        assert_eq!(off.title, "POP3 is turned off for this account");
        assert_eq!(
            off.body,
            "Turn on POP3 in your provider's web settings, then try again."
        );
        assert!(
            failure(LoginFailure::Unreachable)
                .body
                .contains("Nothing answers at pop.example.test on port 995.")
        );
        assert!(
            failure(LoginFailure::Other)
                .body
                .contains("Another mail app downloading from this mailbox")
        );
        // IMAP's texts stay as they were.
        let imap = login_failure(
            &LoginError {
                protocol: Protocol::Imap,
                failure: LoginFailure::Other,
            },
            &context,
        );
        assert_eq!(
            imap.body,
            "The incoming mail server (IMAP) reported a problem Sealwax doesn't recognise. \
             Check the settings below, or try again later."
        );
        assert!(POP3_NOTE.contains("leaves it on the server"));
    }

    #[test]
    fn every_sign_in_failure_ends_in_signing_in_again() {
        // Microsoft's sign-in reaches IMAP and SMTP, never POP3's failures.
        let failures = [LoginFailure::SignInDeclined]
            .into_iter()
            .chain(FAILURES)
            .filter(|failure| !matches!(failure, LoginFailure::NoUidl | LoginFailure::Busy))
            .collect::<Vec<_>>();
        let mut titles = Vec::new();
        for &failure in &failures {
            let notice = sign_in_failure(
                &LoginError {
                    protocol: Protocol::Smtp,
                    failure,
                },
                Some("Outlook.com"),
            );
            assert_eq!(notice.tone, Tone::Problem);
            assert!(notice.body.ends_with('.'), "{failure:?}: {}", notice.body);
            assert!(
                notice.body.to_lowercase().contains("sign in again"),
                "{failure:?}: {}",
                notice.body
            );
            assert_eq!(notice.link, None);
            titles.push(notice.title);
        }
        titles.sort();
        titles.dedup();
        assert_eq!(
            titles.len(),
            failures.len() - 1,
            "one heading per failure; the server's two refusals share one"
        );
        let declined = sign_in_failure(
            &LoginError {
                protocol: Protocol::Imap,
                failure: LoginFailure::SignInDeclined,
            },
            None,
        );
        assert!(declined.body.contains("admin may have to approve it"));
    }

    #[test]
    fn a_password_typed_again_is_refused_without_pointing_at_settings() {
        let (imap, smtp) = (
            endpoint("imap.example.test", 993),
            endpoint("smtp.example.test", 465),
        );
        let link = HelpLink {
            label: "Create an app password".to_owned(),
            url: "https://example.test/app-passwords".to_owned(),
        };
        let context = Context {
            provider: Some("iCloud"),
            incoming: &imap,
            smtp: &smtp,
            app_password: Some(&link),
            enable_imap: None,
        };
        let mut titles = Vec::new();
        for failure in FAILURES {
            for protocol in [Protocol::Imap, Protocol::Smtp] {
                let notice = password_again_failure(&LoginError { protocol, failure }, &context);
                assert_eq!(notice.tone, Tone::Problem);
                assert!(notice.body.ends_with('.'), "{failure:?}: {}", notice.body);
                for settings in ["below", "Check both", "server name"] {
                    assert!(
                        !notice.body.contains(settings),
                        "{failure:?}: {}",
                        notice.body
                    );
                }
                titles.push(notice.title);
            }
        }
        titles.dedup();
        assert_eq!(titles.len(), FAILURES.len(), "one heading per failure");
        let wrong = |context: &Context| {
            password_again_failure(
                &LoginError {
                    protocol: Protocol::Imap,
                    failure: LoginFailure::WrongPassword,
                },
                context,
            )
        };
        assert_eq!(wrong(&context).link.as_ref(), Some(&link));
        assert!(
            wrong(&context)
                .body
                .contains("iCloud needs an app password")
        );
        let plain = wrong(&Context {
            provider: None,
            app_password: None,
            ..context
        });
        assert_eq!(
            plain.body,
            "The incoming mail server (IMAP) didn't accept it. Check it and try again; if you \
             changed it at your provider, type the new one."
        );
        assert_eq!(password_accepted("Ola"), "Password accepted for Ola");
    }

    #[test]
    fn a_server_on_this_computer_asks_for_trust() {
        let bridge = Endpoint {
            security: sealwax_domain::account::Security::StartTls,
            ..endpoint("127.0.0.1", 1143)
        };
        let body = certificate_body(Protocol::Imap, &bridge);
        assert!(body.starts_with(
            "The incoming mail server (IMAP) at 127.0.0.1, port 1143, runs on this computer"
        ));
        let pin = CertificatePin([0xab; 32]);
        let lines = fingerprint_lines(&pin);
        assert_eq!(lines.lines().count(), 2);
        assert_eq!(lines.replace('\n', ":"), pin.to_string());
        let error = LoginError {
            protocol: Protocol::Imap,
            failure: LoginFailure::Certificate,
        };
        let smtp = endpoint("smtp.example.test", 465);
        let context = Context {
            provider: Some("Proton Mail"),
            incoming: &bridge,
            smtp: &smtp,
            app_password: None,
            enable_imap: None,
        };
        assert!(
            login_failure(&error, &context)
                .body
                .starts_with("127.0.0.1 is on this computer")
        );
        assert_eq!(
            bridge_password_hint(Some("Proton Mail")),
            "Start Proton Mail Bridge on this computer, then type the password it shows for \
             this address, not your Proton Mail password."
        );
    }

    #[test]
    fn dead_ends_say_why() {
        assert_eq!(
            detection(&Detection::NoMail, "example.test").unwrap().title,
            "example.test receives no mail"
        );
        let tuta = detection(
            &Detection::Unsupported {
                provider: "Tuta".to_owned(),
                reason: UnsupportedReason::NoImap,
            },
            "example.test",
        )
        .unwrap();
        assert_eq!(tuta.title, "Tuta doesn't work with mail apps");
        let not_found = detection(&Detection::NotFound, "example.test").unwrap();
        assert_eq!(not_found.tone, Tone::Info);
        assert_eq!(not_found.title, "No settings found for example.test");
    }

    #[test]
    fn found_settings_read_as_in_the_mock() {
        let settings = DetectedSettings {
            imap: endpoint("mail.example.test", 993),
            smtp: endpoint("mail.example.test", 465),
            username: UsernameForm::Address,
            second_username: None,
            auth: AuthMethod::Password,
            provider: Some("Fjordpost".to_owned()),
            confirm: false,
            app_password: None,
            enable_imap: None,
            source: DetectSource::Autoconfig,
        };
        assert_eq!(
            found(&settings, "example.test"),
            [
                "Fjordpost",
                "IMAP mail.example.test · 993 · TLS",
                "SMTP mail.example.test · 465 · TLS",
                "Username: your full address",
            ]
        );
        assert_eq!(
            source(DetectSource::Autoconfig, "example.test"),
            "example.test autoconfig"
        );
    }
}
