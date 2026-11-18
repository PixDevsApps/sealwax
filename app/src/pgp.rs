//! The user's GnuPG in the app (ADR-030, ADR-032): the reader decrypts and checks with it, the
//! engine signs and encrypts what is sent, and the composer asks it which keys a draft would go
//! to. Never in the Flatpak (its sandbox doesn't reach the keyring) or in unit tests; headless
//! runs get an empty keyring of their own from `tools/headless.sh`.

use gtk4::gio;
use gtk4::prelude::*;
use sealwax_domain::compose::Draft;
use sealwax_domain::compose::protect::{self, OwnKeyProblem, Plan, Problem};
use sealwax_infrastructure::pgp::{Gpg, SealError};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// The user's `gpg` for an app called `application`; detached signatures for the reader's
/// checks are written under `$XDG_RUNTIME_DIR/<application>/pgp`.
pub(crate) fn find_for(application: &str) -> Option<Gpg> {
    if cfg!(test) || sealwax_infrastructure::sandbox::is_flatpak() {
        return None;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(std::env::temp_dir);
    Gpg::find(&runtime.join(application).join("pgp"))
}

/// The user's `gpg` for this application, looked for once.
pub(crate) fn shared() -> Option<Arc<Gpg>> {
    static FOUND: OnceLock<Option<Arc<Gpg>>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let application = gio::Application::default()
                .and_then(|app| app.application_id())
                .map_or_else(|| "sealwax".to_owned(), |id| id.to_string());
            find_for(&application).map(Arc::new)
        })
        .clone()
}

/// Why Sign and Encrypt can't be used here.
pub(crate) fn unavailable() -> &'static str {
    if sealwax_infrastructure::sandbox::is_flatpak() {
        "In the Flatpak, Sealwax can't reach your GnuPG keyring"
    } else {
        "Install GnuPG to sign or encrypt"
    }
}

/// The keys `draft` would be signed and encrypted with, read off the GTK thread;
/// `configured` is the account's `pgp_key`.
pub(crate) async fn plan(draft: Draft, configured: Option<String>) -> Result<Plan, Problem> {
    let Some(gpg) = shared() else {
        return Err(Problem::NoGnuPG);
    };
    gio::spawn_blocking(move || match gpg.plan(&draft, configured.as_deref()) {
        Ok(plan) => Ok(plan),
        Err(SealError::Problem(problem)) => Err(problem),
        Err(error) => {
            tracing::warn!(?error, "GnuPG couldn't list the keys for a draft");
            Err(Problem::NoGnuPG)
        }
    })
    .await
    .unwrap_or(Err(Problem::NoGnuPG))
}

/// The user's public key for `from` (or `configured`), armoured, read off the GTK thread:
/// its fingerprint and the key.
pub(crate) async fn own_public_key(
    from: String,
    configured: Option<String>,
) -> Result<(String, Vec<u8>), String> {
    let Some(gpg) = shared() else {
        return Err(unavailable().to_owned());
    };
    gio::spawn_blocking(move || {
        let secret = gpg
            .secret_keys()
            .map_err(|_| "GnuPG couldn't list your keys".to_owned())?;
        let any = protect::Protection::default();
        let fingerprint = protect::own_key(&from, &secret, configured.as_deref(), &any)
            .map_err(|problem| own_key_text(problem, &from))?;
        let key = gpg
            .export(&fingerprint)
            .map_err(|_| "GnuPG couldn't export your key".to_owned())?;
        Ok((fingerprint, key))
    })
    .await
    .unwrap_or_else(|_| Err("GnuPG couldn't export your key".to_owned()))
}

fn own_key_text(problem: OwnKeyProblem, from: &str) -> String {
    match problem {
        OwnKeyProblem::Missing => format!("There's no secret key for {from} in your GnuPG keyring"),
        OwnKeyProblem::Unusable => format!("Your key for {from} can't sign or encrypt"),
        OwnKeyProblem::Several => format!(
            "You have several keys for {from}: name the one to use as pgp_key in accounts.toml"
        ),
    }
}

/// What the composer says about `problem` with `draft`: the addresses are the user's own.
pub(crate) fn problem_text(problem: &Problem, from: &str) -> String {
    match problem {
        Problem::NoGnuPG => format!("Not sent: {}", unavailable().to_lowercase()),
        Problem::OwnKey(problem) => format!("Not sent: {}", own_key_text(*problem, from)),
        Problem::Missing(addresses) => format!(
            "Not sent: there's no key to encrypt to for {}. Import it into GnuPG, or turn \
             Encrypt off to send without encryption",
            addresses.join(", ")
        ),
        Problem::Choose(choices) => format!(
            "Pick a key for {}",
            choices
                .iter()
                .map(|(address, _)| address.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The line under the composer's fields while Encrypt is on, `None` when all is well.
pub(crate) fn keys_line(outcome: &Result<Plan, Problem>, from: &str) -> Option<String> {
    match outcome {
        Ok(plan) if plan.unverified.is_empty() => None,
        Ok(plan) => Some(format!(
            "Key not verified: {}. Check its fingerprint with the recipient",
            plan.unverified.join(", ")
        )),
        Err(Problem::Choose(choices)) => Some(format!(
            "Several keys for {}: you pick one when you send",
            choices
                .iter()
                .map(|(address, _)| address.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Err(Problem::Missing(addresses)) => {
            Some(format!("No key to encrypt to: {}", addresses.join(", ")))
        }
        Err(problem) => Some(problem_text(problem, from).replacen("Not sent: ", "", 1)),
    }
}

/// A fingerprint in groups of four, as GnuPG prints it.
pub(crate) fn grouped(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_line_names_what_is_missing_or_unverified() {
        let unverified = Ok(Plan {
            unverified: vec!["bob@example.test".into()],
            ..Plan::default()
        });
        assert_eq!(
            keys_line(&unverified, "ada@example.test").as_deref(),
            Some("Key not verified: bob@example.test. Check its fingerprint with the recipient")
        );
        assert_eq!(keys_line(&Ok(Plan::default()), "ada@example.test"), None);
        let missing = Err(Problem::Missing(vec!["carl@example.test".into()]));
        assert_eq!(
            keys_line(&missing, "ada@example.test").as_deref(),
            Some("No key to encrypt to: carl@example.test")
        );
        assert_eq!(grouped("0123456789ABCDEF0123"), "0123 4567 89AB CDEF 0123");
    }
}
