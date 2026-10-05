//! The OAuth module against a mock identity platform on 127.0.0.1 (ported from S5's tests):
//! PKCE, the sign-in page's URL, the loopback, the code exchange and refresh, how refusals are
//! classified, the ID token's checks, and the access-token cache with rotation.

use super::error::{Grant, aadsts_in, authorize_error, token_error};
use super::flow::{Loopback, challenge, random};
use super::id_token::check;
use super::*;
use crate::secrets::{MemoryStore, SecretKind, SecretStore};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sealwax_domain::account::{AccountKey, LoginFailure};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const CLIENT: &str = "client-0001";
const WORK_TENANT: &str = "0b5e8d2a-1c3f-4e6a-9d7b-2f4a6c8e0a1b";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A JWT with these claims and a signature nobody checks.
fn jwt(claims: &serde_json::Value) -> String {
    let encode = |text: &str| URL_SAFE_NO_PAD.encode(text);
    format!(
        "{}.{}.{}",
        encode(r#"{"alg":"RS256","typ":"JWT"}"#),
        encode(&claims.to_string()),
        encode("signature")
    )
}

fn claims(username: &str, tenant: &str) -> serde_json::Value {
    serde_json::json!({
        "iss": format!("https://login.microsoftonline.com/{tenant}/v2.0"),
        "aud": CLIENT,
        "exp": now() + 3600,
        "iat": now(),
        "tid": tenant,
        "preferred_username": username,
        "ver": "2.0",
    })
}

#[derive(Default)]
struct MockState {
    challenge: Option<String>,
    redirect_uri: Option<String>,
    username: String,
    lifetime: u64,
    issued: u32,
    codes: HashSet<String>,
    refresh: HashSet<String>,
    /// Every grant asked for: `authorization_code`, or `refresh_token` and the token.
    grants: Vec<String>,
    /// The token endpoint's answer to the next code exchange instead of tokens.
    refuse_code: Option<&'static str>,
    no_refresh_token: bool,
}

/// A fake Microsoft: `/authorize` redirects to the loopback with a code, `/token` checks the
/// PKCE verifier against the challenge it saw and rotates refresh tokens.
struct Mock {
    port: u16,
    state: Arc<Mutex<MockState>>,
}

impl Mock {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(MockState {
            username: "signin@example.test".into(),
            lifetime: 3600,
            ..MockState::default()
        }));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, shared.clone()));
            }
        });
        Self { port, state }
    }

    fn provider(&self) -> Provider {
        Provider::new(
            &format!("http://127.0.0.1:{}/authorize", self.port),
            &format!("http://127.0.0.1:{}/token", self.port),
        )
        .unwrap()
    }

    fn client(&self) -> Client {
        Client::new(CLIENT.into(), self.provider()).unwrap()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state.lock().unwrap()
    }

    /// A refresh token the mock accepts, as if a sign-in had issued it.
    fn grant_refresh_token(&self) -> String {
        let mut state = self.state();
        state.issued += 1;
        let token = format!("refresh-{}", state.issued);
        state.refresh.insert(token.clone());
        token
    }
}

fn answer(status: &str, json: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{json}",
        json.len()
    )
}

/// New tokens from the mock, with an ID token unless `id_token` is false.
fn tokens(state: &mut MockState, id_token: bool) -> String {
    state.issued += 1;
    let n = state.issued;
    let refresh = format!("refresh-{n}");
    state.refresh.insert(refresh.clone());
    let mut json = serde_json::json!({
        "token_type": "Bearer",
        "access_token": format!("access-{n}"),
        "expires_in": state.lifetime,
        "scope": "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send",
    });
    if !state.no_refresh_token {
        json["refresh_token"] = refresh.into();
    }
    if id_token {
        json["id_token"] = jwt(&claims(&state.username, CONSUMER_TENANT)).into();
    }
    answer("200 OK", &json.to_string())
}

