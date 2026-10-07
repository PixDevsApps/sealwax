//! The ML-032 live check, for the lead to run under the user's authorization of 2026-10-05: the
//! real engine service on scratch folders, the session's keyring under the test app-id
//! `io.github.pixdevsapps.Sealwax.Devel.ml032`, Microsoft's sign-in page in the default browser
//! (`xdg-open`) and the real Outlook servers.
//!
//! ```text
//! cargo run -p sealwax-engine --example oauth_live -- <address> [--resign-in]
//! cargo run -p sealwax-engine --example oauth_live -- --forget
//! ```
//!
//! 1. Sign in: the browser opens once; the engine checks IMAP and SMTP as the sign-in's user.
//! 2. `AddAccount` with that sign-in, then its folder list and first INBOX header sync.
//! 3. The engine stops; a new one starts on the same folders and gets `Command::Accounts` from
//!    the scratch `accounts.toml`. Its sync has no access token, so it must refresh from the
//!    keyring (a refresh across a restart), and the rotated refresh token must replace the
//!    stored one.
//! 4. With `--resign-in`: a re-sign-in for the account (the browser opens again), which replaces
//!    the refresh token and syncs again.
//! 5. `RemoveAccount`, then the keyring must hold nothing under the test app-id.
//!
//! `--forget` only deletes whatever an interrupted run left under the test app-id.
//!
//! The client ID comes from `SEALWAX_MICROSOFT_CLIENT_ID`, else from `config.toml`'s
//! `[oauth.microsoft] client_id` (read, never written). The runner prints timings, counts,
//! booleans and failure names only: never a token, a code, the address, the username, the page's
//! URL or a folder name. The engine's own log lines (stderr, `RUST_LOG`, default `info`) carry
//! none of them either. Nothing is sent, moved, flagged or deleted on the server, and the scratch
//! folders are deleted at the end.

