//! Connecting: implicit TLS (993) or STARTTLS (143, never downgraded), AUTHENTICATE PLAIN with a
//! LOGIN fallback, ID, capabilities and ENABLE QRESYNC.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_imap::imap_proto::{Capability, Response, ResponseCode, Status};
use async_imap::types::UnsolicitedResponse;
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::raw::{self, ImapError};

pub type Stream = tokio_rustls::client::TlsStream<TcpStream>;
pub type Session = async_imap::Session<Stream>;
type Client = async_imap::Client<Stream>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Tls,
    StartTls,
}

#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Address to connect to (`127.0.0.1` for the testbed).
    pub host: String,
    pub port: u16,
    /// Name the certificate must match.
    pub server_name: String,
    pub security: Security,
}

#[derive(Clone)]
pub struct Login {
    pub user: String,
    pub password: String,
}

#[derive(Clone, Debug, Default)]
pub struct Caps(HashSet<String>);

impl Caps {
    fn from_iter<'a>(caps: impl IntoIterator<Item = &'a Capability<'a>>) -> Self {
        Caps(
            caps.into_iter()
                .map(|c| match c {
                    Capability::Imap4rev1 => "IMAP4REV1".to_owned(),
                    Capability::Auth(m) => format!("AUTH={}", m.to_ascii_uppercase()),
                    Capability::Atom(a) => a.to_ascii_uppercase(),
                })
                .collect(),
        )
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.contains(&name.to_ascii_uppercase())
    }

    pub fn sorted(&self) -> Vec<String> {
        let mut all: Vec<String> = self.0.iter().cloned().collect();
        all.sort();
        all
    }
}

/// How the session authenticated, for the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    Plain,
    Login,
}

pub struct Connection {
    pub session: Session,
    pub caps: Caps,
    pub qresync: bool,
    pub auth: AuthMethod,
    /// Wire name of the mailbox selected or examined, if any.
    pub selected: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct ConnectOptions {
    pub enable_qresync: bool,
    pub send_id: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions {
            enable_qresync: true,
            send_id: true,
        }
    }
}

struct Plain<'a>(&'a Login);

impl async_imap::Authenticator for Plain<'_> {
    type Response = Vec<u8>;

    fn process(&mut self, _challenge: &[u8]) -> Vec<u8> {
        format!("\0{}\0{}", self.0.user, self.0.password).into_bytes()
    }
}

async fn tls_handshake(
    tcp: TcpStream,
    config: Arc<ClientConfig>,
    server_name: &str,
) -> Result<Stream, ImapError> {
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| ImapError::Protocol(e.to_string()))?;
    Ok(TlsConnector::from(config).connect(name, tcp).await?)
}

/// Reads the greeting, returning capabilities if it carried a CAPABILITY code.
async fn greeting<T>(client: &mut async_imap::Client<T>) -> Result<Option<Caps>, ImapError>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    let response = client
        .read_response()
        .await?
        .ok_or(ImapError::ConnectionLost)?;
    match response.parsed() {
        Response::Data {
            status: Status::Ok,
            outcome,
        } => Ok(match &outcome.code {
            Some(ResponseCode::Capabilities(caps)) => Some(Caps::from_iter(caps)),
            _ => None,
        }),
        Response::Data {
            status: Status::Bye,
            ..
        } => Err(ImapError::Bye("greeting".into())),
        _ => Err(ImapError::Protocol("unexpected greeting".into())),
    }
}

/// CAPABILITY before authentication. `Client` exposes only `run_command_and_check_ok`, which
/// forwards untagged responses to the channel given to it.
async fn client_capabilities(client: &mut Client) -> Result<Caps, ImapError> {
    let (tx, rx) = async_channel::bounded(16);
    client
        .run_command_and_check_ok("CAPABILITY", Some(tx))
        .await?;
    while let Ok(response) = rx.try_recv() {
        if let UnsolicitedResponse::Other(data) = response
            && let Response::Capabilities(caps) = data.parsed()
        {
            return Ok(Caps::from_iter(caps));
        }
    }
    Err(ImapError::Protocol("no CAPABILITY response".into()))
}

pub async fn capabilities(session: &mut Session) -> Result<Caps, ImapError> {
    let mut caps = None;
    raw::run(session, "CAPABILITY", |r| {
        if let Response::Capabilities(c) = r {
            caps = Some(Caps::from_iter(c));
        }
    })
    .await?;
    caps.ok_or_else(|| ImapError::Protocol("no CAPABILITY response".into()))
}