async fn serve(stream: TcpStream, state: Arc<Mutex<MockState>>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await.unwrap();
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.unwrap() <= 2 {
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
        state.challenge = Some(query["code_challenge"].clone());
        state.redirect_uri = Some(query["redirect_uri"].clone());
        state.issued += 1;
        let code = format!("code-{}", state.issued);
        state.codes.insert(code.clone());
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {}?code={code}&state={}\r\ncontent-length: 0\r\n\
             connection: close\r\n\r\n",
            query["redirect_uri"], query["state"]
        )
    } else {
        let form: HashMap<String, String> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        let mut state = state.lock().unwrap();
        assert_eq!(form["client_id"], CLIENT);
        match form["grant_type"].as_str() {
            "authorization_code" => {
                state.grants.push("authorization_code".into());
                let fits = state.codes.remove(&form["code"])
                    && Some(challenge(&form["code_verifier"])) == state.challenge
                    && Some(&form["redirect_uri"]) == state.redirect_uri.as_ref();
                match state.refuse_code.take() {
                    Some(json) => answer("400 Bad Request", json),
                    None if fits => tokens(&mut state, true),
                    None => answer("400 Bad Request", r#"{"error":"invalid_grant"}"#),
                }
            }
            "refresh_token" => {
                state
                    .grants
                    .push(format!("refresh_token {}", form["refresh_token"]));
                if state.refresh.remove(&form["refresh_token"]) {
                    tokens(&mut state, true)
                } else {
                    answer(
                        "400 Bad Request",
                        r#"{"error":"invalid_grant","error_description":"AADSTS70000: The provided grant has expired. Trace ID: x","error_codes":[70000]}"#,
                    )
                }
            }
            _ => answer("400 Bad Request", r#"{"error":"unsupported_grant_type"}"#),
        }
    };
    reader
        .into_inner()
        .write_all(response.as_bytes())
        .await
        .unwrap();
}

/// The browser: follows the sign-in page's redirect to the loopback and returns the page shown.
async fn browse(url: String) -> String {
    let tls = crate::net::Trust::Platform.client_config().unwrap();
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

fn query_of(url: &str) -> HashMap<String, String> {
    let url = url::Url::parse(url).unwrap();
    url.query_pairs().into_owned().collect()
}

/// A plain GET to the loopback; the status line's code, or 0 when the connection failed.
async fn get(port: u16, target: &str) -> u16 {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await else {
        return 0;
    };
    stream
        .write_all(format!("GET {target} HTTP/1.1\r\nhost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response).await;
    response
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

fn port_of(redirect_uri: &str) -> u16 {
    redirect_uri.rsplit(':').next().unwrap().parse().unwrap()
}

#[test]
fn pkce_matches_the_rfc_7636_example() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let verifier = random(64).unwrap();
    assert_eq!(verifier.len(), 86, "RFC 7636 allows 43–128 characters");
    assert!(
        verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    assert_ne!(*verifier, *random(64).unwrap());
}

#[test]
fn endpoints_must_be_https_except_on_loopback() {
    assert!(
        Provider::new(
            "https://login.example.test/a",
            "https://login.example.test/t"
        )
        .is_ok()
    );
    assert!(Provider::new("http://127.0.0.1:1/a", "http://[::1]:1/t").is_ok());
    for bad in [
        "http://login.example.test/t",
        "http://localhost:1/t",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert!(
            matches!(
                Provider::new("https://login.example.test/a", bad),
                Err(OAuthError::Endpoint)
            ),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn the_sign_in_page_asks_microsoft_without_the_address() {
    let client = Client::new(CLIENT.into(), Provider::microsoft()).unwrap();
    let authorization = client.begin().await.unwrap();
    let url = url::Url::parse(authorization.url()).unwrap();
    assert_eq!(
        url[..url::Position::AfterPath].to_string(),
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
    );
    let query = query_of(authorization.url());
    assert_eq!(query["client_id"], CLIENT);
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["prompt"], "select_account");
    assert_eq!(query["code_challenge"].len(), 43);
    assert!(query["state"].len() >= 22);
    let redirect = &query["redirect_uri"];
    assert!(redirect.starts_with("http://localhost:") && !redirect.ends_with('/'));
    let scopes: HashSet<&str> = query["scope"].split(' ').collect();
    assert_eq!(
        scopes,
        HashSet::from([
            "openid",
            "profile",
            "offline_access",
            "https://outlook.office.com/IMAP.AccessAsUser.All",
            "https://outlook.office.com/SMTP.Send",
        ])
    );
    assert!(
        query["scope"].starts_with("https://outlook.office.com/"),
        "the Outlook resource comes first"
    );
    assert!(!query.contains_key("login_hint"));
    assert!(!authorization.url().contains("example.test"));
    assert_eq!(format!("{authorization:?}"), "Authorization(..)");
}

#[tokio::test]
async fn a_sign_in_completes_against_the_mock() {
    let mock = Mock::start().await;
    let client = mock.client();
    let authorization = client.begin().await.unwrap();
    let browser = tokio::spawn(browse(authorization.url().to_owned()));
    let granted = client
        .finish(authorization, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(
        browser
            .await
            .unwrap()
            .contains("Sealwax has the sign-in. You can close this tab")
    );
    assert_eq!(granted.identity.username, "signin@example.test");
    assert!(granted.identity.consumer());
    assert_eq!(granted.tokens.access.as_str(), "access-2");
    assert_eq!(
        granted.tokens.refresh.as_deref().map(String::as_str),
        Some("refresh-2")
    );
    assert_eq!(granted.tokens.lifetime, Duration::from_secs(3600));
    assert!(
        granted
            .tokens
            .granted("https://outlook.office.com/SMTP.Send")
    );
    assert!(!granted.tokens.granted("openid"));
    assert_eq!(mock.state().grants, ["authorization_code"]);
    let shown = format!("{granted:?}");
    for secret in ["access-2", "refresh-2", "signin@example.test", "code-1"] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

#[tokio::test]
async fn the_loopback_ignores_other_requests_until_the_right_state_arrives() {
    let mock = Mock::start().await;
    let client = mock.client();
    let authorization = client.begin().await.unwrap();
    let query = query_of(authorization.url());
    let port = port_of(&query["redirect_uri"]);
    let state = query["state"].clone();
    let waiting = tokio::spawn(async move {
        client
            .finish(authorization, Duration::from_secs(10))
            .await
            .map(|granted| granted.identity.username)
    });
    // A connection that never sends anything (a browser's preconnect) blocks nobody.
    let _idle = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    assert_eq!(get(port, "/favicon.ico").await, 404);
    assert_eq!(get(port, "/?code=forged&state=forged").await, 404);
    assert_eq!(
        get(port, &format!("/elsewhere?code=x&state={state}")).await,
        404
    );
    assert_eq!(get(port, "/?error=access_denied").await, 404);
    assert!(!waiting.is_finished());
    // The right state ends the wait; the mock never issued this code, so the exchange is
    // refused, which is beside the point here.
    assert_eq!(
        get(port, &format!("/?code=unknown&state={state}")).await,
        200
    );
    let error = waiting.await.unwrap().unwrap_err();
    assert!(matches!(error, OAuthError::Refused(None)), "{error}");
    assert_eq!(mock.state().grants, ["authorization_code"]);
    let mut closed = false;
    for _ in 0..50 {
        if get(port, "/").await == 0 {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(closed, "the loopback stops listening");
}

#[tokio::test]
async fn a_declined_sign_in_is_reported() {
    for (query, failure, code) in [
        ("error=access_denied", LoginFailure::SignInDeclined, None),
        (
            "error=invalid_request&error_description=AADSTS65004%3A+User+declined+to+consent",
            LoginFailure::SignInDeclined,
            Some(65004),
        ),
        ("error=consent_required", LoginFailure::SignInDeclined, None),
        (
            "error=server_error&error_description=AADSTS90033",
            LoginFailure::Other,
            Some(90033),
        ),
    ] {
        let mock = Mock::start().await;
        let client = mock.client();
        let authorization = client.begin().await.unwrap();
        let params = query_of(authorization.url());
        let port = port_of(&params["redirect_uri"]);
        let target = format!("/?{query}&state={}", params["state"]);
        let page = tokio::spawn(async move { get(port, &target).await });
        let error = client
            .finish(authorization, Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(page.await.unwrap(), 200);
        assert_eq!(error.failure(), failure, "{query}");
        assert!(
            matches!(error, OAuthError::Declined(found) | OAuthError::Authorize(found) if found == code),
            "{query}: {error}"
        );
        assert!(!error.to_string().contains("declined to consent"));
        assert!(mock.state().grants.is_empty(), "no exchange after an error");
    }
}

#[tokio::test]
async fn the_loopback_listens_on_both_loopback_addresses() {
    let ipv6 = TcpListener::bind("[::1]:0").await.is_ok();
    let loopback = Loopback::bind().await.unwrap();
    let port = port_of(&loopback.redirect_uri());
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_ok());
    assert_eq!(TcpStream::connect(("::1", port)).await.is_ok(), ipv6);
}

#[tokio::test]
async fn no_browser_times_out() {
    let loopback = Loopback::bind().await.unwrap();
    let error = loopback
        .wait("state", Duration::from_millis(100))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, OAuthError::BrowserTimeout(_)));
    assert_eq!(error.failure(), LoginFailure::Timeout);
}

#[tokio::test]
async fn an_exchange_without_a_refresh_token_or_an_id_token_fails() {
    let mock = Mock::start().await;
    mock.state().no_refresh_token = true;
    let client = mock.client();
    let authorization = client.begin().await.unwrap();
    tokio::spawn(browse(authorization.url().to_owned()));
    let error = client
        .finish(authorization, Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::NoRefreshToken), "{error}");

    let mock = Mock::start().await;
    mock.state().refuse_code = Some(
        r#"{"error":"invalid_grant","error_description":"AADSTS65001: The user or administrator has not consented","error_codes":[65001]}"#,
    );
    let client = mock.client();
    let authorization = client.begin().await.unwrap();
    tokio::spawn(browse(authorization.url().to_owned()));
    let error = client
        .finish(authorization, Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(
        matches!(error, OAuthError::Declined(Some(65001))),
        "{error}"
    );
    assert_eq!(error.failure(), LoginFailure::SignInDeclined);
    assert_eq!(
        error.to_string(),
        "the sign-in was declined (AADSTS65001)",
        "no server text"
    );
}

#[tokio::test]
async fn refreshes_rotate_and_a_refused_refresh_means_signing_in_again() {
    let mock = Mock::start().await;
    let client = mock.client();
    let first = mock.grant_refresh_token();
    let refreshed = client.refresh(&first).await.unwrap();
    assert_eq!(refreshed.access.as_str(), "access-2");
    assert_eq!(
        refreshed.refresh.as_deref().map(String::as_str),
        Some("refresh-2")
    );
    let error = client.refresh(&first).await.unwrap_err();
    assert!(
        matches!(error, OAuthError::RefreshRefused(Some(70000))),
        "{error}"
    );
    assert_eq!(error.failure(), LoginFailure::OAuthRequired);
    assert!(!error.to_string().contains("expired"), "no server text");
}

#[tokio::test]
async fn a_token_endpoint_that_is_down_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let provider = Provider::new(
        &format!("http://127.0.0.1:{port}/authorize"),
        &format!("http://127.0.0.1:{port}/token"),
    )
    .unwrap();
    let client = Client::new(CLIENT.into(), provider).unwrap();
    let error = client.refresh("refresh-1").await.unwrap_err();
    assert!(matches!(error, OAuthError::Unreachable), "{error}");
    assert_eq!(error.failure(), LoginFailure::Unreachable);
}

#[test]
fn refusals_are_classified() {
    assert_eq!(aadsts_in("AADSTS65004: User declined"), Some(65004));
    assert_eq!(aadsts_in("x AADSTS7000215: y"), Some(7000215));
    assert_eq!(aadsts_in("AADSTS: none"), None);
    assert_eq!(aadsts_in("plain text"), None);
    for (grant, error, code, failure) in [
        (
            Grant::Refresh,
            "invalid_grant",
            None,
            LoginFailure::OAuthRequired,
        ),
        (
            Grant::Refresh,
            "invalid_grant",
            Some(65001),
            LoginFailure::OAuthRequired,
        ),
        (
            Grant::Refresh,
            "interaction_required",
            Some(50076),
            LoginFailure::OAuthRequired,
        ),
        (
            Grant::Refresh,
            "invalid_client",
            Some(700016),
            LoginFailure::Other,
        ),
        (
            Grant::Code,
            "invalid_grant",
            Some(70008),
            LoginFailure::Other,
        ),
        (
            Grant::Code,
            "invalid_grant",
            Some(65001),
            LoginFailure::SignInDeclined,
        ),
        (
            Grant::Code,
            "consent_required",
            None,
            LoginFailure::SignInDeclined,
        ),
        (Grant::Code, "invalid_client", None, LoginFailure::Other),
    ] {
        assert_eq!(
            token_error(grant, error, code).failure(),
            failure,
            "{grant:?} {error} {code:?}"
        );
    }
    for (error, code, failure) in [
        ("access_denied", None, LoginFailure::SignInDeclined),
        ("invalid_request", Some(90094), LoginFailure::SignInDeclined),
        ("invalid_request", Some(90095), LoginFailure::SignInDeclined),
        ("interaction_required", None, LoginFailure::Other),
        ("unauthorized_client", Some(700016), LoginFailure::Other),
    ] {
        assert_eq!(authorize_error(error, code).failure(), failure, "{error}");
    }
}

#[test]
fn the_id_token_names_the_user_and_is_checked() {
    let at = SystemTime::now();
    let token = jwt(&claims(" signin@example.test ", CONSUMER_TENANT));
    let identity = check(&token, CLIENT, at).unwrap();
    assert_eq!(identity.username, "signin@example.test");
    assert!(identity.consumer());
    assert!(!format!("{identity:?}").contains("signin"));

    let work = check(
        &jwt(&claims("someone@example.test", WORK_TENANT)),
        CLIENT,
        at,
    )
    .unwrap();
    assert!(!work.consumer());

    let mut padded = token.split('.').map(str::to_owned).collect::<Vec<_>>();
    padded[1] = base64::engine::general_purpose::URL_SAFE
        .encode(claims("signin@example.test", CONSUMER_TENANT).to_string());
    assert!(
        check(&padded.join("."), CLIENT, at).is_ok(),
        "padding is fine"
    );

    let mut many = claims("signin@example.test", CONSUMER_TENANT);
    many["aud"] = serde_json::json!([CLIENT, "other"]);
    assert!(
        check(&jwt(&many), CLIENT, at).is_err(),
        "several audiences need azp"
    );
    many["azp"] = CLIENT.into();
    assert!(check(&jwt(&many), CLIENT, at).is_ok());

    let refused = |edit: &dyn Fn(&mut serde_json::Value), reason: &str| {
        let mut changed = claims("signin@example.test", CONSUMER_TENANT);
        edit(&mut changed);
        match check(&jwt(&changed), CLIENT, at) {
            Err(OAuthError::IdToken(found)) => assert_eq!(found, reason),
            other => panic!("{reason}: {other:?}"),
        }
    };
    refused(
        &|c| c["aud"] = "another-client".into(),
        "issued to another client",
    );
    refused(
        &|c| {
            c["iss"] = "https://login.example.test/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0".into()
        },
        "not Microsoft's issuer for its tenant",
    );
    refused(
        &|c| c["iss"] = format!("https://login.microsoftonline.com/{WORK_TENANT}/v2.0").into(),
        "not Microsoft's issuer for its tenant",
    );
    refused(&|c| c["tid"] = "common".into(), "no tenant");
    refused(&|c| c["exp"] = (now() - 3600).into(), "expired");
    refused(
        &|c| {
            c.as_object_mut().unwrap().remove("preferred_username");
        },
        "no username",
    );
    refused(&|c| c["preferred_username"] = "  ".into(), "no username");
    // Five minutes' leeway for the clock.
    let mut late = claims("signin@example.test", CONSUMER_TENANT);
    late["exp"] = (now() - 60).into();
    assert!(check(&jwt(&late), CLIENT, at).is_ok());

    for broken in [
        "",
        "a.b",
        "a.b.c.d",
        "a.!!!.c",
        &format!("a.{}.c", URL_SAFE_NO_PAD.encode("[]")),
    ] {
        assert!(
            matches!(check(broken, CLIENT, at), Err(OAuthError::IdToken(_))),
            "{broken}"
        );
    }
}

async fn stored(secrets: &MemoryStore, key: &AccountKey) -> String {
    let secret = secrets
        .read(&key.0, SecretKind::RefreshToken)
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(secret.to_vec()).unwrap()
}

#[tokio::test]
async fn access_tokens_are_cached_refreshed_and_their_refresh_tokens_rotated() {
    let mock = Mock::start().await;
    let client = mock.client();
    let secrets = MemoryStore::default();
    let key = AccountKey("account-1".into());
    let tokens = AccessTokens::default();
    let get = |renew| tokens.get(&key, "Sealwax: test", &secrets, Some(&client), renew);

    assert_eq!(
        get(false).await.err(),
        Some(LoginFailure::OAuthRequired),
        "nothing stored"
    );
    assert_eq!(
        tokens.get(&key, "", &secrets, None, false).await.err(),
        Some(LoginFailure::OAuthRequired),
        "no client"
    );

    let first = mock.grant_refresh_token();
    secrets
        .store(&key.0, SecretKind::RefreshToken, "", first.as_bytes())
        .await
        .unwrap();
    let fetched = get(false).await.unwrap();
    assert!(fetched.refreshed);
    assert_eq!(fetched.token.as_str(), "access-2");
    assert_eq!(
        stored(&secrets, &key).await,
        "refresh-2",
        "the rotated token is stored"
    );

    let cached = get(false).await.unwrap();
    assert!(!cached.refreshed);
    assert_eq!(cached.token.as_str(), "access-2");
    assert_eq!(mock.state().grants.len(), 1, "no second refresh");

    let renewed = get(true).await.unwrap();
    assert!(renewed.refreshed);
    assert_eq!(renewed.token.as_str(), "access-3");
    assert_eq!(stored(&secrets, &key).await, "refresh-3");
    assert_eq!(
        mock.state().grants,
        ["refresh_token refresh-1", "refresh_token refresh-2"]
    );

    // A token about to expire is replaced before use.
    mock.state().lifetime = 60;
    get(true).await.unwrap();
    assert!(
        get(false).await.unwrap().refreshed,
        "less than the margin left"
    );

    // Revoked: sign in again, and the stored token stays as it was.
    mock.state().refresh.clear();
    let before = stored(&secrets, &key).await;
    assert_eq!(get(true).await.err(), Some(LoginFailure::OAuthRequired));
    assert_eq!(stored(&secrets, &key).await, before);
    assert_eq!(
        get(false).await.err(),
        Some(LoginFailure::OAuthRequired),
        "nothing cached"
    );

    // A sign-in's token is used until it runs low; a forgotten account has none.
    tokens.seed(
        &key,
        AccessToken::new(
            zeroize::Zeroizing::new("seeded".into()),
            Duration::from_secs(3600),
        ),
    );
    assert_eq!(get(false).await.unwrap().token.as_str(), "seeded");
    tokens.forget(&key);
    assert_eq!(get(false).await.err(), Some(LoginFailure::OAuthRequired));
}

#[tokio::test]
async fn token_answers_are_read_whatever_their_lifetime_looks_like() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        for json in [
            r#"{"access_token":"a","expires_in":"1800"}"#,
            r#"{"access_token":"b"}"#,
            "not json",
        ] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap() > 2 {
                line.clear();
            }
            let mut body = [0u8; 256];
            let _ = tokio::time::timeout(Duration::from_millis(50), reader.read(&mut body)).await;
            reader
                .into_inner()
                .write_all(answer("200 OK", json).as_bytes())
                .await
                .unwrap();
        }
    });
    let provider = Provider::new(
        &format!("http://127.0.0.1:{port}/authorize"),
        &format!("http://127.0.0.1:{port}/token"),
    )
    .unwrap();
    let client = Client::new(CLIENT.into(), provider).unwrap();
    let text = client.refresh("r").await.unwrap();
    assert_eq!(text.lifetime, Duration::from_secs(1800));
    assert!(text.refresh.is_none() && text.id_token.is_none());
    let missing = client.refresh("r").await.unwrap();
    assert_eq!(missing.lifetime, Duration::from_secs(600));
    assert!(matches!(
        client.refresh("r").await.unwrap_err(),
        OAuthError::Response
    ));
}
