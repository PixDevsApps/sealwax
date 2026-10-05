//! Microsoft sign-in (ML-032; ADR-008 and its S5 amendment), ported from spike S5, which proved
//! the flow live on 2026-10-05: the authorization-code flow with PKCE (S256) in the system
//! browser and a loopback redirect, the code exchange and refreshes at Microsoft's `common`
//! endpoints, the username from the ID token, and a per-account cache of access tokens that
//! refreshes from the keyring and stores every rotated refresh token back.
//!
//! Nothing here logs or displays a token, a code, a verifier, an ID token, a username or the
//! authorize URL. Errors carry no server text: only the OAuth error's class and Microsoft's
//! numeric AADSTS code.

mod access;
mod error;
mod flow;
mod id_token;
mod token;

pub use access::{AccessToken, AccessTokens, Fetched, MARGIN};
pub use error::OAuthError;
pub use flow::Authorization;
pub use id_token::{CONSUMER_TENANT, Identity};
pub use token::Tokens;

use crate::net::Trust;
use std::fmt;
use std::time::Duration;
use url::Url;

/// What Sealwax asks Microsoft for: IMAP and SMTP as the signed-in user, a refresh token, and an
/// ID token that names the user. The Outlook scopes come first, because Microsoft issues the
/// access token for the resource of the first scope when a request names several.
pub const MICROSOFT_SCOPES: &str = "https://outlook.office.com/IMAP.AccessAsUser.All \
    https://outlook.office.com/SMTP.Send offline_access openid profile";

/// Connect and answer together, for every token request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// An authorization server's two endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    authorize: Url,
    token: Url,
}

impl Provider {
    /// Microsoft's `common` endpoints, for work, school and personal accounts alike.
    pub fn microsoft() -> Self {
        Self::new(
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        )
        .expect("Microsoft's endpoints are HTTPS URLs")
    }

    /// Endpoints must be HTTPS; plain HTTP is accepted only on a loopback address, for tests.
    pub fn new(authorize: &str, token: &str) -> Result<Self, OAuthError> {
        Ok(Self {
            authorize: endpoint(authorize)?,
            token: endpoint(token)?,
        })
    }
}

fn endpoint(text: &str) -> Result<Url, OAuthError> {
    let url = Url::parse(text).map_err(|_| OAuthError::Endpoint)?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err(OAuthError::Endpoint),
    }
}

/// A registered public client (no secret: desktop clients use PKCE) and the HTTP client its
/// token requests go through: rustls with the platform verifier, no proxy, no redirects.
pub struct Client {
    id: String,
    provider: Provider,
    http: reqwest::Client,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(id: String, provider: Provider) -> Result<Self, OAuthError> {
        let tls = Trust::Platform
            .client_config()
            .map_err(|_| OAuthError::Setup)?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("Sealwax/", env!("CARGO_PKG_VERSION")))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .tls_backend_preconfigured((*tls).clone())
            .connect_timeout(HTTP_TIMEOUT)
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|_| OAuthError::Setup)?;
        Ok(Self { id, provider, http })
    }
}

/// A finished sign-in: who signed in, and the tokens. Its debug form shows neither.
pub struct Granted {
    pub identity: Identity,
    /// Always with a refresh token.
    pub tokens: Tokens,
}

impl fmt::Debug for Granted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Granted")
            .field("identity", &self.identity)
            .field("tokens", &self.tokens)
            .finish()
    }
}

#[cfg(test)]
mod tests;
