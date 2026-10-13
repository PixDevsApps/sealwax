//! A fake Microsoft for the engine's sign-in tests, on 127.0.0.1 only: the sign-in page and the
//! token endpoint over plain HTTP (which the engine accepts only on a loopback address), and
//! IMAP and SMTP servers over TLS with a throwaway CA. The servers take XOAUTH2 with the access
//! tokens the token endpoint issued; SMTP, like Outlook's (S5), only as the token's identity.

#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use sealwax_domain::account::{Endpoint, Security};
use sealwax_engine::net::Trust;
use sealwax_engine::oauth::{CONSUMER_TENANT, Provider};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub const CLIENT: &str = "mock-client-0001";

/// A CA in a scratch folder, trusted through `trust`, and a certificate for 127.0.0.1 it signed.
pub struct Tls {
    _dir: tempfile::TempDir,
    pub trust: Trust,
    pub server: Arc<ServerConfig>,
}

pub fn tls() -> Tls {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.signed_by(&key, &ca).unwrap();
    let server =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ca.pem");
    std::fs::write(&path, ca.pem()).unwrap();
    Tls {
        _dir: dir,
        trust: Trust::Roots(path),
        server: Arc::new(server),
    }
}

/// What the fake Microsoft knows and has seen.
pub struct State {
    /// The ID token's `preferred_username`: who signs in next.
    pub username: String,
    pub tenant: String,
    /// `expires_in` of new access tokens.
    pub lifetime: u64,
    /// The sign-in page answers `access_denied` (the user declined).
    pub decline: bool,
    /// IMAP refuses every token.
    pub imap_refuses: bool,
    /// Refresh tokens the token endpoint takes; a refresh replaces the one it used.
    pub refresh: HashSet<String>,
    /// Access tokens IMAP and SMTP take.
    pub access: HashSet<String>,
    /// Every grant asked for: `code`, or `refresh <token>`.
    pub grants: Vec<String>,
    /// Every code, token, ID token and verifier issued or seen, for greps.
    pub secrets: Vec<String>,
    issued: u32,
    codes: HashMap<String, (String, String)>,
}

impl State {
    fn next(&mut self, kind: &str) -> String {
        self.issued += 1;
        let value = format!("Sealwax-mock-{kind}-{}-7c41e9", self.issued);
        self.secrets.push(value.clone());
        value
    }

    /// The latest `kind` (`code`, `access` or `refresh`) issued.
    pub fn latest(&self, kind: &str) -> Option<String> {
        let marker = format!("-{kind}-");
        self.secrets
            .iter()
            .rev()
            .find(|secret| secret.starts_with("Sealwax-mock-") && secret.contains(&marker))
            .cloned()
    }

    /// The refresh token the latest grant issued.
    pub fn latest_refresh(&self) -> Option<String> {
        self.latest("refresh")
    }
}

pub struct Microsoft {
    pub port: u16,
    state: Arc<Mutex<State>>,
}

impl Microsoft {
    pub async fn start(username: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            username: username.to_owned(),
            tenant: CONSUMER_TENANT.to_owned(),
            lifetime: 3600,
            decline: false,
            imap_refuses: false,
            refresh: HashSet::new(),
            access: HashSet::new(),
            grants: Vec::new(),
            secrets: Vec::new(),
            issued: 0,
            codes: HashMap::new(),
        }));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(http(stream, shared.clone()));
            }
        });
        Self { port, state }
    }

    pub fn provider(&self) -> Provider {
        Provider::new(
            &format!("http://127.0.0.1:{}/authorize", self.port),
            &format!("http://127.0.0.1:{}/token", self.port),
        )
        .unwrap()
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn id_token(state: &mut State) -> String {
    let claims = serde_json::json!({
        "iss": format!("https://login.microsoftonline.com/{}/v2.0", state.tenant),
        "aud": CLIENT,
        "exp": now() + 3600,
        "iat": now(),
        "tid": state.tenant,
        "preferred_username": state.username,
        "ver": "2.0",
    });
    let token = format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
        URL_SAFE_NO_PAD.encode("signature")
    );
    state.secrets.push(token.clone());
    token
}

