//! The guarded TLS check (S4): TCP, then TLS or STARTTLS with a verified certificate, the
//! greeting, and CAPABILITY or `EHLO [127.0.0.1]`. Nothing else is ever written to the server:
//! no AUTH, LOGIN, USER or PASS. Data sent before a STARTTLS handshake aborts the check.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use sealwax_domain::account::{Protocol, Security};
use sealwax_domain::autoconfig::probing::Target;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

const MAX_LINE: usize = 8 * 1024;
const MAX_LINES: usize = 200;
/// SMTP clients name themselves in EHLO; an address literal reveals nothing (ADR-007).
pub const EHLO_NAME: &str = "[127.0.0.1]";

/// How far a check got before it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Dns,
    Connect,
    Greeting,
    StartTls,
    Tls,
    Capability,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{stage:?}: {detail}")]
pub struct ProbeError {
    pub stage: Stage,
    pub detail: String,
}

impl ProbeError {
    pub fn new(stage: Stage, detail: impl Into<String>) -> Self {
        Self {
            stage,
            detail: detail.into(),
        }
    }
}

type Step<T> = Result<T, ProbeError>;

/// Checks `target` on already-resolved public `addrs`.
pub async fn check(
    target: &Target,
    addrs: &[SocketAddr],
    tls: Arc<ClientConfig>,
    limit: Duration,
) -> Step<()> {
    match timeout(limit, run(target, addrs, tls, limit)).await {
        Ok(result) => result,
        Err(_) => Err(ProbeError::new(
            Stage::Timeout,
            format!("no answer within {} ms", limit.as_millis()),
        )),
    }
}

async fn run(
    target: &Target,
    addrs: &[SocketAddr],
    tls: Arc<ClientConfig>,
    limit: Duration,
) -> Step<()> {
    let tcp = connect(addrs, limit).await?;
    let name = ServerName::try_from(target.host.clone())
        .map_err(|e| ProbeError::new(Stage::Tls, e.to_string()))?;
    let connector = TlsConnector::from(tls);
    let handshake = |e: io::Error| ProbeError::new(Stage::Tls, e.to_string());
    match (target.protocol, target.security) {
        (Protocol::Imap, Security::Tls) => {
            let stream = connector.connect(name, tcp).await.map_err(handshake)?;
            let mut io = BufReader::new(stream);
            imap_greeting(&mut io).await?;
            imap_capability(&mut io, "a1").await.map(drop)
        }
        (Protocol::Imap, Security::StartTls) => {
            let mut io = BufReader::new(tcp);
            imap_greeting(&mut io).await?;
            imap_starttls(&mut io).await?;
            let stream = connector
                .connect(name, unwrap_plain(io)?)
                .await
                .map_err(handshake)?;
            imap_capability(&mut BufReader::new(stream), "a3")
                .await
                .map(drop)
        }
        (Protocol::Smtp, Security::Tls) => {
            let stream = connector.connect(name, tcp).await.map_err(handshake)?;
            let mut io = BufReader::new(stream);
            smtp_greeting(&mut io).await?;
            smtp_ehlo(&mut io).await.map(drop)
        }
        (Protocol::Smtp, Security::StartTls) => {
            let mut io = BufReader::new(tcp);
            smtp_greeting(&mut io).await?;
            smtp_starttls(&mut io).await?;
            let stream = connector
                .connect(name, unwrap_plain(io)?)
                .await
                .map_err(handshake)?;
            smtp_ehlo(&mut BufReader::new(stream)).await.map(drop)
        }
        // Autodetect's chains name IMAP and SMTP only; POP3 accounts are set up by hand (M8).
        (Protocol::Pop3, _) => Err(ProbeError::new(
            Stage::Greeting,
            "autodetect doesn't probe POP3".to_owned(),
        )),
    }
}

