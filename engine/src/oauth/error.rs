//! What can go wrong in a sign-in or a refresh, and what it means for the user. Server texts
//! (`error_description`) are read only for Microsoft's AADSTS number and then dropped.

use sealwax_domain::account::LoginFailure;
use std::io;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("an endpoint isn't an HTTPS URL")]
    Endpoint,
    #[error("the HTTP client couldn't be set up")]
    Setup,
    #[error("no random numbers")]
    Random,
    #[error("the loopback redirect couldn't listen ({0})")]
    Loopback(io::ErrorKind),
    #[error("no answer from the browser within {0:?}")]
    BrowserTimeout(Duration),
    #[error("the sign-in was declined{}", aadsts(.0))]
    Declined(Option<u32>),
    #[error("the sign-in page answered with an error{}", aadsts(.0))]
    Authorize(Option<u32>),
    #[error("the token endpoint couldn't be reached")]
    Unreachable,
    #[error("the token endpoint's certificate didn't verify")]
    Certificate,
    #[error("the token endpoint didn't answer in time")]
    Timeout,
    #[error("the refresh token was refused{}", aadsts(.0))]
    RefreshRefused(Option<u32>),
    #[error("the token endpoint refused the request{}", aadsts(.0))]
    Refused(Option<u32>),
    #[error("the token endpoint's answer couldn't be read")]
    Response,
    #[error("no refresh token came back")]
    NoRefreshToken,
    #[error("the ID token doesn't check out: {0}")]
    IdToken(&'static str),
}

fn aadsts(code: &Option<u32>) -> String {
    code.map(|code| format!(" (AADSTS{code})"))
        .unwrap_or_default()
}

impl OAuthError {
    /// What the setup assistant or the sidebar tells the user.
    pub fn failure(&self) -> LoginFailure {
        match self {
            Self::BrowserTimeout(_) | Self::Timeout => LoginFailure::Timeout,
            Self::Declined(_) => LoginFailure::SignInDeclined,
            Self::RefreshRefused(_) => LoginFailure::OAuthRequired,
            Self::Unreachable => LoginFailure::Unreachable,
            Self::Certificate => LoginFailure::Certificate,
            _ => LoginFailure::Other,
        }
    }
}

/// Which grant a token request made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grant {
    Code,
    Refresh,
}

/// Microsoft's AADSTS codes for consent that the user or their organization didn't give
/// (learn.microsoft.com, Entra error codes): 65001 not consented, 65004 declined, 90094 and
/// 90095 an admin's consent required.
const CONSENT: [u32; 4] = [65001, 65004, 90094, 90095];

fn consent(error: &str, code: Option<u32>) -> bool {
    matches!(error, "access_denied" | "consent_required")
        || code.is_some_and(|code| CONSENT.contains(&code))
}

/// An error the sign-in page sent to the redirect (RFC 6749 4.1.2.1).
pub(crate) fn authorize_error(error: &str, code: Option<u32>) -> OAuthError {
    if consent(error, code) {
        OAuthError::Declined(code)
    } else {
        OAuthError::Authorize(code)
    }
}

/// An error the token endpoint answered with (RFC 6749 5.2). A refused refresh, for whatever
/// reason the grant is gone (expired, revoked, consent withdrawn, a new sign-in demanded), means
/// the account must sign in again.
pub(crate) fn token_error(grant: Grant, error: &str, code: Option<u32>) -> OAuthError {
    match grant {
        Grant::Refresh
            if matches!(
                error,
                "invalid_grant" | "interaction_required" | "consent_required"
            ) =>
        {
            OAuthError::RefreshRefused(code)
        }
        Grant::Code if consent(error, code) => OAuthError::Declined(code),
        _ => OAuthError::Refused(code),
    }
}

/// The first AADSTS number in an error description ("AADSTS65004: User declined …").
pub(crate) fn aadsts_in(text: &str) -> Option<u32> {
    let start = text.find("AADSTS")? + "AADSTS".len();
    let digits: String = text[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}
