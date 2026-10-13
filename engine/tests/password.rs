//! A new password for a password account the server refused (ML-048), through the engine
//! service: offline against scripted IMAP and SMTP servers on 127.0.0.1, and on the Docker
//! testbed as `bob` (ignored without it). A password both servers take replaces the stored one
//! and the account syncs again; a refused one is answered with why, and nothing is stored.
//!
//! cargo test -p sealwax-engine --test password -- --ignored --test-threads=1

mod common;
mod mock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use mock::Tls;
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, Endpoint, LoginError, LoginFailure, Protocol, Security,
};
use sealwax_domain::model::AccountId;
use sealwax_engine::api::{Command, Event, Password, RequestId, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::{self, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing_subscriber::fmt::MakeWriter;

const WAIT: Duration = Duration::from_secs(20);
const OLD: &str = "fictional-old-password";
const NEW: &str = "fictional-new-password";
const GUESS: &str = "fictional-wrong-guess";
const ADDRESS: &str = "password@example.test";

/// Every log line, as the formatter wrote it.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

/// Every log line of this test binary at trace level, with no target capped.
fn logs() -> &'static Captured {
    static LOGS: OnceLock<Captured> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Captured::default();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .finish(),
        )
        .unwrap();
        logs
    })
}

/// Every file under `dir` whose bytes contain `needle`. The store's WAL may go away meanwhile.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path).is_ok_and(|bytes| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            }) {
                found.push(path);
            }
        }
    }
    found
}

fn start(dirs: &SealwaxDirs, trust: Trust) -> Engine {
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let options = StoreOptions {
        blobs: Some(blobs.clone()),
        ..StoreOptions::default()
    };
    let store = Store::open(&dirs.store_file(), options).unwrap();
    Engine::start(EngineConfig {
        application: "io.github.pixdevsapps.Sealwax.Test".into(),
        store: Arc::new(store),
        blobs,
        paths: AccountPaths::new(dirs),
        trust,
        secrets: SecretBackend::Memory,
        detect: DetectOptions::default(),
        microsoft_client_id: None,
        sign_in: SignInOptions::default(),
        sync: SyncConfig::default(),
    })
    .unwrap()
}

/// The next event `pick` accepts; others are skipped. A `KeyringUnavailable` fails the test:
/// these engines keep secrets in memory by request, which is no fallback.
async fn next<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let events = engine.events();
    tokio::time::timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("the engine stopped");
            assert!(
                !matches!(event, Event::KeyringUnavailable),
                "a memory backend is no fallback"
            );
            if let Some(found) = pick(event) {
                return found;
            }
        }
    })
    .await
    .expect("no such event in time")
}

fn send(engine: &Engine, command: Command) {
    engine.commands().try_send(command).unwrap();
}

fn stop(engine: Engine) {
    tokio::task::block_in_place(|| engine.stop());
}

async fn added(engine: &Engine, request: RequestId, settings: AccountSettings) -> AccountId {
    send(
        engine,
        Command::AddAccount {
            request,
            settings,
            password: Password::new(OLD.into()),
            sign_in: None,
        },
    );
    next(engine, |event| match event {
        Event::AccountAdded {
            request: id,
            result,
        } if id == request => Some(result.expect("the add failed")),
        _ => None,
    })
    .await
}

/// The account's next state other than syncing.
async fn settled(engine: &Engine, account: AccountId) -> SyncStatus {
    next(engine, |event| match event {
        Event::AccountStatus {
            account: id,
            status,
        } if id == account && status != SyncStatus::Syncing => Some(status),
        _ => None,
    })
    .await
}

/// `UpdatePassword` and its answer.
async fn update(
    engine: &Engine,
    request: RequestId,
    key: &AccountKey,
    password: &str,
) -> Result<(), LoginError> {
    send(
        engine,
        Command::UpdatePassword {
            request,
            key: key.clone(),
            password: Password::new(password.into()),
        },
    );
    next(engine, |event| match event {
        Event::PasswordUpdated {
            request: id,
            result,
        } if id == request => Some(result),
        _ => None,
    })
    .await
}

