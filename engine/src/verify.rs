//! The login check behind `Command::Verify` (PRD A3): log in to IMAP, then to SMTP, saving
//! nothing. When the server refuses the first username, `second_username` is tried once, so a
//! check makes at most two IMAP login attempts; SMTP then logs in with the username that
//! worked. A failure names the protocol and why, never the server's text.

use crate::api::Password;
use crate::imap::{self, Credentials, ImapError};
use crate::net::Trust;
use crate::smtp;
use sealwax_domain::account::{AccountSettings, AuthMethod, LoginError, LoginFailure, Protocol};

fn imap_error(failure: LoginFailure) -> LoginError {
    LoginError {
        protocol: Protocol::Imap,
        failure,
    }
}

/// Checks a password account's settings. `Ok` carries the username that logged in. Accounts
/// that sign in with OAuth are checked by the sign-in itself (ML-032), so no password is sent
/// for them.
pub async fn verify(
    settings: &AccountSettings,
    second_username: Option<&str>,
    password: &Password,
    trust: &Trust,
) -> Result<String, LoginError> {
    if let AuthMethod::OAuth(_) = settings.auth {
        return Err(imap_error(LoginFailure::OAuthRequired));
    }
    let tls = trust
        .client_config()
        .map_err(|_| imap_error(LoginFailure::Other))?;
    let mut usernames = vec![settings.username.as_str()];
    usernames.extend(second_username.filter(|second| *second != settings.username));
    let mut username = None;
    for (attempt, candidate) in usernames.iter().enumerate() {
        let credentials = Credentials::Password {
            username: candidate,
            password: password.expose(),
        };
        match imap::connect(&settings.imap, credentials, tls.clone()).await {
            Ok(connection) => {
                connection.logout().await;
                username = Some(*candidate);
                break;
            }
            Err(ImapError::Rejected(LoginFailure::WrongPassword))
                if attempt + 1 < usernames.len() =>
            {
                tracing::debug!(attempt, "imap login refused; trying the other username");
            }
            Err(error) => {
                tracing::debug!(attempt, %error, "imap login check failed");
                return Err(imap_error(error.failure()));
            }
        }
    }
    let username = username.ok_or_else(|| imap_error(LoginFailure::WrongPassword))?;
    let credentials = Credentials::Password {
        username,
        password: password.expose(),
    };
    smtp::check_login(&settings.smtp, credentials, trust)
        .await
        .map_err(|failure| LoginError {
            protocol: Protocol::Smtp,
            failure,
        })?;
    Ok(username.to_owned())
}
