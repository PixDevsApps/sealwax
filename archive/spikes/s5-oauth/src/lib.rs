//! Spike S5 (M0): OAuth2 authorization code flow with PKCE and a loopback redirect, for
//! Google and Microsoft mail. Measurement code only; production crates never depend on it.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("random number generator: {0}")]
    Random(String),
    #[error("loopback listener: {0}")]
    Listener(#[from] std::io::Error),
    #[error("no answer from the browser within {0:?}")]
    Timeout(Duration),
    #[error("the sign-in page returned an error: {0}")]
    Denied(String),
    #[error("the redirect carried the wrong state")]
    StateMismatch,
    #[error("the redirect carried no code")]
    NoCode,
    #[error("token endpoint: {0}")]
    Token(String),
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub name: &'static str,
    pub authorize_url: String,
    pub token_url: String,
    pub scope: &'static str,
    /// Microsoft matches `http://localhost` with any port; Google wants the loopback IP.
    pub redirect_host: &'static str,
    pub extra: &'static [(&'static str, &'static str)],
}

impl Provider {
    pub fn google() -> Self {
        Self {
            name: "google",
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scope: "https://mail.google.com/",
            redirect_host: "127.0.0.1",
            extra: &[("access_type", "offline"), ("prompt", "consent")],
        }
    }

    pub fn microsoft() -> Self {
        Self {
            name: "microsoft",
            authorize_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            scope: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access",
            redirect_host: "localhost",
            extra: &[("prompt", "select_account")],
        }
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    pub id: String,
    /// Google desktop clients have a non-confidential secret; Microsoft public clients none.
    pub secret: Option<String>,
}

fn random_token(bytes: usize) -> Result<String, OAuthError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|error| OAuthError::Random(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// RFC 7636: a 64-byte verifier (86 characters) and its S256 challenge.
pub struct Pkce {
    pub verifier: Zeroizing<String>,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Result<Self, OAuthError> {
        let verifier = Zeroizing::new(random_token(64)?);
        let challenge = challenge_for(&verifier);
        Ok(Self {
            verifier,
            challenge,
        })
    }
}

pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A loopback listener bound before the browser opens, so the redirect URI has a real port.
pub struct Loopback {
    listener: TcpListener,
    pub redirect_uri: String,
}

impl Loopback {
    pub async fn bind(host: &str) -> Result<Self, OAuthError> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            // No trailing slash: the registered `http://localhost` has no path (Microsoft ignores
            // only the port of a localhost redirect).
            redirect_uri: format!("http://{host}:{port}"),
        })
    }

    /// Waits for the browser's redirect, answers it with a plain page and returns the query.
    pub async fn wait(self, timeout: Duration) -> Result<HashMap<String, String>, OAuthError> {
        tokio::time::timeout(timeout, self.accept())
            .await
            .map_err(|_| OAuthError::Timeout(timeout))?
    }

    async fn accept(self) -> Result<HashMap<String, String>, OAuthError> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).await?;
            let mut header = String::new();
            while reader.read_line(&mut header).await? > 2 {
                header.clear();
            }
            let target = request_line.split_whitespace().nth(1).unwrap_or("/");
            let query = parse_query(target);
            let mut stream = reader.into_inner();
            if query.is_empty() {
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await?;
                continue;
            }
            let body = "<!doctype html><meta charset=utf-8><title>Sealwax</title><p>You can close this tab and return to Sealwax.</p>";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-security-policy: default-src 'none'\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await?;
            return Ok(query);
        }
    }
}

