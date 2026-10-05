//! The token endpoint (RFC 6749 4.1.3 and 6): the code exchange with the PKCE verifier, and
//! refreshes. Answers are read into wiped buffers, at most 64 KiB; a refusal is reduced to its
//! OAuth error class and AADSTS number.

use super::Client;
use super::error::{Grant, OAuthError, aadsts_in, token_error};
use serde::Deserialize;
use std::fmt;
use std::time::Duration;
use zeroize::Zeroizing;

const MAX_ANSWER: usize = 64 * 1024;
/// An answer without `expires_in` gets this short a lifetime.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(600);

/// What the token endpoint issued. Its debug form shows no token.
pub struct Tokens {
    pub access: Zeroizing<String>,
    /// Microsoft rotates it on every refresh (S5): the new one replaces the stored one.
    pub refresh: Option<Zeroizing<String>>,
    pub lifetime: Duration,
    pub id_token: Option<Zeroizing<String>>,
    /// The scopes granted, which can be fewer than those asked for.
    pub scope: Option<String>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("refresh", &self.refresh.is_some())
            .field("lifetime", &self.lifetime)
            .field("id_token", &self.id_token.is_some())
            .finish_non_exhaustive()
    }
}

impl Tokens {
    /// The granted scopes include `scope`.
    pub fn granted(&self, scope: &str) -> bool {
        self.scope
            .as_deref()
            .is_some_and(|granted| granted.split(' ').any(|one| one == scope))
    }
}

#[derive(Deserialize)]
struct Answer {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<Lifetime>,
    id_token: Option<String>,
    scope: Option<String>,
}

/// `expires_in` is a number, but some servers send it as a string.
#[derive(Deserialize)]
#[serde(untagged)]
enum Lifetime {
    Seconds(u64),
    Text(String),
}

#[derive(Deserialize)]
struct Refusal {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    error_codes: Vec<u32>,
}

impl Client {
    pub(crate) async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<Tokens, OAuthError> {
        self.request(
            Grant::Code,
            &[
                ("client_id", &self.id),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ],
        )
        .await
    }

    /// A new access token (and, from Microsoft, a new refresh token) for a refresh token. Sent
    /// as S5 sent it, without `scope`, which keeps the grant's scopes.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, OAuthError> {
        self.request(
            Grant::Refresh,
            &[
                ("client_id", &self.id),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
        .await
    }

    async fn request(&self, grant: Grant, form: &[(&str, &str)]) -> Result<Tokens, OAuthError> {
        let body = Zeroizing::new(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(form)
                .finish(),
        );
        let mut response = self
            .http
            .post(self.provider.token.clone())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body.as_bytes().to_vec())
            .send()
            .await
            .map_err(transport)?;
        let status = response.status();
        let mut answer = Zeroizing::new(Vec::with_capacity(MAX_ANSWER));
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if answer.len() + chunk.len() > MAX_ANSWER {
                return Err(OAuthError::Response);
            }
            answer.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let refusal: Refusal =
                serde_json::from_slice(&answer).map_err(|_| OAuthError::Response)?;
            let code = refusal
                .error_codes
                .first()
                .copied()
                .or_else(|| refusal.error_description.as_deref().and_then(aadsts_in));
            return Err(token_error(grant, &refusal.error, code));
        }
        let answer: Answer = serde_json::from_slice(&answer).map_err(|_| OAuthError::Response)?;
        let lifetime = match answer.expires_in {
            Some(Lifetime::Seconds(seconds)) => Some(seconds),
            Some(Lifetime::Text(text)) => text.trim().parse().ok(),
            None => None,
        }
        .map_or(DEFAULT_LIFETIME, Duration::from_secs);
        Ok(Tokens {
            access: Zeroizing::new(answer.access_token),
            refresh: answer.refresh_token.map(Zeroizing::new),
            lifetime,
            id_token: answer.id_token.map(Zeroizing::new),
            scope: answer.scope,
        })
    }
}

/// A request that got no answer: a timeout, a certificate that didn't verify, or no connection.
fn transport(error: reqwest::Error) -> OAuthError {
    if error.is_timeout() {
        return OAuthError::Timeout;
    }
    let invalid = |error: &(dyn std::error::Error + 'static)| {
        matches!(
            error.downcast_ref::<rustls::Error>(),
            Some(rustls::Error::InvalidCertificate(_))
        )
    };
    let mut source = std::error::Error::source(&error);
    while let Some(inner) = source {
        let wrapped = inner
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
            .is_some_and(|io| invalid(io));
        if invalid(inner) || wrapped {
            return OAuthError::Certificate;
        }
        source = inner.source();
    }
    OAuthError::Unreachable
}
