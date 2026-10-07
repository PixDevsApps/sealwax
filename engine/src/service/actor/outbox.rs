//! The outbox and the Drafts folder on the account's actor (ML-076, ML-077, ML-074; ADR-015's
//! at-most-once send), run on the command connection after the op runner: after every login,
//! on `Command::OutboxQueued` and `Command::DraftsChanged`, and when the store's next send,
//! Sent copy, upload or deletion comes due.
//!
//! **A send:** the message is built from the draft and its parts' bytes off the async threads
//! (or the bytes an earlier start recorded are read back), the Sent folder's mark is taken, and
//! one SMTP connection per round carries every due send: MAIL FROM and RCPT TO, then the store's
//! durable `send_started`, then DATA. Only a missing answer after the data started is uncertain;
//! a stop or `Reconnect` while the data goes out is recorded as uncertain before the actor ends,
//! and one the actor couldn't record is made uncertain by `recover_sends` at the next start.
//! Refusals go back to the user as a failed draft; offline and a login the user must fix keep
//! the send queued, with the account's status saying why.
//!
//! **The Sent copy:** none for servers that save their own (Microsoft's, Gmail's); otherwise a
//! look for the Message-ID from the mark taken before the send (a server may have saved one),
//! then APPEND `\Seen` with the folder's mark recorded first, so a retry finds an earlier
//! attempt's copy instead of making a second.
//!
//! **Drafts:** each changed draft's copy is appended (`\Draft \Seen`) with the folder's mark
//! recorded first; the store queues the copy it replaces for deletion. A retry never takes a
//! copy above the mark for the new revision (an older revision shares its Message-ID): it
//! appends again and removes what the earlier attempts left. Deletions the store holds no row
//! for go by UID or by a Message-ID search above a mark.

use super::ops::{MAX_ATTEMPTS, instant_at, unix_ms};
use super::{Interrupt, Poller, backoff_delay, discard, race};
use crate::accounts::secret_label;
use crate::api::{Event, SendError, SyncStatus};
use crate::imap::Credentials;
use crate::imap::write::{self, Append, Mark, OpFolder, WriteError, WriteFailure};
use crate::secrets::{SecretKind, SecretStore};
use crate::smtp::submit::{self, Envelope, SubmitError, SubmitFailure, Submitter};
use sealwax_domain::account::{AccountSettings, AuthMethod, LoginFailure};
use sealwax_domain::compose::mime::{self, BuildError, Field};
use sealwax_domain::compose::{Draft, address};
use sealwax_domain::model::{Flags, FolderRole};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    DeletionTarget, DraftDeletion, DraftUpload, DueSend, DueSentCopy, Failure, Prepared,
    SendFailed, SendFailure, SendStarted, SentCopyDone,
};
use std::time::Instant;

