//! The browser half (RFC 8252): PKCE (RFC 7636, S256), the state, the sign-in page's URL, and
//! the loopback redirect that receives the code.

use super::error::{OAuthError, aadsts_in, authorize_error};
use super::id_token;
use super::{Client, Granted, MICROSOFT_SCOPES};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use zeroize::Zeroizing;

/// How long one connection to the loopback may take to send its request.
const REQUEST_WAIT: Duration = Duration::from_secs(10);
/// The longest request head the loopback reads.
const MAX_REQUEST: usize = 16 * 1024;
/// Connections the loopback serves at once; more are closed unanswered.
const MAX_CONNECTIONS: usize = 16;

/// Ports tried for a loopback that holds both 127.0.0.1 and ::1.
const BIND_ATTEMPTS: usize = 8;

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Sealwax</title>\
    <p>Sealwax has the sign-in. You can close this tab and return to Sealwax.";
const FAILED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Sealwax</title>\
    <p>The sign-in didn't finish. You can close this tab and return to Sealwax.";

/// `bytes` random bytes, base64url without padding.
pub(crate) fn random(bytes: usize) -> Result<Zeroizing<String>, OAuthError> {
    let mut buffer = Zeroizing::new(vec![0u8; bytes]);
    getrandom::fill(&mut buffer).map_err(|_| OAuthError::Random)?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(&*buffer)))
}

/// The S256 code challenge of a verifier.
pub(crate) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// What the browser brought back to the redirect.
pub(crate) enum Callback {
    Code(Zeroizing<String>),
    /// The OAuth error word and the AADSTS number of its description.
    Error {
        error: String,
        code: Option<u32>,
    },
}

/// The redirect's listeners, bound before the browser opens so the redirect URI has a real port:
/// 127.0.0.1 and ::1 on the same port, because a browser may resolve `localhost` to either. A
/// port whose ::1 side another program holds is given up for another; without IPv6, 127.0.0.1
/// alone. (Whoever else got the redirect would hold a code that is useless without this
/// client's PKCE verifier.)
pub(crate) struct Loopback {
    listeners: Vec<TcpListener>,
    port: u16,
}

impl Loopback {
    pub(crate) async fn bind() -> Result<Self, OAuthError> {
        let failed = |error: io::Error| OAuthError::Loopback(error.kind());
        let mut attempts = 0;
        loop {
            attempts += 1;
            let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .map_err(failed)?;
            let port = v4.local_addr().map_err(failed)?.port();
            match TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))).await {
                Ok(v6) => {
                    return Ok(Self {
                        listeners: vec![v4, v6],
                        port,
                    });
                }
                Err(error)
                    if error.kind() == io::ErrorKind::AddrInUse && attempts < BIND_ATTEMPTS => {}
                Err(_) => {
                    return Ok(Self {
                        listeners: vec![v4],
                        port,
                    });
                }
            }
        }
    }

    /// `http://localhost:<port>` without a path: the registered `http://localhost` matches it
    /// whatever the port, and a trailing slash wouldn't match (S5).
    pub(crate) fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// Waits up to `wait` for the browser's redirect with this `state`. Every request is
    /// answered; one for another path or with another state is ignored, so nothing but the
    /// browser that holds the state can end the wait.
    pub(crate) async fn wait(self, state: &str, wait: Duration) -> Result<Callback, OAuthError> {
        let (found, mut received) = mpsc::channel(1);
        let state: Arc<str> = Arc::from(state);
        let mut accepting = JoinSet::new();
        for listener in self.listeners {
            accepting.spawn(accept(listener, state.clone(), found.clone()));
        }
        drop(found);
        // Returning drops `accepting`: the listeners close and open connections end.
        match tokio::time::timeout(wait, received.recv()).await {
            Ok(Some(callback)) => Ok(callback),
            Ok(None) => Err(OAuthError::Loopback(io::ErrorKind::ConnectionAborted)),
            Err(_) => Err(OAuthError::BrowserTimeout(wait)),
        }
    }
}

async fn accept(listener: TcpListener, state: Arc<str>, found: mpsc::Sender<Callback>) {
    let mut connections = JoinSet::new();
    while let Ok((stream, _)) = listener.accept().await {
        while connections.try_join_next().is_some() {}
        if connections.len() < MAX_CONNECTIONS {
            connections.spawn(answer(stream, state.clone(), found.clone()));
        }
    }
}

