use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};

use crate::model::{Auth, Protocol, Security, Server, Username};

#[derive(Debug, Default, PartialEq)]
pub struct ClientConfig {
    pub provider_id: Option<String>,
    pub domains: Vec<String>,
    pub servers: Vec<Server>,
    pub skipped: Vec<String>,
    pub oauth_issuer: Option<String>,
    pub enable_imap: bool,
}

#[derive(Default)]
struct PendingServer {
    kind: String,
    hostname: String,
    port: String,
    socket_type: String,
    username: Option<String>,
    auth: Vec<String>,
}

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
        "emailProvider" => {
            config.provider_id = attribute(start, "id");
        }
        "incomingServer" | "outgoingServer" => {
            *pending = Some(PendingServer {
                kind: attribute(start, "type")
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                ..PendingServer::default()
            });
        }
        "enable" => config.enable_imap = true,
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
        ("domain", Some("emailProvider")) => config.domains.push(value.to_ascii_lowercase()),
        ("issuer", Some("oAuth2")) => config.oauth_issuer = Some(value.to_string()),
        (field, Some("incomingServer" | "outgoingServer")) => {
            if let Some(server) = pending.as_mut() {
                match field {
                    "hostname" => server.hostname = value.to_string(),
                    "port" => server.port = value.to_string(),
                    "socketType" => server.socket_type = value.to_string(),
                    "username" => server.username = Some(value.to_string()),
                    "authentication" => server.auth.push(value.to_string()),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn finish(server: PendingServer, address: &str, config: &mut ClientConfig) {
    let domain = address
        .split_once('@')
        .map_or(address, |(_, domain)| domain);
    let protocol = match server.kind.as_str() {
        "imap" => Protocol::Imap,
        "pop3" => Protocol::Pop3,
        "smtp" => Protocol::Smtp,
        other => {
            config.skipped.push(format!("server type {other:?}"));
            return;
        }
    };
    let security = match server.socket_type.to_ascii_lowercase().as_str() {
        "ssl" | "tls" => Security::Tls,
        "starttls" => Security::StartTls,
        "plain" => Security::Plain,
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
    if host.is_empty() || host.contains('%') || host.contains('/') {
        config
            .skipped
            .push(format!("{protocol:?} hostname {:?}", server.hostname));
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
    let auth: Vec<Auth> = server.auth.iter().map(|a| auth_kind(a)).collect();
    config.servers.push(
        Server::new(protocol, &host, port, security)
            .with_username(username_form(server.username.as_deref(), address))
            .with_auth(&auth),
    );
}

pub fn username_form(template: Option<&str>, address: &str) -> Username {
    let local = address.split_once('@').map_or(address, |(local, _)| local);
    match template.map(str::trim) {
        None | Some("") | Some("%EMAILADDRESS%") | Some("%EMAILLOCALPART%@%EMAILDOMAIN%") => {
            Username::Address
        }
        Some("%EMAILLOCALPART%") => Username::LocalPart,
        Some(literal) if literal.eq_ignore_ascii_case(address) => Username::Address,
        Some(literal) if literal.eq_ignore_ascii_case(local) => Username::LocalPart,
        Some(other) => Username::Template(other.to_string()),
    }
}

fn auth_kind(value: &str) -> Auth {
    match value.trim() {
        "password-cleartext" | "plain" => Auth::Password,
        "password-encrypted" | "secure" => Auth::PasswordEncrypted,
        "OAuth2" => Auth::OAuth2,
        other => Auth::Other(other.to_string()),
    }
}

pub fn default_port(protocol: Protocol, security: Security) -> u16 {
    match (protocol, security) {
        (Protocol::Imap, Security::Tls) => 993,
        (Protocol::Imap, _) => 143,
        (Protocol::Pop3, Security::Tls) => 995,
        (Protocol::Pop3, _) => 110,
        (Protocol::Smtp, Security::Tls) => 465,
        (Protocol::Smtp, _) => 587,
    }
}

fn attribute(start: &BytesStart<'_>, key: &str) -> Option<String> {
    let attribute = start.try_get_attribute(key).ok().flatten()?;
    attribute
        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
        .ok()
        .map(|v| v.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GMAIL_LIKE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <domain>Example.NET</domain>
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
      <port>110</port>
      <socketType>plain</socketType>
      <username>%EMAILLOCALPART%</username>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="exchange"><hostname>x</hostname></incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.com</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILLOCALPART%@%EMAILDOMAIN%</username>
      <authentication>password-encrypted</authentication>
    </outgoingServer>
  </emailProvider>
  <oAuth2><issuer>accounts.example.com</issuer></oAuth2>
  <enable visiturl="https://example.com/settings?a=1&amp;b=2"><instruction>Enable IMAP</instruction></enable>
</clientConfig>"#;

    #[test]
    fn parses_servers_in_document_order() {
        let config = parse(GMAIL_LIKE, "test@example.com").unwrap();
        assert_eq!(config.provider_id.as_deref(), Some("example.com"));
        assert_eq!(config.domains, ["example.com", "example.net"]);
        assert_eq!(config.servers.len(), 3);
        let imap = &config.servers[0];
        assert_eq!(
            (imap.protocol, imap.host.as_str(), imap.port),
            (Protocol::Imap, "imap.example.com", 993)
        );
        assert_eq!(imap.security, Security::Tls);
        assert_eq!(imap.username, Username::Address);
        assert_eq!(imap.auth, [Auth::OAuth2, Auth::Password]);
        let pop = &config.servers[1];
        assert_eq!(pop.host, "pop.example.com");
        assert_eq!(pop.security, Security::Plain);
        assert_eq!(pop.username, Username::LocalPart);
        let smtp = &config.servers[2];
        assert_eq!((smtp.port, smtp.security), (587, Security::StartTls));
        assert_eq!(smtp.username, Username::Address);
        assert_eq!(config.skipped, ["server type \"exchange\""]);
        assert_eq!(config.oauth_issuer.as_deref(), Some("accounts.example.com"));
        assert!(config.enable_imap);
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
            Username::Address
        );
        assert_eq!(
            username_form(Some(" %EMAILLOCALPART% "), address),
            Username::LocalPart
        );
        assert_eq!(username_form(None, address), Username::Address);
        assert_eq!(
            username_form(Some("TEST@example.com"), address),
            Username::Address
        );
        assert_eq!(username_form(Some("test"), address), Username::LocalPart);
        assert_eq!(
            username_form(Some("%EMAILLOCALPART%.%EMAILDOMAIN%"), address),
            Username::Template("%EMAILLOCALPART%.%EMAILDOMAIN%".into())
        );
    }
}