pub async fn connect(
    endpoint: &Endpoint,
    tls: Arc<ClientConfig>,
    login: &Login,
    options: ConnectOptions,
) -> Result<Connection, ImapError> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_inner(endpoint, tls, login, options),
    )
    .await
    .map_err(|_| ImapError::Timeout)?
}

async fn connect_inner(
    endpoint: &Endpoint,
    tls: Arc<ClientConfig>,
    login: &Login,
    options: ConnectOptions,
) -> Result<Connection, ImapError> {
    let tcp = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).await?;
    tcp.set_nodelay(true)?;
    let (mut client, greeting_caps) = match endpoint.security {
        Security::Tls => {
            let stream = tls_handshake(tcp, tls, &endpoint.server_name).await?;
            let mut client = Client::new(stream);
            let caps = greeting(&mut client).await?;
            (client, caps)
        }
        Security::StartTls => {
            let mut plain = async_imap::Client::new(tcp);
            greeting(&mut plain).await?;
            // STARTTLS is sent whatever the plaintext capabilities say (an attacker can strip
            // them); anything but OK ends the attempt, so there is no plaintext fallback.
            plain.run_command_and_check_ok("STARTTLS", None).await?;
            // into_inner drops anything buffered after the OK, so injected plaintext can't be
            // read as if it came over TLS.
            let tcp = plain.into_inner();
            let stream = tls_handshake(tcp, tls, &endpoint.server_name).await?;
            (Client::new(stream), None)
        }
    };
    let caps = match greeting_caps {
        Some(caps) => caps,
        None => client_capabilities(&mut client).await?,
    };
    let (mut session, auth) = if caps.has("AUTH=PLAIN") {
        let session = client
            .authenticate("PLAIN", Plain(login))
            .await
            .map_err(|(e, _)| e)?;
        (session, AuthMethod::Plain)
    } else if !caps.has("LOGINDISABLED") {
        let session = client
            .login(&login.user, &login.password)
            .await
            .map_err(|(e, _)| e)?;
        (session, AuthMethod::Login)
    } else {
        return Err(ImapError::Protocol(
            "no usable authentication mechanism".into(),
        ));
    };
    let caps = capabilities(&mut session).await?;
    if options.send_id && caps.has("ID") {
        raw::run(
            &mut session,
            "ID (\"name\" \"mail-dev-spike\" \"version\" \"0.0.1\")",
            |_| {},
        )
        .await?;
    }
    let mut qresync = false;
    if options.enable_qresync && caps.has("QRESYNC") && caps.has("ENABLE") {
        raw::run(&mut session, "ENABLE QRESYNC", |r| {
            if let Response::Capabilities(enabled) = r {
                qresync = enabled
                    .iter()
                    .any(|c| matches!(c, Capability::Atom(a) if a.eq_ignore_ascii_case("QRESYNC")));
            }
        })
        .await?;
    }
    Ok(Connection {
        session,
        caps,
        qresync,
        auth,
        selected: None,
    })
}

impl Connection {
    pub async fn logout(mut self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            raw::run(&mut self.session, "LOGOUT", |_| {}),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A plaintext server that offers AUTH=PLAIN but refuses STARTTLS: the client must give up
    /// without ever sending credentials.
    #[tokio::test]
    async fn refused_starttls_never_falls_back_to_plaintext() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket
                .write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] plaintext only\r\n")
                .await
                .unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return received;
                }
                received.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&received).into_owned();
                if let Some(line) = text.lines().find(|l| l.contains("STARTTLS")) {
                    let tag = line.split_whitespace().next().unwrap_or("*").to_owned();
                    socket
                        .write_all(format!("{tag} NO not here\r\n").as_bytes())
                        .await
                        .unwrap();
                    received.clear();
                    received.extend_from_slice(b"STARTTLS-refused ");
                }
            }
        });
        let endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port,
            server_name: "localhost".into(),
            security: Security::StartTls,
        };
        let login = Login {
            user: "nobody".into(),
            password: "secret".into(),
        };
        let result = connect(
            &endpoint,
            crate::tls::platform().unwrap(),
            &login,
            ConnectOptions::default(),
        )
        .await;
        assert!(result.is_err());
        drop(result);
        let received = String::from_utf8_lossy(&server.await.unwrap()).into_owned();
        assert!(received.starts_with("STARTTLS-refused"), "{received}");
        assert!(
            !received.contains("AUTHENTICATE")
                && !received.contains("LOGIN")
                && !received.contains("secret")
        );
    }
}
