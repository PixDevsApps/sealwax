//! The engine service in the app (ADR-003): started once the store is open, handed the accounts
//! `accounts.toml` lists, stopped before the store closes. Its events reach the setup assistant
//! through the [`EngineLink`](crate::engine_link::EngineLink) and the sidebar's account dots
//! through the mailbox.

use crate::window::AccountStatus;
use sealwax_domain::account::LoginFailure;
use sealwax_domain::model::AccountId;
use sealwax_engine::api::{Event, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::Store;
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::sync::Arc;

/// Development builds only: a CA file trusted instead of the system's (the mail testbed's).
pub const TEST_CA_ENV: &str = "SEALWAX_TEST_CA";
/// Development builds only: `memory` keeps secrets out of the keyring.
pub const SECRETS_ENV: &str = "SEALWAX_SECRETS";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineOptions {
    pub trust: Trust,
    pub secrets: SecretBackend,
}

impl EngineOptions {
    /// The system's trust and the keyring. Development builds honour the two overrides above;
    /// release builds never read them.
    pub fn from_env() -> Self {
        let mut options = Self {
            trust: Trust::Platform,
            secrets: SecretBackend::Keyring,
        };
        if cfg!(debug_assertions) {
            if let Some(ca) = std::env::var_os(TEST_CA_ENV) {
                options.trust = Trust::Roots(ca.into());
            }
            if std::env::var(SECRETS_ENV).is_ok_and(|value| value == "memory") {
                options.secrets = SecretBackend::Memory;
            }
        }
        options
    }
}

pub fn start(
    app_id: &str,
    dirs: &SealwaxDirs,
    store: Arc<Store>,
    blobs: BlobStore,
    options: &EngineOptions,
    microsoft_client_id: Option<String>,
) -> std::io::Result<Engine> {
    Engine::start(EngineConfig {
        application: app_id.to_owned(),
        store,
        blobs,
        paths: AccountPaths::new(dirs),
        trust: options.trust.clone(),
        secrets: options.secrets,
        detect: DetectOptions::default(),
        microsoft_client_id,
        sign_in: SignInOptions::default(),
        sync: SyncConfig::default(),
    })
}

/// What the sidebar shows for an account after `event`.
pub fn account_status(event: &Event) -> Option<(AccountId, AccountStatus)> {
    match *event {
        Event::FoldersSynced { account, .. } | Event::SyncProgress { account, .. } => {
            Some((account, AccountStatus::Syncing))
        }
        Event::AccountStatus { account, status } => Some((account, sidebar_status(status))),
        _ => None,
    }
}

fn sidebar_status(status: SyncStatus) -> AccountStatus {
    match status {
        SyncStatus::Syncing => AccountStatus::Syncing,
        SyncStatus::Synced => AccountStatus::Synced,
        SyncStatus::QuotaFull => AccountStatus::QuotaFull,
        // Signing in again doesn't help: only the organization's admin can turn it on.
        SyncStatus::NeedsSignIn(LoginFailure::SmtpAuthDisabled) => AccountStatus::SendingOff,
        SyncStatus::NeedsSignIn(LoginFailure::ServerNotMicrosoft) => AccountStatus::AddAgain,
        SyncStatus::NeedsSignIn(_) => AccountStatus::NeedsSignIn,
        SyncStatus::Offline(_) => AccountStatus::Offline,
        SyncStatus::CertificateChanged => AccountStatus::CertificateChanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::FolderId;

    #[test]
    fn sync_events_become_account_states() {
        let account = AccountId(3);
        let progress = Event::SyncProgress {
            account,
            folder: FolderId(1),
            fetched: 200,
            total: 1000,
        };
        assert_eq!(
            account_status(&progress),
            Some((account, AccountStatus::Syncing))
        );
        let status = |status| account_status(&Event::AccountStatus { account, status });
        assert_eq!(
            status(SyncStatus::Synced),
            Some((account, AccountStatus::Synced))
        );
        let failed = |failure| status(SyncStatus::from_failure(failure));
        assert_eq!(
            failed(LoginFailure::WrongPassword),
            Some((account, AccountStatus::NeedsSignIn))
        );
        assert_eq!(
            failed(LoginFailure::SmtpAuthDisabled),
            Some((account, AccountStatus::SendingOff)),
            "signing in again wouldn't help"
        );
        assert_eq!(
            failed(LoginFailure::ServerNotMicrosoft),
            Some((account, AccountStatus::AddAgain)),
            "nor here"
        );
        assert_eq!(
            failed(LoginFailure::Unreachable),
            Some((account, AccountStatus::Offline))
        );
        assert_eq!(
            failed(LoginFailure::Certificate),
            Some((account, AccountStatus::CertificateChanged))
        );
        assert_eq!(
            status(SyncStatus::QuotaFull),
            Some((account, AccountStatus::QuotaFull))
        );
    }
}
