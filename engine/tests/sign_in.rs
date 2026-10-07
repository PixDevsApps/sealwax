//! Microsoft sign-in through the engine service, offline (ML-032): a fake Microsoft, a simulated
//! browser and scripted IMAP and SMTP servers, all on 127.0.0.1. The sign-in checks IMAP and SMTP
//! as the ID token's user; the account keeps the sign-in's tokens and syncs with them; refreshes
//! rotate the stored refresh token; a refused refresh asks for a new sign-in, which replaces the
//! token; cancel and decline answer as they should.

mod mock;

use mock::{CLIENT, Microsoft, Server, Tls};
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, LoginError, LoginFailure, OAuthProvider, Protocol,
};
use sealwax_domain::model::AccountId;
use sealwax_engine::api::{AccountError, Command, Event, Password, RequestId, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::{self, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::sync::Arc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);
const ADDRESS: &str = "alias@example.test";
const USER: &str = "signin@example.test";

struct Setup {
    _root: tempfile::TempDir,
    dirs: SealwaxDirs,
    tls: Tls,
    microsoft: Microsoft,
    imap: Server,
    smtp: Server,
}

async fn setup() -> Setup {
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let tls = mock::tls();
    let microsoft = Microsoft::start(USER).await;
    let imap = mock::imap(&tls, &microsoft).await;
    let smtp = mock::smtp(&tls, &microsoft).await;
    Setup {
        _root: root,
        dirs,
        tls,
        microsoft,
        imap,
        smtp,
    }
}

impl Setup {
    fn start(&self, client_id: Option<&str>) -> Engine {
        let blobs = BlobStore::open(&self.dirs.blobs_dir()).unwrap();
        let options = StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        };
        let store = Store::open(&self.dirs.store_file(), options).unwrap();
        Engine::start(EngineConfig {
            application: "io.github.pixdevsapps.Sealwax.Test".into(),
            store: Arc::new(store),
            blobs,
            paths: AccountPaths::new(&self.dirs),
            trust: self.tls.trust.clone(),
            secrets: SecretBackend::Memory,
            detect: DetectOptions::default(),
            microsoft_client_id: client_id.map(str::to_owned),
            sign_in: SignInOptions {
                microsoft: self.microsoft.provider(),
                servers: Some((self.imap.endpoint(), self.smtp.endpoint())),
                wait: Duration::from_secs(10),
            },
            sync: SyncConfig::default(),
        })
        .unwrap()
    }

    /// What the assistant sends after a sign-in: the app's idea of the username doesn't count.
    fn settings(&self) -> AccountSettings {
        AccountSettings {
            key: AccountKey(String::new()),
            name: "Microsoft".into(),
            address: ADDRESS.into(),
            incoming: self.imap.endpoint(),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: self.smtp.endpoint(),
            username: ADDRESS.into(),
            auth: AuthMethod::OAuth(OAuthProvider::Microsoft),
            signature: None,
            identities: Vec::new(),
        }
    }

    fn listed(&self) -> Vec<AccountSettings> {
        accounts::read(&self.dirs.accounts_file()).unwrap()
    }
}

/// The next event `pick` accepts; others are skipped.
async fn next<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let events = engine.events();
    tokio::time::timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("the engine stopped");
            if let Some(found) = pick(event) {
                return found;
            }
        }
    })
    .await
    .expect("no such event in time")
}

/// No event `pick` accepts arrives within `wait`.
async fn none<T>(engine: &Engine, wait: Duration, mut pick: impl FnMut(Event) -> Option<T>) {
    let events = engine.events();
    let found = tokio::time::timeout(wait, async {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if pick(event).is_some() {
                        return;
                    }
                }
                Err(_) => std::future::pending::<()>().await,
            }
        }
    })
    .await;
    assert!(found.is_err(), "an unexpected event arrived");
}

fn send(engine: &Engine, command: Command) {
    engine.commands().try_send(command).unwrap();
}

