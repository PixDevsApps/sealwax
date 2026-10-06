//! The Docker testbed (tools/testbed): endpoints, the test users and the testbed CA. Test data
//! only; every port is on 127.0.0.1.

#![allow(dead_code)]

use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Security};
use sealwax_domain::model::AccountId;
use sealwax_engine::imap::{self, Connection, Credentials};
use sealwax_engine::net::Trust;
use sealwax_infrastructure::store::{NewAccount, Store, StoreOptions};
use std::path::{Path, PathBuf};

pub const PASSWORD: &str = "testbed-password";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Server {
    pub name: &'static str,
    pub tls: u16,
    pub starttls: u16,
}

pub const DOVECOT: Server = Server {
    name: "dovecot",
    tls: 14993,
    starttls: 14143,
};
pub const DOVECOT_REV1: Server = Server {
    name: "dovecot-rev1",
    tls: 15993,
    starttls: 15143,
};
pub const STALWART: Server = Server {
    name: "stalwart",
    tls: 16993,
    starttls: 16143,
};
pub const SERVERS: [Server; 3] = [DOVECOT, DOVECOT_REV1, STALWART];

/// `SEALWAX_TESTBED_CA`, else the CA `tools/testbed/up.sh` generated.
pub fn ca() -> PathBuf {
    std::env::var_os("SEALWAX_TESTBED_CA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/testbed/certs/ca.pem")
        })
}

pub fn trust() -> Trust {
    Trust::Roots(ca())
}

pub fn endpoint(port: u16, security: Security) -> Endpoint {
    Endpoint {
        host: "127.0.0.1".into(),
        port,
        security,
    }
}

pub fn imap(server: Server, security: Security) -> Endpoint {
    match security {
        Security::Tls => endpoint(server.tls, security),
        Security::StartTls => endpoint(server.starttls, security),
    }
}

/// Stalwart's submission ports.
pub fn submission(security: Security) -> Endpoint {
    match security {
        Security::Tls => endpoint(16465, security),
        Security::StartTls => endpoint(16587, security),
    }
}

/// Mailpit, the mail catcher: STARTTLS required, any login accepted.
pub fn mailpit() -> Endpoint {
    endpoint(17025, Security::StartTls)
}

pub fn settings(imap: Endpoint, smtp: Endpoint, username: &str) -> AccountSettings {
    AccountSettings {
        key: AccountKey("testbed".into()),
        name: "Testbed".into(),
        address: format!("{username}@example.test"),
        imap,
        smtp,
        username: username.into(),
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    }
}

pub async fn login(server: Server, security: Security, user: &str) -> Connection {
    imap::connect(
        &imap(server, security),
        Credentials::Password {
            username: user,
            password: PASSWORD,
        },
        trust().client_config().unwrap(),
    )
    .await
    .unwrap_or_else(|error| panic!("{} {security:?}: {error}", server.name))
}

/// A store in a scratch folder under `target/` (on the real disk), with one account.
pub fn store() -> (tempfile::TempDir, Store, AccountId) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("engine-")
        .tempdir_in(base)
        .unwrap();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Testbed".into(),
            address: "test@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    (dir, store, account)
}
