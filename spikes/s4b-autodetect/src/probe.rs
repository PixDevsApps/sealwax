use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hickory_resolver::TokioResolver;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

use crate::dnsdata::public_address;
use crate::fetch::Ledger;
use crate::model::{Protocol, Security};

const MAX_LINE: usize = 8 * 1024;
const MAX_LINES: usize = 200;
pub const EHLO_NAME: &str = "[127.0.0.1]";

pub fn tls_config() -> Result<Arc<ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_platform_verifier()
        .map_err(|e| e.to_string())?
        .with_no_client_auth();
    Ok(Arc::new(config))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub security: Security,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} {}:{} {}",
            self.protocol, self.host, self.port, self.security
        )
    }
}

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

#[derive(Clone, Debug)]
pub struct Greeting {
    pub banner: String,
    pub capabilities: Vec<String>,
    pub tls_version: String,
}

#[derive(Clone, Debug)]
pub struct Probe {
    pub target: Target,
    pub elapsed_ms: f64,
    pub result: Result<Greeting, (Stage, String)>,
}

pub async fn probe(
    target: Target,
    tls: Arc<ClientConfig>,
    limit: Duration,
    resolver: &TokioResolver,
    ledger: Option<&Ledger>,
) -> Probe {
    let start = Instant::now();
    let result = match timeout(limit, run(&target, tls, limit, resolver, ledger)).await {
        Ok(result) => result,
        Err(_) => Err((
            Stage::Timeout,
            format!("no answer within {} ms", limit.as_millis()),
        )),
    };
    Probe {
        target,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        result,
    }
}

#[derive(Clone, Default)]
pub struct ProbeCache(Arc<Mutex<HashMap<Target, Arc<OnceCell<Probe>>>>>);

impl ProbeCache {
    pub async fn probe(
        &self,
        target: Target,
        tls: Arc<ClientConfig>,
        limit: Duration,
        resolver: &TokioResolver,
        ledger: Option<&Ledger>,
    ) -> Probe {
        let cell = match self.0.lock() {
            Ok(mut cells) => cells.entry(target.clone()).or_default().clone(),
            Err(_) => Arc::new(OnceCell::new()),
        };
        cell.get_or_init(|| probe(target, tls, limit, resolver, ledger))
            .await
            .clone()
    }

    pub fn done(&self) -> Vec<Probe> {
        let Ok(cells) = self.0.lock() else {
            return Vec::new();
        };
        cells
            .values()
            .filter_map(|cell| cell.get().cloned())
            .collect()
    }
}