fn json(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn tokens(state: &mut State) -> String {
    let access = state.next("access");
    let refresh = state.next("refresh");
    state.access.insert(access.clone());
    state.refresh.insert(refresh.clone());
    let body = serde_json::json!({
        "token_type": "Bearer",
        "access_token": access,
        "refresh_token": refresh,
        "expires_in": state.lifetime,
        "scope": "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send",
        "id_token": id_token(state),
    });
    json("200 OK", &body.to_string())
}

fn refused(error: &str, code: u32) -> String {
    let body = serde_json::json!({
        "error": error,
        "error_description": format!("AADSTS{code}: refused by the mock for someone@example.test"),
        "error_codes": [code],
    });
    json("400 Bad Request", &body.to_string())
}

async fn http(stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await.is_err() {
        return;
    }
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.unwrap_or(0) <= 2 {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let response = if let Some(query) = target.strip_prefix("/authorize?") {
        let query: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let mut state = state.lock().unwrap();
        assert!(!query.contains_key("login_hint"));
        let redirect = &query["redirect_uri"];
        let answer = if state.decline {
            "error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent"
                .to_owned()
        } else {
            let code = state.next("code");
            state.codes.insert(
                code.clone(),
                (query["code_challenge"].clone(), redirect.clone()),
            );
            format!("code={code}")
        };
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {redirect}?{answer}&state={}\r\n\
             content-length: 0\r\nconnection: close\r\n\r\n",
            query["state"]
        )
    } else {
        let form: HashMap<String, String> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        let mut state = state.lock().unwrap();
        assert_eq!(form["client_id"], CLIENT);
        match form["grant_type"].as_str() {
            "authorization_code" => {
                state.grants.push("code".into());
                state.secrets.push(form["code_verifier"].clone());
                let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(&form["code_verifier"]));
                match state.codes.remove(&form["code"]) {
                    Some((expected, redirect))
                        if expected == challenge && redirect == form["redirect_uri"] =>
                    {
                        tokens(&mut state)
                    }
                    _ => refused("invalid_grant", 70008),
                }
            }
            "refresh_token" => {
                let token = form["refresh_token"].clone();
                state.grants.push(format!("refresh {token}"));
                if state.refresh.remove(&token) {
                    tokens(&mut state)
                } else {
                    refused("invalid_grant", 70000)
                }
            }
            _ => refused("unsupported_grant_type", 70003),
        }
    };
    let _ = reader.into_inner().write_all(response.as_bytes()).await;
}

/// The browser: opens the sign-in page and follows its redirect to the engine's loopback.
/// Returns the page the loopback showed.
pub async fn browse(url: String) -> String {
    let tls = Trust::Platform.client_config().unwrap();
    reqwest::Client::builder()
        .no_proxy()
        .tls_backend_preconfigured((*tls).clone())
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

/// Every XOAUTH2 login a server saw: the user and the token.
pub type Logins = Arc<Mutex<Vec<(String, String)>>>;

pub struct Server {
    pub port: u16,
    pub logins: Logins,
}

impl Server {
    pub fn endpoint(&self) -> Endpoint {
        Endpoint {
            host: "127.0.0.1".into(),
            port: self.port,
            security: Security::Tls,
            certificate: None,
        }
    }

    pub fn logins(&self) -> Vec<(String, String)> {
        self.logins.lock().unwrap().clone()
    }
}

/// The user and token in an XOAUTH2 response.
fn xoauth2(encoded: &str) -> (String, String) {
    let decoded = String::from_utf8(STANDARD.decode(encoded.trim()).unwrap()).unwrap();
    let mut user = String::new();
    let mut token = String::new();
    for part in decoded.split('\x01') {
        if let Some(value) = part.strip_prefix("user=") {
            user = value.to_owned();
        } else if let Some(value) = part.strip_prefix("auth=Bearer ") {
            token = value.to_owned();
        }
    }
    (user, token)
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn serve<F, S>(tls: &Tls, session: F) -> u16
where
    F: Fn(BufReader<Box<dyn Io>>) -> S + Send + Sync + 'static,
    S: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(tls.server.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let Ok(stream) = acceptor.accept(tcp).await else {
                continue;
            };
            let boxed: Box<dyn Io> = Box::new(stream);
            tokio::spawn(session(BufReader::new(boxed)));
        }
    });
    port
}

