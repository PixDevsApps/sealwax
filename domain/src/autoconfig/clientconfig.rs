//! Thunderbird's clientConfig XML (autoconfig v1.1), as the domain's own autoconfig and the ISPDB
//! serve it: incoming IMAP and outgoing SMTP servers, `%EMAILADDRESS%`-style usernames, the
//! provider's name and an "enable IMAP" link. POP3, plaintext servers and IP-literal or
//! `localhost` hosts are skipped.

use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};

use super::dns::acceptable_host;
use super::{Finding, Hints, Server};
use crate::account::{HelpLink, Protocol, Security, UsernameForm};

const MAX_NAME_CHARS: usize = 60;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClientConfig {
    pub provider_id: Option<String>,
    pub display_name: Option<String>,
    pub display_short_name: Option<String>,
    pub servers: Vec<Server>,
    /// Why entries were left out, for diagnostics.
    pub skipped: Vec<String>,
    /// `<enable visiturl>`: the provider's page about turning IMAP on.
    pub enable_url: Option<String>,
}

impl ClientConfig {
    pub fn finding(self) -> Finding {
        if self.servers.is_empty() {
            return Finding::Nothing;
        }
        let provider = self
            .display_short_name
            .as_deref()
            .or(self.display_name.as_deref())
            .and_then(clean_name);
        let enable_imap = self
            .enable_url
            .filter(|url| url.starts_with("https://") && !url.chars().any(char::is_whitespace))
            .map(|url| HelpLink {
                label: "Turn on IMAP".into(),
                url,
            });
        Finding::Servers {
            servers: self.servers,
            hints: Hints {
                provider,
                enable_imap,
                ..Hints::default()
            },
        }
    }
}

fn clean_name(name: &str) -> Option<String> {
    let name: String = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    (!name.is_empty()).then_some(name)
}

#[derive(Default)]
struct PendingServer {
    kind: String,
    hostname: String,
    port: String,
    socket_type: String,
    username: Option<String>,
}

/// Parses a clientConfig document. `address` fills the username templates; it is never sent
/// anywhere from here.
pub fn parse(xml: &str, address: &str) -> Result<ClientConfig, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut config = ClientConfig::default();
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut pending: Option<PendingServer> = None;
    let mut seen_root = false;

    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("xml error at {}: {e}", reader.buffer_position()))?;
        match event {
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_string();
                if !seen_root {
                    if name != "clientConfig" {
                        return Err(format!("root element is <{name}>, not <clientConfig>"));
                    }
                    seen_root = true;
                }
                open(&start, &name, &mut config, &mut pending);
                path.push(name);
                text.clear();
            }
            Event::Empty(start) => {
                let name = start.local_name().as_ref().to_string();
                if !seen_root {
                    return Err(format!("root element is <{name}/>, not <clientConfig>"));
                }
                open(&start, &name, &mut config, &mut pending);
                if name.ends_with("Server")
                    && let Some(server) = pending.take()
                {
                    finish(server, address, &mut config);
                }
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(t) => text.push_str(&t.into_inner()),
            Event::GeneralRef(r) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    text.push(c);
                } else if let Some(s) = resolve_predefined_entity(&r) {
                    text.push_str(s);
                }
            }
            Event::End(_) => {
                let Some(name) = path.pop() else {
                    return Err("unbalanced end tag".into());
                };
                close(
                    &name,
                    path.last().map(String::as_str),
                    &text,
                    &mut config,
                    &mut pending,
                    address,
                );
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_root {
        return Err("empty document".into());
    }
    Ok(config)
}

fn open(
    start: &BytesStart<'_>,
    name: &str,
    config: &mut ClientConfig,
    pending: &mut Option<PendingServer>,
) {
    match name {
        "emailProvider" => config.provider_id = attribute(start, "id"),
        "incomingServer" | "outgoingServer" => {
            *pending = Some(PendingServer {
                kind: attribute(start, "type")
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                ..PendingServer::default()
            });
        }
        "enable" if config.enable_url.is_none() => {
            config.enable_url = attribute(start, "visiturl");
        }
        _ => {}
    }
}

