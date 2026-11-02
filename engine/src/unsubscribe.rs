//! Unsubscribe's one-click POST (RFC 8058, ADR-023), sent only when the user confirmed it. It
//! carries `List-Unsubscribe=One-Click` and nothing about the user: the generic User-Agent, no
//! cookies, no credentials, no Referer, no proxy, rustls with the platform verifier. The host is
//! resolved first and refused unless every address is public, and the connection goes only to
//! the addresses checked, so a message can't make Sealwax POST into the user's own network. No
//! redirect is followed. The URI is the sender's: never logged.

use crate::api::UnsubscribeError;
use crate::net::Trust;
use sealwax_domain::autoconfig::dns::public_address;
use std::net::SocketAddr;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);
const BODY: &str = "List-Unsubscribe=One-Click";

/// POSTs RFC 8058's one-click form to `uri`, an HTTPS URI from List-Unsubscribe.
pub async fn one_click(uri: &str) -> Result<(), UnsubscribeError> {
    post(uri, false).await
}

/// `local`: the tests' plain HTTP to this computer.
async fn post(uri: &str, local: bool) -> Result<(), UnsubscribeError> {
    let url = url::Url::parse(uri).map_err(|_| UnsubscribeError::Refused)?;
    let https = url.scheme() == "https";
    if !(https || local && url.scheme() == "http")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(UnsubscribeError::Refused);
    }
    let port = url
        .port_or_known_default()
        .ok_or(UnsubscribeError::Refused)?;
    let (name, addresses): (Option<String>, Vec<SocketAddr>) = match url.host() {
        Some(url::Host::Ipv4(address)) => (None, vec![SocketAddr::new(address.into(), port)]),
        Some(url::Host::Ipv6(address)) => (None, vec![SocketAddr::new(address.into(), port)]),
        Some(url::Host::Domain(domain)) => {
            let found = tokio::time::timeout(TIMEOUT, tokio::net::lookup_host((domain, port)))
                .await
                .map_err(|_| UnsubscribeError::Timeout)?
                .map_err(|_| UnsubscribeError::Offline)?;
            (Some(domain.to_owned()), found.collect())
        }
        None => return Err(UnsubscribeError::Refused),
    };
    if addresses.is_empty() {
        return Err(UnsubscribeError::Offline);
    }
    if !local && !addresses.iter().all(|address| public_address(address.ip())) {
        return Err(UnsubscribeError::Refused);
    }
    let tls = Trust::Platform
        .client_config()
        .map_err(|_| UnsubscribeError::Offline)?;
    let mut builder = reqwest::Client::builder()
        .user_agent("Sealwax")
        .https_only(https)
        .no_proxy()
        .referer(false)
        .redirect(reqwest::redirect::Policy::none())
        .tls_backend_preconfigured((*tls).clone())
        .connect_timeout(TIMEOUT)
        .timeout(TIMEOUT);
    if let Some(name) = &name {
        builder = builder.resolve_to_addrs(name, &addresses);
    }
    let http = builder.build().map_err(|_| UnsubscribeError::Offline)?;
    let response = http
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(BODY)
        .send()
        .await
        .map_err(transport)?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        tracing::info!(
            status = status.as_u16(),
            "a one-click unsubscribe wasn't confirmed"
        );
        Err(UnsubscribeError::NotConfirmed)
    }
}

fn transport(error: reqwest::Error) -> UnsubscribeError {
    if error.is_timeout() {
        UnsubscribeError::Timeout
    } else {
        UnsubscribeError::Offline
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A server on 127.0.0.1 that answers one request with `status`, and keeps what it read.
    async fn server(status: &'static str) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/u/1?list=nyheter", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(String::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if text.contains("\r\n\r\n") && text.ends_with(BODY) {
                    break;
                }
            }
            *record.lock().unwrap() = String::from_utf8_lossy(&request).into_owned();
            let answer = format!(
                "HTTP/1.1 {status}\r\nLocation: http://127.0.0.1:9/next\r\nContent-Length: 0\r\n\
                 Connection: close\r\n\r\n"
            );
            stream.write_all(answer.as_bytes()).await.unwrap();
            stream.shutdown().await.ok();
        });
        (url, seen)
    }

    #[tokio::test]
    async fn posts_the_one_click_form_and_nothing_about_the_user() {
        let (url, seen) = server("200 OK").await;
        assert_eq!(post(&url, true).await, Ok(()));
        let request = seen.lock().unwrap().clone();
        assert!(
            request.starts_with("POST /u/1?list=nyheter HTTP/1.1\r\n"),
            "{request}"
        );
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("content-type: application/x-www-form-urlencoded\r\n"));
        assert!(lower.contains("user-agent: sealwax\r\n"));
        assert!(request.ends_with("\r\n\r\nList-Unsubscribe=One-Click"));
        for header in ["cookie", "authorization", "referer"] {
            assert!(!lower.contains(&format!("\r\n{header}:")), "{header}");
        }
    }

    #[tokio::test]
    async fn a_redirect_or_a_refusal_isnt_a_confirmation() {
        for status in ["302 Found", "404 Not Found", "500 Internal Server Error"] {
            let (url, _) = server(status).await;
            assert_eq!(
                post(&url, true).await,
                Err(UnsubscribeError::NotConfirmed),
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn only_https_to_public_addresses() {
        for uri in [
            "http://list.example/u/1",
            "mailto:ut@list.example",
            "https://user:secret@list.example/u",
            "https://127.0.0.1/u",
            "https://[::1]/u",
            "https://10.0.0.1/u",
            "https://192.168.1.1/u",
            "https://169.254.169.254/latest",
            "https://[fd00::1]/u",
            "https://localhost/u",
            "not a uri",
        ] {
            assert_eq!(
                one_click(uri).await,
                Err(UnsubscribeError::Refused),
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn nothing_listening() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/u", listener.local_addr().unwrap());
        drop(listener);
        assert_eq!(post(&url, true).await, Err(UnsubscribeError::Offline));
    }
}