/// Whether the account's server keeps its own copy of what it sends, so Sealwax appends none:
/// Outlook.com and Microsoft 365 (and Gmail, for accounts added by hand). Hosts by suffix.
pub(super) fn server_saves_sent(settings: &AccountSettings) -> bool {
    if matches!(settings.auth, AuthMethod::OAuth(_)) {
        return true;
    }
    let host = settings
        .smtp
        .host
        .trim_end_matches('.')
        .to_ascii_lowercase();
    [
        "office365.com",
        "outlook.com",
        "gmail.com",
        "googlemail.com",
    ]
    .iter()
    .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// The message as it goes out: what the store records at the start, and its bytes.
struct Built {
    prepared: Prepared,
    bytes: Vec<u8>,
}

/// Why a message couldn't be built.
enum Unbuilt {
    /// The draft can't be sent as it is.
    Draft(BuildError),
    /// A part's or the recorded message's bytes couldn't be read or written.
    Blobs,
}

/// What the round does after one send.
enum Next {
    Continue,
    /// The SMTP server can't be reached or refused the login: the rest wait too.
    Stop,
}

/// Unix seconds, for the message's Date.
fn unix_seconds() -> i64 {
    unix_ms() / 1000
}

/// Reads the parts' bytes and builds the transmitted copy and the Sent copy, storing both in
/// the blob store: blocking work, for `spawn_blocking`.
fn build(blobs: &BlobStore, draft: &Draft, date: i64) -> Result<Built, Unbuilt> {
    let parts: Vec<Vec<u8>> = draft
        .parts
        .iter()
        .map(|part| blobs.get(&part.blob))
        .collect::<Result<_, _>>()
        .map_err(|_| Unbuilt::Blobs)?;
    let slices: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    let outgoing = mime::build(draft, date, &slices).map_err(Unbuilt::Draft)?;
    let message_blob = blobs
        .put(&outgoing.transmitted)
        .map_err(|_| Unbuilt::Blobs)?;
    let sent_copy_blob = if outgoing.sent_copy == outgoing.transmitted {
        None
    } else {
        Some(blobs.put(&outgoing.sent_copy).map_err(|_| Unbuilt::Blobs)?)
    };
    Ok(Built {
        prepared: Prepared {
            message_blob,
            sent_copy_blob,
            size: outgoing.transmitted.len() as u64,
            envelope_from: outgoing.envelope.mail_from,
            envelope_to: outgoing.envelope.recipients,
        },
        bytes: outgoing.transmitted,
    })
}

/// The Drafts folder's copy of `draft`.
fn build_drafts_copy(blobs: &BlobStore, draft: &Draft, date: i64) -> Result<Vec<u8>, Unbuilt> {
    let parts: Vec<Vec<u8>> = draft
        .parts
        .iter()
        .map(|part| blobs.get(&part.blob))
        .collect::<Result<_, _>>()
        .map_err(|_| Unbuilt::Blobs)?;
    let slices: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    mime::drafts_copy(draft, date, &slices).map_err(Unbuilt::Draft)
}

/// The store's reason and the app's error for a draft that can't be built.
fn build_failure(draft: &Draft, error: BuildError) -> (SendFailure, SendError) {
    let list = match error {
        BuildError::Address(Field::To) => Some(&draft.to),
        BuildError::Address(Field::Cc) => Some(&draft.cc),
        BuildError::Address(Field::Bcc) => Some(&draft.bcc),
        _ => None,
    };
    match list {
        Some(list) => (
            SendFailure::RecipientsRefused,
            SendError::RecipientsRefused(
                list.iter()
                    .filter(|recipient| address::ascii(&recipient.email).is_none())
                    .map(|recipient| recipient.email.clone())
                    .collect(),
            ),
        ),
        None => (SendFailure::Refused, SendError::Refused),
    }
}

/// The store's reason for a refused submission.
fn refusal(error: &SubmitError) -> SendFailure {
    match error {
        SubmitError::RecipientsRefused { .. }
        | SubmitError::InvalidAddress { sender: false, .. }
        | SubmitError::NeedsSmtpUtf8 { sender: false, .. } => SendFailure::RecipientsRefused,
        SubmitError::TooLarge { .. } => SendFailure::TooLarge,
        SubmitError::Auth(_) => SendFailure::NeedsSignIn,
        SubmitError::Connect(_) | SubmitError::Transient | SubmitError::Uncertain => {
            SendFailure::Other
        }
        _ => SendFailure::Refused,
    }
}

/// The login failure behind an SMTP connection that couldn't log in.
fn login_failure(error: &SubmitError) -> LoginFailure {
    match error {
        SubmitError::Connect(failure) | SubmitError::Auth(failure) => *failure,
        _ => LoginFailure::Other,
    }
}

impl Poller {
    /// What the account shows when nothing is wrong with its IMAP side: a full mailbox, a
    /// submission login the user must fix, or synced.
    pub(super) fn healthy(&self) -> SyncStatus {
        if self.quota {
            SyncStatus::QuotaFull
        } else {
            self.smtp_status.unwrap_or(SyncStatus::Synced)
        }
    }

    /// The wait before trying the submission server again after a failure.
    fn smtp_backoff(&mut self) -> std::time::Duration {
        self.smtp_failures += 1;
        let unit = self.jitter.unit();
        backoff_delay(self.config(), self.smtp_failures + 1, unit).max(self.config().backoff_min)
    }

    /// Sends what is due, then the Sent copies, and sets the timer for the next.
    pub(super) async fn run_outbox(&mut self, seen: u64) -> Result<(), Interrupt> {
        self.outbox_now = false;
        let now = unix_ms();
        let due = match self.shared.store.due_sends(self.account, now).recv().await {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "due sends couldn't be read");
                self.next_outbox = Some(Instant::now() + self.config().backoff_max);
                return Ok(());
            }
        };
        self.next_outbox = due.next.map(|at| instant_at(at, now));
        if !due.sends.is_empty() {
            self.send_all(due.sends, seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        for copy in due.sent_copies {
            self.sent_copy(copy, seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// The Sent folder's mark before any data goes out: a server that saves its own copy puts
    /// it above the mark, where the Sent copy's step looks first.
    async fn sent_mark(&mut self, seen: u64) -> Result<Option<Mark>, Interrupt> {
        if server_saves_sent(&self.settings) {
            return Ok(None);
        }
        let folders = self.shared.store.account_folders(self.account).recv().await;
        let Some(sent) = folders.ok().and_then(|folders| {
            folders
                .into_iter()
                .find(|folder| folder.role == FolderRole::Sent && folder.selectable)
        }) else {
            return Ok(None);
        };
        let Some(conn) = self.conn.as_mut() else {
            return Ok(None);
        };
        let _held = race(&self.signals, seen, self.locks.lock(sent.folder)).await?;
        let marked = race(
            &self.signals,
            seen,
            write::mark_target(conn, &sent.name_raw),
        )
        .await?;
        self.used = Instant::now();
        Ok(match marked {
            Ok(mark) => Some(mark),
            Err(error) => {
                if error.is_connection() {
                    self.lost();
                }
                None
            }
        })
    }

    /// Every due send on one SMTP connection.
    async fn send_all(&mut self, sends: Vec<DueSend>, seen: u64) -> Result<(), Interrupt> {
        let mark = self.sent_mark(seen).await?;
        let mut smtp: Option<Submitter> = None;
        let mut result = Ok(());
        for send in sends {
            match self.send_one(&send, &mut smtp, mark, seen).await {
                Ok(Next::Continue) => {}
                Ok(Next::Stop) => break,
                Err(interrupt) => {
                    result = Err(interrupt);
                    break;
                }
            }
        }
        if let Some(smtp) = smtp
            && result.is_ok()
        {
            smtp.quit().await;
        }
        result
    }

    /// The message to send: the bytes an earlier start recorded, or built now.
    async fn message(&self, send: &DueSend) -> Result<Built, Unbuilt> {
        let blobs = self.shared.blobs.clone();
        let task = match &send.prepared {
            Some(prepared) => {
                let prepared = prepared.clone();
                tokio::task::spawn_blocking(move || {
                    let bytes = blobs
                        .get(&prepared.message_blob)
                        .map_err(|_| Unbuilt::Blobs)?;
                    Ok(Built { prepared, bytes })
                })
            }
            None => {
                let draft = send.message.clone();
                let date = unix_seconds();
                tokio::task::spawn_blocking(move || build(&blobs, &draft, date))
            }
        };
        task.await.unwrap_or(Err(Unbuilt::Blobs))
    }

    /// An SMTP session with the account's password, or its access token refreshed once when
    /// the server refuses it.
    async fn submitter(&self) -> Result<Submitter, SubmitError> {
        let settings = &self.settings;
        let shared = &self.shared;
        let timeouts = shared.sync.submission;
        let secrets = shared.secrets().await;
        match settings.auth {
            AuthMethod::Password => {
                let password = secrets
                    .read(&settings.key.0, SecretKind::Password)
                    .await
                    .map_err(|error| {
                        tracing::warn!(%error, "the account's password couldn't be read");
                        SubmitError::Connect(LoginFailure::Other)
                    })?
                    .ok_or(SubmitError::Auth(LoginFailure::WrongPassword))?;
                let password = std::str::from_utf8(&password)
                    .map_err(|_| SubmitError::Auth(LoginFailure::WrongPassword))?;
                let credentials = Credentials::Password {
                    username: &settings.username,
                    password,
                };
                Submitter::connect_with(&settings.smtp, credentials, &shared.trust, timeouts).await
            }
            AuthMethod::OAuth(_) => {
                let label = secret_label(&settings.address);
                let client = shared.oauth();
                submit::connect_oauth(
                    &settings.smtp,
                    &settings.username,
                    &shared.trust,
                    timeouts,
                    |renew| {
                        shared
                            .access
                            .get(&settings.key, &label, secrets, client.as_deref(), renew)
                    },
                )
                .await
            }
        }
    }

    /// The submission server couldn't be reached or refused the login: the sends wait, and a
    /// login the user must fix shows in the account's status.
    fn smtp_failed(&mut self, error: &SubmitError) {
        let wait = self.smtp_backoff();
        tracing::info!(
            account = self.account.0,
            ?error,
            "the submission server couldn't be used"
        );
        if error.failure() == SubmitFailure::Account {
            let status = SyncStatus::from_failure(login_failure(error));
            self.smtp_status = Some(status);
            self.status.set(status);
        }
        let retry = Instant::now() + wait;
        self.next_outbox = Some(self.next_outbox.map_or(retry, |next| next.min(retry)));
    }

    fn smtp_ok(&mut self) {
        self.smtp_failures = 0;
        if self.smtp_status.take().is_some() {
            self.status.set(self.healthy());
        }
    }

    async fn send_one(
        &mut self,
        send: &DueSend,
        smtp: &mut Option<Submitter>,
        mark: Option<Mark>,
        seen: u64,
    ) -> Result<Next, Interrupt> {
        let started = Instant::now();
        let built = match self.message(send).await {
            Ok(built) => built,
            Err(Unbuilt::Draft(error)) => {
                tracing::info!(
                    account = self.account.0,
                    ?error,
                    "a draft couldn't be built"
                );
                let (reason, error) = build_failure(&send.message, error);
                self.refused(send, reason, error).await;
                return Ok(Next::Continue);
            }
            Err(Unbuilt::Blobs) => {
                tracing::warn!(
                    account = self.account.0,
                    "a draft's parts couldn't be read or its message stored"
                );
                self.refused(send, SendFailure::Other, SendError::Store)
                    .await;
                return Ok(Next::Continue);
            }
        };
        if !smtp.as_ref().is_some_and(Submitter::is_connected) {
            match race(&self.signals, seen, self.submitter()).await? {
                Ok(submitter) => {
                    *smtp = Some(submitter);
                    self.smtp_ok();
                }
                Err(error) => {
                    self.smtp_failed(&error);
                    return Ok(Next::Stop);
                }
            }
        }
        let Some(submitter) = smtp.as_mut() else {
            return Ok(Next::Stop);
        };
        let envelope = Envelope {
            from: &built.prepared.envelope_from,
            to: &built.prepared.envelope_to,
        };
        let prepared = match race(
            &self.signals,
            seen,
            submitter.prepare(envelope, &built.bytes),
        )
        .await?
        {
            Ok(prepared) => prepared,
            Err(error) => {
                self.not_sent(send, &built.prepared, &error).await;
                return Ok(Next::Continue);
            }
        };
        if let Some(mark) = mark {
            self.sent_marks.entry(send.draft).or_insert(mark);
        }
        let recorded = self
            .shared
            .store
            .send_started(send.draft, built.prepared.clone(), unix_ms())
            .recv()
            .await;
        match recorded {
            Ok(SendStarted::Started) => {}
            other => {
                // Undone meanwhile, or the start couldn't be made durable: no data goes out.
                tracing::info!(account = self.account.0, ?other, "a send didn't start");
                let _ = race(&self.signals, seen, prepared.cancel()).await?;
                if other.is_err() {
                    let retry = Instant::now() + self.config().backoff_min;
                    self.next_outbox = Some(self.next_outbox.map_or(retry, |next| next.min(retry)));
                }
                return Ok(Next::Continue);
            }
        }
        match race(&self.signals, seen, prepared.send()).await {
            Err(interrupt) => {
                self.uncertain(send).await;
                Err(interrupt)
            }
            Ok(Ok(())) => {
                self.delivered(send, started).await;
                Ok(Next::Continue)
            }
            Ok(Err(error)) => {
                self.not_sent(send, &built.prepared, &error).await;
                Ok(Next::Continue)
            }
        }
    }

    /// The server took the message.
    async fn delivered(&mut self, send: &DueSend, started: Instant) {
        self.smtp_failures = 0;
        match self
            .shared
            .store
            .send_done(send.draft, unix_ms())
            .recv()
            .await
        {
            Ok(recorded) => {
                tracing::debug!(
                    account = self.account.0,
                    attempts = send.attempts + 1,
                    ms = started.elapsed().as_secs_f64() * 1000.0,
                    "message sent"
                );
                self.shared.send(Event::Sent {
                    account: self.account,
                    draft: send.draft,
                });
                self.ops_now |= recorded.ops_queued;
                self.drafts_now |= recorded.copies_to_delete;
                self.outbox_now = true;
                discard(&self.shared, recorded.orphaned_blobs).await;
            }
            Err(error) => {
                // Left `Sending`: the next start makes it uncertain, never sent again.
                tracing::warn!(account = self.account.0, %error, "a sent message couldn't be recorded");
            }
        }
    }

    /// The data may have reached the server: never sent again without the user.
    async fn uncertain(&mut self, send: &DueSend) {
        tracing::info!(account = self.account.0, "a send's outcome is unknown");
        match self.shared.store.send_uncertain(send.draft).recv().await {
            Ok(recorded) if recorded.applied => self.shared.send(Event::SendUncertain {
                account: self.account,
                draft: send.draft,
            }),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "an uncertain send couldn't be recorded");
            }
        }
    }

    /// Back to the user: a draft whose send failed for good.
    async fn refused(&mut self, send: &DueSend, reason: SendFailure, error: SendError) {
        tracing::info!(account = self.account.0, ?reason, "a send was refused");
        match self
            .shared
            .store
            .send_failed(send.draft, SendFailed::Permanent(reason))
            .recv()
            .await
        {
            Ok(recorded) => {
                if recorded.applied {
                    self.shared.send(Event::SendFailed {
                        account: self.account,
                        draft: send.draft,
                        error,
                    });
                }
                discard(&self.shared, recorded.orphaned_blobs).await;
            }
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "a refused send couldn't be recorded");
            }
        }
    }

    /// A submission that didn't go through, by its class.
    async fn not_sent(&mut self, send: &DueSend, prepared: &Prepared, error: &SubmitError) {
        match error.failure() {
            SubmitFailure::Uncertain => self.uncertain(send).await,
            SubmitFailure::Refuse => {
                let reason = refusal(error);
                self.refused(send, reason, error.send_error(&prepared.envelope_to))
                    .await;
            }
            SubmitFailure::Retry | SubmitFailure::Account => {
                if error.failure() == SubmitFailure::Account {
                    self.smtp_failed(error);
                }
                let wait = self.smtp_backoff();
                let retry_at = unix_ms() + wait.as_millis() as i64;
                tracing::info!(
                    account = self.account.0,
                    ?error,
                    "a send waits to be tried again"
                );
                let recorded = self
                    .shared
                    .store
                    .send_failed(send.draft, SendFailed::Transient { retry_at })
                    .recv()
                    .await;
                if let Err(error) = recorded {
                    tracing::warn!(account = self.account.0, %error, "a deferred send couldn't be recorded");
                }
                let retry = Instant::now() + wait;
                self.next_outbox = Some(self.next_outbox.map_or(retry, |next| next.min(retry)));
            }
        }
    }

    /// The Sent copy of a sent message: none where the server saves its own; else found from
    /// the mark taken before the send, or appended `\Seen` with the folder's mark recorded
    /// first.
    async fn sent_copy(&mut self, copy: DueSentCopy, seen: u64) -> Result<(), Interrupt> {
        if server_saves_sent(&self.settings) {
            self.copy_done(&copy, SentCopyDone::ServerSaves).await;
            return Ok(());
        }
        let Some(folder) = copy.folder.clone() else {
            self.copy_done(&copy, SentCopyDone::GaveUp).await;
            return Ok(());
        };
        if copy.attempts >= MAX_ATTEMPTS || copy.message_id.is_empty() {
            self.copy_done(&copy, SentCopyDone::GaveUp).await;
            return Ok(());
        }
        let _held = race(&self.signals, seen, self.locks.lock(folder.folder)).await?;
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        let (mark, retry) = match copy.retry_from {
            Some(uidnext) => (
                Mark {
                    uidvalidity: folder.uidvalidity,
                    uidnext,
                },
                true,
            ),
            None => {
                let before = self.sent_marks.get(&copy.draft).copied();
                let found = race(
                    &self.signals,
                    seen,
                    write::find(conn, &folder.name_raw, &copy.message_id, before),
                )
                .await?;
                match found {
                    Ok(found) if !found.uids.is_empty() => {
                        self.used = Instant::now();
                        let uid = found.uids.first().copied();
                        self.copy_done(&copy, SentCopyDone::Appended { uid }).await;
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(error) => {
                        self.copy_failed(&copy, &error).await;
                        return Ok(());
                    }
                }
                match race(
                    &self.signals,
                    seen,
                    write::mark_target(conn, &folder.name_raw),
                )
                .await?
                {
                    Ok(mark) => (mark, false),
                    Err(error) => {
                        self.copy_failed(&copy, &error).await;
                        return Ok(());
                    }
                }
            }
        };
        let uidvalidity = mark.uidvalidity.or(folder.uidvalidity).unwrap_or(0);
        let started = self
            .shared
            .store
            .sent_copy_started(copy.draft, folder.folder, uidvalidity, mark.uidnext)
            .recv()
            .await;
        if !started.is_ok_and(|started| started.applied) {
            return Ok(());
        }
        let blobs = self.shared.blobs.clone();
        let key = copy.blob;
        let bytes = match tokio::task::spawn_blocking(move || blobs.get(&key)).await {
            Ok(Ok(bytes)) => bytes,
            _ => {
                tracing::warn!(
                    account = self.account.0,
                    "a Sent copy's bytes couldn't be read"
                );
                self.copy_done(&copy, SentCopyDone::GaveUp).await;
                return Ok(());
            }
        };
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        let request = Append {
            folder: &folder.name_raw,
            message: &bytes,
            message_id: &copy.message_id,
            flags: Flags::SEEN,
            mark,
            retry,
        };
        let appended = race(&self.signals, seen, write::append(conn, request)).await?;
        self.used = Instant::now();
        match appended {
            Ok(appended) => {
                self.copy_done(&copy, SentCopyDone::Appended { uid: appended.uid })
                    .await;
            }
            Err(error) => self.copy_failed(&copy, &error).await,
        }
        Ok(())
    }

    async fn copy_done(&mut self, copy: &DueSentCopy, done: SentCopyDone) {
        self.sent_marks.remove(&copy.draft);
        tracing::debug!(account = self.account.0, ?done, "Sent copy done");
        match self
            .shared
            .store
            .sent_copy_done(copy.draft, done)
            .recv()
            .await
        {
            Ok(recorded) => discard(&self.shared, recorded.orphaned_blobs).await,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "a Sent copy couldn't be recorded");
            }
        }
    }

    async fn copy_failed(&mut self, copy: &DueSentCopy, error: &WriteError) {
        tracing::info!(account = self.account.0, %error, "a Sent copy failed");
        if error.is_connection() {
            self.lost();
        }
        if error.failure() == WriteFailure::Refuse {
            self.copy_done(copy, SentCopyDone::GaveUp).await;
            return;
        }
        let unit = self.jitter.unit();
        let wait =
            backoff_delay(self.config(), copy.attempts + 2, unit).max(self.config().backoff_min);
        let retry_at = unix_ms() + wait.as_millis() as i64;
        if let Err(error) = self
            .shared
            .store
            .sent_copy_failed(copy.draft, retry_at)
            .recv()
            .await
        {
            tracing::warn!(account = self.account.0, %error, "a failed Sent copy couldn't be recorded");
        }
        let retry = Instant::now() + wait;
        self.next_outbox = Some(self.next_outbox.map_or(retry, |next| next.min(retry)));
    }

    /// The Drafts folder's uploads and deletions that are due.
    pub(super) async fn run_drafts(&mut self, seen: u64) -> Result<(), Interrupt> {
        self.drafts_now = false;
        let now = unix_ms();
        let due = match self.shared.store.due_drafts(self.account, now).recv().await {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "due drafts couldn't be read");
                self.next_drafts = Some(Instant::now() + self.config().backoff_max);
                return Ok(());
            }
        };
        self.next_drafts = due.next.map(|at| instant_at(at, now));
        for upload in due.uploads {
            self.upload(upload, seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        let mut done = Vec::new();
        for deletion in due.deletions {
            if self.delete_copy(&deletion, seen).await? {
                done.push(deletion.id);
            }
            if self.conn.is_none() {
                break;
            }
        }
        if !done.is_empty()
            && let Err(error) = self.shared.store.draft_deletions_done(done).recv().await
        {
            tracing::warn!(account = self.account.0, %error, "draft deletions couldn't be recorded");
        }
        Ok(())
    }

    fn write_failure(&mut self, attempts: u32, error: &WriteError) -> Failure {
        if error.is_connection() {
            self.lost();
        }
        match error.failure() {
            WriteFailure::Refuse => Failure::Permanent,
            _ if attempts + 1 >= MAX_ATTEMPTS => Failure::Permanent,
            _ => {
                let unit = self.jitter.unit();
                let wait =
                    backoff_delay(self.config(), attempts + 2, unit).max(self.config().backoff_min);
                let retry = Instant::now() + wait;
                self.next_drafts = Some(self.next_drafts.map_or(retry, |next| next.min(retry)));
                Failure::Transient {
                    retry_at: Some(unix_ms() + wait.as_millis() as i64),
                }
            }
        }
    }

    /// One draft's copy: appended `\Draft \Seen` with the folder's mark recorded first. A retry
    /// appends again and then removes what earlier attempts left above the mark, since an
    /// older revision shares the Message-ID and can't be told from this one by it.
    async fn upload(&mut self, upload: DraftUpload, seen: u64) -> Result<(), Interrupt> {
        let started_at = Instant::now();
        let folder = upload.folder.clone();
        let message_id = upload.message.message_id.clone();
        let blobs = self.shared.blobs.clone();
        let draft = upload.message.clone();
        let date = unix_seconds();
        let built = tokio::task::spawn_blocking(move || build_drafts_copy(&blobs, &draft, date))
            .await
            .unwrap_or(Err(Unbuilt::Blobs));
        let bytes = match built {
            Ok(bytes) if !message_id.is_empty() => bytes,
            _ => {
                let _ = self
                    .shared
                    .store
                    .draft_upload_failed(upload.draft, Failure::Permanent)
                    .recv()
                    .await;
                return Ok(());
            }
        };
        let _held = race(&self.signals, seen, self.locks.lock(folder.folder)).await?;
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        let mark = match upload.retry_from {
            Some(uidnext) => Mark {
                uidvalidity: folder.uidvalidity,
                uidnext,
            },
            None => match race(
                &self.signals,
                seen,
                write::mark_target(conn, &folder.name_raw),
            )
            .await?
            {
                Ok(mark) => mark,
                Err(error) => {
                    let failure = self.write_failure(upload.attempts, &error);
                    let _ = self
                        .shared
                        .store
                        .draft_upload_failed(upload.draft, failure)
                        .recv()
                        .await;
                    return Ok(());
                }
            },
        };
        let earlier = if upload.retry_from.is_some() {
            match race(
                &self.signals,
                seen,
                write::find(conn, &folder.name_raw, &message_id, Some(mark)),
            )
            .await?
            {
                Ok(found) => found.uids,
                Err(error) => {
                    let failure = self.write_failure(upload.attempts, &error);
                    let _ = self
                        .shared
                        .store
                        .draft_upload_failed(upload.draft, failure)
                        .recv()
                        .await;
                    return Ok(());
                }
            }
        } else {
            Vec::new()
        };
        let uidvalidity = mark.uidvalidity.or(folder.uidvalidity).unwrap_or(0);
        let recorded = self
            .shared
            .store
            .draft_upload_started(
                upload.draft,
                upload.revision,
                folder.folder,
                uidvalidity,
                mark.uidnext,
            )
            .recv()
            .await;
        if !recorded.is_ok_and(|recorded| recorded.applied) {
            return Ok(());
        }
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        let request = Append {
            folder: &folder.name_raw,
            message: &bytes,
            message_id: &message_id,
            flags: Flags::DRAFT.union(Flags::SEEN),
            mark,
            retry: false,
        };
        let appended = race(&self.signals, seen, write::append(conn, request)).await?;
        self.used = Instant::now();
        let appended = match appended {
            Ok(appended) => appended,
            Err(error) => {
                let failure = self.write_failure(upload.attempts, &error);
                let _ = self
                    .shared
                    .store
                    .draft_upload_failed(upload.draft, failure)
                    .recv()
                    .await;
                return Ok(());
            }
        };
        let stale: Vec<u32> = earlier
            .into_iter()
            .filter(|uid| Some(*uid) != appended.uid)
            .collect();
        if let (false, Some(uidvalidity), Some(conn)) =
            (stale.is_empty(), appended.uidvalidity, self.conn.as_mut())
        {
            let folder = OpFolder {
                name_raw: &folder.name_raw,
                uidvalidity,
            };
            if let Err(error) =
                race(&self.signals, seen, write::expunge(conn, folder, &stale)).await?
            {
                tracing::info!(account = self.account.0, %error, "an earlier upload's copy wasn't removed");
                if error.is_connection() {
                    self.lost();
                }
            }
        }
        match self
            .shared
            .store
            .draft_upload_done(upload.draft, upload.revision, appended.uid)
            .recv()
            .await
        {
            Ok(recorded) => {
                self.ops_now |= recorded.ops_queued;
                tracing::debug!(
                    account = self.account.0,
                    attempts = upload.attempts + 1,
                    stale = stale.len(),
                    ms = started_at.elapsed().as_secs_f64() * 1000.0,
                    "draft uploaded"
                );
            }
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "an upload couldn't be recorded");
            }
        }
        Ok(())
    }

    /// A copy the store holds no row for, by UID or by Message-ID above a mark. True when it
    /// is done (deleted, gone, or the folder's UIDVALIDITY isn't its own any more).
    async fn delete_copy(
        &mut self,
        deletion: &DraftDeletion,
        seen: u64,
    ) -> Result<bool, Interrupt> {
        let _held = race(&self.signals, seen, self.locks.lock(deletion.folder)).await?;
        let Some(conn) = self.conn.as_mut() else {
            return Ok(false);
        };
        let folder = OpFolder {
            name_raw: &deletion.folder_name,
            uidvalidity: deletion.uidvalidity,
        };
        let uids = match &deletion.target {
            DeletionTarget::Uid(uid) => Ok(vec![*uid]),
            DeletionTarget::Search {
                message_id,
                from_uid,
                below_uid,
            } => {
                let mark = Mark {
                    uidvalidity: Some(deletion.uidvalidity),
                    uidnext: *from_uid,
                };
                race(
                    &self.signals,
                    seen,
                    write::find(conn, &deletion.folder_name, message_id, Some(mark)),
                )
                .await?
                .map(|found| {
                    if found.uidvalidity != deletion.uidvalidity {
                        return Vec::new();
                    }
                    found
                        .uids
                        .into_iter()
                        .filter(|uid| below_uid.is_none_or(|below| *uid < below))
                        .collect()
                })
            }
        };
        let deleted = match uids {
            Ok(uids) if uids.is_empty() => Ok(()),
            Ok(uids) => race(&self.signals, seen, write::expunge(conn, folder, &uids))
                .await?
                .map(drop),
            Err(error) => Err(error),
        };
        self.used = Instant::now();
        match deleted {
            Ok(()) | Err(WriteError::UidValidity { .. }) => Ok(true),
            Err(error) => {
                let failure = self.write_failure(deletion.attempts, &error);
                let _ = self
                    .shared
                    .store
                    .draft_deletion_failed(deletion.id, failure)
                    .recv()
                    .await;
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::{AccountKey, Endpoint, OAuthProvider, Security};

    fn settings(smtp: &str, auth: AuthMethod) -> AccountSettings {
        let endpoint = |host: &str| Endpoint {
            host: host.into(),
            port: 465,
            security: Security::Tls,
        };
        AccountSettings {
            key: AccountKey("k".into()),
            name: "n".into(),
            address: "a@example.test".into(),
            incoming: endpoint("imap.example.test"),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: endpoint(smtp),
            username: "a".into(),
            auth,
            signature: None,
            identities: Vec::new(),
        }
    }

    #[test]
    fn servers_that_save_their_own_sent_copy() {
        for (host, auth, saves) in [
            ("smtp.example.test", AuthMethod::Password, false),
            ("127.0.0.1", AuthMethod::Password, false),
            (
                "smtp-mail.outlook.com",
                AuthMethod::OAuth(OAuthProvider::Microsoft),
                true,
            ),
            ("smtp.office365.com", AuthMethod::Password, true),
            ("SMTP.Gmail.com.", AuthMethod::Password, true),
            ("notgmail.com.example.test", AuthMethod::Password, false),
            ("evilgmail.com", AuthMethod::Password, false),
        ] {
            assert_eq!(server_saves_sent(&settings(host, auth)), saves, "{host}");
        }
    }
}
