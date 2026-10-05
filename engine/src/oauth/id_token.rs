//! The ID token, for the username: S5 found that Outlook's SMTP accepts only the identity the
//! token was issued to as XOAUTH2's `user=`, which is the ID token's `preferred_username`.
//!
//! The ID token comes straight from the token endpoint over TLS (HTTPS only, the platform
//! verifier) in exchange for a code and this client's PKCE verifier, so OpenID Connect Core
//! 3.1.3.7 lets the TLS server check stand in for the signature, which isn't checked. What is
//! checked: the issuer is Microsoft's for the token's own tenant
//! (`https://login.microsoftonline.com/{tid}/v2.0`), the audience is this client (and `azp` too
//! when there are several), it hasn't expired, and it names a user. No nonce is sent, so none is
//! checked.

use super::error::OAuthError;
use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::Deserialize;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// The tenant of every personal Microsoft account (Outlook.com, Hotmail): its `tid`.
pub const CONSUMER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";
const ISSUER: &str = "https://login.microsoftonline.com/";
/// Clock skew allowed on `exp`.
const LEEWAY: Duration = Duration::from_secs(300);

/// base64url, with or without padding.
const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Who signed in. Its debug form doesn't show the username.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    /// `preferred_username`: XOAUTH2's `user=` for IMAP and SMTP.
    pub username: String,
    /// `tid`, lower case.
    pub tenant: String,
}

impl Identity {
    /// A personal Microsoft account rather than a work or school one.
    pub fn consumer(&self) -> bool {
        self.tenant == CONSUMER_TENANT
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("consumer", &self.consumer())
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Audience,
    azp: Option<String>,
    exp: f64,
    tid: Option<String>,
    preferred_username: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

/// Reads and checks the ID token's claims (above) for `client_id` at `now`.
pub(crate) fn check(
    id_token: &str,
    client_id: &str,
    now: SystemTime,
) -> Result<Identity, OAuthError> {
    let mut parts = id_token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(OAuthError::IdToken("not a JWT"));
    };
    let json = Zeroizing::new(
        BASE64URL
            .decode(payload)
            .map_err(|_| OAuthError::IdToken("not base64url"))?,
    );
    let claims: Claims =
        serde_json::from_slice(&json).map_err(|_| OAuthError::IdToken("unreadable claims"))?;
    let tenant = claims
        .tid
        .filter(|tenant| guid(tenant))
        .ok_or(OAuthError::IdToken("no tenant"))?;
    if claims.iss != format!("{ISSUER}{tenant}/v2.0") {
        return Err(OAuthError::IdToken("not Microsoft's issuer for its tenant"));
    }
    let ours = match &claims.aud {
        Audience::One(audience) => audience == client_id,
        Audience::Many(audiences) => {
            audiences.iter().any(|audience| audience == client_id)
                && (audiences.len() == 1 || claims.azp.as_deref() == Some(client_id))
        }
    };
    if !ours {
        return Err(OAuthError::IdToken("issued to another client"));
    }
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64());
    if claims.exp + LEEWAY.as_secs_f64() <= now {
        return Err(OAuthError::IdToken("expired"));
    }
    let username = claims
        .preferred_username
        .map(|username| username.trim().to_owned())
        .filter(|username| !username.is_empty())
        .ok_or(OAuthError::IdToken("no username"))?;
    Ok(Identity {
        username,
        tenant: tenant.to_ascii_lowercase(),
    })
}

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` in hex digits.
fn guid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        })
}