async fn connect(addrs: &[SocketAddr], limit: Duration) -> Step<TcpStream> {
    let per_address = (limit / 2).max(Duration::from_millis(500));
    let mut last = String::from("no address");
    for addr in addrs {
        match timeout(per_address, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last = e.to_string(),
            Err(_) => last = "connect timed out".into(),
        }
    }
    Err(ProbeError::new(Stage::Connect, last))
}

fn unwrap_plain(io: BufReader<TcpStream>) -> Step<TcpStream> {
    if !io.buffer().is_empty() {
        return Err(ProbeError::new(
            Stage::StartTls,
            "data sent before the TLS handshake",
        ));
    }
    Ok(io.into_inner())
}

async fn send<S: AsyncWrite + Unpin>(io: &mut S, line: &str, stage: Stage) -> Step<()> {
    io.write_all(line.as_bytes())
        .await
        .map_err(|e| ProbeError::new(stage, e.to_string()))?;
    io.flush()
        .await
        .map_err(|e| ProbeError::new(stage, e.to_string()))
}

async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<String> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if let Some(end) = available.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&available[..=end]);
            reader.consume(end + 1);
            break;
        }
        let taken = available.len();
        line.extend_from_slice(available);
        reader.consume(taken);
        if line.len() > MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
    Ok(String::from_utf8_lossy(&line).trim_end().to_string())
}

async fn line<R: AsyncBufRead + Unpin>(io: &mut R, stage: Stage) -> Step<String> {
    read_line(io)
        .await
        .map_err(|e| ProbeError::new(stage, e.to_string()))
}

async fn imap_greeting<R: AsyncBufRead + Unpin>(io: &mut R) -> Step<()> {
    let greeting = line(io, Stage::Greeting).await?;
    if strip_prefix_ci(&greeting, "* OK").is_some() {
        Ok(())
    } else {
        Err(ProbeError::new(Stage::Greeting, sanitize(&greeting)))
    }
}

async fn imap_capability<S: AsyncBufRead + AsyncWrite + Unpin>(
    io: &mut S,
    tag: &str,
) -> Step<Vec<String>> {
    send(io, &format!("{tag} CAPABILITY\r\n"), Stage::Capability).await?;
    let mut capabilities = Vec::new();
    for _ in 0..MAX_LINES {
        let reply = line(io, Stage::Capability).await?;
        if let Some(rest) = strip_prefix_ci(&reply, "* CAPABILITY ") {
            capabilities = rest.split_ascii_whitespace().map(str::to_string).collect();
        } else if strip_prefix_ci(&reply, &format!("{tag} ")).is_some() {
            if strip_prefix_ci(&reply, &format!("{tag} OK")).is_some() {
                return Ok(capabilities);
            }
            return Err(ProbeError::new(Stage::Capability, sanitize(&reply)));
        }
    }
    Err(ProbeError::new(Stage::Capability, "too many lines"))
}

async fn imap_starttls<S: AsyncBufRead + AsyncWrite + Unpin>(io: &mut S) -> Step<()> {
    let capabilities = imap_capability(io, "a1").await?;
    if !capabilities
        .iter()
        .any(|c| c.eq_ignore_ascii_case("STARTTLS"))
    {
        return Err(ProbeError::new(Stage::StartTls, "STARTTLS not offered"));
    }
    send(io, "a2 STARTTLS\r\n", Stage::StartTls).await?;
    for _ in 0..MAX_LINES {
        let reply = line(io, Stage::StartTls).await?;
        if strip_prefix_ci(&reply, "a2 OK").is_some() {
            return Ok(());
        }
        if strip_prefix_ci(&reply, "a2 ").is_some() {
            return Err(ProbeError::new(Stage::StartTls, sanitize(&reply)));
        }
    }
    Err(ProbeError::new(Stage::StartTls, "too many lines"))
}

