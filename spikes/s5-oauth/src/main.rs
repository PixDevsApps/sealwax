//! Live S5 run: `email-spike-s5 google|microsoft <address>` with the client IDs in
//! SEALWAX_GOOGLE_CLIENT_ID / SEALWAX_GOOGLE_CLIENT_SECRET / SEALWAX_MICROSOFT_CLIENT_ID.
//! Opens the system browser, completes the flow, stores the refresh token in the keyring under
//! the test app-id, refreshes once, and prints only timings and booleans — never a token.

use email_spike_s5::{Client, Provider, begin, finish, http_client, refresh};
use sealwax_engine::secrets::{KeyringStore, SecretKind, SecretStore};
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.pixdevsapps.Sealwax.Devel.s5";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(which), Some(address)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: email-spike-s5 google|microsoft <address>");
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
    let started = Instant::now();
    let authorization = begin(&provider, &client, Some(address)).await?;
    std::process::Command::new("xdg-open")
        .arg(&authorization.url)
        .status()?;
    let http = http_client();
    let tokens = finish(
        &http,
        &provider,
        &client,
        authorization,
        Duration::from_secs(300),
    )
    .await?;
    println!(
        "{}: signed in after {:.1} s; refresh token: {}; expires in {:?}",
        provider.name,
        started.elapsed().as_secs_f64(),
        tokens.refresh.is_some(),
        tokens.expires_in
    );
    let Some(refresh_token) = tokens.refresh else {
        return Err("no refresh token returned".into());
    };
    let keyring = KeyringStore::open(APP_ID).await?;
    keyring
        .store(
            "s5-test",
            SecretKind::RefreshToken,
            "Sealwax S5 test token",
            refresh_token.as_bytes(),
        )
        .await?;
    let stored = keyring
        .read("s5-test", SecretKind::RefreshToken)
        .await?
        .ok_or("refresh token missing from the keyring")?;
    let stored = String::from_utf8(stored.to_vec())?;
    let refreshed = refresh(&http, &provider, &client, &stored).await?;
    println!(
        "{}: refresh worked; new access token expires in {:?}",
        provider.name, refreshed.expires_in
    );
    keyring.delete_account("s5-test").await?;
    Ok(())
}
