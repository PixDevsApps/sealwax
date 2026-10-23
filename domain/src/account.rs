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
    /// POP3 (M8, PRD A9).
    Pop3,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// The one certificate this server is trusted with although the system doesn't trust it
    /// (Proton Mail Bridge, issue #9). Only a server on this computer can have one
    /// ([`Endpoint::pinned`]).
    pub certificate: Option<CertificatePin>,
}

impl Endpoint {
    /// The pinned certificate, honoured only while the host is a loopback name or address.
    pub fn pinned(&self) -> Option<CertificatePin> {
        self.certificate.filter(|_| is_loopback(&self.host))
    }
}

/// Whether `host` names this computer: `localhost`, or an address in 127.0.0.0/8 or `::1`.
/// The engine also checks that the connection it made reached a loopback address.
pub fn is_loopback(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// The SHA-256 of a server certificate's DER bytes: what a pinned certificate is compared by.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CertificatePin(pub [u8; 32]);

impl CertificatePin {
    /// Reads 64 hexadecimal digits, in either case, with or without a colon between bytes and
    /// a `sha256:` in front (`openssl x509 -fingerprint -sha256` prints them so).
    pub fn parse(text: &str) -> Option<CertificatePin> {
        let text = text.trim();
        let text = text
            .get(..7)
            .filter(|prefix| prefix.eq_ignore_ascii_case("sha256:"))
            .map_or(text, |_| &text[7..]);
        let text = text
            .get(..19)
            .filter(|prefix| prefix.eq_ignore_ascii_case("sha256 fingerprint="))
            .map_or(text, |_| &text[19..]);
        let digits: Vec<u8> = text.bytes().filter(|byte| *byte != b':').collect();
        if digits.len() != 64 || text.starts_with(':') || text.ends_with(':') {
            return None;
        }
        if text.contains(':') && (text.len() != 95 || text.contains("::")) {
            return None;
        }
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(digits.chunks(2)) {
            let pair = std::str::from_utf8(pair).ok()?;
            *byte = u8::from_str_radix(pair, 16).ok()?;
        }
        Some(CertificatePin(bytes))
    }

    /// The bytes as upper-case hexadecimal pairs, `separator` between them.
    pub fn hex(&self, separator: &str) -> String {
        self.0
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(separator)
    }
}

impl fmt::Display for CertificatePin {
    /// `AB:CD:…`, as `accounts.toml` and the assistant write it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex(":"))
    }
}

impl fmt::Debug for CertificatePin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertificatePin({})", self.hex(""))
    }
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

/// How an account receives its mail (M8, PRD A9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Receive {
    /// IMAP: the server's folders, kept in sync.
    #[default]
    Imap,
    /// POP3: new mail downloaded from the server into the account's local folders (ML-087,
    /// ML-088).
    Pop3(Pop3Policy),
}

/// What happens on the POP3 server to mail Sealwax has downloaded (PRD A9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pop3Policy {
    /// Downloaded mail stays on the server.
    pub leave_on_server: bool,
    /// With `leave_on_server`, mail is deleted from the server this many days after it was
    /// downloaded; `None` keeps it there.
    pub delete_after_days: Option<u32>,
}

impl Receive {
    /// The incoming server's protocol.
    pub const fn protocol(self) -> Protocol {
        match self {
            Self::Imap => Protocol::Imap,
            Self::Pop3(_) => Protocol::Pop3,
        }
    }
}

impl Default for Pop3Policy {
    /// Leave on server, never delete: the lead's recommendation at M8's start.
    fn default() -> Self {
        Self {
            leave_on_server: true,
            delete_after_days: None,
        }
    }
}

/// A muted colour that marks an account in the sidebar (the user's UI tweak, 2026-10-08): a
/// fixed hue whose lightness and chroma the theme layer sets per mode, so its text reads on
/// light and dark themes alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountColor {
    Rose,
    Clay,
    Olive,
    Sage,
    Teal,
    Slate,
    Iris,
    Mauve,
}

impl AccountColor {
    /// Every colour, in the order the setup assistant shows them: round the colour wheel.
    pub const ALL: [Self; 8] = [
        Self::Rose,
        Self::Clay,
        Self::Olive,
        Self::Sage,
        Self::Teal,
        Self::Slate,
        Self::Iris,
        Self::Mauve,
    ];

    /// Its name in `accounts.toml` and in CSS: `"sage"`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rose => "rose",
            Self::Clay => "clay",
            Self::Olive => "olive",
            Self::Sage => "sage",
            Self::Teal => "teal",
            Self::Slate => "slate",
            Self::Iris => "iris",
            Self::Mauve => "mauve",
        }
    }

    /// What the setup assistant and screen readers call it: `"Sage"`.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Rose => "Rose",
            Self::Clay => "Clay",
            Self::Olive => "Olive",
            Self::Sage => "Sage",
            Self::Teal => "Teal",
            Self::Slate => "Slate",
            Self::Iris => "Iris",
            Self::Mauve => "Mauve",
        }
    }

    /// Its hue in OKLCH, in degrees.
    pub const fn hue(self) -> f64 {
        match self {
            Self::Rose => 15.0,
            Self::Clay => 55.0,
            Self::Olive => 95.0,
            Self::Sage => 145.0,
            Self::Teal => 195.0,
            Self::Slate => 245.0,
            Self::Iris => 290.0,
            Self::Mauve => 335.0,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|color| color.name().eq_ignore_ascii_case(name.trim()))
    }

    /// The first colour none of `used` has, else the first.
    pub fn first_unused(used: &[Self]) -> Self {
        Self::ALL
            .into_iter()
            .find(|color| !used.contains(color))
            .unwrap_or(Self::Rose)
    }
}