pub fn parse_query(target: &str) -> HashMap<String, String> {
    let Some((_, query)) = target.split_once('?') else {
        return HashMap::new();
    };
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

/// Everything needed to send the user to the sign-in page and finish the flow afterwards.
pub struct Authorization {
    pub url: String,
    pub state: String,
    pub pkce: Pkce,
    pub loopback: Loopback,
}

pub async fn begin(
    provider: &Provider,
    client: &Client,
    login_hint: Option<&str>,
) -> Result<Authorization, OAuthError> {
    let loopback = Loopback::bind(provider.redirect_host).await?;
    let pkce = Pkce::new()?;
    let state = random_token(16)?;
    let mut url = url::Url::parse(&provider.authorize_url)
        .map_err(|error| OAuthError::Token(error.to_string()))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.id)
            .append_pair("redirect_uri", &loopback.redirect_uri)
            .append_pair("scope", provider.scope)
            .append_pair("state", &state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        for (key, value) in provider.extra {
            query.append_pair(key, value);
        }
        if let Some(hint) = login_hint {
            query.append_pair("login_hint", hint);
        }
    }
    Ok(Authorization {
        url: url.into(),
        state,
        pkce,
        loopback,
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    error_description: Option<String>,
}

pub struct Tokens {
    pub access: Zeroizing<String>,
    pub refresh: Option<Zeroizing<String>>,
    pub expires_in: Option<Duration>,
    /// The scopes the server granted, which may be fewer than those asked for.
    pub scope: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("refresh", &self.refresh.is_some())
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("mail-dev-spike (automated)")
        .timeout(Duration::from_secs(20))
        .build()
        .expect("reqwest client")
}

async fn token_request(
    http: &reqwest::Client,
    provider: &Provider,
    form: &[(&str, &str)],
) -> Result<Tokens, OAuthError> {
    let response = http
        .post(&provider.token_url)
        .form(form)
        .send()
        .await
        .map_err(|error| OAuthError::Token(error.without_url().to_string()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| OAuthError::Token(error.without_url().to_string()))?;
    if !status.is_success() {
        let message = serde_json::from_slice::<TokenError>(&body)
            .map(|e| format!("{}: {}", e.error, e.error_description.unwrap_or_default()))
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(OAuthError::Token(message));
    }
    let parsed: TokenResponse =
        serde_json::from_slice(&body).map_err(|error| OAuthError::Token(error.to_string()))?;
    Ok(Tokens {
        access: Zeroizing::new(parsed.access_token),
        refresh: parsed.refresh_token.map(Zeroizing::new),
        expires_in: parsed.expires_in.map(Duration::from_secs),
        scope: parsed.scope,
    })
}

/// Waits for the redirect, checks the state and exchanges the code (with the PKCE verifier).
pub async fn finish(
    http: &reqwest::Client,
    provider: &Provider,
    client: &Client,
    authorization: Authorization,
    timeout: Duration,
) -> Result<Tokens, OAuthError> {
    let Authorization {
        state,
        pkce,
        loopback,
        ..
    } = authorization;
    let redirect_uri = loopback.redirect_uri.clone();
    let query = loopback.wait(timeout).await?;
    if let Some(error) = query.get("error") {
        return Err(OAuthError::Denied(error.clone()));
    }
    if query.get("state") != Some(&state) {
        return Err(OAuthError::StateMismatch);
    }
    let code = Zeroizing::new(query.get("code").ok_or(OAuthError::NoCode)?.clone());
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client.id.as_str()),
        ("code_verifier", pkce.verifier.as_str()),
    ];
    if let Some(secret) = &client.secret {
        form.push(("client_secret", secret.as_str()));
    }
    token_request(http, provider, &form).await
}

pub async fn refresh(
    http: &reqwest::Client,
    provider: &Provider,
    client: &Client,
    refresh_token: &str,
) -> Result<Tokens, OAuthError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client.id.as_str()),
    ];
    if let Some(secret) = &client.secret {
        form.push(("client_secret", secret.as_str()));
    }
    token_request(http, provider, &form).await
}

/// The SASL XOAUTH2 initial response (before base64), for IMAP, SMTP and POP3.
pub fn xoauth2(user: &str, access_token: &str) -> Zeroizing<String> {
    Zeroizing::new(format!("user={user}\x01auth=Bearer {access_token}\x01\x01"))
}

#[cfg(test)]
mod tests;