async fn smtp_reply<R: AsyncBufRead + Unpin>(io: &mut R, stage: Stage) -> Step<(u16, Vec<String>)> {
    let mut lines = Vec::new();
    for _ in 0..MAX_LINES {
        let reply = line(io, stage).await?;
        let code = reply
            .get(..3)
            .and_then(|c| c.parse::<u16>().ok())
            .ok_or_else(|| ProbeError::new(stage, sanitize(&reply)))?;
        let more = reply.as_bytes().get(3) == Some(&b'-');
        lines.push(reply.get(4..).unwrap_or("").to_string());
        if !more {
            return Ok((code, lines));
        }
    }
    Err(ProbeError::new(stage, "too many lines"))
}

async fn smtp_greeting<R: AsyncBufRead + Unpin>(io: &mut R) -> Step<()> {
    match smtp_reply(io, Stage::Greeting).await? {
        (220, _) => Ok(()),
        (code, lines) => Err(ProbeError::new(
            Stage::Greeting,
            format!(
                "{code} {}",
                sanitize(lines.first().map_or("", String::as_str))
            ),
        )),
    }
}

async fn smtp_ehlo<S: AsyncBufRead + AsyncWrite + Unpin>(io: &mut S) -> Step<Vec<String>> {
    send(io, &format!("EHLO {EHLO_NAME}\r\n"), Stage::Capability).await?;
    match smtp_reply(io, Stage::Capability).await? {
        (250, lines) => Ok(lines.into_iter().skip(1).collect()),
        (code, _) => Err(ProbeError::new(
            Stage::Capability,
            format!("EHLO answered {code}"),
        )),
    }
}

async fn smtp_starttls<S: AsyncBufRead + AsyncWrite + Unpin>(io: &mut S) -> Step<()> {
    let extensions = smtp_ehlo(io).await?;
    if !extensions
        .iter()
        .any(|c| c.eq_ignore_ascii_case("STARTTLS"))
    {
        return Err(ProbeError::new(Stage::StartTls, "STARTTLS not offered"));
    }
    send(io, "STARTTLS\r\n", Stage::StartTls).await?;
    match smtp_reply(io, Stage::StartTls).await? {
        (220, _) => Ok(()),
        (code, _) => Err(ProbeError::new(
            Stage::StartTls,
            format!("STARTTLS answered {code}"),
        )),
    }
}

fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &line[prefix.len()..])
}