use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, OAuthProvider};
use sealwax_domain::autoconfig::{address_domain, presets};
use sealwax_domain::config::Config;
use sealwax_domain::model::AccountId;
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::secrets::{KeyringStore, Secret, SecretKind, SecretStore};
use sealwax_engine::service::{
    Engine, EngineConfig, SIGN_IN_WAIT, SecretBackend, SignInOptions, SyncConfig,
};
use sealwax_infrastructure::accounts::{self, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.pixdevsapps.Sealwax.Devel.ml032";
const SYNC_WAIT: Duration = Duration::from_secs(180);

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

/// Outlook.com's servers for an Outlook.com address, Microsoft 365's otherwise (offline: the
/// presets, no lookup carries the address anywhere).
fn servers(address: &str) -> Result<(Endpoint, Endpoint, &'static str), Error> {
    let outlook = address_domain(address)
        .and_then(|domain| presets::by_domain(&domain))
        .is_some_and(|preset| preset.id == "outlook");
    let id = if outlook { "outlook" } else { "microsoft365" };
    let preset = presets::PRESETS
        .iter()
        .find(|preset| preset.id == id)
        .ok_or("no preset")?;
    let presets::Kind::Servers { imap, smtp, .. } = &preset.kind else {
        return Err("no servers".into());
    };
    let endpoint = |found: &presets::Endpoint| Endpoint {
        host: found.host.to_owned(),
        port: found.port,
        security: found.security,
    };
    Ok((endpoint(&imap[0]), endpoint(&smtp[0]), preset.name))
}

/// How many keyring items the test app-id holds.
async fn keyring_items() -> Result<usize, Error> {
    let keyring = oo7::Keyring::new().await?;
    Ok(keyring
        .search_items(&[("application", APP_ID)])
        .await?
        .len())
}

async fn refresh_token(key: &AccountKey) -> Result<Option<Secret>, Error> {
    let keyring = KeyringStore::open(APP_ID).await?;
    Ok(keyring.read(&key.0, SecretKind::RefreshToken).await?)
}

struct Running {
    engine: Engine,
    store: Arc<Store>,
}

fn start(dirs: &SealwaxDirs, client_id: &str) -> Result<Running, Error> {
    let blobs = BlobStore::open(&dirs.blobs_dir())?;
    let options = StoreOptions {
        blobs: Some(blobs.clone()),
        ..StoreOptions::default()
    };
    let store = Arc::new(Store::open(&dirs.store_file(), options)?);
    let engine = Engine::start(EngineConfig {
        application: APP_ID.into(),
        store: store.clone(),
        blobs,
        paths: AccountPaths::new(dirs),
        trust: Trust::Platform,
        secrets: SecretBackend::Keyring,
        detect: DetectOptions::default(),
        microsoft_client_id: Some(client_id.to_owned()),
        sign_in: SignInOptions::default(),
        sync: SyncConfig::default(),
    })?;
    Ok(Running { engine, store })
}

impl Running {
    fn send(&self, command: Command) -> Result<(), Error> {
        Ok(self.engine.commands().try_send(command)?)
    }

    async fn next<T>(
        &self,
        wait: Duration,
        mut pick: impl FnMut(Event) -> Option<T>,
    ) -> Result<T, Error> {
        let events = self.engine.events();
        tokio::time::timeout(wait, async {
            loop {
                let event = events.recv().await?;
                if let Some(found) = pick(event) {
                    return Ok(found);
                }
            }
        })
        .await
        .map_err(|_| "no answer in time")?
    }

    /// Opens the sign-in page in the browser and waits for the engine's answer.
    async fn sign_in(
        &self,
        request: u64,
        address: &str,
        account: Option<AccountKey>,
    ) -> Result<String, Error> {
        let started = Instant::now();
        self.send(Command::SignIn {
            request,
            provider: OAuthProvider::Microsoft,
            address: address.to_owned(),
            account,
        })?;
        let url = self
            .next(Duration::from_secs(10), |event| match event {
                Event::SignInOpen { request: id, url } if id == request => Some(url),
                _ => None,
            })
            .await?;
        let opened = std::process::Command::new("xdg-open")
            .arg(&url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        println!("sign-in page opened in the browser: {}", opened.success());
        let result = self
            .next(
                SIGN_IN_WAIT + Duration::from_secs(60),
                |event| match event {
                    Event::SignedIn {
                        request: id,
                        result,
                    } if id == request => Some(result),
                    _ => None,
                },
            )
            .await?;
        match result {
            Ok(username) => {
                println!(
                    "signed in after {:.1} s; IMAP and SMTP took the token; the username equals the address typed: {}",
                    started.elapsed().as_secs_f64(),
                    username.eq_ignore_ascii_case(address)
                );
                Ok(username)
            }
            Err(error) => Err(format!(
                "sign-in failed after {:.1} s: {:?} {:?}",
                started.elapsed().as_secs_f64(),
                error.protocol,
                error.failure
            )
            .into()),
        }
    }

    /// Waits for the account's folder list and first INBOX sync. A sync after a restart lists
    /// the folders and stores no headers: the INBOX's first sync is done (M2 has no re-sync).
    async fn synced(&self, account: AccountId, what: &str) -> Result<(), Error> {
        let started = Instant::now();
        let mut folders = 0;
        let mut headers = 0;
        let outcome = self
            .next(SYNC_WAIT, |event| match event {
                Event::FoldersSynced {
                    account: id,
                    folders: count,
                } if id == account => {
                    folders = count;
                    None
                }
                Event::SyncProgress {
                    account: id,
                    fetched,
                    ..
                } if id == account => {
                    headers = fetched;
                    None
                }
                Event::AccountStatus {
                    account: id,
                    status: SyncStatus::Synced,
                } if id == account => Some(Ok(())),
                Event::AccountStatus {
                    account: id,
                    status,
                } if id == account => status.failure().map(Err),
                _ => None,
            })
            .await?;
        let seconds = started.elapsed().as_secs_f64();
        match outcome {
            Ok(()) => {
                println!(
                    "{what}: sync finished in {seconds:.1} s; {folders} folders listed; {headers} INBOX headers stored"
                );
                Ok(())
            }
            Err(failure) => {
                Err(format!("{what}: sync failed after {seconds:.1} s: {failure:?}").into())
            }
        }
    }

    async fn remove(&self, key: AccountKey) -> Result<(), Error> {
        self.send(Command::RemoveAccount { request: 90, key })?;
        let result = self
            .next(Duration::from_secs(30), |event| match event {
                Event::AccountRemoved {
                    request: 90,
                    result,
                } => Some(result),
                _ => None,
            })
            .await?;
        println!("account removed: {}", result.is_ok());
        result.map_err(|error| format!("removal failed: {error:?}").into())
    }

    fn stop(self) -> Result<(), Error> {
        self.engine.stop();
        Arc::into_inner(self.store)
            .ok_or("the store is still shared")?
            .close()?;
        Ok(())
    }
}

/// Steps 1–4. The account's key is recorded in `added` as soon as it exists, so a failure can
/// still remove it; every engine is stopped, and its store closed, before the next one starts.
async fn check(
    dirs: &SealwaxDirs,
    client_id: &str,
    address: &str,
    resign_in: bool,
    added: &mut Option<AccountKey>,
) -> Result<(), Error> {
    let first = start(dirs, client_id)?;
    let steps = sign_in_and_add(&first, dirs, address, added).await;
    first.stop()?;
    println!("engine stopped");
    let (account, key, before) = steps?;

    let second = start(dirs, client_id)?;
    let steps = restart(&second, dirs, address, resign_in, account, &key, before).await;
    second.stop()?;
    steps
}

/// Steps 1 and 2: the account's store id and key, and its refresh token as stored.
async fn sign_in_and_add(
    first: &Running,
    dirs: &SealwaxDirs,
    address: &str,
    added: &mut Option<AccountKey>,
) -> Result<(AccountId, AccountKey, Option<Secret>), Error> {
    let (imap, smtp, preset) = servers(address)?;
    println!("servers: the {preset} preset");
    let username = first.sign_in(1, address, None).await?;
    first.send(Command::AddAccount {
        request: 2,
        settings: AccountSettings {
            key: AccountKey(String::new()),
            name: "ML-032 live check".into(),
            address: address.to_owned(),
            incoming: imap,
            receive: sealwax_domain::account::Receive::Imap,
            smtp,
            username,
            auth: AuthMethod::OAuth(OAuthProvider::Microsoft),
            signature: None,
            identities: Vec::new(),
        },
        password: Password::new(String::new()),
        sign_in: Some(1),
    })?;
    let account = first
        .next(Duration::from_secs(30), |event| match event {
            Event::AccountAdded { request: 2, result } => Some(result),
            _ => None,
        })
        .await?
        .map_err(|error| format!("add failed: {error:?}"))?;
    let listed = accounts::read(&dirs.accounts_file())?;
    let key = listed
        .first()
        .ok_or("accounts.toml lists nothing")?
        .key
        .clone();
    *added = Some(key.clone());
    println!("account added: true; accounts.toml lists {}", listed.len());
    first
        .synced(account, "first sync (the sign-in's access token)")
        .await?;
    let before = refresh_token(&key).await?;
    println!("refresh token in the keyring: {}", before.is_some());
    Ok((account, key, before))
}

/// Steps 3 and 4, in a new engine on the same folders.
async fn restart(
    second: &Running,
    dirs: &SealwaxDirs,
    address: &str,
    resign_in: bool,
    account: AccountId,
    key: &AccountKey,
    before: Option<Secret>,
) -> Result<(), Error> {
    let listed = accounts::read(&dirs.accounts_file())?;
    second.send(Command::Accounts { accounts: listed })?;
    let restarted = second
        .synced(account, "after a restart (a refresh from the keyring)")
        .await;
    let after = refresh_token(key).await?;
    println!(
        "refresh token still in the keyring: {}; replaced by the rotated one: {}",
        after.is_some(),
        before.as_deref() != after.as_deref()
    );
    restarted?;
    if resign_in {
        second.sign_in(3, address, Some(key.clone())).await?;
        second.synced(account, "after the re-sign-in").await?;
        let replaced = refresh_token(key).await?;
        println!(
            "the re-sign-in replaced the refresh token: {}",
            replaced.is_some() && replaced.as_deref() != after.as_deref()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    if let Err(error) = sealwax_infrastructure::logging::init() {
        eprintln!("no logging: {error}");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--forget") {
        oo7::Keyring::new()
            .await?
            .delete(&[("application", APP_ID)])
            .await?;
        println!(
            "keyring items under the test app-id: {}",
            keyring_items().await?
        );
        return Ok(());
    }
    let resign_in = args.iter().any(|arg| arg == "--resign-in");
    let Some(address) = args.iter().find(|arg| !arg.starts_with("--")) else {
        eprintln!("usage: oauth_live <address> [--resign-in] | --forget");
        std::process::exit(2);
    };
    let left = keyring_items().await?;
    if left > 0 {
        return Err(format!(
            "{left} keyring items under the test app-id from an earlier run; run with --forget first"
        )
        .into());
    }
    let client_id = client_id()?;
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base)?;
    let root = tempfile::Builder::new()
        .prefix("oauth-live-")
        .tempdir_in(base)?;
    println!("scratch folders: {}", root.path().display());
    let dirs = SealwaxDirs::under(root.path());

    let mut added = None;
    let outcome = check(&dirs, &client_id, address, resign_in, &mut added).await;
    if let Err(error) = &outcome {
        println!("stopped: {error}");
    }
    if let Some(key) = added {
        let last = start(&dirs, &client_id)?;
        let removed = last.remove(key).await;
        last.stop()?;
        removed?;
    }
    let left = keyring_items().await?;
    println!("keyring items under the test app-id: {left}");
    drop(root);
    println!("scratch folders deleted");
    if left > 0 {
        return Err("the keyring still holds items under the test app-id".into());
    }
    if outcome.is_ok() {
        println!("live check passed");
    }
    outcome
}
