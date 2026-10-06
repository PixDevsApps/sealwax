//! Account settings and the setup assistant's vocabulary: server endpoints, what autodetect found
//! and why a login failed. Plain values shared by `accounts.toml`, the engine and the app; no
//! secret is ever part of them.

use std::fmt;

/// An account's stable name in `accounts.toml` and the keyring. Unlike the store's `AccountId`,
/// it survives a deleted cache.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountKey(pub String);

impl fmt::Display for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a connection gets its TLS. There is no plaintext mode (SECURITY.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Security {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    Tls,
    /// A plain connection upgraded with STARTTLS, which must succeed (IMAP 143, SMTP 587).
    StartTls,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Imap,
    Smtp,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OAuthProvider {
    Microsoft,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AuthMethod {
    /// A password or app password, kept in the keyring.
    Password,
    /// A browser sign-in; the refresh token is kept in the keyring.
    OAuth(OAuthProvider),
}

/// One account as `accounts.toml` holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSettings {
    pub key: AccountKey,
    /// The sender name on outgoing mail and the account's label in the sidebar.
    pub name: String,
    pub address: String,
    pub imap: Endpoint,
    pub smtp: Endpoint,
    /// The login name for both IMAP and SMTP.
    pub username: String,
    pub auth: AuthMethod,
    /// The signature under mail sent from `address` (M6, ML-070): plain text, `None` for none.
    pub signature: Option<String>,
    /// The other addresses the account sends from (aliases, PRD A6), from
    /// `[[account.identity]]` (M6, ML-070).
    pub identities: Vec<Identity>,
}

/// An address the account sends from besides its own (PRD A6): an alias with its own sender
/// name and signature. A reply goes out from the identity the message was sent to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The sender name; the account's when the file leaves it out.
    pub name: String,
    pub address: String,
    /// Plain text; `None` for none.
    pub signature: Option<String>,
}

/// The login name a detected setting expects, relative to the address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UsernameForm {
    Address,
    LocalPart,
    /// A provider-assigned name that the address doesn't reveal: the user types it.
    Other,
}

impl UsernameForm {
    /// The username for `address`; `None` for `Other`.
    pub fn apply(self, address: &str) -> Option<String> {
        match self {
            UsernameForm::Address => Some(address.to_owned()),
            UsernameForm::LocalPart => address
                .rsplit_once('@')
                .map(|(local, _)| local.to_owned())
                .filter(|local| !local.is_empty()),
            UsernameForm::Other => None,
        }
    }
}

/// Where autodetect looks, in the order the assistant lists them while it waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DetectSource {
    /// Built-in provider settings, by domain or by the MX's hosting platform.
    Preset,
    /// The domain's own HTTPS autoconfig.
    Autoconfig,
    /// Mozilla's ISPDB (the domain only).
    Ispdb,
    /// The provider behind the domain's MX records.
    Mx,
    /// DNS SRV records (RFC 6186, RFC 8314).
    Srv,
    /// TLS handshakes with likely host names.
    Probe,
}

/// A provider page the assistant links to: where to make an app password or turn IMAP on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpLink {
    pub label: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedSettings {
    pub imap: Endpoint,
    pub smtp: Endpoint,
    /// The username of the first login attempt.
    pub username: UsernameForm,
    /// The other form, tried as the second and last attempt (S4b's extended rubric).
    pub second_username: Option<UsernameForm>,
    pub auth: AuthMethod,
    /// The provider's name when known ("Fastmail"), for the assistant's text.
    pub provider: Option<String>,
    /// Found only through the MX operator's hosts: show the settings with the username field for
    /// confirmation before logging in (M0 gate decision).
    pub confirm: bool,
    /// The provider needs an app password instead of the account password.
    pub app_password: Option<HelpLink>,
    /// The provider needs IMAP turned on in its web settings first.
    pub enable_imap: Option<HelpLink>,
    pub source: DetectSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// Reachable only through the provider's own bridge (Proton).
    BridgeOnly,
    /// The provider offers no IMAP (Tuta).
    NoImap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detection {
    Found(DetectedSettings),
    Unsupported {
        provider: String,
        reason: UnsupportedReason,
    },
    /// The domain says it receives no mail (a null MX).
    NoMail,
    /// Nothing usable: the assistant opens the manual settings.
    NotFound,
}

/// Why a login check failed, in the terms the assistant's messages need (PRD A3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoginFailure {
    /// The server rejected the username or password.
    WrongPassword,
    /// The provider wants an app password (its reply says so, or the preset knows it).
    AppPasswordRequired,
    /// IMAP is turned off for this account in the provider's settings.
    ImapDisabled,
    /// The server accepts only a browser sign-in.
    OAuthRequired,
    /// The browser sign-in was declined: by the user, or by their organization's consent policy.
    SignInDeclined,
    /// The server offers neither TLS nor STARTTLS, or STARTTLS failed.
    NoTls,
    /// The server's certificate didn't verify.
    Certificate,
    /// The host doesn't resolve or refuses the connection.
    Unreachable,
    Timeout,
    /// Anything else the server or the connection did.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginError {
    pub protocol: Protocol,
    pub failure: LoginFailure,
}

#[cfg(test)]
mod tests {
    use super::UsernameForm;

    #[test]
    fn username_forms_apply_to_an_address() {
        let address = "test@example.test";
        assert_eq!(
            UsernameForm::Address.apply(address).as_deref(),
            Some(address)
        );
        assert_eq!(
            UsernameForm::LocalPart.apply(address).as_deref(),
            Some("test")
        );
        assert_eq!(UsernameForm::Other.apply(address), None);
        assert_eq!(UsernameForm::LocalPart.apply("@example.test"), None);
    }
}