fn stop(engine: Engine) {
    tokio::task::block_in_place(|| engine.stop());
}

/// Starts a sign-in and returns the page the engine wants opened.
async fn open(engine: &Engine, request: RequestId, account: Option<AccountKey>) -> String {
    send(
        engine,
        Command::SignIn {
            request,
            provider: OAuthProvider::Microsoft,
            address: ADDRESS.into(),
            account,
        },
    );
    next(engine, |event| match event {
        Event::SignInOpen { request: id, url } if id == request => Some(url),
        _ => None,
    })
    .await
}

async fn signed_in(engine: &Engine, request: RequestId) -> Result<String, LoginError> {
    next(engine, |event| match event {
        Event::SignedIn {
            request: id,
            result,
        } if id == request => Some(result),
        _ => None,
    })
    .await
}

/// A whole sign-in in the simulated browser.
async fn sign_in(
    engine: &Engine,
    request: RequestId,
    account: Option<AccountKey>,
) -> Result<String, LoginError> {
    let url = open(engine, request, account).await;
    let page = tokio::spawn(mock::browse(url));
    let result = signed_in(engine, request).await;
    assert!(page.await.unwrap().contains("You can close this tab"));
    result
}

async fn add(
    engine: &Engine,
    setup: &Setup,
    request: RequestId,
    sign_in: Option<RequestId>,
) -> Result<AccountId, AccountError> {
    send(
        engine,
        Command::AddAccount {
            request,
            settings: setup.settings(),
            password: Password::new(String::new()),
            sign_in,
        },
    );
    next(engine, |event| match event {
        Event::AccountAdded {
            request: id,
            result,
        } if id == request => Some(result),
        _ => None,
    })
    .await
}