/// One account as `accounts.toml` holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSettings {
    pub key: AccountKey,
    /// The sender name on outgoing mail.
    pub name: String,
    /// What Sealwax calls the account in the sidebar, banners and menus; `None` shows the
    /// address ([`AccountSettings::title`]).
    pub label: Option<String>,
    /// The account's colour in the sidebar; `None` keeps the theme's plain heading.
    pub color: Option<AccountColor>,
    pub address: String,
    /// The server mail comes from: IMAP's, or POP3's when `receive` says so.
    pub incoming: Endpoint,
    pub receive: Receive,
    pub smtp: Endpoint,
    /// The login name for both the incoming server and SMTP.
    pub username: String,
    pub auth: AuthMethod,
    /// The signature under mail sent from `address` (M6, ML-070): plain text, `None` for none.
    pub signature: Option<String>,
    /// The other addresses the account sends from (aliases, PRD A6), from
    /// `[[account.identity]]` (M6, ML-070).
    pub identities: Vec<Identity>,
}

impl AccountSettings {
    /// What Sealwax calls the account: its label, else its address.
    pub fn title(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.address)
    }
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
    /// The provider offers no IMAP (Tuta).
    NoImap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detection {
    Found(Box<DetectedSettings>),
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
    /// SMTP AUTH is turned off for the mailbox or its organization (Microsoft 365's
    /// "SmtpClientAuthentication is disabled", issue #21): mail comes in, but none can be sent.
    SmtpAuthDisabled,
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
    /// A POP3 server that doesn't number messages for good (no UIDL), so Sealwax can't tell what
    /// it has downloaded (M8).
    NoUidl,
    /// The mailbox is in use by another session, or the server wants a pause between logins
    /// (POP3's `[IN-USE]`, `[LOGIN-DELAY]`, `[SYS/TEMP]`; M8).
    Busy,
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
    use super::{AccountColor, CertificatePin, Endpoint, Security, UsernameForm};

    #[test]
    fn loopback_hosts() {
        for host in [
            "localhost",
            "LOCALHOST.",
            "127.0.0.1",
            "127.8.9.1",
            "::1",
            "[::1]",
        ] {
            assert!(super::is_loopback(host), "{host}");
        }
        for host in [
            "localhost.example.test",
            "128.0.0.1",
            "0.0.0.0",
            "::",
            "::ffff:127.0.0.1x",
            "imap.example.test",
            "",
        ] {
            assert!(!super::is_loopback(host), "{host}");
        }
    }

    #[test]
    fn a_pin_is_honoured_only_on_this_computer() {
        let pin = CertificatePin([7; 32]);
        let mut endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: 1143,
            security: Security::StartTls,
            certificate: Some(pin),
        };
        assert_eq!(endpoint.pinned(), Some(pin));
        endpoint.host = "imap.example.test".into();
        assert_eq!(endpoint.pinned(), None);
    }

    #[test]
    fn certificate_pins_read_and_write() {
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from(index * 7 + 160).unwrap_or(0) ^ 0x5a;
        }
        let pin = CertificatePin(bytes);
        let shown = pin.to_string();
        assert_eq!(shown.len(), 95);
        assert_eq!(CertificatePin::parse(&shown), Some(pin));
        assert_eq!(CertificatePin::parse(&shown.to_lowercase()), Some(pin));
        assert_eq!(CertificatePin::parse(&pin.hex("")), Some(pin));
        assert_eq!(CertificatePin::parse(&format!("sha256:{shown}")), Some(pin));
        assert_eq!(
            CertificatePin::parse(&format!("sha256 Fingerprint={shown}\n")),
            Some(pin)
        );
        for bad in [
            "",
            "AB",
            &shown[..94],
            &format!("{shown}:"),
            &format!(":{}", &shown[1..]),
            &shown.replacen(':', "", 3),
            &format!("G{}", &shown[1..]),
            &format!("{}0", pin.hex("")),
        ] {
            assert_eq!(CertificatePin::parse(bad), None, "{bad}");
        }
    }

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

    #[test]
    fn account_colors_read_by_name_and_go_round_the_wheel() {
        for color in AccountColor::ALL {
            assert_eq!(AccountColor::from_name(color.name()), Some(color));
            assert_eq!(color.title().to_lowercase(), color.name());
        }
        assert_eq!(AccountColor::from_name(" Sage "), Some(AccountColor::Sage));
        assert_eq!(AccountColor::from_name("green"), None);
        let hues = AccountColor::ALL.map(AccountColor::hue);
        assert!(hues.windows(2).all(|pair| pair[1] - pair[0] >= 40.0));
        assert!(hues[0] + 360.0 - hues[7] >= 40.0);
    }

    #[test]
    fn the_first_unused_color_is_offered() {
        assert_eq!(AccountColor::first_unused(&[]), AccountColor::Rose);
        assert_eq!(
            AccountColor::first_unused(&[AccountColor::Rose, AccountColor::Olive]),
            AccountColor::Clay
        );
        assert_eq!(
            AccountColor::first_unused(&AccountColor::ALL),
            AccountColor::Rose
        );
    }
}
