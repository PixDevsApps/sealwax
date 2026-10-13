//! A live check of sending through Microsoft 365 with the patched lettre (vendor/README.md), for
//! the lead to run under the owner's authorization of 2026-10-13: one sign-in in the default
//! browser (`xdg-open`), then two sends through the Microsoft 365 preset's SMTP server with the
//! engine's submitter, both to the signed-in address and nowhere else:
//!
//! 1. from the signed-in address: a normal send, which the server should accept;
//! 2. from `sendas-check@example.com`, an address the mailbox doesn't own: Microsoft refuses it
//!    with SendAsDenied after the message's end, the long reply of issue #2. The patched lettre
//!    should read it (a 554 refusal) where lettre 0.11.23 left the send uncertain.
//!
//! ```text
//! RUST_LOG=info,sealwax_engine::smtp=debug cargo run -p sealwax-engine --example send_live -- <address>
//! ```
//!
//! Nothing is stored: the tokens stay in memory and no keyring item is written. The client ID
//! comes from `SEALWAX_MICROSOFT_CLIENT_ID`, else from `config.toml`'s `[oauth.microsoft]
//! client_id` (read, never written). The runner prints outcomes, codes, timings and lengths only:
//! never a token, the address, the username, the page's URL or the server's text. With the debug
//! filter above, the engine logs each refusal's length ("smtp refusal of the message read").

use sealwax_domain::account::Endpoint;
use sealwax_domain::autoconfig::presets;
use sealwax_domain::compose::{Draft, ids, mime};
use sealwax_domain::config::Config;
use sealwax_domain::model::Address;
use sealwax_engine::net::Trust;
use sealwax_engine::oauth::{Client, Fetched, Provider};
use sealwax_engine::smtp::submit::{Envelope, SubmitError, Submitter, Timeouts, connect_oauth};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
const SMTP_SCOPE: &str = "https://outlook.office.com/SMTP.Send";
const NOT_OWNED: &str = "sendas-check@example.com";

type Error = Box<dyn std::error::Error>;

fn client_id() -> Result<String, Error> {
    if let Ok(id) = std::env::var("SEALWAX_MICROSOFT_CLIENT_ID")
        && !id.trim().is_empty()
    {
        println!("client id: from SEALWAX_MICROSOFT_CLIENT_ID");
        return Ok(id.trim().to_owned());
    }
    let text = std::fs::read_to_string(SealwaxDirs::from_env().config_file())?;
    let config = Config::parse(&text).map_err(|_| "config.toml has mistakes")?;
    let id =
        config.oauth.microsoft.client_id.ok_or(
            "no client id: set SEALWAX_MICROSOFT_CLIENT_ID or [oauth.microsoft] client_id",
        )?;
    println!("client id: from config.toml");
    Ok(id)
}

/// The Microsoft 365 preset's SMTP server.
fn smtp() -> Result<Endpoint, Error> {
    let preset = presets::PRESETS
        .iter()
        .find(|preset| preset.id == "microsoft365")
        .ok_or("no Microsoft 365 preset")?;
    let presets::Kind::Servers { smtp, .. } = &preset.kind else {
        return Err("no servers".into());
    };
    Ok(Endpoint {
        host: smtp[0].host.to_owned(),
        port: smtp[0].port,
        security: smtp[0].security,
    })
}

