//! Live S5 run against a dedicated test account, with the client ID in
//! SEALWAX_MICROSOFT_CLIENT_ID (or SEALWAX_GOOGLE_CLIENT_ID and SEALWAX_GOOGLE_CLIENT_SECRET):
//!
//! - `email-spike-s5 microsoft sign-in <address>` opens the system browser once (no
//!   `login_hint`: the address never goes into the URL), completes the flow, keeps the refresh
//!   token in the keyring under a test app-id, logs in to IMAP (capabilities and the folder
//!   list, read only) and SMTP (AUTH, then QUIT) with XOAUTH2, then drops the access token as
//!   if it had expired, refreshes and logs in to IMAP again;
//! - `email-spike-s5 microsoft resume <address>`, in a new process: reads the refresh token
//!   back from the keyring, refreshes, logs in to IMAP, then deletes the token;
//! - `email-spike-s5 microsoft forget` deletes the token.
//!
//! Nothing is sent, moved, flagged or deleted. It prints only timings, counts and booleans:
//! never a token, a folder name or the address.

use email_spike_s5::{Client, Provider, begin, finish, http_client, refresh};
use sealwax_domain::account::{Endpoint, Security};
use sealwax_engine::imap::{self, Credentials};
use sealwax_engine::net::Trust;
use sealwax_engine::secrets::{KeyringStore, SecretKind, SecretStore};
use sealwax_engine::smtp;
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.pixdevsapps.Sealwax.Devel.s5";
const ACCOUNT: &str = "s5-test";

type Error = Box<dyn std::error::Error>;

fn endpoints(provider: &Provider) -> (Endpoint, Endpoint) {
    let endpoint = |host: &str, port, security| Endpoint {
        host: host.into(),
        port,
        security,
    };
    match provider.name {
        "google" => (
            endpoint("imap.gmail.com", 993, Security::Tls),
            endpoint("smtp.gmail.com", 465, Security::Tls),
        ),
        // Outlook.com, as Microsoft documents it (and as the autodetect preset has it).
        _ => (
            endpoint("outlook.office365.com", 993, Security::Tls),
            endpoint("smtp-mail.outlook.com", 587, Security::StartTls),
        ),
    }
}

/// IMAP with XOAUTH2: the login, then the folder list (read only).
async fn imap_check(imap: &Endpoint, address: &str, token: &str) -> Result<(), Error> {
    let started = Instant::now();
    let tls = Trust::Platform.client_config()?;
    let credentials = Credentials::OAuth2 {
        username: address,
        token,
    };
    let mut connection = imap::connect(imap, credentials, tls)
        .await
        .map_err(|error| format!("IMAP XOAUTH2 failed: {:?}", error.failure()))?;
    let logged_in = started.elapsed();
    let folders = imap::list(&mut connection)
        .await
        .map_err(|error| format!("IMAP LIST failed: {:?}", error.failure()))?;
    connection.logout().await;
    println!(
        "imap: XOAUTH2 login ok in {:.0} ms; {} folders listed",
        logged_in.as_secs_f64() * 1000.0,
        folders.len()
    );
    Ok(())
}

