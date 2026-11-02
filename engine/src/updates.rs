//! Check for Updates (ADR-021 amendment, 2026-10-20): one GET of GitHub's answer for Sealwax's
//! latest release, made only when the user runs the command. It carries nothing about the user:
//! a generic User-Agent, no cookies, no query, no Referer; rustls with the platform verifier and
//! no proxy, as the sign-in's requests. Only the answer's `tag_name` is read.

use crate::api::UpdateError;
use crate::net::Trust;
use sealwax_domain::updates::{REPOSITORY, Version};
use std::time::Duration;

/// GitHub's latest release of Sealwax: drafts and pre-releases are never it.
pub fn latest_release_url() -> String {
    format!("https://api.github.com/repos/{REPOSITORY}/releases/latest")
}

const TIMEOUT: Duration = Duration::from_secs(20);
/// The largest answer read; a release's holds its notes and its files' list.
const MAX_ANSWER: usize = 512 * 1024;

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
}

/// The version of the latest release `url` names. `url` is HTTPS, or plain HTTP to this
/// computer for the tests.
pub async fn latest(url: &str) -> Result<Version, UpdateError> {
    let url = url::Url::parse(url).map_err(|_| UpdateError::Unreadable)?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    let https = url.scheme() == "https";
    if !https && !(url.scheme() == "http" && loopback) {
        return Err(UpdateError::Unreadable);
    }
    let tls = Trust::Platform
        .client_config()
        .map_err(|_| UpdateError::Offline)?;
    let http = reqwest::Client::builder()
        .user_agent("Sealwax")
        .https_only(https)
        .no_proxy()
        .referer(false)
        .redirect(reqwest::redirect::Policy::limited(3))
        .tls_backend_preconfigured((*tls).clone())
        .connect_timeout(TIMEOUT)
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| UpdateError::Offline)?;
    let mut response = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(transport)?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(UpdateError::NoRelease);
    }
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err(UpdateError::Busy);
    }
    if !status.is_success() {
        tracing::info!(status = status.as_u16(), "the update check got no release");
        return Err(UpdateError::Unreadable);
    }
    let mut answer = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if answer.len() + chunk.len() > MAX_ANSWER {
            return Err(UpdateError::Unreadable);
        }
        answer.extend_from_slice(&chunk);
    }
    let release: Release = serde_json::from_slice(&answer).map_err(|_| UpdateError::Unreadable)?;
    Version::parse(&release.tag_name).ok_or(UpdateError::Unreadable)
}

/// A request that got no answer: a timeout, a certificate that didn't verify, or no connection.
fn transport(error: reqwest::Error) -> UpdateError {
    if error.is_timeout() {
        return UpdateError::Timeout;
    }
    let invalid = |error: &(dyn std::error::Error + 'static)| {
        matches!(
            error.downcast_ref::<rustls::Error>(),
            Some(rustls::Error::InvalidCertificate(_))
        )
    };
    let mut source = std::error::Error::source(&error);
    while let Some(inner) = source {
        let wrapped = inner
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
            .is_some_and(|io| invalid(io));
        if invalid(inner) || wrapped {
            return UpdateError::Certificate;
        }
        source = inner.source();
    }
    UpdateError::Offline
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A server on 127.0.0.1 that answers one request with `status` and `body`, and keeps the
    /// request it read.
    async fn server(status: &'static str, body: String) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/repos/{REPOSITORY}/releases/latest",
            listener.local_addr().unwrap()
        );
        let seen = Arc::new(Mutex::new(String::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            *record.lock().unwrap() = String::from_utf8_lossy(&request).into_owned();
            let answer = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).await.unwrap();
            stream.shutdown().await.ok();
        });
        (url, seen)
    }

    #[tokio::test]
    async fn reads_the_latest_tag_and_sends_nothing_about_the_user() {
        let body = serde_json::json!({
            "tag_name": "v0.6.0",
            "name": "Sealwax 0.6.0",
            "draft": false,
            "prerelease": false,
            "body": "Notes",
            "assets": [{"name": "sealwax-x86_64.pkg.tar.zst"}],
        })
        .to_string();
        let (url, seen) = server("200 OK", body).await;
        assert_eq!(
            latest(&url).await,
            Ok(Version {
                major: 0,
                minor: 6,
                patch: 0
            })
        );
        let request = seen.lock().unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("get /repos/pixdevsapps/sealwax/releases/latest http/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains("\r\nuser-agent: sealwax\r\n"), "{request}");
        assert!(
            request.contains("\r\naccept: application/vnd.github+json\r\n"),
            "{request}"
        );
        for header in ["cookie", "referer", "authorization"] {
            assert!(!request.contains(&format!("\r\n{header}:")), "{request}");
        }
    }

    #[tokio::test]
    async fn answers_that_name_no_version() {
        let tag = |tag: &str| serde_json::json!({ "tag_name": tag }).to_string();
        for body in [
            tag("v0.6.0; rm -rf ~"),
            tag("nightly"),
            "{}".to_owned(),
            "not json".to_owned(),
        ] {
            let (url, _) = server("200 OK", body.clone()).await;
            assert_eq!(latest(&url).await, Err(UpdateError::Unreadable), "{body}");
        }
        let (url, _) = server("200 OK", " ".repeat(MAX_ANSWER + 1)).await;
        assert_eq!(latest(&url).await, Err(UpdateError::Unreadable));
    }

    #[tokio::test]
    async fn refusals() {
        for (status, error) in [
            ("404 Not Found", UpdateError::NoRelease),
            ("403 Forbidden", UpdateError::Busy),
            ("429 Too Many Requests", UpdateError::Busy),
            ("500 Internal Server Error", UpdateError::Unreadable),
        ] {
            let (url, _) = server(status, "{}".to_owned()).await;
            assert_eq!(latest(&url).await, Err(error), "{status}");
        }
    }

    #[tokio::test]
    async fn nothing_listening() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        assert_eq!(latest(&url).await, Err(UpdateError::Offline));
    }

    /// The real answer: `cargo test -p sealwax-engine updates::tests::github -- --ignored`.
    #[tokio::test]
    #[ignore = "reaches api.github.com"]
    async fn github() {
        let latest = latest(&latest_release_url()).await.unwrap();
        println!("the latest release is {latest}");
    }

    #[tokio::test]
    async fn only_https_or_this_computer() {
        for url in [
            "http://api.github.com/repos/PixDevsApps/sealwax/releases/latest",
            "ftp://127.0.0.1/",
            "not a url",
        ] {
            assert_eq!(latest(url).await, Err(UpdateError::Unreadable), "{url}");
        }
        assert_eq!(
            latest_release_url(),
            "https://api.github.com/repos/PixDevsApps/sealwax/releases/latest"
        );
    }
}