/// Logs every account in again, as the banner's Retry does; the listed accounts stay as
/// `accounts.toml` has them.
async fn sync_again(engine: &Engine, dirs: &SealwaxDirs, account: AccountId) -> SyncStatus {
    assert!(!accounts::read(&dirs.accounts_file()).unwrap().is_empty());
    send(engine, Command::Reconnect);
    settled(engine, account).await
}

fn refused(protocol: Protocol, failure: LoginFailure) -> Result<(), LoginError> {
    Err(LoginError { protocol, failure })
}

/// What a scripted server takes and has seen.
#[derive(Default)]
struct Seen {
    /// The password it takes now, as after a change at the provider.
    accepts: String,
    /// Every password tried, in order (fictional ones only).
    tried: Vec<String>,
}

type Shared = Arc<Mutex<Seen>>;

struct Server {
    port: u16,
    seen: Shared,
}

impl Server {
    fn endpoint(&self) -> Endpoint {
        Endpoint {
            host: "127.0.0.1".into(),
            port: self.port,
            security: Security::Tls,
            certificate: None,
        }
    }

    fn accept(&self, password: &str) {
        self.seen.lock().unwrap().accepts = password.to_owned();
    }

    fn tried(&self) -> Vec<String> {
        self.seen.lock().unwrap().tried.clone()
    }
}

/// The password inside a base64 SASL PLAIN response; the username must be `user`.
fn plain_password(encoded: &str) -> String {
    let decoded = STANDARD.decode(encoded.trim()).unwrap();
    let mut parts = decoded.split(|&byte| byte == 0).skip(1);
    assert_eq!(parts.next(), Some(&b"user"[..]), "logged in as the account");
    String::from_utf8(parts.next().unwrap().to_vec()).unwrap()
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Accepts TLS connections and runs `session` on each.
async fn serve<F, S>(tls: &Tls, session: F) -> u16
where
    F: Fn(BufReader<Box<dyn Io>>) -> S + Send + Sync + 'static,
    S: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(tls.server.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let Ok(stream) = acceptor.accept(tcp).await else {
                continue;
            };
            let boxed: Box<dyn Io> = Box::new(stream);
            tokio::spawn(session(BufReader::new(boxed)));
        }
    });
    port
}

