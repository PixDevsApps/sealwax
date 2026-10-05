//! IDLE (RFC 2177) and NOOP for the connection that keeps INBOX current (ADR-006, S1). A notice
//! only wakes the caller for a state-based pass; its content is never read. async-imap's IDLE
//! handle needs three things around it (S1): a timeout on IDLE's start and on DONE, which it
//! doesn't have; an overall deadline, because its own timeout restarts with every "still here"
//! line a server sends (Dovecot sends one every two minutes); and the end of the stream, which it
//! reports like a requested interrupt, read as a lost connection.

use super::connection::Connection;
use super::error::ImapError;
use super::raw;
use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::{MailboxDatum, Response};
use std::future::Future;
use std::time::Duration;

/// How long IDLE's start and DONE may take.
pub const IDLE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Why an IDLE ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The server said something about the mailbox.
    Notice,
    /// The deadline passed: IDLE is re-issued before servers drop it (RFC 2177: 29 minutes).
    Timeout,
}

/// What ends an IDLE from the client's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Drop the connection at once (reconnecting: it may be dead).
    Drop,
    /// DONE and LOGOUT within `wait`, then close (quitting).
    Logout(Duration),
}

/// How an IDLE came back.
#[derive(Debug)]
pub enum Idled {
    Woke(Box<Connection>, Wake),
    Stopped,
}

impl Connection {
    /// NOOP; whether the server reported a change of the selected mailbox with it. S1: Stalwart
    /// doesn't report changes made between a client's last command and IDLE, so a NOOP that finds
    /// one means another pass instead of IDLE.
    pub async fn noop(&mut self) -> Result<bool, ImapError> {
        let mut changed = false;
        raw::run(&mut self.session, "NOOP", |response| {
            if matches!(
                response,
                Response::MailboxData(MailboxDatum::Exists(_))
                    | Response::Expunge(_)
                    | Response::Fetch(..)
                    | Response::Vanished { .. }
            ) {
                changed = true;
            }
        })
        .await?;
        Ok(changed)
    }

    /// IDLE on the selected mailbox until the server says anything, `deadline` passes or `stop`
    /// resolves.
    pub async fn idle(
        self,
        deadline: Duration,
        stop: impl Future<Output = Stop>,
    ) -> Result<Idled, ImapError> {
        let (session, parts) = self.into_parts();
        let mut handle = session.idle();
        tokio::time::timeout(IDLE_COMMAND_TIMEOUT, handle.init())
            .await
            .map_err(|_| ImapError::Timeout)??;
        let outcome = {
            let (wait, _interrupt) = handle.wait_with_timeout(deadline);
            tokio::select! {
                result = wait => match result? {
                    IdleResponse::NewData(_) => Ok(Wake::Notice),
                    IdleResponse::Timeout => Ok(Wake::Timeout),
                    IdleResponse::ManualInterrupt => return Err(ImapError::ConnectionLost),
                },
                _ = tokio::time::sleep(deadline) => Ok(Wake::Timeout),
                stop = stop => Err(stop),
            }
        };
        let wait = match outcome {
            Ok(_) => IDLE_COMMAND_TIMEOUT,
            Err(Stop::Drop) => return Ok(Idled::Stopped),
            Err(Stop::Logout(wait)) => wait,
        };
        let session = tokio::time::timeout(wait, handle.done())
            .await
            .map_err(|_| ImapError::Timeout)??;
        let connection = Connection::from_parts(session, parts);
        match outcome {
            Ok(wake) => Ok(Idled::Woke(Box::new(connection), wake)),
            Err(_) => {
                connection.logout_within(wait).await;
                Ok(Idled::Stopped)
            }
        }
    }
}