/// A short plain message from `from` to `to`, built by Sealwax's MIME builder.
fn message(from: &str, to: &str, subject: &str) -> Result<(Vec<u8>, String), Error> {
    let mut random = [0; 16];
    getrandom::fill(&mut random)?;
    let draft = Draft {
        from: Address::new("Sealwax live check", from),
        to: vec![Address::new("", to)],
        subject: subject.to_owned(),
        html: "<p>A test from Sealwax's send_live runner (the patched lettre). \
               It can be deleted.</p>"
            .to_owned(),
        message_id: ids::message_id(from, random).ok_or("no Message-ID")?,
        ..Draft::default()
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let built = mime::build(&draft, now, &[]).map_err(|_| "the message couldn't be built")?;
    Ok((built.transmitted, built.envelope.mail_from))
}

/// One send: prepares the envelope and sends the message, and says how it went.
async fn send(
    submitter: &mut Submitter,
    from: &str,
    to: &[String],
    bytes: &[u8],
) -> (Result<(), SubmitError>, f64) {
    let started = Instant::now();
    let result = match submitter.prepare(Envelope { from, to }, bytes).await {
        Ok(prepared) => prepared.send().await,
        Err(error) => Err(error),
    };
    (result, started.elapsed().as_secs_f64())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    if let Err(error) = sealwax_infrastructure::logging::init() {
        eprintln!("no logging: {error}");
    }
    let Some(address) = std::env::args().nth(1) else {
        eprintln!("usage: send_live <address>");
        std::process::exit(2);
    };
    let endpoint = smtp()?;
    println!("servers: the Microsoft 365 preset");

    let client = Client::new(client_id()?, Provider::microsoft())?;
    let started = Instant::now();
    let authorization = client.begin().await?;
    let opened = std::process::Command::new("xdg-open")
        .arg(authorization.url())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    println!("sign-in page opened in the browser: {}", opened.success());
    let granted = client
        .finish(authorization, SIGN_IN_WAIT)
        .await
        .map_err(|error| format!("sign-in failed: {error:?}"))?;
    println!(
        "signed in after {:.1} s; a work or school account: {}; SMTP scope granted: {}; the username equals the address typed: {}",
        started.elapsed().as_secs_f64(),
        !granted.identity.consumer(),
        granted.tokens.granted(SMTP_SCOPE),
        granted.identity.username.eq_ignore_ascii_case(&address),
    );
    let username = granted.identity.username.clone();
    let access = granted.tokens.access.clone();
    let connect = || {
        connect_oauth(
            &endpoint,
            &username,
            &Trust::Platform,
            Timeouts::default(),
            |_renew| {
                let token = Zeroizing::new(access.as_str().to_owned());
                async move {
                    Ok(Fetched {
                        token,
                        refreshed: true,
                    })
                }
            },
        )
    };

    let to = vec![address.clone()];
    let mut submitter = connect()
        .await
        .map_err(|error| format!("SMTP login failed: {error:?}"))?;
    println!(
        "SMTP login: ok; SIZE {:?}",
        submitter.extensions().size.is_some()
    );

    let (bytes, from) = message(&address, &address, "Sealwax live check: a normal send")?;
    let (result, seconds) = send(&mut submitter, &from, &to, &bytes).await;
    println!(
        "1. from the signed-in address ({} bytes): {} in {seconds:.1} s",
        bytes.len(),
        match &result {
            Ok(()) => "accepted".to_owned(),
            Err(error) => format!("{error:?}"),
        }
    );

    if !submitter.is_connected() {
        submitter.quit().await;
        submitter = connect()
            .await
            .map_err(|error| format!("SMTP login failed: {error:?}"))?;
    }
    let (bytes, from) = message(NOT_OWNED, &address, "Sealwax live check: a From not owned")?;
    let (result, seconds) = send(&mut submitter, &from, &to, &bytes).await;
    println!(
        "2. from an address the mailbox doesn't own ({} bytes): {} in {seconds:.1} s",
        bytes.len(),
        match &result {
            Ok(()) => "accepted".to_owned(),
            Err(error) => format!("{error:?}"),
        }
    );
    println!(
        "{}",
        match result {
            Err(SubmitError::Refused { .. }) =>
                "the refusal was read: a definite answer, not an uncertain send",
            Err(SubmitError::Uncertain(_)) =>
                "the send is uncertain: the reply still couldn't be read",
            Ok(()) => "the server took a From the mailbox doesn't own",
            Err(_) => "another outcome; see above",
        }
    );
    submitter.quit().await;
    Ok(())
}