async fn run(
    target: &Target,
    tls: Arc<ClientConfig>,
    limit: Duration,
    resolver: &TokioResolver,
    ledger: Option<&Ledger>,
) -> Result<Greeting, (Stage, String)> {
    if let Some(ledger) = ledger {
        ledger.record("dns", format!("{} AAAA/A", target.host), false);
    }
    let addrs: Vec<SocketAddr> = resolver
        .lookup_ip(format!("{}.", target.host))
        .await
        .map_err(|e| (Stage::Dns, e.to_string()))?
        .iter()
        .map(|ip| SocketAddr::new(ip, target.port))
        .collect();
    if addrs.is_empty() {
        return Err((Stage::Dns, "no addresses".into()));
    }
    if addrs.iter().any(|addr| !public_address(addr.ip())) {
        return Err((Stage::Dns, "non-public address refused".into()));
    }
    if let Some(ledger) = ledger {
        ledger.record("tcp", format!("{}:{}", target.host, target.port), false);
    }
    let tcp = connect(&addrs, limit).await?;
    let name =
        ServerName::try_from(target.host.clone()).map_err(|e| (Stage::Tls, e.to_string()))?;
    let connector = TlsConnector::from(tls);
    match (target.protocol, target.security) {
        (_, Security::Plain) => Err((Stage::StartTls, "plaintext is never probed".into())),
        (Protocol::Imap, Security::Tls) => {
            let stream = connector
                .connect(name, tcp)
                .await
                .map_err(|e| (Stage::Tls, e.to_string()))?;
            let version = tls_version(stream.get_ref().1);
            let mut io = BufReader::new(stream);
            let banner = imap_greeting(&mut io).await?;
            let capabilities = imap_capability(&mut io, "a1").await?;
            Ok(Greeting {
                banner,
                capabilities,
                tls_version: version,
            })
        }
        (Protocol::Imap, Security::StartTls) => {
            let mut io = BufReader::new(tcp);
            let banner = imap_greeting(&mut io).await?;
            let before = imap_capability(&mut io, "a1").await?;
            if !before.iter().any(|c| c.eq_ignore_ascii_case("STARTTLS")) {
                return Err((Stage::StartTls, "STARTTLS not offered".into()));
            }
            send(&mut io, "a2 STARTTLS\r\n").await?;
            let reply = tagged_reply(&mut io, "a2").await?;
            if !reply.to_ascii_uppercase().starts_with("A2 OK") {
                return Err((Stage::StartTls, sanitize(&reply)));
            }
            let tcp = unwrap_plain(io)?;
            let stream = connector
                .connect(name, tcp)
                .await
                .map_err(|e| (Stage::Tls, e.to_string()))?;
            let version = tls_version(stream.get_ref().1);
            let mut io = BufReader::new(stream);
            let capabilities = imap_capability(&mut io, "a3").await?;
            Ok(Greeting {
                banner,
                capabilities,
                tls_version: version,
            })
        }
        (Protocol::Smtp, Security::Tls) => {
            let stream = connector
                .connect(name, tcp)
                .await
                .map_err(|e| (Stage::Tls, e.to_string()))?;
            let version = tls_version(stream.get_ref().1);
            let mut io = BufReader::new(stream);
            let banner = smtp_greeting(&mut io).await?;
            let capabilities = smtp_ehlo(&mut io).await?;
            Ok(Greeting {
                banner,
                capabilities,
                tls_version: version,
            })
        }
        (Protocol::Smtp, Security::StartTls) => {
            let mut io = BufReader::new(tcp);
            let banner = smtp_greeting(&mut io).await?;
            let before = smtp_ehlo(&mut io).await?;
            if !before.iter().any(|c| c.eq_ignore_ascii_case("STARTTLS")) {
                return Err((Stage::StartTls, "STARTTLS not offered".into()));
            }
            send(&mut io, "STARTTLS\r\n").await?;
            let (code, lines) = smtp_reply(&mut io).await?;
            if code != 220 {
                return Err((
                    Stage::StartTls,
                    sanitize(lines.first().map_or("", String::as_str)),
                ));
            }
            let tcp = unwrap_plain(io)?;
            let stream = connector
                .connect(name, tcp)
                .await
                .map_err(|e| (Stage::Tls, e.to_string()))?;
            let version = tls_version(stream.get_ref().1);
            let mut io = BufReader::new(stream);
            let capabilities = smtp_ehlo(&mut io).await?;
            Ok(Greeting {
                banner,
                capabilities,
                tls_version: version,
            })
        }
        (Protocol::Pop3, _) => Err((Stage::Greeting, "POP3 is not probed".into())),
    }
}

async fn connect(addrs: &[SocketAddr], limit: Duration) -> Result<TcpStream, (Stage, String)> {
    let per_address = (limit / 2).max(Duration::from_millis(500));
    let mut last = String::from("no address tried");
    for addr in addrs {
        match timeout(per_address, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last = format!("{addr}: {e}"),
            Err(_) => last = format!("{addr}: connect timed out"),
        }
    }
    Err((Stage::Connect, last))
}

fn unwrap_plain(io: BufReader<TcpStream>) -> Result<TcpStream, (Stage, String)> {
    if !io.buffer().is_empty() {
        return Err((
            Stage::StartTls,
            "data sent before the TLS handshake (injection)".into(),
        ));
    }
    Ok(io.into_inner())
}

fn tls_version(connection: &rustls::ClientConnection) -> String {
    connection
        .protocol_version()
        .map_or_else(|| "?".into(), |v| format!("{v:?}"))
}

async fn send<S: AsyncWrite + Unpin>(io: &mut S, line: &str) -> Result<(), (Stage, String)> {
    io.write_all(line.as_bytes())
        .await
        .map_err(|e| (Stage::Capability, e.to_string()))?;
    io.flush()
        .await
        .map_err(|e| (Stage::Capability, e.to_string()))
}