async fn line(io: &mut BufReader<Box<dyn Io>>) -> Option<String> {
    let mut line = String::new();
    match io.read_line(&mut line).await {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

async fn write(io: &mut BufReader<Box<dyn Io>>, text: &str) {
    let stream = io.get_mut();
    let _ = stream.write_all(text.as_bytes()).await;
    let _ = stream.flush().await;
}

/// IMAP with AUTHENTICATE PLAIN for `user` and the password it takes now, and an empty INBOX.
async fn imap(tls: &Tls) -> Server {
    let seen = Shared::default();
    let state = seen.clone();
    let port = serve(tls, move |mut io| {
        let state = state.clone();
        async move {
            write(&mut io, "* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n").await;
            while let Some(command) = line(&mut io).await {
                let (tag, rest) = command.split_once(' ').unwrap_or((&command, ""));
                let verb = rest.split(' ').next().unwrap_or("").to_ascii_uppercase();
                let reply = match verb.as_str() {
                    "AUTHENTICATE" => {
                        write(&mut io, "+ \r\n").await;
                        let password = plain_password(&line(&mut io).await.unwrap_or_default());
                        let mut seen = state.lock().unwrap();
                        let accepted = password == seen.accepts;
                        seen.tried.push(password);
                        if accepted {
                            format!("{tag} OK authenticated\r\n")
                        } else {
                            format!("{tag} NO [AUTHENTICATIONFAILED] Authentication failed.\r\n")
                        }
                    }
                    "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n"),
                    "LIST" => format!("* LIST (\\HasNoChildren) \"/\" INBOX\r\n{tag} OK done\r\n"),
                    "STATUS" => {
                        format!("* STATUS INBOX (MESSAGES 0 UNSEEN 0)\r\n{tag} OK done\r\n")
                    }
                    "EXAMINE" => format!(
                        "* 0 EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY 7] ok\r\n\
                         * OK [UIDNEXT 1] ok\r\n{tag} OK [READ-ONLY] done\r\n"
                    ),
                    "LOGOUT" => {
                        write(&mut io, &format!("* BYE bye\r\n{tag} OK done\r\n")).await;
                        return;
                    }
                    _ => format!("{tag} BAD unknown\r\n"),
                };
                write(&mut io, &reply).await;
            }
        }
    })
    .await;
    Server { port, seen }
}

/// SMTP submission over implicit TLS with AUTH PLAIN for `user` and the password it takes now.
async fn smtp(tls: &Tls) -> Server {
    let seen = Shared::default();
    let state = seen.clone();
    let port = serve(tls, move |mut io| {
        let state = state.clone();
        async move {
            write(&mut io, "220 mock ESMTP\r\n").await;
            while let Some(command) = line(&mut io).await {
                let upper = command.to_ascii_uppercase();
                let reply = if upper.starts_with("EHLO") {
                    "250-mock\r\n250 AUTH PLAIN\r\n".to_owned()
                } else if let Some(encoded) = command.strip_prefix("AUTH PLAIN ") {
                    let password = plain_password(encoded);
                    let mut seen = state.lock().unwrap();
                    let accepted = password == seen.accepts;
                    seen.tried.push(password);
                    if accepted {
                        "235 2.7.0 Authentication successful\r\n".to_owned()
                    } else {
                        "535 5.7.8 Authentication credentials invalid\r\n".to_owned()
                    }
                } else if upper.starts_with("QUIT") {
                    write(&mut io, "221 bye\r\n").await;
                    return;
                } else {
                    "502 unknown\r\n".to_owned()
                };
                write(&mut io, &reply).await;
            }
        }
    })
    .await;
    Server { port, seen }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_password_is_checked_stored_and_the_account_syncs_again() {
    let logs = logs();
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let tls = mock::tls();
    let (imap, smtp) = (imap(&tls).await, smtp(&tls).await);
    imap.accept(OLD);
    smtp.accept(OLD);
    let engine = start(&dirs, tls.trust.clone());
    let settings = AccountSettings {
        key: AccountKey(String::new()),
        name: "Password".into(),
        label: None,
        color: None,
        address: ADDRESS.into(),
        incoming: imap.endpoint(),
        receive: sealwax_domain::account::Receive::Imap,
        smtp: smtp.endpoint(),
        username: "user".into(),
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    };
    let account = added(&engine, 1, settings).await;
    assert_eq!(settled(&engine, account).await, SyncStatus::Synced);
    let key = accounts::read(&dirs.accounts_file()).unwrap()[0]
        .key
        .clone();

    // The password was changed at the provider: the account needs it again.
    imap.accept(NEW);
    smtp.accept(NEW);
    assert_eq!(
        sync_again(&engine, &dirs, account).await,
        SyncStatus::NeedsSignIn(LoginFailure::WrongPassword)
    );

    // A wrong guess is refused by IMAP, SMTP never sees it, and the old password stays.
    assert_eq!(
        update(&engine, 2, &key, GUESS).await,
        refused(Protocol::Imap, LoginFailure::WrongPassword)
    );
    assert!(
        smtp.tried().is_empty(),
        "no SMTP after a refused IMAP login"
    );
    assert_eq!(
        sync_again(&engine, &dirs, account).await,
        SyncStatus::NeedsSignIn(LoginFailure::WrongPassword)
    );
    assert_eq!(imap.tried().last().map(String::as_str), Some(OLD));

    // IMAP takes it but SMTP doesn't: refused as SMTP's, and still nothing stored.
    smtp.accept("fictional-smtp-only");
    assert_eq!(
        update(&engine, 3, &key, NEW).await,
        refused(Protocol::Smtp, LoginFailure::WrongPassword)
    );
    assert_eq!(
        sync_again(&engine, &dirs, account).await,
        SyncStatus::NeedsSignIn(LoginFailure::WrongPassword)
    );
    assert_eq!(imap.tried().last().map(String::as_str), Some(OLD));

    // Both take it: stored, and the account syncs again at once with it.
    smtp.accept(NEW);
    let logins = imap.tried().len();
    assert_eq!(update(&engine, 4, &key, NEW).await, Ok(()));
    let syncing = next(&engine, |event| match event {
        Event::AccountStatus {
            account: id,
            status,
        } if id == account => Some(status),
        _ => None,
    })
    .await;
    assert_eq!(syncing, SyncStatus::Syncing, "the sync restarts by itself");
    assert_eq!(settled(&engine, account).await, SyncStatus::Synced);
    assert_eq!(
        imap.tried()[logins..],
        [NEW, NEW],
        "the check's login, then the sync's"
    );
    assert_eq!(smtp.tried().last().map(String::as_str), Some(NEW));

    // The next start of the sync uses it too: it was stored.
    assert_eq!(
        sync_again(&engine, &dirs, account).await,
        SyncStatus::Synced
    );
    assert_eq!(imap.tried().last().map(String::as_str), Some(NEW));
    stop(engine);

    // No password typed reaches a file or a log line, nor does the address reach the logs.
    let text = logs.text();
    assert!(
        text.contains("the new password replaced the old one"),
        "logs were captured: {text}"
    );
    for password in [OLD, NEW, GUESS] {
        assert!(!text.contains(password), "a password is in the logs");
        assert_eq!(
            files_containing(root.path(), password),
            Vec::<PathBuf>::new(),
            "a password is in files"
        );
    }
    assert!(!text.contains(ADDRESS), "the address is in the logs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_password_for_an_account_that_isnt_listed_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let engine = start(&dirs, Trust::Platform);
    assert_eq!(
        update(&engine, 1, &AccountKey("unknown".into()), NEW).await,
        refused(Protocol::Imap, LoginFailure::Other)
    );
    assert!(!dirs.accounts_file().exists(), "nothing was written");
    stop(engine);
}

/// `bob` on the testbed: Dovecot over TLS, Stalwart's submission over TLS.
#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn a_new_password_on_the_testbed() {
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let engine = start(&dirs, common::trust());
    sealwax_engine::runtime().unwrap().block_on(async {
        let mut settings = common::settings(
            common::imap(common::DOVECOT, Security::Tls),
            common::submission(Security::Tls),
            "bob",
        );
        settings.key = AccountKey(String::new());
        send(
            &engine,
            Command::AddAccount {
                request: 1,
                settings,
                password: Password::new(common::PASSWORD.into()),
                sign_in: None,
            },
        );
        let account = next(&engine, |event| match event {
            Event::AccountAdded { request: 1, result } => Some(result.expect("the add failed")),
            _ => None,
        })
        .await;
        assert_eq!(settled(&engine, account).await, SyncStatus::Synced);
        let key = accounts::read(&dirs.accounts_file()).unwrap()[0]
            .key
            .clone();

        assert_eq!(
            update(&engine, 2, &key, "wrong-password").await,
            refused(Protocol::Imap, LoginFailure::WrongPassword)
        );
        assert_eq!(
            sync_again(&engine, &dirs, account).await,
            SyncStatus::Synced,
            "the stored password is unchanged"
        );

        // Stalwart drops a burst of submission connections from one address.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(update(&engine, 3, &key, common::PASSWORD).await, Ok(()));
        assert_eq!(settled(&engine, account).await, SyncStatus::Synced);

        send(&engine, Command::RemoveAccount { request: 4, key });
        let removed = next(&engine, |event| match event {
            Event::AccountRemoved { request: 4, result } => Some(result),
            _ => None,
        })
        .await;
        assert_eq!(removed, Ok(()));
    });
    engine.stop();
}