/// Waits for the account's sync to finish or fail.
async fn synced(engine: &Engine, account: AccountId) -> Result<(), LoginFailure> {
    next(engine, |event| match event {
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
    .await
}

/// Logs every account in again (a reconnect, as after resume or the banner's Retry), with the
/// listed accounts as `accounts.toml` has them.
async fn sync_again(
    engine: &Engine,
    setup: &Setup,
    account: AccountId,
) -> Result<(), LoginFailure> {
    send(
        engine,
        Command::Accounts {
            accounts: setup.listed(),
        },
    );
    send(engine, Command::Reconnect);
    synced(engine, account).await
}

fn grants(setup: &Setup) -> Vec<String> {
    setup.microsoft.state().grants.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_checks_both_servers_as_the_token_identity_and_the_account_syncs_with_it() {
    let setup = setup().await;
    let engine = setup.start(Some(CLIENT));
    let url = open(&engine, 1, None).await;
    assert!(!url.contains("example.test") && !url.contains("login_hint"));
    assert!(url.contains("prompt=select_account"));
    let page = tokio::spawn(mock::browse(url));
    assert_eq!(signed_in(&engine, 1).await, Ok(USER.to_owned()));
    assert!(page.await.unwrap().contains("Sealwax has the sign-in."));

    let access = setup.microsoft.state().latest("access").unwrap();
    let login = (USER.to_owned(), access);
    assert_eq!(
        setup.imap.logins(),
        std::slice::from_ref(&login),
        "IMAP as the ID token's user"
    );
    assert_eq!(
        setup.smtp.logins(),
        std::slice::from_ref(&login),
        "SMTP as the ID token's user"
    );

    let account = add(&engine, &setup, 2, Some(1)).await.unwrap();
    assert_eq!(synced(&engine, account).await, Ok(()));
    assert_eq!(
        setup.imap.logins(),
        [login.clone(), login],
        "the sign-in's token synced"
    );
    assert_eq!(
        grants(&setup),
        ["code"],
        "no refresh while the token is fresh"
    );

    let listed = setup.listed();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].address, ADDRESS);
    assert_eq!(listed[0].username, USER, "the username is the sign-in's");
    assert_eq!(listed[0].auth, AuthMethod::OAuth(OAuthProvider::Microsoft));

    assert_eq!(
        add(&engine, &setup, 3, Some(1)).await,
        Err(AccountError::SignInExpired),
        "a sign-in serves one add"
    );
    send(
        &engine,
        Command::RemoveAccount {
            request: 4,
            key: listed[0].key.clone(),
        },
    );
    let removed = next(&engine, |event| match event {
        Event::AccountRemoved { request: 4, result } => Some(result),
        _ => None,
    })
    .await;
    assert_eq!(removed, Ok(()));
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_client_id_or_a_sign_in_nothing_happens() {
    let setup = setup().await;
    let engine = setup.start(None);
    send(
        &engine,
        Command::SignIn {
            request: 1,
            provider: OAuthProvider::Microsoft,
            address: ADDRESS.into(),
            account: None,
        },
    );
    assert_eq!(
        signed_in(&engine, 1).await,
        Err(LoginError {
            protocol: Protocol::Imap,
            failure: LoginFailure::OAuthRequired
        })
    );
    assert_eq!(
        add(&engine, &setup, 2, None).await,
        Err(AccountError::SignInExpired)
    );
    assert_eq!(
        add(&engine, &setup, 3, Some(1)).await,
        Err(AccountError::SignInExpired)
    );
    assert!(setup.listed().is_empty());
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_sign_in_answers_nothing_and_keeps_nothing() {
    let setup = setup().await;
    let engine = setup.start(Some(CLIENT));
    let url = open(&engine, 1, None).await;
    let redirect: String = url::Url::parse(&url)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    let port: u16 = redirect.rsplit(':').next().unwrap().parse().unwrap();
    send(&engine, Command::CancelSignIn { request: 1 });
    let mut closed = false;
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(closed, "the loopback stops listening");
    none(&engine, Duration::from_millis(300), |event| match event {
        Event::SignedIn { .. } => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        add(&engine, &setup, 2, Some(1)).await,
        Err(AccountError::SignInExpired)
    );

    // A finished sign-in that is cancelled drops its tokens.
    assert_eq!(sign_in(&engine, 3, None).await, Ok(USER.to_owned()));
    send(&engine, Command::CancelSignIn { request: 3 });
    assert_eq!(
        add(&engine, &setup, 4, Some(3)).await,
        Err(AccountError::SignInExpired)
    );
    assert!(setup.listed().is_empty());
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declined_sign_in_is_reported_and_nothing_is_exchanged() {
    let setup = setup().await;
    setup.microsoft.state().decline = true;
    let engine = setup.start(Some(CLIENT));
    assert_eq!(
        sign_in(&engine, 1, None).await,
        Err(LoginError {
            protocol: Protocol::Imap,
            failure: LoginFailure::SignInDeclined
        })
    );
    assert!(grants(&setup).is_empty());
    assert!(setup.imap.logins().is_empty() && setup.smtp.logins().is_empty());
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_store_the_rotated_token_and_a_refused_one_asks_for_a_sign_in() {
    let setup = setup().await;
    // Every access token has less than the five-minute margin left: each sync refreshes.
    setup.microsoft.state().lifetime = 60;
    let engine = setup.start(Some(CLIENT));
    sign_in(&engine, 1, None).await.unwrap();
    let issued = setup.microsoft.state().latest_refresh().unwrap();
    let account = add(&engine, &setup, 2, Some(1)).await.unwrap();
    assert_eq!(synced(&engine, account).await, Ok(()));
    let rotated = setup.microsoft.state().latest_refresh().unwrap();
    assert_eq!(
        grants(&setup),
        ["code".to_owned(), format!("refresh {issued}")]
    );

    // Only the rotated token works now, so a second refresh proves it was stored.
    assert_eq!(sync_again(&engine, &setup, account).await, Ok(()));
    assert_eq!(grants(&setup).last(), Some(&format!("refresh {rotated}")));
    let (user, token) = setup.imap.logins().last().unwrap().clone();
    assert_eq!(user, USER);
    assert!(setup.microsoft.state().access.contains(&token));

    // Revoked: the account must sign in again.
    setup.microsoft.state().refresh.clear();
    assert_eq!(
        sync_again(&engine, &setup, account).await,
        Err(LoginFailure::OAuthRequired)
    );
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_access_token_is_refreshed_once() {
    let setup = setup().await;
    let engine = setup.start(Some(CLIENT));
    sign_in(&engine, 1, None).await.unwrap();
    let account = add(&engine, &setup, 2, Some(1)).await.unwrap();
    assert_eq!(synced(&engine, account).await, Ok(()));
    let logins = setup.imap.logins().len();

    // The server stops taking the token although it hasn't expired.
    setup.microsoft.state().access.clear();
    assert_eq!(sync_again(&engine, &setup, account).await, Ok(()));
    assert_eq!(grants(&setup).len(), 2, "one refresh");
    assert_eq!(
        setup.imap.logins().len(),
        logins + 2,
        "refused, then accepted"
    );

    // A fresh token refused too: no second refresh, and the account must sign in again.
    setup.microsoft.state().imap_refuses = true;
    assert_eq!(
        sync_again(&engine, &setup, account).await,
        Err(LoginFailure::OAuthRequired)
    );
    assert_eq!(grants(&setup).len(), 3, "one more refresh, not two");
    assert_eq!(setup.imap.logins().len(), logins + 4);
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_re_sign_in_replaces_the_refresh_token_and_must_be_the_same_user() {
    let setup = setup().await;
    let engine = setup.start(Some(CLIENT));
    sign_in(&engine, 1, None).await.unwrap();
    let account = add(&engine, &setup, 2, Some(1)).await.unwrap();
    assert_eq!(synced(&engine, account).await, Ok(()));
    let key = setup.listed()[0].key.clone();

    // The grant is gone: the sync asks for a sign-in.
    setup.microsoft.state().refresh.clear();
    setup.microsoft.state().access.clear();
    assert_eq!(
        sync_again(&engine, &setup, account).await,
        Err(LoginFailure::OAuthRequired)
    );

    // Signed in again: the account syncs at once with the new access token.
    assert_eq!(
        sign_in(&engine, 3, Some(key.clone())).await,
        Ok(USER.to_owned())
    );
    let replaced = setup.microsoft.state().latest_refresh().unwrap();
    assert_eq!(synced(&engine, account).await, Ok(()));
    // The new refresh token is the stored one: the next refresh uses it.
    setup.microsoft.state().access.clear();
    assert_eq!(sync_again(&engine, &setup, account).await, Ok(()));
    assert_eq!(grants(&setup).last(), Some(&format!("refresh {replaced}")));

    // Another user signing in for this account is refused, and the token stays.
    let stored = setup.microsoft.state().latest_refresh().unwrap();
    setup.microsoft.state().username = "someone-else@example.test".into();
    assert_eq!(
        sign_in(&engine, 4, Some(key.clone())).await,
        Err(LoginError {
            protocol: Protocol::Imap,
            failure: LoginFailure::WrongPassword
        })
    );
    setup.microsoft.state().username = USER.into();
    setup.microsoft.state().access.clear();
    assert_eq!(sync_again(&engine, &setup, account).await, Ok(()));
    assert_eq!(grants(&setup).last(), Some(&format!("refresh {stored}")));

    // An account `accounts.toml` doesn't list can't be signed in again.
    assert!(matches!(
        sign_in_unknown(&engine, 5).await,
        Err(LoginError {
            failure: LoginFailure::Other,
            ..
        })
    ));
    stop(engine);
}

/// A re-sign-in for an account `accounts.toml` doesn't list.
async fn sign_in_unknown(engine: &Engine, request: RequestId) -> Result<String, LoginError> {
    send(
        engine,
        Command::SignIn {
            request,
            provider: OAuthProvider::Microsoft,
            address: ADDRESS.into(),
            account: Some(AccountKey("unknown".into())),
        },
    );
    signed_in(engine, request).await
}