pub async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<String> {
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

pub async fn imap_greeting<R: AsyncBufRead + Unpin>(io: &mut R) -> Result<String, (Stage, String)> {
    let line = read_line(io)
        .await
        .map_err(|e| (Stage::Greeting, e.to_string()))?;
    if line.to_ascii_uppercase().starts_with("* OK") {
        Ok(sanitize(&line))
    } else {
        Err((Stage::Greeting, sanitize(&line)))
    }
}

pub async fn imap_capability<S: AsyncBufRead + AsyncWrite + Unpin>(
    io: &mut S,
    tag: &str,
) -> Result<Vec<String>, (Stage, String)> {
    send(io, &format!("{tag} CAPABILITY\r\n")).await?;
    let mut capabilities = Vec::new();
    for _ in 0..MAX_LINES {
        let line = read_line(io)
            .await
            .map_err(|e| (Stage::Capability, e.to_string()))?;
        if let Some(rest) = strip_prefix_ci(&line, "* CAPABILITY ") {
            capabilities = rest.split_ascii_whitespace().map(str::to_string).collect();
        } else if strip_prefix_ci(&line, &format!("{tag} ")).is_some() {
            if strip_prefix_ci(&line, &format!("{tag} OK")).is_some() {
                return Ok(capabilities);
            }
            return Err((Stage::Capability, sanitize(&line)));
        }
    }
    Err((Stage::Capability, "too many lines".into()))
}

async fn tagged_reply<R: AsyncBufRead + Unpin>(
    io: &mut R,
    tag: &str,
) -> Result<String, (Stage, String)> {
    for _ in 0..MAX_LINES {
        let line = read_line(io)
            .await
            .map_err(|e| (Stage::StartTls, e.to_string()))?;
        if strip_prefix_ci(&line, &format!("{tag} ")).is_some() {
            return Ok(line);
        }
    }
    Err((Stage::StartTls, "too many lines".into()))
}

pub async fn smtp_reply<R: AsyncBufRead + Unpin>(
    io: &mut R,
) -> Result<(u16, Vec<String>), (Stage, String)> {
    let mut lines = Vec::new();
    for _ in 0..MAX_LINES {
        let line = read_line(io)
            .await
            .map_err(|e| (Stage::Greeting, e.to_string()))?;
        let code = line
            .get(..3)
            .and_then(|c| c.parse::<u16>().ok())
            .ok_or_else(|| (Stage::Greeting, sanitize(&line)))?;
        let separator = line.as_bytes().get(3).copied();
        lines.push(line.get(4..).unwrap_or("").to_string());
        if separator != Some(b'-') {
            return Ok((code, lines));
        }
    }
    Err((Stage::Greeting, "too many lines".into()))
}

pub async fn smtp_greeting<R: AsyncBufRead + Unpin>(io: &mut R) -> Result<String, (Stage, String)> {
    let (code, lines) = smtp_reply(io).await?;
    let first = sanitize(lines.first().map_or("", String::as_str));
    if code == 220 {
        Ok(format!("220 {first}"))
    } else {
        Err((Stage::Greeting, format!("{code} {first}")))
    }
}

pub async fn smtp_ehlo<S: AsyncBufRead + AsyncWrite + Unpin>(
    io: &mut S,
) -> Result<Vec<String>, (Stage, String)> {
    send(io, &format!("EHLO {EHLO_NAME}\r\n")).await?;
    let (code, lines) = smtp_reply(io)
        .await
        .map_err(|(_, e)| (Stage::Capability, e))?;
    if code != 250 {
        return Err((Stage::Capability, format!("EHLO answered {code}")));
    }
    Ok(lines.into_iter().skip(1).collect())
}

fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &line[prefix.len()..])
}