/// A server line for diagnostics: IP literals (Gmail's greeting echoes the client's address)
/// become `[ip]`, control characters go, and at most 100 characters stay.
pub fn sanitize(line: &str) -> String {
    let redacted: Vec<&str> = line
        .split(' ')
        .map(|word| {
            let bare =
                word.trim_matches(|c: char| !(c.is_ascii_hexdigit() || c == ':' || c == '.'));
            if bare.parse::<std::net::IpAddr>().is_ok() {
                "[ip]"
            } else {
                word
            }
        })
        .collect();
    redacted
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncRead, AsyncReadExt, duplex};

    /// A scripted server: each step expects exactly these bytes from the client (if any), then
    /// answers. It fails on anything else the client writes, so a test proves the probe sends
    /// nothing but what is listed.
    async fn script<S: AsyncRead + AsyncWrite + Unpin>(
        mut server: S,
        steps: Vec<(Option<&'static str>, &'static str)>,
    ) {
        for (expect, reply) in steps {
            if let Some(expect) = expect {
                let mut buf = vec![0; expect.len()];
                server.read_exact(&mut buf).await.unwrap();
                assert_eq!(String::from_utf8(buf).unwrap(), expect);
            }
            server.write_all(reply.as_bytes()).await.unwrap();
        }
        let mut rest = Vec::new();
        server.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "client sent more: {rest:?}");
    }

    #[tokio::test]
    async fn imap_sends_only_capability() {
        let (client, server) = duplex(4096);
        let fake = tokio::spawn(script(
            server,
            vec![
                (None, "* OK [CAPABILITY IMAP4rev1] ready\r\n"),
                (
                    Some("a1 CAPABILITY\r\n"),
                    "* CAPABILITY IMAP4rev1 AUTH=PLAIN SASL-IR\r\na1 OK done\r\n",
                ),
            ],
        ));
        let mut io = BufReader::new(client);
        imap_greeting(&mut io).await.unwrap();
        let capabilities = imap_capability(&mut io, "a1").await.unwrap();
        assert_eq!(capabilities, ["IMAP4rev1", "AUTH=PLAIN", "SASL-IR"]);
        drop(io);
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn imap_starttls_asks_capability_then_starttls() {
        let (client, server) = duplex(4096);
        let fake = tokio::spawn(script(
            server,
            vec![
                (None, "* OK ready\r\n"),
                (
                    Some("a1 CAPABILITY\r\n"),
                    "* CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED\r\na1 OK\r\n",
                ),
                (Some("a2 STARTTLS\r\n"), "a2 OK begin TLS\r\n"),
            ],
        ));
        let mut io = BufReader::new(client);
        imap_greeting(&mut io).await.unwrap();
        imap_starttls(&mut io).await.unwrap();
        drop(io);
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn imap_without_starttls_is_refused() {
        let (client, server) = duplex(4096);
        let fake = tokio::spawn(script(
            server,
            vec![
                (None, "* OK ready\r\n"),
                (
                    Some("a1 CAPABILITY\r\n"),
                    "* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\na1 OK\r\n",
                ),
            ],
        ));
        let mut io = BufReader::new(client);
        imap_greeting(&mut io).await.unwrap();
        assert_eq!(
            imap_starttls(&mut io).await.unwrap_err().stage,
            Stage::StartTls
        );
        drop(io);
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn imap_bye_greeting_is_an_error() {
        let (client, server) = duplex(1024);
        tokio::spawn(script(server, vec![(None, "* BYE go away\r\n")]));
        let mut io = BufReader::new(client);
        assert_eq!(
            imap_greeting(&mut io).await.unwrap_err().stage,
            Stage::Greeting
        );
    }

    #[tokio::test]
    async fn smtp_sends_only_ehlo_and_starttls() {
        let (client, server) = duplex(4096);
        let fake = tokio::spawn(script(
            server,
            vec![
                (None, "220-mx.example ESMTP\r\n220 second line\r\n"),
                (
                    Some("EHLO [127.0.0.1]\r\n"),
                    "250-mx.example\r\n250-STARTTLS\r\n250 8BITMIME\r\n",
                ),
                (Some("STARTTLS\r\n"), "220 go ahead\r\n"),
            ],
        ));
        let mut io = BufReader::new(client);
        smtp_greeting(&mut io).await.unwrap();
        smtp_starttls(&mut io).await.unwrap();
        drop(io);
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn smtp_rejection_is_an_error() {
        let (client, server) = duplex(1024);
        tokio::spawn(script(server, vec![(None, "554 no service\r\n")]));
        let mut io = BufReader::new(client);
        let error = smtp_greeting(&mut io).await.unwrap_err();
        assert!(error.detail.starts_with("554"), "{error}");
    }

    #[tokio::test]
    async fn overlong_lines_are_refused() {
        let (client, mut server) = duplex(64 * 1024);
        tokio::spawn(async move {
            let _ = server.write_all(&vec![b'a'; MAX_LINE * 2]).await;
        });
        let mut io = BufReader::new(client);
        assert!(read_line(&mut io).await.is_err());
    }

    #[test]
    fn banners_never_keep_the_client_address() {
        assert_eq!(
            sanitize("* OK Gimap ready for requests from 2001:db8::1 abc"),
            "* OK Gimap ready for requests from [ip] abc"
        );
        assert_eq!(
            sanitize("220 hello [192.0.2.7], pleased"),
            "220 hello [ip] pleased"
        );
        assert_eq!(
            sanitize("220 smtp.example.com ESMTP"),
            "220 smtp.example.com ESMTP"
        );
    }
}