/// SMTP with XOAUTH2: AUTH, then QUIT; no MAIL FROM, so nothing is sent.
async fn smtp_check(smtp: &Endpoint, address: &str, token: &str) -> Result<(), Error> {
    let started = Instant::now();
    let credentials = Credentials::OAuth2 {
        username: address,
        token,
    };
    smtp::check_login(smtp, credentials, &Trust::Platform)
        .await
        .map_err(|failure| format!("SMTP XOAUTH2 failed: {failure:?}"))?;
    println!(
        "smtp: XOAUTH2 login ok in {:.0} ms (quit without sending)",
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

async fn sign_in(provider: &Provider, client: &Client, address: &str) -> Result<(), Error> {
    let (imap, smtp) = endpoints(provider);
    let started = Instant::now();
    let authorization = begin(provider, client, None).await?;
    std::process::Command::new("xdg-open")
        .arg(&authorization.url)
        .status()?;
    let http = http_client();
    let tokens = finish(
        &http,
        provider,
        client,
        authorization,
        Duration::from_secs(300),
    )
    .await?;
    println!(
        "{}: signed in after {:.1} s; refresh token: {}; access token expires in {:?}",
        provider.name,
        started.elapsed().as_secs_f64(),
        tokens.refresh.is_some(),
        tokens.expires_in
    );
    let refresh_token = tokens.refresh.ok_or("no refresh token returned")?;
    let keyring = KeyringStore::open(APP_ID).await?;
    keyring
        .store(
            ACCOUNT,
            SecretKind::RefreshToken,
            "Sealwax S5 test token",
            refresh_token.as_bytes(),
        )
        .await?;
    println!("keyring: refresh token stored under the test app-id");

    imap_check(&imap, address, &tokens.access).await?;
    smtp_check(&smtp, address, &tokens.access).await?;

    // A forced expiry: the access token is dropped unused, as if it had run out.
    drop(tokens.access);
    let stored = keyring
        .read(ACCOUNT, SecretKind::RefreshToken)
        .await?
        .ok_or("refresh token missing from the keyring")?;
    let stored = String::from_utf8(stored.to_vec())?;
    let refreshed = refresh(&http, provider, client, &stored).await?;
    println!(
        "refresh after the forced expiry: ok; new access token expires in {:?}; new refresh token: {}",
        refreshed.expires_in,
        refreshed.refresh.is_some()
    );
    if let Some(rotated) = &refreshed.refresh {
        keyring
            .store(
                ACCOUNT,
                SecretKind::RefreshToken,
                "Sealwax S5 test token",
                rotated.as_bytes(),
            )
            .await?;
    }
    imap_check(&imap, address, &refreshed.access).await?;
    println!("kept the refresh token for `resume` (a new process)");
    Ok(())
}

async fn resume(provider: &Provider, client: &Client, address: &str) -> Result<(), Error> {
    let (imap, _) = endpoints(provider);
    let keyring = KeyringStore::open(APP_ID).await?;
    let stored = keyring
        .read(ACCOUNT, SecretKind::RefreshToken)
        .await?
        .ok_or("no refresh token in the keyring; run sign-in first")?;
    println!("keyring: refresh token read back after a restart");
    let stored = String::from_utf8(stored.to_vec())?;
    let refreshed = refresh(&http_client(), provider, client, &stored).await?;
    println!(
        "refresh in the new process: ok; access token expires in {:?}",
        refreshed.expires_in
    );
    imap_check(&imap, address, &refreshed.access).await?;
    keyring.delete_account(ACCOUNT).await?;
    println!("keyring: test token deleted");
    Ok(())
}

/// Diagnosis for a refused SMTP login: AUTH XOAUTH2 with lettre directly, printing the
/// server's reply (code and text, which carry neither the token nor the address), then QUIT.
async fn smtp_diagnose(provider: &Provider, client: &Client, address: &str) -> Result<(), Error> {
    use lettre::transport::smtp::authentication::{Credentials as SmtpCredentials, Mechanism};
    use lettre::transport::smtp::extension::ClientId;
    use lettre::{AsyncSmtpTransport, Tokio1Executor};
    let keyring = KeyringStore::open(APP_ID).await?;
    let stored = keyring
        .read(ACCOUNT, SecretKind::RefreshToken)
        .await?
        .ok_or("no refresh token in the keyring; run sign-in first")?;
    let stored = String::from_utf8(stored.to_vec())?;
    let refreshed = refresh(&http_client(), provider, client, &stored).await?;
    if let Some(rotated) = &refreshed.refresh {
        keyring
            .store(
                ACCOUNT,
                SecretKind::RefreshToken,
                "Sealwax S5 test token",
                rotated.as_bytes(),
            )
            .await?;
    }
    println!("refresh: ok; granted scopes: {:?}", refreshed.scope);
    for host in ["smtp-mail.outlook.com", "smtp.office365.com"] {
        if let Err(error) = smtp_two_step(host, address, &refreshed.access).await {
            println!("{host}: two-step exchange failed: {error}");
        }
    }
    for host in ["smtp-mail.outlook.com", "smtp.office365.com"] {
        let transport: AsyncSmtpTransport<Tokio1Executor> =
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?
                .port(587)
                .hello_name(ClientId::Ipv4(std::net::Ipv4Addr::LOCALHOST))
                .credentials(SmtpCredentials::new(
                    address.to_owned(),
                    refreshed.access.to_string(),
                ))
                .authentication(vec![Mechanism::Xoauth2])
                .timeout(Some(Duration::from_secs(30)))
                .build();
        match transport.test_connection().await {
            Ok(ok) => println!("{host}:587 XOAUTH2: accepted (connection test {ok})"),
            Err(error) => println!(
                "{host}:587 XOAUTH2: refused; code {:?}; {}",
                error.status(),
                error.to_string().replace(address, "<address>")
            ),
        }
    }
    Ok(())
}

/// Reads one SMTP reply (all its lines) and prints it.
async fn reply<S: tokio::io::AsyncBufRead + Unpin>(stream: &mut S) -> Result<String, Error> {
    use tokio::io::AsyncBufReadExt;
    let mut whole = String::new();
    loop {
        let mut line = String::new();
        let read =
            tokio::time::timeout(Duration::from_secs(30), stream.read_line(&mut line)).await??;
        if read == 0 {
            return Err("the server closed the connection".into());
        }
        whole.push_str(&line);
        if line.as_bytes().get(3) != Some(&b'-') {
            return Ok(whole);
        }
    }
}

/// Microsoft's documented two-step XOAUTH2 exchange, by hand: `AUTH XOAUTH2`, the server's
/// `334`, then the encoded token. Prints the server's replies only, then QUIT.
async fn smtp_two_step(host: &str, address: &str, token: &str) -> Result<(), Error> {
    use base64::Engine;
    use tokio::io::{AsyncWriteExt, BufReader};
    let tcp = tokio::net::TcpStream::connect((host, 587)).await?;
    let mut plain = BufReader::new(tcp);
    reply(&mut plain).await?;
    plain.write_all(b"EHLO [127.0.0.1]\r\n").await?;
    reply(&mut plain).await?;
    plain.write_all(b"STARTTLS\r\n").await?;
    let answer = reply(&mut plain).await?;
    if !answer.starts_with("220") {
        return Err(format!("STARTTLS refused: {}", answer.trim()).into());
    }
    let connector = tokio_rustls::TlsConnector::from(Trust::Platform.client_config()?);
    let name = rustls_pki_types::ServerName::try_from(host.to_owned())?;
    let tls = connector.connect(name, plain.into_inner()).await?;
    let mut tls = BufReader::new(tls);
    tls.write_all(b"EHLO [127.0.0.1]\r\n").await?;
    let capabilities = reply(&mut tls).await?;
    let auth = capabilities
        .lines()
        .find(|line| line[4..].starts_with("AUTH"))
        .unwrap_or("(no AUTH line)")
        .to_owned();
    println!("{host}: after STARTTLS, {}", auth.trim());
    tls.write_all(b"AUTH XOAUTH2\r\n").await?;
    let challenge = reply(&mut tls).await?;
    println!("{host}: AUTH XOAUTH2 -> {}", challenge.trim());
    if challenge.starts_with("334") {
        let initial = format!("user={address}\x01auth=Bearer {token}\x01\x01");
        let encoded = base64::engine::general_purpose::STANDARD.encode(initial.as_bytes());
        tls.write_all(encoded.as_bytes()).await?;
        tls.write_all(b"\r\n").await?;
        let outcome = reply(&mut tls).await?;
        println!("{host}: token -> {}", outcome.trim());
        if outcome.starts_with("334") {
            // The error challenge: an empty line ends the exchange.
            tls.write_all(b"\r\n").await?;
            println!(
                "{host}: after the error challenge -> {}",
                reply(&mut tls).await?.trim()
            );
        }
    }
    tls.write_all(b"QUIT\r\n").await?;
    let _ = reply(&mut tls).await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: email-spike-s5 microsoft|google sign-in|resume <address> | forget";
    let (Some(which), Some(step)) = (args.get(1), args.get(2)) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let (provider, client) = match which.as_str() {
        "google" => (
            Provider::google(),
            Client {
                id: std::env::var("SEALWAX_GOOGLE_CLIENT_ID")?,
                secret: std::env::var("SEALWAX_GOOGLE_CLIENT_SECRET").ok(),
            },
        ),
        "microsoft" => (
            Provider::microsoft(),
            Client {
                id: std::env::var("SEALWAX_MICROSOFT_CLIENT_ID")?,
                secret: None,
            },
        ),
        other => return Err(format!("unknown provider {other}").into()),
    };
    match (step.as_str(), args.get(3)) {
        ("sign-in", Some(address)) => sign_in(&provider, &client, address).await,
        ("resume", Some(address)) => resume(&provider, &client, address).await,
        ("smtp-diagnose", Some(address)) => smtp_diagnose(&provider, &client, address).await,
        ("forget", _) => {
            KeyringStore::open(APP_ID)
                .await?
                .delete_account(ACCOUNT)
                .await?;
            println!("keyring: test token deleted");
            Ok(())
        }
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
}