fn sanitize(line: &str) -> String {
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

pub fn auth_summary(protocol: Protocol, capabilities: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    match protocol {
        Protocol::Imap => {
            for c in capabilities {
                if let Some(mech) = strip_prefix_ci(c, "AUTH=") {
                    out.push(mech.to_ascii_uppercase());
                }
                if c.eq_ignore_ascii_case("LOGINDISABLED") {
                    out.push("LOGINDISABLED".into());
                }
            }
        }
        Protocol::Smtp => {
            for c in capabilities {
                if let Some(rest) =
                    strip_prefix_ci(c, "AUTH ").or_else(|| strip_prefix_ci(c, "AUTH="))
                {
                    out.extend(rest.split_ascii_whitespace().map(str::to_ascii_uppercase));
                }
            }
        }
        Protocol::Pop3 => {}
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        "-".into()
    } else {
        out.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncRead, AsyncReadExt, duplex};

    async fn script<S: AsyncRead + AsyncWrite + Unpin>(
        mut server: S,
        steps: Vec<(Option<&'static str>, &'static str)>,
    ) -> String {
        let mut seen = String::new();
        for (expect, reply) in steps {
            if let Some(expect) = expect {
                let mut buf = vec![0; expect.len()];
                server.read_exact(&mut buf).await.unwrap();
                let got = String::from_utf8(buf).unwrap();
                assert_eq!(got, expect);
                seen.push_str(&got);
            }
            server.write_all(reply.as_bytes()).await.unwrap();
        }
        seen
    }

    #[tokio::test]
    async fn imap_greeting_and_capability() {
        let (client, server) = duplex(4096);
        let fake = tokio::spawn(script(
            server,
            vec![
                (None, "* OK [CAPABILITY IMAP4rev1] ready\r\n"),
                (
                    Some("a1 CAPABILITY\r\n"),
                    "* CAPABILITY IMAP4rev1 AUTH=PLAIN AUTH=XOAUTH2 SASL-IR\r\na1 OK done\r\n",
                ),
            ],
        ));
        let mut io = BufReader::new(client);
        assert!(imap_greeting(&mut io).await.unwrap().starts_with("* OK"));
        let caps = imap_capability(&mut io, "a1").await.unwrap();
        assert_eq!(auth_summary(Protocol::Imap, &caps), "PLAIN,XOAUTH2");
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn imap_bye_greeting_is_an_error() {
        let (client, server) = duplex(1024);
        tokio::spawn(script(server, vec![(None, "* BYE go away\r\n")]));
        let mut io = BufReader::new(client);
        assert_eq!(imap_greeting(&mut io).await.unwrap_err().0, Stage::Greeting);
    }

    #[tokio::test]
    async fn smtp_multiline_greeting_and_ehlo() {
        let (client, server) = duplex(4096);
        let fake = tokio::spawn(script(
            server,
            vec![
                (None, "220-mx.example ESMTP\r\n220 second line\r\n"),
                (
                    Some("EHLO [127.0.0.1]\r\n"),
                    "250-mx.example\r\n250-PIPELINING\r\n250-AUTH LOGIN PLAIN XOAUTH2\r\n250 8BITMIME\r\n",
                ),
            ],
        ));
        let mut io = BufReader::new(client);
        assert_eq!(
            smtp_greeting(&mut io).await.unwrap(),
            "220 mx.example ESMTP"
        );
        let caps = smtp_ehlo(&mut io).await.unwrap();
        assert_eq!(caps, ["PIPELINING", "AUTH LOGIN PLAIN XOAUTH2", "8BITMIME"]);
        assert_eq!(auth_summary(Protocol::Smtp, &caps), "LOGIN,PLAIN,XOAUTH2");
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn smtp_rejection_is_an_error() {
        let (client, server) = duplex(1024);
        tokio::spawn(script(server, vec![(None, "554 no service\r\n")]));
        let mut io = BufReader::new(client);
        assert!(
            smtp_greeting(&mut io)
                .await
                .unwrap_err()
                .1
                .starts_with("554")
        );
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
            sanitize("ready to talk with ::ffff:192.0.2.1"),
            "ready to talk with [ip]"
        );
        assert_eq!(
            sanitize("220 smtp.example.com ESMTP"),
            "220 smtp.example.com ESMTP"
        );
    }

    #[tokio::test]
    async fn overlong_lines_are_refused() {
        let (client, mut server) = duplex(64 * 1024);
        tokio::spawn(async move {
            server.write_all(&vec![b'a'; MAX_LINE * 2]).await.unwrap();
        });
        let mut io = BufReader::new(client);
        assert!(read_line(&mut io).await.is_err());
    }
}
