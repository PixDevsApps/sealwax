//! The login check behind `Command::Verify` (PRD A3): log in to the incoming server (IMAP, or
//! POP3 for a POP3 account, M8), then to SMTP, saving nothing. When the incoming server refuses
//! the first username, `second_username` is tried once, so a check makes at most two incoming
//! login attempts; SMTP then logs in with the username that worked. A failure names the protocol
//! and why, never the server's text.

use crate::api::Password;
use crate::imap::{self, Credentials};
use crate::net::Trust;
use crate::pop3;
use crate::smtp;
use rustls::ClientConfig;
use sealwax_domain::account::{
    AccountSettings, AuthMethod, LoginError, LoginFailure, Protocol, Receive,
};
use std::sync::Arc;

fn incoming_error(settings: &AccountSettings, failure: LoginFailure) -> LoginError {
    LoginError {
        protocol: settings.receive.protocol(),
        failure,
    }
}

/// Logs in to the account's incoming server and out again.
async fn incoming(
    settings: &AccountSettings,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
) -> Result<(), LoginFailure> {
    match settings.receive {
        Receive::Imap => match imap::connect(&settings.incoming, credentials, tls).await {
            Ok(connection) => {
                connection.logout().await;
                Ok(())
            }
            Err(error) => {
                tracing::debug!(%error, "imap login check failed");
                Err(error.failure())
            }
        },
        Receive::Pop3(_) => pop3::check_login(&settings.incoming, credentials, tls).await,
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
        return Err(incoming_error(settings, LoginFailure::OAuthRequired));
    }
    let tls = trust
        .client_config()
        .map_err(|_| incoming_error(settings, LoginFailure::Other))?;
    let mut usernames = vec![settings.username.as_str()];
    usernames.extend(second_username.filter(|second| *second != settings.username));
    let mut username = None;
    for (attempt, candidate) in usernames.iter().enumerate() {
        let credentials = Credentials::Password {
            username: candidate,
            password: password.expose(),
        };
        match incoming(settings, credentials, tls.clone()).await {
            Ok(()) => {
                username = Some(*candidate);
                break;
            }
            Err(LoginFailure::WrongPassword) if attempt + 1 < usernames.len() => {
                tracing::debug!(attempt, "login refused; trying the other username");
            }
            Err(failure) => return Err(incoming_error(settings, failure)),
        }
    }
    let username = username.ok_or_else(|| incoming_error(settings, LoginFailure::WrongPassword))?;
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

#[cfg(test)]
mod tests;
