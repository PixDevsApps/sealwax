use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;

#[test]
fn pkce_matches_the_rfc_7636_example() {
    assert_eq!(
        challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn pkce_verifier_has_a_valid_length() {
    let pkce = Pkce::new().unwrap();
    assert!((43..=128).contains(&pkce.verifier.len()));
    assert_eq!(pkce.challenge, challenge_for(&pkce.verifier));
}

#[test]
fn xoauth2_has_the_documented_shape() {
    assert_eq!(
        xoauth2("test@example.com", "token").as_str(),
        "user=test@example.com\x01auth=Bearer token\x01\x01"
    );
}

/// A fake authorization server: `/authorize` redirects to the loopback with a code, `/token`
/// checks the PKCE verifier against the challenge it saw.
struct MockIdp {
    port: u16,
}

#[derive(Default)]
struct Seen {
    challenge: Option<String>,
    redirect_uri: Option<String>,
}

impl MockIdp {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let seen = Arc::clone(&seen);
                tokio::spawn(async move { serve(stream, seen).await });
            }
        });
        Self { port }
    }

    fn provider(&self) -> Provider {
        Provider {
            name: "mock",
            authorize_url: format!("http://127.0.0.1:{}/authorize", self.port),
            token_url: format!("http://127.0.0.1:{}/token", self.port),
            scope: "mail",
            redirect_host: "127.0.0.1",
            extra: &[("prompt", "consent")],
        }
    }
}

async fn serve(stream: tokio::net::TcpStream, seen: Arc<Mutex<Seen>>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await.unwrap();
    let mut content_length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.unwrap() <= 2 {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).await.unwrap();
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_owned();
    let response = if target.starts_with("/authorize") {
        let query = parse_query(&target);
        let redirect = query["redirect_uri"].clone();
        {
            let mut seen = seen.lock().unwrap();
            seen.challenge = Some(query["code_challenge"].clone());
            seen.redirect_uri = Some(redirect.clone());
        }
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {redirect}?code=code-1&state={}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            query["state"]
        )
    } else {
        let form: HashMap<String, String> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        let seen = seen.lock().unwrap();
        let json = match form["grant_type"].as_str() {
            "authorization_code"
                if form["code"] == "code-1"
                    && Some(challenge_for(&form["code_verifier"])) == seen.challenge
                    && Some(&form["redirect_uri"]) == seen.redirect_uri.as_ref() =>
            {
                Some(
                    r#"{"access_token":"access-1","refresh_token":"refresh-1","expires_in":3600,"token_type":"Bearer"}"#,
                )
            }
            "refresh_token" if form["refresh_token"] == "refresh-1" => {
                Some(r#"{"access_token":"access-2","expires_in":3600,"token_type":"Bearer"}"#)
            }
            _ => None,
        };
        match json {
            Some(json) => format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                json.len()
            ),
            None => {
                let json = r#"{"error":"invalid_grant","error_description":"mock refused"}"#;
                format!(
                    "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                    json.len()
                )
            }
        }
    };
    let mut stream = reader.into_inner();
    stream.write_all(response.as_bytes()).await.unwrap();
}

#[tokio::test]
async fn full_flow_against_a_mock_provider() {
    let idp = MockIdp::start().await;
    let provider = idp.provider();
    let client = Client {
        id: "client-1".into(),
        secret: None,
    };
    let authorization = begin(&provider, &client, Some("test@example.com"))
        .await
        .unwrap();
    assert!(authorization.url.contains("code_challenge_method=S256"));
    assert!(authorization.url.contains("login_hint=test%40example.com"));
    let url = authorization.url.clone();
    let browser = tokio::spawn(async move {
        reqwest::Client::new()
            .get(url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    let http = http_client();
    let tokens = finish(
        &http,
        &provider,
        &client,
        authorization,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(tokens.access.as_str(), "access-1");
    assert_eq!(
        tokens.refresh.as_deref().map(String::as_str),
        Some("refresh-1")
    );
    assert_eq!(tokens.expires_in, Some(Duration::from_secs(3600)));
    assert!(browser.await.unwrap().contains("close this tab"));

    let refreshed = refresh(&http, &provider, &client, "refresh-1")
        .await
        .unwrap();
    assert_eq!(refreshed.access.as_str(), "access-2");
    assert!(refreshed.refresh.is_none());

    let error = refresh(&http, &provider, &client, "stale")
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::Token(message) if message.contains("invalid_grant")));
}

#[tokio::test]
async fn a_redirect_with_the_wrong_state_is_refused() {
    let idp = MockIdp::start().await;
    let provider = idp.provider();
    let client = Client {
        id: "client-1".into(),
        secret: None,
    };
    let authorization = begin(&provider, &client, None).await.unwrap();
    let forged = format!(
        "{}?code=code-1&state=forged",
        authorization.loopback.redirect_uri
    );
    tokio::spawn(async move { reqwest::Client::new().get(forged).send().await });
    let error = finish(
        &http_client(),
        &provider,
        &client,
        authorization,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, OAuthError::StateMismatch));
}

#[tokio::test]
async fn a_denied_sign_in_is_reported() {
    let idp = MockIdp::start().await;
    let provider = idp.provider();
    let client = Client {
        id: "client-1".into(),
        secret: None,
    };
    let authorization = begin(&provider, &client, None).await.unwrap();
    let denied = format!(
        "{}?error=access_denied&state={}",
        authorization.loopback.redirect_uri, authorization.state
    );
    tokio::spawn(async move { reqwest::Client::new().get(denied).send().await });
    let error = finish(
        &http_client(),
        &provider,
        &client,
        authorization,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, OAuthError::Denied(reason) if reason == "access_denied"));
}

#[tokio::test]
async fn no_browser_times_out() {
    let loopback = Loopback::bind("127.0.0.1").await.unwrap();
    let error = loopback.wait(Duration::from_millis(100)).await.unwrap_err();
    assert!(matches!(error, OAuthError::Timeout(_)));
}