/// Answers one request. The page is written before the code is handed on, so the browser gets
/// it before the listeners close.
async fn answer(mut stream: TcpStream, state: Arc<str>, found: mpsc::Sender<Callback>) {
    let Ok(Some(target)) = tokio::time::timeout(REQUEST_WAIT, request_target(&mut stream)).await
    else {
        return;
    };
    let Some(callback) = callback(&target, &state) else {
        let _ = respond(&mut stream, "404 Not Found", "").await;
        return;
    };
    let page = match callback {
        Callback::Code(_) => DONE_PAGE,
        Callback::Error { .. } => FAILED_PAGE,
    };
    let _ = respond(&mut stream, "200 OK", page).await;
    let _ = stream.shutdown().await;
    let _ = found.try_send(callback);
}

/// The target of a GET request, once its whole head has arrived.
async fn request_target(stream: &mut TcpStream) -> Option<Zeroizing<String>> {
    let mut head = Zeroizing::new(Vec::with_capacity(MAX_REQUEST));
    let mut buffer = Zeroizing::new([0u8; 2048]);
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer[..]).await.ok()?;
        if read == 0 || head.len() + read > MAX_REQUEST {
            return None;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let line = head.split(|&byte| byte == b'\r').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    if parts.next()? != "GET" {
        return None;
    }
    Some(Zeroizing::new(parts.next()?.to_owned()))
}

/// The redirect's query, when it is for `/` and carries our state and a code or an error.
pub(crate) fn callback(target: &str, state: &str) -> Option<Callback> {
    let (path, query) = target.split_once('?')?;
    if path != "/" {
        return None;
    }
    let mut code = None;
    let mut returned = None;
    let mut error = None;
    let mut number = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match &*key {
            "code" => code = Some(Zeroizing::new(value.into_owned())),
            "state" => returned = Some(Zeroizing::new(value.into_owned())),
            "error" => error = Some(value.into_owned()),
            "error_description" => number = aadsts_in(&value),
            _ => {}
        }
    }
    if returned.as_deref().map(String::as_str) != Some(state) {
        return None;
    }
    match (error, code) {
        (Some(error), _) => Some(Callback::Error {
            error,
            code: number,
        }),
        (None, Some(code)) => Some(Callback::Code(code)),
        (None, None) => None,
    }
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-security-policy: default-src 'none'\r\nreferrer-policy: no-referrer\r\n\
         cache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

/// A sign-in under way: the page to open, and what finishing it needs.
pub struct Authorization {
    url: String,
    redirect_uri: String,
    state: Zeroizing<String>,
    verifier: Zeroizing<String>,
    loopback: Loopback,
}

impl fmt::Debug for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Authorization(..)")
    }
}

impl Authorization {
    /// The sign-in page, with the state and the code challenge: hand it to the browser, never to
    /// a log.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Client {
    /// Binds the loopback redirect and builds the sign-in page's URL. The address is never part
    /// of it (no `login_hint`); `prompt=select_account` lets the user pick the account.
    pub async fn begin(&self) -> Result<Authorization, OAuthError> {
        let loopback = Loopback::bind().await?;
        // 64 bytes: an 86-character verifier (RFC 7636 allows 43–128).
        let verifier = random(64)?;
        let state = random(16)?;
        let redirect_uri = loopback.redirect_uri();
        let mut url = self.provider.authorize.clone();
        url.query_pairs_mut()
            .append_pair("client_id", &self.id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", MICROSOFT_SCOPES)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        Ok(Authorization {
            url: url.into(),
            redirect_uri,
            state,
            verifier,
            loopback,
        })
    }

    /// Waits up to `wait` for the browser, exchanges the code with the verifier and checks the
    /// ID token. An error from the sign-in page, no answer in time, a refused exchange, no
    /// refresh token or an ID token that doesn't check out ends it.
    pub async fn finish(
        &self,
        authorization: Authorization,
        wait: Duration,
    ) -> Result<Granted, OAuthError> {
        let Authorization {
            redirect_uri,
            state,
            verifier,
            loopback,
            ..
        } = authorization;
        let code = match loopback.wait(&state, wait).await? {
            Callback::Code(code) => code,
            Callback::Error { error, code } => return Err(authorize_error(&error, code)),
        };
        let tokens = self.exchange(&code, &redirect_uri, &verifier).await?;
        let id_token = tokens
            .id_token
            .as_ref()
            .ok_or(OAuthError::IdToken("missing"))?;
        let identity = id_token::check(id_token, &self.id, SystemTime::now())?;
        if tokens.refresh.is_none() {
            return Err(OAuthError::NoRefreshToken);
        }
        Ok(Granted { identity, tokens })
    }
}