fn close(
    name: &str,
    parent: Option<&str>,
    text: &str,
    config: &mut ClientConfig,
    pending: &mut Option<PendingServer>,
    address: &str,
) {
    let value = text.trim();
    match (name, parent) {
        ("incomingServer" | "outgoingServer", _) => {
            if let Some(server) = pending.take() {
                finish(server, address, config);
            }
        }
        ("displayName", Some("emailProvider")) => config.display_name = Some(value.to_string()),
        ("displayShortName", Some("emailProvider")) => {
            config.display_short_name = Some(value.to_string());
        }
        (field, Some("incomingServer" | "outgoingServer")) => {
            if let Some(server) = pending.as_mut() {
                match field {
                    "hostname" => server.hostname = value.to_string(),
                    "port" => server.port = value.to_string(),
                    "socketType" => server.socket_type = value.to_string(),
                    "username" => server.username = Some(value.to_string()),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn finish(server: PendingServer, address: &str, config: &mut ClientConfig) {
    let domain = address
        .rsplit_once('@')
        .map_or(address, |(_, domain)| domain);
    let protocol = match server.kind.as_str() {
        "imap" => Protocol::Imap,
        "smtp" => Protocol::Smtp,
        other => {
            config.skipped.push(format!("server type {other:?}"));
            return;
        }
    };
    let security = match server.socket_type.to_ascii_lowercase().as_str() {
        "ssl" | "tls" => Security::Tls,
        "starttls" => Security::StartTls,
        other => {
            config
                .skipped
                .push(format!("{protocol:?} socketType {other:?}"));
            return;
        }
    };
    let host = server
        .hostname
        .replace("%EMAILDOMAIN%", domain)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !acceptable_host(&host) {
        config
            .skipped
            .push(format!("{protocol:?} hostname refused"));
        return;
    }
    let port = match server.port.parse::<u16>() {
        Ok(port) if port > 0 => port,
        _ if server.port.is_empty() => default_port(protocol, security),
        _ => {
            config
                .skipped
                .push(format!("{protocol:?} port {:?}", server.port));
            return;
        }
    };
    config.servers.push(
        Server::new(protocol, &host, port, security)
            .with_username(username_form(server.username.as_deref(), address)),
    );
}

/// The username form a `<username>` template or literal stands for.
pub fn username_form(template: Option<&str>, address: &str) -> UsernameForm {
    let local = address.rsplit_once('@').map_or(address, |(local, _)| local);
    match template.map(str::trim) {
        None | Some("" | "%EMAILADDRESS%" | "%EMAILLOCALPART%@%EMAILDOMAIN%") => {
            UsernameForm::Address
        }
        Some("%EMAILLOCALPART%") => UsernameForm::LocalPart,
        Some(literal) if literal.eq_ignore_ascii_case(address) => UsernameForm::Address,
        Some(literal) if literal.eq_ignore_ascii_case(local) => UsernameForm::LocalPart,
        Some(_) => UsernameForm::Other,
    }
}

pub fn default_port(protocol: Protocol, security: Security) -> u16 {
    match (protocol, security) {
        (Protocol::Imap, Security::Tls) => 993,
        (Protocol::Imap, Security::StartTls) => 143,
        (Protocol::Smtp, Security::Tls) => 465,
        (Protocol::Smtp, Security::StartTls) => 587,
        (Protocol::Pop3, Security::Tls) => 995,
        (Protocol::Pop3, Security::StartTls) => 110,
    }
}

fn attribute(start: &BytesStart<'_>, key: &str) -> Option<String> {
    let attribute = start.try_get_attribute(key).ok().flatten()?;
    attribute
        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
        .ok()
        .map(|v| v.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Example Mail Services Ltd</displayName>
    <displayShortName>Example</displayShortName>
    <incomingServer type="imap">
      <hostname>imap.example.com</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>OAuth2</authentication>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="pop3">
      <hostname>pop.%EMAILDOMAIN%</hostname>
      <port>995</port>
      <socketType>SSL</socketType>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>imap.%EMAILDOMAIN%</hostname>
      <port>143</port>
      <socketType>plain</socketType>
    </incomingServer>
    <incomingServer type="exchange"><hostname>x</hostname></incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.com</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILLOCALPART%@%EMAILDOMAIN%</username>
    </outgoingServer>
    <enable visiturl="https://example.com/settings?a=1&amp;b=2"><instruction>Enable IMAP</instruction></enable>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn parses_servers_in_document_order() {
        let config = parse(EXAMPLE, "test@example.com").unwrap();
        assert_eq!(config.provider_id.as_deref(), Some("example.com"));
        assert_eq!(config.servers.len(), 2);
        let imap = &config.servers[0];
        assert_eq!(
            (imap.protocol, imap.host.as_str(), imap.port, imap.security),
            (Protocol::Imap, "imap.example.com", 993, Security::Tls)
        );
        assert_eq!(imap.username, UsernameForm::Address);
        let smtp = &config.servers[1];
        assert_eq!((smtp.port, smtp.security), (587, Security::StartTls));
        assert_eq!(smtp.username, UsernameForm::Address);
        assert_eq!(
            config.skipped,
            [
                "server type \"pop3\"",
                "Imap socketType \"plain\"",
                "server type \"exchange\""
            ]
        );
        assert_eq!(
            config.enable_url.as_deref(),
            Some("https://example.com/settings?a=1&b=2")
        );
    }

    #[test]
    fn the_finding_carries_the_short_name_and_the_enable_link() {
        let Finding::Servers { servers, hints } =
            parse(EXAMPLE, "test@example.com").unwrap().finding()
        else {
            panic!("expected servers");
        };
        assert_eq!(servers.len(), 2);
        assert_eq!(hints.provider.as_deref(), Some("Example"));
        assert_eq!(
            hints.enable_imap.map(|link| link.url).as_deref(),
            Some("https://example.com/settings?a=1&b=2")
        );
        assert_eq!(hints.app_password, None);
    }

    #[test]
    fn plain_http_enable_links_are_dropped() {
        let xml = "<clientConfig><emailProvider id='x'><displayName>  A\u{7}  B </displayName>\
                   <incomingServer type='imap'><hostname>imap.example.com</hostname><port>993</port><socketType>SSL</socketType></incomingServer>\
                   <enable visiturl='http://example.com/imap'/></emailProvider></clientConfig>";
        let Finding::Servers { hints, .. } = parse(xml, "test@example.com").unwrap().finding()
        else {
            panic!("expected servers");
        };
        assert_eq!(hints.enable_imap, None);
        assert_eq!(hints.provider.as_deref(), Some("A B"));
    }

    #[test]
    fn rejects_html_error_pages() {
        let html =
            "<!DOCTYPE html><html><head><title>404</title></head><body>Not found</body></html>";
        assert!(parse(html, "test@example.com").is_err());
        assert!(parse("", "test@example.com").is_err());
        assert!(parse("not xml at all", "test@example.com").is_err());
    }

    #[test]
    fn resolves_entities_in_text() {
        let xml = "<clientConfig><emailProvider id='x'><incomingServer type='imap'>\
                   <hostname>imap&#46;example&#x2E;com</hostname><port>993</port>\
                   <socketType>SSL</socketType><username>%EMAILLOCALPART%</username>\
                   </incomingServer></emailProvider></clientConfig>";
        let config = parse(xml, "test@example.com").unwrap();
        assert_eq!(config.servers[0].host, "imap.example.com");
        assert_eq!(config.servers[0].username, UsernameForm::LocalPart);
    }

    #[test]
    fn skips_unknown_socket_types_and_bad_ports() {
        let xml = "<clientConfig><emailProvider id='x'>\
                   <incomingServer type='imap'><hostname>a.example</hostname><port>99999</port><socketType>SSL</socketType></incomingServer>\
                   <incomingServer type='imap'><hostname>b.example</hostname><port>993</port><socketType>weird</socketType></incomingServer>\
                   <outgoingServer type='smtp'><hostname>c.example</hostname><socketType>SSL</socketType></outgoingServer>\
                   </emailProvider></clientConfig>";
        let config = parse(xml, "test@example").unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(
            (config.servers[0].host.as_str(), config.servers[0].port),
            ("c.example", 465)
        );
        assert_eq!(config.skipped.len(), 2);
    }

    #[test]
    fn literal_usernames_are_normalized() {
        let address = "test@example.com";
        assert_eq!(
            username_form(Some("%EMAILADDRESS%"), address),
            UsernameForm::Address
        );
        assert_eq!(
            username_form(Some(" %EMAILLOCALPART% "), address),
            UsernameForm::LocalPart
        );
        assert_eq!(username_form(None, address), UsernameForm::Address);
        assert_eq!(
            username_form(Some("TEST@example.com"), address),
            UsernameForm::Address
        );
        assert_eq!(
            username_form(Some("test"), address),
            UsernameForm::LocalPart
        );
        assert_eq!(
            username_form(Some("%EMAILLOCALPART%.%EMAILDOMAIN%"), address),
            UsernameForm::Other
        );
    }

    #[test]
    fn loopback_and_ip_literal_hosts_are_refused() {
        let xml = "<clientConfig><emailProvider id='x'>\
                   <incomingServer type='imap'><hostname>127.0.0.1</hostname><port>1143</port><socketType>STARTTLS</socketType></incomingServer>\
                   <incomingServer type='imap'><hostname>localhost</hostname><port>993</port><socketType>SSL</socketType></incomingServer>\
                   <outgoingServer type='smtp'><hostname>[::1]</hostname><port>1025</port><socketType>STARTTLS</socketType></outgoingServer>\
                   </emailProvider></clientConfig>";
        let config = parse(xml, "test@example.com").unwrap();
        assert!(config.servers.is_empty());
        assert_eq!(config.skipped.len(), 3);
        assert_eq!(config.finding(), Finding::Nothing);
    }
}