async fn line(io: &mut BufReader<Box<dyn Io>>) -> Option<String> {
    let mut line = String::new();
    match io.read_line(&mut line).await {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

async fn send(io: &mut BufReader<Box<dyn Io>>, text: &str) {
    let stream = io.get_mut();
    let _ = stream.write_all(text.as_bytes()).await;
    let _ = stream.flush().await;
}

/// IMAP with XOAUTH2 for any user holding a live access token, an INBOX and nothing in it.
pub async fn imap(tls: &Tls, microsoft: &Microsoft) -> Server {
    let logins = Logins::default();
    let seen = logins.clone();
    let state = microsoft.state.clone();
    let port = serve(tls, move |mut io| {
        let seen = seen.clone();
        let state = state.clone();
        async move {
            send(
                &mut io,
                "* OK [CAPABILITY IMAP4rev1 AUTH=XOAUTH2] ready\r\n",
            )
            .await;
            while let Some(command) = line(&mut io).await {
                let (tag, rest) = command.split_once(' ').unwrap_or((&command, ""));
                let verb = rest.split(' ').next().unwrap_or("").to_ascii_uppercase();
                let reply = match verb.as_str() {
                    "AUTHENTICATE" => {
                        send(&mut io, "+ \r\n").await;
                        let (user, token) = xoauth2(&line(&mut io).await.unwrap_or_default());
                        let live = {
                            let state = state.lock().unwrap();
                            !state.imap_refuses && state.access.contains(&token)
                        };
                        seen.lock().unwrap().push((user, token));
                        if live {
                            format!("{tag} OK authenticated\r\n")
                        } else {
                            send(&mut io, "+ eyJzdGF0dXMiOiI0MDEifQ==\r\n").await;
                            line(&mut io).await;
                            format!("{tag} NO [AUTHENTICATIONFAILED] AUTHENTICATE failed.\r\n")
                        }
                    }
                    "CAPABILITY" => {
                        format!("* CAPABILITY IMAP4rev1 AUTH=XOAUTH2\r\n{tag} OK done\r\n")
                    }
                    "LIST" => format!("* LIST (\\HasNoChildren) \"/\" INBOX\r\n{tag} OK done\r\n"),
                    "STATUS" => {
                        format!("* STATUS INBOX (MESSAGES 0 UNSEEN 0)\r\n{tag} OK done\r\n")
                    }
                    "EXAMINE" => format!(
                        "* 0 EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY 7] ok\r\n\
                         * OK [UIDNEXT 1] ok\r\n{tag} OK [READ-ONLY] done\r\n"
                    ),
                    "LOGOUT" => {
                        send(&mut io, &format!("* BYE bye\r\n{tag} OK done\r\n")).await;
                        return;
                    }
                    _ => format!("{tag} BAD unknown\r\n"),
                };
                send(&mut io, &reply).await;
            }
        }
    })
    .await;
    Server { port, logins }
}

/// SMTP submission over implicit TLS with XOAUTH2, which, like Outlook's (S5), takes a live
/// access token only as the identity it was issued to: the fake Microsoft's current user.
pub async fn smtp(tls: &Tls, microsoft: &Microsoft) -> Server {
    let logins = Logins::default();
    let seen = logins.clone();
    let state = microsoft.state.clone();
    let port = serve(tls, move |mut io| {
        let seen = seen.clone();
        let state = state.clone();
        async move {
            send(&mut io, "220 mock ESMTP\r\n").await;
            while let Some(command) = line(&mut io).await {
                let upper = command.to_ascii_uppercase();
                let reply = if upper.starts_with("EHLO") {
                    "250-mock\r\n250 AUTH XOAUTH2\r\n".to_owned()
                } else if let Some(encoded) = command.strip_prefix("AUTH XOAUTH2 ") {
                    let (user, token) = xoauth2(encoded);
                    let accepted = {
                        let state = state.lock().unwrap();
                        state.access.contains(&token) && state.username == user
                    };
                    seen.lock().unwrap().push((user, token));
                    if accepted {
                        "235 2.7.0 Authentication successful\r\n".to_owned()
                    } else {
                        "535 5.7.3 Authentication unsuccessful\r\n".to_owned()
                    }
                } else if upper.starts_with("QUIT") {
                    send(&mut io, "221 bye\r\n").await;
                    return;
                } else {
                    "502 unknown\r\n".to_owned()
                };
                send(&mut io, &reply).await;
            }
        }
    })
    .await;
    Server { port, logins }
}
